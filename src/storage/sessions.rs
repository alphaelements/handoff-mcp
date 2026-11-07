use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// Identity of the agent that owns this session (t250.1, spec §4.1
    /// FR-2.1). Populated from `HandlerContext::agent_id` when the session
    /// is saved active; `None` for sessions predating this field or saved
    /// outside an agent-identified call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Ownership format version (v0.47.3). Present only on sessions whose
    /// `agent_id` was written by a build that keeps `agent_id` stable for the
    /// life of an MCP process. A session without it carries an `agent_id`
    /// from the older per-call generator, which says nothing about who owns
    /// the session, so it is treated as having no owner (see
    /// `session_target::effective_owner`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_v: Option<u32>,
    /// The resolved `project_dir` this session was saved from (the worktree
    /// checkout path, or the primary repo path for a non-worktree session).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    /// `"primary" | "worktree" | "ephemeral"`, auto-detected from the git
    /// worktree relationship of `worktree` — never accepted as a caller
    /// parameter. `None` for sessions predating this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Lineage format version (v0.47.3). Present only on sessions created by a
    /// build that records `continues`; its absence marks old data, which the
    /// lineage rules treat as one chronological lineage (see
    /// `storage::lineage`). Never added to an existing session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage_v: Option<u32>,
    /// Ids of the sessions this one continues. `Some(vec![])` (serialized as
    /// `[]`) is a deliberate new lineage origin; `None` is old data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continues: Option<Vec<String>>,
    /// `true` removes a closed session from the head set (written by hand;
    /// no tool sets it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage_end: Option<bool>,
    /// Which checkout the session was saved from, comparable across machines
    /// (`main` for the primary checkout, the `.git/worktrees/<name>` name for a
    /// linked worktree). The absolute `worktree` path differs between a Mac host
    /// and a container.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_key: Option<String>,
}

impl SessionData {
    /// The ids in `continues`, empty for old data.
    pub fn continues_ids(&self) -> &[String] {
        self.continues.as_deref().unwrap_or_default()
    }
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
    let mut slug = String::new();
    for c in summary.chars().take(40) {
        if matches!(c, '\u{0300}'..='\u{036F}' | '\u{3099}'..='\u{309A}') {
            // Combining marks (NFD voiced/accent marks) stay attached to the
            // preceding kept character; one with nothing to attach to is dropped.
            if slug.chars().last().is_some_and(|p| p != '-') {
                slug.push(c);
            }
        } else if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !c.is_ascii() && c.is_alphanumeric() {
            // Letters/digits of any script stay; non-ASCII symbols,
            // punctuation, spaces and emoji become separators.
            slug.push(c);
        } else {
            slug.push('-');
        }
    }
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

pub(crate) fn ids_match(candidate: &str, query: &str) -> bool {
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

/// Find the id of an active session tagged `scope: "primary"` in
/// `sessions_dir` (t250.1 — auto-parenting: a worktree session's
/// `parent_session_id` is set to whichever primary session is active when
/// the worktree session starts, or left `None` if the primary has not
/// started one yet). `sessions_dir` here is always the shared `.handoff/`
/// dir — the same physical directory a worktree session resolves to via
/// `resolve_handoff_dir` — so this is a plain scan, not a cross-worktree
/// lookup. When multiple primary-scope active sessions exist (unusual but
/// not impossible), the most recently started one wins.
pub fn find_active_primary_session_id(sessions_dir: &Path) -> Result<Option<String>> {
    let active = read_active_sessions(sessions_dir)?;
    Ok(active
        .into_iter()
        .filter(|s| s.scope.as_deref() == Some("primary"))
        .filter_map(|s| s.id)
        .next_back())
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

/// The newest closed session by [`session_age`] (the order pruning uses). A
/// session without `ended_at` is placed by the time in its id rather than
/// treated as the oldest.
pub fn read_latest_closed_session(sessions_dir: &Path) -> Result<Option<SessionData>> {
    let sessions = read_sessions_by_status(sessions_dir, "closed")?;
    Ok(sessions
        .into_iter()
        .max_by_key(|s| (session_age(s), s.id.clone())))
}

/// How many closed sessions [`read_recent_closed_sessions`] reads at most.
pub const RECENT_CLOSED_MAX: usize = 100;

/// The newest `limit` closed sessions, newest file first.
///
/// File names start with the `ended_at` second, so name order is age order to
/// within a second, and only the files that are read are parsed: with
/// retention unlimited (v0.47.3) the closed set grows without bound while
/// `load_context` only ever needs the recent lineage.
pub fn read_recent_closed_sessions(sessions_dir: &Path, limit: usize) -> Result<Vec<SessionData>> {
    if !sessions_dir.exists() {
        return Ok(Vec::new());
    }
    let mut names: Vec<String> = std::fs::read_dir(sessions_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".closed.json"))
        .collect();
    names.sort_unstable_by(|a, b| b.cmp(a));
    names.truncate(limit);
    names
        .into_iter()
        .map(|name| {
            let path = sessions_dir.join(&name);
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("Failed to read session: {}", path.display()))?;
            let mut data: SessionData = serde_json::from_str(&content)
                .with_context(|| format!("Failed to parse session: {}", path.display()))?;
            if data.id.is_none() {
                data.id = Some(synthesize_id_from_filename(&name));
            }
            Ok(data)
        })
        .collect()
}

/// When a session last ended: the parsed `ended_at`, else the time embedded in
/// its id (a session closed by id keeps whatever `ended_at` it had, possibly
/// none). `None` sorts as the oldest. Pruning and head detection share this so
/// "newest" means the same thing in both.
pub fn session_age(data: &SessionData) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    data.ended_at
        .as_deref()
        .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
        .or_else(|| data.id.as_deref().and_then(timestamp_from_id))
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
    // Auto-detected, not caller-provided (t250.1): only overwrite when the
    // update actually carries a value, so a `session_status` other than
    // "active" — which leaves these `None` in `handoff_updates` — doesn't
    // erase scope/agent_id/worktree/parent_session_id on close.
    //
    // Ownership travels as a unit and is only (re)written when the session
    // has no trustworthy owner or the writer already is the owner: an update
    // that reaches another agent's session (an explicit id, `force_other_owner`)
    // must not move it to the writer (FR-V0473-008 AC3).
    let may_claim = match crate::storage::session_target::effective_owner(data) {
        None => true,
        Some(owner) => updates.agent_id.as_deref() == Some(owner),
    };
    if may_claim {
        if updates.scope.is_some() {
            data.scope = updates.scope.clone();
        }
        if updates.agent_id.is_some() {
            data.agent_id = updates.agent_id.clone();
            data.owner_v = Some(crate::storage::session_target::OWNER_V);
        }
        if updates.worktree.is_some() {
            data.worktree = updates.worktree.clone();
        }
        if updates.parent_session_id.is_some() {
            data.parent_session_id = updates.parent_session_id.clone();
        }
    }
}

