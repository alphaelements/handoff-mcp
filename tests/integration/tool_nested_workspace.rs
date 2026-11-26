use serde_json::{json, Value};
use std::fs;

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

/// Initialize a project as a subdirectory `name` of `base`.
fn init_project(base: &std::path::Path, name: &str) {
    let project_dir = base.join(name);
    init_project_at(&project_dir, name);
}

/// Initialize a project at the exact given path.
fn init_project_at(project_dir: &std::path::Path, name: &str) {
    fs::create_dir_all(project_dir).unwrap();

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

#[test]
fn list_tasks_include_children_false_matches_existing_behavior() {
    let base = tempfile::tempdir().unwrap();
    init_project(base.path(), "parent");
    let parent_dir = base.path().join("parent").to_string_lossy().to_string();
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &parent_dir,
            "task": { "title": "Parent task", "status": "todo" }
        }),
    );

    let child_dir = base.path().join("parent").join("child-app");
    fs::create_dir_all(&child_dir).unwrap();
    init_project_at(&child_dir, "child-app");
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": child_dir.to_string_lossy(),
            "task": { "title": "Child task", "status": "in_progress" }
        }),
    );

    // default: include_children absent
    let resp_default = call_tool("handoff_list_tasks", json!({ "project_dir": &parent_dir }));
    assert!(
        !is_error(&resp_default),
        "error: {}",
        get_text(&resp_default)
    );
    let parsed_default: Value = serde_json::from_str(&get_text(&resp_default)).unwrap();

    // explicit false
    let resp_false = call_tool(
        "handoff_list_tasks",
        json!({ "project_dir": &parent_dir, "include_children": false }),
    );
    let parsed_false: Value = serde_json::from_str(&get_text(&resp_false)).unwrap();

    assert_eq!(parsed_default, parsed_false);

    let tree = parsed_default["task_tree"].as_array().unwrap();
    assert_eq!(tree.len(), 1, "only parent's own task should be present");
    assert_eq!(tree[0]["title"], "Parent task");
    assert!(parsed_default["task_tree"][0].get("project_name").is_none());
    assert_eq!(parsed_default["task_summary"]["total"], 1);
}

