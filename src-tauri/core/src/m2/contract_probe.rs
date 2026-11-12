//! 契约自检：把运行时探测到的契约文档按 roles-v1 的 expect.shape 判定。
//!
//! 不匹配的实例标为「不兼容」并 `reject_activation`。shell 与 auxiliary 的
//! `required_contract` 为 null，不做探测。

use serde_json::{Map, Value};

use crate::manifest::Role;

pub const EMOTION_LABELS: &[&str] = &[
    "neutral",
    "joy",
    "excitement",
    "comfort",
    "tenderness",
    "calm",
    "sadness",
    "apology",
    "encouragement",
    "surprise",
    "frustration",
    "anger",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compatibility {
    Compatible,
    Incompatible(String),
}

impl Compatibility {
    pub fn is_compatible(&self) -> bool {
        matches!(self, Self::Compatible)
    }
}

fn require_keys(object: &Map<String, Value>, keys: &[&str]) -> Result<(), String> {
    for key in keys {
        if !object.contains_key(*key) {
            return Err(format!("契约文档缺少必填字段 {key}"));
        }
    }
    Ok(())
}

fn collect_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(items) => {
            for item in items {
                collect_strings(item, out);
            }
        }
        Value::Object(map) => {
            for (key, value) in map {
                out.push(key.clone());
                collect_strings(value, out);
            }
        }
        _ => {}
    }
}

fn labels_present(document: &Value) -> bool {
    let mut strings = Vec::new();
    collect_strings(document, &mut strings);
    EMOTION_LABELS
        .iter()
        .all(|label| strings.iter().any(|s| s == label))
}

fn emotion_v2(contracts: Option<&Value>) -> bool {
    let Some(value) = contracts else {
        return false;
    };
    let mut strings = Vec::new();
    collect_strings(value, &mut strings);
    strings.iter().any(|s| s == "2") && strings.iter().any(|s| s == "emotion" || s == "emotion-v2")
}

fn version_numbers(s: &str) -> Option<Vec<u64>> {
    s.split('.').map(|part| part.parse::<u64>().ok()).collect()
}

fn version_at_least(version: &str, minimum: &str) -> bool {
    match (version_numbers(version), version_numbers(minimum)) {
        (Some(a), Some(b)) => a >= b,
        _ => false,
    }
}

pub fn evaluate(role: Role, document: &Value) -> Compatibility {
    let incompatible = |reason: String| Compatibility::Incompatible(reason);
    // required_contract 为 null 的角色不做探测。
    if matches!(role, Role::Shell | Role::Auxiliary) {
        return Compatibility::Compatible;
    }
    let Value::Object(object) = document else {
        return incompatible("契约文档不是 JSON 对象".into());
    };
    match role {
        Role::Input => {
            if let Err(reason) = require_keys(object, &["x-contract-version"]) {
                return incompatible(reason);
            }
            let version = object
                .get("x-contract-version")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !version_at_least(version, "1.1") {
                return incompatible(format!("x-contract-version 低于 1.1：{version}"));
            }
        }
        Role::Llm => {
            if let Err(reason) = require_keys(object, &["service", "service_version", "contracts"])
            {
                return incompatible(reason);
            }
            if !emotion_v2(object.get("contracts")) {
                return incompatible("contracts 未声明 emotion 版本 2".into());
            }
            if !labels_present(document) {
                return incompatible("缺少 12 类情绪标签".into());
            }
        }
        Role::Output => {
            if let Err(reason) = require_keys(
                object,
                &[
                    "contract_version",
                    "contract_hash",
                    "tags",
                    "output",
                    "errors",
                ],
            ) {
                return incompatible(reason);
            }
            if !labels_present(document) {
                return incompatible("tags 未覆盖 12 类情绪标签".into());
            }
        }
        Role::Shell | Role::Auxiliary => {}
    }
    Compatibility::Compatible
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn labels() -> Value {
        json!(EMOTION_LABELS)
    }

    #[test]
    fn three_data_plane_shapes() {
        let input = json!({ "x-contract-version": "1.1" });
        assert_eq!(evaluate(Role::Input, &input), Compatibility::Compatible);
        let llm = json!({
            "service": "omninexus", "service_version": "0.3.3",
            "contracts": { "emotion": { "version": "2", "tags": labels() } },
            "tags": labels()
        });
        assert_eq!(evaluate(Role::Llm, &llm), Compatibility::Compatible);
        let output = json!({
            "contract_version": "1.0", "contract_hash": "abc",
            "tags": labels(), "output": {}, "errors": []
        });
        assert_eq!(evaluate(Role::Output, &output), Compatibility::Compatible);
    }

    #[test]
    fn mismatch_is_incompatible() {
        assert!(!evaluate(Role::Input, &json!({})).is_compatible());
        assert!(!evaluate(Role::Input, &json!({ "x-contract-version": "1.0" })).is_compatible());
        assert!(!evaluate(Role::Output, &json!({ "tags": ["joy"] })).is_compatible());
        assert!(!evaluate(
            Role::Llm,
            &json!({ "service": "x", "service_version": "1", "contracts": {} })
        )
        .is_compatible());
    }

    #[test]
    fn shell_and_auxiliary_are_not_probed() {
        assert_eq!(
            evaluate(Role::Shell, &Value::Null),
            Compatibility::Compatible
        );
        assert_eq!(
            evaluate(Role::Auxiliary, &json!({})),
            Compatibility::Compatible
        );
        assert_eq!(evaluate(Role::Shell, &json!({})), Compatibility::Compatible);
    }
}
