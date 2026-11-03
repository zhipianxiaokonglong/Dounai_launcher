//! 模型层（OmniNexus）厂商管理的 API 代理。
//!
//! ## 职责边界
//!
//! 启动器**不存储密钥**。本模块只做两件事：
//! 1. 把用户的输入转发给模型层的 `/v1/custom-providers` 系列接口；
//! 2. 把模型层返回的状态（含 `apiKeyMasked` 掩码）原样转回前端。
//!
//! 密钥在这一来一回中**只在内存里存活一次调用**：不落盘、不缓存、不写日志。
//!
//! ## 为什么必须走 API 而不是写 `.env`
//!
//! 模型层的**自定义厂商**由它自己用 AES-GCM 加密存进 SQLite
//! （`data/omninexus.db` + `data/provider-secrets.key`），
//! 密钥只存在于模型层进程内。启动器写 `.env` 对它无效——
//! 那只有**内置厂商**才会读。
//!
//! ## 端点约定（来自 OmniNexus docs/api.md）
//!
//! | 方法 | 路径 | 作用 |
//! |---|---|---|
//! | GET | `/v1/custom-providers` | 列表（Key 仅掩码）+ `capabilityPresets` |
//! | POST | `/v1/custom-providers` | 新建 |
//! | PUT | `/v1/custom-providers/{id}` | 更新（`apiKey` 省略=不改） |
//! | PATCH | `/v1/custom-providers/{id}/enabled` | 启停 |
//! | DELETE | `/v1/custom-providers/{id}?confirm=true` | 删除 |
//! | POST | `/v1/custom-providers/test` | 未保存配置测试 |
//! | POST | `/v1/custom-providers/{id}/test` | 已保存配置测试 |
//! | POST | `/v1/custom-providers/discover-models` | 拉 `/models` |
//! | POST | `/v1/custom-providers/{id}/discover-models` | 已保存配置拉 `/models` |
//! | GET/PUT | `/v1/provider-defaults` | 默认厂商/模型 |
//!
//! ## 字段坑（实测踩过）
//!
//! - `models[]` 的元素只有 `name` / `label` / `capabilities` 三个字段，
//!   **没有 `id`**；用 `id` 会得到 422 `models.0.name: Field required`。
//! - `base_url` 是**严格拼接**的，不会自动补 `/v1`。
//! - 协议目前只接受 `openai-compatible`。

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 调用本机模型层 API 的最小客户端。
///
/// 刻意限制为 **127.0.0.1 / localhost**，避免这个模块被当成任意出网原语。
pub struct ProviderAdmin {
    base: String,
    client: reqwest::blocking::Client,
}

impl ProviderAdmin {
    /// 从模块健康地址（如 `http://127.0.0.1:8102/v1/healthz`）推出服务根。
    pub fn new(base: String) -> Result<Self, String> {
        if !is_loopback(&base) {
            return Err(format!("只允许访问本机模型层：{base}"));
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            // 模型层的测试接口需要访问外部厂商，可能较慢；测试用更长超时
            .build()
            .map_err(|e| format!("创建 HTTP 客户端失败：{e}"))?;
        Ok(Self { base, client })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base.trim_end_matches('/'), path)
    }

    fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, String> {
        let mut req = self.client.request(method, self.url(path));
        if let Some(payload) = &body {
            req = req.json(payload);
        }
        let resp = req.send().map_err(|e| format!("请求模型层失败：{e}"))?;
        let status = resp.status();
        let text = resp
            .text()
            .map_err(|e| format!("读取模型层响应失败：{e}"))?;

        if status.is_success() {
            if text.trim().is_empty() {
                return Ok(Value::Null);
            }
            return serde_json::from_str(&text).map_err(|e| format!("模型层返回的不是 JSON：{e}"));
        }

        // 上游错误体结构：{"error":{"code","message","retryable","details"}}
        let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let code = parsed
            .pointer("/error/code")
            .and_then(|v| v.as_str())
            .unwrap_or("UPSTREAM_ERROR");
        let message = parsed
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .unwrap_or("模型层拒绝了请求");
        Err(format!("{code}: {message}（HTTP {status}）"))
    }

    /// 列表（含脱敏 Key 与能力预设）。
    pub fn list(&self) -> Result<ProviderList, String> {
        let raw = self.send(reqwest::Method::GET, "/v1/custom-providers", None)?;
        // 只透传依赖的字段，避免把上游私有结构绑死
        let providers = raw
            .get("providers")
            .cloned()
            .unwrap_or(Value::Array(vec![]));
        let presets = raw.get("capabilityPresets").cloned().unwrap_or(Value::Null);
        Ok(ProviderList {
            providers,
            capability_presets: presets,
        })
    }

