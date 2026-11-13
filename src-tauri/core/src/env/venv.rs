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

pub fn venv_dir(paths: &Paths, instance_id: &str, sha256: &str) -> PathBuf {
    paths.generation_venv_dir(instance_id, sha256)
}

fn remove_dir_if_exists(path: &Path) -> std::io::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
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
        req.push_path(&venv.join("Scripts").join("python.exe"));
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
    // 有进度接收器时才加 -v：默认输出只在结束时给一行汇总，界面看不到装到哪个包。
    // 测试与无人值守路径没有接收器，参数保持原样。
    if !dry_run && super::progress::active() {
        req.args.push("-v".into());
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
    let sha = &resolved.sha256;
    let final_dir = venv_dir(paths, instance_id, sha);
    let final_python = paths.generation_venv_python(instance_id, sha);
    let staging = paths.staging_venv_dir(instance_id, sha);

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

    // 同一 generation 的 venv 已完整时直接复用。sync 与 dry-run 都是幂等的，
    // 不会把别的 generation 的环境改成本次依赖。
    if final_python.exists() {
        super::progress::emit(
            super::progress::EnvProgress::new(
                super::progress::EnvPhase::Sync,
                "复用已装好的依赖",
            )
            .with_detail("只做离线复核，不重建 venv"),
        );
        sync_and_verify(
            paths,
            instance_id,
            manifest,
            module_dir,
            uv,
            python,
            resolved,
            &final_dir,
            options,
            runner,
        )?;
        return Ok(VenvOutcome {
            venv_python: final_python,
            created: false,
        });
    }

    // 清掉上次崩溃留下的半截目录。正式目录只在 rename 成功后出现。
    remove_dir_if_exists(&final_dir).map_err(|e| {
        err(
            EnvErrorKind::VenvCreate,
            instance_id,
            format!("清理不完整的 generation venv 失败：{e}"),
        )
    })?;
    remove_dir_if_exists(&staging).map_err(|e| {
        err(
            EnvErrorKind::VenvCreate,
            instance_id,
            format!("清理残留的临时 venv 失败：{e}"),
        )
    })?;

    let mut rollback = VenvRollback {
        path: staging.clone(),
        created: true,
        armed: true,
    };
    let staging_python = staging.join("Scripts").join("python.exe");

    let mut req = UvRequest::new(&uv.path, instance_id, &["venv"]);
    req.push_path(&staging);
    req.args.push("--python".into());
    req.push_path(&python.path);
    req.cwd = paths.data_root.clone();
    req.env = python_bound_env(paths, python);
    runner
        .run(&req)
        .map_err(|e| e.reclassify_if_generic(EnvErrorKind::VenvCreate))?;
    if !staging_python.exists() {
        return Err(err(
            EnvErrorKind::VenvCreate,
            instance_id,
            format!("临时 venv 建好后没有解释器：{}", staging_python.display()),
        ));
    }

    sync_and_verify(
        paths,
        instance_id,
        manifest,
        module_dir,
        uv,
        python,
        resolved,
        &staging,
        options,
        runner,
    )?;

    fs::rename(&staging, &final_dir).map_err(|e| {
        err(
            EnvErrorKind::VenvCreate,
            instance_id,
            format!("把临时 venv 切换成正式目录失败：{e}"),
        )
    })?;
    if !final_python.exists() {
        return Err(err(
            EnvErrorKind::VenvCreate,
            instance_id,
            format!("切换后的 venv 没有解释器：{}", final_python.display()),
        ));
    }
    rollback.armed = false;
    Ok(VenvOutcome {
        venv_python: final_python,
        created: true,
    })
}

