//! 环境侧的空间回收。
//!
//! 三类可回收物：旧 generation、运行生成的 `__pycache__`、安装后留下的下载包。
//! 都不碰 `data/` 用户数据与 dir 实例的源码目录，也不改写 `runtime.json`。

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use crate::paths::{generation_sha, Paths};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcReport {
    pub removed: Vec<String>,
    pub errors: Vec<String>,
}

impl GcReport {
    pub fn is_empty(&self) -> bool {
        self.removed.is_empty() && self.errors.is_empty()
    }

    pub fn extend(&mut self, other: GcReport) {
        self.removed.extend(other.removed);
        self.errors.extend(other.errors);
    }
}

fn remove_dir(path: &Path, report: &mut GcReport) {
    match fs::remove_dir_all(path) {
        Ok(()) => report.removed.push(path.to_string_lossy().into_owned()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => report
            .errors
            .push(format!("清理 {} 失败：{e}", path.display())),
    }
}

fn remove_file(path: &Path, report: &mut GcReport) {
    match fs::remove_file(path) {
        Ok(()) => report.removed.push(path.to_string_lossy().into_owned()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => report
            .errors
            .push(format!("清理 {} 失败：{e}", path.display())),
    }
}

fn is_short_tag(name: &str) -> bool {
    name.len() == 16
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// 清掉目录树里所有 `__pycache__`。字节码可以随时重建，删除不需要停进程。
pub fn remove_bytecode_under(root: &Path) -> GcReport {
    let mut report = GcReport::default();
    if !root.is_dir() {
        return report;
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if !kind.is_dir() {
                continue;
            }
            if entry.file_name() == OsStr::new("__pycache__") {
                remove_dir(&entry.path(), &mut report);
            } else {
                stack.push(entry.path());
            }
        }
    }
    report
}

pub fn remove_instance_bytecode(paths: &Paths, instance_id: &str) -> GcReport {
    remove_bytecode_under(&paths.instance_venvs_dir(instance_id))
}

pub fn remove_all_bytecode(paths: &Paths) -> GcReport {
    remove_bytecode_under(&paths.data_root.join("instances"))
}

/// 清掉下载目录里的包文件。`data_root/downloads` 本身保留。
pub fn remove_downloads(paths: &Paths) -> GcReport {
    let mut report = GcReport::default();
    let dir = paths.data_root.join("downloads");
    let Ok(entries) = fs::read_dir(&dir) else {
        return report;
    };
    for entry in entries.flatten() {
        if entry.path().is_file() {
            remove_file(&entry.path(), &mut report);
        }
    }
    report
}

/// 回收不再被引用的 generation：venv 目录与按哈希命名的锁、元数据。
///
/// `keep` 传 `runtime.json` 当前指向的哈希和 `snapshot_change.previous_sha256`。
/// 再旧的 generation 既不是当前环境，也不在可回退范围内，可以安全删除。
/// 只认严格的 16/64 位小写十六进制名字，绝不误删 staging 或其他目录。
pub fn collect_generations(paths: &Paths, instance_id: &str, keep: &[String]) -> GcReport {
    let mut report = GcReport::default();
    let keep_tags: BTreeSet<&str> = keep
        .iter()
        .filter_map(|hash| {
            let tag = generation_tag(hash);
            (tag != "invalid").then_some(tag)
        })
        .collect();
    let keep_hashes: BTreeSet<&str> = keep
        .iter()
        .map(|hash| generation_sha(hash))
        .filter(|hex| *hex != "invalid")
        .collect();

    let venvs = paths.instance_venvs_dir(instance_id);
    if let Ok(entries) = fs::read_dir(&venvs) {
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if !kind.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if is_short_tag(&name) && !keep_tags.contains(name.as_str()) {
                remove_dir(&entry.path(), &mut report);
            }
        }
    }

    let deps = paths.deps_generation_dir(instance_id);
    if let Ok(entries) = fs::read_dir(&deps) {
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if !kind.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(hex) = name
                .strip_suffix(".lock")
                .or_else(|| name.strip_suffix(".json"))
            else {
                continue;
            };
            if generation_sha(hex) != hex || keep_hashes.contains(hex) {
                continue;
            }
            remove_file(&entry.path(), &mut report);
        }
    }
    report
}

