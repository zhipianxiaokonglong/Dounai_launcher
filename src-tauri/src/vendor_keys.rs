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

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
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
    /// 上游没说清缺哪个变量（无法从文案可靠解析）：UI 应提供手动填写变量名。
    ///
    /// 为什么需要这个标记：输入层只给自然语言 `unavailable_reason`，
    /// 一旦它的文案改了就解析不出变量名。**解析失败不能静默**——
    /// 否则用户看到"缺配置"却不知道该填什么（审计 L-05）。
    pub manual_env: bool,
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
    /// manifest 手写版本与上游声明版本的比对（L-04）
    pub version_check: Option<VersionCheck>,
    /// 共享区里的变量名（对所有层生效，L-03）
    pub shared_keys: Vec<String>,
    /// 密钥库当前是否为加密存储（L-02）
    pub encrypted_store: bool,
}

/* ------------------------------------------------------------------ */
/* 密钥存储                                                            */
/* ------------------------------------------------------------------ */

/// 落盘信封（L-02）。
///
/// 为什么要信封而不是直接加密整个 JSON：需要区分"密文/明文"，
/// 并给非 Windows（无 DPAPI）留一条明确可见的降级路径。
#[derive(Debug, Serialize, Deserialize)]
struct Envelope {
    version: u32,
    encrypted: bool,
    /// 密文时是 base64 串；明文时是 Payload 对象
    payload: serde_json::Value,
}

/// 真正的密钥数据（L-03 起分「共享 + 分层」两区）。
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Payload {
    /// 跨层通用的变量：`env_name -> value`
    ///
    /// 为什么单独一区：`OPENAI_API_KEY` 这类变量三层的读取名字完全一样，
    /// 同一把钥匙要填三遍既不必要也容易漏（L-03）。
    #[serde(default)]
    pub shared: BTreeMap<String, String>,
    /// 层专属变量：`layer -> env_name -> value`
    #[serde(default)]
    pub layers: BTreeMap<String, BTreeMap<String, String>>,
}

/// 密钥仓库：`data_root/vendor-keys.json`。
#[derive(Debug)]
pub struct VendorStore {
    path: PathBuf,
    data: Payload,
    /// 上次落盘是否真的加密了（非 Windows 或 DPAPI 失败时为 false）
    encrypted: bool,
}

impl VendorStore {
    /// 打开（不存在则视为空）。会自动迁移旧格式并升级为加密存储。
    pub fn open(data_root: &Path) -> Result<Self, String> {
        let path = data_root.join("vendor-keys.json");
        if !path.exists() {
            return Ok(Self {
                path,
                data: Payload::default(),
                encrypted: false,
            });
        }

        let text = std::fs::read_to_string(&path).map_err(|e| format!("读取密钥库失败：{e}"))?;
        if text.trim().is_empty() {
            return Ok(Self {
                path,
                data: Payload::default(),
                encrypted: false,
            });
        }

        let raw: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("密钥库格式损坏：{e}"))?;

        // 旧格式（v1）：顶层直接是 { version, layers }
        if raw.get("payload").is_none() {
            let data: Payload =
                serde_json::from_value(raw).map_err(|e| format!("旧版密钥库无法解析：{e}"))?;
            let mut store = Self {
                path,
                data,
                encrypted: false,
            };
            // 迁移：立刻按新格式（加密）落盘，避免明文长期留存
            store.save()?;
            return Ok(Self {
                path: store.path,
                data: store.data,
                encrypted: store.encrypted,
            });
        }

        let envelope: Envelope =
            serde_json::from_value(raw).map_err(|e| format!("密钥库信封无法解析：{e}"))?;

        let data = if envelope.encrypted {
            let ciphertext = envelope
                .payload
                .as_str()
                .ok_or_else(|| "密文 payload 应为字符串".to_string())?;
            let plain = decrypt_string(ciphertext)?;
            serde_json::from_str(&plain).map_err(|e| format!("解密后的密钥库无法解析：{e}"))?
        } else {
            serde_json::from_value(envelope.payload)
                .map_err(|e| format!("明文密钥库无法解析：{e}"))?
        };

