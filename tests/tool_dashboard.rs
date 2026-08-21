use serde_json::{json, Value};
use std::fs;
use tempfile::TempDir;

/// `handoff_load_context` registers this *process's* agent identity in a
/// single process-wide global. Test binaries run `#[test]`s concurrently in
/// one process, so any two tests that both call `handoff_load_context` race
/// on that global. Serialize the handful of tests below that call it. See
/// the identical comment in `tests/tool_claim_release.rs`.
static AGENT_ID_GLOBAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn send(input: &str) -> Option<Value> {
    let result = handoff_mcp::mcp::protocol::process_line(input)?;
    Some(serde_json::from_str(&result).expect("response should be valid JSON"))
}

fn call_tool(name: &str, arguments: Value) -> Value {
    let req = json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments }
    });
    send(&req.to_string()).unwrap()
}

fn get_text(resp: &Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"].as_bool().unwrap_or(false)
}

fn init_project(base: &std::path::Path, name: &str) {
    let project_dir = base.join(name);
    fs::create_dir_all(&project_dir).unwrap();

    let req = json!({
        "jsonrpc": "2.0", "id": 0,
        "method": "tools/call",
        "params": {
            "name": "handoff_init",
            "arguments": {
                "project_dir": project_dir.to_string_lossy(),
                "project_name": name
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
                "project_dir": project_dir.to_string_lossy(),
                "updates": { "settings.require_estimate_hours": false }
            }
        }
    });
    send(&cfg.to_string()).unwrap();
}

fn setup_scan_dir() -> TempDir {
    tempfile::tempdir().expect("failed to create temp dir")
}

#[test]
fn dashboard_multiple_projects() {
    let scan_dir = setup_scan_dir();

    init_project(scan_dir.path(), "project-a");
    init_project(scan_dir.path(), "project-b");

    let resp = call_tool(
        "handoff_dashboard",
        json!({ "scan_dirs": [scan_dir.path().to_string_lossy()] }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    let projects = parsed["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 2);

    let names: Vec<&str> = projects
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"project-a"));
    assert!(names.contains(&"project-b"));
}

#[test]
fn dashboard_skips_non_handoff_dirs() {
    let scan_dir = setup_scan_dir();

    init_project(scan_dir.path(), "real-project");
    fs::create_dir_all(scan_dir.path().join("not-a-project")).unwrap();

    let resp = call_tool(
        "handoff_dashboard",
        json!({ "scan_dirs": [scan_dir.path().to_string_lossy()] }),
    );

    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    let projects = parsed["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 1);
}

#[test]
fn dashboard_with_tasks() {
    let scan_dir = setup_scan_dir();
    init_project(scan_dir.path(), "proj-tasks");

    let pd = scan_dir
        .path()
        .join("proj-tasks")
        .to_string_lossy()
        .to_string();

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "title": "Task 1", "status": "in_progress" }
        }),
    );
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "title": "Task 2", "status": "blocked", "notes": "Waiting" }
        }),
    );

    let resp = call_tool(
        "handoff_dashboard",
        json!({ "scan_dirs": [scan_dir.path().to_string_lossy()] }),
    );

    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    assert_eq!(parsed["total_active_tasks"], 1);
    assert_eq!(parsed["total_blocked"], 1);

    let proj = &parsed["projects"][0];
    assert_eq!(proj["active_tasks"], 1);
    assert_eq!(proj["blocked_tasks"], 1);
}

#[test]
fn dashboard_empty_scan_dir() {
    let scan_dir = setup_scan_dir();

    let resp = call_tool(
        "handoff_dashboard",
        json!({ "scan_dirs": [scan_dir.path().to_string_lossy()] }),
    );

    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    assert!(parsed["projects"].as_array().unwrap().is_empty());
    assert_eq!(parsed["total_active_tasks"], 0);
}

#[test]
fn dashboard_nonexistent_scan_dir() {
    let resp = call_tool(
        "handoff_dashboard",
        json!({ "scan_dirs": ["/nonexistent/path/that/does/not/exist"] }),
    );

    assert!(!is_error(&resp));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    assert!(parsed["projects"].as_array().unwrap().is_empty());
}

