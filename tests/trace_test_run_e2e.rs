//! Real-binary MCP-tool E2E for `handoff_trace_test_run`
//! (wiki/270-vmodel-m3-design.md §2.6/§4.4, M3-09, FR-304).

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

fn build_project(server: &mut Server, dir: &std::path::Path, project_name: &str) -> String {
    let pd = dir.to_string_lossy().to_string();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": project_name }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "requirements-test-run-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-900 Something important\n\nBody.\n",
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "system-test-test-run-e2e",
            "title": "System tests",
            "layer": "system_test",
            "body": "# System test\n\n### ST-900 Verify REQ-900\n\n- verifies: REQ-900\n\nSteps.\n\n### ST-901 Verify REQ-900 again\n\n- verifies: REQ-900\n\nMore steps.\n",
        }),
    );
    pd
}

/// `create` with `scope.layers` only (no `kinds`) must enumerate every layer
/// item in those layers (the PR-4 fast path, MR-03) and persist a definition
/// file under `.handoff/trace/test_runs/`.
#[test]
fn create_with_layers_only_writes_a_definition_file_and_enumerates_matching_items() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    let pd = build_project(&mut server, &dir, "trace-test-run-e2e-layers-only");

    let created = server.call(
        "handoff_trace_test_run",
        json!({
            "project_dir": pd,
            "action": "create",
            "scope": { "layers": ["system_test"] },
            "label": "Sprint 5 regression",
        }),
    );
    let test_run_id = created["test_run_id"]
        .as_str()
        .expect("test_run_id present")
        .to_string();
    assert!(!test_run_id.is_empty());

    let target_items: Vec<String> = created["target_items"]
        .as_array()
        .expect("target_items array")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        target_items,
        vec!["ST-900".to_string(), "ST-901".to_string()],
        "{created}"
    );
    assert_eq!(created["total_target_count"], 2, "{created}");

    let def_path = dir
        .join(".handoff")
        .join("trace")
        .join("test_runs")
        .join(format!("{test_run_id}.json"));
    assert!(def_path.exists(), "definition file must exist on disk");
    let on_disk: Value =
        serde_json::from_str(&std::fs::read_to_string(&def_path).unwrap()).unwrap();
    assert_eq!(on_disk["label"], "Sprint 5 regression");
    assert_eq!(on_disk["scope"]["layers"], json!(["system_test"]));
    assert_eq!(on_disk["total_target_count"], 2);
}

