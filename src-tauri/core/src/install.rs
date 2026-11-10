//! `state/install.json` 是双根路径的唯一真源：安装器写一次，环境引擎只读。
//!
//! 便携版先做写探针，程序根目录不可写时回退到用户目录。回退结果同样写进 install.json，
//! 下次启动不重新探测，避免同一台机器换了一次权限就换一套路径。

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths::{InstallMode, Paths, PathsError};
use crate::state::{self, StateError};

#[derive(Debug, thiserror::Error)]
pub enum InstallStateError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error(transparent)]
    Paths(#[from] PathsError),
    #[error("install.json 的 schema_version 应为 1，实际 {0}")]
    BadVersion(u8),
    #[error("install.json 的 uv_sha256 不是 sha256: 加 64 位小写十六进制：{0}")]
    BadUvHash(String),
    #[error("install.json 的 uv_path 不是绝对路径：{0}")]
    BadUvPath(String),
    #[error("install.json 与当前 Paths 不一致：{0}")]
    RootMismatch(String),
    #[error("便携版程序根不可写，且没有可用的回退目录")]
    NoWritableRoot,
    #[error("install.json 已存在，不允许覆盖：{0}")]
    AlreadyExists(String),
}

/// 与 contracts/install-v1.schema.json 对齐。`paths` 用 flatten 平铺成三个字段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallState {
    pub schema_version: u8,
    #[serde(flatten)]
    pub paths: Paths,
    pub uv_path: PathBuf,
    pub uv_sha256: String,
    pub uv_version: String,
    pub written_at: String,
}

impl InstallState {
    pub fn initialize(
        paths: &Paths,
        uv_path: PathBuf,
        uv_sha256: String,
        uv_version: String,
    ) -> Result<Self, InstallStateError> {
        let value = Self {
            schema_version: 1,
            paths: paths.clone(),
            uv_path,
            uv_sha256,
            uv_version,
            written_at: crate::time::utc_now(),
        };
        value.validate()?;
        let path = paths.install_state_path();
        if path.exists() {
            return Err(InstallStateError::AlreadyExists(path.display().to_string()));
        }
        state::write_json_once(&path, &value)?;
        Ok(value)
    }

    /// 只从传入的数据根读取，不重新推断安装模式与两个根。
    pub fn load(paths: &Paths) -> Result<Self, InstallStateError> {
        let value: Self = state::read_json(&paths.install_state_path())?;
        value.validate()?;
        value.check_against(paths)?;
        Ok(value)
    }

    /// 仅供测试与迁移显式覆盖已存在的 install.json。生产入口只走 `initialize` 与 `load`。
    pub fn overwrite_for_tests(&self) -> Result<(), InstallStateError> {
        Ok(state::write_json(&self.paths.install_state_path(), self)?)
    }

    /// 环境引擎拿到的是校验过的 `Paths`，不再自己拼路径。
    pub fn resolved_paths(&self) -> Result<Paths, InstallStateError> {
        Ok(self.paths.validate()?)
    }

    fn validate(&self) -> Result<(), InstallStateError> {
        if self.schema_version != 1 {
            return Err(InstallStateError::BadVersion(self.schema_version));
        }
        self.paths.validate()?;
        if !is_hash(&self.uv_sha256) {
            return Err(InstallStateError::BadUvHash(self.uv_sha256.clone()));
        }
        if !self.uv_path.is_absolute() {
            return Err(InstallStateError::BadUvPath(
                self.uv_path.display().to_string(),
            ));
        }
        Ok(())
    }

    fn check_against(&self, paths: &Paths) -> Result<(), InstallStateError> {
        if self.paths != *paths {
            return Err(InstallStateError::RootMismatch(format!(
                "install.json={:?} 当前={:?}",
                self.paths, paths
            )));
        }
        Ok(())
    }
}

fn is_hash(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Paths {
    /// 反序列化后复核不变量，避免有人手改 install.json 绕过 `Paths::new`。
    pub fn validate(&self) -> Result<Paths, PathsError> {
        Paths::new(
            self.install_mode,
            self.install_root.clone(),
            self.data_root.clone(),
        )
    }
}

/// 只写一个临时探针文件，成功即删除。
pub fn probe_writable(dir: &Path) -> bool {
    if fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(format!(
        ".write-probe-{}-{}",
        std::process::id(),
        crate::time::utc_now().replace([':', '-'], "")
    ));
    let ok = fs::write(&probe, b"probe").is_ok();
    if ok {
        let _ = fs::remove_file(&probe);
    }
    ok
}

pub fn local_appdata_fallback() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|v| PathBuf::from(v).join("DounaiLauncher"))
}

