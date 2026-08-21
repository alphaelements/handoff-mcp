//! Storage layer for multi-worktree agent coordination records
//! (`.handoff/agents/<agent-id>.json`).
//!
//! Covers spec sections 3.2 (AgentRecord model) and 6.1 (heartbeat + GC).

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{Duration, Utc};
use handoff_mcp::storage::agents::*;
use tempfile::TempDir;

/// Serializes tests that mutate `CLAUDE_SESSION_ID`, since env vars are
/// process-global and Rust runs tests in threads.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

struct EnvGuard {
    saved: Option<String>,
}

impl EnvGuard {
    fn new() -> Self {
        let saved = std::env::var("CLAUDE_SESSION_ID").ok();
        std::env::remove_var("CLAUDE_SESSION_ID");
        Self { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.saved {
            Some(v) => std::env::set_var("CLAUDE_SESSION_ID", v),
            None => std::env::remove_var("CLAUDE_SESSION_ID"),
        }
    }
}

fn setup() -> TempDir {
    tempfile::tempdir().expect("failed to create temp dir")
}

fn make_record(agent_id: &str, last_heartbeat: chrono::DateTime<Utc>) -> AgentRecord {
    AgentRecord {
        agent_id: agent_id.to_string(),
        session_id: Some("s-20260821-000000-000000".to_string()),
        worktree: PathBuf::from("/home/aeuser/pro/handoff-mcp"),
        branch: Some("feat/multi-wt-p1-session1".to_string()),
        pid: Some(12345),
        registered_at: Utc::now(),
        last_heartbeat,
        status: AgentStatus::Active,
        claimed_tasks: vec!["t240.5".to_string()],
        metadata: HashMap::new(),
    }
}

// --- Serialization round-trip ---

#[test]
fn agent_record_serde_round_trip() {
    let record = make_record("agent-1", Utc::now());
    let json = serde_json::to_string_pretty(&record).unwrap();
    let parsed: AgentRecord = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.agent_id, record.agent_id);
    assert_eq!(parsed.session_id, record.session_id);
    assert_eq!(parsed.worktree, record.worktree);
    assert_eq!(parsed.branch, record.branch);
    assert_eq!(parsed.pid, record.pid);
    assert_eq!(parsed.status, AgentStatus::Active);
    assert_eq!(parsed.claimed_tasks, record.claimed_tasks);
}

#[test]
fn agent_status_serializes_snake_case() {
    let json = serde_json::to_string(&AgentStatus::Disconnected).unwrap();
    assert_eq!(json, "\"disconnected\"");
    let json = serde_json::to_string(&AgentStatus::Stale).unwrap();
    assert_eq!(json, "\"stale\"");
}

#[test]
fn agent_record_metadata_defaults_when_absent() {
    // A record written by an older version without `metadata` must still parse.
    let json = r#"{
        "agent_id": "agent-1",
        "session_id": null,
        "worktree": "/tmp/wt",
        "branch": null,
        "pid": null,
        "registered_at": "2026-08-21T00:00:00Z",
        "last_heartbeat": "2026-08-21T00:00:00Z",
        "status": "active",
        "claimed_tasks": []
    }"#;
    let parsed: AgentRecord = serde_json::from_str(json).unwrap();
    assert!(parsed.metadata.is_empty());
}

// --- agent_id generation ---

#[test]
fn generate_agent_id_uses_claude_session_id_when_set() {
    let _lock = env_lock();
    let _guard = EnvGuard::new();
    std::env::set_var("CLAUDE_SESSION_ID", "claude-session-abc123");

    let id = generate_agent_id();
    assert_eq!(id, "claude-session-abc123");
}

#[test]
fn generate_agent_id_falls_back_when_unset() {
    let _lock = env_lock();
    let _guard = EnvGuard::new();
    // CLAUDE_SESSION_ID intentionally unset by EnvGuard.

    let id = generate_agent_id();
    assert!(id.starts_with("a-"), "id: {id}");
    // a-YYYYMMDD-HHMMSS-FFFFFF
    assert_eq!(id.len(), "a-20260821-153000-123456".len(), "id: {id}");
}

