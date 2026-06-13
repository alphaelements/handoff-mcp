use anyhow::Result;
use chrono::Utc;
use serde_json::Value;

use super::resolve_project_dir;
use crate::storage::ensure_handoff_exists;
use crate::storage::tasks::{find_task_dir_by_id, find_task_file, read_task, write_task};

pub fn handle(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;

    let handoff = ensure_handoff_exists(&project_dir)?;
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
        .ok_or_else(|| anyhow::anyhow!("Task not found: {task_id}"))?;

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

    if let Some((old_path, _)) = find_task_file(&task_dir)? {
        std::fs::remove_file(&old_path)?;
    }

    write_task(&task_dir, &status, &data)?;

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
