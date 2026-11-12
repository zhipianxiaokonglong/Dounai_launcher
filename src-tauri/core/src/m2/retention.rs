//! retention.json：旧版保留与清理边界。
//!
//! 清理只在新版本成功激活后执行；data_dir 永不自动删除，只标记孤儿并提供手动入口。

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::m2::catalog::version_key;
use crate::m2::installed::InstalledState;
use crate::m2::{
    semver_cmp, Category, InstanceSourceKind, M2Error, Verdict, CLEANUP_TRIGGER, DATA_DIR_POLICY,
    DEFAULT_KEEP_VERSIONS, SOURCE_DIR_POLICY,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionPolicy {
    pub schema_version: u8,
    pub keep_versions: u8,
    pub pinned: Vec<String>,
    pub data_dir_policy: String,
    pub source_dir_policy: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_trigger: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_cleanup_at: Option<String>,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            schema_version: 1,
            keep_versions: DEFAULT_KEEP_VERSIONS,
            pinned: Vec::new(),
            data_dir_policy: DATA_DIR_POLICY.into(),
            source_dir_policy: SOURCE_DIR_POLICY.into(),
            cleanup_trigger: Some(CLEANUP_TRIGGER.into()),
            last_cleanup_at: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanupPlan {
    /// 允许删除包文件、venv 与日志的 instance_id。
    pub removable: Vec<String>,
    /// 只标记孤儿，绝不自动删除。
    pub preserved_data_dirs: Vec<String>,
}

impl RetentionPolicy {
    pub fn load(path: &Path) -> Result<Self, M2Error> {
        let policy: Self = crate::state::read_json(path)?;
        policy.validate()?;
        Ok(policy)
    }

    pub fn save(&self, path: &Path) -> Result<(), M2Error> {
        self.validate()?;
        crate::state::write_json(path, self)?;
        Ok(())
    }

    pub fn validate(&self) -> Result<(), M2Error> {
        if self.schema_version != 1 {
            return Err(M2Error::Invalid(
                "retention.json schema_version 必须为 1".into(),
            ));
        }
        if self.keep_versions > 50 {
            return Err(M2Error::Invalid("keep_versions 不得超过 50".into()));
        }
        if self.data_dir_policy != DATA_DIR_POLICY || self.source_dir_policy != SOURCE_DIR_POLICY {
            return Err(M2Error::Invalid(
                "data_dir_policy 与 source_dir_policy 是契约常量，不得改写".into(),
            ));
        }
        if let Some(trigger) = &self.cleanup_trigger {
            if trigger != CLEANUP_TRIGGER {
                return Err(M2Error::Invalid(
                    "cleanup_trigger 只能是 after_successful_activation".into(),
                ));
            }
        }
        Ok(())
    }

    /// 只生成计划，不删文件。激活失败或回滚时返回空计划。
    pub fn cleanup_plan(
        &self,
        state: &InstalledState,
        running: &BTreeSet<String>,
        activation_succeeded: bool,
    ) -> Result<CleanupPlan, M2Error> {
        self.validate()?;
        if !activation_succeeded {
            return Ok(CleanupPlan::default());
        }
        let mut groups: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for (id, instance) in &state.instances {
            let key = version_key(
                instance.publisher_fp.as_deref().unwrap_or(""),
                &instance.module_id,
                &instance.variant,
            );
            groups
                .entry(key)
                .or_default()
                .push((id.clone(), instance.version.clone()));
        }
        let mut plan = CleanupPlan::default();
        for members in groups.values_mut() {
            members.sort_by(|a, b| semver_cmp(&b.1, &a.1).unwrap_or(Ordering::Equal));
            for (index, (id, _)) in members.iter().enumerate() {
                if index < self.keep_versions as usize {
                    continue;
                }
                let Some(instance) = state.instances.get(id) else {
                    continue;
                };
                if instance.pinned || self.pinned.contains(id) {
                    continue;
                }
                if state.active.values().any(|active| active == id) {
                    continue;
                }
                if running.contains(id) {
                    continue;
                }
                if instance.source_kind == InstanceSourceKind::Dir {
                    // 用户源码目录永不进入清理或卸载范围。
                    continue;
                }
                if instance.verdict == Verdict::D || instance.category == Category::Reserved {
                    continue;
                }
                plan.removable.push(id.clone());
                if let Some(data_dir) = &instance.data_dir {
                    plan.preserved_data_dirs.push(data_dir.clone());
                }
            }
        }
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::m2::installed::Instance;
    use crate::m2::{Category, InstanceSourceKind, Verdict};
    use crate::manifest::Role;

    fn instance(id: &str, version: &str, pinned: bool) -> Instance {
        Instance {
            instance_id: id.into(),
            module_id: "dounai/demo".into(),
            variant: "stable".into(),
            version: version.into(),
            role: Role::Llm,
            category: Category::Optional,
            publisher_fp: Some(format!("sha256:{}", "a".repeat(64))),
            verdict: Verdict::A,
            confirmed_at: "2026-01-01T00:00:00Z".into(),
            installed_at: "2026-01-01T00:00:00Z".into(),
            source_kind: InstanceSourceKind::Zip,
            source_path: None,
            git: None,
            pinned,
            auto_update: true,
            zip_sha256: None,
            expected_signer_fp: None,
            signature_present: None,
            allow_unsigned_once: None,
            head_confirmed: None,
            data_dir: Some(format!("C:/data/{id}")),
            note: None,
        }
    }

    fn state_with(versions: &[&str]) -> InstalledState {
        let mut state = InstalledState::new();
        for version in versions {
            let id = format!("dounai/demo@{version}");
            state.upsert(instance(&id, version, false)).unwrap();
        }
        state
            .active
            .insert("llm".into(), "dounai/demo@4.0.0".into());
        state
    }

    #[test]
    fn keep_three_versions_and_protect_active_pinned_running() {
        let mut state = state_with(&["1.0.0", "2.0.0", "3.0.0", "4.0.0"]);
        state.instances.get_mut("dounai/demo@1.0.0").unwrap().pinned = true;
        let running = BTreeSet::from(["dounai/demo@2.0.0".to_string()]);
        let policy = RetentionPolicy::default();
        let plan = policy.cleanup_plan(&state, &running, true).unwrap();
        // 唯一超出配额的 1.0.0 被 pinned 保护，因此没有可清理项。
        assert!(plan.removable.is_empty());
        assert!(policy
            .cleanup_plan(&state, &running, false)
            .unwrap()
            .removable
            .is_empty());
        state.instances.get_mut("dounai/demo@1.0.0").unwrap().pinned = false;
        let plan = policy.cleanup_plan(&state, &running, true).unwrap();
        assert_eq!(plan.removable, vec!["dounai/demo@1.0.0".to_string()]);
        assert!(!plan.preserved_data_dirs.is_empty());
    }

    #[test]
    fn zero_keep_still_protects_active() {
        let state = state_with(&["1.0.0", "2.0.0", "4.0.0"]);
        let policy = RetentionPolicy {
            keep_versions: 0,
            ..Default::default()
        };
        let plan = policy.cleanup_plan(&state, &BTreeSet::new(), true).unwrap();
        assert!(!plan.removable.contains(&"dounai/demo@4.0.0".to_string()));
        assert_eq!(plan.removable.len(), 2);
    }

    #[test]
    fn policy_constants_cannot_be_changed() {
        let policy = RetentionPolicy {
            data_dir_policy: "delete".into(),
            ..Default::default()
        };
        assert!(policy.validate().is_err());
    }
}
