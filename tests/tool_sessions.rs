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

    // Disable multi_session so exclusivity check applies
    call_tool(
        "handoff_update_config",
        json!({
            "project_dir": &pd,
            "updates": { "settings.multi_session": false }
        }),
    );

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

// --- Multi-session tests ---

fn setup_multi_session_project() -> TempDir {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();
    call_tool(
        "handoff_update_config",
        json!({
            "project_dir": &pd,
            "updates": { "settings.multi_session": true }
        }),
    );
    dir
}

#[test]
fn multi_session_allows_concurrent_active_sessions() {
    let dir = setup_multi_session_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create session A (active)
    let resp_a = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "session A",
            "session_status": "active",
            "label": "A",
            "timeline": "feature-x"
        }),
    );
    let sid_a = get_text(&resp_a)
        .lines()
        .find(|l| l.starts_with("Session ID:"))
        .unwrap()
        .trim_start_matches("Session ID: ")
        .trim()
        .to_string();

    // Pause A, create B as open via import, then load B (activates it)
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "pausing",
            "pause_session_id": &sid_a,
            "pause_only": true
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
    // Load activates the open session B
    let resp = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    assert!(!is_error(&resp), "error: {}", get_text(&resp));

    // Resume A while B is active — multi_session allows this
    let resp = call_tool(
        "handoff_load_context",
        json!({ "project_dir": &pd, "session_id": &sid_a }),
    );
    assert!(
        !is_error(&resp),
        "multi_session should allow concurrent active: {}",
        get_text(&resp)
    );

    // Verify we now have 2 active sessions
    let sessions_dir = dir.path().join(".handoff/sessions");
    let active_count = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".active.json"))
        .count();
    assert_eq!(active_count, 2, "should have 2 active sessions");
}

#[test]
fn multi_session_load_context_shows_select_session_guidance() {
    let dir = setup_multi_session_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create two active sessions
    let resp_a = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "session A",
            "session_status": "active",
            "label": "A"
        }),
    );
    let text_a = get_text(&resp_a);
    let sid_a = text_a
        .lines()
        .find(|l| l.starts_with("Session ID:"))
        .unwrap()
        .trim_start_matches("Session ID: ")
        .trim();

    // Pause A, create B, load (activates B)
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "pausing A",
            "pause_session_id": sid_a,
            "pause_only": true
        }),
    );
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "session B",
            "session_status": "active",
            "label": "B"
        }),
    );
    // Resume A (multi_session allows it)
    call_tool(
        "handoff_load_context",
        json!({ "project_dir": &pd, "session_id": sid_a }),
    );

    // Now load without session_id — should get select_session guidance
    let resp = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    assert!(
        parsed.get("active_sessions").is_some(),
        "should include active_sessions: {text}"
    );
    let guidance = &parsed["session_guidance"];
    assert_eq!(
        guidance["action"].as_str().unwrap(),
        "select_session",
        "guidance action should be select_session: {text}"
    );
}

#[test]
fn multi_session_save_context_with_session_id_targets_specific() {
    let dir = setup_multi_session_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create session A (active)
    let resp_a = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "session A",
            "session_status": "active"
        }),
    );
    let sid_a = get_text(&resp_a)
        .lines()
        .find(|l| l.starts_with("Session ID:"))
        .unwrap()
        .trim_start_matches("Session ID: ")
        .trim()
        .to_string();

    // Pause A, create B via import, load to activate B
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "pausing",
            "pause_session_id": &sid_a,
            "pause_only": true
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
    let resp_b = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    let text_b = get_text(&resp_b);
    let parsed_b: Value = serde_json::from_str(&text_b).unwrap();
    let sid_b = parsed_b["session_id"].as_str().unwrap().to_string();

    // Resume A (multi_session allows concurrent)
    call_tool(
        "handoff_load_context",
        json!({ "project_dir": &pd, "session_id": &sid_a }),
    );

    // Now we have 2 active sessions. Close A by session_id.
    let resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "closing A",
            "session_id": &sid_a
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));

    // Session B should still be active
    let sessions_dir = dir.path().join(".handoff/sessions");
    let active: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".active.json"))
        .collect();
    assert_eq!(active.len(), 1, "only session B should remain active");

    let content = std::fs::read_to_string(active[0].path()).unwrap();
    let data: Value = serde_json::from_str(&content).unwrap();
    assert_eq!(data["id"].as_str().unwrap(), sid_b);
}

