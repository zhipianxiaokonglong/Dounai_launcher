//! M1 环境引擎的集成断言：源码目录不被污染、失败清理、真 uv 夜间验收。
//!
//! `#[ignore]` 项需要 `DOUNAI_TEST_UV` 指向 uv 0.13.7，并允许联网下载托管 Python 与 wheels。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use launcher_core::env::{
    prepare_instance_with, verify_instance_with, CurlUvFetcher, EnvContext, EnvError, EnvErrorKind,
    EnvOptions, MockConfig, MockFailure, MockUvRunner, SystemUvRunner, UvFetcher, UV_VERSION,
};
use launcher_core::manifest::ModuleManifestV2;
use launcher_core::{file_sha256_prefixed, InstallMode, InstallState, Paths};

const ID: &str = "dounai/omninexus@stable@main";
const MANIFEST: &str = include_str!("../../../contracts/examples/omninexus.v2.json");

struct NoDownload;

impl UvFetcher for NoDownload {
    fn fetch(&self, _url: &str) -> Result<Vec<u8>, EnvError> {
        Err(EnvError::new(
            EnvErrorKind::UvMissing,
            "",
            "mock 测试不联网",
        ))
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    paths: Paths,
    install: InstallState,
    module: PathBuf,
    runner: MockUvRunner,
    fetcher: NoDownload,
}

impl Fixture {
    fn new(_manifest_text: &str, runner: MockUvRunner) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let install_root = dir.path().join("app");
        let data_root = dir.path().join("data");
        let module = dir.path().join("module");
        fs::create_dir_all(install_root.join("bin")).unwrap();
        fs::create_dir_all(&data_root).unwrap();
        fs::create_dir_all(module.join("src").join("omninexus")).unwrap();
        fs::write(
            module.join("pyproject.toml"),
            b"[project]\nname = \"m1-fixture\"\nversion = \"0.1.0\"\nrequires-python = \">=3.11\"\ndependencies = []\n",
        )
        .unwrap();
        fs::write(module.join("uv.lock"), b"version = 1\n").unwrap();
        fs::write(
            module.join("src").join("omninexus").join("__init__.py"),
            b"",
        )
        .unwrap();
        let exe = b"uv binary";
        fs::write(install_root.join("bin").join("uv.exe"), exe).unwrap();
        let paths = Paths::new(
            InstallMode::CurrentUser,
            install_root.clone(),
            data_root.clone(),
        )
        .unwrap();
        let install = InstallState::initialize(
            &paths,
            paths.bundled_uv_path(),
            format!("sha256:{}", launcher_core::security::sha256_hex(exe)),
            UV_VERSION.into(),
        )
        .unwrap();
        Fixture {
            _dir: dir,
            paths,
            install,
            module,
            runner,
            fetcher: NoDownload,
        }
    }

    fn ctx(&self) -> EnvContext<'_> {
        EnvContext {
            install: &self.install,
            options: EnvOptions {
                offline: false,
                allow_uv_download: false,
                port: Some(18102),
            },
            runner: &self.runner,
            fetcher: &self.fetcher,
        }
    }

    fn prepare(&self, manifest: &ModuleManifestV2) -> Result<launcher_core::PreparedEnv, EnvError> {
        prepare_instance_with(&self.paths, ID, manifest, &self.module, &self.ctx())
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git 应可执行");
    assert!(
        out.status.success(),
        "git {:?} 失败：{}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn init_git_repo(dir: &Path) {
    git(dir, &["init", "-q"]);
    git(dir, &["config", "user.email", "m1@example.invalid"]);
    git(dir, &["config", "user.name", "M1 Test"]);
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "init"]);
}

fn git_status(dir: &Path) -> String {
    git(dir, &["status", "--porcelain"])
}

fn markers(dir: &Path) -> Vec<bool> {
    [".venv", "uv.lock", "__pycache__", "requirements.lock"]
        .iter()
        .map(|name| dir.join(name).exists())
        .collect()
}

#[test]
fn mock_prepare_leaves_upstream_git_status_clean() {
    let f = Fixture::new(MANIFEST, MockUvRunner::default());
    init_git_repo(&f.module);
    let before = git_status(&f.module);
    let before_markers = markers(&f.module);
    let manifest: ModuleManifestV2 = serde_json::from_str(MANIFEST).unwrap();
    let env = f.prepare(&manifest).unwrap();
    assert_eq!(git_status(&f.module), before);
    assert_eq!(markers(&f.module), before_markers);
    assert!(env.venv_python.exists());
    assert!(verify_instance_with(&f.paths, ID, &f.install).is_ok());
}

