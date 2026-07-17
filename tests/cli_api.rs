//! Integration tests for the CLI API (`handoff-mcp <group> <action> [--flags]`).
//!
//! Each test exercises the real binary against a temporary `.handoff/` directory
//! to verify end-to-end behavior — flag parsing, handler dispatch, and JSON
//! output formatting.

use std::path::PathBuf;
use std::process::Command;

fn binary() -> PathBuf {
    let mut path = std::env::current_exe()
        .expect("current_exe")
        .parent()
        .expect("parent")
        .parent()
        .expect("parent")
        .to_path_buf();
    path.push("handoff-mcp");
    path
}

fn run(args: &[&str]) -> (String, String, i32) {
    let output = Command::new(binary())
        .args(args)
        .output()
        .expect("failed to run binary");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let code = output.status.code().unwrap_or(-1);
    (stdout, stderr, code)
}

fn init_project(dir: &std::path::Path) {
    let (stdout, _, code) = run(&[
        "init",
        "--project-dir",
        dir.to_str().unwrap(),
        "--project-name",
        "CLITest",
    ]);
    assert_eq!(code, 0, "init failed: {stdout}");
    // Lower the BM25 floor so single-doc test corpora still produce matches.
    let cfg = dir.join(".handoff/config.toml");
    let s = std::fs::read_to_string(&cfg).unwrap();
    let s = s.replace(
        "memory_query_min_score = 2.0",
        "memory_query_min_score = 0.0",
    );
    std::fs::write(&cfg, s).unwrap();
}

#[test]
fn help_shows_all_groups() {
    let (stdout, _, code) = run(&["--help"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("memory"));
    assert!(stdout.contains("task"));
    assert!(stdout.contains("session"));
    assert!(stdout.contains("timer"));
    assert!(stdout.contains("dashboard"));
}

#[test]
fn group_help_shows_actions() {
    let (stdout, _, code) = run(&["memory", "--help"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("save"));
    assert!(stdout.contains("query"));
    assert!(stdout.contains("delete"));
    assert!(stdout.contains("cleanup"));
}

#[test]
fn unknown_command_exits_1() {
    let (_, stderr, code) = run(&["foobar"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("Unknown command"));
}

#[test]
fn init_creates_handoff_dir() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_str().unwrap();
    let (stdout, _, code) = run(&["init", "--project-dir", dir, "--project-name", "Test"]);
    assert_eq!(code, 0, "stdout: {stdout}");
    assert!(stdout.contains("Initialized"));
    assert!(tmp.path().join(".handoff").exists());
}

#[test]
fn memory_save_query_delete_roundtrip() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_str().unwrap();
    init_project(tmp.path());

    // Save
    let (stdout, _, code) = run(&[
        "memory",
        "save",
        "--project-dir",
        dir,
        "--text",
        "Always use atomic_write",
        "--kind",
        "lesson",
    ]);
    assert_eq!(code, 0, "save failed: {stdout}");
    assert!(stdout.contains("\"status\": \"saved\""));

    let saved: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let mem_id = saved["id"].as_str().unwrap();

    // Query
    let (stdout, _, code) = run(&[
        "memory",
        "query",
        "--project-dir",
        dir,
        "--text",
        "atomic",
        "--limit",
        "5",
    ]);
    assert_eq!(code, 0, "query failed: {stdout}");
    assert!(stdout.contains(mem_id));

    // Delete
    let (stdout, _, code) = run(&["memory", "delete", "--project-dir", dir, "--id", mem_id]);
    assert_eq!(code, 0, "delete failed: {stdout}");
    assert!(stdout.contains("\"status\": \"deleted\""));

    // Query again — should be empty
    let (stdout, _, code) = run(&["memory", "query", "--project-dir", dir, "--text", "atomic"]);
    assert_eq!(code, 0);
    let result: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(result["injected_count"], 0);
}

#[test]
fn task_crud_via_cli() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_str().unwrap();
    init_project(tmp.path());

    // Create
    let (stdout, _, code) = run(&[
        "task",
        "update",
        "--project-dir",
        dir,
        "--id",
        "t1",
        "--title",
        "CLI task",
        "--status",
        "todo",
        "--priority",
        "high",
        "--estimate-hours",
        "1",
    ]);
    assert_eq!(code, 0, "create failed: {stdout}");
    assert!(stdout.contains("t1"));

    // Get
    let (stdout, _, code) = run(&["task", "get", "--project-dir", dir, "--task-id", "t1"]);
    assert_eq!(code, 0, "get failed: {stdout}");
    let task: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(task["title"], "CLI task");
    assert_eq!(task["priority"], "high");

    // List
    let (stdout, _, code) = run(&["task", "list", "--project-dir", dir]);
    assert_eq!(code, 0);
    assert!(stdout.contains("\"total\": 1"));

    // Log time
    let (stdout, _, code) = run(&[
        "task",
        "log-time",
        "--project-dir",
        dir,
        "--task-id",
        "t1",
        "--hours",
        "0.5",
    ]);
    assert_eq!(code, 0, "log-time failed: {stdout}");
    assert!(stdout.contains("0.5h"));
}

