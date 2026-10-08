//! Data collection for the effort report (R5: FR-524 / SPEC-524).
//!
//! [`build_effort_data`] is a pure function from already-loaded project state
//! (tasks and `time_log.jsonl`) to the JSON `effort.md.hbs` renders as
//! `data`; [`collect_effort_data`] loads that state from a `.handoff/`
//! directory.
//!
//! The report covers the time-log entries inside an optional period
//! ([`resolve_period`]; no period means the whole log) that belong to an
//! optional person. A *person* is the entry's `agent_id`, else the assignee of
//! its task, else `(unassigned)`; `scope.assignee` is matched against it.
//! Hours are rounded to 2 decimals; absent values are `null`.
//!
//! ```text
//! period       { start, end } | null           assignee  string | null
//! stats        { total_hours, entries, tasks, people }
//! by_week      [ { week, hours, entries } ]    ISO weeks, chronological
//! by_day       [ { date, hours, entries } ]    UTC dates, chronological
//! by_task      [ { task_id, title, status, assignees, hours, entries,
//!                  estimate_hours, actual_hours, variance_hours,
//!                  variance_percent } ]
//! by_assignee  [ { assignee, hours, entries, tasks, share_percent } ]
//! deviation    { tasks_compared, tasks_not_compared, estimate_hours,
//!                actual_hours, variance_hours, variance_percent }
//! deviations   [ by_task row ]                 estimate and actual known,
//!                                              largest |variance| first
//! warnings     [ string ]
//! ```
//!
//! `estimate_hours` / `actual_hours` of a task row are the task's own
//! schedule values — `actual_hours` is the *cumulative* total, so the
//! variance (`actual - estimate`, and its percentage of the estimate) is a
//! whole-task figure, independent of the period and person filters that
//! select which tasks appear.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use anyhow::{bail, Result};
use chrono::Utc;
use serde_json::{json, Value};

use super::period::Period;
use super::weekly::{self, natural_cmp, percent, round2, ts_date};
use super::ReportScope;
use crate::storage::tasks::{collect_all_tasks, TaskData};
use crate::storage::time_log::{read_time_log, TimeLogEntry};

/// Person label for an entry with neither an `agent_id` nor a task assignee.
const UNASSIGNED: &str = "(unassigned)";

/// Everything [`build_effort_data`] derives the report from.
pub struct EffortInputs<'a> {
    /// `None`: every entry of the log.
    pub period: Option<Period>,
    /// `None`: every person.
    pub assignee: Option<&'a str>,
    /// Every task with its current status.
    pub tasks: &'a [(TaskData, String)],
    pub time_log: &'a [TimeLogEntry],
}

/// Loads the project state under `handoff_dir` and builds the effort data.
pub fn collect_effort_data(
    handoff_dir: &Path,
    period: Option<Period>,
    assignee: Option<&str>,
) -> Result<Value> {
    let mut tasks = Vec::new();
    collect_all_tasks(&handoff_dir.join("tasks"), &mut tasks)?;
    let time_log = read_time_log(handoff_dir)?;
    Ok(build_effort_data(&EffortInputs {
        period,
        assignee,
        tasks: &tasks,
        time_log: &time_log,
    }))
}

/// An effort report is built from the time log only; the verification scope
/// fields would be silently ignored, so they are an error instead.
pub fn validate_scope(scope: &ReportScope) -> Result<()> {
    let unsupported: Vec<&str> = [
        ("layers", !scope.layers.is_empty()),
        ("items", !scope.items.is_empty()),
        ("campaign", scope.campaign.is_some()),
        ("statuses", !scope.statuses.is_empty()),
        ("milestone", scope.milestone.is_some()),
    ]
    .into_iter()
    .filter_map(|(name, set)| set.then_some(name))
    .collect();
    if !unsupported.is_empty() {
        bail!(
            "scope.{} not supported for an effort report (use scope.period / scope.from / scope.to / scope.assignee)",
            unsupported.join(", scope.")
        );
    }
    Ok(())
}

