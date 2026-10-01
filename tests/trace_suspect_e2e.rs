//! Real-binary E2E tests for M2-05 `handoff_trace_suspect`
//! (wiki/260-vmodel-m2-design.md §3.2/§4.1): spawns the actual `handoff-mcp`
//! binary and drives it over real stdio JSON-RPC. Same harness style as
//! `tests/trace_report_slice_e2e.rs`/`tests/trace_scaffold_e2e.rs`.

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

fn suspects_of_kind<'a>(list: &'a Value, kind: &str) -> Vec<&'a Value> {
    list["suspects"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["kind"] == kind)
        .collect()
}

fn suspect_items(list: &Value, kind: &str) -> Vec<String> {
    let mut ids: Vec<String> = suspects_of_kind(list, kind)
        .into_iter()
        .map(|s| s["item"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    ids
}

/// Builds a 4-document V (requirement REQ-100 <- basic_spec SPEC-100,
/// requirement REQ-100 <- acceptance AT-100, basic_spec SPEC-100 <-
/// system_test ST-100) — used by the "1 hop, then deeper after the middle
/// item changes" scenario (wiki/260 §3.2's "E1: 下流への広がりは自然に
/// 1ホップになる").
fn build_v_project(server: &mut Server, dir: &std::path::Path) -> (String, String) {
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "trace-suspect-e2e" }),
    );

    let req = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "req-suspect-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-100 Account lockout\n\n\
                - priority: P1\n\n\
                Original requirement statement text.\n",
        }),
    );
    let req_doc_id = req["doc_id"].as_str().expect("doc_id").to_string();

    let spec = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "spec-suspect-e2e",
            "title": "Basic spec",
            "layer": "basic_spec",
            "body": "# Basic spec\n\n### SPEC-100 Lockout counter\n\n\
                - refines: REQ-100\n\n\
                Original spec statement text.\n",
        }),
    );
    let spec_doc_id = spec["doc_id"].as_str().expect("doc_id").to_string();

    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "at-suspect-e2e",
            "title": "Acceptance",
            "layer": "acceptance",
            "body": "# Acceptance\n\n### AT-100 Lockout after 5 failures\n\n\
                - verifies: REQ-100\n\
                - method: manual\n\n\
                Fail login 5 times, then confirm the account is locked.\n",
        }),
    );

    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "st-suspect-e2e",
            "title": "System tests",
            "layer": "system_test",
            "body": "# System tests\n\n### ST-100 Counter increments\n\n\
                - verifies: SPEC-100\n\
                - method: auto\n\n\
                Assert the counter increments on each failed login.\n",
        }),
    );

    (req_doc_id, spec_doc_id)
}

/// Core scenario (this task's done_criteria): no suspects right after
/// creation (M2-04 already baselines every newly-added link); editing the
/// root REQ-100 makes exactly its 1-hop dependents (SPEC-100's `refines`,
/// AT-100's `verifies`) suspect — ST-100 (2 hops away) stays clean; editing
/// the *middle* item SPEC-100 then additionally makes ST-100 suspect too
/// (§3.2's "中間項目の変更でさらに下へ広がる").
#[test]
fn suspects_spread_one_hop_then_deeper_after_the_middle_item_changes() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    let (req_doc_id, spec_doc_id) = build_v_project(&mut server, &dir);

    let list0 = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    assert!(
        list0["suspects"].as_array().unwrap().is_empty(),
        "no suspects right after creation: {list0}"
    );
    assert_eq!(list0["unbaselined"]["links"], 0, "{list0}");

    // Edit REQ-100 (the root) — its own def_hash changes.
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc_id": req_doc_id,
            "body": "# Requirements\n\n### REQ-100 Account lockout\n\n\
                - priority: P1\n\n\
                Updated requirement statement text (round 1).\n",
        }),
    );

    let list1 = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    let link_items1 = suspect_items(&list1, "link");
    assert_eq!(
        link_items1,
        vec!["AT-100".to_string(), "SPEC-100".to_string()],
        "only REQ-100's direct (1-hop) dependents must be suspect: {list1}"
    );
    for s in suspects_of_kind(&list1, "link") {
        assert_eq!(s["upstream"], "REQ-100", "{s}");
    }

    // Edit SPEC-100 (the middle item) — its own def_hash changes too.
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc_id": spec_doc_id,
            "body": "# Basic spec\n\n### SPEC-100 Lockout counter\n\n\
                - refines: REQ-100\n\n\
                Updated spec statement text (round 1).\n",
        }),
    );

    let list2 = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    let link_items2 = suspect_items(&list2, "link");
    assert_eq!(
        link_items2,
        vec![
            "AT-100".to_string(),
            "SPEC-100".to_string(),
            "ST-100".to_string()
        ],
        "SPEC-100's own change must now also reach ST-100 (one more hop down): {list2}"
    );
    let st_suspect = suspects_of_kind(&list2, "link")
        .into_iter()
        .find(|s| s["item"] == "ST-100")
        .unwrap();
    assert_eq!(st_suspect["upstream"], "SPEC-100", "{st_suspect}");
}

