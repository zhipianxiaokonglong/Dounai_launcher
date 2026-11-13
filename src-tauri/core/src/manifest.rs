//! 环境引擎只需要 manifest 的运行时、入口与端口字段。完整 schema 校验由 contracts 层负责，
//! 这里不重复字段表，也不把未知字段当成错误。

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::state::StateError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Input,
    Llm,
    Output,
    Shell,
    Auxiliary,
}

impl Role {
    /// `shell` 只启动本地 exe，没有 Python 运行时。
    pub fn needs_runtime(self) -> bool {
        self != Role::Shell
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DepsKind {
    UvLock,
    Requirements,
    Pyproject,
}

impl DepsKind {
    /// 界面与进度文案用的短名。
    pub fn as_str(self) -> &'static str {
        match self {
            DepsKind::UvLock => "uv.lock",
            DepsKind::Requirements => "requirements",
            DepsKind::Pyproject => "pyproject",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepsSpec {
    pub kind: DepsKind,
    pub path: String,
    #[serde(default)]
    pub extras: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSpec {
    pub python: String,
    pub venv: String,
    pub deps: DepsSpec,
    #[serde(default)]
    pub snapshot_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthSpec {
    pub url: String,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntrySpec {
    pub command: String,
    #[serde(default)]
    pub working_dir: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub health: Option<HealthSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleManifestV2 {
    pub schema_version: u8,
    pub module_id: String,
    pub version: String,
    pub variant: String,
    pub role: Role,
    #[serde(default)]
    pub runtime: Option<RuntimeSpec>,
    pub entry: EntrySpec,
    #[serde(default)]
    pub ports: Vec<u16>,
}

impl ModuleManifestV2 {
    pub fn from_file(path: &Path) -> Result<Self, StateError> {
        crate::state::read_json(path)
    }

    /// 默认用规范端口；M3 的备用端口段由调用方覆盖。
    pub fn default_port(&self) -> Option<u16> {
        self.ports.first().copied()
    }
}

/// 展开 `{venv_python}` 一类的占位符。键不含花括号。
///
/// 单次扫描：代入值里若还带 `{...}`，不再二次展开，避免前面的占位符被后面的值改写。
pub fn expand_placeholders(text: &str, pairs: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(end) = text[i + 1..].find('}') {
                let key = &text[i + 1..i + 1 + end];
                if let Some((_, value)) = pairs.iter().find(|(k, _)| *k == key) {
                    out.push_str(value);
                    i += end + 2;
                    continue;
                }
            }
        }
        let ch = text[i..].chars().next().expect("i 落在字符边界上");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLES: &[&str] = &[
        include_str!("../../../contracts/examples/dounai-input.v2.json"),
        include_str!("../../../contracts/examples/omninexus.v2.json"),
        include_str!("../../../contracts/examples/moodvoice.v2.json"),
        include_str!("../../../contracts/examples/dounai-ui.v2.json"),
        include_str!("../../../contracts/examples/dounai-memory-reserved.v2.json"),
    ];

    #[test]
    fn five_contract_examples_deserialize() {
        for text in EXAMPLES {
            let m: ModuleManifestV2 = serde_json::from_str(text).expect("样例应能解析");
            assert_eq!(m.schema_version, 2);
            assert!(!m.entry.command.is_empty());
        }
    }

    #[test]
    fn shell_example_has_no_runtime_and_others_do() {
        let ui: ModuleManifestV2 = serde_json::from_str(EXAMPLES[3]).unwrap();
        assert_eq!(ui.role, Role::Shell);
        assert!(ui.runtime.is_none());
        assert!(!ui.role.needs_runtime());
        let input: ModuleManifestV2 = serde_json::from_str(EXAMPLES[0]).unwrap();
        assert_eq!(
            input.runtime.as_ref().unwrap().deps.kind,
            DepsKind::Pyproject
        );
        assert_eq!(input.default_port(), Some(8101));
    }

    #[test]
    fn unknown_role_is_rejected() {
        let text = EXAMPLES[1].replace("\"role\": \"llm\"", "\"role\": \"planner\"");
        assert!(serde_json::from_str::<ModuleManifestV2>(&text).is_err());
    }

    #[test]
    fn placeholders_expand_in_command_and_env() {
        let pairs = [
            ("venv_python", "C:/app/venv/Scripts/python.exe"),
            ("port", "18101"),
        ];
        assert_eq!(
            expand_placeholders("{venv_python} -m omninexus", &pairs),
            "C:/app/venv/Scripts/python.exe -m omninexus"
        );
        assert_eq!(expand_placeholders("{port}", &pairs), "18101");
        assert_eq!(expand_placeholders("{unknown}", &pairs), "{unknown}");
        // 代入值里再带占位符时不二次展开。
        assert_eq!(
            expand_placeholders("{a}", &[("a", "{b}"), ("b", "B")]),
            "{b}"
        );
    }
}
