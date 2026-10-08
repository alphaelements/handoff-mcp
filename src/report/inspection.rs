//! Data collection for the inspection certificate (R2: FR-523 / SPEC-523).
//!
//! [`build_inspection_data`] is pure: the caller (`handoff_report generate`)
//! loads the trace report and the approved campaign, and this module turns
//! the campaign checklist into the `data` object `inspection.md.hbs`
//! renders. Unlike the verification report, an inspection report covers the
//! *whole* checklist of one approved campaign, so no row filter is accepted.

use std::collections::BTreeSet;

use anyhow::{bail, Result};
use serde_json::{json, Value};

use super::verification::{campaign_rows, index_items, summary_json, Row};
use super::ReportScope;
use crate::storage::test_runs::{CampaignStatus, CheckResult, TestRunRecord};

/// Prefix of the document number (`INSP-<campaign id>`).
const DOCUMENT_NO_PREFIX: &str = "INSP-";

/// Overall judgement values; see [`verdict`].
const VERDICT_PASS: &str = "pass";
const VERDICT_FAIL: &str = "fail";
const VERDICT_INCOMPLETE: &str = "incomplete";

/// Everything [`build_inspection_data`] needs, already loaded by the caller.
pub struct InspectionInputs<'a> {
    /// `handoff_trace_report` output including `items[]` (needs `id`,
    /// `layer`, `title`, `waivers`, `tasks`); supplies titles, waiver
    /// reasons and the follow-up tasks of failed items.
    pub trace_report: &'a Value,
    /// Status of a task by id; only called for tasks linked to failed /
    /// blocked rows.
    pub task_status: &'a dyn Fn(&str) -> String,
    /// The approved campaign named by `scope.campaign`.
    pub campaign: &'a TestRunRecord,
    pub project_name: &'a str,
    /// Who generated the report.
    pub author: Option<&'a str>,
    /// Prefix that turns a project-relative evidence path into a link valid
    /// from the report file's own directory (e.g. `../../`).
    pub evidence_href_prefix: &'a str,
}

/// An inspection certificate is only issued for an approved campaign, and
/// covers its whole checklist: any scope field other than `label` and
/// `campaign` would silently narrow or shift what is certified, so it is an
/// error instead.
fn validate_scope(scope: &ReportScope) -> Result<()> {
    let unsupported: Vec<&str> = [
        ("layers", !scope.layers.is_empty()),
        ("items", !scope.items.is_empty()),
        ("statuses", !scope.statuses.is_empty()),
        ("period", scope.period.is_some()),
        ("from", scope.from.is_some()),
        ("to", scope.to.is_some()),
        ("assignee", scope.assignee.is_some()),
        ("milestone", scope.milestone.is_some()),
    ]
    .into_iter()
    .filter_map(|(name, set)| set.then_some(name))
    .collect();
    if !unsupported.is_empty() {
        bail!(
            "scope.{} not supported for an inspection report (it covers the whole campaign; only scope.campaign and scope.label apply)",
            unsupported.join(", scope.")
        );
    }
    Ok(())
}

/// `pass` when every item passed or was waived, `fail` when any item failed
/// or is blocked, `incomplete` when nothing failed but items are pending.
fn verdict(rows: &[&Row]) -> &'static str {
    let any = |result: CheckResult| rows.iter().any(|r| r.result == result.as_str());
    if any(CheckResult::Fail) || any(CheckResult::Blocked) {
        VERDICT_FAIL
    } else if any(CheckResult::Pending) {
        VERDICT_INCOMPLETE
    } else {
        VERDICT_PASS
    }
}