#[test]
fn dashboard_discovers_nested_project_recursively() {
    let scan_dir = setup_scan_dir();

    // parent/child both have .handoff/
    init_project(scan_dir.path(), "parent");
    let child_dir = scan_dir.path().join("parent").join("child");
    fs::create_dir_all(&child_dir).unwrap();
    init_project(&scan_dir.path().join("parent"), "child");

    let resp = call_tool(
        "handoff_dashboard",
        json!({ "scan_dirs": [scan_dir.path().to_string_lossy()] }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    let projects = parsed["projects"].as_array().unwrap();
    let names: Vec<&str> = projects
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"parent"), "names: {names:?}");
    assert!(names.contains(&"child"), "names: {names:?}");
}

#[test]
fn dashboard_max_depth_limits_recursion() {
    let scan_dir = setup_scan_dir();

    // level1/level2/level3, each a project directory, nested 3 deep.
    init_project(scan_dir.path(), "level1");
    let level2_parent = scan_dir.path().join("level1");
    init_project(&level2_parent, "level2");
    let level3_parent = level2_parent.join("level2");
    init_project(&level3_parent, "level3");

    // max_depth=1: only immediate children of scan_dir (level1) should be found.
    let resp = call_tool(
        "handoff_dashboard",
        json!({ "scan_dirs": [scan_dir.path().to_string_lossy()], "max_depth": 1 }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    let projects = parsed["projects"].as_array().unwrap();
    let names: Vec<&str> = projects
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();

    assert_eq!(names, vec!["level1"], "names: {names:?}");
}

#[test]
fn dashboard_exclude_patterns_skips_directory_by_name() {
    let scan_dir = setup_scan_dir();

    init_project(scan_dir.path(), "real-project");
    // Nested project *inside* node_modules — should be excluded from recursion entirely,
    // not merely absent because it's below depth 1.
    let node_modules = scan_dir.path().join("node_modules");
    fs::create_dir_all(&node_modules).unwrap();
    init_project(&node_modules, "some-dep");

    let resp = call_tool(
        "handoff_dashboard",
        json!({
            "scan_dirs": [scan_dir.path().to_string_lossy()],
            "exclude_patterns": ["node_modules"],
            "max_depth": 5
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    let projects = parsed["projects"].as_array().unwrap();
    let names: Vec<&str> = projects
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();

    assert!(names.contains(&"real-project"), "names: {names:?}");
    assert!(!names.contains(&"some-dep"), "names: {names:?}");
    assert_eq!(projects.len(), 1);
}

#[test]
fn dashboard_does_not_descend_into_handoff_internal_dir() {
    let scan_dir = setup_scan_dir();

    init_project(scan_dir.path(), "real-project");

    // Simulate a decoy "project" marker nested inside the discovered project's
    // own .handoff/ bookkeeping tree (e.g. under tasks/<id>/). scan_recursive
    // must not descend into `.handoff/` at all, so this decoy should never be
    // reached/reported as a separate project.
    let decoy_dir = scan_dir
        .path()
        .join("real-project")
        .join(".handoff")
        .join("tasks")
        .join("some-task-id")
        .join(".handoff");
    fs::create_dir_all(&decoy_dir).unwrap();
    fs::write(
        decoy_dir.join("config.toml"),
        "[project]\nname = \"decoy\"\n",
    )
    .unwrap();

    let resp = call_tool(
        "handoff_dashboard",
        json!({ "scan_dirs": [scan_dir.path().to_string_lossy()], "max_depth": 5 }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    let projects = parsed["projects"].as_array().unwrap();
    let names: Vec<&str> = projects
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();

    assert!(names.contains(&"real-project"), "names: {names:?}");
    assert!(!names.contains(&"decoy"), "names: {names:?}");
    assert_eq!(projects.len(), 1, "names: {names:?}");
}

#[test]
fn dashboard_honors_child_project_exclude_patterns_in_umbrella_topology() {
    // Standard default topology: scan_dirs = ["~/pro/"]-like umbrella dir that
    // itself has no .handoff/config.toml — only its child projects do. A
    // child project's own dashboard.exclude_patterns should still take effect
    // when no tool-argument override is given.
    let scan_dir = setup_scan_dir();

    init_project(scan_dir.path(), "child-with-config");
    let child_dir = scan_dir.path().join("child-with-config");
    let cfg = json!({
        "jsonrpc": "2.0", "id": 0,
        "method": "tools/call",
        "params": {
            "name": "handoff_update_config",
            "arguments": {
                "project_dir": child_dir.to_string_lossy(),
                "updates": { "dashboard.exclude_patterns": ["vendored"] }
            }
        }
    });
    send(&cfg.to_string()).unwrap();

    let vendored = scan_dir.path().join("vendored");
    fs::create_dir_all(&vendored).unwrap();
    init_project(&vendored, "vendored-project");

    // No max_depth/exclude_patterns tool argument: the child project's own
    // config should be discovered and its exclude_patterns applied.
    let resp = call_tool(
        "handoff_dashboard",
        json!({ "scan_dirs": [scan_dir.path().to_string_lossy()] }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    let projects = parsed["projects"].as_array().unwrap();
    let names: Vec<&str> = projects
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();

    assert!(names.contains(&"child-with-config"), "names: {names:?}");
    assert!(
        !names.contains(&"vendored-project"),
        "expected child project's exclude_patterns to be honored: names: {names:?}"
    );
}

#[test]
fn dashboard_scoped_config_fallback_does_not_leak_across_scan_dirs() {
    // Two independent umbrella scan_dirs. dirA has a child project with its own
    // dashboard.exclude_patterns = ["special"]. dirB has an unrelated project
    // literally named "special" that has no config overrides of its own.
    // dirB's "special" project must NOT be excluded just because dirA's child
    // config happened to be discovered first — config fallback must be scoped
    // per scan_dir, not applied globally across all scan_dirs.
    let root = setup_scan_dir();
    let dir_a = root.path().join("dirA");
    let dir_b = root.path().join("dirB");
    fs::create_dir_all(&dir_a).unwrap();
    fs::create_dir_all(&dir_b).unwrap();

    init_project(&dir_a, "child-with-config");
    let child_dir = dir_a.join("child-with-config");
    let cfg = json!({
        "jsonrpc": "2.0", "id": 0,
        "method": "tools/call",
        "params": {
            "name": "handoff_update_config",
            "arguments": {
                "project_dir": child_dir.to_string_lossy(),
                "updates": { "dashboard.exclude_patterns": ["special"] }
            }
        }
    });
    send(&cfg.to_string()).unwrap();

    // dirB contains an unrelated project literally named "special" with no
    // config overrides of its own.
    init_project(&dir_b, "special");

    let resp = call_tool(
        "handoff_dashboard",
        json!({
            "scan_dirs": [
                dir_a.to_string_lossy(),
                dir_b.to_string_lossy(),
            ]
        }),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();
    let projects = parsed["projects"].as_array().unwrap();
    let names: Vec<&str> = projects
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();

    assert!(names.contains(&"child-with-config"), "names: {names:?}");
    assert!(
        names.contains(&"special"),
        "dirB's unrelated 'special' project must not be dropped due to dirA's \
         child config leaking across scan_dirs: names: {names:?}"
    );
}

#[test]
fn dashboard_unlocked_tasks_have_no_claim_fields_regression() {
    // Backward compatibility (t240.11 done_criteria #3): a task with no lock
    // must render exactly as before — no claimed_by/lease_remaining/worktree
    // keys, and no warnings section entries.
    let scan_dir = setup_scan_dir();
    init_project(scan_dir.path(), "proj-unlocked");
    let pd = scan_dir
        .path()
        .join("proj-unlocked")
        .to_string_lossy()
        .to_string();

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &pd,
            "task": { "title": "Plain task", "status": "todo" }
        }),
    );

    let resp = call_tool(
        "handoff_dashboard",
        json!({ "scan_dirs": [scan_dir.path().to_string_lossy()] }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let proj = &parsed["projects"][0];

    let tasks = proj["tasks"]
        .as_array()
        .expect("dashboard should expose a tasks array per project");
    assert_eq!(tasks.len(), 1);
    let task = &tasks[0];
    assert!(
        task.get("claimed_by").is_none(),
        "unlocked task must not carry claimed_by: {task}"
    );
    assert!(
        task.get("lease_remaining").is_none(),
        "unlocked task must not carry lease_remaining: {task}"
    );
    assert!(
        task.get("worktree").is_none(),
        "unlocked task must not carry worktree: {task}"
    );

    let warnings = proj["warnings"].as_array();
    assert!(
        warnings.is_none_or(|w| w.is_empty()),
        "no warnings expected for an unlocked task: {warnings:?}"
    );
}

#[test]
fn dashboard_shows_claim_state_for_locked_task() {
    let scan_dir = setup_scan_dir();
    init_project(scan_dir.path(), "proj-claim");
    let pd = scan_dir
        .path()
        .join("proj-claim")
        .to_string_lossy()
        .to_string();

    let create = call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "Claim me", "status": "todo" } }),
    );
    let task_id = get_text(&create)
        .split_whitespace()
        .nth(2)
        .unwrap()
        .trim_end_matches(':')
        .to_string();

    let claim = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": &pd, "task_id": task_id, "session_id": "s-claim" }),
    );
    assert!(!is_error(&claim), "error: {}", get_text(&claim));

    let resp = call_tool(
        "handoff_dashboard",
        json!({ "scan_dirs": [scan_dir.path().to_string_lossy()] }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let proj = &parsed["projects"][0];
    let tasks = proj["tasks"].as_array().unwrap();
    let task = tasks
        .iter()
        .find(|t| t["id"] == task_id)
        .expect("claimed task should be present");

    assert_eq!(task["claimed_by"], "unknown");
    assert!(
        task["lease_remaining"].is_string(),
        "expected a human-readable lease_remaining string: {task}"
    );
    assert_ne!(task["lease_remaining"], "expired");
}

#[test]
fn dashboard_warns_on_expired_lease() {
    let scan_dir = setup_scan_dir();
    init_project(scan_dir.path(), "proj-expired");
    let pd = scan_dir
        .path()
        .join("proj-expired")
        .to_string_lossy()
        .to_string();

    let create = call_tool(
        "handoff_update_task",
        json!({ "project_dir": &pd, "task": { "title": "Expire me", "status": "todo" } }),
    );
    let task_id = get_text(&create)
        .split_whitespace()
        .nth(2)
        .unwrap()
        .trim_end_matches(':')
        .to_string();

    // lease_ttl=0 means the lease is already expired by the time we read it,
    // without needing to sleep in the test.
    let claim = call_tool(
        "handoff_claim_task",
        json!({ "project_dir": &pd, "task_id": task_id, "lease_ttl": 0 }),
    );
    assert!(!is_error(&claim), "error: {}", get_text(&claim));

    let resp = call_tool(
        "handoff_dashboard",
        json!({ "scan_dirs": [scan_dir.path().to_string_lossy()] }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let proj = &parsed["projects"][0];

    let warnings = proj["warnings"]
        .as_array()
        .expect("expired lease should produce a warnings array");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap_or("").contains("LEASE EXPIRED")
                && w.as_str().unwrap_or("").contains(&task_id)),
        "expected a LEASE EXPIRED warning mentioning {task_id}: {warnings:?}"
    );
}

#[test]
fn dashboard_agents_section_present_when_agents_registered() {
    let _agent_id_guard = AGENT_ID_GLOBAL.lock().unwrap_or_else(|e| e.into_inner());

    let scan_dir = setup_scan_dir();
    init_project(scan_dir.path(), "proj-agents");
    let pd = scan_dir
        .path()
        .join("proj-agents")
        .to_string_lossy()
        .to_string();

    // handoff_load_context registers this process as an agent under
    // .handoff/agents/<agent-id>.json.
    let load = call_tool("handoff_load_context", json!({ "project_dir": &pd }));
    assert!(!is_error(&load), "error: {}", get_text(&load));

    let resp = call_tool(
        "handoff_dashboard",
        json!({ "scan_dirs": [scan_dir.path().to_string_lossy()] }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let proj = &parsed["projects"][0];

    let agents = proj["agents"]
        .as_array()
        .expect("expected an agents array once an agent has registered");
    assert_eq!(agents.len(), 1, "agents: {agents:?}");
    assert_eq!(agents[0]["status"], "active");
    assert!(agents[0]["worktree"].is_string());
}

#[test]
fn dashboard_no_agents_dir_is_backward_compatible() {
    // A project that has never had an agent register (no .handoff/agents/)
    // must not error and must not require the caller to expect an "agents"
    // key at all (or it may report an empty array — either is acceptable as
    // long as nothing breaks).
    let scan_dir = setup_scan_dir();
    init_project(scan_dir.path(), "proj-no-agents");

    let resp = call_tool(
        "handoff_dashboard",
        json!({ "scan_dirs": [scan_dir.path().to_string_lossy()] }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let proj = &parsed["projects"][0];

    if let Some(agents) = proj["agents"].as_array() {
        assert!(agents.is_empty(), "agents: {agents:?}");
    }
}
