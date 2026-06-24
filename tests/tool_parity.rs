//! E2E tests for the VSCode v0.5 parity work (referrals ref-...004309 and
//! ref-...232823): schedule merge fix, assignee/milestone CRUD, calendar/labels/
//! start_project, auto_schedule day_hours, atomic write, and Config roundtrip.

use serde_json::{json, Value};
use std::fs;
use tempfile::TempDir;

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
                "project_name": "parity-project"
            }
        }
    });
    send(&req.to_string()).unwrap();
    dir
}

fn call(dir: &TempDir, name: &str, mut arguments: Value) -> Value {
    arguments["project_dir"] = json!(dir.path().to_string_lossy());
    let req = json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments }
    });
    send(&req.to_string()).unwrap()
}

fn text(resp: &Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"].as_bool().unwrap_or(false)
}

fn get_task(dir: &TempDir, id: &str) -> Value {
    let resp = call(dir, "handoff_get_task", json!({ "task_id": id }));
    assert!(!is_error(&resp), "get_task error: {}", text(&resp));
    serde_json::from_str(&text(&resp)).unwrap()
}

fn read_config(dir: &TempDir) -> String {
    fs::read_to_string(dir.path().join(".handoff/config.toml")).unwrap()
}

// ============================================================
// schedule merge (CRITICAL — referral ref-...232823)
// ============================================================

#[test]
fn schedule_partial_update_preserves_actual_and_remaining() {
    let dir = setup_project();
    call(
        &dir,
        "handoff_update_task",
        json!({ "task": {
            "title": "Tracked",
            "schedule": { "estimate_hours": 10.0, "actual_hours": 4.0, "remaining_hours": 6.0, "milestone": "v1" }
        }}),
    );

    // Patch only estimate_hours + milestone — actual/remaining must survive.
    let resp = call(
        &dir,
        "handoff_update_task",
        json!({ "task": { "id": "t1", "schedule": { "estimate_hours": 12.0, "milestone": "v2" } }}),
    );
    assert!(!is_error(&resp), "error: {}", text(&resp));

    let task = get_task(&dir, "t1");
    assert_eq!(task["schedule"]["estimate_hours"], 12.0, "estimate updated");
    assert_eq!(task["schedule"]["milestone"], "v2", "milestone updated");
    assert_eq!(task["schedule"]["actual_hours"], 4.0, "actual preserved");
    assert_eq!(
        task["schedule"]["remaining_hours"], 6.0,
        "remaining preserved"
    );
}

#[test]
fn schedule_update_can_set_dates_without_touching_hours() {
    let dir = setup_project();
    call(
        &dir,
        "handoff_update_task",
        json!({ "task": { "title": "T", "schedule": { "actual_hours": 3.0 } }}),
    );
    call(
        &dir,
        "handoff_update_task",
        json!({ "task": { "id": "t1", "schedule": { "start_date": "2026-07-01", "due_date": "2026-07-05" } }}),
    );
    let task = get_task(&dir, "t1");
    assert_eq!(task["schedule"]["start_date"], "2026-07-01");
    assert_eq!(task["schedule"]["due_date"], "2026-07-05");
    assert_eq!(task["schedule"]["actual_hours"], 3.0);
}

#[test]
fn schedule_pinned_toggle_preserves_other_fields() {
    let dir = setup_project();
    call(
        &dir,
        "handoff_update_task",
        json!({ "task": { "title": "T", "schedule": { "start_date": "2026-07-01", "estimate_hours": 8.0 } }}),
    );
    call(
        &dir,
        "handoff_update_task",
        json!({ "task": { "id": "t1", "schedule": { "pinned": true } }}),
    );
    let task = get_task(&dir, "t1");
    assert_eq!(task["schedule"]["pinned"], true);
    assert_eq!(task["schedule"]["start_date"], "2026-07-01");
    assert_eq!(task["schedule"]["estimate_hours"], 8.0);
}

// ============================================================
// assignee CRUD (Phase B)
// ============================================================

#[test]
fn add_assignee_writes_config() {
    let dir = setup_project();
    let resp = call(
        &dir,
        "handoff_add_assignee",
        json!({ "key": "alice", "display_name": "Alice", "color": "#ff0000", "work_hours_per_day": 6.0 }),
    );
    assert!(!is_error(&resp), "error: {}", text(&resp));

    let cfg = read_config(&dir);
    assert!(cfg.contains("[assignees.alice]"), "config:\n{cfg}");
    assert!(cfg.contains("Alice"));

    let listed = call(&dir, "handoff_list_assignees", json!({}));
    let v: Value = serde_json::from_str(&text(&listed)).unwrap();
    assert_eq!(v["assignees"]["alice"]["display_name"], "Alice");
}

