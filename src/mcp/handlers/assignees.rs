use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use toml_edit::{DocumentMut, Item, Table};

use super::config_crud::{
    load_doc, require_str, save_doc, set_opt_f64, set_opt_str, set_string_array,
};
use super::resolve_project_dir;
use crate::storage::ensure_handoff_exists;
use crate::storage::tasks::{build_task_index, TaskIndex};

pub fn handle(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;
    let config_path = handoff.join("config.toml");
    let tasks_dir = handoff.join("tasks");

    let raw = std::fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read config: {}", config_path.display()))?;
    let doc: DocumentMut = raw.parse().with_context(|| "Failed to parse config.toml")?;

    let assignees_table = doc.get("assignees").and_then(|v| v.as_table());

    let mut result: HashMap<String, Value> = HashMap::new();

    if let Some(table) = assignees_table {
        for (key, item) in table.iter() {
            let sub = match item.as_table() {
                Some(t) => t,
                None => continue,
            };

            let display_name = sub
                .get("display_name")
                .and_then(|v| v.as_str())
                .unwrap_or(key);
            let color = sub.get("color").and_then(|v| v.as_str()).unwrap_or("");
            let work_hours = sub
                .get("work_hours_per_day")
                .and_then(|v| v.as_integer().or_else(|| v.as_float().map(|f| f as i64)))
                .unwrap_or(8);
            let closed_weekdays: Vec<Value> = sub
                .get("closed_weekdays")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| {
                            v.as_str()
                                .map(|s| json!(s))
                                .or_else(|| v.as_integer().map(|n| json!(n)))
                        })
                        .collect()
                })
                .unwrap_or_default();

            result.insert(
                key.to_string(),
                json!({
                    "display_name": display_name,
                    "color": color,
                    "work_hours_per_day": work_hours,
                    "closed_weekdays": closed_weekdays,
                    "task_count": 0,
                    "active_task_count": 0,
                    "total_estimate_hours": 0.0,
                    "total_actual_hours": 0.0,
                }),
            );
        }
    }

    // Count tasks per assignee
    if tasks_dir.exists() {
        let (tree, _) = build_task_index(&tasks_dir, u32::MAX)?;
        count_assignee_tasks(&tree, &mut result);
    }

    let output = json!({ "assignees": result });
    serde_json::to_string_pretty(&output).map_err(Into::into)
}

fn count_assignee_tasks(tree: &[TaskIndex], result: &mut HashMap<String, Value>) {
    for node in tree {
        if let Some(ref assignee) = node.assignee {
            if let Some(entry) = result.get_mut(assignee) {
                if let Some(obj) = entry.as_object_mut() {
                    let tc = obj.get("task_count").and_then(|v| v.as_u64()).unwrap_or(0);
                    obj.insert("task_count".to_string(), json!(tc + 1));

                    if node.status == "in_progress" || node.status == "review" {
                        let ac = obj
                            .get("active_task_count")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        obj.insert("active_task_count".to_string(), json!(ac + 1));
                    }

                    if let Some(ref sched) = node.schedule {
                        if let Some(est) = sched.estimate_hours {
                            let cur = obj
                                .get("total_estimate_hours")
                                .and_then(|v| v.as_f64())
                                .unwrap_or(0.0);
                            obj.insert("total_estimate_hours".to_string(), json!(cur + est));
                        }
                        if let Some(act) = sched.actual_hours {
                            let cur = obj
                                .get("total_actual_hours")
                                .and_then(|v| v.as_f64())
                                .unwrap_or(0.0);
                            obj.insert("total_actual_hours".to_string(), json!(cur + act));
                        }
                    }
                }
            }
        }

        count_assignee_tasks(&node.children, result);
    }
}

/// handoff_add_assignee — create a new `[assignees.<key>]` entry. Fails if the
/// key already exists.
pub fn handle_add(arguments: &Value) -> Result<String> {
    let path = super::config_crud::config_path(arguments)?;
    let key = require_str(arguments, "key")?;
    let mut doc = load_doc(&path)?;

    let assignees = super::config_crud::ensure_table(&mut doc, "assignees")?;
    assignees.set_implicit(true);
    if assignees.contains_key(key) {
        anyhow::bail!("Assignee '{key}' already exists. Use handoff_update_assignee to modify it.");
    }
    let mut sub = Table::new();
    apply_assignee_fields(&mut sub, arguments);
    doc["assignees"][key] = Item::Table(sub);

    save_doc(&path, &doc)?;
    Ok(format!("Added assignee '{key}'"))
}

