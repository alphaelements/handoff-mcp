//! Shared harness of the onboarding E2E tests: a temporary project with its
//! own `HOME`, the real `handoff-mcp` binary as a CLI, and the real server
//! driven over stdio JSON-RPC.
//!
//! Included with `#[path = "support/onboarding_e2e.rs"] mod support;`. Not
//! every test file uses every helper.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::{json, Value};

/// A `CLAUDE.md` with every project section filled in.
pub const FILLED_CLAUDE_MD: &str = "# Demo\n\n## Session Handoff\n\nx\n\n## Build & Test\n\n- test: `true`\n\n## Coding Rules\n\nr\n\n## Project Structure\n\ns\n\n## Documentation Contract\n\nd\n";

/// A temporary project with its own `HOME`.
pub struct Sandbox {
    pub root: tempfile::TempDir,
    pub home: PathBuf,
    pub project: PathBuf,
}

pub struct CliOutput {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

impl CliOutput {
    /// The standard output parsed as JSON.
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|e| {
            panic!(
                "the command did not print JSON ({e})\nstdout: {}\nstderr: {}",
                self.stdout, self.stderr
            )
        })
    }
}

impl Sandbox {
    /// An empty directory: nothing initialized, nothing registered.
    pub fn empty() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        // Canonical, so paths the server reports compare equal to these.
        let base = std::fs::canonicalize(root.path()).expect("canonical tempdir");
        let home = base.join("home");
        let project = base.join("project");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        Self {
            root,
            home,
            project,
        }
    }

    /// A project initialized by the real `handoff-mcp init`, with
    /// `config_tail` appended to its config.
    pub fn initialized(config_tail: &str) -> Self {
        let sandbox = Self::empty();
        let init = sandbox.cli(&["init", "--project-name", "demo"]);
        assert_eq!(init.code, 0, "init failed: {}{}", init.stdout, init.stderr);
        sandbox.append_config(config_tail);
        sandbox
    }

    pub fn write(&self, name: &str, content: &str) {
        let path = self.project.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    pub fn handoff(&self) -> PathBuf {
        self.project.join(".handoff")
    }

    pub fn append_config(&self, tail: &str) {
        let path = self.handoff().join("config.toml");
        let mut text = std::fs::read_to_string(&path).expect("config.toml");
        text.push('\n');
        text.push_str(tail);
        std::fs::write(path, text).unwrap();
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_handoff-mcp"));
        command
            .current_dir(&self.project)
            .env("HOME", &self.home)
            .env_remove("USERPROFILE")
            // Set inside Claude Code; would redirect calls to another project.
            .env_remove("CLAUDE_PROJECT_DIR")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE");
        command
    }

    pub fn cli(&self, args: &[&str]) -> CliOutput {
        Self::output(self.command().args(args))
    }

    pub fn output(command: &mut Command) -> CliOutput {
        let output = command
            .stdin(Stdio::null())
            .output()
            .expect("failed to run handoff-mcp");
        CliOutput {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            code: output.status.code().unwrap_or(-1),
        }
    }

    pub fn server(&self) -> Server {
        Server::start(self.command(), &self.project)
    }

    /// Runs `git` in the project to build a fixture repository.
    pub fn git(&self, args: &[&str]) {
        git(&self.project, args);
    }

    /// A directory of executables that record their own invocation in a log.
    /// Started with this directory as its whole `PATH`, a process cannot
    /// start a shell, `git` or a configured command without leaving a line.
    pub fn tripwires(&self) -> Tripwires {
        let dir = self.root.path().join("tripwires");
        let log = self.root.path().join("spawned.log");
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "sh",
            "bash",
            "dash",
            "zsh",
            "cmd",
            "git",
            "env",
            "kill",
            "ps",
            "touch",
            "true",
            "false",
            "make",
            "npm",
            "node",
            "cargo",
            "handoff-tripwire",
        ] {
            let path = dir.join(name);
            std::fs::write(
                &path,
                format!("#!/bin/sh\necho \"{name} $*\" >> '{}'\n", log.display()),
            )
            .unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Tripwires { dir, log }
    }
}

