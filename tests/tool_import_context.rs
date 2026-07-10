use serde_json::{json, Value};
use tempfile::TempDir;

fn send(input: &str) -> Option<Value> {
    let result = handoff_mcp::mcp::protocol::process_line(input)?;
    Some(serde_json::from_str(&result).expect("response should be valid JSON"))
}

fn setup_project() -> TempDir {
    let dir = tempfile::tempdir().expect("failed to create temp dir");

    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let req = json!({
        "jsonrpc": "2.0", "id": 0,
        "method": "tools/call",
        "params": {
            "name": "handoff_init",
            "arguments": {
                "project_dir": dir.path().to_string_lossy(),
                "project_name": "test"
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

#[test]
fn import_source_only() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_import_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "source": {
                "description": "test import",
                "format": "markdown"
            }
        }),
    );
    let text = get_text(&resp);
    assert!(text.contains("Import complete"));
    assert!(text.contains("Tasks created: 0"));
}

#[test]
fn import_flat_tasks() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_import_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "source": { "description": "flat task import" },
            "tasks": [
                { "title": "Task A", "status": "todo" },
                { "title": "Task B", "status": "in_progress", "priority": "high" }
            ]
        }),
    );
    let text = get_text(&resp);
    assert!(text.contains("Tasks created: 2"));
    assert!(text.contains("2 top-level"));
    assert!(text.contains("0 nested"));

    let list_resp = call_tool(
        "handoff_list_tasks",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    let list_text = get_text(&list_resp);
    assert!(list_text.contains("Task A"));
    assert!(list_text.contains("Task B"));
}

#[test]
fn import_nested_tasks() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_import_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "source": { "description": "nested import" },
            "tasks": [
                {
                    "title": "Parent",
                    "status": "in_progress",
                    "children": [
                        { "title": "Child 1", "status": "done" },
                        {
                            "title": "Child 2",
                            "status": "todo",
                            "children": [
                                { "title": "Grandchild", "status": "todo" }
                            ]
                        }
                    ]
                }
            ]
        }),
    );
    let text = get_text(&resp);
    assert!(text.contains("Tasks created: 4"));
    assert!(text.contains("1 top-level"));
    assert!(text.contains("3 nested"));

    let list_resp = call_tool(
        "handoff_list_tasks",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    let list_text = get_text(&list_resp);
    assert!(list_text.contains("Parent"));
    assert!(list_text.contains("Child 1"));
    assert!(list_text.contains("Child 2"));
    assert!(list_text.contains("Grandchild"));
}

#[test]
fn import_with_session() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_import_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "source": { "description": "session import" },
            "session": {
                "summary": "[import] test session",
                "decisions": [
                    { "decision": "Use OAuth2", "reason": "mobile support", "confidence": "confirmed" }
                ],
                "blockers": ["waiting on API key"],
                "handoff_notes": [
                    { "note": "check auth flow", "category": "caution" }
                ]
            }
        }),
    );
    let text = get_text(&resp);
    assert!(text.contains("Session saved: yes"));

    let load_resp = call_tool(
        "handoff_load_context",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    let load_text = get_text(&load_resp);
    assert!(load_text.contains("[import] test session"));
    assert!(load_text.contains("Use OAuth2"));
    assert!(load_text.contains("waiting on API key"));
}

#[test]
fn import_with_raw_notes() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_import_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "source": { "description": "raw notes import" },
            "session": {
                "summary": "[import] with raw notes"
            },
            "raw_notes": "Deploy on Fridays is forbidden\nSSL cert expires 7/1"
        }),
    );
    let text = get_text(&resp);
    assert!(text.contains("Raw notes: saved as handoff_note"));

    let load_resp = call_tool(
        "handoff_load_context",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    let load_text = get_text(&load_resp);
    assert!(load_text.contains("Deploy on Fridays is forbidden"));
}

#[test]
fn import_raw_notes_without_session_creates_auto_session() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_import_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "source": { "description": "auto session test" },
            "raw_notes": "Some unstructured notes"
        }),
    );
    let text = get_text(&resp);
    assert!(text.contains("Session saved: yes"));
    assert!(text.contains("Raw notes: saved as handoff_note"));

    let load_resp = call_tool(
        "handoff_load_context",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    let load_text = get_text(&load_resp);
    assert!(load_text.contains("[import] auto session test"));
    assert!(load_text.contains("Some unstructured notes"));
}