/// Task suspects, and bulk clear by `{upstream}` (links) + `{task_id}`
/// (task) in a single call — wiki/260 §4.1's "一括解除（upstream / layer）",
/// audited in one `.handoff/trace/clears/<id>.json` file.
#[test]
fn task_suspect_and_bulk_clear_by_upstream_and_task_id() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    let (req_doc_id, _spec_doc_id) = build_v_project(&mut server, &dir);

    server.call(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "id": "t1", "title": "Implement lockout", "requirement_ids": ["REQ-100"] },
        }),
    );

    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc_id": req_doc_id,
            "body": "# Requirements\n\n### REQ-100 Account lockout\n\n\
                - priority: P1\n\n\
                Updated requirement statement text (round 2).\n",
        }),
    );

    let list1 = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    assert_eq!(suspect_items(&list1, "link").len(), 2, "{list1}");
    let task_suspects = suspects_of_kind(&list1, "task");
    assert_eq!(task_suspects.len(), 1, "{list1}");
    assert_eq!(task_suspects[0]["item"], "REQ-100");
    assert_eq!(task_suspects[0]["task"], "t1");

    let clear = server.call(
        "handoff_trace_suspect",
        json!({
            "project_dir": dir.to_string_lossy(),
            "action": "clear",
            "targets": [{"upstream": "REQ-100"}, {"task_id": "t1"}],
            "reason": "reviewed both diffs, text-only changes",
            "executor_kind": "human",
            "executor_id": "reviewer-1",
        }),
    );
    assert_eq!(clear["cleared"]["links"], 2, "{clear}");
    assert_eq!(clear["cleared"]["tasks"], 1, "{clear}");
    assert_eq!(clear["cleared"]["results"], 0, "{clear}");
    let clear_id = clear["clear_id"].as_str().expect("clear_id");

    let clear_path = dir
        .join(".handoff/trace/clears")
        .join(format!("{clear_id}.json"));
    let clear_json: Value =
        serde_json::from_str(&std::fs::read_to_string(&clear_path).unwrap()).unwrap();
    assert_eq!(
        clear_json["reason"],
        "reviewed both diffs, text-only changes"
    );
    assert_eq!(clear_json["executor"]["kind"], "human");
    assert_eq!(clear_json["executor"]["id"], "reviewer-1");
    assert_eq!(clear_json["links"].as_array().unwrap().len(), 2);
    assert_eq!(clear_json["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(clear_json["tasks"][0]["task"], "t1");
    assert_eq!(clear_json["tasks"][0]["item"], "REQ-100");

    let list2 = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    assert!(
        list2["suspects"].as_array().unwrap().is_empty(),
        "cleared suspects must not reappear: {list2}"
    );
}

/// Unbaselined links (a reference authored before its upstream existed, so
/// M2-04's sync-time baseline recording couldn't resolve it — wiki/260 §2.5
/// step 4's "上流が未解決...記録しない") and `action=\"baseline\"`'s
/// dry-run-by-default migration path (§7/§4.1).
#[test]
fn unbaselined_link_and_the_baseline_action() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();

    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "trace-suspect-unbaselined-e2e" }),
    );

    // SPEC-200 refines REQ-200 before REQ-200 exists anywhere in the
    // project — the reference can't be baselined at sync time (dangling).
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "spec-unbaselined-e2e",
            "title": "Basic spec",
            "layer": "basic_spec",
            "body": "# Basic spec\n\n### SPEC-200 Early reference\n\n\
                - refines: REQ-200\n\n\
                References a requirement that does not exist yet.\n",
        }),
    );
    let req_doc = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "req-unbaselined-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-200 Now it exists\n\n\
                Original requirement statement text.\n",
        }),
    );
    let req_doc_id = req_doc["doc_id"].as_str().expect("doc_id").to_string();

    let list0 = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    assert!(
        list0["suspects"].as_array().unwrap().is_empty(),
        "an unbaselined link is never itself a suspect: {list0}"
    );
    assert_eq!(list0["unbaselined"]["links"], 1, "{list0}");

    // dry_run (default) previews without writing.
    let dry = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "baseline" }),
    );
    assert_eq!(dry["baselined"]["links"], 1, "{dry}");
    assert_eq!(dry["dry_run"], true, "{dry}");

    let list_after_dry_run = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    assert_eq!(
        list_after_dry_run["unbaselined"]["links"], 1,
        "dry_run must not write anything: {list_after_dry_run}"
    );

    let apply = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "baseline", "dry_run": false }),
    );
    assert_eq!(apply["baselined"]["links"], 1, "{apply}");
    assert_eq!(apply["dry_run"], false, "{apply}");

    let list1 = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    assert_eq!(list1["unbaselined"]["links"], 0, "{list1}");
    assert!(list1["suspects"].as_array().unwrap().is_empty(), "{list1}");

    // Now that it has a baseline, a subsequent REQ-200 edit does surface a
    // suspect (proving the baseline action wrote a real, usable value, not
    // just decremented a counter).
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc_id": req_doc_id,
            "body": "# Requirements\n\n### REQ-200 Now it exists\n\n\
                Updated requirement statement text.\n",
        }),
    );
    let list2 = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    assert_eq!(
        suspect_items(&list2, "link"),
        vec!["SPEC-200".to_string()],
        "{list2}"
    );
}

