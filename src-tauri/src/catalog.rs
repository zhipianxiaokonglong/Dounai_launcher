//! 联网信息与出网白名单（联网信息、 约束）。
//!
//! 三条硬约束：
//! 1. **只拉取不上传**：仅 GET，不发送任何用户数据；
//! 2. **域名白名单**：未在白名单内的地址一律拒绝，且白名单随版本公开；
//! 3. **离线可用**：拉取失败时回退到本地缓存/内置清单，不阻断模块管理。
//!
//! 这里把「白名单校验」做成独立纯函数，便于穷举测试；
//! 网络层通过 trait 注入，保证测试无需真实网络。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// 白名单错误。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AllowlistError {
    #[error("地址解析失败：{0}")]
    BadUrl(String),
    #[error("协议不被允许：{0}（仅允许 https）")]
    BadScheme(String),
    #[error("域名不在白名单内：{0}")]
    NotAllowed(String),
    #[error("白名单为空，已拒绝全部网络请求")]
    EmptyAllowlist,
}

/// 出网白名单。
///
/// 只接受 **https**：http 会被中间人篡改，而模块清单直接决定装什么，
/// 一次篡改就能造成供应链投毒。
#[derive(Debug, Clone, Default)]
pub struct Allowlist {
    hosts: BTreeSet<String>,
}

impl Allowlist {
    pub fn new<I, S>(hosts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let hosts = hosts
            .into_iter()
            .map(|h| h.into().trim().to_ascii_lowercase())
            .filter(|h| !h.is_empty())
            .collect();
        Self { hosts }
    }

    pub fn hosts(&self) -> Vec<String> {
        self.hosts.iter().cloned().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.hosts.is_empty()
    }

    /// 校验 URL 是否可访问。
    ///
    /// 支持精确匹配与**单层通配**（`*.example.com`），但不允许
    /// `*` 单独匹配任意域名——那等于没有白名单。
    pub fn check(&self, url: &str) -> Result<(), AllowlistError> {
        if self.hosts.is_empty() {
            return Err(AllowlistError::EmptyAllowlist);
        }

        let lower = url.trim().to_ascii_lowercase();

        // 只允许 https
        if !lower.starts_with("https://") {
            let scheme = lower.split("://").next().unwrap_or("(none)").to_string();
            return Err(AllowlistError::BadScheme(scheme));
        }

        // 去掉协议后取 host（到第一个 / 或 ? 或 # 为止）
        let after = &lower["https://".len()..];
        let host_port = after.split(['/', '?', '#']).next().unwrap_or("");
        if host_port.is_empty() {
            return Err(AllowlistError::BadUrl(url.to_string()));
        }

        // 去掉端口与 userinfo
        let host_only = host_port.rsplit('@').next().unwrap_or(host_port);
        let host_only = host_only.split(':').next().unwrap_or(host_only);
        if host_only.is_empty() {
            return Err(AllowlistError::BadUrl(url.to_string()));
        }

        if self.matches(host_only) {
            return Ok(());
        }
        Err(AllowlistError::NotAllowed(host_only.to_string()))
    }

    fn matches(&self, host: &str) -> bool {
        if self.hosts.contains(host) {
            return true;
        }
        // 通配：白名单写 *.example.com，匹配 sub.example.com
        for pattern in &self.hosts {
            if let Some(suffix) = pattern.strip_prefix("*.") {
                // 必须至少有一个子域标签，且不能是裸域名本身
                if let Some(rest) = host.strip_suffix(suffix) {
                    if rest.ends_with('.') && rest.len() > 1 {
                        return true;
                    }
                }
            }
        }
        false
    }
}

/// 清单条目：启动器从远端拉取的模块信息。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct CatalogEntry {
    pub module_id: String,
    pub name: String,
    pub version: String,
    /// 下载地址（必须也在白名单内）
    pub download_url: String,
    /// 期望 SHA-256
    pub sha256: String,
}

/// 清单拉取结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogResult {
    /// 从远端拉取成功
    Remote(Vec<CatalogEntry>),
    /// 回退到本地清单
    Cached(Vec<CatalogEntry>),
    /// 完全不可用（离线且无缓存）
    Unavailable(String),
}

/// HTTP 抓取抽象（只读）。
pub trait CatalogFetcher {
    fn get(&self, url: &str) -> Result<Vec<u8>, String>;
}

