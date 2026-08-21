//! `.handoff/version` marker (spec §3.7): written by `handoff_init`, checked
//! by `handoff_load_context` so mixed-version instances sharing one
//! `.handoff/` get a visible warning instead of silently misbehaving.

use serde_json::{json, Value};
use tempfile::TempDir;

fn send(input: &str) -> Option<Value> {
    let result = handoff_mcp::mcp::protocol::process_line(input)?;
    Some(serde_json::from_str(&result).expect("response should be valid JSON"))
}

fn setup() -> TempDir {
    tempfile::tempdir().expect("failed to create temp dir")
}

fn call(name: &str, arguments: Value) -> Value {
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": name,
            "arguments": arguments
        }
    });
    send(&req.to_string()).expect("should return response")
}

/// `handoff_init` must create `.handoff/version` containing exactly the
/// running binary's `CARGO_PKG_VERSION`.
#[test]
fn init_creates_version_marker_file() {
    let dir = setup();
    let project_dir = dir.path().to_string_lossy().to_string();

    call(
        "handoff_init",
        json!({
            "project_dir": project_dir,
            "project_name": "version-test"
        }),
    );

    let version_path = dir.path().join(".handoff/version");
    assert!(version_path.exists(), "version marker should be created");

    let content = std::fs::read_to_string(&version_path).unwrap();
    assert_eq!(content, env!("CARGO_PKG_VERSION"));
}

/// When `.handoff/version` matches the running binary, `load_context` must
/// not emit a version warning.
#[test]
fn load_context_no_warning_when_versions_match() {
    let dir = setup();
    let project_dir = dir.path().to_string_lossy().to_string();

    call(
        "handoff_init",
        json!({
            "project_dir": project_dir,
            "project_name": "match-test"
        }),
    );

    let resp = call(
        "handoff_load_context",
        json!({ "project_dir": project_dir }),
    );
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.get("warning").is_none()
            || !parsed["warning"]
                .as_str()
                .unwrap_or_default()
                .contains("version"),
        "unexpected version warning: {parsed}"
    );
}

/// When `.handoff/version` differs from the running binary, `load_context`
/// must surface a warning naming both versions.
#[test]
fn load_context_warns_on_version_mismatch() {
    let dir = setup();
    let project_dir = dir.path().to_string_lossy().to_string();

    call(
        "handoff_init",
        json!({
            "project_dir": project_dir,
            "project_name": "mismatch-test"
        }),
    );

    std::fs::write(dir.path().join(".handoff/version"), "0.0.1-does-not-exist").unwrap();

    let resp = call(
        "handoff_load_context",
        json!({ "project_dir": project_dir }),
    );
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();

    let warning = parsed["warning"]
        .as_str()
        .expect("expected a version mismatch warning");
    assert!(warning.contains("0.0.1-does-not-exist"));
    assert!(warning.contains(env!("CARGO_PKG_VERSION")));
}

/// When both a version mismatch and an unresolved `session_id` occur on the
/// same `load_context` call, both warnings must be surfaced together — one
/// must not silently overwrite the other.
#[test]
fn load_context_shows_both_version_and_session_warnings() {
    let dir = setup();
    let project_dir = dir.path().to_string_lossy().to_string();

    call(
        "handoff_init",
        json!({
            "project_dir": project_dir,
            "project_name": "combined-warning-test"
        }),
    );

    std::fs::write(dir.path().join(".handoff/version"), "0.0.1-does-not-exist").unwrap();

    let resp = call(
        "handoff_load_context",
        json!({ "project_dir": project_dir, "session_id": "does-not-exist" }),
    );
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();

    let warning = parsed["warning"]
        .as_str()
        .expect("expected a combined warning");
    assert!(
        warning.contains("0.0.1-does-not-exist") && warning.contains(env!("CARGO_PKG_VERSION")),
        "expected version mismatch warning, got: {warning}"
    );
    assert!(
        warning.contains("does-not-exist"),
        "expected session-not-found warning, got: {warning}"
    );
}

/// A `.handoff/` created before this feature existed has no `version` file
/// at all; `load_context` must not warn in that case (nothing to compare
/// against).
#[test]
fn load_context_no_warning_when_version_marker_absent() {
    let dir = setup();
    let project_dir = dir.path().to_string_lossy().to_string();

    call(
        "handoff_init",
        json!({
            "project_dir": project_dir,
            "project_name": "no-marker-test"
        }),
    );

    std::fs::remove_file(dir.path().join(".handoff/version")).unwrap();

    let resp = call(
        "handoff_load_context",
        json!({ "project_dir": project_dir }),
    );
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.get("warning").is_none(),
        "should not warn when version marker is absent: {parsed}"
    );
}