/// `result` suspect (a `Passing` right-side verification item whose
/// recorded `def_hash` no longer matches its current one — a right-side
/// item's own `state` is exactly its own run result, no vertical/horizontal
/// pull-down, so this isolates the `result`-kind check from `link`), the
/// `reverify` set, and clearing it via a carried-forward
/// `runs/<run_id>.json` entry (D2: the result's authority stays
/// runs-only — wiki/260 §4.1).
#[test]
fn result_suspect_reverify_and_carried_forward_clear() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let handoff = dir.join(".handoff");
    let mut server = Server::spawn();

    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "trace-suspect-result-e2e" }),
    );

    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "req-result-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-300 Account lockout\n\n\
                Original requirement statement text.\n",
        }),
    );
    let at = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "at-result-e2e",
            "title": "Acceptance",
            "layer": "acceptance",
            "body": "# Acceptance\n\n### AT-300 Lockout after 5 failures\n\n\
                - verifies: REQ-300\n\
                - method: manual\n\n\
                Original acceptance test statement text.\n",
        }),
    );
    let at_doc_id = at["doc_id"].as_str().expect("doc_id").to_string();

    let record = server.call(
        "handoff_trace_record",
        json!({
            "project_dir": dir.to_string_lossy(),
            "results": [{"item": "AT-300", "result": "pass"}],
        }),
    );
    let original_run_id = record["run_id"].as_str().expect("run_id").to_string();

    // Edit AT-300's *own* body (not REQ-300's) — isolates the `result`
    // check: AT-300's `verifies: REQ-300` baseline is untouched (REQ-300
    // itself never changed), only AT-300's own recorded-vs-current def_hash
    // diverges.
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc_id": at_doc_id,
            "body": "# Acceptance\n\n### AT-300 Lockout after 5 failures\n\n\
                - verifies: REQ-300\n\
                - method: manual\n\n\
                Updated acceptance test statement text.\n",
        }),
    );

    let list1 = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    assert_eq!(
        suspect_items(&list1, "result"),
        vec!["AT-300".to_string()],
        "{list1}"
    );
    assert!(
        suspects_of_kind(&list1, "link").is_empty(),
        "editing AT-300's own body must not touch its verifies-link baseline: {list1}"
    );
    let reverify: Vec<String> = list1["reverify"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(reverify, vec!["AT-300".to_string()], "{list1}");

    let clear = server.call(
        "handoff_trace_suspect",
        json!({
            "project_dir": dir.to_string_lossy(),
            "action": "clear",
            "targets": [{"result": "AT-300"}],
            "reason": "re-reviewed the diff, still passes",
        }),
    );
    assert_eq!(clear["cleared"]["results"], 1, "{clear}");
    let clear_id = clear["clear_id"].as_str().expect("clear_id");

    let clear_path = handoff
        .join("trace/clears")
        .join(format!("{clear_id}.json"));
    let clear_json: Value =
        serde_json::from_str(&std::fs::read_to_string(&clear_path).unwrap()).unwrap();
    assert_eq!(clear_json["results"].as_array().unwrap().len(), 1);
    assert_eq!(clear_json["results"][0]["item"], "AT-300");
    let new_run_id = clear_json["results"][0]["run_id"]
        .as_str()
        .expect("run_id")
        .to_string();
    assert_ne!(new_run_id, original_run_id);

    let new_run_path = handoff.join("runs").join(format!("{new_run_id}.json"));
    let new_run: Value =
        serde_json::from_str(&std::fs::read_to_string(&new_run_path).unwrap()).unwrap();
    assert_eq!(new_run["results"][0]["item"], "AT-300");
    assert_eq!(new_run["results"][0]["result"], "pass");
    assert_eq!(new_run["results"][0]["carried_from"], original_run_id);
    assert!(
        new_run["results"][0]["def_hash"].is_string(),
        "the carried-forward entry must record the item's *current* def_hash: {new_run}"
    );

    let list2 = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    assert!(
        suspects_of_kind(&list2, "result").is_empty(),
        "the result suspect must be gone after clearing: {list2}"
    );
    assert!(
        list2["reverify"].as_array().unwrap().is_empty(),
        "reverify must clear alongside the result suspect: {list2}"
    );
}

