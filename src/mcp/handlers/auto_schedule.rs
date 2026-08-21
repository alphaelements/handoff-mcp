use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{Datelike, Duration, NaiveDate, Utc};
use serde_json::{json, Value};
use toml_edit::DocumentMut;

use super::HandlerContext;
use crate::storage::agents::list_agents;
use crate::storage::config::weekday_to_num;
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

    // Collect schedulable tasks (non-terminal, not pinned)
    let mut schedulable: Vec<SchedulableTask> = Vec::new();
    collect_schedulable(&tree, &tasks_dir, assignee_filter, &mut schedulable)?;

    // Sort by dependencies then order
    sort_by_deps(&mut schedulable);

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

    let mut assignee_next_date: HashMap<String, NaiveDate> = HashMap::new();
    let mut project_next_date = start_date;
    let mut changes: Vec<Value> = Vec::new();

    for task in &schedulable {
        let cal = task
            .assignee
            .as_ref()
            .and_then(|a| assignee_calendars.get(a))
            .unwrap_or(&calendar);

        let earliest = match &task.assignee {
            Some(a) => assignee_next_date.get(a).copied().unwrap_or(start_date),
            None => project_next_date,
        };

        // Also respect dependency completion dates
        let dep_earliest = task
            .dep_due_dates
            .iter()
            .filter_map(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
            .max()
            .map(|d| d + Duration::days(1))
            .unwrap_or(start_date);

        let task_start = earliest.max(dep_earliest);

        // Find next work day from task_start
        let actual_start = next_work_day(task_start, cal);

        // Calculate end date based on estimate_hours, drawing each day's capacity
        // from the calendar's per-day hours (day_hours overrides).
        let hours = task.estimate_hours.unwrap_or(8.0);
        let actual_end = advance_by_hours(actual_start, hours, cal);

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

        // Advance the next available date for this assignee
        let next_available = actual_end + Duration::days(1);
        match &task.assignee {
            Some(a) => {
                assignee_next_date.insert(a.clone(), next_available);
            }
            None => {
                project_next_date = next_available;
            }
        }

        // Apply if not dry_run
        if !dry_run {
            if let Some(task_dir) = find_task_dir_by_id(&tasks_dir, &task.id)? {
                if let Some((mut data, status)) = read_task(&task_dir)? {
                    let schedule = data.schedule.get_or_insert_with(Default::default);
                    schedule.start_date = Some(new_start);
                    schedule.due_date = Some(new_due);
                    data.updated_at = Some(Utc::now().to_rfc3339());

                    if let Some((old_path, _)) = find_task_file(&task_dir)? {
                        std::fs::remove_file(&old_path)?;
                    }
                    write_task(&task_dir, &status, &data)?;
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

    let result = json!({
        "dry_run": dry_run,
        "scheduled_count": schedulable.len(),
        "changed_count": changes.len(),
        "changes": changes,
        "decision_recorded_in_sessions": decision_recorded_in,
        "calendar_config": {
            "work_hours_per_day": calendar.work_hours_per_day,
            "closed_weekdays": calendar.closed_weekdays,
            "day_hours": calendar.day_hours,
        },
        "assignee_capacity": assignee_capacity,
        "agent_capacity": agent_capacity,
        "ready_tasks": ready_tasks,
    });

    serde_json::to_string_pretty(&result).map_err(Into::into)
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
        });

        collect_schedulable(&node.children, tasks_dir, assignee_filter, result)?;
    }
    Ok(())
}

fn sort_by_deps(tasks: &mut [SchedulableTask]) {
    // Simple topological sort: tasks with no deps first, then by order
    tasks.sort_by(|a, b| {
        let a_has_deps = !a.dependencies.is_empty();
        let b_has_deps = !b.dependencies.is_empty();
        a_has_deps
            .cmp(&b_has_deps)
            .then_with(|| {
                a.order
                    .unwrap_or(u32::MAX)
                    .cmp(&b.order.unwrap_or(u32::MAX))
            })
            .then_with(|| a.id.cmp(&b.id))
    });
}

struct Calendar {
    work_hours_per_day: f64,
    closed_weekdays: Vec<u32>,
    closed_dates: Vec<String>,
    open_dates: Vec<String>,
    /// Per-weekday-name or per-YYYY-MM-DD working-hour overrides.
    day_hours: HashMap<String, f64>,
}

const WEEKDAY_NAMES: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

impl Calendar {
    /// Effective working hours for a specific date. A date-specific override in
    /// `day_hours` takes precedence over a weekday-name override, which takes
    /// precedence over `work_hours_per_day`. Mirrors capacity.rs.
    fn hours_for(&self, date: &NaiveDate) -> f64 {
        let date_str = date.format("%Y-%m-%d").to_string();
        if let Some(h) = self.day_hours.get(&date_str) {
            return *h;
        }
        let weekday_num = weekday_index(date);
        let name = WEEKDAY_NAMES[weekday_num as usize];
        if let Some(h) = self.day_hours.get(name) {
            return *h;
        }
        self.work_hours_per_day
    }

    fn is_work_day(&self, date: &NaiveDate) -> bool {
        let date_str = date.format("%Y-%m-%d").to_string();

        if self.closed_dates.contains(&date_str) {
            return false;
        }

        if self.open_dates.contains(&date_str) {
            return true;
        }

        !self.closed_weekdays.contains(&weekday_index(date))
    }
}

fn weekday_index(date: &NaiveDate) -> u32 {
    match date.weekday() {
        chrono::Weekday::Sun => 0,
        chrono::Weekday::Mon => 1,
        chrono::Weekday::Tue => 2,
        chrono::Weekday::Wed => 3,
        chrono::Weekday::Thu => 4,
        chrono::Weekday::Fri => 5,
        chrono::Weekday::Sat => 6,
    }
}

fn next_work_day(from: NaiveDate, cal: &Calendar) -> NaiveDate {
    let mut date = from;
    while !cal.is_work_day(&date) {
        date += Duration::days(1);
    }
    date
}

/// Advance from `start` (assumed to be a work day) consuming `hours` of effort,
/// drawing each day's capacity from `cal.hours_for(date)`. Returns the last work
/// day the task occupies. Respects per-day hour overrides (day_hours), so a task
/// spanning a half-capacity Friday takes an extra day. (referral ref-...004309 §5)
fn advance_by_hours(start: NaiveDate, hours: f64, cal: &Calendar) -> NaiveDate {
    let mut date = start;
    // Consume the first day's capacity.
    let mut remaining = hours - cal.hours_for(&date).max(0.0);
    // Guard against a zero-capacity calendar (would otherwise loop forever).
    let mut guard = 0;
    while remaining > 1e-9 && guard < 10_000 {
        date = next_work_day(date + Duration::days(1), cal);
        remaining -= cal.hours_for(&date).max(0.0);
        guard += 1;
    }
    date
}

fn parse_project_calendar(config_path: &std::path::Path) -> Result<Calendar> {
    let mut cal = Calendar {
        work_hours_per_day: 8.0,
        closed_weekdays: vec![0, 6], // Sun, Sat
        closed_dates: Vec::new(),
        open_dates: Vec::new(),
        day_hours: HashMap::new(),
    };

    if !config_path.exists() {
        return Ok(cal);
    }

    let raw = std::fs::read_to_string(config_path).with_context(|| "Failed to read config")?;
    let doc: DocumentMut = raw.parse().with_context(|| "Failed to parse config")?;

    if let Some(calendar) = doc.get("calendar").and_then(|v| v.as_table()) {
        if let Some(h) = calendar
            .get("work_hours_per_day")
            .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
        {
            cal.work_hours_per_day = h;
        }
        if let Some(arr) = calendar.get("closed_weekdays").and_then(|v| v.as_array()) {
            cal.closed_weekdays = arr
                .iter()
                .filter_map(|v| {
                    v.as_integer()
                        .map(|i| i as u32)
                        .or_else(|| v.as_str().and_then(weekday_to_num))
                })
                .collect();
        }
        if let Some(arr) = calendar.get("closed_dates").and_then(|v| v.as_array()) {
            cal.closed_dates = arr
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
        }
        if let Some(arr) = calendar.get("open_dates").and_then(|v| v.as_array()) {
            cal.open_dates = arr
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
        }
        if let Some(dh) = calendar.get("day_hours").and_then(|v| v.as_table()) {
            cal.day_hours = parse_day_hours(dh);
        }
    }

    Ok(cal)
}

/// Parse a `[*.day_hours]` table into a map of weekday-name/date -> hours.
fn parse_day_hours(table: &toml_edit::Table) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    for (key, item) in table.iter() {
        if let Some(h) = item
            .as_value()
            .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
        {
            out.insert(key.to_string(), h);
        }
    }
    out
}

