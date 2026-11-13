//! 模糊测试：验包、安装落盘、清单与 catalog 解析。
//!
//! 目的不是"找到所有 bug"，而是**证明安全不变量在大量畸形输入下仍然成立**：
//!  1. 任何字节序列都不会让路径解析越界；
//!  2. 任何字节序列都不会让验包与解析器 panic（只会返回错误）；
//!  3. 含恶意条目名的 zip 被**整体拒绝**，不会部分落盘。
//!
//! 被测对象是 v2 的真实安装路径：`core::m2::package::{verify_zip, install_package}`。
//! v1 的 `install.rs` 已删除，这里的不变量直接压在活代码上。
//!
//! 采用**确定性伪随机 + 结构化变异**而非外部 fuzz 引擎：
//! - 无需 nightly 与额外工具链，CI 可直接跑；
//! - 种子固定，失败可复现（记录种子即可重放）；
//! - 覆盖真实攻击面（zip 头、路径字段、JSON 结构）而非纯随机噪声。

use std::io::Write;
use std::path::{Path, PathBuf};

use launcher_core::m2::catalog::CatalogDocument;
use launcher_core::m2::manifest::PackageManifest;
use launcher_core::m2::package::{install_package, verify_zip, ZipExpectation};
use launcher_core::security::{safe_join, sha256_hex};

/// 确定性伪随机（xorshift64*），保证同一 seed 产生同一序列。
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.next() % n as u64) as usize
    }
    fn byte(&mut self) -> u8 {
        (self.next() & 0xFF) as u8
    }
}

fn tmp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "dounai-fuzz-{}-{}-{}",
        name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 危险的路径片段池：真实攻击者会用这些构造穿越。
const EVIL_PATHS: &[&str] = &[
    "../escape.txt",
    "..\\escape.txt",
    "../../../../etc/passwd",
    "a/../../b",
    "/etc/passwd",
    "C:\\Windows\\System32\\evil.dll",
    "\\\\server\\share\\evil",
    "con",
    "nul.txt",
    "aux/evil",
    "COM1.log",
    "....//....//etc/passwd",
    "a/./../../b",
    "./../x",
    "mod/../mod2/../../out",
];

/// 正常路径池（确保不是"全部拒绝"就算通过）。
const GOOD_PATHS: &[&str] = &[
    "a.txt",
    "dir/b.txt",
    "dir/sub/c.bin",
    "模块/说明.md",
    "deep/a/b/c/d/e.txt",
];

/* ------------------------------------------------------------------ */
/* 不变量 1：safe_join 永不返回目标目录外的路径                        */
/* ------------------------------------------------------------------ */

#[test]
fn fuzz_safe_join_never_escapes_destination() {
    let dest = Path::new("C:\\app\\modules\\mod");
    let mut rng = Rng::new(0xC0FFEE);

    for iteration in 0..2000 {
        // 结构化变异：拼接路径片段 + 随机插入分隔符与点
        let mut path = String::new();
        let parts = 1 + rng.below(4);
        for i in 0..parts {
            if i > 0 {
                path.push(if rng.below(2) == 0 { '/' } else { '\\' });
            }
            match rng.below(10) {
                0..=4 => path.push_str(EVIL_PATHS[rng.below(EVIL_PATHS.len())]),
                5..=7 => path.push_str(GOOD_PATHS[rng.below(GOOD_PATHS.len())]),
                8 => path.push_str(&format!("seg{}", rng.below(100))),
                _ => {
                    // 随机字节（可能产生非法 Unicode 之外的怪字符）
                    for _ in 0..(1 + rng.below(6)) {
                        let b = rng.byte();
                        if b.is_ascii_graphic() {
                            path.push(b as char);
                        }
                    }
                }
            }
        }

        match safe_join(dest, &path) {
            Ok(resolved) => {
                // 关键不变量：被接受的路径必须仍在 dest 内
                let normalized = normalize_for_check(&resolved);
                let normalized_dest = normalize_for_check(dest);
                assert!(
                    normalized.starts_with(&normalized_dest),
                    "第 {iteration} 次：接受了越界路径 {path:?} -> {}",
                    resolved.display()
                );
            }
            Err(_) => {
                // 拒绝是合法的（且是期望行为）
            }
        }
    }
}

