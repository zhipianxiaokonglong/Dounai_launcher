//! M2 双源与包信任的纯逻辑层。
//!
//! 状态文件严格按 contracts/ 的 v1/v2 契约读写，M0 冻结字段不改语义。
//! 网络、窗口与 IPC 留在桌面壳；适配器只定义 trait，不发起请求。

pub mod adapters;
pub mod catalog;
pub mod contract_probe;
pub mod installed;
pub mod local;
pub mod manifest;
pub mod package;
pub mod retention;
pub mod source;
pub mod sources;
pub mod trust;

use std::cmp::Ordering;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 签名算法标识。Ed25519 直接对 `canonical_payload` 的字节签名；SHA-256 只用于
/// 逐文件哈希与公钥指纹，不是签名算法的一部分。改动此值等于换签名格式，
/// 首次对外发布后必须升 schema 版本，不能叠加兼容分支。
pub const SUPPORTED_ALGO: &str = "Ed25519";
pub const K5_KEY_ID: &str = "K5";
pub const DATA_DIR_POLICY: &str = "never_auto_delete";
pub const SOURCE_DIR_POLICY: &str = "never_delete";
pub const CLEANUP_TRIGGER: &str = "after_successful_activation";
pub const DEFAULT_KEEP_VERSIONS: u8 = 3;
pub const STANDBY_PORT_BASE: u16 = 18101;
pub const MAX_UNPACKED_BYTES: u64 = 512 * 1024 * 1024;
pub const CATALOG_MAX_AGE_DAYS: i64 = 7;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum M2Error {
    #[error("契约数据不合法：{0}")]
    Invalid(String),
    #[error("状态文件读写失败：{0}")]
    State(String),
    #[error("签名校验失败：{0}")]
    Signature(String),
    #[error("包不完整或被篡改：{0}")]
    Package(String),
    #[error("来源不可用：{0}")]
    Source(String),
    #[error("硬拒：{0}")]
    Rejected(String),
}

impl From<crate::state::StateError> for M2Error {
    fn from(e: crate::state::StateError) -> Self {
        Self::State(e.to_string())
    }
}
impl From<crate::signature::SignatureError> for M2Error {
    fn from(e: crate::signature::SignatureError) -> Self {
        Self::Signature(e.to_string())
    }
}
impl From<serde_json::Error> for M2Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Invalid(e.to_string())
    }
}

/// 四级验签结果。D 类硬拒，不写入实例表，也不提供确认入口。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    A,
    B,
    C,
    D,
}

