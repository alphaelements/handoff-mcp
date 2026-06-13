use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<TaskIndex>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSummary {
    pub total: u32,
    pub by_status: std::collections::HashMap<String, u32>,
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
    std::fs::write(&file_path, content)
        .with_context(|| format!("Failed to write task: {}", file_path.display()))?;
    Ok(())
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

fn find_task_dir_recursive(dir: &Path, task_id: &str) -> Result<Option<PathBuf>> {
    if !dir.exists() {
        return Ok(None);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let entry_id = name.split('-').next().unwrap_or("");
        if entry_id == task_id {
            return Ok(Some(entry.path()));
        }
        if let Some(found) = find_task_dir_recursive(&entry.path(), task_id)? {
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
        by_status: std::collections::HashMap::new(),
    };
    let mut done_count: u32 = 0;

    build_index_recursive(
        tasks_dir,
        &mut tree,
        &mut summary,
        &mut done_count,
        done_task_limit,
    )?;

    Ok((tree, summary))
}

fn build_index_recursive(
    dir: &Path,
    tree: &mut Vec<TaskIndex>,
    summary: &mut TaskSummary,
    done_count: &mut u32,
    done_task_limit: u32,
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
        )?;

        tree.push(TaskIndex {
            id: data.id,
            title: data.title,
            status,
            children,
        });
    }

    Ok(())
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
