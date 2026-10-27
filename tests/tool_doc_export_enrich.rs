//! REQ-PDF-030: `handoff_doc_export` with `enrich` draws a meta block under
//! each requirement item's heading (priority / dev stage badges, coverage
//! bars, titled links, implementing tasks, acceptance criteria marked checked
//! or not); `enrich: false` restores the plain output.
//!
//! Driven through the real JSON-RPC entry point with a real Typst CLI. Without
//! Typst the tests print a notice and pass, unless `HANDOFF_REQUIRE_TYPST=1`
//! turns that into a failure.

use handoff_mcp::storage::docs::pdf::deps::{detect, Tool, ToolStatus};
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

fn text(resp: &Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .expect("text content")
        .to_string()
}

fn ok(resp: Value) -> Value {
    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "tool failed: {}",
        text(&resp)
    );
    serde_json::from_str(&text(&resp)).unwrap_or(Value::Null)
}

fn typst_available() -> bool {
    match detect(Tool::Typst) {
        ToolStatus::Found { .. } => true,
        other => {
            assert!(
                std::env::var_os("HANDOFF_REQUIRE_TYPST").is_none(),
                "HANDOFF_REQUIRE_TYPST is set but Typst is unusable: {other:?}"
            );
            eprintln!("SKIPPED: typst not available ({other:?})");
            false
        }
    }
}

/// Joined with explicit `\n`s: a `\` line continuation in a string literal
/// would eat the indentation the nested criteria need.
const BODY_LINES: [&str; 22] = [
    "# Requirements",
    "",
    "## REQ-001 Login",
    "",
    "- priority: P0",
    "",
    "Users can log in.",
    "",
    "受入基準:",
    "- AC1: the login form is shown",
    "- AC2: a wrong password shows an error",
    "",
    "## REQ-002 Logout",
    "",
    "- priority: P1",
    "- acceptance_criteria:",
    "  - [x] AC1: the session ends",
    "  - AC2: the cookie is removed",
    "",
    "Closing words.",
    "",
    "",
];

struct Project {
    _dir: TempDir,
    pd: String,
    doc_id: String,
}

fn project() -> Project {
    let dir = tempfile::tempdir().unwrap();
    let pd = dir.path().to_string_lossy().to_string();
    ok(call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "enrich-test" }),
    ));
    ok(call(
        "handoff_update_config",
        json!({ "project_dir": pd, "updates": { "trace.profile": "minimal" } }),
    ));
    let saved = ok(call(
        "handoff_doc_save",
        json!({
            "project_dir": pd, "slug": "reqs", "title": "Requirements",
            "body": BODY_LINES.join("\n"), "doc_type": "spec", "layer": "requirement"
        }),
    ));
    let doc_id = saved["doc_id"].as_str().unwrap().to_string();
    // One criterion passes; REQ-001 is implemented by a task.
    ok(call(
        "handoff_trace_record",
        json!({ "project_dir": pd, "results": [{"item": "REQ-001#AC1", "result": "pass"}] }),
    ));
    ok(call(
        "handoff_update_task",
        json!({ "project_dir": pd, "task": {
            "id": "t-login", "title": "Build the login form",
            "requirement_ids": ["REQ-001"]
        }}),
    ));
    Project {
        _dir: dir,
        pd,
        doc_id,
    }
}

fn export(p: &Project, extra: Value) -> (Value, String) {
    let mut args = json!({ "project_dir": p.pd, "doc_id": p.doc_id });
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    let out = ok(call("handoff_doc_export", args));
    let typ = std::fs::read_to_string(out["documents"][0]["typ_path"].as_str().unwrap()).unwrap();
    let pdf = out["pdf_path"].as_str().unwrap();
    let bytes = std::fs::read(pdf).unwrap();
    assert!(bytes.starts_with(b"%PDF-"), "{pdf} is not a PDF");
    (out, typ)
}

