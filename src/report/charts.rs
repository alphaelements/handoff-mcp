//! Automatic Mermaid charts for reports (REQ-PDF-031).
//!
//! Each builder returns the Mermaid source of one chart (without the code
//! fence) or `None` when there is nothing worth drawing. [`build_charts`]
//! collects them per report type into the `charts` object of the report's
//! `data`; the built-in templates wrap each entry in a ```` ```mermaid ````
//! block, which the PDF pipeline later renders to SVG.

use chrono::{Datelike, Duration, NaiveDate};
use serde_json::{Map, Value};

use super::period::{parse_date, Period};
use super::weekly::{round2, ts_date};
use super::ReportType;
use crate::storage::tasks::{is_terminal_status, TaskData};

/// Upper bound on the points of one line/bar series; longer series are
/// thinned (burndown) or switched to a coarser grain (effort).
pub const MAX_POINTS: usize = 31;

/// Weeks shown by the velocity chart at least, counted back from the end of
/// the report period.
pub const VELOCITY_MIN_WEEKS: usize = 4;

/// Weeks shown by the velocity chart at most.
pub const VELOCITY_MAX_WEEKS: usize = 26;

/// Status of a task that counts as completed work in the velocity chart.
const STATUS_DONE: &str = "done";

/// Gantt task tag per task status (`done` / `active`; others untagged).
fn gantt_tag(status: &str) -> Option<&'static str> {
    match status {
        STATUS_DONE => Some("done"),
        "in_progress" => Some("active"),
        _ => None,
    }
}

/// Labels and keys of the verification pie, in display order. These are the
/// verdicts of the verification report's own summary table.
const PIE_SLICES: [(&str, &str); 5] = [
    ("Pass", "pass"),
    ("Fail", "fail"),
    ("Blocked", "blocked"),
    ("Waived", "waived"),
    ("Pending", "pending"),
];