#[test]
fn multi_session_update_session_with_session_id() {
    let dir = setup_multi_session_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create session A (active) with checklist
    let resp_a = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "session A",
            "session_status": "active",
            "checklist": [{"item": "item A", "checked": false}]
        }),
    );
    let sid_a = get_text(&resp_a)
        .lines()
        .find(|l| l.starts_with("Session ID:"))
        .unwrap()
        .trim_start_matches("Session ID: ")
        .trim()
        .to_string();

    // Pause A, create B via import, load B, resume A → 2 active
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "pausing",
            "pause_session_id": &sid_a,
            "pause_only": true
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
    call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    call_tool(
        "handoff_load_context",
        json!({ "project_dir": &pd, "session_id": &sid_a }),
    );

    // Update session A specifically by session_id
    let resp = call_tool(
        "handoff_update_session",
        json!({
            "project_dir": &pd,
            "session_id": &sid_a,
            "checklist_index": 0,
            "checklist_checked": true
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(
        text.contains("item A"),
        "should update session A's checklist: {text}"
    );
}

#[test]
fn save_context_with_timeline_and_label() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "session with metadata",
            "session_status": "active",
            "timeline": "feature-x",
            "label": "WT2作業",
            "related_task_ids": ["t1", "t2"]
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));

    // Verify the session file has the new fields
    let sessions_dir = dir.path().join(".handoff/sessions");
    let active: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".active.json"))
        .collect();
    assert_eq!(active.len(), 1);

    let content = std::fs::read_to_string(active[0].path()).unwrap();
    let data: Value = serde_json::from_str(&content).unwrap();
    assert_eq!(data["timeline"].as_str().unwrap(), "feature-x");
    assert_eq!(data["label"].as_str().unwrap(), "WT2作業");
    assert_eq!(data["related_task_ids"].as_array().unwrap().len(), 2);
}

#[test]
fn list_sessions_timeline_filter() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create session with timeline
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "feature work",
            "timeline": "feature-x"
        }),
    );
    // Create session without timeline
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "hotfix work"
        }),
    );

    // Filter by timeline
    let resp = call_tool(
        "handoff_list_sessions",
        json!({
            "project_dir": &pd,
            "timeline": "feature-x"
        }),
    );
    let text = get_text(&resp);
    let sessions: Vec<Value> = serde_json::from_str(&text).unwrap();
    assert_eq!(sessions.len(), 1, "should only return feature-x session");
    assert_eq!(sessions[0]["timeline"].as_str().unwrap(), "feature-x");
}

#[test]
fn list_sessions_shows_new_fields() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "session with fields",
            "timeline": "tl-1",
            "label": "my-label"
        }),
    );

    let resp = call_tool("handoff_list_sessions", json!({ "project_dir": &pd }));
    let text = get_text(&resp);
    let sessions: Vec<Value> = serde_json::from_str(&text).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["timeline"].as_str().unwrap(), "tl-1");
    assert_eq!(sessions[0]["label"].as_str().unwrap(), "my-label");
}

#[test]
fn multi_session_single_active_mode_still_rejects() {
    let dir = setup_project();
    let pd_setup = dir.path().to_string_lossy().to_string();
    call_tool(
        "handoff_update_config",
        json!({
            "project_dir": &pd_setup,
            "updates": { "settings.multi_session": false }
        }),
    );
    let pd = dir.path().to_string_lossy().to_string();

    // Create active session A
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "session A",
            "session_status": "active"
        }),
    );

    // Create and try to activate session B via import + load
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": &pd,
            "source": { "description": "B" },
            "session": { "summary": "session B" },
            "skip_session_close": true
        }),
    );

    let open = std::fs::read_dir(dir.path().join(".handoff/sessions"))
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().ends_with(".open.json"));

    if let Some(entry) = open {
        let content = std::fs::read_to_string(entry.path()).unwrap();
        let data: Value = serde_json::from_str(&content).unwrap();
        let open_sid = data["id"].as_str().unwrap().to_string();

        let resp = call_tool(
            "handoff_load_context",
            json!({ "project_dir": &pd, "session_id": &open_sid }),
        );
        assert!(
            is_error(&resp),
            "single-active mode should reject: {}",
            get_text(&resp)
        );
        let text = get_text(&resp);
        assert!(
            text.contains("already active") || text.contains("multi_session"),
            "error should mention exclusivity: {text}"
        );
    }
}

