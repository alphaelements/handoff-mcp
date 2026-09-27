//! Real-binary E2E test for M1 task<->item link canonicalization (t360.7,
//! wiki/220-vmodel-integration-design.md §2.5): spawns the actual
//! `handoff-mcp` binary and drives it over real stdio JSON-RPC (same harness
//! style as `tests/layer_sync_e2e.rs`), not `process_line` in-process.
//!
//! Covers the task's required E2E path: a layer document with one left-side
//! (`basic_spec`) item and one right-side (`unit_test`) item -> a task links
//! both via `handoff_update_task(requirement_ids=...)` with no explicit
//! `requirement_roles` -> role is inferred per item (implements for the left
//! item, executes for the right item) and `SubItem.task_ids` is derived on
//! both -> marking the task `done` propagates `dev_stage` to the
//! `implements`-linked (left) item only, never to the `executes`-linked
//! (right) item -> `handoff_doc_repair_task_ids` full-rebuild always
//! runs (ungated, t360.42 B1), even on an immediate repeat call.
//!
//! A second test covers t360.42 B2: an explicit `requirement_roles`
//! override given at task *creation* time (not just on a later
//! `update_task`) must be honored rather than silently falling back to
//! inference.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
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

    /// Returns the tool's raw response text, unparsed — some tools
    /// (`handoff_update_task`'s create path) return a plain confirmation
    /// string, not JSON.
    fn call_raw(&mut self, name: &str, arguments: Value) -> String {
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
        assert!(
            !resp["result"]["isError"].as_bool().unwrap_or(false),
            "{name} failed: {resp}"
        );
        resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        let text = self.call_raw(name, arguments);
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn find_sub_item_by_stable_id<'a>(status: &'a Value, stable_id: &str) -> &'a Value {
    status["items"]
        .as_array()
        .expect("items array")
        .iter()
        .flat_map(|i| i["sub_items"].as_array().expect("sub_items array"))
        .find(|s| s["stable_id"] == stable_id)
        .unwrap_or_else(|| panic!("stable_id {stable_id} not found in {status}"))
}

fn unique_slug(label: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{label}-{n}")
}

#[test]
fn implements_and_executes_roles_derive_task_ids_and_gate_dev_stage_propagation() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();

    let mut server = Server::spawn();

    let init = server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "role-e2e" }),
    );
    assert!(
        init.get("error").is_none() || init["error"].is_null(),
        "init failed: {init}"
    );

    // Left-side (basic_spec) requirement item.
    let spec_slug = unique_slug("basic-spec-role-e2e");
    let spec_body = "# Basic spec\n\n### SPEC-101 Lockout\n\nLock after 5 failed attempts.\n";
    let spec_saved = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": spec_slug,
            "title": "Basic spec (role E2E)",
            "body": spec_body,
            "layer": "basic_spec",
        }),
    );
    let spec_doc_id = spec_saved["doc_id"].as_str().expect("doc_id").to_string();

    // Right-side (unit_test) verification item, verifying SPEC-101.
    let test_slug = unique_slug("unit-test-role-e2e");
    let test_body =
        "# Unit tests\n\n### UT-101 Lockout test\n\n- verifies: SPEC-101\n\nAsserts lockout after 5 attempts.\n";
    let test_saved = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": test_slug,
            "title": "Unit tests (role E2E)",
            "body": test_body,
            "layer": "unit_test",
        }),
    );
    let test_doc_id = test_saved["doc_id"].as_str().expect("doc_id").to_string();

    // A leaf task, created todo (no estimate required at todo). The create
    // path returns a plain confirmation string ("Created task <id>: ..."),
    // not JSON.
    let created_text = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "title": "Implement + test lockout" },
        }),
    );
    assert!(
        created_text.starts_with("Created task "),
        "unexpected create response: {created_text}"
    );
    let task_id = created_text
        .trim_start_matches("Created task ")
        .split(':')
        .next()
        .unwrap_or_default()
        .to_string();
    assert!(
        !task_id.is_empty(),
        "could not extract task id from: {created_text}"
    );

    // Link both stable_ids with no explicit requirement_roles: role must be
    // inferred per item from its effective-layer side.
    server.call(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": {
                "id": task_id,
                "requirement_ids": ["SPEC-101", "UT-101"],
                "schedule": { "estimate_hours": 1.0 },
            },
        }),
    );

    let task = server.call(
        "handoff_get_task",
        json!({ "project_dir": dir.to_string_lossy(), "task_id": task_id }),
    );
    let task_links = task["task_links"].as_array().expect("task_links array");
    let role_for = |label: &str| -> Option<String> {
        task_links
            .iter()
            .find(|l| l["link_type"] == "requirement" && l["label"] == label)
            .and_then(|l| l["role"].as_str())
            .map(str::to_string)
    };
    assert_eq!(
        role_for("SPEC-101").as_deref(),
        Some("implements"),
        "left-side (basic_spec) item must infer role=implements: task_links={task_links:?}"
    );
    assert_eq!(
        role_for("UT-101").as_deref(),
        Some("executes"),
        "right-side (unit_test) item must infer role=executes: task_links={task_links:?}"
    );

    // SubItem.task_ids must be derived on both documents.
    let spec_status = server.call(
        "handoff_doc_verify_status",
        json!({ "project_dir": dir.to_string_lossy(), "doc_id": spec_doc_id, "include_items": true }),
    );
    let spec_sub = find_sub_item_by_stable_id(&spec_status, "SPEC-101");
    assert_eq!(spec_sub["stable_id"], "SPEC-101");
    assert_eq!(
        spec_sub["task_ids"].as_array().unwrap(),
        &vec![Value::String(task_id.clone())]
    );

    let test_status = server.call(
        "handoff_doc_verify_status",
        json!({ "project_dir": dir.to_string_lossy(), "doc_id": test_doc_id, "include_items": true }),
    );
    let test_sub = find_sub_item_by_stable_id(&test_status, "UT-101");
    assert_eq!(test_sub["stable_id"], "UT-101");
    assert_eq!(
        test_sub["task_ids"].as_array().unwrap(),
        &vec![Value::String(task_id.clone())]
    );
    assert_eq!(
        test_sub["dev_stage"], "not_started",
        "a fresh right-side item must start not_started"
    );

    // Marking the task done must propagate dev_stage to the implements-
    // linked (left) item only — never to the executes-linked (right) item.
    server.call(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "id": task_id, "status": "done" },
        }),
    );

    let spec_status_after = server.call(
        "handoff_doc_verify_status",
        json!({ "project_dir": dir.to_string_lossy(), "doc_id": spec_doc_id, "include_items": true }),
    );
    assert_eq!(
        find_sub_item_by_stable_id(&spec_status_after, "SPEC-101")["dev_stage"],
        "implemented",
        "implements-linked item must have dev_stage propagated on task done"
    );

    let test_status_after = server.call(
        "handoff_doc_verify_status",
        json!({ "project_dir": dir.to_string_lossy(), "doc_id": test_doc_id, "include_items": true }),
    );
    assert_eq!(
        find_sub_item_by_stable_id(&test_status_after, "UT-101")["dev_stage"],
        "not_started",
        "executes-linked item must NOT have dev_stage propagated on task done"
    );

    // handoff_doc_repair_task_ids (t360.42 B1, wiki/220 §2.5): the explicit
    // repair tool is ungated — it always forces a full rescan, even on an
    // immediate repeat call with no task changes in between, unlike
    // trace_report's own gated self-repair pass.
    let repair_1 = server.call(
        "handoff_doc_repair_task_ids",
        json!({ "project_dir": dir.to_string_lossy() }),
    );
    assert_eq!(
        repair_1["ran"], true,
        "the explicit repair tool must always run, never gated: {repair_1}"
    );
    let repair_2 = server.call(
        "handoff_doc_repair_task_ids",
        json!({ "project_dir": dir.to_string_lossy() }),
    );
    assert_eq!(
        repair_2["ran"], true,
        "an immediate repeat call must still always run (no fingerprint gate): {repair_2}"
    );
    assert_eq!(
        repair_2["sub_items_changed"], 0,
        "with no task changes in between, the repeat rescan finds no drift to correct: {repair_2}"
    );
}

