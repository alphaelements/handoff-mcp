//! Real-binary E2E test for M2-18 (wiki/260-vmodel-m2-design.md §4.12/§12,
//! FR-804, E11): spawns the actual `handoff-mcp` binary and drives it over
//! real stdio JSON-RPC — same harness style as `tests/trace_ingest_e2e.rs`.
//!
//! Reproduces the exact real-world aelm shape (a `scope_paths:` key
//! immediately followed by a lone `[]` line at the same indentation — 9 of
//! 209 real documents have this) end to end: `handoff_doc_list` must report
//! it in `unreadable` instead of silently dropping it, and
//! `handoff_doc_repair_frontmatter` must fix it and return the document to
//! the normal listing, driven entirely through the real process boundary
//! (not `process_line` in-process).

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

#[test]
fn aelm_scope_paths_shape_reported_unreadable_then_repaired_via_real_binary() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.path().to_string_lossy(), "project_name": "frontmatter-repair-e2e" }),
    );

    let handoff = dir.path().join(".handoff");
    std::fs::create_dir_all(handoff.join("docs")).expect("docs dir");

    // The exact aelm shape (wiki/260 §4.12/E11): `scope_paths:` immediately
    // followed by a lone `[]` line at the same (zero) indentation.
    let bad_path = handoff.join("docs").join("_doc.basic-design.md");
    std::fs::write(
        &bad_path,
        "---\nid: doc-basic-design\ntitle: \"Basic Design\"\ndoc_type: spec\ntags:\n\
         - specification\nscope_paths:\n[]\nparent_id: null\nchildren: []\n\
         related: []\nauto_inject: auto\ntask_ids: []\nsource:\n  origin: authored\n\
         has_bom: false\nline_ending: lf\nsplit_level: 2\n\
         created_at: 2026-07-29T04:54:59Z\nupdated_at: 2026-07-29T04:54:59Z\n\
         content_hash: 8ac9368e71738ec2\n---\n# Basic Design\n\nBody.\n",
    )
    .expect("write malformed frontmatter doc");

    // 1. handoff_doc_list must not silently drop it.
    let listed = server.call(
        "handoff_doc_list",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    assert_eq!(listed["documents"].as_array().unwrap().len(), 0);
    let unreadable = listed["unreadable"].as_array().unwrap();
    assert_eq!(unreadable.len(), 1, "unreadable: {unreadable:?}");
    assert_eq!(unreadable[0]["slug"], "basic-design");
    assert!(unreadable[0]["line"].is_number());

    // 2. dry_run default must not touch the file.
    let before = std::fs::read(&bad_path).unwrap();
    let dry = server.call(
        "handoff_doc_repair_frontmatter",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    assert_eq!(dry["dry_run"], true);
    assert_eq!(dry["repaired"].as_array().unwrap().len(), 1);
    assert_eq!(std::fs::read(&bad_path).unwrap(), before);

    // 3. Apply the fix.
    let applied = server.call(
        "handoff_doc_repair_frontmatter",
        json!({ "project_dir": dir.path().to_string_lossy(), "dry_run": false }),
    );
    assert_eq!(applied["repaired"].as_array().unwrap()[0]["applied"], true);
    assert!(applied["unrepaired"].as_array().unwrap().is_empty());

    // 4. The document is back in the normal listing and no longer
    //    unreadable — round-tripped through the real binary end to end.
    let after = server.call(
        "handoff_doc_list",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    assert_eq!(after["documents"].as_array().unwrap().len(), 1);
    assert_eq!(after["documents"][0]["title"], "Basic Design");
    assert!(after["unreadable"].as_array().unwrap().is_empty());

    // 5. The repaired on-disk frontmatter itself is standard, parseable
    //    YAML (not just "the server's in-memory view agrees") — read it
    //    back with a second, independent handoff_doc_get call.
    let got = server.call(
        "handoff_doc_get",
        json!({ "project_dir": dir.path().to_string_lossy(), "doc_id": "basic-design", "format": "meta" }),
    );
    assert_eq!(got["title"], "Basic Design");
    assert!(got["scope_paths"].as_array().unwrap().is_empty());
}
