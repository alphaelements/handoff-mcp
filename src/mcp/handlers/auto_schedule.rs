use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Result;
use chrono::{Duration, NaiveDate, Utc};
use serde_json::{json, Value};
use toml_edit::DocumentMut;

use super::HandlerContext;
use crate::scheduling::calendar::{
    advance_by_hours, next_work_day, parse_assignee_calendars, parse_project_calendar, Calendar,
    EPSILON,
};
use crate::storage::agents::list_agents;
use crate::storage::tasks::*;

pub fn handle(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;
    let config_path = handoff.join("config.toml");
    let tasks_dir = handoff.join("tasks");

    let dry_run = arguments
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let assignee_filter = arguments.get("assignee_filter").and_then(|v| v.as_str());

    let calendar = parse_project_calendar(&config_path)?;
    let assignee_calendars = parse_assignee_calendars(&config_path)?;

    let (tree, _) = build_task_index(&tasks_dir, u32::MAX)?;

    // REQ-MS-SCHED-002: milestone `depends_on` graph. A cycle is rejected up
    // front, before any task is placed or written.
    let (config, config_warning) = crate::storage::config::read_config_lenient(handoff);
    let milestone_deps = milestone_dependencies(config.as_ref());
    let (default_estimate, default_estimate_warning) = default_estimate_hours(config.as_ref());
    let mut missing_estimate_count = 0usize;
    let milestone_levels = milestone_levels(&milestone_deps).map_err(|cycle| {
        anyhow::anyhow!(
            "Circular milestone dependency detected: {} (check [milestones.*].depends_on)",
            cycle.join(" \u{2192} ")
        )
    })?;

    // Collect schedulable tasks (non-terminal, not pinned)
    let mut schedulable: Vec<SchedulableTask> = Vec::new();
    collect_schedulable(&tree, &tasks_dir, assignee_filter, &mut schedulable)?;

    // SPEC-SI-009: a task with no order of its own takes its plan SubItem's.
    let plan_order_warnings = apply_plan_orders(handoff, &mut schedulable);

    // Sort by dependencies then order
    sort_by_deps(&mut schedulable);
    // A milestone's tasks are placed after those of every milestone it depends
    // on (stable, so the order above is kept within a level).
    schedulable.sort_by_key(|t| {
        t.milestone
            .as_deref()
            .and_then(|m| milestone_levels.get(m))
            .copied()
            .unwrap_or(0)
    });

    // Non-terminal tasks this run leaves where they are (pinned, or outside
    // `assignee_filter`). Their due dates still count towards when a
    // predecessor milestone finishes.
    let fixed_tasks = collect_fixed_milestone_tasks(&tasks_dir, &schedulable)?;
    let mut milestone_latest_due: HashMap<String, NaiveDate> = HashMap::new();
    for fixed in &fixed_tasks {
        if let Some(due) = fixed.due {
            record_milestone_due(&mut milestone_latest_due, &fixed.milestone, due);
        }
    }

    // Schedule each task. The anchor defaults to today (UTC) but can be pinned
    // via `start_date` for planning a future start (and for deterministic tests).
    let start_date = match arguments.get("start_date").and_then(|v| v.as_str()) {
        Some(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map_err(|_| anyhow::anyhow!("Invalid start_date '{s}' (expected YYYY-MM-DD)"))?,
        None => {
            let today = Utc::now().format("%Y-%m-%d").to_string();
            NaiveDate::parse_from_str(&today, "%Y-%m-%d")?
        }
    };

    // Open slots per assignee: each is a date and the working hours still open
    // on it. Small tasks pack into a partly filled day instead of taking one
    // each; with `max_parallel_tasks` > 1 an owner may hold several slots, so a
    // day's leftover hours stay usable after a larger task moved on.
    let max_parallel = max_parallel_tasks(config.as_ref());
    let mut assignee_next: HashMap<String, Vec<Slot>> = HashMap::new();
    let mut project_next = vec![full_day(next_work_day(start_date, &calendar), &calendar)];
    let mut computed_due: HashMap<String, NaiveDate> = HashMap::new();
    let mut changes: Vec<Value> = Vec::new();
    let mut placed: Vec<PlacedTask> = Vec::with_capacity(schedulable.len());

    for task in &schedulable {
        let cal = task
            .assignee
            .as_ref()
            .and_then(|a| assignee_calendars.get(a))
            .unwrap_or(&calendar);

        let slots: &mut Vec<Slot> = match &task.assignee {
            Some(a) => assignee_next
                .entry(a.clone())
                .or_insert_with(|| vec![full_day(next_work_day(start_date, cal), cal)]),
            None => &mut project_next,
        };

        // Also respect dependency completion dates
        // (stored due dates, plus those computed earlier in this run: packing
        // would otherwise put a task on the same day as its not-yet-saved
        // predecessor).
        let dep_earliest = task
            .dep_due_dates
            .iter()
            .filter_map(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
            .chain(
                task.dependencies
                    .iter()
                    .filter_map(|dep| computed_due.get(dep).copied()),
            )
            .max()
            .map(|d| d + Duration::days(1))
            .unwrap_or(start_date);

        // REQ-MS-SCHED-002: not before the milestones this one depends on end.
        let milestone_earliest = task
            .milestone
            .as_deref()
            .and_then(|m| predecessor_floor(&milestone_deps, &milestone_latest_due, m))
            .unwrap_or(start_date);

        let constraint_floor = dep_earliest.max(milestone_earliest);

        // Calculate end date based on estimate_hours, drawing each day's capacity
        // from the calendar's per-day hours (day_hours overrides).
        let hours = match task.estimate_hours {
            Some(h) => h,
            None => {
                missing_estimate_count += 1;
                default_estimate
            }
        };
        let placement = select_slot(slots, constraint_floor, hours, cal, max_parallel);
        let (actual_start, initial_capacity) = (placement.start, placement.capacity);
        let (actual_end, remaining) = advance_by_hours(actual_start, hours, initial_capacity, cal);

        if let Some(m) = &task.milestone {
            record_milestone_due(&mut milestone_latest_due, m, actual_end);
        }
        computed_due.insert(task.id.clone(), actual_end);
        placed.push(PlacedTask {
            id: task.id.clone(),
            milestone: task.milestone.clone(),
            estimate_hours: hours,
            due: actual_end,
        });

        let new_start = actual_start.format("%Y-%m-%d").to_string();
        let new_due = actual_end.format("%Y-%m-%d").to_string();

        if task.old_start.as_deref() != Some(&new_start)
            || task.old_due.as_deref() != Some(&new_due)
        {
            changes.push(json!({
                "task_id": task.id,
                "old_start": task.old_start,
                "new_start": new_start,
                "old_due": task.old_due,
                "new_due": new_due,
            }));
        }

        // Leave the day's unused hours for the next task, or move on to the
        // next work day with its full capacity when this one used the day up.
        let next = if remaining > EPSILON {
            (actual_end, remaining)
        } else {
            // Never onto a day another slot of this owner already draws on.
            frontier_day(slots, placement.slot, actual_end + Duration::days(1), cal)
        };
        match placement.slot {
            Some(i) => slots[i] = next,
            None => slots.push(next),
        }

        // Apply if not dry_run
        if !dry_run {
            if let Some(task_dir) = find_task_dir_by_id(&tasks_dir, &task.id)? {
                if let Some((mut data, status)) = read_task(&task_dir)? {
                    let schedule = data.schedule.get_or_insert_with(Default::default);
                    // DS-P4-008: record the first computed plan as the baseline,
                    // never overwriting one that is already set (reset it with
                    // `update_task(schedule={baseline_*: ""})` to re-record).
                    schedule
                        .baseline_start
                        .get_or_insert_with(|| new_start.clone());
                    schedule.baseline_due.get_or_insert_with(|| new_due.clone());
                    schedule.start_date = Some(new_start);
                    schedule.due_date = Some(new_due);
                    data.updated_at = Some(Utc::now().to_rfc3339());

                    // t374/t375: write-then-remove, never remove-then-write —
                    // see `write_task_transition`'s doc comment
                    // (src/storage/tasks.rs). `status` is unchanged here, so
                    // this is a same-name atomic overwrite (no removal).
                    write_task_transition(&task_dir, &status, &status, &data)?;
                }
            }
        }
    }

    // Summarize the assignee calendars that fed the computation (applied conditions).
    let assignee_capacity: serde_json::Map<String, Value> = assignee_calendars
        .iter()
        .map(|(k, c)| {
            (
                k.clone(),
                json!({
                    "work_hours_per_day": c.work_hours_per_day,
                    "closed_weekdays": c.closed_weekdays,
                }),
            )
        })
        .collect();

    // When changes are actually applied, record them on the active session(s) so
    // the decision is part of the audit trail, not just this response.
    let mut decision_recorded_in = 0usize;
    if !dry_run && !changes.is_empty() {
        let assignees_touched: std::collections::BTreeSet<&str> = schedulable
            .iter()
            .filter_map(|t| t.assignee.as_deref())
            .collect();
        let summary = format!(
            "Auto-scheduled {} task(s) across {} assignee(s) from {}",
            changes.len(),
            assignees_touched.len().max(1),
            start_date.format("%Y-%m-%d")
        );
        let decision = json!({
            "decision": summary,
            "reason": "handoff_auto_schedule applied computed start/due dates",
            "confidence": "confirmed",
        });
        let sessions_dir = handoff.join("sessions");
        decision_recorded_in = crate::storage::sessions::append_decision_to_active_sessions(
            &sessions_dir,
            decision,
            None,
        )?;
    }

    // Agent capacity: how many tasks each registered agent currently holds
    // vs. how many it may claim concurrently (t250.6, FR-2.6). `max_concurrent`
    // is read from `[worktree.session_loop].max_concurrent_wts`, defaulting to
    // 4 to match the documented default (wiki/200 §3.6) when unset or on a
    // config parse failure (fail-closed to the conservative default rather
    // than reporting unlimited capacity).
    let max_concurrent = parse_max_concurrent_wts(&config_path);
    let agent_records = list_agents(handoff).unwrap_or_default();
    let agent_capacity: Vec<Value> = agent_records
        .iter()
        .map(|a| {
            let claimed = a.claimed_tasks.len();
            let available = max_concurrent.saturating_sub(claimed as u32);
            json!({
                "agent_id": a.agent_id,
                "claimed": claimed,
                "max_concurrent": max_concurrent,
                "available": available,
            })
        })
        .collect();

    // Ready tasks: todo tasks whose dependencies are all done, sorted by
    // priority (high > medium > low > unspecified), then by order/id for a
    // deterministic tie-break.
    let ready_tasks = compute_ready_tasks(&tasks_dir)?;

    // REQ-MS-SCHED-001: check the computed due dates against milestone dates.
    let deadlines = milestone_deadlines(config.as_ref());
    let milestone_report = milestone_report(&placed, &deadlines);
    let mut warnings = plan_order_warnings;
    warnings.extend(milestone_report.warnings);
    warnings.extend(default_estimate_warning);
    warnings.extend(missing_estimate_warning(
        missing_estimate_count,
        default_estimate,
    ));
    // REQ-MS-SCHED-002: soft constraint — fixed tasks that start before a
    // predecessor milestone ends are kept, but flagged.
    warnings.extend(fixed_task_dependency_warnings(
        &fixed_tasks,
        &milestone_deps,
        &milestone_latest_due,
    ));
    if !milestone_report.summary.is_empty() {
        crate::storage::config::push_config_warning(&mut warnings, config_warning);
    }

    let mut result = json!({
        "dry_run": dry_run,
        "scheduled_count": schedulable.len(),
        "changed_count": changes.len(),
        "changes": changes,
        "decision_recorded_in_sessions": decision_recorded_in,
        "calendar_config": {
            "work_hours_per_day": calendar.work_hours_per_day,
            "closed_weekdays": calendar.closed_weekdays,
            "day_hours": calendar.day_hours,
            "max_utilization": calendar.max_utilization,
        },
        "assignee_capacity": assignee_capacity,
        "agent_capacity": agent_capacity,
        "ready_tasks": ready_tasks,
        // REQ-PLA-005: stateless design — `doc_save`/`update_task` judge
        // staleness per change, so right after a (re)schedule it is never stale.
        "schedule_stale": false,
        // Per-milestone task count / estimate / latest due / overdue flag for
        // the tasks scheduled in this run (pinned and terminal tasks are not
        // part of the computation and are not counted).
        "milestone_summary": milestone_report.summary,
    });
    if !warnings.is_empty() {
        result["warnings"] = json!(warnings);
    }

    serde_json::to_string_pretty(&result).map_err(Into::into)
}

/// An open scheduling slot: a work day and the hours still free on it.
type Slot = (NaiveDate, f64);

/// `date` paired with the hours auto-schedule may pack into it
/// (working hours scaled by `max_utilization`).
fn full_day(date: NaiveDate, cal: &Calendar) -> Slot {
    (date, cal.effective_capacity(&date))
}

/// `calendar.max_parallel_tasks` from the loaded config: how many slots an owner
/// may hold open at once. Unset (or no usable config, whose load warning is
/// surfaced separately) means 1, the serial behaviour that predates the setting;
/// a hand-edited 0 is clamped to 1 (the write path rejects it).
fn max_parallel_tasks(config: Option<&crate::storage::config::Config>) -> usize {
    config
        .and_then(|c| c.calendar.max_parallel_tasks)
        .map_or(1, |n| (n as usize).max(1))
}

/// Where [`select_slot`] decided to put a task.
struct Placement {
    /// Index of the slot the task uses up, or `None` to open a new slot.
    slot: Option<usize>,
    /// First work day of the task.
    start: NaiveDate,
    /// Hours available on `start`.
    capacity: f64,
}

/// How a candidate placement relates to its slot; lower is preferred when two
/// candidates start on the same day.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Fit {
    /// The task shares the slot's own (partly or untouched) day.
    InSlot,
    /// A new slot, opened while `max_parallel` leaves room.
    NewSlot,
    /// The slot is moved on to a later day (the rest of its day is given up).
    MovedOn,
}

/// The latest date among `slots`, leaving out slot `skip`: every day an owner's
/// other slots may already have hours booked on lies on or before it.
fn latest_other_date(slots: &[Slot], skip: Option<usize>) -> Option<NaiveDate> {
    slots
        .iter()
        .enumerate()
        .filter(|(i, _)| Some(*i) != skip)
        .map(|(_, (d, _))| *d)
        .max()
}

/// A fresh day for a slot that gives up its current day: the first work day on
/// or after `from` and strictly after every day the owner's other slots reach.
fn frontier_day(slots: &[Slot], skip: Option<usize>, from: NaiveDate, cal: &Calendar) -> Slot {
    let after_others =
        latest_other_date(slots, skip).map_or(from, |d| from.max(d + Duration::days(1)));
    full_day(next_work_day(after_others, cal), cal)
}

/// Choose the slot a task of `hours` goes into, given the owner's open `slots`
/// and `constraint_floor`, the first day its dependencies / milestones allow.
///
/// 1. The earliest start wins; among equal starts a slot whose day the task
///    fits is preferred.
/// 2. A task that fits no slot's day opens a new slot while fewer than
///    `max_parallel` slots exist, leaving the other slots' free hours available
///    to later, smaller tasks.
/// 3. With `max_parallel` slots open, the task moves the slot that lets it
///    start earliest on to a later work day.
///
/// A day's hours are only ever drawn from by one slot: a slot's date is the last
/// day it has booked hours on, so a slot that leaves its day (is moved on, or
/// runs a task across several days) and a new slot both start strictly after
/// every other slot's date. A task that merely shares its slot's own partly used
/// day, within the hours left there, is the only placement that may start
/// earlier.
///
/// With `max_parallel` = 1 there is exactly one slot, which reproduces the
/// serial packing of [`pack_start`].
fn select_slot(
    slots: &[Slot],
    constraint_floor: NaiveDate,
    hours: f64,
    cal: &Calendar,
    max_parallel: usize,
) -> Placement {
    let mut best: Option<(Fit, Placement)> = None;
    let mut consider = |fit: Fit, placement: Placement| {
        let better = best
            .as_ref()
            .is_none_or(|(f, p)| (placement.start, fit) < (p.start, *f));
        if better {
            best = Some((fit, placement));
        }
    };

    for (i, &slot) in slots.iter().enumerate() {
        let task_start_work = next_work_day(slot.0.max(constraint_floor), cal);
        let (mut start, mut capacity) = pack_start(slot, task_start_work, hours, cal);
        let mut fit = if start == slot.0 {
            Fit::InSlot
        } else {
            Fit::MovedOn
        };
        // Leaving the slot's day, or running past it, must not reach days the
        // other slots have booked.
        let leaves_day = fit == Fit::MovedOn || hours > capacity + EPSILON;
        let blocked = latest_other_date(slots, Some(i)).is_some_and(|d| start <= d);
        if leaves_day && blocked {
            (start, capacity) = frontier_day(slots, Some(i), start, cal);
            fit = Fit::MovedOn;
        }
        consider(
            fit,
            Placement {
                slot: Some(i),
                start,
                capacity,
            },
        );
    }

    if slots.len() < max_parallel {
        let (start, capacity) = frontier_day(slots, None, constraint_floor, cal);
        consider(
            Fit::NewSlot,
            Placement {
                slot: None,
                start,
                capacity,
            },
        );
    }

    match best {
        Some((_, placement)) => placement,
        // `slots` is never empty (it is seeded with the owner's first work
        // day), so a candidate always exists; this opens a slot at the floor
        // should that ever stop holding.
        None => {
            let (start, capacity) = full_day(next_work_day(constraint_floor, cal), cal);
            Placement {
                slot: None,
                start,
                capacity,
            }
        }
    }
}

/// Choose where a task of `hours` starts and how many hours its first day
/// offers, given the owner's next free slot and the earliest work day the task's
/// constraints (dependencies, milestones) allow.
///
/// A task shares the slot's day only when constraints did not push it later and
/// it fits the hours still open there: a task is never split across a day
/// boundary after the day has been partly used. A task on an untouched day may
/// of course run on into following days.
fn pack_start(next_slot: Slot, task_start_work: NaiveDate, hours: f64, cal: &Calendar) -> Slot {
    let (slot_date, slot_remaining) = next_slot;
    if task_start_work != slot_date || slot_remaining <= EPSILON {
        return full_day(task_start_work, cal);
    }
    let untouched = slot_remaining >= cal.effective_capacity(&slot_date) - EPSILON;
    if untouched || hours <= slot_remaining + EPSILON {
        (slot_date, slot_remaining)
    } else {
        full_day(next_work_day(slot_date + Duration::days(1), cal), cal)
    }
}

/// Default `[worktree.session_loop].max_concurrent_wts` when unset (wiki/200
/// §3.6 documents 4 as the default).
const DEFAULT_MAX_CONCURRENT_WTS: u32 = 4;

/// Read `[worktree.session_loop].max_concurrent_wts` from config.toml.
/// Falls back to [`DEFAULT_MAX_CONCURRENT_WTS`] if the file is absent, the
/// table/key is missing, or the value fails to parse — the same fail-closed
/// posture as `parse_project_calendar` uses for calendar fields.
fn parse_max_concurrent_wts(config_path: &std::path::Path) -> u32 {
    let raw = match std::fs::read_to_string(config_path) {
        Ok(s) => s,
        Err(_) => return DEFAULT_MAX_CONCURRENT_WTS,
    };
    let doc: DocumentMut = match raw.parse() {
        Ok(d) => d,
        Err(_) => return DEFAULT_MAX_CONCURRENT_WTS,
    };
    doc.get("worktree")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("session_loop"))
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("max_concurrent_wts"))
        .and_then(|v| v.as_integer())
        .and_then(|i| u32::try_from(i).ok())
        .unwrap_or(DEFAULT_MAX_CONCURRENT_WTS)
}

