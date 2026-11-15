//! The handoff calls the session-closer makes for the requirement-link rule
//! (REQ-TRC-020, SPEC-TRC-011), against the real `handoff-mcp` binary over
//! stdio JSON-RPC.
//!
//! The closer is an agent: its definition
//! (`plugin-task-loop/agents/session-closer.md`) is pinned by
//! `plugin-task-loop/workflows/lib/session-closer.agent.test.js`. These tests
//! pin the other half, the server behaviour that definition relies on: the
//! links a report names are applied first, `handoff_get_task` with
//! `include_orphan` then tells an orphan from a task that may be closed, and
//! one `handoff_update_task` call sets `review` and appends the note.

#[path = "support/onboarding_e2e.rs"]
mod support;

use serde_json::{json, Value};
use support::{Sandbox, Server};

const REQUIREMENT_BODY: &str = "# Requirements\n\n## Section\n\n### REQ-001 The first requirement\n\n- priority: P1\n\nThe system does the first thing.\n\n受入基準:\n- AC1: it does\n";

/// The note of `plugin-task-loop/agents/session-closer.md`, word for word.
const NOTE: &str = "Requirement link missing: this task has no requirement link, so it was left in review instead of done. Link it to the requirement it implements (handoff_update_task requirement_ids; handoff_trace_propose suggests candidates), or add the label no-trace if it is deliberately outside requirement traceability, then mark it done.";

fn project_with(orphan_guard: Option<&str>, layer_document: bool) -> (Sandbox, Server) {
    let sandbox = Sandbox::initialized("");
    let mut server = sandbox.server();
    server.ok(
        "handoff_update_config",
        json!({ "updates": { "settings.require_estimate_hours": false } }),
    );
    if let Some(mode) = orphan_guard {
        server.ok(
            "handoff_update_config",
            json!({ "updates": { "trace.orphan_guard": mode } }),
        );
    }
    if layer_document {
        server.ok(
            "handoff_doc_save",
            json!({
                "slug": "req-closer",
                "title": "Requirements",
                "doc_type": "spec",
                "layer": "requirement",
                "body": REQUIREMENT_BODY,
            }),
        );
    }
    (sandbox, server)
}

/// A task a developer has been working on.
fn in_progress(server: &mut Server, id: &str, extra: Value) {
    let mut task = json!({ "id": id, "title": format!("Title of {id}"), "status": "in_progress" });
    for (key, value) in extra.as_object().expect("object") {
        task[key] = value.clone();
    }
    server.ok("handoff_update_task", json!({ "task": task }));
}

fn orphan(server: &mut Server, id: &str) -> Value {
    server.call(
        "handoff_get_task",
        json!({ "task_id": id, "include_orphan": true }),
    )["orphan"]
        .clone()
}

fn task(server: &mut Server, id: &str) -> Value {
    server.call("handoff_get_task", json!({ "task_id": id }))
}

/// The closer's Step 4 for one task: read `orphan`, then `review` plus the
/// note for an orphan, `done` otherwise. Returns the update's response text.
fn close(server: &mut Server, id: &str) -> Result<String, String> {
    if orphan(server, id) == json!(true) {
        server.call_text(
            "handoff_update_task",
            json!({ "task": { "id": id, "status": "review", "notes_append": NOTE } }),
        )
    } else {
        server.call_text(
            "handoff_update_task",
            json!({ "task": { "id": id, "status": "done" } }),
        )
    }
}

// --- REQ-TRC-020 AC1 / AC2 ---

#[test]
fn a_task_with_no_requirement_link_ends_in_review_with_the_note() {
    let (_sandbox, mut server) = project_with(None, true);
    in_progress(&mut server, "a", json!({}));

    let response = close(&mut server, "a").expect("the review transition applies");
    assert!(
        response.starts_with("Updated task a: Title of a [review]"),
        "{response}"
    );

    let closed = task(&mut server, "a");
    assert_eq!(closed["status"], "review");
    assert_eq!(closed["completed_at"], Value::Null, "it was never done");
    let notes = closed["notes"].as_str().expect("notes");
    assert!(notes.contains(NOTE), "{notes}");
    // Still an orphan afterwards: what the verifier reads back.
    assert_eq!(orphan(&mut server, "a"), json!(true));
}

/// Steps 2 and 3 come first: a task the report links is not an orphan by the
/// time the status is decided.
#[test]
fn a_task_linked_from_its_report_is_closed_as_done() {
    let (_sandbox, mut server) = project_with(None, true);
    in_progress(&mut server, "a", json!({}));
    assert_eq!(orphan(&mut server, "a"), json!(true));

    // Step 2 (trace update) and Step 3 (requirement links).
    server.call(
        "handoff_trace_update",
        json!({ "task_id": "a", "ops": [
            { "op": "link", "item": "REQ-001", "role": "implements" },
            { "op": "set", "item": "REQ-001", "dev_stage": "implemented" },
        ] }),
    );
    server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "a", "requirement_ids": ["REQ-001"] } }),
    );
    assert_eq!(orphan(&mut server, "a"), json!(false));

    close(&mut server, "a").expect("done");
    let closed = task(&mut server, "a");
    assert_eq!(closed["status"], "done");
    assert!(
        !closed["notes"]
            .as_str()
            .unwrap_or("")
            .contains("Requirement link missing"),
        "{closed}"
    );
}

