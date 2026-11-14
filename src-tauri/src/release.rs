//! 发布工具共用逻辑。私钥只在发布机上使用，不进入启动器运行时。
//!
//! 角色枚举直接编译期嵌入 `contracts/roles-v1.json`，不在工具里另写一份。

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::signature::{canonical_payload, verify_manifest, SignatureError, SUPPORTED_ALGO};

/// 编译期内嵌的角色契约，保持单一真源。
pub const ROLES_JSON: &str = include_str!("../../contracts/roles-v1.json");

#[derive(Debug, thiserror::Error)]
pub enum ReleaseError {
    #[error("文件操作失败：{0}")]
    Io(#[from] io::Error),
    #[error("JSON 处理失败：{0}")]
    Json(#[from] serde_json::Error),
    #[error("签名处理失败：{0}")]
    Signature(#[from] SignatureError),
    #[error("zip 处理失败：{0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("私钥无效：{0}")]
    BadKey(String),
    #[error("manifest 不合法：{0}")]
    BadManifest(String),
    #[error("catalog 不合法：{0}")]
    BadCatalog(String),
    #[error("包内路径不合法：{0}")]
    BadPath(String),
}

fn general(msg: impl Into<String>) -> ReleaseError {
    ReleaseError::BadManifest(msg.into())
}

/// 从内嵌契约读出封闭角色集合。
pub fn role_ids() -> Result<BTreeSet<String>, ReleaseError> {
    let doc: Value = serde_json::from_str(ROLES_JSON)?;
    let roles = doc["roles"]
        .as_array()
        .ok_or_else(|| ReleaseError::BadManifest("roles-v1.json 缺少 roles 数组".into()))?;
    Ok(roles
        .iter()
        .filter_map(|r| r["id"].as_str().map(String::from))
        .collect())
}

// ---- UTC 时间 ----

/// 把 Unix 秒格式化为 `YYYY-MM-DDTHH:MM:SSZ`。零依赖，避免为时间格式引入 crate。
pub fn format_unix_seconds(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// 只接受工具自己产出的 UTC 格式。带毫秒或时区偏移时拒绝，避免字典序比较失真。
pub fn parse_utc_seconds(text: &str) -> Result<i64, ReleaseError> {
    let bytes = text.as_bytes();
    let shape = bytes.len() == 20
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes[19] == b'Z';
    if !shape {
        return Err(ReleaseError::BadCatalog(format!(
            "时间字段必须是 YYYY-MM-DDTHH:MM:SSZ：{text}"
        )));
    }
    let num = |a: usize, b: usize| -> Result<i64, ReleaseError> {
        text[a..b]
            .parse::<i64>()
            .map_err(|_| ReleaseError::BadCatalog(format!("时间字段不是数字：{text}")))
    };
    let (y, mo, d, h, mi, s) = (
        num(0, 4)?,
        num(5, 7)?,
        num(8, 10)?,
        num(11, 13)?,
        num(14, 16)?,
        num(17, 19)?,
    );
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 59 {
        return Err(ReleaseError::BadCatalog(format!("时间字段越界：{text}")));
    }
    Ok(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + s)
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = m + if m > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn utc_now() -> Result<String, ReleaseError> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| ReleaseError::BadManifest(e.to_string()))?
        .as_secs();
    Ok(format_unix_seconds(secs as i64))
}

// ---- 密钥与签名 ----

pub fn load_signing_key(path: &Path) -> Result<SigningKey, ReleaseError> {
    let text = std::fs::read_to_string(path)?;
    let raw = B64
        .decode(text.trim().as_bytes())
        .map_err(|e| ReleaseError::BadKey(e.to_string()))?;
    let seed: [u8; 32] = raw
        .try_into()
        .map_err(|_| ReleaseError::BadKey("私钥必须是 32 字节".into()))?;
    Ok(SigningKey::from_bytes(&seed))
}

/// 用系统随机源生成密钥。走 BCryptGenRandom 或 /dev/urandom，不引额外依赖。
pub fn generate_keypair() -> Result<(SigningKey, String), ReleaseError> {
    let mut seed = [0u8; 32];
    getrandom_fill(&mut seed)?;
    let sk = SigningKey::from_bytes(&seed);
    let pk = B64.encode(sk.verifying_key().to_bytes());
    Ok((sk, pk))
}

fn getrandom_fill(buf: &mut [u8]) -> Result<(), ReleaseError> {
    #[cfg(windows)]
    {
        #[link(name = "bcrypt")]
        extern "system" {
            fn BCryptGenRandom(
                h: *mut core::ffi::c_void,
                buf: *mut u8,
                len: u32,
                flags: u32,
            ) -> i32;
        }
        const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;
        let status = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status != 0 {
            return Err(ReleaseError::BadKey(format!(
                "系统随机数生成失败（NTSTATUS {status}）"
            )));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let mut f = File::open("/dev/urandom")?;
        f.read_exact(buf)?;
        Ok(())
    }
}

fn attach_signature(doc: &mut Value, sk: &SigningKey, block: Value) -> Result<(), ReleaseError> {
    doc["signature"] = block;
    let payload = canonical_payload(doc)?;
    let sig = sk.sign(&payload);
    doc["signature"]["value"] = json!(B64.encode(sig.to_bytes()));
    Ok(())
}

/// 给 zip 包的 manifest 签名。签名段自带公钥，供启动器先自证完整再查信任。
pub fn sign_manifest(
    manifest: &mut Value,
    sk: &SigningKey,
    key_id: &str,
) -> Result<(), ReleaseError> {
    attach_signature(
        manifest,
        sk,
        json!({
            "algo": SUPPORTED_ALGO,
            "pubkey": B64.encode(sk.verifying_key().to_bytes()),
            "key_id": key_id,
            "signed_at": utc_now()?,
            "value": ""
        }),
    )
}

/// 给 catalog 签名。公钥是内置 K5，包内不带 `pubkey`。
pub fn sign_catalog_doc(
    catalog: &mut Value,
    sk: &SigningKey,
    key_id: &str,
) -> Result<(), ReleaseError> {
    // 指纹从签名钥直接算，不给调用方「填一个和公钥不一致的值」的机会。
    let signer_fp = launcher_core::m2::fingerprint(&B64.encode(sk.verifying_key().to_bytes()))
        .map_err(|e| ReleaseError::BadCatalog(e.to_string()))?;
    let block = json!({
        "algo": SUPPORTED_ALGO,
        "key_id": key_id,
        "signer_fp": signer_fp,
        "signed_at": utc_now()?,
        "value": ""
    });
    attach_signature(catalog, sk, block)
}

// ---- 字段校验 ----

fn is_semver_three(text: &str) -> bool {
    let parts: Vec<&str> = text.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.bytes().all(|b| b.is_ascii_digit())
                && (p.len() == 1 || !p.starts_with('0'))
        })
}

/// 结构性检查。完整 JSON Schema 校验由 `contracts/validate.mjs` 与 CI 的 ajv 负责。
pub fn check_manifest_basics(m: &Value) -> Result<(), ReleaseError> {
    if m["schema_version"] != json!(2) {
        return Err(general("schema_version 必须是 2"));
    }
    const REQUIRED: &[&str] = &[
        "module_id",
        "name",
        "version",
        "variant",
        "role",
        "category",
        "publisher",
        "requires",
        "entry",
        "ports",
        "permissions",
    ];
    for f in REQUIRED {
        if m.get(f).is_none() {
            return Err(general(format!("缺少必填字段 {f}")));
        }
    }
    let role = m["role"]
        .as_str()
        .ok_or_else(|| general("role 必须是字符串"))?;
    if !role_ids()?.contains(role) {
        return Err(general(format!("未注册的 role：{role}")));
    }
    let version = m["version"]
        .as_str()
        .ok_or_else(|| general("version 必须是字符串"))?;
    if !is_semver_three(version) {
        return Err(general(format!("version 必须是 SemVer 三段：{version}")));
    }
    Ok(())
}

pub fn check_catalog(c: &Value) -> Result<(), ReleaseError> {
    if c["schema_version"] != json!(2) {
        return Err(ReleaseError::BadCatalog("schema_version 必须是 2".into()));
    }
    for f in [
        "sequence",
        "generated_at",
        "expires",
        "sources",
        "modules",
        "revoked",
    ] {
        if c.get(f).is_none() {
            return Err(ReleaseError::BadCatalog(format!("缺少必填字段 {f}")));
        }
    }
    if c["sequence"].as_u64().unwrap_or(0) < 1 {
        return Err(ReleaseError::BadCatalog("sequence 必须不小于 1".into()));
    }
    let gen = parse_utc_seconds(
        c["generated_at"]
            .as_str()
            .ok_or_else(|| ReleaseError::BadCatalog("generated_at 必须是字符串".into()))?,
    )?;
    let exp = parse_utc_seconds(
        c["expires"]
            .as_str()
            .ok_or_else(|| ReleaseError::BadCatalog("expires 必须是字符串".into()))?,
    )?;
    if exp <= gen {
        return Err(ReleaseError::BadCatalog(
            "expires 必须晚于 generated_at".into(),
        ));
    }
    if exp - gen > 7 * 86_400 {
        return Err(ReleaseError::BadCatalog(
            "catalog 有效期不得超过 7 天".into(),
        ));
    }
    if c["sources"].as_array().is_none_or(|a| a.is_empty()) {
        return Err(ReleaseError::BadCatalog("sources 不得为空".into()));
    }
    if c["modules"].as_array().is_none() || c["revoked"].as_array().is_none() {
        return Err(ReleaseError::BadCatalog(
            "modules 与 revoked 必须是数组".into(),
        ));
    }
    if c.get("signature").and_then(|s| s.get("pubkey")).is_some() {
        return Err(ReleaseError::BadCatalog(
            "catalog 不得自述 pubkey，验签公钥只来自 sources.json".into(),
        ));
    }
    Ok(())
}

// ---- 打包 ----

/// 校验包内相对路径。
///
/// 直接复用 core 的安装侧判定：打包与安装只有一份规则，不会打出
/// 「打包器接受、启动器拒收」的包。历史上两边各写一套，结果 `.gitignore`
/// 能通过打包却在安装时报「非法的包内路径」。
pub fn safe_package_path(rel: &str) -> Result<(), ReleaseError> {
    launcher_core::m2::manifest::validate_relative_path(rel)
        .map_err(|e| ReleaseError::BadPath(e.to_string()))
}

fn hash_reader<R: Read>(mut reader: R) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

/// 扫描源目录，返回包内路径到 sha256 的映射。manifest.json 自身不计入。
pub fn scan_files(dir: &Path) -> Result<BTreeMap<String, String>, ReleaseError> {
    let mut out = BTreeMap::new();
    for entry in walkdir::WalkDir::new(dir).sort_by_file_name() {
        let entry = entry.map_err(|e| ReleaseError::BadPath(e.to_string()))?;
        let meta = entry
            .metadata()
            .map_err(|e| ReleaseError::BadPath(e.to_string()))?;
        if meta.is_dir() {
            continue;
        }
        if meta.file_type().is_symlink() {
            return Err(ReleaseError::BadPath(format!(
                "拒绝符号链接：{}",
                entry.path().display()
            )));
        }
        let rel = entry
            .path()
            .strip_prefix(dir)
            .map_err(|e| ReleaseError::BadPath(e.to_string()))?
            .to_string_lossy()
            .replace('\\', "/");
        if rel == "manifest.json" {
            continue;
        }
        safe_package_path(&rel)?;
        out.insert(rel, hash_reader(File::open(entry.path())?)?);
    }
    Ok(out)
}

/// 写出 zip。manifest 在最前，其余文件按路径排序，保证同输入产出稳定。
pub fn write_zip(
    out: &Path,
    dir: &Path,
    manifest_text: &str,
    files: &BTreeMap<String, String>,
) -> Result<(), ReleaseError> {
    let mut zw = zip::ZipWriter::new(File::create(out)?);
    let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zw.start_file("manifest.json", opts)?;
    zw.write_all(manifest_text.as_bytes())?;
    for rel in files.keys() {
        safe_package_path(rel)?;
        zw.start_file(rel.clone(), opts)?;
        let mut f = File::open(dir.join(rel))?;
        io::copy(&mut f, &mut zw)?;
    }
    zw.finish()?;
    Ok(())
}

/// zip 的验签结果，同时给出填 catalog 条目要用的整体哈希与字节数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageReport {
    pub module_id: String,
    pub version: String,
    pub file_count: usize,
    pub sha256: String,
    pub size: u64,
}

/// 发布前复核 zip：包内 manifest 验签、逐文件哈希、文件集合一致。
///
/// 签名覆盖的是剔除 `signature` 后的整份 manifest，因此 `files` 清单在覆盖内。
/// 先验签再比哈希，清单被改时会在第一步失败。
pub fn verify_zip(path: &Path, pubkey_b64: &str) -> Result<PackageReport, ReleaseError> {
    let size = std::fs::metadata(path)?.len();
    let sha256 = hash_reader(File::open(path)?)?;
    let mut archive = zip::ZipArchive::new(File::open(path)?)?;
    let names: BTreeSet<String> = archive
        .file_names()
        .map(|n| n.map(|cow| cow.into_owned()))
        .collect::<Result<_, _>>()?;
    if !names.contains("manifest.json") {
        return Err(ReleaseError::BadManifest("zip 内缺少 manifest.json".into()));
    }

    let mut text = String::new();
    archive
        .by_name("manifest.json")?
        .read_to_string(&mut text)?;
    let manifest: Value = serde_json::from_str(&text)?;
    verify_manifest(&manifest, pubkey_b64)?;
    check_manifest_basics(&manifest)?;

    let files = manifest["files"]
        .as_object()
        .ok_or_else(|| ReleaseError::BadManifest("files 必须是对象".into()))?;
    let declared: BTreeSet<&String> = files.keys().collect();
    let actual: BTreeSet<String> = names
        .iter()
        .filter(|n| n.as_str() != "manifest.json")
        .cloned()
        .collect();
    let actual_refs: BTreeSet<&String> = actual.iter().collect();
    if declared != actual_refs {
        return Err(ReleaseError::BadManifest(
            "files 清单与 zip 内文件不一致".into(),
        ));
    }
    for (rel, expected) in files {
        safe_package_path(rel)?;
        let got = hash_reader(archive.by_name(rel)?)?;
        if Some(got.as_str()) != expected.as_str() {
            return Err(ReleaseError::BadManifest(format!("文件哈希不符：{rel}")));
        }
    }

    Ok(PackageReport {
        module_id: manifest["module_id"].as_str().unwrap_or("").to_string(),
        version: manifest["version"].as_str().unwrap_or("").to_string(),
        file_count: files.len(),
        sha256,
        size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signature::verify_manifest;

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[5u8; 32])
    }

    fn manifest() -> Value {
        json!({
            "schema_version": 2,
            "module_id": "dounai/omninexus",
            "name": "OmniNexus",
            "version": "0.3.3",
            "variant": "stable",
            "role": "llm",
            "category": "core",
            "publisher": { "name": "Dounai" },
            "requires": [],
            "entry": { "command": "{venv_python} -m omninexus" },
            "ports": { "canonical": 8102 },
            "permissions": { "network": [], "filesystem": [], "devices": [] },
            "files": {}
        })
    }

    fn catalog() -> Value {
        json!({
            "schema_version": 2,
            "sequence": 1,
            "generated_at": "2026-11-10T00:00:00Z",
            "expires": "2026-11-16T00:00:00Z",
            "sources": [{ "id": "github", "kind": "github", "base_url": "https://example.com", "priority": 1 }],
            "modules": [],
            "revoked": []
        })
    }

    #[test]
    fn unix_timestamps_match_known_values() {
        assert_eq!(format_unix_seconds(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_unix_seconds(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(format_unix_seconds(1_735_689_600), "2025-01-01T00:00:00Z");
    }

    #[test]
    fn time_roundtrip() {
        for secs in [0, 1, 86_400, 1_700_000_000, 2_147_483_647] {
            let text = format_unix_seconds(secs);
            assert_eq!(parse_utc_seconds(&text).unwrap(), secs);
        }
    }

    #[test]
    fn rejects_non_utc_time() {
        assert!(parse_utc_seconds("2026-11-10T00:00:00.000Z").is_err());
        assert!(parse_utc_seconds("2026-11-10T00:00:00+08:00").is_err());
        assert!(parse_utc_seconds("2026-13-10T00:00:00Z").is_err());
    }

    #[test]
    fn manifest_signature_roundtrip() {
        let sk = key();
        let mut m = manifest();
        sign_manifest(&mut m, &sk, "K1").unwrap();
        let pk = B64.encode(sk.verifying_key().to_bytes());
        assert!(verify_manifest(&m, &pk).is_ok());
        assert_eq!(m["signature"]["pubkey"], json!(pk));
        m["entry"]["command"] = json!("evil.exe");
        assert!(verify_manifest(&m, &pk).is_err());
    }

    #[test]
    fn replaced_pubkey_fails_verification() {
        let sk = key();
        let mut m = manifest();
        sign_manifest(&mut m, &sk, "K1").unwrap();
        let other = SigningKey::from_bytes(&[6u8; 32]);
        m["signature"]["pubkey"] = json!(B64.encode(other.verifying_key().to_bytes()));
        let pk = B64.encode(other.verifying_key().to_bytes());
        assert!(verify_manifest(&m, &pk).is_err());
    }

    #[test]
    fn catalog_signature_roundtrip() {
        let sk = key();
        let mut c = catalog();
        sign_catalog_doc(&mut c, &sk, "K5").unwrap();
        let pk = B64.encode(sk.verifying_key().to_bytes());
        assert!(verify_manifest(&c, &pk).is_ok());
        assert_eq!(c["signature"]["key_id"], json!("K5"));
        assert_eq!(
            c["signature"]["signer_fp"],
            json!(launcher_core::m2::fingerprint(&pk).unwrap())
        );
        assert!(c["signature"].get("pubkey").is_none());
        c["sequence"] = json!(99);
        assert!(verify_manifest(&c, &pk).is_err());
    }

    #[test]
    fn manifest_basics_reject_unknown_role_and_bad_version() {
        assert!(check_manifest_basics(&manifest()).is_ok());
        let mut m = manifest();
        m["role"] = json!("new_role");
        assert!(check_manifest_basics(&m).is_err());
        let mut m = manifest();
        m["version"] = json!("0.3.3-agent");
        assert!(check_manifest_basics(&m).is_err());
        let mut m = manifest();
        m["version"] = json!("01.2.3");
        assert!(check_manifest_basics(&m).is_err());
    }

    #[test]
    fn role_ids_come_from_embedded_contract() {
        let ids = role_ids().unwrap();
        assert_eq!(ids.len(), 5);
        assert!(ids.contains("auxiliary"));
    }

    #[test]
    fn catalog_rejects_expired_or_long_lived_index() {
        assert!(check_catalog(&catalog()).is_ok());
        let mut c = catalog();
        c["expires"] = json!("2026-11-10T00:00:00Z");
        assert!(check_catalog(&c).is_err());
        let mut c = catalog();
        c["expires"] = json!("2026-12-10T00:00:00Z");
        assert!(check_catalog(&c).is_err());
        let mut c = catalog();
        c["sources"] = json!([]);
        assert!(check_catalog(&c).is_err());
    }

    #[test]
    fn rejects_unsafe_package_paths() {
        for p in [
            "../evil", "a/../b", "/abs", "C:/x", "a\\b", "CON", "COM1.txt", "a//b", "",
        ] {
            assert!(safe_package_path(p).is_err(), "{p} 应当被拒");
        }
        for p in ["main.py", "src/app.py", "assets/voice-1.wav", ".gitignore"] {
            assert!(safe_package_path(p).is_ok(), "{p} 应当通过");
        }
    }

    #[test]
    fn scan_and_zip_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("manifest.json"), "{}\n").unwrap();
        std::fs::create_dir(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.py"), b"print('hi')\n").unwrap();
        let files = scan_files(dir).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files.contains_key("src/main.py"));
        assert!(files["src/main.py"].starts_with("sha256:"));

        let zip_path = tmp.path().join("out.zip");
        write_zip(&zip_path, dir, "{\"signed\":true}\n", &files).unwrap();
        let mut archive = zip::ZipArchive::new(File::open(&zip_path).unwrap()).unwrap();
        let mut names: Vec<String> = archive
            .file_names()
            .map(|n| n.unwrap().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["manifest.json", "src/main.py"]);
        let mut body = String::new();
        archive
            .by_name("src/main.py")
            .unwrap()
            .read_to_string(&mut body)
            .unwrap();
        assert_eq!(body, "print('hi')\n");
    }

    #[test]
    fn generated_keypair_works() {
        let (sk, pk) = generate_keypair().unwrap();
        let mut m = manifest();
        sign_manifest(&mut m, &sk, "K1").unwrap();
        assert!(verify_manifest(&m, &pk).is_ok());
    }

    fn manifest_from_zip(path: &Path) -> Value {
        let mut archive = zip::ZipArchive::new(File::open(path).unwrap()).unwrap();
        let mut text = String::new();
        archive
            .by_name("manifest.json")
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        serde_json::from_str(&text).unwrap()
    }

    /// 建一个可验的小型发布包，返回 zip 路径、文件清单与公钥。
    fn build_package(dir: &Path) -> (std::path::PathBuf, BTreeMap<String, String>, String) {
        let src = dir.join("src");
        std::fs::create_dir_all(src.join("app")).unwrap();
        std::fs::write(src.join("app/main.py"), b"print('hi')\n").unwrap();
        let sk = key();
        let files = scan_files(&src).unwrap();
        let mut m = manifest();
        m["files"] = json!(files);
        sign_manifest(&mut m, &sk, "K1").unwrap();
        let zip_path = dir.join("pkg.zip");
        write_zip(&zip_path, &src, &serde_json::to_string(&m).unwrap(), &files).unwrap();
        let pk = B64.encode(sk.verifying_key().to_bytes());
        (zip_path, files, pk)
    }

    /// `files` 决定 zip 里实际写入哪些文件，与 manifest 内的清单可以不同。
    fn repack(dir: &Path, files: &BTreeMap<String, String>, m: &Value) -> std::path::PathBuf {
        let zip_path = dir.join("bad.zip");
        write_zip(
            &zip_path,
            &dir.join("src"),
            &serde_json::to_string(m).unwrap(),
            files,
        )
        .unwrap();
        zip_path
    }

    #[test]
    fn pack_then_verify_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let (zip_path, _, pk) = build_package(tmp.path());
        let report = verify_zip(&zip_path, &pk).unwrap();
        assert_eq!(report.module_id, "dounai/omninexus");
        assert_eq!(report.version, "0.3.3");
        assert_eq!(report.file_count, 1);
        assert!(report.sha256.starts_with("sha256:"));
        assert!(report.size > 0);
    }

    #[test]
    fn verify_rejects_wrong_key_and_stripped_signature() {
        let tmp = tempfile::tempdir().unwrap();
        let (zip_path, files, pk) = build_package(tmp.path());
        let other = SigningKey::from_bytes(&[8u8; 32]);
        let wrong = B64.encode(other.verifying_key().to_bytes());
        assert!(verify_zip(&zip_path, &wrong).is_err());

        let mut m = manifest_from_zip(&zip_path);
        m["signature"] = Value::Null;
        let stripped = repack(tmp.path(), &files, &m);
        assert!(verify_zip(&stripped, &pk).is_err());
    }

    #[test]
    fn verify_rejects_list_mismatch_and_tampered_file() {
        let tmp = tempfile::tempdir().unwrap();
        let (zip_path, mut files, pk) = build_package(tmp.path());
        let src = tmp.path().join("src");
        let sk = key();

        // 清单多一个键，zip 里没有这个文件。
        files.insert("app/missing.py".into(), "sha256:00".into());
        let mut m = manifest_from_zip(&zip_path);
        m["files"] = json!(files);
        sign_manifest(&mut m, &sk, "K1").unwrap();
        let extra = repack(tmp.path(), &BTreeMap::new(), &m);
        assert!(verify_zip(&extra, &pk).is_err());

        // 源文件内容变了，清单仍写旧哈希。
        files.remove("app/missing.py");
        std::fs::write(src.join("app/main.py"), b"print('evil')\n").unwrap();
        m["files"] = json!(files);
        sign_manifest(&mut m, &sk, "K1").unwrap();
        let tampered = repack(tmp.path(), &files, &m);
        assert!(verify_zip(&tampered, &pk).is_err());
    }
}
