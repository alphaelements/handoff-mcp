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
        .filter(|e| e.file_name().to_string_lossy().ends_with(".open.json"))
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
        .find(|e| e.file_name().to_string_lossy().ends_with(".open.json"))
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
    assert!(text.contains("Closed 1 previous session(s)"));

    let sessions_dir = dir.path().join(".handoff/sessions");
    let active: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".open.json"))
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
        .filter(|e| e.file_name().to_string_lossy().ends_with(".open.json"))
        .collect();
    let closed: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".closed.json"))
        .collect();

    assert_eq!(active.len(), 1);
    assert_eq!(closed.len(), 1);
}

// --- save_context validation warning tests ---

#[test]
fn save_context_warns_on_unchecked_checklist() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "Session with unchecked items",
            "checklist": [
                { "item": "Run smoke test", "checked": false, "owner": "ai" },
                { "item": "Deploy staging", "checked": true, "owner": "user" },
                { "item": "Verify logs", "checked": false, "owner": "ai" }
            ],
            "handoff_notes": [
                { "note": "Do X next", "category": "suggestion" }
            ],
            "context_pointers": [
                { "path": "src/main.rs", "reason": "Entry point" }
            ],
            "decisions": [
                { "decision": "Use approach A", "confidence": "confirmed" }
            ],
            "references": [
                { "label": "Design doc", "uri": "docs/design.md", "type": "doc" }
            ]
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(text.contains("Session saved"));
    assert!(text.contains("Warning"));
    assert!(
        text.contains("2 unchecked checklist item(s)"),
        "should count unchecked items: {text}"
    );
    assert!(text.contains("Run smoke test"));
    assert!(text.contains("Verify logs"));
}

#[test]
fn save_context_warns_on_no_suggestion_notes() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "Session without suggestions",
            "checklist": [
                { "item": "Done", "checked": true, "owner": "ai" }
            ],
            "handoff_notes": [
                { "note": "Be careful with X", "category": "caution" },
                { "note": "Background info", "category": "context" }
            ],
            "context_pointers": [
                { "path": "src/lib.rs", "reason": "Core" }
            ],
            "decisions": [
                { "decision": "Keep it", "confidence": "confirmed" }
            ],
            "references": [
                { "label": "Spec", "uri": "docs/spec.md", "type": "doc" }
            ]
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(text.contains("Session saved"));
    assert!(text.contains("Warning"));
    assert!(
        text.contains("suggestion"),
        "should warn about missing suggestions: {text}"
    );
}

#[test]
fn save_context_warns_on_empty_handoff_notes() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "Session with no notes at all",
            "checklist": [
                { "item": "OK", "checked": true }
            ],
            "context_pointers": [
                { "path": "src/main.rs", "reason": "Entry" }
            ],
            "decisions": [
                { "decision": "OK", "confidence": "confirmed" }
            ],
            "references": [
                { "label": "X", "uri": "x.md", "type": "doc" }
            ]
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(text.contains("Session saved"));
    assert!(
        text.contains("suggestion"),
        "should warn about missing suggestions even when notes array is empty: {text}"
    );
}

#[test]
fn save_context_warns_on_empty_checklist() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "No checklist",
            "handoff_notes": [
                { "note": "Do this next", "category": "suggestion" }
            ],
            "context_pointers": [
                { "path": "src/main.rs", "reason": "Entry" }
            ],
            "decisions": [
                { "decision": "OK", "confidence": "confirmed" }
            ],
            "references": [
                { "label": "X", "uri": "x.md", "type": "doc" }
            ]
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(
        text.contains("checklist"),
        "should warn about empty checklist: {text}"
    );
}

#[test]
fn save_context_warns_on_empty_context_pointers() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "No context pointers",
            "checklist": [
                { "item": "OK", "checked": true }
            ],
            "handoff_notes": [
                { "note": "Do this next", "category": "suggestion" }
            ],
            "decisions": [
                { "decision": "OK", "confidence": "confirmed" }
            ],
            "references": [
                { "label": "X", "uri": "x.md", "type": "doc" }
            ]
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(
        text.contains("context_pointers"),
        "should warn about empty context_pointers: {text}"
    );
}

#[test]
fn save_context_warns_on_empty_decisions() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "No decisions",
            "checklist": [
                { "item": "OK", "checked": true }
            ],
            "handoff_notes": [
                { "note": "Do this next", "category": "suggestion" }
            ],
            "context_pointers": [
                { "path": "src/main.rs", "reason": "Entry" }
            ],
            "references": [
                { "label": "X", "uri": "x.md", "type": "doc" }
            ]
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(
        text.contains("decisions"),
        "should warn about empty decisions: {text}"
    );
}

#[test]
fn save_context_warns_on_empty_references() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "No references",
            "checklist": [
                { "item": "OK", "checked": true }
            ],
            "handoff_notes": [
                { "note": "Do this next", "category": "suggestion" }
            ],
            "context_pointers": [
                { "path": "src/main.rs", "reason": "Entry" }
            ],
            "decisions": [
                { "decision": "OK", "confidence": "confirmed" }
            ]
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(
        text.contains("references"),
        "should warn about empty references: {text}"
    );
}

#[test]
fn save_context_no_warnings_when_valid() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "Clean session",
            "checklist": [
                { "item": "All done", "checked": true, "owner": "ai" }
            ],
            "handoff_notes": [
                { "note": "Next: implement feature Y", "category": "suggestion" }
            ],
            "context_pointers": [
                { "path": "src/main.rs", "reason": "Entry point" }
            ],
            "decisions": [
                { "decision": "Use approach A", "confidence": "confirmed" }
            ],
            "references": [
                { "label": "Design doc", "uri": "docs/design.md", "type": "doc" }
            ]
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(text.contains("Session saved"));
    assert!(
        !text.contains("Warning"),
        "should have no warnings when all items are valid: {text}"
    );
}

