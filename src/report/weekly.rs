//! Weekly progress report data (FR-515 / SPEC-515).
//!
//! [`build_weekly_data`] is a pure function from already-loaded project state
//! (tasks, `time_log.jsonl`, `events.jsonl`, configured milestones) to the JSON
//! the `weekly` template renders as `data`. [`collect_weekly_data`] loads that
//! state from a `.handoff/` directory. The verification-progress block comes
//! from the trace graph and is attached by the caller (`handoff_report`), since
//! it needs the trace engine.
//!
//! Output shape (all hours are rounded to 2 decimals; absent values are `null`):
//!
//! ```text
//! period      { start, end, next_start, next_end }
//! stats       { completed_this_week, completed_total, tasks_total, hours_this_week,
//!               estimate_hours_total, actual_hours_total, consumption_percent }
//! completed   [ { id, title, estimate_hours, actual_hours, hours_this_week,
//!                 assignee, completed_at, source } ]       source: completed_at | event
//! in_progress [ { id, title, progress_percent, remaining_hours, due_date,
//!                 assignee, hours_this_week } ]
//! blockers    [ { id, title, assignee, due_date, unmet_dependencies } ]
//! time_log    { total_hours, entries, by_task: [ { task_id, title, hours } ] }
//! milestones  [ { name, date, description, done, total, percent,
//!                 estimate_hours, actual_hours } ]
//! next_week   [ { id, title, status, start_date, due_date, estimate_hours, assignee } ]
//! warnings    [ string ]
//! ```

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use anyhow::{bail, Result};
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{json, Value};

use super::period::{parse_date, Period};
use super::ReportScope;
use crate::storage::config::{read_config, MilestoneConfig};
use crate::storage::events::{read_events, EventFilters, EventRecord, EVENT_TASK_STATUS_CHANGED};
use crate::storage::tasks::{collect_all_tasks, is_terminal_status, TaskData};
use crate::storage::time_log::{read_time_log, TimeLogEntry};

/// Task status a report treats as "completed" (`skipped` is terminal but not
/// a completion).
const STATUS_DONE: &str = "done";

/// Where a completed task was detected; see the module docs.
const SOURCE_COMPLETED_AT: &str = "completed_at";
const SOURCE_EVENT: &str = "event";

/// Everything [`build_weekly_data`] derives the report from.
pub struct WeeklyInputs<'a> {
    pub period: Period,
    /// Every task with its current status.
    pub tasks: &'a [(TaskData, String)],
    pub time_log: &'a [TimeLogEntry],
    /// `task.status_changed` events (other events are ignored).
    pub events: &'a [EventRecord],
    pub milestones: &'a HashMap<String, MilestoneConfig>,
}

/// Loads the project state under `handoff_dir` and builds the weekly data.
pub fn collect_weekly_data(handoff_dir: &Path, period: Period) -> Result<Value> {
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
    // Same policy as the verification report: an unreadable config.toml is an
    // error, not a silent loss of the milestone dates.
    let milestones = read_config(&handoff_dir.join("config.toml"))?.milestones;
    Ok(build_weekly_data(&WeeklyInputs {
        period,
        tasks: &tasks,
        time_log: &time_log,
        events: &events,
        milestones: &milestones,
    }))
}

/// Resolves the period a weekly report covers from `scope`:
/// `period` (ISO week or date range), else `from` + `to`, else the ISO week
/// containing `today`. `period` together with `from`/`to`, or only one of
/// `from`/`to`, is an error rather than a silent pick.
pub fn resolve_period(scope: &ReportScope, today: NaiveDate) -> Result<Period> {
    match (&scope.period, &scope.from, &scope.to) {
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
            bail!("scope.period cannot be combined with scope.from/scope.to")
        }
        (Some(period), None, None) => Period::parse(period),
        (None, Some(from), Some(to)) => Period::new(parse_date(from)?, parse_date(to)?),
        (None, Some(_), None) | (None, None, Some(_)) => {
            bail!("scope.from and scope.to must be given together")
        }
        (None, None, None) => Ok(Period::iso_week_of(today)),
    }
}

