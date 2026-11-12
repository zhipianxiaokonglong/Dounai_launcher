//! M3 启动编排：把 v2 实例、manifest 与环境引擎拼成 `StartRequest`。
//!
//! 本模块不碰 Tauri，也不启动进程；`Supervisor::start` 的调用留在命令面。
//! 这样端口选择、占位符展开、目录确认门禁与契约探测地址都能离线断言。

use std::collections::BTreeMap;
use std::path::PathBuf;

use launcher_core::lifecycle::RolesDoc;
use launcher_core::m2::installed::Instance;
use launcher_core::m2::local::{observe_source, require_confirmed};
use launcher_core::m2::{reserved_env_key, valid_env_key, InstanceSourceKind, M2Error};
use launcher_core::manifest::{expand_placeholders, ModuleManifestV2, Role};
use launcher_core::paths::Paths;
use launcher_core::time::utc_now;

use crate::lifecycle_runtime::StartRequest;
use crate::process::win::SpawnSpec;
use crate::state_v2::AppStateV2;

/// 包内 manifest 文件名，与 zip 契约一致。
pub const MANIFEST_FILE: &str = "manifest.json";

/// 展开后的启动输入。命令面把它交给 `Supervisor::start`。
#[derive(Debug, Clone)]
pub struct LaunchPlan {
    pub request: StartRequest,
    pub manifest: ModuleManifestV2,
}

