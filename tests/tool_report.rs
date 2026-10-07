//! FR-513 / SPEC-513: `handoff_report` — report engine foundation
//! (generate / list / get / submit / approve / reject) over `.handoff/reports/`
//! with built-in Handlebars templates overridable from `.handoff/templates/`.

use serde_json::{json, Value};
use std::path::PathBuf;
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

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"].as_bool().unwrap_or(false)
}

fn setup() -> (TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let pd = dir.path().to_string_lossy().to_string();
    let resp = call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "report-test" }),
    );
    assert!(!is_error(&resp), "init failed: {}", text(&resp));
    (dir, pd)
}

fn report(pd: &str, mut args: Value) -> Value {
    args["project_dir"] = json!(pd);
    call("handoff_report", args)
}

/// Calls `handoff_report`, asserts success, and returns the parsed JSON body.
fn report_ok(pd: &str, args: Value) -> Value {
    let resp = report(pd, args.clone());
    assert!(!is_error(&resp), "{args} failed: {}", text(&resp));
    serde_json::from_str(&text(&resp)).expect("tool output is JSON")
}

/// Calls `handoff_report`, asserts an error, and returns its message.
fn report_err(pd: &str, args: Value) -> String {
    let resp = report(pd, args.clone());
    assert!(
        is_error(&resp),
        "{args} should have failed: {}",
        text(&resp)
    );
    text(&resp)
}

fn handoff(dir: &TempDir) -> PathBuf {
    dir.path().join(".handoff")
}

fn generate(pd: &str, report_type: &str) -> String {
    let out = report_ok(
        pd,
        json!({ "action": "generate", "report_type": report_type }),
    );
    out["report"]["report_id"].as_str().unwrap().to_string()
}