/// action="list" and action="baseline" (dry_run) must never write
/// `.handoff/`'s bytes (E6) — mirrors the read-only-tools byte-stability
/// convention `tests/trace_report_derived_file_e2e.rs` already established
/// for `trace_history`.
///
/// M2-08 (wiki/260 §4.1's session-review note, t360.20.8): this is no longer
/// gated behind a "warm up `runs/_latest.json` first" step — `list`/
/// `baseline(dry_run=true)` now go through `load_trace_input_read_only`
/// (`runs::load_latest_readonly`, never `runs::sync`), so even the very
/// first call on a project with zero runs recorded must be byte-stable, not
/// just idempotent *after* a prior materializing call.
#[test]
fn list_and_baseline_dry_run_never_write_to_handoff() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let handoff = dir.join(".handoff");
    let mut server = Server::spawn();
    build_v_project(&mut server, &dir);

    fn snapshot(handoff: &std::path::Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        for entry in walkdir_lite(handoff) {
            if entry.is_file() {
                out.push((entry.clone(), std::fs::read(&entry).unwrap()));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    fn walkdir_lite(dir: &std::path::Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    out.extend(walkdir_lite(&path));
                } else {
                    out.push(path);
                }
            }
        }
        out
    }

    let before = snapshot(&handoff);
    server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "baseline" }),
    );
    let after = snapshot(&handoff);
    assert_eq!(
        before, after,
        "action=list / action=baseline(dry_run=true) must never write any byte under .handoff/, \
         including on the very first call of a fresh project"
    );
}

