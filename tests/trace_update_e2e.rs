//! Real-binary E2E tests for M2-14 `handoff_trace_update`
//! (wiki/260-vmodel-m2-design.md §4.8): spawns the actual `handoff-mcp`
//! binary and drives it over real stdio JSON-RPC, plus the `trace update`
//! CLI subcommand. Same harness style as `tests/trace_scaffold_e2e.rs`.

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
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "trace-update-e2e" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "req-update-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-001 Account lockout\n\nStatement text.\n",
        }),
    );
}

/// `upsert_item` on a new id creates the item, through the real parser +
/// layer sync — `doc_get`'s own body reflects the rendered Markdown and a
/// second `handoff_trace_report` sees the new item (§4.8/§4.7's "描画 → 解析
/// の往復で同じ項目になる").
#[test]
fn upsert_item_creates_a_new_item_and_it_round_trips_through_trace_report() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut server = Server::spawn();
    build_project(&mut server, dir.path());

    let out = server.call(
        "handoff_trace_update",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "ops": [{
                "op": "upsert_item",
                "doc": "req-update-e2e",
                "id": "REQ-002",
                "title": "Session timeout",
                "statement": "Idle sessions expire after 30 minutes.",
                "attrs": {"priority": "P2"},
            }],
        }),
    );
    assert!(out.get("failed").is_none(), "{out}");
    let applied = out["applied"].as_array().unwrap();
    assert_eq!(applied.len(), 1, "{out}");
    assert_eq!(applied[0]["result"]["created"], true, "{out}");

    let doc = server.call(
        "handoff_doc_get",
        json!({ "project_dir": dir.path().to_string_lossy(), "doc_id": "req-update-e2e" }),
    );
    let body = doc["body"].as_str().unwrap_or_default();
    assert!(body.contains("REQ-002"), "{body}");
    assert!(body.contains("Idle sessions expire"), "{body}");
    assert!(body.contains("- priority: P2"), "{body}");

    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.path().to_string_lossy(), "include_items": true }),
    );
    let items = report["items"].as_array().unwrap();
    assert!(
        items.iter().any(|i| i["id"] == "REQ-002"),
        "REQ-002 must appear in trace_report: {report}"
    );
}

/// `dry_run=true` validates and previews (with a unified-diff hunk for the
/// `upsert_item` op) without writing anything to `.handoff/` — byte-for-byte
/// identical before/after, same contract style as
/// `tests/trace_suspect_e2e.rs`'s `list_and_baseline_dry_run_never_write...`.
#[test]
fn dry_run_never_writes_to_handoff() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut server = Server::spawn();
    build_project(&mut server, dir.path());

    let doc_path = dir.path().join(".handoff/docs/_doc.req-update-e2e.md");
    let before = std::fs::read(&doc_path).expect("read doc before");

    let out = server.call(
        "handoff_trace_update",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "dry_run": true,
            "ops": [{
                "op": "upsert_item",
                "doc": "req-update-e2e",
                "id": "REQ-001",
                "statement": "Changed statement text.",
            }],
        }),
    );
    assert_eq!(out["dry_run"], true, "{out}");
    let diff = out["applied"][0]["result"]["diff"].as_str().unwrap();
    assert!(diff.contains("-Statement text."), "{diff}");
    assert!(diff.contains("+Changed statement text."), "{diff}");

    let after = std::fs::read(&doc_path).expect("read doc after");
    assert_eq!(before, after, "dry_run must not write anything");
}