/// Builds the `data` object for `inspection.md.hbs`.
///
/// ```text
/// document_no, project_name, author, verdict (pass|fail|incomplete), verdict_label (upper case)
/// campaign    { id, label, status, completed_at, approved_by, approved_at }
/// inspectors  [ verifier ]                   distinct, sorted
/// summary     { total, pass, fail, ..., <result>_pct }
/// results     [ row ]                        one per checklist item
/// failures    [ row ]                        fail / blocked, with follow-up tasks
/// waived      [ { item_id, title, layer, reason, approver } ]
/// ```
pub fn build_inspection_data(scope: &ReportScope, inputs: &InspectionInputs) -> Result<Value> {
    validate_scope(scope)?;
    let campaign = inputs.campaign;
    if campaign.campaign_status != CampaignStatus::Approved {
        bail!(
            "Campaign '{}' is not approved (status: {}); an inspection report requires an approved campaign",
            campaign.test_run_id,
            campaign.campaign_status.as_str()
        );
    }

    let items = index_items(inputs.trace_report);
    let rows = campaign_rows(
        campaign,
        &items,
        inputs.task_status,
        inputs.evidence_href_prefix,
    );
    let all: Vec<&Row> = rows.iter().collect();
    let failures: Vec<&Row> = all
        .iter()
        .copied()
        .filter(|r| {
            r.result == CheckResult::Fail.as_str() || r.result == CheckResult::Blocked.as_str()
        })
        .collect();

    let verdict = verdict(&all);
    let inspectors: BTreeSet<&str> = campaign
        .checklist
        .iter()
        .filter_map(|c| c.verified_by.as_deref())
        .filter(|name| !name.trim().is_empty())
        .collect();

    let waived: Vec<Value> = all
        .iter()
        .filter(|r| r.result == CheckResult::Waived.as_str())
        .map(|r| {
            let reason = r
                .waiver_reason
                .clone()
                .filter(|s| !s.is_empty())
                .or_else(|| Some(r.note.clone()).filter(|s| !s.is_empty()))
                .unwrap_or_default();
            json!({
                "item_id": r.item_id,
                "title": r.title,
                "layer": r.layer,
                "reason": reason,
                "approver": campaign.approved_by,
            })
        })
        .collect();

    Ok(json!({
        "document_no": format!("{DOCUMENT_NO_PREFIX}{}", campaign.test_run_id),
        "project_name": inputs.project_name,
        "author": inputs.author,
        "verdict": verdict,
        "verdict_label": verdict.to_uppercase(),
        "campaign": {
            "id": campaign.test_run_id,
            "label": campaign.label,
            "status": campaign.campaign_status.as_str(),
            "completed_at": campaign.completed_at,
            "approved_by": campaign.approved_by,
            "approved_at": campaign.approved_at,
        },
        "inspectors": inspectors,
        "summary": summary_json(&all),
        "results": rows,
        "failures": failures,
        "waived": waived,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{ReportEngine, ReportMeta, ReportStatus, ReportType, RevisionEntry};
    use crate::storage::test_runs::{
        compute_campaign_progress, CampaignStatus, CheckResult, ChecklistItem, EvidenceEntry,
        TestRunScope,
    };

    fn trace_report() -> Value {
        json!({
            "trace_layers": { "in_use": ["system_test"] },
            "items": [
                { "id": "ST-1", "layer": "system_test", "title": "Login works",
                  "state": "passing", "waivers": [], "tasks": [] },
                { "id": "ST-2", "layer": "system_test", "title": "Login | fails",
                  "state": "failing", "waivers": [],
                  "tasks": [{ "id": "t9", "role": "fix" }] },
                { "id": "ST-3", "layer": "system_test", "title": "Print",
                  "state": "not_run",
                  "waivers": [{ "axis": "verify", "reason": "checked by eye" }],
                  "tasks": [] }
            ]
        })
    }

    fn check(id: &str, result: CheckResult, by: Option<&str>, note: &str) -> ChecklistItem {
        ChecklistItem {
            item_id: id.into(),
            acceptance_text: format!("accept {id}"),
            result,
            evidence: Vec::new(),
            note: note.into(),
            verified_by: by.map(str::to_string),
            verified_at: Some("2026-10-07T00:00:00.000Z".into()),
        }
    }

    fn campaign(checklist: Vec<ChecklistItem>, status: CampaignStatus) -> TestRunRecord {
        TestRunRecord {
            test_run_id: "tr-1".into(),
            created_at: "2026-10-01T00:00:00.000Z".into(),
            label: Some("Release 1".into()),
            scope: TestRunScope::default(),
            target_items: checklist.iter().map(|c| c.item_id.clone()).collect(),
            total_target_count: checklist.len(),
            progress: compute_campaign_progress(&checklist),
            checklist,
            campaign_status: status,
            completed_at: Some("2026-10-07T12:00:00.000Z".into()),
            approved_by: Some("dave".into()),
            approved_at: Some("2026-10-08T00:00:00.000Z".into()),
        }
    }

    fn mixed_campaign() -> TestRunRecord {
        let mut with_evidence = check("ST-1", CheckResult::Pass, Some("carol"), "fine");
        with_evidence.evidence = vec![EvidenceEntry {
            path: "docs/shot 1.png".into(),
            evidence_type: "screenshot".into(),
            caption: String::new(),
        }];
        campaign(
            vec![
                with_evidence,
                check("ST-2", CheckResult::Fail, Some("erin"), "401 expected"),
                check("ST-3", CheckResult::Waived, Some("carol"), "approved by PO"),
            ],
            CampaignStatus::Approved,
        )
    }

    fn task_status(id: &str) -> String {
        if id == "t9" { "in_progress" } else { "unknown" }.to_string()
    }

    fn build(c: &TestRunRecord, scope: &ReportScope) -> Result<Value> {
        let trace = trace_report();
        build_inspection_data(
            scope,
            &InspectionInputs {
                trace_report: &trace,
                task_status: &task_status,
                campaign: c,
                project_name: "Demo",
                author: Some("bob"),
                evidence_href_prefix: "../../",
            },
        )
    }

    fn all_pass() -> TestRunRecord {
        campaign(
            vec![
                check("ST-1", CheckResult::Pass, Some("carol"), ""),
                check("ST-3", CheckResult::Waived, Some("carol"), ""),
            ],
            CampaignStatus::Approved,
        )
    }

    #[test]
    fn data_covers_every_checklist_row_with_summary_and_failures() {
        let data = build(&mixed_campaign(), &ReportScope::default()).unwrap();
        assert_eq!(data["document_no"], "INSP-tr-1");
        assert_eq!(data["project_name"], "Demo");
        assert_eq!(data["author"], "bob");
        assert_eq!(data["campaign"]["approved_by"], "dave");
        assert_eq!(data["summary"]["total"], 3);
        assert_eq!(data["summary"]["pass"], 1);
        assert_eq!(data["summary"]["fail"], 1);
        assert_eq!(data["summary"]["waived"], 1);

        let results = data["results"].as_array().unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0]["item_id"], "ST-1");
        assert_eq!(results[0]["title"], "Login works");
        assert_eq!(results[0]["evidence"][0]["href"], "../../docs/shot%201.png");

        let failures = data["failures"].as_array().unwrap();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0]["item_id"], "ST-2");
        assert_eq!(failures[0]["tasks"][0]["id"], "t9");
        assert_eq!(failures[0]["tasks"][0]["status"], "in_progress");

        let waived = data["waived"].as_array().unwrap();
        assert_eq!(waived[0]["item_id"], "ST-3");
        assert_eq!(waived[0]["reason"], "checked by eye");
        assert_eq!(waived[0]["approver"], "dave");
    }

    #[test]
    fn inspectors_are_the_distinct_sorted_verifiers() {
        let data = build(&mixed_campaign(), &ReportScope::default()).unwrap();
        assert_eq!(data["inspectors"], json!(["carol", "erin"]));
    }

    #[test]
    fn verdict_is_pass_only_when_nothing_failed_blocked_or_pending() {
        let pass = build(&all_pass(), &ReportScope::default()).unwrap();
        assert_eq!(pass["verdict"], "pass");

        let fail = build(&mixed_campaign(), &ReportScope::default()).unwrap();
        assert_eq!(fail["verdict"], "fail");

        let blocked = campaign(
            vec![check("ST-1", CheckResult::Blocked, None, "")],
            CampaignStatus::Approved,
        );
        let data = build(&blocked, &ReportScope::default()).unwrap();
        assert_eq!(data["verdict"], "fail");
        assert_eq!(data["failures"].as_array().unwrap().len(), 1);

        let pending = campaign(
            vec![
                check("ST-1", CheckResult::Pass, None, ""),
                check("ST-3", CheckResult::Pending, None, ""),
            ],
            CampaignStatus::Approved,
        );
        let data = build(&pending, &ReportScope::default()).unwrap();
        assert_eq!(data["verdict"], "incomplete");
    }

    #[test]
    fn a_campaign_that_is_not_approved_is_rejected() {
        for status in [
            CampaignStatus::Draft,
            CampaignStatus::InProgress,
            CampaignStatus::Completed,
        ] {
            let c = campaign(vec![check("ST-1", CheckResult::Pass, None, "")], status);
            let err = build(&c, &ReportScope::default()).unwrap_err().to_string();
            assert!(err.contains("not approved"), "{err}");
            assert!(err.contains(status.as_str()), "{err}");
        }
    }

    #[test]
    fn scope_fields_that_would_narrow_or_shift_the_report_are_rejected() {
        let c = all_pass();
        let scopes = [
            ReportScope {
                layers: vec!["system_test".into()],
                ..Default::default()
            },
            ReportScope {
                items: vec!["ST-1".into()],
                ..Default::default()
            },
            ReportScope {
                statuses: vec!["fail".into()],
                ..Default::default()
            },
            ReportScope {
                period: Some("2026-W41".into()),
                ..Default::default()
            },
            ReportScope {
                from: Some("2026-10-01".into()),
                ..Default::default()
            },
            ReportScope {
                assignee: Some("a".into()),
                ..Default::default()
            },
        ];
        for scope in scopes {
            let err = build(&c, &scope).unwrap_err().to_string();
            assert!(err.contains("inspection"), "{err}");
        }
    }

    #[test]
    fn label_alone_is_an_accepted_scope_field() {
        let scope = ReportScope {
            label: Some("final".into()),
            ..Default::default()
        };
        assert!(build(&all_pass(), &scope).is_ok());
    }

    fn render(data: &Value, history: Vec<RevisionEntry>) -> String {
        let meta = ReportMeta {
            report_id: "inspection-1".into(),
            report_type: ReportType::Inspection,
            scope: ReportScope::default(),
            version: 3,
            status: ReportStatus::Draft,
            generated_at: "2026-10-08T00:00:00+00:00".into(),
            reviewer: None,
            approved_at: None,
            output_path: "reports/inspection-1.md".into(),
            revision_history: history,
        };
        ReportEngine::new().unwrap().generate(&meta, data).unwrap()
    }

    fn creation() -> RevisionEntry {
        RevisionEntry {
            ts: "2026-10-08T00:00:00+00:00".into(),
            from: None,
            to: ReportStatus::Draft,
            actor: Some("bob".into()),
            comment: None,
        }
    }

    #[test]
    fn template_renders_document_control_judgements_and_signatures() {
        let data = build(&mixed_campaign(), &ReportScope::default()).unwrap();
        let md = render(&data, vec![creation()]);
        assert!(md.starts_with("# Inspection Certificate"), "{md}");
        assert!(md.contains("| Document No. | INSP-tr-1 |"), "{md}");
        assert!(md.contains("| Version | 3 |"), "{md}");
        assert!(md.contains("| Overall judgement | FAIL |"), "{md}");
        assert!(md.contains("| Inspectors | carol, erin |"), "{md}");
        assert!(md.contains("| Approver | dave |"), "{md}");
        // Every item with its judgement; pipe in title escaped.
        assert!(md.contains("Login \\| fails"), "{md}");
        assert!(md.contains("| ST-1 |"), "{md}");
        assert!(
            md.contains("[docs/shot 1.png](../../docs/shot%201.png)"),
            "{md}"
        );
        // Revision history.
        assert!(md.contains("## Revision History"), "{md}");
        assert!(
            md.contains("| 2026-10-08T00:00:00+00:00 | - | draft | bob | - |"),
            "{md}"
        );
        // Non-conforming item with disposition column + follow-up task.
        assert!(md.contains("## Non-conforming Items"), "{md}");
        assert!(md.contains("t9 (in_progress)"), "{md}");
        // Waiver.
        assert!(md.contains("checked by eye"), "{md}");
        // Signatures.
        assert!(md.contains("## Signatures"), "{md}");
        assert!(md.contains("| Inspector | carol, erin | | |"), "{md}");
    }

    #[test]
    fn template_for_a_clean_campaign_has_no_nonconforming_items() {
        let data = build(&all_pass(), &ReportScope::default()).unwrap();
        let md = render(&data, vec![creation()]);
        assert!(md.contains("| Overall judgement | PASS |"), "{md}");
        assert!(md.contains("No non-conforming items."), "{md}");
    }
}
