//! Real-binary E2E for the recommended `.handoff/.gitignore` (t439.12,
//! FR-V0473-015): the files the server writes only for local coordination
//! (locks, agent records, timer state, injection caches) must not show up in
//! `git status` of a repository that tracks `.handoff/`.
//!
//! The ignore rules are never written by the test: they are whatever the
//! binary itself produced, so a missing or wrong rule shows up as a real
//! `git status` entry.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use handoff_mcp::storage::gitignore::RECOMMENDED_GITIGNORE_ENTRIES;
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
    project_dir: String,
}

impl Server {
    fn spawn(project_dir: &Path) -> Self {
        let mut child = Command::new(binary())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env("CLAUDE_CODE_SESSION_ID", "gitignore-e2e")
            .env_remove("CLAUDE_SESSION_ID")
            .spawn()
            .expect("spawn handoff-mcp server");
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

    fn ok(&mut self, name: &str, mut arguments: Value) -> String {
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
        let resp: Value = serde_json::from_str(line.trim()).expect("response is JSON");
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("")
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

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// `git status` lines (every untracked file listed) that are not tracked data.
fn status(dir: &Path) -> Vec<String> {
    git(dir, &["status", "--porcelain", "--untracked-files=all"])
        .lines()
        .map(str::to_string)
        .collect()
}

fn local_state_entries(status: &[String]) -> Vec<&String> {
    status
        .iter()
        .filter(|l| {
            l.contains(".lock")
                || l.contains("agents/")
                || l.contains("timer/")
                || l.contains("injected/")
        })
        .collect()
}

fn gitignore_bytes(project: &Path) -> Vec<u8> {
    std::fs::read(project.join(".handoff/.gitignore")).unwrap_or_default()
}

/// Exercises every path that writes machine-local state under `.handoff/`.
fn exercise_local_state_paths(server: &mut Server, round: u32) {
    server.ok("handoff_load_context", json!({}));
    server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "t1", "title": "Lock me", "status": "in_progress",
                          "schedule": { "estimate_hours": 1.0 } } }),
    );
    server.ok(
        "handoff_doc_save",
        json!({
            "slug": format!("guide-{round}"),
            "title": format!("Guide {round}"),
            "body": "# Guide\n\nborrow checker notes\n"
        }),
    );
    server.ok(
        "handoff_doc_query",
        json!({ "text": "borrow checker", "session_id": "sess-a" }),
    );
    server.ok(
        "handoff_memory_save",
        json!({ "text": "always use atomic_write for handoff files", "kind": "rule" }),
    );
    server.ok(
        "handoff_memory_query",
        json!({ "text": "always use atomic_write for handoff files", "session_id": "sess-a" }),
    );
    server.ok("handoff_timer_start", json!({ "task_id": "t1" }));
}

#[test]
fn init_writes_the_recommended_gitignore_and_leaves_the_project_repo_alone() {
    let project = TempDir::new().unwrap();
    git(project.path(), &["init", "-q"]);
    let mut server = Server::spawn(project.path());
    server.ok(
        "handoff_init",
        json!({ "project_name": "gi", "description": "d" }),
    );

    let content = String::from_utf8(gitignore_bytes(project.path())).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines, RECOMMENDED_GITIGNORE_ENTRIES);
    assert!(
        !project.path().join(".gitignore").exists(),
        "handoff_init must not create the project's own .gitignore"
    );
}

#[test]
fn local_state_never_appears_in_git_status_of_a_repo_tracking_handoff() {
    let project = TempDir::new().unwrap();
    git(project.path(), &["init", "-q"]);
    let mut server = Server::spawn(project.path());
    server.ok(
        "handoff_init",
        json!({ "project_name": "gi", "description": "d" }),
    );
    git(project.path(), &["add", "-A"]);
    git(project.path(), &["commit", "-q", "-m", "init"]);

    exercise_local_state_paths(&mut server, 1);

    // The files really were written ...
    let handoff = project.path().join(".handoff");
    assert!(handoff.join("agents").is_dir(), "agents/ was not written");
    assert!(handoff.join("timer").is_dir(), "timer/ was not written");
    assert!(handoff.join("docs/injected").is_dir());
    assert!(handoff.join("memory/injected").is_dir());
    // ... and none of them is visible to git.
    let status = status(project.path());
    assert!(
        local_state_entries(&status).is_empty(),
        "local state leaked into git status: {status:#?}"
    );
}

#[test]
fn existing_project_without_recommended_entries_gets_them_appended_once() {
    let project = TempDir::new().unwrap();
    git(project.path(), &["init", "-q"]);
    let mut server = Server::spawn(project.path());
    server.ok(
        "handoff_init",
        json!({ "project_name": "gi", "description": "d" }),
    );
    // A project created by an older version: only the user's own rule.
    std::fs::write(project.path().join(".handoff/.gitignore"), "my-notes/\n").unwrap();
    git(project.path(), &["add", "-A"]);
    git(project.path(), &["commit", "-q", "-m", "legacy"]);
    let project_gitignore_before = std::fs::read(project.path().join(".gitignore")).ok();

    exercise_local_state_paths(&mut server, 1);

    let content = String::from_utf8(gitignore_bytes(project.path())).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(
        lines[0], "my-notes/",
        "user line must be kept first: {content}"
    );
    for entry in [
        "*.lock",
        "/agents/",
        "/timer/",
        "/docs/injected/",
        "/memory/injected/",
    ] {
        assert_eq!(
            lines.iter().filter(|l| **l == entry).count(),
            1,
            "{entry} must be appended exactly once: {content}"
        );
    }
    let status = status(project.path());
    assert!(
        local_state_entries(&status).is_empty(),
        "local state leaked into git status: {status:#?}"
    );

    // Repeating the same operations changes nothing.
    let settled = gitignore_bytes(project.path());
    exercise_local_state_paths(&mut server, 2);
    assert_eq!(gitignore_bytes(project.path()), settled);
    assert_eq!(
        std::fs::read(project.path().join(".gitignore")).ok(),
        project_gitignore_before,
        "the project's own .gitignore must never be touched"
    );
}
