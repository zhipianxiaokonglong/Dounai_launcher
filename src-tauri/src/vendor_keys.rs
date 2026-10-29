//! 厂商密钥的统一存储与注入。
//!
//! ## 为什么需要这个模块
//!
//! 三个上游各自用**进程环境变量**读密钥，但周边机制互不相同：
//! - 输入层：只认环境变量，且明确「Secrets 永不从 TOML 读」，不加载 `.env`；
//! - 模型层：`load_dotenv(cwd/.env, override=false)`，真实环境变量优先；
//! - 语音层：`Secrets` 只从环境变量加载，`.env` 只是方便的填充手段。
//!
//! 结果是用户要维护三套互不兼容的配置，还要记得每层缺哪个变量。
//! 部门解散后没人做统一入口，启动器补上：**保存唯一副本，启动模块时注入 env**。
//!
//! 选注入而不是写文件，理由有三：
//! 1. 三方都认环境变量，注入可覆盖全部三层，无需改上游代码；
//! 2. 不会往用户的仓库目录里写 `.env`（那里是人家自己的 git 仓库）；
//! 3. 输入层根本不读 `.env`，写文件对它无效。
//!
//! ## 安全约束
//!
//! - 密钥值**只在 Rust 侧使用，从不返回给前端**：`overview` 只给「已配置」布尔值。
//! - 写入时校验变量名与值（禁止换行），避免被当作多行环境注入。
//! - 落盘为 JSON；Unix 下收紧为 0600，Windows 依赖用户目录 ACL。
//! - 任何错误信息与日志都不得包含密钥值。

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// 已知的三层（模块 id 是跨部门的固定约定）。
pub const LAYERS: [LayerSpec; 3] = [
    LayerSpec {
        module_id: "dounai-input",
        label: "输入层",
        catalog_path: "/v1/providers",
    },
    LayerSpec {
        module_id: "omninexus",
        label: "模型层",
        catalog_path: "/v1/providers",
    },
    LayerSpec {
        module_id: "moodvoice",
        label: "语音层",
        catalog_path: "/api/health",
    },
];

#[derive(Debug, Clone, Copy)]
pub struct LayerSpec {
    pub module_id: &'static str,
    pub label: &'static str,
    /// 取「厂商清单」的 HTTP 路径（各层约定不同，这里固化下来）
    pub catalog_path: &'static str,
}

pub fn layer_of(module_id: &str) -> Option<LayerSpec> {
    LAYERS.iter().copied().find(|l| l.module_id == module_id)
}

/// 一个厂商的展示信息（**不含任何密钥值**）。
#[derive(Debug, Clone, Serialize)]
pub struct ProviderView {
    pub id: String,
    pub label: String,
    /// 上游报告自己可用
    pub ready: bool,
    /// 还需要哪些环境变量（名字，不含值）
    pub missing: Vec<String>,
    /// 这些缺失的变量里，有几个已经由启动器保存
    pub saved: Vec<String>,
    pub privacy: String,
}

/// 某一层的总览。
#[derive(Debug, Clone, Serialize)]
pub struct LayerView {
    pub module_id: String,
    pub label: String,
    /// 模块是否已登记/安装
    pub installed: bool,
    /// 进程是否在运行（不运行就取不到清单）
    pub running: bool,
    pub providers: Vec<ProviderView>,
    /// 取清单失败时的原因（人话）
    pub error: Option<String>,
}

/* ------------------------------------------------------------------ */
/* 密钥存储                                                            */
/* ------------------------------------------------------------------ */

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreFile {
    #[serde(default)]
    version: u32,
    /// layer -> env_name -> value
    #[serde(default)]
    layers: BTreeMap<String, BTreeMap<String, String>>,
}

/// 密钥仓库：`data_root/vendor-keys.json`。
#[derive(Debug)]
pub struct VendorStore {
    path: PathBuf,
    data: StoreFile,
}

