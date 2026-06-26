use anyhow::{Context, Result};
use chrono::{Datelike, NaiveDate};
use serde_json::{json, Value};
use toml_edit::DocumentMut;

use super::resolve_project_dir;
use crate::storage::config::read_config;
use crate::storage::ensure_handoff_exists;
use crate::storage::tasks::{build_task_index, is_terminal_status, TaskIndex};

pub fn handle(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;
    let config_path = handoff.join("config.toml");
    let tasks_dir = handoff.join("tasks");

    let start_date_str = arguments
        .get("start_date")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'start_date' parameter is required (YYYY-MM-DD)"))?;
    let end_date_str = arguments
        .get("end_date")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'end_date' parameter is required (YYYY-MM-DD)"))?;
    let assignee_filter = arguments.get("assignee").and_then(|v| v.as_str());

    let start = NaiveDate::parse_from_str(start_date_str, "%Y-%m-%d")
        .with_context(|| format!("Invalid start_date: {start_date_str}"))?;
    let end = NaiveDate::parse_from_str(end_date_str, "%Y-%m-%d")
        .with_context(|| format!("Invalid end_date: {end_date_str}"))?;

    if end < start {
        anyhow::bail!("end_date must be >= start_date");
    }

    let calendar = parse_calendar(&config_path, assignee_filter)?;

    let mut days: Vec<Value> = Vec::new();
    let mut total_hours: f64 = 0.0;
    let mut work_days: u32 = 0;

    let mut date = start;
    while date <= end {
        let hours = calendar.hours_for_date(&date);
        if hours > 0.0 {
            work_days += 1;
            total_hours += hours;
        }
        days.push(json!({
            "date": date.format("%Y-%m-%d").to_string(),
            "capacity_hours": hours,
            "allocated_hours": 0.0,
        }));
        date += chrono::Duration::days(1);
    }

    // Calculate allocated hours from tasks
    let multiplier = read_config(&config_path)
        .map(|c| c.settings.ai_estimate_multiplier)
        .unwrap_or(0.2);
    if tasks_dir.exists() {
        let (tree, _) = build_task_index(&tasks_dir, u32::MAX)?;
        allocate_task_hours(&tree, assignee_filter, &start, &end, &mut days, multiplier);
    }

    let allocated_hours: f64 = days
        .iter()
        .filter_map(|d| d.get("allocated_hours").and_then(|v| v.as_f64()))
        .sum();

    let result = json!({
        "work_days": work_days,
        "total_hours": total_hours,
        "allocated_hours": allocated_hours,
        "available_hours": (total_hours - allocated_hours).max(0.0),
        "days": days,
    });

    serde_json::to_string_pretty(&result).map_err(Into::into)
}

struct CalendarConfig {
    work_hours_per_day: f64,
    closed_weekdays: Vec<u32>,
    closed_dates: Vec<String>,
    open_dates: Vec<String>,
    day_hours: Vec<(String, f64)>,
}

impl CalendarConfig {
    fn hours_for_date(&self, date: &NaiveDate) -> f64 {
        let date_str = date.format("%Y-%m-%d").to_string();

        // Check per-date hours first
        for (d, h) in &self.day_hours {
            if d == &date_str {
                return *h;
            }
        }

        // Check if explicitly open (overrides closed weekday)
        let is_open_override = self.open_dates.iter().any(|d| d == &date_str);

        // Check closed dates
        if self.closed_dates.iter().any(|d| d == &date_str) {
            return 0.0;
        }

        // Check closed weekdays (chrono: Mon=1 .. Sun=7, we use 0=Sun..6=Sat)
        let weekday_num = match date.weekday() {
            chrono::Weekday::Sun => 0,
            chrono::Weekday::Mon => 1,
            chrono::Weekday::Tue => 2,
            chrono::Weekday::Wed => 3,
            chrono::Weekday::Thu => 4,
            chrono::Weekday::Fri => 5,
            chrono::Weekday::Sat => 6,
        };

        if !is_open_override && self.closed_weekdays.contains(&weekday_num) {
            return 0.0;
        }

        // Check per-weekday hours
        let weekday_str = match weekday_num {
            0 => "sun",
            1 => "mon",
            2 => "tue",
            3 => "wed",
            4 => "thu",
            5 => "fri",
            6 => "sat",
            _ => "",
        };
        for (d, h) in &self.day_hours {
            if d == weekday_str {
                return *h;
            }
        }

        self.work_hours_per_day
    }
}

