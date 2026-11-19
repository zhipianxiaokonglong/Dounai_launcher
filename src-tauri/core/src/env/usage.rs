//! M4 首次向导要显示首次下载量。M1 只量出数字，不写界面文案。

use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;

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

/// 同一物理文件在多个 venv 与 uv-cache 里出现时只计一次。
///
/// Windows 用「卷序列号 + 文件索引」，Unix 用 `dev + ino`；拿不到索引时
/// 退回路径哈希，至少不会因为重复路径把同一个逻辑文件算两遍。长度也进 key，
/// 避免扫描期间文件被替换时误合并。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct FileKey {
    dev: u64,
    ino: u64,
    len: u64,
}

impl FileKey {
    fn new(meta: &fs::Metadata, path: &Path) -> Self {
        let (dev, ino) = file_identity(meta, path).unwrap_or_else(|| (0, hash_path(path)));
        Self {
            dev,
            ino,
            len: meta.len(),
        }
    }
}

#[cfg(windows)]
fn file_identity(_meta: &fs::Metadata, path: &Path) -> Option<(u64, u64)> {
    use std::ffi::c_void;
    use std::fs::File;

    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    #[allow(dead_code)]
    struct ByHandleFileInformation {
        attributes: u32,
        creation_time: FileTime,
        last_access_time: FileTime,
        last_write_time: FileTime,
        volume_serial: u32,
        size_high: u32,
        size_low: u32,
        links: u32,
        index_high: u32,
        index_low: u32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandle(
            handle: *mut c_void,
            information: *mut ByHandleFileInformation,
        ) -> i32;
    }

    let file = File::open(path).ok()?;
    let mut info = ByHandleFileInformation::default();
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) };
    if ok == 0 {
        return None;
    }
    let index = (u64::from(info.index_high) << 32) | u64::from(info.index_low);
    Some((u64::from(info.volume_serial), index))
}

#[cfg(unix)]
fn file_identity(meta: &fs::Metadata, _path: &Path) -> Option<(u64, u64)> {
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(any(windows, unix)))]
fn file_identity(_meta: &fs::Metadata, _path: &Path) -> Option<(u64, u64)> {
    None
}

fn hash_path(path: &Path) -> u64 {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    hasher.finish()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UsageKind {
    Python,
    Dependency,
    Bytecode,
    Download,
    Other,
}

fn is_bytecode_path(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == "__pycache__")
        || matches!(
            path.extension().and_then(|s| s.to_str()),
            Some("pyc" | "pyo")
        )
}

fn classify(path: &Path, paths: &Paths) -> UsageKind {
    if path.starts_with(paths.python_dir()) {
        return UsageKind::Python;
    }
    if path.starts_with(paths.data_root.join("downloads")) {
        return UsageKind::Download;
    }
    if path.starts_with(paths.uv_cache_dir()) {
        return UsageKind::Dependency;
    }
    let instances = paths.data_root.join("instances");
    if path.starts_with(&instances) {
        if is_bytecode_path(path) {
            return UsageKind::Bytecode;
        }
        if path.components().any(|c| c.as_os_str() == "venvs") {
            return UsageKind::Dependency;
        }
    }
    UsageKind::Other
}

/// 数据根的物理占用。类别互斥：硬链接只算一次，字节码按所在目录单独列出。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhysicalUsage {
    /// 全数据根按物理文件去重后的字节数。
    pub total_bytes: u64,
    pub python_bytes: u64,
    /// uv 缓存与各实例 venv 去重后的字节数。
    pub dependency_bytes: u64,
    /// venv 里运行生成的 `.pyc`；可以安全清理。
    pub bytecode_bytes: u64,
    /// 安装后留下的 zip；可以安全清理。
    pub download_bytes: u64,
    /// 当前可直接回收的数量：字节码 + 下载包。
    pub reclaimable_bytes: u64,
}

pub fn physical_usage(paths: &Paths) -> std::io::Result<PhysicalUsage> {
    let mut seen: HashSet<FileKey> = HashSet::new();
    let mut out = PhysicalUsage::default();
    let mut stack = vec![paths.data_root.clone()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let meta = match entry.metadata() {
                Ok(meta) => meta,
                Err(_) => continue,
            };
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            if !seen.insert(FileKey::new(&meta, &path)) {
                continue;
            }
            let len = meta.len();
            out.total_bytes += len;
            match classify(&path, paths) {
                UsageKind::Python => out.python_bytes += len,
                UsageKind::Dependency => out.dependency_bytes += len,
                UsageKind::Bytecode => out.bytecode_bytes += len,
                UsageKind::Download => out.download_bytes += len,
                UsageKind::Other => {}
            }
        }
    }
    out.reclaimable_bytes = out.bytecode_bytes + out.download_bytes;
    Ok(out)
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
    fn physical_usage_counts_hardlinked_dependencies_once() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(
            InstallMode::CurrentUser,
            dir.path().join("app"),
            dir.path().join("data"),
        )
        .unwrap();
        let cache = paths.uv_cache_dir();
        fs::create_dir_all(&cache).unwrap();
        let wheel = cache.join("wheel.bin");
        fs::write(&wheel, vec![0; 100]).unwrap();

        let site = paths
            .instance_venvs_dir("mod@a")
            .join("aaaaaaaaaaaaaaaa")
            .join("Lib")
            .join("site-packages");
        fs::create_dir_all(&site).unwrap();
        fs::hard_link(&wheel, site.join("wheel.bin")).unwrap();

        let pycache = site.join("__pycache__");
        fs::create_dir_all(&pycache).unwrap();
        fs::write(pycache.join("mod.cpython-312.pyc"), vec![0; 50]).unwrap();

        fs::create_dir_all(paths.python_dir()).unwrap();
        fs::write(paths.python_dir().join("python.exe"), vec![0; 10]).unwrap();

        let downloads = paths.data_root.join("downloads");
        fs::create_dir_all(&downloads).unwrap();
        fs::write(downloads.join("mod.zip"), vec![0; 20]).unwrap();

        let usage = physical_usage(&paths).unwrap();
        assert_eq!(usage.dependency_bytes, 100, "硬链接不能重复计空间");
        assert_eq!(usage.bytecode_bytes, 50);
        assert_eq!(usage.python_bytes, 10);
        assert_eq!(usage.download_bytes, 20);
        assert_eq!(usage.reclaimable_bytes, 70);
        assert_eq!(usage.total_bytes, 180);
    }

    #[test]
    fn dir_usage_ignores_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(dir_usage(&dir.path().join("nope")).unwrap(), 0);
    }
}
