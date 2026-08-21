use serde_json::{json, Value};

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

#[test]
fn e2e_overview_and_reclaim_via_real_jsonrpc_dispatch() {
    let tmp = tempfile::tempdir().unwrap();
    let pd = tmp.path().to_string_lossy().to_string();

    let init = call_tool(
        "handoff_init",
        json!({"project_dir": &pd, "project_name": "e2e"}),
    );
    assert!(!is_error(&init), "{}", get_text(&init));

    let cfg = call_tool(
        "handoff_update_config",
        json!({"project_dir": &pd, "updates": { "settings.require_estimate_hours": false }}),
    );
    assert!(!is_error(&cfg), "{}", get_text(&cfg));

    let overview = call_tool("handoff_overview", json!({"project_dir": &pd}));
    assert!(!is_error(&overview), "{}", get_text(&overview));
    let parsed: Value = serde_json::from_str(&get_text(&overview)).unwrap();
    assert_eq!(parsed["summary"]["total_agents"], 0);

    // Create + claim a task, then reclaim it.
    let create = call_tool(
        "handoff_update_task",
        json!({"project_dir": &pd, "task": { "title": "T1" }}),
    );
    assert!(!is_error(&create), "{}", get_text(&create));
    let text = get_text(&create);
    let task_id = text
        .strip_prefix("Created task ")
        .and_then(|s| s.split(':').next())
        .unwrap()
        .to_string();

    let claim = call_tool(
        "handoff_claim_task",
        json!({"project_dir": &pd, "task_id": &task_id}),
    );
    assert!(!is_error(&claim), "{}", get_text(&claim));

    let overview2 = call_tool("handoff_overview", json!({"project_dir": &pd}));
    let parsed2: Value = serde_json::from_str(&get_text(&overview2)).unwrap();
    let matrix = parsed2["task_matrix"].as_array().unwrap();
    assert!(matrix
        .iter()
        .any(|t| t["task_id"] == task_id && t["status"] == "in_progress"));

    let reclaim = call_tool(
        "handoff_reclaim_task",
        json!({"project_dir": &pd, "task_id": &task_id, "reason": "e2e test"}),
    );
    assert!(!is_error(&reclaim), "{}", get_text(&reclaim));
    assert!(get_text(&reclaim).contains("reclaimed successfully"));

    let events = std::fs::read_to_string(tmp.path().join(".handoff/events.jsonl")).unwrap();
    assert!(events.lines().any(|l| l.contains("task.reclaimed")));
}