fn parse_calendar(config_path: &std::path::Path, assignee: Option<&str>) -> Result<CalendarConfig> {
    let mut cal = CalendarConfig {
        work_hours_per_day: 8.0,
        closed_weekdays: Vec::new(),
        closed_dates: Vec::new(),
        open_dates: Vec::new(),
        day_hours: Vec::new(),
    };

    if !config_path.exists() {
        return Ok(cal);
    }

    let raw = std::fs::read_to_string(config_path).with_context(|| "Failed to read config.toml")?;
    let doc: DocumentMut = raw.parse().with_context(|| "Failed to parse config.toml")?;

    // Read project-level calendar
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
            for (k, v) in dh.iter() {
                if let Some(h) = v.as_float().or_else(|| v.as_integer().map(|i| i as f64)) {
                    cal.day_hours.push((k.to_string(), h));
                }
            }
        }
    }

    // Override with assignee-specific calendar
    if let Some(assignee_key) = assignee {
        if let Some(assignees) = doc.get("assignees").and_then(|v| v.as_table()) {
            if let Some(a) = assignees.get(assignee_key).and_then(|v| v.as_table()) {
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
                if let Some(dh) = a.get("day_hours").and_then(|v| v.as_table()) {
                    for (k, v) in dh.iter() {
                        if let Some(h) = v.as_float().or_else(|| v.as_integer().map(|i| i as f64)) {
                            cal.day_hours.push((k.to_string(), h));
                        }
                    }
                }
            }
        }
    }

    Ok(cal)
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

fn allocate_task_hours(
    tree: &[TaskIndex],
    assignee_filter: Option<&str>,
    start: &NaiveDate,
    end: &NaiveDate,
    days: &mut [Value],
    multiplier: f64,
) {
    for node in tree {
        if is_terminal_status(&node.status) {
            continue;
        }

        let matches_assignee = match assignee_filter {
            Some(f) => node.assignee.as_deref() == Some(f),
            None => true,
        };

        if matches_assignee {
            if let Some(ref sched) = node.schedule {
                if let (Some(ref sd), Some(ref dd)) = (&sched.start_date, &sched.due_date) {
                    if let (Ok(task_start), Ok(task_end)) = (
                        NaiveDate::parse_from_str(sd, "%Y-%m-%d"),
                        NaiveDate::parse_from_str(dd, "%Y-%m-%d"),
                    ) {
                        // remaining_hours is already actual AI-effort progress, so it
                        // is used as-is; the raw estimate is human-effort and gets the
                        // AI multiplier applied to derive AI-effort allocation.
                        let est = match sched.remaining_hours {
                            Some(rem) => rem,
                            None => sched.estimate_hours.unwrap_or(0.0) * multiplier,
                        };
                        let overlap_start = (*start).max(task_start);
                        let overlap_end = (*end).min(task_end);
                        if overlap_start <= overlap_end {
                            let task_days = (task_end - task_start).num_days().max(1) as f64;
                            let hours_per_day = est / task_days;

                            let mut date = overlap_start;
                            while date <= overlap_end {
                                let idx = (date - *start).num_days() as usize;
                                if idx < days.len() {
                                    if let Some(obj) = days[idx].as_object_mut() {
                                        let cur = obj
                                            .get("allocated_hours")
                                            .and_then(|v| v.as_f64())
                                            .unwrap_or(0.0);
                                        obj.insert(
                                            "allocated_hours".to_string(),
                                            json!(cur + hours_per_day),
                                        );
                                    }
                                }
                                date += chrono::Duration::days(1);
                            }
                        }
                    }
                }
            }
        }

        allocate_task_hours(
            &node.children,
            assignee_filter,
            start,
            end,
            days,
            multiplier,
        );
    }
}
