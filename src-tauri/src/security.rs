//! 供应链安全防线（涉及供应链与隔离降级两条要求）。
//!
//! 这里刻意把**纯校验逻辑**与 IO 分开：
//! `zip` 解压前先用 `safe_join` 判断每个条目是否越界，越界即整体拒绝，
//! 不做"跳过坏条目继续解压"——部分成功会留下半残环境，比整体失败更糟。
//!
//! 本文件不依赖 external crate，可直接单元测试。

use std::path::{Component, Path, PathBuf};

/// 路径安全错误。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PathError {
    /// 条目路径包含绝对路径或盘符
    #[error("条目使用了绝对路径：{0}")]
    Absolute(String),
    /// 条目路径包含 `..` 越界
    #[error("条目试图越出目标目录：{0}")]
    Traversal(String),
    /// 条目路径为空
    #[error("条目路径为空")]
    Empty,
    /// 条目含 Windows 保留设备名
    #[error("条目使用了保留设备名：{0}")]
    ReservedName(String),
    /// 解压后路径解析失败
    #[error("无法规范化路径：{0}")]
    Unresolvable(String),
}

/// Windows 保留设备名（不区分大小写，且带扩展名也无效）。
///
/// 这些名字在 Windows 上是设备而非文件，攻击者可借此造成写入被重定向
/// 或创建异常文件，因此一律拒绝。
const RESERVED_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// 判断一个压缩条目的相对路径是否安全，并返回拼接后的目标绝对路径。
///
/// 规则：
/// 1. 拒绝绝对路径（`/x`、`C:\x`、`\\server\share`）；
/// 2. 拒绝任何 `..` 组件；
/// 3. 拒绝保留设备名；
/// 4. 解析后必须仍在 `dest` 之内（二次确认，防 `Base 与 Component 组合绕过`）。
pub fn safe_join(dest: &Path, entry: &str) -> Result<PathBuf, PathError> {
    let trimmed = entry.trim().trim_start_matches("./");
    if trimmed.is_empty() {
        return Err(PathError::Empty);
    }

    let candidate = Path::new(trimmed);

    // 规则 1：绝对路径与盘符前缀
    if candidate.is_absolute() {
        return Err(PathError::Absolute(entry.to_string()));
    }
    for comp in candidate.components() {
        match comp {
            Component::Prefix(_) => return Err(PathError::Absolute(entry.to_string())),
            Component::RootDir => return Err(PathError::Absolute(entry.to_string())),
            Component::ParentDir => return Err(PathError::Traversal(entry.to_string())),
            Component::CurDir | Component::Normal(_) => {}
        }
    }

    // 规则 3：保留设备名（检查每个组件的主干名）
    for comp in candidate.components() {
        if let Component::Normal(os) = comp {
            let name = os.to_string_lossy();
            let stem = name.split('.').next().unwrap_or("").to_ascii_lowercase();
            if RESERVED_NAMES.contains(&stem.as_str()) {
                return Err(PathError::ReservedName(entry.to_string()));
            }
        }
    }

    // 规则 4：拼出结果并做包含性复核
    let joined = dest.join(candidate);
    let normalized_dest = normalize(dest);
    let normalized_joined = normalize(&joined);
    if !normalized_joined.starts_with(&normalized_dest) {
        return Err(PathError::Unresolvable(entry.to_string()));
    }
    Ok(joined)
}

