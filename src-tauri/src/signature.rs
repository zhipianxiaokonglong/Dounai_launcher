//! 模块 manifest 签名校验（`signature` 字段，配合哈希校验）。
//!
//! 签名对象是 **manifest 的规范化 JSON**（去掉 `signature` 字段本身），
//! 这样签名不受字段顺序与空白影响，也不会因为签名字段自身而无法验签。
//!
//! 为什么必须签名：启动器会按 manifest 决定「启动哪个命令、开放哪些权限」。
//! 一个被篡改的 manifest 可以把恶意命令伪装成正常模块，这是最直接的后门路径。

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

/// 签名校验错误。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SignatureError {
    #[error("manifest 缺少 signature 段")]
    Missing,
    #[error("签名算法不受支持：{0}")]
    UnsupportedAlgo(String),
    #[error("签名值为空")]
    EmptyValue,
    #[error("公钥格式非法：{0}")]
    BadPublicKey(String),
    #[error("签名格式非法：{0}")]
    BadSignature(String),
    #[error("验签失败：签名与内容不匹配")]
    Invalid,
    #[error("无法序列化 manifest：{0}")]
    Serialize(String),
}

/// 受支持的算法标识。
pub const SUPPORTED_ALGO: &str = "SHA-256 + Ed25519";

/// 从 manifest JSON 中取出待验签内容：剔除 `signature` 字段后的规范化字符串。
///
/// 规范化 = 按 key 升序重新序列化，保证同一内容任何书写顺序都得到同一字节串。
pub fn canonical_payload(manifest: &serde_json::Value) -> Result<Vec<u8>, SignatureError> {
    let mut clone = manifest.clone();
    if let Some(obj) = clone.as_object_mut() {
        obj.remove("signature");
    }
    let canonical = canonicalize(&clone);
    serde_json::to_vec(&canonical).map_err(|e| SignatureError::Serialize(e.to_string()))
}

/// 递归地按键名排序，保证序列化稳定。
fn canonicalize(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for k in keys {
                out.insert(k.clone(), canonicalize(&map[k]));
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonicalize).collect())
        }
        other => other.clone(),
    }
}