    pub fn create(&self, payload: Value) -> Result<Value, String> {
        self.send(reqwest::Method::POST, "/v1/custom-providers", Some(payload))
    }

    pub fn update(&self, id: &str, payload: Value) -> Result<Value, String> {
        self.send(
            reqwest::Method::PUT,
            &format!("/v1/custom-providers/{id}"),
            Some(payload),
        )
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<Value, String> {
        self.send(
            reqwest::Method::PATCH,
            &format!("/v1/custom-providers/{id}/enabled"),
            Some(serde_json::json!({ "enabled": enabled })),
        )
    }

    pub fn delete(&self, id: &str) -> Result<Value, String> {
        // confirm=true 是上游要求：删除默认/使用中的厂商需显式确认
        self.send(
            reqwest::Method::DELETE,
            &format!("/v1/custom-providers/{id}?confirm=true"),
            None,
        )
    }

    /// 用未保存的配置做三级连接测试（可带 id 复用已保存的 Key）。
    pub fn test(&self, payload: Value) -> Result<Value, String> {
        self.send(
            reqwest::Method::POST,
            "/v1/custom-providers/test",
            Some(payload),
        )
    }

    pub fn test_saved(&self, id: &str) -> Result<Value, String> {
        // 上游要求带 JSON body：不带会得到 422 "Field required"（实测）
        self.send(
            reqwest::Method::POST,
            &format!("/v1/custom-providers/{id}/test"),
            Some(serde_json::json!({})),
        )
    }

    /// 拉取远端 `/models`（未保存配置）。
    pub fn discover(&self, payload: Value) -> Result<Value, String> {
        self.send(
            reqwest::Method::POST,
            "/v1/custom-providers/discover-models",
            Some(payload),
        )
    }

    pub fn discover_saved(&self, id: &str) -> Result<Value, String> {
        // 同 test_saved：必须带 JSON body，否则 422
        self.send(
            reqwest::Method::POST,
            &format!("/v1/custom-providers/{id}/discover-models"),
            Some(serde_json::json!({})),
        )
    }

    pub fn defaults(&self) -> Result<Value, String> {
        self.send(reqwest::Method::GET, "/v1/provider-defaults", None)
    }

    pub fn set_defaults(&self, payload: Value) -> Result<Value, String> {
        self.send(reqwest::Method::PUT, "/v1/provider-defaults", Some(payload))
    }

    /// 内置厂商（只读；密钥来自环境变量/`.env`）。
    pub fn builtin(&self) -> Result<Value, String> {
        self.send(reqwest::Method::GET, "/v1/providers", None)
    }

    /// 全部模型（含能力与可用性）。
    pub fn models(&self) -> Result<Value, String> {
        self.send(reqwest::Method::GET, "/v1/models", None)
    }
}

/// 列表返回：`providers` 原样透传，另附能力预设施。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderList {
    pub providers: Value,
    #[serde(rename = "capabilityPresets")]
    pub capability_presets: Value,
}

fn is_loopback(base: &str) -> bool {
    let rest = match base
        .strip_prefix("http://")
        .or_else(|| base.strip_prefix("https://"))
    {
        Some(r) => r,
        None => return false,
    };
    let authority = rest.split('/').next().unwrap_or("");
    let host = match authority.rsplit_once(':') {
        Some((h, _)) => h,
        None => authority,
    };
    matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_loopback() {
        // 防被当作任意出网原语
        assert!(ProviderAdmin::new("http://example.com".to_string()).is_err());
        assert!(ProviderAdmin::new("https://api.openai.com/v1".to_string()).is_err());
    }

    #[test]
    fn accepts_loopback_variants() {
        assert!(is_loopback("http://127.0.0.1:8102"));
        assert!(is_loopback("http://localhost:8102"));
        assert!(is_loopback("http://[::1]:8102"));
        assert!(!is_loopback("http://192.168.1.5:8102"));
        assert!(!is_loopback("http://example.com"));
    }

    #[test]
    fn builds_urls_without_double_slash() {
        let a = ProviderAdmin::new("http://127.0.0.1:8102/".to_string()).unwrap();
        assert_eq!(
            a.url("/v1/custom-providers"),
            "http://127.0.0.1:8102/v1/custom-providers"
        );
    }
}
