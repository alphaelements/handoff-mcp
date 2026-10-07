//! FR-514 / SPEC-514: `handoff_report generate report_type=verification`
//! collects its data from the trace report, recorded runs and verification
//! campaigns, and renders the verification template (R1: STR/ATR).

use serde_json::{json, Value};
use tempfile::TempDir;

fn call(name: &str, arguments: Value) -> Value {
    let req = json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments }
    });
    let out = handoff_mcp::mcp::protocol::process_line(&req.to_string()).expect("response");
    serde_json::from_str(&out).expect("valid JSON response")
}

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"].as_bool().unwrap_or(false)
}

fn text(resp: &Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .expect("text content")
        .to_string()
}

fn ok(name: &str, arguments: Value) -> Value {
    let resp = call(name, arguments);
    assert!(!is_error(&resp), "{name} failed: {}", text(&resp));
    serde_json::from_str(&text(&resp)).unwrap_or(Value::Null)
}

/// Requirement REQ-910, three acceptance items: AT-910 (will fail), AT-911
/// (passes), AT-912 (waived by its definition).
fn build_project() -> (TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let pd = dir.path().to_string_lossy().to_string();
    ok(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "Verif Demo" }),
    );
    ok(
        "handoff_doc_save",
        json!({
            "project_dir": pd, "slug": "requirements", "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-910 Export\n\nBody.\n",
        }),
    );
    ok(
        "handoff_doc_save",
        json!({
            "project_dir": pd, "slug": "acceptance", "title": "Acceptance",
            "layer": "acceptance",
            "body": "# Acceptance\n\n\
                ### AT-910 Export CSV | quoted\n\n- verifies: REQ-910\n\nSteps.\n\n\
                ### AT-911 Export JSON\n\n- verifies: REQ-910\n\nSteps.\n\n\
                ### AT-912 Export PDF\n\n- verifies: REQ-910\n- waive-verify: print layout checked by eye\n\nSteps.\n",
        }),
    );
    (dir, pd)
}

fn record(pd: &str, item: &str, result: &str, note: &str, evidence: &[&str]) {
    let out = ok(
        "handoff_trace_update",
        json!({
            "project_dir": pd, "executor_kind": "human", "executor_id": "alice",
            "ops": [{ "op": "record", "item": item, "result": result,
                      "note": note, "evidence": evidence }]
        }),
    );
    assert!(out.get("failed").is_none(), "{out}");
}

fn generate(pd: &str, scope: Value) -> (Value, String) {
    let out = ok(
        "handoff_report",
        json!({ "project_dir": pd, "action": "generate",
                "report_type": "verification", "scope": scope }),
    );
    let id = out["report"]["report_id"].as_str().unwrap().to_string();
    let got = ok(
        "handoff_report",
        json!({ "project_dir": pd, "action": "get", "report_id": id }),
    );
    let body = got["body"].as_str().unwrap().to_string();
    (out, body)
}

#[test]
fn layer_scope_report_shows_results_evidence_failures_and_waivers() {
    let (_dir, pd) = build_project();
    record(
        &pd,
        "AT-910",
        "fail",
        "wrong header",
        &["evidence/at910 run.png"],
    );
    record(&pd, "AT-911", "pass", "", &["https://ci.example/run/7"]);

    let (_, md) = generate(&pd, json!({ "layers": ["acceptance"] }));

    assert!(md.contains("| Project | Verif Demo |"), "{md}");
    assert!(md.contains("| Pass | 1 | 33.3% |"), "{md}");
    assert!(md.contains("| Fail | 1 | 33.3% |"), "{md}");
    assert!(md.contains("| Waived | 1 | 33.3% |"), "{md}");
    assert!(md.contains("| acceptance | in_progress | 3 |"), "{md}");
    // Result rows: verifier from the run, evidence as links (relative to reports/).
    assert!(md.contains("human:alice"), "{md}");
    assert!(
        md.contains("[evidence/at910 run.png](../../evidence/at910%20run.png)"),
        "{md}"
    );
    assert!(
        md.contains("[https://ci.example/run/7](https://ci.example/run/7)"),
        "{md}"
    );
    // Pipe in a title is escaped; failure carries the recorded note.
    assert!(md.contains("Export CSV \\| quoted"), "{md}");
    assert!(md.contains("wrong header"), "{md}");
    // Waived item with its reason.
    assert!(md.contains("print layout checked by eye"), "{md}");
    // The requirement layer is out of scope.
    assert!(!md.contains("REQ-910"), "{md}");
}

