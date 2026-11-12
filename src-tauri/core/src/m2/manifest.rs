//! zip 发布包与 dir 目录实例共用的完整 module-v2 manifest。
//!
//! 环境引擎用的 `crate::manifest::ModuleManifestV2` 只取运行时字段；这里保留
//! 全字段与原始 JSON，因为签名覆盖的是剔除 `signature` 后的整份 manifest。

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::m2::{is_hash, reserved_env_key, valid_env_key, M2Error, SUPPORTED_ALGO};
use crate::manifest::{HealthSpec, Role, RuntimeSpec};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Publisher {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requires {
    pub launcher: String,
    #[serde(default)]
    pub contract: Option<String>,
    #[serde(default)]
    pub roles_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Permissions {
    pub network: Vec<String>,
    pub filesystem: Vec<String>,
    pub devices: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageEntry {
    pub command: String,
    #[serde(default)]
    pub working_dir: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub health: Option<HealthSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignatureBlock {
    pub algo: String,
    pub pubkey: String,
    pub key_id: String,
    pub signed_at: String,
    pub value: String,
}

impl SignatureBlock {
    /// 空 value 表示未签名，按 C 类处理。
    pub fn is_signed(&self) -> bool {
        !self.value.trim().is_empty()
    }
}

fn empty_value() -> serde_json::Value {
    serde_json::Value::Null
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageManifest {
    pub schema_version: u8,
    pub module_id: String,
    pub name: String,
    pub version: String,
    pub variant: String,
    pub role: Role,
    pub category: super::Category,
    pub publisher: Publisher,
    pub requires: Requires,
    pub entry: PackageEntry,
    pub ports: Vec<u16>,
    pub permissions: Permissions,
    pub files: BTreeMap<String, String>,
    #[serde(default)]
    pub runtime: Option<RuntimeSpec>,
    #[serde(default)]
    pub signature: Option<SignatureBlock>,
    #[serde(default)]
    pub depends_on: Vec<Role>,
    #[serde(default)]
    pub auxiliary_kind: Option<String>,
    /// 原始 JSON。验签必须用原文，不能丢失未知字段或改变键顺序语义。
    #[serde(skip, default = "empty_value")]
    pub raw: serde_json::Value,
}

impl PackageManifest {
    pub fn parse(text: &str) -> Result<Self, M2Error> {
        let raw: serde_json::Value = serde_json::from_str(text)?;
        let mut manifest: Self = serde_json::from_value(raw.clone())?;
        manifest.raw = raw;
        manifest.validate_shape()?;
        Ok(manifest)
    }

    /// 契约层用 ajv 做完整校验；这里挡住会影响判定与落盘的关键约束。
    pub fn validate_shape(&self) -> Result<(), M2Error> {
        if self.schema_version != 2 {
            return Err(M2Error::Invalid("schema_version 必须为 2".into()));
        }
        if !valid_module_id(&self.module_id) {
            return Err(M2Error::Invalid(format!(
                "module_id 必须是 namespace/name：{}",
                self.module_id
            )));
        }
        crate::m2::semver_cmp(&self.version, "0.0.0")?;
        if !valid_variant(&self.variant) {
            return Err(M2Error::Invalid(format!("variant 非法：{}", self.variant)));
        }
        for banned in [
            "instance_id",
            "source_kind",
            "publisher_fp",
            "trust",
            "official",
        ] {
            if self.raw.get(banned).is_some() {
                return Err(M2Error::Invalid(format!("manifest 不得自述 {banned}")));
            }
        }
        if self.role == Role::Shell {
            if !self.entry.command.contains("{install_root}") {
                return Err(M2Error::Invalid(
                    "shell 的 command 必须含 {install_root}".into(),
                ));
            }
        } else {
            if !self.entry.command.contains("{venv_python}") {
                return Err(M2Error::Invalid(
                    "数据面 command 必须含 {venv_python}".into(),
                ));
            }
            let runtime = self
                .runtime
                .as_ref()
                .ok_or_else(|| M2Error::Invalid("非 shell 实例必须有 runtime".into()))?;
            validate_relative_path(&runtime.deps.path)?;
        }
        if (self.role == Role::Auxiliary) != self.auxiliary_kind.is_some() {
            return Err(M2Error::Invalid(
                "auxiliary_kind 只能由 auxiliary 实例声明".into(),
            ));
        }
        for key in self.entry.env.keys() {
            if !valid_env_key(key) || reserved_env_key(key) {
                return Err(M2Error::Invalid(format!("非法的环境变量键名：{key}")));
            }
        }
        for (path, hash) in &self.files {
            validate_relative_path(path)?;
            if !is_hash(hash) {
                return Err(M2Error::Invalid(format!("files 哈希格式非法：{path}")));
            }
        }
        if let Some(sig) = &self.signature {
            if sig.algo != SUPPORTED_ALGO {
                return Err(M2Error::Invalid("签名算法不受支持".into()));
            }
            crate::m2::fingerprint(&sig.pubkey)?;
            if !sig.is_signed() {
                // 空 value 合法，表示未签名。
            } else if !valid_signature_value(&sig.value) {
                return Err(M2Error::Invalid("签名值格式非法".into()));
            }
        }
        Ok(())
    }

    pub fn is_signed(&self) -> bool {
        self.signature
            .as_ref()
            .is_some_and(SignatureBlock::is_signed)
    }
}

pub fn valid_signature_value(value: &str) -> bool {
    value.len() == 88
        && value.ends_with("==")
        && value[..86]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
}

pub fn valid_variant(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn valid_module_id(s: &str) -> bool {
    fn segment(x: &str) -> bool {
        if x.is_empty() || x.len() > 64 {
            return false;
        }
        let mut chars = x.chars();
        matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
            && chars.all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')
            })
    }
    let Some((namespace, name)) = s.split_once('/') else {
        return false;
    };
    segment(namespace) && segment(name)
}

pub fn validate_relative_path(path: &str) -> Result<(), M2Error> {
    if path.is_empty() || !path.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
        return Err(M2Error::Invalid(format!("非法的包内路径：{path}")));
    }
    crate::security::safe_join(Path::new("package"), path)
        .map_err(|e| M2Error::Invalid(format!("非法的包内路径 {path}：{e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../../../../contracts/examples/omninexus.v2.json");

    #[test]
    fn contract_example_parses_with_raw_preserved() {
        let m = PackageManifest::parse(EXAMPLE).unwrap();
        assert_eq!(m.schema_version, 2);
        assert!(m.raw.get("entry").is_some());
        assert!(m.raw.get("files").is_some());
    }

    #[test]
    fn rejects_self_described_state_fields() {
        let mut value: serde_json::Value = serde_json::from_str(EXAMPLE).unwrap();
        value["official"] = serde_json::json!(true);
        assert!(PackageManifest::parse(&value.to_string()).is_err());
    }

    #[test]
    fn rejects_lowercase_env_keys() {
        let mut value: serde_json::Value = serde_json::from_str(EXAMPLE).unwrap();
        value["entry"]["env"]["omni_port"] = serde_json::json!("1");
        assert!(PackageManifest::parse(&value.to_string()).is_err());
    }
}