#[test]
fn list_tasks_include_children_true_aggregates_child_projects() {
    let base = tempfile::tempdir().unwrap();
    init_project(base.path(), "parent");
    let parent_dir = base.path().join("parent").to_string_lossy().to_string();
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &parent_dir,
            "task": { "title": "Parent task", "status": "todo" }
        }),
    );

    let child_dir = base.path().join("parent").join("child-app");
    fs::create_dir_all(&child_dir).unwrap();
    init_project_at(&child_dir, "child-app");
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": child_dir.to_string_lossy(),
            "task": { "title": "Child task", "status": "in_progress" }
        }),
    );

    let resp = call_tool(
        "handoff_list_tasks",
        json!({ "project_dir": &parent_dir, "include_children": true }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    let parsed: Value = serde_json::from_str(&text).unwrap();

    let tree = parsed["task_tree"].as_array().unwrap();
    // Should have parent's own task plus the child project's task.
    assert_eq!(tree.len(), 2, "tree: {tree:#?}");

    let parent_task = tree
        .iter()
        .find(|t| t["title"] == "Parent task")
        .expect("parent task present");
    assert_eq!(parent_task["project_name"], "parent");

    let child_task = tree
        .iter()
        .find(|t| t["title"] == "Child task")
        .expect("child task present");
    assert_eq!(child_task["project_name"], "child-app");
    assert!(child_task["project_dir"]
        .as_str()
        .unwrap()
        .contains("child-app"));

    // `id` must remain the *raw* task id (unmangled) so it stays directly
    // usable with `handoff_get_task`/`handoff_update_task` when combined with
    // the sibling `project_dir` field, and so that `dependencies` entries
    // (which reference raw ids) keep resolving correctly.
    let child_id = child_task["id"].as_str().unwrap();
    assert!(
        !child_id.contains(':'),
        "id must stay raw/unmangled: {child_id}"
    );

    // A separate composite `task_ref` field provides a globally unique,
    // display-friendly identifier for cross-project disambiguation.
    let child_ref = child_task["task_ref"].as_str().unwrap();
    assert!(
        child_ref.starts_with("child-app-") && child_ref.ends_with(&format!(":{child_id}")),
        "task_ref: {child_ref}"
    );

    // The raw id must actually round-trip through handoff_get_task when
    // paired with project_dir — this is the concrete usability guarantee.
    let child_dir = child_task["project_dir"].as_str().unwrap();
    let get_resp = call_tool(
        "handoff_get_task",
        json!({ "project_dir": child_dir, "task_id": child_id }),
    );
    assert!(
        !is_error(&get_resp),
        "get_task error: {}",
        get_text(&get_resp)
    );
    let get_parsed: Value = serde_json::from_str(&get_text(&get_resp)).unwrap();
    assert_eq!(get_parsed["title"], "Child task");

    assert_eq!(parsed["task_summary"]["total"], 2);
}

#[test]
fn list_tasks_include_children_dedupes_ids_when_project_names_collide() {
    // Two child projects that both use the same `project.name` ("app") must
    // still produce distinct composite task IDs, since project_name alone is
    // free-form user text with no cross-directory uniqueness guarantee.
    let base = tempfile::tempdir().unwrap();
    init_project(base.path(), "parent");
    let parent_dir = base.path().join("parent").to_string_lossy().to_string();

    let child_a_dir = base.path().join("parent").join("service-a");
    fs::create_dir_all(&child_a_dir).unwrap();
    init_project_at(&child_a_dir, "app");
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": child_a_dir.to_string_lossy(),
            "task": { "title": "Task in A", "status": "todo" }
        }),
    );

    let child_b_dir = base.path().join("parent").join("service-b");
    fs::create_dir_all(&child_b_dir).unwrap();
    init_project_at(&child_b_dir, "app");
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": child_b_dir.to_string_lossy(),
            "task": { "title": "Task in B", "status": "todo" }
        }),
    );

    let resp = call_tool(
        "handoff_list_tasks",
        json!({ "project_dir": &parent_dir, "include_children": true }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let tree = parsed["task_tree"].as_array().unwrap();
    assert_eq!(tree.len(), 2, "tree: {tree:#?}");

    // Raw `id`s may legitimately collide across sibling projects (they are
    // only unique within their own project_dir); the composite `task_ref`
    // is what must stay globally unique.
    let refs: Vec<&str> = tree
        .iter()
        .map(|t| t["task_ref"].as_str().unwrap())
        .collect();
    assert_ne!(
        refs[0], refs[1],
        "composite task_ref must be unique across child projects sharing the same project_name: {refs:?}"
    );
}

#[test]
fn list_tasks_include_children_dependencies_stay_raw_and_resolvable() {
    // A child project's task `dependencies` array references another raw
    // task id within that same project. `include_children=true` must not
    // rewrite `id` (which would desync it from `dependencies`) — the raw id
    // must keep resolving via handoff_get_task scoped to the task's own
    // project_dir.
    let base = tempfile::tempdir().unwrap();
    init_project(base.path(), "parent");
    let parent_dir = base.path().join("parent").to_string_lossy().to_string();

    let child_dir = base.path().join("parent").join("child-app");
    fs::create_dir_all(&child_dir).unwrap();
    init_project_at(&child_dir, "child-app");
    let child_dir_str = child_dir.to_string_lossy().to_string();

    let dep_resp = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &child_dir_str,
            "task": { "title": "Dependency task", "status": "done" }
        }),
    );
    // Response is a plain confirmation string: "Created task {id}: {title} [{status}]".
    let dep_resp_text = get_text(&dep_resp);
    let dep_id = dep_resp_text
        .strip_prefix("Created task ")
        .and_then(|rest| rest.split(':').next())
        .expect("response should start with 'Created task {id}:'")
        .to_string();

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": &child_dir_str,
            "task": {
                "title": "Dependent task",
                "status": "todo",
                "dependencies": [dep_id.clone()]
            }
        }),
    );

    let resp = call_tool(
        "handoff_list_tasks",
        json!({ "project_dir": &parent_dir, "include_children": true }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let tree = parsed["task_tree"].as_array().unwrap();

    let dependent = tree
        .iter()
        .find(|t| t["title"] == "Dependent task")
        .expect("dependent task present");
    let deps = dependent["dependencies"].as_array().unwrap();
    assert_eq!(deps.len(), 1);
    // dependencies must remain raw ids (not rewritten to task_ref format).
    assert_eq!(deps[0].as_str().unwrap(), dep_id);

    // The raw id must still resolve within the child project.
    let get_resp = call_tool(
        "handoff_get_task",
        json!({ "project_dir": &child_dir_str, "task_id": deps[0].as_str().unwrap() }),
    );
    assert!(!is_error(&get_resp), "error: {}", get_text(&get_resp));
    let dep_task: Value = serde_json::from_str(&get_text(&get_resp)).unwrap();
    assert_eq!(dep_task["title"], "Dependency task");
}

#[test]
fn load_context_child_projects_empty_when_no_children() {
    let base = tempfile::tempdir().unwrap();
    init_project(base.path(), "solo");
    let dir = base.path().join("solo").to_string_lossy().to_string();

    let resp = call_tool("handoff_load_context", json!({ "project_dir": &dir }));
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let children = parsed["child_projects"].as_array().unwrap();
    assert!(children.is_empty());
}

#[test]
fn load_context_child_projects_discovers_nested() {
    let base = tempfile::tempdir().unwrap();
    init_project(base.path(), "parent");

    let child_dir = base.path().join("parent").join("sub-fw");
    fs::create_dir_all(&child_dir).unwrap();
    init_project_at(&child_dir, "sub-fw");
    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": child_dir.to_string_lossy(),
            "task": { "title": "FW task", "status": "todo" }
        }),
    );

    let resp = call_tool(
        "handoff_load_context",
        json!({ "project_dir": base.path().join("parent").to_string_lossy() }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let parsed: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    let children = parsed["child_projects"].as_array().unwrap();
    assert_eq!(children.len(), 1, "children: {children:#?}");
    assert_eq!(children[0]["name"], "sub-fw");
    assert_eq!(children[0]["task_count"], 1);
    assert!(children[0]["dir"].as_str().unwrap().contains("sub-fw"));
    assert_eq!(children[0]["status_summary"]["todo"], 1);
}
