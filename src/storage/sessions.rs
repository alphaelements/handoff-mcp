use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionData {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
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

pub fn generate_session_id() -> String {
    let now = chrono::Utc::now();
    format!("s-{}", now.format("%Y%m%d-%H%M%S-%6f"))
}

pub fn generate_session_filename(summary: &str, timestamp: &str) -> String {
    let slug = summary_to_slug(summary);
    format!("{timestamp}-{slug}")
}

fn summary_to_slug(summary: &str) -> String {
    let slug: String = summary
        .chars()
        .take(40)
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else if !c.is_ascii() {
                c
            } else {
                '-'
            }
        })
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

fn compact_timestamp(data: &SessionData) -> String {
    let timestamp = data.ended_at.as_deref().unwrap_or("00000000-000000");
    let ts_compact = timestamp
        .replace(['-', ':'], "")
        .replace('T', "-")
        .replace('Z', "");
    if ts_compact.len() >= 15 {
        ts_compact[..15].to_string()
    } else {
        ts_compact
    }
}

fn synthesize_id_from_filename(filename: &str) -> String {
    // Old format: YYYYMMDD-HHMMSS-slug.status.json
    // Extract timestamp part and create s-YYYYMMDD-HHMMSS-000000
    let base = filename
        .rsplit_once('.')
        .and_then(|(rest, _)| rest.rsplit_once('.'))
        .map(|(rest, _)| rest)
        .unwrap_or(filename);

    if base.len() >= 15 {
        let ts = &base[..15]; // YYYYMMDD-HHMMSS
        format!("s-{ts}-000000")
    } else {
        format!("s-{base}-000000")
    }
}

pub fn write_open_session(sessions_dir: &Path, data: &SessionData) -> Result<PathBuf> {
    let mut data = data.clone();
    if data.id.is_none() {
        data.id = Some(generate_session_id());
    }

    let ts_part = compact_timestamp(&data);
    let base = generate_session_filename(&data.summary, &ts_part);
    let filename = format!("{base}.open.json");
    let path = sessions_dir.join(&filename);

    let content = serde_json::to_string_pretty(&data).context("Failed to serialize session")?;
    std::fs::write(&path, content)
        .with_context(|| format!("Failed to write session: {}", path.display()))?;

    Ok(path)
}

pub fn read_sessions_by_status(sessions_dir: &Path, status: &str) -> Result<Vec<SessionData>> {
    let mut sessions = Vec::new();
    let suffix = format!(".{status}.json");

    if !sessions_dir.exists() {
        return Ok(sessions);
    }

    let mut entries: Vec<_> = std::fs::read_dir(sessions_dir)?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(&suffix) {
            let content = std::fs::read_to_string(entry.path())
                .with_context(|| format!("Failed to read session: {}", entry.path().display()))?;
            let mut data: SessionData = serde_json::from_str(&content)
                .with_context(|| format!("Failed to parse session: {}", entry.path().display()))?;
            if data.id.is_none() {
                data.id = Some(synthesize_id_from_filename(&name));
            }
            sessions.push(data);
        }
    }

    Ok(sessions)
}

pub fn read_open_sessions(sessions_dir: &Path) -> Result<Vec<SessionData>> {
    read_sessions_by_status(sessions_dir, "open")
}

pub fn read_active_sessions(sessions_dir: &Path) -> Result<Vec<SessionData>> {
    read_sessions_by_status(sessions_dir, "active")
}

fn transition_sessions(sessions_dir: &Path, from: &str, to: &str) -> Result<Vec<PathBuf>> {
    let mut transitioned = Vec::new();
    let from_suffix = format!(".{from}.json");
    let to_suffix = format!(".{to}.json");

    if !sessions_dir.exists() {
        return Ok(transitioned);
    }

    for entry in std::fs::read_dir(sessions_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(&from_suffix) {
            let new_name = name.replace(&from_suffix, &to_suffix);
            let new_path = sessions_dir.join(&new_name);
            std::fs::rename(entry.path(), &new_path).with_context(|| {
                format!(
                    "Failed to transition session {from}->{to}: {}",
                    entry.path().display()
                )
            })?;
            transitioned.push(new_path);
        }
    }

    Ok(transitioned)
}

fn transition_session_by_id(
    sessions_dir: &Path,
    session_id: &str,
    from: &str,
    to: &str,
) -> Result<Option<PathBuf>> {
    let from_suffix = format!(".{from}.json");
    let to_suffix = format!(".{to}.json");

    if !sessions_dir.exists() {
        return Ok(None);
    }

    for entry in std::fs::read_dir(sessions_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(&from_suffix) {
            continue;
        }

        let content = std::fs::read_to_string(entry.path())
            .with_context(|| format!("Failed to read session: {}", entry.path().display()))?;
        let data: SessionData = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse session: {}", entry.path().display()))?;

        let file_id = data.id.as_deref().unwrap_or("").to_string();
        let synthesized = if file_id.is_empty() {
            synthesize_id_from_filename(&name)
        } else {
            file_id
        };

        if synthesized == session_id {
            let new_name = name.replace(&from_suffix, &to_suffix);
            let new_path = sessions_dir.join(&new_name);
            std::fs::rename(entry.path(), &new_path).with_context(|| {
                format!(
                    "Failed to transition session {from}->{to}: {}",
                    entry.path().display()
                )
            })?;
            return Ok(Some(new_path));
        }
    }

    Ok(None)
}

pub fn activate_open_sessions(sessions_dir: &Path) -> Result<Vec<PathBuf>> {
    transition_sessions(sessions_dir, "open", "active")
}

pub fn activate_session_by_id(sessions_dir: &Path, session_id: &str) -> Result<Option<PathBuf>> {
    transition_session_by_id(sessions_dir, session_id, "open", "active")
}