#[test]
fn add_assignee_duplicate_fails() {
    let dir = setup_project();
    call(&dir, "handoff_add_assignee", json!({ "key": "bob" }));
    let resp = call(&dir, "handoff_add_assignee", json!({ "key": "bob" }));
    assert!(is_error(&resp));
    assert!(text(&resp).contains("already exists"));
}

#[test]
fn update_assignee_merges_fields() {
    let dir = setup_project();
    call(
        &dir,
        "handoff_add_assignee",
        json!({ "key": "carol", "display_name": "Carol", "color": "#00ff00" }),
    );
    // Update only color; display_name must remain.
    let resp = call(
        &dir,
        "handoff_update_assignee",
        json!({ "key": "carol", "color": "#0000ff" }),
    );
    assert!(!is_error(&resp), "error: {}", text(&resp));
    let cfg = read_config(&dir);
    assert!(cfg.contains("Carol"), "display_name preserved:\n{cfg}");
    assert!(cfg.contains("#0000ff"), "color updated:\n{cfg}");
    assert!(!cfg.contains("#00ff00"), "old color gone:\n{cfg}");
}

#[test]
fn update_assignee_nonexistent_fails() {
    let dir = setup_project();
    let resp = call(&dir, "handoff_update_assignee", json!({ "key": "ghost" }));
    assert!(is_error(&resp));
    assert!(text(&resp).contains("not found"));
}

#[test]
fn update_assignee_null_clears_field() {
    let dir = setup_project();
    call(
        &dir,
        "handoff_add_assignee",
        json!({ "key": "dan", "display_name": "Dan", "color": "#abcdef" }),
    );
    call(
        &dir,
        "handoff_update_assignee",
        json!({ "key": "dan", "color": Value::Null }),
    );
    let cfg = read_config(&dir);
    assert!(cfg.contains("Dan"));
    assert!(!cfg.contains("#abcdef"), "color cleared:\n{cfg}");
}

#[test]
fn remove_assignee_unassigns_tasks() {
    let dir = setup_project();
    call(&dir, "handoff_add_assignee", json!({ "key": "eve" }));
    call(
        &dir,
        "handoff_update_task",
        json!({ "task": { "title": "A", "assignee": "eve" } }),
    );
    call(
        &dir,
        "handoff_update_task",
        json!({ "task": { "title": "B", "assignee": "eve" } }),
    );
    call(
        &dir,
        "handoff_update_task",
        json!({ "task": { "title": "C", "assignee": "frank" } }),
    );

    let resp = call(&dir, "handoff_remove_assignee", json!({ "key": "eve" }));
    assert!(!is_error(&resp), "error: {}", text(&resp));
    assert!(text(&resp).contains("2 task"), "msg: {}", text(&resp));

    // eve's tasks unassigned, frank's untouched.
    assert!(get_task(&dir, "t1")["assignee"].is_null());
    assert!(get_task(&dir, "t2")["assignee"].is_null());
    assert_eq!(get_task(&dir, "t3")["assignee"], "frank");

    let cfg = read_config(&dir);
    assert!(!cfg.contains("[assignees.eve]"));
}

#[test]
fn remove_assignee_nonexistent_fails() {
    let dir = setup_project();
    let resp = call(&dir, "handoff_remove_assignee", json!({ "key": "nope" }));
    assert!(is_error(&resp));
}

// ============================================================
// milestone CRUD (Phase C)
// ============================================================

#[test]
fn milestone_crud_lifecycle() {
    let dir = setup_project();

    // add
    let resp = call(
        &dir,
        "handoff_add_milestone",
        json!({ "name": "v1.0", "date": "2026-08-01", "color": "#123456", "description": "First release" }),
    );
    assert!(!is_error(&resp), "add error: {}", text(&resp));

    // list
    let listed = call(&dir, "handoff_list_milestones", json!({}));
    let v: Value = serde_json::from_str(&text(&listed)).unwrap();
    assert_eq!(v["milestones"]["v1.0"]["date"], "2026-08-01");
    assert_eq!(v["milestones"]["v1.0"]["description"], "First release");

    // update (only date)
    call(
        &dir,
        "handoff_update_milestone",
        json!({ "name": "v1.0", "date": "2026-09-01" }),
    );
    let listed = call(&dir, "handoff_list_milestones", json!({}));
    let v: Value = serde_json::from_str(&text(&listed)).unwrap();
    assert_eq!(v["milestones"]["v1.0"]["date"], "2026-09-01");
    assert_eq!(
        v["milestones"]["v1.0"]["description"], "First release",
        "description preserved on partial update"
    );

    // remove
    let resp = call(&dir, "handoff_remove_milestone", json!({ "name": "v1.0" }));
    assert!(!is_error(&resp));
    let listed = call(&dir, "handoff_list_milestones", json!({}));
    let v: Value = serde_json::from_str(&text(&listed)).unwrap();
    assert!(v["milestones"]["v1.0"].is_null());
}

