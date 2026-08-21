pub mod assignees;
pub mod auto_schedule;
pub mod bulk_update;
pub mod calendar;
pub mod capacity;
pub mod check_criterion;
pub mod claim_release;
pub mod config;
pub mod config_crud;
pub mod dashboard;
pub mod docs;
pub mod docs_query;
pub mod events;
pub mod fork_session;
pub mod get_session;
pub mod get_task;
pub mod import_context;
pub mod init;
pub mod list_agents;
pub mod list_sessions;
pub mod list_tasks;
pub mod load_context;
pub mod log_time;
pub mod memory;
pub mod merge_sessions;
pub mod metrics;
pub mod milestones;
pub mod overview;
pub mod refer;
pub mod referrals;
pub mod save_context;
pub mod task_checklist;
pub mod timer;
pub mod update_session;
pub mod update_task;

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::mcp::types::JsonRpcResponse;

pub fn resolve_project_dir(arguments: &Value) -> Result<PathBuf> {
    let raw = match arguments
        .get("project_dir")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && !s.starts_with("${"))
    {
        Some(dir) => PathBuf::from(dir),
        None => match std::env::var("CLAUDE_PROJECT_DIR") {
            Ok(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => std::env::current_dir().context("Failed to get current directory")?,
        },
    };
    std::fs::canonicalize(&raw)
        .with_context(|| format!("Invalid project path: {raw}", raw = raw.display()))
}

/// Shared per-call context threaded through every handler. Carries the
/// resolved project/handoff directories (computed once by the MCP/CLI
/// dispatch layer) plus the calling agent's identity, so handlers no longer
/// each re-resolve `project_dir` / `.handoff/` themselves.
///
/// `agent_id` is `None` until this process has called `handoff_load_context`
/// at least once; from then on it carries the agent id that call registered
/// (see [`crate::mcp::router::set_agent_id`]/`get_agent_id`).
pub struct HandlerContext {
    pub agent_id: Option<String>,
    pub project_dir: PathBuf,
    pub handoff_dir: PathBuf,
}

