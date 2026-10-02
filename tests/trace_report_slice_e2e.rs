//! Real-binary E2E tests for M1 `handoff_trace_report`/`handoff_trace_slice`
//! (t360.10/t360.11, wiki/220-vmodel-integration-design.md §3.2/§3.3): spawns
//! the actual `handoff-mcp` binary and drives both tools over real stdio
//! JSON-RPC (same harness style as `tests/trace_record_e2e.rs`), building a
//! small requirement/basic_spec/acceptance/system_test project via
//! `handoff_doc_save`, `handoff_update_task`, and `handoff_trace_record`.

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
        Self::spawn_inner(None)
    }

    /// Same as [`Server::spawn`], but sets `HANDOFF_MCP_DERIVED_WRITE_LOG` so
    /// `_requirements_summary.json` writes can be counted (mirrors
    /// `tests/derived_summary_write_discipline.rs`'s technique).
    fn spawn_with_derived_log(log_path: &std::path::Path) -> Self {
        Self::spawn_inner(Some(log_path))
    }

    fn spawn_inner(derived_log: Option<&std::path::Path>) -> Self {
        let mut cmd = Command::new(binary());
        if let Some(log_path) = derived_log {
            cmd.env("HANDOFF_MCP_DERIVED_WRITE_LOG", log_path);
        }
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

/// `doc_id`s of the four documents `build_project` creates, for tests that
/// need to update one of them afterward (`handoff_doc_save` requires
/// `doc_id` — not just `slug` — to update an existing document).
struct BuiltProjectDocs {
    #[allow(dead_code)]
    requirements_doc_id: String,
    basic_spec_doc_id: String,
    #[allow(dead_code)]
    acceptance_doc_id: String,
    #[allow(dead_code)]
    system_test_doc_id: String,
}

/// Builds a small V-model project: `requirement` doc with REQ-001 (verified
/// by AT-001) and REQ-002 (no verifier — an `unverified` gap), an
/// `acceptance` doc with AT-001, a `basic_spec` doc with SPEC-001 (refines
/// REQ-001), and a `system_test` doc with ST-001 (verifies SPEC-001).
fn build_project(server: &mut Server, dir: &std::path::Path) -> BuiltProjectDocs {
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "trace-report-slice-e2e" }),
    );

    let req = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "requirements-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-001 Account lockout\n\n- priority: P0\n\nAfter 5 failures the account locks.\n\n### REQ-002 Session timeout\n\n- priority: P1\n\nIdle sessions expire after 30 minutes.\n",
        }),
    );
    assert!(
        req.get("doc_id").is_some(),
        "doc_save requirements failed: {req}"
    );

    let spec = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "basic-spec-e2e",
            "title": "Basic spec",
            "layer": "basic_spec",
            "body": "# Basic spec\n\n### SPEC-001 Lockout counter\n\n- refines: REQ-001\n\nMaintain a per-account failure counter.\n",
        }),
    );
    assert!(
        spec.get("doc_id").is_some(),
        "doc_save basic_spec failed: {spec}"
    );

    let at = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "acceptance-e2e",
            "title": "Acceptance tests",
            "layer": "acceptance",
            "body": "# Acceptance\n\n### AT-001 Lockout after 5 failures\n\n- verifies: REQ-001\n- method: manual\n\nFail login 5 times, then confirm the account is locked.\n",
        }),
    );
    assert!(
        at.get("doc_id").is_some(),
        "doc_save acceptance failed: {at}"
    );

    let st = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "system-test-e2e",
            "title": "System tests",
            "layer": "system_test",
            "body": "# System tests\n\n### ST-001 Counter increments\n\n- verifies: SPEC-001\n- method: auto\n\nAssert the counter increments on each failed login.\n",
        }),
    );
    assert!(
        st.get("doc_id").is_some(),
        "doc_save system_test failed: {st}"
    );

    BuiltProjectDocs {
        requirements_doc_id: req["doc_id"].as_str().unwrap().to_string(),
        basic_spec_doc_id: spec["doc_id"].as_str().unwrap().to_string(),
        acceptance_doc_id: at["doc_id"].as_str().unwrap().to_string(),
        system_test_doc_id: st["doc_id"].as_str().unwrap().to_string(),
    }
}