/// An unfinished (`active` / `open` / `paused`) session together with the
/// file it lives in. This is what the ownership rules in
/// [`crate::storage::session_target`] operate on: the status is not part of
/// [`SessionData`], it is the file-name suffix.
#[derive(Debug, Clone)]
pub struct SessionEntry {
    pub data: SessionData,
    pub status: String,
    pub path: PathBuf,
}

const UNFINISHED_STATUSES: [&str; 3] = ["active", "open", "paused"];

/// Every active, open and paused session, oldest file name first.
pub fn read_session_entries(sessions_dir: &Path) -> Result<Vec<SessionEntry>> {
    let mut entries = Vec::new();
    if !sessions_dir.exists() {
        return Ok(entries);
    }
    let mut files: Vec<_> = std::fs::read_dir(sessions_dir)?
        .filter_map(|e| e.ok())
        .collect();
    files.sort_by_key(|e| e.file_name());
    for file in files {
        let name = file.file_name().to_string_lossy().to_string();
        let Some(status) = UNFINISHED_STATUSES
            .iter()
            .find(|s| name.ends_with(&format!(".{s}.json")))
        else {
            continue;
        };
        let path = file.path();
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read session: {}", path.display()))?;
        let mut data: SessionData = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse session: {}", path.display()))?;
        if data.id.is_none() {
            data.id = Some(synthesize_id_from_filename(&name));
        }
        entries.push(SessionEntry {
            data,
            status: (*status).to_string(),
            path,
        });
    }
    Ok(entries)
}

/// Rename a session file to another status (`foo.active.json` ->
/// `foo.paused.json`) and return the new path.
pub fn transition_session_file(path: &Path, to: &str) -> Result<PathBuf> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("Invalid session path: {}", path.display()))?;
    let base = name
        .strip_suffix(".json")
        .and_then(|rest| rest.rsplit_once('.'))
        .map(|(base, _)| base)
        .ok_or_else(|| anyhow::anyhow!("Not a session file name: {name}"))?;
    let new_path = path.with_file_name(format!("{base}.{to}.json"));
    std::fs::rename(path, &new_path).with_context(|| {
        format!(
            "Failed to transition session file {} -> {}",
            path.display(),
            new_path.display()
        )
    })?;
    Ok(new_path)
}