pub fn handle_tool_call(ctx: &HandlerContext, name: &str, arguments: &Value) -> JsonRpcResponse {
    let result = match name {
        "handoff_init" => init::handle(ctx, arguments),
        "handoff_update_task" => update_task::handle(ctx, arguments),
        "handoff_list_tasks" => list_tasks::handle(ctx, arguments),
        "handoff_save_context" => save_context::handle(ctx, arguments),
        "handoff_load_context" => load_context::handle(ctx, arguments),
        "handoff_dashboard" => dashboard::handle(ctx, arguments),
        "handoff_get_config" => config::handle_get(ctx, arguments),
        "handoff_update_config" => config::handle_update(ctx, arguments),
        "handoff_get_task" => get_task::handle(ctx, arguments),
        "handoff_check_criterion" => check_criterion::handle(ctx, arguments),
        "handoff_import_context" => import_context::handle(ctx, arguments),
        "handoff_refer" => refer::handle(ctx, arguments),
        "handoff_list_referrals" => referrals::handle_list(ctx, arguments),
        "handoff_get_referral" => referrals::handle_get(ctx, arguments),
        "handoff_update_referral" => referrals::handle_update(ctx, arguments),
        "handoff_update_session" => update_session::handle(ctx, arguments),
        "handoff_log_time" => log_time::handle(ctx, arguments),
        "handoff_get_metrics" => metrics::handle(ctx, arguments),
        "handoff_list_sessions" => list_sessions::handle(ctx, arguments),
        "handoff_list_assignees" => assignees::handle(ctx, arguments),
        "handoff_bulk_update_tasks" => bulk_update::handle(ctx, arguments),
        "handoff_get_session" => get_session::handle(ctx, arguments),
        "handoff_get_capacity" => capacity::handle(ctx, arguments),
        "handoff_auto_schedule" => auto_schedule::handle(ctx, arguments),
        "handoff_add_assignee" => assignees::handle_add(ctx, arguments),
        "handoff_update_assignee" => assignees::handle_update(ctx, arguments),
        "handoff_remove_assignee" => assignees::handle_remove(ctx, arguments),
        "handoff_list_milestones" => milestones::handle_list(ctx, arguments),
        "handoff_add_milestone" => milestones::handle_add(ctx, arguments),
        "handoff_update_milestone" => milestones::handle_update(ctx, arguments),
        "handoff_remove_milestone" => milestones::handle_remove(ctx, arguments),
        "handoff_update_calendar" => calendar::handle_update_calendar(ctx, arguments),
        "handoff_update_labels" => calendar::handle_update_labels(ctx, arguments),
        "handoff_start_project" => calendar::handle_start_project(ctx, arguments),
        "handoff_memory_save" => memory::handle_save(ctx, arguments),
        "handoff_memory_query" => memory::handle_query(ctx, arguments),
        "handoff_memory_delete" => memory::handle_delete(ctx, arguments),
        "handoff_memory_cleanup" => memory::handle_cleanup(ctx, arguments),
        "handoff_fork_session" => fork_session::handle(ctx, arguments),
        "handoff_merge_sessions" => merge_sessions::handle(ctx, arguments),
        "handoff_timer_start" => timer::handle_start(ctx, arguments),
        "handoff_timer_stop" => timer::handle_stop(ctx, arguments),
        "handoff_timer_get_time" => timer::handle_get_time(ctx, arguments),
        "handoff_doc_save" => docs::handle_doc_save(ctx, arguments),
        "handoff_doc_update_section" => docs::handle_doc_update_section(ctx, arguments),
        "handoff_doc_get" => docs::handle_doc_get(ctx, arguments),
        "handoff_doc_list" => docs::handle_doc_list(ctx, arguments),
        "handoff_doc_delete" => docs::handle_doc_delete(ctx, arguments),
        "handoff_doc_reassemble" => docs::handle_doc_reassemble(ctx, arguments),
        "handoff_doc_tree" => docs::handle_doc_tree(ctx, arguments),
        "handoff_doc_graph" => docs::handle_doc_graph(ctx, arguments),
        "handoff_doc_trace" => docs::handle_doc_trace(ctx, arguments),
        "handoff_doc_verify" => docs::handle_doc_verify(ctx, arguments),
        "handoff_doc_verify_status" => docs::handle_doc_verify_status(ctx, arguments),
        "handoff_doc_query" => docs_query::handle_doc_query(ctx, arguments),
        "handoff_doc_analyze" => docs_query::handle_doc_analyze(ctx, arguments),
        "handoff_doc_import" => docs_query::handle_doc_import(ctx, arguments),
        "handoff_task_checklist" => task_checklist::handle(ctx, arguments),
        "handoff_claim_task" => claim_release::handle_claim(ctx, arguments),
        "handoff_release_task" => claim_release::handle_release(ctx, arguments),
        "handoff_reclaim_task" => claim_release::handle_reclaim(ctx, arguments),
        "handoff_list_agents" => list_agents::handle(ctx, arguments),
        "handoff_overview" => overview::handle(ctx, arguments),
        "handoff_events" => events::handle(ctx, arguments),
        _ => Err(anyhow::anyhow!("Tool not implemented: {name}")),
    };

    match result {
        Ok(content) => {
            let tool_result = serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": content
                }]
            });
            JsonRpcResponse::success(None, tool_result)
        }
        Err(e) => {
            let tool_result = serde_json::json!({
                "isError": true,
                "content": [{
                    "type": "text",
                    "text": format!("Error: {e}")
                }]
            });
            JsonRpcResponse::success(None, tool_result)
        }
    }
}

#[cfg(test)]
mod handler_context_tests {
    use super::*;

    #[test]
    fn handler_context_carries_agent_id_and_dirs() {
        // HandlerContext must expose agent_id / project_dir / handoff_dir so
        // handlers stop each re-resolving these from `arguments` themselves.
        let ctx = HandlerContext {
            agent_id: Some("agent-1".to_string()),
            project_dir: PathBuf::from("/tmp/proj"),
            handoff_dir: PathBuf::from("/tmp/proj/.handoff"),
        };
        assert_eq!(ctx.agent_id.as_deref(), Some("agent-1"));
        assert_eq!(ctx.project_dir, PathBuf::from("/tmp/proj"));
        assert_eq!(ctx.handoff_dir, PathBuf::from("/tmp/proj/.handoff"));
    }

    #[test]
    fn handle_tool_call_dispatches_with_context() {
        // handle_tool_call must accept a HandlerContext and route it through
        // to the handler (using an unknown tool name keeps this test
        // independent of any single handler's filesystem side effects).
        let ctx = HandlerContext {
            agent_id: None,
            project_dir: PathBuf::from("/tmp/proj"),
            handoff_dir: PathBuf::from("/tmp/proj/.handoff"),
        };
        let resp = handle_tool_call(&ctx, "not_a_real_tool", &Value::Null);
        let result = resp.result.expect("response should carry a result");
        assert_eq!(result["isError"], true);
    }
}
