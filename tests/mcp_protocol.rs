use serde_json::{json, Value};

fn send(input: &str) -> Option<Value> {
    let result = handoff_mcp::mcp::protocol::process_line(input)?;
    Some(serde_json::from_str(&result).expect("response should be valid JSON"))
}

#[test]
fn initialize_returns_capabilities() {
    let resp = send(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"0.1.0"}}}"#,
    )
    .expect("initialize should return a response");

    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 1);
    assert!(resp["error"].is_null());

    let result = &resp["result"];
    assert_eq!(result["protocolVersion"], "2025-03-26");
    assert!(result["capabilities"]["tools"].is_object());
    assert!(result["capabilities"]["resources"].is_object());
    assert_eq!(result["serverInfo"]["name"], "handoff-mcp");
    assert!(result["serverInfo"]["version"].is_string());
    assert!(result["instructions"].is_string());
}

#[test]
fn initialized_notification_returns_nothing() {
    let resp = send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    assert!(
        resp.is_none(),
        "notifications should not produce a response"
    );
}

#[test]
fn tools_list_returns_all_tools() {
    let resp = send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#)
        .expect("tools/list should return a response");

    assert_eq!(resp["id"], 2);
    assert!(resp["error"].is_null());

    let tools = resp["result"]["tools"]
        .as_array()
        .expect("tools should be an array");

    let expected_names = [
        "handoff_init",
        "handoff_load_context",
        "handoff_save_context",
        "handoff_list_tasks",
        "handoff_update_task",
        "handoff_get_config",
        "handoff_update_config",
        "handoff_dashboard",
        "handoff_import_context",
        "handoff_refer",
        "handoff_list_referrals",
        "handoff_get_referral",
        "handoff_update_referral",
    ];

    let tool_names: Vec<&str> = tools
        .iter()
        .map(|t| t["name"].as_str().expect("tool name should be a string"))
        .collect();

    for expected in &expected_names {
        assert!(tool_names.contains(expected), "missing tool: {expected}");
    }

    for tool in tools {
        assert!(
            tool["description"].is_string(),
            "tool should have description"
        );
        assert!(
            tool["inputSchema"].is_object(),
            "tool should have inputSchema"
        );
    }
}

#[test]
fn resources_list_returns_resources() {
    let resp = send(r#"{"jsonrpc":"2.0","id":3,"method":"resources/list","params":{}}"#)
        .expect("resources/list should return a response");

    assert_eq!(resp["id"], 3);
    assert!(resp["error"].is_null());

    let resources = resp["result"]["resources"]
        .as_array()
        .expect("resources should be an array");

    assert_eq!(resources.len(), 2);

    let uris: Vec<&str> = resources
        .iter()
        .map(|r| r["uri"].as_str().expect("resource uri"))
        .collect();

    assert!(uris.contains(&"handoff://sessions"));
    assert!(uris.contains(&"handoff://config"));
}

#[test]
fn invalid_json_returns_parse_error() {
    let resp = send("not valid json at all").expect("should return error response");

    assert_eq!(resp["jsonrpc"], "2.0");
    assert!(resp["id"].is_null());

    let error = &resp["error"];
    assert_eq!(error["code"], -32700);
    assert!(error["message"].as_str().unwrap().contains("Parse error"));
}

#[test]
fn unknown_method_returns_method_not_found() {
    let resp = send(r#"{"jsonrpc":"2.0","id":99,"method":"nonexistent/method","params":{}}"#)
        .expect("should return error response");

    assert_eq!(resp["id"], 99);

    let error = &resp["error"];
    assert_eq!(error["code"], -32601);
    assert!(error["message"]
        .as_str()
        .unwrap()
        .contains("Method not found"));
}

#[test]
fn invalid_jsonrpc_version_returns_error() {
    let resp = send(r#"{"jsonrpc":"1.0","id":5,"method":"initialize","params":{}}"#)
        .expect("should return error response");

    assert_eq!(resp["id"], 5);
    let error = &resp["error"];
    assert_eq!(error["code"], -32600);
}

#[test]
fn empty_line_returns_none() {
    assert!(send("").is_none());
    assert!(send("   ").is_none());
}

#[test]
fn request_id_is_preserved() {
    let resp_str = send(r#"{"jsonrpc":"2.0","id":"abc-123","method":"tools/list","params":{}}"#)
        .expect("should return response");
    assert_eq!(resp_str["id"], "abc-123");

    let resp_num = send(r#"{"jsonrpc":"2.0","id":42,"method":"tools/list","params":{}}"#)
        .expect("should return response");
    assert_eq!(resp_num["id"], 42);
}

#[test]
fn tools_have_valid_input_schemas() {
    let resp = send(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#)
        .expect("tools/list response");

    let tools = resp["result"]["tools"].as_array().unwrap();
    for tool in tools {
        let schema = &tool["inputSchema"];
        assert_eq!(
            schema["type"], "object",
            "inputSchema for {} should have type: object",
            tool["name"]
        );
    }
}

#[test]
fn multiple_requests_in_sequence() {
    let r1 = send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#).unwrap();
    assert!(r1["result"].is_object());

    send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);

    let r2 = send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#).unwrap();
    assert!(r2["result"]["tools"].is_array());

    let r3 = send(r#"{"jsonrpc":"2.0","id":3,"method":"resources/list","params":{}}"#).unwrap();
    assert!(r3["result"]["resources"].is_array());
}

#[test]
fn e2e_json_roundtrip() {
    let input = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": { "name": "test-client", "version": "1.0" }
        }
    });

    let resp = send(&input.to_string()).unwrap();
    assert_eq!(resp["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(resp["result"]["serverInfo"]["name"], "handoff-mcp");
}
