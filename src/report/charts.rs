//! Chart selection for reports (REQ-VIS-020).
//!
//! Determines which charts a report type needs based on the report data.
//! Chart rendering is delegated to the VSCode extension via the file channel
//! (see [`crate::charts::file_channel`]); this module only decides which
//! charts to request.
//!
//! The Mermaid source generation that was previously here has been removed
//! (REQ-VIS-020). User-written Mermaid in document bodies is still rendered
//! by [`crate::storage::docs::pdf::mermaid`].

use serde_json::{Map, Value};

use super::period::Period;
use super::ReportType;
use crate::storage::tasks::{is_terminal_status, TaskData};

/// Makes free text safe inside a Mermaid title / gantt task name: the
/// characters Mermaid treats as syntax (`:` `;` `#` `"` `%`) are dropped and
/// whitespace (including line breaks) collapses to single spaces.
///
/// Still used by the trace diagram module for user-written Mermaid content.
pub(super) fn sanitize(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, ':' | ';' | '#' | '"' | '%'))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Returns whether there are tasks that can be placed on a burndown chart:
/// at least one task with a readable created_at or completed_at.
fn has_burndown_data(tasks: &[(TaskData, String)]) -> bool {
    tasks.iter().any(|(data, status)| {
        let has_created = data.created_at.is_some();
        let has_completed = is_terminal_status(status) && data.completed_at.is_some();
        has_created || has_completed
    })
}

/// Returns whether there are tasks (velocity always has data when tasks exist,
/// since it at least shows zero-count weeks).
fn has_velocity_data(tasks: &[(TaskData, String)]) -> bool {
    !tasks.is_empty()
}

/// Returns whether the verification data has non-zero verdicts.
fn has_coverage_data(data: &Value) -> bool {
    let Some(summary) = data.get("summary") else {
        return false;
    };
    ["pass", "fail", "blocked", "waived", "pending"]
        .iter()
        .any(|key| summary.get(*key).and_then(Value::as_u64).unwrap_or(0) > 0)
}

/// Returns whether the milestone data has dated tasks or a planned date.
fn has_timeline_data(data: &Value) -> bool {
    let has_dated_tasks = data["tasks"].as_array().into_iter().flatten().any(|task| {
        task.get("start_date").and_then(Value::as_str).is_some()
            || task.get("due_date").and_then(Value::as_str).is_some()
    });
    let has_planned = data
        .get("milestone")
        .and_then(|m| m.get("planned_date"))
        .and_then(Value::as_str)
        .is_some();
    has_dated_tasks || has_planned
}

/// Returns whether effort data has time logged.
fn has_effort_data(data: &Value) -> bool {
    data["by_day"].as_array().is_some_and(|arr| !arr.is_empty())
}