/// Priority rank for `ready_tasks` sorting: lower sorts first. Unrecognized
/// or absent priority values sort last, alongside `None`.
fn priority_rank(priority: Option<&str>) -> u8 {
    match priority {
        Some("high") => 0,
        Some("medium") => 1,
        Some("low") => 2,
        _ => 3,
    }
}

/// Compute the `ready_tasks` list: `todo` tasks whose every dependency is
/// `done`, sorted by priority (high > medium > low > unspecified) and then
/// by `order`/id for a deterministic tie-break.
fn compute_ready_tasks(tasks_dir: &std::path::Path) -> Result<Vec<Value>> {
    let mut all_tasks: Vec<(TaskData, String)> = Vec::new();
    collect_all_tasks(tasks_dir, &mut all_tasks)?;

    let status_by_id: HashMap<&str, &str> = all_tasks
        .iter()
        .map(|(data, status)| (data.id.as_str(), status.as_str()))
        .collect();

    let mut ready: Vec<&(TaskData, String)> = all_tasks
        .iter()
        .filter(|(data, status)| {
            status == "todo"
                && data
                    .dependencies
                    .iter()
                    .all(|dep| status_by_id.get(dep.as_str()) == Some(&"done"))
        })
        .collect();

    ready.sort_by(|(a, _), (b, _)| {
        priority_rank(a.priority.as_deref())
            .cmp(&priority_rank(b.priority.as_deref()))
            .then_with(|| {
                a.order
                    .unwrap_or(u32::MAX)
                    .cmp(&b.order.unwrap_or(u32::MAX))
            })
            .then_with(|| a.id.cmp(&b.id))
    });

    Ok(ready
        .into_iter()
        .map(|(data, _)| {
            json!({
                "id": data.id,
                "title": data.title,
                "priority": data.priority,
                "estimate_hours": data.schedule.as_ref().and_then(|s| s.estimate_hours),
            })
        })
        .collect())
}

struct SchedulableTask {
    id: String,
    assignee: Option<String>,
    estimate_hours: Option<f64>,
    old_start: Option<String>,
    old_due: Option<String>,
    dependencies: Vec<String>,
    dep_due_dates: Vec<String>,
    order: Option<u32>,
    /// `stable_id`s of the task's `link_type: "requirement"` links — the
    /// route to a plan `SubItem` (see [`apply_plan_orders`]).
    requirement_ids: Vec<String>,
    /// `schedule.milestone` — the milestone whose `date` is this task's
    /// deadline (REQ-MS-SCHED-001).
    milestone: Option<String>,
}

/// Hours assumed for a task with no `estimate_hours` (one working day) when
/// `settings.default_estimate_hours` is not set.
const FALLBACK_ESTIMATE_HOURS: f64 = 8.0;

/// The hours to assume for a task without `estimate_hours`, from
/// `settings.default_estimate_hours`. An unset value yields
/// [`FALLBACK_ESTIMATE_HOURS`]; a hand-edited non-positive / non-finite value
/// also yields it, plus a warning (the write path rejects such values).
fn default_estimate_hours(
    config: Option<&crate::storage::config::Config>,
) -> (f64, Option<String>) {
    match config.and_then(|c| c.settings.default_estimate_hours) {
        None => (FALLBACK_ESTIMATE_HOURS, None),
        Some(h) if h.is_finite() && h > 0.0 => (h, None),
        Some(h) => (
            FALLBACK_ESTIMATE_HOURS,
            Some(format!(
                "settings.default_estimate_hours = {h} is invalid (must be > 0) — using {FALLBACK_ESTIMATE_HOURS:?}h"
            )),
        ),
    }
}

/// F5: one warning naming how many tasks fell back to the default estimate.
fn missing_estimate_warning(count: usize, default_hours: f64) -> Option<String> {
    if count == 0 {
        return None;
    }
    let (noun, verb) = if count == 1 {
        ("task", "has")
    } else {
        ("tasks", "have")
    };
    Some(format!(
        "{count} {noun} {verb} no estimate_hours \u{2014} using default {default_hours:?}h each. \
         Set estimate_hours on tasks or settings.default_estimate_hours in config.toml."
    ))
}

/// One scheduled task, as far as the milestone report is concerned.
struct PlacedTask {
    id: String,
    milestone: Option<String>,
    estimate_hours: f64,
    due: NaiveDate,
}

/// Milestone deadline report (REQ-MS-SCHED-001).
struct MilestoneReport {
    /// One entry per milestone that has at least one scheduled task.
    summary: Vec<Value>,
    warnings: Vec<String>,
}

