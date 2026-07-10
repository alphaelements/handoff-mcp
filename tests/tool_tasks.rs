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

#[test]
fn create_top_level_task() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "First task" }
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(text.contains("t1"));
    assert!(text.contains("First task"));

    let tasks = std::fs::read_dir(dir.path().join(".handoff/tasks"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().unwrap().is_dir())
        .count();
    assert_eq!(tasks, 1);
}

#[test]
fn create_child_task() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "Parent" } }),
    );

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "title": "Child task" },
            "parent_id": "t1"
        }),
    );

    assert!(!is_error(&resp));
    let text = get_text(&resp);
    assert!(text.contains("t1.1"));
}

#[test]
fn update_existing_task() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "Original" } }),
    );

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": {
                "id": "t1",
                "title": "Updated title",
                "status": "in_progress",
                "notes": "Working on it"
            }
        }),
    );

    assert!(!is_error(&resp));
    let text = get_text(&resp);
    assert!(text.contains("Updated title"));
    assert!(text.contains("in_progress"));
}

#[test]
fn status_change_renames_file() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "Task" } }),
    );

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "t1", "title": "Task", "status": "in_progress" }
        }),
    );

    let task_dir = dir.path().join(".handoff/tasks");
    let found: Vec<String> = walkdir(task_dir.to_str().unwrap());
    assert!(
        found.iter().any(|f| f.contains("_task.in_progress.json")),
        "expected in_progress file, found: {found:?}"
    );
    assert!(
        !found.iter().any(|f| f.contains("_task.todo.json")),
        "old status file should be gone"
    );
}

fn walkdir(dir: &str) -> Vec<String> {
    let mut result = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                result.extend(walkdir(path.to_str().unwrap()));
            } else {
                result.push(path.to_string_lossy().to_string());
            }
        }
    }
    result
}

#[test]
fn done_with_unchecked_criteria_fails() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": {
                "title": "Task with criteria",
                "done_criteria": [
                    { "item": "test passes", "checked": false }
                ]
            }
        }),
    );

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "t1", "title": "Task with criteria", "status": "done" }
        }),
    );

    assert!(is_error(&resp), "should fail: {}", get_text(&resp));
    assert!(get_text(&resp).contains("done_criteria"));
}

#[test]
fn done_with_non_terminal_children_fails() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "Parent" } }),
    );

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "title": "Child in progress", "status": "in_progress" },
            "parent_id": "t1"
        }),
    );

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "t1", "title": "Parent", "status": "done" }
        }),
    );

    assert!(is_error(&resp));
}

#[test]
fn done_with_all_terminal_children_succeeds() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "Parent" } }),
    );

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "title": "Child done", "status": "done" },
            "parent_id": "t1"
        }),
    );

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "t1", "title": "Parent", "status": "done" }
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
}

#[test]
fn move_task_to_new_parent() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "Task A" } }),
    );
    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "Task B" } }),
    );

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "t1", "title": "Task A" },
            "move_to": "t2"
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    assert!(get_text(&resp).contains("Moved"));

    let t2_children: Vec<_> = std::fs::read_dir(
        dir.path()
            .join(".handoff/tasks")
            .read_dir()
            .unwrap()
            .find(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("t2-")
            })
            .unwrap()
            .unwrap()
            .path(),
    )
    .unwrap()
    .filter_map(|e| e.ok())
    .filter(|e| e.file_type().unwrap().is_dir())
    .collect();

    assert!(!t2_children.is_empty(), "t1 should be moved under t2");
}

#[test]
fn list_tasks_returns_tree() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "Task 1" } }),
    );
    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "Task 2", "status": "in_progress" } }),
    );

    let resp = call_tool("handoff_list_tasks", json!({ "project_dir": &pd }));

    assert!(!is_error(&resp));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    assert_eq!(parsed["task_summary"]["total"], 2);
    assert!(parsed["task_tree"].as_array().unwrap().len() >= 2);
}

