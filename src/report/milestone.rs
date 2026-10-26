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
//! requirements { available, total, implemented, tested, by_stage: { <dev_stage>: n },
//!              items: [ { id, title, dev_stage, status, impl, test } ] }
//! gui        { has_vrt, total, done, tasks: [ { id, title, requirement, status,
//!              golden, vrt, ai_visual } ] }
//! schedule_risks [ { id, title, status, due_date, milestone_date, overdue_days } ]
//! warnings   [ string ]
//! ```
//!
//! REQ-MS-REPORT-001 sections:
//!
//! - `requirements` lists the requirements whose effective milestone is this
//!   one (item `- milestone:`, else the document's), keyed by `dev_stage`
//!   (never set = `not_started`). `impl` / `test` say whether the item has
//!   code / test references. `available` is `false` (and a warning is added)
//!   when the requirement documents could not be read.
//! - `gui` summarises the milestone's `vrt`-labelled tasks (the `[VRT]` tasks
//!   `handoff_trace_tasks` generates, REQ-GUI-REQ-002); `has_vrt` is `false`
//!   when there are none, and the report omits the section. `golden` / `vrt` /
//!   `ai_visual` are the checked state of the task's three fixed
//!   done_criteria, rendered `PASS` or `PENDING`; `golden` is `true` when the
//!   first criterion is checked. `requirement` is the stable_id parsed from
//!   the `[VRT] <stable_id> <title>` task title.
//! - `schedule_risks` lists open tasks whose `due_date` falls after the
//!   milestone's planned date (`overdue_days` = due - planned, so +4 means the
//!   task is planned to finish 4 days after the milestone), worst first. It is
//!   empty when the milestone has no valid planned date.
//!
//! `status` is `achieved` / `achieved_late` once every task is done or
//! skipped (`actual_date` is the latest completion), else `overdue` /
//! `on_track` / `no_date` by the planned date against today, and `no_tasks`
//! for a milestone nothing is scheduled on. `variance_days` is
//! `actual_date - planned_date` (positive = late). A task's `delay_days` is
//! `completed - due` when finished, or `today - due` while it is open and
//! past due. `estimate_hours` / `actual_hours` / their variance are plain sums
//! over the milestone's tasks.

use std::collections::BTreeMap;

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

/// One requirement attached to the milestone, as far as the report is
/// concerned (see [`parse_requirements`]).
#[derive(Debug, Clone, PartialEq)]
pub struct MilestoneRequirement {
    pub stable_id: String,
    pub title: String,
    /// `None` = never set, counted as `not_started`.
    pub dev_stage: Option<String>,
    pub verification_status: String,
    pub has_impl: bool,
    pub has_test: bool,
}

/// `dev_stage` of a requirement that never had one set.
const UNSET_DEV_STAGE: &str = "not_started";
/// Label marking a VRT verification task (case-insensitive).
const VRT_LABEL: &str = "vrt";
/// Prefix of a generated VRT task's title: `[VRT] <stable_id> <title>`.
const VRT_TITLE_PREFIX: &str = "[VRT]";
/// Position of each fixed VRT done_criterion (REQ-GUI-REQ-002).
const VRT_CRITERION_GOLDEN: usize = 0;
const VRT_CRITERION_VRT: usize = 1;
const VRT_CRITERION_AI: usize = 2;

/// Parses the `items[]` of a `doc_req_list` response (already filtered to this
/// milestone) into [`MilestoneRequirement`]s.
pub fn parse_requirements(list: &Value) -> Result<Vec<MilestoneRequirement>> {
    let items = list
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("doc_req_list response has no items array"))?;
    items
        .iter()
        .map(|item| {
            let text = |key: &str| item.get(key).and_then(Value::as_str);
            let non_empty = |key: &str| {
                item.get(key)
                    .and_then(Value::as_array)
                    .is_some_and(|a| !a.is_empty())
            };
            Ok(MilestoneRequirement {
                stable_id: text("stable_id")
                    .ok_or_else(|| anyhow!("doc_req_list item has no stable_id"))?
                    .to_string(),
                title: text("title").unwrap_or_default().to_string(),
                dev_stage: text("dev_stage").map(str::to_string),
                verification_status: text("verification_status").unwrap_or_default().to_string(),
                has_impl: non_empty("impl_refs"),
                has_test: non_empty("test_refs"),
            })
        })
        .collect()
}