#[test]
fn trace_report_aggregates_coverage_and_gaps_over_real_stdio() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);

    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy() }),
    );

    let in_use: Vec<String> = report["trace_layers"]["in_use"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    for layer in ["requirement", "basic_spec", "acceptance", "system_test"] {
        assert!(
            in_use.contains(&layer.to_string()),
            "expected {layer} to be in use: {in_use:?}"
        );
    }
    assert_eq!(report["trace_layers"]["source"], "auto");

    let req_cov = &report["coverage"]["requirement"];
    assert_eq!(req_cov["total"], 2);
    assert_eq!(
        req_cov["horizontal"]["covered"], 1,
        "REQ-001 is verified by AT-001"
    );
    assert_eq!(
        req_cov["horizontal"]["uncovered"], 1,
        "REQ-002 has no verifier"
    );

    let gaps = report["gaps"].as_array().unwrap();
    let has_unverified_req002 = gaps
        .iter()
        .any(|g| g["kind"] == "unverified" && g["item"] == "REQ-002");
    assert!(
        has_unverified_req002,
        "expected an unverified gap for REQ-002: {gaps:?}"
    );
    assert!(
        report["gap_counts"]["unverified"].as_u64().unwrap() >= 1,
        "gap_counts must report the unverified kind: {}",
        report["gap_counts"]
    );

    // The fixture must have more than one gap so both the gap_kinds filter
    // below (which must actually drop something) and the limit/truncation
    // check further down are exercising a real filter/truncation, not a
    // vacuously-true assertion over an empty or single-entry list.
    assert!(
        gaps.len() > 1,
        "fixture must produce more than one gap for this test to be meaningful: {gaps:?}"
    );

    // gap_kinds filters the detail list but not gap_counts. REQ-002 has no
    // verifier (unverified) *and* no refining child (unrefined), so
    // restricting to "unverified" must still return a non-empty, all-
    // "unverified" list that includes it, while gap_counts still reports the
    // "unrefined" kind untouched.
    let filtered = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy(), "gap_kinds": ["unverified"] }),
    );
    let filtered_gaps = filtered["gaps"].as_array().unwrap();
    assert!(
        !filtered_gaps.is_empty(),
        "gap_kinds=[unverified] must return at least REQ-002's unverified gap: {filtered_gaps:?}"
    );
    assert!(
        filtered_gaps.iter().all(|g| g["kind"] == "unverified"),
        "gap_kinds must restrict gaps[] to the requested kind(s): {filtered_gaps:?}"
    );
    assert!(
        filtered_gaps
            .iter()
            .any(|g| g["item"] == "REQ-002" && g["kind"] == "unverified"),
        "gap_kinds=[unverified] must include REQ-002's unverified gap: {filtered_gaps:?}"
    );
    assert!(
        filtered["gap_counts"]["unverified"].as_u64().unwrap() >= 1,
        "gap_counts must stay unfiltered even when gaps[] is restricted: {}",
        filtered["gap_counts"]
    );
    assert!(
        filtered["gap_counts"]["unrefined"].as_u64().unwrap() >= 1,
        "gap_counts must still report the unrefined kind even though gaps[] was filtered to \
         unverified only: {}",
        filtered["gap_counts"]
    );

    // limit truncates gaps[] and notes it in warnings. The fixture has more
    // than one gap (asserted above), so limit=1 always truncates.
    let limited = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy(), "limit": 1 }),
    );
    assert!(limited["gaps"].as_array().unwrap().len() <= 1);
    let warnings = limited["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("truncated")),
        "a limit below the matching gap count must be noted in warnings: {warnings:?}"
    );

    // include_items surfaces layer/side/state/category for every item.
    let with_items = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy(), "include_items": true }),
    );
    let items = with_items["items"].as_array().expect("items array");
    let req001 = items
        .iter()
        .find(|it| it["id"] == "REQ-001")
        .expect("REQ-001 in items[]");
    assert_eq!(req001["layer"], "requirement");
    assert_eq!(req001["side"], "left");
    assert_eq!(req001["category"], "requirement");
    // REQ-001 is verified by AT-001 (not_run, no trace_record yet) and
    // refined by SPEC-001 (also not_run) -> state resolves to not_run.
    assert_eq!(req001["state"], "not_run");

    let at001 = items
        .iter()
        .find(|it| it["id"] == "AT-001")
        .expect("AT-001 in items[]");
    assert_eq!(at001["side"], "right");
    assert_eq!(at001["category"], "check");
}

