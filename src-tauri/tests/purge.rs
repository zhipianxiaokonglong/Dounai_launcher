//! 数据清除能力测试。
//!
//! 关键约束：只清本层数据（缓存/状态/模块安装目录），
//! 不触碰上游的对话、记忆、密钥——那些不在本层的数据根内。

use dounai_launcher::catalog::Allowlist;
use dounai_launcher::commands::{AppState, PurgeRequest};
use dounai_launcher::state::Category;

use std::path::PathBuf;

fn tmp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "dounai-purge-{}-{}-{}",
        name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn seed(root: &std::path::Path) {
    // 缓存
    std::fs::write(root.join("catalog-cache.json"), b"{}").unwrap();
    // 状态
    std::fs::write(root.join("modules-state.json"), b"{\"modules\":{}}").unwrap();
    // 两个模块目录
    let m1 = root.join("modules").join("mod-a");
    let m2 = root.join("modules").join("mod-b");
    std::fs::create_dir_all(&m1).unwrap();
    std::fs::create_dir_all(&m2).unwrap();
    std::fs::write(m1.join("f.txt"), b"a").unwrap();
    std::fs::write(m2.join("f.txt"), b"b").unwrap();
    // 模拟"上游数据"：不在本层数据根内，绝不能被清除
    let upstream = root.parent().unwrap().join("upstream-data");
    std::fs::create_dir_all(&upstream).ok();
    std::fs::write(upstream.join("chat.db"), b"user conversation").ok();
}

#[test]
fn purge_caches_only() {
    let root = tmp_root("caches");
    seed(&root);
    let st = AppState::new(root.clone(), Allowlist::default()).unwrap();

    let resp = purge_local_data_direct(
        &st,
        PurgeRequest {
            caches: true,
            state: false,
            module_files: false,
        },
    );

    assert!(resp
        .removed_caches
        .contains(&"catalog-cache.json".to_string()));
    assert!(!root.join("catalog-cache.json").exists());
    // 状态与模块目录应保留
    assert!(root.join("modules-state.json").exists());
    assert!(root.join("modules").join("mod-a").exists());
    assert!(resp.errors.is_empty());
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn purge_state_only() {
    let root = tmp_root("state");
    seed(&root);
    let st = AppState::new(root.clone(), Allowlist::default()).unwrap();

    let resp = purge_local_data_direct(
        &st,
        PurgeRequest {
            caches: false,
            state: true,
            module_files: false,
        },
    );

    assert!(resp.state_cleared);
    assert!(!root.join("modules-state.json").exists());
    // 缓存与模块文件保留
    assert!(root.join("catalog-cache.json").exists());
    assert!(root.join("modules").join("mod-a").exists());
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn purge_module_files_only() {
    let root = tmp_root("files");
    seed(&root);
    let st = AppState::new(root.clone(), Allowlist::default()).unwrap();

    let resp = purge_local_data_direct(
        &st,
        PurgeRequest {
            caches: false,
            state: false,
            module_files: true,
        },
    );

    assert_eq!(resp.removed_module_dirs.len(), 2);
    assert!(!root.join("modules").join("mod-a").exists());
    assert!(!root.join("modules").join("mod-b").exists());
    // 状态文件保留（用户可能想只删文件保留记录）
    assert!(root.join("modules-state.json").exists());
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn purge_everything() {
    let root = tmp_root("all");
    seed(&root);
    let upstream = root.parent().unwrap().join("upstream-data");
    std::fs::write(upstream.join("chat.db"), b"user conversation").ok();

    let st = AppState::new(root.clone(), Allowlist::default()).unwrap();
    let resp = purge_local_data_direct(
        &st,
        PurgeRequest {
            caches: true,
            state: true,
            module_files: true,
        },
    );

    assert!(resp.state_cleared);
    assert_eq!(resp.removed_module_dirs.len(), 2);
    assert!(!root.join("catalog-cache.json").exists());
    assert!(!root.join("modules-state.json").exists());

    // 最重要的一条：上游数据必须完好
    assert!(
        upstream.join("chat.db").exists(),
        "本层清除不得影响上游的用户数据"
    );
    assert_eq!(
        std::fs::read(upstream.join("chat.db")).unwrap(),
        b"user conversation"
    );

    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&upstream).ok();
}

#[test]
fn purge_when_nothing_exists_is_safe() {
    let root = tmp_root("empty");
    let st = AppState::new(root.clone(), Allowlist::default()).unwrap();
    let resp = purge_local_data_direct(
        &st,
        PurgeRequest {
            caches: true,
            state: true,
            module_files: true,
        },
    );
    // 不存在的项不应产生错误
    assert!(
        resp.errors.is_empty(),
        "空目录清除不应报错：{:?}",
        resp.errors
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn purge_does_not_touch_modules_dir_entries_outside() {
    let root = tmp_root("outside");
    seed(&root);
    // 在数据根外放一个同名文件，确认清除操作不越界
    let outside = root.parent().unwrap().join("outside-marker.txt");
    std::fs::write(&outside, b"keep me").ok();

    let st = AppState::new(root.clone(), Allowlist::default()).unwrap();
    purge_local_data_direct(
        &st,
        PurgeRequest {
            caches: true,
            state: true,
            module_files: true,
        },
    );

    assert!(outside.exists(), "清除操作不应影响数据根外的文件");
    std::fs::remove_file(&outside).ok();
    std::fs::remove_dir_all(&root).ok();
}

/// 直接调用 purge 逻辑（绕过 Tauri State 包装）。
fn purge_local_data_direct(
    state: &AppState,
    request: PurgeRequest,
) -> dounai_launcher::commands::PurgeResponse {
    dounai_launcher::commands::purge_local_data_inner(state, request)
}

#[allow(dead_code)]
fn _keep_category_used(_: Category) {}
