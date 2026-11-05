//! 本机模块探测工具（原 vendor_keys 的通用部分）。
//!
//! ## 为什么这个文件变薄了
//!
//! 厂商管理原本由启动器自己实现（存密钥、解析厂商清单、画表单）。
//! 后来三个模块都提供了**自带的 Web 管理界面**，启动器改为只负责
//! 「打开那个界面」（见 `commands::open_admin_window`）。
//!
//! 于是这里只剩与厂商无关的通用工具：
//! - 从 health 地址推出服务根；
//! - 对本机发一个最小 GET。
//!
//! **启动器不存储任何密钥**，也不解析任何厂商清单——那些是模块自己的事。

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// 从健康地址推出 `http://host:port`。
///
/// 模块 manifest 里声明的是完整健康地址（如
/// `http://127.0.0.1:8102/v1/healthz`），而我们往往只需要服务根来拼别的路径
/// （例如 `admin_ui.web_ui.path`）。
pub fn base_from_health_url(health_url: &str) -> Option<String> {
    let rest = health_url
        .strip_prefix("http://")
        .or_else(|| health_url.strip_prefix("https://"))?;
    let authority = rest.split('/').next()?;
    if authority.is_empty() {
        return None;
    }
    Some(format!("http://{authority}"))
}

/// 对**本机**地址发一个极简 GET，返回响应体。
///
/// 刻意不复用下载模块的 HTTP 客户端：那条路径带白名单与重定向策略，
/// 而这里只是读本机服务的状态，语义不同、不该混用。
///
/// 只允许回环地址——这个函数不能被当成任意出网原语。
pub fn http_get_local(url: &str, timeout: Duration) -> Result<String, String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| "只支持 http:// 的本机地址".to_string())?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| "端口不合法".to_string())?),
        None => (authority, 80u16),
    };
    if !(host == "127.0.0.1" || host == "localhost" || host == "[::1]") {
        return Err(format!("只允许访问本机地址：{host}"));
    }

    let addr = (host.trim_matches(['[', ']']), port)
        .to_socket_addrs()
        .map_err(|e| format!("解析地址失败：{e}"))?
        .next()
        .ok_or_else(|| "解析地址失败".to_string())?;

    let mut stream =
        TcpStream::connect_timeout(&addr, timeout).map_err(|e| format!("连接失败：{e}"))?;
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();

    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {authority}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("发送请求失败：{e}"))?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("读取响应失败：{e}"))?;

    let text = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| "响应格式异常".to_string())?;
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("000");
    if status != "200" {
        return Err(format!("上游返回 HTTP {status}"));
    }
    Ok(body.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_url_to_base() {
        assert_eq!(
            base_from_health_url("http://127.0.0.1:8101/healthz").unwrap(),
            "http://127.0.0.1:8101"
        );
        assert_eq!(
            base_from_health_url("http://127.0.0.1:8102/v1/healthz").unwrap(),
            "http://127.0.0.1:8102"
        );
        assert!(base_from_health_url("").is_none());
        assert!(base_from_health_url("ftp://x").is_none());
    }

    #[test]
    fn local_http_refuses_non_loopback() {
        // 防止这个函数被当成任意出网原语
        let err = http_get_local("http://example.com/x", Duration::from_millis(200)).unwrap_err();
        assert!(err.contains("只允许访问本机"), "实际：{err}");
    }
}
