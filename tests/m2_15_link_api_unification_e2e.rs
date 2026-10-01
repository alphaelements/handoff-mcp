//! Real-binary E2E for M2-15 (wiki/260-vmodel-m2-design.md §4.8/§11 Q5,
//! FR-601): `handoff_doc_verify(action="link_task")` delegates to the
//! task-side-primary `apply_requirement_links` path instead of writing
//! `SubItem.task_ids` directly and returns a `deprecated` notice;
//! `handoff_doc_save(task_ids=...)` derives `doc.task_ids` from the task
//! side it just wrote rather than echoing the caller's argument back
//! verbatim; and the document-level self-repair
//! (`handoff_doc_repair_task_ids`) is append-only, with any disagreement it
//! cannot resolve on its own surfaced by `handoff_trace_lint`'s
//! `task_ids_drift` rule.

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
fn link_task_delegates_to_the_task_primary_path_and_returns_deprecated() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "m2-15-e2e" }),
    );

    let slug = unique_slug("m2-15-link-task-spec");
    let body = "# Spec\n\n### REQ-001 First requirement\n\nBody one.\n";
    let saved = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": slug,
            "title": "M2-15 link_task spec",
            "body": body,
            "layer": "requirement",
        }),
    );
    let doc_id = saved["doc_id"].as_str().expect("doc_id").to_string();

    let created = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "title": "Implement REQ-001" },
        }),
    );
    assert!(!created.0, "create_task failed: {}", created.1);
    let task_id = created_task_id(&created.1);

    let link_resp = server.call(
        "handoff_doc_verify",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc_id": &doc_id,
            "action": "link_task",
            "sub_item_id": "REQ-001",
            "task_ids": [&task_id],
        }),
    );

    // Deprecated notice names the replacement tool (§11 Q5).
    assert_eq!(
        link_resp["deprecated"]["replacement"],
        "handoff_update_task"
    );
    assert!(
        link_resp["deprecated"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("deprecated"),
        "{link_resp}"
    );

    // The reverse link landed on the task side with a def_hash baseline —
    // something only `apply_requirement_links` (the delegate) ever stamps;
    // the old direct-write `add_reverse_task_links` never resolved one (see
    // this task's removal of that function).
    let task_resp = server.call(
        "handoff_get_task",
        json!({ "project_dir": dir.to_string_lossy(), "task_id": &task_id }),
    );
    let links = task_resp["task_links"]
        .as_array()
        .or_else(|| task_resp["task"]["task_links"].as_array())
        .expect("task_links present")
        .clone();
    let link = links
        .iter()
        .find(|l| l["link_type"] == "requirement" && l["label"] == "REQ-001")
        .unwrap_or_else(|| panic!("expected reverse link, got {links:?}"));
    assert_eq!(link["role"], "implements");
    assert!(
        link["baseline_hash"].is_string(),
        "apply_requirement_links must stamp a baseline_hash for a brand-new link once the \
         item has a def_hash (layer-synced REQ-001 does): {link}"
    );

    // SubItem.task_ids still reflects the link (apply_requirement_links
    // writes the doc side too, just via the single shared path).
    let status = server.call(
        "handoff_doc_verify_status",
        json!({ "project_dir": dir.to_string_lossy(), "doc_id": &doc_id, "include_items": true }),
    );
    let sub = status["items"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|i| i["sub_items"].as_array().unwrap())
        .find(|s| s["stable_id"] == "REQ-001")
        .expect("REQ-001 sub_item");
    assert_eq!(sub["task_ids"].as_array().unwrap(), &vec![json!(task_id)]);
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

/// Reviewer fix (M2-S10 round 1, §4.8 "link_task が SubItem を先に書かない"):
/// a `link_task` task id that doesn't resolve must be reported as a warning
/// *and* must not be recorded in `SubItem.task_ids` — the delegate writes
/// the SubItem side before it discovers the task is missing, so the
/// unresolved id has to be filtered out before delegating (same rule
/// `doc_save(task_ids)` follows for the document level).
#[test]
fn link_task_with_an_unresolved_task_id_warns_and_does_not_record_it() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "m2-15-e2e-unresolved" }),
    );

    let slug = unique_slug("m2-15-link-task-unresolved");
    let saved = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": slug,
            "title": "M2-15 link_task unresolved",
            "body": "# Spec\n\n### REQ-001 First requirement\n\nBody one.\n",
            "layer": "requirement",
        }),
    );
    let doc_id = saved["doc_id"].as_str().expect("doc_id").to_string();

    let created = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "title": "Real task" },
        }),
    );
    assert!(!created.0, "{}", created.1);
    let task_id = created_task_id(&created.1);

    let link_resp = server.call(
        "handoff_doc_verify",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc_id": &doc_id,
            "action": "link_task",
            "sub_item_id": "REQ-001",
            "task_ids": [&task_id, "t-does-not-exist"],
        }),
    );
    let warnings = link_resp["warnings"].as_array().expect("warnings array");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().is_some_and(|s| s.contains("t-does-not-exist"))),
        "the unresolved task id must be reported: {link_resp}"
    );

    let status = server.call(
        "handoff_doc_verify_status",
        json!({ "project_dir": dir.to_string_lossy(), "doc_id": &doc_id, "include_items": true }),
    );
    let sub = status["items"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|i| i["sub_items"].as_array().unwrap())
        .find(|s| s["stable_id"] == "REQ-001")
        .expect("REQ-001 sub_item");
    assert_eq!(
        sub["task_ids"].as_array().unwrap(),
        &vec![json!(task_id)],
        "only the resolved task id may be recorded on the SubItem side: {sub}"
    );
}

