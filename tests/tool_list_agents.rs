//! `handoff_list_agents` tool (spec §7.1) and `handoff_load_context`
//! agent-registration integration (spec §7.2).

use serde_json::{json, Value};
use tempfile::TempDir;

fn send(input: &str) -> Option<Value> {
    let result = handoff_mcp::mcp::protocol::process_line(input)?;
    Some(serde_json::from_str(&result).expect("response should be valid JSON"))
}

fn setup() -> TempDir {
    tempfile::tempdir().expect("failed to create temp dir")
}

fn call(name: &str, arguments: Value) -> Value {
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": name,
            "arguments": arguments
        }
    });
    send(&req.to_string()).expect("should return response")
}

fn text_of(resp: &Value) -> Value {
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("expected text content");
    serde_json::from_str(text).expect("expected valid JSON text")
}

fn init(dir: &TempDir) -> String {
    let project_dir = dir.path().to_string_lossy().to_string();
    call(
        "handoff_init",
        json!({
            "project_dir": project_dir,
            "project_name": "list-agents-test"
        }),
    );
    project_dir
}

/// With no agents registered, `handoff_list_agents` must return an empty
/// list rather than erroring.
#[test]
fn list_agents_empty_when_none_registered() {
    let dir = setup();
    let project_dir = init(&dir);

    let resp = call("handoff_list_agents", json!({ "project_dir": project_dir }));
    let parsed = text_of(&resp);

    assert_eq!(parsed["total"], 0);
    assert_eq!(parsed["agents"].as_array().unwrap().len(), 0);
}

/// `handoff_load_context` must auto-register an agent record under
/// `.handoff/agents/` and the record must then be visible via
/// `handoff_list_agents`.
#[test]
fn load_context_registers_agent_visible_in_list_agents() {
    let dir = setup();
    let project_dir = init(&dir);

    call(
        "handoff_load_context",
        json!({ "project_dir": project_dir }),
    );

    let agents_dir = dir.path().join(".handoff/agents");
    assert!(agents_dir.exists(), "agents dir should be created");
    let count = std::fs::read_dir(&agents_dir).unwrap().count();
    assert_eq!(count, 1, "exactly one agent record should be written");

    let resp = call("handoff_list_agents", json!({ "project_dir": project_dir }));
    let parsed = text_of(&resp);
    assert_eq!(parsed["total"], 1);
    let agent = &parsed["agents"][0];
    assert!(agent["agent_id"].as_str().is_some_and(|s| !s.is_empty()));
    assert_eq!(agent["status"], "active");
}

/// `handoff_load_context`'s response must itself carry the registered
/// `agent_id`.
#[test]
fn load_context_response_includes_agent_id() {
    let dir = setup();
    let project_dir = init(&dir);

    let resp = call(
        "handoff_load_context",
        json!({ "project_dir": project_dir }),
    );
    let parsed = text_of(&resp);

    assert!(
        parsed["agent_id"].as_str().is_some_and(|s| !s.is_empty()),
        "expected non-empty agent_id in load_context response: {parsed}"
    );
    assert!(parsed["claimed_tasks"].is_array());
}

/// `handoff_list_agents` must filter by `status`.
#[test]
fn list_agents_filters_by_status() {
    let dir = setup();
    let project_dir = init(&dir);

    // Active agent via load_context.
    call(
        "handoff_load_context",
        json!({ "project_dir": project_dir }),
    );

    // Manually write a stale and a disconnected agent record directly to
    // storage (bypassing load_context, which always registers "active").
    let agents_dir = dir.path().join(".handoff/agents");
    let now = chrono::Utc::now();
    let stale_hb = now - chrono::Duration::seconds(1800 * 3 / 2); // within (ttl, 2*ttl]
    let disconnected_hb = now - chrono::Duration::seconds(1800 * 3); // beyond 2*ttl

    for (id, hb) in [
        ("agent-stale", stale_hb),
        ("agent-disconnected", disconnected_hb),
    ] {
        let record = json!({
            "agent_id": id,
            "session_id": null,
            "worktree": project_dir,
            "branch": null,
            "pid": null,
            "registered_at": now.to_rfc3339(),
            "last_heartbeat": hb.to_rfc3339(),
            "status": "active",
            "claimed_tasks": [],
            "metadata": {}
        });
        std::fs::write(
            agents_dir.join(format!("{id}.json")),
            serde_json::to_string_pretty(&record).unwrap(),
        )
        .unwrap();
    }

    let all_resp = call("handoff_list_agents", json!({ "project_dir": project_dir }));
    assert_eq!(text_of(&all_resp)["total"], 3);

    let active_resp = call(
        "handoff_list_agents",
        json!({ "project_dir": project_dir, "status": "active" }),
    );
    let active = text_of(&active_resp);
    assert_eq!(active["total"], 1);

    let stale_resp = call(
        "handoff_list_agents",
        json!({ "project_dir": project_dir, "status": "stale" }),
    );
    let stale = text_of(&stale_resp);
    assert_eq!(stale["total"], 1);
    assert_eq!(stale["agents"][0]["agent_id"], "agent-stale");

    let disconnected_resp = call(
        "handoff_list_agents",
        json!({ "project_dir": project_dir, "status": "disconnected" }),
    );
    let disconnected = text_of(&disconnected_resp);
    assert_eq!(disconnected["total"], 1);
    assert_eq!(disconnected["agents"][0]["agent_id"], "agent-disconnected");
}

