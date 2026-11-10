//! M1 环境引擎。对外只有 `prepare_instance` 与 `verify_instance` 两个入口。
//!
//! `prepare_instance` 用系统 uv 跑真命令，测试与 CI 通过 `EnvContext` 注入 mock。
//! `verify_instance` 只读状态，不联网、不重建 venv。

pub mod bootstrap;
pub mod deps;
pub mod error;
pub mod python;
pub mod runner;
pub mod usage;
pub mod venv;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::install::InstallState;
use crate::manifest::{expand_placeholders, ModuleManifestV2};
use crate::paths::{InstallMode, Paths};
use crate::state;

pub use bootstrap::{
    locate_uv, CurlUvFetcher, UvBinary, UvFetcher, UV_DOWNLOAD_URL, UV_MIN_VERSION, UV_VERSION,
};
pub use deps::{commit, verify_snapshot, DepsMetadata, ResolvedDeps};
pub use error::{EnvError, EnvErrorKind, FailureContext};
pub use python::{ensure_python, ManagedPython};
pub use runner::{
    MockConfig, MockFailure, MockUvRunner, SystemUvRunner, UvOutput, UvRequest, UvRunner,
};
pub use usage::{cache_usage, CacheUsage};
pub use venv::{prepare_venv, VenvOutcome};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvOptions {
    /// 断网重跑时全程带 `--offline`，并注入 `UV_OFFLINE=1`。
    pub offline: bool,
    /// 程序根缺 uv.exe 时是否允许按固定 URL 下载。
    pub allow_uv_download: bool,
    /// 备用端口段由调用方给；None 时用 manifest 的规范端口。
    pub port: Option<u16>,
}

impl Default for EnvOptions {
    fn default() -> Self {
        Self {
            offline: std::env::var("UV_OFFLINE")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            allow_uv_download: true,
            port: None,
        }
    }
}

pub struct EnvContext<'a> {
    pub install: &'a InstallState,
    pub options: EnvOptions,
    pub runner: &'a dyn UvRunner,
    pub fetcher: &'a dyn UvFetcher,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedEnv {
    /// `instances/<key>/venv/Scripts/python.exe`
    pub venv_python: PathBuf,
    pub working_dir: PathBuf,
    pub env_vars: BTreeMap<String, String>,
    pub python_build_id: String,
    pub uv_version: String,
    pub snapshot_sha256: String,
}

/// `state/runtime.json` 是引擎内部状态，不进跨仓契约。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeState {
    pub schema_version: u8,
    pub install_mode: InstallMode,
    pub python_install_dir: PathBuf,
    pub python_build_id: String,
    pub uv_version: String,
    pub instances: BTreeMap<String, InstanceRuntime>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceRuntime {
    pub env: PreparedEnv,
    pub prepared_at: String,
}

impl RuntimeState {
    fn load_required(paths: &Paths, instance_id: &str) -> Result<Self, EnvError> {
        let path = paths.runtime_state_path();
        if !path.exists() {
            return Err(EnvError::new(
                EnvErrorKind::State,
                instance_id,
                format!("没有 {}，实例尚未准备", path.display()),
            ));
        }
        state::read_json(&path).map_err(|e| {
            EnvError::new(
                EnvErrorKind::State,
                instance_id,
                format!("读取 runtime.json 失败：{e}"),
            )
        })
    }

    fn load_or_empty(
        paths: &Paths,
        install: &InstallState,
        instance_id: &str,
    ) -> Result<Self, EnvError> {
        if !paths.runtime_state_path().exists() {
            return Ok(Self {
                schema_version: 1,
                install_mode: paths.install_mode,
                python_install_dir: paths.python_dir(),
                python_build_id: String::new(),
                uv_version: install.uv_version.clone(),
                instances: BTreeMap::new(),
            });
        }
        Self::load_required(paths, instance_id)
    }

    fn save(&self, paths: &Paths, instance_id: &str) -> Result<(), EnvError> {
        state::write_json(&paths.runtime_state_path(), self).map_err(|e| {
            EnvError::new(
                EnvErrorKind::State,
                instance_id,
                format!("写 runtime.json 失败：{e}"),
            )
        })
    }
}