/// wiki/220 §2.4: `handoff_trace_report` must resync a layer document that
/// was edited directly on disk (not via `handoff_doc_save`) before
/// aggregating — this is the tool's only allowed side effect.
#[test]
fn trace_report_resyncs_a_directly_edited_layer_document() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let handoff = dir.join(".handoff");

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "trace-report-direct-edit-e2e" }),
    );
    let saved = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "req-direct-edit",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-100 First requirement\n\nOriginal body.\n",
        }),
    );
    let doc_id = saved["doc_id"].as_str().unwrap().to_string();

    // First trace_report call establishes REQ-100's baseline body_raw_hash.
    let first = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy(), "include_items": true }),
    );
    let ids: Vec<String> = first["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|it| it["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, vec!["REQ-100".to_string()]);

    let body_path = handoff.join("docs").join("_doc.req-direct-edit.md");
    let body_raw_hash_before = frontmatter_value(&body_path, &["source", "body_raw_hash"])
        .as_str()
        .expect("source.body_raw_hash must be a string after the first trace_report call")
        .to_string();

    // Directly rewrite the body file on disk, bypassing handoff_doc_save
    // entirely, adding a second item.
    let existing = std::fs::read_to_string(&body_path).unwrap();
    // Preserve the YAML frontmatter block, replace only the body after it.
    let mut parts = existing.splitn(3, "---\n");
    let _empty = parts.next().unwrap_or_default();
    let frontmatter = parts.next().unwrap_or_default();
    let new_body = "\n# Requirements\n\n### REQ-100 First requirement\n\nOriginal body.\n\n### REQ-101 Second requirement (added by hand)\n\nAdded directly on disk.\n";
    let rewritten = format!("---\n{frontmatter}---\n{new_body}");
    std::fs::write(&body_path, rewritten).unwrap();

    let second = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy(), "include_items": true }),
    );
    let mut ids2: Vec<String> = second["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|it| it["id"].as_str().unwrap().to_string())
        .collect();
    ids2.sort();
    assert_eq!(
        ids2,
        vec!["REQ-100".to_string(), "REQ-101".to_string()],
        "trace_report must pick up a directly-edited layer document's new item: {second}"
    );

    // The on-disk document's own verification matrix must have been
    // rewritten too (not just the in-memory response): its `source.
    // body_raw_hash` must have advanced past the pre-edit baseline (proof a
    // real resync ran, not just a response built from stale in-memory data),
    // and its `sub_items` frontmatter must now list REQ-101 alongside
    // REQ-100.
    let doc_yaml = read_frontmatter(&body_path);
    assert_eq!(doc_yaml["id"].as_str(), Some(doc_id.as_str()));
    let body_raw_hash_after = doc_yaml["source"]["body_raw_hash"]
        .as_str()
        .expect("source.body_raw_hash must be a string after the second trace_report call");
    assert_ne!(
        body_raw_hash_after, body_raw_hash_before,
        "the on-disk document's source.body_raw_hash must advance once the direct edit is \
         resynced: {doc_yaml:?}"
    );
    let stable_ids = collect_stable_ids(&doc_yaml);
    assert!(
        stable_ids.contains(&"REQ-101".to_string()),
        "the on-disk document's verification matrix must list REQ-101 after resync: \
         {stable_ids:?}"
    );
    assert!(
        stable_ids.contains(&"REQ-100".to_string()),
        "REQ-100 must still be present after resync: {stable_ids:?}"
    );
}

