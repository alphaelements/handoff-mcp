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
    let closed_files: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".closed.json"))
        .collect();
    assert_eq!(
        closed_files.len(),
        1,
        "save_context without active session creates .closed.json"
    );

    let open_files: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".open.json"))
        .collect();
    assert_eq!(
        open_files.len(),
        0,
        "save_context should not create .open.json"
    );
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
    let closed_file = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().ends_with(".closed.json"))
        .unwrap();

    let content = std::fs::read_to_string(closed_file.path()).unwrap();
    let session: Value = serde_json::from_str(&content).unwrap();

    assert!(session["branch"].is_string());
    assert!(session["commit"].is_string());
    assert!(session["ended_at"].is_string());
}

#[test]
fn save_context_closes_previous_active() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create an open session via import, then activate it via load_context
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": &pd,
            "source": { "description": "setup" },
            "session": { "summary": "First session" }
        }),
    );
    call_tool("handoff_load_context", json!({ "project_dir": &pd }));

    // Save context — should update+close the active session
    let resp = call_tool(
        "handoff_save_context",
        json!({ "project_dir": &pd, "summary": "Second session" }),
    );

    let text = get_text(&resp);
    assert!(text.contains("Closed 1 previous session(s)"));

    let sessions_dir = dir.path().join(".handoff/sessions");
    let closed: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".closed.json"))
        .collect();

    assert_eq!(closed.len(), 1, "the active session should now be closed");
}

#[test]
fn save_context_preserves_open_sessions() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create open sessions via import (save_context no longer creates .open.json)
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": &pd,
            "source": { "description": "plan A" },
            "session": { "summary": "Plan A" },
            "skip_session_close": true
        }),
    );
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": &pd,
            "source": { "description": "plan B" },
            "session": { "summary": "Plan B" },
            "skip_session_close": true
        }),
    );

    // save_context should not affect these open sessions
    call_tool(
        "handoff_save_context",
        json!({ "project_dir": &pd, "summary": "closing work" }),
    );

    let sessions_dir = dir.path().join(".handoff/sessions");
    let open: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".open.json"))
        .collect();

    assert_eq!(
        open.len(),
        2,
        "open sessions should not be affected by save_context"
    );
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
    assert_eq!(parsed["task_summary"]["total"], 1);
    assert!(!parsed["task_tree"].as_array().unwrap().is_empty());

    let prev = &parsed["previous_session"];
    assert!(
        prev["summary"].as_str().unwrap().contains("Did some work"),
        "previous_session should contain the closed session summary"
    );
    assert!(
        !prev["decisions"].as_array().unwrap().is_empty(),
        "previous_session should contain decisions"
    );
    assert!(
        !prev["handoff_notes"].as_array().unwrap().is_empty(),
        "previous_session should contain handoff_notes"
    );
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

    // Create an open session via import, activate it, then close via save
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": &pd,
            "source": { "description": "plan" },
            "session": { "summary": "Session A: started feature X" }
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
    let open: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".open.json"))
        .collect();
    let closed: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".closed.json"))
        .collect();

    assert_eq!(open.len(), 0, "no open sessions after full lifecycle");
    assert_eq!(closed.len(), 1, "active session closed with handoff data");
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
                { "path": ".handoff/config.toml", "reason": "Entry point" }
            ],
            "decisions": [
                { "decision": "Use approach A", "confidence": "confirmed" }
            ],
            "references": [
                { "label": "Config", "uri": ".handoff/config.toml", "type": "doc" }
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

    let prev_notes = parsed["previous_session"]["handoff_notes"]
        .as_array()
        .unwrap();
    assert_eq!(
        prev_notes.len(),
        3,
        "previous_session handoff_notes still contains all notes"
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

    // Create an open session via import, then load it
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": &pd,
            "source": { "description": "setup" },
            "session": { "summary": "test session" }
        }),
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

    // Create an open session via import, activate it
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": &pd,
            "source": { "description": "setup" },
            "session": { "summary": "session one" }
        }),
    );
    let resp = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    let session_id = parsed["session_id"].as_str().unwrap().to_string();

    // Close the specific session by ID
    let resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "session two",
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

    // Create an open session via import
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": &pd,
            "source": { "description": "setup" },
            "session": { "summary": "first session" }
        }),
    );

    let resp1 = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    let text1 = get_text(&resp1);
    let parsed1: Value = serde_json::from_str(&text1).unwrap();

    assert!(parsed1["session_id"].is_string());
    let sid = parsed1["session_id"].as_str().unwrap();
    assert!(sid.starts_with("s-"));
}

#[test]
fn load_context_warns_on_unknown_session_id() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({ "project_dir": &pd, "summary": "some session" }),
    );

    let resp = call_tool(
        "handoff_load_context",
        json!({ "project_dir": &pd, "session_id": "s-99999999-999999-999999" }),
    );
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    assert!(
        parsed["warning"].is_string(),
        "should have a warning when session_id is not found: {text}"
    );
    assert!(
        parsed["warning"].as_str().unwrap().contains("not found"),
        "warning should mention 'not found': {}",
        parsed["warning"]
    );
}

