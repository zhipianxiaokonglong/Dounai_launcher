//! M3 命令面：命令名、参数与返回形状与 v1 完全一致，内部改走 v2。
//!
//! 这样 `src/bridge.ts` 不需要改一行；v1 命令面已移除，这些 DTO 是唯一真源，
//! 不会出现「Rust 改了形状、TS typecheck 查不出来」。
//!
//! 边界：状态文件只由 `AppStateV2` 写；进程只由 `Supervisor` 管；
//! 验签、目录确认、端口分配与启动编排分别由 core / `lifecycle_ops` 负责。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager, State};

use launcher_core::lifecycle::{
    activation_steps, rollback_steps, ActivationStep, Phase, ProbeState, RecoveryStep, RolesDoc,
    Snapshot,
};
use launcher_core::admin_host::{AdminHost, Bounds, MountRequest};
use launcher_core::m2::adapters::RemoteCatalogAdapter;
use launcher_core::m2::catalog::{
    check_update_allowed, select_catalog_entry, version_key, CatalogEntry,
};
use launcher_core::m2::installed::Instance;
use launcher_core::m2::local::{observe_source, validate_local_dir};
use launcher_core::m2::manifest::PackageManifest;
use launcher_core::m2::package::{install_package, verify_zip, ZipExpectation};
use launcher_core::m2::source::{validate_download_url, SourceAdapter, SourceError};
use launcher_core::m2::sources::{
    derive_host_suffixes, valid_id, validate_suffix, SourceClass, SourceEntry, SourceRegistry,
    OFFICIAL_GROUP,
};
use launcher_core::m2::trust::{classify, ClassifyContext};
use launcher_core::m2::{semver_cmp, Category, InstanceSourceKind, Verdict, MAX_UNPACKED_BYTES};
use launcher_core::manifest::{ModuleManifestV2, Role};

use crate::lifecycle_ops::{
    all_instances, assign_ports, build_start_request, ordered_instances, package_dir, write_manifest,
    MANIFEST_FILE,
};
use crate::lifecycle_runtime::Supervisor;
use crate::m2_http::ReqwestCatalogFetcher;
use crate::state_v2::{bare_sha256, prefixed_sha256, AppStateV2, CachedEntry};

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
pub struct SourceStatusView {
    pub id: String,
    pub name: String,
    /// ok / unavailable / invalid。
    pub state: String,
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SourceView {
    pub id: String,
    pub name: String,
    pub kind: SourceClass,
    pub base_url: String,
    pub catalog_fingerprint: String,
    pub host_suffixes: Vec<String>,
    pub enabled: bool,
    pub deletable: bool,
    pub auto_update: bool,
    pub revoked: bool,
    pub sequence: Option<u64>,
    pub expires: Option<String>,
}

impl From<&SourceEntry> for SourceView {
    fn from(source: &SourceEntry) -> Self {
        Self {
            id: source.id.clone(),
            name: source.name.clone(),
            kind: source.kind,
            base_url: source.base_url.clone(),
            catalog_fingerprint: source.catalog_fingerprint.clone(),
            host_suffixes: source.host_suffixes.clone(),
            enabled: source.enabled,
            deletable: source.deletable,
            auto_update: source.auto_update,
            revoked: source.is_revoked(),
            sequence: source.sequence,
            expires: source.expires.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SourcesView {
    pub sources: Vec<SourceView>,
    pub allowlist: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct SourcePreview {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub catalog_pubkey: String,
    /// 完整指纹，界面必须原样展示后才允许确认。
    pub catalog_fingerprint: String,
    pub host_suffixes: Vec<String>,
    /// true 表示后缀由 base_url 派生、用户尚未改过。
    pub derived: bool,
    pub note: String,
}

#[derive(Debug, Serialize)]
pub struct RevokeSourceResult {
    pub sources: SourcesView,
    /// 从该源安装、现在标「发布者已不再受信任」的实例 ID。
    pub affected: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct CatalogEntryView {
    /// 会话缓存主键，安装时原样回传，界面不做隐式优选。
    pub entry_key: String,
    pub source_id: String,
    pub source_name: String,
    pub source_kind: SourceClass,
    pub catalog_fingerprint: String,
    pub module_id: String,
    pub variant: String,
    pub name: String,
    pub version: String,
    pub role: Role,
    /// 目录声明的整包哈希（`sha256:` 前缀），界面原样展示。
    pub sha256: String,
    pub size: u64,
    /// 本源组内的下载地址，仅供排障阅读；安装时由 Rust 重新校验。
    pub download_url: String,
    pub signature_required: bool,
    pub publisher_fp: String,
    pub min_launcher: Option<String>,
    pub min_version: Option<String>,
    pub expires: String,
    pub revoked: bool,
    /// available / installed / upgrade / downgrade / incompatible。
    pub installed_state: String,
    pub auto_update: bool,
    pub source_revoked: bool,
    /// 版本回退 / 不兼容等判定理由，界面原样展示。
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CatalogResponse {
    /// remote / partial / cache / unavailable。
    pub source: String,
    pub entries: Vec<CatalogEntryView>,
    pub sources: Vec<SourceStatusView>,
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
pub struct InstallResponse {
    pub module_id: String,
    pub files: usize,
    pub bytes: u64,
    pub replaced: bool,
    pub install_dir: String,
    pub trust: TrustVerdictView,
    /// 装完之后顺便占下了该 role 的 active 槽时，这里是那个实例 ID。
    ///
    /// 只有槽原本为空才会发生。非 active 实例一律跑 18101+ 的备用端口，
    /// 而组装层只认 8101-8103，所以每一条链路的第一件模块必须上位。
    pub activated: Option<String>,
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
    /// 本次读的是哪一份文件；`None` 表示「跨文件的最新尾部」。
    pub file: Option<String>,
    /// 子进程 stdout/stderr 是否落盘（由启动时的开关决定，页面只做展示）。
    pub child_output: bool,
    /// 日志目录绝对路径，供「打开目录」与说明文案使用。
    pub dir: String,
}

/// 一份日志文件的元信息。
#[derive(Debug, Serialize)]
pub struct LogFileInfo {
    pub name: String,
    pub size: u64,
    /// UTC 时间字符串，界面负责转本地时区。
    pub modified_at: String,
    /// 是否是最新（正在写）的那一份。
    pub current: bool,
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
pub struct KeyRequest {
    pub fingerprint: String,
    #[serde(default)]
    pub label: Option<String>,
}

/// 添加源第一步：只算指纹与派生白名单，绝不落盘。
#[derive(Debug, Deserialize)]
pub struct InspectSourceRequest {
    pub base_url: String,
    pub catalog_pubkey: String,
    #[serde(default)]
    pub host_suffixes: Option<Vec<String>>,
}

/// 添加源第二步。`confirm=false` 一律拒绝写入，界面必须先展示完整指纹。
#[derive(Debug, Deserialize)]
pub struct AddSourceRequest {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub catalog_pubkey: String,
    pub host_suffixes: Vec<String>,
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Deserialize)]
pub struct SourceIdRequest {
    pub id: String,
    #[serde(default)]
    pub enabled: Option<bool>,
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

/* ------------------------------------------------------------------ */
/* 生命周期                                                            */
/* ------------------------------------------------------------------ */

#[tauri::command]
pub async fn start_module(module_id: String, app: tauri::AppHandle) -> Result<RunView, String> {
    // 启动要 spawn 进程、短探端口、等健康检查，放主线程会冻住窗口。
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppStateV2>();
        let supervisor = app.state::<Supervisor>();
        start_module_impl(&module_id, &state, &supervisor)
    })
    .await
    .map_err(|e| format!("启动没有跑完：{e}"))?
}

fn start_module_impl(
    key: &str,
    state: &AppStateV2,
    supervisor: &Supervisor,
) -> Result<RunView, String> {
    let instance = state.resolve_instance(key)?;
    let port = single_instance_port(state, supervisor, &instance);
    Ok(match start_instance(state, supervisor, &instance, port)? {
        StartOutcome::Managed(snap) => run_view(&instance.module_id, Some(&snap), None),
        StartOutcome::External => external_run_view(&instance.module_id),
    })
}

/// 启动结果：由本启动器托管，或端口上已有外部健康服务。
enum StartOutcome {
    Managed(Snapshot),
    External,
}

/// 单实例启动时的端口；与 start_all 的整体分配保持同一套规则。
///
/// 必须传整张实例表：备用端口是按顺序分配的，只传当前这一个实例时，
/// 每个非 active 实例都会拿到 `18101`，后启动的直接撞端口退出（退出码 3）。
fn single_instance_port(
    state: &AppStateV2,
    supervisor: &Supervisor,
    instance: &Instance,
) -> Option<u16> {
    let all = all_instances(state);
    assign_ports(state, supervisor.roles(), &all)
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
    // 进程表里还留着它：上一轮探测超时最常见。先看它是不是已经自己好了，
    // 好了就把状态收回 Running，而不是再 spawn 一个抢同一个端口。
    if supervisor.owns(&instance.instance_id) {
        let healthy = prestart_health_url(supervisor.roles(), instance, port)
            .map(|url| Supervisor::probe_external_once(&url, PRESTART_TIMEOUT_MS))
            .unwrap_or(false);
        if healthy {
            if let Some(snap) = supervisor.recover(&instance.instance_id) {
                return Ok(StartOutcome::Managed(snap));
            }
            // 状态机没处在「已判失败」的位置（已经 Running 或仍在 Starting）：
            // 进程健康就够了，原样返回快照，让界面刷新到最新状态。
            if let Some(snap) = supervisor.snapshot().get(&instance.instance_id).cloned() {
                return Ok(StartOutcome::Managed(snap));
            }
        }
        return Err(format!(
            "{} 的进程还在运行，但健康检查没通过。先点「停止」，再启动，避免同时跑两个实例。",
            instance.module_id
        ));
    }
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
pub async fn stop_module(module_id: String, app: tauri::AppHandle) -> Result<RunView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppStateV2>();
        let supervisor = app.state::<Supervisor>();
        let instance = state.resolve_instance(&module_id)?;
        let snap = supervisor.stop(&instance.instance_id)?;
        Ok(run_view(&instance.module_id, Some(&snap), None))
    })
    .await
    .map_err(|e| format!("停止没有跑完：{e}"))?
}

#[tauri::command]
pub async fn start_all(app: tauri::AppHandle) -> Vec<RunView> {
    match tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppStateV2>();
        let supervisor = app.state::<Supervisor>();
        start_all_impl(&state, &supervisor)
    })
    .await
    {
        Ok(out) => out,
        Err(error) => vec![run_view(
            "start_all",
            None,
            Some(format!("启动没有跑完：{error}")),
        )],
    }
}

fn start_all_impl(state: &AppStateV2, supervisor: &Supervisor) -> Vec<RunView> {
    // 端口按全集分配，再挑出要启动的那些：只按启动集合算，同一实例
    // 在列表里显示 18102、这里却可能分到 18101。
    let all = all_instances(state);
    let ports = assign_ports(state, supervisor.roles(), &all);
    let instances: Vec<Instance> = all
        .into_iter()
        .filter(|i| !state.is_disabled(&i.module_id))
        .collect();
    let ordered = match ordered_instances(supervisor.roles(), instances) {
        Ok(list) => list,
        Err(error) => return vec![run_view("start_all", None, Some(error))],
    };
    let mut out = Vec::with_capacity(ordered.len());
    for instance in &ordered {
        // 整批分配端口仍在这里做，start_instance 只负责单个实例的启动前探测。
        let port = ports.get(&instance.instance_id).copied().flatten();
        match start_instance(state, supervisor, instance, port) {
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
pub async fn stop_all(app: tauri::AppHandle) -> Vec<RunView> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppStateV2>();
        let supervisor = app.state::<Supervisor>();
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
    })
    .await
    .unwrap_or_default()
}

#[tauri::command]
pub async fn restart_instance(
    instance_id: String,
    app: tauri::AppHandle,
) -> Result<RunView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppStateV2>();
        let supervisor = app.state::<Supervisor>();
        restart_instance_impl(&instance_id, &state, &supervisor)
    })
    .await
    .map_err(|e| format!("重启没有跑完：{e}"))?
}

