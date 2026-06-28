use serde_json::{json, Value};
use std::fs;
use tempfile::TempDir;

fn send(input: &str) -> Option<Value> {
    let result = handoff_mcp::mcp::protocol::process_line(input)?;
    Some(serde_json::from_str(&result).expect("response should be valid JSON"))
}

fn setup_project() -> TempDir {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let req = json!({
        "jsonrpc": "2.0", "id": 0,
        "method": "tools/call",
        "params": {
            "name": "handoff_init",
            "arguments": {
                "project_dir": dir.path().to_string_lossy(),
                "project_name": "timer-test"
            }
        }
    });
    send(&req.to_string()).unwrap();
    let cfg = json!({
        "jsonrpc": "2.0", "id": 0,
        "method": "tools/call",
        "params": {
            "name": "handoff_update_config",
            "arguments": {
                "project_dir": dir.path().to_string_lossy(),
                "updates": { "settings.require_estimate_hours": false }
            }
        }
    });
    send(&cfg.to_string()).unwrap();
    dir
}

fn call(dir: &TempDir, name: &str, mut arguments: Value) -> Value {
    arguments["project_dir"] = json!(dir.path().to_string_lossy());
    let req = json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments }
    });
    send(&req.to_string()).unwrap()
}

fn text(resp: &Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"].as_bool().unwrap_or(false)
}

fn create_task(dir: &TempDir, id: &str) {
    let resp = call(
        dir,
        "handoff_update_task",
        json!({
            "task": {
                "id": id,
                "title": format!("Test task {id}"),
                "status": "in_progress"
            }
        }),
    );
    assert!(!is_error(&resp), "create_task error: {}", text(&resp));
}

// ============================================================
// Tools appear in tools/list
// ============================================================

#[test]
fn timer_tools_appear_in_tools_list() {
    let resp = send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#)
        .expect("tools/list should return");
    let tools = resp["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(
        names.contains(&"handoff_timer_start"),
        "missing timer_start"
    );
    assert!(names.contains(&"handoff_timer_stop"), "missing timer_stop");
    assert!(
        names.contains(&"handoff_timer_get_time"),
        "missing timer_get_time"
    );
}

// ============================================================
// Fallback timer: start → get → stop round-trip
// ============================================================

#[test]
fn fallback_timer_start_get_stop_round_trip() {
    let dir = setup_project();
    create_task(&dir, "t1");

    // Start
    let resp = call(&dir, "handoff_timer_start", json!({ "task_id": "t1" }));
    assert!(!is_error(&resp), "start error: {}", text(&resp));
    let msg = text(&resp);
    assert!(
        msg.contains("MCP fallback timer started"),
        "expected fallback start, got: {msg}"
    );

    // Get time — should show tracking
    let resp = call(&dir, "handoff_timer_get_time", json!({ "task_id": "t1" }));
    assert!(!is_error(&resp), "get_time error: {}", text(&resp));
    let state: Value = serde_json::from_str(&text(&resp)).unwrap();
    assert_eq!(state["state"], "tracking");
    assert_eq!(state["task_id"], "t1");

    // Stop
    let resp = call(&dir, "handoff_timer_stop", json!({ "task_id": "t1" }));
    assert!(!is_error(&resp), "stop error: {}", text(&resp));
    let msg = text(&resp);
    assert!(
        msg.contains("MCP fallback timer stopped"),
        "expected fallback stop, got: {msg}"
    );
    assert!(msg.contains("logged"), "should mention logged hours");

    // Get time after stop — should be stopped
    let resp = call(&dir, "handoff_timer_get_time", json!({ "task_id": "t1" }));
    let state: Value = serde_json::from_str(&text(&resp)).unwrap();
    assert_eq!(state["state"], "stopped");
}

// ============================================================
// Stop adds to actual_hours via optimistic lock
// ============================================================

