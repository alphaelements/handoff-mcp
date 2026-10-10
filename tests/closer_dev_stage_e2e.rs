//! Real-binary E2E for t390.3 (dev_stage auto-promotion in the session-closer
//! flow): spawns the actual `handoff-mcp` binary over stdio JSON-RPC.
//!
//! The session-closer agent (`plugin-task-loop/agents/session-closer.md`) links
//! a finished task's requirements and marks it done in ONE `handoff_update_task`
//! call (`requirement_ids` + `status: "done"`). That single-call path must
//! promote the linked requirement's `dev_stage` to `implemented`
//! (`propagate_dev_stage_for_task`). A task completed without any requirement
//! link — the gap the closer exists to close — must leave `dev_stage` untouched.

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

fn find_sub_item_by_stable_id<'a>(status: &'a Value, stable_id: &str) -> &'a Value {
    status["items"]
        .as_array()
        .expect("items array")
        .iter()
        .flat_map(|i| i["sub_items"].as_array().expect("sub_items array"))
        .find(|s| s["stable_id"] == stable_id)
        .unwrap_or_else(|| panic!("stable_id {stable_id} not found in {status}"))
}

fn unique_slug(label: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{label}-{n}")
}

struct Fixture {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    server: Server,
    doc_id: String,
    task_id: String,
}

fn setup(project: &str) -> Fixture {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": project }),
    );
    let saved = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": unique_slug("closer-spec"),
            "title": "Closer spec",
            "body": "# Spec\n\n### REQ-201 Closer item\n\nBody.\n",
            "layer": "basic_spec",
        }),
    );
    let doc_id = saved["doc_id"].as_str().expect("doc_id").to_string();
    let created = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "title": "Implement closer item", "schedule": { "estimate_hours": 1.0 } },
        }),
    );
    let task_id = created
        .trim_start_matches("Created task ")
        .split(':')
        .next()
        .unwrap_or_default()
        .to_string();
    assert!(
        !task_id.is_empty(),
        "could not extract task id from: {created}"
    );
    Fixture {
        _tmp: tmp,
        dir,
        server,
        doc_id,
        task_id,
    }
}

fn dev_stage(f: &mut Fixture) -> Value {
    let status = f.server.call(
        "handoff_doc_verify_status",
        json!({ "project_dir": f.dir.to_string_lossy(), "doc_id": f.doc_id, "include_items": true }),
    );
    find_sub_item_by_stable_id(&status, "REQ-201")["dev_stage"].clone()
}

/// closer-style single call: requirement_ids + status=done together.
#[test]
fn closer_single_call_links_and_done_promotes_dev_stage() {
    let mut f = setup("closer-link-done");
    assert_eq!(dev_stage(&mut f), "not_started");

    f.server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": f.dir.to_string_lossy(),
            "task": {
                "id": f.task_id,
                "requirement_ids": ["REQ-201"],
                "status": "done",
            },
        }),
    );

    let task = f.server.call(
        "handoff_get_task",
        json!({ "project_dir": f.dir.to_string_lossy(), "task_id": f.task_id }),
    );
    let linked = task["task_links"]
        .as_array()
        .expect("task_links array")
        .iter()
        .any(|l| l["link_type"] == "requirement" && l["label"] == "REQ-201");
    assert!(linked, "requirement_ids must create the task link: {task}");
    assert_eq!(
        dev_stage(&mut f),
        "implemented",
        "linking and completing in one call must promote dev_stage"
    );
}

/// Negative control: completing a task with no requirement link (the gap the
/// closer closes) never promotes dev_stage.
#[test]
fn done_without_requirement_link_leaves_dev_stage_untouched() {
    let mut f = setup("closer-unlinked-done");

    f.server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": f.dir.to_string_lossy(),
            "task": { "id": f.task_id, "status": "done" },
        }),
    );

    assert_eq!(dev_stage(&mut f), "not_started");
}
