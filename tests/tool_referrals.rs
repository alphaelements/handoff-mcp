use serde_json::{json, Value};
use tempfile::TempDir;

fn send(input: &str) -> Option<Value> {
    let result = handoff_mcp::mcp::protocol::process_line(input)?;
    Some(serde_json::from_str(&result).expect("response should be valid JSON"))
}

fn setup_project(base: &std::path::Path, name: &str) -> std::path::PathBuf {
    let dir = base.join(name);
    std::fs::create_dir_all(&dir).unwrap();

    std::process::Command::new("git")
        .args(["init"])
        .current_dir(&dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(&dir)
        .output()
        .unwrap();

    let req = json!({
        "jsonrpc": "2.0", "id": 0,
        "method": "tools/call",
        "params": {
            "name": "handoff_init",
            "arguments": {
                "project_dir": dir.to_string_lossy(),
                "project_name": name
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
                "project_dir": dir.to_string_lossy(),
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
        .unwrap_or("")
        .to_string()
}

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"].as_bool().unwrap_or(false)
}

fn setup_two_projects() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let base = tempfile::tempdir().expect("failed to create temp dir");
    let proj_a = setup_project(base.path(), "project-a");
    let proj_b = setup_project(base.path(), "project-b");
    (base, proj_a, proj_b)
}

#[test]
fn send_referral_by_path() {
    let (_base, proj_a, proj_b) = setup_two_projects();

    let resp = call_tool(
        "handoff_refer",
        json!({
            "project_dir": proj_a.to_string_lossy(),
            "target_project_dir": proj_b.to_string_lossy(),
            "summary": "Please fix the bug",
            "referral_type": "bug",
            "priority": "high"
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(text.contains("Referral sent"));
    assert!(text.contains("project-a"));
    assert!(text.contains("project-b"));
    assert!(text.contains("bug"));

    let referrals_dir = proj_b.join(".handoff/referrals");
    assert!(referrals_dir.exists());
    let files: Vec<_> = std::fs::read_dir(&referrals_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(files.len(), 1);
    assert!(files[0]
        .file_name()
        .to_string_lossy()
        .ends_with(".open.json"));
}

#[test]
fn send_referral_by_name() {
    let (base, proj_a, _proj_b) = setup_two_projects();

    let resp = call_tool(
        "handoff_update_config",
        json!({
            "project_dir": proj_a.to_string_lossy(),
            "updates": {
                "dashboard.scan_dirs": [base.path().to_string_lossy()]
            }
        }),
    );
    assert!(!is_error(&resp), "config update error: {}", get_text(&resp));

    let resp = call_tool(
        "handoff_refer",
        json!({
            "project_dir": proj_a.to_string_lossy(),
            "target_project": "project-b",
            "summary": "Feature request via name",
            "referral_type": "improvement"
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(text.contains("Referral sent"));
    assert!(text.contains("project-b"));
}

#[test]
fn referral_appears_in_load_context() {
    let (_base, proj_a, proj_b) = setup_two_projects();

    call_tool(
        "handoff_refer",
        json!({
            "project_dir": proj_a.to_string_lossy(),
            "target_project_dir": proj_b.to_string_lossy(),
            "summary": "Improve error messages",
            "referral_type": "improvement",
            "priority": "medium"
        }),
    );

    let resp = call_tool(
        "handoff_load_context",
        json!({ "project_dir": proj_b.to_string_lossy() }),
    );
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    assert!(parsed["referrals"].is_array());
    let referrals = parsed["referrals"].as_array().unwrap();
    assert_eq!(referrals.len(), 1);
    assert_eq!(referrals[0]["source_project"], "project-a");
    assert_eq!(referrals[0]["summary"], "Improve error messages");
    assert_eq!(referrals[0]["status"], "open");
}

#[test]
fn list_referrals_default() {
    let (_base, proj_a, proj_b) = setup_two_projects();

    call_tool(
        "handoff_refer",
        json!({
            "project_dir": proj_a.to_string_lossy(),
            "target_project_dir": proj_b.to_string_lossy(),
            "summary": "Referral 1",
            "referral_type": "bug"
        }),
    );
    call_tool(
        "handoff_refer",
        json!({
            "project_dir": proj_a.to_string_lossy(),
            "target_project_dir": proj_b.to_string_lossy(),
            "summary": "Referral 2",
            "referral_type": "request"
        }),
    );

    let resp = call_tool(
        "handoff_list_referrals",
        json!({ "project_dir": proj_b.to_string_lossy() }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed["total"], 2);
}

#[test]
fn list_referrals_with_filter() {
    let (_base, proj_a, proj_b) = setup_two_projects();

    call_tool(
        "handoff_refer",
        json!({
            "project_dir": proj_a.to_string_lossy(),
            "target_project_dir": proj_b.to_string_lossy(),
            "summary": "Open one",
            "referral_type": "bug"
        }),
    );

    let list_resp = call_tool(
        "handoff_list_referrals",
        json!({
            "project_dir": proj_b.to_string_lossy(),
            "status_filter": "acknowledged"
        }),
    );
    let text = get_text(&list_resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed["total"], 0);

    let list_resp2 = call_tool(
        "handoff_list_referrals",
        json!({
            "project_dir": proj_b.to_string_lossy(),
            "status_filter": "open"
        }),
    );
    let text2 = get_text(&list_resp2);
    let parsed2: Value = serde_json::from_str(&text2).unwrap();
    assert_eq!(parsed2["total"], 1);
}

#[test]
fn acknowledge_referral() {
    let (_base, proj_a, proj_b) = setup_two_projects();

    let send_resp = call_tool(
        "handoff_refer",
        json!({
            "project_dir": proj_a.to_string_lossy(),
            "target_project_dir": proj_b.to_string_lossy(),
            "summary": "Ack test",
            "referral_type": "request"
        }),
    );
    let send_text = get_text(&send_resp);
    let ref_id = send_text
        .lines()
        .next()
        .unwrap()
        .strip_prefix("Referral sent: ")
        .unwrap()
        .to_string();

    let resp = call_tool(
        "handoff_update_referral",
        json!({
            "project_dir": proj_b.to_string_lossy(),
            "referral_id": ref_id,
            "status": "acknowledged"
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    assert!(get_text(&resp).contains("acknowledged"));

    let list_resp = call_tool(
        "handoff_list_referrals",
        json!({
            "project_dir": proj_b.to_string_lossy(),
            "status_filter": "acknowledged"
        }),
    );
    let list_text = get_text(&list_resp);
    let parsed: Value = serde_json::from_str(&list_text).unwrap();
    assert_eq!(parsed["total"], 1);
}

#[test]
fn resolve_referral() {
    let (_base, proj_a, proj_b) = setup_two_projects();

    let send_resp = call_tool(
        "handoff_refer",
        json!({
            "project_dir": proj_a.to_string_lossy(),
            "target_project_dir": proj_b.to_string_lossy(),
            "summary": "Resolve test",
            "referral_type": "info"
        }),
    );
    let send_text = get_text(&send_resp);
    let ref_id = send_text
        .lines()
        .next()
        .unwrap()
        .strip_prefix("Referral sent: ")
        .unwrap()
        .to_string();

    call_tool(
        "handoff_update_referral",
        json!({
            "project_dir": proj_b.to_string_lossy(),
            "referral_id": &ref_id,
            "status": "acknowledged"
        }),
    );

    let resp = call_tool(
        "handoff_update_referral",
        json!({
            "project_dir": proj_b.to_string_lossy(),
            "referral_id": &ref_id,
            "status": "resolved"
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    assert!(get_text(&resp).contains("resolved"));
}

#[test]
fn resolved_referral_not_in_load_context() {
    let (_base, proj_a, proj_b) = setup_two_projects();

    let send_resp = call_tool(
        "handoff_refer",
        json!({
            "project_dir": proj_a.to_string_lossy(),
            "target_project_dir": proj_b.to_string_lossy(),
            "summary": "Will be resolved",
            "referral_type": "bug"
        }),
    );
    let send_text = get_text(&send_resp);
    let ref_id = send_text
        .lines()
        .next()
        .unwrap()
        .strip_prefix("Referral sent: ")
        .unwrap()
        .to_string();

    call_tool(
        "handoff_update_referral",
        json!({
            "project_dir": proj_b.to_string_lossy(),
            "referral_id": &ref_id,
            "status": "resolved"
        }),
    );

    let resp = call_tool(
        "handoff_load_context",
        json!({ "project_dir": proj_b.to_string_lossy() }),
    );
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    assert!(
        parsed.get("referrals").is_none() || parsed["referrals"].as_array().unwrap().is_empty(),
        "resolved referrals should not appear in load_context"
    );
}

#[test]
fn invalid_target_project_fails() {
    let (base, proj_a, _proj_b) = setup_two_projects();

    call_tool(
        "handoff_update_config",
        json!({
            "project_dir": proj_a.to_string_lossy(),
            "updates": {
                "dashboard.scan_dirs": [base.path().to_string_lossy()]
            }
        }),
    );

    let resp = call_tool(
        "handoff_refer",
        json!({
            "project_dir": proj_a.to_string_lossy(),
            "target_project": "nonexistent-project",
            "summary": "Should fail"
        }),
    );

    assert!(is_error(&resp), "should fail for nonexistent target");
}

#[test]
fn invalid_referral_type_fails() {
    let (_base, proj_a, proj_b) = setup_two_projects();

    let resp = call_tool(
        "handoff_refer",
        json!({
            "project_dir": proj_a.to_string_lossy(),
            "target_project_dir": proj_b.to_string_lossy(),
            "summary": "Bad type",
            "referral_type": "emergency"
        }),
    );

    assert!(is_error(&resp), "should reject invalid referral_type");
    assert!(get_text(&resp).contains("Invalid referral_type"));
}

#[test]
fn referral_validates_priority() {
    let (_base, proj_a, proj_b) = setup_two_projects();

    let resp = call_tool(
        "handoff_refer",
        json!({
            "project_dir": proj_a.to_string_lossy(),
            "target_project_dir": proj_b.to_string_lossy(),
            "summary": "Bad priority",
            "priority": "critical"
        }),
    );

    assert!(is_error(&resp), "should reject invalid priority");
    assert!(get_text(&resp).contains("Invalid priority"));
}

#[test]
fn no_referrals_dir_is_graceful() {
    let base = tempfile::tempdir().unwrap();
    let proj = setup_project(base.path(), "lonely");

    let resp = call_tool(
        "handoff_list_referrals",
        json!({ "project_dir": proj.to_string_lossy() }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed["total"], 0);
}

#[test]
fn refer_warns_on_missing_details() {
    let (_base, proj_a, proj_b) = setup_two_projects();

    let resp = call_tool(
        "handoff_refer",
        json!({
            "project_dir": proj_a.to_string_lossy(),
            "target_project_dir": proj_b.to_string_lossy(),
            "summary": "Minimal referral"
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(text.contains("Referral sent"));
    let warning_count = text.matches("Warning").count();
    assert!(
        warning_count >= 3,
        "should have at least 3 warnings (details, tasks, context), got {warning_count}: {text}"
    );
    assert!(
        text.contains("details"),
        "should warn about missing details: {text}"
    );
    assert!(
        text.contains("tasks"),
        "should warn about missing tasks: {text}"
    );
    assert!(
        text.contains("context"),
        "should warn about missing context: {text}"
    );
}

#[test]
fn refer_warns_on_tasks_without_done_criteria() {
    let (_base, proj_a, proj_b) = setup_two_projects();

    let resp = call_tool(
        "handoff_refer",
        json!({
            "project_dir": proj_a.to_string_lossy(),
            "target_project_dir": proj_b.to_string_lossy(),
            "summary": "Referral with bare tasks",
            "details": "Some details here",
            "priority": "high",
            "context": { "branch": "main" },
            "tasks": [
                { "title": "Task without criteria" },
                { "title": "Task with criteria", "done_criteria": [{"item": "check", "checked": false}] }
            ]
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(
        text.contains("Task #1 'Task without criteria' has no done_criteria"),
        "should warn about task without criteria: {text}"
    );
    assert!(
        !text.contains("Task #2"),
        "should NOT warn about task with criteria: {text}"
    );
}

#[test]
fn refer_no_warnings_when_complete() {
    let (_base, proj_a, proj_b) = setup_two_projects();

    let resp = call_tool(
        "handoff_refer",
        json!({
            "project_dir": proj_a.to_string_lossy(),
            "target_project_dir": proj_b.to_string_lossy(),
            "summary": "Complete referral",
            "referral_type": "request",
            "priority": "high",
            "details": "Full description of what needs to happen",
            "context": {
                "branch": "feat/x",
                "commit": "abc123",
                "spec_docs": [
                    format!("{}/.handoff/config.toml", proj_a.to_string_lossy()),
                    "https://gitlab.example.com/project/-/merge_requests/1"
                ]
            },
            "tasks": [
                {
                    "title": "Do the thing",
                    "done_criteria": [{"item": "Thing is done", "checked": false}]
                }
            ]
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(text.contains("Referral sent"));
    assert!(
        !text.contains("Warning"),
        "should have no warnings when fully specified: {text}"
    );
}

#[test]
fn refer_warns_on_context_without_spec_docs() {
    let (_base, proj_a, proj_b) = setup_two_projects();

    let resp = call_tool(
        "handoff_refer",
        json!({
            "project_dir": proj_a.to_string_lossy(),
            "target_project_dir": proj_b.to_string_lossy(),
            "summary": "Has context but no spec refs",
            "details": "Description",
            "priority": "medium",
            "context": { "branch": "main", "commit": "abc123" },
            "tasks": [
                { "title": "Task", "done_criteria": [{"item": "check", "checked": false}] }
            ]
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(
        text.contains("spec/doc references"),
        "should warn about missing spec references in context: {text}"
    );
}

#[test]
fn refer_warns_on_nonexistent_spec_path() {
    let (_base, proj_a, proj_b) = setup_two_projects();

    let resp = call_tool(
        "handoff_refer",
        json!({
            "project_dir": proj_a.to_string_lossy(),
            "target_project_dir": proj_b.to_string_lossy(),
            "summary": "Spec path does not exist",
            "details": "Description",
            "priority": "medium",
            "context": {
                "spec_docs": ["/nonexistent/path/to/spec.md"]
            },
            "tasks": [
                { "title": "Task", "done_criteria": [{"item": "check", "checked": false}] }
            ]
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(
        text.contains("does not exist"),
        "should warn about nonexistent spec path: {text}"
    );
}

fn ref_id_from_send(resp: &Value) -> String {
    get_text(resp)
        .lines()
        .next()
        .unwrap()
        .strip_prefix("Referral sent: ")
        .unwrap()
        .to_string()
}

#[test]
fn get_referral_returns_full_details() {
    let (_base, proj_a, proj_b) = setup_two_projects();

    let send_resp = call_tool(
        "handoff_refer",
        json!({
            "project_dir": proj_a.to_string_lossy(),
            "target_project_dir": proj_b.to_string_lossy(),
            "summary": "Add a knob",
            "details": "The full description with all the reasoning the receiver needs.",
            "referral_type": "improvement",
            "priority": "medium",
            "tasks": [
                {
                    "title": "Implement the knob",
                    "priority": "high",
                    "done_criteria": [
                        { "item": "design decided" },
                        { "item": "implemented" }
                    ]
                }
            ],
            "context": { "spec_docs": ["wiki/spec.md"] }
        }),
    );
    let ref_id = ref_id_from_send(&send_resp);

    let resp = call_tool(
        "handoff_get_referral",
        json!({
            "project_dir": proj_b.to_string_lossy(),
            "referral_id": ref_id,
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();

    assert_eq!(parsed["source_project"], "project-a");
    assert_eq!(parsed["summary"], "Add a knob");
    assert_eq!(
        parsed["details"],
        "The full description with all the reasoning the receiver needs."
    );
    assert_eq!(parsed["priority"], "medium");
    assert_eq!(parsed["status"], "open");
    assert_eq!(parsed["referral_type"], "improvement");

    let tasks = parsed["tasks"].as_array().expect("tasks should be array");
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["title"], "Implement the knob");
    let dc = tasks[0]["done_criteria"]
        .as_array()
        .expect("done_criteria array");
    assert_eq!(dc.len(), 2);
    assert_eq!(dc[0]["item"], "design decided");

    assert_eq!(parsed["context"]["spec_docs"][0], "wiki/spec.md");
}

#[test]
fn get_referral_works_for_acknowledged_status() {
    let (_base, proj_a, proj_b) = setup_two_projects();

    let send_resp = call_tool(
        "handoff_refer",
        json!({
            "project_dir": proj_a.to_string_lossy(),
            "target_project_dir": proj_b.to_string_lossy(),
            "summary": "Status changes should not hide details",
            "details": "Body text",
            "referral_type": "request"
        }),
    );
    let ref_id = ref_id_from_send(&send_resp);

    call_tool(
        "handoff_update_referral",
        json!({
            "project_dir": proj_b.to_string_lossy(),
            "referral_id": &ref_id,
            "status": "acknowledged"
        }),
    );

    let resp = call_tool(
        "handoff_get_referral",
        json!({
            "project_dir": proj_b.to_string_lossy(),
            "referral_id": &ref_id,
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(parsed["status"], "acknowledged");
    assert_eq!(parsed["details"], "Body text");
}

#[test]
fn get_referral_unknown_id_errors() {
    let (_base, _proj_a, proj_b) = setup_two_projects();

    let resp = call_tool(
        "handoff_get_referral",
        json!({
            "project_dir": proj_b.to_string_lossy(),
            "referral_id": "ref-does-not-exist"
        }),
    );

    assert!(is_error(&resp), "should error for unknown id");
    assert!(get_text(&resp).contains("not found"));
}
