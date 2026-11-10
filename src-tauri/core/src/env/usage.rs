//! M4 首次向导要显示首次下载量。M1 只量出数字，不写界面文案。

use std::fs;
use std::path::{Path, PathBuf};

use crate::paths::Paths;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheUsage {
    pub cache_dir: PathBuf,
    pub bytes: u64,
    pub files: u64,
}

/// 只统计数据根里的 uv 缓存，不碰程序根，也不联网。
pub fn cache_usage(paths: &Paths) -> std::io::Result<CacheUsage> {
    let dir = paths.uv_cache_dir();
    let mut usage = CacheUsage {
        cache_dir: dir.clone(),
        bytes: 0,
        files: 0,
    };
    if dir.exists() {
        walk(&dir, &mut usage)?;
    }
    Ok(usage)
}

fn walk(dir: &Path, usage: &mut CacheUsage) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            walk(&path, usage)?;
        } else {
            usage.bytes += entry.metadata()?.len();
            usage.files += 1;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::{InstallMode, Paths};

    #[test]
    fn reports_cache_bytes_and_file_count() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(
            InstallMode::CurrentUser,
            dir.path().join("app"),
            dir.path().join("data"),
        )
        .unwrap();
        assert_eq!(cache_usage(&paths).unwrap().bytes, 0);
        let cache = paths.uv_cache_dir();
        fs::create_dir_all(cache.join("wheels-v5")).unwrap();
        fs::write(cache.join("wheels-v5").join("a.whl"), b"12345").unwrap();
        fs::write(cache.join("b.bin"), b"abc").unwrap();
        let usage = cache_usage(&paths).unwrap();
        assert_eq!(usage.bytes, 8);
        assert_eq!(usage.files, 2);
        assert_eq!(usage.cache_dir, cache);
    }
}
