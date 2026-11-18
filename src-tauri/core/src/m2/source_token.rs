//! 源邀请码：把「catalog 地址 + 签名公钥」压成一行可复制的字符串。
//!
//! 格式：
//!
//! ```text
//! dnsrc1:<payload>.<check>
//! ```
//!
//! - `dnsrc1` 是版本前缀，便于以后换格式而不叠加兼容分支。
//! - `payload` 是 UTF-8 JSON 的 base64url（无 padding）。字段：`v`=1、`u`=catalog 基址、
//!   `k`=Ed25519 公钥 32 字节的标准 base64（与 sources.json 同格式）、`n`=名称（可选）。
//!   有意**不带**指纹与自定义白名单：指纹一律从 `k` 现场重算，白名单一律从 `u` 派生。
//! - `check` 是 `payload` 字符串 SHA-256 的前 8 字节 base64url，只防粘贴截断/手抖，
//!   **不是信任根**。能造邀请码的人同样能造 check；信任仍来自带外核对指纹后显式确认。
//!
//! 本模块只做编解码与形状校验，不联网、不写文件、不判断主机是否回环——
//! 这些留在命令层与既有 source/sources 逻辑里，保持一条派生路径。

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{fingerprint, M2Error};

pub const SOURCE_TOKEN_PREFIX: &str = "dnsrc1";
const TOKEN_VERSION: u32 = 1;
const CHECK_BYTES: usize = 8;
const MAX_TOKEN_LEN: usize = 8192;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TokenPayload {
    v: u32,
    u: String,
    k: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    n: Option<String>,
}

/// 解析后的邀请码。只有三个字段——指纹与白名单刻意不落在结果里，
/// 调用方必须自己从 `catalog_pubkey` 算指纹、从 `base_url` 派生白名单。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceToken {
    pub base_url: String,
    pub catalog_pubkey: String,
    pub name: Option<String>,
}

fn checksum(payload: &str) -> String {
    let digest = Sha256::digest(payload.as_bytes());
    URL_SAFE_NO_PAD.encode(&digest[..CHECK_BYTES])
}

/// 生成邀请码。`catalog_pubkey` 必须是 32 字节 Ed25519 公钥的标准 base64。
pub fn issue(base_url: &str, catalog_pubkey: &str, name: Option<&str>) -> Result<String, M2Error> {
    let url = base_url.trim().trim_end_matches('/').to_string();
    if url.is_empty() {
        return Err(M2Error::Invalid("邀请码的 catalog 地址不能为空".into()));
    }
    let key = catalog_pubkey.trim().to_string();
    // 复用指纹函数做公钥形状校验；不把结果写进邀请码。
    fingerprint(&key)?;
    let trimmed_name = name.map(str::trim).filter(|s| !s.is_empty());
    let payload = TokenPayload {
        v: TOKEN_VERSION,
        u: url,
        k: key,
        n: trimmed_name.map(str::to_string),
    };
    let json = serde_json::to_vec(&payload)
        .map_err(|e| M2Error::Invalid(format!("邀请码载荷序列化失败：{e}")))?;
    let body = URL_SAFE_NO_PAD.encode(&json);
    Ok(format!("{SOURCE_TOKEN_PREFIX}:{body}.{}", checksum(&body)))
}

