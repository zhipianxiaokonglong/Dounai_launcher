//! 诊断日志的轮转策略。M3 默认不落盘，这里只算规则，不碰文件句柄与 Win32 ACL。

use std::path::PathBuf;

pub const LOG_DIR_NAME: &str = "logs";
pub const DEFAULT_MAX_BYTES: u64 = 2 * 1024 * 1024;
pub const DEFAULT_KEEP_FILES: usize = 3;
pub const DEFAULT_DIAGNOSTIC_MODE: bool = false;
pub const LOG_DIR_ACL_RULE: &str = "仅当前用户与启动器进程可写；M4 开启诊断模式时设置";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogFile {
    pub path: PathBuf,
    pub size: u64,
    /// UTC 时间字符串，字典序即时间序。
    pub modified_at: String,
}

pub fn diagnostic_enabled() -> bool {
    DEFAULT_DIAGNOSTIC_MODE
}

/// 返回应从旧到新删除的文件。先按保留份数裁，再按总大小上限裁；结果不重复。
pub fn rotation_plan(files: &[LogFile], max_bytes: u64, keep: usize) -> Vec<PathBuf> {
    let mut sorted = files.to_vec();
    sorted.sort_by(|a, b| {
        a.modified_at
            .cmp(&b.modified_at)
            .then_with(|| a.path.cmp(&b.path))
    });

    let mut plan: Vec<PathBuf> = Vec::new();
    let total = sorted.len();
    for (index, file) in sorted.iter().enumerate() {
        let within_keep = keep > 0 && index + keep >= total;
        if !within_keep {
            plan.push(file.path.clone());
        }
    }

    let mut size: u64 = sorted.iter().map(|f| f.size).sum();
    for file in &sorted {
        if size <= max_bytes {
            break;
        }
        if !plan.contains(&file.path) {
            plan.push(file.path.clone());
        }
        size = size.saturating_sub(file.size);
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, size: u64, at: &str) -> LogFile {
        LogFile {
            path: PathBuf::from(name),
            size,
            modified_at: at.to_string(),
        }
    }

    #[test]
    fn keeps_only_the_newest_files() {
        let files = vec![
            file("a.log", 10, "2026-01-01T00:00:00Z"),
            file("b.log", 10, "2026-01-02T00:00:00Z"),
            file("c.log", 10, "2026-01-03T00:00:00Z"),
        ];
        assert_eq!(rotation_plan(&files, 1024, 2), vec![PathBuf::from("a.log")]);
        assert_eq!(rotation_plan(&files, 1024, 0).len(), 3);
    }

    #[test]
    fn size_cap_removes_more_than_keep() {
        let files = vec![
            file("a.log", 80, "2026-01-01T00:00:00Z"),
            file("b.log", 80, "2026-01-02T00:00:00Z"),
        ];
        let plan = rotation_plan(&files, 100, 2);
        assert_eq!(plan, vec![PathBuf::from("a.log")]);
    }
}
