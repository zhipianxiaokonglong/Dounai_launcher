//! 依赖来源按 manifest 声明的 `kind` 分派，启动器不嗅探文件，也不按优先级猜测。
//!
//! 解析结果先写 `.lock.new`，venv 与离线校验都成功后才提交成正式锁文件，
//! 中途失败不会覆盖上一次成功的快照。

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::bootstrap::UvBinary;
use super::error::{EnvError, EnvErrorKind};
use super::python::{python_bound_env, ManagedPython};
use super::runner::{UvRequest, UvRunner};
use super::EnvOptions;
use crate::manifest::{DepsKind, ModuleManifestV2};
use crate::paths::Paths;
use crate::state;

/// 与 contracts/deps-v1.schema.json 对齐。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepsMetadata {
    pub schema_version: u8,
    pub instance_id: String,
    pub kind: DepsKind,
    pub source_path: String,
    pub extras: Vec<String>,
    pub snapshot_sha256: String,
    pub python_build_id: String,
    pub generated_at: String,
    pub offline_capable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDeps {
    pub lock_path: PathBuf,
    pub staging_path: PathBuf,
    pub sha256: String,
    pub metadata: DepsMetadata,
}

fn err(instance_id: &str, message: impl Into<String>) -> EnvError {
    EnvError::new(EnvErrorKind::DepsResolve, instance_id, message)
}

/// 声明的文件必须存在且可读。绝对路径、盘符与 `..` 一律拒绝。
pub fn declared_source(
    module_dir: &Path,
    declared: &str,
    instance_id: &str,
) -> Result<PathBuf, EnvError> {
    if declared.is_empty()
        || declared.starts_with('/')
        || declared.contains('\\')
        || declared.contains("..")
        || Path::new(declared).is_absolute()
    {
        return Err(err(instance_id, format!("非法的依赖声明路径：{declared}")));
    }
    let path = crate::security::safe_join(module_dir, declared)
        .map_err(|e| err(instance_id, format!("依赖声明路径越界：{e}")))?;
    // safe_join 只做词法检查；junction 或符号链接仍可能把文件指向模块目录之外。
    let base = fs::canonicalize(module_dir)
        .map_err(|e| err(instance_id, format!("规范化模块目录失败：{e}")))?;
    let canonical = fs::canonicalize(&path)
        .map_err(|e| err(instance_id, format!("规范化依赖文件失败：{e}")))?;
    if !canonical.starts_with(&base) {
        return Err(err(
            instance_id,
            format!("依赖声明路径越出模块目录：{declared}"),
        ));
    }
    if !path.is_file() {
        return Err(err(
            instance_id,
            format!("声明的依赖文件不存在：{}", path.display()),
        ));
    }
    Ok(path)
}

pub fn staging_path(paths: &Paths, instance_id: &str) -> PathBuf {
    PathBuf::from(format!(
        "{}.new",
        paths.deps_lock_path(instance_id).display()
    ))
}

#[allow(clippy::too_many_arguments)]
pub fn resolve(
    paths: &Paths,
    instance_id: &str,
    manifest: &ModuleManifestV2,
    module_dir: &Path,
    uv: &UvBinary,
    python: &ManagedPython,
    options: &EnvOptions,
    runner: &dyn UvRunner,
) -> Result<ResolvedDeps, EnvError> {
    let runtime = manifest
        .runtime
        .as_ref()
        .ok_or_else(|| err(instance_id, "manifest 缺少 runtime 段"))?;
    let deps = &runtime.deps;
    let source = declared_source(module_dir, &deps.path, instance_id)?;
    let staging = staging_path(paths, instance_id);
    state::remove_if_exists(&staging)
        .map_err(|e| err(instance_id, format!("清理旧的临时快照失败：{e}")))?;

    match deps.kind {
        DepsKind::UvLock => {
            // uv sync --frozen 只认项目根的 uv.lock，没有参数可以指向别处的锁文件。
            // 声明成别的路径会让「哈希的快照」与「实际消费的锁」变成两份。
            if deps.path != "uv.lock" {
                return Err(err(
                    instance_id,
                    format!(
                        "uv_lock 必须声明项目根的 uv.lock，实际 {}；uv sync 不读别处的锁",
                        deps.path
                    ),
                ));
            }
            // 不跑 compile，直接把包内 uv.lock 的内容复制成运行时快照哈希锚。
            let bytes = fs::read(&source)
                .map_err(|e| err(instance_id, format!("读取 {} 失败：{e}", source.display())))?;
            state::write_bytes_atomic(&staging, &bytes)
                .map_err(|e| err(instance_id, format!("写临时快照失败：{e}")))?;
        }
        DepsKind::Requirements | DepsKind::Pyproject => {
            let mut request = UvRequest::new(&uv.path, instance_id, &["pip", "compile"]);
            request.push_path(&source);
            request.args.push("--generate-hashes".into());
            request.args.push("--python".into());
            request.push_path(&python.path);
            request.args.push("-o".into());
            request.push_path(&staging);
            for extra in &deps.extras {
                request.args.push("--extra".into());
                request.args.push(extra.clone());
            }
            if options.offline {
                request.args.push("--offline".into());
                request.offline = true;
            }
            request.cwd = module_dir.to_path_buf();
            request.env = python_bound_env(paths, python);
            runner
                .run(&request)
                .map_err(|e| e.reclassify_if_generic(EnvErrorKind::DepsResolve))?;
        }
    }

    let sha256 = state::file_sha256_prefixed(&staging)
        .map_err(|e| err(instance_id, format!("快照哈希计算失败：{e}")))?;
    Ok(ResolvedDeps {
        lock_path: paths.deps_lock_path(instance_id),
        staging_path: staging,
        sha256: sha256.clone(),
        metadata: DepsMetadata {
            schema_version: 1,
            instance_id: instance_id.to_string(),
            kind: deps.kind,
            source_path: deps.path.clone(),
            extras: deps.extras.clone(),
            snapshot_sha256: sha256,
            python_build_id: python.build_id.clone(),
            generated_at: crate::time::utc_now(),
            offline_capable: false,
        },
    })
}

