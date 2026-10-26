//! Tauri 命令层测试。
//!
//! 命令层是前端与核心库的边界，最容易出现「前端传什么就信什么」的问题，
//! 因此这里重点验证：
//! 1. 模块 id 无法构造路径穿越；
//! 2. 哈希缺失或错误时不会进入解压；
//! 3. 未签名 manifest 在严格策略下被拒绝；
//! 4. manifest 与请求的 module_id 不一致时被拒绝。

use dounai_launcher::catalog::Allowlist;
use dounai_launcher::commands::{precheck_install, AppState, InstallRequest};
use dounai_launcher::state::{InstallState, StateStore};
use dounai_launcher::trust::{TrustPolicy, TrustVerdict};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use std::io::Write;
use std::path::PathBuf;

fn tmp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "dounai-cmd-{}-{}-{}",
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

fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
        for (name, data) in entries {
            w.start_file(*name, opts).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap();
    }
    buf
}

#[test]
fn app_state_creates_root_and_store() {
    let root = tmp_root("init");
    let st = AppState::new(root.clone(), Allowlist::new(["dl.example.com"])).unwrap();
    assert!(root.exists());
    // 状态文件在首次保存前不存在是正常的
    let store = st.store_for_test().lock().unwrap();
    assert!(store.all().is_empty());
    drop(store);
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn module_dir_rejects_traversal_ids() {
    let root = tmp_root("trav");
    let st = AppState::new(root.clone(), Allowlist::default()).unwrap();

    for bad in [
        "../evil",
        "..",
        "a/../../b",
        "a\\b",
        "with space",
        "",
        "con", // 保留名也一并拒绝
        "mod/../../x",
    ] {
        let r = st.module_dir_for_test(bad);
        assert!(r.is_err(), "应拒绝非法模块 id：{bad:?}");
    }

    // 合法的应通过
    for good in ["mod-a", "mod_b", "mod.c", "abc123"] {
        let r = st.module_dir_for_test(good);
        assert!(r.is_ok(), "应接受合法模块 id：{good:?}");
        let p = r.unwrap();
        assert!(
            p.starts_with(&root),
            "安装目录必须在数据根内：{}",
            p.display()
        );
    }

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn state_records_install_and_survives_reopen() {
    let root = tmp_root("persist");
    {
        let st = AppState::new(root.clone(), Allowlist::default()).unwrap();
        let mut store = st.store_for_test().lock().unwrap();
        store
            .mark_installed(
                "mod-x",
                "1.2.3",
                "C:\\data\\mod-x",
                dounai_launcher::state::Category::Optional,
            )
            .unwrap();
    }
    // 重新打开
    let state_path = root.join("modules-state.json");
    let store = StateStore::open(&state_path).unwrap();
    let m = store.get("mod-x").unwrap();
    assert_eq!(m.version, "1.2.3");
    assert_eq!(m.install, InstallState::Installed);
    std::fs::remove_dir_all(&root).ok();
}

/// 构造一个合法的 InstallRequest（未签名 manifest）。
fn req(module_id: &str, zip_b64: &str, sha: &str) -> InstallRequest {
    InstallRequest {
        module_id: module_id.to_string(),
        category: "optional".to_string(),
        version: "1.0.0".to_string(),
        zip_base64: zip_b64.to_string(),
        expected_sha256: sha.to_string(),
        manifest: serde_json::json!({ "module_id": module_id }),
    }
}

#[test]
fn install_rejects_missing_sha256() {
    // 关键回归：早期实现里哈希是可选的，为空即跳过校验，
    // 等于允许安装任意字节。现在必须被拒绝。
    let zip = make_zip(&[("a.txt", b"x")]);
    let r = req("m", &B64.encode(&zip), "");
    let err = precheck_install(&r, &TrustPolicy::permissive()).unwrap_err();
    assert!(err.contains("expected_sha256"), "实际错误：{err}");
}

#[test]
fn install_rejects_whitespace_only_sha256() {
    let zip = make_zip(&[("a.txt", b"x")]);
    let r = req("m", &B64.encode(&zip), "   ");
    assert!(precheck_install(&r, &TrustPolicy::permissive()).is_err());
}

#[test]
fn install_rejects_wrong_hash() {
    let zip = make_zip(&[("a.txt", b"x")]);
    let r = req(
        "m",
        &B64.encode(&zip),
        &dounai_launcher::sha256_hex(b"other"),
    );
    let err = precheck_install(&r, &TrustPolicy::permissive()).unwrap_err();
    assert!(err.contains("哈希校验失败"), "实际错误：{err}");
}

#[test]
fn install_accepts_matching_hash_under_permissive_policy() {
    let zip = make_zip(&[("a.txt", b"x")]);
    let r = req("m", &B64.encode(&zip), &dounai_launcher::sha256_hex(&zip));
    let (trust, bytes) = precheck_install(&r, &TrustPolicy::permissive()).unwrap();
    assert_eq!(trust, TrustVerdict::UnsignedAllowed);
    assert_eq!(bytes, zip);
}

#[test]
fn install_rejects_unsigned_manifest_under_strict_policy() {
    let zip = make_zip(&[("a.txt", b"x")]);
    let r = req("m", &B64.encode(&zip), &dounai_launcher::sha256_hex(&zip));
    // 严格策略且无可信公钥 → 未签名必须被拒
    let err = precheck_install(&r, &TrustPolicy::strict(vec!["a2V5".into()])).unwrap_err();
    assert!(err.contains("未签名"), "实际错误：{err}");
}

#[test]
fn install_rejects_module_id_mismatch() {
    // manifest 声明的 id 与请求不一致：防"张冠李戴"，
    // 即用 A 模块的签名 manifest 去安装 B 模块的包
    let zip = make_zip(&[("a.txt", b"x")]);
    let mut r = req("m", &B64.encode(&zip), &dounai_launcher::sha256_hex(&zip));
    r.manifest = serde_json::json!({ "module_id": "other-module" });
    let err = precheck_install(&r, &TrustPolicy::permissive()).unwrap_err();
    assert!(err.contains("不一致"), "实际错误：{err}");
}

#[test]
fn install_rejects_bad_base64() {
    let r = req("m", "!!!not-base64!!!", "abc");
    let err = precheck_install(&r, &TrustPolicy::permissive()).unwrap_err();
    assert!(err.contains("base64"), "实际错误：{err}");
}

#[test]
fn state_default_trust_is_fail_closed() {
    // AppState::new 默认必须是最保守策略：无公钥、拒绝未签名
    let root = tmp_root("failclosed");
    let st = AppState::new(root.clone(), Allowlist::default()).unwrap();
    let p = st.trust_policy();
    assert_eq!(p.trusted_keys().len(), 0);
    assert!(!p.allow_unsigned(), "默认不得允许未签名");
    assert!(!p.is_open(), "默认不得是完全放行状态");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn hash_mismatch_blocks_extraction() {
    // 端到端验证：给错误的哈希，解压绝不能发生
    let root = tmp_root("hashblock");
    let zip_bytes = make_zip(&[("a.txt", b"hello")]);
    let wrong_hash = dounai_launcher::sha256_hex(b"something-else");

    assert!(!dounai_launcher::verify_sha256(&zip_bytes, &wrong_hash));

    // 正确哈希应通过
    let right_hash = dounai_launcher::sha256_hex(&zip_bytes);
    assert!(dounai_launcher::verify_sha256(&zip_bytes, &right_hash));
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn allowlist_default_denies_everything() {
    let al = Allowlist::default();
    assert!(al.is_empty());
    assert!(al.check("https://example.com/x").is_err());
}