impl VendorStore {
    /// 打开（不存在则视为空）。
    pub fn open(data_root: &Path) -> Result<Self, String> {
        let path = data_root.join("vendor-keys.json");
        let data = if path.exists() {
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("读取密钥库失败：{e}"))?;
            if text.trim().is_empty() {
                StoreFile::default()
            } else {
                serde_json::from_str(&text).map_err(|e| format!("密钥库格式损坏：{e}"))?
            }
        } else {
            StoreFile::default()
        };
        Ok(Self { path, data })
    }

    fn save(&self) -> Result<(), String> {
        let mut out = serde_json::to_vec_pretty(&self.data).map_err(|e| e.to_string())?;
        out.push(b'\n');
        // 原子写：先写临时文件再改名，避免断电留下半个文件把密钥库弄坏
        let tmp = self.path.with_extension("json.tmp");
        {
            let mut f = std::fs::File::create(&tmp).map_err(|e| format!("写入密钥库失败：{e}"))?;
            f.write_all(&out)
                .map_err(|e| format!("写入密钥库失败：{e}"))?;
            f.sync_all().ok();
        }
        std::fs::rename(&tmp, &self.path).map_err(|e| format!("替换密钥库失败：{e}"))?;
        restrict_permissions(&self.path);
        Ok(())
    }

    /// 保存一个密钥。`env_name` 必须像环境变量名，值不得含换行。
    pub fn set(&mut self, layer: &str, env_name: &str, value: &str) -> Result<(), String> {
        let spec = layer_of(layer).ok_or_else(|| format!("未知的层：{layer}"))?;
        let _ = spec;
        let name = env_name.trim();
        if !is_env_name(name) {
            return Err(format!(
                "环境变量名不合法：{name}（应形如 DASHSCOPE_API_KEY）"
            ));
        }
        let val = value.trim();
        if val.is_empty() {
            return Err("密钥不能为空".to_string());
        }
        if val.contains('\n') || val.contains('\r') || val.contains('\0') {
            return Err("密钥不能包含换行或空字符".to_string());
        }
        self.data
            .layers
            .entry(layer.to_string())
            .or_default()
            .insert(name.to_string(), val.to_string());
        self.data.version = 1;
        self.save()
    }

    /// 清除一个密钥（幂等）。
    pub fn clear(&mut self, layer: &str, env_name: &str) -> Result<(), String> {
        layer_of(layer).ok_or_else(|| format!("未知的层：{layer}"))?;
        if let Some(m) = self.data.layers.get_mut(layer) {
            m.remove(env_name.trim());
            if m.is_empty() {
                self.data.layers.remove(layer);
            }
        }
        self.save()
    }

    /// 该层已保存的变量名（**不含值**），供前端显示"已配置"。
    pub fn saved_names(&self, layer: &str) -> Vec<String> {
        self.data
            .layers
            .get(layer)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub fn has(&self, layer: &str, env_name: &str) -> bool {
        self.data
            .layers
            .get(layer)
            .map(|m| m.contains_key(env_name))
            .unwrap_or(false)
    }

    /// 供启动模块时注入的完整环境变量。
    pub fn env_for(&self, layer: &str) -> Vec<(String, String)> {
        self.data
            .layers
            .get(layer)
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }
}

fn is_env_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() => {}
        _ => return false,
    }
    s.len() <= 128 && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {
    // Windows：文件位于用户 AppData 下，依赖目录 ACL；如需更强保护应改用 DPAPI 加密。
}

/* ------------------------------------------------------------------ */
/* 厂商清单解析（纯函数，便于离线单测）                                 */
/* ------------------------------------------------------------------ */

