//! Task id placement and allocation, through the real `handoff-mcp` binary
//! over stdio JSON-RPC.
//!
//! A task id is unique in a project. Two rules keep it so:
//!
//! - an id given by the caller that contains a dot (`t-book.16`) names a
//!   child of the task before the last dot, so it is created under that task
//!   or not at all;
//! - an id allocated by the server is never one that is already in use
//!   anywhere in the task tree.

use crate::support::onboarding_e2e as support;

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use support::{Sandbox, Server};

fn project() -> (Sandbox, Server) {
    let sandbox = Sandbox::initialized("");
    let server = sandbox.server();
    (sandbox, server)
}

/// Creates a task and returns the id the server reports for it.
fn create(server: &mut Server, arguments: Value) -> String {
    let text = server.ok("handoff_update_task", arguments);
    created_id(&text)
}

/// The id in a `Created task <id>: <title> [<status>]` confirmation.
fn created_id(text: &str) -> String {
    text.strip_prefix("Created task ")
        .and_then(|rest| rest.split_once(": "))
        .map(|(id, _)| id.to_string())
        .unwrap_or_else(|| panic!("not a creation message: {text}"))
}

/// Every task id in the project with the ids of its ancestors, from
/// `handoff_list_tasks`: `(id, parent id or None)`.
fn listed(server: &mut Server) -> Vec<(String, Option<String>)> {
    fn walk(nodes: &Value, parent: Option<&str>, out: &mut Vec<(String, Option<String>)>) {
        for node in nodes.as_array().into_iter().flatten() {
            let id = node["id"].as_str().expect("id").to_string();
            out.push((id.clone(), parent.map(String::from)));
            walk(&node["children"], Some(&id), out);
        }
    }
    let list = server.call("handoff_list_tasks", json!({}));
    let mut out = Vec::new();
    walk(&list["task_tree"], None, &mut out);
    out
}

fn count_of(tasks: &[(String, Option<String>)], id: &str) -> usize {
    tasks.iter().filter(|(task_id, _)| task_id == id).count()
}

fn parent_of(tasks: &[(String, Option<String>)], id: &str) -> Option<String> {
    let mut matches = tasks.iter().filter(|(task_id, _)| task_id == id);
    let (_, parent) = matches
        .next()
        .unwrap_or_else(|| panic!("no task {id} in {tasks:?}"));
    assert!(matches.next().is_none(), "{id} is listed twice: {tasks:?}");
    parent.clone()
}

/// Every directory below `.handoff/tasks`, relative to it.
fn task_dirs(sandbox: &Sandbox) -> Vec<PathBuf> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                out.push(entry.path().strip_prefix(root).unwrap().to_path_buf());
                walk(root, &entry.path(), out);
            }
        }
    }
    let root = sandbox.handoff().join("tasks");
    let mut out = Vec::new();
    walk(&root, &root, &mut out);
    out.sort();
    out
}

/// Writes a task straight to disk, the way an older build left it.
fn write_task_on_disk(sandbox: &Sandbox, relative_dir: &str, id: &str, status: &str) {
    let dir = sandbox.handoff().join("tasks").join(relative_dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("_task.{status}.json")),
        json!({ "id": id, "title": format!("on disk {id}") }).to_string(),
    )
    .unwrap();
}

// --- the reported defect ---

