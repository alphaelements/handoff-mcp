//! Real-binary E2E for t-reqflow.2.8 / t-reqflow.2.9 (SPEC-SI-008 / SPEC-SI-009):
//! spawns the actual `handoff-mcp` binary over stdio JSON-RPC and checks
//! `handoff_update_task`'s `plan_update_needed` line (a task moving to `done`
//! that is linked to a plan `SubItem`) and `handoff_auto_schedule`'s use of a
//! plan `SubItem`'s `order` as the linked task's execution order.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
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

    /// Returns the tool's raw response text, unparsed — some tools
    /// (`handoff_update_task`'s create path) return a plain confirmation
    /// string, not JSON.
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

fn project() -> (tempfile::TempDir, PathBuf, Server) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "plan-baseline" }),
    );
    (tmp, dir, server)
}

fn pd(dir: &Path) -> String {
    dir.to_string_lossy().to_string()
}

/// Each task fills a whole default work day, so auto-schedule gives every task
/// its own date and the schedule order is visible in the start dates (smaller
/// tasks would be packed into a shared day).
fn create_task(server: &mut Server, dir: &Path, title: &str) -> String {
    let created = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": pd(dir),
            "task": { "title": title, "schedule": { "estimate_hours": 8.0 } },
        }),
    );
    created
        .trim_start_matches("Created task ")
        .split(':')
        .next()
        .unwrap_or_default()
        .to_string()
}

fn save_doc(server: &mut Server, dir: &Path, slug: &str, doc_type: &str, body: &str) -> Value {
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd(dir),
            "slug": slug,
            "title": slug,
            "doc_type": doc_type,
            "layer": "basic_spec",
            "body": body,
        }),
    )
}

fn update(server: &mut Server, dir: &Path, task: Value) -> String {
    server.call_raw(
        "handoff_update_task",
        json!({ "project_dir": pd(dir), "task": task }),
    )
}

fn link(server: &mut Server, dir: &Path, task_id: &str, stable_id: &str) {
    update(
        server,
        dir,
        json!({ "id": task_id, "requirement_ids": [stable_id] }),
    );
}

/// The JSON array after the `plan_update_needed:` line, if the response has one.
fn plan_update_needed(resp: &str) -> Option<Value> {
    resp.lines()
        .find_map(|l| l.strip_prefix("plan_update_needed: "))
        .map(|json| serde_json::from_str(json).expect("plan_update_needed is JSON"))
}

#[test]
fn done_transition_on_plan_linked_task_reports_plan_update_needed() {
    let (_tmp, dir, mut server) = project();
    let plan = save_doc(
        &mut server,
        &dir,
        "rollout-plan",
        "plan",
        "# Plan\n\n### FR-PLAN-001 First\n\nA.\n",
    );
    let t1 = create_task(&mut server, &dir, "Implement FR-PLAN-001");
    link(&mut server, &dir, &t1, "FR-PLAN-001");

    let resp = update(&mut server, &dir, json!({ "id": t1, "status": "done" }));
    let needed =
        plan_update_needed(&resp).unwrap_or_else(|| panic!("no plan_update_needed: {resp}"));
    let entries = needed.as_array().expect("array");
    assert_eq!(entries.len(), 1, "{resp}");
    assert_eq!(entries[0]["doc_id"], plan["doc_id"], "{resp}");
    assert_eq!(entries[0]["stable_id"], "FR-PLAN-001");
    assert_eq!(entries[0]["suggested_dev_stage"], "implemented");
    assert!(entries[0]["current_dev_stage"].is_string(), "{resp}");
}