#[test]
fn import_full_scenario() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_import_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "source": {
                "description": "tmp/260601-sprint-handoff.md",
                "format": "markdown"
            },
            "tasks": [
                {
                    "title": "Auth renewal",
                    "status": "in_progress",
                    "priority": "high",
                    "labels": ["auth"],
                    "children": [
                        { "title": "Session migration", "status": "done" },
                        { "title": "PKCE implementation", "status": "in_progress" }
                    ]
                },
                {
                    "title": "CI speedup",
                    "status": "todo",
                    "priority": "medium",
                    "done_criteria": [
                        { "item": "Build time under 5min", "checked": false }
                    ]
                }
            ],
            "session": {
                "summary": "[import] Sprint handoff migration",
                "decisions": [
                    { "decision": "OAuth2 + PKCE", "confidence": "confirmed" }
                ],
                "blockers": ["DB migration window TBD"],
                "references": [
                    { "label": "Original doc", "uri": "tmp/260601-sprint-handoff.md", "type": "doc" }
                ],
                "context_pointers": [
                    { "path": "src/auth/oauth.rs", "reason": "PKCE core" }
                ]
            },
            "raw_notes": "Deploy on Fridays is forbidden"
        }),
    );
    let text = get_text(&resp);
    assert!(text.contains("Tasks created: 4"));
    assert!(text.contains("2 top-level"));
    assert!(text.contains("2 nested"));
    assert!(text.contains("Session saved: yes"));
    assert!(text.contains("Raw notes: saved as handoff_note"));
}

#[test]
fn import_without_source_fails() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_import_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "tasks": [{ "title": "Something" }]
        }),
    );
    let text = get_text(&resp);
    assert!(text.contains("Error"));
    assert!(text.contains("source"));
}

#[test]
fn import_task_without_title_fails() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_import_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "source": { "description": "bad task" },
            "tasks": [{ "status": "todo" }]
        }),
    );
    let text = get_text(&resp);
    assert!(text.contains("Error"));
    assert!(text.contains("title"));
}

#[test]
fn import_preserves_existing_tasks() {
    let dir = setup_project();

    call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "Pre-existing task", "status": "in_progress" }
        }),
    );

    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "source": { "description": "import after existing" },
            "tasks": [
                { "title": "Imported task", "status": "todo" }
            ]
        }),
    );

    let list_resp = call_tool(
        "handoff_list_tasks",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    let list_text = get_text(&list_resp);
    assert!(list_text.contains("Pre-existing task"));
    assert!(list_text.contains("Imported task"));
}

#[test]
fn import_source_recorded_in_environment() {
    let dir = setup_project();
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "source": {
                "description": "my-handoff.md",
                "format": "markdown"
            },
            "session": {
                "summary": "[import] env test"
            }
        }),
    );

    let sessions_dir = dir.path().join(".handoff/sessions");
    let entries: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".open.json"))
        .collect();
    assert_eq!(entries.len(), 1);

    let content = std::fs::read_to_string(entries[0].path()).unwrap();
    let session: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(
        session["environment"]["import_source"]["description"],
        "my-handoff.md"
    );
    assert_eq!(
        session["environment"]["import_source"]["format"],
        "markdown"
    );
}

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"].as_bool().unwrap_or(false)
}

#[test]
fn import_invalid_priority_rejected() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_import_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "source": { "description": "bad priority import" },
            "tasks": [
                { "title": "Task", "priority": "urgent" }
            ]
        }),
    );
    assert!(is_error(&resp));
    let text = get_text(&resp);
    assert!(text.contains("Invalid priority"));
}

#[test]
fn import_valid_priorities_accepted() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_import_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "source": { "description": "valid priorities" },
            "tasks": [
                { "title": "Low", "priority": "low" },
                { "title": "Medium", "priority": "medium" },
                { "title": "High", "priority": "high" }
            ]
        }),
    );
    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    let text = get_text(&resp);
    assert!(text.contains("Tasks created: 3"));
}

// --- import_context honours the estimate_hours requirement (t82) ---

