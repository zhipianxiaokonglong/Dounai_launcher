//! 模块运行管理：拉起、停止、健康检查、退出监听。
//!
//! 语义上与「启用」严格区分：
//! - `enabled`（持久化）决定模块**能不能**跑；
//! - `run`（内存态）决定模块**在不在**跑。
//!
//! 安全立场（与安装链路一致，不放松）：
//! 1. **只执行已安装 manifest 声明的入口**，不接受前端传入的任意命令或路径；
//! 2. `working_dir` 经路径校验后必须落在模块安装根内；
//! 3. **不做 shell 拼接**：命令按「程序 + 参数」拆分，拒绝 shell 元字符；
//! 4. **日志不记录子进程 stdout**（上游可能把请求体打进 stdout，落盘即泄露对话）。
//!
//! 运行状态不持久化：启动器进程重启后一律 `Stopped`，
//! 天然避免「死进程显示为运行中」。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::state::RunState;

/// 健康检查默认参数。
const DEFAULT_TIMEOUT_MS: u64 = 3000;
const DEFAULT_MAX_PROBE: u32 = 5;
/// 优雅停止等待上限。
const STOP_GRACE: Duration = Duration::from_secs(5);

/// 启动失败原因（逐条可读，便于 UI 直接展示）。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum StartError {
    #[error("模块 {0} 未安装，无法启动")]
    NotInstalled(String),
    #[error("模块 {0} 已停用，请先启用再启动")]
    Disabled(String),
    #[error("模块 {0} 的入口类型是 {1}，不支持作为进程启动")]
    NotExecutable(String, String),
    #[error("模块 {0} 缺少入口命令")]
    NoCommand(String),
    #[error("入口工作目录越界，已拒绝启动：{0}")]
    UnsafeWorkingDir(String),
    #[error("入口命令包含不允许的 shell 元字符：{0}")]
    ShellMeta(String),
    #[error("拉起进程失败：{0}")]
    Spawn(String),
    #[error("健康检查超时（{0} 次探测均未通过）")]
    HealthTimeout(u32),
    #[error("进程提前退出（退出码 {0:?}）")]
    ExitedEarly(Option<i32>),
}

/// 一次运行状态的快照，前端可直接渲染。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RunView {
    pub module_id: String,
    pub run: String,
    pub pid: Option<u32>,
    pub last_error: Option<String>,
    pub healthy: bool,
}

impl RunView {
    pub fn stopped(id: &str) -> Self {
        Self {
            module_id: id.to_string(),
            run: "stopped".into(),
            pid: None,
            last_error: None,
            healthy: false,
        }
    }
}

/// 启动/停止所需的 manifest 入口信息（由调用方从已安装 manifest 提取并校验）。
#[derive(Debug, Clone)]
pub struct EntrySpec {
    pub module_id: String,
    /// 入口类型，例如 `command` / `plugin`
    pub entry_type: String,
    /// 原始命令串，如 `python -m omninexus`
    pub command: String,
    /// 工作目录（已替换占位符），可为空表示模块根
    pub working_dir: String,
    /// 环境变量（已替换占位符）
    pub env: Vec<(String, String)>,
    /// 健康检查地址；为空表示没有健康端点（如悬浮窗）
    pub health_url: Option<String>,
    pub health_timeout_ms: u64,
}

/// 一条被托管的子进程。
///
/// `child` 为 `None` 表示进程已退出、句柄已回收，仅保留状态与失败原因供 UI 展示。
struct ManagedProcess {
    child: Option<Child>,
    module_id: String,
    pid: u32,
    /// 启动序号：用于 `stop_all` 按启动顺序逆序停止（HashMap 键序不可依赖）
    start_seq: u64,
    run: RunState,
    last_error: Option<String>,
    healthy: bool,
}

/// 进程表：管理所有由启动器拉起的子进程。
///
/// 用 `Mutex` 而非 async：操作都是短临界区，且命令本身是同步调用。
pub struct ProcessManager {
    procs: Mutex<HashMap<String, ManagedProcess>>,
    /// 健康检查轮询间隔上限（测试可调小）
    probe_interval: Duration,
    max_probe: u32,
}

