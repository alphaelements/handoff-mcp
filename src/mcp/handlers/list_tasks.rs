use anyhow::{Context, Result};
use serde_json::Value;

use super::resolve_project_dir;
use crate::storage::config::read_config;
use crate::storage::ensure_handoff_exists;
use crate::storage::tasks::build_task_index;

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

    let filtered_tree = if let Some(filter) = status_filter {
        filter_tree(&tree, filter)
    } else {
        tree
    };

    let result = serde_json::json!({
        "task_tree": filtered_tree,
        "task_summary": summary,
    });

    serde_json::to_string_pretty(&result).context("Failed to serialize task list")
}

fn filter_tree(
    tree: &[crate::storage::tasks::TaskIndex],
    status: &str,
) -> Vec<crate::storage::tasks::TaskIndex> {
    tree.iter()
        .filter_map(|node| {
            let children = filter_tree(&node.children, status);
            if node.status == status || !children.is_empty() {
                Some(crate::storage::tasks::TaskIndex {
                    id: node.id.clone(),
                    title: node.title.clone(),
                    status: node.status.clone(),
                    children,
                })
            } else {
                None
            }
        })
        .collect()
}
