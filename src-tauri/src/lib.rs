//! 启动器桌面壳：Tauri 命令面与进程装配。
//!
//! 纯 Rust 逻辑在 `launcher-core`，含验签、路径安全、双根路径与管理页宿主契约。
//! 本 crate 的命令面只校验参数与转换结果，不放业务逻辑。
//! 其余模块按里程碑逐步迁入 `launcher-core`。

pub mod catalog;
pub mod commands;
pub mod download;
pub mod env_probe;
pub mod http_client;
pub mod install;
pub mod lifecycle_ops;
pub mod lifecycle_runtime;
pub mod m2_http;
pub mod module_probe;
pub mod process;
pub mod release;
pub mod runtime;
pub mod state;
pub mod state_v2;
pub mod trust;

// 供应链安全、路径与管理页宿主契约住在 launcher-core。
// 重导出模块名，使 crate 内既有的 `crate::security` / `crate::signature`
// 路径与外部的 `dounai_launcher::security` 路径继续可用。
pub use launcher_core::{admin_host, paths, security, signature};

pub use catalog::{
    fetch_catalog, parse_catalog, Allowlist, AllowlistError, CatalogEntry, CatalogFetcher,
    CatalogResult,
};
pub use download::{download, DownloadError, DownloadOptions, DownloadReport, HttpFetcher};
pub use install::{extract_zip_into, InstallError, InstallReport, InstallTransaction};
pub use launcher_core::security::{
    is_safe_filename, safe_join, sha256_hex, verify_sha256, PathError,
};
pub use launcher_core::signature::{
    canonical_payload, verify_manifest, SignatureError, SUPPORTED_ALGO,
};
pub use runtime::{
    check_working_dir, resolve_placeholders, split_command, EntrySpec, ProcessManager, RunView,
    StartError,
};
pub use state::{Category, InstallState, ModuleState, RunState, StateError, StateStore};
pub use trust::{TrustError, TrustPolicy, TrustVerdict};