#[test]
fn stop_adds_actual_hours_to_task() {
    let dir = setup_project();
    create_task(&dir, "t2");

    // Log some initial hours
    let resp = call(
        &dir,
        "handoff_log_time",
        json!({ "task_id": "t2", "hours": 1.0 }),
    );
    assert!(!is_error(&resp), "log_time error: {}", text(&resp));

    // Start and immediately stop (elapsed ≈ 0)
    call(&dir, "handoff_timer_start", json!({ "task_id": "t2" }));
    let resp = call(&dir, "handoff_timer_stop", json!({ "task_id": "t2" }));
    assert!(!is_error(&resp));
    let msg = text(&resp);
    // actual should be ≈ 1.0 (the timer ran for near-zero time)
    assert!(msg.contains("actual="), "should show actual: {msg}");

    // Verify via get_task
    let task_resp = call(&dir, "handoff_get_task", json!({ "task_id": "t2" }));
    let task: Value = serde_json::from_str(&text(&task_resp)).unwrap();
    let actual = task["schedule"]["actual_hours"].as_f64().unwrap();
    assert!(
        actual >= 1.0,
        "actual_hours should be at least 1.0, got {actual}"
    );
}

// ============================================================
// Double start returns already-running message
// ============================================================

#[test]
fn double_start_returns_already_running() {
    let dir = setup_project();
    create_task(&dir, "t3");

    call(&dir, "handoff_timer_start", json!({ "task_id": "t3" }));
    let resp = call(&dir, "handoff_timer_start", json!({ "task_id": "t3" }));
    assert!(!is_error(&resp));
    let msg = text(&resp);
    assert!(
        msg.contains("already running"),
        "expected already running, got: {msg}"
    );

    // Cleanup
    call(&dir, "handoff_timer_stop", json!({ "task_id": "t3" }));
}

// ============================================================
// Stop without start returns error
// ============================================================

#[test]
fn stop_without_start_is_error() {
    let dir = setup_project();
    create_task(&dir, "t4");

    let resp = call(&dir, "handoff_timer_stop", json!({ "task_id": "t4" }));
    assert!(is_error(&resp), "stop without start should be error");
    let msg = text(&resp);
    assert!(
        msg.contains("No active timer"),
        "expected no active timer, got: {msg}"
    );
}

// ============================================================
// Timer with nonexistent task returns error
// ============================================================

#[test]
fn timer_with_nonexistent_task_errors() {
    let dir = setup_project();

    let resp = call(&dir, "handoff_timer_start", json!({ "task_id": "t999" }));
    assert!(is_error(&resp));
    assert!(text(&resp).contains("Task not found"));
}

// ============================================================
// Timer disabled (provider = off)
// ============================================================

#[test]
fn timer_off_rejects_start() {
    let dir = setup_project();
    create_task(&dir, "t5");

    // Set provider to off
    call(
        &dir,
        "handoff_update_config",
        json!({
            "updates": { "settings.timer_provider": "off" }
        }),
    );

    let resp = call(&dir, "handoff_timer_start", json!({ "task_id": "t5" }));
    assert!(is_error(&resp));
    assert!(
        text(&resp).contains("disabled"),
        "should say disabled: {}",
        text(&resp)
    );
}

// ============================================================
// VSCode delegation: creates request file
// ============================================================

