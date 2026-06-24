use std::cell::Cell;

use anyhow::Result;
use chrono::Utc;
use serde_json::Value;

use super::resolve_project_dir;
use crate::storage::ensure_handoff_exists;
use crate::storage::tasks::{find_task_dir_by_id, read_modify_write_task};

pub fn handle(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;

    let handoff = ensure_handoff_exists(&project_dir)?;
    let tasks_dir = handoff.join("tasks");

    let task_id = arguments
        .get("task_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'task_id' parameter is required"))?;

    let hours = arguments
        .get("hours")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| anyhow::anyhow!("'hours' parameter is required (number)"))?;

    if hours <= 0.0 {
        anyhow::bail!("'hours' must be positive");
    }

    let task_dir = find_task_dir_by_id(&tasks_dir, task_id)?.ok_or_else(|| {
        anyhow::anyhow!(
            "Task not found: {task_id}. Use handoff_list_tasks to see available task IDs."
        )
    })?;

    // Capture the post-update values for the response message. read_modify_write
    // re-runs the closure on a concurrent-write retry, so the cells always hold
    // the values from the committed attempt.
    let new_actual = Cell::new(0.0_f64);
    let new_remaining: Cell<Option<f64>> = Cell::new(None);

    read_modify_write_task(&task_dir, |data, status| {
        let schedule = data.schedule.get_or_insert_with(Default::default);
        let actual = schedule.actual_hours.unwrap_or(0.0) + hours;
        schedule.actual_hours = Some(actual);
        new_actual.set(actual);

        if let Some(rem) = schedule.remaining_hours {
            let r = (rem - hours).max(0.0);
            schedule.remaining_hours = Some(r);
            new_remaining.set(Some(r));
        } else {
            new_remaining.set(None);
        }

        data.updated_at = Some(Utc::now().to_rfc3339());
        Ok(status.to_string())
    })?;

    let remaining_msg = match new_remaining.get() {
        Some(r) => format!(", remaining={r:.1}h"),
        None => String::new(),
    };

    Ok(format!(
        "Logged {hours:.1}h on {task_id}: actual={:.1}h{remaining_msg}",
        new_actual.get()
    ))
}