/// The period an effort report covers: `scope.period` (ISO week or date
/// range) or `scope.from` + `scope.to`; `None` when the scope names no
/// period at all. Conflicting or half-given periods are an error, as for the
/// weekly report.
pub fn resolve_period(scope: &ReportScope) -> Result<Option<Period>> {
    if scope.period.is_none() && scope.from.is_none() && scope.to.is_none() {
        return Ok(None);
    }
    // `today` is only consulted when the scope names no period, handled above.
    weekly::resolve_period(scope, Utc::now().date_naive()).map(Some)
}

#[derive(Default)]
struct HoursAcc {
    hours: f64,
    entries: usize,
}

impl HoursAcc {
    fn add(&mut self, hours: f64) {
        self.hours += hours;
        self.entries += 1;
    }
}

#[derive(Default)]
struct TaskAcc {
    hours: HoursAcc,
    people: BTreeSet<String>,
}

#[derive(Default)]
struct PersonAcc {
    hours: HoursAcc,
    tasks: BTreeSet<String>,
}

pub fn build_effort_data(inputs: &EffortInputs) -> Value {
    let by_id: HashMap<&str, &(TaskData, String)> =
        inputs.tasks.iter().map(|t| (t.0.id.as_str(), t)).collect();
    let mut warnings: Vec<String> = Vec::new();

    let mut total = HoursAcc::default();
    let mut by_day: BTreeMap<chrono::NaiveDate, HoursAcc> = BTreeMap::new();
    let mut by_week: BTreeMap<String, HoursAcc> = BTreeMap::new();
    let mut by_task: BTreeMap<&str, TaskAcc> = BTreeMap::new();
    let mut by_person: BTreeMap<String, PersonAcc> = BTreeMap::new();

    for entry in inputs.time_log {
        let person = entry
            .agent_id
            .as_deref()
            .or_else(|| {
                by_id
                    .get(entry.task_id.as_str())
                    .and_then(|t| t.0.assignee.as_deref())
            })
            .unwrap_or(UNASSIGNED);
        if inputs.assignee.is_some_and(|wanted| wanted != person) {
            continue;
        }
        let Some(date) = ts_date(&entry.ts) else {
            warnings.push(format!(
                "time_log entry for {} has an unparseable ts '{}' and was skipped",
                entry.task_id, entry.ts
            ));
            continue;
        };
        if inputs.period.is_some_and(|p| !p.contains(date)) {
            continue;
        }

        total.add(entry.hours);
        by_day.entry(date).or_default().add(entry.hours);
        by_week
            .entry(date.format("%G-W%V").to_string())
            .or_default()
            .add(entry.hours);
        let task = by_task.entry(entry.task_id.as_str()).or_default();
        task.hours.add(entry.hours);
        task.people.insert(person.to_string());
        let who = by_person.entry(person.to_string()).or_default();
        who.hours.add(entry.hours);
        who.tasks.insert(entry.task_id.clone());
    }

    let mut task_ids: Vec<&str> = by_task.keys().copied().collect();
    task_ids.sort_by(|a, b| natural_cmp(a, b));
    for id in &task_ids {
        if !by_id.contains_key(id) {
            warnings.push(format!(
                "time_log references unknown task '{id}' (listed without a title or estimate)"
            ));
        }
    }

    let task_rows: Vec<Value> = task_ids
        .iter()
        .map(|id| {
            let acc = &by_task[id];
            let task = by_id.get(id);
            let schedule = task.and_then(|t| t.0.schedule.as_ref());
            let estimate = schedule.and_then(|s| s.estimate_hours);
            let actual = schedule.and_then(|s| s.actual_hours);
            let variance = estimate.zip(actual).map(|(e, a)| a - e);
            json!({
                "task_id": id,
                "title": task.map(|t| t.0.title.as_str()),
                "status": task.map(|t| t.1.as_str()),
                "assignees": acc.people,
                "hours": round2(acc.hours.hours),
                "entries": acc.hours.entries,
                "estimate_hours": estimate.map(round2),
                "actual_hours": actual.map(round2),
                "variance_hours": variance.map(round2),
                "variance_percent": variance
                    .zip(estimate)
                    .and_then(|(v, e)| percent(v, e)),
            })
        })
        .collect();

    // ---- deviation analysis ---------------------------------------------
    let mut deviations: Vec<(f64, &Value)> = task_rows
        .iter()
        .filter_map(|row| row["variance_hours"].as_f64().map(|v| (v, row)))
        .collect();
    deviations.sort_by(|a, b| b.0.abs().total_cmp(&a.0.abs()));
    let (estimate_sum, actual_sum) = deviations.iter().fold((0.0, 0.0), |(e, a), (_, row)| {
        (
            e + row["estimate_hours"].as_f64().unwrap_or(0.0),
            a + row["actual_hours"].as_f64().unwrap_or(0.0),
        )
    });
    let variance_sum = actual_sum - estimate_sum;
    let deviation = json!({
        "tasks_compared": deviations.len(),
        "tasks_not_compared": task_rows.len() - deviations.len(),
        "estimate_hours": round2(estimate_sum),
        "actual_hours": round2(actual_sum),
        "variance_hours": round2(variance_sum),
        "variance_percent": percent(variance_sum, estimate_sum),
    });
    let deviations: Vec<Value> = deviations.into_iter().map(|(_, row)| row.clone()).collect();

    // ---- by person ------------------------------------------------------
    let mut people: Vec<(&String, &PersonAcc)> = by_person.iter().collect();
    people.sort_by(|a, b| {
        b.1.hours
            .hours
            .total_cmp(&a.1.hours.hours)
            .then_with(|| a.0.cmp(b.0))
    });
    let by_assignee: Vec<Value> = people
        .iter()
        .map(|(name, acc)| {
            json!({
                "assignee": name,
                "hours": round2(acc.hours.hours),
                "entries": acc.hours.entries,
                "tasks": acc.tasks.len(),
                "share_percent": percent(acc.hours.hours, total.hours),
            })
        })
        .collect();

    json!({
        "period": inputs.period.map(|p| json!({
            "start": p.start.to_string(),
            "end": p.end.to_string(),
        })),
        "assignee": inputs.assignee,
        "stats": {
            "total_hours": round2(total.hours),
            "entries": total.entries,
            "tasks": task_rows.len(),
            "people": by_assignee.len(),
        },
        "by_week": by_week
            .iter()
            .map(|(week, acc)| json!({
                "week": week,
                "hours": round2(acc.hours),
                "entries": acc.entries,
            }))
            .collect::<Vec<_>>(),
        "by_day": by_day
            .iter()
            .map(|(date, acc)| json!({
                "date": date.to_string(),
                "hours": round2(acc.hours),
                "entries": acc.entries,
            }))
            .collect::<Vec<_>>(),
        "by_task": task_rows,
        "by_assignee": by_assignee,
        "deviation": deviation,
        "deviations": deviations,
        "warnings": warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{ReportEngine, ReportMeta, ReportStatus, ReportType};
    use crate::storage::tasks::{Schedule, TaskData};
    use crate::storage::time_log::TimeLogEntry;
    use std::collections::HashMap;

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

    fn est(data: &mut TaskData, estimate: Option<f64>, actual: Option<f64>) {
        data.schedule = Some(Schedule {
            estimate_hours: estimate,
            actual_hours: actual,
            ..Default::default()
        });
    }

    fn log(ts: &str, task_id: &str, hours: f64, agent: Option<&str>) -> TimeLogEntry {
        TimeLogEntry {
            ts: ts.into(),
            task_id: task_id.into(),
            hours,
            agent_id: agent.map(str::to_string),
            note: None,
        }
    }

    // 2026-W41 = Mon 2026-10-05 .. Sun 2026-10-11.
    fn week() -> Option<Period> {
        Some(Period::parse("2026-W41").unwrap())
    }

    fn build(
        period: Option<Period>,
        assignee: Option<&str>,
        tasks: &[(TaskData, String)],
        time_log: &[TimeLogEntry],
    ) -> Value {
        build_effort_data(&EffortInputs {
            period,
            assignee,
            tasks,
            time_log,
        })
    }

    #[test]
    fn hours_are_summed_per_task_within_the_inclusive_period() {
        let tasks = vec![task("t1", "done", |_| {}), task("t2", "todo", |_| {})];
        let entries = vec![
            log("2026-10-05T00:00:00+00:00", "t1", 1.5, None),
            log("2026-10-11T23:59:59+00:00", "t1", 2.0, None),
            log("2026-10-07T10:00:00+00:00", "t2", 0.5, None),
            log("2026-10-04T23:59:59+00:00", "t1", 9.0, None),
            log("2026-10-12T00:00:00+00:00", "t2", 9.0, None),
        ];
        let data = build(week(), None, &tasks, &entries);
        assert_eq!(data["stats"]["total_hours"], 4.0);
        assert_eq!(data["stats"]["entries"], 3);
        assert_eq!(data["stats"]["tasks"], 2);
        let by_task = data["by_task"].as_array().unwrap();
        assert_eq!(by_task[0]["task_id"], "t1");
        assert_eq!(by_task[0]["hours"], 3.5);
        assert_eq!(by_task[0]["entries"], 2);
        assert_eq!(by_task[0]["title"], "Title t1");
        assert_eq!(by_task[0]["status"], "done");
        assert_eq!(by_task[1]["task_id"], "t2");
        assert_eq!(by_task[1]["hours"], 0.5);
        assert_eq!(data["period"]["start"], "2026-10-05");
        assert_eq!(data["period"]["end"], "2026-10-11");
    }

    #[test]
    fn offset_timestamps_are_bucketed_by_their_utc_date() {
        // 2026-10-12T06:00+09:00 = 2026-10-11T21:00Z -> inside the week.
        let tasks = vec![task("t1", "todo", |_| {})];
        let entries = vec![log("2026-10-12T06:00:00+09:00", "t1", 1.0, None)];
        let data = build(week(), None, &tasks, &entries);
        assert_eq!(data["stats"]["total_hours"], 1.0);
        assert_eq!(data["by_day"][0]["date"], "2026-10-11");
    }

    #[test]
    fn without_a_period_every_entry_counts() {
        let tasks = vec![task("t1", "todo", |_| {})];
        let entries = vec![
            log("2020-01-01T00:00:00+00:00", "t1", 1.0, None),
            log("2026-10-07T00:00:00+00:00", "t1", 2.0, None),
        ];
        let data = build(None, None, &tasks, &entries);
        assert_eq!(data["stats"]["total_hours"], 3.0);
        assert!(data["period"].is_null());
    }

    #[test]
    fn by_day_and_by_week_group_hours_chronologically() {
        let tasks = vec![task("t1", "todo", |_| {})];
        let entries = vec![
            log("2026-10-12T09:00:00+00:00", "t1", 1.0, None),
            log("2026-10-05T09:00:00+00:00", "t1", 2.0, None),
            log("2026-10-05T15:00:00+00:00", "t1", 0.5, None),
            log("2026-10-11T09:00:00+00:00", "t1", 4.0, None),
        ];
        let data = build(None, None, &tasks, &entries);
        let days: Vec<(&str, f64, i64)> = data["by_day"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| {
                (
                    d["date"].as_str().unwrap(),
                    d["hours"].as_f64().unwrap(),
                    d["entries"].as_i64().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            days,
            vec![
                ("2026-10-05", 2.5, 2),
                ("2026-10-11", 4.0, 1),
                ("2026-10-12", 1.0, 1)
            ]
        );
        let weeks: Vec<(&str, f64)> = data["by_week"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| (w["week"].as_str().unwrap(), w["hours"].as_f64().unwrap()))
            .collect();
        assert_eq!(weeks, vec![("2026-W41", 6.5), ("2026-W42", 1.0)]);
    }

    #[test]
    fn assignee_is_the_entry_agent_else_the_task_assignee_else_unassigned() {
        let tasks = vec![
            task("t1", "todo", |d| d.assignee = Some("alice".into())),
            task("t2", "todo", |_| {}),
        ];
        let entries = vec![
            log("2026-10-06T00:00:00+00:00", "t1", 1.0, Some("bob")),
            log("2026-10-06T00:00:00+00:00", "t1", 2.0, None),
            log("2026-10-06T00:00:00+00:00", "t2", 4.0, None),
            log("2026-10-06T00:00:00+00:00", "t2", 1.0, Some("bob")),
        ];
        let data = build(None, None, &tasks, &entries);
        let people: Vec<(&str, f64, i64, f64)> = data["by_assignee"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                (
                    p["assignee"].as_str().unwrap(),
                    p["hours"].as_f64().unwrap(),
                    p["tasks"].as_i64().unwrap(),
                    p["share_percent"].as_f64().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            people,
            vec![
                ("(unassigned)", 4.0, 1, 50.0),
                ("alice", 2.0, 1, 25.0),
                ("bob", 2.0, 2, 25.0)
            ]
        );
        assert_eq!(data["stats"]["people"], 3);
    }

    #[test]
    fn assignee_filter_keeps_only_that_persons_entries() {
        let tasks = vec![
            task("t1", "todo", |d| d.assignee = Some("alice".into())),
            task("t2", "todo", |_| {}),
        ];
        let entries = vec![
            log("2026-10-06T00:00:00+00:00", "t1", 1.0, Some("bob")),
            log("2026-10-06T00:00:00+00:00", "t1", 2.0, None),
            log("2026-10-06T00:00:00+00:00", "t2", 4.0, Some("bob")),
        ];
        let bob = build(None, Some("bob"), &tasks, &entries);
        assert_eq!(bob["stats"]["total_hours"], 5.0);
        assert_eq!(bob["assignee"], "bob");
        assert_eq!(bob["by_task"].as_array().unwrap().len(), 2);

        let alice = build(None, Some("alice"), &tasks, &entries);
        assert_eq!(alice["stats"]["total_hours"], 2.0);
        assert_eq!(alice["by_task"][0]["task_id"], "t1");

        let nobody = build(None, Some("zed"), &tasks, &entries);
        assert_eq!(nobody["stats"]["total_hours"], 0.0);
        assert_eq!(nobody["stats"]["entries"], 0);
        assert!(nobody["by_task"].as_array().unwrap().is_empty());
    }

    #[test]
    fn deviation_compares_cumulative_actual_with_estimate_largest_first() {
        let tasks = vec![
            task("t1", "done", |d| est(d, Some(10.0), Some(12.0))),
            task("t2", "done", |d| est(d, Some(10.0), Some(5.0))),
            task("t3", "done", |d| est(d, Some(2.0), Some(2.5))),
            // No estimate: listed in by_task, excluded from the deviation.
            task("t4", "done", |d| est(d, None, Some(3.0))),
            // No schedule at all.
            task("t5", "todo", |_| {}),
        ];
        let entries: Vec<TimeLogEntry> = ["t1", "t2", "t3", "t4", "t5"]
            .iter()
            .map(|id| log("2026-10-06T00:00:00+00:00", id, 1.0, None))
            .collect();
        let data = build(None, None, &tasks, &entries);

        let dev = data["deviations"].as_array().unwrap();
        let ids: Vec<&str> = dev.iter().map(|d| d["task_id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["t2", "t1", "t3"]);
        assert_eq!(dev[0]["variance_hours"], -5.0);
        assert_eq!(dev[0]["variance_percent"], -50.0);
        assert_eq!(dev[1]["variance_hours"], 2.0);
        assert_eq!(dev[1]["variance_percent"], 20.0);
        assert_eq!(dev[2]["variance_percent"], 25.0);

        let summary = &data["deviation"];
        assert_eq!(summary["tasks_compared"], 3);
        assert_eq!(summary["tasks_not_compared"], 2);
        assert_eq!(summary["estimate_hours"], 22.0);
        assert_eq!(summary["actual_hours"], 19.5);
        assert_eq!(summary["variance_hours"], -2.5);
        assert_eq!(summary["variance_percent"], -11.4);

        let t4 = &data["by_task"][3];
        assert_eq!(t4["task_id"], "t4");
        assert!(t4["variance_hours"].is_null());
    }

    #[test]
    fn a_zero_estimate_has_no_variance_percent() {
        let tasks = vec![task("t1", "done", |d| est(d, Some(0.0), Some(1.0)))];
        let entries = vec![log("2026-10-06T00:00:00+00:00", "t1", 1.0, None)];
        let data = build(None, None, &tasks, &entries);
        assert_eq!(data["deviations"][0]["variance_hours"], 1.0);
        assert!(data["deviations"][0]["variance_percent"].is_null());
    }

    #[test]
    fn hours_are_rounded_to_two_decimals_and_never_negative_zero() {
        let tasks = vec![task("t1", "todo", |_| {})];
        let entries = vec![
            log("2026-10-06T00:00:00+00:00", "t1", 0.1, None),
            log("2026-10-06T00:00:00+00:00", "t1", 0.2, None),
        ];
        let data = build(None, None, &tasks, &entries);
        assert_eq!(data["stats"]["total_hours"], 0.3);
        let empty = build(None, None, &[], &[]);
        assert!(empty["stats"]["total_hours"]
            .as_f64()
            .unwrap()
            .is_sign_positive());
    }

    #[test]
    fn bad_timestamps_and_unknown_tasks_become_warnings() {
        let tasks = vec![task("t1", "todo", |_| {})];
        let entries = vec![
            log("garbage", "t1", 1.0, None),
            log("2026-10-06T00:00:00+00:00", "gone", 2.0, None),
            log("2026-10-06T01:00:00+00:00", "gone", 1.0, None),
        ];
        let data = build(None, None, &tasks, &entries);
        // The unparseable entry is skipped; the unknown task is still counted.
        assert_eq!(data["stats"]["total_hours"], 3.0);
        assert_eq!(data["by_task"][0]["task_id"], "gone");
        assert!(data["by_task"][0]["title"].is_null());
        let warnings: Vec<&str> = data["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w.as_str().unwrap())
            .collect();
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("garbage")));
        assert_eq!(
            warnings.iter().filter(|w| w.contains("'gone'")).count(),
            1,
            "an unknown task is warned about once: {warnings:?}"
        );
    }

    #[test]
    fn natural_task_order_in_by_task() {
        let tasks = vec![task("t10", "todo", |_| {}), task("t9", "todo", |_| {})];
        let entries = vec![
            log("2026-10-06T00:00:00+00:00", "t10", 1.0, None),
            log("2026-10-06T00:00:00+00:00", "t9", 1.0, None),
        ];
        let data = build(None, None, &tasks, &entries);
        assert_eq!(data["by_task"][0]["task_id"], "t9");
        assert_eq!(data["by_task"][1]["task_id"], "t10");
    }

    fn scope(period: Option<&str>, from: Option<&str>, to: Option<&str>) -> ReportScope {
        ReportScope {
            period: period.map(str::to_string),
            from: from.map(str::to_string),
            to: to.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn period_resolution_is_optional_but_never_ambiguous() {
        assert!(resolve_period(&scope(None, None, None)).unwrap().is_none());
        let iso = resolve_period(&scope(Some("2026-W41"), None, None))
            .unwrap()
            .unwrap();
        assert_eq!(iso.start.to_string(), "2026-10-05");
        let range = resolve_period(&scope(None, Some("2026-10-01"), Some("2026-10-31")))
            .unwrap()
            .unwrap();
        assert_eq!(range.end.to_string(), "2026-10-31");
        assert!(resolve_period(&scope(None, Some("2026-10-01"), None)).is_err());
        assert!(resolve_period(&scope(Some("2026-W41"), Some("2026-10-01"), None)).is_err());
        assert!(resolve_period(&scope(Some("nonsense"), None, None)).is_err());
    }

    #[test]
    fn verification_only_scope_fields_are_rejected() {
        let scopes = [
            ReportScope {
                layers: vec!["x".into()],
                ..Default::default()
            },
            ReportScope {
                items: vec!["x".into()],
                ..Default::default()
            },
            ReportScope {
                campaign: Some("x".into()),
                ..Default::default()
            },
            ReportScope {
                statuses: vec!["pass".into()],
                ..Default::default()
            },
        ];
        for scope in scopes {
            let err = validate_scope(&scope).unwrap_err().to_string();
            assert!(err.contains("effort"), "{err}");
        }
        let ok = ReportScope {
            label: Some("l".into()),
            assignee: Some("a".into()),
            period: Some("2026-W41".into()),
            ..Default::default()
        };
        assert!(validate_scope(&ok).is_ok());
    }

    #[test]
    fn collect_reads_tasks_and_time_log_from_a_handoff_dir() {
        let tmp = tempfile::tempdir().unwrap();
        // No tasks dir, no time log: an empty report, not an error.
        let data = collect_effort_data(tmp.path(), None, None).unwrap();
        assert_eq!(data["stats"]["entries"], 0);
    }

    fn render(data: &Value, scope: ReportScope) -> String {
        let meta = ReportMeta {
            report_id: "effort-1".into(),
            report_type: ReportType::Effort,
            scope,
            version: 1,
            status: ReportStatus::Draft,
            generated_at: "2026-10-08T00:00:00+00:00".into(),
            reviewer: None,
            approved_at: None,
            output_path: "reports/effort-1.md".into(),
            revision_history: Vec::new(),
        };
        ReportEngine::new().unwrap().generate(&meta, data).unwrap()
    }

    #[test]
    fn template_renders_every_section() {
        let tasks = vec![
            task("t1", "done", |d| {
                d.title = "Do | it".into();
                est(d, Some(10.0), Some(12.0));
            }),
            task("t2", "todo", |_| {}),
        ];
        let entries = vec![
            log("2026-10-06T00:00:00+00:00", "t1", 3.0, Some("bob")),
            log("2026-10-07T00:00:00+00:00", "t2", 1.0, Some("alice")),
        ];
        let data = build(week(), None, &tasks, &entries);
        let md = render(&data, ReportScope::default());
        assert!(md.starts_with("# Effort Report"), "{md}");
        assert!(md.contains("| Period | 2026-10-05 to 2026-10-11 |"), "{md}");
        assert!(md.contains("| Assignee | all |"), "{md}");
        assert!(md.contains("| Total hours | 4.0 |"), "{md}");
        assert!(md.contains("## Hours by Period"), "{md}");
        assert!(md.contains("| 2026-W41 | 4.0 | 2 |"), "{md}");
        assert!(md.contains("| 2026-10-06 | 3.0 | 1 |"), "{md}");
        assert!(md.contains("## Hours by Task"), "{md}");
        assert!(md.contains("| t1 | Do \\| it | done | bob | 3.0 |"), "{md}");
        assert!(md.contains("## Hours by Assignee"), "{md}");
        assert!(md.contains("| bob | 3.0 | 1 | 1 | 75% |"), "{md}");
        assert!(md.contains("## Estimate vs Actual"), "{md}");
        assert!(
            md.contains("| t1 | Do \\| it | 10.0 | 12.0 | +2.0 | 20% |"),
            "{md}"
        );
    }

    #[test]
    fn template_with_an_assignee_filter_and_no_data() {
        let data = build(None, Some("zed"), &[], &[]);
        let md = render(&data, ReportScope::default());
        assert!(md.contains("| Period | all time |"), "{md}");
        assert!(md.contains("| Assignee | zed |"), "{md}");
        assert!(md.contains("No time logged in scope."), "{md}");
        assert!(
            md.contains("No tasks with both an estimate and logged time."),
            "{md}"
        );
    }
}
