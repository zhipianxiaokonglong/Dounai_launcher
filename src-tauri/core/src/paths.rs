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

    pub fn bundled_uv_path(&self) -> PathBuf {
        self.install_root.join("bin").join("uv.exe")
    }

    pub fn downloaded_uv_path(&self) -> PathBuf {
        self.data_root.join("runtime").join("bin").join("uv.exe")
    }

    pub fn install_state_path(&self) -> PathBuf {
        self.state_dir().join("install.json")
    }

    pub fn runtime_state_path(&self) -> PathBuf {
        self.state_dir().join("runtime.json")
    }

    pub fn deps_dir(&self) -> PathBuf {
        self.state_dir().join("deps")
    }

    /// 落盘目录名用编码后的 key，不把 instance_id 里的斜杠当目录分隔符。
    pub fn instance_dir_keyed(&self, instance_id: &str) -> PathBuf {
        self.data_root
            .join("instances")
            .join(instance_key(instance_id))
    }

    /// venv 按 generation 分目录。`runtime.json` 指向哪一代就用哪一代，
    /// 准备新依赖时不会原地改写旧环境。
    pub fn instance_venvs_dir(&self, instance_id: &str) -> PathBuf {
        self.instance_dir_keyed(instance_id).join("venvs")
    }

    pub fn generation_venv_dir(&self, instance_id: &str, sha256: &str) -> PathBuf {
        self.instance_venvs_dir(instance_id)
            .join(generation_tag(sha256))
    }

    pub fn generation_venv_python(&self, instance_id: &str, sha256: &str) -> PathBuf {
        self.generation_venv_dir(instance_id, sha256)
            .join("Scripts")
            .join("python.exe")
    }

    /// 新建 venv 的临时目录。全部校验通过后 rename 成 generation 目录，
    /// 崩溃只会留下 `.staging`，正式目录永远完整。
    pub fn staging_venv_dir(&self, instance_id: &str, sha256: &str) -> PathBuf {
        self.instance_venvs_dir(instance_id)
            .join(format!("{}.staging", generation_tag(sha256)))
    }

    pub fn instance_data_dir_keyed(&self, instance_id: &str) -> PathBuf {
        self.instance_dir_keyed(instance_id).join("data")
    }

    pub fn instance_logs_dir_keyed(&self, instance_id: &str) -> PathBuf {
        self.instance_dir_keyed(instance_id).join("logs")
    }

    /// 每个实例的 generation 目录。锁与元数据都按内容哈希命名，写入后永不改写。
    pub fn deps_generation_dir(&self, instance_id: &str) -> PathBuf {
        self.deps_dir().join(instance_key(instance_id))
    }

    /// 内容寻址锁：`state/deps/<key>/<sha256>.lock`。
    pub fn content_lock_path(&self, instance_id: &str, sha256: &str) -> PathBuf {
        self.deps_generation_dir(instance_id)
            .join(format!("{}.lock", generation_sha(sha256)))
    }

    /// 内容寻址元数据，与锁同一个 generation。
    pub fn content_meta_path(&self, instance_id: &str, sha256: &str) -> PathBuf {
        self.deps_generation_dir(instance_id)
            .join(format!("{}.json", generation_sha(sha256)))
    }

    /// 解析阶段的临时文件。它不参与一致性判定，每次 resolve 先删。
    pub fn deps_staging_path(&self, instance_id: &str) -> PathBuf {
        self.deps_dir()
            .join(format!("{}.lock.new", instance_key(instance_id)))
    }
}

/// 生成路径只接受 `sha256:` 加 64 位小写十六进制。非法值落到不存在的文件名上，
/// 由读取方报 SnapshotMismatch，不把未校验的字符串拼进路径。
pub fn generation_sha(sha256: &str) -> &str {
    let hex = sha256.strip_prefix("sha256:").unwrap_or(sha256);
    if hex.len() == 64
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        hex
    } else {
        "invalid"
    }
}

/// venv 目录名取哈希前 16 位，与锁文件的完整哈希区分开。
pub fn generation_tag(sha256: &str) -> String {
    let hex = generation_sha(sha256);
    if hex.len() == 64 {
        hex[..16].to_string()
    } else {
        "invalid".to_string()
    }
}

