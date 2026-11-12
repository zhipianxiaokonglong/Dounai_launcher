//! app 侧网络实现：把 `reqwest` 接成 `launcher-core` 的 M2 适配器 trait。
//!
//! core 只定义取数接口，不发起请求；这里负责超时、重定向与域名后缀校验。
//! 下载器（断点续传/重试）继续复用 `download.rs`，见 DESIGN.md:126。

use std::time::Duration;

use launcher_core::m2::adapters::HttpFetcher;
use launcher_core::m2::source::SourceError;

/// 允许的 catalog/包下载域名后缀。与双源契约一致。
pub const ALL_HOST_SUFFIXES: &[&str] = &[
    "github.com",
    "githubusercontent.com",
    "sourceforge.net",
    "sourceforge.io",
];

pub struct ReqwestCatalogFetcher {
    client: reqwest::blocking::Client,
}

impl ReqwestCatalogFetcher {
    pub fn new() -> Result<Self, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("dounai-launcher/0.3.1")
            .build()
            .map_err(|e| format!("创建 HTTP 客户端失败：{e}"))?;
        Ok(Self { client })
    }
}

impl HttpFetcher for ReqwestCatalogFetcher {
    fn get(&self, url: &str) -> Result<Vec<u8>, SourceError> {
        if !url.starts_with("https://") {
            return Err(SourceError::Unavailable {
                source_id: "http".into(),
                reason: format!("只允许 https 地址：{url}"),
            });
        }
        let response = self
            .client
            .get(url)
            .send()
            .map_err(|e| SourceError::Unavailable {
                source_id: url.into(),
                reason: e.to_string(),
            })?;
        let status = response.status().as_u16();
        if (300..400).contains(&status) {
            return Err(SourceError::Unavailable {
                source_id: url.into(),
                reason: format!("服务端返回重定向 {status}，不自动跟随"),
            });
        }
        if !(200..300).contains(&status) {
            return Err(SourceError::Unavailable {
                source_id: url.into(),
                reason: format!("服务端返回状态 {status}"),
            });
        }
        response
            .bytes()
            .map(|b| b.to_vec())
            .map_err(|e| SourceError::Unavailable {
                source_id: url.into(),
                reason: e.to_string(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_https_is_rejected_before_any_request() {
        let fetcher = ReqwestCatalogFetcher::new().unwrap();
        assert!(matches!(
            fetcher.get("http://github.com/a/catalog.json"),
            Err(SourceError::Unavailable { .. })
        ));
    }

    #[test]
    fn allowlist_covers_both_sources() {
        assert_eq!(ALL_HOST_SUFFIXES.len(), 4);
        assert!(launcher_core::m2::source::host_allowed(
            "objects.githubusercontent.com",
            ALL_HOST_SUFFIXES
        ));
        assert!(!launcher_core::m2::source::host_allowed(
            "evil.github.com.example.org",
            ALL_HOST_SUFFIXES
        ));
    }
}
