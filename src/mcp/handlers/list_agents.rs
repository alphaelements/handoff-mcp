//! `handoff_list_agents` tool (spec §7.1).
//!
//! Lists registered agents (`.handoff/agents/<agent-id>.json`), recomputing
//! each record's [`AgentStatus`] as of "now" (rather than trusting whatever
//! status was last persisted) so a caller always sees a fresh
//! active/stale/disconnected classification.

use anyhow::Result;
use chrono::Utc;
use serde_json::Value;

use super::HandlerContext;
use crate::storage::agents::{compute_status, list_agents, AgentStatus};

pub fn handle(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let status_filter = arguments.get("status").and_then(|v| v.as_str());
    let include_tasks = arguments
        .get("include_tasks")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let agents = list_agents(&ctx.handoff_dir)?;
    let now = Utc::now();

    let filtered: Vec<_> = agents
        .into_iter()
        .map(|mut a| {
            a.status = compute_status(&a, now);
            a
        })
        .filter(|a| match status_filter {
            Some("active") => a.status == AgentStatus::Active,
            Some("stale") => a.status == AgentStatus::Stale,
            Some("disconnected") => a.status == AgentStatus::Disconnected,
            _ => true, // "all" or unspecified
        })
        .collect();

    let agents_json: Vec<Value> = filtered
        .iter()
        .map(|a| {
            let mut obj = serde_json::json!({
                "agent_id": a.agent_id,
                "session_id": a.session_id,
                "worktree": a.worktree,
                "branch": a.branch,
                "status": a.status,
                "registered_at": a.registered_at,
                "last_heartbeat": a.last_heartbeat,
            });
            if include_tasks {
                obj["claimed_tasks"] = serde_json::json!(a.claimed_tasks);
            }
            obj
        })
        .collect();

    let result = serde_json::json!({
        "agents": agents_json,
        "total": filtered.len(),
    });

    serde_json::to_string_pretty(&result).map_err(Into::into)
}
