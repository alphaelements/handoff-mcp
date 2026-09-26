use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::Utc;
use fs2::FileExt;
use serde_json::Value;

use super::HandlerContext;
use crate::storage::config::read_config;
use crate::storage::tasks::*;

pub fn handle(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;
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
            return handle_update(
                &tasks_dir,
                existing_id,
                task_val,
                require_estimate_hours,
                ctx.agent_id.as_deref(),
                handoff,
            );
        }
        return handle_upsert_create(
            &tasks_dir,
            existing_id,
            task_val,
            arguments,
            require_estimate_hours,
            handoff,
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
        handoff,
    )
}

/// Applies `task.requirement_ids` (t330.1) right after a brand-new task has
/// been written to disk, for both creation paths (`handle_create` and
/// `handle_upsert_create`). Non-fatal: any warnings returned by
/// `link_requirements_to_task` (e.g. unresolved stable_ids) are appended to
/// the handler's plain confirmation message rather than failing the create.
fn append_requirement_link_warnings(
    handoff_dir: &std::path::Path,
    task_id: &str,
    task_val: &Value,
    msg: &mut String,
) -> Result<()> {
    if task_val.get("requirement_ids").is_none() {
        return Ok(());
    }
    let stable_ids = extract_string_array(task_val, "requirement_ids");
    if stable_ids.is_empty() {
        return Ok(());
    }
    let warnings =
        crate::mcp::handlers::docs::link_requirements_to_task(handoff_dir, task_id, &stable_ids)?;
    for warning in &warnings {
        msg.push_str(&format!("\n{warning}"));
    }
    Ok(())
}

/// Like `append_requirement_link_warnings`, but for updating an existing task:
/// computes the diff between the task's currently-linked requirement stable_ids
/// (from `task_links` with `link_type == "requirement"`) and the new
/// `requirement_ids`, then applies the added and removed stable_ids in one
/// call.
///
/// P-M3 (wiki/240 §4, review round 2 MAJOR fix): `to_add` and `to_remove` are
/// both passed to a single `apply_requirement_links` call rather than to the
/// add-only/remove-only helpers separately — a `requirement_ids` update that
/// both adds and removes stable_ids in the same `handoff_update_task` call
/// (the common "swap one requirement for another" case) must not pay for two
/// `DocSet` loads, two summary writes, and two task read-modify-writes when
/// one of each does the whole job.
fn apply_requirement_ids_diff(
    handoff_dir: &std::path::Path,
    task_id: &str,
    task_val: &Value,
    existing_task_links: &[crate::storage::tasks::TaskLink],
    msg: &mut String,
) -> Result<()> {
    if task_val.get("requirement_ids").is_none() {
        return Ok(());
    }
    let new_ids: std::collections::HashSet<String> =
        extract_string_array(task_val, "requirement_ids")
            .into_iter()
            .collect();

    let old_ids: std::collections::HashSet<String> = existing_task_links
        .iter()
        .filter(|l| l.link_type == "requirement")
        .filter_map(|l| l.label.clone())
        .collect();

    let to_add: Vec<String> = new_ids.difference(&old_ids).cloned().collect();
    let to_remove: Vec<String> = old_ids.difference(&new_ids).cloned().collect();

    if to_add.is_empty() && to_remove.is_empty() {
        return Ok(());
    }

    let warnings = crate::mcp::handlers::docs::apply_requirement_links(
        handoff_dir,
        task_id,
        &to_add,
        &to_remove,
    )?;
    for warning in &warnings {
        msg.push_str(&format!("\n{warning}"));
    }

    Ok(())
}

fn handle_create(
    tasks_dir: &std::path::Path,
    title: &str,
    task_val: &Value,
    arguments: &Value,
    require_estimate_hours: bool,
    handoff_dir: &std::path::Path,
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
        lock: None,
        scope_paths: extract_string_array(task_val, "scope_paths"),
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

    // Requirements-traceability P0 (t330.1 rework): `requirement_ids` must be
    // honored on create too, not only on a follow-up update. Runs after
    // `write_task` above so the task file exists before
    // `link_requirements_to_task` resolves and reverse-links it.
    let mut msg = format!("Created task {new_id}: {title} [{status}]");
    append_requirement_link_warnings(handoff_dir, &new_id, task_val, &mut msg)?;

    if status != "todo" && status != "blocked" {
        let task_data = read_task(&task_dir)?
            .map(|(d, _)| d.task_links)
            .unwrap_or_default();
        if let Err(e) =
            crate::mcp::handlers::docs::propagate_dev_stage_for_task(handoff_dir, &task_data)
        {
            msg.push_str(&format!("\nWarning: dev_stage propagation failed: {e}"));
        }
    }

    Ok(msg)
}