/// t360.42 B2 (M1 adversarial review, BLOCKER): an explicit
/// `requirement_roles` override given at task *creation* time (both the
/// brand-new-task path and the upsert-create path) must be honored, not
/// silently dropped in favor of inference — the create paths used to call
/// `link_requirements_to_task` (`roles: &HashMap::new()`), so a
/// `requirement_roles: {"REQ-101": "executes"}` given alongside
/// `requirement_ids` at creation never reached `apply_requirement_links`.
#[test]
fn create_time_requirement_roles_override_is_honored_not_dropped() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();

    let mut server = Server::spawn();

    let init = server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "create-role-e2e" }),
    );
    assert!(
        init.get("error").is_none() || init["error"].is_null(),
        "init failed: {init}"
    );

    // A left-side (basic_spec) item — its inferred role would be
    // "implements", so an explicit "executes" override at create time is
    // unambiguously distinguishable from the inferred default.
    let spec_slug = unique_slug("create-role-spec");
    let spec_body = "# Spec\n\n### REQ-101 Something\n\nBody.\n";
    let spec_saved = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": spec_slug,
            "title": "Create-time role spec",
            "body": spec_body,
            "layer": "basic_spec",
        }),
    );
    let spec_doc_id = spec_saved["doc_id"].as_str().expect("doc_id").to_string();

    // Brand-new task creation path (no pre-existing id): requirement_ids +
    // requirement_roles given in the same create call.
    let created_text = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": {
                "title": "Verify REQ-101",
                "requirement_ids": ["REQ-101"],
                "requirement_roles": { "REQ-101": "executes" },
            },
        }),
    );
    assert!(
        created_text.starts_with("Created task "),
        "unexpected create response: {created_text}"
    );
    let task_id = created_text
        .trim_start_matches("Created task ")
        .split(':')
        .next()
        .unwrap_or_default()
        .to_string();
    assert!(
        !task_id.is_empty(),
        "could not extract task id from: {created_text}"
    );

    let task = server.call(
        "handoff_get_task",
        json!({ "project_dir": dir.to_string_lossy(), "task_id": task_id }),
    );
    let task_links = task["task_links"].as_array().expect("task_links array");
    let role = task_links
        .iter()
        .find(|l| l["link_type"] == "requirement" && l["label"] == "REQ-101")
        .and_then(|l| l["role"].as_str());
    assert_eq!(
        role,
        Some("executes"),
        "an explicit requirement_roles override given at creation time must be \
         honored (not dropped in favor of the inferred \"implements\" default \
         for a left-side item): task_links={task_links:?}"
    );

    let spec_status = server.call(
        "handoff_doc_verify_status",
        json!({ "project_dir": dir.to_string_lossy(), "doc_id": spec_doc_id, "include_items": true }),
    );
    let sub = find_sub_item_by_stable_id(&spec_status, "REQ-101");
    assert_eq!(
        sub["task_ids"].as_array().unwrap(),
        &vec![Value::String(task_id.clone())],
        "task_ids must still be derived even though the role was overridden"
    );
}
