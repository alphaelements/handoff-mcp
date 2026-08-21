use serde_json::{json, Value};
use tempfile::TempDir;

/// `handoff_load_context` registers this *process's* agent identity in a
/// single process-wide global (`crate::mcp::router::AGENT_ID` — one running
/// MCP server serves exactly one agent for its lifetime). Test binaries run
/// `#[test]`s concurrently in one process, so any two tests that both call
/// `handoff_load_context` race on that global: a claim captured under one
/// test's identity can be raced by another thread's `handoff_load_context`
/// before that same test's release re-reads the (now different) identity,
/// causing a spurious ownership-mismatch failure that has nothing to do with
/// the behavior under test. Serialize the handful of tests that call
/// `handoff_load_context` against each other with this lock so the global
/// is stable for the duration of each such test.
static AGENT_ID_GLOBAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

fn create_task(dir: &TempDir, title: &str) -> String {
    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": title }
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    // "Created task t1: <title> [todo]"
    text.split_whitespace()
        .nth(2)
        .unwrap()
        .trim_end_matches(':')
        .to_string()
}

#[test]
fn claim_task_via_tool_call_sets_lock() {
    let dir = setup_project();
    let task_id = create_task(&dir, "Claimable task");

    let resp = call_tool(
        "handoff_claim_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task_id": task_id,
            "session_id": "s-1"
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(parsed["lock"]["agent_id"], "unknown");
    assert_eq!(parsed["lock"]["session_id"], "s-1");

    let tasks_dir = dir.path().join(".handoff").join("tasks");
    let task_dir = handoff_mcp::storage::tasks::find_task_dir_by_id(&tasks_dir, &task_id)
        .unwrap()
        .unwrap();
    let (_, status) = handoff_mcp::storage::tasks::read_task(&task_dir)
        .unwrap()
        .unwrap();
    assert_eq!(status, "in_progress");
}

#[test]
fn claim_task_twice_returns_error() {
    let dir = setup_project();
    let task_id = create_task(&dir, "Double claim task");

    let first = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id }),
    );
    assert!(!is_error(&first), "error: {}", get_text(&first));

    let second = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id }),
    );
    assert!(is_error(&second), "expected error: {}", get_text(&second));
}

#[test]
fn release_task_via_tool_call_clears_lock_and_reverts_status() {
    let dir = setup_project();
    let task_id = create_task(&dir, "Release me");

    // Register this process's agent identity first, as a real caller would
    // (handoff_load_context before any claim/release). Claiming and
    // releasing under the unresolved "unknown" sentinel identity is
    // rejected on release — see
    // release_task_by_unknown_identity_is_rejected_even_if_lock_owner_is_also_unknown
    // in tests/storage_tasks.rs.
    //
    // Held for the whole claim->release window: see AGENT_ID_GLOBAL's doc
    // comment for why this must be serialized against other tests that also
    // call handoff_load_context.
    let _agent_id_guard = AGENT_ID_GLOBAL.lock().unwrap_or_else(|e| e.into_inner());

    let load = call_tool(
        "handoff_load_context",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    assert!(!is_error(&load), "error: {}", get_text(&load));

    let claim = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id }),
    );
    assert!(!is_error(&claim), "error: {}", get_text(&claim));

    let release = call_tool(
        "handoff_release_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task_id": task_id,
            "reason": "pausing work"
        }),
    );
    assert!(!is_error(&release), "error: {}", get_text(&release));
    assert!(get_text(&release).contains("released successfully"));
    assert!(get_text(&release).contains("pausing work"));

    let list = call_tool(
        "handoff_get_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id }),
    );
    assert!(!is_error(&list), "error: {}", get_text(&list));
    let parsed: Value = serde_json::from_str(&get_text(&list)).unwrap();
    assert_eq!(parsed["status"], "todo");

    // handoff_get_task does not surface `lock` in its response shape, so
    // assert the on-disk lock state directly via the storage layer.
    let tasks_dir = dir.path().join(".handoff").join("tasks");
    let task_dir = handoff_mcp::storage::tasks::find_task_dir_by_id(&tasks_dir, &task_id)
        .unwrap()
        .unwrap();
    let (data, status) = handoff_mcp::storage::tasks::read_task(&task_dir)
        .unwrap()
        .unwrap();
    assert!(data.lock.is_none());
    assert_eq!(status, "todo");
}

#[test]
fn done_transition_clears_lock() {
    let dir = setup_project();
    let task_id = create_task(&dir, "Finish me");

    let claim = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id }),
    );
    assert!(!is_error(&claim), "error: {}", get_text(&claim));

    let done = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "id": task_id, "status": "done" }
        }),
    );
    assert!(!is_error(&done), "error: {}", get_text(&done));

    // handoff_get_task does not surface `lock` in its response shape, so
    // assert the on-disk lock state directly via the storage layer.
    let tasks_dir = dir.path().join(".handoff").join("tasks");
    let task_dir = handoff_mcp::storage::tasks::find_task_dir_by_id(&tasks_dir, &task_id)
        .unwrap()
        .unwrap();
    let (data, status) = handoff_mcp::storage::tasks::read_task(&task_dir)
        .unwrap()
        .unwrap();
    assert!(data.lock.is_none(), "lock should be cleared on done");
    assert_eq!(status, "done");
}

