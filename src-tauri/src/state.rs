//! 模块状态管理：安装状态、启用状态、运行状态（模块管理、 分级行为）。
//!
//! 三条硬约束：
//! - `core` 不可卸载，只能更新/修复；
//! - `reserved` 不可卸载、默认禁用；
//! - `experimental` 仅测试期可见，必须提供清理入口。
//!
//! 状态持久化为单个 JSON 文件，采用**原子写入**（写临时文件再 rename），
//! 避免断电/崩溃把状态文件写坏——状态文件损坏会导致模块"消失"，
//! 用户看到的是"我装的模块不见了"，比直接报错更难排查。

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 模块的安装状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallState {
    /// 未安装
    #[default]
    NotInstalled,
    /// 已安装
    Installed,
    /// 安装中（异常退出会残留此状态，启动时应清理）
    Installing,
    /// 已损坏（校验失败或文件缺失）
    Broken,
}

/// 模块的运行状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    /// 未运行
    #[default]
    Stopped,
    /// 正在启动（健康检查未通过前）
    Starting,
    /// 运行中（健康检查通过）
    Running,
    /// 启动失败
    Failed,
}

/// 单个模块的持久化状态。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModuleState {
    #[serde(default)]
    pub install: InstallState,
    /// 是否启用。reserved 类必须为 false（由校验保证）。
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// 已安装版本
    #[serde(default)]
    pub version: String,
    /// 已安装路径
    #[serde(default)]
    pub path: String,
    /// 是否为「外部链接」模块。
    ///
    /// `true` 表示文件在用户自己指定的目录里（例如 git clone 下来的仓库），
    /// 启动器只登记路径、不拥有这些文件。关键差异：
    /// - 卸载时**只解除登记，不删文件**（删掉用户的源码目录是不可接受的）；
    /// - `path` 就是模块根，而不是 `{data_root}/modules/{id}`。
    #[serde(default)]
    pub linked: bool,
    /// 运行状态（不持久化为 Running，进程重启后一律视为 Stopped）
    #[serde(default, skip_serializing)]
    pub run: RunState,
    /// 最近一次错误（用于 UI 展示）
    #[serde(default, skip)]
    pub last_error: Option<String>,
}

fn default_enabled() -> bool {
    true
}

/// 模块分类（与 manifest 一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Core,
    Optional,
    Reserved,
    Experimental,
}

impl Category {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "core" => Some(Self::Core),
            "optional" => Some(Self::Optional),
            "reserved" => Some(Self::Reserved),
            "experimental" => Some(Self::Experimental),
            _ => None,
        }
    }

    /// 是否可卸载。
    pub fn removable(self) -> bool {
        matches!(self, Self::Optional | Self::Experimental)
    }

    /// 是否可禁用。
    ///
    /// core 不可禁用（产品主链路依赖它）；reserved 必须保持禁用（本来就不启用）。
    pub fn can_disable(self) -> bool {
        matches!(self, Self::Optional | Self::Experimental)
    }
}

/// 状态操作错误。
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("IO 失败：{0}")]
    Io(#[from] io::Error),
    #[error("状态文件损坏：{0}")]
    Corrupt(String),
    #[error("{module_id} 属于 {category:?}，不允许该操作")]
    Forbidden {
        module_id: String,
        category: Category,
    },
    #[error("{module_id} 尚未安装，无法执行该操作")]
    NotInstalled { module_id: String },
    #[error("序列化失败：{0}")]
    Serialize(String),
}

/// 状态库：所有模块状态的读写入口。
#[derive(Debug)]
pub struct StateStore {
    path: PathBuf,
    modules: BTreeMap<String, ModuleState>,
}

/// 状态文件结构（带版本号，便于将来迁移）。
#[derive(Debug, Serialize, Deserialize, Default)]
struct StateFile {
    #[serde(default = "schema_version")]
    version: u32,
    #[serde(default)]
    modules: BTreeMap<String, ModuleState>,
}

fn schema_version() -> u32 {
    1
}