/// 拉取模块清单。
///
/// 失败时回退到 `cache_path`；缓存也无效则返回 `Unavailable`，
/// **不伪造空清单**——空清单会让用户以为"没有任何模块可装"。
pub fn fetch_catalog<F: CatalogFetcher>(
    fetcher: &F,
    allowlist: &Allowlist,
    catalog_url: &str,
    cache_path: &Path,
) -> CatalogResult {
    if let Err(err) = allowlist.check(catalog_url) {
        // 白名单拒绝不是网络故障，直接回退缓存并说明原因
        return match read_cache(cache_path) {
            Some(entries) => CatalogResult::Cached(entries),
            None => CatalogResult::Unavailable(format!("清单地址被白名单拒绝：{err}")),
        };
    }

    match fetcher.get(catalog_url) {
        Ok(bytes) => match parse_catalog(&bytes) {
            Ok(entries) => {
                // 顺手更新缓存，供下次离线使用
                let _ = write_cache(cache_path, &bytes);
                CatalogResult::Remote(entries)
            }
            Err(err) => match read_cache(cache_path) {
                Some(entries) => CatalogResult::Cached(entries),
                None => CatalogResult::Unavailable(format!("清单解析失败：{err}")),
            },
        },
        Err(err) => match read_cache(cache_path) {
            Some(entries) => CatalogResult::Cached(entries),
            None => CatalogResult::Unavailable(format!("网络不可用：{err}")),
        },
    }
}

/// 解析清单 JSON。
///
/// 同时接受 `{"modules": [...]}` 与裸数组两种形式，便于服务端演进。
pub fn parse_catalog(bytes: &[u8]) -> Result<Vec<CatalogEntry>, String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| format!("不是合法 JSON：{e}"))?;

    let arr = if let Some(list) = value.get("modules") {
        list.as_array().ok_or("modules 必须是数组")?
    } else {
        value
            .as_array()
            .ok_or("清单必须是数组或含 modules 数组的对象")?
    };

    let mut out = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        let module_id = item
            .get("module_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("第 {i} 项缺少 module_id"))?;
        let download_url = item
            .get("download_url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("第 {i} 项缺少 download_url"))?;
        // 下载地址必须是 https，否则装包可被篡改
        if !download_url.to_ascii_lowercase().starts_with("https://") {
            return Err(format!("第 {i} 项的下载地址不是 https：{download_url}"));
        }

        out.push(CatalogEntry {
            module_id: module_id.to_string(),
            name: item
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(module_id)
                .to_string(),
            version: item
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("0.0.0")
                .to_string(),
            download_url: download_url.to_string(),
            sha256: item
                .get("sha256")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        });
    }
    Ok(out)
}

fn read_cache(path: &Path) -> Option<Vec<CatalogEntry>> {
    let bytes = std::fs::read(path).ok()?;
    parse_catalog(&bytes).ok()
}

