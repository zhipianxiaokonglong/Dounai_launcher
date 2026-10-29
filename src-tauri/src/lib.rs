//! Dounai 启动器核心库。
//!
//! 一期聚焦三件事：
//! 1. **供应链安全**：哈希校验、解压防路径穿越；
//! 2. **安装事务**（依赖安装）：失败自动回滚，不留半残环境；
//! 3. **模块 manifest 校验**：字段完整性与签名。
//!
//! 不含 UI：UI 通过 Tauri 命令调用本库。

pub mod catalog;
pub mod commands;
pub mod download;
pub mod env_probe;
pub mod http_client;
pub mod install;
pub mod runtime;
pub mod security;
pub mod signature;
pub mod state;
pub mod trust;
pub mod vendor_keys;

pub use catalog::{
    fetch_catalog, parse_catalog, Allowlist, AllowlistError, CatalogEntry, CatalogFetcher,
    CatalogResult,
};
pub use download::{download, DownloadError, DownloadOptions, DownloadReport, HttpFetcher};
pub use install::{extract_zip_into, InstallError, InstallReport, InstallTransaction};
pub use runtime::{
    check_working_dir, resolve_placeholders, split_command, EntrySpec, ProcessManager, RunView,
    StartError,
};
pub use security::{is_safe_filename, safe_join, sha256_hex, verify_sha256, PathError};
pub use signature::{canonical_payload, verify_manifest, SignatureError, SUPPORTED_ALGO};
pub use state::{Category, InstallState, ModuleState, RunState, StateError, StateStore};
pub use trust::{TrustError, TrustPolicy, TrustVerdict};
