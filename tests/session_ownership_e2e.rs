//! Real-binary E2E for session ownership (t439.5, FR-V0473-008).
//!
//! Every test starts the actual `handoff-mcp` server one or more times against
//! the same `.handoff/`, each with an explicit `CLAUDE_CODE_SESSION_ID` and
//! the legacy `CLAUDE_SESSION_ID` removed, so a developer's own Claude
//! session id never leaks into the result. Two servers = two windows = two
//! `agent_id`s.

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
    project_dir: String,
}

impl Server {
    fn spawn(agent: &str, project_dir: &Path) -> Self {
        let mut cmd = Command::new(binary());
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env(ENV_PRIMARY, agent)
            .env_remove(ENV_LEGACY);
        let mut child = cmd.spawn().expect("spawn handoff-mcp server");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Server {
            child,
            stdin,
            stdout,
            next_id: 1,
            project_dir: project_dir.to_string_lossy().to_string(),
        }
    }

    /// Call a tool (with `project_dir` filled in) and return the raw response.
    fn call(&mut self, name: &str, mut arguments: Value) -> Value {
        arguments["project_dir"] = json!(self.project_dir);
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

    /// Call a tool that must succeed; return its text.
    fn ok(&mut self, name: &str, arguments: Value) -> String {
        let resp = self.call(name, arguments);
        assert!(!is_error(&resp), "{name} failed: {}", text_of(&resp));
        text_of(&resp)
    }

    /// Call a tool that must fail; return the error text.
    fn err(&mut self, name: &str, arguments: Value) -> String {
        let resp = self.call(name, arguments);
        assert!(
            is_error(&resp),
            "{name} unexpectedly ok: {}",
            text_of(&resp)
        );
        text_of(&resp)
    }

    fn load(&mut self, arguments: Value) -> Value {
        serde_json::from_str(&self.ok("handoff_load_context", arguments)).expect("load JSON")
    }

    /// `save_context(session_status: "active")`, returning the session id.
    fn start(&mut self, summary: &str) -> String {
        let text = self.ok(
            "handoff_save_context",
            json!({ "summary": summary, "session_status": "active" }),
        );
        session_id_of(&text)
    }

    fn close(&mut self, summary: &str, extra: Value) -> String {
        let mut args = json!({ "summary": summary });
        merge(&mut args, extra);
        self.ok("handoff_save_context", args)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn merge(into: &mut Value, extra: Value) {
    if let (Some(a), Some(b)) = (into.as_object_mut(), extra.as_object()) {
        for (k, v) in b {
            a.insert(k.clone(), v.clone());
        }
    }
}

fn text_of(resp: &Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"].as_bool().unwrap_or(false)
}

fn session_id_of(text: &str) -> String {
    text.lines()
        .find_map(|l| l.strip_prefix("Session ID: "))
        .unwrap_or_else(|| panic!("no Session ID in: {text}"))
        .trim()
        .to_string()
}

/// A git repo with an initialised `.handoff/`.
fn project() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for args in [
        vec!["init"],
        vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ],
    ] {
        Command::new("git")
            .args(&args)
            .current_dir(dir.path())
            .output()
            .expect("git");
    }
    let mut s = Server::spawn("setup-agent", dir.path());
    s.ok("handoff_init", json!({ "project_name": "ownership" }));
    dir
}

fn sessions(dir: &Path, status: &str) -> Vec<Value> {
    let suffix = format!(".{status}.json");
    let mut out = Vec::new();
    let sessions_dir = dir.join(".handoff/sessions");
    if let Ok(rd) = std::fs::read_dir(sessions_dir) {
        let mut files: Vec<_> = rd.filter_map(|e| e.ok()).collect();
        files.sort_by_key(|e| e.file_name());
        for f in files {
            if f.file_name().to_string_lossy().ends_with(&suffix) {
                out.push(
                    serde_json::from_str(&std::fs::read_to_string(f.path()).unwrap()).unwrap(),
                );
            }
        }
    }
    out
}

fn session_by_id(dir: &Path, status: &str, id: &str) -> Option<Value> {
    sessions(dir, status)
        .into_iter()
        .find(|s| s["id"].as_str() == Some(id))
}

fn events(dir: &Path, kind: &str) -> Vec<Value> {
    std::fs::read_to_string(dir.join(".handoff/events.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|e| e["event"] == kind)
        .collect()
}

fn set_config(s: &mut Server, key: &str, value: Value) {
    s.ok(
        "handoff_update_config",
        json!({ "updates": { key: value } }),
    );
}

// ---- AC7 / AC1 / AC2: two windows on one .handoff ------------------------

#[test]
fn one_agents_closed_save_does_not_close_the_other_agents_active_session() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", dir.path());
    let a_sid = a.start("A is working");

    b.close("B is done", json!({}));

    assert!(
        session_by_id(dir.path(), "active", &a_sid).is_some(),
        "A's session must still be active"
    );
    let closed = sessions(dir.path(), "closed");
    assert_eq!(closed.len(), 1, "B's save becomes its own closed session");
    assert_eq!(closed[0]["summary"], "B is done");
    assert_eq!(
        closed[0]["agent_id"], "agent-b",
        "closed session records its creator"
    );
    assert_eq!(closed[0]["owner_v"], 1);
}

#[test]
fn later_active_session_does_not_take_over_the_earlier_owner() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", dir.path());
    let a_sid = a.start("A is working");

    let b_sid = b.start("B is working");

    assert_ne!(a_sid, b_sid, "B gets its own session");
    let a_session = session_by_id(dir.path(), "active", &a_sid).expect("A's session");
    assert_eq!(a_session["agent_id"], "agent-a", "A keeps ownership");
    assert_eq!(
        a_session["summary"], "A is working",
        "A's content untouched"
    );
    let b_session = session_by_id(dir.path(), "active", &b_sid).expect("B's session");
    assert_eq!(b_session["agent_id"], "agent-b");
}

#[test]
fn keep_active_update_targets_the_callers_own_active_session() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", dir.path());
    let a_sid = a.start("A v1");
    let b_sid = b.start("B v1");

