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

/// Saves a layer document through the binary's stdio JSON-RPC transport (one
/// request, then the server is closed), independently of the CLI under test.
fn save_requirement_doc_over_stdio(project_dir: &str) {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;

    let mut child = Command::new(binary())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn handoff-mcp server");
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "handoff_doc_save",
            "arguments": {
                "project_dir": project_dir,
                "slug": "req-cli",
                "title": "Requirements",
                "layer": "requirement",
                "body": "# Req\n\n### REQ-001 Something\n\nBody.\n",
            },
        },
    });
    let mut stdin = child.stdin.take().expect("stdin");
    writeln!(stdin, "{request}").expect("write request");
    stdin.flush().expect("flush request");
    let mut line = String::new();
    BufReader::new(child.stdout.take().expect("stdout"))
        .read_line(&mut line)
        .expect("read response");
    drop(stdin);
    let _ = child.wait();
    let response: serde_json::Value = serde_json::from_str(&line).expect("JSON-RPC response");
    assert_ne!(
        response["result"]["isError"],
        serde_json::json!(true),
        "doc_save failed: {response}"
    );
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
fn metrics_snapshot_via_cli() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_str().unwrap();
    init_project(tmp.path());

    let (stdout, _, code) = run(&["metrics", "snapshot", "--project-dir", dir]);
    assert_eq!(code, 0, "metrics snapshot failed: {stdout}");
    let snap: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(snap["schema_version"], 2);
    assert!(snap["tasks"].is_array());
    let path = snap["path"].as_str().unwrap();
    assert!(tmp.path().join(".handoff").join(path).is_file());
}

#[test]
fn metrics_snapshot_at_via_cli_replays_changes_after_capture() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_str().unwrap();
    init_project(tmp.path());

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
    ]);
    assert_eq!(code, 0, "create failed: {stdout}");
    let (_, _, code) = run(&["metrics", "snapshot", "--project-dir", dir]);
    assert_eq!(code, 0);
    let (stdout, _, code) = run(&[
        "task",
        "update",
        "--project-dir",
        dir,
        "--id",
        "t1",
        "--status",
        "in_progress",
        "--estimate-hours",
        "1",
    ]);
    assert_eq!(code, 0, "update failed: {stdout}");

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let (stdout, stderr, code) = run(&[
        "metrics",
        "snapshot-at",
        "--project-dir",
        dir,
        "--date",
        &today,
    ]);
    assert_eq!(code, 0, "snapshot-at failed: {stdout} {stderr}");
    let st: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(st["source"], "snapshot_plus_events");
    assert_eq!(st["exact"], false);
    assert_eq!(st["tasks"][0]["status"], "in_progress");
}

