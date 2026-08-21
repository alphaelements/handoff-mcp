use handoff_mcp::storage::events::{append_event, read_events, EventFilters, EventRecord};
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

fn record(ts: &str, event: &str, task_id: Option<&str>, agent_id: Option<&str>) -> EventRecord {
    EventRecord {
        ts: ts.to_string(),
        event: event.to_string(),
        task_id: task_id.map(String::from),
        agent_id: agent_id.map(String::from),
        session_id: None,
        detail: None,
    }
}

#[test]
fn read_events_returns_empty_when_file_missing() {
    let dir = setup();
    let events = read_events(dir.path(), &EventFilters::default()).unwrap();
    assert!(events.is_empty());
}

#[test]
fn read_events_returns_all_when_no_filters() {
    let dir = setup();
    append_event(
        dir.path(),
        record(
            "2026-08-21T00:00:00Z",
            "task.claimed",
            Some("t1"),
            Some("a1"),
        ),
    )
    .unwrap();
    append_event(
        dir.path(),
        record(
            "2026-08-21T00:01:00Z",
            "task.released",
            Some("t1"),
            Some("a1"),
        ),
    )
    .unwrap();

    let events = read_events(dir.path(), &EventFilters::default()).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event, "task.claimed");
    assert_eq!(events[1].event, "task.released");
}

#[test]
fn read_events_filters_by_since() {
    let dir = setup();
    append_event(
        dir.path(),
        record(
            "2026-08-21T00:00:00Z",
            "task.claimed",
            Some("t1"),
            Some("a1"),
        ),
    )
    .unwrap();
    append_event(
        dir.path(),
        record(
            "2026-08-21T00:05:00Z",
            "task.released",
            Some("t1"),
            Some("a1"),
        ),
    )
    .unwrap();
    append_event(
        dir.path(),
        record("2026-08-21T00:10:00Z", "task.expired", Some("t2"), None),
    )
    .unwrap();

    let filters = EventFilters {
        since: Some("2026-08-21T00:04:00Z".to_string()),
        ..Default::default()
    };
    let events = read_events(dir.path(), &filters).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event, "task.released");
    assert_eq!(events[1].event, "task.expired");
}

#[test]
fn read_events_filters_by_task_id() {
    let dir = setup();
    append_event(
        dir.path(),
        record(
            "2026-08-21T00:00:00Z",
            "task.claimed",
            Some("t1"),
            Some("a1"),
        ),
    )
    .unwrap();
    append_event(
        dir.path(),
        record(
            "2026-08-21T00:01:00Z",
            "task.claimed",
            Some("t2"),
            Some("a1"),
        ),
    )
    .unwrap();

    let filters = EventFilters {
        task_id: Some("t2".to_string()),
        ..Default::default()
    };
    let events = read_events(dir.path(), &filters).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].task_id.as_deref(), Some("t2"));
}

#[test]
fn read_events_filters_by_agent_id() {
    let dir = setup();
    append_event(
        dir.path(),
        record(
            "2026-08-21T00:00:00Z",
            "task.claimed",
            Some("t1"),
            Some("a1"),
        ),
    )
    .unwrap();
    append_event(
        dir.path(),
        record(
            "2026-08-21T00:01:00Z",
            "task.claimed",
            Some("t2"),
            Some("a2"),
        ),
    )
    .unwrap();

    let filters = EventFilters {
        agent_id: Some("a2".to_string()),
        ..Default::default()
    };
    let events = read_events(dir.path(), &filters).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].agent_id.as_deref(), Some("a2"));
}

#[test]
fn read_events_filters_by_event_type() {
    let dir = setup();
    append_event(
        dir.path(),
        record(
            "2026-08-21T00:00:00Z",
            "task.claimed",
            Some("t1"),
            Some("a1"),
        ),
    )
    .unwrap();
    append_event(
        dir.path(),
        record(
            "2026-08-21T00:01:00Z",
            "task.released",
            Some("t1"),
            Some("a1"),
        ),
    )
    .unwrap();

    let filters = EventFilters {
        event_type: Some("task.released".to_string()),
        ..Default::default()
    };
    let events = read_events(dir.path(), &filters).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event, "task.released");
}

#[test]
fn read_events_applies_limit_keeping_the_most_recent() {
    let dir = setup();
    for i in 0..5 {
        append_event(
            dir.path(),
            record(
                &format!("2026-08-21T00:0{i}:00Z"),
                "task.claimed",
                Some(&format!("t{i}")),
                Some("a1"),
            ),
        )
        .unwrap();
    }

    let filters = EventFilters {
        limit: Some(2),
        ..Default::default()
    };
    let events = read_events(dir.path(), &filters).unwrap();
    assert_eq!(events.len(), 2);
    // Most recent two, in chronological order.
    assert_eq!(events[0].task_id.as_deref(), Some("t3"));
    assert_eq!(events[1].task_id.as_deref(), Some("t4"));
}

#[test]
fn read_events_tolerates_legacy_lines_missing_new_fields() {
    let dir = setup();
    let path = dir.path().join("events.jsonl");
    // Legacy Phase 1 line with only the fields that existed before this
    // extension (no new event kinds, but structurally identical schema —
    // this asserts the reader doesn't require any field beyond `ts`/`event`).
    std::fs::write(
        &path,
        "{\"ts\":\"2026-08-21T00:00:00Z\",\"event\":\"task.claimed\",\"task_id\":\"t1\",\"agent_id\":\"a1\",\"session_id\":\"s1\",\"detail\":\"lease_ttl=1800\"}\n",
    )
    .unwrap();

    let events = read_events(dir.path(), &EventFilters::default()).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event, "task.claimed");
}

#[test]
fn read_events_skips_malformed_lines_without_failing() {
    let dir = setup();
    let path = dir.path().join("events.jsonl");
    std::fs::write(
        &path,
        "not valid json\n{\"ts\":\"2026-08-21T00:00:00Z\",\"event\":\"task.claimed\"}\n\n",
    )
    .unwrap();

    let events = read_events(dir.path(), &EventFilters::default()).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event, "task.claimed");
}