/// The report: `t-book.16` given without `parent_id` became a top-level task,
/// and the next child of `t-book` was numbered `t-book.16` as well.
#[test]
fn a_dotted_id_without_parent_id_does_not_end_in_two_tasks_with_one_id() {
    let (_sandbox, mut server) = project();
    create(
        &mut server,
        json!({ "task": { "id": "t-book", "title": "Book" } }),
    );
    for n in 1..=15 {
        let id = create(
            &mut server,
            json!({ "parent_id": "t-book", "task": { "title": format!("Chapter {n}") } }),
        );
        assert_eq!(id, format!("t-book.{n}"));
    }

    let dotted = create(
        &mut server,
        json!({ "task": { "id": "t-book.16", "title": "Given a dotted id" } }),
    );
    assert_eq!(dotted, "t-book.16");
    let numbered = create(
        &mut server,
        json!({ "parent_id": "t-book", "task": { "title": "Numbered by the server" } }),
    );
    assert_eq!(numbered, "t-book.17");

    let tasks = listed(&mut server);
    assert_eq!(count_of(&tasks, "t-book.16"), 1, "{tasks:?}");
    assert_eq!(parent_of(&tasks, "t-book.16").as_deref(), Some("t-book"));
    assert_eq!(parent_of(&tasks, "t-book.17").as_deref(), Some("t-book"));
    let mut ids: Vec<&str> = tasks.iter().map(|(id, _)| id.as_str()).collect();
    let total = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), total, "an id is listed twice: {tasks:?}");
}

// --- a dotted id names its parent ---

#[test]
fn a_dotted_id_without_parent_id_is_created_under_the_task_it_names() {
    let (sandbox, mut server) = project();
    create(&mut server, json!({ "task": { "title": "Parent" } }));

    let text = server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "t1.4", "title": "Child" } }),
    );
    assert_eq!(text, "Created task t1.4: Child [todo]");

    let tasks = listed(&mut server);
    assert_eq!(parent_of(&tasks, "t1.4").as_deref(), Some("t1"));
    assert_eq!(
        task_dirs(&sandbox),
        [
            PathBuf::from("t1-parent"),
            PathBuf::from("t1-parent/t1.4-child")
        ]
    );
    let task = server.call("handoff_get_task", json!({ "task_id": "t1.4" }));
    assert_eq!(task["title"], "Child");
}

#[test]
fn a_deeper_dotted_id_is_created_under_its_direct_parent() {
    let (sandbox, mut server) = project();
    create(&mut server, json!({ "task": { "title": "Top" } }));
    create(
        &mut server,
        json!({ "parent_id": "t1", "task": { "title": "Middle" } }),
    );

    create(
        &mut server,
        json!({ "task": { "id": "t1.1.7", "title": "Leaf" } }),
    );

    let tasks = listed(&mut server);
    assert_eq!(parent_of(&tasks, "t1.1.7").as_deref(), Some("t1.1"));
    assert!(
        task_dirs(&sandbox).contains(&PathBuf::from("t1-top/t1.1-middle/t1.1.7-leaf")),
        "{:?}",
        task_dirs(&sandbox)
    );
}

/// The parent is found by id, not by where its id says it should be.
#[test]
fn a_dotted_id_follows_a_parent_that_was_moved() {
    let (_sandbox, mut server) = project();
    create(&mut server, json!({ "task": { "title": "First" } }));
    create(&mut server, json!({ "task": { "title": "Second" } }));
    server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "t2" }, "move_to": "t1" }),
    );

    create(
        &mut server,
        json!({ "task": { "id": "t2.1", "title": "Child" } }),
    );

    let tasks = listed(&mut server);
    assert_eq!(parent_of(&tasks, "t2").as_deref(), Some("t1"));
    assert_eq!(parent_of(&tasks, "t2.1").as_deref(), Some("t2"));
}

#[test]
fn a_dotted_id_whose_parent_does_not_exist_is_rejected_and_creates_nothing() {
    let (sandbox, mut server) = project();
    create(&mut server, json!({ "task": { "title": "Only task" } }));
    let before = task_dirs(&sandbox);

    let error = server.err(
        "handoff_update_task",
        json!({ "task": { "id": "t-book.16", "title": "Orphan" } }),
    );
    assert!(
        error.contains("'t-book.16'") && error.contains("'t-book'"),
        "the error must name the id and the missing parent: {error}"
    );
    assert!(error.contains("does not exist"), "{error}");

    // The grandparent existing is not enough: `t1.2` is the parent named.
    let error = server.err(
        "handoff_update_task",
        json!({ "task": { "id": "t1.2.3", "title": "Orphan" } }),
    );
    assert!(
        error.contains("'t1.2.3'") && error.contains("'t1.2'"),
        "{error}"
    );

    assert_eq!(task_dirs(&sandbox), before, "a rejected create left a dir");
    assert_eq!(listed(&mut server).len(), 1);
}