impl Default for ProcessManager {
    fn default() -> Self {
        Self::new(Duration::from_millis(300), DEFAULT_MAX_PROBE)
    }
}

impl ProcessManager {
    pub fn new(probe_interval: Duration, max_probe: u32) -> Self {
        Self {
            procs: Mutex::new(HashMap::new()),
            probe_interval,
            max_probe,
        }
    }

    /// 查询某模块的运行快照；不在进程表里即 `Stopped`。
    pub fn view(&self, module_id: &str) -> RunView {
        let mut procs = match self.procs.lock() {
            Ok(g) => g,
            Err(_) => return RunView::stopped(module_id),
        };

        let Some(p) = procs.get_mut(module_id) else {
            return RunView::stopped(module_id);
        };

        // 每次查询都顺手回收已退出的进程：这是「不显示假运行」的关键兜底
        let mut exited_code: Option<Option<i32>> = None;
        if let Some(child) = p.child.as_mut() {
            if let Ok(Some(status)) = child.try_wait() {
                exited_code = Some(status.code());
            }
        }

        if let Some(code) = exited_code {
            p.run = RunState::Failed;
            p.last_error = Some(format!("进程已退出（退出码 {code:?}）"));
            p.healthy = false;
            p.child = None;
            p.pid = 0;
        }

        if p.pid == 0 {
            return RunView {
                module_id: module_id.to_string(),
                run: run_str(p.run),
                pid: None,
                last_error: p.last_error.clone(),
                healthy: false,
            };
        }

        RunView {
            module_id: module_id.to_string(),
            run: run_str(p.run),
            pid: Some(p.pid),
            last_error: p.last_error.clone(),
            healthy: p.healthy,
        }
    }

    /// 列出所有正在托管（含失败待重试）的模块。
    pub fn list(&self) -> Vec<RunView> {
        let ids: Vec<String> = match self.procs.lock() {
            Ok(g) => g.keys().cloned().collect(),
            Err(_) => Vec::new(),
        };
        ids.iter().map(|id| self.view(id)).collect()
    }

    /// 是否该模块已有活着的进程。
    pub fn is_active(&self, module_id: &str) -> bool {
        matches!(self.view(module_id).run.as_str(), "starting" | "running")
    }

    /// 启动一个模块。
    ///
    /// 幂等：已在 `starting/running` 时直接返回当前快照，不重复拉起。
    /// 同步等待健康检查收敛（最多 `max_probe` 次），因此调用方应放在后台线程。
    pub fn start(&self, spec: &EntrySpec) -> Result<RunView, StartError> {
        // 幂等：已在跑就不再拉一个
        if self.is_active(&spec.module_id) {
            return Ok(self.view(&spec.module_id));
        }

        let (mut program, args) = split_command(&spec.command)?;
        program = normalize_program_separators(&program);
        let mut cmd = Command::new(&program);
        cmd.args(&args);
        if !spec.working_dir.is_empty() {
            cmd.current_dir(&spec.working_dir);
        }
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        // stdout/stderr 丢弃：绝不把上游输出写进日志（可能含对话内容）
        cmd.stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null());

        let child = cmd.spawn().map_err(|e| StartError::Spawn(e.to_string()))?;
        let pid = child.id();

        {
            let mut procs = self
                .procs
                .lock()
                .map_err(|_| StartError::Spawn("状态锁不可用".into()))?;
            let seq = procs.values().map(|p| p.start_seq).max().unwrap_or(0) + 1;
            procs.insert(
                spec.module_id.clone(),
                ManagedProcess {
                    child: Some(child),
                    module_id: spec.module_id.clone(),
                    pid,
                    start_seq: seq,
                    run: RunState::Starting,
                    last_error: None,
                    healthy: false,
                },
            );
        }

        // 健康检查收敛
        match spec.health_url.as_deref() {
            Some(url) if !url.is_empty() => {
                self.await_healthy(&spec.module_id, url, spec.health_timeout_ms)?
            }
            // 无健康端点（如悬浮窗）：进程存活即视为运行中
            _ => self.mark_running(&spec.module_id),
        }

