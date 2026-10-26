//! manifest 信任决策（签名校验与安装链路绑定）。
//!
//! 为什么单独一层：`signature.rs` 只回答"这个签名对不对"，
//! 而安装链路需要回答"**在没有可信签名时，我该不该继续**"。
//! 后者是策略问题，会随构建类型（调试/发布）变化，
//! 混进签名验算里会让两件事都变难测试。
//!
//! 策略默认值（fail-closed）：
//! - 调试构建：允许未签名（便于用仓库里的样例 manifest 开发）；
//! - 发布构建：**拒绝未签名**，且必须至少配置一个可信公钥。
//!
//! 发布构建拒绝未签名是刻意的：启动器按 manifest 决定"启动什么命令、
//! 开哪些权限"，一个被篡改的 manifest 可以把恶意命令伪装成正常模块。

use serde::{Deserialize, Serialize};

use crate::signature::{verify_manifest, SignatureError};

/// 单个 manifest 的信任判定结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrustVerdict {
    /// 由第 index 个可信公钥验证通过
    Signed { key_index: usize },
    /// 未签名，但策略允许（仅调试构建）
    UnsignedAllowed,
}

impl TrustVerdict {
    pub fn is_signed(&self) -> bool {
        matches!(self, Self::Signed { .. })
    }
}

/// 信任决策失败。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TrustError {
    #[error("manifest 未签名，当前策略不允许安装未签名模块")]
    Unsigned,
    #[error("没有配置任何可信公钥，无法验证签名")]
    NoTrustedKeys,
    #[error("manifest 的签名无法通过任何可信公钥验证：{0}")]
    BadSignature(String),
}

/// 信任策略。
#[derive(Debug, Clone)]
pub struct TrustPolicy {
    /// 受信任的 Ed25519 公钥（base64，32 字节）
    trusted_keys: Vec<String>,
    /// 是否允许未签名 manifest
    allow_unsigned: bool,
}

impl Default for TrustPolicy {
    /// 默认策略：无公钥、不允许未签名（最保守）。
    fn default() -> Self {
        Self {
            trusted_keys: Vec::new(),
            allow_unsigned: false,
        }
    }
}

impl TrustPolicy {
    pub fn new(trusted_keys: Vec<String>, allow_unsigned: bool) -> Self {
        Self {
            trusted_keys: trusted_keys
                .into_iter()
                .map(|k| k.trim().to_string())
                .filter(|k| !k.is_empty())
                .collect(),
            allow_unsigned,
        }
    }

    /// 调试构建用：允许未签名。
    pub fn permissive() -> Self {
        Self {
            trusted_keys: Vec::new(),
            allow_unsigned: true,
        }
    }

    /// 发布构建用：配置可信公钥，并拒绝未签名。
    pub fn strict(trusted_keys: Vec<String>) -> Self {
        Self::new(trusted_keys, false)
    }

    pub fn trusted_keys(&self) -> &[String] {
        &self.trusted_keys
    }

    pub fn allow_unsigned(&self) -> bool {
        self.allow_unsigned
    }

    /// 是否"完全放行"：无公钥且允许未签名。
    ///
    /// 这个状态下安装链路没有任何签名保障，仅应出现在开发期。
    pub fn is_open(&self) -> bool {
        self.trusted_keys.is_empty() && self.allow_unsigned
    }
}

