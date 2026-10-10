//! FR-531 / SPEC-531: `handoff_report generate` for the remaining report
//! types — `monthly` (R4), `milestone` (R6), `defect` (R7) and `completion`
//! (R8) — collects its own data from tasks, `time_log.jsonl`, metrics
//! snapshots, the milestone config and the trace report, and renders the
//! built-in templates.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

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

// ---------------------------------------------------------------- monthly

/// October 2026: W41 = 10-05..10-11, W42 = 10-12..10-18. September work and
/// snapshots must not leak in.
fn seed_monthly(dir: &TempDir) {
    let h = handoff(dir);
    put_task(
        &h,
        "done",
        &task("t1", |d| {
            d.completed_at = Some("2026-10-07T09:00:00+00:00".into());
            d.assignee = Some("alice".into());
            sched(d, |s| {
                s.estimate_hours = Some(4.0);
                s.actual_hours = Some(5.0);
            });
        }),
    );
    put_task(
        &h,
        "done",
        &task("t2", |d| {
            d.completed_at = Some("2026-10-14T09:00:00+00:00".into());
        }),
    );
    put_task(
        &h,
        "done",
        &task("t3", |d| {
            d.completed_at = Some("2026-09-20T09:00:00+00:00".into());
        }),
    );
    put_task(&h, "todo", &task("t4", |_| {}));
    log(&h, "2026-10-06T10:00:00+00:00", "t1", 3.0);
    log(&h, "2026-10-14T10:00:00+00:00", "t2", 2.0);
    log(&h, "2026-09-20T10:00:00+00:00", "t3", 40.0);
    snapshot(&h, "2026-09-30", 1, 4, 41.0);
    snapshot(&h, "2026-10-01", 1, 4, 42.0);
    snapshot(&h, "2026-10-31", 3, 4, 47.0);
}

#[test]
fn monthly_report_aggregates_weeks_and_snapshot_trend() {
    let (dir, pd) = setup();
    seed_monthly(&dir);
    let out = generate(&pd, "monthly", json!({ "period": "2026-10" }));

    assert_eq!(out["report"]["report_type"], "monthly");
    assert_eq!(out["report"]["scope"]["from"], "2026-10-01");
    assert_eq!(out["report"]["scope"]["to"], "2026-10-31");

    let md = body(&dir, &out);
    assert!(md.contains("# Monthly Report"), "{md}");
    assert!(md.contains("2026-10-01"), "{md}");

    let completed = section(&md, "Completed Tasks");
    assert!(
        completed.contains("t1") && completed.contains("t2"),
        "{completed}"
    );
    assert!(!completed.contains("t3"), "{completed}");

    // 3h + 2h inside October; the 40h September entry is excluded.
    let summary = section(&md, "Summary");
    assert!(summary.contains("5"), "{summary}");
    assert!(!summary.contains("45"), "{summary}");

    let weeks = section(&md, "Weekly Breakdown");
    assert!(weeks.contains("2026-W41"), "{weeks}");
    assert!(weeks.contains("2026-W42"), "{weeks}");
    // The first and last ISO weeks are clipped to the month.
    assert!(weeks.contains("2026-10-01"), "{weeks}");
    assert!(weeks.contains("2026-10-31"), "{weeks}");

    let trend = section(&md, "Trend");
    assert!(
        trend.contains("2026-10-01") && trend.contains("2026-10-31"),
        "{trend}"
    );
    assert!(!trend.contains("2026-09-30"), "{trend}");
    // 25% -> 75%: +50.
    assert!(trend.contains("+50"), "{trend}");
}

#[test]
fn monthly_report_defaults_to_the_current_month_and_accepts_ranges() {
    let (dir, pd) = setup();
    let out = generate(&pd, "monthly", json!({}));
    let from = out["report"]["scope"]["from"].as_str().unwrap().to_string();
    let to = out["report"]["scope"]["to"].as_str().unwrap().to_string();
    assert!(from.ends_with("-01"), "{from}");
    assert_eq!(&from[..7], &to[..7], "same month: {from}..{to}");
    let md = body(&dir, &out);
    assert!(
        section(&md, "Trend").contains("No metrics snapshots"),
        "{md}"
    );

    let ranged = generate(
        &pd,
        "monthly",
        json!({ "from": "2026-10-05", "to": "2026-11-04" }),
    );
    assert_eq!(ranged["report"]["scope"]["to"], "2026-11-04");
}