#[test]
fn list_tasks_with_status_filter() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "Todo task" } }),
    );
    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "Active task", "status": "in_progress" } }),
    );

    let resp = call_tool(
        "handoff_list_tasks",
        json!({ "project_dir": &pd, "status_filter": "in_progress" }),
    );

    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    let tree = parsed["task_tree"].as_array().unwrap();

    assert!(tree.iter().all(|t| t["status"] == "in_progress"));
}

#[test]
fn list_tasks_uninitialized_project_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    let resp = call_tool(
        "handoff_list_tasks",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    assert!(is_error(&resp));
}

#[test]
fn hierarchical_id_numbering() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "T1" } }),
    );
    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "T2" } }),
    );
    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "T3" } }),
    );

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "title": "Child of T2" },
            "parent_id": "t2"
        }),
    );

    let text = get_text(&resp);
    assert!(text.contains("t2.1"), "should be t2.1, got: {text}");

    let resp2 = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "title": "Second child of T2" },
            "parent_id": "t2"
        }),
    );
    let text2 = get_text(&resp2);
    assert!(text2.contains("t2.2"), "should be t2.2, got: {text2}");
}

#[test]
fn invalid_priority_rejected_on_create() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "Bad priority", "priority": "critical" }
        }),
    );

    assert!(is_error(&resp), "should reject invalid priority");
    assert!(get_text(&resp).contains("Invalid priority"));
}

#[test]
fn valid_priority_accepted_on_create() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "Good priority", "priority": "high" }
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
}

#[test]
fn null_priority_accepted_on_create() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "No priority" }
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
}

#[test]
fn invalid_priority_rejected_on_update() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "Task" } }),
    );

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "t1", "title": "Task", "priority": "urgent" }
        }),
    );

    assert!(is_error(&resp), "should reject invalid priority on update");
    assert!(get_text(&resp).contains("Invalid priority"));
}

#[test]
fn update_without_title_works() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "Original title" } }),
    );

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "t1", "status": "in_progress" }
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(text.contains("Original title"));
    assert!(text.contains("in_progress"));
}

#[test]
fn create_without_title_fails() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "status": "todo" }
        }),
    );

    assert!(is_error(&resp), "should fail without title for new task");
    assert!(get_text(&resp).contains("title"));
}

// --- get_task tests ---

#[test]
fn get_task_returns_full_details() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": {
                "title": "Detailed task",
                "status": "in_progress",
                "notes": "Some important notes",
                "priority": "high",
                "labels": ["bug", "urgent"],
                "links": ["https://example.com/issue/1"],
                "done_criteria": [
                    { "item": "Fix the bug", "checked": false },
                    { "item": "Add tests", "checked": true }
                ]
            }
        }),
    );

    let resp = call_tool(
        "handoff_get_task",
        json!({ "project_dir": &pd, "task_id": "t1" }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();

    assert_eq!(parsed["id"], "t1");
    assert_eq!(parsed["title"], "Detailed task");
    assert_eq!(parsed["status"], "in_progress");
    assert_eq!(parsed["notes"], "Some important notes");
    assert_eq!(parsed["priority"], "high");
    assert_eq!(parsed["labels"].as_array().unwrap().len(), 2);
    assert_eq!(parsed["links"].as_array().unwrap().len(), 1);
    assert_eq!(parsed["done_criteria"].as_array().unwrap().len(), 2);
    assert!(parsed["created_at"].is_string());
}

#[test]
fn get_task_not_found() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_get_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": "t999" }),
    );
    assert!(is_error(&resp));
    assert!(get_text(&resp).contains("not found"));
}

#[test]
fn get_task_missing_id() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_get_task",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    assert!(is_error(&resp));
    assert!(get_text(&resp).contains("task_id"));
}

