//! Real-binary MCP-tool E2E for the V-model bug flow (FR-522 / SPEC-522):
//! verification failure -> automatic layer demotion -> bug task linked to the
//! requirement item -> fix -> suspect/reverify -> re-verification -> layer
//! recovery, plus the structured waiver record (`set.waive_reason` /
//! `waive_approved_by`).

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

fn build_project(server: &mut Server, dir: &Path) -> String {
    let pd = dir.to_string_lossy().to_string();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "bug-flow-e2e" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "requirements-bug-flow",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-920 Something\n\nBody.\n",
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "acceptance-bug-flow",
            "title": "Acceptance",
            "layer": "acceptance",
            "body": "# Acceptance\n\n### AT-920 Verify REQ-920\n\n- verifies: REQ-920\n\nSteps.\n",
        }),
    );
    pd
}

fn record(server: &mut Server, pd: &str, result: &str) {
    let out = server.call(
        "handoff_trace_update",
        json!({"project_dir": pd, "ops": [{"op": "record", "item": "AT-920", "result": result}]}),
    );
    assert!(out.get("failed").is_none(), "{out}");
}

fn report(server: &mut Server, pd: &str) -> Value {
    server.call(
        "handoff_trace_report",
        json!({"project_dir": pd, "include_items": true}),
    )
}

fn item<'a>(report: &'a Value, id: &str) -> &'a Value {
    report["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|i| i["id"] == id)
        .unwrap_or_else(|| panic!("item {id} missing: {report}"))
}

#[test]
fn failure_demotes_bug_links_fix_suspects_and_reverification_restores_the_layer() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let mut server = Server::spawn();
    let pd = build_project(&mut server, tmp.path());

    // Baseline: verified.
    record(&mut server, &pd, "pass");
    let r = report(&mut server, &pd);
    assert_eq!(r["layer_statuses"]["acceptance"], "verified", "{r}");

    // 1. A failing verification demotes the layer automatically.
    record(&mut server, &pd, "fail");
    let r = report(&mut server, &pd);
    assert_eq!(r["layer_statuses"]["acceptance"], "in_progress", "{r}");
    assert_eq!(item(&r, "AT-920")["state"], "failing", "{r}");

    // 2. The bug task is an ordinary task labelled `bug`, linked to the
    //    failing item through requirement_ids.
    server.call(
        "handoff_update_task",
        json!({"project_dir": pd, "task": {
            "id": "bug1", "title": "AT-920 fails", "status": "todo",
            "labels": ["bug", "bug:fix"], "requirement_ids": ["AT-920"]}}),
    );
    let r = report(&mut server, &pd);
    let tasks = item(&r, "AT-920")["tasks"].as_array().unwrap().clone();
    assert!(
        tasks.iter().any(|t| t["id"] == "bug1"),
        "bug task must be linked to AT-920: {tasks:?}"
    );
    let task = server.call(
        "handoff_get_task",
        json!({"project_dir": pd, "task_id": "bug1"}),
    );
    assert!(task.to_string().contains("\"bug\""), "{task}");

    // 3. The fix changes the upstream requirement: the verifies link on
    //    AT-920 turns suspect.
    let fixed = server.call(
        "handoff_trace_update",
        json!({"project_dir": pd, "ops": [{"op": "upsert_item", "doc": "requirements-bug-flow",
            "id": "REQ-920", "statement": "Body, corrected by the fix."}]}),
    );
    assert!(fixed.get("failed").is_none(), "{fixed}");
    // 4. Re-verification passes, but the stale link keeps it flagged reverify.
    record(&mut server, &pd, "pass");
    let r = report(&mut server, &pd);
    let at = item(&r, "AT-920");
    assert_eq!(at["state"], "passing", "{r}");
    assert!(
        !at["suspect"].as_array().unwrap().is_empty(),
        "fix must trigger a suspect: {at}"
    );
    assert_eq!(at["reverify"], true, "{at}");

    // 5. Clearing the suspect with a reason completes the cycle.
    let cleared = server.call(
        "handoff_trace_update",
        json!({"project_dir": pd, "ops": [{"op": "clear_suspect", "item": "AT-920",
            "reason": "fix reviewed, bug1"}]}),
    );
    assert!(cleared.get("failed").is_none(), "{cleared}");
    let r = report(&mut server, &pd);
    let at = item(&r, "AT-920");
    assert_eq!(at["reverify"], false, "{at}");
    assert_eq!(r["layer_statuses"]["acceptance"], "verified", "{r}");
}

#[test]
fn waive_record_is_stored_structured_and_surfaced_in_the_trace_report() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let mut server = Server::spawn();
    let pd = build_project(&mut server, tmp.path());

    let out = server.call(
        "handoff_trace_update",
        json!({"project_dir": pd, "ops": [{"op": "set", "item": "AT-920",
            "waive_reason": "cosmetic defect, accepted for this release",
            "waive_approved_by": "product-owner"}]}),
    );
    assert!(out.get("failed").is_none(), "{out}");
    assert!(
        out["warnings"].to_string().contains("waiver_added: AT-920"),
        "{out}"
    );

    let r = report(&mut server, &pd);
    let at = item(&r, "AT-920");
    assert_eq!(
        at["waive"]["reason"],
        "cosmetic defect, accepted for this release"
    );
    assert_eq!(at["waive"]["approved_by"], "product-owner");
    assert!(item(&r, "REQ-920").get("waive").is_none());

    // Clearing both fields removes it again.
    let out = server.call(
        "handoff_trace_update",
        json!({"project_dir": pd, "ops": [{"op": "set", "item": "AT-920",
            "waive_reason": null, "waive_approved_by": null}]}),
    );
    assert!(out.get("failed").is_none(), "{out}");
    assert!(item(&report(&mut server, &pd), "AT-920")
        .get("waive")
        .is_none());
}

#[test]
fn waive_with_only_one_of_the_two_fields_is_refused() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let mut server = Server::spawn();
    let pd = build_project(&mut server, tmp.path());
    let out = server.call(
        "handoff_trace_update",
        json!({"project_dir": pd, "ops": [{"op": "set", "item": "AT-920",
            "waive_reason": "no approver"}]}),
    );
    assert!(
        out["failed"]["error"]
            .as_str()
            .unwrap()
            .contains("together"),
        "{out}"
    );
    assert!(item(&report(&mut server, &pd), "AT-920")
        .get("waive")
        .is_none());
}