#[test]
fn done_transition_without_plan_link_has_no_plan_update_needed() {
    let (_tmp, dir, mut server) = project();
    save_doc(
        &mut server,
        &dir,
        "login-spec",
        "spec",
        "# Spec\n\n### REQ-501 Login\n\nA.\n",
    );
    let unlinked = create_task(&mut server, &dir, "No links");
    let spec_linked = create_task(&mut server, &dir, "Spec only");
    link(&mut server, &dir, &spec_linked, "REQ-501");

    let resp = update(
        &mut server,
        &dir,
        json!({ "id": unlinked, "status": "done" }),
    );
    assert!(!resp.contains("plan_update_needed"), "no links: {resp}");
    let resp = update(
        &mut server,
        &dir,
        json!({ "id": spec_linked, "status": "done" }),
    );
    assert!(!resp.contains("plan_update_needed"), "spec link: {resp}");
}

#[test]
fn non_done_status_change_has_no_plan_update_needed() {
    let (_tmp, dir, mut server) = project();
    save_doc(
        &mut server,
        &dir,
        "rollout-plan",
        "plan",
        "# Plan\n\n### FR-PLAN-001 First\n\nA.\n",
    );
    let t1 = create_task(&mut server, &dir, "Implement FR-PLAN-001");
    link(&mut server, &dir, &t1, "FR-PLAN-001");

    let resp = update(
        &mut server,
        &dir,
        json!({ "id": t1, "status": "in_progress" }),
    );
    assert!(!resp.contains("plan_update_needed"), "{resp}");
}

#[test]
fn resaving_an_already_done_task_has_no_plan_update_needed() {
    let (_tmp, dir, mut server) = project();
    save_doc(
        &mut server,
        &dir,
        "rollout-plan",
        "plan",
        "# Plan\n\n### FR-PLAN-001 First\n\nA.\n",
    );
    let t1 = create_task(&mut server, &dir, "Implement FR-PLAN-001");
    link(&mut server, &dir, &t1, "FR-PLAN-001");

    let first = update(&mut server, &dir, json!({ "id": t1, "status": "done" }));
    assert!(plan_update_needed(&first).is_some(), "first done: {first}");

    // done -> done (status re-sent): the status did not change.
    let resend = update(&mut server, &dir, json!({ "id": t1, "status": "done" }));
    assert!(!resend.contains("plan_update_needed"), "resend: {resend}");

    // Another field changed, no status in the request.
    let edit = update(&mut server, &dir, json!({ "id": t1, "title": "Renamed" }));
    assert!(!edit.contains("plan_update_needed"), "edit: {edit}");
}

const SCHEDULE_ANCHOR: &str = "2026-03-02";

