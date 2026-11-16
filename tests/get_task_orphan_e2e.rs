//! `handoff_get_task(include_orphan: true)` (REQ-TRC-020, SPEC-TRC-011 /
//! SPEC-TRC-012), through the real `handoff-mcp` binary: the server over
//! stdio JSON-RPC and the `task get` CLI.
//!
//! The session-closer and the session-closer-verifier ask one task at a time
//! whether it is an orphan. The answer is the shared orphan predicate, so it
//! agrees with the `task_orphan` lint rule for every task, and it is only
//! worked out when asked for: without the argument the response is what it
//! always was.

#[path = "support/onboarding_e2e.rs"]
mod support;

use serde_json::{json, Value};
use support::{Sandbox, Server};

const REQUIREMENT_BODY: &str = "# Requirements\n\n## Section\n\n### REQ-001 The first requirement\n\n- priority: P1\n\nThe system does the first thing.\n\n受入基準:\n- AC1: it does\n";

fn project_with(layer_document: bool) -> (Sandbox, Server) {
    let sandbox = Sandbox::initialized("");
    let mut server = sandbox.server();
    server.ok(
        "handoff_update_config",
        json!({ "updates": { "settings.require_estimate_hours": false } }),
    );
    if layer_document {
        server.ok(
            "handoff_doc_save",
            json!({
                "slug": "req-get-task",
                "title": "Requirements",
                "doc_type": "spec",
                "layer": "requirement",
                "body": REQUIREMENT_BODY,
            }),
        );
    }
    (sandbox, server)
}

fn create(server: &mut Server, id: &str, extra: Value) {
    let mut task = json!({ "id": id, "title": format!("Title of {id}") });
    for (key, value) in extra.as_object().expect("object") {
        task[key] = value.clone();
    }
    server.ok("handoff_update_task", json!({ "task": task }));
}

fn linked() -> Value {
    json!({ "requirement_ids": ["REQ-001"] })
}

/// Every id of the fixture, with whether it is an orphan.
const FIXTURE: [(&str, bool); 12] = [
    ("leaf", true),
    ("traced", false),
    ("dropped", false),
    ("exempt", false),
    ("finished", true),
    ("covered", false),
    ("covered.1", false),
    ("covered.2", false),
    ("partial", true),
    ("partial.1", false),
    ("partial.2", true),
    ("doconly", true),
];

fn build_tasks(server: &mut Server) {
    create(server, "leaf", json!({}));
    create(server, "traced", linked());
    create(server, "dropped", json!({ "status": "skipped" }));
    create(server, "exempt", json!({ "labels": ["chore", "no-trace"] }));
    create(server, "finished", json!({ "status": "done" }));
    create(server, "covered", json!({}));
    create(server, "covered.1", linked());
    create(server, "covered.2", json!({ "labels": ["no-trace"] }));
    create(server, "partial", json!({}));
    create(server, "partial.1", linked());
    create(server, "partial.2", json!({}));
    // A document link is not a requirement link.
    create(server, "doconly", json!({}));
    server.ok(
        "handoff_doc_save",
        json!({
            "slug": "design-note",
            "title": "Design note",
            "doc_type": "note",
            "body": "# Design note\n\ntext\n",
            "task_ids": ["doconly"],
        }),
    );
}

fn get(server: &mut Server, id: &str, include_orphan: bool) -> Value {
    let mut arguments = json!({ "task_id": id });
    if include_orphan {
        arguments["include_orphan"] = json!(true);
    }
    server.call("handoff_get_task", arguments)
}

