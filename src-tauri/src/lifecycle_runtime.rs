//! 生命周期运行时：退出监视、后台探测与状态事件。
//! 命令面只读内存快照，不在调用线程发 HTTP。

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Child;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use launcher_core::lifecycle::{
    startup_order, LifecycleState, Phase, ProbeState, RolesDoc, Snapshot,
};
use launcher_core::m2::contract_probe::{evaluate, Compatibility};
use launcher_core::manifest::Role;
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter};

use crate::process::win::{SpawnSpec, WinProcess};

pub const EVENT_NAME: &str = "lifecycle-changed";
pub const GRACEFUL_TIMEOUT: Duration = Duration::from_secs(3);
pub const PROBE_BUDGET: Duration = Duration::from_secs(4);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Serialize)]
pub struct LifecycleEvent {
    pub instance_id: String,
    pub phase: Phase,
    pub pid: Option<u32>,
    pub health: ProbeState,
    pub contract: ProbeState,
    pub exit_code: Option<i32>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StartRequest {
    pub instance_id: String,
    pub role: Role,
    pub spec: SpawnSpec,
    /// shell 与 auxiliary 为 None，不设健康门禁。
    pub health_url: Option<String>,
    pub contract_url: Option<String>,
    pub timeout_ms: u64,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn lock_child(slot: &Arc<Mutex<Option<Child>>>) -> MutexGuard<'_, Option<Child>> {
    slot.lock().unwrap_or_else(|e| e.into_inner())
}

fn emit_snapshot(app: &AppHandle, id: &str, snapshot: Option<&Snapshot>) {
    let Some(snap) = snapshot else { return };
    let event = LifecycleEvent {
        instance_id: id.to_string(),
        phase: snap.phase,
        pid: snap.pid,
        health: snap.health,
        contract: snap.contract,
        exit_code: snap.exit_code,
        last_error: snap.last_error.clone(),
    };
    let _ = app.emit(EVENT_NAME, event);
}

pub struct Supervisor {
    lifecycle: Arc<Mutex<LifecycleState>>,
    processes: Mutex<BTreeMap<String, WinProcess>>,
    roles_by_instance: Mutex<BTreeMap<String, Role>>,
    /// 包住 begin_start 到进程登记，状态机本身不代替临界区。
    start_lock: Mutex<()>,
    roles: RolesDoc,
    app: AppHandle,
}

impl Supervisor {
    pub fn new(app: AppHandle) -> Result<Self, String> {
        let roles = launcher_core::lifecycle::embedded_roles().map_err(|e| e.to_string())?;
        Ok(Self {
            lifecycle: Arc::new(Mutex::new(LifecycleState::default())),
            processes: Mutex::new(BTreeMap::new()),
            roles_by_instance: Mutex::new(BTreeMap::new()),
            start_lock: Mutex::new(()),
            roles,
            app,
        })
    }

    pub fn state(&self) -> Arc<Mutex<LifecycleState>> {
        self.lifecycle.clone()
    }

    pub fn snapshot(&self) -> BTreeMap<String, Snapshot> {
        lock(&self.lifecycle).snapshot()
    }

    pub fn running(&self) -> BTreeSet<String> {
        lock(&self.lifecycle).running()
    }

    pub fn roles(&self) -> &RolesDoc {
        &self.roles
    }

    /// 返回进入 Starting 的快照；health 与契约探测在后台收敛。
    pub fn start(&self, request: StartRequest) -> Result<Snapshot, String> {
        let _critical = lock(&self.start_lock);
        let id = request.instance_id.clone();
        let ticket = {
            let mut state = lock(&self.lifecycle);
            let ticket = state.allocate_ticket();
            state.begin_start(&id, ticket).map_err(|e| e.to_string())?;
            ticket
        };

        let process = match WinProcess::spawn(&request.spec) {
            Ok(process) => process,
            Err(error) => {
                let mut state = lock(&self.lifecycle);
                let _ = state.mark_failed(&id, ticket, &error.to_string());
                emit_snapshot(&self.app, &id, state.get(&id));
                return Err(error.to_string());
            }
        };

        let pid = process.pid;
        {
            let mut state = lock(&self.lifecycle);
            state
                .attach_process(&id, ticket, pid)
                .map_err(|e| e.to_string())?;
        }
        self.spawn_watcher(&id, ticket, process.watcher_slot());
        self.spawn_probe(&id, ticket, &request);
        lock(&self.processes).insert(id.clone(), process);
        lock(&self.roles_by_instance).insert(id.clone(), request.role);

        let snapshot = {
            let state = lock(&self.lifecycle);
            let snap = state.get(&id).cloned();
            emit_snapshot(&self.app, &id, snap.as_ref());
            snap
        };
        snapshot.ok_or_else(|| "状态机缺少刚登记的实例".into())
    }

