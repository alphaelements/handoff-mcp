//! E2E: `handoff_update_config` validates `trace.profile`, `trace.layers` and
//! `trace.done_guard` before writing them (t-vfix.19). An invalid value must be
//! rejected and must leave `config.toml` untouched; valid values keep working.

use serde_json::{json, Value};
use tempfile::TempDir;

fn send(input: &str) -> Option<Value> {
    let result = handoff_mcp::mcp::protocol::process_line(input)?;
    Some(serde_json::from_str(&result).expect("response should be valid JSON"))
}

fn call_tool(name: &str, arguments: Value) -> Value {
    let req = json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments }
    });
    send(&req.to_string()).unwrap()
}

fn text(resp: &Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"].as_bool().unwrap_or(false)
}

fn setup() -> (TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let pd = dir.path().to_string_lossy().to_string();
    let resp = call_tool(
        "handoff_init",
        json!({ "project_dir": &pd, "project_name": "test" }),
    );
    assert!(!is_error(&resp), "init failed: {}", text(&resp));
    (dir, pd)
}

fn update(pd: &str, updates: Value) -> Value {
    call_tool(
        "handoff_update_config",
        json!({ "project_dir": pd, "updates": updates }),
    )
}

fn config_text(pd: &str) -> String {
    std::fs::read_to_string(std::path::Path::new(pd).join(".handoff/config.toml")).unwrap()
}

fn assert_rejected(pd: &str, updates: Value, needle: &str) {
    let before = config_text(pd);
    let resp = update(pd, updates.clone());
    assert!(
        is_error(&resp),
        "{updates} must be rejected: {}",
        text(&resp)
    );
    assert!(
        text(&resp).contains(needle),
        "error should mention {needle:?}: {}",
        text(&resp)
    );
    assert_eq!(before, config_text(pd), "rejected update must not write");
}

#[test]
fn invalid_profile_is_rejected() {
    let (_d, pd) = setup();
    assert_rejected(&pd, json!({ "trace.profile": "bogus" }), "trace.profile");
}

#[test]
fn non_string_profile_is_rejected() {
    let (_d, pd) = setup();
    let before = config_text(&pd);
    let resp = update(&pd, json!({ "trace.profile": 5 }));
    assert!(is_error(&resp), "{}", text(&resp));
    assert_eq!(before, config_text(&pd));
}

#[test]
fn builtin_profiles_are_accepted() {
    let (_d, pd) = setup();
    for p in ["minimal", "standard", "full", "bugfix"] {
        let resp = update(&pd, json!({ "trace.profile": p }));
        assert!(!is_error(&resp), "{p}: {}", text(&resp));
        assert!(config_text(&pd).contains(&format!("profile = \"{p}\"")));
    }
}

#[test]
fn custom_profile_defined_in_config_is_accepted() {
    let (_d, pd) = setup();
    let resp = update(
        &pd,
        json!({
            "trace.profiles.mine.layers": ["requirement", "acceptance"],
            "trace.profile": "mine"
        }),
    );
    assert!(!is_error(&resp), "{}", text(&resp));
    // Referencing it later (defined in an earlier call) also works.
    let resp = update(&pd, json!({ "trace.profile": "mine" }));
    assert!(!is_error(&resp), "{}", text(&resp));
}

#[test]
fn invalid_layers_are_rejected() {
    let (_d, pd) = setup();
    assert_rejected(
        &pd,
        json!({ "trace.layers": ["requirement", "nonsense"] }),
        "nonsense",
    );
    assert_rejected(
        &pd,
        json!({ "trace.layers": "requirement" }),
        "trace.layers",
    );
    assert_rejected(&pd, json!({ "trace.layers": [1, 2] }), "trace.layers");
}

#[test]
fn valid_layers_are_accepted() {
    let (_d, pd) = setup();
    let resp = update(
        &pd,
        json!({ "trace.layers": ["requirement", "basic_spec", "acceptance", "system_test"] }),
    );
    assert!(!is_error(&resp), "{}", text(&resp));
    assert!(config_text(&pd).contains("basic_spec"));
}

#[test]
fn invalid_done_guard_is_rejected() {
    let (_d, pd) = setup();
    assert_rejected(
        &pd,
        json!({ "trace.done_guard": "error" }),
        "trace.done_guard",
    );
    assert_rejected(&pd, json!({ "trace.done_guard": true }), "trace.done_guard");
}

#[test]
fn valid_done_guard_values_are_accepted() {
    let (_d, pd) = setup();
    for v in ["off", "warn", "block"] {
        let resp = update(&pd, json!({ "trace.done_guard": v }));
        assert!(!is_error(&resp), "{v}: {}", text(&resp));
    }
    assert!(config_text(&pd).contains("done_guard = \"block\""));
}

#[test]
fn invalid_value_in_mixed_batch_blocks_whole_update() {
    let (_d, pd) = setup();
    assert_rejected(
        &pd,
        json!({ "trace.done_guard": "block", "trace.profile": "bogus" }),
        "trace.profile",
    );
}

#[test]
fn mixed_typed_and_raw_batch_with_invalid_trace_key_applies_nothing() {
    // t-vfix.21: a typed key (project.name) must not be written when a raw key
    // in the same batch (trace.profile) fails validation.
    let (_d, pd) = setup();
    assert_rejected(
        &pd,
        json!({ "project.name": "CHANGED", "trace.profile": "bogus" }),
        "trace.profile",
    );
    assert!(!config_text(&pd).contains("CHANGED"));
}

#[test]
fn mixed_typed_and_raw_batch_with_invalid_gate_applies_nothing() {
    let (_d, pd) = setup();
    let before = config_text(&pd);
    let resp = update(
        &pd,
        json!({
            "settings.history_limit": 7,
            "trace.requirement_gate": "not-a-table"
        }),
    );
    assert!(is_error(&resp), "{}", text(&resp));
    assert_eq!(before, config_text(&pd));
}

#[test]
fn mixed_typed_and_raw_batch_with_invalid_typed_value_applies_nothing() {
    // A typed validation failure (bail inside the typed loop) must also leave
    // the raw half of the batch unwritten.
    let (_d, pd) = setup();
    let before = config_text(&pd);
    let resp = update(
        &pd,
        json!({
            "trace.done_guard": "warn",
            "settings.memory_dup_threshold": 5
        }),
    );
    assert!(is_error(&resp), "{}", text(&resp));
    assert_eq!(before, config_text(&pd));
}

#[test]
fn mixed_typed_and_raw_batch_applies_both_when_valid() {
    let (_d, pd) = setup();
    let resp = update(
        &pd,
        json!({ "project.name": "RENAMED", "trace.done_guard": "block" }),
    );
    assert!(!is_error(&resp), "{}", text(&resp));
    let cfg = config_text(&pd);
    assert!(
        cfg.contains("RENAMED") && cfg.contains("done_guard = \"block\""),
        "{cfg}"
    );
}
