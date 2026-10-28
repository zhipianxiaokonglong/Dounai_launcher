//! 本地模块登记（手动添加）测试。
//!
//! 这类模块的文件在用户自己的目录里，启动器只登记路径、不拥有文件。
//! 因此测试重点不是"装进去"，而是：
//! 1. 能正确识别含 manifest 的目录；
//! 2. 登记后能真的启动（用的是登记路径，不是 data_root 下的空目录）；
//! 3. **卸载不删用户文件**——这是最容易写错、后果最严重的一条；
//! 4. 没有 manifest 时给出可读提示（用户常把模型权重当成模块）。

use std::path::{Path, PathBuf};

use serde_json::json;

use dounai_launcher::catalog::Allowlist;
use dounai_launcher::commands::{
    add_local_module_inner, remove_local_module_inner, scan_local_modules_inner,
    start_module_inner, uninstall_module_inner, AppState, UninstallRequest,
};
use dounai_launcher::state::InstallState;
use dounai_launcher::trust::TrustPolicy;

fn tmp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "dounai-local-{}-{}-{}",
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

fn state(root: &Path, trust: TrustPolicy) -> AppState {
    AppState::with_trust(
        root.to_path_buf(),
        Allowlist::new(Vec::<String>::new()),
        trust,
    )
    .unwrap()
}

fn long_running_cmd(seconds: u32) -> String {
    if cfg!(windows) {
        format!("cmd /c timeout /t {seconds}")
    } else {
        format!("sleep {seconds}")
    }
}

/// 在指定目录写一个最小 manifest。
fn write_manifest(dir: &Path, module_id: &str, command: &str, category: &str) {
    let m = json!({
        "module_id": module_id,
        "name": module_id,
        "version": "0.2.0",
        "category": category,
        "entry": {
            "type": "command",
            "command": command,
            "working_dir": "",
            "env": {},
            "health": { "url": null, "timeout_ms": 500 }
        },
        "dependencies": [],
        "model_packs": [],
        "signature": { "algo": "SHA-256 + Ed25519", "value": "" }
    });
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("manifest.json"), m.to_string()).unwrap();
}

/* ---------------- 扫描 ---------------- */

