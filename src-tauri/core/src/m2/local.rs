//! 本地 zip 与目录/Git 导入。
//!
//! 目录实例不复制源码，固定 C 类。只读观察 .git；没有 .git 时用源码快照哈希
//! 作为 head，HEAD 或哈希变化都要求重新确认并禁止自动启动。
//! 本模块不执行 git 命令，也不发起任何 git 网络操作。

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::m2::installed::GitObservation;
use crate::m2::source::{PackageReceipt, SourceKind};
use crate::m2::M2Error;
use crate::security::sha256_hex;

const SKIP_DIRS: &[&str] = &[
    ".git",
    "venv",
    ".venv",
    "__pycache__",
    "node_modules",
    "target",
    ".staging",
    "dist",
    "build",
];
const MAX_FILES: usize = 2_000;
const MAX_HASHED_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceObservation {
    pub git_present: bool,
    /// 有 .git 时是 commit；没有 .git 时是 `snapshot:<源码哈希>`。
    pub head: Option<String>,
    /// 只展示，不拦截。本实现不执行 git，变更检测靠 head 与 head_confirmed 比对。
    pub dirty: bool,
    pub observed_at: String,
    pub fingerprint: String,
}

impl SourceObservation {
    pub fn to_git_observation(&self) -> GitObservation {
        GitObservation {
            present: self.git_present,
            origin: None,
            branch: None,
            head: self.head.clone(),
            dirty: self.dirty,
            observed_at: self.observed_at.clone(),
        }
    }

    /// 统一比对入口：有 .git 比 HEAD，没有 .git 比源码哈希。
    pub fn confirmation_anchor(&self) -> &str {
        self.head.as_deref().unwrap_or("")
    }
}

/// 只读观察源码目录。不读取 .git/config，避免把可能含凭据的 origin 落盘或写进日志。
pub fn observe_source(dir: &Path, now: &str) -> Result<SourceObservation, M2Error> {
    let fingerprint = format!("sha256:{}", directory_fingerprint(dir)?);
    let head_path = dir.join(".git").join("HEAD");
    if head_path.is_file() {
        let text = fs::read_to_string(&head_path)
            .map_err(|e| M2Error::State(format!("读取 .git/HEAD 失败：{e}")))?;
        let trimmed = text.trim();
        let head = if let Some(reference) = trimmed.strip_prefix("ref: ") {
            resolve_ref(dir, reference)
        } else if trimmed.len() == 40 && trimmed.bytes().all(|b| b.is_ascii_hexdigit()) {
            Some(trimmed.to_ascii_lowercase())
        } else {
            None
        };
        Ok(SourceObservation {
            git_present: true,
            head,
            dirty: false,
            observed_at: now.into(),
            fingerprint,
        })
    } else {
        Ok(SourceObservation {
            git_present: false,
            head: Some(format!("snapshot:{fingerprint}")),
            dirty: false,
            observed_at: now.into(),
            fingerprint,
        })
    }
}