// ==========================================================================
// Phase 2: fork_session / merge_sessions / include_children tests
// ==========================================================================

#[test]
fn fork_session_creates_child_with_parent_id() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create source session
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "parent session",
            "session_status": "active",
            "decisions": [{"decision": "use DMA", "confidence": "confirmed"}],
            "context_pointers": [{"path": "src/main.rs", "reason": "entry"}],
            "references": [{"label": "spec", "uri": "wiki/spec.md"}],
            "handoff_notes": [{"note": "continue work", "category": "suggestion"}]
        }),
    );
    let resp = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let parent_sid = parsed["session_id"].as_str().unwrap().to_string();

    // Fork
    let resp = call_tool(
        "handoff_fork_session",
        json!({
            "project_dir": &pd,
            "source_session_id": &parent_sid,
            "summary": "exploring alternative API design"
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(text.contains("Forked session"), "output: {text}");
    assert!(text.contains(&parent_sid), "should mention parent: {text}");

    let fork_json: Value = text
        .split("\n\n")
        .last()
        .and_then(|s| serde_json::from_str(s).ok())
        .expect("should contain JSON output");
    assert_eq!(fork_json["parent_session_id"], parent_sid);
    assert_eq!(fork_json["status"], "active");
    let forked_sid = fork_json["session_id"].as_str().unwrap().to_string();

    // Verify the forked session file exists and has parent_session_id
    let resp = call_tool(
        "handoff_get_session",
        json!({"project_dir": &pd, "session_id": &forked_sid}),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let session: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(
        session["parent_session_id"].as_str().unwrap(),
        parent_sid,
        "forked session should have parent_session_id"
    );
}

#[test]
fn fork_session_inherits_decisions_by_default() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "parent session",
            "session_status": "active",
            "decisions": [
                {"decision": "use DMA", "confidence": "confirmed"},
                {"decision": "use async", "confidence": "estimated"}
            ],
            "context_pointers": [{"path": "src/lib.rs", "reason": "core"}],
            "references": [{"label": "doc", "uri": "doc.md"}]
        }),
    );
    let resp = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let parent_sid = parsed["session_id"].as_str().unwrap().to_string();

    let resp = call_tool(
        "handoff_fork_session",
        json!({
            "project_dir": &pd,
            "source_session_id": &parent_sid,
            "summary": "alternative approach"
        }),
    );
    let text = get_text(&resp);
    let fork_json: Value = text
        .split("\n\n")
        .last()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap();
    let forked_sid = fork_json["session_id"].as_str().unwrap().to_string();

    let resp = call_tool(
        "handoff_get_session",
        json!({"project_dir": &pd, "session_id": &forked_sid}),
    );
    let session: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(
        session["decisions"].as_array().unwrap().len(),
        2,
        "should inherit 2 decisions"
    );
    assert_eq!(
        session["context_pointers"].as_array().unwrap().len(),
        1,
        "should inherit context_pointers"
    );
    assert_eq!(
        session["references"].as_array().unwrap().len(),
        1,
        "should inherit references"
    );
}

#[test]
fn fork_session_custom_inherit() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "parent",
            "session_status": "active",
            "decisions": [{"decision": "use DMA"}],
            "context_pointers": [{"path": "src/main.rs"}],
            "references": [{"label": "ref", "uri": "ref.md"}],
            "handoff_notes": [{"note": "note", "category": "context"}],
            "blockers": ["blocker-1"]
        }),
    );
    let resp = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let parent_sid = parsed["session_id"].as_str().unwrap().to_string();

    // Only inherit decisions
    let resp = call_tool(
        "handoff_fork_session",
        json!({
            "project_dir": &pd,
            "source_session_id": &parent_sid,
            "summary": "minimal fork",
            "inherit": ["decisions"]
        }),
    );
    let text = get_text(&resp);
    let fork_json: Value = text
        .split("\n\n")
        .last()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap();
    let forked_sid = fork_json["session_id"].as_str().unwrap().to_string();

    let resp = call_tool(
        "handoff_get_session",
        json!({"project_dir": &pd, "session_id": &forked_sid}),
    );
    let session: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(session["decisions"].as_array().unwrap().len(), 1);
    assert!(
        session["context_pointers"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true),
        "should NOT inherit context_pointers"
    );
    assert!(
        session["references"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true),
        "should NOT inherit references"
    );
}

