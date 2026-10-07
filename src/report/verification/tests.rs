use super::*;
use crate::report::{ReportEngine, ReportMeta, ReportStatus, ReportType};
use crate::storage::test_runs::{
    CampaignProgress, CampaignStatus, ChecklistItem, EvidenceEntry, TestRunScope,
};

fn trace_report() -> Value {
    json!({
        "trace_layers": { "in_use": ["requirements", "system_test", "acceptance_test"] },
        "layer_statuses": {
            "requirements": "verified",
            "system_test": "in_progress",
            "acceptance_test": "in_progress"
        },
        "items": [
            { "id": "ST-2", "layer": "system_test", "title": "Login fails | with bad pw",
              "state": "failing", "waivers": [],
              "tasks": [{ "id": "t9", "role": "fix" }] },
            { "id": "ST-1", "layer": "system_test", "title": "Login works",
              "state": "passing", "waivers": [], "tasks": [] },
            { "id": "AT-1", "layer": "acceptance_test", "title": "Export CSV",
              "state": "not_run",
              "waivers": [{ "axis": "verify", "reason": "manual only in prod" }],
              "tasks": [] },
            { "id": "AT-2", "layer": "acceptance_test", "title": "Print",
              "state": "blocked", "waivers": [], "tasks": [] },
            { "id": "REQ-1", "layer": "requirements", "title": "Users can log in",
              "state": "passing", "waivers": [], "tasks": [] }
        ]
    })
}

fn latest_entry(result: &str, run_id: &str, note: &str, evidence: &[&str]) -> LatestItemResult {
    LatestItemResult {
        result: result.into(),
        executed_at: "2026-10-08T01:02:03.000Z".into(),
        run_id: run_id.into(),
        body_hash: None,
        def_hash: None,
        note: note.into(),
        evidence: evidence.iter().map(|s| s.to_string()).collect(),
        carried_from: None,
    }
}

struct Fixture {
    trace: Value,
    latest: HashMap<String, LatestItemResult>,
    executors: HashMap<String, String>,
}

fn task_status(id: &str) -> String {
    match id {
        "t9" => "in_progress".to_string(),
        _ => "unknown".to_string(),
    }
}

fn fixture() -> Fixture {
    let mut latest = HashMap::new();
    latest.insert(
        "ST-1".into(),
        latest_entry(
            "pass",
            "run-a",
            "ok",
            &["evidence/st1 shot.png", "https://ci/x"],
        ),
    );
    latest.insert(
        "ST-2".into(),
        latest_entry("fail", "run-b", "401 expected, got 500", &[]),
    );
    Fixture {
        trace: trace_report(),
        latest,
        executors: HashMap::from([("run-a".to_string(), "human:alice".to_string())]),
    }
}

fn inputs<'a>(f: &'a Fixture, campaign: Option<&'a TestRunRecord>) -> VerificationInputs<'a> {
    VerificationInputs {
        trace_report: &f.trace,
        latest: &f.latest,
        executors: &f.executors,
        task_status: &task_status,
        campaign,
        project_name: "Demo",
        author: Some("bob"),
        evidence_href_prefix: "../../",
    }
}

fn check(id: &str, result: CheckResult, note: &str) -> ChecklistItem {
    ChecklistItem {
        item_id: id.into(),
        acceptance_text: format!("accept {id}"),
        result,
        evidence: Vec::new(),
        note: note.into(),
        verified_by: Some("carol".into()),
        verified_at: Some("2026-10-07T00:00:00.000Z".into()),
    }
}

fn campaign() -> TestRunRecord {
    let mut with_evidence = check("ST-1", CheckResult::Pass, "fine");
    with_evidence.evidence = vec![EvidenceEntry {
        path: "docs/evidence/login (1).png".into(),
        evidence_type: "screenshot".into(),
        caption: "Login [ok]".into(),
    }];
    let checklist = vec![
        with_evidence,
        check("ST-2", CheckResult::Fail, "broken"),
        check("AT-1", CheckResult::Waived, "approved by PO"),
        check("AT-2", CheckResult::Pending, ""),
    ];
    TestRunRecord {
        test_run_id: "tr-1".into(),
        created_at: "2026-10-01T00:00:00.000Z".into(),
        label: Some("Release 1".into()),
        scope: TestRunScope::default(),
        target_items: checklist.iter().map(|c| c.item_id.clone()).collect(),
        total_target_count: 4,
        progress: crate::storage::test_runs::compute_campaign_progress(&checklist),
        checklist,
        campaign_status: CampaignStatus::Approved,
        completed_at: Some("2026-10-07T12:00:00.000Z".into()),
        approved_by: Some("dave".into()),
        approved_at: Some("2026-10-08T00:00:00.000Z".into()),
    }
}

