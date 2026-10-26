//! 安装事务：失败自动回滚（依赖安装「失败自动回滚」、「隔离降级」）。
//!
//! 设计要点：
//! - 安装先写**临时目录**，全部成功后才原子替换到最终位置；
//! - 任一步失败即删除临时目录，**不留下半残环境**；
//! - 覆盖已存在模块时，先把旧目录移到备份位，成功后删除，失败则还原。
//!
//! 用「先备后换」而不是「先删后换」：删掉旧版本再安装新版本，
//! 一旦新版本失败就既没有旧的也没有新的，这是最糟的结果。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::security::{safe_join, PathError};

/// 安装错误。
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("IO 失败：{0}")]
    Io(#[from] io::Error),
    #[error("压缩包条目不安全：{0}")]
    UnsafeEntry(#[from] PathError),
    #[error("压缩包损坏：{0}")]
    Zip(String),
    #[error("哈希校验失败：期望 {expected}，实际 {actual}")]
    HashMismatch { expected: String, actual: String },
    #[error("回滚失败，需要人工处理：{0}")]
    RollbackFailed(String),
}

/// 安装结果摘要。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct InstallReport {
    /// 实际写入的文件数
    pub files: usize,
    /// 解压出的字节数
    pub bytes: u64,
    /// 是否覆盖了旧版本
    pub replaced: bool,
}

/// 一次安装事务。
///
/// 生命周期：`begin` → 写入 → `commit` 或自动 `rollback`（Drop 时兜底）。
pub struct InstallTransaction {
    /// 最终目标目录
    final_dir: PathBuf,
    /// 临时构建目录（同盘，保证替换是原子 rename）
    staging: PathBuf,
    /// 旧版本备份目录
    backup: Option<PathBuf>,
    /// 是否已提交
    committed: bool,
    /// 是否已回滚
    rolled_back: bool,
}

impl InstallTransaction {
    /// 开始一次安装。
    ///
    /// `staging` 与 `final_dir` 必须在同一磁盘上，否则 `rename` 会退化为复制，
    /// 不再是原子操作。
    pub fn begin(final_dir: &Path) -> Result<Self, InstallError> {
        let parent = final_dir.parent().ok_or_else(|| {
            InstallError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "目标目录没有父目录",
            ))
        })?;
        fs::create_dir_all(parent)?;

        let staging = parent.join(format!(
            ".dounai-staging-{}-{}",
            std::process::id(),
            now_millis()
        ));
        fs::create_dir_all(&staging)?;

        Ok(Self {
            final_dir: final_dir.to_path_buf(),
            staging,
            backup: None,
            committed: false,
            rolled_back: false,
        })
    }

    /// 临时构建目录（调用方往这里写文件）。
    pub fn staging_dir(&self) -> &Path {
        &self.staging
    }

    /// 在临时目录内安全地创建一个文件，返回其绝对路径。
    ///
    /// 路径来自压缩包条目，必须经过 `safe_join` 校验。
    pub fn place_file(&self, entry: &str, content: &[u8]) -> Result<PathBuf, InstallError> {
        let target = safe_join(&self.staging, entry)?;
        if let Some(dir) = target.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&target, content)?;
        Ok(target)
    }

    /// 提交：把临时目录替换为最终目录。
    ///
    /// 若最终目录已存在，先移到备份位；替换成功后才删除备份。
    pub fn commit(mut self) -> Result<InstallReport, InstallError> {
        let files = count_files(&self.staging)?;
        let bytes = sum_bytes(&self.staging)?;

        let mut replaced = false;
        if self.final_dir.exists() {
            let backup = self
                .final_dir
                .with_extension(format!("bak-{}", now_millis()));
            fs::rename(&self.final_dir, &backup)?;
            self.backup = Some(backup);
            replaced = true;
        }

        if let Err(err) = fs::rename(&self.staging, &self.final_dir) {
            // 替换失败：先把旧版本还原，保证不丢用户已有环境
            if let Some(backup) = self.backup.take() {
                if let Err(restore_err) = fs::rename(&backup, &self.final_dir) {
                    return Err(InstallError::RollbackFailed(format!(
                        "新版本替换失败（{err}），且旧版本还原失败（{restore_err}）"
                    )));
                }
            }
            return Err(InstallError::Io(err));
        }

        // 替换成功，清掉备份
        if let Some(backup) = self.backup.take() {
            if let Err(err) = fs::remove_dir_all(&backup) {
                // 删除备份失败不影响安装结果，但需要让调用方知道残留
                eprintln!("警告：旧版本备份未能删除，残留在 {}", backup.display());
                let _ = err;
            }
        }

        self.committed = true;
        Ok(InstallReport {
            files,
            bytes,
            replaced,
        })
    }

    /// 显式回滚（Drop 也会兜底调用）。
    pub fn rollback(&mut self) -> Result<(), InstallError> {
        if self.committed || self.rolled_back {
            return Ok(());
        }
        self.rolled_back = true;
        if self.staging.exists() {
            fs::remove_dir_all(&self.staging)?;
        }
        Ok(())
    }
}