/// Makes free text safe inside a Mermaid title / gantt task name: the
/// characters Mermaid treats as syntax (`:` `;` `#` `"` `%`) are dropped and
/// whitespace (including line breaks) collapses to single spaces.
pub(super) fn sanitize(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, ':' | ';' | '#' | '"' | '%'))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn quoted_list(labels: &[String]) -> String {
    labels
        .iter()
        .map(|l| format!("\"{l}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

fn number_list(values: &[f64]) -> String {
    values
        .iter()
        .map(f64::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// An `xychart-beta` with a single `kind` (`line`|`bar`) series. The y axis
/// spans the series and zero; `f64::to_string` prints whole numbers without
/// a fraction, which Mermaid accepts.
fn xychart(title: &str, y_label: &str, kind: &str, labels: &[String], values: &[f64]) -> String {
    let low = values.iter().copied().fold(0.0_f64, f64::min).floor();
    let high = values
        .iter()
        .copied()
        .fold(0.0_f64, f64::max)
        .ceil()
        .max(low + 1.0);
    format!(
        "xychart-beta\n    title \"{title}\"\n    x-axis [{}]\n    y-axis \"{y_label}\" {low} --> {high}\n    {kind} [{}]\n",
        quoted_list(labels),
        number_list(values),
    )
}

fn task_date(ts: &Option<String>) -> Option<NaiveDate> {
    ts.as_deref().and_then(ts_date)
}

/// Remaining (not yet done/skipped) tasks at the end of each day of
/// `period`. A task counts from its creation day (unknown: always) until
/// its completion day; a finished task without a readable `completed_at`
/// cannot be placed on the timeline and is left out. A period longer than
/// [`MAX_POINTS`] days is sampled at an even step, always ending on its
/// last day.
pub fn burndown(tasks: &[(TaskData, String)], period: Period) -> Option<String> {
    // (created day, finished day) of every task that can be placed.
    let spans: Vec<(Option<NaiveDate>, Option<NaiveDate>)> = tasks
        .iter()
        .filter_map(|(data, status)| {
            let finished = if is_terminal_status(status) {
                Some(task_date(&data.completed_at)?)
            } else {
                None
            };
            Some((task_date(&data.created_at), finished))
        })
        .collect();
    if spans.is_empty() {
        return None;
    }
    let step = (period.days() as usize).div_ceil(MAX_POINTS).max(1) as i64;
    let mut days = Vec::new();
    let mut day = period.start;
    while day <= period.end {
        days.push(day);
        day += Duration::days(step);
    }
    if days.last() != Some(&period.end) {
        days.push(period.end);
    }
    let labels: Vec<String> = days.iter().map(|d| d.format("%m-%d").to_string()).collect();
    let remaining: Vec<f64> = days
        .iter()
        .map(|&d| {
            spans
                .iter()
                .filter(|(created, finished)| {
                    created.is_none_or(|c| c <= d) && finished.is_none_or(|f| f > d)
                })
                .count() as f64
        })
        .collect();
    Some(xychart(
        "Burndown (remaining tasks)",
        "Remaining tasks",
        "line",
        &labels,
        &remaining,
    ))
}

/// Tasks completed (status `done`, by `completed_at`) per ISO week: every
/// week the period touches, extended back to at least
/// [`VELOCITY_MIN_WEEKS`] and trimmed to the last [`VELOCITY_MAX_WEEKS`].
pub fn velocity(tasks: &[(TaskData, String)], period: Period) -> Option<String> {
    if tasks.is_empty() {
        return None;
    }
    let last = Period::iso_week_of(period.end).start;
    let first = Period::iso_week_of(period.start)
        .start
        .min(last - Duration::weeks(VELOCITY_MIN_WEEKS as i64 - 1))
        .max(last - Duration::weeks(VELOCITY_MAX_WEEKS as i64 - 1));
    let completed: Vec<NaiveDate> = tasks
        .iter()
        .filter(|(_, status)| status == STATUS_DONE)
        .filter_map(|(data, _)| task_date(&data.completed_at))
        .collect();

    let mut labels = Vec::new();
    let mut counts = Vec::new();
    let mut start = first;
    while start <= last {
        let week = Period::iso_week_of(start);
        let iso = start.iso_week();
        labels.push(format!("{}-W{:02}", iso.year(), iso.week()));
        counts.push(completed.iter().filter(|d| week.contains(**d)).count() as f64);
        start += Duration::weeks(1);
    }
    Some(xychart(
        "Velocity (tasks completed per week)",
        "Tasks completed",
        "bar",
        &labels,
        &counts,
    ))
}

/// Pie of the verification report's verdicts (`data.summary`); zero slices
/// are left out and a report without items has no chart.
pub fn verification_pie(data: &Value) -> Option<String> {
    let summary = data.get("summary")?;
    let slices: Vec<String> = PIE_SLICES
        .iter()
        .filter_map(|(label, key)| {
            let count = summary.get(*key)?.as_u64().filter(|c| *c > 0)?;
            Some(format!("    \"{label}\" : {count}\n"))
        })
        .collect();
    if slices.is_empty() {
        return None;
    }
    Some(format!(
        "pie showData title Verification coverage\n{}",
        slices.concat()
    ))
}

fn data_date(row: &Value, key: &str) -> Option<NaiveDate> {
    row.get(key)?.as_str().and_then(|s| parse_date(s).ok())
}

/// Gantt of a milestone report: the milestone's dated tasks (start..due; a
/// single known date becomes a one-day bar) and a marker on the planned
/// date. Tasks without any date and an unparseable planned date are left
/// out; no entries at all means no chart.
pub fn milestone_gantt(data: &Value) -> Option<String> {
    let name = sanitize(data["milestone"]["name"].as_str().unwrap_or_default());
    let mut task_lines = String::new();
    let mut count = 0;
    for task in data["tasks"].as_array().into_iter().flatten() {
        let (start, end) = match (data_date(task, "start_date"), data_date(task, "due_date")) {
            (Some(start), Some(due)) => (start, due.max(start).to_string()),
            (Some(start), None) => (start, "1d".to_string()),
            (None, Some(due)) => (due, "1d".to_string()),
            (None, None) => continue,
        };
        count += 1;
        let title = sanitize(&format!(
            "{} {}",
            task["id"].as_str().unwrap_or_default(),
            task["title"].as_str().unwrap_or_default()
        ));
        let tag = gantt_tag(task["status"].as_str().unwrap_or_default())
            .map(|t| format!("{t}, "))
            .unwrap_or_default();
        task_lines.push_str(&format!("    {title} :{tag}t{count}, {start}, {end}\n"));
    }
    let planned = data_date(&data["milestone"], "planned_date");
    if count == 0 && planned.is_none() {
        return None;
    }
    let mut chart = format!("gantt\n    title Milestone {name}\n    dateFormat YYYY-MM-DD\n");
    if count > 0 {
        chart.push_str("    section Tasks\n");
        chart.push_str(&task_lines);
    }
    if let Some(planned) = planned {
        chart.push_str(&format!(
            "    section Milestone\n    {name} :milestone, m1, {planned}, 0d\n"
        ));
    }
    Some(chart)
}

/// Cumulative logged hours: per day, or per ISO week when the log spans
/// more than [`MAX_POINTS`] days. Corrections may pull the total below zero;
/// the y axis follows. No time logged means no chart.
pub fn effort_cumulative(data: &Value) -> Option<String> {
    let daily = data["by_day"].as_array().map_or(&[][..], Vec::as_slice);
    let (rows, key) = if daily.len() > MAX_POINTS {
        (
            data["by_week"].as_array().map_or(&[][..], Vec::as_slice),
            "week",
        )
    } else {
        (daily, "date")
    };
    let mut labels = Vec::new();
    let mut totals = Vec::new();
    let mut sum = 0.0;
    for row in rows {
        sum += row["hours"].as_f64()?;
        labels.push(row[key].as_str()?.to_string());
        totals.push(round2(sum));
    }
    if labels.is_empty() {
        return None;
    }
    Some(xychart(
        "Cumulative effort (hours)",
        "Cumulative hours",
        "line",
        &labels,
        &totals,
    ))
}

/// The charts of a `report_type` report as a `{name: mermaid source}`
/// object, ready to be stored as `data.charts`; empty for types without
/// charts. The progress charts of weekly/monthly reports need the report
/// `period`; without one they are skipped.
pub fn build_charts(
    report_type: ReportType,
    data: &Value,
    tasks: &[(TaskData, String)],
    period: Option<Period>,
) -> Map<String, Value> {
    let mut charts = Map::new();
    let mut put = |name: &str, chart: Option<String>| {
        if let Some(chart) = chart {
            // The templates put the closing fence on the next line.
            charts.insert(
                name.to_string(),
                Value::String(chart.trim_end().to_string()),
            );
        }
    };
    match report_type {
        ReportType::Weekly | ReportType::Monthly => {
            if let Some(period) = period {
                put("burndown", burndown(tasks, period));
                put("velocity", velocity(tasks, period));
            }
        }
        ReportType::Verification => put("coverage", verification_pie(data)),
        ReportType::Milestone => put("timeline", milestone_gantt(data)),
        ReportType::Effort => put("cumulative_hours", effort_cumulative(data)),
        ReportType::Inspection | ReportType::Defect | ReportType::Completion => {}
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
    fn burndown_counts_tasks_not_yet_finished_per_day() {
        let tasks = vec![
            task(
                "1",
                "done",
                Some("2026-10-01"),
                Some("2026-10-06T10:00:00Z"),
            ),
            task(
                "2",
                "done",
                Some("2026-10-01"),
                Some("2026-10-08T10:00:00Z"),
            ),
            task("3", "in_progress", Some("2026-10-01"), None),
        ];
        let chart = burndown(&tasks, week()).unwrap();
        assert!(chart.starts_with("xychart-beta\n"), "{chart}");
        assert!(
            chart.contains(
                "x-axis [\"10-05\", \"10-06\", \"10-07\", \"10-08\", \"10-09\", \"10-10\", \"10-11\"]"
            ),
            "{chart}"
        );
        assert!(chart.contains("line [3, 2, 2, 1, 1, 1, 1]"), "{chart}");
        assert!(
            chart.contains("y-axis \"Remaining tasks\" 0 --> 3"),
            "{chart}"
        );
    }

    #[test]
    fn burndown_ignores_tasks_created_after_the_day_and_undated_completions() {
        let tasks = vec![
            task("1", "in_progress", Some("2026-10-09T00:00:00Z"), None),
            task("2", "in_progress", None, None),
            task("3", "skipped", None, None),
        ];
        let chart = burndown(&tasks, week()).unwrap();
        assert!(chart.contains("line [1, 1, 1, 1, 2, 2, 2]"), "{chart}");
    }

    #[test]
    fn burndown_is_none_without_tasks() {
        assert_eq!(burndown(&[], week()), None);
    }

    #[test]
    fn burndown_thins_long_periods_and_keeps_the_last_day() {
        let tasks = vec![task("1", "in_progress", Some("2026-01-01"), None)];
        let period = Period::parse("2026-01-01..2026-04-10").unwrap(); // 100 days
        let chart = burndown(&tasks, period).unwrap();
        let xs = chart.lines().find(|l| l.contains("x-axis")).unwrap();
        let n = xs.matches('"').count() / 2;
        assert!(n <= MAX_POINTS + 1, "{n} points");
        assert!(xs.contains("\"04-10\""), "{xs}");
    }

    #[test]
    fn velocity_counts_done_tasks_per_iso_week_for_at_least_four_weeks() {
        let tasks = vec![
            task("1", "done", None, Some("2026-10-06T10:00:00Z")),
            task("2", "done", None, Some("2026-10-07T10:00:00Z")),
            task("3", "done", None, Some("2026-09-22T10:00:00Z")),
            task("4", "skipped", None, Some("2026-10-07T10:00:00Z")),
            task("5", "in_progress", None, None),
        ];
        let chart = velocity(&tasks, week()).unwrap();
        assert!(
            chart.contains("x-axis [\"2026-W38\", \"2026-W39\", \"2026-W40\", \"2026-W41\"]"),
            "{chart}"
        );
        assert!(chart.contains("bar [0, 1, 0, 2]"), "{chart}");
        assert!(
            chart.contains("y-axis \"Tasks completed\" 0 --> 2"),
            "{chart}"
        );
    }

    #[test]
    fn velocity_covers_every_week_of_a_month_period() {
        let tasks = vec![task("1", "done", None, Some("2026-10-06"))];
        let month = Period::parse_month("2026-10").unwrap();
        let chart = velocity(&tasks, month).unwrap();
        // 2026-10-01 is in W40, 2026-10-31 in W44.
        assert!(
            chart.contains("\"2026-W40\", \"2026-W41\", \"2026-W42\", \"2026-W43\", \"2026-W44\""),
            "{chart}"
        );
    }

    #[test]
    fn pie_shows_non_zero_verdicts_only() {
        let data = json!({"summary": {"total": 6, "pass": 3, "fail": 1, "blocked": 0, "waived": 0, "pending": 2}});
        let chart = verification_pie(&data).unwrap();
        assert_eq!(
            chart,
            "pie showData title Verification coverage\n    \"Pass\" : 3\n    \"Fail\" : 1\n    \"Pending\" : 2\n"
        );
    }

    #[test]
    fn pie_is_none_when_there_are_no_items() {
        let data = json!({"summary": {"total": 0, "pass": 0, "fail": 0, "blocked": 0, "waived": 0, "pending": 0}});
        assert_eq!(verification_pie(&data), None);
        assert_eq!(verification_pie(&json!({})), None);
    }

    fn milestone_data() -> Value {
        json!({
            "milestone": {"name": "v1: beta", "planned_date": "2026-10-31"},
            "tasks": [
                {"id": "1", "title": "Design: API; #1", "status": "done", "start_date": "2026-10-01", "due_date": "2026-10-05"},
                {"id": "2", "title": "Build", "status": "in_progress", "start_date": null, "due_date": "2026-10-20"},
                {"id": "3", "title": "Docs", "status": "todo", "start_date": "2026-10-21", "due_date": null},
                {"id": "4", "title": "Undated", "status": "todo", "start_date": null, "due_date": null},
                {"id": "5", "title": "Backwards", "status": "todo", "start_date": "2026-10-10", "due_date": "2026-10-09"}
            ]
        })
    }

    #[test]
    fn gantt_lists_dated_tasks_and_the_milestone_marker() {
        let chart = milestone_gantt(&milestone_data()).unwrap();
        let expected = "gantt\n    title Milestone v1 beta\n    dateFormat YYYY-MM-DD\n    section Tasks\n    1 Design API 1 :done, t1, 2026-10-01, 2026-10-05\n    2 Build :active, t2, 2026-10-20, 1d\n    3 Docs :t3, 2026-10-21, 1d\n    5 Backwards :t4, 2026-10-10, 2026-10-10\n    section Milestone\n    v1 beta :milestone, m1, 2026-10-31, 0d\n";
        assert_eq!(chart, expected);
    }

    #[test]
    fn gantt_is_none_without_dates() {
        let data = json!({"milestone": {"name": "m", "planned_date": null},
            "tasks": [{"id": "1", "title": "x", "status": "todo", "start_date": null, "due_date": null}]});
        assert_eq!(milestone_gantt(&data), None);
    }

    #[test]
    fn gantt_ignores_an_unparseable_planned_date() {
        let data = json!({"milestone": {"name": "m", "planned_date": "soon"},
            "tasks": [{"id": "1", "title": "x", "status": "todo", "start_date": "2026-10-01", "due_date": "2026-10-02"}]});
        let chart = milestone_gantt(&data).unwrap();
        assert!(!chart.contains("milestone"), "{chart}");
    }

    #[test]
    fn effort_chart_accumulates_daily_hours() {
        let data = json!({"by_day": [
            {"date": "2026-10-05", "hours": 2.5, "entries": 1},
            {"date": "2026-10-06", "hours": 1.0, "entries": 1},
            {"date": "2026-10-08", "hours": 4.0, "entries": 2}
        ], "by_week": []});
        let chart = effort_cumulative(&data).unwrap();
        assert!(chart.starts_with("xychart-beta\n"), "{chart}");
        assert!(
            chart.contains("x-axis [\"2026-10-05\", \"2026-10-06\", \"2026-10-08\"]"),
            "{chart}"
        );
        assert!(chart.contains("line [2.5, 3.5, 7.5]"), "{chart}");
        assert!(
            chart.contains("y-axis \"Cumulative hours\" 0 --> 8"),
            "{chart}"
        );
    }

    #[test]
    fn effort_chart_falls_back_to_weeks_for_long_logs() {
        let days: Vec<Value> = (0..40)
            .map(|i| {
                let d = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap() + chrono::Duration::days(i);
                json!({"date": d.to_string(), "hours": 1.0, "entries": 1})
            })
            .collect();
        let data = json!({"by_day": days, "by_week": [
            {"week": "2026-W01", "hours": 4.0, "entries": 4},
            {"week": "2026-W02", "hours": 7.0, "entries": 7}
        ]});
        let chart = effort_cumulative(&data).unwrap();
        assert!(
            chart.contains("x-axis [\"2026-W01\", \"2026-W02\"]"),
            "{chart}"
        );
        assert!(chart.contains("line [4, 11]"), "{chart}");
    }

    #[test]
    fn effort_chart_is_none_without_time_logged() {
        assert_eq!(
            effort_cumulative(&json!({"by_day": [], "by_week": []})),
            None
        );
    }

    #[test]
    fn effort_chart_keeps_negative_corrections_inside_the_axis() {
        let data =
            json!({"by_day": [{"date": "2026-10-05", "hours": -2.0, "entries": 1}], "by_week": []});
        let chart = effort_cumulative(&data).unwrap();
        assert!(
            chart.contains("y-axis \"Cumulative hours\" -2 --> 0"),
            "{chart}"
        );
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