fn write_cache(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // 与状态文件一致：先写临时再 rename
    let tmp: PathBuf = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct FakeFetcher {
        result: Result<Vec<u8>, String>,
        calls: RefCell<Vec<String>>,
    }

    impl CatalogFetcher for FakeFetcher {
        fn get(&self, url: &str) -> Result<Vec<u8>, String> {
            self.calls.borrow_mut().push(url.to_string());
            self.result.clone()
        }
    }

    fn tmp_cache(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dounai-cat-{}-{}", name, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("catalog.json")
    }

    fn sample_catalog() -> Vec<u8> {
        br#"{"modules":[
            {"module_id":"a","name":"A","version":"1.0.0",
             "download_url":"https://dl.example.com/a.zip","sha256":"abc"}
        ]}"#
        .to_vec()
    }

    #[test]
    fn allowlist_accepts_exact_host() {
        let al = Allowlist::new(["dl.example.com"]);
        assert!(al.check("https://dl.example.com/a.zip").is_ok());
    }

    #[test]
    fn allowlist_rejects_other_host() {
        let al = Allowlist::new(["dl.example.com"]);
        assert_eq!(
            al.check("https://evil.com/a.zip"),
            Err(AllowlistError::NotAllowed("evil.com".into()))
        );
    }

    #[test]
    fn allowlist_rejects_http() {
        let al = Allowlist::new(["dl.example.com"]);
        assert!(matches!(
            al.check("http://dl.example.com/a.zip"),
            Err(AllowlistError::BadScheme(_))
        ));
    }

    #[test]
    fn allowlist_rejects_empty() {
        let al = Allowlist::new(Vec::<String>::new());
        assert_eq!(
            al.check("https://anything.com"),
            Err(AllowlistError::EmptyAllowlist)
        );
    }

    #[test]
    fn allowlist_wildcard_matches_subdomain() {
        let al = Allowlist::new(["*.example.com"]);
        assert!(al.check("https://dl.example.com/x").is_ok());
        assert!(al.check("https://a.b.example.com/x").is_ok());
        // 裸域名不应被 *. 匹配
        assert!(al.check("https://example.com/x").is_err());
        // 相似域名不得被误匹配
        assert!(al.check("https://notexample.com/x").is_err());
        assert!(al.check("https://example.com.evil.com/x").is_err());
    }

    #[test]
    fn allowlist_ignores_port_and_userinfo() {
        let al = Allowlist::new(["dl.example.com"]);
        assert!(al.check("https://dl.example.com:8443/a").is_ok());
        assert!(al.check("https://user@dl.example.com/a").is_ok());
    }

    #[test]
    fn catalog_parses_object_form() {
        let entries = parse_catalog(&sample_catalog()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].module_id, "a");
        assert_eq!(entries[0].sha256, "abc");
    }

    #[test]
    fn catalog_parses_bare_array() {
        let raw = br#"[{"module_id":"b","download_url":"https://dl.example.com/b.zip"}]"#;
        let entries = parse_catalog(raw).unwrap();
        assert_eq!(entries[0].module_id, "b");
        assert_eq!(entries[0].version, "0.0.0");
    }

    #[test]
    fn catalog_rejects_http_download_url() {
        let raw = br#"[{"module_id":"c","download_url":"http://x/c.zip"}]"#;
        assert!(parse_catalog(raw).is_err());
    }

    #[test]
    fn catalog_rejects_missing_fields() {
        assert!(parse_catalog(br#"[{"name":"no id"}]"#).is_err());
        assert!(parse_catalog(br#"[{"module_id":"x"}]"#).is_err());
    }

    #[test]
    fn fetch_remote_success_writes_cache() {
        let cache = tmp_cache("remote");
        let f = FakeFetcher {
            result: Ok(sample_catalog()),
            calls: RefCell::new(vec![]),
        };
        let al = Allowlist::new(["dl.example.com"]);
        let r = fetch_catalog(&f, &al, "https://dl.example.com/catalog.json", &cache);
        assert!(matches!(r, CatalogResult::Remote(ref e) if e.len() == 1));
        assert!(cache.exists(), "应写入缓存");
        std::fs::remove_dir_all(cache.parent().unwrap()).ok();
    }

    #[test]
    fn fetch_falls_back_to_cache_when_offline() {
        let cache = tmp_cache("offline");
        std::fs::write(&cache, sample_catalog()).unwrap();
        let f = FakeFetcher {
            result: Err("无网络".into()),
            calls: RefCell::new(vec![]),
        };
        let al = Allowlist::new(["dl.example.com"]);
        let r = fetch_catalog(&f, &al, "https://dl.example.com/catalog.json", &cache);
        assert!(matches!(r, CatalogResult::Cached(ref e) if e.len() == 1));
        std::fs::remove_dir_all(cache.parent().unwrap()).ok();
    }

    #[test]
    fn fetch_unavailable_when_no_cache() {
        let cache = tmp_cache("nocache");
        let f = FakeFetcher {
            result: Err("无网络".into()),
            calls: RefCell::new(vec![]),
        };
        let al = Allowlist::new(["dl.example.com"]);
        let r = fetch_catalog(&f, &al, "https://dl.example.com/catalog.json", &cache);
        assert!(matches!(r, CatalogResult::Unavailable(_)));
        std::fs::remove_dir_all(cache.parent().unwrap()).ok();
    }

    #[test]
    fn fetch_blocked_by_allowlist_does_not_call_network() {
        let cache = tmp_cache("blocked");
        let f = FakeFetcher {
            result: Ok(sample_catalog()),
            calls: RefCell::new(vec![]),
        };
        let al = Allowlist::new(["dl.example.com"]);
        let r = fetch_catalog(&f, &al, "https://evil.com/catalog.json", &cache);
        // 关键：白名单拒绝时不应发出任何请求
        assert!(f.calls.borrow().is_empty(), "白名单拦截后不得发起网络请求");
        assert!(matches!(r, CatalogResult::Unavailable(_)));
        std::fs::remove_dir_all(cache.parent().unwrap()).ok();
    }
}