/// Apply `updates` to the session stored at `path` and optionally move it to
/// another status, returning the final path. Works on any status, so a
/// `close_session_id` of a paused session records its summary too.
pub fn update_session_at(
    path: &Path,
    updates: &SessionData,
    provided_keys: Option<&Value>,
    transition_to: Option<&str>,
) -> Result<PathBuf> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read session: {}", path.display()))?;
    let mut data: SessionData = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse session: {}", path.display()))?;
    apply_session_updates(&mut data, updates, provided_keys);
    let updated = serde_json::to_string_pretty(&data).context("Failed to serialize session")?;
    crate::storage::atomic_write(path, updated.as_bytes())
        .with_context(|| format!("Failed to write session: {}", path.display()))?;
    match transition_to {
        Some(status) => {
            let ts_part = compact_timestamp(&data);
            let base = generate_session_filename(&data.summary, &ts_part);
            let new_path = path.with_file_name(format!("{base}.{status}.json"));
            std::fs::rename(path, &new_path).with_context(|| {
                format!(
                    "Failed to transition session to {status}: {}",
                    path.display()
                )
            })?;
            Ok(new_path)
        }
        None => Ok(path.to_path_buf()),
    }
}

/// Who a session is being handed to by [`adopt_session`].
#[derive(Debug, Clone, Copy)]
pub struct OwnerStamp<'a> {
    pub agent_id: &'a str,
    pub worktree: &'a str,
    pub scope: &'a str,
}

/// What a session looked like before [`adopt_session`] rewrote it.
#[derive(Debug, Clone)]
pub struct AdoptOutcome {
    pub path: PathBuf,
    pub data: SessionData,
    pub previous_owner: Option<String>,
    pub previous_worktree: Option<String>,
    pub previous_scope: Option<String>,
}

/// Hand an unfinished session to `stamp` and put it in `to_status`.
///
/// `expected_owner` is the owner the caller saw when it decided to adopt. The
/// comparison and the rewrite happen under one exclusive file lock, so of two
/// processes adopting the same session at once exactly one wins and the other
/// gets an error naming the new owner instead of silently overwriting it
/// (spec §4.4' SHOULD-FIX 12). The body's `agent_id`, `worktree` and `scope`
/// are replaced by the new owner's; the old values are returned for the
/// event detail.
pub fn adopt_session(
    sessions_dir: &Path,
    session_id: &str,
    expected_owner: Option<&str>,
    stamp: &OwnerStamp<'_>,
    to_status: &str,
) -> Result<AdoptOutcome> {
    use fs2::FileExt;

    let handoff = sessions_dir.parent().unwrap_or(sessions_dir);
    let lock_dir = handoff.join("agents");
    std::fs::create_dir_all(&lock_dir)
        .with_context(|| format!("Failed to create lock dir: {}", lock_dir.display()))?;
    crate::storage::gitignore::ensure_gitignore_entries(
        handoff,
        &[
            crate::storage::gitignore::AGENTS_DIR,
            crate::storage::gitignore::LOCK_FILES,
        ],
    )?;
    let lock_path = lock_dir.join("_sessions.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("Failed to open lock file: {}", lock_path.display()))?;
    lock.lock_exclusive()
        .context("Failed to lock sessions for adopt")?;
    let result = adopt_locked(sessions_dir, session_id, expected_owner, stamp, to_status);
    let _ = FileExt::unlock(&lock);
    result
}

fn adopt_locked(
    sessions_dir: &Path,
    session_id: &str,
    expected_owner: Option<&str>,
    stamp: &OwnerStamp<'_>,
    to_status: &str,
) -> Result<AdoptOutcome> {
    let entries = read_session_entries(sessions_dir)?;
    let entry = entries
        .iter()
        .find(|e| e.data.id.as_deref() == Some(session_id))
        .ok_or_else(|| anyhow::anyhow!("Session '{session_id}' is no longer available"))?;
    let current_owner = crate::storage::session_target::effective_owner(&entry.data);
    if current_owner != expected_owner {
        anyhow::bail!(
            "Session '{session_id}' changed owner while it was being adopted (now owned by {}). \
             Reload and try again.",
            current_owner.unwrap_or("nobody")
        );
    }
    let previous_owner = current_owner.map(String::from);
    let previous_worktree = entry.data.worktree.clone();
    let previous_scope = entry.data.scope.clone();

    let mut data = entry.data.clone();
    data.agent_id = Some(stamp.agent_id.to_string());
    data.owner_v = Some(crate::storage::session_target::OWNER_V);
    data.worktree = Some(stamp.worktree.to_string());
    data.scope = Some(stamp.scope.to_string());
    let content = serde_json::to_string_pretty(&data).context("Failed to serialize session")?;
    crate::storage::atomic_write(&entry.path, content.as_bytes())
        .with_context(|| format!("Failed to write session: {}", entry.path.display()))?;
    let path = if entry.status == to_status {
        entry.path.clone()
    } else {
        transition_session_file(&entry.path, to_status)?
    };
    Ok(AdoptOutcome {
        path,
        data,
        previous_owner,
        previous_worktree,
        previous_scope,
    })
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

/// The caller a forked session is created for.
#[derive(Debug, Clone, Copy)]
pub struct ForkOwner<'a> {
    pub agent_id: Option<&'a str>,
    pub worktree: &'a str,
    pub scope: &'a str,
}

/// What the forked session says about itself.
#[derive(Debug, Clone)]
pub struct ForkSpec<'a> {
    pub summary: &'a str,
    pub label: Option<&'a str>,
    pub timeline: Option<&'a str>,
    pub related_task_ids: Vec<String>,
    /// Source fields the fork copies (`decisions`, `context_pointers`, ...).
    pub inherit: &'a [&'a str],
}

