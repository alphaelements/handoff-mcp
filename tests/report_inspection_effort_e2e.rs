//! FR-523 / SPEC-523 and FR-524 / SPEC-524: `handoff_report generate`
//! with `report_type=inspection` (R2: certificate of an approved
//! verification campaign) and `report_type=effort` (R5: hours from
//! `time_log.jsonl` by period / assignee).

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

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"].as_bool().unwrap_or(false)
}

fn text(resp: &Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .expect("text content")
        .to_string()
}

fn ok(name: &str, arguments: Value) -> Value {
    let resp = call(name, arguments);
    assert!(!is_error(&resp), "{name} failed: {}", text(&resp));
    serde_json::from_str(&text(&resp)).unwrap_or(Value::Null)
}

fn init(name: &str) -> (TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let pd = dir.path().to_string_lossy().to_string();
    ok(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": name }),
    );
    (dir, pd)
}

fn generate_raw(pd: &str, report_type: &str, scope: Value) -> Value {
    call(
        "handoff_report",
        json!({ "project_dir": pd, "action": "generate",
                "report_type": report_type, "scope": scope }),
    )
}

/// Generates a report and returns `(metadata, markdown body)`.
fn generate(pd: &str, report_type: &str, scope: Value) -> (Value, String) {
    let resp = generate_raw(pd, report_type, scope);
    assert!(!is_error(&resp), "generate failed: {}", text(&resp));
    let out: Value = serde_json::from_str(&text(&resp)).unwrap();
    let id = out["report"]["report_id"].as_str().unwrap().to_string();
    let got = ok(
        "handoff_report",
        json!({ "project_dir": pd, "action": "get", "report_id": id }),
    );
    (
        out["report"].clone(),
        got["body"].as_str().unwrap().to_string(),
    )
}

fn listed_reports(pd: &str) -> usize {
    ok(
        "handoff_report",
        json!({ "project_dir": pd, "action": "list" }),
    )["reports"]
        .as_array()
        .unwrap()
        .len()
}

// ---- inspection ----------------------------------------------------------

/// Acceptance items AT-910 (pass), AT-911 (fail), AT-912 (waived), checked in
/// a campaign that stops at `final_status`.
fn campaign_project(final_status: &str) -> (TempDir, String, String) {
    let (dir, pd) = init("Inspect Demo");
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
            "body": "# Acceptance\n\n\
                ### AT-910 Export CSV\n\n- verifies: REQ-910\n\nSteps.\n\n\
                ### AT-911 Export JSON\n\n- verifies: REQ-910\n\nSteps.\n\n\
                ### AT-912 Export PDF\n\n- verifies: REQ-910\n- waive-verify: checked by eye\n\nSteps.\n",
        }),
    );
    let created = ok(
        "handoff_trace_test_run",
        json!({ "project_dir": pd, "action": "create", "label": "Release 1",
                "scope": { "layers": ["acceptance"] } }),
    );
    let id = created["test_run_id"].as_str().unwrap().to_string();
    let check = |item: &str, result: &str, note: &str, by: &str| {
        ok(
            "handoff_trace_test_run",
            json!({ "project_dir": pd, "action": "record_check", "test_run_id": id,
                    "item_id": item, "result": result, "note": note,
                    "verified_by": by,
                    "evidence": [{ "path": format!("evidence/{item}.png"), "type": "screenshot", "caption": "shot" }] }),
        );
    };
    check("AT-910", "pass", "fine", "carol");
    check("AT-911", "fail", "timeout", "erin");
    check("AT-912", "waived", "approved by PO", "carol");
    let status = |status: &str, extra: Value| {
        let mut args = json!({ "project_dir": pd, "action": "set_status",
                               "test_run_id": id, "status": status });
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        ok("handoff_trace_test_run", args);
    };
    if final_status != "in_progress" {
        status("completed", json!({}));
    }
    if final_status == "approved" {
        status("approved", json!({ "approved_by": "dave" }));
    }
    (dir, pd, id)
}

