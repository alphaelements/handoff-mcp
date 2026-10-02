//! Real-binary E2E for M2-15 (wiki/260-vmodel-m2-design.md §4.8/§11 Q5,
//! FR-601): `handoff_doc_save(task_ids=...)` derives `doc.task_ids` from the
//! task side it just wrote rather than echoing the caller's argument back
//! verbatim; and the document-level self-repair
//! (`handoff_doc_repair_task_ids`) is append-only, with any disagreement it
//! cannot resolve on its own surfaced by `handoff_trace_lint`'s
//! `task_ids_drift` rule.
//!
//! This file originally also covered `handoff_doc_verify(action="link_task")`
//! — that action was removed at the M3 release (wiki/270-vmodel-m3-design.md
//! §4.8; use `handoff_update_task(requirement_ids=...)` instead), and its
//! tests were removed along with it (t360.40.12).

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
        (
            is_error,
            resp["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        )
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

fn unique_slug(label: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{label}-{n}")
}

fn created_task_id(create_resp_text: &str) -> String {
    create_resp_text
        .strip_prefix("Created task ")
        .and_then(|rest| rest.split(':').next())
        .unwrap_or_else(|| panic!("expected 'Created task {{id}}: ...', got {create_resp_text:?}"))
        .to_string()
}

/// Finds a task's directory under `.handoff/tasks` by the task's own id
/// prefix — task dirs are named `<id>-<slug>`, not just `<id>`, mirroring
/// `tests/task_ids_self_repair_e2e.rs`'s own lookup.
fn find_task_dir(handoff: &std::path::Path, task_id: &str) -> PathBuf {
    std::fs::read_dir(handoff.join("tasks"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&format!("{task_id}-")))
        })
        .unwrap_or_else(|| panic!("could not find task dir for {task_id}"))
}

fn read_task_json(task_dir: &std::path::Path) -> (PathBuf, Value) {
    let file = std::fs::read_dir(task_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("_task.") && n.ends_with(".json"))
        })
        .expect("task json file");
    let json: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    (file, json)
}

#[test]
fn doc_save_task_ids_derives_from_the_task_side_over_real_stdio() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "m2-15-e2e-save" }),
    );

    let created = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "title": "Real task" },
        }),
    );
    assert!(!created.0, "create_task failed: {}", created.1);
    let task_id = created_task_id(&created.1);

    let slug = unique_slug("m2-15-doc-save-derive");
    let saved = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": slug,
            "title": "Doc with one bad link",
            "body": "# H\n\nbody\n",
            "task_ids": [&task_id, "t-does-not-exist"],
        }),
    );
    let doc_id = saved["doc_id"].as_str().unwrap().to_string();

    let meta = server.call(
        "handoff_doc_get",
        json!({ "project_dir": dir.to_string_lossy(), "doc_id": &doc_id }),
    );
    assert_eq!(
        meta["task_ids"].as_array().unwrap(),
        &vec![json!(task_id)],
        "the unresolved id must not be echoed back into doc.task_ids: {meta}"
    );
}

