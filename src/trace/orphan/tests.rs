//! Unit tests for the shared orphan-task predicate (REQ-TRC-001,
//! SPEC-TRC-001 / SPEC-TRC-002).

use super::*;

/// A leaf task: `todo`, no label, no requirement link.
fn task(id: &str) -> TaskFacts {
    TaskFacts {
        id: id.to_string(),
        title: format!("Title of {id}"),
        status: "todo".to_string(),
        ..Default::default()
    }
}

fn linked(id: &str) -> TaskFacts {
    TaskFacts {
        has_requirement_link: true,
        ..task(id)
    }
}

fn with_status(mut t: TaskFacts, status: &str) -> TaskFacts {
    t.status = status.to_string();
    t
}

fn labelled(mut t: TaskFacts, label: &str) -> TaskFacts {
    t.labels.push(label.to_string());
    t
}

fn parent(mut t: TaskFacts, children: Vec<TaskFacts>) -> TaskFacts {
    t.children = children;
    t
}

fn orphan_ids(tasks: &[TaskFacts]) -> Vec<String> {
    orphan_tasks(tasks, true)
        .into_iter()
        .map(|o| o.id)
        .collect()
}

fn link(link_type: &str) -> TaskLink {
    TaskLink {
        target: "doc-1".to_string(),
        link_type: link_type.to_string(),
        label: Some("REQ-001".to_string()),
        ..Default::default()
    }
}

// --- REQ-TRC-001 AC1 / AC6 ---

#[test]
fn an_unlinked_leaf_task_is_an_orphan_with_its_id_title_and_status() {
    let found = orphan_tasks(&[with_status(task("t1"), "in_progress")], true);
    assert_eq!(
        found,
        vec![OrphanTask {
            id: "t1".to_string(),
            title: "Title of t1".to_string(),
            status: "in_progress".to_string(),
        }]
    );
}

#[test]
fn a_linked_leaf_task_is_not_an_orphan() {
    assert!(orphan_ids(&[linked("t1")]).is_empty());
}

/// The requirement names `skipped` as the only excluded status, so a `done`
/// task without a link is still reported.
#[test]
fn every_status_but_skipped_is_reported() {
    for status in ["todo", "in_progress", "review", "done", "blocked"] {
        assert_eq!(
            orphan_ids(&[with_status(task("t1"), status)]),
            vec!["t1"],
            "status {status}"
        );
    }
}

// --- AC2 ---

#[test]
fn a_skipped_task_is_not_an_orphan() {
    assert!(orphan_ids(&[with_status(task("t1"), "skipped")]).is_empty());
}

// --- AC5 ---

#[test]
fn a_no_trace_task_is_not_an_orphan() {
    assert!(orphan_ids(&[labelled(task("t1"), NO_TRACE_LABEL)]).is_empty());
}

#[test]
fn only_the_exact_no_trace_label_excludes_a_task() {
    for label in ["no-trace-yet", "No-Trace", "notrace", "trace"] {
        assert_eq!(
            orphan_ids(&[labelled(task("t1"), label)]),
            vec!["t1"],
            "label {label}"
        );
    }
    let among_others = labelled(labelled(task("t2"), "mcp"), NO_TRACE_LABEL);
    assert!(orphan_ids(&[among_others]).is_empty());
}

// --- AC3 ---

#[test]
fn a_parent_whose_children_are_all_linked_no_trace_or_skipped_is_not_an_orphan() {
    let tree = [parent(
        task("t1"),
        vec![
            linked("t1.1"),
            labelled(task("t1.2"), NO_TRACE_LABEL),
            with_status(task("t1.3"), "skipped"),
        ],
    )];
    assert!(orphan_ids(&tree).is_empty());
}

// --- AC4 ---

#[test]
fn a_parent_with_only_some_children_linked_is_an_orphan_and_so_is_the_unlinked_child() {
    let tree = [parent(task("t1"), vec![linked("t1.1"), task("t1.2")])];
    assert_eq!(orphan_ids(&tree), vec!["t1", "t1.2"]);
}

#[test]
fn a_parent_whose_children_are_all_unlinked_is_an_orphan() {
    let tree = [parent(task("t1"), vec![task("t1.1")])];
    assert_eq!(orphan_ids(&tree), vec!["t1", "t1.1"]);
}

/// A parent that is itself linked is never an orphan, but its link does not
/// vouch for its children.
#[test]
fn a_linked_parent_is_not_an_orphan_but_its_unlinked_child_still_is() {
    let tree = [parent(linked("t1"), vec![task("t1.1")])];
    assert_eq!(orphan_ids(&tree), vec!["t1.1"]);
}

#[test]
fn a_no_trace_or_skipped_parent_is_not_an_orphan_but_its_unlinked_child_still_is() {
    let tree = [
        parent(labelled(task("t1"), NO_TRACE_LABEL), vec![task("t1.1")]),
        parent(
            with_status(task("t2"), "skipped"),
            vec![with_status(task("t2.1"), "done")],
        ),
    ];
    assert_eq!(orphan_ids(&tree), vec!["t1.1", "t2.1"]);
}

