//! Real-binary MCP-tool E2E for layer lifecycle statuses (FR-510 / SPEC-510):
//! `_trace_report.json`'s `layer_statuses` / `project_status`, the
//! `handoff_trace_update` `set_layer_status` approval op, and automatic
//! demotion.

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
        json!({ "project_dir": pd, "project_name": "layer-status-e2e" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "requirements-layer-status",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-910 Something\n\nBody.\n",
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "acceptance-layer-status",
            "title": "Acceptance",
            "layer": "acceptance",
            "body": "# Acceptance\n\n### AT-910 Verify REQ-910\n\n- verifies: REQ-910\n\nSteps.\n",
        }),
    );
    pd
}

fn persisted_report(dir: &Path) -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(dir.join(".handoff/docs/_trace_report.json")).unwrap(),
    )
    .unwrap()
}

fn record(server: &mut Server, pd: &str, result: &str) {
    let out = server.call(
        "handoff_trace_update",
        json!({"project_dir": pd, "ops": [{"op": "record", "item": "AT-910", "result": result}]}),
    );
    assert!(out.get("failed").is_none(), "{out}");
}

fn set_layer_status(server: &mut Server, pd: &str, layer: &str, status: &str) -> Value {
    server.call(
        "handoff_trace_update",
        json!({"project_dir": pd, "executor_kind": "human", "executor_id": "reviewer",
            "ops": [{"op": "set_layer_status", "layer": layer, "status": status}]}),
    )
}

#[test]
fn report_lists_derived_layer_statuses_and_project_status() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let mut server = Server::spawn();
    let pd = build_project(&mut server, tmp.path());

    let resp = server.call("handoff_trace_report", json!({"project_dir": pd}));
    assert_eq!(
        resp["layer_statuses"]["acceptance"], "in_progress",
        "{resp}"
    );
    assert_eq!(resp["project_status"], "in_progress");

    record(&mut server, &pd, "pass");
    server.call("handoff_trace_report", json!({"project_dir": pd}));
    let p = persisted_report(tmp.path());
    assert_eq!(p["layer_statuses"]["acceptance"], "verified", "{p}");
    assert_eq!(p["layer_statuses"]["requirement"], "in_progress", "{p}");
    assert_eq!(p["project_status"], "in_progress");
}

#[test]
fn approval_is_refused_until_verified_then_persists_and_is_demoted_on_failure() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let mut server = Server::spawn();
    let pd = build_project(&mut server, tmp.path());
    let status_file = tmp.path().join(".handoff/trace/layer_status.json");

    // Not verified yet -> refused, nothing written.
    let refused = set_layer_status(&mut server, &pd, "acceptance", "approved");
    assert!(
        refused["failed"]["error"]
            .as_str()
            .unwrap()
            .contains("only a verified layer"),
        "{refused}"
    );
    assert!(!status_file.exists());

    record(&mut server, &pd, "pass");
    let ok = set_layer_status(&mut server, &pd, "acceptance", "approved");
    assert!(ok.get("failed").is_none(), "{ok}");
    assert!(status_file.exists());
    let recorded: Value =
        serde_json::from_str(&std::fs::read_to_string(&status_file).unwrap()).unwrap();
    assert_eq!(recorded["layers"]["acceptance"]["status"], "approved");
    assert_eq!(
        recorded["layers"]["acceptance"]["executor"]["kind"],
        "human"
    );
    assert_eq!(
        recorded["layers"]["acceptance"]["executor"]["id"],
        "reviewer"
    );

    // The approval op already refreshed _trace_report.json.
    assert_eq!(
        persisted_report(tmp.path())["layer_statuses"]["acceptance"],
        "approved"
    );

    // A failing result demotes the layer and drops the record.
    record(&mut server, &pd, "fail");
    let resp = server.call("handoff_trace_report", json!({"project_dir": pd}));
    assert_eq!(
        resp["layer_statuses"]["acceptance"], "in_progress",
        "{resp}"
    );
    assert!(!status_file.exists(), "demoted record must be dropped");

    // ...and a later fix does not silently re-approve it.
    record(&mut server, &pd, "pass");
    let resp = server.call("handoff_trace_report", json!({"project_dir": pd}));
    assert_eq!(resp["layer_statuses"]["acceptance"], "verified", "{resp}");
}

#[test]
fn reset_returns_the_layer_to_its_derived_status() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let mut server = Server::spawn();
    let pd = build_project(&mut server, tmp.path());
    record(&mut server, &pd, "pass");
    set_layer_status(&mut server, &pd, "acceptance", "under_review");
    assert_eq!(
        persisted_report(tmp.path())["layer_statuses"]["acceptance"],
        "under_review"
    );
    let out = set_layer_status(&mut server, &pd, "acceptance", "reset");
    assert!(out.get("failed").is_none(), "{out}");
    assert_eq!(
        persisted_report(tmp.path())["layer_statuses"]["acceptance"],
        "verified"
    );
}

#[test]
fn cli_trace_update_accepts_set_layer_status() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let mut server = Server::spawn();
    let pd = build_project(&mut server, tmp.path());
    record(&mut server, &pd, "pass");
    drop(server);

    let output = Command::new(binary())
        .args([
            "trace",
            "update",
            "--project-dir",
            &pd,
            "--ops",
            r#"[{"op":"set_layer_status","layer":"acceptance","status":"approved"}]"#,
        ])
        .output()
        .expect("failed to run binary");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        persisted_report(tmp.path())["layer_statuses"]["acceptance"],
        "approved"
    );
}
