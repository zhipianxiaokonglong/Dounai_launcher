//! M3 生命周期纯逻辑的端到端断言。全程离线，不 spawn 进程，不发 HTTP。

use std::path::PathBuf;

use launcher_core::lifecycle::*;
use launcher_core::m2::local::{observe_source, require_confirmed};
use launcher_core::manifest::Role;

fn position(order: &[Role], role: Role) -> usize {
    order.iter().position(|r| *r == role).unwrap()
}

#[test]
fn embedded_roles_are_the_single_source() {
    let doc = embedded_roles().unwrap();
    assert_eq!(doc.roles.len(), 5);
    assert_eq!(doc.standby_port_base, 18101);
    assert_eq!(role_name(Role::Input), "input");
    assert_eq!(role_from_name("auxiliary"), Some(Role::Auxiliary));
    assert_eq!(role_from_name("unknown"), None);
    assert_eq!(doc.health(Role::Input).unwrap().path, "/healthz");
    assert_eq!(doc.health(Role::Llm).unwrap().path, "/v1/healthz");
    assert_eq!(doc.health(Role::Output).unwrap().path, "/api/health");
    assert!(doc.health(Role::Shell).is_none());
    assert_eq!(doc.health(Role::Input).unwrap().timeout_ms, 3000);
}

#[test]
fn startup_order_respects_the_data_plane_chain() {
    let doc = embedded_roles().unwrap();
    let all = [
        Role::Input,
        Role::Llm,
        Role::Output,
        Role::Shell,
        Role::Auxiliary,
    ];
    let order = startup_order(&doc, &all).unwrap();
    assert_eq!(order.len(), 5);
    assert!(position(&order, Role::Input) < position(&order, Role::Llm));
    assert!(position(&order, Role::Llm) < position(&order, Role::Output));
    assert!(position(&order, Role::Input) < position(&order, Role::Shell));
    assert!(position(&order, Role::Llm) < position(&order, Role::Shell));

    let single = startup_order(&doc, &[Role::Llm, Role::Llm]).unwrap();
    assert_eq!(single, vec![Role::Llm]);
}

#[test]
fn malformed_role_documents_are_rejected() {
    let mut value: serde_json::Value = serde_json::from_str(ROLES_JSON).unwrap();
    let roles = value["roles"].as_array_mut().unwrap();
    for r in roles.iter_mut() {
        if r["id"] == "input" {
            r["depends_on"] = serde_json::json!(["output"]);
        }
    }
    let cyclic = value.to_string();
    assert!(matches!(RolesDoc::parse(&cyclic), Err(TopoError::Cycle(_))));

    let mut value: serde_json::Value = serde_json::from_str(ROLES_JSON).unwrap();
    let roles = value["roles"].as_array_mut().unwrap();
    roles.retain(|r| r["id"] != "auxiliary");
    let doc = RolesDoc::parse(&value.to_string()).unwrap();
    assert!(matches!(
        startup_order(&doc, &[Role::Auxiliary]),
        Err(TopoError::UnknownRole(_))
    ));
    assert!(matches!(
        RolesDoc::parse("not json"),
        Err(TopoError::Parse(_))
    ));
}

#[test]
fn concurrent_start_intent_is_rejected_by_the_state_machine() {
    let mut state = LifecycleState::default();
    let first = state.allocate_ticket();
    let second = state.allocate_ticket();
    assert!(state.begin_start("official/input@1", first).is_ok());
    assert_eq!(
        state.begin_start("official/input@1", second),
        Err(StartError::AlreadyStarting)
    );
    assert_eq!(
        state.get("official/input@1").unwrap().phase,
        Phase::Starting
    );
    assert_eq!(state.running().len(), 1);
}

#[test]
fn stale_events_never_overwrite_a_new_start() {
    let mut state = LifecycleState::default();
    let first = state.allocate_ticket();
    state.begin_start("a", first).unwrap();
    assert_eq!(
        state.mark_healthy("a", first + 1),
        Err(StartError::StaleTicket)
    );
    assert!(!state.mark_exited_for("a", first + 1, Some(1)));
    assert_eq!(state.get("a").unwrap().phase, Phase::Starting);

    state.mark_stopped("a", first);
    let second = state.allocate_ticket();
    state.begin_start("a", second).unwrap();
    assert!(!state.mark_exited_for("a", first, Some(9)));
    assert_eq!(state.get("a").unwrap().phase, Phase::Starting);
    assert!(state.mark_exited_for("a", second, Some(9)));
    assert_eq!(state.get("a").unwrap().exit_code, Some(9));
}

