//! Real-binary E2E for t391.3 (requirement-change reminders): spawns the
//! actual `handoff-mcp` binary over stdio JSON-RPC and checks that
//!
//! - adding a SubItem to a layer document returns `NEW_REQUIREMENT_DETECTED`
//!   (and editing an existing one does not),
//! - removing a SubItem returns `REQUIREMENT_REMOVED` naming the tasks that
//!   still link it, without deleting those task links,
//! - `load_context` carries `suspect_summary` (null with no suspects, a
//!   per-kind summary once `trace_report` has persisted a suspect).

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

fn warnings_with_code<'a>(resp: &'a Value, code: &str) -> Vec<&'a Value> {
    resp["warnings"]
        .as_array()
        .map(|a| a.iter().filter(|w| w["code"] == code).collect())
        .unwrap_or_default()
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

    fn save_doc(&mut self, doc_id: Option<&str>, body: &str) -> Value {
        let mut args = json!({
            "project_dir": self.dir.to_string_lossy(),
            "slug": "change-spec",
            "title": "Change spec",
            "body": body,
        });
        match doc_id {
            Some(id) => args["doc_id"] = json!(id),
            None => args["layer"] = json!("basic_spec"),
        }
        self.server.call("handoff_doc_save", args)
    }

    fn create_task(&mut self, title: &str) -> String {
        let created = self.server.call_raw(
            "handoff_update_task",
            json!({
                "project_dir": self.dir.to_string_lossy(),
                "task": { "title": title, "schedule": { "estimate_hours": 1.0 } },
            }),
        );
        created
            .trim_start_matches("Created task ")
            .split(':')
            .next()
            .unwrap_or_default()
            .to_string()
    }

    fn link(&mut self, task_id: &str, stable_id: &str) {
        self.server.call_raw(
            "handoff_update_task",
            json!({
                "project_dir": self.dir.to_string_lossy(),
                "task": { "id": task_id, "requirement_ids": [stable_id] },
            }),
        );
    }

    fn load_context(&mut self) -> Value {
        self.server.call(
            "handoff_load_context",
            json!({ "project_dir": self.dir.to_string_lossy() }),
        )
    }
}

const BODY_V1: &str = "# Spec\n\n### REQ-301 First\n\nBody.\n\n### REQ-302 Second\n\nBody.\n";

#[test]
fn adding_a_sub_item_warns_new_requirement_detected() {
    let mut f = Fixture::new("req-change-add");
    let first = f.save_doc(None, BODY_V1);
    let doc_id = first["doc_id"].as_str().unwrap().to_string();

    // Existing items edited, none added: silent.
    let edited = f.save_doc(
        Some(&doc_id),
        &BODY_V1.replace("Body.\n\n### REQ-302", "Changed.\n\n### REQ-302"),
    );
    assert!(warnings_with_code(&edited, "NEW_REQUIREMENT_DETECTED").is_empty());

    let added = f.save_doc(
        Some(&doc_id),
        &format!("{BODY_V1}\n### REQ-303 Third\n\nBody.\n"),
    );
    let hits = warnings_with_code(&added, "NEW_REQUIREMENT_DETECTED");
    assert_eq!(hits.len(), 1, "{added}");
    assert!(hits[0]["message"]
        .as_str()
        .unwrap()
        .contains("New requirement REQ-303 detected"));
}

#[test]
fn removing_a_sub_item_warns_with_affected_tasks_and_keeps_links() {
    let mut f = Fixture::new("req-change-remove");
    let first = f.save_doc(None, BODY_V1);
    let doc_id = first["doc_id"].as_str().unwrap().to_string();
    let t_linked = f.create_task("Linked to REQ-302");
    let t_other = f.create_task("Linked to REQ-301");
    f.link(&t_linked, "REQ-302");
    f.link(&t_other, "REQ-301");

    let removed = f.save_doc(Some(&doc_id), "# Spec\n\n### REQ-301 First\n\nBody.\n");
    let hits = warnings_with_code(&removed, "REQUIREMENT_REMOVED");
    assert_eq!(hits.len(), 1, "{removed}");
    let msg = hits[0]["message"].as_str().unwrap();
    assert!(
        msg.contains("Requirement REQ-302 removed from document."),
        "{msg}"
    );
    assert!(
        msg.contains(&format!("Affected tasks: [{t_linked}]")),
        "{msg}"
    );
    assert!(!msg.contains(&t_other), "{msg}");

    // The task link survives (no auto-unlink).
    let task = f.server.call(
        "handoff_get_task",
        json!({ "project_dir": f.dir.to_string_lossy(), "task_id": t_linked }),
    );
    assert!(
        task["task_links"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["link_type"] == "requirement" && l["label"] == "REQ-302"),
        "{task}"
    );
}

#[test]
fn load_context_suspect_summary_null_then_populated() {
    let mut f = Fixture::new("req-change-suspect");
    let first = f.save_doc(None, BODY_V1);
    let doc_id = first["doc_id"].as_str().unwrap().to_string();

    f.server.call(
        "handoff_trace_report",
        json!({ "project_dir": f.dir.to_string_lossy() }),
    );
    let ctx = f.load_context();
    assert!(
        ctx.as_object().unwrap().contains_key("suspect_summary"),
        "suspect_summary key must always be present: {ctx}"
    );
    assert!(ctx["suspect_summary"].is_null(), "{ctx}");

    // Link a task (records its baseline), then change the requirement's
    // definition: the task-kind suspect shows up once trace_report persists.
    let task = f.create_task("Implements REQ-301");
    f.link(&task, "REQ-301");
    f.save_doc(
        Some(&doc_id),
        &BODY_V1.replace(
            "### REQ-301 First\n\nBody.",
            "### REQ-301 First\n\nRewritten.",
        ),
    );
    f.server.call(
        "handoff_trace_report",
        json!({ "project_dir": f.dir.to_string_lossy() }),
    );

    let ctx = f.load_context();
    let summary = &ctx["suspect_summary"];
    assert!(!summary.is_null(), "{ctx}");
    assert!(summary["total"].as_u64().unwrap() >= 1, "{summary}");
    assert_eq!(summary["stale"], false, "{summary}");
    assert!(
        summary["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["kind"] == "task"
                && i["task_id"] == task.as_str()
                && i["stable_id"] == "REQ-301"),
        "{summary}"
    );
}