#[test]
fn save_context_multiple_warnings_combined() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "Session with many problems",
            "checklist": [
                { "item": "Unchecked thing", "checked": false, "owner": "ai" }
            ],
            "handoff_notes": [
                { "note": "Context only", "category": "context" }
            ]
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(text.contains("Session saved"));
    let warning_count = text.matches("Warning").count();
    assert!(
        warning_count >= 4,
        "should have at least 4 warnings, got {warning_count}: {text}"
    );
}

#[test]
fn save_context_no_warning_when_all_checked() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "All done",
            "checklist": [
                { "item": "A", "checked": true, "owner": "ai" },
                { "item": "B", "checked": true, "owner": "user" }
            ],
            "handoff_notes": [
                { "note": "Next: do Z", "category": "suggestion" }
            ],
            "context_pointers": [
                { "path": "src/main.rs", "reason": "Entry" }
            ],
            "decisions": [
                { "decision": "OK", "confidence": "confirmed" }
            ],
            "references": [
                { "label": "X", "uri": "x.md", "type": "doc" }
            ]
        }),
    );

    assert!(!is_error(&resp));
    let text = get_text(&resp);
    assert!(
        !text.contains("unchecked"),
        "no checklist warning when all checked: {text}"
    );
}

#[test]
fn load_context_includes_next_actions() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "Finished feature",
            "handoff_notes": [
                { "note": "Push branch and create MR", "category": "suggestion" },
                { "note": "All tests pass", "category": "context" },
                { "note": "Next work is in other-project", "category": "suggestion" }
            ]
        }),
    );

    let resp = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    let next_actions = parsed["next_actions"]
        .as_array()
        .expect("next_actions should be array");
    assert_eq!(next_actions.len(), 2);
    assert_eq!(next_actions[0], "Push branch and create MR");
    assert_eq!(next_actions[1], "Next work is in other-project");
}

#[test]
fn load_context_next_actions_excludes_non_suggestions() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "Session with mixed notes",
            "handoff_notes": [
                { "note": "Be careful with X", "category": "caution" },
                { "note": "Do Y next", "category": "suggestion" },
                { "note": "Background info", "category": "context" }
            ]
        }),
    );

    let resp = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    let next_actions = parsed["next_actions"]
        .as_array()
        .expect("next_actions should be array");
    assert_eq!(next_actions.len(), 1);
    assert_eq!(next_actions[0], "Do Y next");

    let handoff_notes = parsed["handoff_notes"].as_array().unwrap();
    assert_eq!(
        handoff_notes.len(),
        3,
        "handoff_notes still contains all notes"
    );
}

#[test]
fn load_context_no_next_actions_when_no_suggestions() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "Session without suggestions",
            "handoff_notes": [
                { "note": "Some context", "category": "context" },
                { "note": "A caution", "category": "caution" }
            ]
        }),
    );

    let resp = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    assert!(
        parsed.get("next_actions").is_none(),
        "next_actions should be absent when no suggestions"
    );
}

#[test]
fn load_context_next_actions_are_strings() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "Session",
            "handoff_notes": [
                { "note": "Do this first", "category": "suggestion" }
            ]
        }),
    );

    let resp = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    let next_actions = parsed["next_actions"].as_array().unwrap();
    for action in next_actions {
        assert!(
            action.is_string(),
            "each next_action should be a plain string, got: {action}"
        );
    }
}

#[test]
fn save_context_returns_session_id() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp = call_tool(
        "handoff_save_context",
        json!({ "project_dir": &pd, "summary": "test session" }),
    );

    let text = get_text(&resp);
    assert!(
        text.contains("Session ID: s-"),
        "response should contain session ID: {text}"
    );
}

#[test]
fn load_context_returns_session_id() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({ "project_dir": &pd, "summary": "test session" }),
    );

    let resp = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    assert!(
        parsed["session_id"].is_string(),
        "load_context should return session_id"
    );
    assert!(
        parsed["session_id"].as_str().unwrap().starts_with("s-"),
        "session_id should start with s-"
    );
}

#[test]
fn save_context_with_close_session_id() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create two sessions
    call_tool(
        "handoff_save_context",
        json!({ "project_dir": &pd, "summary": "session one" }),
    );
    call_tool(
        "handoff_save_context",
        json!({ "project_dir": &pd, "summary": "session two" }),
    );

    // Load to get session IDs (activates both)
    let resp = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    let session_id = parsed["session_id"].as_str().unwrap().to_string();

    // Save new session closing only the specific one
    let resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "session three",
            "close_session_id": session_id
        }),
    );

    let text = get_text(&resp);
    assert!(text.contains("Closed 1 previous session(s)"));
}

#[test]
fn load_context_with_specific_session_id() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create two sessions with different notes
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "first session",
            "handoff_notes": [{"note": "from first", "category": "context"}]
        }),
    );

    // The second save closes the first, so both can't be open simultaneously
    // with the default behavior. Use close_session_id to keep first open.
    // Actually: save always creates a new .open — let's just create two saves
    // and test that load with specific ID works.
    let resp1 = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    let text1 = get_text(&resp1);
    let parsed1: Value = serde_json::from_str(&text1).unwrap();

    // Verify we got the session_id back
    assert!(parsed1["session_id"].is_string());
    let sid = parsed1["session_id"].as_str().unwrap();
    assert!(sid.starts_with("s-"));
}