#[test]
fn get_task_nested() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "Parent" } }),
    );
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "title": "Nested child", "notes": "Deep detail" },
            "parent_id": "t1"
        }),
    );

    let resp = call_tool(
        "handoff_get_task",
        json!({ "project_dir": &pd, "task_id": "t1.1" }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(parsed["id"], "t1.1");
    assert_eq!(parsed["notes"], "Deep detail");
}

// --- check_criterion tests ---

#[test]
fn check_criterion_toggles_item() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": {
                "title": "Task with criteria",
                "done_criteria": [
                    { "item": "Step 1", "checked": false },
                    { "item": "Step 2", "checked": false },
                    { "item": "Step 3", "checked": false }
                ]
            }
        }),
    );

    let resp = call_tool(
        "handoff_check_criterion",
        json!({ "project_dir": &pd, "task_id": "t1", "criterion_index": 1, "checked": true }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();

    assert_eq!(parsed["task_id"], "t1");
    assert_eq!(parsed["criterion_index"], 1);
    assert_eq!(parsed["item"], "Step 2");
    assert_eq!(parsed["checked"], true);
    assert_eq!(parsed["done_criteria_summary"]["total"], 3);
    assert_eq!(parsed["done_criteria_summary"]["checked"], 1);
}

#[test]
fn check_criterion_uncheck() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": {
                "title": "Task",
                "done_criteria": [
                    { "item": "Step 1", "checked": true }
                ]
            }
        }),
    );

    let resp = call_tool(
        "handoff_check_criterion",
        json!({ "project_dir": &pd, "task_id": "t1", "criterion_index": 0, "checked": false }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(parsed["checked"], false);
    assert_eq!(parsed["done_criteria_summary"]["checked"], 0);
}

#[test]
fn check_criterion_out_of_range() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": {
                "title": "Task",
                "done_criteria": [{ "item": "Only one", "checked": false }]
            }
        }),
    );

    let resp = call_tool(
        "handoff_check_criterion",
        json!({ "project_dir": &pd, "task_id": "t1", "criterion_index": 5, "checked": true }),
    );

    assert!(is_error(&resp));
    assert!(get_text(&resp).contains("out of range"));
}

#[test]
fn check_criterion_task_not_found() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_check_criterion",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": "t999", "criterion_index": 0, "checked": true }),
    );
    assert!(is_error(&resp));
    assert!(get_text(&resp).contains("not found"));
}

#[test]
fn check_criterion_no_criteria() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "No criteria" } }),
    );

    let resp = call_tool(
        "handoff_check_criterion",
        json!({ "project_dir": &pd, "task_id": "t1", "criterion_index": 0, "checked": true }),
    );

    assert!(is_error(&resp));
    assert!(get_text(&resp).contains("out of range"));
}

#[test]
fn check_criterion_persists() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": {
                "title": "Persist test",
                "done_criteria": [
                    { "item": "A", "checked": false },
                    { "item": "B", "checked": false }
                ]
            }
        }),
    );

    call_tool(
        "handoff_check_criterion",
        json!({ "project_dir": &pd, "task_id": "t1", "criterion_index": 0, "checked": true }),
    );

    let resp = call_tool(
        "handoff_get_task",
        json!({ "project_dir": &pd, "task_id": "t1" }),
    );

    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let criteria = parsed["done_criteria"].as_array().unwrap();
    assert_eq!(criteria[0]["checked"], true);
    assert_eq!(criteria[1]["checked"], false);
}

// --- upsert tests ---

#[test]
fn upsert_creates_task_with_specified_id() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "t5", "title": "Upserted task", "status": "todo" }
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(text.contains("t5"), "should use specified id, got: {text}");
    assert!(text.contains("Upserted task"));

    let resp2 = call_tool(
        "handoff_get_task",
        json!({ "project_dir": &pd, "task_id": "t5" }),
    );
    assert!(!is_error(&resp2));
    let parsed: Value = serde_json::from_str(&get_text(&resp2)).unwrap();
    assert_eq!(parsed["id"], "t5");
    assert_eq!(parsed["title"], "Upserted task");
}

