//! trust.json、A/B/C/D 四级判定、公钥 TOFU 与撤销。
//!
//! 信任单位是公钥，不是源。判定顺序固定：先用 `signature.pubkey` 验签，
//! 再由公钥字节算指纹，最后查信任表。验不过的公钥一律 D 类硬拒，不给确认入口。

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::m2::catalog::CatalogEntry;
use crate::m2::manifest::PackageManifest;
use crate::m2::{fingerprint, is_fingerprint, M2Error, Verdict};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyPurpose {
    ModuleSigning,
    CatalogSigning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustOrigin {
    Tofu,
    Import,
    Builtin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltinKey {
    pub key_id: String,
    pub fingerprint: String,
    pub purpose: KeyPurpose,
    pub enabled: bool,
    /// 契约固定为 false：可见、可禁用、不可删除。
    pub deletable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedKey {
    pub fingerprint: String,
    pub label: String,
    pub confirmed_at: String,
    pub enabled: bool,
    pub origin: TrustOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustStore {
    pub schema_version: u8,
    pub builtin: Vec<BuiltinKey>,
    pub trusted: Vec<TrustedKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<String>,
}

impl TrustStore {
    /// K1 与 K5 的真实指纹由编译期内嵌值传入，M5 负责生成与轮换。
    pub fn new(k1_fingerprint: &str, k5_fingerprint: &str) -> Result<Self, M2Error> {
        for fp in [k1_fingerprint, k5_fingerprint] {
            if !is_fingerprint(fp) {
                return Err(M2Error::Invalid(format!("内嵌公钥指纹非法：{fp}")));
            }
        }
        Ok(Self {
            schema_version: 1,
            builtin: vec![
                BuiltinKey {
                    key_id: "K1".into(),
                    fingerprint: k1_fingerprint.into(),
                    purpose: KeyPurpose::ModuleSigning,
                    enabled: true,
                    deletable: false,
                },
                BuiltinKey {
                    key_id: "K5".into(),
                    fingerprint: k5_fingerprint.into(),
                    purpose: KeyPurpose::CatalogSigning,
                    enabled: true,
                    deletable: false,
                },
            ],
            trusted: Vec::new(),
            last_updated: None,
        })
    }

    pub fn load(path: &Path) -> Result<Self, M2Error> {
        let store: Self = crate::state::read_json(path)?;
        store.validate()?;
        Ok(store)
    }

    pub fn save(&self, path: &Path) -> Result<(), M2Error> {
        self.validate()?;
        crate::state::write_json(path, self)?;
        Ok(())
    }

    pub fn validate(&self) -> Result<(), M2Error> {
        if self.schema_version != 1 {
            return Err(M2Error::Invalid(
                "trust.json schema_version 必须为 1".into(),
            ));
        }
        if self.builtin.len() < 2
            || !self.builtin.iter().any(|k| k.key_id == "K1")
            || !self.builtin.iter().any(|k| k.key_id == "K5")
        {
            return Err(M2Error::Invalid("内嵌公钥必须同时含 K1 与 K5".into()));
        }
        for key in &self.builtin {
            if key.deletable || !is_fingerprint(&key.fingerprint) {
                return Err(M2Error::Invalid("内嵌公钥不可删除且指纹必须完整".into()));
            }
        }
        for key in &self.trusted {
            if !is_fingerprint(&key.fingerprint) {
                return Err(M2Error::Invalid("信任指纹格式非法".into()));
            }
        }
        Ok(())
    }

    pub fn is_trusted(&self, fp: &str) -> bool {
        self.builtin
            .iter()
            .any(|k| k.fingerprint == fp && k.enabled)
            || self
                .trusted
                .iter()
                .any(|k| k.fingerprint == fp && k.enabled)
    }

    pub fn is_revoked(&self, fp: &str) -> bool {
        self.trusted
            .iter()
            .any(|k| k.fingerprint == fp && !k.enabled)
    }

    /// B 类确认时记住该公钥。已撤销的公钥不能靠再次确认恢复。
    pub fn tofu(&mut self, fp: &str, label: &str, at: &str) -> Result<(), M2Error> {
        if !is_fingerprint(fp) {
            return Err(M2Error::Invalid("TOFU 指纹格式非法".into()));
        }
        if self.is_revoked(fp) {
            return Err(M2Error::Rejected("该公钥已被撤销，不能重新确认".into()));
        }
        if self.is_trusted(fp) {
            return Ok(());
        }
        self.trusted.push(TrustedKey {
            fingerprint: fp.into(),
            label: label.into(),
            confirmed_at: at.into(),
            enabled: true,
            origin: TrustOrigin::Tofu,
            revoked_at: None,
            note: None,
        });
        self.last_updated = Some(at.into());
        Ok(())
    }

    /// 撤销只禁用，不卸载已装实例；上层据此禁止自动更新。
    pub fn revoke(&mut self, fp: &str, at: &str) -> bool {
        let mut changed = false;
        for key in &mut self.trusted {
            if key.fingerprint == fp && key.enabled {
                key.enabled = false;
                key.revoked_at = Some(at.into());
                changed = true;
            }
        }
        for key in &mut self.builtin {
            if key.fingerprint == fp && key.enabled {
                key.enabled = false;
                changed = true;
            }
        }
        if changed {
            self.last_updated = Some(at.into());
        }
        changed
    }
}

#[derive(Debug, Clone, Default)]
pub struct ClassifyContext<'a> {
    pub catalog_entry: Option<&'a CatalogEntry>,
    /// 首次本地导入时用户填写的期望指纹。
    pub expected_signer_fp: Option<&'a str>,
    /// 已装实例的发布者指纹，用于更新时阻断指纹变更。
    pub previous_publisher_fp: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    pub verdict: Verdict,
    pub publisher_fp: Option<String>,
    pub signature_present: bool,
    pub reason: String,
}

impl Classification {
    fn hard(reason: impl Into<String>, present: bool) -> Self {
        Self {
            verdict: Verdict::D,
            publisher_fp: None,
            signature_present: present,
            reason: reason.into(),
        }
    }
}

/// 四级判定。返回 Ok(D) 是正常判定结果，不是程序错误。
pub fn classify(
    store: &TrustStore,
    manifest: &PackageManifest,
    ctx: &ClassifyContext<'_>,
) -> Result<Classification, M2Error> {
    let Some(sig) = manifest.signature.as_ref().filter(|s| s.is_signed()) else {
        // 删掉签名段的降级攻击：catalog 声明、期望指纹、历史指纹任一存在即硬拒。
        let required = ctx.catalog_entry.is_some_and(|e| e.signature_required);
        if required || ctx.expected_signer_fp.is_some() || ctx.previous_publisher_fp.is_some() {
            return Ok(Classification::hard(
                "包缺少签名，但 catalog、期望指纹或历史记录要求必须签名",
                false,
            ));
        }
        return Ok(Classification {
            verdict: Verdict::C,
            publisher_fp: None,
            signature_present: false,
            reason: "包未签名，按未验证处理，需要单次确认".into(),
        });
    };

    if crate::signature::verify_manifest(&manifest.raw, &sig.pubkey).is_err() {
        return Ok(Classification::hard(
            "signature.pubkey 验签失败，硬拒且不提供确认入口",
            true,
        ));
    }
    let fp = fingerprint(&sig.pubkey)?;
    for expected in [
        ctx.catalog_entry
            .and_then(|e| e.expected_signer_fp.as_deref()),
        ctx.expected_signer_fp,
        ctx.previous_publisher_fp,
    ]
    .into_iter()
    .flatten()
    {
        if expected != fp {
            return Ok(Classification::hard(
                format!("签名者指纹不符：期望 {expected}，实际 {fp}"),
                true,
            ));
        }
    }
    if store.is_revoked(&fp) {
        return Ok(Classification::hard("发布者公钥已被撤销", true));
    }
    let verdict = if store.is_trusted(&fp) {
        Verdict::A
    } else {
        Verdict::B
    };
    Ok(Classification {
        verdict,
        publisher_fp: Some(fp.clone()),
        signature_present: true,
        reason: if verdict == Verdict::A {
            "公钥已登记且启用".into()
        } else {
            format!("公钥未登记，完整指纹 {fp}，确认前必须展示")
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::json;

    fn key(seed: u8) -> (SigningKey, String) {
        let sk = SigningKey::from_bytes(&[seed; 32]);
        let pk = base64::engine::general_purpose::STANDARD.encode(sk.verifying_key().to_bytes());
        (sk, pk)
    }

    fn signed_manifest(sk: &SigningKey, pk: &str) -> PackageManifest {
        let mut value = json!({
            "schema_version": 2,
            "module_id": "dounai/demo",
            "name": "demo",
            "version": "1.0.0",
            "variant": "stable",
            "role": "llm",
            "category": "optional",
            "publisher": { "id": "demo", "name": "Demo" },
            "requires": { "launcher": ">=1.0.0" },
            "runtime": { "python": ">=3.11", "venv": "{instance_venv}",
                         "deps": { "kind": "pyproject", "path": "pyproject.toml" } },
            "entry": { "command": "{venv_python} -m demo", "env": {}, "health": null },
            "ports": [8102],
            "permissions": { "network": [], "filesystem": [], "devices": [] },
            "files": {},
            "signature": { "algo": "Ed25519", "pubkey": pk,
                           "key_id": "K1", "signed_at": "2026-01-01T00:00:00Z", "value": "" }
        });
        let payload = crate::signature::canonical_payload(&value).unwrap();
        let sig = sk.sign(&payload);
        value["signature"]["value"] =
            json!(base64::engine::general_purpose::STANDARD.encode(sig.to_bytes()));
        PackageManifest::parse(&value.to_string()).unwrap()
    }

    #[test]
    fn unsigned_is_c_but_signature_required_forces_d() {
        let (_sk, pk) = key(1);
        let mut value: serde_json::Value = serde_json::from_str(
            &signed_manifest(&SigningKey::from_bytes(&[1; 32]), &pk)
                .raw
                .to_string(),
        )
        .unwrap();
        value["signature"]["value"] = json!("");
        let manifest = PackageManifest::parse(&value.to_string()).unwrap();
        let (_, k5) = key(2);
        let store =
            TrustStore::new(&fingerprint(&pk).unwrap(), &fingerprint(&k5).unwrap()).unwrap();
        let plain = classify(&store, &manifest, &ClassifyContext::default()).unwrap();
        assert_eq!(plain.verdict, Verdict::C);
        let entry = CatalogEntry {
            module_id: "dounai/demo".into(),
            variant: "stable".into(),
            version: "1.0.0".into(),
            role: crate::manifest::Role::Llm,
            name: "demo".into(),
            publisher_fp: fingerprint(&pk).unwrap(),
            sha256: format!("sha256:{}", "a".repeat(64)),
            size: 1,
            sources: vec![],
            signature_required: true,
            expires: "2026-01-08T00:00:00Z".into(),
            expected_signer_fp: None,
            min_version: None,
            revoked: vec![],
        };
        let forced = classify(
            &store,
            &manifest,
            &ClassifyContext {
                catalog_entry: Some(&entry),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(forced.verdict, Verdict::D);
    }

    #[test]
    fn unregistered_key_is_b_and_tofu_promotes_to_a() {
        let (sk, pk) = key(7);
        let manifest = signed_manifest(&sk, &pk);
        let (_, k5) = key(8);
        let mut store = TrustStore::new(
            &format!("sha256:{}", "a".repeat(64)),
            &fingerprint(&k5).unwrap(),
        )
        .unwrap();
        let first = classify(&store, &manifest, &ClassifyContext::default()).unwrap();
        assert_eq!(first.verdict, Verdict::B);
        let fp = first.publisher_fp.clone().unwrap();
        store
            .tofu(&fp, "Demo Publisher", "2026-01-01T00:00:00Z")
            .unwrap();
        let second = classify(&store, &manifest, &ClassifyContext::default()).unwrap();
        assert_eq!(second.verdict, Verdict::A);
        assert_eq!(store.trusted[0].origin, TrustOrigin::Tofu);
        assert!(store.revoke(&fp, "2026-01-02T00:00:00Z"));
        assert_eq!(
            classify(&store, &manifest, &ClassifyContext::default())
                .unwrap()
                .verdict,
            Verdict::D
        );
        assert!(store.tofu(&fp, "retry", "2026-01-03T00:00:00Z").is_err());
    }
}