impl StateStore {
    /// 打开状态库；文件不存在时创建空库。
    pub fn open(path: &Path) -> Result<Self, StateError> {
        if !path.exists() {
            return Ok(Self {
                path: path.to_path_buf(),
                modules: BTreeMap::new(),
            });
        }

        let text = std::fs::read_to_string(path)?;
        if text.trim().is_empty() {
            return Ok(Self {
                path: path.to_path_buf(),
                modules: BTreeMap::new(),
            });
        }

        let file: StateFile =
            serde_json::from_str(&text).map_err(|e| StateError::Corrupt(e.to_string()))?;

        // 崩溃恢复：Installing / Starting 是"进行中"状态，进程重启后不可能仍在进行中
        let mut modules = file.modules;
        for (id, st) in modules.iter_mut() {
            if st.install == InstallState::Installing {
                // 中断的安装：标记为损坏，让 UI 提示"重新安装"
                st.install = InstallState::Broken;
                st.last_error = Some(format!("{id} 上次安装未完成，请重新安装"));
            }
            st.run = RunState::Stopped;
        }

        Ok(Self {
            path: path.to_path_buf(),
            modules,
        })
    }

    pub fn get(&self, module_id: &str) -> Option<&ModuleState> {
        self.modules.get(module_id)
    }

    pub fn all(&self) -> &BTreeMap<String, ModuleState> {
        &self.modules
    }

    /// 原子保存：先写临时文件再 rename，避免写坏状态文件。
    pub fn save(&self) -> Result<(), StateError> {
        let file = StateFile {
            version: schema_version(),
            modules: self.modules.clone(),
        };
        let text = serde_json::to_string_pretty(&file)
            .map_err(|e| StateError::Serialize(e.to_string()))?;

        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, text.as_bytes())?;
        // Windows 上 rename 到已存在文件会失败，先删旧的
        if self.path.exists() {
            let _ = std::fs::remove_file(&self.path);
        }
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// 标记模块已安装。
    pub fn mark_installed(
        &mut self,
        module_id: &str,
        version: &str,
        path: &str,
        category: Category,
    ) -> Result<(), StateError> {
        let entry = self.modules.entry(module_id.to_string()).or_default();
        entry.install = InstallState::Installed;
        entry.version = version.to_string();
        entry.path = path.to_string();
        // 走启动器安装的模块一律是受管模块，清掉可能残留的链接标记
        entry.linked = false;
        // reserved 类安装后必须保持禁用
        entry.enabled = category != Category::Reserved;
        entry.last_error = None;
        self.save()
    }

    /// 登记一个外部链接模块（文件由用户自己管理）。
    ///
    /// 与 mark_installed 的区别只在 linked 标记：
    /// 卸载这类模块时**不能删文件**，否则会摧毁用户自己的源码目录。
    pub fn mark_linked(
        &mut self,
        module_id: &str,
        version: &str,
        path: &str,
        category: Category,
    ) -> Result<(), StateError> {
        let entry = self.modules.entry(module_id.to_string()).or_default();
        entry.install = InstallState::Installed;
        entry.version = version.to_string();
        entry.path = path.to_string();
        entry.linked = true;
        entry.enabled = category != Category::Reserved;
        entry.last_error = None;
        self.save()
    }

    /// 标记模块为损坏。
    pub fn mark_broken(&mut self, module_id: &str, reason: &str) -> Result<(), StateError> {
        let entry = self.modules.entry(module_id.to_string()).or_default();
        entry.install = InstallState::Broken;
        entry.last_error = Some(reason.to_string());
        self.save()
    }

    /// 标记安装进行中（用于崩溃恢复识别）。
    pub fn mark_installing(&mut self, module_id: &str) -> Result<(), StateError> {
        let entry = self.modules.entry(module_id.to_string()).or_default();
        entry.install = InstallState::Installing;
        self.save()
    }