#[test]
fn save_context_warns_on_unknown_close_session_id() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({ "project_dir": &pd, "summary": "some session" }),
    );

    let resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "new session",
            "close_session_id": "s-99999999-999999-999999"
        }),
    );

    let text = get_text(&resp);
    assert!(
        text.contains("not found"),
        "should warn about unknown close_session_id: {text}"
    );
}

// --- pause/resume session tests ---

#[test]
fn save_context_with_pause_session_id() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create open session via import, activate it
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": &pd,
            "source": { "description": "setup" },
            "session": { "summary": "session one" }
        }),
    );
    let resp = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    let session_id = parsed["session_id"].as_str().unwrap().to_string();

    let resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "session two (switching work)",
            "pause_session_id": session_id
        }),
    );

    let text = get_text(&resp);
    assert!(!is_error(&resp), "error: {text}");
    assert!(
        text.contains("Paused 1 session(s)"),
        "should report paused: {text}"
    );

    let sessions_dir = dir.path().join(".handoff/sessions");
    let paused: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".paused.json"))
        .collect();
    assert_eq!(paused.len(), 1, "should have 1 paused session");
}

#[test]
fn save_context_with_pause_active() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create open session via import, activate it
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": &pd,
            "source": { "description": "setup" },
            "session": { "summary": "session one" }
        }),
    );
    call_tool("handoff_load_context", json!({ "project_dir": &pd }));

    let resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "session two",
            "pause_active": true
        }),
    );

    let text = get_text(&resp);
    assert!(!is_error(&resp), "error: {text}");
    assert!(
        text.contains("Paused 1 session(s)"),
        "should report paused: {text}"
    );
}

#[test]
fn load_context_resumes_paused_session() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create open session via import, activate it
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": &pd,
            "source": { "description": "setup" },
            "session": {
                "summary": "original work",
                "handoff_notes": [{ "note": "Continue feature X", "category": "suggestion" }]
            }
        }),
    );

    let resp = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    let original_sid = parsed["session_id"].as_str().unwrap().to_string();

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "switching to urgent work",
            "pause_session_id": &original_sid
        }),
    );

    let resp = call_tool(
        "handoff_load_context",
        json!({ "project_dir": &pd, "session_id": &original_sid }),
    );
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    assert_eq!(
        parsed["session_id"].as_str().unwrap(),
        original_sid,
        "should resume the paused session"
    );
    assert!(
        parsed["last_session"]["summary"]
            .as_str()
            .unwrap()
            .contains("original work"),
        "should load the original session data"
    );
}

#[test]
fn load_context_shows_paused_sessions() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create open session via import, activate it, then pause
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": &pd,
            "source": { "description": "setup" },
            "session": { "summary": "work A" }
        }),
    );
    call_tool("handoff_load_context", json!({ "project_dir": &pd }));

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "work B",
            "pause_active": true
        }),
    );

    let resp = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    let paused = parsed["paused_sessions"]
        .as_array()
        .expect("should have paused_sessions");
    assert_eq!(paused.len(), 1);
    assert_eq!(paused[0]["summary"], "work A");
}

#[test]
fn save_context_pause_unknown_id_warns() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "new session",
            "pause_session_id": "s-99999999-999999-999999"
        }),
    );

    let text = get_text(&resp);
    assert!(
        text.contains("not found"),
        "should warn about unknown pause_session_id: {text}"
    );
}

#[test]
fn full_pause_resume_lifecycle() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create feature work session via import, activate it
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": &pd,
            "source": { "description": "feature" },
            "session": { "summary": "feature work" }
        }),
    );
    let resp = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    let feature_sid = parsed["session_id"].as_str().unwrap().to_string();

    // Pause feature work, create urgent fix session via import
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "urgent fix",
            "pause_session_id": &feature_sid
        }),
    );

    // Create urgent fix open session and load it
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": &pd,
            "source": { "description": "urgent" },
            "session": { "summary": "urgent fix" }
        }),
    );

    let resp2 = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    let text2 = get_text(&resp2);
    let parsed2: Value = serde_json::from_str(&text2).unwrap();
    assert!(
        parsed2["last_session"]["summary"]
            .as_str()
            .unwrap()
            .contains("urgent fix"),
        "should load the new session"
    );
    assert!(
        parsed2["paused_sessions"].as_array().unwrap().len() == 1,
        "should show 1 paused session"
    );

    let urgent_sid = parsed2["session_id"].as_str().unwrap().to_string();
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "urgent fix done",
            "close_session_id": &urgent_sid
        }),
    );

    let resp3 = call_tool(
        "handoff_load_context",
        json!({ "project_dir": &pd, "session_id": &feature_sid }),
    );
    let text3 = get_text(&resp3);
    let parsed3: Value = serde_json::from_str(&text3).unwrap();
    assert_eq!(
        parsed3["session_id"].as_str().unwrap(),
        feature_sid,
        "should resume the paused feature session"
    );
    assert!(
        parsed3
            .get("paused_sessions")
            .and_then(|v| v.as_array())
            .is_none()
            || parsed3["paused_sessions"].as_array().unwrap().is_empty(),
        "no more paused sessions after resume"
    );
}

