//! launcher-core 不依赖 Tauri，供命令行工具、CI 与桌面壳共用。
//!
//! 边界：这一层只放纯 Rust 逻辑。窗口、子 WebView 与 IPC 代码留在桌面壳。
//! 命令面负责校验参数与转换结果，业务逻辑逐步迁入本层。

pub mod admin_host;
pub mod env;
pub mod install;
pub mod manifest;
pub mod paths;
pub mod security;
pub mod signature;
pub mod state;
pub mod time;

pub use admin_host::{AdminHost, AdminHostError, HostEvent, MountRequest, RecordingAdminHost};
pub use env::{
    cache_usage, prepare_instance, prepare_instance_with, verify_instance, verify_instance_with,
    CacheUsage, EnvContext, EnvError, EnvErrorKind, EnvOptions, ManagedPython, PreparedEnv,
    RuntimeState, UvBinary, UV_VERSION,
};
pub use install::{InstallState, InstallStateError};
pub use manifest::{
    expand_placeholders, DepsKind, DepsSpec, EntrySpec, HealthSpec, ModuleManifestV2, Role,
    RuntimeSpec,
};
pub use paths::{instance_key, InstallMode, Paths, PathsError};
pub use security::{is_safe_filename, safe_join, sha256_hex, verify_sha256, PathError};
pub use signature::{canonical_payload, verify_manifest, SignatureError, SUPPORTED_ALGO};
pub use state::{file_sha256_prefixed, read_json, write_json, StateError};
