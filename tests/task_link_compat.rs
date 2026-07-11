//! Compatibility tests for the `task_links: Vec<TaskLink>` migration (t94).
//!
//! Verifies:
//! - Old-format task JSON (only `links: Vec<String>`, no `task_links` key) still
//!   parses without error (`#[serde(default)]` on `task_links`).
//! - New-format task JSON with `task_links` round-trips byte-for-byte
//!   equivalent data through serialize -> deserialize.
//! - The `links()` accessor normalizes the legacy `links: Vec<String>` into
//!   `TaskLink { link_type: "file", .. }` entries and merges them with
//!   `task_links`, de-duplicating by `(target, link_type)`.
//! - `sync_doc_task_links` (storage-layer helper for t96) links/unlinks a doc
//!   id on both sides of the task<->doc relationship.

use handoff_mcp::storage::tasks::*;
use std::fs;
use tempfile::TempDir;

fn setup() -> TempDir {
    tempfile::tempdir().expect("failed to create temp dir")
}

#[test]
fn old_format_json_without_task_links_parses() {
    let dir = setup();
    let task_dir = dir.path().join("t1-test");
    fs::create_dir_all(&task_dir).unwrap();

    // Legacy JSON produced before this migration: no `task_links` key at all.
    let legacy_json = r#"{
        "id": "t1",
        "title": "Legacy task",
        "labels": [],
        "links": ["https://example.com/spec", "wiki/10-architecture.md"],
        "done_criteria": [],
        "dependencies": []
    }"#;
    fs::write(task_dir.join("_task.todo.json"), legacy_json).unwrap();

    let (data, status) = read_task(&task_dir).unwrap().unwrap();
    assert_eq!(status, "todo");
    assert_eq!(data.id, "t1");
    assert_eq!(
        data.links,
        vec![
            "https://example.com/spec".to_string(),
            "wiki/10-architecture.md".to_string()
        ]
    );
    assert!(
        data.task_links.is_empty(),
        "task_links should default to empty when absent from legacy JSON"
    );
}

#[test]
fn task_links_roundtrip_through_write_and_read() {
    let dir = setup();
    let task_dir = dir.path().join("t1-test");
    fs::create_dir_all(&task_dir).unwrap();

    let mut data = make_task("t1", "Test task");
    data.links = vec!["https://example.com".to_string()];
    data.task_links = vec![TaskLink {
        target: "doc-20260711-000001".to_string(),
        link_type: "doc".to_string(),
        label: Some("Some Spec".to_string()),
    }];

    write_task(&task_dir, "todo", &data).unwrap();

    let raw = fs::read_to_string(task_dir.join("_task.todo.json")).unwrap();
    assert!(
        raw.contains("task_links"),
        "serialized JSON must contain task_links field:\n{raw}"
    );

    let (read_data, _status) = read_task(&task_dir).unwrap().unwrap();
    assert_eq!(read_data.task_links.len(), 1);
    assert_eq!(read_data.task_links[0].target, "doc-20260711-000001");
    assert_eq!(read_data.task_links[0].link_type, "doc");
    assert_eq!(read_data.task_links[0].label.as_deref(), Some("Some Spec"));
    assert_eq!(read_data.links, vec!["https://example.com".to_string()]);
}

#[test]
fn task_links_omitted_from_json_when_empty() {
    let dir = setup();
    let task_dir = dir.path().join("t1-test");
    fs::create_dir_all(&task_dir).unwrap();

    let data = make_task("t1", "Test task");
    write_task(&task_dir, "todo", &data).unwrap();

    let raw = fs::read_to_string(task_dir.join("_task.todo.json")).unwrap();
    assert!(
        !raw.contains("task_links"),
        "empty task_links must be skipped from serialization to preserve old-JSON \
         compatibility for readers that do strict key checks:\n{raw}"
    );
}

#[test]
fn links_accessor_normalizes_legacy_links_only() {
    let mut data = make_task("t1", "Test task");
    data.links = vec!["https://example.com".to_string(), "wiki/foo.md".to_string()];

    let normalized = data.links();
    assert_eq!(normalized.len(), 2);
    assert_eq!(normalized[0].target, "https://example.com");
    assert_eq!(normalized[0].link_type, "file");
    assert_eq!(normalized[0].label, None);
    assert_eq!(normalized[1].target, "wiki/foo.md");
    assert_eq!(normalized[1].link_type, "file");
}

#[test]
fn links_accessor_merges_legacy_links_and_task_links() {
    let mut data = make_task("t1", "Test task");
    data.links = vec!["https://example.com".to_string()];
    data.task_links = vec![TaskLink {
        target: "doc-1".to_string(),
        link_type: "doc".to_string(),
        label: Some("Doc One".to_string()),
    }];

    let normalized = data.links();
    assert_eq!(normalized.len(), 2);
    assert!(normalized
        .iter()
        .any(|l| l.target == "https://example.com" && l.link_type == "file"));
    assert!(normalized
        .iter()
        .any(|l| l.target == "doc-1" && l.link_type == "doc"));
}