#[test]
fn metrics_via_cli() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_str().unwrap();
    init_project(tmp.path());

    let (stdout, _, code) = run(&["metrics", "--project-dir", dir]);
    assert_eq!(code, 0, "metrics failed: {stdout}");
    let m: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(m["total"], 0);
}

#[test]
fn session_load_via_cli() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_str().unwrap();
    init_project(tmp.path());

    let (stdout, _, code) = run(&["session", "load", "--project-dir", dir]);
    assert_eq!(code, 0, "session load failed: {stdout}");
    let s: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(s["project"], "CLITest");
}

#[test]
fn config_get_via_cli() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_str().unwrap();
    init_project(tmp.path());

    let (stdout, _, code) = run(&["config", "get", "--project-dir", dir]);
    assert_eq!(code, 0, "config get failed: {stdout}");
    let c: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(c["project"]["name"], "CLITest");
}

#[test]
fn comma_separated_tags_become_array() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_str().unwrap();
    init_project(tmp.path());

    let (stdout, _, code) = run(&[
        "memory",
        "save",
        "--project-dir",
        dir,
        "--text",
        "rule with tags",
        "--kind",
        "rule",
        "--tags",
        "safety,io,perf",
    ]);
    assert_eq!(code, 0, "save with tags failed: {stdout}");

    // Query and check tags survived
    let (stdout, _, _) = run(&[
        "memory",
        "query",
        "--project-dir",
        dir,
        "--text",
        "rule tags safety",
    ]);
    assert!(stdout.contains("rule with tags"));
}

#[test]
fn numeric_looking_task_id_stays_string() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_str().unwrap();
    init_project(tmp.path());

    // Create task with numeric-looking id
    let (stdout, _, code) = run(&[
        "task",
        "update",
        "--project-dir",
        dir,
        "--id",
        "42",
        "--title",
        "Numeric ID",
        "--status",
        "todo",
        "--estimate-hours",
        "1",
    ]);
    assert_eq!(code, 0, "create with numeric id failed: {stdout}");

    // Get it back
    let (stdout, _, code) = run(&["task", "get", "--project-dir", dir, "--task-id", "42"]);
    assert_eq!(code, 0, "get with numeric id failed: {stdout}");
    let task: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(task["id"], "42");
    assert_eq!(task["title"], "Numeric ID");
}

#[test]
fn help_after_action_shows_group_help() {
    let (stdout, _, code) = run(&["memory", "save", "--help"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("save"));
    assert!(stdout.contains("query"));
}

#[test]
fn error_output_has_exit_code_1() {
    let (stdout, _, code) = run(&[
        "memory",
        "delete",
        "--project-dir",
        "/nonexistent/path/here",
        "--id",
        "m-fake",
    ]);
    assert_eq!(code, 1);
    assert!(stdout.contains("error"));
}
