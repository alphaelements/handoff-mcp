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

fn create_task_with_scope(dir: &TempDir, title: &str, scope_paths: &[&str]) -> String {
    let resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": title, "scope_paths": scope_paths }
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
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

/// `handoff_update_task` must accept `task.scope_paths` on create and
/// `handoff_get_task` must surface it back unchanged.
#[test]
fn update_task_sets_scope_paths_and_get_task_reads_them_back() {
    let dir = setup_project();
    let task_id = create_task_with_scope(&dir, "Scoped task", &["src/main.rs", "src/lib.rs"]);

    let resp = call_tool(
        "handoff_get_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let scope_paths: Vec<&str> = parsed["scope_paths"]
        .as_array()
        .expect("scope_paths should be present")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(scope_paths, vec!["src/main.rs", "src/lib.rs"]);
}

/// Updating an existing task with `scope_paths` replaces the stored list,
/// mirroring how `labels`/`links` are replaced on update.
#[test]
fn update_task_replaces_scope_paths_on_existing_task() {
    let dir = setup_project();
    let task_id = create_task_with_scope(&dir, "Scoped task", &["src/old.rs"]);

    let update = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "id": task_id, "scope_paths": ["src/new.rs"] }
        }),
    );
    assert!(!is_error(&update), "error: {}", get_text(&update));

    let resp = call_tool(
        "handoff_get_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id }),
    );
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let scope_paths: Vec<&str> = parsed["scope_paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(scope_paths, vec!["src/new.rs"]);
}

/// `handoff_get_task` must surface the task's current lock so a caller can
/// see ownership/lease info without dropping to the storage layer directly.
#[test]
fn get_task_includes_lock_field() {
    let dir = setup_project();
    let task_id = create_task(&dir, "Locked task");

    let claim = call_tool(
        "handoff_claim_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task_id": task_id,
            "session_id": "s-lock-test"
        }),
    );
    assert!(!is_error(&claim), "error: {}", get_text(&claim));

    let resp = call_tool(
        "handoff_get_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();

    assert!(
        parsed.get("lock").is_some(),
        "response should include a lock field: {parsed}"
    );
    assert_eq!(parsed["lock"]["session_id"], "s-lock-test");
    assert!(parsed["lock"]["agent_id"].is_string());
}

/// An unclaimed task must report `"lock": null`, not omit the field.
#[test]
fn get_task_shows_null_lock_for_unclaimed_task() {
    let dir = setup_project();
    let task_id = create_task(&dir, "Unclaimed task");

    let resp = call_tool(
        "handoff_get_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();

    assert!(
        parsed.get("lock").is_some(),
        "response should include a lock key even when unclaimed: {parsed}"
    );
    assert!(
        parsed["lock"].is_null(),
        "expected lock: null for an unclaimed task, got {}",
        parsed["lock"]
    );
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

    // Assert the on-disk lock state directly via the storage layer (in
    // addition to `handoff_get_task`'s own `lock` field, covered by
    // get_task_includes_lock_field / get_task_shows_null_lock_for_unclaimed_task).
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

    // Assert the on-disk lock state directly via the storage layer (in
    // addition to `handoff_get_task`'s own `lock` field, covered by
    // get_task_includes_lock_field / get_task_shows_null_lock_for_unclaimed_task).
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

/// Same bug class as auto_schedule_agent_capacity_reflects_a_real_claim, but
/// via the done-transition path (handoff_update_task status: done), which
/// clears the lock through a separate code path from handoff_release_task.
#[test]
fn done_transition_removes_task_from_claiming_agents_claimed_tasks() {
    // See AGENT_ID_GLOBAL's doc comment.
    let _agent_id_guard = AGENT_ID_GLOBAL.lock().unwrap_or_else(|e| e.into_inner());

    let dir = setup_project();
    let task_id = create_task(&dir, "Finish me (agent bookkeeping)");

    let load = call_tool(
        "handoff_load_context",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    assert!(!is_error(&load), "error: {}", get_text(&load));
    let agent_id = serde_json::from_str::<Value>(&get_text(&load)).unwrap()["agent_id"]
        .as_str()
        .expect("handoff_load_context response should carry agent_id")
        .to_string();

    let claim = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id }),
    );
    assert!(!is_error(&claim), "error: {}", get_text(&claim));

    let schedule_before = call_tool(
        "handoff_auto_schedule",
        json!({ "project_dir": dir.path().to_string_lossy(), "dry_run": true }),
    );
    let before_parsed: Value = serde_json::from_str(&get_text(&schedule_before)).unwrap();
    let claimed_before = before_parsed["agent_capacity"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["agent_id"] == agent_id)
        .unwrap()["claimed"]
        .as_u64()
        .unwrap();
    assert_eq!(claimed_before, 1);

    let done = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "id": task_id, "status": "done" }
        }),
    );
    assert!(!is_error(&done), "error: {}", get_text(&done));

    let schedule_after = call_tool(
        "handoff_auto_schedule",
        json!({ "project_dir": dir.path().to_string_lossy(), "dry_run": true }),
    );
    let after_parsed: Value = serde_json::from_str(&get_text(&schedule_after)).unwrap();
    let claimed_after = after_parsed["agent_capacity"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["agent_id"] == agent_id)
        .unwrap()["claimed"]
        .as_u64()
        .unwrap();
    assert_eq!(
        claimed_after, 0,
        "task should no longer count as claimed after the done transition"
    );
}