// --- active session uniqueness tests ---

#[test]
fn load_context_rejects_activate_when_another_session_active() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create session A via import, activate it
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": &pd,
            "source": { "description": "A" },
            "session": { "summary": "session A" }
        }),
    );
    let resp = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    let sid_a = parsed["session_id"].as_str().unwrap().to_string();

    // Pause A, create session B via import
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "session B",
            "pause_session_id": &sid_a
        }),
    );
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": &pd,
            "source": { "description": "B" },
            "session": { "summary": "session B" },
            "skip_session_close": true
        }),
    );

    // Load B (activates it)
    let resp = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    assert!(!is_error(&resp));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    let sid_b = parsed["session_id"].as_str().unwrap().to_string();

    // Try to resume A while B is active — should fail
    let resp = call_tool(
        "handoff_load_context",
        json!({ "project_dir": &pd, "session_id": &sid_a }),
    );
    assert!(
        is_error(&resp),
        "should reject activating A while B is active: {}",
        get_text(&resp)
    );
    let text = get_text(&resp);
    assert!(
        text.contains("already active"),
        "error should mention active session: {text}"
    );
    assert!(
        text.contains(&sid_b),
        "error should mention the blocking session ID: {text}"
    );
}

#[test]
fn load_context_allows_reloading_already_active_session() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create open session via import, activate it
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": &pd,
            "source": { "description": "setup" },
            "session": { "summary": "session A" }
        }),
    );

    let resp = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    let sid = parsed["session_id"].as_str().unwrap().to_string();

    // Loading the same session again should succeed (idempotent)
    let resp2 = call_tool(
        "handoff_load_context",
        json!({ "project_dir": &pd, "session_id": &sid }),
    );
    assert!(
        !is_error(&resp2),
        "reloading the same active session should succeed: {}",
        get_text(&resp2)
    );
    let text2 = get_text(&resp2);
    let parsed2: Value = serde_json::from_str(&text2).unwrap();
    assert_eq!(parsed2["session_id"].as_str().unwrap(), sid);
}

#[test]
fn load_context_returns_active_session_without_open() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create open session via import, activate it
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": &pd,
            "source": { "description": "setup" },
            "session": { "summary": "my session" }
        }),
    );

    // First load activates the open session
    let resp = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    let sid = parsed["session_id"].as_str().unwrap().to_string();

    // Second load (no session_id) should return the active session
    let resp2 = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    assert!(!is_error(&resp2), "error: {}", get_text(&resp2));
    let text2 = get_text(&resp2);
    let parsed2: Value = serde_json::from_str(&text2).unwrap();
    assert_eq!(
        parsed2["session_id"].as_str().unwrap(),
        sid,
        "should return the same active session"
    );
}

// --- pause_only tests (Bug 1+2+5 fix) ---

#[test]
fn save_context_pause_only_does_not_create_session() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create open session via import, activate it
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": &pd,
            "source": { "description": "setup" },
            "session": { "summary": "session one" }
        }),
    );
    call_tool("handoff_load_context", json!({ "project_dir": &pd }));

    let sessions_dir = dir.path().join(".handoff/sessions");
    let count_before: usize = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .count();

    let resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "pause_active": true,
            "pause_only": true
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(text.contains("Paused 1 session(s)"));

    let count_after: usize = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .count();
    assert_eq!(
        count_before, count_after,
        "no new session file should be created"
    );
}

#[test]
fn save_context_pause_only_with_session_id_does_not_create_session() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create open session via import, activate it
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": &pd,
            "source": { "description": "setup" },
            "session": { "summary": "session one" }
        }),
    );
    let resp = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    let sid = parsed["session_id"].as_str().unwrap().to_string();

    let sessions_dir = dir.path().join(".handoff/sessions");
    let count_before: usize = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .count();

    let resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "pause_session_id": &sid,
            "pause_only": true
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(text.contains("Paused 1 session(s)"));

    let count_after: usize = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .count();
    assert_eq!(
        count_before, count_after,
        "no new session file should be created"
    );
}

#[test]
fn save_context_pause_only_without_summary_succeeds() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create open session via import, activate it
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": &pd,
            "source": { "description": "setup" },
            "session": { "summary": "session one" }
        }),
    );
    call_tool("handoff_load_context", json!({ "project_dir": &pd }));

    let resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "pause_active": true,
            "pause_only": true
        }),
    );

    assert!(
        !is_error(&resp),
        "pause_only should not require summary: {}",
        get_text(&resp)
    );
}

#[test]
fn save_context_without_pause_only_requires_summary() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp = call_tool("handoff_save_context", json!({ "project_dir": &pd }));
    assert!(is_error(&resp));
    let text = get_text(&resp);
    assert!(text.contains("summary"));
}

// --- pause_session_by_id with open sessions (Bug 3 fix) ---