/// Reads `path`'s YAML frontmatter block (`---\n<yaml>\n---\n<body>`) as a
/// `serde_yaml::Value` — same technique as `tests/doc_verify.rs`'s
/// `clear_all_stable_ids`.
fn read_frontmatter(path: &std::path::Path) -> serde_yaml::Value {
    let content = std::fs::read_to_string(path).unwrap();
    let rest = content.strip_prefix("---\n").expect("frontmatter fence");
    let (yaml_block, _body) = rest.split_once("\n---\n").expect("closing fence");
    serde_yaml::from_str(yaml_block).expect("valid frontmatter YAML")
}

/// Reads one dotted-path value out of `path`'s frontmatter (e.g. `["source",
/// "body_raw_hash"]`), for capturing a baseline before an edit.
fn frontmatter_value(path: &std::path::Path, keys: &[&str]) -> serde_yaml::Value {
    let mut value = read_frontmatter(path);
    for key in keys {
        value = value.get(key).cloned().unwrap_or(serde_yaml::Value::Null);
    }
    value
}

/// Every `stable_id` recorded under `verification.items[].sub_items[]` in a
/// document's frontmatter (mirrors `tests/doc_verify.rs`'s traversal).
fn collect_stable_ids(doc_yaml: &serde_yaml::Value) -> Vec<String> {
    let mut out = Vec::new();
    let Some(items) = doc_yaml
        .get("verification")
        .and_then(|v| v.get("items"))
        .and_then(|v| v.as_sequence())
    else {
        return out;
    };
    for item in items {
        let Some(subs) = item.get("sub_items").and_then(|v| v.as_sequence()) else {
            continue;
        };
        for sub in subs {
            if let Some(id) = sub.get("stable_id").and_then(|v| v.as_str()) {
                out.push(id.to_string());
            }
        }
    }
    out
}

