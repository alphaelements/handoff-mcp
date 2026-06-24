//! Milestone CRUD over the `[milestones.<name>]` config.toml section.
//! Mirrors the VSCode extension's addMilestone/updateMilestone/removeMilestone.

use anyhow::Result;
use serde_json::{json, Value};
use toml_edit::{Item, Table};

use super::config_crud::{
    config_path, ensure_subtable, ensure_table, load_doc, require_str, save_doc, set_opt_str,
};

/// handoff_list_milestones — return every `[milestones.*]` entry.
pub fn handle_list(arguments: &Value) -> Result<String> {
    let path = config_path(arguments)?;
    let doc = load_doc(&path)?;

    let mut out = serde_json::Map::new();
    if let Some(table) = doc.get("milestones").and_then(|v| v.as_table()) {
        for (name, item) in table.iter() {
            let sub = match item.as_table() {
                Some(t) => t,
                None => continue,
            };
            out.insert(
                name.to_string(),
                json!({
                    "date": sub.get("date").and_then(|v| v.as_str()),
                    "color": sub.get("color").and_then(|v| v.as_str()),
                    "description": sub.get("description").and_then(|v| v.as_str()),
                }),
            );
        }
    }
    serde_json::to_string_pretty(&json!({ "milestones": out })).map_err(Into::into)
}

/// handoff_add_milestone — create a `[milestones.<name>]` entry. Fails if it exists.
pub fn handle_add(arguments: &Value) -> Result<String> {
    let path = config_path(arguments)?;
    let name = require_str(arguments, "name")?;
    let mut doc = load_doc(&path)?;

    let table = ensure_table(&mut doc, "milestones")?;
    table.set_implicit(true);
    if table.contains_key(name) {
        anyhow::bail!(
            "Milestone '{name}' already exists. Use handoff_update_milestone to modify it."
        );
    }
    let mut sub = Table::new();
    apply_fields(&mut sub, arguments);
    doc["milestones"][name] = Item::Table(sub);

    save_doc(&path, &doc)?;
    Ok(format!("Added milestone '{name}'"))
}

/// handoff_update_milestone — patch an existing `[milestones.<name>]` entry.
pub fn handle_update(arguments: &Value) -> Result<String> {
    let path = config_path(arguments)?;
    let name = require_str(arguments, "name")?;
    let mut doc = load_doc(&path)?;

    let exists = doc
        .get("milestones")
        .and_then(|v| v.as_table())
        .map(|t| t.contains_key(name))
        .unwrap_or(false);
    if !exists {
        anyhow::bail!("Milestone '{name}' not found. Use handoff_add_milestone to create it.");
    }
    let sub = ensure_subtable(&mut doc, "milestones", name)?;
    apply_fields(sub, arguments);

    save_doc(&path, &doc)?;
    Ok(format!("Updated milestone '{name}'"))
}

/// handoff_remove_milestone — delete a `[milestones.<name>]` entry.
pub fn handle_remove(arguments: &Value) -> Result<String> {
    let path = config_path(arguments)?;
    let name = require_str(arguments, "name")?;
    let mut doc = load_doc(&path)?;

    let removed = doc
        .get_mut("milestones")
        .and_then(|v| v.as_table_mut())
        .map(|t| t.remove(name).is_some())
        .unwrap_or(false);
    if !removed {
        anyhow::bail!("Milestone '{name}' not found.");
    }
    save_doc(&path, &doc)?;
    Ok(format!("Removed milestone '{name}'"))
}

fn apply_fields(table: &mut Table, arguments: &Value) {
    table.set_implicit(false);
    set_opt_str(table, "date", arguments.get("date"));
    set_opt_str(table, "color", arguments.get("color"));
    set_opt_str(table, "description", arguments.get("description"));
}