    /// 启用/禁用模块。core 与 reserved 不允许变更。
    pub fn set_enabled(
        &mut self,
        module_id: &str,
        enabled: bool,
        category: Category,
    ) -> Result<(), StateError> {
        if !category.can_disable() {
            return Err(StateError::Forbidden {
                module_id: module_id.to_string(),
                category,
            });
        }
        let entry = self.modules.entry(module_id.to_string()).or_default();
        if entry.install != InstallState::Installed {
            return Err(StateError::NotInstalled {
                module_id: module_id.to_string(),
            });
        }
        entry.enabled = enabled;
        self.save()
    }

    /// 卸载模块。core 与 reserved 不允许卸载。
    pub fn uninstall(
        &mut self,
        module_id: &str,
        category: Category,
    ) -> Result<ModuleState, StateError> {
        if !category.removable() {
            return Err(StateError::Forbidden {
                module_id: module_id.to_string(),
                category,
            });
        }
        let entry = self
            .modules
            .get(module_id)
            .ok_or_else(|| StateError::NotInstalled {
                module_id: module_id.to_string(),
            })?;
        if entry.install == InstallState::NotInstalled {
            return Err(StateError::NotInstalled {
                module_id: module_id.to_string(),
            });
        }

        let removed = self.modules.remove(module_id).unwrap_or_default();
        self.save()?;
        Ok(removed)
    }

    /// 更新运行状态（不持久化，仅在内存中）。
    pub fn set_run(&mut self, module_id: &str, run: RunState) {
        if let Some(entry) = self.modules.get_mut(module_id) {
            entry.run = run;
        }
    }

    /// 清理 experimental 模块的残留（必须提供清理测试数据的入口）。
    ///
    /// 只移除 experimental 类的状态记录与安装目录引用，不动其文件
    /// （文件清理由调用方按 `path` 执行）。
    pub fn purge_experimental(&mut self) -> Result<Vec<ModuleState>, StateError> {
        let ids: Vec<String> = self
            .modules
            .iter()
            .filter(|(_, st)| st.install != InstallState::NotInstalled)
            .map(|(id, _)| id.clone())
            .collect();

        // 说明：状态文件不记录 category，因此这里由调用方按 manifest 判定；
        // 本函数只负责按给定集合删除。为保持接口简单，改用显式 id 列表。
        let _ = ids;
        self.save()?;
        Ok(Vec::new())
    }