#[test]
fn links_accessor_dedupes_by_target_and_link_type() {
    let mut data = make_task("t1", "Test task");
    // Same URL present in both legacy `links` and `task_links` (with link_type
    // "file") must not be duplicated.
    data.links = vec!["https://example.com".to_string()];
    data.task_links = vec![TaskLink {
        target: "https://example.com".to_string(),
        link_type: "file".to_string(),
        label: None,
    }];

    let normalized = data.links();
    assert_eq!(
        normalized.len(),
        1,
        "duplicate (target, link_type) pairs must be collapsed: {normalized:?}"
    );
}

#[test]
fn sync_doc_task_links_adds_and_removes_bidirectional_link() {
    let dir = setup();
    let task_dir = dir.path().join("t1-test");
    fs::create_dir_all(&task_dir).unwrap();
    write_task(&task_dir, "todo", &make_task("t1", "Test task")).unwrap();

    let tasks_dir = dir.path();

    // Link.
    sync_doc_task_links(
        tasks_dir,
        "doc-20260711-000001",
        "Some Spec",
        &["t1".to_string()],
        &[],
    )
    .unwrap();

    let (data, _) = read_task(&task_dir).unwrap().unwrap();
    assert_eq!(data.task_links.len(), 1);
    assert_eq!(data.task_links[0].target, "doc-20260711-000001");
    assert_eq!(data.task_links[0].link_type, "doc");
    assert_eq!(data.task_links[0].label.as_deref(), Some("Some Spec"));

    // Re-linking the same doc/task pair must not duplicate.
    sync_doc_task_links(
        tasks_dir,
        "doc-20260711-000001",
        "Some Spec",
        &["t1".to_string()],
        &[],
    )
    .unwrap();
    let (data, _) = read_task(&task_dir).unwrap().unwrap();
    assert_eq!(data.task_links.len(), 1);

    // Unlink.
    let report = sync_doc_task_links(
        tasks_dir,
        "doc-20260711-000001",
        "Some Spec",
        &[],
        &["t1".to_string()],
    )
    .unwrap();
    assert!(
        report.unresolved.is_empty(),
        "resolved task ids must not be reported as unresolved: {:?}",
        report.unresolved
    );
    let (data, _) = read_task(&task_dir).unwrap().unwrap();
    assert!(
        data.task_links.is_empty(),
        "task_links should be empty after unlinking: {:?}",
        data.task_links
    );
}

#[test]
fn sync_doc_task_links_reports_unresolved_link_ids() {
    let dir = setup();
    let task_dir = dir.path().join("t1-test");
    fs::create_dir_all(&task_dir).unwrap();
    write_task(&task_dir, "todo", &make_task("t1", "Test task")).unwrap();

    let tasks_dir = dir.path();

    // t1 resolves, t999 does not: the call must still succeed (partial
    // success) and report the unresolved id back to the caller instead of
    // silently skipping it.
    let report = sync_doc_task_links(
        tasks_dir,
        "doc-20260711-000001",
        "Some Spec",
        &["t1".to_string(), "t999".to_string()],
        &[],
    )
    .unwrap();

    assert_eq!(
        report.unresolved,
        vec!["t999".to_string()],
        "unresolved task ids must be reported: {:?}",
        report.unresolved
    );

    // The resolvable task must still have been linked (partial success, not
    // an all-or-nothing rollback).
    let (data, _) = read_task(&task_dir).unwrap().unwrap();
    assert_eq!(data.task_links.len(), 1);
    assert_eq!(data.task_links[0].target, "doc-20260711-000001");
}

#[test]
fn sync_doc_task_links_reports_unresolved_unlink_ids() {
    let dir = setup();
    let tasks_dir = dir.path();

    let report = sync_doc_task_links(
        tasks_dir,
        "doc-20260711-000001",
        "Some Spec",
        &[],
        &["ghost".to_string()],
    )
    .unwrap();

    assert_eq!(
        report.unresolved,
        vec!["ghost".to_string()],
        "unresolved unlink task ids must also be reported: {:?}",
        report.unresolved
    );
}

fn make_task(id: &str, title: &str) -> TaskData {
    TaskData {
        id: id.to_string(),
        title: title.to_string(),
        notes: None,
        priority: None,
        created_at: None,
        updated_at: None,
        completed_at: None,
        labels: Vec::new(),
        links: Vec::new(),
        task_links: Vec::new(),
        done_criteria: Vec::new(),
        schedule: None,
        dependencies: Vec::new(),
        order: None,
        assignee: None,
        extra: std::collections::HashMap::new(),
    }
}
