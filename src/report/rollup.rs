//! Quarterly and yearly report data (REQ-PDF-037 / REQ-PDF-038).
//!
//! A quarterly report is the monthly report widened to three calendar months;
//! a yearly report to twelve, with the quarters as a second level. Both are
//! the weekly roll-up of the whole period plus one row per calendar month
//! (and, for a yearly report, per calendar quarter) with the metrics snapshot
//! the unit ended on. [`build_rollup_data`] is a pure function from
//! already-loaded project state to the JSON `quarterly.md.hbs` /
//! `yearly.md.hbs` render as `data`; [`collect_rollup_data`] loads that state
//! from a `.handoff/` directory. The verification-progress block is attached
//! by the caller (`handoff_report`), as for the weekly report.
//!
//! Output shape (hours rounded to 2 decimals; absent values are `null`):
//!
//! ```text
//! period      { start, end }
//! stats       { completed_in_period, completed_total, tasks_total, hours_in_period,
//!               estimate_hours_total, actual_hours_total, consumption_percent }
//! months      [ { label (YYYY-MM), start, end, completed, hours, entries, snapshot } ]
//! quarters    [ { label (YYYY-Qn), ... as months } ]      yearly reports only
//! completed / in_progress / blockers / milestones / time_log   as in the weekly report
//! trend       { delta: { completion_percent, done, actual_hours, remaining_hours } | null }
//! warnings    [ string ]
//! ```
//!
//! Rows are clipped to the period, so a period that starts or ends mid-month
//! has a short first/last row. `snapshot` is the latest metrics snapshot
//! inside the row (a trend point as in the monthly report) or `null`. `trend`
//! has no daily points (a year would be 365 rows): `delta` is the last
//! snapshot of the period minus the first, `null` with fewer than two.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Result};
use chrono::{Datelike, Duration, NaiveDate};
use serde_json::{json, Value};

use super::monthly::{snapshot_point, trend};
use super::period::{quarter_label, Period};
use super::weekly::{self, ts_date, WeeklyInputs};
use super::{ReportScope, ReportType};
use crate::storage::config::MilestoneConfig;
use crate::storage::events::{read_events, EventFilters, EventRecord, EVENT_TASK_STATUS_CHANGED};
use crate::storage::metrics_snapshots::read_snapshots;
use crate::storage::tasks::{collect_all_tasks, TaskData};
use crate::storage::time_log::{read_time_log, TimeLogEntry};

/// Everything [`build_rollup_data`] derives the report from.
pub struct RollupInputs<'a> {
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

/// Fields a quarterly or yearly report accepts besides `label`.
const SUPPORTED_SCOPE: &[&str] = &["period", "from", "to"];

/// The roll-up report types; anything else is a programming error.
fn require_rollup(report_type: ReportType) {
    assert!(
        matches!(report_type, ReportType::Quarterly | ReportType::Yearly),
        "{} is not a roll-up report",
        report_type.as_str()
    );
}

/// A roll-up report is about a period only; any other scope field would be
/// silently ignored, so it is an error instead.
pub fn validate_scope(report_type: ReportType, scope: &ReportScope) -> Result<()> {
    require_rollup(report_type);
    scope.require_only(
        report_type,
        SUPPORTED_SCOPE,
        "use scope.period / scope.from / scope.to",
    )
}

/// The period a quarterly or yearly report covers: `scope.period` (`2026-Q4`
/// / `2026`, or any other form [`Period::parse`] reads), else `scope.from` +
/// `scope.to`, else the calendar quarter / year containing `today`. `period`
/// together with `from`/`to`, or only one of `from`/`to`, is an error.
pub fn resolve_period(
    report_type: ReportType,
    scope: &ReportScope,
    today: NaiveDate,
) -> Result<Period> {
    let default = match report_type {
        ReportType::Quarterly => Period::quarter_of(today),
        ReportType::Yearly => Period::year_of(today),
        other => bail!("{} is not a roll-up report", other.as_str()),
    };
    weekly::resolve_scope_period(scope, default)
}

/// Loads the project state under `handoff_dir` and builds the roll-up data.
pub fn collect_rollup_data(
    handoff_dir: &Path,
    report_type: ReportType,
    period: Period,
) -> Result<Value> {
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
    Ok(build_rollup_data(
        report_type,
        &RollupInputs {
            period,
            tasks: &tasks,
            time_log: &time_log,
            events: &events,
            milestones: &milestones,
            snapshots: &snapshots.snapshots,
            snapshot_warnings: &snapshots.warnings,
        },
    ))
}