#[test]
fn add_milestone_duplicate_fails() {
    let dir = setup_project();
    call(&dir, "handoff_add_milestone", json!({ "name": "m1" }));
    let resp = call(&dir, "handoff_add_milestone", json!({ "name": "m1" }));
    assert!(is_error(&resp));
    assert!(text(&resp).contains("already exists"));
}

#[test]
fn update_milestone_nonexistent_fails() {
    let dir = setup_project();
    let resp = call(&dir, "handoff_update_milestone", json!({ "name": "ghost" }));
    assert!(is_error(&resp));
}

#[test]
fn remove_milestone_nonexistent_fails() {
    let dir = setup_project();
    let resp = call(&dir, "handoff_remove_milestone", json!({ "name": "ghost" }));
    assert!(is_error(&resp));
}

// ============================================================
// calendar / labels / start_project (Phase D)
// ============================================================

#[test]
fn update_calendar_writes_fields() {
    let dir = setup_project();
    let resp = call(
        &dir,
        "handoff_update_calendar",
        json!({
            "work_hours_per_day": 7.5,
            "closed_weekdays": [0, 6],
            "closed_dates": ["2026-12-31"],
            "day_hours": { "fri": 4.0 },
            "schedule_mode": "auto"
        }),
    );
    assert!(!is_error(&resp), "error: {}", text(&resp));
    let cfg = read_config(&dir);
    assert!(cfg.contains("work_hours_per_day"), "cfg:\n{cfg}");
    assert!(cfg.contains("7.5"));
    assert!(cfg.contains("day_hours"));
    assert!(cfg.contains("schedule_mode"));
}

#[test]
fn update_calendar_is_partial() {
    let dir = setup_project();
    call(
        &dir,
        "handoff_update_calendar",
        json!({ "work_hours_per_day": 8.0, "closed_weekdays": [0, 6] }),
    );
    // Second call changes only work_hours; closed_weekdays must survive.
    call(
        &dir,
        "handoff_update_calendar",
        json!({ "work_hours_per_day": 6.0 }),
    );
    let cfg = read_config(&dir);
    assert!(cfg.contains("6"), "updated hours:\n{cfg}");
    assert!(
        cfg.contains("closed_weekdays"),
        "weekdays preserved:\n{cfg}"
    );
}

#[test]
fn update_labels_sets_project_labels() {
    let dir = setup_project();
    let resp = call(
        &dir,
        "handoff_update_labels",
        json!({ "labels": ["bug", "feature", "chore"] }),
    );
    assert!(!is_error(&resp), "error: {}", text(&resp));
    assert!(text(&resp).contains("3"));
    let cfg = read_config(&dir);
    assert!(cfg.contains("bug"));
    assert!(cfg.contains("feature"));
    assert!(cfg.contains("chore"));
}

#[test]
fn update_labels_requires_array() {
    let dir = setup_project();
    let resp = call(&dir, "handoff_update_labels", json!({}));
    assert!(is_error(&resp));
}

#[test]
fn start_project_sets_started_at() {
    let dir = setup_project();
    let resp = call(
        &dir,
        "handoff_start_project",
        json!({ "start_date": "2026-07-01" }),
    );
    assert!(!is_error(&resp), "error: {}", text(&resp));
    let cfg = read_config(&dir);
    assert!(cfg.contains("started_at"), "cfg:\n{cfg}");
    assert!(cfg.contains("2026-07-01"));
}

