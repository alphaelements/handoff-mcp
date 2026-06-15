use anyhow::{Context, Result};
use serde_json::Value;

use super::resolve_project_dir;
use crate::storage::config::read_config;
use crate::storage::referrals::read_referral_summaries;
use crate::storage::sessions::{activate_open_sessions, read_active_sessions, read_open_sessions};
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

    let sessions = read_open_sessions(&sessions_dir)?;
    activate_open_sessions(&sessions_dir)?;
    let _active = read_active_sessions(&sessions_dir)?;

    let (task_tree, task_summary) = build_task_index(&tasks_dir, config.settings.done_task_limit)?;

    let last_session = sessions.last().map(|s| {
        serde_json::json!({
            "ended_at": s.ended_at,
            "summary": s.summary,
            "branch": s.branch,
            "commit": s.commit,
        })
    });

    let active_sessions: Vec<Value> = sessions
        .iter()
        .map(|s| serde_json::to_value(s).unwrap_or_default())
        .collect();

    let mut result = serde_json::json!({
        "project": config.project.name,
        "task_tree": task_tree,
        "task_summary": task_summary,
    });

    if let Some(ls) = last_session {
        result["last_session"] = ls;
    }

    if !active_sessions.is_empty() {
        let latest = &active_sessions[active_sessions.len() - 1];

        for key in [
            "decisions",
            "blockers",
            "checklist",
            "handoff_notes",
            "references",
            "context_pointers",
        ] {
            if let Some(val) = latest.get(key) {
                if val.as_array().is_some_and(|a| !a.is_empty()) {
                    result[key] = val.clone();
                }
            }
        }

        if let Some(env) = latest.get("environment") {
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

    serde_json::to_string_pretty(&result).context("Failed to serialize context")
}
