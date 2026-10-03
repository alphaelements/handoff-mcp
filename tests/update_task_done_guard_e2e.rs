//! Real-binary MCP-tool E2E for M2-13 (wiki/260-vmodel-m2-design.md §3.4/
//! §4.11, t360.20.13): `list_tasks(layer, role)`, `get_task`/
//! `task_checklist(view)`'s `trace` field, and `update_task`'s done guard
//! (`warn`/`block`/`off`). CLI/unit coverage of the underlying blocker
//! computation lives in `src/trace/task_view.rs`'s `lightweight_tests` and
//! `src/mcp/handlers/update_task.rs`'s `done_guard_tests`; this file only
//! exercises the real stdio JSON-RPC wiring end to end.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::{json, Value};

use handoff_mcp::storage::config::{read_config, write_config};

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

/// Builds a project with one `requirement`-layer doc (`REQ-900`), one
/// `acceptance`-layer doc verifying it (`AT-900`, no run recorded -> a
/// `not_run` blocker), and a leaf task `t1` linked to `REQ-900` via
/// `requirement_ids` (`implements`, the default role). Returns `(tmp_dir,
/// project_dir_string)`.
fn setup_project(server: &mut Server, name: &str) -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": name }),
    );
    server.call(
        "handoff_update_config",
        json!({
            "project_dir": pd,
            "updates": { "settings.require_estimate_hours": false }
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "req-done-guard-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-900 Something\n\nBody.\n",
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "at-done-guard-e2e",
            "title": "Acceptance",
            "layer": "acceptance",
            "body": "# Acceptance\n\n### AT-900 Confirms it\n\n\
                - verifies: REQ-900\n\
                - method: manual\n\n\
                Body.\n",
        }),
    );
    server.call(
        "handoff_update_task",
        json!({
            "project_dir": pd,
            "task": {
                "title": "Implement REQ-900",
                "requirement_ids": ["REQ-900"]
            }
        }),
    );

    (tmp, pd)
}

fn first_task_id(tree: &Value) -> String {
    tree["task_tree"][0]["id"]
        .as_str()
        .expect("task id")
        .to_string()
}

#[test]
fn list_tasks_layer_and_role_filters() {
    let mut server = Server::spawn();
    let (_tmp, pd) = setup_project(&mut server, "list-tasks-layer-role-e2e");
    let tree = server.call("handoff_list_tasks", json!({ "project_dir": pd }));
    let task_id = first_task_id(&tree);

    let matching = server.call(
        "handoff_list_tasks",
        json!({ "project_dir": pd, "layer": "requirement" }),
    );
    assert_eq!(
        matching["task_tree"].as_array().unwrap().len(),
        1,
        "{matching}"
    );
    assert_eq!(matching["task_tree"][0]["id"], task_id);

    let non_matching = server.call(
        "handoff_list_tasks",
        json!({ "project_dir": pd, "layer": "acceptance" }),
    );
    assert_eq!(
        non_matching["task_tree"].as_array().unwrap().len(),
        0,
        "{non_matching}"
    );

    let implements = server.call(
        "handoff_list_tasks",
        json!({ "project_dir": pd, "role": "implements" }),
    );
    assert_eq!(
        implements["task_tree"].as_array().unwrap().len(),
        1,
        "{implements}"
    );

    let executes = server.call(
        "handoff_list_tasks",
        json!({ "project_dir": pd, "role": "executes" }),
    );
    assert_eq!(
        executes["task_tree"].as_array().unwrap().len(),
        0,
        "{executes}"
    );
}

#[test]
fn get_task_and_task_checklist_view_expose_trace_blockers() {
    let mut server = Server::spawn();
    let (_tmp, pd) = setup_project(&mut server, "get-task-trace-e2e");
    let tree = server.call("handoff_list_tasks", json!({ "project_dir": pd }));
    let task_id = first_task_id(&tree);

    let got = server.call(
        "handoff_get_task",
        json!({ "project_dir": pd, "task_id": task_id }),
    );
    assert_eq!(got["trace"]["blockers"]["not_run"], 1, "{got}");
    assert_eq!(
        got["trace"]["layers"][0],
        json!({ "layer": "requirement", "role": "implements", "count": 1 }),
        "{got}"
    );

    let checklist = server.call(
        "handoff_task_checklist",
        json!({ "project_dir": pd, "task_id": task_id }),
    );
    assert_eq!(checklist["trace"]["blockers"]["not_run"], 1, "{checklist}");
    assert_eq!(checklist["no_linked_docs"], true, "{checklist}");
}

