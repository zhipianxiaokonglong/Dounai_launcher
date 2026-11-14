//! 各源登记公钥签名的 catalog、官方双副本裁决与新鲜度。
//!
//! 不得回退到未验签的旧缓存：任一来源验签失败、序号倒退或过期，都按
//! `CatalogInvalid` 处理。序号与过期水位按源（官方主/镜像共享一组）独立
//! 记在 sources.json；installed.json 的 catalog_state 只留 last_seen_version。

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::m2::manifest::valid_signature_value;
use crate::m2::source::SourceError;
use crate::m2::{
    is_expired, is_fingerprint, is_hash, semver_cmp, within_catalog_window, M2Error,
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
    /// 展示用槽位标识，不参与判定。官方写 K5，社区源写自己的标识。
    pub key_id: String,
    /// 签名者公钥指纹，必须等于该源在 sources.json 登记的指纹。
    pub signer_fp: String,
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
    pub min_launcher: Option<String>,
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
    /// 用 sources.json 里该源登记的公钥验签，`expected_fp` 是同一条记录的指纹。
    /// 签名覆盖剔除 `signature` 后的整份规范化 JSON；catalog 自述的 signer_fp
    /// 只用于比对，公钥绝不从文档里取。
    pub fn parse(
        text: &str,
        pubkey_b64: &str,
        expected_fp: &str,
    ) -> Result<Self, SourceError> {
        let bad = |reason: String| SourceError::CatalogInvalid { reason };
        let raw: serde_json::Value =
            serde_json::from_str(text).map_err(|e| bad(format!("catalog 不是合法 JSON：{e}")))?;
        let mut doc: Self = serde_json::from_value(raw.clone())
            .map_err(|e| bad(format!("catalog 字段不合法：{e}")))?;
        doc.raw = raw;
        if doc.schema_version != 2 {
            return Err(bad("schema_version 必须为 2".into()));
        }
        // 契约层 additionalProperties=false 会拒，但 serde 默认忽略未知字段，
        // 运行时也必须显式挡一次：公钥只来自本地源表。
        if doc
            .raw
            .get("signature")
            .and_then(|s| s.get("pubkey"))
            .is_some()
        {
            return Err(bad(
                "catalog 不得自述 pubkey，验签公钥只来自 sources.json".into(),
            ));
        }
        if doc.sequence < 1 {
            return Err(bad("sequence 必须不小于 1".into()));
        }
        if doc.signature.algo != SUPPORTED_ALGO {
            return Err(bad("catalog 签名算法标识不合法".into()));
        }
        if !is_fingerprint(&doc.signature.signer_fp) || doc.signature.signer_fp != expected_fp {
            return Err(bad(format!(
                "catalog 签名者指纹与源登记值不符：文档 {}，登记 {expected_fp}",
                doc.signature.signer_fp
            )));
        }
        if !valid_signature_value(&doc.signature.value) {
            return Err(bad("catalog 签名值格式非法".into()));
        }
        crate::signature::verify_manifest(&doc.raw, pubkey_b64)
            .map_err(|e| bad(format!("该源公钥验签失败：{e}")))?;
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

    /// 新鲜度、序号与时间单调裁决。旧缓存一律不参与。水位按源（或官方组）传入。
    pub fn validate(&self, previous: &SourceWatermark, now: &str) -> Result<(), SourceError> {
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

/// installed.json 的 catalog_state。M8 起只承载版本单调水位；
/// `sequence` / `expires` 保留字段只为读旧文件，写入时一律置空、不参与判定。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<String>,
    pub last_seen_version: BTreeMap<String, String>,
}

/// 每源（或官方双副本共享的一组）独立的新鲜度水位，落在 sources.json。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceWatermark {
    pub sequence: Option<u64>,
    pub expires: Option<String>,
}

/// 会话缓存的主键：跨源同名、同 variant、多个版本都能精确定位，不做隐式优选。
pub fn entry_key(source_id: &str, module_id: &str, variant: &str, version: &str) -> String {
    format!("{source_id}::{module_id}@{variant}@{version}")
}

pub fn version_key(publisher_fp: &str, module_id: &str, variant: &str) -> String {
    format!("{publisher_fp}/{module_id}/{variant}")
}

impl CatalogState {
    /// 只在该源 catalog 成功激活后调用，避免失败路径把版本水位推进。
    /// 序号与过期由 `SourceRegistry::advance` 单独推进。
    pub fn observe(&mut self, doc: &CatalogDocument) -> Result<(), M2Error> {
        self.sequence = None;
        self.expires = None;
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

/// 官方主源与镜像的二选一裁决：签名有效、序号不倒退、时间不倒退；
/// 单源 Unavailable 不中断。不同发布者的源不走这里，各自独立裁决后并列展示。
pub fn select_catalog(
    candidates: &[(u32, Result<CatalogDocument, SourceError>)],
    previous: &SourceWatermark,
    now: &str,
) -> Result<CatalogDocument, SourceError> {
    select_catalog_entry(candidates, previous, now).map(|(_, doc)| doc)
}

/// 同 `select_catalog`，但返回胜出副本的 priority，供命令面标注来源。
pub fn select_catalog_entry(
    candidates: &[(u32, Result<CatalogDocument, SourceError>)],
    previous: &SourceWatermark,
    now: &str,
) -> Result<(u32, CatalogDocument), SourceError> {
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
        .ok_or_else(|| SourceError::CatalogInvalid {
            reason: format!("没有可用的已验签 catalog：{}", failures.join("；")),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use ed25519_dalek::SigningKey;
    use serde_json::json;

    fn sample_doc() -> CatalogDocument {
        CatalogDocument {
            schema_version: 2,
            sequence: 1,
            generated_at: "2026-01-01T00:00:00Z".into(),
            expires: "2026-01-02T00:00:00Z".into(),
            sources: vec![],
            modules: vec![],
            revoked: vec![],
            signature: CatalogSignature {
                algo: SUPPORTED_ALGO.into(),
                key_id: "K5".into(),
                signer_fp: format!("sha256:{}", "a".repeat(64)),
                signed_at: "2026-01-01T00:00:00Z".into(),
                value: String::new(),
            },
            raw: serde_json::Value::Null,
        }
    }

    fn sample_entry() -> CatalogEntry {
        CatalogEntry {
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
            min_launcher: None,
            revoked: vec![],
        }
    }

    #[test]
    fn version_key_scopes_variant_and_publisher() {
        let a = version_key("sha256:a", "dounai/omninexus", "stable");
        let b = version_key("sha256:a", "dounai/omninexus", "agent");
        assert_ne!(a, b);
    }

    #[test]
    fn entry_key_scopes_source_variant_and_version() {
        let a = entry_key("official-sf", "dounai/omninexus", "stable", "1.0.0");
        let b = entry_key("community-demo", "dounai/omninexus", "stable", "1.0.0");
        let c = entry_key("official-sf", "dounai/omninexus", "stable", "1.1.0");
        assert_ne!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn observe_only_raises_seen_version() {
        let mut state = CatalogState::default();
        state
            .last_seen_version
            .insert("fp/m/v".into(), "1.2.0".into());
        state.sequence = Some(7);
        state.expires = Some("2026-01-01T00:00:00Z".into());
        let mut doc = sample_doc();
        doc.modules.push(sample_entry());
        state.observe(&doc).unwrap();
        assert_eq!(state.last_seen_version["fp/m/v"], "1.2.0");
        assert_eq!(state.sequence, None);
        assert_eq!(state.expires, None);
        doc.modules[0].version = "1.3.0".into();
        state.observe(&doc).unwrap();
        assert_eq!(state.last_seen_version["fp/m/v"], "1.3.0");
    }

    /// 验签公钥来自本地源表，catalog 自述的指纹不符必须硬拒，
    /// 且不能靠塞一个 pubkey 字段绕过（schema 的 additionalProperties=false 会先拦）。
    #[test]
    fn parse_rejects_a_signer_fingerprint_that_differs_from_the_registry() {
        let sk = SigningKey::from_bytes(&[3; 32]);
        let pk = base64::engine::general_purpose::STANDARD.encode(sk.verifying_key().to_bytes());
        let registered = crate::m2::fingerprint(&pk).unwrap();
        let raw = json!({
            "schema_version": 2, "sequence": 1,
            "generated_at": "2026-01-01T00:00:00Z", "expires": "2026-01-02T00:00:00Z",
            "sources": [{ "id": "demo", "kind": "https", "base_url": "https://example.org", "priority": 1 }],
            "modules": [], "revoked": [],
            "signature": {
                "algo": "Ed25519", "key_id": "community-k1",
                "signer_fp": format!("sha256:{}", "b".repeat(64)),
                "signed_at": "2026-01-01T00:00:00Z",
                "value": format!("{}===", "A".repeat(83))
            }
        });
        let error = CatalogDocument::parse(&raw.to_string(), &pk, &registered).unwrap_err();
        assert!(error.reason().contains("指纹与源登记值不符"), "{}", error.reason());
    }

    #[test]
    fn per_source_watermarks_do_not_interfere() {
        let doc = sample_doc();
        let official = SourceWatermark::default();
        let community = SourceWatermark {
            sequence: Some(9999),
            expires: None,
        };
        assert!(doc.validate(&official, "2026-01-01T12:00:00Z").is_ok());
        assert!(doc.validate(&community, "2026-01-01T12:00:00Z").is_err());
    }
}
