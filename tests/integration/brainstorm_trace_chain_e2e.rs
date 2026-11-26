//! Real-binary E2E for the seam between the brainstorm tools and the
//! `brainstorm` trace layer (wiki/360 §9): the real output of
//! `handoff_brainstorm_finalize` must be a layer document whose `BS-*` items a
//! requirement can `refines`, and `handoff_trace_report` must show the chain.
//! Unlike `layer_registry_profile_e2e.rs`, no document is hand-written here.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};

const BINARY: &str = env!("CARGO_BIN_EXE_handoff-mcp");

struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Session {
    fn spawn() -> Self {
        let mut child = Command::new(BINARY)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn handoff-mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Session {
            child,
            stdin,
            stdout,
            next_id: 1,
        }
    }

    fn ok(&mut self, project: &Path, name: &str, mut arguments: Value) -> Value {
        arguments["project_dir"] = json!(project.to_string_lossy());
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        });
        writeln!(self.stdin, "{req}").expect("write request");
        self.stdin.flush().expect("flush");
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read response");
        let resp: Value = serde_json::from_str(line.trim()).expect("response is JSON");
        assert_eq!(resp["id"], id, "response id mismatch: {resp}");
        let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            !resp["result"]["isError"].as_bool().unwrap_or(false),
            "{name} failed: {text}"
        );
        // Setup tools (init, update_config) answer in plain text; the tools
        // under test answer in JSON and are asserted on structurally.
        serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string()))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Declares the `brainstorm` origin layer and a profile that includes it,
/// exactly as `requirement-loop` Phase 0 instructs (through
/// `handoff_update_config`).
fn declare_brainstorm_layer(s: &mut Session, p: &Path) {
    s.ok(
        p,
        "handoff_update_config",
        json!({ "updates": {
            "trace.layer": [{
                "id": "brainstorm",
                "name": "ブレインストーミング",
                "side": "left",
                "level": 0,
                "verification": false,
                "id_prefixes": ["BS"]
            }],
            "trace.profiles.with-brainstorm.extends": "minimal",
            "trace.profiles.with-brainstorm.layers":
                ["brainstorm", "requirement", "acceptance"],
            "trace.profile": "with-brainstorm"
        }}),
    );
}

fn run_brainstorm(s: &mut Session, p: &Path) -> Value {
    let started = s.ok(
        p,
        "handoff_brainstorm_start",
        json!({ "topic": "IoT 温度監視システム" }),
    );
    let id = started["brainstorm_id"].as_str().unwrap().to_string();
    s.ok(
        p,
        "handoff_brainstorm_turn",
        json!({
            "brainstorm_id": id, "topic_tag": "scope",
            "summary": "スコープを確定", "decisions": ["温度ログとアラートに絞る"]
        }),
    );
    s.ok(
        p,
        "handoff_brainstorm_finalize",
        json!({
            "brainstorm_id": id,
            "executive_summary": "冷蔵庫の温度を遠隔監視する",
            "scope_in": ["温度ログ", "アラート通知"],
            "priorities": ["アラート通知"],
            "requirement_hints": ["通知遅延は5分以内"]
        }),
    )
}

#[test]
fn finalize_output_chains_into_requirements_and_trace_report() {
    let mut s = Session::spawn();
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path();
    s.ok(p, "handoff_init", json!({ "project_name": "bs-chain-e2e" }));
    declare_brainstorm_layer(&mut s, p);

    let fin = run_brainstorm(&mut s, p);
    assert_eq!(fin["layer"], "brainstorm", "{fin}");
    // The only warning is the unfinished coverage; nothing about the layer.
    assert!(
        !fin["warnings"].to_string().contains("trace.layer"),
        "no layer warning when the layer is declared: {fin}"
    );
    let report_id = fin["report_doc_id"].as_str().unwrap();
    let item_ids = fin["item_ids"].as_array().expect("item_ids in response");
    assert!(item_ids.contains(&json!("BS-SCOPE-001")), "{fin}");
    assert!(item_ids.contains(&json!("BS-SCOPE-002")), "{fin}");

    // The report is a real layer document straight out of finalize.
    let doc = s.ok(p, "handoff_doc_get", json!({ "doc_id": report_id }));
    assert_eq!(doc["layer"], "brainstorm", "{doc}");

    // A requirement refines a BS item that finalize itself produced.
    s.ok(
        p,
        "handoff_doc_save",
        json!({
            "slug": "reqs-from-brainstorm",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-001 Alert notification\n\n- refines: BS-SCOPE-002\n\nAlerts are delivered.\n",
        }),
    );

    let report = s.ok(p, "handoff_trace_report", json!({ "include_items": true }));
    assert_eq!(
        report["trace_layers"]["in_use"],
        json!(["brainstorm", "requirement", "acceptance"]),
        "{report}"
    );
    let items = report["items"].as_array().expect("items");
    let find = |id: &str| {
        items
            .iter()
            .find(|i| i["id"] == id)
            .unwrap_or_else(|| panic!("{id} missing from trace_report items: {report}"))
    };
    let scope = find("BS-SCOPE-002");
    assert_eq!(scope["layer"], "brainstorm");
    assert_eq!(scope["coverage"]["horizontal"], "na");
    assert_eq!(find("REQ-001")["refines"], json!(["BS-SCOPE-002"]));
    assert_eq!(find("BS-SCOPE-001")["layer"], "brainstorm");
    let gaps = report["gaps"].as_array().expect("gaps");
    assert!(
        gaps.iter().all(|g| g["kind"] != "invalid_link"),
        "refines edge must be valid: {gaps:?}"
    );
}

#[test]
fn finalize_without_a_declared_layer_warns_and_leaves_the_report_layerless() {
    let mut s = Session::spawn();
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path();
    s.ok(
        p,
        "handoff_init",
        json!({ "project_name": "bs-nolayer-e2e" }),
    );

    let fin = run_brainstorm(&mut s, p);
    assert!(fin["layer"].is_null(), "{fin}");
    assert!(
        fin["warnings"].to_string().contains("trace.layer"),
        "warning must say how to declare the layer: {fin}"
    );
    let doc = s.ok(
        p,
        "handoff_doc_get",
        json!({ "doc_id": fin["report_doc_id"] }),
    );
    assert!(doc["layer"].is_null(), "{doc}");
    assert!(
        doc["body"].as_str().unwrap().contains("BS-SCOPE-001"),
        "BS ids are rendered even without the layer: {doc}"
    );
}
