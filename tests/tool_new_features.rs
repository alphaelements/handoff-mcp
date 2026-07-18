use serde_json::{json, Value};
use std::fs;
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
                "project_name": "test-project"
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

// ============================================================
// P1: TaskData struct extensions (assignee, remaining_hours, pinned)
// ============================================================

#[test]
fn create_task_with_assignee_and_extended_schedule() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": {
                "title": "Task with assignee",
                "assignee": "alice",
                "schedule": {
                    "start_date": "2026-06-23",
                    "due_date": "2026-06-27",
                    "estimate_hours": 16.0,
                    "remaining_hours": 12.0,
                    "pinned": true,
                    "milestone": "v1.0"
                }
            }
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));

    // Verify via get_task
    let resp = call_tool(
        "handoff_get_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task_id": "t1"
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let task: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(task["assignee"], "alice");
    assert_eq!(task["schedule"]["remaining_hours"], 12.0);
    assert_eq!(task["schedule"]["pinned"], true);
    assert_eq!(task["schedule"]["milestone"], "v1.0");
}

#[test]
fn update_task_assignee() {
    let dir = setup_project();
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "Unassigned task" }
        }),
    );

    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "id": "t1", "assignee": "bob" }
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));

    let resp = call_tool(
        "handoff_get_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task_id": "t1"
        }),
    );
    let task: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(task["assignee"], "bob");
}

#[test]
fn extra_fields_preserved_on_roundtrip() {
    let dir = setup_project();
    let tasks_dir = dir.path().join(".handoff/tasks");
    let task_dir = tasks_dir.join("t1-custom");
    fs::create_dir_all(&task_dir).unwrap();

    // Write a task file with extra unknown fields (like VSCode extension writes)
    let custom_json = json!({
        "id": "t1",
        "title": "Custom task",
        "custom_field": "preserved_value",
        "another_extra": 42
    });
    fs::write(
        task_dir.join("_task.todo.json"),
        serde_json::to_string_pretty(&custom_json).unwrap(),
    )
    .unwrap();

    // Update the task via MCP (should preserve unknown fields)
    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "id": "t1", "priority": "high" }
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));

    // Read file directly and verify extra fields survived
    let content = fs::read_to_string(task_dir.join("_task.todo.json")).unwrap();
    let saved: Value = serde_json::from_str(&content).unwrap();
    assert_eq!(saved["custom_field"], "preserved_value");
    assert_eq!(saved["another_extra"], 42);
    assert_eq!(saved["priority"], "high");
}

#[test]
fn assignee_appears_in_list_tasks() {
    let dir = setup_project();
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "Alice task", "assignee": "alice" }
        }),
    );

    let resp = call_tool(
        "handoff_list_tasks",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    let data: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(data["task_tree"][0]["assignee"], "alice");
}

// ============================================================
// P1: handoff_log_time
// ============================================================

#[test]
fn log_time_adds_to_actual_and_deducts_remaining() {
    let dir = setup_project();
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": {
                "title": "Tracked task",
                "schedule": {
                    "estimate_hours": 8.0,
                    "actual_hours": 2.0,
                    "remaining_hours": 6.0
                }
            }
        }),
    );

    let resp = call_tool(
        "handoff_log_time",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task_id": "t1",
            "hours": 1.5
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(text.contains("actual=3.5h"));
    assert!(text.contains("remaining=4.5h"));

    // Verify by reading task
    let resp = call_tool(
        "handoff_get_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task_id": "t1"
        }),
    );
    let task: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(task["schedule"]["actual_hours"], 3.5);
    assert_eq!(task["schedule"]["remaining_hours"], 4.5);
}

#[test]
fn log_time_clamps_remaining_to_zero() {
    let dir = setup_project();
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": {
                "title": "Almost done",
                "schedule": { "actual_hours": 7.0, "remaining_hours": 0.5 }
            }
        }),
    );

    let resp = call_tool(
        "handoff_log_time",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task_id": "t1",
            "hours": 2.0
        }),
    );
    assert!(!is_error(&resp));
    let text = get_text(&resp);
    assert!(text.contains("remaining=0.0h"));
}

#[test]
fn log_time_without_remaining_hours_still_works() {
    let dir = setup_project();
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "No remaining", "schedule": { "actual_hours": 1.0 } }
        }),
    );

    let resp = call_tool(
        "handoff_log_time",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task_id": "t1",
            "hours": 0.5
        }),
    );
    assert!(!is_error(&resp));
    let text = get_text(&resp);
    assert!(text.contains("actual=1.5h"));
    assert!(!text.contains("remaining="));
}