    let text = a.ok(
        "handoff_save_context",
        json!({ "summary": "A v2", "session_status": "active" }),
    );

    assert_eq!(session_id_of(&text), a_sid);
    assert_eq!(
        session_by_id(dir.path(), "active", &a_sid).unwrap()["summary"],
        "A v2"
    );
    assert_eq!(
        session_by_id(dir.path(), "active", &b_sid).unwrap()["summary"],
        "B v1"
    );
}

#[test]
fn agent_id_is_only_overwritten_when_the_owner_is_none() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", dir.path());
    let a_sid = a.start("A v1");

    // B names A's session explicitly with keep_active: allowed (additive),
    // but the owner stays A and the write is logged.
    let text = b.ok(
        "handoff_save_context",
        json!({ "summary": "B edits", "session_status": "active", "session_id": a_sid }),
    );

    assert!(text.contains("Warning"), "warns: {text}");
    assert_eq!(session_id_of(&text), a_sid);
    let a_session = session_by_id(dir.path(), "active", &a_sid).unwrap();
    assert_eq!(a_session["agent_id"], "agent-a", "owner is not rewritten");
    assert_eq!(
        a_session["summary"], "A v1",
        "the owner's summary is not replaced by a foreign write"
    );
    assert_eq!(sessions(dir.path(), "active").len(), 1, "no extra session");
    assert!(events(dir.path(), "session.close_refused").is_empty());
    let writes = events(dir.path(), "session.foreign_write");
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0]["agent_id"], "agent-b");
    assert_eq!(writes[0]["session_id"], a_sid.as_str());
}

#[test]
fn foreign_keep_active_write_appends_and_never_erases_the_owners_content() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", dir.path());
    let a_sid = a.start("A v1");
    a.ok(
        "handoff_save_context",
        json!({
            "summary": "A v2",
            "session_status": "active",
            "decisions": [
                { "decision": "A1", "reason": "r", "confidence": "high" },
                { "decision": "A2", "reason": "r", "confidence": "high" }
            ],
            "checklist": [{ "item": "A task", "checked": true, "owner": "ai" }],
            "handoff_notes": [{ "note": "A note", "category": "context" }],
            "context_pointers": [{ "path": "src/a.rs", "reason": "A" }]
        }),
    );

    let text = b.ok(
        "handoff_save_context",
        json!({
            "summary": "B overwrites",
            "session_status": "active",
            "session_id": a_sid,
            "decisions": [{ "decision": "B1", "reason": "r", "confidence": "high" }],
            "checklist": [{ "item": "B task", "checked": false, "owner": "ai" }],
            "handoff_notes": [{ "note": "B note", "category": "context" }],
            "context_pointers": [{ "path": "src/b.rs", "reason": "B" }]
        }),
    );

    assert!(text.contains("Warning"), "existing warning kept: {text}");
    let s = session_by_id(dir.path(), "active", &a_sid).unwrap();
    assert_eq!(s["summary"], "A v2", "the owner's summary is kept");
    assert_eq!(s["agent_id"], "agent-a");
    let decisions: Vec<&str> = s["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["decision"].as_str())
        .collect();
    assert_eq!(decisions, vec!["A1", "A2", "B1"]);
    let items: Vec<&str> = s["checklist"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["item"].as_str())
        .collect();
    assert_eq!(items, vec!["A task", "B task"]);
    assert_eq!(s["handoff_notes"].as_array().unwrap().len(), 2);
    assert_eq!(s["context_pointers"].as_array().unwrap().len(), 2);
    assert_eq!(events(dir.path(), "session.foreign_write").len(), 1);
}

