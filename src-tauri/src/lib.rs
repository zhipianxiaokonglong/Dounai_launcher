//! 启动器桌面壳：Tauri 命令面与进程装配。
//!
//! 纯 Rust 逻辑在 `launcher-core`，含验签、路径安全、双根路径与管理页宿主契约。
//! 本 crate 的命令面只校验参数与转换结果，不放业务逻辑。
//! v1 的进程管理、状态库与命令面已在 M3 移除；命令面在 `commands_v2`，状态层在 `state_v2`。

pub mod commands_v2;
pub mod diag_log;
pub mod download;
pub mod env_probe;
pub mod http_client;
pub mod lifecycle_ops;
pub mod lifecycle_runtime;
pub mod m2_http;
pub mod process;
pub mod release;
pub mod state_v2;

// 供应链安全、路径与管理页宿主契约住在 launcher-core。
// 重导出模块名，使 crate 内既有的 `crate::security` / `crate::signature`
// 路径与外部的 `dounai_launcher::security` 路径继续可用。
pub use launcher_core::{admin_host, paths, security, signature};

pub use download::{download, DownloadError, DownloadOptions, DownloadReport, HttpFetcher};
pub use launcher_core::security::{
    is_safe_filename, safe_join, sha256_hex, verify_sha256, PathError,
};
pub use launcher_core::signature::{
    canonical_payload, verify_manifest, SignatureError, SUPPORTED_ALGO,
};
