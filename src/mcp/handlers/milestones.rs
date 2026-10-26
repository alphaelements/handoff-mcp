//! Milestone CRUD over the `[milestones.<name>]` config.toml section.
//! Mirrors the VSCode extension's addMilestone/updateMilestone/removeMilestone.

use std::path::Path;

use anyhow::Result;
use serde_json::{json, Value};
use toml_edit::{Item, Table};

use super::config_crud::{
    config_path, ensure_subtable, ensure_table, load_doc, require_str, save_doc, set_opt_i64,
    set_opt_str, set_string_array,
};
use super::HandlerContext;
use crate::storage::config::{
    is_forward_milestone_transition, read_config_lenient, MILESTONE_DEFAULT_STATUS,
    MILESTONE_FROZEN_STATUS, MILESTONE_STATUSES,
};

/// handoff_list_milestones — return every `[milestones.*]` entry, ordered by
/// `order` (unordered entries last, ties by name), optionally narrowed by
/// `status_filter` (an unset status counts as `planning`).
pub fn handle_list(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let path = config_path(ctx)?;
    let doc = load_doc(&path)?;

    let status_filter = match arguments.get("status_filter") {
        None | Some(Value::Null) => None,
        Some(v) => Some(validate_status(v.as_str().unwrap_or_default())?),
    };

    let mut entries: Vec<(&str, &Table)> = doc
        .get("milestones")
        .and_then(|v| v.as_table())
        .into_iter()
        .flat_map(|t| t.iter())
        .filter_map(|(name, item)| item.as_table().map(|t| (name, t)))
        .filter(|(_, sub)| status_filter.is_none_or(|f| table_status(sub) == f))
        .collect();
    entries.sort_by_key(|(name, sub)| {
        let order = table_order(sub);
        (order.is_none(), order, *name)
    });

    let mut out = serde_json::Map::new();
    for (name, sub) in entries {
        out.insert(
            name.to_string(),
            json!({
                "date": sub.get("date").and_then(|v| v.as_str()),
                "color": sub.get("color").and_then(|v| v.as_str()),
                "description": sub.get("description").and_then(|v| v.as_str()),
                "version": sub.get("version").and_then(|v| v.as_str()),
                "status": table_status(sub),
                "order": table_order(sub),
                "depends_on": table_depends_on(sub),
            }),
        );
    }
    serde_json::to_string_pretty(&json!({ "milestones": out })).map_err(Into::into)
}

/// handoff_add_milestone — create a `[milestones.<name>]` entry. Fails if it exists.
pub fn handle_add(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let path = config_path(ctx)?;
    let name = require_str(arguments, "name")?;
    let mut doc = load_doc(&path)?;

    let table = ensure_table(&mut doc, "milestones")?;
    table.set_implicit(true);
    if table.contains_key(name) {
        anyhow::bail!(
            "Milestone '{name}' already exists. Use handoff_update_milestone to modify it."
        );
    }
    let fields = ValidatedFields::parse(arguments)?;
    let mut sub = Table::new();
    fields.apply(&mut sub, arguments);
    let mut msg = format!("Added milestone '{name}'");
    append_dependency_warnings(&mut msg, name, &fields.depends_on, table);
    doc["milestones"][name] = Item::Table(sub);

    save_doc(&path, &doc)?;
    Ok(msg)
}

/// handoff_update_milestone — patch an existing `[milestones.<name>]` entry.
/// A `status` change must follow the lifecycle (see
/// `is_forward_milestone_transition`) unless `force` is true.
pub fn handle_update(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let path = config_path(ctx)?;
    let name = require_str(arguments, "name")?;
    let mut doc = load_doc(&path)?;

    let existing = doc
        .get("milestones")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get(name))
        .and_then(|v| v.as_table());
    let Some(existing) = existing else {
        anyhow::bail!("Milestone '{name}' not found. Use handoff_add_milestone to create it.");
    };

    let fields = ValidatedFields::parse(arguments)?;
    if let Some(target) = fields.status {
        let force = arguments
            .get("force")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let current = table_status(existing);
        if !force && !is_forward_milestone_transition(current, target) {
            anyhow::bail!(
                "Milestone '{name}': status {current} -> {target} is not a forward lifecycle \
                 transition (planning -> active -> frozen -> released; cancelled is allowed \
                 from any status). Pass force: true to override."
            );
        }
    }

    let milestones = ensure_table(&mut doc, "milestones")?;
    let mut msg = format!("Updated milestone '{name}'");
    append_dependency_warnings(&mut msg, name, &fields.depends_on, milestones);
    let sub = ensure_subtable(&mut doc, "milestones", name)?;
    fields.apply(sub, arguments);

    save_doc(&path, &doc)?;
    Ok(msg)
}

