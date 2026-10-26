//! 安装链路端到端测试（真实文件系统 + 真实 zip）。
//!
//! 此前只测了各组件（precheck / transaction / extract），
//! 但"命令层把组件串起来"这段没被测到——组件都对、拼起来错的情况是存在的。
//! 这里调用 `install_module_inner` 走完整条链：
//! 校验 → 标记 Installing → 解压 → 原子提交 → 记录状态 → 返回信任判定。

use std::io::Write;
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde_json::json;

use dounai_launcher::catalog::Allowlist;
use dounai_launcher::commands::{install_module_inner, AppState, InstallRequest};
use dounai_launcher::state::{InstallState, StateStore};
use dounai_launcher::trust::{TrustPolicy, TrustVerdict};

fn tmp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "dounai-e2e-{}-{}-{}",
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

fn permissive_state(root: &Path) -> AppState {
    AppState::with_trust(
        root.to_path_buf(),
        Allowlist::new(Vec::<String>::new()),
        TrustPolicy::permissive(),
    )
    .unwrap()
}

#[test]
fn full_install_chain_lands_files_and_records_state() {
    let root = tmp_root("chain");
    let state = permissive_state(&root);

    let zip = make_zip(&[
        ("dounai_input/__init__.py", b"# pkg"),
        ("dounai_input/server.py", b"print('hi')"),
        ("requirements.txt", b"fastapi"),
    ]);
    let hash = dounai_launcher::sha256_hex(&zip);

    let resp = install_module_inner(
        &state,
        InstallRequest {
            module_id: "dounai-input".into(),
            category: "core".into(),
            version: "0.1.0".into(),
            zip_base64: B64.encode(&zip),
            expected_sha256: hash,
            manifest: json!({ "module_id": "dounai-input" }),
        },
    )
    .expect("安装应成功");

    assert_eq!(resp.files, 3);
    assert!(!resp.replaced);
    assert_eq!(resp.trust, TrustVerdict::UnsignedAllowed);

    // 文件真实落盘在数据根内
    let installed = root.join("modules").join("dounai-input");
    assert!(installed.join("dounai_input/__init__.py").exists());
    assert!(installed.join("requirements.txt").exists());
    assert_eq!(
        std::fs::read(installed.join("dounai_input/server.py")).unwrap(),
        b"print('hi')"
    );

    // 状态被记录
    let store = StateStore::open(&root.join("modules-state.json")).unwrap();
    let st = store.get("dounai-input").unwrap();
    assert_eq!(st.install, InstallState::Installed);
    assert_eq!(st.version, "0.1.0");
    assert!(st.enabled, "core 装完应默认启用");

    // 无临时目录残留
    let leftovers: Vec<String> = std::fs::read_dir(root.join("modules"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains("staging") || n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "不应残留：{leftovers:?}");

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn failed_install_leaves_no_files_and_marks_broken() {
    let root = tmp_root("fail");
    let state = permissive_state(&root);

    let zip = make_zip(&[("a.txt", b"data")]);
    // 故意给错哈希
    let wrong = dounai_launcher::sha256_hex(b"other");

    let err = install_module_inner(
        &state,
        InstallRequest {
            module_id: "mod-bad".into(),
            category: "optional".into(),
            version: "1.0.0".into(),
            zip_base64: B64.encode(&zip),
            expected_sha256: wrong,
            manifest: json!({ "module_id": "mod-bad" }),
        },
    )
    .unwrap_err();
    assert!(err.contains("哈希校验失败"), "实际：{err}");

    // 关键：校验在 mark_installing 之前失败，不应留下 Broken 状态，
    // 也不应创建任何目录
    assert!(!root.join("modules").join("mod-bad").exists());
    let store = StateStore::open(&root.join("modules-state.json")).unwrap();
    assert!(
        store.get("mod-bad").is_none(),
        "校验阶段失败不应写入任何状态"
    );

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn strict_policy_blocks_unsigned_install_end_to_end() {
    let root = tmp_root("strict");
    let state = AppState::with_trust(
        root.clone(),
        Allowlist::new(Vec::<String>::new()),
        TrustPolicy::strict(vec!["Zm9v".into()]),
    )
    .unwrap();

    let zip = make_zip(&[("a.txt", b"data")]);
    let hash = dounai_launcher::sha256_hex(&zip);

    let err = install_module_inner(
        &state,
        InstallRequest {
            module_id: "mod-x".into(),
            category: "optional".into(),
            version: "1".into(),
            zip_base64: B64.encode(&zip),
            expected_sha256: hash,
            manifest: json!({ "module_id": "mod-x" }),
        },
    )
    .unwrap_err();
    assert!(err.contains("未签名"), "实际：{err}");
    assert!(!root.join("modules").join("mod-x").exists());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn reinstall_replaces_previous_version() {
    let root = tmp_root("upgrade");
    let state = permissive_state(&root);

    let v1 = make_zip(&[("version.txt", b"1.0"), ("old.txt", b"legacy")]);
    let h1 = dounai_launcher::sha256_hex(&v1);
    let r1 = install_module_inner(
        &state,
        InstallRequest {
            module_id: "m".into(),
            category: "optional".into(),
            version: "1.0".into(),
            zip_base64: B64.encode(&v1),
            expected_sha256: h1,
            manifest: json!({ "module_id": "m" }),
        },
    )
    .unwrap();
    assert!(!r1.replaced);

    let v2 = make_zip(&[("version.txt", b"2.0"), ("new.txt", b"fresh")]);
    let h2 = dounai_launcher::sha256_hex(&v2);
    let r2 = install_module_inner(
        &state,
        InstallRequest {
            module_id: "m".into(),
            category: "optional".into(),
            version: "2.0".into(),
            zip_base64: B64.encode(&v2),
            expected_sha256: h2,
            manifest: json!({ "module_id": "m" }),
        },
    )
    .unwrap();
    assert!(r2.replaced, "重装应报告 replaced");

    let dir = root.join("modules").join("m");
    assert_eq!(std::fs::read(dir.join("version.txt")).unwrap(), b"2.0");
    assert!(dir.join("new.txt").exists());
    assert!(!dir.join("old.txt").exists(), "旧版本文件不应残留");

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn malicious_zip_cannot_escape_via_command_layer() {
    let root = tmp_root("escape");
    let state = permissive_state(&root);

    let zip = make_zip(&[("ok.txt", b"fine"), ("../../escaped.txt", b"evil")]);
    let hash = dounai_launcher::sha256_hex(&zip);

    let err = install_module_inner(
        &state,
        InstallRequest {
            module_id: "evil".into(),
            category: "optional".into(),
            version: "1".into(),
            zip_base64: B64.encode(&zip),
            expected_sha256: hash,
            manifest: json!({ "module_id": "evil" }),
        },
    )
    .unwrap_err();
    assert!(err.contains("不安全"), "实际：{err}");

    // 既没有安装结果，也没有越界写出
    assert!(!root.join("modules").join("evil").exists());
    assert!(!root.join("escaped.txt").exists());
    assert!(!root.parent().unwrap().join("escaped.txt").exists());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn reserved_module_installs_disabled() {
    let root = tmp_root("reserved");
    let state = permissive_state(&root);

    let zip = make_zip(&[("stub.txt", b"x")]);
    let hash = dounai_launcher::sha256_hex(&zip);

    install_module_inner(
        &state,
        InstallRequest {
            module_id: "dounai-memory-reserved".into(),
            category: "reserved".into(),
            version: "0.1.0".into(),
            zip_base64: B64.encode(&zip),
            expected_sha256: hash,
            manifest: json!({ "module_id": "dounai-memory-reserved" }),
        },
    )
    .expect("reserved 模块应可安装");

    let store = StateStore::open(&root.join("modules-state.json")).unwrap();
    let st = store.get("dounai-memory-reserved").unwrap();
    assert_eq!(st.install, InstallState::Installed);
    assert!(!st.enabled, "reserved 装完必须保持禁用");

    std::fs::remove_dir_all(&root).ok();
}
