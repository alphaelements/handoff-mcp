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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_task_ids: Vec<String>,
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

fn extract_timestamp_from_id(id: &str) -> Option<String> {
    // "s-20260704-003557-792065" → "20260704-003557"
    let rest = id.strip_prefix("s-")?;
    if rest.len() >= 15 {
        Some(rest[..15].to_string())
    } else {
        None
    }
}

fn compact_timestamp(data: &SessionData) -> String {
    data.ended_at
        .as_deref()
        .map(|ts| {
            let compact = ts
                .replace(['-', ':'], "")
                .replace('T', "-")
                .replace('Z', "");
            if compact.len() >= 15 {
                compact[..15].to_string()
            } else {
                compact
            }
        })
        .or_else(|| data.id.as_deref().and_then(extract_timestamp_from_id))
        .unwrap_or_else(|| "00000000-000000".to_string())
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

fn ids_match(candidate: &str, query: &str) -> bool {
    candidate == query || candidate.starts_with(query) || query.starts_with(candidate)
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
    crate::storage::atomic_write(&path, content.as_bytes())
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

/// Append `decision` to active session(s). When `target_session_id` is Some,
/// only the matching session is updated; otherwise all active sessions are updated.
pub fn append_decision_to_active_sessions(
    sessions_dir: &Path,
    decision: serde_json::Value,
    target_session_id: Option<&str>,
) -> Result<usize> {
    let suffix = ".active.json";
    if !sessions_dir.exists() {
        return Ok(0);
    }

    let mut count = 0;
    for entry in std::fs::read_dir(sessions_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(suffix) {
            continue;
        }
        let path = entry.path();
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read session: {}", path.display()))?;
        let mut data: SessionData = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse session: {}", path.display()))?;

        if let Some(tid) = target_session_id {
            let file_id = data.id.as_deref().unwrap_or("");
            if !ids_match(file_id, tid) {
                continue;
            }
        }

        data.decisions.push(decision.clone());
        let updated = serde_json::to_string_pretty(&data).context("Failed to serialize session")?;
        crate::storage::atomic_write(&path, updated.as_bytes())
            .with_context(|| format!("Failed to write session: {}", path.display()))?;
        count += 1;
    }
    Ok(count)
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

    let mut matches: Vec<(PathBuf, String, String)> = Vec::new();

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
        let resolved_id = if file_id.is_empty() {
            synthesize_id_from_filename(&name)
        } else {
            file_id
        };

        if ids_match(&resolved_id, session_id) {
            matches.push((entry.path(), name, resolved_id));
        }
    }

    if matches.len() > 1 {
        let candidates: Vec<&str> = matches.iter().map(|(_, _, id)| id.as_str()).collect();
        anyhow::bail!(
            "Ambiguous session_id '{}': matched {} sessions ({}). Provide a more specific ID.",
            session_id,
            matches.len(),
            candidates.join(", ")
        );
    }

    if let Some((path, name, _)) = matches.into_iter().next() {
        let new_name = name.replace(&from_suffix, &to_suffix);
        let new_path = sessions_dir.join(&new_name);
        std::fs::rename(&path, &new_path).with_context(|| {
            format!(
                "Failed to transition session {from}->{to}: {}",
                path.display()
            )
        })?;
        return Ok(Some(new_path));
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

fn apply_session_updates(
    data: &mut SessionData,
    updates: &SessionData,
    provided_keys: Option<&Value>,
) {
    data.summary = updates.summary.clone();
    data.ended_at = updates.ended_at.clone();
    data.branch = updates.branch.clone();
    data.commit = updates.commit.clone();
    data.dirty_files = updates.dirty_files.clone();

    let was_provided = |key: &str| provided_keys.and_then(|v| v.get(key)).is_some();

    if was_provided("decisions") {
        data.decisions = updates.decisions.clone();
    }
    if was_provided("blockers") {
        data.blockers = updates.blockers.clone();
    }
    if was_provided("checklist") {
        data.checklist = updates.checklist.clone();
    }
    if was_provided("handoff_notes") {
        data.handoff_notes = updates.handoff_notes.clone();
    }
    if was_provided("references") {
        data.references = updates.references.clone();
    }
    if was_provided("context_pointers") {
        data.context_pointers = updates.context_pointers.clone();
    }
    if updates.environment.is_some() {
        data.environment = updates.environment.clone();
    }
    if updates.timeline.is_some() {
        data.timeline = updates.timeline.clone();
    }
    if updates.label.is_some() {
        data.label = updates.label.clone();
    }
    if was_provided("related_task_ids") && !updates.related_task_ids.is_empty() {
        data.related_task_ids = updates.related_task_ids.clone();
    }
}

fn find_and_update_active_session(
    sessions_dir: &Path,
    session_id: &str,
    updates: &SessionData,
    transition_to: Option<&str>,
    provided_keys: Option<&Value>,
) -> Result<Option<PathBuf>> {
    let suffix = ".active.json";

    if !sessions_dir.exists() {
        return Ok(None);
    }

    let mut matches: Vec<(PathBuf, String, String)> = Vec::new();

    for entry in std::fs::read_dir(sessions_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(suffix) {
            continue;
        }

        let content = std::fs::read_to_string(entry.path())
            .with_context(|| format!("Failed to read session: {}", entry.path().display()))?;
        let data: SessionData = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse session: {}", entry.path().display()))?;

        let file_id = data.id.as_deref().unwrap_or("").to_string();
        let resolved_id = if file_id.is_empty() {
            synthesize_id_from_filename(&name)
        } else {
            file_id
        };

        if ids_match(&resolved_id, session_id) {
            matches.push((entry.path(), name, resolved_id));
        }
    }

    if matches.len() > 1 {
        let candidates: Vec<&str> = matches.iter().map(|(_, _, id)| id.as_str()).collect();
        anyhow::bail!(
            "Ambiguous session_id '{}': matched {} sessions ({}). Provide a more specific ID.",
            session_id,
            matches.len(),
            candidates.join(", ")
        );
    }

    if let Some((path, _, _)) = matches.into_iter().next() {
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read session: {}", path.display()))?;
        let mut data: SessionData = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse session: {}", path.display()))?;

        apply_session_updates(&mut data, updates, provided_keys);

        let updated_content =
            serde_json::to_string_pretty(&data).context("Failed to serialize session")?;
        crate::storage::atomic_write(&path, updated_content.as_bytes())
            .with_context(|| format!("Failed to write session: {}", path.display()))?;

        if let Some(target_status) = transition_to {
            let ts_part = compact_timestamp(&data);
            let base = generate_session_filename(&data.summary, &ts_part);
            let new_name = format!("{base}.{target_status}.json");
            let new_path = sessions_dir.join(&new_name);
            std::fs::rename(&path, &new_path).with_context(|| {
                format!(
                    "Failed to transition session active->{target_status}: {}",
                    path.display()
                )
            })?;
            return Ok(Some(new_path));
        }

        return Ok(Some(path));
    }

    Ok(None)
}

pub fn update_and_close_active_session(
    sessions_dir: &Path,
    session_id: &str,
    updates: &SessionData,
    provided_keys: Option<&Value>,
) -> Result<Option<PathBuf>> {
    find_and_update_active_session(
        sessions_dir,
        session_id,
        updates,
        Some("closed"),
        provided_keys,
    )
}

pub fn update_active_session(
    sessions_dir: &Path,
    session_id: &str,
    updates: &SessionData,
    provided_keys: Option<&Value>,
) -> Result<Option<PathBuf>> {
    find_and_update_active_session(sessions_dir, session_id, updates, None, provided_keys)
}

pub fn read_session_by_id(sessions_dir: &Path, session_id: &str) -> Result<Option<SessionData>> {
    if !sessions_dir.exists() {
        return Ok(None);
    }

    let mut matches: Vec<(SessionData, String)> = Vec::new();

    for entry in std::fs::read_dir(sessions_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".json") {
            continue;
        }
        let content = std::fs::read_to_string(entry.path())
            .with_context(|| format!("Failed to read session: {}", entry.path().display()))?;
        let data: SessionData = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse session: {}", entry.path().display()))?;

        let file_id = data.id.as_deref().unwrap_or("");
        let resolved_id = if file_id.is_empty() {
            synthesize_id_from_filename(&name)
        } else {
            file_id.to_string()
        };

        if ids_match(&resolved_id, session_id) {
            matches.push((data, resolved_id));
        }
    }

    if matches.len() > 1 {
        let candidates: Vec<&str> = matches.iter().map(|(_, id)| id.as_str()).collect();
        anyhow::bail!(
            "Ambiguous session_id '{}': matched {} sessions ({}). Provide a more specific ID.",
            session_id,
            matches.len(),
            candidates.join(", ")
        );
    }

    Ok(matches.into_iter().next().map(|(data, _)| data))
}

pub fn fork_session(
    sessions_dir: &Path,
    source: &SessionData,
    summary: &str,
    label: Option<&str>,
    timeline: Option<&str>,
    related_task_ids: Vec<String>,
    inherit: &[&str],
) -> Result<SessionData> {
    let source_id = source
        .id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Source session has no ID"))?;

    let mut forked = SessionData {
        version: source.version,
        id: Some(generate_session_id()),
        ended_at: None,
        summary: summary.to_string(),
        branch: None,
        commit: None,
        dirty_files: Vec::new(),
        decisions: Vec::new(),
        blockers: Vec::new(),
        checklist: Vec::new(),
        handoff_notes: Vec::new(),
        references: Vec::new(),
        context_pointers: Vec::new(),
        environment: None,
        timeline: timeline
            .map(String::from)
            .or_else(|| source.timeline.clone()),
        label: label.map(String::from),
        parent_session_id: Some(source_id.to_string()),
        related_task_ids,
    };

    for field in inherit {
        match *field {
            "decisions" => forked.decisions = source.decisions.clone(),
            "context_pointers" => forked.context_pointers = source.context_pointers.clone(),
            "references" => forked.references = source.references.clone(),
            "handoff_notes" => forked.handoff_notes = source.handoff_notes.clone(),
            "environment" => forked.environment = source.environment.clone(),
            "blockers" => forked.blockers = source.blockers.clone(),
            "checklist" => forked.checklist = source.checklist.clone(),
            _ => {}
        }
    }

    write_session_with_status(sessions_dir, &forked, "active")?;
    Ok(forked)
}

pub fn merge_sessions(
    sessions_dir: &Path,
    source_ids: &[&str],
    target_id: &str,
    close_sources: bool,
) -> Result<MergeResult> {
    let mut sources: Vec<SessionData> = Vec::new();
    for sid in source_ids {
        let session = read_session_by_id(sessions_dir, sid)?
            .ok_or_else(|| anyhow::anyhow!("Source session not found: {sid}"))?;
        sources.push(session);
    }

    let mut target = read_session_by_id(sessions_dir, target_id)?
        .ok_or_else(|| anyhow::anyhow!("Target session not found: {target_id}"))?;

    let target_actual_id = target.id.clone().unwrap_or_default();

    let mut merged_decisions = 0usize;
    let mut merged_notes = 0usize;
    let mut merged_references = 0usize;
    let mut merged_context_pointers = 0usize;
    let mut conflicts = Vec::new();
    let mut closed_sessions = Vec::new();

    for source in &sources {
        let source_actual_id = source.id.as_deref().unwrap_or("");
        if source_actual_id == target_actual_id {
            continue;
        }

        for d in &source.decisions {
            let decision_text = d.get("decision").and_then(|v| v.as_str()).unwrap_or("");
            let already_exists = target.decisions.iter().any(|td| {
                td.get("decision").and_then(|v| v.as_str()).unwrap_or("") == decision_text
            });
            if already_exists {
                conflicts.push(MergeConflict {
                    conflict_type: "decision_conflict".to_string(),
                    description: format!("Duplicate decision: {decision_text}"),
                    session_a: target_actual_id.clone(),
                    session_b: source_actual_id.to_string(),
                });
            } else {
                target.decisions.push(d.clone());
                merged_decisions += 1;
            }
        }

        for n in &source.handoff_notes {
            target.handoff_notes.push(n.clone());
            merged_notes += 1;
        }

        for r in &source.references {
            let label = r.get("label").and_then(|v| v.as_str()).unwrap_or("");
            let already_exists = target
                .references
                .iter()
                .any(|tr| tr.get("label").and_then(|v| v.as_str()).unwrap_or("") == label);
            if !already_exists {
                target.references.push(r.clone());
                merged_references += 1;
            }
        }

        for cp in &source.context_pointers {
            let path = cp.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let already_exists = target
                .context_pointers
                .iter()
                .any(|tcp| tcp.get("path").and_then(|v| v.as_str()).unwrap_or("") == path);
            if !already_exists {
                target.context_pointers.push(cp.clone());
                merged_context_pointers += 1;
            }
        }

        for task_id in &source.related_task_ids {
            if !target.related_task_ids.contains(task_id) {
                target.related_task_ids.push(task_id.clone());
            }
        }

        if close_sources {
            if let Some(path) = close_session_by_id(sessions_dir, source_actual_id)? {
                let _ = path;
                closed_sessions.push(source_actual_id.to_string());
            }
        }
    }

    update_session_in_place(sessions_dir, &target_actual_id, &target)?;

    Ok(MergeResult {
        merged_session_id: target_actual_id,
        merged_decisions,
        merged_notes,
        merged_references,
        merged_context_pointers,
        conflicts,
        closed_sessions,
    })
}

fn update_session_in_place(
    sessions_dir: &Path,
    session_id: &str,
    updated: &SessionData,
) -> Result<()> {
    if !sessions_dir.exists() {
        anyhow::bail!("Sessions directory not found");
    }

    let mut matches: Vec<(PathBuf, String)> = Vec::new();

    for entry in std::fs::read_dir(sessions_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".json") {
            continue;
        }
        let content = std::fs::read_to_string(entry.path())?;
        let data: SessionData = serde_json::from_str(&content)?;
        let file_id = data.id.as_deref().unwrap_or("");
        let resolved_id = if file_id.is_empty() {
            synthesize_id_from_filename(&name)
        } else {
            file_id.to_string()
        };
        if ids_match(&resolved_id, session_id) {
            matches.push((entry.path(), resolved_id));
        }
    }

    if matches.len() > 1 {
        let candidates: Vec<&str> = matches.iter().map(|(_, id)| id.as_str()).collect();
        anyhow::bail!(
            "Ambiguous session_id '{}': matched {} sessions ({}). Provide a more specific ID.",
            session_id,
            matches.len(),
            candidates.join(", ")
        );
    }

    if let Some((path, _)) = matches.into_iter().next() {
        let serialized =
            serde_json::to_string_pretty(updated).context("Failed to serialize session")?;
        crate::storage::atomic_write(path, serialized.as_bytes())?;
        return Ok(());
    }

    anyhow::bail!("Session not found for in-place update: {session_id}")
}

#[derive(Debug)]
pub struct MergeConflict {
    pub conflict_type: String,
    pub description: String,
    pub session_a: String,
    pub session_b: String,
}

#[derive(Debug)]
pub struct MergeResult {
    pub merged_session_id: String,
    pub merged_decisions: usize,
    pub merged_notes: usize,
    pub merged_references: usize,
    pub merged_context_pointers: usize,
    pub conflicts: Vec<MergeConflict>,
    pub closed_sessions: Vec<String>,
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
    crate::storage::atomic_write(&path, content.as_bytes())
        .with_context(|| format!("Failed to write session: {}", path.display()))?;

    Ok(path)
}
