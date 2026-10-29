//! Tauri 命令层：把启动器能力暴露给前端。
//!
//! 安全要点：
//! - 前端**不能**传任意 URL/路径进来执行；所有路径都以 `app_data_dir` 为根拼接；
//! - 每个命令的错误都返回结构化信息，前端据此展示，不吞异常；
//! - 危险操作（卸载 core、启用 reserved）在状态层已拒绝，这里不再重复判断，
//!   避免两处规则不一致。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::catalog::{fetch_catalog, Allowlist, CatalogEntry, CatalogResult};
use crate::install::{extract_zip_into, InstallTransaction};
use crate::runtime::ProcessManager;
use crate::state::{Category, InstallState, ModuleState, RunState, StateStore};
use crate::trust::{TrustPolicy, TrustVerdict};
use crate::{sha256_hex, verify_sha256};

/// 应用共享状态。
pub struct AppState {
    /// 数据根目录（所有文件操作的唯一根）
    data_root: PathBuf,
    /// 模块状态库
    store: Mutex<StateStore>,
    /// 出网白名单（发布时随版本确定）
    allowlist: Allowlist,
    /// manifest 信任策略（决定未签名模块能否安装）
    trust: TrustPolicy,
    /// 进程表：管理由启动器拉起的子进程（内存态，不持久化）
    processes: ProcessManager,
}

impl AppState {
    /// 构造时使用**最保守**的信任策略（无公钥、拒绝未签名）。
    ///
    /// 调用方应显式选择策略：开发用 `with_trust(TrustPolicy::permissive())`，
    /// 发布用 `TrustPolicy::strict(keys)`。默认不安全地放行是绝不可接受的。
    pub fn new(data_root: PathBuf, allowlist: Allowlist) -> Result<Self, String> {
        Self::with_trust(data_root, allowlist, TrustPolicy::default())
    }

    pub fn with_trust(
        data_root: PathBuf,
        allowlist: Allowlist,
        trust: TrustPolicy,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(&data_root).map_err(|e| e.to_string())?;
        let state_path = data_root.join("modules-state.json");
        let store = StateStore::open(&state_path).map_err(|e| e.to_string())?;
        Ok(Self {
            data_root,
            store: Mutex::new(store),
            allowlist,
            trust,
            processes: ProcessManager::default(),
        })
    }

    pub fn trust_policy(&self) -> &TrustPolicy {
        &self.trust
    }

    /// 进程表引用。
    pub fn processes(&self) -> &ProcessManager {
        &self.processes
    }

    /// 模块根目录。
    ///
    /// 受管模块在 `{data_root}/modules/{module_id}`；
    /// **外部链接模块**（用户自己 clone / 下载的目录）用登记时记录的路径。
    /// 两种情况下后续的 `working_dir` 校验都把该根当作边界，
    /// 因此链接模块并不会让进程跑到根之外。
    pub fn module_root(&self, module_id: &str) -> Result<PathBuf, String> {
        if let Ok(store) = self.store.lock() {
            if let Some(st) = store.get(module_id) {
                if st.linked && !st.path.is_empty() {
                    return Ok(PathBuf::from(&st.path));
                }
            }
        }
        self.module_dir(module_id)
    }