#[test]
fn generate_agent_id_falls_back_when_empty_string() {
    let _lock = env_lock();
    let _guard = EnvGuard::new();
    std::env::set_var("CLAUDE_SESSION_ID", "");

    let id = generate_agent_id();
    assert!(id.starts_with("a-"), "id: {id}");
}

// --- CRUD ---

#[test]
fn agents_dir_is_handoff_dir_slash_agents() {
    let dir = setup();
    let handoff_dir = dir.path().join(".handoff");
    assert_eq!(agents_dir(&handoff_dir), handoff_dir.join("agents"));
}

#[test]
fn write_then_read_agent_round_trips() {
    let dir = setup();
    let handoff_dir = dir.path().join(".handoff");
    std::fs::create_dir_all(agents_dir(&handoff_dir)).unwrap();

    let record = make_record("agent-1", Utc::now());
    write_agent(&handoff_dir, &record).unwrap();

    let read_back = read_agent(&handoff_dir, "agent-1").unwrap();
    assert!(read_back.is_some());
    let read_back = read_back.unwrap();
    assert_eq!(read_back.agent_id, "agent-1");
    assert_eq!(read_back.claimed_tasks, vec!["t240.5".to_string()]);
}

#[test]
fn write_agent_creates_agents_dir_if_missing() {
    let dir = setup();
    let handoff_dir = dir.path().join(".handoff");
    // Deliberately do not pre-create agents_dir.

    let record = make_record("agent-2", Utc::now());
    write_agent(&handoff_dir, &record).unwrap();

    assert!(agents_dir(&handoff_dir).join("agent-2.json").exists());
}

#[test]
fn read_agent_returns_none_when_missing() {
    let dir = setup();
    let handoff_dir = dir.path().join(".handoff");
    std::fs::create_dir_all(agents_dir(&handoff_dir)).unwrap();

    let result = read_agent(&handoff_dir, "does-not-exist").unwrap();
    assert!(result.is_none());
}

#[test]
fn read_agent_returns_none_when_agents_dir_missing() {
    let dir = setup();
    let handoff_dir = dir.path().join(".handoff");
    // agents_dir not created at all.

    let result = read_agent(&handoff_dir, "agent-1").unwrap();
    assert!(result.is_none());
}

/// `safe_file_stem` maps every unsafe character to `_`, so `"agent:1"` and
/// `"agent_1"` both resolve to the same on-disk file `agent_1.json`. If a
/// caller later reads under the *other* id, the file exists but its
/// `agent_id` field does not match what was requested — `read_agent` must
/// treat that as "not found" rather than silently returning the wrong
/// agent's record.
#[test]
fn read_agent_returns_none_when_file_exists_but_agent_id_differs() {
    let dir = setup();
    let handoff_dir = dir.path().join(".handoff");
    std::fs::create_dir_all(agents_dir(&handoff_dir)).unwrap();

    write_agent(&handoff_dir, &make_record("agent:1", Utc::now())).unwrap();

    // Sanity: both ids collide on the same sanitized filename.
    assert_eq!(
        agents_dir(&handoff_dir).join("agent_1.json"),
        agents_dir(&handoff_dir).join("agent_1.json")
    );

    let result = read_agent(&handoff_dir, "agent_1").unwrap();
    assert!(
        result.is_none(),
        "reading under a colliding but different agent_id must return None, got {result:?}"
    );

    // The original id is unaffected.
    let original = read_agent(&handoff_dir, "agent:1").unwrap();
    assert!(original.is_some());
    assert_eq!(original.unwrap().agent_id, "agent:1");
}

#[test]
fn list_agents_returns_all_records() {
    let dir = setup();
    let handoff_dir = dir.path().join(".handoff");
    std::fs::create_dir_all(agents_dir(&handoff_dir)).unwrap();

    write_agent(&handoff_dir, &make_record("agent-1", Utc::now())).unwrap();
    write_agent(&handoff_dir, &make_record("agent-2", Utc::now())).unwrap();

    let mut agents = list_agents(&handoff_dir).unwrap();
    agents.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));

    assert_eq!(agents.len(), 2);
    assert_eq!(agents[0].agent_id, "agent-1");
    assert_eq!(agents[1].agent_id, "agent-2");
}

#[test]
fn list_agents_empty_when_dir_missing() {
    let dir = setup();
    let handoff_dir = dir.path().join(".handoff");

    let agents = list_agents(&handoff_dir).unwrap();
    assert!(agents.is_empty());
}

