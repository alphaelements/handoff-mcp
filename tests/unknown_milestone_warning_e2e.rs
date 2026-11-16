//! The `UNKNOWN_MILESTONE` warning, through the real `handoff-mcp` binary over
//! stdio JSON-RPC.
//!
//! A task may be assigned to any milestone name. A name with no
//! `[milestones.<name>]` definition in `config.toml` is most often a typing
//! mistake, so every call that passes such a name in `schedule.milestone`
//! answers with a warning. The write still succeeds, and a call that passes a
//! defined name, or no milestone at all, answers exactly as it did before.

#[path = "support/onboarding_e2e.rs"]
mod support;

use serde_json::{json, Value};
use support::{Sandbox, Server};

/// `v1` is defined, `ice` is defined and frozen, `v9` is defined nowhere.
const MILESTONES: &str = "[milestones.v1]\n\n[milestones.ice]\nstatus = \"frozen\"\n";

fn project() -> (Sandbox, Server) {
    let sandbox = Sandbox::initialized(MILESTONES);
    let server = sandbox.server();
    (sandbox, server)
}

fn unknown(name: &str) -> String {
    format!(
        "Warning [UNKNOWN_MILESTONE]: Milestone '{name}' does not match any milestone defined \
         in config.toml. Check the milestone name spelling or define it with \
         handoff_add_milestone. The assignment was still saved."
    )
}

fn milestone_of(server: &mut Server, id: &str) -> Value {
    server.call("handoff_get_task", json!({ "task_id": id }))["schedule"]["milestone"].clone()
}

// --- handoff_update_task ---

#[test]
fn creating_a_task_in_an_unknown_milestone_warns_and_still_creates_it() {
    let (_sandbox, mut server) = project();

    let text = server.ok(
        "handoff_update_task",
        json!({ "task": { "title": "A", "schedule": { "milestone": "v9" } } }),
    );
    assert_eq!(
        text,
        format!("Created task t1: A [todo]\n{}", unknown("v9"))
    );
    assert_eq!(milestone_of(&mut server, "t1"), "v9");

    let text = server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "custom", "title": "B", "schedule": { "milestone": "v9" } } }),
    );
    assert_eq!(
        text,
        format!("Created task custom: B [todo]\n{}", unknown("v9"))
    );
    assert_eq!(milestone_of(&mut server, "custom"), "v9");
}

#[test]
fn updating_a_task_with_an_unknown_milestone_warns_every_time_it_is_passed() {
    let (_sandbox, mut server) = project();
    server.ok("handoff_update_task", json!({ "task": { "title": "A" } }));

    let moved = server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "t1", "schedule": { "milestone": "v9" } } }),
    );
    assert_eq!(
        moved,
        format!("Updated task t1: A [todo]\n{}", unknown("v9"))
    );
    assert_eq!(milestone_of(&mut server, "t1"), "v9");

    // The same name again: it is still not defined, so it still warns.
    let again = server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "t1", "schedule": { "milestone": "v9" } } }),
    );
    assert_eq!(again, moved);
}

/// A call that does not pass `schedule.milestone` says nothing about the
/// milestone the task already has.
#[test]
fn an_update_that_does_not_pass_the_milestone_does_not_warn() {
    let (_sandbox, mut server) = project();
    server.ok(
        "handoff_update_task",
        json!({ "task": { "title": "A", "schedule": { "milestone": "v9" } } }),
    );

    for task in [
        json!({ "id": "t1", "notes": "n" }),
        json!({ "id": "t1", "schedule": { "estimate_hours": 2 } }),
        json!({ "id": "t1", "status": "in_progress" }),
    ] {
        let text = server.ok("handoff_update_task", json!({ "task": task }));
        assert!(
            !text.contains("UNKNOWN_MILESTONE"),
            "{task} must not warn: {text}"
        );
    }
    assert_eq!(milestone_of(&mut server, "t1"), "v9");
}

#[test]
fn a_defined_milestone_or_no_milestone_answers_as_before() {
    let (_sandbox, mut server) = project();

    let created = server.ok(
        "handoff_update_task",
        json!({ "task": { "title": "A", "schedule": { "milestone": "v1" } } }),
    );
    assert_eq!(created, "Created task t1: A [todo]");
    let created = server.ok("handoff_update_task", json!({ "task": { "title": "B" } }));
    assert_eq!(created, "Created task t2: B [todo]");
    let created = server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "custom", "title": "C", "schedule": { "milestone": "v1" } } }),
    );
    assert_eq!(created, "Created task custom: C [todo]");

    let updated = server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "t2", "schedule": { "milestone": "v1" } } }),
    );
    assert_eq!(updated, "Updated task t2: B [todo]");
    let updated = server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "t2", "schedule": { "milestone": "v1" } } }),
    );
    assert_eq!(updated, "Updated task t2: B [todo]");
    let updated = server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "t2", "notes": "n" } }),
    );
    assert_eq!(updated, "Updated task t2: B [todo]");
}

/// A frozen milestone is defined: its own warning is the only one.
#[test]
fn a_frozen_milestone_keeps_its_own_warning_and_is_not_unknown() {
    let (_sandbox, mut server) = project();

    let text = server.ok(
        "handoff_update_task",
        json!({ "task": { "title": "A", "schedule": { "milestone": "ice" } } }),
    );
    assert_eq!(
        text,
        "Created task t1: A [todo]\nWarning: milestone 'ice' is frozen; the task was still \
         assigned to it. Move it elsewhere, or unfreeze the milestone with \
         handoff_update_milestone (status, force: true) if this is unintended."
    );
}

