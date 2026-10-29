//! 厂商密钥注入的集成测试。
//!
//! 重点验证三件事：
//! 1. 保存的密钥会进入模块的 `entry.env`（这是注入能否生效的关键环节）；
//! 2. **子进程真的拿到了**这个变量（端到端，用 `cmd /c set` 落盘核对）；
//! 3. 总览数据里**不含密钥值**（隐私约束，结构上就不该有）。

use std::path::{Path, PathBuf};

use serde_json::json;

use dounai_launcher::catalog::Allowlist;
use dounai_launcher::commands::{
    add_local_module_inner, clear_vendor_key_inner, set_vendor_key_inner, vendor_overview_inner,
    AppState, ClearVendorKeyRequest, SetVendorKeyRequest,
};
use dounai_launcher::trust::TrustPolicy;
use dounai_launcher::vendor_keys::VendorStore;

fn tmp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "dounai-vendor-it-{}-{}-{}",
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

fn state(root: &Path) -> AppState {
    AppState::with_trust(
        root.join("appdata"),
        Allowlist::new(Vec::<String>::new()),
        TrustPolicy::permissive(),
    )
    .unwrap()
}

/// 登记一个本地模块（走本地登记通道，需先开启 dev_mode）。
fn register(state: &AppState, root: &Path, module_id: &str, command: &str) {
    let dir = root.join("ext").join(module_id);
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = json!({
        "module_id": module_id,
        "name": module_id,
        "version": "0.1.0",
        "category": "core",
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
    std::fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();

    if !state.dev_mode() {
        state.enable_dev_mode();
    }
    add_local_module_inner(state, dir.to_str().unwrap()).unwrap();
}

#[test]
fn saved_key_is_injected_into_entry_env() {
    let root = tmp_root("inject");
    let st = state(&root);
    register(&st, &root, "moodvoice", "cmd /c timeout /t 5");

    set_vendor_key_inner(
        &st,
        &SetVendorKeyRequest {
            module_id: "moodvoice".to_string(),
            env_name: "OPENAI_API_KEY".to_string(),
            value: "sk-integration-test".to_string(),
        },
    )
    .unwrap();

    let spec = st.entry_spec_for_test("moodvoice").unwrap();
    let got = spec
        .env
        .iter()
        .find(|(k, _)| k == "OPENAI_API_KEY")
        .map(|(_, v)| v.clone());
    assert_eq!(got.as_deref(), Some("sk-integration-test"));

    // 清除后不应再注入
    clear_vendor_key_inner(
        &st,
        &ClearVendorKeyRequest {
            module_id: "moodvoice".to_string(),
            env_name: "OPENAI_API_KEY".to_string(),
        },
    )
    .unwrap();
    let spec2 = st.entry_spec_for_test("moodvoice").unwrap();
    assert!(
        !spec2.env.iter().any(|(k, _)| k == "OPENAI_API_KEY"),
        "清除后不应再注入"
    );

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn manifest_env_does_not_override_saved_key() {
    let root = tmp_root("precedence");
    let st = state(&root);
    let dir = root.join("ext").join("omninexus");
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = json!({
        "module_id": "omninexus",
        "name": "omninexus",
        "version": "0.1.0",
        "category": "core",
        "entry": {
            "type": "command",
            "command": "cmd /c timeout /t 5",
            "working_dir": "",
            // manifest 里写了一个占位值，启动器保存的密钥应胜出
            "env": { "OPENAI_API_KEY": "from-manifest" },
            "health": { "url": null, "timeout_ms": 500 }
        },
        "dependencies": [],
        "model_packs": [],
        "signature": { "algo": "SHA-256 + Ed25519", "value": "" }
    });
    std::fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
    st.enable_dev_mode();
    add_local_module_inner(&st, dir.to_str().unwrap()).unwrap();

    set_vendor_key_inner(
        &st,
        &SetVendorKeyRequest {
            module_id: "omninexus".to_string(),
            env_name: "OPENAI_API_KEY".to_string(),
            value: "from-launcher".to_string(),
        },
    )
    .unwrap();

    let spec = st.entry_spec_for_test("omninexus").unwrap();
    let vals: Vec<&String> = spec
        .env
        .iter()
        .filter(|(k, _)| k == "OPENAI_API_KEY")
        .map(|(_, v)| v)
        .collect();
    assert_eq!(vals.len(), 1, "同名变量只能出现一次，实际 {:?}", vals);
    assert_eq!(vals[0], "from-launcher", "启动器保存的密钥优先");

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn overview_never_contains_secret_values() {
    let root = tmp_root("noleak");
    let st = state(&root);
    register(&st, &root, "dounai-input", "cmd /c timeout /t 5");

    let secret = "sk-should-never-appear-1234567890";
    set_vendor_key_inner(
        &st,
        &SetVendorKeyRequest {
            module_id: "dounai-input".to_string(),
            env_name: "DASHSCOPE_API_KEY".to_string(),
            value: secret.to_string(),
        },
    )
    .unwrap();

    // 总览会尝试连本机端口；这里模块未运行，必定走 error 分支，但结构里没有值字段
    let views = vendor_overview_inner(&st);
    let json = serde_json::to_string(&views).unwrap();
    assert!(!json.contains(secret), "总览序列化后不得包含密钥值：{json}");
    // 但应当能看出"这层有已保存的密钥"
    let input_layer = views
        .iter()
        .find(|v| v.module_id == "dounai-input")
        .unwrap();
    assert!(input_layer.installed);
    assert!(!input_layer.running);
    assert!(input_layer.error.is_some(), "未运行时应给出原因");

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn key_reaches_child_process_env() {
    // 端到端：走真实启动路径（start_module_inner → ProcessManager），
    // 让子进程把变量值写进文件，再核对内容。
    //
    // 注意：启动器**禁止入口命令包含 shell 元字符**（`>` 也在内），
    // 所以重定向不能写在命令里，得放进一个 .cmd 脚本由它承载。
    if !cfg!(windows) {
        return;
    }
    use dounai_launcher::commands::start_module_inner;

    let root = tmp_root("child");
    let st = state(&root);
    let out = root.join("envdump.txt");
    let script = root.join("check.cmd");
    // %VAR% 在 cmd 里展开：注入没到子进程的话，这里会写成字面量
    std::fs::write(
        &script,
        format!(
            "@echo off\r\necho %OPENAI_API_KEY%> \"{}\"\r\n",
            out.display()
        ),
    )
    .unwrap();

    let cmd = format!("cmd /c {}", script.display());
    register(&st, &root, "moodvoice", &cmd);

    set_vendor_key_inner(
        &st,
        &SetVendorKeyRequest {
            module_id: "moodvoice".to_string(),
            env_name: "OPENAI_API_KEY".to_string(),
            value: "sk-child-check-777".to_string(),
        },
    )
    .unwrap();

    let _view = start_module_inner(&st, "moodvoice").expect("模块应能启动");

    // 等子进程写完（最多 3 秒）
    let mut content = String::new();
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Ok(t) = std::fs::read_to_string(&out) {
            if !t.trim().is_empty() {
                content = t;
                break;
            }
        }
    }
    st.processes().stop("moodvoice");

    assert!(
        content.contains("sk-child-check-777"),
        "子进程环境里应能看到注入的密钥，实际读到：{:?}",
        content
    );

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn store_file_lives_under_data_root() {
    let root = tmp_root("path");
    let st = state(&root);
    set_vendor_key_inner(
        &st,
        &SetVendorKeyRequest {
            module_id: "omninexus".to_string(),
            env_name: "ANTHROPIC_API_KEY".to_string(),
            value: "sk-ant-xyz".to_string(),
        },
    )
    .unwrap();

    let file = root.join("appdata").join("vendor-keys.json");
    assert!(file.exists(), "密钥库应落在数据根下：{}", file.display());

    // 重新打开能读到（落盘正确）
    let reopened = VendorStore::open(&root.join("appdata")).unwrap();
    assert!(reopened.has("omninexus", "ANTHROPIC_API_KEY"));

    std::fs::remove_dir_all(&root).ok();
}
