use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{Datelike, Duration, NaiveDate, Utc};
use serde_json::{json, Value};
use toml_edit::DocumentMut;

use super::resolve_project_dir;
use crate::storage::ensure_handoff_exists;
use crate::storage::tasks::*;

pub fn handle(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;
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
    });

    serde_json::to_string_pretty(&result).map_err(Into::into)
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

fn weekday_to_num(s: &str) -> Option<u32> {
    match s.to_lowercase().as_str() {
        "sun" | "sunday" => Some(0),
        "mon" | "monday" => Some(1),
        "tue" | "tuesday" => Some(2),
        "wed" | "wednesday" => Some(3),
        "thu" | "thursday" => Some(4),
        "fri" | "friday" => Some(5),
        "sat" | "saturday" => Some(6),
        _ => None,
    }
}