pub fn build_weekly_data(inputs: &WeeklyInputs) -> Value {
    let period = inputs.period;
    let next = period.next();
    let mut warnings: Vec<String> = Vec::new();

    let mut tasks: Vec<&(TaskData, String)> = inputs.tasks.iter().collect();
    tasks.sort_by(|a, b| natural_cmp(&a.0.id, &b.0.id));
    let by_id: HashMap<&str, &(TaskData, String)> =
        tasks.iter().map(|t| (t.0.id.as_str(), *t)).collect();

    // ---- time log -------------------------------------------------------
    let mut hours_by_task: BTreeMap<&str, f64> = BTreeMap::new();
    let mut entries_in_period = 0usize;
    for entry in inputs.time_log {
        match ts_date(&entry.ts) {
            Some(date) if period.contains(date) => {
                entries_in_period += 1;
                *hours_by_task.entry(entry.task_id.as_str()).or_insert(0.0) += entry.hours;
            }
            Some(_) => {}
            None => warnings.push(format!(
                "time_log entry for {} has an unparseable ts '{}' and was skipped",
                entry.task_id, entry.ts
            )),
        }
    }
    let hours_of = |id: &str| hours_by_task.get(id).copied().unwrap_or(0.0);
    let hours_this_week: f64 = hours_by_task.values().sum();
    let mut by_task: Vec<(&str, f64)> = hours_by_task.iter().map(|(k, v)| (*k, *v)).collect();
    by_task.sort_by(|a, b| natural_cmp(a.0, b.0));

    // ---- completed tasks -----------------------------------------------
    let mut completed_ids: Vec<(&str, &'static str)> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for (data, status) in &tasks {
        if status != STATUS_DONE {
            continue;
        }
        match data.completed_at.as_deref().map(ts_date) {
            Some(Some(date)) if period.contains(date) => {
                seen.insert(data.id.as_str());
                completed_ids.push((data.id.as_str(), SOURCE_COMPLETED_AT));
            }
            Some(None) => warnings.push(format!(
                "task {} has an unparseable completed_at '{}'",
                data.id,
                data.completed_at.as_deref().unwrap_or_default()
            )),
            _ => {}
        }
    }
    // Supplement from the event log: a task that is done now and transitioned
    // to done inside the period, but whose completed_at is missing or outside it.
    for event in inputs.events {
        if event.event != EVENT_TASK_STATUS_CHANGED {
            continue;
        }
        let Some(task_id) = event.task_id.as_deref() else {
            continue;
        };
        let Some(date) = ts_date(&event.ts) else {
            warnings.push(format!(
                "status_changed event for {task_id} has an unparseable ts '{}' and was skipped",
                event.ts
            ));
            continue;
        };
        if !period.contains(date) || seen.contains(task_id) {
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
        let to_done = detail.get("to").and_then(Value::as_str) == Some(STATUS_DONE);
        let still_done = by_id.get(task_id).is_some_and(|t| t.1 == STATUS_DONE);
        if to_done && still_done {
            seen.insert(task_id);
            completed_ids.push((task_id, SOURCE_EVENT));
        }
    }
    completed_ids.sort_by(|a, b| natural_cmp(a.0, b.0));
    let completed: Vec<Value> = completed_ids
        .iter()
        .filter_map(|(id, source)| by_id.get(id).map(|t| (t, source)))
        .map(|((data, _), source)| {
            let schedule = data.schedule.as_ref();
            json!({
                "id": data.id,
                "title": data.title,
                "estimate_hours": schedule.and_then(|s| s.estimate_hours).map(round2),
                "actual_hours": schedule.and_then(|s| s.actual_hours).map(round2),
                "hours_this_week": round2(hours_of(&data.id)),
                "assignee": data.assignee,
                "completed_at": data.completed_at,
                "source": source,
            })
        })
        .collect();

    // ---- in progress / blockers ----------------------------------------
    let in_progress: Vec<Value> = tasks
        .iter()
        .filter(|(_, status)| status == "in_progress")
        .map(|(data, _)| {
            let schedule = data.schedule.as_ref();
            json!({
                "id": data.id,
                "title": data.title,
                "progress_percent": progress_percent(data),
                "remaining_hours": schedule.and_then(|s| s.remaining_hours).map(round2),
                "due_date": schedule.and_then(|s| s.due_date.clone()),
                "assignee": data.assignee,
                "hours_this_week": round2(hours_of(&data.id)),
            })
        })
        .collect();

    let blockers: Vec<Value> = tasks
        .iter()
        .filter(|(_, status)| status == "blocked")
        .map(|(data, _)| {
            let unmet: Vec<&str> = data
                .dependencies
                .iter()
                .map(String::as_str)
                .filter(|dep| by_id.get(dep).is_none_or(|t| !is_terminal_status(&t.1)))
                .collect();
            json!({
                "id": data.id,
                "title": data.title,
                "assignee": data.assignee,
                "due_date": data.schedule.as_ref().and_then(|s| s.due_date.clone()),
                "unmet_dependencies": unmet,
            })
        })
        .collect();

    // ---- totals ---------------------------------------------------------
    let hours_sum = |pick: fn(&crate::storage::tasks::Schedule) -> Option<f64>| -> f64 {
        tasks
            .iter()
            .filter_map(|(d, _)| d.schedule.as_ref().and_then(pick))
            .sum()
    };
    let estimate_total = hours_sum(|s| s.estimate_hours);
    let actual_total = hours_sum(|s| s.actual_hours);
    let completed_total = tasks.iter().filter(|(_, s)| s == STATUS_DONE).count();

    // ---- milestones -----------------------------------------------------
    let milestones = milestone_rows(&tasks, inputs.milestones);

    // ---- next week ------------------------------------------------------
    let in_next = |date: Option<&String>| {
        date.and_then(|d| parse_date(d).ok())
            .is_some_and(|d| next.contains(d))
    };
    let mut next_week: Vec<&(TaskData, String)> = tasks
        .iter()
        .copied()
        .filter(|(_, status)| matches!(status.as_str(), "todo" | "in_progress" | "review"))
        .filter(|(data, _)| {
            let schedule = data.schedule.as_ref();
            in_next(schedule.and_then(|s| s.start_date.as_ref()))
                || in_next(schedule.and_then(|s| s.due_date.as_ref()))
        })
        .collect();
    next_week.sort_by(|a, b| {
        let due = |t: &(TaskData, String)| t.0.schedule.as_ref().and_then(|s| s.due_date.clone());
        due(a)
            .cmp(&due(b))
            .then_with(|| natural_cmp(&a.0.id, &b.0.id))
    });
    let next_week: Vec<Value> = next_week
        .iter()
        .map(|(data, status)| {
            let schedule = data.schedule.as_ref();
            json!({
                "id": data.id,
                "title": data.title,
                "status": status,
                "start_date": schedule.and_then(|s| s.start_date.clone()),
                "due_date": schedule.and_then(|s| s.due_date.clone()),
                "estimate_hours": schedule.and_then(|s| s.estimate_hours).map(round2),
                "assignee": data.assignee,
            })
        })
        .collect();

    json!({
        "period": {
            "start": period.start.to_string(),
            "end": period.end.to_string(),
            "next_start": next.start.to_string(),
            "next_end": next.end.to_string(),
        },
        "stats": {
            "completed_this_week": completed.len(),
            "completed_total": completed_total,
            "tasks_total": tasks.len(),
            "hours_this_week": round2(hours_this_week),
            "estimate_hours_total": round2(estimate_total),
            "actual_hours_total": round2(actual_total),
            "consumption_percent": percent(actual_total, estimate_total),
        },
        "completed": completed,
        "in_progress": in_progress,
        "blockers": blockers,
        "time_log": {
            "total_hours": round2(hours_this_week),
            "entries": entries_in_period,
            "by_task": by_task
                .iter()
                .map(|(id, hours)| json!({
                    "task_id": id,
                    "title": by_id.get(id).map(|t| t.0.title.as_str()),
                    "hours": round2(*hours),
                }))
                .collect::<Vec<_>>(),
        },
        "milestones": milestones,
        "next_week": next_week,
        "warnings": warnings,
    })
}

/// One row per milestone (configured ones, plus any a task refers to):
/// `{ name, date, description, done, total, percent, estimate_hours,
/// actual_hours }`. Dated milestones come first (earliest first), then
/// undated ones; the name breaks ties.
pub(super) fn milestone_rows(
    tasks: &[&(TaskData, String)],
    configured: &HashMap<String, MilestoneConfig>,
) -> Vec<Value> {
    #[derive(Default)]
    struct MilestoneAcc {
        done: usize,
        total: usize,
        estimate: f64,
        actual: f64,
    }
    let mut acc: BTreeMap<&str, MilestoneAcc> = BTreeMap::new();
    for name in configured.keys() {
        acc.entry(name.as_str()).or_default();
    }
    for (data, status) in tasks {
        let Some(schedule) = data.schedule.as_ref() else {
            continue;
        };
        let Some(name) = schedule.milestone.as_deref() else {
            continue;
        };
        let m = acc.entry(name).or_default();
        m.total += 1;
        if is_terminal_status(status) {
            m.done += 1;
        }
        m.estimate += schedule.estimate_hours.unwrap_or(0.0);
        m.actual += schedule.actual_hours.unwrap_or(0.0);
    }
    let mut milestones: Vec<Value> = acc
        .into_iter()
        .map(|(name, m)| {
            let config = configured.get(name);
            json!({
                "name": name,
                "date": config.and_then(|c| c.date.clone()),
                "description": config.and_then(|c| c.description.clone()),
                "done": m.done,
                "total": m.total,
                "percent": percent(m.done as f64, m.total as f64),
                "estimate_hours": round2(m.estimate),
                "actual_hours": round2(m.actual),
            })
        })
        .collect();
    // Dated milestones first (earliest first), undated last; name breaks ties.
    milestones.sort_by(|a, b| {
        let key = |v: &Value| v["date"].as_str().map(str::to_string);
        let by_date = match (key(a), key(b)) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        };
        by_date.then_with(|| a["name"].as_str().cmp(&b["name"].as_str()))
    });
    milestones
}

