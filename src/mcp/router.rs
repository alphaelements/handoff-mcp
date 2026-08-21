use std::sync::{LazyLock, Mutex};

use serde_json::{json, Value};

use super::handlers::{handle_tool_call, resolve_project_dir, HandlerContext};
use super::tools::{all_resource_definitions, all_tool_definitions};
use super::types::{
    InitializeResult, JsonRpcResponse, ResourcesCapability, ServerCapabilities, ServerInfo,
    ToolsCapability, ToolsListResult, INTERNAL_ERROR, METHOD_NOT_FOUND, PROTOCOL_VERSION,
};

/// Process-wide agent identity, set once `handoff_load_context` registers
/// this process as an agent (t240.12). `None` until then, and for any
/// process (e.g. tests) that never calls `handoff_load_context`.
///
/// A single global is deliberate: one running MCP server process serves
/// exactly one agent identity for its whole lifetime, and every subsequent
/// tool call needs that identity threaded into its [`HandlerContext`]
/// without the caller having to resend it on every request.
static AGENT_ID: LazyLock<Mutex<Option<String>>> = LazyLock::new(|| Mutex::new(None));

/// Record `id` as this process's agent identity for all future
/// [`HandlerContext`]s built by [`build_handler_context`].
pub fn set_agent_id(id: String) {
    *AGENT_ID.lock().unwrap_or_else(|e| e.into_inner()) = Some(id);
}

/// The agent identity registered by a prior `handoff_load_context` call in
/// this process, if any.
pub fn get_agent_id() -> Option<String> {
    AGENT_ID.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn handle_request(method: &str, params: Option<&Value>) -> JsonRpcResponse {
    match method {
        "initialize" => handle_initialize(),
        "notifications/initialized" => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: None,
            result: None,
            error: None,
        },
        "tools/list" => handle_tools_list(),
        "tools/call" => handle_tools_call(params),
        "resources/list" => handle_resources_list(),
        "resources/read" => handle_resources_read(params),
        _ => JsonRpcResponse::error(
            None,
            METHOD_NOT_FOUND,
            format!("Method not found: {method}"),
        ),
    }
}

fn handle_initialize() -> JsonRpcResponse {
    let result = InitializeResult {
        protocol_version: PROTOCOL_VERSION.to_string(),
        capabilities: ServerCapabilities {
            tools: Some(ToolsCapability {
                list_changed: false,
            }),
            resources: Some(ResourcesCapability {}),
        },
        server_info: ServerInfo {
            name: "handoff-mcp".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        instructions: Some(
            "Handoff MCP server for AI session context persistence. \
             Call handoff_load_context at session start, \
             handoff_save_context at session end.\n\n\
             ## Session Start\n\
             1. Call handoff_load_context (no args needed — uses cwd)\n\
             2. If it returns \"not initialized\", call handoff_init with the project name\n\
             3. If session_guidance is present, call handoff_save_context with session_status='active' to establish a persistent session before starting work\n\
             4. Check the `next_actions` array first — these are the previous session's recommended next steps. Do not re-verify work the previous session already completed\n\n\
             ## During Work — Progressive Updates\n\
             - Use handoff_update_task to create/update tasks as work progresses\n\
             - Mark tasks in_progress when starting, done when complete\n\
             - Use handoff_check_criterion to check off task done_criteria as each item is verified — do not wait until the task is fully done\n\
             - Use handoff_update_session to progressively update the active session: toggle checklist items, append decisions, notes, or context pointers\n\
             - When work reaches a point requiring user confirmation, set the task status to review\n\
             - Record decisions as they are made, not just at session end\n\n\
             ## Session End\n\
             1. Call handoff_save_context with:\n\
                - summary: one-line description of what was accomplished\n\
                - decisions: key decisions made (with reason and confidence)\n\
                - blockers: anything preventing progress\n\
                - handoff_notes: caution/context/suggestion for the next session\n\
                - context_pointers: files and line ranges the next session should look at"
                .to_string(),
        ),
    };
    match serde_json::to_value(result) {
        Ok(value) => JsonRpcResponse::success(None, value),
        Err(e) => JsonRpcResponse::error(None, INTERNAL_ERROR, format!("Serialization error: {e}")),
    }
}

fn handle_tools_list() -> JsonRpcResponse {
    let result = ToolsListResult {
        tools: all_tool_definitions(),
    };
    match serde_json::to_value(result) {
        Ok(value) => JsonRpcResponse::success(None, value),
        Err(e) => JsonRpcResponse::error(None, INTERNAL_ERROR, format!("Serialization error: {e}")),
    }
}

fn handle_tools_call(params: Option<&Value>) -> JsonRpcResponse {
    let params = match params {
        Some(p) => p,
        None => {
            return JsonRpcResponse::error(
                None,
                super::types::INVALID_REQUEST,
                "tools/call requires params",
            );
        }
    };

    let name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => {
            return JsonRpcResponse::error(
                None,
                super::types::INVALID_REQUEST,
                "tools/call requires 'name' parameter",
            );
        }
    };

    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let ctx = match build_handler_context(name, &arguments) {
        Ok(ctx) => ctx,
        Err(e) => {
            let tool_result = json!({
                "isError": true,
                "content": [{
                    "type": "text",
                    "text": format!("Error: {e}")
                }]
            });
            return JsonRpcResponse::success(None, tool_result);
        }
    };

    handle_tool_call(&ctx, name, &arguments)
}

/// Resolve `project_dir` and (for every tool except `handoff_init` /
/// `handoff_load_context`, which must tolerate a project with no
/// `.handoff/` yet) verify `.handoff/` exists, producing the shared
/// `HandlerContext` passed to every handler.
///
/// `agent_id` is populated from the process-wide identity set by a prior
/// `handoff_load_context` call (see [`set_agent_id`]); it stays `None` until
/// then.
fn build_handler_context(name: &str, arguments: &Value) -> anyhow::Result<HandlerContext> {
    let project_dir = resolve_project_dir(arguments)?;

    let handoff_dir = if matches!(name, "handoff_init" | "handoff_load_context") {
        crate::storage::handoff_dir(&project_dir)
    } else {
        crate::storage::ensure_handoff_exists(&project_dir)?
    };

    Ok(HandlerContext {
        agent_id: get_agent_id(),
        project_dir,
        handoff_dir,
    })
}

fn handle_resources_list() -> JsonRpcResponse {
    let resources = all_resource_definitions();
    let result = json!({ "resources": resources });
    JsonRpcResponse::success(None, result)
}

fn handle_resources_read(params: Option<&Value>) -> JsonRpcResponse {
    let params = match params {
        Some(p) => p,
        None => {
            return JsonRpcResponse::error(
                None,
                super::types::INVALID_REQUEST,
                "resources/read requires params",
            );
        }
    };

    let uri = match params.get("uri").and_then(|v| v.as_str()) {
        Some(u) => u,
        None => {
            return JsonRpcResponse::error(
                None,
                super::types::INVALID_REQUEST,
                "resources/read requires 'uri' parameter",
            );
        }
    };

    match super::resources::handle_resource_read(uri) {
        Ok(result) => JsonRpcResponse::success(None, result),
        Err(e) => JsonRpcResponse::error(
            None,
            super::types::INVALID_REQUEST,
            format!("Resource error: {e}"),
        ),
    }
}
