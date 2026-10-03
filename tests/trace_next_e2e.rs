//! Real-binary E2E for `handoff_trace_next` / CLI `trace next`
//! (wiki/260-vmodel-m2-design.md §3.5/§4.5/§5.3, M2-10): the 8 kinds' ranking
//! over a real project, `task_id`/`layers`/`kinds`/`limit` filtering, the CLI
//! entry point, and the E6 "never writes to `.handoff/`" contract (same
//! pattern `tests/trace_matrix_e2e.rs`/`tests/trace_lint_e2e.rs` use).

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use handoff_mcp::storage::config::{read_config, write_config};
use serde_json::{json, Value};

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

fn run_cli(args: &[&str]) -> (String, String, i32) {
    let output = Command::new(binary())
        .args(args)
        .output()
        .expect("failed to run binary");
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
        output.status.code().unwrap_or(-1),
    )
}

struct Server {
    child: Child,
    stdin: std::process::ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
}

impl Server {
    fn spawn() -> Self {
        let mut cmd = Command::new(binary());
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("failed to spawn handoff-mcp server");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut buf = String::new();
                match reader.read_line(&mut buf) {
                    Ok(0) => break,
                    Ok(_) => {
                        if tx.send(buf.trim_end().to_string()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Server {
            child,
            stdin,
            lines: rx,
            next_id: 1,
        }
    }

    fn call_raw(&mut self, name: &str, arguments: Value) -> (bool, String) {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        });
        writeln!(self.stdin, "{req}").expect("write to server stdin");
        self.stdin.flush().expect("flush server stdin");

        let line = self
            .lines
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| panic!("no response for {name} within 10s"));
        let resp: Value = serde_json::from_str(&line).expect("valid JSON-RPC response");
        assert_eq!(resp["id"], id, "response id must match request id");
        let is_error = resp["result"]["isError"].as_bool().unwrap_or(false);
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        (is_error, text)
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        let (is_error, text) = self.call_raw(name, arguments);
        assert!(!is_error, "{name} failed: {text}");
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn snapshot(handoff: &std::path::Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else {
                    out.push(path);
                }
            }
        }
    }
    let mut paths = Vec::new();
    walk(handoff, &mut paths);
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            let bytes = std::fs::read(&p).unwrap();
            (p, bytes)
        })
        .collect()
}

/// REQ-001 (P0, no verifier -> horizontal uncovered -> write_verification,
/// and not_started with no implementing task -> create_task) and REQ-002
/// (P1, implemented, verified by AT-002 which is `not_run` -> rerun; task t2
/// `implements` REQ-002 so no create_task for it). `acceptance` is the
/// project's in-use right-side pair for `requirement` (REQ-001 needs it to
/// read as `uncovered` rather than `na`).
fn build_project(server: &mut Server, dir: &std::path::Path) {
    let pd = dir.to_string_lossy().to_string();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "trace-next-e2e" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "requirements-next-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-001 Account lockout\n\n- priority: P0\n\nAfter 5 failures the account locks.\n\n### REQ-002 Session timeout\n\n- priority: P1\n\nIdle sessions expire after 15 minutes.\n",
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "acceptance-next-e2e",
            "title": "Acceptance tests",
            "layer": "acceptance",
            "body": "# Acceptance\n\n### AT-002 Session expires\n\n- verifies: REQ-002\n- method: manual\n\nWait 15 minutes idle, then confirm the session is gone.\n",
        }),
    );
    server.call(
        "handoff_update_task",
        json!({
            "project_dir": pd,
            "task": {
                "id": "t2",
                "title": "Implement session timeout",
                "requirement_ids": ["REQ-002", "AT-002"],
                "requirement_roles": {"REQ-002": "implements", "AT-002": "executes"},
            },
        }),
    );
    // `dev_stage` is tool-managed (not a body attribute, wiki/260 §3.3) —
    // REQ-002 must be `implemented` for AT-002's `rerun` candidacy (§3.5:
    // "対象が implemented 以上").
    server.call(
        "handoff_trace_update",
        json!({
            "project_dir": pd,
            "ops": [{"op": "set", "item": "REQ-002", "dev_stage": "implemented"}],
        }),
    );
}

