//! Real-binary E2E for the requirement-tree follow-ups (wiki/300, SPEC-TREE-011
//! to 014): `trace_propose(parent=...)` (MCP and CLI), `orphaned_children` on
//! `doc_save` / `trace_update`, and the decomposition edge in `trace_slice` /
//! `trace_matrix`. Drives the actual `handoff-mcp` binary over stdio JSON-RPC.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use handoff_mcp::storage::config::{read_config, write_config};
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

fn run_cli(args: &[&str]) -> (String, String, i32) {
    let output = Command::new(binary())
        .args(args)
        .output()
        .expect("failed to run binary");
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
        output.status.code().unwrap_or(-1),
    )
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

const TREE_BODY: &str = "# Requirements\n\n\
    ### REQ-001 Root\n\nThe root.\n\n\
    ### REQ-002 Other root\n\nAnother.\n\n\
    ### REQ-001-001 Only child\n\n- parent: REQ-001\n\nC.\n\n\
    ### REQ-001-002 Shared child\n\n- parent: REQ-001, REQ-002\n\nS.\n";

fn build_tree_project(server: &mut Server, dir: &std::path::Path) -> String {
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "tree-followups-e2e" }),
    );
    let config_path = dir.join(".handoff").join("config.toml");
    let mut config = read_config(&config_path).expect("read config");
    config.trace.profile = Some("minimal".to_string());
    write_config(&config_path, &config).expect("write config");
    let saved = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "req-tree",
            "title": "Requirement tree",
            "doc_type": "spec",
            "layer": "requirement",
            "body": TREE_BODY,
        }),
    );
    saved["doc_id"].as_str().unwrap().to_string()
}

#[test]
fn propose_with_parent_allocates_the_next_child_id_over_mcp_and_cli() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    build_tree_project(&mut server, &dir);

    let out = server.call(
        "handoff_trace_propose",
        json!({ "project_dir": dir.to_string_lossy(), "title": "Third child", "parent": "REQ-001" }),
    );
    assert_eq!(out["proposal"]["next_ids"][0], "REQ-001-003", "{out}");
    assert_eq!(out["proposal"]["doc"], "req-tree", "{out}");
    assert!(
        out["proposal"]["markdown"]
            .as_str()
            .unwrap()
            .contains("- parent: REQ-001"),
        "{out}"
    );

    let (_, text) = server.call_raw(
        "handoff_trace_propose",
        json!({ "project_dir": dir.to_string_lossy(), "title": "x", "parent": "REQ-404" }),
    );
    assert!(text.contains("REQ-404"), "{text}");
    drop(server);

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "propose",
        "--project-dir",
        dir.to_str().unwrap(),
        "--title",
        "Third child",
        "--parent",
        "REQ-001",
    ]);
    assert_eq!(code, 0, "stdout={stdout}\nstderr={stderr}");
    let cli: Value = serde_json::from_str(&stdout).expect("valid JSON stdout");
    assert_eq!(cli["proposal"]["next_ids"][0], "REQ-001-003", "{cli}");
}

#[test]
fn removing_a_parent_reports_orphaned_children_but_not_dag_children_with_another_parent() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    let doc_id = build_tree_project(&mut server, &dir);

    let without_root = TREE_BODY.replace("### REQ-001 Root\n\nThe root.\n\n", "");
    let out = server.call(
        "handoff_doc_save",
        json!({ "project_dir": dir.to_string_lossy(), "doc_id": doc_id, "body": without_root }),
    );
    // REQ-001-001's only parent is gone; REQ-001-002 still has REQ-002.
    assert_eq!(out["orphaned_children"], json!(["REQ-001-001"]), "{out}");

    let upsert = server.call(
        "handoff_trace_update",
        json!({ "project_dir": dir.to_string_lossy(), "ops": [
            {"op": "upsert_item", "doc": "req-tree", "id": "REQ-002", "priority": "P1",
             "attrs": {"priority": "P1"}}
        ]}),
    );
    assert_eq!(upsert["orphaned_children"], json!([]), "{upsert}");
}

#[test]
fn slice_and_matrix_follow_the_decomposition_edges() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    build_tree_project(&mut server, &dir);

    let slice = server.call(
        "handoff_trace_slice",
        json!({ "project_dir": dir.to_string_lossy(), "item": "REQ-001", "direction": "down" }),
    );
    let ids: Vec<&str> = slice["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec!["REQ-001", "REQ-001-001", "REQ-001-002"],
        "{slice}"
    );
    assert_eq!(slice["items"][1]["via"], "parent", "{slice}");
    assert_eq!(
        slice["items"][2]["parents"],
        json!(["REQ-001", "REQ-002"]),
        "{slice}"
    );

    let edges = server.call(
        "handoff_trace_matrix",
        json!({ "project_dir": dir.to_string_lossy(), "format": "csv", "shape": "edges" }),
    );
    let csv = edges["content"].as_str().unwrap();
    assert!(
        csv.contains("\"REQ-001-002\",\"REQ-002\",\"parent\""),
        "{csv}"
    );

    let tree = server.call(
        "handoff_trace_matrix",
        json!({ "project_dir": dir.to_string_lossy(), "format": "markdown", "shape": "tree" }),
    );
    let md = tree["content"].as_str().unwrap();
    assert!(md.contains("| item |"), "{md}");
    assert!(md.contains("&nbsp;&nbsp;REQ-001-002*"), "{md}");
}