#[test]
fn foreign_keep_active_write_does_not_leave_the_caller_with_two_actives() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", dir.path());
    let a_sid = a.start("A v1");
    let b_sid = b.start("B v1");

    b.ok(
        "handoff_save_context",
        json!({ "summary": "B edits A", "session_status": "active", "session_id": a_sid }),
    );

    assert_eq!(sessions(dir.path(), "active").len(), 2);
    // B's next implicit closed save still finds exactly one own active.
    let text = b.close("B done", json!({}));
    assert_eq!(session_id_of(&text), b_sid);
    assert!(session_by_id(dir.path(), "closed", &b_sid).is_some());
}

// ---- AC4 / AC5: explicit ids on another agent's active session -----------

#[test]
fn closing_another_agents_active_session_by_id_is_refused_with_no_side_effects() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", dir.path());
    let a_sid = a.start("A is working");

    let err = b.err(
        "handoff_save_context",
        json!({ "summary": "B wanted to close A", "close_session_id": a_sid }),
    );

    assert!(err.contains(&a_sid), "names the protected session: {err}");
    assert!(err.contains("force_other_owner"), "{err}");
    let a_session = session_by_id(dir.path(), "active", &a_sid).expect("A still active");
    assert_eq!(a_session["summary"], "A is working", "content untouched");
    assert!(
        sessions(dir.path(), "closed").is_empty(),
        "no new closed session"
    );
    assert_eq!(sessions(dir.path(), "active").len(), 1);
    let refused = events(dir.path(), "session.close_refused");
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0]["agent_id"], "agent-b");
    assert_eq!(refused[0]["session_id"], a_sid.as_str());
}

#[test]
fn session_id_plus_closed_on_another_agents_active_session_is_refused() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", dir.path());
    let a_sid = a.start("A is working");

    b.err(
        "handoff_save_context",
        json!({ "summary": "B tries", "session_id": a_sid }),
    );

    let a_session = session_by_id(dir.path(), "active", &a_sid).expect("A still active");
    assert_eq!(a_session["summary"], "A is working");
    assert!(sessions(dir.path(), "closed").is_empty());
    assert_eq!(events(dir.path(), "session.close_refused").len(), 1);
}

#[test]
fn closing_a_paused_session_of_another_worktree_keeps_its_git_state() {
    let dir = project();
    let mut b = Server::spawn("agent-b", dir.path());
    let id = "s-20260101-000000-000009";
    let body = json!({
        "version": 2,
        "id": id,
        "ended_at": "2026-01-01T00:00:00+00:00",
        "summary": "paused elsewhere",
        "branch": "feature/elsewhere",
        "commit": "abc1234",
        "dirty_files": ["elsewhere.rs"],
        "agent_id": "agent-a",
        "owner_v": 1,
        "worktree": "/some/other/worktree",
    });
    std::fs::write(
        dir.path()
            .join(".handoff/sessions/20260101-000000-paused-elsewhere.paused.json"),
        serde_json::to_string_pretty(&body).unwrap(),
    )
    .unwrap();

    b.close("discard it", json!({ "close_session_id": id }));

    let closed = session_by_id(dir.path(), "closed", id).expect("closed in place");
    assert_eq!(closed["summary"], "discard it", "summary applied");
    assert_eq!(closed["branch"], "feature/elsewhere");
    assert_eq!(closed["commit"], "abc1234");
    assert_eq!(closed["dirty_files"], json!(["elsewhere.rs"]));
}

#[test]
fn force_other_owner_closes_with_reason_and_applies_the_summary_to_the_target() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", dir.path());
    let a_sid = a.start("A is working");

    b.close(
        "closed by B",
        json!({
            "close_session_id": a_sid,
            "force_other_owner": true,
            "reason": "A's window crashed",
            "decisions": [{ "decision": "cleanup", "reason": "crash", "confidence": "confirmed" }]
        }),
    );

    assert!(session_by_id(dir.path(), "active", &a_sid).is_none());
    let closed = session_by_id(dir.path(), "closed", &a_sid).expect("target closed in place");
    assert_eq!(
        closed["summary"], "closed by B",
        "content applied to the target"
    );
    assert_eq!(closed["decisions"][0]["decision"], "cleanup");
    assert_eq!(closed["agent_id"], "agent-a", "owner is not rewritten");
    assert_eq!(sessions(dir.path(), "closed").len(), 1, "no extra session");
    let forced = events(dir.path(), "session.foreign_close");
    assert_eq!(forced.len(), 1);
    assert!(
        forced[0]["detail"]
            .as_str()
            .unwrap()
            .contains("A's window crashed"),
        "reason in the event: {}",
        forced[0]
    );
    // SPEC-V0473-108 AC2: the close event itself carries the justification.
    let closed_events: Vec<Value> = events(dir.path(), "session.closed")
        .into_iter()
        .filter(|e| e["session_id"] == a_sid.as_str())
        .collect();
    assert_eq!(closed_events.len(), 1);
    let detail: Value = serde_json::from_str(
        closed_events[0]["detail"]
            .as_str()
            .expect("detail on close"),
    )
    .expect("JSON detail");
    assert_eq!(detail["reason"], "A's window crashed");
    assert_eq!(detail["forced"], true);
    assert_eq!(detail["previous_owner"], "agent-a");
}