impl Verdict {
    pub fn can_install(self) -> bool {
        matches!(self, Self::A | Self::B | Self::C)
    }
    pub fn can_auto_update(self) -> bool {
        self == Self::A
    }
    pub fn needs_confirmation(self) -> bool {
        matches!(self, Self::B | Self::C)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
            Self::C => "C",
            Self::D => "D",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Core,
    Optional,
    Reserved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceSourceKind {
    Zip,
    Dir,
}

impl InstanceSourceKind {
    /// 装包层从这里把 installed.json 的 source_kind 填进 EnvOptions，
    /// 避免 zip 实例误走 dir 的「允许重解析」分支。
    pub fn as_env_kind(self) -> crate::env::SourceKind {
        match self {
            Self::Zip => crate::env::SourceKind::Zip,
            Self::Dir => crate::env::SourceKind::Dir,
        }
    }
}

/// Ed25519 公钥原始 32 字节的 SHA-256，带 `sha256:` 前缀。
pub fn fingerprint(pubkey_b64: &str) -> Result<String, M2Error> {
    let bytes = B64
        .decode(pubkey_b64.trim())
        .map_err(|e| M2Error::Signature(format!("pubkey 不是合法 base64：{e}")))?;
    if bytes.len() != 32 {
        return Err(M2Error::Signature("pubkey 必须是 32 字节".into()));
    }
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

pub fn is_hash(s: &str) -> bool {
    let Some(value) = s.strip_prefix("sha256:") else {
        return false;
    };
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn is_fingerprint(s: &str) -> bool {
    is_hash(s)
}

pub fn semver_cmp(a: &str, b: &str) -> Result<Ordering, M2Error> {
    fn parts(s: &str) -> Result<[u64; 3], M2Error> {
        let bad = || M2Error::Invalid(format!("版本不是 SemVer 三段：{s}"));
        let fields: Vec<&str> = s.split('.').collect();
        if fields.len() != 3 {
            return Err(bad());
        }
        let mut out = [0u64; 3];
        for (i, part) in fields.iter().enumerate() {
            if part.is_empty()
                || !part.bytes().all(|c| c.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
            {
                return Err(bad());
            }
            out[i] = part.parse().map_err(|_| bad())?;
        }
        Ok(out)
    }
    Ok(parts(a)?.cmp(&parts(b)?))
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = m as i64 + if m > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 把 `YYYY-MM-DDTHH:MM:SSZ` 转成 UNIX 秒；格式不符返回 None。
pub fn parse_utc(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let num = |a: usize, z: usize| -> Option<i64> { s.get(a..z)?.parse().ok() };
    let (y, m, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hh, mm, ss) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 59 {
        return None;
    }
    Some(days_from_civil(y, m as u32, d as u32) * 86_400 + hh * 3_600 + mm * 60 + ss)
}

pub fn is_expired(expires: &str, now: &str) -> Result<bool, M2Error> {
    let e =
        parse_utc(expires).ok_or_else(|| M2Error::Invalid(format!("时间格式非法：{expires}")))?;
    let n = parse_utc(now).ok_or_else(|| M2Error::Invalid(format!("时间格式非法：{now}")))?;
    Ok(n > e)
}

/// K5 泄露靠短过期窗口兜底，catalog 有效期不得超过 7 天。
pub fn within_catalog_window(generated_at: &str, expires: &str) -> Result<bool, M2Error> {
    let g =
        parse_utc(generated_at).ok_or_else(|| M2Error::Invalid("generated_at 格式非法".into()))?;
    let e = parse_utc(expires).ok_or_else(|| M2Error::Invalid("expires 格式非法".into()))?;
    Ok(e > g && e - g <= CATALOG_MAX_AGE_DAYS * 86_400)
}

pub fn valid_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_uppercase() || c == '_')
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

pub const RESERVED_ENV_KEYS: &[&str] = &[
    "PATH",
    "PYTHONHOME",
    "PYTHONPATH",
    "VIRTUAL_ENV",
    "CONDA_PREFIX",
    "CONDA_DEFAULT_ENV",
    "SYSTEMROOT",
    "TEMP",
    "TMP",
];

pub fn reserved_env_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.starts_with("UV_") || RESERVED_ENV_KEYS.contains(&upper.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn utc_roundtrip_matches_known_values() {
        assert_eq!(parse_utc("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_utc("2026-01-01T00:00:00Z"), Some(1_767_225_600));
        assert_eq!(parse_utc("2024-02-29T12:34:56Z"), Some(1_709_210_096));
        assert_eq!(parse_utc("2026-01-01 00:00:00Z"), None);
        assert_eq!(parse_utc("2026-13-01T00:00:00Z"), None);
    }

    #[test]
    fn semver_only_compares_three_numeric_segments() {
        assert_eq!(semver_cmp("1.2.10", "1.2.9").unwrap(), Ordering::Greater);
        assert_eq!(semver_cmp("1.2.0", "1.2.0").unwrap(), Ordering::Equal);
        assert!(semver_cmp("1.2", "1.2.0").is_err());
        assert!(semver_cmp("1.02.0", "1.2.0").is_err());
        assert!(semver_cmp("1.2.0-agent", "1.2.0").is_err());
    }

    #[test]
    fn hash_and_env_key_shapes() {
        assert!(is_hash(&format!("sha256:{}", "a".repeat(64))));
        assert!(!is_hash("a".repeat(64).as_str()));
        assert!(valid_env_key("OMNI_PORT"));
        assert!(!valid_env_key("omni_port"));
        assert!(reserved_env_key("uv_cache_dir"));
        assert!(reserved_env_key("PATH"));
    }

    #[test]
    fn fingerprint_is_sha256_of_raw_key_bytes() {
        let key = [7u8; 32];
        let b64 = B64.encode(key);
        assert_eq!(
            fingerprint(&b64).unwrap(),
            format!("sha256:{}", crate::security::sha256_hex(&key))
        );
        assert!(fingerprint("AAAA").is_err());
    }
}