#[test]
fn tool_is_registered() {
    let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
    let out = handoff_mcp::mcp::protocol::process_line(&req.to_string()).unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    let names: Vec<&str> = v["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(names.contains(&"handoff_report"));
}

#[test]
fn generate_writes_markdown_and_metadata() {
    let (dir, pd) = setup();
    let out = report_ok(
        &pd,
        json!({
            "action": "generate",
            "report_type": "verification",
            "scope": { "label": "Sprint 5", "layers": ["system", "unit"] },
            "data": { "summary": "All <green> & done" }
        }),
    );

    let meta = &out["report"];
    let id = meta["report_id"].as_str().unwrap();
    assert!(id.starts_with("verification-"), "id: {id}");
    assert_eq!(meta["report_type"], "verification");
    assert_eq!(meta["status"], "draft");
    assert_eq!(meta["version"], 1);
    assert_eq!(meta["scope"]["label"], "Sprint 5");
    assert_eq!(meta["output_path"], format!("reports/{id}.md"));
    assert!(meta["reviewer"].is_null());
    assert!(meta["approved_at"].is_null());
    assert_eq!(meta["revision_history"].as_array().unwrap().len(), 1);
    assert_eq!(meta["revision_history"][0]["to"], "draft");

    let md = std::fs::read_to_string(handoff(&dir).join(format!("reports/{id}.md"))).unwrap();
    assert!(md.contains(id), "template renders the report id: {md}");
    assert!(md.contains("Sprint 5"), "template renders scope: {md}");
    // Markdown output: Handlebars HTML escaping must be off.
    assert!(
        md.contains("All <green> & done"),
        "data rendered unescaped: {md}"
    );

    let on_disk: Value = serde_json::from_str(
        &std::fs::read_to_string(handoff(&dir).join(format!("reports/{id}.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(&on_disk, meta);
}

#[test]
fn generate_weekly_uses_its_own_template() {
    let (dir, pd) = setup();
    let id = generate(&pd, "weekly");
    let md = std::fs::read_to_string(handoff(&dir).join(format!("reports/{id}.md"))).unwrap();
    assert!(md.contains("Weekly"), "{md}");
}

#[test]
fn generate_rejects_unknown_type_and_bad_scope() {
    let (_dir, pd) = setup();
    let e = report_err(&pd, json!({ "action": "generate", "report_type": "nope" }));
    assert!(e.contains("report_type"), "{e}");
    let e = report_err(&pd, json!({ "action": "generate" }));
    assert!(e.contains("report_type"), "{e}");
    let e = report_err(
        &pd,
        json!({ "action": "generate", "report_type": "weekly", "scope": { "lable": "typo" } }),
    );
    assert!(e.contains("scope"), "{e}");
    let e = report_err(
        &pd,
        json!({ "action": "generate", "report_type": "weekly", "data": "not an object" }),
    );
    assert!(e.contains("data"), "{e}");
}

#[test]
fn repeated_generate_gets_distinct_ids_and_increasing_versions() {
    let (_dir, pd) = setup();
    let a = report_ok(
        &pd,
        json!({ "action": "generate", "report_type": "weekly", "scope": { "label": "w1" } }),
    );
    let b = report_ok(
        &pd,
        json!({ "action": "generate", "report_type": "weekly", "scope": { "label": "w1" } }),
    );
    let c = report_ok(
        &pd,
        json!({ "action": "generate", "report_type": "weekly", "scope": { "label": "w2" } }),
    );
    assert_ne!(a["report"]["report_id"], b["report"]["report_id"]);
    assert_eq!(a["report"]["version"], 1);
    assert_eq!(b["report"]["version"], 2);
    assert_eq!(c["report"]["version"], 1, "different scope restarts at 1");
}

#[test]
fn list_returns_reports_and_filters() {
    let (dir, pd) = setup();
    let empty = report_ok(&pd, json!({ "action": "list" }));
    assert_eq!(empty["reports"].as_array().unwrap().len(), 0);

    let v = generate(&pd, "verification");
    let w = generate(&pd, "weekly");
    report_ok(&pd, json!({ "action": "submit", "report_id": w }));

    let all = report_ok(&pd, json!({ "action": "list" }));
    assert_eq!(all["reports"].as_array().unwrap().len(), 2);

    let only_v = report_ok(
        &pd,
        json!({ "action": "list", "report_type": "verification" }),
    );
    let ids: Vec<&str> = only_v["reports"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["report_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![v.as_str()]);

    let submitted = report_ok(&pd, json!({ "action": "list", "status": "submitted" }));
    assert_eq!(submitted["reports"].as_array().unwrap().len(), 1);
    assert_eq!(submitted["reports"][0]["report_id"], w);

    let e = report_err(&pd, json!({ "action": "list", "status": "bogus" }));
    assert!(e.contains("status"), "{e}");

    // A corrupt metadata file is reported, not fatal.
    std::fs::write(handoff(&dir).join("reports/broken.json"), "{ not json").unwrap();
    let with_warning = report_ok(&pd, json!({ "action": "list" }));
    assert_eq!(with_warning["reports"].as_array().unwrap().len(), 2);
    assert_eq!(with_warning["warnings"].as_array().unwrap().len(), 1);
}

#[test]
fn get_returns_metadata_and_body() {
    let (_dir, pd) = setup();
    let id = generate(&pd, "verification");
    let got = report_ok(&pd, json!({ "action": "get", "report_id": id }));
    assert_eq!(got["report"]["report_id"], id);
    assert!(got["body"].as_str().unwrap().contains(&id));
}

#[test]
fn get_unknown_or_unsafe_id_is_an_error() {
    let (_dir, pd) = setup();
    let e = report_err(&pd, json!({ "action": "get", "report_id": "missing-1" }));
    assert!(e.contains("missing-1"), "{e}");
    let e = report_err(&pd, json!({ "action": "get", "report_id": "../config" }));
    assert!(e.contains("report_id"), "{e}");
    let e = report_err(&pd, json!({ "action": "get" }));
    assert!(e.contains("report_id"), "{e}");
}

#[test]
fn submit_then_approve_records_reviewer_and_history() {
    let (dir, pd) = setup();
    let id = generate(&pd, "verification");

    let submitted = report_ok(&pd, json!({ "action": "submit", "report_id": id }));
    assert_eq!(submitted["report"]["status"], "submitted");

    let approved = report_ok(
        &pd,
        json!({ "action": "approve", "report_id": id, "reviewer": "alice", "comment": "LGTM" }),
    );
    let meta = &approved["report"];
    assert_eq!(meta["status"], "approved");
    assert_eq!(meta["reviewer"], "alice");
    assert!(meta["approved_at"].is_string());
    let history = meta["revision_history"].as_array().unwrap();
    let tos: Vec<&str> = history.iter().map(|h| h["to"].as_str().unwrap()).collect();
    assert_eq!(tos, vec!["draft", "submitted", "approved"]);
    assert_eq!(history[2]["comment"], "LGTM");
    assert_eq!(history[2]["actor"], "alice");

    // Persisted, not just echoed.
    let on_disk: Value = serde_json::from_str(
        &std::fs::read_to_string(handoff(&dir).join(format!("reports/{id}.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(on_disk["status"], "approved");
}

#[test]
fn reject_requests_revision_and_allows_resubmit() {
    let (_dir, pd) = setup();
    let id = generate(&pd, "weekly");
    report_ok(&pd, json!({ "action": "submit", "report_id": id }));

    let rejected = report_ok(
        &pd,
        json!({ "action": "reject", "report_id": id, "reviewer": "bob", "comment": "add totals" }),
    );
    assert_eq!(rejected["report"]["status"], "revision_requested");
    assert_eq!(rejected["report"]["reviewer"], "bob");
    assert!(rejected["report"]["approved_at"].is_null());

    report_ok(&pd, json!({ "action": "submit", "report_id": id }));
    let approved = report_ok(
        &pd,
        json!({ "action": "approve", "report_id": id, "reviewer": "bob" }),
    );
    assert_eq!(approved["report"]["status"], "approved");
    assert_eq!(
        approved["report"]["revision_history"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
}

#[test]
fn invalid_transitions_are_rejected_without_changing_state() {
    let (_dir, pd) = setup();
    let id = generate(&pd, "weekly");

    // draft cannot be approved/rejected directly.
    let e = report_err(
        &pd,
        json!({ "action": "approve", "report_id": id, "reviewer": "a" }),
    );
    assert!(e.contains("draft"), "{e}");
    let e = report_err(
        &pd,
        json!({ "action": "reject", "report_id": id, "reviewer": "a", "comment": "x" }),
    );
    assert!(e.contains("draft"), "{e}");

    report_ok(&pd, json!({ "action": "submit", "report_id": id }));
    // submitted cannot be submitted again.
    let e = report_err(&pd, json!({ "action": "submit", "report_id": id }));
    assert!(e.contains("submitted"), "{e}");

    // reject requires a reason.
    let e = report_err(
        &pd,
        json!({ "action": "reject", "report_id": id, "reviewer": "a" }),
    );
    assert!(e.contains("comment"), "{e}");

    // approve needs a reviewer when no agent identity is known.
    let e = report_err(&pd, json!({ "action": "approve", "report_id": id }));
    assert!(e.contains("reviewer"), "{e}");

    report_ok(
        &pd,
        json!({ "action": "approve", "report_id": id, "reviewer": "a" }),
    );
    // approved is terminal for now.
    let e = report_err(&pd, json!({ "action": "submit", "report_id": id }));
    assert!(e.contains("approved"), "{e}");

    let got = report_ok(&pd, json!({ "action": "get", "report_id": id }));
    assert_eq!(got["report"]["status"], "approved");
    assert_eq!(
        got["report"]["revision_history"].as_array().unwrap().len(),
        3
    );
}

#[test]
fn custom_template_overrides_builtin() {
    let (dir, pd) = setup();
    let tpl_dir = handoff(&dir).join("templates");
    std::fs::create_dir_all(&tpl_dir).unwrap();
    std::fs::write(
        tpl_dir.join("weekly.md.hbs"),
        "CUSTOM {{report.report_type}} v{{report.version}} {{data.note}}\n",
    )
    .unwrap();

    let out = report_ok(
        &pd,
        json!({ "action": "generate", "report_type": "weekly", "data": { "note": "hello" } }),
    );
    assert_eq!(out["custom_templates"], json!(["weekly"]));
    let id = out["report"]["report_id"].as_str().unwrap();
    let md = std::fs::read_to_string(handoff(&dir).join(format!("reports/{id}.md"))).unwrap();
    assert_eq!(md, "CUSTOM weekly v1 hello\n");

    // The other built-in is untouched.
    let v = generate(&pd, "verification");
    let md = std::fs::read_to_string(handoff(&dir).join(format!("reports/{v}.md"))).unwrap();
    assert!(!md.contains("CUSTOM"), "{md}");
}

#[test]
fn broken_custom_template_fails_loudly_and_writes_nothing() {
    let (dir, pd) = setup();
    let tpl_dir = handoff(&dir).join("templates");
    std::fs::create_dir_all(&tpl_dir).unwrap();
    std::fs::write(tpl_dir.join("weekly.md.hbs"), "{{#if}} unclosed").unwrap();

    let e = report_err(
        &pd,
        json!({ "action": "generate", "report_type": "weekly" }),
    );
    assert!(e.contains("weekly.md.hbs"), "{e}");
    assert!(!handoff(&dir).join("reports").exists());
}

#[test]
fn unknown_or_missing_action_is_an_error() {
    let (_dir, pd) = setup();
    let e = report_err(&pd, json!({ "action": "publish" }));
    assert!(e.contains("action"), "{e}");
    let e = report_err(&pd, json!({}));
    assert!(e.contains("action"), "{e}");
}