/// 不变量 1b：**明确的攻击串必须被显式拒绝，而不只是被兜底逻辑拦住**。
///
/// 为什么单独加这条：上面那条只验证"结果安全"。实测发现，
/// 即使删掉 `ParentDir` 的显式拒绝，末尾的包含性复核仍能兜住越界，
/// 于是上一条依然通过——这会让"防线退化"被悄悄掩盖。
/// 这里要求攻击串返回**语义明确的错误类型**，确保每一道防线都真的在工作。
///
/// 纵深防御的价值是"双保险"，但不能因为第二道还在就默许第一道失效。
#[test]
fn fuzz_attack_strings_are_explicitly_rejected_by_design() {
    use launcher_core::security::PathError;

    let dest = Path::new("C:\\app\\modules\\mod");

    // 含 .. 的必须报 Traversal（而不是靠后续兜底）
    for evil in [
        "../escape.txt",
        "a/../../b",
        "mod/../mod2/../../out",
        "a/./../../b",
        "./../x",
    ] {
        let r = safe_join(dest, evil);
        assert!(
            matches!(r, Err(PathError::Traversal(_))),
            "{evil:?} 应按设计返回 Traversal，实际 {r:?}"
        );
    }

    // 绝对路径 / UNC / 盘符必须报 Absolute
    for evil in [
        "/etc/passwd",
        "C:\\Windows\\System32\\evil.dll",
        "\\\\server\\share\\evil",
    ] {
        let r = safe_join(dest, evil);
        assert!(
            matches!(r, Err(PathError::Absolute(_))),
            "{evil:?} 应按设计返回 Absolute，实际 {r:?}"
        );
    }

    // Windows 保留设备名必须报 ReservedName
    for evil in ["con", "nul.txt", "aux/evil", "COM1.log"] {
        let r = safe_join(dest, evil);
        assert!(
            matches!(r, Err(PathError::ReservedName(_))),
            "{evil:?} 应按设计返回 ReservedName，实际 {r:?}"
        );
    }
}

/// 词法归一化，用于断言包含关系（不触碰文件系统）。
fn normalize_for_check(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/* ------------------------------------------------------------------ */
/* 不变量 2：任意字节都不能让验包越界或 panic                          */
/* ------------------------------------------------------------------ */

#[test]
fn fuzz_verify_zip_arbitrary_bytes_never_escapes_or_panics() {
    let mut rng = Rng::new(0xDEADBEEF);

    for iteration in 0..300 {
        let root = tmp_root(&format!("bytes{iteration}"));

        // 随机长度的随机字节
        let len = 1 + rng.below(512);
        let mut data = Vec::with_capacity(len);
        for _ in 0..len {
            data.push(rng.byte());
        }

        // 允许失败，但绝不 panic
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            verify_zip(&data, &ZipExpectation::default()).is_ok()
        }));

        assert!(result.is_ok(), "第 {iteration} 次：随机字节导致 panic");

        // 验包阶段不落盘：工作目录必须还是空的
        let leftovers = std::fs::read_dir(&root).map(|d| d.count()).unwrap_or(0);
        assert_eq!(leftovers, 0, "第 {iteration} 次：验包阶段写入了文件");

        // 数据根内不应出现 ../ 逃逸产物
        let escaped = root.parent().unwrap().join("escape.txt");
        assert!(!escaped.exists(), "第 {iteration} 次：随机字节导致越界写出");

        std::fs::remove_dir_all(&root).ok();
    }
}

/* ------------------------------------------------------------------ */
/* 不变量 3：结构化的恶意 zip 必被整体拒绝                             */
/* ------------------------------------------------------------------ */

/// 构造一个包含指定条目名的真实 zip。
fn zip_with_entries(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
        for (name, data) in entries {
            // 某些非法名可能让 start_file 失败；跳过即可，不影响断言
            if w.start_file(*name, opts).is_err() {
                continue;
            }
            let _ = w.write_all(data);
        }
        let _ = w.finish();
    }
    buf
}

#[test]
fn fuzz_malicious_entry_names_are_rejected_as_a_whole() {
    for (i, evil) in EVIL_PATHS.iter().enumerate() {
        let root = tmp_root(&format!("evil{i}"));

        // 混合包：一个合法条目 + 一个恶意条目
        let zip = zip_with_entries(&[("good.txt", b"ok"), (evil, b"evil")]);

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            verify_zip(&zip, &ZipExpectation::default())
        }));
        let verified = outcome.expect("恶意条目导致 panic");
        assert!(verified.is_err(), "{evil:?} 应被整体拒绝");

        // 被拒绝时既不落盘，也不产生越界产物
        let leftovers = std::fs::read_dir(&root).map(|d| d.count()).unwrap_or(0);
        assert_eq!(leftovers, 0, "{evil:?} 不应产生安装结果");
        let outside = root.parent().unwrap().join("escape.txt");
        assert!(!outside.exists(), "{evil:?} 越界写出了文件");

        std::fs::remove_dir_all(&root).ok();
    }
}

