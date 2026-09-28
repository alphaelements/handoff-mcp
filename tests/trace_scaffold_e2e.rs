//! Real-binary E2E tests for M2-12 `handoff_trace_scaffold`
//! (wiki/260-vmodel-m2-design.md §4.7): spawns the actual `handoff-mcp`
//! binary and drives it over real stdio JSON-RPC, plus the `trace scaffold`
//! CLI subcommand. Same harness style as `tests/trace_ingest_e2e.rs`.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::{json, Value};

/// Extracts the `---`-fenced YAML frontmatter block from a `_doc.<slug>.md`
/// file and parses it into a generic [`serde_yaml::Value`] for structured
/// field assertions — mirrors `tests/layer_body_notation_e2e.rs`'s helper of
/// the same name. Needed because no read tool exposes the M2 SubItem fields
/// this test checks (`from`) yet (M2-02's own implementation note:
/// `handoff_doc_verify_status` doesn't return `def_hash`/`acceptance`/etc.
/// either, for the same reason).
fn read_frontmatter_yaml(path: &std::path::Path) -> serde_yaml::Value {
    let content = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    let mut lines = content.lines();
    assert_eq!(
        lines.next(),
        Some("---"),
        "document must start with a YAML frontmatter fence: {content}"
    );
    let yaml: String = lines
        .by_ref()
        .take_while(|l| *l != "---")
        .collect::<Vec<_>>()
        .join("\n");
    serde_yaml::from_str(&yaml).unwrap_or_else(|e| panic!("parse frontmatter YAML: {e}\n{yaml}"))
}

fn find_sub_item_yaml<'a>(fm: &'a serde_yaml::Value, stable_id: &str) -> &'a serde_yaml::Mapping {
    fm["verification"]["items"]
        .as_sequence()
        .expect("verification.items sequence")
        .iter()
        .flat_map(|item| {
            item["sub_items"]
                .as_sequence()
                .map_or(&[][..], |s| s.as_slice())
        })
        .find(|sub| sub["stable_id"].as_str() == Some(stable_id))
        .unwrap_or_else(|| panic!("stable_id {stable_id} not found in frontmatter: {fm:?}"))
        .as_mapping()
        .expect("sub_item is a mapping")
}

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

/// Runs the CLI binary directly (no shell) and returns (stdout, stderr, exit
/// code).
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

fn build_project(server: &mut Server, dir: &std::path::Path) {
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "trace-scaffold-e2e" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "req-scaffold-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n\
                ### REQ-003 Account lockout\n\n\
                - priority: P1\n\n\
                5 consecutive failed logins lock the account for 15 minutes.\n\n\
                受入基準:\n\
                - AC1: Given the account already failed 4 times When it fails a 5th time Then the account is locked\n\
                - AC2: WHEN the account is locked THE SYSTEM SHALL reject even a correct password\n",
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "at-scaffold-e2e",
            "title": "Acceptance tests",
            "layer": "acceptance",
            "body": "# Acceptance\n",
        }),
    );
}

/// Core scenario: `mode="apply"` generates one item per AC (a `gwt`-kind
/// AC1 splits into steps/expected-result; an `ears`-kind AC2 uses the full
/// text as its expected result and a placeholder for steps), writes them
/// into `target_doc` via the real parser + layer sync, and the round-trip
/// (`doc_get` reading the item back) reproduces the same `verifies`/`from`
/// attributes (§4.7: "描画 → 解析の往復で同じ項目になる").
#[test]
fn trace_scaffold_apply_generates_one_item_per_acceptance_criterion() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut server = Server::spawn();
    build_project(&mut server, dir.path());

    let out = server.call(
        "handoff_trace_scaffold",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "items": ["REQ-003"],
            "target_doc": "at-scaffold-e2e",
            "mode": "apply",
        }),
    );
    assert_eq!(out["applied"], true, "{out}");
    let generated = out["generated"].as_array().unwrap();
    assert_eq!(generated.len(), 2, "{out}");
    assert_eq!(generated[0]["id"], "AT-REQ-003-1");
    assert_eq!(generated[0]["from"], "REQ-003#AC1");
    assert_eq!(generated[1]["id"], "AT-REQ-003-2");
    assert_eq!(generated[1]["from"], "REQ-003#AC2");
    assert!(out["skipped"].as_array().unwrap().is_empty(), "{out}");

    // Round-trip: re-reading the target document's persisted SubItems (via
    // its on-disk frontmatter — no read tool exposes `from`/`acceptance`
    // yet, see this file's `read_frontmatter_yaml` doc comment) shows the
    // same verifies/from attributes the scaffold call reported, i.e.
    // render -> parse reproduced the same item (§4.7).
    let doc_path = dir.path().join(".handoff/docs/_doc.at-scaffold-e2e.md");
    let fm = read_frontmatter_yaml(&doc_path);
    let ac1 = find_sub_item_yaml(&fm, "AT-REQ-003-1");
    assert_eq!(
        ac1["verifies"].as_sequence().unwrap()[0].as_str(),
        Some("REQ-003")
    );
    assert_eq!(ac1["from"].as_str(), Some("REQ-003#AC1"));
    assert_eq!(ac1["method"].as_str(), Some("manual"));

    let ac2 = find_sub_item_yaml(&fm, "AT-REQ-003-2");
    assert_eq!(ac2["from"].as_str(), Some("REQ-003#AC2"));
}