/// The trace update alone (Step 2) already links the task.
#[test]
fn the_trace_update_link_alone_is_enough() {
    let (_sandbox, mut server) = project_with(None, true);
    in_progress(&mut server, "a", json!({}));
    server.call(
        "handoff_trace_update",
        json!({ "task_id": "a", "ops": [
            { "op": "link", "item": "REQ-001", "role": "implements" },
        ] }),
    );
    assert_eq!(orphan(&mut server, "a"), json!(false));
}

// --- AC3: no-trace ---

#[test]
fn a_no_trace_task_is_closed_as_done_as_usual() {
    let (_sandbox, mut server) = project_with(None, true);
    in_progress(&mut server, "a", json!({ "labels": ["chore", "no-trace"] }));
    assert_eq!(orphan(&mut server, "a"), json!(false));

    let response = close(&mut server, "a").expect("done");
    assert!(
        response.starts_with("Updated task a: Title of a [done]"),
        "{response}"
    );
    assert!(!response.contains("ORPHAN_TASK"), "{response}");
    assert_eq!(task(&mut server, "a")["status"], "done");
}

// --- SPEC-TRC-011 AC4: no layer document ---

#[test]
fn in_a_project_without_a_layer_document_every_task_is_closed_as_before() {
    let (_sandbox, mut server) = project_with(None, false);
    in_progress(&mut server, "a", json!({}));
    assert_eq!(orphan(&mut server, "a"), json!(false));

    let response = close(&mut server, "a").expect("done");
    assert_eq!(
        response,
        "Updated task a: Title of a [done]\nschedule_stale: true (run handoff_auto_schedule to recompute)"
    );
    let closed = task(&mut server, "a");
    assert_eq!(closed["status"], "done");
    assert_eq!(closed["notes"], Value::Null);
}

// --- the parent rule ---

#[test]
fn a_parent_traced_through_its_subtasks_is_closed_as_done() {
    let (_sandbox, mut server) = project_with(None, true);
    in_progress(&mut server, "p", json!({}));
    in_progress(
        &mut server,
        "p.1",
        json!({ "requirement_ids": ["REQ-001"] }),
    );
    in_progress(&mut server, "q", json!({}));
    in_progress(&mut server, "q.1", json!({}));

    assert_eq!(orphan(&mut server, "p"), json!(false));
    assert_eq!(orphan(&mut server, "q"), json!(true));
    assert_eq!(orphan(&mut server, "q.1"), json!(true));
}

// --- together with the orphan guard ---

/// Under the default `warn` the closer's `review` transition applies and the
/// guard adds its own warning line to the same response.
#[test]
fn under_warn_the_review_transition_applies_with_the_guard_warning() {
    let (_sandbox, mut server) = project_with(Some("warn"), true);
    in_progress(&mut server, "a", json!({}));
    let response = close(&mut server, "a").expect("review");
    assert!(
        response.contains("Warning [ORPHAN_TASK]: Task a has no requirement link."),
        "{response}"
    );
    assert_eq!(task(&mut server, "a")["status"], "review");
}

/// Under `block` the project forbids an orphan from entering `review` too:
/// the closer's call is rejected and changes nothing, so the closer reports
/// the task as not closed. A notes-only call still records the note.
#[test]
fn under_block_the_review_transition_is_rejected_and_the_note_still_goes_in() {
    let (_sandbox, mut server) = project_with(Some("block"), true);
    in_progress(&mut server, "a", json!({}));

    let rejected = close(&mut server, "a").unwrap_err();
    assert!(
        rejected.contains("orphan_guard: task a has no requirement link."),
        "{rejected}"
    );
    assert!(rejected.contains("orphan_guard = \"block\""), "{rejected}");
    let unchanged = task(&mut server, "a");
    assert_eq!(unchanged["status"], "in_progress");
    assert_eq!(
        unchanged["notes"],
        Value::Null,
        "a rejected call writes nothing"
    );

    server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "a", "notes_append": NOTE } }),
    );
    let noted = task(&mut server, "a");
    assert_eq!(noted["status"], "in_progress");
    assert!(noted["notes"].as_str().expect("notes").contains(NOTE));
}

/// The note the tests use is the one the definition and the workflow carry.
#[test]
fn the_note_is_the_one_the_closer_definition_and_the_workflow_carry() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let definition =
        std::fs::read_to_string(root.join("plugin-task-loop/agents/session-closer.md"))
            .expect("session-closer.md");
    assert!(
        definition.contains(NOTE),
        "session-closer.md must carry the note"
    );

    let workflow =
        std::fs::read_to_string(root.join("plugin-task-loop/workflows/session-execute.js"))
            .expect("session-execute.js");
    let start = workflow
        .find("const REQUIREMENT_LINK_MISSING_NOTE =")
        .expect("the workflow constant");
    // The statement ends at the first `';` (the note itself contains a `;`).
    let end = workflow[start..].find("';").expect("end of the constant") + start + 1;
    let joined: String = workflow[start..end]
        .split('\'')
        .skip(1)
        .step_by(2)
        .collect();
    assert_eq!(joined, NOTE);
}
