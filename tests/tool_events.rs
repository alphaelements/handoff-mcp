//! `handoff_events` tool (spec §3.6.3, §4.2 FR-2.5): query
//! `.handoff/events.jsonl` with `since`/`task_id`/`agent_id`/`event_type`/
//! `limit` filters.

use serde_json::{json, Value};
use tempfile::TempDir;

/// `handoff_load_context` registers this *process's* agent identity in a
/// single process-wide global (see `tests/tool_claim_release.rs` for the
/// full rationale). Serialize the handful of tests here that call
/// `handoff_load_context` against each other.
static AGENT_ID_GLOBAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn send(input: &str) -> Option<Value> {
    let result = handoff_mcp::mcp::protocol::process_line(input)?;
    Some(serde_json::from_str(&result).expect("response should be valid JSON"))
}

fn setup() -> TempDir {
    tempfile::tempdir().expect("failed to create temp dir")
}

fn call(name: &str, arguments: Value) -> Value {
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": name,
            "arguments": arguments
        }
    });
    send(&req.to_string()).expect("should return response")
}

fn text_of(resp: &Value) -> Value {
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("expected text content");
    serde_json::from_str(text).expect("expected valid JSON text")
}

fn init(dir: &TempDir) -> String {
    let project_dir = dir.path().to_string_lossy().to_string();
    call(
        "handoff_init",
        json!({
            "project_dir": project_dir,
            "project_name": "events-test"
        }),
    );
    call(
        "handoff_update_config",
        json!({
            "project_dir": project_dir,
            "updates": { "settings.require_estimate_hours": false }
        }),
    );
    project_dir
}

/// Create a task via `handoff_update_task` and return its auto-generated id,
/// parsed from the "Created task <id>: <title> [<status>]" response text.
fn create_task(project_dir: &str, title: &str) -> String {
    let resp = call(
        "handoff_update_task",
        json!({ "project_dir": project_dir, "task": { "title": title } }),
    );
    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "handoff_update_task failed: {resp}"
    );
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    text.split_whitespace()
        .nth(2)
        .unwrap()
        .trim_end_matches(':')
        .to_string()
}

fn write_raw_event(dir: &TempDir, line: &str) {
    let path = dir.path().join(".handoff/events.jsonl");
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(f, "{line}").unwrap();
}

/// No events.jsonl yet: `handoff_events` must return an empty array, not
/// error.
#[test]
fn events_empty_when_no_log_yet() {
    let dir = setup();
    let project_dir = init(&dir);

    let resp = call("handoff_events", json!({ "project_dir": project_dir }));
    let parsed = text_of(&resp);
    assert_eq!(parsed["total"], 0);
    assert_eq!(parsed["events"].as_array().unwrap().len(), 0);
}

/// `handoff_claim_task` / `handoff_release_task` already append events
/// (Phase 1); `handoff_events` must surface them with no filters.
#[test]
fn events_lists_claim_and_release_events() {
    let _guard = AGENT_ID_GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = setup();
    let project_dir = init(&dir);
    // A resolved caller identity is required for handoff_release_task (see
    // storage::tasks::release_task's UNKNOWN_IDENTITY rejection).
    call(
        "handoff_load_context",
        json!({ "project_dir": project_dir }),
    );

    let task_id = create_task(&project_dir, "Test task");

    call(
        "handoff_claim_task",
        json!({ "project_dir": project_dir, "task_id": task_id, "session_id": "s-1" }),
    );
    let release_resp = call(
        "handoff_release_task",
        json!({ "project_dir": project_dir, "task_id": task_id }),
    );
    assert!(
        !release_resp["result"]["isError"].as_bool().unwrap_or(false),
        "handoff_release_task failed: {release_resp}"
    );

    let resp = call("handoff_events", json!({ "project_dir": project_dir }));
    let parsed = text_of(&resp);
    let events = parsed["events"].as_array().unwrap();
    assert!(
        events.len() >= 2,
        "expected at least claim+release events: {parsed}"
    );
    let kinds: Vec<&str> = events
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"task.claimed"));
    assert!(kinds.contains(&"task.released"));
}

