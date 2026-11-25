//! The requirement-link guidance on task creation (REQ-TRC-003 / REQ-TRC-004,
//! SPEC-TRC-006 / SPEC-TRC-007 / SPEC-TRC-008), through the real
//! `handoff-mcp` binary over stdio JSON-RPC.
//!
//! `handoff_update_task` answers the creation of a task that has no
//! requirement link with two `Info` lines: link it or add the requirement,
//! and `handoff_trace_propose` finds candidates. The lines never block, and
//! they are absent when an existing task is updated, when the new task is
//! labelled `no-trace` or names `requirement_ids`, and in a project with no
//! layer document.

use crate::support::onboarding_e2e as support;

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
                "slug": "req-guidance",
                "title": "Requirements",
                "doc_type": "spec",
                "layer": "requirement",
                "body": REQUIREMENT_BODY,
            }),
        );
    }
    (sandbox, server)
}

fn project() -> (Sandbox, Server) {
    project_with(true)
}

fn update(server: &mut Server, task: Value) -> String {
    server.ok("handoff_update_task", json!({ "task": task }))
}

/// The two guidance lines for a new task `id`.
fn guidance(id: &str) -> String {
    format!(
        "Info [REQUIREMENT_LINK_MISSING]: Task {id} has no requirement link. Link it to an \
         existing requirement with handoff_update_task(task: {{id: \"{id}\", requirement_ids: \
         [...]}}), or add the missing requirement first; label the task \"no-trace\" if it is \
         deliberately outside requirement traceability.\n\
         Info [TRACE_PROPOSE]: handoff_trace_propose(task_id: \"{id}\") lists existing \
         requirements that may already cover this task and drafts a new one when none does."
    )
}

fn assert_no_guidance(text: &str) {
    assert!(!text.contains("REQUIREMENT_LINK_MISSING"), "{text}");
    assert!(!text.contains("TRACE_PROPOSE"), "{text}");
    assert!(!text.contains("Info ["), "{text}");
}

// --- REQ-TRC-003 AC1 / AC3, REQ-TRC-004 AC1 / AC2 ---

#[test]
fn creating_a_task_without_requirement_ids_answers_with_the_guidance_and_creates_it() {
    let (_sandbox, mut server) = project();

    // Caller-chosen id.
    let text = update(&mut server, json!({ "id": "a", "title": "A" }));
    assert_eq!(text, format!("Created task a: A [todo]\n{}", guidance("a")));
    // Server-assigned id.
    let text = update(&mut server, json!({ "title": "B" }));
    assert_eq!(
        text,
        format!("Created task t1: B [todo]\n{}", guidance("t1"))
    );
    // An empty list names no requirement either.
    let text = update(
        &mut server,
        json!({ "id": "c", "title": "C", "requirement_ids": [] }),
    );
    assert_eq!(text, format!("Created task c: C [todo]\n{}", guidance("c")));

    // Not blocking: every task exists.
    for (id, title) in [("a", "A"), ("t1", "B"), ("c", "C")] {
        let task = server.call("handoff_get_task", json!({ "task_id": id }));
        assert_eq!(task["title"], title);
        assert_eq!(task["status"], "todo");
    }
}

#[test]
fn the_guidance_is_info_level_and_points_at_trace_propose() {
    let (_sandbox, mut server) = project();
    let text = update(&mut server, json!({ "id": "a", "title": "A" }));

    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert!(
        lines[1].starts_with("Info [REQUIREMENT_LINK_MISSING]: "),
        "{text}"
    );
    assert!(lines[1].contains("existing requirement"), "{text}");
    assert!(lines[1].contains("add the missing requirement"), "{text}");
    assert!(lines[2].starts_with("Info [TRACE_PROPOSE]: "), "{text}");
    assert!(
        lines[2].contains("handoff_trace_propose(task_id: \"a\")"),
        "{text}"
    );
    assert!(
        !text.contains("Warning"),
        "an info line is not a warning: {text}"
    );

    // The call the hint names works as written.
    let proposal = server.call("handoff_trace_propose", json!({ "task_id": "a" }));
    assert!(proposal["candidates"].is_array(), "{proposal}");
}