/// Full lifecycle: create -> definition file exists -> trace_record against
/// one of the target items with that test_run_id -> progress reflects the
/// recorded result and lists the rest as remaining -> list returns the test
/// run.
#[test]
fn create_then_record_then_progress_then_list_full_lifecycle() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    let pd = build_project(&mut server, &dir, "trace-test-run-e2e-lifecycle");

    let created = server.call(
        "handoff_trace_test_run",
        json!({
            "project_dir": pd,
            "action": "create",
            "scope": { "layers": ["system_test"] },
            "label": "Full lifecycle",
        }),
    );
    let test_run_id = created["test_run_id"].as_str().unwrap().to_string();
    assert_eq!(created["total_target_count"], 2, "{created}");

    // Before any run is recorded, progress must show everything outstanding.
    let progress_before = server.call(
        "handoff_trace_test_run",
        json!({ "project_dir": pd, "action": "progress", "test_run_id": test_run_id }),
    );
    assert_eq!(progress_before["total"], 2, "{progress_before}");
    assert_eq!(progress_before["executed"], 0, "{progress_before}");
    assert_eq!(progress_before["not_run"], 2, "{progress_before}");
    assert_eq!(progress_before["progress_pct"], 0.0, "{progress_before}");

    // Record a passing result for ST-900 against this test run.
    server.call(
        "handoff_trace_record",
        json!({
            "project_dir": pd,
            "results": [{ "item": "ST-900", "result": "pass" }],
            "test_run_id": test_run_id,
        }),
    );

    let progress_after = server.call(
        "handoff_trace_test_run",
        json!({ "project_dir": pd, "action": "progress", "test_run_id": test_run_id }),
    );
    assert_eq!(progress_after["total"], 2, "{progress_after}");
    assert_eq!(progress_after["executed"], 1, "{progress_after}");
    assert_eq!(progress_after["pass"], 1, "{progress_after}");
    assert_eq!(progress_after["not_run"], 1, "{progress_after}");
    assert_eq!(progress_after["progress_pct"], 50.0, "{progress_after}");
    let remaining: Vec<String> = progress_after["remaining"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(remaining, vec!["ST-901".to_string()], "{progress_after}");

    // A run recorded with no test_run_id must not pollute this test run's
    // own progress (the top-level runs/_latest.json items map is updated,
    // but by_test_run[test_run_id] must not gain a spurious entry).
    server.call(
        "handoff_trace_record",
        json!({ "project_dir": pd, "results": [{ "item": "ST-901", "result": "fail" }] }),
    );
    let progress_unaffected = server.call(
        "handoff_trace_test_run",
        json!({ "project_dir": pd, "action": "progress", "test_run_id": test_run_id }),
    );
    assert_eq!(
        progress_unaffected["not_run"], 1,
        "an un-scoped record must not count toward this test run's progress: {progress_unaffected}"
    );

    let listed = server.call(
        "handoff_trace_test_run",
        json!({ "project_dir": pd, "action": "list" }),
    );
    let test_runs = listed["test_runs"].as_array().expect("test_runs array");
    assert_eq!(test_runs.len(), 1, "{listed}");
    assert_eq!(test_runs[0]["test_run_id"], test_run_id);
    assert_eq!(test_runs[0]["label"], "Full lifecycle");
    assert_eq!(test_runs[0]["total_target_count"], 2);
}

/// `progress` with an unknown `test_run_id` is an error.
#[test]
fn progress_with_unknown_test_run_id_is_an_error() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "trace-test-run-e2e-unknown" }),
    );

    let (is_error, text) = server.call_raw(
        "handoff_trace_test_run",
        json!({ "project_dir": pd, "action": "progress", "test_run_id": "does-not-exist" }),
    );
    assert!(is_error, "expected an error, got: {text}");
}

/// `create` with `scope.kinds` must use the slower graph-based next-action
/// enumeration and only return items matching one of the requested kinds.
#[test]
fn create_with_kinds_scope_uses_next_action_derivation() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    let pd = build_project(&mut server, &dir, "trace-test-run-e2e-kinds");

    // ST-900/ST-901 have never been run -> both are `rerun` candidates
    // (never-run verifier whose target is implemented or beyond; dev_stage
    // defaults make REQ-900 "not_started", so this also depends on
    // trace_next's own rerun rule tolerating that — assert only that the
    // kinds-scoped call succeeds and returns a subset of the unscoped one).
    let created = server.call(
        "handoff_trace_test_run",
        json!({
            "project_dir": pd,
            "action": "create",
            "scope": { "kinds": ["create_task"] },
        }),
    );
    let target_items: Vec<String> = created["target_items"]
        .as_array()
        .expect("target_items array")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(
        target_items.contains(&"REQ-900".to_string()),
        "REQ-900 has no implementing task yet -> create_task candidate: {created}"
    );
    assert!(
        !target_items.contains(&"ST-900".to_string()),
        "ST-900 is a verification item, never a create_task candidate: {created}"
    );

    // The persisted definition's scope.kinds must round-trip in the same
    // lowercase snake_case vocabulary the caller supplied, not NextActionKind's
    // Debug spelling ("CreateTask").
    let test_run_id = created["test_run_id"].as_str().unwrap();
    let def_path = dir
        .join(".handoff")
        .join("trace")
        .join("test_runs")
        .join(format!("{test_run_id}.json"));
    let on_disk: Value =
        serde_json::from_str(&std::fs::read_to_string(&def_path).unwrap()).unwrap();
    assert_eq!(
        on_disk["scope"]["kinds"],
        json!(["create_task"]),
        "{on_disk}"
    );
}