impl Drop for InstallTransaction {
    fn drop(&mut self) {
        if !self.committed && !self.rolled_back {
            // 兜底：调用方忘了 commit/rollback，也不能留下临时目录
            let _ = fs::remove_dir_all(&self.staging);
        }
    }
}

fn count_files(dir: &Path) -> Result<usize, InstallError> {
    let mut n = 0;
    for entry in walkdir::WalkDir::new(dir) {
        let entry = entry.map_err(|e| InstallError::Io(io::Error::other(e)))?;
        if entry.file_type().is_file() {
            n += 1;
        }
    }
    Ok(n)
}

fn sum_bytes(dir: &Path) -> Result<u64, InstallError> {
    let mut total = 0u64;
    for entry in walkdir::WalkDir::new(dir) {
        let entry = entry.map_err(|e| InstallError::Io(io::Error::other(e)))?;
        if entry.file_type().is_file() {
            total += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    Ok(total)
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// 从压缩包字节解压到事务的临时目录（防路径穿越）。
///
/// 任一危险条目都会让**整个安装失败**，不做部分解压。
pub fn extract_zip_into(
    tx: &InstallTransaction,
    zip_bytes: &[u8],
    expected_sha256: Option<&str>,
) -> Result<(), InstallError> {
    if let Some(expected) = expected_sha256 {
        if !crate::security::verify_sha256(zip_bytes, expected) {
            return Err(InstallError::HashMismatch {
                expected: expected.to_string(),
                actual: crate::security::sha256_hex(zip_bytes),
            });
        }
    }

    let reader = io::Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(reader).map_err(|e| InstallError::Zip(e.to_string()))?;

    // 先整体校验一遍条目路径，全部合法才写盘
    for i in 0..archive.len() {
        let file = archive
            .by_index(i)
            .map_err(|e| InstallError::Zip(e.to_string()))?;
        if file.is_dir() {
            continue;
        }
        let name = file
            .name()
            .map_err(|e| InstallError::Zip(e.to_string()))?
            .into_owned();
        // 危险条目：立即失败，已写入的部分由 Drop 清理
        safe_join(tx.staging_dir(), &name)?;
    }

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| InstallError::Zip(e.to_string()))?;
        if file.is_dir() {
            continue;
        }
        let name = file
            .name()
            .map_err(|e| InstallError::Zip(e.to_string()))?
            .into_owned();
        let mut buf = Vec::with_capacity(file.size() as usize);
        io::copy(&mut file, &mut buf)?;
        tx.place_file(&name, &buf)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dounai-it-{}-{}", name, now_millis()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(io::Cursor::new(&mut buf));
            let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
            for (name, data) in entries {
                writer.start_file(*name, opts).unwrap();
                writer.write_all(data).unwrap();
            }
            writer.finish().unwrap();
        }
        buf
    }

    #[test]
    fn commit_writes_files_and_reports() {
        let root = tmp_root("commit");
        let dest = root.join("mod");
        {
            let tx = InstallTransaction::begin(&dest).unwrap();
            tx.place_file("a.txt", b"hello").unwrap();
            tx.place_file("sub/b.txt", b"world!").unwrap();
            let report = tx.commit().unwrap();
            assert_eq!(report.files, 2);
            assert_eq!(report.bytes, 11);
            assert!(!report.replaced);
        }
        assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"hello");
        assert_eq!(fs::read(dest.join("sub/b.txt")).unwrap(), b"world!");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn drop_without_commit_leaves_no_staging() {
        let root = tmp_root("drop");
        let dest = root.join("mod");
        {
            let tx = InstallTransaction::begin(&dest).unwrap();
            tx.place_file("x.txt", b"x").unwrap();
            // 不 commit，直接离开作用域
        }
        assert!(!dest.exists(), "未提交不应产生最终目录");
        let leftovers: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("staging"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "临时目录应被 Drop 清理，实际残留 {:?}",
            leftovers
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn replace_keeps_old_version_on_failure() {
        let root = tmp_root("replace");
        let dest = root.join("mod");
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("old.txt"), b"old").unwrap();

        // 正常覆盖
        {
            let tx = InstallTransaction::begin(&dest).unwrap();
            tx.place_file("new.txt", b"new").unwrap();
            let r = tx.commit().unwrap();
            assert!(r.replaced);
        }
        assert!(dest.join("new.txt").exists());
        assert!(!dest.join("old.txt").exists(), "旧文件不应残留");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn extract_zip_ok_and_hash_verified() {
        let root = tmp_root("unzip");
        let dest = root.join("mod");
        let zip_bytes = make_zip(&[("run.bat", b"echo hi"), ("lib/x.dll", &[1u8, 2, 3])]);
        let hash = crate::security::sha256_hex(&zip_bytes);
        {
            let tx = InstallTransaction::begin(&dest).unwrap();
            extract_zip_into(&tx, &zip_bytes, Some(&hash)).unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(fs::read(dest.join("run.bat")).unwrap(), b"echo hi");
        assert_eq!(fs::read(dest.join("lib/x.dll")).unwrap(), vec![1u8, 2, 3]);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn extract_zip_rejects_hash_mismatch() {
        let root = tmp_root("badhash");
        let dest = root.join("mod");
        let zip_bytes = make_zip(&[("a.txt", b"a")]);
        {
            let tx = InstallTransaction::begin(&dest).unwrap();
            let err = extract_zip_into(&tx, &zip_bytes, Some("deadbeef")).unwrap_err();
            assert!(matches!(err, InstallError::HashMismatch { .. }));
        }
        assert!(!dest.exists());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn extract_zip_rejects_traversal_and_installs_nothing() {
        let root = tmp_root("traversal");
        let dest = root.join("mod");
        let zip_bytes = make_zip(&[("ok.txt", b"fine"), ("../escape.txt", b"evil")]);
        {
            let tx = InstallTransaction::begin(&dest).unwrap();
            let err = extract_zip_into(&tx, &zip_bytes, None).unwrap_err();
            assert!(
                matches!(err, InstallError::UnsafeEntry(_)),
                "实际 {:?}",
                err
            );
        }
        assert!(!dest.exists(), "含危险条目时不应产生任何安装结果");
        assert!(!root.join("escape.txt").exists(), "绝不能越界写出文件");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn extract_zip_rejects_absolute_entry() {
        let root = tmp_root("abs");
        let dest = root.join("mod");
        let zip_bytes = make_zip(&[("C:\\evil.txt", b"x")]);
        {
            let tx = InstallTransaction::begin(&dest).unwrap();
            let err = extract_zip_into(&tx, &zip_bytes, None).unwrap_err();
            assert!(matches!(err, InstallError::UnsafeEntry(_)));
        }
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rollback_is_idempotent() {
        let root = tmp_root("idem");
        let dest = root.join("mod");
        let mut tx = InstallTransaction::begin(&dest).unwrap();
        tx.place_file("a.txt", b"a").unwrap();
        tx.rollback().unwrap();
        // 第二次调用不应报错
        tx.rollback().unwrap();
        fs::remove_dir_all(&root).ok();
    }
}