/// 用公钥验证 manifest 签名。
///
/// `public_key_b64` 是 base64 编码的 32 字节 Ed25519 公钥。
pub fn verify_manifest(
    manifest: &serde_json::Value,
    public_key_b64: &str,
) -> Result<(), SignatureError> {
    let sig = manifest.get("signature").ok_or(SignatureError::Missing)?;

    let algo = sig
        .get("algo")
        .and_then(|v| v.as_str())
        .ok_or(SignatureError::Missing)?;
    if algo != SUPPORTED_ALGO {
        return Err(SignatureError::UnsupportedAlgo(algo.to_string()));
    }

    let value = sig
        .get("value")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if value.is_empty() {
        return Err(SignatureError::EmptyValue);
    }

    let pk_bytes = B64
        .decode(public_key_b64.trim())
        .map_err(|e| SignatureError::BadPublicKey(e.to_string()))?;
    let pk_array: [u8; 32] = pk_bytes
        .try_into()
        .map_err(|_| SignatureError::BadPublicKey("公钥必须是 32 字节".into()))?;
    let verifying_key = VerifyingKey::from_bytes(&pk_array)
        .map_err(|e| SignatureError::BadPublicKey(e.to_string()))?;

    let sig_bytes = B64
        .decode(value.as_bytes())
        .map_err(|e| SignatureError::BadSignature(e.to_string()))?;
    let sig_array: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| SignatureError::BadSignature("签名必须是 64 字节".into()))?;
    let signature = Signature::from_bytes(&sig_array);

    let payload = canonical_payload(manifest)?;
    verifying_key
        .verify(&payload, &signature)
        .map_err(|_| SignatureError::Invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::json;

    fn keypair() -> (SigningKey, String) {
        // 固定种子，保证测试可复现
        let seed = [7u8; 32];
        let sk = SigningKey::from_bytes(&seed);
        let pk_b64 = B64.encode(sk.verifying_key().to_bytes());
        (sk, pk_b64)
    }

    fn signed_manifest(sk: &SigningKey) -> serde_json::Value {
        let mut m = json!({
            "module_id": "demo",
            "name": "演示模块",
            "version": "1.0.0",
            "category": "optional",
            "entry": { "command": "run.exe" },
            "signature": { "algo": SUPPORTED_ALGO, "value": "" }
        });
        let payload = canonical_payload(&m).unwrap();
        let sig = sk.sign(&payload);
        m["signature"]["value"] = json!(B64.encode(sig.to_bytes()));
        m
    }

    #[test]
    fn verifies_valid_signature() {
        let (sk, pk) = keypair();
        let m = signed_manifest(&sk);
        assert_eq!(verify_manifest(&m, &pk), Ok(()));
    }

    #[test]
    fn rejects_tampered_content() {
        let (sk, pk) = keypair();
        let mut m = signed_manifest(&sk);
        // 篡改入口命令——典型后门手法
        m["entry"]["command"] = json!("evil.exe");
        assert_eq!(verify_manifest(&m, &pk), Err(SignatureError::Invalid));
    }

    #[test]
    fn rejects_tampered_permissions() {
        let (sk, pk) = keypair();
        let mut m = signed_manifest(&sk);
        m["permissions"] = json!({ "network": ["*"], "filesystem": ["/"], "devices": [] });
        assert_eq!(verify_manifest(&m, &pk), Err(SignatureError::Invalid));
    }

    #[test]
    fn canonicalization_is_key_order_independent() {
        let a = json!({ "b": 1, "a": 2 });
        let b = json!({ "a": 2, "b": 1 });
        assert_eq!(
            canonical_payload(&json!({"x": a, "signature": {"value": "z"}})).unwrap(),
            canonical_payload(&json!({"signature": {"value": "z"}, "x": b})).unwrap()
        );
    }

    #[test]
    fn rejects_missing_signature_block() {
        let (_, pk) = keypair();
        let m = json!({ "module_id": "x" });
        assert_eq!(verify_manifest(&m, &pk), Err(SignatureError::Missing));
    }

    #[test]
    fn rejects_unsupported_algo() {
        let (_, pk) = keypair();
        let m = json!({
            "module_id": "x",
            "signature": { "algo": "MD5", "value": "AAA" }
        });
        assert!(matches!(
            verify_manifest(&m, &pk),
            Err(SignatureError::UnsupportedAlgo(_))
        ));
    }

    #[test]
    fn rejects_empty_signature_value() {
        let (_, pk) = keypair();
        let m = json!({
            "module_id": "x",
            "signature": { "algo": SUPPORTED_ALGO, "value": "" }
        });
        assert_eq!(verify_manifest(&m, &pk), Err(SignatureError::EmptyValue));
    }

    #[test]
    fn rejects_bad_public_key() {
        let (sk, _) = keypair();
        let m = signed_manifest(&sk);
        assert!(matches!(
            verify_manifest(&m, "not-base64!!"),
            Err(SignatureError::BadPublicKey(_))
        ));
    }

    #[test]
    fn rejects_wrong_length_public_key() {
        let (sk, _) = keypair();
        let m = signed_manifest(&sk);
        let short = B64.encode([1u8, 2, 3]);
        assert!(matches!(
            verify_manifest(&m, &short),
            Err(SignatureError::BadPublicKey(_))
        ));
    }

    #[test]
    fn rejects_signature_from_other_key() {
        let (sk, _) = keypair();
        let m = signed_manifest(&sk);
        let other = SigningKey::from_bytes(&[9u8; 32]);
        let other_pk = B64.encode(other.verifying_key().to_bytes());
        assert_eq!(verify_manifest(&m, &other_pk), Err(SignatureError::Invalid));
    }
}