/// Idempotency (§4.7): applying the same `items`/`target_doc` a second time
/// generates nothing new — every AC already has a scaffolded item (matched
/// by `from`), so both are reported as `skipped`, not duplicated.
#[test]
fn trace_scaffold_apply_is_idempotent_on_a_second_call() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut server = Server::spawn();
    build_project(&mut server, dir.path());

    let first = server.call(
        "handoff_trace_scaffold",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "items": ["REQ-003"],
            "target_doc": "at-scaffold-e2e",
            "mode": "apply",
        }),
    );
    assert_eq!(first["generated"].as_array().unwrap().len(), 2, "{first}");

    let second = server.call(
        "handoff_trace_scaffold",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "items": ["REQ-003"],
            "target_doc": "at-scaffold-e2e",
            "mode": "apply",
        }),
    );
    assert_eq!(second["applied"], false, "{second}");
    assert!(
        second["generated"].as_array().unwrap().is_empty(),
        "{second}"
    );
    let skipped = second["skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 2, "{second}");
    assert!(skipped
        .iter()
        .any(|s| s["ac"] == "REQ-003#AC1" && s["existing"] == "AT-REQ-003-1"));
    assert!(skipped
        .iter()
        .any(|s| s["ac"] == "REQ-003#AC2" && s["existing"] == "AT-REQ-003-2"));
}

/// ID collision avoidance (§4.7): if the target id is already used by an
/// unrelated existing item, the scaffold falls back to a lettered suffix.
#[test]
fn trace_scaffold_avoids_id_collision_with_a_lettered_suffix() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.path().to_string_lossy(), "project_name": "trace-scaffold-collision-e2e" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "slug": "req-collision-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n\
                ### REQ-010 Something\n\n\
                Statement.\n\n\
                受入基準:\n\
                - AC1: The system does the thing\n",
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "slug": "at-collision-e2e",
            "title": "Acceptance tests",
            "layer": "acceptance",
            // Pre-existing item that collides with the scaffold's would-be id.
            "body": "# Acceptance\n\n### AT-REQ-010-1 Unrelated pre-existing item\n\n- method: manual\n\nAlready here.\n",
        }),
    );

    let out = server.call(
        "handoff_trace_scaffold",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "items": ["REQ-010"],
            "target_doc": "at-collision-e2e",
            "mode": "apply",
        }),
    );
    let generated = out["generated"].as_array().unwrap();
    assert_eq!(generated.len(), 1, "{out}");
    assert_eq!(generated[0]["id"], "AT-REQ-010-1a", "{out}");
}

/// `doc` mode scaffolds every acceptance-bearing item in a source document
/// (not just an explicit `items` list), and `mode="preview"` (the default)
/// computes the same result without writing anything.
#[test]
fn trace_scaffold_doc_mode_preview_does_not_write() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut server = Server::spawn();
    build_project(&mut server, dir.path());

    let out = server.call(
        "handoff_trace_scaffold",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "doc": "req-scaffold-e2e",
            "target_doc": "at-scaffold-e2e",
        }),
    );
    assert_eq!(out["mode"], "preview");
    assert_eq!(out["applied"], false, "{out}");
    assert_eq!(out["generated"].as_array().unwrap().len(), 2, "{out}");

    // Nothing was actually written: the target document's body on disk is
    // unchanged (still just its preamble, no appended item headings).
    let body_path = dir.path().join(".handoff/docs/_doc.at-scaffold-e2e.md");
    let body = std::fs::read_to_string(&body_path).unwrap();
    assert!(
        !body.contains("AT-REQ-003"),
        "preview mode must not write anything: {body}"
    );
}

/// `target_doc` must be a layer document — a document with no `layer` set
/// cannot supply a "検証層の既定接頭辞" (default id prefix) to generate from.
#[test]
fn trace_scaffold_requires_target_doc_to_be_a_layer_document() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut server = Server::spawn();
    build_project(&mut server, dir.path());
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "slug": "not-a-layer-doc",
            "title": "Notes",
            "body": "# Notes\n",
        }),
    );

    let (is_error, text) = server.call_raw(
        "handoff_trace_scaffold",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "items": ["REQ-003"],
            "target_doc": "not-a-layer-doc",
            "mode": "preview",
        }),
    );
    assert!(is_error, "{text}");
}

#[test]
fn cli_trace_scaffold_previews_without_a_shell() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut server = Server::spawn();
    build_project(&mut server, dir.path());
    drop(server);

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "scaffold",
        "--project-dir",
        dir.path().to_str().unwrap(),
        "--items",
        "REQ-003",
        "--target-doc",
        "at-scaffold-e2e",
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["mode"], "preview");
    assert_eq!(parsed["generated"].as_array().unwrap().len(), 2, "{parsed}");
}

/// Registration check (mirrors `task_checklist_appears_in_tools_list`):
/// `handoff_trace_scaffold` is discoverable via `tools/list`, confirming its
/// `ToolDefinition` in `src/mcp/tools.rs` is actually wired up.
#[test]
fn trace_scaffold_appears_in_tools_list() {
    let mut server = Server::spawn();
    let req = json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "tools/list",
        "params": {}
    });
    writeln!(server.stdin, "{req}").expect("write to server stdin");
    server.stdin.flush().expect("flush server stdin");
    let line = server
        .lines
        .recv_timeout(Duration::from_secs(10))
        .expect("tools/list response");
    let resp: Value = serde_json::from_str(&line).expect("valid JSON-RPC response");
    let tools = resp["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == "handoff_trace_scaffold"));
}