#[test]
fn inspection_report_certifies_an_approved_campaign() {
    let (_dir, pd, id) = campaign_project("approved");
    let (meta, md) = generate(&pd, "inspection", json!({ "campaign": id }));

    assert_eq!(meta["report_type"], "inspection");
    assert_eq!(meta["status"], "draft");
    assert_eq!(meta["version"], 1);
    assert!(
        meta["report_id"]
            .as_str()
            .unwrap()
            .starts_with("inspection-"),
        "{meta}"
    );
    assert!(md.starts_with("# Inspection Certificate"), "{md}");
    // Document control.
    assert!(
        md.contains(&format!("| Document No. | INSP-{id} |")),
        "{md}"
    );
    assert!(md.contains("| Version | 1 |"), "{md}");
    assert!(md.contains("| Project | Inspect Demo |"), "{md}");
    assert!(md.contains("| Inspectors | carol, erin |"), "{md}");
    assert!(md.contains("| Approver | dave |"), "{md}");
    assert!(md.contains("| Overall judgement | FAIL |"), "{md}");
    // All items judged.
    for item in ["AT-910", "AT-911", "AT-912"] {
        assert!(md.contains(&format!("| {item} |")), "{item}: {md}");
    }
    assert!(md.contains("| Pass | 1 | 33.3% |"), "{md}");
    assert!(md.contains("[shot](../../evidence/AT-910.png)"), "{md}");
    // Non-conforming item and its finding; waiver reason.
    let non_conforming = md.split("## Non-conforming Items").nth(1).unwrap();
    let non_conforming = non_conforming.split("\n## ").next().unwrap();
    assert!(
        non_conforming.contains("| AT-911 | Export JSON | fail | timeout |"),
        "{md}"
    );
    assert!(!non_conforming.contains("AT-910"), "{md}");
    assert!(
        md.contains("| AT-912 | Export PDF | checked by eye | dave |"),
        "{md}"
    );
    // Revision history and signatures.
    assert!(md.contains("## Revision History"), "{md}");
    assert!(md.contains("| - | draft |"), "{md}");
    assert!(md.contains("## Signatures"), "{md}");
    assert!(md.contains("| Inspector | carol, erin | | |"), "{md}");

    // Regenerating the same scope bumps the version.
    let (again, md2) = generate(&pd, "inspection", json!({ "campaign": id }));
    assert_eq!(again["version"], 2);
    assert!(md2.contains("| Version | 2 |"), "{md2}");
}

#[test]
fn inspection_report_requires_an_approved_campaign() {
    for final_status in ["in_progress", "completed"] {
        let (_dir, pd, id) = campaign_project(final_status);
        let resp = generate_raw(&pd, "inspection", json!({ "campaign": id }));
        assert!(is_error(&resp), "{final_status}: {}", text(&resp));
        let message = text(&resp);
        assert!(message.contains("not approved"), "{message}");
        assert!(message.contains(final_status), "{message}");
        assert_eq!(listed_reports(&pd), 0);
    }
}

#[test]
fn inspection_report_rejects_a_missing_unknown_or_narrowing_scope() {
    let (_dir, pd, id) = campaign_project("approved");
    for (scope, needle) in [
        (json!({}), "scope.campaign is required"),
        (json!({ "campaign": "nope" }), "not found"),
        (
            json!({ "campaign": id, "layers": ["acceptance"] }),
            "not supported for an inspection report",
        ),
        (
            json!({ "campaign": id, "statuses": ["fail"] }),
            "not supported for an inspection report",
        ),
    ] {
        let resp = generate_raw(&pd, "inspection", scope.clone());
        assert!(is_error(&resp), "{scope}: {}", text(&resp));
        assert!(text(&resp).contains(needle), "{scope}: {}", text(&resp));
    }
    assert_eq!(listed_reports(&pd), 0);
}

#[test]
fn inspection_report_goes_through_the_approval_workflow() {
    let (_dir, pd, id) = campaign_project("approved");
    let (meta, _) = generate(&pd, "inspection", json!({ "campaign": id }));
    let report_id = meta["report_id"].as_str().unwrap();
    ok(
        "handoff_report",
        json!({ "project_dir": pd, "action": "submit", "report_id": report_id }),
    );
    let approved = ok(
        "handoff_report",
        json!({ "project_dir": pd, "action": "approve", "report_id": report_id,
                "reviewer": "frank" }),
    );
    assert_eq!(approved["report"]["status"], "approved");
    assert_eq!(approved["report"]["reviewer"], "frank");
}

