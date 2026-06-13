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

#[test]
fn import_source_only() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_import_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "source": {
                "description": "test import",
                "format": "markdown"
            }
        }),
    );
    let text = get_text(&resp);
    assert!(text.contains("Import complete"));
    assert!(text.contains("Tasks created: 0"));
}

#[test]
fn import_flat_tasks() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_import_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "source": { "description": "flat task import" },
            "tasks": [
                { "title": "Task A", "status": "todo" },
                { "title": "Task B", "status": "in_progress", "priority": "high" }
            ]
        }),
    );
    let text = get_text(&resp);
    assert!(text.contains("Tasks created: 2"));
    assert!(text.contains("2 top-level"));
    assert!(text.contains("0 nested"));

    let list_resp = call_tool(
        "handoff_list_tasks",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    let list_text = get_text(&list_resp);
    assert!(list_text.contains("Task A"));
    assert!(list_text.contains("Task B"));
}

#[test]
fn import_nested_tasks() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_import_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "source": { "description": "nested import" },
            "tasks": [
                {
                    "title": "Parent",
                    "status": "in_progress",
                    "children": [
                        { "title": "Child 1", "status": "done" },
                        {
                            "title": "Child 2",
                            "status": "todo",
                            "children": [
                                { "title": "Grandchild", "status": "todo" }
                            ]
                        }
                    ]
                }
            ]
        }),
    );
    let text = get_text(&resp);
    assert!(text.contains("Tasks created: 4"));
    assert!(text.contains("1 top-level"));
    assert!(text.contains("3 nested"));

    let list_resp = call_tool(
        "handoff_list_tasks",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    let list_text = get_text(&list_resp);
    assert!(list_text.contains("Parent"));
    assert!(list_text.contains("Child 1"));
    assert!(list_text.contains("Child 2"));
    assert!(list_text.contains("Grandchild"));
}

#[test]
fn import_with_session() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_import_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "source": { "description": "session import" },
            "session": {
                "summary": "[import] test session",
                "decisions": [
                    { "decision": "Use OAuth2", "reason": "mobile support", "confidence": "confirmed" }
                ],
                "blockers": ["waiting on API key"],
                "handoff_notes": [
                    { "note": "check auth flow", "category": "caution" }
                ]
            }
        }),
    );
    let text = get_text(&resp);
    assert!(text.contains("Session saved: yes"));

    let load_resp = call_tool(
        "handoff_load_context",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    let load_text = get_text(&load_resp);
    assert!(load_text.contains("[import] test session"));
    assert!(load_text.contains("Use OAuth2"));
    assert!(load_text.contains("waiting on API key"));
}

#[test]
fn import_with_raw_notes() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_import_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "source": { "description": "raw notes import" },
            "session": {
                "summary": "[import] with raw notes"
            },
            "raw_notes": "Deploy on Fridays is forbidden\nSSL cert expires 7/1"
        }),
    );
    let text = get_text(&resp);
    assert!(text.contains("Raw notes: saved as handoff_note"));

    let load_resp = call_tool(
        "handoff_load_context",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    let load_text = get_text(&load_resp);
    assert!(load_text.contains("Deploy on Fridays is forbidden"));
}

#[test]
fn import_raw_notes_without_session_creates_auto_session() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_import_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "source": { "description": "auto session test" },
            "raw_notes": "Some unstructured notes"
        }),
    );
    let text = get_text(&resp);
    assert!(text.contains("Session saved: yes"));
    assert!(text.contains("Raw notes: saved as handoff_note"));

    let load_resp = call_tool(
        "handoff_load_context",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    let load_text = get_text(&load_resp);
    assert!(load_text.contains("[import] auto session test"));
    assert!(load_text.contains("Some unstructured notes"));
}

#[test]
fn import_full_scenario() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_import_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "source": {
                "description": "tmp/260601-sprint-handoff.md",
                "format": "markdown"
            },
            "tasks": [
                {
                    "title": "Auth renewal",
                    "status": "in_progress",
                    "priority": "high",
                    "labels": ["auth"],
                    "children": [
                        { "title": "Session migration", "status": "done" },
                        { "title": "PKCE implementation", "status": "in_progress" }
                    ]
                },
                {
                    "title": "CI speedup",
                    "status": "todo",
                    "priority": "medium",
                    "done_criteria": [
                        { "item": "Build time under 5min", "checked": false }
                    ]
                }
            ],
            "session": {
                "summary": "[import] Sprint handoff migration",
                "decisions": [
                    { "decision": "OAuth2 + PKCE", "confidence": "confirmed" }
                ],
                "blockers": ["DB migration window TBD"],
                "references": [
                    { "label": "Original doc", "uri": "tmp/260601-sprint-handoff.md", "type": "doc" }
                ],
                "context_pointers": [
                    { "path": "src/auth/oauth.rs", "reason": "PKCE core" }
                ]
            },
            "raw_notes": "Deploy on Fridays is forbidden"
        }),
    );
    let text = get_text(&resp);
    assert!(text.contains("Tasks created: 4"));
    assert!(text.contains("2 top-level"));
    assert!(text.contains("2 nested"));
    assert!(text.contains("Session saved: yes"));
    assert!(text.contains("Raw notes: saved as handoff_note"));
}

#[test]
fn import_without_source_fails() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_import_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "tasks": [{ "title": "Something" }]
        }),
    );
    let text = get_text(&resp);
    assert!(text.contains("Error"));
    assert!(text.contains("source"));
}

#[test]
fn import_task_without_title_fails() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_import_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "source": { "description": "bad task" },
            "tasks": [{ "status": "todo" }]
        }),
    );
    let text = get_text(&resp);
    assert!(text.contains("Error"));
    assert!(text.contains("title"));
}

#[test]
fn import_preserves_existing_tasks() {
    let dir = setup_project();

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "Pre-existing task", "status": "in_progress" }
        }),
    );

    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "source": { "description": "import after existing" },
            "tasks": [
                { "title": "Imported task", "status": "todo" }
            ]
        }),
    );

    let list_resp = call_tool(
        "handoff_list_tasks",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    let list_text = get_text(&list_resp);
    assert!(list_text.contains("Pre-existing task"));
    assert!(list_text.contains("Imported task"));
}

#[test]
fn import_source_recorded_in_environment() {
    let dir = setup_project();
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "source": {
                "description": "my-handoff.md",
                "format": "markdown"
            },
            "session": {
                "summary": "[import] env test"
            }
        }),
    );

    let sessions_dir = dir.path().join(".handoff/sessions");
    let entries: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".active.json"))
        .collect();
    assert_eq!(entries.len(), 1);

    let content = std::fs::read_to_string(entries[0].path()).unwrap();
    let session: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(
        session["environment"]["import_source"]["description"],
        "my-handoff.md"
    );
    assert_eq!(
        session["environment"]["import_source"]["format"],
        "markdown"
    );
}
