//! M3 命令面的共享状态：双根路径、实例表、信任表与保留策略。
//!
//! v1 的 `AppState` 把模块状态、进程表与信任策略揉在一起，M3 只保留
//! `launcher-core` 的 v2 状态文件（installed/trust/retention），进程表
//! 由 `lifecycle_runtime::Supervisor` 独立持有。运行态不落盘。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use launcher_core::m2::catalog::CatalogDocument;
use launcher_core::m2::installed::{InstalledState, Instance};
use launcher_core::m2::retention::RetentionPolicy;
use launcher_core::m2::trust::TrustStore;
use launcher_core::m2::{InstanceSourceKind, M2Error};
use launcher_core::paths::{InstallMode, Paths};
use launcher_core::time::utc_now;

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
/// catalog 的签名段不含 `pubkey`，启动器只能拿下面这枚内置公钥验签；信任判定
/// 比对同一枚公钥算出的指纹。`set_k5_pubkey` 可在本次会话内替换公钥，
/// 重启回落到内置值。
pub const BUILTIN_K5_PUBKEY_B64: &str = "+56W2k6Sw60cMspmEKUESgqX1TGJLqmVozPJvI0n8G8=";
pub const BUILTIN_K5_FINGERPRINT: &str =
    "sha256:8c1d31cf6f5ce98961be579f88fe49f8521242bcaa07483e85d0bbea1532eff5";

/// 允许的出网域名后缀集，与 `m2::source` 的双源白名单一致。
pub fn allowed_hosts() -> Vec<String> {
    let mut hosts: Vec<String> = launcher_core::m2::source::GITHUB_HOST_SUFFIXES
        .iter()
        .chain(launcher_core::m2::source::SOURCEFORGE_HOST_SUFFIXES.iter())
        .map(|s| (*s).to_string())
        .collect();
    hosts.sort();
    hosts.dedup();
    hosts
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct AppStateV2 {
    pub paths: Paths,
    installed: Mutex<InstalledState>,
    trust: Mutex<TrustStore>,
    retention: Mutex<RetentionPolicy>,
    catalog: Mutex<Option<CatalogDocument>>,
    /// v1 界面的启用开关是内存态：M0 冻结的 installed.json 没有 enabled 字段，
    /// M3 不新增状态文件。M4 界面重做时按实例粒度重新定义。
    disabled: Mutex<BTreeSet<String>>,
    pub dev_mode: AtomicBool,
    k5_pubkey_b64: Mutex<String>,
    /// 启动时补写/校正 `install.json` 的说明；`None` 表示本来就是一致的。
    /// 设置页直接展示，用户能看出「安装器没跑，是启动器兜的」。
    pub install_note: Option<String>,
    /// 每实例一把互斥锁：依赖准备、卸载与清理共用，避免 `.staging` 并发互删。
    instance_locks: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
}

impl AppStateV2 {
    pub fn new(install_root: PathBuf, data_root: PathBuf) -> Result<Self, String> {
        let paths = Paths::new(InstallMode::CurrentUser, install_root, data_root)
            .map_err(|e| format!("双根路径不合法：{e}"))?;
        for dir in [
            paths.state_dir(),
            paths.data_root.join("instances"),
            paths.data_root.join("downloads"),
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
        let trust = load_or(&paths.state_dir().join("trust.json"), || {
            TrustStore::new(BUILTIN_K1_FINGERPRINT, BUILTIN_K5_FINGERPRINT)
                .map_err(|e| e.to_string())
        })?;
        let retention = load_or(&paths.state_dir().join("retention.json"), || {
            Ok(RetentionPolicy::default())
        })?;
        Ok(Self {
            paths,
            installed: Mutex::new(installed),
            trust: Mutex::new(trust),
            retention: Mutex::new(retention),
            catalog: Mutex::new(None),
            disabled: Mutex::new(BTreeSet::new()),
            dev_mode: AtomicBool::new(false),
            k5_pubkey_b64: Mutex::new(BUILTIN_K5_PUBKEY_B64.to_string()),
            install_note,
            instance_locks: Mutex::new(BTreeMap::new()),
        })
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

    pub fn with_retention<R>(&self, f: impl FnOnce(&RetentionPolicy) -> R) -> R {
        f(&lock(&self.retention))
    }

    pub fn save_retention(&self, policy: &RetentionPolicy) -> Result<(), String> {
        policy
            .save(&self.retention_path())
            .map_err(|e| e.to_string())
    }

    pub fn k5_pubkey(&self) -> String {
        lock(&self.k5_pubkey_b64).clone()
    }

    pub fn set_k5_pubkey(&self, pubkey_b64: String) {
        *lock(&self.k5_pubkey_b64) = pubkey_b64;
    }

    pub fn cached_catalog(&self) -> Option<CatalogDocument> {
        lock(&self.catalog).clone()
    }

    pub fn set_cached_catalog(&self, doc: Option<CatalogDocument>) {
        *lock(&self.catalog) = doc;
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
    fn hash_normalisation_is_reversible() {
        let hex = "a".repeat(64);
        assert_eq!(bare_sha256(&prefixed_sha256(&hex)), hex);
        assert_eq!(prefixed_sha256("sha256:abc"), "sha256:abc");
    }
}