/// venv 与离线校验都成功后调用。正式锁文件用原子写，元数据单独落盘。
pub fn commit(paths: &Paths, resolved: &mut ResolvedDeps) -> Result<(), EnvError> {
    let id = &resolved.metadata.instance_id;
    let bytes =
        fs::read(&resolved.staging_path).map_err(|e| err(id, format!("读取临时快照失败：{e}")))?;
    state::write_bytes_atomic(&resolved.lock_path, &bytes)
        .map_err(|e| err(id, format!("提交锁文件失败：{e}")))?;
    resolved.metadata.offline_capable = true;
    state::write_json(&paths.deps_meta_path(id), &resolved.metadata)
        .map_err(|e| err(id, format!("写依赖元数据失败：{e}")))?;
    state::remove_if_exists(&resolved.staging_path)
        .map_err(|e| err(id, format!("清理临时快照失败：{e}")))?;
    Ok(())
}

/// 篡改锁文件时报 `SnapshotMismatch`，绝不覆盖锁文件，也不静默重建。
pub fn verify_snapshot(
    paths: &Paths,
    instance_id: &str,
    expected: &str,
) -> Result<String, EnvError> {
    let lock = paths.deps_lock_path(instance_id);
    let actual = state::file_sha256_prefixed(&lock).map_err(|e| {
        EnvError::new(
            EnvErrorKind::State,
            instance_id,
            format!("读取锁文件 {} 失败：{e}", lock.display()),
        )
    })?;
    if actual != expected {
        return Err(EnvError::new(
            EnvErrorKind::SnapshotMismatch,
            instance_id,
            format!("锁文件哈希不符：期望 {expected}，实际 {actual}"),
        )
        .with_command_line(&format!("verify {}", lock.display())));
    }
    Ok(actual)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::runner::MockUvRunner;
    use crate::manifest::ModuleManifestV2;
    use crate::paths::{InstallMode, Paths};

    fn setup() -> (tempfile::TempDir, Paths, PathBuf, UvBinary, ManagedPython) {
        let dir = tempfile::tempdir().unwrap();
        let module = dir.path().join("module");
        fs::create_dir_all(&module).unwrap();
        fs::write(module.join("pyproject.toml"), b"[project]\nname='x'\n").unwrap();
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
        (dir, p, module, uv, python)
    }

    fn manifest(text: &str) -> ModuleManifestV2 {
        serde_json::from_str(text).unwrap()
    }

    const PYPROJECT: &str = include_str!("../../../../contracts/examples/omninexus.v2.json");

    #[test]
    fn pyproject_dispatches_compile_with_hashes_and_output_file() {
        let (_dir, paths, module, uv, python) = setup();
        let runner = MockUvRunner::default();
        let resolved = resolve(
            &paths,
            "mod@a",
            &manifest(PYPROJECT),
            &module,
            &uv,
            &python,
            &EnvOptions::default(),
            &runner,
        )
        .unwrap();
        let call = &runner.calls()[0];
        assert!(call.args.contains(&"compile".to_string()));
        assert!(call.args.contains(&"--generate-hashes".to_string()));
        assert!(call.args.contains(&"--python".to_string()));
        assert!(call.args.iter().any(|a| a.ends_with(".lock.new")));
        assert_eq!(call.env["UV_MANAGED_PYTHON"], "1");
        assert!(call.env.contains_key("UV_PYTHON"));
        assert_eq!(call.cwd, module);
        assert!(resolved.staging_path.exists());
        assert!(!resolved.lock_path.exists());
    }

    #[test]
    fn uv_lock_does_not_compile() {
        let (_dir, paths, module, uv, python) = setup();
        fs::write(module.join("uv.lock"), b"version = 1\n").unwrap();
        let m = manifest(&PYPROJECT.replace(
            "\"kind\": \"pyproject\", \"path\": \"pyproject.toml\"",
            "\"kind\": \"uv_lock\", \"path\": \"uv.lock\"",
        ));
        let runner = MockUvRunner::default();
        let resolved = resolve(
            &paths,
            "mod@a",
            &m,
            &module,
            &uv,
            &python,
            &EnvOptions::default(),
            &runner,
        )
        .unwrap();
        assert!(runner.calls().is_empty());
        assert_eq!(
            fs::read_to_string(&resolved.staging_path).unwrap(),
            "version = 1\n"
        );
    }

    #[test]
    fn uv_lock_rejects_non_root_lock_path() {
        let (_dir, paths, module, uv, python) = setup();
        fs::create_dir_all(module.join("locks")).unwrap();
        fs::write(module.join("locks/prod.lock"), b"version = 1\n").unwrap();
        let m = manifest(&PYPROJECT.replace(
            "\"kind\": \"pyproject\", \"path\": \"pyproject.toml\"",
            "\"kind\": \"uv_lock\", \"path\": \"locks/prod.lock\"",
        ));
        let runner = MockUvRunner::default();
        let e = resolve(
            &paths,
            "mod@a",
            &m,
            &module,
            &uv,
            &python,
            &EnvOptions::default(),
            &runner,
        )
        .unwrap_err();
        assert_eq!(e.kind, EnvErrorKind::DepsResolve);
        assert!(e.message.contains("uv.lock"));
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn missing_or_traversing_declaration_is_rejected() {
        let (_dir, paths, module, uv, python) = setup();
        let runner = MockUvRunner::default();
        let bad = manifest(
            &PYPROJECT.replace("\"path\": \"pyproject.toml\"", "\"path\": \"missing.txt\""),
        );
        assert_eq!(
            resolve(
                &paths,
                "mod@a",
                &bad,
                &module,
                &uv,
                &python,
                &EnvOptions::default(),
                &runner
            )
            .unwrap_err()
            .kind,
            EnvErrorKind::DepsResolve
        );
        let evil = manifest(
            &PYPROJECT.replace("\"path\": \"pyproject.toml\"", "\"path\": \"../evil.txt\""),
        );
        assert_eq!(
            resolve(
                &paths,
                "mod@a",
                &evil,
                &module,
                &uv,
                &python,
                &EnvOptions::default(),
                &runner
            )
            .unwrap_err()
            .kind,
            EnvErrorKind::DepsResolve
        );
    }

    #[test]
    fn extras_are_passed_explicitly() {
        let (_dir, paths, module, uv, python) = setup();
        let m = manifest(&PYPROJECT.replace("\"extras\": []", "\"extras\": [\"keyring\"]"));
        let runner = MockUvRunner::default();
        resolve(
            &paths,
            "mod@a",
            &m,
            &module,
            &uv,
            &python,
            &EnvOptions::default(),
            &runner,
        )
        .unwrap();
        let args = &runner.calls()[0].args;
        assert!(args.contains(&"--extra".to_string()));
        assert!(args.contains(&"keyring".to_string()));
    }

    #[test]
    fn commit_writes_metadata_and_verify_detects_tampering() {
        let (_dir, paths, module, uv, python) = setup();
        let runner = MockUvRunner::default();
        let mut resolved = resolve(
            &paths,
            "mod@a",
            &manifest(PYPROJECT),
            &module,
            &uv,
            &python,
            &EnvOptions::default(),
            &runner,
        )
        .unwrap();
        commit(&paths, &mut resolved).unwrap();
        assert!(!resolved.staging_path.exists());
        let meta: serde_json::Value = state::read_json(&paths.deps_meta_path("mod@a")).unwrap();
        assert_eq!(meta["schema_version"], 1);
        assert_eq!(meta["offline_capable"], true);
        assert_eq!(meta["kind"], "pyproject");
        assert_eq!(
            verify_snapshot(&paths, "mod@a", &resolved.sha256).unwrap(),
            resolved.sha256
        );
        fs::write(resolved.lock_path.clone(), b"tampered").unwrap();
        assert_eq!(
            verify_snapshot(&paths, "mod@a", &resolved.sha256)
                .unwrap_err()
                .kind,
            EnvErrorKind::SnapshotMismatch
        );
        assert_eq!(fs::read_to_string(&resolved.lock_path).unwrap(), "tampered");
    }
}
