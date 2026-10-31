//! Real-binary E2E for t-layergen.9 (REQ-LAG-006, wiki/430 §3.7):
//! `handoff_trace_next` proposes `update_drifted_spec` (and `rerun`) actions
//! for items whose `impl_refs` files changed since their recorded
//! `content_hash`, and attaches `plan_update_needed` when a task implementing
//! the drifted item is also linked to a plan-document item. Proposal only:
//! no task is created.

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

fn pd(dir: &Path) -> String {
    dir.to_string_lossy().to_string()
}

fn actions_of(server: &mut Server, dir: &Path, kinds: Value) -> Vec<Value> {
    let out = server.call(
        "handoff_trace_next",
        json!({ "project_dir": pd(dir), "kinds": kinds }),
    );
    out["actions"].as_array().expect("actions array").clone()
}

/// A project with `src/a.rs`, requirement `REQ-001` (`impl_refs` -> a.rs, hash
/// recorded), an acceptance item `AT-001` verifying it, a plan document item
/// `FR-PLAN-001`, and a task implementing both REQ-001 and FR-PLAN-001.
fn drift_project() -> (tempfile::TempDir, PathBuf, Server, String) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/a.rs"), "fn a() {}\n").unwrap();
    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd(&dir), "project_name": "drift-next" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd(&dir), "slug": "reqs", "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-001 First\n\nBody.\n",
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd(&dir), "slug": "acceptance", "title": "Acceptance",
            "layer": "acceptance",
            "body": "# Acceptance\n\n### AT-001 Check\n\n- verifies: REQ-001\n\nBody.\n",
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd(&dir), "slug": "rollout-plan", "title": "Plan",
            "doc_type": "plan", "layer": "basic_spec",
            "body": "# Plan\n\n### FR-PLAN-001 Rollout\n\nA.\n",
        }),
    );
    server.call(
        "handoff_trace_update",
        json!({
            "project_dir": pd(&dir),
            "ops": [{ "op": "set", "item": "REQ-001", "impl_refs": [{ "path": "src/a.rs" }] }],
        }),
    );
    let created = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": pd(&dir),
            "task": { "title": "Implement REQ-001", "schedule": { "estimate_hours": 1.0 } },
        }),
    );
    let task_id = created
        .trim_start_matches("Created task ")
        .split(':')
        .next()
        .unwrap_or_default()
        .to_string();
    server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": pd(&dir),
            "task": { "id": task_id, "requirement_ids": ["REQ-001", "FR-PLAN-001"] },
        }),
    );
    (tmp, dir, server, task_id)
}

#[test]
fn unchanged_code_yields_no_drift_actions() {
    let (_tmp, dir, mut server, _task) = drift_project();
    let actions = actions_of(&mut server, &dir, json!(["update_drifted_spec"]));
    assert!(actions.is_empty(), "{actions:?}");
}

#[test]
fn drifted_code_yields_update_drifted_spec_with_plan_update_needed() {
    let (_tmp, dir, mut server, task_id) = drift_project();
    std::fs::write(dir.join("src/a.rs"), "fn a() { 1 }\n").unwrap();

    let actions = actions_of(&mut server, &dir, json!(["update_drifted_spec"]));
    assert_eq!(actions.len(), 1, "{actions:?}");
    let a = &actions[0];
    assert_eq!(a["kind"], "update_drifted_spec");
    assert_eq!(a["item"], "REQ-001");
    assert_eq!(a["rank"], 3);
    assert_eq!(a["suggest"]["tool"], "handoff_trace_update");
    let plan = a["plan_update_needed"]
        .as_array()
        .expect("plan_update_needed");
    assert_eq!(plan.len(), 1, "{a}");
    assert_eq!(plan[0]["stable_id"], "FR-PLAN-001");
    assert!(plan[0]["doc_id"].is_string() && plan[0]["current_dev_stage"].is_string());

    // The suggested preview call is itself accepted by the server.
    let mut args = a["suggest"]["arguments"].clone();
    args["project_dir"] = json!(pd(&dir));
    assert_eq!(args["dry_run"], true);
    server.call_raw("handoff_trace_update", args);

    // Task-scoped call still sees the task's own drifted item.
    let scoped = server.call(
        "handoff_trace_next",
        json!({ "project_dir": pd(&dir), "task_id": task_id, "kinds": ["update_drifted_spec"] }),
    );
    assert_eq!(scoped["actions"].as_array().unwrap().len(), 1, "{scoped}");
}

#[test]
fn drifted_spec_also_proposes_its_verifier_for_rerun_and_creates_no_task() {
    let (_tmp, dir, mut server, _task) = drift_project();
    std::fs::write(dir.join("src/a.rs"), "fn a() { 2 }\n").unwrap();

    let actions = actions_of(&mut server, &dir, json!(["rerun"]));
    assert!(
        actions
            .iter()
            .any(|a| a["item"] == "AT-001" && a["kind"] == "rerun"),
        "{actions:?}"
    );
    // Proposal only: the one task the fixture created is still the only one.
    let task_dirs = std::fs::read_dir(dir.join(".handoff/tasks"))
        .expect("tasks dir")
        .count();
    assert_eq!(task_dirs, 1);
}
