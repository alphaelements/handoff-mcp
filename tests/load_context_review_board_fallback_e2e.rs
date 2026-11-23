//! FR-RB-003: `handoff_load_context` `review_board` falls back to the count of
//! `review`-status tasks when there is no `review_board.md`, through the real
//! JSON-RPC dispatch.

use serde_json::{json, Value};

fn call(name: &str, arguments: Value) -> Value {
    let req = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": name, "arguments": arguments }
    });
    let out =
        handoff_mcp::mcp::protocol::process_line(&req.to_string()).expect("should return response");
    let resp: Value = serde_json::from_str(&out).expect("response should be valid JSON");
    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false) && resp.get("error").is_none(),
        "unexpected error: {resp}"
    );
    resp
}

fn payload(resp: &Value) -> Value {
    serde_json::from_str(resp["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

fn project() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_string_lossy().to_string();
    call(
        "handoff_init",
        json!({ "project_dir": path, "project_name": "rbfallback" }),
    );
    (dir, path)
}

fn add_task(path: &str, status: &str) {
    call(
        "handoff_update_task",
        json!({
            "project_dir": path,
            "task": { "title": "t", "status": status, "schedule": { "estimate_hours": 1.0 } }
        }),
    );
}

fn review_board(path: &str) -> Value {
    payload(&call(
        "handoff_load_context",
        json!({ "project_dir": path }),
    ))["review_board"]
        .clone()
}

#[test]
fn no_board_and_review_tasks_returns_the_fallback_count() {
    let (_d, path) = project();
    add_task(&path, "review");
    add_task(&path, "review");
    add_task(&path, "todo");
    let rb = review_board(&path);
    assert_eq!(rb["pending_review_tasks_count"], 2);
    assert_eq!(rb["source"], "task_fallback");
    assert!(rb["note"].as_str().unwrap().contains("review_board.md"));
}

#[test]
fn existing_board_keeps_the_normal_summary() {
    let (_d, path) = project();
    add_task(&path, "review");
    std::fs::write(
        std::path::Path::new(&path).join(".handoff/review_board.md"),
        "### A <!-- rb-id: R-001 -->\n- [ ] 確認済み\nメモ:\n",
    )
    .unwrap();
    let rb = review_board(&path);
    assert_eq!(rb["pending_count"], 1);
    assert!(rb.get("source").is_none(), "{rb}");
}

#[test]
fn no_board_and_no_review_tasks_is_null() {
    let (_d, path) = project();
    add_task(&path, "todo");
    assert!(review_board(&path).is_null());
}