#[test]
fn a_dotted_id_with_a_parent_id_that_names_another_task_is_rejected() {
    let (sandbox, mut server) = project();
    create(&mut server, json!({ "task": { "title": "First" } }));
    create(&mut server, json!({ "task": { "title": "Second" } }));
    let before = task_dirs(&sandbox);

    let error = server.err(
        "handoff_update_task",
        json!({ "parent_id": "t2", "task": { "id": "t1.5", "title": "Torn" } }),
    );
    assert!(
        error.contains("'t1.5'") && error.contains("'t1'") && error.contains("'t2'"),
        "the error must name the id, the parent it implies and parent_id: {error}"
    );
    assert_eq!(task_dirs(&sandbox), before, "a rejected create left a dir");

    // Agreeing values are accepted, as before.
    let id = create(
        &mut server,
        json!({ "parent_id": "t1", "task": { "id": "t1.5", "title": "Agreed" } }),
    );
    assert_eq!(id, "t1.5");
    assert_eq!(
        parent_of(&listed(&mut server), "t1.5").as_deref(),
        Some("t1")
    );
}

#[test]
fn a_dotted_id_with_an_empty_part_is_rejected() {
    let (sandbox, mut server) = project();
    create(&mut server, json!({ "task": { "title": "Parent" } }));
    let before = task_dirs(&sandbox);

    for id in ["t1.", ".1", "t1..2", "."] {
        let error = server.err(
            "handoff_update_task",
            json!({ "task": { "id": id, "title": "Malformed" } }),
        );
        assert!(
            error.contains(&format!("'{id}'")) && error.contains("empty"),
            "{id}: {error}"
        );
    }
    assert_eq!(task_dirs(&sandbox), before);
}

/// The id becomes part of a directory name. With a path separator in it the
/// task was written somewhere it could never be found by id (so a second
/// create made a second task with the same id), or outside the project.
#[test]
fn an_id_with_a_path_separator_is_rejected_and_writes_nothing_anywhere() {
    let (sandbox, mut server) = project();
    create(&mut server, json!({ "task": { "title": "Parent" } }));
    let before = task_dirs(&sandbox);
    let outside = sandbox.root.path().join("outside");
    let absolute = outside.to_string_lossy().to_string();

    for id in ["a/b", absolute.as_str(), "x\\y", "t1.a/b"] {
        for title in ["First", "Second"] {
            let error = server.err(
                "handoff_update_task",
                json!({ "task": { "id": id, "title": title } }),
            );
            assert!(
                error.contains(&format!("'{id}'")) && error.contains("path separator"),
                "{id}: {error}"
            );
        }
    }

    assert_eq!(task_dirs(&sandbox), before, "a rejected create left a dir");
    let siblings: Vec<String> = std::fs::read_dir(sandbox.root.path())
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with("outside"))
        .collect();
    assert!(
        siblings.is_empty(),
        "written outside the project: {siblings:?}"
    );
}

/// An id without a dot still creates a top-level task with that id, and
/// under `parent_id` when one is given: unchanged.
#[test]
fn an_id_without_a_dot_is_placed_as_before() {
    let (_sandbox, mut server) = project();
    create(
        &mut server,
        json!({ "task": { "id": "t-book", "title": "Book" } }),
    );
    create(
        &mut server,
        json!({ "parent_id": "t-book", "task": { "id": "appendix", "title": "Appendix" } }),
    );

    let tasks = listed(&mut server);
    assert_eq!(parent_of(&tasks, "t-book"), None);
    assert_eq!(parent_of(&tasks, "appendix").as_deref(), Some("t-book"));
}