#[test]
fn monthly_rejects_bad_or_unsupported_scope_and_writes_nothing() {
    let (dir, pd) = setup();
    for scope in [
        json!({ "period": "2026-13" }),
        json!({ "period": "2026-10", "from": "2026-10-01" }),
        json!({ "from": "2026-10-01" }),
        json!({ "layers": ["unit"] }),
        json!({ "milestone": "beta" }),
    ] {
        generate_err(&pd, "monthly", scope);
    }
    no_reports_written(&dir);
}

// -------------------------------------------------------------- milestone

fn seed_milestone(dir: &TempDir, pd: &str) {
    let h = handoff(dir);
    call_ok(
        "handoff_add_milestone",
        json!({ "project_dir": pd, "name": "beta", "date": "2026-09-30",
                "description": "Public beta" }),
    );
    call_ok(
        "handoff_add_milestone",
        json!({ "project_dir": pd, "name": "ga", "date": "2026-12-01" }),
    );
    put_task(
        &h,
        "done",
        &task("t1", |d| {
            d.completed_at = Some("2026-09-28T09:00:00+00:00".into());
            sched(d, |s| {
                s.milestone = Some("beta".into());
                s.due_date = Some("2026-09-25".into());
                s.estimate_hours = Some(4.0);
                s.actual_hours = Some(6.0);
            });
        }),
    );
    put_task(
        &h,
        "in_progress",
        &task("t2", |d| {
            sched(d, |s| {
                s.milestone = Some("beta".into());
                s.due_date = Some("2026-09-29".into());
                s.estimate_hours = Some(2.0);
                s.actual_hours = Some(1.0);
            });
        }),
    );
    put_task(
        &h,
        "todo",
        &task("b1", |d| {
            d.labels = vec!["bug".into()];
            d.priority = Some("high".into());
            sched(d, |s| s.milestone = Some("beta".into()));
        }),
    );
    put_task(
        &h,
        "done",
        &task("g1", |d| {
            d.completed_at = Some("2026-09-01T09:00:00+00:00".into());
            sched(d, |s| s.milestone = Some("ga".into()));
        }),
    );
}

#[test]
fn milestone_report_compares_plan_to_actual_and_filters_by_milestone() {
    let (dir, pd) = setup();
    seed_milestone(&dir, &pd);
    let out = generate(&pd, "milestone", json!({ "milestone": "beta" }));
    assert_eq!(out["report"]["scope"]["milestone"], "beta");

    let md = body(&dir, &out);
    assert!(md.contains("# Milestone Report"), "{md}");
    assert!(md.contains("beta"), "{md}");

    let overview = section(&md, "Schedule");
    assert!(overview.contains("2026-09-30"), "planned date: {overview}");
    assert!(overview.contains("Public beta"), "{overview}");
    // Open tasks and a planned date in the past: overdue.
    assert!(overview.contains("overdue"), "{overview}");

    let tasks = section(&md, "Tasks");
    assert!(
        tasks.contains("t1") && tasks.contains("t2") && tasks.contains("b1"),
        "{tasks}"
    );
    assert!(!tasks.contains("g1"), "other milestone excluded: {tasks}");

    // Effort: estimate 6h (4 + 2), actual 7h (6 + 1).
    let effort = section(&md, "Effort");
    assert!(effort.contains("6") && effort.contains("7"), "{effort}");

    let quality = section(&md, "Quality");
    assert!(quality.contains("b1"), "open bug listed: {quality}");
}

#[test]
fn milestone_report_for_a_completed_milestone_shows_the_actual_date() {
    let (dir, pd) = setup();
    seed_milestone(&dir, &pd);
    let out = generate(&pd, "milestone", json!({ "milestone": "ga" }));
    let md = body(&dir, &out);
    let overview = section(&md, "Schedule");
    assert!(overview.contains("2026-12-01"), "{overview}");
    assert!(overview.contains("2026-09-01"), "actual date: {overview}");
    assert!(overview.contains("achieved"), "{overview}");
}

#[test]
fn milestone_requires_a_known_milestone_and_rejects_other_scope() {
    let (dir, pd) = setup();
    seed_milestone(&dir, &pd);
    let missing = generate_err(&pd, "milestone", json!({}));
    assert!(missing.contains("scope.milestone"), "{missing}");
    let unknown = generate_err(&pd, "milestone", json!({ "milestone": "nope" }));
    assert!(unknown.contains("nope"), "{unknown}");
    generate_err(
        &pd,
        "milestone",
        json!({ "milestone": "beta", "period": "2026-10" }),
    );
    generate_err(
        &pd,
        "milestone",
        json!({ "milestone": "beta", "layers": ["unit"] }),
    );
    no_reports_written(&dir);
}