// ---- effort --------------------------------------------------------------

fn handoff(dir: &TempDir) -> PathBuf {
    dir.path().join(".handoff")
}

fn put_task(
    handoff: &Path,
    status: &str,
    id: &str,
    title: &str,
    assignee: Option<&str>,
    estimate: Option<f64>,
    actual: Option<f64>,
) {
    let data = TaskData {
        id: id.into(),
        title: title.into(),
        notes: None,
        priority: None,
        created_at: None,
        updated_at: None,
        completed_at: None,
        labels: vec![],
        links: vec![],
        task_links: vec![],
        done_criteria: vec![],
        schedule: Some(Schedule {
            estimate_hours: estimate,
            actual_hours: actual,
            ..Default::default()
        }),
        dependencies: vec![],
        order: None,
        assignee: assignee.map(str::to_string),
        lock: None,
        scope_paths: vec![],
        extra: HashMap::new(),
    };
    let dir = handoff.join("tasks").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    write_task(&dir, status, &data).unwrap();
}

fn log(handoff: &Path, ts: &str, task_id: &str, hours: f64, agent: Option<&str>) {
    append_time_log(
        handoff,
        &TimeLogEntry {
            ts: ts.into(),
            task_id: task_id.into(),
            hours,
            agent_id: agent.map(str::to_string),
            note: None,
        },
    )
    .unwrap();
}

/// 2026-W41 = Mon 2026-10-05 .. Sun 2026-10-11.
fn effort_project() -> (TempDir, String) {
    let (dir, pd) = init("Effort Demo");
    let h = handoff(&dir);
    put_task(
        &h,
        "done",
        "t1",
        "Build parser",
        Some("alice"),
        Some(4.0),
        Some(5.0),
    );
    put_task(
        &h,
        "in_progress",
        "t2",
        "Write docs",
        None,
        Some(2.0),
        Some(1.0),
    );
    put_task(&h, "todo", "t3", "No estimate", None, None, Some(1.0));
    log(&h, "2026-10-06T10:00:00+00:00", "t1", 3.0, Some("alice"));
    log(&h, "2026-10-08T10:00:00+00:00", "t1", 1.5, Some("bob"));
    log(&h, "2026-10-09T10:00:00+00:00", "t2", 1.0, Some("bob"));
    log(&h, "2026-10-10T10:00:00+00:00", "t3", 1.0, None);
    // Outside the week.
    log(&h, "2026-10-02T10:00:00+00:00", "t1", 0.5, Some("alice"));
    log(&h, "2026-10-13T10:00:00+00:00", "t2", 0.25, Some("bob"));
    (dir, pd)
}

#[test]
fn effort_report_aggregates_the_time_log_for_a_period() {
    let (_dir, pd) = effort_project();
    let (meta, md) = generate(&pd, "effort", json!({ "period": "2026-W41" }));

    assert_eq!(meta["report_type"], "effort");
    assert_eq!(meta["scope"]["from"], "2026-10-05");
    assert_eq!(meta["scope"]["to"], "2026-10-11");
    assert!(md.starts_with("# Effort Report"), "{md}");
    assert!(md.contains("| Period | 2026-10-05 to 2026-10-11 |"), "{md}");
    assert!(md.contains("| Assignee | all |"), "{md}");
    // 3.0 + 1.5 + 1.0 + 1.0 inside the week.
    assert!(md.contains("| Total hours | 6.5 |"), "{md}");
    assert!(md.contains("| Time log entries | 4 |"), "{md}");
    assert!(md.contains("| 2026-W41 | 6.5 | 4 |"), "{md}");
    assert!(md.contains("| 2026-10-06 | 3.0 | 1 |"), "{md}");
    // By task: t1 logged by alice and bob.
    assert!(
        md.contains("| t1 | Build parser | done | alice, bob | 4.5 | 2 |"),
        "{md}"
    );
    assert!(
        md.contains("| t2 | Write docs | in_progress | bob | 1.0 | 1 |"),
        "{md}"
    );
    // By assignee: bob 2.5h (2 tasks), alice 3.0h, unassigned 1.0h.
    assert!(md.contains("| alice | 3.0 | 1 | 1 | 46.2% |"), "{md}");
    assert!(md.contains("| bob | 2.5 | 2 | 2 | 38.5% |"), "{md}");
    assert!(
        md.contains("| (unassigned) | 1.0 | 1 | 1 | 15.4% |"),
        "{md}"
    );
    // Deviation: t1 +1.0 (25%), t2 -1.0 (-50%); t3 has no estimate.
    assert!(
        md.contains("| t2 | Write docs | 2.0 | 1.0 | -1.0 | -50% |"),
        "{md}"
    );
    assert!(
        md.contains("| t1 | Build parser | 4.0 | 5.0 | +1.0 | 25% |"),
        "{md}"
    );
    assert!(
        md.contains("1 task(s) without an estimate or actual hours"),
        "{md}"
    );
}