#[test]
fn force_other_owner_without_a_reason_is_an_error_with_no_side_effects() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", dir.path());
    let a_sid = a.start("A is working");

    let err = b.err(
        "handoff_save_context",
        json!({ "summary": "x", "close_session_id": a_sid, "force_other_owner": true }),
    );

    assert!(err.contains("reason"), "{err}");
    assert!(session_by_id(dir.path(), "active", &a_sid).is_some());
    assert!(sessions(dir.path(), "closed").is_empty());
}

#[test]
fn paused_session_of_another_agent_can_be_closed_with_a_foreign_close_event() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", dir.path());
    let a_sid = a.start("A is working");
    a.ok(
        "handoff_save_context",
        json!({ "pause_only": true, "pause_session_id": a_sid }),
    );
    assert!(session_by_id(dir.path(), "paused", &a_sid).is_some());

    b.close(
        "discard A's paused work",
        json!({ "close_session_id": a_sid }),
    );

    assert!(session_by_id(dir.path(), "closed", &a_sid).is_some());
    assert_eq!(events(dir.path(), "session.foreign_close").len(), 1);
    assert!(events(dir.path(), "session.close_refused").is_empty());
}

#[test]
fn pausing_another_agents_active_session_by_id_is_an_error() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", dir.path());
    let a_sid = a.start("A is working");

    let err = b.err(
        "handoff_save_context",
        json!({ "pause_only": true, "pause_session_id": a_sid }),
    );

    assert!(err.contains("force_other_owner"), "{err}");
    assert!(session_by_id(dir.path(), "active", &a_sid).is_some());
    assert!(sessions(dir.path(), "paused").is_empty());
    let refused = events(dir.path(), "session.close_refused");
    assert_eq!(
        refused.len(),
        1,
        "a refused pause is logged like a refused close"
    );
    assert_eq!(refused[0]["agent_id"], "agent-b");
    assert_eq!(refused[0]["session_id"], a_sid.as_str());
}

#[test]
fn pause_active_pauses_only_the_callers_own_sessions() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", dir.path());
    let a_sid = a.start("A is working");
    let b_sid = b.start("B is working");

    b.ok(
        "handoff_save_context",
        json!({ "pause_only": true, "pause_active": true }),
    );

    assert!(session_by_id(dir.path(), "active", &a_sid).is_some());
    assert!(session_by_id(dir.path(), "paused", &b_sid).is_some());
}

#[test]
fn pause_active_over_an_ownerless_session_warns_and_logs_a_foreign_close() {
    let dir = project();
    let legacy = write_legacy_active(dir.path(), Some("a-20260101-000000-000000"), None);
    let mut b = Server::spawn("agent-b", dir.path());

    let text = b.ok(
        "handoff_save_context",
        json!({ "pause_only": true, "pause_active": true }),
    );

    assert!(text.contains("no recorded owner"), "warns: {text}");
    assert!(session_by_id(dir.path(), "paused", &legacy).is_some());
    let ev = events(dir.path(), "session.foreign_close");
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!(ev[0]["session_id"], legacy.as_str());
    assert_eq!(ev[0]["agent_id"], "agent-b");
}

#[test]
fn import_over_an_ownerless_session_warns_and_logs_a_foreign_close() {
    let dir = project();
    let legacy = write_legacy_active(dir.path(), None, None);
    let mut b = Server::spawn("agent-b", dir.path());

    let text = b.ok(
        "handoff_import_context",
        json!({
            "source": { "description": "notes", "format": "other" },
            "session": { "summary": "imported by B" }
        }),
    );

    assert!(text.contains("no recorded owner"), "warns: {text}");
    assert!(session_by_id(dir.path(), "closed", &legacy).is_some());
    assert_eq!(events(dir.path(), "session.foreign_close").len(), 1);
}

// ---- update_session -------------------------------------------------------

#[test]
fn update_session_without_an_own_active_session_errors_and_changes_nothing() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", dir.path());
    let a_sid = a.start("A is working");

    let err = b.err(
        "handoff_update_session",
        json!({ "add_decision": { "decision": "from B", "reason": "r" } }),
    );

    assert!(err.contains("save_context"), "guides to create one: {err}");
    let a_session = session_by_id(dir.path(), "active", &a_sid).unwrap();
    assert!(
        a_session.get("decisions").is_none(),
        "A untouched: {a_session}"
    );
}