#[test]
fn uv_lock_branch_keeps_source_clean_and_uses_frozen() {
    let text = MANIFEST.replace(
        "\"kind\": \"pyproject\", \"path\": \"pyproject.toml\"",
        "\"kind\": \"uv_lock\", \"path\": \"uv.lock\"",
    );
    let f = Fixture::new(&text, MockUvRunner::default());
    init_git_repo(&f.module);
    let before = git_status(&f.module);
    let before_markers = markers(&f.module);
    let manifest: ModuleManifestV2 = serde_json::from_str(&text).unwrap();
    f.prepare(&manifest).unwrap();
    assert_eq!(
        git_status(&f.module),
        before,
        "uv sync 不得在源码目录生成 uv.lock"
    );
    assert_eq!(markers(&f.module), before_markers);
    let calls = f.runner.calls();
    assert!(calls
        .iter()
        .any(|c| c.args.contains(&"--frozen".to_string())));
    assert!(calls.iter().any(|c| c.cwd == f.module));
}

#[test]
fn failed_resolve_keeps_previous_snapshot_and_venv() {
    let f = Fixture::new(MANIFEST, MockUvRunner::default());
    let manifest: ModuleManifestV2 = serde_json::from_str(MANIFEST).unwrap();
    f.prepare(&manifest).unwrap();
    let previous = fs::read(f.paths.deps_lock_path(ID)).unwrap();

    let failing = MockUvRunner::new(MockConfig {
        fail: Some(MockFailure {
            when_contains: "pip compile".into(),
            kind: EnvErrorKind::DepsResolve,
            message: "包名写错".into(),
        }),
        ..MockConfig::default()
    });
    let ctx = EnvContext {
        install: &f.install,
        options: EnvOptions {
            offline: false,
            allow_uv_download: false,
            port: None,
        },
        runner: &failing,
        fetcher: &NoDownload,
    };
    let e = prepare_instance_with(&f.paths, ID, &manifest, &f.module, &ctx).unwrap_err();
    assert_eq!(e.kind, EnvErrorKind::DepsResolve);
    assert_eq!(fs::read(f.paths.deps_lock_path(ID)).unwrap(), previous);
    assert!(
        f.paths.instance_venv_python(ID).exists(),
        "上次成功的 venv 不得被清理"
    );
    assert!(verify_instance_with(&f.paths, ID, &f.install).is_ok());
}

#[test]
#[ignore = "needs uv 0.13.7 in DOUNAI_TEST_UV and network; run via env-nightly workflow"]
fn real_uv_prepares_venv_without_touching_source() {
    let uv = PathBuf::from(std::env::var("DOUNAI_TEST_UV").expect("DOUNAI_TEST_UV 未设置"));
    assert!(uv.exists(), "uv.exe 不存在：{}", uv.display());
    let f = Fixture::new(MANIFEST, MockUvRunner::default());
    fs::copy(&uv, f.paths.bundled_uv_path()).unwrap();
    let mut install = f.install.clone();
    install.uv_sha256 = file_sha256_prefixed(&f.paths.bundled_uv_path()).unwrap();
    install.overwrite_for_tests().unwrap();
    init_git_repo(&f.module);
    let before = git_status(&f.module);
    let manifest: ModuleManifestV2 = serde_json::from_str(MANIFEST).unwrap();
    let runner = SystemUvRunner::new(f.paths.install_root.clone());
    let ctx = EnvContext {
        install: &install,
        options: EnvOptions {
            offline: false,
            allow_uv_download: true,
            port: Some(18102),
        },
        runner: &runner,
        fetcher: &CurlUvFetcher,
    };
    let env = prepare_instance_with(&f.paths, ID, &manifest, &f.module, &ctx)
        .expect("真 uv 应能建好 venv");
    assert!(env.venv_python.exists());
    assert_eq!(git_status(&f.module), before);
    assert!(verify_instance_with(&f.paths, ID, &f.install).is_ok());
    let _ = file_sha256_prefixed(&f.paths.deps_lock_path(ID)).unwrap();
}
