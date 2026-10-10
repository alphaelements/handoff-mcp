//! FR-515 / SPEC-515: `handoff_report generate report_type=weekly` collects
//! its data from tasks, `time_log.jsonl`, `events.jsonl`, milestones, and the
//! trace report, and renders the weekly template.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use handoff_mcp::storage::events::{append_event, EventRecord, EVENT_TASK_STATUS_CHANGED};
use handoff_mcp::storage::tasks::{write_task, DoneCriterion, Schedule, TaskData};
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

fn setup() -> (TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let pd = dir.path().to_string_lossy().to_string();
    let resp = call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "weekly-test" }),
    );
    assert!(!is_error(&resp), "init failed: {}", text(&resp));
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

fn status_event(handoff: &Path, ts: &str, task_id: &str, to: &str) {
    append_event(
        handoff,
        EventRecord {
            ts: ts.into(),
            event: EVENT_TASK_STATUS_CHANGED.into(),
            task_id: Some(task_id.into()),
            agent_id: None,
            session_id: None,
            detail: Some(json!({ "from": "in_progress", "to": to }).to_string()),
        },
    )
    .unwrap();
}

/// Seeds a project for 2026-W41 (Mon 2026-10-05 .. Sun 2026-10-11).
fn seed(dir: &TempDir) {
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
                s.milestone = Some("beta".into());
            });
        }),
    );
    // Completed last week: must not be listed.
    put_task(
        &h,
        "done",
        &task("t2", |d| {
            d.completed_at = Some("2026-10-01T09:00:00+00:00".into());
        }),
    );
    // No completed_at, found through the status_changed event.
    put_task(&h, "done", &task("t3", |_| {}));
    put_task(
        &h,
        "in_progress",
        &task("t4", |d| {
            d.done_criteria = vec![
                DoneCriterion {
                    item: "a".into(),
                    checked: true,
                },
                DoneCriterion {
                    item: "b".into(),
                    checked: false,
                },
            ];
            sched(d, |s| {
                s.remaining_hours = Some(1.5);
                s.due_date = Some("2026-10-14".into());
                s.milestone = Some("beta".into());
            });
        }),
    );
    put_task(
        &h,
        "blocked",
        &task("t5", |d| d.dependencies = vec!["t4".into()]),
    );
    put_task(
        &h,
        "todo",
        &task("t6", |d| {
            sched(d, |s| s.due_date = Some("2026-10-13".into()))
        }),
    );

    log(&h, "2026-10-06T10:00:00+00:00", "t1", 3.0);
    log(&h, "2026-10-08T10:00:00+00:00", "t1", 1.5);
    log(&h, "2026-10-09T10:00:00+00:00", "t4", 2.0);
    log(&h, "2026-10-02T10:00:00+00:00", "t1", 40.0); // previous week
    status_event(&h, "2026-10-08T12:00:00+00:00", "t3", "done");

    let resp = call(
        "handoff_add_milestone",
        json!({ "project_dir": dir.path(), "name": "beta", "date": "2026-11-01" }),
    );
    assert!(!is_error(&resp), "{}", text(&resp));
}

