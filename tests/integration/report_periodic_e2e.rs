//! REQ-PDF-036/037/038: `handoff_report generate` for the `daily`,
//! `quarterly` and `yearly` report types collects its own data from tasks,
//! `time_log.jsonl`, `events.jsonl` and the metrics snapshots, and renders
//! the built-in templates.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use handoff_mcp::storage::events::{append_event, EventRecord, EVENT_TASK_STATUS_CHANGED};
use handoff_mcp::storage::tasks::{write_task, Schedule, TaskData};
use handoff_mcp::storage::time_log::{append_time_log, TimeLogEntry};
use serde_json::{json, Value};
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

/// Calls a tool and asserts success.
fn call_ok(name: &str, arguments: Value) -> Value {
    let resp = call(name, arguments.clone());
    assert!(
        !is_error(&resp),
        "{name} {arguments} failed: {}",
        text(&resp)
    );
    serde_json::from_str(&text(&resp)).unwrap_or(Value::Null)
}

fn setup() -> (TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let pd = dir.path().to_string_lossy().to_string();
    call_ok(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "remaining-reports" }),
    );
    (dir, pd)
}

fn handoff(dir: &TempDir) -> PathBuf {
    dir.path().join(".handoff")
}

fn task(id: &str, f: impl FnOnce(&mut TaskData)) -> TaskData {
    let mut data = TaskData {
        id: id.into(),
        title: format!("Title of {id}"),
        notes: None,
        priority: None,
        created_at: None,
        updated_at: None,
        completed_at: None,
        labels: vec![],
        links: vec![],
        task_links: vec![],
        done_criteria: vec![],
        schedule: None,
        dependencies: vec![],
        order: None,
        assignee: None,
        lock: None,
        scope_paths: vec![],
        extra: HashMap::new(),
    };
    f(&mut data);
    data
}

fn put_task(handoff: &Path, status: &str, data: &TaskData) {
    let dir = handoff.join("tasks").join(&data.id);
    std::fs::create_dir_all(&dir).unwrap();
    write_task(&dir, status, data).unwrap();
}

fn sched(s: &mut TaskData, f: impl FnOnce(&mut Schedule)) {
    f(s.schedule.get_or_insert_with(Default::default));
}

fn log(handoff: &Path, ts: &str, task_id: &str, hours: f64) {
    append_time_log(
        handoff,
        &TimeLogEntry {
            ts: ts.into(),
            task_id: task_id.into(),
            hours,
            agent_id: None,
            note: None,
            source: None,
        },
    )
    .unwrap();
}

fn snapshot(handoff: &Path, date: &str, done: u64, total: u64, actual: f64) {
    let dir = handoff.join("metrics_snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    let doc = json!({
        "schema_version": 1,
        "date": date,
        "captured_at": format!("{date}T12:00:00+00:00"),
        "metrics": {
            "total": total,
            "by_status": { "done": done, "todo": total - done },
            "completion_percent": (done as f64 / total as f64 * 1000.0).round() / 10.0,
            "total_actual_hours": actual,
            "total_remaining_hours": 10.0,
            "overdue_count": 1,
        },
    });
    std::fs::write(
        dir.join(format!("{date}.json")),
        serde_json::to_vec_pretty(&doc).unwrap(),
    )
    .unwrap();
}

fn generate_raw(pd: &str, report_type: &str, scope: Value) -> Value {
    call(
        "handoff_report",
        json!({
            "project_dir": pd,
            "action": "generate",
            "report_type": report_type,
            "scope": scope,
        }),
    )
}

fn generate(pd: &str, report_type: &str, scope: Value) -> Value {
    let resp = generate_raw(pd, report_type, scope.clone());
    assert!(
        !is_error(&resp),
        "generate {report_type} {scope} failed: {}",
        text(&resp)
    );
    serde_json::from_str(&text(&resp)).unwrap()
}

fn generate_err(pd: &str, report_type: &str, scope: Value) -> String {
    let resp = generate_raw(pd, report_type, scope.clone());
    assert!(
        is_error(&resp),
        "generate {report_type} {scope} should fail: {}",
        text(&resp)
    );
    text(&resp)
}

fn body(dir: &TempDir, out: &Value) -> String {
    let path = out["report"]["output_path"].as_str().unwrap();
    std::fs::read_to_string(handoff(dir).join(path)).unwrap()
}