/// t360.20.34 (M2-S10 reviewer proposal 1, wiki/260 §4.8): `link_task` must
/// diff against the task side's own `TaskLink{requirement}` entries
/// (`collect_requirement_task_links`, D3's source of truth), not this
/// SubItem's own (possibly drifted) `task_ids` — otherwise a task linked by
/// hand-editing the task file directly (bypassing `doc_save`/`link_task`,
/// so `SubItem.task_ids` never recorded it) could never be unlinked via
/// `link_task(task_ids=[])`: diffing against the stale, empty doc-side value
/// would see "nothing to remove" and leave the task-side link in place.
#[test]
fn link_task_removes_a_task_side_link_even_when_sub_item_task_ids_never_recorded_it() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let handoff = dir.join(".handoff");

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "m2-15-e2e-drifted-diff" }),
    );

    let slug = unique_slug("m2-15-link-task-drifted-diff");
    let saved = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": slug,
            "title": "M2-15 link_task drifted diff spec",
            "body": "# Spec\n\n### REQ-001 First requirement\n\nBody one.\n",
            "layer": "requirement",
        }),
    );
    let doc_id = saved["doc_id"].as_str().expect("doc_id").to_string();

    let created = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "title": "Hand-linked task" },
        }),
    );
    assert!(!created.0, "{}", created.1);
    let task_id = created_task_id(&created.1);

    // Hand-add the task-side reverse link directly (bypassing `doc_save`/
    // `link_task`/`handoff_update_task(requirement_ids=...)` entirely) —
    // `SubItem.task_ids` on the REQ-001 item never gets a chance to record
    // this, simulating a drift between the two sides.
    let task_dir = find_task_dir(&handoff, &task_id);
    let (task_file, mut task_json) = read_task_json(&task_dir);
    task_json["task_links"] = json!([{
        "target": doc_id,
        "link_type": "requirement",
        "label": "REQ-001",
        "role": "implements",
    }]);
    std::fs::write(
        &task_file,
        serde_json::to_string_pretty(&task_json).unwrap(),
    )
    .unwrap();

    // Confirm the drift: REQ-001's own `task_ids` does not list the task.
    let status_before = server.call(
        "handoff_doc_verify_status",
        json!({ "project_dir": dir.to_string_lossy(), "doc_id": &doc_id, "include_items": true }),
    );
    let sub_before = status_before["items"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|i| i["sub_items"].as_array().unwrap())
        .find(|s| s["stable_id"] == "REQ-001")
        .expect("REQ-001 sub_item");
    assert_eq!(
        sub_before["task_ids"].as_array().unwrap(),
        &Vec::<Value>::new(),
        "fixture setup: SubItem.task_ids must start out NOT reflecting the hand-added link"
    );

    // `link_task(task_ids=[])` must still remove the task-side link — the
    // diff basis is the task side's own `task_links`, not the (drifted,
    // empty-looking) `SubItem.task_ids`.
    server.call(
        "handoff_doc_verify",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc_id": &doc_id,
            "action": "link_task",
            "sub_item_id": "REQ-001",
            "task_ids": [],
        }),
    );

    let task_resp = server.call(
        "handoff_get_task",
        json!({ "project_dir": dir.to_string_lossy(), "task_id": &task_id }),
    );
    let links = task_resp["task_links"]
        .as_array()
        .or_else(|| task_resp["task"]["task_links"].as_array())
        .expect("task_links present");
    assert!(
        links
            .iter()
            .all(|l| !(l["link_type"] == "requirement" && l["label"] == "REQ-001")),
        "the task-side link must be removed even though SubItem.task_ids never had it: {links:?}"
    );
}

