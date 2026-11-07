use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::Value;

use super::save_context::{append_session_event, prune_sessions_per_config};
use super::{lineage, HandlerContext};
use crate::storage::config::read_config;
use crate::storage::git::capture_git_state;
use crate::storage::session_target::{own_active_group, Caller, OWNER_V};
use crate::storage::sessions::{
    generate_session_id, read_session_entries, transition_session_file, write_open_session,
    SessionData,
};
use crate::storage::tasks::*;

/// Write an imported session as `open` and record its `session.created`
/// event (carrying the caller's `agent_id`). The id is assigned here so the
/// event can name the session the writer creates.
fn write_imported_session(
    ctx: &HandlerContext,
    sessions_dir: &Path,
    mut data: SessionData,
    closed_active: Option<String>,
) -> Result<()> {
    let session_id = data.id.get_or_insert_with(generate_session_id).clone();
    // The import replaces the caller's own active session, so it continues
    // that one; with none to replace it decides like any other creation.
    let continues = match closed_active {
        Some(id) => {
            lineage::forget(&ctx.handoff_dir, &ctx.project_dir.to_string_lossy());
            vec![id]
        }
        None => {
            let entries = read_session_entries(sessions_dir)?;
            lineage::continues_for_new(&lineage::NewSessionLineage {
                ctx,
                sessions_dir,
                entries: &entries,
                explicit: None,
                token: None,
                parent: None,
                scope: crate::storage::detect_session_scope(&ctx.project_dir),
            })?
            .continues
        }
    };
    lineage::stamp_new(&mut data, continues, &ctx.project_dir);
    write_open_session(sessions_dir, &data)?;
    append_session_event(
        &ctx.handoff_dir,
        "session.created",
        Some(&session_id),
        ctx.agent_id.as_deref(),
    );
    Ok(())
}

/// Close the caller's own active sessions (and ownerless ones) before an
/// imported session replaces them. Another agent's active session is never
/// closed by an import. Returns the id of the most recently updated session it
/// closed, which the import continues.
fn close_own_active_sessions(ctx: &HandlerContext, sessions_dir: &Path) -> Result<Option<String>> {
    let worktree = ctx.project_dir.to_string_lossy().to_string();
    let caller = Caller {
        agent_id: ctx.agent_id.as_deref(),
        worktree: &worktree,
    };
    let entries = read_session_entries(sessions_dir)?;
    let mut latest: Option<(Option<&str>, &str)> = None;
    for entry in own_active_group(&entries, &caller) {
        transition_session_file(&entry.path, "closed")?;
        append_session_event(
            &ctx.handoff_dir,
            "session.closed",
            entry.data.id.as_deref(),
            ctx.agent_id.as_deref(),
        );
        if let Some(id) = entry.data.id.as_deref() {
            let key = (entry.data.ended_at.as_deref(), id);
            if latest.is_none_or(|l| key > l) {
                latest = Some(key);
            }
        }
    }
    Ok(latest.map(|(_, id)| id.to_string()))
}

/// Ownership of a session written by this call: the caller's identity (none
/// for the CLI), worktree and scope.
fn imported_owner(
    ctx: &HandlerContext,
) -> (Option<String>, Option<u32>, Option<String>, Option<String>) {
    (
        ctx.agent_id.clone(),
        ctx.agent_id.as_ref().map(|_| OWNER_V),
        Some(ctx.project_dir.to_string_lossy().to_string()),
        Some(crate::storage::detect_session_scope(&ctx.project_dir).to_string()),
    )
}

