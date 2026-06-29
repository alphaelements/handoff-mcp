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
        done_criteria: Vec::new(),
        schedule: None,
        dependencies: Vec::new(),
        order: None,
        assignee: None,
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

#[test]
fn validate_priority_invalid_is_err() {
    let err = validate_priority(Some("critical")).unwrap_err();
    assert!(err.to_string().contains("Invalid priority"));
    assert!(err.to_string().contains("critical"));
}