#[test]
fn delete_agent_removes_file() {
    let dir = setup();
    let handoff_dir = dir.path().join(".handoff");
    std::fs::create_dir_all(agents_dir(&handoff_dir)).unwrap();

    write_agent(&handoff_dir, &make_record("agent-1", Utc::now())).unwrap();
    assert!(agents_dir(&handoff_dir).join("agent-1.json").exists());

    delete_agent(&handoff_dir, "agent-1").unwrap();
    assert!(!agents_dir(&handoff_dir).join("agent-1.json").exists());
}

#[test]
fn delete_agent_is_noop_when_missing() {
    let dir = setup();
    let handoff_dir = dir.path().join(".handoff");
    std::fs::create_dir_all(agents_dir(&handoff_dir)).unwrap();

    // Should not error even though the file was never created.
    delete_agent(&handoff_dir, "no-such-agent").unwrap();
}

// --- status computation ---

#[test]
fn compute_status_active_within_ttl() {
    let now = Utc::now();
    let record = make_record("agent-1", now - Duration::seconds(1000));
    assert_eq!(compute_status(&record, now), AgentStatus::Active);
}

#[test]
fn compute_status_active_at_exact_ttl_boundary() {
    let now = Utc::now();
    let record = make_record("agent-1", now - Duration::seconds(1800));
    assert_eq!(compute_status(&record, now), AgentStatus::Active);
}

#[test]
fn compute_status_stale_just_past_ttl() {
    let now = Utc::now();
    let record = make_record("agent-1", now - Duration::seconds(1801));
    assert_eq!(compute_status(&record, now), AgentStatus::Stale);
}

#[test]
fn compute_status_stale_at_exact_double_ttl_boundary() {
    let now = Utc::now();
    let record = make_record("agent-1", now - Duration::seconds(3600));
    assert_eq!(compute_status(&record, now), AgentStatus::Stale);
}

#[test]
fn compute_status_disconnected_past_double_ttl() {
    let now = Utc::now();
    let record = make_record("agent-1", now - Duration::seconds(3601));
    assert_eq!(compute_status(&record, now), AgentStatus::Disconnected);
}

// --- heartbeat debounce ---
//
// `update_heartbeat`'s debounce timer is process-global by design (it rate
// limits disk writes for the whole process, not per agent_id — see
// `src/storage/agents.rs`). Rust runs `#[test]` fns concurrently on
// different threads within the same test binary, and the crate exposes no
// reset hook for that static timer, so splitting these into independent
// `#[test]` functions would make whichever runs first silently consume the
// "first call in this process" write for the others. They are combined into
// one test that drives the debounce state machine through its full,
// order-dependent sequence instead.
#[test]
fn update_heartbeat_lifecycle() {
    let dir = setup();
    let handoff_dir = dir.path().join(".handoff");
    std::fs::create_dir_all(agents_dir(&handoff_dir)).unwrap();

    // 1. Calling update_heartbeat for an agent that was never registered
    //    must not count as a debounced write, and must report false.
    let updated = update_heartbeat(&handoff_dir, "agent-never-registered").unwrap();
    assert!(!updated, "missing agent must not be treated as updated");

    // 2. The very first successful call in this process writes through.
    let old_heartbeat = Utc::now() - Duration::seconds(5000);
    let mut record = make_record("agent-hb-1", old_heartbeat);
    record.status = AgentStatus::Stale;
    write_agent(&handoff_dir, &record).unwrap();

    let first = update_heartbeat(&handoff_dir, "agent-hb-1").unwrap();
    assert!(first, "expected first heartbeat call to write");

    let read_back = read_agent(&handoff_dir, "agent-hb-1").unwrap().unwrap();
    assert!(read_back.last_heartbeat > old_heartbeat);
    assert_eq!(read_back.status, AgentStatus::Active);

    // 3. A second call within 60s of the first — even for a *different*
    //    agent — is debounced (the timer is process-global, not per-agent).
    let record2 = make_record("agent-hb-2", Utc::now() - Duration::seconds(100));
    write_agent(&handoff_dir, &record2).unwrap();

    let second = update_heartbeat(&handoff_dir, "agent-hb-2").unwrap();
    assert!(
        !second,
        "second call within 60s of the first should be debounced"
    );
    let unchanged = read_agent(&handoff_dir, "agent-hb-2").unwrap().unwrap();
    assert_eq!(
        unchanged.last_heartbeat, record2.last_heartbeat,
        "debounced call must not touch the record on disk"
    );
}

