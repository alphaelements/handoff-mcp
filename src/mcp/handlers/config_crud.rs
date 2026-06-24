//! Shared helpers for config.toml CRUD handlers (assignees, milestones,
//! calendar, labels, project start). All mutate the raw TOML document via
//! `toml_edit` so that comments and unrelated keys are preserved, then write it
//! back atomically.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;
use toml_edit::{Array, DocumentMut, Item, Value as TomlValue};

use super::resolve_project_dir;
use crate::storage::ensure_handoff_exists;

/// Resolve the project dir, ensure `.handoff/` exists, and return the
/// config.toml path.
pub fn config_path(arguments: &Value) -> Result<PathBuf> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;
    Ok(handoff.join("config.toml"))
}

/// Parse config.toml into a mutable document.
pub fn load_doc(path: &Path) -> Result<DocumentMut> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read config: {}", path.display()))?;
    raw.parse::<DocumentMut>()
        .with_context(|| format!("Failed to parse config: {}", path.display()))
}

/// Serialize and atomically write the document back to disk.
pub fn save_doc(path: &Path, doc: &DocumentMut) -> Result<()> {
    crate::storage::atomic_write(path, doc.to_string().as_bytes())
        .with_context(|| format!("Failed to write config: {}", path.display()))
}

/// Required string argument or a descriptive error.
pub fn require_str<'a>(arguments: &'a Value, key: &str) -> Result<&'a str> {
    arguments
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'{key}' is required"))
}

/// Set `table[field]` to a string if the JSON arg is present; if it is JSON
/// null, remove the key (allows clearing a value).
pub fn set_opt_str(table: &mut toml_edit::Table, field: &str, arg: Option<&Value>) {
    match arg {
        Some(Value::Null) => {
            table.remove(field);
        }
        Some(v) => {
            if let Some(s) = v.as_str() {
                table[field] = Item::Value(TomlValue::from(s));
            }
        }
        None => {}
    }
}

/// Set `table[field]` to a number if present; null removes it.
pub fn set_opt_f64(table: &mut toml_edit::Table, field: &str, arg: Option<&Value>) {
    match arg {
        Some(Value::Null) => {
            table.remove(field);
        }
        Some(v) => {
            if let Some(n) = v.as_f64() {
                table[field] = Item::Value(TomlValue::from(n));
            }
        }
        None => {}
    }
}

/// Set `table[field]` to a bool if present; null removes it.
pub fn set_opt_bool(table: &mut toml_edit::Table, field: &str, arg: Option<&Value>) {
    match arg {
        Some(Value::Null) => {
            table.remove(field);
        }
        Some(v) => {
            if let Some(b) = v.as_bool() {
                table[field] = Item::Value(TomlValue::from(b));
            }
        }
        None => {}
    }
}

/// Set `table[field]` to a TOML array of strings if the JSON arg is an array;
/// null removes it.
pub fn set_string_array(table: &mut toml_edit::Table, field: &str, arg: Option<&Value>) {
    match arg {
        Some(Value::Null) => {
            table.remove(field);
        }
        Some(Value::Array(items)) => {
            let mut arr = Array::new();
            for it in items {
                if let Some(s) = it.as_str() {
                    arr.push(s);
                }
            }
            table[field] = Item::Value(TomlValue::Array(arr));
        }
        _ => {}
    }
}

/// Set `table[field]` to a TOML array preserving integer-or-string items
/// (used for `closed_weekdays`, which may be numbers or weekday names); null removes it.
pub fn set_mixed_array(table: &mut toml_edit::Table, field: &str, arg: Option<&Value>) {
    match arg {
        Some(Value::Null) => {
            table.remove(field);
        }
        Some(Value::Array(items)) => {
            let mut arr = Array::new();
            for it in items {
                if let Some(i) = it.as_i64() {
                    arr.push(i);
                } else if let Some(s) = it.as_str() {
                    arr.push(s);
                }
            }
            table[field] = Item::Value(TomlValue::Array(arr));
        }
        _ => {}
    }
}

/// Replace a nested `[parent.<key>.<field>]`-style map of `{name: hours}` with
/// the provided JSON object (e.g. `day_hours`). null removes the whole sub-table.
pub fn set_f64_map(table: &mut toml_edit::Table, field: &str, arg: Option<&Value>) {
    match arg {
        Some(Value::Null) => {
            table.remove(field);
        }
        Some(Value::Object(map)) => {
            let mut sub = toml_edit::Table::new();
            sub.set_implicit(false);
            for (k, v) in map {
                if let Some(n) = v.as_f64() {
                    sub[k] = Item::Value(TomlValue::from(n));
                }
            }
            table[field] = Item::Table(sub);
        }
        _ => {}
    }
}

/// Get a mutable reference to `doc[section]` as a table, creating it if absent.
pub fn ensure_table<'a>(
    doc: &'a mut DocumentMut,
    section: &str,
) -> Result<&'a mut toml_edit::Table> {
    if !doc.contains_table(section) {
        doc[section] = Item::Table(toml_edit::Table::new());
    }
    doc[section]
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("[{section}] exists but is not a table"))
}

/// Get a mutable reference to `doc[section][key]` as a table, creating both if
/// absent. Marks the parent as a dotted/implicit table so it serializes as
/// `[section.key]`.
pub fn ensure_subtable<'a>(
    doc: &'a mut DocumentMut,
    section: &str,
    key: &str,
) -> Result<&'a mut toml_edit::Table> {
    let parent = ensure_table(doc, section)?;
    parent.set_implicit(true);
    if !parent.contains_table(key) {
        parent[key] = Item::Table(toml_edit::Table::new());
    }
    parent[key]
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("[{section}.{key}] exists but is not a table"))
}
