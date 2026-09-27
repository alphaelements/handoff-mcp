//! Contract-fixture E2E test for `.handoff/docs/_trace_report.json` (t360.13,
//! wiki/220-vmodel-integration-design.md §3.4, NFR-005): `tests/fixtures/trace/`
//! is the shared contract between handoff-mcp (Rust, this test) and
//! handoff-vscode (TypeScript) — see `tests/fixtures/trace/README.md`. This
//! test copies the committed fixture project into a tempdir, runs the real
//! `handoff-mcp` binary against it (both the CLI `trace report` subcommand
//! and the `handoff_trace_report` MCP tool over stdio), and asserts the
//! output matches `tests/fixtures/trace/expected_output.json` — the same
//! byte-for-byte content contract handoff-vscode's TS reader must honor.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::Value;

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

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/trace")
}

/// Copies `src` to `dst` recursively (`std::fs` has no built-in equivalent).
fn copy_dir_recursive(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let file_type = entry.file_type().unwrap();
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path);
        } else {
            std::fs::copy(entry.path(), &dst_path).unwrap();
        }
    }
}

/// Sets up a fresh project directory from the committed fixture (`project/
/// handoff/` — named without a leading dot so the repo's blanket `.handoff/`
/// `.gitignore` rule doesn't swallow it — copied to `<tmp>/proj/.handoff`, the
/// real directory name every handoff-mcp command expects).
fn setup_project(tmp: &Path) -> PathBuf {
    let proj = tmp.join("proj");
    copy_dir_recursive(
        &fixture_dir().join("project/handoff"),
        &proj.join(".handoff"),
    );
    proj
}

fn expected_output() -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(fixture_dir().join("expected_output.json")).unwrap(),
    )
    .unwrap()
}

/// Zeroes out the two mtime-derived `inputs` fields that necessarily differ
/// across a fresh copy of the fixture (the copy's on-disk mtimes are "now",
/// not the fixture-generation timestamp baked into `expected_output.json`) —
/// every other `inputs` field (`docs_count`/`tasks_count`/`runs_count`/
/// `runs_max_id`, all structural counts/names fixed by the fixture's file
/// set, not by copy time) and `schema_version` are compared exactly.
fn normalize_inputs_mtimes(mut value: Value) -> Value {
    if let Some(inputs) = value.get_mut("inputs").and_then(|v| v.as_object_mut()) {
        inputs.insert("docs_max_mtime_ns".to_string(), serde_json::json!(0));
        inputs.insert("tasks_max_mtime_ns".to_string(), serde_json::json!(0));
    }
    value
}

#[test]
fn cli_trace_report_output_matches_the_committed_contract_fixture() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let proj = setup_project(tmp.path());

    let output = Command::new(binary())
        .args(["trace", "report", "--project-dir", proj.to_str().unwrap()])
        .output()
        .expect("failed to run binary");
    assert!(
        output.status.success(),
        "trace report failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let persisted: Value = serde_json::from_str(
        &std::fs::read_to_string(proj.join(".handoff/docs/_trace_report.json")).unwrap(),
    )
    .unwrap();

    assert_eq!(
        normalize_inputs_mtimes(persisted),
        normalize_inputs_mtimes(expected_output()),
        "_trace_report.json must match tests/fixtures/trace/expected_output.json (mtime fields normalized)"
    );
}

/// Same fixture, driven over the `handoff_trace_report` MCP tool (stdio
/// JSON-RPC) instead of the CLI — both entry points call the same handler,
/// so both must produce byte-identical `_trace_report.json` content.
#[test]
fn mcp_trace_report_output_matches_the_committed_contract_fixture() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let proj = setup_project(tmp.path());

    let mut server = Server::spawn();
    server.call(
        "handoff_trace_report",
        serde_json::json!({ "project_dir": proj.to_string_lossy() }),
    );

    let persisted: Value = serde_json::from_str(
        &std::fs::read_to_string(proj.join(".handoff/docs/_trace_report.json")).unwrap(),
    )
    .unwrap();

    assert_eq!(
        normalize_inputs_mtimes(persisted),
        normalize_inputs_mtimes(expected_output()),
        "_trace_report.json must match the fixture regardless of entry point (CLI vs MCP tool)"
    );
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
        let req = serde_json::json!({
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
