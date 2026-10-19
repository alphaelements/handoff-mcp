//! Real-binary MCP-tool E2E for `handoff_trace_lint(action="nfr_check")`
//! (SPEC-RQ-001: ISO/IEC 25010 NFR checklist, persisted into
//! `_trace_report.json`'s `nfr_coverage`) and the `[trace.requirement_gate]`
//! config section (SPEC-RQ-010) read through `handoff_get_config`.

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

const CATEGORIES: [&str; 8] = [
    "performance_efficiency",
    "compatibility",
    "usability",
    "reliability",
    "security",
    "maintainability",
    "portability",
    "functional_suitability",
];

fn new_project(server: &mut Server, name: &str) -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": name }),
    );
    (tmp, pd)
}

fn save_requirements(server: &mut Server, pd: &str) {
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "req-nfr-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n\
                ### REQ-900 Users can export a report\n\nBody.\n\n\
                ### NFR-SECURITY-901 All traffic is encrypted\n\nBody.\n\n\
                ### NFR-902 Response time stays under 200 ms\n\nBody.\n\n\
                ### NFR-903 The system shall be nice\n\nBody.\n",
        }),
    );
}

fn category<'a>(out: &'a Value, name: &str) -> &'a Value {
    out["categories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("category {name} missing: {out}"))
}