#[test]
fn actions_cover_write_verification_create_task_and_rerun_with_concrete_suggests() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);

    let resp = server.call("handoff_trace_next", json!({ "project_dir": pd }));
    let actions = resp["actions"].as_array().expect("actions array");
    assert!(!actions.is_empty());

    let write_verification = actions
        .iter()
        .find(|a| a["kind"] == "write_verification" && a["item"] == "REQ-001")
        .unwrap_or_else(|| {
            panic!("expected a write_verification action for REQ-001, got {actions:#?}")
        });
    assert_eq!(
        write_verification["suggest"]["tool"],
        "handoff_trace_scaffold"
    );
    assert_eq!(write_verification["rank"], 4);

    let create_task = actions
        .iter()
        .find(|a| a["kind"] == "create_task" && a["item"] == "REQ-001")
        .unwrap_or_else(|| panic!("expected a create_task action for REQ-001, got {actions:#?}"));
    assert_eq!(create_task["suggest"]["tool"], "handoff_trace_tasks");
    assert_eq!(create_task["rank"], 6);

    // REQ-002 already has an implementing task (t2) -> no create_task for it.
    assert!(!actions
        .iter()
        .any(|a| a["kind"] == "create_task" && a["item"] == "REQ-002"));

    let rerun = actions
        .iter()
        .find(|a| a["kind"] == "rerun" && a["item"] == "AT-002")
        .unwrap_or_else(|| panic!("expected a rerun action for AT-002 (not_run, target implemented), got {actions:#?}"));
    assert_eq!(rerun["suggest"]["tool"], "handoff_trace_ingest");
    assert_eq!(rerun["rank"], 3);
}

#[test]
fn ordering_is_deterministic_across_repeated_calls() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);

    let first = server.call("handoff_trace_next", json!({ "project_dir": pd }));
    let second = server.call("handoff_trace_next", json!({ "project_dir": pd }));
    assert_eq!(
        first, second,
        "repeated calls over an unchanged project must return byte-identical ordering"
    );
}

#[test]
fn task_id_narrows_to_that_tasks_own_linked_items_only() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);

    let resp = server.call(
        "handoff_trace_next",
        json!({ "project_dir": pd, "task_id": "t2" }),
    );
    let actions = resp["actions"].as_array().expect("actions array");
    assert!(!actions.is_empty());
    for a in actions {
        let item = a["item"].as_str().unwrap_or_default();
        assert!(
            item == "REQ-002" || item == "AT-002",
            "task_id=t2 must only surface actions for REQ-002/AT-002, got {a:#?}"
        );
    }
}

#[test]
fn unknown_task_id_is_rejected() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);

    let (is_error, text) = server.call_raw(
        "handoff_trace_next",
        json!({ "project_dir": pd, "task_id": "does-not-exist" }),
    );
    assert!(
        is_error,
        "expected an error for an unknown task_id, got {text}"
    );
}

#[test]
fn kinds_filter_restricts_the_output() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);

    let resp = server.call(
        "handoff_trace_next",
        json!({ "project_dir": pd, "kinds": ["create_task"] }),
    );
    let actions = resp["actions"].as_array().expect("actions array");
    assert!(!actions.is_empty());
    assert!(actions.iter().all(|a| a["kind"] == "create_task"));
}

#[test]
fn unknown_kind_is_rejected() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);

    let (is_error, text) = server.call_raw(
        "handoff_trace_next",
        json!({ "project_dir": pd, "kinds": ["not_a_real_kind"] }),
    );
    assert!(
        is_error,
        "expected an error for an unknown kind, got {text}"
    );
}

#[test]
fn limit_truncates_and_reports_truncated_true() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);

    let resp = server.call(
        "handoff_trace_next",
        json!({ "project_dir": pd, "limit": 1 }),
    );
    let actions = resp["actions"].as_array().expect("actions array");
    assert_eq!(actions.len(), 1);
    assert_eq!(resp["truncated"], true);
}

#[test]
fn cli_trace_next_matches_the_mcp_tool_and_never_writes_to_handoff() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();
    let pd = dir.to_string_lossy().to_string();
    let handoff = dir.join(".handoff");

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    let mcp_resp = server.call("handoff_trace_next", json!({ "project_dir": pd }));
    drop(server);

    let before = snapshot(&handoff);
    let (stdout, stderr, code) = run_cli(&["trace", "next", "--project-dir", dir_str]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let after = snapshot(&handoff);
    assert_eq!(
        before, after,
        "trace next must never write any byte under .handoff/: stdout={stdout} stderr={stderr}"
    );

    let cli_resp: Value = serde_json::from_str(&stdout).expect("CLI output must be valid JSON");
    assert_eq!(
        cli_resp, mcp_resp,
        "CLI `trace next` and the `handoff_trace_next` MCP tool must return identical output"
    );
}

