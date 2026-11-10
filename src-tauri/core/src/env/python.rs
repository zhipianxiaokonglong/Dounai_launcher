//! 托管 Python 装到数据根，解释器标识由 `uv python list` 读出，不写死补丁版本。
//!
//! 只写 `UV_PYTHON_PREFERENCE` 时 uv 0.13.7 仍会选中系统 CPython 3.13，
//! 因此 `UV_PYTHON` 与 `UV_MANAGED_PYTHON` 一起绑定。

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::bootstrap::UvBinary;
use super::error::{EnvError, EnvErrorKind};
use super::runner::{UvRequest, UvRunner};
use super::EnvOptions;
use crate::paths::Paths;

pub const PYTHON_SERIES: &str = "3.12";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedPython {
    pub path: PathBuf,
    pub build_id: String,
    pub install_dir: PathBuf,
}

/// 装解释器阶段还没有 `UV_PYTHON` 可用，先给这五个变量。
pub fn base_uv_env(paths: &Paths) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert(
        "UV_PYTHON_INSTALL_DIR".into(),
        paths.uv_python_install_dir().to_string_lossy().into_owned(),
    );
    env.insert(
        "UV_CACHE_DIR".into(),
        paths.uv_cache_dir().to_string_lossy().into_owned(),
    );
    env.insert("UV_PYTHON_NO_REGISTRY".into(), "1".into());
    // uv 0.13.7 拒绝 UV_MANAGED_PYTHON 与 UV_PYTHON_PREFERENCE 同时出现，只留前者。
    env.insert("UV_MANAGED_PYTHON".into(), "1".into());
    env
}

/// 依赖解析与同步阶段把解释器钉到托管 3.12 的绝对路径。
pub fn python_bound_env(paths: &Paths, python: &ManagedPython) -> BTreeMap<String, String> {
    let mut env = base_uv_env(paths);
    env.insert(
        "UV_PYTHON".into(),
        python.path.to_string_lossy().into_owned(),
    );
    env
}

fn list_request(uv: &UvBinary, paths: &Paths, instance_id: &str) -> UvRequest {
    let mut req = UvRequest::new(
        &uv.path,
        instance_id,
        &["python", "list", "--only-installed"],
    );
    req.cwd = paths.data_root.clone();
    req.env = base_uv_env(paths);
    req
}

/// 已装 3.12 时跳过下载，断网重跑因此不会重新下载解释器。
pub fn ensure_python(
    paths: &Paths,
    uv: &UvBinary,
    options: &EnvOptions,
    runner: &dyn UvRunner,
    instance_id: &str,
) -> Result<ManagedPython, EnvError> {
    let list = list_request(uv, paths, instance_id);
    let first = runner
        .run(&list)
        .map_err(|e| e.reclassify_if_generic(EnvErrorKind::PythonInstall))?;
    let install_dir = paths.uv_python_install_dir();
    if !has_managed_python(&first.stdout, &install_dir) {
        let mut install =
            UvRequest::new(&uv.path, instance_id, &["python", "install", PYTHON_SERIES]);
        install.args.push("--install-dir".into());
        install.push_path(&paths.uv_python_install_dir());
        install.args.push("--no-registry".into());
        install.args.push("--managed-python".into());
        install.cwd = paths.data_root.clone();
        install.env = base_uv_env(paths);
        install.offline = options.offline;
        runner
            .run(&install)
            .map_err(|e| e.reclassify_if_generic(EnvErrorKind::PythonInstall))?;
    }

    let second = runner
        .run(&list)
        .map_err(|e| e.reclassify_if_generic(EnvErrorKind::PythonInstall))?;
    let (build_id, python_exe) =
        parse_managed_python(&second.stdout, &install_dir, &paths.data_root, instance_id)?;
    let python = ManagedPython {
        path: python_exe,
        build_id,
        install_dir: install_dir.clone(),
    };
    if !python.path.exists() {
        return Err(EnvError::new(
            EnvErrorKind::PythonInstall,
            instance_id,
            format!("托管解释器不存在：{}", python.path.display()),
        )
        .with_output_text(&format!("{}{}", second.stdout, second.stderr)));
    }
    Ok(python)
}

/// 路径统一成正斜杠小写，避免 Windows 盘符大小写与分隔符差异导致漏判。
fn normalized(path: &str) -> String {
    path.replace('\\', "/").to_ascii_lowercase()
}

/// uv 以数据根为 cwd 时输出的是相对路径，取安装目录的最后两级做标记。
fn install_marker(install_dir: &std::path::Path) -> String {
    let norm = normalized(&install_dir.to_string_lossy());
    let parts: Vec<&str> = norm.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() < 2 {
        return norm;
    }
    format!("{}/{}", parts[parts.len() - 2], parts[parts.len() - 1])
}

/// `uv python list` 会连系统 Python 一起列出。只认安装目录落在数据根下的那一行。
fn has_managed_python(stdout: &str, install_dir: &std::path::Path) -> bool {
    let marker = install_marker(install_dir);
    stdout.lines().any(|line| {
        let text = normalized(line);
        text.contains(&marker) && text.contains(&format!("{PYTHON_SERIES}."))
    })
}

