//! Real-binary E2E for `agent_id` stability (t439.3, FR-V0473-006).
//!
//! Each test spawns the actual `handoff-mcp` server (stdio JSON-RPC) with a
//! controlled environment: the developer's own `CLAUDE_CODE_SESSION_ID` /
//! `CLAUDE_SESSION_ID` must never leak into the result, so both variables
//! are removed (or set explicitly) on every spawn.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};
use tempfile::TempDir;

const ENV_PRIMARY: &str = "CLAUDE_CODE_SESSION_ID";
const ENV_LEGACY: &str = "CLAUDE_SESSION_ID";

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
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Server {
    /// Spawn a server whose agent-id env vars are exactly `primary` /
    /// `legacy` (`None` removes the variable from the child's environment).
    fn spawn(primary: Option<&str>, legacy: Option<&str>) -> Self {
        let mut cmd = Command::new(binary());
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (key, value) in [(ENV_PRIMARY, primary), (ENV_LEGACY, legacy)] {
            match value {
                Some(v) => cmd.env(key, v),
                None => cmd.env_remove(key),
            };
        }
        let mut child = cmd.spawn().expect("spawn handoff-mcp server");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Server {
            child,
            stdin,
            stdout,
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
            "params": { "name": name, "arguments": arguments }
        });
        writeln!(self.stdin, "{req}").expect("write request");
        self.stdin.flush().expect("flush request");
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read response");
        serde_json::from_str(line.trim()).expect("response is JSON")
    }

    /// Call a tool and return the response text parsed as JSON.
    fn call_json(&mut self, name: &str, arguments: Value) -> Value {
        let resp = self.call(name, arguments);
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no text content in {resp}"));
        serde_json::from_str(text).unwrap_or_else(|_| panic!("text is not JSON: {text}"))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn init_project(server: &mut Server, dir: &TempDir) -> String {
    let project_dir = dir.path().to_string_lossy().to_string();
    server.call(
        "handoff_init",
        json!({ "project_dir": project_dir, "project_name": "agent-id-e2e" }),
    );
    project_dir
}

fn event_count(server: &mut Server, project_dir: &str, event_type: &str) -> usize {
    let parsed = server.call_json(
        "handoff_events",
        json!({ "project_dir": project_dir, "event_type": event_type }),
    );
    parsed["events"].as_array().expect("events array").len()
}

/// Every `*.json` session file under `.handoff/sessions/`, parsed.
fn session_files(project_dir: &str) -> Vec<Value> {
    let dir = Path::new(project_dir).join(".handoff/sessions");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).expect("sessions dir") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            let text = std::fs::read_to_string(&path).expect("read session");
            out.push(serde_json::from_str(&text).expect("session JSON"));
        }
    }
    out
}

/// AC2 (t439.3): two `load_context` calls in one process keep the same
/// `agent_id` and emit exactly one `agent.registered`, even with no env var.
#[test]
fn load_context_twice_keeps_agent_id_and_registers_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = Server::spawn(None, None);
    let project_dir = init_project(&mut server, &dir);

    let first = server.call_json(
        "handoff_load_context",
        json!({ "project_dir": project_dir }),
    );
    let second = server.call_json(
        "handoff_load_context",
        json!({ "project_dir": project_dir }),
    );

    let first_id = first["agent_id"].as_str().expect("agent_id").to_string();
    assert!(
        first_id.starts_with("a-"),
        "generated id format: {first_id}"
    );
    assert_eq!(second["agent_id"], first_id);
    assert_eq!(
        event_count(&mut server, &project_dir, "agent.registered"),
        1
    );

    let agents = std::fs::read_dir(dir.path().join(".handoff/agents"))
        .unwrap()
        .count();
    assert_eq!(agents, 1, "one agent record, not one per call");
}

/// AC3 (t439.3): `save_context` before any `load_context` still stamps the
/// session with an `agent_id`, and the later `load_context` reports the same
/// id (no second identity is minted).
#[test]
fn save_context_before_load_context_has_agent_id() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = Server::spawn(None, None);
    let project_dir = init_project(&mut server, &dir);

    server.call(
        "handoff_save_context",
        json!({
            "project_dir": project_dir,
            "session_status": "active",
            "summary": "saved before load"
        }),
    );
    let sessions = session_files(&project_dir);
    assert_eq!(sessions.len(), 1);
    let session_agent = sessions[0]["agent_id"]
        .as_str()
        .expect("session agent_id must not be None")
        .to_string();

    let loaded = server.call_json(
        "handoff_load_context",
        json!({ "project_dir": project_dir }),
    );
    assert_eq!(loaded["agent_id"], session_agent);
    assert_eq!(
        event_count(&mut server, &project_dir, "agent.registered"),
        1
    );
}

/// AC1 (t439.3): `CLAUDE_CODE_SESSION_ID` wins over `CLAUDE_SESSION_ID`.
#[test]
fn claude_code_session_id_takes_priority_over_legacy() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = Server::spawn(Some("cc-primary-id"), Some("legacy-id"));
    let project_dir = init_project(&mut server, &dir);

    let loaded = server.call_json(
        "handoff_load_context",
        json!({ "project_dir": project_dir }),
    );
    assert_eq!(loaded["agent_id"], "cc-primary-id");
}

/// AC1 (t439.3): `CLAUDE_SESSION_ID` is still honoured when the primary
/// variable is absent.
#[test]
fn legacy_session_id_used_when_primary_absent() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = Server::spawn(None, Some("legacy-only-id"));
    let project_dir = init_project(&mut server, &dir);

    let loaded = server.call_json(
        "handoff_load_context",
        json!({ "project_dir": project_dir }),
    );
    assert_eq!(loaded["agent_id"], "legacy-only-id");
}

/// A second server process with the same env id resolves to the same record
/// (reconnect), and does not emit a second `agent.registered`.
#[test]
fn reconnect_with_same_env_id_does_not_reregister() {
    let dir = tempfile::tempdir().unwrap();
    let project_dir;
    {
        let mut first = Server::spawn(Some("cc-reconnect"), None);
        project_dir = init_project(&mut first, &dir);
        first.call_json(
            "handoff_load_context",
            json!({ "project_dir": project_dir }),
        );
    }
    let mut second = Server::spawn(Some("cc-reconnect"), None);
    let loaded = second.call_json(
        "handoff_load_context",
        json!({ "project_dir": project_dir }),
    );
    assert_eq!(loaded["agent_id"], "cc-reconnect");
    assert_eq!(
        event_count(&mut second, &project_dir, "agent.registered"),
        1
    );
}