    /// 模块是否为外部链接（文件由用户管理）。
    pub fn is_linked(&self, module_id: &str) -> bool {
        self.store
            .lock()
            .ok()
            .and_then(|s| s.get(module_id).map(|st| st.linked))
            .unwrap_or(false)
    }

    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    /// 读取已安装模块的 manifest 并组装成可执行的入口规格。
    ///
    /// 这是「只执行 manifest 声明的入口」的落点：命令与工作目录
    /// 全部来自安装时已验签的文件，不接受前端传入。
    fn entry_spec(&self, module_id: &str) -> Result<crate::runtime::EntrySpec, String> {
        let root = self.module_root(module_id)?;

        // manifest 随包安装时落在模块根下；找不到就说明这个包没带 manifest
        let manifest_path = ["manifest.json", "dounai-manifest.json"]
            .iter()
            .map(|n| root.join(n))
            .find(|p| p.exists())
            .ok_or_else(|| format!("模块 {module_id} 缺少 manifest.json，无法解析启动入口"))?;

        let text = std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("读取 manifest 失败：{e}"))?;
        let m: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("manifest 不是合法 JSON：{e}"))?;

        let entry = m
            .get("entry")
            .ok_or_else(|| "manifest 缺少 entry 段".to_string())?;
        let entry_type = entry
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("command");
        let command = entry.get("command").and_then(|v| v.as_str()).unwrap_or("");
        let working_raw = entry
            .get("working_dir")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let data_dir = self.data_root.join("data").join(module_id);
        let app_local = self.data_root.clone();

        let command = crate::runtime::resolve_placeholders(command, &root, &data_dir, &app_local);
        let working =
            crate::runtime::resolve_placeholders(working_raw, &root, &data_dir, &app_local);
        let working_dir = crate::runtime::check_working_dir(&root, &working)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .to_string();

        // 环境变量同样替换占位符
        let mut env = Vec::new();
        if let Some(obj) = entry.get("env").and_then(|v| v.as_object()) {
            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    env.push((
                        k.clone(),
                        crate::runtime::resolve_placeholders(s, &root, &data_dir, &app_local),
                    ));
                }
            }
        }

        let health = entry.get("health");
        let health_url = health
            .and_then(|h| h.get("url"))
            .and_then(|v| v.as_str())
            .map(|s| crate::runtime::resolve_placeholders(s, &root, &data_dir, &app_local));
        let health_timeout_ms = health
            .and_then(|h| h.get("timeout_ms"))
            .and_then(|v| v.as_u64())
            .unwrap_or(3000);

        Ok(crate::runtime::EntrySpec {
            module_id: module_id.to_string(),
            entry_type: entry_type.to_string(),
            command,
            working_dir,
            env,
            health_url,
            health_timeout_ms,
        })
    }

    /// 启动前的状态校验（未安装/已停用/入口不可执行）。
    fn precheck_start(&self, module_id: &str) -> Result<crate::runtime::EntrySpec, String> {
        let category = {
            let store = self.store.lock().map_err(|_| "状态锁不可用")?;
            let st = store
                .get(module_id)
                .ok_or_else(|| format!("模块 {module_id} 未安装，无法启动"))?;
            if st.install != InstallState::Installed {
                return Err(format!("模块 {module_id} 未安装完成，无法启动"));
            }
            if !st.enabled {
                return Err(format!("模块 {module_id} 已停用，请先启用再启动"));
            }
            st.path.is_empty()
        };
        let _ = category;

        let spec = self.entry_spec(module_id)?;
        if spec.entry_type != "command" {
            return Err(format!(
                "模块 {module_id} 的入口类型是 {}，不支持作为进程启动",
                spec.entry_type
            ));
        }
        Ok(spec)
    }

    /// 把模块 id 解析为安装目录，并确保结果仍在 data_root 内。
    ///
    /// 这一步是防「前端传入 `../../` 让安装写到任意位置」的关键。
    fn module_dir(&self, module_id: &str) -> Result<PathBuf, String> {
        // 模块 id 只允许安全字符，从根本上排除路径穿越
        if module_id.is_empty()
            || !module_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
            || module_id.contains("..")
        {
            return Err(format!("非法的模块 id：{module_id}"));
        }
        // 仅由字母组成也会踩 Windows 保留设备名（con/nul/aux/com1…），
        // 而目录名一旦是设备名，创建会失败或行为异常。复用 security 层的判定。
        if !crate::security::is_safe_filename(module_id) {
            return Err(format!("模块 id 使用了保留设备名：{module_id}"));
        }
        Ok(self.data_root.join("modules").join(module_id))
    }

    /// 供测试使用：验证模块 id 到路径的映射规则。
    pub fn module_dir_for_test(&self, module_id: &str) -> Result<PathBuf, String> {
        self.module_dir(module_id)
    }

    /// 供测试使用：访问状态库。
    pub fn store_for_test(&self) -> &Mutex<StateStore> {
        &self.store
    }

    /// 供测试使用：直接改状态库并落盘（模拟用户在 UI 上操作后的状态）。
    ///
    /// 注意：`AppState` 构造时把状态文件读进内存，之后再改磁盘文件是**看不到**的，
    /// 因此测试必须走这个入口，否则会出现"文件改了但行为没变"的假象。
    pub fn set_state_for_test(
        &self,
        module_id: &str,
        version: &str,
        category: Category,
        enabled: Option<bool>,
    ) -> Result<(), String> {
        let mut store = self.store.lock().map_err(|_| "状态锁不可用")?;
        store
            .mark_installed(module_id, version, "x", category)
            .map_err(|e| e.to_string())?;
        if let Some(v) = enabled {
            store
                .set_enabled(module_id, v, category)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

/* ------------------------------------------------------------------ */
/* 模块运行（启动 / 停止）                                             */
/* ------------------------------------------------------------------ */

#[tauri::command]
pub fn start_module(
    state: State<AppState>,
    module_id: String,
) -> Result<crate::runtime::RunView, String> {
    start_module_inner(&state, &module_id)
}

/// 启动单个模块（与 Tauri 解耦，便于测试）。
pub fn start_module_inner(
    state: &AppState,
    module_id: &str,
) -> Result<crate::runtime::RunView, String> {
    // 已在跑：幂等返回，不重复拉起
    if state.processes().is_active(module_id) {
        return Ok(state.processes().view(module_id));
    }
    let spec = state.precheck_start(module_id)?;
    state.processes().start(&spec).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn stop_module(state: State<AppState>, module_id: String) -> crate::runtime::RunView {
    state.processes().stop(&module_id)
}

/// 一键启动：按固定依赖顺序逐个拉起，任一失败不阻塞其余。
///
/// 顺序来自实际启动约束（上游先起，悬浮窗最后）。只启动
/// `installed` 且 `enabled` 的模块；`reserved` 永远不参与。
pub fn start_order(state: &AppState) -> Vec<String> {
    // 固定顺序：输入层 → 模型层 → 语音层 → 总装
    const ORDER: &[&str] = &["dounai-input", "omninexus", "moodvoice", "dounai-ui"];

    let installed: Vec<String> = match state.store.lock() {
        Ok(store) => store
            .all()
            .iter()
            .filter(|(_, st)| st.install == InstallState::Installed && st.enabled)
            .map(|(id, _)| id.clone())
            .collect(),
        Err(_) => Vec::new(),
    };

    // 先按固定顺序，再把不在清单里的已安装模块追加到末尾（保持确定性）
    let mut ordered: Vec<String> = ORDER
        .iter()
        .filter(|id| installed.contains(&id.to_string()))
        .map(|id| id.to_string())
        .collect();
    let mut rest: Vec<String> = installed
        .into_iter()
        .filter(|id| !ORDER.contains(&id.as_str()))
        .collect();
    rest.sort();
    ordered.append(&mut rest);
    ordered
}

#[tauri::command]
pub fn start_all(state: State<AppState>) -> Vec<crate::runtime::RunView> {
    start_all_inner(&state)
}

pub fn start_all_inner(state: &AppState) -> Vec<crate::runtime::RunView> {
    let mut out = Vec::new();
    for id in start_order(state) {
        // 单个失败不阻断其余：与降级哲学一致
        match start_module_inner(state, &id) {
            Ok(v) => out.push(v),
            Err(e) => out.push(crate::runtime::RunView {
                module_id: id,
                run: "failed".into(),
                pid: None,
                last_error: Some(e),
                healthy: false,
            }),
        }
    }
    out
}

#[tauri::command]
pub fn stop_all(state: State<AppState>) -> Vec<crate::runtime::RunView> {
    state.processes().stop_all()
}

/* ------------------------------------------------------------------ */
/* 模块状态                                                            */
/* ------------------------------------------------------------------ */

#[derive(Debug, Serialize)]
pub struct ModuleView {
    pub module_id: String,
    pub install: InstallState,
    pub enabled: bool,
    pub version: String,
    pub path: String,
    pub run: RunState,
    pub last_error: Option<String>,
    /// 运行中时的进程号（未运行为 null）
    pub pid: Option<u32>,
    /// 最近一次健康检查是否通过
    pub healthy: bool,
    /// 是否为外部链接模块（文件在用户自己的目录里）
    pub linked: bool,
}

fn to_view(id: &str, st: &ModuleState, pm: &ProcessManager) -> ModuleView {
    // 运行态以进程表为准，而不是状态文件里的内存字段：
    // 后者在启动器重启后一律是 Stopped，但进程可能还活着，
    // 因此统一从进程表取真实快照并回收已退出进程。
    let rv = pm.view(id);
    ModuleView {
        module_id: id.to_string(),
        install: st.install,
        enabled: st.enabled,
        version: st.version.clone(),
        path: st.path.clone(),
        run: parse_run(&rv.run),
        last_error: rv.last_error.or_else(|| st.last_error.clone()),
        pid: rv.pid,
        healthy: rv.healthy,
        linked: st.linked,
    }
}

fn parse_run(s: &str) -> RunState {
    match s {
        "starting" => RunState::Starting,
        "running" => RunState::Running,
        "failed" => RunState::Failed,
        _ => RunState::Stopped,
    }
}

#[tauri::command]
pub fn list_module_states(state: State<AppState>) -> Result<Vec<ModuleView>, String> {
    let store = state.store.lock().map_err(|_| "状态锁不可用")?;
    let pm = state.processes();
    Ok(store
        .all()
        .iter()
        .map(|(id, st)| to_view(id, st, pm))
        .collect())
}

#[derive(Debug, Deserialize)]
pub struct SetEnabledRequest {
    pub module_id: String,
    pub enabled: bool,
    pub category: String,
}

#[tauri::command]
pub fn set_module_enabled(
    state: State<AppState>,
    request: SetEnabledRequest,
) -> Result<(), String> {
    let category = Category::parse(&request.category)
        .ok_or_else(|| format!("未知分类：{}", request.category))?;
    let mut store = state.store.lock().map_err(|_| "状态锁不可用")?;
    store
        .set_enabled(&request.module_id, request.enabled, category)
        .map_err(|e| e.to_string())
}

#[derive(Debug, Deserialize)]
pub struct UninstallRequest {
    pub module_id: String,
    pub category: String,
    /// 是否同时删除磁盘文件
    #[serde(default)]
    pub remove_files: bool,
}

#[tauri::command]
pub fn uninstall_module(state: State<AppState>, request: UninstallRequest) -> Result<(), String> {
    uninstall_module_inner(&state, request)
}

/// 卸载的实际实现（与 Tauri 解耦，便于测试链接模块不删文件这一关键行为）。
pub fn uninstall_module_inner(state: &AppState, request: UninstallRequest) -> Result<(), String> {
    let category = Category::parse(&request.category)
        .ok_or_else(|| format!("未知分类：{}", request.category))?;

    // 卸载前必须先停止：进程还开着就删目录，Windows 上会因文件占用失败，
    // 而且会留下"目录已删、进程还在跑"的错乱状态。
    if state.processes().is_active(&request.module_id) {
        return Err(format!(
            "模块 {} 正在运行，请先停止再卸载",
            request.module_id
        ));
    }
    // 已退出但仍在进程表里留了失败记录的，也一并清掉
    state.processes().stop(&request.module_id);

    // 链接模块的文件属于用户，卸载只解除登记。
    // 删掉用户自己 clone 的源码目录是不可接受的破坏性操作。
    let linked = state.is_linked(&request.module_id);
    let dir = if linked {
        None
    } else {
        Some(state.module_dir(&request.module_id)?)
    };

    let mut store = state.store.lock().map_err(|_| "状态锁不可用")?;
    store
        .uninstall(&request.module_id, category)
        .map_err(|e| e.to_string())?;

    if let Some(dir) = dir {
        if request.remove_files && dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| format!("删除安装目录失败：{e}"))?;
        }
    }
    Ok(())
}

/* ------------------------------------------------------------------ */
/* 安装                                                                */
/* ------------------------------------------------------------------ */

#[derive(Debug, Deserialize)]
pub struct InstallRequest {
    pub module_id: String,
    pub category: String,
    pub version: String,
    /// zip 包内容（前端已完成下载时传入）
    pub zip_base64: String,
    /// 期望 SHA-256。**必填**：留空会被拒绝。
    ///
    /// 早期版本这里是 `Option`，为空即跳过校验，等于允许安装任意字节。
    /// 哈希校验必须与安装链路绑定，因此改为强制。
    pub expected_sha256: String,
    /// 该模块的 manifest（用于签名验证）。
    /// 发布构建下未签名会被拒绝（见 `trust.rs`）。
    pub manifest: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct InstallResponse {
    pub module_id: String,
    pub files: usize,
    pub bytes: u64,
    pub replaced: bool,
    pub install_dir: String,
    /// 本次安装的信任判定结果，便于 UI 展示"已验签 / 未验签"
    pub trust: TrustVerdict,
}

/// 安装前的全部校验（与 Tauri 解耦，便于直接测试）。
///
/// 顺序刻意安排为「签名 → 身份 → 哈希」：
/// - 签名最便宜且最关键的判断，先做；
/// - 哈希需要先解码 base64 并算摘要，放最后。
///
/// 任一步失败都不应产生任何磁盘写入。
pub fn precheck_install(
    request: &InstallRequest,
    trust_policy: &TrustPolicy,
) -> Result<(TrustVerdict, Vec<u8>), String> {
    // 1) 哈希必填：缺失就是配置错误，不能静默跳过
    let expected = request.expected_sha256.trim();
    if expected.is_empty() {
        return Err("拒绝安装：缺少 expected_sha256（哈希校验不可跳过）".to_string());
    }

    // 2) manifest 签名：按策略判定
    let trust = crate::trust::evaluate(&request.manifest, trust_policy)
        .map_err(|e| format!("拒绝安装：{e}"))?;

    // 3) manifest 声明的 module_id 必须与请求一致，防"张冠李戴"
    if let Some(declared) = request.manifest.get("module_id").and_then(|v| v.as_str()) {
        if declared != request.module_id {
            return Err(format!(
                "拒绝安装：manifest 声明的 module_id（{declared}）与请求（{}）不一致",
                request.module_id
            ));
        }
    }

    let zip_bytes = base64_decode(&request.zip_base64)?;

    // 4) 哈希校验：不匹配直接拒绝，绝不进入解压
    if !verify_sha256(&zip_bytes, expected) {
        return Err(format!(
            "哈希校验失败：期望 {expected}，实际 {}",
            sha256_hex(&zip_bytes)
        ));
    }

    Ok((trust, zip_bytes))
}

#[tauri::command]
pub fn install_module(
    state: State<AppState>,
    request: InstallRequest,
) -> Result<InstallResponse, String> {
    install_module_inner(&state, request)
}

/// 安装的实际实现（与 Tauri 解耦，便于端到端测试整条链路）。
///
/// 顺序：校验 → 标记 Installing → 解压到临时目录 → 原子提交 → 记录状态。
/// 失败时状态置为 Broken，让 UI 提示"可重装"而不是假装已安装。
pub fn install_module_inner(
    state: &AppState,
    request: InstallRequest,
) -> Result<InstallResponse, String> {
    let category = Category::parse(&request.category)
        .ok_or_else(|| format!("未知分类：{}", request.category))?;

    let (trust, zip_bytes) = precheck_install(&request, state.trust_policy())?;

    let dir = state.module_dir(&request.module_id)?;
    let mut store = state.store.lock().map_err(|_| "状态锁不可用")?;
    store
        .mark_installing(&request.module_id)
        .map_err(|e| e.to_string())?;
    drop(store);

    let result = (|| -> Result<InstallResponse, String> {
        let tx = InstallTransaction::begin(&dir).map_err(|e| e.to_string())?;
        extract_zip_into(&tx, &zip_bytes, None).map_err(|e| e.to_string())?;
        let report = tx.commit().map_err(|e| e.to_string())?;
        Ok(InstallResponse {
            module_id: request.module_id.clone(),
            files: report.files,
            bytes: report.bytes,
            replaced: report.replaced,
            install_dir: dir.to_string_lossy().to_string(),
            trust: trust.clone(),
        })
    })();

    let mut store = state.store.lock().map_err(|_| "状态锁不可用")?;
    match &result {
        Ok(_) => {
            store
                .mark_installed(
                    &request.module_id,
                    &request.version,
                    &dir.to_string_lossy(),
                    category,
                )
                .map_err(|e| e.to_string())?;
        }
        Err(err) => {
            // 安装失败：状态回到损坏，UI 会提示可重装
            store
                .mark_broken(&request.module_id, err)
                .map_err(|e| e.to_string())?;
        }
    }

    result
}

/* ------------------------------------------------------------------ */
/* 联网清单                                                            */
/* ------------------------------------------------------------------ */

#[derive(Debug, Serialize)]
pub struct CatalogResponse {
    pub source: String,
    pub entries: Vec<CatalogEntry>,
    pub allowlist: Vec<String>,
    /// 不可用时的原因（供 UI 展示，不伪造成功）
    pub reason: Option<String>,
}

#[tauri::command]
pub fn refresh_catalog(state: State<AppState>, catalog_url: String) -> CatalogResponse {
    let cache = state.data_root.join("catalog-cache.json");

    // 真实 HTTP 客户端；构建失败时降级为不可用，不影响本地已装模块的查看与启停
    let fetcher = crate::http_client::ReqwestClient::default_client();
    let result = match &fetcher {
        Ok(client) => fetch_catalog(client, &state.allowlist, &catalog_url, &cache),
        Err(err) => CatalogResult::Unavailable(format!("HTTP 客户端不可用：{err}")),
    };

    let (source, entries, reason) = match result {
        CatalogResult::Remote(e) => ("remote", e, None),
        CatalogResult::Cached(e) => ("cache", e, None),
        CatalogResult::Unavailable(r) => ("unavailable", Vec::new(), Some(r)),
    };
    CatalogResponse {
        source: source.to_string(),
        entries,
        allowlist: state.allowlist.hosts(),
        reason,
    }
}

/* ------------------------------------------------------------------ */
/* 本地模块：扫描与手动添加                                            */
/* ------------------------------------------------------------------ */

/// 扫描到的候选模块。
#[derive(Debug, Serialize)]
pub struct ScanCandidate {
    pub path: String,
    pub module_id: String,
    pub name: String,
    pub version: String,
    pub category: String,
    /// manifest 是否带非空签名
    pub signed: bool,
    /// 是否已被登记过
    pub registered: bool,
}

/// manifest 候选文件名（安装包与源码仓库两种约定都支持）。
const MANIFEST_NAMES: [&str; 2] = ["manifest.json", "dounai-manifest.json"];

fn find_manifest(dir: &Path) -> Option<PathBuf> {
    MANIFEST_NAMES
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.exists())
}

/// 在给定目录下扫描含 manifest 的子目录（含自身），最多两层。
///
/// 为什么要扫描而不是让用户逐个试：用户把仓库 clone 到一个大目录后，
/// 往往不记得哪个子目录才是模块根。扫描把候选列出来，减少盲目试错。
#[tauri::command]
pub fn scan_local_modules(
    state: State<AppState>,
    root: String,
) -> Result<Vec<ScanCandidate>, String> {
    scan_local_modules_inner(&state, &root)
}

pub fn scan_local_modules_inner(
    state: &AppState,
    root: &str,
) -> Result<Vec<ScanCandidate>, String> {
    let root_path = PathBuf::from(root.trim());
    if !root_path.is_dir() {
        return Err(format!("目录不存在或不是目录：{}", root_path.display()));
    }

    let mut out = Vec::new();
    let mut budget = 2000usize;
    let mut seen_dirs = std::collections::HashSet::new();

    // 递归遍历（最大深度 6，配合预算上限，避免把整个磁盘扫一遍）。
    // 为什么递归而不是只扫一层：用户常把模块 clone 在 `repo/modules/xxx` 这类
    // 多级结构里（如 `D:\code\all\modules\omninexus`），一层扫不到。
    //
    // 跳过常见的巨型无关目录：node_modules / target / dist / .git / .venv /
    // __pycache__ / .run / .cache 等——这些目录可能含数十万文件，
    // 不跳过会瞬间吃光预算，导致后续模块目录根本没被扫到（真实事故：
    // 扫 D:\Dounai-ai 时 Dounai 仓库的 node_modules+target 抢先占满预算，
    // 后面的 OmniNexus 等全被漏掉）。
    fn walk(
        dir: &Path,
        depth: usize,
        budget: &mut usize,
        seen: &mut std::collections::HashSet<PathBuf>,
        out: &mut Vec<PathBuf>,
    ) {
        if depth > 6 || *budget == 0 {
            return;
        }
        let Ok(canon) = dir.canonicalize() else {
            return;
        };
        if !seen.insert(canon.clone()) {
            return; // 防止符号链接循环
        }
        *budget -= 1;

        // 已确认是模块目录：登记后不再下钻。
        // 模块内部几乎不会嵌套另一个模块，而它的子目录（src/docs/tests…）
        // 可能含大量文件，继续递归只会浪费预算、还可能扫出误报。
        if find_manifest(&canon).is_some() {
            out.push(canon);
            return;
        }

        out.push(canon);

        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let Ok(ft) = e.file_type() else {
                continue;
            };
            if !ft.is_dir() {
                continue;
            }
            // 跳过大型构建/依赖目录（大小写不敏感）
            let name = e.file_name().to_string_lossy().to_lowercase();
            if matches!(
                name.as_str(),
                "node_modules"
                    | "target"
                    | "dist"
                    | "build"
                    | ".git"
                    | ".svn"
                    | ".hg"
                    | ".venv"
                    | "venv"
                    | "__pycache__"
                    | ".run"
                    | ".cache"
                    | ".idea"
                    | ".vscode"
                    | ".pytest_cache"
                    | ".mypy_cache"
                    | ".ruff_cache"
            ) {
                continue;
            }
            walk(&e.path(), depth + 1, budget, seen, out);
        }
    }

    let mut dirs: Vec<PathBuf> = Vec::new();
    walk(&root_path, 0, &mut budget, &mut seen_dirs, &mut dirs);

    for dir in dirs {
        let Some(manifest_path) = find_manifest(&dir) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(&manifest_path) else {
            continue;
        };
        let Ok(m) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };

        let module_id = m
            .get("module_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if module_id.is_empty() {
            continue;
        }

        let signed = m
            .get("signature")
            .and_then(|s| s.get("value"))
            .and_then(|v| v.as_str())
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);

        let registered = state
            .store
            .lock()
            .map(|s| s.get(&module_id).is_some())
            .unwrap_or(false);

        out.push(ScanCandidate {
            path: dir.to_string_lossy().to_string(),
            module_id: module_id.clone(),
            name: m
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(&module_id)
                .to_string(),
            version: m
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("0.0.0")
                .to_string(),
            category: m
                .get("category")
                .and_then(|v| v.as_str())
                .unwrap_or("optional")
                .to_string(),
            signed,
            registered,
        });
    }

    out.sort_by(|a, b| a.module_id.cmp(&b.module_id));
    Ok(out)
}