/// Re-enable the estimate requirement, which setup_project() disables by default.
fn enable_estimate_requirement(dir: &TempDir) {
    let resp = call_tool(
        "handoff_update_config",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "updates": { "settings.require_estimate_hours": true }
        }),
    );
    assert!(!is_error(&resp), "config error: {}", get_text(&resp));
}

fn import_one(dir: &TempDir, tasks: Value) -> Value {
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "source": { "description": "estimate rule import" },
            "tasks": tasks
        }),
    )
}

fn list_text(dir: &TempDir) -> String {
    let resp = call_tool(
        "handoff_list_tasks",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    get_text(&resp)
}

#[test]
fn import_rejects_estimateless_leaf_in_required_status() {
    for status in ["todo", "in_progress", "review", "done"] {
        let dir = setup_project();
        enable_estimate_requirement(&dir);

        let resp = import_one(&dir, json!([{ "title": "No estimate", "status": status }]));

        assert!(
            is_error(&resp),
            "status '{status}' must be rejected without an estimate: {}",
            get_text(&resp)
        );
        let text = get_text(&resp);
        assert!(
            text.contains("estimate_hours"),
            "rejection should name estimate_hours for '{status}': {text}"
        );
        // A rejected import must not leave the task behind.
        assert!(
            !list_text(&dir).contains("No estimate"),
            "status '{status}': rejected task must not be written to disk"
        );
    }
}

#[test]
fn import_rejection_example_includes_title_because_import_creates() {
    let dir = setup_project();
    enable_estimate_requirement(&dir);

    let resp = import_one(&dir, json!([{ "title": "No estimate", "status": "todo" }]));

    assert!(is_error(&resp));
    let text = get_text(&resp);
    // import always creates, so the resend example must carry `title` —
    // unlike bulk_update/update_task, where it would imply overwriting it.
    assert!(
        text.contains("\"title\""),
        "a create's resend example must include title: {text}"
    );
    assert!(
        text.contains("\"id\"") && text.contains("estimate_hours"),
        "resend example should show id + title + schedule.estimate_hours: {text}"
    );
}

#[test]
fn import_task_without_status_defaults_to_todo_and_needs_an_estimate() {
    let dir = setup_project();
    enable_estimate_requirement(&dir);

    // No `status` key at all: the default must be a status that requires an
    // estimate, otherwise omitting `status` silently bypasses the rule.
    let resp = import_one(&dir, json!([{ "title": "No status key" }]));

    assert!(
        is_error(&resp),
        "a status-less task defaults to todo and must require an estimate: {}",
        get_text(&resp)
    );
    assert!(get_text(&resp).contains("estimate_hours"));
    assert!(!list_text(&dir).contains("No status key"));
}

#[test]
fn import_accepts_leaf_with_estimate() {
    let dir = setup_project();
    enable_estimate_requirement(&dir);

    let resp = import_one(
        &dir,
        json!([{
            "title": "With estimate",
            "status": "todo",
            "schedule": { "estimate_hours": 2.0 }
        }]),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    assert!(get_text(&resp).contains("Tasks created: 1"));
}

#[test]
fn import_rejects_zero_estimate_leaf() {
    let dir = setup_project();
    enable_estimate_requirement(&dir);

    let resp = import_one(
        &dir,
        json!([{
            "title": "Zero estimate",
            "status": "todo",
            "schedule": { "estimate_hours": 0.0 }
        }]),
    );

    assert!(
        is_error(&resp),
        "estimate_hours must be > 0: {}",
        get_text(&resp)
    );
}

#[test]
fn import_allows_exempt_statuses_without_estimate() {
    for status in ["blocked", "skipped"] {
        let dir = setup_project();
        enable_estimate_requirement(&dir);

        let resp = import_one(&dir, json!([{ "title": "Exempt task", "status": status }]));

        assert!(
            !is_error(&resp),
            "status '{status}' is exempt and must import: {}",
            get_text(&resp)
        );
        assert!(list_text(&dir).contains("Exempt task"));
    }
}

#[test]
fn import_allows_parent_without_estimate_when_child_has_one() {
    let dir = setup_project();
    enable_estimate_requirement(&dir);

    // The parent's children exist only in the payload, never yet on disk, so the
    // exemption must be derived from the payload rather than the filesystem.
    let resp = import_one(
        &dir,
        json!([{
            "title": "Parent no estimate",
            "status": "in_progress",
            "children": [
                { "title": "Leaf child", "status": "todo", "schedule": { "estimate_hours": 1.5 } }
            ]
        }]),
    );

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    assert!(get_text(&resp).contains("Tasks created: 2"));
    let listed = list_text(&dir);
    assert!(listed.contains("Parent no estimate"));
    assert!(listed.contains("Leaf child"));
}

#[test]
fn import_rejects_estimateless_nested_child() {
    let dir = setup_project();
    enable_estimate_requirement(&dir);

    let resp = import_one(
        &dir,
        json!([{
            "title": "Parent ok",
            "status": "in_progress",
            "children": [
                { "title": "Bad leaf", "status": "todo" }
            ]
        }]),
    );

    assert!(
        is_error(&resp),
        "an estimateless nested leaf must be rejected: {}",
        get_text(&resp)
    );
    let text = get_text(&resp);
    assert!(text.contains("estimate_hours"), "{text}");
    // The child is validated before the parent directory is created, so a
    // rejected import leaves no half-written tree and burns no task ID.
    let listed = list_text(&dir);
    assert!(
        !listed.contains("Parent ok") && !listed.contains("Bad leaf"),
        "rejected import must not persist the parent: {listed}"
    );
}

#[test]
fn import_rejected_task_does_not_burn_task_id() {
    let dir = setup_project();
    enable_estimate_requirement(&dir);

    let bad = import_one(&dir, json!([{ "title": "Rejected", "status": "todo" }]));
    assert!(is_error(&bad));

    let good = import_one(
        &dir,
        json!([{
            "title": "Accepted",
            "status": "todo",
            "schedule": { "estimate_hours": 1.0 }
        }]),
    );
    assert!(!is_error(&good), "error: {}", get_text(&good));

    // The rejected import must not have consumed t1.
    let resp = call_tool(
        "handoff_get_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": "t1" }),
    );
    assert!(!is_error(&resp), "t1 should exist: {}", get_text(&resp));
    let task: Value = serde_json::from_str(&get_text(&resp)).unwrap();
    assert_eq!(task["title"], "Accepted", "the rejected import burned t1");
}

