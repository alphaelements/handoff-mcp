use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;

use super::HandlerContext;
use crate::storage::agents::{compute_status, list_agents};
use crate::storage::config::{read_config, DashboardConfig};
use crate::storage::expand_tilde;
use crate::storage::referrals::read_referral_summaries;
use crate::storage::sessions::{read_active_sessions, read_open_sessions, read_paused_sessions};
use crate::storage::tasks::{build_task_index, collect_all_tasks, TaskLock};

/// `handoff_dashboard` scans multiple projects under `scan_dirs`, so it does
/// not use `ctx.project_dir`/`ctx.handoff_dir` (there is no single project in
/// scope). `ctx` is accepted for signature consistency with every other
/// handler and to leave room for future per-agent dashboard filtering.
pub fn handle(_ctx: &HandlerContext, arguments: &Value) -> Result<String> {
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

    // Lazy scan (spec 3.3.5, 7.2): reclaim expired leases for each scanned
    // project before summarizing its task counts. This also clears the
    // now-expired `lock` from disk, so the ids it reclaimed are captured
    // here and turned into "LEASE EXPIRED" warnings below — by the time
    // `tasks_with_claims` reads the task back, the lock is already gone.
    let expired_ids =
        crate::storage::tasks::scan_expired_leases(&handoff_dir.join("tasks")).unwrap_or_default();

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

    // Agent worktree lookup for claimed tasks' `worktree` field (spec
    // FR-1.6 §3): built once per project so `tasks_with_claims` doesn't
    // re-read the agents/ directory per locked task.
    let agent_records = list_agents(&handoff_dir).unwrap_or_default();

    let (tasks, mut warnings) = tasks_with_claims(&handoff_dir, &agent_records);
    for task_id in &expired_ids {
        warnings.push(format!("⚠ LEASE EXPIRED: task {task_id} (lease reclaimed)"));
    }

    let agents = agents_summary(&agent_records);

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
        "tasks": tasks,
        "warnings": warnings,
        "agents": agents,
    }))
}

/// Builds the per-task claim view (spec FR-1.6 §1-2) plus the project's
/// stale/expired lease warnings. A task with no `lock` renders with none of
/// `claimed_by`/`lease_remaining`/`worktree` present, preserving the exact
/// pre-t240.11 shape for unlocked tasks (done_criteria #3).
fn tasks_with_claims(
    handoff_dir: &Path,
    agent_records: &[crate::storage::agents::AgentRecord],
) -> (Vec<Value>, Vec<String>) {
    let mut all = Vec::new();
    if collect_all_tasks(&handoff_dir.join("tasks"), &mut all).is_err() {
        return (Vec::new(), Vec::new());
    }

    let now = Utc::now();
    let mut tasks = Vec::new();
    let mut warnings = Vec::new();

    for (data, status) in all {
        let mut task_json = serde_json::json!({
            "id": data.id,
            "title": data.title,
            "status": status,
        });

        if let Some(lock) = &data.lock {
            task_json["claimed_by"] = serde_json::json!(lock.agent_id);
            task_json["lease_remaining"] = serde_json::json!(format_lease_remaining(lock, now));
            if let Some(worktree) = agent_records
                .iter()
                .find(|a| a.agent_id == lock.agent_id)
                .map(|a| a.worktree.to_string_lossy().to_string())
            {
                task_json["worktree"] = serde_json::json!(worktree);
            }

            if let Some(warning) = lease_warning(&data.id, lock, now) {
                warnings.push(warning);
            }
        }

        tasks.push(task_json);
    }

    (tasks, warnings)
}

/// A lease that has already passed `lease_expires_at` gets `"⚠ LEASE
/// EXPIRED"`; one that has not yet expired but has less than one full TTL of
/// buffer left (i.e. it has been claimed for more than its own
/// `lease_ttl_seconds`, mirroring how [`compute_status`] classifies an agent
/// heartbeat as `Stale` once it exceeds its own TTL window) gets `"⚠
/// STALE"`. Only called for tasks that carry a `lock` — `claim_task` always
/// transitions a task to `in_progress` when it sets one, so in practice this
/// only ever fires for `in_progress` tasks, matching the task spec.
fn lease_warning(task_id: &str, lock: &TaskLock, now: DateTime<Utc>) -> Option<String> {
    let expires_at = DateTime::parse_from_rfc3339(&lock.lease_expires_at)
        .ok()?
        .with_timezone(&Utc);
    let remaining = (expires_at - now).num_seconds();

    if remaining <= 0 {
        Some(format!(
            "⚠ LEASE EXPIRED: task {task_id} (agent {})",
            lock.agent_id
        ))
    } else if remaining < lock.lease_ttl_seconds as i64 {
        Some(format!("⚠ STALE: task {task_id} (agent {})", lock.agent_id))
    } else {
        None
    }
}

/// Formats remaining lease time as a coarse human-readable string
/// (`"25m"`, `"1h"`, `"expired"`). Coarse because a dashboard consumer only
/// needs "about how long", not second-level precision.
fn format_lease_remaining(lock: &TaskLock, now: DateTime<Utc>) -> String {
    let expires_at = match DateTime::parse_from_rfc3339(&lock.lease_expires_at) {
        Ok(dt) => dt.with_timezone(&Utc),
        Err(_) => return "unknown".to_string(),
    };
    let remaining = (expires_at - now).num_seconds();
    if remaining <= 0 {
        return "expired".to_string();
    }
    if remaining < 3600 {
        format!("{}m", (remaining + 59) / 60)
    } else {
        format!("{}h", (remaining + 3599) / 3600)
    }
}

/// Builds the `## Agents` section (spec FR-1.6 §3). Returns an empty vec
/// (never an error) when `.handoff/agents/` does not exist, so a project
/// with no registered agents renders identically to before t240.11
/// (done_criteria #4/#6).
fn agents_summary(agent_records: &[crate::storage::agents::AgentRecord]) -> Vec<Value> {
    let now = Utc::now();
    agent_records
        .iter()
        .map(|a| {
            serde_json::json!({
                "agent_id": a.agent_id,
                "status": compute_status(a, now),
                "worktree": a.worktree,
                "claimed_tasks": a.claimed_tasks,
            })
        })
        .collect()
}
