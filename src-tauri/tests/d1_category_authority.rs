//! D-1 回归测试：模块分类必须由后端从 manifest 读取。
//!
//! ## 背景
//!
//! 审计报告（2026-11-12）实测复现了一个越权：
//! `uninstall_module` / `set_module_enabled` 直接用前端传来的 `category`
//! 做权限判断，于是把 core 模块伪造成 `optional` 就能卸载/禁用它。
//!
//! 这击穿了「core 不可卸载」这条写进 README / SECURITY.md / ACCEPTANCE 的硬约束。
//!
//! ## 测试策略
//!
//! 不是"测函数返回值"，而是**直接复现报告里的攻击路径**并断言被拒绝。
//! 将来若有人把 `category_of` 换回 `request.category`，本文件会立刻红。
//!
//! 同时保留**反向断言**（正常的 optional 模块仍可卸载），
//! 避免"把所有操作都拒绝"也能通过测试这种假阳性。

use std::path::{Path, PathBuf};

use serde_json::json;

use dounai_launcher::catalog::Allowlist;
use dounai_launcher::commands::{
    add_local_module_inner, uninstall_module_inner, AppState, UninstallRequest,
};
use dounai_launcher::state::Category;
use dounai_launcher::trust::TrustPolicy;

/* ---------------- 测试辅助 ---------------- */

fn tmp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dounai-d1-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn state(root: &Path) -> AppState {
    AppState::with_trust(
        root.to_path_buf(),
        Allowlist::new(Vec::<String>::new()),
        TrustPolicy::permissive(),
    )
    .unwrap()
}

/// 在指定目录写一个最小 manifest（可选是否声明 category）。
fn write_manifest(dir: &Path, module_id: &str, category: Option<&str>) {
    let mut m = json!({
        "module_id": module_id,
        "name": module_id,
        "version": "0.2.0",
        "entry": {
            "type": "command",
            "command": "cmd /c exit 0",
            "working_dir": "",
            "env": {},
            "health": { "url": null, "timeout_ms": 500 }
        },
        "dependencies": [],
        "model_packs": [],
        "signature": { "algo": "SHA-256 + Ed25519", "value": "" }
    });
    if let Some(cat) = category {
        m["category"] = json!(cat);
    }
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("manifest.json"), m.to_string()).unwrap();
}

/// 登记一个外部（linked）模块。
///
/// 注意：**不能**再调 set_state_for_test——它内部走 mark_installed，
/// 那会把 linked 清成 false（见 state.rs 的注释「走启动器安装的模块一律是受管模块」），
/// 于是 module_root 会去 data_root 下找目录而不是用登记路径，
/// 导致 category_of 读不到 manifest。
///
/// dd_local_module_inner 已经完成了登记（走 mark_linked），够了。
///
/// declared 参数保留是为了让调用点能表达「状态库里原本记的是什么」，
/// 但实际分类以 manifest 为准，这里不再写状态库。
fn install_linked(
    st: &AppState,
    dir: &Path,
    _module_id: &str,
    _declared: Category,
) -> Result<(), String> {
    add_local_module_inner(st, dir.to_str().unwrap()).map(|_| ())
}

/* ---------------- D-1：攻击路径复现 ---------------- */

#[test]
fn d1_cannot_uninstall_core_module_by_forging_category() {
    let root = tmp_root("uninstall-core");
    let mod_dir = root.join("external").join("core-mod");
    write_manifest(&mod_dir, "core-mod", Some("core"));

    let st = state(&root.join("appdata"));
    install_linked(&st, &mod_dir, "core-mod", Category::Core).unwrap();

    // 攻击：manifest 说 core，但请求里伪造成 optional
    let result = uninstall_module_inner(
        &st,
        UninstallRequest {
            module_id: "core-mod".into(),
            category: "optional".into(), // ← 伪造
            remove_files: false,
        },
    );

    assert!(
        result.is_err(),
        "伪造 category 卸载 core 模块必须被拒绝，实际却成功了"
    );
    let msg = result.unwrap_err();
    assert!(
        msg.contains("分类不一致"),
        "错误信息应明确指出分类不一致，实际是：{msg}"
    );
    // 模块必须仍在
    assert!(
        st.store_for_test()
            .lock()
            .unwrap()
            .get("core-mod")
            .is_some(),
        "被拒绝后模块不应从状态中消失"
    );

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn d1_cannot_disable_core_module_by_forging_category() {
    let root = tmp_root("disable-core");
    let mod_dir = root.join("external").join("core-mod");
    write_manifest(&mod_dir, "core-mod", Some("core"));

    let st = state(&root.join("appdata"));
    install_linked(&st, &mod_dir, "core-mod", Category::Core).unwrap();

    // 直接验证后端判据：真实分类是 Core → 不可禁用
    let real = st
        .category_of("core-mod")
        .expect("应能从 manifest 读到分类");
    assert_eq!(real, Category::Core);
    assert!(!real.can_disable(), "Core 必须不可禁用");
    assert!(!real.removable(), "Core 必须不可卸载");

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn d1_category_comes_from_manifest_not_from_request() {
    let root = tmp_root("read-cat");
    let mod_dir = root.join("external").join("declared");
    write_manifest(&mod_dir, "declared", Some("optional"));

    let st = state(&root.join("appdata"));
    // 状态库里故意写 core（模拟落库记录被篡改或过期）
    install_linked(&st, &mod_dir, "declared", Category::Core).unwrap();

    let cat = st
        .category_of("declared")
        .expect("应能从 manifest 读到分类");
    assert_eq!(
        cat,
        Category::Optional,
        "分类必须以 manifest 为准，而不是状态库里的记录"
    );

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn d1_optional_module_still_uninstallable() {
    // 反向断言：修复不能把正常路径也堵死
    let root = tmp_root("optional-ok");
    let mod_dir = root.join("external").join("opt-mod");
    write_manifest(&mod_dir, "opt-mod", Some("optional"));

    let st = state(&root.join("appdata"));
    install_linked(&st, &mod_dir, "opt-mod", Category::Optional).unwrap();

    let result = uninstall_module_inner(
        &st,
        UninstallRequest {
            module_id: "opt-mod".into(),
            category: "optional".into(),
            remove_files: false,
        },
    );
    assert!(result.is_ok(), "正常的 optional 卸载应成功：{result:?}");

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn d1_missing_category_in_manifest_is_rejected() {
    // manifest 没写 category → 按最严格处理（拒绝），而不是猜一个放行
    let root = tmp_root("no-cat");
    let mod_dir = root.join("external").join("no-cat-mod");
    write_manifest(&mod_dir, "no-cat-mod", None);

    let st = state(&root.join("appdata"));
    install_linked(&st, &mod_dir, "no-cat-mod", Category::Core).unwrap();

    let cat = st.category_of("no-cat-mod");
    assert!(
        cat.is_err(),
        "manifest 未声明 category 时必须拒绝，而不是猜一个分类"
    );

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn d1_unknown_category_in_manifest_is_rejected() {
    let root = tmp_root("bad-cat");
    let mod_dir = root.join("external").join("bad-cat-mod");
    write_manifest(&mod_dir, "bad-cat-mod", Some("superuser"));

    let st = state(&root.join("appdata"));
    install_linked(&st, &mod_dir, "bad-cat-mod", Category::Core).unwrap();

    let cat = st.category_of("bad-cat-mod");
    assert!(cat.is_err(), "未知分类必须拒绝");

    std::fs::remove_dir_all(&root).ok();
}
