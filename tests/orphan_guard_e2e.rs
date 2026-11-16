//! The orphan guard (`[trace] orphan_guard`, REQ-TRC-002, SPEC-TRC-003 /
//! SPEC-TRC-004 / SPEC-TRC-005), through the real `handoff-mcp` binary over
//! stdio JSON-RPC.
//!
//! Moving a task with no requirement link into `review` or `done` warns
//! (`warn`, the default), is rejected (`block`, unless `force`), or does
//! nothing (`off`). The guard uses the same predicate as the `task_orphan`
//! lint rule, so a `no-trace` task, a parent traced through its children and
//! every task of a project without a layer document pass untouched.

#[path = "support/onboarding_e2e.rs"]
mod support;

use serde_json::{json, Value};
use support::{Sandbox, Server};

const REQUIREMENT_BODY: &str = "# Requirements\n\n## Section\n\n### REQ-001 The first requirement\n\n- priority: P1\n\nThe system does the first thing.\n\n受入基準:\n- AC1: it does\n";

/// The line every existing-task update that changes the status ends with.
const SCHEDULE_STALE: &str = "schedule_stale: true (run handoff_auto_schedule to recompute)";

/// A project whose `[trace] orphan_guard` is `mode` (`None`: the key is not
/// set), with or without a layer document.
fn project_with(mode: Option<&str>, layer_document: bool) -> (Sandbox, Server) {
    let sandbox = Sandbox::initialized("");
    let mut server = sandbox.server();
    // No estimate rule: the tests move bare tasks into review/done.
    server.ok(
        "handoff_update_config",
        json!({ "updates": { "settings.require_estimate_hours": false } }),
    );
    if let Some(mode) = mode {
        server.ok(
            "handoff_update_config",
            json!({ "updates": { "trace.orphan_guard": mode } }),
        );
    }
    if layer_document {
        server.ok(
            "handoff_doc_save",
            json!({
                "slug": "req-guard",
                "title": "Requirements",
                "doc_type": "spec",
                "layer": "requirement",
                "body": REQUIREMENT_BODY,
            }),
        );
    }
    (sandbox, server)
}

fn project(mode: Option<&str>) -> (Sandbox, Server) {
    project_with(mode, true)
}

/// Creates a `todo` task; `extra` is merged into the task object.
fn create(server: &mut Server, id: &str, title: &str, extra: Value) -> String {
    let mut task = json!({ "id": id, "title": title });
    for (key, value) in extra.as_object().expect("object") {
        task[key] = value.clone();
    }
    server.ok("handoff_update_task", json!({ "task": task }))
}

fn set_status(server: &mut Server, id: &str, status: &str) -> Result<String, String> {
    server.call_text(
        "handoff_update_task",
        json!({ "task": { "id": id, "status": status } }),
    )
}

fn task(server: &mut Server, id: &str) -> Value {
    server.call("handoff_get_task", json!({ "task_id": id }))
}

fn status_of(server: &mut Server, id: &str) -> String {
    task(server, id)["status"]
        .as_str()
        .expect("status")
        .to_string()
}

/// The warning line of `warn` mode for task `id`.
fn warning(id: &str) -> String {
    format!(
        "Warning [ORPHAN_TASK]: Task {id} has no requirement link. Link task {id} to the \
         requirement it implements with handoff_update_task(task: {{id: \"{id}\", \
         requirement_ids: [...]}}) (handoff_trace_propose suggests candidates), or add the \
         label \"no-trace\" if it is deliberately outside requirement traceability. The status \
         change was applied ([trace] orphan_guard = \"warn\")."
    )
}

fn assert_no_orphan_output(text: &str) {
    assert!(!text.contains("ORPHAN_TASK"), "{text}");
    assert!(!text.contains("orphan_guard"), "{text}");
}

// --- AC1: warn ---

#[test]
fn warn_is_the_default_and_warns_on_review_and_on_done_while_applying_both() {
    let (_sandbox, mut server) = project(None);
    create(&mut server, "a", "A", json!({}));

    let text = set_status(&mut server, "a", "review").expect("the transition applies");
    assert_eq!(
        text,
        format!(
            "Updated task a: A [review]\n{}\n{SCHEDULE_STALE}",
            warning("a")
        )
    );
    assert_eq!(status_of(&mut server, "a"), "review");

    let text = set_status(&mut server, "a", "done").expect("the transition applies");
    assert_eq!(
        text,
        format!(
            "Updated task a: A [done]\n{}\n{SCHEDULE_STALE}",
            warning("a")
        )
    );
    assert_eq!(status_of(&mut server, "a"), "done");
}

