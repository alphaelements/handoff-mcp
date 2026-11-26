//! The `DONE_TASK_OVERWRITE` warning, through the real `handoff-mcp` binary
//! over stdio JSON-RPC.
//!
//! A task in `done` or `review` is a record of finished work. Replacing its
//! title, notes or list fields (done_criteria, labels, links, scope_paths,
//! dependencies) destroys that record, most often because a task id was
//! reused by mistake. The write is still applied (a warning, not a rejection),
//! but the reply says which field of which task was overwritten. `force=true`
//! states the overwrite is intended and keeps the reply quiet, and
//! `notes_append` — which adds to the notes without replacing them — never
//! warns.

use crate::support::onboarding_e2e as support;

use serde_json::{json, Value};
use support::{Sandbox, Server};

const CODE: &str = "DONE_TASK_OVERWRITE";

fn project() -> (Sandbox, Server) {
    let sandbox = Sandbox::initialized("");
    let server = sandbox.server();
    (sandbox, server)
}

/// Creates `t1` ("Original title", notes "original notes") and moves it to
/// `status`.
fn task_in(server: &mut Server, status: &str) {
    server.ok(
        "handoff_update_task",
        json!({ "task": {
            "title": "Original title",
            "notes": "original notes",
            "schedule": { "estimate_hours": 1 },
        } }),
    );
    if status != "todo" {
        server.ok(
            "handoff_update_task",
            json!({ "task": { "id": "t1", "status": status } }),
        );
    }
}

fn update(server: &mut Server, task: Value) -> String {
    server.ok("handoff_update_task", json!({ "task": task }))
}

fn forced(server: &mut Server, task: Value) -> String {
    server.ok(
        "handoff_update_task",
        json!({ "task": task, "force": true }),
    )
}

fn stored(server: &mut Server) -> Value {
    server.call("handoff_get_task", json!({ "task_id": "t1" }))
}

fn warnings(reply: &str) -> Vec<&str> {
    reply.lines().filter(|l| l.contains(CODE)).collect()
}

// --- handoff_update_task ---

#[test]
fn overwriting_the_title_of_a_done_task_warns_and_still_writes() {
    let (_sandbox, mut server) = project();
    task_in(&mut server, "done");

    let reply = update(
        &mut server,
        json!({ "id": "t1", "title": "Something else" }),
    );
    assert_eq!(
        warnings(&reply),
        vec![
            "Warning [DONE_TASK_OVERWRITE]: Overwriting title of done task t1 (was: \"Original \
             title\"). Use force=true to suppress this warning."
        ],
        "{reply}"
    );
    // A warning, not a rejection: the new title is stored.
    assert_eq!(stored(&mut server)["title"], "Something else");
}

#[test]
fn overwriting_the_notes_of_a_done_task_warns_and_still_writes() {
    let (_sandbox, mut server) = project();
    task_in(&mut server, "done");

    let reply = update(&mut server, json!({ "id": "t1", "notes": "replaced" }));
    let lines = warnings(&reply);
    assert_eq!(lines.len(), 1, "{reply}");
    assert!(
        lines[0].starts_with("Warning [DONE_TASK_OVERWRITE]: Overwriting notes of done task t1"),
        "{reply}"
    );
    assert!(
        lines[0].contains("notes_append") && lines[0].contains("force=true"),
        "the warning names both ways out: {reply}"
    );
    assert_eq!(stored(&mut server)["notes"], "replaced");
}

#[test]
fn overwriting_title_and_notes_together_warns_once_per_field() {
    let (_sandbox, mut server) = project();
    task_in(&mut server, "done");

    let reply = update(
        &mut server,
        json!({ "id": "t1", "title": "Something else", "notes": "replaced" }),
    );
    let lines = warnings(&reply);
    assert_eq!(lines.len(), 2, "{reply}");
    assert!(lines[0].contains("title of done task t1"), "{reply}");
    assert!(lines[1].contains("notes of done task t1"), "{reply}");
}

