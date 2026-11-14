//! state/sources.json：catalog 源注册表与每源新鲜度水位。
//!
//! 与 `trust.rs` 物理隔离：这里只放「签 catalog 的公钥」，包签名公钥的信任
//! 判定完全不读本文件。验 catalog 的公钥只来自这张本地表，绝不采信 catalog
//! 自述的 `signature.pubkey`——否则任何能托管一个 JSON 的人都能自证。
//!
//! 官方主源与镜像是同一份 catalog 的两个副本，共享 `official` 水位组；
//! 不同发布者的源各自独立记 `sequence` / `expires`，互不比较。

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::m2::catalog::SourceWatermark;
use crate::m2::source::{
    host_allowed, url_host, GITHUB_HOST_SUFFIXES, SOURCEFORGE_HOST_SUFFIXES,
};
use crate::m2::{fingerprint, is_fingerprint, M2Error};

pub const OFFICIAL_GROUP: &str = "official";
pub const OFFICIAL_SF_ID: &str = "official-sf";
pub const OFFICIAL_GH_ID: &str = "official-gh";

/// 从 base_url 派生出的后缀不能是公共后缀，否则等于「任意 https 都能拉」。
const PUBLIC_SUFFIXES: &[&str] = &[
    "com", "net", "org", "io", "cn", "co", "top", "xyz", "site", "online", "dev", "app",
    "info", "biz", "me", "tv", "cc", "uk", "us", "jp", "de", "ru",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceClass {
    Official,
    Community,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceEntry {
    pub id: String,
    pub name: String,
    pub kind: SourceClass,
    pub base_url: String,
    /// Ed25519 公钥原始 32 字节的 base64，用户带外导入。
    pub catalog_pubkey: String,
    /// 与 `catalog_pubkey` 重算结果必须一致，界面展示与判定用同一个值。
    pub catalog_fingerprint: String,
    pub host_suffixes: Vec<String>,
    pub enabled: bool,
    pub deletable: bool,
    pub auto_update: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl SourceEntry {
    pub fn group_id(&self) -> &str {
        self.group.as_deref().unwrap_or(&self.id)
    }

    /// 该源是否已被撤销公钥。撤销是停用，不是删除。
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRegistry {
    pub schema_version: u8,
    pub sources: Vec<SourceEntry>,
}

impl SourceRegistry {
    /// 内置两个官方源，共用 K5 公钥，可禁用不可删除。
    pub fn builtin(k5_pubkey_b64: &str, k5_fingerprint: &str) -> Result<Self, M2Error> {
        let common = |id: &str, name: &str, base_url: &str, hosts: &[&str], priority: u32| {
            SourceEntry {
                id: id.into(),
                name: name.into(),
                kind: SourceClass::Official,
                base_url: base_url.into(),
                catalog_pubkey: k5_pubkey_b64.into(),
                catalog_fingerprint: k5_fingerprint.into(),
                host_suffixes: hosts.iter().map(|s| (*s).to_string()).collect(),
                enabled: true,
                deletable: false,
                auto_update: true,
                group: Some(OFFICIAL_GROUP.into()),
                sequence: None,
                expires: None,
                priority: Some(priority),
                added_at: None,
                revoked_at: None,
                note: None,
            }
        };
        let registry = Self {
            schema_version: 1,
            sources: vec![
                common(
                    OFFICIAL_SF_ID,
                    "官方源（SourceForge）",
                    "https://downloads.sourceforge.net/project/dounai-releases",
                    SOURCEFORGE_HOST_SUFFIXES,
                    1,
                ),
                common(
                    OFFICIAL_GH_ID,
                    "官方镜像（GitHub）",
                    "https://raw.githubusercontent.com/Tonyjh07/dounai-releases/main",
                    GITHUB_HOST_SUFFIXES,
                    2,
                ),
            ],
        };
        registry.validate()?;
        Ok(registry)
    }

    pub fn load(path: &Path) -> Result<Self, M2Error> {
        let registry: Self = crate::state::read_json(path)?;
        registry.validate()?;
        Ok(registry)
    }

    pub fn save(&self, path: &Path) -> Result<(), M2Error> {
        self.validate()?;
        crate::state::write_json(path, self)?;
        Ok(())
    }

    pub fn validate(&self) -> Result<(), M2Error> {
        let bad = |reason: String| M2Error::Invalid(reason);
        if self.schema_version != 1 {
            return Err(bad("sources.json schema_version 必须为 1".into()));
        }
        if self.sources.is_empty() {
            return Err(bad("源注册表至少要有一个源".into()));
        }
        let mut ids = BTreeSet::new();
        for source in &self.sources {
            if !valid_id(&source.id) || !ids.insert(source.id.clone()) {
                return Err(bad(format!("源 id 非法或重复：{}", source.id)));
            }
            if source.name.trim().is_empty() {
                return Err(bad(format!("源 {} 缺少名称", source.id)));
            }
            if let Some(group) = &source.group {
                if !valid_id(group) {
                    return Err(bad(format!("源 {} 的水位组名非法", source.id)));
                }
            }
            if !is_fingerprint(&source.catalog_fingerprint) {
                return Err(bad(format!("源 {} 的指纹格式非法", source.id)));
            }
            let actual = fingerprint(&source.catalog_pubkey)
                .map_err(|e| bad(format!("源 {} 的公钥不合法：{e}", source.id)))?;
            if actual != source.catalog_fingerprint {
                return Err(bad(format!(
                    "源 {} 的公钥与登记指纹不符：界面会显示 A、判定却用 B",
                    source.id
                )));
            }
            let host = url_host(&source.base_url)
                .ok_or_else(|| bad(format!("源 {} 的 base_url 必须是 https", source.id)))?;
            if source.host_suffixes.is_empty() {
                return Err(bad(format!("源 {} 没有出网白名单", source.id)));
            }
            for suffix in &source.host_suffixes {
                validate_suffix(suffix).map_err(|reason| bad(format!("源 {}：{reason}", source.id)))?;
            }
            let suffixes: Vec<&str> = source.host_suffixes.iter().map(String::as_str).collect();
            if !host_allowed(host, &suffixes) {
                return Err(bad(format!(
                    "源 {} 的 base_url 主机 {} 不在自己的白名单内",
                    source.id, host
                )));
            }
            if source.kind == SourceClass::Official && source.deletable {
                return Err(bad(format!("内置官方源 {} 不可删除", source.id)));
            }
            if source.kind == SourceClass::Community && source.auto_update {
                return Err(bad(format!("社区源 {} 不得自动更新", source.id)));
            }
            if source.is_revoked() && source.enabled {
                return Err(bad(format!("源 {} 的公钥已撤销却仍处于启用状态", source.id)));
            }
        }
        Ok(())
    }

    pub fn find(&self, id: &str) -> Option<&SourceEntry> {
        self.sources.iter().find(|s| s.id == id)
    }

    pub fn find_mut(&mut self, id: &str) -> Option<&mut SourceEntry> {
        self.sources.iter_mut().find(|s| s.id == id)
    }

    pub fn enabled(&self) -> Vec<&SourceEntry> {
        self.sources.iter().filter(|s| s.enabled).collect()
    }

    /// 只有已启用源的后缀进入 allowlist；禁用全部源即得到空集。
    pub fn allowlist(&self) -> Vec<String> {
        let mut hosts: BTreeSet<String> = BTreeSet::new();
        for source in self.enabled() {
            hosts.extend(source.host_suffixes.iter().cloned());
        }
        hosts.into_iter().collect()
    }

    /// 同一水位组内所有源 id。官方两源互为副本，包下载地址可共用；
    /// 社区源组内只有自己，绝不跨发布者回落。
    pub fn group_ids(&self, id: &str) -> Vec<String> {
        let group = self.find(id).map(SourceEntry::group_id).unwrap_or(id);
        self.sources
            .iter()
            .filter(|s| s.group_id() == group)
            .map(|s| s.id.clone())
            .collect()
    }

    /// 组内已见过的最大序号与最大过期时间，作为该组下一步裁决的基线。
    pub fn watermark(&self, id: &str) -> SourceWatermark {
        let group = self.find(id).map(SourceEntry::group_id).unwrap_or(id);
        let mut watermark = SourceWatermark::default();
        for source in self.sources.iter().filter(|s| s.group_id() == group) {
            if let Some(sequence) = source.sequence {
                watermark.sequence = Some(watermark.sequence.map_or(sequence, |v| v.max(sequence)));
            }
            if let Some(expires) = &source.expires {
                let raise = watermark
                    .expires
                    .as_deref()
                    .map(|current| expires.as_str() > current)
                    .unwrap_or(true);
                if raise {
                    watermark.expires = Some(expires.clone());
                }
            }
        }
        watermark
    }

    /// 一份 catalog 成功激活后推进整组水位，避免镜像的旧副本再次参与裁决。
    pub fn advance(&mut self, id: &str, sequence: u64, expires: &str) {
        let group = self.find(id).map(SourceEntry::group_id).unwrap_or(id).to_string();
        for source in self.sources.iter_mut().filter(|s| s.group_id() == group) {
            source.sequence = Some(source.sequence.map_or(sequence, |v| v.max(sequence)));
            let raise = source
                .expires
                .as_deref()
                .map(|current| expires > current)
                .unwrap_or(true);
            if raise {
                source.expires = Some(expires.to_string());
            }
        }
    }

    pub fn add(&mut self, entry: SourceEntry) -> Result<(), M2Error> {
        if self.find(&entry.id).is_some() {
            return Err(M2Error::Invalid(format!("源 {} 已经存在", entry.id)));
        }
        self.sources.push(entry);
        self.validate()
    }

    pub fn remove(&mut self, id: &str) -> Result<(), M2Error> {
        let index = self
            .sources
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| M2Error::Invalid(format!("源 {id} 不存在")))?;
        if !self.sources[index].deletable {
            return Err(M2Error::Rejected(format!("源 {id} 是内置源，只能禁用不能删除")));
        }
        self.sources.remove(index);
        self.validate()
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), M2Error> {
        let source = self
            .find_mut(id)
            .ok_or_else(|| M2Error::Invalid(format!("源 {id} 不存在")))?;
        if enabled && source.is_revoked() {
            return Err(M2Error::Rejected(format!(
                "源 {id} 的 catalog 公钥已撤销，不能重新启用"
            )));
        }
        source.enabled = enabled;
        self.validate()
    }

    /// 撤销该源的 catalog 公钥：立即停用、记时间、不删除源与已装实例。
    pub fn revoke(&mut self, id: &str, at: &str) -> Result<(), M2Error> {
        let source = self
            .find_mut(id)
            .ok_or_else(|| M2Error::Invalid(format!("源 {id} 不存在")))?;
        source.enabled = false;
        source.revoked_at = Some(at.to_string());
        self.validate()
    }
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// 至少两段、小写域名字符、且不是公共后缀。
pub fn validate_suffix(suffix: &str) -> Result<(), String> {
    let lower = suffix.to_ascii_lowercase();
    if lower != suffix
        || lower.len() < 3
        || lower.split('.').count() < 2
        || lower.split('.').any(|part| {
            part.is_empty()
                || part.starts_with('-')
                || part.ends_with('-')
                || !part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
    {
        return Err(format!("非法域名后缀：{suffix}"));
    }
    if PUBLIC_SUFFIXES.contains(&lower.as_str()) {
        return Err(format!("不得把公共后缀 {suffix} 当白名单"));
    }
    Ok(())
}

/// 添加源时从 base_url 派生白名单：已知宿主给整套后缀，自建源取注册域。
/// 派生结果会在确认框里完整展示，用户确认后才落盘。
pub fn derive_host_suffixes(base_url: &str) -> Result<Vec<String>, M2Error> {
    let host = url_host(base_url)
        .ok_or_else(|| M2Error::Invalid("源地址必须是 https".into()))?
        .to_ascii_lowercase();
    if host_allowed(&host, GITHUB_HOST_SUFFIXES) {
        return Ok(GITHUB_HOST_SUFFIXES.iter().map(|s| (*s).to_string()).collect());
    }
    if host_allowed(&host, SOURCEFORGE_HOST_SUFFIXES) {
        return Ok(SOURCEFORGE_HOST_SUFFIXES
            .iter()
            .map(|s| (*s).to_string())
            .collect());
    }
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() < 2 {
        return Err(M2Error::Invalid(format!("无法从 {host} 派生域名后缀")));
    }
    let derived = parts[parts.len() - 2..].join(".");
    validate_suffix(&derived).map_err(M2Error::Invalid)?;
    Ok(vec![derived])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::m2::fingerprint;
    use base64::Engine as _;
    use ed25519_dalek::SigningKey;

    fn key(seed: u8) -> (String, String) {
        let sk = SigningKey::from_bytes(&[seed; 32]);
        let pk = base64::engine::general_purpose::STANDARD.encode(sk.verifying_key().to_bytes());
        let fp = fingerprint(&pk).unwrap();
        (pk, fp)
    }

    fn registry() -> SourceRegistry {
        let (pk, fp) = key(5);
        SourceRegistry::builtin(&pk, &fp).unwrap()
    }

    fn community(id: &str) -> SourceEntry {
        let (pk, fp) = key(9);
        SourceEntry {
            id: id.into(),
            name: "Demo".into(),
            kind: SourceClass::Community,
            base_url: "https://example.org/catalog".into(),
            catalog_pubkey: pk,
            catalog_fingerprint: fp,
            host_suffixes: vec!["example.org".into()],
            enabled: true,
            deletable: true,
            auto_update: false,
            group: None,
            sequence: None,
            expires: None,
            priority: Some(3),
            added_at: None,
            revoked_at: None,
            note: None,
        }
    }

    #[test]
    fn builtin_registry_has_two_undeletable_sources_sharing_one_watermark() {
        let registry = registry();
        assert_eq!(registry.sources.len(), 2);
        assert!(registry.sources.iter().all(|s| !s.deletable && s.enabled));
        assert_eq!(registry.group_ids(OFFICIAL_SF_ID).len(), 2);
        assert_eq!(registry.watermark(OFFICIAL_GH_ID), SourceWatermark::default());
        assert_eq!(registry.allowlist().len(), 4);
    }

    #[test]
    fn public_key_must_match_its_registered_fingerprint() {
        let mut registry = registry();
        registry.sources[0].catalog_fingerprint = format!("sha256:{}", "a".repeat(64));
        assert!(registry.validate().is_err());
    }

    #[test]
    fn official_sources_cannot_be_deleted_but_can_be_disabled() {
        let mut registry = registry();
        assert!(registry.remove(OFFICIAL_SF_ID).is_err());
        registry.set_enabled(OFFICIAL_SF_ID, false).unwrap();
        assert_eq!(registry.enabled().len(), 1);
        registry.set_enabled(OFFICIAL_SF_ID, true).unwrap();
        assert_eq!(registry.enabled().len(), 2);
    }

    #[test]
    fn community_watermark_is_independent_from_official() {
        let mut registry = registry();
        registry.add(community("demo")).unwrap();
        registry.advance("demo", 9999, "2026-12-01T00:00:00Z");
        assert_eq!(registry.watermark("demo").sequence, Some(9999));
        assert_eq!(registry.watermark(OFFICIAL_SF_ID).sequence, None);
        assert_eq!(registry.group_ids("demo"), vec!["demo".to_string()]);
    }

    #[test]
    fn revoking_a_key_disables_the_source_and_blocks_re_enable() {
        let mut registry = registry();
        registry.add(community("demo")).unwrap();
        registry.revoke("demo", "2026-11-14T00:00:00Z").unwrap();
        assert!(!registry.find("demo").unwrap().enabled);
        assert!(!registry.allowlist().contains(&"example.org".to_string()));
        assert!(registry.set_enabled("demo", true).is_err());
        registry.remove("demo").unwrap();
        assert!(registry.find("demo").is_none());
    }

    #[test]
    fn derived_suffixes_never_allow_a_public_suffix() {
        assert_eq!(
            derive_host_suffixes("https://downloads.sourceforge.net/project/x").unwrap(),
            SOURCEFORGE_HOST_SUFFIXES
                .iter()
                .map(|s| (*s).to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            derive_host_suffixes("https://raw.githubusercontent.com/x/y").unwrap(),
            GITHUB_HOST_SUFFIXES
                .iter()
                .map(|s| (*s).to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            derive_host_suffixes("https://releases.example.org/catalog").unwrap(),
            vec!["example.org".to_string()]
        );
        assert!(derive_host_suffixes("https://localhost/catalog").is_err());
        assert!(derive_host_suffixes("http://example.org/catalog").is_err());
        assert!(validate_suffix("com").is_err());
        assert!(validate_suffix("EXAMPLE.ORG").is_err());
    }

    #[test]
    fn allowlist_only_contains_enabled_sources() {
        let mut registry = registry();
        registry.add(community("demo")).unwrap();
        assert!(registry.allowlist().contains(&"example.org".to_string()));
        registry.set_enabled("demo", false).unwrap();
        assert!(!registry.allowlist().contains(&"example.org".to_string()));
    }
}
