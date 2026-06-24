use anyhow::{Context, Result};
use serde_json::Value;

use super::resolve_project_dir;
use crate::storage::config::read_config;
use crate::storage::ensure_handoff_exists;
use crate::storage::tasks::{build_task_index, TaskIndex};

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

    let result = serde_json::json!({
        "task_tree": filtered_tree,
        "task_summary": summary,
    });

    serde_json::to_string_pretty(&result).context("Failed to serialize task list")
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
