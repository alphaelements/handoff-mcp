use handoff_mcp::storage::config::{read_config, write_config, Config};
use std::fs;
use tempfile::TempDir;

fn setup() -> TempDir {
    tempfile::tempdir().expect("failed to create temp dir")
}

#[test]
fn write_and_read_config() {
    let dir = setup();
    let path = dir.path().join("config.toml");

    let config = Config::new("test-project", "A test project");
    write_config(&path, &config).unwrap();

    let read_back = read_config(&path).unwrap();
    assert_eq!(read_back.project.name, "test-project");
    assert_eq!(
        read_back.project.description.as_deref(),
        Some("A test project")
    );
}

#[test]
fn config_has_correct_defaults() {
    let dir = setup();
    let path = dir.path().join("config.toml");

    let config = Config::new("proj", "");
    write_config(&path, &config).unwrap();

    let read_back = read_config(&path).unwrap();
    assert_eq!(read_back.settings.history_limit, 20);
    assert_eq!(read_back.settings.done_task_limit, 10);
    assert!(read_back.settings.auto_git_summary);
    assert!(read_back.settings.context_files.is_empty());
    assert_eq!(read_back.dashboard.scan_dirs, vec!["~/pro/"]);
    assert!(read_back.dashboard.exclude_patterns.is_empty());
    assert!(read_back.project.description.is_none());
}

#[test]
fn config_with_custom_values() {
    let dir = setup();
    let path = dir.path().join("config.toml");

    let toml_content = r#"
[project]
name = "custom-proj"
description = "Custom"

[settings]
history_limit = 50
done_task_limit = 5
auto_git_summary = false
context_files = ["README.md", "CLAUDE.md"]

[dashboard]
scan_dirs = ["~/work/", "~/projects/"]
exclude_patterns = ["*/target", "*/node_modules"]
"#;
    fs::write(&path, toml_content).unwrap();

    let config = read_config(&path).unwrap();
    assert_eq!(config.project.name, "custom-proj");
    assert_eq!(config.settings.history_limit, 50);
    assert_eq!(config.settings.done_task_limit, 5);
    assert!(!config.settings.auto_git_summary);
    assert_eq!(config.settings.context_files.len(), 2);
    assert_eq!(config.dashboard.scan_dirs.len(), 2);
    assert_eq!(config.dashboard.exclude_patterns.len(), 2);
}

#[test]
fn config_missing_file_returns_error() {
    let dir = setup();
    let path = dir.path().join("nonexistent.toml");
    assert!(read_config(&path).is_err());
}

#[test]
fn config_invalid_toml_returns_error() {
    let dir = setup();
    let path = dir.path().join("bad.toml");
    fs::write(&path, "this is not valid toml [[[").unwrap();
    assert!(read_config(&path).is_err());
}

#[test]
fn config_partial_toml_uses_defaults() {
    let dir = setup();
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        r#"
[project]
name = "minimal"
"#,
    )
    .unwrap();

    let config = read_config(&path).unwrap();
    assert_eq!(config.project.name, "minimal");
    assert_eq!(config.settings.history_limit, 20);
    assert_eq!(config.dashboard.scan_dirs, vec!["~/pro/"]);
}
