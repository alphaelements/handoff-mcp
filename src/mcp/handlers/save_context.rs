use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use serde_json::Value;

use super::resolve_project_dir;
use crate::storage::config::read_config;
use crate::storage::ensure_handoff_exists;
use crate::storage::git::capture_git_state;
use crate::storage::sessions::{
    close_active_sessions, close_open_sessions, close_session_by_id, enforce_history_limit,
    generate_session_id, pause_active_sessions, pause_session_by_id, read_active_sessions,
    write_open_session, SessionData,
};

pub fn handle(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;

    let handoff = ensure_handoff_exists(&project_dir)?;
    let sessions_dir = handoff.join("sessions");
    let config_path = handoff.join("config.toml");

    let summary = arguments
        .get("summary")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'summary' is required"))?;

    let close_id = arguments.get("close_session_id").and_then(|v| v.as_str());
    let pause_id = arguments.get("pause_session_id").and_then(|v| v.as_str());
    let pause_all = arguments
        .get("pause_active")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut total_paused = 0usize;
    if let Some(id) = pause_id {
        if pause_session_by_id(&sessions_dir, id)?.is_some() {
            total_paused = 1;
        }
    } else if pause_all {
        total_paused = pause_active_sessions(&sessions_dir)?.len();
    }

    let total_closed = if let Some(id) = close_id {
        let closed = close_session_by_id(&sessions_dir, id)?;
        if closed.is_some() {
            1
        } else {
            0
        }
    } else if pause_id.is_some() || pause_all {
        let closed_open = close_open_sessions(&sessions_dir)?;
        closed_open.len()
    } else {
        let active = read_active_sessions(&sessions_dir)?;
        if active.len() > 1 {
            let active_ids: Vec<String> = active.iter().filter_map(|s| s.id.clone()).collect();
            anyhow::bail!(
                "Multiple active sessions found ({}).\n\
                 Use close_session_id or pause_session_id to specify which to \
                 close/pause before saving a new session.",
                active_ids.join(", ")
            );
        }
        let closed_active = close_active_sessions(&sessions_dir)?;
        let closed_open = close_open_sessions(&sessions_dir)?;
        closed_active.len() + closed_open.len()
    };

    let git_state = capture_git_state(&project_dir)?;
    let now = Utc::now().to_rfc3339();
    let session_id = generate_session_id();

    let data = SessionData {
        version: 2,
        id: Some(session_id.clone()),
        ended_at: Some(now),
        summary: summary.to_string(),
        branch: Some(git_state.branch),
        commit: Some(git_state.commit),
        dirty_files: git_state.dirty_files,
        decisions: extract_array(arguments, "decisions"),
        blockers: extract_string_array(arguments, "blockers"),
        checklist: extract_array(arguments, "checklist"),
        handoff_notes: extract_array(arguments, "handoff_notes"),
        references: extract_array(arguments, "references"),
        context_pointers: extract_array(arguments, "context_pointers"),
        environment: arguments.get("environment").cloned(),
    };

    let path = write_open_session(&sessions_dir, &data)?;

    let history_limit = if config_path.exists() {
        read_config(&config_path)
            .map(|c| c.settings.history_limit)
            .unwrap_or(20)
    } else {
        20
    };
    let removed = enforce_history_limit(&sessions_dir, history_limit)?;

    let mut msg = format!(
        "Session saved: {}\nSession ID: {}\nFile: {}",
        summary,
        session_id,
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    );

    if total_paused > 0 {
        msg.push_str(&format!("\nPaused {} session(s)", total_paused));
    }
    if let Some(id) = pause_id {
        if total_paused == 0 {
            msg.push_str(&format!(
                "\nWarning: pause_session_id '{id}' not found among active sessions"
            ));
        }
    }
    if total_closed > 0 {
        msg.push_str(&format!("\nClosed {} previous session(s)", total_closed));
    }
    if let Some(id) = close_id {
        if total_closed == 0 {
            msg.push_str(&format!(
                "\nWarning: close_session_id '{id}' not found among active/open/paused sessions"
            ));
        }
    }
    if removed > 0 {
        msg.push_str(&format!(
            "\nRemoved {removed} old session(s) (history_limit: {history_limit})"
        ));
    }

    for w in collect_save_warnings(&data, &project_dir) {
        msg.push_str(&format!("\n{w}"));
    }

    Ok(msg)
}

fn collect_save_warnings(data: &SessionData, project_dir: &Path) -> Vec<String> {
    let mut warnings = Vec::new();

    if data.checklist.is_empty() {
        warnings.push(
            "Warning: No checklist items. Consider adding verification items for the next session."
                .to_string(),
        );
    } else {
        let unchecked: Vec<&str> = data
            .checklist
            .iter()
            .filter_map(|item| {
                let checked = item
                    .get("checked")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if !checked {
                    item.get("item").and_then(|v| v.as_str())
                } else {
                    None
                }
            })
            .collect();

        if !unchecked.is_empty() {
            warnings.push(format!(
                "Warning: {} unchecked checklist item(s) \u{2014} {}",
                unchecked.len(),
                unchecked.join(", ")
            ));
        }
    }

    let has_suggestion = data.handoff_notes.iter().any(|note| {
        note.get("category")
            .and_then(|v| v.as_str())
            .is_some_and(|c| c == "suggestion")
    });
    if !has_suggestion {
        warnings.push(
            "Warning: No 'suggestion' handoff_notes \u{2014} the next session won't know what to \
             do first. Add at least one note with category 'suggestion' describing the recommended \
             next action."
                .to_string(),
        );
    }

    if data.context_pointers.is_empty() {
        warnings.push(
            "Warning: No context_pointers. The next session won't know which files to read first."
                .to_string(),
        );
    }

    if data.decisions.is_empty() {
        warnings.push(
            "Warning: No decisions recorded. Consider documenting key decisions made during this session."
                .to_string(),
        );
    }

    if data.references.is_empty() {
        warnings.push(
            "Warning: No references. Consider adding links to relevant docs, issues, or MRs."
                .to_string(),
        );
    } else {
        for r in &data.references {
            let uri = r.get("uri").and_then(|v| v.as_str()).unwrap_or("");
            if uri.is_empty() {
                continue;
            }
            if uri.starts_with("http://") || uri.starts_with("https://") {
                continue;
            }
            if uri.starts_with("ref-") {
                continue;
            }
            let resolved = if Path::new(uri).is_absolute() {
                std::path::PathBuf::from(uri)
            } else {
                project_dir.join(uri)
            };
            let check_path = resolved
                .to_string_lossy()
                .split('#')
                .next()
                .unwrap_or("")
                .to_string();
            if !Path::new(&check_path).exists() {
                let label = r.get("label").and_then(|v| v.as_str()).unwrap_or("");
                warnings.push(format!(
                    "Warning: reference '{label}' path does not exist: {uri}"
                ));
            }
        }
    }

    for cp in &data.context_pointers {
        let p = cp.get("path").and_then(|v| v.as_str()).unwrap_or("");
        if p.is_empty() {
            continue;
        }
        let resolved = if Path::new(p).is_absolute() {
            std::path::PathBuf::from(p)
        } else {
            project_dir.join(p)
        };
        if !resolved.exists() {
            warnings.push(format!("Warning: context_pointer path does not exist: {p}"));
        }
    }

    warnings
}

fn extract_array(val: &Value, key: &str) -> Vec<Value> {
    val.get(key)
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

fn extract_string_array(val: &Value, key: &str) -> Vec<String> {
    val.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}
