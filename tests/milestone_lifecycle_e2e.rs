//! REQ-MS-CFG-001 / REQ-MS-CFG-002: milestone schema extension (version,
//! status, order, depends_on) and the CRUD / task-assignment tools that use it.

use serde_json::{json, Value};
use std::fs;
use tempfile::TempDir;

fn send(input: &str) -> Option<Value> {
    let result = handoff_mcp::mcp::protocol::process_line(input)?;
    Some(serde_json::from_str(&result).expect("response should be valid JSON"))
}

fn setup_project() -> TempDir {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    call(
        &dir,
        "handoff_init",
        json!({ "project_name": "ms-project" }),
    );
    call(
        &dir,
        "handoff_update_config",
        json!({ "updates": { "settings.require_estimate_hours": false } }),
    );
    dir
}

fn call(dir: &TempDir, name: &str, mut arguments: Value) -> Value {
    arguments["project_dir"] = json!(dir.path().to_string_lossy());
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

fn list(dir: &TempDir, args: Value) -> Value {
    let resp = call(dir, "handoff_list_milestones", args);
    assert!(!is_error(&resp), "list error: {}", text(&resp));
    serde_json::from_str(&text(&resp)).unwrap()
}

fn add(dir: &TempDir, args: Value) -> Value {
    call(dir, "handoff_add_milestone", args)
}

fn set_status(dir: &TempDir, name: &str, status: &str, force: bool) -> Value {
    let mut args = json!({ "name": name, "status": status });
    if force {
        args["force"] = json!(true);
    }
    call(dir, "handoff_update_milestone", args)
}

// AC1 / AC7: version, status, order, depends_on are accepted, stored and listed.
#[test]
fn add_stores_and_list_returns_extended_fields() {
    let dir = setup_project();
    add(&dir, json!({ "name": "base" }));
    let resp = add(
        &dir,
        json!({
            "name": "v1", "date": "2026-11-01", "version": "1.0.0",
            "status": "active", "order": 3, "depends_on": ["base"]
        }),
    );
    assert!(!is_error(&resp), "add error: {}", text(&resp));
    assert!(
        !text(&resp).contains("Warning"),
        "known dependency must not warn: {}",
        text(&resp)
    );

    let v = list(&dir, json!({}));
    let m = &v["milestones"]["v1"];
    assert_eq!(m["version"], "1.0.0");
    assert_eq!(m["status"], "active");
    assert_eq!(m["order"], 3);
    assert_eq!(m["depends_on"], json!(["base"]));

    let toml = fs::read_to_string(dir.path().join(".handoff/config.toml")).unwrap();
    let cfg: toml::Value = toml::from_str(&toml).unwrap();
    assert_eq!(cfg["milestones"]["v1"]["order"].as_integer(), Some(3));
}

// AC5: a milestone with only date/color/description is unchanged; status defaults to planning.
#[test]
fn legacy_milestone_lists_with_defaults_and_is_not_rewritten() {
    let dir = setup_project();
    add(
        &dir,
        json!({ "name": "old", "date": "2026-08-01", "color": "#fff", "description": "d" }),
    );
    let toml = fs::read_to_string(dir.path().join(".handoff/config.toml")).unwrap();
    assert!(!toml.contains("status"), "no new keys written: {toml}");
    assert!(!toml.contains("depends_on"), "no new keys written: {toml}");

    let v = list(&dir, json!({}));
    let m = &v["milestones"]["old"];
    assert_eq!(m["date"], "2026-08-01");
    assert_eq!(m["status"], "planning");
    assert!(m["version"].is_null());
    assert!(m["order"].is_null());
    assert_eq!(m["depends_on"], json!([]));
}

// AC2: all five statuses accepted, anything else rejected (add and update).
#[test]
fn status_accepts_five_values_and_rejects_others() {
    let dir = setup_project();
    for s in ["planning", "active", "frozen", "released", "cancelled"] {
        let resp = add(&dir, json!({ "name": format!("m-{s}"), "status": s }));
        assert!(!is_error(&resp), "{s}: {}", text(&resp));
    }
    let resp = add(&dir, json!({ "name": "bad", "status": "done" }));
    assert!(is_error(&resp));
    assert!(
        text(&resp).contains("Invalid milestone status"),
        "{}",
        text(&resp)
    );
    assert!(list(&dir, json!({}))["milestones"]["bad"].is_null());

    let resp = set_status(&dir, "m-planning", "bogus", true);
    assert!(is_error(&resp), "force must not bypass value validation");
}

// AC3: order controls list output order; unordered milestones come last (by name).
#[test]
fn list_is_sorted_by_order() {
    let dir = setup_project();
    add(&dir, json!({ "name": "z-late", "order": 20 }));
    add(&dir, json!({ "name": "a-none" }));
    add(&dir, json!({ "name": "m-early", "order": 1 }));
    add(&dir, json!({ "name": "b-none" }));
    let v = list(&dir, json!({}));
    let keys: Vec<&str> = v["milestones"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, vec!["m-early", "z-late", "a-none", "b-none"]);
}

// AC4: a depends_on naming an unknown milestone warns but still saves.
#[test]
fn unknown_dependency_warns_but_saves() {
    let dir = setup_project();
    let resp = add(&dir, json!({ "name": "v2", "depends_on": ["ghost"] }));
    assert!(!is_error(&resp), "{}", text(&resp));
    assert!(text(&resp).contains("Warning"), "{}", text(&resp));
    assert!(text(&resp).contains("ghost"), "{}", text(&resp));
    assert_eq!(
        list(&dir, json!({}))["milestones"]["v2"]["depends_on"],
        json!(["ghost"])
    );

    let resp = call(
        &dir,
        "handoff_update_milestone",
        json!({ "name": "v2", "depends_on": ["v2", "nope"] }),
    );
    assert!(!is_error(&resp));
    assert!(text(&resp).contains("nope"), "{}", text(&resp));
    assert!(text(&resp).contains("itself"), "{}", text(&resp));
}

// AC6 / AC8: lifecycle transitions.
#[test]
fn status_transitions_forward_only_unless_forced() {
    let dir = setup_project();
    add(&dir, json!({ "name": "v1" }));
    for s in ["active", "frozen", "released"] {
        let resp = set_status(&dir, "v1", s, false);
        assert!(!is_error(&resp), "{s}: {}", text(&resp));
        assert_eq!(list(&dir, json!({}))["milestones"]["v1"]["status"], s);
    }

    // backward is rejected and leaves the value untouched
    let resp = set_status(&dir, "v1", "active", false);
    assert!(is_error(&resp));
    assert!(text(&resp).contains("force"), "{}", text(&resp));
    assert_eq!(
        list(&dir, json!({}))["milestones"]["v1"]["status"],
        "released"
    );

    // force overrides
    let resp = set_status(&dir, "v1", "active", true);
    assert!(!is_error(&resp), "{}", text(&resp));
    assert_eq!(
        list(&dir, json!({}))["milestones"]["v1"]["status"],
        "active"
    );

    // cancelled from any status, but leaving cancelled is a backward move
    let resp = set_status(&dir, "v1", "cancelled", false);
    assert!(!is_error(&resp), "{}", text(&resp));
    assert!(is_error(&set_status(&dir, "v1", "planning", false)));
    assert!(!is_error(&set_status(&dir, "v1", "planning", true)));
}

#[test]
fn legacy_milestone_without_status_is_planning_for_transitions() {
    let dir = setup_project();
    add(&dir, json!({ "name": "old" }));
    assert!(!is_error(&set_status(&dir, "old", "planning", false)));
    assert!(!is_error(&set_status(&dir, "old", "frozen", false)));
}

#[test]
fn update_null_clears_extended_fields() {
    let dir = setup_project();
    add(
        &dir,
        json!({ "name": "v1", "order": 2, "version": "1.0", "depends_on": ["x"] }),
    );
    call(
        &dir,
        "handoff_update_milestone",
        json!({ "name": "v1", "order": null, "version": null, "depends_on": null }),
    );
    let m = &list(&dir, json!({}))["milestones"]["v1"];
    assert!(m["order"].is_null());
    assert!(m["version"].is_null());
    assert_eq!(m["depends_on"], json!([]));
}

// AC10
#[test]
fn list_status_filter() {
    let dir = setup_project();
    add(&dir, json!({ "name": "a", "status": "active" }));
    add(&dir, json!({ "name": "p" }));
    add(&dir, json!({ "name": "f", "status": "frozen" }));

    let v = list(&dir, json!({ "status_filter": "active" }));
    let keys: Vec<&String> = v["milestones"].as_object().unwrap().keys().collect();
    assert_eq!(keys, vec!["a"]);

    let v = list(&dir, json!({ "status_filter": "planning" }));
    let keys: Vec<&String> = v["milestones"].as_object().unwrap().keys().collect();
    assert_eq!(keys, vec!["p"], "unset status counts as planning");

    let resp = call(
        &dir,
        "handoff_list_milestones",
        json!({ "status_filter": "nope" }),
    );
    assert!(is_error(&resp));
}

// AC11: assigning a task to a frozen milestone warns (never blocks).
#[test]
fn update_task_warns_on_frozen_milestone() {
    let dir = setup_project();
    add(&dir, json!({ "name": "frozen-ms", "status": "frozen" }));
    add(&dir, json!({ "name": "open-ms", "status": "active" }));

    let resp = call(
        &dir,
        "handoff_update_task",
        json!({ "task": { "title": "A", "schedule": { "milestone": "frozen-ms" } } }),
    );
    assert!(!is_error(&resp), "{}", text(&resp));
    assert!(text(&resp).contains("frozen"), "create: {}", text(&resp));

    let resp = call(
        &dir,
        "handoff_update_task",
        json!({ "task": { "title": "B", "schedule": { "milestone": "open-ms" } } }),
    );
    assert!(!text(&resp).contains("frozen"), "{}", text(&resp));

    // moving B into the frozen milestone warns; the write still lands
    let resp = call(
        &dir,
        "handoff_update_task",
        json!({ "task": { "id": "t2", "schedule": { "milestone": "frozen-ms" } } }),
    );
    assert!(!is_error(&resp), "{}", text(&resp));
    assert!(text(&resp).contains("frozen"), "update: {}", text(&resp));
    let got = call(&dir, "handoff_get_task", json!({ "task_id": "t2" }));
    assert!(text(&got).contains("frozen-ms"));

    // re-sending the same milestone is not "adding" the task: no warning
    let resp = call(
        &dir,
        "handoff_update_task",
        json!({ "task": { "id": "t2", "notes": "x", "schedule": { "milestone": "frozen-ms" } } }),
    );
    assert!(!text(&resp).contains("frozen"), "{}", text(&resp));
}

#[test]
fn bulk_update_warns_on_frozen_milestone() {
    let dir = setup_project();
    add(&dir, json!({ "name": "frozen-ms", "status": "frozen" }));
    call(
        &dir,
        "handoff_update_task",
        json!({ "task": { "title": "A" } }),
    );
    call(
        &dir,
        "handoff_update_task",
        json!({ "task": { "title": "B" } }),
    );

    let resp = call(
        &dir,
        "handoff_bulk_update_tasks",
        json!({ "updates": [
            { "task_id": "t1", "schedule": { "milestone": "frozen-ms" } },
            { "task_id": "t2", "schedule": { "milestone": "other" } }
        ]}),
    );
    assert!(!is_error(&resp), "{}", text(&resp));
    let v: Value = serde_json::from_str(&text(&resp)).unwrap();
    assert_eq!(v["applied"], 2);
    let warnings = v["warnings"].as_array().expect("warnings present");
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0]["task_id"], "t1");
    assert!(warnings[0]["warning"].as_str().unwrap().contains("frozen"));
}