#[test]
fn fork_session_with_timeline_and_label() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "parent",
            "session_status": "active",
            "timeline": "feature-x"
        }),
    );
    let resp = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let parent_sid = parsed["session_id"].as_str().unwrap().to_string();

    let resp = call_tool(
        "handoff_fork_session",
        json!({
            "project_dir": &pd,
            "source_session_id": &parent_sid,
            "summary": "API design branch",
            "label": "API設計",
            "timeline": "feature-x"
        }),
    );
    let text = get_text(&resp);
    let fork_json: Value = text
        .split("\n\n")
        .last()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap();
    let forked_sid = fork_json["session_id"].as_str().unwrap().to_string();

    let resp = call_tool(
        "handoff_get_session",
        json!({"project_dir": &pd, "session_id": &forked_sid}),
    );
    let session: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(session["timeline"].as_str().unwrap(), "feature-x");
    assert_eq!(session["label"].as_str().unwrap(), "API設計");
}

#[test]
fn fork_session_inherits_source_timeline_when_not_specified() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "parent",
            "session_status": "active",
            "timeline": "my-timeline"
        }),
    );
    let resp = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let parent_sid = parsed["session_id"].as_str().unwrap().to_string();

    let resp = call_tool(
        "handoff_fork_session",
        json!({
            "project_dir": &pd,
            "source_session_id": &parent_sid,
            "summary": "child without explicit timeline"
        }),
    );
    let text = get_text(&resp);
    let fork_json: Value = text
        .split("\n\n")
        .last()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap();
    let forked_sid = fork_json["session_id"].as_str().unwrap().to_string();

    let resp = call_tool(
        "handoff_get_session",
        json!({"project_dir": &pd, "session_id": &forked_sid}),
    );
    let session: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(
        session["timeline"].as_str().unwrap(),
        "my-timeline",
        "should inherit parent's timeline"
    );
}

#[test]
fn fork_session_source_not_found() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp = call_tool(
        "handoff_fork_session",
        json!({
            "project_dir": &pd,
            "source_session_id": "s-nonexistent",
            "summary": "fork from nothing"
        }),
    );
    assert!(is_error(&resp), "should fail: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(text.contains("not found"), "error: {text}");
}

#[test]
fn fork_session_with_related_task_ids() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "parent",
            "session_status": "active"
        }),
    );
    let resp = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let parent_sid = parsed["session_id"].as_str().unwrap().to_string();

    let resp = call_tool(
        "handoff_fork_session",
        json!({
            "project_dir": &pd,
            "source_session_id": &parent_sid,
            "summary": "focused on t1 and t2",
            "related_task_ids": ["t1", "t2"]
        }),
    );
    let text = get_text(&resp);
    let fork_json: Value = text
        .split("\n\n")
        .last()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap();
    let forked_sid = fork_json["session_id"].as_str().unwrap().to_string();

    let resp = call_tool(
        "handoff_get_session",
        json!({"project_dir": &pd, "session_id": &forked_sid}),
    );
    let session: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let task_ids: Vec<&str> = session["related_task_ids"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(task_ids, vec!["t1", "t2"]);
}

// --- merge_sessions tests ---

