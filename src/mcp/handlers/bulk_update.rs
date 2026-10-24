use anyhow::Result;
use chrono::Utc;
use serde_json::{json, Value};

use super::update_task::{
    done_guard_blocker_message, load_done_guard_block_check, log_direct_actual_hours_change,
    non_empty, resolve_done_guard,
};
use super::HandlerContext;
use crate::storage::config::read_config;
use crate::storage::tasks::*;

/// Keys of one `updates[]` entry that `apply_single_update` actually applies
/// (and the tool schema documents). Anything else would be silently dropped
/// while the call still reports `applied`, so it is rejected up front instead.
const SUPPORTED_UPDATE_FIELDS: &[&str] = &[
    "task_id",
    "status",
    "priority",
    "assignee",
    "notes",
    "notes_append",
    "schedule",
];

/// Keys of `updates[].schedule` that `apply_single_update` actually applies.
const SUPPORTED_SCHEDULE_FIELDS: &[&str] = &[
    "start_date",
    "due_date",
    "estimate_hours",
    "actual_hours",
    "remaining_hours",
    "milestone",
    "pinned",
    "baseline_start",
    "baseline_due",
];

/// Names (sorted) of the keys in `update` — and in its `schedule` object, as
/// `schedule.<key>` — that bulk update does not support.
fn unsupported_fields(update: &Value) -> Vec<String> {
    let unknown = |obj: &Value, supported: &[&str], prefix: &str| -> Vec<String> {
        obj.as_object()
            .into_iter()
            .flat_map(|m| m.keys())
            .filter(|k| !supported.contains(&k.as_str()))
            .map(|k| format!("{prefix}{k}"))
            .collect()
    };
    let mut fields = unknown(update, SUPPORTED_UPDATE_FIELDS, "");
    if let Some(schedule) = update.get("schedule") {
        fields.extend(unknown(schedule, SUPPORTED_SCHEDULE_FIELDS, "schedule."));
    }
    fields.sort();
    fields
}

pub fn handle(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let require_estimate_hours = read_config(&handoff.join("config.toml"))
        .map(|c| c.settings.require_estimate_hours)
        .unwrap_or(true);

    // `[trace] done_guard`, same mode and same `force` override as
    // `handoff_update_task`: a bulk patch must not be a way around the guard
    // (t-vfix.4).
    let done_guard = resolve_done_guard(handoff);
    let force = arguments
        .get("force")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let updates = arguments
        .get("updates")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("'updates' parameter is required (array)"))?;

    let mut applied: u32 = 0;
    let mut errors: Vec<Value> = Vec::new();
    let mut warnings: Vec<Value> = Vec::new();

    for update in updates {
        let task_id = match update.get("task_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => {
                errors.push(json!({"task_id": null, "error": "missing task_id"}));
                continue;
            }
        };

        // An update carrying fields this tool would silently ignore is
        // rejected whole (nothing applied, not counted in `applied`).
        let unsupported = unsupported_fields(update);
        if !unsupported.is_empty() {
            errors.push(json!({
                "task_id": task_id,
                "error": format!(
                    "Task {task_id}: unsupported fields in bulk update: [{}]. \
                     Use handoff_update_task for these fields.",
                    unsupported.join(", ")
                ),
            }));
            continue;
        }

        // A rejected update is reported per task and the rest still land: that is
        // this tool's established `applied` + `errors[]` contract.
        match apply_single_update(
            handoff,
            task_id,
            update,
            require_estimate_hours,
            ctx.agent_id.as_deref(),
            &done_guard,
            force,
        ) {
            Ok(warning) => {
                applied += 1;
                if let Some(warning) = warning {
                    warnings.push(json!({"task_id": task_id, "warning": warning}));
                }
            }
            Err(e) => errors.push(json!({"task_id": task_id, "error": e.to_string()})),
        }
    }

    let mut result = json!({
        "applied": applied,
        "errors": errors,
    });
    // Present only when a `warn`-mode guard fired, so the response shape for
    // every other call is unchanged.
    if !warnings.is_empty() {
        result["warnings"] = Value::Array(warnings);
    }

    serde_json::to_string_pretty(&result).map_err(Into::into)
}