/// The chart names a `report_type` report needs, as a `{name: true}` map.
/// Empty for types without charts. The progress charts of weekly/monthly
/// reports need the report `period`; without one they are skipped.
///
/// The values are `true` (a marker); the actual chart content is rendered by
/// the VSCode extension via the file channel. The caller
/// ([`crate::mcp::handlers::report::with_charts`]) replaces these markers
/// with image references or fallback notes.
pub fn build_charts(
    report_type: ReportType,
    data: &Value,
    tasks: &[(TaskData, String)],
    period: Option<Period>,
) -> Map<String, Value> {
    let mut charts = Map::new();
    let mut put = |name: &str, available: bool| {
        if available {
            charts.insert(name.to_string(), Value::Bool(true));
        }
    };
    match report_type {
        ReportType::Weekly | ReportType::Monthly => {
            if period.is_some() {
                put("burndown", has_burndown_data(tasks));
                put("velocity", has_velocity_data(tasks));
            }
        }
        ReportType::Verification => put("coverage", has_coverage_data(data)),
        ReportType::Milestone => put("timeline", has_timeline_data(data)),
        ReportType::Effort => put("cumulative_hours", has_effort_data(data)),
        ReportType::Inspection
        | ReportType::Defect
        | ReportType::Completion
        | ReportType::Daily
        | ReportType::Quarterly
        | ReportType::Yearly => {}
    }
    charts
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    fn task(
        id: &str,
        status: &str,
        created: Option<&str>,
        completed: Option<&str>,
    ) -> (TaskData, String) {
        let data = TaskData {
            id: id.into(),
            title: format!("Title {id}"),
            notes: None,
            priority: None,
            created_at: created.map(Into::into),
            updated_at: None,
            completed_at: completed.map(Into::into),
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
        (data, status.into())
    }

    fn week() -> Period {
        Period::parse("2026-W41").unwrap() // 2026-10-05..2026-10-11
    }

    #[test]
    fn burndown_detected_when_tasks_have_dates() {
        let tasks = vec![
            task(
                "1",
                "done",
                Some("2026-10-01"),
                Some("2026-10-06T10:00:00Z"),
            ),
            task("2", "in_progress", Some("2026-10-01"), None),
        ];
        assert!(has_burndown_data(&tasks));
    }

    #[test]
    fn burndown_not_detected_without_tasks() {
        assert!(!has_burndown_data(&[]));
    }

    #[test]
    fn velocity_detected_when_tasks_exist() {
        let tasks = vec![task("1", "done", None, Some("2026-10-06T10:00:00Z"))];
        assert!(has_velocity_data(&tasks));
        assert!(!has_velocity_data(&[]));
    }

    #[test]
    fn coverage_detected_with_non_zero_verdicts() {
        let data = json!({"summary": {"total": 6, "pass": 3, "fail": 1, "blocked": 0, "waived": 0, "pending": 2}});
        assert!(has_coverage_data(&data));
    }

    #[test]
    fn coverage_not_detected_when_all_zero() {
        let data = json!({"summary": {"total": 0, "pass": 0, "fail": 0, "blocked": 0, "waived": 0, "pending": 0}});
        assert!(!has_coverage_data(&data));
        assert!(!has_coverage_data(&json!({})));
    }

    #[test]
    fn timeline_detected_with_dated_tasks() {
        let data = json!({
            "milestone": {"name": "v1", "planned_date": "2026-10-31"},
            "tasks": [{"id": "1", "start_date": "2026-10-01", "due_date": "2026-10-05"}]
        });
        assert!(has_timeline_data(&data));
    }

    #[test]
    fn timeline_not_detected_without_dates() {
        let data = json!({
            "milestone": {"name": "m", "planned_date": null},
            "tasks": [{"id": "1", "start_date": null, "due_date": null}]
        });
        assert!(!has_timeline_data(&data));
    }

    #[test]
    fn effort_detected_with_logged_hours() {
        let data = json!({"by_day": [
            {"date": "2026-10-05", "hours": 2.5, "entries": 1}
        ], "by_week": []});
        assert!(has_effort_data(&data));
    }

    #[test]
    fn effort_not_detected_without_time_logged() {
        assert!(!has_effort_data(&json!({"by_day": [], "by_week": []})));
    }

    #[test]
    fn build_charts_selects_charts_by_report_type() {
        let tasks = vec![task("1", "in_progress", Some("2026-10-01"), None)];
        let names = |t: ReportType, data: &Value| -> Vec<String> {
            build_charts(t, data, &tasks, Some(week()))
                .keys()
                .cloned()
                .collect()
        };
        assert_eq!(
            names(ReportType::Weekly, &json!({})),
            ["burndown", "velocity"]
        );
        assert_eq!(
            names(ReportType::Monthly, &json!({})),
            ["burndown", "velocity"]
        );
        assert_eq!(names(ReportType::Defect, &json!({})), Vec::<String>::new());
        assert_eq!(
            names(ReportType::Completion, &json!({})),
            Vec::<String>::new()
        );
        assert_eq!(
            names(ReportType::Verification, &json!({"summary": {"pass": 1}})),
            ["coverage"]
        );
        assert_eq!(
            names(ReportType::Verification, &json!({})),
            Vec::<String>::new()
        );
        assert_eq!(
            names(
                ReportType::Effort,
                &json!({"by_day": [{"date": "2026-10-05", "hours": 1.0}], "by_week": []})
            ),
            ["cumulative_hours"]
        );
    }

    #[test]
    fn build_charts_skips_progress_charts_without_a_period() {
        let tasks = vec![task("1", "in_progress", None, None)];
        assert!(build_charts(ReportType::Weekly, &json!({}), &tasks, None).is_empty());
    }
}