        Ok(self.view(&spec.module_id))
    }

    /// 轮询健康检查直到通过或超过 max_probe。
    fn await_healthy(&self, module_id: &str, url: &str, timeout_ms: u64) -> Result<(), StartError> {
        let timeout = if timeout_ms == 0 {
            DEFAULT_TIMEOUT_MS
        } else {
            timeout_ms
        };
        let per_try = Duration::from_millis(timeout.min(1500));

        for _ in 0..self.max_probe {
            // 先看进程是否已退出：比健康检查更早发现问题
            if let Some(code) = self.exited(module_id) {
                self.set_failed(module_id, format!("进程提前退出（退出码 {code:?}）"));
                return Err(StartError::ExitedEarly(code));
            }

            if http_ok(url, per_try) {
                self.mark_running(module_id);
                return Ok(());
            }
            std::thread::sleep(self.probe_interval);
        }

        self.set_failed(module_id, format!("健康检查 {url} 未通过"));
        Err(StartError::HealthTimeout(self.max_probe))
    }

    fn mark_running(&self, module_id: &str) {
        if let Ok(mut procs) = self.procs.lock() {
            if let Some(p) = procs.get_mut(module_id) {
                p.run = RunState::Running;
                p.healthy = true;
            }
        }
    }

    fn set_failed(&self, module_id: &str, reason: String) {
        if let Ok(mut procs) = self.procs.lock() {
            if let Some(p) = procs.get_mut(module_id) {
                p.run = RunState::Failed;
                p.healthy = false;
                p.last_error = Some(reason);
            }
        }
    }

    /// 若进程已退出，返回退出码。
    fn exited(&self, module_id: &str) -> Option<Option<i32>> {
        let mut procs = self.procs.lock().ok()?;
        let p = procs.get_mut(module_id)?;
        let child = p.child.as_mut()?;
        match child.try_wait() {
            Ok(Some(status)) => Some(status.code()),
            _ => None,
        }
    }

    /// 停止一个模块：先优雅终止，超时则强杀进程树。
    pub fn stop(&self, module_id: &str) -> RunView {
        let mut procs = match self.procs.lock() {
            Ok(g) => g,
            Err(_) => return RunView::stopped(module_id),
        };

        let Some(p) = procs.get_mut(module_id) else {
            return RunView::stopped(module_id);
        };

        // 没有子进程（已退出或从未拉起）：直接清理记录
        let Some(child) = p.child.as_mut() else {
            procs.remove(module_id);
            return RunView::stopped(module_id);
        };

        // 已退出：直接清理记录
        if let Ok(Some(_)) = child.try_wait() {
            procs.remove(module_id);
            return RunView::stopped(module_id);
        }

        graceful_kill(p.pid);
        let deadline = Instant::now() + STOP_GRACE;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        force_kill_tree(p.pid);
                        let _ = child.wait();
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(_) => {
                    let _ = child.kill();
                    break;
                }
            }
        }

        // 回收句柄防僵尸
        let _ = child.wait();
        procs.remove(module_id);
        RunView::stopped(module_id)
    }

    /// 停止全部（按**启动顺序的逆序**：先悬浮窗，再上游）。
    ///
    /// 为什么不能直接用 `HashMap` 的遍历顺序：哈希表键序不稳定，
    /// 每次运行可能不同。先停上游会让悬浮窗反复重连、日志刷屏，
    /// 因此必须按确定的启动顺序反着来。
    pub fn stop_all(&self) -> Vec<RunView> {
        let order: Vec<String> = match self.procs.lock() {
            Ok(g) => {
                let mut items: Vec<(u64, String)> = g
                    .values()
                    // 不按 pid 过滤：已退出但留在表里的记录也要清掉，
                    // 否则这些记录会永久占位，list() 一直显示"启动失败"。
                    .map(|p| (p.start_seq, p.module_id.clone()))
                    .collect();
                // 启动越晚越先停
                items.sort_by_key(|(seq, _)| std::cmp::Reverse(*seq));
                items.into_iter().map(|(_, id)| id).collect()
            }
            Err(_) => Vec::new(),
        };
        order.iter().map(|id| self.stop(id)).collect()
    }
}