pub struct MilestoneInputs<'a> {
    pub name: &'a str,
    /// The milestone's `config.toml` entry, if it has one.
    pub config: Option<&'a MilestoneConfig>,
    /// Every task with its current status.
    pub tasks: &'a [(TaskData, String)],
    /// The requirements whose effective milestone is `name`, or why they
    /// could not be read (rendered as a warning, not a failed report).
    pub requirements: Result<&'a [MilestoneRequirement], &'a str>,
    pub today: NaiveDate,
}

fn requirements_block(
    requirements: &Result<&[MilestoneRequirement], &str>,
    warnings: &mut Vec<String>,
) -> Value {
    let reqs: &[MilestoneRequirement] = match requirements {
        Ok(reqs) => reqs,
        Err(reason) => {
            warnings.push(format!("requirement coverage is unavailable: {reason}"));
            return json!({
                "available": false, "total": 0, "implemented": 0, "tested": 0,
                "by_stage": {}, "items": [],
            });
        }
    };
    let mut by_stage: BTreeMap<&str, usize> = BTreeMap::new();
    for r in reqs {
        *by_stage
            .entry(r.dev_stage.as_deref().unwrap_or(UNSET_DEV_STAGE))
            .or_default() += 1;
    }
    let mut sorted: Vec<&MilestoneRequirement> = reqs.iter().collect();
    sorted.sort_by(|a, b| natural_cmp(&a.stable_id, &b.stable_id));
    json!({
        "available": true,
        "total": reqs.len(),
        "implemented": reqs.iter().filter(|r| r.has_impl).count(),
        "tested": reqs.iter().filter(|r| r.has_test).count(),
        "by_stage": by_stage,
        "items": sorted.iter().map(|r| json!({
            "id": r.stable_id,
            "title": r.title,
            "dev_stage": r.dev_stage.as_deref().unwrap_or(UNSET_DEV_STAGE),
            "status": r.verification_status,
            "impl": r.has_impl,
            "test": r.has_test,
        })).collect::<Vec<_>>(),
    })
}

fn is_vrt_task(data: &TaskData) -> bool {
    data.labels
        .iter()
        .any(|l| l.eq_ignore_ascii_case(VRT_LABEL))
}

fn gui_block(tasks: &[&(TaskData, String)]) -> Value {
    let rows: Vec<Value> = tasks
        .iter()
        .filter(|(d, _)| is_vrt_task(d))
        .map(|(data, status)| {
            let checked = |i: usize| data.done_criteria.get(i).is_some_and(|c| c.checked);
            let verdict = |i: usize| if checked(i) { "PASS" } else { "PENDING" };
            let requirement = data
                .title
                .strip_prefix(VRT_TITLE_PREFIX)
                .and_then(|rest| rest.split_whitespace().next());
            json!({
                "id": data.id,
                "title": data.title,
                "requirement": requirement,
                "status": status,
                "golden": checked(VRT_CRITERION_GOLDEN),
                "vrt": verdict(VRT_CRITERION_VRT),
                "ai_visual": verdict(VRT_CRITERION_AI),
            })
        })
        .collect();
    let done = rows.iter().filter(|r| r["status"] == "done").count();
    json!({
        "has_vrt": !rows.is_empty(),
        "total": rows.len(),
        "done": done,
        "tasks": rows,
    })
}