/// `task_id` filter narrows to events for a single task.
#[test]
fn events_filters_by_task_id() {
    let dir = setup();
    let project_dir = init(&dir);

    let task1_id = create_task(&project_dir, "T1");
    let task2_id = create_task(&project_dir, "T2");
    call(
        "handoff_claim_task",
        json!({ "project_dir": project_dir, "task_id": task1_id, "session_id": "s-1" }),
    );
    call(
        "handoff_claim_task",
        json!({ "project_dir": project_dir, "task_id": task2_id, "session_id": "s-1" }),
    );

    let resp = call(
        "handoff_events",
        json!({ "project_dir": project_dir, "task_id": task2_id }),
    );
    let parsed = text_of(&resp);
    let events = parsed["events"].as_array().unwrap();
    assert!(!events.is_empty());
    for e in events {
        assert_eq!(e["task_id"], task2_id);
    }
}

/// `event_type` filter narrows to a single event kind.
#[test]
fn events_filters_by_event_type() {
    let _guard = AGENT_ID_GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = setup();
    let project_dir = init(&dir);
    call(
        "handoff_load_context",
        json!({ "project_dir": project_dir }),
    );

    let task_id = create_task(&project_dir, "T1");
    call(
        "handoff_claim_task",
        json!({ "project_dir": project_dir, "task_id": task_id, "session_id": "s-1" }),
    );
    let release_resp = call(
        "handoff_release_task",
        json!({ "project_dir": project_dir, "task_id": task_id }),
    );
    assert!(
        !release_resp["result"]["isError"].as_bool().unwrap_or(false),
        "handoff_release_task failed: {release_resp}"
    );

    let resp = call(
        "handoff_events",
        json!({ "project_dir": project_dir, "event_type": "task.released" }),
    );
    let parsed = text_of(&resp);
    let events = parsed["events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event"], "task.released");
}

/// `limit` truncates to the most recent N matching events.
#[test]
fn events_applies_limit() {
    let dir = setup();
    let project_dir = init(&dir);

    for i in 0..5 {
        write_raw_event(
            &dir,
            &format!(
                "{{\"ts\":\"2026-08-21T00:0{i}:00Z\",\"event\":\"task.claimed\",\"task_id\":\"t{i}\",\"agent_id\":\"a1\"}}"
            ),
        );
    }

    let resp = call(
        "handoff_events",
        json!({ "project_dir": project_dir, "limit": 2 }),
    );
    let parsed = text_of(&resp);
    let events = parsed["events"].as_array().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["task_id"], "t3");
    assert_eq!(events[1]["task_id"], "t4");
}

/// A legacy Phase-1-style events.jsonl (no new event kinds present) must
/// still be readable via `handoff_events`.
#[test]
fn events_reads_legacy_events_jsonl() {
    let dir = setup();
    let project_dir = init(&dir);

    write_raw_event(
        &dir,
        "{\"ts\":\"2026-08-21T00:00:00Z\",\"event\":\"task.claimed\",\"task_id\":\"t1\",\"agent_id\":\"a1\",\"session_id\":\"s1\",\"detail\":\"lease_ttl=1800\"}",
    );

    let resp = call("handoff_events", json!({ "project_dir": project_dir }));
    let parsed = text_of(&resp);
    let events = parsed["events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event"], "task.claimed");
}

/// `handoff_load_context`'s first-time agent registration must record an
/// `agent.registered` event, visible via `handoff_events`.
#[test]
fn load_context_records_agent_registered_event() {
    let _guard = AGENT_ID_GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = setup();
    let project_dir = init(&dir);

    call(
        "handoff_load_context",
        json!({ "project_dir": project_dir }),
    );

    let resp = call(
        "handoff_events",
        json!({ "project_dir": project_dir, "event_type": "agent.registered" }),
    );
    let parsed = text_of(&resp);
    let events = parsed["events"].as_array().unwrap();
    assert_eq!(
        events.len(),
        1,
        "expected exactly one agent.registered event: {parsed}"
    );
}

/// A second `handoff_load_context` call from the *same* agent identity
/// (reconnect) must not append another `agent.registered` event.
#[test]
fn load_context_reconnect_does_not_duplicate_agent_registered_event() {
    let _agent = PinnedAgent::new("s-events-reconnect-test");

    let dir = setup();
    let project_dir = init(&dir);

    call(
        "handoff_load_context",
        json!({ "project_dir": project_dir }),
    );
    call(
        "handoff_load_context",
        json!({ "project_dir": project_dir }),
    );

    let resp = call(
        "handoff_events",
        json!({ "project_dir": project_dir, "event_type": "agent.registered" }),
    );
    let parsed = text_of(&resp);
    let events = parsed["events"].as_array().unwrap();
    assert_eq!(
        events.len(),
        1,
        "reconnect must not duplicate agent.registered: {parsed}"
    );
}

