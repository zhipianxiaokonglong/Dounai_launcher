//! M3 命令面：命令名、参数与返回形状与 v1 完全一致，内部改走 v2。
//!
//! 这样 `src/bridge.ts` 不需要改一行；v1 命令面已移除，这些 DTO 是唯一真源，
//! 不会出现「Rust 改了形状、TS typecheck 查不出来」。
//!
//! 边界：状态文件只由 `AppStateV2` 写；进程只由 `Supervisor` 管；
//! 验签、目录确认、端口分配与启动编排分别由 core / `lifecycle_ops` 负责。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tauri::{Manager, State};

use launcher_core::lifecycle::{
    activation_steps, rollback_steps, ActivationStep, Phase, ProbeState, RecoveryStep, RolesDoc,
    Snapshot,
};
use launcher_core::m2::adapters::RemoteCatalogAdapter;
use launcher_core::m2::catalog::{select_catalog, CatalogState};
use launcher_core::m2::installed::Instance;
use launcher_core::m2::local::{observe_source, validate_local_dir};
use launcher_core::m2::manifest::PackageManifest;
use launcher_core::m2::package::{install_package, verify_zip, ZipExpectation};
use launcher_core::m2::source::{validate_download_url, SourceAdapter};
use launcher_core::m2::trust::{classify, ClassifyContext};
use launcher_core::m2::{Category, InstanceSourceKind, Verdict, MAX_UNPACKED_BYTES};
use launcher_core::manifest::{ModuleManifestV2, Role};

use crate::lifecycle_ops::{
    assign_ports, build_start_request, ordered_instances, package_dir, write_manifest,
    MANIFEST_FILE,
};
use crate::lifecycle_runtime::Supervisor;
use crate::m2_http::{ReqwestCatalogFetcher, ALL_HOST_SUFFIXES};
use crate::state_v2::{bare_sha256, prefixed_sha256, AppStateV2};

/// 启动前外部服务探测预算：只发一次请求，超时就走正常启动路径。
const PRESTART_TIMEOUT_MS: u64 = 600;

/* ------------------------------------------------------------------ */
/* 与 v1 等价的返回形状                                                */
/* ------------------------------------------------------------------ */

