//! 每实例诊断日志：写入、按大小轮转、保留固定份数、读取尾部。
//!
//! 默认只记录启动器自己看到的事件（拉起、退出、健康与契约探测结果）。子进程的
//! stdout/stderr 可能含用户对话内容，只有显式设置 `DOUNAI_LOG_CHILD_OUTPUT=1`
//! 才会捕获，M4 的诊断开关接管这个环境变量。
//!
//! 轮转规则本身是纯逻辑，住在 `launcher_core::lifecycle::logs`，这里只负责文件
//! 句柄、目录清理与尾部读取。

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use launcher_core::lifecycle::{
    diagnostic_enabled, rotation_plan, LogFile, DEFAULT_KEEP_FILES, DEFAULT_MAX_BYTES,
};

/// 读取尾部时最多回看的字节数，避免把上百 MB 的日志一次读进内存。
pub const MAX_TAIL_BYTES: u64 = 256 * 1024;
/// 界面一次展示的最大行数。
pub const MAX_TAIL_LINES: usize = 300;
/// 单行上限：子进程可能一次输出整段堆栈，超长行按字节截断。
pub const MAX_LINE_BYTES: usize = 8 * 1024;
/// 捕获子进程输出的开关。默认关闭，M4 的界面开关接管。
pub const CHILD_OUTPUT_ENV: &str = "DOUNAI_LOG_CHILD_OUTPUT";

pub type SharedLog = Arc<Mutex<LogSink>>;

/// 环境变量是否显式设置了子进程输出；没设返回 `None`。
///
/// 真值源是 `state/diagnostics.json`（界面上那个开关），环境变量只作临时覆盖，
/// 所以这里要区分「没设」与「设成关」——否则空值会把用户的设置压掉。
pub fn env_child_output() -> Option<bool> {
    std::env::var(CHILD_OUTPUT_ENV)
        .ok()
        .map(|value| parse_flag(&value))
}

/// 子进程 stdout/stderr 是否落盘（不看界面设置，只看环境变量与内置默认）。
pub fn child_output_enabled() -> bool {
    env_child_output().unwrap_or_else(diagnostic_enabled)
}

pub fn parse_flag(value: &str) -> bool {
    value == "1" || value.eq_ignore_ascii_case("true")
}

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// 打开一份新的日志文件并清理超出保留份数的旧文件。
pub fn open_shared(dir: &Path) -> std::io::Result<SharedLog> {
    let sink = LogSink::open(dir)?;
    Ok(Arc::new(Mutex::new(sink)))
}

/// 写一行；取不到锁或写失败都静默忽略——诊断日志不该让主流程失败。
pub fn write(sink: &SharedLog, scope: &str, message: &str) {
    lock(sink).line(scope, message);
}

pub struct LogSink {
    dir: PathBuf,
    max_bytes: u64,
    keep: usize,
    current: Option<(PathBuf, File)>,
    written: u64,
}

impl LogSink {
    pub fn open(dir: &Path) -> std::io::Result<Self> {
        Self::open_with(dir, DEFAULT_MAX_BYTES, DEFAULT_KEEP_FILES)
    }

    pub fn open_with(dir: &Path, max_bytes: u64, keep: usize) -> std::io::Result<Self> {
        fs::create_dir_all(dir)?;
        let mut sink = Self {
            dir: dir.to_path_buf(),
            max_bytes: max_bytes.max(1),
            keep: keep.max(1),
            current: None,
            written: 0,
        };
        sink.rotate()?;
        Ok(sink)
    }

    pub fn current_path(&self) -> Option<&Path> {
        self.current.as_ref().map(|(path, _)| path.as_path())
    }