#[test]
fn merge_sessions_combines_decisions_and_notes() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create session A with decisions
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "session A",
            "session_status": "active",
            "decisions": [{"decision": "use DMA"}],
            "handoff_notes": [{"note": "note from A", "category": "context"}]
        }),
    );
    let resp = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let sid_a = parsed["session_id"].as_str().unwrap().to_string();

    // Fork session B from A (inherit defaults, then add new data)
    let resp = call_tool(
        "handoff_fork_session",
        json!({
            "project_dir": &pd,
            "source_session_id": &sid_a,
            "summary": "session B",
            "inherit": []
        }),
    );
    let text = get_text(&resp);
    let fork_json: Value = text
        .split("\n\n")
        .last()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap();
    let sid_b = fork_json["session_id"].as_str().unwrap().to_string();

    // Add decisions/notes to session B
    call_tool(
        "handoff_update_session",
        json!({
            "project_dir": &pd,
            "session_id": &sid_b,
            "add_decision": {"decision": "use async"}
        }),
    );
    call_tool(
        "handoff_update_session",
        json!({
            "project_dir": &pd,
            "session_id": &sid_b,
            "add_handoff_note": {"note": "note from B", "category": "suggestion"}
        }),
    );

    // Merge B into A
    let resp = call_tool(
        "handoff_merge_sessions",
        json!({
            "project_dir": &pd,
            "source_session_ids": [&sid_a, &sid_b],
            "target_session_id": &sid_a
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(text.contains("Merged 2 sessions"), "output: {text}");
    assert!(
        text.contains("1 decisions"),
        "should merge 1 decision: {text}"
    );
    assert!(text.contains("1 notes"), "should merge 1 note: {text}");

    // Verify target session has combined data
    let resp = call_tool(
        "handoff_get_session",
        json!({"project_dir": &pd, "session_id": &sid_a}),
    );
    let session: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(
        session["decisions"].as_array().unwrap().len(),
        2,
        "target should have 2 decisions"
    );
    assert_eq!(
        session["handoff_notes"].as_array().unwrap().len(),
        2,
        "target should have 2 notes"
    );
}

#[test]
fn merge_sessions_detects_duplicate_decisions_as_conflicts() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Session A with "use DMA"
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "session A",
            "session_status": "active",
            "decisions": [{"decision": "use DMA"}]
        }),
    );
    let resp = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let sid_a = parsed["session_id"].as_str().unwrap().to_string();

    // Fork B, inherit decisions (so it also has "use DMA"), then add the same decision again
    let resp = call_tool(
        "handoff_fork_session",
        json!({
            "project_dir": &pd,
            "source_session_id": &sid_a,
            "summary": "session B",
            "inherit": ["decisions"]
        }),
    );
    let text = get_text(&resp);
    let fork_json: Value = text
        .split("\n\n")
        .last()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap();
    let sid_b = fork_json["session_id"].as_str().unwrap().to_string();

    let resp = call_tool(
        "handoff_merge_sessions",
        json!({
            "project_dir": &pd,
            "source_session_ids": [&sid_a, &sid_b],
            "target_session_id": &sid_a
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(
        text.contains("decision_conflict"),
        "should report conflict: {text}"
    );
}

#[test]
fn merge_sessions_closes_non_target_sources() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "session A",
            "session_status": "active"
        }),
    );
    let resp = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let sid_a = parsed["session_id"].as_str().unwrap().to_string();

    // Fork B from A
    let resp = call_tool(
        "handoff_fork_session",
        json!({
            "project_dir": &pd,
            "source_session_id": &sid_a,
            "summary": "session B",
            "inherit": []
        }),
    );
    let text = get_text(&resp);
    let fork_json: Value = text
        .split("\n\n")
        .last()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap();
    let sid_b = fork_json["session_id"].as_str().unwrap().to_string();

    let resp = call_tool(
        "handoff_merge_sessions",
        json!({
            "project_dir": &pd,
            "source_session_ids": [&sid_a, &sid_b],
            "target_session_id": &sid_a,
            "close_sources": true
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(
        text.contains("Closed source sessions"),
        "should close B: {text}"
    );

    // Verify B is now closed
    let closed: Vec<_> = std::fs::read_dir(dir.path().join(".handoff/sessions"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".closed.json"))
        .collect();
    assert!(
        !closed.is_empty(),
        "source session B should be closed after merge"
    );
}

#[test]
fn merge_sessions_requires_at_least_two_sources() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp = call_tool(
        "handoff_merge_sessions",
        json!({
            "project_dir": &pd,
            "source_session_ids": ["s-only-one"],
            "target_session_id": "s-only-one"
        }),
    );
    assert!(is_error(&resp), "should fail: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(text.contains("at least 2"), "error: {text}");
}

#[test]
fn merge_sessions_target_must_be_in_sources() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp = call_tool(
        "handoff_merge_sessions",
        json!({
            "project_dir": &pd,
            "source_session_ids": ["s-a", "s-b"],
            "target_session_id": "s-c"
        }),
    );
    assert!(is_error(&resp), "should fail: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(text.contains("must be one of"), "error: {text}");
}

#[test]
fn merge_sessions_close_sources_false_keeps_them() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "session A",
            "session_status": "active"
        }),
    );
    let resp = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let sid_a = parsed["session_id"].as_str().unwrap().to_string();

    // Fork B from A
    let resp = call_tool(
        "handoff_fork_session",
        json!({
            "project_dir": &pd,
            "source_session_id": &sid_a,
            "summary": "session B",
            "inherit": []
        }),
    );
    let text = get_text(&resp);
    let fork_json: Value = text
        .split("\n\n")
        .last()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap();
    let sid_b = fork_json["session_id"].as_str().unwrap().to_string();

    let resp = call_tool(
        "handoff_merge_sessions",
        json!({
            "project_dir": &pd,
            "source_session_ids": [&sid_a, &sid_b],
            "target_session_id": &sid_a,
            "close_sources": false
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));

    // B should still be active
    let active_after: Vec<_> = std::fs::read_dir(dir.path().join(".handoff/sessions"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".active.json"))
        .collect();
    assert_eq!(
        active_after.len(),
        2,
        "both sessions should remain active when close_sources=false"
    );
}