#[allow(clippy::too_many_arguments)]
fn sync_and_verify(
    paths: &Paths,
    instance_id: &str,
    manifest: &ModuleManifestV2,
    module_dir: &Path,
    uv: &UvBinary,
    python: &ManagedPython,
    resolved: &ResolvedDeps,
    venv: &Path,
    options: &EnvOptions,
    runner: &dyn UvRunner,
) -> Result<(), EnvError> {
    let sync = sync_request(
        paths,
        instance_id,
        manifest,
        module_dir,
        uv,
        python,
        resolved,
        venv,
        options,
        false,
    );
    // 只给泛类错误补语义；离线缓存缺失等具体结论保留原类别。
    runner
        .run(&sync)
        .map_err(|e| e.reclassify_if_generic(EnvErrorKind::VenvSync))?;

    // 每次 prepare 都用离线 dry-run 复核缓存，不走联网、不静默重建。
    let dry = sync_request(
        paths,
        instance_id,
        manifest,
        module_dir,
        uv,
        python,
        resolved,
        venv,
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
    Ok(())
}

/// 失败时清掉临时快照、临时 venv 与不完整的 generation venv。
/// 已经完整落盘的 generation 目录保留，因为上一次成功的 runtime 可能仍指向它。
/// 清理失败不静默吞掉，交给调用方附到错误上下文里。
pub fn cleanup_after_failure(
    paths: &Paths,
    instance_id: &str,
    resolved: Option<&ResolvedDeps>,
) -> Vec<String> {
    let mut problems = Vec::new();
    let staging = resolved
        .map(|r| r.staging_path.clone())
        .unwrap_or_else(|| super::deps::staging_path(paths, instance_id));
    if let Err(e) = crate::state::remove_if_exists(&staging) {
        problems.push(format!("清理临时快照 {} 失败：{e}", staging.display()));
    }
    let Some(r) = resolved else {
        return problems;
    };
    let sha = &r.sha256;
    let staging_venv = paths.staging_venv_dir(instance_id, sha);
    if let Err(e) = remove_dir_if_exists(&staging_venv) {
        problems.push(format!(
            "清理临时 venv {} 失败：{e}",
            staging_venv.display()
        ));
    }
    let dir = paths.generation_venv_dir(instance_id, sha);
    if dir.exists() && !paths.generation_venv_python(instance_id, sha).exists() {
        if let Err(e) = remove_dir_if_exists(&dir) {
            problems.push(format!("清理不完整的 generation venv 失败：{e}"));
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

    fn test_sha() -> String {
        format!("sha256:{}", "a".repeat(64))
    }

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
            paths
                .staging_venv_dir("mod@a", &resolved.sha256)
                .to_string_lossy()
        );
        assert!(paths
            .generation_venv_python("mod@a", &resolved.sha256)
            .exists());
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
        fs::create_dir_all(venv_dir(&paths, "mod@a", &test_sha()).join("Scripts")).unwrap();
        let runner = MockUvRunner::new(MockConfig {
            fail: Some(MockFailure {
                when_contains: "pip sync".into(),
                kind: EnvErrorKind::VenvSync,
                message: "sync failed".into(),
            }),
            ..MockConfig::default()
        });
        // 先手工建好 venv，确认失败清理不会误删旧环境。
        fs::write(paths.generation_venv_python("mod@a", &test_sha()), b"old").unwrap();
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
        cleanup_after_failure(&paths, "mod@a", Some(&resolved));
        assert!(!resolved.staging_path.exists());
        assert_eq!(fs::read_to_string(&resolved.lock_path).unwrap(), "previous");
        assert!(paths.generation_venv_python("mod@a", &test_sha()).exists());
    }

    #[test]
    fn cleanup_removes_incomplete_generation_but_keeps_complete_one() {
        let (_dir, paths, _module, _uv, _python, resolved) = setup();
        let sha = &resolved.sha256;
        let dir = paths.generation_venv_dir("mod@a", sha);
        let staging = paths.staging_venv_dir("mod@a", sha);
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(&staging).unwrap();
        cleanup_after_failure(&paths, "mod@a", Some(&resolved));
        assert!(!dir.exists(), "不完整的 generation 目录要删掉");
        assert!(!staging.exists(), "临时 venv 要删掉");
        assert!(!resolved.staging_path.exists());

        fs::create_dir_all(dir.join("Scripts")).unwrap();
        fs::write(paths.generation_venv_python("mod@a", sha), b"ok").unwrap();
        fs::create_dir_all(&staging).unwrap();
        cleanup_after_failure(&paths, "mod@a", Some(&resolved));
        assert!(paths.generation_venv_python("mod@a", sha).exists());
    }

    #[test]
    fn failed_prepare_never_leaves_a_formal_generation_venv() {
        let (_dir, paths, module, uv, python, resolved) = setup();
        let runner = MockUvRunner::new(MockConfig {
            fail: Some(MockFailure {
                when_contains: "pip sync".into(),
                kind: EnvErrorKind::VenvSync,
                message: "sync failed".into(),
            }),
            ..MockConfig::default()
        });
        let e = prepare_venv(
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
        .unwrap_err();
        assert_eq!(e.kind, EnvErrorKind::VenvSync);
        assert!(!paths
            .generation_venv_dir("mod@a", &resolved.sha256)
            .exists());
        assert!(!paths.staging_venv_dir("mod@a", &resolved.sha256).exists());
    }

    #[test]
    fn new_generation_gets_its_own_venv_without_touching_previous_one() {
        let (_dir, paths, module, uv, python, mut resolved) = setup();
        let runner = MockUvRunner::default();
        let first = prepare_venv(
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
        assert!(first.created);
        let old_dir = paths.generation_venv_dir("mod@a", &resolved.sha256);
        assert!(old_dir.exists());

        resolved.sha256 = format!("sha256:{}", "b".repeat(64));
        resolved.metadata.snapshot_sha256 = resolved.sha256.clone();
        let second = prepare_venv(
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
        assert_eq!(
            second.venv_python,
            paths.generation_venv_python("mod@a", &resolved.sha256)
        );
        assert!(old_dir.exists(), "旧 generation 的 venv 不得被改写或删除");
    }
}
