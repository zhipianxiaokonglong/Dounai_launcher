//! 程序根与数据根由一处计算，其余代码不自行拼接路径。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::security::is_safe_filename;

/// 安装模式。`both` 安装器把运行时选择写成其中一项。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallMode {
    CurrentUser,
    PerMachine,
    Portable,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PathsError {
    #[error("路径必须是绝对路径：{0}")]
    Relative(String),
    #[error("程序根与数据根不能相同：{0}")]
    SameRoot(String),
    #[error("instance_id 非法：{0}")]
    BadInstanceId(String),
}

/// 两个根的布局。字段写进 `state/install.json`，后续启动读取，不重新推断。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Paths {
    pub install_mode: InstallMode,
    pub install_root: PathBuf,
    pub data_root: PathBuf,
}

impl Paths {
    pub fn new(
        install_mode: InstallMode,
        install_root: PathBuf,
        data_root: PathBuf,
    ) -> Result<Self, PathsError> {
        for p in [&install_root, &data_root] {
            if !p.is_absolute() {
                return Err(PathsError::Relative(p.display().to_string()));
            }
        }
        if install_root == data_root {
            return Err(PathsError::SameRoot(data_root.display().to_string()));
        }
        Ok(Self {
            install_mode,
            install_root,
            data_root,
        })
    }

    pub fn python_dir(&self) -> PathBuf {
        self.data_root.join("runtime").join("python")
    }

    pub fn uv_python_install_dir(&self) -> PathBuf {
        self.python_dir()
    }

    pub fn uv_cache_dir(&self) -> PathBuf {
        self.data_root.join("runtime").join("uv-cache")
    }

    pub fn state_dir(&self) -> PathBuf {
        self.data_root.join("state")
    }

    pub fn webview_dir(&self) -> PathBuf {
        self.data_root.join("webview")
    }

    pub fn instance_dir(&self, instance_id: &str) -> Result<PathBuf, PathsError> {
        if !is_safe_filename(instance_id) {
            return Err(PathsError::BadInstanceId(instance_id.to_string()));
        }
        Ok(self.data_root.join("instances").join(instance_id))
    }

    pub fn instance_venv_dir(&self, instance_id: &str) -> Result<PathBuf, PathsError> {
        Ok(self.instance_dir(instance_id)?.join("venv"))
    }

    pub fn instance_data_dir(&self, instance_id: &str) -> Result<PathBuf, PathsError> {
        Ok(self.instance_dir(instance_id)?.join("data"))
    }

    pub fn instance_logs_dir(&self, instance_id: &str) -> Result<PathBuf, PathsError> {
        Ok(self.instance_dir(instance_id)?.join("logs"))
    }

    /// uv 的两个目录都指向数据根，不写程序根。
    pub fn uv_env(&self) -> [(&'static str, PathBuf); 2] {
        [
            ("UV_PYTHON_INSTALL_DIR", self.uv_python_install_dir()),
            ("UV_CACHE_DIR", self.uv_cache_dir()),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:/{name}"))
        } else {
            PathBuf::from(format!("/{name}"))
        }
    }

    fn paths() -> Paths {
        Paths::new(
            InstallMode::PerMachine,
            root("Program Files/DounaiLauncher"),
            root("Users/me/AppData/Local/DounaiLauncher"),
        )
        .unwrap()
    }

    #[test]
    fn all_writable_dirs_stay_under_data_root() {
        let p = paths();
        let id = "omninexus@stable@main";
        for dir in [
            p.python_dir(),
            p.uv_python_install_dir(),
            p.uv_cache_dir(),
            p.state_dir(),
            p.webview_dir(),
            p.instance_dir(id).unwrap(),
            p.instance_venv_dir(id).unwrap(),
            p.instance_data_dir(id).unwrap(),
            p.instance_logs_dir(id).unwrap(),
        ] {
            assert!(dir.starts_with(&p.data_root), "越界：{}", dir.display());
            assert!(
                !dir.starts_with(&p.install_root),
                "写入程序根：{}",
                dir.display()
            );
        }
    }

    #[test]
    fn uv_env_points_at_data_root() {
        let p = paths();
        let env = p.uv_env();
        assert_eq!(env.len(), 2);
        assert_eq!(env[0].0, "UV_PYTHON_INSTALL_DIR");
        assert_eq!(env[1].0, "UV_CACHE_DIR");
        for (_, dir) in env {
            assert!(dir.starts_with(&p.data_root));
        }
    }

    #[test]
    fn rejects_relative_root_and_identical_roots() {
        assert_eq!(
            Paths::new(
                InstallMode::CurrentUser,
                PathBuf::from("relative"),
                root("data")
            ),
            Err(PathsError::Relative("relative".into()))
        );
        let same = root("same");
        assert!(matches!(
            Paths::new(InstallMode::Portable, same.clone(), same),
            Err(PathsError::SameRoot(_))
        ));
    }

    #[test]
    fn instance_id_cannot_escape_instances_dir() {
        let p = paths();
        for bad in ["../evil", "a/b", "a\\b", "", " spaced", "CON"] {
            assert!(p.instance_dir(bad).is_err(), "未被拒绝：{bad:?}");
        }
        assert!(p.instance_dir("omninexus@agent@dev").is_ok());
    }

    #[test]
    fn install_mode_roundtrips_through_json() {
        let p = paths();
        let text = serde_json::to_string(&p).unwrap();
        let back: Paths = serde_json::from_str(&text).unwrap();
        assert_eq!(p, back);
        assert!(text.contains("per_machine"));
    }
}
