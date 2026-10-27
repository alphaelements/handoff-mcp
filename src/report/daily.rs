//! Daily report data (REQ-PDF-036).
//!
//! A daily report is the weekly report narrowed to one calendar day plus the
//! day's metrics snapshot and its change since the previous snapshot, and the
//! list of task status changes made that day. [`build_daily_data`] is a pure
//! function from already-loaded project state to the JSON `daily.md.hbs`
//! renders as `data`; [`collect_daily_data`] loads that state from a
//! `.handoff/` directory. The verification-progress block is attached by the
//! caller (`handoff_report`), as for the weekly report.
//!
//! Output shape (hours rounded to 2 decimals; absent values are `null`):
//!
//! ```text
//! period         { start, end }                      both the report day
//! stats          { completed_in_period, completed_total, tasks_total, hours_in_period,
//!                  estimate_hours_total, actual_hours_total, consumption_percent }
//! completed / in_progress / blockers / time_log / milestones   as in the weekly report
//! status_changes [ { ts, task_id, title, from, to } ]           oldest first
//! metrics        { current, previous, delta }
//! warnings       [ string ]
//! ```
//!
//! `metrics.current` is the snapshot dated the report day and
//! `metrics.previous` the latest snapshot dated before it (not necessarily
//! the day before: a day without a snapshot is skipped over), each in the
//! trend-point shape of the monthly report. `delta` is `current - previous`
//! and is `null` unless both exist.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Result};
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{json, Value};

use super::monthly::{point_delta, snapshot_point};
use super::period::Period;
use super::weekly::{self, ts_date, WeeklyInputs};
use super::{ReportScope, ReportType};
use crate::storage::config::MilestoneConfig;
use crate::storage::events::{read_events, EventFilters, EventRecord, EVENT_TASK_STATUS_CHANGED};
use crate::storage::metrics_snapshots::read_snapshots;
use crate::storage::tasks::{collect_all_tasks, TaskData};
use crate::storage::time_log::{read_time_log, TimeLogEntry};

/// Everything [`build_daily_data`] derives the report from.
pub struct DailyInputs<'a> {
    /// The report day.
    pub date: NaiveDate,
    /// Every task with its current status.
    pub tasks: &'a [(TaskData, String)],
    pub time_log: &'a [TimeLogEntry],
    /// `task.status_changed` events (other events are ignored).
    pub events: &'a [EventRecord],
    pub milestones: &'a HashMap<String, MilestoneConfig>,
    /// Metrics snapshot envelopes (any date), oldest first.
    pub snapshots: &'a [Value],
    /// Messages about snapshot files that could not be read.
    pub snapshot_warnings: &'a [String],
}

/// Fields a daily report accepts besides `label`.
const SUPPORTED_SCOPE: &[&str] = &["period", "from", "to"];

/// A daily report is about a period only; any other scope field would be
/// silently ignored, so it is an error instead.
pub fn validate_scope(scope: &ReportScope) -> Result<()> {
    scope.require_only(
        ReportType::Daily,
        SUPPORTED_SCOPE,
        "use scope.period / scope.from / scope.to",
    )
}

/// The day a daily report covers: `scope.period` (`2026-10-08`), else
/// `scope.from` + `scope.to`, else `today`. The resolved period must be a
/// single day; a wider one is an error rather than a silent pick.
pub fn resolve_date(scope: &ReportScope, today: NaiveDate) -> Result<NaiveDate> {
    let period = weekly::resolve_scope_period(scope, Period::day(today))?;
    if period.days() != 1 {
        bail!(
            "A daily report covers exactly one day, but the period is {} to {} ({} days); use a day like 2026-10-08",
            period.start,
            period.end,
            period.days()
        );
    }
    Ok(period.start)
}

/// Loads the project state under `handoff_dir` and builds the daily data.
pub fn collect_daily_data(handoff_dir: &Path, date: NaiveDate) -> Result<Value> {
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
    Ok(build_daily_data(&DailyInputs {
        date,
        tasks: &tasks,
        time_log: &time_log,
        events: &events,
        milestones: &milestones,
        snapshots: &snapshots.snapshots,
        snapshot_warnings: &snapshots.warnings,
    }))
}

