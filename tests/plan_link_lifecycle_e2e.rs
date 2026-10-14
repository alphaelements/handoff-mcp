//! Real-binary E2E for t-pla.4 (REQ-PLA-006 / REQ-PLA-007): spawns the actual
//! `handoff-mcp` binary over stdio JSON-RPC and covers
//! - `handoff_trace_suspect(action="clear")` returning `baseline_suggestions`
//!   for cleared task suspects (suggest-only: never writes a baseline), and
//! - `handoff_doc_save` removing a deleted requirement's task-side links and
//!   reporting `orphaned_tasks` (task status never changes).

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

    fn call(&mut self, name: &str, arguments: Value) -> Value {
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
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string()))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    server: Server,
}

impl Fixture {
    fn new(project: &str) -> Self {
        let tmp = tempfile::tempdir().expect("temp dir");
        let dir = tmp.path().join("proj");
        std::fs::create_dir_all(&dir).unwrap();
        let mut server = Server::spawn();
        server.call(
            "handoff_init",
            json!({ "project_dir": dir.to_string_lossy(), "project_name": project }),
        );
        Fixture {
            _tmp: tmp,
            dir,
            server,
        }
    }

    fn pd(&self) -> String {
        self.dir.to_string_lossy().to_string()
    }

    fn save_doc(&mut self, slug: &str, doc_id: Option<&str>, body: &str) -> Value {
        let mut args = json!({
            "project_dir": self.pd(),
            "slug": slug,
            "title": slug,
            "body": body,
        });
        match doc_id {
            Some(id) => args["doc_id"] = json!(id),
            None => args["layer"] = json!("requirement"),
        }
        self.server.call("handoff_doc_save", args)
    }

    fn update_task(&mut self, task: Value) {
        self.server.call(
            "handoff_update_task",
            json!({ "project_dir": self.pd(), "task": task }),
        );
    }

    fn get_task(&mut self, id: &str) -> Value {
        self.server.call(
            "handoff_get_task",
            json!({ "project_dir": self.pd(), "task_id": id }),
        )
    }
}

