//! 依赖来源按 manifest 声明的 `kind` 分派，启动器不嗅探文件，也不按优先级猜测。
//!
//! 解析结果先写 `.lock.new`。venv 与离线校验都成功后，锁与元数据按内容哈希写进
//! 各自的 generation 文件，写入后永不改写。`runtime.json` 是唯一的提交点：它
//! 引用的 generation 必须先落盘，所以任何一步崩溃都不会留下「锁新、哈希旧」。

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

/// 内容冲突说明状态被改写，不是依赖解析失败。其余 I/O 错误仍归 `DepsResolve`。
fn state_write_err(instance_id: &str, context: &str, e: state::StateError) -> EnvError {
    if matches!(e, state::StateError::Conflict { .. }) {
        EnvError::new(
            EnvErrorKind::SnapshotMismatch,
            instance_id,
            format!("{context}：{e}"),
        )
    } else {
        err(instance_id, format!("{context}：{e}"))
    }
}

/// 清掉「文件名是完整哈希、内容却是半截」的残损 generation 文件。
///
/// 旧实现在写入中途崩溃会留下这种文件，重跑时因内容不符会永久报冲突。只在
/// `commit` 前执行：此时临时快照已复核为 `sha`，哈希不符的锁不可能是合法内容。
/// 只删哈希不符的锁与无法解析的元数据；权限等读取错误保留，交由上层报错。
fn discard_corrupt_generation_files(
    paths: &Paths,
    instance_id: &str,
    sha: &str,
) -> Result<(), EnvError> {
    let lock = paths.content_lock_path(instance_id, sha);
    if lock.exists() {
        let actual = state::file_sha256_prefixed(&lock)
            .map_err(|e| err(instance_id, format!("复核已有 generation 锁失败：{e}")))?;
        if actual != sha {
            state::remove_if_exists(&lock)
                .map_err(|e| err(instance_id, format!("清理残损 generation 锁失败：{e}")))?;
        }
    }
    let meta = paths.content_meta_path(instance_id, sha);
    if meta.exists()
        && matches!(
            state::read_json::<DepsMetadata>(&meta),
            Err(state::StateError::Json { .. })
        )
    {
        state::remove_if_exists(&meta)
            .map_err(|e| err(instance_id, format!("清理残损 generation 元数据失败：{e}")))?;
    }
    Ok(())
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
    paths.deps_staging_path(instance_id)
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
        lock_path: paths.content_lock_path(instance_id, &sha256),
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

/// 把临时快照固化成不可变 generation。只有 `runtime.json` 切换后才算提交成功。
///
/// 同一 generation 重复写是幂等的：已存在且内容一致直接复用，内容不同视为篡改。
pub fn commit(paths: &Paths, resolved: &mut ResolvedDeps) -> Result<(), EnvError> {
    let id = &resolved.metadata.instance_id;
    let bytes =
        fs::read(&resolved.staging_path).map_err(|e| err(id, format!("读取临时快照失败：{e}")))?;
    let sha = state::file_sha256_prefixed(&resolved.staging_path)
        .map_err(|e| err(id, format!("复核临时快照失败：{e}")))?;
    if sha != resolved.sha256 {
        return Err(EnvError::new(
            EnvErrorKind::SnapshotMismatch,
            id,
            format!(
                "临时快照在提交前被改动：期望 {}，实际 {sha}",
                resolved.sha256
            ),
        ));
    }
    discard_corrupt_generation_files(paths, id, &sha)?;
    state::write_bytes_once(&paths.content_lock_path(id, &sha), &bytes)
        .map_err(|e| state_write_err(id, "写 generation 锁失败", e))?;
    resolved.metadata.offline_capable = true;
    let meta_path = paths.content_meta_path(id, &sha);
    if meta_path.exists() {
        // 同一 generation 重复提交时保留首次生成时间。其余字段必须逐项一致，
        // 否则视为本地状态被改写，不静默覆盖。
        let existing: DepsMetadata = state::read_json(&meta_path).map_err(|e| {
            EnvError::new(
                EnvErrorKind::SnapshotMismatch,
                id,
                format!("读取已有 generation 元数据失败：{e}"),
            )
        })?;
        let mut expected = resolved.metadata.clone();
        expected.generated_at = existing.generated_at.clone();
        if existing != expected {
            return Err(EnvError::new(
                EnvErrorKind::SnapshotMismatch,
                id,
                "同一 generation 的元数据与已有文件不一致",
            ));
        }
    } else {
        let mut meta = serde_json::to_vec_pretty(&resolved.metadata)
            .map_err(|e| err(id, format!("序列化依赖元数据失败：{e}")))?;
        meta.push(b'\n');
        state::write_bytes_once(&meta_path, &meta)
            .map_err(|e| state_write_err(id, "写 generation 元数据失败", e))?;
    }
    state::remove_if_exists(&resolved.staging_path)
        .map_err(|e| err(id, format!("清理临时快照失败：{e}")))?;
    Ok(())
}

/// 按 `runtime.json` 里的哈希定位不可变锁，并复核文件内容确实等于该哈希。
/// 篡改任一字节都会报 `SnapshotMismatch`，绝不覆盖，也不静默重建。
pub fn verify_snapshot(
    paths: &Paths,
    instance_id: &str,
    expected: &str,
) -> Result<String, EnvError> {
    let lock = paths.content_lock_path(instance_id, expected);
    let actual = state::file_sha256_prefixed(&lock).map_err(|e| {
        EnvError::new(
            EnvErrorKind::SnapshotMismatch,
            instance_id,
            format!("找不到或读不了 generation 锁 {}：{e}", lock.display()),
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

/// 元数据与锁同属一个 generation，单独落盘并按哈希取名，不参与一致性判定之外
/// 的任何写入。读不到或哈希对不上都按 `SnapshotMismatch` 处理。
pub fn verify_metadata(
    paths: &Paths,
    instance_id: &str,
    expected: &str,
) -> Result<DepsMetadata, EnvError> {
    let path = paths.content_meta_path(instance_id, expected);
    let meta: DepsMetadata = state::read_json(&path).map_err(|e| {
        EnvError::new(
            EnvErrorKind::SnapshotMismatch,
            instance_id,
            format!("读取 generation 元数据 {} 失败：{e}", path.display()),
        )
    })?;
    if meta.snapshot_sha256 != expected || meta.instance_id != instance_id {
        return Err(EnvError::new(
            EnvErrorKind::SnapshotMismatch,
            instance_id,
            "依赖元数据与 generation 哈希或实例 ID 不一致",
        ));
    }
    Ok(meta)
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
        let meta: serde_json::Value =
            state::read_json(&paths.content_meta_path("mod@a", &resolved.sha256)).unwrap();
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

    #[test]
    fn commit_repairs_truncated_generation_files() {
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
        let lock = paths.content_lock_path("mod@a", &resolved.sha256);
        let meta = paths.content_meta_path("mod@a", &resolved.sha256);
        fs::create_dir_all(lock.parent().unwrap()).unwrap();
        // 模拟写入中途崩溃：文件名是完整哈希，内容只有半截。
        fs::write(&lock, b"half").unwrap();
        fs::write(&meta, b"{ broken").unwrap();

        commit(&paths, &mut resolved).unwrap();
        assert_eq!(
            verify_snapshot(&paths, "mod@a", &resolved.sha256).unwrap(),
            resolved.sha256
        );
        let stored: DepsMetadata = state::read_json(&meta).unwrap();
        assert_eq!(stored.snapshot_sha256, resolved.sha256);
    }

    #[test]
    fn commit_rejects_metadata_from_another_generation() {
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
        let meta = paths.content_meta_path("mod@a", &resolved.sha256);
        fs::create_dir_all(meta.parent().unwrap()).unwrap();
        let mut wrong = resolved.metadata.clone();
        wrong.snapshot_sha256 = format!("sha256:{}", "b".repeat(64));
        wrong.offline_capable = true;
        fs::write(&meta, serde_json::to_vec_pretty(&wrong).unwrap()).unwrap();

        assert_eq!(
            commit(&paths, &mut resolved).unwrap_err().kind,
            EnvErrorKind::SnapshotMismatch
        );
    }
}
