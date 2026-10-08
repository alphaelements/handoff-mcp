//! Completion report data (R8: FR-531 / SPEC-531).
//!
//! The completion report integrates the whole project: scope and schedule,
//! per-layer verification, milestones, the defect register, effort against
//! estimate, and the approved reports. It is only issued for a finished
//! project: [`require_complete`] refuses unless the trace report's
//! `project_status` is `complete` (every layer approved, FR-510).
//! [`build_completion_data`] is a pure function from already-loaded project
//! state to the JSON `completion.md.hbs` renders as `data`.
//!
//! Output shape (hours rounded to 2 decimals; absent values are `null`):
//!
//! ```text
//! project       { name, status, started_on, completed_on }
//! stats         { tasks_total, done, skipped, open, estimate_hours, actual_hours,
//!                 variance_hours, variance_percent, logged_hours }
//! verification  { available, project_status, layers: [...] }     as in the weekly report
//! milestones    [ ... ]                                           as in the weekly report
//! defects       the defect register (see [`super::defect`]), unfiltered
//! effort        { stats, by_assignee, deviation }                 as in the effort report
//! reports       [ { report_id, report_type, version, status, reviewer, approved_at } ]
//! warnings      [ string ]
//! ```
//!
//! `started_on` is the earliest task `created_at` and `completed_on` the
//! latest `completed_at` of a done task. `reports` lists the approved or
//! published reports (newest first) other than completion reports.

use std::collections::HashMap;

use anyhow::{bail, Result};
use serde_json::{json, Value};

use super::defect::{build_defect_data, DefectInputs};
use super::effort::{build_effort_data, EffortInputs};
use super::weekly::{self, percent, round2, ts_date};
use super::{ReportMeta, ReportScope, ReportStatus, ReportType};
use crate::storage::config::MilestoneConfig;
use crate::storage::tasks::{is_terminal_status, TaskData};
use crate::storage::time_log::TimeLogEntry;

/// `project_status` value of a project whose layers are all approved.
const PROJECT_COMPLETE: &str = "complete";

/// A completion report covers the whole project; any scope field other than
/// `label` would be silently ignored, so it is an error instead.
pub fn validate_scope(scope: &ReportScope) -> Result<()> {
    scope.require_only(
        ReportType::Completion,
        &[],
        "it covers the whole project; only scope.label applies",
    )
}

/// Errors unless the trace report says every layer is approved.
pub fn require_complete(trace_report: &Value) -> Result<()> {
    let status = trace_report
        .get("project_status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if status != PROJECT_COMPLETE {
        bail!(
            "A completion report needs project_status = {PROJECT_COMPLETE} (every layer approved), \
             but project_status is '{status}'"
        );
    }
    Ok(())
}

pub struct CompletionInputs<'a> {
    pub project_name: &'a str,
    /// Every task with its current status.
    pub tasks: &'a [(TaskData, String)],
    pub time_log: &'a [TimeLogEntry],
    pub milestones: &'a HashMap<String, MilestoneConfig>,
    /// The trace report, with `items[]`.
    pub trace_report: &'a Value,
    /// Every stored report (any status).
    pub reports: &'a [ReportMeta],
}