/// 生产入口：读 install.json，用随包自带的 uv 跑真命令。
pub fn prepare_instance(
    paths: &Paths,
    instance_id: &str,
    manifest: &ModuleManifestV2,
    module_dir: &Path,
) -> Result<PreparedEnv, EnvError> {
    let install = InstallState::load(paths).map_err(|e| {
        EnvError::new(
            EnvErrorKind::RootMismatch,
            instance_id,
            format!("读取 install.json 失败：{e}"),
        )
    })?;
    let runner = SystemUvRunner::new(paths.install_root.clone());
    let fetcher = CurlUvFetcher;
    let ctx = EnvContext {
        install: &install,
        options: EnvOptions::default(),
        runner: &runner,
        fetcher: &fetcher,
    };
    prepare_instance_with(paths, instance_id, manifest, module_dir, &ctx)
}

/// 注入入口：M2 装完包后调用本函数，M3 只消费返回的 `PreparedEnv`。
pub fn prepare_instance_with(
    paths: &Paths,
    instance_id: &str,
    manifest: &ModuleManifestV2,
    module_dir: &Path,
    ctx: &EnvContext<'_>,
) -> Result<PreparedEnv, EnvError> {
    crate::paths::validate_instance_id(instance_id).map_err(|e| {
        EnvError::new(
            EnvErrorKind::State,
            instance_id,
            format!("instance_id 不合法：{e}"),
        )
    })?;
    if !manifest.role.needs_runtime() {
        return Err(EnvError::new(
            EnvErrorKind::NoRuntime,
            instance_id,
            "shell 实例没有 Python 运行时，命令面应跳过",
        ));
    }
    if !module_dir.is_absolute() || !module_dir.is_dir() {
        return Err(EnvError::new(
            EnvErrorKind::State,
            instance_id,
            format!("模块目录必须是存在的绝对路径：{}", module_dir.display()),
        ));
    }
    ctx.install.resolved_paths().map_err(|e| {
        EnvError::new(
            EnvErrorKind::RootMismatch,
            instance_id,
            format!("install.json 的路径不合法：{e}"),
        )
    })?;
    if ctx.install.paths != *paths {
        return Err(EnvError::new(
            EnvErrorKind::RootMismatch,
            instance_id,
            "install.json 与当前 Paths 不一致",
        ));
    }

    let working_dir = resolve_working_dir(
        module_dir,
        manifest.entry.working_dir.as_deref(),
        instance_id,
    )?;
    let mut resolved_seen: Option<ResolvedDeps> = None;
    let mut created_venv = false;

    let result: Result<(PreparedEnv, RuntimeState), EnvError> = (|| {
        let mut runtime = RuntimeState::load_or_empty(paths, ctx.install, instance_id)?;
        let uv = bootstrap::locate_uv(
            paths,
            ctx.install,
            &ctx.options,
            ctx.runner,
            ctx.fetcher,
            instance_id,
        )?;
        let python = python::ensure_python(paths, &uv, &ctx.options, ctx.runner, instance_id)?;
        let mut resolved = deps::resolve(
            paths,
            instance_id,
            manifest,
            module_dir,
            &uv,
            &python,
            &ctx.options,
            ctx.runner,
        )?;
        resolved_seen = Some(resolved.clone());
        let outcome = venv::prepare_venv(
            paths,
            instance_id,
            manifest,
            module_dir,
            &uv,
            &python,
            &resolved,
            &ctx.options,
            ctx.runner,
        )?;
        created_venv = outcome.created;

        deps::commit(paths, &mut resolved)?;
        let env_vars = build_env_vars(
            paths,
            instance_id,
            manifest,
            module_dir,
            outcome.venv_python.clone(),
            &python,
            &ctx.options,
        )?;
        let prepared = PreparedEnv {
            venv_python: outcome.venv_python,
            working_dir,
            env_vars,
            python_build_id: python.build_id.clone(),
            uv_version: uv.version.clone(),
            snapshot_sha256: resolved.sha256.clone(),
        };
        runtime.install_mode = paths.install_mode;
        runtime.python_install_dir = paths.python_dir();
        runtime.python_build_id = python.build_id.clone();
        runtime.uv_version = uv.version.clone();
        runtime.instances.insert(
            instance_id.to_string(),
            InstanceRuntime {
                env: prepared.clone(),
                prepared_at: crate::time::utc_now(),
            },
        );
        Ok((prepared, runtime))
    })();

    match result {
        Ok((prepared, runtime)) => {
            runtime.save(paths, instance_id)?;
            Ok(prepared)
        }
        Err(mut e) => {
            let problems = venv::cleanup_after_failure(
                paths,
                instance_id,
                resolved_seen.as_ref(),
                created_venv,
            );
            if !problems.is_empty() {
                e.message
                    .push_str(&format!("\n清理失败：{}", problems.join("；")));
            }
            Err(e)
        }
    }
}