#[test]
fn an_explicit_warn_behaves_like_the_default() {
    let (_sandbox, mut server) = project(Some("warn"));
    create(&mut server, "a", "A", json!({}));
    let text = set_status(&mut server, "a", "done").unwrap();
    assert!(text.contains(&warning("a")), "{text}");
    assert_eq!(status_of(&mut server, "a"), "done");
}

#[test]
fn transitions_into_other_statuses_are_never_checked() {
    let (_sandbox, mut server) = project(Some("block"));
    create(&mut server, "a", "A", json!({}));
    for status in ["in_progress", "blocked", "todo", "skipped"] {
        let text = set_status(&mut server, "a", status).expect(status);
        assert_no_orphan_output(&text);
    }
}

// --- AC2: block ---

#[test]
fn block_rejects_review_and_done_and_leaves_the_task_untouched() {
    let (_sandbox, mut server) = project(Some("block"));
    create(&mut server, "a", "A", json!({ "notes": "original" }));
    let before = task(&mut server, "a");

    for status in ["review", "done"] {
        let error = server.err(
            "handoff_update_task",
            json!({ "task": { "id": "a", "status": status, "notes_append": "must not land", "priority": "high" } }),
        );
        assert!(
            error.contains("orphan_guard: task a has no requirement link"),
            "{error}"
        );
        assert!(
            error.contains("[trace] orphan_guard = \"block\" rejects this status change"),
            "{error}"
        );
        assert!(error.contains("force: true"), "{error}");
        assert!(error.contains("requirement_ids"), "{error}");
        assert!(error.contains("\"no-trace\""), "{error}");
        // Nothing of the rejected call landed: status, notes, priority, timestamp.
        assert_eq!(
            task(&mut server, "a"),
            before,
            "after a rejected move to {status}"
        );
    }
}

// --- AC3: off ---

#[test]
fn off_answers_exactly_as_before_the_guard_existed() {
    let (_sandbox, mut server) = project(Some("off"));
    create(&mut server, "a", "A", json!({}));

    assert_eq!(
        set_status(&mut server, "a", "review").unwrap(),
        format!("Updated task a: A [review]\n{SCHEDULE_STALE}")
    );
    assert_eq!(
        set_status(&mut server, "a", "done").unwrap(),
        format!("Updated task a: A [done]\n{SCHEDULE_STALE}")
    );
    // Creating straight into a guarded status: the guard adds nothing. The
    // `no-trace` label keeps the creation hint (REQ-TRC-003, a separate
    // feature with no off switch) out of the response, so it is the legacy
    // one, byte for byte.
    assert_eq!(
        server.ok(
            "handoff_update_task",
            json!({ "task": { "id": "b", "title": "B", "status": "done", "labels": ["no-trace"] } })
        ),
        "Created task b: B [done]"
    );
    // Without the label, with and without an id: still no guard output.
    for new_task in [
        json!({ "id": "c", "title": "C", "status": "done" }),
        json!({ "title": "D", "status": "review" }),
    ] {
        let created = server.ok("handoff_update_task", json!({ "task": new_task }));
        assert!(created.starts_with("Created task "), "{created}");
        assert_no_orphan_output(&created);
    }
}

// --- AC4: force ---

#[test]
fn force_overrides_block() {
    let (_sandbox, mut server) = project(Some("block"));
    create(&mut server, "a", "A", json!({}));

    let text = server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "a", "status": "done" }, "force": true }),
    );
    assert_eq!(text, format!("Updated task a: A [done]\n{SCHEDULE_STALE}"));
    assert_eq!(status_of(&mut server, "a"), "done");
}

/// `force` only overrides `block`: under `warn` the line is still there.
#[test]
fn force_does_not_silence_warn() {
    let (_sandbox, mut server) = project(Some("warn"));
    create(&mut server, "a", "A", json!({}));
    let text = server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "a", "status": "review" }, "force": true }),
    );
    assert!(text.contains(&warning("a")), "{text}");
}

// --- AC5: no-trace ---

