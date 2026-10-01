//! Real-binary E2E for t360.20.29 (bug, M2-S6 reviewer finding): a new
//! upstream item added to a layer document by a *direct body edit* (not a
//! never-synced document from scratch like t360.20.28 — this one has already
//! been through `handoff_doc_save` once, and is then hand-edited again
//! without going back through it) must still resolve as a cross-document
//! baseline owner, both through the single-document `handoff_doc_save` path
//! (`sync_layer_items_if_needed_reporting`, `src/mcp/handlers/docs.rs`) and
//! through `handoff_trace_record`'s own per-document resync loop
//! (`src/mcp/handlers/trace.rs`). Pre-fix, `resolve_upstream_ref_across_corpus`
//! decided ownership from the candidate document's *stored* `verification`
//! (stale — it predates the hand edit), so a downstream reference to the
//! newly-added item was left unbaselined forever even though the owning
//! document's current body plainly contains it. Same harness style as
//! `tests/trace_batch_resync_cross_doc_baseline_e2e.rs` (t360.20.28).

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::{json, Value};

use handoff_mcp::storage::docs::write_doc_body;

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

/// `handoff_doc_save`'s own single-document sync path resolving a reference
/// it discovers in *its own* body against an upstream item a sibling
/// document just gained via direct edit (not yet resynced since).
#[test]
fn doc_save_resolves_baseline_against_an_upstream_item_added_by_a_direct_edit_of_an_already_synced_doc(
) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let handoff = dir.join(".handoff");
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "t360-20-29-doc-save-e2e" }),
    );

    // req-doc is synced once via doc_save with only REQ-500.
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "req-doc-20-29",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-500 Session timeout\n\n本文。\n",
        }),
    );

    // Direct body edit (bypassing doc_save entirely): REQ-500's document
    // gains REQ-501 — its stored `verification` still predates this edit.
    write_doc_body(
        &handoff,
        "req-doc-20-29",
        "# Requirements\n\n### REQ-500 Session timeout\n\n本文。\n\n\
         ### REQ-501 New requirement\n\n本文。\n",
    )
    .expect("write_doc_body");

    // A fresh spec document, synced through doc_save, is the first thing to
    // ever discover `refines: REQ-501` — its own sync call must resolve the
    // baseline against req-doc's *current* (edited, unsynced) body.
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "spec-doc-20-29",
            "title": "Basic spec",
            "layer": "basic_spec",
            "body": "# Basic spec\n\n### SPEC-500 Timeout enforcement\n\n- refines: REQ-501\n\n本文。\n",
        }),
    );

    // `handoff_trace_suspect(list)` itself cannot distinguish this from a
    // dangling reference (both read as "not a suspect" — `trace::suspect`'s
    // own doc comment: a reference whose upstream can't be resolved at all
    // is reported as a `dangling` gap elsewhere, never as `unbaselined`,
    // since `TraceInput.items` is built from each document's *stored*
    // verification, which legitimately doesn't yet contain REQ-501 either).
    // Read SPEC-500's own stored `link_baselines` directly off disk instead —
    // same technique this task's design doc notes use for fields a tool
    // response doesn't surface (wiki/260 §2.5 M2-02 implementation memo).
    let spec_doc_raw =
        std::fs::read_to_string(handoff.join("docs").join("_doc.spec-doc-20-29.md")).unwrap();
    assert!(
        spec_doc_raw.contains("REQ-501:"),
        "SPEC-500's refines: REQ-501 must get a recorded link_baselines entry from doc_save's \
         own sync, even though req-doc had not been resynced since REQ-501 was added by a \
         direct body edit — got:\n{spec_doc_raw}"
    );
}

/// `handoff_trace_record`'s own per-document resync loop resolving the same
/// kind of cross-document reference, discovered as part of resyncing the
/// document that owns the item being recorded.
#[test]
fn trace_record_resolves_baseline_against_an_upstream_item_added_by_a_direct_edit_of_an_already_synced_doc(
) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let handoff = dir.join(".handoff");
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "t360-20-29-trace-record-e2e" }),
    );

    // req-doc synced once with only REQ-600.
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "req-doc-tr-20-29",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-600 Session timeout\n\n本文。\n",
        }),
    );
    // st-doc synced once with ST-600 carrying no verifies yet, so
    // trace_record's "does this known document own the recorded item"
    // pre-check finds it.
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "st-doc-tr-20-29",
            "title": "System test",
            "layer": "system_test",
            "body": "# System test\n\n### ST-600 Confirm timeout\n\n手順。\n",
        }),
    );

    // Direct body edits on both documents, bypassing doc_save/sync entirely:
    // req-doc gains REQ-601, st-doc gains `verifies: REQ-601` on ST-600.
    write_doc_body(
        &handoff,
        "req-doc-tr-20-29",
        "# Requirements\n\n### REQ-600 Session timeout\n\n本文。\n\n\
         ### REQ-601 New requirement\n\n本文。\n",
    )
    .expect("write_doc_body (req)");
    write_doc_body(
        &handoff,
        "st-doc-tr-20-29",
        "# System test\n\n### ST-600 Confirm timeout\n\n- verifies: REQ-601\n\n手順。\n",
    )
    .expect("write_doc_body (st)");

    server.call(
        "handoff_trace_record",
        json!({
            "project_dir": pd,
            "results": [{"item": "ST-600", "result": "pass"}],
        }),
    );

    let st_doc_raw =
        std::fs::read_to_string(handoff.join("docs").join("_doc.st-doc-tr-20-29.md")).unwrap();
    assert!(
        st_doc_raw.contains("REQ-601:"),
        "ST-600's verifies: REQ-601 must get a recorded link_baselines entry from \
         trace_record's own resync, even though req-doc had not been resynced since REQ-601 \
         was added by a direct body edit — got:\n{st_doc_raw}"
    );
}