/// 保留 v1 的四个取值，让前端 union 不用改；当前只有 installed 会被构造。
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallStateView {
    NotInstalled,
    Installed,
    Installing,
    Broken,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStateView {
    Stopped,
    Starting,
    Running,
    Failed,
}

#[derive(Debug, Serialize)]
pub struct ModuleView {
    pub module_id: String,
    pub install: InstallStateView,
    pub enabled: bool,
    pub version: String,
    pub path: String,
    pub run: RunStateView,
    pub last_error: Option<String>,
    pub pid: Option<u32>,
    pub healthy: bool,
    pub linked: bool,
    pub external: bool,
}

#[derive(Debug, Serialize)]
pub struct RunView {
    pub module_id: String,
    pub run: RunStateView,
    pub pid: Option<u32>,
    pub last_error: Option<String>,
    pub healthy: bool,
    /// 服务健康但不在本启动器进程表里：由外部启动，无权停止（上游 D-4）。
    #[serde(default)]
    pub external: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrustVerdictView {
    Signed { key_index: usize },
    UnsignedAllowed,
}

#[derive(Debug, Serialize)]
pub struct CatalogEntryView {
    pub module_id: String,
    pub name: String,
    pub version: String,
    pub download_url: String,
    pub sha256: String,
}

#[derive(Debug, Serialize)]
pub struct CatalogResponse {
    pub source: String,
    pub entries: Vec<CatalogEntryView>,
    pub allowlist: Vec<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ScanCandidate {
    pub path: String,
    pub module_id: String,
    pub name: String,
    pub version: String,
    pub category: String,
    pub signed: bool,
    pub registered: bool,
}

#[derive(Debug, Serialize)]
pub struct BulkAddResult {
    pub module_id: String,
    pub path: String,
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModuleManifestView {
    pub module_id: String,
    pub manifest: Option<serde_json::Value>,
    pub error: Option<String>,
    pub root: String,
}

#[derive(Debug, Serialize)]
pub struct PurgeResponse {
    pub removed_caches: Vec<String>,
    pub state_cleared: bool,
    pub removed_module_dirs: Vec<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct DownloadResponse {
    pub path: String,
    pub bytes: u64,
    pub source: String,
    pub resumed_from: u64,
    pub attempts: usize,
}

#[derive(Debug, Serialize)]
pub struct InstallResponse {
    pub module_id: String,
    pub files: usize,
    pub bytes: u64,
    pub replaced: bool,
    pub install_dir: String,
    pub trust: TrustVerdictView,
}

#[derive(Debug, Serialize)]
pub struct TrustInfo {
    pub trusted_keys: usize,
    pub allow_unsigned: bool,
    pub open: bool,
}

#[derive(Debug, Serialize)]
pub struct DevModeInfo {
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct InstanceLogs {
    pub instance_id: String,
    pub lines: Vec<String>,
    /// 保留窗口内的日志文件数。
    pub files: usize,
    /// 是否截掉了更早的内容。
    pub truncated: bool,
    pub note: String,
}

#[derive(Debug, Serialize)]
pub struct KeyInfo {
    pub fingerprint: String,
    pub accepted: bool,
    pub note: String,
}

/* ------------------------------------------------------------------ */
/* 请求形状（字段名沿用 v1，新增字段一律可选）                         */
/* ------------------------------------------------------------------ */

#[derive(Debug, Deserialize)]
pub struct SetEnabledRequest {
    pub module_id: String,
    pub enabled: bool,
    #[serde(default)]
    pub category: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UninstallRequest {
    pub module_id: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub remove_files: bool,
}

#[derive(Debug, Deserialize)]
pub struct InstallRequest {
    pub module_id: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    pub zip_base64: String,
    pub expected_sha256: String,
    /// v1 遗留字段：签名判定一律以包内 manifest 为准，这里只用来传用户确认。
    #[serde(default)]
    pub manifest: serde_json::Value,
    /// B/C 类安装必须显式确认；缺省即拒绝，不能靠「不填就放行」。
    #[serde(default)]
    pub confirm: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct AddLocalModuleRequest {
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct BulkAddLocalModulesRequest {
    pub paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct OpenAdminUiRequest {
    pub module_id: String,
}

#[derive(Debug, Deserialize)]
pub struct PurgeRequest {
    #[serde(default)]
    pub caches: bool,
    #[serde(default)]
    pub state: bool,
    #[serde(default)]
    pub module_files: bool,
}

#[derive(Debug, Deserialize)]
pub struct DownloadRequest {
    pub module_id: String,
    pub sources: Vec<String>,
    pub expected_sha256: String,
    #[serde(default = "default_retries")]
    pub retries: usize,
}

fn default_retries() -> usize {
    2
}

#[derive(Debug, Deserialize)]
pub struct KeyRequest {
    pub fingerprint: String,
    #[serde(default)]
    pub label: Option<String>,
}

/* ------------------------------------------------------------------ */
/* 视图转换                                                            */
/* ------------------------------------------------------------------ */

fn phase_to_run(phase: Option<Phase>) -> RunStateView {
    match phase {
        Some(Phase::Starting) => RunStateView::Starting,
        Some(Phase::Running) => RunStateView::Running,
        Some(Phase::Failed) => RunStateView::Failed,
        _ => RunStateView::Stopped,
    }
}

fn run_view(
    module_id: &str,
    snapshot: Option<&Snapshot>,
    fallback_error: Option<String>,
) -> RunView {
    match snapshot {
        Some(snap) => RunView {
            module_id: module_id.to_string(),
            run: phase_to_run(Some(snap.phase)),
            pid: snap.pid,
            last_error: snap.last_error.clone().or(fallback_error),
            healthy: snap.health == ProbeState::Healthy
                || (snap.phase == Phase::Running && snap.health == ProbeState::NotChecked),
            external: false,
        },
        None => RunView {
            module_id: module_id.to_string(),
            run: RunStateView::Stopped,
            pid: None,
            last_error: fallback_error,
            healthy: false,
            external: false,
        },
    }
}

/// 外部已运行：健康、无 pid、不可由启动器停止。
fn external_run_view(module_id: &str) -> RunView {
    RunView {
        module_id: module_id.to_string(),
        run: RunStateView::Running,
        pid: None,
        last_error: None,
        healthy: true,
        external: true,
    }
}

fn module_view(state: &AppStateV2, instance: &Instance, supervisor: &Supervisor) -> ModuleView {
    let snapshot = supervisor.snapshot().remove(&instance.instance_id);
    let view = run_view(&instance.module_id, snapshot.as_ref(), None);
    let linked = instance.source_kind == InstanceSourceKind::Dir;
    ModuleView {
        module_id: instance.module_id.clone(),
        install: InstallStateView::Installed,
        enabled: !state.is_disabled(&instance.module_id),
        version: instance.version.clone(),
        path: state.module_dir(instance).to_string_lossy().into_owned(),
        run: view.run,
        last_error: view.last_error,
        pid: view.pid,
        healthy: view.healthy,
        linked,
        // 启动前短探发现端口上已有健康服务时置 true（上游 D-4）。
        // 快照里也已并入合成的 Running/Healthy 状态，卡片不会显示「启动失败」。
        external: supervisor.is_external(&instance.instance_id),
    }
}

/* ------------------------------------------------------------------ */
/* 生命周期                                                            */
/* ------------------------------------------------------------------ */

#[tauri::command]
pub fn start_module(
    module_id: String,
    state: State<'_, AppStateV2>,
    supervisor: State<'_, Supervisor>,
) -> Result<RunView, String> {
    let instance = state.resolve_instance(&module_id)?;
    let port = single_instance_port(&state, &supervisor, &instance);
    Ok(
        match start_instance(&state, &supervisor, &instance, port)? {
            StartOutcome::Managed(snap) => run_view(&instance.module_id, Some(&snap), None),
            StartOutcome::External => external_run_view(&instance.module_id),
        },
    )
}

/// 启动结果：由本启动器托管，或端口上已有外部健康服务。
enum StartOutcome {
    Managed(Snapshot),
    External,
}

/// 单实例启动时的端口；与 start_all 的整体分配保持同一套规则。
fn single_instance_port(
    state: &AppStateV2,
    supervisor: &Supervisor,
    instance: &Instance,
) -> Option<u16> {
    assign_ports(state, supervisor.roles(), std::slice::from_ref(instance))
        .get(&instance.instance_id)
        .copied()
        .flatten()
}

/// 按 roles 契约拼出启动前健康探测地址；无 health 端点的角色返回 None。
fn prestart_health_url(roles: &RolesDoc, instance: &Instance, port: Option<u16>) -> Option<String> {
    let port = port?;
    roles
        .health(instance.role)
        .map(|h| format!("http://127.0.0.1:{port}{}", h.path))
}

fn start_instance(
    state: &AppStateV2,
    supervisor: &Supervisor,
    instance: &Instance,
    port: Option<u16>,
) -> Result<StartOutcome, String> {
    // 上游 D-4：进程表里没有，但服务可能由用户自己拉起，或是启动器重启后
    // 留下的孤儿。直接 spawn 会因端口占用立刻退出（退出码 3），界面误报
    // 「启动失败」，而服务其实一直健康。先按 roles 的 health 路径短探一次。
    if let Some(url) = prestart_health_url(supervisor.roles(), instance, port) {
        if Supervisor::probe_external_once(&url, PRESTART_TIMEOUT_MS) {
            supervisor.mark_external(&instance.instance_id);
            return Ok(StartOutcome::External);
        }
    }
    // 探不通就把旧的外部标记清掉，允许接管这个端口。
    supervisor.clear_external(&instance.instance_id);

    let plan = build_start_request(state, instance, port, supervisor.roles())?;
    supervisor.start(plan.request).map(StartOutcome::Managed)
}

#[tauri::command]
pub fn stop_module(
    module_id: String,
    state: State<'_, AppStateV2>,
    supervisor: State<'_, Supervisor>,
) -> Result<RunView, String> {
    let instance = state.resolve_instance(&module_id)?;
    let snap = supervisor.stop(&instance.instance_id)?;
    Ok(run_view(&module_id, Some(&snap), None))
}

#[tauri::command]
pub fn start_all(state: State<'_, AppStateV2>, supervisor: State<'_, Supervisor>) -> Vec<RunView> {
    let instances: Vec<Instance> = state.with_installed(|s| {
        s.instances
            .values()
            .filter(|i| !state.is_disabled(&i.module_id))
            .cloned()
            .collect()
    });
    let ordered = match ordered_instances(supervisor.roles(), instances) {
        Ok(list) => list,
        Err(error) => return vec![run_view("start_all", None, Some(error))],
    };
    let ports = assign_ports(&state, supervisor.roles(), &ordered);
    let mut out = Vec::with_capacity(ordered.len());
    for instance in &ordered {
        // 整批分配端口仍在这里做，start_instance 只负责单个实例的启动前探测。
        let port = ports.get(&instance.instance_id).copied().flatten();
        match start_instance(&state, &supervisor, instance, port) {
            Ok(StartOutcome::Managed(snap)) => {
                out.push(run_view(&instance.module_id, Some(&snap), None))
            }
            Ok(StartOutcome::External) => out.push(external_run_view(&instance.module_id)),
            Err(error) => out.push(run_view(&instance.module_id, None, Some(error))),
        }
    }
    out
}

#[tauri::command]
pub fn stop_all(state: State<'_, AppStateV2>, supervisor: State<'_, Supervisor>) -> Vec<RunView> {
    supervisor
        .stop_all()
        .into_iter()
        .map(|snap| {
            let module_id = state
                .instance(&snap.instance_id)
                .map(|i| i.module_id)
                .unwrap_or_else(|| snap.instance_id.clone());
            run_view(&module_id, Some(&snap), None)
        })
        .collect()
}

#[tauri::command]
pub fn restart_instance(
    instance_id: String,
    state: State<'_, AppStateV2>,
    supervisor: State<'_, Supervisor>,
) -> Result<RunView, String> {
    let instance = state
        .instance(&instance_id)
        .ok_or_else(|| format!("实例不存在：{instance_id}"))?;
    if supervisor.is_external(&instance_id) {
        return Err(format!("实例 {instance_id} 由外部启动，启动器无法重启"));
    }
    if supervisor.running().contains(&instance_id) {
        supervisor.stop(&instance_id)?;
    }
    let port = single_instance_port(&state, &supervisor, &instance);
    Ok(
        match start_instance(&state, &supervisor, &instance, port)? {
            StartOutcome::Managed(snap) => run_view(&instance.module_id, Some(&snap), None),
            StartOutcome::External => external_run_view(&instance.module_id),
        },
    )
}

#[tauri::command]
pub fn get_lifecycle_snapshot(supervisor: State<'_, Supervisor>) -> Vec<Snapshot> {
    supervisor.snapshot().into_values().collect()
}

/* ------------------------------------------------------------------ */
/* 状态查询与开关                                                      */
/* ------------------------------------------------------------------ */

#[tauri::command]
pub fn list_module_states(
    state: State<'_, AppStateV2>,
    supervisor: State<'_, Supervisor>,
) -> Result<Vec<ModuleView>, String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let ids: Vec<String> = state.with_installed(|s| s.instances.keys().cloned().collect());
    for id in ids {
        let Some(instance) = state.instance(&id) else {
            continue;
        };
        if !seen.insert(instance.module_id.clone()) {
            continue;
        }
        // 多实例时展示 active 槽指向的那个，与启停命令的解析保持一致。
        let chosen = state
            .resolve_instance(&instance.module_id)
            .unwrap_or(instance);
        out.push(module_view(&state, &chosen, &supervisor));
    }
    Ok(out)
}

#[tauri::command]
pub fn list_instances(state: State<'_, AppStateV2>) -> Vec<Instance> {
    state.with_installed(|s| s.instances.values().cloned().collect())
}

/// 界面一次拿齐渲染一张卡片所需的全部事实。
///
/// 光有 `Instance` 不够：端口由 `assign_ports` 按 active / 备用端口段分配，
/// 前端自己推算必然与真实启动值分叉（M4 的卡片要显示端口）。
/// 所以端口只从这里出，前端不复制分配规则。
#[derive(Debug, Serialize)]
pub struct InstanceRuntimeView {
    pub instance: Instance,
    /// 该实例真正会在用的端口；`shell` 与未分配备用端口时为 None。
    pub port: Option<u16>,
    /// 是否为所在 role 的 active 槽实例。
    pub active: bool,
    /// 模块级停用标记（按 module_id 记录，不是按实例）。
    pub disabled: bool,
    /// 生命周期快照；启动器没见过的实例为 None。
    pub snapshot: Option<Snapshot>,
}

#[tauri::command]
pub fn list_instance_views(
    state: State<'_, AppStateV2>,
    supervisor: State<'_, Supervisor>,
) -> Vec<InstanceRuntimeView> {
    let mut instances: Vec<Instance> =
        state.with_installed(|s| s.instances.values().cloned().collect());
    instances.sort_by(|a, b| a.instance_id.cmp(&b.instance_id));
    let ports = assign_ports(&state, supervisor.roles(), &instances);
    let active = state.with_installed(|s| s.active.clone());
    let disabled: BTreeSet<String> = instances
        .iter()
        .map(|i| i.module_id.clone())
        .filter(|id| state.is_disabled(id))
        .collect();
    compose_instance_views(
        instances,
        &ports,
        &active,
        &disabled,
        &supervisor.snapshot(),
    )
}

/// 组合逻辑单独成函数：命令层只取数据，排序、端口回填与 active 判定都在这里，
/// 便于用普通单测覆盖（`State` 在测试里不可构造）。
fn compose_instance_views(
    instances: Vec<Instance>,
    ports: &std::collections::BTreeMap<String, Option<u16>>,
    active: &std::collections::BTreeMap<String, String>,
    disabled: &BTreeSet<String>,
    snapshots: &std::collections::BTreeMap<String, Snapshot>,
) -> Vec<InstanceRuntimeView> {
    instances
        .into_iter()
        .map(|instance| InstanceRuntimeView {
            port: ports.get(&instance.instance_id).copied().flatten(),
            active: active.values().any(|id| id == &instance.instance_id),
            disabled: disabled.contains(&instance.module_id),
            snapshot: snapshots.get(&instance.instance_id).cloned(),
            instance,
        })
        .collect()
}

#[tauri::command]
pub fn set_module_enabled(
    state: State<'_, AppStateV2>,
    request: SetEnabledRequest,
) -> Result<(), String> {
    state
        .resolve_instance(&request.module_id)
        .map(|_| state.set_disabled(&request.module_id, !request.enabled))
}

#[tauri::command]
pub fn uninstall_module(
    state: State<'_, AppStateV2>,
    supervisor: State<'_, Supervisor>,
    request: UninstallRequest,
) -> Result<(), String> {
    let instance = state.resolve_instance(&request.module_id)?;
    if supervisor.running().contains(&instance.instance_id) {
        return Err(format!(
            "实例 {} 正在运行，请先停止再卸载",
            instance.instance_id
        ));
    }
    if supervisor.is_external(&instance.instance_id) {
        return Err(format!(
            "实例 {} 的服务由外部启动且仍在运行，请先在原进程里关闭再卸载",
            instance.instance_id
        ));
    }
    let dir = state.module_dir(&instance);
    let removable = instance.source_kind == InstanceSourceKind::Zip;
    // 与 prepare 互斥：正在写 venv 时不允许删目录。
    let slot = state.instance_slot(&instance.instance_id);
    let _exclusive = slot.lock().unwrap_or_else(|e| e.into_inner());
    state.remove_instance(&instance.instance_id)?;
    if request.remove_files && removable {
        let base = state.paths.instance_dir_keyed(&instance.instance_id);
        // data/ 是用户数据，按 never_auto_delete 契约保留；包、venv 与日志一起清。
        let mut targets = vec![dir];
        for child in ["venvs", "logs"] {
            targets.push(base.join(child));
        }
        for path in targets {
            if !path.exists() {
                continue;
            }
            std::fs::remove_dir_all(&path)
                .map_err(|e| format!("删除 {} 失败：{e}", path.display()))?;
        }
    }
    Ok(())
}

/// active 切换的探测预算：比单次 health 超时宽裕，仍不足以让界面看起来卡死。
const ACTIVATION_PROBE_TIMEOUT: Duration = Duration::from_secs(8);
const ACTIVATION_POLL: Duration = Duration::from_millis(50);

/// 等待后台探测收敛到可判定状态。
///
/// `need_contract` 为真时要求契约也通过；角色没有 health 端点时探测线程会直接
/// 置 Healthy，所以这里不需要知道角色细节。
fn wait_for_activation(
    supervisor: &Supervisor,
    instance_id: &str,
    need_contract: bool,
) -> Result<(), String> {
    let deadline = Instant::now() + ACTIVATION_PROBE_TIMEOUT;
    loop {
        match supervisor.snapshot().get(instance_id).cloned() {
            None => return Err("启动后状态机里没有该实例".into()),
            Some(snap) => {
                if snap.phase == Phase::Failed {
                    return Err(match (&snap.last_error, snap.exit_code) {
                        (Some(error), _) => format!("实例启动失败：{error}"),
                        (None, Some(code)) => format!("实例启动后退出，退出码 {code}"),
                        _ => "实例启动失败".into(),
                    });
                }
                if snap.health == ProbeState::Unhealthy {
                    return Err("健康检查未通过".into());
                }
                if snap.contract == ProbeState::Unhealthy {
                    return Err("契约探测未通过，模块与当前契约不兼容".into());
                }
                let healthy = snap.health == ProbeState::Healthy
                    && (!need_contract || snap.contract == ProbeState::Healthy);
                if healthy {
                    return Ok(());
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "等待探测结果超时（{} 秒）",
                ACTIVATION_PROBE_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(ACTIVATION_POLL);
    }
}

fn activation_step_label(step: ActivationStep) -> &'static str {
    match step {
        ActivationStep::StopPrevious => "停止上一个实例",
        ActivationStep::StartNextOnCanonicalPort => "在规范端口启动目标实例",
        ActivationStep::ProbeHealth => "健康检查",
        ActivationStep::ProbeContract => "契约探测",
        ActivationStep::CommitActive => "提交 active 槽",
        ActivationStep::Cleanup => "清理",
    }
}

/// 按 core 给出的恢复步骤执行，返回可读的处置说明。
///
/// 只尽力而为：回滚本身失败不能让原始错误消失，所以每步都记在说明里。
fn rollback_activation(
    state: &AppStateV2,
    supervisor: &Supervisor,
    previous: Option<&str>,
    next: &str,
    failed: ActivationStep,
) -> Vec<String> {
    let mut notes = Vec::new();
    for step in rollback_steps(previous, failed) {
        match step {
            RecoveryStep::StopFailedCandidate => {
                if supervisor.running().contains(next) {
                    match supervisor.stop(next) {
                        Ok(_) => notes.push(format!("已停止未通过探测的 {next}")),
                        Err(error) => notes.push(format!("停止 {next} 失败：{error}")),
                    }
                }
            }
            RecoveryStep::RestorePrevious => {
                if let Some(prev) = previous.filter(|p| *p != next) {
                    if !supervisor.running().contains(prev) {
                        match state.instance(prev) {
                            Some(prev_instance) => {
                                let port = supervisor.roles().canonical_port(prev_instance.role);
                                match start_instance(state, supervisor, &prev_instance, port) {
                                    Ok(_) => notes.push(format!("已重新拉起上一个实例 {prev}")),
                                    Err(error) => {
                                        notes.push(format!("重新拉起 {prev} 失败：{error}"))
                                    }
                                }
                            }
                            None => {
                                notes.push(format!("上一个实例 {prev} 已不在实例表里，无法恢复"))
                            }
                        }
                    }
                }
            }
            RecoveryStep::RestoreActiveSlot => {
                // 提交前失败时槽位本来就没动；提交后失败由 mutate_installed 自己回滚内存值。
                notes.push("active 槽位保持原值".to_string());
            }
            RecoveryStep::ReportFailure => {}
        }
    }
    notes
}

/// 同一个实例且已在跑：只重新探测，不动槽位也不重启。
/// 同一个实例但没在跑：按「没有上一个」处理，否则事务会退化成空转。
fn activation_plan(
    previous: Option<&str>,
    next: &str,
    target_running: bool,
) -> Vec<ActivationStep> {
    let baseline = if previous == Some(next) && !target_running {
        None
    } else {
        previous
    };
    activation_steps(baseline, next)
}

/// 切换 active：停旧、起新、过健康与契约、提交槽位，失败按计划回滚。
///
/// v1 只改槽位不碰进程，切完 active 指向的实例还是旧的。这里把 core 里已有的
/// `activation_steps` / `rollback_steps` 真正执行掉。
#[tauri::command]
pub fn switch_active(
    instance_id: String,
    state: State<'_, AppStateV2>,
    supervisor: State<'_, Supervisor>,
) -> Result<Vec<String>, String> {
    let instance = state
        .instance(&instance_id)
        .ok_or_else(|| format!("实例不存在：{instance_id}"))?;
    if instance.role == Role::Auxiliary {
        return Err("auxiliary 角色不设 active 槽".into());
    }
    let next = instance.instance_id.clone();

    // 目标服务若是外部拉起的，重启与回滚都做不到，契约也验不了，直接拒绝。
    if supervisor.is_external(&next) {
        return Err(format!(
            "目标实例 {next} 的服务由外部进程提供，启动器无法验证契约一致性，也无法在失败时回滚；请先手动关闭该服务再切换"
        ));
    }

    // 同角色当前的 active；指向已卸载实例时等同于没有上一个。
    let previous = state.with_installed(|st| {
        st.active
            .values()
            .find(|id| {
                st.instances
                    .get(*id)
                    .map(|i| i.role == instance.role)
                    .unwrap_or(false)
            })
            .cloned()
    });

    // 同一个实例且已在跑只重新探测；同一个实例没在跑则按「没有上一个」起一次。
    let target_running = supervisor.running().contains(&next);
    let steps = activation_plan(previous.as_deref(), &next, target_running);

    let mut failure: Option<(ActivationStep, String)> = None;
    for step in steps {
        let outcome = match step {
            ActivationStep::StopPrevious => match previous.as_deref() {
                None => Ok(()),
                Some(prev) if supervisor.running().contains(prev) => supervisor
                    .stop(prev)
                    .map(|_| ())
                    .map_err(|error| format!("停止 {prev} 失败：{error}")),
                // 上一个已在运行态之外（崩溃后未清理、或本来就停着）不需要动作。
                Some(_) => Ok(()),
            },
            ActivationStep::StartNextOnCanonicalPort => {
                if supervisor.running().contains(&next) {
                    Ok(())
                } else {
                    let port = supervisor.roles().canonical_port(instance.role);
                    match start_instance(&state, &supervisor, &instance, port) {
                        Ok(StartOutcome::Managed(_)) => Ok(()),
                        Ok(StartOutcome::External) => Err(format!(
                            "规范端口上已有外部服务，无法确认它就是 {next}；请先关闭再切换"
                        )),
                        Err(error) => Err(error),
                    }
                }
            }
            ActivationStep::ProbeHealth => wait_for_activation(&supervisor, &next, false),
            ActivationStep::ProbeContract => wait_for_activation(&supervisor, &next, true),
            ActivationStep::CommitActive => {
                let path = state.installed_path();
                state.mutate_installed(|st| {
                    st.switch_active(&path, instance.role, &next, true)
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                })
            }
            // 旧实例的包、venv 与数据由保留策略处理，切换本身没有临时产物要收。
            ActivationStep::Cleanup => Ok(()),
        };
        if let Err(error) = outcome {
            failure = Some((step, error));
            break;
        }
    }

    if let Some((failed, error)) = failure {
        let notes = rollback_activation(&state, &supervisor, previous.as_deref(), &next, failed);
        let suffix = if notes.is_empty() {
            String::new()
        } else {
            format!("；{}", notes.join("；"))
        };
        return Err(format!(
            "切换 active 在「{}」失败：{error}{suffix}",
            activation_step_label(failed)
        ));
    }

    Ok(vec![
        launcher_core::lifecycle::role_name(instance.role).to_string(),
        next,
    ])
}

/* ------------------------------------------------------------------ */
/* 安装：整包哈希 → 包内 manifest → 四级验签 → 原子落盘 → 登记实例     */
/* ------------------------------------------------------------------ */

fn read_manifest_value(dir: &Path) -> Result<serde_json::Value, String> {
    let path = dir.join(MANIFEST_FILE);
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("读取 {} 失败：{e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{} 不是合法 JSON：{e}", path.display()))
}

fn runtime_manifest(raw: &serde_json::Value) -> Result<ModuleManifestV2, String> {
    serde_json::from_value(raw.clone()).map_err(|e| format!("manifest 缺少运行时字段：{e}"))
}

/// 安装一个已经过整包哈希校验的包：判级 → 落盘 → 登记实例。
///
/// 离线包（`install_module`）与本地 zip（`install_local_package`）共用这一段，
/// 验签判定只写一份，不会因为入口不同而各自漂移。
///
/// `module_id_in_request` 与 `version_in_request` 是「调用方以为的包内容」，
/// 用于挡住请求与包内清单不一致的情况；本地导入没有这两个声明，传 None。
fn commit_install(
    state: &AppStateV2,
    bytes: &[u8],
    expected_sha256: &str,
    module_id_in_request: Option<&str>,
    version_in_request: Option<&str>,
    confirmed: bool,
) -> Result<InstallResponse, String> {
    if expected_sha256.trim().is_empty() {
        return Err("拒绝安装：缺少 expected_sha256".into());
    }
    let expected = prefixed_sha256(expected_sha256.trim());
    let expectation = ZipExpectation {
        sha256: Some(expected.as_str()),
        size: None,
        max_unpacked: MAX_UNPACKED_BYTES,
    };
    let verified = verify_zip(bytes, &expectation).map_err(|e| e.to_string())?;
    if let Some(want) = module_id_in_request {
        if verified.manifest.module_id != want {
            return Err(format!(
                "包内 module_id（{}）与请求（{want}）不一致",
                verified.manifest.module_id
            ));
        }
    }
    if let Some(version) = version_in_request {
        if !version.is_empty() && version != verified.manifest.version {
            return Err(format!(
                "包内版本（{}）与请求（{version}）不一致",
                verified.manifest.version
            ));
        }
    }

    // 验签分类只看包内 manifest，绝不采信请求里传的 manifest 对象。
    // catalog entry 与历史指纹先取成 owned，再进只读闭包，避免悬垂引用。
    let module_id = verified.manifest.module_id.clone();
    let cached_entry = state
        .cached_catalog()
        .and_then(|doc| doc.entry(&module_id, &verified.manifest.variant).cloned());
    let previous_fp = state
        .instances_for_module(&module_id)
        .first()
        .and_then(|i| i.publisher_fp.clone());
    let classification = state.with_trust(|store| {
        classify(
            store,
            &verified.manifest,
            &ClassifyContext {
                catalog_entry: cached_entry.as_ref(),
                expected_signer_fp: None,
                previous_publisher_fp: previous_fp.as_deref(),
            },
        )
        .map_err(|e| e.to_string())
    })?;
    if classification.verdict == Verdict::D {
        return Err(format!(
            "包被硬拒，不提供确认入口：{}",
            classification.reason
        ));
    }
    if classification.verdict.needs_confirmation() && !confirmed {
        return Err(format!(
            "该包为 {} 类，需要用户显式确认后才能安装：{}",
            classification.verdict.as_str(),
            classification.reason
        ));
    }

    let instance_id = format!(
        "{}@{}",
        verified.manifest.module_id, verified.manifest.variant
    );
    launcher_core::paths::validate_instance_id(&instance_id)
        .map_err(|e| format!("instance_id 不合法：{e}"))?;
    let dir = package_dir(&state.paths, &instance_id);
    let replaced = dir.exists();
    install_package(&verified, &dir).map_err(|e| e.to_string())?;
    // install_package 只写 files；manifest 原文要单独写回，启动与展示都读它。
    write_manifest(&dir, &verified.manifest.raw)?;

    let now = AppStateV2::now();
    let instance = Instance {
        instance_id: instance_id.clone(),
        module_id: verified.manifest.module_id.clone(),
        variant: verified.manifest.variant.clone(),
        version: verified.manifest.version.clone(),
        role: verified.manifest.role,
        category: verified.manifest.category,
        publisher_fp: classification.publisher_fp.clone(),
        verdict: classification.verdict,
        confirmed_at: now.clone(),
        installed_at: now,
        source_kind: InstanceSourceKind::Zip,
        source_path: None,
        git: None,
        pinned: false,
        auto_update: classification.verdict.can_auto_update(),
        zip_sha256: Some(verified.sha256.clone()),
        expected_signer_fp: cached_entry
            .as_ref()
            .and_then(|e| e.expected_signer_fp.clone()),
        signature_present: Some(classification.signature_present),
        allow_unsigned_once: (classification.verdict == Verdict::C).then_some(true),
        head_confirmed: None,
        data_dir: Some(
            state
                .paths
                .instance_data_dir_keyed(&instance_id)
                .to_string_lossy()
                .into_owned(),
        ),
        note: None,
    };
    state.upsert(instance)?;

    Ok(InstallResponse {
        module_id,
        files: verified.files.len(),
        bytes: verified.size,
        replaced,
        install_dir: dir.to_string_lossy().into_owned(),
        trust: if classification.signature_present {
            TrustVerdictView::Signed { key_index: 0 }
        } else {
            TrustVerdictView::UnsignedAllowed
        },
    })
}

#[tauri::command]
pub fn install_module(
    state: State<'_, AppStateV2>,
    request: InstallRequest,
) -> Result<InstallResponse, String> {
    let bytes = base64_decode(&request.zip_base64)?;
    // v1 遗留字段只用来传用户确认，签名判定一律以包内 manifest 为准。
    let confirmed = request.confirm.unwrap_or(false)
        || request
            .manifest
            .get("confirm")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
    commit_install(
        &state,
        &bytes,
        &request.expected_sha256,
        Some(&request.module_id),
        request.version.as_deref(),
        confirmed,
    )
}

/* ------------------------------------------------------------------ */
/* 本地包导入：选一个 .zip，先看清楚再装                                */
/* ------------------------------------------------------------------ */

/// `inspect_local_package` 的结果：装之前先把包内清单摊开给用户看。
#[derive(Debug, Serialize)]
pub struct LocalPackagePreview {
    pub path: String,
    pub module_id: String,
    pub name: String,
    pub version: String,
    pub variant: String,
    pub role: Role,
    pub category: Category,
    pub files: usize,
    pub bytes: u64,
    pub sha256: String,
    pub publisher_fp: Option<String>,
    pub verdict: String,
    pub reason: String,
    pub signed: bool,
    pub needs_confirmation: bool,
    pub replace_existing: bool,
}

/// 本地包上限：解压上限由 `MAX_UNPACKED_BYTES` 兜，这里只挡明显选错的文件。
const MAX_LOCAL_ZIP_BYTES: u64 = 256 * 1024 * 1024;

fn read_local_zip(raw_path: &str) -> Result<(PathBuf, Vec<u8>), String> {
    let trimmed = raw_path.trim().trim_matches('"');
    if trimmed.is_empty() {
        return Err("请先选择一个 .zip 模块包".into());
    }
    let path = PathBuf::from(trimmed);
    if !path.is_file() {
        return Err(format!("文件不存在：{}", path.display()));
    }
    let is_zip = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("zip"))
        .unwrap_or(false);
    if !is_zip {
        return Err("只支持 .zip 模块包".into());
    }
    let size = std::fs::metadata(&path)
        .map_err(|e| format!("读取文件信息失败：{e}"))?
        .len();
    if size > MAX_LOCAL_ZIP_BYTES {
        return Err(format!(
            "包太大（{} MB，上限 {} MB）",
            size / 1024 / 1024,
            MAX_LOCAL_ZIP_BYTES / 1024 / 1024
        ));
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("读取文件失败：{e}"))?;
    Ok((path, bytes))
}

/// 解包 + 分类，供「检查」和「安装」共用；不落盘、不改状态。
fn inspect_bytes(
    state: &AppStateV2,
    bytes: &[u8],
) -> Result<(launcher_core::m2::package::VerifiedPackage, TrustView), String> {
    let expectation = ZipExpectation {
        sha256: None,
        size: None,
        max_unpacked: MAX_UNPACKED_BYTES,
    };
    let verified = verify_zip(bytes, &expectation).map_err(|e| e.to_string())?;
    let cached_entry = state.cached_catalog().and_then(|doc| {
        doc.entry(&verified.manifest.module_id, &verified.manifest.variant)
            .cloned()
    });
    let previous_fp = state
        .instances_for_module(&verified.manifest.module_id)
        .first()
        .and_then(|i| i.publisher_fp.clone());
    let classification = state.with_trust(|store| {
        classify(
            store,
            &verified.manifest,
            &ClassifyContext {
                catalog_entry: cached_entry.as_ref(),
                expected_signer_fp: None,
                previous_publisher_fp: previous_fp.as_deref(),
            },
        )
        .map_err(|e| e.to_string())
    })?;
    Ok((
        verified,
        TrustView {
            verdict: classification.verdict.as_str().to_string(),
            reason: classification.reason.clone(),
            publisher_fp: classification.publisher_fp.clone(),
            signed: classification.signature_present,
            needs_confirmation: classification.verdict.needs_confirmation(),
        },
    ))
}

/// 判级结果：命令面只暴露这一份，预览与安装走的判定完全一致。
#[derive(Debug, Clone)]
struct TrustView {
    verdict: String,
    reason: String,
    publisher_fp: Option<String>,
    signed: bool,
    needs_confirmation: bool,
}

#[tauri::command]
pub async fn inspect_local_package(
    state: State<'_, AppStateV2>,
    path: String,
) -> Result<LocalPackagePreview, String> {
    // 读文件与解包都是磁盘活，放到阻塞池，别占住事件循环。
    let (resolved, bytes) = tauri::async_runtime::spawn_blocking(move || read_local_zip(&path))
        .await
        .map_err(|e| format!("读取包失败：{e}"))??;
    let (verified, trust) = inspect_bytes(&state, &bytes)?;
    let instance_id = format!(
        "{}@{}",
        verified.manifest.module_id, verified.manifest.variant
    );
    Ok(LocalPackagePreview {
        path: resolved.to_string_lossy().into_owned(),
        module_id: verified.manifest.module_id.clone(),
        name: verified
            .manifest
            .raw
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or(&verified.manifest.module_id)
            .to_string(),
        version: verified.manifest.version.clone(),
        variant: verified.manifest.variant.clone(),
        role: verified.manifest.role,
        category: verified.manifest.category,
        files: verified.files.len(),
        bytes: verified.size,
        sha256: verified.sha256.clone(),
        publisher_fp: trust.publisher_fp.clone(),
        verdict: trust.verdict.clone(),
        reason: trust.reason.clone(),
        signed: trust.signed,
        needs_confirmation: trust.needs_confirmation,
        replace_existing: state.instance(&instance_id).is_some(),
    })
}

#[tauri::command]
pub async fn install_local_package(
    state: State<'_, AppStateV2>,
    path: String,
    expected_sha256: String,
    confirm: Option<bool>,
) -> Result<InstallResponse, String> {
    let (_, bytes) = tauri::async_runtime::spawn_blocking(move || read_local_zip(&path))
        .await
        .map_err(|e| format!("读取包失败：{e}"))??;
    commit_install(
        &state,
        &bytes,
        &expected_sha256,
        None,
        None,
        confirm.unwrap_or(false),
    )
}

#[tauri::command]
pub fn prepare_instance_env(
    instance_id: String,
    allow_online: Option<bool>,
    state: State<'_, AppStateV2>,
    supervisor: State<'_, Supervisor>,
) -> Result<String, String> {
    let instance = state
        .instance(&instance_id)
        .ok_or_else(|| format!("实例不存在：{instance_id}"))?;
    if !instance.role.needs_runtime() {
        return Err(format!("{:?} 角色不需要准备依赖环境", instance.role));
    }
    // `.staging` 路径按实例哈希固定，同一实例并发 prepare 会互删。
    // 锁槽在实例粒度，不同实例的准备仍然并行。
    let slot = state.instance_slot(&instance_id);
    let _exclusive = slot.lock().unwrap_or_else(|e| e.into_inner());
    let dir = state.module_dir(&instance);
    let raw = read_manifest_value(&dir)?;
    let manifest = runtime_manifest(&raw)?;
    let install = launcher_core::install::InstallState::load(&state.paths)
        .map_err(|e| format!("读取 install.json 失败：{e}"))?;
    let runner = launcher_core::env::SystemUvRunner::new(state.paths.install_root.clone());
    let fetcher = launcher_core::env::CurlUvFetcher;
    let online = allow_online.unwrap_or(false);
    let ctx = launcher_core::env::EnvContext {
        install: &install,
        options: launcher_core::env::EnvOptions {
            // 离线优先；只有用户显式放行才允许联网补依赖与下载 uv。
            offline: !online,
            allow_uv_download: online,
            port: supervisor.roles().canonical_port(instance.role),
            source_kind: instance.source_kind.as_env_kind(),
        },
        runner: &runner,
        fetcher: &fetcher,
    };
    let prepared = launcher_core::env::prepare_instance_with(
        &state.paths,
        &instance_id,
        &manifest,
        &dir,
        &ctx,
    )
    .map_err(|e| {
        if online {
            format!("依赖准备失败：{e}")
        } else {
            format!("离线依赖准备失败（{e}）。如确需联网补齐依赖，请显式放行后重试。")
        }
    })?;
    Ok(prepared.venv_python.to_string_lossy().into_owned())
}

/* ------------------------------------------------------------------ */
/* 双源 catalog                                                        */
/* ------------------------------------------------------------------ */

#[tauri::command]
pub fn refresh_catalog(state: State<'_, AppStateV2>, catalog_url: String) -> CatalogResponse {
    let allowlist = crate::state_v2::allowed_hosts();
    let fetcher = match ReqwestCatalogFetcher::new() {
        Ok(client) => client,
        Err(error) => return unavailable(&allowlist, error),
    };
    let k5 = state.k5_pubkey();
    let github = RemoteCatalogAdapter::github("github", &catalog_url, 1, &k5, &fetcher);
    let github_result = github.fetch_catalog();

    // 第二源只有显式配置时才参与；没有配置不算故障，单源裁决照样成立。
    let sf_url = std::env::var("DOUNAI_SF_CATALOG_URL").unwrap_or_default();
    let sf = RemoteCatalogAdapter::sourceforge("sourceforge", &sf_url, 2, &k5, &fetcher);
    let mut candidates: Vec<(u32, Result<_, _>)> = Vec::new();
    match github_result {
        Ok(Some(doc)) => candidates.push((1, Ok(doc))),
        Ok(None) => {}
        Err(error) => candidates.push((1, Err(error))),
    }
    if !sf_url.is_empty() {
        match sf.fetch_catalog() {
            Ok(Some(doc)) => candidates.push((2, Ok(doc))),
            Ok(None) => {}
            Err(error) => candidates.push((2, Err(error))),
        }
    }

    let previous = state.with_installed(|s| s.catalog_state.clone());
    let now = AppStateV2::now();
    match select_catalog(&candidates, &previous, &now) {
        Ok(doc) => {
            let entries = catalog_entries(&doc);
            let _ = state
                .mutate_installed(|s| s.catalog_state.observe(&doc).map_err(|e| e.to_string()));
            state.set_cached_catalog(Some(doc));
            CatalogResponse {
                source: "remote".into(),
                entries,
                allowlist,
                reason: None,
            }
        }
        Err(error) => {
            // 只允许回退本次会话内已验签过的 doc；不读磁盘上的旧缓存，
            // 避免把过期/序号倒退的 catalog 当成功结果展示。
            if let Some(doc) = state.cached_catalog() {
                CatalogResponse {
                    source: "cache".into(),
                    entries: catalog_entries(&doc),
                    allowlist,
                    reason: Some(error.reason().to_string()),
                }
            } else {
                unavailable(&allowlist, error.reason().to_string())
            }
        }
    }
}

fn catalog_entries(doc: &launcher_core::m2::catalog::CatalogDocument) -> Vec<CatalogEntryView> {
    doc.modules
        .iter()
        .map(|entry| CatalogEntryView {
            module_id: entry.module_id.clone(),
            name: entry.name.clone(),
            version: entry.version.clone(),
            download_url: entry
                .sources
                .first()
                .map(|s| s.url.clone())
                .unwrap_or_default(),
            sha256: bare_sha256(&entry.sha256),
        })
        .collect()
}

fn unavailable(allowlist: &[String], reason: String) -> CatalogResponse {
    CatalogResponse {
        source: "unavailable".into(),
        entries: Vec::new(),
        allowlist: allowlist.to_vec(),
        reason: Some(reason),
    }
}

#[tauri::command]
pub fn set_k5_pubkey(pubkey_b64: String, state: State<'_, AppStateV2>) -> Result<KeyInfo, String> {
    let fp = launcher_core::m2::fingerprint(&pubkey_b64).map_err(|e| e.to_string())?;
    state.set_k5_pubkey(pubkey_b64);
    Ok(KeyInfo {
        fingerprint: fp,
        accepted: true,
        note: "K5 公钥仅本次会话有效，重启回落到内置公钥。".into(),
    })
}

#[tauri::command]
pub fn confirm_trusted_key(
    state: State<'_, AppStateV2>,
    request: KeyRequest,
) -> Result<KeyInfo, String> {
    let fp = prefixed_sha256(&request.fingerprint);
    if !launcher_core::m2::is_fingerprint(&fp) {
        return Err("指纹必须是 sha256: + 64 位小写 hex".into());
    }
    state.mutate_trust(|store| {
        store
            .tofu(
                &fp,
                request.label.as_deref().unwrap_or("用户确认"),
                &AppStateV2::now(),
            )
            .map_err(|e| e.to_string())
    })?;
    Ok(KeyInfo {
        fingerprint: fp,
        accepted: true,
        note: "公钥已登记，后续 A 类判定生效。".into(),
    })
}

#[tauri::command]
pub fn revoke_trusted_key(
    fingerprint: String,
    state: State<'_, AppStateV2>,
) -> Result<KeyInfo, String> {
    let fp = prefixed_sha256(&fingerprint);
    let removed = state.mutate_trust(|store| Ok(store.revoke(&fp, &AppStateV2::now())))?;
    Ok(KeyInfo {
        fingerprint: fp,
        accepted: removed,
        note: if removed {
            "公钥已撤销。".into()
        } else {
            "指纹不在信任表中，无需撤销。".into()
        },
    })
}

#[tauri::command]
pub fn set_keep_versions(keep_versions: u8, state: State<'_, AppStateV2>) -> Result<u8, String> {
    let mut policy = state.with_retention(|p| p.clone());
    policy.keep_versions = keep_versions;
    policy
        .validate()
        .map_err(|e| format!("保留策略不合法：{e}"))?;
    state.save_retention(&policy)?;
    Ok(policy.keep_versions)
}

/* ------------------------------------------------------------------ */
/* 本地目录 / Git 导入                                                 */
/* ------------------------------------------------------------------ */

const SKIP_SCAN_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    ".git",
    ".svn",
    ".hg",
    ".venv",
    "venv",
    "__pycache__",
    ".cache",
    ".idea",
    ".vscode",
    ".pytest_cache",
    ".mypy_cache",
];

fn find_manifest(dir: &Path) -> Option<PathBuf> {
    ["manifest.json", "dounai-manifest.json"]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.exists())
}

fn scan_dirs(
    root: &Path,
    out: &mut Vec<PathBuf>,
    budget: &mut usize,
    depth: usize,
    seen: &mut BTreeSet<PathBuf>,
) {
    if depth > 4 || *budget == 0 {
        return;
    }
    let Ok(canonical) = root.canonicalize() else {
        return;
    };
    if !seen.insert(canonical.clone()) {
        return;
    }
    *budget -= 1;
    if find_manifest(&canonical).is_some() {
        out.push(canonical);
        return;
    }
    out.push(canonical);
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if SKIP_SCAN_DIRS.contains(&name.as_str()) {
            continue;
        }
        scan_dirs(&entry.path(), out, budget, depth + 1, seen);
    }
}

#[tauri::command]
pub fn scan_local_modules(
    state: State<'_, AppStateV2>,
    root: String,
) -> Result<Vec<ScanCandidate>, String> {
    require_dev_mode(&state)?;
    let root = PathBuf::from(root.trim());
    if !root.is_dir() {
        return Err(format!("目录不存在或不是目录：{}", root.display()));
    }
    let mut dirs = Vec::new();
    let mut seen = BTreeSet::new();
    let mut budget = 500usize;
    scan_dirs(&root, &mut dirs, &mut budget, 0, &mut seen);
    let mut out = Vec::new();
    for dir in dirs {
        let Some(path) = find_manifest(&dir) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(raw) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(module_id) = raw.get("module_id").and_then(|v| v.as_str()) else {
            continue;
        };
        if module_id.is_empty() {
            continue;
        }
        let name = raw
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or(module_id)
            .to_string();
        out.push(ScanCandidate {
            path: dir.to_string_lossy().into_owned(),
            module_id: module_id.to_string(),
            name,
            version: raw
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("0.0.0")
                .to_string(),
            category: raw
                .get("category")
                .and_then(|v| v.as_str())
                .unwrap_or("optional")
                .to_string(),
            signed: raw
                .pointer("/signature/value")
                .and_then(|v| v.as_str())
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false),
            registered: !state.instances_for_module(module_id).is_empty(),
        });
    }
    out.sort_by(|a, b| a.module_id.cmp(&b.module_id));
    Ok(out)
}

fn add_local_dir(state: &AppStateV2, raw_path: &str) -> Result<ModuleView, String> {
    let trimmed = raw_path.trim().trim_matches('"');
    if trimmed.is_empty() {
        return Err("请填写模块目录路径".into());
    }
    let dir = PathBuf::from(trimmed);
    if !dir.is_dir() {
        return Err(format!("目录不存在或不是目录：{}", dir.display()));
    }
    let canonical = dir
        .canonicalize()
        .map_err(|e| format!("无法解析目录：{e}"))?;
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| state.paths.install_root.clone());
    validate_local_dir(
        &canonical,
        &state.paths.install_root,
        &state.paths.data_root,
        &exe_dir,
    )
    .map_err(|e| e.to_string())?;

    let manifest_path = find_manifest(&canonical)
        .ok_or_else(|| format!("目录缺少 manifest.json：{}", canonical.display()))?;
    let text =
        std::fs::read_to_string(&manifest_path).map_err(|e| format!("读取 manifest 失败：{e}"))?;
    let manifest = PackageManifest::parse(&text).map_err(|e| e.to_string())?;
    let _runtime = runtime_manifest(&manifest.raw)?;

    let observed = observe_source(&canonical, &AppStateV2::now()).map_err(|e| e.to_string())?;
    let instance_id = format!("{}@local", manifest.module_id);
    launcher_core::paths::validate_instance_id(&instance_id)
        .map_err(|e| format!("instance_id 不合法：{e}"))?;
    let now = AppStateV2::now();
    // 契约固定：dir 实例一律 C 类 / optional / 不自动更新，即便包内带签名。
    // 用户显式选择这个目录就等于完成一次确认；源码变化后仍需重新确认。
    let instance = Instance {
        instance_id: instance_id.clone(),
        module_id: manifest.module_id.clone(),
        variant: "local".into(),
        version: manifest.version.clone(),
        role: manifest.role,
        category: Category::Optional,
        publisher_fp: None,
        verdict: Verdict::C,
        confirmed_at: now.clone(),
        installed_at: now,
        source_kind: InstanceSourceKind::Dir,
        source_path: Some(canonical.to_string_lossy().into_owned()),
        git: Some(observed.to_git_observation()),
        pinned: true,
        auto_update: false,
        zip_sha256: Some(observed.fingerprint.clone()),
        expected_signer_fp: None,
        signature_present: Some(manifest.is_signed()),
        allow_unsigned_once: None,
        head_confirmed: Some(observed.confirmation_anchor().to_string()),
        data_dir: Some(
            state
                .paths
                .instance_data_dir_keyed(&instance_id)
                .to_string_lossy()
                .into_owned(),
        ),
        note: None,
    };
    state.upsert(instance.clone())?;
    // 不加 Supervisor 参数时返回无运行态的视图；调用方命令再补上进程快照。
    let view = ModuleView {
        module_id: instance.module_id.clone(),
        install: InstallStateView::Installed,
        enabled: !state.is_disabled(&instance.module_id),
        version: instance.version.clone(),
        path: canonical.to_string_lossy().into_owned(),
        run: RunStateView::Stopped,
        last_error: None,
        pid: None,
        healthy: false,
        linked: true,
        external: false,
    };
    Ok(view)
}

#[tauri::command]
pub fn add_local_module(
    state: State<'_, AppStateV2>,
    supervisor: State<'_, Supervisor>,
    request: AddLocalModuleRequest,
) -> Result<ModuleView, String> {
    require_dev_mode(&state)?;
    let mut view = add_local_dir(&state, &request.path)?;
    let instance = state.resolve_instance(&view.module_id)?;
    view.run = phase_to_run(
        supervisor
            .snapshot()
            .get(&instance.instance_id)
            .map(|s| s.phase),
    );
    Ok(view)
}

#[tauri::command]
pub fn add_local_modules_bulk(
    state: State<'_, AppStateV2>,
    request: BulkAddLocalModulesRequest,
) -> Result<Vec<BulkAddResult>, String> {
    require_dev_mode(&state)?;
    Ok(request
        .paths
        .iter()
        .map(|p| match add_local_dir(&state, p) {
            Ok(view) => BulkAddResult {
                module_id: view.module_id,
                path: view.path,
                ok: true,
                error: None,
            },
            Err(error) => BulkAddResult {
                module_id: String::new(),
                path: p.clone(),
                ok: false,
                error: Some(error),
            },
        })
        .collect())
}

#[tauri::command]
pub fn remove_local_module(
    module_id: String,
    state: State<'_, AppStateV2>,
    supervisor: State<'_, Supervisor>,
) -> Result<(), String> {
    require_dev_mode(&state)?;
    let instance = state.resolve_instance(&module_id)?;
    if instance.source_kind != InstanceSourceKind::Dir {
        return Err("remove_local_module 只解除本地目录登记，zip 实例请用 uninstall_module".into());
    }
    if supervisor.running().contains(&instance.instance_id)
        || supervisor.is_external(&instance.instance_id)
    {
        return Err("实例正在运行（含外部启动），请先停止再解除登记".into());
    }
    state.remove_instance(&instance.instance_id)?;
    // 源码目录归用户所有，绝不删除。
    Ok(())
}

#[tauri::command]
pub fn confirm_local_source(
    instance_id: String,
    state: State<'_, AppStateV2>,
) -> Result<String, String> {
    let instance = state
        .instance(&instance_id)
        .ok_or_else(|| format!("实例不存在：{instance_id}"))?;
    if instance.source_kind != InstanceSourceKind::Dir {
        return Err("只有目录实例需要确认源码状态".into());
    }
    let dir = state.module_dir(&instance);
    let observed = observe_source(&dir, &AppStateV2::now()).map_err(|e| e.to_string())?;
    let anchor = observed.confirmation_anchor().to_string();
    state.mutate_installed(|s| {
        let entry = s
            .instances
            .get_mut(&instance_id)
            .ok_or_else(|| "实例在确认过程中消失".to_string())?;
        entry.head_confirmed = Some(anchor.clone());
        entry.confirmed_at = AppStateV2::now();
        if let Some(git) = entry.git.as_mut() {
            git.head = observed.head.clone();
            git.observed_at = observed.observed_at.clone();
        }
        Ok(())
    })?;
    Ok(anchor)
}

/* ------------------------------------------------------------------ */
/* 展示、窗口、下载、清理                                              */
/* ------------------------------------------------------------------ */

#[tauri::command]
pub fn read_module_manifests(state: State<'_, AppStateV2>) -> Vec<ModuleManifestView> {
    let ids: Vec<String> = state.with_installed(|s| s.instances.keys().cloned().collect());
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let Some(instance) = state.instance(&id) else {
            continue;
        };
        let dir = state.module_dir(&instance);
        let root = dir.to_string_lossy().into_owned();
        match read_manifest_value(&dir) {
            Ok(manifest) => out.push(ModuleManifestView {
                module_id: instance.module_id,
                manifest: Some(manifest),
                error: None,
                root,
            }),
            Err(error) => out.push(ModuleManifestView {
                module_id: instance.module_id,
                manifest: None,
                error: Some(error),
                root,
            }),
        }
    }
    out
}

fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    base64::engine::general_purpose::STANDARD
        .decode(input.trim())
        .map_err(|e| format!("不是合法 base64：{e}"))
}

fn admin_url_v2(
    state: &AppStateV2,
    supervisor: &Supervisor,
    module_id: &str,
) -> Result<String, String> {
    let instance = state.resolve_instance(module_id)?;
    let raw = read_manifest_value(&state.module_dir(&instance))?;
    let web_path = raw
        .pointer("/admin_ui/web_ui/path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("模块 {module_id} 没有声明管理界面（admin_ui.web_ui.path）"))?;
    if !web_path.starts_with('/') {
        return Err(format!("管理界面路径必须以 / 开头，实际：{web_path}"));
    }
    let requires_running = raw
        .pointer("/admin_ui/web_ui/requires_running")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    // 外部已运行的服务同样算「在跑」，管理页应可打开（上游 D-4）。
    let alive = supervisor.running().contains(&instance.instance_id)
        || supervisor.is_external(&instance.instance_id);
    if requires_running && !alive {
        return Err(format!("模块 {module_id} 未运行，请先启动它"));
    }
    let port = supervisor
        .roles()
        .canonical_port(instance.role)
        .unwrap_or(8101);
    Ok(format!("http://127.0.0.1:{port}{web_path}"))
}

#[tauri::command]
pub async fn open_admin_window(
    app: tauri::AppHandle,
    state: State<'_, AppStateV2>,
    supervisor: State<'_, Supervisor>,
    request: OpenAdminUiRequest,
) -> Result<String, String> {
    let url = admin_url_v2(&state, &supervisor, &request.module_id)?;
    let module_id = request.module_id.clone();
    let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
    let app_for_thread = app.clone();
    let url_for_thread = url.clone();
    // 与 v1 相同：Tauri 2 在 Windows 上必须把建窗口挪出命令线程，
    // 否则 Webview2 COM 初始化会死锁在 about:blank。
    std::thread::spawn(move || {
        let result = (|| -> Result<String, String> {
            let label = format!("admin-{module_id}");
            if let Some(existing) = app_for_thread.get_webview_window(&label) {
                let _ = existing.show();
                let _ = existing.set_focus();
                return Ok(url_for_thread.clone());
            }
            let parsed = url_for_thread
                .parse::<tauri::Url>()
                .map_err(|e| format!("管理界面地址不合法：{e}"))?;
            tauri::WebviewWindowBuilder::new(
                &app_for_thread,
                label,
                tauri::WebviewUrl::External(parsed),
            )
            .title(format!("{module_id} · 管理界面"))
            .inner_size(1080.0, 760.0)
            .min_inner_size(720.0, 520.0)
            // 纵深防御：外部 URL 的 WebView 会继承 Tauri 的全局注入。
            // capabilities 把权限限定在 windows:["main"]，admin 窗口调用命令
            // 会被 ACL 拒绝，但注入对象本身仍等于告诉上游页面「我跑在 Tauri 里」，
            // 并留下可枚举的入口。这里在页面脚本执行前清掉。
            //
            // 必须用 for_all_frames：上游页面可能用 iframe 加载子页面，
            // 只清顶层会留后门。也不能全局关 withGlobalTauri——主窗口
            // 的 bridge.ts 完全依赖 window.__TAURI__（未引入 @tauri-apps/api）。
            .initialization_script_for_all_frames(
                r#"(function(){ try { window.__DOUNAI_ADMIN_SANITIZED__ = true; } catch (e) {}
  // __TAURI_INTERNALS__ 被 Tauri 定义为 configurable:false，delete 无效，
  // 只能覆盖其值；连覆盖都被拒绝时再退回到赋值。
  var kill = function (name) {
    try { delete window[name]; } catch (e) {}
    if (typeof window[name] !== 'undefined') {
      try {
        Object.defineProperty(window, name, { value: undefined, configurable: false, writable: false });
      } catch (e) {
        try { window[name] = undefined; } catch (e2) {}
      }
    }
  };
  kill('__TAURI__');
  kill('__TAURI_IPC__');
  kill('__TAURI_METADATA__');
  // 内部对象换成冻结空壳，切断 invoke 通道；ACL 仍是最后一道防线。
  try {
    Object.defineProperty(window, '__TAURI_INTERNALS__', {
      value: Object.freeze({}),
      configurable: false,
      writable: false,
    });
  } catch (e) {
    try { window.__TAURI_INTERNALS__ = Object.freeze({}); } catch (e2) {}
  }
})();"#,
            )
            .center()
            .build()
            .map_err(|e| format!("打开管理窗口失败：{e}"))?;
            Ok(url_for_thread)
        })();
        let _ = tx.send(result);
    });
    rx.recv_timeout(std::time::Duration::from_secs(15))
        .unwrap_or_else(|_| Err("打开管理窗口超时".into()))
}

#[tauri::command]
pub fn download_package(
    state: State<'_, AppStateV2>,
    request: DownloadRequest,
) -> Result<DownloadResponse, String> {
    if request.sources.is_empty() {
        return Err("没有提供下载源".into());
    }
    if request.expected_sha256.trim().is_empty() {
        return Err("拒绝下载：缺少 expected_sha256".into());
    }
    for source in &request.sources {
        validate_download_url(source, ALL_HOST_SUFFIXES)
            .map_err(|e| format!("下载源 {source} 被拒绝：{e}"))?;
    }
    if !launcher_core::security::is_safe_filename(&request.module_id) {
        return Err(format!("非法的模块 id：{}", request.module_id));
    }
    let dest = state
        .paths
        .data_root
        .join("downloads")
        .join(format!("{}.zip", request.module_id));
    let client = crate::http_client::ReqwestClient::default_client()?;
    let options = crate::download::DownloadOptions {
        retries: request.retries,
        backoff_ms: 150,
        expected_sha256: Some(bare_sha256(request.expected_sha256.trim())),
    };
    let report = crate::download::download(&client, &request.sources, &dest, &options)
        .map_err(|e| format!("下载失败：{e}"))?;
    Ok(DownloadResponse {
        path: report.path.to_string_lossy().into_owned(),
        bytes: report.bytes,
        source: report.source,
        resumed_from: report.resumed_from,
        attempts: report.attempts,
    })
}

/// 设置页一次读完：信任表、保留策略与两枚内置公钥的指纹。
///
/// 指纹全部用编译期内嵌值或会话内 K5 重算，界面不自行拼装字符串——
/// 指纹是必须一眼看出来的东西，不能出现「界面显示 A、判定用 B」。
#[derive(Debug, Serialize)]
pub struct SettingsSnapshot {
    pub trust: launcher_core::m2::trust::TrustStore,
    pub retention: launcher_core::m2::retention::RetentionPolicy,
    pub k1_fingerprint: String,
    pub k5_fingerprint: String,
}

#[tauri::command]
pub fn settings_snapshot(state: State<'_, AppStateV2>) -> SettingsSnapshot {
    let trust = state.with_trust(|store| store.clone());
    let retention = state.with_retention(|policy| policy.clone());
    let k5_fingerprint = launcher_core::m2::fingerprint(&state.k5_pubkey())
        .unwrap_or_else(|_| crate::state_v2::BUILTIN_K5_FINGERPRINT.to_string());
    SettingsSnapshot {
        trust,
        retention,
        k1_fingerprint: crate::state_v2::BUILTIN_K1_FINGERPRINT.to_string(),
        k5_fingerprint,
    }
}

#[tauri::command]
pub fn trust_info(state: State<'_, AppStateV2>) -> TrustInfo {
    let trusted_keys = state.with_trust(|store| {
        store.builtin.iter().filter(|k| k.enabled).count()
            + store
                .trusted
                .iter()
                .filter(|k| k.enabled && k.revoked_at.is_none())
                .count()
    });
    TrustInfo {
        trusted_keys,
        // v2 没有「全局允许未签名」策略：未签名走 C 类单次确认。
        allow_unsigned: false,
        open: false,
    }
}

#[tauri::command]
pub fn dev_mode_status(state: State<'_, AppStateV2>) -> DevModeInfo {
    DevModeInfo {
        enabled: state.dev_mode.load(Ordering::SeqCst),
    }
}

#[tauri::command]
pub fn enable_dev_mode(state: State<'_, AppStateV2>) -> DevModeInfo {
    state.dev_mode.store(true, Ordering::SeqCst);
    DevModeInfo { enabled: true }
}

fn require_dev_mode(state: &AppStateV2) -> Result<(), String> {
    if state.dev_mode.load(Ordering::SeqCst) {
        Ok(())
    } else {
        Err("本地模块登记未开启。".into())
    }
}

#[tauri::command]
pub fn purge_local_data(
    state: State<'_, AppStateV2>,
    supervisor: State<'_, Supervisor>,
    request: PurgeRequest,
) -> PurgeResponse {
    let mut removed_caches = Vec::new();
    let mut removed_module_dirs = Vec::new();
    let mut errors = Vec::new();
    let mut state_cleared = false;

    if request.caches {
        for name in ["catalog-cache.json", "catalog-cache.json.tmp"] {
            let path = state.paths.state_dir().join(name);
            if path.exists() {
                match std::fs::remove_file(&path) {
                    Ok(()) => removed_caches.push(path.to_string_lossy().into_owned()),
                    Err(e) => errors.push(format!("清除 {} 失败：{e}", path.display())),
                }
            }
        }
        let downloads = state.paths.data_root.join("downloads");
        if let Ok(entries) = std::fs::read_dir(&downloads) {
            for entry in entries.flatten() {
                if entry.path().is_file() {
                    match std::fs::remove_file(entry.path()) {
                        Ok(()) => removed_caches.push(entry.path().to_string_lossy().into_owned()),
                        Err(e) => errors.push(format!("清除下载缓存失败：{e}")),
                    }
                }
            }
        }
        state.set_cached_catalog(None);
    }

    if request.state {
        // installed.json 的实例表是唯一真源，不随「清状态」一起删：
        // 删掉会留下孤儿包目录，比清缓存更危险。这里只重置 catalog 水位，
        // 语义差异写进 M3.5 文档，不塞进 errors 让 UI 误报成故障。
        match state.mutate_installed(|s| {
            s.catalog_state = CatalogState::default();
            Ok(())
        }) {
            Ok(()) => state_cleared = true,
            Err(e) => errors.push(format!("重置 catalog 水位失败：{e}")),
        }
    }

    if request.module_files {
        // 顺序是「先停再删」：运行中的进程握着 venv 里的文件句柄，直接删会半途失败。
        // 只停自己拉起的；外部实例不在 Job 里，启动器无权停，只能跳过并如实报告。
        if !supervisor.running().is_empty() {
            supervisor.stop_all();
        }
        for id in state.with_installed(|s| s.instances.keys().cloned().collect::<Vec<_>>()) {
            let Some(instance) = state.instance(&id) else {
                continue;
            };
            if instance.source_kind == InstanceSourceKind::Dir {
                // 源码目录归用户，永不删除。
                continue;
            }
            if supervisor.running().contains(&id) || supervisor.is_external(&id) {
                errors.push(format!("实例 {id} 由外部启动且无法停止，跳过清理"));
                continue;
            }
            let slot = state.instance_slot(&id);
            let _exclusive = slot.lock().unwrap_or_else(|e| e.into_inner());
            let base = state.paths.instance_dir_keyed(&id);
            for child in ["package", "venvs", "logs"] {
                let path = base.join(child);
                if path.exists() {
                    match std::fs::remove_dir_all(&path) {
                        Ok(()) => removed_module_dirs.push(path.to_string_lossy().into_owned()),
                        Err(e) => errors.push(format!("清理 {} 失败：{e}", path.display())),
                    }
                }
            }
            // data/ 依据 never_auto_delete 契约保留。
        }
    }

    PurgeResponse {
        removed_caches,
        state_cleared,
        removed_module_dirs,
        errors,
    }
}

#[tauri::command]
pub fn read_instance_logs(instance_id: String, state: State<'_, AppStateV2>) -> InstanceLogs {
    let dir = state.paths.instance_logs_dir_keyed(&instance_id);
    let tail = crate::diag_log::read_tail(&dir, crate::diag_log::MAX_TAIL_LINES);
    let note = if tail.files == 0 {
        "该实例还没有日志：启动过一次才会写入。启动器事件始终记录，子进程输出需显式打开诊断开关。"
            .to_string()
    } else if tail.truncated {
        format!(
            "显示最近 {} 行，更早的内容已截断。保留 {} 个文件，每个超过 2 MiB 时轮转。",
            tail.lines.len(),
            tail.files
        )
    } else {
        format!(
            "共 {} 个文件。启动器事件始终记录，子进程输出需显式打开诊断开关。",
            tail.files
        )
    };
    InstanceLogs {
        instance_id,
        lines: tail.lines,
        files: tail.files,
        truncated: tail.truncated,
        note,
    }
}

#[tauri::command]
pub fn hash_file(path: String, state: State<'_, AppStateV2>) -> Result<String, String> {
    let target = PathBuf::from(&path);
    let canonical = target
        .canonicalize()
        .map_err(|e| format!("无法解析路径：{e}"))?;
    // 与 v1 一致：只放行数据目录，不把命令变成任意文件读取原语。
    let root = state
        .paths
        .data_root
        .canonicalize()
        .map_err(|e| format!("无法解析数据目录：{e}"))?;
    if !canonical.starts_with(&root) {
        return Err("只能读取启动器数据目录内的文件".into());
    }
    let bytes = std::fs::read(&canonical).map_err(|e| e.to_string())?;
    Ok(launcher_core::security::sha256_hex(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_views_match_v1_wire_format() {
        let started = Snapshot {
            instance_id: "dounai/demo@stable".into(),
            phase: Phase::Starting,
            ticket: 1,
            pid: Some(42),
            health: ProbeState::Pending,
            contract: ProbeState::NotChecked,
            exit_code: None,
            last_error: None,
        };
        let value = serde_json::to_value(run_view("dounai/demo", Some(&started), None)).unwrap();
        assert_eq!(value["run"], "starting");
        assert_eq!(value["pid"], 42);
        assert_eq!(value["healthy"], false);
        assert_eq!(value["module_id"], "dounai/demo");
        let stopped = serde_json::to_value(run_view("dounai/demo", None, None)).unwrap();
        assert_eq!(stopped["run"], "stopped");
        assert!(stopped["pid"].is_null());
    }

    #[test]
    fn module_view_has_the_v1_field_set() {
        let view = ModuleView {
            module_id: "dounai/demo".into(),
            install: InstallStateView::Installed,
            enabled: true,
            version: "1.0.0".into(),
            path: "C:/tmp/demo".into(),
            run: RunStateView::Stopped,
            last_error: None,
            pid: None,
            healthy: false,
            linked: false,
            external: false,
        };
        let value = serde_json::to_value(view).unwrap();
        for field in [
            "module_id",
            "install",
            "enabled",
            "version",
            "path",
            "run",
            "last_error",
            "pid",
            "healthy",
            "linked",
            "external",
        ] {
            assert!(value.get(field).is_some(), "缺少字段 {field}");
        }
        assert_eq!(value["install"], "installed");
    }

    #[test]
    fn trust_verdict_matches_bridge_union() {
        let signed = serde_json::to_value(TrustVerdictView::Signed { key_index: 0 }).unwrap();
        assert_eq!(signed["kind"], "signed");
        assert_eq!(signed["key_index"], 0);
        let allowed = serde_json::to_value(TrustVerdictView::UnsignedAllowed).unwrap();
        assert_eq!(allowed["kind"], "unsigned_allowed");
    }

    #[test]
    fn base64_decode_rejects_garbage() {
        assert!(base64_decode("not-base64!!").is_err());
        assert_eq!(base64_decode("aGk=").unwrap(), b"hi");
    }

    #[test]
    fn activation_plan_covers_all_four_shapes() {
        // 没有上一个：起新、探测、提交、清理。
        assert_eq!(
            activation_plan(None, "b", false),
            vec![
                ActivationStep::StartNextOnCanonicalPort,
                ActivationStep::ProbeHealth,
                ActivationStep::ProbeContract,
                ActivationStep::CommitActive,
                ActivationStep::Cleanup,
            ]
        );
        // 有上一个：先停旧的，再走同一串。
        let full = activation_plan(Some("a"), "b", false);
        assert_eq!(full.first(), Some(&ActivationStep::StopPrevious));
        assert_eq!(full.len(), 6);
        // 目标就是当前 active 且在跑：只重新探测，既不重启也不动槽位。
        assert_eq!(
            activation_plan(Some("b"), "b", true),
            vec![ActivationStep::ProbeHealth, ActivationStep::ProbeContract]
        );
        // 目标就是当前 active 但没在跑：必须真的拉起，不能空转。
        let restart = activation_plan(Some("b"), "b", false);
        assert_eq!(
            restart.first(),
            Some(&ActivationStep::StartNextOnCanonicalPort)
        );
        assert!(!restart.contains(&ActivationStep::StopPrevious));
    }

    #[test]
    fn commit_active_is_the_last_state_change() {
        // 槽位只能在探测全过之后提交：提交后只剩 Cleanup，回滚才有意义。
        let steps = activation_plan(Some("a"), "b", false);
        let commit = steps
            .iter()
            .position(|s| *s == ActivationStep::CommitActive)
            .expect("缺少提交步骤");
        assert_eq!(commit, steps.len() - 2);
        assert_eq!(steps[commit + 1], ActivationStep::Cleanup);
        // 探测排在提交之前，保证失败时槽位未被改写。
        let probe = steps
            .iter()
            .position(|s| *s == ActivationStep::ProbeContract)
            .unwrap();
        assert!(probe < commit);
    }

    /// 卡片渲染所需的合并视图：端口、active、停用与快照各归各位。
    #[test]
    fn instance_views_carry_port_active_and_snapshot() {
        let instance = |id: &str, role: &str| -> Instance {
            serde_json::from_value(serde_json::json!({
                "instance_id": id,
                "module_id": "omninexus",
                "variant": "stable",
                "version": "0.1.0",
                "role": role,
                "category": "optional",
                "verdict": "A",
                "confirmed_at": "2026-11-13T00:00:00Z",
                "installed_at": "2026-11-13T00:00:00Z",
                "source_kind": "zip",
                "pinned": false,
                "auto_update": false,
            }))
            .unwrap()
        };
        let active_instance = instance("omninexus@stable", "llm");
        let standby = instance("omninexus@local", "llm");
        let mut ports = std::collections::BTreeMap::new();
        ports.insert("omninexus@stable".to_string(), Some(8102));
        ports.insert("omninexus@local".to_string(), Some(18101));
        let mut active = std::collections::BTreeMap::new();
        active.insert("llm".to_string(), "omninexus@stable".to_string());
        let disabled: BTreeSet<String> = BTreeSet::new();
        let mut snapshots = std::collections::BTreeMap::new();
        snapshots.insert(
            "omninexus@stable".to_string(),
            Snapshot {
                instance_id: "omninexus@stable".into(),
                phase: Phase::Running,
                ticket: 3,
                pid: Some(99),
                health: ProbeState::Healthy,
                contract: ProbeState::Healthy,
                exit_code: None,
                last_error: None,
            },
        );

        let views = compose_instance_views(
            vec![active_instance, standby],
            &ports,
            &active,
            &disabled,
            &snapshots,
        );
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].port, Some(8102));
        assert!(views[0].active);
        assert_eq!(views[0].snapshot.as_ref().unwrap().pid, Some(99));
        // 备用实例：端口不同、不是 active、没有快照（没被启动过）。
        assert_eq!(views[1].port, Some(18101));
        assert!(!views[1].active);
        assert!(views[1].snapshot.is_none());
    }

    /// 造一个未签名的合法包：`files` 必须与 zip 内条目逐一对应。
    fn unsigned_package() -> Vec<u8> {
        use sha2::{Digest, Sha256};
        use std::io::Write as _;
        use zip::write::SimpleFileOptions;

        let body = b"[project]\nname = \"demo\"\n";
        let sha = format!("sha256:{}", hex::encode(Sha256::digest(body)));
        let manifest = serde_json::json!({
            "schema_version": 2,
            "module_id": "dounai/demo-local",
            "name": "演示模块",
            "version": "0.1.0",
            "variant": "stable",
            "role": "llm",
            "category": "optional",
            "publisher": { "id": "demo", "name": "Demo" },
            "requires": { "launcher": ">=1.0.0" },
            "runtime": {
                "python": ">=3.11,<3.14",
                "venv": "{instance_venv}",
                "deps": { "kind": "pyproject", "path": "pyproject.toml" }
            },
            "entry": { "command": "{venv_python} -m demo", "env": {}, "health": null },
            "ports": [8101],
            "permissions": { "network": [], "filesystem": [], "devices": [] },
            "files": { "pyproject.toml": sha },
            "signature": null
        });
        let mut out = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut out));
            let options = SimpleFileOptions::default();
            writer.start_file("manifest.json", options).unwrap();
            writer.write_all(manifest.to_string().as_bytes()).unwrap();
            writer.start_file("pyproject.toml", options).unwrap();
            writer.write_all(body).unwrap();
            writer.finish().unwrap();
        }
        out
    }

    #[test]
    fn local_package_needs_confirmation_then_installs() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppStateV2::new(dir.path().join("app"), dir.path().join("data")).unwrap();
        let zip_path = dir.path().join("demo.zip");
        std::fs::write(&zip_path, unsigned_package()).unwrap();

        let (resolved, bytes) = read_local_zip(zip_path.to_str().unwrap()).unwrap();
        assert_eq!(resolved, zip_path);
        let (verified, trust) = inspect_bytes(&state, &bytes).unwrap();
        // 未签名一律 C 类，必须让用户显式确认。
        assert_eq!(trust.verdict, "C");
        assert!(trust.needs_confirmation);
        assert!(!trust.signed);
        assert_eq!(verified.files.len(), 1);

        let sha = verified.sha256.clone();
        let err = commit_install(&state, &bytes, &sha, None, None, false).unwrap_err();
        assert!(err.contains("需要用户显式确认"), "{err}");
        assert!(state.instance("dounai/demo-local@stable").is_none());

        let response = commit_install(&state, &bytes, &sha, None, None, true).unwrap();
        assert_eq!(response.module_id, "dounai/demo-local");
        assert_eq!(response.files, 1);
        assert!(!response.replaced);
        assert!(state.instance("dounai/demo-local@stable").is_some());

        // 装第二次会覆盖：包目录已存在，返回 replaced。
        let again = commit_install(&state, &bytes, &sha, None, None, true).unwrap();
        assert!(again.replaced);
    }

    #[test]
    fn local_package_hash_mismatch_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppStateV2::new(dir.path().join("app"), dir.path().join("data")).unwrap();
        let bytes = unsigned_package();
        // 检查后文件被换掉：调用方带着旧哈希来装，必须失败而不是装进去。
        let bogus = format!("sha256:{}", "0".repeat(64));
        let err = commit_install(&state, &bytes, &bogus, None, None, true).unwrap_err();
        assert!(err.contains("整包哈希不符"), "{err}");
        assert!(state.instance("dounai/demo-local@stable").is_none());
    }
}