/// One call combining `link` + `record` + `clear_suspect` exercises the E15
/// write order end to end: the task gets a baselined requirement link, the
/// record op lands in exactly one run file, and a real suspect (created by
/// directly re-editing the upstream item via `doc_save`) is cleared with its
/// own audit file.
#[test]
fn combined_link_record_and_clear_suspect_ops_in_one_call() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut server = Server::spawn();
    build_project(&mut server, dir.path());
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "slug": "at-update-e2e",
            "title": "Acceptance",
            "layer": "acceptance",
            "body": "# Acceptance\n\n### AT-001 Lockout check\n\n- verifies: REQ-001\n\nCheck the lockout.\n",
        }),
    );
    server.call(
        "handoff_update_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task": { "id": "t1", "title": "Implement lockout" } }),
    );

    let out = server.call(
        "handoff_trace_update",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "ops": [
                {"op": "link", "item": "REQ-001", "task": "t1"},
                {"op": "record", "item": "AT-001", "result": "pass"},
            ],
        }),
    );
    assert!(out.get("failed").is_none(), "{out}");
    let applied = out["applied"].as_array().unwrap();
    assert_eq!(applied.len(), 2, "{out}");

    let task = server.call(
        "handoff_get_task",
        json!({ "project_dir": dir.path().to_string_lossy(), "task_id": "t1" }),
    );
    assert!(
        task["trace"]["layers"]
            .as_array()
            .is_some_and(|l| !l.is_empty()),
        "task must show the new requirement link: {task}"
    );

    // Force a real link-suspect: re-save REQ-001's body (changing its text,
    // and therefore its def_hash) without re-baselining AT-001's `verifies`
    // link. `doc_save`'s own `doc_id` argument only resolves a real id (not
    // a slug, unlike most other tools in this suite) — fetch it first via
    // `doc_get`, which does accept either.
    let req_doc_meta = server.call(
        "handoff_doc_get",
        json!({ "project_dir": dir.path().to_string_lossy(), "doc_id": "req-update-e2e", "format": "meta" }),
    );
    let req_doc_id = req_doc_meta["id"].as_str().expect("doc id");
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "doc_id": req_doc_id,
            "body": "# Requirements\n\n### REQ-001 Account lockout\n\nChanged statement text.\n",
        }),
    );

    let clear_out = server.call(
        "handoff_trace_update",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "ops": [{
                "op": "clear_suspect",
                "item": "AT-001",
                "upstream": "REQ-001",
                "reason": "text-only change, no behavior impact",
            }],
        }),
    );
    assert!(clear_out.get("failed").is_none(), "{clear_out}");
    assert_eq!(
        clear_out["applied"][0]["result"]["cleared"]["links"], 1,
        "{clear_out}"
    );

    let clears_dir = dir.path().join(".handoff/trace/clears");
    let count = std::fs::read_dir(&clears_dir)
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(count, 1, "exactly one audit file must be written");
}

/// CLI `trace update --ops '[...]'` reaches the same handler as the MCP
/// tool call.
#[test]
fn cli_trace_update_applies_an_upsert_item_op() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut server = Server::spawn();
    build_project(&mut server, dir.path());
    drop(server);

    let ops = json!([{
        "op": "upsert_item",
        "doc": "req-update-e2e",
        "id": "REQ-003",
        "title": "CLI item",
        "statement": "Added via the CLI.",
    }])
    .to_string();
    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "update",
        "--project-dir",
        dir.path().to_str().unwrap(),
        "--ops",
        &ops,
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let out: Value = serde_json::from_str(&stdout).expect("valid JSON on stdout");
    assert!(out.get("failed").is_none(), "{out}");

    let body = std::fs::read_to_string(dir.path().join(".handoff/docs/_doc.req-update-e2e.md"))
        .expect("read doc body");
    assert!(body.contains("REQ-003"), "{body}");
    assert!(body.contains("Added via the CLI."), "{body}");
}