/// 解析并校验邀请码。任何写在载荷里的指纹/白名单字段都会被忽略：
/// `TokenPayload` 没有这些字段，serde 默认丢弃未知键，调用方拿不到它们。
pub fn parse(token: &str) -> Result<SourceToken, M2Error> {
    let token = token.trim();
    if token.len() > MAX_TOKEN_LEN {
        return Err(M2Error::Invalid("邀请码过长".into()));
    }
    let body = token
        .strip_prefix(SOURCE_TOKEN_PREFIX)
        .and_then(|rest| rest.strip_prefix(':'))
        .ok_or_else(|| M2Error::Invalid("邀请码必须以 dnsrc1: 开头".into()))?;
    let (payload, check) = body
        .rsplit_once('.')
        .ok_or_else(|| M2Error::Invalid("邀请码缺少校验段".into()))?;
    if payload.is_empty() || check.is_empty() {
        return Err(M2Error::Invalid("邀请码载荷或校验段为空".into()));
    }
    if checksum(payload) != check {
        return Err(M2Error::Invalid(
            "邀请码校验失败：粘贴不完整或内容被改动".into(),
        ));
    }
    let json = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|e| M2Error::Invalid(format!("邀请码载荷不是合法 base64url：{e}")))?;
    let decoded: TokenPayload = serde_json::from_slice(&json)
        .map_err(|e| M2Error::Invalid(format!("邀请码载荷不是合法 JSON：{e}")))?;
    if decoded.v != TOKEN_VERSION {
        return Err(M2Error::Invalid(format!(
            "不支持的邀请码版本：{}",
            decoded.v
        )));
    }
    let url = decoded.u.trim().trim_end_matches('/').to_string();
    if url.is_empty() {
        return Err(M2Error::Invalid("邀请码里的 catalog 地址为空".into()));
    }
    // 只校验公钥形状；指纹由命令层从同一字段重算，不信任载荷里的任何声明。
    fingerprint(&decoded.k)?;
    Ok(SourceToken {
        base_url: url,
        catalog_pubkey: decoded.k,
        name: decoded.n.filter(|s| !s.trim().is_empty()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;

    fn key(byte: u8) -> String {
        STANDARD.encode([byte; 32])
    }

    #[test]
    fn roundtrip_with_and_without_name() {
        let token = issue(
            "http://127.0.0.1:8765/releases",
            &key(7),
            Some("本地测试源"),
        )
        .unwrap();
        assert!(token.starts_with("dnsrc1:"));
        let parsed = parse(&token).unwrap();
        assert_eq!(parsed.base_url, "http://127.0.0.1:8765/releases");
        assert_eq!(parsed.catalog_pubkey, key(7));
        assert_eq!(parsed.name.as_deref(), Some("本地测试源"));

        let bare = issue("https://example.org/releases/", &key(9), Some("  ")).unwrap();
        let parsed = parse(&bare).unwrap();
        assert_eq!(parsed.base_url, "https://example.org/releases");
        assert_eq!(parsed.name, None);
    }

    #[test]
    fn trailing_slash_and_whitespace_are_normalised() {
        let token = issue("  https://example.org/releases/  ", &key(1), None).unwrap();
        let parsed = parse(&format!("  {token}  ")).unwrap();
        assert_eq!(parsed.base_url, "https://example.org/releases");
    }

    #[test]
    fn truncated_or_tampered_token_is_rejected() {
        let token = issue("https://example.org/releases", &key(3), None).unwrap();
        assert!(parse(&token[..token.len() - 3]).is_err());
        let tampered = format!("{}x", token);
        assert!(parse(&tampered).is_err());
        let (payload, check) = token.rsplit_once('.').unwrap();
        let bad_check = format!("dnsrc1:{payload}.{}x", check);
        assert!(parse(&bad_check).is_err());
    }

    #[test]
    fn wrong_prefix_and_bad_base64_are_rejected() {
        let token = issue("https://example.org/releases", &key(4), None).unwrap();
        assert!(parse(token.strip_prefix("dnsrc1:").unwrap()).is_err());
        assert!(parse("dnsrc1:!!!.AAAAAAAAAAA").is_err());
        assert!(parse("dnsrc1:no-dot").is_err());
    }

    #[test]
    fn wrong_version_and_bad_key_shape_are_rejected() {
        let json = r#"{"v":9,"u":"https://example.org/x","k":"AAAAAAAA"}"#;
        let body = URL_SAFE_NO_PAD.encode(json.as_bytes());
        let token = format!("dnsrc1:{body}.{}", checksum(&body));
        assert!(parse(&token).unwrap_err().to_string().contains("版本"));

        let token = issue("https://example.org/x", "short", None).unwrap_err();
        assert!(token.to_string().contains("base64") || token.to_string().contains("32 字节"));
    }

    #[test]
    fn embedded_fingerprint_and_allowlist_are_ignored() {
        // 载荷里塞一个伪造指纹与恶意白名单：serde 丢弃未知键，结果里拿不到。
        let json = format!(
            r#"{{"v":1,"u":"https://example.org/x","k":"{}","fingerprint":"sha256:deadbeef","h":["evil.example"],"n":"源"}}"#,
            key(5)
        );
        let body = URL_SAFE_NO_PAD.encode(json.as_bytes());
        let token = format!("dnsrc1:{body}.{}", checksum(&body));
        let parsed = parse(&token).unwrap();
        assert_eq!(parsed.catalog_pubkey, key(5));
        assert_eq!(parsed.name.as_deref(), Some("源"));
        // 结果与伪造值无关：指纹要从 k 重算，白名单要从 u 派生。
        assert_eq!(
            super::fingerprint(&parsed.catalog_pubkey).unwrap().len(),
            71
        );
    }

    #[test]
    fn oversized_token_is_rejected() {
        let huge = format!("dnsrc1:{}.{}", "A".repeat(MAX_TOKEN_LEN), "AAAAAAAAAAA");
        assert!(parse(&huge).is_err());
    }
}
