//! 本地目录导入向导：把旧版 v1 manifest 补成完整的 module-v2 清单。
//!
//! 这里只做**导入期转换**，不是运行时兼容层。运行时（`build_start_request`、
//! `prepare_instance`、`read_instance_manifest`）从头到尾只认 module-v2；
//! 旧清单只当向导的预填素材，用户确认后生成的新清单落在数据根，
//! 源码目录只读。catalog 安装与 zip 导入都不经过这里。
//!
//! 生成物一律按 C 类登记：category 固定 optional、signature 为 null、
//! files 为空、不自动更新。这些字段不给用户填，避免「补字段」变成
//! 把本地目录伪装成官方签名包的通道。

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::lifecycle::{RolesDoc, ROLES_JSON};
use crate::m2::manifest::{valid_variant, PackageManifest};
use crate::manifest::{DepsKind, ModuleManifestV2, Role};

/// 用户要拍板的内容。其余 v2 字段都由 v1 自动推导或固定，不进这张表。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1Options {
    /// `namespace/name`，三段式里没有 namespace 的 v1 标识由用户补。
    pub module_id: String,
    pub role: Role,
    pub variant: String,
    /// 规范端口。shell 通常为空。
    pub ports: Vec<u16>,
    /// shell 为 None；数据面必须有值。
    pub deps_kind: Option<DepsKind>,
    /// 包内相对路径，例如 `pyproject.toml`。
    pub deps_path: String,
    /// 自动推导后仍允许用户改：v1 各自假设的 venv 位置不一定能被识别。
    pub command: String,
    /// role 为 auxiliary 时必填。
    pub auxiliary_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct V1Preview {
    pub path: String,
    /// v1 原文哈希。确认时重读比对，防止预览之后源码被改动。
    pub source_sha256: String,
    pub options: V1Options,
    pub manifest: Value,
    pub manifest_text: String,
    pub warnings: Vec<String>,
    /// 当前选项生成不出合法 module-v2 时的原因。有值就不许确认。
    pub problem: Option<String>,
}

/// 认 v1 只看两件事：没有 `schema_version: 2`，且带 v1 的入口结构。
/// 不按文件名猜，也不接受随便一个缺字段的 JSON。
pub fn is_v1(raw: &Value) -> bool {
    raw.get("schema_version").and_then(Value::as_u64) != Some(2)
        && raw.get("module_id").and_then(Value::as_str).is_some()
        && raw
            .pointer("/entry/command")
            .and_then(Value::as_str)
            .is_some()
}

/// 目录里的 v1 原文。清单不存在、不是 JSON、或已经是 v2 都明确报错。
pub fn read_v1(dir: &Path) -> Result<(String, Value), String> {
    let path = ["manifest.json", "dounai-manifest.json"]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.exists())
        .ok_or_else(|| {
            format!(
                "目录里没有 manifest.json 或 dounai-manifest.json：{}",
                dir.display()
            )
        })?;
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("读取 {} 失败：{e}", path.display()))?;
    let raw: Value = serde_json::from_str(&text)
        .map_err(|e| format!("{} 不是合法 JSON：{e}", path.display()))?;
    if !is_v1(&raw) {
        return Err("这个目录已经是 module-v2 清单，直接登记即可，不需要补齐。".into());
    }
    Ok((text, raw))
}

/// v1 env 里写死的端口，或 health URL 里的端口。
fn v1_port(raw: &Value) -> Option<u16> {
    if let Some(url) = raw.pointer("/entry/health/url").and_then(Value::as_str) {
        if let Some(port) = port_in_url(url) {
            return Some(port);
        }
    }
    raw.pointer("/entry/env")
        .and_then(Value::as_object)
        .and_then(|env| {
            env.values()
                .filter_map(|v| v.as_str())
                .filter_map(|s| s.parse::<u16>().ok())
                .find(|p| *p >= 1024)
        })
}

