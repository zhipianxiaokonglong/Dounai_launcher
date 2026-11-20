//! M3 命令面的共享状态：双根路径、实例表、信任表与保留策略。
//!
//! v1 的 `AppState` 把模块状态、进程表与信任策略揉在一起，M3 只保留
//! `launcher-core` 的 v2 状态文件（installed/trust/retention），进程表
//! 由 `lifecycle_runtime::Supervisor` 独立持有。运行态不落盘。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use launcher_core::chain::{ChainRegistry, ChainRunResult};
use launcher_core::m2::catalog::{entry_key, CatalogDocument, CatalogEntry};
use launcher_core::m2::installed::{InstalledState, Instance};
use launcher_core::m2::retention::RetentionPolicy;
use launcher_core::m2::sources::{SourceClass, SourceEntry, SourceRegistry};
use launcher_core::m2::trust::TrustStore;
use launcher_core::m2::{InstanceSourceKind, M2Error};
use launcher_core::paths::{InstallMode, Paths};
use launcher_core::time::utc_now;

use crate::diag_log::{self, SharedLog};

/// 日志页里代表「启动器自身」的伪实例 id。
///
/// 启动器操作（装依赖、切换 active、安装、导入等）不属于任何单个模块，写在
/// `data_root/logs/`，日志页用这个 id 读。真实实例的 id 不可能与它相撞。
pub const LAUNCHER_LOG_ID: &str = "__launcher__";

/// K1 是模块包签名钥，2026-11-13 生成，私钥离线保管在仓库外。
/// 登记文件见仓库 `keys/K1.json`，生成与轮换规程见 `docs/SIGNING.md`。
///
/// 包内 `signature.pubkey` 自带公钥，所以验签不需要内置公钥；信任判定只比对
/// 下面这枚指纹。`BUILTIN_K1_PUBKEY_B64` 只供审计与离线复核，不参与判定。
pub const BUILTIN_K1_PUBKEY_B64: &str = "8w9nSTqc4mWn1n9z1JITBHDU1RBFjU9iPjD2rSgw28U=";
pub const BUILTIN_K1_FINGERPRINT: &str =
    "sha256:7da721cb4346c215b857e50be8bbeeb89374180884eebc8802296b5a898bd909";

/// K5 是 catalog 签名钥，必须与 K1 分离，2026-11-13 生成，私钥离线保管在仓库外。
/// 登记文件见仓库 `keys/K5.json`，生成与轮换规程见 `docs/SIGNING.md`。
///
/// catalog 的签名段不含 `pubkey`，启动器只能拿本地 sources.json 里该源登记的
/// 公钥验签；这把公钥与包信任表物理隔离，不存在「会话内替换」的入口。
pub const BUILTIN_K5_PUBKEY_B64: &str = "+56W2k6Sw60cMspmEKUESgqX1TGJLqmVozPJvI0n8G8=";
pub const BUILTIN_K5_FINGERPRINT: &str =
    "sha256:8c1d31cf6f5ce98961be579f88fe49f8521242bcaa07483e85d0bbea1532eff5";

/// K3 是 nightly 模块包签名钥，2026-11-20 生成。与 K1 一样只按指纹判定，
/// 指纹进 trust store 内置表后 nightly 包判 A 类。私钥在 nightly 仓的 CI 环境里，
/// 泄露处置见 nightly 仓 `contracts.md` 第 7 节（短窗 + 换钥 + 发新版启动器）。
pub const BUILTIN_K3_FINGERPRINT: &str =
    "sha256:554305ebee1ac076368606a2381f1ece9c4c55afce36568decc8566e8c58a60f";

/// K4 是 nightly catalog 签名钥，只验 nightly 源的 catalog，不进包信任表。
pub const BUILTIN_K4_PUBKEY_B64: &str = "j+eFVaRvr2aoeMGPbwJtHu9hGSQWIFXlCkyU1bFiUYc=";
pub const BUILTIN_K4_FINGERPRINT: &str =
    "sha256:c1ba74e21a86c0de0d093c8966c8b43cb59a84483b0ee28e2d720ede8acf5253";

/// 会话缓存里的一条可安装条目。键含 source_id / variant / version，
/// 跨源同名与多版本不做隐式优选，用户点哪一行就装哪一行。
#[derive(Debug, Clone)]
pub struct CachedEntry {
    pub key: String,
    pub source_id: String,
    pub source_name: String,
    pub source_kind: SourceClass,
    pub catalog_fingerprint: String,
    pub auto_update: bool,
    pub host_suffixes: Vec<String>,
    pub group_ids: Vec<String>,
    pub entry: CatalogEntry,
    /// 条目所属的那份已验签 catalog，撤销列表与版本单调判定都从它读。
    pub doc: CatalogDocument,
}