/// M2-08 (wiki/260 §4.1's session-review note, t360.20.8, E6 (1)): `list`
/// must detect a suspect introduced by a *direct* body edit of a layer
/// document — resynced in memory only for this call — without writing
/// anything to `.handoff/`. Pre-fix, `list` used `load_trace_input` (which
/// reads each document's stored, pre-edit `def_hash` straight off disk, no
/// resync at all), so a direct edit's suspect only ever appeared once some
/// write-classified tool (`doc_save`, `trace_report`) happened to run first.
#[test]
fn list_detects_a_direct_edit_suspect_in_memory_without_writing_anything() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let handoff = dir.join(".handoff");
    let mut server = Server::spawn();
    build_v_project(&mut server, &dir);

    fn snapshot(handoff: &std::path::Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        for entry in walkdir_lite(handoff) {
            if entry.is_file() {
                out.push((entry.clone(), std::fs::read(&entry).unwrap()));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
    fn walkdir_lite(dir: &std::path::Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    out.extend(walkdir_lite(&path));
                } else {
                    out.push(path);
                }
            }
        }
        out
    }

    // REQ-100's body is edited directly (bypassing doc_save/sync entirely) —
    // SPEC-100's `refines: REQ-100` baseline on disk still reflects the
    // pre-edit def_hash.
    handoff_mcp::storage::docs::write_doc_body(
        &handoff,
        "req-suspect-e2e",
        "# Requirements\n\n### REQ-100 Account lockout\n\n\
         - priority: P1\n\n\
         Directly edited requirement statement text.\n",
    )
    .expect("write_doc_body");

    let before = snapshot(&handoff);
    let list = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    let after = snapshot(&handoff);

    assert_eq!(
        before, after,
        "list must detect the direct edit's suspect without writing any byte under .handoff/"
    );
    assert_eq!(
        suspect_items(&list, "link"),
        vec!["AT-100".to_string(), "SPEC-100".to_string()],
        "list must surface SPEC-100 (refines) and AT-100 (verifies) as link suspects from the \
         in-memory-only resync of REQ-100's direct edit: {list}"
    );
}

/// Reviewer-added (session review, round 1): the done_criteria's
/// "一括解除（upstream / layer）" `{layer}` half, which no other test
/// exercised — clearing `{layer: "basic_spec"}` must clear SPEC-100's
/// suspect link (a basic_spec item) and leave AT-100's (acceptance) alone.
/// Also pins two argument-handling contracts: an unknown `kinds` entry is
/// rejected instead of silently filtering everything out, and
/// `action="baseline"`'s `scope.doc` accepts a slug (the tool
/// description's "slug or id"), warning on a value that matches nothing.
#[test]
fn layer_bulk_clear_kinds_validation_and_baseline_scope_by_slug() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    let (req_doc_id, _spec_doc_id) = build_v_project(&mut server, &dir);

    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc_id": req_doc_id,
            "body": "# Requirements\n\n### REQ-100 Account lockout\n\n\
                - priority: P1\n\n\
                Updated requirement statement text (layer clear).\n",
        }),
    );

    let (is_error, text) = server.call_raw(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list", "kinds": ["links"] }),
    );
    assert!(is_error, "an unknown kind must be rejected, got: {text}");
    assert!(text.contains("unknown kind"), "{text}");

    let clear = server.call(
        "handoff_trace_suspect",
        json!({
            "project_dir": dir.to_string_lossy(),
            "action": "clear",
            "targets": [{"layer": "basic_spec"}],
            "reason": "basic_spec reviewed against the new REQ-100 text",
        }),
    );
    assert_eq!(clear["cleared"]["links"], 1, "{clear}");
    assert!(clear["clear_id"].is_string(), "{clear}");

    let list = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    assert_eq!(
        suspect_items(&list, "link"),
        vec!["AT-100".to_string()],
        "only the acceptance-layer suspect must remain: {list}"
    );

    // Unbaselined reference in a second project, scoped by slug.
    let dir2 = tmp.path().join("proj2");
    std::fs::create_dir_all(&dir2).unwrap();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir2.to_string_lossy(), "project_name": "trace-suspect-scope-e2e" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir2.to_string_lossy(),
            "slug": "spec-scope-e2e",
            "title": "Basic spec",
            "layer": "basic_spec",
            "body": "# Basic spec\n\n### SPEC-400 Early reference\n\n\
                - refines: REQ-400\n\n\
                References a requirement that does not exist yet.\n",
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir2.to_string_lossy(),
            "slug": "req-scope-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-400 Now it exists\n\nText.\n",
        }),
    );
    let by_slug = server.call(
        "handoff_trace_suspect",
        json!({
            "project_dir": dir2.to_string_lossy(),
            "action": "baseline",
            "scope": {"doc": "spec-scope-e2e"},
        }),
    );
    assert_eq!(by_slug["baselined"]["links"], 1, "{by_slug}");
    let other_slug = server.call(
        "handoff_trace_suspect",
        json!({
            "project_dir": dir2.to_string_lossy(),
            "action": "baseline",
            "scope": {"doc": "req-scope-e2e"},
        }),
    );
    assert_eq!(other_slug["baselined"]["links"], 0, "{other_slug}");
    let unknown = server.call(
        "handoff_trace_suspect",
        json!({
            "project_dir": dir2.to_string_lossy(),
            "action": "baseline",
            "scope": {"doc": "no-such-doc"},
        }),
    );
    assert_eq!(unknown["baselined"]["links"], 0, "{unknown}");
    assert!(
        unknown["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap_or("").contains("matches no document")),
        "{unknown}"
    );
}

