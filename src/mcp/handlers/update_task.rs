use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::Value;

use super::resolve_project_dir;
use crate::storage::ensure_handoff_exists;
use crate::storage::tasks::*;

pub fn handle(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;

    let handoff = ensure_handoff_exists(&project_dir)?;
    let tasks_dir = handoff.join("tasks");

    let task_val = arguments
        .get("task")
        .ok_or_else(|| anyhow::anyhow!("'task' parameter is required"))?;

    let title = task_val
        .get("title")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'task.title' is required"))?;

    let task_id = task_val.get("id").and_then(|v| v.as_str());
    let move_to = arguments.get("move_to").and_then(|v| v.as_str());

    if let Some(existing_id) = task_id {
        if let Some(new_parent_id) = move_to {
            return handle_move(&tasks_dir, existing_id, new_parent_id);
        }
        return handle_update(&tasks_dir, existing_id, task_val);
    }

    handle_create(&tasks_dir, title, task_val, arguments)
}

fn handle_create(
    tasks_dir: &std::path::Path,
    title: &str,
    task_val: &Value,
    arguments: &Value,
) -> Result<String> {
    let parent_id = arguments.get("parent_id").and_then(|v| v.as_str());

    let (new_id, parent_dir) = match parent_id {
        Some(pid) => {
            let parent_dir = find_task_dir_by_id(tasks_dir, pid)?
                .ok_or_else(|| anyhow::anyhow!("Parent task not found: {pid}"))?;
            let id = next_child_id(&parent_dir, pid)?;
            (id, parent_dir)
        }
        None => {
            let id = next_top_level_id(tasks_dir)?;
            (id, tasks_dir.to_path_buf())
        }
    };

    let slug = title_to_slug(title);
    let dir_name = format!("{new_id}-{slug}");
    let task_dir = parent_dir.join(&dir_name);
    std::fs::create_dir_all(&task_dir)
        .with_context(|| format!("Failed to create task dir: {}", task_dir.display()))?;

    let now = Utc::now().to_rfc3339();
    let status = task_val
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("todo");

    if !is_valid_status(status) {
        anyhow::bail!("Invalid status: {status}");
    }

    let data = TaskData {
        id: new_id.clone(),
        title: title.to_string(),
        notes: task_val
            .get("notes")
            .and_then(|v| v.as_str())
            .map(String::from),
        priority: task_val
            .get("priority")
            .and_then(|v| v.as_str())
            .map(String::from),
        created_at: Some(now.clone()),
        updated_at: Some(now),
        completed_at: None,
        labels: extract_string_array(task_val, "labels"),
        links: extract_string_array(task_val, "links"),
        done_criteria: extract_done_criteria(task_val),
    };

    write_task(&task_dir, status, &data)?;

    Ok(format!("Created task {new_id}: {title} [{status}]"))
}

fn handle_update(tasks_dir: &std::path::Path, task_id: &str, task_val: &Value) -> Result<String> {
    let task_dir = find_task_dir_by_id(tasks_dir, task_id)?
        .ok_or_else(|| anyhow::anyhow!("Task not found: {task_id}"))?;

    let (mut data, current_status) = read_task(&task_dir)?
        .ok_or_else(|| anyhow::anyhow!("Task file not found in {}", task_dir.display()))?;

    if let Some(title) = task_val.get("title").and_then(|v| v.as_str()) {
        data.title = title.to_string();
    }
    if let Some(notes) = task_val.get("notes").and_then(|v| v.as_str()) {
        data.notes = Some(notes.to_string());
    }
    if let Some(priority) = task_val.get("priority").and_then(|v| v.as_str()) {
        data.priority = Some(priority.to_string());
    }
    if task_val.get("labels").is_some() {
        data.labels = extract_string_array(task_val, "labels");
    }
    if task_val.get("links").is_some() {
        data.links = extract_string_array(task_val, "links");
    }
    if task_val.get("done_criteria").is_some() {
        data.done_criteria = extract_done_criteria(task_val);
    }

    let new_status = task_val
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

    Ok(format!(
        "Updated task {task_id}: {} [{new_status}]",
        data.title
    ))
}

fn handle_move(tasks_dir: &std::path::Path, task_id: &str, new_parent_id: &str) -> Result<String> {
    let task_dir = find_task_dir_by_id(tasks_dir, task_id)?
        .ok_or_else(|| anyhow::anyhow!("Task not found: {task_id}"))?;

    let new_parent_dir = find_task_dir_by_id(tasks_dir, new_parent_id)?
        .ok_or_else(|| anyhow::anyhow!("New parent task not found: {new_parent_id}"))?;

    let dir_name = task_dir
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("Invalid task dir"))?;

    let dest = new_parent_dir.join(dir_name);

    std::fs::rename(&task_dir, &dest).with_context(|| {
        format!(
            "Failed to move {} -> {}",
            task_dir.display(),
            dest.display()
        )
    })?;

    Ok(format!("Moved task {task_id} under {new_parent_id}"))
}

fn extract_string_array(val: &Value, key: &str) -> Vec<String> {
    val.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn extract_done_criteria(val: &Value) -> Vec<DoneCriterion> {
    val.get("done_criteria")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| {
                    let item = v.get("item")?.as_str()?;
                    let checked = v.get("checked").and_then(|c| c.as_bool()).unwrap_or(false);
                    Some(DoneCriterion {
                        item: item.to_string(),
                        checked,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}
