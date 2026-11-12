//! 生命周期状态机。纯内存，启动器重启即重置；不落盘 pid 与 health。
//! ticket 用来丢弃上一轮启动的迟到事件，临界区由 app 侧的锁负责。

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Stopped,
    Starting,
    Running,
    Failed,
    Stopping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeState {
    NotChecked,
    Pending,
    Healthy,
    Unhealthy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub instance_id: String,
    pub phase: Phase,
    pub ticket: u64,
    pub pid: Option<u32>,
    pub health: ProbeState,
    pub contract: ProbeState,
    pub exit_code: Option<i32>,
    pub last_error: Option<String>,
}

impl Snapshot {
    fn starting(instance_id: &str, ticket: u64) -> Self {
        Self {
            instance_id: instance_id.to_string(),
            phase: Phase::Starting,
            ticket,
            pid: None,
            health: ProbeState::NotChecked,
            contract: ProbeState::NotChecked,
            exit_code: None,
            last_error: None,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum StartError {
    #[error("实例已在启动中")]
    AlreadyStarting,
    #[error("实例已在运行")]
    AlreadyRunning,
    #[error("实例正在停止")]
    Stopping,
    #[error("ticket 已过期")]
    StaleTicket,
    #[error("ticket 不能为零")]
    InvalidTicket,
    #[error("状态机里没有该实例")]
    UnknownInstance,
    #[error("当前阶段不允许该转换：{0}")]
    UnexpectedPhase(String),
    #[error("健康检查或契约探测未通过")]
    ProbeFailed,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum StopError {
    #[error("实例未运行")]
    NotRunning,
    #[error("实例正在停止")]
    AlreadyStopping,
    #[error("状态机里没有该实例")]
    UnknownInstance,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LifecycleState {
    entries: BTreeMap<String, Snapshot>,
    last_ticket: u64,
}

impl LifecycleState {
    pub fn allocate_ticket(&mut self) -> u64 {
        self.last_ticket += 1;
        self.last_ticket
    }

    pub fn begin_start(&mut self, id: &str, ticket: u64) -> Result<(), StartError> {
        if ticket == 0 {
            return Err(StartError::InvalidTicket);
        }
        match self.entries.get(id).map(|e| e.phase) {
            Some(Phase::Starting) => return Err(StartError::AlreadyStarting),
            Some(Phase::Running) => return Err(StartError::AlreadyRunning),
            Some(Phase::Stopping) => return Err(StartError::Stopping),
            _ => {}
        }
        self.entries
            .insert(id.to_string(), Snapshot::starting(id, ticket));
        Ok(())
    }

    pub fn attach_process(&mut self, id: &str, ticket: u64, pid: u32) -> Result<(), StartError> {
        self.check_ticket(id, ticket)?;
        let entry = self
            .entries
            .get_mut(id)
            .ok_or(StartError::UnknownInstance)?;
        if entry.phase != Phase::Starting {
            return Err(StartError::UnexpectedPhase("attach_process".into()));
        }
        entry.pid = Some(pid);
        Ok(())
    }

    pub fn mark_probe(
        &mut self,
        id: &str,
        ticket: u64,
        health: ProbeState,
        contract: ProbeState,
    ) -> Result<(), StartError> {
        self.check_ticket(id, ticket)?;
        let entry = self
            .entries
            .get_mut(id)
            .ok_or(StartError::UnknownInstance)?;
        if entry.phase != Phase::Starting {
            return Err(StartError::UnexpectedPhase("mark_probe".into()));
        }
        entry.health = health;
        entry.contract = contract;
        Ok(())
    }

    pub fn mark_healthy(&mut self, id: &str, ticket: u64) -> Result<(), StartError> {
        self.check_ticket(id, ticket)?;
        let entry = self
            .entries
            .get_mut(id)
            .ok_or(StartError::UnknownInstance)?;
        if entry.phase != Phase::Starting {
            return Err(StartError::UnexpectedPhase("mark_healthy".into()));
        }
        if entry.health == ProbeState::Unhealthy || entry.contract == ProbeState::Unhealthy {
            entry.phase = Phase::Failed;
            entry.pid = None;
            entry.last_error = Some("健康检查或契约探测未通过".into());
            return Err(StartError::ProbeFailed);
        }
        entry.health = ProbeState::Healthy;
        entry.contract = ProbeState::Healthy;
        entry.phase = Phase::Running;
        Ok(())
    }

    pub fn mark_failed(&mut self, id: &str, ticket: u64, reason: &str) -> Result<(), StartError> {
        self.check_ticket(id, ticket)?;
        let entry = self
            .entries
            .get_mut(id)
            .ok_or(StartError::UnknownInstance)?;
        entry.phase = Phase::Failed;
        entry.pid = None;
        entry.last_error = Some(reason.to_string());
        Ok(())
    }

    /// watcher 用带 ticket 的版本，避免上一轮的退出事件覆盖新启动。
    pub fn mark_exited_for(&mut self, id: &str, ticket: u64, code: Option<i32>) -> bool {
        if self.entries.get(id).map(|e| e.ticket) != Some(ticket) {
            return false;
        }
        self.mark_exited(id, code);
        true
    }

    pub fn mark_exited(&mut self, id: &str, code: Option<i32>) {
        let Some(entry) = self.entries.get_mut(id) else {
            return;
        };
        if entry.phase == Phase::Stopped {
            return;
        }
        entry.exit_code = code;
        entry.pid = None;
        if entry.phase == Phase::Stopping {
            entry.phase = Phase::Stopped;
        } else {
            entry.phase = Phase::Failed;
            entry.last_error = Some(match code {
                Some(c) => format!("进程退出，退出码 {c}"),
                None => "进程终止，未提供退出码".into(),
            });
        }
    }

    pub fn begin_stop(&mut self, id: &str) -> Result<(), StopError> {
        let entry = self.entries.get_mut(id).ok_or(StopError::UnknownInstance)?;
        match entry.phase {
            Phase::Stopped => Err(StopError::NotRunning),
            Phase::Stopping => Err(StopError::AlreadyStopping),
            _ => {
                entry.phase = Phase::Stopping;
                Ok(())
            }
        }
    }

    pub fn mark_stopped(&mut self, id: &str, ticket: u64) {
        if let Some(entry) = self.entries.get_mut(id) {
            if entry.ticket != ticket {
                return;
            }
            entry.phase = Phase::Stopped;
            entry.pid = None;
            entry.health = ProbeState::NotChecked;
            entry.contract = ProbeState::NotChecked;
            entry.exit_code = None;
            entry.last_error = None;
        }
    }

    pub fn remove(&mut self, id: &str) {
        self.entries.remove(id);
    }

    pub fn get(&self, id: &str) -> Option<&Snapshot> {
        self.entries.get(id)
    }

    pub fn snapshot(&self) -> BTreeMap<String, Snapshot> {
        self.entries.clone()
    }

    /// 喂给 retention::cleanup_plan；过渡阶段一律视为占用，避免清理正在启停的实例。
    pub fn running(&self) -> BTreeSet<String> {
        self.entries
            .iter()
            .filter(|(_, e)| matches!(e.phase, Phase::Starting | Phase::Running | Phase::Stopping))
            .map(|(id, _)| id.clone())
            .collect()
    }

    fn check_ticket(&self, id: &str, ticket: u64) -> Result<(), StartError> {
        let entry = self.entries.get(id).ok_or(StartError::UnknownInstance)?;
        if entry.ticket != ticket {
            return Err(StartError::StaleTicket);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_start_is_exclusive() {
        let mut state = LifecycleState::default();
        let first = state.allocate_ticket();
        let second = state.allocate_ticket();
        assert!(state.begin_start("a", first).is_ok());
        assert_eq!(
            state.begin_start("a", second),
            Err(StartError::AlreadyStarting)
        );
    }

    #[test]
    fn invalid_ticket_is_rejected_on_empty_state() {
        let mut state = LifecycleState::default();
        assert_eq!(state.begin_start("a", 0), Err(StartError::InvalidTicket));
    }

    #[test]
    fn stop_requires_a_live_instance() {
        let mut state = LifecycleState::default();
        assert_eq!(state.begin_stop("missing"), Err(StopError::UnknownInstance));
        let ticket = state.allocate_ticket();
        state.begin_start("a", ticket).unwrap();
        assert_eq!(state.begin_stop("a"), Ok(()));
        assert_eq!(state.begin_stop("a"), Err(StopError::AlreadyStopping));
        state.mark_stopped("a", ticket);
        assert_eq!(state.begin_stop("a"), Err(StopError::NotRunning));
    }
}