/// 从 `uv python list --only-installed` 取实际版本标识与解释器路径。
///
/// 目录名不带补丁号（`cpython-3.12-windows-x86_64-none`），输出里的标识带补丁号
/// （`cpython-3.12.16-windows-x86_64-none`）。按 build_id 拼路径会找不到文件。
pub fn parse_managed_python(
    stdout: &str,
    install_dir: &std::path::Path,
    data_root: &std::path::Path,
    instance_id: &str,
) -> Result<(String, PathBuf), EnvError> {
    let marker = install_marker(install_dir);
    for line in stdout.lines() {
        if !normalized(line).contains(&marker) {
            continue;
        }
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let Some(build) = tokens.first() else {
            continue;
        };
        if !build.starts_with(&format!("cpython-{PYTHON_SERIES}.")) || !build.contains("-windows-")
        {
            continue;
        }
        let Some(raw) = tokens.get(1) else { continue };
        let path = PathBuf::from(raw);
        let absolute = if path.is_absolute() {
            path
        } else {
            data_root.join(path)
        };
        return Ok((build.to_string(), absolute));
    }
    Err(EnvError::new(
        EnvErrorKind::PythonInstall,
        instance_id,
        "uv python list 没有报出装在数据根的托管 3.12 解释器",
    )
    .with_output_text(stdout))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::runner::{MockConfig, MockUvRunner};
    use crate::paths::{InstallMode, Paths};
    use std::path::Path;

    fn paths(dir: &Path) -> Paths {
        Paths::new(InstallMode::CurrentUser, dir.join("app"), dir.join("data")).unwrap()
    }

    fn uv() -> UvBinary {
        UvBinary {
            path: PathBuf::from("uv.exe"),
            version: "0.13.7".into(),
            sha256: format!("sha256:{}", "a".repeat(64)),
            downloaded_bytes: 0,
        }
    }

    #[test]
    fn base_and_bound_env_bind_managed_python() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let base = base_uv_env(&p);
        assert_eq!(base["UV_PYTHON_NO_REGISTRY"], "1");
        assert_eq!(base["UV_MANAGED_PYTHON"], "1");
        assert!(
            !base.contains_key("UV_PYTHON_PREFERENCE"),
            "uv 0.13.7 会拒绝两个变量同时出现"
        );
        assert!(base["UV_CACHE_DIR"].contains("uv-cache"));
        assert!(!base.contains_key("UV_PYTHON"));
        let python = ManagedPython {
            path: p
                .uv_python_install_dir()
                .join("cpython-3.12.8-windows-x86_64-none/python.exe"),
            build_id: "cpython-3.12.8-windows-x86_64-none".into(),
            install_dir: p.uv_python_install_dir(),
        };
        assert_eq!(
            python_bound_env(&p, &python)["UV_PYTHON"],
            python.path.to_string_lossy()
        );
    }

    #[test]
    fn parse_managed_python_uses_reported_path_and_ignores_system_python() {
        let install_dir = std::path::Path::new("C:/data/runtime/python");
        let data_root = std::path::Path::new("C:/data");
        // 真 uv 0.13.7 的实测输出：目录名不带补丁号，路径相对数据根。
        let (build, path) = parse_managed_python(
            "cpython-3.12.16-windows-x86_64-none    C:\\Users\\me\\.local\\bin\\python3.12.exe\n\
             cpython-3.12.16-windows-x86_64-none    runtime\\python\\cpython-3.12-windows-x86_64-none\\python.exe\n",
            install_dir,
            data_root,
            "x",
        )
        .unwrap();
        assert_eq!(build, "cpython-3.12.16-windows-x86_64-none");
        assert_eq!(
            path,
            data_root.join("runtime/python/cpython-3.12-windows-x86_64-none/python.exe")
        );
        // 系统里也有 3.12，但路径不在数据根，不能被当成托管解释器。
        let system_only =
            "cpython-3.12.0-windows-x86_64-none  C:\\Program Files\\Python312\\python.exe\n";
        assert_eq!(
            parse_managed_python(system_only, install_dir, data_root, "x")
                .unwrap_err()
                .kind,
            EnvErrorKind::PythonInstall
        );
        assert!(!has_managed_python(system_only, install_dir));
    }

    #[test]
    fn installed_python_is_reused_without_install_command() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let runner = MockUvRunner::default();
        let python = ensure_python(&p, &uv(), &EnvOptions::default(), &runner, "mod@a").unwrap();
        assert_eq!(python.build_id, "cpython-3.12.8-windows-x86_64-none");
        assert!(python.path.exists());
        assert!(!runner
            .calls()
            .iter()
            .any(|c| c.args.contains(&"install".to_string())));
    }

    #[test]
    fn missing_python_triggers_install_with_managed_flags() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let runner = MockUvRunner::new(MockConfig {
            python_installed: false,
            ..MockConfig::default()
        });
        let python = ensure_python(&p, &uv(), &EnvOptions::default(), &runner, "mod@a").unwrap();
        assert!(python.path.exists());
        let install = runner
            .calls()
            .into_iter()
            .find(|c| {
                c.args.first().map(String::as_str) == Some("python")
                    && c.args.contains(&"install".to_string())
            })
            .expect("应执行 python install");
        assert!(install.args.contains(&"3.12".to_string()));
        assert!(install.args.contains(&"--no-registry".to_string()));
        assert!(install.args.contains(&"--managed-python".to_string()));
        assert_eq!(install.env["UV_PYTHON_NO_REGISTRY"], "1");
    }
}