#[test]
fn upsert_with_dependencies_batch_create() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp1 = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "t10", "title": "First batch task" }
        }),
    );
    assert!(!is_error(&resp1), "error: {}", get_text(&resp1));

    let resp2 = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": {
                "id": "t11",
                "title": "Depends on t10",
                "dependencies": ["t10"]
            }
        }),
    );
    assert!(!is_error(&resp2), "error: {}", get_text(&resp2));

    let resp3 = call_tool(
        "handoff_get_task",
        json!({ "project_dir": &pd, "task_id": "t11" }),
    );
    let parsed: Value = serde_json::from_str(&get_text(&resp3)).unwrap();
    assert_eq!(parsed["dependencies"][0], "t10");
}

#[test]
fn upsert_existing_id_still_updates() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "Original" } }),
    );

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "t1", "title": "Updated via upsert" }
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(text.contains("Updated"));
    assert!(text.contains("Updated via upsert"));
}

#[test]
fn upsert_requires_title_for_new_task() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "id": "t99", "status": "todo" }
        }),
    );

    assert!(is_error(&resp), "should fail without title for new upsert");
    let text = get_text(&resp);
    assert!(text.contains("title"), "error should mention title: {text}");
}

// --- friendly error message tests ---

#[test]
fn update_nonexistent_task_error_has_hint() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "Existing" } }),
    );

    // With upsert, this will now create, but the old behavior hint
    // can be verified via get_task on a non-existent id
    let resp = call_tool(
        "handoff_get_task",
        json!({ "project_dir": &pd, "task_id": "t999" }),
    );
    assert!(is_error(&resp));
    let text = get_text(&resp);
    assert!(
        text.contains("handoff_list_tasks") || text.contains("Available"),
        "error should include guidance, got: {text}"
    );
}

#[test]
fn check_criterion_nonexistent_task_has_hint() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_check_criterion",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task_id": "t999",
            "criterion_index": 0,
            "checked": true
        }),
    );
    assert!(is_error(&resp));
    let text = get_text(&resp);
    assert!(
        text.contains("handoff_list_tasks")
            || text.contains("Available")
            || text.contains("No tasks exist"),
        "error should include guidance, got: {text}"
    );
}

#[test]
fn move_nonexistent_task_has_hint() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "Target" } }),
    );

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "t999" },
            "move_to": "t1"
        }),
    );
    assert!(is_error(&resp));
    let text = get_text(&resp);
    assert!(
        text.contains("handoff_list_tasks") || text.contains("Available"),
        "error should include guidance, got: {text}"
    );
}

#[test]
fn move_to_nonexistent_parent_has_hint() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "Source" } }),
    );

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "t1" },
            "move_to": "t999"
        }),
    );
    assert!(is_error(&resp));
    let text = get_text(&resp);
    assert!(
        text.contains("handoff_list_tasks") || text.contains("Available"),
        "error should include guidance, got: {text}"
    );
}

// --- estimate_hours requirement (referral ref-20260625-015320) ---

/// Re-enable the estimate requirement, which setup_project() disables by default.
fn enable_estimate_requirement(pd: &str) {
    let resp = call_tool(
        "handoff_update_config",
        json!({
            "project_dir": pd,
            "updates": { "settings.require_estimate_hours": true }
        }),
    );
    assert!(!is_error(&resp), "config error: {}", get_text(&resp));
}

#[test]
fn create_leaf_without_estimate_is_rejected() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();
    enable_estimate_requirement(&pd);

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "title": "No estimate" }
        }),
    );

    assert!(is_error(&resp), "should reject missing estimate");
    let text = get_text(&resp);
    assert!(
        text.contains("estimate_hours"),
        "error should mention estimate_hours: {text}"
    );
}

