//! Real-binary E2E for M1 deep-coverage state propagation and the layer-skip
//! ("inline verification") path (t360.12, wiki/220-vmodel-integration-design.md
//! §2.7/§3.2/§3.3, FR-303/FR-1006/FR-203) — the two scenarios not already
//! exercised by `tests/trace_report_slice_e2e.rs`,
//! `tests/trace_record_e2e.rs`, or `tests/trace_report_contract_fixture_e2e.rs`:
//! those files build a 4-layer-document project and drive `trace_report`/
//! `trace_slice` structurally (gaps, filters, traversal, direct-edit resync),
//! but never actually call `handoff_trace_record` with real pass/fail results
//! against that graph and check the derived `state` values, and never
//! exercise a `test`/`method`-attribute item with **no separate verification
//! document at all**. This file closes both gaps:
//!
//! 1. A full requirement/basic_spec/acceptance/system_test project (two
//!    independent requirement chains) with `handoff_update_task` role-linked
//!    tasks, then `handoff_trace_record` with **mixed pass/fail** results:
//!    one chain's acceptance+system_test both `pass` (-> the requirement's
//!    deep-coverage state must resolve to `"passing"`), the other chain's
//!    acceptance check `pass`es but the system_test one level down (on
//!    SPEC-020) `fail`s (-> REQ-020 must resolve to `"failing"` purely via
//!    its `refines` child SPEC-020 — FR-303 deep coverage — even though its
//!    own direct verifier passed). Checked through both
//!    `handoff_trace_report(include_items)` and `handoff_trace_slice`.
//! 2. A single `basic_spec` document with **no acceptance/system_test
//!    document at all** — the layer-skip path (wiki/220 §2.7's inline
//!    verification): one item carries `- method: manual`, is linked to an
//!    `implements` task (for vertical coverage — the lowest used left layer
//!    needs an implementing task, not a refining child), and a `pass` result
//!    recorded directly against it resolves the item's own state to
//!    `"passing"` — closing the V without ever creating a second document.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
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

    /// Creates a task in `project_dir` linked to `requirement_ids` at
    /// creation time, returning its id. `handoff_update_task`'s create path
    /// (no `task.id`) returns a plain confirmation string ("Created task
    /// <id>: ..." — optionally followed by non-fatal warning lines), not
    /// JSON, so this bypasses [`Server::call`]'s JSON-parsing `call` and
    /// asserts directly on the raw text instead (same technique as
    /// `tests/task_link_role_e2e.rs`).
    fn create_task(&mut self, project_dir: &Path, title: &str, requirement_ids: &[&str]) -> String {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "handoff_update_task",
                "arguments": {
                    "project_dir": project_dir.to_string_lossy(),
                    "task": { "title": title, "requirement_ids": requirement_ids },
                },
            },
        });
        writeln!(self.stdin, "{req}").expect("write to server stdin");
        self.stdin.flush().expect("flush server stdin");
        let line = self
            .lines
            .recv_timeout(Duration::from_secs(10))
            .expect("no response for handoff_update_task within 10s");
        let resp: Value = serde_json::from_str(&line).expect("valid JSON-RPC response");
        let is_error = resp["result"]["isError"].as_bool().unwrap_or(false);
        let text_resp = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !is_error && text_resp.starts_with("Created task "),
            "unexpected create response: {text_resp}"
        );
        text_resp
            .trim_start_matches("Created task ")
            .split(':')
            .next()
            .unwrap_or_default()
            .to_string()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn item_by_id<'a>(items: &'a [Value], id: &str) -> &'a Value {
    items
        .iter()
        .find(|it| it["id"] == id)
        .unwrap_or_else(|| panic!("item {id} not present in {items:?}"))
}