impl CachedEntry {
    pub fn new(
        source: &SourceEntry,
        group_ids: Vec<String>,
        doc: &CatalogDocument,
        entry: &CatalogEntry,
    ) -> Self {
        Self {
            key: entry_key(&source.id, &entry.module_id, &entry.variant, &entry.version),
            source_id: source.id.clone(),
            source_name: source.name.clone(),
            source_kind: source.kind,
            catalog_fingerprint: source.catalog_fingerprint.clone(),
            auto_update: source.auto_update,
            host_suffixes: source.host_suffixes.clone(),
            group_ids,
            entry: entry.clone(),
            doc: doc.clone(),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// 关闭主窗口时的行为。
///
/// 启动器是运行期管理者，不是「拉起就走」的启动器：实例的 Job Object 由这个
/// 进程持有，进程一退，`KILL_ON_JOB_CLOSE` 就把整棵树收掉。所以「关窗」和
/// 「退出」对用户是两件事，必须显式选一次，不能默认按 Windows 习惯直接退。
///
/// 三个取值里只有 `Tray` 能让实例在窗口消失后继续跑——它靠的是进程不退，
/// 而不是拆掉 Job 的兜底能力。真正退进程还能留住实例，Windows 上没有免费
/// 的实现路径，见 `docs/DESIGN.md` 的关闭行为一节。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseBehavior {
    /// 窗口收进托盘，实例继续跑；托盘菜单「退出」才真的停。
    #[default]
    Tray,
    /// 停掉全部实例并退出进程。
    Quit,
    /// 每次都问，勾了「记住」才落到上面两项之一。
    Ask,
}

impl CloseBehavior {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "tray" => Some(Self::Tray),
            "quit" => Some(Self::Quit),
            "ask" => Some(Self::Ask),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tray => "tray",
            Self::Quit => "quit",
            Self::Ask => "ask",
        }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ClosePolicyFile {
    #[serde(default)]
    pub behavior: CloseBehavior,
}

/// 诊断设置：目前只有「是否捕获子进程输出」。
///
/// 子进程的 stdout/stderr 里可能有用户的对话内容，默认丢弃；打开后新拉起的
/// 实例会把输出写进各自的日志。设置落盘到 `state/diagnostics.json`，因为
/// 「排障时打开、事后关掉」跨重启才有意义——只存在内存里，用户下次还得再点一次。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Diagnostics {
    #[serde(default)]
    pub child_output: bool,
}

/// 首次向导的完成状态。
///
/// 只记「走完过或明确跳过」，不记进度：向导没有断点续传的语义，中途关掉下次
/// 从头开始更省事，也不会出现「停在第三步、模块其实装了一半」这种要还原的状态。
/// 存盘而不是只放内存，是因为向导的目的就是「新用户只被引导一次」——
/// 每次启动都重新弹一遍比不弹更烦人。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Onboarding {
    #[serde(default = "onboarding_schema")]
    pub schema_version: u32,
    #[serde(default)]
    pub completed: bool,
    /// true 表示用户点了「以后再说」，不是走完流程。
    #[serde(default)]
    pub skipped: bool,
    #[serde(default)]
    pub completed_at: Option<String>,
}

fn onboarding_schema() -> u32 {
    1
}

impl Default for Onboarding {
    fn default() -> Self {
        Self {
            schema_version: onboarding_schema(),
            completed: false,
            skipped: false,
            completed_at: None,
        }
    }
}

pub struct AppStateV2 {
    pub paths: Paths,
    installed: Mutex<InstalledState>,
    trust: Mutex<TrustStore>,
    retention: Mutex<RetentionPolicy>,
    sources: Mutex<SourceRegistry>,
    /// 用户自定义链路。默认链路不在这里，由实例表按 role 拓扑隐式生成。
    chains: Mutex<ChainRegistry>,
    /// 每条链路最近一次启动结果，供 only_started 回滚停止使用。内存态，重启即空。
    last_chain_runs: Mutex<BTreeMap<String, ChainRunResult>>,
    /// 本次会话内已验签的目录条目，键是 entry_key。重启即空，不落盘。
    catalog: Mutex<BTreeMap<String, CachedEntry>>,
    /// v1 界面的启用开关是内存态：M0 冻结的 installed.json 没有 enabled 字段，
    /// M3 不新增状态文件。M4 界面重做时按实例粒度重新定义。
    disabled: Mutex<BTreeSet<String>>,
    pub dev_mode: AtomicBool,
    /// 诊断设置（是否捕获子进程输出）。新建实例读取它，改动即时落盘。
    diagnostics: Mutex<Diagnostics>,
    /// 关闭主窗口时的行为。落盘到 `state/close-behavior.json`。
    close_behavior: Mutex<CloseBehavior>,
    /// 已经决定退出。托盘「退出」与用户选「停止并退出」都会置位，
    /// 窗口关闭事件据此放行——否则「收进托盘」的拦截会把自己的退出也挡掉。
    pub quitting: AtomicBool,
    /// 托盘图标是否建成。建不成时「收进托盘」就没有回来的入口，
    /// 关窗必须退回「停止并退出」，不能让窗口消失得无声无息。
    tray_ready: AtomicBool,
    /// 首次向导是否走完。界面据此决定开机是否弹向导。
    onboarding: Mutex<Onboarding>,
    /// 启动时补写/校正 `install.json` 的说明；`None` 表示本来就是一致的。
    /// 设置页直接展示，用户能看出「安装器没跑，是启动器兜的」。
    pub install_note: Option<String>,
    /// 每实例一把互斥锁：依赖准备、卸载与清理共用，避免 `.staging` 并发互删。
    instance_locks: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
    /// 启动器自身的操作日志。写到 `data_root/logs/`，日志页按 `LAUNCHER_LOG_ID` 读。
    /// 懒打开：日志目录不具写权限时退化为不落盘，不阻断启动器。
    app_log: Mutex<Option<SharedLog>>,
}