#[test]
fn create_leaf_with_estimate_succeeds() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();
    enable_estimate_requirement(&pd);

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": {
                "title": "Has estimate",
                "schedule": { "estimate_hours": 3.0 }
            }
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
}

#[test]
fn create_blocked_task_without_estimate_is_allowed() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();
    enable_estimate_requirement(&pd);

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "title": "Blocked", "status": "blocked" }
        }),
    );

    assert!(
        !is_error(&resp),
        "blocked tasks are exempt: {}",
        get_text(&resp)
    );
}

#[test]
fn parent_task_update_without_estimate_is_allowed() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();
    enable_estimate_requirement(&pd);

    // Create parent (leaf at creation, so it needs an estimate)...
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "title": "Parent", "schedule": { "estimate_hours": 1.0 } }
        }),
    );
    // ...then a child, which makes t1 a parent.
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "parent_id": "t1",
            "task": { "title": "Child", "schedule": { "estimate_hours": 1.0 } }
        }),
    );

    // Updating the now-parent task (notes only) must be allowed even though the
    // patch carries no estimate — parents are exempt.
    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "t1", "notes": "parent now" }
        }),
    );
    assert!(
        !is_error(&resp),
        "parent task update should not require estimate: {}",
        get_text(&resp)
    );
}

#[test]
fn update_existing_task_keeps_estimate_without_resending() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();
    enable_estimate_requirement(&pd);

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "title": "Task", "schedule": { "estimate_hours": 5.0 } }
        }),
    );

    // Update notes only; existing estimate is preserved, so no error.
    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "t1", "notes": "updated" }
        }),
    );
    assert!(
        !is_error(&resp),
        "existing estimate should satisfy requirement: {}",
        get_text(&resp)
    );
}

#[test]
fn estimate_requirement_can_be_disabled() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();
    // setup_project() already disabled it; confirm a no-estimate create works.
    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "title": "No estimate, opt-out" }
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
}

// --- t79: the rejection must be fixable in one shot ---

/// The error must name the task it rejected. Without the id/title the caller
/// cannot tell which task in a multi-step flow failed.
#[test]
fn rejection_names_the_offending_task() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();
    enable_estimate_requirement(&pd);

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "t42", "title": "Refactor the parser" }
        }),
    );

    assert!(is_error(&resp), "should reject missing estimate");
    let text = get_text(&resp);
    assert!(
        text.contains("t42"),
        "error should name the task id: {text}"
    );
    assert!(
        text.contains("Refactor the parser"),
        "error should name the task title: {text}"
    );
}

/// The error must carry a concrete resend example, so the caller can retry
/// correctly on the first attempt rather than guessing the shape.
#[test]
fn rejection_includes_a_resend_example() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();
    enable_estimate_requirement(&pd);

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "title": "No estimate" }
        }),
    );

    let text = get_text(&resp);
    assert!(
        text.contains("\"estimate_hours\""),
        "error should show the JSON key in a resend example: {text}"
    );
    assert!(
        text.contains("\"schedule\""),
        "resend example should show the enclosing schedule object: {text}"
    );
}

/// A create was rejected, so the example must carry `title` — creating a task
/// without one fails. An example that triggers a *second* error defeats the
/// entire purpose of the message.
#[test]
fn create_rejection_example_includes_title() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();
    enable_estimate_requirement(&pd);

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "t9", "title": "New task" }
        }),
    );

    let text = get_text(&resp);
    assert!(
        text.contains("\"title\""),
        "a create-path example must include title, or the retry fails again: {text}"
    );
}

/// An update was rejected, so the task already exists and `title` need not be
/// resent. Keep the example minimal to avoid implying a needless overwrite.
#[test]
fn update_rejection_example_omits_title() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create without an estimate while the requirement is off...
    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "Legacy" } }),
    );
    enable_estimate_requirement(&pd);

    // ...then an update into a requiring status is rejected.
    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "t1", "status": "in_progress" }
        }),
    );

    assert!(is_error(&resp), "update into todo-like status must reject");
    let text = get_text(&resp);
    assert!(
        !text.contains("\"title\""),
        "an update-path example should not ask for title: {text}"
    );
}

