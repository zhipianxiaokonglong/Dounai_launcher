//! M3 命令面的共享状态：双根路径、实例表、信任表与保留策略。
//!
//! v1 的 `AppState` 把模块状态、进程表与信任策略揉在一起，M3 只保留
//! `launcher-core` 的 v2 状态文件（installed/trust/retention），进程表
//! 由 `lifecycle_runtime::Supervisor` 独立持有。运行态不落盘。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

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

/// K5 是 catalog 签名钥，必须与 K1 分离。这里仍是编译期占位值。
/// catalog 的签名段不含 `pubkey`，启动器只能拿内置公钥验签；占位公钥不是
/// 曲线上的 Ed25519 公钥，因此远程 catalog 一律验签失败（fail-closed），
/// 不会伪造「官方已验证」。`register_trusted_key` 可在本次会话内替换，
/// 重启回落到占位值。真正启用远程分发前必须生成 K5 并替换这两项。
pub const BUILTIN_K5_FINGERPRINT: &str =
    "sha256:1111111111111111111111111111111111111111111111111111111111111111";
/// 32 个零字节的 base64；不是曲线上的 Ed25519 公钥，验签必然失败。
pub const BUILTIN_K5_PUBKEY_B64: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

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
        })
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

    pub fn catalog_cache_path(&self) -> PathBuf {
        self.paths.state_dir().join("catalog-cache.json")
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

    /// v1 命令按 module_id 调用；优先 active 槽，其次按 instance_id 稳定取第一个。
    pub fn resolve_instance(&self, module_id: &str) -> Result<Instance, String> {
        let state = lock(&self.installed);
        let mut matching: Vec<&Instance> = state
            .instances
            .values()
            .filter(|i| i.module_id == module_id)
            .collect();
        if matching.is_empty() {
            return Err(format!("模块 {module_id} 未安装"));
        }
        matching.sort_by(|a, b| a.instance_id.cmp(&b.instance_id));
        let active = state
            .active
            .values()
            .find_map(|id| matching.iter().find(|i| i.instance_id == *id).copied());
        active
            .or_else(|| matching.first().copied())
            .cloned()
            .ok_or_else(|| format!("模块 {module_id} 没有可用实例"))
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
    fn builtin_k1_matches_its_pubkey_and_k5_stays_a_placeholder() {
        assert_eq!(
            launcher_core::m2::fingerprint(BUILTIN_K1_PUBKEY_B64).unwrap(),
            BUILTIN_K1_FINGERPRINT
        );
        assert!(launcher_core::m2::fingerprint(BUILTIN_K5_PUBKEY_B64).is_ok());
        assert_ne!(
            launcher_core::m2::fingerprint(BUILTIN_K5_PUBKEY_B64).unwrap(),
            BUILTIN_K5_FINGERPRINT
        );
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
    fn hash_normalisation_is_reversible() {
        let hex = "a".repeat(64);
        assert_eq!(bare_sha256(&prefixed_sha256(&hex)), hex);
        assert_eq!(prefixed_sha256("sha256:abc"), "sha256:abc");
    }
}
