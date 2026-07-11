use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::Value;

use super::resolve_project_dir;
use crate::storage::config::read_config;
use crate::storage::ensure_handoff_exists;
use crate::storage::tasks::*;

pub fn handle(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;

    let handoff = ensure_handoff_exists(&project_dir)?;
    let tasks_dir = handoff.join("tasks");

    let require_estimate_hours = read_config(&handoff.join("config.toml"))
        .map(|c| c.settings.require_estimate_hours)
        .unwrap_or(true);

    let task_val = arguments
        .get("task")
        .ok_or_else(|| anyhow::anyhow!("'task' parameter is required"))?;

    let task_id = task_val.get("id").and_then(|v| v.as_str());
    let move_to = arguments.get("move_to").and_then(|v| v.as_str());

    if let Some(existing_id) = task_id {
        if let Some(new_parent_id) = move_to {
            return handle_move(&tasks_dir, existing_id, new_parent_id);
        }
        let task_exists = find_task_dir_by_id(&tasks_dir, existing_id)?.is_some();
        if task_exists {
            return handle_update(&tasks_dir, existing_id, task_val, require_estimate_hours);
        }
        return handle_upsert_create(
            &tasks_dir,
            existing_id,
            task_val,
            arguments,
            require_estimate_hours,
        );
    }

    let title = task_val
        .get("title")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'task.title' is required for new tasks"))?;

    handle_create(
        &tasks_dir,
        title,
        task_val,
        arguments,
        require_estimate_hours,
    )
}

fn handle_create(
    tasks_dir: &std::path::Path,
    title: &str,
    task_val: &Value,
    arguments: &Value,
    require_estimate_hours: bool,
) -> Result<String> {
    let parent_id = arguments.get("parent_id").and_then(|v| v.as_str());

    let (new_id, parent_dir) = match parent_id {
        Some(pid) => {
            let parent_dir = find_task_dir_by_id(tasks_dir, pid)?
                .ok_or_else(|| anyhow::anyhow!("{}", suggest_task_id(tasks_dir, pid)))?;
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

    let now = Utc::now().to_rfc3339();
    let status = task_val
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("todo");

    if !is_valid_status(status) {
        anyhow::bail!("Invalid status: {status}");
    }

    let priority = task_val.get("priority").and_then(|v| v.as_str());
    validate_priority(priority)?;

    let dependencies = extract_string_array(task_val, "dependencies");
    if !dependencies.is_empty() {
        validate_dependencies(tasks_dir, &new_id, &dependencies)?;
    }

    let data = TaskData {
        id: new_id.clone(),
        title: title.to_string(),
        notes: task_val
            .get("notes")
            .and_then(|v| v.as_str())
            .map(String::from),
        priority: priority.map(String::from),
        created_at: Some(now.clone()),
        updated_at: Some(now),
        completed_at: None,
        labels: extract_string_array(task_val, "labels"),
        links: extract_string_array(task_val, "links"),
        task_links: Vec::new(),
        done_criteria: extract_done_criteria(task_val),
        schedule: extract_schedule(task_val),
        dependencies,
        order: task_val
            .get("order")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32),
        assignee: task_val
            .get("assignee")
            .and_then(|v| v.as_str())
            .map(String::from),
        extra: HashMap::new(),
    };

    // A newly created task is always a leaf (no children yet).
    validate_estimate_required(
        require_estimate_hours,
        &new_id,
        title,
        status,
        false,
        true,
        data.schedule.as_ref(),
    )?;

    // Create the directory only once every validation has passed. A rejected
    // create must leave nothing behind: an orphan dir would burn the task ID,
    // because `next_top_level_id` counts directories, not task files.
    std::fs::create_dir_all(&task_dir)
        .with_context(|| format!("Failed to create task dir: {}", task_dir.display()))?;

    write_task(&task_dir, status, &data)?;

    Ok(format!("Created task {new_id}: {title} [{status}]"))
}

fn handle_upsert_create(
    tasks_dir: &std::path::Path,
    task_id: &str,
    task_val: &Value,
    arguments: &Value,
    require_estimate_hours: bool,
) -> Result<String> {
    let title = task_val
        .get("title")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            let hint = suggest_task_id(tasks_dir, task_id);
            anyhow::anyhow!("{hint}\nProvide 'title' to create a new task with this ID.")
        })?;

    let parent_id = arguments.get("parent_id").and_then(|v| v.as_str());

    let parent_dir = match parent_id {
        Some(pid) => find_task_dir_by_id(tasks_dir, pid)?
            .ok_or_else(|| anyhow::anyhow!("{}", suggest_task_id(tasks_dir, pid)))?,
        None => tasks_dir.to_path_buf(),
    };

    let slug = title_to_slug(title);
    let dir_name = format!("{task_id}-{slug}");
    let task_dir = parent_dir.join(&dir_name);

    let now = Utc::now().to_rfc3339();
    let status = task_val
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("todo");

    if !is_valid_status(status) {
        anyhow::bail!("Invalid status: {status}");
    }

    let priority = task_val.get("priority").and_then(|v| v.as_str());
    validate_priority(priority)?;

    let dependencies = extract_string_array(task_val, "dependencies");
    if !dependencies.is_empty() {
        validate_dependencies(tasks_dir, task_id, &dependencies)?;
    }

    let data = TaskData {
        id: task_id.to_string(),
        title: title.to_string(),
        notes: task_val
            .get("notes")
            .and_then(|v| v.as_str())
            .map(String::from),
        priority: priority.map(String::from),
        created_at: Some(now.clone()),
        updated_at: Some(now),
        completed_at: None,
        labels: extract_string_array(task_val, "labels"),
        links: extract_string_array(task_val, "links"),
        task_links: Vec::new(),
        done_criteria: extract_done_criteria(task_val),
        schedule: extract_schedule(task_val),
        dependencies,
        order: task_val
            .get("order")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32),
        assignee: task_val
            .get("assignee")
            .and_then(|v| v.as_str())
            .map(String::from),
        extra: HashMap::new(),
    };

    // Upsert-create: a brand-new task is a leaf.
    validate_estimate_required(
        require_estimate_hours,
        task_id,
        title,
        status,
        false,
        true,
        data.schedule.as_ref(),
    )?;

    // Create the directory only once every validation has passed, so a rejected
    // upsert-create leaves no orphan dir shadowing the requested ID.
    std::fs::create_dir_all(&task_dir)
        .with_context(|| format!("Failed to create task dir: {}", task_dir.display()))?;

    write_task(&task_dir, status, &data)?;

    Ok(format!("Created task {task_id}: {title} [{status}]"))
}