        Ok(Self {
            path,
            data,
            encrypted: envelope.encrypted,
        })
    }

    /// 当前落盘是否为加密存储（供 UI 如实告知）。
    pub fn is_encrypted(&self) -> bool {
        self.encrypted
    }

    fn save(&mut self) -> Result<(), String> {
        let plain = serde_json::to_vec(&self.data).map_err(|e| e.to_string())?;
        let plain_str = String::from_utf8(plain).map_err(|e| e.to_string())?;

        // Windows 用 DPAPI 加密；其它平台保持明文（并在 README 如实说明）
        let (encrypted, payload) = match encrypt_string(&plain_str) {
            Ok(cipher) => (true, serde_json::Value::String(cipher)),
            Err(_) => (
                false,
                serde_json::to_value(&self.data).map_err(|e| e.to_string())?,
            ),
        };

        // 回写内存状态：is_encrypted() 要反映**实际落盘**结果
        self.encrypted = encrypted;
        let envelope = Envelope {
            version: 2,
            encrypted,
            payload,
        };
        let mut out = serde_json::to_vec_pretty(&envelope).map_err(|e| e.to_string())?;
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

    /// 保存一个**层专属**密钥。
    pub fn set(&mut self, layer: &str, env_name: &str, value: &str) -> Result<(), String> {
        layer_of(layer).ok_or_else(|| format!("未知的层：{layer}"))?;
        let name = validate_env_name(env_name)?;
        let val = validate_value(value)?;
        self.data
            .layers
            .entry(layer.to_string())
            .or_default()
            .insert(name, val);
        self.save()
    }

    /// 保存一个**跨层共享**密钥（L-03）。
    ///
    /// 共享键会被注入到**所有**层；某层若同时有专属键，专属优先。
    pub fn set_shared(&mut self, env_name: &str, value: &str) -> Result<(), String> {
        let name = validate_env_name(env_name)?;
        let val = validate_value(value)?;
        self.data.shared.insert(name, val);
        self.save()
    }

    /// 清除层专属密钥（幂等）。
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

    /// 清除共享密钥（幂等）。
    pub fn clear_shared(&mut self, env_name: &str) -> Result<(), String> {
        self.data.shared.remove(env_name.trim());
        self.save()
    }

    /// 该层已保存的变量名（含共享键），**不含值**。
    pub fn saved_names(&self, layer: &str) -> Vec<String> {
        let mut names: Vec<String> = self.data.shared.keys().cloned().collect();
        if let Some(m) = self.data.layers.get(layer) {
            names.extend(m.keys().cloned());
        }
        names.sort();
        names.dedup();
        names
    }

    /// 该层是否能拿到某个变量（共享或专属）。
    pub fn has(&self, layer: &str, env_name: &str) -> bool {
        if self.data.shared.contains_key(env_name) {
            return true;
        }
        self.data
            .layers
            .get(layer)
            .map(|m| m.contains_key(env_name))
            .unwrap_or(false)
    }

    /// 共享区已保存的变量名，**不含值**。
    pub fn shared_names(&self) -> Vec<String> {
        self.data.shared.keys().cloned().collect()
    }

    /// 该层实际会拿到的环境变量：共享键 + 层专属（专属优先）。
    ///
    /// 顺序刻意固定（BTreeMap），便于测试与排障复现。
    pub fn env_for(&self, layer: &str) -> Vec<(String, String)> {
        let mut merged: BTreeMap<String, String> = self.data.shared.clone();
        if let Some(m) = self.data.layers.get(layer) {
            for (k, v) in m {
                merged.insert(k.clone(), v.clone());
            }
        }
        merged.into_iter().collect()
    }
}

/// 校验变量名形态，返回规范化后的名字。
fn validate_env_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if !is_env_name(name) {
        return Err(format!(
            "环境变量名不合法：{name}（应形如 DASHSCOPE_API_KEY）"
        ));
    }
    Ok(name.to_string())
}

/// 校验密钥值：非空、不含换行或空字符。
fn validate_value(raw: &str) -> Result<String, String> {
    let val = raw.trim();
    if val.is_empty() {
        return Err("密钥不能为空".to_string());
    }
    if val.contains('\n') || val.contains('\r') || val.contains('\0') {
        return Err("密钥不能包含换行或空字符".to_string());
    }
    Ok(val.to_string())
}