#[test]
fn scan_finds_manifest_dirs_one_level_deep() {
    let root = tmp_root("scan");
    let modules = root.join("my-modules");
    write_manifest(&modules.join("repo-a"), "mod-a", "echo a", "core");
    write_manifest(&modules.join("repo-b"), "mod-b", "echo b", "optional");
    // 一个没有 manifest 的目录（模拟模型权重目录）
    std::fs::create_dir_all(modules.join("weights")).unwrap();
    std::fs::write(modules.join("weights/model.gguf"), b"fake").unwrap();

    let st = state(&root.join("appdata"), TrustPolicy::permissive());
    let found = scan_local_modules_inner(&st, modules.to_str().unwrap()).unwrap();

    let ids: Vec<&str> = found.iter().map(|c| c.module_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["mod-a", "mod-b"],
        "应识别两个模块，且忽略权重目录"
    );
    assert!(found.iter().all(|c| !c.registered), "应是未登记状态");

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn scan_rejects_missing_directory() {
    let root = tmp_root("scan-missing");
    let st = state(&root.join("appdata"), TrustPolicy::permissive());
    let err = scan_local_modules_inner(&st, "Z:\\definitely\\not\\here").unwrap_err();
    assert!(err.contains("不存在"), "实际：{err}");
    std::fs::remove_dir_all(&root).ok();
}

/* ---------------- 添加 ---------------- */

#[test]
fn add_registers_linked_module_and_marks_it_installed() {
    let root = tmp_root("add");
    let mod_dir = root.join("external").join("my-mod");
    write_manifest(&mod_dir, "my-mod", &long_running_cmd(20), "core");

    let st = state(&root.join("appdata"), TrustPolicy::permissive());
    let view = add_local_module_inner(&st, mod_dir.to_str().unwrap()).unwrap();

    assert_eq!(view.module_id, "my-mod");
    assert_eq!(view.install, InstallState::Installed);
    assert!(view.linked, "应标记为外部链接模块");
    assert_eq!(view.version, "0.2.0");

    // 路径必须是规范化后的绝对路径
    let stored = st.store_for_test().lock().unwrap();
    let p = &stored.get("my-mod").unwrap().path;
    assert!(Path::new(p).is_absolute(), "应存绝对路径，实际 {p}");

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn add_rejects_directory_without_manifest() {
    let root = tmp_root("nomanifest");
    let dir = root.join("weights-only");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("model.gguf"), b"fake").unwrap();

    let st = state(&root.join("appdata"), TrustPolicy::permissive());
    let err = add_local_module_inner(&st, dir.to_str().unwrap()).unwrap_err();

    // 用户常把模型权重误当模块，提示必须点明这一点
    assert!(err.contains("manifest.json"), "实际：{err}");
    assert!(err.contains("模型权重"), "应提示这是模型而非模块：{err}");

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn add_rejects_unsigned_under_strict_policy() {
    let root = tmp_root("strict");
    let mod_dir = root.join("external").join("un-signed");
    write_manifest(&mod_dir, "un-signed", "echo hi", "optional");

    // 严格策略 + 无可信公钥 → 未签名必须被拒
    let st = state(
        &root.join("appdata"),
        TrustPolicy::strict(vec!["Zm9v".into()]),
    );
    let err = add_local_module_inner(&st, mod_dir.to_str().unwrap()).unwrap_err();
    assert!(err.contains("未签名"), "实际：{err}");

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn add_rejects_empty_path() {
    let root = tmp_root("emptypath");
    let st = state(&root.join("appdata"), TrustPolicy::permissive());
    assert!(add_local_module_inner(&st, "   ").is_err());
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn add_rejects_managed_module_takeover() {
    let root = tmp_root("takeover");
    let st = state(&root.join("appdata"), TrustPolicy::permissive());

    // 先模拟一个由启动器安装的受管模块
    st.set_state_for_test(
        "managed",
        "1.0.0",
        dounai_launcher::state::Category::Optional,
        None,
    )
    .unwrap();

    let mod_dir = root.join("external").join("managed");
    write_manifest(&mod_dir, "managed", "echo hi", "optional");

    let err = add_local_module_inner(&st, mod_dir.to_str().unwrap()).unwrap_err();
    assert!(err.contains("已由启动器安装"), "实际：{err}");

    std::fs::remove_dir_all(&root).ok();
}

/* ---------------- 登记后能真的启动 ---------------- */

#[test]
fn linked_module_can_start_using_registered_path() {
    let root = tmp_root("start");
    let mod_dir = root.join("external").join("runnable");
    write_manifest(&mod_dir, "runnable", &long_running_cmd(20), "core");

    let st = state(&root.join("appdata"), TrustPolicy::permissive());
    add_local_module_inner(&st, mod_dir.to_str().unwrap()).unwrap();

    // 关键：module_root 必须指向登记路径，而不是 data_root 下的空目录
    let resolved = st.module_root("runnable").unwrap();
    assert_eq!(
        resolved.canonicalize().unwrap(),
        mod_dir.canonicalize().unwrap(),
        "模块根应是登记路径"
    );

    let v = start_module_inner(&st, "runnable").expect("链接模块应能启动");
    assert_eq!(v.run, "running");
    assert!(v.pid.is_some());

    st.processes().stop("runnable");
    std::fs::remove_dir_all(&root).ok();
}

/* ---------------- 卸载不删用户文件（最重要） ---------------- */

#[test]
fn uninstall_linked_module_keeps_user_files() {
    let root = tmp_root("keepfiles");
    let mod_dir = root.join("external").join("user-repo");
    write_manifest(&mod_dir, "user-repo", "echo hi", "optional");
    // 用户自己的源码
    std::fs::write(mod_dir.join("main.py"), b"print('my code')").unwrap();

    let st = state(&root.join("appdata"), TrustPolicy::permissive());
    add_local_module_inner(&st, mod_dir.to_str().unwrap()).unwrap();

    // 用「卸载」路径，并显式要求删文件
    uninstall_module_inner(
        &st,
        UninstallRequest {
            module_id: "user-repo".into(),
            category: "optional".into(),
            remove_files: true,
        },
    )
    .expect("卸载应成功");

    // 关键断言：用户的目录与文件必须原封不动
    assert!(mod_dir.exists(), "链接模块卸载不得删除用户目录");
    assert!(
        mod_dir.join("main.py").exists(),
        "链接模块卸载不得删除用户文件"
    );
    assert!(
        mod_dir.join("manifest.json").exists(),
        "manifest 也不该被删"
    );

    // 登记已解除
    assert!(st
        .store_for_test()
        .lock()
        .unwrap()
        .get("user-repo")
        .is_none());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn remove_local_module_unregisters_without_deleting() {
    let root = tmp_root("unregister");
    let mod_dir = root.join("external").join("m");
    write_manifest(&mod_dir, "m", "echo hi", "optional");

    let st = state(&root.join("appdata"), TrustPolicy::permissive());
    add_local_module_inner(&st, mod_dir.to_str().unwrap()).unwrap();

    remove_local_module_inner(&st, "m").unwrap();

    assert!(mod_dir.exists(), "解除登记不应删文件");
    assert!(st.store_for_test().lock().unwrap().get("m").is_none());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn remove_local_module_rejects_managed_module() {
    let root = tmp_root("reject-managed");
    let st = state(&root.join("appdata"), TrustPolicy::permissive());
    st.set_state_for_test(
        "managed",
        "1.0.0",
        dounai_launcher::state::Category::Optional,
        None,
    )
    .unwrap();

    let err = remove_local_module_inner(&st, "managed").unwrap_err();
    assert!(err.contains("由启动器安装"), "实际：{err}");

    std::fs::remove_dir_all(&root).ok();
}

/* ---------------- 清除数据不碰链接模块 ---------------- */

#[test]
fn purge_does_not_touch_linked_module_directory() {
    use dounai_launcher::commands::{purge_local_data_inner, PurgeRequest};

    let root = tmp_root("purge");
    let mod_dir = root.join("external").join("keep-me");
    write_manifest(&mod_dir, "keep-me", "echo hi", "optional");
    std::fs::write(mod_dir.join("source.py"), b"mine").unwrap();

    let st = state(&root.join("appdata"), TrustPolicy::permissive());
    add_local_module_inner(&st, mod_dir.to_str().unwrap()).unwrap();

    // 连同模块文件一起清除
    let resp = purge_local_data_inner(
        &st,
        PurgeRequest {
            caches: true,
            state: true,
            module_files: true,
        },
    );
    assert!(resp.errors.is_empty(), "不应有错误：{:?}", resp.errors);

    // 关键：purge 只清 data_root/modules，链接目录必须完好
    assert!(mod_dir.exists(), "清除数据不得删除链接模块目录");
    assert!(
        mod_dir.join("source.py").exists(),
        "清除数据不得删除链接模块里的文件"
    );

    std::fs::remove_dir_all(&root).ok();
}

/* ---------------- 链接模块仍受路径约束 ---------------- */

#[test]
fn linked_module_working_dir_still_confined() {
    use dounai_launcher::runtime::{check_working_dir, StartError};
    // working_dir 的边界是模块根；对链接模块就是用户目录，
    // 但依然不许越出这个根
    let root = Path::new("D:\\external\\my-mod");
    assert!(check_working_dir(root, "sub").is_ok());
    assert!(matches!(
        check_working_dir(root, "..\\..\\Windows"),
        Err(StartError::UnsafeWorkingDir(_))
    ));
}
