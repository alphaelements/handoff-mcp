//! Real-binary MCP-tool E2E for `needs` (wiki/270-vmodel-m3-design.md
//! §2.1/§3.1, M3-01, FR-202): a layer item's `- needs: <id>[,<id>...]`
//! attribute line gates which verifier layers count toward its
//! `handoff_trace_report` coverage, and a verifier from a layer outside
//! `needs` surfaces as an `unwanted_coverage` finding via `handoff_trace_lint`.

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

/// A `requirement` item with `- needs: acceptance` is `covered` once an
/// `acceptance`-layer verifier exists, but a `system_test`-layer verifier
/// targeting the same item neither contributes to its coverage nor its
/// `unverified` gap disappearing on its own — it instead surfaces as an
/// `unwanted_coverage` lint finding (info by default).
#[test]
fn needs_gates_trace_report_coverage_and_lint_reports_unwanted_coverage() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "trace-needs-e2e" }),
    );

    // Use the "full" built-in profile so basic_spec/detailed_spec/
    // system_test/unit_test are all in scope, not just the auto-detected
    // requirement/acceptance pair.
    let config_path = dir.join(".handoff").join("config.toml");
    let mut config = handoff_mcp::storage::config::read_config(&config_path).expect("read config");
    config.trace.profile = Some("full".to_string());
    handoff_mcp::storage::config::write_config(&config_path, &config).expect("write config");

    let req_doc = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "req-needs-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-900 Needs acceptance only\n\n\
                - needs: acceptance, system_test\n\nBody.\n",
        }),
    );
    let req_doc_id = req_doc["doc_id"].as_str().expect("doc_id").to_string();
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "at-needs-e2e",
            "title": "Acceptance",
            "layer": "acceptance",
            "body": "# Acceptance\n\n### AT-900 Confirms REQ-900\n\n\
                - verifies: REQ-900\n- method: manual\n\nBody.\n",
        }),
    );

    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": pd, "include_items": true }),
    );
    let items = report["items"].as_array().expect("items array");
    let req900 = items
        .iter()
        .find(|it| it["id"] == "REQ-900")
        .expect("REQ-900 present in trace_report items");
    assert_eq!(
        req900["coverage"]["horizontal"], "covered",
        "needs: [acceptance, system_test] is satisfied by the acceptance verifier: {req900}"
    );

    // Now add a system_test verifier too — needs already includes
    // system_test, so this does NOT trigger unwanted_coverage.
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "st-needs-e2e",
            "title": "System test",
            "layer": "system_test",
            "body": "# System test\n\n### ST-900 Confirms REQ-900\n\n\
                - verifies: REQ-900\n- method: manual\n\nBody.\n",
        }),
    );
    let lint_both_wanted = server.call("handoff_trace_lint", json!({ "project_dir": pd }));
    assert!(
        !lint_both_wanted["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["rule"] == "unwanted_coverage"),
        "both acceptance and system_test are in needs, no unwanted_coverage expected: {lint_both_wanted}"
    );

    // Replace needs with just acceptance: the existing system_test verifier
    // (ST-900) is now outside needs and must surface as unwanted_coverage.
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "doc_id": req_doc_id,
            "body": "# Requirements\n\n### REQ-900 Needs acceptance only\n\n\
                - needs: acceptance\n\nBody.\n",
        }),
    );
    let lint = server.call("handoff_trace_lint", json!({ "project_dir": pd }));
    let findings = lint["findings"].as_array().unwrap();
    let finding = findings
        .iter()
        .find(|f| f["rule"] == "unwanted_coverage" && f["item"] == "REQ-900")
        .unwrap_or_else(|| panic!("expected an unwanted_coverage finding for REQ-900: {lint}"));
    assert_eq!(finding["severity"], "info");
    assert!(
        finding["message"].as_str().unwrap().contains("ST-900"),
        "{finding}"
    );

    // REQ-900's own coverage is still `covered` (the acceptance verifier
    // alone still satisfies the narrowed needs set).
    let report2 = server.call(
        "handoff_trace_report",
        json!({ "project_dir": pd, "include_items": true }),
    );
    let items2 = report2["items"].as_array().expect("items array");
    let req900_after = items2
        .iter()
        .find(|it| it["id"] == "REQ-900")
        .expect("REQ-900 present");
    assert_eq!(req900_after["coverage"]["horizontal"], "covered");
}

/// An empty `- needs:` line (the §2.1 3-state "explicitly no coverage
/// required") reports `waived`, not `uncovered`, even with zero verifiers.
#[test]
fn empty_needs_waives_coverage_with_no_verifiers() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "trace-needs-empty-e2e" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "req-needs-empty-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-901 No coverage required\n\n\
                - needs:\n\nBody.\n",
        }),
    );

    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": pd, "include_items": true }),
    );
    let items = report["items"].as_array().expect("items array");
    let req901 = items
        .iter()
        .find(|it| it["id"] == "REQ-901")
        .expect("REQ-901 present");
    assert_eq!(req901["coverage"]["horizontal"], "waived", "{req901}");

    let lint = server.call("handoff_trace_lint", json!({ "project_dir": pd }));
    assert!(
        !lint["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["rule"] == "unverified" && f["item"] == "REQ-901"),
        "an explicitly-exempt item must not also trigger unverified: {lint}"
    );
}