/// `include_tasks: true` must surface each agent's `claimed_tasks`; when
/// omitted (or false) the field must not appear.
#[test]
fn list_agents_include_tasks_toggle() {
    let dir = setup();
    let project_dir = init(&dir);

    let agents_dir_path = dir.path().join(".handoff/agents");
    std::fs::create_dir_all(&agents_dir_path).unwrap();
    let now = chrono::Utc::now();
    let record = json!({
        "agent_id": "agent-with-tasks",
        "session_id": null,
        "worktree": project_dir,
        "branch": null,
        "pid": null,
        "registered_at": now.to_rfc3339(),
        "last_heartbeat": now.to_rfc3339(),
        "status": "active",
        "claimed_tasks": ["t1", "t1.2"],
        "metadata": {}
    });
    std::fs::write(
        agents_dir_path.join("agent-with-tasks.json"),
        serde_json::to_string_pretty(&record).unwrap(),
    )
    .unwrap();

    let without = call("handoff_list_agents", json!({ "project_dir": project_dir }));
    let without_parsed = text_of(&without);
    assert!(without_parsed["agents"][0].get("claimed_tasks").is_none());

    let with = call(
        "handoff_list_agents",
        json!({ "project_dir": project_dir, "include_tasks": true }),
    );
    let with_parsed = text_of(&with);
    let tasks = with_parsed["agents"][0]["claimed_tasks"]
        .as_array()
        .expect("claimed_tasks should be present");
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0], "t1");
}

/// Serializes tests that mutate `CLAUDE_SESSION_ID`, since env vars are
/// process-global and Rust runs tests in threads.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Reconnecting with the same `CLAUDE_SESSION_ID` (i.e. the same derived
/// `agent_id`, see `generate_agent_id`) must update the existing agent
/// record in place rather than creating a second one, and must preserve any
/// `claimed_tasks` already recorded against it.
#[test]
fn load_context_reconnect_updates_existing_record_not_duplicate() {
    let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved = std::env::var("CLAUDE_SESSION_ID").ok();
    std::env::set_var("CLAUDE_SESSION_ID", "s-reconnect-test-fixed-id");

    let dir = setup();
    let project_dir = init(&dir);

    let first = call(
        "handoff_load_context",
        json!({ "project_dir": project_dir }),
    );
    let first_agent_id = text_of(&first)["agent_id"].as_str().unwrap().to_string();
    assert_eq!(first_agent_id, "s-reconnect-test-fixed-id");

    // Simulate an in-flight claim recorded on this agent between the two
    // load_context calls.
    let agents_dir = dir.path().join(".handoff/agents");
    let path = agents_dir.join(format!("{first_agent_id}.json"));
    let mut record: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    record["claimed_tasks"] = json!(["t99"]);
    std::fs::write(&path, serde_json::to_string_pretty(&record).unwrap()).unwrap();

    let second = call(
        "handoff_load_context",
        json!({ "project_dir": project_dir }),
    );
    let second_parsed = text_of(&second);
    assert_eq!(second_parsed["agent_id"], first_agent_id);
    assert_eq!(second_parsed["claimed_tasks"], json!(["t99"]));

    let count = std::fs::read_dir(&agents_dir).unwrap().count();
    assert_eq!(count, 1, "reconnect must update, not duplicate, the record");

    match saved {
        Some(v) => std::env::set_var("CLAUDE_SESSION_ID", v),
        None => std::env::remove_var("CLAUDE_SESSION_ID"),
    }
}

/// Full E2E flow: init -> load_context -> list_agents shows the registered
/// agent as active with no claimed tasks.
#[test]
fn e2e_init_load_context_list_agents_flow() {
    let dir = setup();
    let project_dir = init(&dir);

    let load_resp = call(
        "handoff_load_context",
        json!({ "project_dir": project_dir }),
    );
    let load_parsed = text_of(&load_resp);
    let agent_id = load_parsed["agent_id"]
        .as_str()
        .expect("agent_id should be present")
        .to_string();

    let list_resp = call("handoff_list_agents", json!({ "project_dir": project_dir }));
    let list_parsed = text_of(&list_resp);
    assert_eq!(list_parsed["total"], 1);
    assert_eq!(list_parsed["agents"][0]["agent_id"], agent_id);
    assert_eq!(list_parsed["agents"][0]["status"], "active");
}