#[test]
fn a_review_task_is_guarded_like_a_done_one() {
    let (_sandbox, mut server) = project();
    task_in(&mut server, "review");

    let reply = update(
        &mut server,
        json!({ "id": "t1", "title": "Something else" }),
    );
    let lines = warnings(&reply);
    assert_eq!(lines.len(), 1, "{reply}");
    assert!(
        lines[0].contains("Overwriting title of review task t1"),
        "{reply}"
    );

    let reply = update(&mut server, json!({ "id": "t1", "notes": "replaced" }));
    assert!(
        warnings(&reply)
            .iter()
            .any(|l| l.contains("Overwriting notes of review task t1")),
        "{reply}"
    );
}

#[test]
fn notes_append_on_a_done_task_does_not_warn() {
    let (_sandbox, mut server) = project();
    task_in(&mut server, "done");

    let reply = update(
        &mut server,
        json!({ "id": "t1", "notes_append": "follow-up" }),
    );
    assert!(!reply.contains(CODE), "{reply}");
    let notes = stored(&mut server)["notes"].as_str().unwrap().to_string();
    assert!(
        notes.starts_with("original notes") && notes.ends_with("follow-up"),
        "append keeps what was there: {notes}"
    );
}

#[test]
fn force_suppresses_the_warning_and_the_write_is_applied() {
    let (_sandbox, mut server) = project();
    task_in(&mut server, "done");

    let reply = forced(
        &mut server,
        json!({ "id": "t1", "title": "Something else", "notes": "replaced" }),
    );
    assert!(!reply.contains(CODE), "{reply}");
    let task = stored(&mut server);
    assert_eq!(task["title"], "Something else");
    assert_eq!(task["notes"], "replaced");
}

#[test]
fn tasks_that_are_not_done_or_in_review_are_never_warned_about() {
    for status in ["todo", "in_progress", "blocked"] {
        let (_sandbox, mut server) = project();
        task_in(&mut server, status);

        let reply = update(
            &mut server,
            json!({ "id": "t1", "title": "Something else", "notes": "replaced" }),
        );
        assert!(!reply.contains(CODE), "{status}: {reply}");
    }
}

/// The status that counts is the one the task is in when the call arrives:
/// finishing a task and retitling it in the same call overwrites nothing
/// finished, while reopening a finished task under a new title does.
#[test]
fn the_status_before_the_call_decides() {
    let (_sandbox, mut server) = project();
    task_in(&mut server, "in_progress");
    let reply = update(
        &mut server,
        json!({ "id": "t1", "status": "done", "title": "Final title", "notes": "final" }),
    );
    assert!(!reply.contains(CODE), "{reply}");

    let reply = update(
        &mut server,
        json!({ "id": "t1", "status": "todo", "title": "A different job" }),
    );
    let lines = warnings(&reply);
    assert_eq!(lines.len(), 1, "{reply}");
    assert!(
        lines[0].contains("Overwriting title of done task t1 (was: \"Final title\")"),
        "{reply}"
    );
}

/// Nothing is lost when the value sent is the one already stored, or when
/// there were no notes to replace.
#[test]
fn resending_the_stored_value_or_filling_empty_notes_does_not_warn() {
    let (_sandbox, mut server) = project();
    server.ok(
        "handoff_update_task",
        json!({ "task": { "title": "Original title", "schedule": { "estimate_hours": 1 } } }),
    );
    update(&mut server, json!({ "id": "t1", "status": "done" }));

    let reply = update(
        &mut server,
        json!({ "id": "t1", "title": "Original title" }),
    );
    assert!(!reply.contains(CODE), "same title: {reply}");
    let reply = update(&mut server, json!({ "id": "t1", "notes": "first notes" }));
    assert!(!reply.contains(CODE), "no notes to overwrite: {reply}");
    let reply = update(&mut server, json!({ "id": "t1", "notes": "first notes" }));
    assert!(!reply.contains(CODE), "same notes: {reply}");
}

/// Fields outside the guarded set never warn, and a guarded list field that
/// holds nothing yet loses nothing when it is filled in.
#[test]
fn unguarded_fields_and_filling_an_empty_list_do_not_warn() {
    let (_sandbox, mut server) = project();
    task_in(&mut server, "done");

    for task in [
        json!({ "id": "t1", "priority": "high" }),
        json!({ "id": "t1", "schedule": { "estimate_hours": 2 } }),
        json!({ "id": "t1", "labels": ["x"] }),
        json!({ "id": "t1", "links": ["https://example.com"] }),
        json!({ "id": "t1", "scope_paths": ["src/"] }),
        json!({ "id": "t1", "done_criteria": [{ "item": "a" }] }),
    ] {
        let reply = update(&mut server, task.clone());
        assert!(!reply.contains(CODE), "{task}: {reply}");
    }
}

