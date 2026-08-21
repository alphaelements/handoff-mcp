use handoff_mcp::storage::tasks::*;
use std::fs;
use tempfile::TempDir;

fn setup() -> TempDir {
    tempfile::tempdir().expect("failed to create temp dir")
}

fn create_task_dir(base: &std::path::Path, dir_name: &str, status: &str, data: &TaskData) {
    let dir = base.join(dir_name);
    fs::create_dir_all(&dir).unwrap();
    write_task(&dir, status, data).unwrap();
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
        lock: None,
        extra: std::collections::HashMap::new(),
    }
}

#[test]
fn write_and_read_task() {
    let dir = setup();
    let task_dir = dir.path().join("t1-test");
    fs::create_dir_all(&task_dir).unwrap();

    let data = make_task("t1", "Test task");
    write_task(&task_dir, "todo", &data).unwrap();

    let (read_data, status) = read_task(&task_dir).unwrap().unwrap();
    assert_eq!(read_data.id, "t1");
    assert_eq!(read_data.title, "Test task");
    assert_eq!(status, "todo");
}

#[test]
fn change_task_status() {
    let dir = setup();
    let task_dir = dir.path().join("t1-test");
    fs::create_dir_all(&task_dir).unwrap();

    let data = make_task("t1", "Test");
    write_task(&task_dir, "todo", &data).unwrap();

    change_status(&task_dir, "in_progress").unwrap();

    let (_, status) = read_task(&task_dir).unwrap().unwrap();
    assert_eq!(status, "in_progress");

    assert!(!task_dir.join("_task.todo.json").exists());
    assert!(task_dir.join("_task.in_progress.json").exists());
}

#[test]
fn change_to_same_status_is_noop() {
    let dir = setup();
    let task_dir = dir.path().join("t1-test");
    fs::create_dir_all(&task_dir).unwrap();

    let data = make_task("t1", "Test");
    write_task(&task_dir, "todo", &data).unwrap();

    change_status(&task_dir, "todo").unwrap();
    let (_, status) = read_task(&task_dir).unwrap().unwrap();
    assert_eq!(status, "todo");
}

#[test]
fn invalid_status_returns_error() {
    let dir = setup();
    let task_dir = dir.path().join("t1-test");
    fs::create_dir_all(&task_dir).unwrap();

    let data = make_task("t1", "Test");
    write_task(&task_dir, "todo", &data).unwrap();

    assert!(change_status(&task_dir, "invalid_status").is_err());
}

#[test]
fn next_top_level_id_empty() {
    let dir = setup();
    let tasks_dir = dir.path().join("tasks");
    fs::create_dir_all(&tasks_dir).unwrap();

    assert_eq!(next_top_level_id(&tasks_dir).unwrap(), "t1");
}

#[test]
fn next_top_level_id_with_existing() {
    let dir = setup();
    let tasks_dir = dir.path().join("tasks");
    fs::create_dir_all(tasks_dir.join("t1-first")).unwrap();
    fs::create_dir_all(tasks_dir.join("t3-third")).unwrap();

    assert_eq!(next_top_level_id(&tasks_dir).unwrap(), "t4");
}

#[test]
fn next_child_id_empty_parent() {
    let dir = setup();
    let parent_dir = dir.path().join("tasks/t1-parent");
    fs::create_dir_all(&parent_dir).unwrap();

    assert_eq!(next_child_id(&parent_dir, "t1").unwrap(), "t1.1");
}

#[test]
fn next_child_id_with_existing_children() {
    let dir = setup();
    let parent_dir = dir.path().join("tasks/t1-parent");
    fs::create_dir_all(parent_dir.join("t1.1-first")).unwrap();
    fs::create_dir_all(parent_dir.join("t1.2-second")).unwrap();

    assert_eq!(next_child_id(&parent_dir, "t1").unwrap(), "t1.3");
}

#[test]
fn title_to_slug_basic() {
    assert_eq!(title_to_slug("Hello World"), "hello-world");
    assert_eq!(title_to_slug("P0: SM_RESTART"), "p0-sm-restart");
    assert_eq!(title_to_slug("  Multiple   Spaces  "), "multiple-spaces");
}

#[test]
fn title_to_slug_empty() {
    assert_eq!(title_to_slug(""), "task");
}

#[test]
fn find_task_dir_by_id_finds_nested() {
    let dir = setup();
    let tasks_dir = dir.path().join("tasks");
    let nested = tasks_dir.join("t1-parent/t1.1-child");
    fs::create_dir_all(&nested).unwrap();
    write_task(&nested, "todo", &make_task("t1.1", "child")).unwrap();

    let found = find_task_dir_by_id(&tasks_dir, "t1.1").unwrap();
    assert!(found.is_some());
    assert!(found.unwrap().ends_with("t1.1-child"));
}