#[test]
fn update_task_done_guard_warn_mode_is_the_default() {
    let mut server = Server::spawn();
    let (_tmp, pd) = setup_project(&mut server, "done-guard-warn-e2e");
    let tree = server.call("handoff_list_tasks", json!({ "project_dir": pd }));
    let task_id = first_task_id(&tree);

    let (is_error, text) = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": pd,
            "task": { "id": task_id, "status": "review" }
        }),
    );
    assert!(!is_error, "{text}");
    assert!(
        text.contains("done_guard") && text.contains("not_run=1"),
        "expected a done_guard warning, got: {text}"
    );

    let got = server.call(
        "handoff_get_task",
        json!({ "project_dir": pd, "task_id": task_id }),
    );
    assert_eq!(
        got["status"], "review",
        "warn mode must not block the transition"
    );
}

#[test]
fn update_task_done_guard_block_mode_rejects_unless_forced() {
    let mut server = Server::spawn();
    let (tmp, pd) = setup_project(&mut server, "done-guard-block-e2e");
    let tree = server.call("handoff_list_tasks", json!({ "project_dir": pd }));
    let task_id = first_task_id(&tree);

    let config_path = tmp.path().join("proj").join(".handoff").join("config.toml");
    let mut config = read_config(&config_path).expect("read config");
    config.trace.done_guard = "block".to_string();
    write_config(&config_path, &config).expect("write config");

    let (is_error, text) = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": pd,
            "task": { "id": task_id, "status": "review" }
        }),
    );
    assert!(is_error, "expected block mode to reject: {text}");
    assert!(
        text.contains("done_guard") && text.contains("force"),
        "expected a done_guard rejection mentioning force, got: {text}"
    );

    let got = server.call(
        "handoff_get_task",
        json!({ "project_dir": pd, "task_id": task_id }),
    );
    assert_eq!(
        got["status"], "todo",
        "rejected call must not have changed status"
    );

    // force: true overrides the rejection.
    let (is_error2, text2) = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": pd,
            "task": { "id": task_id, "status": "review" },
            "force": true
        }),
    );
    assert!(!is_error2, "{text2}");
    let got2 = server.call(
        "handoff_get_task",
        json!({ "project_dir": pd, "task_id": task_id }),
    );
    assert_eq!(got2["status"], "review");
}

/// Review round 2 MAJOR rework: before this fix, `block` mode's pre-check
/// only ran on `handle_update` (an existing task's status transition) —
/// creating a brand-new task directly in `status: "done"` with
/// `requirement_ids` attached in the very same `handoff_update_task` call
/// bypassed the guard entirely, since `handle_create` never consulted
/// `[trace] done_guard` at all.
#[test]
fn update_task_done_guard_block_mode_rejects_create_straight_into_done_with_a_blocker() {
    let mut server = Server::spawn();
    let (tmp, pd) = setup_project(&mut server, "done-guard-block-create-e2e");
    let before = server.call("handoff_list_tasks", json!({ "project_dir": pd }));
    let before_count = before["task_tree"].as_array().unwrap().len();

    let config_path = tmp.path().join("proj").join(".handoff").join("config.toml");
    let mut config = read_config(&config_path).expect("read config");
    config.trace.done_guard = "block".to_string();
    write_config(&config_path, &config).expect("write config");

    // REQ-900 (from `setup_project`) has a `not_run` blocker (its only
    // verifier AT-900 has no recorded run) — creating a new task directly in
    // `done`, `implements`-linked to it in the same call, must be rejected.
    let (is_error, text) = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": pd,
            "task": {
                "title": "A second implementer",
                "status": "done",
                "requirement_ids": ["REQ-900"]
            }
        }),
    );
    assert!(is_error, "expected block mode to reject the create: {text}");
    assert!(
        text.contains("done_guard") && text.contains("force"),
        "expected a done_guard rejection mentioning force, got: {text}"
    );

    // Nothing left behind: a rejected create must not burn a task id/dir.
    let after = server.call("handoff_list_tasks", json!({ "project_dir": pd }));
    assert_eq!(
        after["task_tree"].as_array().unwrap().len(),
        before_count,
        "a rejected create must leave no new task behind: {after}"
    );

    // force: true overrides the rejection, same as the existing-task path.
    let (is_error2, text2) = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": pd,
            "task": {
                "title": "A second implementer",
                "status": "done",
                "requirement_ids": ["REQ-900"]
            },
            "force": true
        }),
    );
    assert!(!is_error2, "{text2}");
}