/// M2-15's append-only document-level self-repair + `task_ids_drift` lint
/// coverage, both directions:
///
/// 1. A `TaskLink{doc}` the task side has, that `doc.task_ids` is missing
///    (simulating a hand-added link, or one `doc_save` never got the chance
///    to add), is *appended* by `handoff_doc_repair_task_ids` — never
///    reported as a lingering drift once appended.
/// 2. An id in `doc.task_ids` whose `TaskLink{doc}` was removed by hand on
///    the task side is *never removed* by the repair tool (append-only),
///    and `handoff_trace_lint`'s `task_ids_drift` rule reports it with the
///    document's slug instead.
#[test]
fn doc_level_task_ids_self_repair_is_append_only_and_drift_is_reported_by_trace_lint() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let handoff = dir.join(".handoff");

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "m2-15-e2e-drift" }),
    );

    let slug = unique_slug("m2-15-doc-level-drift");
    let saved = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": slug,
            "title": "Doc-level drift fixture",
            "body": "# H\n\nbody\n",
        }),
    );
    let doc_id = saved["doc_id"].as_str().unwrap().to_string();

    // t_manual: hand-added `TaskLink{doc}` the task side has, but
    // `doc.task_ids` never got (bypassing doc_save entirely).
    let created_manual = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "title": "Hand-linked task" },
        }),
    );
    assert!(!created_manual.0, "{}", created_manual.1);
    let t_manual_id = created_task_id(&created_manual.1);
    let t_manual_dir = find_task_dir(&handoff, &t_manual_id);
    let (t_manual_file, mut t_manual_json) = read_task_json(&t_manual_dir);
    t_manual_json["task_links"] = json!([{
        "target": doc_id,
        "link_type": "doc",
        "label": "Doc-level drift fixture",
    }]);
    std::fs::write(
        &t_manual_file,
        serde_json::to_string_pretty(&t_manual_json).unwrap(),
    )
    .unwrap();

    // t_normal: linked the normal way (doc_save), then its reverse link is
    // removed by hand from the task side only — `doc.task_ids` still lists
    // it.
    let created_normal = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "title": "Normally-linked task" },
        }),
    );
    assert!(!created_normal.0, "{}", created_normal.1);
    let t_normal_id = created_task_id(&created_normal.1);
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc_id": &doc_id,
            "task_ids": [&t_normal_id],
        }),
    );
    let t_normal_dir = find_task_dir(&handoff, &t_normal_id);
    let (t_normal_file, mut t_normal_json) = read_task_json(&t_normal_dir);
    t_normal_json["task_links"] = json!([]);
    std::fs::write(
        &t_normal_file,
        serde_json::to_string_pretty(&t_normal_json).unwrap(),
    )
    .unwrap();

    // `handoff_trace_lint` (read-only, E6) must report the document-level
    // drift for t_normal's now-orphaned `doc.task_ids` entry — and must not
    // write anything to `.handoff/` while doing so. Document metadata
    // (including `task_ids`) lives in `_doc.<slug>.md`'s YAML frontmatter
    // (v5 rearchitecture — there is no separate `.json` sidecar for a
    // current-format document), so a byte-for-byte compare of that one file
    // is sufficient to catch a write.
    let doc_md_file = handoff.join("docs").join(format!("_doc.{slug}.md"));
    let before: Vec<u8> = std::fs::read(&doc_md_file).unwrap();
    let lint = server.call(
        "handoff_trace_lint",
        json!({ "project_dir": dir.to_string_lossy() }),
    );
    let after: Vec<u8> = std::fs::read(&doc_md_file).unwrap();
    assert_eq!(
        before, after,
        "trace_lint is read-only (E6) and must not write the document"
    );
    let findings = lint["findings"].as_array().expect("findings array");
    let drift = findings
        .iter()
        .find(|f| f["rule"] == "task_ids_drift" && f["doc"] == slug)
        .unwrap_or_else(|| {
            panic!("expected a document-level task_ids_drift finding for {slug}, got {findings:?}")
        });
    assert!(
        drift["item"].is_null(),
        "document-level drift carries no item id: {drift}"
    );
    assert!(
        drift["message"]
            .as_str()
            .unwrap_or_default()
            .contains(&t_normal_id),
        "message should mention the orphaned task id: {drift}"
    );

    // The explicit repair tool must append t_manual's id (its TaskLink{doc}
    // has no counterpart in doc.task_ids yet) while leaving t_normal's
    // now-orphaned id untouched (append-only — never removed here).
    let repair = server.call(
        "handoff_doc_repair_task_ids",
        json!({ "project_dir": dir.to_string_lossy() }),
    );
    assert_eq!(repair["doc_task_ids_appended"], 1, "{repair}");

    let meta = server.call(
        "handoff_doc_get",
        json!({ "project_dir": dir.to_string_lossy(), "doc_id": &doc_id }),
    );
    let mut task_ids: Vec<String> = meta["task_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    task_ids.sort();
    let mut expected = vec![t_manual_id.clone(), t_normal_id.clone()];
    expected.sort();
    assert_eq!(
        task_ids, expected,
        "t_manual's id must be appended, and t_normal's orphaned id must survive untouched"
    );
}

/// Reviewer fix (M2-S10 round 1, §4.8's named case): a document whose
/// *only* `task_ids` entry is orphaned — no task declares a `TaskLink{doc}`
/// for it at all anymore — must still be reported by `task_ids_drift`. The
/// E6 read-only pass used to skip any document with no task-side doc link
/// whatsoever, so the drift above was only caught when some *other* task
/// still linked the same document.
#[test]
fn trace_lint_reports_doc_level_drift_when_no_task_links_the_document_at_all() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let handoff = dir.join(".handoff");

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "m2-15-e2e-sole-orphan" }),
    );

    let created = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "title": "Only linked task" },
        }),
    );
    assert!(!created.0, "{}", created.1);
    let task_id = created_task_id(&created.1);

    let slug = unique_slug("m2-15-sole-orphan");
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": slug,
            "title": "Sole-orphan fixture",
            "body": "# H\n\nbody\n",
            "task_ids": [&task_id],
        }),
    );

    // Remove the task side's only `TaskLink{doc}` by hand.
    let task_dir = find_task_dir(&handoff, &task_id);
    let (task_file, mut task_json) = read_task_json(&task_dir);
    task_json["task_links"] = json!([]);
    std::fs::write(
        &task_file,
        serde_json::to_string_pretty(&task_json).unwrap(),
    )
    .unwrap();

    let lint = server.call(
        "handoff_trace_lint",
        json!({ "project_dir": dir.to_string_lossy() }),
    );
    let findings = lint["findings"].as_array().expect("findings array");
    let drift = findings
        .iter()
        .find(|f| f["rule"] == "task_ids_drift" && f["doc"] == slug)
        .unwrap_or_else(|| {
            panic!("expected a document-level task_ids_drift finding for {slug}, got {findings:?}")
        });
    assert!(drift["item"].is_null(), "{drift}");
    assert!(
        drift["message"]
            .as_str()
            .unwrap_or_default()
            .contains(&task_id),
        "{drift}"
    );
}
