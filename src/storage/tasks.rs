use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

fn is_empty_map(m: &HashMap<String, Value>) -> bool {
    m.is_empty()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskData {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub done_criteria: Vec<DoneCriterion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "is_empty_map")]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Schedule {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate_hours: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_hours: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_hours: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milestone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoneCriterion {
    pub item: String,
    #[serde(default)]
    pub checked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskIndex {
    pub id: String,
    pub title: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<TaskIndex>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSummary {
    pub total: u32,
    pub by_status: std::collections::HashMap<String, u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_estimate_hours: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_actual_hours: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_rate: Option<f64>,
    #[serde(default)]
    pub overdue_count: u32,
}

const VALID_STATUSES: &[&str] = &[
    "todo",
    "in_progress",
    "review",
    "done",
    "blocked",
    "skipped",
];

const VALID_PRIORITIES: &[&str] = &["low", "medium", "high"];

pub fn is_valid_status(status: &str) -> bool {
    VALID_STATUSES.contains(&status)
}

pub fn is_valid_priority(priority: &str) -> bool {
    VALID_PRIORITIES.contains(&priority)
}

pub fn validate_priority(priority: Option<&str>) -> Result<()> {
    if let Some(p) = priority {
        if !is_valid_priority(p) {
            anyhow::bail!(
                "Invalid priority: '{p}'. Must be one of: {}",
                VALID_PRIORITIES.join(", ")
            );
        }
    }
    Ok(())
}

pub fn is_terminal_status(status: &str) -> bool {
    status == "done" || status == "skipped"
}

pub fn title_to_slug(title: &str) -> String {
    let slug: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else if !c.is_ascii() {
                c
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-').to_string();
    let mut result = String::new();
    let mut prev_dash = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_dash {
                result.push(c);
            }
            prev_dash = true;
        } else {
            result.push(c);
            prev_dash = false;
        }
    }
    if result.is_empty() {
        "task".to_string()
    } else {
        result
    }
}

pub fn find_task_file(task_dir: &Path) -> Result<Option<(PathBuf, String)>> {
    for entry in std::fs::read_dir(task_dir)
        .with_context(|| format!("Failed to read task dir: {}", task_dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(status) = parse_task_filename(&name) {
            return Ok(Some((entry.path(), status)));
        }
    }
    Ok(None)
}

fn parse_task_filename(name: &str) -> Option<String> {
    let name = name.strip_prefix("_task.")?;
    let status = name.strip_suffix(".json")?;
    if is_valid_status(status) {
        Some(status.to_string())
    } else {
        None
    }
}

pub fn read_task(task_dir: &Path) -> Result<Option<(TaskData, String)>> {
    let (file_path, status) = match find_task_file(task_dir)? {
        Some(v) => v,
        None => return Ok(None),
    };
    let content = std::fs::read_to_string(&file_path)
        .with_context(|| format!("Failed to read task: {}", file_path.display()))?;
    let data: TaskData = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse task: {}", file_path.display()))?;
    Ok(Some((data, status)))
}

pub fn write_task(task_dir: &Path, status: &str, data: &TaskData) -> Result<()> {
    let file_path = task_dir.join(format!("_task.{status}.json"));
    let content = serde_json::to_string_pretty(data).context("Failed to serialize task")?;
    crate::storage::atomic_write(&file_path, content.as_bytes())
        .with_context(|| format!("Failed to write task: {}", file_path.display()))?;
    Ok(())
}

/// Read-modify-write a task with optimistic concurrency control.
///
/// Reads the current task, runs `mutate` on a copy, then re-reads just before
/// writing: if the file's `updated_at` changed since the snapshot (another
/// writer — e.g. the VSCode extension — won the race), the whole cycle retries
/// up to `MAX_RETRIES` times. This matches the VSCode side's `updated_at`
/// protocol (wiki/95-concurrency-safety.md) and prevents lost updates that
/// atomic_write alone cannot (atomic_write stops *torn* reads, not *lost*
/// updates).
///
/// `mutate` receives the current `TaskData` and the resolved status, and returns
/// the new status the task should have after the change (usually unchanged).
pub fn read_modify_write_task<F>(task_dir: &Path, mut mutate: F) -> Result<()>
where
    F: FnMut(&mut TaskData, &str) -> Result<String>,
{
    const MAX_RETRIES: usize = 5;

    for attempt in 0..=MAX_RETRIES {
        let (mut data, status) = read_task(task_dir)?
            .ok_or_else(|| anyhow::anyhow!("Task file not found in {}", task_dir.display()))?;
        let snapshot_updated_at = data.updated_at.clone();

        let new_status = mutate(&mut data, &status)?;

        // Re-read to detect a concurrent writer before committing.
        let current_updated_at = read_task(task_dir)?.and_then(|(d, _)| d.updated_at);
        if current_updated_at != snapshot_updated_at {
            // Someone else wrote between our read and write. Retry from scratch.
            if attempt == MAX_RETRIES {
                anyhow::bail!(
                    "Concurrent modification of task in {} after {} retries; aborting to avoid \
                     overwriting another writer's changes.",
                    task_dir.display(),
                    MAX_RETRIES
                );
            }
            continue;
        }

        if new_status != status {
            change_status(task_dir, &new_status)?;
        }
        write_task(task_dir, &new_status, &data)?;
        return Ok(());
    }
    unreachable!("loop returns or bails within MAX_RETRIES iterations")
}

pub fn change_status(task_dir: &Path, new_status: &str) -> Result<()> {
    if !is_valid_status(new_status) {
        anyhow::bail!("Invalid status: {new_status}");
    }

    let (old_path, old_status) = find_task_file(task_dir)?
        .ok_or_else(|| anyhow::anyhow!("No task file found in {}", task_dir.display()))?;

    if old_status == new_status {
        return Ok(());
    }

    let new_path = task_dir.join(format!("_task.{new_status}.json"));
    std::fs::rename(&old_path, &new_path).with_context(|| {
        format!(
            "Failed to rename {} -> {}",
            old_path.display(),
            new_path.display()
        )
    })?;

    Ok(())
}

pub fn next_child_id(parent_dir: &Path, parent_id: &str) -> Result<String> {
    let mut max_n: u32 = 0;

    if parent_dir.exists() {
        for entry in std::fs::read_dir(parent_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(n) = extract_child_number(&name, parent_id) {
                max_n = max_n.max(n);
            }
        }
    }

    Ok(format!("{parent_id}.{}", max_n + 1))
}

pub fn next_top_level_id(tasks_dir: &Path) -> Result<String> {
    let mut max_n: u32 = 0;

    if tasks_dir.exists() {
        for entry in std::fs::read_dir(tasks_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(n) = extract_top_level_number(&name) {
                max_n = max_n.max(n);
            }
        }
    }

    Ok(format!("t{}", max_n + 1))
}

fn extract_child_number(dir_name: &str, parent_id: &str) -> Option<u32> {
    let prefix = format!("{parent_id}.");
    let rest = dir_name.strip_prefix(&prefix)?;
    let num_part = rest.split('-').next()?;
    num_part.parse().ok()
}

fn extract_top_level_number(dir_name: &str) -> Option<u32> {
    let rest = dir_name.strip_prefix('t')?;
    let num_part = rest.split('-').next()?;
    if num_part.contains('.') {
        return None;
    }
    num_part.parse().ok()
}

pub fn find_task_dir_by_id(tasks_dir: &Path, task_id: &str) -> Result<Option<PathBuf>> {
    find_task_dir_recursive(tasks_dir, task_id)
}

fn dir_name_could_match(dir_name: &str, task_id: &str) -> bool {
    dir_name == task_id
        || (dir_name.starts_with(task_id) && dir_name.as_bytes().get(task_id.len()) == Some(&b'-'))
}

fn find_task_dir_recursive(dir: &Path, task_id: &str) -> Result<Option<PathBuf>> {
    if !dir.exists() {
        return Ok(None);
    }
    let mut candidates = Vec::new();
    let mut other_subdirs = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if dir_name_could_match(&name, task_id) {
            candidates.push(entry.path());
        } else {
            other_subdirs.push(entry.path());
        }
    }
    // Verify candidates by reading the JSON id field.
    for candidate in &candidates {
        if let Some((data, _)) = read_task(candidate)? {
            if data.id == task_id {
                return Ok(Some(candidate.clone()));
            }
        }
    }
    // Recurse into all subdirectories (candidates that didn't match + others).
    for subdir in candidates.into_iter().chain(other_subdirs) {
        if let Some(found) = find_task_dir_recursive(&subdir, task_id)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

pub fn build_task_index(
    tasks_dir: &Path,
    done_task_limit: u32,
) -> Result<(Vec<TaskIndex>, TaskSummary)> {
    let mut tree = Vec::new();
    let mut summary = TaskSummary {
        total: 0,
        by_status: HashMap::new(),
        total_estimate_hours: None,
        total_actual_hours: None,
        completion_rate: None,
        overdue_count: 0,
    };
    let mut done_count: u32 = 0;
    let mut estimate_sum: f64 = 0.0;
    let mut actual_sum: f64 = 0.0;
    let mut has_hours = false;
    let today = Utc::now().format("%Y-%m-%d").to_string();

    build_index_recursive(
        tasks_dir,
        &mut tree,
        &mut summary,
        &mut done_count,
        done_task_limit,
        &mut estimate_sum,
        &mut actual_sum,
        &mut has_hours,
        &today,
    )?;

    if has_hours {
        summary.total_estimate_hours = Some(estimate_sum);
        summary.total_actual_hours = Some(actual_sum);
    }

    if summary.total > 0 {
        let done = *summary.by_status.get("done").unwrap_or(&0) as f64;
        let skipped = *summary.by_status.get("skipped").unwrap_or(&0) as f64;
        summary.completion_rate = Some((done + skipped) / summary.total as f64);
    }

    Ok((tree, summary))
}

#[allow(clippy::too_many_arguments)]
fn build_index_recursive(
    dir: &Path,
    tree: &mut Vec<TaskIndex>,
    summary: &mut TaskSummary,
    done_count: &mut u32,
    done_task_limit: u32,
    estimate_sum: &mut f64,
    actual_sum: &mut f64,
    has_hours: &mut bool,
    today: &str,
) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }

    let mut entries: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }

        let task_dir = entry.path();
        let (data, status) = match read_task(&task_dir)? {
            Some(v) => v,
            None => continue,
        };

        summary.total += 1;
        *summary.by_status.entry(status.clone()).or_insert(0) += 1;

        if let Some(ref sched) = data.schedule {
            if let Some(est) = sched.estimate_hours {
                *estimate_sum += est;
                *has_hours = true;
            }
            if let Some(act) = sched.actual_hours {
                *actual_sum += act;
                *has_hours = true;
            }
            if let Some(ref due) = sched.due_date {
                if !is_terminal_status(&status) && due.as_str() < today {
                    summary.overdue_count += 1;
                }
            }
        }

        if is_terminal_status(&status) {
            *done_count += 1;
            if *done_count > done_task_limit {
                continue;
            }
        }

        let mut children = Vec::new();
        build_index_recursive(
            &task_dir,
            &mut children,
            summary,
            done_count,
            done_task_limit,
            estimate_sum,
            actual_sum,
            has_hours,
            today,
        )?;

        tree.push(TaskIndex {
            id: data.id,
            title: data.title,
            status,
            schedule: data.schedule,
            dependencies: data.dependencies,
            order: data.order,
            assignee: data.assignee,
            children,
        });
    }

    Ok(())
}