// --- GC ---

#[test]
fn gc_agents_removes_disconnected_past_retention() {
    let dir = setup();
    let handoff_dir = dir.path().join(".handoff");
    std::fs::create_dir_all(agents_dir(&handoff_dir)).unwrap();

    // Disconnected threshold is last_heartbeat + 3600s; retention adds 7 more
    // days on top of that before GC removes the record.
    let long_gone = Utc::now() - Duration::seconds(3600) - Duration::days(8);
    write_agent(&handoff_dir, &make_record("agent-old", long_gone)).unwrap();

    let removed = gc_agents(&handoff_dir).unwrap();

    assert_eq!(removed, vec!["agent-old".to_string()]);
    assert!(read_agent(&handoff_dir, "agent-old").unwrap().is_none());
}

#[test]
fn gc_agents_removes_at_exact_retention_boundary() {
    let dir = setup();
    let handoff_dir = dir.path().join(".handoff");
    std::fs::create_dir_all(agents_dir(&handoff_dir)).unwrap();

    // Exactly 7 days since the record crossed into Disconnected.
    // disconnected_since = last_heartbeat + 3600s, so
    // last_heartbeat = now - 3600s - 7 days.
    let exact_boundary = Utc::now() - Duration::seconds(3600) - Duration::days(7);
    write_agent(&handoff_dir, &make_record("agent-exact", exact_boundary)).unwrap();

    let removed = gc_agents(&handoff_dir).unwrap();

    assert_eq!(
        removed,
        vec!["agent-exact".to_string()],
        "record exactly at the 7-day retention boundary must be removed (>= semantics)"
    );
    assert!(read_agent(&handoff_dir, "agent-exact").unwrap().is_none());
}

#[test]
fn gc_agents_keeps_disconnected_just_before_retention_boundary() {
    let dir = setup();
    let handoff_dir = dir.path().join(".handoff");
    std::fs::create_dir_all(agents_dir(&handoff_dir)).unwrap();

    // 6 days + 23 hours since the record crossed into Disconnected — just
    // under 7 days, so num_days() returns 6 and the record must be kept.
    let just_under = Utc::now() - Duration::seconds(3600) - Duration::days(7) + Duration::hours(1);
    write_agent(&handoff_dir, &make_record("agent-almost", just_under)).unwrap();

    let removed = gc_agents(&handoff_dir).unwrap();

    assert!(
        removed.is_empty(),
        "record just under the 7-day retention boundary must be kept"
    );
    assert!(read_agent(&handoff_dir, "agent-almost").unwrap().is_some());
}

#[test]
fn gc_agents_keeps_disconnected_within_retention() {
    let dir = setup();
    let handoff_dir = dir.path().join(".handoff");
    std::fs::create_dir_all(agents_dir(&handoff_dir)).unwrap();

    // Disconnected (elapsed > 3600s) but less than 7 days since it became so.
    let recently_gone = Utc::now() - Duration::seconds(3600) - Duration::days(2);
    write_agent(&handoff_dir, &make_record("agent-recent", recently_gone)).unwrap();

    let removed = gc_agents(&handoff_dir).unwrap();

    assert!(removed.is_empty());
    assert!(read_agent(&handoff_dir, "agent-recent").unwrap().is_some());
}

#[test]
fn gc_agents_keeps_active_and_stale_records() {
    let dir = setup();
    let handoff_dir = dir.path().join(".handoff");
    std::fs::create_dir_all(agents_dir(&handoff_dir)).unwrap();

    write_agent(&handoff_dir, &make_record("agent-active", Utc::now())).unwrap();
    write_agent(
        &handoff_dir,
        &make_record("agent-stale", Utc::now() - Duration::seconds(2000)),
    )
    .unwrap();

    let removed = gc_agents(&handoff_dir).unwrap();

    assert!(removed.is_empty());
    assert!(read_agent(&handoff_dir, "agent-active").unwrap().is_some());
    assert!(read_agent(&handoff_dir, "agent-stale").unwrap().is_some());
}
