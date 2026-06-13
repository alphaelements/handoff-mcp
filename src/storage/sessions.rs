use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionData {
    pub version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dirty_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checklist: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub handoff_notes: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_pointers: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<Value>,
}

pub fn generate_session_filename(summary: &str, timestamp: &str) -> String {
    let slug = summary_to_slug(summary);
    format!("{timestamp}-{slug}")
}

fn summary_to_slug(summary: &str) -> String {
    let slug: String = summary
        .to_lowercase()
        .chars()
        .take(40)
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let slug = slug.trim_matches('-').to_string();
    let mut result = String::new();
    let mut prev_dash = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_dash {
                result.push(c);
            }
            prev_dash = true;
        } else {
            result.push(c);
            prev_dash = false;
        }
    }
    if result.is_empty() {
        "session".to_string()
    } else {
        result
    }
}

pub fn write_active_session(sessions_dir: &Path, data: &SessionData) -> Result<PathBuf> {
    let timestamp = data.ended_at.as_deref().unwrap_or("00000000-000000");
    let ts_compact = timestamp
        .replace(['-', ':'], "")
        .replace('T', "-")
        .replace('Z', "");
    let ts_part = if ts_compact.len() >= 15 {
        &ts_compact[..15]
    } else {
        &ts_compact
    };

    let base = generate_session_filename(&data.summary, ts_part);
    let filename = format!("{base}.active.json");
    let path = sessions_dir.join(&filename);

    let content = serde_json::to_string_pretty(data).context("Failed to serialize session")?;
    std::fs::write(&path, content)
        .with_context(|| format!("Failed to write session: {}", path.display()))?;

    Ok(path)
}

pub fn close_active_sessions(sessions_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut closed = Vec::new();

    if !sessions_dir.exists() {
        return Ok(closed);
    }

    for entry in std::fs::read_dir(sessions_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".active.json") {
            let new_name = name.replace(".active.json", ".closed.json");
            let new_path = sessions_dir.join(&new_name);
            std::fs::rename(entry.path(), &new_path)
                .with_context(|| format!("Failed to close session: {}", entry.path().display()))?;
            closed.push(new_path);
        }
    }

    Ok(closed)
}

pub fn read_active_sessions(sessions_dir: &Path) -> Result<Vec<SessionData>> {
    let mut sessions = Vec::new();

    if !sessions_dir.exists() {
        return Ok(sessions);
    }

    let mut entries: Vec<_> = std::fs::read_dir(sessions_dir)?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".active.json") {
            let content = std::fs::read_to_string(entry.path())
                .with_context(|| format!("Failed to read session: {}", entry.path().display()))?;
            let data: SessionData = serde_json::from_str(&content)
                .with_context(|| format!("Failed to parse session: {}", entry.path().display()))?;
            sessions.push(data);
        }
    }

    Ok(sessions)
}

pub fn enforce_history_limit(sessions_dir: &Path, limit: u32) -> Result<u32> {
    if !sessions_dir.exists() {
        return Ok(0);
    }

    let mut closed_files: Vec<PathBuf> = Vec::new();

    for entry in std::fs::read_dir(sessions_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".closed.json") {
            closed_files.push(entry.path());
        }
    }

    closed_files.sort();

    let mut removed = 0u32;
    while closed_files.len() > limit as usize {
        if let Some(oldest) = closed_files.first() {
            std::fs::remove_file(oldest)
                .with_context(|| format!("Failed to remove old session: {}", oldest.display()))?;
            closed_files.remove(0);
            removed += 1;
        }
    }

    Ok(removed)
}