// ----------------------------------------------------------------- defect

const FIXTURE_LAYERS: [(&str, &str, &str); 2] = [
    (
        "requirements-remaining",
        "requirement",
        "# Requirements\n\n### REQ-930 Something\n\nBody.\n",
    ),
    (
        "acceptance-remaining",
        "acceptance",
        "# Acceptance\n\n### AT-930 Verify REQ-930\n\n- verifies: REQ-930\n\nSteps.\n",
    ),
];

fn seed_trace(pd: &str) {
    for (slug, layer, body) in FIXTURE_LAYERS {
        call_ok(
            "handoff_doc_save",
            json!({ "project_dir": pd, "slug": slug, "title": slug, "layer": layer, "body": body }),
        );
    }
}

fn record(pd: &str, result: &str) {
    let out = call_ok(
        "handoff_trace_update",
        json!({"project_dir": pd, "ops": [{"op": "record", "item": "AT-930", "result": result}]}),
    );
    assert!(out.get("failed").is_none(), "{out}");
}

fn seed_defects(dir: &TempDir, pd: &str) {
    seed_trace(pd);
    record(pd, "fail");
    let h = handoff(dir);
    put_task(
        &h,
        "todo",
        &task("bug1", |d| {
            d.labels = vec!["bug".into(), "bug:fix".into()];
            d.priority = Some("high".into());
            d.assignee = Some("alice".into());
            d.created_at = Some("2026-10-02T09:00:00+00:00".into());
        }),
    );
    put_task(
        &h,
        "done",
        &task("bug2", |d| {
            d.labels = vec!["bug".into(), "bug:defer".into()];
            d.priority = Some("low".into());
            d.assignee = Some("bob".into());
            d.created_at = Some("2026-09-02T09:00:00+00:00".into());
            d.completed_at = Some("2026-09-05T09:00:00+00:00".into());
        }),
    );
    put_task(&h, "todo", &task("feat1", |_| {}));
    // Link bug1 to the failing acceptance item.
    call_ok(
        "handoff_update_task",
        json!({"project_dir": pd, "task": {"id": "bug1", "requirement_ids": ["AT-930"]}}),
    );
}

#[test]
fn defect_report_lists_bug_tasks_and_failing_items() {
    let (dir, pd) = setup();
    seed_defects(&dir, &pd);
    let out = generate(&pd, "defect", json!({}));
    let md = body(&dir, &out);
    assert!(md.contains("# Defect Report"), "{md}");

    let summary = section(&md, "Summary");
    assert!(summary.contains("2"), "two bug tasks: {summary}");

    let defects = section(&md, "Defects");
    assert!(
        defects.contains("bug1") && defects.contains("bug2"),
        "{defects}"
    );
    assert!(!defects.contains("feat1"), "non-bug excluded: {defects}");
    assert!(defects.contains("high"), "severity: {defects}");
    assert!(
        defects.contains("fix") && defects.contains("defer"),
        "disposition: {defects}"
    );
    assert!(defects.contains("AT-930"), "linked item: {defects}");

    let failing = section(&md, "Failing Items");
    assert!(failing.contains("AT-930"), "{failing}");
    assert!(failing.contains("bug1"), "bug task attached: {failing}");
}

#[test]
fn defect_scope_filters_by_period_assignee_and_layer() {
    let (dir, pd) = setup();
    seed_defects(&dir, &pd);

    // created_at in October: only bug1.
    let md = body(
        &dir,
        &generate(&pd, "defect", json!({ "period": "2026-10-01..2026-10-31" })),
    );
    let defects = section(&md, "Defects");
    assert!(
        defects.contains("bug1") && !defects.contains("bug2"),
        "{defects}"
    );

    let md = body(&dir, &generate(&pd, "defect", json!({ "assignee": "bob" })));
    let defects = section(&md, "Defects");
    assert!(
        defects.contains("bug2") && !defects.contains("bug1"),
        "{defects}"
    );

    // Layer filter: bug1 is linked to an acceptance item; bug2 is unlinked.
    let md = body(
        &dir,
        &generate(&pd, "defect", json!({ "layers": ["acceptance"] })),
    );
    let defects = section(&md, "Defects");
    assert!(
        defects.contains("bug1") && !defects.contains("bug2"),
        "{defects}"
    );
    let md = body(
        &dir,
        &generate(&pd, "defect", json!({ "layers": ["unit"] })),
    );
    assert!(!section(&md, "Defects").contains("bug1"), "{md}");
    assert!(!section(&md, "Failing Items").contains("AT-930"), "{md}");
}