/// Verification-progress block for the weekly report, from a
/// `handoff_trace_report` result (`layer_statuses`, `project_status`,
/// `coverage.<layer>.state`, `trace_layers.in_use`):
///
/// ```text
/// { available: true, project_status,
///   layers: [ { layer, status, total, passing, failing, blocked, not_run,
///               uncovered, percent } ] }
/// ```
///
/// `percent` is `passing / total` (null for an empty layer). Layers follow the
/// in-use order; a layer present only in `layer_statuses` is appended.
pub fn verification_block(trace_report: &Value) -> Value {
    let statuses = trace_report
        .get("layer_statuses")
        .and_then(Value::as_object);
    let coverage = trace_report.get("coverage").and_then(Value::as_object);

    let mut order: Vec<String> = trace_report["trace_layers"]["in_use"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    for layer in statuses.into_iter().flat_map(|m| m.keys()) {
        if !order.contains(layer) {
            order.push(layer.clone());
        }
    }

    let count = |layer: &str, key: &str| -> u64 {
        coverage
            .and_then(|c| c.get(layer))
            .and_then(|l| l["state"][key].as_u64())
            .unwrap_or(0)
    };
    let layers: Vec<Value> = order
        .iter()
        .map(|layer| {
            let total = coverage
                .and_then(|c| c.get(layer))
                .and_then(|l| l["total"].as_u64())
                .unwrap_or(0);
            let passing = count(layer, "passing");
            json!({
                "layer": layer,
                "status": statuses.and_then(|m| m.get(layer)).cloned().unwrap_or(Value::Null),
                "total": total,
                "passing": passing,
                "failing": count(layer, "failing"),
                "blocked": count(layer, "blocked"),
                "not_run": count(layer, "not_run"),
                "uncovered": count(layer, "uncovered"),
                "percent": percent(passing as f64, total as f64),
            })
        })
        .collect();

    json!({
        "available": true,
        "project_status": trace_report.get("project_status").cloned().unwrap_or(Value::Null),
        "layers": layers,
    })
}

/// Placeholder for [`verification_block`] when the trace report could not be
/// produced; the reason is rendered instead of failing the whole report.
pub fn verification_unavailable(reason: &str) -> Value {
    json!({ "available": false, "error": reason, "layers": [] })
}

/// Done-criteria completion, falling back to the hours burned down
/// (`(estimate - remaining) / estimate`); `null` when neither is known.
fn progress_percent(data: &TaskData) -> Option<f64> {
    if !data.done_criteria.is_empty() {
        let checked = data.done_criteria.iter().filter(|c| c.checked).count();
        return percent(checked as f64, data.done_criteria.len() as f64);
    }
    let schedule = data.schedule.as_ref()?;
    let estimate = schedule.estimate_hours?;
    let remaining = schedule.remaining_hours?;
    percent((estimate - remaining).max(0.0).min(estimate), estimate)
}

/// `part / whole` as a percentage with one decimal; `None` when `whole` is 0.
pub(super) fn percent(part: f64, whole: f64) -> Option<f64> {
    (whole > 0.0).then(|| (part / whole * 1000.0).round() / 10.0)
}

/// Rounds to two decimals. `+ 0.0` turns a negative zero (what an empty
/// `f64` sum or a tiny negative value rounds to) into `0.0`, so reports never
/// print `-0.0`.
pub(super) fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0 + 0.0
}

