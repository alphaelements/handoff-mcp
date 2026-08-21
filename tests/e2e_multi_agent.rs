//! E2E tests for multi-agent claim/release contention (spec FR-1.4, NFR-5).
//!
//! These drive the real JSON-RPC dispatch path
//! (`handoff_mcp::mcp::protocol::process_line`) rather than calling storage
//! functions directly, so they exercise the same code path a real MCP client
//! would: argument parsing, handler dispatch, lazy scan triggers, advisory
//! warnings and event logging all as wired together in `mcp::handlers`.
//!
//! `crate::mcp::router::set_agent_id` is a process-wide global (one running
//! MCP server process serves exactly one agent for its lifetime — see its
//! doc comment). Test binaries run `#[test]`s concurrently in one process, so
//! any test that calls `set_agent_id` must be serialized against every other
//! such test with `AGENT_ID_GLOBAL`, mirroring the pattern already used in
//! `tests/tool_claim_release.rs`.
use std::path::Path;
use std::sync::Barrier;
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

static AGENT_ID_GLOBAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

/// Initialize a fresh project under `dir` and disable the estimate-hours
/// requirement so `create_todo_task` can create bare tasks without extra
/// fields.
fn setup_project(dir: &Path) -> Value {
    let init = call_tool(
        "handoff_init",
        json!({
            "project_dir": dir.to_string_lossy(),
            "project_name": "test"
        }),
    );
    assert!(!is_error(&init), "init failed: {}", get_text(&init));

    let cfg = call_tool(
        "handoff_update_config",
        json!({
            "project_dir": dir.to_string_lossy(),
            "updates": { "settings.require_estimate_hours": false }
        }),
    );
    assert!(!is_error(&cfg), "config update failed: {}", get_text(&cfg));
    init
}

/// Create a `todo` task via `handoff_update_task` and return its id.
fn create_todo_task(dir: &Path, title: &str) -> String {
    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "title": title }
        }),
    );
    assert!(!is_error(&resp), "create task failed: {}", get_text(&resp));
    let text = get_text(&resp);
    // "Created task t1: <title> [todo]"
    text.split_whitespace()
        .nth(2)
        .unwrap()
        .trim_end_matches(':')
        .to_string()
}

fn read_task_lock(
    dir: &Path,
    task_id: &str,
) -> (Option<handoff_mcp::storage::tasks::TaskLock>, String) {
    let tasks_dir = dir.join(".handoff").join("tasks");
    let task_dir = handoff_mcp::storage::tasks::find_task_dir_by_id(&tasks_dir, task_id)
        .unwrap()
        .unwrap();
    let (data, status) = handoff_mcp::storage::tasks::read_task(&task_dir)
        .unwrap()
        .unwrap();
    (data.lock, status)
}

fn read_events(dir: &Path) -> Vec<Value> {
    let path = dir.join(".handoff").join("events.jsonl");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid JSON event line"))
        .collect()
}

// -- 1. Two threads claiming the same task concurrently: exactly one wins ---

#[test]
fn two_threads_claiming_same_task_only_one_succeeds() {
    let dir = tempfile::tempdir().expect("tempdir");
    setup_project(dir.path());
    let task_id = create_todo_task(dir.path(), "Contended task");

    let dir_str = dir.path().to_string_lossy().to_string();
    let barrier = std::sync::Arc::new(Barrier::new(2));

    let handles: Vec<_> = (0..2)
        .map(|i| {
            let dir_str = dir_str.clone();
            let task_id = task_id.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                call_tool(
                    "handoff_claim_task",
                    json!({
                        "project_dir": dir_str,
                        "task_id": task_id,
                        "session_id": format!("s-{i}")
                    }),
                )
            })
        })
        .collect();

    let results: Vec<Value> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    let success_count = results.iter().filter(|r| !is_error(r)).count();
    let error_count = results.iter().filter(|r| is_error(r)).count();

    assert_eq!(
        success_count, 1,
        "exactly one claim should succeed, results: {results:?}"
    );
    assert_eq!(error_count, 1, "exactly one claim should fail");

    let failed = results.iter().find(|r| is_error(r)).unwrap();
    assert!(
        get_text(failed).contains("currently claimed"),
        "expected 'currently claimed' error, got: {}",
        get_text(failed)
    );

    // The winning claim actually set a lock on disk and moved the task to
    // in_progress.
    let (lock, status) = read_task_lock(dir.path(), &task_id);
    assert!(
        lock.is_some(),
        "task should carry a lock after one claim wins"
    );
    assert_eq!(status, "in_progress");
}

// -- 2. claim -> release -> re-claim cycle -----------------------------------