/// The text of the `## <title>` section (up to the next `## ` heading).
fn section(md: &str, title: &str) -> String {
    let start = md
        .find(&format!("## {title}"))
        .unwrap_or_else(|| panic!("missing section '{title}' in:\n{md}"));
    let rest = &md[start + 3..];
    let end = rest.find("\n## ").map_or(rest.len(), |i| i + 3);
    md[start..start + end].to_string()
}

fn no_reports_written(dir: &TempDir) {
    let reports = handoff(dir).join("reports");
    assert!(
        !reports.exists() || std::fs::read_dir(reports).unwrap().next().is_none(),
        "a failed generate must write nothing"
    );
}

// ------------------------------------------------------------------ seed

fn status_event(handoff: &Path, ts: &str, task_id: &str, from: &str, to: &str) {
    append_event(
        handoff,
        EventRecord {
            ts: ts.into(),
            event: EVENT_TASK_STATUS_CHANGED.into(),
            task_id: Some(task_id.into()),
            agent_id: None,
            session_id: None,
            detail: Some(json!({ "from": from, "to": to }).to_string()),
        },
    )
    .unwrap();
}

/// Work spread over 2026: Q3 (September), Q4 (October, December) and the
/// previous year, with snapshots on the days that matter to the three reports.
fn seed(dir: &TempDir) {
    let h = handoff(dir);
    put_task(
        &h,
        "done",
        &task("t1", |d| {
            d.completed_at = Some("2026-10-08T09:00:00+00:00".into());
            d.assignee = Some("alice".into());
            sched(d, |s| s.estimate_hours = Some(4.0));
        }),
    );
    put_task(
        &h,
        "done",
        &task("t2", |d| {
            d.completed_at = Some("2026-12-20T09:00:00+00:00".into());
        }),
    );
    put_task(
        &h,
        "done",
        &task("t3", |d| {
            d.completed_at = Some("2026-09-20T09:00:00+00:00".into());
        }),
    );
    put_task(
        &h,
        "done",
        &task("t4", |d| {
            d.completed_at = Some("2025-12-31T09:00:00+00:00".into());
        }),
    );
    put_task(&h, "in_progress", &task("t5", |_| {}));
    log(&h, "2026-10-08T10:00:00+00:00", "t1", 3.0);
    log(&h, "2026-10-08T11:00:00+00:00", "t5", 1.0);
    log(&h, "2026-10-07T10:00:00+00:00", "t1", 40.0);
    log(&h, "2026-12-20T10:00:00+00:00", "t2", 2.0);
    log(&h, "2026-09-20T10:00:00+00:00", "t3", 7.0);
    status_event(&h, "2026-10-08T08:00:00+00:00", "t1", "todo", "in_progress");
    status_event(&h, "2026-10-08T09:00:00+00:00", "t1", "in_progress", "done");
    status_event(&h, "2026-10-07T09:00:00+00:00", "t5", "todo", "in_progress");
    snapshot(&h, "2026-09-30", 1, 4, 41.0);
    snapshot(&h, "2026-10-06", 1, 4, 42.0);
    snapshot(&h, "2026-10-08", 2, 4, 45.0);
    snapshot(&h, "2026-12-31", 3, 4, 50.0);
}

// ------------------------------------------------------------------ daily

#[test]
fn daily_report_shows_the_day_its_status_changes_and_the_metric_delta() {
    let (dir, pd) = setup();
    seed(&dir);
    let out = generate(&pd, "daily", json!({ "period": "2026-10-08" }));

    assert_eq!(out["report"]["report_type"], "daily");
    assert_eq!(out["report"]["scope"]["from"], "2026-10-08");
    assert_eq!(out["report"]["scope"]["to"], "2026-10-08");

    let md = body(&dir, &out);
    assert!(md.starts_with("# Daily Report"), "{md}");

    let completed = section(&md, "Completed Tasks");
    assert!(completed.contains("t1"), "{completed}");
    assert!(
        !completed.contains("t2") && !completed.contains("t3"),
        "{completed}"
    );

    // 3h + 1h on the 8th; the 40h entry of the 7th is excluded.
    let log_section = section(&md, "Time Log");
    assert!(log_section.contains("**4.0**"), "{log_section}");

    let changes = section(&md, "Status Changes");
    assert!(
        changes.contains("todo") && changes.contains("in_progress") && changes.contains("done"),
        "{changes}"
    );
    assert!(!changes.contains("t5"), "{changes}");
    assert!(
        changes.find("08:00").unwrap() < changes.find("09:00").unwrap(),
        "oldest first: {changes}"
    );

    // Previous snapshot is the 6th (the 7th has none): 25% -> 50%.
    let metrics = section(&md, "Metrics");
    assert!(metrics.contains("2026-10-06"), "{metrics}");
    assert!(metrics.contains("+25"), "{metrics}");
    assert!(metrics.contains("+1"), "{metrics}");
}

