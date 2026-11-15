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

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_MORE_DATA, FILETIME, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicProcessIdList,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_BASIC_PROCESS_ID_LIST, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, CREATE_NO_WINDOW, PROCESS_QUERY_LIMITED_INFORMATION,
};

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
    #[error("查询 Job 进程列表失败，系统错误码 {0}")]
    QueryJob(i32),
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

    /// 当前整棵进程树的原始读数。
    ///
    /// 走 Job 的 pid 列表而不是 parent-pid 找子进程：孙子进程也在同一个 Job 里，
    /// parent-pid 只能找到一层，且父进程先退出后关系就断了。
    ///
    /// 单个 pid 打不开（已退出、权限不够）只跳过它，不让整轮采样失败——
    /// 少算一个进程比整片显示「—」有用。
    pub fn usage(&self) -> Result<UsageRaw, WinError> {
        let pids = self.job_pids()?;
        let mut raw = UsageRaw {
            pids: pids.clone(),
            ..UsageRaw::default()
        };
        for pid in pids {
            let Some((ticks, working_set)) = sample_pid(pid) else {
                continue;
            };
            raw.cpu_ticks += ticks;
            raw.working_set_bytes += working_set;
        }
        Ok(raw)
    }

    /// Job 里当前挂着哪些 pid。
    ///
    /// `JOBOBJECT_BASIC_PROCESS_ID_LIST` 是变长结构：尾部是 `ProcessIdList[1]`，
    /// 实际容量按 pid 数另算，所以缓冲区要自己撑开并在 `ERROR_MORE_DATA` 时扩容。
    fn job_pids(&self) -> Result<Vec<u32>, WinError> {
        let mut capacity = 8usize;
        for _ in 0..4 {
            let size = std::mem::size_of::<JOBOBJECT_BASIC_PROCESS_ID_LIST>()
                + capacity.saturating_sub(1) * std::mem::size_of::<usize>();
            let mut buffer = vec![0u8; size];
            let mut returned = 0u32;
            let ok = unsafe {
                QueryInformationJobObject(
                    self.handle(),
                    JobObjectBasicProcessIdList,
                    buffer.as_mut_ptr() as *mut c_void,
                    size as u32,
                    &mut returned,
                )
            };
            let list = unsafe { &*(buffer.as_ptr() as *const JOBOBJECT_BASIC_PROCESS_ID_LIST) };
            if ok != 0 {
                let count = (list.NumberOfProcessIdsInList as usize).min(capacity);
                let ids = unsafe { std::slice::from_raw_parts(list.ProcessIdList.as_ptr(), count) };
                return Ok(ids.iter().map(|id| *id as u32).collect());
            }
            let code = last_error();
            if code != ERROR_MORE_DATA as i32 {
                return Err(WinError::QueryJob(code));
            }
            // 缓冲区不够时系统仍会写上需要的数量，按它扩容。
            let needed = list.NumberOfAssignedProcesses as usize;
            capacity = capacity.max(needed).max(capacity * 2);
        }
        Err(WinError::QueryJob(ERROR_MORE_DATA as i32))
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

/// 一个实例当前的原始读数。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageRaw {
    pub pids: Vec<u32>,
    /// kernel + user，单位 100ns。单调递增，两次之差就是这段墙钟里用掉的 CPU 时间。
    pub cpu_ticks: u64,
    /// 整棵树的工作集之和。工作集含共享页，与任务管理器同一口径。
    pub working_set_bytes: u64,
}

fn filetime_to_u64(value: &FILETIME) -> u64 {
    ((value.dwHighDateTime as u64) << 32) | value.dwLowDateTime as u64
}

/// 单个 pid 的 (CPU 时间累积值, 工作集)。
fn sample_pid(pid: u32) -> Option<(u64, u64)> {
    if pid == 0 {
        return None;
    }
    // 只要查询权限：不开 PROCESS_VM_READ，拿不到目标进程的内存内容。
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut creation: FILETIME = unsafe { std::mem::zeroed() };
    let mut exited: FILETIME = unsafe { std::mem::zeroed() };
    let mut kernel: FILETIME = unsafe { std::mem::zeroed() };
    let mut user: FILETIME = unsafe { std::mem::zeroed() };
    let times_ok =
        unsafe { GetProcessTimes(handle, &mut creation, &mut exited, &mut kernel, &mut user) };
    let mut counters: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
    counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    let mem_ok = unsafe { GetProcessMemoryInfo(handle, &mut counters, counters.cb) };
    unsafe { CloseHandle(handle) };
    if times_ok == 0 && mem_ok == 0 {
        return None;
    }
    let ticks = if times_ok != 0 {
        filetime_to_u64(&kernel) + filetime_to_u64(&user)
    } else {
        0
    };
    let working_set = if mem_ok != 0 {
        counters.WorkingSetSize as u64
    } else {
        0
    };
    Some((ticks, working_set))
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