#[test]
fn a_no_trace_task_passes_block_without_a_warning() {
    let (_sandbox, mut server) = project(Some("block"));
    create(
        &mut server,
        "a",
        "A",
        json!({ "labels": ["chore", "no-trace"] }),
    );
    create(&mut server, "b", "B", json!({}));

    let text = set_status(&mut server, "a", "done").expect("a no-trace task is exempt");
    assert_eq!(text, format!("Updated task a: A [done]\n{SCHEDULE_STALE}"));

    // The label may arrive in the very call that moves the task.
    let text = server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "b", "status": "review", "labels": ["no-trace"] } }),
    );
    assert_no_orphan_output(&text);
    assert_eq!(status_of(&mut server, "b"), "review");
}

#[test]
fn a_no_trace_task_gets_no_warning_in_warn_mode_either() {
    let (_sandbox, mut server) = project(Some("warn"));
    create(&mut server, "a", "A", json!({ "labels": ["no-trace"] }));
    let text = set_status(&mut server, "a", "review").unwrap();
    assert_eq!(
        text,
        format!("Updated task a: A [review]\n{SCHEDULE_STALE}")
    );
}

#[test]
fn removing_the_no_trace_label_in_the_same_call_is_judged_without_it() {
    let (_sandbox, mut server) = project(Some("block"));
    create(&mut server, "a", "A", json!({ "labels": ["no-trace"] }));
    let error = server.err(
        "handoff_update_task",
        json!({ "task": { "id": "a", "status": "review", "labels": [] } }),
    );
    assert!(error.contains("orphan_guard: task a"), "{error}");
    assert_eq!(task(&mut server, "a")["labels"], json!(["no-trace"]));
}

// --- requirement links ---

#[test]
fn a_linked_task_is_never_affected() {
    for mode in ["warn", "block"] {
        let (_sandbox, mut server) = project(Some(mode));
        create(
            &mut server,
            "a",
            "A",
            json!({ "requirement_ids": ["REQ-001"] }),
        );
        for status in ["review", "done"] {
            let text = set_status(&mut server, "a", status).expect(status);
            assert_no_orphan_output(&text);
        }
    }
}

#[test]
fn linking_in_the_same_call_passes_block_and_an_unknown_id_does_not() {
    let (_sandbox, mut server) = project(Some("block"));
    create(&mut server, "a", "A", json!({}));
    create(&mut server, "b", "B", json!({}));

    let text = server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "a", "status": "done", "requirement_ids": ["REQ-001"] } }),
    );
    assert_no_orphan_output(&text);
    assert_eq!(status_of(&mut server, "a"), "done");

    // An id that resolves to nothing adds no link, so the task stays an orphan.
    let error = server.err(
        "handoff_update_task",
        json!({ "task": { "id": "b", "status": "done", "requirement_ids": ["REQ-404"] } }),
    );
    assert!(error.contains("orphan_guard: task b"), "{error}");
    assert_eq!(status_of(&mut server, "b"), "todo");
    assert_eq!(task(&mut server, "b")["task_links"], json!([]));
}

#[test]
fn removing_the_last_link_in_the_same_call_is_rejected_under_block() {
    let (_sandbox, mut server) = project(Some("block"));
    create(
        &mut server,
        "a",
        "A",
        json!({ "requirement_ids": ["REQ-001"] }),
    );

    let error = server.err(
        "handoff_update_task",
        json!({ "task": { "id": "a", "status": "review", "requirement_ids": [] } }),
    );
    assert!(error.contains("orphan_guard: task a"), "{error}");
    let after = task(&mut server, "a");
    assert_eq!(after["status"], "todo");
    assert_eq!(
        after["task_links"][0]["label"], "REQ-001",
        "the link is still there"
    );
}

#[test]
fn a_document_link_does_not_count_as_a_requirement_link() {
    let (_sandbox, mut server) = project(Some("block"));
    create(&mut server, "a", "A", json!({}));
    server.ok(
        "handoff_doc_save",
        json!({ "slug": "note", "title": "Note", "doc_type": "note", "body": "# Note\n\ntext\n", "task_ids": ["a"] }),
    );
    let error = set_status(&mut server, "a", "review").unwrap_err();
    assert!(error.contains("orphan_guard: task a"), "{error}");
}

// --- only a transition is checked ---

