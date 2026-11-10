//! M1 一次性验收夹具：不进正式进程管理器，验收后可直接删掉。
//!
//! 用法（在 src-tauri/ 下）：
//! ```text
//! cargo run -p launcher-core --bin env_smoke -- \
//!   --install-root C:/temp/launcher-app --data-root C:/temp/launcher-data \
//!   --uv C:/temp/uv-new/uv.exe \
//!   --input E:/Dounai-ai/Dounai_input \
//!   --llm E:/Dounai-ai/OmniNexus \
//!   --output E:/Dounai-ai/MoodVoice \
//!   --port-base 18101 [--offline] [--no-spawn] [--verify-only] [--tamper] [--check-registry]
//! ```
//!
//! 注册表比对默认关闭。加 `--check-registry` 才读 HKCU 与 HKLM 的 Python 键；
//! 没有检查时报告里的 `registry_unchanged` 是 null，不把未检查写成通过。
//!
//! 覆盖 ROADMAP 第 1 至 6 条：health、程序根与注册表、快照篡改、离线重跑、上游与源码目录不污染。
//! 结果写进 `<data_root>/state/env_smoke.json`。检查没跑成一律记进 errors，不把「未检查」写成通过。

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use launcher_core::env::{
    prepare_instance_with, verify_instance, CurlUvFetcher, EnvContext, EnvOptions, PreparedEnv,
    SystemUvRunner, UV_VERSION,
};
use launcher_core::manifest::ModuleManifestV2;
use launcher_core::{file_sha256_prefixed, InstallMode, InstallState, Paths};
use serde::Serialize;

const INPUT_MANIFEST: &str = include_str!("../../../../contracts/examples/dounai-input.v2.json");
const LLM_MANIFEST: &str = include_str!("../../../../contracts/examples/omninexus.v2.json");
const OUTPUT_MANIFEST: &str = include_str!("../../../../contracts/examples/moodvoice.v2.json");

#[derive(Debug, Serialize)]
struct InstanceReport {
    role: &'static str,
    instance_id: String,
    port: u16,
    prepared: bool,
    verified: bool,
    health: String,
    python_build_id: String,
    snapshot_sha256: String,
}

#[derive(Debug, Serialize)]
struct Report {
    install_mode: String,
    offline: bool,
    verify_only: bool,
    install_root_unchanged: bool,
    /// None 表示本次没有检查注册表，不把未检查写成通过。
    registry_unchanged: Option<bool>,
    git_status_unchanged: bool,
    source_dirs_clean: bool,
    instances: Vec<InstanceReport>,
    snapshot_tamper: Option<String>,
    errors: Vec<String>,
}

fn arg(args: &[String], name: &str) -> Result<String, String> {
    let mut i = 0;
    while i + 1 < args.len() {
        if args[i] == name {
            return Ok(args[i + 1].clone());
        }
        i += 1;
    }
    Err(format!("缺少参数 {name}"))
}

fn optional_arg(args: &[String], name: &str) -> Option<String> {
    let mut i = 0;
    while i + 1 < args.len() {
        if args[i] == name {
            return Some(args[i + 1].clone());
        }
        i += 1;
    }
    None
}

fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

/// 清单取内容哈希，等长度改写也要能查出来。读目录失败要报错，不能静默当成空清单。
fn tree_snapshot(
    root: &Path,
    prefix: &str,
    out: &mut BTreeMap<String, String>,
) -> Result<(), String> {
    let entries = fs::read_dir(root).map_err(|e| format!("读取 {} 失败：{e}", root.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("枚举 {} 失败：{e}", root.display()))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if path.is_dir() {
            tree_snapshot(&path, &rel, out)?;
        } else {
            let hash = file_sha256_prefixed(&path)
                .map_err(|e| format!("计算 {} 的哈希失败：{e}", path.display()))?;
            out.insert(rel, hash);
        }
    }
    Ok(())
}

/// 注册表比对只在显式 `--check-registry` 时做。没检查或任一查询失败都返回 None。
fn registry_snapshot(enabled: bool) -> Option<String> {
    if !enabled || !cfg!(windows) {
        return None;
    }
    let mut out = String::new();
    for key in ["HKCU\\Software\\Python", "HKLM\\Software\\Python"] {
        let result = Command::new("reg")
            .args(["query", key, "/s"])
            .output()
            .ok()?;
        if !result.status.success() {
            return None;
        }
        out.push_str(&String::from_utf8_lossy(&result.stdout));
        out.push_str(&String::from_utf8_lossy(&result.stderr));
    }
    Some(out)
}