#[test]
fn log_time_rejects_negative_hours() {
    let dir = setup_project();
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "Task" }
        }),
    );

    let resp = call_tool(
        "handoff_log_time",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task_id": "t1",
            "hours": -1.0
        }),
    );
    assert!(is_error(&resp));
}

// ============================================================
// P1: handoff_get_metrics
// ============================================================

#[test]
fn get_metrics_returns_correct_counts() {
    let dir = setup_project();

    // Create tasks with different statuses
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "Done task", "status": "done", "schedule": { "estimate_hours": 4.0, "actual_hours": 3.0 } }
        }),
    );
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "IP task", "status": "in_progress", "assignee": "alice", "schedule": { "estimate_hours": 8.0, "due_date": "2020-01-01" } }
        }),
    );
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "Todo task" }
        }),
    );

    let resp = call_tool(
        "handoff_get_metrics",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let metrics: Value = serde_json::from_str(&get_text(&resp)).unwrap();

    assert_eq!(metrics["total"], 3);
    assert_eq!(metrics["by_status"]["done"], 1);
    assert_eq!(metrics["by_status"]["in_progress"], 1);
    assert_eq!(metrics["by_status"]["todo"], 1);
    assert_eq!(metrics["total_estimate_hours"], 12.0);
    assert_eq!(metrics["total_actual_hours"], 3.0);
    assert_eq!(metrics["overdue_count"], 1);
    assert!(metrics["overdue_tasks"][0]["id"]
        .as_str()
        .unwrap()
        .contains("t2"));
}

#[test]
fn get_metrics_with_assignee_filter() {
    let dir = setup_project();
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "Alice task", "assignee": "alice", "schedule": { "estimate_hours": 5.0 } }
        }),
    );
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "Bob task", "assignee": "bob", "schedule": { "estimate_hours": 10.0 } }
        }),
    );

    let resp = call_tool(
        "handoff_get_metrics",
        json!({ "project_dir": dir.path().to_string_lossy(), "assignee": "alice" }),
    );
    let metrics: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(metrics["total"], 1);
    assert_eq!(metrics["total_estimate_hours"], 5.0);
}

// ============================================================
// P1: handoff_update_config extension
// ============================================================

#[test]
fn update_config_calendar_and_assignees() {
    let dir = setup_project();

    let resp = call_tool(
        "handoff_update_config",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "updates": {
                "calendar.work_hours_per_day": 7,
                "calendar.closed_weekdays": ["sat", "sun"],
                "calendar.schedule_mode": "auto",
                "effort_budget.total_hours": 200,
                "assignees.alice.display_name": "Alice Chen",
                "assignees.alice.color": "#4A90D9",
                "assignees.alice.work_hours_per_day": 6,
                "gantt_view.sort": "start",
                "gantt_view.zoom": "week"
            }
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));

    // Verify by reading config.toml directly
    let config_content = fs::read_to_string(dir.path().join(".handoff/config.toml")).unwrap();
    assert!(config_content.contains("work_hours_per_day = 7"));
    assert!(config_content.contains("schedule_mode = \"auto\""));
    assert!(config_content.contains("total_hours = 200"));
    assert!(config_content.contains("display_name = \"Alice Chen\""));
    assert!(config_content.contains("color = \"#4A90D9\""));
    assert!(config_content.contains("sort = \"start\""));
}

// ============================================================
// P1: handoff_list_sessions
// ============================================================

#[test]
fn list_sessions_returns_sessions() {
    let dir = setup_project();

    // Create a session via save_context
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "summary": "Test session 1"
        }),
    );

    let resp = call_tool(
        "handoff_list_sessions",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let sessions: Vec<Value> = serde_json::from_str(&get_text(&resp)).unwrap();
    assert!(!sessions.is_empty());
    assert_eq!(sessions[0]["summary"], "Test session 1");
    assert!(sessions[0]["id"].as_str().unwrap().starts_with("s-"));
}

#[test]
fn list_sessions_with_status_filter() {
    let dir = setup_project();
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "summary": "Closed session"
        }),
    );

    let resp = call_tool(
        "handoff_list_sessions",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "status_filter": "active"
        }),
    );
    let sessions: Vec<Value> = serde_json::from_str(&get_text(&resp)).unwrap();
    // After save_context with default, session is closed, so active filter returns empty
    assert!(sessions.is_empty());
}