/// wiki/270-vmodel-m3-design.md §2.3 (M3-03, FR-406), real-binary E2E: the
/// full `draft -> review -> approved` lifecycle via `trace_update(set.approval)`,
/// `handoff_trace_report` reflecting each transition, an automatic
/// `approved -> draft` rollback when the item's body (and therefore
/// `def_hash`) changes afterward, and exactly one audit file written under
/// `.handoff/trace/approvals/` for the `-> approved` transition.
#[test]
fn approval_lifecycle_draft_review_approved_then_auto_rollback_on_body_change() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut server = Server::spawn();
    build_project(&mut server, dir.path());

    // A freshly-synced item has no `approval` field yet -> E12 compat
    // read-mapping reports "draft" (status defaults to "pending").
    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.path().to_string_lossy(), "include_items": true }),
    );
    let items = report["items"].as_array().unwrap();
    let req001 = items.iter().find(|i| i["id"] == "REQ-001").unwrap();
    assert_eq!(req001["approval"], "draft", "{report}");

    // draft -> review
    let out = server.call(
        "handoff_trace_update",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "ops": [{"op": "set", "item": "REQ-001", "approval": "review"}],
        }),
    );
    assert!(out.get("failed").is_none(), "{out}");
    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.path().to_string_lossy(), "include_items": true }),
    );
    let req001 = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "REQ-001")
        .unwrap();
    assert_eq!(req001["approval"], "review", "{report}");

    // review -> approved (human executor) — stamps approved_hash/by/at and
    // writes exactly one audit file.
    let out = server.call(
        "handoff_trace_update",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "executor_kind": "human",
            "executor_id": "ryoma",
            "ops": [{"op": "set", "item": "REQ-001", "approval": "approved"}],
        }),
    );
    assert!(out.get("failed").is_none(), "{out}");
    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.path().to_string_lossy(), "include_items": true }),
    );
    let req001 = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "REQ-001")
        .unwrap();
    assert_eq!(req001["approval"], "approved", "{report}");
    let approved_def_hash = req001["def_hash"]
        .as_str()
        .expect("def_hash present")
        .to_string();

    let approvals_dir = dir.path().join(".handoff/trace/approvals");
    let entries: Vec<_> = std::fs::read_dir(&approvals_dir)
        .expect("approvals dir exists")
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(entries.len(), 1, "exactly one approval audit file expected");
    let record: Value =
        serde_json::from_str(&std::fs::read_to_string(entries[0].path()).unwrap()).unwrap();
    assert_eq!(record["executor"]["kind"], "human", "{record}");
    assert_eq!(record["executor"]["id"], "ryoma", "{record}");
    assert_eq!(record["items"][0]["id"], "REQ-001", "{record}");
    assert_eq!(record["items"][0]["from_approval"], "review", "{record}");
    assert_eq!(record["items"][0]["to_approval"], "approved", "{record}");
    assert_eq!(
        record["items"][0]["def_hash"], approved_def_hash,
        "{record}"
    );

    // Changing REQ-001's body (def_hash changes) auto-rolls-back approval to
    // draft on the next sync — approved_hash is NOT cleared (§2.3/§3.2).
    // `doc_save`'s `doc_id` argument only resolves a real id (not a slug) —
    // fetch it first via `doc_get`, which accepts either.
    let req_doc_meta = server.call(
        "handoff_doc_get",
        json!({ "project_dir": dir.path().to_string_lossy(), "doc_id": "req-update-e2e", "format": "meta" }),
    );
    let req_doc_id = req_doc_meta["id"].as_str().expect("doc id");
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "doc_id": req_doc_id,
            "body": "# Requirements\n\n### REQ-001 Account lockout\n\nStatement text, revised.\n",
        }),
    );
    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.path().to_string_lossy(), "include_items": true }),
    );
    let req001 = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "REQ-001")
        .unwrap();
    assert_eq!(
        req001["approval"], "draft",
        "a def_hash change must auto-reset approval to draft: {report}"
    );
    assert_ne!(
        req001["def_hash"].as_str().unwrap(),
        approved_def_hash,
        "the body edit must actually change def_hash"
    );

    // Still only one audit file — the automatic rollback is not itself an
    // approval event and must not write one.
    let count_after_rollback = std::fs::read_dir(&approvals_dir).unwrap().count();
    assert_eq!(
        count_after_rollback, 1,
        "automatic draft rollback must not write a new approval audit file"
    );
}