#[test]
fn a_child_task_gets_the_guidance_too() {
    let (_sandbox, mut server) = project();
    update(
        &mut server,
        json!({ "id": "p", "title": "Parent", "labels": ["no-trace"] }),
    );
    let text = server.ok(
        "handoff_update_task",
        json!({ "task": { "title": "Child" }, "parent_id": "p" }),
    );
    assert_eq!(
        text,
        format!("Created task p.1: Child [todo]\n{}", guidance("p.1"))
    );
}

// --- AC2: updates of existing tasks ---

#[test]
fn updating_an_existing_task_never_answers_with_the_guidance() {
    let (_sandbox, mut server) = project();
    update(&mut server, json!({ "id": "a", "title": "A" }));
    update(
        &mut server,
        json!({ "id": "linked", "title": "L", "requirement_ids": ["REQ-001"] }),
    );

    // No requirement_ids, other fields, and the same title again.
    assert_eq!(
        update(&mut server, json!({ "id": "a", "notes": "n" })),
        "Updated task a: A [todo]"
    );
    assert_eq!(
        update(&mut server, json!({ "id": "a", "title": "A" })),
        "Updated task a: A [todo]"
    );
    let text = update(&mut server, json!({ "id": "a", "status": "in_progress" }));
    assert_no_guidance(&text);
    // Touching the links of an existing task is not a creation either.
    let text = update(
        &mut server,
        json!({ "id": "linked", "requirement_ids": [] }),
    );
    assert_no_guidance(&text);
    let text = update(&mut server, json!({ "id": "linked", "priority": "low" }));
    assert_no_guidance(&text);
}

// --- AC4: no-trace ---

#[test]
fn a_new_no_trace_task_gets_no_guidance() {
    let (_sandbox, mut server) = project();
    let text = update(
        &mut server,
        json!({ "id": "a", "title": "A", "labels": ["chore", "no-trace"] }),
    );
    assert_eq!(text, "Created task a: A [todo]");
    let text = update(&mut server, json!({ "title": "B", "labels": ["no-trace"] }));
    assert_eq!(text, "Created task t1: B [todo]");
}

// --- requirement_ids given ---

#[test]
fn a_new_task_that_names_requirement_ids_gets_no_guidance() {
    let (_sandbox, mut server) = project();
    let text = update(
        &mut server,
        json!({ "id": "a", "title": "A", "requirement_ids": ["REQ-001"] }),
    );
    assert_eq!(text, "Created task a: A [todo]");
    let task = server.call("handoff_get_task", json!({ "task_id": "a" }));
    assert_eq!(task["task_links"][0]["label"], "REQ-001");

    // An id that does not resolve already has its own warning; the guidance
    // is for a call that names no requirement at all.
    let text = update(
        &mut server,
        json!({ "id": "b", "title": "B", "requirement_ids": ["REQ-404"] }),
    );
    assert!(text.contains("REQ-404"), "{text}");
    assert_no_guidance(&text);
}

// --- shared predicate: a skipped task is not an orphan ---

#[test]
fn a_task_created_as_skipped_gets_no_guidance() {
    let (_sandbox, mut server) = project();
    let text = update(
        &mut server,
        json!({ "id": "a", "title": "A", "status": "skipped" }),
    );
    assert_eq!(text, "Created task a: A [skipped]");
}

// --- scope guard ---

#[test]
fn a_project_without_a_layer_document_answers_as_before() {
    let (_sandbox, mut server) = project_with(false);
    server.ok(
        "handoff_doc_save",
        json!({ "slug": "note", "title": "Note", "doc_type": "note", "body": "# Note\n\ntext\n" }),
    );
    assert_eq!(
        update(&mut server, json!({ "id": "a", "title": "A" })),
        "Created task a: A [todo]"
    );
    assert_eq!(
        update(&mut server, json!({ "title": "B" })),
        "Created task t1: B [todo]"
    );
    assert_eq!(
        server.ok(
            "handoff_update_task",
            json!({ "task": { "title": "C" }, "parent_id": "a" })
        ),
        "Created task a.1: C [todo]"
    );
}

// --- together with the other response lines ---

