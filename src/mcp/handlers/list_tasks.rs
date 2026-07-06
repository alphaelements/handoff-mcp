use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::resolve_project_dir;
use crate::storage::config::read_config;
use crate::storage::ensure_handoff_exists;
use crate::storage::tasks::{build_task_index, TaskIndex, TaskSummary};

/// Maximum depth (relative to the base project dir) scanned for nested
/// `.handoff/` child projects.
const MAX_CHILD_SCAN_DEPTH: usize = 5;

/// Directory names skipped while scanning for child projects.
const DEFAULT_SCAN_EXCLUDES: &[&str] = &["node_modules", ".git", "target", "dist", ".next"];

pub fn handle(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;

    let handoff = ensure_handoff_exists(&project_dir)?;
    let tasks_dir = handoff.join("tasks");
    let config_path = handoff.join("config.toml");

    let done_task_limit = if config_path.exists() {
        read_config(&config_path)
            .map(|c| c.settings.done_task_limit)
            .unwrap_or(10)
    } else {
        10
    };

    let (tree, summary) = build_task_index(&tasks_dir, done_task_limit)?;

    let status_filter = arguments.get("status_filter").and_then(|v| v.as_str());
    let assignee_filter = arguments.get("assignee_filter").and_then(|v| v.as_str());
    let milestone_filter = arguments.get("milestone_filter").and_then(|v| v.as_str());
    let priority_filter = arguments.get("priority_filter").and_then(|v| v.as_str());
    let label_filter = arguments.get("label_filter").and_then(|v| v.as_str());

    let filters = Filters {
        status: status_filter,
        assignee: assignee_filter,
        milestone: milestone_filter,
        priority: priority_filter,
        label: label_filter,
    };

    let filtered_tree = if filters.any_active() {
        filter_tree(&tree, &filters)
    } else {
        tree
    };

    let include_children = arguments
        .get("include_children")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !include_children {
        let result = serde_json::json!({
            "task_tree": filtered_tree,
            "task_summary": summary,
        });
        return serde_json::to_string_pretty(&result).context("Failed to serialize task list");
    }

    // Determine own project name for annotation (defaults to config, fallback to "root").
    let own_project_name = if config_path.exists() {
        read_config(&config_path)
            .map(|c| c.project.name)
            .unwrap_or_else(|_| "root".to_string())
    } else {
        "root".to_string()
    };
    let own_project_dir = project_dir.to_string_lossy().to_string();

    let mut aggregated: Vec<Value> =
        annotate_tasks_json(filtered_tree, &own_project_name, &own_project_dir);
    let mut total_summary = summary;

    for (child_name, child_dir) in discover_child_projects(&project_dir, MAX_CHILD_SCAN_DEPTH) {
        let child_handoff = child_dir.join(".handoff");
        let child_tasks_dir = child_handoff.join("tasks");
        let child_config_path = child_handoff.join("config.toml");
        let child_done_task_limit = read_config(&child_config_path)
            .map(|c| c.settings.done_task_limit)
            .unwrap_or(10);

        let (child_tree, child_summary) =
            match build_task_index(&child_tasks_dir, child_done_task_limit) {
                Ok(v) => v,
                Err(_) => continue,
            };

        let child_filtered = if filters.any_active() {
            filter_tree(&child_tree, &filters)
        } else {
            child_tree
        };

        let child_dir_str = child_dir.to_string_lossy().to_string();
        aggregated.extend(annotate_tasks_json(
            child_filtered,
            &child_name,
            &child_dir_str,
        ));
        merge_summary(&mut total_summary, &child_summary);
    }

    let result = serde_json::json!({
        "task_tree": aggregated,
        "task_summary": total_summary,
    });

    serde_json::to_string_pretty(&result).context("Failed to serialize task list")
}

fn merge_summary(base: &mut TaskSummary, other: &TaskSummary) {
    base.total += other.total;
    for (status, count) in &other.by_status {
        *base.by_status.entry(status.clone()).or_insert(0) += count;
    }
    base.overdue_count += other.overdue_count;
    if let Some(other_est) = other.total_estimate_hours {
        base.total_estimate_hours = Some(base.total_estimate_hours.unwrap_or(0.0) + other_est);
    }
    if let Some(other_act) = other.total_actual_hours {
        base.total_actual_hours = Some(base.total_actual_hours.unwrap_or(0.0) + other_act);
    }
    if base.total > 0 {
        let done = *base.by_status.get("done").unwrap_or(&0) as f64;
        let skipped = *base.by_status.get("skipped").unwrap_or(&0) as f64;
        base.completion_rate = Some((done + skipped) / base.total as f64);
    }
}

