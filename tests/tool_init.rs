use serde_json::{json, Value};
use tempfile::TempDir;

fn send(input: &str) -> Option<Value> {
    let result = handoff_mcp::mcp::protocol::process_line(input)?;
    Some(serde_json::from_str(&result).expect("response should be valid JSON"))
}

fn setup() -> TempDir {
    tempfile::tempdir().expect("failed to create temp dir")
}

#[test]
fn init_creates_handoff_directory() {
    let dir = setup();
    let project_dir = dir.path().to_string_lossy();

    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "handoff_init",
            "arguments": {
                "project_dir": project_dir,
                "project_name": "test-project",
                "description": "A test project"
            }
        }
    });

    let resp = send(&req.to_string()).expect("should return response");
    assert_eq!(resp["id"], 1);
    assert!(resp["error"].is_null(), "error: {:?}", resp["error"]);

    let content = &resp["result"]["content"][0]["text"];
    assert!(
        content.as_str().unwrap().contains("test-project"),
        "response: {content}"
    );

    let handoff = dir.path().join(".handoff");
    assert!(handoff.exists());
    assert!(handoff.join("config.toml").exists());
    assert!(handoff.join("sessions").is_dir());
    assert!(handoff.join("tasks").is_dir());

    let config_content = std::fs::read_to_string(handoff.join("config.toml")).unwrap();
    assert!(config_content.contains("test-project"));
    assert!(config_content.contains("A test project"));
}

#[test]
fn init_double_init_returns_error() {
    let dir = setup();
    let project_dir = dir.path().to_string_lossy();

    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "handoff_init",
            "arguments": {
                "project_dir": project_dir,
                "project_name": "test-project"
            }
        }
    });

    send(&req.to_string()).unwrap();

    let req2 = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "handoff_init",
            "arguments": {
                "project_dir": project_dir,
                "project_name": "test-project"
            }
        }
    });

    let resp = send(&req2.to_string()).unwrap();
    let content = &resp["result"]["content"][0]["text"];
    assert!(content.as_str().unwrap().contains("already"));
    assert_eq!(resp["result"]["isError"], true);
}

#[test]
fn init_without_project_name_returns_error() {
    let dir = setup();
    let project_dir = dir.path().to_string_lossy();

    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "handoff_init",
            "arguments": {
                "project_dir": project_dir
            }
        }
    });

    let resp = send(&req.to_string()).unwrap();
    assert_eq!(resp["result"]["isError"], true);
}

#[test]
fn init_config_has_defaults() {
    let dir = setup();
    let project_dir = dir.path().to_string_lossy();

    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "handoff_init",
            "arguments": {
                "project_dir": project_dir,
                "project_name": "defaults-test"
            }
        }
    });

    send(&req.to_string()).unwrap();

    let config =
        handoff_mcp::storage::config::read_config(&dir.path().join(".handoff/config.toml"))
            .unwrap();

    assert_eq!(config.project.name, "defaults-test");
    assert_eq!(config.settings.history_limit, 20);
    assert_eq!(config.settings.done_task_limit, 10);
    assert!(config.settings.auto_git_summary);
}

#[test]
fn tools_call_unknown_tool_returns_error() {
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "nonexistent_tool",
            "arguments": {}
        }
    });

    let resp = send(&req.to_string()).unwrap();
    assert_eq!(resp["result"]["isError"], true);
    assert!(resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("not implemented"));
}

#[test]
fn tools_call_missing_name_returns_error() {
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "arguments": {}
        }
    });

    let resp = send(&req.to_string()).unwrap();
    assert!(resp["error"].is_object());
}
