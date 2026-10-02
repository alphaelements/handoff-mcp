//! Real-binary E2E tests for M2-03 (wiki/260-vmodel-m2-design.md §2.1/§2.2/
//! §3.1): spawns the actual `handoff-mcp` binary and drives real stdio
//! JSON-RPC (same harness style as `tests/trace_report_slice_e2e.rs`),
//! writing real Markdown bodies through `handoff_doc_save` so the M2-02 body
//! parser + layer sync + M2-03 engine changes are exercised together, not
//! just the pure `TraceInput` unit tests in `src/trace/engine/tests.rs`.
//!
//! Covers: horizontal `partial` from an `X#ACn` sub-reference that verifies
//! only one of two declared acceptance criteria, `- waive-verify:` reporting
//! `waived` instead of `uncovered` (and suppressing the `unverified` gap),
//! `- derived:` suppressing the `orphan` gap, and a per-document
//! `trace_profile` override applying to a child item that lives in a
//! *different*, non-overridden document but is only reachable through the
//! overridden root's `refines` chain (§2.1 規則 1-4).

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

fn init(server: &mut Server, dir: &std::path::Path, name: &str) {
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": name }),
    );
}

#[test]
fn horizontal_partial_when_one_of_two_acceptance_criteria_is_verified_over_real_stdio() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    init(&mut server, &dir, "m2-03-partial-e2e");

    let req = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "requirements-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-001 Account lockout\n\n- priority: P0\n\nAfter 5 failures the account locks.\n\n受入基準:\n- AC1: Given 4 failures When a 5th failure happens Then the account locks\n- AC2: WHEN the account is locked THE SYSTEM SHALL reject a correct password\n",
        }),
    );
    assert!(req.get("doc_id").is_some(), "doc_save failed: {req}");

    let at = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "acceptance-e2e",
            "title": "Acceptance",
            "layer": "acceptance",
            "body": "# Acceptance\n\n### AT-001 Lockout after 5 failures\n\n- verifies: REQ-001#AC1\n- method: manual\n\nOnly AC1 is covered by an explicit verification item.\n",
        }),
    );
    assert!(at.get("doc_id").is_some(), "doc_save failed: {at}");

    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy() }),
    );
    let req_cov = &report["coverage"]["requirement"]["horizontal"];
    assert_eq!(
        req_cov["partial"], 1,
        "AC1 verified, AC2 not -> partial, not covered: {req_cov}"
    );
    assert_eq!(req_cov["covered"], 0);
    // `partial` is a softer classification than the M1 gap list (wiki/260
    // §3.1) — no `unverified` gap for REQ-001.
    let gaps = report["gaps"].as_array().unwrap();
    assert!(
        !gaps
            .iter()
            .any(|g| g["kind"] == "unverified" && g["item"] == "REQ-001"),
        "partial must not also report unverified: {gaps:?}"
    );
}

#[test]
fn waive_verify_reports_waived_and_suppresses_the_unverified_gap_over_real_stdio() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    init(&mut server, &dir, "m2-03-waiver-e2e");

    let req = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "requirements-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-001 Legacy behavior\n\n- waive-verify: covered by an external manual test suite (2026-09 audit)\n\nNo automated verification exists for this legacy requirement.\n",
        }),
    );
    assert!(req.get("doc_id").is_some(), "doc_save failed: {req}");

    // `acceptance` (requirement's pair) must be "in use" for the axis to be
    // anything other than `na` — configure both layers explicitly so a
    // real, would-otherwise-be-uncovered axis is what gets waived.
    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy(), "layers": ["requirement", "acceptance"] }),
    );
    let req_cov = &report["coverage"]["requirement"]["horizontal"];
    assert_eq!(req_cov["waived"], 1, "{req_cov}");
    assert_eq!(req_cov["uncovered"], 0);
    let gaps = report["gaps"].as_array().unwrap();
    assert!(
        !gaps
            .iter()
            .any(|g| g["kind"] == "unverified" && g["item"] == "REQ-001"),
        "waived must suppress the unverified gap: {gaps:?}"
    );
}