#[test]
fn pause_session_by_id_pauses_open_session() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create open session via import
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": &pd,
            "source": { "description": "setup" },
            "session": { "summary": "open session" }
        }),
    );

    let sessions_dir = dir.path().join(".handoff/sessions");
    let open = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().ends_with(".open.json"))
        .unwrap();
    let content = std::fs::read_to_string(open.path()).unwrap();
    let session: Value = serde_json::from_str(&content).unwrap();
    let sid = session["id"].as_str().unwrap().to_string();

    let resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "pause_session_id": &sid,
            "pause_only": true
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(
        text.contains("Paused 1 session(s)"),
        "should pause the open session: {text}"
    );

    let paused: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".paused.json"))
        .collect();
    assert_eq!(paused.len(), 1, "open session should now be paused");
}

// --- import_context skip_session_close (Bug 4 fix) ---

#[test]
fn import_context_skip_session_close_preserves_active() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create open session via import, activate it
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": &pd,
            "source": { "description": "setup" },
            "session": { "summary": "active session" }
        }),
    );
    call_tool("handoff_load_context", json!({ "project_dir": &pd }));

    let resp = call_tool(
        "handoff_import_context",
        json!({
            "project_dir": &pd,
            "source": { "description": "test import" },
            "session": { "summary": "imported session" },
            "skip_session_close": true
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));

    let sessions_dir = dir.path().join(".handoff/sessions");
    let active: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".active.json"))
        .collect();
    assert_eq!(
        active.len(),
        1,
        "active session should be preserved when skip_session_close=true"
    );
}

#[test]
fn import_context_default_closes_active() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create open session via import, activate it
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": &pd,
            "source": { "description": "setup" },
            "session": { "summary": "active session" }
        }),
    );
    call_tool("handoff_load_context", json!({ "project_dir": &pd }));

    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": &pd,
            "source": { "description": "test import" },
            "session": { "summary": "imported session" }
        }),
    );

    let sessions_dir = dir.path().join(".handoff/sessions");
    let active: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".active.json"))
        .collect();
    assert_eq!(
        active.len(),
        0,
        "active session should be closed by default"
    );
}

// --- save_context no-proliferation tests ---

#[test]
fn save_context_updates_active_session_in_place() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create open via import, activate it
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": &pd,
            "source": { "description": "setup" },
            "session": { "summary": "initial plan" }
        }),
    );
    call_tool("handoff_load_context", json!({ "project_dir": &pd }));

    // Save context — should update the active session with handoff data and close it
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "completed work",
            "decisions": [{ "decision": "Used approach A", "confidence": "confirmed" }],
            "handoff_notes": [{ "note": "Push next", "category": "suggestion" }]
        }),
    );

    let sessions_dir = dir.path().join(".handoff/sessions");

    let closed: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".closed.json"))
        .collect();
    assert_eq!(closed.len(), 1, "active session should be closed");

    let content = std::fs::read_to_string(closed[0].path()).unwrap();
    let session: Value = serde_json::from_str(&content).unwrap();
    assert_eq!(session["summary"], "completed work");
    assert!(!session["decisions"].as_array().unwrap().is_empty());
    assert!(!session["handoff_notes"].as_array().unwrap().is_empty());
}

#[test]
fn load_context_includes_previous_session() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Save context to create a closed session with handoff data
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "previous work",
            "decisions": [{ "decision": "Use DMA", "confidence": "confirmed" }],
            "handoff_notes": [{ "note": "Continue with feature Y", "category": "suggestion" }],
            "context_pointers": [{ "path": "src/main.rs", "reason": "Entry point" }]
        }),
    );

    let resp = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    let prev = &parsed["previous_session"];
    assert!(prev.is_object(), "should have previous_session");
    assert_eq!(prev["summary"], "previous work");
    assert!(!prev["decisions"].as_array().unwrap().is_empty());
    assert!(!prev["handoff_notes"].as_array().unwrap().is_empty());
    assert!(!prev["context_pointers"].as_array().unwrap().is_empty());

    let next_actions = parsed["next_actions"].as_array().unwrap();
    assert_eq!(next_actions.len(), 1);
    assert_eq!(next_actions[0], "Continue with feature Y");
}

#[test]
fn load_context_returns_previous_session_when_no_open() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create a closed session via save_context
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "all done",
            "handoff_notes": [{ "note": "Nothing more to do", "category": "context" }]
        }),
    );

    // Load context with no open/active sessions
    let resp = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    // No session_id since nothing was activated
    assert!(
        parsed.get("session_id").is_none() || parsed["session_id"].is_null(),
        "should not have session_id when no open sessions"
    );

    // But previous_session should be present
    assert!(
        parsed["previous_session"].is_object(),
        "should have previous_session from closed"
    );
    assert_eq!(parsed["previous_session"]["summary"], "all done");
}

#[test]
fn save_context_session_status_active_creates_active_session() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "starting session",
            "session_status": "active"
        }),
    );

    let text = get_text(&resp);
    assert!(
        text.contains("Session kept active"),
        "should indicate session kept active: {text}"
    );

    let sessions_dir = dir.path().join(".handoff/sessions");
    let active_files: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".active.json"))
        .collect();
    assert_eq!(active_files.len(), 1, "should have one active session");

    let closed_files: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".closed.json"))
        .collect();
    assert!(
        closed_files.is_empty(),
        "should have no closed sessions after active save"
    );
}

