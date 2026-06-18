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
        text.contains("handoff_list_tasks") || text.contains("Available"),
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
