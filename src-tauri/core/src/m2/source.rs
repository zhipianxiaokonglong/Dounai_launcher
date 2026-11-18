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

/// 回环主机：本机测试源允许走明文 http。公网地址一律仍要求 https。
/// 只认精确值，避免 `127.0.0.1.evil.com` 这类 DNS 名被误判为本机。
pub fn is_loopback_host(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    h == "127.0.0.1" || h == "localhost" || h == "::1"
}

fn authority_host(authority: &str) -> Option<&str> {
    let hostport = authority.rsplit('@').next()?;
    // IPv6 字面量形如 [::1]:8765，方括号内才是 host。
    let host = if let Some(inner) = hostport.strip_prefix('[') {
        inner.split(']').next()?
    } else {
        hostport.split(':').next()?
    };
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

/// 取 URL 的 host：https 任意主机都接受，http 仅接受回环主机；
/// 去掉 userinfo 与端口。非放行 scheme 返回 None。
pub fn url_host(url: &str) -> Option<&str> {
    let (rest, is_http) = if let Some(r) = url.strip_prefix("https://") {
        (r, false)
    } else if let Some(r) = url.strip_prefix("http://") {
        (r, true)
    } else {
        return None;
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority_host(authority)?;
    if is_http && !is_loopback_host(host) {
        return None;
    }
    Some(host)
}

/// URL 的 scheme + 主机是否放行（https 任意，http 仅回环）。
/// 与 `url_host` 共用同一判定，避免各层各写一套导致口径漂移。
pub fn url_is_allowed(url: &str) -> bool {
    url_host(url).is_some()
}

pub fn host_allowed(host: &str, suffixes: &[&str]) -> bool {
    let lower = host.to_ascii_lowercase();
    suffixes
        .iter()
        .any(|suffix| lower == *suffix || lower.ends_with(&format!(".{suffix}")))
}

pub fn validate_download_url(url: &str, suffixes: &[&str]) -> Result<(), SourceError> {
    let host = url_host(url).ok_or_else(|| SourceError::PackageInvalid {
        reason: format!("下载地址必须是 https，或本机回环 http：{url}"),
    })?;
    if !host_allowed(host, suffixes) && !is_loopback_host(host) {
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
        // 公网 http 一律拒绝；回环 http 放行（本机测试源）。
        assert_eq!(url_host("http://example.com/x"), None);
        assert_eq!(url_host("http://127.0.0.1:8765/x"), Some("127.0.0.1"));
        assert_eq!(url_host("http://localhost:8765/x"), Some("localhost"));
        assert_eq!(url_host("http://[::1]:8765/x"), Some("::1"));
        assert_eq!(url_host("http://127.0.0.1.evil.com/x"), None);
        assert!(url_is_allowed("http://127.0.0.1:8765/catalog.json"));
        assert!(!url_is_allowed("http://evil.example/catalog.json"));
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
        // 回环 http 下载地址放行，且不要求它在域名后缀白名单里。
        assert!(validate_download_url("http://127.0.0.1:8765/a.zip", &[]).is_ok());
        assert!(validate_download_url("http://evil.example/a.zip", &[]).is_err());
    }
}