#[test]
fn the_guidance_comes_after_the_warnings_of_the_same_call() {
    let (_sandbox, mut server) = project();
    // An undefined milestone and a dependency on a task that does not exist.
    let text = update(
        &mut server,
        json!({ "id": "a", "title": "A", "dependencies": ["ghost"], "schedule": { "milestone": "v9" } }),
    );
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 5, "{text}");
    assert_eq!(lines[0], "Created task a: A [todo]");
    assert!(
        lines[1].starts_with("Warning [DANGLING_DEPENDENCY]"),
        "{text}"
    );
    assert!(
        lines[2].starts_with("Warning [UNKNOWN_MILESTONE]"),
        "{text}"
    );
    assert_eq!(lines[3..].join("\n"), guidance("a"));
}

#[test]
fn creating_an_orphan_task_straight_into_done_carries_the_guard_warning_then_the_guidance() {
    let (_sandbox, mut server) = project();
    let text = update(
        &mut server,
        json!({ "id": "a", "title": "A", "status": "done" }),
    );
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4, "{text}");
    assert_eq!(lines[0], "Created task a: A [done]");
    assert!(
        lines[1].starts_with("Warning [ORPHAN_TASK]: Task a "),
        "{text}"
    );
    assert_eq!(lines[2..].join("\n"), guidance("a"));
}

/// The guidance is independent of `[trace] orphan_guard`: `off` silences the
/// guard, not the creation hint, and `block` does not turn the hint into a
/// rejection.
#[test]
fn the_guidance_does_not_depend_on_the_orphan_guard_mode() {
    for mode in ["off", "block"] {
        let (_sandbox, mut server) = project();
        server.ok(
            "handoff_update_config",
            json!({ "updates": { "trace.orphan_guard": mode } }),
        );
        let text = update(&mut server, json!({ "id": "a", "title": "A" }));
        assert_eq!(
            text,
            format!("Created task a: A [todo]\n{}", guidance("a")),
            "{mode}"
        );
    }
}

// --- other creation paths (SPEC-TRC-007) ---

#[test]
fn trace_tasks_creates_linked_tasks_and_reports_no_guidance() {
    let (_sandbox, mut server) = project();
    let result = server.call(
        "handoff_trace_tasks",
        json!({ "mode": "apply", "estimate_hours": 1.0 }),
    );
    let created = result["created"].as_array().expect("created");
    assert_eq!(created.len(), 1, "{result}");
    let rendered = result.to_string();
    assert!(!rendered.contains("REQUIREMENT_LINK_MISSING"), "{rendered}");
    assert!(!rendered.contains("TRACE_PROPOSE"), "{rendered}");
    // The task it created is linked, so it is not an orphan.
    let lint = server.call("handoff_trace_lint", json!({ "rules": ["task_orphan"] }));
    assert_eq!(lint["findings"], json!([]));
}

fn import(server: &mut Server, tasks: Value) -> String {
    server.ok(
        "handoff_import_context",
        json!({ "source": { "description": "notes" }, "tasks": tasks, "skip_session_close": true }),
    )
}

#[test]
fn import_context_reports_its_unlinked_tasks_in_one_summary_line() {
    let (_sandbox, mut server) = project();
    let text = import(
        &mut server,
        json!([
            { "title": "Epic", "children": [ { "title": "Story" }, { "title": "Chore", "labels": ["no-trace"] } ] },
            { "title": "Housekeeping", "labels": ["no-trace"] },
            { "title": "Dropped", "status": "skipped" },
            { "title": "Loose end" },
        ]),
    );
    let info: Vec<&str> = text.lines().filter(|l| l.contains("Info [")).collect();
    assert_eq!(info.len(), 1, "one summary line, not one per task: {text}");
    assert_eq!(
        info[0],
        "  Info [REQUIREMENT_LINK_MISSING]: 3 imported task(s) have no requirement link: t1, \
         t1.1, t4. Link each to a requirement with handoff_update_task(task: {id, \
         requirement_ids: [...]}) (handoff_trace_propose(task_id) suggests candidates), or \
         label it \"no-trace\"; handoff_trace_lint(rules: [\"task_orphan\"]) lists them again."
    );
    assert!(text.contains("Tasks created: 6"), "{text}");
}