fn ids(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

fn read_report(pd: &str) -> Value {
    let path = PathBuf::from(pd)
        .join(".handoff")
        .join("docs")
        .join("_trace_report.json");
    serde_json::from_slice(&std::fs::read(path).expect("report exists")).expect("report is JSON")
}

#[test]
fn nfr_check_returns_all_eight_categories_classifies_and_persists() {
    let mut server = Server::spawn();
    let (_tmp, pd) = new_project(&mut server, "nfr-check-e2e");
    save_requirements(&mut server, &pd);

    let out = server.call(
        "handoff_trace_lint",
        json!({ "project_dir": pd, "action": "nfr_check" }),
    );
    let names: Vec<&str> = out["categories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, CATEGORIES, "{out}");
    for c in out["categories"].as_array().unwrap() {
        let t = c["prompt_template"].as_str().unwrap();
        assert!(t.contains("{text}") && t.contains(c["name"].as_str().unwrap()));
    }

    let security = category(&out, "security");
    assert_eq!(ids(&security["items"]), ["NFR-SECURITY-901"]);
    assert_eq!(security["status"], "applicable");
    let perf = category(&out, "performance_efficiency");
    assert_eq!(ids(&perf["items"]), ["NFR-902"]);
    assert_eq!(
        ids(&category(&out, "functional_suitability")["items"]),
        ["REQ-900"]
    );
    let compat = category(&out, "compatibility");
    assert_eq!(compat["status"], "not_reviewed");
    assert!(ids(&compat["items"]).is_empty());
    assert_eq!(ids(&out["unclassified"]), ["NFR-903"]);

    // Persisted into a complete, schema-versioned _trace_report.json.
    let report = read_report(&pd);
    assert!(report["schema_version"].as_u64().is_some(), "{report}");
    assert!(report["items"].as_array().is_some());
    let cov = &report["nfr_coverage"];
    assert_eq!(cov["categories"].as_array().unwrap().len(), 8, "{cov}");
    assert!(cov["categories"][0].get("prompt_template").is_none());
    let persisted_sec = cov["categories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "security")
        .unwrap();
    assert_eq!(persisted_sec["status"], "applicable");
    assert_eq!(ids(&persisted_sec["items"]), ["NFR-SECURITY-901"]);
    assert_eq!(ids(&cov["unclassified"]), ["NFR-903"]);
}

#[test]
fn nfr_coverage_survives_a_trace_report_rebuild() {
    let mut server = Server::spawn();
    let (_tmp, pd) = new_project(&mut server, "nfr-check-survive-e2e");
    save_requirements(&mut server, &pd);
    server.call(
        "handoff_trace_lint",
        json!({ "project_dir": pd, "action": "nfr_check" }),
    );
    let before = read_report(&pd)["nfr_coverage"].clone();
    assert!(before.is_object());

    // A new document changes the report's inputs, forcing a real rewrite.
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd, "slug": "at-nfr-e2e", "title": "Acceptance",
            "layer": "acceptance",
            "body": "# Acceptance\n\n### AT-900 Confirms REQ-900\n\n- verifies: REQ-900\n- method: manual\n\nBody.\n",
        }),
    );
    server.call("handoff_trace_report", json!({ "project_dir": pd }));
    let report = read_report(&pd);
    assert!(
        report["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["id"] == "AT-900"),
        "report was rebuilt"
    );
    assert_eq!(report["nfr_coverage"], before);
}

#[test]
fn nfr_check_review_statuses_are_recorded_and_sticky() {
    let mut server = Server::spawn();
    let (_tmp, pd) = new_project(&mut server, "nfr-check-review-e2e");
    save_requirements(&mut server, &pd);

    let out = server.call(
        "handoff_trace_lint",
        json!({
            "project_dir": pd, "action": "nfr_check",
            "review": { "portability": "not_applicable", "security": "reviewed" },
        }),
    );
    assert_eq!(category(&out, "portability")["status"], "not_applicable");
    assert_eq!(category(&out, "security")["status"], "reviewed");

    // A later call without `review` keeps the recorded decisions.
    let again = server.call(
        "handoff_trace_lint",
        json!({ "project_dir": pd, "action": "nfr_check" }),
    );
    assert_eq!(category(&again, "portability")["status"], "not_applicable");
    assert_eq!(category(&again, "security")["status"], "reviewed");
    assert_eq!(category(&again, "usability")["status"], "not_reviewed");

    // An explicit status overrides the sticky one.
    let reset = server.call(
        "handoff_trace_lint",
        json!({ "project_dir": pd, "action": "nfr_check", "review": { "portability": "not_reviewed" } }),
    );
    assert_eq!(category(&reset, "portability")["status"], "not_reviewed");
}

#[test]
fn nfr_check_items_filter_and_argument_errors() {
    let mut server = Server::spawn();
    let (_tmp, pd) = new_project(&mut server, "nfr-check-args-e2e");
    save_requirements(&mut server, &pd);

    let out = server.call(
        "handoff_trace_lint",
        json!({ "project_dir": pd, "action": "nfr_check", "items": ["NFR-SECURITY-901", "NOPE-1"] }),
    );
    assert_eq!(
        ids(&category(&out, "security")["items"]),
        ["NFR-SECURITY-901"]
    );
    assert!(ids(&category(&out, "functional_suitability")["items"]).is_empty());
    assert!(
        out["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("NOPE-1")),
        "{out}"
    );

    for (args, needle) in [
        (json!({ "items": [] }), "items"),
        (json!({ "items": "REQ-1" }), "items"),
        (json!({ "review": { "safety": "reviewed" } }), "safety"),
        (json!({ "review": { "security": "maybe" } }), "maybe"),
        (json!({ "review": ["security"] }), "review"),
    ] {
        let mut a = args.clone();
        a["project_dir"] = json!(pd);
        a["action"] = json!("nfr_check");
        let (is_error, text) = server.call_raw("handoff_trace_lint", a);
        assert!(is_error && text.contains(needle), "{args}: {text}");
    }
}

#[test]
fn nfr_check_with_no_requirements_still_returns_eight_empty_categories() {
    let mut server = Server::spawn();
    let (_tmp, pd) = new_project(&mut server, "nfr-check-empty-e2e");
    let out = server.call(
        "handoff_trace_lint",
        json!({ "project_dir": pd, "action": "nfr_check" }),
    );
    assert_eq!(out["categories"].as_array().unwrap().len(), 8);
    for c in out["categories"].as_array().unwrap() {
        assert_eq!(c["status"], "not_reviewed");
        assert!(ids(&c["items"]).is_empty());
    }
}

#[test]
fn requirement_gate_is_settable_and_readable_through_the_config_api() {
    let mut server = Server::spawn();
    let (_tmp, pd) = new_project(&mut server, "requirement-gate-e2e");

    let before = server.call("handoff_get_config", json!({ "project_dir": pd }));
    assert!(
        before["trace"].get("requirement_gate").is_none(),
        "{before}"
    );

    let (is_error, text) = server.call_raw(
        "handoff_update_config",
        json!({ "project_dir": pd, "updates": {
            "trace.requirement_gate.fail_on": "warning",
            "trace.requirement_gate.required_checks": ["iso29148", "nfr"],
            "trace.requirement_gate.skip_checks": ["completeness"],
        }}),
    );
    assert!(!is_error, "{text}");
    let cfg = server.call("handoff_get_config", json!({ "project_dir": pd }));
    let gate = &cfg["trace"]["requirement_gate"];
    assert_eq!(gate["fail_on"], "warning", "{cfg}");
    assert_eq!(gate["required_checks"], json!(["iso29148", "nfr"]));
    assert_eq!(gate["skip_checks"], json!(["completeness"]));

    // The server still loads the config (lint reads it).
    server.call("handoff_trace_lint", json!({ "project_dir": pd }));
}

#[test]
fn requirement_gate_rejects_invalid_values_without_touching_config() {
    let mut server = Server::spawn();
    let (_tmp, pd) = new_project(&mut server, "requirement-gate-invalid-e2e");
    let config_path = PathBuf::from(&pd).join(".handoff").join("config.toml");
    let original = std::fs::read_to_string(&config_path).unwrap();

    for (key, value, needle) in [
        ("trace.requirement_gate.fail_on", json!("info"), "fail_on"),
        (
            "trace.requirement_gate.required_checks",
            json!(["bogus"]),
            "bogus",
        ),
        (
            "trace.requirement_gate.skip_checks",
            json!("nfr"),
            "requirement_gate",
        ),
    ] {
        let (is_error, text) = server.call_raw(
            "handoff_update_config",
            json!({ "project_dir": pd, "updates": { key: value } }),
        );
        assert!(is_error && text.contains(needle), "{key}: {text}");
        assert_eq!(
            std::fs::read_to_string(&config_path).unwrap(),
            original,
            "{key}: a rejected update must not modify config.toml"
        );
    }
}

#[test]
fn hand_edited_invalid_requirement_gate_is_reported_by_trace_lint() {
    let mut server = Server::spawn();
    let (_tmp, pd) = new_project(&mut server, "requirement-gate-lint-e2e");
    let config_path = PathBuf::from(&pd).join(".handoff").join("config.toml");
    let mut content = std::fs::read_to_string(&config_path).unwrap();
    content.push_str("\n[trace.requirement_gate]\nfail_on = \"fatal\"\n");
    std::fs::write(&config_path, content).unwrap();

    let (is_error, text) = server.call_raw("handoff_trace_lint", json!({ "project_dir": pd }));
    assert!(is_error && text.contains("fail_on"), "{text}");
}
