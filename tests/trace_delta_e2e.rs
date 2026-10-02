//! Real-binary MCP-tool E2E for `handoff_trace_delta` and
//! `handoff_trace_update(propose=true)` (wiki/270-vmodel-m3-design.md
//! §2.5/§4.2/§4.3, M3-08, FR-407).

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

fn init_project(server: &mut Server, project_name: &str) -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": project_name }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "req-delta-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-900 Something\n\nBody.\n",
        }),
    );
    (tmp, pd)
}

/// create -> list (stale=false) -> apply -> list (status=applied).
#[test]
fn create_list_apply_list_full_lifecycle() {
    let mut server = Server::spawn();
    let (_tmp, pd) = init_project(&mut server, "trace-delta-e2e-lifecycle");

    let created = server.call(
        "handoff_trace_delta",
        json!({
            "project_dir": pd,
            "action": "create",
            "description": "bump dev_stage",
            "ops": [{"op": "set", "item": "REQ-900", "dev_stage": "in_progress"}],
        }),
    );
    let delta_id = created["delta_id"].as_str().expect("delta_id").to_string();
    assert!(!delta_id.is_empty());
    assert_eq!(created["ops_count"], 1);
    assert_eq!(created["previews"].as_array().unwrap().len(), 1);

    let delta_path = std::path::Path::new(&pd)
        .join(".handoff")
        .join("trace")
        .join("deltas")
        .join(format!("{delta_id}.json"));
    assert!(delta_path.exists(), "delta file must exist on disk");
    let on_disk: Value =
        serde_json::from_str(&std::fs::read_to_string(&delta_path).unwrap()).unwrap();
    assert_eq!(on_disk["status"], "pending");
    assert_eq!(on_disk["description"], "bump dev_stage");
    assert!(on_disk["baseline_hashes"]["REQ-900"].is_string());

    let listed = server.call(
        "handoff_trace_delta",
        json!({ "project_dir": pd, "action": "list" }),
    );
    let deltas = listed["deltas"].as_array().unwrap();
    assert_eq!(deltas.len(), 1, "{listed}");
    assert_eq!(deltas[0]["delta_id"], delta_id);
    assert_eq!(deltas[0]["status"], "pending");
    assert_eq!(deltas[0]["stale"], false, "{listed}");

    let applied = server.call(
        "handoff_trace_delta",
        json!({ "project_dir": pd, "action": "apply", "delta_id": delta_id }),
    );
    assert_eq!(applied["applied"].as_array().unwrap().len(), 1, "{applied}");
    assert!(applied["remainder_delta_id"].is_null());

    let listed_after = server.call(
        "handoff_trace_delta",
        json!({ "project_dir": pd, "action": "list", "status": "applied" }),
    );
    let deltas_after = listed_after["deltas"].as_array().unwrap();
    assert_eq!(deltas_after.len(), 1, "{listed_after}");
    assert_eq!(deltas_after[0]["delta_id"], delta_id);
    assert_eq!(deltas_after[0]["status"], "applied");

    // A default (pending) list call no longer returns it.
    let listed_pending = server.call(
        "handoff_trace_delta",
        json!({ "project_dir": pd, "action": "list" }),
    );
    assert!(listed_pending["deltas"].as_array().unwrap().is_empty());
}