pub fn validate_dependencies(tasks_dir: &Path, task_id: &str, new_deps: &[String]) -> Result<()> {
    let dep_graph = build_dependency_graph(tasks_dir)?;

    let mut graph = dep_graph;
    graph.insert(task_id.to_string(), new_deps.to_vec());

    let mut visited = HashSet::new();
    let mut stack = HashSet::new();

    if has_cycle(&graph, task_id, &mut visited, &mut stack) {
        anyhow::bail!(
            "Circular dependency detected: setting dependencies {:?} on task {task_id} would create a cycle",
            new_deps
        );
    }

    Ok(())
}

fn build_dependency_graph(tasks_dir: &Path) -> Result<HashMap<String, Vec<String>>> {
    let mut graph = HashMap::new();
    build_dep_graph_recursive(tasks_dir, &mut graph)?;
    Ok(graph)
}

fn build_dep_graph_recursive(dir: &Path, graph: &mut HashMap<String, Vec<String>>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name.starts_with('_') {
            continue;
        }
        let task_dir = entry.path();
        if let Some((data, _)) = read_task(&task_dir)? {
            graph.insert(data.id.clone(), data.dependencies.clone());
            build_dep_graph_recursive(&task_dir, graph)?;
        }
    }
    Ok(())
}

fn has_cycle(
    graph: &HashMap<String, Vec<String>>,
    node: &str,
    visited: &mut HashSet<String>,
    stack: &mut HashSet<String>,
) -> bool {
    if stack.contains(node) {
        return true;
    }
    if visited.contains(node) {
        return false;
    }
    visited.insert(node.to_string());
    stack.insert(node.to_string());

    if let Some(deps) = graph.get(node) {
        for dep in deps {
            if has_cycle(graph, dep, visited, stack) {
                return true;
            }
        }
    }

    stack.remove(node);
    false
}

