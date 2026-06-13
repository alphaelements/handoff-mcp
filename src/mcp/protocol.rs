use super::router::handle_request;
use super::types::{JsonRpcRequest, JsonRpcResponse, INVALID_REQUEST, PARSE_ERROR};

pub fn process_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let response = match serde_json::from_str::<JsonRpcRequest>(trimmed) {
        Ok(req) => {
            if req.jsonrpc != "2.0" {
                JsonRpcResponse::error(req.id, INVALID_REQUEST, "Invalid JSON-RPC version")
            } else {
                let mut resp = handle_request(&req.method, req.params.as_ref());
                if resp.id.is_none() && req.id.is_some() {
                    resp.id = req.id;
                }
                if is_notification(&req.method) && resp.result.is_none() && resp.error.is_none() {
                    return None;
                }
                resp
            }
        }
        Err(e) => JsonRpcResponse::error(None, PARSE_ERROR, format!("Parse error: {e}")),
    };

    match serde_json::to_string(&response) {
        Ok(s) => Some(s),
        Err(e) => {
            let fallback = format!(
                r#"{{"jsonrpc":"2.0","id":null,"error":{{"code":-32603,"message":"Serialization error: {e}"}}}}"#
            );
            Some(fallback)
        }
    }
}

fn is_notification(method: &str) -> bool {
    method.starts_with("notifications/")
}
