//! zip 包完整性与原子安装。
//!
//! 解压前按 zip 元数据核对总大小；逐文件哈希必须与 manifest.files 完全一致，
//! 多一个、少一个、改一个字节都整体拒绝且不落盘。全部通过后 staging 一次 rename，
//! 覆盖旧版先备份，失败自动还原。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use zip::ZipArchive;

use crate::m2::manifest::PackageManifest;
use crate::m2::{is_hash, M2Error, MAX_UNPACKED_BYTES};
use crate::security::{safe_join, sha256_hex};

static INSTALL_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct ZipExpectation<'a> {
    /// catalog 声明的整包哈希；本地导入可留空由启动器自算。
    pub sha256: Option<&'a str>,
    pub size: Option<u64>,
    pub max_unpacked: u64,
}

impl<'a> Default for ZipExpectation<'a> {
    fn default() -> Self {
        Self {
            sha256: None,
            size: None,
            max_unpacked: MAX_UNPACKED_BYTES,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PackageFile {
    pub path: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct VerifiedPackage {
    pub manifest: PackageManifest,
    pub sha256: String,
    pub size: u64,
    pub files: Vec<PackageFile>,
}

pub fn verify_zip(
    bytes: &[u8],
    expectation: &ZipExpectation<'_>,
) -> Result<VerifiedPackage, M2Error> {
    let bad = |reason: String| M2Error::Package(reason);
    let actual_sha = format!("sha256:{}", sha256_hex(bytes));
    if let Some(expected) = expectation.sha256 {
        if !is_hash(expected) {
            return Err(bad(format!("期望哈希格式非法：{expected}")));
        }
        if actual_sha != expected {
            return Err(bad(format!(
                "整包哈希不符：期望 {expected}，实际 {actual_sha}"
            )));
        }
    }
    if let Some(expected_size) = expectation.size {
        if bytes.len() as u64 != expected_size {
            return Err(bad(format!(
                "包大小不符：期望 {expected_size}，实际 {}",
                bytes.len()
            )));
        }
    }

    let mut archive =
        ZipArchive::new(Cursor::new(bytes)).map_err(|e| bad(format!("zip 无法打开：{e}")))?;
    let mut total: u64 = 0;
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut manifest_bytes: Option<Vec<u8>> = None;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|e| bad(format!("zip 条目读取失败：{e}")))?;
        let name = entry
            .name()
            .map_err(|e| bad(format!("zip 条目名非法：{e}")))?
            .to_string();
        if name.contains('\\') {
            return Err(bad(format!("zip 条目路径含反斜杠：{name}")));
        }
        safe_join(Path::new("package"), &name)
            .map_err(|e| bad(format!("zip 条目路径非法 {name}：{e}")))?;
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170_000 == 0o120_000)
        {
            return Err(bad(format!("zip 条目是符号链接：{name}")));
        }
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| bad("解压总大小溢出".into()))?;
        if total > expectation.max_unpacked {
            return Err(bad(format!(
                "解压后总大小 {} 超过上限 {}，拒绝落盘",
                total, expectation.max_unpacked
            )));
        }
        let is_dir = name.ends_with('/');
        let mut buffer = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut buffer)
            .map_err(|e| bad(format!("读取 zip 条目 {name} 失败：{e}")))?;
        if is_dir {
            continue;
        }
        if name == "manifest.json" {
            manifest_bytes = Some(buffer);
        } else {
            files.insert(name, buffer);
        }
    }

    let raw = manifest_bytes.ok_or_else(|| bad("包内缺少 manifest.json".into()))?;
    let text =
        std::str::from_utf8(&raw).map_err(|e| bad(format!("manifest.json 不是 UTF-8：{e}")))?;
    let manifest = PackageManifest::parse(text)?;

    let declared: BTreeSet<&str> = manifest.files.keys().map(String::as_str).collect();
    let actual: BTreeSet<&str> = files.keys().map(String::as_str).collect();
    if declared != actual {
        let missing: Vec<&str> = declared.difference(&actual).copied().collect();
        let extra: Vec<&str> = actual.difference(&declared).copied().collect();
        return Err(bad(format!(
            "files 清单与 zip 不一致：缺少 {missing:?}，多出 {extra:?}"
        )));
    }
    for (path, expected) in &manifest.files {
        let got = format!("sha256:{}", sha256_hex(&files[path.as_str()]));
        if got != *expected {
            return Err(bad(format!("文件哈希不符：{path}")));
        }
    }

    Ok(VerifiedPackage {
        manifest,
        sha256: actual_sha,
        size: bytes.len() as u64,
        files: files
            .into_iter()
            .map(|(path, bytes)| PackageFile { path, bytes })
            .collect(),
    })
}

struct InstallGuard {
    staging: PathBuf,
    backup: PathBuf,
    final_dir: PathBuf,
    backup_taken: bool,
    committed: bool,
}

