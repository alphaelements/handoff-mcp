//! Unknown-argument rejection for the `handoff_trace_*` tools.
//!
//! A misspelled argument (`dryrun`, `layer`) or scope key (`item`) used to be
//! dropped without a word, so a write ran with the wider default instead of
//! the caller's intent. `handoff_report` already refuses unknown scope fields;
//! this does the same for every trace tool, driven by the tool's own
//! `input_schema` so the accepted keys can never drift from what the tool
//! advertises.

use std::collections::HashMap;
use std::sync::OnceLock;

use anyhow::{bail, Result};
use serde_json::Value;

/// Tool-name prefix this check applies to.
const TRACE_TOOL_PREFIX: &str = "handoff_trace_";

/// `name -> input_schema` for every trace tool, built once: the definitions
/// carry multi-kilobyte descriptions and are not worth rebuilding per call.
fn trace_schemas() -> &'static HashMap<String, Value> {
    static SCHEMAS: OnceLock<HashMap<String, Value>> = OnceLock::new();
    SCHEMAS.get_or_init(|| {
        crate::mcp::tools::all_tool_definitions()
            .into_iter()
            .filter(|t| t.name.starts_with(TRACE_TOOL_PREFIX))
            .map(|t| (t.name, t.input_schema))
            .collect()
    })
}

/// Errors when `arguments` carries a key the tool's schema does not declare,
/// at the top level or inside a declared object (or array-of-object)
/// property. A non-trace tool, or a schema node that declares no
/// `properties`, is not checked.
pub fn reject_unknown_arguments(tool: &str, arguments: &Value) -> Result<()> {
    if !tool.starts_with(TRACE_TOOL_PREFIX) {
        return Ok(());
    }
    let Some(schema) = trace_schemas().get(tool) else {
        return Ok(());
    };
    let mut unknown = Vec::new();
    collect_unknown(schema, arguments, "", &mut unknown);
    if unknown.is_empty() {
        return Ok(());
    }
    bail!(
        "{tool}: unknown argument(s): {}. Accepted at that level: {}",
        unknown
            .iter()
            .map(|(path, _)| path.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        accepted_summary(&unknown)
    );
}

fn collect_unknown(
    schema: &Value,
    value: &Value,
    path: &str,
    out: &mut Vec<(String, Vec<String>)>,
) {
    let (Some(props), Some(obj)) = (
        schema.get("properties").and_then(Value::as_object),
        value.as_object(),
    ) else {
        return;
    };
    if schema.get("additionalProperties") == Some(&Value::Bool(true)) {
        return;
    }
    for (key, child) in obj {
        let child_path = if path.is_empty() {
            key.clone()
        } else {
            format!("{path}.{key}")
        };
        match props.get(key) {
            None => {
                let mut accepted: Vec<String> = props.keys().cloned().collect();
                accepted.sort();
                out.push((child_path, accepted));
            }
            Some(child_schema) => descend(child_schema, child, &child_path, out),
        }
    }
}

fn descend(schema: &Value, value: &Value, path: &str, out: &mut Vec<(String, Vec<String>)>) {
    match value {
        Value::Object(_) => collect_unknown(schema, value, path, out),
        Value::Array(items) => {
            if let Some(item_schema) = schema.get("items") {
                for (i, item) in items.iter().enumerate() {
                    descend(item_schema, item, &format!("{path}[{i}]"), out);
                }
            }
        }
        _ => {}
    }
}

fn accepted_summary(unknown: &[(String, Vec<String>)]) -> String {
    let mut seen: Vec<&Vec<String>> = Vec::new();
    for (_, accepted) in unknown {
        if !seen.contains(&accepted) {
            seen.push(accepted);
        }
    }
    seen.iter()
        .map(|a| a.join("/"))
        .collect::<Vec<_>>()
        .join(" | ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn known_arguments_pass() {
        assert!(reject_unknown_arguments(
            "handoff_trace_test_run",
            &json!({"action": "create", "scope": {"layers": ["a"], "kinds": []}, "label": "x"})
        )
        .is_ok());
    }

    #[test]
    fn unknown_top_level_argument_is_rejected_and_named() {
        let err = reject_unknown_arguments("handoff_trace_lint", &json!({"dryrun": true}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("dryrun"), "{err}");
        assert!(err.contains("handoff_trace_lint"), "{err}");
    }

    #[test]
    fn unknown_scope_key_is_rejected_with_its_path() {
        let err = reject_unknown_arguments(
            "handoff_trace_test_run",
            &json!({"action": "create", "scope": {"item": ["A-1"]}}),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("scope.item"), "{err}");
    }

    #[test]
    fn non_trace_tools_are_not_checked() {
        assert!(reject_unknown_arguments("handoff_list_tasks", &json!({"zzz": 1})).is_ok());
    }

    #[test]
    fn every_trace_tool_declares_project_dir_and_has_a_schema() {
        let schemas = trace_schemas();
        assert!(schemas.len() >= 15, "trace schemas: {}", schemas.len());
        for (name, schema) in schemas {
            assert!(
                schema["properties"]["project_dir"].is_object(),
                "{name} must declare project_dir"
            );
        }
    }

    #[test]
    fn dispatcher_refuses_an_unknown_trace_argument_before_running_the_tool() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff_dir).unwrap();
        let ctx = super::super::HandlerContext {
            agent_id: None,
            project_dir: tmp.path().to_path_buf(),
            handoff_dir,
        };
        let resp =
            super::super::handle_tool_call(&ctx, "handoff_trace_lint", &json!({"dryrun": true}));
        let result = resp.result.expect("tool result");
        assert_eq!(result["isError"], true, "{result}");
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("unknown argument"), "{text}");
        assert!(text.contains("dryrun"), "{text}");
    }
}