impl AppStateV2 {
    pub fn new(install_root: PathBuf, data_root: PathBuf) -> Result<Self, String> {
        let paths = Paths::new(InstallMode::CurrentUser, install_root, data_root)
            .map_err(|e| format!("双根路径不合法：{e}"))?;
        for dir in [
            paths.state_dir(),
            paths.data_root.join("instances"),
            paths.data_root.join("downloads"),
            paths.data_root.join("logs"),
        ] {
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("创建 {} 失败：{e}", dir.display()))?;
        }
        let installed = load_or(&paths.state_dir().join("installed.json"), || {
            Ok(InstalledState::new())
        })?;
        // M5 的安装器还没做，`install.json` 从来没人写；不在这里兜底，
        // `prepare_instance_env` 一读就是 os error 2，整条环境链路跑不起来。
        let (_, install_note) = launcher_core::install::InstallState::ensure(
            &paths,
            paths.bundled_uv_path(),
            launcher_core::env::bootstrap::UV_SHA256.to_string(),
            launcher_core::env::bootstrap::UV_VERSION.to_string(),
        )
        .map_err(|e| format!("初始化 install.json 失败：{e}"))?;
        if let Some(note) = &install_note {
            eprintln!("[install] {note}");
        }
        let trust_path = paths.state_dir().join("trust.json");
        let mut trust = load_or(&trust_path, || {
            TrustStore::new_with_nightly(
                BUILTIN_K1_FINGERPRINT,
                BUILTIN_K5_FINGERPRINT,
                Some(BUILTIN_K3_FINGERPRINT),
            )
            .map_err(|e| e.to_string())
        })?;
        // 老 trust.json 里没有 K3：补齐内置项后写回，不动用户确认过的公钥。
        if trust
            .ensure_builtin_k3(BUILTIN_K3_FINGERPRINT)
            .map_err(|e| e.to_string())?
        {
            trust.save(&trust_path).map_err(|e| e.to_string())?;
        }
        let retention = load_or(&paths.state_dir().join("retention.json"), || {
            Ok(RetentionPolicy::default())
        })?;
        let sources_path = paths.state_dir().join("sources.json");
        let mut sources = load_or(&sources_path, || {
            SourceRegistry::builtin_with_nightly(
                BUILTIN_K5_PUBKEY_B64,
                BUILTIN_K5_FINGERPRINT,
                Some(BUILTIN_K4_PUBKEY_B64),
                Some(BUILTIN_K4_FINGERPRINT),
            )
            .map_err(|e| e.to_string())
        })?;
        // load_or 只做反序列化，绕过 core 的隐藏剔除：旧文件里被撤销的 GitHub
        // 镜像会原样留在内存、显示在下载页。这里补上剔除，并把清理写回文件，
        // 免得每次启动都重复读到这条已经不打算下发的源。
        let before = sources.sources.len();
        sources.drop_hidden_builtin();
        // 老 sources.json 里没有 nightly 源，补齐内置项；用户手加的同 id 源不动。
        let added_nightly = sources
            .ensure_builtin_nightly(BUILTIN_K4_PUBKEY_B64, BUILTIN_K4_FINGERPRINT)
            .map_err(|e| e.to_string())?;
        if sources.sources.len() != before || added_nightly {
            sources.save(&sources_path).map_err(|e| e.to_string())?;
        }
        let chains = load_or(&paths.state_dir().join("chains.json"), || {
            Ok(ChainRegistry::empty())
        })?;
        chains
            .validate()
            .map_err(|e| format!("chains.json 不合法：{e}"))?;
        let mut diagnostics = load_or(&paths.state_dir().join("diagnostics.json"), || {
            Ok(Diagnostics::default())
        })?;
        // 环境变量仍可临时强开：排障时不必先改设置再重启启动器。
        if crate::diag_log::env_child_output() == Some(true) {
            diagnostics.child_output = true;
        }
        let onboarding = load_or(&paths.state_dir().join("onboarding.json"), || {
            Ok(Onboarding::default())
        })?;
        let close_behavior = load_or(&paths.state_dir().join("close-behavior.json"), || {
            Ok(ClosePolicyFile::default())
        })?
        .behavior;
        Ok(Self {
            paths,
            installed: Mutex::new(installed),
            trust: Mutex::new(trust),
            retention: Mutex::new(retention),
            sources: Mutex::new(sources),
            chains: Mutex::new(chains),
            last_chain_runs: Mutex::new(BTreeMap::new()),
            catalog: Mutex::new(BTreeMap::new()),
            disabled: Mutex::new(BTreeSet::new()),
            dev_mode: AtomicBool::new(false),
            diagnostics: Mutex::new(diagnostics),
            onboarding: Mutex::new(onboarding),
            close_behavior: Mutex::new(close_behavior),
            quitting: AtomicBool::new(false),
            tray_ready: AtomicBool::new(false),
            install_note,
            instance_locks: Mutex::new(BTreeMap::new()),
            app_log: Mutex::new(None),
        })
    }

    /// 启动器日志目录。与实例日志同格式轮转，只是落在数据根而非实例目录。
    pub fn app_log_dir(&self) -> PathBuf {
        self.paths.data_root.join("logs")
    }

    /// 写一条启动器日志。第一次调用时打开 sink，之后复用。
    ///
    /// 日志失败一律静默：诊断设施不能让主流程失败。
    pub fn app_log_line(&self, scope: &str, message: &str) {
        let mut guard = diag_log::lock(&self.app_log);
        if guard.is_none() {
            *guard = diag_log::open_shared(&self.app_log_dir()).ok();
        }
        if let Some(log) = guard.as_ref() {
            diag_log::write(log, scope, message);
        }
    }

    /// 日志页按 id 映射到目录：`__launcher__` 走数据根，其余走实例目录。
    pub fn logs_dir_for(&self, instance_id: &str) -> PathBuf {
        if instance_id == LAUNCHER_LOG_ID {
            self.app_log_dir()
        } else {
            self.paths.instance_logs_dir_keyed(instance_id)
        }
    }

    /// 取实例锁槽。同一实例永远拿到同一个 `Arc`，不同实例互不阻塞。
    ///
    /// 调用方必须自己持有 guard：`let slot = state.instance_slot(id); let _held = slot.lock()…`。
    /// 不做「返回 guard」的包装，是因为 guard 的生命周期绑在局部 `Arc` 上，返回即借用错误。
    pub fn instance_slot(&self, instance_id: &str) -> Arc<Mutex<()>> {
        let mut map = lock(&self.instance_locks);
        map.entry(instance_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub fn installed_path(&self) -> PathBuf {
        self.paths.state_dir().join("installed.json")
    }

    pub fn trust_path(&self) -> PathBuf {
        self.paths.state_dir().join("trust.json")
    }

    pub fn retention_path(&self) -> PathBuf {
        self.paths.state_dir().join("retention.json")
    }

    pub fn with_installed<R>(&self, f: impl FnOnce(&InstalledState) -> R) -> R {
        f(&lock(&self.installed))
    }

    /// 改动后一次原子落盘；闭包返回 Err 时不写文件，内存改动也不生效
    /// 之外的部分由调用方负责（见 `mutate_*` 的用法约束）。
    pub fn mutate_installed<R>(
        &self,
        f: impl FnOnce(&mut InstalledState) -> Result<R, String>,
    ) -> Result<R, String> {
        let mut guard = lock(&self.installed);
        let before = guard.clone();
        match f(&mut guard) {
            Ok(value) => {
                guard
                    .save(&self.installed_path())
                    .map_err(|e| e.to_string())?;
                Ok(value)
            }
            Err(error) => {
                *guard = before;
                Err(error)
            }
        }
    }

    pub fn save_installed(&self, state: &InstalledState) -> Result<(), String> {
        state
            .save(&self.installed_path())
            .map_err(|e| e.to_string())
    }

    pub fn with_trust<R>(&self, f: impl FnOnce(&TrustStore) -> R) -> R {
        f(&lock(&self.trust))
    }

    pub fn mutate_trust<R>(
        &self,
        f: impl FnOnce(&mut TrustStore) -> Result<R, String>,
    ) -> Result<R, String> {
        let mut guard = lock(&self.trust);
        let before = guard.clone();
        match f(&mut guard) {
            Ok(value) => {
                guard.save(&self.trust_path()).map_err(|e| e.to_string())?;
                Ok(value)
            }
            Err(error) => {
                *guard = before;
                Err(error)
            }
        }
    }

    pub fn save_trust(&self, store: &TrustStore) -> Result<(), String> {
        store.save(&self.trust_path()).map_err(|e| e.to_string())
    }

    pub fn diagnostics_path(&self) -> PathBuf {
        self.paths.state_dir().join("diagnostics.json")
    }

    /// 新拉起的实例是否捕获子进程输出。
    pub fn child_output(&self) -> bool {
        lock(&self.diagnostics).child_output
    }

    pub fn set_child_output(&self, enabled: bool) -> Result<bool, String> {
        let mut guard = lock(&self.diagnostics);
        let next = Diagnostics {
            child_output: enabled,
        };
        launcher_core::state::write_json(&self.diagnostics_path(), &next)
            .map_err(|e| format!("保存诊断设置失败：{e}"))?;
        *guard = next;
        Ok(enabled)
    }

    pub fn close_behavior_path(&self) -> PathBuf {
        self.paths.state_dir().join("close-behavior.json")
    }

    pub fn close_behavior(&self) -> CloseBehavior {
        *lock(&self.close_behavior)
    }

    pub fn set_close_behavior(&self, behavior: CloseBehavior) -> Result<CloseBehavior, String> {
        let mut guard = lock(&self.close_behavior);
        launcher_core::state::write_json(
            &self.close_behavior_path(),
            &ClosePolicyFile { behavior },
        )
        .map_err(|e| format!("保存关闭行为失败：{e}"))?;
        *guard = behavior;
        Ok(behavior)
    }

    pub fn is_quitting(&self) -> bool {
        self.quitting.load(Ordering::SeqCst)
    }

    pub fn begin_quit(&self) {
        self.quitting.store(true, Ordering::SeqCst);
    }

    pub fn mark_tray_ready(&self) {
        self.tray_ready.store(true, Ordering::SeqCst);
    }

    pub fn tray_ready(&self) -> bool {
        self.tray_ready.load(Ordering::SeqCst)
    }

    pub fn onboarding_path(&self) -> PathBuf {
        self.paths.state_dir().join("onboarding.json")
    }

    pub fn onboarding(&self) -> Onboarding {
        lock(&self.onboarding).clone()
    }

    /// 记下「向导已处理」。`skipped=true` 是用户点了「以后再说」——
    /// 与走完流程分开记，设置页才能如实写「已跳过」而不是「已完成」。
    pub fn complete_onboarding(&self, skipped: bool) -> Result<Onboarding, String> {
        let mut guard = lock(&self.onboarding);
        let next = Onboarding {
            schema_version: onboarding_schema(),
            completed: true,
            skipped,
            completed_at: Some(launcher_core::time::utc_now()),
        };
        launcher_core::state::write_json(&self.onboarding_path(), &next)
            .map_err(|e| format!("保存向导状态失败：{e}"))?;
        *guard = next.clone();
        Ok(next)
    }

    /// 让向导下次启动再弹一次（设置页的「重新运行首次向导」）。
    pub fn reset_onboarding(&self) -> Result<Onboarding, String> {
        let mut guard = lock(&self.onboarding);
        let next = Onboarding::default();
        launcher_core::state::write_json(&self.onboarding_path(), &next)
            .map_err(|e| format!("保存向导状态失败：{e}"))?;
        *guard = next.clone();
        Ok(next)
    }

    pub fn with_retention<R>(&self, f: impl FnOnce(&RetentionPolicy) -> R) -> R {
        f(&lock(&self.retention))
    }

    pub fn save_retention(&self, policy: &RetentionPolicy) -> Result<(), String> {
        policy
            .save(&self.retention_path())
            .map_err(|e| e.to_string())
    }

    pub fn chains_path(&self) -> PathBuf {
        self.paths.state_dir().join("chains.json")
    }

    pub fn chains(&self) -> ChainRegistry {
        lock(&self.chains).clone()
    }

    /// 改动后一次原子落盘；闭包返回 Err 时内存回滚，不写文件。
    pub fn mutate_chains<R>(
        &self,
        f: impl FnOnce(&mut ChainRegistry) -> Result<R, String>,
    ) -> Result<R, String> {
        let mut guard = lock(&self.chains);
        let before = guard.clone();
        match f(&mut guard) {
            Ok(value) => {
                guard.save(&self.chains_path()).map_err(|e| e.to_string())?;
                Ok(value)
            }
            Err(error) => {
                *guard = before;
                Err(error)
            }
        }
    }

    /// 当前实例表里所有 instance_id。链路读取时用它与 steps 比对，只在内存里标悬空。
    pub fn known_instance_ids(&self) -> BTreeSet<String> {
        lock(&self.installed).instances.keys().cloned().collect()
    }

    pub fn set_last_chain_run(&self, result: &ChainRunResult) {
        lock(&self.last_chain_runs).insert(result.chain_id.clone(), result.clone());
    }

    pub fn last_chain_run(&self, chain_id: &str) -> Option<ChainRunResult> {
        lock(&self.last_chain_runs).get(chain_id).cloned()
    }

    pub fn sources_path(&self) -> PathBuf {
        self.paths.state_dir().join("sources.json")
    }

    pub fn sources(&self) -> SourceRegistry {
        lock(&self.sources).clone()
    }

    /// 改动后一次原子落盘；闭包返回 Err 时内存回滚，不写文件。
    pub fn mutate_sources<R>(
        &self,
        f: impl FnOnce(&mut SourceRegistry) -> Result<R, String>,
    ) -> Result<R, String> {
        let mut guard = lock(&self.sources);
        let before = guard.clone();
        match f(&mut guard) {
            Ok(value) => {
                guard
                    .save(&self.sources_path())
                    .map_err(|e| e.to_string())?;
                Ok(value)
            }
            Err(error) => {
                *guard = before;
                Err(error)
            }
        }
    }

    /// 已启用源的域名后缀并集；禁用全部源即为空集，此时不发任何远程请求。
    pub fn allowlist(&self) -> Vec<String> {
        lock(&self.sources).allowlist()
    }

    pub fn cached_entries(&self) -> BTreeMap<String, CachedEntry> {
        lock(&self.catalog).clone()
    }

    pub fn cached_entry(&self, key: &str) -> Option<CachedEntry> {
        lock(&self.catalog).get(key).cloned()
    }

    pub fn replace_cached_entries(&self, entries: BTreeMap<String, CachedEntry>) {
        *lock(&self.catalog) = entries;
    }

    pub fn is_disabled(&self, module_id: &str) -> bool {
        lock(&self.disabled).contains(module_id)
    }

    pub fn set_disabled(&self, module_id: &str, disabled: bool) {
        let mut set = lock(&self.disabled);
        if disabled {
            set.insert(module_id.to_string());
        } else {
            set.remove(module_id);
        }
    }

    /// 包目录：zip 实例固定在 `instances/<key>/package`，与同级的
    /// venvs/data/logs 隔开；dir 实例直接用用户源码目录，永不复制。
    pub fn module_dir(&self, instance: &Instance) -> PathBuf {
        match instance.source_kind {
            InstanceSourceKind::Dir => instance
                .source_path
                .as_ref()
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    crate::lifecycle_ops::package_dir(&self.paths, &instance.instance_id)
                }),
            InstanceSourceKind::Zip => {
                crate::lifecycle_ops::package_dir(&self.paths, &instance.instance_id)
            }
        }
    }

    /// 实例实际使用的 module-v2 清单。
    ///
    /// 默认就是模块根里的 `manifest.json`。导入期把旧版 v1 清单补成 v2 时，
    /// 生成物落在数据根，源码目录一个字节都不改，运行时改读这份生成物。
    pub fn manifest_file(&self, instance: &Instance) -> PathBuf {
        match &instance.manifest_path {
            Some(path) => PathBuf::from(path),
            None => self
                .module_dir(instance)
                .join(crate::lifecycle_ops::MANIFEST_FILE),
        }
    }

    pub fn instances_for_module(&self, module_id: &str) -> Vec<Instance> {
        self.with_installed(|state| {
            state
                .instances
                .values()
                .filter(|i| i.module_id == module_id)
                .cloned()
                .collect()
        })
    }

    pub fn instance(&self, instance_id: &str) -> Option<Instance> {
        self.with_installed(|state| state.instances.get(instance_id).cloned())
    }

    /// v1 命令按 module_id 调用，v2 界面按 instance_id 调用；两种键都认。
    ///
    /// 先按 instance_id 精确匹配，再退回 module_id：active 槽优先，其次按
    /// instance_id 稳定取第一个。这样「启动某个实例」与「启动某模块」不会
    /// 各自实现一套解析，界面传 instance_id 也不会被当成模块名判「未安装」。
    pub fn resolve_instance(&self, key: &str) -> Result<Instance, String> {
        let state = lock(&self.installed);
        if let Some(exact) = state.instances.get(key) {
            return Ok(exact.clone());
        }
        let mut matching: Vec<&Instance> = state
            .instances
            .values()
            .filter(|i| i.module_id == key)
            .collect();
        if matching.is_empty() {
            return Err(format!("没有找到实例或模块：{key}"));
        }
        matching.sort_by(|a, b| a.instance_id.cmp(&b.instance_id));
        let active = state
            .active
            .values()
            .find_map(|id| matching.iter().find(|i| i.instance_id == *id).copied());
        active
            .or_else(|| matching.first().copied())
            .cloned()
            .ok_or_else(|| format!("模块 {key} 没有可用实例"))
    }

    /// 登记或更新实例，落盘一次原子写。
    pub fn upsert(&self, instance: Instance) -> Result<(), String> {
        let mut state = lock(&self.installed);
        state
            .upsert(instance)
            .map_err(|e| format!("实例表不合法：{e}"))?;
        state
            .save(&self.installed_path())
            .map_err(|e| e.to_string())
    }

    pub fn remove_instance(&self, instance_id: &str) -> Result<Option<Instance>, String> {
        let mut state = lock(&self.installed);
        let removed = state.instances.remove(instance_id);
        if removed.is_some() {
            state.active.retain(|_, active_id| active_id != instance_id);
            state
                .save(&self.installed_path())
                .map_err(|e| e.to_string())?;
        }
        Ok(removed)
    }

    pub fn now() -> String {
        utc_now()
    }
}

