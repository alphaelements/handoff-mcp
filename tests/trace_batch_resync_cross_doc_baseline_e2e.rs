//! Real-binary E2E test for t360.20.28 (bug): `resync_direct_edited_layer_docs`
//! (`src/mcp/handlers/trace.rs`) — the batch resync `handoff_trace_report`/
//! `handoff_trace_slice`/`handoff_trace_suspect`'s write actions run before
//! aggregating (wiki/260-vmodel-m2-design.md §2.5) — must resolve
//! cross-document `refines`/`verifies` baselines against the in-memory
//! `DocSet` it is itself building, not by re-reading disk. Pre-fix, a corpus
//! with more than one layer document that has never been through
//! `handoff_doc_save` even once (raw `.handoff/docs` files, e.g. right after
//! a fresh `git clone`, or a hand-authored fixture) leaves every
//! cross-document link unbaselined forever: each sibling document's on-disk
//! frontmatter still shows no `origin=body` SubItems until this same batch
//! call flushes, so `resolve_upstream_ref_across_corpus`'s ownership check
//! (`src/mcp/handlers/docs.rs`) never finds it. Same harness style as
//! `tests/trace_suspect_e2e.rs`/`tests/trace_report_slice_e2e.rs`.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::{json, Value};

use handoff_mcp::storage::docs::model::DocMetadata;
use handoff_mcp::storage::docs::{write_doc, write_doc_body};

const TS: &str = "2026-09-01T00:00:00.000000000+00:00";

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

fn suspects_of_kind<'a>(list: &'a Value, kind: &str) -> Vec<&'a Value> {
    list["suspects"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["kind"] == kind)
        .collect()
}

fn suspect_items(list: &Value, kind: &str) -> Vec<String> {
    let mut ids: Vec<String> = suspects_of_kind(list, kind)
        .into_iter()
        .map(|s| s["item"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    ids
}

/// Writes one layer document directly via the storage layer's own writer
/// functions, exactly as `tests/support/perf_fixture.rs`'s `write_trace_doc`
/// does — never through `handoff_doc_save`, so the document starts with no
/// `source.body_raw_hash` and no `verification` at all (§2.4's "1回同期し
/// て保存" case, "never synced").
fn write_layer_doc(
    handoff_dir: &Path,
    doc_id: &str,
    slug: &str,
    layer: &str,
    title: &str,
    body: &str,
) {
    let mut doc = DocMetadata::new(
        doc_id.to_string(),
        slug.to_string(),
        title.to_string(),
        "spec".to_string(),
        TS.to_string(),
    );
    doc.layer = Some(layer.to_string());
    write_doc_body(handoff_dir, slug, body).expect("write_doc_body");
    write_doc(handoff_dir, &doc).expect("write_doc");
}

/// Core repro (t360.20.28): two layer documents (REQ-200, and SPEC-200 which
/// `refines: REQ-200`) are written directly to `.handoff/docs`, neither ever
/// touched by `handoff_doc_save`. The very first aggregation call
/// (`handoff_trace_report`) must resync both in one batch *and* resolve
/// SPEC-200's `refines: REQ-200` to a real baseline — not leave it
/// unbaselined just because REQ-200's on-disk frontmatter had no
/// `origin=body` SubItems yet at the moment SPEC-200 was resolved.
#[test]
fn batch_resync_baselines_cross_doc_links_between_never_synced_docs() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let handoff = dir.join(".handoff");

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "batch-resync-baseline-e2e" }),
    );

    write_layer_doc(
        &handoff,
        "doc-20260901-000000-0001",
        "req-batch-resync",
        "requirement",
        "Requirements",
        "# Requirements\n\n### REQ-200 Session timeout\n\n- priority: P1\n\n\
         Original requirement statement text.\n",
    );
    write_layer_doc(
        &handoff,
        "doc-20260901-000000-0002",
        "spec-batch-resync",
        "basic_spec",
        "Basic spec",
        "# Basic spec\n\n### SPEC-200 Timeout enforcement\n\n- refines: REQ-200\n\n\
         Original spec statement text.\n",
    );

    // First aggregation: the one-time "never synced" batch resync for both
    // documents together.
    server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy() }),
    );

    let list = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    assert_eq!(
        list["unbaselined"]["links"], 0,
        "SPEC-200's refines: REQ-200 must get a recorded baseline from the \
         very first batch resync, not stay unbaselined forever: {list}"
    );
    assert!(
        suspect_items(&list, "link").is_empty(),
        "a freshly-baselined link must not itself read as suspect: {list}"
    );

    // Directly rewrite REQ-200's body on disk again (bypassing
    // `handoff_doc_save`) and re-aggregate — now that a real baseline
    // exists, this must surface SPEC-200 as a link suspect (done_criteria's
    // second scenario).
    write_doc_body(
        &handoff,
        "req-batch-resync",
        "# Requirements\n\n### REQ-200 Session timeout\n\n- priority: P1\n\n\
         Updated requirement statement text.\n",
    )
    .expect("write_doc_body (second edit)");

    server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy() }),
    );
    let list2 = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    assert_eq!(
        suspect_items(&list2, "link"),
        vec!["SPEC-200".to_string()],
        "changing REQ-200 after a real baseline was recorded must surface \
         SPEC-200 as a link suspect: {list2}"
    );
}