#[test]
fn metrics_help_lists_snapshot_actions() {
    let (stdout, _, code) = run(&["metrics", "--help"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("snapshot-at"), "{stdout}");
    assert!(stdout.contains("snapshot "), "{stdout}");
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
fn session_load_task_tree_and_task_scope_flags_via_cli() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_str().unwrap();
    init_project(tmp.path());
    for title in ["Alpha", "Beta"] {
        let task = serde_json::json!({
            "title": title,
            "status": "done",
            "priority": "low",
            "schedule": { "estimate_hours": 1 },
        })
        .to_string();
        let (stdout, _, code) = run(&["task", "update", "--project-dir", dir, "--task", &task]);
        assert_eq!(code, 0, "task update failed: {stdout}");
    }

    // Compact is the default; a single --task-scope value is a one-item list,
    // and a done task named by it is not included.
    let (stdout, _, code) = run(&[
        "session",
        "load",
        "--project-dir",
        dir,
        "--task-scope",
        "t1",
    ]);
    assert_eq!(code, 0, "{stdout}");
    let compact: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(compact["task_tree_meta"]["mode"], "compact");
    assert_eq!(compact["task_tree"].as_array().unwrap().len(), 0);
    assert_eq!(compact["task_tree_meta"]["omitted_total"], 2);

    let (stdout, _, code) = run(&[
        "session",
        "load",
        "--project-dir",
        dir,
        "--task-tree",
        "full",
    ]);
    assert_eq!(code, 0, "{stdout}");
    let full: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(full["task_tree_meta"]["mode"], "full");
    assert_eq!(full["task_tree"].as_array().unwrap().len(), 2);

    let (stdout, stderr, code) = run(&[
        "session",
        "load",
        "--project-dir",
        dir,
        "--task-tree",
        "wide",
    ]);
    assert_ne!(code, 0, "{stdout}");
    assert!(
        stdout.contains("compact") && stdout.contains("full")
            || stderr.contains("compact") && stderr.contains("full"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
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

#[test]
fn end_of_options_marker_allows_dash_prefixed_values() {
    // `--key -- <value>` forces the next argument to be read as a value. Without
    // it, user text beginning with `--` is silently swallowed as a boolean flag
    // — which quietly disabled memory injection for any prompt starting with a
    // dash (a pasted flag, "--force is not working").
    let dir = tempfile::tempdir().expect("tempdir");
    init_project(dir.path());

    let (save_out, _, save_code) = run(&[
        "memory",
        "save",
        "--project-dir",
        dir.path().to_str().unwrap(),
        "--text",
        "ZEBRA_TOKEN must be exported before make release; the CI runner masks it in logs.",
        "--kind",
        "gotcha",
    ]);
    assert_eq!(save_code, 0, "save failed: {save_out}");

    let query = "--ZEBRA_TOKEN must be exported before make release CI runner masks logs";
    let (stdout, _, code) = run(&[
        "memory",
        "query",
        "--project-dir",
        dir.path().to_str().unwrap(),
        "--text",
        "--",
        query,
        "--limit",
        "5",
    ]);
    assert_eq!(code, 0);

    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let n = v["memories"].as_array().map(|a| a.len()).unwrap_or(0);
    assert_eq!(
        n, 1,
        "a `--`-prefixed value after `--` must reach the tool, got: {stdout}"
    );
}

#[test]
fn report_workflow_via_cli() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_str().unwrap();
    init_project(tmp.path());

    let (stdout, _, code) = run(&[
        "report",
        "generate",
        "--project-dir",
        dir,
        "--report-type",
        "weekly",
        "--scope",
        r#"{"label":"w42"}"#,
    ]);
    assert_eq!(code, 0, "report generate failed: {stdout}");
    let out: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let id = out["report"]["report_id"].as_str().unwrap().to_string();
    assert_eq!(out["report"]["status"], "draft");

    let (stdout, _, code) = run(&["report", "list", "--project-dir", dir]);
    assert_eq!(code, 0, "{stdout}");
    let list: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(list["reports"].as_array().unwrap().len(), 1);

    let (_, _, code) = run(&["report", "submit", "--project-dir", dir, "--report-id", &id]);
    assert_eq!(code, 0);
    let (stdout, _, code) = run(&[
        "report",
        "approve",
        "--project-dir",
        dir,
        "--report-id",
        &id,
        "--reviewer",
        "alice",
    ]);
    assert_eq!(code, 0, "{stdout}");
    let (stdout, _, code) = run(&["report", "get", "--project-dir", dir, "--report-id", &id]);
    assert_eq!(code, 0, "{stdout}");
    let got: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(got["report"]["status"], "approved");
    assert_eq!(got["report"]["reviewer"], "alice");
    assert!(got["body"].as_str().unwrap().contains("w42"));

    // Invalid transition exits non-zero.
    let (_, _, code) = run(&["report", "submit", "--project-dir", dir, "--report-id", &id]);
    assert_eq!(code, 1);
}

/// FR-520 (t531): `trace test-run` is reachable from the CLI — handoff-vscode's
/// campaign tab spawns it. Free text such as a label with a comma must arrive
/// as text (not be split into an array), and a refused status transition must
/// exit non-zero.
#[test]
fn trace_test_run_campaign_via_cli() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_str().unwrap();
    init_project(tmp.path());

    // A campaign needs at least one target item (an empty scope is refused).
    save_requirement_doc_over_stdio(dir);

    let (stdout, stderr, code) = run(&[
        "trace",
        "test-run",
        "--action",
        "create",
        "--project-dir",
        dir,
        "--label",
        "sprint,5",
    ]);
    assert_eq!(code, 0, "create failed: {stdout} {stderr}");
    let created: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let id = created["test_run_id"].as_str().unwrap().to_string();
    assert_eq!(created["campaign_status"], "draft");

    let (stdout, _, code) = run(&[
        "trace",
        "test-run",
        "--action",
        "list",
        "--project-dir",
        dir,
    ]);
    assert_eq!(code, 0, "{stdout}");
    let list: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(list["test_runs"][0]["label"], "sprint,5");

    let (stdout, _, code) = run(&[
        "trace",
        "test-run",
        "--action",
        "set_status",
        "--project-dir",
        dir,
        "--test-run-id",
        &id,
        "--status",
        "in_progress",
    ]);
    assert_eq!(code, 0, "{stdout}");
    let set: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(set["campaign_status"], "in_progress");

    // draft/in_progress -> approved is not an allowed transition.
    let (_, _, code) = run(&[
        "trace",
        "test-run",
        "--action",
        "set_status",
        "--project-dir",
        dir,
        "--test-run-id",
        &id,
        "--status",
        "approved",
        "--approved-by",
        "lead",
    ]);
    assert_eq!(code, 1);

    let (stdout, _, _) = run(&["trace", "--help"]);
    assert!(stdout.contains("test-run"), "{stdout}");
}

/// Creates task `t1` through the CLI and returns its requirement stable_ids as
/// `task get` reports them (the `requirement` entries of `task_links`).
fn linked_requirements(dir: &str, task_id: &str) -> Vec<String> {
    let (stdout, stderr, code) = run(&["task", "get", "--project-dir", dir, "--task-id", task_id]);
    assert_eq!(code, 0, "task get failed: {stdout} {stderr}");
    let task: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let mut ids: Vec<String> = task["task_links"]
        .as_array()
        .map(|links| {
            links
                .iter()
                .filter(|l| l["link_type"] == "requirement")
                .filter_map(|l| l["label"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}

fn create_cli_task(dir: &str) {
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
    ]);
    assert_eq!(code, 0, "create failed: {stdout}");
}

/// WS1 / t430 (SPEC-V0473-101): the CLI-equivalent table given to sub-agents
/// writes `requirement_ids` through `--task '{...}'`; `task get` must show the
/// stored link.
#[test]
fn task_update_json_task_arg_stores_requirement_ids() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_str().unwrap();
    init_project(tmp.path());
    save_requirement_doc_over_stdio(dir);
    create_cli_task(dir);

    let (stdout, stderr, code) = run(&[
        "task",
        "update",
        "--project-dir",
        dir,
        "--task",
        r#"{"id":"t1","requirement_ids":["REQ-001"]}"#,
    ]);
    assert_eq!(code, 0, "update failed: {stdout} {stderr}");
    assert_eq!(linked_requirements(dir, "t1"), vec!["REQ-001".to_string()]);
}

/// The flat `--requirement-ids` flag used to land at the top level of the
/// arguments and be silently dropped; it must be routed into `task`, and a
/// single value (no comma) must still be a one-element array.
#[test]
fn task_update_flat_requirement_ids_flag_is_routed_into_task() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_str().unwrap();
    init_project(tmp.path());
    save_requirement_doc_over_stdio(dir);
    create_cli_task(dir);

    let (stdout, stderr, code) = run(&[
        "task",
        "update",
        "--project-dir",
        dir,
        "--id",
        "t1",
        "--requirement-ids",
        "REQ-001",
    ]);
    assert_eq!(code, 0, "update failed: {stdout} {stderr}");
    assert_eq!(linked_requirements(dir, "t1"), vec!["REQ-001".to_string()]);
}

/// `trace update` reports an invalid op as `failed` in a normal response. The
/// CLI used to exit 0 on it, so a sub-agent branching on `$?` believed the
/// write had landed.
#[test]
fn trace_update_failure_exits_non_zero() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_str().unwrap();
    init_project(tmp.path());
    save_requirement_doc_over_stdio(dir);

    let (stdout, _, code) = run(&[
        "trace",
        "update",
        "--project-dir",
        dir,
        "--ops",
        r#"[{"op":"set","item":"NO-SUCH-ITEM","dev_stage":"implemented"}]"#,
    ]);
    assert_eq!(code, 1, "a failed op must exit 1: {stdout}");
    let out: serde_json::Value = serde_json::from_str(&stdout).expect("failure body stays JSON");
    assert!(out["failed"].is_object(), "{stdout}");

    let (stdout, _, code) = run(&[
        "trace",
        "update",
        "--project-dir",
        dir,
        "--ops",
        r#"[{"op":"set","item":"REQ-001","dev_stage":"implemented"}]"#,
    ]);
    assert_eq!(code, 0, "a valid op must still exit 0: {stdout}");
}

/// `doc update-section --seq 1`: `seq` is an integer parameter, so the CLI has
/// to coerce it (the session-spec-reconciler's CLI fallback table relies on
/// this command).
#[test]
fn doc_update_section_accepts_numeric_seq_via_cli() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_str().unwrap();
    init_project(tmp.path());
    let body = tmp.path().join("plain.md");
    std::fs::write(&body, "# A\n\nfirst\n\n## B\n\nsecond\n").unwrap();
    let (stdout, stderr, code) = run(&[
        "doc",
        "save",
        "--project-dir",
        dir,
        "--slug",
        "plain",
        "--title",
        "Plain",
        "--body-file",
        body.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "doc save failed: {stdout} {stderr}");
    let saved: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let doc_id = saved["doc_id"].as_str().unwrap().to_string();

    let (stdout, stderr, code) = run(&[
        "doc",
        "update-section",
        "--project-dir",
        dir,
        "--doc-id",
        &doc_id,
        "--seq",
        "1",
        "--new-content",
        "## B\n\nchanged\n",
    ]);
    assert_eq!(code, 0, "update-section failed: {stdout} {stderr}");

    let (stdout, _, code) = run(&["doc", "get", "--project-dir", dir, "--doc-id", &doc_id]);
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("changed"), "section not updated: {stdout}");
}