#[test]
fn daily_report_handles_a_day_without_activity_or_snapshots() {
    let (dir, pd) = setup();
    let out = generate(&pd, "daily", json!({ "period": "2026-01-01" }));
    let md = body(&dir, &out);
    assert!(
        section(&md, "Metrics").contains("No metrics snapshot"),
        "{md}"
    );
    assert!(
        section(&md, "Status Changes").contains("No task status changes"),
        "{md}"
    );
    assert!(section(&md, "Completed Tasks").contains("None."), "{md}");
}

#[test]
fn daily_report_defaults_to_today_and_accepts_from_to() {
    let (dir, pd) = setup();
    let out = generate(&pd, "daily", json!({}));
    let from = out["report"]["scope"]["from"].as_str().unwrap();
    assert_eq!(from, out["report"]["scope"]["to"].as_str().unwrap());
    assert_eq!(from.len(), 10, "{from}");
    let _ = body(&dir, &out);

    let ranged = generate(
        &pd,
        "daily",
        json!({ "from": "2026-10-08", "to": "2026-10-08" }),
    );
    assert_eq!(ranged["report"]["scope"]["from"], "2026-10-08");
}

#[test]
fn daily_rejects_multi_day_bad_or_unsupported_scope_and_writes_nothing() {
    let (dir, pd) = setup();
    for scope in [
        json!({ "period": "2026-W41" }),
        json!({ "period": "2026-10" }),
        json!({ "period": "2026-Q4" }),
        json!({ "period": "2026-10-08..2026-10-09" }),
        json!({ "period": "2026-02-30" }),
        json!({ "period": "2026-10-08", "from": "2026-10-08" }),
        json!({ "from": "2026-10-08" }),
        json!({ "layers": ["unit"] }),
    ] {
        generate_err(&pd, "daily", scope);
    }
    no_reports_written(&dir);
}

// -------------------------------------------------------------- quarterly

#[test]
fn quarterly_report_aggregates_the_months_of_the_quarter() {
    let (dir, pd) = setup();
    seed(&dir);
    let out = generate(&pd, "quarterly", json!({ "period": "2026-Q4" }));

    assert_eq!(out["report"]["report_type"], "quarterly");
    assert_eq!(out["report"]["scope"]["from"], "2026-10-01");
    assert_eq!(out["report"]["scope"]["to"], "2026-12-31");

    let md = body(&dir, &out);
    assert!(md.starts_with("# Quarterly Report"), "{md}");

    let months = section(&md, "Monthly Breakdown");
    for m in ["2026-10", "2026-11", "2026-12"] {
        assert!(months.contains(m), "{months}");
    }
    assert!(!months.contains("2026-09"), "{months}");

    let completed = section(&md, "Completed Tasks");
    assert!(
        completed.contains("t1") && completed.contains("t2"),
        "{completed}"
    );
    assert!(
        !completed.contains("t3") && !completed.contains("t4"),
        "{completed}"
    );

    // 40 + 3 + 1 + 2; September's 7h is excluded.
    let summary = section(&md, "Summary");
    assert!(summary.contains("46"), "{summary}");
    assert!(!summary.contains("53"), "{summary}");

    // First snapshot of the quarter 25% (10-06), last 75% (12-31): +50.
    let trend = section(&md, "Trend");
    assert!(trend.contains("+50"), "{trend}");
}

#[test]
fn quarterly_report_defaults_to_the_current_quarter_and_accepts_ranges() {
    let (dir, pd) = setup();
    let out = generate(&pd, "quarterly", json!({}));
    let from = out["report"]["scope"]["from"].as_str().unwrap();
    let to = out["report"]["scope"]["to"].as_str().unwrap();
    assert!(
        ["-01-01", "-04-01", "-07-01", "-10-01"]
            .iter()
            .any(|s| from.ends_with(s)),
        "{from}"
    );
    assert!(
        ["-03-31", "-06-30", "-09-30", "-12-31"]
            .iter()
            .any(|s| to.ends_with(s)),
        "{to}"
    );
    let md = body(&dir, &out);
    assert!(section(&md, "Trend").contains("Fewer than two"), "{md}");

    let ranged = generate(
        &pd,
        "quarterly",
        json!({ "from": "2026-10-15", "to": "2026-11-10" }),
    );
    assert_eq!(ranged["report"]["scope"]["to"], "2026-11-10");
}