fn restart_instance_impl(
    instance_id: &str,
    state: &AppStateV2,
    supervisor: &Supervisor,
) -> Result<RunView, String> {
    let instance = state
        .instance(instance_id)
        .ok_or_else(|| format!("实例不存在：{instance_id}"))?;
    if supervisor.is_external(instance_id) {
        return Err(format!("实例 {instance_id} 由外部启动，启动器无法重启"));
    }
    if supervisor.running().contains(instance_id) {
        supervisor.stop(instance_id)?;
    }
    let port = single_instance_port(state, supervisor, &instance);
    Ok(match start_instance(state, supervisor, &instance, port)? {
        StartOutcome::Managed(snap) => run_view(&instance.module_id, Some(&snap), None),
        StartOutcome::External => external_run_view(&instance.module_id),
    })
}

/* ------------------------------------------------------------------ */
/* 状态查询与开关                                                      */
/* ------------------------------------------------------------------ */

/// 界面一次拿齐渲染一张卡片所需的全部事实。
///
/// 模块自带管理界面的入口声明（来自模块根 manifest 的 `admin_ui.web_ui`）。
#[derive(Debug, Clone, Serialize)]
pub struct AdminUiView {
    pub label: String,
    /// 必须以 `/` 开头；完整 URL 由 `open_admin_window` 按端口拼。
    pub path: String,
    pub requires_running: bool,
}

/// 界面一次拿齐渲染一张卡片所需的全部事实。
///
/// 光有 `Instance` 不够：端口由 `assign_ports` 按 active / 备用端口段分配，
/// 前端自己推算必然与真实启动值分叉（M4 的卡片要显示端口）。
/// 所以端口只从这里出，前端不复制分配规则。
#[derive(Debug, Serialize)]
pub struct InstanceRuntimeView {
    pub instance: Instance,
    /// 模块清单里的展示名；读不到退回 module_id。
    ///
    /// 为什么由 Rust 提供而不是前端另读一份内置清单：内置 manifest 的
    /// module_id 是 v1 的 `dounai-input`，实例是 v2 的 `dounai/dounai-input`，
    /// 前端用内置清单查表永远落空，管理页按钮也就永远不出现。
    pub name: String,
    /// 没有声明管理界面的模块为 None，界面据此不渲染按钮。
    pub admin_ui: Option<AdminUiView>,
    /// 该实例真正会在用的端口；`shell` 与未分配备用端口时为 None。
    pub port: Option<u16>,
    /// 是否为所在 role 的 active 槽实例。
    pub active: bool,
    /// 模块级停用标记（按 module_id 记录，不是按实例）。
    pub disabled: bool,
    /// 生命周期快照；启动器没见过的实例为 None。
    pub snapshot: Option<Snapshot>,
    /// 依赖环境是否已就绪。shell 角色不要运行时，恒为 true。
    /// 未就绪时界面不给「启动」，先引导装依赖——否则点下去只会失败。
    pub prepared: bool,
    /// 当前依赖快照是否由签名包里的 pinned 哈希锚定。
    ///
    /// `None` 表示没有记录（未准备或 shell）。`Some(false)` 只说明「这个包的 manifest
    /// 没声明快照哈希，这份快照是运行时算出来的」，不是错误，界面据此提示一句。
    pub env_pinned: Option<bool>,
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
    let prepared: BTreeSet<String> = instances
        .iter()
        .filter(|i| {
            // shell 不需要运行时，直接算就绪；其余要能读到生成好的 venv。
            !i.role.needs_runtime()
                || launcher_core::verify_instance(&state.paths, &i.instance_id).is_ok()
        })
        .map(|i| i.instance_id.clone())
        .collect();
    let briefs: std::collections::BTreeMap<String, ManifestBrief> = instances
        .iter()
        .map(|i| (i.instance_id.clone(), manifest_brief(&state, i)))
        .collect();
    let env_facts = EnvFacts {
        prepared,
        pinned: instances
            .iter()
            .filter_map(|i| {
                launcher_core::env::instance_snapshot_pinned(&state.paths, &i.instance_id)
                    .map(|value| (i.instance_id.clone(), value))
            })
            .collect(),
    };
    compose_instance_views(
        instances,
        &ports,
        &active,
        &disabled,
        &supervisor.snapshot(),
        &env_facts,
        &briefs,
    )
}

/// 卡片需要的环境事实，由命令层算好，组合函数只负责落位。
///
/// 合成一个结构体而不是继续加参数：这两个字段都来自 runtime.json 与 verify，
/// 语义上是同一件事，分开传只会让签名越挂越长。
#[derive(Debug, Default)]
struct EnvFacts {
    /// 依赖环境已就绪的实例。
    prepared: BTreeSet<String>,
    /// 快照是否由签名包锚定；没有记录的实例不在表里。
    pinned: std::collections::BTreeMap<String, bool>,
}

/// 卡片展示用的清单摘要。
#[derive(Debug, Clone)]
struct ManifestBrief {
    name: String,    admin_ui: Option<AdminUiView>,
}

/// 读模块根 manifest 取展示字段。
///
/// 读不到不算错误：卡片退回 module_id 展示，管理页按钮不出现——
/// 这与「声明了才给入口」是同一件事，不编造一个不存在的界面。
fn manifest_brief(state: &AppStateV2, instance: &Instance) -> ManifestBrief {
    let raw = match read_manifest_value(&state.module_dir(instance)) {
        Ok(value) => value,
        Err(_) => {
            return ManifestBrief {
                name: instance.module_id.clone(),
                admin_ui: None,
            }
        }
    };
    let name = raw
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(&instance.module_id)
        .to_string();
    // shell 角色的管理界面不依赖实例运行，其余默认要求已在运行。
    let default_requires_running = instance.role.needs_runtime();
    let admin_ui = raw
        .pointer("/admin_ui/web_ui")
        .and_then(|web| {
            let path = web.get("path")?.as_str()?;
            if !path.starts_with('/') {
                return None;
            }
            Some(AdminUiView {
                label: web
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap_or("打开管理界面")
                    .to_string(),
                path: path.to_string(),
                requires_running: web
                    .get("requires_running")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(default_requires_running),
            })
        });
    ManifestBrief { name, admin_ui }
}

