use anyhow::Result;
use serde_json::Value;

use super::resolve_project_dir;
use crate::storage;

pub fn handle(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;

    let project_name = arguments
        .get("project_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("project_name is required"))?;

    let description = arguments
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    storage::init_handoff(&project_dir, project_name, description)?;

    Ok(format!(
        "Initialized handoff tracking for '{}' at {}/.handoff/\n\
         Created: config.toml, sessions/, tasks/, memory/",
        project_name,
        project_dir.display()
    ))
}
