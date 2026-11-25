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

// ============================================================
// Adversarial E2E: multiple timers — stopping one preserves others
// ============================================================

#[test]
fn stop_one_timer_preserves_other_running_timers() {
    let dir = setup_project();
    create_task(&dir, "t20a");
    create_task(&dir, "t20b");
    create_task(&dir, "t20c");

    call(&dir, "handoff_timer_start", json!({ "task_id": "t20a" }));
    call(&dir, "handoff_timer_start", json!({ "task_id": "t20b" }));
    call(&dir, "handoff_timer_start", json!({ "task_id": "t20c" }));

    // Stop only the middle one
    let resp = call(&dir, "handoff_timer_stop", json!({ "task_id": "t20b" }));
    assert!(!is_error(&resp), "stop t20b error: {}", text(&resp));

    // t20a and t20c should still be tracking
    let timer_path = dir.path().join(".handoff/timer");
    let state: Value =
        serde_json::from_str(&fs::read_to_string(timer_path.join("state.json")).unwrap()).unwrap();
    assert_eq!(state["timers"]["t20a"]["state"], "tracking");
    assert!(state["timers"]["t20b"].is_null(), "t20b should be removed");
    assert_eq!(state["timers"]["t20c"]["state"], "tracking");

    // get_time confirms
    let resp_a = call(&dir, "handoff_timer_get_time", json!({ "task_id": "t20a" }));
    let state_a: Value = serde_json::from_str(&text(&resp_a)).unwrap();
    assert_eq!(state_a["state"], "tracking");

    let resp_b = call(&dir, "handoff_timer_get_time", json!({ "task_id": "t20b" }));
    let state_b: Value = serde_json::from_str(&text(&resp_b)).unwrap();
    assert_eq!(state_b["state"], "stopped");

    // Cleanup
    call(&dir, "handoff_timer_stop", json!({ "task_id": "t20a" }));
    call(&dir, "handoff_timer_stop", json!({ "task_id": "t20c" }));
}

// ============================================================
// Adversarial E2E: timer_provider=mcp ignores VSCode authority
// ============================================================

#[test]
fn mcp_provider_ignores_vscode_authority() {
    let dir = setup_project();
    create_task(&dir, "t21");

    // Force timer_provider = mcp
    call(
        &dir,
        "handoff_update_config",
        json!({ "updates": { "settings.timer_provider": "mcp" } }),
    );

    // Write a live VSCode authority (would normally cause delegation)
    let timer_dir = dir.path().join(".handoff/timer");
    fs::create_dir_all(timer_dir.join("requests")).unwrap();
    let auth = json!({
        "version": 1,
        "owner": "vscode",
        "owner_instance": "55555",
        "heartbeat_at": chrono::Utc::now().to_rfc3339(),
        "ttl_secs": 30,
        "updated_at": chrono::Utc::now().to_rfc3339()
    });
    fs::write(
        timer_dir.join("authority.json"),
        serde_json::to_string_pretty(&auth).unwrap(),
    )
    .unwrap();

    // Start should use MCP fallback despite live VSCode authority
    let resp = call(&dir, "handoff_timer_start", json!({ "task_id": "t21" }));
    assert!(!is_error(&resp), "start error: {}", text(&resp));
    let msg = text(&resp);
    assert!(
        msg.contains("MCP fallback"),
        "should use MCP fallback when provider=mcp: {msg}"
    );

    // Cleanup
    call(&dir, "handoff_timer_stop", json!({ "task_id": "t21" }));
}

// ============================================================
// Adversarial E2E: get_time with provider=off rejects
// ============================================================

#[test]
fn get_time_with_provider_off_rejects() {
    let dir = setup_project();
    create_task(&dir, "t22");

    call(
        &dir,
        "handoff_update_config",
        json!({ "updates": { "settings.timer_provider": "off" } }),
    );

    let resp = call(&dir, "handoff_timer_get_time", json!({ "task_id": "t22" }));
    assert!(is_error(&resp), "get_time with off should be error");
    assert!(text(&resp).contains("disabled"), "should mention disabled");
}

