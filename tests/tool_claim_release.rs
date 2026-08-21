use serde_json::{json, Value};
use tempfile::TempDir;

fn send(input: &str) -> Option<Value> {
    let result = handoff_mcp::mcp::protocol::process_line(input)?;
    Some(serde_json::from_str(&result).expect("response should be valid JSON"))
}

fn setup_project() -> TempDir {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
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
        .unwrap_or("")
        .to_string()
}

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"].as_bool().unwrap_or(false)
}

fn create_task(dir: &TempDir, title: &str) -> String {
    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": title }
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    // "Created task t1: <title> [todo]"
    text.split_whitespace()
        .nth(2)
        .unwrap()
        .trim_end_matches(':')
        .to_string()
}

#[test]
fn claim_task_via_tool_call_sets_lock() {
    let dir = setup_project();
    let task_id = create_task(&dir, "Claimable task");

    let resp = call_tool(
        "handoff_claim_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task_id": task_id,
            "session_id": "s-1"
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(parsed["lock"]["agent_id"], "unknown");
    assert_eq!(parsed["lock"]["session_id"], "s-1");

    let tasks_dir = dir.path().join(".handoff").join("tasks");
    let task_dir = handoff_mcp::storage::tasks::find_task_dir_by_id(&tasks_dir, &task_id)
        .unwrap()
        .unwrap();
    let (_, status) = handoff_mcp::storage::tasks::read_task(&task_dir)
        .unwrap()
        .unwrap();
    assert_eq!(status, "in_progress");
}

#[test]
fn claim_task_twice_returns_error() {
    let dir = setup_project();
    let task_id = create_task(&dir, "Double claim task");

    let first = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id }),
    );
    assert!(!is_error(&first), "error: {}", get_text(&first));

    let second = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id }),
    );
    assert!(is_error(&second), "expected error: {}", get_text(&second));
}

#[test]
fn release_task_via_tool_call_clears_lock_and_reverts_status() {
    let dir = setup_project();
    let task_id = create_task(&dir, "Release me");

    let claim = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id }),
    );
    assert!(!is_error(&claim), "error: {}", get_text(&claim));

    let release = call_tool(
        "handoff_release_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task_id": task_id,
            "reason": "pausing work"
        }),
    );
    assert!(!is_error(&release), "error: {}", get_text(&release));
    assert!(get_text(&release).contains("released successfully"));
    assert!(get_text(&release).contains("pausing work"));

    let list = call_tool(
        "handoff_get_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id }),
    );
    assert!(!is_error(&list), "error: {}", get_text(&list));
    let parsed: Value = serde_json::from_str(&get_text(&list)).unwrap();
    assert_eq!(parsed["status"], "todo");

    // handoff_get_task does not surface `lock` in its response shape, so
    // assert the on-disk lock state directly via the storage layer.
    let tasks_dir = dir.path().join(".handoff").join("tasks");
    let task_dir = handoff_mcp::storage::tasks::find_task_dir_by_id(&tasks_dir, &task_id)
        .unwrap()
        .unwrap();
    let (data, status) = handoff_mcp::storage::tasks::read_task(&task_dir)
        .unwrap()
        .unwrap();
    assert!(data.lock.is_none());
    assert_eq!(status, "todo");
}

#[test]
fn done_transition_clears_lock() {
    let dir = setup_project();
    let task_id = create_task(&dir, "Finish me");

    let claim = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id }),
    );
    assert!(!is_error(&claim), "error: {}", get_text(&claim));

    let done = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "id": task_id, "status": "done" }
        }),
    );
    assert!(!is_error(&done), "error: {}", get_text(&done));

    // handoff_get_task does not surface `lock` in its response shape, so
    // assert the on-disk lock state directly via the storage layer.
    let tasks_dir = dir.path().join(".handoff").join("tasks");
    let task_dir = handoff_mcp::storage::tasks::find_task_dir_by_id(&tasks_dir, &task_id)
        .unwrap()
        .unwrap();
    let (data, status) = handoff_mcp::storage::tasks::read_task(&task_dir)
        .unwrap()
        .unwrap();
    assert!(data.lock.is_none(), "lock should be cleared on done");
    assert_eq!(status, "done");
}