/// 从各层的目录端点解析出厂商视图。
///
/// 三层的返回结构不同，这里分别适配：
/// - 输入层：`{providers:[{id,name,available,requires_api_key,unavailable_reason}]}`
///   —— 缺哪个变量只能从 `unavailable_reason`（"Set XXX to use this provider."）里抠；
/// - 模型层：`{providers:[{name,label,requires_key,configured,api_key_env,local}]}`
/// - 语音层：`{providers:[{id,ready,missing:[...]}]}`
pub fn parse_catalog(
    layer: &str,
    body: &str,
    saved: &[String],
) -> Result<Vec<ProviderView>, String> {
    // 先校验层名：否则 providers 为空数组时循环不执行，未知层会被当成合法输入
    if layer_of(layer).is_none() {
        return Err(format!("未知的层：{layer}"));
    }
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("上游返回的不是 JSON：{e}"))?;
    let arr = v
        .get("providers")
        .and_then(|p| p.as_array())
        .ok_or_else(|| "上游返回里没有 providers 字段".to_string())?;

    let saved_set: std::collections::BTreeSet<&str> = saved.iter().map(|s| s.as_str()).collect();

    let mut out = Vec::with_capacity(arr.len());
    for p in arr {
        let (id, label, ready, missing, privacy) = match layer {
            "dounai-input" => {
                let id = str_of(p, "id");
                let label = str_of(p, "name");
                let requires = p
                    .get("requires_api_key")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false);
                let available = p.get("available").and_then(|x| x.as_bool()).unwrap_or(true);
                let reason = str_of(p, "unavailable_reason");
                let mut missing = Vec::new();
                if requires && !available {
                    if let Some(name) = extract_env_name(&reason) {
                        missing.push(name);
                    }
                }
                let privacy = str_of(p, "privacy");
                (id, label, available, missing, privacy)
            }
            "omninexus" => {
                let id = str_of(p, "name");
                let label = str_of(p, "label");
                let requires = p
                    .get("requires_key")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false);
                let configured = p
                    .get("configured")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false);
                let local = p.get("local").and_then(|x| x.as_bool()).unwrap_or(false);
                let mut missing = Vec::new();
                if requires && !configured {
                    let env = str_of(p, "api_key_env");
                    if !env.is_empty() {
                        missing.push(env);
                    }
                }
                let ready = !requires || configured || local;
                // 本地推理服务（ollama / vllm …）不该被标成 remote
                let privacy = if local { "local" } else { "remote" };
                (id, label, ready, missing, privacy.to_string())
            }
            "moodvoice" => {
                let id = str_of(p, "id");
                let label = str_of(p, "display_name");
                let ready = p.get("ready").and_then(|x| x.as_bool()).unwrap_or(false);
                let missing = p
                    .get("missing")
                    .and_then(|m| m.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str())
                            .filter(|s| is_env_name(s))
                            .map(|s| s.to_string())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                (id, label, ready, missing, "remote".to_string())
            }
            other => return Err(format!("未知的层：{other}")),
        };

        let saved_hit: Vec<String> = missing
            .iter()
            .filter(|m| saved_set.contains(m.as_str()))
            .cloned()
            .collect();

        out.push(ProviderView {
            id: id.clone(),
            label: if label.is_empty() { id } else { label },
            ready,
            missing,
            saved: saved_hit,
            privacy,
        });
    }
    Ok(out)
}

fn str_of(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

/// 从 "Set DASHSCOPE_API_KEY to use this provider." 里取出变量名。
fn extract_env_name(text: &str) -> Option<String> {
    let mut word = String::new();
    for ch in text.chars() {
        if ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_' {
            word.push(ch);
        } else {
            if is_env_name(&word) && word.contains('_') {
                return Some(word);
            }
            word.clear();
        }
    }
    if is_env_name(&word) && word.contains('_') {
        return Some(word);
    }
    None
}

/* ------------------------------------------------------------------ */
/* 本机 HTTP（只用于 127.0.0.1 的厂商目录）                             */
/* ------------------------------------------------------------------ */