/// A title containing a quote must not produce a malformed example. The
/// example is advertised as resendable, so it has to parse as JSON.
#[test]
fn create_rejection_example_is_valid_json_for_quoted_title() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();
    enable_estimate_requirement(&pd);

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "t1", "title": "Fix the \"retry\" path" }
        }),
    );

    let text = get_text(&resp);
    let example = text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with('{'))
        .unwrap_or_else(|| panic!("no example object in error: {text}"));

    let parsed: Value = serde_json::from_str(example)
        .unwrap_or_else(|e| panic!("example must be valid JSON ({e}): {example}"));
    assert_eq!(parsed["title"], "Fix the \"retry\" path");
    assert_eq!(parsed["schedule"]["estimate_hours"], 2.0);
}

/// A title containing a control character (newline, tab) must still yield a
/// parseable example. Hand-rolled `"` / `\` escaping does not cover these:
/// a raw newline inside a JSON string is a parse error.
#[test]
fn create_rejection_example_is_valid_json_for_control_chars_in_title() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();
    enable_estimate_requirement(&pd);

    let nasty = "line1\nline2\ttabbed";
    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "t1", "title": nasty }
        }),
    );

    let text = get_text(&resp);
    let example = text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with('{') && l.contains("estimate_hours"))
        .unwrap_or_else(|| panic!("no example object in error: {text}"));

    let parsed: Value = serde_json::from_str(example)
        .unwrap_or_else(|e| panic!("example must be valid JSON ({e}): {example}"));
    assert_eq!(parsed["title"], nasty, "title must round-trip exactly");
}

/// The error must state which statuses are exempt, so the caller learns the
/// rule instead of only the violation.
#[test]
fn rejection_states_the_exemptions() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();
    enable_estimate_requirement(&pd);

    let resp = call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "No estimate" } }),
    );

    let text = get_text(&resp);
    assert!(
        text.contains("blocked") && text.contains("skipped"),
        "error should name the exempt statuses: {text}"
    );
}

// --- t79: exemptions must survive the change ---

#[test]
fn create_skipped_task_without_estimate_is_allowed() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();
    enable_estimate_requirement(&pd);

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "title": "Skipped", "status": "skipped" }
        }),
    );

    assert!(
        !is_error(&resp),
        "skipped tasks are exempt: {}",
        get_text(&resp)
    );
}

/// A status-only patch on a task that never carried an estimate must still be
/// rejected when moving *into* a requiring status, but moving *out* to an
/// exempt status must be allowed. This is the partial-update path the t79
/// notes warn against breaking.
#[test]
fn status_only_update_into_exempt_status_is_allowed() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    // Create without an estimate while the requirement is off...
    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "Legacy task" } }),
    );
    // ...then turn the requirement on, as a real project would after adopting it.
    enable_estimate_requirement(&pd);

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "t1", "status": "blocked" }
        }),
    );
    assert!(
        !is_error(&resp),
        "status-only move into an exempt status must not require an estimate: {}",
        get_text(&resp)
    );
}

/// Status-only patch on a task that already has an estimate must not require
/// the caller to resend the estimate.
#[test]
fn status_only_update_keeps_persisted_estimate() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();
    enable_estimate_requirement(&pd);

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "title": "Task", "schedule": { "estimate_hours": 2.0 } }
        }),
    );

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "t1", "status": "in_progress" }
        }),
    );
    assert!(
        !is_error(&resp),
        "status-only update must reuse the persisted estimate: {}",
        get_text(&resp)
    );
}