#[test]
fn derived_item_suppresses_the_orphan_gap_over_real_stdio() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    init(&mut server, &dir, "m2-03-derived-e2e");

    let req = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "requirements-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-001 Root requirement\n\nHas no upper layer, never orphan on its own.\n",
        }),
    );
    assert!(req.get("doc_id").is_some(), "doc_save failed: {req}");

    let spec = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "basic-spec-e2e",
            "title": "Basic spec",
            "layer": "basic_spec",
            "body": "# Basic spec\n\n### SPEC-001 Implementation detail\n\n- derived: needed by the chosen implementation, no upstream requirement\n\nNo `refines` line on purpose.\n",
        }),
    );
    assert!(spec.get("doc_id").is_some(), "doc_save failed: {spec}");

    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy() }),
    );
    let gaps = report["gaps"].as_array().unwrap();
    assert!(
        !gaps
            .iter()
            .any(|g| g["kind"] == "orphan" && g["item"] == "SPEC-001"),
        "a derived item must not report orphan: {gaps:?}"
    );
}

#[test]
fn doc_profile_override_applies_to_a_child_in_a_different_non_overridden_document_over_real_stdio()
{
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    init(&mut server, &dir, "m2-03-profile-tree-e2e");

    // REQ-100's own document overrides `trace_profile` to `minimal` (only
    // `requirement`/`acceptance` — no `basic_spec`).
    let req = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "root-doc-e2e",
            "title": "Root requirement",
            "layer": "requirement",
            "trace_profile": "minimal",
            "body": "# Root requirement\n\n### REQ-100 Root item\n\nOverrides this document's V-model profile to minimal.\n",
        }),
    );
    assert!(req.get("doc_id").is_some(), "doc_save failed: {req}");

    // BS-1 lives in a *different*, non-overridden document but only refines
    // REQ-100 — it must inherit REQ-100's `minimal` profile (which drops
    // `basic_spec` out of scope), not the project's own default/auto layers.
    let spec = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "other-doc-e2e",
            "title": "Other spec",
            "layer": "basic_spec",
            "body": "# Other spec\n\n### BS-1 Reachable only via REQ-100\n\n- refines: REQ-100\n\nLives in a document with no `trace_profile` override of its own.\n",
        }),
    );
    assert!(spec.get("doc_id").is_some(), "doc_save failed: {spec}");

    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy() }),
    );
    assert!(
        report["coverage"].get("basic_spec").is_none(),
        "BS-1 must drop out of scope under REQ-100's inherited minimal profile: {}",
        report["coverage"]
    );
}

/// §2.1 規則 4: `items[].profile` (the effective, sorted profile name array
/// reached by an item's own tree) must be observable by a real caller, not
/// just `TraceGraph::item_profile` in a unit test — both `handoff_trace_report
/// (include_items=true)` (and therefore the persisted `_trace_report.json`,
/// same builder) and `handoff_trace_slice` must emit a `profile` key on every
/// item (round 2 integration BLOCKER: neither did).
#[test]
fn items_profile_reflects_the_items_overridden_tree_via_trace_report_and_trace_slice_over_real_stdio(
) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    init(&mut server, &dir, "m2-03-items-profile-e2e");

    // REQ-100's own document overrides `trace_profile` to `minimal`.
    let req = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "root-doc-e2e",
            "title": "Root requirement",
            "layer": "requirement",
            "trace_profile": "minimal",
            "body": "# Root requirement\n\n### REQ-100 Root item\n\nOverrides this document's V-model profile to minimal.\n",
        }),
    );
    assert!(req.get("doc_id").is_some(), "doc_save failed: {req}");

    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy(), "include_items": true }),
    );
    let items = report["items"].as_array().expect("items array");
    let req_100 = items
        .iter()
        .find(|it| it["id"] == "REQ-100")
        .expect("REQ-100 present in items[]");
    assert_eq!(
        req_100["profile"],
        json!(["minimal"]),
        "handoff_trace_report items[].profile must carry the item's effective, overridden \
         profile name(s): {req_100}"
    );

    let slice = server.call(
        "handoff_trace_slice",
        json!({ "project_dir": dir.to_string_lossy(), "item": "REQ-100" }),
    );
    let slice_items = slice["items"].as_array().expect("items array");
    let slice_req_100 = slice_items
        .iter()
        .find(|it| it["id"] == "REQ-100")
        .expect("REQ-100 present in trace_slice items[]");
    assert_eq!(
        slice_req_100["profile"],
        json!(["minimal"]),
        "handoff_trace_slice items[].profile must also carry the item's effective profile: \
         {slice_req_100}"
    );
}
