//! Real-binary E2E for `handoff_trace_matrix` / CLI `trace matrix`
//! (wiki/260-vmodel-m2-design.md §4.4/§5.3, t360.20.9/M2-09): tree/edges
//! shapes, CSV/Markdown rendering, `layers`/`root_layer` filtering,
//! `output_file` (write + path-escape rejection), and the E6 "never writes
//! to `.handoff/`" contract (same pattern `tests/cli_trace.rs`'s
//! `cli_trace_lint_never_writes_to_handoff` and `tests/trace_lint_e2e.rs`
//! use).

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

/// REQ-001 (requirement) <-refines- SPEC-001 (basic_spec) <-verifies- ST-001
/// (system_test); REQ-001 <-verifies- AT-001 (acceptance); task t1 implements
/// REQ-001 — a small but non-trivial V-shape spanning 4 layers.
fn build_project(server: &mut Server, dir: &std::path::Path) {
    let pd = dir.to_string_lossy().to_string();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "trace-matrix-e2e" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "requirements-matrix-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-001 Account lockout\n\n- priority: P0\n\nAfter 5 failures the account locks.\n",
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "spec-matrix-e2e",
            "title": "Basic spec",
            "layer": "basic_spec",
            "body": "# Basic spec\n\n### SPEC-001 Lockout counter\n\n- refines: REQ-001\n\nTrack failed attempts.\n",
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "acceptance-matrix-e2e",
            "title": "Acceptance tests",
            "layer": "acceptance",
            "body": "# Acceptance\n\n### AT-001 Lockout after 5 failures\n\n- verifies: REQ-001\n- method: manual\n\nFail login 5 times, then confirm the account is locked.\n",
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "system-test-matrix-e2e",
            "title": "System tests",
            "layer": "system_test",
            "body": "# System tests\n\n### ST-001 Counter increments\n\n- verifies: SPEC-001\n- method: manual\n\nVerify the counter increments on failure.\n",
        }),
    );
    server.call(
        "handoff_update_task",
        json!({
            "project_dir": pd,
            "task": { "id": "t1", "title": "Implement lockout", "requirement_ids": ["REQ-001"] },
        }),
    );
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

// ---------------------------------------------------------------------
// shape=tree (default), format=csv
// ---------------------------------------------------------------------

#[test]
fn tree_csv_default_root_layer_is_the_shallowest_in_use_left_layer_with_all_four_layer_columns() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);

    let resp = server.call(
        "handoff_trace_matrix",
        json!({ "project_dir": pd, "format": "csv" }),
    );
    assert_eq!(resp["format"], "csv");
    assert_eq!(resp["shape"], "tree");
    assert_eq!(resp["root_layer"], "requirement");
    assert_eq!(
        resp["columns"],
        json!([
            "requirement",
            "basic_spec",
            "acceptance",
            "system_test",
            "tasks",
            "state",
            "suspect"
        ])
    );
    assert_eq!(resp["rows"], 1);

    let content = resp["content"].as_str().expect("content must be a string");
    assert!(content.starts_with(
        "\"requirement\",\"basic_spec\",\"acceptance\",\"system_test\",\"tasks\",\"state\",\"suspect\"\n"
    ));
    assert!(content.contains("\"REQ-001\""));
    assert!(content.contains("\"SPEC-001\""));
    assert!(content.contains("\"AT-001\""));
    assert!(content.contains("\"ST-001\""));
    assert!(content.contains("\"t1\""));
    assert!(!content.starts_with('\u{feff}'), "no BOM");
    assert!(!content.contains('\r'), "LF only");
}

#[test]
fn tree_markdown_columns_narrowed_by_layers_argument() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);

    let resp = server.call(
        "handoff_trace_matrix",
        json!({
            "project_dir": pd,
            "format": "markdown",
            "layers": ["requirement", "acceptance"],
            "include_tasks": false,
        }),
    );
    assert_eq!(
        resp["columns"],
        json!(["requirement", "acceptance", "state", "suspect"])
    );
    let content = resp["content"].as_str().unwrap();
    assert!(content.starts_with("| requirement | acceptance | state | suspect |\n"));
    assert!(content.contains("AT-001"));
    assert!(!content.contains("SPEC-001"), "{content}");
}

#[test]
fn explicit_root_layer_overrides_the_default() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);

    let resp = server.call(
        "handoff_trace_matrix",
        json!({ "project_dir": pd, "format": "csv", "root_layer": "basic_spec" }),
    );
    assert_eq!(resp["root_layer"], "basic_spec");
    assert_eq!(resp["rows"], 1);
    let content = resp["content"].as_str().unwrap();
    assert!(content.contains("\"SPEC-001\""));
}

