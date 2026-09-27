//! M0/M1 backward-compat E2E test (t360.12, wiki/220-vmodel-integration-design.md
//! §5/§7, NFR-001/002): spawns the actual **current** `handoff-mcp` binary
//! against a project frozen by the real `main`-branch binary (see
//! `tests/fixtures/pre_m1_compat/README.md` for exactly how it was produced
//! and what is deliberately excluded from this comparison) and asserts that
//! `handoff_doc_req_list`/`handoff_doc_req_status`/`handoff_doc_verify_status`
//! reproduce `expected_output.json` byte-for-byte over the three pre-existing
//! shapes M0/M1 must not disturb: a layer-less document, a `C{n}`-prefixed
//! section-attached SubItem, and a freeform (`fragment_seq: null`) SubItem.
//!
//! This test only calls **read** tools and never re-runs
//! `handoff_doc_save`/`handoff_doc_req_import`/`handoff_doc_verify` mutating
//! actions against the fixture — see the README for why (M0's FR-806 fix
//! intentionally changed what a *fresh* `req_import` produces; this test
//! guards reading an *already-frozen* pre-M0/M1 project, not re-writing one).

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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pre_m1_compat")
}

/// Copies `src` to `dst` recursively (`std::fs` has no built-in equivalent) —
/// same technique as `tests/trace_report_contract_fixture_e2e.rs`.
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
/// handoff/`, named without a leading dot so the repo's blanket `.handoff/`
/// `.gitignore` rule doesn't swallow it — copied to `<tmp>/proj/.handoff`,
/// the real directory name every handoff-mcp command expects). The doc ids
/// baked into the copied `.md` frontmatter (and therefore every read tool's
/// output) never change, since this test never re-creates the documents.
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

const DOC_A_ID: &str = "doc-20260927-045154-945488"; // req-c01-legacy-spec
const DOC_B_ID: &str = "doc-20260927-045154-958010"; // req-c02-board-setup

struct Server {
    child: Child,
    stdin: std::process::ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
}

impl Server {
    fn spawn() -> Self {
        let mut child = Command::new(binary())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn handoff-mcp server");
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

#[test]
fn req_list_req_status_and_doc_verify_status_match_pre_m0_m1_main_output() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let proj = setup_project(tmp.path());

    let mut server = Server::spawn();

    let req_list = server.call(
        "handoff_doc_req_list",
        serde_json::json!({ "project_dir": proj.to_string_lossy() }),
    );
    let req_status = server.call(
        "handoff_doc_req_status",
        serde_json::json!({ "project_dir": proj.to_string_lossy() }),
    );
    let verify_status_a = server.call(
        "handoff_doc_verify_status",
        serde_json::json!({
            "project_dir": proj.to_string_lossy(), "doc_id": DOC_A_ID, "include_items": true,
        }),
    );
    let verify_status_b = server.call(
        "handoff_doc_verify_status",
        serde_json::json!({
            "project_dir": proj.to_string_lossy(), "doc_id": DOC_B_ID, "include_items": true,
        }),
    );

    let expected = expected_output();

    assert_eq!(
        req_list, expected["req_list"],
        "handoff_doc_req_list output must be byte-for-byte unchanged for a \
         layer-less doc's C{{n}} SubItem and a freeform SubItem"
    );
    assert_eq!(
        req_status, expected["req_status"],
        "handoff_doc_req_status output must be byte-for-byte unchanged"
    );
    assert_eq!(
        verify_status_a, expected["verify_status_a"],
        "handoff_doc_verify_status must be byte-for-byte unchanged for the \
         layer-less document with a section-attached C{{n}} SubItem"
    );
    assert_eq!(
        verify_status_b, expected["verify_status_b"],
        "handoff_doc_verify_status must be byte-for-byte unchanged for the \
         document whose SubItems live inside a freeform (fragment_seq: null) item"
    );
}