#[test]
fn save_context_session_status_active_updates_existing_active_in_place() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp1 = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "initial session",
            "session_status": "active",
            "decisions": [{"decision": "chose Rust", "reason": "speed"}]
        }),
    );
    let text1 = get_text(&resp1);
    assert!(text1.contains("Session kept active"));

    let resp2 = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "updated session",
            "session_status": "active",
            "decisions": [{"decision": "chose Rust", "reason": "speed"}, {"decision": "added caching", "reason": "perf"}]
        }),
    );
    let text2 = get_text(&resp2);
    assert!(text2.contains("Session kept active"));

    let sessions_dir = dir.path().join(".handoff/sessions");
    let active_files: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".active.json"))
        .collect();
    assert_eq!(
        active_files.len(),
        1,
        "should still have exactly one active session"
    );

    let content = std::fs::read_to_string(active_files[0].path()).unwrap();
    assert!(
        content.contains("updated session"),
        "active session should have updated summary"
    );
    assert!(
        content.contains("added caching"),
        "active session should have updated decisions"
    );
}

#[test]
fn save_context_active_then_close_lifecycle() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "work in progress",
            "session_status": "active"
        }),
    );

    let resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "work completed",
            "handoff_notes": [{"note": "push the branch", "category": "suggestion"}],
            "context_pointers": [{"path": "src/main.rs"}],
            "decisions": [{"decision": "done"}],
            "references": [{"label": "MR", "uri": "https://example.com"}],
            "checklist": [{"item": "verified", "checked": true}]
        }),
    );
    let text = get_text(&resp);
    assert!(
        !text.contains("Session kept active"),
        "default should close the session"
    );

    let sessions_dir = dir.path().join(".handoff/sessions");
    let active_files: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".active.json"))
        .collect();
    assert!(
        active_files.is_empty(),
        "should have no active sessions after close"
    );

    let closed_files: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".closed.json"))
        .collect();
    assert_eq!(closed_files.len(), 1, "should have one closed session");
}

#[test]
fn load_context_returns_session_guidance_when_no_active_session() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    assert!(
        parsed.get("session_guidance").is_some(),
        "should include session_guidance when no active session: {text}"
    );
    let guidance = &parsed["session_guidance"];
    assert_eq!(guidance["action"], "create_session");
    assert!(guidance["message"]
        .as_str()
        .unwrap()
        .contains("No active session"),);
}

#[test]
fn load_context_no_session_guidance_when_active_session_exists() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "active session",
            "session_status": "active"
        }),
    );

    let resp = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    assert!(
        parsed.get("session_guidance").is_none(),
        "should NOT include session_guidance when active session exists: {text}"
    );
}

#[test]
fn load_context_session_guidance_includes_previous_session_fields() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "previous work",
            "decisions": [{"decision": "use tokio", "reason": "async"}],
            "context_pointers": [{"path": "src/lib.rs", "reason": "main module"}],
            "references": [{"label": "spec", "uri": "wiki/spec.md"}],
            "handoff_notes": [{"note": "next: implement parser", "category": "suggestion"}],
            "checklist": [{"item": "tests pass", "checked": true}]
        }),
    );

    let resp = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    let guidance = parsed
        .get("session_guidance")
        .expect("should have session_guidance");
    let suggested = guidance
        .get("suggested_fields")
        .expect("should have suggested_fields");

    assert!(
        suggested["summary"]
            .as_str()
            .unwrap()
            .contains("previous work"),
        "should suggest summary from previous session"
    );
    assert!(
        suggested.get("decisions").is_some(),
        "should include decisions from previous session"
    );
    assert!(
        suggested.get("context_pointers").is_some(),
        "should include context_pointers from previous session"
    );
    assert!(
        suggested.get("references").is_some(),
        "should include references from previous session"
    );
}

#[test]
fn e2e_session_guidance_then_establish_then_load_no_guidance() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "prior session",
            "decisions": [{"decision": "design A"}],
            "handoff_notes": [{"note": "implement feature X", "category": "suggestion"}],
            "context_pointers": [{"path": "src/main.rs"}],
            "references": [{"label": "doc", "uri": "wiki/doc.md"}],
            "checklist": [{"item": "done", "checked": true}]
        }),
    );

    let resp1 = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let text1 = get_text(&resp1);
    let parsed1: Value = serde_json::from_str(&text1).unwrap();
    assert!(
        parsed1.get("session_guidance").is_some(),
        "step1: should have session_guidance"
    );

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "Continuing: prior session",
            "session_status": "active",
            "decisions": [{"decision": "design A"}],
            "context_pointers": [{"path": "src/main.rs"}],
            "references": [{"label": "doc", "uri": "wiki/doc.md"}],
            "handoff_notes": [{"note": "implement feature X", "category": "suggestion"}],
            "checklist": [{"item": "established", "checked": true}]
        }),
    );

    let resp2 = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let text2 = get_text(&resp2);
    let parsed2: Value = serde_json::from_str(&text2).unwrap();
    assert!(
        parsed2.get("session_guidance").is_none(),
        "step3: should NOT have session_guidance after establishing session"
    );
    assert!(
        parsed2.get("session_id").is_some(),
        "should have session_id from active session"
    );

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "work completed",
            "handoff_notes": [{"note": "push branch", "category": "suggestion"}],
            "context_pointers": [{"path": "src/main.rs"}],
            "decisions": [{"decision": "completed"}],
            "references": [{"label": "MR", "uri": "https://example.com"}],
            "checklist": [{"item": "verified", "checked": true}]
        }),
    );

    let resp3 = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let text3 = get_text(&resp3);
    let parsed3: Value = serde_json::from_str(&text3).unwrap();
    assert!(
        parsed3.get("session_guidance").is_some(),
        "step5: should have session_guidance again after close"
    );
    assert!(
        parsed3.get("previous_session").is_some(),
        "should have previous_session from the closed session"
    );
    let prev = &parsed3["previous_session"];
    assert_eq!(
        prev["summary"].as_str().unwrap(),
        "work completed",
        "previous_session should be the most recent closed session"
    );
}

