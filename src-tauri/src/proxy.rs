//! 把用户的代理设置套到启动器自身的 reqwest 客户端上。
//!
//! 只服务目录拉取与包下载两条出网链路；本机实例的 health 探测不走这里。
//! 代理是传输层配置，不改变 `core::m2::source` 的域名后缀白名单与 https-only
//! 约束——开启代理后仍然逐跳校验目标 URL。

use crate::state_v2::ProxyConfig;

/// 造一个已按用户设置配好代理的客户端构造器。
///
/// 三种模式对应 reqwest 的三种行为：
/// - `Off`：`no_proxy()` 显式关掉，包括环境变量里的全局代理；
/// - `System`：不干预 builder，reqwest 在 `system-proxy` feature 下自行读取；
/// - `Manual`：把用户填的 URL 作为 http/https 代理全部套上。
///
/// 返回的 builder 还没设超时、重定向策略与 UA，由调用方按各自链路补齐。
/// 手动模式 URL 非法时返回 Err，绝不静默退回系统代理——那会让用户以为自己
/// 在用代理、实际走另一条链路。
pub fn apply(
    builder: reqwest::blocking::ClientBuilder,
    config: &ProxyConfig,
) -> Result<reqwest::blocking::ClientBuilder, String> {
    match config {
        ProxyConfig::Off => Ok(builder.no_proxy()),
        ProxyConfig::System => Ok(builder),
        ProxyConfig::Manual { url } => {
            let proxy = reqwest::Proxy::all(url).map_err(|e| format!("代理地址不合法：{e}"))?;
            Ok(builder.proxy(proxy))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_modes_build_clients() {
        for config in [
            ProxyConfig::Off,
            ProxyConfig::System,
            ProxyConfig::Manual {
                url: "http://127.0.0.1:7897".into(),
            },
        ] {
            let builder = reqwest::blocking::Client::builder();
            assert!(apply(builder, &config).unwrap().build().is_ok());
        }
    }

    #[test]
    fn manual_invalid_url_is_rejected() {
        let config = ProxyConfig::Manual {
            url: "not a url".into(),
        };
        let builder = reqwest::blocking::Client::builder();
        assert!(apply(builder, &config).is_err());
    }

    #[test]
    fn config_helpers() {
        assert_eq!(ProxyConfig::Off.as_str(), "off");
        assert_eq!(ProxyConfig::System.as_str(), "system");
        assert_eq!(
            ProxyConfig::parse("manual", " http://x:1 ").unwrap().url(),
            Some("http://x:1")
        );
        assert!(ProxyConfig::parse("bogus", "").is_none());
    }
}
