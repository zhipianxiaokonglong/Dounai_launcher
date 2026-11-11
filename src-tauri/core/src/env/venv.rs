//! 按实例建 venv，并按离线优先策略同步依赖。
//!
//! `uv_lock` 分支不加 `--frozen` 时，uv 会在项目根生成 `uv.lock`，污染 dir 实例的源码目录。
//! 这里把 venv 落到数据根，并把 `UV_PROJECT_ENVIRONMENT` 指过去。

use std::fs;
use std::path::{Path, PathBuf};

use super::bootstrap::UvBinary;
use super::deps::ResolvedDeps;
use super::error::{EnvError, EnvErrorKind};
use super::python::{python_bound_env, ManagedPython};
use super::runner::{UvRequest, UvRunner};
use super::EnvOptions;
use crate::manifest::{DepsKind, ModuleManifestV2};
use crate::paths::Paths;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenvOutcome {
    pub venv_python: PathBuf,
    pub created: bool,
}

/// 本函数内部失败时删掉本次新建的 venv。`uv venv` 建了目录却失败、
/// 或首次 sync 失败，都会走 Drop，不再依赖上层能否拿到 created 标记。
struct VenvRollback {
    path: PathBuf,
    created: bool,
    armed: bool,
}

impl Drop for VenvRollback {
    fn drop(&mut self) {
        if self.created && self.armed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn err(kind: EnvErrorKind, instance_id: &str, message: impl Into<String>) -> EnvError {
    EnvError::new(kind, instance_id, message)
}

pub fn venv_dir(paths: &Paths, instance_id: &str) -> PathBuf {
    paths.instance_dir_keyed(instance_id).join("venv")
}

#[allow(clippy::too_many_arguments)]
pub fn sync_request(
    paths: &Paths,
    instance_id: &str,
    manifest: &ModuleManifestV2,
    module_dir: &Path,
    uv: &UvBinary,
    python: &ManagedPython,
    resolved: &ResolvedDeps,
    venv: &Path,
    options: &EnvOptions,
    dry_run: bool,
) -> UvRequest {
    let kind = manifest
        .runtime
        .as_ref()
        .map(|r| r.deps.kind)
        .unwrap_or(DepsKind::Pyproject);
    let mut req = if kind == DepsKind::UvLock {
        UvRequest::new(
            &uv.path,
            instance_id,
            &["sync", "--frozen", "--no-install-project", "--no-dev"],
        )
    } else {
        let mut req = UvRequest::new(&uv.path, instance_id, &["pip", "sync"]);
        req.push_path(&resolved.staging_path);
        req.args.push("--python".into());
        req.push_path(&paths.instance_venv_python(instance_id));
        req.args.push("--require-hashes".into());
        req
    };

    if dry_run {
        req.args.push("--dry-run".into());
        req.args.push("--offline".into());
        req.offline = true;
    } else if options.offline {
        req.args.push("--offline".into());
        req.offline = true;
    }
    req.cwd = if kind == DepsKind::UvLock {
        module_dir.to_path_buf()
    } else {
        paths.data_root.clone()
    };
    req.env = python_bound_env(paths, python);
    if kind == DepsKind::UvLock {
        req.env.insert(
            "UV_PROJECT_ENVIRONMENT".into(),
            venv.to_string_lossy().into_owned(),
        );
    }
    req
}

#[allow(clippy::too_many_arguments)]
pub fn prepare_venv(
    paths: &Paths,
    instance_id: &str,
    manifest: &ModuleManifestV2,
    module_dir: &Path,
    uv: &UvBinary,
    python: &ManagedPython,
    resolved: &ResolvedDeps,
    options: &EnvOptions,
    runner: &dyn UvRunner,
) -> Result<VenvOutcome, EnvError> {
    let venv = venv_dir(paths, instance_id);
    let venv_python = paths.instance_venv_python(instance_id);
    let created = !venv_python.exists();
    let mut rollback = VenvRollback {
        path: venv.clone(),
        created,
        armed: true,
    };

    // uv_lock 的 sync 只消费项目根的 uv.lock，且没有参数可指向别处。
    // 执行前再比对一次，确保「哈希过的快照」就是「uv 即将读的那份」。
    if manifest.runtime.as_ref().map(|r| r.deps.kind) == Some(DepsKind::UvLock) {
        let consumed = module_dir.join("uv.lock");
        let actual = crate::state::file_sha256_prefixed(&consumed).map_err(|e| {
            err(
                EnvErrorKind::SnapshotMismatch,
                instance_id,
                format!("读取项目根 uv.lock 失败：{e}"),
            )
        })?;
        if actual != resolved.sha256 {
            return Err(err(
                EnvErrorKind::SnapshotMismatch,
                instance_id,
                format!(
                    "项目根 uv.lock 与校验过的快照不符：期望 {}，实际 {}",
                    resolved.sha256, actual
                ),
            ));
        }
    }

    if created {
        let mut req = UvRequest::new(&uv.path, instance_id, &["venv"]);
        req.push_path(&venv);
        req.args.push("--python".into());
        req.push_path(&python.path);
        req.cwd = paths.data_root.clone();
        req.env = python_bound_env(paths, python);
        runner
            .run(&req)
            .map_err(|e| e.reclassify_if_generic(EnvErrorKind::VenvCreate))?;
        if !venv_python.exists() {
            return Err(err(
                EnvErrorKind::VenvCreate,
                instance_id,
                format!("venv 建好后没有解释器：{}", venv_python.display()),
            ));
        }
    }

    let sync = sync_request(
        paths,
        instance_id,
        manifest,
        module_dir,
        uv,
        python,
        resolved,
        &venv,
        options,
        false,
    );
    // 只给泛类错误补语义；离线缓存缺失等具体结论保留原类别。
    runner
        .run(&sync)
        .map_err(|e| e.reclassify_if_generic(EnvErrorKind::VenvSync))?;

    // 每次启动都用离线 dry-run 复核缓存，不走联网、不静默重建。
    let dry = sync_request(
        paths,
        instance_id,
        manifest,
        module_dir,
        uv,
        python,
        resolved,
        &venv,
        options,
        true,
    );
    runner.run(&dry).map_err(|e| {
        let kind = if options.offline {
            EnvErrorKind::OfflineCacheMiss
        } else {
            EnvErrorKind::VenvSync
        };
        e.reclassify_if_generic(kind)
    })?;

    rollback.armed = false;
    Ok(VenvOutcome {
        venv_python,
        created,
    })
}

/// 失败时清掉本次新建的 venv 与临时快照，保留上次成功的正式锁文件与元数据。
/// 清理失败不静默吞掉，交给调用方附到错误上下文里。
pub fn cleanup_after_failure(
    paths: &Paths,
    instance_id: &str,
    resolved: Option<&ResolvedDeps>,
    created_venv: bool,
) -> Vec<String> {
    let mut problems = Vec::new();
    let staging = resolved
        .map(|r| r.staging_path.clone())
        .unwrap_or_else(|| super::deps::staging_path(paths, instance_id));
    if let Err(e) = crate::state::remove_if_exists(&staging) {
        problems.push(format!("清理临时快照 {} 失败：{e}", staging.display()));
    }
    if created_venv {
        if let Err(e) = fs::remove_dir_all(venv_dir(paths, instance_id)) {
            problems.push(format!("清理本次新建的 venv 失败：{e}"));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::deps::DepsMetadata;
    use crate::env::runner::{MockConfig, MockFailure, MockUvRunner};
    use crate::manifest::ModuleManifestV2;
    use crate::paths::{InstallMode, Paths};

    fn setup() -> (
        tempfile::TempDir,
        Paths,
        PathBuf,
        UvBinary,
        ManagedPython,
        ResolvedDeps,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let module = dir.path().join("module");
        fs::create_dir_all(&module).unwrap();
        let p = Paths::new(
            InstallMode::CurrentUser,
            dir.path().join("app"),
            dir.path().join("data"),
        )
        .unwrap();
        let uv = UvBinary {
            path: PathBuf::from("uv.exe"),
            version: "0.13.7".into(),
            sha256: format!("sha256:{}", "a".repeat(64)),
            downloaded_bytes: 0,
        };
        let python = ManagedPython {
            path: dir.path().join("managed/python.exe"),
            build_id: "cpython-3.12.8-windows-x86_64-none".into(),
            install_dir: dir.path().join("managed"),
        };
        let staging = super::super::deps::staging_path(&p, "mod@a");
        fs::create_dir_all(p.deps_dir()).unwrap();
        fs::write(&staging, b"pinned").unwrap();
        let sha = format!("sha256:{}", "a".repeat(64));
        let resolved = ResolvedDeps {
            lock_path: p.content_lock_path("mod@a", &sha),
            staging_path: staging,
            sha256: sha.clone(),
            metadata: DepsMetadata {
                schema_version: 1,
                instance_id: "mod@a".into(),
                kind: DepsKind::Pyproject,
                source_path: "pyproject.toml".into(),
                extras: vec![],
                snapshot_sha256: sha,
                python_build_id: python.build_id.clone(),
                generated_at: "2026-11-10T00:00:00Z".into(),
                offline_capable: false,
            },
        };
        (dir, p, module, uv, python, resolved)
    }

    fn manifest() -> ModuleManifestV2 {
        serde_json::from_str(include_str!(
            "../../../../contracts/examples/omninexus.v2.json"
        ))
        .unwrap()
    }

    #[test]
    fn creates_venv_and_syncs_with_require_hashes() {
        let (_dir, paths, module, uv, python, resolved) = setup();
        let runner = MockUvRunner::default();
        let out = prepare_venv(
            &paths,
            "mod@a",
            &manifest(),
            &module,
            &uv,
            &python,
            &resolved,
            &EnvOptions::default(),
            &runner,
        )
        .unwrap();
        assert!(out.created);
        assert!(out.venv_python.exists());
        let calls = runner.calls();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].args[0], "venv");
        assert!(calls[0].args.contains(&"--python".to_string()));
        assert!(calls[1].args.contains(&"--require-hashes".to_string()));
        assert!(!calls[1].args.contains(&"--frozen".to_string()));
        assert!(calls[2].args.contains(&"--dry-run".to_string()));
        assert!(calls[2].args.contains(&"--offline".to_string()));
        assert!(calls[2].offline);
        assert_eq!(calls[1].cwd, paths.data_root);
    }

    fn uv_lock_manifest() -> ModuleManifestV2 {
        serde_json::from_str(
            &include_str!("../../../../contracts/examples/omninexus.v2.json").replace(
                "\"kind\": \"pyproject\", \"path\": \"pyproject.toml\"",
                "\"kind\": \"uv_lock\", \"path\": \"uv.lock\"",
            ),
        )
        .unwrap()
    }

    #[test]
    fn uv_lock_uses_frozen_and_external_project_environment() {
        let (_dir, paths, module, uv, python, mut resolved) = setup();
        // sync 消费项目根的 uv.lock，夹具要提供同一份内容并把期望哈希对齐。
        fs::write(module.join("uv.lock"), b"pinned").unwrap();
        resolved.sha256 = crate::state::file_sha256_prefixed(&module.join("uv.lock")).unwrap();
        let m = uv_lock_manifest();
        let runner = MockUvRunner::default();
        prepare_venv(
            &paths,
            "mod@a",
            &m,
            &module,
            &uv,
            &python,
            &resolved,
            &EnvOptions::default(),
            &runner,
        )
        .unwrap();
        let sync = &runner.calls()[1];
        assert!(sync.args.contains(&"--frozen".to_string()));
        assert!(sync.args.contains(&"--no-install-project".to_string()));
        assert!(sync.args.contains(&"--no-dev".to_string()));
        assert_eq!(sync.cwd, module);
        assert_eq!(
            sync.env["UV_PROJECT_ENVIRONMENT"],
            venv_dir(&paths, "mod@a").to_string_lossy()
        );
    }

    #[test]
    fn uv_lock_rejects_changed_source_lock() {
        let (_dir, paths, module, uv, python, mut resolved) = setup();
        let lock = module.join("uv.lock");
        fs::write(&lock, b"original").unwrap();
        resolved.sha256 = crate::state::file_sha256_prefixed(&lock).unwrap();
        fs::write(&lock, b"tampered").unwrap();
        let runner = MockUvRunner::default();
        let e = prepare_venv(
            &paths,
            "mod@a",
            &uv_lock_manifest(),
            &module,
            &uv,
            &python,
            &resolved,
            &EnvOptions::default(),
            &runner,
        )
        .unwrap_err();
        assert_eq!(e.kind, EnvErrorKind::SnapshotMismatch);
        assert!(runner.calls().is_empty(), "不该在锁文件被改后启动 uv");
    }

    #[test]
    fn offline_run_marks_sync_offline() {
        let (_dir, paths, module, uv, python, resolved) = setup();
        let runner = MockUvRunner::default();
        let options = EnvOptions {
            offline: true,
            ..EnvOptions::default()
        };
        prepare_venv(
            &paths,
            "mod@a",
            &manifest(),
            &module,
            &uv,
            &python,
            &resolved,
            &options,
            &runner,
        )
        .unwrap();
        assert!(runner.calls()[1].args.contains(&"--offline".to_string()));
    }

    #[test]
    fn offline_cache_miss_is_reported() {
        let (_dir, paths, module, uv, python, resolved) = setup();
        let runner = MockUvRunner::new(MockConfig {
            offline_cache_miss: true,
            ..MockConfig::default()
        });
        let options = EnvOptions {
            offline: true,
            ..EnvOptions::default()
        };
        let e = prepare_venv(
            &paths,
            "mod@a",
            &manifest(),
            &module,
            &uv,
            &python,
            &resolved,
            &options,
            &runner,
        )
        .unwrap_err();
        assert_eq!(e.kind, EnvErrorKind::OfflineCacheMiss);
    }

    #[test]
    fn failed_sync_keeps_previous_lock_and_existing_venv() {
        let (_dir, paths, _module, _uv, _python, resolved) = setup();
        fs::create_dir_all(paths.deps_generation_dir("mod@a")).unwrap();
        fs::write(&resolved.lock_path, b"previous").unwrap();
        fs::create_dir_all(venv_dir(&paths, "mod@a").join("Scripts")).unwrap();
        let runner = MockUvRunner::new(MockConfig {
            fail: Some(MockFailure {
                when_contains: "pip sync".into(),
                kind: EnvErrorKind::VenvSync,
                message: "sync failed".into(),
            }),
            ..MockConfig::default()
        });
        // 先手工建好 venv，确认失败清理不会误删旧环境。
        fs::write(paths.instance_venv_python("mod@a"), b"old").unwrap();
        let m = manifest();
        let dummy_uv = UvBinary {
            path: PathBuf::from("uv.exe"),
            version: "0.13.7".into(),
            sha256: String::new(),
            downloaded_bytes: 0,
        };
        let dummy_python = ManagedPython {
            path: PathBuf::from("python.exe"),
            build_id: String::new(),
            install_dir: PathBuf::new(),
        };
        let e = prepare_venv(
            &paths,
            "mod@a",
            &m,
            Path::new("."),
            &dummy_uv,
            &dummy_python,
            &resolved,
            &EnvOptions::default(),
            &runner,
        )
        .unwrap_err();
        assert_eq!(e.kind, EnvErrorKind::VenvSync);
        cleanup_after_failure(&paths, "mod@a", Some(&resolved), false);
        assert!(!resolved.staging_path.exists());
        assert_eq!(fs::read_to_string(&resolved.lock_path).unwrap(), "previous");
        assert!(paths.instance_venv_python("mod@a").exists());
    }

    #[test]
    fn cleanup_removes_venv_created_by_failed_prepare() {
        let (_dir, paths, _module, _uv, _python, resolved) = setup();
        fs::create_dir_all(venv_dir(&paths, "mod@a").join("Scripts")).unwrap();
        fs::write(paths.instance_venv_python("mod@a"), b"new").unwrap();
        cleanup_after_failure(&paths, "mod@a", Some(&resolved), true);
        assert!(!venv_dir(&paths, "mod@a").exists());
        assert!(!resolved.staging_path.exists());
    }
}
