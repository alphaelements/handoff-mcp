//! Milestone report data (R6: FR-531 / SPEC-531).
//!
//! [`build_milestone_data`] is a pure function from the project's tasks (and
//! the milestone's `config.toml` entry) to the JSON `milestone.md.hbs`
//! renders as `data`. The project-wide verification state comes from the
//! trace graph and is attached by the caller (`handoff_report`) as
//! `verification`.
//!
//! The milestone's tasks are those whose `schedule.milestone` equals
//! `scope.milestone`. Output shape (hours rounded to 2 decimals; absent
//! values are `null`):
//!
//! ```text
//! milestone  { name, description, planned_date, actual_date, variance_days, status }
//! stats      { total, done, skipped, open, percent, estimate_hours, actual_hours,
//!              variance_hours, variance_percent, overdue_open }
//! tasks      [ { id, title, status, assignee, due_date, completed_at, estimate_hours,
//!                actual_hours, variance_hours, delay_days } ]
//! quality    { bugs: { total, open, closed }, open_bugs: [ { id, title, status,
//!              priority, assignee } ] }
//! warnings   [ string ]
//! ```
//!
//! `status` is `achieved` / `achieved_late` once every task is done or
//! skipped (`actual_date` is the latest completion), else `overdue` /
//! `on_track` / `no_date` by the planned date against today, and `no_tasks`
//! for a milestone nothing is scheduled on. `variance_days` is
//! `actual_date - planned_date` (positive = late). A task's `delay_days` is
//! `completed - due` when finished, or `today - due` while it is open and
//! past due. `estimate_hours` / `actual_hours` / their variance are plain sums
//! over the milestone's tasks.

use anyhow::{anyhow, bail, Result};
use chrono::NaiveDate;
use serde_json::{json, Value};

use super::defect::is_bug;
use super::period::parse_date;
use super::weekly::{natural_cmp, percent, round2, ts_date};
use super::{ReportScope, ReportType};
use crate::storage::config::MilestoneConfig;
use crate::storage::tasks::{is_terminal_status, TaskData};

/// A milestone report is about one milestone only; any other scope field
/// would be silently ignored, so it is an error instead. Returns the
/// milestone name.
pub fn validate_scope(scope: &ReportScope) -> Result<&str> {
    scope.require_only(
        ReportType::Milestone,
        &["milestone"],
        "only scope.milestone and scope.label apply",
    )?;
    scope
        .milestone
        .as_deref()
        .ok_or_else(|| anyhow!("scope.milestone is required for a milestone report"))
}

pub struct MilestoneInputs<'a> {
    pub name: &'a str,
    /// The milestone's `config.toml` entry, if it has one.
    pub config: Option<&'a MilestoneConfig>,
    /// Every task with its current status.
    pub tasks: &'a [(TaskData, String)],
    pub today: NaiveDate,
}