pub struct Tripwires {
    pub dir: PathBuf,
    pub log: PathBuf,
}

impl Tripwires {
    /// What has been started so far, one line per process.
    pub fn spawned(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

/// Runs `git` in `dir` for a test fixture, isolated from the user's config.
pub fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .output()
        .expect("git must be installed to run these tests");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The real server, driven over stdio.
pub struct Server {
    child: Child,
    stdin: ChildStdin,
    responses: Receiver<String>,
    next_id: u64,
    pub project: PathBuf,
}

impl Server {
    pub fn start(mut command: Command, project: &Path) -> Self {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to start the handoff-mcp server");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let (tx, responses) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            responses,
            next_id: 0,
            project: project.to_path_buf(),
        }
    }

    pub fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let request = json!({
            "jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params,
        });
        writeln!(self.stdin, "{request}").expect("write request");
        self.stdin.flush().expect("flush");
        let line = self
            .responses
            .recv_timeout(Duration::from_secs(20))
            .unwrap_or_else(|_| panic!("no response to {method} within 20s"));
        let response: Value = serde_json::from_str(&line).expect("JSON-RPC response");
        assert_eq!(response["id"], self.next_id);
        response
    }

    /// The definition of `tool` from `tools/list`.
    pub fn tool_definition(&mut self, tool: &str) -> Value {
        let response = self.request("tools/list", json!({}));
        response["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .find(|definition| definition["name"] == json!(tool))
            .unwrap_or_else(|| panic!("{tool} is not listed"))
            .clone()
    }

    /// Calls `tool` on this server's project; `Ok(text)` or `Err(error text)`.
    pub fn call_text(&mut self, tool: &str, mut arguments: Value) -> Result<String, String> {
        arguments["project_dir"] = json!(self.project.to_string_lossy());
        let response = self.request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        );
        if let Some(error) = response.get("error").filter(|e| !e.is_null()) {
            return Err(error["message"].as_str().unwrap_or("").to_string());
        }
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string();
        if response["result"]["isError"].as_bool().unwrap_or(false) {
            return Err(text);
        }
        Ok(text)
    }

    /// Calls `tool` and parses its JSON answer; panics when the call fails.
    pub fn call(&mut self, tool: &str, arguments: Value) -> Value {
        let text = self
            .call_text(tool, arguments.clone())
            .unwrap_or_else(|e| panic!("{tool}({arguments}) failed: {e}"));
        serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("{tool} did not answer JSON ({e}): {text}"))
    }

    /// Calls `tool` for its side effect; panics when the call fails.
    pub fn ok(&mut self, tool: &str, arguments: Value) -> String {
        self.call_text(tool, arguments.clone())
            .unwrap_or_else(|e| panic!("{tool}({arguments}) failed: {e}"))
    }

    /// Calls `tool` expecting it to be rejected; returns the error text.
    pub fn err(&mut self, tool: &str, arguments: Value) -> String {
        match self.call_text(tool, arguments.clone()) {
            Err(error) => error,
            Ok(text) => panic!("{tool}({arguments}) must be rejected, answered: {text}"),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The entry with `id` in `list` (an array of `{id, ...}`).
pub fn entry<'a>(list: &'a Value, id: &str) -> &'a Value {
    list.as_array()
        .expect("array")
        .iter()
        .find(|item| item["id"] == json!(id))
        .unwrap_or_else(|| panic!("no entry `{id}` in {list}"))
}

/// The `status` of the entry with `id` in `list`.
pub fn status_of<'a>(list: &'a Value, id: &str) -> &'a str {
    entry(list, id)["status"].as_str().expect("status")
}

/// The ids of `list`, in order.
pub fn ids(list: &Value) -> Vec<&str> {
    list.as_array()
        .expect("array")
        .iter()
        .map(|item| item["id"].as_str().expect("id"))
        .collect()
}

/// Every file under `dir` with its content.
pub fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push((path.clone(), std::fs::read(&path).unwrap()));
            }
        }
    }
    files.sort();
    files
}
