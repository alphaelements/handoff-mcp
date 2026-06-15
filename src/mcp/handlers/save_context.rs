use anyhow::Result;
use chrono::Utc;
use serde_json::Value;

use super::resolve_project_dir;
use crate::storage::config::read_config;
use crate::storage::ensure_handoff_exists;
use crate::storage::git::capture_git_state;
use crate::storage::sessions::{
    close_active_sessions, close_open_sessions, enforce_history_limit, write_open_session,
    SessionData,
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

    let closed_active = close_active_sessions(&sessions_dir)?;
    let closed_open = close_open_sessions(&sessions_dir)?;

    let git_state = capture_git_state(&project_dir)?;
    let now = Utc::now().to_rfc3339();

    let data = SessionData {
        version: 2,
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
        "Session saved: {}\nFile: {}",
        summary,
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    );

    let total_closed = closed_active.len() + closed_open.len();
    if total_closed > 0 {
        msg.push_str(&format!("\nClosed {} previous session(s)", total_closed));
    }
    if removed > 0 {
        msg.push_str(&format!(
            "\nRemoved {removed} old session(s) (history_limit: {history_limit})"
        ));
    }

    for w in collect_save_warnings(&data) {
        msg.push_str(&format!("\n{w}"));
    }

    Ok(msg)
}

fn collect_save_warnings(data: &SessionData) -> Vec<String> {
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