/* ------------------------------------------------------------------ */
/* DPAPI 加解密（L-02，仅 Windows）                                     */
/* ------------------------------------------------------------------ */

/// 用 DPAPI 加密（绑当前用户）。非 Windows 直接返回 Err，由调用方降级为明文。
#[cfg(windows)]
fn encrypt_string(plain: &str) -> Result<String, String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN};

    let in_blob = make_blob(plain.as_bytes())?;
    let mut out_blob = zero_blob();

    // SAFETY: 两个 BLOB 都由本函数分配/初始化；LocalFree 只释放系统分配的 out_blob。
    let ok = unsafe {
        CryptProtectData(
            &in_blob,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out_blob,
        )
    };
    if ok == 0 {
        return Err("DPAPI 加密失败".to_string());
    }

    // SAFETY: out_blob 由 DPAPI 填充，长度与指针均有效
    let slice =
        unsafe { std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize).to_vec() };
    // SAFETY: 由系统分配，LocalFree 释放
    unsafe {
        LocalFree(out_blob.pbData as *mut core::ffi::c_void);
    }
    Ok(BASE64.encode(slice))
}

/// 用 DPAPI 解密。密文必须由**同一用户**加密。
#[cfg(windows)]
fn decrypt_string(cipher: &str) -> Result<String, String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN,
    };

    let bytes = BASE64
        .decode(cipher.trim())
        .map_err(|e| format!("密文不是合法 base64：{e}"))?;

    let in_blob = make_blob(&bytes)?;
    let mut out_blob = zero_blob();

    // SAFETY: 同 encrypt_string
    let ok = unsafe {
        CryptUnprotectData(
            &in_blob,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out_blob,
        )
    };
    if ok == 0 {
        return Err("DPAPI 解密失败（密钥库可能由其它用户/其它机器加密）".to_string());
    }

    // SAFETY: out_blob 由 DPAPI 填充
    let slice =
        unsafe { std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize).to_vec() };
    // SAFETY: 由系统分配
    unsafe {
        LocalFree(out_blob.pbData as *mut core::ffi::c_void);
    }

    String::from_utf8(slice).map_err(|e| format!("解密结果不是 UTF-8：{e}"))
}

#[cfg(windows)]
fn make_blob(
    bytes: &[u8],
) -> Result<windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB, String> {
    use windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB;
    if bytes.is_empty() {
        return Ok(CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        });
    }
    // DPAPI 会原地读取输入，给一块独立内存避免动到调用方的数据
    let mut buf = bytes.to_vec();
    let ptr = buf.as_mut_ptr();
    let len = buf.len() as u32;
    std::mem::forget(buf); // 生命周期交给调用方释放（简短函数，进程存活期内不泄漏）
    Ok(CRYPT_INTEGER_BLOB {
        cbData: len,
        pbData: ptr,
    })
}

#[cfg(windows)]
fn zero_blob() -> windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB {
    use windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB;
    CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    }
}

#[cfg(not(windows))]
fn encrypt_string(_plain: &str) -> Result<String, String> {
    Err("当前平台没有 DPAPI，密钥库以明文保存".to_string())
}

