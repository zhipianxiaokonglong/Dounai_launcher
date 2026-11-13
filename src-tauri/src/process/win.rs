//! Windows 进程托管。桌面壳只有本文件调用 Win32 API。
//! Job Object 用于生命周期清理与强杀兜底，不是安全隔离边界。

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

#[derive(Debug, thiserror::Error)]
pub enum WinError {
    #[error("创建 Job Object 失败，系统错误码 {0}")]
    CreateJob(i32),
    #[error("设置 Job 限制失败，系统错误码 {0}")]
    SetLimit(i32),
    #[error("启动进程失败：{0}")]
    Spawn(String),
    #[error("把进程加入 Job 失败，系统错误码 {0}")]
    Assign(i32),
    #[error("终止 Job 失败，系统错误码 {0}")]
    Terminate(i32),
}

fn last_error() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn lock_child(slot: &Arc<Mutex<Option<Child>>>) -> MutexGuard<'_, Option<Child>> {
    slot.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Debug, Clone)]
pub struct SpawnSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub working_dir: PathBuf,
    pub env: BTreeMap<String, String>,
    /// 打开时接管子进程的 stdout/stderr 管道；默认丢弃，与 M3 之前一致。
    pub capture_output: bool,
}

/// job 存成 usize，让整个结构可以跨 Tauri 的命令线程传递。
pub struct WinProcess {
    job: usize,
    pub pid: u32,
    child: Arc<Mutex<Option<Child>>>,
    /// 只有在 `SpawnSpec::capture_output` 打开时才存在，由调用方取走接管。
    output: Option<(ChildStdout, ChildStderr)>,
}

impl WinProcess {
    fn handle(&self) -> HANDLE {
        self.job as HANDLE
    }

    pub fn spawn(spec: &SpawnSpec) -> Result<Self, WinError> {
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(WinError::CreateJob(last_error()));
        }

        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let set = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if set == 0 {
            let code = last_error();
            unsafe { CloseHandle(job) };
            return Err(WinError::SetLimit(code));
        }

        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .current_dir(&spec.working_dir)
            .stdin(Stdio::null())
            .stdout(if spec.capture_output {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(if spec.capture_output {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .creation_flags(CREATE_NO_WINDOW);
        for (key, value) in &spec.env {
            command.env(key, value);
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                unsafe { CloseHandle(job) };
                return Err(WinError::Spawn(error.to_string()));
            }
        };

        let assigned = unsafe { AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) };
        if assigned == 0 {
            let code = last_error();
            let _ = child.kill();
            let _ = child.wait();
            unsafe { CloseHandle(job) };
            return Err(WinError::Assign(code));
        }

        let pid = child.id();
        let output = match (child.stdout.take(), child.stderr.take()) {
            (Some(out), Some(err)) => Some((out, err)),
            // 只有一个管道拿到时把另一个放回，不让描述符在半路丢失。
            (out, err) => {
                child.stdout = out;
                child.stderr = err;
                None
            }
        };
        Ok(Self {
            job: job as usize,
            pid,
            child: Arc::new(Mutex::new(Some(child))),
            output,
        })
    }

    /// 取走输出管道，交给日志泵线程；只能取一次。
    pub fn take_output(&mut self) -> Option<(ChildStdout, ChildStderr)> {
        self.output.take()
    }

    /// watcher 独占这份 Child，用 try_wait 轮询，退出即把槽清空。
    pub fn watcher_slot(&self) -> Arc<Mutex<Option<Child>>> {
        self.child.clone()
    }

    /// 进程是否还在跑。
    ///
    /// 槽为空就说明 watcher 已经看到退出；槽还在时用 `try_wait` 兜住
    /// 「刚退出、watcher 还没轮到」这段窗口。这里不清槽：退出原因由
    /// watcher 统一记录，重复 reap 不会丢掉状态码。
    pub fn is_alive(&self) -> bool {
        let mut guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }

    /// 先请求优雅退出，超时后由 Job 一次性终止整棵进程树。
    pub fn stop_graceful(&self, timeout: Duration) -> Result<(), WinError> {
        let pid = self.pid.to_string();
        let _ = Command::new("taskkill")
            .args(["/PID", pid.as_str(), "/T"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // taskkill 同样是控制台程序，不加这个标志会在停止实例时闪一下黑窗。
            .creation_flags(CREATE_NO_WINDOW)
            .status();

        let deadline = Instant::now() + timeout;
        loop {
            if lock_child(&self.child).is_none() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.kill()?;
        Ok(())
    }

    pub fn kill(&self) -> Result<(), WinError> {
        let terminated = unsafe { TerminateJobObject(self.handle(), 1) };
        if terminated == 0 {
            return Err(WinError::Terminate(last_error()));
        }
        Ok(())
    }
}

impl Drop for WinProcess {
    fn drop(&mut self) {
        unsafe {
            TerminateJobObject(self.handle(), 1);
            CloseHandle(self.handle());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(program: &str) -> SpawnSpec {
        SpawnSpec {
            program: PathBuf::from(program),
            args: Vec::new(),
            working_dir: std::env::current_dir().unwrap(),
            env: BTreeMap::new(),
            capture_output: false,
        }
    }

    #[test]
    fn missing_program_fails_without_leaking_a_job() {
        let result = WinProcess::spawn(&spec("definitely-not-a-real-program.exe"));
        assert!(matches!(result, Err(WinError::Spawn(_))));
    }
}
