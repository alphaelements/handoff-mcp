use serde_json::{json, Value};
use std::fs;
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

fn get_text(resp: &Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"].as_bool().unwrap_or(false)
}

fn init_project(base: &std::path::Path, name: &str) {
    let project_dir = base.join(name);
    fs::create_dir_all(&project_dir).unwrap();

    let req = json!({
        "jsonrpc": "2.0", "id": 0,
        "method": "tools/call",
        "params": {
            "name": "handoff_init",
            "arguments": {
                "project_dir": project_dir.to_string_lossy(),
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
                "project_dir": project_dir.to_string_lossy(),
                "updates": { "settings.require_estimate_hours": false }
            }
        }
    });
    send(&cfg.to_string()).unwrap();
}

fn setup_scan_dir() -> TempDir {
    tempfile::tempdir().expect("failed to create temp dir")
}

#[test]
fn dashboard_multiple_projects() {
    let scan_dir = setup_scan_dir();

    init_project(scan_dir.path(), "project-a");
    init_project(scan_dir.path(), "project-b");

    let resp = call_tool(
        "handoff_dashboard",
        json!({ "scan_dirs": [scan_dir.path().to_string_lossy()] }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    let projects = parsed["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 2);

    let names: Vec<&str> = projects
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"project-a"));
    assert!(names.contains(&"project-b"));
}

#[test]
fn dashboard_skips_non_handoff_dirs() {
    let scan_dir = setup_scan_dir();

    init_project(scan_dir.path(), "real-project");
    fs::create_dir_all(scan_dir.path().join("not-a-project")).unwrap();

    let resp = call_tool(
        "handoff_dashboard",
        json!({ "scan_dirs": [scan_dir.path().to_string_lossy()] }),
    );

    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    let projects = parsed["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 1);
}

#[test]
fn dashboard_with_tasks() {
    let scan_dir = setup_scan_dir();
    init_project(scan_dir.path(), "proj-tasks");

    let pd = scan_dir
        .path()
        .join("proj-tasks")
        .to_string_lossy()
        .to_string();

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "title": "Task 1", "status": "in_progress" }
        }),
    );
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "title": "Task 2", "status": "blocked", "notes": "Waiting" }
        }),
    );

    let resp = call_tool(
        "handoff_dashboard",
        json!({ "scan_dirs": [scan_dir.path().to_string_lossy()] }),
    );

    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    assert_eq!(parsed["total_active_tasks"], 1);
    assert_eq!(parsed["total_blocked"], 1);

    let proj = &parsed["projects"][0];
    assert_eq!(proj["active_tasks"], 1);
    assert_eq!(proj["blocked_tasks"], 1);
}

#[test]
fn dashboard_empty_scan_dir() {
    let scan_dir = setup_scan_dir();

    let resp = call_tool(
        "handoff_dashboard",
        json!({ "scan_dirs": [scan_dir.path().to_string_lossy()] }),
    );

    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    assert!(parsed["projects"].as_array().unwrap().is_empty());
    assert_eq!(parsed["total_active_tasks"], 0);
}

#[test]
fn dashboard_nonexistent_scan_dir() {
    let resp = call_tool(
        "handoff_dashboard",
        json!({ "scan_dirs": ["/nonexistent/path/that/does/not/exist"] }),
    );

    assert!(!is_error(&resp));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    assert!(parsed["projects"].as_array().unwrap().is_empty());
}