#[test]
fn unknown_root_layer_is_an_error() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);

    let (is_error, text) = server.call_raw(
        "handoff_trace_matrix",
        json!({ "project_dir": pd, "format": "csv", "root_layer": "not-a-layer" }),
    );
    assert!(is_error, "{text}");
    assert!(text.contains("not-a-layer"), "{text}");
}

// ---------------------------------------------------------------------
// shape=edges
// ---------------------------------------------------------------------

#[test]
fn edges_shape_one_row_per_resolved_link_with_from_to_and_layers() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);

    let resp = server.call(
        "handoff_trace_matrix",
        json!({ "project_dir": pd, "format": "csv", "shape": "edges" }),
    );
    assert_eq!(resp["shape"], "edges");
    assert!(resp.get("root_layer").is_none(), "{resp}");
    assert_eq!(
        resp["columns"],
        json!([
            "from",
            "to",
            "link_type",
            "from_layer",
            "to_layer",
            "state",
            "suspect"
        ])
    );
    // SPEC-001 refines REQ-001, AT-001 verifies REQ-001, ST-001 verifies SPEC-001.
    assert_eq!(resp["rows"], 3);
    let content = resp["content"].as_str().unwrap();
    assert!(content.contains("\"SPEC-001\",\"REQ-001\",\"refines\""));
    assert!(content.contains("\"AT-001\",\"REQ-001\",\"verifies\""));
    assert!(content.contains("\"ST-001\",\"SPEC-001\",\"verifies\""));
}

// ---------------------------------------------------------------------
// output_file
// ---------------------------------------------------------------------

#[test]
fn output_file_writes_under_the_project_dir_and_omits_inline_content() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);

    let resp = server.call(
        "handoff_trace_matrix",
        json!({
            "project_dir": pd,
            "format": "csv",
            "output_file": "out/matrix.csv",
        }),
    );
    assert_eq!(resp["output_file"], "out/matrix.csv");
    assert!(resp.get("content").is_none(), "{resp}");

    let written = std::fs::read_to_string(dir.join("out/matrix.csv")).unwrap();
    assert!(written.contains("\"REQ-001\""));
}

#[test]
fn output_file_rejects_a_path_that_escapes_the_project_dir() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);

    let (is_error, text) = server.call_raw(
        "handoff_trace_matrix",
        json!({ "project_dir": pd, "format": "csv", "output_file": "../escape.csv" }),
    );
    assert!(is_error, "{text}");
    assert!(!tmp.path().join("escape.csv").exists());
}

#[test]
fn output_file_rejects_an_absolute_path() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);

    let (is_error, text) = server.call_raw(
        "handoff_trace_matrix",
        json!({ "project_dir": pd, "format": "csv", "output_file": "/tmp/absolute-escape.csv" }),
    );
    assert!(is_error, "{text}");
}

// ---------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------

#[test]
fn cli_trace_matrix_prints_json_with_csv_content_to_stdout() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    drop(server);

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "matrix",
        "--project-dir",
        dir_str,
        "--format",
        "csv",
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("`trace matrix` must print JSON: {e}: {stdout}"));
    assert_eq!(parsed["format"], "csv");
    assert!(parsed["content"].as_str().unwrap().contains("REQ-001"));
}

#[test]
fn cli_trace_matrix_output_flag_maps_to_output_file() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    drop(server);

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "matrix",
        "--project-dir",
        dir_str,
        "--format",
        "csv",
        "--output",
        "matrix-out.csv",
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["output_file"], "matrix-out.csv");
    assert!(dir.join("matrix-out.csv").exists());
}

/// M2-09 (wiki/260 §4.4's E6 contract): `trace matrix` must never write any
/// byte under `.handoff/` — its only allowed write is `output_file`, which
/// lives under the project directory, not `.handoff/`.
#[test]
fn cli_trace_matrix_never_writes_to_handoff() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();
    let handoff = dir.join(".handoff");

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    drop(server);

    let before = snapshot(&handoff);
    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "matrix",
        "--project-dir",
        dir_str,
        "--format",
        "markdown",
        "--shape",
        "edges",
        "--output",
        "edges.md",
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let after = snapshot(&handoff);
    assert_eq!(
        before, after,
        "trace matrix must never write any byte under .handoff/: stdout={stdout} stderr={stderr}"
    );
    assert!(dir.join("edges.md").exists());
}
