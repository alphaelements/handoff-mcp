//! `handoff_load_context`'s M4 health summary (t378.4, wiki/270 design):
//! `trace_health`/`requirements_health`, read as a lightweight pass over
//! `.handoff/docs/_trace_report.json` / `_requirements_summary.json` (both
//! written by M5, t378.5) rather than by rebuilding either derivation from
//! scratch. Same in-process JSON-RPC harness as `tests/tool_sessions.rs`.

use serde_json::{json, Value};
use tempfile::TempDir;

fn send(input: &str) -> Option<Value> {
    let result = handoff_mcp::mcp::protocol::process_line(input)?;
    Some(serde_json::from_str(&result).expect("response should be valid JSON"))
}

fn setup_project() -> TempDir {
    let dir = tempfile::tempdir().expect("failed to create temp dir");

    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let req = json!({
        "jsonrpc": "2.0", "id": 0,
        "method": "tools/call",
        "params": {
            "name": "handoff_init",
            "arguments": {
                "project_dir": dir.path().to_string_lossy(),
                "project_name": "test"
            }
        }
    });
    send(&req.to_string()).unwrap();
    let cfg = json!({
        "jsonrpc": "2.0", "id": 0,
        "method": "tools/call",
        "params": {
            "name": "handoff_update_config",
            "arguments": {
                "project_dir": dir.path().to_string_lossy(),
                "updates": { "settings.require_estimate_hours": false }
            }
        }
    });
    send(&cfg.to_string()).unwrap();
    dir
}

fn call_tool(name: &str, arguments: Value) -> Value {
    let req = json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments }
    });
    send(&req.to_string()).unwrap()
}

fn get_text(resp: &Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn load_context(pd: &str) -> Value {
    let resp = call_tool("handoff_load_context", json!({ "project_dir": pd }));
    let text = get_text(&resp);
    serde_json::from_str(&text).unwrap()
}

/// No `_trace_report.json`/`_requirements_summary.json` yet (brand-new
/// project, neither `handoff_trace_report` nor any `doc_verify`-affecting
/// call has run) -> `has_data: false`, empty warnings, no error.
#[test]
fn load_context_reports_no_data_when_neither_derived_file_exists() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let parsed = load_context(&pd);

    assert_eq!(
        parsed["trace_health"]["has_data"],
        json!(false),
        "trace_health.has_data must be false when _trace_report.json does not exist yet: \
         {parsed}"
    );
    assert_eq!(
        parsed["trace_health"]["warnings"],
        json!([]),
        "trace_health.warnings must be empty when there is no data: {parsed}"
    );
    assert_eq!(
        parsed["requirements_health"]["has_data"],
        json!(false),
        "requirements_health.has_data must be false when _requirements_summary.json does not \
         exist yet: {parsed}"
    );
    assert_eq!(
        parsed["requirements_health"]["warnings"],
        json!([]),
        "requirements_health.warnings must be empty when there is no data: {parsed}"
    );
    assert_eq!(
        parsed["docs_health"]["has_data"],
        json!(false),
        "docs_health.has_data must be false when _requirements_summary.json does not exist \
         yet: {parsed}"
    );
    assert_eq!(
        parsed["docs_health"]["warnings"],
        json!([]),
        "docs_health.warnings must be empty when there is no data: {parsed}"
    );
}

/// `handoff_trace_report` on a brand-new project (no `[trace]` layers
/// configured, no layer documents) persists DIAG-T001/T002 into
/// `_trace_report.json` (M5) — `load_context` must surface them in
/// `trace_health` without re-deriving anything, and must fold a mention into
/// `session_guidance.message`.
#[test]
fn load_context_surfaces_trace_report_warnings_and_flags_session_guidance() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool("handoff_trace_report", json!({ "project_dir": &pd }));

    let parsed = load_context(&pd);

    assert_eq!(
        parsed["trace_health"]["has_data"],
        json!(true),
        "trace_health.has_data must be true once _trace_report.json exists: {parsed}"
    );
    let warnings = parsed["trace_health"]["warnings"]
        .as_array()
        .expect("trace_health.warnings must be an array");
    let codes: Vec<&str> = warnings
        .iter()
        .filter_map(|w| w.get("code").and_then(|c| c.as_str()))
        .collect();
    assert!(
        codes.contains(&"DIAG-T001"),
        "expected DIAG-T001 to surface in trace_health.warnings, got: {codes:?}"
    );

    let guidance_message = parsed["session_guidance"]["message"]
        .as_str()
        .unwrap_or_default();
    assert!(
        !guidance_message.is_empty(),
        "session_guidance.message must mention project health when warnings are present: \
         {parsed}"
    );
}

/// A project with real layered data (no DIAG-T0* applicable) must report
/// `has_data: true` with an empty `warnings` array, plus the `coverage`
/// block carried over from `_trace_report.json`.
#[test]
fn load_context_reports_healthy_trace_data_with_coverage() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_doc_save",
        json!({
            "project_dir": &pd,
            "slug": "req-doc",
            "title": "Req doc",
            "body": "# Req\n\n### REQ-100 title\n\nbody.\n",
            "layer": "requirement",
        }),
    );
    call_tool("handoff_trace_report", json!({ "project_dir": &pd }));

    let parsed = load_context(&pd);

    assert_eq!(parsed["trace_health"]["has_data"], json!(true));
    assert_eq!(
        parsed["trace_health"]["warnings"],
        json!([]),
        "a healthy project must have no trace_health warnings: {parsed}"
    );
    assert!(
        parsed["trace_health"]["coverage"].is_object(),
        "trace_health.coverage must be carried over from _trace_report.json: {parsed}"
    );
}