/// 便携版的两个根。`exe_dir` 是程序根，数据根优先落在同级的 `data/`。
pub fn portable_roots(
    exe_dir: &Path,
    fallback: Option<PathBuf>,
) -> Result<Paths, InstallStateError> {
    let preferred = exe_dir.join("data");
    let data_root = if probe_writable(&preferred) {
        preferred
    } else {
        fallback.ok_or(InstallStateError::NoWritableRoot)?
    };
    Ok(Paths::new(
        InstallMode::Portable,
        exe_dir.to_path_buf(),
        data_root,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(install: &Path, data: &Path) -> Paths {
        Paths::new(
            InstallMode::CurrentUser,
            install.to_path_buf(),
            data.to_path_buf(),
        )
        .unwrap()
    }

    fn state(p: &Paths) -> InstallState {
        InstallState {
            schema_version: 1,
            paths: p.clone(),
            uv_path: p.bundled_uv_path(),
            uv_sha256: format!("sha256:{}", "a".repeat(64)),
            uv_version: "0.13.7".into(),
            written_at: "2026-11-10T00:00:00Z".into(),
        }
    }

    #[test]
    fn initialize_writes_flattened_install_json() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(&dir.path().join("app"), &dir.path().join("data"));
        let s = InstallState::initialize(
            &p,
            p.bundled_uv_path(),
            format!("sha256:{}", "a".repeat(64)),
            "0.13.7".into(),
        )
        .unwrap();
        let text = fs::read_to_string(p.install_state_path()).unwrap();
        assert!(text.contains("\"install_mode\": \"current_user\""));
        assert!(text.contains("\"data_root\""));
        assert!(!text.contains("\"paths\""));
        assert_eq!(InstallState::load(&p).unwrap(), s);
    }

    #[test]
    fn initialize_refuses_to_overwrite_existing_state() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(&dir.path().join("app"), &dir.path().join("data"));
        let first = InstallState::initialize(
            &p,
            p.bundled_uv_path(),
            format!("sha256:{}", "a".repeat(64)),
            "0.13.7".into(),
        )
        .unwrap();
        let err = InstallState::initialize(
            &p,
            p.bundled_uv_path(),
            format!("sha256:{}", "b".repeat(64)),
            "0.13.7".into(),
        )
        .unwrap_err();
        assert!(matches!(err, InstallStateError::AlreadyExists(_)));
        assert_eq!(InstallState::load(&p).unwrap(), first);
    }

    #[test]
    fn load_rejects_changed_roots() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(&dir.path().join("app"), &dir.path().join("data"));
        state(&p).overwrite_for_tests().unwrap();
        let other = Paths::new(
            InstallMode::Portable,
            dir.path().join("other-app"),
            dir.path().join("data"),
        )
        .unwrap();
        assert!(matches!(
            InstallState::load(&other),
            Err(InstallStateError::RootMismatch(_))
        ));
    }

    #[test]
    fn validate_rejects_bad_version_hash_and_relative_uv() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(&dir.path().join("app"), &dir.path().join("data"));
        let mut s = state(&p);
        s.schema_version = 2;
        assert!(matches!(
            s.validate(),
            Err(InstallStateError::BadVersion(2))
        ));

        let mut s = state(&p);
        s.uv_sha256 = "a".repeat(64);
        assert!(matches!(s.validate(), Err(InstallStateError::BadUvHash(_))));

        let mut s = state(&p);
        s.uv_path = PathBuf::from("bin/uv.exe");
        assert!(matches!(s.validate(), Err(InstallStateError::BadUvPath(_))));
    }

    #[test]
    fn portable_prefers_exe_sibling_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("app");
        fs::create_dir_all(&exe).unwrap();
        let p = portable_roots(&exe, Some(dir.path().join("fallback"))).unwrap();
        assert_eq!(p.install_mode, InstallMode::Portable);
        assert_eq!(p.data_root, exe.join("data"));
        assert!(p.data_root.starts_with(&exe));
        assert_ne!(p.install_root, p.data_root);
    }

    #[test]
    fn write_probe_does_not_leave_files() {
        let dir = tempfile::tempdir().unwrap();
        assert!(probe_writable(dir.path()));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