#[derive(Debug, Deserialize)]
pub struct AddLocalModuleRequest {
    /// 模块目录（绝对路径，或相对启动器工作目录）
    pub path: String,
}

/// 把一个已存在于本地的目录登记为模块。
///
/// 与「从 zip 安装」的区别：**不复制文件**，只登记路径。
/// 这是刻意取舍——用户 clone 的是自己的仓库，模型目录可能有几十 GB，
/// 复制一份既浪费空间，也会让"改了源码但要重新导入"变成日常困扰。
///
/// 签名按信任策略校验（见 inner 内注释），与受管 zip 安装一致。
///
/// 安全边界：
/// - 必须是用户显式指定的目录（不是被诱骗填 URL / 任意路径）；
/// - 必须存在 `manifest.json`，且 `module_id` 通过文件名校验；
/// - `working_dir` 被限制在该模块根内，仍然越不出去；
/// - 卸载只解除登记，**绝不删除用户文件**。
#[tauri::command]
pub fn add_local_module(
    state: State<AppState>,
    request: AddLocalModuleRequest,
) -> Result<ModuleView, String> {
    add_local_module_inner(&state, &request.path)
}

pub fn add_local_module_inner(state: &AppState, raw_path: &str) -> Result<ModuleView, String> {
    let trimmed = raw_path.trim().trim_matches('"');
    if trimmed.is_empty() {
        return Err("请填写模块目录路径".to_string());
    }

    let dir = PathBuf::from(trimmed);
    if !dir.exists() {
        return Err(format!("目录不存在：{}", dir.display()));
    }
    if !dir.is_dir() {
        return Err(format!("不是目录：{}", dir.display()));
    }
    // 规范化：存绝对路径，避免启动器换工作目录后解析失败
    let dir = dir
        .canonicalize()
        .map_err(|e| format!("无法解析路径：{e}"))?;

    let manifest_path = find_manifest(&dir).ok_or_else(|| {
        format!(
            "{} 下没有 manifest.json 或 dounai-manifest.json。\n\
             如果你拉取的是模型权重而不是模块，它不是可添加的模块——\
             模型权重应放在模块自己的数据目录里，由模块读取。",
            dir.display()
        )
    })?;

    let text =
        std::fs::read_to_string(&manifest_path).map_err(|e| format!("读取 manifest 失败：{e}"))?;
    let manifest: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("manifest 不是合法 JSON：{e}"))?;

    let module_id = manifest
        .get("module_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if module_id.is_empty() {
        return Err("manifest 缺少 module_id".to_string());
    }
    // 复用与受管模块相同的 id 规则，防止 id 本身带路径分隔符
    if !module_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        || module_id.contains("..")
        || !crate::security::is_safe_filename(&module_id)
    {
        return Err(format!("manifest 的 module_id 不合法：{module_id}"));
    }

    // 签名：与受管 zip 安装同一套信任策略。
    //
    // 为什么不能"本地目录就免检"：manifest 决定启动什么命令、开哪些权限，
    // 一个被篡改的 manifest 可以把恶意命令伪装成正常模块。用户选择目录
    // 只说明"这些文件是我放的"，不说明"这份 manifest 没被改过"。
    // 调试构建允许未签名（便于开发），发布构建必须验签通过。
    let _verdict = crate::trust::evaluate(&manifest, state.trust_policy())
        .map_err(|e| format!("拒绝添加：{e}"))?;

    let category = manifest
        .get("category")
        .and_then(|v| v.as_str())
        .and_then(Category::parse)
        .unwrap_or(Category::Optional);
    let version = manifest
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("0.0.0");

    // 已被启动器安装过的受管模块，不允许直接改成本地链接（会让文件归属混乱）
    {
        let store = state.store.lock().map_err(|_| "状态锁不可用")?;
        if let Some(st) = store.get(&module_id) {
            if st.install == InstallState::Installed && !st.linked {
                return Err(format!(
                    "模块 {module_id} 已由启动器安装（{}），请先卸载再改为本地目录",
                    st.path
                ));
            }
        }
    }

    let path_str = dir.to_string_lossy().to_string();
    {
        let mut store = state.store.lock().map_err(|_| "状态锁不可用")?;
        store
            .mark_linked(&module_id, version, &path_str, category)
            .map_err(|e| e.to_string())?;
    }

    // 返回登记后的视图
    let store = state.store.lock().map_err(|_| "状态锁不可用")?;
    let st = store
        .get(&module_id)
        .ok_or_else(|| "登记后读取状态失败".to_string())?;
    let pm = state.processes();
    Ok(to_view(&module_id, st, pm))
}