/// Annotate a task tree with `project_name`/`project_dir` and a composite
/// `task_ref` disambiguator (`{project_name}-{project_dir_hash}:{id}`) for
/// cross-project display purposes.
///
/// The original `id` field is deliberately left **unmodified**: it is the
/// value accepted by `handoff_get_task`/`handoff_update_task`/etc. (scoped by
/// the sibling `project_dir` field), and it is also what `dependencies`
/// entries reference. Rewriting `id` would make it unusable with every other
/// tool and would desync it from `dependencies`, which still contain raw ids.
///
/// `project_name` alone is free-form user text with no cross-directory
/// uniqueness guarantee, so two child projects can share the same name. The
/// `task_ref` additionally embeds a short hash of `project_dir` (which is
/// unique per filesystem location) to guarantee it never collides even when
/// `project_name` is duplicated.
fn annotate_tasks_json(tasks: Vec<TaskIndex>, project_name: &str, project_dir: &str) -> Vec<Value> {
    let dir_hash = short_hash(project_dir);
    tasks
        .into_iter()
        .map(|t| {
            let mut v = serde_json::to_value(&t).unwrap_or_default();
            if let Some(obj) = v.as_object_mut() {
                obj.insert("project_name".to_string(), json!(project_name));
                obj.insert("project_dir".to_string(), json!(project_dir));
                if let Some(id) = obj.get("id").and_then(|v| v.as_str()).map(String::from) {
                    obj.insert(
                        "task_ref".to_string(),
                        json!(format!("{project_name}-{dir_hash}:{id}")),
                    );
                }
                if let Some(children) = obj.remove("children") {
                    if let Ok(child_tasks) = serde_json::from_value::<Vec<TaskIndex>>(children) {
                        obj.insert(
                            "children".to_string(),
                            json!(annotate_tasks_json(child_tasks, project_name, project_dir)),
                        );
                    }
                }
            }
            v
        })
        .collect()
}

/// Deterministic short hash (FNV-1a, hex-encoded) used to disambiguate
/// composite task IDs across child projects that share a `project_name`.
/// Not a dependency addition — plain FNV-1a is sufficient since this only
/// needs to be stable and collision-resistant for distinct directory paths,
/// not cryptographically secure.
fn short_hash(input: &str) -> String {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET_BASIS;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:08x}")
}

fn discover_child_projects(base: &Path, max_depth: usize) -> Vec<(String, PathBuf)> {
    let mut results = Vec::new();
    scan_children(base, 1, max_depth, &mut results);
    results
}

fn scan_children(dir: &Path, depth: usize, max_depth: usize, results: &mut Vec<(String, PathBuf)>) {
    if depth > max_depth {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') || DEFAULT_SCAN_EXCLUDES.contains(&name_str.as_ref()) {
            continue;
        }
        let path = entry.path();
        let config_path = path.join(".handoff").join("config.toml");
        if config_path.exists() {
            if let Ok(config) = read_config(&config_path) {
                results.push((config.project.name.clone(), path.clone()));
            }
        }
        scan_children(&path, depth + 1, max_depth, results);
    }
}

struct Filters<'a> {
    status: Option<&'a str>,
    assignee: Option<&'a str>,
    milestone: Option<&'a str>,
    priority: Option<&'a str>,
    label: Option<&'a str>,
}

impl Filters<'_> {
    fn any_active(&self) -> bool {
        self.status.is_some()
            || self.assignee.is_some()
            || self.milestone.is_some()
            || self.priority.is_some()
            || self.label.is_some()
    }

    fn matches(&self, node: &TaskIndex, data: Option<&crate::storage::tasks::TaskData>) -> bool {
        if let Some(status) = self.status {
            if node.status != status {
                return false;
            }
        }
        if let Some(assignee) = self.assignee {
            if node.assignee.as_deref() != Some(assignee) {
                return false;
            }
        }
        if let Some(milestone) = self.milestone {
            let ms = node.schedule.as_ref().and_then(|s| s.milestone.as_deref());
            if ms != Some(milestone) {
                return false;
            }
        }
        if let Some(priority) = self.priority {
            if let Some(d) = data {
                if d.priority.as_deref() != Some(priority) {
                    return false;
                }
            } else {
                return false;
            }
        }
        if let Some(label) = self.label {
            if let Some(d) = data {
                if !d.labels.iter().any(|l| l == label) {
                    return false;
                }
            } else {
                return false;
            }
        }
        true
    }
}

fn filter_tree(tree: &[TaskIndex], filters: &Filters) -> Vec<TaskIndex> {
    tree.iter()
        .filter_map(|node| {
            let children = filter_tree(&node.children, filters);
            if filters.matches(node, None) || !children.is_empty() {
                Some(TaskIndex {
                    id: node.id.clone(),
                    title: node.title.clone(),
                    status: node.status.clone(),
                    schedule: node.schedule.clone(),
                    dependencies: node.dependencies.clone(),
                    order: node.order,
                    assignee: node.assignee.clone(),
                    children,
                })
            } else {
                None
            }
        })
        .collect()
}
