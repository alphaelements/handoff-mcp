use anyhow::{Context, Result};
use serde_json::Value;

use super::resolve_project_dir;
use crate::storage::config::read_config;
use crate::storage::referrals::read_referral_summaries;
use crate::storage::sessions::{
    activate_open_sessions, activate_session_by_id, read_active_sessions,
    read_latest_closed_session, read_open_sessions, read_paused_sessions,
    resume_paused_session_by_id,
};
use crate::storage::tasks::{build_task_index, TaskIndex};
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

    let multi_session = config.settings.multi_session;

    let selected_session = if let Some(sid) = target_session_id {
        let already_active = active_sessions
            .iter()
            .any(|s| s.id.as_deref().is_some_and(|id| id == sid));
        if already_active {
            active_sessions
                .into_iter()
                .find(|s| s.id.as_deref().is_some_and(|id| id == sid))
        } else if !multi_session && !active_sessions.is_empty() {
            let active_ids: Vec<String> = active_sessions
                .iter()
                .filter_map(|s| s.id.clone())
                .collect();
            anyhow::bail!(
                "Cannot activate session '{sid}': another session is already active ({}).\n\
                 Use save_context with close_session_id or pause_session_id to \
                 close/pause the active session first, or enable multi_session in config.",
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

    if let Some(prev) = read_latest_closed_session(&sessions_dir)? {
        let prev_val = serde_json::to_value(&prev).unwrap_or_default();
        let mut prev_obj = serde_json::json!({
            "summary": prev.summary,
            "ended_at": prev.ended_at,
            "branch": prev.branch,
            "commit": prev.commit,
        });
        if let Some(ref id) = prev.id {
            prev_obj["id"] = serde_json::json!(id);
        }
        for key in [
            "decisions",
            "handoff_notes",
            "context_pointers",
            "checklist",
            "references",
            "blockers",
        ] {
            if let Some(val) = prev_val.get(key) {
                if val.as_array().is_some_and(|a| !a.is_empty()) {
                    prev_obj[key] = val.clone();
                }
            }
        }
        if let Some(env) = prev_val.get("environment") {
            if !env.is_null() {
                prev_obj["environment"] = env.clone();
            }
        }
        result["previous_session"] = prev_obj;
    }

    let notes_sources: Vec<&Value> = [
        result.get("handoff_notes"),
        result
            .get("previous_session")
            .and_then(|ps| ps.get("handoff_notes")),
    ]
    .into_iter()
    .flatten()
    .collect();

    let suggestions: Vec<&str> = notes_sources
        .iter()
        .filter_map(|v| v.as_array())
        .flatten()
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

    if !config.settings.context_files.is_empty() {
        result["suggested_reads"] = serde_json::to_value(&config.settings.context_files)?;
    }

    let referrals_dir = handoff.join("referrals");
    let open_referrals = read_referral_summaries(&referrals_dir, Some("open"))?;
    if !open_referrals.is_empty() {
        result["referrals"] = serde_json::to_value(&open_referrals)?;
    }

    let current_open = read_open_sessions(&sessions_dir)?;
    if !current_open.is_empty() {
        let summaries: Vec<Value> = current_open.iter().map(session_summary_json).collect();
        result["open_sessions"] = serde_json::json!(summaries);
    }

    let current_paused = read_paused_sessions(&sessions_dir)?;
    if !current_paused.is_empty() {
        let summaries: Vec<Value> = current_paused.iter().map(session_summary_json).collect();
        result["paused_sessions"] = serde_json::json!(summaries);
    }

    let current_active = read_active_sessions(&sessions_dir)?;
    if current_active.len() > 1 {
        let summaries: Vec<Value> = current_active.iter().map(session_summary_json).collect();
        result["active_sessions"] = serde_json::json!(summaries);
    }

    if current_active.is_empty() {
        let mut guidance = serde_json::json!({
            "action": "create_session",
            "message": "No active session. Before starting work, call handoff_save_context with session_status='active' to establish a session. Include inherited context (decisions, context_pointers, references) from the previous session so your work survives interruptions."
        });
        if let Some(prev) = result.get("previous_session") {
            let mut suggested = serde_json::json!({});
            if let Some(summary) = prev.get("summary").and_then(|v| v.as_str()) {
                suggested["summary"] = serde_json::json!(format!("Continuing: {summary}"));
            }
            for key in ["decisions", "context_pointers", "references"] {
                if let Some(val) = prev.get(key) {
                    if val.as_array().is_some_and(|a| !a.is_empty()) {
                        suggested[key] = val.clone();
                    }
                }
            }
            if let Some(task_ids) = collect_active_task_ids(&task_tree) {
                suggested["related_task_ids"] = serde_json::json!(task_ids);
            }
            guidance["suggested_fields"] = suggested;
        }
        result["session_guidance"] = guidance;
    } else if current_active.len() > 1 && target_session_id.is_none() {
        let summaries: Vec<Value> = current_active.iter().map(session_summary_json).collect();
        result["session_guidance"] = serde_json::json!({
            "action": "select_session",
            "message": "Multiple active sessions. Use session_id to specify which to work with, or create a new session.",
            "active_sessions": summaries
        });
    }

    serde_json::to_string_pretty(&result).context("Failed to serialize context")
}

fn session_summary_json(s: &crate::storage::sessions::SessionData) -> Value {
    let mut obj = serde_json::json!({
        "id": s.id,
        "summary": s.summary,
        "ended_at": s.ended_at,
        "branch": s.branch,
    });
    if let Some(ref label) = s.label {
        obj["label"] = serde_json::json!(label);
    }
    if let Some(ref timeline) = s.timeline {
        obj["timeline"] = serde_json::json!(timeline);
    }
    obj
}

fn collect_active_task_ids(task_tree: &[TaskIndex]) -> Option<Vec<String>> {
    let mut ids = Vec::new();
    collect_active_ids_recursive(task_tree, &mut ids);
    if ids.is_empty() {
        None
    } else {
        Some(ids)
    }
}

fn collect_active_ids_recursive(tasks: &[TaskIndex], ids: &mut Vec<String>) {
    for task in tasks {
        if matches!(
            task.status.as_str(),
            "in_progress" | "blocked" | "todo" | "review"
        ) {
            ids.push(task.id.clone());
        }
        collect_active_ids_recursive(&task.children, ids);
    }
}