#[test]
fn claim_release_reclaim_cycle() {
    let _guard = AGENT_ID_GLOBAL.lock().unwrap_or_else(|e| e.into_inner());

    let dir = tempfile::tempdir().expect("tempdir");
    setup_project(dir.path());
    let task_id = create_todo_task(dir.path(), "Cycle task");

    // Agent A claims.
    handoff_mcp::mcp::router::set_agent_id("agent-a".to_string());
    let claim_a = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id, "session_id": "s-a" }),
    );
    assert!(
        !is_error(&claim_a),
        "agent A claim failed: {}",
        get_text(&claim_a)
    );

    let (lock, status) = read_task_lock(dir.path(), &task_id);
    assert_eq!(lock.unwrap().agent_id, "agent-a");
    assert_eq!(status, "in_progress");

    // Agent A releases.
    let release_a = call_tool(
        "handoff_release_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id }),
    );
    assert!(
        !is_error(&release_a),
        "agent A release failed: {}",
        get_text(&release_a)
    );

    let (lock, status) = read_task_lock(dir.path(), &task_id);
    assert!(lock.is_none(), "lock should be cleared after release");
    assert_eq!(status, "todo");

    // Agent B claims successfully now that the task is free.
    handoff_mcp::mcp::router::set_agent_id("agent-b".to_string());
    let claim_b = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id, "session_id": "s-b" }),
    );
    assert!(
        !is_error(&claim_b),
        "agent B claim failed: {}",
        get_text(&claim_b)
    );

    let (lock, status) = read_task_lock(dir.path(), &task_id);
    assert_eq!(lock.unwrap().agent_id, "agent-b");
    assert_eq!(status, "in_progress");
}

// -- 3. Expired lease is reclaimed after a lazy scan -------------------------

#[test]
fn expired_lease_is_reclaimed_after_lazy_scan() {
    let _guard = AGENT_ID_GLOBAL.lock().unwrap_or_else(|e| e.into_inner());

    let dir = tempfile::tempdir().expect("tempdir");
    setup_project(dir.path());
    let task_id = create_todo_task(dir.path(), "Short lease task");

    // Agent A claims with a 1-second lease.
    handoff_mcp::mcp::router::set_agent_id("agent-a".to_string());
    let claim_a = call_tool(
        "handoff_claim_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task_id": task_id,
            "session_id": "s-a",
            "lease_ttl": 1
        }),
    );
    assert!(
        !is_error(&claim_a),
        "agent A claim failed: {}",
        get_text(&claim_a)
    );

    // Wait for the lease to expire.
    thread::sleep(Duration::from_secs(2));

    // Agent B triggers a lazy scan via handoff_list_tasks (in the lazy-scan
    // allowlist per tests/tool_claim_release.rs::list_tasks_triggers_lazy_scan).
    handoff_mcp::mcp::router::set_agent_id("agent-b".to_string());
    let list = call_tool(
        "handoff_list_tasks",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    assert!(!is_error(&list), "list_tasks failed: {}", get_text(&list));

    let (lock, status) = read_task_lock(dir.path(), &task_id);
    assert!(
        lock.is_none(),
        "expired lease should have been cleared by lazy scan"
    );
    assert_eq!(status, "todo");

    // Agent B can now claim successfully.
    let claim_b = call_tool(
        "handoff_claim_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task_id": task_id,
            "session_id": "s-b"
        }),
    );
    assert!(
        !is_error(&claim_b),
        "agent B claim failed: {}",
        get_text(&claim_b)
    );

    let (lock, status) = read_task_lock(dir.path(), &task_id);
    assert_eq!(lock.unwrap().agent_id, "agent-b");
    assert_eq!(status, "in_progress");
}

// -- 4. update_task on a claimed task by a different agent returns advisory -

#[test]
fn update_task_on_claimed_task_by_different_agent_returns_advisory() {
    let _guard = AGENT_ID_GLOBAL.lock().unwrap_or_else(|e| e.into_inner());

    let dir = tempfile::tempdir().expect("tempdir");
    setup_project(dir.path());
    let task_id = create_todo_task(dir.path(), "Advisory task");

    handoff_mcp::mcp::router::set_agent_id("agent-a".to_string());
    let claim_a = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id, "session_id": "s-a" }),
    );
    assert!(
        !is_error(&claim_a),
        "agent A claim failed: {}",
        get_text(&claim_a)
    );

    handoff_mcp::mcp::router::set_agent_id("agent-b".to_string());
    let update_b = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "id": task_id, "notes": "agent B poking at this" }
        }),
    );
    assert!(
        !is_error(&update_b),
        "agent B update failed: {}",
        get_text(&update_b)
    );

    let text = get_text(&update_b);
    assert!(
        text.contains("Advisory") && text.contains("agent-a"),
        "expected advisory warning naming the claiming agent, got: {text}"
    );

    // The update was still applied (advisory, not a rejection) and the lock
    // is untouched (still agent-a).
    let (lock, _status) = read_task_lock(dir.path(), &task_id);
    assert_eq!(lock.unwrap().agent_id, "agent-a");
}

// -- 5. Full lifecycle produces a correctly-ordered event log ---------------