/// 组合逻辑单独成函数：命令层只取数据，排序、端口回填与 active 判定都在这里，
/// 便于用普通单测覆盖（`State` 在测试里不可构造）。
fn compose_instance_views(
    instances: Vec<Instance>,
    ports: &std::collections::BTreeMap<String, Option<u16>>,
    active: &std::collections::BTreeMap<String, String>,
    disabled: &BTreeSet<String>,
    snapshots: &std::collections::BTreeMap<String, Snapshot>,
    env_facts: &EnvFacts,
    briefs: &std::collections::BTreeMap<String, ManifestBrief>,
) -> Vec<InstanceRuntimeView> {
    instances
        .into_iter()
        .map(|instance| {
            let brief = briefs.get(&instance.instance_id);
            InstanceRuntimeView {
                name: brief
                    .map(|b| b.name.clone())
                    .unwrap_or_else(|| instance.module_id.clone()),
                admin_ui: brief.and_then(|b| b.admin_ui.clone()),
                port: ports.get(&instance.instance_id).copied().flatten(),
                active: active.values().any(|id| id == &instance.instance_id),
                disabled: disabled.contains(&instance.module_id),
                prepared: env_facts.prepared.contains(&instance.instance_id),
                env_pinned: env_facts.pinned.get(&instance.instance_id).copied(),
                snapshot: snapshots.get(&instance.instance_id).cloned(),
                instance,
            }
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
    // 卸掉的若是原本的 active，槽位会空出来，剩下同 role 的实例只跑备用端口，
    // 组装层就再也看不到这个角色了。补一个上位（C 类仍要用户显式切换）。
    // 已经在跑的实例保持原端口，下次启动才落到规范端口。
    let _ = fill_vacant_active(&state, instance.role, None, false);
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
pub async fn switch_active(
    instance_id: String,
    app: tauri::AppHandle,
) -> Result<Vec<String>, String> {
    // 切换要停旧、起新、同步等 health，最长接近契约上限，必须离开主线程。
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppStateV2>();
        let supervisor = app.state::<Supervisor>();
        switch_active_impl(&instance_id, &state, &supervisor)
    })
    .await
    .map_err(|e| format!("切换没有跑完：{e}"))?
}

fn switch_active_impl(
    instance_id: &str,
    state: &AppStateV2,
    supervisor: &Supervisor,
) -> Result<Vec<String>, String> {
    let instance = state
        .instance(instance_id)
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
                    match start_instance(state, supervisor, &instance, port) {
                        Ok(StartOutcome::Managed(_)) => Ok(()),
                        Ok(StartOutcome::External) => Err(format!(
                            "规范端口上已有外部服务，无法确认它就是 {next}；请先关闭再切换"
                        )),
                        Err(error) => Err(error),
                    }
                }
            }
            ActivationStep::ProbeHealth => wait_for_activation(supervisor, &next, false),
            ActivationStep::ProbeContract => wait_for_activation(supervisor, &next, true),
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
        let notes = rollback_activation(state, supervisor, previous.as_deref(), &next, failed);
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
/// 在线安装时随包一起落盘的来源信息。
#[derive(Debug, Clone)]
pub struct InstallSource {
    pub source_id: String,
    pub source_kind: SourceClass,
    pub auto_update: bool,
}

/// 在线安装时随包一起落盘的来源与目录条目。本地 zip 导入传 None。
/// 收成一个参数，是为了不把「这次安装有没有来源」藏进一长串位置参数里。
#[derive(Debug, Clone, Default)]
pub struct CatalogContext {
    pub source: Option<InstallSource>,
    pub entry: Option<CatalogEntry>,
}

fn commit_install(
    state: &AppStateV2,
    bytes: &[u8],
    expected_sha256: &str,
    module_id_in_request: Option<&str>,
    version_in_request: Option<&str>,
    confirmed: bool,
    catalog: Option<CatalogContext>,
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
    let cached_entry = catalog.as_ref().and_then(|c| c.entry.clone());
    let source = catalog.as_ref().and_then(|c| c.source.as_ref());
    let previous_fp = state.with_installed(|s| {
        s.instances
            .values()
            .find(|i| {
                i.module_id == module_id
                    && i.variant == verified.manifest.variant
                    && i.source_id == source.map(|src| src.source_id.clone())
            })
            .and_then(|i| i.publisher_fp.clone())
    });
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

    // 社区源的实例 id 带来源后缀，跨源同名才真的并存；官方源沿用 M1 的
    // `module_id@variant`，老实例与 active 槽都不变。
    let instance_id = match source {
        Some(src) if src.source_kind == SourceClass::Community => format!(
            "{}@{}@{}",
            verified.manifest.module_id, verified.manifest.variant, src.source_id
        ),
        _ => format!(
            "{}@{}",
            verified.manifest.module_id, verified.manifest.variant
        ),
    };
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
        source_id: source.map(|s| s.source_id.clone()),
        git: None,
        pinned: false,
        // 社区源永远 false；A 类官方包沿用四级判定结果。
        auto_update: classification.verdict.can_auto_update()
            && source.map(|s| s.auto_update).unwrap_or(true),
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
    // 该 role 的第一件模块直接占下 active 槽：槽空着的话它只会跑备用端口，
    // 而组装层只认 8101-8103，装完也用不起来。补位失败不影响安装结果。
    let activated = fill_vacant_active(state, verified.manifest.role, Some(&instance_id), false);

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
        activated,
    })
}

/// 启动时把每个 role 的空 active 槽补齐，返回补上位的实例 ID。
///
/// 老版本的安装流程不写 active，装出来三个模块全在 18101+ 的备用端口上跑，
/// 组装层只认 8101-8103，于是界面看着一切正常、链路一条都连不上。
/// 「有实例却没有 active」不是可用的状态，启动时一次性修好。
/// C 类实例仍按契约等用户在菜单里显式切换。
pub fn heal_active_slots(state: &AppStateV2) -> Vec<String> {
    let mut healed = Vec::new();
    for role in [Role::Input, Role::Llm, Role::Output] {
        if let Some(id) = fill_vacant_active(state, role, None, false) {
            healed.push(id);
        }
    }
    healed
}

/// 补齐某个 role 的 active 槽，返回刚上位的实例 ID。
///
/// 槽位已占用时什么都不做（新装的退居备用实例，用户可在菜单里切）。
/// `explicit` 见 `InstalledState::fill_vacant_active`：只有用户亲手挑的
/// 本地目录才算显式指名，能让 C 类实例上位。
/// 失败只当没补上，安装本身不因为这一步失败。
fn fill_vacant_active(
    state: &AppStateV2,
    role: launcher_core::manifest::Role,
    preferred: Option<&str>,
    explicit: bool,
) -> Option<String> {
    let path = state.installed_path();
    state
        .mutate_installed(|st| {
            st.fill_vacant_active(&path, role, preferred, explicit)
                .map_err(|e| e.to_string())
        })
        .ok()
        .flatten()
        .map(|plan| plan.next)
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
    let cached_entry = state.cached_entries().values().find(|cached| {
        cached.entry.module_id == verified.manifest.module_id
            && cached.entry.variant == verified.manifest.variant
    }).map(|cached| cached.entry.clone());
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
        None,
    )
}

/* ------------------------------------------------------------------ */
/* 依赖准备的进度事件                                                  */
/* ------------------------------------------------------------------ */

/// 依赖准备的过程事件。前端按 `instanceId` 落到对应卡片上。
pub const EVENT_ENV_PROGRESS: &str = "instance-env-progress";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvProgressEvent {
    pub instance_id: String,
    /// `uv` / `python` / `resolve` / `sync` / `verify` / `done`。
    pub phase: &'static str,
    pub title: String,
    pub detail: Option<String>,
    pub current: Option<u32>,
    pub total: Option<u32>,
}

/// 把引擎的阶段上报转成前端事件，并做一次降频。
///
/// 为什么降频：`uv -v` 在装大包时会连续刷行，每行都发一次事件会把 webview 的
/// 消息队列排满。阶段或标题变化立即发，只有细节变化的按 150ms 合并。
fn env_progress_sink(
    app: &tauri::AppHandle,
    instance_id: &str,
) -> launcher_core::env::Sink {
    use std::sync::{Arc, Mutex};
    let app = app.clone();
    let id = instance_id.to_string();
    let gate: Arc<Mutex<Option<(String, String, Instant)>>> = Arc::new(Mutex::new(None));
    Arc::new(move |progress: launcher_core::env::EnvProgress| {
        let phase = progress.phase.as_str().to_string();
        let detail = progress.detail.clone().unwrap_or_default();
        {
            let mut guard = gate.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((last_phase, last_detail, at)) = guard.as_ref() {
                if *last_phase == phase {
                    if *last_detail == detail {
                        return;
                    }
                    if at.elapsed() < Duration::from_millis(150) {
                        return;
                    }
                }
            }
            *guard = Some((phase.clone(), detail, Instant::now()));
        }
        let _ = app.emit(
            EVENT_ENV_PROGRESS,
            EnvProgressEvent {
                instance_id: id.clone(),
                phase: progress.phase.as_str(),
                title: progress.title,
                detail: progress.detail,
                current: progress.current,
                total: progress.total,
            },
        );
    })
}