fn handle_upsert_create(
    tasks_dir: &std::path::Path,
    task_id: &str,
    task_val: &Value,
    arguments: &Value,
    require_estimate_hours: bool,
    handoff_dir: &std::path::Path,
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
        lock: None,
        scope_paths: extract_string_array(task_val, "scope_paths"),
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

    // Requirements-traceability P0 (t330.1 rework): same rationale as
    // `handle_create` above — upsert-create is a create path too and must
    // honor `requirement_ids` in the same call.
    let mut msg = format!("Created task {task_id}: {title} [{status}]");
    append_requirement_link_warnings(handoff_dir, task_id, task_val, &mut msg)?;

    if status != "todo" && status != "blocked" {
        let task_data = read_task(&task_dir)?
            .map(|(d, _)| d.task_links)
            .unwrap_or_default();
        if let Err(e) =
            crate::mcp::handlers::docs::propagate_dev_stage_for_task(handoff_dir, &task_data)
        {
            msg.push_str(&format!("\nWarning: dev_stage propagation failed: {e}"));
        }
    }

    Ok(msg)
}

/// Update an existing task. The whole read-modify-write cycle below is
/// guarded by an exclusive `flock` on the task directory's `.lock` file, so a
/// concurrent `handoff_claim_task`/`handoff_release_task`/lease-expiry scan —
/// or another `handoff_update_task` call in a different process — cannot
/// interleave with this one and silently drop either side's write (spec
/// 3.3/7.1, mirrors `read_modify_write_task_locked`).
fn handle_update(
    tasks_dir: &std::path::Path,
    task_id: &str,
    task_val: &Value,
    require_estimate_hours: bool,
    agent_id: Option<&str>,
    handoff_dir: &std::path::Path,
) -> Result<String> {
    let task_dir = find_task_dir_by_id(tasks_dir, task_id)?
        .ok_or_else(|| anyhow::anyhow!("{}", suggest_task_id(tasks_dir, task_id)))?;

    let lock_file = crate::storage::tasks::open_lock_file(&task_dir)?;
    lock_file
        .lock_exclusive()
        .with_context(|| format!("Failed to acquire flock on {}", task_dir.display()))?;

    let result = handle_update_locked(
        tasks_dir,
        task_id,
        &task_dir,
        task_val,
        require_estimate_hours,
        agent_id,
        handoff_dir,
    );

    // P-M7 (wiki/240-performance-design.md §3 C8, §4): release the flock
    // *before* running dev_stage propagation, not after. `handle_update_locked`
    // above only performs this task's own read-modify-write and returns the
    // links to propagate (if any) without acting on them — propagation is a
    // separate, self-contained document read-modify-write
    // (`propagate_dev_stage_for_task`, optimistic-locked per document,
    // wiki/240 §4 P-M7) that does not touch *this* task's file and gains
    // nothing from holding its flock. Every other writer contending for this
    // same task (a concurrent `handoff_update_task`, `handoff_claim_task`,
    // lease-expiry scan) previously had to wait out the full propagation
    // pass for no correctness reason.
    let _ = fs2::FileExt::unlock(&lock_file);

    let (mut msg, propagate_links) = result?;
    if let Some(links) = propagate_links {
        if let Err(e) =
            crate::mcp::handlers::docs::propagate_dev_stage_for_task(handoff_dir, &links)
        {
            msg.push_str(&format!("\nWarning: dev_stage propagation failed: {e}"));
        }
    }
    Ok(msg)
}