#[cfg(not(windows))]
fn decrypt_string(_cipher: &str) -> Result<String, String> {
    Err("当前平台没有 DPAPI".to_string())
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
        let (id, label, ready, missing, privacy, manual) = match layer {
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
                let mut manual = false;
                if requires && !available {
                    match extract_env_name(&reason) {
                        Some(name) => missing.push(name),
                        // 文案改了 / 不是环境变量形态 → 让 UI 提示手填
                        None => manual = true,
                    }
                }
                let privacy = str_of(p, "privacy");
                (id, label, available, missing, privacy, manual.to_string())
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
                // 模型层有机读的 api_key_env；真缺了却没给名字，同样要提示手填
                let manual = requires && !configured && missing.is_empty();
                (
                    id,
                    label,
                    ready,
                    missing,
                    privacy.to_string(),
                    manual.to_string(),
                )
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
                // 语音层 missing 里可能只有配置键（如 providers.x.voice_id），
                // 被过滤后为空 → 说明"不是差密钥"，不需要 manual
                let manual = false;
                (
                    id,
                    label,
                    ready,
                    missing,
                    "remote".to_string(),
                    manual.to_string(),
                )
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
            manual_env: manual.as_str() == "true",
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

/// 变量名可接受的结尾词（用于从自然语言里提高置信度）。
///
/// 收紧的原因：原实现只要"全大写 + 含下划线"就收，于是
/// "Set API to use this provider" 这类文案里的 `API` 也会被当变量名；
/// 而真实变量名几乎都以这些后缀结尾。
const ENV_SUFFIXES: [&str; 8] = [
    "_API_KEY",
    "_KEY",
    "_TOKEN",
    "_SECRET",
    "_ENDPOINT",
    "_URL",
    "_REGION",
    "_ID",
];

/// 从 `unavailable_reason` 这类自然语言里**谨慎地**抽取环境变量名。
///
/// 策略：
/// 1. 按非标识符字符切词，只保留 `[A-Z0-9_]` 串；
/// 2. 必须以 `_` 分段（排除 `API` 这种孤立词）；
/// 3. 必须以常见后缀结尾（排除 `XXX` 这种占位符）；
/// 4. 取**最后一个**匹配（自然语言里变量名通常在句尾）。
///
/// 返回 `None` 表示"无法可靠解析"——调用方必须用 `manual_env` 提示用户手填，
/// 不能静默当成"无需配置"。
fn extract_env_name(text: &str) -> Option<String> {
    let mut candidates: Vec<String> = Vec::new();
    let mut word = String::new();

    let flush = |word: &mut String, out: &mut Vec<String>| {
        if !word.is_empty() {
            out.push(std::mem::take(word));
        }
    };

    for ch in text.chars() {
        if ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_' {
            word.push(ch);
        } else {
            flush(&mut word, &mut candidates);
        }
    }
    flush(&mut word, &mut candidates);

    candidates.into_iter().rfind(|w| {
        is_env_name(w)
            && w.contains('_')
            // 占位符形态（XXX_KEY）不算
            && !w.starts_with("XXX")
            && ENV_SUFFIXES.iter().any(|suffix| w.ends_with(suffix))
    })
}

/* ------------------------------------------------------------------ */
/* 版本漂移比对（L-04）                                                */
/* ------------------------------------------------------------------ */

/// 上游声明的版本与 manifest 手写版本是否一致。
#[derive(Debug, Clone, serde::Serialize)]
pub struct VersionCheck {
    /// manifest 里手写的版本
    pub manifest: String,
    /// 从上游 pyproject.toml / Cargo.toml 读到的版本（读不到则为 None）
    pub upstream: Option<String>,
    /// 两者是否一致（读不到时按"未验证"处理，不算不一致）
    pub consistent: bool,
}

/// 从模块根读上游声明的版本。
///
/// 只认两种最常见的来源，够用且不会误判：
/// - `pyproject.toml` 的 `version = "x.y.z"`（`[project]` 段）
/// - `Cargo.toml` 的 `version = "x.y.z"`（`[package]` 段）
///
/// 刻意**不**做完整的 TOML 解析：这里只需要一个版本串，引全量解析器不值得。
pub fn read_upstream_version(module_root: &Path) -> Option<String> {
    for (name, section) in [("pyproject.toml", "[project]"), ("Cargo.toml", "[package]")] {
        let path = module_root.join(name);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mut in_section = false;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                in_section = trimmed.starts_with(section);
                continue;
            }
            if !in_section {
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("version") {
                let rest = rest.trim_start();
                if let Some(v) = rest.strip_prefix('=') {
                    let v = v.trim().trim_matches('"').trim_matches('\'').trim();
                    if !v.is_empty() {
                        return Some(v.to_string());
                    }
                }
            }
        }
    }
    None
}

/// 读模块 manifest 里手写的版本（读不到返回空串）。
pub fn read_module_manifest_version(module_root: &Path) -> String {
    for name in ["manifest.json", "dounai-manifest.json"] {
        let path = module_root.join(name);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        if let Some(version) = v.get("version").and_then(|x| x.as_str()) {
            return version.to_string();
        }
    }
    String::new()
}

