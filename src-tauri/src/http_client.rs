//! 基于 `reqwest` 的真实 HTTP 实现，对接 `download` 的 trait。
//!
//! 设计要点：
//! - **逐跳校验重定向**：SourceForge 的下载域一律 302 到镜像主机
//!   （`xxx.dl.sourceforge.net`），一律拒绝会让整个分发源不可用；
//!   所以这里跟随跳转，但每一跳的 host 都必须仍在白名单后缀内，
//!   超过 `MAX_HOPS` 即放弃。自动跟随看不到中间跳，等于绕过白名单；
//! - **超时必设**：无超时的请求在弱网下会永久挂起，用户看到的是"卡住不动"；
//! - **Range 支持**：断点续传依赖 `Range` 头，服务端不支持时返回 200 由上层回退。

use std::time::Duration;

use launcher_core::m2::source::{host_allowed, url_host};

use crate::download::{FetchResponse, HttpFetcher};

/// 跟随重定向的跳数上限。SF 实测是 downloads → 镜像，共 1 到 2 跳。
const MAX_HOPS: usize = 5;

/// 真实 HTTP 客户端。
pub struct ReqwestClient {
    client: reqwest::blocking::Client,
    /// 只属于本次安装所在源的域名后缀，不复用全局常量。
    host_suffixes: Vec<String>,
}

impl ReqwestClient {
    pub fn new(timeout: Duration, host_suffixes: Vec<String>) -> Result<Self, String> {
        if host_suffixes.is_empty() {
            return Err("该源没有出网白名单，拒绝发起请求".into());
        }
        // 跳转由本文件手动跟随：自动跟随时中间跳不可见，无法逐跳校验。
        let client = reqwest::blocking::Client::builder()
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("dounai-launcher/0.3.1")
            .build()
            .map_err(|e| format!("创建 HTTP 客户端失败：{e}"))?;
        Ok(Self {
            client,
            host_suffixes,
        })
    }

    pub fn default_client(host_suffixes: Vec<String>) -> Result<Self, String> {
        Self::new(Duration::from_secs(60), host_suffixes)
    }

    /// 校验一个待请求地址：必须 https，且主机在本源白名单后缀内。
    fn check_url(&self, url: &str) -> Result<(), String> {
        if !url.starts_with("https://") {
            return Err(format!("只允许 https 地址：{url}"));
        }
        let host = url_host(url).ok_or_else(|| format!("无法解析主机名：{url}"))?;
        let suffixes: Vec<&str> = self.host_suffixes.iter().map(String::as_str).collect();
        if !host_allowed(host, &suffixes) {
            return Err(format!("域名不在本源白名单内：{host}"));
        }
        Ok(())
    }
}

impl HttpFetcher for ReqwestClient {
    fn fetch(&self, url: &str, range_from: Option<u64>) -> Result<FetchResponse, String> {
        let mut current = url.to_string();
        for _ in 0..=MAX_HOPS {
            self.check_url(&current)?;
            let mut req = self.client.get(&current);
            if let Some(from) = range_from {
                req = req.header("Range", format!("bytes={from}-"));
            }
            let resp = req.send().map_err(|e| format!("请求失败：{e}"))?;
            let status = resp.status().as_u16();

            // 3xx 不交给 reqwest 自动跟随：这里逐跳校验域名后再跟。
            if (300..400).contains(&status) {
                let next = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| format!("重定向 {status} 缺少 Location：{current}"))?;
                current = next.to_string();
                continue;
            }

            let body = resp
                .bytes()
                .map_err(|e| format!("读取响应体失败：{e}"))?
                .to_vec();
            return Ok(FetchResponse { status, body });
        }
        Err(format!("重定向超过 {MAX_HOPS} 跳仍未取到内容：{url}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::m2_http::ALL_HOST_SUFFIXES;

    #[test]
    fn client_builds_with_timeout() {
        let c = ReqwestClient::new(Duration::from_secs(5), vec!["example.org".into()]);
        assert!(c.is_ok());
        assert!(ReqwestClient::new(Duration::from_secs(5), vec![]).is_err());
    }

    #[test]
    fn default_client_builds() {
        assert!(ReqwestClient::default_client(
            ALL_HOST_SUFFIXES.iter().map(|s| s.to_string()).collect()
        )
        .is_ok());
    }

    #[test]
    fn package_download_cannot_cross_into_another_sources_host() {
        let c = ReqwestClient::new(Duration::from_millis(300), vec!["example.org".into()]).unwrap();
        assert!(c.fetch("https://github.com/a.zip", None).is_err());
    }

    /// 连接一个必定失败的地址，验证返回 Err 而不是 panic。
    #[test]
    fn unreachable_host_returns_error_not_panic() {
        let c = ReqwestClient::new(Duration::from_millis(600), vec!["example.org".into()]).unwrap();
        // 保留端口，几乎不可能有服务
        let r = c.fetch("http://127.0.0.1:1/nonexistent", None);
        assert!(r.is_err(), "不可达主机应返回错误");
    }
}