/// Runs the flock-protected read-modify-write for `handoff_update_task` on an
/// existing task. Returns the response message plus, when this update
/// changed the task's status, the `task_links` [`propagate_dev_stage_for_task`]
/// should be run against — the caller (`handle_update`) runs that call
/// itself *after* releasing the flock this function was called under (P-M7,
/// wiki/240 §3 C8) rather than this function calling it directly while still
/// locked.
fn handle_update_locked(
    tasks_dir: &std::path::Path,
    task_id: &str,
    task_dir: &std::path::Path,
    task_val: &Value,
    require_estimate_hours: bool,
    agent_id: Option<&str>,
    handoff_dir: &std::path::Path,
) -> Result<(String, Option<Vec<TaskLink>>)> {
    let (mut data, current_status) = read_task(task_dir)?
        .ok_or_else(|| anyhow::anyhow!("Task file not found in {}", task_dir.display()))?;

    // Advisory warning (spec 3.3.5, 7.2): the caller's write is never
    // rejected over a claim held by another agent — only flagged, so the
    // claiming agent can be told a concurrent edit landed on their task.
    // Captured from the lock as read, before any mutation below (including
    // the done-transition's own `data.lock = None`) can change it.
    let advisory_warning = match (agent_id, data.lock.as_ref()) {
        (Some(caller), Some(lock)) if lock.agent_id != caller => Some(format!(
            "Advisory: Task {task_id} is claimed by agent {}. Your update was applied but \
             may conflict with the claiming agent's work.",
            lock.agent_id
        )),
        _ => None,
    };

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
    if task_val.get("scope_paths").is_some() {
        data.scope_paths = extract_string_array(task_val, "scope_paths");
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
        validate_done_transition(task_dir, &data)?;
        data.completed_at = Some(Utc::now().to_rfc3339());
        // Moving to done always releases any outstanding claim lease: a
        // finished task has nothing left to protect from concurrent work.
        // Record a task.released event, mirroring handoff_release_task, so
        // the event log reflects the lease being given up here too.
        if let Some(lock) = data.lock.take() {
            let _ = crate::storage::events::append_event(
                handoff_dir,
                crate::storage::events::EventRecord {
                    ts: Utc::now().to_rfc3339(),
                    event: "task.released".to_string(),
                    task_id: Some(task_id.to_string()),
                    agent_id: Some(lock.agent_id.clone()),
                    session_id: Some(lock.session_id),
                    detail: Some("revert_status=done".to_string()),
                },
            );
            // Same bookkeeping as handoff_release_task (t250.6, FR-2.6): the
            // lock owner's AgentRecord.claimed_tasks must drop this task too,
            // since a done task can no longer be "claimed". Best-effort, same
            // rationale as the event log above.
            let _ =
                crate::storage::agents::remove_claimed_task(handoff_dir, &lock.agent_id, task_id);
        }
    }

    if new_status == "skipped" && current_status != "skipped" {
        validate_skipped_transition(task_dir, &data)?;
    }

    // Lease auto-extension (spec: update_task keeps a claim alive while its
    // owning agent keeps working the task). Only extends when the caller's
    // agent_id matches the lock owner; an unset ctx.agent_id (agent identity
    // not yet wired end-to-end, t240.12) or an update from a different agent
    // leaves the existing lease/expiry untouched rather than guessing.
    if let (Some(agent_id), Some(lock)) = (agent_id, data.lock.as_mut()) {
        if lock.agent_id == agent_id {
            let now = Utc::now();
            lock.lease_expires_at =
                (now + chrono::Duration::seconds(lock.lease_ttl_seconds as i64)).to_rfc3339();
        }
    }

    // Parent tasks (with children) are exempt; only leaf tasks need an estimate.
    let has_children = task_has_children(task_dir)?;
    validate_estimate_required(
        require_estimate_hours,
        task_id,
        &data.title,
        new_status,
        has_children,
        false,
        data.schedule.as_ref(),
    )?;

    // Snapshot existing task_links before write_task — needed for diff-based
    // requirement_ids handling below.
    let existing_task_links = data.task_links.clone();

    data.updated_at = Some(Utc::now().to_rfc3339());

    if let Some((old_path, _)) = find_task_file(task_dir)? {
        std::fs::remove_file(&old_path)?;
    }

    write_task(task_dir, new_status, &data)?;

    // Requirements-traceability: on update, compute the diff between the
    // task's currently-linked requirement stable_ids and the new
    // requirement_ids, then unlink removed and link added. Must run *after*
    // write_task (link/unlink functions use read_modify_write_task).
    let mut msg = format!("Updated task {task_id}: {} [{new_status}]", data.title);
    if let Some(warning) = advisory_warning {
        msg.push_str(&format!("\n{warning}"));
    }
    apply_requirement_ids_diff(
        handoff_dir,
        task_id,
        task_val,
        &existing_task_links,
        &mut msg,
    )?;

    // dev_stage propagation to linked requirement SubItems, when this update
    // changed task status, must run *after* the caller releases this task's
    // flock (P-M7, wiki/240 §3 C8) — it is a separate, self-contained
    // document read-modify-write that never touches this task's own file, so
    // holding this task's lock across it only makes every other writer
    // contending for this task wait longer with no correctness benefit.
    // Re-read task_links after apply_requirement_ids_diff (which may have
    // added/removed links) while still under the lock, since that read must
    // see this update's own write.
    let propagate_links = if new_status != current_status {
        let current_links = if task_val.get("requirement_ids").is_some() {
            read_task(task_dir)?
                .map(|(d, _)| d.task_links)
                .unwrap_or_default()
        } else {
            data.task_links.clone()
        };
        Some(current_links)
    } else {
        None
    };

    Ok((msg, propagate_links))
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

#[cfg(test)]
mod lease_tests {
    use super::*;

    fn make_todo_task(task_dir: &std::path::Path, id: &str) {
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
            done_criteria: Vec::new(),
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

    #[test]
    fn handle_update_extends_lease_when_agent_id_matches_lock_owner() {
        let tmp = tempfile::tempdir().unwrap();
        let tasks_dir = tmp.path().join("tasks");
        let task_dir = tasks_dir.join("t1-test");
        make_todo_task(&task_dir, "t1");

        crate::storage::tasks::claim_task(&task_dir, "agent-1", "session-1", 1800, tmp.path())
            .unwrap();
        let (before, _) = read_task(&task_dir).unwrap().unwrap();
        let expires_before = before.lock.as_ref().unwrap().lease_expires_at.clone();

        // Simulate time passing by asserting the update handler recomputes a
        // fresh `now + ttl` expiry (a later timestamp) rather than merely
        // preserving the same value.
        std::thread::sleep(std::time::Duration::from_millis(1100));

        handle_update(
            &tasks_dir,
            "t1",
            &serde_json::json!({ "notes": "still working" }),
            false,
            Some("agent-1"),
            tmp.path(),
        )
        .unwrap();

        let (after, _) = read_task(&task_dir).unwrap().unwrap();
        let lock = after.lock.expect("lock should still be present");
        assert_eq!(lock.agent_id, "agent-1");
        assert!(
            lock.lease_expires_at > expires_before,
            "lease should have been extended: before={expires_before} after={}",
            lock.lease_expires_at
        );
    }

    #[test]
    fn handle_update_does_not_extend_lease_for_different_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let tasks_dir = tmp.path().join("tasks");
        let task_dir = tasks_dir.join("t1-test");
        make_todo_task(&task_dir, "t1");

        crate::storage::tasks::claim_task(&task_dir, "agent-1", "session-1", 1800, tmp.path())
            .unwrap();
        let (before, _) = read_task(&task_dir).unwrap().unwrap();
        let expires_before = before.lock.as_ref().unwrap().lease_expires_at.clone();

        handle_update(
            &tasks_dir,
            "t1",
            &serde_json::json!({ "notes": "someone else editing" }),
            false,
            Some("agent-2"),
            tmp.path(),
        )
        .unwrap();

        let (after, _) = read_task(&task_dir).unwrap().unwrap();
        let lock = after.lock.expect("lock should still be present");
        assert_eq!(lock.agent_id, "agent-1");
        assert_eq!(lock.lease_expires_at, expires_before);
    }

    #[test]
    fn handle_update_by_non_owning_agent_includes_advisory_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let tasks_dir = tmp.path().join("tasks");
        let task_dir = tasks_dir.join("t1-test");
        make_todo_task(&task_dir, "t1");

        crate::storage::tasks::claim_task(&task_dir, "agent-1", "session-1", 1800, tmp.path())
            .unwrap();

        let result = handle_update(
            &tasks_dir,
            "t1",
            &serde_json::json!({ "notes": "someone else editing" }),
            false,
            Some("agent-2"),
            tmp.path(),
        )
        .unwrap();

        assert!(
            result.contains("Advisory") && result.contains("t1") && result.contains("agent-1"),
            "expected advisory warning naming the claiming agent, got: {result}"
        );

        // The update must still be applied (advisory, not a rejection).
        let (after, _) = read_task(&task_dir).unwrap().unwrap();
        assert_eq!(after.notes.as_deref(), Some("someone else editing"));
    }

    #[test]
    fn handle_update_by_owning_agent_has_no_advisory_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let tasks_dir = tmp.path().join("tasks");
        let task_dir = tasks_dir.join("t1-test");
        make_todo_task(&task_dir, "t1");

        crate::storage::tasks::claim_task(&task_dir, "agent-1", "session-1", 1800, tmp.path())
            .unwrap();

        let result = handle_update(
            &tasks_dir,
            "t1",
            &serde_json::json!({ "notes": "still working" }),
            false,
            Some("agent-1"),
            tmp.path(),
        )
        .unwrap();

        assert!(
            !result.contains("Advisory"),
            "owner's own update should not carry an advisory warning: {result}"
        );
    }

    #[test]
    fn handle_update_on_unclaimed_task_has_no_advisory_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let tasks_dir = tmp.path().join("tasks");
        let task_dir = tasks_dir.join("t1-test");
        make_todo_task(&task_dir, "t1");

        let result = handle_update(
            &tasks_dir,
            "t1",
            &serde_json::json!({ "notes": "no lock here" }),
            false,
            Some("agent-2"),
            tmp.path(),
        )
        .unwrap();

        assert!(!result.contains("Advisory"), "got: {result}");
    }

    #[test]
    fn handle_update_to_done_clears_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let tasks_dir = tmp.path().join("tasks");
        let task_dir = tasks_dir.join("t1-test");
        make_todo_task(&task_dir, "t1");

        crate::storage::tasks::claim_task(&task_dir, "agent-1", "session-1", 1800, tmp.path())
            .unwrap();

        handle_update(
            &tasks_dir,
            "t1",
            &serde_json::json!({ "status": "done" }),
            false,
            Some("agent-1"),
            tmp.path(),
        )
        .unwrap();

        let (after, status) = read_task(&task_dir).unwrap().unwrap();
        assert!(after.lock.is_none());
        assert_eq!(status, "done");
    }

    #[test]
    fn handle_update_to_done_records_task_released_event() {
        let tmp = tempfile::tempdir().unwrap();
        let tasks_dir = tmp.path().join("tasks");
        let task_dir = tasks_dir.join("t1-test");
        make_todo_task(&task_dir, "t1");

        crate::storage::tasks::claim_task(&task_dir, "agent-1", "session-1", 1800, tmp.path())
            .unwrap();

        handle_update(
            &tasks_dir,
            "t1",
            &serde_json::json!({ "status": "done" }),
            false,
            Some("agent-1"),
            tmp.path(),
        )
        .unwrap();

        let events_path = tmp.path().join("events.jsonl");
        let content = std::fs::read_to_string(&events_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        // Line 0: task.claimed (from claim_task above). Line 1: task.released
        // (from this done transition).
        assert_eq!(lines.len(), 2);
        let released: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(released["event"], "task.released");
        assert_eq!(released["task_id"], "t1");
        assert_eq!(released["agent_id"], "agent-1");
    }

    /// `handle_update`'s read-modify-write cycle must be flock-protected: a
    /// concurrent `handoff_update_task` (notes change) and `claim_task`
    /// racing on the same task must not lose either write. Without flock,
    /// the two read-modify-write cycles can interleave and one write clobbers
    /// the other's on-disk state.
    #[test]
    fn concurrent_update_and_claim_do_not_lose_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let tasks_dir = tmp.path().join("tasks");
        let task_dir = tasks_dir.join("t1-test");
        make_todo_task(&task_dir, "t1");

        let tmp_path = tmp.path().to_path_buf();
        let tasks_dir_a = tasks_dir.clone();
        let tasks_dir_b = tasks_dir.clone();

        let handle_a = std::thread::spawn(move || {
            for _ in 0..25 {
                let _ = handle_update(
                    &tasks_dir_a,
                    "t1",
                    &serde_json::json!({ "notes": "concurrent notes update" }),
                    false,
                    Some("agent-updater"),
                    &tmp_path,
                );
            }
        });

        let tmp_path_b = tmp.path().to_path_buf();
        let handle_b = std::thread::spawn(move || {
            for _ in 0..25 {
                let task_dir = tasks_dir_b.join("t1-test");
                let _ = crate::storage::tasks::claim_task(
                    &task_dir,
                    "agent-1",
                    "session-1",
                    1800,
                    &tmp_path_b,
                );
                let _ =
                    crate::storage::tasks::release_task(&task_dir, "agent-1", "todo", &tmp_path_b);
            }
        });

        handle_a.join().unwrap();
        handle_b.join().unwrap();

        // The task file must still be readable and internally consistent
        // (proves no write was interrupted mid-flight and left corrupt JSON,
        // and no in-flight update was silently dropped).
        let (data, status) = read_task(&task_dir).unwrap().unwrap();
        assert_eq!(data.notes.as_deref(), Some("concurrent notes update"));
        assert!(status == "todo" || status == "in_progress");
    }
}

