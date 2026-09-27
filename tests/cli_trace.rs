//! CLI E2E tests for the `trace` command group (t360.13,
//! wiki/220-vmodel-integration-design.md §3.4): `handoff-mcp trace
//! report|record|slice|history` must run without a shell
//! (`std::process::Command::new` + `.args`, never a string passed to `sh -c`)
//! and print JSON to stdout — the contract handoff-vscode's VSCode-side
//! caller relies on (it spawns the native binary directly, wiki/220 §3.4:
//! "シェルなしで起動できる形"). Project setup uses the real binary's stdio
//! JSON-RPC transport (same harness as `tests/trace_report_slice_e2e.rs`)
//! since there is no `doc`/`update_task` CLI group yet; the `trace`
//! subcommands themselves are always exercised through the plain CLI.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

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

/// Runs the CLI binary directly (no shell) and returns (stdout, stderr, exit
/// code).
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

    fn call(&mut self, name: &str, arguments: Value) -> Value {
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

/// Builds a small V-model project: `requirement` doc with REQ-001, an
/// `acceptance` doc with AT-001 verifying it, and task `t1` implementing
/// REQ-001.
fn build_project(server: &mut Server, dir: &std::path::Path) {
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "cli-trace-e2e" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "requirements-cli-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-001 Account lockout\n\n- priority: P0\n\nAfter 5 failures the account locks.\n",
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "acceptance-cli-e2e",
            "title": "Acceptance tests",
            "layer": "acceptance",
            "body": "# Acceptance\n\n### AT-001 Lockout after 5 failures\n\n- verifies: REQ-001\n- method: manual\n\nFail login 5 times, then confirm the account is locked.\n",
        }),
    );
    server.call(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "id": "t1", "title": "Implement lockout", "requirement_ids": ["REQ-001"] },
        }),
    );
}

#[test]
fn cli_trace_report_writes_the_derived_file_and_prints_json_over_stdout() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    drop(server);

    let (stdout, stderr, code) = run_cli(&["trace", "report", "--project-dir", dir_str]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("`trace report` must print JSON to stdout: {e}: {stdout}"));
    assert!(parsed.get("trace_layers").is_some(), "{parsed}");
    assert!(parsed.get("coverage").is_some(), "{parsed}");

    let trace_report_path = dir.join(".handoff/docs/_trace_report.json");
    assert!(
        trace_report_path.exists(),
        "CLI `trace report` must (re)generate _trace_report.json"
    );
    let persisted: Value =
        serde_json::from_str(&std::fs::read_to_string(&trace_report_path).unwrap()).unwrap();
    assert_eq!(persisted["schema_version"], 1);
    assert!(persisted["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|it| it["id"] == "REQ-001"));
}

#[test]
fn cli_trace_report_include_items_flag_is_parsed() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    drop(server);

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "report",
        "--project-dir",
        dir_str,
        "--include-items",
        "true",
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    let items = parsed["items"]
        .as_array()
        .expect("include-items=true must add items[] to the CLI response");
    assert!(items.iter().any(|it| it["id"] == "REQ-001"));
}

#[test]
fn cli_trace_record_records_a_result_without_a_shell() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    drop(server);

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "record",
        "--project-dir",
        dir_str,
        "--results",
        r#"[{"item":"AT-001","result":"pass"}]"#,
        "--task-id",
        "t1",
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["recorded"], 1);
    assert!(!parsed["run_id"].as_str().unwrap().is_empty());

    let latest: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join(".handoff/runs/_latest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(latest["items"]["AT-001"]["result"], "pass");
}