#[tauri::command]
pub async fn prepare_instance_env(
    instance_id: String,
    allow_online: Option<bool>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    // 依赖准备要建 venv、跑 uv sync，离线也可能几十秒，联网更久。
    let progress_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = progress_app.state::<AppStateV2>();
        let supervisor = progress_app.state::<Supervisor>();
        prepare_instance_env_impl(
            &instance_id,
            allow_online,
            &state,
            &supervisor,
            Some(env_progress_sink(&progress_app, &instance_id)),
        )
    })
    .await
    .map_err(|e| format!("依赖准备没有跑完：{e}"))?
}

fn prepare_instance_env_impl(
    instance_id: &str,
    allow_online: Option<bool>,
    state: &AppStateV2,
    supervisor: &Supervisor,
    sink: Option<launcher_core::env::Sink>,
) -> Result<String, String> {
    let instance = state
        .instance(instance_id)
        .ok_or_else(|| format!("实例不存在：{instance_id}"))?;
    if !instance.role.needs_runtime() {
        return Err(format!("{:?} 角色不需要准备依赖环境", instance.role));
    }
    // `.staging` 路径按实例哈希固定，同一实例并发 prepare 会互删。
    // 锁槽在实例粒度，不同实例的准备仍然并行。
    let slot = state.instance_slot(instance_id);
    let _exclusive = slot.lock().unwrap_or_else(|e| e.into_inner());
    let dir = state.module_dir(&instance);
    let raw = read_manifest_value(&dir)?;
    let manifest = runtime_manifest(&raw)?;
    let install = launcher_core::install::InstallState::load(&state.paths).map_err(|e| {
        format!(
            "读取 install.json 失败：{e}。启动时会自动补写，若这个报错持续出现，\
             删掉 {} 后重启即可重建。",
            state.paths.install_state_path().display()
        )
    })?;
    let runner = launcher_core::env::SystemUvRunner::new(state.paths.install_root.clone());
    let fetcher = launcher_core::env::CurlUvFetcher;
    // 首次装依赖必然要拉包与解释器，离线默认等于按钮天生不可用。
    // 想强制离线时由界面显式传 false（模块菜单里的「安装依赖（离线）」）。
    let online = allow_online.unwrap_or(true);
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
    let prepared = launcher_core::env::progress::with_sink(sink, || {
        launcher_core::env::prepare_instance_with(&state.paths, instance_id, &manifest, &dir, &ctx)
    })
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

/// 内置分发根：SourceForge 项目 `dounai-releases` 的下载目录。
///
/// 用一条源表记录装配远程适配器：公钥、指纹与白名单全来自本地表。
/// 没有「任意 URL + 内置 K5」这条通道，也没有会话级换源入口。
fn source_adapter<'a>(
    source: &SourceEntry,
    registry: &SourceRegistry,
    http: &'a dyn launcher_core::m2::adapters::HttpFetcher,
) -> RemoteCatalogAdapter<'a> {
    RemoteCatalogAdapter::from_source(source, registry.group_ids(&source.id), http)
}

fn sources_view(registry: &SourceRegistry) -> SourcesView {
    SourcesView {
        sources: registry.sources.iter().map(SourceView::from).collect(),
        allowlist: registry.allowlist(),
    }
}

fn source_status(source: &SourceEntry, state: &str, reason: Option<String>) -> SourceStatusView {
    SourceStatusView {
        id: source.id.clone(),
        name: source.name.clone(),
        state: state.to_string(),
        reason,
    }
}

/// 把一份已验签的 catalog 展开成会话缓存条目，键含来源、变体与版本。
fn collect_entries(
    registry: &SourceRegistry,
    source: &SourceEntry,
    doc: &launcher_core::m2::catalog::CatalogDocument,
    out: &mut BTreeMap<String, CachedEntry>,
) {
    let group_ids = registry.group_ids(&source.id);
    for entry in &doc.modules {
        let cached = CachedEntry::new(source, group_ids.clone(), doc, entry);
        out.insert(cached.key.clone(), cached);
    }
}

/// 版本与兼容性判定。`installed_version` 为 None 表示首次安装，
/// 不检查只针对更新链路的 `min_version`，但仍查版本回退与撤销列表。
fn entry_installability(
    cached: &CachedEntry,
    installed_version: Option<&str>,
    catalog_state: &launcher_core::m2::catalog::CatalogState,
) -> Result<String, String> {
    let entry = &cached.entry;
    let key = version_key(&entry.publisher_fp, &entry.module_id, &entry.variant);
    if let Some(seen) = catalog_state.last_seen_version.get(&key) {
        if semver_cmp(&entry.version, seen).map_err(|e| e.to_string())? == std::cmp::Ordering::Less {
            return Ok("downgrade".into());
        }
    }
    if let Some(min_launcher) = &entry.min_launcher {
        let current = env!("CARGO_PKG_VERSION");
        if semver_cmp(current, min_launcher).map_err(|e| e.to_string())? == std::cmp::Ordering::Less {
            return Ok("incompatible".into());
        }
    }
    if let Some(local) = installed_version {
        check_update_allowed(&cached.doc, catalog_state, entry, local)
            .map_err(|e| e.reason().to_string())?;
        return Ok(match semver_cmp(&entry.version, local).map_err(|e| e.to_string())? {
            std::cmp::Ordering::Greater => "upgrade".into(),
            std::cmp::Ordering::Equal => "installed".into(),
            std::cmp::Ordering::Less => "downgrade".into(),
        });
    }
    let revoked = cached
        .doc
        .revoked
        .iter()
        .any(|r| r.id == entry.version || r.id == entry.sha256)
        || entry
            .revoked
            .iter()
            .any(|id| id == &entry.version || id == &entry.sha256);
    Ok(if revoked { "downgrade" } else { "available" }.into())
}