#[test]
fn trace_slice_traverses_up_and_down_with_expand_and_truncation() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);

    // Link a task to REQ-001 so task_id-based slicing has a starting set.
    server.call(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "id": "t1", "title": "Implement lockout", "requirement_ids": ["REQ-001"] },
        }),
    );

    // down from REQ-001 must reach AT-001 (verified_by) and SPEC-001
    // (refines_children).
    let down = server.call(
        "handoff_trace_slice",
        json!({ "project_dir": dir.to_string_lossy(), "item": "REQ-001", "direction": "down" }),
    );
    let down_ids: Vec<String> = down["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|it| it["id"].as_str().unwrap().to_string())
        .collect();
    assert!(down_ids.contains(&"REQ-001".to_string()));
    assert!(down_ids.contains(&"AT-001".to_string()), "{down_ids:?}");
    assert!(down_ids.contains(&"SPEC-001".to_string()), "{down_ids:?}");
    assert_eq!(down["truncated"], false);
    // Default (no expand): no statement key on any item.
    for item in down["items"].as_array().unwrap() {
        assert!(item.get("statement").is_none(), "{item}");
    }
    // t360.20.25/M2-07 (wiki/260-vmodel-m2-design.md §4.11): `trace_slice`'s
    // own items[] carry `coverage`/`suspect`/`reverify`/`approval`, not just
    // `_trace_report.json`.
    let req_001_down = down["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|it| it["id"] == "REQ-001")
        .expect("REQ-001 present");
    assert!(
        req_001_down["coverage"]["horizontal"].is_string(),
        "{req_001_down}"
    );
    assert!(req_001_down["coverage"]["vertical"].is_string());
    assert!(req_001_down["suspect"].as_array().is_some());
    assert!(req_001_down["reverify"].is_boolean());
    assert!(req_001_down["approval"].is_string());

    // up from AT-001 must reach REQ-001, not SPEC-001/ST-001 (unrelated
    // branch of the graph).
    let up = server.call(
        "handoff_trace_slice",
        json!({ "project_dir": dir.to_string_lossy(), "item": "AT-001", "direction": "up" }),
    );
    let up_ids: Vec<String> = up["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|it| it["id"].as_str().unwrap().to_string())
        .collect();
    assert!(up_ids.contains(&"REQ-001".to_string()));
    assert!(!up_ids.contains(&"SPEC-001".to_string()), "{up_ids:?}");

    // task_id starting set = REQ-001 (the requirement_ids link above).
    let by_task = server.call(
        "handoff_trace_slice",
        json!({ "project_dir": dir.to_string_lossy(), "task_id": "t1", "direction": "both" }),
    );
    let by_task_ids: Vec<String> = by_task["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|it| it["id"].as_str().unwrap().to_string())
        .collect();
    assert!(by_task_ids.contains(&"REQ-001".to_string()));
    assert!(by_task_ids.contains(&"AT-001".to_string()));

    // expand returns statement text for the requested id only.
    let expanded = server.call(
        "handoff_trace_slice",
        json!({
            "project_dir": dir.to_string_lossy(), "item": "REQ-001", "direction": "down",
            "expand": ["REQ-001"],
        }),
    );
    let req001 = expanded["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|it| it["id"] == "REQ-001")
        .expect("REQ-001 present");
    assert!(
        req001["statement"]
            .as_str()
            .unwrap()
            .contains("After 5 failures the account locks."),
        "{req001}"
    );
    let spec001 = expanded["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|it| it["id"] == "SPEC-001")
        .expect("SPEC-001 present");
    assert!(
        spec001.get("statement").is_none(),
        "only expand-requested ids get a statement: {spec001}"
    );

    // max_items truncates the full reachable set.
    let capped = server.call(
        "handoff_trace_slice",
        json!({
            "project_dir": dir.to_string_lossy(), "item": "REQ-001", "direction": "both",
            "max_items": 1,
        }),
    );
    assert_eq!(capped["items"].as_array().unwrap().len(), 1);
    assert_eq!(capped["truncated"], true);
}

/// t376 (bug found in M-S10): a dangling reference must not consume one of
/// `max_items`' slots. A task's own `requirement_ids` link records are the
/// authority on the task side (wiki/220 §2.5, D3) and are **not** cleaned up
/// when the whole document owning the linked stable_id is later deleted —
/// `handoff_doc_delete` only unlinks a document's own `doc`-type task links,
/// not the reverse `requirement`-type links any of its SubItems' stable_ids
/// still hold. The same is true of an in-place edit that removes just one
/// item from a still-existing document: since rework round 2's MAJOR fix,
/// `sync_layer_items_if_needed` never touches task files either (it only
/// drops the `SubItem` and leaves an informational warning) — so a task-side
/// link surviving a document deletion and a task-side link surviving a body
/// edit are now the same kind of dangling link, not two different cases. So
/// linking a task to a real item and then deleting that item's owning
/// document (or removing the item from the body) leaves a genuinely dangling
/// `task_requirement_links` entry: a stable_id with a task-side link but no
/// `meta` entry anywhere, sitting in `start_ids` right alongside real,
/// resolvable ids.
///
/// `start_ids` is sorted, so BFS walks both depth-0 start ids
/// (`REQ-001`, `REQ-999`) before any of REQ-001's down-neighbors — exactly
/// the bug: with `max_items: 3` the buggy (pre-fix) order was `[REQ-001,
/// REQ-999, SPEC-001, AT-001, ST-001]`, truncating to `[REQ-001, REQ-999,
/// SPEC-001]` and then dropping `REQ-999` for lack of `meta`, leaving only 2
/// real items (REQ-001, SPEC-001) even though 4 real items (REQ-001,
/// SPEC-001, AT-001, ST-001) were reachable.
#[test]
fn trace_slice_max_items_is_not_consumed_by_a_dangling_start_id() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);

    // A second, separate requirement document holding REQ-999 — kept
    // separate from `requirements-e2e` (REQ-001/REQ-002) so deleting it
    // doesn't also remove the real items the rest of this test relies on.
    let dangling_doc = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "dangling-req-e2e",
            "title": "Soon-to-be-deleted requirement",
            "layer": "requirement",
            "body": "# Soon to be deleted\n\n### REQ-999 Will be deleted\n\nThis item's document is deleted right after linking.\n",
        }),
    );
    let dangling_doc_id = dangling_doc["doc_id"].as_str().unwrap().to_string();

    // t1 links to both a real requirement (REQ-001) and REQ-999 —
    // both resolve at link time, so both get a real `requirement`-type
    // task_links entry.
    server.call(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": {
                "id": "t1",
                "title": "Implement lockout",
                "requirement_ids": ["REQ-001", "REQ-999"],
            },
        }),
    );

    // Deleting REQ-999's owning document removes it from `meta`
    // entirely, but (per the doc comment above) leaves t1's reverse
    // `requirement` link to it dangling.
    server.call(
        "handoff_doc_delete",
        json!({ "project_dir": dir.to_string_lossy(), "doc_id": dangling_doc_id }),
    );

    // Down from {REQ-999, REQ-001} reaches 4 real items in total
    // (REQ-001, SPEC-001, AT-001, ST-001) plus the dangling id itself (which
    // carries no `meta` and must never appear in `items[]`). `max_items: 3`
    // must therefore still return exactly 3 *real* items, not 2.
    let sliced = server.call(
        "handoff_trace_slice",
        json!({
            "project_dir": dir.to_string_lossy(), "task_id": "t1", "direction": "down",
            "max_items": 3,
        }),
    );
    let items = sliced["items"].as_array().expect("items array");
    assert_eq!(
        items.len(),
        3,
        "a dangling start id must not consume one of max_items' slots for a real item: {sliced}"
    );
    assert!(
        items.iter().all(|it| it["id"] != "REQ-999"),
        "a dangling id must never appear in items[] (it has no meta to render): {sliced}"
    );
    assert_eq!(
        sliced["truncated"], true,
        "4 real items reachable with max_items=3 must still report truncated=true: {sliced}"
    );
}