fn handle_update(
    tasks_dir: &std::path::Path,
    task_id: &str,
    task_val: &Value,
    require_estimate_hours: bool,
) -> Result<String> {
    let task_dir = find_task_dir_by_id(tasks_dir, task_id)?
        .ok_or_else(|| anyhow::anyhow!("{}", suggest_task_id(tasks_dir, task_id)))?;

    let (mut data, current_status) = read_task(&task_dir)?
        .ok_or_else(|| anyhow::anyhow!("Task file not found in {}", task_dir.display()))?;

    if let Some(title) = task_val.get("title").and_then(|v| v.as_str()) {
        data.title = title.to_string();
    }
    if let Some(notes) = task_val.get("notes").and_then(|v| v.as_str()) {
        data.notes = Some(notes.to_string());
    } else if let Some(append) = task_val.get("notes_append").and_then(|v| v.as_str()) {
        let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%S");
        let block = format!("--- {timestamp}\n{append}");
        match &mut data.notes {
            Some(existing) if !existing.is_empty() => {
                existing.push_str(&format!("\n\n{block}"));
            }
            _ => data.notes = Some(block),
        }
    }
    if let Some(priority) = task_val.get("priority").and_then(|v| v.as_str()) {
        validate_priority(Some(priority))?;
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
    if let Some(sched_val) = task_val.get("schedule") {
        // Field-level merge (not full replacement) so that fields not present in
        // the patch — e.g. actual_hours/remaining_hours accrued by the VSCode timer —
        // are preserved. Mirrors bulk_update_tasks. (referral ref-20260623-232823)
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
    if task_val.get("dependencies").is_some() {
        let new_deps = extract_string_array(task_val, "dependencies");
        if !new_deps.is_empty() {
            validate_dependencies(tasks_dir, task_id, &new_deps)?;
        }
        data.dependencies = new_deps;
    }
    if let Some(order) = task_val.get("order").and_then(|v| v.as_u64()) {
        data.order = Some(order as u32);
    }
    if task_val.get("assignee").is_some() {
        data.assignee = task_val
            .get("assignee")
            .and_then(|v| v.as_str())
            .map(String::from);
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

    // Parent tasks (with children) are exempt; only leaf tasks need an estimate.
    let has_children = task_has_children(&task_dir)?;
    validate_estimate_required(
        require_estimate_hours,
        task_id,
        &data.title,
        new_status,
        has_children,
        false,
        data.schedule.as_ref(),
    )?;

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
        .ok_or_else(|| anyhow::anyhow!("{}", suggest_task_id(tasks_dir, task_id)))?;

    let new_parent_dir = find_task_dir_by_id(tasks_dir, new_parent_id)?
        .ok_or_else(|| anyhow::anyhow!("{}", suggest_task_id(tasks_dir, new_parent_id)))?;

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

fn extract_schedule(val: &Value) -> Option<Schedule> {
    let sched = val.get("schedule")?;
    if sched.is_null() {
        return None;
    }
    Some(Schedule {
        start_date: sched
            .get("start_date")
            .and_then(|v| v.as_str())
            .map(String::from),
        due_date: sched
            .get("due_date")
            .and_then(|v| v.as_str())
            .map(String::from),
        estimate_hours: sched.get("estimate_hours").and_then(|v| v.as_f64()),
        actual_hours: sched.get("actual_hours").and_then(|v| v.as_f64()),
        remaining_hours: sched.get("remaining_hours").and_then(|v| v.as_f64()),
        milestone: sched
            .get("milestone")
            .and_then(|v| v.as_str())
            .map(String::from),
        pinned: sched.get("pinned").and_then(|v| v.as_bool()),
    })
}
