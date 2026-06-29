use anyhow::Result;
use chrono::Utc;
use serde_json::{json, Value};

use super::resolve_project_dir;
use crate::storage::ensure_handoff_exists;
use crate::storage::tasks::*;

pub fn handle(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;
    let tasks_dir = handoff.join("tasks");

    let updates = arguments
        .get("updates")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("'updates' parameter is required (array)"))?;

    let mut applied: u32 = 0;
    let mut errors: Vec<Value> = Vec::new();

    for update in updates {
        let task_id = match update.get("task_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => {
                errors.push(json!({"task_id": null, "error": "missing task_id"}));
                continue;
            }
        };

        if let Err(e) = apply_single_update(&tasks_dir, task_id, update) {
            errors.push(json!({"task_id": task_id, "error": e.to_string()}));
        } else {
            applied += 1;
        }
    }

    let result = json!({
        "applied": applied,
        "errors": errors,
    });

    serde_json::to_string_pretty(&result).map_err(Into::into)
}

fn apply_single_update(tasks_dir: &std::path::Path, task_id: &str, update: &Value) -> Result<()> {
    let task_dir = find_task_dir_by_id(tasks_dir, task_id)?
        .ok_or_else(|| anyhow::anyhow!("{}", suggest_task_id(tasks_dir, task_id)))?;

    let (mut data, current_status) = read_task(&task_dir)?
        .ok_or_else(|| anyhow::anyhow!("Task file not found for {task_id}"))?;

    if let Some(priority) = update.get("priority").and_then(|v| v.as_str()) {
        validate_priority(Some(priority))?;
        data.priority = Some(priority.to_string());
    }

    if update.get("assignee").is_some() {
        data.assignee = update
            .get("assignee")
            .and_then(|v| v.as_str())
            .map(String::from);
    }

    if let Some(sched_val) = update.get("schedule") {
        let schedule = data.schedule.get_or_insert_with(Default::default);
        if let Some(sd) = sched_val.get("start_date").and_then(|v| v.as_str()) {
            schedule.start_date = Some(sd.to_string());
        }
        if let Some(dd) = sched_val.get("due_date").and_then(|v| v.as_str()) {
            schedule.due_date = Some(dd.to_string());
        }
        if let Some(eh) = sched_val.get("estimate_hours").and_then(|v| v.as_f64()) {
            schedule.estimate_hours = Some(eh);
        }
        if let Some(ah) = sched_val.get("actual_hours").and_then(|v| v.as_f64()) {
            schedule.actual_hours = Some(ah);
        }
        if let Some(rh) = sched_val.get("remaining_hours").and_then(|v| v.as_f64()) {
            schedule.remaining_hours = Some(rh);
        }
        if let Some(ms) = sched_val.get("milestone").and_then(|v| v.as_str()) {
            schedule.milestone = Some(ms.to_string());
        }
        if let Some(p) = sched_val.get("pinned").and_then(|v| v.as_bool()) {
            schedule.pinned = Some(p);
        }
    }

    let new_status = update
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or(&current_status);

    if !is_valid_status(new_status) {
        anyhow::bail!("Invalid status: {new_status}");
    }

    if new_status == "done" && current_status != "done" {
        validate_done_transition(&task_dir, &data)?;
        data.completed_at = Some(Utc::now().to_rfc3339());
    }
    if new_status == "skipped" && current_status != "skipped" {
        validate_skipped_transition(&task_dir, &data)?;
    }

    data.updated_at = Some(Utc::now().to_rfc3339());

    if let Some((old_path, _)) = find_task_file(&task_dir)? {
        std::fs::remove_file(&old_path)?;
    }
    write_task(&task_dir, new_status, &data)?;

    Ok(())
}