/// t376 rework (review round 1, MAJOR): `handle_trace_slice` runs
/// `resync_direct_edited_layer_docs` before building its graph — the tool's
/// one allowed side effect (wiki/220 §2.4) — but was dropping the resulting
/// `warnings` (a `removed: [ids]` notice, an unlink notice, or a
/// collision notice) on the floor instead of returning them in its own
/// `{items, truncated}` response. Since the resync has already persisted by
/// the time `handle_trace_slice` returns, and `trace_slice` is often the
/// first call after a direct `.md` edit (progressive-disclosure entry
/// point), a caller had no way to ever learn a `removed: [...]` warning
/// happened — the next `trace_report`/`trace_slice` call sees nothing left
/// to resync, so the warning is lost for good. This directly edits a layer
/// document's body on disk to drop `REQ-002` (mirrors
/// `trace_report_resyncs_a_directly_edited_layer_document`'s technique) and
/// asserts `handoff_trace_slice`'s own response surfaces the `removed:`
/// warning `sync_layer_items` recorded for it.
#[test]
fn trace_slice_surfaces_a_resync_removed_warning() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let handoff = dir.join(".handoff");

    let mut server = Server::spawn();
    build_project(&mut server, &dir);

    // Establish REQ-001/REQ-002's baseline `source.body_raw_hash` so the
    // *next* call is the one that observes the direct edit as a change.
    server.call(
        "handoff_trace_slice",
        json!({ "project_dir": dir.to_string_lossy(), "item": "REQ-001" }),
    );

    // Directly rewrite the requirements document's body on disk (bypassing
    // `handoff_doc_save`), dropping REQ-002 entirely.
    let body_path = handoff.join("docs").join("_doc.requirements-e2e.md");
    let existing = std::fs::read_to_string(&body_path).unwrap();
    let mut parts = existing.splitn(3, "---\n");
    let _empty = parts.next().unwrap_or_default();
    let frontmatter = parts.next().unwrap_or_default();
    let new_body = "\n# Requirements\n\n### REQ-001 Account lockout\n\n- priority: P0\n\nAfter 5 failures the account locks.\n";
    let rewritten = format!("---\n{frontmatter}---\n{new_body}");
    std::fs::write(&body_path, rewritten).unwrap();

    let sliced = server.call(
        "handoff_trace_slice",
        json!({ "project_dir": dir.to_string_lossy(), "item": "REQ-001" }),
    );
    let warnings = sliced["warnings"]
        .as_array()
        .expect("handoff_trace_slice must return a warnings[] array, same as handoff_trace_report");
    assert!(
        warnings.iter().any(|w| w
            .as_str()
            .unwrap_or_default()
            .contains("removed: [REQ-002]")),
        "a direct-edit removal resynced during handle_trace_slice must be surfaced in its own \
         response, not just persisted silently (it is already too late to see it on the next \
         call): {sliced}"
    );
}