#[test]
fn vscode_delegation_creates_request_file() {
    let dir = setup_project();
    create_task(&dir, "t6");

    // Simulate a live VSCode authority
    let timer_dir = dir.path().join(".handoff/timer");
    fs::create_dir_all(timer_dir.join("requests")).unwrap();
    let auth = json!({
        "version": 1,
        "owner": "vscode",
        "owner_instance": "12345",
        "heartbeat_at": chrono::Utc::now().to_rfc3339(),
        "ttl_secs": 30,
        "updated_at": chrono::Utc::now().to_rfc3339()
    });
    fs::write(
        timer_dir.join("authority.json"),
        serde_json::to_string_pretty(&auth).unwrap(),
    )
    .unwrap();

    // Start should delegate
    let resp = call(&dir, "handoff_timer_start", json!({ "task_id": "t6" }));
    assert!(!is_error(&resp), "delegate error: {}", text(&resp));
    let msg = text(&resp);
    assert!(
        msg.contains("delegated to VSCode"),
        "should delegate: {msg}"
    );

    // Verify request file was created
    let requests: Vec<_> = fs::read_dir(timer_dir.join("requests"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .collect();
    assert_eq!(requests.len(), 1, "should have exactly 1 request file");

    let req: Value =
        serde_json::from_str(&fs::read_to_string(requests[0].path()).unwrap()).unwrap();
    assert_eq!(req["cmd"], "start");
    assert_eq!(req["task_id"], "t6");
    assert_eq!(req["issued_by"], "mcp");
}

// ============================================================
// Stale authority falls back to MCP
// ============================================================

#[test]
fn stale_authority_falls_back_to_mcp() {
    let dir = setup_project();
    create_task(&dir, "t7");

    // Write a stale vscode authority (heartbeat 60 seconds ago with 30s TTL)
    let timer_dir = dir.path().join(".handoff/timer");
    fs::create_dir_all(timer_dir.join("requests")).unwrap();
    let stale_time = (chrono::Utc::now() - chrono::Duration::seconds(60)).to_rfc3339();
    let auth = json!({
        "version": 1,
        "owner": "vscode",
        "owner_instance": "old-pid",
        "heartbeat_at": stale_time,
        "ttl_secs": 30,
        "updated_at": stale_time
    });
    fs::write(
        timer_dir.join("authority.json"),
        serde_json::to_string_pretty(&auth).unwrap(),
    )
    .unwrap();

    // Start should fall back to MCP
    let resp = call(&dir, "handoff_timer_start", json!({ "task_id": "t7" }));
    assert!(!is_error(&resp), "fallback error: {}", text(&resp));
    let msg = text(&resp);
    assert!(
        msg.contains("MCP fallback"),
        "should fall back to MCP: {msg}"
    );

    // Authority should now be MCP
    let auth_content = fs::read_to_string(timer_dir.join("authority.json")).unwrap();
    let auth: Value = serde_json::from_str(&auth_content).unwrap();
    assert_eq!(auth["owner"], "mcp");

    // Cleanup
    call(&dir, "handoff_timer_stop", json!({ "task_id": "t7" }));
}

// ============================================================
// .handoff/timer/ directory layout
// ============================================================

#[test]
fn timer_creates_expected_directory_layout() {
    let dir = setup_project();
    create_task(&dir, "t8");

    call(&dir, "handoff_timer_start", json!({ "task_id": "t8" }));

    let timer = dir.path().join(".handoff/timer");
    assert!(timer.exists(), "timer/ should exist");
    assert!(timer.join("requests").exists(), "requests/ should exist");
    assert!(
        timer.join("authority.json").exists(),
        "authority.json should exist"
    );
    assert!(timer.join("state.json").exists(), "state.json should exist");

    // Verify state.json structure
    let state: Value =
        serde_json::from_str(&fs::read_to_string(timer.join("state.json")).unwrap()).unwrap();
    assert_eq!(state["version"], 1);
    assert_eq!(state["owner"], "mcp");
    assert!(state["timers"]["t8"].is_object());
    assert_eq!(state["timers"]["t8"]["state"], "tracking");

    // Cleanup
    call(&dir, "handoff_timer_stop", json!({ "task_id": "t8" }));
}

// ============================================================
// Config: timer settings roundtrip
// ============================================================

#[test]
fn config_timer_settings_roundtrip() {
    let dir = setup_project();

    // Update timer settings
    let resp = call(
        &dir,
        "handoff_update_config",
        json!({
            "updates": {
                "settings.timer_provider": "mcp",
                "settings.timer_authority_ttl_secs": 60,
                "settings.timer_idle_timeout_minutes": 15
            }
        }),
    );
    assert!(!is_error(&resp), "update error: {}", text(&resp));

    // Read back
    let resp = call(&dir, "handoff_get_config", json!({}));
    let config: Value = serde_json::from_str(&text(&resp)).unwrap();
    assert_eq!(config["settings"]["timer_provider"], "mcp");
    assert_eq!(config["settings"]["timer_authority_ttl_secs"], 60);
    assert_eq!(config["settings"]["timer_idle_timeout_minutes"], 15);
}

// ============================================================
// Config: invalid timer_provider rejected
// ============================================================

#[test]
fn config_rejects_invalid_timer_provider() {
    let dir = setup_project();

    let resp = call(
        &dir,
        "handoff_update_config",
        json!({
            "updates": { "settings.timer_provider": "invalid" }
        }),
    );
    assert!(is_error(&resp));
    assert!(text(&resp).contains("must be one of"));
}

// ============================================================
// get_time for non-started timer shows stopped
// ============================================================

#[test]
fn get_time_no_timer_shows_stopped() {
    let dir = setup_project();
    create_task(&dir, "t9");

    let resp = call(&dir, "handoff_timer_get_time", json!({ "task_id": "t9" }));
    assert!(!is_error(&resp), "get_time error: {}", text(&resp));
    let state: Value = serde_json::from_str(&text(&resp)).unwrap();
    assert_eq!(state["state"], "stopped");
    assert_eq!(state["elapsed_ms"], 0);
}

// ============================================================
// Adversarial E2E: request IDs are unique across rapid calls
// ============================================================

#[test]
fn request_ids_are_unique_across_rapid_calls() {
    let dir = setup_project();
    create_task(&dir, "t10");

    // Simulate live VSCode authority
    let timer_dir = dir.path().join(".handoff/timer");
    fs::create_dir_all(timer_dir.join("requests")).unwrap();
    let auth = json!({
        "version": 1,
        "owner": "vscode",
        "owner_instance": "99999",
        "heartbeat_at": chrono::Utc::now().to_rfc3339(),
        "ttl_secs": 30,
        "updated_at": chrono::Utc::now().to_rfc3339()
    });
    fs::write(
        timer_dir.join("authority.json"),
        serde_json::to_string_pretty(&auth).unwrap(),
    )
    .unwrap();

    // Rapid-fire: start then stop in same process → 2 request files
    call(&dir, "handoff_timer_start", json!({ "task_id": "t10" }));
    call(&dir, "handoff_timer_stop", json!({ "task_id": "t10" }));

    let requests: Vec<_> = fs::read_dir(timer_dir.join("requests"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .collect();
    assert_eq!(
        requests.len(),
        2,
        "should have 2 distinct request files, got {}",
        requests.len()
    );

    // Verify IDs are different
    let ids: Vec<String> = requests
        .iter()
        .map(|e| {
            let r: Value = serde_json::from_str(&fs::read_to_string(e.path()).unwrap()).unwrap();
            r["id"].as_str().unwrap().to_string()
        })
        .collect();
    assert_ne!(ids[0], ids[1], "request IDs must be unique: {ids:?}");
}

// ============================================================
// Adversarial E2E: vscode delegation with stale authority warns
// ============================================================

#[test]
fn vscode_forced_delegation_with_stale_authority_warns() {
    let dir = setup_project();
    create_task(&dir, "t11");

    // Force timer_provider = vscode
    call(
        &dir,
        "handoff_update_config",
        json!({ "updates": { "settings.timer_provider": "vscode" } }),
    );

    // Write a stale authority
    let timer_dir = dir.path().join(".handoff/timer");
    fs::create_dir_all(timer_dir.join("requests")).unwrap();
    let stale_time = (chrono::Utc::now() - chrono::Duration::seconds(120)).to_rfc3339();
    let auth = json!({
        "version": 1,
        "owner": "vscode",
        "owner_instance": "dead-pid",
        "heartbeat_at": stale_time,
        "ttl_secs": 30,
        "updated_at": stale_time
    });
    fs::write(
        timer_dir.join("authority.json"),
        serde_json::to_string_pretty(&auth).unwrap(),
    )
    .unwrap();

    // Start should delegate but include WARNING
    let resp = call(&dir, "handoff_timer_start", json!({ "task_id": "t11" }));
    assert!(!is_error(&resp), "should not error: {}", text(&resp));
    let msg = text(&resp);
    assert!(
        msg.contains("delegated to VSCode"),
        "should delegate: {msg}"
    );
    assert!(
        msg.contains("WARNING"),
        "should warn about stale authority: {msg}"
    );
}

// ============================================================
// Adversarial E2E: get_time projected_total_hours reflects
// log_time calls made while timer is running
// ============================================================

#[test]
fn get_time_projected_total_reflects_interim_log_time() {
    let dir = setup_project();
    create_task(&dir, "t12");

    // Log 2 hours before starting timer
    call(
        &dir,
        "handoff_log_time",
        json!({ "task_id": "t12", "hours": 2.0 }),
    );

    // Start timer (base_hours should snapshot 2.0)
    call(&dir, "handoff_timer_start", json!({ "task_id": "t12" }));

    // Log another 1 hour while timer is running (simulates concurrent session)
    call(
        &dir,
        "handoff_log_time",
        json!({ "task_id": "t12", "hours": 1.0 }),
    );

    // get_time should show current_actual_hours = 3.0 (not stale base_hours = 2.0)
    let resp = call(&dir, "handoff_timer_get_time", json!({ "task_id": "t12" }));
    assert!(!is_error(&resp), "get_time error: {}", text(&resp));
    let state: Value = serde_json::from_str(&text(&resp)).unwrap();

    assert_eq!(
        state["base_hours"], 2.0,
        "base_hours is start-time snapshot"
    );
    let current_actual = state["current_actual_hours"].as_f64().unwrap();
    assert!(
        current_actual >= 3.0,
        "current_actual_hours should reflect interim log_time: got {current_actual}"
    );

    // projected_total should be current_actual + elapsed (elapsed ≈ 0)
    let projected: f64 = state["projected_total_hours"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        projected >= 3.0,
        "projected_total_hours should be >= 3.0: got {projected}"
    );

    // Cleanup
    call(&dir, "handoff_timer_stop", json!({ "task_id": "t12" }));
}

// ============================================================
// Adversarial E2E: crash-safe stop ordering — after stop,
// state.json has no timer entry even though actual_hours is updated
// ============================================================

#[test]
fn stop_removes_timer_from_state_and_adds_actual_hours() {
    let dir = setup_project();
    create_task(&dir, "t13");

    call(&dir, "handoff_timer_start", json!({ "task_id": "t13" }));

    // Verify timer is in state.json
    let timer_path = dir.path().join(".handoff/timer");
    let state_before: Value =
        serde_json::from_str(&fs::read_to_string(timer_path.join("state.json")).unwrap()).unwrap();
    assert!(
        state_before["timers"]["t13"].is_object(),
        "timer should exist before stop"
    );

    call(&dir, "handoff_timer_stop", json!({ "task_id": "t13" }));

    // After stop: state.json should NOT have the timer entry
    let state_after: Value =
        serde_json::from_str(&fs::read_to_string(timer_path.join("state.json")).unwrap()).unwrap();
    assert!(
        state_after["timers"]["t13"].is_null(),
        "timer should be removed from state.json after stop"
    );

    // But actual_hours should have been updated on the task
    let task_resp = call(&dir, "handoff_get_task", json!({ "task_id": "t13" }));
    let task: Value = serde_json::from_str(&text(&task_resp)).unwrap();
    assert!(
        task["schedule"]["actual_hours"].is_number(),
        "actual_hours should be set after stop"
    );
}

// ============================================================
// Adversarial E2E: state.json optimistic lock — verify
// updated_at changes after each write
// ============================================================

#[test]
fn state_json_updated_at_changes_on_each_write() {
    let dir = setup_project();
    create_task(&dir, "t14");
    create_task(&dir, "t14b");

    call(&dir, "handoff_timer_start", json!({ "task_id": "t14" }));

    let timer_path = dir.path().join(".handoff/timer");
    let state1: Value =
        serde_json::from_str(&fs::read_to_string(timer_path.join("state.json")).unwrap()).unwrap();
    let ts1 = state1["updated_at"].as_str().unwrap().to_string();

    // Small delay to ensure timestamp differs
    std::thread::sleep(std::time::Duration::from_millis(10));

    // Start another timer — should update state.json with new updated_at
    call(&dir, "handoff_timer_start", json!({ "task_id": "t14b" }));

    let state2: Value =
        serde_json::from_str(&fs::read_to_string(timer_path.join("state.json")).unwrap()).unwrap();
    let ts2 = state2["updated_at"].as_str().unwrap().to_string();

    assert_ne!(ts1, ts2, "updated_at should change between writes");

    // Both timers should exist
    assert!(state2["timers"]["t14"].is_object());
    assert!(state2["timers"]["t14b"].is_object());

    // Cleanup
    call(&dir, "handoff_timer_stop", json!({ "task_id": "t14" }));
    call(&dir, "handoff_timer_stop", json!({ "task_id": "t14b" }));
}