#[test]
fn updating_a_task_that_stays_in_review_or_done_is_not_checked() {
    let (_sandbox, mut server) = project(Some("block"));
    create(&mut server, "a", "A", json!({}));
    server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "a", "status": "review" }, "force": true }),
    );

    // Same status again, and other fields with the status unchanged.
    let text = server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "a", "status": "review", "notes_append": "still in review" } }),
    );
    assert_eq!(text, "Updated task a: A [review]");
    let text = server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "a", "priority": "low" } }),
    );
    assert_eq!(text, "Updated task a: A [review]");

    // review -> done is a transition again.
    let error = set_status(&mut server, "a", "done").unwrap_err();
    assert!(error.contains("orphan_guard: task a"), "{error}");
}

// --- the parent rule (same predicate as the lint) ---

#[test]
fn a_parent_traced_through_its_children_passes_and_a_partly_traced_one_does_not() {
    let (_sandbox, mut server) = project(Some("block"));
    create(&mut server, "covered", "Covered", json!({}));
    create(
        &mut server,
        "covered.1",
        "linked",
        json!({ "requirement_ids": ["REQ-001"] }),
    );
    create(
        &mut server,
        "covered.2",
        "exempt",
        json!({ "labels": ["no-trace"] }),
    );
    create(
        &mut server,
        "covered.3",
        "dropped",
        json!({ "status": "skipped" }),
    );
    create(&mut server, "partial", "Partial", json!({}));
    create(
        &mut server,
        "partial.1",
        "linked",
        json!({ "requirement_ids": ["REQ-001"] }),
    );
    create(&mut server, "partial.2", "unlinked", json!({}));
    create(&mut server, "deep", "Deep", json!({}));
    create(
        &mut server,
        "deep.1",
        "linked",
        json!({ "requirement_ids": ["REQ-001"] }),
    );
    create(&mut server, "deep.1.1", "unlinked grandchild", json!({}));

    let text = set_status(&mut server, "covered", "review").expect("traced through its children");
    assert_no_orphan_output(&text);

    for parent in ["partial", "deep"] {
        let error = set_status(&mut server, parent, "review").unwrap_err();
        assert!(
            error.contains(&format!(
                "orphan_guard: task {parent} has no requirement link"
            )),
            "{error}"
        );
        assert_eq!(status_of(&mut server, parent), "todo");
    }
    // `deep.1` is linked itself: its own transition is fine.
    let text = set_status(&mut server, "deep.1", "review").expect("a linked task");
    assert_no_orphan_output(&text);
}

/// SPEC-TRC-002 AC3: for one task tree, the tasks `block` rejects are exactly
/// the tasks the `task_orphan` lint rule reports.
#[test]
fn the_guard_rejects_exactly_the_tasks_the_lint_reports() {
    let (_sandbox, mut server) = project(Some("block"));
    let all = [
        ("leaf", json!({})),
        ("traced", json!({ "requirement_ids": ["REQ-001"] })),
        ("exempt", json!({ "labels": ["no-trace"] })),
        ("covered", json!({})),
        ("covered.1", json!({ "requirement_ids": ["REQ-001"] })),
        ("covered.2", json!({ "labels": ["no-trace"] })),
        ("partial", json!({})),
        ("partial.1", json!({ "requirement_ids": ["REQ-001"] })),
        ("partial.2", json!({})),
        ("deep", json!({})),
        ("deep.1", json!({ "requirement_ids": ["REQ-001"] })),
        ("deep.1.1", json!({})),
    ];
    for (id, extra) in &all {
        create(&mut server, id, id, extra.clone());
    }
    let lint = server.call("handoff_trace_lint", json!({ "rules": ["task_orphan"] }));
    let reported: Vec<String> = lint["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["task_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        reported,
        ["deep", "deep.1.1", "leaf", "partial", "partial.2"]
    );

    // Children first, so a parent is judged with its children still as they
    // were when the lint ran (`review` is not a status the predicate reads).
    let mut rejected = Vec::new();
    for (id, _) in all.iter().rev() {
        if set_status(&mut server, id, "review").is_err() {
            rejected.push(id.to_string());
        }
    }
    rejected.sort();
    assert_eq!(rejected, reported);
}

// --- scope guard ---