#[test]
fn import_ignores_estimate_rule_when_disabled() {
    // setup_project() leaves require_estimate_hours = false.
    let dir = setup_project();

    let resp = import_one(&dir, json!([{ "title": "No estimate", "status": "todo" }]));

    assert!(!is_error(&resp), "error: {}", get_text(&resp));
    assert!(get_text(&resp).contains("Tasks created: 1"));
}

// --- import_context rejects circular dependencies (t84) ---

/// Import with the estimate rule off, so these tests isolate dependency checks.
fn import_deps(dir: &TempDir, tasks: Value) -> Value {
    call_tool(
        "handoff_import_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "source": { "description": "dep import" },
            "tasks": tasks
        }),
    )
}

#[test]
fn import_rejects_self_dependency() {
    let dir = setup_project();

    // The first imported task becomes t1, so depending on "t1" is a self-cycle.
    let resp = import_deps(
        &dir,
        json!([{ "title": "Selfdep", "dependencies": ["t1"] }]),
    );

    assert!(
        is_error(&resp),
        "a self-dependency must be rejected: {}",
        get_text(&resp)
    );
    assert!(
        get_text(&resp).contains("Circular dependency"),
        "{}",
        get_text(&resp)
    );
    assert!(!list_text(&dir).contains("Selfdep"));
}

#[test]
fn import_rejects_cycle_through_an_existing_task() {
    let dir = setup_project();
    // t1 already on disk, depending on t2 (which does not exist yet).
    let seed = call_tool(
        "handoff_update_task",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "task": { "title": "Existing", "dependencies": ["t2"] }
        }),
    );
    assert!(!is_error(&seed), "seed: {}", get_text(&seed));

    // Importing t2 -> t1 closes the loop t1 -> t2 -> t1.
    let resp = import_deps(
        &dir,
        json!([{ "title": "Closes loop", "dependencies": ["t1"] }]),
    );

    assert!(
        is_error(&resp),
        "an indirect cycle through an existing task must be rejected: {}",
        get_text(&resp)
    );
    assert!(!list_text(&dir).contains("Closes loop"));
}