/// The upsert-create path (id given, task does not exist) is a third call site
/// of the validator and must reject and report identically.
#[test]
fn upsert_create_leaf_without_estimate_is_rejected_and_named() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();
    enable_estimate_requirement(&pd);

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "custom-id", "title": "Upserted" }
        }),
    );

    assert!(is_error(&resp), "upsert-create should reject: {:?}", resp);
    let text = get_text(&resp);
    assert!(
        text.contains("custom-id") && text.contains("Upserted"),
        "upsert-create error should name id and title: {text}"
    );
}

/// A rejected create must not leave a half-built task directory behind.
#[test]
fn rejected_create_leaves_no_task_behind() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();
    enable_estimate_requirement(&pd);

    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "id": "ghost", "title": "Ghost" } }),
    );

    // Assert on the filesystem, not on handoff_list_tasks: the task index skips
    // directories with no `_task.*.json`, so an orphan dir is invisible there
    // and a list-based assertion would pass vacuously.
    let tasks_dir = dir.path().join(".handoff/tasks");
    let leftovers: Vec<String> = std::fs::read_dir(&tasks_dir)
        .expect("tasks dir should exist")
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();

    assert!(
        leftovers.is_empty(),
        "a rejected create must not leave a directory behind, found: {leftovers:?}"
    );
}

/// A rejected create must not consume its task ID. Otherwise every forgotten
/// estimate permanently burns an ID, and the next real task is misnumbered.
#[test]
fn rejected_create_does_not_burn_the_task_id() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();
    enable_estimate_requirement(&pd);

    // Two auto-id creates rejected for a missing estimate.
    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "First" } }),
    );
    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "Second" } }),
    );

    // The first task that actually succeeds must be t1.
    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "title": "Real", "schedule": { "estimate_hours": 1.0 } }
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(
        text.contains("Created task t1:"),
        "rejected creates must not burn ids; expected t1, got: {text}"
    );
}

// --- Hyphenated task ID resolution tests ---

#[test]
fn hyphenated_id_create_then_update() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "m2-burst", "title": "Burst mode state machine" }
        }),
    );
    assert!(!is_error(&resp), "create failed: {}", get_text(&resp));
    assert!(get_text(&resp).contains("m2-burst"));

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "m2-burst", "status": "in_progress" }
        }),
    );
    assert!(
        !is_error(&resp),
        "update by hyphenated id failed: {}",
        get_text(&resp)
    );
    assert!(get_text(&resp).contains("in_progress"));
}

#[test]
fn hyphenated_id_list_then_update_roundtrip() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "feat-login", "title": "Login feature" }
        }),
    );

    let list_resp = call_tool("handoff_list_tasks", json!({ "project_dir": &pd }));
    let list_text = get_text(&list_resp);
    assert!(
        list_text.contains("feat-login"),
        "list should show hyphenated id"
    );

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "feat-login", "status": "done" }
        }),
    );
    assert!(
        !is_error(&resp),
        "update via list_tasks id failed: {}",
        get_text(&resp)
    );
}

#[test]
fn hyphenated_id_get_task() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "p1-sub", "title": "Sub feature" }
        }),
    );

    let resp = call_tool(
        "handoff_get_task",
        json!({ "project_dir": &pd, "task_id": "p1-sub" }),
    );
    assert!(!is_error(&resp), "get_task failed: {}", get_text(&resp));
    assert!(get_text(&resp).contains("p1-sub"));
    assert!(get_text(&resp).contains("Sub feature"));
}

#[test]
fn hyphenated_id_check_criterion() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": {
                "id": "fix-auth",
                "title": "Auth fix",
                "done_criteria": [{"item": "Tests pass", "checked": false}]
            }
        }),
    );

    let resp = call_tool(
        "handoff_check_criterion",
        json!({
            "project_dir": &pd,
            "task_id": "fix-auth",
            "criterion_index": 0,
            "checked": true
        }),
    );
    assert!(
        !is_error(&resp),
        "check_criterion with hyphenated id failed: {}",
        get_text(&resp)
    );
}

