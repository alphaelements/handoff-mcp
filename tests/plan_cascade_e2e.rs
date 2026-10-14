//! Real-binary E2E for t-pla.3 (REQ-PLA-004 / REQ-PLA-005): spawns the actual
//! `handoff-mcp` binary over stdio JSON-RPC and checks the notification-only
//! cascade fields — `doc_save`'s `scaffold_suggestions` / `schedule_stale`,
//! `handoff_update_task`'s `schedule_stale` line, and `handoff_auto_schedule`'s
//! explicit `schedule_stale: false`.

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

fn create_task(server: &mut Server, dir: &Path, title: &str) -> String {
    let created = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": pd(dir),
            "task": { "title": title, "schedule": { "estimate_hours": 2.0 } },
        }),
    );
    created
        .trim_start_matches("Created task ")
        .split(':')
        .next()
        .unwrap_or_default()
        .to_string()
}

fn save_plan(server: &mut Server, dir: &Path, slug: &str, body: &str, extra: Value) -> Value {
    let mut args = json!({
        "project_dir": pd(dir),
        "slug": slug,
        "title": "Plan",
        "doc_type": "plan",
        "layer": "basic_spec",
        "body": body,
    });
    for (k, v) in extra.as_object().expect("extra object") {
        args[k] = v.clone();
    }
    server.call("handoff_doc_save", args)
}

fn suggested_ids(resp: &Value) -> Vec<String> {
    resp["scaffold_suggestions"]
        .as_array()
        .unwrap_or_else(|| panic!("scaffold_suggestions must be an array: {resp}"))
        .iter()
        .map(|s| s["stable_id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn plan_doc_save_suggests_only_unlinked_requirements() {
    let (_tmp, dir, mut server) = project();
    let body = "# Plan\n\n### REQ-401 First\n\nA.\n\n### REQ-402 Second\n\nB.\n";
    let first = save_plan(&mut server, &dir, "rollout-plan", body, json!({}));
    assert_eq!(suggested_ids(&first), ["REQ-401", "REQ-402"], "{first}");
    assert_eq!(first["scaffold_suggestions"][0]["title"], "First");

    // Link REQ-401 to a task; a later save of the plan only suggests REQ-402.
    let t1 = create_task(&mut server, &dir, "Implement REQ-401");
    server.call_raw(
        "handoff_update_task",
        json!({ "project_dir": pd(&dir), "task": { "id": t1, "requirement_ids": ["REQ-401"] } }),
    );
    let body2 = format!("{body}\n### REQ-403 Third\n\nC.\n");
    let second = save_plan(
        &mut server,
        &dir,
        "rollout-plan",
        &body2,
        json!({ "doc_id": first["doc_id"] }),
    );
    assert_eq!(suggested_ids(&second), ["REQ-402", "REQ-403"], "{second}");
}

#[test]
fn non_plan_doc_save_has_empty_scaffold_suggestions() {
    let (_tmp, dir, mut server) = project();
    let resp = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd(&dir),
            "slug": "login-spec",
            "title": "Login",
            "doc_type": "spec",
            "layer": "basic_spec",
            "body": "# Spec\n\n### REQ-501 Login\n\nA.\n",
        }),
    );
    assert_eq!(resp["doc_type"], "spec");
    assert_eq!(resp["scaffold_suggestions"], json!([]), "{resp}");
    assert_eq!(resp["schedule_stale"], false, "{resp}");
}

#[test]
fn doc_save_task_ids_change_sets_schedule_stale() {
    let (_tmp, dir, mut server) = project();
    let t1 = create_task(&mut server, &dir, "One");
    let t2 = create_task(&mut server, &dir, "Two");
    let body = "# Plan\n\nScope.\n";

    let created = save_plan(&mut server, &dir, "p", body, json!({ "task_ids": [t1] }));
    assert_eq!(created["schedule_stale"], true, "new link: {created}");
    let doc_id = created["doc_id"].clone();

    let same = save_plan(
        &mut server,
        &dir,
        "p",
        body,
        json!({ "doc_id": doc_id, "task_ids": [t1] }),
    );
    assert_eq!(same["schedule_stale"], false, "same ids: {same}");

    let omitted = save_plan(&mut server, &dir, "p", body, json!({ "doc_id": doc_id }));
    assert_eq!(
        omitted["schedule_stale"], false,
        "no task_ids arg: {omitted}"
    );

    let added = save_plan(
        &mut server,
        &dir,
        "p",
        body,
        json!({ "doc_id": doc_id, "task_ids": [t1, t2] }),
    );
    assert_eq!(added["schedule_stale"], true, "added: {added}");

    let removed = save_plan(
        &mut server,
        &dir,
        "p",
        body,
        json!({ "doc_id": doc_id, "task_ids": [t1] }),
    );
    assert_eq!(removed["schedule_stale"], true, "removed: {removed}");

    // An unresolvable id is never linked, so it does not make the schedule stale.
    let unresolved = save_plan(
        &mut server,
        &dir,
        "p",
        body,
        json!({ "doc_id": doc_id, "task_ids": [t1, "t-nope"] }),
    );
    assert_eq!(
        unresolved["schedule_stale"], false,
        "unresolved: {unresolved}"
    );
}

fn update(server: &mut Server, dir: &Path, task: Value) -> String {
    server.call_raw(
        "handoff_update_task",
        json!({ "project_dir": pd(dir), "task": task }),
    )
}

#[test]
fn update_task_reports_schedule_stale_for_schedule_affecting_changes() {
    let (_tmp, dir, mut server) = project();
    let t1 = create_task(&mut server, &dir, "One");
    let t2 = create_task(&mut server, &dir, "Two");
    let stale = "schedule_stale: true";

    let r = update(&mut server, &dir, json!({ "id": t2, "dependencies": [t1] }));
    assert!(r.contains(stale), "dependency change: {r}");
    let r = update(&mut server, &dir, json!({ "id": t2, "dependencies": [t1] }));
    assert!(!r.contains(stale), "same dependencies: {r}");

    let r = update(
        &mut server,
        &dir,
        json!({ "id": t2, "schedule": { "estimate_hours": 5.0 } }),
    );
    assert!(r.contains(stale), "estimate change: {r}");
    let r = update(
        &mut server,
        &dir,
        json!({ "id": t2, "schedule": { "estimate_hours": 5.0 } }),
    );
    assert!(!r.contains(stale), "same estimate: {r}");
    let r = update(
        &mut server,
        &dir,
        json!({ "id": t2, "schedule": { "start_date": "2026-03-02" } }),
    );
    assert!(r.contains(stale), "start_date change: {r}");
    let r = update(
        &mut server,
        &dir,
        json!({ "id": t2, "schedule": { "due_date": "2026-03-06" } }),
    );
    assert!(r.contains(stale), "due_date change: {r}");

    let r = update(
        &mut server,
        &dir,
        json!({ "id": t2, "notes": "just a note" }),
    );
    assert!(!r.contains(stale), "notes only: {r}");

    let r = update(
        &mut server,
        &dir,
        json!({ "id": t1, "status": "in_progress" }),
    );
    assert!(r.contains(stale), "status change: {r}");
}

#[test]
fn auto_schedule_response_states_schedule_stale_false() {
    let (_tmp, dir, mut server) = project();
    create_task(&mut server, &dir, "One");
    let resp = server.call(
        "handoff_auto_schedule",
        json!({ "project_dir": pd(&dir), "dry_run": true }),
    );
    assert_eq!(resp["schedule_stale"], false, "{resp}");
}