fn resolve_ref(dir: &Path, reference: &str) -> Option<String> {
    let loose = dir.join(".git").join(reference);
    if let Ok(text) = fs::read_to_string(&loose) {
        let value = text.trim();
        if value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Some(value.to_ascii_lowercase());
        }
    }
    let packed = fs::read_to_string(dir.join(".git").join("packed-refs")).ok()?;
    for line in packed.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with('^') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let hash = fields.next()?;
        if fields.next() == Some(reference) && hash.len() == 40 {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

/// 目录级聚合哈希：路径与内容一起进哈希，顺序固定，跳过构建产物与符号链接。
fn directory_fingerprint(root: &Path) -> Result<String, M2Error> {
    let mut hasher = Sha256::new();
    let mut stack = vec![root.to_path_buf()];
    let mut files = 0usize;
    let mut bytes_total = 0u64;
    while let Some(current) = stack.pop() {
        let mut paths: Vec<PathBuf> = fs::read_dir(&current)
            .map_err(|e| M2Error::State(format!("读取目录 {} 失败：{e}", current.display())))?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .collect();
        paths.sort();
        for path in paths {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            let metadata = fs::symlink_metadata(&path)
                .map_err(|e| M2Error::State(format!("读取文件元数据失败：{e}")))?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                stack.push(path);
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            hasher.update(relative.as_bytes());
            let bytes = fs::read(&path)
                .map_err(|e| M2Error::State(format!("读取 {} 失败：{e}", path.display())))?;
            files += 1;
            bytes_total += bytes.len() as u64;
            if files > MAX_FILES || bytes_total > MAX_HASHED_BYTES {
                return Err(M2Error::Invalid("源码快照超出预算，无法生成确认锚".into()));
            }
            hasher.update((bytes.len() as u64).to_le_bytes());
            hasher.update(&bytes);
        }
    }
    Ok(hex::encode(hasher.finalize()))
}

/// 启动前比对。变化即要求重新确认并禁止自动启动。
pub fn require_confirmed(
    head_confirmed: Option<&str>,
    observed: &SourceObservation,
) -> Result<(), M2Error> {
    match head_confirmed {
        Some(previous) if previous == observed.confirmation_anchor() => Ok(()),
        Some(previous) => Err(M2Error::Rejected(format!(
            "源码 HEAD 或哈希已变化（已确认 {previous}，当前 {}），需重新确认并禁止自动启动",
            observed.confirmation_anchor()
        ))),
        None => Err(M2Error::Rejected(
            "目录实例尚未确认源码状态，禁止自动启动".into(),
        )),
    }
}

/// LocalDir 拒绝启动器自身目录及其祖先，也拒绝指向数据根的符号链接。
pub fn validate_local_dir(
    dir: &Path,
    install_root: &Path,
    data_root: &Path,
    exe_dir: &Path,
) -> Result<(), M2Error> {
    let canonical = dir
        .canonicalize()
        .map_err(|e| M2Error::Rejected(format!("本地目录不可读：{e}")))?;
    for root in [install_root, data_root, exe_dir] {
        let resolved = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        if canonical == resolved || resolved.starts_with(&canonical) {
            return Err(M2Error::Rejected(format!(
                "本地目录不能是启动器目录本身或其祖先：{}",
                canonical.display()
            )));
        }
    }
    let data_resolved = data_root
        .canonicalize()
        .unwrap_or_else(|_| data_root.to_path_buf());
    let is_symlink = fs::symlink_metadata(dir)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    if is_symlink && canonical.starts_with(&data_resolved) {
        return Err(M2Error::Rejected(
            "本地目录不能是指向启动器数据根的符号链接".into(),
        ));
    }
    Ok(())
}

/// 本地 zip 的哈希由启动器自算，只防传输损坏；无新鲜度锚、不自动更新。
pub fn local_zip_receipt(path: &Path) -> Result<PackageReceipt, M2Error> {
    let bytes = fs::read(path).map_err(|e| M2Error::Package(format!("读取本地 zip 失败：{e}")))?;
    Ok(PackageReceipt {
        source_kind: SourceKind::LocalZip,
        source_path: Some(path.to_path_buf()),
        sha256: format!("sha256:{}", sha256_hex(&bytes)),
        size: bytes.len() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_git_uses_snapshot_hash_and_detects_changes() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.py"), b"print(1)").unwrap();
        let first = observe_source(dir.path(), "2026-01-01T00:00:00Z").unwrap();
        assert!(!first.git_present);
        assert!(first
            .head
            .as_deref()
            .unwrap()
            .starts_with("snapshot:sha256:"));
        assert!(require_confirmed(Some(first.confirmation_anchor()), &first).is_ok());
        fs::write(dir.path().join("a.py"), b"print(2)").unwrap();
        let second = observe_source(dir.path(), "2026-01-02T00:00:00Z").unwrap();
        assert_ne!(first.head, second.head);
        assert!(require_confirmed(Some(first.confirmation_anchor()), &second).is_err());
        assert!(require_confirmed(None, &second).is_err());
    }

    #[test]
    fn git_head_is_read_only_without_network() {
        let dir = tempfile::tempdir().unwrap();
        let git = dir.path().join(".git");
        fs::create_dir_all(git.join("refs").join("heads")).unwrap();
        fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(
            git.join("refs").join("heads").join("main"),
            format!("{}\n", "a".repeat(40)),
        )
        .unwrap();
        let observed = observe_source(dir.path(), "2026-01-01T00:00:00Z").unwrap();
        assert!(observed.git_present);
        assert_eq!(observed.head.as_deref(), Some("a".repeat(40).as_str()));
        assert_eq!(
            observed.to_git_observation().head.as_deref(),
            Some("a".repeat(40).as_str())
        );
    }

    #[test]
    fn rejects_launcher_roots_and_ancestors() {
        let root = tempfile::tempdir().unwrap();
        let install = root.path().join("app");
        let data = root.path().join("data");
        let exe = root.path().join("app");
        fs::create_dir_all(&install).unwrap();
        fs::create_dir_all(&data).unwrap();
        assert!(validate_local_dir(root.path(), &install, &data, &exe).is_err());
        assert!(validate_local_dir(&data, &install, &data, &exe).is_err());
        let source = root.path().join("source");
        fs::create_dir_all(&source).unwrap();
        assert!(validate_local_dir(&source, &install, &data, &exe).is_ok());
    }

    #[test]
    fn local_zip_receipt_hashes_bytes_itself() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("demo.zip");
        fs::write(&zip, b"PK").unwrap();
        let receipt = local_zip_receipt(&zip).unwrap();
        assert_eq!(receipt.size, 2);
        assert_eq!(receipt.source_kind, SourceKind::LocalZip);
        assert!(receipt.sha256.starts_with("sha256:"));
    }
}
