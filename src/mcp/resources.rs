use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::storage::config::read_config;
use crate::storage::sessions::{read_active_sessions, read_open_sessions, read_paused_sessions};
use crate::storage::{ensure_handoff_exists, handoff_dir};

pub fn handle_resource_read(uri: &str) -> Result<Value> {
    let project_dir = std::env::current_dir().context("Failed to get current directory")?;
    let hdir = handoff_dir(&project_dir);

    if !hdir.exists() {
        anyhow::bail!("No .handoff/ directory found. Run handoff_init first.");
    }

    let handoff = ensure_handoff_exists(&project_dir)?;

    match uri {
        "handoff://sessions" => read_sessions_resource(&handoff),
        "handoff://config" => read_config_resource(&handoff),
        _ => anyhow::bail!("Unknown resource URI: {uri}"),
    }
}

fn read_sessions_resource(handoff: &std::path::Path) -> Result<Value> {
    let sessions_dir = handoff.join("sessions");
    let mut sessions = read_open_sessions(&sessions_dir)?;
    sessions.extend(read_active_sessions(&sessions_dir)?);
    sessions.extend(read_paused_sessions(&sessions_dir)?);

    let contents: Vec<Value> = sessions
        .iter()
        .map(|s| serde_json::to_value(s).unwrap_or_default())
        .collect();

    Ok(json!({
        "contents": [{
            "uri": "handoff://sessions",
            "mimeType": "application/json",
            "text": serde_json::to_string_pretty(&contents)?
        }]
    }))
}

fn read_config_resource(handoff: &std::path::Path) -> Result<Value> {
    let config_path = handoff.join("config.toml");
    let config = read_config(&config_path)?;
    let toml_str = toml::to_string_pretty(&config).context("Failed to serialize config")?;

    Ok(json!({
        "contents": [{
            "uri": "handoff://config",
            "mimeType": "application/toml",
            "text": toml_str
        }]
    }))
}