pub fn build_daily_data(inputs: &DailyInputs) -> Value {
    let whole = weekly::build_weekly_data(&WeeklyInputs {
        period: Period::day(inputs.date),
        tasks: inputs.tasks,
        time_log: inputs.time_log,
        events: inputs.events,
        milestones: inputs.milestones,
    });
    let stats = &whole["stats"];

    let mut warnings: Vec<Value> = whole["warnings"].as_array().cloned().unwrap_or_default();
    let (status_changes, change_warnings) = status_changes(inputs);
    for warning in change_warnings {
        // The weekly pass reports some of the same malformed events.
        if !warnings.contains(&json!(warning)) {
            warnings.push(json!(warning));
        }
    }
    warnings.extend(inputs.snapshot_warnings.iter().map(|w| json!(w)));

    json!({
        "period": {
            "start": inputs.date.to_string(),
            "end": inputs.date.to_string(),
        },
        "stats": {
            "completed_in_period": stats["completed_this_week"],
            "completed_total": stats["completed_total"],
            "tasks_total": stats["tasks_total"],
            "hours_in_period": stats["hours_this_week"],
            "estimate_hours_total": stats["estimate_hours_total"],
            "actual_hours_total": stats["actual_hours_total"],
            "consumption_percent": stats["consumption_percent"],
        },
        "completed": whole["completed"],
        "in_progress": whole["in_progress"],
        "blockers": whole["blockers"],
        "time_log": whole["time_log"],
        "milestones": whole["milestones"],
        "status_changes": status_changes,
        "metrics": metrics(inputs.date, inputs.snapshots),
        "warnings": warnings,
    })
}

/// The `task.status_changed` events of the report day, oldest first, and a
/// note for every event of that day that is unusable (the periodic data
/// would otherwise silently miss a change).
fn status_changes(inputs: &DailyInputs) -> (Vec<Value>, Vec<String>) {
    let title_of = |id: &str| {
        inputs
            .tasks
            .iter()
            .find(|(t, _)| t.id == id)
            .map(|(t, _)| t.title.as_str())
    };
    let mut warnings = Vec::new();
    let mut rows: Vec<(DateTime<Utc>, Value)> = Vec::new();
    for event in inputs.events {
        if event.event != EVENT_TASK_STATUS_CHANGED {
            continue;
        }
        let Some(task_id) = event.task_id.as_deref() else {
            continue;
        };
        // An unparseable ts is already reported by the weekly pass.
        let Some(date) = ts_date(&event.ts) else {
            continue;
        };
        if date != inputs.date {
            continue;
        }
        let Some(detail) = event
            .detail
            .as_deref()
            .and_then(|d| serde_json::from_str::<Value>(d).ok())
        else {
            warnings.push(format!(
                "status_changed event for {task_id} at {} has a missing or unparseable detail and was skipped",
                event.ts
            ));
            continue;
        };
        // A bare `YYYY-MM-DD` ts has no time of day; it sorts at midnight.
        let at = DateTime::parse_from_rfc3339(&event.ts)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| date.and_time(chrono::NaiveTime::MIN).and_utc());
        rows.push((
            at,
            json!({
                "ts": event.ts,
                "task_id": task_id,
                "title": title_of(task_id),
                "from": detail.get("from").and_then(Value::as_str),
                "to": detail.get("to").and_then(Value::as_str),
            }),
        ));
    }
    rows.sort_by_key(|(at, _)| *at);
    (rows.into_iter().map(|(_, row)| row).collect(), warnings)
}