/// Review round 2 MAJOR rework: `update_task.rs::handle`'s own
/// `[trace] done_guard` config-read fallback (an unrecognized value is never
/// silently escalated to `"block"` nor silently disabled as `"off"` — it
/// falls back to the `"warn"` default) had no E2E coverage, even though the
/// unit-test module's own doc comment claimed it did.
#[test]
fn update_task_done_guard_invalid_value_falls_back_to_warn_mode() {
    let mut server = Server::spawn();
    let (tmp, pd) = setup_project(&mut server, "done-guard-invalid-value-e2e");
    let tree = server.call("handoff_list_tasks", json!({ "project_dir": pd }));
    let task_id = first_task_id(&tree);

    let config_path = tmp.path().join("proj").join(".handoff").join("config.toml");
    let mut config = read_config(&config_path).expect("read config");
    config.trace.done_guard = "bogus-value".to_string();
    write_config(&config_path, &config).expect("write config");

    let (is_error, text) = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": pd,
            "task": { "id": task_id, "status": "review" }
        }),
    );
    assert!(
        !is_error,
        "an invalid [trace] done_guard value must never escalate to block: {text}"
    );
    assert!(
        text.contains("done_guard") && text.contains("not_run=1"),
        "expected the warn-mode fallback advisory, got: {text}"
    );

    let got = server.call(
        "handoff_get_task",
        json!({ "project_dir": pd, "task_id": task_id }),
    );
    assert_eq!(
        got["status"], "review",
        "the warn fallback must not block the transition"
    );
}

/// M3-04 (wiki/270-vmodel-m3-design.md §3.3, FR-406): a `draft`-approval
/// linked item (REQ-900's default) is its own `approval_blocker`, warned on
/// in `warn` mode and rejected in `block` mode, even once every other
/// blocker kind (`not_run`/etc.) is cleared by a recorded passing run —
/// isolating this category end to end through the real stdio binary.
#[test]
fn update_task_done_guard_approval_draft_blocker_warns_then_blocks_then_clears() {
    let mut server = Server::spawn();
    let (_tmp, pd) = setup_project(&mut server, "done-guard-approval-draft-e2e");
    let tree = server.call("handoff_list_tasks", json!({ "project_dir": pd }));
    let task_id = first_task_id(&tree);

    // Clear the `not_run` blocker so only `approval_draft` remains.
    server.call(
        "handoff_trace_update",
        json!({
            "project_dir": pd,
            "ops": [{"op": "record", "item": "AT-900", "result": "pass"}],
        }),
    );

    let (is_error, text) = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": pd,
            "task": { "id": task_id, "status": "review" }
        }),
    );
    assert!(!is_error, "{text}");
    assert!(
        text.contains("done_guard") && text.contains("approval_draft=1"),
        "expected a done_guard warning naming the approval_draft blocker, got: {text}"
    );

    let config_path = tmp_config_path(&pd);
    let mut config = read_config(&config_path).expect("read config");
    config.trace.done_guard = "block".to_string();
    write_config(&config_path, &config).expect("write config");

    // Revert the task to `todo` so the next call is a fresh guarded
    // transition (the warn-mode call above already moved it to `review`).
    server.call(
        "handoff_update_task",
        json!({ "project_dir": pd, "task": { "id": task_id, "status": "todo" } }),
    );

    let (is_error, text) = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": pd,
            "task": { "id": task_id, "status": "review" }
        }),
    );
    assert!(is_error, "expected block mode to reject: {text}");
    assert!(
        text.contains("done_guard") && text.contains("approval_draft=1"),
        "expected a done_guard rejection naming the approval_draft blocker, got: {text}"
    );

    // Approving REQ-900 clears the blocker; block mode now allows the
    // transition without `force`.
    server.call(
        "handoff_trace_update",
        json!({
            "project_dir": pd,
            "ops": [{"op": "set", "item": "REQ-900", "approval": "approved"}],
        }),
    );
    let (is_error, text) = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": pd,
            "task": { "id": task_id, "status": "review" }
        }),
    );
    assert!(!is_error, "expected no blocker once approved: {text}");
    assert!(!text.contains("done_guard"), "got: {text}");
}

fn tmp_config_path(project_dir: &str) -> PathBuf {
    PathBuf::from(project_dir)
        .join(".handoff")
        .join("config.toml")
}

#[test]
fn update_task_done_guard_off_mode_never_warns_or_blocks() {
    let mut server = Server::spawn();
    let (tmp, pd) = setup_project(&mut server, "done-guard-off-e2e");
    let tree = server.call("handoff_list_tasks", json!({ "project_dir": pd }));
    let task_id = first_task_id(&tree);

    let config_path = tmp.path().join("proj").join(".handoff").join("config.toml");
    let mut config = read_config(&config_path).expect("read config");
    config.trace.done_guard = "off".to_string();
    write_config(&config_path, &config).expect("write config");

    let (is_error, text) = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": pd,
            "task": { "id": task_id, "status": "review" }
        }),
    );
    assert!(!is_error, "{text}");
    assert!(!text.contains("done_guard"), "got: {text}");
}