/// 构造启动请求。
///
/// 顺序刻意为「目录确认 → 环境校验 → 占位符展开 → 命令安全拆分」：
/// 任何一步失败都不会产生进程，也不会写状态文件。
pub fn build_start_request(
    state: &AppStateV2,
    instance: &Instance,
    port: Option<u16>,
    roles: &RolesDoc,
) -> Result<LaunchPlan, String> {
    let module_dir = state.module_dir(instance);
    let manifest_path = module_dir.join(MANIFEST_FILE);
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("读取实例 manifest 失败（{}）：{e}", manifest_path.display()))?;
    let package_manifest = launcher_core::m2::manifest::PackageManifest::parse(&text)
        .map_err(|e| format!("实例 manifest 不是合法的 module-v2：{e}"))?;
    let runtime_manifest: ModuleManifestV2 =
        serde_json::from_value(package_manifest.raw.clone())
            .map_err(|e| format!("实例 manifest 缺少运行时字段：{e}"))?;
    if runtime_manifest.role != instance.role {
        return Err(format!(
            "实例 {} 的角色与 manifest 不一致（{:?} vs {:?}）",
            instance.instance_id, instance.role, runtime_manifest.role
        ));
    }

    // dir 实例：源码变更确认前禁止启动，观察结果只用于比对，不写回状态。
    if instance.source_kind == InstanceSourceKind::Dir {
        let observed = observe_source(&module_dir, &utc_now()).map_err(confirm_error)?;
        require_confirmed(instance.head_confirmed.as_deref(), &observed).map_err(confirm_error)?;
    }

    let port = port
        .or_else(|| runtime_manifest.default_port())
        .unwrap_or(8101);
    let port_text = port.to_string();
    let module_text = module_dir.to_string_lossy().into_owned();
    let data_dir = state.paths.instance_data_dir_keyed(&instance.instance_id);
    let data_text = data_dir.to_string_lossy().into_owned();
    let install_text = state.paths.install_root.to_string_lossy().into_owned();
    let app_local = state.paths.data_root.to_string_lossy().into_owned();

    // shell 跳过环境引擎；数据面必须已有生成好的 venv（启动不重跑 prepare）。
    let prepared = if instance.role.needs_runtime() {
        Some(
            launcher_core::verify_instance(&state.paths, &instance.instance_id).map_err(|e| {
                format!(
                    "实例 {} 的依赖环境未就绪（{}）。请先执行 prepare_instance_env，再启动。",
                    instance.instance_id, e
                )
            })?,
        )
    } else {
        None
    };

    let venv_python = prepared
        .as_ref()
        .map(|p| p.venv_python.to_string_lossy().into_owned())
        .unwrap_or_default();
    let instance_venv = prepared
        .as_ref()
        .and_then(|p| {
            p.venv_python
                .parent()
                .map(|d| d.to_string_lossy().into_owned())
        })
        .unwrap_or_default();

    let pairs: Vec<(&str, &str)> = vec![
        ("venv_python", venv_python.as_str()),
        ("instance_venv", instance_venv.as_str()),
        ("module_dir", module_text.as_str()),
        ("data_dir", data_text.as_str()),
        ("install_root", install_text.as_str()),
        ("app_local", app_local.as_str()),
        ("port", port_text.as_str()),
        ("repo", module_text.as_str()),
    ];

    let command = expand_placeholders(&runtime_manifest.entry.command, &pairs);
    if command.contains("{venv_python}") && instance.role.needs_runtime() {
        return Err("启动命令仍含未展开的 {venv_python}".into());
    }
    let (program, args) = split_command(&command)?;

    let mut env: BTreeMap<String, String> = prepared
        .as_ref()
        .map(|p| p.env_vars.clone())
        .unwrap_or_default();
    for (key, raw) in &runtime_manifest.entry.env {
        if !valid_env_key(key) || reserved_env_key(key) {
            return Err(format!("manifest 的非法环境变量键名：{key}"));
        }
        env.insert(key.clone(), expand_placeholders(raw, &pairs));
    }
    env.insert("DOUNAI_PORT".into(), port_text.clone());

    let working_dir = prepared
        .as_ref()
        .map(|p| p.working_dir.clone())
        .unwrap_or_else(|| module_dir.clone());
    let working_raw = runtime_manifest
        .entry
        .working_dir
        .as_deref()
        .map(|w| expand_placeholders(w, &pairs));
    let working_dir = match working_raw {
        Some(raw) if !raw.trim().is_empty() => {
            let path = PathBuf::from(&raw);
            if path.is_absolute() {
                path
            } else {
                launcher_core::security::safe_join(&working_dir, &raw)
                    .map_err(|e| format!("working_dir 越界：{e}"))?
            }
        }
        _ => working_dir,
    };

    let health_url = roles
        .health(instance.role)
        .map(|h| format!("http://127.0.0.1:{port}{}", h.path));
    let contract_url = roles
        .contract_probe(instance.role)
        .map(|p| format!("http://127.0.0.1:{port}{}", p.path));
    let timeout_ms = roles
        .health(instance.role)
        .map(|h| h.timeout_ms)
        .or_else(|| roles.contract_probe(instance.role).map(|p| p.timeout_ms))
        .unwrap_or(3000);

    Ok(LaunchPlan {
        request: StartRequest {
            instance_id: instance.instance_id.clone(),
            role: instance.role,
            spec: SpawnSpec {
                program: PathBuf::from(program),
                args,
                working_dir,
                env,
            },
            health_url,
            contract_url,
            timeout_ms,
        },
        manifest: runtime_manifest,
    })
}

fn confirm_error(error: M2Error) -> String {
    format!("目录实例启动被拒：{error}")
}

/// 只按空白切分，拒绝 shell 元字符；命令一律来自已验签 manifest。
pub fn split_command(command: &str) -> Result<(String, Vec<String>), String> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Err("manifest 没有启动命令".into());
    }
    if trimmed
        .chars()
        .any(|c| matches!(c, '|' | '&' | ';' | '>' | '<' | '`' | '$'))
    {
        return Err(format!("启动命令含 shell 元字符，拒绝执行：{trimmed}"));
    }
    let mut parts = trimmed.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| "manifest 没有启动程序".to_string())?
        .to_string();
    Ok((program, parts.map(str::to_string).collect()))
}