/// 从健康地址推出 `http://host:port`
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
/// 而这里只是读本机服务的清单，语义不同、不该混用。
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
    // 只允许本机，杜绝被当成任意出网原语
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
    fn env_name_validation() {
        assert!(is_env_name("DASHSCOPE_API_KEY"));
        assert!(is_env_name("A"));
        assert!(!is_env_name("dashscope_api_key"));
        assert!(!is_env_name("1ABC"));
        assert!(!is_env_name("A B"));
        assert!(!is_env_name(""));
        assert!(!is_env_name("A-B"));
    }

    #[test]
    fn store_set_clear_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "dounai-vendor-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let mut s = VendorStore::open(&dir).unwrap();
        s.set("moodvoice", "OPENAI_API_KEY", "sk-test-123").unwrap();
        assert!(s.has("moodvoice", "OPENAI_API_KEY"));
        assert_eq!(
            s.saved_names("moodvoice"),
            vec!["OPENAI_API_KEY".to_string()]
        );
        assert_eq!(
            s.env_for("moodvoice"),
            vec![("OPENAI_API_KEY".to_string(), "sk-test-123".to_string())]
        );

        // 重新打开应仍在（落盘验证）
        let s2 = VendorStore::open(&dir).unwrap();
        assert_eq!(s2.env_for("moodvoice").len(), 1);

        // 清除
        let mut s3 = VendorStore::open(&dir).unwrap();
        s3.clear("moodvoice", "OPENAI_API_KEY").unwrap();
        assert!(!s3.has("moodvoice", "OPENAI_API_KEY"));
        assert!(VendorStore::open(&dir)
            .unwrap()
            .env_for("moodvoice")
            .is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn store_rejects_bad_input() {
        let dir = std::env::temp_dir().join(format!(
            "dounai-vendor-bad-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = VendorStore::open(&dir).unwrap();

        assert!(s.set("unknown-layer", "A_B", "v").is_err());
        assert!(s.set("omninexus", "lower_case", "v").is_err());
        assert!(s.set("omninexus", "OK_KEY", "").is_err());
        assert!(s.set("omninexus", "OK_KEY", "a\nb").is_err());
        assert!(s.set("omninexus", "OK_KEY", "a\rb").is_err());
        // 合法值 trim 后保存
        assert!(s.set("omninexus", "OK_KEY", "  spaced  ").is_ok());
        assert_eq!(
            s.env_for("omninexus"),
            vec![("OK_KEY".to_string(), "spaced".to_string())]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_input_catalog() {
        // 输入层：缺哪个变量只能从 unavailable_reason 里抠
        let body = r#"{"providers":[
            {"id":"offline","name":"离线直通","available":true,"requires_api_key":false,"unavailable_reason":"","privacy":"local"},
            {"id":"dashscope","name":"通义千问","available":false,"requires_api_key":true,
             "unavailable_reason":"Set DASHSCOPE_API_KEY to use this provider.","privacy":"remote"}
        ]}"#;
        let v = parse_catalog("dounai-input", body, &["DASHSCOPE_API_KEY".to_string()]).unwrap();
        assert_eq!(v.len(), 2);
        assert!(v[0].ready && v[0].missing.is_empty());
        assert!(!v[1].ready);
        assert_eq!(v[1].missing, vec!["DASHSCOPE_API_KEY".to_string()]);
        assert_eq!(v[1].saved, vec!["DASHSCOPE_API_KEY".to_string()]);
        assert_eq!(v[1].privacy, "remote");
    }

    #[test]
    fn parses_omninexus_catalog() {
        let body = r#"{"providers":[
            {"name":"openai","label":"OpenAI","requires_key":true,"configured":false,"api_key_env":"OPENAI_API_KEY","local":false},
            {"name":"echo","label":"Echo","requires_key":false,"configured":true,"api_key_env":"","local":true}
        ]}"#;
        let v = parse_catalog("omninexus", body, &[]).unwrap();
        assert!(!v[0].ready);
        assert_eq!(v[0].missing, vec!["OPENAI_API_KEY".to_string()]);
        assert!(v[1].ready, "本地 provider 不需要密钥也应视为可用");
        assert_eq!(v[1].privacy, "local", "本地推理服务应标为 local");
    }

    #[test]
    fn parses_moodvoice_catalog() {
        let body = r#"{"providers":[
            {"id":"edge","display_name":"edge-tts","ready":true,"missing":[]},
            {"id":"openai","display_name":"OpenAI","ready":false,"missing":["OPENAI_API_KEY"]},
            {"id":"elevenlabs","display_name":"ElevenLabs","ready":false,
             "missing":["ELEVENLABS_API_KEY","providers.elevenlabs.voice_id"]}
        ]}"#;
        let v = parse_catalog("moodvoice", body, &["OPENAI_API_KEY".to_string()]).unwrap();
        assert!(v[0].ready);
        assert_eq!(v[1].missing, vec!["OPENAI_API_KEY".to_string()]);
        assert_eq!(v[1].saved, vec!["OPENAI_API_KEY".to_string()]);
        // 非环境变量形式的配置项（providers.elevenlabs.voice_id）要过滤掉
        assert_eq!(
            v[2].missing,
            vec!["ELEVENLABS_API_KEY".to_string()],
            "只保留像环境变量的项"
        );
    }

    #[test]
    fn catalog_rejects_malformed() {
        assert!(parse_catalog("omninexus", "not json", &[]).is_err());
        assert!(parse_catalog("omninexus", r#"{"nope":[]}"#, &[]).is_err());
        assert!(parse_catalog("unknown", r#"{"providers":[]}"#, &[]).is_err());
    }

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
    }

    #[test]
    fn local_http_refuses_non_loopback() {
        // 防止这个函数被当成任意出网原语
        let err = http_get_local("http://example.com/x", Duration::from_millis(200)).unwrap_err();
        assert!(err.contains("只允许访问本机"), "实际：{err}");
    }
}
