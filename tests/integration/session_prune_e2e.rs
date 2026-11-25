//! Real-binary E2E for the session auto-prune default (t439.7, FR-V0473-010).
//!
//! Spawns the actual `handoff-mcp` server (stdio JSON-RPC) against a copy of
//! the frozen `pre_m1_compat` project, whose `config.toml` carries the legacy
//! `history_limit = 20` that old `handoff_init` runs wrote. Since v0.47.3 that
//! key must not delete anything; only the opt-in `session_prune_limit` does.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};
use tempfile::TempDir;

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
    fn spawn() -> Self {
        let mut child = Command::new(binary())
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CLAUDE_SESSION_ID")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn handoff-mcp server");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Server {
            child,
            stdin,
            stdout,
            next_id: 1,
        }
    }

    /// Returns the tool's text payload; panics on a tool error.
    fn call(&mut self, name: &str, arguments: Value) -> String {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        });
        writeln!(self.stdin, "{req}").expect("write request");
        self.stdin.flush().expect("flush request");
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read response");
        let resp: Value = serde_json::from_str(&line).expect("valid JSON-RPC response");
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !resp["result"]["isError"].as_bool().unwrap_or(false),
            "{name} failed: {text}"
        );
        text
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_recursive(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// Copy of the `pre_m1_compat` project (legacy `history_limit = 20`) with a
/// git repo so `save_context` can capture git state.
fn legacy_project() -> (TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("proj");
    copy_dir_recursive(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pre_m1_compat/project/handoff"),
        &proj.join(".handoff"),
    );
    // The frozen fixture predates any saved session, so it ships no sessions dir.
    std::fs::create_dir_all(proj.join(".handoff/sessions")).unwrap();
    git(&proj, &["init", "-q"]);
    git(
        &proj,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    );
    let pd = proj.to_string_lossy().to_string();
    (tmp, pd)
}

fn closed_count(pd: &str) -> usize {
    std::fs::read_dir(Path::new(pd).join(".handoff/sessions"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".closed.json"))
        .count()
}

fn events(pd: &str) -> Vec<Value> {
    let path = Path::new(pd).join(".handoff/events.jsonl");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn pruned_events(pd: &str) -> Vec<Value> {
    events(pd)
        .into_iter()
        .filter(|e| e["event"] == "session.pruned")
        .collect()
}

fn save(server: &mut Server, pd: &str, n: usize) -> String {
    server.call(
        "handoff_save_context",
        json!({ "project_dir": pd, "summary": format!("e2e session {n:02}") }),
    )
}

fn load_warnings(server: &mut Server, pd: &str) -> Vec<Value> {
    let text = server.call("handoff_load_context", json!({ "project_dir": pd }));
    let v: Value = serde_json::from_str(&text).unwrap();
    v["warnings"].as_array().cloned().unwrap_or_default()
}

/// The deprecation hint must not tell the user to delete the key while an older
/// binary may share the `.handoff/` (it would prune on a missing value).
#[test]
fn deprecation_hint_warns_about_older_binaries_before_removing_the_key() {
    let (_tmp, pd) = legacy_project();
    let mut server = Server::spawn();
    let warnings = load_warnings(&mut server, &pd);
    let w = warnings
        .iter()
        .find(|w| w["code"] == "DEPRECATED_HISTORY_LIMIT")
        .expect("legacy history_limit must be warned about");
    let hint = w["fix_hint"].as_str().unwrap();
    assert!(hint.contains("older than v0.47.3"), "{hint}");
    assert!(hint.contains("as 20 and prunes"), "{hint}");
    assert!(hint.contains("keep a large history_limit"), "{hint}");
    assert!(
        hint.contains("Once all are upgraded"),
        "removal must be conditional: {hint}"
    );
}

fn has_deprecation_warning(warnings: &[Value]) -> bool {
    warnings
        .iter()
        .any(|w| w["code"] == "DEPRECATED_HISTORY_LIMIT")
}

/// AC1: the legacy `history_limit = 20` keeps all 25 saved sessions.
#[test]
fn legacy_history_limit_20_keeps_all_25_sessions() {
    let (_tmp, pd) = legacy_project();
    let mut server = Server::spawn();
    for n in 1..=25 {
        save(&mut server, &pd, n);
    }
    assert_eq!(closed_count(&pd), 25);
    assert!(pruned_events(&pd).is_empty());
}

/// AC4: the legacy key is warned about, `update_config null` removes it, and
/// a fresh `handoff_init` never writes it.
#[test]
fn history_limit_is_warned_removable_and_never_written_by_init() {
    let (_tmp, pd) = legacy_project();
    let mut server = Server::spawn();

    assert!(has_deprecation_warning(&load_warnings(&mut server, &pd)));

    // DS-V0473-311: a number is accepted and stored, with a deprecation
    // warning in the reply, and it never prunes.
    let reply = server.call(
        "handoff_update_config",
        json!({ "project_dir": &pd, "updates": { "settings.history_limit": 5 } }),
    );
    assert!(reply.contains("settings.history_limit = 5"), "{reply}");
    assert!(reply.contains("deprecated"), "{reply}");
    assert!(reply.contains("session_prune_limit"), "{reply}");
    let cfg = std::fs::read_to_string(Path::new(&pd).join(".handoff/config.toml")).unwrap();
    assert!(cfg.contains("history_limit = 5"), "{cfg}");
    for n in 1..=7 {
        save(&mut server, &pd, n);
    }
    assert_eq!(closed_count(&pd), 7, "history_limit must not prune");

    server.call(
        "handoff_update_config",
        json!({ "project_dir": &pd, "updates": { "settings.history_limit": null } }),
    );
    let cfg = std::fs::read_to_string(Path::new(&pd).join(".handoff/config.toml")).unwrap();
    assert!(!cfg.contains("history_limit"), "{cfg}");
    assert!(!has_deprecation_warning(&load_warnings(&mut server, &pd)));

    let fresh = tempfile::tempdir().unwrap();
    git(fresh.path(), &["init", "-q"]);
    let fpd = fresh.path().to_string_lossy().to_string();
    server.call(
        "handoff_init",
        json!({ "project_dir": &fpd, "project_name": "fresh" }),
    );
    let cfg = std::fs::read_to_string(fresh.path().join(".handoff/config.toml")).unwrap();
    assert!(!cfg.contains("history_limit"), "{cfg}");
    assert!(!cfg.contains("session_prune_limit"), "{cfg}");
}

/// AC2: only `session_prune_limit` prunes; each deletion is a
/// `session.pruned` event and the newest session (the head) survives.
#[test]
fn session_prune_limit_prunes_oldest_and_records_events() {
    let (_tmp, pd) = legacy_project();
    let mut server = Server::spawn();
    for n in 1..=10 {
        save(&mut server, &pd, n);
    }
    assert_eq!(closed_count(&pd), 10);

    server.call(
        "handoff_update_config",
        json!({ "project_dir": &pd, "updates": { "settings.session_prune_limit": 4 } }),
    );
    let msg = save(&mut server, &pd, 11);

    assert_eq!(closed_count(&pd), 4);
    assert!(msg.contains("Removed 7 old session(s)"), "{msg}");
    let pruned = pruned_events(&pd);
    assert_eq!(pruned.len(), 7);
    assert!(pruned.iter().all(|e| e["session_id"]
        .as_str()
        .is_some_and(|s| s.starts_with("s-"))));

    // The newest session is still there.
    let sessions: Vec<String> = std::fs::read_dir(Path::new(&pd).join(".handoff/sessions"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        sessions.iter().any(|n| n.contains("e2e-session-11")),
        "{sessions:?}"
    );
}

/// SPEC-V0473-110: with no `continues` data the head is the single newest
/// closed session, not one per branch. Many branches must not pin one session
/// each, and a head that already fits the limit raises no warning.
#[test]
fn session_prune_limit_with_many_branches_keeps_exactly_limit() {
    let (_tmp, pd) = legacy_project();
    let mut server = Server::spawn();
    let proj = PathBuf::from(&pd);
    for n in 1..=6 {
        git(&proj, &["checkout", "-q", "-B", &format!("feature-{n}")]);
        save(&mut server, &pd, n);
    }
    assert_eq!(closed_count(&pd), 6);

    server.call(
        "handoff_update_config",
        json!({ "project_dir": &pd, "updates": { "settings.session_prune_limit": 2 } }),
    );
    let msg = save(&mut server, &pd, 7);

    assert_eq!(closed_count(&pd), 2, "{msg}");
    assert!(!msg.contains("because they are heads"), "{msg}");
    assert_eq!(pruned_events(&pd).len(), 5);
}

/// Review regression: closed saves land within one second, so file names
/// (second + summary slug) do not give the age order. The newest save must
/// survive and stay `previous_session`, whatever its slug sorts like.
#[test]
fn newest_save_survives_prune_even_when_its_slug_sorts_first() {
    let (_tmp, pd) = legacy_project();
    let mut server = Server::spawn();
    server.call(
        "handoff_update_config",
        json!({ "project_dir": &pd, "updates": { "settings.session_prune_limit": 1 } }),
    );
    // Descending slugs: each newer save sorts alphabetically BEFORE the older.
    for summary in ["zulu", "yankee", "xray", "whiskey", "victor"] {
        server.call(
            "handoff_save_context",
            json!({ "project_dir": &pd, "summary": summary }),
        );
    }

    assert_eq!(closed_count(&pd), 1);
    let text = server.call("handoff_load_context", json!({ "project_dir": &pd }));
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["previous_session"]["summary"], "victor", "{text}");
}