#[test]
fn find_task_dir_by_id_not_found() {
    let dir = setup();
    let tasks_dir = dir.path().join("tasks");
    fs::create_dir_all(&tasks_dir).unwrap();

    assert!(find_task_dir_by_id(&tasks_dir, "t99").unwrap().is_none());
}

#[test]
fn find_task_dir_by_id_hyphenated_id() {
    let dir = setup();
    let tasks_dir = dir.path().join("tasks");
    let task_dir = tasks_dir.join("m2-burst-burst-mode-state-machine");
    fs::create_dir_all(&task_dir).unwrap();
    write_task(&task_dir, "todo", &make_task("m2-burst", "Burst mode")).unwrap();

    let found = find_task_dir_by_id(&tasks_dir, "m2-burst").unwrap();
    assert!(found.is_some(), "should find task by hyphenated id");
    assert!(found
        .unwrap()
        .ends_with("m2-burst-burst-mode-state-machine"));
}

#[test]
fn find_task_dir_by_id_hyphenated_nested() {
    let dir = setup();
    let tasks_dir = dir.path().join("tasks");
    let parent = tasks_dir.join("p1-parent");
    fs::create_dir_all(&parent).unwrap();
    write_task(&parent, "in_progress", &make_task("p1", "Parent")).unwrap();

    let child = parent.join("p1-sub-feature-impl");
    fs::create_dir_all(&child).unwrap();
    write_task(&child, "todo", &make_task("p1-sub", "Sub feature")).unwrap();

    let found = find_task_dir_by_id(&tasks_dir, "p1-sub").unwrap();
    assert!(found.is_some(), "should find nested hyphenated id");
    assert!(found.unwrap().ends_with("p1-sub-feature-impl"));
}

#[test]
fn find_task_dir_by_id_no_false_positive_on_prefix() {
    let dir = setup();
    let tasks_dir = dir.path().join("tasks");
    let task_dir = tasks_dir.join("t1-some-title");
    fs::create_dir_all(&task_dir).unwrap();
    write_task(&task_dir, "todo", &make_task("t1", "Some title")).unwrap();

    assert!(
        find_task_dir_by_id(&tasks_dir, "t1-some")
            .unwrap()
            .is_none(),
        "should not match partial id that differs from json id"
    );
}

#[test]
fn build_task_index_basic() {
    let dir = setup();
    let tasks_dir = dir.path().join("tasks");

    create_task_dir(
        &tasks_dir,
        "t1-parent",
        "in_progress",
        &make_task("t1", "Parent"),
    );
    create_task_dir(
        &tasks_dir.join("t1-parent"),
        "t1.1-child",
        "done",
        &make_task("t1.1", "Child Done"),
    );
    create_task_dir(&tasks_dir, "t2-other", "todo", &make_task("t2", "Other"));

    let (tree, summary) = build_task_index(&tasks_dir, 10).unwrap();
    assert_eq!(summary.total, 3);
    assert_eq!(*summary.by_status.get("in_progress").unwrap_or(&0), 1);
    assert_eq!(*summary.by_status.get("done").unwrap_or(&0), 1);
    assert_eq!(*summary.by_status.get("todo").unwrap_or(&0), 1);

    assert_eq!(tree.len(), 2);
    let t1 = tree.iter().find(|t| t.id == "t1").unwrap();
    assert_eq!(t1.children.len(), 1);
    assert_eq!(t1.children[0].id, "t1.1");
}

#[test]
fn build_task_index_done_limit() {
    let dir = setup();
    let tasks_dir = dir.path().join("tasks");

    for i in 1..=5 {
        create_task_dir(
            &tasks_dir,
            &format!("t{i}-task-{i}"),
            "done",
            &make_task(&format!("t{i}"), &format!("Task {i}")),
        );
    }

    let (tree, summary) = build_task_index(&tasks_dir, 3).unwrap();
    assert_eq!(summary.total, 5);
    assert_eq!(*summary.by_status.get("done").unwrap(), 5);
    assert_eq!(tree.len(), 3);
}

#[test]
fn validate_done_unchecked_criteria_fails() {
    let dir = setup();
    let task_dir = dir.path().join("t1-test");
    fs::create_dir_all(&task_dir).unwrap();

    let data = TaskData {
        done_criteria: vec![
            DoneCriterion {
                item: "test passes".to_string(),
                checked: true,
            },
            DoneCriterion {
                item: "review done".to_string(),
                checked: false,
            },
        ],
        ..make_task("t1", "Test")
    };

    assert!(validate_done_transition(&task_dir, &data).is_err());
}