#[test]
fn a_project_without_a_layer_document_is_never_affected() {
    let (_sandbox, mut server) = project_with(Some("block"), false);
    create(&mut server, "a", "A", json!({}));
    // A document without a layer does not switch the guard on.
    server.ok(
        "handoff_doc_save",
        json!({ "slug": "note", "title": "Note", "doc_type": "note", "body": "# Note\n\ntext\n" }),
    );

    assert_eq!(
        set_status(&mut server, "a", "review").unwrap(),
        format!("Updated task a: A [review]\n{SCHEDULE_STALE}")
    );
    assert_eq!(
        set_status(&mut server, "a", "done").unwrap(),
        format!("Updated task a: A [done]\n{SCHEDULE_STALE}")
    );
    assert_eq!(
        server.ok(
            "handoff_update_task",
            json!({ "task": { "id": "b", "title": "B", "status": "done" } })
        ),
        "Created task b: B [done]"
    );
}

#[test]
fn the_guard_starts_once_a_layer_document_exists() {
    let (_sandbox, mut server) = project_with(None, false);
    create(&mut server, "a", "A", json!({}));
    let text = set_status(&mut server, "a", "review").unwrap();
    assert_no_orphan_output(&text);

    server.ok(
        "handoff_doc_save",
        json!({ "slug": "req-guard", "title": "Requirements", "doc_type": "spec", "layer": "requirement", "body": REQUIREMENT_BODY }),
    );
    let text = set_status(&mut server, "a", "done").unwrap();
    assert!(text.contains(&warning("a")), "{text}");
}

// --- creating a task straight into review / done ---

#[test]
fn creating_an_orphan_task_in_a_guarded_status_warns_or_is_rejected() {
    // warn: created, with the warning.
    let (_sandbox, mut server) = project(Some("warn"));
    let text = server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "a", "title": "A", "status": "done" } }),
    );
    assert!(text.starts_with("Created task a: A [done]\n"), "{text}");
    assert!(text.contains(&warning("a")), "{text}");
    let text = server.ok(
        "handoff_update_task",
        json!({ "task": { "title": "B", "status": "review" } }),
    );
    assert!(text.contains(&warning("t1")), "{text}");

    // block: rejected, and nothing is left behind.
    let (_sandbox, mut server) = project(Some("block"));
    let error = server.err(
        "handoff_update_task",
        json!({ "task": { "id": "a", "title": "A", "status": "done" } }),
    );
    assert!(
        error.contains("orphan_guard: task a has no requirement link"),
        "{error}"
    );
    assert!(
        error.contains("[trace] orphan_guard = \"block\" rejects creating this task directly in status \"done\""),
        "{error}"
    );
    let error = server.err(
        "handoff_update_task",
        json!({ "task": { "title": "B", "status": "review" } }),
    );
    assert!(error.contains("orphan_guard: task t1"), "{error}");
    let listed = server.call("handoff_list_tasks", json!({}));
    assert_eq!(listed["task_tree"], json!([]), "{listed}");
    // The rejected auto-numbered create did not burn its id.
    let text = server.ok("handoff_update_task", json!({ "task": { "title": "C" } }));
    assert!(text.starts_with("Created task t1: C [todo]"), "{text}");

    // block: a create that links, is labelled no-trace, or is forced goes through.
    for (id, extra, force) in [
        ("linked", json!({ "requirement_ids": ["REQ-001"] }), false),
        ("exempt", json!({ "labels": ["no-trace"] }), false),
        ("forced", json!({}), true),
    ] {
        let mut new_task = json!({ "id": id, "title": id, "status": "done" });
        for (key, value) in extra.as_object().unwrap() {
            new_task[key] = value.clone();
        }
        let text = server.ok(
            "handoff_update_task",
            json!({ "task": new_task, "force": force }),
        );
        assert!(
            text.starts_with(&format!("Created task {id}: {id} [done]")),
            "{text}"
        );
        assert_no_orphan_output(&text);
    }
}

// --- handoff_bulk_update_tasks (SPEC-TRC-005) ---

fn bulk(server: &mut Server, updates: Value, force: bool) -> Value {
    let mut arguments = json!({ "updates": updates });
    if force {
        arguments["force"] = json!(true);
    }
    server.call("handoff_bulk_update_tasks", arguments)
}