// --- list_sessions include_children tests ---

#[test]
fn list_sessions_include_children_shows_parent_child_tree() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create parent session
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "parent session",
            "session_status": "active"
        }),
    );
    let resp = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let parent_sid = parsed["session_id"].as_str().unwrap().to_string();

    // Fork a child
    let resp = call_tool(
        "handoff_fork_session",
        json!({
            "project_dir": &pd,
            "source_session_id": &parent_sid,
            "summary": "child session",
            "label": "child-1"
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));

    // List with include_children
    let resp = call_tool(
        "handoff_list_sessions",
        json!({
            "project_dir": &pd,
            "include_children": true
        }),
    );
    let text = get_text(&resp);
    let sessions: Vec<Value> = serde_json::from_str(&text).unwrap();

    let parent = sessions
        .iter()
        .find(|s| s["id"].as_str().unwrap() == parent_sid);
    assert!(parent.is_some(), "should find parent session");

    let parent = parent.unwrap();
    let children = parent.get("children").and_then(|v| v.as_array());
    assert!(
        children.is_some(),
        "parent should have children field: {}",
        serde_json::to_string_pretty(parent).unwrap()
    );
    let children = children.unwrap();
    assert_eq!(children.len(), 1, "should have 1 child");
    assert_eq!(children[0]["summary"], "child session");
    assert_eq!(children[0]["label"], "child-1");
}

#[test]
fn list_sessions_without_include_children_has_no_children_field() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "parent session",
            "session_status": "active"
        }),
    );
    let resp = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let parent_sid = parsed["session_id"].as_str().unwrap().to_string();

    call_tool(
        "handoff_fork_session",
        json!({
            "project_dir": &pd,
            "source_session_id": &parent_sid,
            "summary": "child session"
        }),
    );

    let resp = call_tool("handoff_list_sessions", json!({"project_dir": &pd}));
    let text = get_text(&resp);
    let sessions: Vec<Value> = serde_json::from_str(&text).unwrap();

    for s in &sessions {
        assert!(
            s.get("children").is_none(),
            "should not have children field without include_children"
        );
    }
}

// --- Fork + Merge E2E lifecycle test ---