pub fn handle(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let project_dir = &ctx.project_dir;
    let handoff = &ctx.handoff_dir;
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

    let require_estimate_hours = read_config(&config_path)
        .map(|c| c.settings.require_estimate_hours)
        .unwrap_or(true);

    let mut tasks_created: u32 = 0;
    let mut top_level_count: u32 = 0;
    let mut nested_count: u32 = 0;

    if let Some(tasks) = arguments.get("tasks").and_then(|v| v.as_array()) {
        // Validate the whole payload before creating anything. Import writes many
        // tasks in one call, so a rejection discovered mid-tree would otherwise
        // leave a half-written forest behind and burn task IDs —
        // `next_top_level_id` counts directories, not task files.
        //
        // Nothing is created yet, so `next_top_level_id` keeps returning the same
        // value; project the IDs the writing pass below will assign, purely so the
        // rejection names the task the caller sent.
        let first_id = next_top_level_id(&tasks_dir)?;
        let first_n: u32 = first_id
            .strip_prefix('t')
            .and_then(|n| n.parse().ok())
            .with_context(|| format!("Unexpected task id form: {first_id}"))?;
        let mut pending_deps: Vec<(String, Vec<String>)> = Vec::new();
        for (i, task_val) in tasks.iter().enumerate() {
            let projected_id = format!("t{}", first_n + i as u32);
            validate_task_recursive(
                &projected_id,
                task_val,
                require_estimate_hours,
                &mut pending_deps,
            )?;
        }

        // Check the dependency graph once, with every task in this payload already
        // in it. Task-at-a-time checking would both reject a valid dependency on a
        // sibling still unwritten and miss a cycle confined to the payload.
        validate_dependencies_batch(&tasks_dir, &pending_deps)?;

        for task_val in tasks {
            let title = task_title(task_val)?;

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
    let mut prune_lines: Vec<String> = Vec::new();
    if let Some(session) = arguments.get("session") {
        let summary = session
            .get("summary")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                anyhow::anyhow!("'session.summary' is required when session is provided")
            })?;

        let closed_active = if skip_session_close {
            None
        } else {
            close_own_active_sessions(ctx, &sessions_dir)?
        };
        let git_state = capture_git_state(project_dir)?;
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

        let owner = imported_owner(ctx);
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
            timeline: None,
            label: None,
            parent_session_id: None,
            related_task_ids: Vec::new(),
            agent_id: owner.0,
            owner_v: owner.1,
            worktree: owner.2,
            scope: owner.3,
            ..Default::default()
        };

        write_imported_session(ctx, &sessions_dir, data, closed_active)?;

        prune_lines.extend(prune_sessions_per_config(handoff, ctx.agent_id.as_deref())?);

        session_saved = true;
    } else if let Some(raw_notes) = arguments.get("raw_notes").and_then(|v| v.as_str()) {
        if !raw_notes.is_empty() {
            let closed_active = if skip_session_close {
                None
            } else {
                close_own_active_sessions(ctx, &sessions_dir)?
            };
            let git_state = capture_git_state(project_dir)?;
            let now = Utc::now().to_rfc3339();

            let owner = imported_owner(ctx);
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
                timeline: None,
                label: None,
                parent_session_id: None,
                related_task_ids: Vec::new(),
                agent_id: owner.0,
                owner_v: owner.1,
                worktree: owner.2,
                scope: owner.3,
                ..Default::default()
            };

            write_imported_session(ctx, &sessions_dir, data, closed_active)?;

            prune_lines.extend(prune_sessions_per_config(handoff, ctx.agent_id.as_deref())?);

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
    for line in &prune_lines {
        msg.push_str("\n  ");
        msg.push_str(line);
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

// The validating pass and the writing pass must read the payload identically, or
// a task could pass validation and then be written as something else. These three
// accessors are the single source of truth for the fields the estimate rule keys
// on; both passes go through them rather than reaching into `task_val` directly.

fn task_title(task_val: &Value) -> Result<&str> {
    task_val
        .get("title")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Each task requires a 'title'"))
}

fn task_status(task_val: &Value) -> &str {
    task_val
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("todo")
}

fn task_children(task_val: &Value) -> Option<&Vec<Value>> {
    task_val.get("children").and_then(|v| v.as_array())
}

/// Check one task subtree against the rules `create_task_recursive` would
/// otherwise only discover after writing earlier siblings to disk.
///
/// `has_children` is read from the payload, not the filesystem: at this point the
/// child directories do not exist, so a filesystem probe would call every parent
/// a leaf and demand an estimate it is exempt from.
fn validate_task_recursive(
    task_id: &str,
    task_val: &Value,
    require_estimate_hours: bool,
    pending_deps: &mut Vec<(String, Vec<String>)>,
) -> Result<()> {
    let title = task_title(task_val)?;

    let status = task_status(task_val);
    if !is_valid_status(status) {
        anyhow::bail!("Invalid status: {status}");
    }

    validate_priority(task_val.get("priority").and_then(|v| v.as_str()))?;

    let children = task_children(task_val);
    // An empty `children` array creates no child tasks, so such a task is a leaf
    // and owes an estimate — matching what the writing pass will produce.
    let has_children = children.is_some_and(|c| !c.is_empty());

    // import only ever creates, so `is_create` is true and the resend example
    // carries `title` — the caller has no stored task to preserve it.
    validate_estimate_required(
        require_estimate_hours,
        task_id,
        title,
        status,
        has_children,
        true,
        extract_schedule(task_val).as_ref(),
    )?;

    // Record this task's edges under the ID the writing pass will give it, so the
    // batch cycle check below sees the graph as it will exist after the import.
    pending_deps.push((
        task_id.to_string(),
        extract_string_array_from(task_val, "dependencies"),
    ));

    if let Some(children) = children {
        for (i, child_val) in children.iter().enumerate() {
            let child_id = format!("{task_id}.{}", i + 1);
            validate_task_recursive(&child_id, child_val, require_estimate_hours, pending_deps)?;
        }
    }

    Ok(())
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

    create_task_dir_all(&task_dir)?;

    let now = Utc::now().to_rfc3339();
    let status = task_status(task_val);

    // `validate_task_recursive` has already passed on this payload, so these two
    // cannot fail here. They stay as defense-in-depth: this function writes to
    // disk, and a future caller that forgets the pre-pass must not slip an
    // invalid status or priority through unchecked.
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
        task_links: Vec::new(),
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
        lock: None,
        scope_paths: extract_string_array_from(task_val, "scope_paths"),
        extra: std::collections::HashMap::new(),
    };

    write_task(&task_dir, status, &data)?;

    let mut count: u32 = 1;

    if let Some(children) = task_children(task_val) {
        for (i, child_val) in children.iter().enumerate() {
            let child_title = task_title(child_val)?;

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
        baseline_start: sched
            .get("baseline_start")
            .and_then(|v| v.as_str())
            .map(String::from),
        baseline_due: sched
            .get("baseline_due")
            .and_then(|v| v.as_str())
            .map(String::from),
    })
}