#[test]
fn include_orphan_answers_for_each_task_what_the_lint_reports() {
    let (_sandbox, mut server) = project_with(true);
    build_tasks(&mut server);

    let lint = server.call("handoff_trace_lint", json!({ "rules": ["task_orphan"] }));
    let lint_ids: Vec<&str> = lint["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .map(|f| f["task_id"].as_str().expect("task_id"))
        .collect();

    for (id, orphan) in FIXTURE {
        let task = get(&mut server, id, true);
        assert_eq!(task["orphan"], json!(orphan), "{id}: {task}");
        assert_eq!(lint_ids.contains(&id), orphan, "{id}: lint disagrees");
    }
    assert_eq!(
        lint_ids.len(),
        FIXTURE.iter().filter(|(_, orphan)| *orphan).count()
    );
}

#[test]
fn without_the_argument_the_response_has_no_orphan_key() {
    let (_sandbox, mut server) = project_with(true);
    build_tasks(&mut server);

    for (id, _) in FIXTURE {
        let plain = get(&mut server, id, false);
        assert!(plain.get("orphan").is_none(), "{id}: {plain}");
        let explicit_false = server.call(
            "handoff_get_task",
            json!({ "task_id": id, "include_orphan": false }),
        );
        assert_eq!(explicit_false, plain, "{id}");

        // Asking adds the one key and changes nothing else.
        let mut asked = get(&mut server, id, true);
        assert!(asked["orphan"].is_boolean(), "{id}: {asked}");
        asked.as_object_mut().unwrap().remove("orphan");
        assert_eq!(asked, plain, "{id}");
    }
}

/// Scope guard: without a layer document nothing is an orphan.
#[test]
fn nothing_is_an_orphan_in_a_project_without_a_layer_document() {
    let (_sandbox, mut server) = project_with(false);
    create(&mut server, "a", json!({}));
    create(&mut server, "b", json!({ "status": "done" }));
    for id in ["a", "b"] {
        assert_eq!(get(&mut server, id, true)["orphan"], json!(false), "{id}");
    }

    server.ok(
        "handoff_doc_save",
        json!({
            "slug": "req-late",
            "title": "Requirements",
            "doc_type": "spec",
            "layer": "requirement",
            "body": REQUIREMENT_BODY,
        }),
    );
    for id in ["a", "b"] {
        assert_eq!(get(&mut server, id, true)["orphan"], json!(true), "{id}");
    }
}

#[test]
fn the_answer_follows_the_task_as_it_is_linked_labelled_or_skipped() {
    let (_sandbox, mut server) = project_with(true);
    create(&mut server, "a", json!({}));
    assert_eq!(get(&mut server, "a", true)["orphan"], json!(true));

    server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "a", "requirement_ids": ["REQ-001"] } }),
    );
    assert_eq!(get(&mut server, "a", true)["orphan"], json!(false));

    server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "a", "requirement_ids": [] } }),
    );
    assert_eq!(get(&mut server, "a", true)["orphan"], json!(true));

    server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "a", "labels": ["no-trace"] } }),
    );
    assert_eq!(get(&mut server, "a", true)["orphan"], json!(false));
}

/// When the only layer document cannot be read the answer is not a guess:
/// the call that asked fails, the plain read still works.
#[test]
fn an_unreadable_layer_document_fails_the_question_not_the_plain_read() {
    let (sandbox, mut server) = project_with(true);
    create(&mut server, "a", json!({}));
    create(&mut server, "exempt", json!({ "labels": ["no-trace"] }));
    let path = sandbox.handoff().join("docs/_doc.req-get-task.md");
    let text = std::fs::read_to_string(&path).expect("the layer document");
    std::fs::write(&path, text.replacen("---\n", "---\n: : broken [\n", 1)).unwrap();

    let error = server.err(
        "handoff_get_task",
        json!({ "task_id": "a", "include_orphan": true }),
    );
    assert!(error.contains("orphan check failed"), "{error}");
    assert!(error.contains("req-get-task"), "{error}");

    assert_eq!(get(&mut server, "a", false)["status"], "todo");
    // A task accounted for by itself needs no document.
    assert_eq!(get(&mut server, "exempt", true)["orphan"], json!(false));
}

#[test]
fn include_orphan_must_be_a_boolean() {
    let (_sandbox, mut server) = project_with(true);
    create(&mut server, "a", json!({}));
    let error = server.err(
        "handoff_get_task",
        json!({ "task_id": "a", "include_orphan": "yes" }),
    );
    assert!(
        error.contains("'include_orphan' must be a boolean"),
        "{error}"
    );
}

/// The read is read-only: nothing under `.handoff/tasks` changes.
#[test]
fn asking_writes_nothing() {
    let (sandbox, mut server) = project_with(true);
    build_tasks(&mut server);
    let before = support::snapshot(&sandbox.handoff().join("tasks"));
    for (id, _) in FIXTURE {
        get(&mut server, id, true);
    }
    assert_eq!(support::snapshot(&sandbox.handoff().join("tasks")), before);
}

/// The closer's CLI fallback: `task get --include-orphan true`.
#[test]
fn the_cli_answers_the_same_question() {
    let (sandbox, mut server) = project_with(true);
    build_tasks(&mut server);
    drop(server);

    for (id, orphan) in [("leaf", true), ("traced", false), ("covered", false)] {
        let out = sandbox.cli(&["task", "get", "--task-id", id, "--include-orphan", "true"]);
        assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
        assert_eq!(out.json()["orphan"], json!(orphan), "{id}");
    }
    let plain = sandbox.cli(&["task", "get", "--task-id", "leaf"]);
    assert!(plain.json().get("orphan").is_none(), "{}", plain.stdout);
}

#[test]
fn the_tool_definition_documents_include_orphan() {
    let (_sandbox, mut server) = project_with(false);
    let definition = server.tool_definition("handoff_get_task");
    let property = &definition["inputSchema"]["properties"]["include_orphan"];
    assert_eq!(property["type"], "boolean", "{definition}");
    let description = definition["description"].as_str().expect("description");
    for needle in ["include_orphan", "orphan", "task_orphan", "no-trace"] {
        assert!(description.contains(needle), "description lacks {needle:?}");
    }
}