// ============================================================
// Adversarial E2E: stop→re-start creates fresh timer
// ============================================================

#[test]
fn stop_then_restart_creates_fresh_timer() {
    let dir = setup_project();
    create_task(&dir, "t23");

    // First cycle: start → stop
    call(&dir, "handoff_timer_start", json!({ "task_id": "t23" }));
    call(&dir, "handoff_timer_stop", json!({ "task_id": "t23" }));

    // Second start should succeed (not "already running")
    let resp = call(&dir, "handoff_timer_start", json!({ "task_id": "t23" }));
    assert!(!is_error(&resp), "re-start error: {}", text(&resp));
    let msg = text(&resp);
    assert!(
        msg.contains("MCP fallback timer started"),
        "should start fresh: {msg}"
    );

    // Timer should be tracking again
    let resp = call(&dir, "handoff_timer_get_time", json!({ "task_id": "t23" }));
    let state: Value = serde_json::from_str(&text(&resp)).unwrap();
    assert_eq!(state["state"], "tracking");

    // Cleanup
    call(&dir, "handoff_timer_stop", json!({ "task_id": "t23" }));
}

// ============================================================
// Adversarial E2E: corrupted state.json — start recovers
// ============================================================

#[test]
fn corrupted_state_json_handled_gracefully() {
    let dir = setup_project();
    create_task(&dir, "t24");

    // First create a valid timer dir
    let timer_dir = dir.path().join(".handoff/timer");
    fs::create_dir_all(timer_dir.join("requests")).unwrap();

    // Write corrupted state.json
    fs::write(timer_dir.join("state.json"), "{ not valid json !!!").unwrap();

    // Start should fail with a parse error (not panic/crash)
    let resp = call(&dir, "handoff_timer_start", json!({ "task_id": "t24" }));
    assert!(is_error(&resp), "corrupted state should cause error");
    let msg = text(&resp);
    assert!(
        msg.contains("parse") || msg.contains("state"),
        "error should mention parse/state: {msg}"
    );
}

// ============================================================
// Adversarial E2E: clock skew — started_at in the future
// keeps elapsed_ms at 0 (no underflow)
// ============================================================

#[test]
fn future_started_at_does_not_underflow() {
    let dir = setup_project();
    create_task(&dir, "t25");

    // Start timer normally
    call(&dir, "handoff_timer_start", json!({ "task_id": "t25" }));

    // Manually overwrite state.json with started_at 1 hour in the future
    let timer_dir = dir.path().join(".handoff/timer");
    let future_time = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let state = json!({
        "version": 1,
        "owner": "mcp",
        "timers": {
            "t25": {
                "state": "tracking",
                "elapsed_ms": 0,
                "started_at": future_time,
                "paused_by_idle": false,
                "base_hours": 0.0
            }
        },
        "updated_at": chrono::Utc::now().to_rfc3339()
    });
    fs::write(
        timer_dir.join("state.json"),
        serde_json::to_string_pretty(&state).unwrap(),
    )
    .unwrap();

    // get_time should return 0 elapsed (not overflow/negative)
    let resp = call(&dir, "handoff_timer_get_time", json!({ "task_id": "t25" }));
    assert!(!is_error(&resp), "get_time error: {}", text(&resp));
    let result: Value = serde_json::from_str(&text(&resp)).unwrap();
    assert_eq!(result["state"], "tracking");
    assert_eq!(
        result["elapsed_ms"], 0,
        "future started_at should clamp to 0"
    );

    // Stop should also work safely (logging ~0 hours)
    let resp = call(&dir, "handoff_timer_stop", json!({ "task_id": "t25" }));
    assert!(!is_error(&resp), "stop error: {}", text(&resp));
}

// ============================================================
// Adversarial E2E: start with missing task_id parameter
// ============================================================

#[test]
fn start_missing_task_id_returns_error() {
    let dir = setup_project();

    let resp = call(&dir, "handoff_timer_start", json!({}));
    assert!(is_error(&resp), "missing task_id should be error");
    assert!(
        text(&resp).contains("task_id"),
        "should mention task_id: {}",
        text(&resp)
    );
}