/// Compares the computed due dates against each milestone's `date`.
///
/// `deadlines` maps every configured milestone to its optional `date`. A
/// milestone without a (parseable) date, or one that is not configured at all,
/// carries no constraint: its tasks are summarised but never flagged. An
/// unparseable date is reported once as a warning rather than silently
/// dropped. Tasks without a milestone are ignored.
fn milestone_report(
    placed: &[PlacedTask],
    deadlines: &HashMap<String, Option<String>>,
) -> MilestoneReport {
    let mut by_milestone: BTreeMap<&str, Vec<&PlacedTask>> = BTreeMap::new();
    for task in placed {
        if let Some(name) = task.milestone.as_deref() {
            by_milestone.entry(name).or_default().push(task);
        }
    }

    let mut summary = Vec::new();
    let mut warnings = Vec::new();
    for (name, tasks) in by_milestone {
        let raw_date = deadlines.get(name).and_then(|d| d.as_deref());
        let deadline = raw_date.and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
        if let (Some(raw), None) = (raw_date, deadline) {
            warnings.push(format!(
                "Milestone '{name}' has an invalid date '{raw}' (expected YYYY-MM-DD); no deadline constraint applied"
            ));
        }

        let mut overdue = false;
        if let (Some(raw), Some(deadline)) = (raw_date, deadline) {
            for task in &tasks {
                let late_days = (task.due - deadline).num_days();
                if late_days > 0 {
                    overdue = true;
                    warnings.push(format!(
                        "Milestone '{name}' (deadline {raw}): task {} scheduled to {} exceeds deadline by {late_days} day{}",
                        task.id,
                        task.due.format("%Y-%m-%d"),
                        if late_days == 1 { "" } else { "s" },
                    ));
                }
            }
        }

        summary.push(json!({
            "milestone": name,
            "date": raw_date,
            "task_count": tasks.len(),
            "total_estimate_hours": tasks.iter().map(|t| t.estimate_hours).sum::<f64>(),
            "latest_due": tasks.iter().map(|t| t.due).max().map(|d| d.format("%Y-%m-%d").to_string()),
            "overdue": overdue,
        }));
    }
    MilestoneReport { summary, warnings }
}

