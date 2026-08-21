use handoff_mcp::storage::events::{append_event, EventRecord};
use tempfile::TempDir;

fn setup() -> TempDir {
    tempfile::tempdir().expect("failed to create temp dir")
}

fn read_lines(handoff_dir: &std::path::Path) -> Vec<String> {
    let path = handoff_dir.join("events.jsonl");
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(String::from)
        .collect()
}

#[test]
fn append_event_creates_events_jsonl_with_valid_json_line() {
    let dir = setup();

    append_event(
        dir.path(),
        EventRecord {
            ts: "2026-08-21T00:00:00Z".to_string(),
            event: "task.claimed".to_string(),
            task_id: Some("t1".to_string()),
            agent_id: Some("agent-1".to_string()),
            session_id: Some("s-1".to_string()),
            detail: Some("lease_ttl=1800".to_string()),
        },
    )
    .unwrap();

    let lines = read_lines(dir.path());
    assert_eq!(lines.len(), 1);
    let parsed: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(parsed["event"], "task.claimed");
    assert_eq!(parsed["task_id"], "t1");
    assert_eq!(parsed["agent_id"], "agent-1");
    assert_eq!(parsed["session_id"], "s-1");
}

#[test]
fn append_event_appends_multiple_lines_in_order() {
    let dir = setup();

    append_event(
        dir.path(),
        EventRecord {
            ts: "2026-08-21T00:00:00Z".to_string(),
            event: "task.claimed".to_string(),
            task_id: Some("t1".to_string()),
            agent_id: Some("agent-1".to_string()),
            session_id: Some("s-1".to_string()),
            detail: None,
        },
    )
    .unwrap();
    append_event(
        dir.path(),
        EventRecord {
            ts: "2026-08-21T00:01:00Z".to_string(),
            event: "task.released".to_string(),
            task_id: Some("t1".to_string()),
            agent_id: Some("agent-1".to_string()),
            session_id: Some("s-1".to_string()),
            detail: None,
        },
    )
    .unwrap();

    let lines = read_lines(dir.path());
    assert_eq!(lines.len(), 2);
    for line in &lines {
        // Each line must independently parse as valid JSON (JSON Lines format).
        let _: serde_json::Value = serde_json::from_str(line).expect("valid JSON per line");
    }
    let first: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    let second: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
    assert_eq!(first["event"], "task.claimed");
    assert_eq!(second["event"], "task.released");
}

#[test]
fn append_event_omits_none_fields_from_json() {
    let dir = setup();

    append_event(
        dir.path(),
        EventRecord {
            ts: "2026-08-21T00:00:00Z".to_string(),
            event: "task.expired".to_string(),
            task_id: None,
            agent_id: None,
            session_id: None,
            detail: None,
        },
    )
    .unwrap();

    let lines = read_lines(dir.path());
    let parsed: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    assert!(parsed.get("task_id").is_none());
    assert!(parsed.get("agent_id").is_none());
    assert!(parsed.get("session_id").is_none());
    assert!(parsed.get("detail").is_none());
}