#[test]
fn cli_trace_slice_returns_the_neighborhood_without_a_shell() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    drop(server);

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "slice",
        "--project-dir",
        dir_str,
        "--item",
        "REQ-001",
        "--direction",
        "down",
        "--max-items",
        "5",
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    let ids: Vec<String> = parsed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|it| it["id"].as_str().unwrap().to_string())
        .collect();
    assert!(ids.contains(&"REQ-001".to_string()));
    assert!(ids.contains(&"AT-001".to_string()), "{ids:?}");

    // task_id-based slice + numeric --depth flag also parse correctly.
    let (stdout2, stderr2, code2) = run_cli(&[
        "trace",
        "slice",
        "--project-dir",
        dir_str,
        "--task-id",
        "t1",
        "--depth",
        "1",
    ]);
    assert_eq!(code2, 0, "stdout={stdout2} stderr={stderr2}");
    let parsed2: Value = serde_json::from_str(&stdout2).unwrap();
    let ids2: Vec<String> = parsed2["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|it| it["id"].as_str().unwrap().to_string())
        .collect();
    assert!(ids2.contains(&"REQ-001".to_string()), "{ids2:?}");
}

#[test]
fn cli_trace_history_lists_recorded_results_newest_first_without_a_shell() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    server.call(
        "handoff_trace_record",
        json!({ "project_dir": dir.to_string_lossy(), "results": [{"item": "AT-001", "result": "fail"}] }),
    );
    std::thread::sleep(Duration::from_millis(5));
    server.call(
        "handoff_trace_record",
        json!({ "project_dir": dir.to_string_lossy(), "results": [{"item": "AT-001", "result": "pass"}] }),
    );
    drop(server);

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "history",
        "--project-dir",
        dir_str,
        "--item",
        "AT-001",
        "--limit",
        "10",
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    let items = parsed["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{items:?}");
    assert_eq!(items[0]["result"], "pass", "newest first: {items:?}");
    assert_eq!(items[1]["result"], "fail");
}

/// A single-value array flag (`--expand ID`, `--gap-kinds KIND`, `--layers
/// L` with no comma) must still reach the handler as a one-element array —
/// previously it arrived as a bare string and the handler's `as_array()`
/// read silently dropped it (reviewer finding, t360.13).
#[test]
fn cli_trace_single_value_array_flags_are_not_silently_dropped() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    drop(server);

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "slice",
        "--project-dir",
        dir_str,
        "--item",
        "REQ-001",
        "--expand",
        "REQ-001",
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    let req = parsed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|it| it["id"] == "REQ-001")
        .expect("REQ-001 in slice");
    assert!(
        req.get("statement").is_some(),
        "--expand REQ-001 (single value) must expand REQ-001: {req}"
    );

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "report",
        "--project-dir",
        dir_str,
        "--layers",
        "requirement",
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        parsed["trace_layers"]["in_use"],
        json!(["requirement"]),
        "--layers requirement (single value) must override the layer set: {parsed}"
    );
}

/// A `layers` override shapes only the calling request's response; it must
/// never be persisted into `_trace_report.json`, whose `inputs` fingerprint
/// does not record the override and would otherwise make an ad-hoc layer
/// view look like a fresh canonical report (reviewer finding, t360.13).
#[test]
fn cli_trace_report_with_layers_override_does_not_persist_the_override() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    drop(server);

    let path = dir.join(".handoff/docs/_trace_report.json");
    let (stdout, stderr, code) = run_cli(&["trace", "report", "--project-dir", dir_str]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let canonical = std::fs::read(&path).unwrap();

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "report",
        "--project-dir",
        dir_str,
        "--layers",
        "requirement",
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["trace_layers"]["in_use"], json!(["requirement"]));

    assert_eq!(
        std::fs::read(&path).unwrap(),
        canonical,
        "a --layers override must leave _trace_report.json as the canonical view"
    );
}

#[test]
fn cli_trace_help_lists_all_four_actions() {
    let (stdout, _stderr, code) = run_cli(&["trace", "--help"]);
    assert_eq!(code, 0);
    for action in ["report", "record", "slice", "history"] {
        assert!(
            stdout.contains(action),
            "trace --help must list {action}: {stdout}"
        );
    }
}
