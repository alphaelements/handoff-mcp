use std::collections::HashMap;

use anyhow::Result;
use chrono::Utc;
use serde_json::{json, Value};

use super::resolve_project_dir;
use crate::storage::ensure_handoff_exists;
use crate::storage::tasks::{build_task_index, is_terminal_status, TaskIndex};

pub fn handle(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;
    let tasks_dir = handoff.join("tasks");

    let assignee_filter = arguments.get("assignee").and_then(|v| v.as_str());

    let (tree, _) = build_task_index(&tasks_dir, u32::MAX)?;

    let today = Utc::now().format("%Y-%m-%d").to_string();

    let mut total: u32 = 0;
    let mut by_status: HashMap<String, u32> = HashMap::new();
    let mut estimate_sum: f64 = 0.0;
    let mut actual_sum: f64 = 0.0;
    let mut remaining_sum: f64 = 0.0;
    let mut overdue_tasks: Vec<Value> = Vec::new();
    let mut milestones: HashMap<String, (u32, u32, f64, f64)> = HashMap::new();

    collect_metrics(
        &tree,
        assignee_filter,
        &today,
        &mut total,
        &mut by_status,
        &mut estimate_sum,
        &mut actual_sum,
        &mut remaining_sum,
        &mut overdue_tasks,
        &mut milestones,
    );

    let done_count = *by_status.get("done").unwrap_or(&0);
    let skipped_count = *by_status.get("skipped").unwrap_or(&0);
    let completion_percent = if total > 0 {
        ((done_count + skipped_count) as f64 / total as f64) * 100.0
    } else {
        0.0
    };

    let budget = read_budget(&handoff);

    let milestone_list: Vec<Value> = milestones
        .into_iter()
        .map(|(name, (done, ms_total, est, act))| {
            json!({
                "name": name,
                "done": done,
                "total": ms_total,
                "estimate_hours": est,
                "actual_hours": act,
            })
        })
        .collect();

    let result = json!({
        "total": total,
        "by_status": by_status,
        "completion_percent": (completion_percent * 10.0).round() / 10.0,
        "total_estimate_hours": estimate_sum,
        "total_actual_hours": actual_sum,
        "total_remaining_hours": remaining_sum,
        "overdue_count": overdue_tasks.len(),
        "overdue_tasks": overdue_tasks,
        "budget": budget,
        "milestones": milestone_list,
    });

    serde_json::to_string_pretty(&result).map_err(Into::into)
}

#[allow(clippy::too_many_arguments)]
fn collect_metrics(
    tree: &[TaskIndex],
    assignee_filter: Option<&str>,
    today: &str,
    total: &mut u32,
    by_status: &mut HashMap<String, u32>,
    estimate_sum: &mut f64,
    actual_sum: &mut f64,
    remaining_sum: &mut f64,
    overdue_tasks: &mut Vec<Value>,
    milestones: &mut HashMap<String, (u32, u32, f64, f64)>,
) {
    for node in tree {
        let task_assignee = node.assignee.as_deref();
        let include = match assignee_filter {
            Some(f) => task_assignee == Some(f),
            None => true,
        };

        if include {
            *total += 1;
            *by_status.entry(node.status.clone()).or_insert(0) += 1;

            if let Some(ref sched) = node.schedule {
                if let Some(est) = sched.estimate_hours {
                    *estimate_sum += est;
                }
                if let Some(act) = sched.actual_hours {
                    *actual_sum += act;
                }
                if let Some(rem) = sched.remaining_hours {
                    *remaining_sum += rem;
                }

                if let Some(ref due) = sched.due_date {
                    if !is_terminal_status(&node.status) && due.as_str() < today {
                        let days_overdue = days_between(due, today).unwrap_or(0);
                        overdue_tasks.push(json!({
                            "id": node.id,
                            "title": node.title,
                            "due_date": due,
                            "days_overdue": days_overdue,
                        }));
                    }
                }

                if let Some(ref ms) = sched.milestone {
                    let entry = milestones.entry(ms.clone()).or_insert((0, 0, 0.0, 0.0));
                    entry.1 += 1;
                    if is_terminal_status(&node.status) {
                        entry.0 += 1;
                    }
                    if let Some(est) = sched.estimate_hours {
                        entry.2 += est;
                    }
                    if let Some(act) = sched.actual_hours {
                        entry.3 += act;
                    }
                }
            }
        }

        collect_metrics(
            &node.children,
            assignee_filter,
            today,
            total,
            by_status,
            estimate_sum,
            actual_sum,
            remaining_sum,
            overdue_tasks,
            milestones,
        );
    }
}

fn days_between(from: &str, to: &str) -> Option<i64> {
    let from_date = chrono::NaiveDate::parse_from_str(from, "%Y-%m-%d").ok()?;
    let to_date = chrono::NaiveDate::parse_from_str(to, "%Y-%m-%d").ok()?;
    Some((to_date - from_date).num_days())
}

fn read_budget(handoff: &std::path::Path) -> Value {
    let config_path = handoff.join("config.toml");
    let content = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(_) => return Value::Null,
    };

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("total_hours") {
            if let Some(val_str) = trimmed.split('=').nth(1) {
                if let Ok(total) = val_str.trim().parse::<f64>() {
                    return json!({ "total_hours": total });
                }
            }
        }
    }

    Value::Null
}