/// `{ current, previous, delta }`; see the module docs.
fn metrics(date: NaiveDate, snapshots: &[Value]) -> Value {
    let dated = |s: &&Value| s["date"].as_str().and_then(ts_date);
    let current = snapshots
        .iter()
        .rfind(|s| dated(s) == Some(date))
        .map(snapshot_point);
    let previous = snapshots
        .iter()
        .filter(|s| dated(s).is_some_and(|d| d < date))
        .max_by_key(|s| dated(s))
        .map(snapshot_point);
    let delta = match (&current, &previous) {
        (Some(current), Some(previous)) => point_delta(previous, current),
        _ => Value::Null,
    };
    json!({ "current": current, "previous": previous, "delta": delta })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::period::parse_date;
    use crate::storage::tasks::Schedule;

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

    fn event(ts: &str, task_id: &str, from: &str, to: &str) -> EventRecord {
        EventRecord {
            ts: ts.into(),
            event: EVENT_TASK_STATUS_CHANGED.into(),
            task_id: Some(task_id.into()),
            agent_id: None,
            session_id: None,
            detail: Some(json!({ "from": from, "to": to }).to_string()),
        }
    }

    fn log(ts: &str, task_id: &str, hours: f64) -> TimeLogEntry {
        TimeLogEntry {
            ts: ts.into(),
            task_id: task_id.into(),
            hours,
            agent_id: None,
            note: None,
            source: None,
        }
    }

    fn build(
        day: &str,
        tasks: &[(TaskData, String)],
        time_log: &[TimeLogEntry],
        events: &[EventRecord],
        snapshots: &[Value],
    ) -> Value {
        build_daily_data(&DailyInputs {
            date: d(day),
            tasks,
            time_log,
            events,
            milestones: &HashMap::new(),
            snapshots,
            snapshot_warnings: &["snapshot x.json is unreadable".to_string()],
        })
    }

    #[test]
    fn date_resolution() {
        let today = d("2026-10-08");
        assert_eq!(
            resolve_date(&scope(None, None, None), today).unwrap(),
            today
        );
        assert_eq!(
            resolve_date(&scope(Some("2026-10-05"), None, None), today).unwrap(),
            d("2026-10-05")
        );
        assert_eq!(
            resolve_date(&scope(None, Some("2026-10-05"), Some("2026-10-05")), today).unwrap(),
            d("2026-10-05")
        );
        assert_eq!(
            resolve_date(&scope(Some("2026-10-05..2026-10-05"), None, None), today).unwrap(),
            d("2026-10-05")
        );
        for bad in [
            scope(Some("2026-W41"), None, None),
            scope(Some("2026-10"), None, None),
            scope(Some("2026-Q4"), None, None),
            scope(Some("2026"), None, None),
            scope(Some("2026-10-05..2026-10-06"), None, None),
            scope(Some("2026-10-05"), Some("2026-10-05"), None),
            scope(None, Some("2026-10-05"), None),
            scope(Some("yesterday"), None, None),
        ] {
            assert!(resolve_date(&bad, today).is_err(), "{bad:?}");
        }
        let err = resolve_date(&scope(Some("2026-W41"), None, None), today)
            .unwrap_err()
            .to_string();
        assert!(err.contains("exactly one day"), "{err}");
    }

    #[test]
    fn scope_accepts_only_period_fields() {
        assert!(validate_scope(&scope(Some("2026-10-08"), None, None)).is_ok());
        let bad = ReportScope {
            milestone: Some("beta".into()),
            ..Default::default()
        };
        assert!(validate_scope(&bad)
            .unwrap_err()
            .to_string()
            .contains("daily"));
    }

    #[test]
    fn only_the_report_day_counts() {
        let tasks = [
            task("t1", "done", |t| {
                t.completed_at = Some("2026-10-08T09:00:00+00:00".into());
            }),
            task("t2", "done", |t| {
                t.completed_at = Some("2026-10-07T09:00:00+00:00".into());
            }),
            task("t3", "in_progress", |t| {
                t.schedule = Some(Schedule {
                    estimate_hours: Some(8.0),
                    ..Default::default()
                });
            }),
        ];
        let data = build(
            "2026-10-08",
            &tasks,
            &[
                log("2026-10-08T10:00:00+00:00", "t3", 2.5),
                log("2026-10-07T10:00:00+00:00", "t3", 40.0),
            ],
            &[],
            &[],
        );
        assert_eq!(data["period"]["start"], "2026-10-08");
        assert_eq!(data["period"]["end"], "2026-10-08");
        assert_eq!(data["stats"]["completed_in_period"], 1);
        assert_eq!(data["stats"]["completed_total"], 2);
        assert_eq!(data["stats"]["hours_in_period"], 2.5);
        assert_eq!(data["completed"][0]["id"], "t1");
        assert_eq!(data["in_progress"][0]["id"], "t3");
        assert_eq!(data["time_log"]["by_task"][0]["hours"], 2.5);
    }

    #[test]
    fn status_changes_of_the_day_are_listed_oldest_first() {
        let tasks = [task("t1", "done", |_| {}), task("t2", "review", |_| {})];
        let data = build(
            "2026-10-08",
            &tasks,
            &[],
            &[
                event("2026-10-08T15:00:00+00:00", "t1", "review", "done"),
                event("2026-10-07T23:59:59+00:00", "t1", "todo", "in_progress"),
                event("2026-10-08T09:00:00+00:00", "t2", "in_progress", "review"),
                // 2026-10-09T01:00 in +09:00 is 16:00 on the 8th in UTC: the
                // day is judged in UTC and the order is by instant.
                event("2026-10-09T01:00:00+09:00", "t2", "review", "blocked"),
                event("2026-10-09T00:00:00+00:00", "t2", "blocked", "todo"),
            ],
            &[],
        );
        let rows = data["status_changes"].as_array().unwrap();
        let seq: Vec<(&str, &str, &str)> = rows
            .iter()
            .map(|r| {
                (
                    r["task_id"].as_str().unwrap(),
                    r["from"].as_str().unwrap(),
                    r["to"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            seq,
            [
                ("t2", "in_progress", "review"),
                ("t1", "review", "done"),
                ("t2", "review", "blocked"),
            ]
        );
        assert_eq!(rows[0]["title"], "Title t2");
    }

    #[test]
    fn a_status_change_of_an_unknown_task_has_a_null_title() {
        let data = build(
            "2026-10-08",
            &[],
            &[],
            &[event("2026-10-08T09:00:00+00:00", "gone", "todo", "done")],
            &[],
        );
        assert!(data["status_changes"][0]["title"].is_null());
    }

    #[test]
    fn a_malformed_event_detail_is_warned_about_once() {
        let mut bad = event("2026-10-08T09:00:00+00:00", "t1", "a", "b");
        bad.detail = Some("not json".into());
        let tasks = [task("t1", "todo", |_| {})];
        let data = build("2026-10-08", &tasks, &[], &[bad], &[]);
        assert!(data["status_changes"].as_array().unwrap().is_empty());
        let warnings: Vec<&str> = data["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|w| w.as_str())
            .collect();
        let hits = warnings
            .iter()
            .filter(|w| w.contains("unparseable detail"))
            .count();
        assert_eq!(hits, 1, "{warnings:?}");
        assert!(warnings.contains(&"snapshot x.json is unreadable"));
    }

    #[test]
    fn metrics_compare_with_the_latest_earlier_snapshot() {
        let snaps = [
            snap("2026-10-03", 1, 4),
            snap("2026-10-06", 2, 4),
            snap("2026-10-08", 3, 4),
            snap("2026-10-09", 4, 4),
        ];
        let data = build("2026-10-08", &[], &[], &[], &snaps);
        let m = &data["metrics"];
        assert_eq!(m["current"]["date"], "2026-10-08");
        assert_eq!(m["current"]["done"], 3);
        // 10-07 has no snapshot, so the 6th is the baseline.
        assert_eq!(m["previous"]["date"], "2026-10-06");
        assert_eq!(m["delta"]["done"], 1.0);
        assert_eq!(m["delta"]["completion_percent"], 25.0);
        assert_eq!(m["delta"]["remaining_hours"], -1.0);
    }

    #[test]
    fn metrics_without_both_snapshots_have_no_delta() {
        let only_current = build("2026-10-08", &[], &[], &[], &[snap("2026-10-08", 3, 4)]);
        assert_eq!(only_current["metrics"]["current"]["done"], 3);
        assert!(only_current["metrics"]["previous"].is_null());
        assert!(only_current["metrics"]["delta"].is_null());

        let only_previous = build("2026-10-08", &[], &[], &[], &[snap("2026-10-07", 3, 4)]);
        assert!(only_previous["metrics"]["current"].is_null());
        assert_eq!(only_previous["metrics"]["previous"]["done"], 3);
        assert!(only_previous["metrics"]["delta"].is_null());

        let none = build("2026-10-08", &[], &[], &[], &[]);
        assert!(none["metrics"]["current"].is_null());
        assert!(none["metrics"]["delta"].is_null());
    }

    #[test]
    fn an_empty_project_yields_empty_sections_not_errors() {
        let data = build("2026-10-08", &[], &[], &[], &[]);
        assert_eq!(data["stats"]["tasks_total"], 0);
        assert!(data["completed"].as_array().unwrap().is_empty());
        assert!(data["status_changes"].as_array().unwrap().is_empty());
    }
}
