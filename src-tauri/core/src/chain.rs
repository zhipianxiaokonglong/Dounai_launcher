//! state/chains.json：用户自定义链路注册表与链路执行计划。
//!
//! 链路只编排、不安装。steps 存 instance_id，不复制 installed.json 里的任何事实；
//! 实例卸载后保留原值，读取时才标成悬空，绝不回写用户数据。
//! 默认链路不落盘：由未禁用实例按 role 拓扑隐式生成，避免增删实例后维护第二份事实。
//! 运行进度也不写这个文件，只在内存与事件里流动。

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::state::{read_json, write_json, StateError};
use crate::time::utc_now;

pub const DEFAULT_CHAIN_ID: &str = "default";
pub const DEFAULT_CHAIN_NAME: &str = "默认链路";
pub const MAX_STEPS: usize = 32;
pub const MAX_ID_LEN: usize = 32;
pub const MAX_NAME_LEN: usize = 64;
pub const MAX_INSTANCE_ID_LEN: usize = 256;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ChainError {
    #[error("链路数据不合法：{0}")]
    Invalid(String),
    #[error("状态文件读写失败：{0}")]
    State(String),
}

impl From<StateError> for ChainError {
    fn from(e: StateError) -> Self {
        Self::State(e.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chain {
    pub id: String,
    pub name: String,
    pub steps: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainRegistry {
    pub schema_version: u8,
    pub chains: Vec<Chain>,
}

impl Default for ChainRegistry {
    fn default() -> Self {
        Self {
            schema_version: 1,
            chains: Vec::new(),
        }
    }
}

impl ChainRegistry {
    /// 空表是合法状态：用户还没建过链路，一键启动仍走隐式默认链路。
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn load(path: &Path) -> Result<Self, ChainError> {
        let registry: Self = read_json(path)?;
        registry.validate()?;
        Ok(registry)
    }

    pub fn save(&self, path: &Path) -> Result<(), ChainError> {
        self.validate()?;
        write_json(path, self)?;
        Ok(())
    }

    pub fn find(&self, id: &str) -> Option<&Chain> {
        self.chains.iter().find(|c| c.id == id)
    }

    /// 新建或编辑一条链路。编辑保留 created_at，只更新名称、步骤与 updated_at。
    /// 时间由调用方传入，core 不自己读时钟，便于测试断言。
    pub fn upsert(
        &mut self,
        id: &str,
        name: &str,
        steps: Vec<String>,
        now: &str,
    ) -> Result<(), ChainError> {
        validate_id(id)?;
        validate_name(name)?;
        validate_steps(&steps)?;
        validate_timestamp(now)?;
        if let Some(chain) = self.chains.iter_mut().find(|c| c.id == id) {
            chain.name = name.to_string();
            chain.steps = steps;
            chain.updated_at = now.to_string();
        } else {
            self.chains.push(Chain {
                id: id.to_string(),
                name: name.to_string(),
                steps,
                created_at: now.to_string(),
                updated_at: now.to_string(),
            });
        }
        self.validate()
    }

    pub fn remove(&mut self, id: &str) -> Result<(), ChainError> {
        let before = self.chains.len();
        self.chains.retain(|c| c.id != id);
        if self.chains.len() == before {
            return Err(ChainError::Invalid(format!("链路不存在：{id}")));
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), ChainError> {
        if self.schema_version != 1 {
            return Err(ChainError::Invalid(format!(
                "schema_version 应为 1，实际 {}",
                self.schema_version
            )));
        }
        let mut ids = BTreeSet::new();
        for chain in &self.chains {
            validate_id(&chain.id)?;
            if !ids.insert(chain.id.as_str()) {
                return Err(ChainError::Invalid(format!("链路 id 重复：{}", chain.id)));
            }
            validate_name(&chain.name)?;
            validate_steps(&chain.steps)?;
            validate_timestamp(&chain.created_at)?;
            validate_timestamp(&chain.updated_at)?;
        }
        Ok(())
    }
}

fn validate_id(id: &str) -> Result<(), ChainError> {
    let ok = !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(ChainError::Invalid(format!("链路 id 不合法：{id}")))
    }
}

fn validate_name(name: &str) -> Result<(), ChainError> {
    if name.is_empty() || name.chars().count() > MAX_NAME_LEN {
        return Err(ChainError::Invalid("链路名称为空或过长".into()));
    }
    Ok(())
}

fn validate_steps(steps: &[String]) -> Result<(), ChainError> {
    if steps.is_empty() || steps.len() > MAX_STEPS {
        return Err(ChainError::Invalid(format!(
            "steps 数量应在 1 到 {MAX_STEPS} 之间，实际 {}",
            steps.len()
        )));
    }
    let mut seen = BTreeSet::new();
    for step in steps {
        if step.is_empty() || step.len() > MAX_INSTANCE_ID_LEN {
            return Err(ChainError::Invalid(format!("instance_id 不合法：{step}")));
        }
        if !seen.insert(step.as_str()) {
            return Err(ChainError::Invalid(format!("steps 重复：{step}")));
        }
    }
    Ok(())
}

/// 与 chains-v1.schema.json 的 pattern 保持一致。core 不引正则库，只查形状。
fn validate_timestamp(value: &str) -> Result<(), ChainError> {
    let bytes = value.as_bytes();
    let ok = bytes.len() == 20
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes[19] == b'Z'
        && bytes
            .iter()
            .enumerate()
            .all(|(i, b)| matches!(i, 4 | 7 | 10 | 13 | 16 | 19) || b.is_ascii_digit());
    if ok {
        Ok(())
    } else {
        Err(ChainError::Invalid(format!("时间戳不是 UTC RFC 3339：{value}")))
    }
}

/* ------------------------------------------------------------------ */
/* 执行计划                                                            */
/* ------------------------------------------------------------------ */

/// 读取时的步骤状态。悬空只在这里标记，不改写 chains.json。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Ready,
    Dangling,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedStep {
    /// 声明顺序，从 1 开始，用于界面编号与失败定位。
    pub index: usize,
    pub instance_id: String,
    pub status: StepStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainPlan {
    pub chain_id: String,
    pub name: String,
    pub is_default: bool,
    pub steps: Vec<PlannedStep>,
}

impl ChainPlan {
    pub fn from_registry(
        registry: &ChainRegistry,
        id: &str,
        known: &BTreeSet<String>,
    ) -> Result<Self, ChainError> {
        let chain = registry
            .find(id)
            .ok_or_else(|| ChainError::Invalid(format!("链路不存在：{id}")))?;
        Ok(Self::from_chain(chain, known))
    }

    pub fn from_chain(chain: &Chain, known: &BTreeSet<String>) -> Self {
        Self {
            chain_id: chain.id.clone(),
            name: chain.name.clone(),
            is_default: false,
            steps: enumerate(&chain.steps, known),
        }
    }

    /// 默认链路按调用方给好的拓扑顺序生成，不落盘。
    pub fn from_default(ordered: &[String]) -> Self {
        Self {
            chain_id: DEFAULT_CHAIN_ID.to_string(),
            name: DEFAULT_CHAIN_NAME.to_string(),
            is_default: true,
            steps: ordered
                .iter()
                .enumerate()
                .map(|(i, id)| PlannedStep {
                    index: i + 1,
                    instance_id: id.clone(),
                    status: StepStatus::Ready,
                })
                .collect(),
        }
    }

    pub fn dangling(&self) -> Vec<&PlannedStep> {
        self.steps
            .iter()
            .filter(|s| s.status == StepStatus::Dangling)
            .collect()
    }

    /// 只有存在的实例才进入启动与停止；blocked 由 app 层按依赖状态判。
    pub fn runnable_ids(&self) -> Vec<String> {
        self.steps
            .iter()
            .filter(|s| s.status == StepStatus::Ready)
            .map(|s| s.instance_id.clone())
            .collect()
    }
}

fn enumerate(steps: &[String], known: &BTreeSet<String>) -> Vec<PlannedStep> {
    steps
        .iter()
        .enumerate()
        .map(|(i, id)| PlannedStep {
            index: i + 1,
            instance_id: id.clone(),
            status: if known.contains(id) {
                StepStatus::Ready
            } else {
                StepStatus::Dangling
            },
        })
        .collect()
}

/* ------------------------------------------------------------------ */
/* 执行结果                                                            */
/* ------------------------------------------------------------------ */

/// 单步的同步结论。不等健康检查：Started 只表示进程已拉起，后续失败由
/// lifecycle-changed 回填到对应步骤的状态点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainStepState {
    Started,
    External,
    Failed,
    Blocked,
    Dangling,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainStepResult {
    pub index: usize,
    pub instance_id: String,
    pub state: ChainStepState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainRunResult {
    pub chain_id: String,
    pub is_default: bool,
    pub steps: Vec<ChainStepResult>,
    /// 依赖未就绪的 instance_id，供界面一次性提示与补装。
    pub missing_deps: Vec<String>,
}

impl ChainRunResult {
    pub fn from_plan(plan: &ChainPlan) -> Self {
        Self {
            chain_id: plan.chain_id.clone(),
            is_default: plan.is_default,
            steps: plan
                .steps
                .iter()
                .map(|s| ChainStepResult {
                    index: s.index,
                    instance_id: s.instance_id.clone(),
                    state: if s.status == StepStatus::Dangling {
                        ChainStepState::Dangling
                    } else {
                        ChainStepState::Started
                    },
                    reason: if s.status == StepStatus::Dangling {
                        Some("实例已卸载或不存在".into())
                    } else {
                        None
                    },
                })
                .collect(),
            missing_deps: Vec::new(),
        }
    }

    pub fn set(&mut self, index: usize, state: ChainStepState, reason: Option<String>) {
        if let Some(step) = self.steps.iter_mut().find(|s| s.index == index) {
            step.state = state;
            step.reason = reason;
        }
    }

    /// 本次成功拉起的实例，供 only_started 回滚停止使用。
    pub fn started_ids(&self) -> Vec<String> {
        self.steps
            .iter()
            .filter(|s| matches!(s.state, ChainStepState::Started | ChainStepState::External))
            .map(|s| s.instance_id.clone())
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainProgress {
    pub chain_id: String,
    pub index: usize,
    pub total: usize,
    pub instance_id: String,
    pub phase: ChainProgressPhase,
    pub state: Option<ChainStepState>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainProgressPhase {
    Submitting,
    Finished,
}

/// 便捷构造：core 层唯一读时钟的地方，app 层一般直接传 AppStateV2::now()。
pub fn now() -> String {
    utc_now()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIME: &str = "2026-11-14T10:00:00Z";

    fn registry_with_one() -> ChainRegistry {
        let mut r = ChainRegistry::empty();
        r.upsert(
            "daily-chat",
            "日常对话",
            vec!["dounai/input@stable@main".into(), "dounai/llm@stable@main".into()],
            TIME,
        )
        .unwrap();
        r
    }

    #[test]
    fn empty_registry_is_valid_and_represents_no_user_chains() {
        assert!(ChainRegistry::empty().validate().is_ok());
        assert!(ChainRegistry::empty().chains.is_empty());
    }

    #[test]
    fn upsert_creates_then_edits_without_losing_created_at() {
        let mut r = registry_with_one();
        r.upsert(
            "daily-chat",
            "改过的名字",
            vec!["dounai/llm@stable@main".into()],
            "2026-11-14T11:00:00Z",
        )
        .unwrap();
        assert_eq!(r.chains.len(), 1);
        assert_eq!(r.chains[0].created_at, TIME);
        assert_eq!(r.chains[0].updated_at, "2026-11-14T11:00:00Z");
        assert_eq!(r.chains[0].name, "改过的名字");
        assert_eq!(r.chains[0].steps.len(), 1);
    }

    #[test]
    fn validate_rejects_bad_ids_names_steps_and_timestamps() {
        let mut r = registry_with_one();
        assert!(r.upsert("Daily/Chat", "x", vec!["a".into()], TIME).is_err());
        assert!(r.upsert("ok", "", vec!["a".into()], TIME).is_err());
        assert!(r.upsert("ok", "x", vec![], TIME).is_err());
        assert!(r.upsert("ok", "x", vec!["a".into(), "a".into()], TIME).is_err());
        assert!(r
            .upsert("ok", "x", (0..33).map(|i| format!("i-{i}")).collect(), TIME)
            .is_err());
        assert!(r.upsert("ok", "x", vec!["a".into()], "2026-11-14 10:00").is_err());
        // 失败的 upsert 不能污染注册表。
        assert_eq!(r.chains.len(), 1);
        assert_eq!(r.chains[0].id, "daily-chat");
    }

    #[test]
    fn duplicate_chain_ids_are_rejected_on_load_shape() {
        let mut r = registry_with_one();
        let mut dup = r.chains[0].clone();
        dup.name = "另一条".into();
        r.chains.push(dup);
        assert!(r.validate().is_err());
    }

    #[test]
    fn remove_reports_unknown_id() {
        let mut r = registry_with_one();
        assert!(r.remove("nope").is_err());
        assert!(r.remove("daily-chat").is_ok());
        assert!(r.chains.is_empty());
    }

    #[test]
    fn load_and_save_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chains.json");
        registry_with_one().save(&path).unwrap();
        assert_eq!(ChainRegistry::load(&path).unwrap(), registry_with_one());
    }

    #[test]
    fn plan_marks_dangling_steps_without_rewriting_the_registry() {
        let r = registry_with_one();
        let known: BTreeSet<String> = ["dounai/input@stable@main".to_string()].into_iter().collect();
        let plan = ChainPlan::from_registry(&r, "daily-chat", &known).unwrap();
        assert_eq!(plan.dangling().len(), 1);
        assert_eq!(plan.dangling()[0].instance_id, "dounai/llm@stable@main");
        assert_eq!(plan.runnable_ids(), vec!["dounai/input@stable@main"]);
        // 注册表里原值仍在，是读取方标记而非回写。
        assert_eq!(r.chains[0].steps.len(), 2);
    }

    #[test]
    fn default_plan_keeps_the_given_topology_order_and_is_not_persisted() {
        let plan = ChainPlan::from_default(&["b".into(), "a".into()]);
        assert!(plan.is_default);
        assert_eq!(plan.chain_id, DEFAULT_CHAIN_ID);
        assert_eq!(plan.steps[0].instance_id, "b");
        assert_eq!(plan.steps[1].index, 2);
        assert!(plan.dangling().is_empty());
    }

    #[test]
    fn run_result_only_lists_started_or_external_instances() {
        let mut result = ChainRunResult::from_plan(&ChainPlan::from_default(&[
            "a".into(),
            "b".into(),
            "c".into(),
        ]));
        result.set(2, ChainStepState::Failed, Some("端口被占".into()));
        result.set(3, ChainStepState::Blocked, Some("依赖未装".into()));
        result.missing_deps.push("c".into());
        assert_eq!(result.started_ids(), vec!["a"]);
        assert_eq!(result.steps[1].reason.as_deref(), Some("端口被占"));
    }

    #[test]
    fn dangling_steps_are_reported_in_the_run_result() {
        let r = registry_with_one();
        let plan = ChainPlan::from_chain(&r.chains[0], &BTreeSet::new());
        let result = ChainRunResult::from_plan(&plan);
        assert!(result
            .steps
            .iter()
            .all(|s| s.state == ChainStepState::Dangling));
        assert!(result.started_ids().is_empty());
    }

    #[test]
    fn now_matches_the_schema_timestamp_pattern() {
        assert!(validate_timestamp(&now()).is_ok());
    }
}
