//! FR-503 / SPEC-503: daily metrics snapshots under
//! `.handoff/metrics_snapshots/<YYYY-MM-DD>.json`, written by
//! `handoff_save_context` and by the manual `handoff_snapshot_metrics` tool.

use chrono::Utc;
use serde_json::{json, Value};
use std::path::PathBuf;
use tempfile::TempDir;

fn call(name: &str, arguments: Value) -> Value {
    let req = json!({
        "jsonrpc": "2.0", "id": 1,
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

fn setup() -> (TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let pd = dir.path().to_string_lossy().to_string();
    call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "snap-test" }),
    );
    call(
        "handoff_update_config",
        json!({ "project_dir": pd, "updates": { "settings.require_estimate_hours": false } }),
    );
    (dir, pd)
}

fn snapshots_dir(dir: &TempDir) -> PathBuf {
    dir.path().join(".handoff").join("metrics_snapshots")
}

fn today() -> String {
    Utc::now().format("%Y-%m-%d").to_string()
}

fn read_snapshot(dir: &TempDir, date: &str) -> Value {
    let p = snapshots_dir(dir).join(format!("{date}.json"));
    let raw = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    serde_json::from_str(&raw).expect("snapshot is valid JSON")
}

fn assert_snapshot_schema(snap: &Value, expected_date: &str) {
    assert_eq!(snap["schema_version"], 1);
    assert_eq!(snap["date"], expected_date);
    let captured = snap["captured_at"].as_str().expect("captured_at string");
    chrono::DateTime::parse_from_rfc3339(captured).expect("captured_at is RFC 3339");
    let m = &snap["metrics"];
    assert!(m["total"].is_u64());
    assert!(m["by_status"].is_object());
    assert!(m["completion_percent"].is_number());
    assert!(m["total_estimate_hours"].is_number());
    assert!(m["total_actual_hours"].is_number());
    assert!(m["total_remaining_hours"].is_number());
    assert!(m["overdue_count"].is_u64());
    assert!(m["overdue_tasks"].is_array());
    assert!(m["milestones"].is_array());
}

fn add_task(pd: &str, title: &str) {
    let r = call(
        "handoff_update_task",
        json!({ "project_dir": pd, "task": { "title": title } }),
    );
    assert!(!is_error(&r), "{}", text(&r));
}

fn save(pd: &str, summary: &str) -> Value {
    call(
        "handoff_save_context",
        json!({ "project_dir": pd, "summary": summary }),
    )
}

#[test]
fn save_context_writes_daily_snapshot_with_schema() {
    let (dir, pd) = setup();
    add_task(&pd, "one");
    add_task(&pd, "two");
    let r = save(&pd, "s1");
    assert!(!is_error(&r), "{}", text(&r));

    let snap = read_snapshot(&dir, &today());
    assert_snapshot_schema(&snap, &today());
    assert_eq!(snap["metrics"]["total"], 2);
}

#[test]
fn save_context_same_day_overwrites_single_file() {
    let (dir, pd) = setup();
    add_task(&pd, "one");
    save(&pd, "first");
    add_task(&pd, "two");
    save(&pd, "second");

    let files: Vec<_> = std::fs::read_dir(snapshots_dir(&dir))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(files, vec![format!("{}.json", today())], "got {files:?}");
    let snap = read_snapshot(&dir, &today());
    assert_eq!(
        snap["metrics"]["total"], 2,
        "second save overwrote the first"
    );
}

#[test]
fn pause_only_save_does_not_write_snapshot() {
    let (dir, pd) = setup();
    call(
        "handoff_save_context",
        json!({ "project_dir": pd, "pause_only": true, "pause_active": true }),
    );
    assert!(!snapshots_dir(&dir).exists());
}

#[test]
fn snapshot_metrics_tool_saves_and_returns_snapshot() {
    let (dir, pd) = setup();
    add_task(&pd, "one");
    let r = call("handoff_snapshot_metrics", json!({ "project_dir": pd }));
    assert!(!is_error(&r), "{}", text(&r));
    let returned: Value = serde_json::from_str(&text(&r)).unwrap();
    assert_snapshot_schema(&returned, &today());
    assert_eq!(
        returned["path"],
        format!("metrics_snapshots/{}.json", today())
    );
    assert_eq!(returned["metrics"]["total"], 1);

    let on_disk = read_snapshot(&dir, &today());
    assert_snapshot_schema(&on_disk, &today());
    assert_eq!(on_disk["metrics"], returned["metrics"]);
    assert!(
        on_disk.get("path").is_none(),
        "path is a response-only field"
    );
}

#[test]
fn snapshot_metrics_tool_overwrites_same_day() {
    let (dir, pd) = setup();
    call("handoff_snapshot_metrics", json!({ "project_dir": pd }));
    add_task(&pd, "late");
    call("handoff_snapshot_metrics", json!({ "project_dir": pd }));
    assert_eq!(std::fs::read_dir(snapshots_dir(&dir)).unwrap().count(), 1);
    assert_eq!(read_snapshot(&dir, &today())["metrics"]["total"], 1);
}

#[test]
fn snapshot_metrics_is_listed_without_assignee_filter() {
    // The snapshot is always project-wide; the tool takes no assignee filter.
    let req = json!({"jsonrpc":"2.0","id":1,"method":"tools/list"});
    let out = handoff_mcp::mcp::protocol::process_line(&req.to_string()).unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    let tool = v["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "handoff_snapshot_metrics")
        .expect("tool listed");
    assert!(tool["inputSchema"]["properties"].get("assignee").is_none());
}