/// Guards the production call site (not just `apply_requirement_links`
/// itself): a `requirement_ids` diff that both adds and removes stable_ids
/// must reach the task file through exactly one read-modify-write
/// (t370.3 review round 2). Fails if `apply_requirement_ids_diff` is ever
/// reverted to separate add-only / remove-only passes.
#[cfg(test)]
mod requirement_ids_diff_tests {
    use super::*;
    use crate::storage::docs::{write_doc, DocMetadata, SubItem, Verification, VerificationItem};

    fn make_req_doc(
        handoff: &std::path::Path,
        doc_id: &str,
        stable_id: &str,
        task_ids: Vec<String>,
    ) {
        let now = Utc::now().to_rfc3339();
        let mut doc = DocMetadata::new(
            doc_id.to_string(),
            doc_id.to_string(),
            "Req".to_string(),
            "spec".to_string(),
            now.clone(),
        );
        doc.verification = Some(Verification {
            status: "pending".to_string(),
            created_at: now.clone(),
            updated_at: now,
            items: vec![VerificationItem {
                fragment_seq: Some(1),
                heading: "Section 1".to_string(),
                status: "pending".to_string(),
                impl_refs: Vec::new(),
                test_refs: Vec::new(),
                reviewer: None,
                verified_at: None,
                notes: String::new(),
                content_hash_at_verify: None,
                category: "section".to_string(),
                sub_items: vec![SubItem {
                    index: 0,
                    description: "req".to_string(),
                    stable_id: Some(stable_id.to_string()),
                    task_ids,
                    ..Default::default()
                }],
                label: None,
            }],
        });
        write_doc(handoff, &doc).unwrap();
    }