pub fn fork_session(
    sessions_dir: &Path,
    source: &SessionData,
    spec: ForkSpec<'_>,
    owner: &ForkOwner<'_>,
) -> Result<SessionData> {
    let ForkSpec {
        summary,
        label,
        timeline,
        related_task_ids,
        inherit,
    } = spec;
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
        // The fork belongs to whoever forked it, not to the source's owner
        // (FR-V0473-008 AC3).
        agent_id: owner.agent_id.map(String::from),
        owner_v: owner
            .agent_id
            .map(|_| crate::storage::session_target::OWNER_V),
        worktree: Some(owner.worktree.to_string()),
        scope: Some(owner.scope.to_string()),
        // A fork continues the session it was forked from.
        lineage_v: Some(crate::storage::lineage::LINEAGE_V),
        continues: Some(vec![source_id.to_string()]),
        lineage_end: None,
        worktree_key: crate::storage::worktree_key(Path::new(owner.worktree)),
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
    caller_agent_id: Option<&str>,
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

    // The target now continues every other source. Old data gets `lineage_v`
    // with the first appended id, because `continues` alone is ignored without it.
    let appended: Vec<String> = sources
        .iter()
        .filter_map(|s| s.id.clone())
        .filter(|id| *id != target_actual_id)
        .collect();
    if !appended.is_empty() {
        let continues = target.continues.get_or_insert_with(Vec::new);
        for id in appended {
            if !continues.contains(&id) {
                continues.push(id);
            }
        }
        target
            .lineage_v
            .get_or_insert(crate::storage::lineage::LINEAGE_V);
    }

    // The merge target is an existing session, not a new one: its owner is
    // only ever filled in, never replaced (FR-V0473-008 AC3).
    if crate::storage::session_target::effective_owner(&target).is_none() {
        if let Some(agent) = caller_agent_id {
            target.agent_id = Some(agent.to_string());
            target.owner_v = Some(crate::storage::session_target::OWNER_V);
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

/// One closed session removed by [`prune_closed_sessions`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrunedSession {
    /// The session id from the file body, or the id synthesized from the
    /// file name for old files that carry none.
    pub id: String,
    pub file_name: String,
}

#[derive(Debug, Default)]
pub struct PruneOutcome {
    pub removed: Vec<PrunedSession>,
    /// Closed sessions still on disk above `limit` solely because they are
    /// heads (see [`prune_closed_sessions`]).
    pub retained_for_head: u32,
}

/// A closed session file with the data the prune order and head choice need.
struct ClosedEntry {
    path: PathBuf,
    file_name: String,
    id: String,
    ended_at: Option<chrono::DateTime<chrono::FixedOffset>>,
}

/// The UTC instant embedded in a session id (`s-YYYYMMDD-HHMMSS-uuuuuu`, or
/// the `-000000` form synthesized from an old file name).
pub(crate) fn timestamp_from_id(id: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    let rest = id.strip_prefix("s-")?;
    let naive = chrono::NaiveDateTime::parse_from_str(rest.get(..22)?, "%Y%m%d-%H%M%S-%6f").ok()?;
    Some(naive.and_utc().fixed_offset())
}

/// Every `*.closed.json`, oldest first.
///
/// Age is the parsed `ended_at` (the same notion of "newest" as
/// [`read_latest_closed_session`]), with the file name as tie-breaker. The
/// file name alone is not an age order: it carries the `ended_at` second
/// followed by the summary slug, so two saves within one second would sort by
/// slug. When `ended_at` is absent or unreadable the timestamp in the session
/// id stands in; a file with neither sorts as the oldest.
fn closed_entries_oldest_first(sessions_dir: &Path) -> Result<Vec<ClosedEntry>> {
    let mut entries: Vec<ClosedEntry> = Vec::new();
    for entry in std::fs::read_dir(sessions_dir)? {
        let entry = entry?;
        let file_name = entry.file_name().to_string_lossy().to_string();
        if !file_name.ends_with(".closed.json") {
            continue;
        }
        let path = entry.path();
        // An unreadable or unparseable file is still a closed session that
        // counts toward the limit; it just has no id or timestamp of its own.
        let parsed: Option<SessionData> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|c| serde_json::from_str(&c).ok());
        let (id, ended_at) = match parsed {
            Some(mut d) => {
                let id =
                    d.id.get_or_insert_with(|| synthesize_id_from_filename(&file_name))
                        .clone();
                (id, session_age(&d))
            }
            None => (synthesize_id_from_filename(&file_name), None),
        };
        entries.push(ClosedEntry {
            path,
            file_name,
            id,
            ended_at,
        });
    }
    entries.sort_by(|a, b| {
        a.ended_at
            .cmp(&b.ended_at)
            .then_with(|| a.file_name.cmp(&b.file_name))
    });
    Ok(entries)
}

/// The ids of the closed sessions that are lineage heads (see
/// [`crate::storage::lineage`]), over every closed session on disk. Callers
/// hand the result to [`prune_closed_sessions`], which never removes them.
pub fn closed_head_ids(sessions_dir: &Path) -> Result<HashSet<String>> {
    if !sessions_dir.exists() {
        return Ok(HashSet::new());
    }
    // Like the prune loop, an unparseable file is skipped rather than failing
    // the save that triggered pruning: it can only make a head go unnoticed
    // when the file itself was that head, and a file that cannot be read is not
    // one `load_context` could return either.
    let mut closed = Vec::new();
    for entry in std::fs::read_dir(sessions_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".closed.json") {
            continue;
        }
        let parsed: Option<SessionData> = std::fs::read_to_string(entry.path())
            .ok()
            .and_then(|c| serde_json::from_str(&c).ok());
        if let Some(mut data) = parsed {
            data.id
                .get_or_insert_with(|| synthesize_id_from_filename(&name));
            closed.push(data);
        }
    }
    Ok(crate::storage::lineage::head_ids(&closed))
}