/// Creates `t1` with every list field populated, then moves it to `status`.
fn task_with_lists_in(server: &mut Server, status: &str) {
    server.ok(
        "handoff_update_task",
        json!({ "task": {
            "title": "Original title",
            "notes": "original notes",
            "labels": ["a", "b"],
            "links": ["https://example.com/old"],
            "scope_paths": ["src/old.rs"],
            "done_criteria": [
                { "item": "first", "checked": true },
                { "item": "second", "checked": true },
            ],
            "schedule": { "estimate_hours": 1 },
        } }),
    );
    server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "t1", "status": status } }),
    );
}

/// `(field, replacement value)` for every list field the warning covers.
fn list_field_patches() -> Vec<(&'static str, Value)> {
    vec![
        ("done_criteria", json!([{ "item": "other" }])),
        ("labels", json!(["z"])),
        ("links", json!(["https://example.com/new"])),
        ("scope_paths", json!(["src/new.rs"])),
    ]
}

#[test]
fn overwriting_a_list_field_of_a_done_task_warns_and_still_writes() {
    for (field, value) in list_field_patches() {
        let (_sandbox, mut server) = project();
        task_with_lists_in(&mut server, "done");

        let reply = update(&mut server, json!({ "id": "t1", field: value.clone() }));
        assert_eq!(
            warnings(&reply),
            vec![format!(
                "Warning [DONE_TASK_OVERWRITE]: Overwriting {field} of done task t1. Use \
                 force=true to suppress this warning."
            )],
            "{field}: {reply}"
        );
        assert_eq!(
            stored(&mut server)[field].as_array().unwrap().len(),
            value.as_array().unwrap().len(),
            "{field}"
        );
    }
}

#[test]
fn overwriting_dependencies_of_a_done_task_warns_and_still_writes() {
    let (_sandbox, mut server) = project();
    task_with_lists_in(&mut server, "done");
    for title in ["Dep one", "Dep two"] {
        server.ok("handoff_update_task", json!({ "task": { "title": title } }));
    }
    // Filling the empty dependency list loses nothing.
    let reply = update(&mut server, json!({ "id": "t1", "dependencies": ["t2"] }));
    assert!(!reply.contains(CODE), "{reply}");

    let reply = update(&mut server, json!({ "id": "t1", "dependencies": ["t3"] }));
    assert_eq!(
        warnings(&reply),
        vec![
            "Warning [DONE_TASK_OVERWRITE]: Overwriting dependencies of done task t1. Use \
             force=true to suppress this warning."
        ],
        "{reply}"
    );
    assert_eq!(stored(&mut server)["dependencies"], json!(["t3"]));
}

#[test]
fn a_review_task_warns_for_list_fields_too() {
    let (_sandbox, mut server) = project();
    task_with_lists_in(&mut server, "review");

    let reply = update(&mut server, json!({ "id": "t1", "labels": ["z"] }));
    assert_eq!(warnings(&reply).len(), 1, "{reply}");
    assert!(
        reply.contains("Overwriting labels of review task t1"),
        "{reply}"
    );
}

#[test]
fn several_overwritten_list_fields_warn_once_each() {
    let (_sandbox, mut server) = project();
    task_with_lists_in(&mut server, "done");

    let reply = update(
        &mut server,
        json!({ "id": "t1", "labels": ["z"], "links": [], "done_criteria": [] }),
    );
    let lines = warnings(&reply);
    assert_eq!(lines.len(), 3, "{reply}");
    for field in ["done_criteria", "labels", "links"] {
        assert!(
            lines
                .iter()
                .any(|l| l.contains(&format!("Overwriting {field} of done"))),
            "{field}: {reply}"
        );
    }
}