    #[test]
    fn swap_diff_writes_task_file_exactly_once() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(handoff.join("docs")).unwrap();
        make_req_doc(&handoff, "doc-a", "REQ-A", Vec::new());
        make_req_doc(&handoff, "doc-b", "REQ-B", vec!["t1".to_string()]);

        let task_dir = handoff.join("tasks").join("t1");
        std::fs::create_dir_all(&task_dir).unwrap();
        let existing = vec![TaskLink {
            target: "doc-b".to_string(),
            link_type: "requirement".to_string(),
            label: Some("REQ-B".to_string()),
        }];
        let data = TaskData {
            id: "t1".to_string(),
            title: "Test".to_string(),
            notes: None,
            priority: None,
            created_at: None,
            updated_at: None,
            completed_at: None,
            labels: Vec::new(),
            links: Vec::new(),
            task_links: existing.clone(),
            done_criteria: Vec::new(),
            schedule: None,
            dependencies: Vec::new(),
            order: None,
            assignee: None,
            lock: None,
            scope_paths: Vec::new(),
            extra: HashMap::new(),
        };
        write_task(&task_dir, "todo", &data).unwrap();
        let writes_before = crate::storage::tasks::task_file_write_count(&task_dir);

        let mut msg = String::new();
        apply_requirement_ids_diff(
            &handoff,
            "t1",
            &serde_json::json!({ "requirement_ids": ["REQ-A"] }),
            &existing,
            &mut msg,
        )
        .unwrap();

