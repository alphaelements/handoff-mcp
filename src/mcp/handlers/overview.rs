//! `handoff_overview` handler (multi-WT spec FR-2.2).
//!
//! Aggregates a single project's `.handoff/` state across worktrees into one
//! cross-cutting view: registered agents, a task x agent claim matrix, and a
//! worktree x branch x session mapping. Unlike `handoff_dashboard` (which
//! scans *multiple projects*), `overview` is scoped to the single project
//! resolved by `ctx.handoff_dir` and is meant to be called from the primary
//! worktree to monitor every worktree working the same project.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::Value;

use super::HandlerContext;
use crate::storage::agents::{compute_status, list_agents, AgentRecord};
use crate::storage::sessions::read_active_sessions;
use crate::storage::tasks::{collect_all_tasks, TaskLock};

pub fn handle(ctx: &HandlerContext, _arguments: &Value) -> Result<String> {
    let agent_records = list_agents(&ctx.handoff_dir).unwrap_or_default();
    let now = Utc::now();

    let agents_json = agents_summary(&agent_records, now);

    let mut all_tasks = Vec::new();
    let _ = collect_all_tasks(&ctx.handoff_dir.join("tasks"), &mut all_tasks);
    let task_matrix = task_matrix(&all_tasks, now);

    let sessions = read_active_sessions(&ctx.handoff_dir.join("sessions")).unwrap_or_default();
    let wt_sessions = wt_sessions_view(&agent_records, &sessions);

    let total_agents = agent_records.len();
    let active_agents = agent_records
        .iter()
        .filter(|a| compute_status(a, now) == crate::storage::agents::AgentStatus::Active)
        .count();
    let total_claimed: usize = agent_records.iter().map(|a| a.claimed_tasks.len()).sum();
    let total_in_progress = task_matrix
        .iter()
        .filter(|t| t["status"].as_str() == Some("in_progress"))
        .count();

    let result = serde_json::json!({
        "agents": agents_json,
        "task_matrix": task_matrix,
        "wt_sessions": wt_sessions,
        "summary": {
            "total_agents": total_agents,
            "active_agents": active_agents,
            "total_claimed": total_claimed,
            "total_in_progress": total_in_progress,
        },
    });

    serde_json::to_string_pretty(&result).map_err(Into::into)
}

fn agents_summary(agent_records: &[AgentRecord], now: DateTime<Utc>) -> Vec<Value> {
    agent_records
        .iter()
        .map(|a| {
            serde_json::json!({
                "agent_id": a.agent_id,
                "status": compute_status(a, now),
                "worktree": a.worktree,
                "branch": a.branch,
                "claimed_tasks": a.claimed_tasks,
            })
        })
        .collect()
}

/// Builds the task x agent matrix: every task with its status and (if
/// locked) the claiming agent and lease info. `lease_remaining_s` is the
/// raw seconds remaining (not the coarse human string used by
/// `handoff_dashboard`), since `overview` is meant for programmatic
/// consumption by an orchestrating agent.
fn task_matrix(
    all_tasks: &[(crate::storage::tasks::TaskData, String)],
    now: DateTime<Utc>,
) -> Vec<Value> {
    let mut matrix = Vec::new();
    for (data, status) in all_tasks {
        let mut entry = serde_json::json!({
            "task_id": data.id,
            "title": data.title,
            "status": status,
        });
        if let Some(lock) = &data.lock {
            entry["claimed_by"] = serde_json::json!(lock.agent_id);
            entry["lease_remaining_s"] = serde_json::json!(lease_remaining_seconds(lock, now));
        }
        matrix.push(entry);
    }
    matrix.sort_by(|a, b| {
        a["task_id"]
            .as_str()
            .unwrap_or("")
            .cmp(b["task_id"].as_str().unwrap_or(""))
    });
    matrix
}

fn lease_remaining_seconds(lock: &TaskLock, now: DateTime<Utc>) -> i64 {
    DateTime::parse_from_rfc3339(&lock.lease_expires_at)
        .map(|dt| (dt.with_timezone(&Utc) - now).num_seconds().max(0))
        .unwrap_or(0)
}