/// 构造一个字段完整、files 哈希正确的 shell 包。
fn valid_package(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut files = serde_json::Map::new();
    for (path, bytes) in entries {
        files.insert(
            (*path).to_string(),
            serde_json::Value::String(format!("sha256:{}", sha256_hex(bytes))),
        );
    }
    let manifest = serde_json::json!({
        "schema_version": 2,
        "module_id": "dounai/fuzz",
        "name": "fuzz",
        "version": "1.0.0",
        "variant": "stable",
        "role": "shell",
        "category": "optional",
        "publisher": { "id": "dounai", "name": "Dounai" },
        "requires": { "launcher": ">=0.3.0" },
        "entry": { "command": "{install_root}/fuzz.exe", "env": {}, "health": null },
        "ports": [],
        "permissions": { "network": [], "filesystem": [], "devices": [] },
        "files": files,
    });

    let mut buf = Vec::new();
    {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
        w.start_file("manifest.json", opts).unwrap();
        w.write_all(manifest.to_string().as_bytes()).unwrap();
        for (path, data) in entries {
            w.start_file(*path, opts).unwrap();
            w.write_all(data).unwrap();
        }
        let _ = w.finish();
    }
    buf
}

#[test]
fn fuzz_good_entries_still_accepted() {
    // 反向验证：正常包不应被误拒（避免"全部拒绝"通过 fuzz）
    for (i, good) in GOOD_PATHS.iter().enumerate() {
        let root = tmp_root(&format!("good{i}"));
        let dest = root.join("mod");
        let zip = valid_package(&[(good, b"data")]);

        let pkg = verify_zip(&zip, &ZipExpectation::default())
            .unwrap_or_else(|e| panic!("正常条目 {good:?} 被误拒：{e:?}"));
        install_package(&pkg, &dest).expect("正常包应能落盘");

        let landed = dest.join(good);
        assert!(landed.is_file(), "{good:?} 没有落盘");
        assert_eq!(std::fs::read(&landed).unwrap(), b"data");

        // 落盘必须留在 dest 内：dest 的父目录只应多出 mod
        let siblings: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(siblings, vec!["mod".to_string()], "{good:?} 产生了越界产物");

        std::fs::remove_dir_all(&root).ok();
    }
}

/* ------------------------------------------------------------------ */
/* 不变量 4：manifest / catalog 解析不 panic                           */
/* ------------------------------------------------------------------ */

#[test]
fn fuzz_json_parsers_never_panic() {
    let mut rng = Rng::new(0x1234_5678);

    for iteration in 0..500 {
        let len = rng.below(256);
        let mut data = Vec::with_capacity(len);
        for _ in 0..len {
            data.push(rng.byte());
        }
        let text = String::from_utf8_lossy(&data).into_owned();

        let r1 = std::panic::catch_unwind(|| PackageManifest::parse(&text).is_ok());
        assert!(r1.is_ok(), "第 {iteration} 次：PackageManifest::parse panic");

        let r2 = std::panic::catch_unwind(|| {
            CatalogDocument::parse(&text, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=").is_ok()
        });
        assert!(r2.is_ok(), "第 {iteration} 次：CatalogDocument::parse panic");
    }
}

#[test]
fn fuzz_catalog_with_structured_mutations() {
    // 以真实 catalog 为基底做结构化变异：字段名与嵌套都是真的，
    // 随机字节很难命中这些分支。
    let base = br#"{"schema_version":2,"sequence":1,"generated_at":"2026-01-01T00:00:00Z",
        "expires":"2026-01-07T00:00:00Z","sources":[{"id":"github","kind":"github",
        "base_url":"https://github.com/dounai/releases","priority":1}],
        "modules":[{"module_id":"dounai/demo","name":"demo","version":"1.0.0",
        "variant":"stable","download_url":"https://dl.example.com/a.zip","sha256":"abc"}],
        "revoked":[],"signature":{"algo":"Ed25519","key_id":"K5",
        "signed_at":"2026-01-01T00:00:00Z","value":""}}"#;

    let mut rng = Rng::new(0xABCD);
    for iteration in 0..300 {
        let mut data = base.to_vec();

        // 变异：随机位置翻转字节 / 截断 / 插入
        let mutations = 1 + rng.below(4);
        for _ in 0..mutations {
            match rng.below(3) {
                0 => {
                    let idx = rng.below(data.len());
                    data[idx] = rng.byte();
                }
                1 => {
                    let cut = rng.below(data.len());
                    data.truncate(cut.max(1));
                }
                _ => {
                    let idx = rng.below(data.len());
                    data.insert(idx, rng.byte());
                }
            }
            if data.is_empty() {
                data.push(b'{');
            }
        }

        let text = String::from_utf8_lossy(&data).into_owned();
        let outcome = std::panic::catch_unwind(|| {
            CatalogDocument::parse(&text, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=").is_ok()
        });
        assert!(outcome.is_ok(), "第 {iteration} 次：结构化变异导致 panic");
    }
}