        assert_eq!(
            crate::storage::tasks::task_file_write_count(&task_dir) - writes_before,
            1,
            "add+remove diff must be applied in one task read-modify-write; msg={msg}"
        );
        let (after, _) = read_task(&task_dir).unwrap().unwrap();
        let labels: Vec<_> = after
            .task_links
            .iter()
            .filter(|l| l.link_type == "requirement")
            .filter_map(|l| l.label.as_deref())
            .collect();
        assert_eq!(labels, vec!["REQ-A"]);
    }
}

/// P-M7 (wiki/240-performance-design.md §3 C8, §4): `handle_update` must
/// release the task's flock *before* running `propagate_dev_stage_for_task`
/// (a separate, self-contained document read-modify-write that never
/// touches the task's own file), rather than holding the task's flock
/// across it. Round-2 review NIT: this invariant had no dedicated
/// deterministic concurrency test.
#[cfg(test)]
mod flock_released_before_propagate_tests {
    use super::*;
    use crate::storage::docs::{write_doc, DocMetadata, SubItem, Verification, VerificationItem};

    /// Writes `count` small requirement documents (`doc-0`..`doc-{count-1}`),
    /// each with one `SubItem`. Only `doc-0`'s `REQ-0` is actually linked to
    /// the test's task — the rest exist purely so `propagate_dev_stage_for_task`'s
    /// `DocSet::load` (a full `read_all_docs` pass) has enough real work to
    /// take measurably longer than the task's own (single small JSON file)
    /// read-modify-write. This is what makes the timing assertions below a
    /// genuine structural asymmetry rather than sleep-based luck.
    fn make_req_docs(handoff: &std::path::Path, count: usize) {
        for i in 0..count {
            let now = Utc::now().to_rfc3339();
            let doc_id = format!("doc-{i}");
            let mut doc = DocMetadata::new(
                doc_id.clone(),
                doc_id.clone(),
                "Req".to_string(),
                "spec".to_string(),
                now.clone(),
            );
            doc.verification = Some(Verification {
                status: "pending".to_string(),
                created_at: now.clone(),
                updated_at: now,
                items: vec![VerificationItem {
                    fragment_seq: None,
                    heading: "Req".to_string(),
                    status: "pending".to_string(),
                    impl_refs: Vec::new(),
                    test_refs: Vec::new(),
                    reviewer: None,
                    verified_at: None,
                    notes: String::new(),
                    content_hash_at_verify: None,
                    category: "requirement".to_string(),
                    sub_items: vec![SubItem {
                        index: 0,
                        description: "req".to_string(),
                        stable_id: Some(format!("REQ-{i}")),
                        task_ids: if i == 0 {
                            vec!["t1".to_string()]
                        } else {
                            Vec::new()
                        },
                        dev_stage: Some("not_started".to_string()),
                        ..Default::default()
                    }],
                    label: Some("reqs".to_string()),
                }],
            });
            write_doc(handoff, &doc).unwrap();
        }
    }