/// `handoff_save_context` with `session_status='active'` creating a brand
/// new session must record a `session.created` event.
#[test]
fn save_context_records_session_created_event() {
    let dir = setup();
    let project_dir = init(&dir);

    call(
        "handoff_save_context",
        json!({
            "project_dir": project_dir,
            "session_status": "active",
            "summary": "starting work"
        }),
    );

    let resp = call(
        "handoff_events",
        json!({ "project_dir": project_dir, "event_type": "session.created" }),
    );
    let parsed = text_of(&resp);
    let events = parsed["events"].as_array().unwrap();
    assert_eq!(
        events.len(),
        1,
        "expected one session.created event: {parsed}"
    );
}

/// Closing an active session (default `session_status`) must record a
/// `session.closed` event.
#[test]
fn save_context_records_session_closed_event() {
    let dir = setup();
    let project_dir = init(&dir);

    call(
        "handoff_save_context",
        json!({
            "project_dir": project_dir,
            "session_status": "active",
            "summary": "starting work"
        }),
    );
    call(
        "handoff_save_context",
        json!({
            "project_dir": project_dir,
            "summary": "wrapping up"
        }),
    );

    let resp = call(
        "handoff_events",
        json!({ "project_dir": project_dir, "event_type": "session.closed" }),
    );
    let parsed = text_of(&resp);
    let events = parsed["events"].as_array().unwrap();
    assert_eq!(
        events.len(),
        1,
        "expected one session.closed event: {parsed}"
    );
}

/// Creating a session directly as "closed" (never active) must NOT record a
/// `session.closed` event — there was no active->closed transition.
#[test]
fn save_context_new_directly_closed_session_does_not_record_closed_event() {
    let dir = setup();
    let project_dir = init(&dir);

    call(
        "handoff_save_context",
        json!({
            "project_dir": project_dir,
            "summary": "one-shot closed session"
        }),
    );

    let resp = call(
        "handoff_events",
        json!({ "project_dir": project_dir, "event_type": "session.closed" }),
    );
    let parsed = text_of(&resp);
    let events = parsed["events"].as_array().unwrap();
    assert_eq!(
        events.len(),
        0,
        "no active session existed to close: {parsed}"
    );
}

/// Holds `AGENT_ID_GLOBAL` and pins the process-wide agent identity to `id`
/// for the guard's lifetime, then restores the unregistered (`None`) state.
struct PinnedAgent {
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl PinnedAgent {
    fn new(id: &str) -> Self {
        let lock = AGENT_ID_GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        handoff_mcp::mcp::router::set_agent_id(id.to_string());
        Self { _lock: lock }
    }
}

impl Drop for PinnedAgent {
    fn drop(&mut self) {
        handoff_mcp::mcp::router::reset_agent_id_for_test();
    }
}

fn events_of(project_dir: &str, event_type: &str) -> Vec<Value> {
    let resp = call(
        "handoff_events",
        json!({ "project_dir": project_dir, "event_type": event_type }),
    );
    text_of(&resp)["events"].as_array().unwrap().clone()
}

/// The `session_id` out of a `save_context` reply ("Session ID: <id>").
fn saved_session_id(resp: &Value) -> String {
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    text.lines()
        .find_map(|l| l.strip_prefix("Session ID: "))
        .unwrap_or_else(|| panic!("no Session ID line in: {text}"))
        .trim()
        .to_string()
}

fn session_json(project_dir: &str, session_id: &str) -> Value {
    let dir = std::path::Path::new(project_dir).join(".handoff/sessions");
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        if v["id"] == session_id {
            return v;
        }
    }
    panic!("session {session_id} not found");
}

/// t439.4 AC1: a new *active* session's `session.created` carries the
/// caller's `agent_id` and the new session's id.
#[test]
fn save_context_active_created_event_carries_agent_id() {
    let _agent = PinnedAgent::new("agent-created-active");
    let dir = setup();
    let project_dir = init(&dir);

    let resp = call(
        "handoff_save_context",
        json!({ "project_dir": project_dir, "session_status": "active", "summary": "a" }),
    );
    let sid = saved_session_id(&resp);

    let events = events_of(&project_dir, "session.created");
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["agent_id"], "agent-created-active");
    assert_eq!(events[0]["session_id"], sid);
}

