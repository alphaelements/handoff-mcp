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
    assert_eq!(snap["schema_version"], 2);
    assert!(snap["tasks"].is_array(), "v2 carries the task list");
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

// ---- REQ-RPT-004: task-state snapshot (schema v2) and point-in-time restore ----

fn add_task_full(pd: &str, task: Value) -> String {
    let r = call(
        "handoff_update_task",
        json!({ "project_dir": pd, "task": task }),
    );
    assert!(!is_error(&r), "{}", text(&r));
    // "Created task <id>: <title> [<status>]"
    text(&r)
        .strip_prefix("Created task ")
        .and_then(|rest| rest.split(':').next())
        .expect("create response names the task id")
        .to_string()
}

fn write_snapshot_file(dir: &TempDir, date: &str, body: &Value) {
    let d = snapshots_dir(dir);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join(format!("{date}.json")), body.to_string()).unwrap();
}

fn write_events(dir: &TempDir, lines: &[Value]) {
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(dir.path().join(".handoff").join("events.jsonl"), body).unwrap();
}

fn status_event(ts: &str, task_id: &str, from: &str, to: &str) -> Value {
    json!({
        "ts": ts, "event": "task.status_changed", "task_id": task_id,
        "detail": json!({"from": from, "to": to}).to_string()
    })
}

fn snapshot_at(pd: &str, date: &str) -> Value {
    let r = call(
        "handoff_get_metrics_snapshot",
        json!({ "project_dir": pd, "date": date }),
    );
    assert!(!is_error(&r), "{}", text(&r));
    serde_json::from_str(&text(&r)).unwrap()
}