// -- t250.6: agent_capacity in handoff_auto_schedule ------------------------

/// End-to-end reproduction of the round-1 rework finding: a real
/// `handoff_load_context -> handoff_claim_task -> handoff_auto_schedule`
/// sequence (exactly as run by the integration tester) must report the
/// claiming agent's `claimed` count as 1, not 0. Exercises the real
/// production write path (no synthetic AgentRecord construction).
#[test]
fn auto_schedule_agent_capacity_reflects_a_real_claim() {
    // See AGENT_ID_GLOBAL's doc comment: handoff_load_context mutates the
    // process-wide agent identity global.
    let _agent_id_guard = AGENT_ID_GLOBAL.lock().unwrap_or_else(|e| e.into_inner());

    let dir = setup_project();
    let task_id = create_task(&dir, "Claim me for capacity check");

    let load = call_tool(
        "handoff_load_context",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    assert!(!is_error(&load), "error: {}", get_text(&load));
    let load_parsed: Value = serde_json::from_str(&get_text(&load)).unwrap();
    let agent_id = load_parsed["agent_id"]
        .as_str()
        .expect("handoff_load_context response should carry agent_id")
        .to_string();

    let claim = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id }),
    );
    assert!(!is_error(&claim), "error: {}", get_text(&claim));
    let claim_parsed: Value = serde_json::from_str(&get_text(&claim)).unwrap();
    let claiming_agent_id = claim_parsed["lock"]["agent_id"]
        .as_str()
        .expect("claim response should carry lock.agent_id")
        .to_string();
    // The identity used for the claim must match handoff_load_context's.
    assert_eq!(agent_id, claiming_agent_id);

    let schedule = call_tool(
        "handoff_auto_schedule",
        json!({ "project_dir": dir.path().to_string_lossy(), "dry_run": true }),
    );
    assert!(!is_error(&schedule), "error: {}", get_text(&schedule));
    let schedule_parsed: Value = serde_json::from_str(&get_text(&schedule)).unwrap();

    let capacity_entries = schedule_parsed["agent_capacity"]
        .as_array()
        .expect("agent_capacity should be present");
    let entry = capacity_entries
        .iter()
        .find(|e| e["agent_id"] == claiming_agent_id)
        .unwrap_or_else(|| {
            panic!(
                "no agent_capacity entry for claiming agent {claiming_agent_id}: {capacity_entries:?}"
            )
        });
    assert_eq!(
        entry["claimed"], 1,
        "agent_capacity.claimed must reflect the just-claimed task, got: {entry}"
    );
    assert_eq!(
        entry["available"],
        entry["max_concurrent"].as_u64().unwrap() - 1
    );
}

// -- t250.4: scope_paths conflict detection on claim -----------------------

/// A task with no scope_paths must claim exactly as before: no warnings key,
/// no regression in behavior (done_criteria #4).
#[test]
fn claim_with_no_scope_paths_has_no_warnings() {
    let dir = setup_project();
    let task_id = create_task(&dir, "No scope task");

    let resp = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert!(
        parsed.get("warnings").is_none() || parsed["warnings"].as_array().unwrap().is_empty(),
        "unscoped claim should carry no warnings: {parsed}"
    );
}

