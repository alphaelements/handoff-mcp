pub mod assignees;
pub mod auto_schedule;
pub mod bulk_update;
pub mod calendar;
pub mod capacity;
pub mod check_criterion;
pub mod config;
pub mod config_crud;
pub mod dashboard;
pub mod docs;
pub mod docs_query;
pub mod fork_session;
pub mod get_session;
pub mod get_task;
pub mod import_context;
pub mod init;
pub mod list_sessions;
pub mod list_tasks;
pub mod load_context;
pub mod log_time;
pub mod memory;
pub mod merge_sessions;
pub mod metrics;
pub mod milestones;
pub mod refer;
pub mod referrals;
pub mod save_context;
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

pub fn handle_tool_call(name: &str, arguments: &Value) -> JsonRpcResponse {
    let result = match name {
        "handoff_init" => init::handle(arguments),
        "handoff_update_task" => update_task::handle(arguments),
        "handoff_list_tasks" => list_tasks::handle(arguments),
        "handoff_save_context" => save_context::handle(arguments),
        "handoff_load_context" => load_context::handle(arguments),
        "handoff_dashboard" => dashboard::handle(arguments),
        "handoff_get_config" => config::handle_get(arguments),
        "handoff_update_config" => config::handle_update(arguments),
        "handoff_get_task" => get_task::handle(arguments),
        "handoff_check_criterion" => check_criterion::handle(arguments),
        "handoff_import_context" => import_context::handle(arguments),
        "handoff_refer" => refer::handle(arguments),
        "handoff_list_referrals" => referrals::handle_list(arguments),
        "handoff_get_referral" => referrals::handle_get(arguments),
        "handoff_update_referral" => referrals::handle_update(arguments),
        "handoff_update_session" => update_session::handle(arguments),
        "handoff_log_time" => log_time::handle(arguments),
        "handoff_get_metrics" => metrics::handle(arguments),
        "handoff_list_sessions" => list_sessions::handle(arguments),
        "handoff_list_assignees" => assignees::handle(arguments),
        "handoff_bulk_update_tasks" => bulk_update::handle(arguments),
        "handoff_get_session" => get_session::handle(arguments),
        "handoff_get_capacity" => capacity::handle(arguments),
        "handoff_auto_schedule" => auto_schedule::handle(arguments),
        "handoff_add_assignee" => assignees::handle_add(arguments),
        "handoff_update_assignee" => assignees::handle_update(arguments),
        "handoff_remove_assignee" => assignees::handle_remove(arguments),
        "handoff_list_milestones" => milestones::handle_list(arguments),
        "handoff_add_milestone" => milestones::handle_add(arguments),
        "handoff_update_milestone" => milestones::handle_update(arguments),
        "handoff_remove_milestone" => milestones::handle_remove(arguments),
        "handoff_update_calendar" => calendar::handle_update_calendar(arguments),
        "handoff_update_labels" => calendar::handle_update_labels(arguments),
        "handoff_start_project" => calendar::handle_start_project(arguments),
        "handoff_memory_save" => memory::handle_save(arguments),
        "handoff_memory_query" => memory::handle_query(arguments),
        "handoff_memory_delete" => memory::handle_delete(arguments),
        "handoff_memory_cleanup" => memory::handle_cleanup(arguments),
        "handoff_fork_session" => fork_session::handle(arguments),
        "handoff_merge_sessions" => merge_sessions::handle(arguments),
        "handoff_timer_start" => timer::handle_start(arguments),
        "handoff_timer_stop" => timer::handle_stop(arguments),
        "handoff_timer_get_time" => timer::handle_get_time(arguments),
        "handoff_doc_save" => docs::handle_doc_save(arguments),
        "handoff_doc_get" => docs::handle_doc_get(arguments),
        "handoff_doc_list" => docs::handle_doc_list(arguments),
        "handoff_doc_delete" => docs::handle_doc_delete(arguments),
        "handoff_doc_reassemble" => docs::handle_doc_reassemble(arguments),
        "handoff_doc_tree" => docs::handle_doc_tree(arguments),
        "handoff_doc_query" => docs_query::handle_doc_query(arguments),
        "handoff_doc_analyze" => docs_query::handle_doc_analyze(arguments),
        "handoff_doc_import" => docs_query::handle_doc_import(arguments),
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