// ============================================================
// P2: handoff_list_assignees
// ============================================================

#[test]
fn list_assignees_with_task_counts() {
    let dir = setup_project();

    // Set up config with assignees
    call_tool(
        "handoff_update_config",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "updates": {
                "assignees.alice.display_name": "Alice",
                "assignees.alice.color": "#ff0000",
                "assignees.bob.display_name": "Bob",
                "assignees.bob.color": "#00ff00"
            }
        }),
    );

    // Create tasks with assignees
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "Alice task 1", "assignee": "alice", "status": "in_progress", "schedule": { "estimate_hours": 4.0 } }
        }),
    );
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "Alice task 2", "assignee": "alice" }
        }),
    );
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "Bob task", "assignee": "bob", "schedule": { "estimate_hours": 8.0 } }
        }),
    );

    let resp = call_tool(
        "handoff_list_assignees",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let data: Value = serde_json::from_str(&get_text(&resp)).unwrap();

    assert_eq!(data["assignees"]["alice"]["task_count"], 2);
    assert_eq!(data["assignees"]["alice"]["active_task_count"], 1);
    assert_eq!(data["assignees"]["alice"]["total_estimate_hours"], 4.0);
    assert_eq!(data["assignees"]["bob"]["task_count"], 1);
    assert_eq!(data["assignees"]["bob"]["total_estimate_hours"], 8.0);
}

// ============================================================
// P2: handoff_bulk_update_tasks
// ============================================================

#[test]
fn bulk_update_applies_multiple_changes() {
    let dir = setup_project();
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "Task A" }
        }),
    );
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "Task B" }
        }),
    );
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "Task C" }
        }),
    );

    let resp = call_tool(
        "handoff_bulk_update_tasks",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "updates": [
                { "task_id": "t1", "assignee": "alice", "schedule": { "start_date": "2026-07-01", "due_date": "2026-07-05" } },
                { "task_id": "t2", "status": "in_progress", "priority": "high" },
                { "task_id": "t3", "schedule": { "pinned": true } }
            ]
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let result: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(result["applied"], 3);
    assert_eq!(result["errors"].as_array().unwrap().len(), 0);

    // Verify changes
    let resp = call_tool(
        "handoff_get_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": "t1" }),
    );
    let t1: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(t1["assignee"], "alice");
    assert_eq!(t1["schedule"]["start_date"], "2026-07-01");

    let resp = call_tool(
        "handoff_get_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": "t2" }),
    );
    let t2: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(t2["status"], "in_progress");
    assert_eq!(t2["priority"], "high");

    let resp = call_tool(
        "handoff_get_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": "t3" }),
    );
    let t3: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(t3["schedule"]["pinned"], true);
}

#[test]
fn bulk_update_reports_per_task_errors() {
    let dir = setup_project();
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "Exists" }
        }),
    );

    let resp = call_tool(
        "handoff_bulk_update_tasks",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "updates": [
                { "task_id": "t1", "priority": "high" },
                { "task_id": "t99", "priority": "high" }
            ]
        }),
    );
    let result: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(result["applied"], 1);
    assert_eq!(result["errors"].as_array().unwrap().len(), 1);
    assert_eq!(result["errors"][0]["task_id"], "t99");
}

// --- bulk_update honours the estimate_hours requirement (t80) ---

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

/// Create a task while the requirement is off, so the fixture can hold a task
/// in any status with any (or no) estimate. The requirement is enabled after.
fn seed_task(pd: &str, task: Value) {
    let resp = call_tool(
        "handoff_update_task",
        json!({ "project_dir": pd, "task": task }),
    );
    assert!(!is_error(&resp), "seed error: {}", get_text(&resp));
}

fn task_status(pd: &str, task_id: &str) -> String {
    let resp = call_tool(
        "handoff_get_task",
        json!({ "project_dir": pd, "task_id": task_id }),
    );
    let task: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    task["status"].as_str().unwrap().to_string()
}