/// 按 sources.json 里已启用的源刷新目录。
///
/// 官方主源与镜像是同一份 catalog 的两个副本，走二选一裁决；不同发布者的
/// 社区源各自独立验签、独立记水位，成功条目并列合并。任何一个源失败都不
/// 影响其它源，也不提供「仍然继续 / 跳过验签」。
#[tauri::command]
pub fn refresh_catalog(state: State<'_, AppStateV2>) -> CatalogResponse {
    let registry = state.sources();
    let allowlist = registry.allowlist();
    let enabled: Vec<SourceEntry> = registry.enabled().into_iter().cloned().collect();
    if enabled.is_empty() {
        return CatalogResponse {
            source: "unavailable".into(),
            entries: Vec::new(),
            sources: registry
                .sources
                .iter()
                .map(|s| source_status(s, "disabled", None))
                .collect(),
            allowlist,
            reason: Some("没有启用的下载源，未发出任何远程请求".into()),
        };
    }

    let now = AppStateV2::now();
    let mut statuses: Vec<SourceStatusView> = Vec::new();
    let mut cache: BTreeMap<String, CachedEntry> = BTreeMap::new();
    let mut observed: Vec<launcher_core::m2::catalog::CatalogDocument> = Vec::new();
    let mut registry_next = registry.clone();

    // 官方组：二选一，胜出副本决定展示用的来源名，整组水位一起推进。
    let official: Vec<&SourceEntry> = enabled
        .iter()
        .filter(|s| s.kind == SourceClass::Official && s.group_id() == OFFICIAL_GROUP)
        .collect();
    if !official.is_empty() {
        let previous = registry.watermark(OFFICIAL_GROUP);
        let mut candidates: Vec<(u32, Result<_, SourceError>)> = Vec::new();
        for source in &official {
            match ReqwestCatalogFetcher::new(source.host_suffixes.clone()) {
                Ok(fetcher) => {
                    let result = match source_adapter(source, &registry, &fetcher).fetch_catalog() {
                        Ok(Some(doc)) => Ok(doc),
                        Ok(None) => Err(SourceError::Unavailable {
                            source_id: source.id.clone(),
                            reason: "该源没有返回 catalog".into(),
                        }),
                        Err(error) => Err(error),
                    };
                    candidates.push((source.priority.unwrap_or(1), result));
                }
                Err(error) => candidates.push((
                    source.priority.unwrap_or(1),
                    Err(SourceError::Unavailable {
                        source_id: source.id.clone(),
                        reason: error,
                    }),
                )),
            }
        }
        match select_catalog_entry(&candidates, &previous, &now) {
            Ok((priority, doc)) => {
                let winner = official
                    .iter()
                    .find(|s| s.priority.unwrap_or(1) == priority)
                    .copied()
                    .unwrap_or(official[0]);
                collect_entries(&registry, winner, &doc, &mut cache);
                registry_next.advance(OFFICIAL_GROUP, doc.sequence, &doc.expires);
                observed.push(doc);
                for source in &official {
                    statuses.push(source_status(source, "ok", None));
                }
            }
            Err(error) => {
                for source in &official {
                    statuses.push(source_status(
                        source,
                        "invalid",
                        Some(error.reason().to_string()),
                    ));
                }
            }
        }
    }

    // 社区源：每个发布者各自验签、各自裁决，不用官方组的序号压自己。
    for source in enabled.iter().filter(|s| s.kind != SourceClass::Official) {
        let fetcher = match ReqwestCatalogFetcher::new(source.host_suffixes.clone()) {
            Ok(client) => client,
            Err(error) => {
                statuses.push(source_status(source, "unavailable", Some(error)));
                continue;
            }
        };
        let fetched = source_adapter(source, &registry, &fetcher).fetch_catalog();
        match fetched {
            Ok(Some(doc)) => match doc.validate(&registry.watermark(&source.id), &now) {
                Ok(()) => {
                    collect_entries(&registry, source, &doc, &mut cache);
                    registry_next.advance(&source.id, doc.sequence, &doc.expires);
                    observed.push(doc);
                    statuses.push(source_status(source, "ok", None));
                }
                Err(error) => statuses.push(source_status(
                    source,
                    "invalid",
                    Some(error.reason().to_string()),
                )),
            },
            Ok(None) => statuses.push(source_status(
                source,
                "invalid",
                Some("该源没有返回 catalog".into()),
            )),
            Err(error) => statuses.push(source_status(
                source,
                if matches!(error, SourceError::Unavailable { .. }) {
                    "unavailable"
                } else {
                    "invalid"
                },
                Some(error.reason().to_string()),
            )),
        }
    }

    let failed = !statuses.is_empty() && statuses.iter().all(|s| s.state != "ok");
    if !failed {
        let next = registry_next.clone();
        if let Err(error) = state.mutate_sources(|current| {
            *current = next;
            Ok(())
        }) {
            return unavailable(&allowlist, format!("保存源水位失败：{error}"));
        }
        if let Err(error) = state.mutate_installed(|installed| {
            for doc in &observed {
                installed
                    .catalog_state
                    .observe(doc)
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        }) {
            return unavailable(&allowlist, format!("保存版本水位失败：{error}"));
        }
        state.replace_cached_entries(cache.clone());
        let partial = statuses.iter().any(|s| s.state != "ok");
        return CatalogResponse {
            source: if partial { "partial" } else { "remote" }.into(),
            entries: catalog_entries(&state, &cache),
            sources: statuses,
            allowlist,
            reason: partial.then(|| "部分源不可用，只展示已验签来源的条目".into()),
        };
    }

    // 只允许回退本次会话内已验签过的条目；不读磁盘上的旧 catalog。
    let cached = state.cached_entries();
    if cached.is_empty() {
        unavailable(
            &allowlist,
            statuses
                .iter()
                .filter_map(|s| s.reason.as_ref().map(|r| format!("{}：{r}", s.name)))
                .collect::<Vec<_>>()
                .join("；"),
        )
    } else {
        CatalogResponse {
            source: "cache".into(),
            entries: catalog_entries(&state, &cached),
            sources: statuses,
            allowlist,
            reason: Some("在线源不可用，以下是本次会话内已验签的目录".into()),
        }
    }
}

/// 只读本次会话内已验签过的目录，不发任何网络请求。
///
/// 安装之后要刷新「已安装 / 可升级」这类状态。重新拉网络既慢又可能失败，
/// 一旦失败，界面会把刚装好的条目连同整张列表一起清掉——用户看到的是
/// 「刚装的东西不见了」。这里只读缓存：条目视图每次都按 `installed.json`
/// 现算，所以状态照样是最新的。
#[tauri::command]
pub fn catalog_cache(state: State<'_, AppStateV2>) -> CatalogResponse {
    let registry = state.sources();
    let allowlist = registry.allowlist();
    let statuses: Vec<SourceStatusView> = registry
        .sources
        .iter()
        .map(|s| {
            let state = if s.is_revoked() {
                "invalid"
            } else if s.enabled {
                "ok"
            } else {
                "disabled"
            };
            source_status(s, state, None)
        })
        .collect();
    let cached = state.cached_entries();
    if cached.is_empty() {
        return CatalogResponse {
            source: "unavailable".into(),
            entries: Vec::new(),
            sources: statuses,
            allowlist,
            reason: Some("本次会话还没有验签过任何目录".into()),
        };
    }
    CatalogResponse {
        source: "cache".into(),
        entries: catalog_entries(&state, &cached),
        sources: statuses,
        allowlist,
        reason: None,
    }
}

/// 缓存条目 → 界面行。安装目标由用户点选的那一行决定，不做隐式优选。
fn catalog_entries(
    state: &AppStateV2,
    cache: &BTreeMap<String, CachedEntry>,
) -> Vec<CatalogEntryView> {
    let installed = state.with_installed(|s| s.clone());
    let registry = state.sources();
    cache
        .values()
        .map(|cached| {
            let entry = &cached.entry;
            let same_source = installed.instances.values().find(|i| {
                i.module_id == entry.module_id
                    && i.variant == entry.variant
                    && (i.source_id.as_deref() == Some(cached.source_id.as_str())
                        || (cached.source_kind == SourceClass::Official && i.source_id.is_none()))
            });
            let (installed_state, note) = match entry_installability(
                cached,
                same_source.map(|i| i.version.as_str()),
                &installed.catalog_state,
            ) {
                Ok(kind) => (kind, None),
                Err(reason) => ("incompatible".into(), Some(reason)),
            };
            let revoked = cached
                .doc
                .revoked
                .iter()
                .any(|r| r.id == entry.version || r.id == entry.sha256)
                || entry
                    .revoked
                    .iter()
                    .any(|id| id == &entry.version || id == &entry.sha256);
            let download_url = entry
                .sources
                .iter()
                .find(|s| cached.group_ids.contains(&s.source_id))
                .map(|s| s.url.clone())
                .unwrap_or_default();
            CatalogEntryView {
                entry_key: cached.key.clone(),
                source_id: cached.source_id.clone(),
                source_name: cached.source_name.clone(),
                source_kind: cached.source_kind,
                catalog_fingerprint: cached.catalog_fingerprint.clone(),
                module_id: entry.module_id.clone(),
                variant: entry.variant.clone(),
                name: entry.name.clone(),
                version: entry.version.clone(),
                role: entry.role,
                sha256: entry.sha256.clone(),
                size: entry.size,
                download_url,
                signature_required: entry.signature_required,
                publisher_fp: entry.publisher_fp.clone(),
                min_launcher: entry.min_launcher.clone(),
                min_version: entry.min_version.clone(),
                expires: entry.expires.clone(),
                revoked,
                installed_state,
                auto_update: cached.auto_update,
                source_revoked: registry
                    .find(&cached.source_id)
                    .is_some_and(SourceEntry::is_revoked),
                note,
            }
        })
        .collect()
}

fn unavailable(allowlist: &[String], reason: String) -> CatalogResponse {
    CatalogResponse {
        source: "unavailable".into(),
        entries: Vec::new(),
        sources: Vec::new(),
        allowlist: allowlist.to_vec(),
        reason: Some(reason),
    }
}

/// 源与公钥的只读快照。界面据此渲染「下载」页的来源列表。
#[tauri::command]
pub fn list_sources(state: State<'_, AppStateV2>) -> SourcesView {
    sources_view(&state.sources())
}

/// 添加源第一步：算完整指纹、派生白名单。此命令不写任何文件。
#[tauri::command]
pub fn inspect_source(request: InspectSourceRequest) -> Result<SourcePreview, String> {
    let base_url = request.base_url.trim().trim_end_matches('/').to_string();
    let pubkey = request.catalog_pubkey.trim().to_string();
    let fingerprint = launcher_core::m2::fingerprint(&pubkey).map_err(|e| e.to_string())?;
    let (host_suffixes, derived) = match request.host_suffixes {
        Some(list) if !list.is_empty() => (list, false),
        _ => (derive_host_suffixes(&base_url).map_err(|e| e.to_string())?, true),
    };
    for suffix in &host_suffixes {
        validate_suffix(suffix)?;
    }
    let host = launcher_core::m2::source::url_host(&base_url)
        .ok_or_else(|| "源地址必须是 https".to_string())?;
    let suffixes: Vec<&str> = host_suffixes.iter().map(String::as_str).collect();
    if !launcher_core::m2::source::host_allowed(host, &suffixes) {
        return Err(format!("源地址主机 {host} 不在你确认的白名单内"));
    }
    let id = host
        .split('.')
        .rev()
        .nth(1)
        .unwrap_or(host)
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_lowercase() || c.is_ascii_digit() { c } else { '-' })
        .collect::<String>();
    Ok(SourcePreview {
        name: host.to_string(),
        id,
        base_url,
        catalog_pubkey: pubkey,
        catalog_fingerprint: fingerprint,
        host_suffixes,
        derived,
        note: "下一步会展示完整指纹并请你确认；确认前不会写入 sources.json，也不会写 trust.json。".into(),
    })
}