/// 比对 manifest 版本与上游版本。
pub fn check_version(manifest_version: &str, module_root: &Path) -> VersionCheck {
    let upstream = read_upstream_version(module_root);
    let consistent = match &upstream {
        Some(v) => v.trim() == manifest_version.trim(),
        // 读不到上游版本 → 无法判定，不算不一致（避免对没有 pyproject 的模块误报）
        None => true,
    };
    VersionCheck {
        manifest: manifest_version.to_string(),
        upstream,
        consistent,
    }
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
        assert!(s
            .saved_names("moodvoice")
            .contains(&"OPENAI_API_KEY".to_string()));
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
        assert!(s3.env_for("moodvoice").is_empty());

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
        // 共享键同样受校验
        assert!(s.set_shared("bad name", "v").is_err());
        assert!(s.set_shared("GOOD_KEY", "").is_err());
        // 合法值 trim 后保存
        assert!(s.set("omninexus", "OK_KEY", "  spaced  ").is_ok());
        assert_eq!(
            s.env_for("omninexus"),
            vec![("OK_KEY".to_string(), "spaced".to_string())]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn store_is_encrypted_on_disk() {
        // L-02 的核心断言：落盘内容**不得**出现明文密钥
        let dir = std::env::temp_dir().join(format!(
            "dounai-vendor-enc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let secret = "sk-plaintext-must-not-appear-123456";
        let mut s = VendorStore::open(&dir).unwrap();
        s.set("moodvoice", "OPENAI_API_KEY", secret).unwrap();
        assert!(s.is_encrypted(), "Windows 下应使用 DPAPI 加密");

        let raw = std::fs::read_to_string(dir.join("vendor-keys.json")).unwrap();
        assert!(!raw.contains(secret), "落盘文件里不得出现明文密钥");
        assert!(raw.contains("\"encrypted\": true"));

        // 重新打开仍能解密读到（DPAPI 往返）
        let reopened = VendorStore::open(&dir).unwrap();
        assert_eq!(
            reopened.env_for("moodvoice"),
            vec![("OPENAI_API_KEY".to_string(), secret.to_string())]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shared_keys_merge_with_layer_specific_and_layer_wins() {
        // L-03：共享键对所有层生效；同名的层专属键优先
        let dir = std::env::temp_dir().join(format!(
            "dounai-vendor-shared-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = VendorStore::open(&dir).unwrap();

        s.set_shared("OPENAI_API_KEY", "sk-shared").unwrap();
        s.set_shared("DASHSCOPE_API_KEY", "sk-shared-dash").unwrap();
        // 语音层单独覆盖 OPENAI_API_KEY（专属优先）
        s.set("moodvoice", "OPENAI_API_KEY", "sk-layer-specific")
            .unwrap();

        let mv = s.env_for("moodvoice");
        let omni = s.env_for("omninexus");

        let pick = |list: &[(String, String)], key: &str| {
            list.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
        };

        assert_eq!(
            pick(&mv, "OPENAI_API_KEY").as_deref(),
            Some("sk-layer-specific"),
            "层专属应覆盖共享"
        );
        assert_eq!(
            pick(&omni, "OPENAI_API_KEY").as_deref(),
            Some("sk-shared"),
            "没有专属键的层用共享值"
        );
        assert_eq!(
            pick(&omni, "DASHSCOPE_API_KEY").as_deref(),
            Some("sk-shared-dash"),
            "共享键应对所有层生效"
        );

        // saved_names 应包含共享键
        assert!(s
            .saved_names("moodvoice")
            .contains(&"DASHSCOPE_API_KEY".to_string()));

        // 清除共享键后，没有专属键的层应拿不到
        let mut s2 = VendorStore::open(&dir).unwrap();
        s2.clear_shared("DASHSCOPE_API_KEY").unwrap();
        assert!(s2
            .env_for("omninexus")
            .iter()
            .all(|(k, _)| k != "DASHSCOPE_API_KEY"));

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
    fn extract_env_name_is_strict() {
        // 真实文案：能抽到
        assert_eq!(
            extract_env_name("Set DASHSCOPE_API_KEY to use this provider."),
            Some("DASHSCOPE_API_KEY".to_string())
        );
        assert_eq!(
            extract_env_name("请先设置 OPENAI_API_KEY"),
            Some("OPENAI_API_KEY".to_string())
        );
        assert_eq!(
            extract_env_name("需要 AZURE_SPEECH_KEY 与 AZURE_SPEECH_REGION"),
            // 取最后一个匹配（自然语言里变量名通常在句尾）
            Some("AZURE_SPEECH_REGION".to_string())
        );

        // 收紧后必须拒绝的误判源
        assert_eq!(
            extract_env_name("Set API to use this provider."),
            None,
            "裸 API 不是变量名"
        );
        assert_eq!(
            extract_env_name("USE THE KEY"),
            None,
            "无下划线的单词不是变量名"
        );
        assert_eq!(
            extract_env_name("Set XXX_API_KEY to use"),
            None,
            "占位符不算"
        );
        assert_eq!(extract_env_name("配置完成后即可使用"), None, "无大写串");
        assert_eq!(extract_env_name(""), None);
    }

    #[test]
    fn input_catalog_marks_manual_env_when_reason_unparsable() {
        // 上游改了文案（没有变量名）→ 必须置 manual_env，而不是静默当成"无需配置"
        let body = r#"{"providers":[
            {"id":"dashscope","name":"通义千问","available":false,"requires_api_key":true,
             "unavailable_reason":"configuration required","privacy":"remote"}
        ]}"#;
        let v = parse_catalog("dounai-input", body, &[]).unwrap();
        assert!(!v[0].ready);
        assert!(v[0].missing.is_empty(), "解析不出变量名时 missing 为空");
        assert!(v[0].manual_env, "必须标记需要手动填写变量名");
    }

    #[test]
    fn input_catalog_no_manual_env_when_not_requiring_key() {
        // 不需要密钥的厂商（如本地 whisper 未装模型）不该提示手填变量名
        let body = r#"{"providers":[
            {"id":"local-whisper","name":"本地 Whisper","available":false,
             "requires_api_key":false,"unavailable_reason":"model not installed","privacy":"local"}
        ]}"#;
        let v = parse_catalog("dounai-input", body, &[]).unwrap();
        assert!(!v[0].manual_env, "不需要密钥时不应要求手动填变量名");
    }

    #[test]
    fn omninexus_catalog_marks_manual_env_when_env_name_missing() {
        let body = r#"{"providers":[
            {"name":"weird","label":"Weird","requires_key":true,"configured":false,"api_key_env":"","local":false}
        ]}"#;
        let v = parse_catalog("omninexus", body, &[]).unwrap();
        assert!(!v[0].ready);
        assert!(v[0].missing.is_empty());
        assert!(v[0].manual_env, "有依赖密钥却没给变量名 → 需要手填");
    }

    #[test]
    fn reads_pyproject_and_cargo_versions() {
        let dir = std::env::temp_dir().join(format!(
            "dounai-ver-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();

        // pyproject
        std::fs::write(
            dir.join("pyproject.toml"),
            "[build-system]\nrequires = []\n\n[project]\nname = \"x\"\nversion = \"1.2.3\"\n",
        )
        .unwrap();
        assert_eq!(
            read_upstream_version(&dir).as_deref(),
            Some("1.2.3"),
            "应读到 [project] 段里的版本"
        );

        // manifest 与上游一致
        let ok = check_version("1.2.3", &dir);
        assert!(ok.consistent);
        assert_eq!(ok.upstream.as_deref(), Some("1.2.3"));

        // manifest 与上游不一致 → 标记为不一致（但不阻塞）
        let bad = check_version("0.1.0", &dir);
        assert!(!bad.consistent);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn version_check_is_neutral_when_upstream_unknown() {
        // 没有 pyproject/Cargo.toml（例如悬浮窗仓库）→ 不误报不一致
        let dir = std::env::temp_dir().join(format!(
            "dounai-ver-none-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let v = check_version("0.1.0", &dir);
        assert_eq!(v.upstream, None);
        assert!(v.consistent, "读不到上游版本时不应判定为不一致");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn local_http_refuses_non_loopback() {
        // 防止这个函数被当成任意出网原语
        let err = http_get_local("http://example.com/x", Duration::from_millis(200)).unwrap_err();
        assert!(err.contains("只允许访问本机"), "实际：{err}");
    }
}