#[test]
fn hyphenated_id_log_time() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": {
                "id": "dev-setup",
                "title": "Dev environment setup",
                "schedule": { "estimate_hours": 2.0 }
            }
        }),
    );

    let resp = call_tool(
        "handoff_log_time",
        json!({
            "project_dir": &pd,
            "task_id": "dev-setup",
            "hours": 0.5
        }),
    );
    assert!(
        !is_error(&resp),
        "log_time with hyphenated id failed: {}",
        get_text(&resp)
    );
}

#[test]
fn hyphenated_id_no_false_positive() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "t1", "title": "Task one" }
        }),
    );

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "t1-extra", "status": "done" }
        }),
    );
    assert!(
        is_error(&resp),
        "should NOT match t1 when looking for t1-extra"
    );
}

#[test]
fn notes_append_adds_to_existing() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "title": "Task with notes", "notes": "Line 1" }
        }),
    );

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "t1", "notes_append": "Line 2" }
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));

    let detail = call_tool(
        "handoff_get_task",
        json!({ "project_dir": &pd, "task_id": "t1" }),
    );
    let parsed: Value = serde_json::from_str(&get_text(&detail)).unwrap();
    let notes = parsed["notes"].as_str().unwrap();
    assert!(notes.contains("Line 1"), "original notes preserved");
    assert!(notes.contains("Line 2"), "appended text present");
}

#[test]
fn notes_append_to_empty() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "No notes yet" } }),
    );

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "t1", "notes_append": "First append" }
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));

    let detail = call_tool(
        "handoff_get_task",
        json!({ "project_dir": &pd, "task_id": "t1" }),
    );
    let parsed: Value = serde_json::from_str(&get_text(&detail)).unwrap();
    let notes = parsed["notes"].as_str().unwrap();
    assert!(notes.contains("First append"));
}

#[test]
fn notes_replace_takes_precedence_over_append() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "title": "Task", "notes": "Original" }
        }),
    );

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": {
                "id": "t1",
                "notes": "Replaced",
                "notes_append": "Should be ignored"
            }
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));

    let detail = call_tool(
        "handoff_get_task",
        json!({ "project_dir": &pd, "task_id": "t1" }),
    );
    let parsed: Value = serde_json::from_str(&get_text(&detail)).unwrap();
    let notes = parsed["notes"].as_str().unwrap();
    assert_eq!(notes, "Replaced");
    assert!(!notes.contains("Should be ignored"));
}

#[test]
fn notes_append_has_timestamp_heading() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "title": "Task", "notes": "Existing" }
        }),
    );

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "id": "t1", "notes_append": "Appended block" }
        }),
    );

    let detail = call_tool(
        "handoff_get_task",
        json!({ "project_dir": &pd, "task_id": "t1" }),
    );
    let parsed: Value = serde_json::from_str(&get_text(&detail)).unwrap();
    let notes = parsed["notes"].as_str().unwrap();
    // Timestamp heading format: --- YYYY-MM-DDTHH:MM:SS
    assert!(
        notes.contains("--- 20"),
        "should contain timestamp heading, got: {notes}"
    );
}

#[test]
fn notes_append_multiple_times() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();

    call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "Task" } }),
    );

    for i in 1..=3 {
        call_tool(
            "handoff_update_task",
            json!({
                "project_dir": &pd,
                "task": { "id": "t1", "notes_append": format!("Block {i}") }
            }),
        );
    }

    let detail = call_tool(
        "handoff_get_task",
        json!({ "project_dir": &pd, "task_id": "t1" }),
    );
    let parsed: Value = serde_json::from_str(&get_text(&detail)).unwrap();
    let notes = parsed["notes"].as_str().unwrap();
    assert!(notes.contains("Block 1"));
    assert!(notes.contains("Block 2"));
    assert!(notes.contains("Block 3"));
}