#[test]
fn bulk_update_rejects_exempt_to_required_status_without_estimate() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();
    seed_task(
        &pd,
        json!({ "title": "Blocked, no estimate", "status": "blocked" }),
    );
    enable_estimate_requirement(&pd);

    let resp = call_tool(
        "handoff_bulk_update_tasks",
        json!({
            "project_dir": &pd,
            "updates": [ { "task_id": "t1", "status": "in_progress" } ]
        }),
    );

    let result: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(result["applied"], 0, "the update must not be applied");
    assert_eq!(result["errors"].as_array().unwrap().len(), 1);
    assert_eq!(result["errors"][0]["task_id"], "t1");
    let err = result["errors"][0]["error"].as_str().unwrap();
    // Bulk only ever updates, so the resend example must not carry `title` —
    // suggesting it would imply the stored title gets overwritten.
    assert!(
        !err.contains("\"title\""),
        "an update's resend example must not include title: {err}"
    );
    assert!(
        err.contains("\"id\"") && err.contains("estimate_hours"),
        "resend example should show id + schedule.estimate_hours: {err}"
    );

    // The rejection must not have been half-applied to disk.
    assert_eq!(task_status(&pd, "t1"), "blocked");
}

#[test]
fn bulk_update_accepts_status_change_when_estimate_supplied_in_same_patch() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();
    seed_task(
        &pd,
        json!({ "title": "Blocked, no estimate", "status": "blocked" }),
    );
    enable_estimate_requirement(&pd);

    let resp = call_tool(
        "handoff_bulk_update_tasks",
        json!({
            "project_dir": &pd,
            "updates": [
                { "task_id": "t1", "status": "in_progress", "schedule": { "estimate_hours": 2.0 } }
            ]
        }),
    );

    let result: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(
        result["applied"],
        1,
        "supplying the estimate in the same patch must pass: {}",
        get_text(&resp)
    );
    assert_eq!(task_status(&pd, "t1"), "in_progress");
}

#[test]
fn bulk_update_allows_transition_into_exempt_status_without_estimate() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();
    seed_task(&pd, json!({ "title": "Todo, no estimate" }));
    enable_estimate_requirement(&pd);

    // todo -> blocked, and todo -> skipped: both target statuses are exempt.
    let resp = call_tool(
        "handoff_bulk_update_tasks",
        json!({
            "project_dir": &pd,
            "updates": [ { "task_id": "t1", "status": "blocked" } ]
        }),
    );
    let result: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(
        result["applied"],
        1,
        "blocked is exempt: {}",
        get_text(&resp)
    );

    let resp = call_tool(
        "handoff_bulk_update_tasks",
        json!({
            "project_dir": &pd,
            "updates": [ { "task_id": "t1", "status": "skipped" } ]
        }),
    );
    let result: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(
        result["applied"],
        1,
        "skipped is exempt: {}",
        get_text(&resp)
    );
    assert_eq!(task_status(&pd, "t1"), "skipped");
}

#[test]
fn bulk_update_allows_parent_task_without_estimate() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();
    seed_task(&pd, json!({ "title": "Parent, no estimate" }));
    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "parent_id": "t1",
            "task": { "title": "Child" }
        }),
    );
    assert!(!is_error(&resp), "seed child: {}", get_text(&resp));
    enable_estimate_requirement(&pd);

    // t1 is now a parent: exempt even though it carries no estimate and the
    // patch drives it into a status that would otherwise require one.
    let resp = call_tool(
        "handoff_bulk_update_tasks",
        json!({
            "project_dir": &pd,
            "updates": [ { "task_id": "t1", "status": "in_progress" } ]
        }),
    );
    let result: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(
        result["applied"],
        1,
        "parent tasks are exempt: {}",
        get_text(&resp)
    );
    assert_eq!(task_status(&pd, "t1"), "in_progress");
}

#[test]
fn bulk_update_rejects_estimateless_task_on_date_only_patch() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();
    seed_task(
        &pd,
        json!({ "title": "In progress, no estimate", "status": "in_progress" }),
    );
    enable_estimate_requirement(&pd);

    // A date-only patch does not change status, but the task is left in
    // `in_progress` without an estimate — the same state update_task refuses to
    // write. The check is on the resulting task, not on the patch.
    let resp = call_tool(
        "handoff_bulk_update_tasks",
        json!({
            "project_dir": &pd,
            "updates": [ { "task_id": "t1", "schedule": { "start_date": "2026-07-01" } } ]
        }),
    );
    let result: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(result["applied"], 0);
    let err = result["errors"][0]["error"].as_str().unwrap();
    assert!(
        err.contains("estimate_hours"),
        "error should mention estimate_hours: {err}"
    );
}