/// Prune the oldest closed sessions down to `limit`, never removing a head.
///
/// `limit == 0` means "no limit" and removes nothing: a zero used to wipe
/// every closed session. Sessions whose id is in `heads` are skipped, so the
/// result can exceed `limit`; the overshoot is reported in
/// [`PruneOutcome::retained_for_head`]. Only closed sessions are considered.
pub fn prune_closed_sessions(
    sessions_dir: &Path,
    limit: u32,
    heads: &HashSet<String>,
) -> Result<PruneOutcome> {
    let mut outcome = PruneOutcome::default();
    if limit == 0 || !sessions_dir.exists() {
        return Ok(outcome);
    }

    let entries = closed_entries_oldest_first(sessions_dir)?;
    let mut remaining = entries.len();
    for entry in &entries {
        if remaining <= limit as usize {
            break;
        }
        if heads.contains(&entry.id) {
            continue;
        }
        std::fs::remove_file(&entry.path)
            .with_context(|| format!("Failed to remove old session: {}", entry.path.display()))?;
        outcome.removed.push(PrunedSession {
            id: entry.id.clone(),
            file_name: entry.file_name.clone(),
        });
        remaining -= 1;
    }
    outcome.retained_for_head = remaining.saturating_sub(limit as usize) as u32;

    Ok(outcome)
}

/// Count-only prune: delete the oldest closed sessions beyond `limit`,
/// head-agnostic, and return how many were removed. `limit == 0` removes
/// nothing. Head-aware pruning with events is [`prune_closed_sessions`].
pub fn enforce_history_limit(sessions_dir: &Path, limit: u32) -> Result<u32> {
    Ok(prune_closed_sessions(sessions_dir, limit, &HashSet::new())?
        .removed
        .len() as u32)
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