/// 解除本地模块登记（不删除文件）。
#[tauri::command]
pub fn remove_local_module(state: State<AppState>, module_id: String) -> Result<(), String> {
    remove_local_module_inner(&state, &module_id)
}

pub fn remove_local_module_inner(state: &AppState, module_id: &str) -> Result<(), String> {
    if state.processes().is_active(module_id) {
        return Err(format!("模块 {module_id} 正在运行，请先停止"));
    }
    state.processes().stop(module_id);

    let mut store = state.store.lock().map_err(|_| "状态锁不可用")?;
    let st = store
        .get(module_id)
        .ok_or_else(|| format!("模块 {module_id} 未登记"))?;
    if !st.linked {
        return Err(format!(
            "模块 {module_id} 由启动器安装，请用「卸载」而不是「解除登记」"
        ));
    }
    store
        .purge(&[module_id.to_string()])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/* ------------------------------------------------------------------ */
/* 本地模块：批量登记                                                  */
/* ------------------------------------------------------------------ */

/// 单个路径的批量登记结果。
#[derive(Debug, Serialize)]
pub struct BulkAddResult {
    pub module_id: String,
    pub path: String,
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct BulkAddLocalModulesRequest {
    /// 待登记的模块目录路径列表（绝对路径，来自扫描结果）。
    pub paths: Vec<String>,
}

/// 一次登记多个本地目录（配合扫描结果使用，避免逐个手动添加）。
///
/// 语义与单条 `add_local_module` 完全一致（同样验签、只登记路径、不复制文件），
/// 只是循环执行并**逐条汇报成败**——一个失败不影响其余。
#[tauri::command]
pub fn add_local_modules_bulk(
    state: State<AppState>,
    request: BulkAddLocalModulesRequest,
) -> Result<Vec<BulkAddResult>, String> {
    Ok(add_local_modules_bulk_inner(&state, &request.paths))
}

/// 批量登记的实际实现（与 Tauri 解耦，便于测试"单个失败不影响其余"）。
pub fn add_local_modules_bulk_inner(state: &AppState, paths: &[String]) -> Vec<BulkAddResult> {
    let mut results = Vec::with_capacity(paths.len());
    for p in paths {
        match add_local_module_inner(state, p) {
            Ok(view) => results.push(BulkAddResult {
                module_id: view.module_id,
                path: view.path,
                ok: true,
                error: None,
            }),
            Err(e) => results.push(BulkAddResult {
                module_id: String::new(),
                path: p.clone(),
                ok: false,
                error: Some(e),
            }),
        }
    }
    results
}

/* ------------------------------------------------------------------ */
/* 数据清除（用户可一键清除本地数据）                                  */
/* ------------------------------------------------------------------ */

/// 清除范围。
#[derive(Debug, Deserialize)]
pub struct PurgeRequest {
    /// 清除安装日志与下载缓存
    #[serde(default)]
    pub caches: bool,
    /// 清除模块状态记录（不删模块文件）
    #[serde(default)]
    pub state: bool,
    /// 清除全部模块安装目录（危险操作，需显式开启）
    #[serde(default)]
    pub module_files: bool,
}

#[derive(Debug, Serialize)]
pub struct PurgeResponse {
    pub removed_caches: Vec<String>,
    pub state_cleared: bool,
    pub removed_module_dirs: Vec<String>,
    pub errors: Vec<String>,
}

/// 一键清除本层数据。
///
/// 只清**本层自己的数据**：缓存、状态、模块安装目录。
/// 用户的对话、记忆、密钥属于上游模块，本层不接触也不清除，
/// 需要在对应模块里操作——这一点必须在 UI 上说明，否则用户会以为
/// 「清除完了」而实际对话数据还在。
#[tauri::command]
pub fn purge_local_data(state: State<AppState>, request: PurgeRequest) -> PurgeResponse {
    purge_local_data_inner(&state, request)
}

/// 实际清除逻辑（与 Tauri 解耦，便于测试）。
pub fn purge_local_data_inner(state: &AppState, request: PurgeRequest) -> PurgeResponse {
    let mut removed_caches = Vec::new();
    let mut removed_module_dirs = Vec::new();
    let mut errors = Vec::new();
    let mut state_cleared = false;

    if request.caches {
        for name in ["catalog-cache.json", "catalog-cache.json.tmp"] {
            let p = state.data_root.join(name);
            if p.exists() {
                match std::fs::remove_file(&p) {
                    Ok(()) => removed_caches.push(name.to_string()),
                    Err(e) => errors.push(format!("删除 {name} 失败：{e}")),
                }
            }
        }
    }

    if request.state {
        let state_path = state.data_root.join("modules-state.json");
        if state_path.exists() {
            match std::fs::remove_file(&state_path) {
                Ok(()) => state_cleared = true,
                Err(e) => errors.push(format!("删除状态文件失败：{e}")),
            }
        } else {
            state_cleared = true;
        }
    }

    if request.module_files {
        let modules_dir = state.data_root.join("modules");
        if modules_dir.exists() {
            match std::fs::read_dir(&modules_dir) {
                Ok(entries) => {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.is_dir() {
                            match std::fs::remove_dir_all(&path) {
                                Ok(()) => removed_module_dirs.push(
                                    path.file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                        .to_string(),
                                ),
                                Err(e) => errors.push(format!("删除 {} 失败：{e}", path.display())),
                            }
                        }
                    }
                }
                Err(e) => errors.push(format!("读取模块目录失败：{e}")),
            }
        }
    }

    PurgeResponse {
        removed_caches,
        state_cleared,
        removed_module_dirs,
        errors,
    }
}

/* ------------------------------------------------------------------ */
/* 下载（依赖安装：重试 / 断点续传 / 镜像加速 / 哈希校验）        */
/* ------------------------------------------------------------------ */

#[derive(Debug, Deserialize)]
pub struct DownloadRequest {
    /// 模块 id，仅用于组成落盘文件名
    pub module_id: String,
    /// 候选下载源，按序尝试（镜像加速）
    pub sources: Vec<String>,
    /// 期望 SHA-256，必填
    pub expected_sha256: String,
    /// 每个源的重试次数
    #[serde(default = "default_retries")]
    pub retries: usize,
}

fn default_retries() -> usize {
    2
}

#[derive(Debug, Serialize)]
pub struct DownloadResponse {
    pub path: String,
    pub bytes: u64,
    pub source: String,
    pub resumed_from: u64,
    pub attempts: usize,
}

/// 下载模块包到数据目录。
///
/// 白名单在下载前逐一校验每个源：任一源不在白名单内就整体拒绝，
/// 而不是"跳过它继续试下一个"——后者会让配置错误被静默忽略。
#[tauri::command]
pub fn download_package(
    state: State<AppState>,
    request: DownloadRequest,
) -> Result<DownloadResponse, String> {
    if request.sources.is_empty() {
        return Err("没有提供下载源".to_string());
    }
    if request.expected_sha256.trim().is_empty() {
        return Err("拒绝下载：缺少 expected_sha256".to_string());
    }

    // 白名单：全部源都要通过
    for src in &request.sources {
        state
            .allowlist
            .check(src)
            .map_err(|e| format!("下载源 {src} 被白名单拒绝：{e}"))?;
    }

    let safe_id = crate::security::is_safe_filename(&request.module_id);
    if !safe_id {
        return Err(format!("非法的模块 id：{}", request.module_id));
    }

    let dest = state
        .data_root
        .join("downloads")
        .join(format!("{}.zip", request.module_id));

    let client = crate::http_client::ReqwestClient::default_client()?;
    let options = crate::download::DownloadOptions {
        retries: request.retries,
        backoff_ms: 150,
        expected_sha256: Some(request.expected_sha256.trim().to_string()),
    };

    let report = crate::download::download(&client, &request.sources, &dest, &options)
        .map_err(|e| format!("下载失败：{e}"))?;

    Ok(DownloadResponse {
        path: report.path.to_string_lossy().to_string(),
        bytes: report.bytes,
        source: report.source,
        resumed_from: report.resumed_from,
        attempts: report.attempts,
    })
}

/* ------------------------------------------------------------------ */
/* 信任策略信息                                                        */
/* ------------------------------------------------------------------ */

#[derive(Debug, Serialize)]
pub struct TrustInfo {
    pub trusted_keys: usize,
    pub allow_unsigned: bool,
    /// 完全放行（无公钥且允许未签名）——仅开发期可能出现
    pub open: bool,
}

#[tauri::command]
pub fn trust_info(state: State<AppState>) -> TrustInfo {
    let p = state.trust_policy();
    TrustInfo {
        trusted_keys: p.trusted_keys().len(),
        allow_unsigned: p.allow_unsigned(),
        open: p.is_open(),
    }
}

/* 工具                                                                */
/* ------------------------------------------------------------------ */

fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(input.trim())
        .map_err(|e| format!("zip 数据不是合法 base64：{e}"))
}

/// 计算文件哈希（供前端校验已下载内容）。
#[tauri::command]
pub fn hash_file(path: String, state: State<AppState>) -> Result<String, String> {
    // 只允许读 data_root 内的文件，防止被当作任意文件读取原语
    let p = PathBuf::from(&path);
    let root = &state.data_root;
    let canonical_root = root.canonicalize().map_err(|e| e.to_string())?;
    let canonical = p.canonicalize().map_err(|e| format!("无法解析路径：{e}"))?;
    if !canonical.starts_with(&canonical_root) {
        return Err("只能读取启动器数据目录内的文件".into());
    }
    let bytes = std::fs::read(&canonical).map_err(|e| e.to_string())?;
    Ok(sha256_hex(&bytes))
}

#[allow(dead_code)]
fn _assert_path_type(_: &Path) {}