/// t439.4 AC1+AC3: a brand-new *closed* session also emits `session.created`
/// with `agent_id`, and stores `agent_id` / `worktree` / `scope` itself.
#[test]
fn save_context_new_closed_session_emits_created_and_records_ownership() {
    let _agent = PinnedAgent::new("agent-created-closed");
    let dir = setup();
    let project_dir = init(&dir);

    let resp = call(
        "handoff_save_context",
        json!({ "project_dir": project_dir, "summary": "one-shot closed" }),
    );
    let sid = saved_session_id(&resp);

    let events = events_of(&project_dir, "session.created");
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["agent_id"], "agent-created-closed");
    assert_eq!(events[0]["session_id"], sid);

    let session = session_json(&project_dir, &sid);
    assert_eq!(session["agent_id"], "agent-created-closed");
    assert_eq!(session["worktree"], project_dir);
    assert!(
        session["scope"].is_string(),
        "scope must be auto-detected and stored: {session}"
    );
}

/// Closing an existing active session is not a creation: no extra
/// `session.created`, and the owner recorded at creation is kept.
#[test]
fn save_context_closing_active_session_emits_no_second_created() {
    let _agent = PinnedAgent::new("agent-close-existing");
    let dir = setup();
    let project_dir = init(&dir);

    call(
        "handoff_save_context",
        json!({ "project_dir": project_dir, "session_status": "active", "summary": "start" }),
    );
    // Close from a *different* caller identity: with the same pinned id an
    // owner overwrite on the close path would be invisible (DS-V0473-207 AC3).
    // t439.5 (no closing another agent's session) will revisit this scenario.
    handoff_mcp::mcp::router::set_agent_id("agent-close-other-caller".to_string());
    let closed = call(
        "handoff_save_context",
        json!({ "project_dir": project_dir, "summary": "end" }),
    );
    let sid = saved_session_id(&closed);

    assert_eq!(events_of(&project_dir, "session.created").len(), 1);
    assert_eq!(
        session_json(&project_dir, &sid)["agent_id"],
        "agent-close-existing"
    );
}

/// t439.4 AC1: `import_context` creating a session emits `session.created`.
#[test]
fn import_context_session_emits_created_with_agent_id() {
    let _agent = PinnedAgent::new("agent-import");
    let dir = setup();
    let project_dir = init(&dir);

    call(
        "handoff_import_context",
        json!({
            "project_dir": project_dir,
            "source": { "description": "notes", "format": "other" },
            "session": { "summary": "imported work" }
        }),
    );

    let events = events_of(&project_dir, "session.created");
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["agent_id"], "agent-import");
    let sid = events[0]["session_id"].as_str().unwrap();
    assert_eq!(session_json(&project_dir, sid)["summary"], "imported work");
}

/// t439.4 AC1: the raw_notes-only import path creates a session too.
#[test]
fn import_context_raw_notes_emits_created_with_agent_id() {
    let _agent = PinnedAgent::new("agent-import-raw");
    let dir = setup();
    let project_dir = init(&dir);

    call(
        "handoff_import_context",
        json!({
            "project_dir": project_dir,
            "source": { "description": "chat log", "format": "other" },
            "raw_notes": "some notes"
        }),
    );

    let events = events_of(&project_dir, "session.created");
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["agent_id"], "agent-import-raw");
}

/// t439.4 AC1: `fork_session` emits `session.created` for the forked id.
#[test]
fn fork_session_emits_created_with_agent_id() {
    let _agent = PinnedAgent::new("agent-fork");
    let dir = setup();
    let project_dir = init(&dir);

    let src = call(
        "handoff_save_context",
        json!({ "project_dir": project_dir, "session_status": "active", "summary": "source" }),
    );
    let source_id = saved_session_id(&src);

    let resp = call(
        "handoff_fork_session",
        json!({
            "project_dir": project_dir,
            "source_session_id": source_id,
            "summary": "forked"
        }),
    );
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let forked_id = text
        .lines()
        .find_map(|l| l.strip_prefix("Session ID: "))
        .unwrap()
        .trim()
        .to_string();

    let created = events_of(&project_dir, "session.created");
    let fork_event = created
        .iter()
        .find(|e| e["session_id"] == forked_id)
        .unwrap_or_else(|| panic!("no session.created for fork: {created:?}"));
    assert_eq!(fork_event["agent_id"], "agent-fork");
}