#[test]
fn validate_done_all_checked_passes() {
    let dir = setup();
    let task_dir = dir.path().join("t1-test");
    fs::create_dir_all(&task_dir).unwrap();

    let data = TaskData {
        done_criteria: vec![DoneCriterion {
            item: "test passes".to_string(),
            checked: true,
        }],
        ..make_task("t1", "Test")
    };

    assert!(validate_done_transition(&task_dir, &data).is_ok());
}

#[test]
fn validate_done_child_not_terminal_fails() {
    let dir = setup();
    let task_dir = dir.path().join("t1-test");
    fs::create_dir_all(&task_dir).unwrap();

    create_task_dir(
        &task_dir,
        "t1.1-child",
        "in_progress",
        &make_task("t1.1", "Child"),
    );

    let data = make_task("t1", "Parent");
    assert!(validate_done_transition(&task_dir, &data).is_err());
}

#[test]
fn validate_done_child_all_terminal_passes() {
    let dir = setup();
    let task_dir = dir.path().join("t1-test");
    fs::create_dir_all(&task_dir).unwrap();

    create_task_dir(
        &task_dir,
        "t1.1-child-a",
        "done",
        &make_task("t1.1", "Child A"),
    );
    create_task_dir(
        &task_dir,
        "t1.2-child-b",
        "skipped",
        &make_task("t1.2", "Child B"),
    );

    let data = make_task("t1", "Parent");
    assert!(validate_done_transition(&task_dir, &data).is_ok());
}

#[test]
fn validate_skipped_child_not_terminal_fails() {
    let dir = setup();
    let task_dir = dir.path().join("t1-test");
    fs::create_dir_all(&task_dir).unwrap();

    create_task_dir(&task_dir, "t1.1-child", "todo", &make_task("t1.1", "Child"));

    let data = make_task("t1", "Parent");
    assert!(validate_skipped_transition(&task_dir, &data).is_err());
}

#[test]
fn is_valid_priority_accepts_valid() {
    assert!(is_valid_priority("low"));
    assert!(is_valid_priority("medium"));
    assert!(is_valid_priority("high"));
}

#[test]
fn is_valid_priority_rejects_invalid() {
    assert!(!is_valid_priority("critical"));
    assert!(!is_valid_priority("urgent"));
    assert!(!is_valid_priority(""));
    assert!(!is_valid_priority("HIGH"));
}

#[test]
fn validate_priority_none_is_ok() {
    assert!(validate_priority(None).is_ok());
}

#[test]
fn validate_priority_valid_is_ok() {
    assert!(validate_priority(Some("low")).is_ok());
    assert!(validate_priority(Some("medium")).is_ok());
    assert!(validate_priority(Some("high")).is_ok());
}

fn make_task_with_deps(id: &str, title: &str, deps: &[&str]) -> TaskData {
    let mut t = make_task(id, title);
    t.dependencies = deps.iter().map(|s| s.to_string()).collect();
    t
}

#[test]
fn find_dependents_finds_direct_child() {
    let dir = setup();
    let tasks_dir = dir.path().join("tasks");
    create_task_dir(
        &tasks_dir,
        "t74.4-group-index",
        "done",
        &make_task("t74.4", "Group index"),
    );
    create_task_dir(
        &tasks_dir,
        "t74.6-marquee-wire",
        "todo",
        &make_task_with_deps("t74.6", "Wire marquee selection", &["t74.4"]),
    );

    let deps = find_dependents(&tasks_dir, "t74.4").unwrap();
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].id, "t74.6");
    assert_eq!(deps[0].title, "Wire marquee selection");
    assert_eq!(deps[0].status, "todo");
}

#[test]
fn find_dependents_none_found() {
    let dir = setup();
    let tasks_dir = dir.path().join("tasks");
    create_task_dir(&tasks_dir, "t1-solo", "todo", &make_task("t1", "Solo task"));

    let deps = find_dependents(&tasks_dir, "t1").unwrap();
    assert!(deps.is_empty());
}