pub fn build_completion_data(inputs: &CompletionInputs) -> Result<Value> {
    require_complete(inputs.trace_report)?;

    let tasks: Vec<&(TaskData, String)> = inputs.tasks.iter().collect();
    let count = |status: &str| tasks.iter().filter(|(_, s)| s == status).count();
    let done = count("done");
    let terminal = tasks.iter().filter(|(_, s)| is_terminal_status(s)).count();
    let hours = |pick: fn(&crate::storage::tasks::Schedule) -> Option<f64>| -> f64 {
        tasks
            .iter()
            .filter_map(|(d, _)| d.schedule.as_ref().and_then(pick))
            .sum()
    };
    let estimate = hours(|s| s.estimate_hours);
    let actual = hours(|s| s.actual_hours);
    let logged: f64 = inputs.time_log.iter().map(|e| e.hours).sum();

    let started_on = tasks
        .iter()
        .filter_map(|(d, _)| d.created_at.as_deref().and_then(ts_date))
        .min();
    let completed_on = tasks
        .iter()
        .filter(|(_, s)| s == "done")
        .filter_map(|(d, _)| d.completed_at.as_deref().and_then(ts_date))
        .max();

    let defects = build_defect_data(&DefectInputs {
        tasks: inputs.tasks,
        trace: Ok(inputs.trace_report),
        period: None,
        assignee: None,
        layers: &[],
        items: &[],
    })?;
    let effort = build_effort_data(&EffortInputs {
        period: None,
        assignee: None,
        tasks: inputs.tasks,
        time_log: inputs.time_log,
    });

    let mut warnings: Vec<Value> = Vec::new();
    for source in [&defects, &effort] {
        warnings.extend(source["warnings"].as_array().cloned().unwrap_or_default());
    }

    let reports: Vec<Value> = inputs
        .reports
        .iter()
        .filter(|m| m.report_type != ReportType::Completion)
        .filter(|m| matches!(m.status, ReportStatus::Approved | ReportStatus::Published))
        .map(|m| {
            json!({
                "report_id": m.report_id,
                "report_type": m.report_type,
                "version": m.version,
                "status": m.status,
                "reviewer": m.reviewer,
                "approved_at": m.approved_at,
            })
        })
        .collect();

    Ok(json!({
        "project": {
            "name": inputs.project_name,
            "status": inputs.trace_report["project_status"],
            "started_on": started_on.map(|d| d.to_string()),
            "completed_on": completed_on.map(|d| d.to_string()),
        },
        "stats": {
            "tasks_total": tasks.len(),
            "done": done,
            "skipped": terminal - done,
            "open": tasks.len() - terminal,
            "estimate_hours": round2(estimate),
            "actual_hours": round2(actual),
            "variance_hours": round2(actual - estimate),
            "variance_percent": percent(actual - estimate, estimate),
            "logged_hours": round2(logged),
        },
        "verification": weekly::verification_block(inputs.trace_report),
        "milestones": weekly::milestone_rows(&tasks, inputs.milestones),
        "defects": defects,
        "effort": {
            "stats": effort["stats"],
            "by_assignee": effort["by_assignee"],
            "deviation": effort["deviation"],
        },
        "reports": reports,
        "warnings": warnings,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{ReportScope, ReportStatus};
    use crate::storage::tasks::Schedule;

    fn task(id: &str, status: &str, f: impl FnOnce(&mut TaskData)) -> (TaskData, String) {
        let mut data = TaskData {
            id: id.into(),
            title: format!("Title {id}"),
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
        (data, status.into())
    }

    fn trace(status: &str) -> Value {
        json!({
            "project_status": status,
            "trace_layers": { "in_use": ["requirement", "acceptance"] },
            "layer_statuses": { "requirement": "approved", "acceptance": "approved" },
            "coverage": {
                "requirement": { "total": 1, "state": { "passing": 1 } },
                "acceptance": { "total": 1, "state": { "passing": 1 } },
            },
            "items": [],
        })
    }

    fn meta(id: &str, t: ReportType, status: ReportStatus) -> ReportMeta {
        ReportMeta {
            report_id: id.into(),
            report_type: t,
            scope: ReportScope::default(),
            version: 1,
            status,
            generated_at: "2026-10-08T00:00:00+00:00".into(),
            reviewer: Some("qa".into()),
            approved_at: Some("2026-10-08T01:00:00+00:00".into()),
            output_path: format!("reports/{id}.md"),
            revision_history: vec![],
        }
    }

    #[test]
    fn only_a_complete_project_is_accepted() {
        assert!(require_complete(&trace("complete")).is_ok());
        for status in ["verified", "in_progress", "under_review", "not_started"] {
            let err = require_complete(&trace(status)).unwrap_err().to_string();
            assert!(
                err.contains(status) && err.contains("project_status"),
                "{err}"
            );
        }
        let err = require_complete(&json!({})).unwrap_err().to_string();
        assert!(err.contains("unknown"), "{err}");
    }

    #[test]
    fn scope_takes_only_a_label() {
        let ok = ReportScope {
            label: Some("v1".into()),
            ..Default::default()
        };
        assert!(validate_scope(&ok).is_ok());
        let bad = ReportScope {
            layers: vec!["unit".into()],
            ..Default::default()
        };
        assert!(validate_scope(&bad)
            .unwrap_err()
            .to_string()
            .contains("completion"));
    }

    #[test]
    fn data_integrates_stats_defects_effort_and_approved_reports() {
        let tasks = [
            task("t1", "done", |t| {
                t.created_at = Some("2026-09-01T00:00:00+00:00".into());
                t.completed_at = Some("2026-10-03T00:00:00+00:00".into());
                t.assignee = Some("alice".into());
                t.schedule = Some(Schedule {
                    estimate_hours: Some(10.0),
                    actual_hours: Some(12.0),
                    milestone: Some("beta".into()),
                    ..Default::default()
                });
            }),
            task("t2", "skipped", |_| {}),
            task("b1", "done", |t| {
                t.labels = vec!["bug".into(), "bug:fix".into()]
            }),
            task("t3", "todo", |_| {}),
        ];
        let time_log = [TimeLogEntry {
            ts: "2026-09-10T10:00:00+00:00".into(),
            task_id: "t1".into(),
            hours: 12.0,
            agent_id: None,
            note: None,
        }];
        let milestones = HashMap::new();
        let reports = [
            meta(
                "verification-1",
                ReportType::Verification,
                ReportStatus::Approved,
            ),
            meta("weekly-1", ReportType::Weekly, ReportStatus::Draft),
            meta(
                "inspection-1",
                ReportType::Inspection,
                ReportStatus::Published,
            ),
            meta(
                "completion-1",
                ReportType::Completion,
                ReportStatus::Approved,
            ),
        ];
        let trace_report = trace("complete");
        let data = build_completion_data(&CompletionInputs {
            project_name: "demo",
            tasks: &tasks,
            time_log: &time_log,
            milestones: &milestones,
            trace_report: &trace_report,
            reports: &reports,
        })
        .unwrap();

        assert_eq!(data["project"]["name"], "demo");
        assert_eq!(data["project"]["status"], "complete");
        assert_eq!(data["project"]["started_on"], "2026-09-01");
        assert_eq!(data["project"]["completed_on"], "2026-10-03");
        let stats = &data["stats"];
        assert_eq!(stats["tasks_total"], 4);
        assert_eq!(stats["done"], 2);
        assert_eq!(stats["skipped"], 1);
        assert_eq!(stats["open"], 1);
        assert_eq!(stats["variance_hours"], 2.0);
        assert_eq!(stats["variance_percent"], 20.0);
        assert_eq!(stats["logged_hours"], 12.0);

        assert_eq!(data["verification"]["project_status"], "complete");
        assert_eq!(data["verification"]["layers"][0]["status"], "approved");
        assert_eq!(data["milestones"][0]["name"], "beta");
        assert_eq!(data["defects"]["stats"]["total"], 1);
        assert_eq!(data["effort"]["stats"]["total_hours"], 12.0);
        assert_eq!(data["effort"]["deviation"]["actual_hours"], 12.0);

        let ids: Vec<&str> = data["reports"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["report_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["verification-1", "inspection-1"]);
    }

    #[test]
    fn incomplete_project_is_refused_by_the_builder_too() {
        let milestones = HashMap::new();
        let trace_report = trace("verified");
        let err = build_completion_data(&CompletionInputs {
            project_name: "demo",
            tasks: &[],
            time_log: &[],
            milestones: &milestones,
            trace_report: &trace_report,
            reports: &[],
        })
        .unwrap_err();
        assert!(err.to_string().contains("verified"));
    }
}