#[test]
fn fork_then_merge_lifecycle() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create parent session
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "main work",
            "session_status": "active",
            "decisions": [{"decision": "base decision"}],
            "handoff_notes": [{"note": "base note", "category": "context"}]
        }),
    );
    let resp = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let parent_sid = parsed["session_id"].as_str().unwrap().to_string();

    // Fork a child
    let resp = call_tool(
        "handoff_fork_session",
        json!({
            "project_dir": &pd,
            "source_session_id": &parent_sid,
            "summary": "exploring alternative"
        }),
    );
    let text = get_text(&resp);
    let fork_json: Value = text
        .split("\n\n")
        .last()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap();
    let child_sid = fork_json["session_id"].as_str().unwrap().to_string();

    // Add new decision to child via update_session
    call_tool(
        "handoff_update_session",
        json!({
            "project_dir": &pd,
            "session_id": &child_sid,
            "add_decision": {"decision": "child decision", "confidence": "estimated"}
        }),
    );

    // Merge child back into parent
    let resp = call_tool(
        "handoff_merge_sessions",
        json!({
            "project_dir": &pd,
            "source_session_ids": [&parent_sid, &child_sid],
            "target_session_id": &parent_sid,
            "close_sources": true
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(
        text.contains("1 decisions"),
        "should merge child decision: {text}"
    );

    // Verify parent now has both decisions
    let resp = call_tool(
        "handoff_get_session",
        json!({"project_dir": &pd, "session_id": &parent_sid}),
    );
    let session: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let decisions = session["decisions"].as_array().unwrap();
    assert_eq!(
        decisions.len(),
        2,
        "parent should have 2 decisions after merge"
    );

    let decision_texts: Vec<&str> = decisions
        .iter()
        .filter_map(|d| d.get("decision").and_then(|v| v.as_str()))
        .collect();
    assert!(decision_texts.contains(&"base decision"));
    assert!(decision_texts.contains(&"child decision"));
}

#[test]
fn fork_then_immediate_close_survives_history_limit() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Set history_limit to 5
    call_tool(
        "handoff_update_config",
        json!({
            "project_dir": &pd,
            "updates": { "settings.session_history_limit": 5 }
        }),
    );

    // Create 5 closed sessions to fill the limit
    for i in 0..5 {
        call_tool(
            "handoff_save_context",
            json!({
                "project_dir": &pd,
                "summary": format!("old session {i}")
            }),
        );
    }

    // Create an active parent session
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "parent session",
            "session_status": "active"
        }),
    );
    let resp = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let parent_sid = parsed["session_id"].as_str().unwrap().to_string();

    // Fork and immediately close the forked session
    let resp = call_tool(
        "handoff_fork_session",
        json!({
            "project_dir": &pd,
            "source_session_id": &parent_sid,
            "summary": "forked session"
        }),
    );
    let text = get_text(&resp);
    let fork_json: Value = text
        .split("\n\n")
        .last()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap();
    let forked_sid = fork_json["session_id"].as_str().unwrap().to_string();

    let close_resp = call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "close_session_id": &forked_sid,
            "summary": "closing forked session"
        }),
    );
    let close_text = get_text(&close_resp);
    assert!(!is_error(&close_resp), "close failed: {}", close_text);

    // The forked session must still be retrievable
    let resp = call_tool(
        "handoff_get_session",
        json!({"project_dir": &pd, "session_id": &forked_sid}),
    );
    assert!(
        !is_error(&resp),
        "forked session was deleted by enforce_history_limit: {}",
        get_text(&resp)
    );

    // Verify the closed file doesn't start with 00000000
    let sessions_dir = dir.path().join(".handoff/sessions");
    let closed_files: Vec<String> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains("forked") && n.ends_with(".closed.json"))
        .collect();
    assert_eq!(
        closed_files.len(),
        1,
        "expected exactly one forked closed session"
    );
    assert!(
        !closed_files[0].starts_with("00000000"),
        "closed file should not start with 00000000: {}",
        closed_files[0]
    );
}

// --- t115: truncate must not panic on multibyte UTF-8 ---

#[test]
fn update_session_add_handoff_note_with_long_japanese_text() {
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

    // This Japanese text is 61 bytes in UTF-8, with byte 60 landing mid-character.
    // Before the fix, this would panic in truncate().
    let long_ja = "aあいうえおかきくけこさしすせそたちつての";
    let resp = call_tool(
        "handoff_update_session",
        json!({
            "project_dir": &pd,
            "add_handoff_note": {"note": long_ja, "category": "caution"}
        }),
    );
    assert!(!is_error(&resp), "should not error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(
        text.contains("added handoff_note"),
        "should confirm note added: {text}"
    );
}

#[test]
fn update_session_add_handoff_note_with_pure_japanese_over_60_chars() {
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

    // 70 hiragana characters = 210 bytes, well over the 60-char truncation point
    let long_text = "あいうえおかきくけこさしすせそたちつてのはひふへほまみむめもあいうえおかきくけこさしすせそたちつてのはひふへほまみむめもあいうえおかきくけこ";
    let resp = call_tool(
        "handoff_update_session",
        json!({
            "project_dir": &pd,
            "add_handoff_note": {"note": long_text, "category": "context"}
        }),
    );
    assert!(!is_error(&resp), "should not error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(
        text.contains("added handoff_note"),
        "should confirm note added: {text}"
    );
    // Truncated text should end with "..."
    assert!(
        text.contains("..."),
        "long note should be truncated with ...: {text}"
    );
}