#[test]
fn find_dependents_finds_nested_dependent() {
    let dir = setup();
    let tasks_dir = dir.path().join("tasks");
    let parent = tasks_dir.join("t74-epic");
    fs::create_dir_all(&parent).unwrap();
    write_task(&parent, "in_progress", &make_task("t74", "Epic")).unwrap();

    let child = parent.join("t74.4-group-index");
    fs::create_dir_all(&child).unwrap();
    write_task(&child, "done", &make_task("t74.4", "Group index")).unwrap();

    let sibling = parent.join("t74.6-marquee-wire");
    fs::create_dir_all(&sibling).unwrap();
    write_task(
        &sibling,
        "todo",
        &make_task_with_deps("t74.6", "Wire marquee selection", &["t74.4"]),
    )
    .unwrap();

    let deps = find_dependents(&tasks_dir, "t74.4").unwrap();
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].id, "t74.6");
}

#[test]
fn find_dependents_multiple_sorted_by_id() {
    let dir = setup();
    let tasks_dir = dir.path().join("tasks");
    create_task_dir(&tasks_dir, "t1-base", "done", &make_task("t1", "Base"));
    create_task_dir(
        &tasks_dir,
        "t3-consumer",
        "todo",
        &make_task_with_deps("t3", "Consumer B", &["t1"]),
    );
    create_task_dir(
        &tasks_dir,
        "t2-consumer",
        "todo",
        &make_task_with_deps("t2", "Consumer A", &["t1"]),
    );

    let deps = find_dependents(&tasks_dir, "t1").unwrap();
    let ids: Vec<&str> = deps.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids, vec!["t2", "t3"]);
}

#[test]
fn find_dependents_carries_notes_and_done_criteria_without_a_second_lookup() {
    let dir = setup();
    let tasks_dir = dir.path().join("tasks");
    create_task_dir(&tasks_dir, "t1-base", "done", &make_task("t1", "Base"));

    let mut dependent = make_task_with_deps("t2", "Wires t1's function in", &["t1"]);
    dependent.notes = Some("Wires t1's build_group_index into capabilities_for()".to_string());
    dependent.done_criteria = vec![DoneCriterion {
        item: "dispatch table updated".to_string(),
        checked: false,
    }];
    create_task_dir(&tasks_dir, "t2-consumer", "todo", &dependent);

    let deps = find_dependents(&tasks_dir, "t1").unwrap();
    assert_eq!(deps.len(), 1);
    assert_eq!(
        deps[0].notes.as_deref(),
        Some("Wires t1's build_group_index into capabilities_for()")
    );
    assert_eq!(deps[0].done_criteria.len(), 1);
    assert_eq!(deps[0].done_criteria[0].item, "dispatch table updated");
}

#[test]
fn validate_priority_invalid_is_err() {
    let err = validate_priority(Some("critical")).unwrap_err();
    assert!(err.to_string().contains("Invalid priority"));
    assert!(err.to_string().contains("critical"));
}

// ---- TaskLock / claim_task / release_task (t240.6) ----

#[test]
fn task_lock_serde_round_trip() {
    let lock = TaskLock {
        agent_id: "agent-1".to_string(),
        session_id: "s-1".to_string(),
        claimed_at: "2026-08-21T00:00:00+00:00".to_string(),
        lease_expires_at: "2026-08-21T00:30:00+00:00".to_string(),
        lease_ttl_seconds: 1800,
    };
    let json = serde_json::to_string(&lock).unwrap();
    let back: TaskLock = serde_json::from_str(&json).unwrap();
    assert_eq!(back.agent_id, "agent-1");
    assert_eq!(back.session_id, "s-1");
    assert_eq!(back.lease_ttl_seconds, 1800);
}

#[test]
fn task_data_lock_field_does_not_leak_into_extra() {
    // Regression for spec 3.7.1: `lock` must be declared before the
    // `#[serde(flatten)]` extra map so a "lock" JSON key deserializes into
    // the typed field, not into `extra`.
    let dir = setup();
    let task_dir = dir.path().join("t1-test");
    fs::create_dir_all(&task_dir).unwrap();

    let mut data = make_task("t1", "Test task");
    data.lock = Some(TaskLock {
        agent_id: "agent-1".to_string(),
        session_id: "s-1".to_string(),
        claimed_at: "2026-08-21T00:00:00+00:00".to_string(),
        lease_expires_at: "2026-08-21T00:30:00+00:00".to_string(),
        lease_ttl_seconds: 1800,
    });
    write_task(&task_dir, "todo", &data).unwrap();

    let (read_data, _) = read_task(&task_dir).unwrap().unwrap();
    assert!(!read_data.extra.contains_key("lock"));
    assert_eq!(
        read_data.lock.as_ref().unwrap().agent_id,
        "agent-1".to_string()
    );
}