#[test]
fn bulk_update_allows_date_only_patch_when_estimate_present() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();
    seed_task(
        &pd,
        json!({ "title": "Estimated", "schedule": { "estimate_hours": 4.0 } }),
    );
    enable_estimate_requirement(&pd);

    // The auto_schedule shape: move dates, leave status and estimate alone.
    let resp = call_tool(
        "handoff_bulk_update_tasks",
        json!({
            "project_dir": &pd,
            "updates": [
                { "task_id": "t1", "schedule": { "start_date": "2026-07-01", "due_date": "2026-07-05" } }
            ]
        }),
    );
    let result: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(
        result["applied"],
        1,
        "date-only rescheduling must keep working: {}",
        get_text(&resp)
    );

    let resp = call_tool(
        "handoff_get_task",
        json!({ "project_dir": &pd, "task_id": "t1" }),
    );
    let t1: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(t1["schedule"]["start_date"], "2026-07-01");
    assert_eq!(t1["schedule"]["estimate_hours"], 4.0);
}

#[test]
fn bulk_update_rejects_only_the_offending_task() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();
    seed_task(&pd, json!({ "title": "No estimate", "status": "blocked" }));
    seed_task(
        &pd,
        json!({ "title": "Estimated", "schedule": { "estimate_hours": 2.0 }, "status": "blocked" }),
    );
    enable_estimate_requirement(&pd);

    // Per-task errors, not an all-or-nothing rollback: bulk_update's contract is
    // `applied` + `errors[]`, and the good task must still land.
    let resp = call_tool(
        "handoff_bulk_update_tasks",
        json!({
            "project_dir": &pd,
            "updates": [
                { "task_id": "t1", "status": "in_progress" },
                { "task_id": "t2", "status": "in_progress" }
            ]
        }),
    );
    let result: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(result["applied"], 1);
    assert_eq!(result["errors"].as_array().unwrap().len(), 1);
    assert_eq!(result["errors"][0]["task_id"], "t1");
    assert_eq!(task_status(&pd, "t1"), "blocked");
    assert_eq!(task_status(&pd, "t2"), "in_progress");
}

#[test]
fn bulk_update_fails_closed_when_config_is_unreadable() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy().to_string();
    seed_task(
        &pd,
        json!({ "title": "Blocked, no estimate", "status": "blocked" }),
    );

    // An unparseable config must not silently disable the requirement: a
    // corrupt file is exactly when a guard is most likely to be bypassed.
    fs::write(
        dir.path().join(".handoff/config.toml"),
        "this is not = valid = toml",
    )
    .unwrap();

    let resp = call_tool(
        "handoff_bulk_update_tasks",
        json!({
            "project_dir": &pd,
            "updates": [ { "task_id": "t1", "status": "in_progress" } ]
        }),
    );
    let result: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(
        result["applied"],
        0,
        "a corrupt config must fail closed, not open: {}",
        get_text(&resp)
    );
    let err = result["errors"][0]["error"].as_str().unwrap();
    assert!(
        err.contains("estimate_hours"),
        "error should mention estimate_hours: {err}"
    );
    assert_eq!(task_status(&pd, "t1"), "blocked");
}

// ============================================================
// P2: handoff_get_session
// ============================================================

#[test]
fn get_session_returns_full_detail() {
    let dir = setup_project();
    call_tool(
        "handoff_save_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "summary": "Detailed session",
            "decisions": [{"decision": "Use Rust", "reason": "Performance", "confidence": "confirmed"}],
            "handoff_notes": [{"note": "Check memory usage", "category": "caution"}]
        }),
    );

    // Get session list to find the ID
    let resp = call_tool(
        "handoff_list_sessions",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    let sessions: Vec<Value> = serde_json::from_str(&get_text(&resp)).unwrap();
    let session_id = sessions[0]["id"].as_str().unwrap();

    let resp = call_tool(
        "handoff_get_session",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "session_id": session_id
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let session: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(session["summary"], "Detailed session");
    assert_eq!(session["decisions"][0]["decision"], "Use Rust");
    assert_eq!(session["handoff_notes"][0]["category"], "caution");
}

// ============================================================
// P3: handoff_list_tasks filter extensions
// ============================================================

#[test]
fn list_tasks_filter_by_assignee() {
    let dir = setup_project();
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "Alice task", "assignee": "alice" }
        }),
    );
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "Bob task", "assignee": "bob" }
        }),
    );

    let resp = call_tool(
        "handoff_list_tasks",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "assignee_filter": "alice"
        }),
    );
    let data: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let tree = data["task_tree"].as_array().unwrap();
    assert_eq!(tree.len(), 1);
    assert_eq!(tree[0]["assignee"], "alice");
}