/// Directly overwrites REQ-100's on-disk statement text in
/// `req-suspect-e2e`'s `_doc.<slug>.md` (`build_v_project`'s slug) — a raw
/// `std::fs::write`, never going through `handoff_doc_save`/any MCP tool, so
/// the document's stored (frontmatter) `def_hash` is left exactly as it was
/// before this call (same pattern `tests/layer_sync_e2e.rs`'s hand-edit
/// tests already use).
fn hand_edit_req_100_body(dir: &std::path::Path, old_text: &str, new_text: &str) {
    let md_path = dir.join(".handoff/docs").join("_doc.req-suspect-e2e.md");
    let on_disk = std::fs::read_to_string(&md_path).unwrap();
    assert!(
        on_disk.contains(old_text),
        "expected {old_text:?} in {md_path:?}: {on_disk}"
    );
    let edited = on_disk.replace(old_text, new_text);
    std::fs::write(&md_path, &edited).unwrap();
}

/// Reviewer-added (rework round 1, MAJOR — R-05): `action="clear"` must
/// resync a layer document that was edited directly on disk (VSCode, git
/// pull) *before* selecting suspects — never take a stale stored `def_hash`.
/// Reproduces the reviewer's exact repro on the real binary: hand-edit
/// REQ-100's body with no intervening `doc_save`/`trace_report`, then call
/// `clear` straight away. Pre-fix this returned "no suspects matched"
/// because `handle_clear` read the unsynced (stale) stored `def_hash` for
/// REQ-100 straight off disk. The round-trip second half (hand-edit back to
/// the original text, clear again) is the part that actually proves the
/// first `clear` wrote the *post-edit* hash as the new baseline rather than
/// merely detecting the suspect once and stopping: if the first `clear` had
/// left the baseline stale (bug) or written some placeholder, reverting to
/// the original text would find current == baseline again and report zero
/// matches — this test would then fail on its second `assert_eq!`.
#[test]
fn clear_resyncs_a_directly_edited_layer_doc_with_no_intervening_sync_first() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    build_v_project(&mut server, &dir);

    let original_text = "Original requirement statement text.\n";
    let hand_edited_text = "Direct hand-edit text, no intervening sync (round A).\n";

    // Hand-edit REQ-100 directly, then call `clear` with *no* `list`,
    // `doc_save`, or `trace_report` in between (the reviewer's exact repro
    // shape) — this must still see and clear both of REQ-100's 1-hop
    // dependents.
    hand_edit_req_100_body(&dir, original_text, hand_edited_text);
    let clear1 = server.call(
        "handoff_trace_suspect",
        json!({
            "project_dir": dir.to_string_lossy(),
            "action": "clear",
            "targets": [{"upstream": "REQ-100"}],
            "reason": "reviewed the direct hand-edit (round A)",
        }),
    );
    assert_eq!(
        clear1["cleared"]["links"], 2,
        "clear must resync the hand-edited REQ-100 before selecting suspects, \
         not read the stale stored def_hash straight off disk: {clear1}"
    );
    assert!(
        clear1["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|w| !w.as_str().unwrap_or("").contains("no suspects matched")),
        "{clear1}"
    );

    // Hand-edit REQ-100 back to the *original* text (still no doc_save in
    // between) — if the first `clear` really recorded the post-edit hash as
    // the new baseline, this must be suspect again (current == original !=
    // baseline == hand_edited_text's hash).
    hand_edit_req_100_body(&dir, hand_edited_text, original_text);
    let clear2 = server.call(
        "handoff_trace_suspect",
        json!({
            "project_dir": dir.to_string_lossy(),
            "action": "clear",
            "targets": [{"upstream": "REQ-100"}],
            "reason": "reverted the direct hand-edit (round A)",
        }),
    );
    assert_eq!(
        clear2["cleared"]["links"], 2,
        "the first clear must have written the post-edit hash as the new baseline — \
         reverting to the original text must therefore be suspect again: {clear2}"
    );
}