/// SPEC-V0473-107 (provisional): merge updates an existing target and
/// creates nothing, so it must not emit `session.created`.
#[test]
fn merge_sessions_emits_no_session_created() {
    let _agent = PinnedAgent::new("agent-merge");
    let dir = setup();
    let project_dir = init(&dir);

    let a = saved_session_id(&call(
        "handoff_save_context",
        json!({ "project_dir": project_dir, "session_status": "active", "summary": "a" }),
    ));
    let b = saved_session_id(&call(
        "handoff_fork_session",
        json!({ "project_dir": project_dir, "source_session_id": a, "summary": "b" }),
    ));
    let before = events_of(&project_dir, "session.created").len();

    call(
        "handoff_merge_sessions",
        json!({
            "project_dir": project_dir,
            "source_session_ids": [a, b],
            "target_session_id": a
        }),
    );

    assert_eq!(events_of(&project_dir, "session.created").len(), before);
}

/// t439.4 AC2: open -> active (implicit activation by `load_context`) emits
/// `session.activated` for the transitioned session, with the caller's id.
#[test]
fn load_context_implicit_activation_emits_session_activated() {
    let _agent = PinnedAgent::new("agent-activate");
    let dir = setup();
    let project_dir = init(&dir);

    call(
        "handoff_import_context",
        json!({
            "project_dir": project_dir,
            "source": { "description": "notes", "format": "other" },
            "session": { "summary": "open one" }
        }),
    );
    let created = events_of(&project_dir, "session.created");
    let sid = created[0]["session_id"].as_str().unwrap().to_string();

    call(
        "handoff_load_context",
        json!({ "project_dir": project_dir }),
    );

    let events = events_of(&project_dir, "session.activated");
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["session_id"], sid);
    assert_eq!(events[0]["agent_id"], "agent-activate");
    assert!(events_of(&project_dir, "session.resumed").is_empty());
}

/// t439.4 AC2: `load_context(session_id=<open>)` also emits `session.activated`.
#[test]
fn load_context_explicit_open_session_emits_session_activated() {
    let _agent = PinnedAgent::new("agent-activate-explicit");
    let dir = setup();
    let project_dir = init(&dir);

    for summary in ["open one", "open two"] {
        call(
            "handoff_import_context",
            json!({
                "project_dir": project_dir,
                "source": { "description": "notes", "format": "other" },
                "session": { "summary": summary },
                "skip_session_close": true
            }),
        );
    }
    let created = events_of(&project_dir, "session.created");
    let sid = created[1]["session_id"].as_str().unwrap().to_string();

    call(
        "handoff_load_context",
        json!({ "project_dir": project_dir, "session_id": sid }),
    );

    let events = events_of(&project_dir, "session.activated");
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["session_id"], sid);
}

/// t439.4 AC2: paused -> active emits `session.resumed` (and not
/// `session.activated`).
#[test]
fn load_context_resuming_paused_session_emits_session_resumed() {
    let _agent = PinnedAgent::new("agent-resume");
    let dir = setup();
    let project_dir = init(&dir);

    let sid = saved_session_id(&call(
        "handoff_save_context",
        json!({ "project_dir": project_dir, "session_status": "active", "summary": "p" }),
    ));
    call(
        "handoff_save_context",
        json!({ "project_dir": project_dir, "pause_only": true, "pause_active": true }),
    );

    call(
        "handoff_load_context",
        json!({ "project_dir": project_dir, "session_id": sid }),
    );

    let events = events_of(&project_dir, "session.resumed");
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["session_id"], sid);
    assert_eq!(events[0]["agent_id"], "agent-resume");
    assert!(events_of(&project_dir, "session.activated").is_empty());
}

fn update_status(project_dir: &str, task_id: &str, status: &str) {
    let resp = call(
        "handoff_update_task",
        json!({ "project_dir": project_dir, "task": { "id": task_id, "status": status } }),
    );
    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "handoff_update_task failed: {resp}"
    );
}

