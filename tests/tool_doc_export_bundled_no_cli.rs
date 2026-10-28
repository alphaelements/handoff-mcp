//! REQ-PDF-040: a `bundled-typst` build exports PDFs with no Typst CLI at all.
//! `PATH` is emptied and `HANDOFF_TYPST_BIN` points at a missing file — the
//! setup that makes a CLI build fail fast (see `tool_doc_export_no_typst.rs`)
//! — and the export through the MCP tools still succeeds.
//!
//! Lives in its own test binary because it rewrites process-global
//! environment variables.
#![cfg(feature = "bundled-typst")]

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

#[test]
fn export_works_without_a_typst_cli_and_ignores_handoff_typst_bin() {
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("PATH", "");
    std::env::set_var("HANDOFF_TYPST_BIN", dir.path().join("no-such-typst"));
    let pd = dir.path().to_string_lossy().to_string();
    let init = call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "bundled" }),
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
    assert!(!is_error(&resp), "{}", text(&resp));
    let pdf = std::fs::read(dir.path().join(".handoff/exports/a-doc.pdf")).expect("PDF written");
    assert!(pdf.starts_with(b"%PDF-"));

    let resp = call(
        "handoff_report",
        json!({ "project_dir": pd, "action": "generate", "report_type": "weekly", "format": "pdf" }),
    );
    assert!(!is_error(&resp), "{}", text(&resp));
}