/// create -> edit the body directly -> list (stale=true) -> apply (error) ->
/// apply (force=true).
#[test]
fn stale_delta_blocks_apply_until_forced() {
    let mut server = Server::spawn();
    let (_tmp, pd) = init_project(&mut server, "trace-delta-e2e-stale");

    let created = server.call(
        "handoff_trace_delta",
        json!({
            "project_dir": pd,
            "action": "create",
            "ops": [{"op": "set", "item": "REQ-900", "dev_stage": "in_progress"}],
        }),
    );
    let delta_id = created["delta_id"].as_str().unwrap().to_string();

    // Change REQ-900's body directly (changes its def_hash), independent of
    // the pending delta above.
    server.call(
        "handoff_trace_update",
        json!({
            "project_dir": pd,
            "ops": [{"op": "upsert_item", "doc": "req-delta-e2e", "id": "REQ-900",
                "statement": "A changed requirement."}],
        }),
    );

    let listed = server.call(
        "handoff_trace_delta",
        json!({ "project_dir": pd, "action": "list" }),
    );
    let deltas = listed["deltas"].as_array().unwrap();
    assert_eq!(deltas[0]["delta_id"], delta_id);
    assert_eq!(deltas[0]["stale"], true, "{listed}");

    let (is_error, text) = server.call_raw(
        "handoff_trace_delta",
        json!({ "project_dir": pd, "action": "apply", "delta_id": delta_id }),
    );
    assert!(
        is_error,
        "apply on a stale delta without force must fail: {text}"
    );
    assert!(text.contains("stale"), "{text}");

    let forced = server.call(
        "handoff_trace_delta",
        json!({ "project_dir": pd, "action": "apply", "delta_id": delta_id, "force": true }),
    );
    assert_eq!(forced["applied"].as_array().unwrap().len(), 1, "{forced}");
    let warnings = forced["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("stale")),
        "{forced}"
    );

    let on_disk: Value = serde_json::from_str(
        &std::fs::read_to_string(
            std::path::Path::new(&pd)
                .join(".handoff")
                .join("trace")
                .join("deltas")
                .join(format!("{delta_id}.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(on_disk["status"], "applied");
}

/// A partial apply (`op_indices`) leaves the remainder as a brand-new pending
/// delta, renumbered from 0.
#[test]
fn partial_apply_creates_a_renumbered_remainder_delta() {
    let mut server = Server::spawn();
    let (_tmp, pd) = init_project(&mut server, "trace-delta-e2e-partial");
    server.call(
        "handoff_trace_update",
        json!({
            "project_dir": pd,
            "ops": [{"op": "upsert_item", "doc": "req-delta-e2e", "id": "REQ-901",
                "title": "Second", "statement": "Another requirement."}],
        }),
    );

    let created = server.call(
        "handoff_trace_delta",
        json!({
            "project_dir": pd,
            "action": "create",
            "ops": [
                {"op": "set", "item": "REQ-900", "dev_stage": "in_progress"},
                {"op": "set", "item": "REQ-901", "dev_stage": "in_progress"},
            ],
        }),
    );
    let delta_id = created["delta_id"].as_str().unwrap().to_string();

    let applied = server.call(
        "handoff_trace_delta",
        json!({ "project_dir": pd, "action": "apply", "delta_id": delta_id, "op_indices": [0] }),
    );
    assert_eq!(applied["applied"].as_array().unwrap().len(), 1, "{applied}");
    let remainder_id = applied["remainder_delta_id"]
        .as_str()
        .expect("remainder_delta_id present")
        .to_string();
    assert_ne!(remainder_id, delta_id);

    let original_on_disk: Value = serde_json::from_str(
        &std::fs::read_to_string(
            std::path::Path::new(&pd)
                .join(".handoff")
                .join("trace")
                .join("deltas")
                .join(format!("{delta_id}.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(original_on_disk["status"], "applied");

    let remainder_on_disk: Value = serde_json::from_str(
        &std::fs::read_to_string(
            std::path::Path::new(&pd)
                .join(".handoff")
                .join("trace")
                .join("deltas")
                .join(format!("{remainder_id}.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(remainder_on_disk["status"], "pending");
    let remainder_ops = remainder_on_disk["ops"].as_array().unwrap();
    assert_eq!(remainder_ops.len(), 1);
    assert_eq!(remainder_ops[0]["item"], "REQ-901");
}

/// `reject` marks a delta `rejected` and blocks a later apply.
#[test]
fn reject_marks_rejected_and_blocks_apply() {
    let mut server = Server::spawn();
    let (_tmp, pd) = init_project(&mut server, "trace-delta-e2e-reject");

    let created = server.call(
        "handoff_trace_delta",
        json!({
            "project_dir": pd,
            "action": "create",
            "ops": [{"op": "set", "item": "REQ-900", "dev_stage": "in_progress"}],
        }),
    );
    let delta_id = created["delta_id"].as_str().unwrap().to_string();

    let rejected = server.call(
        "handoff_trace_delta",
        json!({ "project_dir": pd, "action": "reject", "delta_id": delta_id, "reason": "not needed" }),
    );
    assert_eq!(rejected["status"], "rejected", "{rejected}");

    let (is_error, text) = server.call_raw(
        "handoff_trace_delta",
        json!({ "project_dir": pd, "action": "apply", "delta_id": delta_id }),
    );
    assert!(is_error, "apply on a rejected delta must fail: {text}");
}

/// `trace_update(propose=true)` must create a delta rather than writing the
/// document directly.
#[test]
fn trace_update_propose_creates_a_delta_instead_of_writing() {
    let mut server = Server::spawn();
    let (_tmp, pd) = init_project(&mut server, "trace-delta-e2e-propose");

    let proposed = server.call(
        "handoff_trace_update",
        json!({
            "project_dir": pd,
            "propose": true,
            "description": "proposed via trace_update",
            "ops": [{"op": "set", "item": "REQ-900", "dev_stage": "in_progress"}],
        }),
    );
    let delta_id = proposed["delta_id"].as_str().expect("delta_id").to_string();
    assert_eq!(proposed["propose"], true, "{proposed}");

    let listed = server.call(
        "handoff_trace_delta",
        json!({ "project_dir": pd, "action": "list" }),
    );
    let deltas = listed["deltas"].as_array().unwrap();
    assert_eq!(deltas.len(), 1, "{listed}");
    assert_eq!(deltas[0]["delta_id"], delta_id);
    assert_eq!(
        deltas[0]["description"], "proposed via trace_update",
        "{listed}"
    );

    // Nothing was actually written to the document (propose never applies).
    let doc_path = std::path::Path::new(&pd)
        .join(".handoff")
        .join("docs")
        .join("_doc.req-delta-e2e.md");
    let body = std::fs::read_to_string(&doc_path).unwrap();
    assert!(body.contains("REQ-900"), "{body}");
    assert!(body.contains("dev_stage: not_started"), "{body}");
    assert!(!body.contains("dev_stage: in_progress"), "{body}");

    // dry_run + propose together is an error.
    let (is_error, text) = server.call_raw(
        "handoff_trace_update",
        json!({
            "project_dir": pd,
            "dry_run": true,
            "propose": true,
            "ops": [{"op": "set", "item": "REQ-900", "dev_stage": "in_progress"}],
        }),
    );
    assert!(is_error, "dry_run+propose must be rejected: {text}");
}