/// 仅做 `.` 与 `..` 的词法归一化，不触碰文件系统。
///
/// 不用 `canonicalize`：解压目标目录里的文件可能尚不存在，`canonicalize`
/// 会直接失败；而这里只需要阻止路径越界，词法归一化已经足够，
/// 且不会因符号链接解析带来 TOCTOU 风险。
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// 判断一个文件名（不含目录）是否是安全的普通文件/目录名。
///
/// 与 `safe_join` 共用同一套保留名规则，供「模块 id → 目录名」这类
/// 不涉及相对路径的场景复用，避免两处规则漂移。
pub fn is_safe_filename(name: &str) -> bool {
    if name.is_empty() || name.trim() != name {
        return false;
    }
    // 含路径分隔符就交给 safe_join 处理，这里直接拒绝
    if name.contains('/') || name.contains('\\') {
        return false;
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_lowercase();
    !RESERVED_NAMES.contains(&stem.as_str())
}

/// 计算字节的 SHA-256（小写十六进制）。
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// 校验下载内容与期望哈希是否一致。
///
/// 只做**等长比较 + 逐字节**比较，避免不同长度提前返回造成的时间侧信道
/// （对本地安装场景影响有限，但保持一致的安全习惯）。
pub fn verify_sha256(bytes: &[u8], expected_hex: &str) -> bool {
    let expected = expected_hex.trim().to_ascii_lowercase();
    if expected.is_empty() {
        return false;
    }
    let actual = sha256_hex(bytes);
    if actual.len() != expected.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in actual.bytes().zip(expected.bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn accepts_normal_relative_path() {
        let dest = Path::new("C:\\app");
        let got = safe_join(dest, "module/bin/run.exe").unwrap();
        assert!(got.starts_with("C:\\app"));
        assert!(got.ends_with("run.exe"));
    }

    #[test]
    fn accepts_dot_prefixed_path() {
        let dest = Path::new("C:\\app");
        assert!(safe_join(dest, "./mod/a.txt").is_ok());
    }

    #[test]
    fn rejects_absolute_posix() {
        let dest = Path::new("C:\\app");
        assert_eq!(
            safe_join(dest, "/etc/passwd"),
            Err(PathError::Absolute("/etc/passwd".into()))
        );
    }

    #[test]
    fn rejects_windows_absolute() {
        let dest = Path::new("C:\\app");
        assert!(matches!(
            safe_join(dest, "C:\\Windows\\system32\\x.dll"),
            Err(PathError::Absolute(_))
        ));
    }

    #[test]
    fn rejects_unc_path() {
        let dest = Path::new("C:\\app");
        assert!(matches!(
            safe_join(dest, "\\\\server\\share\\evil"),
            Err(PathError::Absolute(_))
        ));
    }

    #[test]
    fn rejects_parent_traversal() {
        let dest = Path::new("C:\\app");
        assert!(matches!(
            safe_join(dest, "../outside.txt"),
            Err(PathError::Traversal(_))
        ));
        assert!(matches!(
            safe_join(dest, "mod/../../outside.txt"),
            Err(PathError::Traversal(_))
        ));
    }

    #[test]
    fn rejects_nested_traversal_disguised() {
        // 中间夹一个正常目录，仍然必须被拒绝
        let dest = Path::new("C:\\app");
        assert!(matches!(
            safe_join(dest, "mod/bin/../../../etc/passwd"),
            Err(PathError::Traversal(_))
        ));
    }

    #[test]
    fn rejects_reserved_device_names() {
        let dest = Path::new("C:\\app");
        for name in ["con.txt", "NUL", "aux/evil.txt", "COM1.log"] {
            let r = safe_join(dest, name);
            assert!(
                matches!(r, Err(PathError::ReservedName(_))),
                "应拒绝保留名 {name}，实际 {:?}",
                r
            );
        }
    }

    #[test]
    fn rejects_empty_entry() {
        let dest = Path::new("C:\\app");
        assert!(matches!(safe_join(dest, ""), Err(PathError::Empty)));
        assert!(matches!(safe_join(dest, "   "), Err(PathError::Empty)));
    }

    #[test]
    fn sha256_matches_known_vector() {
        // "abc" 的标准 SHA-256
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn empty_input_hash_is_correct() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn verify_accepts_matching_hash_case_insensitively() {
        let h = sha256_hex(b"payload");
        assert!(verify_sha256(b"payload", &h));
        assert!(verify_sha256(b"payload", &h.to_uppercase()));
    }

    #[test]
    fn verify_rejects_mismatch() {
        assert!(!verify_sha256(b"payload", &sha256_hex(b"other")));
    }

    #[test]
    fn verify_rejects_empty_expectation() {
        assert!(!verify_sha256(b"payload", ""));
    }

    #[test]
    fn safe_filename_accepts_normal_names() {
        for name in ["mod-a", "mod_b", "abc123", "a.b", "dounai-input"] {
            assert!(is_safe_filename(name), "应接受 {name}");
        }
    }

    #[test]
    fn safe_filename_rejects_reserved_and_separators() {
        for name in ["con", "NUL", "aux", "com1", "lpt9", "CON.txt"] {
            assert!(!is_safe_filename(name), "应拒绝保留名 {name}");
        }
        for name in ["", " a", "a ", "a/b", "a\\b"] {
            assert!(!is_safe_filename(name), "应拒绝 {name:?}");
        }
    }
}