fn generate(pd: &str, scope: Value) -> Value {
    let resp = call(
        "handoff_report",
        json!({
            "project_dir": pd,
            "action": "generate",
            "report_type": "weekly",
            "scope": scope,
        }),
    );
    assert!(!is_error(&resp), "generate failed: {}", text(&resp));
    serde_json::from_str(&text(&resp)).unwrap()
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

#[test]
fn iso_week_report_collects_tasks_hours_and_milestones() {
    let (dir, pd) = setup();
    seed(&dir);
    let out = generate(&pd, json!({ "period": "2026-W41" }));

    // The resolved range is recorded on the report's scope.
    assert_eq!(out["report"]["scope"]["period"], "2026-W41");
    assert_eq!(out["report"]["scope"]["from"], "2026-10-05");
    assert_eq!(out["report"]["scope"]["to"], "2026-10-11");

    let md = body(&dir, &out);
    assert!(md.contains("# Weekly Report"), "{md}");
    assert!(md.contains("2026-10-05"), "{md}");
    assert!(md.contains("2026-10-11"), "{md}");

    let section = |title: &str| section(&md, title);

    // Completed: t1 via completed_at, t3 via the event; t2 is last week's.
    let completed = section("Completed Tasks");
    assert!(completed.contains("t1"), "{completed}");
    assert!(completed.contains("t3"), "{completed}");
    assert!(!completed.contains("t2"), "{completed}");
    assert!(completed.contains("alice"), "{completed}");

    // Hours: 3.0 + 1.5 + 2.0 inside the week; the 40h entry is excluded.
    let summary = section("Summary");
    assert!(summary.contains("6.5"), "{summary}");
    assert!(!summary.contains("46.5"), "{summary}");

    let in_progress = section("In Progress");
    assert!(in_progress.contains("t4"), "{in_progress}");
    assert!(
        in_progress.contains("50"),
        "progress 1/2 criteria: {in_progress}"
    );
    assert!(in_progress.contains("2026-10-14"), "{in_progress}");

    let blockers = section("Blockers");
    assert!(blockers.contains("t5"), "{blockers}");

    let milestones = section("Milestones");
    assert!(milestones.contains("beta"), "{milestones}");
    assert!(milestones.contains("2026-11-01"), "{milestones}");

    let next = section("Next Week");
    assert!(next.contains("t6"), "{next}");
    assert!(next.contains("t4"), "{next}");
}

#[test]
fn date_range_period_is_equivalent_to_the_iso_week() {
    let (dir, pd) = setup();
    seed(&dir);
    let out = generate(&pd, json!({ "period": "2026-10-05..2026-10-11" }));
    assert_eq!(out["report"]["scope"]["from"], "2026-10-05");
    assert_eq!(out["report"]["scope"]["to"], "2026-10-11");
    let md = body(&dir, &out);
    assert!(md.contains("t1") && md.contains("t3"), "{md}");
}

#[test]
fn from_and_to_scope_fields_select_the_period() {
    let (dir, pd) = setup();
    seed(&dir);
    // Only the first two days of the week: t1's completion (10-07) is out.
    let out = generate(&pd, json!({ "from": "2026-10-05", "to": "2026-10-06" }));
    let md = body(&dir, &out);
    // t1 completed on 10-07 (outside); its 3h logged on 10-06 is inside.
    let completed = section(&md, "Completed Tasks");
    assert!(!completed.contains("t1"), "{completed}");
    assert!(!completed.contains("t3"), "{completed}");
    assert!(section(&md, "Time Log").contains("Title of t1"), "{md}");
    assert!(!section(&md, "Time Log").contains("Title of t4"), "{md}");
}

#[test]
fn omitted_period_defaults_to_the_current_iso_week() {
    let (dir, pd) = setup();
    let out = generate(&pd, json!({}));
    let from = out["report"]["scope"]["from"].as_str().unwrap();
    let to = out["report"]["scope"]["to"].as_str().unwrap();
    let from = chrono::NaiveDate::parse_from_str(from, "%Y-%m-%d").unwrap();
    let to = chrono::NaiveDate::parse_from_str(to, "%Y-%m-%d").unwrap();
    assert_eq!((to - from).num_days(), 6);
    let today = chrono::Utc::now().date_naive();
    assert!(from <= today && today <= to, "{from}..{to} vs {today}");
    let _ = body(&dir, &out);
}

#[test]
fn invalid_or_conflicting_period_is_an_error_and_writes_nothing() {
    let (dir, pd) = setup();
    for scope in [
        json!({ "period": "next week" }),
        json!({ "period": "2026-W41", "from": "2026-10-05" }),
        json!({ "from": "2026-10-05" }),
        json!({ "from": "2026-10-12", "to": "2026-10-05" }),
        json!({ "from": "someday", "to": "2026-10-05" }),
    ] {
        let resp = call(
            "handoff_report",
            json!({
                "project_dir": pd, "action": "generate",
                "report_type": "weekly", "scope": scope,
            }),
        );
        assert!(is_error(&resp), "{scope} should fail: {}", text(&resp));
    }
    assert!(
        !handoff(&dir).join("reports").exists()
            || std::fs::read_dir(handoff(&dir).join("reports"))
                .unwrap()
                .next()
                .is_none()
    );
}

#[test]
fn caller_data_is_merged_over_collected_data() {
    let (dir, pd) = setup();
    seed(&dir);
    let resp = call(
        "handoff_report",
        json!({
            "project_dir": pd, "action": "generate", "report_type": "weekly",
            "scope": { "period": "2026-W41" },
            "data": { "summary": "Narrative <ok> & fine", "blockers": [] },
        }),
    );
    assert!(!is_error(&resp), "{}", text(&resp));
    let out: Value = serde_json::from_str(&text(&resp)).unwrap();
    let md = body(&dir, &out);
    assert!(md.contains("Narrative <ok> & fine"), "{md}");
    // The caller's empty `blockers` replaced the collected list.
    assert!(!md.contains("Title of t5"), "{md}");
    // Everything else is still collected.
    assert!(md.contains("Title of t1"), "{md}");
}

#[test]
fn project_without_any_activity_still_renders_every_section() {
    let (dir, pd) = setup();
    let out = generate(&pd, json!({ "period": "2026-W41" }));
    let md = body(&dir, &out);
    for title in [
        "Summary",
        "Completed Tasks",
        "In Progress",
        "Blockers",
        "Time Log",
        "Verification Progress",
        "Milestones",
        "Next Week",
    ] {
        assert!(
            md.contains(&format!("## {title}")),
            "missing {title}:\n{md}"
        );
    }
}

#[test]
fn verification_progress_reflects_layer_statuses_and_pass_rates() {
    let (dir, pd) = setup();
    for args in [
        json!({
            "slug": "requirements-weekly", "title": "Requirements", "layer": "requirement",
            "body": "# Requirements\n\n### REQ-910 Something\n\nBody.\n",
        }),
        json!({
            "slug": "acceptance-weekly", "title": "Acceptance", "layer": "acceptance",
            "body": "# Acceptance\n\n### AT-910 Verify REQ-910\n\n- verifies: REQ-910\n\nSteps.\n",
        }),
    ] {
        let mut args = args;
        args["project_dir"] = json!(pd);
        let resp = call("handoff_doc_save", args);
        assert!(!is_error(&resp), "{}", text(&resp));
    }
    let resp = call(
        "handoff_trace_update",
        json!({"project_dir": pd, "ops": [{"op": "record", "item": "AT-910", "result": "pass"}]}),
    );
    assert!(!is_error(&resp), "{}", text(&resp));

    let out = generate(&pd, json!({ "period": "2026-W41" }));
    let md = body(&dir, &out);
    let verification = section(&md, "Verification Progress");
    assert!(
        verification.contains("| acceptance | verified | 1 / 1 | 100% |"),
        "{verification}"
    );
    assert!(
        verification.contains("| requirement | in_progress |"),
        "{verification}"
    );
    assert!(
        verification.contains("Project status: in_progress"),
        "{verification}"
    );
}

#[test]
fn empty_project_report_shows_zero_hours_never_negative_zero() {
    let (dir, pd) = setup();
    let out = generate(&pd, json!({ "period": "2026-W41" }));
    let md = body(&dir, &out);
    assert!(!md.contains("-0.0"), "{md}");
    assert!(
        section(&md, "Summary").contains("| Hours (actual) | 0.0 | 0.0 |"),
        "{md}"
    );
}

#[test]
fn unreadable_config_fails_the_weekly_report_like_the_verification_report() {
    let (dir, pd) = setup();
    std::fs::write(handoff(&dir).join("config.toml"), "this is = = not toml").unwrap();
    let resp = call(
        "handoff_report",
        json!({
            "project_dir": pd, "action": "generate", "report_type": "weekly",
            "scope": { "period": "2026-W41" },
        }),
    );
    assert!(is_error(&resp), "{}", text(&resp));
    assert!(text(&resp).contains("config"), "{}", text(&resp));
}

#[test]
fn trace_report_failure_is_rendered_in_the_verification_section() {
    let (dir, pd) = setup();
    // A directory where the trace report file is written makes the trace
    // report call fail while everything else stays collectable.
    std::fs::create_dir_all(handoff(&dir).join("docs").join("_trace_report.json")).unwrap();
    let out = generate(&pd, json!({ "period": "2026-W41" }));
    let md = body(&dir, &out);
    let verification = section(&md, "Verification Progress");
    assert!(verification.contains("Not available: "), "{verification}");
}