#[test]
fn cli_trace_next_task_id_and_limit_flags_reach_the_handler() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    drop(server);

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "next",
        "--project-dir",
        dir_str,
        "--task-id",
        "t2",
        "--limit",
        "1",
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let resp: Value = serde_json::from_str(&stdout).expect("valid JSON");
    let actions = resp["actions"].as_array().expect("actions array");
    assert_eq!(actions.len(), 1);
    let item = actions[0]["item"].as_str().unwrap_or_default();
    assert!(item == "REQ-002" || item == "AT-002");
}

#[test]
fn trace_report_persists_next_actions_matching_the_live_tool() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);

    let live = server.call(
        "handoff_trace_next",
        json!({ "project_dir": pd, "limit": 20 }),
    );
    server.call("handoff_trace_report", json!({ "project_dir": pd }));

    let persisted: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join(".handoff/docs/_trace_report.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        persisted["next_actions"], live["actions"],
        "_trace_report.json's next_actions must match handoff_trace_next's own actions[] for the \
         same project state"
    );
}

// -- M3-02: assignee filter + manual_pending kind
// (wiki/270-vmodel-m3-design.md §4.5, FR-307) --

/// REQ-010 (P1) is verified by AT-010 (method: manual, assignee: ryoma, never
/// run) and AT-011 (method: auto, no assignee, never run) — only AT-010
/// qualifies for `manual_pending`; `assignee="ryoma"` must return exactly
/// that item and nothing else, while the unfiltered call returns a strictly
/// larger set (REQ-010 itself has no create_task candidate since dev_stage
/// defaults unset -> not_started with no task, so it also appears).
fn build_assignee_project(server: &mut Server, dir: &std::path::Path) {
    let pd = dir.to_string_lossy().to_string();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "trace-next-assignee-e2e" }),
    );
    server.call(
        "handoff_add_assignee",
        json!({ "project_dir": pd, "key": "ryoma" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "requirements-assignee-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-010 Manual review needed\n\n- priority: P1\n\nSomething that needs eyeballing.\n",
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "acceptance-assignee-e2e",
            "title": "Acceptance tests",
            "layer": "acceptance",
            "body": "# Acceptance\n\n### AT-010 Manual check\n\n- verifies: REQ-010\n- method: manual\n- assignee: ryoma\n\nManually eyeball the output.\n\n### AT-011 Automated check\n\n- verifies: REQ-010\n- method: auto\n\nRun the automated suite.\n",
        }),
    );
}

#[test]
fn assignee_filter_returns_only_that_assignees_items_and_fewer_than_unfiltered() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    build_assignee_project(&mut server, &dir);

    let unfiltered = server.call(
        "handoff_trace_next",
        json!({ "project_dir": pd, "limit": 50 }),
    );
    let unfiltered_actions = unfiltered["actions"].as_array().expect("actions array");

    let filtered = server.call(
        "handoff_trace_next",
        json!({ "project_dir": pd, "assignee": "ryoma", "limit": 50 }),
    );
    let filtered_actions = filtered["actions"].as_array().expect("actions array");

    assert!(
        !filtered_actions.is_empty(),
        "expected at least AT-010's manual_pending action, got {filtered_actions:#?}"
    );
    assert!(
        filtered_actions.iter().all(|a| a["item"] == "AT-010"),
        "assignee=ryoma must only surface AT-010's own actions, got {filtered_actions:#?}"
    );
    assert!(
        filtered_actions.len() < unfiltered_actions.len(),
        "assignee filter must return strictly fewer actions than the unfiltered call: \
         filtered={filtered_actions:#?} unfiltered={unfiltered_actions:#?}"
    );

    let manual_pending = filtered_actions
        .iter()
        .find(|a| a["kind"] == "manual_pending")
        .unwrap_or_else(|| {
            panic!("expected a manual_pending action for AT-010, got {filtered_actions:#?}")
        });
    assert_eq!(
        manual_pending["rank"], 3,
        "manual_pending shares rerun's rank (3)"
    );

    // The automated AT-011 (no assignee) must never appear as manual_pending
    // in the unfiltered call either.
    assert!(!unfiltered_actions
        .iter()
        .any(|a| a["item"] == "AT-011" && a["kind"] == "manual_pending"));
}