/// `task_id -> new_start` from a dry-run `handoff_auto_schedule`.
fn scheduled_starts(server: &mut Server, dir: &Path) -> Vec<(String, String)> {
    let resp = server.call(
        "handoff_auto_schedule",
        json!({ "project_dir": pd(dir), "dry_run": true, "start_date": SCHEDULE_ANCHOR }),
    );
    let mut starts: Vec<(String, String)> = resp["changes"]
        .as_array()
        .unwrap_or_else(|| panic!("changes array: {resp}"))
        .iter()
        .map(|c| {
            (
                c["task_id"].as_str().unwrap().to_string(),
                c["new_start"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    starts.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
    starts
}

fn schedule_order(starts: &[(String, String)]) -> Vec<&str> {
    starts.iter().map(|(id, _)| id.as_str()).collect()
}

const ORDERED_PLAN: &str = "# Plan\n\n\
### FR-PLAN-001 A\n\n- order: 3\n\nA.\n\n\
### FR-PLAN-002 B\n\n- order: 1\n\nB.\n\n\
### FR-PLAN-003 C\n\n- order: 2\n\nC.\n";

#[test]
fn auto_schedule_follows_plan_sub_item_order() {
    let (_tmp, dir, mut server) = project();
    save_doc(&mut server, &dir, "ordered-plan", "plan", ORDERED_PLAN);
    let t1 = create_task(&mut server, &dir, "A");
    let t2 = create_task(&mut server, &dir, "B");
    let t3 = create_task(&mut server, &dir, "C");
    link(&mut server, &dir, &t1, "FR-PLAN-001");
    link(&mut server, &dir, &t2, "FR-PLAN-002");
    link(&mut server, &dir, &t3, "FR-PLAN-003");

    let starts = scheduled_starts(&mut server, &dir);
    assert_eq!(
        schedule_order(&starts),
        [t2.as_str(), t3.as_str(), t1.as_str()],
        "{starts:?}"
    );
}

#[test]
fn auto_schedule_ignores_order_of_non_plan_documents() {
    let (_tmp, dir, mut server) = project();
    save_doc(&mut server, &dir, "ordered-spec", "spec", ORDERED_PLAN);
    let t1 = create_task(&mut server, &dir, "A");
    let t2 = create_task(&mut server, &dir, "B");
    link(&mut server, &dir, &t1, "FR-PLAN-001");
    link(&mut server, &dir, &t2, "FR-PLAN-002");

    let starts = scheduled_starts(&mut server, &dir);
    assert_eq!(
        schedule_order(&starts),
        [t1.as_str(), t2.as_str()],
        "{starts:?}"
    );
}

#[test]
fn auto_schedule_task_own_order_wins_over_plan_order() {
    let (_tmp, dir, mut server) = project();
    save_doc(&mut server, &dir, "ordered-plan", "plan", ORDERED_PLAN);
    let t1 = create_task(&mut server, &dir, "A");
    let t2 = create_task(&mut server, &dir, "B");
    link(&mut server, &dir, &t1, "FR-PLAN-001"); // plan order 3
    link(&mut server, &dir, &t2, "FR-PLAN-002"); // plan order 1
                                                 // t1's own order (0) beats t2's plan-derived order (1).
    update(&mut server, &dir, json!({ "id": t1, "order": 0 }));

    let starts = scheduled_starts(&mut server, &dir);
    assert_eq!(
        schedule_order(&starts),
        [t1.as_str(), t2.as_str()],
        "{starts:?}"
    );
}

#[test]
fn auto_schedule_dependencies_take_priority_over_plan_order() {
    let (_tmp, dir, mut server) = project();
    save_doc(&mut server, &dir, "ordered-plan", "plan", ORDERED_PLAN);
    let t1 = create_task(&mut server, &dir, "A");
    let t2 = create_task(&mut server, &dir, "B");
    link(&mut server, &dir, &t1, "FR-PLAN-001"); // plan order 3
    link(&mut server, &dir, &t2, "FR-PLAN-002"); // plan order 1, but depends on t1
    update(&mut server, &dir, json!({ "id": t2, "dependencies": [t1] }));

    let starts = scheduled_starts(&mut server, &dir);
    assert_eq!(
        schedule_order(&starts),
        [t1.as_str(), t2.as_str()],
        "{starts:?}"
    );
}

#[test]
fn auto_schedule_survives_an_unreadable_document_and_reports_a_warning() {
    let (_tmp, dir, mut server) = project();
    save_doc(&mut server, &dir, "ordered-plan", "plan", ORDERED_PLAN);
    let t1 = create_task(&mut server, &dir, "A");
    let t2 = create_task(&mut server, &dir, "B");
    link(&mut server, &dir, &t1, "FR-PLAN-001"); // plan order 3
    link(&mut server, &dir, &t2, "FR-PLAN-002"); // plan order 1
    std::fs::write(
        dir.join(".handoff/docs/_doc.broken.md"),
        "---\nid: [unterminated\n---\nbody\n",
    )
    .unwrap();

    let resp = server.call(
        "handoff_auto_schedule",
        json!({ "project_dir": pd(&dir), "dry_run": true, "start_date": SCHEDULE_ANCHOR }),
    );
    assert_eq!(resp["scheduled_count"], 2, "{resp}");
    let warnings = resp["warnings"].as_array().expect("warnings array");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("broken")),
        "{resp}"
    );
    // The readable plan document still orders the tasks.
    let starts = scheduled_starts(&mut server, &dir);
    assert_eq!(
        schedule_order(&starts),
        [t2.as_str(), t1.as_str()],
        "{starts:?}"
    );
}