fn status_events(project_dir: &str, task_id: &str) -> Vec<Value> {
    let resp = call(
        "handoff_events",
        json!({ "project_dir": project_dir, "task_id": task_id, "event_type": "task.status_changed" }),
    );
    text_of(&resp)["events"].as_array().unwrap().clone()
}

/// FR-502: a status change via `handoff_update_task` emits
/// `task.status_changed` with `{from, to}` in `detail`, and the
/// `event_type` filter of `handoff_events` surfaces it.
#[test]
fn update_task_status_change_emits_status_changed_event() {
    let _guard = AGENT_ID_GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = setup();
    let project_dir = init(&dir);
    call(
        "handoff_load_context",
        json!({ "project_dir": project_dir }),
    );
    let task_id = create_task(&project_dir, "T1");

    // Creation itself is not a transition.
    assert!(status_events(&project_dir, &task_id).is_empty());

    update_status(&project_dir, &task_id, "in_progress");
    update_status(&project_dir, &task_id, "review");

    let events = status_events(&project_dir, &task_id);
    assert_eq!(events.len(), 2, "{events:?}");
    for e in &events {
        assert_eq!(e["event"], "task.status_changed");
        assert_eq!(e["task_id"], task_id);
        assert!(
            e["agent_id"].as_str().is_some_and(|a| !a.is_empty()),
            "agent_id should carry the caller identity: {e}"
        );
        chrono::DateTime::parse_from_rfc3339(e["ts"].as_str().unwrap()).unwrap();
    }
    let d0: Value = serde_json::from_str(events[0]["detail"].as_str().unwrap()).unwrap();
    assert_eq!(d0, json!({ "from": "todo", "to": "in_progress" }));
    let d1: Value = serde_json::from_str(events[1]["detail"].as_str().unwrap()).unwrap();
    assert_eq!(d1, json!({ "from": "in_progress", "to": "review" }));
}

/// Re-sending the current status (or updating a non-status field) is not a
/// transition and must not emit an event.
#[test]
fn update_task_same_status_emits_no_status_changed_event() {
    let dir = setup();
    let project_dir = init(&dir);
    let task_id = create_task(&project_dir, "T1");

    update_status(&project_dir, &task_id, "todo");
    call(
        "handoff_update_task",
        json!({ "project_dir": project_dir, "task": { "id": task_id, "title": "renamed" } }),
    );

    assert!(status_events(&project_dir, &task_id).is_empty());
}

/// `handoff_bulk_update` changes status through its own path and must emit
/// the same event.
#[test]
fn bulk_update_status_change_emits_status_changed_event() {
    let dir = setup();
    let project_dir = init(&dir);
    let task_id = create_task(&project_dir, "T1");

    let resp = call(
        "handoff_bulk_update_tasks",
        json!({
            "project_dir": project_dir,
            "updates": [
                { "task_id": task_id, "status": "in_progress" },
                { "task_id": task_id, "status": "in_progress" }
            ]
        }),
    );
    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "{resp}"
    );

    let events = status_events(&project_dir, &task_id);
    assert_eq!(events.len(), 1, "{events:?}");
    let d: Value = serde_json::from_str(events[0]["detail"].as_str().unwrap()).unwrap();
    assert_eq!(d, json!({ "from": "todo", "to": "in_progress" }));
}

/// Existing lease events are unaffected and still filterable alongside the
/// new kind (a `done` transition on a claimed task still releases it).
#[test]
fn status_changed_coexists_with_claim_and_release_events() {
    let _guard = AGENT_ID_GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = setup();
    let project_dir = init(&dir);
    call(
        "handoff_load_context",
        json!({ "project_dir": project_dir }),
    );
    let task_id = create_task(&project_dir, "T1");
    call(
        "handoff_claim_task",
        json!({ "project_dir": project_dir, "task_id": task_id, "session_id": "s-1" }),
    );
    update_status(&project_dir, &task_id, "done");

    let kinds = |ty: &str| {
        let resp = call(
            "handoff_events",
            json!({ "project_dir": project_dir, "event_type": ty }),
        );
        text_of(&resp)["total"].as_u64().unwrap()
    };
    assert_eq!(kinds("task.claimed"), 1);
    assert_eq!(kinds("task.released"), 1);
    assert_eq!(kinds("task.status_changed"), 1);
}