/// Open tasks due after the milestone's planned date, worst first.
fn schedule_risks(tasks: &[&(TaskData, String)], planned: Option<NaiveDate>) -> Vec<Value> {
    let Some(planned) = planned else {
        return Vec::new();
    };
    let mut risks: Vec<(i64, Value)> = tasks
        .iter()
        .filter(|(_, status)| !is_terminal_status(status))
        .filter_map(|(data, status)| {
            let due_raw = data.schedule.as_ref()?.due_date.as_deref()?;
            let due = parse_date(due_raw).ok()?;
            let overdue = (due - planned).num_days();
            (overdue > 0).then(|| {
                (
                    overdue,
                    json!({
                        "id": data.id,
                        "title": data.title,
                        "status": status,
                        "due_date": due_raw,
                        "milestone_date": planned.to_string(),
                        "overdue_days": overdue,
                    }),
                )
            })
        })
        .collect();
    risks.sort_by_key(|r| std::cmp::Reverse(r.0));
    risks.into_iter().map(|(_, v)| v).collect()
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

    let requirements = requirements_block(&inputs.requirements, &mut warnings);
    let gui = gui_block(&tasks);
    let risks = schedule_risks(&tasks, planned);

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
        "requirements": requirements,
        "gui": gui,
        "schedule_risks": risks,
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
        build_with(name, cfg, tasks, Ok(&[]), today)
    }

    fn build_with(
        name: &str,
        cfg: Option<&MilestoneConfig>,
        tasks: &[(TaskData, String)],
        requirements: Result<&[MilestoneRequirement], &str>,
        today: &str,
    ) -> Result<Value> {
        build_milestone_data(&MilestoneInputs {
            name,
            config: cfg,
            tasks,
            requirements,
            today: d(today),
        })
    }

    fn req(id: &str, stage: Option<&str>, has_impl: bool, has_test: bool) -> MilestoneRequirement {
        MilestoneRequirement {
            stable_id: id.into(),
            title: format!("Req {id}"),
            dev_stage: stage.map(str::to_string),
            verification_status: "pending".into(),
            has_impl,
            has_test,
        }
    }

    fn vrt_task(id: &str, status: &str, checked: [bool; 3]) -> (TaskData, String) {
        task(id, status, "beta", |t| {
            t.title = format!("[VRT] REQ-GUI-001 Login screen {id}");
            t.labels = vec!["VRT".into(), "gui-verification".into()];
            t.done_criteria = checked
                .iter()
                .map(|&c| crate::storage::tasks::DoneCriterion {
                    item: "c".into(),
                    checked: c,
                })
                .collect();
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

    #[test]
    fn requirement_coverage_counts_stages_and_refs() {
        let cfg = config(Some("2026-12-01"));
        let reqs = [
            req("REQ-10", Some("implemented"), true, false),
            req("REQ-2", None, false, false),
            req("REQ-3", Some("implemented"), true, true),
        ];
        let data = build_with("beta", Some(&cfg), &[], Ok(&reqs), "2026-10-08").unwrap();
        let r = &data["requirements"];
        assert_eq!(r["available"], true);
        assert_eq!(r["total"], 3);
        assert_eq!(r["implemented"], 2);
        assert_eq!(r["tested"], 1);
        assert_eq!(r["by_stage"], json!({"implemented": 2, "not_started": 1}));
        let ids: Vec<&str> = r["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["REQ-2", "REQ-3", "REQ-10"], "natural order");
        assert_eq!(r["items"][0]["dev_stage"], "not_started");
        assert_eq!(r["items"][2]["impl"], true);
        assert_eq!(r["items"][2]["test"], false);
    }

    #[test]
    fn unreadable_requirements_are_a_warning_not_a_failure() {
        let cfg = config(None);
        let data = build_with("beta", Some(&cfg), &[], Err("broken doc"), "2026-10-08").unwrap();
        assert_eq!(data["requirements"]["available"], false);
        let warnings = data["warnings"].as_array().unwrap();
        assert!(warnings[0].as_str().unwrap().contains("broken doc"));
    }

    #[test]
    fn parse_requirements_reads_doc_req_list_items() {
        let list = json!({"items": [
            {"stable_id": "REQ-1", "title": "T", "dev_stage": "tested",
             "verification_status": "verified",
             "impl_refs": [{"path": "a.rs"}], "test_refs": []},
            {"stable_id": "REQ-2", "title": "U", "verification_status": "pending",
             "impl_refs": [], "test_refs": []},
        ], "total": 2});
        let reqs = parse_requirements(&list).unwrap();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].dev_stage.as_deref(), Some("tested"));
        assert!(reqs[0].has_impl && !reqs[0].has_test);
        assert_eq!(reqs[1].dev_stage, None);
        assert!(parse_requirements(&json!({})).is_err());
        assert!(parse_requirements(&json!({"items": [{}]})).is_err());
    }

    #[test]
    fn gui_summary_covers_only_vrt_labelled_milestone_tasks() {
        let cfg = config(Some("2026-12-01"));
        let mut other = vrt_task("v9", "todo", [false; 3]);
        other.0.schedule.as_mut().unwrap().milestone = Some("other".into());
        let tasks = [
            vrt_task("v1", "done", [true, true, true]),
            vrt_task("v2", "in_progress", [true, false, false]),
            task("t1", "todo", "beta", |_| {}),
            other,
        ];
        let data = build("beta", Some(&cfg), &tasks, "2026-10-08").unwrap();
        let gui = &data["gui"];
        assert_eq!(gui["has_vrt"], true);
        assert_eq!(gui["total"], 2);
        assert_eq!(gui["done"], 1);
        let v1 = &gui["tasks"][0];
        assert_eq!(v1["requirement"], "REQ-GUI-001");
        assert_eq!(v1["golden"], true);
        assert_eq!(v1["vrt"], "PASS");
        assert_eq!(v1["ai_visual"], "PASS");
        let v2 = &gui["tasks"][1];
        assert_eq!(v2["golden"], true);
        assert_eq!(v2["vrt"], "PENDING");
        assert_eq!(v2["ai_visual"], "PENDING");
    }

    #[test]
    fn gui_summary_is_empty_without_vrt_tasks() {
        let cfg = config(None);
        let tasks = [task("t1", "todo", "beta", |_| {})];
        let data = build("beta", Some(&cfg), &tasks, "2026-10-08").unwrap();
        assert_eq!(data["gui"]["has_vrt"], false);
        assert_eq!(data["gui"]["total"], 0);
        // A criterion list shorter than three leaves the missing ones pending.
        let short = [task("v1", "todo", "beta", |t| {
            t.labels = vec!["vrt".into()];
            t.title = "no prefix".into();
        })];
        let data = build("beta", Some(&cfg), &short, "2026-10-08").unwrap();
        assert_eq!(data["gui"]["tasks"][0]["vrt"], "PENDING");
        assert!(data["gui"]["tasks"][0]["requirement"].is_null());
    }

    #[test]
    fn schedule_risks_list_open_tasks_due_after_the_milestone() {
        let cfg = config(Some("2026-12-01"));
        let due = |d: &'static str| {
            move |t: &mut TaskData| t.schedule.as_mut().unwrap().due_date = Some(d.into())
        };
        let tasks = [
            task("t1", "todo", "beta", due("2026-12-05")),
            task("t2", "in_progress", "beta", due("2026-12-10")),
            task("t3", "todo", "beta", due("2026-12-01")),
            task("t4", "done", "beta", due("2026-12-20")),
            task("t5", "todo", "beta", |_| {}),
            task("t6", "todo", "beta", due("not-a-date")),
        ];
        let data = build("beta", Some(&cfg), &tasks, "2026-10-08").unwrap();
        let risks = data["schedule_risks"].as_array().unwrap();
        let ids: Vec<&str> = risks.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(
            ids,
            ["t2", "t1"],
            "worst first; on-time, done, undated skipped"
        );
        assert_eq!(risks[1]["overdue_days"], 4);
        assert_eq!(risks[1]["milestone_date"], "2026-12-01");
        assert_eq!(risks[1]["due_date"], "2026-12-05");
    }

    #[test]
    fn schedule_risks_need_a_valid_milestone_date() {
        let tasks = [task("t1", "todo", "beta", |t| {
            t.schedule.as_mut().unwrap().due_date = Some("2026-12-05".into())
        })];
        let data = build("beta", Some(&config(None)), &tasks, "2026-10-08").unwrap();
        assert!(data["schedule_risks"].as_array().unwrap().is_empty());
        let data = build("beta", None, &tasks, "2026-10-08").unwrap();
        assert!(data["schedule_risks"].as_array().unwrap().is_empty());
    }

    #[test]
    fn empty_milestone_has_empty_new_sections() {
        let cfg = config(Some("2026-12-01"));
        let data = build("beta", Some(&cfg), &[], "2026-10-08").unwrap();
        assert_eq!(data["requirements"]["total"], 0);
        assert_eq!(data["gui"]["has_vrt"], false);
        assert!(data["schedule_risks"].as_array().unwrap().is_empty());
    }
}
