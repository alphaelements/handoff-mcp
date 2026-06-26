use anyhow::{Context, Result};
use serde_json::Value;
use toml_edit::{DocumentMut, Item, Value as TomlValue};

use super::resolve_project_dir;
use crate::storage::config::{read_config, write_config};
use crate::storage::ensure_handoff_exists;

pub fn handle_get(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;

    let handoff = ensure_handoff_exists(&project_dir)?;
    let config_path = handoff.join("config.toml");

    let raw = std::fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read config: {}", config_path.display()))?;
    let doc: DocumentMut = raw.parse().with_context(|| "Failed to parse config.toml")?;

    let toml_str = doc.to_string();
    let toml_value: toml::Value =
        toml::from_str(&toml_str).with_context(|| "Failed to deserialize config")?;
    let json_value =
        serde_json::to_value(&toml_value).with_context(|| "Failed to convert to JSON")?;

    serde_json::to_string_pretty(&json_value).context("Failed to format config")
}

pub fn handle_update(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;

    let handoff = ensure_handoff_exists(&project_dir)?;
    let config_path = handoff.join("config.toml");

    let updates = arguments
        .get("updates")
        .ok_or_else(|| anyhow::anyhow!("'updates' parameter is required"))?;

    let updates = updates
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("'updates' must be an object"))?;

    // Separate typed keys (settings/dashboard/project) from raw TOML keys
    let typed_keys = [
        "settings.history_limit",
        "settings.done_task_limit",
        "settings.auto_git_summary",
        "settings.require_estimate_hours",
        "settings.ai_estimate_multiplier",
        "settings.context_files",
        "dashboard.scan_dirs",
        "dashboard.exclude_patterns",
        "project.name",
        "project.description",
    ];

    let mut has_typed = false;
    let mut has_raw = false;

    for key in updates.keys() {
        if typed_keys.contains(&key.as_str()) {
            has_typed = true;
        } else {
            has_raw = true;
        }
    }

    let mut applied = Vec::new();

    // Handle typed keys via existing Config struct
    if has_typed {
        let mut config = read_config(&config_path)?;
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
                "settings.require_estimate_hours" => {
                    if let Some(b) = value.as_bool() {
                        config.settings.require_estimate_hours = b;
                        applied.push(format!("settings.require_estimate_hours = {b}"));
                    }
                }
                "settings.ai_estimate_multiplier" => {
                    if let Some(n) = value.as_f64() {
                        if n < 0.0 {
                            anyhow::bail!("settings.ai_estimate_multiplier must be >= 0");
                        }
                        config.settings.ai_estimate_multiplier = n;
                        applied.push(format!("settings.ai_estimate_multiplier = {n}"));
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
                _ => {}
            }
        }
        write_config(&config_path, &config)?;
    }

    // Handle raw TOML keys (calendar, assignees, effort_budget, gantt_view, etc.)
    if has_raw {
        let raw = std::fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read config: {}", config_path.display()))?;
        let mut doc: DocumentMut = raw
            .parse()
            .with_context(|| "Failed to parse config.toml for raw update")?;

        for (key, value) in updates {
            if typed_keys.contains(&key.as_str()) {
                continue;
            }

            let parts: Vec<&str> = key.split('.').collect();
            if parts.is_empty() {
                applied.push(format!("{key}: invalid key"));
                continue;
            }

            match set_toml_value(&mut doc, &parts, value) {
                Ok(()) => applied.push(format!("{key} = {value}")),
                Err(e) => applied.push(format!("{key}: error ({e})")),
            }
        }

        crate::storage::atomic_write(&config_path, doc.to_string().as_bytes())
            .with_context(|| format!("Failed to write config: {}", config_path.display()))?;
    }

    if applied.is_empty() {
        Ok("No updates applied".to_string())
    } else {
        Ok(format!("Updated config:\n{}", applied.join("\n")))
    }
}

fn set_toml_value(doc: &mut DocumentMut, parts: &[&str], json_val: &Value) -> Result<()> {
    let toml_val = json_to_toml_value(json_val)?;

    match parts.len() {
        1 => {
            doc[parts[0]] = Item::Value(toml_val);
        }
        2 => {
            if !doc.contains_table(parts[0]) {
                doc[parts[0]] = Item::Table(toml_edit::Table::new());
            }
            doc[parts[0]][parts[1]] = Item::Value(toml_val);
        }
        3 => {
            if !doc.contains_table(parts[0]) {
                doc[parts[0]] = Item::Table(toml_edit::Table::new());
            }
            let table = doc[parts[0]]
                .as_table_mut()
                .ok_or_else(|| anyhow::anyhow!("{} is not a table", parts[0]))?;
            if !table.contains_table(parts[1]) {
                table[parts[1]] = Item::Table(toml_edit::Table::new());
            }
            table[parts[1]][parts[2]] = Item::Value(toml_val);
        }
        4 => {
            if !doc.contains_table(parts[0]) {
                doc[parts[0]] = Item::Table(toml_edit::Table::new());
            }
            let t0 = doc[parts[0]]
                .as_table_mut()
                .ok_or_else(|| anyhow::anyhow!("{} is not a table", parts[0]))?;
            if !t0.contains_table(parts[1]) {
                t0[parts[1]] = Item::Table(toml_edit::Table::new());
            }
            let t1 = t0[parts[1]]
                .as_table_mut()
                .ok_or_else(|| anyhow::anyhow!("{}.{} is not a table", parts[0], parts[1]))?;
            if !t1.contains_table(parts[2]) {
                t1[parts[2]] = Item::Table(toml_edit::Table::new());
            }
            t1[parts[2]][parts[3]] = Item::Value(toml_val);
        }
        _ => {
            anyhow::bail!("Key depth > 4 not supported");
        }
    }

    Ok(())
}

fn json_to_toml_value(val: &Value) -> Result<TomlValue> {
    match val {
        Value::String(s) => Ok(TomlValue::from(s.as_str())),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(TomlValue::from(i))
            } else if let Some(f) = n.as_f64() {
                Ok(TomlValue::from(f))
            } else {
                anyhow::bail!("Unsupported number: {n}")
            }
        }
        Value::Bool(b) => Ok(TomlValue::from(*b)),
        Value::Array(arr) => {
            let mut toml_arr = toml_edit::Array::new();
            for item in arr {
                toml_arr.push(json_to_toml_value(item)?);
            }
            Ok(TomlValue::Array(toml_arr))
        }
        Value::Object(obj) => {
            let mut inline = toml_edit::InlineTable::new();
            for (k, v) in obj {
                inline.insert(k, json_to_toml_value(v)?);
            }
            Ok(TomlValue::InlineTable(inline))
        }
        Value::Null => Ok(TomlValue::from("")),
    }
}