#[test]
fn defect_report_without_any_bugs_says_so_and_rejects_unsupported_scope() {
    let (dir, pd) = setup();
    let out = generate(&pd, "defect", json!({}));
    let md = body(&dir, &out);
    assert!(section(&md, "Defects").contains("No defects"), "{md}");
    generate_err(&pd, "defect", json!({ "campaign": "x" }));
    generate_err(&pd, "defect", json!({ "milestone": "beta" }));
}

// ------------------------------------------------------------- completion

fn make_complete(dir: &TempDir, pd: &str) {
    seed_trace(pd);
    put_task(
        &handoff(dir),
        "done",
        &task("impl1", |d| {
            d.created_at = Some("2026-09-01T09:00:00+00:00".into());
            d.completed_at = Some("2026-10-03T09:00:00+00:00".into());
            d.assignee = Some("alice".into());
            sched(d, |s| {
                s.estimate_hours = Some(10.0);
                s.actual_hours = Some(12.0);
                s.milestone = Some("beta".into());
            });
        }),
    );
    call_ok(
        "handoff_update_task",
        json!({"project_dir": pd, "task": {"id": "impl1", "requirement_ids": ["REQ-930"]}}),
    );
    log(&handoff(dir), "2026-09-10T10:00:00+00:00", "impl1", 12.0);
    record(pd, "pass");
}

fn approve_all_layers(pd: &str) -> Value {
    call_ok(
        "handoff_trace_update",
        json!({"project_dir": pd, "executor_kind": "human", "executor_id": "reviewer",
        "ops": [
            {"op": "set_layer_status", "layer": "requirement", "status": "approved"},
            {"op": "set_layer_status", "layer": "acceptance", "status": "approved"},
        ]}),
    )
}

#[test]
fn completion_report_is_refused_until_every_layer_is_approved() {
    let (dir, pd) = setup();
    make_complete(&dir, &pd);
    let err = generate_err(&pd, "completion", json!({}));
    assert!(err.contains("project_status"), "{err}");
    assert!(
        err.contains("verified"),
        "reports the current status: {err}"
    );
    no_reports_written(&dir);
}

#[test]
fn completion_report_integrates_everything_once_complete() {
    let (dir, pd) = setup();
    make_complete(&dir, &pd);
    call_ok(
        "handoff_add_milestone",
        json!({ "project_dir": pd, "name": "beta", "date": "2026-10-01" }),
    );
    // An approved verification report is listed as an attachment.
    let verification = generate(&pd, "verification", json!({}));
    let vid = verification["report"]["report_id"]
        .as_str()
        .unwrap()
        .to_string();
    call_ok(
        "handoff_report",
        json!({"project_dir": pd, "action": "submit", "report_id": vid}),
    );
    call_ok(
        "handoff_report",
        json!({"project_dir": pd, "action": "approve", "report_id": vid, "reviewer": "qa"}),
    );
    let applied = approve_all_layers(&pd);
    assert!(applied.get("failed").is_none(), "{applied}");

    let out = generate(&pd, "completion", json!({ "label": "v1.0" }));
    assert_eq!(out["report"]["report_type"], "completion");
    let md = body(&dir, &out);
    assert!(md.contains("# Completion Report"), "{md}");
    assert!(md.contains("v1.0"), "{md}");
    assert!(md.contains("remaining-reports"), "project name: {md}");

    let overview = section(&md, "Overview");
    assert!(overview.contains("complete"), "{overview}");
    assert!(
        overview.contains("2026-10-03"),
        "completion date: {overview}"
    );

    let verification = section(&md, "Verification");
    assert!(
        verification.contains("requirement") && verification.contains("acceptance"),
        "{verification}"
    );
    assert!(verification.contains("approved"), "{verification}");

    let effort = section(&md, "Effort");
    assert!(effort.contains("12") && effort.contains("10"), "{effort}");
    assert!(effort.contains("alice"), "{effort}");

    assert!(section(&md, "Milestones").contains("beta"), "{md}");
    assert!(section(&md, "Defects").contains("No defects"), "{md}");
    let attachments = section(&md, "Approved Reports");
    assert!(attachments.contains(&vid), "{attachments}");
}

#[test]
fn completion_rejects_scope_fields_it_would_ignore() {
    let (dir, pd) = setup();
    make_complete(&dir, &pd);
    approve_all_layers(&pd);
    generate_err(&pd, "completion", json!({ "period": "2026-10" }));
    generate_err(&pd, "completion", json!({ "milestone": "beta" }));
    generate(&pd, "completion", json!({}));
}