#[test]
fn enrich_is_the_default_and_draws_meta_blocks_that_compile() {
    if !typst_available() {
        return;
    }
    let p = project();
    let (out, typ) = export(&p, json!({}));
    assert!(
        out["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|w| w["code"] != "enrich_unavailable"),
        "{out}"
    );

    // Each item's block sits directly under its heading, before its body.
    let login = typ.find("REQ-001 Login").expect("heading");
    let login_block = typ[login..]
        .find("#req-meta(priority: \"P0\"")
        .expect("block");
    let login_body = typ[login..].find("Users can log in.").expect("body");
    assert!(login_block < login_body, "{typ}");
    assert!(
        typ.contains("#import \"/.handoff/templates/parts/req-meta.typ\": req-meta"),
        "{typ}"
    );

    // The task that implements REQ-001, by id and title.
    assert!(
        typ.contains("tasks: ((id: \"t-login\", title: \"Build the login form\""),
        "{typ}"
    );
    // REQ-001#AC1 passed, AC2 did not.
    assert!(
        typ.contains("(label: \"AC1\", text: \"the login form is shown\", checked: true)"),
        "{typ}"
    );
    assert!(
        typ.contains("(label: \"AC2\", text: \"a wrong password shows an error\", checked: false)"),
        "{typ}"
    );
    // Coverage is the trace graph's: every criterion of REQ-001 has a
    // verification item (covered) whether or not it has passed yet; nothing
    // refines it into the lower layers (vertical partial/uncovered).
    assert!(typ.contains("horizontal: (state: \"covered\")"), "{typ}");
    assert!(typ.contains("vertical: (state: \"partial\")"), "{typ}");
    // The attribute-form criteria of REQ-002: the [x] mark counts.
    assert!(
        typ.contains("(label: \"AC1\", text: \"the session ends\", checked: true)"),
        "{typ}"
    );
    assert!(
        typ.contains("(label: \"AC2\", text: \"the cookie is removed\", checked: false)"),
        "{typ}"
    );
    // Moved into the block, not repeated in the body.
    assert!(!typ.contains("acceptance\\_criteria"), "{typ}");
    assert!(!typ.contains("受入基準"), "{typ}");
    assert!(typ.contains("Closing words."), "{typ}");

    // The part is generated once and left for the project to restyle.
    let part = std::path::Path::new(&p.pd).join(".handoff/templates/parts/req-meta.typ");
    assert!(part.is_file());
}

#[test]
fn enrich_false_restores_the_plain_export() {
    if !typst_available() {
        return;
    }
    let p = project();
    let (_out, typ) = export(&p, json!({ "enrich": false }));
    assert!(!typ.contains("req-meta"), "{typ}");
    assert!(
        !typ.contains("priority"),
        "attribute lines are dropped: {typ}"
    );
    // Plain output keeps the body as it was (the acceptance text stays).
    assert!(typ.contains("受入基準"), "{typ}");
    assert!(typ.contains("Users can log in."), "{typ}");
    assert!(
        !std::path::Path::new(&p.pd)
            .join(".handoff/templates/parts")
            .exists(),
        "a plain export must not create the part"
    );
}

#[test]
fn enrich_applies_to_every_document_of_a_bundle() {
    if !typst_available() {
        return;
    }
    let p = project();
    let other = ok(call(
        "handoff_doc_save",
        json!({
            "project_dir": p.pd, "slug": "notes", "title": "Notes",
            "body": "# Notes\n\nJust prose.\n", "doc_type": "spec"
        }),
    ));
    let out = ok(call(
        "handoff_doc_export",
        json!({
            "project_dir": p.pd,
            "doc_ids": [other["doc_id"], p.doc_id],
            "bundle": true,
        }),
    ));
    let typ = std::fs::read_to_string(out["typ_path"].as_str().unwrap()).unwrap();
    assert_eq!(typ.matches("#req-meta(").count(), 2, "{typ}");
    assert!(typ.contains("Just prose."), "{typ}");
}

#[test]
fn a_document_without_items_exports_as_before() {
    if !typst_available() {
        return;
    }
    let p = project();
    let plain = ok(call(
        "handoff_doc_save",
        json!({
            "project_dir": p.pd, "slug": "plain", "title": "Plain",
            "body": "# Plain\n\n## REQ-9 Looks like one\n\n- priority: P3\n\nText.\n",
            "doc_type": "spec"
        }),
    ));
    let out = ok(call(
        "handoff_doc_export",
        json!({ "project_dir": p.pd, "doc_id": plain["doc_id"] }),
    ));
    let typ = std::fs::read_to_string(out["documents"][0]["typ_path"].as_str().unwrap()).unwrap();
    assert!(!typ.contains("req-meta"), "{typ}");
    assert!(typ.contains("Text."), "{typ}");
}

#[test]
fn enrich_must_be_a_boolean() {
    if !typst_available() {
        return;
    }
    let p = project();
    let resp = call(
        "handoff_doc_export",
        json!({ "project_dir": p.pd, "doc_id": p.doc_id, "enrich": "yes" }),
    );
    assert!(resp["result"]["isError"].as_bool().unwrap_or(false));
    assert!(text(&resp).contains("'enrich' must be a boolean"));
}