#[test]
fn import_context_without_unlinked_tasks_or_without_a_layer_document_answers_as_before() {
    // Every imported task is accounted for.
    let (_sandbox, mut server) = project();
    let text = import(
        &mut server,
        json!([ { "title": "Housekeeping", "labels": ["no-trace"] }, { "title": "Dropped", "status": "skipped" } ]),
    );
    assert_eq!(
        text,
        "Import complete:\n  Source: notes\n  Tasks created: 2 (2 top-level, 0 nested)"
    );

    // No layer document: nothing is an orphan.
    let (_sandbox, mut server) = project_with(false);
    let text = import(&mut server, json!([ { "title": "Loose end" } ]));
    assert_eq!(
        text,
        "Import complete:\n  Source: notes\n  Tasks created: 1 (1 top-level, 0 nested)"
    );
}

/// A task the project already had is not part of the import's summary.
#[test]
fn import_context_counts_only_the_tasks_it_created() {
    let (_sandbox, mut server) = project();
    update(&mut server, json!({ "title": "Already here" }));
    let text = import(&mut server, json!([ { "title": "Imported" } ]));
    assert!(
        text.contains("1 imported task(s) have no requirement link: t2."),
        "{text}"
    );
}

// --- documentation contract ---

#[test]
fn the_tool_description_and_the_handoff_skill_describe_the_guidance() {
    let (_sandbox, mut server) = project();
    let definition = server.tool_definition("handoff_update_task");
    let description = definition["description"].as_str().expect("description");
    for needle in [
        "Info [REQUIREMENT_LINK_MISSING]",
        "Info [TRACE_PROPOSE]",
        "handoff_trace_propose",
        "no-trace",
        "layer document",
    ] {
        assert!(
            description.contains(needle),
            "handoff_update_task description lacks {needle:?}"
        );
    }
    let import = server.tool_definition("handoff_import_context");
    assert!(
        import["description"]
            .as_str()
            .unwrap()
            .contains("REQUIREMENT_LINK_MISSING"),
        "handoff_import_context description lacks the summary line"
    );

    let skill = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/handoff/SKILL.md"),
    )
    .expect("skills/handoff/SKILL.md");
    for needle in [
        "REQUIREMENT_LINK_MISSING",
        "handoff_trace_propose",
        "`no-trace`",
    ] {
        assert!(skill.contains(needle), "the handoff skill lacks {needle:?}");
    }
}

/// A large import names the first twenty orphans and counts the rest, so the
/// line stays readable.
#[test]
fn import_context_caps_the_list_of_task_ids() {
    let (_sandbox, mut server) = project();
    let tasks: Vec<Value> = (1..=25)
        .map(|n| json!({ "title": format!("Task {n}") }))
        .collect();
    let text = import(&mut server, Value::Array(tasks));
    let line = text
        .lines()
        .find(|l| l.contains("REQUIREMENT_LINK_MISSING"))
        .expect("the summary line");
    assert!(
        line.contains("25 imported task(s) have no requirement link: t1, t2, "),
        "{line}"
    );
    assert!(line.contains(", t20 and 5 more. "), "{line}");
    assert!(!line.contains("t21"), "{line}");
}

// --- a layer document that cannot be read ---

/// When the only layer document cannot be read, the call cannot tell whether
/// the project uses requirement traceability: it says so in one warning line
/// instead of guessing, and the task is created all the same.
#[test]
fn an_unreadable_layer_document_is_reported_instead_of_guessed_past() {
    let (sandbox, mut server) = project();
    let path = sandbox.handoff().join("docs/_doc.req-guidance.md");
    let text = std::fs::read_to_string(&path).expect("the layer document");
    std::fs::write(&path, text.replacen("---\n", "---\n: : broken [\n", 1)).unwrap();

    let created = update(&mut server, json!({ "id": "a", "title": "A" }));
    let lines: Vec<&str> = created.lines().collect();
    assert_eq!(lines[0], "Created task a: A [todo]", "{created}");
    assert_eq!(lines.len(), 2, "{created}");
    assert!(
        lines[1].starts_with("Warning: requirement-link guidance check failed: "),
        "{created}"
    );
    assert!(lines[1].contains("req-guidance"), "{created}");
    assert_no_guidance(&created);
    assert_eq!(
        server.call("handoff_get_task", json!({ "task_id": "a" }))["status"],
        "todo"
    );

    // A task cleared by its own label never needs the documents.
    let exempt = update(
        &mut server,
        json!({ "id": "b", "title": "B", "labels": ["no-trace"] }),
    );
    assert_eq!(exempt, "Created task b: B [todo]");
}
