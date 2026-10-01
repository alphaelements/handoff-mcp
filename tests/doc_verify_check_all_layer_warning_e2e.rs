//! Real-binary MCP-tool E2E for `handoff_doc_verify`'s layer-document warning
//! on `check`/`check_all` (wiki/260-vmodel-m2-design.md §3.3, M2-13 round 3
//! MAJOR finding, t360.20.33 M2-S9 tester/reviewer): on a layer document, M2
//! aggregation reads `approval` from `SubItem.status`, never from the legacy
//! per-`VerificationItem` `status` that `check`/`check_all` mutate — so those
//! two actions keep working (back-compat) but must warn the caller their
//! effect is invisible to the trace model. `src/mcp/handlers/docs.rs` already
//! covers this in-process (`doc_verify_check_all_on_layer_doc_warns_not_used_
//! for_aggregation`); this file is the real-binary stdio JSON-RPC
//! counterpart — the same transport handoff-vscode's caller and `handoff-mcp`
//! CLI consumers actually use — and additionally confirms the *absence* of
//! the warning on a plain (non-layer) document, which the in-process test
//! doesn't check at all.

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

const NOT_USED_FOR_AGGREGATION: &str = "does not feed layer aggregation";

/// A layer document's `verification` matrix is auto-populated by the layer
/// body sync on save (no explicit `generate` needed, wiki/260 §2.5) — so
/// `check_all` can run straight away, and must warn.
#[test]
fn doc_verify_check_all_on_a_layer_document_warns_not_used_for_aggregation() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "doc-verify-check-all-layer-e2e" }),
    );
    let saved = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "spec-check-all-layer-e2e",
            "title": "Basic spec",
            "layer": "basic_spec",
            "body": "# Basic spec\n\n### SPEC-001 Lockout\n\nBody.\n",
        }),
    );
    let doc_id = saved["doc_id"].as_str().unwrap().to_string();

    let result = server.call(
        "handoff_doc_verify",
        json!({ "project_dir": pd, "doc_id": doc_id, "action": "check_all" }),
    );
    let warnings = result["warnings"].as_array().expect("warnings array");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap_or("").contains(NOT_USED_FOR_AGGREGATION)),
        "doc_verify(check_all) on a layer document must warn it is not used for \
         aggregation: {result}"
    );
}

/// The same action on a plain (non-layer) document — where `check_all`
/// mutating `VerificationItem.status` *is* the only aggregation mechanism —
/// must never emit the layer-only warning.
#[test]
fn doc_verify_check_all_on_a_non_layer_document_does_not_warn() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "doc-verify-check-all-non-layer-e2e" }),
    );
    let saved = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "spec-check-all-non-layer-e2e",
            "title": "Plain spec",
            "doc_type": "spec",
            "body": "Intro.\n\n## Section A\n\nBody A.\n",
        }),
    );
    let doc_id = saved["doc_id"].as_str().unwrap().to_string();

    // Non-layer documents don't auto-populate a matrix — `generate` first
    // (check_all errors "if no matrix exists yet").
    server.call(
        "handoff_doc_verify",
        json!({ "project_dir": pd, "doc_id": doc_id, "action": "generate" }),
    );

    let result = server.call(
        "handoff_doc_verify",
        json!({ "project_dir": pd, "doc_id": doc_id, "action": "check_all" }),
    );
    let warnings = result["warnings"].as_array().expect("warnings array");
    assert!(
        !warnings
            .iter()
            .any(|w| w.as_str().unwrap_or("").contains(NOT_USED_FOR_AGGREGATION)),
        "doc_verify(check_all) on a non-layer document must not warn about layer \
         aggregation: {result}"
    );
}
