use anyhow::{Context, Result};
use serde_json::Value;

use super::resolve_project_dir;
use crate::storage::config::read_config;
use crate::storage::referrals::read_referral_summaries;
use crate::storage::sessions::{
    activate_open_sessions, activate_session_by_id, read_active_sessions, read_open_sessions,
    read_paused_sessions, resume_paused_session_by_id,
};
use crate::storage::tasks::build_task_index;
use crate::storage::{ensure_handoff_exists, handoff_dir};

pub fn handle(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;

    let hdir = handoff_dir(&project_dir);
    if !hdir.exists() {
        let result = serde_json::json!({
            "status": "not_initialized",
            "message": format!(
                "No .handoff/ directory found in {}. Run handoff_init to set up handoff tracking.",
                project_dir.display()
            )
        });
        return serde_json::to_string_pretty(&result).context("serialize");
    }

    let handoff = ensure_handoff_exists(&project_dir)?;
    let sessions_dir = handoff.join("sessions");
    let tasks_dir = handoff.join("tasks");
    let config_path = handoff.join("config.toml");

    let config = if config_path.exists() {
        read_config(&config_path)?
    } else {
        anyhow::bail!("config.toml not found");
    };

    let target_session_id = arguments.get("session_id").and_then(|v| v.as_str());

    let active_sessions = read_active_sessions(&sessions_dir)?;
    let sessions = read_open_sessions(&sessions_dir)?;
    let paused_sessions = read_paused_sessions(&sessions_dir)?;

    let selected_session = if let Some(sid) = target_session_id {
        let already_active = active_sessions
            .iter()
            .any(|s| s.id.as_deref().is_some_and(|id| id == sid));
        if already_active {
            active_sessions
                .into_iter()
                .find(|s| s.id.as_deref().is_some_and(|id| id == sid))
        } else if !active_sessions.is_empty() {
            let active_ids: Vec<String> = active_sessions
                .iter()
                .filter_map(|s| s.id.clone())
                .collect();
            anyhow::bail!(
                "Cannot activate session '{sid}': another session is already active ({}).\n\
                 Use save_context with close_session_id or pause_session_id to \
                 close/pause the active session first.",
                active_ids.join(", ")
            );
        } else if activate_session_by_id(&sessions_dir, sid)?.is_some() {
            sessions
                .into_iter()
                .find(|s| s.id.as_deref().is_some_and(|id| id == sid))
        } else if resume_paused_session_by_id(&sessions_dir, sid)?.is_some() {
            paused_sessions
                .into_iter()
                .find(|s| s.id.as_deref().is_some_and(|id| id == sid))
        } else {
            None
        }
    } else if !active_sessions.is_empty() {
        active_sessions.into_iter().last()
    } else if sessions.len() > 1 {
        let open_ids: Vec<String> = sessions.iter().filter_map(|s| s.id.clone()).collect();
        anyhow::bail!(
            "Multiple open sessions found ({}).\n\
             Specify session_id to choose one, or use save_context to \
             close/pause the others first.",
            open_ids.join(", ")
        );
    } else {
        activate_open_sessions(&sessions_dir)?;
        sessions.into_iter().last()
    };

    let (task_tree, task_summary) = build_task_index(&tasks_dir, config.settings.done_task_limit)?;

    let mut result = serde_json::json!({
        "project": config.project.name,
        "task_tree": task_tree,
        "task_summary": task_summary,
    });

    if selected_session.is_none() {
        if let Some(sid) = target_session_id {
            result["warning"] =
                serde_json::json!(format!("session_id '{sid}' not found among open sessions"));
        }
    }

    if let Some(ref session) = selected_session {
        result["last_session"] = serde_json::json!({
            "ended_at": session.ended_at,
            "summary": session.summary,
            "branch": session.branch,
            "commit": session.commit,
        });

        if let Some(ref id) = session.id {
            result["session_id"] = serde_json::json!(id);
        }

        let session_val = serde_json::to_value(session).unwrap_or_default();

        for key in [
            "decisions",
            "blockers",
            "checklist",
            "handoff_notes",
            "references",
            "context_pointers",
        ] {
            if let Some(val) = session_val.get(key) {
                if val.as_array().is_some_and(|a| !a.is_empty()) {
                    result[key] = val.clone();
                }
            }
        }

        if let Some(env) = session_val.get("environment") {
            if !env.is_null() {
                result["environment"] = env.clone();
            }
        }
    }

    if let Some(notes) = result.get("handoff_notes").and_then(|v| v.as_array()) {
        let suggestions: Vec<&str> = notes
            .iter()
            .filter(|n| {
                n.get("category")
                    .and_then(|c| c.as_str())
                    .is_some_and(|c| c == "suggestion")
            })
            .filter_map(|n| n.get("note").and_then(|v| v.as_str()))
            .collect();
        if !suggestions.is_empty() {
            result["next_actions"] = serde_json::json!(suggestions);
        }
    }

    if !config.settings.context_files.is_empty() {
        result["suggested_reads"] = serde_json::to_value(&config.settings.context_files)?;
    }

    let referrals_dir = handoff.join("referrals");
    let open_referrals = read_referral_summaries(&referrals_dir, Some("open"))?;
    if !open_referrals.is_empty() {
        result["referrals"] = serde_json::to_value(&open_referrals)?;
    }

    let current_paused = read_paused_sessions(&sessions_dir)?;
    if !current_paused.is_empty() {
        let summaries: Vec<Value> = current_paused
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id,
                    "summary": s.summary,
                    "ended_at": s.ended_at,
                    "branch": s.branch,
                })
            })
            .collect();
        result["paused_sessions"] = serde_json::json!(summaries);
    }

    serde_json::to_string_pretty(&result).context("Failed to serialize context")
}
