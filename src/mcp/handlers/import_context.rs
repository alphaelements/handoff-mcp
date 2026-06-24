use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::Value;

use super::resolve_project_dir;
use crate::storage::config::read_config;
use crate::storage::ensure_handoff_exists;
use crate::storage::git::capture_git_state;
use crate::storage::sessions::{
    close_active_sessions, enforce_history_limit, write_open_session, SessionData,
};
use crate::storage::tasks::*;

pub fn handle(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;
    let tasks_dir = handoff.join("tasks");
    let sessions_dir = handoff.join("sessions");
    let config_path = handoff.join("config.toml");

    let source = arguments
        .get("source")
        .ok_or_else(|| anyhow::anyhow!("'source' parameter is required"))?;
    let source_description = source
        .get("description")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'source.description' is required"))?;
    let source_format = source
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("other");

    let mut tasks_created: u32 = 0;
    let mut top_level_count: u32 = 0;
    let mut nested_count: u32 = 0;

    if let Some(tasks) = arguments.get("tasks").and_then(|v| v.as_array()) {
        for task_val in tasks {
            let title = task_val
                .get("title")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Each task requires a 'title'"))?;

            let task_id = next_top_level_id(&tasks_dir)?;
            let count = create_task_recursive(&tasks_dir, &task_id, None, title, task_val)?;
            tasks_created += count;
            top_level_count += 1;
            nested_count += count - 1;
        }
    }

    let skip_session_close = arguments
        .get("skip_session_close")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut session_saved = false;
    if let Some(session) = arguments.get("session") {
        let summary = session
            .get("summary")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                anyhow::anyhow!("'session.summary' is required when session is provided")
            })?;

        if !skip_session_close {
            close_active_sessions(&sessions_dir)?;
        }
        let git_state = capture_git_state(&project_dir)?;
        let now = Utc::now().to_rfc3339();

        let mut handoff_notes = extract_array(session, "handoff_notes");

        if let Some(raw_notes) = arguments.get("raw_notes").and_then(|v| v.as_str()) {
            if !raw_notes.is_empty() {
                handoff_notes.push(serde_json::json!({
                    "note": raw_notes,
                    "category": "context"
                }));
            }
        }

        let mut environment = session
            .get("environment")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        if let Some(env_obj) = environment.as_object_mut() {
            env_obj.insert(
                "import_source".to_string(),
                serde_json::json!({
                    "description": source_description,
                    "format": source_format
                }),
            );
        }

        let data = SessionData {
            version: 2,
            id: None,
            ended_at: Some(now),
            summary: summary.to_string(),
            branch: Some(git_state.branch),
            commit: Some(git_state.commit),
            dirty_files: git_state.dirty_files,
            decisions: extract_array(session, "decisions"),
            blockers: extract_string_array(session, "blockers"),
            checklist: extract_array(session, "checklist"),
            handoff_notes,
            references: extract_array(session, "references"),
            context_pointers: extract_array(session, "context_pointers"),
            environment: Some(environment),
        };

        write_open_session(&sessions_dir, &data)?;

        let history_limit = if config_path.exists() {
            read_config(&config_path)
                .map(|c| c.settings.history_limit)
                .unwrap_or(20)
        } else {
            20
        };
        enforce_history_limit(&sessions_dir, history_limit)?;

        session_saved = true;
    } else if let Some(raw_notes) = arguments.get("raw_notes").and_then(|v| v.as_str()) {
        if !raw_notes.is_empty() {
            if !skip_session_close {
                close_active_sessions(&sessions_dir)?;
            }
            let git_state = capture_git_state(&project_dir)?;
            let now = Utc::now().to_rfc3339();

            let data = SessionData {
                version: 2,
                id: None,
                ended_at: Some(now),
                summary: format!("[import] {source_description}"),
                branch: Some(git_state.branch),
                commit: Some(git_state.commit),
                dirty_files: git_state.dirty_files,
                decisions: Vec::new(),
                blockers: Vec::new(),
                checklist: Vec::new(),
                handoff_notes: vec![serde_json::json!({
                    "note": raw_notes,
                    "category": "context"
                })],
                references: Vec::new(),
                context_pointers: Vec::new(),
                environment: Some(serde_json::json!({
                    "import_source": {
                        "description": source_description,
                        "format": source_format
                    }
                })),
            };

            write_open_session(&sessions_dir, &data)?;

            let history_limit = if config_path.exists() {
                read_config(&config_path)
                    .map(|c| c.settings.history_limit)
                    .unwrap_or(20)
            } else {
                20
            };
            enforce_history_limit(&sessions_dir, history_limit)?;

            session_saved = true;
        }
    }

    let mut msg = format!("Import complete:\n  Source: {source_description}");

    if tasks_created > 0 {
        msg.push_str(&format!(
            "\n  Tasks created: {tasks_created} ({top_level_count} top-level, {nested_count} nested)"
        ));
    } else {
        msg.push_str("\n  Tasks created: 0");
    }

    if session_saved {
        msg.push_str("\n  Session saved: yes");
    }

    if arguments
        .get("raw_notes")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
    {
        msg.push_str("\n  Raw notes: saved as handoff_note (context)");
    }

    Ok(msg)
}

fn create_task_recursive(
    tasks_dir: &std::path::Path,
    task_id: &str,
    parent_dir: Option<&std::path::Path>,
    title: &str,
    task_val: &Value,
) -> Result<u32> {
    let slug = title_to_slug(title);
    let dir_name = format!("{task_id}-{slug}");
    let base_dir = parent_dir.unwrap_or(tasks_dir);
    let task_dir = base_dir.join(&dir_name);

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

    let priority = task_val.get("priority").and_then(|v| v.as_str());
    validate_priority(priority)?;

    let completed_at = if is_terminal_status(status) {
        Some(now.clone())
    } else {
        None
    };

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
        completed_at,
        labels: extract_string_array_from(task_val, "labels"),
        links: extract_string_array_from(task_val, "links"),
        done_criteria: extract_done_criteria(task_val),
        schedule: extract_schedule(task_val),
        dependencies: extract_string_array_from(task_val, "dependencies"),
        order: task_val
            .get("order")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32),
        assignee: task_val
            .get("assignee")
            .and_then(|v| v.as_str())
            .map(String::from),
        extra: std::collections::HashMap::new(),
    };

    write_task(&task_dir, status, &data)?;

    let mut count: u32 = 1;

    if let Some(children) = task_val.get("children").and_then(|v| v.as_array()) {
        for (i, child_val) in children.iter().enumerate() {
            let child_title = child_val
                .get("title")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Each child task requires a 'title'"))?;

            let child_id = format!("{task_id}.{}", i + 1);
            count += create_task_recursive(
                &task_dir,
                &child_id,
                Some(&task_dir),
                child_title,
                child_val,
            )?;
        }
    }

    Ok(count)
}

fn extract_array(val: &Value, key: &str) -> Vec<Value> {
    val.get(key)
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
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

fn extract_string_array_from(val: &Value, key: &str) -> Vec<String> {
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