/// Write an already-expired lock directly onto a task's on-disk file,
/// bypassing `claim_task` (which would refuse to let a lease expire in the
/// past). Used to set up "lazy scan should revert this" fixtures without
/// waiting on a real clock.
fn write_expired_lock(dir: &TempDir, task_id: &str) {
    let tasks_dir = dir.path().join(".handoff").join("tasks");
    let task_dir = handoff_mcp::storage::tasks::find_task_dir_by_id(&tasks_dir, task_id)
        .unwrap()
        .unwrap();
    let (mut data, _status) = handoff_mcp::storage::tasks::read_task(&task_dir)
        .unwrap()
        .unwrap();
    data.lock = Some(handoff_mcp::storage::tasks::TaskLock {
        agent_id: "agent-old".to_string(),
        session_id: "session-old".to_string(),
        claimed_at: "2020-01-01T00:00:00+00:00".to_string(),
        lease_expires_at: "2020-01-01T00:30:00+00:00".to_string(),
        lease_ttl_seconds: 1800,
    });
    handoff_mcp::storage::tasks::write_task(&task_dir, "in_progress", &data).unwrap();
}

fn assert_reverted_to_todo(dir: &TempDir, task_id: &str) {
    let tasks_dir = dir.path().join(".handoff").join("tasks");
    let task_dir = handoff_mcp::storage::tasks::find_task_dir_by_id(&tasks_dir, task_id)
        .unwrap()
        .unwrap();
    let (data, status) = handoff_mcp::storage::tasks::read_task(&task_dir)
        .unwrap()
        .unwrap();
    assert!(data.lock.is_none(), "expired lock should have been cleared");
    assert_eq!(status, "todo", "task should have reverted to todo");
}

fn assert_still_locked(dir: &TempDir, task_id: &str) {
    let tasks_dir = dir.path().join(".handoff").join("tasks");
    let task_dir = handoff_mcp::storage::tasks::find_task_dir_by_id(&tasks_dir, task_id)
        .unwrap()
        .unwrap();
    let (data, status) = handoff_mcp::storage::tasks::read_task(&task_dir)
        .unwrap()
        .unwrap();
    assert!(data.lock.is_some(), "lock should not have been touched");
    assert_eq!(status, "in_progress");
}

#[test]
fn claim_task_triggers_lazy_scan_of_other_expired_leases() {
    let dir = setup_project();
    let expired_id = create_task(&dir, "Expired elsewhere");
    let fresh_id = create_task(&dir, "Claim me");
    write_expired_lock(&dir, &expired_id);

    let resp = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": fresh_id }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));

    assert_reverted_to_todo(&dir, &expired_id);
}

#[test]
fn release_task_triggers_lazy_scan_of_other_expired_leases() {
    let dir = setup_project();
    let expired_id = create_task(&dir, "Expired elsewhere");
    let claimed_id = create_task(&dir, "Claimed then released");
    write_expired_lock(&dir, &expired_id);

    // Register a real agent identity first (see comment in
    // release_task_via_tool_call_clears_lock_and_reverts_status) so the
    // subsequent release is not rejected as an unresolved-identity release.
    // Held for the whole claim->release window: see AGENT_ID_GLOBAL's doc
    // comment.
    let _agent_id_guard = AGENT_ID_GLOBAL.lock().unwrap_or_else(|e| e.into_inner());

    let load = call_tool(
        "handoff_load_context",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    assert!(!is_error(&load), "error: {}", get_text(&load));

    let claim = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": claimed_id }),
    );
    assert!(!is_error(&claim), "error: {}", get_text(&claim));

    let release = call_tool(
        "handoff_release_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": claimed_id }),
    );
    assert!(!is_error(&release), "error: {}", get_text(&release));

    assert_reverted_to_todo(&dir, &expired_id);
}

#[test]
fn list_tasks_triggers_lazy_scan() {
    let dir = setup_project();
    let expired_id = create_task(&dir, "Expired task");
    write_expired_lock(&dir, &expired_id);

    let resp = call_tool(
        "handoff_list_tasks",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));

    assert_reverted_to_todo(&dir, &expired_id);
}

#[test]
fn dashboard_triggers_lazy_scan() {
    let dir = setup_project();
    let expired_id = create_task(&dir, "Expired task");
    write_expired_lock(&dir, &expired_id);

    // handoff_dashboard discovers projects *nested inside* each scan_dir
    // (it does not treat the scan_dir itself as a project), so scan from
    // the project directory's parent.
    let scan_root = dir.path().parent().unwrap().to_string_lossy().to_string();
    let resp = call_tool("handoff_dashboard", json!({ "scan_dirs": [scan_root] }));
    assert!(!is_error(&resp), "error: {}", get_text(&resp));

    assert_reverted_to_todo(&dir, &expired_id);
}

#[test]
fn load_context_triggers_lazy_scan() {
    // See AGENT_ID_GLOBAL's doc comment: this call mutates the process-wide
    // agent identity global, which other tests capture-and-compare across a
    // claim/release window. Serialize against them.
    let _agent_id_guard = AGENT_ID_GLOBAL.lock().unwrap_or_else(|e| e.into_inner());

    let dir = setup_project();
    let expired_id = create_task(&dir, "Expired task");
    write_expired_lock(&dir, &expired_id);

    let resp = call_tool(
        "handoff_load_context",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));

    assert_reverted_to_todo(&dir, &expired_id);
}

#[test]
fn get_task_does_not_trigger_lazy_scan() {
    let dir = setup_project();
    let expired_id = create_task(&dir, "Expired task");
    write_expired_lock(&dir, &expired_id);

    let resp = call_tool(
        "handoff_get_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": expired_id }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));

    // handoff_get_task is not in the lazy-scan allowlist (spec 3.3.5/7.2):
    // the expired lock must remain untouched.
    assert_still_locked(&dir, &expired_id);
}