#[test]
fn quarterly_rejects_bad_or_unsupported_scope_and_writes_nothing() {
    let (dir, pd) = setup();
    for scope in [
        json!({ "period": "2026-Q5" }),
        json!({ "period": "2026-Q0" }),
        json!({ "period": "2026-Q4", "to": "2026-12-31" }),
        json!({ "to": "2026-12-31" }),
        json!({ "milestone": "beta" }),
    ] {
        generate_err(&pd, "quarterly", scope);
    }
    no_reports_written(&dir);
}

// ----------------------------------------------------------------- yearly

#[test]
fn yearly_report_aggregates_twelve_months_and_four_quarters() {
    let (dir, pd) = setup();
    seed(&dir);
    let out = generate(&pd, "yearly", json!({ "period": "2026" }));

    assert_eq!(out["report"]["report_type"], "yearly");
    assert_eq!(out["report"]["scope"]["from"], "2026-01-01");
    assert_eq!(out["report"]["scope"]["to"], "2026-12-31");

    let md = body(&dir, &out);
    assert!(md.starts_with("# Yearly Report"), "{md}");

    let months = section(&md, "Monthly Breakdown");
    for m in 1..=12 {
        assert!(months.contains(&format!("2026-{m:02}")), "{months}");
    }
    let quarters = section(&md, "Quarterly Breakdown");
    for q in ["2026-Q1", "2026-Q2", "2026-Q3", "2026-Q4"] {
        assert!(quarters.contains(q), "{quarters}");
    }

    let completed = section(&md, "Completed Tasks");
    for id in ["t1", "t2", "t3"] {
        assert!(completed.contains(id), "{completed}");
    }
    assert!(!completed.contains("t4"), "{completed}");

    // 40 + 3 + 1 + 2 + 7.
    let summary = section(&md, "Summary");
    assert!(summary.contains("53"), "{summary}");

    // 25% (09-30) -> 75% (12-31): +50.
    assert!(section(&md, "Trend").contains("+50"), "{md}");
}

#[test]
fn yearly_report_defaults_to_the_current_year() {
    let (_dir, pd) = setup();
    let out = generate(&pd, "yearly", json!({}));
    let from = out["report"]["scope"]["from"].as_str().unwrap();
    let to = out["report"]["scope"]["to"].as_str().unwrap();
    assert!(
        from.ends_with("-01-01") && to.ends_with("-12-31"),
        "{from}..{to}"
    );
    assert_eq!(&from[..4], &to[..4]);
}

#[test]
fn yearly_rejects_bad_or_unsupported_scope_and_writes_nothing() {
    let (dir, pd) = setup();
    for scope in [
        json!({ "period": "26" }),
        json!({ "period": "2026", "from": "2026-01-01" }),
        json!({ "layers": ["unit"] }),
        json!({ "assignee": "alice" }),
    ] {
        generate_err(&pd, "yearly", scope);
    }
    no_reports_written(&dir);
}

// -------------------------------------------------------------- templates

#[test]
fn the_new_report_types_are_listed_as_builtin_templates_and_filterable() {
    let (_dir, pd) = setup();
    let templates = call_ok(
        "handoff_report",
        json!({ "project_dir": pd, "action": "templates" }),
    );
    let builtin: Vec<&str> = templates["builtin"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for name in ["daily", "quarterly", "yearly"] {
        assert!(builtin.contains(&name), "{builtin:?}");
    }
    generate(&pd, "daily", json!({ "period": "2026-10-08" }));
    let listed = call_ok(
        "handoff_report",
        json!({ "project_dir": pd, "action": "list", "report_type": "daily" }),
    );
    assert_eq!(listed["reports"].as_array().unwrap().len(), 1);
}

#[test]
fn a_project_template_overrides_the_builtin_daily_template() {
    let (dir, pd) = setup();
    let templates = handoff(&dir).join("templates");
    std::fs::create_dir_all(&templates).unwrap();
    std::fs::write(
        templates.join("daily.md.hbs"),
        "DAY {{data.period.start}} done={{data.stats.completed_in_period}}",
    )
    .unwrap();
    let out = generate(&pd, "daily", json!({ "period": "2026-10-08" }));
    assert_eq!(body(&dir, &out), "DAY 2026-10-08 done=0");
}