#[test]
fn import_rejects_cycle_entirely_inside_the_payload() {
    let dir = setup_project();

    // t1 <-> t2, neither on disk: only a whole-payload graph can see this.
    let resp = import_deps(
        &dir,
        json!([
            { "title": "Alpha", "dependencies": ["t2"] },
            { "title": "Beta",  "dependencies": ["t1"] }
        ]),
    );

    assert!(
        is_error(&resp),
        "a cycle contained in the payload must be rejected: {}",
        get_text(&resp)
    );
    let listed = list_text(&dir);
    assert!(
        !listed.contains("Alpha") && !listed.contains("Beta"),
        "rejected import must write nothing: {listed}"
    );
}

#[test]
fn import_rejects_cycle_among_earlier_tasks_when_the_last_is_innocent() {
    let dir = setup_project();

    // t1 <-> t2 form a cycle; t3 has no dependencies. The cycle is unreachable
    // from the last node, so every pending node must be searched, not just one.
    let resp = import_deps(
        &dir,
        json!([
            { "title": "Alpha", "dependencies": ["t2"] },
            { "title": "Beta",  "dependencies": ["t1"] },
            { "title": "Innocent" }
        ]),
    );

    assert!(
        is_error(&resp),
        "a cycle among earlier tasks must be found even when the last task is clean: {}",
        get_text(&resp)
    );
    let listed = list_text(&dir);
    assert!(
        !listed.contains("Alpha") && !listed.contains("Beta") && !listed.contains("Innocent"),
        "rejected import must write nothing: {listed}"
    );
}

#[test]
fn import_allows_valid_dependency_within_the_same_payload() {
    let dir = setup_project();

    // Beta depends on Alpha, which is created in this very import. Nothing is on
    // disk at validation time, so a per-task disk check would wrongly reject it.
    let resp = import_deps(
        &dir,
        json!([
            { "title": "Alpha" },
            { "title": "Beta", "dependencies": ["t1"] }
        ]),
    );

    assert!(
        !is_error(&resp),
        "a forward dependency inside one payload must be allowed: {}",
        get_text(&resp)
    );
    assert!(get_text(&resp).contains("Tasks created: 2"));
}

#[test]
fn import_allows_child_to_depend_on_its_parent() {
    let dir = setup_project();

    let resp = import_deps(
        &dir,
        json!([{
            "title": "Parent",
            "children": [ { "title": "Child", "dependencies": ["t1"] } ]
        }]),
    );

    assert!(
        !is_error(&resp),
        "a child depending on its parent is acyclic and must pass: {}",
        get_text(&resp)
    );
    assert!(get_text(&resp).contains("Tasks created: 2"));
}

#[test]
fn import_rejects_cycle_between_parent_and_child() {
    let dir = setup_project();

    // Parent(t1) -> t1.1, Child(t1.1) -> t1.
    let resp = import_deps(
        &dir,
        json!([{
            "title": "Parent",
            "dependencies": ["t1.1"],
            "children": [ { "title": "Child", "dependencies": ["t1"] } ]
        }]),
    );

    assert!(
        is_error(&resp),
        "a parent<->child cycle must be rejected: {}",
        get_text(&resp)
    );
    assert!(!list_text(&dir).contains("Parent"));
}

#[test]
fn import_allows_dangling_dependency_like_update_task_does() {
    let dir = setup_project();

    // update_task accepts a dependency on a task that does not exist, so import
    // must too — rejecting only here would make the two tools disagree.
    let resp = import_deps(
        &dir,
        json!([{ "title": "Dangling", "dependencies": ["t999"] }]),
    );

    assert!(
        !is_error(&resp),
        "a dangling dependency is accepted elsewhere and must be accepted here: {}",
        get_text(&resp)
    );
    assert!(list_text(&dir).contains("Dangling"));
}

#[test]
fn import_invalid_priority_in_child_rejected() {
    let dir = setup_project();
    let resp = call_tool(
        "handoff_import_context",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "source": { "description": "bad child priority" },
            "tasks": [
                {
                    "title": "Parent",
                    "priority": "high",
                    "children": [
                        { "title": "Child", "priority": "ASAP" }
                    ]
                }
            ]
        }),
    );
    assert!(is_error(&resp));
    let text = get_text(&resp);
    assert!(text.contains("Invalid priority"));
}
