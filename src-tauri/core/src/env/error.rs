//! 环境错误统一带 `instance_id`、失败命令与 uv 原始输出，供 M4 的环境页直接展示。
//!
//! `State` 是通用的文件与子进程 IO 失败；其余变体对应方案里的八类验收条件。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnvErrorKind {
    UvMissing,
    UvVersion,
    PythonInstall,
    DepsResolve,
    VenvCreate,
    VenvSync,
    SnapshotMismatch,
    RootMismatch,
    OfflineCacheMiss,
    NoRuntime,
    State,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureContext {
    pub instance_id: String,
    pub command: Option<String>,
    pub output: String,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[error("{kind:?}: {message}")]
pub struct EnvError {
    pub kind: EnvErrorKind,
    pub message: String,
    pub context: FailureContext,
}

impl EnvError {
    pub fn new(kind: EnvErrorKind, instance_id: &str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            context: FailureContext {
                instance_id: instance_id.to_string(),
                ..FailureContext::default()
            },
        }
    }

    pub fn with_command_line(mut self, line: &str) -> Self {
        self.context.command = Some(line.to_string());
        self
    }

    pub fn with_output_text(mut self, text: &str) -> Self {
        self.context.output = text.trim().to_string();
        self
    }

    /// 子进程只报底层失败，调用方按命令语义给出准确类别。
    pub fn reclassify(mut self, kind: EnvErrorKind) -> Self {
        self.kind = kind;
        self
    }

    /// 只改写还没判明原因的泛类错误，保留 `OfflineCacheMiss`、
    /// `SnapshotMismatch` 这类已经分类的结果，避免调用方把具体结论冲掉。
    pub fn reclassify_if_generic(mut self, kind: EnvErrorKind) -> Self {
        if self.kind == EnvErrorKind::State {
            self.kind = kind;
        }
        self
    }

    pub fn describe(&self) -> String {
        let mut out = format!("[{:?}] {}", self.kind, self.message);
        if let Some(cmd) = &self.context.command {
            out.push_str(&format!("\n命令：{cmd}"));
        }
        if !self.context.output.is_empty() {
            out.push_str(&format!("\n原始输出：{}", self.context.output));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_keeps_command_and_raw_output() {
        let e = EnvError::new(EnvErrorKind::DepsResolve, "mod@a", "包名写错")
            .with_command_line("uv pip compile pyproject.toml")
            .with_output_text("  error: package not found  \n");
        assert_eq!(e.kind, EnvErrorKind::DepsResolve);
        assert_eq!(e.context.instance_id, "mod@a");
        assert!(e.describe().contains("uv pip compile"));
        assert!(e.describe().contains("package not found"));
        assert_eq!(e.context.output, "error: package not found");
    }

    #[test]
    fn reclassify_preserves_context() {
        let e = EnvError::new(EnvErrorKind::State, "mod@a", "exit 2")
            .with_command_line("uv venv")
            .reclassify(EnvErrorKind::VenvCreate);
        assert_eq!(e.kind, EnvErrorKind::VenvCreate);
        assert_eq!(e.context.command.as_deref(), Some("uv venv"));
    }

    #[test]
    fn reclassify_if_generic_keeps_specific_kinds() {
        for specific in [
            EnvErrorKind::OfflineCacheMiss,
            EnvErrorKind::SnapshotMismatch,
            EnvErrorKind::UvVersion,
        ] {
            let e =
                EnvError::new(specific, "mod@a", "x").reclassify_if_generic(EnvErrorKind::VenvSync);
            assert_eq!(e.kind, specific);
        }
        let generic = EnvError::new(EnvErrorKind::State, "mod@a", "x")
            .reclassify_if_generic(EnvErrorKind::VenvSync);
        assert_eq!(generic.kind, EnvErrorKind::VenvSync);
    }
}
