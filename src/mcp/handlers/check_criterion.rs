use anyhow::Result;
use chrono::Utc;
use serde_json::Value;

use super::HandlerContext;
use crate::storage::tasks::{
    find_task_dir_by_id, read_task, suggest_task_id, write_task_transition,
};

pub fn handle(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;
    let tasks_dir = handoff.join("tasks");

    let task_id = arguments
        .get("task_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'task_id' parameter is required"))?;

    let criterion_index = arguments
        .get("criterion_index")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow::anyhow!("'criterion_index' parameter is required"))?
        as usize;

    let checked = arguments
        .get("checked")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| anyhow::anyhow!("'checked' parameter is required (boolean)"))?;

    let task_dir = find_task_dir_by_id(&tasks_dir, task_id)?
        .ok_or_else(|| anyhow::anyhow!("{}", suggest_task_id(&tasks_dir, task_id)))?;

    let (mut data, status) = read_task(&task_dir)?
        .ok_or_else(|| anyhow::anyhow!("Task file not found in {}", task_dir.display()))?;

    if criterion_index >= data.done_criteria.len() {
        anyhow::bail!(
            "criterion_index {criterion_index} is out of range (task has {} criteria)",
            data.done_criteria.len()
        );
    }

    data.done_criteria[criterion_index].checked = checked;
    data.updated_at = Some(Utc::now().to_rfc3339());

    // t374/t375: write-then-remove, never remove-then-write — see
    // `write_task_transition`'s doc comment (src/storage/tasks.rs). `status`
    // is unchanged here, so this is a same-name atomic overwrite (no
    // removal).
    write_task_transition(&task_dir, &status, &status, &data)?;

    let checked_count = data.done_criteria.iter().filter(|c| c.checked).count();
    let total = data.done_criteria.len();

    let result = serde_json::json!({
        "task_id": data.id,
        "criterion_index": criterion_index,
        "item": data.done_criteria[criterion_index].item,
        "checked": checked,
        "done_criteria_summary": {
            "total": total,
            "checked": checked_count,
        }
    });

    serde_json::to_string_pretty(&result).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::handlers::HandlerContext;
    use crate::storage::tasks::{write_task, DoneCriterion, TaskData};
    use std::collections::HashMap;

    fn ctx(handoff_dir: std::path::PathBuf) -> HandlerContext {
        HandlerContext {
            agent_id: None,
            project_dir: handoff_dir.parent().unwrap().to_path_buf(),
            handoff_dir,
        }
    }

    fn make_todo_task_with_criteria(task_dir: &std::path::Path, id: &str, n: usize) {
        std::fs::create_dir_all(task_dir).unwrap();
        let data = TaskData {
            id: id.to_string(),
            title: "Test".to_string(),
            notes: None,
            priority: None,
            created_at: None,
            updated_at: None,
            completed_at: None,
            labels: Vec::new(),
            links: Vec::new(),
            task_links: Vec::new(),
            done_criteria: (0..n)
                .map(|i| DoneCriterion {
                    item: format!("criterion {i}"),
                    checked: false,
                })
                .collect(),
            schedule: None,
            dependencies: Vec::new(),
            order: None,
            assignee: None,
            lock: None,
            scope_paths: Vec::new(),
            extra: HashMap::new(),
        };
        write_task(task_dir, "todo", &data).unwrap();
    }

    /// `handle` rewrites the same-named task file (status unchanged, only
    /// `done_criteria[i].checked` flips) on every call. Before t375's fix it
    /// did `find_task_file` -> `remove_file` -> `write_task`, briefly leaving
    /// zero task files on disk; a concurrent unlocked reader (`read_task`)
    /// landing in that window would see `None` and report a spurious "Task
    /// not found". `write_task_transition` closes that window.
    #[test]
    fn concurrent_check_criterion_calls_and_read_do_not_report_task_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        let tasks_dir = handoff_dir.join("tasks");
        make_todo_task_with_criteria(&tasks_dir.join("t1-test"), "t1", 2);
        let task_dir = tasks_dir.join("t1-test");

        let c = ctx(handoff_dir);
        let writer = std::thread::spawn(move || {
            for i in 0..50 {
                handle(
                    &c,
                    &serde_json::json!({
                        "task_id": "t1",
                        "criterion_index": i % 2,
                        "checked": i % 2 == 0,
                    }),
                )
                .unwrap();
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
}