/// Reviewer-added (rework round 1, MAJOR — R-05), the "other order": a real
/// suspect already exists (created through the normal `doc_save` path, so
/// its stored `def_hash` legitimately advanced once), and *then* the same
/// upstream document is hand-edited directly again before `clear` runs.
/// Pre-fix, `clear` read the stored `def_hash` frozen at the first (synced)
/// edit and wrote *that* stale value as the new baseline/audit `to_hash`
/// instead of the second, unsynced hand-edit's real current value — so the
/// suspect would incorrectly reappear (or not) depending on which of the two
/// texts the document is reverted to. This test hand-edits back to the
/// *first* (synced) text and asserts the suspect **is** there (only possible
/// if `clear` actually recorded the second hand-edit's hash, not the first
/// sync's).
#[test]
fn clear_resyncs_again_when_a_synced_suspect_is_then_hand_edited_a_second_time() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    let (req_doc_id, _spec_doc_id) = build_v_project(&mut server, &dir);

    let synced_text = "# Requirements\n\n### REQ-100 Account lockout\n\n\
        - priority: P1\n\n\
        Synced update text (round B1).\n";

    // A real, synced edit through `doc_save` — `list` must show the suspect
    // this creates (sanity check that this setup is actually exercising the
    // "suspect already exists" half of the scenario, not starting from
    // nothing).
    server.call(
        "handoff_doc_save",
        json!({ "project_dir": dir.to_string_lossy(), "doc_id": req_doc_id, "body": synced_text }),
    );
    let list_after_sync = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    assert_eq!(
        suspect_items(&list_after_sync, "link").len(),
        2,
        "{list_after_sync}"
    );

    // Hand-edit REQ-100 a *second* time, bypassing `doc_save` entirely — the
    // stored `def_hash` on disk is still the round-B1 synced value; the raw
    // body is now a third, never-synced text.
    let hand_edited_text_2 = "Direct hand-edit text, unsynced (round B2).\n";
    hand_edit_req_100_body(&dir, "Synced update text (round B1).\n", hand_edited_text_2);

    let clear = server.call(
        "handoff_trace_suspect",
        json!({
            "project_dir": dir.to_string_lossy(),
            "action": "clear",
            "targets": [{"upstream": "REQ-100"}],
            "reason": "reviewed round B1, unaware of the round B2 hand-edit yet",
        }),
    );
    assert_eq!(clear["cleared"]["links"], 2, "{clear}");

    // Hand-edit REQ-100 back to the round-B1 *synced* text (no doc_save).
    // If `clear` above had wrongly recorded round-B1's stale stored hash as
    // the new baseline (the bug), this text would now match that baseline
    // and report zero suspects. If it correctly recorded round-B2's
    // (resynced) hash, round-B1's text is different from that baseline and
    // must be suspect again.
    hand_edit_req_100_body(&dir, hand_edited_text_2, "Synced update text (round B1).\n");
    let clear_back = server.call(
        "handoff_trace_suspect",
        json!({
            "project_dir": dir.to_string_lossy(),
            "action": "clear",
            "targets": [{"upstream": "REQ-100"}],
            "reason": "reverted to the round B1 text",
        }),
    );
    assert_eq!(
        clear_back["cleared"]["links"], 2,
        "the previous clear must have recorded round B2's (resynced) hash as the new \
         baseline, not round B1's stale stored one — otherwise reverting to round B1's \
         text would wrongly show zero suspects: {clear_back}"
    );
}

