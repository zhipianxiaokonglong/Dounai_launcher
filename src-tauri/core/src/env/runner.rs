//! uv 子进程调用的唯一出口。core 不依赖 Tauri，也不把 `Command` 夹在命令面里。
//!
//! `MockUvRunner` 覆盖命令序列与失败分支，不联网、不装 Python，能在 CI 的 Linux job 跑。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use super::error::{EnvError, EnvErrorKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UvRequest {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
    pub instance_id: String,
    pub offline: bool,
}

impl UvRequest {
    pub fn new(program: &Path, instance_id: &str, args: &[&str]) -> Self {
        Self {
            program: program.to_path_buf(),
            args: args.iter().map(|s| (*s).to_string()).collect(),
            env: BTreeMap::new(),
            cwd: PathBuf::new(),
            instance_id: instance_id.to_string(),
            offline: false,
        }
    }

    pub fn command_line(&self) -> String {
        format!("{} {}", self.program.display(), self.args.join(" "))
    }

    /// 参数路径统一走 `as_os_str`，避免 Windows 上的转义差异。
    pub fn push_path(&mut self, value: &Path) {
        self.args.push(value.to_string_lossy().into_owned());
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UvOutput {
    pub stdout: String,
    pub stderr: String,
}

impl UvOutput {
    pub fn combined(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

pub trait UvRunner {
    fn run(&self, req: &UvRequest) -> Result<UvOutput, EnvError>;
}

/// 真机执行。cwd 落在程序根内时直接拒绝，防止 uv 把文件写回只读目录。
pub struct SystemUvRunner {
    pub install_root: PathBuf,
}

impl SystemUvRunner {
    pub fn new(install_root: PathBuf) -> Self {
        Self { install_root }
    }
}

impl UvRunner for SystemUvRunner {
    fn run(&self, req: &UvRequest) -> Result<UvOutput, EnvError> {
        let line = req.command_line();
        if req.cwd.as_os_str().is_empty() || !req.cwd.is_absolute() {
            return Err(EnvError::new(
                EnvErrorKind::State,
                &req.instance_id,
                format!("uv 子进程的 cwd 必须是绝对路径：{}", req.cwd.display()),
            )
            .with_command_line(&line));
        }
        // 词法比较挡不住大小写差异与 junction；两边都规范化后再判包含关系。
        // 目录还不存在时退回词法比较；真正执行时 Command 自己会报启动失败。
        let cwd = fs::canonicalize(&req.cwd).unwrap_or_else(|_| req.cwd.clone());
        let root =
            fs::canonicalize(&self.install_root).unwrap_or_else(|_| self.install_root.clone());
        if cwd.starts_with(&root) {
            return Err(EnvError::new(
                EnvErrorKind::State,
                &req.instance_id,
                format!("uv 子进程不得在程序根内执行：{}", req.cwd.display()),
            )
            .with_command_line(&line));
        }
        let mut command = Command::new(&req.program);
        command
            .args(&req.args)
            .current_dir(&cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        // 父进程可能带着 uv 或 Python 相关变量。继承会与注入值冲突：uv 0.13.7
        // 明确拒绝 UV_MANAGED_PYTHON 与 UV_PYTHON_PREFERENCE 同时出现。
        // 只清掉会干扰隔离的键，保留 PATH、SystemRoot 等 Windows 必需变量。
        for key in [
            "UV_PYTHON_PREFERENCE",
            "UV_PYTHON",
            "UV_MANAGED_PYTHON",
            "UV_PYTHON_INSTALL_DIR",
            "UV_PYTHON_NO_REGISTRY",
            "UV_CACHE_DIR",
            "UV_PROJECT_ENVIRONMENT",
            "UV_INDEX_URL",
            "UV_EXTRA_INDEX_URL",
            "UV_OFFLINE",
            "UV_NO_PROGRESS",
            "PYTHONHOME",
            "PYTHONPATH",
            "VIRTUAL_ENV",
            "CONDA_PREFIX",
            "CONDA_DEFAULT_ENV",
            "PIP_INDEX_URL",
            "PIP_EXTRA_INDEX_URL",
        ] {
            command.env_remove(key);
        }
        for (key, value) in &req.env {
            command.env(key, value);
        }
        if req.offline {
            command.env("UV_OFFLINE", "1");
        }
        let result = command.output().map_err(|e| {
            EnvError::new(
                EnvErrorKind::State,
                &req.instance_id,
                format!("无法启动 {}: {e}", req.program.display()),
            )
            .with_command_line(&line)
        })?;
        let output = UvOutput {
            stdout: String::from_utf8_lossy(&result.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&result.stderr).into_owned(),
        };
        if !result.status.success() {
            let combined = output.combined();
            // 只在明确的离线请求里做文案分类；uv 原文始终挂进 FailureContext，
            // 文案匹配不到时 M4 仍能从原始输出判断原因。
            let kind = if req.offline && looks_like_offline_cache_miss(&combined) {
                EnvErrorKind::OfflineCacheMiss
            } else {
                EnvErrorKind::State
            };
            return Err(EnvError::new(
                kind,
                &req.instance_id,
                format!("uv 退出状态 {:?}", result.status.code()),
            )
            .with_command_line(&line)
            .with_output_text(&combined));
        }
        Ok(output)
    }
}

/// 从 uv 的 stderr 认出「离线缓存缺 wheel」。文案会随版本变化，这里只作分类提示。
fn looks_like_offline_cache_miss(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("not found in the cache")
        || lower.contains("no cached")
        || lower.contains("failed to download")
        || (lower.contains("offline") && lower.contains("cache"))
}

#[derive(Debug, Clone, Default)]
pub struct MockState {
    pub calls: Vec<UvRequest>,
    pub python_installed: bool,
}

#[derive(Debug, Clone)]
pub struct MockFailure {
    pub when_contains: String,
    pub kind: EnvErrorKind,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct MockConfig {
    pub version: String,
    pub python_build_id: String,
    pub snapshot: String,
    pub fail: Option<MockFailure>,
    pub offline_cache_miss: bool,
    /// false 时 `python list` 返回空，让编排走一次 `python install`。
    pub python_installed: bool,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self {
            version: "uv 0.13.7 (mock)".into(),
            python_build_id: "cpython-3.12.8-windows-x86_64-none".into(),
            snapshot: "# pinned by uv\nuvicorn==0.34.0 --hash=sha256:aaaa\n".into(),
            fail: None,
            offline_cache_miss: false,
            python_installed: true,
        }
    }
}

/// 按命令关键字模拟 uv 的副作用，让编排逻辑能在无网络、无 Python 的环境里跑通。
#[derive(Debug, Clone)]
pub struct MockUvRunner {
    pub state: Arc<Mutex<MockState>>,
    pub config: MockConfig,
}

impl MockUvRunner {
    pub fn new(config: MockConfig) -> Self {
        Self {
            state: Arc::new(Mutex::new(MockState {
                python_installed: config.python_installed,
                ..MockState::default()
            })),
            config,
        }
    }

    pub fn calls(&self) -> Vec<UvRequest> {
        self.state
            .lock()
            .expect("mock state poisoned")
            .calls
            .clone()
    }
}

impl Default for MockUvRunner {
    fn default() -> Self {
        Self::new(MockConfig::default())
    }
}

fn arg_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn write_file(path: &Path, content: &[u8]) -> Result<(), EnvError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| EnvError::new(EnvErrorKind::State, "mock", format!("建目录失败：{e}")))?;
    }
    fs::write(path, content)
        .map_err(|e| EnvError::new(EnvErrorKind::State, "mock", format!("写文件失败：{e}")))
}

impl UvRunner for MockUvRunner {
    fn run(&self, req: &UvRequest) -> Result<UvOutput, EnvError> {
        let mut guard = self.state.lock().expect("mock state poisoned");
        guard.calls.push(req.clone());
        let python_installed = guard.python_installed;
        drop(guard);
        let line = req.command_line();

        if let Some(f) = &self.config.fail {
            if line.contains(&f.when_contains) {
                return Err(EnvError::new(f.kind, &req.instance_id, f.message.clone())
                    .with_command_line(&line));
            }
        }
        if req.offline
            && self.config.offline_cache_miss
            && (line.contains("sync") || line.contains("compile"))
        {
            return Err(EnvError::new(
                EnvErrorKind::OfflineCacheMiss,
                &req.instance_id,
                "离线缓存缺少 wheel",
            )
            .with_command_line(&line)
            .with_output_text("error: Failed to download distribution due to offline mode"));
        }

        let args = &req.args;
        if args.iter().any(|a| a == "--version") {
            return Ok(UvOutput {
                stdout: format!("{}\n", self.config.version),
                stderr: String::new(),
            });
        }
        if args.first().map(String::as_str) == Some("python") && args.iter().any(|a| a == "list") {
            if !python_installed {
                return Ok(UvOutput::default());
            }
            if let Some(dir) = req.env.get("UV_PYTHON_INSTALL_DIR") {
                let exe = Path::new(dir)
                    .join(&self.config.python_build_id)
                    .join("python.exe");
                write_file(&exe, b"mock managed python")?;
            }
            let install_dir = req
                .env
                .get("UV_PYTHON_INSTALL_DIR")
                .map(String::as_str)
                .unwrap_or("<data_root>/runtime/python");
            return Ok(UvOutput {
                stdout: format!(
                    "{}  {}/{}/python.exe\n",
                    self.config.python_build_id, install_dir, self.config.python_build_id
                ),
                stderr: String::new(),
            });
        }
        if args.first().map(String::as_str) == Some("python") && args.iter().any(|a| a == "install")
        {
            self.state
                .lock()
                .expect("mock state poisoned")
                .python_installed = true;
            if let Some(dir) = arg_after(args, "--install-dir") {
                let exe = Path::new(dir)
                    .join(&self.config.python_build_id)
                    .join("python.exe");
                write_file(&exe, b"mock python")?;
            }
            return Ok(UvOutput::default());
        }
        if args.first().map(String::as_str) == Some("venv") {
            if let Some(dir) = args.get(1) {
                write_file(
                    &Path::new(dir).join("Scripts").join("python.exe"),
                    b"mock venv python",
                )?;
                write_file(&Path::new(dir).join("pyvenv.cfg"), b"home = mock\n")?;
            }
            return Ok(UvOutput::default());
        }
        if args.iter().any(|a| a == "compile") {
            if let Some(out) = arg_after(args, "-o") {
                write_file(Path::new(out), self.config.snapshot.as_bytes())?;
            }
            return Ok(UvOutput {
                stdout: "Resolved 1 package in 1ms\n".into(),
                stderr: String::new(),
            });
        }
        Ok(UvOutput::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(args: &[&str]) -> UvRequest {
        let mut r = UvRequest::new(Path::new("uv.exe"), "mod@a", args);
        r.cwd = PathBuf::from("C:/data");
        r
    }

    #[test]
    fn mock_records_calls_in_order() {
        let runner = MockUvRunner::default();
        runner.run(&req(&["--version"])).unwrap();
        runner.run(&req(&["python", "install", "3.12"])).unwrap();
        let calls = runner.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].args, vec!["--version"]);
        assert!(calls[1].args.contains(&"install".to_string()));
    }

    #[test]
    fn mock_writes_snapshot_to_output_file() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("nested").join("deps.lock");
        let runner = MockUvRunner::default();
        let mut r = req(&["pip", "compile", "pyproject.toml", "-o"]);
        r.push_path(&out);
        runner.run(&r).unwrap();
        assert_eq!(fs::read_to_string(&out).unwrap(), runner.config.snapshot);
    }

    #[test]
    fn mock_creates_venv_python() {
        let dir = tempfile::tempdir().unwrap();
        let venv = dir.path().join("venv");
        let mut r = req(&["venv"]);
        r.push_path(&venv);
        r.args.push("--python".into());
        MockUvRunner::default().run(&r).unwrap();
        assert!(venv.join("Scripts").join("python.exe").exists());
    }

    #[test]
    fn mock_failure_and_offline_cache_miss_carry_context() {
        let runner = MockUvRunner::new(MockConfig {
            fail: Some(MockFailure {
                when_contains: "pip compile".into(),
                kind: EnvErrorKind::DepsResolve,
                message: "包名写错".into(),
            }),
            ..MockConfig::default()
        });
        let e = runner.run(&req(&["pip", "compile", "x"])).unwrap_err();
        assert_eq!(e.kind, EnvErrorKind::DepsResolve);
        assert!(e.context.command.unwrap().contains("pip compile"));

        let runner = MockUvRunner::new(MockConfig {
            offline_cache_miss: true,
            ..MockConfig::default()
        });
        let mut r = req(&["pip", "sync", "x.lock"]);
        r.offline = true;
        assert_eq!(
            runner.run(&r).unwrap_err().kind,
            EnvErrorKind::OfflineCacheMiss
        );
    }

    #[test]
    fn system_runner_rejects_program_root_as_cwd() {
        let runner = SystemUvRunner::new(PathBuf::from("C:/Program Files/App"));
        let mut r = req(&["--version"]);
        r.cwd = PathBuf::from("C:/Program Files/App/modules");
        let e = runner.run(&r).unwrap_err();
        assert_eq!(e.kind, EnvErrorKind::State);
        assert!(e.message.contains("程序根"));
    }

    #[test]
    fn system_runner_rejects_relative_cwd() {
        let runner = SystemUvRunner::new(PathBuf::from("C:/Program Files/App"));
        let mut r = req(&["--version"]);
        r.cwd = PathBuf::from("relative/dir");
        let e = runner.run(&r).unwrap_err();
        assert!(e.message.contains("绝对路径"));
    }

    #[test]
    fn offline_cache_miss_detection_uses_uv_text() {
        assert!(looks_like_offline_cache_miss(
            "error: Package `uvicorn` was not found in the cache"
        ));
        assert!(looks_like_offline_cache_miss(
            "error: Failed to download distribution due to offline mode"
        ));
        assert!(!looks_like_offline_cache_miss(
            "error: invalid pyproject.toml"
        ));
    }
}
