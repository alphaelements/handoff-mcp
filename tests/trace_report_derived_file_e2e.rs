//! Real-binary E2E tests for `.handoff/docs/_trace_report.json` (t360.13,
//! wiki/220-vmodel-integration-design.md §3.4/§4.3 r3): spawns the actual
//! `handoff-mcp` binary over stdio JSON-RPC (same harness as
//! `tests/trace_report_slice_e2e.rs`) and verifies the derived file's
//! content, its schema_version/inputs fingerprint, and the P-M4 write
//! discipline (unchanged content is never rewritten; an unrelated write that
//! moves the `inputs` fingerprint forces a rewrite even when the trace
//! content itself is unchanged).

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
        Self::spawn_inner(None)
    }

    fn spawn_with_derived_log(log_path: &std::path::Path) -> Self {
        Self::spawn_inner(Some(log_path))
    }

    fn spawn_inner(derived_log: Option<&std::path::Path>) -> Self {
        let mut cmd = Command::new(binary());
        if let Some(log_path) = derived_log {
            cmd.env("HANDOFF_MCP_DERIVED_WRITE_LOG", log_path);
        }
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

fn build_small_project(server: &mut Server, dir: &std::path::Path) {
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "trace-report-derived-file-e2e" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "requirements-derived-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-001 Account lockout\n\n- priority: P0\n\nAfter 5 failures the account locks.\n",
        }),
    );
}

fn trace_report_path(dir: &std::path::Path) -> PathBuf {
    dir.join(".handoff").join("docs").join("_trace_report.json")
}

fn read_trace_report(dir: &std::path::Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(trace_report_path(dir)).unwrap()).unwrap()
}

/// `handoff_trace_report` must write `_trace_report.json` with
/// `schema_version` + `inputs` alongside the same `trace_layers`/`coverage`/
/// `gaps`/`gap_counts`/`items` shape `include_items=true` returns, and the
/// file must be compact (no pretty-printing, matching
/// `_requirements_summary.json`'s own P-M4 discipline).
#[test]
fn trace_report_writes_trace_report_json_with_schema_version_and_inputs() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();

    let mut server = Server::spawn();
    build_small_project(&mut server, &dir);

    let response = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy() }),
    );

    let path = trace_report_path(&dir);
    assert!(path.exists(), "_trace_report.json must be written");
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(
        !raw.contains("\n  "),
        "_trace_report.json must be compact JSON, not pretty-printed: {raw}"
    );

    let persisted = read_trace_report(&dir);
    // M2-07 (wiki/260-vmodel-m2-design.md §5.1/§11 Q2): schema_version 2.
    assert_eq!(persisted["schema_version"], 2);
    let inputs = &persisted["inputs"];
    assert!(inputs["docs_count"].as_u64().unwrap() >= 1);
    assert!(inputs.get("tasks_max_mtime_ns").is_some());
    assert!(inputs.get("runs_count").is_some());

    // Content matches trace_report(include_items=true)'s own response shape
    // (minus schema_version/inputs), and unlike the calling request's own
    // response (which had no `include_items` argument here), the persisted
    // file always carries `items[]`.
    assert_eq!(persisted["trace_layers"], response["trace_layers"]);
    assert_eq!(persisted["coverage"], response["coverage"]);
    let items = persisted["items"].as_array().expect("items array present");
    assert!(
        items.iter().any(|it| it["id"] == "REQ-001"),
        "persisted items[] must include REQ-001: {items:?}"
    );
}

/// P-M4 (wiki/240 §4): a repeat `handoff_trace_report` call with nothing
/// changed since the last one must not rewrite `_trace_report.json` again.
#[test]
fn trace_report_does_not_rewrite_trace_report_json_when_nothing_changed() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let log_path = tmp.path().join("derived_writes.log");

    let mut server = Server::spawn_with_derived_log(&log_path);
    build_small_project(&mut server, &dir);

    server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy() }),
    );
    let after_first = trace_report_write_count(&log_path);
    assert!(after_first >= 1, "the first call must write the file");

    server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy() }),
    );
    let after_second = trace_report_write_count(&log_path);
    assert_eq!(
        after_second, after_first,
        "a repeat call with nothing changed must not rewrite _trace_report.json again"
    );
}

/// wiki/220 §4.3 r3: saving a document unrelated to the trace graph's
/// content still moves the `docs_*` half of the `inputs` fingerprint (more
/// `_doc.*.md` files, a newer max mtime) — the next `handoff_trace_report`
/// call must rewrite `_trace_report.json` to record the new fingerprint,
/// even though the trace-derivation content (trace_layers/coverage/gaps/
/// items) itself hasn't changed at all.
#[test]
fn trace_report_rewrites_when_an_unrelated_doc_save_moves_the_inputs_fingerprint() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let log_path = tmp.path().join("derived_writes.log");

    let mut server = Server::spawn_with_derived_log(&log_path);
    build_small_project(&mut server, &dir);

    server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy() }),
    );
    let first = read_trace_report(&dir);
    let after_first = trace_report_write_count(&log_path);

    // A plain, non-layer document — entirely unrelated to the trace graph's
    // own content — still counts toward `docs_count`/`docs_max_mtime_ns`.
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "unrelated-note",
            "title": "Unrelated note",
            "body": "Nothing to do with the trace graph.",
        }),
    );

    server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy() }),
    );
    let second = read_trace_report(&dir);
    let after_second = trace_report_write_count(&log_path);

    assert!(
        after_second > after_first,
        "an unrelated doc save must force a rewrite (inputs fingerprint moved): \
         after_first={after_first} after_second={after_second}"
    );
    assert_ne!(
        first["inputs"], second["inputs"],
        "the inputs fingerprint must change once an unrelated document is saved"
    );
    assert_eq!(
        first["trace_layers"], second["trace_layers"],
        "the trace-derivation content itself must be unchanged"
    );
    assert_eq!(first["gaps"], second["gaps"]);
}

fn trace_report_write_count(log_path: &std::path::Path) -> usize {
    std::fs::read_to_string(log_path)
        .map(|s| {
            s.lines()
                .filter(|line| line.contains("_trace_report.json"))
                .count()
        })
        .unwrap_or(0)
}