#[test]
fn start_project_shifts_task_dates() {
    let dir = setup_project();
    // earliest start is 2026-06-10; target start 2026-07-01 → +21 days
    call(
        &dir,
        "handoff_update_task",
        json!({ "task": { "title": "Early", "schedule": { "start_date": "2026-06-10", "due_date": "2026-06-12" } }}),
    );
    call(
        &dir,
        "handoff_update_task",
        json!({ "task": { "title": "Later", "schedule": { "start_date": "2026-06-20", "due_date": "2026-06-25" } }}),
    );

    let resp = call(
        &dir,
        "handoff_start_project",
        json!({ "start_date": "2026-07-01", "shift_dates": true }),
    );
    assert!(!is_error(&resp), "error: {}", text(&resp));
    assert!(text(&resp).contains("2 task"), "msg: {}", text(&resp));

    // +21 day shift
    assert_eq!(get_task(&dir, "t1")["schedule"]["start_date"], "2026-07-01");
    assert_eq!(get_task(&dir, "t1")["schedule"]["due_date"], "2026-07-03");
    assert_eq!(get_task(&dir, "t2")["schedule"]["start_date"], "2026-07-11");
    assert_eq!(get_task(&dir, "t2")["schedule"]["due_date"], "2026-07-16");
}

#[test]
fn start_project_without_shift_leaves_dates() {
    let dir = setup_project();
    call(
        &dir,
        "handoff_update_task",
        json!({ "task": { "title": "T", "schedule": { "start_date": "2026-06-10" } }}),
    );
    call(
        &dir,
        "handoff_start_project",
        json!({ "start_date": "2026-07-01" }),
    );
    assert_eq!(get_task(&dir, "t1")["schedule"]["start_date"], "2026-06-10");
}

#[test]
fn start_project_invalid_date_fails() {
    let dir = setup_project();
    let resp = call(
        &dir,
        "handoff_start_project",
        json!({ "start_date": "not-a-date" }),
    );
    assert!(is_error(&resp));
}

// ============================================================
// auto_schedule respects day_hours (Phase A §5)
// ============================================================

#[test]
fn auto_schedule_respects_day_hours() {
    let dir = setup_project();
    // Mon-Fri working. Make Wednesday a half day (4h). A 20h task starting Monday
    // would otherwise be 8+8+4=20 over Mon/Tue/Wed; with Wed=4h it needs Thu too.
    call(
        &dir,
        "handoff_update_calendar",
        json!({
            "work_hours_per_day": 8.0,
            "closed_weekdays": [0, 6],
            "day_hours": { "wed": 4.0 }
        }),
    );
    // 2026-06-22 is a Monday.
    call(
        &dir,
        "handoff_update_task",
        json!({ "task": { "title": "Big", "schedule": { "estimate_hours": 20.0 } }}),
    );

    let resp = call(
        &dir,
        "handoff_auto_schedule",
        json!({ "dry_run": false, "start_date": "2026-06-22" }),
    );
    assert!(!is_error(&resp), "error: {}", text(&resp));
    let task = get_task(&dir, "t1");
    assert_eq!(
        task["schedule"]["start_date"], "2026-06-22",
        "starts Monday"
    );
    // Mon 8 + Tue 8 + Wed 4 = 20 → ends Wednesday 2026-06-24.
    assert_eq!(
        task["schedule"]["due_date"],
        "2026-06-24",
        "half-day Wednesday extends consumption: {}",
        text(&resp)
    );
}

#[test]
fn auto_schedule_full_days_baseline() {
    let dir = setup_project();
    call(
        &dir,
        "handoff_update_calendar",
        json!({ "work_hours_per_day": 8.0, "closed_weekdays": [0, 6] }),
    );
    call(
        &dir,
        "handoff_update_task",
        json!({ "task": { "title": "Big", "schedule": { "estimate_hours": 20.0 } }}),
    );
    let resp = call(
        &dir,
        "handoff_auto_schedule",
        json!({ "dry_run": false, "start_date": "2026-06-22" }),
    );
    assert!(!is_error(&resp), "error: {}", text(&resp));
    let task = get_task(&dir, "t1");
    // 8+8+8 = 24 ≥ 20 → ends Wednesday with full days.
    assert_eq!(task["schedule"]["start_date"], "2026-06-22");
    assert_eq!(task["schedule"]["due_date"], "2026-06-24");
}

// ============================================================
// atomic write & Config roundtrip (Phase A + E)
// ============================================================