    fn read_req0_dev_stage(handoff: &std::path::Path) -> Option<String> {
        let doc = crate::storage::docs::read_doc(handoff, "doc-0")
            .unwrap()
            .unwrap();
        doc.verification.unwrap().items[0].sub_items[0]
            .dev_stage
            .clone()
    }

    /// Deterministic (no sleep-based timing luck for *ordering* — only a
    /// relative-margin comparison for the timing asymmetry, backed by real
    /// structural work, not a guessed delay):
    ///
    /// 1. Main pre-locks the task's flock itself, then spawns the worker
    ///    thread that calls `handle_update` — the worker blocks queued on
    ///    the flock as the *sole* contender (guaranteed, since the waiter
    ///    thread below doesn't exist yet).
    /// 2. Main releases its pre-lock. The worker (only contender) acquires
    ///    it and proceeds into its own read-modify-write.
    /// 3. Main confirms (bounded polling — proving *that* the worker holds
    ///    it, not catching any narrow window) that the flock is now held,
    ///    then spawns the waiter thread, which blocks on its own
    ///    `lock_exclusive()` call with no other contender — so its wake-up
    ///    is deterministically exactly the worker's `unlock()` call (P-M7:
    ///    unlock happens *before* `propagate_dev_stage_for_task`, not after).
    /// 4. If the flock were (incorrectly) held across propagate, the waiter
    ///    could not acquire it until the *entire* call (RMW + propagate)
    ///    finished, so `waiter_acquired_at` would sit right next to
    ///    `call_end`. Because `make_req_docs` gives propagate real,
    ///    proportional work (a `DocSet::load` over many documents) while the
    ///    task's own RMW touches a single small JSON file, the correct
    ///    (current) behavior instead leaves a comfortable, structural gap:
    ///    the waiter acquires well before the call as a whole finishes.
    #[test]
    fn propagate_dev_stage_runs_after_task_flock_is_released() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        let tasks_dir = handoff.join("tasks");
        std::fs::create_dir_all(&tasks_dir).unwrap();
        std::fs::create_dir_all(handoff.join("docs")).unwrap();

