//! installed.json：主键是 instance_id，active 切换是一次原子写事务。
//!
//! 切换失败时恢复内存中的旧 active 且状态文件不变，旧实例继续运行。
//! 进程启停、health 与契约探测属于 M3；这里只负责状态层的事务边界。

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::m2::catalog::CatalogState;
use crate::m2::{Category, InstanceSourceKind, M2Error, Verdict, STANDBY_PORT_BASE};
use crate::manifest::Role;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitObservation {
    pub present: bool,
    /// 可能含凭据，禁止写进日志；本实现不读取、不落盘 origin。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub head: Option<String>,
    pub dirty: bool,
    pub observed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Instance {
    pub instance_id: String,
    pub module_id: String,
    pub variant: String,
    pub version: String,
    pub role: Role,
    pub category: Category,
    pub publisher_fp: Option<String>,
    pub verdict: Verdict,
    pub confirmed_at: String,
    pub installed_at: String,
    pub source_kind: InstanceSourceKind,
    pub source_path: Option<String>,
    pub git: Option<GitObservation>,
    pub pinned: bool,
    pub auto_update: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zip_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_signer_fp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_present: Option<bool>,
    /// C 类的一次性授权记录，不得做成可常开的设置项。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_unsigned_once: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_confirmed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledState {
    pub schema_version: u8,
    pub active: BTreeMap<String, String>,
    pub instances: BTreeMap<String, Instance>,
    pub catalog_state: CatalogState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivePlan {
    pub role: String,
    pub previous: Option<String>,
    pub next: String,
}

pub fn role_key(role: Role) -> Result<&'static str, M2Error> {
    match role {
        Role::Input => Ok("input"),
        Role::Llm => Ok("llm"),
        Role::Output => Ok("output"),
        Role::Shell => Ok("shell"),
        Role::Auxiliary => Err(M2Error::Invalid("auxiliary 不设 active 槽".into())),
    }
}

impl InstalledState {
    pub fn new() -> Self {
        Self {
            schema_version: 1,
            ..Default::default()
        }
    }

    pub fn load(path: &Path) -> Result<Self, M2Error> {
        let state: Self = crate::state::read_json(path)?;
        state.validate()?;
        Ok(state)
    }

    pub fn save(&self, path: &Path) -> Result<(), M2Error> {
        self.validate()?;
        crate::state::write_json(path, self)?;
        Ok(())
    }

    pub fn validate(&self) -> Result<(), M2Error> {
        if self.schema_version != 1 {
            return Err(M2Error::Invalid(
                "installed.json schema_version 必须为 1".into(),
            ));
        }
        for role in self.active.keys() {
            if !["input", "llm", "output", "shell"].contains(&role.as_str()) {
                return Err(M2Error::Invalid(format!("非法的 active 槽：{role}")));
            }
        }
        for (role, id) in &self.active {
            let instance = self
                .instances
                .get(id)
                .ok_or_else(|| M2Error::Invalid(format!("active[{role}] 指向不存在的实例 {id}")))?;
            if role_key(instance.role)? != role {
                return Err(M2Error::Invalid(format!(
                    "active[{role}] 指向的实例角色不符：{id}"
                )));
            }
        }
        for (id, instance) in &self.instances {
            if instance.instance_id != *id {
                return Err(M2Error::Invalid(format!("实例主键不一致：{id}")));
            }
            crate::paths::validate_instance_id(id)
                .map_err(|e| M2Error::Invalid(format!("instance_id 非法：{e}")))?;
            if instance.verdict == Verdict::D {
                return Err(M2Error::Invalid(format!("D 类实例不应写入实例表：{id}")));
            }
            if instance.verdict == Verdict::C && instance.auto_update {
                return Err(M2Error::Invalid(format!("C 类实例不得自动更新：{id}")));
            }
            if instance.source_kind == InstanceSourceKind::Dir
                && (instance.verdict != Verdict::C
                    || instance.auto_update
                    || instance.category != Category::Optional)
            {
                return Err(M2Error::Invalid(format!(
                    "dir 实例固定为 C 类、optional 且不自动更新：{id}"
                )));
            }
        }
        Ok(())
    }

    pub fn upsert(&mut self, instance: Instance) -> Result<(), M2Error> {
        self.instances
            .insert(instance.instance_id.clone(), instance);
        self.validate()
    }

    /// 原子切换：状态文件一次写完，写失败恢复内存中的旧值。
    /// `explicit=false` 时拒绝把 C 类实例自动切成 active。
    pub fn switch_active(
        &mut self,
        path: &Path,
        role: Role,
        next: &str,
        explicit: bool,
    ) -> Result<ActivePlan, M2Error> {
        let key = role_key(role)?.to_string();
        let instance = self
            .instances
            .get(next)
            .ok_or_else(|| M2Error::Invalid(format!("实例不存在：{next}")))?;
        if instance.role != role {
            return Err(M2Error::Invalid(format!("实例 {next} 不属于 {key} 角色")));
        }
        if instance.verdict == Verdict::C && !explicit {
            return Err(M2Error::Rejected(format!(
                "C 类实例不能自动成为 active，需用户显式切换：{next}"
            )));
        }
        let previous = self.active.get(&key).cloned();
        if previous.as_deref() == Some(next) {
            return Ok(ActivePlan {
                role: key,
                previous,
                next: next.into(),
            });
        }
        self.active.insert(key.clone(), next.to_string());
        if let Err(e) = self.save(path) {
            match previous {
                Some(old) => {
                    self.active.insert(key.clone(), old);
                }
                None => {
                    self.active.remove(&key);
                }
            }
            return Err(e);
        }
        Ok(ActivePlan {
            role: key,
            previous,
            next: next.into(),
        })
    }

    pub fn active_instance(&self, role: Role) -> Option<&Instance> {
        let key = role_key(role).ok()?;
        self.active.get(key).and_then(|id| self.instances.get(id))
    }

    /// 非 active 实例用备用端口段，避免抢占规范端口。
    pub fn standby_port(&self, index: u16) -> u16 {
        STANDBY_PORT_BASE + index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(id: &str, role: Role, verdict: Verdict) -> Instance {
        Instance {
            instance_id: id.into(),
            module_id: "dounai/demo".into(),
            variant: "stable".into(),
            version: "1.0.0".into(),
            role,
            category: Category::Optional,
            publisher_fp: None,
            verdict,
            confirmed_at: "2026-01-01T00:00:00Z".into(),
            installed_at: "2026-01-01T00:00:00Z".into(),
            source_kind: InstanceSourceKind::Zip,
            source_path: None,
            git: None,
            pinned: false,
            auto_update: verdict == Verdict::A,
            zip_sha256: None,
            expected_signer_fp: None,
            signature_present: None,
            allow_unsigned_once: None,
            head_confirmed: None,
            data_dir: None,
            note: None,
        }
    }

    #[test]
    fn two_instances_coexist_and_switch_is_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("installed.json");
        let mut state = InstalledState::new();
        state
            .upsert(instance("dounai/demo@stable", Role::Llm, Verdict::A))
            .unwrap();
        state
            .upsert(instance("dounai/demo@debug", Role::Llm, Verdict::A))
            .unwrap();
        assert_eq!(state.instances.len(), 2);
        let plan = state
            .switch_active(&path, Role::Llm, "dounai/demo@stable", false)
            .unwrap();
        assert!(plan.previous.is_none());
        let plan = state
            .switch_active(&path, Role::Llm, "dounai/demo@debug", false)
            .unwrap();
        assert_eq!(plan.previous.as_deref(), Some("dounai/demo@stable"));
        let reloaded = InstalledState::load(&path).unwrap();
        assert_eq!(reloaded.active["llm"], "dounai/demo@debug");
    }

    #[test]
    fn c_class_cannot_become_active_without_explicit_switch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("installed.json");
        let mut state = InstalledState::new();
        state
            .upsert(instance("dounai/demo@local", Role::Llm, Verdict::C))
            .unwrap();
        assert!(state
            .switch_active(&path, Role::Llm, "dounai/demo@local", false)
            .is_err());
        assert!(state
            .switch_active(&path, Role::Llm, "dounai/demo@local", true)
            .is_ok());
    }

    #[test]
    fn auxiliary_has_no_active_slot() {
        let mut state = InstalledState::new();
        state
            .upsert(instance("dounai/demo@x", Role::Auxiliary, Verdict::A))
            .unwrap();
        state
            .active
            .insert("auxiliary".into(), "dounai/demo@x".into());
        assert!(state.validate().is_err());
        assert!(role_key(Role::Auxiliary).is_err());
    }

    #[test]
    fn dir_instance_is_fixed_to_c_optional() {
        let mut state = InstalledState::new();
        let mut dir_instance = instance("dounai/demo@local", Role::Llm, Verdict::C);
        dir_instance.source_kind = InstanceSourceKind::Dir;
        dir_instance.category = Category::Core;
        assert!(state.upsert(dir_instance).is_err());
    }
}