impl Drop for InstallGuard {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let _ = fs::remove_dir_all(&self.staging);
        if !self.backup_taken {
            return;
        }
        if !self.final_dir.exists() {
            let _ = fs::rename(&self.backup, &self.final_dir);
        } else {
            let _ = fs::remove_dir_all(&self.backup);
        }
    }
}

/// 全部校验通过后才提交。覆盖旧版先备份，rename 失败自动还原。
pub fn install_package(pkg: &VerifiedPackage, final_dir: &Path) -> Result<(), M2Error> {
    let bad = |reason: String| M2Error::Package(reason);
    let parent = final_dir
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|e| bad(format!("创建安装父目录失败：{e}")))?;
    let name = final_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("instance");
    let seq = INSTALL_SEQ.fetch_add(1, Ordering::Relaxed);
    let tag = format!("{name}-{}-{}", std::process::id(), seq);
    let staging = parent.join(format!(".{tag}.staging"));
    let backup = parent.join(format!(".{tag}.backup"));
    fs::create_dir_all(&staging).map_err(|e| bad(format!("创建 staging 失败：{e}")))?;
    let mut guard = InstallGuard {
        staging: staging.clone(),
        backup: backup.clone(),
        final_dir: final_dir.to_path_buf(),
        backup_taken: false,
        committed: false,
    };

    for file in &pkg.files {
        let target = safe_join(&staging, &file.path)
            .map_err(|e| bad(format!("落盘路径非法 {}：{e}", file.path)))?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| bad(format!("创建文件父目录失败：{e}")))?;
        }
        fs::write(&target, &file.bytes)
            .map_err(|e| bad(format!("写 staging 文件 {} 失败：{e}", file.path)))?;
    }

    // 启动前再核一次入口文件哈希，堵住验签与 spawn 之间的窗口。
    for file in &pkg.files {
        let expected = pkg
            .manifest
            .files
            .get(&file.path)
            .ok_or_else(|| bad(format!("入口文件缺少哈希声明：{}", file.path)))?;
        let got = format!("sha256:{}", sha256_hex(&file.bytes));
        if got != *expected {
            return Err(bad(format!("入口文件哈希被改动：{}", file.path)));
        }
    }

    if final_dir.exists() {
        fs::rename(final_dir, &backup).map_err(|e| bad(format!("备份旧版失败：{e}")))?;
        guard.backup_taken = true;
    }
    fs::rename(&staging, final_dir).map_err(|e| bad(format!("提交安装失败：{e}")))?;
    guard.committed = true;
    if guard.backup_taken {
        let _ = fs::remove_dir_all(&backup);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    fn package(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut declared = BTreeMap::new();
        for (path, bytes) in files {
            declared.insert((*path).to_string(), format!("sha256:{}", sha256_hex(bytes)));
        }
        let manifest = serde_json::json!({
            "schema_version": 2,
            "module_id": "dounai/demo",
            "name": "demo",
            "version": "1.0.0",
            "variant": "stable",
            "role": "llm",
            "category": "optional",
            "publisher": { "id": "demo", "name": "Demo" },
            "requires": { "launcher": ">=1.0.0" },
            "runtime": {
                "python": ">=3.11,<3.14",
                "venv": "{instance_venv}",
                "deps": { "kind": "pyproject", "path": "pyproject.toml" }
            },
            "entry": { "command": "{venv_python} -m demo", "env": {}, "health": null },
            "ports": [8101],
            "permissions": { "network": [], "filesystem": [], "devices": [] },
            "files": declared
        });
        let mut out = Vec::new();
        {
            let mut zip = ZipWriter::new(Cursor::new(&mut out));
            let options = SimpleFileOptions::default();
            zip.start_file("manifest.json", options).unwrap();
            zip.write_all(manifest.to_string().as_bytes()).unwrap();
            for (path, bytes) in files {
                zip.start_file(*path, options).unwrap();
                zip.write_all(bytes).unwrap();
            }
            zip.finish().unwrap();
        }
        out
    }

    #[test]
    fn matches_declared_files_and_installs_atomically() {
        let bytes = package(&[
            ("pyproject.toml", b"[project]\n"),
            ("demo.py", b"print(1)\n"),
        ]);
        let verified = verify_zip(&bytes, &ZipExpectation::default()).unwrap();
        assert_eq!(verified.files.len(), 2);
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("modules").join("demo");
        install_package(&verified, &target).unwrap();
        assert!(target.join("demo.py").exists());
        assert!(!dir
            .path()
            .join("modules")
            .join(".demo-1-0.staging")
            .exists());
    }

    #[test]
    fn one_changed_byte_is_rejected() {
        let mut bytes = package(&[("demo.py", b"print(1)\n")]);
        let last = bytes.len() - 30;
        bytes[last] ^= 1;
        assert!(verify_zip(&bytes, &ZipExpectation::default()).is_err());
    }

    #[test]
    fn unpack_size_limit_is_enforced_before_extraction() {
        let bytes = package(&[("demo.py", b"print(1)\n")]);
        let expectation = ZipExpectation {
            max_unpacked: 1,
            ..Default::default()
        };
        assert!(verify_zip(&bytes, &expectation).is_err());
    }
}