/// t360.20.34 (M2-S10 reviewer proposal 2, wiki/260 §4.8): when the same
/// `stable_id` string exists in two different documents (a genuine
/// cross-document collision, wiki/220 §4.2/FR-105), `link_task` must still
/// link to the specific document it was called on (`doc_id`) instead of
/// refusing with "ambiguous" — it already knows exactly which SubItem it
/// means, unlike `handoff_update_task(requirement_ids=...)`, which has no
/// document of its own to disambiguate with and must keep refusing.
#[test]
fn link_task_links_to_its_own_doc_id_when_the_stable_id_is_ambiguous_across_documents() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "m2-15-e2e-ambiguous" }),
    );

    let slug_a = unique_slug("m2-15-link-task-ambiguous-a");
    let saved_a = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": slug_a,
            "title": "Ambiguous spec A",
            "body": "# Spec A\n\n### REQ-700 First owner\n\nBody.\n",
            "layer": "requirement",
        }),
    );
    let doc_id_a = saved_a["doc_id"].as_str().expect("doc_id").to_string();

    let slug_b = unique_slug("m2-15-link-task-ambiguous-b");
    let saved_b = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": slug_b,
            "title": "Ambiguous spec B",
            "body": "# Spec B\n\n### REQ-700 Second owner\n\nBody.\n",
            "layer": "requirement",
        }),
    );
    let doc_id_b = saved_b["doc_id"].as_str().expect("doc_id").to_string();
    assert_ne!(doc_id_a, doc_id_b, "fixture setup: two distinct documents");

    let created = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "title": "Implement REQ-700 (doc B)" },
        }),
    );
    assert!(!created.0, "{}", created.1);
    let task_id = created_task_id(&created.1);

    let link_resp = server.call(
        "handoff_doc_verify",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc_id": &doc_id_b,
            "action": "link_task",
            "sub_item_id": "REQ-700",
            "task_ids": [&task_id],
        }),
    );
    let warnings = link_resp["warnings"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default();
    assert!(
        warnings
            .iter()
            .all(|w| !w.as_str().unwrap_or("").contains("ambiguous")),
        "linking via a specific doc_id must not hit the whole-corpus ambiguity guard: {warnings:?}"
    );

    // Doc B's REQ-700 gained the link ...
    let status_b = server.call(
        "handoff_doc_verify_status",
        json!({ "project_dir": dir.to_string_lossy(), "doc_id": &doc_id_b, "include_items": true }),
    );
    let sub_b = status_b["items"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|i| i["sub_items"].as_array().unwrap())
        .find(|s| s["stable_id"] == "REQ-700")
        .expect("REQ-700 sub_item on doc B");
    assert_eq!(sub_b["task_ids"].as_array().unwrap(), &vec![json!(task_id)]);

    // ... doc A's own REQ-700 did not.
    let status_a = server.call(
        "handoff_doc_verify_status",
        json!({ "project_dir": dir.to_string_lossy(), "doc_id": &doc_id_a, "include_items": true }),
    );
    let sub_a = status_a["items"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|i| i["sub_items"].as_array().unwrap())
        .find(|s| s["stable_id"] == "REQ-700")
        .expect("REQ-700 sub_item on doc A");
    assert_eq!(
        sub_a["task_ids"].as_array().unwrap(),
        &Vec::<Value>::new(),
        "doc A's own REQ-700 must stay unlinked: {sub_a}"
    );

    // The task's own reverse link points at doc B, not doc A.
    let task_resp = server.call(
        "handoff_get_task",
        json!({ "project_dir": dir.to_string_lossy(), "task_id": &task_id }),
    );
    let links = task_resp["task_links"]
        .as_array()
        .or_else(|| task_resp["task"]["task_links"].as_array())
        .expect("task_links present");
    let link = links
        .iter()
        .find(|l| l["link_type"] == "requirement" && l["label"] == "REQ-700")
        .unwrap_or_else(|| panic!("expected a REQ-700 reverse link, got {links:?}"));
    assert_eq!(link["target"], doc_id_b);
}