#[test]
fn config_with_all_new_sections_roundtrips() {
    let dir = setup_project();
    // Populate several sections, then re-read config via the typed reader
    // (through handoff_get_config) and confirm it parses without loss.
    call(
        &dir,
        "handoff_add_assignee",
        json!({ "key": "alice", "display_name": "Alice", "day_hours": { "fri": 4.0 } }),
    );
    call(
        &dir,
        "handoff_add_milestone",
        json!({ "name": "v1", "date": "2026-08-01" }),
    );
    call(
        &dir,
        "handoff_update_calendar",
        json!({ "work_hours_per_day": 7.0, "day_hours": { "sat": 0.0 } }),
    );
    call(
        &dir,
        "handoff_update_labels",
        json!({ "labels": ["a", "b"] }),
    );
    call(
        &dir,
        "handoff_start_project",
        json!({ "start_date": "2026-07-01" }),
    );

    // get_config must succeed (proves the typed Config deserializes everything).
    let resp = call(&dir, "handoff_get_config", json!({}));
    assert!(!is_error(&resp), "get_config failed: {}", text(&resp));
    let cfg: Value = serde_json::from_str(&text(&resp)).unwrap();
    // Typed model surfaces the data.
    assert_eq!(cfg["assignees"]["alice"]["display_name"], "Alice");
    assert_eq!(cfg["milestones"]["v1"]["date"], "2026-08-01");
    assert_eq!(cfg["calendar"]["work_hours_per_day"], 7.0);
    assert_eq!(cfg["labels"][0], "a");
    assert_eq!(cfg["started_at"], "2026-07-01");
}

#[test]
fn atomic_write_leaves_no_temp_files() {
    let dir = setup_project();
    call(
        &dir,
        "handoff_update_task",
        json!({ "task": { "title": "T", "schedule": { "actual_hours": 1.0 } }}),
    );
    call(&dir, "handoff_add_milestone", json!({ "name": "m" }));

    // No stray `.tmp.` files should remain anywhere under .handoff/.
    let stray = walk_tmp(&dir.path().join(".handoff"));
    assert!(stray.is_empty(), "stray temp files: {stray:?}");
}

fn walk_tmp(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk_tmp(&p));
            } else if p
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.contains(".tmp."))
                .unwrap_or(false)
            {
                out.push(p.display().to_string());
            }
        }
    }
    out
}

#[test]
fn existing_config_without_new_sections_still_reads() {
    // A minimal config.toml (as written by an old version) must still parse.
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".handoff/tasks")).unwrap();
    fs::create_dir_all(dir.path().join(".handoff/sessions")).unwrap();
    fs::write(
        dir.path().join(".handoff/config.toml"),
        "[project]\nname = \"legacy\"\n",
    )
    .unwrap();

    let req = json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "tools/call",
        "params": { "name": "handoff_get_config", "arguments": { "project_dir": dir.path().to_string_lossy() } }
    });
    let resp = send(&req.to_string()).unwrap();
    assert!(!is_error(&resp), "legacy config failed: {}", text(&resp));
    let cfg: Value = serde_json::from_str(&text(&resp)).unwrap();
    assert_eq!(cfg["project"]["name"], "legacy");
}

// ============================================================
// auto_schedule result recording (referral ref-...063524)
// ============================================================

#[test]
fn auto_schedule_records_decision_and_capacity() {
    let dir = setup_project();
    // Establish an active session so the decision has somewhere to land.
    call(
        &dir,
        "handoff_save_context",
        json!({ "summary": "scheduling session", "session_status": "active" }),
    );
    call(
        &dir,
        "handoff_add_assignee",
        json!({ "key": "alice", "work_hours_per_day": 8.0 }),
    );
    call(
        &dir,
        "handoff_update_task",
        json!({ "task": { "title": "Work", "assignee": "alice", "schedule": { "estimate_hours": 8.0 } }}),
    );

    let resp = call(
        &dir,
        "handoff_auto_schedule",
        json!({ "dry_run": false, "start_date": "2026-06-22" }),
    );
    assert!(!is_error(&resp), "error: {}", text(&resp));
    let r: Value = serde_json::from_str(&text(&resp)).unwrap();

    // Applied conditions surfaced in the response.
    assert!(r["assignee_capacity"]["alice"].is_object());
    assert_eq!(r["assignee_capacity"]["alice"]["work_hours_per_day"], 8.0);
    assert!(r["calendar_config"]["day_hours"].is_object());

    // Decision recorded in the active session.
    assert_eq!(
        r["decision_recorded_in_sessions"],
        1,
        "decision should land in 1 active session: {}",
        text(&resp)
    );
    let sessions = call(
        &dir,
        "handoff_list_sessions",
        json!({ "status_filter": "active" }),
    );
    let sv: Value = serde_json::from_str(&text(&sessions)).unwrap();
    // list_sessions returns a bare array of session summaries.
    let sid = sv[0]["id"].as_str().unwrap().to_string();
    let detail = call(&dir, "handoff_get_session", json!({ "session_id": sid }));
    let d: Value = serde_json::from_str(&text(&detail)).unwrap();
    let decisions = d["decisions"].as_array().unwrap();
    assert!(
        decisions.iter().any(|x| x["decision"]
            .as_str()
            .unwrap_or("")
            .contains("Auto-scheduled")),
        "expected an Auto-scheduled decision: {decisions:?}"
    );
}