// --- t114: save_context must not overwrite accumulated fields ---

#[test]
fn save_context_active_preserves_accumulated_notes() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create active session with initial note
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "initial session",
            "session_status": "active",
            "handoff_notes": [{"note": "initial note", "category": "suggestion"}],
            "context_pointers": [{"path": "src/main.rs"}],
            "decisions": [{"decision": "use pattern A"}],
            "references": [{"label": "spec", "uri": "https://example.com"}],
            "checklist": [{"item": "step1", "checked": false}]
        }),
    );

    // Add a note via update_session
    call_tool(
        "handoff_update_session",
        json!({
            "project_dir": &pd,
            "add_handoff_note": {"note": "accumulated note", "category": "caution"}
        }),
    );

    // Add a decision via update_session
    call_tool(
        "handoff_update_session",
        json!({
            "project_dir": &pd,
            "add_decision": {"decision": "use Redis", "confidence": "confirmed"}
        }),
    );

    // Check a checklist item via update_session
    call_tool(
        "handoff_update_session",
        json!({"project_dir": &pd, "checklist_index": 0}),
    );

    // Now save_context with session_status=active but WITHOUT passing
    // handoff_notes, decisions, checklist, etc.
    // Before the fix, this would wipe all accumulated fields.
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "updated summary",
            "session_status": "active"
        }),
    );

    // Load and verify accumulated fields survived
    let resp = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    // handoff_notes should still have both notes
    let notes = parsed["handoff_notes"]
        .as_array()
        .expect("should have handoff_notes");
    assert!(
        notes.len() >= 2,
        "should preserve accumulated notes, got {}: {:?}",
        notes.len(),
        notes
    );
    let note_texts: Vec<&str> = notes
        .iter()
        .filter_map(|n| n.get("note").and_then(|v| v.as_str()))
        .collect();
    assert!(
        note_texts.contains(&"initial note"),
        "should preserve initial note: {note_texts:?}"
    );
    assert!(
        note_texts.contains(&"accumulated note"),
        "should preserve accumulated note: {note_texts:?}"
    );

    // decisions should still have both
    let decisions = parsed["decisions"]
        .as_array()
        .expect("should have decisions");
    assert!(
        decisions.len() >= 2,
        "should preserve accumulated decisions, got {}: {:?}",
        decisions.len(),
        decisions
    );

    // checklist item should still be checked
    let checklist = parsed["checklist"]
        .as_array()
        .expect("should have checklist");
    assert!(!checklist.is_empty(), "checklist should not be empty");
    assert_eq!(
        checklist[0]["checked"], true,
        "checklist item should remain checked"
    );
}

#[test]
fn save_context_active_explicit_fields_override() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create active session with initial data
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "initial",
            "session_status": "active",
            "handoff_notes": [{"note": "old note", "category": "suggestion"}],
            "context_pointers": [{"path": "old.rs"}],
            "decisions": [{"decision": "old decision"}],
            "references": [{"label": "old", "uri": "https://old.com"}],
            "checklist": [{"item": "old", "checked": false}]
        }),
    );

    // Save with explicit new values — these SHOULD replace
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": &pd,
            "summary": "updated",
            "session_status": "active",
            "handoff_notes": [{"note": "new note", "category": "suggestion"}],
            "decisions": [{"decision": "new decision"}],
            "checklist": [{"item": "new", "checked": true}]
        }),
    );

    let resp = call_tool("handoff_load_context", json!({"project_dir": &pd}));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    // Explicitly provided fields should be overwritten
    let notes = parsed["handoff_notes"]
        .as_array()
        .expect("should have handoff_notes");
    assert_eq!(notes.len(), 1, "should have exactly the new note");
    assert_eq!(notes[0]["note"], "new note");

    let decisions = parsed["decisions"]
        .as_array()
        .expect("should have decisions");
    assert_eq!(decisions.len(), 1, "should have exactly the new decision");
    assert_eq!(decisions[0]["decision"], "new decision");

    let checklist = parsed["checklist"]
        .as_array()
        .expect("should have checklist");
    assert_eq!(checklist.len(), 1);
    assert_eq!(checklist[0]["item"], "new");
}