#[test]
fn update_session_with_explicit_foreign_id_appends_with_warning_event() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", dir.path());
    let a_sid = a.start("A is working");

    let text = b.ok(
        "handoff_update_session",
        json!({
            "session_id": a_sid,
            "add_decision": { "decision": "from B", "reason": "r" }
        }),
    );

    assert!(text.contains("Warning"), "{text}");
    let a_session = session_by_id(dir.path(), "active", &a_sid).unwrap();
    assert_eq!(a_session["decisions"][0]["decision"], "from B");
    assert_eq!(a_session["agent_id"], "agent-a");
    assert_eq!(events(dir.path(), "session.foreign_write").len(), 1);
}

#[test]
fn update_session_prefers_the_callers_own_active_session() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", dir.path());
    let a_sid = a.start("A is working");
    let b_sid = b.start("B is working");

    b.ok(
        "handoff_update_session",
        json!({ "add_decision": { "decision": "from B", "reason": "r" } }),
    );

    assert_eq!(
        session_by_id(dir.path(), "active", &b_sid).unwrap()["decisions"][0]["decision"],
        "from B"
    );
    assert!(session_by_id(dir.path(), "active", &a_sid)
        .unwrap()
        .get("decisions")
        .is_none());
}

// ---- load_context: select, adoptable, adopt ------------------------------

#[test]
fn load_context_does_not_select_another_agents_active_and_lists_it_as_adoptable() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", dir.path());
    let a_sid = a.start("A is working");
    a.ok(
        "handoff_update_session",
        json!({ "add_decision": { "decision": "secret decision body", "reason": "r" } }),
    );

    let ctx = b.load(json!({}));

    assert!(
        ctx.get("session_id").is_none(),
        "no selected session: {ctx}"
    );
    assert!(ctx.get("decisions").is_none());
    let adoptable = ctx["adoptable_sessions"]
        .as_array()
        .expect("adoptable_sessions");
    assert_eq!(adoptable.len(), 1);
    assert_eq!(adoptable[0]["id"], a_sid.as_str());
    assert_eq!(adoptable[0]["status"], "active");
    assert!(
        adoptable[0].get("decisions").is_none(),
        "no body: {}",
        adoptable[0]
    );
    assert!(adoptable[0]["updated_at"].is_string());
    assert!(
        ctx.get("active_sessions").is_none(),
        "A's session is not B's: {ctx}"
    );
    assert_eq!(ctx["session_guidance"]["action"], "adopt_or_create");
    assert!(session_by_id(dir.path(), "active", &a_sid).is_some());
}

#[test]
fn load_context_without_foreign_sessions_still_guides_to_create_a_session() {
    let dir = project();
    let mut b = Server::spawn("agent-b", dir.path());

    let ctx = b.load(json!({}));

    assert_eq!(ctx["session_guidance"]["action"], "create_session");
    assert!(ctx.get("adoptable_sessions").is_none());
}

#[test]
fn load_context_selects_the_callers_own_active_among_several() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", dir.path());
    a.start("A is working");
    let b_sid = b.start("B is working");

    let ctx = b.load(json!({}));

    assert_eq!(ctx["session_id"], b_sid.as_str());
    assert!(
        ctx.get("active_sessions").is_none(),
        "only one of mine: {ctx}"
    );
}

#[test]
fn adopting_another_agents_active_session_needs_adopt_true() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", dir.path());
    let a_sid = a.start("A is working");

    let err = b.err("handoff_load_context", json!({ "session_id": a_sid }));
    assert!(err.contains("adopt"), "{err}");
    assert_eq!(
        session_by_id(dir.path(), "active", &a_sid).unwrap()["agent_id"],
        "agent-a",
        "refusal has no side effects"
    );

    let ctx = b.load(json!({ "session_id": a_sid, "adopt": true }));

    assert_eq!(ctx["session_id"], a_sid.as_str());
    let adopted = session_by_id(dir.path(), "active", &a_sid).unwrap();
    assert_eq!(adopted["agent_id"], "agent-b");
    assert_eq!(
        adopted["worktree"],
        dir.path().to_string_lossy().to_string()
    );
    let ev = events(dir.path(), "session.adopted");
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0]["agent_id"], "agent-b");
    let detail = ev[0]["detail"].as_str().unwrap();
    assert!(detail.contains("agent-a"), "old owner in detail: {detail}");
    assert!(detail.contains("agent-b"), "new owner in detail: {detail}");
}

#[test]
fn paused_session_of_another_agent_resumes_by_id_alone_and_is_adopted() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", dir.path());
    let a_sid = a.start("A is working");
    a.ok(
        "handoff_save_context",
        json!({ "pause_only": true, "pause_session_id": a_sid }),
    );

    let ctx = b.load(json!({ "session_id": a_sid }));

    assert_eq!(ctx["session_id"], a_sid.as_str());
    let resumed = session_by_id(dir.path(), "active", &a_sid).expect("resumed");
    assert_eq!(resumed["agent_id"], "agent-b");
    assert_eq!(events(dir.path(), "session.resumed").len(), 1);
    assert_eq!(events(dir.path(), "session.adopted").len(), 1);
}