/// Two tasks with disjoint scope_paths must claim without any warning.
#[test]
fn claim_with_disjoint_scope_paths_has_no_warnings() {
    let dir = setup_project();
    let other_id = create_task_with_scope(&dir, "Other task", &["src/other.rs"]);
    let task_id = create_task_with_scope(&dir, "This task", &["src/main.rs"]);

    let claim_other = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": other_id, "session_id": "s-other" }),
    );
    assert!(!is_error(&claim_other), "error: {}", get_text(&claim_other));

    let resp = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id, "session_id": "s-this" }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert!(
        parsed.get("warnings").is_none() || parsed["warnings"].as_array().unwrap().is_empty(),
        "disjoint scope claim should carry no warnings: {parsed}"
    );
}

/// Claiming a task whose scope_paths exactly match a file already in scope
/// of another *active* (in_progress + locked) task must produce a "warn"
/// level advisory warning naming the conflicting task, without blocking the
/// claim.
#[test]
fn claim_with_same_file_scope_conflict_returns_warn_level_warning() {
    let dir = setup_project();
    let other_id = create_task_with_scope(&dir, "Other task", &["src/main.rs"]);
    let task_id = create_task_with_scope(&dir, "This task", &["src/main.rs"]);

    let claim_other = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": other_id, "session_id": "s-other" }),
    );
    assert!(!is_error(&claim_other), "error: {}", get_text(&claim_other));

    let resp = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id, "session_id": "s-this" }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();

    // Claim itself must still succeed (advisory only, never blocking).
    assert!(parsed["lock"]["session_id"] == "s-this");

    let warnings = parsed["warnings"]
        .as_array()
        .expect("expected a warnings array for same-file scope conflict");
    assert_eq!(warnings.len(), 1, "warnings: {warnings:?}");
    assert_eq!(warnings[0]["level"], "warn");
    let message = warnings[0]["message"].as_str().unwrap();
    assert!(message.contains("src/main.rs"), "message: {message}");
    assert!(message.contains(&other_id), "message: {message}");
}

/// Claiming a task whose scope_paths name a directory that contains another
/// active task's exact-file scope_paths (directory containment, not an
/// exact file match) must produce an "info" level advisory warning, not
/// "warn". Two distinct sibling files under the same parent (e.g.
/// "src/a.rs" vs "src/b.rs") are a separate, non-overlapping case and must
/// not warn at all — covered by claim_with_disjoint_scope_paths_has_no_warnings.
#[test]
fn claim_with_same_directory_scope_conflict_returns_info_level_warning() {
    let dir = setup_project();
    let other_id = create_task_with_scope(&dir, "Other task", &["src/mod_a.rs"]);
    let task_id = create_task_with_scope(&dir, "This task", &["src/"]);

    let claim_other = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": other_id, "session_id": "s-other" }),
    );
    assert!(!is_error(&claim_other), "error: {}", get_text(&claim_other));

    let resp = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id, "session_id": "s-this" }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();

    let warnings = parsed["warnings"]
        .as_array()
        .expect("expected a warnings array for same-directory scope conflict");
    assert_eq!(warnings.len(), 1, "warnings: {warnings:?}");
    assert_eq!(warnings[0]["level"], "info");
    let message = warnings[0]["message"].as_str().unwrap();
    assert!(message.contains(&other_id), "message: {message}");
}

/// A conflict must only be raised against *active* tasks (status
/// in_progress AND currently locked) — a task that was claimed and then
/// released must not still trigger a conflict warning.
#[test]
fn claim_does_not_warn_against_released_task_with_overlapping_scope() {
    // A real agent identity is required to release a claim (see
    // AGENT_ID_GLOBAL's doc comment: this mutates the process-wide agent
    // identity global, so serialize against other tests that do the same).
    let _agent_id_guard = AGENT_ID_GLOBAL.lock().unwrap_or_else(|e| e.into_inner());

    let dir = setup_project();
    let other_id = create_task_with_scope(&dir, "Other task", &["src/main.rs"]);
    let task_id = create_task_with_scope(&dir, "This task", &["src/main.rs"]);

    let load = call_tool(
        "handoff_load_context",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    assert!(!is_error(&load), "error: {}", get_text(&load));

    let claim_other = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": other_id, "session_id": "s-other" }),
    );
    assert!(!is_error(&claim_other), "error: {}", get_text(&claim_other));

    let release_other = call_tool(
        "handoff_release_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": other_id }),
    );
    assert!(
        !is_error(&release_other),
        "error: {}",
        get_text(&release_other)
    );

    let resp = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": task_id, "session_id": "s-this" }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert!(
        parsed.get("warnings").is_none() || parsed["warnings"].as_array().unwrap().is_empty(),
        "released task's old scope should not trigger a conflict: {parsed}"
    );
}