    /// 写一行 `时间 [来源] 内容`。超过大小上限先轮转再写。
    pub fn line(&mut self, scope: &str, message: &str) {
        let text = format_line(scope, message);
        if self.written + text.len() as u64 > self.max_bytes {
            // 轮转失败也要把这一行尽力写下去，丢日志比丢事件更糟。
            let _ = self.rotate();
        }
        if let Some((_, file)) = self.current.as_mut() {
            if file.write_all(text.as_bytes()).is_ok() {
                let _ = file.flush();
                self.written = self.written.saturating_add(text.len() as u64);
            }
        }
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        self.current = None;
        let path = self.next_path();
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)?;
        self.written = 0;
        self.current = Some((path, file));
        self.prune();
        Ok(())
    }

    fn next_path(&self) -> PathBuf {
        let stamp = launcher_core::time::utc_now().replace([':', '-'], "");
        let nano = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        self.dir.join(format!("run-{stamp}-{nano:09}.log"))
    }

    /// 按保留份数与总大小裁掉最旧的文件；当前文件永不删。
    fn prune(&self) {
        let mut files = list_logs(&self.dir);
        let current = match self.current.as_ref() {
            Some((path, _)) => path.clone(),
            None => return,
        };
        // 当前文件可能还不在列表里（刚创建），补齐体积信息。
        if !files.iter().any(|f| f.path == current) {
            files.push(LogFile {
                path: current.clone(),
                size: 0,
                modified_at: unix_key(&current),
            });
        }
        for path in rotation_plan(&files, self.max_bytes, self.keep) {
            if path == current {
                continue;
            }
            let _ = fs::remove_file(path);
        }
    }
}

fn format_line(scope: &str, message: &str) -> String {
    let mut body = message.replace(['\r', '\n'], " ");
    if body.len() > MAX_LINE_BYTES {
        let mut cut = MAX_LINE_BYTES;
        while !body.is_char_boundary(cut) {
            cut -= 1;
        }
        body.truncate(cut);
        body.push_str(" …[truncated]");
    }
    format!("{} [{scope}] {body}\n", launcher_core::time::utc_now())
}