/// An existing task is still updated through its dotted id, without
/// `parent_id`: the placement rule applies to creation only.
#[test]
fn an_existing_dotted_id_is_still_updated_without_parent_id() {
    let (_sandbox, mut server) = project();
    create(&mut server, json!({ "task": { "title": "Parent" } }));
    create(
        &mut server,
        json!({ "parent_id": "t1", "task": { "title": "Child" } }),
    );

    let text = server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "t1.1", "title": "Renamed" } }),
    );
    assert_eq!(text, "Updated task t1.1: Renamed [todo]");
}

// --- allocated ids avoid every id in use ---

#[test]
fn a_child_number_is_not_reused_after_the_child_was_moved_away() {
    let (_sandbox, mut server) = project();
    create(&mut server, json!({ "task": { "title": "First" } }));
    create(&mut server, json!({ "task": { "title": "Second" } }));
    for title in ["A", "B"] {
        create(
            &mut server,
            json!({ "parent_id": "t1", "task": { "title": title } }),
        );
    }
    server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "t1.2" }, "move_to": "t2" }),
    );

    let id = create(
        &mut server,
        json!({ "parent_id": "t1", "task": { "title": "C" } }),
    );
    assert_eq!(id, "t1.3", "t1.2 still exists, under t2");
    assert_eq!(count_of(&listed(&mut server), "t1.2"), 1);
}

#[test]
fn a_top_level_number_is_not_reused_after_the_task_was_moved_down() {
    let (_sandbox, mut server) = project();
    for title in ["First", "Second", "Third"] {
        create(&mut server, json!({ "task": { "title": title } }));
    }
    server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "t3" }, "move_to": "t1" }),
    );

    let id = create(&mut server, json!({ "task": { "title": "Fourth" } }));
    assert_eq!(id, "t4", "t3 still exists, under t1");
    assert_eq!(count_of(&listed(&mut server), "t3"), 1);
}

/// A finished child keeps its number: status does not free an id.
#[test]
fn a_child_number_is_not_reused_whatever_the_status_of_its_task() {
    let (_sandbox, mut server) = project();
    create(&mut server, json!({ "task": { "title": "Parent" } }));
    for status in ["done", "skipped", "blocked"] {
        create(
            &mut server,
            json!({ "parent_id": "t1", "task": {
                "title": status, "status": status, "schedule": { "estimate_hours": 1 },
            } }),
        );
    }

    let id = create(
        &mut server,
        json!({ "parent_id": "t1", "task": { "title": "Next" } }),
    );
    assert_eq!(id, "t1.4");
}

// --- a project that already holds a misplaced or duplicated id ---

/// A project written by a build with the defect: `t1.2` sits at the top
/// level. It is not re-created or renumbered into; `move_to` repairs it.
#[test]
fn a_misplaced_dotted_id_is_avoided_by_numbering_and_repaired_by_move_to() {
    let (sandbox, mut server) = project();
    create(&mut server, json!({ "task": { "title": "Parent" } }));
    create(
        &mut server,
        json!({ "parent_id": "t1", "task": { "title": "Child" } }),
    );
    write_task_on_disk(&sandbox, "t1.2-misplaced", "t1.2", "todo");

    let tasks = listed(&mut server);
    assert_eq!(parent_of(&tasks, "t1.2"), None, "listed where it is");
    let task = server.call("handoff_get_task", json!({ "task_id": "t1.2" }));
    assert_eq!(task["title"], "on disk t1.2");

    let id = create(
        &mut server,
        json!({ "parent_id": "t1", "task": { "title": "Next" } }),
    );
    assert_eq!(id, "t1.3", "t1.2 is in use, at the top level");

    server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "t1.2" }, "move_to": "t1" }),
    );
    let tasks = listed(&mut server);
    assert_eq!(parent_of(&tasks, "t1.2").as_deref(), Some("t1"));
}