/// 生产入口：只读状态，比对哈希与安装根，不启动任何子进程。
pub fn verify_instance(paths: &Paths, instance_id: &str) -> Result<PreparedEnv, EnvError> {
    let install = InstallState::load(paths).map_err(|e| {
        EnvError::new(
            EnvErrorKind::RootMismatch,
            instance_id,
            format!("读取 install.json 失败：{e}"),
        )
    })?;
    verify_instance_with(paths, instance_id, &install)
}

pub fn verify_instance_with(
    paths: &Paths,
    instance_id: &str,
    install: &InstallState,
) -> Result<PreparedEnv, EnvError> {
    if install.paths != *paths {
        return Err(EnvError::new(
            EnvErrorKind::RootMismatch,
            instance_id,
            "install.json 与当前 Paths 不一致",
        ));
    }
    let runtime = RuntimeState::load_required(paths, instance_id)?;
    let instance = runtime.instances.get(instance_id).ok_or_else(|| {
        EnvError::new(
            EnvErrorKind::State,
            instance_id,
            "runtime.json 里没有这个实例",
        )
    })?;
    if runtime.install_mode != paths.install_mode
        || runtime.python_install_dir != paths.python_dir()
        || runtime.uv_version != install.uv_version
    {
        return Err(EnvError::new(
            EnvErrorKind::RootMismatch,
            instance_id,
            "安装模式、Python 安装目录或 uv 版本已变化，需要重建 venv",
        ));
    }
    let hash = deps::verify_snapshot(paths, instance_id, &instance.env.snapshot_sha256)?;
    let meta: DepsMetadata = state::read_json(&paths.deps_meta_path(instance_id)).map_err(|e| {
        EnvError::new(
            EnvErrorKind::SnapshotMismatch,
            instance_id,
            format!("读取依赖元数据失败：{e}"),
        )
    })?;
    if meta.snapshot_sha256 != hash || meta.python_build_id != runtime.python_build_id {
        return Err(EnvError::new(
            EnvErrorKind::SnapshotMismatch,
            instance_id,
            "依赖元数据与锁文件或解释器标识不一致",
        ));
    }
    if !instance.env.venv_python.exists() {
        return Err(EnvError::new(
            EnvErrorKind::VenvSync,
            instance_id,
            format!("venv 解释器不存在：{}", instance.env.venv_python.display()),
        ));
    }
    Ok(instance.env.clone())
}

fn resolve_working_dir(
    module_dir: &Path,
    spec: Option<&str>,
    instance_id: &str,
) -> Result<PathBuf, EnvError> {
    let Some(spec) = spec else {
        return Ok(module_dir.to_path_buf());
    };
    let module_text = module_dir.to_string_lossy().into_owned();
    let text = expand_placeholders(spec, &[("module_dir", module_text.as_str())]);
    let path = PathBuf::from(&text);
    let resolved = if path.is_absolute() {
        path
    } else {
        crate::security::safe_join(module_dir, &text).map_err(|e| {
            EnvError::new(
                EnvErrorKind::State,
                instance_id,
                format!("working_dir 越界：{e}"),
            )
        })?
    };
    if !resolved.is_dir() {
        return Err(EnvError::new(
            EnvErrorKind::State,
            instance_id,
            format!("working_dir 不是目录：{}", resolved.display()),
        ));
    }
    // 绝对路径也要复核 containment：manifest 不能把工作目录指到程序根或别处。
    // cwd 与工作目录都做规范化，挡住 junction 与大小写差异。
    let base = std::fs::canonicalize(module_dir).map_err(|e| {
        EnvError::new(
            EnvErrorKind::State,
            instance_id,
            format!("规范化模块目录失败：{e}"),
        )
    })?;
    let target = std::fs::canonicalize(&resolved).map_err(|e| {
        EnvError::new(
            EnvErrorKind::State,
            instance_id,
            format!("规范化 working_dir 失败：{e}"),
        )
    })?;
    if !target.starts_with(&base) {
        return Err(EnvError::new(
            EnvErrorKind::State,
            instance_id,
            format!("working_dir 越出模块目录：{}", resolved.display()),
        ));
    }
    Ok(resolved)
}