fn port_in_url(url: &str) -> Option<u16> {
    let tail = url.split("127.0.0.1:").nth(1)?;
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn replace_url_port(url: &str, port: Option<u16>) -> String {
    let Some(port) = port else {
        return url.to_string();
    };
    let Some((head, tail)) = url.split_once("127.0.0.1:") else {
        return url.to_string();
    };
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    format!("{head}127.0.0.1:{port}{}", &tail[digits.len()..])
}

/// 角色只能预填，不能当结论：它决定端口和链路位置，必须让用户确认。
fn guess_role(raw: &Value) -> Role {
    if raw
        .pointer("/entry/health/url")
        .and_then(Value::as_str)
        .is_none()
    {
        return Role::Shell;
    }
    match v1_port(raw) {
        Some(8101) => Role::Input,
        Some(8102) => Role::Llm,
        Some(8103) => Role::Output,
        _ => match raw
            .get("module_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            id if id.contains("input") => Role::Input,
            id if id.contains("nexus") || id.contains("llm") => Role::Llm,
            id if id.contains("voice") || id.contains("tts") => Role::Output,
            _ => Role::Auxiliary,
        },
    }
}

/// v1 命令把解释器写死在源码目录的 `.venv`；v2 必须由启动器准备的实例
/// venv 提供。只换程序那一段，参数原样保留，用户仍可在向导里改整行。
fn derive_command(raw: &Value, role: Role) -> String {
    let original = raw
        .pointer("/entry/command")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let normalized = original
        .replace('\\', "/")
        .replace("{repo}", "{module_dir}");
    if role == Role::Shell {
        return normalized
            .strip_prefix("./")
            .map(|s| format!("{{module_dir}}/{s}"))
            .unwrap_or(normalized);
    }
    let (program, rest) = match normalized.split_once(char::is_whitespace) {
        Some((program, rest)) => (program, rest.trim_start()),
        None => (normalized.as_str(), ""),
    };
    if program.contains("python") {
        format!("{{venv_python}} {rest}").trim_end().to_string()
    } else {
        normalized
    }
}

fn default_deps(dir: &Path) -> (Option<DepsKind>, String) {
    for (kind, file) in [
        (DepsKind::UvLock, "uv.lock"),
        (DepsKind::Pyproject, "pyproject.toml"),
        (DepsKind::Requirements, "requirements.txt"),
    ] {
        if dir.join(file).is_file() {
            return (Some(kind), file.to_string());
        }
    }
    (Some(DepsKind::Pyproject), "pyproject.toml".to_string())
}

fn python_range(raw: &Value) -> String {
    raw.get("dependencies")
        .and_then(Value::as_array)
        .and_then(|deps| {
            deps.iter().find(|d| {
                d.get("name").and_then(Value::as_str) == Some("Python")
                    && d.get("type").and_then(Value::as_str) == Some("runtime")
            })
        })
        .and_then(|d| d.get("version").and_then(Value::as_str))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(">=3.11,<3.14")
        .to_string()
}

pub fn default_options(raw: &Value, dir: &Path) -> V1Options {
    let role = guess_role(raw);
    let short = raw
        .get("module_id")
        .and_then(Value::as_str)
        .unwrap_or("module")
        .trim();
    // v1 标识可能已经带 namespace，别再套一层。
    let module_id = if short.contains('/') {
        short.to_string()
    } else {
        format!("local/{short}")
    };
    let (deps_kind, deps_path) = if role.needs_runtime() {
        default_deps(dir)
    } else {
        (None, String::new())
    };
    let ports = if role == Role::Shell {
        Vec::new()
    } else {
        v1_port(raw)
            .or_else(|| {
                RolesDoc::parse(ROLES_JSON)
                    .ok()
                    .and_then(|doc| doc.canonical_port(role))
            })
            .into_iter()
            .collect()
    };
    V1Options {
        module_id,
        role,
        variant: "stable".to_string(),
        ports,
        deps_kind,
        deps_path,
        command: derive_command(raw, role),
        auxiliary_kind: (role == Role::Auxiliary).then(|| "tooling".to_string()),
    }
}

fn string_field(raw: &Value, pointer: &str, fallback: &str) -> String {
    raw.pointer(pointer)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(fallback)
        .to_string()
}

fn admin_ui_v2(raw: &Value) -> Option<Value> {
    let src = raw.get("admin_ui")?.as_object()?;
    let mut out = Map::new();
    for key in ["kind", "title", "capabilities"] {
        if let Some(value) = src.get(key) {
            out.insert(key.to_string(), value.clone());
        }
    }
    if let Some(web) = src.get("web_ui").and_then(Value::as_object) {
        let mut kept = Map::new();
        for key in [
            "path",
            "label",
            "requires_running",
            "embed",
            "min_size",
            "placeholder",
        ] {
            if let Some(value) = web.get(key) {
                kept.insert(key.to_string(), value.clone());
            }
        }
        out.insert("web_ui".to_string(), Value::Object(kept));
    }
    Some(Value::Object(out))
}

/// 生成一份完整 module-v2。失败信息直接给向导显示，不丢 serde 原文。
pub fn build_v2(raw: &Value, options: &V1Options) -> Result<Value, String> {
    let namespace = options.module_id.split('/').next().unwrap_or("");
    if !crate::m2::manifest::valid_module_id(&options.module_id) {
        return Err(
            "模块标识要写成 namespace/name，两段都只用小写字母、数字、点、下划线和连字符。".into(),
        );
    }
    if !valid_variant(&options.variant) {
        return Err("变体只能用小写字母、数字和连字符，最多 32 个字符。".into());
    }
    if options.ports.len() > 1 {
        return Err("本地目录实例只填一个规范端口；备用端口由启动器自动分配。".into());
    }
    if options.ports.iter().any(|p| *p < 1) {
        return Err("端口必须大于 0。".into());
    }
    let shell = options.role == Role::Shell;
    if shell && !options.command.contains("{module_dir}") {
        return Err("总装角色的启动命令必须含 {module_dir}。".into());
    }
    if !shell && !options.command.contains("{venv_python}") {
        return Err("数据面角色的启动命令必须含 {venv_python}。".into());
    }
    if options.role.needs_runtime() && options.deps_kind.is_none() {
        return Err("这个角色需要 Python 运行时，请选一种依赖准备方式。".into());
    }
    if (options.role == Role::Auxiliary) != options.auxiliary_kind.is_some() {
        return Err("只有旁路角色需要 auxiliary_kind，其他角色不能填。".into());
    }

    let old_port = v1_port(raw).map(|p| p.to_string()).unwrap_or_default();
    let mut env = Map::new();
    if let Some(src) = raw.pointer("/entry/env").and_then(Value::as_object) {
        for (key, value) in src {
            let text = value.as_str().unwrap_or("");
            let converted = if !old_port.is_empty() && text == old_port {
                "{port}".to_string()
            } else {
                text.replace("{repo}", "{module_dir}")
            };
            env.insert(key.clone(), Value::String(converted));
        }
    }

    let health = match raw.pointer("/entry/health/url").and_then(Value::as_str) {
        Some(url) if !url.is_empty() => json!({
            "url": replace_url_port(url, options.ports.first().copied()),
            "timeout_ms": raw
                .pointer("/entry/health/timeout_ms")
                .and_then(Value::as_u64)
                .unwrap_or(3000),
        }),
        _ => Value::Null,
    };

    let filesystem: Vec<Value> = raw
        .pointer("/permissions/filesystem")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|v| Value::String(v.as_str().unwrap_or("").replace("{repo}", "{module_dir}")))
                .collect()
        })
        .unwrap_or_default();

    let mut manifest = json!({
        "schema_version": 2,
        "module_id": options.module_id,
        "name": string_field(raw, "/name", &options.module_id),
        "version": string_field(raw, "/version", "0.0.0"),
        "variant": options.variant,
        "role": options.role,
        "category": "optional",
        "publisher": { "id": namespace, "name": "本地目录导入", "url": "" },
        "requires": {
            "launcher": string_field(raw, "/compat/app_version", ">=0.4.0"),
            "roles_version": RolesDoc::parse(ROLES_JSON)
                .map(|d| d.roles_version)
                .unwrap_or_else(|_| "1.0.0".to_string()),
        },
        "depends_on": [],
        "entry": {
            "command": options.command,
            "working_dir": "{module_dir}",
            "env": Value::Object(env),
            "health": health,
        },
        "ports": options.ports,
        "permissions": {
            "network": raw.pointer("/permissions/network").cloned().unwrap_or(json!([])),
            "filesystem": filesystem,
            "devices": raw.pointer("/permissions/devices").cloned().unwrap_or(json!([])),
        },
        "model_packs": raw.get("model_packs").cloned().unwrap_or(json!([])),
        "files": {},
        "signature": Value::Null,
    });

    if let Some(obj) = manifest.as_object_mut() {
        if let Some(ui) = admin_ui_v2(raw) {
            obj.insert("admin_ui".to_string(), ui);
        }
        if options.role.needs_runtime() {
            obj.insert(
                "runtime".to_string(),
                json!({
                    "python": python_range(raw),
                    "venv": "{instance_venv}",
                    "deps": {
                        "kind": options.deps_kind,
                        "path": options.deps_path,
                        "extras": [],
                    },
                }),
            );
        }
        if let Some(kind) = &options.auxiliary_kind {
            obj.insert("auxiliary_kind".to_string(), Value::String(kind.clone()));
        }
    }
    Ok(manifest)
}