#[test]
fn healthy_then_crash_marks_failed_with_exit_code() {
    let mut state = LifecycleState::default();
    let ticket = state.allocate_ticket();
    state.begin_start("a", ticket).unwrap();
    state.attach_process("a", ticket, 4242).unwrap();
    state
        .mark_probe("a", ticket, ProbeState::Healthy, ProbeState::Healthy)
        .unwrap();
    state.mark_healthy("a", ticket).unwrap();
    assert_eq!(state.get("a").unwrap().phase, Phase::Running);
    assert_eq!(state.get("a").unwrap().pid, Some(4242));

    state.mark_exited("a", Some(3));
    let snap = state.get("a").unwrap();
    assert_eq!(snap.phase, Phase::Failed);
    assert_eq!(snap.exit_code, Some(3));
    assert!(snap.last_error.as_deref().unwrap().contains("退出码 3"));
    assert_eq!(state.running().len(), 0);
}

#[test]
fn incompatible_contract_blocks_activation() {
    let mut state = LifecycleState::default();
    let ticket = state.allocate_ticket();
    state.begin_start("third/output@1", ticket).unwrap();
    state
        .mark_probe(
            "third/output@1",
            ticket,
            ProbeState::Healthy,
            ProbeState::Unhealthy,
        )
        .unwrap();
    assert_eq!(
        state.mark_healthy("third/output@1", ticket),
        Err(StartError::ProbeFailed)
    );
    assert_eq!(state.get("third/output@1").unwrap().phase, Phase::Failed);
    assert!(state.running().is_empty());
}

#[test]
fn graceful_stop_and_failed_instance_do_not_leak_into_running() {
    let mut state = LifecycleState::default();
    let ticket = state.allocate_ticket();
    state.begin_start("a", ticket).unwrap();
    state.mark_healthy("a", ticket).unwrap();
    assert_eq!(state.begin_stop("a"), Ok(()));
    assert!(state.running().contains("a"));
    state.mark_exited("a", Some(0));
    assert_eq!(state.get("a").unwrap().phase, Phase::Stopped);
    assert!(state.running().is_empty());
    assert_eq!(state.snapshot().len(), 1);
    state.remove("a");
    assert!(state.snapshot().is_empty());
}

#[test]
fn rotation_plan_is_pure_and_bounded() {
    assert!(!diagnostic_enabled());
    assert_eq!(DEFAULT_KEEP_FILES, 3);
    let files = vec![
        LogFile {
            path: PathBuf::from("a.log"),
            size: 10,
            modified_at: "2026-01-01T00:00:00Z".into(),
        },
        LogFile {
            path: PathBuf::from("b.log"),
            size: 10,
            modified_at: "2026-01-02T00:00:00Z".into(),
        },
        LogFile {
            path: PathBuf::from("c.log"),
            size: 10,
            modified_at: "2026-01-03T00:00:00Z".into(),
        },
    ];
    assert_eq!(
        rotation_plan(&files, DEFAULT_MAX_BYTES, DEFAULT_KEEP_FILES),
        Vec::<PathBuf>::new()
    );
    assert_eq!(
        rotation_plan(&files, 20, DEFAULT_KEEP_FILES),
        vec![PathBuf::from("a.log")]
    );
}

#[test]
fn activation_plan_and_rollback_are_symmetric() {
    let steps = activation_steps(Some("old"), "new");
    assert_eq!(steps[0], ActivationStep::StopPrevious);
    assert_eq!(steps[4], ActivationStep::CommitActive);
    for failed in [
        ActivationStep::StartNextOnCanonicalPort,
        ActivationStep::ProbeHealth,
        ActivationStep::ProbeContract,
        ActivationStep::CommitActive,
    ] {
        let recovery = rollback_steps(Some("old"), failed);
        assert!(recovery.contains(&RecoveryStep::RestorePrevious));
        assert!(recovery.contains(&RecoveryStep::RestoreActiveSlot));
    }
    assert_eq!(
        activation_steps(Some("same"), "same"),
        vec![ActivationStep::ProbeHealth, ActivationStep::ProbeContract]
    );
}

#[test]
fn unconfirmed_local_dir_never_enters_starting() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.py"), b"print('hi')").unwrap();
    let observed = observe_source(dir.path(), "2026-11-12T00:00:00Z").unwrap();
    assert!(require_confirmed(None, &observed).is_err());
    assert!(require_confirmed(Some("outdated"), &observed).is_err());

    let mut state = LifecycleState::default();
    let _ = state.allocate_ticket();
    assert!(state.snapshot().is_empty());
    assert_eq!(state.get("local/dir@1"), None);
}
