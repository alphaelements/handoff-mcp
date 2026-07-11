use anyhow::Result;
use serde_json::Value;

use super::resolve_project_dir;
use crate::storage::ensure_handoff_exists;
use crate::storage::tasks::{find_task_dir_by_id, read_task, suggest_task_id};

pub fn handle(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;

    let handoff = ensure_handoff_exists(&project_dir)?;
    let tasks_dir = handoff.join("tasks");

    let task_id = arguments
        .get("task_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'task_id' parameter is required"))?;

    let task_dir = find_task_dir_by_id(&tasks_dir, task_id)?
        .ok_or_else(|| anyhow::anyhow!("{}", suggest_task_id(&tasks_dir, task_id)))?;

    let (data, status) = read_task(&task_dir)?
        .ok_or_else(|| anyhow::anyhow!("Task file not found in {}", task_dir.display()))?;

    // `links` stays the legacy `Vec<String>` for backward compatibility with
    // existing clients (skills / VSCode extension). `task_links` is an
    // additive field carrying the normalized, deduplicated view from the
    // `links()` accessor (wiki/130-document-management.md §9.1), so callers
    // that understand typed links (doc/url/file/task) can read them without
    // re-deriving the merge themselves.
    let normalized_links = data.links();

    let result = serde_json::json!({
        "id": data.id,
        "title": data.title,
        "status": status,
        "notes": data.notes,
        "priority": data.priority,
        "created_at": data.created_at,
        "updated_at": data.updated_at,
        "completed_at": data.completed_at,
        "labels": data.labels,
        "links": data.links,
        "task_links": normalized_links,
        "done_criteria": data.done_criteria,
        "schedule": data.schedule,
        "dependencies": data.dependencies,
        "order": data.order,
        "assignee": data.assignee,
    });

    serde_json::to_string_pretty(&result).map_err(Into::into)
}
