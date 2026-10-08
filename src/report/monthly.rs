//! Monthly report data (R4: FR-531 / SPEC-531).
//!
//! A monthly report is the weekly roll-up of a calendar month plus the trend
//! of the daily metrics snapshots (`.handoff/metrics_snapshots/`) inside it.
//! [`build_monthly_data`] is a pure function from already-loaded project
//! state to the JSON `monthly.md.hbs` renders as `data`;
//! [`collect_monthly_data`] loads that state from a `.handoff/` directory.
//! The verification-progress block comes from the trace graph and is attached
//! by the caller (`handoff_report`), exactly as for the weekly report.
//!
//! Output shape (hours rounded to 2 decimals; absent values are `null`):
//!
//! ```text
//! period      { start, end }
//! stats       { completed_in_period, completed_total, tasks_total, hours_in_period,
//!               estimate_hours_total, actual_hours_total, consumption_percent }
//! weeks       [ { week, start, end, completed, hours, entries } ]   clipped to the period
//! completed / in_progress / blockers / milestones / time_log        as in the weekly report
//! trend       { points: [ { date, total, done, completion_percent, actual_hours,
//!                           remaining_hours, overdue_count } ],
//!               delta: { completion_percent, done, actual_hours, remaining_hours } | null }
//! warnings    [ string ]
//! ```
//!
//! `delta` is last point minus first point and is `null` with fewer than two
//! points. A task completed in a week is counted in that week's row; the
//! month-level `completed` list is deduplicated across the whole period.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Result};
use chrono::{Datelike, NaiveDate};
use serde_json::{json, Value};

use super::period::Period;
use super::weekly::{self, round2, ts_date, WeeklyInputs};
use super::{ReportScope, ReportType};
use crate::storage::config::MilestoneConfig;
use crate::storage::events::{read_events, EventFilters, EventRecord, EVENT_TASK_STATUS_CHANGED};
use crate::storage::metrics_snapshots::read_snapshots;
use crate::storage::tasks::{collect_all_tasks, TaskData};
use crate::storage::time_log::{read_time_log, TimeLogEntry};

/// Everything [`build_monthly_data`] derives the report from.
pub struct MonthlyInputs<'a> {
    pub period: Period,
    /// Every task with its current status.
    pub tasks: &'a [(TaskData, String)],
    pub time_log: &'a [TimeLogEntry],
    /// `task.status_changed` events (other events are ignored).
    pub events: &'a [EventRecord],
    pub milestones: &'a HashMap<String, MilestoneConfig>,
    /// Metrics snapshot envelopes (any date; those outside the period are
    /// ignored), oldest first.
    pub snapshots: &'a [Value],
    /// Messages about snapshot files that could not be read.
    pub snapshot_warnings: &'a [String],
}

/// Fields a monthly report accepts besides `label`.
const SUPPORTED_SCOPE: &[&str] = &["period", "from", "to"];

/// A monthly report is about a period only; any other scope field would be
/// silently ignored, so it is an error instead.
pub fn validate_scope(scope: &ReportScope) -> Result<()> {
    scope.require_only(
        ReportType::Monthly,
        SUPPORTED_SCOPE,
        "use scope.period / scope.from / scope.to",
    )
}

/// The period a monthly report covers: `scope.period` as a calendar month
/// (`2026-10`), an ISO week or a date range, else `scope.from` + `scope.to`,
/// else the calendar month containing `today`. `period` together with
/// `from`/`to`, or only one of `from`/`to`, is an error rather than a silent
/// pick.
pub fn resolve_period(scope: &ReportScope, today: NaiveDate) -> Result<Period> {
    if let Some(month) = scope.period.as_deref().and_then(Period::parse_month) {
        if scope.from.is_some() || scope.to.is_some() {
            bail!("scope.period cannot be combined with scope.from/scope.to");
        }
        return Ok(month);
    }
    if scope.period.is_none() && scope.from.is_none() && scope.to.is_none() {
        return Ok(Period::month_of(today));
    }
    weekly::resolve_period(scope, today)
}

/// Loads the project state under `handoff_dir` and builds the monthly data.
pub fn collect_monthly_data(handoff_dir: &Path, period: Period) -> Result<Value> {
    let mut tasks = Vec::new();
    collect_all_tasks(&handoff_dir.join("tasks"), &mut tasks)?;
    let time_log = read_time_log(handoff_dir)?;
    let events = read_events(
        handoff_dir,
        &EventFilters {
            event_type: Some(EVENT_TASK_STATUS_CHANGED.to_string()),
            limit: Some(usize::MAX),
            ..Default::default()
        },
    )?;
    // An unreadable config.toml is an error, not a silent loss of milestone
    // dates (same policy as the weekly report).
    let milestones =
        crate::storage::config::read_config(&handoff_dir.join("config.toml"))?.milestones;
    let snapshots = read_snapshots(handoff_dir)?;
    Ok(build_monthly_data(&MonthlyInputs {
        period,
        tasks: &tasks,
        time_log: &time_log,
        events: &events,
        milestones: &milestones,
        snapshots: &snapshots.snapshots,
        snapshot_warnings: &snapshots.warnings,
    }))
}