/// 引擎注入的变量不允许模块覆盖。Windows 的环境变量名大小写不敏感，统一转大写比较。
fn reserved_env_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.starts_with("UV_")
        || matches!(
            upper.as_str(),
            "PATH"
                | "PYTHONHOME"
                | "PYTHONPATH"
                | "VIRTUAL_ENV"
                | "CONDA_PREFIX"
                | "CONDA_DEFAULT_ENV"
                | "SYSTEMROOT"
                | "TEMP"
                | "TMP"
        )
}

fn valid_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[allow(clippy::too_many_arguments)]
fn build_env_vars(
    paths: &Paths,
    instance_id: &str,
    manifest: &ModuleManifestV2,
    module_dir: &Path,
    venv_python: PathBuf,
    python: &ManagedPython,
    options: &EnvOptions,
) -> Result<BTreeMap<String, String>, EnvError> {
    let mut vars = python::python_bound_env(paths, python);
    let venv = venv::venv_dir(paths, instance_id);
    vars.insert(
        "UV_PROJECT_ENVIRONMENT".into(),
        venv.to_string_lossy().into_owned(),
    );
    let port = options
        .port
        .or_else(|| manifest.default_port())
        .unwrap_or(0)
        .to_string();
    let data_dir = paths
        .instance_data_dir_keyed(instance_id)
        .to_string_lossy()
        .into_owned();
    let module_text = module_dir.to_string_lossy().into_owned();
    let install_root = paths.install_root.to_string_lossy().into_owned();
    let venv_python_text = venv_python.to_string_lossy().into_owned();
    let venv_text = venv.to_string_lossy().into_owned();
    let pairs: Vec<(&str, &str)> = vec![
        ("port", port.as_str()),
        ("data_dir", data_dir.as_str()),
        ("module_dir", module_text.as_str()),
        ("install_root", install_root.as_str()),
        ("venv_python", venv_python_text.as_str()),
        ("instance_venv", venv_text.as_str()),
    ];
    for (key, value) in &manifest.entry.env {
        if !valid_env_key(key) || reserved_env_key(key) {
            return Err(EnvError::new(
                EnvErrorKind::State,
                instance_id,
                format!("manifest 声明了非法或保留的环境变量名：{key}"),
            ));
        }
        vars.insert(key.clone(), expand_placeholders(value, &pairs));
    }
    // 三个上游都是 src layout，依赖同步不装项目自身，靠 PYTHONPATH 让 `-m` 找到包。
    let src_dir = module_dir.join("src");
    if src_dir.is_dir() {
        let sep = if cfg!(windows) { ";" } else { ":" };
        let value = match vars.get("PYTHONPATH") {
            Some(existing) => format!("{}{sep}{existing}", src_dir.display()),
            None => src_dir.to_string_lossy().into_owned(),
        };
        vars.insert("PYTHONPATH".into(), value);
    }
    Ok(vars)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::instance_key;
    use std::fs;

    struct FakeFetcher;
    impl UvFetcher for FakeFetcher {
        fn fetch(&self, _url: &str) -> Result<Vec<u8>, EnvError> {
            Err(EnvError::new(EnvErrorKind::UvMissing, "", "测试不下载"))
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        paths: Paths,
        install: InstallState,
        module: PathBuf,
        manifest: ModuleManifestV2,
        runner: MockUvRunner,
        fetcher: FakeFetcher,
        options: EnvOptions,
    }

    impl Fixture {
        fn new(options: EnvOptions, manifest_json: &str, runner: MockUvRunner) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let install_root = dir.path().join("app");
            let data_root = dir.path().join("data");
            let module = dir.path().join("module");
            fs::create_dir_all(install_root.join("bin")).unwrap();
            fs::create_dir_all(&data_root).unwrap();
            fs::create_dir_all(&module).unwrap();
            fs::write(module.join("pyproject.toml"), b"[project]\nname='x'\n").unwrap();
            fs::write(module.join("uv.lock"), b"version = 1\n").unwrap();
            let exe = b"uv binary";
            fs::write(install_root.join("bin").join("uv.exe"), exe).unwrap();
            let paths = Paths::new(
                InstallMode::CurrentUser,
                install_root.clone(),
                data_root.clone(),
            )
            .unwrap();
            let install = InstallState {
                schema_version: 1,
                paths: paths.clone(),
                uv_path: paths.bundled_uv_path(),
                uv_sha256: format!("sha256:{}", crate::security::sha256_hex(exe)),
                uv_version: UV_VERSION.into(),
                written_at: "2026-11-10T00:00:00Z".into(),
            };
            install.overwrite_for_tests().unwrap();
            Fixture {
                _dir: dir,
                paths,
                install,
                module,
                manifest: serde_json::from_str(manifest_json).unwrap(),
                runner,
                fetcher: FakeFetcher,
                options,
            }
        }

        fn context(&self) -> EnvContext<'_> {
            EnvContext {
                install: &self.install,
                options: self.options.clone(),
                runner: &self.runner,
                fetcher: &self.fetcher,
            }
        }

        fn prepare(&self) -> Result<PreparedEnv, EnvError> {
            prepare_instance_with(
                &self.paths,
                "dounai/omninexus@stable@main",
                &self.manifest,
                &self.module,
                &self.context(),
            )
        }
    }

    const PYPROJECT: &str = include_str!("../../../../contracts/examples/omninexus.v2.json");

    #[test]
    fn prepare_writes_all_state_files_and_expands_env() {
        let f = Fixture::new(
            EnvOptions {
                offline: false,
                allow_uv_download: false,
                port: Some(18102),
            },
            PYPROJECT,
            MockUvRunner::default(),
        );
        let env = f.prepare().unwrap();
        let id = "dounai/omninexus@stable@main";
        assert_eq!(env.venv_python, f.paths.instance_venv_python(id));
        assert!(env.venv_python.exists());
        assert_eq!(env.working_dir, f.module);
        assert_eq!(env.python_build_id, "cpython-3.12.8-windows-x86_64-none");
        assert_eq!(env.uv_version, UV_VERSION);
        assert!(env.snapshot_sha256.starts_with("sha256:"));
        assert_eq!(env.env_vars["OMNI_PORT"], "18102");
        assert!(env.env_vars["OMNI_DATA_DIR"].contains("data"));
        assert_eq!(env.env_vars["UV_MANAGED_PYTHON"], "1");
        assert!(f.paths.deps_lock_path(id).exists());
        assert!(f.paths.deps_meta_path(id).exists());
        assert!(f.paths.runtime_state_path().exists());
        assert!(!f
            .paths
            .deps_lock_path(id)
            .with_extension("lock.new")
            .exists());

        let calls = f.runner.calls();
        assert!(calls
            .iter()
            .any(|c| c.args.contains(&"--version".to_string())));
        assert!(calls
            .iter()
            .any(|c| c.args.contains(&"--generate-hashes".to_string())));
        assert!(calls.iter().any(|c| c.args[0] == "venv"));
        assert!(calls
            .iter()
            .any(|c| c.args.contains(&"--dry-run".to_string())));
        assert!(!calls
            .iter()
            .any(|c| c.args.contains(&"install".to_string())));
    }

    #[test]
    fn verify_detects_tampered_lock_without_overwriting_it() {
        let f = Fixture::new(
            EnvOptions {
                offline: false,
                allow_uv_download: false,
                port: None,
            },
            PYPROJECT,
            MockUvRunner::default(),
        );
        let prepared = f.prepare().unwrap();
        let id = "dounai/omninexus@stable@main";
        let lock = f.paths.deps_lock_path(id);
        fs::write(&lock, b"tampered").unwrap();
        let e = verify_instance_with(&f.paths, id, &f.install).unwrap_err();
        assert_eq!(e.kind, EnvErrorKind::SnapshotMismatch);
        assert_eq!(fs::read_to_string(&lock).unwrap(), "tampered");
        assert!(prepared.snapshot_sha256.starts_with("sha256:"));
    }

    #[test]
    fn verify_reports_changed_install_mode() {
        let f = Fixture::new(
            EnvOptions {
                offline: false,
                allow_uv_download: false,
                port: None,
            },
            PYPROJECT,
            MockUvRunner::default(),
        );
        f.prepare().unwrap();
        let changed = Paths::new(
            InstallMode::Portable,
            f.paths.install_root.clone(),
            f.paths.data_root.clone(),
        )
        .unwrap();
        let mut install = f.install.clone();
        install.paths = changed.clone();
        let e =
            verify_instance_with(&changed, "dounai/omninexus@stable@main", &install).unwrap_err();
        assert_eq!(e.kind, EnvErrorKind::RootMismatch);
    }

    #[test]
    fn shell_instance_is_skipped_by_command_layer() {
        let ui = include_str!("../../../../contracts/examples/dounai-ui.v2.json");
        let f = Fixture::new(
            EnvOptions {
                offline: false,
                allow_uv_download: false,
                port: None,
            },
            ui,
            MockUvRunner::default(),
        );
        assert_eq!(f.prepare().unwrap_err().kind, EnvErrorKind::NoRuntime);
        assert!(f.runner.calls().is_empty());
    }

    #[test]
    fn failed_resolve_removes_new_venv_and_leaves_no_state() {
        let runner = MockUvRunner::new(MockConfig {
            fail: Some(MockFailure {
                when_contains: "pip compile".into(),
                kind: EnvErrorKind::DepsResolve,
                message: "包名写错".into(),
            }),
            ..MockConfig::default()
        });
        let f = Fixture::new(
            EnvOptions {
                offline: false,
                allow_uv_download: false,
                port: None,
            },
            PYPROJECT,
            runner,
        );
        let e = f.prepare().unwrap_err();
        assert_eq!(e.kind, EnvErrorKind::DepsResolve);
        let id = "dounai/omninexus@stable@main";
        assert!(!f.paths.deps_lock_path(id).exists());
        assert!(!f.paths.runtime_state_path().exists());
        assert!(!f.paths.instance_venv_python(id).exists());
    }

    #[test]
    fn second_prepare_reuses_venv_and_stays_offline() {
        let f = Fixture::new(
            EnvOptions {
                offline: false,
                allow_uv_download: false,
                port: None,
            },
            PYPROJECT,
            MockUvRunner::default(),
        );
        f.prepare().unwrap();
        let first_calls = f.runner.calls().len();
        let offline = EnvOptions {
            offline: true,
            allow_uv_download: false,
            port: None,
        };
        let ctx = EnvContext {
            install: &f.install,
            options: offline,
            runner: &f.runner,
            fetcher: &f.fetcher,
        };
        let env = prepare_instance_with(
            &f.paths,
            "dounai/omninexus@stable@main",
            &f.manifest,
            &f.module,
            &ctx,
        )
        .unwrap();
        assert!(env.snapshot_sha256.starts_with("sha256:"));
        let calls = f.runner.calls();
        let delta = &calls[first_calls..];
        assert!(
            !delta.iter().any(|c| c.args[0] == "venv"),
            "已有 venv 不应重建"
        );
        assert!(delta
            .iter()
            .any(|c| c.args.contains(&"--offline".to_string())));
        assert!(!delta
            .iter()
            .any(|c| c.args.contains(&"install".to_string())));
    }

    #[test]
    fn instance_key_is_used_for_state_filenames() {
        let f = Fixture::new(
            EnvOptions {
                offline: false,
                allow_uv_download: false,
                port: None,
            },
            PYPROJECT,
            MockUvRunner::default(),
        );
        let id = "dounai/omninexus@stable@main";
        assert!(f
            .paths
            .deps_lock_path(id)
            .to_string_lossy()
            .contains(&instance_key(id)));
        assert_eq!(
            expand_placeholders("{module_dir}", &[("module_dir", "X:/m")]),
            "X:/m"
        );
    }
}