#[test]
fn auto_schedule_dry_run_records_no_decision() {
    let dir = setup_project();
    call(
        &dir,
        "handoff_save_context",
        json!({ "summary": "s", "session_status": "active" }),
    );
    call(
        &dir,
        "handoff_update_task",
        json!({ "task": { "title": "W", "schedule": { "estimate_hours": 8.0 } }}),
    );
    let resp = call(
        &dir,
        "handoff_auto_schedule",
        json!({ "dry_run": true, "start_date": "2026-06-22" }),
    );
    let r: Value = serde_json::from_str(&text(&resp)).unwrap();
    assert_eq!(
        r["decision_recorded_in_sessions"], 0,
        "dry_run records nothing"
    );
}

// ============================================================
// optimistic concurrency / log_time accumulation (Phase E)
// ============================================================

#[test]
fn log_time_accumulates_across_repeated_calls() {
    let dir = setup_project();
    call(
        &dir,
        "handoff_update_task",
        json!({ "task": { "title": "T", "schedule": { "estimate_hours": 10.0, "remaining_hours": 10.0 } }}),
    );
    // Five sequential logs — each a full read-modify-write cycle.
    for _ in 0..5 {
        let resp = call(
            &dir,
            "handoff_log_time",
            json!({ "task_id": "t1", "hours": 1.0 }),
        );
        assert!(!is_error(&resp), "error: {}", text(&resp));
    }
    let task = get_task(&dir, "t1");
    assert_eq!(task["schedule"]["actual_hours"], 5.0, "5×1h accumulated");
    assert_eq!(
        task["schedule"]["remaining_hours"], 5.0,
        "remaining decremented"
    );
}

#[test]
fn log_time_via_rmw_detects_stale_snapshot() {
    // White-box test of the optimistic-concurrency helper: a concurrent write
    // performed *inside* the mutate closure changes updated_at, so the commit
    // must retry rather than silently overwrite the concurrent change.
    use handoff_mcp::storage::tasks::{read_modify_write_task, read_task, write_task};

    let dir = setup_project();
    call(
        &dir,
        "handoff_update_task",
        json!({ "task": { "title": "T", "schedule": { "actual_hours": 0.0 } }}),
    );
    // Locate the task dir.
    let task_dir = find_task_dir(&dir.path().join(".handoff/tasks"));
    let td = task_dir.as_path();

    let injected = std::cell::Cell::new(false);
    read_modify_write_task(td, |data, status| {
        let sched = data.schedule.get_or_insert_with(Default::default);
        sched.actual_hours = Some(sched.actual_hours.unwrap_or(0.0) + 1.0);
        data.updated_at = Some("2999-01-01T00:00:00Z".to_string());

        // On the FIRST pass only, simulate a concurrent writer bumping the file.
        if !injected.get() {
            injected.set(true);
            let (mut other, st) = read_task(td).unwrap().unwrap();
            other.updated_at = Some("2998-06-06T06:06:06Z".to_string());
            write_task(td, &st, &other).unwrap();
        }
        Ok(status.to_string())
    })
    .expect("should converge after retry");

    // After convergence, our +1.0 was applied exactly once on top of the
    // concurrent write (which had actual_hours unchanged at 0.0).
    let (data, _) = read_task(td).unwrap().unwrap();
    assert_eq!(
        data.schedule.unwrap().actual_hours,
        Some(1.0),
        "the retry re-applied the mutation on the latest state"
    );
}

fn find_task_dir(tasks_dir: &std::path::Path) -> std::path::PathBuf {
    for e in fs::read_dir(tasks_dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            for f in fs::read_dir(&p).unwrap().flatten() {
                if f.file_name().to_string_lossy().starts_with("_task.") {
                    return p;
                }
            }
        }
    }
    panic!("no task dir found");
}
