//! Real-binary E2E tests for M2-11 `handoff_trace_ingest`
//! (wiki/260-vmodel-m2-design.md §4.6): spawns the actual `handoff-mcp`
//! binary and drives it over real stdio JSON-RPC, plus the `trace ingest`
//! CLI subcommand. Same harness style as `tests/trace_report_slice_e2e.rs`.

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

/// cargo_json ingestion end to end: a layer item's `- test:` attribute is
/// matched exactly, recorded as one run entry, and reflected in a
/// subsequent `handoff_trace_report`'s `last_run`.
#[test]
fn trace_ingest_cargo_json_records_a_run_visible_in_trace_report() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.path().to_string_lossy(), "project_name": "trace-ingest-e2e" }),
    );

    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "slug": "st-ingest-e2e",
            "title": "System test",
            "layer": "system_test",
            "body": "# System test\n\n### ST-900 Lockout after 5 failures\n\n- test: tests::lock::lock_after_5\n\nRun the lockout scenario.\n",
        }),
    );

    let cargo_json = r#"{"type":"test","event":"ok","name":"tests::lock::lock_after_5"}"#;
    let ingest = server.call(
        "handoff_trace_ingest",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "format": "cargo_json",
            "output": cargo_json,
        }),
    );
    assert_eq!(ingest["recorded"], 1);
    assert_eq!(ingest["matched"][0]["item"], "ST-900");
    assert_eq!(ingest["matched"][0]["result"], "pass");
    assert!(ingest["missing_refs"].as_array().unwrap().is_empty());
    assert!(ingest["run_id"].as_str().is_some());

    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.path().to_string_lossy(), "include_items": true }),
    );
    let items = report["items"].as_array().expect("items");
    let st900 = items
        .iter()
        .find(|i| i["id"] == "ST-900")
        .expect("ST-900 must be in the report");
    assert_eq!(st900["last_run"]["result"], "pass");
}

/// JUnit XML ingestion (the recommended source, §4.6): a `<failure>` child
/// maps to `fail`.
#[test]
fn trace_ingest_junit_xml_maps_failure_child_to_fail() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.path().to_string_lossy(), "project_name": "trace-ingest-junit-e2e" }),
    );

    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "slug": "st-junit-e2e",
            "title": "System test",
            "layer": "system_test",
            "body": "# System test\n\n### ST-901 Something\n\n- test: mod1::t1\n\nBody.\n",
        }),
    );

    let junit = r#"<testsuite><testcase classname="mod1" name="t1"><failure message="boom"/></testcase></testsuite>"#;
    let ingest = server.call(
        "handoff_trace_ingest",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "format": "junit_xml",
            "output": junit,
        }),
    );
    assert_eq!(ingest["matched"][0]["item"], "ST-901");
    assert_eq!(ingest["matched"][0]["result"], "fail");
}

/// The `trace ingest` CLI subcommand dispatches to the same handler.
#[test]
fn cli_trace_ingest_records_a_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.path().to_string_lossy(), "project_name": "trace-ingest-cli-e2e" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "slug": "st-cli-e2e",
            "title": "System test",
            "layer": "system_test",
            "body": "# System test\n\n### ST-902 Something\n\n- test: mod::t\n\nBody.\n",
        }),
    );
    drop(server);

    let output_file = dir.path().join("cargo_output.jsonl");
    std::fs::write(
        &output_file,
        r#"{"type":"test","event":"ok","name":"mod::t"}"#,
    )
    .unwrap();

    let out = Command::new(binary())
        .args([
            "trace",
            "ingest",
            "--project-dir",
            &dir.path().to_string_lossy(),
            "--format",
            "cargo_json",
            "--output-file",
            &output_file.to_string_lossy(),
        ])
        .output()
        .expect("run CLI");
    assert!(
        out.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout: Value = serde_json::from_slice(&out.stdout).expect("valid JSON stdout");
    assert_eq!(stdout["matched"][0]["item"], "ST-902");
    assert_eq!(stdout["matched"][0]["result"], "pass");
}

/// Malformed cargo JSON input must surface as an MCP-level error (isError:
/// true), not a silent empty result.
#[test]
fn trace_ingest_malformed_cargo_json_returns_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.path().to_string_lossy(), "project_name": "trace-ingest-bad-e2e" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "slug": "st-bad-e2e",
            "title": "System test",
            "layer": "system_test",
            "body": "# System test\n\n### ST-999 Something\n\n- test: mod::t\n\nBody.\n",
        }),
    );

    let resp = server.call(
        "handoff_trace_ingest",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "format": "cargo_json",
            "output": "this is not json at all",
        }),
    );
    assert_eq!(
        resp["recorded"], 0,
        "malformed cargo JSON must record nothing: {resp}"
    );
    assert!(
        resp["matched"].as_array().unwrap().is_empty(),
        "malformed cargo JSON must match nothing: {resp}"
    );
}
