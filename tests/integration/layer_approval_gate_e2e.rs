//! Real-binary MCP-tool E2E for the layer approval gate (t-vfix.1):
//!
//! 1. `handoff_doc_verify(check / check_all)` on a layer document must not
//!    approve any item (the E12 read-mapping would turn `status: verified`
//!    into `approved`).
//! 2. `handoff_doc_verify(sync)` goes through the shared layer sync, so a
//!    direct `.md` edit that changes an approved item's definition resets it
//!    to `draft`.
//! 3. `handoff_trace_update(set approval=approved)` after a direct `.md` edit
//!    records the *fresh* definition hash as `approved_hash`.
//!
//! Approval is observed through `handoff_trace_report(include_items)`, the
//! same read model every consumer uses.

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

fn approval_of(server: &mut Server, pd: &str, id: &str) -> (String, Value) {
    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": pd, "include_items": true }),
    );
    let item = report["items"]
        .as_array()
        .expect("items array")
        .iter()
        .find(|i| i["id"] == id)
        .unwrap_or_else(|| panic!("{id} missing from report: {report}"))
        .clone();
    (item["approval"].as_str().unwrap().to_string(), item)
}

fn setup(server: &mut Server, name: &str, body: &str) -> (tempfile::TempDir, String, String) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": name }),
    );
    let saved = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "spec-gate-e2e",
            "title": "Basic spec",
            "layer": "basic_spec",
            "body": body,
        }),
    );
    let doc_id = saved["doc_id"].as_str().unwrap().to_string();
    (tmp, pd, doc_id)
}

fn body_path(pd: &str) -> PathBuf {
    PathBuf::from(pd).join(".handoff/docs/_doc.spec-gate-e2e.md")
}

/// Rewrites the document's Markdown body in place, keeping the frontmatter.
fn replace_in_body_file(pd: &str, from: &str, to: &str) {
    let path = body_path(pd);
    let text = std::fs::read_to_string(&path).expect("read doc file");
    assert!(text.contains(from), "{from:?} not found in {path:?}");
    std::fs::write(&path, text.replace(from, to)).expect("write doc file");
}

#[test]
fn check_and_check_all_do_not_approve_layer_items() {
    let mut server = Server::spawn();
    let (_tmp, pd, doc_id) = setup(
        &mut server,
        "gate-check",
        "# Basic spec\n\n### SPEC-001 Lockout\n\nBody.\n",
    );
    assert_eq!(approval_of(&mut server, &pd, "SPEC-001").0, "draft");

    server.call(
        "handoff_doc_verify",
        json!({ "project_dir": pd, "doc_id": doc_id, "action": "check", "sub_item_id": "SPEC-001" }),
    );
    assert_eq!(approval_of(&mut server, &pd, "SPEC-001").0, "draft");

    server.call(
        "handoff_doc_verify",
        json!({ "project_dir": pd, "doc_id": doc_id, "action": "check_all", "reviewer": "ai" }),
    );
    assert_eq!(
        approval_of(&mut server, &pd, "SPEC-001").0,
        "draft",
        "check_all must not approve a layer item"
    );
}

#[test]
fn sync_resets_approval_when_definition_changed_by_direct_edit() {
    let mut server = Server::spawn();
    let (_tmp, pd, doc_id) = setup(
        &mut server,
        "gate-sync",
        "# Basic spec\n\n### SPEC-001 Lockout\n\nBody v1.\n\n### SPEC-002 Other\n\nStable.\n",
    );
    let out = server.call(
        "handoff_trace_update",
        json!({ "ops": [
            { "op": "set", "item": "SPEC-001", "approval": "approved" },
            { "op": "set", "item": "SPEC-002", "approval": "approved" },
        ], "project_dir": pd, "executor_kind": "human", "executor_id": "ryoma" }),
    );
    assert!(out.get("failed").is_none(), "{out}");
    assert_eq!(approval_of(&mut server, &pd, "SPEC-001").0, "approved");

    replace_in_body_file(&pd, "Body v1.", "Body v2, edited directly.");
    server.call(
        "handoff_doc_verify",
        json!({ "project_dir": pd, "doc_id": doc_id, "action": "sync" }),
    );
    assert_eq!(
        approval_of(&mut server, &pd, "SPEC-001").0,
        "draft",
        "definition changed -> approval must fall back to draft"
    );
    assert_eq!(
        approval_of(&mut server, &pd, "SPEC-002").0,
        "approved",
        "unchanged item keeps its approval"
    );
}

#[test]
fn approving_after_direct_edit_records_the_fresh_hash() {
    let mut server = Server::spawn();
    let (_tmp, pd, _doc_id) = setup(
        &mut server,
        "gate-hash",
        "# Basic spec\n\n### SPEC-001 Lockout\n\nBody v1.\n",
    );
    let (_, before) = approval_of(&mut server, &pd, "SPEC-001");
    let stale = before["def_hash"].clone();

    replace_in_body_file(&pd, "Body v1.", "Body v2, edited directly.");
    let out = server.call(
        "handoff_trace_update",
        json!({ "ops": [{ "op": "set", "item": "SPEC-001", "approval": "approved" }],
            "project_dir": pd, "executor_kind": "human", "executor_id": "ryoma" }),
    );
    assert!(out.get("failed").is_none(), "{out}");

    let (approval, after) = approval_of(&mut server, &pd, "SPEC-001");
    assert_eq!(approval, "approved");
    assert_ne!(
        after["def_hash"], stale,
        "def_hash must reflect the edited body"
    );

    // The audit record carries the same fresh hash that was approved.
    let dir = PathBuf::from(&pd).join(".handoff/trace/approvals");
    let mut seen = false;
    for entry in std::fs::read_dir(&dir).expect("approvals dir") {
        let rec: Value =
            serde_json::from_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap()).unwrap();
        assert_eq!(rec["items"][0]["def_hash"], after["def_hash"], "{rec}");
        seen = true;
    }
    assert!(seen, "an approval audit record must exist");
}
