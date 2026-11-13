//! 生命周期运行时：退出监视、后台探测与状态事件。
//! 命令面只读内存快照，不在调用线程发 HTTP。

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, Read};
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

use crate::diag_log::{self, SharedLog};
use crate::process::win::{SpawnSpec, WinProcess};

pub const EVENT_NAME: &str = "lifecycle-changed";
pub const GRACEFUL_TIMEOUT: Duration = Duration::from_secs(3);
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
    /// 单次 HTTP 请求的超时。
    pub timeout_ms: u64,
    /// 从 spawn 到「health 必须通过」的总预算；超了只判「没起来」，不杀进程。
    pub startup_budget_ms: u64,
    /// 实例日志目录；为 None 时不落盘。
    pub log_dir: Option<PathBuf>,
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

/// 进程表之外、由外部启动但健康端点已通的实例（上游 D-4）。
///
/// 这些服务不在 Job Object 里，启动器既不该重复拉起，也无权停止。
/// 只在内存里标记，不落盘；下次启动前会重新探测，探不通就清掉标记。
fn external_snapshot(id: &str) -> Snapshot {
    Snapshot {
        instance_id: id.to_string(),
        phase: Phase::Running,
        ticket: 0,
        pid: None,
        health: ProbeState::Healthy,
        // 只探了 health，契约一致性未知，不冒充 Healthy。
        contract: ProbeState::NotChecked,
        exit_code: None,
        last_error: None,
    }
}