fn bulk_fixture(mode: &str) -> (Sandbox, Server) {
    let (sandbox, mut server) = project(Some(mode));
    create(&mut server, "orphan", "Orphan", json!({}));
    create(
        &mut server,
        "traced",
        "Traced",
        json!({ "requirement_ids": ["REQ-001"] }),
    );
    create(
        &mut server,
        "exempt",
        "Exempt",
        json!({ "labels": ["no-trace"] }),
    );
    (sandbox, server)
}

fn all_done() -> Value {
    json!([
        { "task_id": "orphan", "status": "done" },
        { "task_id": "traced", "status": "done" },
        { "task_id": "exempt", "status": "done" },
    ])
}

#[test]
fn bulk_block_rejects_only_the_orphan_task() {
    let (_sandbox, mut server) = bulk_fixture("block");
    let result = bulk(&mut server, all_done(), false);

    assert_eq!(result["applied"], 2, "{result}");
    let errors = result["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1, "{result}");
    assert_eq!(errors[0]["task_id"], "orphan");
    let message = errors[0]["error"].as_str().unwrap();
    assert!(
        message.contains("orphan_guard: task orphan has no requirement link"),
        "{message}"
    );
    assert!(message.contains("force: true"), "{message}");
    assert_eq!(status_of(&mut server, "orphan"), "todo");
    assert_eq!(status_of(&mut server, "traced"), "done");
    assert_eq!(status_of(&mut server, "exempt"), "done");

    // force overrides it for the whole call.
    let result = bulk(
        &mut server,
        json!([{ "task_id": "orphan", "status": "done" }]),
        true,
    );
    assert_eq!(result, json!({ "applied": 1, "errors": [] }));
    assert_eq!(status_of(&mut server, "orphan"), "done");
}

#[test]
fn bulk_warn_applies_everything_and_warns_per_orphan_task() {
    let (_sandbox, mut server) = bulk_fixture("warn");
    create(&mut server, "second", "Second orphan", json!({}));
    let mut updates = all_done();
    updates
        .as_array_mut()
        .unwrap()
        .push(json!({ "task_id": "second", "status": "review" }));
    let result = bulk(&mut server, updates, false);

    assert_eq!(result["applied"], 4, "{result}");
    assert_eq!(result["errors"], json!([]));
    let warned: Vec<(&str, &str)> = result["warnings"]
        .as_array()
        .expect("warnings")
        .iter()
        .filter(|w| w["warning"].as_str().unwrap().contains("ORPHAN_TASK"))
        .map(|w| {
            (
                w["task_id"].as_str().unwrap(),
                w["warning"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(warned.len(), 2, "{result}");
    assert_eq!(warned[0].0, "orphan");
    assert!(warned[0].1.contains(&warning("orphan")), "{result}");
    assert_eq!(warned[1].0, "second");
    assert!(warned[1].1.contains(&warning("second")), "{result}");
    assert_eq!(status_of(&mut server, "orphan"), "done");
    assert_eq!(status_of(&mut server, "second"), "review");
}

#[test]
fn bulk_off_answers_exactly_as_before_the_guard_existed() {
    let (_sandbox, mut server) = bulk_fixture("off");
    let result = bulk(
        &mut server,
        json!([
            { "task_id": "orphan", "status": "done" },
            { "task_id": "exempt", "status": "review" },
        ]),
        false,
    );
    assert_eq!(result, json!({ "applied": 2, "errors": [] }));
}

#[test]
fn bulk_in_a_project_without_a_layer_document_is_never_affected() {
    let (_sandbox, mut server) = project_with(Some("block"), false);
    create(&mut server, "a", "A", json!({}));
    let result = bulk(
        &mut server,
        json!([{ "task_id": "a", "status": "done" }]),
        false,
    );
    assert_eq!(result, json!({ "applied": 1, "errors": [] }));
}

// --- the existing done guard is untouched ---

#[test]
fn the_done_guard_of_a_linked_task_still_reports_its_blockers() {
    let (_sandbox, mut server) = project(Some("off"));
    server.ok(
        "handoff_doc_save",
        json!({
            "slug": "at-guard",
            "title": "Acceptance",
            "doc_type": "spec",
            "layer": "acceptance",
            "body": "# Acceptance\n\n### AT-001 Confirms it\n\n- verifies: REQ-001\n- method: manual\n\nBody.\n",
        }),
    );
    create(
        &mut server,
        "a",
        "A",
        json!({ "requirement_ids": ["REQ-001"] }),
    );
    let off = set_status(&mut server, "a", "review").unwrap();
    assert!(off.contains("done_guard: task a has"), "{off}");

    // Same project with the orphan guard on: the done-guard line is the same.
    server.ok(
        "handoff_update_config",
        json!({ "updates": { "trace.orphan_guard": "block" } }),
    );
    server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "a", "status": "todo" } }),
    );
    let block = set_status(&mut server, "a", "review").unwrap();
    assert_eq!(block, off);
}

// --- a layer document that cannot be read ---

/// Breaks the frontmatter of the fixture's layer document, so no readable
/// document declares a layer any more.
fn corrupt_layer_document(sandbox: &Sandbox) {
    let path = sandbox.handoff().join("docs/_doc.req-guard.md");
    let text = std::fs::read_to_string(&path).expect("the layer document");
    std::fs::write(&path, text.replacen("---\n", "---\n: : broken [\n", 1)).unwrap();
}

/// When the only layer document cannot be read, the guard cannot tell
/// whether the project uses requirement traceability. `block` then fails
/// closed instead of letting an orphan through, and `warn` says the check
/// could not be made instead of staying silent.
#[test]
fn an_unreadable_layer_document_fails_closed_under_block_and_is_reported_under_warn() {
    let (sandbox, mut server) = project(Some("block"));
    create(&mut server, "a", "A", json!({}));
    create(
        &mut server,
        "exempt",
        "E",
        json!({ "labels": ["no-trace"] }),
    );
    corrupt_layer_document(&sandbox);

    let rejected = set_status(&mut server, "a", "done").unwrap_err();
    assert!(rejected.contains("orphan_guard check failed"), "{rejected}");
    assert!(rejected.contains("could not be read"), "{rejected}");
    assert!(rejected.contains("req-guard"), "{rejected}");
    assert!(
        rejected.contains("handoff_doc_repair_frontmatter"),
        "{rejected}"
    );
    assert_eq!(status_of(&mut server, "a"), "todo");

    // A task that is accounted for by itself needs no document.
    let exempt = set_status(&mut server, "exempt", "done").unwrap();
    assert_no_orphan_output(&exempt);

    // force overrides block here as everywhere else.
    let forced = server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "a", "status": "review" }, "force": true }),
    );
    assert_no_orphan_output(&forced);
    assert_eq!(status_of(&mut server, "a"), "review");

    server.ok(
        "handoff_update_config",
        json!({ "updates": { "trace.orphan_guard": "warn" } }),
    );
    let warned = set_status(&mut server, "a", "done").unwrap();
    assert!(
        warned.contains("\nWarning: orphan_guard check failed: "),
        "{warned}"
    );
    assert!(warned.contains("req-guard"), "{warned}");
    assert_eq!(status_of(&mut server, "a"), "done");
}