pub fn validate_done_transition(task_dir: &Path, data: &TaskData) -> Result<()> {
    for criterion in &data.done_criteria {
        if !criterion.checked {
            anyhow::bail!(
                "Cannot mark task {} as done: done_criteria item '{}' is not checked",
                data.id,
                criterion.item
            );
        }
    }

    check_children_terminal(task_dir, &data.id)?;

    Ok(())
}

pub fn validate_skipped_transition(task_dir: &Path, data: &TaskData) -> Result<()> {
    check_children_terminal(task_dir, &data.id)?;
    Ok(())
}

/// Returns true if the task directory contains at least one child task
/// (a non-`_`/`.`-prefixed subdirectory holding a task file).
pub fn task_has_children(task_dir: &Path) -> Result<bool> {
    if !task_dir.exists() {
        return Ok(false);
    }
    for entry in std::fs::read_dir(task_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('_') || name.starts_with('.') {
            continue;
        }
        if find_task_file(&entry.path())?.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether a task in the given status requires an effort estimate.
/// Parent tasks (with children) and blocked/skipped tasks are exempt;
/// this only covers the status dimension.
pub fn status_requires_estimate(status: &str) -> bool {
    matches!(status, "todo" | "in_progress" | "review" | "done")
}

/// Validate that a leaf task carries an `estimate_hours` when the project
/// requires it. `has_children` lets the caller skip parent tasks.
/// Returns an error guiding the caller to add an estimate.
pub fn validate_estimate_required(
    require_estimate_hours: bool,
    status: &str,
    has_children: bool,
    schedule: Option<&Schedule>,
) -> Result<()> {
    if !require_estimate_hours || has_children || !status_requires_estimate(status) {
        return Ok(());
    }
    let has_estimate = schedule
        .and_then(|s| s.estimate_hours)
        .is_some_and(|h| h > 0.0);
    if !has_estimate {
        anyhow::bail!(
            "Task requires an effort estimate: set schedule.estimate_hours (hours, > 0) \
             when creating or updating a leaf task in status '{status}'. \
             Estimate the human-effort hours for this task. \
             To disable this requirement project-wide, set settings.require_estimate_hours = false."
        );
    }
    Ok(())
}

fn check_children_terminal(task_dir: &Path, parent_id: &str) -> Result<()> {
    if !task_dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(task_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('_') || name.starts_with('.') {
            continue;
        }
        if let Some((_, status)) = find_task_file(&entry.path())? {
            if !is_terminal_status(&status) {
                anyhow::bail!(
                    "Cannot mark task {parent_id} as done/skipped: child task in directory '{}' has status '{status}'",
                    name
                );
            }
        }
    }
    Ok(())
}
