//! Real-binary MCP-tool E2E for `[session_loop] profile` (SPEC-ELITE-031 /
//! SPEC-ELITE-035): the session-loop skill reads its default pipeline profile
//! through `handoff_get_config`, so the key must be readable there, settable
//! through `handoff_update_config`, validated on write, and must survive the
//! tools that rewrite `config.toml` from the typed `Config`.

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
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        (is_error, text)
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

fn new_project(server: &mut Server, name: &str) -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": name }),
    );
    (tmp, pd)
}

fn config_path(pd: &str) -> PathBuf {
    PathBuf::from(pd).join(".handoff").join("config.toml")
}

fn append_to_config(pd: &str, text: &str) {
    let path = config_path(pd);
    let mut raw = std::fs::read_to_string(&path).unwrap();
    raw.push_str(text);
    std::fs::write(&path, raw).unwrap();
}

fn get_config(server: &mut Server, pd: &str) -> Value {
    server.call("handoff_get_config", json!({ "project_dir": pd }))
}

/// SPEC-ELITE-033 / SPEC-ELITE-035 AC2: a fresh project has no
/// `session_loop` key at all, so the skill falls through to its own rules.
#[test]
fn fresh_project_reports_no_session_loop_profile() {
    let mut server = Server::spawn();
    let (_tmp, pd) = new_project(&mut server, "session-loop-profile-unset");

    let cfg = get_config(&mut server, &pd);
    assert!(cfg.get("session_loop").is_none(), "{cfg}");
}

/// SPEC-ELITE-031 AC1 / SPEC-ELITE-035 AC1: a hand-written
/// `[session_loop] profile = "elite"` is what `handoff_get_config` returns.
#[test]
fn hand_written_profile_is_returned_by_get_config() {
    let mut server = Server::spawn();
    let (_tmp, pd) = new_project(&mut server, "session-loop-profile-read");
    append_to_config(&pd, "\n[session_loop]\nprofile = \"elite\"\n");

    let cfg = get_config(&mut server, &pd);
    assert_eq!(cfg["session_loop"]["profile"], "elite", "{cfg}");
}

/// The profile is settable through the config API, for every profile the
/// workflow accepts.
#[test]
fn profile_is_settable_through_update_config() {
    let mut server = Server::spawn();
    let (_tmp, pd) = new_project(&mut server, "session-loop-profile-write");

    for profile in ["express", "standard", "full", "elite"] {
        let (is_error, text) = server.call_raw(
            "handoff_update_config",
            json!({ "project_dir": pd, "updates": { "session_loop.profile": profile } }),
        );
        assert!(!is_error, "{profile}: {text}");
        let cfg = get_config(&mut server, &pd);
        assert_eq!(cfg["session_loop"]["profile"], profile, "{cfg}");
    }

    // `null` clears it: the empty string it leaves behind means "unset", so
    // the skill falls back to its own rules.
    let (is_error, text) = server.call_raw(
        "handoff_update_config",
        json!({ "project_dir": pd, "updates": { "session_loop.profile": null } }),
    );
    assert!(!is_error, "{text}");
    let cfg = get_config(&mut server, &pd);
    assert_eq!(cfg["session_loop"]["profile"], "", "{cfg}");
}

/// A profile the workflow would reject is rejected at write time, before the
/// user plans a session around it, and leaves `config.toml` untouched.
#[test]
fn unknown_profile_is_rejected_without_touching_config() {
    let mut server = Server::spawn();
    let (_tmp, pd) = new_project(&mut server, "session-loop-profile-invalid");
    let original = std::fs::read_to_string(config_path(&pd)).unwrap();

    for (updates, needle) in [
        (json!({ "session_loop.profile": "fast" }), "fast"),
        (json!({ "session_loop.profile": "Elite" }), "Elite"),
        (json!({ "session_loop.profile": 3 }), "session_loop"),
        (json!({ "session_loop": "elite" }), "session_loop"),
        (json!({ "session_loop": { "profile": "turbo" } }), "turbo"),
        // All-or-nothing: a valid typed key in the same batch is not applied.
        (
            json!({ "settings.memory_enabled": false, "session_loop.profile": "fast" }),
            "fast",
        ),
    ] {
        let (is_error, text) = server.call_raw(
            "handoff_update_config",
            json!({ "project_dir": pd, "updates": updates }),
        );
        assert!(is_error && text.contains(needle), "{updates}: {text}");
        assert!(
            text.contains("session_loop"),
            "{updates}: the error should name the section: {text}"
        );
        assert_eq!(
            std::fs::read_to_string(config_path(&pd)).unwrap(),
            original,
            "{updates}: a rejected update must not modify config.toml"
        );
    }
}