/// 判断 manifest 里是否带了非空的签名值。
fn has_signature_value(manifest: &serde_json::Value) -> bool {
    manifest
        .get("signature")
        .and_then(|s| s.get("value"))
        .and_then(|v| v.as_str())
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

/// 按策略判定 manifest 可信度。
///
/// 顺序很重要：先看有没有签名。
/// 有签名就必须验通（不能因为"验不过就当作未签名放行"）；
/// 没签名才落到 `allow_unsigned` 那一支。
pub fn evaluate(
    manifest: &serde_json::Value,
    policy: &TrustPolicy,
) -> Result<TrustVerdict, TrustError> {
    if !has_signature_value(manifest) {
        return if policy.allow_unsigned {
            Ok(TrustVerdict::UnsignedAllowed)
        } else {
            Err(TrustError::Unsigned)
        };
    }

    if policy.trusted_keys.is_empty() {
        return Err(TrustError::NoTrustedKeys);
    }

    let mut last_error = String::new();
    for (index, key) in policy.trusted_keys.iter().enumerate() {
        match verify_manifest(manifest, key) {
            Ok(()) => return Ok(TrustVerdict::Signed { key_index: index }),
            Err(e) => last_error = format_signature_error(&e),
        }
    }

    Err(TrustError::BadSignature(last_error))
}

fn format_signature_error(e: &SignatureError) -> String {
    match e {
        SignatureError::UnsupportedAlgo(a) => format!("不支持的算法 {a}"),
        SignatureError::Invalid => "签名与内容不匹配".to_string(),
        SignatureError::EmptyValue => "签名值为空".to_string(),
        SignatureError::Missing => "缺少 signature 段".to_string(),
        SignatureError::BadPublicKey(m) => format!("公钥非法：{m}"),
        SignatureError::BadSignature(m) => format!("签名格式非法：{m}"),
        SignatureError::Serialize(m) => format!("序列化失败：{m}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine as _;
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::json;

    fn keypair(seed: u8) -> (SigningKey, String) {
        let sk = SigningKey::from_bytes(&[seed; 32]);
        let pk = B64.encode(sk.verifying_key().to_bytes());
        (sk, pk)
    }

    fn signed_manifest(sk: &SigningKey) -> serde_json::Value {
        let mut m = json!({
            "module_id": "demo",
            "name": "演示",
            "version": "1.0.0",
            "entry": { "command": "run.exe" },
            "signature": { "algo": crate::signature::SUPPORTED_ALGO, "value": "" }
        });
        let payload = crate::signature::canonical_payload(&m).unwrap();
        let sig = sk.sign(&payload);
        m["signature"]["value"] = json!(B64.encode(sig.to_bytes()));
        m
    }

    fn unsigned_manifest() -> serde_json::Value {
        json!({
            "module_id": "demo",
            "version": "1.0.0",
            "signature": { "algo": crate::signature::SUPPORTED_ALGO, "value": "" }
        })
    }

    #[test]
    fn unsigned_rejected_by_strict_policy() {
        let policy = TrustPolicy::strict(vec![keypair(1).1]);
        assert_eq!(
            evaluate(&unsigned_manifest(), &policy),
            Err(TrustError::Unsigned)
        );
    }

    #[test]
    fn unsigned_allowed_by_permissive_policy() {
        let policy = TrustPolicy::permissive();
        assert_eq!(
            evaluate(&unsigned_manifest(), &policy),
            Ok(TrustVerdict::UnsignedAllowed)
        );
    }

    #[test]
    fn signed_by_trusted_key_passes() {
        let (sk, pk) = keypair(2);
        let policy = TrustPolicy::strict(vec![pk]);
        let v = evaluate(&signed_manifest(&sk), &policy).unwrap();
        assert_eq!(v, TrustVerdict::Signed { key_index: 0 });
        assert!(v.is_signed());
    }

    #[test]
    fn signed_by_untrusted_key_rejected() {
        let (sk, _) = keypair(3);
        let other_pk = keypair(4).1;
        let policy = TrustPolicy::strict(vec![other_pk]);
        let r = evaluate(&signed_manifest(&sk), &policy);
        assert!(matches!(r, Err(TrustError::BadSignature(_))), "实际 {r:?}");
    }

    #[test]
    fn signed_but_no_trusted_keys_rejected() {
        // 有签名却没有配置公钥：不能因为"配不出公钥"就放行
        let (sk, _) = keypair(5);
        let policy = TrustPolicy::new(Vec::new(), true);
        assert_eq!(
            evaluate(&signed_manifest(&sk), &policy),
            Err(TrustError::NoTrustedKeys)
        );
    }

    #[test]
    fn tampered_signed_manifest_rejected_even_if_unsigned_allowed() {
        // 关键：有签名但被篡改，不能退化成"当作未签名放行"
        let (sk, pk) = keypair(6);
        let mut m = signed_manifest(&sk);
        m["entry"]["command"] = json!("evil.exe");
        let policy = TrustPolicy::new(vec![pk], true);
        assert!(matches!(
            evaluate(&m, &policy),
            Err(TrustError::BadSignature(_))
        ));
    }

    #[test]
    fn trusts_any_of_multiple_keys() {
        let (sk, _) = keypair(7);
        let keys = vec![keypair(8).1, keypair(7).1, keypair(9).1];
        let policy = TrustPolicy::strict(keys);
        let v = evaluate(&signed_manifest(&sk), &policy).unwrap();
        assert_eq!(v, TrustVerdict::Signed { key_index: 1 });
    }

    #[test]
    fn policy_trims_and_drops_empty_keys() {
        let p = TrustPolicy::new(vec!["  ".into(), "abc".into(), "".into()], false);
        assert_eq!(p.trusted_keys(), &["abc".to_string()]);
    }

    #[test]
    fn is_open_only_when_no_keys_and_unsigned_allowed() {
        assert!(TrustPolicy::permissive().is_open());
        assert!(!TrustPolicy::strict(vec!["k".into()]).is_open());
        // 无公钥但不允许未签名：不是"开放"，因为什么都装不了
        assert!(!TrustPolicy::default().is_open());
    }
}