fn load_or<T, F>(path: &Path, create: F) -> Result<T, String>
where
    T: serde::de::DeserializeOwned + serde::Serialize,
    F: FnOnce() -> Result<T, String>,
{
    if path.exists() {
        return launcher_core::state::read_json(path).map_err(|e| e.to_string());
    }
    let value = create()?;
    launcher_core::state::write_json_once(path, &value).map_err(|e| e.to_string())?;
    Ok(value)
}

/// 把 catalog 的 `sha256:` 前缀哈希归一化为下载器使用的裸 hex。
pub fn bare_sha256(value: &str) -> String {
    value.strip_prefix("sha256:").unwrap_or(value).to_string()
}

/// 把下载器/前端给的裸 hex 补成 core 期望的 `sha256:` 前缀格式。
pub fn prefixed_sha256(value: &str) -> String {
    if value.starts_with("sha256:") {
        value.to_string()
    } else {
        format!("sha256:{value}")
    }
}

/// M2 错误统一转成命令面的可读字符串。
pub fn m2_err(error: M2Error) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_keys_match_their_pubkeys_and_stay_separate() {
        assert_eq!(
            launcher_core::m2::fingerprint(BUILTIN_K1_PUBKEY_B64).unwrap(),
            BUILTIN_K1_FINGERPRINT
        );
        assert_eq!(
            launcher_core::m2::fingerprint(BUILTIN_K5_PUBKEY_B64).unwrap(),
            BUILTIN_K5_FINGERPRINT
        );
        // 两钥分离是设计约束，不是巧合；同一把钥匙签包又签 catalog 会让
        // K1 泄露方有能力伪造一份不含 revoked 的索引。
        assert_ne!(BUILTIN_K1_PUBKEY_B64, BUILTIN_K5_PUBKEY_B64);
        assert_ne!(BUILTIN_K1_FINGERPRINT, BUILTIN_K5_FINGERPRINT);
    }

    /// 默认「收进托盘」是产品决定：关窗不该按 Windows 习惯直接杀掉实例。
    /// 三个取值都要能落盘再读回来，否则重启后用户的显式选择会退回默认。
    #[test]
    fn close_behavior_defaults_to_tray_and_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("app");
        let data = dir.path().join("data");
        let state = AppStateV2::new(root, data.clone()).unwrap();
        assert_eq!(state.close_behavior(), CloseBehavior::Tray);
        assert!(!state.tray_ready());
        state.mark_tray_ready();
        assert!(state.tray_ready());

        for value in [CloseBehavior::Quit, CloseBehavior::Ask, CloseBehavior::Tray] {
            assert_eq!(state.set_close_behavior(value).unwrap(), value);
            let reopened = AppStateV2::new(dir.path().join("app"), data.clone()).unwrap();
            assert_eq!(reopened.close_behavior(), value);
        }
        // 磁盘上存的是契约里的字符串，不是枚举的 Debug 形状。
        let raw = std::fs::read_to_string(state.close_behavior_path()).unwrap();
        assert!(raw.contains("\"tray\""), "{raw}");
    }

    /// 回归：旧的 sources.json 里残存被撤销的 GitHub 镜像时，state 加载必须
    /// 剔除它并写回清理，下载页才不会再显示这条已经不打算下发的源。
    #[test]
    fn stale_hidden_github_mirror_is_dropped_and_cleaned_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("app");
        let data = dir.path().join("data");
        let state = AppStateV2::new(root.clone(), data).unwrap();
        let path = state.sources_path();

        let mut stale = state.sources();
        let sf = stale.sources[0].clone();
        stale.sources.push(SourceEntry {
            id: launcher_core::m2::sources::OFFICIAL_GH_ID.into(),
            name: "官方镜像（GitHub）".into(),
            kind: SourceClass::Official,
            base_url: "https://raw.githubusercontent.com/x/main".into(),
            catalog_pubkey: sf.catalog_pubkey.clone(),
            catalog_fingerprint: sf.catalog_fingerprint.clone(),
            host_suffixes: vec!["github.com".into(), "githubusercontent.com".into()],
            enabled: false,
            deletable: false,
            auto_update: true,
            group: sf.group.clone(),
            sequence: None,
            expires: None,
            priority: Some(2),
            added_at: None,
            revoked_at: Some("2026-11-14T12:10:27Z".into()),
            note: None,
            last_error: None,
            last_error_at: None,
        });
        stale.save(&path).unwrap();
        let on_disk_before: SourceRegistry = launcher_core::state::read_json(&path).unwrap();
        assert_eq!(on_disk_before.sources.len(), 3, "夹具确实写入了隐藏源");

        let reopened = AppStateV2::new(root, dir.path().join("data")).unwrap();
        let loaded = reopened.sources();
        let ids: Vec<&str> = loaded.sources.iter().map(|s| s.id.as_str()).collect();
        // 隐藏镜像被剔除；nightly 作为内置源补齐后与官方源并存。
        assert_eq!(
            ids,
            vec![
                launcher_core::m2::sources::OFFICIAL_SF_ID,
                launcher_core::m2::sources::NIGHTLY_GH_ID,
            ]
        );
        // 清理已落盘：直接读文件也不再有这条隐藏源。
        let on_disk: SourceRegistry =
            launcher_core::state::read_json(&reopened.sources_path()).unwrap();
        assert_eq!(on_disk.sources.len(), 2);
        assert!(!on_disk
            .sources
            .iter()
            .any(|s| s.id == launcher_core::m2::sources::OFFICIAL_GH_ID));
    }

    #[test]
    fn state_files_are_created_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("app");
        let data = dir.path().join("data");
        let state = AppStateV2::new(root, data.clone()).unwrap();
        assert!(state.installed_path().exists());
        assert!(state.trust_path().exists());
        assert!(state.retention_path().exists());
        let reopened = AppStateV2::new(dir.path().join("app"), data).unwrap();
        assert!(reopened.with_installed(|s| s.instances.is_empty()));
    }

    #[test]
    fn install_state_is_bootstrapped_and_self_heals() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("app");
        let data = dir.path().join("data");

        let first = AppStateV2::new(root.clone(), data.clone()).unwrap();
        let note = first
            .install_note
            .clone()
            .expect("首次启动必须补写 install.json");
        assert!(note.contains("缺失"), "说明应指出是缺失而非升级：{note}");
        let path = first.paths.install_state_path();
        let loaded = launcher_core::install::InstallState::load(&first.paths).unwrap();
        assert_eq!(loaded.uv_sha256, launcher_core::env::bootstrap::UV_SHA256);
        assert_eq!(loaded.uv_version, launcher_core::env::bootstrap::UV_VERSION);
        assert_eq!(loaded.uv_path, first.paths.bundled_uv_path());

        // 删掉再启动：自愈。这是 M1 验收第 7 条。
        std::fs::remove_file(&path).unwrap();
        let second = AppStateV2::new(root, data).unwrap();
        assert!(second.install_note.is_some());
        assert!(path.exists());
    }

    /// 内置哈希必须与 `bin/uv.exe` 这个实际产物对得上，否则启动器会自己把
    /// 「随包 uv 被篡改」判成真的。仓库里没有该文件时（未跑过下载步骤）跳过。
    #[test]
    fn pinned_uv_hash_matches_the_staged_binary() {
        let bundled = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("bin")
            .join("uv.exe");
        if !bundled.exists() {
            return;
        }
        let meta = std::fs::metadata(&bundled).unwrap();
        assert_eq!(meta.len(), launcher_core::env::bootstrap::UV_BYTES);
        assert_eq!(
            launcher_core::file_sha256_prefixed(&bundled).unwrap(),
            launcher_core::env::bootstrap::UV_SHA256
        );
    }

    #[test]
    fn instance_slots_are_shared_per_instance_and_independent_across() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppStateV2::new(dir.path().join("app"), dir.path().join("data")).unwrap();
        let first = state.instance_slot("a");
        let second = state.instance_slot("a");
        let other = state.instance_slot("b");
        assert!(Arc::ptr_eq(&first, &second));
        assert!(!Arc::ptr_eq(&first, &other));

        let held = first.lock().unwrap();
        // 同实例第二次进入必须排队；不同实例不受影响。
        assert!(second.try_lock().is_err());
        assert!(other.try_lock().is_ok());
        drop(held);
        assert!(second.try_lock().is_ok());
    }

    #[test]
    fn launcher_logs_use_data_root_and_instance_logs_stay_separate() {
        let dir = tempfile::tempdir().unwrap();
        let data_root = dir.path().join("data");
        let state = AppStateV2::new(dir.path().join("app"), data_root.clone()).unwrap();

        assert_eq!(state.logs_dir_for(LAUNCHER_LOG_ID), data_root.join("logs"));
        let instance_dir = state.logs_dir_for("dounai/moodvoice@stable");
        assert!(instance_dir.starts_with(data_root.join("instances")));
        assert_ne!(instance_dir, state.app_log_dir());

        // 写一行启动器日志，必须落在 data_root/logs 且能被读回。
        state.app_log_line("env", "开始装依赖 instance=test");
        let files = diag_log::list_logs(&state.app_log_dir());
        assert_eq!(files.len(), 1, "启动器日志应落一份文件");
        let tail = diag_log::read_tail(&state.app_log_dir(), 20);
        assert!(tail.lines.iter().any(|line| line.contains("开始装依赖")));
    }

    #[test]
    fn hash_normalisation_is_reversible() {
        let hex = "a".repeat(64);
        assert_eq!(bare_sha256(&prefixed_sha256(&hex)), hex);
        assert_eq!(prefixed_sha256("sha256:abc"), "sha256:abc");
    }

    /// 向导状态必须落盘：只在内存里的话，用户跳过之后再启动还会被弹一次。
    #[test]
    fn onboarding_completion_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let install_root = dir.path().join("app");
        let data_root = dir.path().join("data");

        let state = AppStateV2::new(install_root.clone(), data_root.clone()).unwrap();
        assert!(!state.onboarding().completed, "新机器不该是已走过");

        state.complete_onboarding(true).unwrap();
        let reopened = AppStateV2::new(install_root.clone(), data_root.clone()).unwrap();
        let saved = reopened.onboarding();
        assert!(saved.completed);
        assert!(saved.skipped);
        assert!(saved.completed_at.is_some());

        // 重开向导：回到未完成，下次启动会再弹。
        reopened.reset_onboarding().unwrap();
        let again = AppStateV2::new(install_root, data_root).unwrap();
        assert!(!again.onboarding().completed);
    }
}
