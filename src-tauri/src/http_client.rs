//! 基于 `reqwest` 的真实 HTTP 实现，对接 `download` 与 `catalog` 的 trait。
//!
//! 设计要点：
//! - **不跟随重定向**：跟随重定向可能把请求带到白名单外的域名，
//!   等于绕过白名单。需要跳转时由上层显式处理；
//! - **超时必设**：无超时的请求在弱网下会永久挂起，用户看到的是"卡住不动"；
//! - **Range 支持**：断点续传依赖 `Range` 头，服务端不支持时返回 200 由上层回退。

use std::time::Duration;

use crate::catalog::CatalogFetcher;
use crate::download::{FetchResponse, HttpFetcher};

/// 真实 HTTP 客户端。
pub struct ReqwestClient {
    client: reqwest::blocking::Client,
}

impl ReqwestClient {
    pub fn new(timeout: Duration) -> Result<Self, String> {
        // 不跟随重定向：重定向目标可能不在白名单内
        let client = reqwest::blocking::Client::builder()
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("dounai-launcher/0.1.0")
            .build()
            .map_err(|e| format!("创建 HTTP 客户端失败：{e}"))?;
        Ok(Self { client })
    }

    pub fn default_client() -> Result<Self, String> {
        Self::new(Duration::from_secs(60))
    }
}

impl HttpFetcher for ReqwestClient {
    fn fetch(&self, url: &str, range_from: Option<u64>) -> Result<FetchResponse, String> {
        let mut req = self.client.get(url);
        if let Some(from) = range_from {
            req = req.header("Range", format!("bytes={from}-"));
        }
        let resp = req.send().map_err(|e| format!("请求失败：{e}"))?;
        let status = resp.status().as_u16();

        // 3xx 视为错误：不跟随重定向，避免被带去白名单外
        if (300..400).contains(&status) {
            return Err(format!(
                "服务端返回重定向 {status}，为安全起见不自动跟随（目标可能不在白名单内）"
            ));
        }

        let body = resp
            .bytes()
            .map_err(|e| format!("读取响应体失败：{e}"))?
            .to_vec();
        Ok(FetchResponse { status, body })
    }
}

impl CatalogFetcher for ReqwestClient {
    fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        // 清单同样不跟随重定向
        let resp = self
            .client
            .get(url)
            .send()
            .map_err(|e| format!("请求失败：{e}"))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(format!("清单请求返回状态 {status}"));
        }
        Ok(resp.bytes().map_err(|e| format!("读取失败：{e}"))?.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_builds_with_timeout() {
        let c = ReqwestClient::new(Duration::from_secs(5));
        assert!(c.is_ok());
    }

    #[test]
    fn default_client_builds() {
        assert!(ReqwestClient::default_client().is_ok());
    }

    /// 连接一个必定失败的地址，验证返回 Err 而不是 panic。
    #[test]
    fn unreachable_host_returns_error_not_panic() {
        let c = ReqwestClient::new(Duration::from_millis(600)).unwrap();
        // 保留端口，几乎不可能有服务
        let r = c.fetch("http://127.0.0.1:1/nonexistent", None);
        assert!(r.is_err(), "不可达主机应返回错误");
    }
}