/// handoff_update_assignee — patch an existing `[assignees.<key>]` entry.
pub fn handle_update(arguments: &Value) -> Result<String> {
    let path = super::config_crud::config_path(arguments)?;
    let key = require_str(arguments, "key")?;
    let mut doc = load_doc(&path)?;

    let exists = doc
        .get("assignees")
        .and_then(|v| v.as_table())
        .map(|t| t.contains_key(key))
        .unwrap_or(false);
    if !exists {
        anyhow::bail!("Assignee '{key}' not found. Use handoff_add_assignee to create it.");
    }
    let sub = super::config_crud::ensure_subtable(&mut doc, "assignees", key)?;
    apply_assignee_fields(sub, arguments);

    save_doc(&path, &doc)?;
    Ok(format!("Updated assignee '{key}'"))
}

/// handoff_remove_assignee — delete a `[assignees.<key>]` entry and unassign it
/// from every task (matches the VSCode extension's removeAssignee behaviour).
pub fn handle_remove(arguments: &Value) -> Result<String> {
    let path = super::config_crud::config_path(arguments)?;
    let key = require_str(arguments, "key")?;
    let mut doc = load_doc(&path)?;

    let removed = doc
        .get_mut("assignees")
        .and_then(|v| v.as_table_mut())
        .map(|t| t.remove(key).is_some())
        .unwrap_or(false);
    if !removed {
        anyhow::bail!("Assignee '{key}' not found.");
    }
    save_doc(&path, &doc)?;

    // Unassign from tasks.
    let tasks_dir = path
        .parent()
        .map(|p| p.join("tasks"))
        .ok_or_else(|| anyhow::anyhow!("Cannot locate tasks dir"))?;
    let unassigned = unassign_all(&tasks_dir, key)?;

    Ok(format!(
        "Removed assignee '{key}' and unassigned it from {unassigned} task(s)"
    ))
}

/// Apply the optional assignee fields from `arguments` onto a TOML table.
fn apply_assignee_fields(table: &mut Table, arguments: &Value) {
    table.set_implicit(false);
    set_opt_str(table, "display_name", arguments.get("display_name"));
    set_opt_str(table, "color", arguments.get("color"));
    set_opt_f64(
        table,
        "work_hours_per_day",
        arguments.get("work_hours_per_day"),
    );
    super::config_crud::set_mixed_array(table, "closed_weekdays", arguments.get("closed_weekdays"));
    set_string_array(table, "closed_dates", arguments.get("closed_dates"));
    set_string_array(table, "open_dates", arguments.get("open_dates"));
    super::config_crud::set_f64_map(table, "day_hours", arguments.get("day_hours"));
}

/// Clear `assignee` on every task currently assigned to `key`. Returns the count.
fn unassign_all(tasks_dir: &Path, key: &str) -> Result<usize> {
    use crate::storage::tasks::{read_task, write_task};
    use chrono::Utc;

    let mut count = 0;
    let dirs = collect_task_dirs(tasks_dir)?;
    for dir in dirs {
        if let Some((mut data, status)) = read_task(&dir)? {
            if data.assignee.as_deref() == Some(key) {
                data.assignee = None;
                data.updated_at = Some(Utc::now().to_rfc3339());
                write_task(&dir, &status, &data)?;
                count += 1;
            }
        }
    }
    Ok(count)
}

/// Recursively collect every task directory under `tasks_dir`.
fn collect_task_dirs(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    collect_task_dirs_into(dir, &mut out)?;
    Ok(out)
}

fn collect_task_dirs_into(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            // A task directory contains a `_task.<status>.json` file.
            if crate::storage::tasks::find_task_file(&path)?.is_some() {
                out.push(path.clone());
            }
            collect_task_dirs_into(&path, out)?;
        }
    }
    Ok(())
}
