//! active 切换的事务计划。只产步骤，执行与回滚留在 app 侧。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationStep {
    StopPrevious,
    StartNextOnCanonicalPort,
    ProbeHealth,
    ProbeContract,
    CommitActive,
    Cleanup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryStep {
    StopFailedCandidate,
    RestorePrevious,
    RestoreActiveSlot,
    ReportFailure,
}

/// 切换到同一实例时只做重新探测，不动 active 槽。
pub fn activation_steps(old: Option<&str>, next: &str) -> Vec<ActivationStep> {
    if next.is_empty() {
        return Vec::new();
    }
    if old == Some(next) {
        return vec![ActivationStep::ProbeHealth, ActivationStep::ProbeContract];
    }
    let mut steps = Vec::new();
    if old.is_some() {
        steps.push(ActivationStep::StopPrevious);
    }
    steps.extend([
        ActivationStep::StartNextOnCanonicalPort,
        ActivationStep::ProbeHealth,
        ActivationStep::ProbeContract,
        ActivationStep::CommitActive,
        ActivationStep::Cleanup,
    ]);
    steps
}

/// CommitActive 之前失败：停候选、重启旧实例、恢复 active 槽。
/// Cleanup 失败时 active 已提交，不回滚槽，只报告。
pub fn rollback_steps(previous: Option<&str>, failed: ActivationStep) -> Vec<RecoveryStep> {
    if failed == ActivationStep::StopPrevious {
        return vec![RecoveryStep::ReportFailure];
    }
    let mut steps = vec![RecoveryStep::StopFailedCandidate];
    if failed == ActivationStep::Cleanup {
        steps.push(RecoveryStep::ReportFailure);
        return steps;
    }
    if previous.is_some() {
        steps.push(RecoveryStep::RestorePrevious);
        steps.push(RecoveryStep::RestoreActiveSlot);
    }
    steps.push(RecoveryStep::ReportFailure);
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_switch_has_six_steps() {
        assert_eq!(activation_steps(None, "b").len(), 5);
        assert_eq!(activation_steps(Some("a"), "b").len(), 6);
        assert_eq!(
            activation_steps(Some("a"), "b")[0],
            ActivationStep::StopPrevious
        );
        assert!(activation_steps(Some("a"), "").is_empty());
    }

    #[test]
    fn rollback_depends_on_failure_point() {
        assert_eq!(
            rollback_steps(Some("a"), ActivationStep::StopPrevious),
            vec![RecoveryStep::ReportFailure]
        );
        assert_eq!(
            rollback_steps(Some("a"), ActivationStep::ProbeHealth),
            vec![
                RecoveryStep::StopFailedCandidate,
                RecoveryStep::RestorePrevious,
                RecoveryStep::RestoreActiveSlot,
                RecoveryStep::ReportFailure,
            ]
        );
        assert_eq!(
            rollback_steps(Some("a"), ActivationStep::Cleanup),
            vec![
                RecoveryStep::StopFailedCandidate,
                RecoveryStep::ReportFailure
            ]
        );
    }
}
