//! REQ-PDF-031: `handoff_report generate` inserts Mermaid charts (burndown +
//! velocity for weekly/monthly, coverage pie for verification, gantt for
//! milestone, cumulative hours for effort) as ```mermaid blocks, controlled
//! by `include_charts`.

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

fn ok(name: &str, arguments: Value) -> Value {
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
    ok(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "charts" }),
    );
    (dir, pd)
}

fn handoff(dir: &TempDir) -> PathBuf {
    dir.path().join(".handoff")
}

fn put_task(handoff: &Path, status: &str, id: &str, f: impl FnOnce(&mut TaskData)) {
    let mut data = TaskData {
        id: id.into(),
        title: format!("Title of {id}"),
        notes: None,
        priority: None,
        created_at: Some("2026-10-01T09:00:00+00:00".into()),
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
    let dir = handoff.join("tasks").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    write_task(&dir, status, &data).unwrap();
}

fn sched(d: &mut TaskData, f: impl FnOnce(&mut Schedule)) {
    f(d.schedule.get_or_insert_with(Default::default));
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

/// Generates a report and returns (response, Markdown body).
fn generate(
    dir: &TempDir,
    pd: &str,
    report_type: &str,
    scope: Value,
    extra: Value,
) -> (Value, String) {
    let mut args = json!({ "project_dir": pd, "action": "generate",
                           "report_type": report_type, "scope": scope });
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    let out = ok("handoff_report", args);
    let path = out["report"]["output_path"].as_str().unwrap();
    let body = std::fs::read_to_string(handoff(dir).join(path)).unwrap();
    (out, body)
}

/// Contents of every ```mermaid fence in `md`.
fn mermaid_blocks(md: &str) -> Vec<String> {
    md.split("```mermaid\n")
        .skip(1)
        .map(|rest| rest.split("```").next().unwrap().to_string())
        .collect()
}

fn seed_progress(dir: &TempDir) {
    let h = handoff(dir);
    put_task(&h, "done", "t1", |d| {
        d.completed_at = Some("2026-10-06T09:00:00+00:00".into());
    });
    put_task(&h, "done", "t2", |d| {
        d.completed_at = Some("2026-10-08T09:00:00+00:00".into());
    });
    put_task(&h, "in_progress", "t3", |_| {});
}

#[test]
fn weekly_report_has_burndown_and_velocity_charts() {
    let (dir, pd) = setup();
    seed_progress(&dir);
    let (_, md) = generate(
        &dir,
        &pd,
        "weekly",
        json!({ "period": "2026-W41" }),
        json!({}),
    );
    let blocks = mermaid_blocks(&md);
    assert_eq!(blocks.len(), 2, "{md}");
    assert!(blocks[0].starts_with("xychart-beta\n"), "{md}");
    assert!(blocks[0].contains("line [3, 2, 2, 1, 1, 1, 1]"), "{md}");
    assert!(blocks[1].contains("bar [0, 0, 0, 2]"), "{md}");
    assert!(md.contains("## Charts"), "{md}");
}

#[test]
fn monthly_report_has_burndown_and_velocity_charts() {
    let (dir, pd) = setup();
    seed_progress(&dir);
    let (_, md) = generate(
        &dir,
        &pd,
        "monthly",
        json!({ "period": "2026-10" }),
        json!({}),
    );
    let blocks = mermaid_blocks(&md);
    assert_eq!(blocks.len(), 2, "{md}");
    assert!(blocks[0].contains("title \"Burndown"), "{md}");
    assert!(blocks[1].contains("\"2026-W40\", \"2026-W41\""), "{md}");
}

#[test]
fn include_charts_false_leaves_the_report_without_charts() {
    let (dir, pd) = setup();
    seed_progress(&dir);
    for (report_type, scope) in [
        ("weekly", json!({ "period": "2026-W41" })),
        ("monthly", json!({ "period": "2026-10" })),
    ] {
        let (_, md) = generate(
            &dir,
            &pd,
            report_type,
            scope,
            json!({ "include_charts": false }),
        );
        assert!(!md.contains("```mermaid"), "{md}");
        assert!(!md.contains("## Charts"), "{md}");
    }
}

#[test]
fn include_charts_must_be_a_boolean_and_only_applies_to_generate() {
    let (_dir, pd) = setup();
    let resp = call(
        "handoff_report",
        json!({ "project_dir": pd, "action": "generate", "report_type": "weekly",
                "include_charts": "yes" }),
    );
    assert!(is_error(&resp));
    assert!(text(&resp).contains("include_charts"), "{}", text(&resp));
    let resp = call(
        "handoff_report",
        json!({ "project_dir": pd, "action": "list", "include_charts": true }),
    );
    assert!(is_error(&resp));
    assert!(text(&resp).contains("include_charts"), "{}", text(&resp));
}

#[test]
fn effort_report_has_a_cumulative_hours_chart() {
    let (dir, pd) = setup();
    let h = handoff(&dir);
    put_task(&h, "done", "t1", |_| {});
    log(&h, "2026-10-06T10:00:00+00:00", "t1", 3.0);
    log(&h, "2026-10-08T10:00:00+00:00", "t1", 1.5);
    let (_, md) = generate(&dir, &pd, "effort", json!({}), json!({}));
    let blocks = mermaid_blocks(&md);
    assert_eq!(blocks.len(), 1, "{md}");
    assert!(
        blocks[0].contains("x-axis [\"2026-10-06\", \"2026-10-08\"]"),
        "{md}"
    );
    assert!(blocks[0].contains("line [3, 4.5]"), "{md}");

    let (_, md) = generate(
        &dir,
        &pd,
        "effort",
        json!({}),
        json!({ "include_charts": false }),
    );
    assert!(!md.contains("```mermaid"), "{md}");
}

#[test]
fn effort_report_without_time_logged_has_no_chart() {
    let (dir, pd) = setup();
    let (_, md) = generate(&dir, &pd, "effort", json!({}), json!({}));
    assert!(!md.contains("```mermaid"), "{md}");
    assert!(!md.contains("## Charts"), "{md}");
}

#[test]
fn milestone_report_has_a_gantt_chart() {
    let (dir, pd) = setup();
    let h = handoff(&dir);
    ok(
        "handoff_add_milestone",
        json!({ "project_dir": pd, "name": "beta", "date": "2026-10-31" }),
    );
    put_task(&h, "done", "t1", |d| {
        d.title = "Design: API".into();
        d.completed_at = Some("2026-10-05T09:00:00+00:00".into());
        sched(d, |s| {
            s.milestone = Some("beta".into());
            s.start_date = Some("2026-10-01".into());
            s.due_date = Some("2026-10-05".into());
        });
    });
    put_task(&h, "todo", "t2", |d| {
        sched(d, |s| {
            s.milestone = Some("beta".into());
            s.due_date = Some("2026-10-20".into());
        });
    });
    let (_, md) = generate(
        &dir,
        &pd,
        "milestone",
        json!({ "milestone": "beta" }),
        json!({}),
    );
    let blocks = mermaid_blocks(&md);
    assert_eq!(blocks.len(), 1, "{md}");
    let gantt = &blocks[0];
    assert!(gantt.starts_with("gantt\n"), "{md}");
    assert!(
        gantt.contains("t1 Design API :done, t1, 2026-10-01, 2026-10-05"),
        "{md}"
    );
    assert!(gantt.contains("t2 Title of t2 :t2, 2026-10-20, 1d"), "{md}");
    assert!(
        gantt.contains("beta :milestone, m1, 2026-10-31, 0d"),
        "{md}"
    );

    let (_, md) = generate(
        &dir,
        &pd,
        "milestone",
        json!({ "milestone": "beta" }),
        json!({ "include_charts": false }),
    );
    assert!(!md.contains("```mermaid"), "{md}");
}

#[test]
fn verification_report_has_a_coverage_pie() {
    let (dir, pd) = setup();
    ok(
        "handoff_doc_save",
        json!({
            "project_dir": pd, "slug": "requirements", "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-910 Export\n\nBody.\n",
        }),
    );
    ok(
        "handoff_doc_save",
        json!({
            "project_dir": pd, "slug": "acceptance", "title": "Acceptance",
            "layer": "acceptance",
            "body": "# Acceptance\n\n### AT-910 CSV\n\n- verifies: REQ-910\n\nSteps.\n\n### AT-911 JSON\n\n- verifies: REQ-910\n\nSteps.\n",
        }),
    );
    ok(
        "handoff_trace_update",
        json!({ "project_dir": pd, "executor_kind": "human", "executor_id": "alice",
                "ops": [{ "op": "record", "item": "AT-911", "result": "pass" }] }),
    );
    let scope = json!({ "layers": ["acceptance"] });
    let (_, md) = generate(&dir, &pd, "verification", scope.clone(), json!({}));
    let blocks = mermaid_blocks(&md);
    assert_eq!(blocks.len(), 1, "{md}");
    assert_eq!(
        blocks[0],
        "pie showData title Verification coverage\n    \"Pass\" : 1\n    \"Pending\" : 1\n"
    );

    let (_, md) = generate(
        &dir,
        &pd,
        "verification",
        scope,
        json!({ "include_charts": false }),
    );
    assert!(!md.contains("```mermaid"), "{md}");
}

#[test]
fn caller_supplied_charts_in_data_win() {
    let (dir, pd) = setup();
    seed_progress(&dir);
    let (_, md) = generate(
        &dir,
        &pd,
        "weekly",
        json!({ "period": "2026-W41" }),
        json!({ "data": { "charts": { "burndown": "pie title Custom\n    \"A\" : 1" } } }),
    );
    let blocks = mermaid_blocks(&md);
    assert_eq!(
        blocks,
        vec!["pie title Custom\n    \"A\" : 1\n".to_string()],
        "{md}"
    );
}