pub fn build_monthly_data(inputs: &MonthlyInputs) -> Value {
    let weekly_for = |period: Period| {
        weekly::build_weekly_data(&WeeklyInputs {
            period,
            tasks: inputs.tasks,
            time_log: inputs.time_log,
            events: inputs.events,
            milestones: inputs.milestones,
        })
    };
    let whole = weekly_for(inputs.period);

    let mut weeks = Vec::new();
    let mut week = Period::iso_week_of(inputs.period.start);
    while week.start <= inputs.period.end {
        let clipped = Period {
            start: week.start.max(inputs.period.start),
            end: week.end.min(inputs.period.end),
        };
        let data = weekly_for(clipped);
        let iso = week.start.iso_week();
        weeks.push(json!({
            "week": format!("{}-W{:02}", iso.year(), iso.week()),
            "start": clipped.start.to_string(),
            "end": clipped.end.to_string(),
            "completed": data["stats"]["completed_this_week"],
            "hours": data["stats"]["hours_this_week"],
            "entries": data["time_log"]["entries"],
        }));
        week = week.next();
    }

    let stats = &whole["stats"];
    let mut warnings: Vec<Value> = whole["warnings"].as_array().cloned().unwrap_or_default();
    warnings.extend(inputs.snapshot_warnings.iter().map(|w| json!(w)));

    json!({
        "period": whole["period"].clone(),
        "stats": {
            "completed_in_period": stats["completed_this_week"],
            "completed_total": stats["completed_total"],
            "tasks_total": stats["tasks_total"],
            "hours_in_period": stats["hours_this_week"],
            "estimate_hours_total": stats["estimate_hours_total"],
            "actual_hours_total": stats["actual_hours_total"],
            "consumption_percent": stats["consumption_percent"],
        },
        "weeks": weeks,
        "completed": whole["completed"],
        "in_progress": whole["in_progress"],
        "blockers": whole["blockers"],
        "time_log": whole["time_log"],
        "milestones": whole["milestones"],
        "trend": trend(inputs.period, inputs.snapshots),
        "warnings": warnings,
    })
}

