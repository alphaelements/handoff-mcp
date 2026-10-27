//! REQ-PDF-001 AC2 / REQ-PDF-009 AC3 through the PDF tools: with no usable
//! Typst CLI the call fails with the tool name, version and install
//! instructions, and has no side effects.
//!
//! Lives in its own test binary because it points `HANDOFF_TYPST_BIN` at a
//! missing file for the whole process.

use serde_json::{json, Value};

fn call(name: &str, arguments: Value) -> Value {
    let req = json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments }
    });
    let out = handoff_mcp::mcp::protocol::process_line(&req.to_string()).expect("response");
    serde_json::from_str(&out).expect("valid JSON response")
}

fn text(resp: &Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .expect("text content")
        .to_string()
}

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"].as_bool().unwrap_or(false)
}

/// One test body: the env var is process-global, so everything that depends
/// on it runs sequentially here.
#[test]
fn missing_typst_fails_fast_with_install_instructions_and_no_side_effects() {
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("HANDOFF_TYPST_BIN", dir.path().join("no-such-typst"));
    let pd = dir.path().to_string_lossy().to_string();
    let init = call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "no-typst" }),
    );
    assert!(!is_error(&init), "{}", text(&init));
    let saved = call(
        "handoff_doc_save",
        json!({ "project_dir": pd, "slug": "a-doc", "title": "A", "body": "# A\n\nbody\n" }),
    );
    assert!(!is_error(&saved), "{}", text(&saved));

    let resp = call(
        "handoff_doc_export",
        json!({ "project_dir": pd, "doc_id": "a-doc", "format": "pdf" }),
    );
    assert!(is_error(&resp));
    let msg = text(&resp);
    assert!(msg.contains("Typst CLI"), "{msg}");
    assert!(msg.contains("install"), "{msg}");
    assert!(!dir.path().join(".handoff/exports/a-doc.pdf").exists());

    let resp = call(
        "handoff_report",
        json!({ "project_dir": pd, "action": "generate", "report_type": "weekly", "format": "pdf" }),
    );
    assert!(is_error(&resp));
    assert!(text(&resp).contains("Typst CLI"), "{}", text(&resp));
    let listed = call(
        "handoff_report",
        json!({ "project_dir": pd, "action": "list" }),
    );
    let listed: Value = serde_json::from_str(&text(&listed)).unwrap();
    assert_eq!(
        listed["reports"].as_array().unwrap().len(),
        0,
        "a report must not be created when its PDF cannot be built"
    );
}