// ============================================================
// Adversarial E2E: stop with missing task_id parameter
// ============================================================

#[test]
fn stop_missing_task_id_returns_error() {
    let dir = setup_project();

    let resp = call(&dir, "handoff_timer_stop", json!({}));
    assert!(is_error(&resp), "missing task_id should be error");
    assert!(
        text(&resp).contains("task_id"),
        "should mention task_id: {}",
        text(&resp)
    );
}

// ============================================================
// Adversarial E2E: get_time with missing task_id parameter
// ============================================================

#[test]
fn get_time_missing_task_id_returns_error() {
    let dir = setup_project();

    let resp = call(&dir, "handoff_timer_get_time", json!({}));
    assert!(is_error(&resp), "missing task_id should be error");
    assert!(
        text(&resp).contains("task_id"),
        "should mention task_id: {}",
        text(&resp)
    );
}

// ============================================================
// Adversarial E2E: corrupted authority.json handled gracefully
// ============================================================

#[test]
fn corrupted_authority_json_handled_gracefully() {
    let dir = setup_project();
    create_task(&dir, "t30");

    let timer_dir = dir.path().join(".handoff/timer");
    fs::create_dir_all(timer_dir.join("requests")).unwrap();
    fs::write(timer_dir.join("authority.json"), "{ broken json !!!").unwrap();

    let resp = call(&dir, "handoff_timer_start", json!({ "task_id": "t30" }));
    assert!(is_error(&resp), "corrupted authority should cause error");
    let msg = text(&resp);
    assert!(
        msg.contains("parse") || msg.contains("authority"),
        "error should mention parse/authority: {msg}"
    );
}

// ============================================================
// Adversarial E2E: empty string task_id is rejected
// ============================================================

#[test]
fn empty_task_id_is_rejected() {
    let dir = setup_project();

    let resp = call(&dir, "handoff_timer_start", json!({ "task_id": "" }));
    assert!(is_error(&resp), "empty task_id should be error");

    let resp = call(&dir, "handoff_timer_stop", json!({ "task_id": "" }));
    assert!(is_error(&resp), "empty task_id should be error");

    let resp = call(&dir, "handoff_timer_get_time", json!({ "task_id": "" }));
    assert!(is_error(&resp), "empty task_id should be error");
}

// ============================================================
// Adversarial E2E: state.json deleted while timer is tracking
// — stop should return "no active timer" error, not panic
// ============================================================

#[test]
fn stop_after_state_json_deleted_returns_error() {
    let dir = setup_project();
    create_task(&dir, "t31");

    call(&dir, "handoff_timer_start", json!({ "task_id": "t31" }));

    // Delete state.json while timer is running
    let state_path = dir.path().join(".handoff/timer/state.json");
    assert!(state_path.exists(), "state.json should exist after start");
    fs::remove_file(&state_path).unwrap();

    // Stop should fail gracefully (fresh state has no timer entry)
    let resp = call(&dir, "handoff_timer_stop", json!({ "task_id": "t31" }));
    assert!(is_error(&resp), "stop with deleted state should be error");
    assert!(
        text(&resp).contains("No active timer"),
        "should say no active timer: {}",
        text(&resp)
    );
}

// ############################################################
// Cross E2E: MCP × VSCode extension simultaneous operation
// ############################################################
//
// These tests simulate the full cross-process protocol between
// MCP server and VSCode extension via the shared .handoff/timer/
// filesystem. "VSCode" behavior is simulated at the file level
// (authority heartbeat, request ack, state mirror) — the same
// protocol the real extension implements in timerChannel.ts.

// ============================================================
// Cross E2E #1: VSCode alive → MCP delegates via requests/
// → VSCode acks (deletes request) and mirrors state
// ============================================================