#[test]
fn restarted_owner_resumes_from_adoptable_sessions() {
    let dir = project();
    let a_sid = {
        let mut a = Server::spawn("agent-a", dir.path());
        a.start("work before the crash")
        // `a` is dropped here: the window is gone.
    };
    let mut a2 = Server::spawn("agent-a-restarted", dir.path());

    let ctx = a2.load(json!({}));
    let adoptable = ctx["adoptable_sessions"].as_array().expect("adoptable");
    assert_eq!(adoptable[0]["id"], a_sid.as_str());
    assert_eq!(ctx["session_guidance"]["action"], "adopt_or_create");

    let resumed = a2.load(json!({ "session_id": a_sid, "adopt": true }));
    assert_eq!(resumed["session_id"], a_sid.as_str());
    assert_eq!(
        session_by_id(dir.path(), "active", &a_sid).unwrap()["agent_id"],
        "agent-a-restarted"
    );
}

#[test]
fn multi_session_false_counts_only_the_callers_own_actives_when_resuming() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", dir.path());
    set_config(&mut a, "settings.multi_session", json!(false));
    let a_sid = a.start("A is working");
    let b_sid = b.start("B is working");
    b.ok(
        "handoff_save_context",
        json!({ "pause_only": true, "pause_session_id": b_sid }),
    );

    // A still has its own active session, so resuming is refused ...
    let err = a.err("handoff_load_context", json!({ "session_id": b_sid }));
    assert!(err.contains("already active"), "{err}");

    // ... but B, whose only other active session belongs to A, may resume.
    let ctx = b.load(json!({ "session_id": b_sid }));
    assert_eq!(ctx["session_id"], b_sid.as_str());
    assert!(session_by_id(dir.path(), "active", &a_sid).is_some());
}

#[test]
fn multi_session_false_keep_active_creates_a_new_session_next_to_a_foreign_one() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", dir.path());
    set_config(&mut a, "settings.multi_session", json!(false));
    a.start("A is working");

    b.start("B is working");

    assert_eq!(sessions(dir.path(), "active").len(), 2);
}

#[test]
fn multiple_open_sessions_bail_counts_only_the_callers_and_ownerless_ones() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", dir.path());
    // Two imports by A leave two open sessions owned by A.
    for n in 0..2 {
        a.ok(
            "handoff_import_context",
            json!({
                "source": { "description": "notes", "format": "other" },
                "session": { "summary": format!("imported {n}") }
            }),
        );
    }
    assert_eq!(sessions(dir.path(), "open").len(), 2);

    // B has none of its own: no bail, nothing activated.
    let ctx = b.load(json!({}));
    assert!(ctx.get("session_id").is_none());
    assert_eq!(sessions(dir.path(), "open").len(), 2, "A's opens untouched");

    // A has two of its own: bail.
    let err = a.err("handoff_load_context", json!({}));
    assert!(err.contains("Multiple open sessions"), "{err}");
}

#[test]
fn import_context_closes_only_the_callers_active_session() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", dir.path());
    let a_sid = a.start("A is working");
    let b_sid = b.start("B is working");

    b.ok(
        "handoff_import_context",
        json!({
            "source": { "description": "notes", "format": "other" },
            "session": { "summary": "imported by B" }
        }),
    );

    assert!(session_by_id(dir.path(), "active", &a_sid).is_some());
    assert!(session_by_id(dir.path(), "closed", &b_sid).is_some());
    let opened = sessions(dir.path(), "open");
    assert_eq!(opened.len(), 1);
    assert_eq!(
        opened[0]["agent_id"], "agent-b",
        "imported session is the caller's"
    );
}

// ---- fork / merge ---------------------------------------------------------

#[test]
fn forked_session_belongs_to_the_caller() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", dir.path());
    let a_sid = a.start("A is working");

    let text = b.ok(
        "handoff_fork_session",
        json!({ "source_session_id": a_sid, "summary": "B's branch" }),
    );

    let forked = sessions(dir.path(), "active")
        .into_iter()
        .find(|s| s["summary"] == "B's branch")
        .unwrap_or_else(|| panic!("forked session missing: {text}"));
    assert_eq!(forked["agent_id"], "agent-b");
    assert_eq!(forked["parent_session_id"], a_sid.as_str());
    assert_eq!(
        session_by_id(dir.path(), "active", &a_sid).unwrap()["agent_id"],
        "agent-a"
    );
}

#[test]
fn merge_is_refused_atomically_when_a_source_is_another_agents_active_session() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", dir.path());
    let a_sid = a.start("A is working");
    let b_sid = b.start("B is working");

    let err = b.err(
        "handoff_merge_sessions",
        json!({
            "source_session_ids": [a_sid, b_sid],
            "target_session_id": b_sid
        }),
    );

    assert!(err.contains(&a_sid), "{err}");
    assert!(
        session_by_id(dir.path(), "active", &a_sid).is_some(),
        "A not closed"
    );
    let b_session = session_by_id(dir.path(), "active", &b_sid).unwrap();
    assert!(b_session.get("decisions").is_none(), "target not modified");
}