/// One readable layer document is enough to know: a second, unreadable
/// document changes nothing.
#[test]
fn an_unreadable_document_next_to_a_readable_layer_document_changes_nothing() {
    let (sandbox, mut server) = project(Some("block"));
    server.ok(
        "handoff_doc_save",
        json!({
            "slug": "at-guard",
            "title": "Acceptance",
            "doc_type": "spec",
            "layer": "acceptance",
            "body": "# Acceptance\n\n### AT-001 Confirms it\n\n- verifies: REQ-001\n- method: manual\n\nBody.\n",
        }),
    );
    create(&mut server, "a", "A", json!({}));
    corrupt_layer_document(&sandbox);

    let rejected = set_status(&mut server, "a", "done").unwrap_err();
    assert!(
        rejected.contains("orphan_guard: task a has no requirement link."),
        "{rejected}"
    );
    assert!(!rejected.contains("check failed"), "{rejected}");
}

/// A project that never had a layer document is not affected by a broken
/// plain document under `off`, and no task is ever rejected for it there.
#[test]
fn off_never_reads_the_documents() {
    let (sandbox, mut server) = project(Some("off"));
    create(&mut server, "a", "A", json!({}));
    corrupt_layer_document(&sandbox);
    assert_eq!(
        set_status(&mut server, "a", "done").unwrap(),
        format!("Updated task a: A [done]\n{SCHEDULE_STALE}")
    );
}