fn scope_layers(layers: &[&str]) -> ReportScope {
    ReportScope {
        layers: layers.iter().map(|s| s.to_string()).collect(),
        ..ReportScope::default()
    }
}

fn render(scope: ReportScope, data: &Value) -> String {
    let meta = ReportMeta {
        report_id: "verification-1".into(),
        report_type: ReportType::Verification,
        scope,
        version: 3,
        status: ReportStatus::Draft,
        generated_at: "2026-10-08T00:00:00+00:00".into(),
        reviewer: None,
        approved_at: None,
        output_path: "reports/verification-1.md".into(),
        revision_history: Vec::new(),
    };
    ReportEngine::new().unwrap().generate(&meta, data).unwrap()
}

fn ids(data: &Value) -> Vec<&str> {
    data["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["item_id"].as_str().unwrap())
        .collect()
}

#[test]
fn layer_scope_selects_only_those_layers_in_layer_order() {
    let f = fixture();
    let data = build_verification_data(
        &scope_layers(&["acceptance_test", "system_test"]),
        &inputs(&f, None),
    )
    .unwrap();
    assert_eq!(ids(&data), ["ST-1", "ST-2", "AT-1", "AT-2"]);
    assert_eq!(data["summary"]["total"], 4);
    assert_eq!(data["summary"]["pass"], 1);
    assert_eq!(data["summary"]["fail"], 1);
    assert_eq!(data["summary"]["blocked"], 1);
    assert_eq!(data["summary"]["waived"], 1);
    assert_eq!(data["summary"]["pending"], 0);
    assert_eq!(data["summary"]["pass_pct"], 25.0);
    let layers = data["layers"].as_array().unwrap();
    assert_eq!(layers[0]["layer"], "system_test");
    assert_eq!(layers[0]["status"], "in_progress");
    assert_eq!(layers[0]["total"], 2);
    assert_eq!(layers[1]["layer"], "acceptance_test");
}

#[test]
fn run_rows_take_latest_result_executor_and_evidence() {
    let f = fixture();
    let data = build_verification_data(&scope_layers(&["system_test"]), &inputs(&f, None)).unwrap();
    let st1 = &data["results"][0];
    assert_eq!(st1["result"], "pass");
    assert_eq!(st1["verified_by"], "human:alice");
    assert_eq!(st1["executed_at"], "2026-10-08T01:02:03.000Z");
    assert_eq!(st1["evidence"][0]["href"], "../../evidence/st1%20shot.png");
    assert_eq!(st1["evidence"][1]["href"], "https://ci/x");
    assert_eq!(st1["evidence"][1]["type"], "url");
    // A failing item links its task together with the task's status.
    let st2 = &data["results"][1];
    assert_eq!(
        st2["tasks"][0],
        json!({ "id": "t9", "status": "in_progress" })
    );
}

#[test]
fn item_without_run_falls_back_to_waiver_then_graph_state() {
    let f = fixture();
    let data = build_verification_data(&ReportScope::default(), &inputs(&f, None)).unwrap();
    let by_id = |id: &str| {
        data["results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["item_id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(by_id("AT-1")["result"], "waived");
    assert_eq!(by_id("AT-2")["result"], "blocked");
    assert_eq!(by_id("REQ-1")["result"], "pass");
}

#[test]
fn recorded_run_result_outranks_a_waiver_and_graph_state() {
    let mut f = fixture();
    // AT-1 has a verify waiver, but a run has since recorded a real result.
    f.latest.insert(
        "AT-1".into(),
        latest_entry("pass", "run-c", "verified after all", &[]),
    );
    let data = build_verification_data(&ReportScope::default(), &inputs(&f, None)).unwrap();
    let at1 = data["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["item_id"] == "AT-1")
        .unwrap();
    assert_eq!(at1["result"], "pass");
    let waived = data["waived"].as_array().unwrap();
    assert!(
        waived.is_empty(),
        "no waived row once a run decided: {waived:?}"
    );
}

#[test]
fn failures_hold_fail_and_blocked_rows_but_never_waived_ones() {
    let f = fixture();
    let scope = scope_layers(&["system_test", "acceptance_test"]);
    let data = build_verification_data(&scope, &inputs(&f, None)).unwrap();
    let section_ids = |key: &str| -> Vec<String> {
        data[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["item_id"].as_str().unwrap().to_string())
            .collect()
    };
    // ST-2 fails, AT-2 is blocked, AT-1 is waived.
    assert_eq!(section_ids("failures"), ["ST-2", "AT-2"]);
    assert_eq!(section_ids("waived"), ["AT-1"]);

    let md = render(scope, &data);
    let failed = md
        .split("## Failed and Blocked Items")
        .nth(1)
        .and_then(|s| s.split("## Waived Items").next())
        .unwrap();
    let waived = md.split("## Waived Items").nth(1).unwrap();
    assert!(failed.contains("| ST-2 |"), "{failed}");
    assert!(failed.contains("| AT-2 |"), "{failed}");
    assert!(!failed.contains("| AT-1 |"), "{failed}");
    assert!(waived.contains("| AT-1 |"), "{waived}");
    assert!(
        !waived.contains("| AT-2 |") && !waived.contains("| ST-2 |"),
        "{waived}"
    );
}

#[test]
fn campaign_scope_uses_checklist_and_campaign_metadata() {
    let f = fixture();
    let c = campaign();
    let scope = ReportScope {
        campaign: Some("tr-1".into()),
        ..ReportScope::default()
    };
    let data = build_verification_data(&scope, &inputs(&f, Some(&c))).unwrap();
    assert_eq!(ids(&data), ["ST-1", "ST-2", "AT-1", "AT-2"]);
    assert_eq!(data["summary"]["pending"], 1);
    assert_eq!(data["campaign"]["id"], "tr-1");
    assert_eq!(data["campaign"]["status"], "approved");
    assert_eq!(data["campaign"]["approved_by"], "dave");
    let st1 = &data["results"][0];
    assert_eq!(st1["verified_by"], "carol");
    assert_eq!(st1["title"], "Login works");
    // Markdown-significant characters in path and caption cannot break the link.
    assert_eq!(
        st1["evidence"][0]["link"],
        "[Login \\[ok\\]](../../docs/evidence/login%20%281%29.png)"
    );
}

#[test]
fn campaign_scope_without_the_campaign_record_is_an_error() {
    let f = fixture();
    let scope = ReportScope {
        campaign: Some("nope".into()),
        ..ReportScope::default()
    };
    let err = build_verification_data(&scope, &inputs(&f, None)).unwrap_err();
    assert!(err.to_string().contains("nope"), "{err}");
}

#[test]
fn campaign_scope_can_be_narrowed_by_layer_and_status() {
    let f = fixture();
    let c = campaign();
    let scope = ReportScope {
        campaign: Some("tr-1".into()),
        layers: vec!["system_test".into()],
        statuses: vec!["fail".into()],
        ..ReportScope::default()
    };
    let data = build_verification_data(&scope, &inputs(&f, Some(&c))).unwrap();
    assert_eq!(ids(&data), ["ST-2"]);
    assert_eq!(data["summary"]["total"], 1);
}

#[test]
fn status_and_item_filters_narrow_rows_and_summary() {
    let f = fixture();
    let scope = ReportScope {
        statuses: vec!["fail".into(), "blocked".into()],
        ..ReportScope::default()
    };
    let data = build_verification_data(&scope, &inputs(&f, None)).unwrap();
    assert_eq!(ids(&data), ["ST-2", "AT-2"]);
    assert_eq!(data["summary"]["total"], 2);

    let scope = ReportScope {
        items: vec!["ST-1".into(), "GHOST".into()],
        ..ReportScope::default()
    };
    let data = build_verification_data(&scope, &inputs(&f, None)).unwrap();
    assert_eq!(ids(&data), ["ST-1"]);
    assert!(data["warnings"][0].as_str().unwrap().contains("GHOST"));
}

#[test]
fn unknown_status_filter_is_rejected() {
    let f = fixture();
    let scope = ReportScope {
        statuses: vec!["passed".into()],
        ..ReportScope::default()
    };
    let err = build_verification_data(&scope, &inputs(&f, None)).unwrap_err();
    assert!(err.to_string().contains("passed"), "{err}");
}

#[test]
fn unknown_layer_in_scope_is_a_warning_with_an_empty_report() {
    let f = fixture();
    let data = build_verification_data(&scope_layers(&["nope"]), &inputs(&f, None)).unwrap();
    assert_eq!(data["summary"]["total"], 0);
    assert_eq!(data["summary"]["pass_pct"], 0.0);
    assert!(data["warnings"][0].as_str().unwrap().contains("nope"));
}

#[test]
fn rendered_report_for_a_layer_scope_has_every_section() {
    let f = fixture();
    let scope = scope_layers(&["system_test", "acceptance_test"]);
    let data = build_verification_data(&scope, &inputs(&f, None)).unwrap();
    let md = render(scope, &data);
    for heading in [
        "# Verification Report",
        "## Summary",
        "## Coverage by Layer",
        "## Verification Results",
        "## Failed and Blocked Items",
        "## Waived Items",
        "## Approval",
    ] {
        assert!(md.contains(heading), "missing {heading}:\n{md}");
    }
    assert!(md.contains("Demo"), "{md}");
    assert!(md.contains("| Pass | 1 | 25.0% |"), "{md}");
    assert!(md.contains("| system_test | in_progress | 2 |"), "{md}");
    // A pipe in a title cannot split the table row.
    assert!(md.contains("Login fails \\| with bad pw"), "{md}");
    // Failure row carries the follow-up task and its status.
    assert!(md.contains("t9 (in_progress)"), "{md}");
    // Waived row shows the waiver reason.
    assert!(md.contains("manual only in prod"), "{md}");
}

#[test]
fn rendered_report_contains_evidence_links_and_campaign_approval() {
    let f = fixture();
    let c = campaign();
    let scope = ReportScope {
        campaign: Some("tr-1".into()),
        ..ReportScope::default()
    };
    let data = build_verification_data(&scope, &inputs(&f, Some(&c))).unwrap();
    let md = render(scope, &data);
    assert!(
        md.contains("[Login \\[ok\\]](../../docs/evidence/login%20%281%29.png)"),
        "{md}"
    );
    assert!(md.contains("Release 1"), "{md}");
    assert!(md.contains("dave"), "{md}");
    // Waived row from the campaign: reason from the item waiver, approver from the campaign.
    assert!(
        md.contains("| AT-1 | Export CSV | manual only in prod | dave |"),
        "{md}"
    );
    // Failed row shows the verifier's note.
    assert!(md.contains("broken"), "{md}");
}

#[test]
fn rendered_empty_report_says_none_instead_of_empty_tables() {
    let f = fixture();
    let scope = scope_layers(&["requirements"]);
    let data = build_verification_data(&scope, &inputs(&f, None)).unwrap();
    let md = render(scope, &data);
    assert!(md.contains("No failed or blocked items."), "{md}");
    assert!(md.contains("No waived items."), "{md}");
}

#[test]
fn evidence_href_leaves_absolute_locations_alone() {
    let l = evidence_link("../../", "/abs/x.png", "file", "");
    assert_eq!(l.href, "/abs/x.png");
    let l = evidence_link("../../", "#anchor", "file", "");
    assert_eq!(l.href, "#anchor");
}

#[test]
fn compute_progress_import_is_used() {
    // Guards the fixture against drifting from the real progress aggregation.
    let c = campaign();
    assert_eq!(
        c.progress,
        CampaignProgress {
            total: 4,
            checked: 3,
            pass: 1,
            fail: 1,
            blocked: 0,
            waived: 1,
            pending: 1
        }
    );
}

#[test]
fn cell_helper_escapes_pipes_and_line_breaks() {
    let mut engine = ReportEngine::new().unwrap();
    engine
        .registry
        .register_template_string(
            "verification",
            "{{cell data.x}}|{{cell data.missing}}|{{cell data.n}}",
        )
        .unwrap();
    let meta = ReportMeta {
        report_id: "r".into(),
        report_type: ReportType::Verification,
        scope: ReportScope::default(),
        version: 1,
        status: ReportStatus::Draft,
        generated_at: String::new(),
        reviewer: None,
        approved_at: None,
        output_path: String::new(),
        revision_history: Vec::new(),
    };
    let out = engine
        .generate(&meta, &json!({ "x": "a|b\nc\r\nd", "n": 5 }))
        .unwrap();
    assert_eq!(out, "a\\|b<br>c<br>d||5");
}