#[test]
fn trace_report_and_slice_resolve_deep_coverage_passing_and_failing_states_with_task_roles() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "trace-state-e2e" }),
    );

    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "requirements-state-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n\
                ### REQ-010 Lockout works end to end\n\n- priority: P1\n\nAfter 5 failures the account locks.\n\n\
                ### REQ-020 Session timeout works end to end\n\n- priority: P1\n\nIdle sessions expire.\n",
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "basic-spec-state-e2e",
            "title": "Basic spec",
            "layer": "basic_spec",
            "body": "# Basic spec\n\n\
                ### SPEC-010 Lockout counter\n\n- refines: REQ-010\n\nMaintain a per-account failure counter.\n\n\
                ### SPEC-020 Idle timer\n\n- refines: REQ-020\n\nMaintain a per-session idle timer.\n",
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "acceptance-state-e2e",
            "title": "Acceptance",
            "layer": "acceptance",
            "body": "# Acceptance\n\n\
                ### AT-010 Lockout after 5 failures\n\n- verifies: REQ-010\n- method: manual\n\nFail login 5 times.\n\n\
                ### AT-020 Idle session expires\n\n- verifies: REQ-020\n- method: manual\n\nLeave a session idle for 30 minutes.\n",
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "system-test-state-e2e",
            "title": "System tests",
            "layer": "system_test",
            "body": "# System tests\n\n\
                ### ST-010 Counter increments\n\n- verifies: SPEC-010\n- method: auto\n\nAssert the counter increments.\n\n\
                ### ST-020 Idle timer resets on activity\n\n- verifies: SPEC-020\n- method: auto\n\nAssert the timer resets.\n",
        }),
    );

    // basic_spec is the lowest used left layer (no detailed_spec document) —
    // each SPEC item needs an *implements* task for vertical coverage,
    // otherwise its own state would be pulled down to "uncovered" regardless
    // of its horizontal verifier's result (wiki/220 §2.7).
    let t_impl_010 = server.create_task(&dir, "Implement lockout counter", &["SPEC-010"]);
    let t_impl_020 = server.create_task(&dir, "Implement idle timer", &["SPEC-020"]);
    // Role is inferred: right-side (acceptance) -> "executes".
    let t_exec_at010 = server.create_task(&dir, "Run lockout acceptance test", &["AT-010"]);

    // Chain 1 (REQ-010): both verifiers pass end to end.
    server.call(
        "handoff_trace_record",
        json!({
            "project_dir": dir.to_string_lossy(),
            "results": [
                {"item": "AT-010", "result": "pass"},
                {"item": "ST-010", "result": "pass"},
            ],
        }),
    );
    // Chain 2 (REQ-020): the acceptance check (REQ-020's own direct
    // verifier) passes, but the system-test check one level down (on
    // SPEC-020) fails. REQ-020 can therefore only become "failing" through
    // the `refines` recursion into SPEC-020 (FR-303 deep coverage), and
    // "failing" must win over the direct verifier's "passing" in the max()
    // aggregation (wiki/220 §2.7's priority order: failing > blocked >
    // not_run > uncovered > passing). (Reviewer, round 1: the previous
    // AT-020 fail / ST-020 pass arrangement made REQ-020 failing via its
    // direct verifier alone, so this test stayed green with the engine's
    // refines-child recursion removed.)
    server.call(
        "handoff_trace_record",
        json!({
            "project_dir": dir.to_string_lossy(),
            "results": [
                {"item": "AT-020", "result": "pass"},
                {"item": "ST-020", "result": "fail", "note": "regressed"},
            ],
        }),
    );

    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy(), "include_items": true }),
    );
    let items = report["items"].as_array().expect("items array");

    let req010 = item_by_id(items, "REQ-010");
    assert_eq!(
        req010["state"], "passing",
        "REQ-010: AT-010 pass + SPEC-010 (ST-010 pass, implements task) must \
         resolve to passing: {req010}"
    );
    let spec010 = item_by_id(items, "SPEC-010");
    assert_eq!(spec010["state"], "passing", "{spec010}");
    let spec010_tasks = spec010["tasks"].as_array().expect("tasks array");
    assert!(
        spec010_tasks
            .iter()
            .any(|t| t["id"] == t_impl_010 && t["role"] == "implements"),
        "SPEC-010 must list its implements-linked task with the inferred role: {spec010_tasks:?}"
    );

    let spec020 = item_by_id(items, "SPEC-020");
    assert_eq!(
        spec020["state"], "failing",
        "SPEC-020 (verified by the failing ST-020) must be failing: {spec020}"
    );
    let at020 = item_by_id(items, "AT-020");
    assert_eq!(
        at020["state"], "passing",
        "AT-020 (REQ-020's direct verifier) itself passed: {at020}"
    );
    let req020 = item_by_id(items, "REQ-020");
    assert_eq!(
        req020["state"], "failing",
        "REQ-020: its direct verifier AT-020 passed, so failing can only come from \
         the refines child SPEC-020 (deep coverage) and must outrank passing: {req020}"
    );

    let at010 = item_by_id(items, "AT-010");
    let at010_tasks = at010["tasks"].as_array().expect("tasks array");
    assert!(
        at010_tasks
            .iter()
            .any(|t| t["id"] == t_exec_at010 && t["role"] == "executes"),
        "AT-010 (right-side/acceptance) must infer role=executes for its linked task: {at010_tasks:?}"
    );

    // trace_slice from the implements task must reach the same passing state
    // for the whole up-chain from SPEC-010.
    let slice = server.call(
        "handoff_trace_slice",
        json!({ "project_dir": dir.to_string_lossy(), "task_id": t_impl_010, "direction": "both" }),
    );
    let slice_items = slice["items"].as_array().expect("items array");
    let slice_spec010 = item_by_id(slice_items, "SPEC-010");
    assert_eq!(slice_spec010["state"], "passing", "{slice_spec010}");
    let slice_req010 = item_by_id(slice_items, "REQ-010");
    assert_eq!(slice_req010["state"], "passing", "{slice_req010}");

    // trace_slice from the failing chain's implements task must surface
    // REQ-020's failing state, propagated up from SPEC-020's failing verifier.
    let slice2 = server.call(
        "handoff_trace_slice",
        json!({ "project_dir": dir.to_string_lossy(), "task_id": t_impl_020, "direction": "both" }),
    );
    let slice2_items = slice2["items"].as_array().expect("items array");
    let slice2_req020 = item_by_id(slice2_items, "REQ-020");
    assert_eq!(slice2_req020["state"], "failing", "{slice2_req020}");
}

