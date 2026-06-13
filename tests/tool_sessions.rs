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

#[test]
fn save_context_creates_session_file() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "Implemented feature X"
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(text.contains("Session saved"));

    let sessions_dir = dir.path().join(".handoff/sessions");
    let active_files: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".active.json"))
        .collect();
    assert_eq!(active_files.len(), 1);
}

#[test]
fn save_context_captures_git_state() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "Test session"
        }),
    );

    let sessions_dir = dir.path().join(".handoff/sessions");
    let active_file = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().ends_with(".active.json"))
        .unwrap();

    let content = std::fs::read_to_string(active_file.path()).unwrap();
    let session: Value = serde_json::from_str(&content).unwrap();

    assert!(session["branch"].is_string());
    assert!(session["commit"].is_string());
    assert!(session["ended_at"].is_string());
}

#[test]
fn save_context_closes_previous_active() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({ "project_dir": &pd, "summary": "First session" }),
    );

    let resp = call_tool(
        "handoff_save_context",
        json!({ "project_dir": &pd, "summary": "Second session" }),
    );

    let text = get_text(&resp);
    assert!(text.contains("Closed 1 previous"));

    let sessions_dir = dir.path().join(".handoff/sessions");
    let active: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".active.json"))
        .collect();
    let closed: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".closed.json"))
        .collect();

    assert_eq!(active.len(), 1);
    assert_eq!(closed.len(), 1);
}

#[test]
fn save_context_with_full_data() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "Full session",
            "decisions": [
                { "decision": "Use DMA", "reason": "Better throughput", "confidence": "confirmed" }
            ],
            "blockers": ["Waiting for hardware"],
            "checklist": [
                { "item": "Run smoke test", "checked": false, "owner": "ai" }
            ],
            "handoff_notes": [
                { "note": "Push after approval", "category": "caution" }
            ],
            "references": [
                { "label": "Design doc", "uri": "docs/design.md", "type": "doc" }
            ],
            "context_pointers": [
                { "path": "src/main.rs", "reason": "Entry point", "lines": "1-20" }
            ],
            "environment": { "fw_version": "1.0" }
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
}

#[test]
fn save_context_without_summary_fails() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp = call_tool("handoff_save_context", json!({ "project_dir": &pd }));

    assert!(is_error(&resp));
}

#[test]
fn load_context_uninitialized_returns_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let pd = dir.path().to_string_lossy().to_string();

    let resp = call_tool("handoff_load_context", json!({ "project_dir": &pd }));

    assert!(!is_error(&resp));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed["status"], "not_initialized");
}

#[test]
fn load_context_empty_project() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp = call_tool("handoff_load_context", json!({ "project_dir": &pd }));

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed["project"], "test");
    assert!(parsed["task_tree"].as_array().unwrap().is_empty());
    assert_eq!(parsed["task_summary"]["total"], 0);
}

#[test]
fn load_context_with_session_and_tasks() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "title": "Task 1", "status": "in_progress" }
        }),
    );

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "Did some work",
            "decisions": [
                { "decision": "Use approach A", "confidence": "confirmed" }
            ],
            "handoff_notes": [
                { "note": "Check tests", "category": "suggestion" }
            ]
        }),
    );

    let resp = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    assert_eq!(parsed["project"], "test");
    assert!(parsed["last_session"]["summary"]
        .as_str()
        .unwrap()
        .contains("Did some work"));
    assert_eq!(parsed["task_summary"]["total"], 1);
    assert!(!parsed["task_tree"].as_array().unwrap().is_empty());
    assert!(!parsed["decisions"].as_array().unwrap().is_empty());
    assert!(!parsed["handoff_notes"].as_array().unwrap().is_empty());
}

#[test]
fn full_session_lifecycle() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "title": "Feature X" }
        }),
    );

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "Session A: started feature X"
        }),
    );

    let resp = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    let text = get_text(&resp);
    let ctx: Value = serde_json::from_str(&text).unwrap();
    assert!(ctx["last_session"]["summary"]
        .as_str()
        .unwrap()
        .contains("Session A"));

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "t1", "title": "Feature X", "status": "done" }
        }),
    );

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "Session B: completed feature X"
        }),
    );

    let sessions_dir = dir.path().join(".handoff/sessions");
    let active: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".active.json"))
        .collect();
    let closed: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".closed.json"))
        .collect();

    assert_eq!(active.len(), 1);
    assert_eq!(closed.len(), 1);
}