/// git 没跑成必须报错。空输出当成「未变化」会让第 5、6 条验收假通过。
fn git_status(dir: &Path) -> Result<String, String> {
    let out = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(dir)
        .output()
        .map_err(|e| format!("在 {} 执行 git status 失败：{e}", dir.display()))?;
    if !out.status.success() {
        return Err(format!(
            "在 {} 执行 git status 失败：{}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 三个上游本来就有 v1 留下的 `.venv`，只比对前后是否新增，不把既有目录当污染。
fn source_dir_markers(dir: &Path) -> BTreeMap<String, bool> {
    [".venv", "uv.lock", "__pycache__", "requirements.lock"]
        .iter()
        .map(|name| (name.to_string(), dir.join(name).exists()))
        .collect()
}

struct Children(Vec<Child>);

impl Drop for Children {
    fn drop(&mut self) {
        for child in &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// 篡改验收结束后无条件还原锁文件。中途提前返回或 panic 都不留损坏状态。
struct LockRestore {
    path: PathBuf,
    original: Vec<u8>,
}

impl Drop for LockRestore {
    fn drop(&mut self) {
        if let Err(e) = fs::write(&self.path, &self.original) {
            eprintln!("警告：还原 {} 失败：{e}", self.path.display());
        }
    }
}

/// 回环 HTTP/1.0 的最小 GET，避免为夹具引入 HTTP 客户端依赖。
fn http_status(port: u16, path: &str) -> Option<u16> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    write!(stream, "GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").ok()?;
    let mut buf = [0u8; 512];
    let n = stream.read(&mut buf).ok()?;
    let first = String::from_utf8_lossy(&buf[..n]);
    let code = first.split_whitespace().nth(1)?.parse().ok()?;
    Some(code)
}

fn wait_for_health(port: u16, path: &str, timeout: Duration) -> String {
    let started = Instant::now();
    let mut last = "timeout".to_string();
    while started.elapsed() < timeout {
        if let Some(code) = http_status(port, path) {
            if code == 200 {
                return "200".into();
            }
            // 启动期的 404、502、503 属常见瞬态，继续等到超时，别一次判死。
            last = format!("HTTP {code}");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    if last == "timeout" {
        last
    } else {
        format!("{last}（直到超时）")
    }
}

fn spawn_instance(
    paths: &Paths,
    instance_id: &str,
    manifest: &ModuleManifestV2,
    env: &PreparedEnv,
    children: &mut Children,
) -> Result<(), String> {
    let rest = manifest
        .entry
        .command
        .strip_prefix("{venv_python}")
        .unwrap_or(&manifest.entry.command);
    let args: Vec<&str> = rest.split_whitespace().collect();
    let logs = paths.instance_logs_dir_keyed(instance_id);
    fs::create_dir_all(&logs).map_err(|e| e.to_string())?;
    let stdout = File::create(logs.join("smoke.stdout.log")).map_err(|e| e.to_string())?;
    let stderr = File::create(logs.join("smoke.stderr.log")).map_err(|e| e.to_string())?;
    let mut command = Command::new(&env.venv_python);
    command
        .args(&args)
        .current_dir(&env.working_dir)
        .envs(&env.env_vars)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    if manifest.role == launcher_core::Role::Output {
        command.env("EMOTION_TTS_ROOT", &env.working_dir);
    }
    let child = command
        .spawn()
        .map_err(|e| format!("启动 {} 失败：{e}", instance_id))?;
    children.0.push(child);
    Ok(())
}

fn run() -> Result<Report, String> {
    let args: Vec<String> = std::env::args().collect();
    let install_root = PathBuf::from(arg(&args, "--install-root")?);
    let data_root = PathBuf::from(arg(&args, "--data-root")?);
    let uv_path = PathBuf::from(arg(&args, "--uv")?);
    let offline = has(&args, "--offline");
    let verify_only = has(&args, "--verify-only");
    let no_spawn = has(&args, "--no-spawn");
    let tamper = has(&args, "--tamper");
    let port_base: u16 = optional_arg(&args, "--port-base")
        .unwrap_or_else(|| "18101".into())
        .parse()
        .map_err(|e| format!("--port-base 不是端口：{e}"))?;
    let mode = match optional_arg(&args, "--mode").as_deref() {
        Some("per_machine") => InstallMode::PerMachine,
        Some("portable") => InstallMode::Portable,
        _ => InstallMode::CurrentUser,
    };

    let specs: [(&str, u16, &str, &str, String); 3] = [
        (
            "input",
            port_base,
            "/healthz",
            INPUT_MANIFEST,
            arg(&args, "--input")?,
        ),
        (
            "llm",
            port_base + 1,
            "/v1/healthz",
            LLM_MANIFEST,
            arg(&args, "--llm")?,
        ),
        (
            "output",
            port_base + 2,
            "/api/health",
            OUTPUT_MANIFEST,
            arg(&args, "--output")?,
        ),
    ];
    for (_, _, _, _, dir) in &specs {
        if !Path::new(dir).is_dir() {
            return Err(format!("上游目录不存在：{dir}"));
        }
    }

    let paths = Paths::new(mode, install_root.clone(), data_root.clone())
        .map_err(|e| format!("Paths 不合法：{e}"))?;
    if !paths.install_state_path().exists() {
        if verify_only {
            return Err("state/install.json 不存在，--verify-only 需要先跑一次 prepare".into());
        }
        let bundled = paths.bundled_uv_path();
        if !bundled.exists() {
            fs::create_dir_all(bundled.parent().unwrap()).map_err(|e| e.to_string())?;
            fs::copy(&uv_path, &bundled).map_err(|e| format!("复制 uv.exe 失败：{e}"))?;
        }
        let hash = file_sha256_prefixed(&bundled).map_err(|e| e.to_string())?;
        InstallState::initialize(&paths, bundled, hash, UV_VERSION.into())
            .map_err(|e| format!("写 install.json 失败：{e}"))?;
    }
    let install = InstallState::load(&paths).map_err(|e| format!("读 install.json 失败：{e}"))?;

    let mut before_tree = BTreeMap::new();
    tree_snapshot(&install_root, "", &mut before_tree)?;
    let check_registry = has(&args, "--check-registry");
    let before_registry = registry_snapshot(check_registry);
    let before_git: Vec<String> = specs
        .iter()
        .map(|(_, _, _, _, dir)| git_status(Path::new(dir)))
        .collect::<Result<Vec<_>, _>>()?;
    let before_markers: Vec<BTreeMap<String, bool>> = specs
        .iter()
        .map(|(_, _, _, _, dir)| source_dir_markers(Path::new(dir)))
        .collect();

    let runner = SystemUvRunner::new(install_root.clone());
    let fetcher = CurlUvFetcher;
    let mut children = Children(Vec::new());
    let mut report = Report {
        install_mode: format!("{mode:?}"),
        offline,
        verify_only,
        install_root_unchanged: true,
        registry_unchanged: None,
        git_status_unchanged: true,
        source_dirs_clean: true,
        instances: Vec::new(),
        snapshot_tamper: None,
        errors: Vec::new(),
    };
    if check_registry && cfg!(windows) && before_registry.is_none() {
        report
            .errors
            .push("注册表检查未跑成：reg query 失败或键不存在".into());
    }

    for (index, (role, port, health_path, manifest_text, dir)) in specs.iter().enumerate() {
        let instance_id = match *role {
            "input" => "dounai/dounai-input@stable@main",
            "llm" => "dounai/omninexus@stable@main",
            _ => "dounai/moodvoice@stable@main",
        };
        let manifest: ModuleManifestV2 = serde_json::from_str(manifest_text)
            .map_err(|e| format!("解析 {role} manifest 失败：{e}"))?;
        let module_dir = PathBuf::from(dir);
        let mut entry = InstanceReport {
            role,
            instance_id: instance_id.into(),
            port: *port,
            prepared: false,
            verified: false,
            health: "not_checked".into(),
            python_build_id: String::new(),
            snapshot_sha256: String::new(),
        };

        if verify_only {
            match verify_instance(&paths, instance_id) {
                Ok(env) => {
                    entry.verified = true;
                    entry.python_build_id = env.python_build_id;
                    entry.snapshot_sha256 = env.snapshot_sha256;
                }
                Err(e) => report
                    .errors
                    .push(format!("{role} verify 失败：{}", e.describe())),
            }
        } else {
            let ctx = EnvContext {
                install: &install,
                options: EnvOptions {
                    offline,
                    allow_uv_download: true,
                    port: Some(*port),
                },
                runner: &runner,
                fetcher: &fetcher,
            };
            match prepare_instance_with(&paths, instance_id, &manifest, &module_dir, &ctx) {
                Ok(env) => {
                    entry.prepared = true;
                    entry.python_build_id = env.python_build_id.clone();
                    entry.snapshot_sha256 = env.snapshot_sha256.clone();
                    if !no_spawn {
                        if let Err(e) =
                            spawn_instance(&paths, instance_id, &manifest, &env, &mut children)
                        {
                            report.errors.push(e);
                        } else {
                            entry.health =
                                wait_for_health(*port, health_path, Duration::from_secs(90));
                            if entry.health != "200" {
                                report
                                    .errors
                                    .push(format!("{role} health 未达 200：{}", entry.health));
                            }
                        }
                    }
                }
                Err(e) => report
                    .errors
                    .push(format!("{role} prepare 失败：{}", e.describe())),
            }
        }

        match git_status(&module_dir) {
            Ok(after) if after == before_git[index] => {}
            Ok(_) => {
                report.git_status_unchanged = false;
                report
                    .errors
                    .push(format!("{role} 源码目录 git status 被改动"));
            }
            Err(e) => {
                report.git_status_unchanged = false;
                report.errors.push(format!("{role} {e}"));
            }
        }
        if source_dir_markers(&module_dir) != before_markers[index] {
            report.source_dirs_clean = false;
            report.errors.push(format!(
                "{role} 源码目录新增了 .venv、uv.lock 或 __pycache__"
            ));
        }
        report.instances.push(entry);
    }

    if tamper {
        let id = "dounai/omninexus@stable@main";
        let lock = paths.deps_lock_path(id);
        let original = fs::read(&lock).map_err(|e| format!("读锁文件失败：{e}"))?;
        let mut bytes = original.clone();
        if let Some(first) = bytes.first_mut() {
            *first ^= 0x01;
        }
        let guard = LockRestore {
            path: lock.clone(),
            original: original.clone(),
        };
        fs::write(&lock, &bytes).map_err(|e| e.to_string())?;
        let outcome = match verify_instance(&paths, id) {
            Ok(_) => "未被拒绝".to_string(),
            Err(e) => format!("{:?}", e.kind),
        };
        drop(guard);
        if !fs::read(&lock).map(|b| b == original).unwrap_or(false) {
            report
                .errors
                .push(format!("还原锁文件失败：{}", lock.display()));
        }
        report.snapshot_tamper = Some(outcome.clone());
        if outcome != "SnapshotMismatch" {
            report
                .errors
                .push(format!("篡改锁文件后未报 SnapshotMismatch：{outcome}"));
        }
    }

    let mut after_tree = BTreeMap::new();
    match tree_snapshot(&install_root, "", &mut after_tree) {
        Ok(()) => {
            report.install_root_unchanged = before_tree == after_tree;
            if !report.install_root_unchanged {
                report.errors.push("程序根文件清单或内容发生变化".into());
            }
        }
        Err(e) => {
            report.install_root_unchanged = false;
            report.errors.push(format!("程序根清单复核失败：{e}"));
        }
    }
    report.registry_unchanged = match (before_registry, registry_snapshot(check_registry)) {
        (Some(before), Some(after)) => Some(before == after),
        _ => None,
    };
    if report.registry_unchanged == Some(false) {
        report.errors.push("Python 注册表键发生变化".into());
    }

    fs::create_dir_all(paths.state_dir()).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    fs::write(paths.state_dir().join("env_smoke.json"), json).map_err(|e| e.to_string())?;
    Ok(report)
}

fn main() {
    match run() {
        Ok(report) => {
            println!("{}", serde_json::to_string_pretty(&report).unwrap());
            if !report.errors.is_empty() {
                std::process::exit(1);
            }
        }
        Err(message) => {
            eprintln!("env_smoke 失败：{message}");
            std::process::exit(1);
        }
    }
}
