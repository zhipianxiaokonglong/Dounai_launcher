//! 厂商密钥投递测试（按新架构：启动器不存密钥，只写模块 .env）。
//!
//! 架构原则（用户确认）：
//! - 启动器与 Dounai 本体都**不存储**密钥；
//! - 密钥归属各模块：模型层自定义厂商走 AES-GCM+SQLite（API），
//!   其余层读各自模块根目录的 `.env`；
//! - 启动器只负责"帮用户填写"与"读回模块现状"。
//!
//! 因此本文件验证的是：
//! 1. 写入后密钥确实出现在模块 .env 里（模块能读到）；
//! 2. 启动器**没有**在自己的数据目录留下任何密钥副本；
//! 3. 写入不会破坏该文件里的其它配置与注释；
//! 4. overview 不返回密钥值。

use std::path::{Path, PathBuf};

use serde_json::json;

use dounai_launcher::catalog::Allowlist;
use dounai_launcher::commands::{
    add_local_module_inner, clear_vendor_key_inner, set_vendor_key_inner, vendor_overview_inner,
    AppState, ClearVendorKeyRequest, SetVendorKeyRequest,
};
use dounai_launcher::trust::TrustPolicy;

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

/// 登记一个本地模块，返回它的模块根。
fn register(state: &AppState, root: &Path, module_id: &str) -> PathBuf {
    let dir = root.join("ext").join(module_id);
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = json!({
        "module_id": module_id,
        "name": module_id,
        "version": "0.1.0",
        "category": "core",
        "entry": {
            "type": "command",
            "command": "cmd /c timeout /t 5",
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
    dir
}

#[test]
fn key_is_written_into_module_env_file() {
    let root = tmp_root("envwrite");
    let st = state(&root);
    let module_dir = register(&st, &root, "moodvoice");

    set_vendor_key_inner(
        &st,
        &SetVendorKeyRequest {
            module_id: "moodvoice".to_string(),
            env_name: "OPENAI_API_KEY".to_string(),
            value: "sk-written-to-env".to_string(),
        },
    )
    .unwrap();

    // 关键：密钥必须出现在**模块自己**的 .env 里
    let env_path = module_dir.join(".env");
    assert!(env_path.exists(), ".env 应被创建：{}", env_path.display());
    let text = std::fs::read_to_string(&env_path).unwrap();
    assert!(
        text.contains("OPENAI_API_KEY=sk-written-to-env"),
        "实际内容：{text}"
    );

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn launcher_keeps_no_key_copy_in_its_data_dir() {
    // 架构原则的硬断言：启动器数据目录里不得出现密钥文件
    let root = tmp_root("nocopy");
    let st = state(&root);
    let module_dir = register(&st, &root, "omninexus");

    let secret = "sk-must-not-be-stored-by-launcher-123";
    set_vendor_key_inner(
        &st,
        &SetVendorKeyRequest {
            module_id: "omninexus".to_string(),
            env_name: "ANTHROPIC_API_KEY".to_string(),
            value: secret.to_string(),
        },
    )
    .unwrap();

    // 1) 模块 .env 里有它
    let module_env = std::fs::read_to_string(module_dir.join(".env")).unwrap();
    assert!(module_env.contains(secret));

    // 2) 启动器数据根里**任何文件**都不该含它
    let data_root = root.join("appdata");
    for entry in walkdir::WalkDir::new(&data_root).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(entry.path()) {
            assert!(
                !content.contains(secret),
                "启动器在 {:?} 里留下了密钥副本",
                entry.path()
            );
        }
    }

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn writing_preserves_existing_env_content() {
    let root = tmp_root("preserve");
    let st = state(&root);
    let module_dir = register(&st, &root, "dounai-input");

    // 用户事先手写过 .env
    std::fs::write(
        module_dir.join(".env"),
        "# 我自己的配置\nDASHSCOPE_API_KEY=sk-handwritten\nDOUNAI_PORT=8101\n",
    )
    .unwrap();

    set_vendor_key_inner(
        &st,
        &SetVendorKeyRequest {
            module_id: "dounai-input".to_string(),
            env_name: "OPENAI_API_KEY".to_string(),
            value: "sk-added".to_string(),
        },
    )
    .unwrap();

    let text = std::fs::read_to_string(module_dir.join(".env")).unwrap();
    assert!(
        text.contains("DASHSCOPE_API_KEY=sk-handwritten"),
        "原有密钥不能丢"
    );
    assert!(text.contains("DOUNAI_PORT=8101"), "无关配置不能丢");
    assert!(text.contains("# 我自己的配置"), "注释不能丢");
    assert!(text.contains("OPENAI_API_KEY=sk-added"), "新密钥应写入");

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn clear_removes_key_from_env_but_keeps_others() {
    let root = tmp_root("clear");
    let st = state(&root);
    let module_dir = register(&st, &root, "moodvoice");

    set_vendor_key_inner(
        &st,
        &SetVendorKeyRequest {
            module_id: "moodvoice".to_string(),
            env_name: "OPENAI_API_KEY".to_string(),
            value: "sk-temp".to_string(),
        },
    )
    .unwrap();
    set_vendor_key_inner(
        &st,
        &SetVendorKeyRequest {
            module_id: "moodvoice".to_string(),
            env_name: "AZURE_SPEECH_KEY".to_string(),
            value: "az-temp".to_string(),
        },
    )
    .unwrap();

    clear_vendor_key_inner(
        &st,
        &ClearVendorKeyRequest {
            module_id: "moodvoice".to_string(),
            env_name: "OPENAI_API_KEY".to_string(),
        },
    )
    .unwrap();

    let text = std::fs::read_to_string(module_dir.join(".env")).unwrap();
    assert!(!text.contains("OPENAI_API_KEY"), "被清除的变量不应残留");
    assert!(
        text.contains("AZURE_SPEECH_KEY=az-temp"),
        "其它变量必须保留"
    );

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn overview_never_contains_secret_values() {
    let root = tmp_root("noleak");
    let st = state(&root);
    register(&st, &root, "dounai-input");

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

    let views = vendor_overview_inner(&st);
    let json = serde_json::to_string(&views).unwrap();
    assert!(!json.contains(secret), "总览序列化后不得包含密钥值");
    assert!(json.contains("encrypted_store"), "字段结构应保持");
    assert!(
        json.contains("\"shared_keys\":[]"),
        "共享区已移除，应为空数组"
    );

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn writing_to_uninstalled_module_fails_clearly() {
    let root = tmp_root("notinstalled");
    let st = state(&root);

    let err = set_vendor_key_inner(
        &st,
        &SetVendorKeyRequest {
            module_id: "moodvoice".to_string(),
            env_name: "OPENAI_API_KEY".to_string(),
            value: "sk-x".to_string(),
        },
    )
    .unwrap_err();

    // 未登记模块的根目录不存在，应给出可读错误而不是静默成功
    assert!(
        err.contains("模块") || err.contains("目录"),
        "错误信息应指向模块/目录问题，实际：{err}"
    );

    std::fs::remove_dir_all(&root).ok();
}
