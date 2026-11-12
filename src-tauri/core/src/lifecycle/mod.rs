//! M3 生命周期纯逻辑层：角色拓扑、启动互斥状态机、日志轮转策略与 active 事务计划。
//! 不引 tokio、不碰 Win32。进程句柄、watcher 与事件推送留在桌面壳。

pub mod activation;
pub mod logs;
pub mod roles;
pub mod state;

pub use activation::{activation_steps, rollback_steps, ActivationStep, RecoveryStep};
pub use logs::{
    diagnostic_enabled, rotation_plan, LogFile, DEFAULT_DIAGNOSTIC_MODE, DEFAULT_KEEP_FILES,
    DEFAULT_MAX_BYTES, LOG_DIR_ACL_RULE, LOG_DIR_NAME,
};
pub use roles::{
    role_from_name, role_name, startup_order, RoleContract, RoleHealth, RoleProbe, RoleSpec,
    RolesDoc, TopoError, ROLES_JSON,
};
pub use state::{LifecycleState, Phase, ProbeState, Snapshot, StartError, StopError};

pub fn embedded_roles() -> Result<RolesDoc, TopoError> {
    RolesDoc::embedded()
}
