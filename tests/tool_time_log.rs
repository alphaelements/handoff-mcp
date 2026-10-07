//! `handoff_log_time` appends one entry per call to `.handoff/time_log.jsonl`
//! (FR-501 / SPEC-501) without disturbing the `actual_hours` accumulation.

use handoff_mcp::storage::time_log::{append_time_log, read_time_log, TimeLogEntry};
use serde_json::{json, Value};
use tempfile::TempDir;

fn call(name: &str, arguments: Value) -> Value {
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments }
    });
    let out = handoff_mcp::mcp::protocol::process_line(&req.to_string()).expect("response");
    serde_json::from_str(&out).expect("valid JSON response")
}

fn text(resp: &Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .expect("text content")
        .to_string()
}

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"].as_bool().unwrap_or(false)
}

fn init() -> (TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let project_dir = dir.path().to_string_lossy().to_string();
    call(
        "handoff_init",
        json!({ "project_dir": project_dir, "project_name": "time-log-test" }),
    );
    call(
        "handoff_update_config",
        json!({ "project_dir": project_dir, "updates": { "settings.require_estimate_hours": false } }),
    );
    (dir, project_dir)
}

fn create_task(project_dir: &str, schedule: Value) -> String {
    let resp = call(
        "handoff_update_task",
        json!({ "project_dir": project_dir, "task": { "title": "T", "schedule": schedule } }),
    );
    assert!(!is_error(&resp), "{resp}");
    text(&resp)
        .split_whitespace()
        .nth(2)
        .unwrap()
        .trim_end_matches(':')
        .to_string()
}

fn raw_lines(dir: &TempDir) -> Vec<String> {
    std::fs::read_to_string(dir.path().join(".handoff/time_log.jsonl"))
        .unwrap()
        .lines()
        .map(String::from)
        .collect()
}

#[test]
fn log_time_appends_one_jsonl_entry_per_call() {
    let (dir, project_dir) = init();
    let task_id = create_task(&project_dir, json!({ "estimate_hours": 5.0 }));

    let r1 = call(
        "handoff_log_time",
        json!({ "project_dir": project_dir, "task_id": task_id, "hours": 0.5, "note": "first" }),
    );
    assert!(!is_error(&r1), "{r1}");
    let r2 = call(
        "handoff_log_time",
        json!({ "project_dir": project_dir, "task_id": task_id, "hours": 1.25 }),
    );
    assert!(!is_error(&r2), "{r2}");

    let lines = raw_lines(&dir);
    assert_eq!(lines.len(), 2, "one line per call: {lines:?}");

    let e1: Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(e1["task_id"], task_id);
    assert_eq!(e1["hours"], 0.5);
    assert_eq!(e1["note"], "first");
    let ts = e1["ts"].as_str().expect("ts is a string");
    chrono::DateTime::parse_from_rfc3339(ts).expect("ts is ISO 8601 / RFC 3339");

    let e2: Value = serde_json::from_str(&lines[1]).unwrap();
    assert_eq!(e2["hours"], 1.25);
    assert!(e2.get("note").is_none() || e2["note"].is_null());
}

#[test]
fn log_time_still_accumulates_actual_hours_and_remaining() {
    let (_dir, project_dir) = init();
    let task_id = create_task(
        &project_dir,
        json!({ "estimate_hours": 5.0, "remaining_hours": 4.0 }),
    );

    call(
        "handoff_log_time",
        json!({ "project_dir": project_dir, "task_id": task_id, "hours": 1.0 }),
    );
    let resp = call(
        "handoff_log_time",
        json!({ "project_dir": project_dir, "task_id": task_id, "hours": 0.5 }),
    );
    assert!(!is_error(&resp), "{resp}");
    assert_eq!(
        text(&resp),
        format!("Logged 0.5h on {task_id}: actual=1.5h, remaining=2.5h")
    );
}

#[test]
fn log_time_rejected_call_writes_no_entry() {
    let (dir, project_dir) = init();
    let task_id = create_task(&project_dir, json!({ "estimate_hours": 1.0 }));

    let bad_hours = call(
        "handoff_log_time",
        json!({ "project_dir": project_dir, "task_id": task_id, "hours": -1.0 }),
    );
    assert!(is_error(&bad_hours));
    let bad_task = call(
        "handoff_log_time",
        json!({ "project_dir": project_dir, "task_id": "nope", "hours": 1.0 }),
    );
    assert!(is_error(&bad_task));

    assert!(!dir.path().join(".handoff/time_log.jsonl").exists());
}

#[test]
fn storage_roundtrip_skips_malformed_lines_and_missing_file_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    assert!(read_time_log(dir.path()).unwrap().is_empty());

    let entry = |hours: f64| TimeLogEntry {
        ts: "2026-10-08T00:00:00+00:00".to_string(),
        task_id: "t1".to_string(),
        hours,
        agent_id: Some("a1".to_string()),
        note: None,
    };
    append_time_log(dir.path(), &entry(1.0)).unwrap();
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join("time_log.jsonl"))
            .unwrap();
        writeln!(f, "not json").unwrap();
    }
    append_time_log(dir.path(), &entry(2.0)).unwrap();

    let got = read_time_log(dir.path()).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].hours, 1.0);
    assert_eq!(got[1].hours, 2.0);
    assert_eq!(got[0].agent_id.as_deref(), Some("a1"));
}