/// 目录里的日志文件，按文件名（含时间戳）升序，即从旧到新。
pub fn list_logs(dir: &Path) -> Vec<LogFile> {
    let mut out: Vec<LogFile> = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("log") {
            continue;
        }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        out.push(LogFile {
            path: path.clone(),
            size,
            modified_at: unix_key(&path),
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// 用修改时间做可字典序比较的键；取不到就退回文件名字典序。
fn unix_key(path: &Path) -> String {
    let secs = fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs:020}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogTail {
    pub lines: Vec<String>,
    /// 保留窗口内的文件数。
    pub files: usize,
    /// 是否因为行数或字节上限截掉了更早的内容。
    pub truncated: bool,
}

/// 文件名是否只指向本目录内的一份日志。
///
/// 界面会把文件名回传给后端，所以这里必须挡住路径穿越与保留名：
/// 只接受单段名字、只允许字母数字与 `._-`、扩展名必须是 `.log`。
pub fn is_safe_log_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 128 || name.starts_with('.') {
        return false;
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    {
        return false;
    }
    Path::new(name).extension().and_then(|e| e.to_str()) == Some("log")
}

/// 读指定的一份日志文件的尾部；名字不合法或文件不存在都返回 `None`。
pub fn read_file_tail(dir: &Path, name: &str, max_lines: usize) -> Option<LogTail> {
    if !is_safe_log_name(name) {
        return None;
    }
    let path = dir.join(name);
    if !path.is_file() {
        return None;
    }
    let (text, cut) = read_tail_text(&path, MAX_TAIL_BYTES);
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    if cut && !lines.is_empty() {
        // 从中间切进来，第一行可能只剩半截。
        lines.remove(0);
    }
    let mut truncated = cut;
    if lines.len() > max_lines {
        let drop = lines.len() - max_lines;
        lines.drain(0..drop);
        truncated = true;
    }
    Some(LogTail {
        lines,
        files: 1,
        truncated,
    })
}

/// 从最新的文件往回读，最多 `max_lines` 行。
pub fn read_tail(dir: &Path, max_lines: usize) -> LogTail {
    let files = list_logs(dir);
    let total = files.len();
    let mut collected: Vec<String> = Vec::new();
    let mut truncated = false;
    let mut budget = MAX_TAIL_BYTES;

    for path in files.iter().rev() {
        if collected.len() >= max_lines {
            truncated = true;
            break;
        }
        if budget == 0 {
            truncated = true;
            break;
        }
        let (text, cut) = read_tail_text(&path.path, budget);
        budget = budget.saturating_sub(text.len() as u64);
        let mut file_lines: Vec<String> = text.lines().map(str::to_string).collect();
        if cut && !file_lines.is_empty() {
            // 从中间切进来，第一行可能只剩半截。
            file_lines.remove(0);
        }
        // 每轮处理更旧的文件，其行排在已收集内容之前。
        file_lines.extend(collected);
        collected = file_lines;
        if cut {
            truncated = true;
        }
    }

    if collected.len() > max_lines {
        let drop = collected.len() - max_lines;
        collected.drain(0..drop);
        truncated = true;
    }

    LogTail {
        lines: collected,
        files: total,
        truncated,
    }
}

fn read_tail_text(path: &Path, budget: u64) -> (String, bool) {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return (String::new(), false),
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(budget);
    let mut buffer = Vec::new();
    if file.seek(SeekFrom::Start(start)).is_err() {
        return (String::new(), false);
    }
    let _ = file.read_to_end(&mut buffer);
    (String::from_utf8_lossy(&buffer).into_owned(), start > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_carry_time_scope_and_stay_single_line() {
        let text = format_line("lifecycle", "启动\n第二行");
        assert!(text.ends_with('\n'));
        assert!(text.contains("[lifecycle]"));
        assert_eq!(text.matches('\n').count(), 1);
        assert!(text.contains("启动 第二行"));
    }

    #[test]
    fn oversized_line_is_truncated_on_a_char_boundary() {
        let text = format_line("stdout", &"字".repeat(MAX_LINE_BYTES));
        assert!(text.contains("[truncated]"));
        assert!(text.len() < MAX_LINE_BYTES + 128);
    }

    #[test]
    fn rotation_keeps_the_newest_files_and_drops_older() {
        let dir = tempfile::tempdir().unwrap();
        let mut sink = LogSink::open_with(dir.path(), 120, 2).unwrap();
        for index in 0..30 {
            sink.line("lifecycle", &format!("event {index} 填充若干字节"));
        }
        let files = list_logs(dir.path());
        assert!(files.len() <= 2, "保留份数没生效：{}", files.len());
        // 最新文件里一定有最后一条。
        let tail = read_tail(dir.path(), MAX_TAIL_LINES);
        assert!(tail.lines.iter().any(|l| l.contains("event 29")));
    }

    #[test]
    fn read_tail_returns_newest_lines_and_marks_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let mut sink = LogSink::open_with(dir.path(), 64 * 1024, 3).unwrap();
        for index in 0..50 {
            sink.line("stdout", &format!("line-{index}"));
        }
        let tail = read_tail(dir.path(), 10);
        assert_eq!(tail.lines.len(), 10);
        assert!(tail.truncated);
        assert!(tail.lines.last().unwrap().contains("line-49"));
        assert!(tail.lines[0].contains("line-40"));
    }

    #[test]
    fn read_tail_on_missing_dir_is_empty_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let tail = read_tail(&dir.path().join("nope"), 10);
        assert!(tail.lines.is_empty());
        assert_eq!(tail.files, 0);
        assert!(!tail.truncated);
    }

    #[test]
    fn log_names_must_stay_inside_the_directory() {
        assert!(is_safe_log_name("run-20261113T133852Z-000000001.log"));
        assert!(!is_safe_log_name("../escape.log"));
        assert!(!is_safe_log_name("..\\escape.log"));
        assert!(!is_safe_log_name("sub/run.log"));
        assert!(!is_safe_log_name("C:run.log"));
        assert!(!is_safe_log_name(".hidden.log"));
        assert!(!is_safe_log_name("run.txt"));
        assert!(!is_safe_log_name(""));
    }

    #[test]
    fn read_file_tail_reads_one_file_and_rejects_bad_names() {
        let dir = tempfile::tempdir().unwrap();
        let mut sink = LogSink::open_with(dir.path(), 64 * 1024, 3).unwrap();
        for index in 0..20 {
            sink.line("stdout", &format!("solo-{index}"));
        }
        let name = sink
            .current_path()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let tail = read_file_tail(dir.path(), &name, 5).expect("应能读到指定文件");
        assert_eq!(tail.lines.len(), 5);
        assert!(tail.truncated);
        assert!(tail.lines.last().unwrap().contains("solo-19"));
        assert_eq!(read_file_tail(dir.path(), "../escape.log", 5), None);
        assert_eq!(read_file_tail(dir.path(), "missing.log", 5), None);
    }

    #[test]
    fn only_explicit_truthy_flags_enable_child_output() {
        assert!(parse_flag("1"));
        assert!(parse_flag("TRUE"));
        assert!(!parse_flag("0"));
        assert!(!parse_flag(""));
        assert!(!parse_flag("yes"));
        // 内置常量是关闭态，M4 诊断开关接管前不应悄悄打开。
        assert!(!diagnostic_enabled());
    }
}
