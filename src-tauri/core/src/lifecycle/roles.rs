//! 角色拓扑真源。roles-v1.json 编译期内嵌，两个 crate 的相对路径同指仓库 contracts/。
//! 本模块只解析与排序，不发请求、不启动进程。

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::manifest::Role;

pub const ROLES_JSON: &str = include_str!("../../../../contracts/roles-v1.json");

pub fn role_name(role: Role) -> &'static str {
    match role {
        Role::Input => "input",
        Role::Llm => "llm",
        Role::Output => "output",
        Role::Shell => "shell",
        Role::Auxiliary => "auxiliary",
    }
}

pub fn role_from_name(name: &str) -> Option<Role> {
    match name {
        "input" => Some(Role::Input),
        "llm" => Some(Role::Llm),
        "output" => Some(Role::Output),
        "shell" => Some(Role::Shell),
        "auxiliary" => Some(Role::Auxiliary),
        _ => None,
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TopoError {
    #[error("roles 文档不是合法 JSON：{0}")]
    Parse(String),
    #[error("roles 文档含重复角色：{0}")]
    DuplicateRole(String),
    #[error("未知角色：{0}")]
    UnknownRole(String),
    #[error("角色 {0} 依赖了文档中不存在的 {1}")]
    MissingDependency(String, String),
    #[error("角色依赖成环：{0}")]
    Cycle(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolesDoc {
    pub schema_version: u8,
    pub roles_version: String,
    pub closed: bool,
    #[serde(default)]
    pub unknown_role_policy: String,
    #[serde(default)]
    pub distribution: String,
    pub standby_port_base: u16,
    pub roles: Vec<RoleSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleSpec {
    pub id: Role,
    pub kind: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub health: Option<RoleHealth>,
    #[serde(default)]
    pub depends_on: Vec<Role>,
    #[serde(default)]
    pub max_active: Option<u32>,
    #[serde(default)]
    pub degradable: bool,
    #[serde(default)]
    pub third_party_allowed: bool,
    #[serde(default)]
    pub required_contract: Option<RoleContract>,
    #[serde(default)]
    pub activation_policy: Option<String>,
    #[serde(default)]
    pub consumes: Vec<Role>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleHealth {
    pub path: String,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleContract {
    pub probe: RoleProbe,
    #[serde(default)]
    pub on_mismatch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleProbe {
    pub method: String,
    pub path: String,
    pub timeout_ms: u64,
}

impl RolesDoc {
    pub fn parse(text: &str) -> Result<Self, TopoError> {
        let doc: Self = serde_json::from_str(text).map_err(|e| TopoError::Parse(e.to_string()))?;
        doc.check()?;
        Ok(doc)
    }

    pub fn embedded() -> Result<Self, TopoError> {
        Self::parse(ROLES_JSON)
    }

    pub fn spec(&self, role: Role) -> Option<&RoleSpec> {
        self.roles.iter().find(|s| s.id == role)
    }

    pub fn health(&self, role: Role) -> Option<&RoleHealth> {
        self.spec(role).and_then(|s| s.health.as_ref())
    }

    pub fn contract_probe(&self, role: Role) -> Option<&RoleProbe> {
        self.spec(role)
            .and_then(|s| s.required_contract.as_ref())
            .map(|c| &c.probe)
    }

    pub fn canonical_port(&self, role: Role) -> Option<u16> {
        self.spec(role).and_then(|s| s.port)
    }

    /// 文档自检：重复角色、指向文档外的依赖、全图环。
    pub fn check(&self) -> Result<(), TopoError> {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for spec in &self.roles {
            let name = role_name(spec.id).to_string();
            if !seen.insert(name.clone()) {
                return Err(TopoError::DuplicateRole(name));
            }
        }
        for spec in &self.roles {
            for dep in &spec.depends_on {
                let dep_name = role_name(*dep).to_string();
                if !seen.contains(&dep_name) {
                    return Err(TopoError::MissingDependency(
                        role_name(spec.id).to_string(),
                        dep_name,
                    ));
                }
            }
        }
        topo(self, &seen)?;
        Ok(())
    }
}

/// 按 depends_on 做稳定拓扑排序。wanted 之外的依赖视为外部已满足，不自动加入启动集。
pub fn startup_order(roles: &RolesDoc, wanted: &[Role]) -> Result<Vec<Role>, TopoError> {
    roles.check()?;
    let wanted_set: BTreeSet<String> = wanted.iter().map(|r| role_name(*r).to_string()).collect();
    topo(roles, &wanted_set)
}

fn topo(doc: &RolesDoc, wanted: &BTreeSet<String>) -> Result<Vec<Role>, TopoError> {
    let order: Vec<Role> = doc
        .roles
        .iter()
        .map(|s| s.id)
        .filter(|r| wanted.contains(role_name(*r)))
        .collect();
    if order.len() != wanted.len() {
        let missing = wanted
            .iter()
            .find(|n| !order.iter().any(|r| role_name(*r) == n.as_str()))
            .cloned()
            .unwrap_or_default();
        return Err(TopoError::UnknownRole(missing));
    }

    let mut indegree: BTreeMap<String, usize> = BTreeMap::new();
    let mut dependents: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for role in &order {
        let name = role_name(*role).to_string();
        let spec = doc
            .spec(*role)
            .ok_or_else(|| TopoError::UnknownRole(name.clone()))?;
        let mut count = 0usize;
        for dep in &spec.depends_on {
            let dep_name = role_name(*dep);
            if wanted.contains(dep_name) {
                count += 1;
                dependents
                    .entry(dep_name.to_string())
                    .or_default()
                    .push(name.clone());
            }
        }
        indegree.insert(name, count);
    }

    let mut queue: VecDeque<String> = order
        .iter()
        .map(|r| role_name(*r).to_string())
        .filter(|n| indegree.get(n).copied().unwrap_or(0) == 0)
        .collect();
    let mut result = Vec::new();
    while let Some(name) = queue.pop_front() {
        result.push(role_from_name(&name).ok_or_else(|| TopoError::UnknownRole(name.clone()))?);
        if let Some(list) = dependents.get(&name).cloned() {
            for dep in list {
                let entry = indegree
                    .get_mut(&dep)
                    .ok_or_else(|| TopoError::Cycle(dep.clone()))?;
                *entry -= 1;
                if *entry == 0 {
                    queue.push_back(dep);
                }
            }
        }
    }
    if result.len() != order.len() {
        return Err(TopoError::Cycle(
            order
                .iter()
                .map(|r| role_name(*r))
                .collect::<Vec<_>>()
                .join(","),
        ));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_document_parses() {
        let doc = RolesDoc::embedded().unwrap();
        assert_eq!(doc.schema_version, 1);
        assert_eq!(doc.roles.len(), 5);
        assert_eq!(doc.standby_port_base, 18101);
        assert_eq!(doc.canonical_port(Role::Input), Some(8101));
        assert_eq!(doc.canonical_port(Role::Shell), None);
        assert_eq!(
            doc.contract_probe(Role::Input).unwrap().path,
            "/v1/contract"
        );
        assert_eq!(
            doc.contract_probe(Role::Output).unwrap().path,
            "/api/v1/contract"
        );
        assert!(doc.contract_probe(Role::Shell).is_none());
    }

    #[test]
    fn missing_dependency_is_rejected() {
        let mut value: serde_json::Value = serde_json::from_str(ROLES_JSON).unwrap();
        let roles = value["roles"].as_array_mut().unwrap();
        roles.retain(|r| r["id"] != "input");
        let err = RolesDoc::parse(&value.to_string()).unwrap_err();
        assert!(matches!(err, TopoError::MissingDependency(_, _)));
    }
}