fn apply_single_update(
    handoff_dir: &std::path::Path,
    task_id: &str,
    update: &Value,
    require_estimate_hours: bool,
    agent_id: Option<&str>,
    done_guard: &str,
    force: bool,
) -> Result<Option<String>> {
    let tasks_dir = &handoff_dir.join("tasks");
    let task_dir = find_task_dir_by_id(tasks_dir, task_id)?
        .ok_or_else(|| anyhow::anyhow!("{}", suggest_task_id(tasks_dir, task_id)))?;

    let (mut data, current_status) = read_task(&task_dir)?
        .ok_or_else(|| anyhow::anyhow!("Task file not found for {task_id}"))?;

    if let Some(priority) = update.get("priority").and_then(|v| v.as_str()) {
        validate_priority(Some(priority))?;
        data.priority = Some(priority.to_string());
    }

    if let Some(notes) = update.get("notes").and_then(|v| v.as_str()) {
        data.notes = Some(notes.to_string());
    } else if let Some(append) = update.get("notes_append").and_then(|v| v.as_str()) {
        let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%S");
        let block = format!("--- {timestamp}\n{append}");
        match &mut data.notes {
            Some(existing) if !existing.is_empty() => {
                existing.push_str(&format!("\n\n{block}"));
            }
            _ => data.notes = Some(block),
        }
    }

    if update.get("assignee").is_some() {
        data.assignee = update
            .get("assignee")
            .and_then(|v| v.as_str())
            .map(String::from);
    }

    // Stored `actual_hours` before this patch — a direct change is logged to
    // `time_log.jsonl` as a signed delta once the write commits.
    let actual_hours_before = data.schedule.as_ref().and_then(|s| s.actual_hours);

    if let Some(sched_val) = update.get("schedule") {
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
        // Baseline dates: "" resets to None (same contract as handoff_update_task).
        if let Some(b) = sched_val.get("baseline_start").and_then(|v| v.as_str()) {
            schedule.baseline_start = non_empty(b);
        }
        if let Some(b) = sched_val.get("baseline_due").and_then(|v| v.as_str()) {
            schedule.baseline_due = non_empty(b);
        }
    }

    let new_status = update
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

    // `[trace] done_guard` (wiki/260 §3.4): entering review/done with
    // outstanding requirement blockers is rejected in `block` mode (unless
    // `force`) and reported as a warning in `warn` mode, exactly like
    // `handoff_update_task`. Judged before any write so a rejection leaves
    // the task untouched. A bulk update carries no `requirement_ids`, so the
    // task's stored links are the whole link set.
    let mut guard_warning = None;
    let transitioning_to_guarded_status =
        (new_status == "review" || new_status == "done") && new_status != current_status;
    if transitioning_to_guarded_status && done_guard != "off" && !(done_guard == "block" && force) {
        if let Some((_, view)) =
            load_done_guard_block_check(handoff_dir, task_id, &data.task_links, None)?
        {
            if let Some(blocker_msg) = done_guard_blocker_message(task_id, &view.blockers) {
                if done_guard == "block" {
                    anyhow::bail!(
                        "{blocker_msg} [trace] done_guard = \"block\" rejects this status \
                         change; pass force: true to override, fix the blockers first, or \
                         change [trace] done_guard."
                    );
                }
                guard_warning = Some(blocker_msg);
            }
        }
    }

    // Guard the same invariant handoff_update_task enforces, or a bulk patch
    // becomes a way around it. The check is on the task as it would be written —
    // status and schedule already merged — not on the patch. Parent tasks (with
    // children) and tasks in todo/blocked/skipped are exempt.
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

    // t374/t375: write-then-remove, never remove-then-write — see
    // `write_task_transition`'s doc comment (src/storage/tasks.rs).
    write_task_transition(&task_dir, &current_status, new_status, &data)?;

    // FR-502: record the committed transition. Best-effort — the write has
    // landed, so a log failure must not turn this update into an error.
    let _ = crate::storage::events::append_status_changed(
        handoff_dir,
        task_id,
        &current_status,
        new_status,
        agent_id,
    );

    let log_warning = log_direct_actual_hours_change(
        handoff_dir,
        task_id,
        actual_hours_before,
        data.schedule.as_ref().and_then(|s| s.actual_hours),
        agent_id,
    );

    Ok(match (guard_warning, log_warning) {
        (Some(g), Some(l)) => Some(format!("{g}\n{l}")),
        (g, l) => g.or(l),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::handlers::HandlerContext;
    use std::collections::HashMap;

    fn ctx(handoff_dir: std::path::PathBuf) -> HandlerContext {
        HandlerContext {
            agent_id: None,
            project_dir: handoff_dir.parent().unwrap().to_path_buf(),
            handoff_dir,
        }
    }

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
            schedule: Some(Schedule {
                estimate_hours: Some(2.0),
                ..Default::default()
            }),
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
    fn bulk_update_rejects_unsupported_fields_without_applying_any_of_the_update() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        let task_dir = handoff_dir.join("tasks").join("t1-test");
        make_todo_task(&task_dir, "t1");
        make_todo_task(&handoff_dir.join("tasks").join("t2-test"), "t2");
        let c = ctx(handoff_dir);

        let out: Value = serde_json::from_str(
            &handle(
                &c,
                &json!({"updates": [
                    {"task_id": "t1", "priority": "high", "dependencies": ["t2"], "labels": ["x"]},
                    {"task_id": "t2", "priority": "low"}
                ]}),
            )
            .unwrap(),
        )
        .unwrap();

        assert_eq!(out["applied"], 1, "{out}");
        let errors = out["errors"].as_array().unwrap();
        assert_eq!(errors.len(), 1, "{out}");
        assert_eq!(errors[0]["task_id"], "t1");
        assert_eq!(
            errors[0]["error"],
            "Task t1: unsupported fields in bulk update: [dependencies, labels]. \
             Use handoff_update_task for these fields."
        );
        let (t1, _) = read_task(&task_dir).unwrap().unwrap();
        assert_eq!(
            t1.priority, None,
            "rejected update must not be partly applied"
        );
        assert!(t1.dependencies.is_empty());
    }

    #[test]
    fn bulk_update_rejects_unknown_schedule_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        make_todo_task(&handoff_dir.join("tasks").join("t1-test"), "t1");
        let c = ctx(handoff_dir);
        let out: Value = serde_json::from_str(
            &handle(
                &c,
                &json!({"updates": [{"task_id": "t1", "schedule": {"milestone": "m1", "estimte_hours": 3}}]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(out["applied"], 0, "{out}");
        assert!(out["errors"][0]["error"]
            .as_str()
            .unwrap()
            .contains("[schedule.estimte_hours]"));
    }

    #[test]
    fn bulk_update_accepts_every_supported_field() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        make_todo_task(&handoff_dir.join("tasks").join("t1-test"), "t1");
        let c = ctx(handoff_dir);
        let out: Value = serde_json::from_str(
            &handle(
                &c,
                &json!({"updates": [{
                    "task_id": "t1", "status": "in_progress", "priority": "low",
                    "assignee": "a", "notes": "n", "notes_append": "m",
                    "schedule": {
                        "start_date": "2026-01-01", "due_date": "2026-01-02",
                        "estimate_hours": 2.0, "actual_hours": 1.0, "remaining_hours": 1.0,
                        "milestone": "m", "pinned": true,
                        "baseline_start": "2026-01-01", "baseline_due": "2026-01-02"
                    }
                }]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(out["applied"], 1, "{out}");
        assert!(out["errors"].as_array().unwrap().is_empty(), "{out}");
    }

    fn read_log(handoff_dir: &std::path::Path) -> Vec<crate::storage::time_log::TimeLogEntry> {
        crate::storage::time_log::read_time_log(handoff_dir).unwrap()
    }

    #[test]
    fn bulk_update_of_actual_hours_appends_signed_direct_update_deltas() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        let tasks_dir = handoff_dir.join("tasks");
        make_todo_task(&tasks_dir.join("t1-test"), "t1");
        make_todo_task(&tasks_dir.join("t2-test"), "t2");
        let c = ctx(handoff_dir.clone());

        // Raise t1 from unset (0) to 3, then lower it to 1; set t2 to 2 once.
        for (id, hours) in [("t1", 3.0), ("t1", 1.0), ("t2", 2.0), ("t2", 2.0)] {
            let out = handle(
                &c,
                &json!({"updates": [{"task_id": id, "schedule": {"actual_hours": hours}}]}),
            )
            .unwrap();
            assert!(out.contains("\"applied\": 1"), "{out}");
        }

        let log = read_log(&handoff_dir);
        let t1: Vec<f64> = log
            .iter()
            .filter(|e| e.task_id == "t1")
            .map(|e| e.hours)
            .collect();
        let t2: Vec<f64> = log
            .iter()
            .filter(|e| e.task_id == "t2")
            .map(|e| e.hours)
            .collect();
        assert_eq!(t1, vec![3.0, -2.0]);
        assert_eq!(t2, vec![2.0], "an unchanged value must not log again");
        assert!(log
            .iter()
            .all(|e| e.source.as_deref() == Some(crate::storage::time_log::SOURCE_DIRECT_UPDATE)));
    }

    #[test]
    fn bulk_update_without_actual_hours_logs_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        make_todo_task(&handoff_dir.join("tasks").join("t1-test"), "t1");
        let c = ctx(handoff_dir.clone());
        handle(
            &c,
            &json!({"updates": [{"task_id": "t1", "schedule": {"milestone": "m1"}}]}),
        )
        .unwrap();
        assert!(read_log(&handoff_dir).is_empty());
    }

    fn read_schedule(task_dir: &std::path::Path) -> Schedule {
        read_task(task_dir)
            .unwrap()
            .unwrap()
            .0
            .schedule
            .expect("schedule")
    }

    /// Baseline dates follow `handoff_update_task`'s contract: a value sets
    /// them, an omitted key leaves them alone, `""` resets them to unset (so
    /// the next `auto_schedule` run records a fresh baseline). Unlike
    /// `auto_schedule`, a bulk patch overwrites an existing baseline.
    #[test]
    fn bulk_update_sets_keeps_overwrites_and_resets_baseline_dates() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        let task_dir = handoff_dir.join("tasks").join("t1-test");
        make_todo_task(&task_dir, "t1");
        let c = ctx(handoff_dir);
        let patch = |schedule: Value| {
            let out = handle(
                &c,
                &json!({"updates": [{"task_id": "t1", "schedule": schedule}]}),
            )
            .unwrap();
            assert!(out.contains("\"applied\": 1"), "{out}");
        };

        patch(json!({"baseline_start": "2026-03-02", "baseline_due": "2026-03-06"}));
        let s = read_schedule(&task_dir);
        assert_eq!(s.baseline_start.as_deref(), Some("2026-03-02"));
        assert_eq!(s.baseline_due.as_deref(), Some("2026-03-06"));

        // Unrelated schedule keys leave the baseline untouched.
        patch(json!({"milestone": "m1"}));
        let s = read_schedule(&task_dir);
        assert_eq!(s.baseline_start.as_deref(), Some("2026-03-02"));
        assert_eq!(s.baseline_due.as_deref(), Some("2026-03-06"));

        // Each baseline field is independent; a new value overwrites.
        patch(json!({"baseline_due": "2026-03-09"}));
        let s = read_schedule(&task_dir);
        assert_eq!(s.baseline_start.as_deref(), Some("2026-03-02"));
        assert_eq!(s.baseline_due.as_deref(), Some("2026-03-09"));

        // "" resets only the named field.
        patch(json!({"baseline_start": ""}));
        let s = read_schedule(&task_dir);
        assert_eq!(s.baseline_start, None);
        assert_eq!(s.baseline_due.as_deref(), Some("2026-03-09"));
    }

    /// `apply_single_update` renames the task file (`todo` <-> `in_progress`,
    /// changing `_task.<status>.json`'s name) on every call here. Before
    /// t375's fix it did `find_task_file` -> `remove_file` -> `write_task`,
    /// briefly leaving zero task files on disk; a concurrent unlocked reader
    /// (`read_task`) landing in that window would see `None` and report a
    /// spurious "Task not found". `write_task_transition` closes that window
    /// by writing the new-named file before removing the stale one.
    #[test]
    fn concurrent_bulk_update_status_transitions_and_read_do_not_report_task_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        let tasks_dir = handoff_dir.join("tasks");
        make_todo_task(&tasks_dir.join("t1-test"), "t1");
        let task_dir = tasks_dir.join("t1-test");

        let c = ctx(handoff_dir);
        let writer = std::thread::spawn(move || {
            for i in 0..50 {
                let status = if i % 2 == 0 { "in_progress" } else { "todo" };
                handle(
                    &c,
                    &json!({
                        "updates": [
                            { "task_id": "t1", "status": status }
                        ]
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
