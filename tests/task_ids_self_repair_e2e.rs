//! Real-binary E2E reproduction for t360.42 B1 (BLOCKER, M1 adversarial
//! review, wiki/220-vmodel-integration-design.md §2.5/§4.3): the exact
//! repro steps from the review — hand-edit a task file to add a requirement
//! link, call `handoff_doc_req_status` (which refreshes
//! `_requirements_summary.json`'s own input fingerprint as a side effect),
//! then call `handoff_trace_report` (whose self-repair used to compare
//! against that just-refreshed summary fingerprint and wrongly conclude
//! nothing had changed since the last full rebuild) — the hand-added link
//! must be picked up by `trace_report`'s self-repair and reflected in the
//! target SubItem's `task_ids`.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
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

    fn call_raw(&mut self, name: &str, arguments: Value) -> String {
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
        assert!(
            !resp["result"]["isError"].as_bool().unwrap_or(false),
            "{name} failed: {resp}"
        );
        resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        let text = self.call_raw(name, arguments);
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn unique_slug(label: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{label}-{n}")
}

fn find_sub_item_by_stable_id<'a>(status: &'a Value, stable_id: &str) -> &'a Value {
    status["items"]
        .as_array()
        .expect("items array")
        .iter()
        .flat_map(|i| i["sub_items"].as_array().expect("sub_items array"))
        .find(|s| s["stable_id"] == stable_id)
        .unwrap_or_else(|| panic!("stable_id {stable_id} not found in {status}"))
}

#[test]
fn hand_edited_task_link_survives_doc_req_status_then_is_picked_up_by_trace_report_self_repair() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let handoff = dir.join(".handoff");

    let mut server = Server::spawn();

    let init = server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "self-repair-e2e" }),
    );
    assert!(
        init.get("error").is_none() || init["error"].is_null(),
        "init failed: {init}"
    );

    // One layer document with two requirement SubItems.
    let slug = unique_slug("self-repair-spec");
    let body = "# Spec\n\n### REQ-001 First requirement\n\nBody one.\n\n### REQ-002 Second requirement\n\nBody two.\n";
    let saved = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": slug,
            "title": "Self-repair spec",
            "body": body,
            "layer": "basic_spec",
        }),
    );
    let doc_id = saved["doc_id"].as_str().expect("doc_id").to_string();

    // t1 links REQ-001 via the normal API path (baseline; establishes an
    // initial full-rebuild fingerprint via the create-time link).
    let created_t1 = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "title": "Implement REQ-001", "requirement_ids": ["REQ-001"] },
        }),
    );
    let t1_id = created_t1
        .trim_start_matches("Created task ")
        .split(':')
        .next()
        .unwrap_or_default()
        .to_string();
    assert!(!t1_id.is_empty(), "could not extract t1 id: {created_t1}");

    // t2 is created with no links via the API.
    let created_t2 = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "title": "Implement REQ-002" },
        }),
    );
    let t2_id = created_t2
        .trim_start_matches("Created task ")
        .split(':')
        .next()
        .unwrap_or_default()
        .to_string();
    assert!(!t2_id.is_empty(), "could not extract t2 id: {created_t2}");

    // Hand-edit t2's task file on disk to add a requirement link to
    // REQ-002 — bypassing every live link-change path (`update_task`,
    // `doc_verify link_task`) entirely, exactly as the review repro
    // describes.
    let t2_dir = std::fs::read_dir(handoff.join("tasks"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&format!("{t2_id}-")))
        })
        .unwrap_or_else(|| panic!("could not find task dir for {t2_id}"));
    let t2_file = t2_dir.join("_task.todo.json");
    let mut t2_json: Value =
        serde_json::from_str(&std::fs::read_to_string(&t2_file).unwrap()).unwrap();
    if t2_json.get("task_links").is_none() {
        t2_json["task_links"] = json!([]);
    }
    t2_json["task_links"]
        .as_array_mut()
        .expect("task_links array")
        .push(json!({
            "target": doc_id,
            "link_type": "requirement",
            "label": "REQ-002",
        }));
    std::fs::write(&t2_file, serde_json::to_string_pretty(&t2_json).unwrap()).unwrap();

    // handoff_doc_req_status refreshes `_requirements_summary.json`'s own
    // fingerprint as a side effect (unrelated to the full-rebuild gate) —
    // this is the step that used to poison the old (broken) gate.
    server.call(
        "handoff_doc_req_status",
        json!({ "project_dir": dir.to_string_lossy() }),
    );

    // trace_report's self-repair must still notice the hand-added link and
    // fold it into REQ-002's SubItem.task_ids.
    server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy() }),
    );

    let status = server.call(
        "handoff_doc_verify_status",
        json!({ "project_dir": dir.to_string_lossy(), "doc_id": doc_id, "include_items": true }),
    );
    let req_002 = find_sub_item_by_stable_id(&status, "REQ-002");
    assert_eq!(
        req_002["task_ids"].as_array().unwrap(),
        &vec![Value::String(t2_id.clone())],
        "hand-edited task_links entry must be picked up by trace_report's self-repair \
         even after an intervening handoff_doc_req_status call: {status}"
    );
}

/// Rework round 2 integration feedback (BLOCKER): `rebuild_item_task_ids_full`
/// writes its fingerprint file to `.handoff/docs/_task_ids_rebuild.json` via
/// `write_task_ids_rebuild_fingerprint`, which — unlike every other doc-write
/// path (`write_doc_with_body`, `write_trace_report`, ...) — did not call
/// `ensure_docs_dir` first. On a brand-new project that only ever creates
/// tasks (never calls `handoff_doc_save`, so `.handoff/docs/` doesn't exist
/// yet), this made the fingerprint write fail with a hard I/O error, which
/// propagated out of both `handoff_trace_report` (self-repair, `force:
/// false`) and `handoff_doc_repair_task_ids` (`force: true`) as `isError:
/// true`. Neither tool should require a document to already exist.
#[test]
fn trace_report_and_repair_task_ids_work_on_a_docs_less_project() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let handoff = dir.join(".handoff");

    let mut server = Server::spawn();

    let init = server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "docs-less-e2e" }),
    );
    assert!(
        init.get("error").is_none() || init["error"].is_null(),
        "init failed: {init}"
    );

    // A task is created, but no `handoff_doc_save` call is ever made — so
    // `.handoff/docs/` does not exist yet.
    server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "title": "A task with no linked doc" },
        }),
    );
    assert!(
        !handoff.join("docs").exists(),
        "test setup assumption violated: docs/ dir must not exist yet"
    );

    // `handoff_trace_report`'s self-repair (force: false) must not error out
    // just because `.handoff/docs/` doesn't exist yet.
    server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy() }),
    );

    // `handoff_doc_repair_task_ids` (force: true) must likewise succeed.
    server.call(
        "handoff_doc_repair_task_ids",
        json!({ "project_dir": dir.to_string_lossy() }),
    );
}