fn generation_tag(hash: &str) -> &str {
    let hex = generation_sha(hash);
    if hex.len() == 64 {
        &hex[..16]
    } else {
        "invalid"
    }
}

/// 一次完整准备后的回收：旧 generation + 当前 venv 里遗留的字节码。
pub fn after_prepare(paths: &Paths, instance_id: &str, keep: &[String]) -> GcReport {
    let mut report = collect_generations(paths, instance_id, keep);
    report.extend(remove_instance_bytecode(paths, instance_id));
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::InstallMode;
    use std::path::PathBuf;

    fn paths(dir: &Path) -> Paths {
        Paths::new(InstallMode::CurrentUser, dir.join("app"), dir.join("data")).unwrap()
    }

    fn sha(c: char) -> String {
        format!("sha256:{}", c.to_string().repeat(64))
    }

    #[test]
    fn keeps_current_and_previous_generation_only() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let id = "mod@a";
        let old = sha('1');
        let previous = sha('2');
        let current = sha('3');
        fs::create_dir_all(p.deps_generation_dir(id)).unwrap();
        for hash in [&old, &previous, &current] {
            let venv = p.generation_venv_dir(id, hash);
            fs::create_dir_all(venv.join("Scripts")).unwrap();
            fs::write(p.generation_venv_python(id, hash), b"python").unwrap();
            fs::write(p.content_lock_path(id, hash), b"lock").unwrap();
            fs::write(p.content_meta_path(id, hash), b"{}").unwrap();
        }
        fs::create_dir_all(p.instance_venvs_dir(id).join("pending.staging")).unwrap();

        let report = collect_generations(&p, id, &[current.clone(), previous.clone()]);
        assert!(report.errors.is_empty());
        assert!(!p.generation_venv_dir(id, &old).exists());
        assert!(p.generation_venv_dir(id, &previous).exists());
        assert!(p.generation_venv_dir(id, &current).exists());
        assert!(!p.content_lock_path(id, &old).exists());
        assert!(!p.content_meta_path(id, &old).exists());
        assert!(p.content_lock_path(id, &previous).exists());
        assert!(p.instance_venvs_dir(id).join("pending.staging").exists());
        assert_eq!(report.removed.len(), 3);
    }

    #[test]
    fn bytecode_and_downloads_are_reclaimable() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let cache = p
            .instance_venvs_dir("mod@a")
            .join("aaaaaaaaaaaaaaaa")
            .join("Lib")
            .join("site-packages")
            .join("__pycache__");
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join("x.pyc"), b"bytecode").unwrap();
        let downloads = p.data_root.join("downloads");
        fs::create_dir_all(&downloads).unwrap();
        fs::write(downloads.join("mod.zip"), b"zip").unwrap();
        let data = p.instance_data_dir_keyed("mod@a");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("keep.txt"), b"user data").unwrap();

        let report = remove_all_bytecode(&p);
        assert!(report.errors.is_empty());
        assert!(!cache.exists());
        assert_eq!(report.removed.len(), 1);

        let report = remove_downloads(&p);
        assert!(report.errors.is_empty());
        assert!(!downloads.join("mod.zip").exists());
        assert!(data.join("keep.txt").exists());
    }

    #[test]
    fn malformed_generation_names_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let venvs = p.instance_venvs_dir("mod@a");
        fs::create_dir_all(venvs.join("notes")).unwrap();
        fs::create_dir_all(venvs.join("0123456789abcdef0")).unwrap();
        let deps = p.deps_generation_dir("mod@a");
        fs::create_dir_all(&deps).unwrap();
        fs::write(deps.join("readme.txt"), b"keep").unwrap();

        let report = collect_generations(&p, "mod@a", &[sha('1')]);
        assert!(report.removed.is_empty());
        assert!(venvs.join("notes").exists());
        assert!(venvs.join("0123456789abcdef0").exists());
        assert!(deps.join("readme.txt").exists());
    }

    #[test]
    fn gc_report_tracks_all_paths() {
        let mut report = GcReport::default();
        assert!(report.is_empty());
        report
            .removed
            .push(PathBuf::from("a").to_string_lossy().into_owned());
        assert!(!report.is_empty());
    }
}