        make_req_docs(&handoff, 40);
        assert_eq!(
            read_req0_dev_stage(&handoff),
            Some("not_started".to_string())
        );

        let task_dir = tasks_dir.join("t1-test");
        std::fs::create_dir_all(&task_dir).unwrap();
        let data = TaskData {
            id: "t1".to_string(),
            title: "Test".to_string(),
            notes: None,
            priority: None,
            created_at: None,
            updated_at: None,
            completed_at: None,
            labels: Vec::new(),
            links: Vec::new(),
            task_links: vec![TaskLink {
                target: "doc-0".to_string(),
                link_type: "requirement".to_string(),
                label: Some("REQ-0".to_string()),
            }],
            done_criteria: Vec::new(),
            schedule: None,
            dependencies: Vec::new(),
            order: None,
            assignee: None,
            lock: None,
            scope_paths: Vec::new(),
            extra: HashMap::new(),
        };
        write_task(&task_dir, "todo", &data).unwrap();

        // Step 1: main pre-locks, guaranteeing the worker below is the sole
        // contender once it tries to acquire the same flock.
        let pre_lock = open_lock_file(&task_dir).unwrap();
        pre_lock.lock_exclusive().unwrap();

        let call_start = std::time::Instant::now();
        let worker_tasks_dir = tasks_dir.clone();
        let worker_handoff = handoff.clone();
        let worker = std::thread::spawn(move || {
            handle_update(
                &worker_tasks_dir,
                "t1",
                &serde_json::json!({ "status": "in_progress" }),
                false,
                None,
                &worker_handoff,
            )
        });

        // Step 2: release the pre-lock — the worker now acquires it.
        FileExt::unlock(&pre_lock).unwrap();

        // Step 3: bounded polling confirms *that* the worker holds the
        // flock (not a narrow-window race — the worker holds it for the
        // whole RMW, plenty of time for this to observe).
        let confirm_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let probe = open_lock_file(&task_dir).unwrap();
            if probe.try_lock_exclusive().is_err() {
                break;
            }
            FileExt::unlock(&probe).unwrap();
            assert!(
                std::time::Instant::now() < confirm_deadline,
                "worker never acquired the task flock after main released its pre-lock"
            );
            std::thread::yield_now();
        }

        let waiter_task_dir = task_dir.clone();
        let waiter = std::thread::spawn(move || {
            let lock_file = open_lock_file(&waiter_task_dir).unwrap();
            // Blocks until the worker's `handle_update` calls unlock() —
            // deterministic: no other contender exists at this point.
            lock_file.lock_exclusive().unwrap();
            let acquired_at = std::time::Instant::now();
            FileExt::unlock(&lock_file).unwrap();
            acquired_at
        });

        let waiter_acquired_at = waiter.join().unwrap();
        worker.join().unwrap().unwrap();
        let call_end = std::time::Instant::now();

        let total = call_end.duration_since(call_start);
        let until_waiter_acquired = waiter_acquired_at.duration_since(call_start);
        assert!(
            until_waiter_acquired < total,
            "waiter must acquire the flock before the overall call finishes: \
             until_waiter_acquired={until_waiter_acquired:?} total={total:?}"
        );
        // Relative margin (not an absolute ms threshold, to stay robust on a
        // slower/busier machine): the waiter must get in well before the
        // *whole* call (RMW + propagate over 40 documents) finishes. If the
        // flock were instead held across propagate, `until_waiter_acquired`
        // would sit right next to `total` (propagate dominates the call's
        // duration), not comfortably under it.
        assert!(
            until_waiter_acquired.as_secs_f64() < total.as_secs_f64() * 0.7,
            "flock does not appear to have been released before propagate ran: \
             until_waiter_acquired={until_waiter_acquired:?} total={total:?} \
             (expected the waiter to acquire the flock well before the full \
             RMW+propagate call finished)"
        );

        // And propagate did genuinely run (not skipped): the linked
        // SubItem's dev_stage reflects the "in_progress" status change.
        assert_eq!(
            read_req0_dev_stage(&handoff),
            Some("in_progress".to_string())
        );
    }
}