#[test]
fn resending_the_stored_list_does_not_warn() {
    let (_sandbox, mut server) = project();
    task_with_lists_in(&mut server, "done");

    let reply = update(
        &mut server,
        json!({
            "id": "t1",
            "labels": ["a", "b"],
            "links": ["https://example.com/old"],
            "scope_paths": ["src/old.rs"],
            "done_criteria": [
                { "item": "first", "checked": true },
                { "item": "second", "checked": true },
            ],
        }),
    );
    assert!(!reply.contains(CODE), "{reply}");
}

#[test]
fn force_suppresses_list_field_warnings_and_the_write_is_applied() {
    let (_sandbox, mut server) = project();
    task_with_lists_in(&mut server, "done");

    let reply = forced(
        &mut server,
        json!({ "id": "t1", "labels": ["z"], "done_criteria": [{ "item": "other" }] }),
    );
    assert!(!reply.contains(CODE), "{reply}");
    assert_eq!(stored(&mut server)["labels"], json!(["z"]));
}

#[test]
fn list_fields_of_open_tasks_are_never_warned_about() {
    for status in ["todo", "in_progress", "blocked"] {
        let (_sandbox, mut server) = project();
        task_with_lists_in(&mut server, status);

        let reply = update(
            &mut server,
            json!({
                "id": "t1",
                "labels": ["z"],
                "links": [],
                "scope_paths": [],
                "done_criteria": [{ "item": "other" }],
            }),
        );
        assert!(!reply.contains(CODE), "{status}: {reply}");
    }
}

// --- handoff_bulk_update_tasks ---

fn bulk(server: &mut Server, updates: Value, force: bool) -> Value {
    let mut arguments = json!({ "updates": updates });
    if force {
        arguments["force"] = json!(true);
    }
    server.call("handoff_bulk_update_tasks", arguments)
}

#[test]
fn bulk_update_warns_when_it_overwrites_the_notes_of_a_done_task() {
    let (_sandbox, mut server) = project();
    task_in(&mut server, "done");

    let result = bulk(
        &mut server,
        json!([{ "task_id": "t1", "notes": "replaced" }]),
        false,
    );
    assert_eq!(result["applied"], 1, "{result}");
    assert_eq!(result["errors"], json!([]), "{result}");
    let entries = result["warnings"].as_array().expect("warnings[]");
    assert_eq!(entries.len(), 1, "{result}");
    assert_eq!(entries[0]["task_id"], "t1");
    let warning = entries[0]["warning"].as_str().unwrap();
    assert!(
        warning.starts_with("Warning [DONE_TASK_OVERWRITE]: Overwriting notes of done task t1"),
        "{warning}"
    );
    assert_eq!(stored(&mut server)["notes"], "replaced");
}

#[test]
fn bulk_update_does_not_warn_for_append_force_or_an_open_task() {
    let (_sandbox, mut server) = project();
    task_in(&mut server, "done");
    // A second, open task in the same call.
    server.ok(
        "handoff_update_task",
        json!({ "task": { "title": "Open", "notes": "open notes" } }),
    );

    let result = bulk(
        &mut server,
        json!([
            { "task_id": "t1", "notes_append": "follow-up" },
            { "task_id": "t2", "notes": "replaced" },
        ]),
        false,
    );
    assert_eq!(result["applied"], 2, "{result}");
    assert!(result.get("warnings").is_none(), "{result}");

    let result = bulk(
        &mut server,
        json!([{ "task_id": "t1", "notes": "replaced" }]),
        true,
    );
    assert_eq!(result["applied"], 1, "{result}");
    assert!(result.get("warnings").is_none(), "force: {result}");
    assert_eq!(stored(&mut server)["notes"], "replaced");
}

/// `handoff_bulk_update_tasks` does not accept the list fields at all, so it
/// can never overwrite them: the whole update is rejected and nothing is
/// written.
#[test]
fn bulk_update_rejects_list_fields_instead_of_overwriting_them() {
    let (_sandbox, mut server) = project();
    task_with_lists_in(&mut server, "done");

    let result = bulk(
        &mut server,
        json!([{ "task_id": "t1", "labels": ["z"], "done_criteria": [{ "item": "x" }] }]),
        false,
    );
    assert_eq!(result["applied"], 0, "{result}");
    assert_eq!(result["errors"].as_array().unwrap().len(), 1, "{result}");
    assert_eq!(stored(&mut server)["labels"], json!(["a", "b"]));
}