#[test]
fn list_tasks_filter_by_milestone() {
    let dir = setup_project();
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "v1 task", "schedule": { "milestone": "v1.0" } }
        }),
    );
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "v2 task", "schedule": { "milestone": "v2.0" } }
        }),
    );

    let resp = call_tool(
        "handoff_list_tasks",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "milestone_filter": "v1.0"
        }),
    );
    let data: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let tree = data["task_tree"].as_array().unwrap();
    assert_eq!(tree.len(), 1);
    assert!(tree[0]["title"].as_str().unwrap().contains("v1"));
}

// ============================================================
// P3: handoff_get_capacity
// ============================================================

#[test]
fn get_capacity_respects_calendar() {
    let dir = setup_project();

    // Set up calendar with weekends closed
    call_tool(
        "handoff_update_config",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "updates": {
                "calendar.work_hours_per_day": 8,
                "calendar.closed_weekdays": ["sat", "sun"]
            }
        }),
    );

    // Query a week (Mon 2026-06-22 to Sun 2026-06-28)
    let resp = call_tool(
        "handoff_get_capacity",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "start_date": "2026-06-22",
            "end_date": "2026-06-28"
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let cap: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(cap["work_days"], 5);
    assert_eq!(cap["total_hours"], 40.0);
    assert_eq!(cap["days"].as_array().unwrap().len(), 7);
}

// ============================================================
// P3: handoff_auto_schedule
// ============================================================

#[test]
fn auto_schedule_dry_run_returns_changes() {
    let dir = setup_project();

    call_tool(
        "handoff_update_config",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "updates": {
                "calendar.work_hours_per_day": 8,
                "calendar.closed_weekdays": ["sat", "sun"]
            }
        }),
    );

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "Schedule me", "schedule": { "estimate_hours": 16.0 } }
        }),
    );

    let resp = call_tool(
        "handoff_auto_schedule",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "dry_run": true
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let result: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(result["dry_run"], true);
    assert_eq!(result["scheduled_count"], 1);
    assert!(!result["changes"].as_array().unwrap().is_empty());
    let change = &result["changes"][0];
    assert!(change["new_start"].as_str().is_some());
    assert!(change["new_due"].as_str().is_some());
}

#[test]
fn auto_schedule_skips_pinned_tasks() {
    let dir = setup_project();

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": {
                "title": "Pinned task",
                "schedule": { "start_date": "2026-01-01", "due_date": "2026-01-05", "pinned": true, "estimate_hours": 8.0 }
            }
        }),
    );

    let resp = call_tool(
        "handoff_auto_schedule",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "dry_run": true
        }),
    );
    let result: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(result["scheduled_count"], 0);
}

// ============================================================
// File structure compatibility with handoff-vscode
// ============================================================