/// 添加源第二步。只有 confirm=true 才落盘，且只写 sources.json。
#[tauri::command]
pub fn add_source(
    state: State<'_, AppStateV2>,
    request: AddSourceRequest,
) -> Result<SourcesView, String> {
    if !request.confirm {
        return Err("添加源必须先看完整指纹并显式确认；未写入任何文件".into());
    }
    if !valid_id(&request.id) {
        return Err("源 id 只能用 1-32 位小写字母、数字与连字符".into());
    }
    let base_url = request.base_url.trim().trim_end_matches('/').to_string();
    let pubkey = request.catalog_pubkey.trim().to_string();
    let fingerprint = launcher_core::m2::fingerprint(&pubkey).map_err(|e| e.to_string())?;
    for suffix in &request.host_suffixes {
        validate_suffix(suffix)?;
    }
    let entry = SourceEntry {
        id: request.id,
        name: request.name,
        kind: SourceClass::Community,
        base_url,
        catalog_pubkey: pubkey,
        catalog_fingerprint: fingerprint,
        host_suffixes: request.host_suffixes,
        enabled: true,
        deletable: true,
        // 拍板 3：社区源只提示有新版，不自动装。
        auto_update: false,
        group: None,
        sequence: None,
        expires: None,
        priority: Some(10),
        added_at: Some(AppStateV2::now()),
        revoked_at: None,
        note: None,
    };
    state.mutate_sources(|registry| {
        registry.add(entry).map_err(|e| e.to_string())?;
        Ok(())
    })?;
    Ok(sources_view(&state.sources()))
}

/// 启用 / 禁用源。内置官方源可禁用不可删除；已撤销的源不能重新启用。
#[tauri::command]
pub fn set_source_enabled(
    state: State<'_, AppStateV2>,
    request: SourceIdRequest,
) -> Result<SourcesView, String> {
    let enabled = request
        .enabled
        .ok_or_else(|| "缺少 enabled 参数".to_string())?;
    state.mutate_sources(|registry| {
        registry
            .set_enabled(&request.id, enabled)
            .map_err(|e| e.to_string())?;
        Ok(())
    })?;
    Ok(sources_view(&state.sources()))
}

/// 撤销 catalog 公钥：停用该源，已装实例不卸载、不自动更新。
/// 全程不写 trust.json——包的四级判定根本读不到这把钥匙。
#[tauri::command]
pub fn revoke_source(
    state: State<'_, AppStateV2>,
    request: SourceIdRequest,
) -> Result<RevokeSourceResult, String> {
    let affected = state.with_installed(|installed| {
        installed
            .instances
            .values()
            .filter(|i| i.source_id.as_deref() == Some(request.id.as_str()))
            .map(|i| i.instance_id.clone())
            .collect::<Vec<_>>()
    });
    state.mutate_sources(|registry| {
        registry
            .revoke(&request.id, &AppStateV2::now())
            .map_err(|e| e.to_string())?;
        Ok(())
    })?;
    // 撤销后该源的条目不能继续出现在可安装列表里。
    let mut cache = state.cached_entries();
    cache.retain(|_, cached| cached.source_id != request.id);
    state.replace_cached_entries(cache);
    Ok(RevokeSourceResult {
        sources: sources_view(&state.sources()),
        affected,
    })
}

/// 删除社区源。内置源一律拒绝。
#[tauri::command]
pub fn remove_source(
    state: State<'_, AppStateV2>,
    request: SourceIdRequest,
) -> Result<SourcesView, String> {
    state.mutate_sources(|registry| {
        registry.remove(&request.id).map_err(|e| e.to_string())?;
        Ok(())
    })?;
    let mut cache = state.cached_entries();
    cache.retain(|_, cached| cached.source_id != request.id);
    state.replace_cached_entries(cache);
    Ok(sources_view(&state.sources()))
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
        source_id: None,
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
    // 用户亲手选的目录本身就是一次显式指名，所以 dir 实例（恒为 C 类）
    // 也能在槽空时直接上位——否则开发中的模块只能跑备用端口，
    // 组装层永远连不上。
    let _ = fill_vacant_active(state, instance.role, Some(&instance.instance_id), true);
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
    // 卸掉的若是原本的 active，槽位会空出来，剩下同 role 的实例只跑备用端口，
    // 组装层就再也看不到这个角色了。补一个上位（C 类仍要用户显式切换）。
    // 已经在跑的实例保持原端口，下次启动才落到规范端口。
    let _ = fill_vacant_active(&state, instance.role, None, false);
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
    build_admin_window(&app, &url, &request.module_id)
}

/// 独立窗口：既是「在独立窗口打开」按钮的落点，也是内嵌失败的兜底。
///
/// 与 v1 相同，Tauri 2 在 Windows 上必须把建窗口挪出命令线程，否则 Webview2
/// 的 COM 初始化会死锁在 about:blank。注入清理脚本与内嵌页共用一份，避免
/// 两条路径的清理规则分叉。capability 按 webview label 授权，这个窗口的
/// label 不是 main，因此拿不到任何命令权限；清注入仍是必要的纵深防御。
fn build_admin_window(
    app: &tauri::AppHandle,
    url: &str,
    module_id: &str,
) -> Result<String, String> {
    let module_id = module_id.to_string();
    let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
    let app_for_thread = app.clone();
    let url_for_thread = url.to_string();
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
            .initialization_script_for_all_frames(crate::admin_embed::ADMIN_SANITIZE_SCRIPT)
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

/* ------------------------------------------------------------------ */
/* 同窗口内嵌管理页                                                    */
/* ------------------------------------------------------------------ */

/// 子 WebView 的位置与尺寸，单位是相对主窗口客户区的逻辑像素，由 UI 一处计算。
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct AdminBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Deserialize)]
pub struct AdminHostRequest {
    pub label: String,
}

#[derive(Debug, Serialize)]
pub struct AdminMountResult {
    /// embedded = 内嵌成功；window = 内嵌失败但已退化为独立窗口。
    pub mode: String,
    pub label: String,
    pub url: String,
    /// 降级原因；成功时为 null。
    pub reason: Option<String>,
}

fn checked_label(label: &str) -> Result<&str, String> {
    if !label.starts_with(crate::admin_embed::LABEL_PREFIX) {
        return Err("非法的管理页 label".into());
    }
    Ok(label)
}

fn bounds_of(b: AdminBounds) -> Bounds {
    Bounds {
        x: b.x,
        y: b.y,
        width: b.width,
        height: b.height,
    }
}