#[test]
fn effort_report_filters_by_assignee() {
    let (_dir, pd) = effort_project();
    let (meta, md) = generate(
        &pd,
        "effort",
        json!({ "from": "2026-10-05", "to": "2026-10-11", "assignee": "bob" }),
    );
    assert_eq!(meta["scope"]["assignee"], "bob");
    assert!(md.contains("| Assignee | bob |"), "{md}");
    assert!(md.contains("| Total hours | 2.5 |"), "{md}");
    assert!(md.contains("| bob | 2.5 | 2 | 2 | 100% |"), "{md}");
    assert!(!md.contains("| alice |"), "{md}");
    assert!(!md.contains("| t3 |"), "{md}");
}

#[test]
fn effort_report_without_a_period_covers_the_whole_log() {
    let (_dir, pd) = effort_project();
    let (meta, md) = generate(&pd, "effort", json!({}));
    assert!(meta["scope"].get("from").is_none(), "{meta}");
    assert!(md.contains("| Period | all time |"), "{md}");
    // 6.5 in the week + 0.5 + 0.25 outside it.
    assert!(md.contains("| Total hours | 7.25 |"), "{md}");
}

#[test]
fn effort_report_of_an_empty_project_renders_placeholders() {
    let (_dir, pd) = init("Empty");
    let (_, md) = generate(&pd, "effort", json!({ "period": "2026-W41" }));
    assert!(md.contains("| Total hours | 0.0 |"), "{md}");
    assert!(md.contains("No time logged in scope."), "{md}");
    assert!(
        md.contains("No tasks with both an estimate and logged time."),
        "{md}"
    );
}

#[test]
fn effort_report_rejects_bad_scopes_and_writes_nothing() {
    let (_dir, pd) = effort_project();
    for (scope, needle) in [
        (json!({ "period": "garbage" }), "Invalid period"),
        (json!({ "from": "2026-10-05" }), "together"),
        (
            json!({ "period": "2026-W41", "from": "2026-10-05" }),
            "cannot be combined",
        ),
        (
            json!({ "campaign": "tr-1" }),
            "not supported for an effort report",
        ),
        (
            json!({ "layers": ["acceptance"] }),
            "not supported for an effort report",
        ),
    ] {
        let resp = generate_raw(&pd, "effort", scope.clone());
        assert!(is_error(&resp), "{scope}: {}", text(&resp));
        assert!(text(&resp).contains(needle), "{scope}: {}", text(&resp));
    }
    assert_eq!(listed_reports(&pd), 0);
}

#[test]
fn caller_data_is_merged_over_collected_effort_data() {
    let (_dir, pd) = effort_project();
    let resp = call(
        "handoff_report",
        json!({ "project_dir": pd, "action": "generate", "report_type": "effort",
                "scope": { "period": "2026-W41" },
                "data": { "assignee": "override" } }),
    );
    assert!(!is_error(&resp), "{}", text(&resp));
    let out: Value = serde_json::from_str(&text(&resp)).unwrap();
    let got = ok(
        "handoff_report",
        json!({ "project_dir": pd, "action": "get",
                "report_id": out["report"]["report_id"] }),
    );
    assert!(
        got["body"]
            .as_str()
            .unwrap()
            .contains("| Assignee | override |"),
        "{got}"
    );
}