/// active 实例用规范端口；其余按字典序分配备用端口段。
pub fn assign_ports(
    state: &AppStateV2,
    roles: &RolesDoc,
    instances: &[Instance],
) -> BTreeMap<String, Option<u16>> {
    let mut used: Vec<u16> = Vec::new();
    let active = state.with_installed(|s| s.active.clone());
    let mut out = BTreeMap::new();
    // 先固定 active 实例的规范端口，避免被备用端口抢占。
    for instance in instances {
        let port = if active.values().any(|id| id == &instance.instance_id) {
            roles.canonical_port(instance.role)
        } else {
            None
        };
        if let Some(p) = port {
            used.push(p);
        }
        out.insert(instance.instance_id.clone(), port);
    }
    let mut next_index = 0u16;
    for instance in instances {
        if out.get(&instance.instance_id).and_then(|p| *p).is_some()
            || !instance.role.needs_runtime()
        {
            continue;
        }
        let mut candidate = state.with_installed(|s| s.standby_port(next_index));
        while used.contains(&candidate) {
            next_index += 1;
            candidate = state.with_installed(|s| s.standby_port(next_index));
        }
        used.push(candidate);
        out.insert(instance.instance_id.clone(), Some(candidate));
        next_index += 1;
    }
    out
}

/// 启动集合按 role 拓扑序，拓扑内按 instance_id 稳定排序。
pub fn ordered_instances(
    roles: &RolesDoc,
    instances: Vec<Instance>,
) -> Result<Vec<Instance>, String> {
    let mut wanted: Vec<Role> = instances.iter().map(|i| i.role).collect();
    wanted.sort_by_key(|r| format!("{:?}", r));
    wanted.dedup_by_key(|r| *r);
    let order = launcher_core::lifecycle::startup_order(roles, &wanted)
        .map_err(|e| format!("roles 拓扑排序失败：{e}"))?;
    let mut out = Vec::new();
    for role in order {
        let mut group: Vec<Instance> = instances
            .iter()
            .filter(|i| i.role == role)
            .cloned()
            .collect();
        group.sort_by(|a, b| a.instance_id.cmp(&b.instance_id));
        out.extend(group);
    }
    Ok(out)
}

/// 安装后把包内 manifest 原文写回包目录：`install_package` 只写 files，
/// 启动与 `read_module_manifests` 都需要这份原文（验签对象不能由命令面重造）。
pub fn write_manifest(module_dir: &std::path::Path, raw: &serde_json::Value) -> Result<(), String> {
    let text =
        serde_json::to_string_pretty(raw).map_err(|e| format!("序列化 manifest 失败：{e}"))?;
    if text.is_empty() {
        return Err("manifest 原文为空".into());
    }
    // 一次原子写，避免半截 JSON 被后续启动读到。
    launcher_core::state::write_bytes_once(&module_dir.join(MANIFEST_FILE), text.as_bytes())
        .map_err(|e| format!("写回 manifest 失败：{e}"))
}

/// 包目录统一放在 `instances/<key>/package`，与 venvs/data/logs 隔开。
pub fn package_dir(paths: &Paths, instance_id: &str) -> PathBuf {
    paths.instance_dir_keyed(instance_id).join("package")
}

#[cfg(test)]
mod tests {
    use super::*;
    use launcher_core::paths::InstallMode;

    fn state() -> (tempfile::TempDir, AppStateV2) {
        let dir = tempfile::tempdir().unwrap();
        let state = AppStateV2::new(dir.path().join("app"), dir.path().join("data")).unwrap();
        (dir, state)
    }

    #[test]
    fn split_command_rejects_shell_metacharacters() {
        assert_eq!(
            split_command("python -m demo").unwrap(),
            ("python".into(), vec!["-m".into(), "demo".into()])
        );
        assert!(split_command("python -m demo | cmd").is_err());
        assert!(split_command("   ").is_err());
    }

    #[test]
    fn package_dir_is_namespaced_by_instance_key() {
        let (_dir, state) = state();
        let dir = package_dir(&state.paths, "dounai/demo@stable");
        assert!(dir.to_string_lossy().contains("instances"));
        assert!(dir.ends_with("package"));
        assert!(!dir.to_string_lossy().contains("dounai/demo@stable"));
    }

    #[test]
    fn manifest_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let raw = serde_json::json!({"schema_version": 2, "module_id": "dounai/demo"});
        write_manifest(dir.path(), &raw).unwrap();
        let text = std::fs::read_to_string(dir.path().join(MANIFEST_FILE)).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            raw
        );
    }

    #[test]
    fn paths_are_constructible_from_two_roots() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(
            InstallMode::CurrentUser,
            dir.path().join("app"),
            dir.path().join("data"),
        )
        .unwrap();
        assert_ne!(paths.install_root, paths.data_root);
    }
}
