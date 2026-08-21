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
    let _guard = AGENT_ID_GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
    let saved = std::env::var("CLAUDE_SESSION_ID").ok();
    std::env::set_var("CLAUDE_SESSION_ID", "s-events-reconnect-test");

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

    match saved {
        Some(v) => std::env::set_var("CLAUDE_SESSION_ID", v),
        None => std::env::remove_var("CLAUDE_SESSION_ID"),
    }
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
