use anyhow::{Context, Result};
use serde_json::Value;

use super::resolve_project_dir;
use crate::storage::config::{read_config, write_config};
use crate::storage::ensure_handoff_exists;

pub fn handle_get(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;

    let handoff = ensure_handoff_exists(&project_dir)?;
    let config = read_config(&handoff.join("config.toml"))?;
    let result = serde_json::to_value(&config).context("Failed to serialize config")?;
    serde_json::to_string_pretty(&result).context("Failed to format config")
}

pub fn handle_update(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;

    let handoff = ensure_handoff_exists(&project_dir)?;
    let config_path = handoff.join("config.toml");
    let mut config = read_config(&config_path)?;

    let updates = arguments
        .get("updates")
        .ok_or_else(|| anyhow::anyhow!("'updates' parameter is required"))?;

    let updates = updates
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("'updates' must be an object"))?;

    let mut applied = Vec::new();

    for (key, value) in updates {
        match key.as_str() {
            "settings.history_limit" => {
                if let Some(n) = value.as_u64() {
                    config.settings.history_limit = n as u32;
                    applied.push(format!("settings.history_limit = {n}"));
                }
            }
            "settings.done_task_limit" => {
                if let Some(n) = value.as_u64() {
                    config.settings.done_task_limit = n as u32;
                    applied.push(format!("settings.done_task_limit = {n}"));
                }
            }
            "settings.auto_git_summary" => {
                if let Some(b) = value.as_bool() {
                    config.settings.auto_git_summary = b;
                    applied.push(format!("settings.auto_git_summary = {b}"));
                }
            }
            "settings.context_files" => {
                if let Some(arr) = value.as_array() {
                    config.settings.context_files = arr
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();
                    applied.push(format!(
                        "settings.context_files = {:?}",
                        config.settings.context_files
                    ));
                }
            }
            "dashboard.scan_dirs" => {
                if let Some(arr) = value.as_array() {
                    config.dashboard.scan_dirs = arr
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();
                    applied.push(format!(
                        "dashboard.scan_dirs = {:?}",
                        config.dashboard.scan_dirs
                    ));
                }
            }
            "dashboard.exclude_patterns" => {
                if let Some(arr) = value.as_array() {
                    config.dashboard.exclude_patterns = arr
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();
                    applied.push(format!(
                        "dashboard.exclude_patterns = {:?}",
                        config.dashboard.exclude_patterns
                    ));
                }
            }
            "project.name" => {
                if let Some(s) = value.as_str() {
                    config.project.name = s.to_string();
                    applied.push(format!("project.name = {s}"));
                }
            }
            "project.description" => {
                if let Some(s) = value.as_str() {
                    config.project.description = Some(s.to_string());
                    applied.push(format!("project.description = {s}"));
                }
            }
            other => {
                applied.push(format!("{other}: unknown key (skipped)"));
            }
        }
    }

    write_config(&config_path, &config)?;

    if applied.is_empty() {
        Ok("No updates applied".to_string())
    } else {
        Ok(format!("Updated config:\n{}", applied.join("\n")))
    }
}
