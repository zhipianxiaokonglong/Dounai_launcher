//! manifest 签名工具（密钥交接的配套工具）。
//!
//! 为什么需要独立工具：验签逻辑在库里，但**签名**需要私钥，
//! 而私钥绝不能出现在启动器二进制里。把签名做成离线 CLI，
//! 让运维在发布机上跑，启动器只带公钥。
//!
//! 用法：
//! ```text
//! # 1) 生成密钥对（一次性）
//! dounai-sign keygen --out-private priv.b64 --out-public pub.b64
//!
//! # 2) 给 manifest 签名（就地写回 signature.value）
//! dounai-sign sign --key priv.b64 --manifest manifests/omninexus.json
//!
//! # 3) 验证（发布前检查）
//! dounai-sign verify --pubkey pub.b64 --manifest manifests/omninexus.json
//! ```
//!
//! 安全约定：
//! - 私钥只以 base64 文本形式在本地文件间传递，不打印到 stdout（除非显式 --print）；
//! - 签名前会校验 manifest 必填字段，避免把残缺内容签成"可信"。

use std::process::ExitCode;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey};

use dounai_launcher::signature::{canonical_payload, verify_manifest, SUPPORTED_ALGO};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        print_usage();
        return ExitCode::from(2);
    }

    let result = match args[0].as_str() {
        "keygen" => cmd_keygen(&args[1..]),
        "sign" => cmd_sign(&args[1..]),
        "verify" => cmd_verify(&args[1..]),
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(())
        }
        other => Err(format!("未知子命令：{other}")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("错误：{e}");
            ExitCode::FAILURE
        }
    }
}

fn print_usage() {
    eprintln!(
        "Dounai manifest 签名工具\n\
         \n\
         用法:\n\
         \x20 dounai-sign keygen --out-private <路径> --out-public <路径>\n\
         \x20 dounai-sign sign   --key <私钥文件> --manifest <manifest 路径> [--out <输出路径>]\n\
         \x20 dounai-sign verify --pubkey <公钥文件> --manifest <manifest 路径>\n\
         \n\
         说明:\n\
         \x20 sign 默认就地覆盖原文件；指定 --out 则写到新文件。\n\
         \x20 私钥文件请勿提交到仓库（.gitignore 已排除 *.key / *.b64）。"
    )
}

/// 解析 `--flag value` 形式的参数。
fn flag(args: &[String], name: &str) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == name {
            return args.get(i + 1).cloned();
        }
        i += 1;
    }
    None
}

fn require_flag(args: &[String], name: &str) -> Result<String, String> {
    flag(args, name).ok_or_else(|| format!("缺少参数 {name}"))
}

fn cmd_keygen(args: &[String]) -> Result<(), String> {
    let priv_path = require_flag(args, "--out-private")?;
    let pub_path = require_flag(args, "--out-public")?;

    // 用系统随机源生成；不要用固定种子（那等于没有密钥）
    let mut seed = [0u8; 32];
    getrandom_fill(&mut seed)?;
    let sk = SigningKey::from_bytes(&seed);
    let pk = sk.verifying_key();

    std::fs::write(&priv_path, B64.encode(sk.to_bytes()))
        .map_err(|e| format!("写私钥失败：{e}"))?;
    std::fs::write(&pub_path, B64.encode(pk.to_bytes())).map_err(|e| format!("写公钥失败：{e}"))?;

    // 只输出路径与公钥（公钥可公开），私钥内容不打印
    println!("已生成密钥对");
    println!("  私钥：{priv_path}（切勿提交或外传）");
    println!("  公钥：{pub_path}");
    println!("  公钥值：{}", B64.encode(pk.to_bytes()));
    Ok(())
}

/// 用系统随机源填充。
///
/// 走 OS 随机数（Windows 下 BCryptGenRandom，其他平台 /dev/urandom），
/// 不引入额外的 rand 依赖，避免为了 32 字节随机数扩大供应链面。
fn getrandom_fill(buf: &mut [u8]) -> Result<(), String> {
    #[cfg(windows)]
    {
        // 用 std 的 RandomState 不足以保证密码学强度，因此走 BCryptGenRandom
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
            return Err(format!("系统随机数生成失败（NTSTATUS {status}）"));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        use std::io::Read;
        let mut f = std::fs::File::open("/dev/urandom")
            .map_err(|e| format!("打开 /dev/urandom 失败：{e}"))?;
        f.read_exact(buf)
            .map_err(|e| format!("读取随机数失败：{e}"))?;
        Ok(())
    }
}

fn read_manifest(path: &str) -> Result<serde_json::Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("读取 manifest 失败：{e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("manifest 不是合法 JSON：{e}"))
}

/// 签名前校验必填字段，避免把残缺文件签成"可信"。
fn require_fields(manifest: &serde_json::Value) -> Result<(), String> {
    for field in ["module_id", "version", "category", "entry"] {
        if manifest.get(field).is_none() {
            return Err(format!("manifest 缺少必填字段 {field}，拒绝签名"));
        }
    }
    Ok(())
}

