use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::storage::config::read_config;
use crate::storage::expand_tilde;
use crate::storage::referrals::read_referral_summaries;
use crate::storage::sessions::read_active_sessions;
use crate::storage::tasks::build_task_index;

pub fn handle(arguments: &Value) -> Result<String> {
    let scan_dirs: Vec<String> = arguments
        .get("scan_dirs")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_else(|| vec!["~/pro/".to_string()]);

    let mut projects = Vec::new();
    let mut total_active = 0u32;
    let mut total_blocked = 0u32;

    for scan_dir in &scan_dirs {
        let expanded = expand_tilde(scan_dir);
        let expanded_path = Path::new(&expanded);

        if !expanded_path.exists() {
            continue;
        }

        let entries = match std::fs::read_dir(expanded_path) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                continue;
            }

            let project_path = entry.path();
            let handoff_dir = project_path.join(".handoff");

            if !handoff_dir.join("config.toml").exists() {
                continue;
            }

            if let Ok(info) = collect_project_info(&project_path) {
                total_active += info["active_tasks"].as_u64().unwrap_or(0) as u32;
                total_blocked += info["blocked_tasks"].as_u64().unwrap_or(0) as u32;
                projects.push(info);
            }
        }
    }

    let result = serde_json::json!({
        "projects": projects,
        "total_active_tasks": total_active,
        "total_blocked": total_blocked,
    });

    serde_json::to_string_pretty(&result).context("Failed to serialize dashboard")
}

fn collect_project_info(project_path: &Path) -> Result<Value> {
    let handoff_dir = project_path.join(".handoff");
    let config = read_config(&handoff_dir.join("config.toml"))?;

    let sessions = read_active_sessions(&handoff_dir.join("sessions"))?;

    let (_, summary) =
        build_task_index(&handoff_dir.join("tasks"), config.settings.done_task_limit)?;

    let last_session_ended = sessions.last().and_then(|s| s.ended_at.clone());

    let branch = sessions.last().and_then(|s| s.branch.clone());

    let active_tasks = *summary.by_status.get("in_progress").unwrap_or(&0)
        + *summary.by_status.get("todo").unwrap_or(&0)
        + *summary.by_status.get("review").unwrap_or(&0);

    let blocked_tasks = *summary.by_status.get("blocked").unwrap_or(&0);

    let blockers: Vec<String> = sessions
        .iter()
        .flat_map(|s| s.blockers.iter().cloned())
        .collect();

    let unread_referrals = read_referral_summaries(&handoff_dir.join("referrals"), Some("open"))
        .map(|r| r.len() as u32)
        .unwrap_or(0);

    Ok(serde_json::json!({
        "name": config.project.name,
        "path": project_path.to_string_lossy(),
        "last_session_ended": last_session_ended,
        "branch": branch,
        "active_tasks": active_tasks,
        "blocked_tasks": blocked_tasks,
        "blockers": blockers,
        "unread_referrals": unread_referrals,
    }))
}