#[test]
fn load_context_session_guidance_includes_active_task_ids() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": {"title": "implement feature", "status": "in_progress", "priority": "high", "done_criteria": [{"item": "code written"}]}
        }),
    );
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": {"title": "done task", "status": "done", "priority": "low", "done_criteria": [{"item": "done", "checked": true}]}
        }),
    );
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": {"title": "blocked task", "status": "blocked", "priority": "medium", "done_criteria": [{"item": "unblocked"}]}
        }),
    );

    let resp = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    let guidance = parsed
        .get("session_guidance")
        .expect("should have guidance");
    let suggested = guidance.get("suggested_fields");

    assert!(
        suggested.is_none() || suggested.unwrap().get("related_task_ids").is_none(),
        "without previous_session, suggested_fields may not exist or have task_ids"
    );

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "prior session",
            "handoff_notes": [{"note": "continue", "category": "suggestion"}],
            "context_pointers": [{"path": "src/main.rs"}],
            "decisions": [{"decision": "x"}],
            "references": [{"label": "y", "uri": "z"}],
            "checklist": [{"item": "ok", "checked": true}]
        }),
    );

    let resp2 = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let text2 = get_text(&resp2);
    let parsed2: Value = serde_json::from_str(&text2).unwrap();

    let guidance2 = parsed2
        .get("session_guidance")
        .expect("should have guidance");
    let suggested2 = guidance2
        .get("suggested_fields")
        .expect("should have suggested_fields");
    let task_ids = suggested2
        .get("related_task_ids")
        .expect("should have related_task_ids");
    let ids: Vec<&str> = task_ids
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();

    assert!(ids.contains(&"t1"), "should include in_progress task t1");
    assert!(!ids.contains(&"t2"), "should NOT include done task t2");
    assert!(ids.contains(&"t3"), "should include blocked task t3");
}

#[test]
fn update_session_fails_without_active_session() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp = call_tool(
        "handoff_update_session",
        json!({"project_dir": &pd, "checklist_index": 0}),
    );
    assert!(is_error(&resp), "should fail without active session");
    let text = get_text(&resp);
    assert!(text.contains("No active session"), "error: {text}");
}

#[test]
fn update_session_toggle_checklist_item() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "session with checklist",
            "session_status": "active",
            "checklist": [
                {"item": "push branch", "checked": false, "owner": "user"},
                {"item": "run tests", "checked": false, "owner": "ai"}
            ],
            "handoff_notes": [{"note": "next", "category": "suggestion"}],
            "context_pointers": [{"path": "src/main.rs"}],
            "decisions": [{"decision": "x"}],
            "references": [{"label": "y", "uri": "z"}]
        }),
    );

    let resp = call_tool(
        "handoff_update_session",
        json!({"project_dir": &pd, "checklist_index": 1, "checklist_checked": true}),
    );
    let text = get_text(&resp);
    assert!(!is_error(&resp), "should succeed: {text}");
    assert!(
        text.contains("run tests"),
        "should mention toggled item: {text}"
    );
    assert!(text.contains("checked"), "should say checked: {text}");
    assert!(text.contains("1/2 checked"), "should show progress: {text}");

    let resp2 = call_tool(
        "handoff_update_session",
        json!({"project_dir": &pd, "checklist_index": 0}),
    );
    let text2 = get_text(&resp2);
    assert!(
        text2.contains("2/2 checked"),
        "both items should be checked: {text2}"
    );
}

#[test]
fn update_session_add_checklist_item() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "active session",
            "session_status": "active",
            "checklist": [{"item": "existing", "checked": true}],
            "handoff_notes": [{"note": "n", "category": "suggestion"}],
            "context_pointers": [{"path": "f"}],
            "decisions": [{"decision": "d"}],
            "references": [{"label": "r", "uri": "u"}]
        }),
    );

    let resp = call_tool(
        "handoff_update_session",
        json!({
            "project_dir": &pd,
            "add_checklist_item": "verify deployment",
            "checklist_owner": "user"
        }),
    );
    let text = get_text(&resp);
    assert!(
        text.contains("verify deployment"),
        "should mention new item: {text}"
    );
    assert!(
        text.contains("1/2 checked"),
        "should show 1 checked out of 2: {text}"
    );
}