/// SPEC-ELITE-035 AC1 (persistence): both ways a tool rewrites `config.toml`
/// keep the profile — re-serializing the typed `Config` (which drops any
/// section `Config` does not know) and editing the document in place. Losing
/// it would silently turn a persistent `elite` activation back into the
/// default.
#[test]
fn profile_survives_typed_config_rewrites() {
    let mut server = Server::spawn();
    let (_tmp, pd) = new_project(&mut server, "session-loop-profile-persist");
    append_to_config(&pd, "\n[session_loop]\nprofile = \"elite\"\n");

    // Typed-key path of handoff_update_config (read Config -> render Config).
    let (is_error, text) = server.call_raw(
        "handoff_update_config",
        json!({ "project_dir": pd, "updates": { "settings.memory_enabled": false } }),
    );
    assert!(!is_error, "{text}");
    let cfg = get_config(&mut server, &pd);
    assert_eq!(cfg["session_loop"]["profile"], "elite", "{cfg}");

    // A dedicated tool that edits the TOML document in place.
    let (is_error, text) = server.call_raw(
        "handoff_update_labels",
        json!({ "project_dir": pd, "labels": ["bug", "architecture"] }),
    );
    assert!(!is_error, "{text}");
    let cfg = get_config(&mut server, &pd);
    assert_eq!(cfg["session_loop"]["profile"], "elite", "{cfg}");
    assert_eq!(cfg["labels"], json!(["bug", "architecture"]), "{cfg}");
}

/// SPEC-ELITE-035 AC3: writing the pipeline profile does not disturb the
/// multi-WT `[worktree.session_loop]` section, and vice versa.
#[test]
fn profile_and_worktree_session_loop_do_not_interfere() {
    let mut server = Server::spawn();
    let (_tmp, pd) = new_project(&mut server, "session-loop-profile-worktree");

    let (is_error, text) = server.call_raw(
        "handoff_update_config",
        json!({ "project_dir": pd, "updates": {
            "worktree.session_loop.auto_assign": true,
            "worktree.session_loop.max_concurrent_wts": 2,
            "session_loop.profile": "elite",
        }}),
    );
    assert!(!is_error, "{text}");

    let (is_error, text) = server.call_raw(
        "handoff_update_config",
        json!({ "project_dir": pd, "updates": { "session_loop.profile": "standard" } }),
    );
    assert!(!is_error, "{text}");

    let cfg = get_config(&mut server, &pd);
    assert_eq!(cfg["session_loop"]["profile"], "standard", "{cfg}");
    assert_eq!(
        cfg["worktree"]["session_loop"]["auto_assign"], true,
        "{cfg}"
    );
    assert_eq!(
        cfg["worktree"]["session_loop"]["max_concurrent_wts"], 2,
        "{cfg}"
    );
    assert!(
        cfg["worktree"]["session_loop"].get("profile").is_none(),
        "{cfg}"
    );
}

/// A hand-edited unknown profile is still served verbatim (the skill is the
/// one that stops on it) and does not block unrelated config updates.
#[test]
fn hand_edited_unknown_profile_does_not_break_other_config_work() {
    let mut server = Server::spawn();
    let (_tmp, pd) = new_project(&mut server, "session-loop-profile-hand-edited");
    append_to_config(&pd, "\n[session_loop]\nprofile = \"fast\"\n");

    let cfg = get_config(&mut server, &pd);
    assert_eq!(cfg["session_loop"]["profile"], "fast", "{cfg}");

    let (is_error, text) = server.call_raw(
        "handoff_update_config",
        json!({ "project_dir": pd, "updates": { "settings.memory_enabled": false } }),
    );
    assert!(!is_error, "an unrelated update must not be blocked: {text}");
    let cfg = get_config(&mut server, &pd);
    assert_eq!(cfg["session_loop"]["profile"], "fast", "{cfg}");
    assert_eq!(cfg["settings"]["memory_enabled"], false, "{cfg}");
}
