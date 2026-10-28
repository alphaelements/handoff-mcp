use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde_json::{json, Value};

use super::HandlerContext;
use crate::scheduling::calendar::parse_calendar_for;
use crate::storage::config::read_config;
use crate::storage::tasks::{build_task_index, is_terminal_status, TaskIndex};

pub fn handle(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;
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

    let calendar = parse_calendar_for(&config_path, assignee_filter)?;

    let mut days: Vec<Value> = Vec::new();
    let mut total_hours: f64 = 0.0;
    let mut work_days: u32 = 0;

    let mut date = start;
    while date <= end {
        let hours = calendar.capacity_for(&date);
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