pub fn close_active_sessions(sessions_dir: &Path) -> Result<Vec<PathBuf>> {
    transition_sessions(sessions_dir, "active", "closed")
}

pub fn close_open_sessions(sessions_dir: &Path) -> Result<Vec<PathBuf>> {
    transition_sessions(sessions_dir, "open", "closed")
}

pub fn close_session_by_id(sessions_dir: &Path, session_id: &str) -> Result<Option<PathBuf>> {
    // Try active first, then open, then paused
    if let Some(path) = transition_session_by_id(sessions_dir, session_id, "active", "closed")? {
        return Ok(Some(path));
    }
    if let Some(path) = transition_session_by_id(sessions_dir, session_id, "open", "closed")? {
        return Ok(Some(path));
    }
    transition_session_by_id(sessions_dir, session_id, "paused", "closed")
}

pub fn pause_active_sessions(sessions_dir: &Path) -> Result<Vec<PathBuf>> {
    transition_sessions(sessions_dir, "active", "paused")
}

pub fn pause_session_by_id(sessions_dir: &Path, session_id: &str) -> Result<Option<PathBuf>> {
    if let Some(path) = transition_session_by_id(sessions_dir, session_id, "active", "paused")? {
        return Ok(Some(path));
    }
    transition_session_by_id(sessions_dir, session_id, "open", "paused")
}

pub fn resume_paused_session_by_id(
    sessions_dir: &Path,
    session_id: &str,
) -> Result<Option<PathBuf>> {
    transition_session_by_id(sessions_dir, session_id, "paused", "active")
}

pub fn read_paused_sessions(sessions_dir: &Path) -> Result<Vec<SessionData>> {
    read_sessions_by_status(sessions_dir, "paused")
}

pub fn close_paused_sessions(sessions_dir: &Path) -> Result<Vec<PathBuf>> {
    transition_sessions(sessions_dir, "paused", "closed")
}

pub fn read_latest_closed_session(sessions_dir: &Path) -> Result<Option<SessionData>> {
    let mut sessions = read_sessions_by_status(sessions_dir, "closed")?;
    sessions.sort_by(|a, b| a.ended_at.cmp(&b.ended_at));
    Ok(sessions.into_iter().last())
}

fn apply_session_updates(data: &mut SessionData, updates: &SessionData) {
    data.summary = updates.summary.clone();
    data.ended_at = updates.ended_at.clone();
    data.branch = updates.branch.clone();
    data.commit = updates.commit.clone();
    data.dirty_files = updates.dirty_files.clone();
    data.decisions = updates.decisions.clone();
    data.blockers = updates.blockers.clone();
    data.checklist = updates.checklist.clone();
    data.handoff_notes = updates.handoff_notes.clone();
    data.references = updates.references.clone();
    data.context_pointers = updates.context_pointers.clone();
    if updates.environment.is_some() {
        data.environment = updates.environment.clone();
    }
}

fn find_and_update_active_session(
    sessions_dir: &Path,
    session_id: &str,
    updates: &SessionData,
    transition_to: Option<&str>,
) -> Result<Option<PathBuf>> {
    let suffix = ".active.json";

    if !sessions_dir.exists() {
        return Ok(None);
    }

    for entry in std::fs::read_dir(sessions_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(suffix) {
            continue;
        }

        let content = std::fs::read_to_string(entry.path())
            .with_context(|| format!("Failed to read session: {}", entry.path().display()))?;
        let mut data: SessionData = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse session: {}", entry.path().display()))?;

        let file_id = data.id.as_deref().unwrap_or("").to_string();
        let synthesized = if file_id.is_empty() {
            synthesize_id_from_filename(&name)
        } else {
            file_id
        };

        if synthesized != session_id {
            continue;
        }

        apply_session_updates(&mut data, updates);

        let updated_content =
            serde_json::to_string_pretty(&data).context("Failed to serialize session")?;
        std::fs::write(entry.path(), &updated_content)
            .with_context(|| format!("Failed to write session: {}", entry.path().display()))?;

        if let Some(target_status) = transition_to {
            let target_suffix = format!(".{target_status}.json");
            let new_name = name.replace(suffix, &target_suffix);
            let new_path = sessions_dir.join(&new_name);
            std::fs::rename(entry.path(), &new_path).with_context(|| {
                format!(
                    "Failed to transition session active->{target_status}: {}",
                    entry.path().display()
                )
            })?;
            return Ok(Some(new_path));
        }

        return Ok(Some(entry.path()));
    }

    Ok(None)
}

pub fn update_and_close_active_session(
    sessions_dir: &Path,
    session_id: &str,
    updates: &SessionData,
) -> Result<Option<PathBuf>> {
    find_and_update_active_session(sessions_dir, session_id, updates, Some("closed"))
}

pub fn update_active_session(
    sessions_dir: &Path,
    session_id: &str,
    updates: &SessionData,
) -> Result<Option<PathBuf>> {
    find_and_update_active_session(sessions_dir, session_id, updates, None)
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

pub fn write_session_with_status(
    sessions_dir: &Path,
    data: &SessionData,
    status: &str,
) -> Result<PathBuf> {
    let mut data = data.clone();
    if data.id.is_none() {
        data.id = Some(generate_session_id());
    }

    let ts_part = compact_timestamp(&data);
    let base = generate_session_filename(&data.summary, &ts_part);
    let filename = format!("{base}.{status}.json");
    let path = sessions_dir.join(&filename);

    let content = serde_json::to_string_pretty(&data).context("Failed to serialize session")?;
    std::fs::write(&path, content)
        .with_context(|| format!("Failed to write session: {}", path.display()))?;

    Ok(path)
}