// --- SPEC-TRC-002: three levels and more ---

/// SPEC-TRC-002 AC5: an unlinked leaf two levels down makes the top parent an
/// orphan even though its direct child is linked.
#[test]
fn an_unlinked_grandchild_makes_the_grandparent_an_orphan_even_under_a_linked_child() {
    let tree = [parent(
        task("t1"),
        vec![parent(linked("t1.1"), vec![task("t1.1.1")])],
    )];
    assert_eq!(orphan_ids(&tree), vec!["t1", "t1.1.1"]);
}

#[test]
fn a_grouping_child_whose_own_children_are_linked_keeps_the_grandparent_out() {
    let tree = [parent(
        task("t1"),
        vec![parent(
            task("t1.1"),
            vec![linked("t1.1.1"), linked("t1.1.2")],
        )],
    )];
    assert!(orphan_ids(&tree).is_empty());
}

#[test]
fn every_orphan_below_a_parent_is_reported_not_just_the_first() {
    let tree = [parent(
        task("t1"),
        vec![task("t1.1"), linked("t1.2"), task("t1.3")],
    )];
    assert_eq!(orphan_ids(&tree), vec!["t1", "t1.1", "t1.3"]);
}

// --- ordering ---

#[test]
fn orphans_are_listed_in_natural_id_order() {
    let tree = [
        task("t10"),
        parent(linked("t2"), vec![task("t2.10"), task("t2.9")]),
        task("t1"),
    ];
    assert_eq!(orphan_ids(&tree), vec!["t1", "t2.9", "t2.10", "t10"]);
}

// --- scope guard (SPEC-TRC-002 AC1 / AC2) ---

#[test]
fn nothing_is_an_orphan_when_traceability_is_not_in_use() {
    let tree = [parent(task("t1"), vec![task("t1.1")]), task("t2")];
    assert!(orphan_tasks(&tree, false).is_empty());
    assert_eq!(orphan_tasks(&tree, true).len(), 3);
}

#[test]
fn traceability_is_in_use_once_one_document_declares_a_layer() {
    let doc = |slug: &str| {
        DocMetadata::new(
            format!("doc-{slug}"),
            slug.to_string(),
            "Doc".to_string(),
            "spec".to_string(),
            "2026-01-01T00:00:00Z".to_string(),
        )
    };
    let plain = doc("plain");
    let mut layered = doc("layered");
    layered.layer = Some("requirement".to_string());
    assert!(!traceability_in_use(&[]));
    assert!(!traceability_in_use(std::slice::from_ref(&plain)));
    assert!(traceability_in_use(&[plain, layered]));
}

// --- is_orphan: the single-task form the guard uses ---

#[test]
fn is_orphan_agrees_with_orphan_tasks_for_every_task_of_a_tree() {
    let tree = [
        parent(task("t1"), vec![linked("t1.1"), task("t1.2")]),
        parent(task("t2"), vec![linked("t2.1")]),
        linked("t3"),
        with_status(task("t4"), "skipped"),
        labelled(task("t5"), NO_TRACE_LABEL),
        task("t6"),
        parent(
            task("t7"),
            vec![parent(linked("t7.1"), vec![task("t7.1.1")])],
        ),
    ];
    let listed = orphan_ids(&tree);
    fn walk<'a>(tasks: &'a [TaskFacts], out: &mut Vec<&'a TaskFacts>) {
        for t in tasks {
            out.push(t);
            walk(&t.children, out);
        }
    }
    let mut all = Vec::new();
    walk(&tree, &mut all);
    assert_eq!(all.len(), 12);
    for t in all {
        assert_eq!(
            is_orphan(t, true),
            listed.contains(&t.id),
            "task {} disagrees",
            t.id
        );
        assert!(
            !is_orphan(t, false),
            "task {} with the scope guard off",
            t.id
        );
    }
    assert_eq!(listed, vec!["t1", "t1.2", "t6", "t7", "t7.1.1"]);
}

// --- link types (SPEC-TRC-002 AC4) ---

#[test]
fn only_a_requirement_type_link_counts_as_a_requirement_link() {
    assert!(!has_requirement_link(&[]));
    assert!(!has_requirement_link(&[
        link("doc"),
        link("url"),
        link("file"),
        link("task")
    ]));
    assert!(has_requirement_link(&[link("doc"), link("requirement")]));
}

// --- suggested action (AC6) ---

#[test]
fn the_suggested_action_names_the_task_both_remedies_and_trace_propose() {
    let text = suggested_action("t7");
    assert!(text.contains("t7"), "{text}");
    assert!(text.contains("requirement_ids"), "{text}");
    assert!(text.contains("handoff_trace_propose"), "{text}");
    assert!(text.contains("\"no-trace\""), "{text}");
}
