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

/// 目录下所有文件的字节数；目录不存在算 0。
///
/// 读不到单个文件（权限、正在被删）时按 0 计，不让一处失败毁掉整个统计——
/// 这是报给用户看的数字，宁可略小也不要整块报错。
pub fn dir_usage(dir: &Path) -> std::io::Result<u64> {
    if !dir.exists() {
        return Ok(0);
    }
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = match fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.metadata() {
                Ok(meta) if meta.is_dir() => stack.push(path),
                Ok(meta) => total += meta.len(),
                Err(_) => {}
            }
        }
    }
    Ok(total)
}

/// 运行时占用：解释器本体与 uv 缓存。
///
/// 向导首屏要回答「首次还要下多少」，这两个数就是答案的主体：解释器已经装过
/// 就不用再下，缓存里的轮子在装第二个模块时也会被复用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeUsage {
    pub python_bytes: u64,
    pub cache_bytes: u64,
}

pub fn runtime_usage(paths: &Paths) -> std::io::Result<RuntimeUsage> {
    Ok(RuntimeUsage {
        python_bytes: dir_usage(&paths.python_dir())?,
        cache_bytes: dir_usage(&paths.uv_cache_dir())?,
    })
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

    #[test]
    fn runtime_usage_counts_python_and_cache() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(
            InstallMode::CurrentUser,
            dir.path().join("app"),
            dir.path().join("data"),
        )
        .unwrap();
        let empty = runtime_usage(&paths).unwrap();
        assert_eq!(empty.python_bytes, 0);
        assert_eq!(empty.cache_bytes, 0);

        fs::create_dir_all(paths.python_dir().join("cpython-3.12")).unwrap();
        fs::write(
            paths.python_dir().join("cpython-3.12").join("python.exe"),
            b"abcd",
        )
        .unwrap();
        fs::create_dir_all(paths.uv_cache_dir().join("wheels-v5")).unwrap();
        fs::write(
            paths.uv_cache_dir().join("wheels-v5").join("a.whl"),
            b"123456",
        )
        .unwrap();

        let usage = runtime_usage(&paths).unwrap();
        assert_eq!(usage.python_bytes, 4);
        assert_eq!(usage.cache_bytes, 6);
    }

    #[test]
    fn dir_usage_ignores_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(dir_usage(&dir.path().join("nope")).unwrap(), 0);
    }
}
