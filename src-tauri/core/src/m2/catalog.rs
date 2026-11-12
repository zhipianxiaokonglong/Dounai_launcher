//! K5 签名 catalog、双源裁决与新鲜度。
//!
//! 不得回退到未验签的旧缓存：任一来源验签失败、序号倒退或过期，都按
//! `CatalogInvalid` 处理。裁决结果写进 installed.json 的 catalog_state。

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::m2::manifest::valid_signature_value;
use crate::m2::source::SourceError;
use crate::m2::{
    is_expired, is_fingerprint, is_hash, semver_cmp, within_catalog_window, M2Error, K5_KEY_ID,
    SUPPORTED_ALGO,
};
use crate::manifest::Role;

fn default_true() -> bool {
    true
}

fn empty_value() -> serde_json::Value {
    serde_json::Value::Null
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSourceKind {
    Github,
    Sourceforge,
    Https,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogSource {
    pub id: String,
    pub kind: CatalogSourceKind,
    pub base_url: String,
    pub priority: u32,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogSignature {
    pub algo: String,
    pub key_id: String,
    pub signed_at: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntrySource {
    pub source_id: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub module_id: String,
    pub variant: String,
    pub version: String,
    pub role: Role,
    pub name: String,
    pub publisher_fp: String,
    pub sha256: String,
    pub size: u64,
    pub sources: Vec<EntrySource>,
    pub signature_required: bool,
    pub expires: String,
    #[serde(default)]
    pub expected_signer_fp: Option<String>,
    #[serde(default)]
    pub min_version: Option<String>,
    #[serde(default)]
    pub revoked: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevocationKind {
    Key,
    Package,
    Version,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revocation {
    pub kind: RevocationKind,
    pub id: String,
    pub at: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogDocument {
    pub schema_version: u8,
    pub sequence: u64,
    pub generated_at: String,
    pub expires: String,
    pub sources: Vec<CatalogSource>,
    pub modules: Vec<CatalogEntry>,
    pub revoked: Vec<Revocation>,
    pub signature: CatalogSignature,
    #[serde(skip, default = "empty_value")]
    pub raw: serde_json::Value,
}

impl CatalogDocument {
    /// 用内置 K5 公钥验签。签名覆盖剔除 `signature` 后的整份规范化 JSON。
    pub fn parse(text: &str, k5_pubkey_b64: &str) -> Result<Self, SourceError> {
        let bad = |reason: String| SourceError::CatalogInvalid { reason };
        let raw: serde_json::Value =
            serde_json::from_str(text).map_err(|e| bad(format!("catalog 不是合法 JSON：{e}")))?;
        let mut doc: Self = serde_json::from_value(raw.clone())
            .map_err(|e| bad(format!("catalog 字段不合法：{e}")))?;
        doc.raw = raw;
        if doc.schema_version != 2 {
            return Err(bad("schema_version 必须为 2".into()));
        }
        if doc.sequence < 1 {
            return Err(bad("sequence 必须不小于 1".into()));
        }
        if doc.signature.algo != SUPPORTED_ALGO || doc.signature.key_id != K5_KEY_ID {
            return Err(bad("catalog 必须由 K5 用既定算法签名".into()));
        }
        if !valid_signature_value(&doc.signature.value) {
            return Err(bad("catalog 签名值格式非法".into()));
        }
        crate::signature::verify_manifest(&doc.raw, k5_pubkey_b64)
            .map_err(|e| bad(format!("K5 验签失败：{e}")))?;
        doc.validate_content()?;
        Ok(doc)
    }

    fn validate_content(&self) -> Result<(), SourceError> {
        let bad = |reason: String| SourceError::CatalogInvalid { reason };
        if self.sources.is_empty() {
            return Err(bad("catalog 至少要注册一个源".into()));
        }
        for source in &self.sources {
            if !source.base_url.starts_with("https://") {
                return Err(bad(format!("源 {} 的 base_url 必须用 https", source.id)));
            }
        }
        for entry in &self.modules {
            if semver_cmp(&entry.version, "0.0.0").is_err() {
                return Err(bad(format!("{} 的版本非法", entry.module_id)));
            }
            if !is_hash(&entry.sha256) || !is_fingerprint(&entry.publisher_fp) {
                return Err(bad(format!("{} 的哈希或指纹格式非法", entry.module_id)));
            }
            if entry.sources.is_empty() {
                return Err(bad(format!("{} 没有下载地址", entry.module_id)));
            }
            for source in &entry.sources {
                if !source.url.starts_with("https://") {
                    return Err(bad(format!("{} 的下载地址必须用 https", entry.module_id)));
                }
            }
            if crate::m2::parse_utc(&entry.expires).is_none() {
                return Err(bad(format!("{} 的 expires 格式非法", entry.module_id)));
            }
        }
        Ok(())
    }

    /// 新鲜度、序号与时间单调裁决。旧缓存一律不参与。
    pub fn validate(&self, previous: &CatalogState, now: &str) -> Result<(), SourceError> {
        let bad = |reason: String| SourceError::CatalogInvalid { reason };
        if is_expired(&self.expires, now).map_err(|e| bad(e.to_string()))? {
            return Err(bad("catalog 已过期".into()));
        }
        if !within_catalog_window(&self.generated_at, &self.expires)
            .map_err(|e| bad(e.to_string()))?
        {
            return Err(bad("catalog 有效期超过 7 天窗口或时间倒置".into()));
        }
        if let Some(sequence) = previous.sequence {
            if self.sequence < sequence {
                return Err(bad(format!(
                    "catalog 序号倒退：已有 {sequence}，收到 {}",
                    self.sequence
                )));
            }
        }
        if let Some(expires) = &previous.expires {
            let a = crate::m2::parse_utc(expires)
                .ok_or_else(|| bad("本地 catalog_state.expires 格式非法".into()))?;
            let b = crate::m2::parse_utc(&self.expires)
                .ok_or_else(|| bad("catalog expires 格式非法".into()))?;
            if b < a {
                return Err(bad("catalog 有效期时间倒退".into()));
            }
        }
        Ok(())
    }

    pub fn entry(&self, module_id: &str, variant: &str) -> Option<&CatalogEntry> {
        self.modules
            .iter()
            .find(|e| e.module_id == module_id && e.variant == variant)
    }
}

/// installed.json 的 catalog_state。无 catalog 的来源不写 last_seen_version。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogState {
    pub sequence: Option<u64>,
    pub expires: Option<String>,
    pub last_seen_version: BTreeMap<String, String>,
}

pub fn version_key(publisher_fp: &str, module_id: &str, variant: &str) -> String {
    format!("{publisher_fp}/{module_id}/{variant}")
}

impl CatalogState {
    /// 只在新 catalog 成功激活后调用，避免失败路径把单调水位推进。
    pub fn observe(&mut self, doc: &CatalogDocument) -> Result<(), M2Error> {
        self.sequence = Some(match self.sequence {
            Some(previous) => previous.max(doc.sequence),
            None => doc.sequence,
        });
        self.expires = Some(doc.expires.clone());
        for entry in &doc.modules {
            let key = version_key(&entry.publisher_fp, &entry.module_id, &entry.variant);
            let replace = match self.last_seen_version.get(&key) {
                Some(seen) => semver_cmp(&entry.version, seen)? == Ordering::Greater,
                None => true,
            };
            if replace {
                self.last_seen_version.insert(key, entry.version.clone());
            }
        }
        Ok(())
    }
}

/// 版本单调只在同 publisher_fp/module_id/variant 内比较；跨 variant 不比较。
pub fn check_update_allowed(
    doc: &CatalogDocument,
    state: &CatalogState,
    entry: &CatalogEntry,
    local_version: &str,
) -> Result<(), SourceError> {
    let bad = |reason: String| SourceError::CatalogInvalid { reason };
    let key = version_key(&entry.publisher_fp, &entry.module_id, &entry.variant);
    if let Some(seen) = state.last_seen_version.get(&key) {
        if semver_cmp(&entry.version, seen).map_err(|e| bad(e.to_string()))? == Ordering::Less {
            return Err(bad(format!(
                "版本回退：已见 {seen}，收到 {}",
                entry.version
            )));
        }
    }
    if let Some(min) = &entry.min_version {
        if semver_cmp(local_version, min).map_err(|e| bad(e.to_string()))? == Ordering::Less {
            return Err(bad(format!("本地版本低于 min_version {min}")));
        }
    }
    let revoked_here = doc
        .revoked
        .iter()
        .any(|r| r.id == entry.version || r.id == entry.sha256)
        || entry
            .revoked
            .iter()
            .any(|id| id == &entry.version || id == &entry.sha256);
    if revoked_here {
        return Err(bad(format!("{} 的版本或包已被撤销", entry.module_id)));
    }
    Ok(())
}

/// 两源裁决：签名有效、序号不倒退、时间不倒退；单源 Unavailable 不中断。
pub fn select_catalog(
    candidates: &[(u32, Result<CatalogDocument, SourceError>)],
    previous: &CatalogState,
    now: &str,
) -> Result<CatalogDocument, SourceError> {
    let mut usable: Vec<(u32, CatalogDocument)> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    for (priority, result) in candidates {
        match result {
            Ok(doc) => match doc.validate(previous, now) {
                Ok(()) => usable.push((*priority, doc.clone())),
                Err(e) => failures.push(format!(
                    "{}: {}",
                    doc.sources.first().map(|s| s.id.as_str()).unwrap_or("?"),
                    e.reason()
                )),
            },
            Err(SourceError::Unavailable { source_id, reason }) => {
                failures.push(format!("{source_id} 不可用：{reason}"));
            }
            Err(e) => failures.push(e.reason().to_string()),
        }
    }
    usable
        .into_iter()
        .max_by(|(pa, a), (pb, b)| a.sequence.cmp(&b.sequence).then_with(|| pb.cmp(pa)))
        .map(|(_, doc)| doc)
        .ok_or_else(|| SourceError::CatalogInvalid {
            reason: format!("没有可用的已验签 catalog：{}", failures.join("；")),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_key_scopes_variant_and_publisher() {
        let a = version_key("sha256:a", "dounai/omninexus", "stable");
        let b = version_key("sha256:a", "dounai/omninexus", "agent");
        assert_ne!(a, b);
    }

    #[test]
    fn observe_only_raises_seen_version() {
        let mut state = CatalogState::default();
        state
            .last_seen_version
            .insert("fp/m/v".into(), "1.2.0".into());
        let mut doc = CatalogDocument {
            schema_version: 2,
            sequence: 1,
            generated_at: "2026-01-01T00:00:00Z".into(),
            expires: "2026-01-02T00:00:00Z".into(),
            sources: vec![],
            modules: vec![],
            revoked: vec![],
            signature: CatalogSignature {
                algo: SUPPORTED_ALGO.into(),
                key_id: K5_KEY_ID.into(),
                signed_at: "2026-01-01T00:00:00Z".into(),
                value: String::new(),
            },
            raw: serde_json::Value::Null,
        };
        doc.modules.push(CatalogEntry {
            module_id: "m".into(),
            variant: "v".into(),
            version: "1.1.0".into(),
            role: Role::Llm,
            name: "m".into(),
            publisher_fp: "fp".into(),
            sha256: format!("sha256:{}", "a".repeat(64)),
            size: 1,
            sources: vec![],
            signature_required: true,
            expires: "2026-01-02T00:00:00Z".into(),
            expected_signer_fp: None,
            min_version: None,
            revoked: vec![],
        });
        state.observe(&doc).unwrap();
        assert_eq!(state.last_seen_version["fp/m/v"], "1.2.0");
        doc.modules[0].version = "1.3.0".into();
        state.observe(&doc).unwrap();
        assert_eq!(state.last_seen_version["fp/m/v"], "1.3.0");
    }
}
