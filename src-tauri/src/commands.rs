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
        })
    }

    pub fn trust_policy(&self) -> &TrustPolicy {
        &self.trust
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
}

fn to_view(id: &str, st: &ModuleState) -> ModuleView {
    ModuleView {
        module_id: id.to_string(),
        install: st.install,
        enabled: st.enabled,
        version: st.version.clone(),
        path: st.path.clone(),
        run: st.run,
        last_error: st.last_error.clone(),
    }
}

#[tauri::command]
pub fn list_module_states(state: State<AppState>) -> Result<Vec<ModuleView>, String> {
    let store = state.store.lock().map_err(|_| "状态锁不可用")?;
    Ok(store.all().iter().map(|(id, st)| to_view(id, st)).collect())
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
    let category = Category::parse(&request.category)
        .ok_or_else(|| format!("未知分类：{}", request.category))?;

    let dir = state.module_dir(&request.module_id)?;
    let mut store = state.store.lock().map_err(|_| "状态锁不可用")?;
    store
        .uninstall(&request.module_id, category)
        .map_err(|e| e.to_string())?;

    if request.remove_files && dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("删除安装目录失败：{e}"))?;
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

/* ------------------------------------------------------------------ */
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