#[test]
fn cross_e2e_vscode_alive_mcp_delegates_and_vscode_acks() {
    let dir = setup_project();
    create_task(&dir, "tc1");

    let timer_dir = dir.path().join(".handoff/timer");
    fs::create_dir_all(timer_dir.join("requests")).unwrap();

    // Simulate VSCode writing a live authority
    let now = chrono::Utc::now();
    let auth = json!({
        "version": 1,
        "owner": "vscode",
        "owner_instance": "vscode-pid-1234",
        "heartbeat_at": now.to_rfc3339(),
        "ttl_secs": 30,
        "updated_at": now.to_rfc3339()
    });
    fs::write(
        timer_dir.join("authority.json"),
        serde_json::to_string_pretty(&auth).unwrap(),
    )
    .unwrap();

    // MCP: timer_start → should delegate to VSCode
    let resp = call(&dir, "handoff_timer_start", json!({ "task_id": "tc1" }));
    assert!(!is_error(&resp), "start error: {}", text(&resp));
    let msg = text(&resp);
    assert!(
        msg.contains("delegated to VSCode"),
        "should delegate: {msg}"
    );

    // Verify: request file was created in requests/
    let requests: Vec<_> = fs::read_dir(timer_dir.join("requests"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .collect();
    assert_eq!(requests.len(), 1, "should have 1 request file");

    let req_path = requests[0].path();
    let req: Value = serde_json::from_str(&fs::read_to_string(&req_path).unwrap()).unwrap();
    assert_eq!(req["cmd"], "start");
    assert_eq!(req["task_id"], "tc1");
    assert_eq!(req["issued_by"], "mcp");

    // Simulate VSCode: process the request (start tracking) and ack (delete file)
    fs::remove_file(&req_path).unwrap();

    // Simulate VSCode: write state.json mirroring its internal timer
    let vscode_state = json!({
        "version": 1,
        "owner": "vscode",
        "timers": {
            "tc1": {
                "state": "tracking",
                "elapsed_ms": 5000,
                "started_at": now.to_rfc3339(),
                "paused_by_idle": false,
                "base_hours": 0.0
            }
        },
        "updated_at": chrono::Utc::now().to_rfc3339()
    });
    fs::write(
        timer_dir.join("state.json"),
        serde_json::to_string_pretty(&vscode_state).unwrap(),
    )
    .unwrap();

    // MCP: timer_get_time → should read VSCode's state.json
    let resp = call(&dir, "handoff_timer_get_time", json!({ "task_id": "tc1" }));
    assert!(!is_error(&resp), "get_time error: {}", text(&resp));
    let state: Value = serde_json::from_str(&text(&resp)).unwrap();
    assert_eq!(state["state"], "tracking");
    assert_eq!(state["authority"]["owner"], "vscode");
    assert_eq!(state["authority"]["alive"], true);

    // MCP: timer_stop → should delegate to VSCode (not stop internally)
    let resp = call(&dir, "handoff_timer_stop", json!({ "task_id": "tc1" }));
    assert!(!is_error(&resp), "stop error: {}", text(&resp));
    let msg = text(&resp);
    assert!(
        msg.contains("delegated to VSCode"),
        "stop should also delegate: {msg}"
    );

    // Verify: stop request was created
    let stop_requests: Vec<_> = fs::read_dir(timer_dir.join("requests"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .collect();
    assert_eq!(stop_requests.len(), 1, "should have 1 stop request file");
    let stop_req: Value =
        serde_json::from_str(&fs::read_to_string(stop_requests[0].path()).unwrap()).unwrap();
    assert_eq!(stop_req["cmd"], "stop");
    assert_eq!(stop_req["task_id"], "tc1");
}

// ============================================================
// Cross E2E #2: VSCode absent → MCP fallback tracks time →
// stop adds actual_hours correctly
// ============================================================

#[test]
fn cross_e2e_vscode_absent_mcp_fallback_logs_hours() {
    let dir = setup_project();
    create_task(&dir, "tc2");

    // No authority.json exists → MCP should fall back

    // Start fallback timer
    let resp = call(&dir, "handoff_timer_start", json!({ "task_id": "tc2" }));
    assert!(!is_error(&resp), "start error: {}", text(&resp));
    assert!(
        text(&resp).contains("MCP fallback timer started"),
        "should be fallback: {}",
        text(&resp)
    );

    // Verify authority.json was created with owner=mcp
    let timer_dir = dir.path().join(".handoff/timer");
    let auth: Value =
        serde_json::from_str(&fs::read_to_string(timer_dir.join("authority.json")).unwrap())
            .unwrap();
    assert_eq!(auth["owner"], "mcp");

    // Verify state.json shows tracking
    let state: Value =
        serde_json::from_str(&fs::read_to_string(timer_dir.join("state.json")).unwrap()).unwrap();
    assert_eq!(state["owner"], "mcp");
    assert_eq!(state["timers"]["tc2"]["state"], "tracking");
    assert_eq!(state["timers"]["tc2"]["base_hours"], 0.0);

    // get_time should show authority.owner=mcp
    let resp = call(&dir, "handoff_timer_get_time", json!({ "task_id": "tc2" }));
    let gt: Value = serde_json::from_str(&text(&resp)).unwrap();
    assert_eq!(gt["authority"]["owner"], "mcp");
    assert_eq!(gt["authority"]["alive"], true);

    // Stop — should add hours to actual_hours
    let resp = call(&dir, "handoff_timer_stop", json!({ "task_id": "tc2" }));
    assert!(!is_error(&resp), "stop error: {}", text(&resp));
    let msg = text(&resp);
    assert!(
        msg.contains("MCP fallback timer stopped"),
        "should be fallback stop: {msg}"
    );
    assert!(msg.contains("logged"), "should mention logged hours: {msg}");

    // Verify actual_hours was updated on task
    let task_resp = call(&dir, "handoff_get_task", json!({ "task_id": "tc2" }));
    let task: Value = serde_json::from_str(&text(&task_resp)).unwrap();
    let actual = task["schedule"]["actual_hours"].as_f64().unwrap();
    assert!(actual >= 0.0, "actual_hours should be set (>=0): {actual}");

    // No request files should exist (MCP handled internally)
    let requests_dir = timer_dir.join("requests");
    if requests_dir.exists() {
        let count = fs::read_dir(&requests_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
            .count();
        assert_eq!(count, 0, "fallback should not create request files");
    }
}

// ============================================================
// Cross E2E #3: MCP fallback running → VSCode starts up and
// claims authority → MCP switches to delegation on next call
// → no double-counting
// ============================================================

#[test]
fn cross_e2e_fallback_to_vscode_handoff_no_double_count() {
    let dir = setup_project();
    create_task(&dir, "tc3");

    // Phase 1: No VSCode → MCP fallback starts
    let resp = call(&dir, "handoff_timer_start", json!({ "task_id": "tc3" }));
    assert!(!is_error(&resp));
    assert!(text(&resp).contains("MCP fallback timer started"));

    let timer_dir = dir.path().join(".handoff/timer");

    // Verify MCP owns authority
    let auth1: Value =
        serde_json::from_str(&fs::read_to_string(timer_dir.join("authority.json")).unwrap())
            .unwrap();
    assert_eq!(auth1["owner"], "mcp");

    // Read MCP's state.json to capture the base_hours snapshot
    let state1: Value =
        serde_json::from_str(&fs::read_to_string(timer_dir.join("state.json")).unwrap()).unwrap();
    assert_eq!(state1["timers"]["tc3"]["state"], "tracking");
    // Phase 2: Simulate VSCode starting up
    //
    // Per the spec (§3.2): when VSCode starts and sees owner=mcp + alive,
    // it does NOT immediately start counting. It waits until MCP releases.
    // For this test: MCP stops the timer (flush + release), then VSCode claims.

    // MCP: stop timer → flush elapsed to actual_hours, remove from state
    let resp = call(&dir, "handoff_timer_stop", json!({ "task_id": "tc3" }));
    assert!(!is_error(&resp), "mcp stop error: {}", text(&resp));
    assert!(text(&resp).contains("MCP fallback timer stopped"));

    // Capture actual_hours after MCP flush
    let task_resp = call(&dir, "handoff_get_task", json!({ "task_id": "tc3" }));
    let task: Value = serde_json::from_str(&text(&task_resp)).unwrap();
    let actual_after_mcp = task["schedule"]["actual_hours"].as_f64().unwrap();

    // Phase 3: VSCode takes over authority
    let now = chrono::Utc::now();
    let vscode_auth = json!({
        "version": 1,
        "owner": "vscode",
        "owner_instance": "vscode-pid-5678",
        "heartbeat_at": now.to_rfc3339(),
        "ttl_secs": 30,
        "updated_at": now.to_rfc3339()
    });
    fs::write(
        timer_dir.join("authority.json"),
        serde_json::to_string_pretty(&vscode_auth).unwrap(),
    )
    .unwrap();

    // VSCode writes state.json showing it started tracking tc3
    // The base_hours should be the CURRENT actual_hours (after MCP flush)
    let vscode_state = json!({
        "version": 1,
        "owner": "vscode",
        "timers": {
            "tc3": {
                "state": "tracking",
                "elapsed_ms": 3000,
                "started_at": now.to_rfc3339(),
                "paused_by_idle": false,
                "base_hours": actual_after_mcp
            }
        },
        "updated_at": chrono::Utc::now().to_rfc3339()
    });
    fs::write(
        timer_dir.join("state.json"),
        serde_json::to_string_pretty(&vscode_state).unwrap(),
    )
    .unwrap();

    // Phase 4: MCP's next call should delegate to VSCode
    let resp = call(&dir, "handoff_timer_start", json!({ "task_id": "tc3" }));
    assert!(!is_error(&resp));
    assert!(
        text(&resp).contains("delegated to VSCode"),
        "after handoff, MCP should delegate: {}",
        text(&resp)
    );

    // get_time should read VSCode's state
    let resp = call(&dir, "handoff_timer_get_time", json!({ "task_id": "tc3" }));
    let gt: Value = serde_json::from_str(&text(&resp)).unwrap();
    assert_eq!(gt["authority"]["owner"], "vscode");
    assert_eq!(gt["state"], "tracking");

    // Verify no double counting:
    // VSCode's base_hours == actual_after_mcp (the flush amount)
    // So projected_total = actual_after_mcp + elapsed (3s ≈ 0.0008h)
    // This is NOT actual_after_mcp + mcp_elapsed + vscode_elapsed (double count)
    let base = gt["base_hours"].as_f64().unwrap();
    assert!(
        (base - actual_after_mcp).abs() < 0.001,
        "VSCode base_hours ({base}) should match post-MCP actual_hours ({actual_after_mcp})"
    );
    let current_actual = gt["current_actual_hours"].as_f64().unwrap();
    assert!(
        (current_actual - actual_after_mcp).abs() < 0.01,
        "current_actual ({current_actual}) should be close to post-MCP actual ({actual_after_mcp}) — no double counting"
    );
}

// ============================================================
// Cross E2E #4: Stale VSCode authority → MCP claims fallback →
// later VSCode returns with fresh heartbeat → delegation resumes
// ============================================================

#[test]
fn cross_e2e_stale_vscode_then_recovery() {
    let dir = setup_project();
    create_task(&dir, "tc4");

    let timer_dir = dir.path().join(".handoff/timer");
    fs::create_dir_all(timer_dir.join("requests")).unwrap();

    // Write stale VSCode authority (crashed 120s ago)
    let stale = (chrono::Utc::now() - chrono::Duration::seconds(120)).to_rfc3339();
    let auth = json!({
        "version": 1,
        "owner": "vscode",
        "owner_instance": "crashed-vscode",
        "heartbeat_at": stale,
        "ttl_secs": 30,
        "updated_at": stale
    });
    fs::write(
        timer_dir.join("authority.json"),
        serde_json::to_string_pretty(&auth).unwrap(),
    )
    .unwrap();

    // MCP should detect stale and fall back
    let resp = call(&dir, "handoff_timer_start", json!({ "task_id": "tc4" }));
    assert!(!is_error(&resp));
    assert!(
        text(&resp).contains("MCP fallback"),
        "stale VSCode should trigger fallback: {}",
        text(&resp)
    );

    // MCP is now the authority
    let auth_now: Value =
        serde_json::from_str(&fs::read_to_string(timer_dir.join("authority.json")).unwrap())
            .unwrap();
    assert_eq!(auth_now["owner"], "mcp");

    // Stop MCP timer to flush
    call(&dir, "handoff_timer_stop", json!({ "task_id": "tc4" }));

    // VSCode recovers / restarts — writes fresh authority
    let now = chrono::Utc::now();
    let fresh_auth = json!({
        "version": 1,
        "owner": "vscode",
        "owner_instance": "new-vscode-pid",
        "heartbeat_at": now.to_rfc3339(),
        "ttl_secs": 30,
        "updated_at": now.to_rfc3339()
    });
    fs::write(
        timer_dir.join("authority.json"),
        serde_json::to_string_pretty(&fresh_auth).unwrap(),
    )
    .unwrap();

    // MCP should now delegate
    let resp = call(&dir, "handoff_timer_start", json!({ "task_id": "tc4" }));
    assert!(!is_error(&resp));
    assert!(
        text(&resp).contains("delegated to VSCode"),
        "recovered VSCode should receive delegation: {}",
        text(&resp)
    );
}

// ============================================================
// Cross E2E #5: Concurrent start — both MCP and VSCode try to
// claim authority simultaneously (MCP first, then VSCode overwrites)
// → no crash, next MCP call sees VSCode as authority
// ============================================================

#[test]
fn cross_e2e_concurrent_authority_claim_mcp_then_vscode() {
    let dir = setup_project();
    create_task(&dir, "tc5");

    // MCP starts first (no authority exists) → claims fallback
    let resp = call(&dir, "handoff_timer_start", json!({ "task_id": "tc5" }));
    assert!(!is_error(&resp));
    assert!(text(&resp).contains("MCP fallback timer started"));

    let timer_dir = dir.path().join(".handoff/timer");

    // Simulate VSCode immediately overwriting authority (race won by VSCode)
    let now = chrono::Utc::now();
    let vscode_auth = json!({
        "version": 1,
        "owner": "vscode",
        "owner_instance": "vscode-race-winner",
        "heartbeat_at": now.to_rfc3339(),
        "ttl_secs": 30,
        "updated_at": now.to_rfc3339()
    });
    fs::write(
        timer_dir.join("authority.json"),
        serde_json::to_string_pretty(&vscode_auth).unwrap(),
    )
    .unwrap();

    // MCP's next timer_get_time should see VSCode as authority
    // (it reads state.json which still has MCP's entry)
    let resp = call(&dir, "handoff_timer_get_time", json!({ "task_id": "tc5" }));
    assert!(!is_error(&resp));
    let gt: Value = serde_json::from_str(&text(&resp)).unwrap();
    assert_eq!(gt["authority"]["owner"], "vscode");
    assert_eq!(gt["authority"]["alive"], true);

    // MCP's next timer_stop should delegate to VSCode (not stop internally)
    let resp = call(&dir, "handoff_timer_stop", json!({ "task_id": "tc5" }));
    assert!(!is_error(&resp));
    assert!(
        text(&resp).contains("delegated to VSCode"),
        "after VSCode claims authority, MCP should delegate stop: {}",
        text(&resp)
    );

    // The MCP fallback timer entry is still in state.json (VSCode will clean it)
    // but no actual_hours were double-counted because MCP's stop was delegated
    let task_resp = call(&dir, "handoff_get_task", json!({ "task_id": "tc5" }));
    let task: Value = serde_json::from_str(&text(&task_resp)).unwrap();
    let actual = task["schedule"]
        .get("actual_hours")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    assert!(
        actual < 0.01,
        "MCP should NOT have flushed hours since stop was delegated: {actual}"
    );
}

// ============================================================
// Cross E2E #6: Full lifecycle — MCP fallback → stop/flush →
// VSCode takes over → delegates → verify actual_hours continuity
// ============================================================

#[test]
fn cross_e2e_full_lifecycle_continuity() {
    let dir = setup_project();
    create_task(&dir, "tc6");

    // Log 1.0h baseline
    call(
        &dir,
        "handoff_log_time",
        json!({ "task_id": "tc6", "hours": 1.0 }),
    );

    // Phase 1: MCP fallback (no VSCode)
    let resp = call(&dir, "handoff_timer_start", json!({ "task_id": "tc6" }));
    assert!(!is_error(&resp));
    assert!(text(&resp).contains("MCP fallback timer started"));
    assert!(
        text(&resp).contains("base_hours: 1.00"),
        "base should be 1.0: {}",
        text(&resp)
    );

    // Stop MCP timer
    let resp = call(&dir, "handoff_timer_stop", json!({ "task_id": "tc6" }));
    assert!(!is_error(&resp));
    let msg = text(&resp);
    assert!(msg.contains("MCP fallback timer stopped"));

    // Verify actual_hours is ~1.0 (original + tiny MCP elapsed)
    let task_resp = call(&dir, "handoff_get_task", json!({ "task_id": "tc6" }));
    let task: Value = serde_json::from_str(&text(&task_resp)).unwrap();
    let actual_after_phase1 = task["schedule"]["actual_hours"].as_f64().unwrap();
    assert!(
        (1.0..1.1).contains(&actual_after_phase1),
        "after MCP phase, actual should be ~1.0: {actual_after_phase1}"
    );

    // Phase 2: VSCode takes over
    let timer_dir = dir.path().join(".handoff/timer");
    let now = chrono::Utc::now();
    let auth = json!({
        "version": 1,
        "owner": "vscode",
        "owner_instance": "vscode-lifecycle",
        "heartbeat_at": now.to_rfc3339(),
        "ttl_secs": 30,
        "updated_at": now.to_rfc3339()
    });
    fs::write(
        timer_dir.join("authority.json"),
        serde_json::to_string_pretty(&auth).unwrap(),
    )
    .unwrap();

    // Simulate VSCode tracking for ~30 minutes (1800000ms)
    let vscode_state = json!({
        "version": 1,
        "owner": "vscode",
        "timers": {
            "tc6": {
                "state": "tracking",
                "elapsed_ms": 1800000,
                "started_at": now.to_rfc3339(),
                "paused_by_idle": false,
                "base_hours": actual_after_phase1
            }
        },
        "updated_at": chrono::Utc::now().to_rfc3339()
    });
    fs::write(
        timer_dir.join("state.json"),
        serde_json::to_string_pretty(&vscode_state).unwrap(),
    )
    .unwrap();

    // MCP: get_time should show continuous accumulation
    let resp = call(&dir, "handoff_timer_get_time", json!({ "task_id": "tc6" }));
    let gt: Value = serde_json::from_str(&text(&resp)).unwrap();
    assert_eq!(gt["authority"]["owner"], "vscode");
    assert_eq!(gt["state"], "tracking");

    // projected total should be base (~1.0) + 0.5h (1800000ms) + any live delta
    let projected: f64 = gt["projected_total_hours"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        (1.4..2.0).contains(&projected),
        "projected should be ~1.5h (1.0 base + 0.5h vscode): {projected}"
    );

    // MCP: timer_stop delegates to VSCode
    let resp = call(&dir, "handoff_timer_stop", json!({ "task_id": "tc6" }));
    assert!(!is_error(&resp));
    assert!(text(&resp).contains("delegated to VSCode"));

    // task actual_hours was NOT double-counted by MCP's delegation
    let task_resp2 = call(&dir, "handoff_get_task", json!({ "task_id": "tc6" }));
    let task2: Value = serde_json::from_str(&text(&task_resp2)).unwrap();
    let actual_final = task2["schedule"]["actual_hours"].as_f64().unwrap();
    assert!(
        (actual_final - actual_after_phase1).abs() < 0.01,
        "MCP delegation should not add hours: actual_final={actual_final}, after_phase1={actual_after_phase1}"
    );
}