/// manifest 的 `admin_ui.web_ui.embed` 默认 true；声明 false 的模块要求独立窗口。
///
/// 读不到 manifest 时按默认处理：`admin_url_v2` 已经先读过一次并失败就返回了，
/// 这里不会掩盖错误，只是不再为同一个字段再报错一次。
fn admin_embed_preferred(state: &AppStateV2, module_id: &str) -> bool {
    let Ok(instance) = state.resolve_instance(module_id) else {
        return true;
    };
    let Ok(raw) = read_manifest_value(&state.module_dir(&instance)) else {
        return true;
    };
    raw.pointer("/admin_ui/web_ui/embed")
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

/// 在启动器窗口内打开管理页。URL 只来自 `admin_url_v2`，不接受前端传地址。
///
/// 创建失败时自动退化为独立窗口，不丢状态：模块服务还在跑，URL 也一样，
/// 只是展示形态变了。前端据 `mode` 显示占位或「已在独立窗口打开」。
#[tauri::command]
pub async fn open_admin_embedded(
    app: tauri::AppHandle,
    state: State<'_, AppStateV2>,
    supervisor: State<'_, Supervisor>,
    request: OpenAdminUiRequest,
    bounds: AdminBounds,
) -> Result<AdminMountResult, String> {
    let url = admin_url_v2(&state, &supervisor, &request.module_id)?;
    let instance = state.resolve_instance(&request.module_id)?;
    let label = crate::admin_embed::derive_label(&instance.instance_id);
    if !admin_embed_preferred(&state, &request.module_id) {
        // 模块自己要求独立窗口：这是声明，不是内嵌失败，文案要区分开。
        build_admin_window(&app, &url, &request.module_id)?;
        return Ok(AdminMountResult {
            mode: "window".into(),
            label,
            url,
            reason: Some("模块声明 web_ui.embed = false，按它要求开了独立窗口".into()),
        });
    }
    let host = crate::admin_embed::TauriAdminHost::new(app.clone());
    let mount = MountRequest {
        label: label.clone(),
        url: url.clone(),
        bounds: bounds_of(bounds),
    };
    match host.mount(&mount) {
        Ok(()) => Ok(AdminMountResult {
            mode: "embedded".into(),
            label,
            url,
            reason: None,
        }),
        Err(error) => {
            let reason = format!("内嵌失败：{error}");
            match build_admin_window(&app, &url, &request.module_id) {
                Ok(_) => Ok(AdminMountResult {
                    mode: "window".into(),
                    label,
                    url,
                    reason: Some(reason),
                }),
                Err(fallback) => Err(format!("{reason}；独立窗口也打不开：{fallback}")),
            }
        }
    }
}

/// 窗口缩放或容器改尺寸时重下发 bounds。
#[tauri::command]
pub async fn admin_host_resize(
    app: tauri::AppHandle,
    request: AdminHostRequest,
    bounds: AdminBounds,
) -> Result<(), String> {
    let label = checked_label(&request.label)?;
    crate::admin_embed::TauriAdminHost::new(app)
        .resize(label, bounds_of(bounds))
        .map_err(|e| e.to_string())
}

/// health 变 200 后刷新内嵌页，让它重新拉一次自己的状态。
#[tauri::command]
pub async fn admin_host_reload(
    app: tauri::AppHandle,
    request: AdminHostRequest,
) -> Result<(), String> {
    let label = checked_label(&request.label)?;
    crate::admin_embed::TauriAdminHost::new(app)
        .reload(label)
        .map_err(|e| e.to_string())
}

/// 卸载实例时销毁子 WebView；label 由 instance_id 派生，复用时不会撞。
#[tauri::command]
pub async fn admin_host_unmount(
    app: tauri::AppHandle,
    request: AdminHostRequest,
) -> Result<(), String> {
    let label = checked_label(&request.label)?;
    crate::admin_embed::TauriAdminHost::new(app)
        .unmount(label)
        .map_err(|e| e.to_string())
}

/// 切到别的视图时隐藏而不是销毁，切回来再显示，页面里的输入不丢。
#[tauri::command]
pub async fn admin_host_visible(
    app: tauri::AppHandle,
    request: AdminHostRequest,
    visible: bool,
) -> Result<(), String> {
    let label = checked_label(&request.label)?;
    crate::admin_embed::TauriAdminHost::new(app)
        .set_visible(label, visible)
        .map_err(|e| e.to_string())
}

/// 下载目录中缓存包的文件名。带 source_id 前缀，跨源同名不会互相覆盖。
fn download_file_name(cached: &CachedEntry) -> String {
    format!(
        "{}@{}_{}.zip",
        cached.source_id,
        cached.entry.module_id.replace('/', "_"),
        cached.entry.variant
    )
}

/// 在线安装：从本次会话验签过的目录里按 entry_key 精确定位一行。
///
/// 为什么不给前端「先下载再安装」两步：两步之间包可能被换掉，而安装侧的
/// `expected_sha256` 是前端从列表里读的字符串，等于把一个信任决定交给界面。
/// 这里目录条目、下载校验、安装校验三处用的是**同一个** sha256 与 size；
/// 下载地址只认该源组，白名单只认该源自己的后缀。
fn install_from_catalog_impl(
    state: &AppStateV2,
    entry_key: &str,
    confirm: bool,
) -> Result<InstallResponse, String> {
    let cached = state
        .cached_entry(entry_key)
        .ok_or_else(|| "模块目录还没刷新，或该条目已失效，请重新刷新目录".to_string())?;
    let registry = state.sources();
    let source = registry
        .find(&cached.source_id)
        .ok_or_else(|| format!("源 {} 已不在注册表", cached.source_id))?;
    if !source.enabled || source.is_revoked() {
        return Err(format!(
            "源 {} 已停用或公钥已撤销，已装实例保留但不自动更新",
            cached.source_id
        ));
    }

    let installed = state.with_installed(|s| s.clone());
    let same_source = installed.instances.values().find(|i| {
        i.module_id == cached.entry.module_id
            && i.variant == cached.entry.variant
            && i.source_id.as_deref() == Some(cached.source_id.as_str())
    });
    match entry_installability(
        &cached,
        same_source.map(|i| i.version.as_str()),
        &installed.catalog_state,
    ) {
        Ok(kind) if kind == "downgrade" => {
            return Err(format!(
                "拒绝安装 {} 的旧版本 {}：版本回退或该版本已被撤销",
                cached.entry.module_id, cached.entry.version
            ));
        }
        Ok(kind) if kind == "incompatible" => {
            return Err(format!(
                "{} 需要更高版本的启动器（min_launcher {}）",
                cached.entry.module_id,
                cached.entry.min_launcher.as_deref().unwrap_or("未知")
            ));
        }
        Err(reason) => return Err(reason),
        _ => {}
    }

    let suffixes: Vec<&str> = cached.host_suffixes.iter().map(String::as_str).collect();
    let sources: Vec<String> = cached
        .entry
        .sources
        .iter()
        .filter(|s| cached.group_ids.contains(&s.source_id))
        .map(|s| s.url.clone())
        .collect();
    if sources.is_empty() {
        return Err(format!(
            "{} 没有属于源 {} 的下载地址",
            cached.entry.module_id, cached.source_id
        ));
    }
    for source_url in &sources {
        validate_download_url(source_url, &suffixes)
            .map_err(|e| format!("下载源 {source_url} 被拒绝：{e}"))?;
    }
    let file_name = download_file_name(&cached);
    if !launcher_core::security::is_safe_filename(&file_name) {
        return Err(format!("非法的下载文件名：{file_name}"));
    }
    let dest = state.paths.data_root.join("downloads").join(&file_name);
    let client = crate::http_client::ReqwestClient::default_client(cached.host_suffixes.clone())?;
    let options = crate::download::DownloadOptions {
        retries: 2,
        backoff_ms: 150,
        expected_sha256: Some(bare_sha256(&cached.entry.sha256)),
    };
    let report = crate::download::download(&client, &sources, &dest, &options)
        .map_err(|e| format!("下载 {} 失败：{e}", cached.entry.module_id))?;
    let bytes = std::fs::read(&report.path).map_err(|e| format!("读取下载文件失败：{e}"))?;
    if bytes.len() as u64 != cached.entry.size {
        return Err(format!(
            "{} 包大小与目录声明不一致：目录 {} 字节，实际 {} 字节",
            cached.entry.module_id,
            cached.entry.size,
            bytes.len()
        ));
    }
    let install_source = InstallSource {
        source_id: cached.source_id.clone(),
        source_kind: cached.source_kind,
        auto_update: cached.auto_update,
    };
    commit_install(
        state,
        &bytes,
        &cached.entry.sha256,
        Some(&cached.entry.module_id),
        Some(&cached.entry.version),
        confirm,
        Some(CatalogContext {
            source: Some(install_source),
            entry: Some(cached.entry.clone()),
        }),
    )
}

/// 在线安装入口。参数与 `install_local_package` 保持同一返回形状。
#[tauri::command]
pub async fn install_from_catalog(
    entry_key: String,
    confirm: bool,
    app: tauri::AppHandle,
) -> Result<InstallResponse, String> {
    // 下载会阻塞几秒到几十秒，放主线程会冻住窗口。
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppStateV2>();
        install_from_catalog_impl(&state, &entry_key, confirm)
    })
    .await
    .map_err(|e| format!("安装任务失败：{e}"))?
}

/// 设置页一次读完：信任表、保留策略与两枚内置公钥的指纹。
///
/// 指纹全部用编译期内嵌值，界面不自行拼装字符串——指纹是必须一眼看出来
/// 的东西，不能出现「界面显示 A、判定用 B」。catalog 公钥改由「下载」页
/// 的源列表展示，每个源各有自己的指纹。
#[derive(Debug, Serialize)]
pub struct SettingsSnapshot {
    pub trust: launcher_core::m2::trust::TrustStore,
    pub retention: launcher_core::m2::retention::RetentionPolicy,
    pub k1_fingerprint: String,
    pub k5_fingerprint: String,
    /// 启动时是否由启动器补写/校正过 `install.json`；`None` 表示本来就一致。
    pub install_note: Option<String>,
    /// 新拉起的实例是否捕获子进程 stdout/stderr。
    pub child_output: bool,
}

#[tauri::command]
pub fn settings_snapshot(state: State<'_, AppStateV2>) -> SettingsSnapshot {
    let trust = state.with_trust(|store| store.clone());
    let retention = state.with_retention(|policy| policy.clone());
    let k5_fingerprint = crate::state_v2::BUILTIN_K5_FINGERPRINT.to_string();
    SettingsSnapshot {
        trust,
        retention,
        k1_fingerprint: crate::state_v2::BUILTIN_K1_FINGERPRINT.to_string(),
        k5_fingerprint,
        install_note: state.install_note.clone(),
        child_output: state.child_output(),
    }
}

/// 打开/关闭子进程输出捕获。只影响之后拉起的实例，已在跑的不动——
/// 改管道意味着重启进程，而那会打断用户正在用的模块。
#[tauri::command]
pub fn set_child_output(enabled: bool, state: State<'_, AppStateV2>) -> Result<bool, String> {
    state.set_child_output(enabled)
}

/* ------------------------------------------------------------------ */
/* 存储与引导状态                                                      */
/* ------------------------------------------------------------------ */

/// 存储事实：数据根在哪、已经下过什么、占了多少。
///
/// 这些数只能后端给：前端没有数据根路径，也不该自己算「解释器装好没有」。
/// 报解压后的字节数而不是下载量——用户关心的是磁盘要占多少，下载量还得
/// 估压缩比，估不准还不如不给。
#[derive(Debug, Serialize)]
pub struct StorageFacts {
    pub data_root: String,
    /// 数据根里已登记的实例数；0 表示这台机器还没装过模块。
    pub instances: usize,
    /// 解释器已占字节；0 表示还没下过 Python。
    pub python_bytes: u64,
    /// uv 缓存已占字节；复用后第二个模块通常只下增量。
    pub cache_bytes: u64,
    /// 数据根整体占用（含实例目录、日志、venv）。遍历一次要几十毫秒，
    /// 只在存储分组里要，所以不放进每次刷新都会读的快照。
    pub data_bytes: u64,
}

