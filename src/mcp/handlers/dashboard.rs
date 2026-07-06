use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::storage::config::{read_config, DashboardConfig};
use crate::storage::expand_tilde;
use crate::storage::referrals::read_referral_summaries;
use crate::storage::sessions::{read_active_sessions, read_open_sessions, read_paused_sessions};
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

        let (max_depth, exclude_patterns) = resolve_scan_config(expanded_path, arguments);

        let mut discovered = Vec::new();
        scan_recursive(
            expanded_path,
            1,
            max_depth,
            &exclude_patterns,
            &mut discovered,
        );

        for project_path in discovered {
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

/// Resolves effective `max_depth` / `exclude_patterns` for a single scan_dir.
///
/// Precedence: explicit tool `arguments` override (applies uniformly across
/// all scan_dirs, since it's an explicit user choice), then this scan_dir's
/// own `.handoff/config.toml` (if present), then — in the common umbrella-
/// workspace topology where the scan_dir itself is not a handoff project (e.g.
/// `~/pro/`) — the first *discovered child* project's config within this same
/// scan_dir's subtree, then built-in defaults.
///
/// Scoped to a single `expanded_path` so config discovered under one scan_dir
/// never leaks into sibling scan_dirs in a multi-root dashboard call.
fn resolve_scan_config(expanded_path: &Path, arguments: &Value) -> (usize, Vec<String>) {
    let mut defaults = DashboardConfig::default();

    let own_config_path = expanded_path.join(".handoff").join("config.toml");
    if let Ok(config) = read_config(&own_config_path) {
        defaults = config.dashboard;
    } else {
        // scan_dir itself has no config of its own (typical umbrella-workspace
        // case) — do a discovery pass scoped to this scan_dir's own subtree and
        // look for a child project whose own dashboard config overrides the
        // built-in default, so per-project settings still take effect without
        // requiring an explicit tool argument. Discovery order is filesystem-
        // dependent, so sort child paths for deterministic selection.
        //
        // Probe depth is capped at the caller's explicit max_depth argument
        // (if given) so a shallow-depth request doesn't still pay for a full
        // default-depth (5) filesystem walk just to look for fallback config.
        let probe_depth = arguments
            .get("max_depth")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or_else(|| DashboardConfig::default().max_depth)
            .min(DashboardConfig::default().max_depth);
        let mut discovered = Vec::new();
        scan_recursive(expanded_path, 1, probe_depth, &[], &mut discovered);
        discovered.sort();
        for child_path in discovered {
            let child_config_path = child_path.join(".handoff").join("config.toml");
            if let Ok(config) = read_config(&child_config_path) {
                if config.dashboard.max_depth != DashboardConfig::default().max_depth
                    || !config.dashboard.exclude_patterns.is_empty()
                {
                    defaults = config.dashboard;
                    break;
                }
            }
        }
    }

    let max_depth = arguments
        .get("max_depth")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(defaults.max_depth);

    let exclude_patterns = arguments
        .get("exclude_patterns")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or(defaults.exclude_patterns);

    (max_depth, exclude_patterns)
}

/// Recursively scans `dir` up to `max_depth` levels for `.handoff/config.toml`
/// markers, skipping directories whose name exactly matches an entry in
/// `exclude_patterns`. Never descends into a directory literally named
/// `.handoff` — a project's own bookkeeping tree (tasks/sessions/memory/etc.)
/// can never contain a nested project marker, so walking it would only waste
/// I/O proportional to the project's task/session history.
fn scan_recursive(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    exclude_patterns: &[String],
    results: &mut Vec<PathBuf>,
) {
    if depth > max_depth {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str == ".handoff" || exclude_patterns.iter().any(|p| p == name_str.as_ref()) {
            continue;
        }
        let path = entry.path();
        if path.join(".handoff").join("config.toml").exists() {
            results.push(path.clone());
        }
        scan_recursive(&path, depth + 1, max_depth, exclude_patterns, results);
    }
}

fn collect_project_info(project_path: &Path) -> Result<Value> {
    let handoff_dir = project_path.join(".handoff");
    let config = read_config(&handoff_dir.join("config.toml"))?;

    let sessions_dir = handoff_dir.join("sessions");
    let mut sessions = read_open_sessions(&sessions_dir)?;
    sessions.extend(read_active_sessions(&sessions_dir)?);
    let paused = read_paused_sessions(&sessions_dir)?;
    let paused_count = paused.len() as u32;
    sessions.extend(paused);

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
        "paused_sessions": paused_count,
    }))
}
