//! app 侧网络实现：把 `reqwest` 接成 `launcher-core` 的 M2 适配器 trait。
//!
//! core 只定义取数接口，不发起请求；这里负责超时、重定向与域名后缀校验。
//! 下载器（断点续传/重试）继续复用 `download.rs`，见 DESIGN.md:126。
//!
//! **重定向**：SourceForge 的下载域一律 302 到镜像主机（`xxx.dl.sourceforge.net`），
//! 直接拒绝会让整个分发源不可用。这里改成"逐跳跟随但每跳都校验域名"：
//! 每一跳的 host 必须仍在白名单后缀内，超过 `MAX_HOPS` 即放弃。
//! 这与 `core::m2::source::validate_download_url` 的意图一致——它本来就是按
//! "重定向逐跳校验"设计的，只是此前没人真的跟随。

use std::time::Duration;

use launcher_core::m2::adapters::HttpFetcher;
use launcher_core::m2::source::{host_allowed, url_host, SourceError};

/// 内置官方源的后缀全集，仅用于生成默认源表与测试。实际请求一律按源注入，
/// 不让 A 源的 catalog 请求重定向到 B 源的域名。
pub const ALL_HOST_SUFFIXES: &[&str] = &[
    "github.com",
    "githubusercontent.com",
    "sourceforge.net",
    "sourceforge.io",
];

/// 跟随重定向的跳数上限。SF 实测是 master.dl → downloads → 镜像，共 2 跳。
const MAX_HOPS: usize = 5;

pub struct ReqwestCatalogFetcher {
    client: reqwest::blocking::Client,
    host_suffixes: Vec<String>,
}

impl ReqwestCatalogFetcher {
    /// `host_suffixes` 来自 sources.json 的单个源，不含其它发布者的域名。
    pub fn new(host_suffixes: Vec<String>) -> Result<Self, String> {
        if host_suffixes.is_empty() {
            return Err("该源没有出网白名单，拒绝发起请求".into());
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            // 跳转由本文件手动跟随：自动跟随看不到中间跳，等于绕过白名单。
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("dounai-launcher/0.3.1")
            .build()
            .map_err(|e| format!("创建 HTTP 客户端失败：{e}"))?;
        Ok(Self {
            client,
            host_suffixes,
        })
    }
}

impl HttpFetcher for ReqwestCatalogFetcher {
    fn get(&self, url: &str) -> Result<Vec<u8>, SourceError> {
        let unavailable = |target: &str, reason: String| SourceError::Unavailable {
            source_id: target.into(),
            reason,
        };
        let mut current = url.to_string();
        for _hop in 0..=MAX_HOPS {
            if !current.starts_with("https://") {
                return Err(unavailable(&current, format!("只允许 https 地址：{current}")));
            }
            let host = url_host(&current).ok_or_else(|| {
                unavailable(&current, format!("无法解析主机名：{current}"))
            })?;
            if !host_allowed(
                host,
                &self.host_suffixes.iter().map(String::as_str).collect::<Vec<_>>(),
            ) {
                return Err(unavailable(
                    &current,
                    format!("域名不在白名单内：{host}"),
                ));
            }

            let response = self
                .client
                .get(&current)
                .send()
                .map_err(|e| unavailable(&current, e.to_string()))?;
            let status = response.status().as_u16();
            if (300..400).contains(&status) {
                let next = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| {
                        unavailable(&current, format!("重定向 {status} 缺少 Location"))
                    })?;
                current = next.to_string();
                continue;
            }
            if !(200..300).contains(&status) {
                return Err(unavailable(
                    &current,
                    format!("服务端返回状态 {status}"),
                ));
            }
            return response
                .bytes()
                .map(|b| b.to_vec())
                .map_err(|e| unavailable(&current, e.to_string()));
        }
        Err(unavailable(
            url,
            format!("重定向超过 {MAX_HOPS} 跳仍未取到内容"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_https_is_rejected_before_any_request() {
        let fetcher = ReqwestCatalogFetcher::new(ALL_HOST_SUFFIXES.iter().map(|s| s.to_string()).collect()).unwrap();
        assert!(matches!(
            fetcher.get("http://github.com/a/catalog.json"),
            Err(SourceError::Unavailable { .. })
        ));
    }

    #[test]
    fn a_source_cannot_use_another_sources_host() {
        assert!(ReqwestCatalogFetcher::new(vec![]).is_err());
        let sf_only = ReqwestCatalogFetcher::new(vec!["sourceforge.net".into()]).unwrap();
        assert!(matches!(
            sf_only.get("https://github.com/a/catalog.json"),
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