#[test]
fn merge_with_force_other_owner_closes_the_foreign_source() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", dir.path());
    let a_sid = a.start("A is working");
    let b_sid = b.start("B is working");

    b.ok(
        "handoff_merge_sessions",
        json!({
            "source_session_ids": [a_sid, b_sid],
            "target_session_id": b_sid,
            "force_other_owner": true,
            "reason": "consolidating after A's crash"
        }),
    );

    assert!(session_by_id(dir.path(), "closed", &a_sid).is_some());
    assert!(session_by_id(dir.path(), "active", &b_sid).is_some());
    let ev = events(dir.path(), "session.foreign_close");
    assert!(
        ev.iter()
            .any(|e| e["detail"].as_str().unwrap_or("").contains("consolidating")),
        "reason recorded: {ev:?}"
    );
}

// ---- legacy data and the CLI ----------------------------------------------

fn write_legacy_active(dir: &Path, agent: Option<&str>, worktree: Option<&str>) -> String {
    let id = "s-20260101-000000-000001";
    let mut body = json!({
        "version": 2,
        "id": id,
        "ended_at": "2026-01-01T00:00:00+00:00",
        "summary": "legacy active session",
    });
    if let Some(a) = agent {
        body["agent_id"] = json!(a);
    }
    if let Some(w) = worktree {
        body["worktree"] = json!(w);
    }
    let path = dir.join(".handoff/sessions/20260101-000000-legacy-active-session.active.json");
    std::fs::write(path, serde_json::to_string_pretty(&body).unwrap()).unwrap();
    id.to_string()
}

#[test]
fn a_session_without_owner_v_is_ownerless_and_can_be_closed_by_anyone_with_a_warning() {
    let dir = project();
    // The `agent_id` here comes from the old per-call generator; it must not
    // protect the session from a different agent.
    let legacy = write_legacy_active(dir.path(), Some("a-20260101-000000-000000"), None);
    let mut b = Server::spawn("agent-b", dir.path());

    let text = b.close("B closes the legacy session", json!({}));

    assert!(text.contains("Warning"), "{text}");
    assert!(session_by_id(dir.path(), "active", &legacy).is_none());
    let closed = session_by_id(dir.path(), "closed", &legacy).expect("legacy closed in place");
    assert_eq!(closed["summary"], "B closes the legacy session");
}

#[test]
fn keep_active_on_an_ownerless_session_claims_it_for_the_caller() {
    let dir = project();
    let legacy = write_legacy_active(dir.path(), Some("a-20260101-000000-000000"), None);
    let mut b = Server::spawn("agent-b", dir.path());

    b.ok(
        "handoff_save_context",
        json!({ "summary": "B continues", "session_status": "active" }),
    );

    let s = session_by_id(dir.path(), "active", &legacy).unwrap();
    assert_eq!(s["agent_id"], "agent-b");
    assert_eq!(s["owner_v"], 1);
}

fn cli(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(binary())
        .args(args)
        .arg("--project-dir")
        .arg(dir)
        .env_remove(ENV_PRIMARY)
        .env_remove(ENV_LEGACY)
        .output()
        .expect("run cli");
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

#[test]
fn cli_without_agent_id_must_name_the_session_when_an_owned_one_is_active() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    let a_sid = a.start("A is working");

    let (ok, text) = cli(dir.path(), &["session", "save", "--summary", "cli note"]);
    assert!(!ok, "implicit CLI save must fail: {text}");
    assert!(text.contains("session_id"), "{text}");
    assert!(session_by_id(dir.path(), "active", &a_sid).is_some());

    // Naming it explicitly still works, with a warning and an event.
    let (ok, text) = cli(
        dir.path(),
        &[
            "session",
            "save",
            "--summary",
            "cli close",
            "--session-id",
            &a_sid,
        ],
    );
    assert!(ok, "{text}");
    assert!(text.contains("Warning"), "{text}");
    assert!(session_by_id(dir.path(), "closed", &a_sid).is_some());
    assert_eq!(events(dir.path(), "session.foreign_close").len(), 1);
}

#[test]
fn cli_without_any_active_session_creates_a_closed_one() {
    let dir = project();
    let (ok, text) = cli(dir.path(), &["session", "save", "--summary", "cli only"]);
    assert!(ok, "{text}");
    let closed = sessions(dir.path(), "closed");
    assert_eq!(closed.len(), 1);
    assert!(closed[0].get("agent_id").is_none(), "CLI has no identity");
}