#[test]
fn cli_trace_next_assignee_flag_reaches_the_handler() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    build_assignee_project(&mut server, &dir);
    drop(server);

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "next",
        "--project-dir",
        dir_str,
        "--assignee",
        "ryoma",
        "--limit",
        "50",
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let resp: Value = serde_json::from_str(&stdout).expect("valid JSON");
    let actions = resp["actions"].as_array().expect("actions array");
    assert!(!actions.is_empty());
    assert!(actions.iter().all(|a| a["item"] == "AT-010"));
}

// -- M3-13: `relink_candidate` kind (wiki/270-vmodel-m3-design.md §4.7,
// FR-204) --

/// SPEC-001 (basic_spec) is directly `verifies`-linked by UT-001
/// (unit_test). DS-001 (detailed_spec) `refines` SPEC-001 — added to the
/// project *after* the basic_spec/unit_test pair was already wired up, same
/// "途中からの層追加" scenario §4.7 describes. The project default profile
/// stays `"standard"` (requirement/basic_spec/acceptance/system_test only);
/// `[trace] layers` is set explicitly to standard's own set plus
/// `detailed_spec`/`unit_test`, mirroring a project that grew a deeper tier
/// mid-stream while still nominally on the `standard` profile.
fn build_relink_project(server: &mut Server, dir: &std::path::Path) {
    let pd = dir.to_string_lossy().to_string();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "trace-next-relink-e2e" }),
    );

    let config_path = dir.join(".handoff").join("config.toml");
    let mut config = read_config(&config_path).expect("read config");
    config.trace.profile = Some("standard".to_string());
    config.trace.layers = vec![
        "requirement".to_string(),
        "basic_spec".to_string(),
        "acceptance".to_string(),
        "system_test".to_string(),
        "detailed_spec".to_string(),
        "unit_test".to_string(),
    ];
    write_config(&config_path, &config).expect("write config");

    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "basic-spec-relink-e2e",
            "title": "Basic spec",
            "layer": "basic_spec",
            "body": "# Basic spec\n\n### SPEC-001 Lockout rule\n\nAfter 5 failures the account locks.\n",
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "unit-tests-relink-e2e",
            "title": "Unit tests",
            "layer": "unit_test",
            "body": "# Unit tests\n\n### UT-001 Lockout unit test\n\n- verifies: SPEC-001\n\nAsserts the lockout counter.\n",
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "detailed-spec-relink-e2e",
            "title": "Detailed spec",
            "layer": "detailed_spec",
            "body": "# Detailed spec\n\n### DS-001 Lockout counter detail\n\n- refines: SPEC-001\n\nCounter increments per failure, resets on success.\n",
        }),
    );
}

#[test]
fn relink_candidate_fires_once_detailed_spec_is_added_under_standard_profile() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    build_relink_project(&mut server, &dir);

    let resp = server.call(
        "handoff_trace_next",
        json!({ "project_dir": pd, "limit": 50 }),
    );
    let actions = resp["actions"].as_array().expect("actions array");

    let relink = actions
        .iter()
        .find(|a| a["kind"] == "relink_candidate" && a["item"] == "UT-001")
        .unwrap_or_else(|| {
            panic!("expected a relink_candidate action for UT-001, got {actions:#?}")
        });
    assert_eq!(relink["suggest"]["tool"], "handoff_trace_update");
    assert_eq!(relink["suggest"]["arguments"]["dry_run"], true);
    let ops = relink["suggest"]["arguments"]["ops"]
        .as_array()
        .expect("ops array");
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0]["op"], "upsert_item");
    assert_eq!(ops[0]["id"], "UT-001");
    assert_eq!(ops[0]["attrs"]["verifies"], json!(["DS-001"]));
}

#[test]
fn relink_candidate_kinds_filter_reaches_the_handler() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    build_relink_project(&mut server, &dir);

    let resp = server.call(
        "handoff_trace_next",
        json!({ "project_dir": pd, "kinds": ["relink_candidate"], "limit": 50 }),
    );
    let actions = resp["actions"].as_array().expect("actions array");
    assert!(!actions.is_empty());
    assert!(actions.iter().all(|a| a["kind"] == "relink_candidate"));
}