fn cmd_sign(args: &[String]) -> Result<(), String> {
    let key_path = require_flag(args, "--key")?;
    let manifest_path = require_flag(args, "--manifest")?;

    let key_b64 = std::fs::read_to_string(&key_path)
        .map_err(|e| format!("读取私钥失败：{e}"))?
        .trim()
        .to_string();
    let raw = B64
        .decode(key_b64.as_bytes())
        .map_err(|e| format!("私钥不是合法 base64：{e}"))?;
    let seed: [u8; 32] = raw
        .try_into()
        .map_err(|_| "私钥必须是 32 字节".to_string())?;
    let sk = SigningKey::from_bytes(&seed);

    let mut manifest = read_manifest(&manifest_path)?;
    require_fields(&manifest)?;

    // 先写入算法标识，再用剔除 signature 后的规范化内容签名
    manifest["signature"] = serde_json::json!({ "algo": SUPPORTED_ALGO, "value": "" });
    let payload = canonical_payload(&manifest).map_err(|e| format!("规范化失败：{e}"))?;
    let sig = sk.sign(&payload);
    manifest["signature"]["value"] = serde_json::json!(B64.encode(sig.to_bytes()));

    let out_path = flag(args, "--out").unwrap_or_else(|| manifest_path.clone());
    let text = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
    std::fs::write(&out_path, format!("{text}\n")).map_err(|e| format!("写入失败：{e}"))?;

    println!("已签名：{out_path}");
    println!("  module_id = {}", manifest["module_id"]);
    println!(
        "  公钥值    = {}",
        B64.encode(sk.verifying_key().to_bytes())
    );
    Ok(())
}

fn cmd_verify(args: &[String]) -> Result<(), String> {
    let pub_path = require_flag(args, "--pubkey")?;
    let manifest_path = require_flag(args, "--manifest")?;

    let pk_b64 = std::fs::read_to_string(&pub_path)
        .map_err(|e| format!("读取公钥失败：{e}"))?
        .trim()
        .to_string();
    let manifest = read_manifest(&manifest_path)?;

    verify_manifest(&manifest, &pk_b64).map_err(|e| format!("验签失败：{e}"))?;
    println!("验签通过：{manifest_path}");
    Ok(())
}

/// 供测试复用：返回签名后的 manifest（不落盘）。
#[allow(dead_code)]
fn sign_value(
    mut manifest: serde_json::Value,
    sk: &SigningKey,
) -> Result<serde_json::Value, String> {
    manifest["signature"] = serde_json::json!({ "algo": SUPPORTED_ALGO, "value": "" });
    let payload = canonical_payload(&manifest).map_err(|e| format!("规范化失败：{e}"))?;
    let sig = sk.sign(&payload);
    manifest["signature"]["value"] = serde_json::json!(B64.encode(sig.to_bytes()));
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Verifier;

    fn sample() -> serde_json::Value {
        serde_json::json!({
            "module_id": "demo",
            "name": "演示",
            "version": "1.0.0",
            "category": "optional",
            "entry": { "command": "run.exe", "type": "command" },
            "dependencies": [],
            "model_packs": [],
            "signature": { "algo": SUPPORTED_ALGO, "value": "" }
        })
    }

    #[test]
    fn sign_then_verify_roundtrip() {
        let sk = SigningKey::from_bytes(&[11u8; 32]);
        let pk_b64 = B64.encode(sk.verifying_key().to_bytes());
        let signed = sign_value(sample(), &sk).unwrap();
        assert!(verify_manifest(&signed, &pk_b64).is_ok());
    }

    #[test]
    fn signed_manifest_is_rejected_after_tamper() {
        let sk = SigningKey::from_bytes(&[12u8; 32]);
        let pk_b64 = B64.encode(sk.verifying_key().to_bytes());
        let mut signed = sign_value(sample(), &sk).unwrap();
        signed["entry"]["command"] = serde_json::json!("evil.exe");
        assert!(verify_manifest(&signed, &pk_b64).is_err());
    }

    #[test]
    fn require_fields_rejects_incomplete_manifest() {
        let mut m = sample();
        m.as_object_mut().unwrap().remove("category");
        let err = require_fields(&m).unwrap_err();
        assert!(err.contains("category"));
    }

    #[test]
    fn require_fields_accepts_complete_manifest() {
        assert!(require_fields(&sample()).is_ok());
    }

    #[test]
    fn flag_parser_reads_value() {
        let args: Vec<String> = ["--key", "a.b64", "--manifest", "m.json"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(flag(&args, "--key").as_deref(), Some("a.b64"));
        assert_eq!(flag(&args, "--manifest").as_deref(), Some("m.json"));
        assert_eq!(flag(&args, "--out"), None);
    }

    #[test]
    fn require_flag_errors_when_missing() {
        let args: Vec<String> = vec!["--key".into()];
        assert!(require_flag(&args, "--manifest").is_err());
    }

    #[test]
    fn getrandom_produces_distinct_output() {
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        getrandom_fill(&mut a).unwrap();
        getrandom_fill(&mut b).unwrap();
        assert_ne!(a, b, "两次随机数不应相同");
        assert_ne!(a, [0u8; 32], "随机数不应全零");
    }

    #[test]
    fn random_seed_yields_working_keypair() {
        let mut seed = [0u8; 32];
        getrandom_fill(&mut seed).unwrap();
        let sk = SigningKey::from_bytes(&seed);
        let msg = b"payload";
        let sig = sk.sign(msg);
        assert!(sk.verifying_key().verify(msg, &sig).is_ok());
    }
}