/// t360.20.34 rework (review round 2 BLOCKER): the old-link basis
/// `link_task` diffs against must be scoped to its own `doc_id`, not just
/// the bare `label` (stable_id) — otherwise linking a duplicate `stable_id`
/// in a *second* document silently drops an unrelated task's link to the
/// *first* document's SubItem with the same stable_id string, with no
/// warning at all.
#[test]
fn link_task_does_not_disturb_an_unrelated_documents_link_to_the_same_stable_id() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "m2-15-e2e-cross-doc-safety" }),
    );

    let slug_a = unique_slug("m2-15-link-task-cross-doc-a");
    let saved_a = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": slug_a,
            "title": "Cross-doc safety spec A",
            "body": "# Spec A\n\n### REQ-700 First owner\n\nBody.\n",
            "layer": "requirement",
        }),
    );
    let doc_id_a = saved_a["doc_id"].as_str().expect("doc_id").to_string();

    let slug_b = unique_slug("m2-15-link-task-cross-doc-b");
    let saved_b = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": slug_b,
            "title": "Cross-doc safety spec B",
            "body": "# Spec B\n\n### REQ-700 Second owner\n\nBody.\n",
            "layer": "requirement",
        }),
    );
    let doc_id_b = saved_b["doc_id"].as_str().expect("doc_id").to_string();
    assert_ne!(doc_id_a, doc_id_b, "fixture setup: two distinct documents");

    let created_t1 = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "title": "Implements REQ-700 (doc A)" },
        }),
    );
    assert!(!created_t1.0, "{}", created_t1.1);
    let t1 = created_task_id(&created_t1.1);

    // t1 is linked to doc A's REQ-700 first.
    server.call(
        "handoff_doc_verify",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc_id": &doc_id_a,
            "action": "link_task",
            "sub_item_id": "REQ-700",
            "task_ids": [&t1],
        }),
    );

    let created_t2 = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "title": "Implements REQ-700 (doc B)" },
        }),
    );
    assert!(!created_t2.0, "{}", created_t2.1);
    let t2 = created_task_id(&created_t2.1);

    // Linking doc B's own (duplicate-stable_id) REQ-700 to a different task
    // must not touch doc A's own link at all.
    server.call(
        "handoff_doc_verify",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc_id": &doc_id_b,
            "action": "link_task",
            "sub_item_id": "REQ-700",
            "task_ids": [&t2],
        }),
    );

    // Doc A's REQ-700 still lists t1 ...
    let status_a = server.call(
        "handoff_doc_verify_status",
        json!({ "project_dir": dir.to_string_lossy(), "doc_id": &doc_id_a, "include_items": true }),
    );
    let sub_a = status_a["items"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|i| i["sub_items"].as_array().unwrap())
        .find(|s| s["stable_id"] == "REQ-700")
        .expect("REQ-700 sub_item on doc A");
    assert_eq!(
        sub_a["task_ids"].as_array().unwrap(),
        &vec![json!(t1)],
        "doc A's own REQ-700 link to t1 must survive doc B's own link_task call: {sub_a}"
    );

    // ... and t1's own reverse link to doc A must still be present.
    let t1_resp = server.call(
        "handoff_get_task",
        json!({ "project_dir": dir.to_string_lossy(), "task_id": &t1 }),
    );
    let t1_links = t1_resp["task_links"]
        .as_array()
        .or_else(|| t1_resp["task"]["task_links"].as_array())
        .expect("task_links present");
    let t1_link = t1_links
        .iter()
        .find(|l| l["link_type"] == "requirement" && l["label"] == "REQ-700")
        .unwrap_or_else(|| {
            panic!(
                "t1's own REQ-700 reverse link must survive doc B's link_task call: {t1_links:?}"
            )
        });
    assert_eq!(t1_link["target"], doc_id_a);
}