#[test]
fn task_file_structure_matches_vscode_reader() {
    let dir = setup_project();

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": {
                "title": "Full featured task",
                "status": "in_progress",
                "priority": "high",
                "assignee": "alice",
                "labels": ["frontend", "urgent"],
                "links": ["https://gitlab.com/issue/123"],
                "notes": "Some implementation notes",
                "done_criteria": [
                    {"item": "Write tests", "checked": true},
                    {"item": "Code review", "checked": false}
                ],
                "schedule": {
                    "start_date": "2026-06-23",
                    "due_date": "2026-06-30",
                    "estimate_hours": 24.0,
                    "actual_hours": 8.0,
                    "remaining_hours": 16.0,
                    "milestone": "v1.0",
                    "pinned": false
                },
                "dependencies": []
            }
        }),
    );

    // Read the file directly and verify structure matches what vscode expects
    let tasks_dir = dir.path().join(".handoff/tasks");
    let mut task_file = None;
    for entry in fs::read_dir(&tasks_dir).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            let dir_path = entry.path();
            let file = dir_path.join("_task.in_progress.json");
            if file.exists() {
                task_file = Some(file);
                break;
            }
        }
    }

    let task_file = task_file.expect("task file should exist");
    let content = fs::read_to_string(&task_file).unwrap();
    let json: Value = serde_json::from_str(&content).unwrap();

    // Verify all fields the VSCode extension reader expects
    assert_eq!(json["id"], "t1");
    assert_eq!(json["title"], "Full featured task");
    assert_eq!(json["priority"], "high");
    assert_eq!(json["assignee"], "alice");
    assert!(json["labels"]
        .as_array()
        .unwrap()
        .contains(&json!("frontend")));
    assert!(json["links"]
        .as_array()
        .unwrap()
        .contains(&json!("https://gitlab.com/issue/123")));
    assert_eq!(json["notes"], "Some implementation notes");
    assert_eq!(json["done_criteria"][0]["item"], "Write tests");
    assert_eq!(json["done_criteria"][0]["checked"], true);
    assert_eq!(json["done_criteria"][1]["item"], "Code review");
    assert_eq!(json["done_criteria"][1]["checked"], false);
    assert_eq!(json["schedule"]["start_date"], "2026-06-23");
    assert_eq!(json["schedule"]["due_date"], "2026-06-30");
    assert_eq!(json["schedule"]["estimate_hours"], 24.0);
    assert_eq!(json["schedule"]["actual_hours"], 8.0);
    assert_eq!(json["schedule"]["remaining_hours"], 16.0);
    assert_eq!(json["schedule"]["milestone"], "v1.0");
    assert_eq!(json["schedule"]["pinned"], false);
    assert!(json["created_at"].as_str().is_some());
    assert!(json["updated_at"].as_str().is_some());

    // Verify file is named correctly (status in filename)
    assert!(task_file.file_name().unwrap().to_str().unwrap() == "_task.in_progress.json");

    // Verify directory naming convention
    let dir_name = task_file
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    assert!(dir_name.starts_with("t1-"));
}

#[test]
fn config_toml_structure_matches_vscode_reader() {
    let dir = setup_project();

    call_tool(
        "handoff_update_config",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "updates": {
                "calendar.work_hours_per_day": 7,
                "calendar.closed_weekdays": ["sat", "sun"],
                "calendar.closed_dates": ["2026-12-25"],
                "calendar.open_dates": ["2026-12-26"],
                "calendar.schedule_mode": "auto",
                "calendar.overwork_limit_percent": 150,
                "effort_budget.total_hours": 500,
                "assignees.alice.display_name": "Alice Chen",
                "assignees.alice.color": "#4A90D9",
                "assignees.alice.work_hours_per_day": 6,
                "assignees.bob.display_name": "Bob Smith",
                "assignees.bob.color": "#E74C3C",
                "gantt_view.sort": "start",
                "gantt_view.zoom": "week",
                "gantt_view.mode": "compare"
            }
        }),
    );

    // Read config and verify it parses as valid TOML with expected structure
    let config_str = fs::read_to_string(dir.path().join(".handoff/config.toml")).unwrap();
    let config: toml::Value = toml::from_str(&config_str).unwrap();

    // Calendar section
    let calendar = config.get("calendar").unwrap().as_table().unwrap();
    assert_eq!(calendar["work_hours_per_day"].as_integer(), Some(7));
    assert_eq!(calendar["schedule_mode"].as_str(), Some("auto"));
    assert_eq!(calendar["overwork_limit_percent"].as_integer(), Some(150));
    let closed_weekdays = calendar["closed_weekdays"].as_array().unwrap();
    assert_eq!(closed_weekdays.len(), 2);

    // Effort budget
    let budget = config.get("effort_budget").unwrap().as_table().unwrap();
    assert_eq!(budget["total_hours"].as_integer(), Some(500));

    // Assignees
    let assignees = config.get("assignees").unwrap().as_table().unwrap();
    assert_eq!(
        assignees["alice"]["display_name"].as_str(),
        Some("Alice Chen")
    );
    assert_eq!(assignees["alice"]["color"].as_str(), Some("#4A90D9"));
    assert_eq!(assignees["bob"]["display_name"].as_str(), Some("Bob Smith"));

    // Gantt view
    let gantt = config.get("gantt_view").unwrap().as_table().unwrap();
    assert_eq!(gantt["sort"].as_str(), Some("start"));
    assert_eq!(gantt["zoom"].as_str(), Some("week"));
    assert_eq!(gantt["mode"].as_str(), Some("compare"));
}