#[tauri::command]
pub fn storage_facts(state: State<'_, AppStateV2>) -> StorageFacts {
    let usage = launcher_core::runtime_usage(&state.paths).unwrap_or(launcher_core::RuntimeUsage {
        python_bytes: 0,
        cache_bytes: 0,
    });
    StorageFacts {
        data_root: state.paths.data_root.display().to_string(),
        instances: state.with_installed(|installed| installed.instances.len()),
        python_bytes: usage.python_bytes,
        cache_bytes: usage.cache_bytes,
        data_bytes: launcher_core::dir_usage(&state.paths.data_root).unwrap_or(0),
    }
}

#[tauri::command]
pub fn onboarding_state(state: State<'_, AppStateV2>) -> crate::state_v2::Onboarding {
    state.onboarding()
}

#[tauri::command]
pub fn complete_onboarding(
    skipped: bool,
    state: State<'_, AppStateV2>,
) -> Result<crate::state_v2::Onboarding, String> {
    state.complete_onboarding(skipped)
}

#[tauri::command]
pub fn reset_onboarding(
    state: State<'_, AppStateV2>,
) -> Result<crate::state_v2::Onboarding, String> {
    state.reset_onboarding()
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
        state.replace_cached_entries(BTreeMap::new());
    }

    if request.state {
        // installed.json 的实例表是唯一真源，不随「清状态」一起删：
        // 删掉会留下孤儿包目录，比清缓存更危险。这里只重置 catalog 水位，
        // 语义差异写进 M3.5 文档，不塞进 errors 让 UI 误报成故障。
        match state.mutate_installed(|s| {
            s.catalog_state = launcher_core::m2::catalog::CatalogState::default();
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

/// 读日志尾部。`file` 给定时只读那一份历史文件，否则跨文件读最新内容。
#[tauri::command]
pub fn read_instance_logs(
    instance_id: String,
    file: Option<String>,
    state: State<'_, AppStateV2>,
) -> InstanceLogs {
    let dir = state.paths.instance_logs_dir_keyed(&instance_id);
    let total = crate::diag_log::list_logs(&dir).len();
    let tail = match file.as_deref() {
        Some(name) => crate::diag_log::read_file_tail(&dir, name, crate::diag_log::MAX_TAIL_LINES)
            .unwrap_or_else(|| crate::diag_log::LogTail {
                lines: Vec::new(),
                files: 0,
                truncated: false,
            }),
        None => crate::diag_log::read_tail(&dir, crate::diag_log::MAX_TAIL_LINES),
    };
    let note = if total == 0 {
        "该实例还没有日志：启动过一次才会写入。启动器事件始终记录，子进程输出需显式打开诊断开关。"
            .to_string()
    } else if file.is_some() && tail.lines.is_empty() {
        "这份日志已经不在了：轮转只保留最近几份，切回「最新」看当前内容。".to_string()
    } else if tail.truncated {
        format!(
            "显示最近 {} 行，更早的内容已截断。保留 {} 个文件，每个超过 2 MiB 时轮转。",
            tail.lines.len(),
            total
        )
    } else {
        format!(
            "共 {} 个文件。启动器事件始终记录，子进程输出需显式打开诊断开关。",
            total
        )
    };
    InstanceLogs {
        instance_id,
        lines: tail.lines,
        files: total,
        truncated: tail.truncated,
        note,
        file,
        child_output: crate::diag_log::child_output_enabled(),
        dir: dir.display().to_string(),
    }
}

/// 列出该实例保留的日志文件，从新到旧。
#[tauri::command]
pub fn list_instance_logs(instance_id: String, state: State<'_, AppStateV2>) -> Vec<LogFileInfo> {
    let dir = state.paths.instance_logs_dir_keyed(&instance_id);
    let files = crate::diag_log::list_logs(&dir);
    let newest = files.last().map(|f| f.path.clone());
    files
        .into_iter()
        .rev()
        .map(|file| LogFileInfo {
            name: file
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            size: file.size,
            modified_at: file.modified_at,
            current: newest.as_deref() == Some(file.path.as_path()),
        })
        .collect()
}

/// 在文件管理器里打开该实例的日志目录。
///
/// 路径由后端按 instance_id 自己拼，前端传不进来任意路径——否则这个命令就成了
/// 「打开任意目录」的通用入口。
#[tauri::command]
pub fn open_instance_logs_dir(
    instance_id: String,
    state: State<'_, AppStateV2>,
) -> Result<String, String> {
    let dir = state.paths.instance_logs_dir_keyed(&instance_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("无法创建日志目录：{e}"))?;
    #[cfg(windows)]
    let spawned = std::process::Command::new("explorer").arg(&dir).spawn();
    #[cfg(not(windows))]
    let spawned = std::process::Command::new("xdg-open").arg(&dir).spawn();
    spawned.map_err(|e| format!("无法调起文件管理器：{e}"))?;
    Ok(dir.display().to_string())
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

        let prepared: BTreeSet<String> = ["omninexus@stable", "omninexus@local"]
            .iter()
            .map(|id| id.to_string())
            .collect();
        let env_facts = EnvFacts {
            prepared,
            pinned: std::collections::BTreeMap::new(),
        };
        let views = compose_instance_views(
            vec![active_instance, standby],
            &ports,
            &active,
            &disabled,
            &snapshots,
            &env_facts,
            &std::collections::BTreeMap::new(),
        );
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].port, Some(8102));
        assert!(views[0].active);
        // 依赖就绪标记由调用方算好传进来，视图只负责落位。
        assert!(views[0].prepared);
        assert!(views[1].prepared);
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
        // 上游仓库普遍带点开头的开发配置；打包器收它们，安装侧也必须收。
        let dot = b".venv\n__pycache__/\n";
        let dot_sha = format!("sha256:{}", hex::encode(Sha256::digest(dot)));
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
            "files": { "pyproject.toml": sha, ".gitignore": dot_sha },
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
            writer.start_file(".gitignore", options).unwrap();
            writer.write_all(dot).unwrap();
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
        assert_eq!(verified.files.len(), 2);
        assert!(verified.files.iter().any(|f| f.path == ".gitignore"));

        let sha = verified.sha256.clone();
        let err = commit_install(&state, &bytes, &sha, None, None, false, None).unwrap_err();
        assert!(err.contains("需要用户显式确认"), "{err}");
        assert!(state.instance("dounai/demo-local@stable").is_none());

        let response = commit_install(&state, &bytes, &sha, None, None, true, None).unwrap();
        assert_eq!(response.module_id, "dounai/demo-local");
        assert_eq!(response.files, 2);
        assert!(!response.replaced);
        assert!(state.instance("dounai/demo-local@stable").is_some());

        // 装第二次会覆盖：包目录已存在，返回 replaced。
        let again = commit_install(&state, &bytes, &sha, None, None, true, None).unwrap();
        assert!(again.replaced);
    }

    #[test]
    fn local_package_hash_mismatch_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppStateV2::new(dir.path().join("app"), dir.path().join("data")).unwrap();
        let bytes = unsigned_package();
        // 检查后文件被换掉：调用方带着旧哈希来装，必须失败而不是装进去。
        let bogus = format!("sha256:{}", "0".repeat(64));
        let err = commit_install(&state, &bytes, &bogus, None, None, true, None).unwrap_err();
        assert!(err.contains("整包哈希不符"), "{err}");
        assert!(state.instance("dounai/demo-local@stable").is_none());
    }

    /// 界面按 instance_id 调启动/停止，v1 命令面按 module_id 调；两者都得认。
    #[test]
    fn resolve_instance_accepts_instance_id_and_module_id() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppStateV2::new(dir.path().join("app"), dir.path().join("data")).unwrap();
        let instance = |id: &str| -> Instance {
            serde_json::from_value(serde_json::json!({
                "instance_id": id,
                "module_id": "dounai/omninexus",
                "variant": "stable",
                "version": "0.3.4",
                "role": "llm",
                "category": "core",
                "verdict": "A",
                "confirmed_at": "2026-11-13T00:00:00Z",
                "installed_at": "2026-11-13T00:00:00Z",
                "source_kind": "zip",
                "pinned": false,
                "auto_update": false,
            }))
            .unwrap()
        };
        state.upsert(instance("dounai/omninexus@stable")).unwrap();

        // 界面路径：精确 instance_id。
        assert_eq!(
            state
                .resolve_instance("dounai/omninexus@stable")
                .unwrap()
                .module_id,
            "dounai/omninexus"
        );
        // v1 路径：module_id 落到唯一实例。
        assert_eq!(
            state
                .resolve_instance("dounai/omninexus")
                .unwrap()
                .instance_id,
            "dounai/omninexus@stable"
        );
        // 都不匹配时给的是「没找到」而不是「未安装某个模块」。
        let err = state.resolve_instance("dounai/nope@stable").unwrap_err();
        assert!(err.contains("没有找到实例或模块"), "{err}");
    }
}