#[test]
fn inline_verification_alone_reaches_passing_with_no_paired_verification_document() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "trace-inline-e2e" }),
    );

    // A single basic_spec document -- no acceptance/system_test document is
    // ever created. SPEC-050 carries its own `method` attribute, making it
    // an inline-verified item (wiki/220 §2.7): it is simultaneously the
    // requirement definition and its own verification item.
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "basic-spec-inline-e2e",
            "title": "Basic spec (inline)",
            "layer": "basic_spec",
            "body": "# Basic spec\n\n### SPEC-050 Export completes under 5s\n\n- method: manual\n\nExporting a 10k-row report completes in under 5 seconds.\n",
        }),
    );

    // basic_spec is the only (and therefore lowest) used left layer here --
    // vertical coverage needs an implementing task since there is no
    // detailed_spec document to refine into.
    let t_impl = server.create_task(&dir, "Implement fast export", &["SPEC-050"]);

    // Before any run: not_run, but already horizontally *covered* (inline)
    // and vertically *covered* (implements task) -- no "uncovered" gap should
    // be reported for it.
    let before = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy(), "include_items": true }),
    );
    let before_items = before["items"].as_array().expect("items array");
    let spec050_before = item_by_id(before_items, "SPEC-050");
    assert_eq!(spec050_before["state"], "not_run", "{spec050_before}");
    let before_gaps = before["gaps"].as_array().expect("gaps array");
    assert!(
        !before_gaps.iter().any(|g| g["item"] == "SPEC-050"),
        "an inline-verified item with an implements task must have no unverified/unrefined \
         gap even before any run is recorded: {before_gaps:?}"
    );

    server.call(
        "handoff_trace_record",
        json!({
            "project_dir": dir.to_string_lossy(),
            "results": [{"item": "SPEC-050", "result": "pass", "evidence": ["tests/export.rs::under_5s"]}],
            "task_id": t_impl,
        }),
    );

    let after = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy(), "include_items": true }),
    );
    let after_items = after["items"].as_array().expect("items array");
    let spec050 = item_by_id(after_items, "SPEC-050");
    assert_eq!(
        spec050["state"], "passing",
        "an inline-verified item's own pass result, with vertical coverage from its \
         implements task, must resolve to passing with no separate verification document: {spec050}"
    );
    assert_eq!(spec050["category"], "requirement");
    assert_eq!(spec050["side"], "left");

    let in_use: Vec<String> = after["trace_layers"]["in_use"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        in_use,
        vec!["basic_spec".to_string()],
        "only basic_spec must be in use -- no acceptance/system_test document was ever \
         created for this layer-skip path: {in_use:?}"
    );

    // trace_history confirms the recorded run is retrievable per-item too.
    let history = server.call(
        "handoff_trace_history",
        json!({ "project_dir": dir.to_string_lossy(), "item": "SPEC-050" }),
    );
    let history_items = history["items"].as_array().expect("items array");
    assert_eq!(history_items.len(), 1);
    assert_eq!(history_items[0]["result"], "pass");
}
