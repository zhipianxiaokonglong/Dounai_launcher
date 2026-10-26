//! 端到端演示：为真实 manifest 签名并验签。
//!
//! 运行：cargo test --test sign_real_manifests -- --nocapture
//!
//! 这个测试用仓库里**真实的 manifest 文件**走完整签名链路，
//! 证明签名机制对接真实数据可用，而不是只在构造的 JSON 上成立。

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use dounai_launcher::signature::{canonical_payload, verify_manifest, SUPPORTED_ALGO};
use ed25519_dalek::{Signer, SigningKey};
use std::path::{Path, PathBuf};

fn manifests_dir() -> PathBuf {
    // 从 src-tauri 往上两级到仓库根，再进 manifests
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.join("manifests")
}

fn load_manifests() -> Vec<(PathBuf, serde_json::Value)> {
    let dir = manifests_dir();
    assert!(dir.exists(), "manifests 目录不存在：{}", dir.display());
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("读取 manifests 失败") {
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

        // 1) 原始 manifest 的 signature.value 应为空占位
        let empty_sig = json
            .get("signature")
            .and_then(|s| s.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .is_empty();
        assert!(empty_sig, "{name} 的签名占位应为空（发布时才填入）");

        // 2) 签前应验签失败（空值）
        assert!(
            verify_manifest(&json, &pk_b64).is_err(),
            "{name} 未签名时不应通过验签"
        );

        // 3) 真实签名
        let payload = canonical_payload(&json).expect("规范化失败");
        let sig = sk.sign(&payload);
        json["signature"]["value"] = serde_json::json!(B64.encode(sig.to_bytes()));
        json["signature"]["algo"] = serde_json::json!(SUPPORTED_ALGO);

        // 4) 签后应验签通过
        assert!(
            verify_manifest(&json, &pk_b64).is_ok(),
            "{name} 签名后应通过验签"
        );

        // 5) 篡改命令后必须失败
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
            "module_id",
            "name",
            "version",
            "category",
            "entry",
            "dependencies",
            "model_packs",
        ] {
            assert!(json.get(field).is_some(), "{name} 缺少必填字段 {field}");
        }
        // 依赖与模型包必须是数组且分开声明
        assert!(
            json["dependencies"].is_array(),
            "{name} dependencies 必须是数组"
        );
        assert!(
            json["model_packs"].is_array(),
            "{name} model_packs 必须是数组"
        );

        // reserved 类必须有 reserved 段且不可卸载
        if json["category"] == "reserved" {
            assert!(
                json.get("reserved").is_some(),
                "{name} reserved 模块必须有 reserved 段"
            );
            assert_eq!(
                json["reserved"]["removable"], false,
                "{name} reserved 模块不可卸载"
            );
            assert_eq!(
                json["reserved"]["default_enabled"], false,
                "{name} reserved 模块默认禁用"
            );
        }

        println!("{name}: 字段完整");
    }
}

#[allow(dead_code)]
fn _unused(_: &Path) {}