/// An unknown `scope.kinds` entry is a hard error, not silently dropped.
#[test]
fn create_with_unknown_kind_is_an_error() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "trace-test-run-e2e-bad-kind" }),
    );

    let (is_error, text) = server.call_raw(
        "handoff_trace_test_run",
        json!({
            "project_dir": pd,
            "action": "create",
            "scope": { "kinds": ["not_a_real_kind"] },
        }),
    );
    assert!(is_error, "expected an error, got: {text}");
}

fn tr(server: &mut Server, pd: &str, mut args: Value) -> Value {
    args["project_dir"] = json!(pd);
    server.call("handoff_trace_test_run", args)
}

fn tr_err(server: &mut Server, pd: &str, mut args: Value) -> String {
    args["project_dir"] = json!(pd);
    let (is_error, text) = server.call_raw("handoff_trace_test_run", args);
    assert!(is_error, "expected an error, got: {text}");
    text
}

/// Verification campaign (FR-512/SPEC-512): `create` auto-generates a
/// pending checklist from the scoped items' criterion text; checks and
/// evidence are recorded; status walks draft -> approved; the legacy
/// `progress` shape is unchanged.
#[test]
fn campaign_full_lifecycle_checklist_evidence_status_and_progress() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    let pd = build_project(&mut server, &dir, "trace-test-run-e2e-campaign");

    let created = tr(
        &mut server,
        &pd,
        json!({ "action": "create", "scope": { "layers": ["system_test"] }, "label": "Campaign" }),
    );
    let id = created["test_run_id"].as_str().unwrap().to_string();
    assert_eq!(created["campaign_status"], "draft", "{created}");
    assert_eq!(created["checklist_count"], 2, "{created}");

    let got = tr(
        &mut server,
        &pd,
        json!({ "action": "get", "test_run_id": id }),
    );
    let checklist = got["checklist"].as_array().unwrap();
    assert_eq!(checklist[0]["item_id"], "ST-900");
    assert_eq!(checklist[0]["result"], "pending");
    assert!(
        !checklist[0]["acceptance_text"].as_str().unwrap().is_empty(),
        "{got}"
    );
    assert_eq!(got["progress"]["total"], 2);
    assert_eq!(got["progress"]["pending"], 2);

    // Completing / approving too early is rejected.
    tr_err(
        &mut server,
        &pd,
        json!({ "action": "set_status", "test_run_id": id, "status": "completed" }),
    );

    // Record a check with structured evidence; draft auto-advances.
    let rec = tr(
        &mut server,
        &pd,
        json!({
            "action": "record_check", "test_run_id": id, "item_id": "ST-900",
            "result": "pass", "note": "looks right", "verified_by": "ryoma",
            "evidence": [{ "path": "evidence/st900.png", "type": "screenshot", "caption": "after" }],
        }),
    );
    assert_eq!(rec["campaign_status"], "in_progress", "{rec}");
    assert_eq!(rec["progress"]["pass"], 1, "{rec}");
    assert_eq!(rec["progress"]["checked"], 1, "{rec}");

    // Extra evidence without changing the verdict.
    tr(
        &mut server,
        &pd,
        json!({
            "action": "add_evidence", "test_run_id": id, "item_id": "ST-900",
            "evidence": { "path": "evidence/st900.log", "type": "log", "caption": "run log" },
        }),
    );
    // Path traversal is rejected.
    tr_err(
        &mut server,
        &pd,
        json!({ "action": "add_evidence", "test_run_id": id, "item_id": "ST-900", "evidence": { "path": "../etc/passwd", "type": "file" } }),
    );
    // Invalid result / unknown item rejected.
    tr_err(
        &mut server,
        &pd,
        json!({ "action": "record_check", "test_run_id": id, "item_id": "ST-901", "result": "maybe" }),
    );
    tr_err(
        &mut server,
        &pd,
        json!({ "action": "record_check", "test_run_id": id, "item_id": "ghost", "result": "pass" }),
    );

    // Still one pending -> cannot complete.
    tr_err(
        &mut server,
        &pd,
        json!({ "action": "set_status", "test_run_id": id, "status": "completed" }),
    );

    tr(
        &mut server,
        &pd,
        json!({ "action": "record_check", "test_run_id": id, "item_id": "ST-901", "result": "waived", "note": "n/a" }),
    );
    let done = tr(
        &mut server,
        &pd,
        json!({ "action": "set_status", "test_run_id": id, "status": "completed" }),
    );
    assert_eq!(done["campaign_status"], "completed", "{done}");

    // Frozen once completed; approval needs approved_by.
    tr_err(
        &mut server,
        &pd,
        json!({ "action": "record_check", "test_run_id": id, "item_id": "ST-901", "result": "fail" }),
    );
    tr_err(
        &mut server,
        &pd,
        json!({ "action": "set_status", "test_run_id": id, "status": "approved" }),
    );
    let approved = tr(
        &mut server,
        &pd,
        json!({ "action": "set_status", "test_run_id": id, "status": "approved", "approved_by": "boss" }),
    );
    assert_eq!(approved["campaign_status"], "approved", "{approved}");
    assert_eq!(approved["approved_by"], "boss");
    tr_err(
        &mut server,
        &pd,
        json!({ "action": "set_status", "test_run_id": id, "status": "in_progress" }),
    );

    // Persisted on disk with {path,type,caption} evidence.
    let def_path = dir
        .join(".handoff/trace/test_runs")
        .join(format!("{id}.json"));
    let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(def_path).unwrap()).unwrap();
    assert_eq!(
        on_disk["checklist"][0]["evidence"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(on_disk["checklist"][0]["evidence"][0]["type"], "screenshot");
    assert_eq!(on_disk["checklist"][0]["verified_by"], "ryoma");
    assert_eq!(on_disk["progress"]["waived"], 1);

    // list and legacy progress stay compatible, with campaign info added.
    let listed = tr(&mut server, &pd, json!({ "action": "list" }));
    assert_eq!(
        listed["test_runs"][0]["campaign_status"], "approved",
        "{listed}"
    );
    let progress = tr(
        &mut server,
        &pd,
        json!({ "action": "progress", "test_run_id": id }),
    );
    assert_eq!(progress["total"], 2);
    assert_eq!(
        progress["not_run"], 2,
        "legacy runs-based tally unaffected: {progress}"
    );
    assert_eq!(progress["checklist_progress"]["pass"], 1, "{progress}");
    assert_eq!(progress["campaign_status"], "approved");
}

/// `auto_checklist=false` creates a campaign with no checklist; reopening a
/// completed campaign works.
#[test]
fn campaign_auto_checklist_opt_out_and_reopen() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    let pd = build_project(&mut server, &dir, "trace-test-run-e2e-campaign-optout");

    let none = tr(
        &mut server,
        &pd,
        json!({ "action": "create", "scope": { "layers": ["system_test"] }, "auto_checklist": false }),
    );
    assert_eq!(none["checklist_count"], 0, "{none}");
    assert_eq!(none["total_target_count"], 2);

    let c = tr(
        &mut server,
        &pd,
        json!({ "action": "create", "scope": { "layers": ["requirement"] } }),
    );
    let id = c["test_run_id"].as_str().unwrap().to_string();
    tr(
        &mut server,
        &pd,
        json!({ "action": "set_status", "test_run_id": id, "status": "in_progress" }),
    );
    tr(
        &mut server,
        &pd,
        json!({ "action": "record_check", "test_run_id": id, "item_id": "REQ-900", "result": "pass" }),
    );
    tr(
        &mut server,
        &pd,
        json!({ "action": "set_status", "test_run_id": id, "status": "completed" }),
    );
    let reopened = tr(
        &mut server,
        &pd,
        json!({ "action": "set_status", "test_run_id": id, "status": "in_progress" }),
    );
    assert_eq!(reopened["campaign_status"], "in_progress", "{reopened}");
    tr(
        &mut server,
        &pd,
        json!({ "action": "record_check", "test_run_id": id, "item_id": "REQ-900", "result": "fail" }),
    );
}