fn run_str(r: RunState) -> String {
    match r {
        RunState::Stopped => "stopped",
        RunState::Starting => "starting",
        RunState::Running => "running",
        RunState::Failed => "failed",
    }
    .to_string()
}

/// 把命令串拆成「程序 + 参数」，并拒绝 shell 元字符。
///
/// 不做 `sh -c` 式的整串执行：那等于把 manifest 变成任意命令执行入口。
pub fn split_command(command: &str) -> Result<(String, Vec<String>), StartError> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Err(StartError::NoCommand(String::new()));
    }
    // shell 元字符一律拒绝：不允许管道、重定向、串联、命令替换
    if trimmed
        .chars()
        .any(|c| matches!(c, '|' | '&' | ';' | '>' | '<' | '`' | '$'))
    {
        return Err(StartError::ShellMeta(trimmed.to_string()));
    }

    let parts: Vec<String> = trimmed.split_whitespace().map(|s| s.to_string()).collect();
    let mut it = parts.into_iter();
    let program = it
        .next()
        .ok_or_else(|| StartError::NoCommand(trimmed.to_string()))?;
    Ok((program, it.collect()))
}

/// 占位符替换。
pub fn resolve_placeholders(raw: &str, repo: &Path, data_dir: &Path, app_local: &Path) -> String {
    raw.replace("{repo}", &repo.to_string_lossy())
        .replace("{data_dir}", &data_dir.to_string_lossy())
        .replace("{app_local}", &app_local.to_string_lossy())
}

/// 把程序路径的分隔符归一化（Windows 上 `/` → `\`）。
///
/// Windows 的 `CreateProcessW` 对「\ / 混合分隔符」的路径解析不稳定，
/// 可能偶发 `ERROR_INVALID_NAME`（os error 123）。manifest 里常见的
/// `{repo}/src-tauri/...` 经占位符替换后会变成混合分隔符，这里统一修正。
/// 对无分隔符的裸命令（python / uv / cmd…）是无操作；其他平台原样返回。
pub fn normalize_program_separators(program: &str) -> String {
    #[cfg(windows)]
    {
        if program.contains('/') {
            return program.replace('/', "\\");
        }
    }
    program.to_string()
}

/// 校验工作目录是否落在模块根内。
///
/// 复用 `security.rs` 的判定习惯：先看是否为空（空=模块根），
/// 再看规范化后是否仍在根内。
pub fn check_working_dir(root: &Path, working: &str) -> Result<PathBuf, StartError> {
    if working.trim().is_empty() {
        return Ok(root.to_path_buf());
    }
    let candidate = PathBuf::from(working);
    let joined = if candidate.is_absolute() {
        candidate
    } else {
        root.join(candidate)
    };

    let norm_root = normalize(root);
    let norm_joined = normalize(&joined);
    if !norm_joined.starts_with(&norm_root) {
        return Err(StartError::UnsafeWorkingDir(working.to_string()));
    }
    Ok(joined)
}

/// 词法归一化（不触碰文件系统，目标目录可能尚不存在）。
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// 极简 HTTP GET：只判 2xx 且响应体非空。
///
/// 不用 reqwest 是为了让健康检查不依赖 TLS 栈，也便于在单测里指向本地死端口。
pub fn http_ok(url: &str, timeout: Duration) -> bool {
    use std::io::{Read, Write};
    let Some(rest) = url.strip_prefix("http://") else {
        // https 健康端点交给 reqwest 太重；当前上游全是 http://127.0.0.1
        return false;
    };
    let mut split = rest.splitn(2, '/');
    let host_port = split.next().unwrap_or("");
    let path = format!("/{}", split.next().unwrap_or(""));
    let addr = if host_port.contains(':') {
        host_port.to_string()
    } else {
        format!("{host_port}:80")
    };

    let Ok(mut stream) = std::net::TcpStream::connect_timeout(
        &match addr.parse() {
            Ok(a) => a,
            Err(_) => return false,
        },
        timeout,
    ) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(timeout));
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\n\r\n");
    if stream.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut buf = Vec::new();
    let _ = stream.read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    // 状态行 2xx 且响应体非空
    let ok_status = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .map(|c| (200..300).contains(&c))
        .unwrap_or(false);
    let has_body = text
        .split("\r\n\r\n")
        .nth(1)
        .map(|b| !b.trim().is_empty())
        .unwrap_or(false);
    ok_status && has_body
}