pub fn build_rollup_data(report_type: ReportType, inputs: &RollupInputs) -> Value {
    require_rollup(report_type);
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
    let stats = &whole["stats"];

    let rows = |unit_of: fn(NaiveDate) -> Period, label_of: fn(NaiveDate) -> String| {
        unit_rows(inputs, &weekly_for, unit_of, label_of)
    };
    let months = rows(Period::month_of, |d| {
        format!("{}-{:02}", d.year(), d.month())
    });

    let mut warnings: Vec<Value> = whole["warnings"].as_array().cloned().unwrap_or_default();
    warnings.extend(inputs.snapshot_warnings.iter().map(|w| json!(w)));

    let mut data = json!({
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
        "months": months,
        "completed": whole["completed"],
        "in_progress": whole["in_progress"],
        "blockers": whole["blockers"],
        "time_log": whole["time_log"],
        "milestones": whole["milestones"],
        "trend": { "delta": trend(inputs.period, inputs.snapshots)["delta"].clone() },
        "warnings": warnings,
    });
    if report_type == ReportType::Yearly {
        data["quarters"] = json!(rows(Period::quarter_of, quarter_label));
    }
    data
}

/// One row per calendar unit (`unit_of` maps a date to the unit containing
/// it) overlapping the period, clipped to it.
fn unit_rows(
    inputs: &RollupInputs,
    weekly_for: &dyn Fn(Period) -> Value,
    unit_of: fn(NaiveDate) -> Period,
    label_of: fn(NaiveDate) -> String,
) -> Vec<Value> {
    let mut rows = Vec::new();
    let mut cursor = inputs.period.start;
    while cursor <= inputs.period.end {
        let unit = unit_of(cursor);
        let clipped = Period {
            start: unit.start.max(inputs.period.start),
            end: unit.end.min(inputs.period.end),
        };
        let data = weekly_for(clipped);
        rows.push(json!({
            "label": label_of(cursor),
            "start": clipped.start.to_string(),
            "end": clipped.end.to_string(),
            "completed": data["stats"]["completed_this_week"],
            "hours": data["stats"]["hours_this_week"],
            "entries": data["time_log"]["entries"],
            "snapshot": last_snapshot_in(clipped, inputs.snapshots),
        }));
        cursor = unit.end + Duration::days(1);
    }
    rows
}