/// wiki/220 §3.3: `direction: "both"` must be the **union** of the up-only
/// and down-only walks from the same start id, not a single BFS that can
/// turn around partway (go up to a parent, then back down through every
/// sibling subtree). Adds `SPEC-002 refines REQ-001` alongside the existing
/// `SPEC-001 refines REQ-001` so SPEC-001 and SPEC-002 become siblings under
/// REQ-001; slicing "both" from SPEC-001 must reach its ancestor REQ-001 but
/// must NOT reach its sibling SPEC-002, which sits on an unrelated branch of
/// the same up-then-down path a single turning BFS would wrongly cross into.
#[test]
fn trace_slice_both_direction_excludes_siblings_reached_only_by_turning_around() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();

    let mut server = Server::spawn();
    let docs = build_project(&mut server, &dir);

    // Add a second basic_spec item, SPEC-002, that also refines REQ-001 —
    // now REQ-001 has two refining children (SPEC-001, SPEC-002).
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc_id": docs.basic_spec_doc_id,
            "title": "Basic spec",
            "layer": "basic_spec",
            "body": "# Basic spec\n\n### SPEC-001 Lockout counter\n\n- refines: REQ-001\n\nMaintain a per-account failure counter.\n\n### SPEC-002 Another spec\n\n- refines: REQ-001\n\nAnother spec refining REQ-001.\n",
        }),
    );

    let up_only = server.call(
        "handoff_trace_slice",
        json!({ "project_dir": dir.to_string_lossy(), "item": "SPEC-001", "direction": "up" }),
    );
    let up_ids: Vec<String> = up_only["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|it| it["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        up_ids,
        vec!["SPEC-001".to_string(), "REQ-001".to_string()],
        "up from SPEC-001 must reach only its ancestor REQ-001: {up_ids:?}"
    );

    let both = server.call(
        "handoff_trace_slice",
        json!({ "project_dir": dir.to_string_lossy(), "item": "SPEC-001", "direction": "both" }),
    );
    let both_ids: Vec<String> = both["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|it| it["id"].as_str().unwrap().to_string())
        .collect();
    assert!(
        both_ids.contains(&"REQ-001".to_string()),
        "both must still reach the ancestor REQ-001: {both_ids:?}"
    );
    assert!(
        !both_ids.contains(&"SPEC-002".to_string()),
        "both from SPEC-001 must NOT reach sibling SPEC-002 by turning around through their \
         shared parent REQ-001 (that is the union-of-two-one-way-walks bug this test guards \
         against): {both_ids:?}"
    );
}