/// An empty string names no milestone, so there is no name to report.
#[test]
fn an_empty_milestone_name_does_not_warn() {
    let (_sandbox, mut server) = project();

    let text = server.ok(
        "handoff_update_task",
        json!({ "task": { "title": "A", "schedule": { "milestone": "" } } }),
    );
    assert_eq!(text, "Created task t1: A [todo]");
}

/// A milestone name is matched exactly, as everywhere else.
#[test]
fn a_name_that_differs_in_case_or_spacing_is_unknown() {
    let (_sandbox, mut server) = project();

    for name in ["V1", "v1 ", "v1.0"] {
        let text = server.ok(
            "handoff_update_task",
            json!({ "task": { "title": "A", "schedule": { "milestone": name } } }),
        );
        assert!(text.ends_with(&unknown(name)), "{name:?}: {text}");
    }
}

/// A milestone defined after the warning stops it.
#[test]
fn defining_the_milestone_stops_the_warning() {
    let (_sandbox, mut server) = project();
    let text = server.ok(
        "handoff_update_task",
        json!({ "task": { "title": "A", "schedule": { "milestone": "v9" } } }),
    );
    assert!(text.contains("UNKNOWN_MILESTONE"), "{text}");

    server.ok("handoff_add_milestone", json!({ "name": "v9" }));

    let text = server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "t1", "schedule": { "milestone": "v9" } } }),
    );
    assert_eq!(text, "Updated task t1: A [todo]");
}

/// Without a readable `config.toml` nobody can tell which milestones are
/// defined: the task is written and no milestone is called unknown.
#[test]
fn an_unreadable_config_does_not_call_the_milestone_unknown() {
    let (sandbox, mut server) = project();
    server.ok("handoff_update_task", json!({ "task": { "title": "A" } }));
    std::fs::write(sandbox.handoff().join("config.toml"), "not [ toml").unwrap();

    let text = server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "t1", "schedule": {
            "milestone": "v9", "estimate_hours": 1,
        } } }),
    );
    assert!(text.starts_with("Updated task t1: A [todo]"), "{text}");
    assert!(!text.contains("UNKNOWN_MILESTONE"), "{text}");
}

// --- the other writers of schedule.milestone ---

#[test]
fn bulk_update_warns_for_the_tasks_given_an_unknown_milestone_only() {
    let (_sandbox, mut server) = project();
    for title in ["A", "B", "C"] {
        server.ok("handoff_update_task", json!({ "task": { "title": title } }));
    }

    let out = server.call(
        "handoff_bulk_update_tasks",
        json!({ "updates": [
            { "task_id": "t1", "schedule": { "milestone": "v9" } },
            { "task_id": "t2", "schedule": { "milestone": "v1" } },
            { "task_id": "t3", "priority": "high" },
        ] }),
    );
    assert_eq!(
        out,
        json!({
            "applied": 3,
            "errors": [],
            "warnings": [{ "task_id": "t1", "warning": unknown("v9") }],
        })
    );
    assert_eq!(milestone_of(&mut server, "t1"), "v9");
    assert_eq!(milestone_of(&mut server, "t2"), "v1");
}

/// The response of a bulk update without an unknown milestone has no
/// `warnings` key, as before.
#[test]
fn bulk_update_with_defined_milestones_answers_as_before() {
    let (_sandbox, mut server) = project();
    server.ok("handoff_update_task", json!({ "task": { "title": "A" } }));

    let out = server.call(
        "handoff_bulk_update_tasks",
        json!({ "updates": [{ "task_id": "t1", "schedule": { "milestone": "v1" } }] }),
    );
    assert_eq!(out, json!({ "applied": 1, "errors": [] }));
}

/// An import names each unknown milestone once, however many of its tasks
/// use it, and still creates every task.
#[test]
fn import_context_names_each_unknown_milestone_once() {
    let (_sandbox, mut server) = project();

    let text = server.ok(
        "handoff_import_context",
        json!({
            "source": { "description": "milestone test", "format": "other" },
            "skip_session_close": true,
            "tasks": [
                { "title": "A", "schedule": { "milestone": "v9" }, "children": [
                    { "title": "A1", "schedule": { "milestone": "v9" } },
                    { "title": "A2", "schedule": { "milestone": "v8" } },
                ] },
                { "title": "B", "schedule": { "milestone": "v1" } },
                { "title": "C" },
            ],
        }),
    );
    assert_eq!(
        text,
        format!(
            "Import complete:\n  Source: milestone test\n  Tasks created: 5 (3 top-level, 2 \
             nested)\n  {}\n  {}",
            unknown("v8"),
            unknown("v9"),
        )
    );
    assert_eq!(milestone_of(&mut server, "t1.1"), "v9");
    assert_eq!(milestone_of(&mut server, "t1.2"), "v8");
}

#[test]
fn import_context_with_defined_milestones_answers_as_before() {
    let (_sandbox, mut server) = project();

    let text = server.ok(
        "handoff_import_context",
        json!({
            "source": { "description": "milestone test", "format": "other" },
            "skip_session_close": true,
            "tasks": [{ "title": "B", "schedule": { "milestone": "v1" } }, { "title": "C" }],
        }),
    );
    assert_eq!(
        text,
        "Import complete:\n  Source: milestone test\n  Tasks created: 2 (2 top-level, 0 nested)"
    );
}