fn parse_assignee_calendars(config_path: &std::path::Path) -> Result<HashMap<String, Calendar>> {
    let mut result = HashMap::new();

    if !config_path.exists() {
        return Ok(result);
    }

    let raw = std::fs::read_to_string(config_path).with_context(|| "Failed to read config")?;
    let doc: DocumentMut = raw.parse().with_context(|| "Failed to parse config")?;

    let base = parse_project_calendar(config_path)?;

    if let Some(assignees) = doc.get("assignees").and_then(|v| v.as_table()) {
        for (key, item) in assignees.iter() {
            let a = match item.as_table() {
                Some(t) => t,
                None => continue,
            };

            let mut cal = Calendar {
                work_hours_per_day: base.work_hours_per_day,
                closed_weekdays: base.closed_weekdays.clone(),
                closed_dates: base.closed_dates.clone(),
                open_dates: base.open_dates.clone(),
                day_hours: base.day_hours.clone(),
            };

            if let Some(h) = a
                .get("work_hours_per_day")
                .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            {
                cal.work_hours_per_day = h;
            }
            if let Some(arr) = a.get("closed_weekdays").and_then(|v| v.as_array()) {
                cal.closed_weekdays = arr
                    .iter()
                    .filter_map(|v| {
                        v.as_integer()
                            .map(|i| i as u32)
                            .or_else(|| v.as_str().and_then(weekday_to_num))
                    })
                    .collect();
            }
            if let Some(arr) = a.get("closed_dates").and_then(|v| v.as_array()) {
                for item in arr.iter() {
                    if let Some(s) = item.as_str() {
                        cal.closed_dates.push(s.to_string());
                    }
                }
            }
            if let Some(arr) = a.get("open_dates").and_then(|v| v.as_array()) {
                for item in arr.iter() {
                    if let Some(s) = item.as_str() {
                        cal.open_dates.push(s.to_string());
                    }
                }
            }
            // Per-assignee day_hours override the inherited project values key-by-key.
            if let Some(dh) = a.get("day_hours").and_then(|v| v.as_table()) {
                for (k, v) in parse_day_hours(dh) {
                    cal.day_hours.insert(k, v);
                }
            }

            result.insert(key.to_string(), cal);
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::handlers::HandlerContext;
    use crate::storage::agents::{AgentRecord, AgentStatus};
    use crate::storage::tasks::{DoneCriterion, Schedule, TaskData};
    use std::path::PathBuf;

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
}