pub struct Supervisor {
    lifecycle: Arc<Mutex<LifecycleState>>,
    processes: Mutex<BTreeMap<String, WinProcess>>,
    roles_by_instance: Mutex<BTreeMap<String, Role>>,
    external: Mutex<BTreeSet<String>>,
    /// 每实例一份日志句柄，watcher 与探测线程共用。
    logs: Mutex<BTreeMap<String, SharedLog>>,
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
            external: Mutex::new(BTreeSet::new()),
            logs: Mutex::new(BTreeMap::new()),
            start_lock: Mutex::new(()),
            roles,
            app,
        })
    }

    pub fn state(&self) -> Arc<Mutex<LifecycleState>> {
        self.lifecycle.clone()
    }

    /// 视图用快照：进程表加上外部已运行的实例。
    pub fn snapshot(&self) -> BTreeMap<String, Snapshot> {
        let mut map = lock(&self.lifecycle).snapshot();
        for id in lock(&self.external).iter() {
            map.insert(id.clone(), external_snapshot(id));
        }
        map
    }

    /// 只看自己拉起的进程。卸载、一键停止等写操作用这个口径，
    /// 外部实例不算「由我管理」，启动器无权停掉别人的进程。
    pub fn running(&self) -> BTreeSet<String> {
        lock(&self.lifecycle).running()
    }

    pub fn is_external(&self, id: &str) -> bool {
        lock(&self.external).contains(id)
    }

    /// 这个实例的进程是不是由启动器拉起、且仍登记在册。
    ///
    /// 与 `running()` 的区别：状态机可能已经把它判成 Failed（探测超时），
    /// 进程却还活着。停止与「别再拉起一个」都要按这个口径判断。
    pub fn owns(&self, id: &str) -> bool {
        lock(&self.processes).contains_key(id)
    }

    /// 探测超时之后进程其实一直健康：把状态收回到 Running。
    ///
    /// 不这样做的话，界面会一直显示失败，用户再点「启动」又会被判成
    /// 「外部启动」，于是既不能停也不能重启。
    pub fn recover(&self, id: &str) -> Option<Snapshot> {
        if !self.owns(id) {
            return None;
        }
        let snap = {
            let mut state = lock(&self.lifecycle);
            state.mark_recovered(id).ok()?;
            state.get(id).cloned()
        };
        emit_snapshot(&self.app, id, snap.as_ref());
        snap
    }

    pub fn mark_external(&self, id: &str) {
        // 自己持有进程时绝不标外部：那会连带丢掉停止权，
        // 出现「启动器拉起的进程被当成别人的进程」这种自相矛盾的状态。
        if self.owns(id) {
            return;
        }
        lock(&self.external).insert(id.to_string());
        let snap = external_snapshot(id);
        emit_snapshot(&self.app, id, Some(&snap));
    }

    pub fn clear_external(&self, id: &str) {
        lock(&self.external).remove(id);
    }

    /// 启动前一次性短探：端口上已有健康服务就不重复拉起（上游 D-4）。
    ///
    /// 与后台探测分开：这里只发一次请求、不轮询，避免点「启动」先等几秒。
    /// 探测失败一律返回 false，交由正常启动路径处理，不吞掉真实错误。
    pub fn probe_external_once(url: &str, timeout_ms: u64) -> bool {
        let client = match reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(timeout_ms.clamp(100, 1500)))
            .build()
        {
            Ok(client) => client,
            Err(_) => return false,
        };
        matches!(
            client.get(url).send(),
            Ok(response) if response.status().is_success()
        )
    }

    pub fn roles(&self) -> &RolesDoc {
        &self.roles
    }

    /// 返回进入 Starting 的快照；health 与契约探测在后台收敛。
    pub fn start(&self, request: StartRequest) -> Result<Snapshot, String> {
        let _critical = lock(&self.start_lock);
        let id = request.instance_id.clone();
        let log = self.open_log(&id, request.log_dir.as_deref());

        let ticket = {
            let mut state = lock(&self.lifecycle);
            let ticket = state.allocate_ticket();
            state.begin_start(&id, ticket).map_err(|e| e.to_string())?;
            ticket
        };
        // 状态机接受之后才记「start」：并发点击被 AlreadyStarting 挡掉的那些
        // 请求不该在日志里留下看似真的启动记录。
        if let Some(log) = &log {
            diag_log::write(
                log,
                "lifecycle",
                &format!(
                    "start role={:?} program={} health={}",
                    request.role,
                    request.spec.program.display(),
                    request.health_url.as_deref().unwrap_or("-")
                ),
            );
        }

        let mut process = match WinProcess::spawn(&request.spec) {
            Ok(process) => process,
            Err(error) => {
                if let Some(log) = &log {
                    diag_log::write(log, "lifecycle", &format!("spawn failed: {error}"));
                }
                let mut state = lock(&self.lifecycle);
                let _ = state.mark_failed(&id, ticket, &error.to_string());
                emit_snapshot(&self.app, &id, state.get(&id));
                return Err(error.to_string());
            }
        };

        let pid = process.pid;
        if let Some(log) = &log {
            diag_log::write(log, "lifecycle", &format!("spawned pid={pid}"));
            if let Some((stdout, stderr)) = process.take_output() {
                spawn_pump(log.clone(), "stdout", stdout);
                spawn_pump(log.clone(), "stderr", stderr);
            }
        }
        {
            let mut state = lock(&self.lifecycle);
            state
                .attach_process(&id, ticket, pid)
                .map_err(|e| e.to_string())?;
        }
        self.spawn_watcher(&id, ticket, process.watcher_slot(), log.clone());
        self.spawn_probe(&id, ticket, &request, log.clone());
        lock(&self.processes).insert(id.clone(), process);
        lock(&self.roles_by_instance).insert(id.clone(), request.role);
        if let Some(log) = log {
            lock(&self.logs).insert(id.clone(), log);
        }
        // 真正接管成功，外部标记失效。
        self.clear_external(&id);

        let snapshot = {
            let state = lock(&self.lifecycle);
            let snap = state.get(&id).cloned();
            emit_snapshot(&self.app, &id, snap.as_ref());
            snap
        };
        snapshot.ok_or_else(|| "状态机缺少刚登记的实例".into())
    }

    /// 打开（或复用）实例日志。目录不可写时退化为不落盘，不阻断启动。
    fn open_log(&self, id: &str, dir: Option<&std::path::Path>) -> Option<SharedLog> {
        if let Some(existing) = lock(&self.logs).get(id) {
            return Some(existing.clone());
        }
        let dir = dir?;
        diag_log::open_shared(dir).ok()
    }

    fn log_line(&self, id: &str, scope: &str, message: &str) {
        let log = lock(&self.logs).get(id).cloned();
        if let Some(log) = log {
            diag_log::write(&log, scope, message);
        }
    }

    pub fn stop(&self, id: &str) -> Result<Snapshot, String> {
        if self.is_external(id) {
            return Err(format!(
                "实例 {id} 由外部启动，启动器无权停止；请在原进程里关闭"
            ));
        }
        let ticket = {
            let mut state = lock(&self.lifecycle);
            state.begin_stop(id).map_err(|e| e.to_string())?;
            state.get(id).map(|s| s.ticket).unwrap_or(0)
        };
        self.log_line(id, "lifecycle", "stop requested");

        let outcome = {
            let processes = lock(&self.processes);
            match processes.get(id) {
                Some(process) => process.stop_graceful(GRACEFUL_TIMEOUT),
                None => Ok(()),
            }
        };
        lock(&self.processes).remove(id);
        match &outcome {
            Ok(()) => self.log_line(id, "lifecycle", "stopped"),
            Err(error) => self.log_line(id, "lifecycle", &format!("stop failed: {error}")),
        }

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

    fn spawn_watcher(
        &self,
        id: &str,
        ticket: u64,
        slot: Arc<Mutex<Option<Child>>>,
        log: Option<SharedLog>,
    ) {
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
                if let Some(log) = &log {
                    let text = match code {
                        Some(code) => format!("exited code={code}"),
                        // 系统强杀拿不到退出码，不能写成 0 冒充正常退出。
                        None => "exited without an exit code".to_string(),
                    };
                    diag_log::write(log, "lifecycle", &text);
                }
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

    fn spawn_probe(&self, id: &str, ticket: u64, request: &StartRequest, log: Option<SharedLog>) {
        let state = self.lifecycle.clone();
        let app = self.app.clone();
        let id = id.to_string();
        let health_url = request.health_url.clone();
        let contract_url = request.contract_url.clone();
        let role = request.role;
        let single = Duration::from_millis(request.timeout_ms.clamp(100, 3000));
        // 启动预算来自 roles 契约，不再用固定的 4 秒：Python 冷启动常常更久，
        // 提前判死会让用户看到「启动失败」，而服务几秒后其实就通了。
        let budget = Duration::from_millis(request.startup_budget_ms.clamp(1_000, 600_000));

        std::thread::spawn(move || {
            let started = Instant::now();
            let deadline = started + budget;
            let client = reqwest::blocking::Client::builder()
                .timeout(single)
                .build()
                .ok();

            // 先只等 health：没起来就探契约，每次都要白等一个请求超时。
            let health = match (&client, &health_url) {
                (Some(client), Some(url)) => probe_health(client, url, deadline),
                _ => ProbeState::Healthy,
            };
            let contract = match (&client, &contract_url) {
                (Some(client), Some(url)) if health == ProbeState::Healthy => {
                    probe_contract(client, url, role, deadline)
                }
                _ => health,
            };

            if let Some(log) = &log {
                diag_log::write(
                    log,
                    "probe",
                    &format!(
                        "health={health:?} contract={contract:?} after={}ms",
                        started.elapsed().as_millis()
                    ),
                );
            }

            let snap = {
                let mut guard = lock(&state);
                if health == ProbeState::Unhealthy {
                    // 进程可能还活着，只是没在预算内就绪：保留 pid，交给用户决定等待还是停止。
                    let reason = format!(
                        "启动超时：{} 秒内健康检查未通过，进程仍在运行，可稍后重试或先停止",
                        budget.as_secs()
                    );
                    let _ = guard.mark_probe_timeout(&id, ticket, &reason);
                } else if guard.mark_probe(&id, ticket, health, contract).is_ok() {
                    let _ = guard.mark_healthy(&id, ticket);
                }
                guard.get(&id).cloned()
            };
            emit_snapshot(&app, &id, snap.as_ref());
        });
    }
}

/// 把子进程输出按行泵进日志。管道关闭即线程结束，不另设停止信号。
fn spawn_pump(log: SharedLog, scope: &'static str, reader: impl Read + Send + 'static) {
    std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(reader);
        let mut buffer = Vec::new();
        loop {
            buffer.clear();
            // 用字节读到换行再解码：子进程可能输出非 UTF-8，不能让解码失败终止日志。
            match reader.read_until(b'\n', &mut buffer) {
                Ok(0) => break,
                Ok(_) => {
                    let text = String::from_utf8_lossy(&buffer);
                    diag_log::write(&log, scope, text.trim_end_matches(['\r', '\n']));
                }
                Err(_) => break,
            }
        }
    });
}

fn probe_health(client: &reqwest::blocking::Client, url: &str, deadline: Instant) -> ProbeState {
    let started = Instant::now();
    loop {
        if let Ok(response) = client.get(url).send() {
            if response.status().is_success() {
                return ProbeState::Healthy;
            }
        }
        if Instant::now() >= deadline {
            return ProbeState::Unhealthy;
        }
        std::thread::sleep(probe_gap(started.elapsed()));
    }
}

/// 探测间隔：前几秒抓得紧，之后放开。
///
/// 冷启动窗口里服务随时可能起来，50ms 一轮能少等；一分钟的预算若一直
/// 50ms 一轮就是一千多个请求，进程刚起来时反而在跟自己的 init 抢 CPU。
fn probe_gap(elapsed: Duration) -> Duration {
    if elapsed < Duration::from_secs(5) {
        POLL_INTERVAL
    } else {
        Duration::from_millis(400)
    }
}

fn probe_contract(
    client: &reqwest::blocking::Client,
    url: &str,
    role: Role,
    deadline: Instant,
) -> ProbeState {
    let started = Instant::now();
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
        std::thread::sleep(probe_gap(started.elapsed()));
    }
}
