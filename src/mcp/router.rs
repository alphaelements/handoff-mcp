use serde_json::{json, Value};

use super::handlers::handle_tool_call;
use super::tools::{all_resource_definitions, all_tool_definitions};
use super::types::{
    InitializeResult, JsonRpcResponse, ResourcesCapability, ServerCapabilities, ServerInfo,
    ToolsCapability, ToolsListResult, INTERNAL_ERROR, METHOD_NOT_FOUND, PROTOCOL_VERSION,
};

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
             3. Check the `next_actions` array first — these are the previous session's recommended next steps. Do not re-verify work the previous session already completed\n\n\
             ## Session End\n\
             1. Call handoff_save_context with:\n\
                - summary: one-line description of what was accomplished\n\
                - decisions: key decisions made (with reason and confidence)\n\
                - blockers: anything preventing progress\n\
                - handoff_notes: caution/context/suggestion for the next session\n\
                - context_pointers: files and line ranges the next session should look at\n\n\
             ## During Work\n\
             - Use handoff_update_task to create/update tasks as work progresses\n\
             - Mark tasks in_progress when starting, done when complete\n\
             - Record decisions as they are made, not just at session end"
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

    handle_tool_call(name, &arguments)
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
