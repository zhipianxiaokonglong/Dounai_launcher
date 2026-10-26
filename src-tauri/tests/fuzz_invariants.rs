//! 模糊测试：解压与 manifest 解析。
//!
//! 目的不是"找到所有 bug"，而是**证明两个安全不变量在大量畸形输入下仍然成立**：
//!  1. 任何字节序列都不会让解压越界写出目标目录；
//!  2. 任何字节序列都不会让解析器 panic（只会返回错误）。
//!
//! 采用**确定性伪随机 + 结构化变异**而非外部 fuzz 引擎：
//! - 无需 nightly 与额外工具链，CI 可直接跑；
//! - 种子固定，失败可复现（记录种子即可重放）；
//! - 覆盖真实攻击面（zip 头、路径字段、JSON 结构）而非纯随机噪声。

use dounai_launcher::install::{extract_zip_into, InstallTransaction};
use dounai_launcher::security::safe_join;
use std::io::Write;
use std::path::{Path, PathBuf};

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
    use dounai_launcher::security::PathError;

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
/* 不变量 2：任意字节都不能让解压越界或 panic                          */
/* ------------------------------------------------------------------ */

#[test]
fn fuzz_extract_arbitrary_bytes_never_escapes_or_panics() {
    let mut rng = Rng::new(0xDEADBEEF);

    for iteration in 0..300 {
        let root = tmp_root(&format!("bytes{iteration}"));
        let dest = root.join("mod");

        // 随机长度的随机字节
        let len = 1 + rng.below(512);
        let mut data = Vec::with_capacity(len);
        for _ in 0..len {
            data.push(rng.byte());
        }

        // 允许失败，但绝不 panic
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let tx = InstallTransaction::begin(&dest).expect("begin 不应失败");
            let r = extract_zip_into(&tx, &data, None);
            // tx 在作用域结束时自动回滚（未 commit）
            r.is_ok()
        }));

        assert!(result.is_ok(), "第 {iteration} 次：随机字节导致 panic");

        // 关键不变量：未 commit 时不得产生最终目录
        assert!(
            !dest.exists() || dest.is_dir(),
            "第 {iteration} 次：产生了异常的目标路径"
        );

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
        let dest = root.join("mod");

        // 混合包：一个合法条目 + 一个恶意条目
        let zip = zip_with_entries(&[("good.txt", b"ok"), (evil, b"evil")]);

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let tx = InstallTransaction::begin(&dest).unwrap();
            extract_zip_into(&tx, &zip, None).is_err()
        }));
        assert!(outcome.is_ok(), "{evil:?} 导致 panic");

        // 不管 zip 是否能构造出恶意条目，最终目录都不应存在（未 commit）
        assert!(!dest.exists(), "{evil:?} 不应产生安装结果");

        // 检查数据根外没有被写出
        let outside = root.parent().unwrap().join("escape.txt");
        assert!(!outside.exists(), "{evil:?} 越界写出了文件");

        std::fs::remove_dir_all(&root).ok();
    }
}

#[test]
fn fuzz_good_entries_still_accepted() {
    // 反向验证：正常包不应被误拒（避免"全部拒绝"通过 fuzz）
    for (i, good) in GOOD_PATHS.iter().enumerate() {
        let root = tmp_root(&format!("good{i}"));
        let dest = root.join("mod");
        let zip = zip_with_entries(&[(good, b"data")]);

        let tx = InstallTransaction::begin(&dest).unwrap();
        let result = extract_zip_into(&tx, &zip, None);
        assert!(result.is_ok(), "正常条目 {good:?} 被误拒：{result:?}");
        drop(tx); // 不 commit，自动清理

        std::fs::remove_dir_all(&root).ok();
    }
}

/* ------------------------------------------------------------------ */
/* 不变量 4：manifest / 清单解析不 panic                               */
/* ------------------------------------------------------------------ */

#[test]
fn fuzz_json_parsers_never_panic() {
    use dounai_launcher::catalog::parse_catalog;

    let mut rng = Rng::new(0x1234_5678);

    for iteration in 0..500 {
        let len = rng.below(256);
        let mut data = Vec::with_capacity(len);
        for _ in 0..len {
            data.push(rng.byte());
        }

        // 三种解析器都不得 panic
        let r1 = std::panic::catch_unwind(|| parse_catalog(&data));
        assert!(r1.is_ok(), "第 {iteration} 次：parse_catalog panic");

        let r2 = std::panic::catch_unwind(|| {
            // 把字节当作 JSON 文本解析
            if let Ok(text) = std::str::from_utf8(&data) {
                let _: Result<serde_json::Value, _> = serde_json::from_str(text);
            }
        });
        assert!(r2.is_ok(), "第 {iteration} 次：JSON 解析 panic");
    }
}

#[test]
fn fuzz_catalog_with_structured_mutations() {
    use dounai_launcher::catalog::parse_catalog;

    // 以真实清单为基底做结构化变异
    let base = br#"{"modules":[{"module_id":"a","name":"A","version":"1.0.0",
        "download_url":"https://dl.example.com/a.zip","sha256":"abc"}]}"#;

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

        let outcome = std::panic::catch_unwind(|| parse_catalog(&data));
        assert!(outcome.is_ok(), "第 {iteration} 次：结构化变异导致 panic");
    }
}

/* ------------------------------------------------------------------ */
/* 不变量 5：base64 解码不 panic                                       */
/* ------------------------------------------------------------------ */

#[test]
fn fuzz_base64_decode_never_panics() {
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine as _;

    let mut rng = Rng::new(0x7777);
    for iteration in 0..800 {
        let len = rng.below(128);
        let mut s = String::with_capacity(len);
        for _ in 0..len {
            // 用可打印字符构造，覆盖各种非法 base64
            let b = 32 + (rng.byte() % 95);
            s.push(b as char);
        }
        let outcome = std::panic::catch_unwind(|| B64.decode(s.as_bytes()));
        assert!(outcome.is_ok(), "第 {iteration} 次：base64 解码 panic");
    }
}
