use anyhow::{Context, Result};
use serde_json::Value;

use super::resolve_project_dir;
use crate::storage::ensure_handoff_exists;

pub fn handle(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;
    let sessions_dir = handoff.join("sessions");

    let session_id = arguments
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'session_id' parameter is required"))?;

    if !sessions_dir.exists() {
        anyhow::bail!("Sessions directory not found");
    }

    for entry in std::fs::read_dir(&sessions_dir)
        .with_context(|| format!("Failed to read sessions dir: {}", sessions_dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".json") {
            continue;
        }

        let content = match std::fs::read_to_string(entry.path()) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let data: Value = match serde_json::from_str(&content) {
            Ok(d) => d,
            Err(_) => continue,
        };

        let file_id = data.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if file_id == session_id || name.contains(session_id) {
            return serde_json::to_string_pretty(&data).map_err(Into::into);
        }
    }

    anyhow::bail!(
        "Session not found: {session_id}. Use handoff_list_sessions to see available session IDs."
    )
}