#[test]
fn trace_slice_rejects_an_unknown_item_or_task_id_instead_of_returning_empty() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);

    // An unknown item must fail loudly, not resolve to a successful
    // `{items: [], truncated: false}` that reads as "this item has no
    // links" — that empty shape is otherwise reserved for the item-not-found
    // case, since every real item always includes at least itself.
    let (is_error, text) = server.call_raw(
        "handoff_trace_slice",
        json!({ "project_dir": dir.to_string_lossy(), "item": "NOPE-999" }),
    );
    assert!(
        is_error,
        "an unknown item must be rejected as an error, not returned as an empty success: {text}"
    );

    let (is_error_task, text_task) = server.call_raw(
        "handoff_trace_slice",
        json!({ "project_dir": dir.to_string_lossy(), "task_id": "no-such-task" }),
    );
    assert!(
        is_error_task,
        "an unknown task_id must be rejected as an error: {text_task}"
    );
}

#[test]
fn trace_slice_requires_exactly_one_of_task_id_or_item() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "trace-slice-validation-e2e" }),
    );

    let (is_error, _) = server.call_raw(
        "handoff_trace_slice",
        json!({ "project_dir": dir.to_string_lossy() }),
    );
    assert!(is_error, "neither task_id nor item must be rejected");

    let (is_error2, _) = server.call_raw(
        "handoff_trace_slice",
        json!({ "project_dir": dir.to_string_lossy(), "task_id": "t1", "item": "REQ-001" }),
    );
    assert!(is_error2, "both task_id and item together must be rejected");
}

/// wiki/220 §2.4: `handoff_trace_report`'s layer-doc resync side effect must
/// only write `_requirements_summary.json` when a document actually
/// resynced — a repeat call with no direct edit in between must not rewrite
/// it again (P-M4 write discipline, same as every other derived-file writer
/// in this codebase).
///
/// `handoff_doc_save` already records `source.body_raw_hash` at save time
/// (see `src/mcp/handlers/docs.rs`'s doc_save body-hash bookkeeping), so
/// every layer document `build_project` creates is already "in sync" by the
/// time `handoff_trace_report` runs — there is nothing to resync, and so
/// *neither* call below should write `_requirements_summary.json` at all.
/// The log also records other derived-file writes on this path (notably
/// `runs/_latest.json`, refreshed by `runs::sync` on every trace_report
/// call) — this test counts only lines naming
/// `_requirements_summary.json`, not the raw line count, so it isn't
/// satisfied by an unrelated file's write.
#[test]
fn trace_report_does_not_rewrite_the_summary_when_nothing_changed_since_the_last_call() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let log_path = tmp.path().join("derived_writes.log");

    let mut server = Server::spawn_with_derived_log(&log_path);
    build_project(&mut server, &dir);
    let after_build = summary_write_count(&log_path);
    assert!(
        after_build >= 1,
        "build_project's own handoff_doc_save calls must write the summary at least once"
    );

    // Every layer document is already synced (doc_save records
    // body_raw_hash), so this call has nothing to resync and must add zero
    // summary writes.
    server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy() }),
    );
    let after_first = summary_write_count(&log_path);
    assert_eq!(
        after_first, after_build,
        "a trace_report call over already-synced layer documents must not write the summary \
         (no resync work to do)"
    );

    // A second call with no intervening edit must likewise add zero.
    server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy() }),
    );
    let after_second = summary_write_count(&log_path);
    assert_eq!(
        after_second, after_first,
        "a repeat trace_report call with no direct edit must not rewrite the summary again"
    );
}

/// Number of `_requirements_summary.json` write lines logged so far at
/// `log_path` (`HANDOFF_MCP_DERIVED_WRITE_LOG`'s `"{path}\t{bytes}\n"`
/// format) — excludes other derived files the same log records (e.g.
/// `runs/_latest.json`).
fn summary_write_count(log_path: &std::path::Path) -> usize {
    std::fs::read_to_string(log_path)
        .map(|s| {
            s.lines()
                .filter(|line| line.contains("_requirements_summary.json"))
                .count()
        })
        .unwrap_or(0)
}