/// Reviewer-added (rework round 1, MAJOR — R-05): the same "must resync
/// before selecting" requirement, but for `action="baseline"` with
/// `dry_run=false` — an unbaselined link's `current_hash` must come from a
/// freshly-resynced upstream document, not a stale stored `def_hash`.
/// `clear` (already proven to resync correctly by the two tests above) is
/// used here purely as an independent read-probe: it resyncs before
/// comparing, so calling it after reverting the upstream to its pre-hand-edit
/// text tells us, unambiguously, which hash `baseline(dry_run=false)` had
/// actually written.
#[test]
fn baseline_apply_resyncs_a_directly_edited_upstream_before_recording_it() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();

    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "trace-suspect-baseline-resync-e2e" }),
    );

    // SPEC-600 refines REQ-600 before REQ-600 exists — dangling, so this
    // reference stays unbaselined even once REQ-600 is created afterward
    // (same as `unbaselined_link_and_the_baseline_action` above).
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "spec-baseline-resync-e2e",
            "title": "Basic spec",
            "layer": "basic_spec",
            "body": "# Basic spec\n\n### SPEC-600 Early reference\n\n\
                - refines: REQ-600\n\n\
                References a requirement that does not exist yet.\n",
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "req-baseline-resync-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-600 Now it exists\n\n\
                Original requirement statement text (round C1).\n",
        }),
    );

    let list0 = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    assert_eq!(list0["unbaselined"]["links"], 1, "{list0}");

    // Hand-edit REQ-600 directly (bypassing `doc_save`) — no `list`/
    // `doc_save`/`trace_report` in between, so the stored `def_hash` on disk
    // is still round C1's.
    let md_path = dir
        .join(".handoff/docs")
        .join("_doc.req-baseline-resync-e2e.md");
    let round_c1_text = "Original requirement statement text (round C1).\n";
    let round_c2_text = "Direct hand-edit text, unsynced (round C2).\n";
    let on_disk = std::fs::read_to_string(&md_path).unwrap();
    std::fs::write(&md_path, on_disk.replace(round_c1_text, round_c2_text)).unwrap();

    let apply = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "baseline", "dry_run": false }),
    );
    assert_eq!(apply["baselined"]["links"], 1, "{apply}");

    // Revert REQ-600 back to the round-C1 text (again, no `doc_save` in
    // between) and use `clear` (which does resync first) as the read-probe:
    // if `baseline(apply)` above recorded round C2's (correctly resynced)
    // hash, reverting to round C1's text must now be suspect.
    let on_disk_2 = std::fs::read_to_string(&md_path).unwrap();
    std::fs::write(&md_path, on_disk_2.replace(round_c2_text, round_c1_text)).unwrap();

    let clear = server.call(
        "handoff_trace_suspect",
        json!({
            "project_dir": dir.to_string_lossy(),
            "action": "clear",
            "targets": [{"upstream": "REQ-600"}],
            "reason": "probing which hash baseline(apply) actually recorded",
        }),
    );
    assert_eq!(
        clear["cleared"]["links"], 1,
        "baseline(dry_run=false) must have recorded round C2's resynced hash as SPEC-600's \
         new baseline, not round C1's stale stored one — otherwise reverting to round C1's \
         text would wrongly show zero suspects: {clear}"
    );
}