/// Snapshot points inside `period` and the change from the first to the last.
fn trend(period: Period, snapshots: &[Value]) -> Value {
    let points: Vec<Value> = snapshots
        .iter()
        .filter(|s| {
            s["date"]
                .as_str()
                .and_then(ts_date)
                .is_some_and(|d| period.contains(d))
        })
        .map(|s| {
            let m = &s["metrics"];
            // `by_status` omits statuses nobody is in, so a missing `done`
            // key means zero tasks are done; a missing `by_status` map means
            // the snapshot does not say, hence null.
            let done = m["by_status"]
                .as_object()
                .map(|by| by.get("done").and_then(Value::as_u64).unwrap_or(0));
            json!({
                "date": s["date"],
                "total": m["total"],
                "done": done,
                "completion_percent": m["completion_percent"],
                "actual_hours": m["total_actual_hours"],
                "remaining_hours": m["total_remaining_hours"],
                "overdue_count": m["overdue_count"],
            })
        })
        .collect();

    let delta = match (points.first(), points.last()) {
        (Some(first), Some(last)) if points.len() >= 2 => {
            let diff = |key: &str, round: fn(f64) -> f64| -> Value {
                match (first[key].as_f64(), last[key].as_f64()) {
                    (Some(a), Some(b)) => json!(round(b - a)),
                    _ => Value::Null,
                }
            };
            json!({
                "completion_percent": diff("completion_percent", round1),
                "done": diff("done", round2),
                "actual_hours": diff("actual_hours", round2),
                "remaining_hours": diff("remaining_hours", round2),
            })
        }
        _ => Value::Null,
    };
    json!({ "points": points, "delta": delta })
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0 + 0.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::period::parse_date;

    fn d(s: &str) -> NaiveDate {
        parse_date(s).unwrap()
    }

    fn scope(period: Option<&str>, from: Option<&str>, to: Option<&str>) -> ReportScope {
        ReportScope {
            period: period.map(str::to_string),
            from: from.map(str::to_string),
            to: to.map(str::to_string),
            ..Default::default()
        }
    }

    fn snap(date: &str, done: u64, total: u64) -> Value {
        json!({
            "date": date,
            "metrics": {
                "total": total,
                "by_status": { "done": done },
                "completion_percent": done as f64 * 100.0 / total as f64,
                "total_actual_hours": done as f64,
                "total_remaining_hours": (total - done) as f64,
                "overdue_count": 0,
            },
        })
    }

    #[test]
    fn period_resolution() {
        let today = d("2026-10-08");
        let p = resolve_period(&scope(Some("2026-02"), None, None), today).unwrap();
        assert_eq!((p.start, p.end), (d("2026-02-01"), d("2026-02-28")));
        // Without a period: the month containing today.
        let p = resolve_period(&scope(None, None, None), today).unwrap();
        assert_eq!((p.start, p.end), (d("2026-10-01"), d("2026-10-31")));
        // Weekly forms still work.
        let p = resolve_period(&scope(Some("2026-W41"), None, None), today).unwrap();
        assert_eq!(p.start, d("2026-10-05"));
        let p =
            resolve_period(&scope(None, Some("2026-10-05"), Some("2026-11-04")), today).unwrap();
        assert_eq!(p.end, d("2026-11-04"));
        for bad in [
            scope(Some("2026-13"), None, None),
            scope(Some("2026-10"), Some("2026-10-01"), None),
            scope(Some("2026-10"), None, Some("2026-10-31")),
            scope(None, Some("2026-10-01"), None),
        ] {
            assert!(resolve_period(&bad, today).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn scope_accepts_only_period_fields() {
        assert!(validate_scope(&scope(Some("2026-10"), None, None)).is_ok());
        let with_label = ReportScope {
            label: Some("Oct".into()),
            ..Default::default()
        };
        assert!(validate_scope(&with_label).is_ok());
        for bad in [
            ReportScope {
                layers: vec!["unit".into()],
                ..Default::default()
            },
            ReportScope {
                milestone: Some("beta".into()),
                ..Default::default()
            },
            ReportScope {
                assignee: Some("a".into()),
                ..Default::default()
            },
        ] {
            let err = validate_scope(&bad).unwrap_err().to_string();
            assert!(err.contains("monthly"), "{err}");
        }
    }

    fn data_for(period: &str, snapshots: &[Value]) -> Value {
        let milestones = HashMap::new();
        build_monthly_data(&MonthlyInputs {
            period: Period::parse_month(period).unwrap(),
            tasks: &[],
            time_log: &[],
            events: &[],
            milestones: &milestones,
            snapshots,
            snapshot_warnings: &["snapshot x.json is unreadable".to_string()],
        })
    }

    #[test]
    fn snapshots_outside_the_month_are_ignored_and_delta_is_last_minus_first() {
        let data = data_for(
            "2026-10",
            &[
                snap("2026-09-30", 0, 4),
                snap("2026-10-01", 1, 4),
                snap("2026-10-15", 2, 4),
                snap("2026-10-31", 3, 4),
                snap("2026-11-01", 4, 4),
            ],
        );
        let points = data["trend"]["points"].as_array().unwrap();
        let dates: Vec<&str> = points.iter().map(|p| p["date"].as_str().unwrap()).collect();
        assert_eq!(dates, ["2026-10-01", "2026-10-15", "2026-10-31"]);
        assert_eq!(points[0]["done"], 1);
        let delta = &data["trend"]["delta"];
        assert_eq!(delta["completion_percent"], 50.0);
        assert_eq!(delta["done"], 2.0);
        assert_eq!(delta["remaining_hours"], -2.0);
        assert_eq!(data["warnings"][0], "snapshot x.json is unreadable");
    }

    #[test]
    fn delta_needs_two_points() {
        let one = data_for("2026-10", &[snap("2026-10-01", 1, 4)]);
        assert!(one["trend"]["delta"].is_null());
        assert_eq!(one["trend"]["points"].as_array().unwrap().len(), 1);
        let none = data_for("2026-10", &[]);
        assert!(none["trend"]["delta"].is_null());
        assert!(none["trend"]["points"].as_array().unwrap().is_empty());
    }

    #[test]
    fn a_snapshot_without_by_status_has_unknown_done_count() {
        let s = json!({"date": "2026-10-01", "metrics": {"total": 2}});
        let data = data_for("2026-10", &[s]);
        assert!(data["trend"]["points"][0]["done"].is_null());
    }

    #[test]
    fn weeks_are_the_iso_weeks_clipped_to_the_period() {
        let data = data_for("2026-10", &[]);
        let weeks = data["weeks"].as_array().unwrap();
        let labels: Vec<&str> = weeks.iter().map(|w| w["week"].as_str().unwrap()).collect();
        assert_eq!(
            labels,
            ["2026-W40", "2026-W41", "2026-W42", "2026-W43", "2026-W44"]
        );
        assert_eq!(weeks[0]["start"], "2026-10-01");
        assert_eq!(weeks[0]["end"], "2026-10-04");
        assert_eq!(weeks[4]["start"], "2026-10-26");
        assert_eq!(weeks[4]["end"], "2026-10-31");
        assert_eq!(data["period"]["start"], "2026-10-01");
    }

    #[test]
    fn week_labels_use_the_iso_year_across_new_year() {
        let data = data_for("2027-01", &[]);
        assert_eq!(data["weeks"][0]["week"], "2026-W53");
    }
}