pub fn build_milestone_data(inputs: &MilestoneInputs) -> Result<Value> {
    let mut warnings: Vec<String> = Vec::new();
    let mut tasks: Vec<&(TaskData, String)> = inputs
        .tasks
        .iter()
        .filter(|(data, _)| {
            data.schedule.as_ref().and_then(|s| s.milestone.as_deref()) == Some(inputs.name)
        })
        .collect();
    if tasks.is_empty() && inputs.config.is_none() {
        bail!(
            "Milestone '{}' not found (it is not in config.toml and no task is scheduled on it)",
            inputs.name
        );
    }
    tasks.sort_by(|a, b| natural_cmp(&a.0.id, &b.0.id));

    let planned_raw = inputs.config.and_then(|c| c.date.clone());
    let planned = planned_raw
        .as_deref()
        .and_then(|raw| match parse_date(raw) {
            Ok(date) => Some(date),
            Err(_) => {
                warnings.push(format!(
                "milestone date '{raw}' is not a YYYY-MM-DD date; no schedule comparison was made"
            ));
                None
            }
        });

    let total = tasks.len();
    let terminal = tasks.iter().filter(|(_, s)| is_terminal_status(s)).count();
    let done = tasks.iter().filter(|(_, s)| s == "done").count();
    let all_finished = total > 0 && terminal == total;

    let completed_date =
        |data: &TaskData| -> Option<NaiveDate> { data.completed_at.as_deref().and_then(ts_date) };
    let actual_date = if all_finished {
        tasks.iter().filter_map(|(d, _)| completed_date(d)).max()
    } else {
        None
    };
    let variance_days = match (actual_date, planned) {
        (Some(actual), Some(plan)) => Some((actual - plan).num_days()),
        _ => None,
    };
    let status = if total == 0 {
        "no_tasks"
    } else if all_finished {
        match variance_days {
            Some(days) if days > 0 => "achieved_late",
            _ => "achieved",
        }
    } else {
        match planned {
            Some(plan) if inputs.today > plan => "overdue",
            Some(_) => "on_track",
            None => "no_date",
        }
    };

    let hours = |pick: fn(&crate::storage::tasks::Schedule) -> Option<f64>| -> f64 {
        tasks
            .iter()
            .filter_map(|(d, _)| d.schedule.as_ref().and_then(pick))
            .sum()
    };
    let estimate = hours(|s| s.estimate_hours);
    let actual = hours(|s| s.actual_hours);

    let mut overdue_open = 0usize;
    let rows: Vec<Value> = tasks
        .iter()
        .map(|(data, status)| {
            let schedule = data.schedule.as_ref();
            let due = schedule
                .and_then(|s| s.due_date.as_deref())
                .and_then(|d| parse_date(d).ok());
            let finished = is_terminal_status(status);
            if !finished && due.is_some_and(|d| d < inputs.today) {
                overdue_open += 1;
            }
            let delay_days = match (finished, due) {
                (true, Some(due)) => completed_date(data).map(|c| (c - due).num_days()),
                (false, Some(due)) if due < inputs.today => Some((inputs.today - due).num_days()),
                _ => None,
            };
            let estimate = schedule.and_then(|s| s.estimate_hours);
            let actual = schedule.and_then(|s| s.actual_hours);
            json!({
                "id": data.id,
                "title": data.title,
                "status": status,
                "assignee": data.assignee,
                "due_date": schedule.and_then(|s| s.due_date.clone()),
                "completed_at": data.completed_at,
                "estimate_hours": estimate.map(round2),
                "actual_hours": actual.map(round2),
                "variance_hours": match (estimate, actual) {
                    (Some(e), Some(a)) => Some(round2(a - e)),
                    _ => None,
                },
                "delay_days": delay_days,
            })
        })
        .collect();

    let bugs: Vec<&&(TaskData, String)> = tasks.iter().filter(|(d, _)| is_bug(d)).collect();
    let open_bugs: Vec<Value> = bugs
        .iter()
        .filter(|(_, s)| !is_terminal_status(s))
        .map(|(data, status)| {
            json!({
                "id": data.id,
                "title": data.title,
                "status": status,
                "priority": data.priority,
                "assignee": data.assignee,
            })
        })
        .collect();

    Ok(json!({
        "milestone": {
            "name": inputs.name,
            "description": inputs.config.and_then(|c| c.description.clone()),
            "planned_date": planned_raw,
            "actual_date": actual_date.map(|d| d.to_string()),
            "variance_days": variance_days,
            "status": status,
        },
        "stats": {
            "total": total,
            "done": done,
            "skipped": terminal - done,
            "open": total - terminal,
            "percent": percent(terminal as f64, total as f64),
            "estimate_hours": round2(estimate),
            "actual_hours": round2(actual),
            "variance_hours": round2(actual - estimate),
            "variance_percent": percent(actual - estimate, estimate),
            "overdue_open": overdue_open,
        },
        "tasks": rows,
        "quality": {
            "bugs": {
                "total": bugs.len(),
                "open": open_bugs.len(),
                "closed": bugs.len() - open_bugs.len(),
            },
            "open_bugs": open_bugs,
        },
        "warnings": warnings,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::tasks::Schedule;
    use std::collections::HashMap;

    fn d(s: &str) -> NaiveDate {
        parse_date(s).unwrap()
    }

    fn task(
        id: &str,
        status: &str,
        milestone: &str,
        f: impl FnOnce(&mut TaskData),
    ) -> (TaskData, String) {
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
            schedule: Some(Schedule {
                milestone: Some(milestone.into()),
                ..Default::default()
            }),
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

    fn config(date: Option<&str>) -> MilestoneConfig {
        MilestoneConfig {
            date: date.map(str::to_string),
            description: Some("desc".into()),
            ..Default::default()
        }
    }

    fn build(
        name: &str,
        cfg: Option<&MilestoneConfig>,
        tasks: &[(TaskData, String)],
        today: &str,
    ) -> Result<Value> {
        build_milestone_data(&MilestoneInputs {
            name,
            config: cfg,
            tasks,
            today: d(today),
        })
    }

    #[test]
    fn scope_requires_exactly_the_milestone() {
        let ok = ReportScope {
            milestone: Some("beta".into()),
            label: Some("x".into()),
            ..Default::default()
        };
        assert_eq!(validate_scope(&ok).unwrap(), "beta");
        let missing = ReportScope::default();
        assert!(validate_scope(&missing)
            .unwrap_err()
            .to_string()
            .contains("scope.milestone is required"));
        let extra = ReportScope {
            milestone: Some("beta".into()),
            period: Some("2026-10".into()),
            ..Default::default()
        };
        assert!(validate_scope(&extra)
            .unwrap_err()
            .to_string()
            .contains("scope.period"));
    }

    #[test]
    fn unknown_milestone_is_an_error_but_a_configured_empty_one_is_not() {
        let tasks = [task("t1", "todo", "other", |_| {})];
        assert!(build("beta", None, &tasks, "2026-10-08").is_err());
        let cfg = config(Some("2026-12-01"));
        let data = build("beta", Some(&cfg), &tasks, "2026-10-08").unwrap();
        assert_eq!(data["milestone"]["status"], "no_tasks");
        assert_eq!(data["stats"]["total"], 0);
        assert!(data["stats"]["percent"].is_null());
    }

    #[test]
    fn finished_milestone_reports_actual_date_and_variance() {
        let cfg = config(Some("2026-09-30"));
        let tasks = [
            task("t1", "done", "beta", |t| {
                t.completed_at = Some("2026-09-28T09:00:00+00:00".into())
            }),
            task("t2", "done", "beta", |t| {
                t.completed_at = Some("2026-10-02T09:00:00+00:00".into())
            }),
            task("t3", "skipped", "beta", |_| {}),
            task("x", "todo", "other", |_| {}),
        ];
        let data = build("beta", Some(&cfg), &tasks, "2026-10-08").unwrap();
        assert_eq!(data["milestone"]["actual_date"], "2026-10-02");
        assert_eq!(data["milestone"]["variance_days"], 2);
        assert_eq!(data["milestone"]["status"], "achieved_late");
        assert_eq!(data["stats"]["total"], 3);
        assert_eq!(data["stats"]["done"], 2);
        assert_eq!(data["stats"]["skipped"], 1);
        assert_eq!(data["stats"]["percent"], 100.0);

        let early = [task("t1", "done", "beta", |t| {
            t.completed_at = Some("2026-09-01T00:00:00+00:00".into())
        })];
        let data = build("beta", Some(&cfg), &early, "2026-10-08").unwrap();
        assert_eq!(data["milestone"]["status"], "achieved");
        assert_eq!(data["milestone"]["variance_days"], -29);
    }

    #[test]
    fn open_milestone_is_overdue_or_on_track_by_the_planned_date() {
        let tasks = [task("t1", "in_progress", "beta", |_| {})];
        let past = config(Some("2026-10-01"));
        let data = build("beta", Some(&past), &tasks, "2026-10-08").unwrap();
        assert_eq!(data["milestone"]["status"], "overdue");
        assert!(data["milestone"]["actual_date"].is_null());
        let future = config(Some("2026-10-08"));
        let data = build("beta", Some(&future), &tasks, "2026-10-08").unwrap();
        assert_eq!(
            data["milestone"]["status"], "on_track",
            "the planned day itself is not late"
        );
        let data = build("beta", Some(&config(None)), &tasks, "2026-10-08").unwrap();
        assert_eq!(data["milestone"]["status"], "no_date");
        // A task-only milestone (not in config.toml) has no planned date.
        let data = build("beta", None, &tasks, "2026-10-08").unwrap();
        assert_eq!(data["milestone"]["status"], "no_date");
    }

    #[test]
    fn unparseable_planned_date_is_kept_but_warned_about() {
        let cfg = config(Some("Q4"));
        let tasks = [task("t1", "todo", "beta", |_| {})];
        let data = build("beta", Some(&cfg), &tasks, "2026-10-08").unwrap();
        assert_eq!(data["milestone"]["planned_date"], "Q4");
        assert_eq!(data["milestone"]["status"], "no_date");
        assert_eq!(data["warnings"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn hours_delays_and_bugs_are_aggregated_per_task() {
        let cfg = config(Some("2026-12-01"));
        let tasks = [
            task("t1", "done", "beta", |t| {
                t.completed_at = Some("2026-09-28T09:00:00+00:00".into());
                let s = t.schedule.as_mut().unwrap();
                s.due_date = Some("2026-09-25".into());
                s.estimate_hours = Some(4.0);
                s.actual_hours = Some(6.0);
            }),
            task("t2", "in_progress", "beta", |t| {
                let s = t.schedule.as_mut().unwrap();
                s.due_date = Some("2026-10-01".into());
                s.estimate_hours = Some(2.0);
            }),
            task("b1", "todo", "beta", |t| {
                t.labels = vec!["bug".into()];
                t.priority = Some("high".into());
            }),
            task("b2", "done", "beta", |t| {
                t.labels = vec!["bug".into(), "bug:fix".into()]
            }),
            task("b10", "todo", "beta", |t| t.labels = vec!["feature".into()]),
        ];
        let data = build("beta", Some(&cfg), &tasks, "2026-10-08").unwrap();
        assert_eq!(data["stats"]["estimate_hours"], 6.0);
        assert_eq!(data["stats"]["actual_hours"], 6.0);
        assert_eq!(data["stats"]["variance_hours"], 0.0);
        assert_eq!(data["stats"]["overdue_open"], 1);
        let rows = data["tasks"].as_array().unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["b1", "b2", "b10", "t1", "t2"], "natural id order");
        let t1 = rows.iter().find(|r| r["id"] == "t1").unwrap();
        assert_eq!(t1["delay_days"], 3);
        assert_eq!(t1["variance_hours"], 2.0);
        let t2 = rows.iter().find(|r| r["id"] == "t2").unwrap();
        assert_eq!(t2["delay_days"], 7, "open and past due: today - due");
        assert!(t2["variance_hours"].is_null());
        assert_eq!(
            data["quality"]["bugs"],
            json!({"total": 2, "open": 1, "closed": 1})
        );
        assert_eq!(data["quality"]["open_bugs"][0]["id"], "b1");
        assert_eq!(data["quality"]["open_bugs"][0]["priority"], "high");
    }
}