    pub fn stop(&self, id: &str) -> Result<Snapshot, String> {
        let ticket = {
            let mut state = lock(&self.lifecycle);
            state.begin_stop(id).map_err(|e| e.to_string())?;
            state.get(id).map(|s| s.ticket).unwrap_or(0)
        };

        let outcome = {
            let processes = lock(&self.processes);
            match processes.get(id) {
                Some(process) => process.stop_graceful(GRACEFUL_TIMEOUT),
                None => Ok(()),
            }
        };
        lock(&self.processes).remove(id);

        let snapshot = {
            let mut state = lock(&self.lifecycle);
            match &outcome {
                Ok(()) => state.mark_stopped(id, ticket),
                Err(error) => {
                    let _ = state.mark_failed(id, ticket, &error.to_string());
                }
            }
            let snap = state.get(id).cloned();
            emit_snapshot(&self.app, id, snap.as_ref());
            snap
        };
        match outcome {
            Ok(()) => snapshot.ok_or_else(|| "状态机缺少刚停止的实例".into()),
            Err(error) => Err(error.to_string()),
        }
    }

    /// 按拓扑逆序停；退出钩子复用同一入口。
    pub fn stop_all(&self) -> Vec<Snapshot> {
        let running = self.running();
        let all = [
            Role::Input,
            Role::Llm,
            Role::Output,
            Role::Shell,
            Role::Auxiliary,
        ];
        let order = startup_order(&self.roles, &all).unwrap_or_default();
        let role_of = lock(&self.roles_by_instance).clone();
        let mut ids: Vec<String> = Vec::new();
        for role in order.iter().rev() {
            for (id, instance_role) in &role_of {
                if instance_role == role && running.contains(id) {
                    ids.push(id.clone());
                }
            }
        }
        for id in &running {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        ids.iter().filter_map(|id| self.stop(id).ok()).collect()
    }

    fn spawn_watcher(&self, id: &str, ticket: u64, slot: Arc<Mutex<Option<Child>>>) {
        let state = self.lifecycle.clone();
        let app = self.app.clone();
        let id = id.to_string();
        std::thread::spawn(move || loop {
            let outcome = {
                let mut guard = lock_child(&slot);
                match guard.as_mut() {
                    Some(child) => match child.try_wait() {
                        Ok(Some(status)) => {
                            *guard = None;
                            Some(status.code())
                        }
                        Ok(None) => None,
                        Err(_) => {
                            *guard = None;
                            Some(None)
                        }
                    },
                    None => Some(None),
                }
            };
            if let Some(code) = outcome {
                let snap = {
                    let mut guard = lock(&state);
                    guard.mark_exited_for(&id, ticket, code);
                    guard.get(&id).cloned()
                };
                emit_snapshot(&app, &id, snap.as_ref());
                break;
            }
            std::thread::sleep(POLL_INTERVAL);
        });
    }

    fn spawn_probe(&self, id: &str, ticket: u64, request: &StartRequest) {
        let state = self.lifecycle.clone();
        let app = self.app.clone();
        let id = id.to_string();
        let health_url = request.health_url.clone();
        let contract_url = request.contract_url.clone();
        let role = request.role;
        let single = Duration::from_millis(request.timeout_ms.clamp(100, 3000));

        std::thread::spawn(move || {
            let deadline = Instant::now() + PROBE_BUDGET;
            let client = reqwest::blocking::Client::builder()
                .timeout(single)
                .build()
                .ok();

            let health = match (&client, &health_url) {
                (Some(client), Some(url)) => probe_health(client, url, deadline),
                _ => ProbeState::Healthy,
            };
            let contract = match (&client, &contract_url) {
                (Some(client), Some(url)) => probe_contract(client, url, role, deadline),
                _ => ProbeState::Healthy,
            };

            let snap = {
                let mut guard = lock(&state);
                if guard.mark_probe(&id, ticket, health, contract).is_ok() {
                    let _ = guard.mark_healthy(&id, ticket);
                }
                guard.get(&id).cloned()
            };
            emit_snapshot(&app, &id, snap.as_ref());
        });
    }
}

fn probe_health(client: &reqwest::blocking::Client, url: &str, deadline: Instant) -> ProbeState {
    loop {
        if let Ok(response) = client.get(url).send() {
            if response.status().is_success() {
                return ProbeState::Healthy;
            }
        }
        if Instant::now() >= deadline {
            return ProbeState::Unhealthy;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn probe_contract(
    client: &reqwest::blocking::Client,
    url: &str,
    role: Role,
    deadline: Instant,
) -> ProbeState {
    loop {
        if let Ok(response) = client.get(url).send() {
            if response.status().is_success() {
                if let Ok(document) = response.json::<Value>() {
                    return match evaluate(role, &document) {
                        Compatibility::Compatible => ProbeState::Healthy,
                        Compatibility::Incompatible(_) => ProbeState::Unhealthy,
                    };
                }
            }
        }
        if Instant::now() >= deadline {
            return ProbeState::Unhealthy;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// 启动器数据根下的状态文件路径，命令面据此读写 installed.json 与 retention.json。
pub fn state_file(data_root: &std::path::Path, name: &str) -> PathBuf {
    data_root.join("state").join(name)
}