/// `requirements_health` must carry `total`/`by_status` straight from
/// `_requirements_summary.json` once it exists (written as a side effect of
/// `handoff_doc_verify`).
#[test]
fn load_context_reports_requirements_health_total_and_by_status() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_doc_save",
        json!({
            "project_dir": &pd,
            "slug": "req-doc",
            "title": "Req doc",
            "body": "# Req\n\n### REQ-100 title\n\nbody.\n",
            "layer": "requirement",
        }),
    );
    // `doc_verify` on the section containing REQ-100's sub_item triggers
    // `write_requirements_summary` (P0 §3.4/§2.4 step 7).
    call_tool(
        "handoff_doc_verify",
        json!({
            "project_dir": &pd,
            "slug": "req-doc",
            "fragment_seq": 1,
            "action": "verify",
        }),
    );

    let parsed = load_context(&pd);

    assert_eq!(parsed["requirements_health"]["has_data"], json!(true));
    assert!(
        parsed["requirements_health"]["total"].as_u64().is_some(),
        "requirements_health.total must be a number once the file exists: {parsed}"
    );
    assert!(
        parsed["requirements_health"]["by_status"].is_object(),
        "requirements_health.by_status must be carried over: {parsed}"
    );
}

/// `docs_health` is `load_context`'s document-level view of the same
/// `_requirements_summary.json` file `requirements_health` reads: it carries
/// the DIAG-R001 (layer-unset/no-matrix document count) warning plus the
/// total document count (`inputs.docs_count`), since that diagnostic is
/// fundamentally about *documents* being excluded from aggregation, not
/// about requirement-item stats.
#[test]
fn load_context_reports_docs_health_with_layer_unset_warning() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // One doc with no `layer` set (triggers DIAG-R001), saved *before* the
    // layer doc below so that the layer doc's save-time summary refresh
    // (`refresh_after_layer_sync`, triggered by its own `layer_synced`
    // sub-item parse) sees both documents already on disk.
    call_tool(
        "handoff_doc_save",
        json!({
            "project_dir": &pd,
            "slug": "unlayered-doc",
            "title": "Unlayered doc",
            "body": "# Notes\n\nsome body.\n",
        }),
    );
    call_tool(
        "handoff_doc_save",
        json!({
            "project_dir": &pd,
            "slug": "req-doc",
            "title": "Req doc",
            "body": "# Req\n\n### REQ-100 title\n\nbody.\n",
            "layer": "requirement",
        }),
    );

    let parsed = load_context(&pd);

    assert_eq!(parsed["docs_health"]["has_data"], json!(true));
    let warnings = parsed["docs_health"]["warnings"]
        .as_array()
        .expect("docs_health.warnings must be an array");
    let codes: Vec<&str> = warnings
        .iter()
        .filter_map(|w| w.get("code").and_then(|c| c.as_str()))
        .collect();
    assert!(
        codes.contains(&"DIAG-R001"),
        "expected DIAG-R001 to surface in docs_health.warnings, got: {codes:?}"
    );
    assert!(
        parsed["docs_health"]["total_docs"].as_u64().is_some(),
        "docs_health.total_docs must be a number once the file exists: {parsed}"
    );
}

/// Performance requirement (done_criteria): `load_context`'s latency must
/// stay within 200ms even once both derived files exist — the M4 read path
/// must stay a lightweight `fs::read` + `from_slice`, never
/// `aggregate_requirements`/`rebuild_trace_graph`.
#[test]
fn load_context_stays_within_200ms_with_both_derived_files_present() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_doc_save",
        json!({
            "project_dir": &pd,
            "slug": "req-doc",
            "title": "Req doc",
            "body": "# Req\n\n### REQ-100 title\n\nbody.\n",
            "layer": "requirement",
        }),
    );
    call_tool("handoff_trace_report", json!({ "project_dir": &pd }));
    call_tool(
        "handoff_doc_verify",
        json!({
            "project_dir": &pd,
            "slug": "req-doc",
            "fragment_seq": 1,
            "action": "verify",
        }),
    );

    let start = std::time::Instant::now();
    let _ = load_context(&pd);
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_millis() < 200,
        "load_context must stay within 200ms, took {elapsed:?}"
    );
}

/// `docs_health`'s own warnings (not just `trace_health`/
/// `requirements_health`'s) must also feed `session_guidance.message` — the
/// DIAG-R001 layer-unset warning is `severity: "warning"`, so a project with
/// one unlayered document must surface a guidance message even when
/// `trace_health`/`requirements_health` are both clean.
#[test]
fn load_context_docs_health_warning_flags_session_guidance() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_doc_save",
        json!({
            "project_dir": &pd,
            "slug": "unlayered-doc",
            "title": "Unlayered doc",
            "body": "# Notes\n\nsome body.\n",
        }),
    );
    call_tool(
        "handoff_doc_save",
        json!({
            "project_dir": &pd,
            "slug": "req-doc",
            "title": "Req doc",
            "body": "# Req\n\n### REQ-100 title\n\nbody.\n",
            "layer": "requirement",
        }),
    );

    let parsed = load_context(&pd);

    assert!(
        parsed["docs_health"]["warnings"]
            .as_array()
            .is_some_and(|w| !w.is_empty()),
        "expected docs_health.warnings to be non-empty: {parsed}"
    );
    let guidance_message = parsed["session_guidance"]["message"]
        .as_str()
        .unwrap_or_default();
    assert!(
        !guidance_message.is_empty(),
        "session_guidance.message must mention project health when docs_health has a \
         warning-severity entry: {parsed}"
    );
}