#[test]
fn status_and_item_filters_narrow_the_report() {
    let (_dir, pd) = build_project();
    record(&pd, "AT-910", "fail", "wrong header", &[]);
    record(&pd, "AT-911", "pass", "", &[]);

    let (_, md) = generate(
        &pd,
        json!({ "layers": ["acceptance"], "statuses": ["fail"] }),
    );
    assert!(md.contains("AT-910"), "{md}");
    assert!(!md.contains("AT-911"), "{md}");
    assert!(md.contains("| Fail | 1 | 100.0% |"), "{md}");

    let (_, md) = generate(&pd, json!({ "items": ["AT-911", "AT-404"] }));
    assert!(md.contains("AT-911"), "{md}");
    assert!(!md.contains("| AT-910 |"), "{md}");
    assert!(md.contains("Item 'AT-404' was not found"), "{md}");
}

#[test]
fn campaign_scope_report_uses_the_checklist_and_its_approval() {
    let (_dir, pd) = build_project();
    let created = ok(
        "handoff_trace_test_run",
        json!({ "project_dir": pd, "action": "create", "label": "Release 1",
                "scope": { "layers": ["acceptance"] } }),
    );
    let id = created["test_run_id"].as_str().unwrap().to_string();
    let check = |item: &str, result: &str, note: &str, evidence: Value| {
        ok(
            "handoff_trace_test_run",
            json!({ "project_dir": pd, "action": "record_check", "test_run_id": id,
                    "item_id": item, "result": result, "note": note,
                    "verified_by": "carol", "evidence": evidence }),
        );
    };
    check(
        "AT-910",
        "pass",
        "fine",
        json!([{ "path": "evidence/at910.png", "type": "screenshot", "caption": "after export" }]),
    );
    check("AT-911", "fail", "timeout", json!([]));
    check("AT-912", "waived", "approved by PO", json!([]));
    ok(
        "handoff_trace_test_run",
        json!({ "project_dir": pd, "action": "set_status", "test_run_id": id, "status": "completed" }),
    );
    ok(
        "handoff_trace_test_run",
        json!({ "project_dir": pd, "action": "set_status", "test_run_id": id,
                "status": "approved", "approved_by": "dave" }),
    );

    let (out, md) = generate(&pd, json!({ "campaign": id }));
    assert_eq!(out["report"]["scope"]["campaign"], json!(id));
    assert!(md.contains("Release 1"), "{md}");
    assert!(md.contains("approved"), "{md}");
    assert!(md.contains("dave"), "{md}");
    assert!(md.contains("carol"), "{md}");
    assert!(
        md.contains("[after export](../../evidence/at910.png)"),
        "{md}"
    );
    assert!(md.contains("timeout"), "{md}");
    assert!(md.contains("| Pass | 1 | 33.3% |"), "{md}");
    assert!(
        md.contains("| AT-912 | Export PDF | print layout checked by eye | dave |"),
        "{md}"
    );
}

#[test]
fn unknown_campaign_and_unknown_status_are_errors() {
    let (_dir, pd) = build_project();
    for scope in [
        json!({ "campaign": "nope" }),
        json!({ "statuses": ["passed"] }),
    ] {
        let resp = call(
            "handoff_report",
            json!({ "project_dir": pd, "action": "generate",
                    "report_type": "verification", "scope": scope }),
        );
        assert!(is_error(&resp), "{}", text(&resp));
    }
    let listed = ok(
        "handoff_report",
        json!({ "project_dir": pd, "action": "list" }),
    );
    assert_eq!(listed["reports"].as_array().unwrap().len(), 0, "{listed}");
}