/// 校验逻辑 instance_id。斜杠是命名空间分隔符，不是目录分隔符；其余字符收紧到白名单。
pub fn validate_instance_id(instance_id: &str) -> Result<(), PathsError> {
    let bad = || PathsError::BadInstanceId(instance_id.to_string());
    if instance_id.is_empty() || instance_id.len() > 128 {
        return Err(bad());
    }
    if !instance_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '@'))
    {
        return Err(bad());
    }
    if instance_id
        .split('/')
        .any(|seg| seg.is_empty() || seg == "." || seg == "..")
    {
        return Err(bad());
    }
    Ok(())
}

/// 把逻辑 instance_id 编码成安全目录名。
///
/// 点号也替换掉：`CON.txt` 这类 id 若保留点号，首段会落在 Windows 保留设备名上，
/// 目录创建会失败或被重定向。哈希取 16 位十六进制，避免 32 位的碰撞面。
pub fn instance_key(instance_id: &str) -> String {
    let safe: String = instance_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '@') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let digest = crate::security::sha256_hex(instance_id.as_bytes());
    format!("{safe}-{}", &digest[..16])
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
            p.instance_venvs_dir(id),
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
        for bad in [
            "../evil", "a/b", "a\\b", "", " spaced", "CON", ".", "..", "CON.txt",
        ] {
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

    #[test]
    fn instance_key_is_safe_and_collision_resistant() {
        let a = instance_key("dounai/omninexus@stable@main");
        let b = instance_key("dounai_omninexus@stable@main");
        assert_ne!(a, b);
        assert!(!a.contains('/') && !a.contains('\\'));
        assert!(is_safe_filename(&a));
        assert_eq!(a, instance_key("dounai/omninexus@stable@main"));
    }

    #[test]
    fn instance_key_replaces_dots_and_uses_long_hash() {
        let key = instance_key("CON.txt");
        assert!(!key.contains('.'), "点号不得留在 key 里：{key}");
        assert!(is_safe_filename(&key), "保留设备名不得落在 key 首段：{key}");
        let tail = key.rsplit('-').next().unwrap();
        assert_eq!(tail.len(), 16);
        assert!(tail.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn instance_id_validation_rejects_hostile_values() {
        for bad in ["", "a//b", "a/../b", "../evil", "a\\b", "a?b", "CON/.."] {
            assert!(validate_instance_id(bad).is_err(), "未被拒绝：{bad:?}");
        }
        assert!(validate_instance_id(&"x".repeat(129)).is_err());
        assert!(validate_instance_id("dounai/omninexus@stable@main").is_ok());
    }

    #[test]
    fn keyed_and_state_paths_stay_under_data_root() {
        let p = paths();
        let id = "dounai/omninexus@stable@main";
        let sha = format!("sha256:{}", "a".repeat(64));
        for path in [
            p.instance_dir_keyed(id),
            p.instance_venvs_dir(id),
            p.generation_venv_dir(id, &sha),
            p.generation_venv_python(id, &sha),
            p.staging_venv_dir(id, &sha),
            p.instance_data_dir_keyed(id),
            p.instance_logs_dir_keyed(id),
            p.deps_generation_dir(id),
            p.content_lock_path(id, &sha),
            p.content_meta_path(id, &sha),
            p.deps_staging_path(id),
            p.install_state_path(),
            p.runtime_state_path(),
            p.downloaded_uv_path(),
        ] {
            assert!(path.starts_with(&p.data_root), "越界：{}", path.display());
            assert!(
                !path.starts_with(&p.install_root),
                "写入程序根：{}",
                path.display()
            );
        }
        assert!(p.bundled_uv_path().starts_with(&p.install_root));
        assert!(p
            .generation_venv_python(id, &sha)
            .ends_with("Scripts/python.exe"));
        assert!(p
            .generation_venv_dir(id, &sha)
            .to_string_lossy()
            .contains(&generation_tag(&sha)));
    }

    #[test]
    fn generation_sha_only_accepts_lowercase_digest() {
        let hex = "a".repeat(64);
        assert_eq!(generation_sha(&format!("sha256:{hex}")), hex.as_str());
        assert_eq!(generation_sha("sha256:XYZ"), "invalid");
        assert_eq!(
            generation_sha(&format!("sha256:{}", "A".repeat(64))),
            "invalid"
        );
        assert_eq!(generation_sha("../../etc/passwd"), "invalid");
    }
}