// ============================================================
// AI estimate multiplier (referral ref-20260625-015330)
// ============================================================

#[test]
fn metrics_apply_default_ai_estimate_multiplier() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy();

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": pd,
            "task": { "title": "A", "schedule": { "estimate_hours": 10.0 } }
        }),
    );
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": pd,
            "task": { "title": "B", "schedule": { "estimate_hours": 5.0 } }
        }),
    );

    let resp = call_tool("handoff_get_metrics", json!({ "project_dir": pd }));
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let metrics: Value = serde_json::from_str(&get_text(&resp)).unwrap();

    // raw estimate preserved
    assert_eq!(metrics["total_estimate_hours"], 15.0);
    // default multiplier 0.2 applied
    assert_eq!(metrics["ai_estimate_multiplier"], 0.2);
    assert_eq!(metrics["total_adjusted_estimate_hours"], 3.0);
}

#[test]
fn metrics_respect_custom_multiplier() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy();

    let resp = call_tool(
        "handoff_update_config",
        json!({
            "project_dir": pd,
            "updates": { "settings.ai_estimate_multiplier": 0.5 }
        }),
    );
    assert!(!is_error(&resp), "config error: {}", get_text(&resp));

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": pd,
            "task": {
                "title": "A",
                "schedule": { "estimate_hours": 8.0, "milestone": "v1" }
            }
        }),
    );

    let resp = call_tool("handoff_get_metrics", json!({ "project_dir": pd }));
    let metrics: Value = serde_json::from_str(&get_text(&resp)).unwrap();

    assert_eq!(metrics["total_estimate_hours"], 8.0);
    assert_eq!(metrics["ai_estimate_multiplier"], 0.5);
    assert_eq!(metrics["total_adjusted_estimate_hours"], 4.0);

    // per-milestone adjusted value too
    let ms = &metrics["milestones"][0];
    assert_eq!(ms["estimate_hours"], 8.0);
    assert_eq!(ms["adjusted_estimate_hours"], 4.0);
}

#[test]
fn capacity_allocates_adjusted_estimate_hours() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy();

    // multiplier 0.5; a 10h human estimate over a single day -> 5h allocated.
    call_tool(
        "handoff_update_config",
        json!({
            "project_dir": pd,
            "updates": {
                "settings.ai_estimate_multiplier": 0.5,
                "calendar.work_hours_per_day": 8.0
            }
        }),
    );

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": pd,
            "task": {
                "title": "Single-day task",
                "status": "in_progress",
                "schedule": {
                    "estimate_hours": 10.0,
                    "start_date": "2030-06-03",
                    "due_date": "2030-06-03"
                }
            }
        }),
    );

    let resp = call_tool(
        "handoff_get_capacity",
        json!({
            "project_dir": pd,
            "start_date": "2030-06-03",
            "end_date": "2030-06-03"
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let cap: Value = serde_json::from_str(&get_text(&resp)).unwrap();

    // 10h * 0.5 multiplier = 5h allocated (not the raw 10h).
    assert!(
        (cap["allocated_hours"].as_f64().unwrap() - 5.0).abs() < 1e-9,
        "expected 5.0 allocated, got {}",
        cap["allocated_hours"]
    );
}

#[test]
fn negative_multiplier_is_rejected() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy();

    let resp = call_tool(
        "handoff_update_config",
        json!({
            "project_dir": pd,
            "updates": { "settings.ai_estimate_multiplier": -0.5 }
        }),
    );
    assert!(is_error(&resp), "negative multiplier should be rejected");
}

// ============================================================
// closed_weekdays string deserialization (referral ref-20260718-045008)
// ============================================================

#[test]
fn read_config_parses_string_closed_weekdays() {
    let dir = setup_project();
    let pd = dir.path().to_string_lossy();

    let resp = call_tool(
        "handoff_update_config",
        json!({
            "project_dir": pd,
            "updates": {
                "calendar.closed_weekdays": ["sun", "sat"],
                "assignees.alice.closed_weekdays": ["mon", "friday"]
            }
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));

    let config =
        handoff_mcp::storage::config::read_config(&dir.path().join(".handoff/config.toml"))
            .expect("read_config should parse string weekday names");
    assert_eq!(config.calendar.closed_weekdays, vec![0, 6]);
    let alice = config.assignees.get("alice").unwrap();
    assert_eq!(alice.closed_weekdays, vec![1, 5]);
}