fn requirement_labels(task: &Value) -> Vec<String> {
    task["task_links"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|l| l["link_type"] == "requirement")
                .filter_map(|l| l["label"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// REQ-PLA-006: baseline_suggestions on trace_suspect(action="clear")
// ---------------------------------------------------------------------------

const REQ_V1: &str = "# Requirements\n\n### REQ-100 Lockout\n\n- priority: P1\n\nOriginal text.\n";
const REQ_V2: &str =
    "# Requirements\n\n### REQ-100 Lockout\n\n- priority: P1\n\nChanged text (round 2).\n";

/// Builds a project where `t1` (with the given `schedule`) is linked to
/// REQ-100, then edits REQ-100 so `t1` becomes a task suspect, and clears it.
/// Returns the fixture and the `clear` response.
fn clear_task_suspect_with_schedule(project: &str, schedule: Value) -> (Fixture, Value) {
    let mut f = Fixture::new(project);
    let saved = f.save_doc("reqs", None, REQ_V1);
    let doc_id = saved["doc_id"].as_str().unwrap().to_string();
    f.update_task(json!({
        "id": "t1",
        "title": "Implement lockout",
        "requirement_ids": ["REQ-100"],
        "schedule": schedule,
    }));
    f.save_doc("reqs", Some(&doc_id), REQ_V2);

    let list = f.server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": f.pd(), "action": "list", "kinds": ["task"] }),
    );
    assert_eq!(list["suspects"].as_array().unwrap().len(), 1, "{list}");

    let clear = f.server.call(
        "handoff_trace_suspect",
        json!({
            "project_dir": f.pd(),
            "action": "clear",
            "targets": [{"task_id": "t1"}],
            "reason": "reviewed, text-only change",
        }),
    );
    (f, clear)
}

#[test]
fn clear_suggests_setting_baseline_when_task_has_none() {
    let (mut f, clear) = clear_task_suspect_with_schedule(
        "pla4-set",
        json!({ "start_date": "2026-03-02", "due_date": "2026-03-06" }),
    );
    assert_eq!(clear["cleared"]["tasks"], 1, "{clear}");
    let sugg = clear["baseline_suggestions"].as_array().expect("{clear}");
    assert_eq!(sugg.len(), 1, "{clear}");
    assert_eq!(sugg[0]["task_id"], "t1");
    assert_eq!(sugg[0]["action"], "set_recommended");
    assert_eq!(sugg[0]["current_start"], "2026-03-02");
    assert_eq!(sugg[0]["current_due"], "2026-03-06");

    // Suggest-only: nothing was written to the task's baseline.
    let task = f.get_task("t1");
    assert!(
        task["schedule"]["baseline_start"].is_null(),
        "baseline must not be written: {task}"
    );
}

#[test]
fn clear_suggests_update_when_baseline_differs_from_current_schedule() {
    let (mut f, clear) = clear_task_suspect_with_schedule(
        "pla4-update",
        json!({
            "start_date": "2026-03-09", "due_date": "2026-03-13",
            "baseline_start": "2026-03-02", "baseline_due": "2026-03-06",
        }),
    );
    let sugg = clear["baseline_suggestions"].as_array().expect("{clear}");
    assert_eq!(sugg.len(), 1, "{clear}");
    assert_eq!(sugg[0]["action"], "update_recommended");
    assert_eq!(sugg[0]["current_start"], "2026-03-09");
    assert_eq!(sugg[0]["current_due"], "2026-03-13");
    assert_eq!(sugg[0]["baseline_start"], "2026-03-02");
    assert_eq!(sugg[0]["baseline_due"], "2026-03-06");

    let task = f.get_task("t1");
    assert_eq!(task["schedule"]["baseline_start"], "2026-03-02", "{task}");
    assert_eq!(task["schedule"]["baseline_due"], "2026-03-06", "{task}");
}

#[test]
fn clear_does_not_suggest_when_baseline_already_matches_current_schedule() {
    let (_f, clear) = clear_task_suspect_with_schedule(
        "pla4-same",
        json!({
            "start_date": "2026-03-02", "due_date": "2026-03-06",
            "baseline_start": "2026-03-02", "baseline_due": "2026-03-06",
        }),
    );
    assert_eq!(clear["baseline_suggestions"], json!([]), "{clear}");
}

#[test]
fn clear_warns_without_overwriting_a_manually_set_pinned_baseline() {
    let (mut f, clear) = clear_task_suspect_with_schedule(
        "pla4-manual",
        json!({
            "pinned": true,
            "start_date": "2026-03-09", "due_date": "2026-03-13",
            "baseline_start": "2026-03-02", "baseline_due": "2026-03-06",
        }),
    );
    let sugg = clear["baseline_suggestions"].as_array().expect("{clear}");
    assert_eq!(sugg.len(), 1, "{clear}");
    assert_eq!(sugg[0]["action"], "manual_baseline_detected");
    assert_eq!(
        sugg[0]["warning"],
        "Baseline was manually set; not overwriting"
    );
    let task = f.get_task("t1");
    assert_eq!(task["schedule"]["baseline_start"], "2026-03-02", "{task}");
    assert_eq!(task["schedule"]["baseline_due"], "2026-03-06", "{task}");
}

#[test]
fn clear_without_task_suspects_returns_empty_baseline_suggestions() {
    let mut f = Fixture::new("pla4-none");
    f.save_doc("reqs", None, REQ_V1);
    let clear = f.server.call(
        "handoff_trace_suspect",
        json!({
            "project_dir": f.pd(),
            "action": "clear",
            "targets": [{"task_id": "t-none"}],
            "reason": "nothing to clear",
        }),
    );
    assert_eq!(clear["baseline_suggestions"], json!([]), "{clear}");
}

// ---------------------------------------------------------------------------
// REQ-PLA-007: requirement deletion unlinks tasks + orphaned_tasks
// ---------------------------------------------------------------------------

const BODY_TWO: &str = "# Spec\n\n### REQ-301 First\n\nBody.\n\n### REQ-302 Second\n\nBody.\n";
const BODY_ONE: &str = "# Spec\n\n### REQ-301 First\n\nBody.\n";

fn set_status(f: &mut Fixture, id: &str, status: &str) {
    f.update_task(json!({ "id": id, "status": status, "schedule": { "estimate_hours": 1.0 } }));
}

#[test]
fn deleting_a_requirement_unlinks_tasks_and_reports_orphans_without_touching_status() {
    let mut f = Fixture::new("pla4-orphan");
    let saved = f.save_doc("spec", None, BODY_TWO);
    let doc_id = saved["doc_id"].as_str().unwrap().to_string();

    // t-orphan: only REQ-302 -> orphaned. t-keep: REQ-302 + REQ-301 -> not.
    f.update_task(
        json!({ "id": "t-orphan", "title": "Only second", "requirement_ids": ["REQ-302"] }),
    );
    f.update_task(
        json!({ "id": "t-keep", "title": "Both", "requirement_ids": ["REQ-301", "REQ-302"] }),
    );
    set_status(&mut f, "t-orphan", "in_progress");
    set_status(&mut f, "t-keep", "in_progress");

    let out = f.save_doc("spec", Some(&doc_id), BODY_ONE);

    let orphaned = out["orphaned_tasks"].as_array().expect("{out}");
    assert_eq!(orphaned.len(), 1, "{out}");
    assert_eq!(orphaned[0]["task_id"], "t-orphan");
    assert_eq!(orphaned[0]["title"], "Only second");

    let orphan = f.get_task("t-orphan");
    assert!(requirement_labels(&orphan).is_empty(), "{orphan}");
    assert_eq!(orphan["status"], "in_progress", "status must not change");

    let keep = f.get_task("t-keep");
    assert_eq!(
        requirement_labels(&keep),
        vec!["REQ-301".to_string()],
        "{keep}"
    );
    assert_eq!(keep["status"], "in_progress");
}

#[test]
fn deleting_a_requirement_does_not_change_status_of_a_done_task() {
    let mut f = Fixture::new("pla4-done");
    let saved = f.save_doc("spec", None, BODY_TWO);
    let doc_id = saved["doc_id"].as_str().unwrap().to_string();
    f.update_task(json!({ "id": "t1", "title": "Done one", "requirement_ids": ["REQ-302"] }));
    set_status(&mut f, "t1", "done");

    let out = f.save_doc("spec", Some(&doc_id), BODY_ONE);
    assert_eq!(out["orphaned_tasks"][0]["task_id"], "t1", "{out}");
    assert_eq!(f.get_task("t1")["status"], "done");
}

#[test]
fn save_without_removed_requirements_has_no_orphaned_tasks() {
    let mut f = Fixture::new("pla4-noremoval");
    let saved = f.save_doc("spec", None, BODY_TWO);
    let doc_id = saved["doc_id"].as_str().unwrap().to_string();
    f.update_task(json!({ "id": "t1", "title": "T", "requirement_ids": ["REQ-302"] }));

    let out = f.save_doc("spec", Some(&doc_id), &BODY_TWO.replace("Body.", "Edited."));
    assert!(out.get("orphaned_tasks").is_none(), "{out}");
    assert_eq!(
        requirement_labels(&f.get_task("t1")),
        vec!["REQ-302".to_string()]
    );
}

#[test]
fn moving_a_requirement_to_another_document_keeps_the_task_link() {
    let mut f = Fixture::new("pla4-move");
    let a = f.save_doc("spec-a", None, BODY_TWO);
    let a_id = a["doc_id"].as_str().unwrap().to_string();
    f.update_task(json!({ "id": "t1", "title": "T", "requirement_ids": ["REQ-302"] }));
    // The requirement's new home exists before the old copy is removed.
    f.save_doc("spec-b", None, "# B\n\n### REQ-302 Second\n\nMoved.\n");

    let out = f.save_doc("spec-a", Some(&a_id), BODY_ONE);
    assert!(out.get("orphaned_tasks").is_none(), "{out}");
    assert_eq!(
        requirement_labels(&f.get_task("t1")),
        vec!["REQ-302".to_string()]
    );
}