/// Builds the WT x branch x session mapping. Primarily driven by each active
/// session's own `worktree`/`scope`/`agent_id` fields (t250.1, spec §4.1
/// FR-2.1) — these are auto-detected at session-save time and are the
/// authoritative source for "which worktree/scope is this session in".
/// Registered agents that have no matching active session (e.g. between
/// `handoff_load_context` and the first `handoff_save_context`) still appear,
/// sourced from their `AgentRecord`, with `session_id`/`scope` left `null`,
/// so a single-WT or no-active-session environment renders without error.
fn wt_sessions_view(
    agent_records: &[AgentRecord],
    sessions: &[crate::storage::sessions::SessionData],
) -> Vec<Value> {
    let mut rows: Vec<Value> = sessions
        .iter()
        .map(|s| {
            serde_json::json!({
                "worktree": s.worktree,
                "branch": s.branch,
                "session_id": s.id,
                "agent_id": s.agent_id,
                "scope": s.scope,
            })
        })
        .collect();

    let covered_agent_ids: std::collections::HashSet<&str> = sessions
        .iter()
        .filter_map(|s| s.agent_id.as_deref())
        .collect();

    for a in agent_records {
        if covered_agent_ids.contains(a.agent_id.as_str()) {
            continue;
        }
        rows.push(serde_json::json!({
            "worktree": a.worktree,
            "branch": a.branch,
            "session_id": a.session_id,
            "agent_id": a.agent_id,
            "scope": Value::Null,
        }));
    }

    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ctx(handoff_dir: PathBuf) -> HandlerContext {
        HandlerContext {
            agent_id: None,
            project_dir: handoff_dir.parent().unwrap().to_path_buf(),
            handoff_dir,
        }
    }

    fn write_todo_task(tasks_dir: &std::path::Path, id: &str, dir_name: &str) {
        let task_dir = tasks_dir.join(dir_name);
        std::fs::create_dir_all(&task_dir).unwrap();
        let data = crate::storage::tasks::TaskData {
            id: id.to_string(),
            title: "Test".to_string(),
            notes: None,
            priority: None,
            created_at: None,
            updated_at: None,
            completed_at: None,
            labels: Vec::new(),
            links: Vec::new(),
            task_links: Vec::new(),
            done_criteria: Vec::new(),
            schedule: None,
            dependencies: Vec::new(),
            order: None,
            assignee: None,
            lock: None,
            scope_paths: Vec::new(),
            extra: Default::default(),
        };
        crate::storage::tasks::write_task(&task_dir, "todo", &data).unwrap();
    }

    #[test]
    fn handle_on_empty_project_returns_empty_collections_no_error() {
        // Single-WT / no-agents environment must not error (done_criteria #4).
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff_dir).unwrap();
        let c = ctx(handoff_dir);

        let result = handle(&c, &serde_json::json!({})).unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();

        assert_eq!(parsed["agents"].as_array().unwrap().len(), 0);
        assert_eq!(parsed["task_matrix"].as_array().unwrap().len(), 0);
        assert_eq!(parsed["wt_sessions"].as_array().unwrap().len(), 0);
        assert_eq!(parsed["summary"]["total_agents"], 0);
        assert_eq!(parsed["summary"]["active_agents"], 0);
    }

    #[test]
    fn handle_reports_agents_and_task_matrix() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        let tasks_dir = handoff_dir.join("tasks");
        write_todo_task(&tasks_dir, "t1", "t1-test");

        let record = AgentRecord {
            agent_id: "agent-1".to_string(),
            session_id: Some("s-1".to_string()),
            worktree: PathBuf::from("/tmp/wt1"),
            branch: Some("feat/x".to_string()),
            pid: None,
            registered_at: Utc::now(),
            last_heartbeat: Utc::now(),
            status: crate::storage::agents::AgentStatus::Active,
            claimed_tasks: vec!["t1".to_string()],
            metadata: Default::default(),
        };
        crate::storage::agents::write_agent(&handoff_dir, &record).unwrap();

        let c = ctx(handoff_dir.clone());

        // Claim the task so the matrix reflects an actual claimed_by entry.
        crate::storage::tasks::claim_task(
            &tasks_dir.join("t1-test"),
            "agent-1",
            "s-1",
            1800,
            &handoff_dir,
        )
        .unwrap();

        let result = handle(&c, &serde_json::json!({})).unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();

        assert_eq!(parsed["agents"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["agents"][0]["agent_id"], "agent-1");
        assert_eq!(parsed["agents"][0]["worktree"], "/tmp/wt1");

        let matrix = parsed["task_matrix"].as_array().unwrap();
        assert_eq!(matrix.len(), 1);
        assert_eq!(matrix[0]["task_id"], "t1");
        assert_eq!(matrix[0]["claimed_by"], "agent-1");
        assert_eq!(matrix[0]["status"], "in_progress");
        assert!(matrix[0]["lease_remaining_s"].as_i64().unwrap() > 0);

        assert_eq!(parsed["summary"]["total_agents"], 1);
        assert_eq!(parsed["summary"]["active_agents"], 1);
        assert_eq!(parsed["summary"]["total_claimed"], 1);
        assert_eq!(parsed["summary"]["total_in_progress"], 1);
    }

    #[test]
    fn handle_task_without_claim_has_no_claimed_by_field() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        let tasks_dir = handoff_dir.join("tasks");
        write_todo_task(&tasks_dir, "t1", "t1-test");

        let c = ctx(handoff_dir);
        let result = handle(&c, &serde_json::json!({})).unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();

        let matrix = parsed["task_matrix"].as_array().unwrap();
        assert_eq!(matrix.len(), 1);
        assert_eq!(matrix[0]["task_id"], "t1");
        assert_eq!(matrix[0]["status"], "todo");
        assert!(matrix[0].get("claimed_by").is_none());
    }
}
