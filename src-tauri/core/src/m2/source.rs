//! M0 冻结的 SourceAdapter trait，逐字对应 contracts/source-adapter-v1.md。
//!
//! 适配器只负责取数，不做验签、信任判定、安装或激活。本地来源不复制源码，
//! 也不执行任何 git 网络操作。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::m2::catalog::{CatalogDocument, CatalogEntry};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    RemoteCatalog,
    LocalZip,
    LocalDir,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageReceipt {
    pub source_kind: SourceKind,
    pub source_path: Option<PathBuf>,
    /// 启动器自算；本地来源只防传输损坏，不构成完整性保证。
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SourceError {
    #[error("源 {source_id} 不可用：{reason}")]
    Unavailable { source_id: String, reason: String },
    #[error("catalog 不合法：{reason}")]
    CatalogInvalid { reason: String },
    #[error("包不合法：{reason}")]
    PackageInvalid { reason: String },
    #[error("本地路径被拒：{reason}")]
    LocalPathRejected { reason: String },
}

impl SourceError {
    pub fn reason(&self) -> &str {
        match self {
            Self::Unavailable { reason, .. }
            | Self::CatalogInvalid { reason }
            | Self::PackageInvalid { reason }
            | Self::LocalPathRejected { reason } => reason,
        }
    }
}

pub trait SourceAdapter: Send + Sync {
    fn id(&self) -> &str;
    fn kind(&self) -> SourceKind;
    fn priority(&self) -> u32;
    /// LocalZip 与 LocalDir 返回 Ok(None)，表示没有新鲜度锚，不表示来源不可用。
    fn fetch_catalog(&self) -> Result<Option<CatalogDocument>, SourceError>;
    fn fetch_package(
        &self,
        entry: &CatalogEntry,
        dest: &Path,
    ) -> Result<PackageReceipt, SourceError>;
}

/// GitHub asset 与 SourceForge /download 都会跳到动态镜像，精确 host 白名单
/// 挡不住，改用域名后缀集；最终仍以 entry.sha256 为准。
pub const GITHUB_HOST_SUFFIXES: &[&str] = &["github.com", "githubusercontent.com"];
pub const SOURCEFORGE_HOST_SUFFIXES: &[&str] = &["sourceforge.net", "sourceforge.io"];

/// 取 https URL 的 host，去掉 userinfo 与端口。非 https 返回 None。
pub fn url_host(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?.split(':').next()?;
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

pub fn host_allowed(host: &str, suffixes: &[&str]) -> bool {
    let lower = host.to_ascii_lowercase();
    suffixes
        .iter()
        .any(|suffix| lower == *suffix || lower.ends_with(&format!(".{suffix}")))
}

pub fn validate_download_url(url: &str, suffixes: &[&str]) -> Result<(), SourceError> {
    let host = url_host(url).ok_or_else(|| SourceError::PackageInvalid {
        reason: format!("下载地址必须是 https：{url}"),
    })?;
    if !host_allowed(host, suffixes) {
        return Err(SourceError::PackageInvalid {
            reason: format!("重定向目标域名不在白名单内：{host}"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_suffixes_cover_dynamic_mirrors() {
        assert_eq!(url_host("https://github.com/a/b"), Some("github.com"));
        assert_eq!(
            url_host("https://objects.githubusercontent.com/x?sig=1"),
            Some("objects.githubusercontent.com")
        );
        assert_eq!(url_host("http://example.com/x"), None);
        assert_eq!(
            url_host("https://user:pw@example.com:8443/x"),
            Some("example.com")
        );
        assert!(host_allowed(
            "phoenixnap.dl.sourceforge.net",
            SOURCEFORGE_HOST_SUFFIXES
        ));
        assert!(!host_allowed(
            "evil-sourceforge.net",
            SOURCEFORGE_HOST_SUFFIXES
        ));
    }

    #[test]
    fn rejects_unlisted_redirect_host() {
        assert!(validate_download_url("https://evil.example/x", GITHUB_HOST_SUFFIXES).is_err());
        assert!(validate_download_url("https://github.com/x", GITHUB_HOST_SUFFIXES).is_ok());
    }
}