#[test]
fn claim_task_sets_lock_and_moves_to_in_progress() {
    let dir = setup();
    let task_dir = dir.path().join("t1-test");
    fs::create_dir_all(&task_dir).unwrap();
    write_task(&task_dir, "todo", &make_task("t1", "Test")).unwrap();

    let data = claim_task(&task_dir, "agent-1", "session-1", 1800).unwrap();

    assert!(data.lock.is_some());
    let lock = data.lock.unwrap();
    assert_eq!(lock.agent_id, "agent-1");
    assert_eq!(lock.session_id, "session-1");
    assert_eq!(lock.lease_ttl_seconds, 1800);

    let (_, status) = read_task(&task_dir).unwrap().unwrap();
    assert_eq!(status, "in_progress");
}

#[test]
fn claim_task_already_claimed_by_valid_lease_returns_error() {
    let dir = setup();
    let task_dir = dir.path().join("t1-test");
    fs::create_dir_all(&task_dir).unwrap();
    write_task(&task_dir, "todo", &make_task("t1", "Test")).unwrap();

    claim_task(&task_dir, "agent-1", "session-1", 1800).unwrap();

    let err = claim_task(&task_dir, "agent-2", "session-2", 1800).unwrap_err();
    assert!(err.to_string().contains("agent-1"));
}

#[test]
fn claim_task_with_expired_lease_is_overwritten() {
    let dir = setup();
    let task_dir = dir.path().join("t1-test");
    fs::create_dir_all(&task_dir).unwrap();

    let mut data = make_task("t1", "Test");
    data.lock = Some(TaskLock {
        agent_id: "agent-old".to_string(),
        session_id: "session-old".to_string(),
        claimed_at: "2020-01-01T00:00:00+00:00".to_string(),
        lease_expires_at: "2020-01-01T00:30:00+00:00".to_string(),
        lease_ttl_seconds: 1800,
    });
    write_task(&task_dir, "in_progress", &data).unwrap();

    let claimed = claim_task(&task_dir, "agent-new", "session-new", 1800).unwrap();
    assert_eq!(claimed.lock.unwrap().agent_id, "agent-new");
}

#[test]
fn release_task_clears_lock_and_reverts_status() {
    let dir = setup();
    let task_dir = dir.path().join("t1-test");
    fs::create_dir_all(&task_dir).unwrap();
    write_task(&task_dir, "todo", &make_task("t1", "Test")).unwrap();

    claim_task(&task_dir, "agent-1", "session-1", 1800).unwrap();
    release_task(&task_dir, "agent-1", "todo").unwrap();

    let (data, status) = read_task(&task_dir).unwrap().unwrap();
    assert!(data.lock.is_none());
    assert_eq!(status, "todo");
}

#[test]
fn release_task_by_non_owner_returns_error() {
    let dir = setup();
    let task_dir = dir.path().join("t1-test");
    fs::create_dir_all(&task_dir).unwrap();
    write_task(&task_dir, "todo", &make_task("t1", "Test")).unwrap();

    claim_task(&task_dir, "agent-1", "session-1", 1800).unwrap();

    let err = release_task(&task_dir, "agent-2", "todo").unwrap_err();
    assert!(err.to_string().contains("agent-1"));

    // Lock must remain intact after the rejected release.
    let (data, _) = read_task(&task_dir).unwrap().unwrap();
    assert_eq!(data.lock.unwrap().agent_id, "agent-1");
}

#[test]
fn concurrent_claim_from_two_threads_only_one_succeeds() {
    use std::sync::Arc;
    use std::thread;

    let dir = setup();
    let task_dir = Arc::new(dir.path().join("t1-test"));
    fs::create_dir_all(task_dir.as_path()).unwrap();
    write_task(&task_dir, "todo", &make_task("t1", "Test")).unwrap();

    let barrier = Arc::new(std::sync::Barrier::new(2));

    let handles: Vec<_> = (0..2)
        .map(|i| {
            let task_dir = Arc::clone(&task_dir);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                claim_task(
                    &task_dir,
                    &format!("agent-{i}"),
                    &format!("session-{i}"),
                    1800,
                )
            })
        })
        .collect();

    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let successes = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(successes, 1, "exactly one concurrent claim must succeed");
}

#[test]
fn read_modify_write_task_locked_protects_mutation() {
    let dir = setup();
    let task_dir = dir.path().join("t1-test");
    fs::create_dir_all(&task_dir).unwrap();
    write_task(&task_dir, "todo", &make_task("t1", "Test")).unwrap();

    read_modify_write_task_locked(&task_dir, |data, status| {
        data.notes = Some("locked write".to_string());
        Ok(status.to_string())
    })
    .unwrap();

    let (data, _) = read_task(&task_dir).unwrap().unwrap();
    assert_eq!(data.notes.as_deref(), Some("locked write"));
}