#[test]
fn cli_closes_its_own_ownerless_active_session_without_ceremony() {
    let dir = project();
    let (ok, text) = cli(
        dir.path(),
        &[
            "session",
            "save",
            "--summary",
            "cli start",
            "--session-status",
            "active",
        ],
    );
    assert!(ok, "{text}");
    let (ok, text) = cli(dir.path(), &["session", "save", "--summary", "cli end"]);
    assert!(ok, "{text}");
    assert!(sessions(dir.path(), "active").is_empty());
    assert_eq!(sessions(dir.path(), "closed").len(), 1);
    assert!(events(dir.path(), "session.foreign_close").is_empty());
}

#[test]
fn cli_load_context_does_not_register_an_agent() {
    let dir = project();
    let agents_before = std::fs::read_dir(dir.path().join(".handoff/agents"))
        .map(|rd| rd.count())
        .unwrap_or(0);
    let (ok, text) = cli(dir.path(), &["session", "load"]);
    assert!(ok, "{text}");
    let agents_after = std::fs::read_dir(dir.path().join(".handoff/agents"))
        .map(|rd| rd.count())
        .unwrap_or(0);
    assert_eq!(
        agents_before, agents_after,
        "CLI must not leave an agent record"
    );
}

// ---- multi-worktree regression --------------------------------------------

#[test]
fn same_agent_in_two_worktrees_keeps_one_session_per_worktree() {
    let dir = project();
    let wt_holder = tempfile::tempdir().unwrap();
    let wt = wt_holder.path().join("second");
    let status = Command::new("git")
        .args(["worktree", "add", "-b", "second", wt.to_str().unwrap()])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());

    let mut main = Server::spawn("agent-a", dir.path());
    let primary_sid = main.start("primary work");
    // The same agent saves from the other worktree.
    let mut wt_args = json!({ "summary": "worktree work", "session_status": "active" });
    wt_args["project_dir"] = json!(wt.to_string_lossy());
    let resp = {
        // `call` overwrites project_dir, so address the worktree explicitly.
        let mut s = Server::spawn("agent-a", dir.path());
        s.project_dir = wt.to_string_lossy().to_string();
        s.call("handoff_save_context", wt_args)
    };
    assert!(!is_error(&resp), "{}", text_of(&resp));
    let wt_sid = session_id_of(&text_of(&resp));

    assert_ne!(
        primary_sid, wt_sid,
        "a different worktree gets its own session"
    );
    assert_eq!(sessions(dir.path(), "active").len(), 2);
}

// ---- auto-parenting never uses another agent's primary session -----------

#[test]
fn worktree_session_is_not_parented_to_another_agents_primary_session() {
    let dir = project();
    let wt_parent = tempfile::tempdir().unwrap();
    let wt = wt_parent.path().join("feature-wt");
    let out = Command::new("git")
        .args(["worktree", "add", "-b", "feat", wt.to_str().unwrap()])
        .current_dir(dir.path())
        .output()
        .expect("git worktree add");
    assert!(out.status.success(), "{out:?}");
    let mut a = Server::spawn("agent-a", dir.path());
    let mut b = Server::spawn("agent-b", &wt);
    let mut a_in_wt = Server::spawn("agent-a", &wt);
    let primary_sid = a.start("A primary work");

    let b_sid = b.start("B in the worktree");
    let a_wt_sid = a_in_wt.start("A in the worktree");

    let b_session = session_by_id(dir.path(), "active", &b_sid).expect("B's session");
    assert_eq!(b_session["scope"], "worktree");
    assert!(
        b_session["parent_session_id"].is_null(),
        "another agent's primary is not B's parent: {b_session}"
    );
    let a_session = session_by_id(dir.path(), "active", &a_wt_sid).expect("A's worktree session");
    assert_eq!(
        a_session["parent_session_id"],
        primary_sid.as_str(),
        "the owner's own primary session is its parent"
    );
}

// ---- an unreadable closed file does not block a save ---------------------

#[test]
fn an_unparseable_closed_file_does_not_block_save_context() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    a.close("first", json!({}));
    std::fs::write(
        dir.path()
            .join(".handoff/sessions/29990101-000000-conflicted.closed.json"),
        "<<<<<<< HEAD\n{}\n=======\n{}\n>>>>>>> other\n",
    )
    .unwrap();

    let text = a.ok(
        "handoff_save_context",
        json!({ "summary": "second", "session_status": "active" }),
    );

    assert!(text.contains("Session saved: second"), "{text}");
    assert!(text.contains("conflicted.closed.json"), "warns: {text}");
    assert_eq!(sessions(dir.path(), "active").len(), 1);
}

#[test]
fn an_unparseable_closed_file_does_not_block_load_context() {
    let dir = project();
    let mut a = Server::spawn("agent-a", dir.path());
    a.close("first", json!({}));
    std::fs::write(
        dir.path()
            .join(".handoff/sessions/29990101-000000-conflicted.closed.json"),
        "not json",
    )
    .unwrap();

    let loaded = a.load(json!({}));

    let warnings = loaded["warnings"].to_string();
    assert!(warnings.contains("conflicted.closed.json"), "{warnings}");
}