#[test]
fn update_session_add_decision() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "active",
            "session_status": "active",
            "handoff_notes": [{"note": "n", "category": "suggestion"}],
            "context_pointers": [{"path": "f"}],
            "decisions": [{"decision": "initial"}],
            "references": [{"label": "r", "uri": "u"}],
            "checklist": [{"item": "ok", "checked": true}]
        }),
    );

    let resp = call_tool(
        "handoff_update_session",
        json!({
            "project_dir": &pd,
            "add_decision": {"decision": "use Redis", "reason": "caching", "confidence": "confirmed"}
        }),
    );
    let text = get_text(&resp);
    assert!(
        text.contains("use Redis"),
        "should mention decision: {text}"
    );
}

#[test]
fn update_session_checklist_index_out_of_range() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "active",
            "session_status": "active",
            "checklist": [{"item": "only item", "checked": false}],
            "handoff_notes": [{"note": "n", "category": "suggestion"}],
            "context_pointers": [{"path": "f"}],
            "decisions": [{"decision": "d"}],
            "references": [{"label": "r", "uri": "u"}]
        }),
    );

    let resp = call_tool(
        "handoff_update_session",
        json!({"project_dir": &pd, "checklist_index": 5}),
    );
    assert!(is_error(&resp), "should fail for out of range");
    let text = get_text(&resp);
    assert!(text.contains("out of range"), "error: {text}");
}

#[test]
fn update_session_no_updates_fails() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "active",
            "session_status": "active",
            "handoff_notes": [{"note": "n", "category": "suggestion"}],
            "context_pointers": [{"path": "f"}],
            "decisions": [{"decision": "d"}],
            "references": [{"label": "r", "uri": "u"}],
            "checklist": [{"item": "ok", "checked": true}]
        }),
    );

    let resp = call_tool("handoff_update_session", json!({"project_dir": &pd}));
    assert!(is_error(&resp), "should fail with no updates");
}

#[test]
fn update_session_persists_across_load_context() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "work session",
            "session_status": "active",
            "checklist": [
                {"item": "write code", "checked": false},
                {"item": "write tests", "checked": false}
            ],
            "handoff_notes": [{"note": "continue", "category": "suggestion"}],
            "context_pointers": [{"path": "src/main.rs"}],
            "decisions": [{"decision": "d"}],
            "references": [{"label": "r", "uri": "u"}]
        }),
    );

    call_tool(
        "handoff_update_session",
        json!({"project_dir": &pd, "checklist_index": 0}),
    );
    call_tool(
        "handoff_update_session",
        json!({
            "project_dir": &pd,
            "add_decision": {"decision": "switched to async", "confidence": "confirmed"}
        }),
    );

    let resp = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    let checklist = parsed["checklist"]
        .as_array()
        .expect("should have checklist");
    assert_eq!(checklist.len(), 2);
    assert_eq!(
        checklist[0]["checked"], true,
        "first item should be checked"
    );
    assert_eq!(
        checklist[1]["checked"], false,
        "second item should be unchecked"
    );

    let decisions = parsed["decisions"]
        .as_array()
        .expect("should have decisions");
    assert_eq!(decisions.len(), 2, "should have original + added decision");
    assert_eq!(decisions[1]["decision"], "switched to async");
}

#[test]
fn e2e_progressive_updates_full_lifecycle() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "implement feature X",
            "session_status": "active",
            "checklist": [
                {"item": "implement handler", "checked": false, "owner": "ai"},
                {"item": "add tests", "checked": false, "owner": "ai"},
                {"item": "run clippy", "checked": false, "owner": "ai"},
                {"item": "user approval", "checked": false, "owner": "user"}
            ],
            "handoff_notes": [{"note": "implement feature X", "category": "suggestion"}],
            "context_pointers": [{"path": "src/handler.rs"}],
            "decisions": [{"decision": "use pattern A"}],
            "references": [{"label": "spec", "uri": "wiki/spec.md"}]
        }),
    );

    call_tool(
        "handoff_update_session",
        json!({"project_dir": &pd, "checklist_index": 0}),
    );

    call_tool(
        "handoff_update_session",
        json!({"project_dir": &pd, "checklist_index": 1}),
    );

    call_tool(
        "handoff_update_session",
        json!({
            "project_dir": &pd,
            "checklist_index": 2,
            "add_decision": {"decision": "added error handling", "reason": "edge case found", "confidence": "confirmed"}
        }),
    );

    let resp = call_tool(
        "handoff_update_session",
        json!({"project_dir": &pd, "add_handoff_note": {"note": "ready for user review", "category": "context"}}),
    );
    let text = get_text(&resp);
    assert!(
        text.contains("3/4 checked"),
        "3 of 4 should be checked: {text}"
    );

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "feature X implemented, awaiting user approval",
            "handoff_notes": [
                {"note": "implement feature X", "category": "suggestion"},
                {"note": "ready for user review", "category": "context"}
            ],
            "context_pointers": [{"path": "src/handler.rs"}],
            "decisions": [
                {"decision": "use pattern A"},
                {"decision": "added error handling", "reason": "edge case found", "confidence": "confirmed"}
            ],
            "references": [{"label": "spec", "uri": "wiki/spec.md"}],
            "checklist": [
                {"item": "implement handler", "checked": true, "owner": "ai"},
                {"item": "add tests", "checked": true, "owner": "ai"},
                {"item": "run clippy", "checked": true, "owner": "ai"},
                {"item": "user approval", "checked": false, "owner": "user"}
            ]
        }),
    );

    let resp2 = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let text2 = get_text(&resp2);
    let parsed: Value = serde_json::from_str(&text2).unwrap();
    let prev = &parsed["previous_session"];
    assert_eq!(
        prev["summary"].as_str().unwrap(),
        "feature X implemented, awaiting user approval"
    );
}

