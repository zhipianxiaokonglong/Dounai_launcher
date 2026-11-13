//! 随包 uv 的端到端自举。
//!
//! 这条覆盖 M1 收口的两件事，且都走生产代码：
//! 1. `AppStateV2::new` 在 `install.json` 缺失时补写，写出的 uv 三元组与内置常量一致；
//! 2. `locate_uv` 从程序根 `bin/uv.exe` 命中文件、哈希对得上、`uv --version` 真跑通。
//!
//! 程序根用仓库里的 `src-tauri`（它下面就有 `bin/uv.exe`），数据根用临时目录，
//! 所以不会碰用户真实数据。仓库里没有该文件时整条跳过——它由发布流程保证存在。

use std::path::{Path, PathBuf};

use dounai_launcher::state_v2::AppStateV2;
use launcher_core::env::{
    locate_uv, EnvError, EnvOptions, SystemUvRunner, UvFetcher, UV_SHA256, UV_VERSION,
};
use launcher_core::install::InstallState;

/// 只在真的需要下载时被调用；本测试走随包路径，它一旦被调用就说明走错了。
struct NoFetch;

impl UvFetcher for NoFetch {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, EnvError> {
        panic!("不该联网补齐 uv：{url}");
    }
}

fn bundled_binary() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("bin").join("uv.exe")
}

#[test]
fn bootstraps_install_state_and_runs_the_bundled_uv() {
    let bundled = bundled_binary();
    if !bundled.exists() {
        eprintln!("跳过：仓库里没有 bin/uv.exe");
        return;
    }
    let install_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let data_dir = tempfile::tempdir().unwrap();
    let data_root = data_dir.path().join("data");

    let state = AppStateV2::new(install_root, data_root).unwrap();
    let state_path = state.paths.install_state_path();
    assert!(state_path.exists(), "启动时必须补写 install.json");
    assert!(
        state.install_note.is_some(),
        "首次补写要留下说明，设置页据此提示"
    );

    let install = InstallState::load(&state.paths).unwrap();
    assert_eq!(install.uv_path, state.paths.bundled_uv_path());
    assert_eq!(install.uv_sha256, UV_SHA256);
    assert_eq!(install.uv_version, UV_VERSION);

    // 不允许联网：能跑通就证明用的是随包文件，不是下载回来的。
    let options = EnvOptions {
        offline: true,
        allow_uv_download: false,
        ..EnvOptions::default()
    };
    let runner = SystemUvRunner::new(state.paths.install_root.clone());
    let uv = locate_uv(
        &state.paths,
        &install,
        &options,
        &runner,
        &NoFetch,
        "test/bootstrap",
    )
    .unwrap();

    assert_eq!(uv.path, state.paths.bundled_uv_path());
    assert_eq!(uv.version, UV_VERSION);
    assert_eq!(uv.sha256, UV_SHA256);
    assert_eq!(uv.downloaded_bytes, 0, "随包命中时不该有下载字节");
}

#[test]
fn a_deleted_install_json_is_recreated_on_the_next_boot() {
    let install_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let data_dir = tempfile::tempdir().unwrap();
    let data_root = data_dir.path().join("data");

    let first = AppStateV2::new(install_root.clone(), data_root.clone()).unwrap();
    let state_path = first.paths.install_state_path();
    std::fs::remove_file(&state_path).unwrap();

    let second = AppStateV2::new(install_root, data_root).unwrap();
    assert!(state_path.exists(), "删掉后重启必须自愈");
    assert_eq!(
        InstallState::load(&second.paths).unwrap().uv_sha256,
        UV_SHA256
    );
}

#[test]
fn a_tampered_install_json_is_backed_up_before_being_rewritten() {
    let install_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let data_dir = tempfile::tempdir().unwrap();
    let data_root = data_dir.path().join("data");

    let first = AppStateV2::new(install_root.clone(), data_root.clone()).unwrap();
    let state_path = first.paths.install_state_path();
    // 手改成非法 JSON：这是唯一能让人为「读不出来」的情况。
    std::fs::write(&state_path, b"{ not json").unwrap();

    let second = AppStateV2::new(install_root, data_root).unwrap();
    let backup = state_path.with_extension("json.bak");
    assert!(backup.exists(), "改写前必须留备份");
    assert_eq!(std::fs::read(&backup).unwrap(), b"{ not json");
    assert!(second.install_note.unwrap().contains("不可用"));
    assert_eq!(
        InstallState::load(&second.paths).unwrap().uv_sha256,
        UV_SHA256
    );
}
