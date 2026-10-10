use std::cell::Cell;

use anyhow::Result;
use chrono::Utc;
use serde_json::Value;

use super::HandlerContext;
use crate::storage::tasks::{find_task_dir_by_id, read_modify_write_task, suggest_task_id};
use crate::storage::time_log::{append_time_log, TimeLogEntry};

pub fn handle(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;
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

    let note = arguments
        .get("note")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let task_dir = find_task_dir_by_id(&tasks_dir, task_id)?
        .ok_or_else(|| anyhow::anyhow!("{}", suggest_task_id(&tasks_dir, task_id)))?;

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

    // Append to the time series only after the actual_hours update committed
    // (the closure above may re-run on a concurrent-write retry, so appending
    // inside it could duplicate entries). Best-effort: the hours are already
    // recorded, so a log failure must not fail the call — a retry would
    // double-count actual_hours. It is surfaced in the response instead.
    let log_warning = append_time_log(
        handoff,
        &TimeLogEntry {
            ts: Utc::now().to_rfc3339(),
            task_id: task_id.to_string(),
            hours,
            agent_id: ctx.agent_id.clone(),
            note,
        },
    )
    .err()
    .map(|e| {
        format!("\nWarning: hours were recorded but the time log entry could not be written: {e:#}")
    })
    .unwrap_or_default();

    let remaining_msg = match new_remaining.get() {
        Some(r) => format!(", remaining={r:.1}h"),
        None => String::new(),
    };

    Ok(format!(
        "Logged {hours:.1}h on {task_id}: actual={:.1}h{remaining_msg}{log_warning}",
        new_actual.get()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::time_log::read_time_log;
    use serde_json::json;

    #[test]
    fn entry_carries_context_agent_id() {
        let dir = tempfile::tempdir().unwrap();
        let handoff_dir = dir.path().join(".handoff");
        let task_dir = handoff_dir.join("tasks/t1");
        std::fs::create_dir_all(&task_dir).unwrap();
        let data = crate::storage::tasks::TaskData {
            id: "t1".to_string(),
            title: "T".to_string(),
            notes: None,
            priority: None,
            created_at: None,
            updated_at: None,
            completed_at: None,
            labels: Vec::new(),
            links: Vec::new(),
            task_links: Vec::new(),
            done_criteria: Vec::new(),
            schedule: None,
            dependencies: Vec::new(),
            order: None,
            assignee: None,
            lock: None,
            scope_paths: Vec::new(),
            extra: Default::default(),
        };
        crate::storage::tasks::write_task(&task_dir, "todo", &data).unwrap();

        let ctx = HandlerContext {
            agent_id: Some("agent-7".to_string()),
            project_dir: dir.path().to_path_buf(),
            handoff_dir: handoff_dir.clone(),
        };
        handle(&ctx, &json!({"task_id": "t1", "hours": 0.25, "note": "n"})).unwrap();

        let log = read_time_log(&handoff_dir).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].agent_id.as_deref(), Some("agent-7"));
        assert_eq!(log[0].note.as_deref(), Some("n"));
        assert_eq!(log[0].hours, 0.25);
    }
}