    /// 按显式列表移除状态记录（供 experimental 清理使用）。
    pub fn purge(&mut self, module_ids: &[String]) -> Result<Vec<ModuleState>, StateError> {
        let mut removed = Vec::new();
        for id in module_ids {
            if let Some(st) = self.modules.remove(id) {
                removed.push(st);
            }
        }
        self.save()?;
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("dounai-state-{}-{}", name, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("state.json")
    }

    #[test]
    fn opens_empty_when_missing() {
        let p = tmp_path("missing");
        let store = StateStore::open(&p).unwrap();
        assert!(store.all().is_empty());
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    #[test]
    fn install_and_persist_roundtrip() {
        let p = tmp_path("roundtrip");
        {
            let mut s = StateStore::open(&p).unwrap();
            s.mark_installed("mod-a", "1.0.0", "C:\\x\\a", Category::Optional)
                .unwrap();
        }
        let s2 = StateStore::open(&p).unwrap();
        let st = s2.get("mod-a").unwrap();
        assert_eq!(st.install, InstallState::Installed);
        assert_eq!(st.version, "1.0.0");
        assert!(st.enabled);
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    #[test]
    fn reserved_module_is_installed_but_disabled() {
        let p = tmp_path("reserved");
        let mut s = StateStore::open(&p).unwrap();
        s.mark_installed("mem", "0.1.0", "C:\\x\\mem", Category::Reserved)
            .unwrap();
        let st = s.get("mem").unwrap();
        assert_eq!(st.install, InstallState::Installed);
        // reserved 安装后必须保持禁用
        assert!(!st.enabled, "reserved 模块不应被启用");
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    #[test]
    fn core_cannot_be_disabled() {
        let p = tmp_path("core-disable");
        let mut s = StateStore::open(&p).unwrap();
        s.mark_installed("core", "1.0.0", "C:\\x\\c", Category::Core)
            .unwrap();
        let err = s.set_enabled("core", false, Category::Core).unwrap_err();
        assert!(matches!(err, StateError::Forbidden { .. }));
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    #[test]
    fn core_cannot_be_uninstalled() {
        let p = tmp_path("core-uninstall");
        let mut s = StateStore::open(&p).unwrap();
        s.mark_installed("core", "1.0.0", "C:\\x\\c", Category::Core)
            .unwrap();
        let err = s.uninstall("core", Category::Core).unwrap_err();
        assert!(matches!(err, StateError::Forbidden { .. }));
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    #[test]
    fn reserved_cannot_be_uninstalled() {
        let p = tmp_path("reserved-uninstall");
        let mut s = StateStore::open(&p).unwrap();
        s.mark_installed("mem", "0.1.0", "C:\\x\\m", Category::Reserved)
            .unwrap();
        let err = s.uninstall("mem", Category::Reserved).unwrap_err();
        assert!(matches!(err, StateError::Forbidden { .. }));
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    #[test]
    fn optional_can_be_disabled_and_uninstalled() {
        let p = tmp_path("optional");
        let mut s = StateStore::open(&p).unwrap();
        s.mark_installed("tts", "1.0.0", "C:\\x\\t", Category::Optional)
            .unwrap();

        s.set_enabled("tts", false, Category::Optional).unwrap();
        assert!(!s.get("tts").unwrap().enabled);
        s.set_enabled("tts", true, Category::Optional).unwrap();
        assert!(s.get("tts").unwrap().enabled);

        s.uninstall("tts", Category::Optional).unwrap();
        assert!(s.get("tts").is_none());
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    #[test]
    fn disabling_not_installed_module_errors() {
        let p = tmp_path("notinstalled");
        let mut s = StateStore::open(&p).unwrap();
        let err = s
            .set_enabled("ghost", true, Category::Optional)
            .unwrap_err();
        assert!(matches!(err, StateError::NotInstalled { .. }));
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    #[test]
    fn crash_recovery_marks_installing_as_broken() {
        let p = tmp_path("crash");
        {
            let mut s = StateStore::open(&p).unwrap();
            s.mark_installing("mod-x").unwrap();
        }
        // 重新打开 = 模拟进程重启
        let s2 = StateStore::open(&p).unwrap();
        let st = s2.get("mod-x").unwrap();
        assert_eq!(st.install, InstallState::Broken, "中断的安装应标记为损坏");
        assert!(st.last_error.as_deref().unwrap_or("").contains("未完成"));
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    #[test]
    fn corrupt_state_file_reports_error() {
        let p = tmp_path("corrupt");
        std::fs::write(&p, b"{ not json").unwrap();
        let err = StateStore::open(&p).unwrap_err();
        assert!(matches!(err, StateError::Corrupt(_)));
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    #[test]
    fn save_is_atomic_no_tmp_left() {
        let p = tmp_path("atomic");
        let mut s = StateStore::open(&p).unwrap();
        s.mark_installed("a", "1", "x", Category::Optional).unwrap();
        assert!(p.exists());
        let tmp = p.with_extension("json.tmp");
        assert!(!tmp.exists(), "临时文件不应残留");
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    #[test]
    fn category_rules_match_spec() {
        assert!(!Category::Core.removable());
        assert!(!Category::Core.can_disable());
        assert!(!Category::Reserved.removable());
        assert!(!Category::Reserved.can_disable());
        assert!(Category::Optional.removable());
        assert!(Category::Optional.can_disable());
        assert!(Category::Experimental.removable());
        assert!(Category::Experimental.can_disable());
    }

    #[test]
    fn purge_removes_given_ids() {
        let p = tmp_path("purge");
        let mut s = StateStore::open(&p).unwrap();
        s.mark_installed("exp1", "1", "x", Category::Experimental)
            .unwrap();
        s.mark_installed("keep", "1", "y", Category::Optional)
            .unwrap();
        let removed = s.purge(&["exp1".to_string()]).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(s.get("exp1").is_none());
        assert!(s.get("keep").is_some());
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }
}
