//! 端到端演示：为真实 manifest 签名并验签。
//!
//! 运行：cargo test --test sign_real_manifests -- --nocapture
//!
//! 数据来源是 `contracts/examples/*.v2.json`——契约里的 v2 样例清单。
//! 用它而不是自造的 JSON：样例是打包、安装、验签三处共用的形状，
//! 样例改坏了这里会立刻红。
//!
//! v1 的 `manifests/` 目录已随前端内置清单一起删除：那份清单是 v1 schema
//! （module_id 没有 namespace、entry.command 是裸命令），与实际安装的包对不上。

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use dounai_launcher::signature::{canonical_payload, verify_manifest, SUPPORTED_ALGO};
use ed25519_dalek::{Signer, SigningKey};
use std::path::PathBuf;

fn examples_dir() -> PathBuf {
    // 从 src-tauri 往上两级到仓库根，再进 contracts/examples
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.join("contracts").join("examples")
}

fn load_manifests() -> Vec<(PathBuf, serde_json::Value)> {
    let dir = examples_dir();
    assert!(dir.exists(), "契约样例目录不存在：{}", dir.display());
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("读取 contracts/examples 失败") {
        let entry = entry.expect("目录项");
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("读取 manifest");
        let json: serde_json::Value = serde_json::from_str(&text).expect("解析 manifest");
        out.push((path, json));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn signs_and_verifies_every_real_manifest() {
    let (sk, pk_b64) = {
        let sk = SigningKey::from_bytes(&[42u8; 32]);
        let pk = B64.encode(sk.verifying_key().to_bytes());
        (sk, pk)
    };

    let manifests = load_manifests();
    assert!(!manifests.is_empty(), "没有找到任何 manifest");

    for (path, mut json) in manifests {
        let name = path.file_name().unwrap().to_string_lossy().to_string();

        // 1) 样例里的 signature 是 null 或空占位：发布时才填。
        let signed_already = json
            .get("signature")
            .and_then(|s| s.get("value"))
            .and_then(|v| v.as_str())
            .is_some_and(|v| !v.is_empty());
        assert!(!signed_already, "{name} 的签名占位应为空（发布时才填入）");

        // 2) 签前应验签失败
        assert!(
            verify_manifest(&json, &pk_b64).is_err(),
            "{name} 未签名时不应通过验签"
        );

        // 3) 真实签名
        let payload = canonical_payload(&json).expect("规范化失败");
        let sig = sk.sign(&payload);
        json["signature"] = serde_json::json!({
            "algo": SUPPORTED_ALGO,
            "pubkey": pk_b64,
            "key_id": "K1",
            "signed_at": "2026-01-01T00:00:00Z",
            "value": B64.encode(sig.to_bytes()),
        });

        // 4) 签后应验签通过
        assert!(
            verify_manifest(&json, &pk_b64).is_ok(),
            "{name} 签名后应通过验签"
        );

        // 5) 篡改入口命令后必须失败
        let mut tampered = json.clone();
        tampered["entry"]["command"] = serde_json::json!("evil.exe --payload");
        assert!(
            verify_manifest(&tampered, &pk_b64).is_err(),
            "{name} 被篡改后必须验签失败"
        );

        println!("{name}: 签名→验签通过→篡改被拒");
    }
}

#[test]
fn manifest_files_are_valid_json_with_required_fields() {
    for (path, json) in load_manifests() {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        for field in [
            "schema_version",
            "module_id",
            "name",
            "version",
            "variant",
            "role",
            "category",
            "publisher",
            "requires",
            "entry",
            "ports",
            "permissions",
            "files",
        ] {
            assert!(json.get(field).is_some(), "{name} 缺少必填字段 {field}");
        }
        assert_eq!(json["schema_version"], serde_json::json!(2), "{name} 必须是 v2 清单");
        // module_id 必须是 namespace/name，否则实例 id 拼不出来。
        let module_id = json["module_id"].as_str().unwrap_or_default();
        assert!(
            module_id.matches('/').count() == 1 && !module_id.starts_with('/'),
            "{name} 的 module_id 不是 namespace/name：{module_id}"
        );
        // 数据面模块的入口必须走 venv，shell 必须指安装根。
        let command = json["entry"]["command"].as_str().unwrap_or_default();
        if json["role"] == "shell" {
            assert!(
                command.contains("{install_root}"),
                "{name} shell 入口必须含 {{install_root}}"
            );
        } else {
            assert!(
                command.contains("{venv_python}"),
                "{name} 数据面入口必须含 {{venv_python}}"
            );
            assert!(
                json.get("runtime").is_some(),
                "{name} 非 shell 必须有 runtime"
            );
        }

        println!("{name}: 字段完整");
    }
}