#[cfg(windows)]
fn graceful_kill(pid: u32) {
    // 不带 /F：先请求正常退出
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(windows)]
fn force_kill_tree(pid: u32) {
    // /T 终止整棵进程树，/F 强制
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(unix)]
fn graceful_kill(pid: u32) {
    let _ = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status();
}

#[cfg(unix)]
fn force_kill_tree(pid: u32) {
    let _ = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_command_rejects_shell_meta() {
        for bad in ["a | b", "a && b", "a; b", "a > f", "a `id`", "a $(id)"] {
            assert!(
                matches!(split_command(bad), Err(StartError::ShellMeta(_))),
                "应拒绝 {bad}"
            );
        }
    }

    #[test]
    fn split_command_parses_program_and_args() {
        let (p, a) = split_command("python -m omninexus --port 8102").unwrap();
        assert_eq!(p, "python");
        assert_eq!(a, vec!["-m", "omninexus", "--port", "8102"]);
    }

    #[test]
    fn split_command_rejects_empty() {
        assert!(matches!(
            split_command("   "),
            Err(StartError::NoCommand(_))
        ));
    }

    #[test]
    fn placeholders_are_replaced() {
        let got = resolve_placeholders(
            "{repo}/x/{data_dir}/y/{app_local}/z",
            Path::new("C:/r"),
            Path::new("C:/d"),
            Path::new("C:/a"),
        );
        assert!(got.contains("C:\\r") || got.contains("C:/r"));
        assert!(!got.contains("{repo}"));
        assert!(!got.contains("{data_dir}"));
        assert!(!got.contains("{app_local}"));
    }

    #[test]
    fn program_separators_are_normalized() {
        // Windows：/ 归一化为 \；其余平台原样返回
        #[cfg(windows)]
        {
            let mixed = normalize_program_separators("E:\\Dounai-ai\\Dounai/src-tauri/dounai.exe");
            assert!(!mixed.contains('/'), "不应残留正斜杠：{mixed}");
            assert!(mixed.contains("\\"), "应保留反斜杠：{mixed}");
            assert!(
                mixed.contains("src-tauri\\dounai.exe"),
                "目录分隔应统一：{mixed}"
            );
        }
        #[cfg(not(windows))]
        {
            assert_eq!(
                normalize_program_separators("E:\\Dounai-ai\\Dounai/src-tauri/dounai.exe"),
                "E:\\Dounai-ai\\Dounai/src-tauri/dounai.exe"
            );
        }
        // 裸命令 no-op
        assert_eq!(normalize_program_separators("python"), "python");
        assert_eq!(normalize_program_separators("uv"), "uv");
    }

    #[test]
    fn working_dir_must_stay_inside_root() {
        let root = Path::new("C:\\app\\modules\\m");
        assert!(check_working_dir(root, "").is_ok());
        assert!(check_working_dir(root, "sub/dir").is_ok());
        assert!(matches!(
            check_working_dir(root, "..\\..\\windows"),
            Err(StartError::UnsafeWorkingDir(_))
        ));
        assert!(matches!(
            check_working_dir(root, "C:\\Windows"),
            Err(StartError::UnsafeWorkingDir(_))
        ));
    }

    #[test]
    fn start_rejects_unreachable_health() {
        // 指向必定无服务的端口：应在 max_probe 后报 HealthTimeout
        let pm = ProcessManager::new(Duration::from_millis(20), 2);
        let spec = EntrySpec {
            module_id: "t".into(),
            entry_type: "command".into(),
            // 用必然存在的命令，确保进程能起来
            command: if cfg!(windows) {
                "cmd /c timeout /t 2".into()
            } else {
                "sleep 2".into()
            },
            working_dir: String::new(),
            env: vec![],
            health_url: Some("http://127.0.0.1:1/healthz".into()),
            health_timeout_ms: 200,
        };
        let err = pm.start(&spec).unwrap_err();
        assert!(
            matches!(
                err,
                StartError::HealthTimeout(_) | StartError::ExitedEarly(_)
            ),
            "实际 {err:?}"
        );
        let _ = pm.stop("t");
    }

    #[test]
    fn start_without_health_marks_running() {
        let pm = ProcessManager::new(Duration::from_millis(10), 1);
        let spec = EntrySpec {
            module_id: "noh".into(),
            entry_type: "command".into(),
            command: if cfg!(windows) {
                "cmd /c timeout /t 5".into()
            } else {
                "sleep 5".into()
            },
            working_dir: String::new(),
            env: vec![],
            health_url: None,
            health_timeout_ms: 100,
        };
        let v = pm.start(&spec).unwrap();
        assert_eq!(v.run, "running");
        assert!(v.pid.is_some());
        let stopped = pm.stop("noh");
        assert_eq!(stopped.run, "stopped");
    }

    #[test]
    fn start_is_idempotent() {
        let pm = ProcessManager::new(Duration::from_millis(10), 1);
        let spec = EntrySpec {
            module_id: "idem".into(),
            entry_type: "command".into(),
            command: if cfg!(windows) {
                "cmd /c timeout /t 5".into()
            } else {
                "sleep 5".into()
            },
            working_dir: String::new(),
            env: vec![],
            health_url: None,
            health_timeout_ms: 100,
        };
        let a = pm.start(&spec).unwrap();
        let b = pm.start(&spec).unwrap();
        assert_eq!(a.pid, b.pid, "重复启动不应产生第二个进程");
        let _ = pm.stop("idem");
    }

    #[test]
    fn stop_is_idempotent() {
        let pm = ProcessManager::new(Duration::from_millis(10), 1);
        assert_eq!(pm.stop("never-started").run, "stopped");
    }

    #[test]
    fn exited_process_reports_failed_not_running() {
        let pm = ProcessManager::new(Duration::from_millis(10), 1);
        let spec = EntrySpec {
            module_id: "quick".into(),
            entry_type: "command".into(),
            command: if cfg!(windows) {
                "cmd /c exit 3".into()
            } else {
                "true".into()
            },
            working_dir: String::new(),
            env: vec![],
            health_url: None,
            health_timeout_ms: 100,
        };
        let v = pm.start(&spec).unwrap();
        // 立即退出后，快照必须回落为 failed，而不是显示 running
        std::thread::sleep(Duration::from_millis(300));
        let after = pm.view("quick");
        assert_ne!(after.run, "running", "已退出进程不得显示为运行中");
        assert_eq!(v.run, "running", "启动瞬间（无健康端点）记为 running");
        let _ = pm.stop("quick");
    }

    #[test]
    fn list_and_stop_all_work() {
        let pm = ProcessManager::new(Duration::from_millis(10), 1);
        for id in ["a", "b"] {
            let spec = EntrySpec {
                module_id: id.into(),
                entry_type: "command".into(),
                command: if cfg!(windows) {
                    "cmd /c timeout /t 5".into()
                } else {
                    "sleep 5".into()
                },
                working_dir: String::new(),
                env: vec![],
                health_url: None,
                health_timeout_ms: 100,
            };
            pm.start(&spec).unwrap();
        }
        assert_eq!(pm.list().len(), 2);
        // 逆序停止：b 先于 a
        let order: Vec<String> = pm.stop_all().into_iter().map(|v| v.module_id).collect();
        assert_eq!(order, vec!["b", "a"]);
        assert!(pm.list().is_empty());
    }
}
