//! 状态文件的读写收敛到这里：临时文件加原子替换，避免半截 JSON 被下一次启动读到。
//!
//! 只防意外断电与写入中断，不防本地恶意改写。模块与用户都拥有当前用户权限。

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::de::DeserializeOwned;
use serde::Serialize;
use sha2::{Digest, Sha256};

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("状态文件 {path} 读写失败：{source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("状态文件 {path} 不是合法 JSON：{message}")]
    Json { path: String, message: String },
    #[error("状态路径没有文件名：{0}")]
    NoFileName(String),
}

impl StateError {
    pub fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.display().to_string(),
            source,
        }
    }
    pub fn json(path: &Path, message: impl Into<String>) -> Self {
        Self::Json {
            path: path.display().to_string(),
            message: message.into(),
        }
    }
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, StateError> {
    let text = fs::read_to_string(path).map_err(|e| StateError::io(path, e))?;
    serde_json::from_str(&text).map_err(|e| StateError::json(path, e.to_string()))
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), StateError> {
    let mut bytes =
        serde_json::to_vec_pretty(value).map_err(|e| StateError::json(path, e.to_string()))?;
    bytes.push(b'\n');
    write_bytes_atomic(path, &bytes)
}

/// 只写一次：目标已存在时直接失败，不覆盖。安装器用它写 install.json。
///
/// 写入中途失败会删掉半截文件，让下一次启动明确报错，而不是读到不完整的状态。
pub fn write_json_once<T: Serialize>(path: &Path, value: &T) -> Result<(), StateError> {
    let mut bytes =
        serde_json::to_vec_pretty(value).map_err(|e| StateError::json(path, e.to_string()))?;
    bytes.push(b'\n');
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| StateError::io(path, e))?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| StateError::io(path, e))?;
    let write = (|| -> std::io::Result<()> {
        file.write_all(&bytes)?;
        file.flush()?;
        file.sync_all()
    })();
    if let Err(e) = write {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(StateError::io(path, e));
    }
    Ok(())
}

pub fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<(), StateError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| StateError::io(path, e))?;
    }
    let name = path
        .file_name()
        .ok_or_else(|| StateError::NoFileName(path.display().to_string()))?
        .to_string_lossy();
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_file_name(format!("{name}.tmp-{}-{seq}", std::process::id()));

    let write = (|| -> std::io::Result<()> {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.flush()?;
        f.sync_all()
    })();
    if let Err(e) = write {
        let _ = fs::remove_file(&tmp);
        return Err(StateError::io(&tmp, e));
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(StateError::io(path, e));
    }
    Ok(())
}

/// 带 `sha256:` 前缀的文件哈希，与契约里的哈希格式一致。
pub fn file_sha256_prefixed(path: &Path) -> Result<String, StateError> {
    let mut f = File::open(path).map_err(|e| StateError::io(path, e))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| StateError::io(path, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

pub fn remove_if_exists(path: &Path) -> Result<(), StateError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(StateError::io(path, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_roundtrip_is_atomic_and_pretty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("state.json");
        write_json(&path, &json!({ "a": 1 })).unwrap();
        let back: serde_json::Value = read_json(&path).unwrap();
        assert_eq!(back["a"], 1);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.ends_with('\n'));
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn rewrite_replaces_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        write_json(&path, &json!({ "v": 1 })).unwrap();
        write_json(&path, &json!({ "v": 2 })).unwrap();
        let back: serde_json::Value = read_json(&path).unwrap();
        assert_eq!(back["v"], 2);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn truncated_json_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        fs::write(&path, b"{\"a\":").unwrap();
        assert!(matches!(
            read_json::<serde_json::Value>(&path),
            Err(StateError::Json { .. })
        ));
    }

    #[test]
    fn file_hash_matches_known_vector() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abc");
        fs::write(&path, b"abc").unwrap();
        assert_eq!(
            file_sha256_prefixed(&path).unwrap(),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn missing_file_hash_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(file_sha256_prefixed(&dir.path().join("nope")).is_err());
    }

    #[test]
    fn remove_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x");
        remove_if_exists(&path).unwrap();
        fs::write(&path, b"x").unwrap();
        remove_if_exists(&path).unwrap();
        assert!(!path.exists());
    }
}