/// 生成物必须能过运行时两道解析：完整清单与运行时投影。
pub fn validate_v2(text: &str) -> Result<(), String> {
    let manifest = PackageManifest::parse(text).map_err(|e| e.to_string())?;
    serde_json::from_value::<ModuleManifestV2>(manifest.raw)
        .map_err(|e| format!("清单缺少运行时字段：{e}"))?;
    Ok(())
}

pub fn preview(dir: &Path, text: &str, raw: &Value, options: Option<V1Options>) -> V1Preview {
    let source_sha256 = crate::security::sha256_hex(text.as_bytes());
    let options = options.unwrap_or_else(|| default_options(raw, dir));
    let built = build_v2(raw, &options);
    let manifest = built.as_ref().ok().cloned().unwrap_or(Value::Null);
    let manifest_text = serde_json::to_string_pretty(&manifest).unwrap_or_default();
    let problem = match &built {
        Ok(_) => validate_v2(&manifest_text).err(),
        Err(error) => Some(error.clone()),
    };
    let mut warnings = vec![
        "源码目录只读：生成的 module-v2 清单写在数据根，仓库里不会出现新文件。".to_string(),
        "首次启动会在实例目录重建虚拟环境，不再使用源码目录里的 .venv。".to_string(),
        "按 C 类（未验证）登记：不进 catalog、不自动更新、不显示官方徽标。".to_string(),
        "角色与端口是按 v1 的健康地址预填的，请确认后再生成。".to_string(),
    ];
    if options.role.needs_runtime() && !dir.join(&options.deps_path).is_file() {
        warnings.push(format!(
            "目录里找不到 {}，请改依赖文件，否则首次启动装不上依赖。",
            options.deps_path
        ));
    }
    V1Preview {
        path: dir.to_string_lossy().into_owned(),
        source_sha256,
        options,
        manifest,
        manifest_text,
        warnings,
        problem,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INPUT_V1: &str = r#"{
      "module_id": "dounai-input",
      "name": "输入接入层",
      "version": "0.2.0",
      "category": "core",
      "entry": {
        "command": "{module_dir}/.venv/Scripts/python.exe -m dounai_input serve",
        "working_dir": "",
        "env": { "DOUNAI_PORT": "8101", "DOUNAI_DATA_DIR": "{data_dir}/input" },
        "health": { "url": "http://127.0.0.1:8101/healthz", "timeout_ms": 3000 }
      },
      "compat": { "app_version": ">=0.1.0" },
      "dependencies": [
        { "name": "Python", "version": ">=3.11,<3.13", "type": "runtime" }
      ],
      "permissions": {
        "network": [],
        "filesystem": ["{data_dir}/input", "{repo}"],
        "devices": ["microphone"]
      },
      "admin_ui": {
        "kind": "input",
        "title": "输入层",
        "config_source": { "kind": "env_file", "path": ".env" },
        "note": "不该带进 v2",
        "web_ui": { "path": "/", "label": "控制台", "requires_running": true }
      },
      "signature": { "algo": "SHA-256 + Ed25519", "value": "" }
    }"#;

    const SHELL_V1: &str = r#"{
      "module_id": "dounai-ui",
      "name": "总装",
      "version": "0.1.0",
      "entry": {
        "command": "{repo}\\src-tauri\\target\\release\\dounai.exe",
        "working_dir": ".",
        "env": {},
        "health": { "url": null, "timeout_ms": 3000 }
      },
      "compat": { "app_version": ">=0.1.0" },
      "dependencies": [{ "name": "WebView2 Runtime", "version": ">=100", "type": "runtime" }],
      "permissions": { "network": [], "filesystem": ["{app_local}"], "devices": [] },
      "signature": { "algo": "SHA-256 + Ed25519", "value": "" }
    }"#;

    fn parse(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn v1_input_becomes_installable_v2() {
        let raw = parse(INPUT_V1);
        assert!(is_v1(&raw));
        let options = default_options(&raw, Path::new("C:/src"));
        assert_eq!(options.role, Role::Input);
        assert_eq!(options.ports, vec![8101]);
        assert_eq!(options.module_id, "local/dounai-input");
        let v2 = build_v2(&raw, &options).unwrap();
        let text = serde_json::to_string_pretty(&v2).unwrap();
        validate_v2(&text).unwrap();

        assert_eq!(v2["category"], "optional");
        assert!(v2["signature"].is_null());
        assert_eq!(v2["files"].as_object().unwrap().len(), 0);
        assert_eq!(v2["entry"]["env"]["DOUNAI_PORT"], "{port}");
        assert_eq!(
            v2["entry"]["command"],
            "{venv_python} -m dounai_input serve"
        );
        assert_eq!(v2["runtime"]["python"], ">=3.11,<3.13");
        assert_eq!(v2["runtime"]["venv"], "{instance_venv}");
        assert_eq!(v2["permissions"]["filesystem"][1], "{module_dir}");
        // v1 的管理页私字段不进 v2 的封闭 schema。
        let ui = v2["admin_ui"].as_object().unwrap();
        assert!(!ui.contains_key("config_source"));
        assert!(!ui.contains_key("note"));
        assert_eq!(ui["web_ui"]["path"], "/");
    }

    #[test]
    fn v1_shell_has_no_runtime_and_uses_module_dir() {
        let raw = parse(SHELL_V1);
        let options = default_options(&raw, Path::new("C:/src"));
        assert_eq!(options.role, Role::Shell);
        assert!(options.ports.is_empty());
        assert!(options.deps_kind.is_none());
        let v2 = build_v2(&raw, &options).unwrap();
        validate_v2(&serde_json::to_string_pretty(&v2).unwrap()).unwrap();
        assert!(v2.get("runtime").is_none());
        assert!(v2["entry"]["health"].is_null());
        assert_eq!(
            v2["entry"]["command"],
            "{module_dir}/src-tauri/target/release/dounai.exe"
        );
    }

    #[test]
    fn v2_manifest_is_not_treated_as_v1() {
        let v2: Value = serde_json::from_str(include_str!(
            "../../../../contracts/examples/dounai-input.v2.json"
        ))
        .unwrap();
        assert!(!is_v1(&v2));
    }

    #[test]
    fn bad_options_are_rejected_before_writing_anything() {
        let raw = parse(INPUT_V1);
        let mut options = default_options(&raw, Path::new("C:/src"));
        options.module_id = "没有命名空间".into();
        assert!(build_v2(&raw, &options).is_err());
        options = default_options(&raw, Path::new("C:/src"));
        options.command = "python -m dounai_input serve".into();
        assert!(build_v2(&raw, &options).is_err());
        options = default_options(&raw, Path::new("C:/src"));
        options.deps_kind = None;
        assert!(build_v2(&raw, &options).is_err());
    }

    #[test]
    fn port_edit_rewrites_health_url() {
        let raw = parse(INPUT_V1);
        let mut options = default_options(&raw, Path::new("C:/src"));
        options.ports = vec![18101];
        let v2 = build_v2(&raw, &options).unwrap();
        assert_eq!(
            v2["entry"]["health"]["url"],
            "http://127.0.0.1:18101/healthz"
        );
    }
}