/// t360.20.34 rework (review round 2 BLOCKER): a `SubItem.task_ids` entry
/// pointing at a task that has since been deleted (dangling on the doc side,
/// absent on the task side simply because the task no longer exists at all)
/// must still be clearable via `link_task(task_ids=[])` — the task-side scan
/// alone can never discover it (the task file is gone), so the old-link
/// basis must still fall back to the doc's own `task_ids` for ids the
/// task-side scan doesn't account for.
#[test]
fn link_task_clears_a_dangling_sub_item_task_id_after_its_task_is_deleted() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let handoff = dir.join(".handoff");

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "m2-15-e2e-dangling-deleted-task" }),
    );

    let slug = unique_slug("m2-15-link-task-dangling-deleted-task");
    let saved = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": slug,
            "title": "M2-15 link_task dangling deleted task spec",
            "body": "# Spec\n\n### REQ-001 First requirement\n\nBody one.\n",
            "layer": "requirement",
        }),
    );
    let doc_id = saved["doc_id"].as_str().expect("doc_id").to_string();

    let created = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "title": "Task to be deleted" },
        }),
    );
    assert!(!created.0, "{}", created.1);
    let task_id = created_task_id(&created.1);

    server.call(
        "handoff_doc_verify",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc_id": &doc_id,
            "action": "link_task",
            "sub_item_id": "REQ-001",
            "task_ids": [&task_id],
        }),
    );

    // Delete the task entirely (simulating a task deletion that never ran
    // its own reverse-link cleanup) — the task-side scan can no longer find
    // it at all, but the SubItem's own `task_ids` still names it.
    let task_dir = find_task_dir(&handoff, &task_id);
    std::fs::remove_dir_all(&task_dir).expect("delete task dir");

    let status_before = server.call(
        "handoff_doc_verify_status",
        json!({ "project_dir": dir.to_string_lossy(), "doc_id": &doc_id, "include_items": true }),
    );
    let sub_before = status_before["items"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|i| i["sub_items"].as_array().unwrap())
        .find(|s| s["stable_id"] == "REQ-001")
        .expect("REQ-001 sub_item");
    assert_eq!(
        sub_before["task_ids"].as_array().unwrap(),
        &vec![json!(&task_id)],
        "fixture setup: SubItem.task_ids must still dangle on the deleted task"
    );

    server.call(
        "handoff_doc_verify",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc_id": &doc_id,
            "action": "link_task",
            "sub_item_id": "REQ-001",
            "task_ids": [],
        }),
    );

    let status_after = server.call(
        "handoff_doc_verify_status",
        json!({ "project_dir": dir.to_string_lossy(), "doc_id": &doc_id, "include_items": true }),
    );
    let sub_after = status_after["items"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|i| i["sub_items"].as_array().unwrap())
        .find(|s| s["stable_id"] == "REQ-001")
        .expect("REQ-001 sub_item");
    assert_eq!(
        sub_after["task_ids"].as_array().unwrap(),
        &Vec::<Value>::new(),
        "link_task(task_ids=[]) must clear a dangling id even after its task was deleted: {sub_after}"
    );
}
