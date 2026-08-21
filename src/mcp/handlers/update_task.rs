use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::Utc;
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
        lock: None,
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
        lock: None,
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
    agent_id: Option<&str>,
    handoff_dir: &std::path::Path,
) -> Result<String> {
    let task_dir = find_task_dir_by_id(tasks_dir, task_id)?
        .ok_or_else(|| anyhow::anyhow!("{}", suggest_task_id(tasks_dir, task_id)))?;

    let (mut data, current_status) = read_task(&task_dir)?
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
                    agent_id: Some(lock.agent_id),
                    session_id: Some(lock.session_id),
                    detail: Some("revert_status=done".to_string()),
                },
            );
        }
    }

    if new_status == "skipped" && current_status != "skipped" {
        validate_skipped_transition(&task_dir, &data)?;
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

    let mut msg = format!("Updated task {task_id}: {} [{new_status}]", data.title);
    if let Some(warning) = advisory_warning {
        msg.push_str(&format!("\n{warning}"));
    }
    Ok(msg)
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
}
