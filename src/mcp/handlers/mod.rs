pub mod config;
pub mod dashboard;
pub mod import_context;
pub mod init;
pub mod list_tasks;
pub mod load_context;
pub mod refer;
pub mod referrals;
pub mod save_context;
pub mod update_task;

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::mcp::types::JsonRpcResponse;

pub fn resolve_project_dir(arguments: &Value) -> Result<PathBuf> {
    let raw = match arguments.get("project_dir").and_then(|v| v.as_str()) {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir().context("Failed to get current directory")?,
    };
    std::fs::canonicalize(&raw).with_context(|| format!("Invalid project path: {}", raw.display()))
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
        "handoff_import_context" => import_context::handle(arguments),
        "handoff_refer" => refer::handle(arguments),
        "handoff_list_referrals" => referrals::handle_list(arguments),
        "handoff_update_referral" => referrals::handle_update(arguments),
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