/// A project that already holds two tasks with one id: every read answers,
/// both are listed, the id resolves to the one under the parent it names,
/// and no third one is added.
#[test]
fn a_duplicated_id_is_readable_and_never_allocated_again() {
    let (sandbox, mut server) = project();
    create(&mut server, json!({ "task": { "title": "Parent" } }));
    write_task_on_disk(&sandbox, "t1-parent/t1.1-nested", "t1.1", "todo");
    write_task_on_disk(&sandbox, "t1.1-misplaced", "t1.1", "in_progress");

    let tasks = listed(&mut server);
    assert_eq!(count_of(&tasks, "t1.1"), 2, "both are listed: {tasks:?}");
    for (tool, arguments) in [
        ("handoff_load_context", json!({})),
        ("handoff_get_metrics", json!({})),
        ("handoff_task_checklist", json!({ "task_id": "t1.1" })),
        ("handoff_auto_schedule", json!({ "dry_run": true })),
    ] {
        let answer = server.call_text(tool, arguments);
        assert!(
            answer.is_ok(),
            "{tool} failed on a duplicated id: {answer:?}"
        );
    }

    let task = server.call("handoff_get_task", json!({ "task_id": "t1.1" }));
    assert_eq!(task["status"], "todo", "the nested one answers to the id");
    let text = server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "t1.1", "notes": "addressed by id" } }),
    );
    assert_eq!(text, "Updated task t1.1: on disk t1.1 [todo]");

    let id = create(
        &mut server,
        json!({ "parent_id": "t1", "task": { "title": "Next" } }),
    );
    assert_eq!(id, "t1.2");
    assert_eq!(count_of(&listed(&mut server), "t1.1"), 2);
}

/// An id is in use when a task file says so, whatever its directory is
/// called.
#[test]
fn an_id_stored_in_a_task_file_is_avoided_whatever_its_directory_is_called() {
    let (sandbox, mut server) = project();
    create(&mut server, json!({ "task": { "title": "Parent" } }));
    write_task_on_disk(&sandbox, "renamed-by-hand", "t2", "todo");
    write_task_on_disk(&sandbox, "t1-parent/also-renamed", "t1.1", "done");

    assert_eq!(
        create(&mut server, json!({ "task": { "title": "Top" } })),
        "t3"
    );
    assert_eq!(
        create(
            &mut server,
            json!({ "parent_id": "t1", "task": { "title": "Child" } })
        ),
        "t1.2"
    );
}

/// A project written by a build with the defect holds `t2.1` without a `t2`.
/// A new top-level task is not numbered `t2`: it would be born with a child
/// it never numbered, and `handoff_import_context`, which numbers the
/// children of the tasks it creates from 1, would write a second `t2.1`.
#[test]
fn a_top_level_number_named_by_an_orphaned_dotted_id_is_not_allocated() {
    let (sandbox, mut server) = project();
    create(&mut server, json!({ "task": { "title": "First" } }));
    write_task_on_disk(&sandbox, "t2.1-orphan", "t2.1", "todo");

    server.ok(
        "handoff_import_context",
        json!({
            "source": { "description": "allocation test", "format": "other" },
            "skip_session_close": true,
            "tasks": [{ "title": "Imported", "children": [{ "title": "Imported child" }] }],
        }),
    );

    let tasks = listed(&mut server);
    assert_eq!(count_of(&tasks, "t2.1"), 1, "{tasks:?}");
    assert_eq!(count_of(&tasks, "t2"), 0, "{tasks:?}");
    assert_eq!(parent_of(&tasks, "t3"), None);
    assert_eq!(parent_of(&tasks, "t3.1").as_deref(), Some("t3"));
    assert_eq!(
        create(&mut server, json!({ "task": { "title": "Next" } })),
        "t4"
    );
}