/// UTC calendar date of an RFC 3339 timestamp, or of a bare `YYYY-MM-DD`.
pub(super) fn ts_date(ts: &str) -> Option<NaiveDate> {
    DateTime::parse_from_rfc3339(ts)
        .map(|dt| dt.with_timezone(&Utc).date_naive())
        .ok()
        .or_else(|| parse_date(ts).ok())
}

/// Orders ids so digit runs compare numerically (`t9` < `t10`, `t5.2` < `t5.10`).
pub(super) fn natural_cmp(a: &str, b: &str) -> Ordering {
    fn chunks(s: &str) -> Vec<(bool, &str)> {
        let mut out = Vec::new();
        let mut start = 0;
        let mut prev: Option<bool> = None;
        for (i, c) in s.char_indices() {
            let digit = c.is_ascii_digit();
            if prev.is_some_and(|p| p != digit) {
                out.push((prev == Some(true), &s[start..i]));
                start = i;
            }
            prev = Some(digit);
        }
        if prev.is_some() {
            out.push((prev == Some(true), &s[start..]));
        }
        out
    }
    let (ca, cb) = (chunks(a), chunks(b));
    for (x, y) in ca.iter().zip(cb.iter()) {
        let ord = match (x.0, y.0) {
            (true, true) => {
                let (nx, ny) = (x.1.trim_start_matches('0'), y.1.trim_start_matches('0'));
                nx.len().cmp(&ny.len()).then_with(|| nx.cmp(ny))
            }
            _ => x.1.cmp(y.1),
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    ca.len().cmp(&cb.len()).then_with(|| a.cmp(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::tasks::{DoneCriterion, Schedule};

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

    fn schedule(data: &mut TaskData, f: impl FnOnce(&mut Schedule)) {
        f(data.schedule.get_or_insert_with(Default::default));
    }

    fn log(ts: &str, task_id: &str, hours: f64) -> TimeLogEntry {
        TimeLogEntry {
            ts: ts.into(),
            task_id: task_id.into(),
            hours,
            agent_id: None,
            note: None,
        }
    }

    fn status_event(ts: &str, task_id: &str, from: &str, to: &str) -> EventRecord {
        EventRecord {
            ts: ts.into(),
            event: EVENT_TASK_STATUS_CHANGED.into(),
            task_id: Some(task_id.into()),
            agent_id: None,
            session_id: None,
            detail: Some(json!({ "from": from, "to": to }).to_string()),
        }
    }

    // 2026-W41 = Mon 2026-10-05 .. Sun 2026-10-11.
    fn week() -> Period {
        Period::parse("2026-W41").unwrap()
    }

    fn build(
        tasks: &[(TaskData, String)],
        time_log: &[TimeLogEntry],
        events: &[EventRecord],
    ) -> Value {
        build_weekly_data(&WeeklyInputs {
            period: week(),
            tasks,
            time_log,
            events,
            milestones: &HashMap::new(),
        })
    }

    fn ids(v: &Value) -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn completed_tasks_are_those_done_with_completed_at_in_the_period() {
        let tasks = vec![
            task("t1", "done", |d| {
                d.completed_at = Some("2026-10-05T00:00:00+00:00".into())
            }),
            task("t2", "done", |d| {
                d.completed_at = Some("2026-10-11T23:59:59+00:00".into())
            }),
            task("t3", "done", |d| {
                d.completed_at = Some("2026-10-04T23:59:59+00:00".into())
            }),
            task("t4", "done", |d| {
                d.completed_at = Some("2026-10-12T00:00:00+00:00".into())
            }),
            // In range but not done (reopened): not completed.
            task("t5", "in_progress", |d| {
                d.completed_at = Some("2026-10-07T00:00:00+00:00".into())
            }),
            task("t6", "skipped", |d| {
                d.completed_at = Some("2026-10-07T00:00:00+00:00".into())
            }),
        ];
        let data = build(&tasks, &[], &[]);
        assert_eq!(ids(&data["completed"]), vec!["t1", "t2"]);
        assert_eq!(data["completed"][0]["source"], "completed_at");
        assert_eq!(data["stats"]["completed_this_week"], 2);
        assert_eq!(data["stats"]["completed_total"], 4);
        assert_eq!(data["stats"]["tasks_total"], 6);
    }

    #[test]
    fn completed_at_with_an_offset_is_compared_in_utc() {
        // 2026-10-12T06:00+09:00 is 2026-10-11T21:00Z -> inside the week.
        let tasks = vec![task("t1", "done", |d| {
            d.completed_at = Some("2026-10-12T06:00:00+09:00".into())
        })];
        assert_eq!(ids(&build(&tasks, &[], &[])["completed"]), vec!["t1"]);
    }

    #[test]
    fn status_changed_events_supplement_completed_tasks() {
        let tasks = vec![
            // Done now, transitioned inside the period, completed_at missing.
            task("t1", "done", |_| {}),
            // Transitioned inside the period but completed_at is later.
            task("t2", "done", |d| {
                d.completed_at = Some("2026-10-20T00:00:00+00:00".into())
            }),
            // Already counted through completed_at: must not be duplicated.
            task("t3", "done", |d| {
                d.completed_at = Some("2026-10-06T00:00:00+00:00".into())
            }),
            // Event inside the period but the task was reopened since.
            task("t4", "in_progress", |_| {}),
            // Event outside the period.
            task("t5", "done", |_| {}),
            // Event to a non-done status.
            task("t6", "review", |_| {}),
        ];
        let events = vec![
            status_event("2026-10-07T10:00:00+00:00", "t1", "in_progress", "done"),
            status_event("2026-10-08T10:00:00+00:00", "t2", "review", "done"),
            status_event("2026-10-06T10:00:00+00:00", "t3", "review", "done"),
            status_event("2026-10-09T10:00:00+00:00", "t4", "review", "done"),
            status_event("2026-10-12T10:00:00+00:00", "t5", "review", "done"),
            status_event("2026-10-07T10:00:00+00:00", "t6", "in_progress", "review"),
            status_event("2026-10-07T10:00:00+00:00", "ghost", "in_progress", "done"),
        ];
        let data = build(&tasks, &[], &events);
        assert_eq!(ids(&data["completed"]), vec!["t1", "t2", "t3"]);
        let source = |i: usize| data["completed"][i]["source"].clone();
        assert_eq!(source(0), "event");
        assert_eq!(source(1), "event");
        assert_eq!(source(2), "completed_at");
    }

    #[test]
    fn malformed_event_detail_is_skipped_with_a_warning() {
        let tasks = vec![task("t1", "done", |_| {})];
        let mut event = status_event("2026-10-07T10:00:00+00:00", "t1", "a", "done");
        event.detail = Some("not json".into());
        let mut no_detail = event.clone();
        no_detail.detail = None;
        let data = build(&tasks, &[], &[event, no_detail]);
        assert!(ids(&data["completed"]).is_empty());
        let warnings = data["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings.iter().all(|w| w.as_str().unwrap().contains("t1")));
    }

    #[test]
    fn event_with_a_bad_ts_is_skipped_with_a_warning() {
        let tasks = vec![task("t1", "done", |_| {})];
        let event = status_event("yesterday-ish", "t1", "a", "done");
        let data = build(&tasks, &[], &[event]);
        assert!(ids(&data["completed"]).is_empty());
        let warnings = data["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].as_str().unwrap().contains("yesterday-ish"));
    }

    #[test]
    fn unparseable_completed_at_is_reported_not_dropped_silently() {
        let tasks = vec![task("t1", "done", |d| {
            d.completed_at = Some("last tuesday".into())
        })];
        let data = build(&tasks, &[], &[]);
        assert!(ids(&data["completed"]).is_empty());
        let warnings = data["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].as_str().unwrap().contains("t1"));
    }

    #[test]
    fn time_log_hours_are_summed_per_task_within_the_period() {
        let tasks = vec![
            task("t1", "done", |d| {
                d.completed_at = Some("2026-10-07T00:00:00+00:00".into());
                schedule(d, |s| {
                    s.estimate_hours = Some(4.0);
                    s.actual_hours = Some(5.5);
                });
            }),
            task("t2", "in_progress", |_| {}),
        ];
        let entries = vec![
            log("2026-10-04T23:59:59+00:00", "t1", 9.0), // before
            log("2026-10-05T00:00:00+00:00", "t1", 1.25),
            log("2026-10-07T12:00:00+00:00", "t1", 0.5),
            log("2026-10-11T23:59:59+00:00", "t2", 2.0),
            log("2026-10-12T00:00:00+00:00", "t2", 9.0), // after
            log("2026-10-08T00:00:00+00:00", "gone", 0.75), // unknown task
        ];
        let data = build(&tasks, &entries, &[]);
        assert_eq!(data["stats"]["hours_this_week"], 4.5);
        assert_eq!(data["time_log"]["total_hours"], 4.5);
        assert_eq!(data["time_log"]["entries"], 4);
        let by_task = data["time_log"]["by_task"].as_array().unwrap();
        let row = |id: &str| by_task.iter().find(|r| r["task_id"] == id).unwrap();
        assert_eq!(row("t1")["hours"], 1.75);
        assert_eq!(row("t1")["title"], "Title t1");
        assert_eq!(row("t2")["hours"], 2.0);
        assert_eq!(row("gone")["hours"], 0.75);
        assert!(row("gone")["title"].is_null());
        // Per-task hours also appear on the task rows.
        assert_eq!(data["completed"][0]["hours_this_week"], 1.75);
        assert_eq!(data["completed"][0]["estimate_hours"], 4.0);
        assert_eq!(data["completed"][0]["actual_hours"], 5.5);
        assert_eq!(data["in_progress"][0]["hours_this_week"], 2.0);
    }

    #[test]
    fn time_log_entry_with_a_bad_ts_is_skipped_with_a_warning() {
        let data = build(&[], &[log("garbage", "t1", 3.0)], &[]);
        assert_eq!(data["stats"]["hours_this_week"], 0.0);
        assert_eq!(data["warnings"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn consumption_is_actual_over_estimate_and_null_without_estimates() {
        let tasks = vec![
            task("t1", "done", |d| {
                schedule(d, |s| {
                    s.estimate_hours = Some(8.0);
                    s.actual_hours = Some(2.0);
                })
            }),
            task("t2", "todo", |d| {
                schedule(d, |s| {
                    s.estimate_hours = Some(2.0);
                    s.actual_hours = Some(1.0);
                })
            }),
        ];
        let data = build(&tasks, &[], &[]);
        assert_eq!(data["stats"]["estimate_hours_total"], 10.0);
        assert_eq!(data["stats"]["actual_hours_total"], 3.0);
        assert_eq!(data["stats"]["consumption_percent"], 30.0);

        let none = build(&[task("t1", "todo", |_| {})], &[], &[]);
        assert!(none["stats"]["consumption_percent"].is_null());
    }

    #[test]
    fn in_progress_rows_report_progress_remaining_and_due() {
        let tasks = vec![
            task("t1", "in_progress", |d| {
                d.assignee = Some("alice".into());
                d.done_criteria = vec![
                    DoneCriterion {
                        item: "a".into(),
                        checked: true,
                    },
                    DoneCriterion {
                        item: "b".into(),
                        checked: false,
                    },
                    DoneCriterion {
                        item: "c".into(),
                        checked: false,
                    },
                    DoneCriterion {
                        item: "d".into(),
                        checked: false,
                    },
                ];
                schedule(d, |s| {
                    s.remaining_hours = Some(3.0);
                    s.due_date = Some("2026-10-14".into());
                });
            }),
            // No criteria: falls back to estimate/remaining burn-down.
            task("t2", "in_progress", |d| {
                schedule(d, |s| {
                    s.estimate_hours = Some(8.0);
                    s.remaining_hours = Some(2.0);
                })
            }),
            // Nothing to derive progress from.
            task("t3", "in_progress", |_| {}),
            task("t4", "todo", |_| {}),
        ];
        let data = build(&tasks, &[], &[]);
        let rows = data["in_progress"].as_array().unwrap();
        assert_eq!(ids(&data["in_progress"]), vec!["t1", "t2", "t3"]);
        assert_eq!(rows[0]["progress_percent"], 25.0);
        assert_eq!(rows[0]["remaining_hours"], 3.0);
        assert_eq!(rows[0]["due_date"], "2026-10-14");
        assert_eq!(rows[0]["assignee"], "alice");
        assert_eq!(rows[1]["progress_percent"], 75.0);
        assert!(rows[2]["progress_percent"].is_null());
        assert!(rows[2]["remaining_hours"].is_null());
    }

    #[test]
    fn blockers_list_blocked_tasks_with_unmet_dependencies() {
        let tasks = vec![
            task("t1", "done", |_| {}),
            task("t2", "todo", |_| {}),
            task("t3", "blocked", |d| {
                d.dependencies = vec!["t1".into(), "t2".into(), "missing".into()]
            }),
            task("t4", "in_progress", |_| {}),
        ];
        let data = build(&tasks, &[], &[]);
        assert_eq!(ids(&data["blockers"]), vec!["t3"]);
        assert_eq!(
            data["blockers"][0]["unmet_dependencies"],
            json!(["t2", "missing"])
        );
    }

    #[test]
    fn milestones_aggregate_task_progress_and_join_config() {
        let tasks = vec![
            task("t1", "done", |d| {
                schedule(d, |s| {
                    s.milestone = Some("beta".into());
                    s.estimate_hours = Some(2.0);
                    s.actual_hours = Some(1.0);
                })
            }),
            task("t2", "skipped", |d| {
                schedule(d, |s| s.milestone = Some("beta".into()))
            }),
            task("t3", "todo", |d| {
                schedule(d, |s| {
                    s.milestone = Some("beta".into());
                    s.estimate_hours = Some(3.0);
                })
            }),
            task("t4", "todo", |d| {
                schedule(d, |s| s.milestone = Some("adhoc".into()))
            }),
            task("t5", "todo", |_| {}),
        ];
        let mut configured = HashMap::new();
        configured.insert(
            "beta".to_string(),
            MilestoneConfig {
                date: Some("2026-11-01".into()),
                color: None,
                description: Some("Beta release".into()),
            },
        );
        configured.insert(
            "alpha".to_string(),
            MilestoneConfig {
                date: Some("2026-10-01".into()),
                color: None,
                description: None,
            },
        );
        let data = build_weekly_data(&WeeklyInputs {
            period: week(),
            tasks: &tasks,
            time_log: &[],
            events: &[],
            milestones: &configured,
        });
        let ms = data["milestones"].as_array().unwrap();
        // Dated first by date (alpha, beta), then undated (adhoc).
        let names: Vec<&str> = ms.iter().map(|m| m["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["alpha", "beta", "adhoc"]);
        assert_eq!(ms[0]["total"], 0);
        assert!(ms[0]["percent"].is_null());
        assert_eq!(ms[1]["done"], 2);
        assert_eq!(ms[1]["total"], 3);
        assert_eq!(ms[1]["percent"], 66.7);
        assert_eq!(ms[1]["estimate_hours"], 5.0);
        assert_eq!(ms[1]["actual_hours"], 1.0);
        assert_eq!(ms[1]["date"], "2026-11-01");
        assert_eq!(ms[1]["description"], "Beta release");
        assert_eq!(ms[2]["total"], 1);
        assert!(ms[2]["date"].is_null());
    }

    #[test]
    fn next_week_lists_open_tasks_starting_or_due_next_week() {
        // Next week = 2026-10-12 .. 2026-10-18.
        let tasks = vec![
            task("t1", "todo", |d| {
                schedule(d, |s| s.start_date = Some("2026-10-12".into()))
            }),
            task("t2", "in_progress", |d| {
                schedule(d, |s| s.due_date = Some("2026-10-18".into()))
            }),
            task("t3", "todo", |d| {
                schedule(d, |s| s.due_date = Some("2026-10-19".into()))
            }),
            task("t4", "done", |d| {
                schedule(d, |s| s.due_date = Some("2026-10-14".into()))
            }),
            task("t5", "blocked", |d| {
                schedule(d, |s| s.due_date = Some("2026-10-14".into()))
            }),
            task("t6", "review", |d| {
                schedule(d, |s| s.due_date = Some("2026-10-13".into()))
            }),
            task("t7", "todo", |_| {}),
        ];
        let data = build(&tasks, &[], &[]);
        assert_eq!(data["period"]["next_start"], "2026-10-12");
        assert_eq!(data["period"]["next_end"], "2026-10-18");
        // Sorted by due date (undated first), then id.
        assert_eq!(ids(&data["next_week"]), vec!["t1", "t6", "t2"]);
        assert_eq!(data["next_week"][1]["status"], "review");
    }

    #[test]
    fn nested_and_numeric_ids_sort_naturally() {
        let mut v = vec!["t10", "t9", "t5.10", "t5.2", "t5", "t100"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["t5", "t5.2", "t5.10", "t9", "t10", "t100"]);
    }

    #[test]
    fn verification_block_summarises_layers_in_use_order() {
        let report = json!({
            "trace_layers": { "in_use": ["system", "unit"] },
            "layer_statuses": { "unit": "in_progress", "system": "verified" },
            "project_status": "in_progress",
            "coverage": {
                "system": { "total": 4, "state": {
                    "passing": 4, "failing": 0, "blocked": 0, "not_run": 0, "uncovered": 0 } },
                "unit": { "total": 8, "state": {
                    "passing": 2, "failing": 1, "blocked": 1, "not_run": 3, "uncovered": 1 } },
            },
        });
        let block = verification_block(&report);
        assert_eq!(block["available"], true);
        assert_eq!(block["project_status"], "in_progress");
        let layers = block["layers"].as_array().unwrap();
        assert_eq!(layers[0]["layer"], "system");
        assert_eq!(layers[0]["status"], "verified");
        assert_eq!(layers[0]["percent"], 100.0);
        assert_eq!(layers[1]["layer"], "unit");
        assert_eq!(layers[1]["status"], "in_progress");
        assert_eq!(layers[1]["total"], 8);
        assert_eq!(layers[1]["passing"], 2);
        assert_eq!(layers[1]["failing"], 1);
        assert_eq!(layers[1]["not_run"], 3);
        assert_eq!(layers[1]["percent"], 25.0);
    }

    #[test]
    fn verification_block_handles_empty_layers_and_missing_sections() {
        let block = verification_block(&json!({
            "layer_statuses": { "requirement": "not_started" },
        }));
        let layers = block["layers"].as_array().unwrap();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0]["layer"], "requirement");
        assert_eq!(layers[0]["total"], 0);
        assert!(layers[0]["percent"].is_null());
        assert!(block["project_status"].is_null());
    }

    #[test]
    fn verification_unavailable_carries_the_reason() {
        let block = verification_unavailable("boom");
        assert_eq!(block["available"], false);
        assert_eq!(block["error"], "boom");
    }

    #[test]
    fn resolve_period_precedence_and_errors() {
        let today = parse_date("2026-10-08").unwrap();
        let scope = |period: Option<&str>, from: Option<&str>, to: Option<&str>| ReportScope {
            period: period.map(str::to_string),
            from: from.map(str::to_string),
            to: to.map(str::to_string),
            ..Default::default()
        };
        assert_eq!(
            resolve_period(&scope(Some("2026-W40"), None, None), today).unwrap(),
            Period::parse("2026-W40").unwrap()
        );
        assert_eq!(
            resolve_period(&scope(None, Some("2026-10-01"), Some("2026-10-03")), today)
                .unwrap()
                .days(),
            3
        );
        assert_eq!(
            resolve_period(&scope(None, None, None), today).unwrap(),
            week()
        );
        for bad in [
            scope(Some("2026-W41"), Some("2026-10-05"), None),
            scope(Some("2026-W41"), None, Some("2026-10-11")),
            scope(None, Some("2026-10-05"), None),
            scope(None, None, Some("2026-10-11")),
            scope(None, Some("2026-10-12"), Some("2026-10-05")),
            scope(None, Some("soon"), Some("2026-10-05")),
            scope(Some("whenever"), None, None),
        ] {
            assert!(resolve_period(&bad, today).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn empty_project_yields_empty_sections_not_errors() {
        let data = build(&[], &[], &[]);
        assert_eq!(data["period"]["start"], "2026-10-05");
        assert_eq!(data["period"]["end"], "2026-10-11");
        for key in [
            "completed",
            "in_progress",
            "blockers",
            "milestones",
            "next_week",
            "warnings",
        ] {
            assert!(data[key].as_array().unwrap().is_empty(), "{key}");
        }
        assert_eq!(data["stats"]["tasks_total"], 0);
    }

    /// `[].sum::<f64>()` is -0.0 on current Rust; it must not reach the report.
    #[test]
    fn empty_project_reports_positive_zero_hours() {
        let data = build(&[], &[], &[]);
        let json = data.to_string();
        assert!(!json.contains("-0.0"), "{json}");
        for v in [
            &data["stats"]["hours_this_week"],
            &data["stats"]["estimate_hours_total"],
            &data["stats"]["actual_hours_total"],
            &data["time_log"]["total_hours"],
        ] {
            assert!(v.as_f64().unwrap().is_sign_positive(), "{v}");
        }
    }

    #[test]
    fn round2_never_returns_negative_zero() {
        assert!(round2(-0.0).is_sign_positive());
        assert!(round2(-0.001).is_sign_positive());
        assert_eq!(round2(1.005 * 2.0), 2.01);
    }
}