/// handoff_remove_milestone — delete a `[milestones.<name>]` entry.
pub fn handle_remove(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let path = config_path(ctx)?;
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

/// The effective status of a `[milestones.<name>]` table (`planning` when unset).
fn table_status(table: &Table) -> &str {
    table
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or(MILESTONE_DEFAULT_STATUS)
}

fn table_order(table: &Table) -> Option<i64> {
    table.get("order").and_then(|v| v.as_integer())
}

fn table_depends_on(table: &Table) -> Vec<&str> {
    table
        .get("depends_on")
        .and_then(|v| v.as_array())
        .into_iter()
        .flat_map(|a| a.iter())
        .filter_map(|v| v.as_str())
        .collect()
}

/// `status` must be one of [`MILESTONE_STATUSES`]; returns it on success.
fn validate_status(status: &str) -> Result<&str> {
    if MILESTONE_STATUSES.contains(&status) {
        Ok(status)
    } else {
        anyhow::bail!(
            "Invalid milestone status '{status}'. Valid values: {}.",
            MILESTONE_STATUSES.join(", ")
        )
    }
}

/// The new schema fields of an add/update request, type-checked before
/// anything is written so a rejected request leaves config.toml untouched.
struct ValidatedFields<'a> {
    /// The status this request sets (a null clear counts as `planning`).
    status: Option<&'a str>,
    depends_on: Vec<&'a str>,
}

impl<'a> ValidatedFields<'a> {
    fn parse(arguments: &'a Value) -> Result<Self> {
        let status = match arguments.get("status") {
            None => None,
            // null clears the key, i.e. back to the default status.
            Some(Value::Null) => Some(MILESTONE_DEFAULT_STATUS),
            Some(v) => {
                Some(validate_status(v.as_str().ok_or_else(|| {
                    anyhow::anyhow!("'status' must be a string or null")
                })?)?)
            }
        };
        match arguments.get("order") {
            None | Some(Value::Null) => {}
            Some(v) if v.is_i64() => {}
            Some(_) => anyhow::bail!("'order' must be an integer or null"),
        }
        match arguments.get("version") {
            None | Some(Value::Null) | Some(Value::String(_)) => {}
            Some(_) => anyhow::bail!("'version' must be a string or null"),
        }
        let depends_on = match arguments.get("depends_on") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| {
                    v.as_str()
                        .ok_or_else(|| anyhow::anyhow!("'depends_on' must be an array of strings"))
                })
                .collect::<Result<_>>()?,
            Some(_) => anyhow::bail!("'depends_on' must be an array of strings or null"),
        };
        Ok(Self { status, depends_on })
    }

    fn apply(&self, table: &mut Table, arguments: &Value) {
        table.set_implicit(false);
        set_opt_str(table, "date", arguments.get("date"));
        set_opt_str(table, "color", arguments.get("color"));
        set_opt_str(table, "description", arguments.get("description"));
        set_opt_str(table, "version", arguments.get("version"));
        set_opt_str(table, "status", arguments.get("status"));
        set_opt_i64(table, "order", arguments.get("order"));
        set_string_array(table, "depends_on", arguments.get("depends_on"));
    }
}

/// Append a `Warning:` line for every `depends_on` entry that names the
/// milestone itself or has no `[milestones.<dep>]` definition. Non-fatal: the
/// dependency is still saved, because the target may be defined later.
fn append_dependency_warnings(msg: &mut String, name: &str, depends_on: &[&str], defined: &Table) {
    for dep in depends_on {
        if *dep == name {
            msg.push_str(&format!(
                "\nWarning: milestone '{name}' lists itself in depends_on."
            ));
        } else if !defined.contains_key(dep) {
            msg.push_str(&format!(
                "\nWarning: milestone '{name}' depends_on '{dep}', which has no \
                 [milestones.{dep}] definition in config.toml."
            ));
        }
    }
}

/// Advisory warning for moving a task into a frozen milestone (REQ-MS-CFG-002).
///
/// `previous` is the task's milestone before the write (`None` on create) and
/// `current` the one it will have after. Only an actual change to a frozen
/// milestone warns — re-sending the unchanged name does not add a task to it.
/// Never blocks. A missing or unreadable config.toml yields no warning: this is
/// an advisory side-check and the config problem is reported by the tools that
/// own the config.
pub fn frozen_assignment_warning(
    handoff_dir: &Path,
    previous: Option<&str>,
    current: Option<&str>,
) -> Option<String> {
    let current = current?;
    if previous == Some(current) {
        return None;
    }
    let (config, config_warning) = read_config_lenient(handoff_dir);
    let Some(config) = config else {
        return config_warning.map(|w| {
            format!(
                "Warning: could not check milestone status for '{current}' \
                 (config.toml unreadable: {w})."
            )
        });
    };
    let status = config
        .milestones
        .get(current)?
        .effective_status()
        .to_string();
    (status == MILESTONE_FROZEN_STATUS).then(|| {
        format!(
            "Warning: milestone '{current}' is frozen; the task was still assigned to it. \
             Move it elsewhere, or unfreeze the milestone with handoff_update_milestone \
             (status, force: true) if this is unintended."
        )
    })
}