/// The latest snapshot dated inside `period` as a trend point, else `null`.
fn last_snapshot_in(period: Period, snapshots: &[Value]) -> Value {
    snapshots
        .iter()
        .filter_map(|s| Some((s["date"].as_str().and_then(ts_date)?, s)))
        .filter(|(date, _)| period.contains(*date))
        .max_by_key(|(date, _)| *date)
        .map_or(Value::Null, |(_, s)| snapshot_point(s))
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

    fn done_task(id: &str, completed_at: &str) -> (TaskData, String) {
        let data = TaskData {
            id: id.into(),
            title: format!("Title {id}"),
            notes: None,
            priority: None,
            created_at: None,
            updated_at: None,
            completed_at: Some(completed_at.into()),
            labels: vec![],
            links: vec![],
            task_links: vec![],
            done_criteria: vec![],
            schedule: Some(Schedule::default()),
            dependencies: vec![],
            order: None,
            assignee: None,
            lock: None,
            scope_paths: vec![],
            extra: HashMap::new(),
        };
        (data, "done".into())
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
        report_type: ReportType,
        period: &str,
        tasks: &[(TaskData, String)],
        time_log: &[TimeLogEntry],
        snapshots: &[Value],
    ) -> Value {
        build_rollup_data(
            report_type,
            &RollupInputs {
                period: Period::parse(period).unwrap(),
                tasks,
                time_log,
                events: &[],
                milestones: &HashMap::new(),
                snapshots,
                snapshot_warnings: &["snapshot x.json is unreadable".to_string()],
            },
        )
    }

    fn labels(rows: &Value) -> Vec<&str> {
        rows.as_array()
            .unwrap()
            .iter()
            .map(|r| r["label"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn period_resolution() {
        let today = d("2026-11-15");
        let q = |s: &ReportScope| resolve_period(ReportType::Quarterly, s, today).unwrap();
        let y = |s: &ReportScope| resolve_period(ReportType::Yearly, s, today).unwrap();
        let none = scope(None, None, None);
        assert_eq!(
            (q(&none).start, q(&none).end),
            (d("2026-10-01"), d("2026-12-31"))
        );
        assert_eq!(
            (y(&none).start, y(&none).end),
            (d("2026-01-01"), d("2026-12-31"))
        );
        let p = q(&scope(Some("2026-Q1"), None, None));
        assert_eq!((p.start, p.end), (d("2026-01-01"), d("2026-03-31")));
        let p = y(&scope(Some("2025"), None, None));
        assert_eq!((p.start, p.end), (d("2025-01-01"), d("2025-12-31")));
        let p = q(&scope(None, Some("2026-10-15"), Some("2026-11-10")));
        assert_eq!((p.start, p.end), (d("2026-10-15"), d("2026-11-10")));
        for bad in [
            scope(Some("2026-Q5"), None, None),
            scope(Some("2026-Q4"), Some("2026-10-01"), None),
            scope(None, Some("2026-10-01"), None),
            scope(None, None, Some("2026-10-01")),
        ] {
            assert!(resolve_period(ReportType::Quarterly, &bad, today).is_err());
            assert!(resolve_period(ReportType::Yearly, &bad, today).is_err());
        }
        assert!(resolve_period(ReportType::Weekly, &none, today).is_err());
    }

    #[test]
    fn scope_accepts_only_period_fields() {
        for t in [ReportType::Quarterly, ReportType::Yearly] {
            assert!(validate_scope(t, &scope(Some("2026"), None, None)).is_ok());
            let bad = ReportScope {
                layers: vec!["unit".into()],
                ..Default::default()
            };
            let err = validate_scope(t, &bad).unwrap_err().to_string();
            assert!(err.contains(t.as_str()), "{err}");
        }
    }

    #[test]
    fn a_quarter_has_one_row_per_month() {
        let tasks = [
            done_task("t1", "2026-10-07T09:00:00+00:00"),
            done_task("t2", "2026-12-31T23:00:00+00:00"),
            done_task("t3", "2026-09-30T09:00:00+00:00"),
            done_task("t4", "2027-01-01T00:00:00+00:00"),
        ];
        let log = [
            log("2026-10-06T10:00:00+00:00", "t1", 3.0),
            log("2026-11-06T10:00:00+00:00", "t1", 1.5),
            log("2026-09-30T10:00:00+00:00", "t3", 40.0),
        ];
        let data = build(ReportType::Quarterly, "2026-Q4", &tasks, &log, &[]);
        assert_eq!(data["period"]["start"], "2026-10-01");
        assert_eq!(data["period"]["end"], "2026-12-31");
        assert_eq!(labels(&data["months"]), ["2026-10", "2026-11", "2026-12"]);
        let months = data["months"].as_array().unwrap();
        assert_eq!(months[0]["completed"], 1);
        assert_eq!(months[0]["hours"], 3.0);
        assert_eq!(months[0]["entries"], 1);
        assert_eq!(months[1]["completed"], 0);
        assert_eq!(months[1]["hours"], 1.5);
        assert_eq!(months[2]["completed"], 1);
        assert_eq!(months[2]["end"], "2026-12-31");
        assert_eq!(data["stats"]["completed_in_period"], 2);
        assert_eq!(data["stats"]["hours_in_period"], 4.5);
        assert_eq!(data["stats"]["completed_total"], 4);
        let done_ids: Vec<&str> = data["completed"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap())
            .collect();
        assert_eq!(done_ids, ["t1", "t2"]);
        assert!(data.get("quarters").is_none());
    }

    #[test]
    fn a_year_has_twelve_months_and_four_quarters() {
        let tasks = [
            done_task("t1", "2026-02-10T09:00:00+00:00"),
            done_task("t2", "2026-05-10T09:00:00+00:00"),
            done_task("t3", "2026-05-20T09:00:00+00:00"),
            done_task("t4", "2025-12-31T09:00:00+00:00"),
        ];
        let data = build(ReportType::Yearly, "2026", &tasks, &[], &[]);
        assert_eq!(data["months"].as_array().unwrap().len(), 12);
        assert_eq!(data["months"][0]["label"], "2026-01");
        assert_eq!(data["months"][11]["label"], "2026-12");
        assert_eq!(data["months"][1]["completed"], 1);
        assert_eq!(data["months"][4]["completed"], 2);
        assert_eq!(
            labels(&data["quarters"]),
            ["2026-Q1", "2026-Q2", "2026-Q3", "2026-Q4"]
        );
        let q: Vec<i64> = data["quarters"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["completed"].as_i64().unwrap())
            .collect();
        assert_eq!(q, [1, 2, 0, 0]);
        assert_eq!(data["quarters"][1]["start"], "2026-04-01");
        assert_eq!(data["quarters"][1]["end"], "2026-06-30");
        assert_eq!(data["stats"]["completed_in_period"], 3);
    }

    #[test]
    fn rows_are_clipped_to_a_period_that_starts_and_ends_mid_unit() {
        let data = build(ReportType::Yearly, "2026-02-15..2026-05-10", &[], &[], &[]);
        let months = data["months"].as_array().unwrap();
        assert_eq!(
            labels(&data["months"]),
            ["2026-02", "2026-03", "2026-04", "2026-05"]
        );
        assert_eq!(months[0]["start"], "2026-02-15");
        assert_eq!(months[0]["end"], "2026-02-28");
        assert_eq!(months[3]["start"], "2026-05-01");
        assert_eq!(months[3]["end"], "2026-05-10");
        let quarters = data["quarters"].as_array().unwrap();
        assert_eq!(labels(&data["quarters"]), ["2026-Q1", "2026-Q2"]);
        assert_eq!(quarters[0]["start"], "2026-02-15");
        assert_eq!(quarters[1]["end"], "2026-05-10");
    }

    #[test]
    fn the_december_rollover_and_leap_february_are_handled() {
        let data = build(
            ReportType::Quarterly,
            "2028-02-01..2028-02-29",
            &[],
            &[],
            &[],
        );
        assert_eq!(labels(&data["months"]), ["2028-02"]);
        assert_eq!(data["months"][0]["end"], "2028-02-29");
        let data = build(
            ReportType::Quarterly,
            "2026-12-15..2027-01-15",
            &[],
            &[],
            &[],
        );
        assert_eq!(labels(&data["months"]), ["2026-12", "2027-01"]);
    }

    #[test]
    fn each_row_carries_the_last_snapshot_inside_it() {
        let snaps = [
            snap("2026-09-30", 0, 4),
            snap("2026-10-01", 1, 4),
            snap("2026-10-20", 2, 4),
            snap("2026-12-31", 4, 4),
            snap("2027-01-01", 4, 4),
        ];
        let data = build(ReportType::Quarterly, "2026-Q4", &[], &[], &snaps);
        let months = data["months"].as_array().unwrap();
        assert_eq!(months[0]["snapshot"]["date"], "2026-10-20");
        assert_eq!(months[0]["snapshot"]["done"], 2);
        // November has no snapshot.
        assert!(months[1]["snapshot"].is_null());
        assert_eq!(months[2]["snapshot"]["date"], "2026-12-31");
        // 25% (10-01) -> 100% (12-31); snapshots outside the quarter ignored.
        let delta = &data["trend"]["delta"];
        assert_eq!(delta["completion_percent"], 75.0);
        assert_eq!(delta["done"], 3.0);
        assert!(data["trend"].get("points").is_none());

        let data = build(ReportType::Yearly, "2026", &[], &[], &snaps);
        let q4 = &data["quarters"][3]["snapshot"];
        assert_eq!(q4["date"], "2026-12-31");
        assert_eq!(data["quarters"][2]["snapshot"]["date"], "2026-09-30");
    }

    #[test]
    fn delta_needs_two_snapshots_and_warnings_are_carried() {
        let data = build(
            ReportType::Yearly,
            "2026",
            &[],
            &[],
            &[snap("2026-03-01", 1, 4)],
        );
        assert!(data["trend"]["delta"].is_null());
        assert_eq!(data["months"][2]["snapshot"]["done"], 1);
        assert_eq!(data["warnings"][0], "snapshot x.json is unreadable");
        let none = build(ReportType::Quarterly, "2026-Q4", &[], &[], &[]);
        assert!(none["trend"]["delta"].is_null());
        assert_eq!(none["stats"]["tasks_total"], 0);
    }

    #[test]
    #[should_panic(expected = "not a roll-up report")]
    fn other_report_types_are_a_programming_error() {
        let _ = build(ReportType::Weekly, "2026", &[], &[], &[]);
    }
}