/// Milestone name -> optional deadline date from the loaded config. No config
/// (absent or unloadable; the load warning is surfaced separately) yields no
/// deadlines: the schedule itself does not depend on milestones.
fn milestone_deadlines(
    config: Option<&crate::storage::config::Config>,
) -> HashMap<String, Option<String>> {
    config
        .map(|c| {
            c.milestones
                .iter()
                .map(|(name, m)| (name.clone(), m.date.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// Milestone name -> the milestones it `depends_on`, for milestones that
/// declare any. A BTreeMap keeps cycle reports deterministic.
fn milestone_dependencies(
    config: Option<&crate::storage::config::Config>,
) -> BTreeMap<String, Vec<String>> {
    config
        .map(|c| {
            c.milestones
                .iter()
                .filter(|(_, m)| !m.depends_on.is_empty())
                .map(|(name, m)| (name.clone(), m.depends_on.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// Dependency depth of every milestone named in `deps` (as key or value): 0
/// for one that depends on nothing, otherwise 1 + the deepest milestone it
/// depends on. Placing tasks in ascending level order guarantees a milestone's
/// predecessors are placed first.
///
/// `Err` carries the first cycle found as a path that starts and ends on the
/// same milestone (`["v0-1", "v0-2", "v0-1"]`, each element depending on the
/// next); nodes that merely lead into the cycle are left out.
fn milestone_levels(
    deps: &BTreeMap<String, Vec<String>>,
) -> std::result::Result<HashMap<String, usize>, Vec<String>> {
    enum Mark {
        InProgress,
        Done(usize),
    }

    fn visit<'a>(
        node: &'a str,
        deps: &'a BTreeMap<String, Vec<String>>,
        marks: &mut HashMap<&'a str, Mark>,
        stack: &mut Vec<&'a str>,
    ) -> std::result::Result<usize, Vec<String>> {
        match marks.get(node) {
            Some(Mark::Done(level)) => return Ok(*level),
            Some(Mark::InProgress) => {
                // `node` is on the stack: the cycle is the stack from there on.
                let from = stack.iter().position(|n| *n == node).unwrap_or(0);
                let mut cycle: Vec<String> = stack[from..].iter().map(|n| n.to_string()).collect();
                cycle.push(node.to_string());
                return Err(cycle);
            }
            None => {}
        }
        marks.insert(node, Mark::InProgress);
        stack.push(node);
        let mut level = 0;
        for dep in deps.get(node).into_iter().flatten() {
            level = level.max(visit(dep, deps, marks, stack)? + 1);
        }
        stack.pop();
        marks.insert(node, Mark::Done(level));
        Ok(level)
    }

    let mut marks: HashMap<&str, Mark> = HashMap::new();
    let mut stack: Vec<&str> = Vec::new();
    for node in deps.keys() {
        visit(node, deps, &mut marks, &mut stack)?;
    }
    Ok(marks
        .into_iter()
        .filter_map(|(name, mark)| match mark {
            Mark::Done(level) => Some((name.to_string(), level)),
            Mark::InProgress => None,
        })
        .collect())
}

/// Keeps the latest known due date of a milestone's open tasks.
fn record_milestone_due(
    latest_due: &mut HashMap<String, NaiveDate>,
    milestone: &str,
    due: NaiveDate,
) {
    latest_due
        .entry(milestone.to_string())
        .and_modify(|d| *d = (*d).max(due))
        .or_insert(due);
}

/// The first day `milestone` may start on: the day after the latest open task
/// of any milestone it depends on. `None` when it depends on nothing, or every
/// predecessor is complete (no open task, hence no recorded due date).
fn predecessor_floor(
    deps: &BTreeMap<String, Vec<String>>,
    latest_due: &HashMap<String, NaiveDate>,
    milestone: &str,
) -> Option<NaiveDate> {
    deps.get(milestone)?
        .iter()
        .filter_map(|pred| latest_due.get(pred))
        .max()
        .map(|d| *d + Duration::days(1))
}

/// A non-terminal task with a milestone that this run does not reschedule.
struct FixedMilestoneTask {
    id: String,
    milestone: String,
    start: Option<NaiveDate>,
    due: Option<NaiveDate>,
}

/// Non-terminal, milestone-tagged tasks missing from `schedulable` (pinned, or
/// excluded by `assignee_filter`). Unparseable stored dates count as unset.
fn collect_fixed_milestone_tasks(
    tasks_dir: &std::path::Path,
    schedulable: &[SchedulableTask],
) -> Result<Vec<FixedMilestoneTask>> {
    let scheduled: HashSet<&str> = schedulable.iter().map(|t| t.id.as_str()).collect();
    let mut all: Vec<(TaskData, String)> = Vec::new();
    collect_all_tasks(tasks_dir, &mut all)?;
    let parse = |d: &Option<String>| {
        d.as_deref()
            .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
    };
    let mut fixed: Vec<FixedMilestoneTask> = all
        .into_iter()
        .filter(|(data, status)| {
            !is_terminal_status(status) && !scheduled.contains(data.id.as_str())
        })
        .filter_map(|(data, _)| {
            let schedule = data.schedule.as_ref()?;
            Some(FixedMilestoneTask {
                milestone: schedule.milestone.clone()?,
                start: parse(&schedule.start_date),
                due: parse(&schedule.due_date),
                id: data.id,
            })
        })
        .collect();
    fixed.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(fixed)
}

/// One warning per fixed task that starts on or before the day a predecessor
/// milestone's last open task ends (the soft-constraint override). A task with
/// no start date is judged by its due date.
fn fixed_task_dependency_warnings(
    fixed: &[FixedMilestoneTask],
    deps: &BTreeMap<String, Vec<String>>,
    latest_due: &HashMap<String, NaiveDate>,
) -> Vec<String> {
    let mut warnings = Vec::new();
    for task in fixed {
        let Some(begin) = task.start.or(task.due) else {
            continue;
        };
        for pred in deps.get(&task.milestone).into_iter().flatten() {
            let Some(pred_due) = latest_due.get(pred) else {
                continue;
            };
            if begin <= *pred_due {
                warnings.push(format!(
                    "Milestone '{}' depends on '{pred}' (latest task due {}): task {} is fixed at {} and starts before that; kept as is (dependency constraint overridden)",
                    task.milestone,
                    pred_due.format("%Y-%m-%d"),
                    task.id,
                    begin.format("%Y-%m-%d"),
                ));
            }
        }
    }
    warnings
}

/// SPEC-SI-009: gives each task that has neither an `order` of its own nor
/// dependencies the smallest `order` among the plan-document `SubItem`s it is
/// linked to. A task's own `order` always wins. Tasks with dependencies are
/// left alone: they already sort after every dependency-free task, and a
/// plan order must never be able to reshuffle a dependency chain.
///
/// Returns warnings when the plan documents could not be fully read; the
/// schedule is still computed (without the missing orders) rather than failed.
fn apply_plan_orders(handoff: &std::path::Path, tasks: &mut [SchedulableTask]) -> Vec<String> {
    let mut wanted: Vec<String> = Vec::new();
    for task in tasks
        .iter()
        .filter(|t| t.order.is_none() && t.dependencies.is_empty())
    {
        for id in &task.requirement_ids {
            if !wanted.contains(id) {
                wanted.push(id.clone());
            }
        }
    }
    let super::plan_links::PlanOrders {
        orders: plan_orders,
        warnings,
    } = super::plan_links::plan_orders(handoff, &wanted);
    if plan_orders.is_empty() {
        return warnings;
    }
    for task in tasks
        .iter_mut()
        .filter(|t| t.order.is_none() && t.dependencies.is_empty())
    {
        task.order = task
            .requirement_ids
            .iter()
            .filter_map(|id| plan_orders.get(id))
            .copied()
            .min();
    }
    warnings
}

fn collect_schedulable(
    tree: &[TaskIndex],
    tasks_dir: &std::path::Path,
    assignee_filter: Option<&str>,
    result: &mut Vec<SchedulableTask>,
) -> Result<()> {
    for node in tree {
        if is_terminal_status(&node.status) {
            collect_schedulable(&node.children, tasks_dir, assignee_filter, result)?;
            continue;
        }

        // Check if pinned
        let pinned = node
            .schedule
            .as_ref()
            .and_then(|s| s.pinned)
            .unwrap_or(false);
        if pinned {
            collect_schedulable(&node.children, tasks_dir, assignee_filter, result)?;
            continue;
        }

        // Check assignee filter
        if let Some(filter) = assignee_filter {
            if node.assignee.as_deref() != Some(filter) {
                collect_schedulable(&node.children, tasks_dir, assignee_filter, result)?;
                continue;
            }
        }

        // Get dependency due dates
        let dep_due_dates: Vec<String> = node
            .dependencies
            .iter()
            .filter_map(|dep_id| {
                find_task_dir_by_id(tasks_dir, dep_id)
                    .ok()
                    .flatten()
                    .and_then(|dir| read_task(&dir).ok().flatten())
                    .and_then(|(data, _)| data.schedule.and_then(|s| s.due_date))
            })
            .collect();

        result.push(SchedulableTask {
            id: node.id.clone(),
            assignee: node.assignee.clone(),
            estimate_hours: node.schedule.as_ref().and_then(|s| s.estimate_hours),
            old_start: node.schedule.as_ref().and_then(|s| s.start_date.clone()),
            old_due: node.schedule.as_ref().and_then(|s| s.due_date.clone()),
            dependencies: node.dependencies.clone(),
            dep_due_dates,
            order: node.order,
            requirement_ids: super::plan_links::linked_stable_ids(&node.task_links, true),
            milestone: node.schedule.as_ref().and_then(|s| s.milestone.clone()),
        });

        collect_schedulable(&node.children, tasks_dir, assignee_filter, result)?;
    }
    Ok(())
}

fn sort_by_deps(tasks: &mut Vec<SchedulableTask>) {
    let id_to_idx: HashMap<&str, usize> = tasks
        .iter()
        .enumerate()
        .map(|(i, t)| (t.id.as_str(), i))
        .collect();

    let n = tasks.len();
    let mut in_degree = vec![0u32; n];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];

    for (i, task) in tasks.iter().enumerate() {
        for dep in &task.dependencies {
            if let Some(&j) = id_to_idx.get(dep.as_str()) {
                in_degree[i] += 1;
                dependents[j].push(i);
            }
        }
    }

    // Seed with zero-in-degree tasks, sorted by (order, id) for a
    // deterministic tie-break among tasks at the same dependency depth.
    let mut ready: std::collections::BinaryHeap<std::cmp::Reverse<(u32, &str, usize)>> =
        std::collections::BinaryHeap::new();
    for (i, &deg) in in_degree.iter().enumerate() {
        if deg == 0 {
            ready.push(std::cmp::Reverse((
                tasks[i].order.unwrap_or(u32::MAX),
                tasks[i].id.as_str(),
                i,
            )));
        }
    }

    let mut order: Vec<usize> = Vec::with_capacity(n);
    while let Some(std::cmp::Reverse((_, _, i))) = ready.pop() {
        order.push(i);
        for &dep_idx in &dependents[i] {
            in_degree[dep_idx] -= 1;
            if in_degree[dep_idx] == 0 {
                ready.push(std::cmp::Reverse((
                    tasks[dep_idx].order.unwrap_or(u32::MAX),
                    tasks[dep_idx].id.as_str(),
                    dep_idx,
                )));
            }
        }
    }

    // Tasks in cycles (in_degree never reached 0) are appended at the end
    // sorted by (order, id) — the cycle itself is caught by the milestone
    // cycle check or by circular-dependency rejection in update_task.
    if order.len() < n {
        let in_order: HashSet<usize> = order.iter().copied().collect();
        let mut remainder: Vec<usize> = (0..n).filter(|i| !in_order.contains(i)).collect();
        remainder.sort_by(|&a, &b| {
            tasks[a]
                .order
                .unwrap_or(u32::MAX)
                .cmp(&tasks[b].order.unwrap_or(u32::MAX))
                .then_with(|| tasks[a].id.cmp(&tasks[b].id))
        });
        order.extend(remainder);
    }

    // Permute tasks in-place according to `order`.
    let mut sorted: Vec<SchedulableTask> = Vec::with_capacity(n);
    let mut taken = vec![false; n];
    for &i in &order {
        taken[i] = true;
    }
    // Drain into a temp vec, then place in order.
    let mut pool: Vec<Option<SchedulableTask>> = tasks.drain(..).map(Some).collect();
    for &i in &order {
        if let Some(task) = pool[i].take() {
            sorted.push(task);
        }
    }
    *tasks = sorted;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::handlers::HandlerContext;
    use crate::storage::agents::{AgentRecord, AgentStatus};
    use crate::storage::tasks::{DoneCriterion, Schedule, TaskData};
    use chrono::Datelike;
    use std::path::PathBuf;

    fn schedulable(id: &str, order: Option<u32>, deps: &[&str], reqs: &[&str]) -> SchedulableTask {
        SchedulableTask {
            id: id.to_string(),
            assignee: None,
            estimate_hours: None,
            old_start: None,
            old_due: None,
            dependencies: deps.iter().map(|d| d.to_string()).collect(),
            dep_due_dates: Vec::new(),
            order,
            requirement_ids: reqs.iter().map(|r| r.to_string()).collect(),
            milestone: None,
        }
    }

    fn write_plan_doc(handoff: &std::path::Path, doc_type: &str, items: &[(&str, u32)]) {
        use crate::storage::docs::model::{DocMetadata, SubItem, Verification, VerificationItem};
        let sub_items = items
            .iter()
            .enumerate()
            .map(|(i, (stable_id, order))| {
                let mut s: SubItem = serde_json::from_value(serde_json::json!({
                    "index": i, "description": "d", "status": "pending",
                }))
                .unwrap();
                s.stable_id = Some(stable_id.to_string());
                s.tree_order = Some(*order);
                s
            })
            .collect();
        let mut doc = DocMetadata::new(
            "doc-plan".to_string(),
            "the-plan".to_string(),
            "Plan".to_string(),
            doc_type.to_string(),
            "2026-01-01T00:00:00Z".to_string(),
        );
        doc.verification = Some(Verification {
            status: "pending".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            items: vec![VerificationItem {
                fragment_seq: Some(1),
                heading: "H".to_string(),
                status: "pending".to_string(),
                impl_refs: Vec::new(),
                test_refs: Vec::new(),
                reviewer: None,
                verified_at: None,
                notes: String::new(),
                content_hash_at_verify: None,
                category: "section".to_string(),
                sub_items,
                label: None,
            }],
        });
        crate::storage::docs::write_doc(handoff, &doc).unwrap();
    }

    #[test]
    fn apply_plan_orders_fills_only_unordered_dependency_free_tasks() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(handoff.join("docs")).unwrap();
        write_plan_doc(&handoff, "plan", &[("P-1", 5), ("P-2", 2), ("P-3", 9)]);

        let mut tasks = vec![
            schedulable("t-free", None, &[], &["P-1", "P-2"]), // min of 5, 2
            schedulable("t-own", Some(1), &[], &["P-3"]),      // own order wins
            schedulable("t-dep", None, &["t-free"], &["P-3"]), // has deps: untouched
            schedulable("t-none", None, &[], &[]),             // no links
            schedulable("t-other", None, &[], &["UNKNOWN"]),   // not in any plan
        ];
        apply_plan_orders(&handoff, &mut tasks);
        let orders: Vec<Option<u32>> = tasks.iter().map(|t| t.order).collect();
        assert_eq!(orders, [Some(2), Some(1), None, None, None]);

        sort_by_deps(&mut tasks);
        let ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
        // Topological: t-dep (depends on t-free) follows t-free immediately.
        // Among zero-deps, order wins (t-own=1, t-free=2), then id (t-none, t-other).
        assert_eq!(ids, ["t-own", "t-free", "t-dep", "t-none", "t-other"]);
    }

    #[test]
    fn apply_plan_orders_degrades_to_a_warning_when_documents_cannot_be_listed() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();
        // `docs` is a regular file, so listing the document directory fails.
        std::fs::write(handoff.join("docs"), "not a directory").unwrap();

        let mut tasks = vec![schedulable("t1", None, &[], &["P-1"])];
        let warnings = apply_plan_orders(&handoff, &mut tasks);
        assert_eq!(tasks[0].order, None);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("plan order"), "{warnings:?}");
    }

    #[test]
    fn apply_plan_orders_warns_about_an_unreadable_document_but_uses_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(handoff.join("docs")).unwrap();
        write_plan_doc(&handoff, "plan", &[("P-1", 5)]);
        std::fs::write(
            handoff.join("docs").join("_doc.broken.md"),
            "---\nid: [unterminated\n---\nbody\n",
        )
        .unwrap();

        let mut tasks = vec![schedulable("t1", None, &[], &["P-1"])];
        let warnings = apply_plan_orders(&handoff, &mut tasks);
        assert_eq!(tasks[0].order, Some(5), "readable plan still applies");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("broken"), "{warnings:?}");
    }

    #[test]
    fn apply_plan_orders_has_no_warning_when_there_is_nothing_to_look_up() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        let mut tasks = vec![schedulable("t1", None, &[], &[])];
        assert!(apply_plan_orders(&handoff, &mut tasks).is_empty());
    }

    #[test]
    fn apply_plan_orders_ignores_non_plan_documents() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(handoff.join("docs")).unwrap();
        write_plan_doc(&handoff, "spec", &[("P-1", 5)]);

        let mut tasks = vec![schedulable("t1", None, &[], &["P-1"])];
        apply_plan_orders(&handoff, &mut tasks);
        assert_eq!(tasks[0].order, None);
    }

    fn ctx(handoff_dir: PathBuf) -> HandlerContext {
        HandlerContext {
            agent_id: None,
            project_dir: handoff_dir.parent().unwrap().to_path_buf(),
            handoff_dir,
        }
    }

    fn write_task_with(
        tasks_dir: &std::path::Path,
        dir_name: &str,
        id: &str,
        status: &str,
        priority: Option<&str>,
        dependencies: Vec<String>,
    ) {
        let task_dir = tasks_dir.join(dir_name);
        std::fs::create_dir_all(&task_dir).unwrap();
        let data = TaskData {
            id: id.to_string(),
            title: format!("Task {id}"),
            notes: None,
            priority: priority.map(String::from),
            created_at: None,
            updated_at: None,
            completed_at: None,
            labels: Vec::new(),
            links: Vec::new(),
            task_links: Vec::new(),
            done_criteria: Vec::<DoneCriterion>::new(),
            schedule: Some(Schedule {
                estimate_hours: Some(3.0),
                ..Default::default()
            }),
            dependencies,
            order: None,
            assignee: None,
            lock: None,
            scope_paths: Vec::new(),
            extra: Default::default(),
        };
        write_task(&task_dir, status, &data).unwrap();
    }

    fn agent_record(id: &str, claimed: Vec<&str>) -> AgentRecord {
        AgentRecord {
            agent_id: id.to_string(),
            session_id: Some(format!("s-{id}")),
            worktree: PathBuf::from("/tmp/wt"),
            branch: None,
            pid: None,
            registered_at: Utc::now(),
            last_heartbeat: Utc::now(),
            status: AgentStatus::Active,
            claimed_tasks: claimed.into_iter().map(String::from).collect(),
            metadata: Default::default(),
        }
    }

    #[test]
    fn agent_capacity_empty_when_no_agents_registered() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff_dir).unwrap();
        let c = ctx(handoff_dir);

        let result = handle(&c, &json!({ "dry_run": true })).unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();

        assert_eq!(parsed["agent_capacity"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn agent_capacity_reflects_claimed_and_default_max_concurrent() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff_dir).unwrap();

        crate::storage::agents::write_agent(
            &handoff_dir,
            &agent_record("agent-1", vec!["t1", "t2"]),
        )
        .unwrap();

        let c = ctx(handoff_dir);
        let result = handle(&c, &json!({ "dry_run": true })).unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();

        let capacity = &parsed["agent_capacity"][0];
        assert_eq!(capacity["agent_id"], "agent-1");
        assert_eq!(capacity["claimed"], 2);
        // No config.toml [worktree.session_loop] max_concurrent_wts set -> default 4.
        assert_eq!(capacity["max_concurrent"], 4);
        assert_eq!(capacity["available"], 2);
    }

    #[test]
    fn agent_capacity_respects_configured_max_concurrent_wts() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff_dir).unwrap();
        std::fs::write(
            handoff_dir.join("config.toml"),
            "[worktree.session_loop]\nmax_concurrent_wts = 2\n",
        )
        .unwrap();

        crate::storage::agents::write_agent(&handoff_dir, &agent_record("agent-1", vec!["t1"]))
            .unwrap();

        let c = ctx(handoff_dir);
        let result = handle(&c, &json!({ "dry_run": true })).unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();

        let capacity = &parsed["agent_capacity"][0];
        assert_eq!(capacity["max_concurrent"], 2);
        assert_eq!(capacity["claimed"], 1);
        assert_eq!(capacity["available"], 1);
    }

    #[test]
    fn ready_tasks_includes_only_todo_with_dependencies_done_sorted_by_priority() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        let tasks_dir = handoff_dir.join("tasks");
        std::fs::create_dir_all(&tasks_dir).unwrap();

        // t1: done (a dependency target)
        write_task_with(&tasks_dir, "t1-done", "t1", "done", None, vec![]);
        // t2: todo, no deps, low priority
        write_task_with(&tasks_dir, "t2-low", "t2", "todo", Some("low"), vec![]);
        // t3: todo, depends on t1 (done) -> ready, high priority
        write_task_with(
            &tasks_dir,
            "t3-high",
            "t3",
            "todo",
            Some("high"),
            vec!["t1".to_string()],
        );
        // t4: todo, no explicit priority -> ready, sorts last
        write_task_with(&tasks_dir, "t4-none", "t4", "todo", None, vec![]);
        // t5: todo, medium priority, ready
        write_task_with(
            &tasks_dir,
            "t5-medium",
            "t5",
            "todo",
            Some("medium"),
            vec![],
        );
        // t6: todo, depends on t2 (not done) -> NOT ready
        write_task_with(
            &tasks_dir,
            "t6-blocked",
            "t6",
            "todo",
            Some("high"),
            vec!["t2".to_string()],
        );

        let c = ctx(handoff_dir);
        let result = handle(&c, &json!({ "dry_run": true })).unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();

        let ready_ids: Vec<String> = parsed["ready_tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap().to_string())
            .collect();

        // t6 excluded (dependency t2 not done). Order: high, medium, low, none.
        assert_eq!(ready_ids, vec!["t3", "t5", "t2", "t4"]);
    }

    /// `handle`'s apply-if-not-dry_run branch rewrites the same-named task
    /// file (status unchanged, only `schedule` fields updated) on every call.
    /// Before t375's fix it did `find_task_file` -> `remove_file` ->
    /// `write_task`, briefly leaving zero task files on disk; a concurrent
    /// unlocked reader (`read_task`, e.g. `handoff_get_task`) landing in that
    /// window would see `None` and the caller would report a spurious "Task
    /// not found". `write_task_transition` closes that window by writing
    /// before removing, and by never removing at all when the status name is
    /// unchanged.
    #[test]
    fn concurrent_auto_schedule_apply_and_read_do_not_report_task_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        let tasks_dir = handoff_dir.join("tasks");
        std::fs::create_dir_all(&tasks_dir).unwrap();

        write_task_with(&tasks_dir, "t1-todo", "t1", "todo", None, vec![]);
        let task_dir = tasks_dir.join("t1-todo");

        let c = ctx(handoff_dir);
        let writer = std::thread::spawn(move || {
            for _ in 0..50 {
                handle(&c, &json!({ "dry_run": false, "start_date": "2026-01-05" })).unwrap();
            }
        });

        let reader_task_dir = task_dir.clone();
        let reader = std::thread::spawn(move || {
            for _ in 0..200 {
                let result = crate::storage::tasks::read_task(&reader_task_dir).unwrap();
                assert!(
                    result.is_some(),
                    "read_task observed zero task files mid-write (Task not found race)"
                );
            }
        });

        writer.join().unwrap();
        reader.join().unwrap();

        assert!(read_task(&task_dir).unwrap().is_some());
    }

    fn set_baseline(
        tasks_dir: &std::path::Path,
        dir_name: &str,
        start: Option<&str>,
        due: Option<&str>,
    ) {
        let task_dir = tasks_dir.join(dir_name);
        let (mut data, status) = read_task(&task_dir).unwrap().unwrap();
        let schedule = data.schedule.get_or_insert_with(Default::default);
        schedule.baseline_start = start.map(String::from);
        schedule.baseline_due = due.map(String::from);
        write_task(&task_dir, &status, &data).unwrap();
    }

    fn schedule_of(tasks_dir: &std::path::Path, dir_name: &str) -> Schedule {
        let (data, _) = read_task(&tasks_dir.join(dir_name)).unwrap().unwrap();
        data.schedule.unwrap()
    }

    #[test]
    fn apply_records_baseline_for_unset_tasks_and_dry_run_does_not() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        let tasks_dir = handoff_dir.join("tasks");
        std::fs::create_dir_all(&tasks_dir).unwrap();
        write_task_with(&tasks_dir, "t1-a", "t1", "todo", None, vec![]);
        let c = ctx(handoff_dir);

        handle(&c, &json!({ "dry_run": true, "start_date": "2026-03-02" })).unwrap();
        let s = schedule_of(&tasks_dir, "t1-a");
        assert_eq!((s.baseline_start, s.baseline_due), (None, None));
        assert_eq!(s.start_date, None);

        handle(&c, &json!({ "dry_run": false, "start_date": "2026-03-02" })).unwrap();
        let s = schedule_of(&tasks_dir, "t1-a");
        assert_eq!(s.start_date.as_deref(), Some("2026-03-02"));
        assert_eq!(s.baseline_start, s.start_date);
        assert_eq!(s.baseline_due, s.due_date);
        assert!(s.baseline_due.is_some());
    }

    #[test]
    fn apply_never_overwrites_existing_baseline_but_fills_missing_half() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        let tasks_dir = handoff_dir.join("tasks");
        std::fs::create_dir_all(&tasks_dir).unwrap();
        write_task_with(&tasks_dir, "t1-a", "t1", "todo", None, vec![]);
        write_task_with(&tasks_dir, "t2-b", "t2", "todo", None, vec![]);
        set_baseline(&tasks_dir, "t1-a", Some("2020-01-01"), Some("2020-01-02"));
        set_baseline(&tasks_dir, "t2-b", Some("2020-01-01"), None);
        let c = ctx(handoff_dir);

        handle(&c, &json!({ "dry_run": false, "start_date": "2026-03-02" })).unwrap();

        let s = schedule_of(&tasks_dir, "t1-a");
        assert_eq!(s.baseline_start.as_deref(), Some("2020-01-01"));
        assert_eq!(s.baseline_due.as_deref(), Some("2020-01-02"));
        assert_ne!(s.start_date.as_deref(), Some("2020-01-01"));

        let s = schedule_of(&tasks_dir, "t2-b");
        assert_eq!(s.baseline_start.as_deref(), Some("2020-01-01"));
        assert_eq!(s.baseline_due, s.due_date);
    }

    #[test]
    fn reset_baseline_then_auto_schedule_records_a_fresh_one() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        let tasks_dir = handoff_dir.join("tasks");
        std::fs::create_dir_all(&tasks_dir).unwrap();
        write_task_with(&tasks_dir, "t1-a", "t1", "todo", None, vec![]);
        set_baseline(&tasks_dir, "t1-a", Some("2020-01-01"), Some("2020-01-02"));
        let c = ctx(handoff_dir);

        set_baseline(&tasks_dir, "t1-a", None, None);
        handle(&c, &json!({ "dry_run": false, "start_date": "2026-03-02" })).unwrap();
        let s = schedule_of(&tasks_dir, "t1-a");
        assert_eq!(s.baseline_start.as_deref(), Some("2026-03-02"));
    }

    // ---- REQ-MS-SCHED-001: milestone deadline report ----

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn placed(id: &str, milestone: Option<&str>, hours: f64, due: &str) -> PlacedTask {
        PlacedTask {
            id: id.to_string(),
            milestone: milestone.map(String::from),
            estimate_hours: hours,
            due: day(due),
        }
    }

    fn deadlines(entries: &[(&str, Option<&str>)]) -> HashMap<String, Option<String>> {
        entries
            .iter()
            .map(|(k, d)| (k.to_string(), d.map(String::from)))
            .collect()
    }

    #[test]
    fn milestone_report_within_deadline_has_no_warning() {
        let placed = vec![
            placed("t1", Some("v0-1"), 8.0, "2026-11-20"),
            placed("t2", Some("v0-1"), 16.0, "2026-11-28"),
        ];
        let report = milestone_report(&placed, &deadlines(&[("v0-1", Some("2026-12-01"))]));
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_eq!(
            report.summary,
            vec![json!({
                "milestone": "v0-1",
                "date": "2026-12-01",
                "task_count": 2,
                "total_estimate_hours": 24.0,
                "latest_due": "2026-11-28",
                "overdue": false,
            })]
        );
    }

    #[test]
    fn milestone_report_warns_for_each_task_past_the_deadline() {
        let placed = vec![
            placed("t-gm-p2.4", Some("v0-1"), 8.0, "2026-12-05"),
            placed("t-ok", Some("v0-1"), 8.0, "2026-12-01"),
        ];
        let report = milestone_report(&placed, &deadlines(&[("v0-1", Some("2026-12-01"))]));
        assert_eq!(
            report.warnings,
            vec![
                "Milestone 'v0-1' (deadline 2026-12-01): task t-gm-p2.4 scheduled to 2026-12-05 exceeds deadline by 4 days"
                    .to_string()
            ]
        );
        assert_eq!(report.summary[0]["overdue"], json!(true));
        assert_eq!(report.summary[0]["latest_due"], json!("2026-12-05"));
    }

    #[test]
    fn milestone_report_singular_day_wording() {
        let placed = vec![placed("t1", Some("m"), 1.0, "2026-12-02")];
        let report = milestone_report(&placed, &deadlines(&[("m", Some("2026-12-01"))]));
        assert!(
            report.warnings[0].ends_with("exceeds deadline by 1 day"),
            "{:?}",
            report.warnings
        );
    }

    #[test]
    fn milestone_report_ignores_tasks_without_milestone() {
        let placed = vec![placed("t1", None, 8.0, "2030-01-01")];
        let report = milestone_report(&placed, &deadlines(&[("v0-1", Some("2026-12-01"))]));
        assert!(report.warnings.is_empty());
        assert!(report.summary.is_empty());
    }

    #[test]
    fn milestone_report_date_none_or_unknown_milestone_is_unconstrained() {
        let placed = vec![
            placed("t1", Some("nodate"), 2.0, "2030-01-01"),
            placed("t2", Some("ghost"), 3.0, "2030-02-01"),
        ];
        let report = milestone_report(&placed, &deadlines(&[("nodate", None)]));
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_eq!(report.summary.len(), 2);
        for entry in &report.summary {
            assert_eq!(entry["date"], Value::Null);
            assert_eq!(entry["overdue"], json!(false));
        }
    }

    #[test]
    fn milestone_report_unparseable_date_warns_and_is_unconstrained() {
        let placed = vec![placed("t1", Some("m"), 2.0, "2030-01-01")];
        let report = milestone_report(&placed, &deadlines(&[("m", Some("soon"))]));
        assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
        assert!(report.warnings[0].contains("'m'") && report.warnings[0].contains("soon"));
        assert_eq!(report.summary[0]["overdue"], json!(false));
    }

    fn write_config(handoff: &std::path::Path, body: &str) {
        std::fs::write(handoff.join("config.toml"), body).unwrap();
    }

    fn set_milestone(tasks_dir: &std::path::Path, dir_name: &str, milestone: &str) {
        let task_dir = tasks_dir.join(dir_name);
        let (mut data, status) = read_task(&task_dir).unwrap().unwrap();
        data.schedule.get_or_insert_with(Default::default).milestone = Some(milestone.to_string());
        write_task(&task_dir, &status, &data).unwrap();
    }

    #[test]
    fn handle_reports_milestone_summary_and_overdue_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        let tasks_dir = handoff_dir.join("tasks");
        std::fs::create_dir_all(&tasks_dir).unwrap();
        write_config(
            &handoff_dir,
            "[project]\nname = \"p\"\n\n[calendar]\nmax_utilization = 100\n\n[milestones.v0-1]\ndate = \"2026-03-02\"\n",
        );
        // 3h each pack into 8h days: t1 and t2 share 03-02, t3 (9h total) spills to 03-03.
        for (dir, id) in [("t1-a", "t1"), ("t2-a", "t2"), ("t3-a", "t3")] {
            write_task_with(&tasks_dir, dir, id, "todo", None, vec![]);
            set_milestone(&tasks_dir, dir, "v0-1");
        }
        write_task_with(&tasks_dir, "t4-a", "t4", "todo", None, vec![]);

        let out = handle(&ctx(handoff_dir), &json!({ "start_date": "2026-03-02" })).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let summary = v["milestone_summary"].as_array().unwrap();
        assert_eq!(summary.len(), 1, "{summary:?}");
        assert_eq!(summary[0]["milestone"], json!("v0-1"));
        assert_eq!(summary[0]["task_count"], json!(3));
        assert_eq!(summary[0]["total_estimate_hours"], json!(9.0));
        assert_eq!(summary[0]["overdue"], json!(true));
        let warnings = v["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].as_str().unwrap().contains("exceeds deadline"));
    }

    #[test]
    fn handle_without_milestones_keeps_legacy_output_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        let tasks_dir = handoff_dir.join("tasks");
        std::fs::create_dir_all(&tasks_dir).unwrap();
        write_task_with(&tasks_dir, "t1-a", "t1", "todo", None, vec![]);

        let out = handle(&ctx(handoff_dir), &json!({ "start_date": "2026-03-02" })).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["milestone_summary"], json!([]));
        assert!(v.get("warnings").is_none(), "{v}");
        assert_eq!(v["scheduled_count"], json!(1));
    }

    // ---- REQ-SCHED-001/003/006: intra-day packing ----

    /// Adds a todo task of `hours` (optionally assigned / dependent) for the
    /// packing tests. Ids are zero-padded so id order equals creation order.
    fn add_sized_task(
        tasks_dir: &std::path::Path,
        id: &str,
        hours: f64,
        assignee: Option<&str>,
        dependencies: &[&str],
    ) {
        let dir = format!("{id}-d");
        write_task_with(
            tasks_dir,
            &dir,
            id,
            "todo",
            None,
            dependencies.iter().map(|d| d.to_string()).collect(),
        );
        edit_task(tasks_dir, &dir, |data, schedule| {
            data.assignee = assignee.map(String::from);
            schedule.estimate_hours = Some(hours);
        });
    }

    fn change_of<'a>(v: &'a Value, id: &str) -> &'a Value {
        v["changes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["task_id"] == json!(id))
            .unwrap_or_else(|| panic!("no change for {id}: {v}"))
    }

    fn start_of(v: &Value, id: &str) -> String {
        change_of(v, id)["new_start"].as_str().unwrap().to_string()
    }

    fn due_of(v: &Value, id: &str) -> String {
        change_of(v, id)["new_due"].as_str().unwrap().to_string()
    }

    fn count_starting_on(v: &Value, date: &str) -> usize {
        v["changes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["new_start"] == json!(date))
            .count()
    }

    #[test]
    fn packing_small_tasks_into_same_day() {
        let (_tmp, handoff, tasks) = ms_project("");
        for i in 1..=5 {
            add_sized_task(&tasks, &format!("t{i:02}"), 0.5, Some("alice"), &[]);
        }
        let v = run(&handoff);
        for i in 1..=5 {
            let id = format!("t{i:02}");
            assert_eq!(start_of(&v, &id), "2026-03-02", "{id}");
            assert_eq!(due_of(&v, &id), "2026-03-02", "{id}");
        }
    }

    #[test]
    fn packing_fills_then_spills_to_next_day() {
        let (_tmp, handoff, tasks) = ms_project(STRICT_DAY);
        for i in 1..=17 {
            add_sized_task(&tasks, &format!("t{i:02}"), 0.5, Some("alice"), &[]);
        }
        let v = run(&handoff);
        assert_eq!(count_starting_on(&v, "2026-03-02"), 16);
        assert_eq!(count_starting_on(&v, "2026-03-03"), 1);
        assert_eq!(start_of(&v, "t17"), "2026-03-03");
    }

    #[test]
    fn task_exceeding_remaining_capacity_starts_next_day() {
        let (_tmp, handoff, tasks) = ms_project(STRICT_DAY);
        add_sized_task(&tasks, "t01", 3.0, Some("alice"), &[]);
        add_sized_task(&tasks, "t02", 6.0, Some("alice"), &[]);
        let v = run(&handoff);
        // 5h are free on 03-02, but the 6h task is not split across the day boundary.
        assert_eq!(start_of(&v, "t02"), "2026-03-03");
        assert_eq!(due_of(&v, "t02"), "2026-03-03");
    }

    #[test]
    fn task_exactly_filling_remaining_capacity_packs_and_next_starts_next_day() {
        let (_tmp, handoff, tasks) = ms_project(STRICT_DAY);
        add_sized_task(&tasks, "t01", 3.0, Some("alice"), &[]);
        add_sized_task(&tasks, "t02", 5.0, Some("alice"), &[]);
        add_sized_task(&tasks, "t03", 0.5, Some("alice"), &[]);
        let v = run(&handoff);
        assert_eq!(due_of(&v, "t02"), "2026-03-02");
        assert_eq!(start_of(&v, "t03"), "2026-03-03");
    }

    #[test]
    fn hundred_half_hour_tasks_fit_in_thirteen_days() {
        let (_tmp, handoff, tasks) = ms_project(STRICT_DAY);
        for i in 1..=100 {
            add_sized_task(&tasks, &format!("t{i:03}"), 0.5, Some("alice"), &[]);
        }
        let v = run(&handoff);
        assert_eq!(v["scheduled_count"], json!(100));
        assert_eq!(v["changes"].as_array().unwrap().len(), 100);
        // 16 tasks per 8h day => ceil(100 / 16) = 7 work days: Mon 03-02 .. Tue 03-10.
        let last = (1..=100)
            .map(|i| due_of(&v, &format!("t{i:03}")))
            .max()
            .unwrap();
        assert_eq!(last, "2026-03-10");
        assert_eq!(count_starting_on(&v, "2026-03-02"), 16);
        // Every task stays on the single day it starts on.
        for i in 1..=100 {
            let id = format!("t{i:03}");
            assert_eq!(start_of(&v, &id), due_of(&v, &id), "{id}");
        }
    }

    #[test]
    fn different_assignees_pack_independently() {
        let (_tmp, handoff, tasks) = ms_project("");
        add_sized_task(&tasks, "a1", 0.5, Some("alice"), &[]);
        add_sized_task(&tasks, "a2", 0.5, Some("alice"), &[]);
        add_sized_task(&tasks, "b1", 0.5, Some("bob"), &[]);
        add_sized_task(&tasks, "b2", 0.5, Some("bob"), &[]);
        let v = run(&handoff);
        for id in ["a1", "a2", "b1", "b2"] {
            assert_eq!(start_of(&v, id), "2026-03-02", "{id}");
        }
    }

    #[test]
    fn assignees_do_not_consume_each_others_capacity() {
        let (_tmp, handoff, tasks) = ms_project("");
        add_sized_task(&tasks, "a1", 8.0, Some("alice"), &[]);
        add_sized_task(&tasks, "b1", 8.0, Some("bob"), &[]);
        let v = run(&handoff);
        assert_eq!(start_of(&v, "a1"), "2026-03-02");
        assert_eq!(start_of(&v, "b1"), "2026-03-02");
    }

    #[test]
    fn unassigned_tasks_share_project_pool() {
        let (_tmp, handoff, tasks) = ms_project("");
        for i in 1..=3 {
            add_sized_task(&tasks, &format!("t{i}"), 0.5, None, &[]);
        }
        let v = run(&handoff);
        for i in 1..=3 {
            assert_eq!(start_of(&v, &format!("t{i}")), "2026-03-02");
        }
    }

    #[test]
    fn unassigned_pool_is_separate_from_assignees() {
        let (_tmp, handoff, tasks) = ms_project("");
        add_sized_task(&tasks, "a1", 8.0, Some("alice"), &[]);
        add_sized_task(&tasks, "u1", 8.0, None, &[]);
        let v = run(&handoff);
        assert_eq!(start_of(&v, "u1"), "2026-03-02");
    }

    #[test]
    fn dependency_forces_next_day_even_with_capacity() {
        let (_tmp, handoff, tasks) = ms_project("");
        add_sized_task(&tasks, "t1", 3.0, Some("alice"), &[]);
        add_sized_task(&tasks, "t2", 3.0, Some("alice"), &["t1"]);
        let v = run(&handoff);
        assert_eq!(due_of(&v, "t1"), "2026-03-02");
        assert_eq!(start_of(&v, "t2"), "2026-03-03");
    }

    #[test]
    fn multi_day_task_leaves_remainder_for_packing() {
        let (_tmp, handoff, tasks) = ms_project("");
        add_sized_task(&tasks, "t1", 12.0, Some("alice"), &[]);
        add_sized_task(&tasks, "t2", 3.0, Some("alice"), &[]);
        let v = run(&handoff);
        // 12h = 8h on 03-02 + 4h on 03-03, leaving 4h on 03-03 for the 3h task.
        assert_eq!(start_of(&v, "t1"), "2026-03-02");
        assert_eq!(due_of(&v, "t1"), "2026-03-03");
        assert_eq!(start_of(&v, "t2"), "2026-03-03");
        assert_eq!(due_of(&v, "t2"), "2026-03-03");
    }

    #[test]
    fn task_longer_than_a_day_starts_on_an_untouched_day() {
        let (_tmp, handoff, tasks) = ms_project("");
        add_sized_task(&tasks, "t1", 12.0, Some("alice"), &[]);
        let v = run(&handoff);
        assert_eq!(start_of(&v, "t1"), "2026-03-02");
    }

    #[test]
    fn full_day_tasks_skip_weekend_when_spilling() {
        let (_tmp, handoff, tasks) = ms_project("");
        // One 8h task per day: t5 lands on Friday 03-06, t6 skips the weekend to Monday.
        add_sized_task(&tasks, "t1", 8.0, Some("alice"), &[]);
        add_sized_task(&tasks, "t2", 8.0, Some("alice"), &[]);
        add_sized_task(&tasks, "t3", 8.0, Some("alice"), &[]);
        add_sized_task(&tasks, "t4", 8.0, Some("alice"), &[]);
        add_sized_task(&tasks, "t5", 8.0, Some("alice"), &[]);
        add_sized_task(&tasks, "t6", 8.0, Some("alice"), &[]);
        let v = run(&handoff);
        assert_eq!(start_of(&v, "t5"), "2026-03-06");
        assert_eq!(start_of(&v, "t6"), "2026-03-09");
    }

    /// Calendar section capping a day at exactly its working hours, so the
    /// 8h-per-day packing arithmetic of these tests holds.
    const STRICT_DAY: &str = "[calendar]\nmax_utilization = 100\n";

    fn util_project(max_utilization: u32) -> (tempfile::TempDir, PathBuf, PathBuf) {
        ms_project(&format!(
            "[calendar]\nmax_utilization = {max_utilization}\n"
        ))
    }

    #[test]
    fn max_utilization_50_packs_four_hours_per_day() {
        let (_tmp, handoff, tasks) = util_project(50);
        add_sized_task(&tasks, "t1", 4.0, Some("alice"), &[]);
        add_sized_task(&tasks, "t2", 4.0, Some("alice"), &[]);
        let v = run(&handoff);
        assert_eq!(start_of(&v, "t1"), "2026-03-02");
        assert_eq!(start_of(&v, "t2"), "2026-03-03");
    }

    #[test]
    fn max_utilization_50_spreads_eight_hour_task_over_two_days() {
        let (_tmp, handoff, tasks) = util_project(50);
        add_sized_task(&tasks, "t1", 8.0, Some("alice"), &[]);
        let v = run(&handoff);
        assert_eq!(start_of(&v, "t1"), "2026-03-02");
        assert_eq!(due_of(&v, "t1"), "2026-03-03");
    }

    #[test]
    fn max_utilization_100_is_strict_eight_hour_day() {
        let (_tmp, handoff, tasks) = util_project(100);
        add_sized_task(&tasks, "t1", 8.0, Some("alice"), &[]);
        add_sized_task(&tasks, "t2", 0.5, Some("alice"), &[]);
        let v = run(&handoff);
        assert_eq!(due_of(&v, "t1"), "2026-03-02");
        assert_eq!(start_of(&v, "t2"), "2026-03-03");
    }

    #[test]
    fn default_utilization_fits_eight_hour_task_in_one_day_and_packs_remainder() {
        let (_tmp, handoff, tasks) = ms_project("");
        add_sized_task(&tasks, "t1", 8.0, Some("alice"), &[]);
        add_sized_task(&tasks, "t2", 1.5, Some("alice"), &[]);
        add_sized_task(&tasks, "t3", 1.0, Some("alice"), &[]);
        let v = run(&handoff);
        assert_eq!(due_of(&v, "t1"), "2026-03-02");
        // 120% of 8h = 9.6h: 8h + 1.5h fits on day one, 1.0h would exceed it.
        assert_eq!(start_of(&v, "t2"), "2026-03-02");
        assert_eq!(start_of(&v, "t3"), "2026-03-03");
    }

    #[test]
    fn default_utilization_overflows_estimate_above_nine_point_six_hours() {
        let (_tmp, handoff, tasks) = ms_project("");
        add_sized_task(&tasks, "t1", 10.0, Some("alice"), &[]);
        let v = run(&handoff);
        assert_eq!(start_of(&v, "t1"), "2026-03-02");
        assert_eq!(due_of(&v, "t1"), "2026-03-03");
    }

    #[test]
    fn response_keeps_all_existing_fields() {
        let (_tmp, handoff, tasks) = ms_project("");
        add_sized_task(&tasks, "t1", 0.5, Some("alice"), &[]);
        let v = run(&handoff);
        for key in [
            "dry_run",
            "scheduled_count",
            "changed_count",
            "changes",
            "decision_recorded_in_sessions",
            "calendar_config",
            "assignee_capacity",
            "agent_capacity",
            "ready_tasks",
            "schedule_stale",
            "milestone_summary",
        ] {
            assert!(v.get(key).is_some(), "missing `{key}` in {v}");
        }
        for key in ["work_hours_per_day", "closed_weekdays", "day_hours"] {
            assert!(v["calendar_config"].get(key).is_some(), "{key}");
        }
        for key in ["task_id", "old_start", "new_start", "old_due", "new_due"] {
            assert!(v["changes"][0].get(key).is_some(), "change.{key}");
        }
    }

    // ---- REQ-MS-SCHED-002: inter-milestone dependency constraint ----

    fn edit_task(
        tasks_dir: &std::path::Path,
        dir_name: &str,
        f: impl FnOnce(&mut TaskData, &mut Schedule),
    ) {
        let task_dir = tasks_dir.join(dir_name);
        let (mut data, status) = read_task(&task_dir).unwrap().unwrap();
        let mut schedule = data.schedule.take().unwrap_or_default();
        f(&mut data, &mut schedule);
        data.schedule = Some(schedule);
        write_task(&task_dir, &status, &data).unwrap();
    }

    /// Adds a todo task (3h) in `milestone` owned by `assignee`; separate
    /// assignees keep the tasks from queueing behind each other.
    fn add_ms_task(
        tasks_dir: &std::path::Path,
        id: &str,
        milestone: &str,
        assignee: &str,
        status: &str,
    ) {
        let dir = format!("{id}-d");
        write_task_with(tasks_dir, &dir, id, status, None, vec![]);
        edit_task(tasks_dir, &dir, |data, schedule| {
            data.assignee = Some(assignee.to_string());
            schedule.milestone = Some(milestone.to_string());
        });
    }

    fn pin(tasks_dir: &std::path::Path, id: &str, start: &str, due: &str) {
        edit_task(tasks_dir, &format!("{id}-d"), |_, schedule| {
            schedule.pinned = Some(true);
            schedule.start_date = Some(start.to_string());
            schedule.due_date = Some(due.to_string());
        });
    }

    /// A project whose config declares the given `[milestones.*]` body.
    fn ms_project(milestones_toml: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        let tasks_dir = handoff_dir.join("tasks");
        std::fs::create_dir_all(&tasks_dir).unwrap();
        write_config(
            &handoff_dir,
            &format!("[project]\nname = \"p\"\n\n{milestones_toml}"),
        );
        (tmp, handoff_dir, tasks_dir)
    }

    const CHAIN_2: &str = "[milestones.v0-1]\n\n[milestones.v0-2]\ndepends_on = [\"v0-1\"]\n";

    fn run(handoff_dir: &std::path::Path) -> Value {
        let out = handle(
            &ctx(handoff_dir.to_path_buf()),
            &json!({ "start_date": "2026-03-02" }),
        )
        .unwrap();
        serde_json::from_str(&out).unwrap()
    }

    fn new_start_of(v: &Value, id: &str) -> Option<String> {
        v["changes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["task_id"] == json!(id))
            .map(|c| c["new_start"].as_str().unwrap().to_string())
    }

    #[test]
    fn dependent_milestone_starts_after_predecessor_tasks_finish() {
        let (_tmp, handoff, tasks) = ms_project(CHAIN_2);
        add_ms_task(&tasks, "t1", "v0-1", "alice", "todo");
        add_ms_task(&tasks, "t2", "v0-2", "bob", "todo");

        let v = run(&handoff);
        // t1 is due 03-02; without the constraint t2 would start 03-02 as well.
        assert_eq!(new_start_of(&v, "t1").as_deref(), Some("2026-03-02"));
        assert_eq!(new_start_of(&v, "t2").as_deref(), Some("2026-03-03"));
        assert!(v.get("warnings").is_none(), "{v}");
    }

    #[test]
    fn completed_predecessor_milestone_adds_no_constraint() {
        let (_tmp, handoff, tasks) = ms_project(CHAIN_2);
        add_ms_task(&tasks, "t1", "v0-1", "alice", "done");
        edit_task(&tasks, "t1-d", |_, s| {
            s.due_date = Some("2026-03-10".into())
        });
        add_ms_task(&tasks, "t2", "v0-2", "bob", "todo");

        let v = run(&handoff);
        assert_eq!(new_start_of(&v, "t2").as_deref(), Some("2026-03-02"));
    }

    #[test]
    fn pinned_predecessor_task_delays_the_dependent_milestone() {
        let (_tmp, handoff, tasks) = ms_project(CHAIN_2);
        add_ms_task(&tasks, "t1", "v0-1", "alice", "todo");
        pin(&tasks, "t1", "2026-03-09", "2026-03-10");
        add_ms_task(&tasks, "t2", "v0-2", "bob", "todo");

        let v = run(&handoff);
        assert_eq!(new_start_of(&v, "t2").as_deref(), Some("2026-03-11"));
        assert!(v.get("warnings").is_none(), "{v}");
    }

    #[test]
    fn pinned_task_violating_the_constraint_is_kept_with_a_warning() {
        let (_tmp, handoff, tasks) = ms_project(CHAIN_2);
        add_ms_task(&tasks, "t1", "v0-1", "alice", "todo");
        pin(&tasks, "t1", "2026-03-09", "2026-03-10");
        add_ms_task(&tasks, "t2", "v0-2", "bob", "todo");
        pin(&tasks, "t2", "2026-03-05", "2026-03-06");

        let v = run(&handoff);
        let warnings = v["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let w = warnings[0].as_str().unwrap();
        assert!(
            w.contains("t2") && w.contains("v0-2") && w.contains("v0-1"),
            "{w}"
        );
        assert!(w.contains("2026-03-05") && w.contains("2026-03-10"), "{w}");
        // The pinned task is neither rescheduled nor counted.
        assert_eq!(v["scheduled_count"], json!(0));
        let kept = schedule_of(&tasks, "t2-d");
        assert_eq!(kept.start_date.as_deref(), Some("2026-03-05"));
    }

    #[test]
    fn pinned_task_after_the_predecessor_finishes_is_not_warned() {
        let (_tmp, handoff, tasks) = ms_project(CHAIN_2);
        add_ms_task(&tasks, "t1", "v0-1", "alice", "todo");
        pin(&tasks, "t1", "2026-03-09", "2026-03-10");
        add_ms_task(&tasks, "t2", "v0-2", "bob", "todo");
        pin(&tasks, "t2", "2026-03-11", "2026-03-12");

        let v = run(&handoff);
        assert!(v.get("warnings").is_none(), "{v}");
    }

    #[test]
    fn circular_milestone_dependency_is_rejected_with_the_cycle_path() {
        let (_tmp, handoff, tasks) = ms_project(
            "[milestones.v0-1]\ndepends_on = [\"v0-2\"]\n\n[milestones.v0-2]\ndepends_on = [\"v0-1\"]\n",
        );
        add_ms_task(&tasks, "t1", "v0-1", "alice", "todo");

        let err = handle(
            &ctx(handoff.clone()),
            &json!({ "start_date": "2026-03-02", "dry_run": false }),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("v0-1 \u{2192} v0-2 \u{2192} v0-1"), "{err}");
        // Rejected before anything is written.
        assert_eq!(schedule_of(&tasks, "t1-d").start_date, None);
    }

    #[test]
    fn milestones_without_depends_on_are_unaffected() {
        let (_tmp, handoff, tasks) = ms_project("[milestones.v0-1]\n\n[milestones.v0-2]\n");
        add_ms_task(&tasks, "t1", "v0-1", "alice", "todo");
        add_ms_task(&tasks, "t2", "v0-2", "bob", "todo");

        let v = run(&handoff);
        assert_eq!(new_start_of(&v, "t1").as_deref(), Some("2026-03-02"));
        assert_eq!(new_start_of(&v, "t2").as_deref(), Some("2026-03-02"));
    }

    #[test]
    fn multi_step_chain_is_scheduled_in_dependency_order_regardless_of_id() {
        let (_tmp, handoff, tasks) = ms_project(
            "[milestones.v0-1]\n\n[milestones.v0-2]\ndepends_on = [\"v0-1\"]\n\n[milestones.v0-3]\ndepends_on = [\"v0-2\"]\n",
        );
        // Ids sort opposite to the milestone order.
        add_ms_task(&tasks, "a1", "v0-3", "carol", "todo");
        add_ms_task(&tasks, "a2", "v0-2", "bob", "todo");
        add_ms_task(&tasks, "a3", "v0-1", "alice", "todo");

        let v = run(&handoff);
        assert_eq!(new_start_of(&v, "a3").as_deref(), Some("2026-03-02"));
        assert_eq!(new_start_of(&v, "a2").as_deref(), Some("2026-03-03"));
        assert_eq!(new_start_of(&v, "a1").as_deref(), Some("2026-03-04"));
    }

    #[test]
    fn unconfigured_predecessor_milestone_adds_no_constraint() {
        let (_tmp, handoff, tasks) = ms_project("[milestones.v0-2]\ndepends_on = [\"ghost\"]\n");
        add_ms_task(&tasks, "t2", "v0-2", "bob", "todo");

        let v = run(&handoff);
        assert_eq!(new_start_of(&v, "t2").as_deref(), Some("2026-03-02"));
    }

    fn deps(entries: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.iter().map(|s| s.to_string()).collect()))
            .collect()
    }

    #[test]
    fn milestone_levels_follow_the_longest_dependency_chain() {
        let levels = milestone_levels(&deps(&[
            ("c", &["a", "b"]),
            ("b", &["a"]),
            ("a", &[]),
            ("d", &["ghost"]),
        ]))
        .unwrap();
        assert_eq!(levels["a"], 0);
        assert_eq!(levels["b"], 1);
        assert_eq!(levels["c"], 2);
        assert_eq!(levels["d"], 1);
        assert_eq!(levels.get("ghost").copied().unwrap_or(0), 0);
    }

    #[test]
    fn milestone_levels_reports_two_and_three_node_cycles_and_self_loops() {
        assert_eq!(
            milestone_levels(&deps(&[("a", &["b"]), ("b", &["a"])])).unwrap_err(),
            vec!["a", "b", "a"]
        );
        assert_eq!(
            milestone_levels(&deps(&[("a", &["b"]), ("b", &["c"]), ("c", &["a"])])).unwrap_err(),
            vec!["a", "b", "c", "a"]
        );
        assert_eq!(
            milestone_levels(&deps(&[("x", &[]), ("a", &["a"])])).unwrap_err(),
            vec!["a", "a"]
        );
    }

    #[test]
    fn milestone_levels_cycle_path_excludes_the_lead_in_nodes() {
        // `a` merely depends on the cycle b <-> c; it is not part of it.
        let cycle =
            milestone_levels(&deps(&[("a", &["b"]), ("b", &["c"]), ("c", &["b"])])).unwrap_err();
        assert_eq!(cycle, vec!["b", "c", "b"]);
    }

    // ---- F4/F5: settings.default_estimate_hours and the no-estimate warning ----

    /// A todo task with no `estimate_hours`.
    fn add_unestimated_task(tasks_dir: &std::path::Path, id: &str) {
        let dir = format!("{id}-d");
        write_task_with(tasks_dir, &dir, id, "todo", None, vec![]);
        edit_task(tasks_dir, &dir, |_, schedule| {
            schedule.estimate_hours = None
        });
    }

    fn project_with_settings(settings_toml: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        ms_project(&format!("[settings]\n{settings_toml}\n"))
    }

    fn estimate_warnings(v: &Value) -> Vec<String> {
        v["warnings"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|w| w.as_str())
                    .filter(|w| w.contains("estimate_hours"))
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn configured_default_estimate_hours_is_used_for_tasks_without_estimate() {
        let (_tmp, handoff, tasks) = project_with_settings("default_estimate_hours = 2.0");
        for i in 1..=5 {
            add_unestimated_task(&tasks, &format!("t{i:02}"));
        }
        let v = run(&handoff);
        // 2h each into 8h days: four share 03-02, the fifth spills.
        assert_eq!(count_starting_on(&v, "2026-03-02"), 4);
        assert_eq!(start_of(&v, "t05"), "2026-03-03");
    }

    #[test]
    fn unset_default_estimate_hours_keeps_the_eight_hour_default() {
        let (_tmp, handoff, tasks) = ms_project("");
        add_unestimated_task(&tasks, "t01");
        add_unestimated_task(&tasks, "t02");
        let v = run(&handoff);
        assert_eq!(start_of(&v, "t01"), "2026-03-02");
        assert_eq!(start_of(&v, "t02"), "2026-03-03");
    }

    #[test]
    fn explicit_estimate_wins_over_configured_default() {
        let (_tmp, handoff, tasks) = project_with_settings("default_estimate_hours = 2.0");
        add_sized_task(&tasks, "t01", 8.0, None, &[]);
        add_unestimated_task(&tasks, "t02");
        let v = run(&handoff);
        assert_eq!(start_of(&v, "t02"), "2026-03-03");
    }

    #[test]
    fn invalid_configured_default_falls_back_to_eight_hours_with_a_warning() {
        let (_tmp, handoff, tasks) = project_with_settings("default_estimate_hours = -3.0");
        add_unestimated_task(&tasks, "t01");
        add_unestimated_task(&tasks, "t02");
        let v = run(&handoff);
        assert_eq!(start_of(&v, "t02"), "2026-03-03");
        let warnings = estimate_warnings(&v);
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("default_estimate_hours") && w.contains("-3")),
            "{warnings:?}"
        );
    }

    #[test]
    fn warns_with_count_and_default_when_estimates_are_missing() {
        let (_tmp, handoff, tasks) = ms_project("");
        add_unestimated_task(&tasks, "t01");
        add_unestimated_task(&tasks, "t02");
        add_sized_task(&tasks, "t03", 1.0, None, &[]);
        let v = run(&handoff);
        assert_eq!(
            estimate_warnings(&v),
            vec![
                "2 tasks have no estimate_hours \u{2014} using default 8.0h each. Set estimate_hours on tasks or settings.default_estimate_hours in config.toml."
                    .to_string()
            ]
        );
    }

    #[test]
    fn warning_shows_the_configured_default_and_singular_wording() {
        let (_tmp, handoff, tasks) = project_with_settings("default_estimate_hours = 2.5");
        add_unestimated_task(&tasks, "t01");
        let v = run(&handoff);
        let warnings = estimate_warnings(&v);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0]
                .starts_with("1 task has no estimate_hours \u{2014} using default 2.5h each."),
            "{warnings:?}"
        );
    }

    #[test]
    fn no_estimate_warning_when_every_task_has_an_estimate() {
        let (_tmp, handoff, tasks) = ms_project("");
        add_sized_task(&tasks, "t01", 1.0, None, &[]);
        add_sized_task(&tasks, "t02", 2.0, None, &[]);
        let v = run(&handoff);
        assert!(estimate_warnings(&v).is_empty(), "{v}");
        assert!(v.get("warnings").is_none(), "{v}");
    }

    #[test]
    fn estimate_warning_merges_with_milestone_warnings() {
        let (_tmp, handoff, tasks) = ms_project("[milestones.m]\ndate = \"2026-03-01\"\n");
        add_ms_task(&tasks, "t01", "m", "alice", "todo");
        edit_task(&tasks, "t01-d", |_, schedule| {
            schedule.estimate_hours = None
        });
        let v = run(&handoff);
        let all = v["warnings"].as_array().unwrap();
        assert!(
            all.iter()
                .any(|w| w.as_str().unwrap().contains("exceeds deadline"))
                || all.len() == 1,
            "{all:?}"
        );
        assert_eq!(estimate_warnings(&v).len(), 1, "{all:?}");
    }

    // ---- F6: max_parallel_tasks slots ----

    const PARALLEL_2: &str = "[calendar]\nmax_parallel_tasks = 2\n";

    #[test]
    fn parallel_two_places_two_tasks_on_day_one_where_serial_packing_cannot() {
        // 9.6h/day (the default 120%): 6h + 3h fit day one, the other 6h does not.
        let sizes = [("t01", 6.0), ("t02", 6.0), ("t03", 3.0)];
        let starts = |config: &str| {
            let (_tmp, handoff, tasks) = ms_project(config);
            for (id, hours) in sizes {
                add_sized_task(&tasks, id, hours, Some("alice"), &[]);
            }
            let v = run(&handoff);
            sizes.map(|(id, _)| start_of(&v, id))
        };
        // Serial: the 3h task queues behind the second 6h task.
        assert_eq!(starts(""), ["2026-03-02", "2026-03-03", "2026-03-03"]);
        // Two slots: it shares day one with the first 6h task.
        assert_eq!(
            starts(PARALLEL_2),
            ["2026-03-02", "2026-03-03", "2026-03-02"]
        );
    }

    /// Hours of `tasks` (id, estimate) charged to each work day, spreading a
    /// multi-day task over consecutive weekdays at `per_day` hours each.
    fn hours_per_day(v: &Value, tasks: &[(String, f64)], per_day: f64) -> HashMap<String, f64> {
        let mut used: HashMap<String, f64> = HashMap::new();
        for (id, hours) in tasks {
            let mut date = NaiveDate::parse_from_str(&start_of(v, id), "%Y-%m-%d").unwrap();
            let mut left = *hours;
            while left > EPSILON {
                if date.weekday().number_from_monday() <= 5 {
                    let take = left.min(per_day);
                    *used.entry(date.to_string()).or_default() += take;
                    left -= take;
                }
                date += Duration::days(1);
            }
        }
        used
    }

    #[test]
    fn parallel_slots_never_overcommit_a_day_when_a_slot_moves_on() {
        // 8h/day (100% utilization). 4h leaves slot 1 with 4h; the 6h task opens
        // slot 2 on day two (2h left); the 7h task fits neither, and moving slot 1
        // on to day two would stack 13h on a day already holding 6h.
        let (_tmp, handoff, tasks) =
            ms_project("[calendar]\nmax_parallel_tasks = 2\nmax_utilization = 100\n");
        for (id, hours) in [("t01", 4.0), ("t02", 6.0), ("t03", 7.0)] {
            add_sized_task(&tasks, id, hours, Some("alice"), &[]);
        }
        let v = run(&handoff);
        assert_eq!(start_of(&v, "t02"), "2026-03-03");
        assert_eq!(start_of(&v, "t03"), "2026-03-04");
    }

    #[test]
    fn parallel_slots_keep_every_day_within_capacity() {
        // Deterministic mixed sizes (single- and multi-day) over several slot
        // counts; no day may carry more than the 8h the calendar offers.
        let mut seed = 7_u32;
        let mut next = move || {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            f64::from((seed >> 16) % 40 + 1) / 2.0 // 0.5h ..= 20h
        };
        for parallel in [2, 3, 4] {
            for round in 0..6 {
                let config =
                    format!("[calendar]\nmax_parallel_tasks = {parallel}\nmax_utilization = 100\n");
                let (_tmp, handoff, tasks_dir) = ms_project(&config);
                let tasks: Vec<(String, f64)> =
                    (0..10).map(|i| (format!("t{i:02}"), next())).collect();
                for (id, hours) in &tasks {
                    add_sized_task(&tasks_dir, id, *hours, Some("alice"), &[]);
                }
                let v = run(&handoff);
                for (day, hours) in hours_per_day(&v, &tasks, 8.0) {
                    assert!(
                        hours <= 8.0 + EPSILON,
                        "parallel={parallel} round={round}: {day} carries {hours}h ({tasks:?})"
                    );
                }
            }
        }
    }

    #[test]
    fn parallel_slots_backfill_a_partly_used_day() {
        let (_tmp, handoff, tasks) = ms_project(PARALLEL_2);
        add_sized_task(&tasks, "t01", 6.0, Some("alice"), &[]);
        add_sized_task(&tasks, "t02", 6.0, Some("alice"), &[]);
        add_sized_task(&tasks, "t03", 2.0, Some("alice"), &[]);
        let v = run(&handoff);
        assert_eq!(start_of(&v, "t01"), "2026-03-02");
        // 2h are free on 03-02 but t02 (6h) does not fit: it takes a new slot on 03-03 ...
        assert_eq!(start_of(&v, "t02"), "2026-03-03");
        // ... and the 2h left on 03-02 stays open for t03.
        assert_eq!(start_of(&v, "t03"), "2026-03-02");
    }

    #[test]
    fn default_max_parallel_keeps_serial_packing() {
        for config in ["", "[calendar]\nmax_parallel_tasks = 1\n"] {
            let (_tmp, handoff, tasks) = ms_project(config);
            add_sized_task(&tasks, "t01", 6.0, Some("alice"), &[]);
            add_sized_task(&tasks, "t02", 6.0, Some("alice"), &[]);
            add_sized_task(&tasks, "t03", 2.0, Some("alice"), &[]);
            let v = run(&handoff);
            assert_eq!(start_of(&v, "t02"), "2026-03-03", "{config:?}");
            // The 2h left on 03-02 are given up: t03 packs behind t02.
            assert_eq!(start_of(&v, "t03"), "2026-03-03", "{config:?}");
        }
    }

    #[test]
    fn parallel_three_small_tasks_share_the_first_day() {
        let (_tmp, handoff, tasks) = ms_project("[calendar]\nmax_parallel_tasks = 3\n");
        for i in 1..=3 {
            add_sized_task(&tasks, &format!("t{i:02}"), 2.0, Some("alice"), &[]);
        }
        let v = run(&handoff);
        assert_eq!(count_starting_on(&v, "2026-03-02"), 3);
    }

    #[test]
    fn parallel_slot_never_places_a_task_before_its_dependency_ends() {
        let (_tmp, handoff, tasks) = ms_project(PARALLEL_2);
        add_sized_task(&tasks, "t01", 6.0, Some("alice"), &[]);
        add_sized_task(&tasks, "t02", 6.0, Some("alice"), &[]);
        // Would fit the 2h left on 03-02, but depends on t02 which ends 03-03.
        add_sized_task(&tasks, "t03", 2.0, Some("alice"), &["t02"]);
        let v = run(&handoff);
        assert_eq!(due_of(&v, "t02"), "2026-03-03");
        assert_eq!(start_of(&v, "t03"), "2026-03-04");
    }

    #[test]
    fn parallel_slots_are_per_assignee_and_cover_unassigned_tasks() {
        let (_tmp, handoff, tasks) = ms_project(PARALLEL_2);
        add_sized_task(&tasks, "a1", 6.0, Some("alice"), &[]);
        add_sized_task(&tasks, "a2", 6.0, Some("alice"), &[]);
        add_sized_task(&tasks, "a3", 2.0, Some("alice"), &[]);
        add_sized_task(&tasks, "u1", 6.0, None, &[]);
        add_sized_task(&tasks, "u2", 6.0, None, &[]);
        add_sized_task(&tasks, "u3", 2.0, None, &[]);
        let v = run(&handoff);
        assert_eq!(start_of(&v, "u1"), "2026-03-02");
        assert_eq!(start_of(&v, "u2"), "2026-03-03");
        assert_eq!(start_of(&v, "u3"), "2026-03-02");
        assert_eq!(start_of(&v, "a3"), "2026-03-02");
    }

    #[test]
    fn max_parallel_tasks_defaults_to_one_and_clamps_zero() {
        assert_eq!(max_parallel_tasks(None), 1);
        let mut cfg = crate::storage::config::Config::new("p", "d");
        assert_eq!(max_parallel_tasks(Some(&cfg)), 1);
        cfg.calendar.max_parallel_tasks = Some(0);
        assert_eq!(max_parallel_tasks(Some(&cfg)), 1);
        cfg.calendar.max_parallel_tasks = Some(3);
        assert_eq!(max_parallel_tasks(Some(&cfg)), 3);
    }
}