fn status_of(state: &Value, id: &str) -> Option<String> {
    state["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == id)
        .map(|t| t["status"].as_str().unwrap().to_string())
}

#[test]
fn snapshot_v2_records_task_list_with_assignee_and_priority() {
    let (dir, pd) = setup();
    let parent = add_task_full(
        &pd,
        json!({ "title": "parent", "assignee": "alice", "priority": "high" }),
    );
    let child = add_task_full(&pd, json!({ "title": "child", "parent_id": parent }));
    save(&pd, "s1");

    let snap = read_snapshot(&dir, &today());
    let tasks = snap["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2);
    let p = tasks.iter().find(|t| t["id"] == parent.as_str()).unwrap();
    assert_eq!(p["title"], "parent");
    assert_eq!(p["status"], "todo");
    assert_eq!(p["assignee"], "alice");
    assert_eq!(p["priority"], "high");
    let c = tasks.iter().find(|t| t["id"] == child.as_str()).unwrap();
    assert!(c["assignee"].is_null() && c["priority"].is_null());
}

#[test]
fn get_metrics_snapshot_returns_exact_snapshot_of_that_day() {
    let (dir, pd) = setup();
    write_snapshot_file(
        &dir,
        "2026-10-02",
        &json!({"schema_version": 2, "date": "2026-10-02", "captured_at": "2026-10-02T10:00:00+00:00",
                "metrics": {}, "tasks": [{"id": "x1", "title": "X", "status": "review",
                "assignee": "bob", "priority": "low"}]}),
    );
    let st = snapshot_at(&pd, "2026-10-02");
    assert_eq!(st["source"], "snapshot");
    assert_eq!(st["exact"], true);
    assert_eq!(st["tasks"][0]["status"], "review");
    assert_eq!(st["tasks"][0]["assignee"], "bob");
}

#[test]
fn get_metrics_snapshot_same_day_replays_changes_made_after_capture() {
    let (dir, pd) = setup();
    write_snapshot_file(
        &dir,
        "2026-10-02",
        &json!({"schema_version": 2, "date": "2026-10-02", "captured_at": "2026-10-02T10:00:00+00:00",
                "metrics": {}, "tasks": [
                    {"id": "a", "title": "A", "status": "todo"},
                    {"id": "b", "title": "B", "status": "todo"}]}),
    );
    write_events(
        &dir,
        &[
            // Already reflected in the snapshot (before capture).
            status_event("2026-10-02T09:00:00+00:00", "b", "blocked", "todo"),
            // Made after the capture, still on the requested day.
            status_event("2026-10-02T15:00:00+00:00", "a", "todo", "in_progress"),
            // Next day: excluded.
            status_event("2026-10-03T00:00:00+00:00", "a", "in_progress", "done"),
        ],
    );
    let st = snapshot_at(&pd, "2026-10-02");
    assert_eq!(st["source"], "snapshot_plus_events");
    assert_eq!(st["exact"], false);
    assert_eq!(st["snapshot_date"], "2026-10-02");
    assert_eq!(st["events_applied"], 1);
    assert_eq!(status_of(&st, "a").as_deref(), Some("in_progress"));
    assert_eq!(status_of(&st, "b").as_deref(), Some("todo"));
}

#[test]
fn get_metrics_snapshot_same_day_stays_exact_when_nothing_changed_after_capture() {
    let (dir, pd) = setup();
    write_snapshot_file(
        &dir,
        "2026-10-02",
        &json!({"schema_version": 2, "date": "2026-10-02", "captured_at": "2026-10-02T10:00:00+00:00",
                "metrics": {}, "tasks": [{"id": "a", "title": "A", "status": "todo"}]}),
    );
    write_events(
        &dir,
        &[
            status_event("2026-10-02T09:00:00+00:00", "a", "blocked", "todo"),
            status_event("2026-10-03T09:00:00+00:00", "a", "todo", "done"),
        ],
    );
    let st = snapshot_at(&pd, "2026-10-02");
    assert_eq!(st["source"], "snapshot");
    assert_eq!(st["exact"], true);
    assert_eq!(st["events_applied"], 0);
    assert_eq!(status_of(&st, "a").as_deref(), Some("todo"));
}

#[test]
fn get_metrics_snapshot_replays_events_on_nearest_past_snapshot() {
    let (dir, pd) = setup();
    write_snapshot_file(
        &dir,
        "2026-10-01",
        &json!({"schema_version": 2, "date": "2026-10-01", "captured_at": "2026-10-01T12:00:00+00:00",
                "metrics": {}, "tasks": [
                    {"id": "a", "title": "A", "status": "todo"},
                    {"id": "b", "title": "B", "status": "todo"}]}),
    );
    write_events(
        &dir,
        &[
            status_event("2026-10-02T09:00:00+00:00", "a", "todo", "in_progress"),
            status_event("2026-10-03T09:00:00+00:00", "a", "in_progress", "done"),
            status_event("2026-10-05T09:00:00+00:00", "b", "todo", "done"),
        ],
    );
    let st = snapshot_at(&pd, "2026-10-03");
    assert_eq!(st["source"], "snapshot_plus_events");
    assert_eq!(st["exact"], false);
    assert_eq!(st["snapshot_date"], "2026-10-01");
    assert_eq!(
        status_of(&st, "a").as_deref(),
        Some("done"),
        "event on the day counts"
    );
    assert_eq!(
        status_of(&st, "b").as_deref(),
        Some("todo"),
        "later event excluded"
    );

    let day2 = snapshot_at(&pd, "2026-10-02");
    assert_eq!(status_of(&day2, "a").as_deref(), Some("in_progress"));
}

#[test]
fn get_metrics_snapshot_without_snapshots_estimates_from_events() {
    let (dir, pd) = setup();
    let a = add_task_full(&pd, json!({ "title": "A" }));
    let b = add_task_full(&pd, json!({ "title": "B" }));
    // Real status changes now (task A: todo -> in_progress -> done), then a
    // hand-written history so the timeline is deterministic.
    call(
        "handoff_update_task",
        json!({ "project_dir": pd, "task": { "id": a, "status": "done" } }),
    );
    write_events(
        &dir,
        &[
            status_event("2026-10-02T09:00:00+00:00", &a, "todo", "in_progress"),
            status_event("2026-10-04T09:00:00+00:00", &a, "in_progress", "done"),
        ],
    );
    let st = snapshot_at(&pd, "2026-10-03");
    assert_eq!(st["source"], "events_estimate");
    assert_eq!(st["exact"], false);
    assert_eq!(status_of(&st, &a).as_deref(), Some("in_progress"));
    assert_eq!(status_of(&st, &b).as_deref(), Some("todo"));
    assert!(
        st["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("estimated")),
        "estimate must be flagged: {st}"
    );
}

#[test]
fn get_metrics_snapshot_v1_only_history_falls_back_to_events() {
    let (dir, pd) = setup();
    let a = add_task_full(&pd, json!({ "title": "A" }));
    write_snapshot_file(
        &dir,
        "2026-10-01",
        &json!({"schema_version": 1, "date": "2026-10-01",
                "captured_at": "2026-10-01T12:00:00+00:00", "metrics": {"total": 1}}),
    );
    write_events(
        &dir,
        &[status_event(
            "2026-10-09T09:00:00+00:00",
            &a,
            "blocked",
            "todo",
        )],
    );
    let st = snapshot_at(&pd, "2026-10-05");
    assert_eq!(st["source"], "events_estimate");
    assert_eq!(status_of(&st, &a).as_deref(), Some("blocked"));
    assert!(st["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w.as_str().unwrap().contains("no task list")));
}

#[test]
fn get_metrics_snapshot_rejects_missing_or_bad_date() {
    let (_dir, pd) = setup();
    let missing = call("handoff_get_metrics_snapshot", json!({ "project_dir": pd }));
    assert!(is_error(&missing));
    let bad = call(
        "handoff_get_metrics_snapshot",
        json!({ "project_dir": pd, "date": "yesterday" }),
    );
    assert!(is_error(&bad));
    assert!(text(&bad).contains("YYYY-MM-DD"), "{}", text(&bad));
}

#[test]
fn get_metrics_snapshot_is_listed_and_read_only() {
    let req = json!({"jsonrpc":"2.0","id":1,"method":"tools/list"});
    let out = handoff_mcp::mcp::protocol::process_line(&req.to_string()).unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    let tool = v["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "handoff_get_metrics_snapshot")
        .expect("tool listed");
    assert_eq!(tool["inputSchema"]["required"], json!(["date"]));

    // Reading never writes a snapshot.
    let (dir, pd) = setup();
    snapshot_at(&pd, &today());
    assert!(!snapshots_dir(&dir).exists());
}