#[test]
fn load_context_shows_open_sessions() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create an active session first so open sessions won't be auto-activated
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "active session",
            "session_status": "active",
            "handoff_notes": [{"category": "suggestion", "note": "next"}],
            "context_pointers": [{"path": "src/main.rs"}],
            "decisions": [{"decision": "d"}],
            "references": [{"label": "r", "uri": "https://example.com"}],
            "checklist": [{"item": "c", "checked": true}]
        }),
    );

    // Create an orphaned open session manually
    let sessions_dir = dir.path().join(".handoff/sessions");
    let open_session = serde_json::json!({
        "version": 2,
        "id": "s-20260620-100000-111111",
        "summary": "orphaned open session",
        "ended_at": "2026-06-20T10:00:00Z",
        "branch": "feat/something"
    });
    std::fs::write(
        sessions_dir.join("20260620-100000-orphaned.open.json"),
        serde_json::to_string_pretty(&open_session).unwrap(),
    )
    .unwrap();

    let resp = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    assert!(
        parsed.get("open_sessions").is_some(),
        "response should include open_sessions"
    );
    let open_list = parsed["open_sessions"].as_array().unwrap();
    assert_eq!(open_list.len(), 1);
    assert_eq!(
        open_list[0]["id"].as_str().unwrap(),
        "s-20260620-100000-111111"
    );
    assert_eq!(
        open_list[0]["summary"].as_str().unwrap(),
        "orphaned open session"
    );
}

#[test]
fn load_context_shows_open_sessions_alongside_active() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create an active session
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "active work",
            "session_status": "active",
            "handoff_notes": [{"category": "suggestion", "note": "keep going"}],
            "context_pointers": [{"path": "src/main.rs"}],
            "decisions": [{"decision": "test"}],
            "references": [{"label": "test", "uri": "https://example.com"}],
            "checklist": [{"item": "verify", "checked": true}]
        }),
    );

    // Create an orphaned open session manually
    let sessions_dir = dir.path().join(".handoff/sessions");
    let open_session = serde_json::json!({
        "version": 2,
        "id": "s-20260619-080000-999999",
        "summary": "ghost session",
        "ended_at": "2026-06-19T08:00:00Z",
        "branch": "old-branch"
    });
    std::fs::write(
        sessions_dir.join("20260619-080000-ghost.open.json"),
        serde_json::to_string_pretty(&open_session).unwrap(),
    )
    .unwrap();

    let resp = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    // Active session should be loaded
    assert!(parsed.get("session_id").is_some());

    // Open sessions should also be visible
    let open_list = parsed["open_sessions"].as_array().unwrap();
    assert_eq!(open_list.len(), 1);
    assert_eq!(
        open_list[0]["id"].as_str().unwrap(),
        "s-20260619-080000-999999"
    );
}

#[test]
fn close_session_by_id_prefix_match_via_save_context() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create an open session with a known ID
    let sessions_dir = dir.path().join(".handoff/sessions");
    let open_session = serde_json::json!({
        "version": 2,
        "id": "s-20260620-150000-654321",
        "summary": "session to close",
        "ended_at": "2026-06-20T15:00:00Z"
    });
    std::fs::write(
        sessions_dir.join("20260620-150000-to-close.open.json"),
        serde_json::to_string_pretty(&open_session).unwrap(),
    )
    .unwrap();

    // Close using prefix (without microseconds)
    let resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "closing ghost",
            "close_session_id": "s-20260620-150000",
            "handoff_notes": [{"category": "suggestion", "note": "next"}],
            "context_pointers": [{"path": "src/main.rs"}],
            "decisions": [{"decision": "d"}],
            "references": [{"label": "r", "uri": "https://example.com"}],
            "checklist": [{"item": "c", "checked": true}]
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(text.contains("Closed 1"), "should confirm closure: {text}");

    // Verify no more open sessions
    let resp2 = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let text2 = get_text(&resp2);
    let parsed: Value = serde_json::from_str(&text2).unwrap();
    assert!(
        parsed.get("open_sessions").is_none(),
        "no open sessions should remain"
    );
}