#[test]
fn full_lifecycle_produces_correct_event_log() {
    let _guard = AGENT_ID_GLOBAL.lock().unwrap_or_else(|e| e.into_inner());

    let dir = tempfile::tempdir().expect("tempdir");
    setup_project(dir.path());
    let task_id = create_todo_task(dir.path(), "Lifecycle task");

    handoff_mcp::mcp::router::set_agent_id("agent-a".to_string());

    let claim = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id, "session_id": "s-a" }),
    );
    assert!(!is_error(&claim), "claim failed: {}", get_text(&claim));

    let update = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "id": task_id, "notes": "working on it" }
        }),
    );
    assert!(!is_error(&update), "update failed: {}", get_text(&update));

    let release = call_tool(
        "handoff_release_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id }),
    );
    assert!(
        !is_error(&release),
        "release failed: {}",
        get_text(&release)
    );

    let events = read_events(dir.path());
    let task_events: Vec<&Value> = events
        .iter()
        .filter(|e| e["task_id"].as_str() == Some(task_id.as_str()))
        .collect();

    let claimed = task_events
        .iter()
        .find(|e| e["event"] == "task.claimed")
        .expect("task.claimed event should be present");
    let released = task_events
        .iter()
        .find(|e| e["event"] == "task.released")
        .expect("task.released event should be present");

    // Every event carries task_id and agent_id.
    for e in &task_events {
        assert_eq!(e["task_id"], task_id);
        assert!(e["agent_id"].is_string(), "event missing agent_id: {e:?}");
    }
    assert_eq!(claimed["agent_id"], "agent-a");
    assert_eq!(released["agent_id"], "agent-a");

    // Chronological order: claimed before released, by timestamp and by
    // position in the append-only log.
    let claimed_ts = claimed["ts"].as_str().unwrap();
    let released_ts = released["ts"].as_str().unwrap();
    assert!(
        claimed_ts <= released_ts,
        "task.claimed ({claimed_ts}) should not be after task.released ({released_ts})"
    );

    let claimed_pos = events.iter().position(|e| std::ptr::eq(e, *claimed));
    let released_pos = events.iter().position(|e| std::ptr::eq(e, *released));
    assert!(
        claimed_pos < released_pos,
        "claim event must precede release event in the log"
    );
}

// -- 6. done transition clears the lock and logs a task.released event ------

#[test]
fn done_transition_clears_lock_and_logs_event() {
    let _guard = AGENT_ID_GLOBAL.lock().unwrap_or_else(|e| e.into_inner());

    let dir = tempfile::tempdir().expect("tempdir");
    setup_project(dir.path());
    let task_id = create_todo_task(dir.path(), "Finish me");

    handoff_mcp::mcp::router::set_agent_id("agent-a".to_string());
    let claim = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id, "session_id": "s-a" }),
    );
    assert!(!is_error(&claim), "claim failed: {}", get_text(&claim));

    let done = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "id": task_id, "status": "done" }
        }),
    );
    assert!(
        !is_error(&done),
        "done transition failed: {}",
        get_text(&done)
    );

    let (lock, status) = read_task_lock(dir.path(), &task_id);
    assert!(lock.is_none(), "lock should be cleared on done transition");
    assert_eq!(status, "done");

    let events = read_events(dir.path());
    let released = events
        .iter()
        .find(|e| e["task_id"].as_str() == Some(task_id.as_str()) && e["event"] == "task.released");
    assert!(
        released.is_some(),
        "expected a task.released event for the done transition, got events: {events:?}"
    );
}

// -- 7. update_task extends the lease when the caller owns the lock ---------

#[test]
fn update_task_extends_lease_when_agent_matches() {
    let _guard = AGENT_ID_GLOBAL.lock().unwrap_or_else(|e| e.into_inner());

    let dir = tempfile::tempdir().expect("tempdir");
    setup_project(dir.path());
    let task_id = create_todo_task(dir.path(), "Long running task");

    handoff_mcp::mcp::router::set_agent_id("agent-a".to_string());
    let claim = call_tool(
        "handoff_claim_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task_id": task_id,
            "session_id": "s-a",
            "lease_ttl": 60
        }),
    );
    assert!(!is_error(&claim), "claim failed: {}", get_text(&claim));

    let (lock_before, _) = read_task_lock(dir.path(), &task_id);
    let expires_before = lock_before.unwrap().lease_expires_at;

    // Ensure a visible clock tick between claim and update.
    thread::sleep(Duration::from_millis(1100));

    let update = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "id": task_id, "notes": "still working" }
        }),
    );
    assert!(!is_error(&update), "update failed: {}", get_text(&update));

    let (lock_after, _) = read_task_lock(dir.path(), &task_id);
    let expires_after = lock_after.unwrap().lease_expires_at;

    assert!(
        expires_after > expires_before,
        "lease should have been extended: before={expires_before} after={expires_after}"
    );
}
