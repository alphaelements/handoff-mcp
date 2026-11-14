//! Real-binary E2E for the readiness diagnosis: the `handoff_doctor` MCP tool
//! (over stdio JSON-RPC) and `handoff-mcp setup --check [--run] [--json]`
//! (SPEC-ONB-002, SPEC-ONB-092, SPEC-ONB-093).
//!
//! Unix only: the commands under test are shell command lines.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const FILLED_CLAUDE_MD: &str = "# Demo\n\n## Session Handoff\n\nx\n\n## Build & Test\n\n- test: `true`\n\n## Coding Rules\n\nr\n\n## Project Structure\n\ns\n\n## Documentation Contract\n\nd\n";

const COMMAND_CHECK_IDS: [&str; 5] = [
    "cmd_install",
    "cmd_build",
    "cmd_test",
    "cmd_lint",
    "cmd_typecheck",
];

/// A temporary project with its own `HOME`.
struct Sandbox {
    root: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
}

struct CliOutput {
    stdout: String,
    stderr: String,
    code: i32,
}

impl Sandbox {
    /// An empty directory: nothing initialized, nothing registered.
    fn empty() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let home = root.path().join("home");
        let project = root.path().join("project");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        Self {
            root,
            home,
            project,
        }
    }

    /// A project in which every file-based check passes. `config_tail` is
    /// appended to the config (`[commands]`, `[onboarding]`, ...).
    fn ready(config_tail: &str) -> Self {
        let sandbox = Self::empty();
        sandbox.write_config(config_tail);
        sandbox.write("CLAUDE.md", FILLED_CLAUDE_MD);
        sandbox.write(
            ".mcp.json",
            r#"{"mcpServers":{"handoff":{"type":"stdio","command":"handoff-mcp"}}}"#,
        );
        sandbox
    }

    fn write(&self, name: &str, content: &str) {
        let path = self.project.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn write_config(&self, tail: &str) {
        self.write(
            ".handoff/config.toml",
            &format!("[project]\nname = \"demo\"\n\n{tail}"),
        );
    }

    fn command(&self) -> Command {
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

    fn cli(&self, args: &[&str]) -> CliOutput {
        let output = self
            .command()
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("failed to run handoff-mcp");
        CliOutput {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            code: output.status.code().unwrap_or(-1),
        }
    }

    /// `handoff-mcp setup --check --json <extra>`, parsed.
    fn check_json(&self, extra: &[&str]) -> (Value, i32) {
        let mut args = vec!["setup", "--check", "--json"];
        args.extend_from_slice(extra);
        let output = self.cli(&args);
        let report = serde_json::from_str(&output.stdout).unwrap_or_else(|e| {
            panic!(
                "setup {args:?} did not print JSON ({e})\nstdout: {}\nstderr: {}",
                output.stdout, output.stderr
            )
        });
        (report, output.code)
    }

    fn server(&self) -> Server {
        Server::start(self.command(), &self.project)
    }
}

/// The real server, driven over stdio.
struct Server {
    child: Child,
    stdin: ChildStdin,
    responses: Receiver<String>,
    next_id: u64,
    project: PathBuf,
}

impl Server {
    fn start(mut command: Command, project: &Path) -> Self {
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

    fn request(&mut self, method: &str, params: Value) -> Value {
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

    /// Calls `handoff_doctor`; `Ok(report)` or `Err(error text)`.
    fn doctor(&mut self, mut arguments: Value) -> Result<Value, String> {
        arguments["project_dir"] = json!(self.project.to_string_lossy());
        let response = self.request(
            "tools/call",
            json!({ "name": "handoff_doctor", "arguments": arguments }),
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
        Ok(serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("handoff_doctor did not answer JSON ({e}): {text}")))
    }

    fn diagnose(&mut self, arguments: Value) -> Value {
        self.doctor(arguments)
            .unwrap_or_else(|e| panic!("handoff_doctor failed: {e}"))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn check<'a>(report: &'a Value, id: &str) -> &'a Value {
    report["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|c| c["id"] == json!(id))
        .unwrap_or_else(|| panic!("no check `{id}` in {report}"))
}

fn status<'a>(report: &'a Value, id: &str) -> &'a str {
    check(report, id)["status"].as_str().expect("status")
}

fn ids(list: &Value) -> Vec<&str> {
    list.as_array()
        .expect("array")
        .iter()
        .map(|item| item["id"].as_str().expect("id"))
        .collect()
}

/// Every file under `dir` with its content.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
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

// ===========================================================================
// handoff_doctor over JSON-RPC
// ===========================================================================

#[test]
fn the_tool_is_listed_with_its_arguments() {
    let sandbox = Sandbox::empty();
    let mut server = sandbox.server();
    let response = server.request("tools/list", json!({}));
    let tool = response["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|tool| tool["name"] == json!("handoff_doctor"))
        .expect("handoff_doctor is listed");
    let properties = &tool["inputSchema"]["properties"];
    assert!(properties["project_dir"].is_object());
    assert_eq!(properties["checks"]["type"], json!("array"));
    assert_eq!(
        properties["checks"]["items"]["enum"],
        json!(handoff_mcp::setup::doctor::CHECK_IDS)
    );
    let result = &properties["results"]["items"];
    assert_eq!(result["required"], json!(["id", "command"]));
    for field in [
        "id",
        "command",
        "exit_code",
        "duration_ms",
        "output_tail",
        "error",
    ] {
        assert!(result["properties"][field].is_object(), "{field}");
    }
    assert_eq!(result["properties"]["id"]["enum"], json!(COMMAND_CHECK_IDS));
    let description = tool["description"].as_str().unwrap();
    assert!(
        description.contains("commands_to_run") && description.contains("never"),
        "the description says who runs the commands: {description}"
    );
}

// SPEC-ONB-002 AC1
#[test]
fn ac1_every_check_has_id_status_detail_and_fails_have_a_next_step() {
    let sandbox = Sandbox::empty();
    let mut server = sandbox.server();
    let report = server.diagnose(json!({}));

    assert_eq!(
        ids(&report["checks"]),
        handoff_mcp::setup::doctor::CHECK_IDS
    );
    let mut failed = 0;
    for item in report["checks"].as_array().unwrap() {
        assert!(item["id"].is_string(), "{item}");
        let status = item["status"].as_str().expect("status");
        assert!(
            ["pass", "fail", "skip", "manual"].contains(&status),
            "{item}"
        );
        assert!(!item["detail"].as_str().unwrap_or("").is_empty(), "{item}");
        if status == "fail" {
            failed += 1;
            assert!(
                !item["next_step"].as_str().unwrap_or("").is_empty(),
                "a failed check must carry next_step: {item}"
            );
        }
    }
    assert!(failed >= 2, "an empty directory is not ready: {report}");
    assert_eq!(report["ready"], json!(false));
    assert_eq!(status(&report, "handoff_initialized"), "fail");
    assert!(report["commands_to_run"].as_array().unwrap().is_empty());
    assert!(report["warnings"].is_array());
    assert!(
        !sandbox.project.join(".handoff").exists(),
        "the diagnosis must not initialize the project"
    );
}

// SPEC-ONB-002 AC2
#[test]
fn ac2_a_todo_marker_in_claude_md_fails_and_makes_the_project_not_ready() {
    let sandbox = Sandbox::ready("[commands]\ntest = \"true\"\n");
    let mut server = sandbox.server();
    let before = server.diagnose(json!({}));
    assert_eq!(status(&before, "claude_md_todo"), "pass");
    assert_eq!(before["ready"], json!(true), "{before}");

    sandbox.write(
        "CLAUDE.md",
        &format!("{FILLED_CLAUDE_MD}\n- smoke: TODO(handoff)\n"),
    );
    let report = server.diagnose(json!({}));
    assert_eq!(status(&report, "claude_md_todo"), "fail");
    assert_eq!(report["ready"], json!(false));
}

// SPEC-ONB-002 AC3
#[test]
fn ac3_a_reported_exit_code_of_one_fails_cmd_test_as_reported() {
    let sandbox = Sandbox::ready("[commands]\ntest = \"cargo test --workspace\"\n");
    let mut server = sandbox.server();
    let report = server.diagnose(json!({
        "results": [{ "id": "cmd_test", "command": "cargo test --workspace", "exit_code": 1 }]
    }));
    let cmd_test = check(&report, "cmd_test");
    assert_eq!(cmd_test["status"], json!("fail"));
    assert_eq!(cmd_test["exit_code"], json!(1));
    assert_eq!(cmd_test["source"], json!("reported"));
    assert_eq!(cmd_test["command"], json!("cargo test --workspace"));
    assert_eq!(report["ready"], json!(false));
}

// SPEC-ONB-002 AC4 / SPEC-ONB-092 AC1
#[test]
fn ac4_without_results_every_command_check_is_skipped_and_handed_back() {
    let sandbox = Sandbox::ready(
        "[commands]\ninstall = \"npm ci\"\nbuild = \"npm run build\"\ntest = \"npm run test\"\nformat = \"npm run format\"\nsmoke = \"./smoke.sh\"\naudit = \"npm audit\"\n",
    );
    let mut server = sandbox.server();
    let report = server.diagnose(json!({}));

    for id in COMMAND_CHECK_IDS {
        assert_eq!(status(&report, id), "skip", "{id}");
        assert!(check(&report, id).get("source").is_none(), "{id}");
    }
    assert!(
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["id"].as_str().unwrap().starts_with("cmd_"))
            .all(|c| c["status"] != json!("pass")),
        "no command check may pass without a result"
    );
    assert_eq!(
        report["commands_to_run"],
        json!([
            { "id": "cmd_install", "command": "npm ci", "timeout_sec": 600 },
            { "id": "cmd_build", "command": "npm run build", "timeout_sec": 600 },
            { "id": "cmd_test", "command": "npm run test", "timeout_sec": 600 },
        ]),
        "defined commands only; format, smoke and audit are not run by the diagnosis"
    );
    assert_eq!(report["ready"], json!(true), "a skip does not lower ready");
}

// SPEC-ONB-002 AC5
#[test]
fn ac5_a_result_for_another_command_fails_as_a_mismatch() {
    let sandbox = Sandbox::ready("[commands]\ntest = \"cargo test\"\n");
    let mut server = sandbox.server();
    let report = server.diagnose(json!({
        "results": [{ "id": "cmd_test", "command": "cargo test --lib", "exit_code": 0 }]
    }));
    let cmd_test = check(&report, "cmd_test");
    assert_eq!(cmd_test["status"], json!("fail"));
    assert!(
        cmd_test["detail"]
            .as_str()
            .unwrap()
            .contains("does not match"),
        "{cmd_test}"
    );
    assert!(cmd_test.get("exit_code").is_none(), "{cmd_test}");
    assert_eq!(report["ready"], json!(false));
}

/// done_criteria: success, failure, cannot start, timeout and undefined, each
/// through the real server.
#[test]
fn jsonrpc_reports_success_failure_start_failure_timeout_and_undefined() {
    let sandbox = Sandbox::ready(
        "[commands]\ninstall = \"npm ci\"\nbuild = \"npm run build\"\ntest = \"npm run test\"\nlint = \"npm run lint\"\n",
    );
    let mut server = sandbox.server();
    let report = server.diagnose(json!({
        "results": [
            { "id": "cmd_install", "command": "npm ci", "exit_code": 0, "duration_ms": 4200 },
            { "id": "cmd_build", "command": "npm run build", "exit_code": 2,
              "output_tail": "src/a.ts(3,1): error TS2304\nFound 1 error." },
            { "id": "cmd_test", "command": "npm run test",
              "error": "could not start: No such file or directory (os error 2)" },
            { "id": "cmd_lint", "command": "npm run lint", "duration_ms": 600000,
              "error": "timed out after 600s" },
        ]
    }));

    // success
    let install = check(&report, "cmd_install");
    assert_eq!(install["status"], json!("pass"));
    assert_eq!(install["exit_code"], json!(0));
    assert_eq!(install["duration_ms"], json!(4200));
    assert_eq!(install["source"], json!("reported"));
    // failure
    let build = check(&report, "cmd_build");
    assert_eq!(build["status"], json!("fail"));
    assert_eq!(build["exit_code"], json!(2));
    assert!(build["detail"].as_str().unwrap().contains("Found 1 error."));
    assert!(!build["next_step"].as_str().unwrap().is_empty());
    // cannot start (SPEC-ONB-092 AC2)
    let test = check(&report, "cmd_test");
    assert_eq!(test["status"], json!("fail"));
    assert!(test["detail"].as_str().unwrap().contains("could not start"));
    assert!(test.get("exit_code").is_none(), "{test}");
    assert_eq!(test["source"], json!("reported"));
    // timeout
    let lint = check(&report, "cmd_lint");
    assert_eq!(lint["status"], json!("fail"));
    assert!(lint["detail"]
        .as_str()
        .unwrap()
        .contains("timed out after 600s"));
    assert_eq!(lint["duration_ms"], json!(600000));
    // undefined
    let typecheck = check(&report, "cmd_typecheck");
    assert_eq!(typecheck["status"], json!("skip"));
    assert!(typecheck["detail"]
        .as_str()
        .unwrap()
        .contains("not defined"));
    assert!(typecheck.get("command").is_none(), "{typecheck}");

    assert_eq!(report["ready"], json!(false));
    assert!(report["commands_to_run"].as_array().unwrap().is_empty());
}

#[test]
fn reporting_results_without_a_test_command_is_not_ready() {
    let sandbox = Sandbox::ready("[commands]\nbuild = \"make\"\n");
    let mut server = sandbox.server();
    assert_eq!(server.diagnose(json!({}))["ready"], json!(true));

    let report = server.diagnose(json!({
        "results": [{ "id": "cmd_build", "command": "make", "exit_code": 0 }]
    }));
    assert_eq!(status(&report, "cmd_build"), "pass");
    assert_eq!(status(&report, "cmd_test"), "skip");
    assert_eq!(status(&report, "commands_defined"), "fail");
    assert_eq!(report["ready"], json!(false));
}

/// With `commands_defined` left out by `checks`, `warnings` is where the
/// missing test command is named, whether or not a reported command failed.
#[test]
fn a_filtered_report_without_a_test_command_says_why_in_warnings() {
    let sandbox = Sandbox::ready("[commands]\nbuild = \"make\"\n");
    let mut server = sandbox.server();
    for (exit_code, build_status) in [(0, "pass"), (2, "fail")] {
        let report = server.diagnose(json!({
            "checks": ["cmd_build"],
            "results": [{ "id": "cmd_build", "command": "make", "exit_code": exit_code }]
        }));
        assert_eq!(ids(&report["checks"]), vec!["cmd_build"]);
        assert_eq!(status(&report, "cmd_build"), build_status);
        assert_eq!(report["ready"], json!(false));
        let warnings = report["warnings"].as_array().expect("warnings");
        assert_eq!(warnings.len(), 1, "{report}");
        assert!(
            warnings[0].as_str().unwrap().contains("[commands] test"),
            "{report}"
        );
    }

    // Nothing to warn about without a run, or when the check itself says it.
    let no_run = server.diagnose(json!({ "checks": ["cmd_build"] }));
    assert_eq!(no_run["warnings"], json!([]));
    let unfiltered = server.diagnose(json!({
        "results": [{ "id": "cmd_build", "command": "make", "exit_code": 0 }]
    }));
    assert_eq!(status(&unfiltered, "commands_defined"), "fail");
    assert_eq!(unfiltered["warnings"], json!([]));
}

#[test]
fn smoke_is_never_a_pass_when_undefined() {
    let sandbox = Sandbox::ready("[commands]\ntest = \"true\"\n");
    let mut server = sandbox.server();
    let report = server.diagnose(json!({}));
    assert_eq!(status(&report, "smoke_defined"), "skip");
    assert!(check(&report, "smoke_defined")["detail"]
        .as_str()
        .unwrap()
        .contains("not defined"));
}

#[test]
fn the_checks_argument_limits_the_report() {
    let sandbox = Sandbox::ready("[commands]\ntest = \"true\"\nbuild = \"make\"\n");
    let mut server = sandbox.server();
    let report = server.diagnose(json!({ "checks": ["cmd_test", "claude_md_todo"] }));
    assert_eq!(ids(&report["checks"]), vec!["claude_md_todo", "cmd_test"]);
    assert_eq!(ids(&report["commands_to_run"]), vec!["cmd_test"]);
}

#[test]
fn requests_that_cannot_be_honored_are_errors() {
    let sandbox = Sandbox::ready("[commands]\ntest = \"true\"\n");
    let mut server = sandbox.server();
    for (arguments, expected) in [
        (json!({ "checks": ["cmd_tset"] }), "cmd_tset"),
        (json!({ "checks": "cmd_test" }), "checks"),
        (json!({ "checks": [] }), "checks"),
        (json!({ "results": { "id": "cmd_test" } }), "results"),
        (
            json!({ "results": [{ "id": "cmd_test", "command": "true", "exitCode": 0 }] }),
            "exitCode",
        ),
        (
            json!({ "results": [{ "id": "cmd_test", "command": "true" }] }),
            "exit_code",
        ),
        (
            json!({ "results": [{ "id": "cmd_format", "command": "x", "exit_code": 0 }] }),
            "cmd_format",
        ),
        (
            json!({ "results": [{ "id": "cmd_test", "exit_code": 0 }] }),
            "command",
        ),
        (
            json!({ "results": [{ "id": "cmd_test", "command": "true", "exit_code": "0" }] }),
            "results",
        ),
        // An `error` that gives no reason, next to a successful exit code.
        (
            json!({ "results": [{ "id": "cmd_test", "command": "true", "exit_code": 0, "error": "" }] }),
            "`error` is empty",
        ),
        // Misspelled arguments: dropping them would answer `ready: true` for
        // results that were never judged.
        (
            json!({ "result": [{ "id": "cmd_test", "command": "true", "exit_code": 1 }] }),
            "unknown argument(s): result.",
        ),
        (
            json!({ "check": ["cmd_test"] }),
            "unknown argument(s): check.",
        ),
    ] {
        let err = server
            .doctor(arguments.clone())
            .expect_err("the request must be rejected");
        assert!(err.contains(expected), "{arguments}: {err}");
    }
}

#[test]
fn the_tool_answers_whatever_the_onboarding_settings_are() {
    for tail in [
        "[commands]\ntest = \"true\"\n",
        "[commands]\ntest = \"true\"\n\n[onboarding]\npreflight = \"off\"\n",
        "[commands]\ntest = \"true\"\n\n[onboarding]\npreflight = \"run\"\ncommand_timeout_sec = 30\n",
    ] {
        let sandbox = Sandbox::ready(tail);
        let mut server = sandbox.server();
        let report = server.diagnose(json!({}));
        assert_eq!(report["ready"], json!(true), "{tail}");
        let expected_timeout = if tail.contains("command_timeout_sec") {
            30
        } else {
            600
        };
        assert_eq!(
            report["commands_to_run"][0]["timeout_sec"],
            json!(expected_timeout),
            "{tail}"
        );
    }
}

#[test]
fn the_tool_writes_nothing() {
    let sandbox = Sandbox::ready("[commands]\ntest = \"touch ran-test\"\n");
    let before = snapshot(sandbox.root.path());
    let mut server = sandbox.server();
    server.diagnose(json!({}));
    server.diagnose(json!({
        "results": [{ "id": "cmd_test", "command": "touch ran-test", "exit_code": 0 }]
    }));
    drop(server);
    assert_eq!(snapshot(sandbox.root.path()), before);
}

// SPEC-ONB-002 AC6
/// The server is started with a `PATH` that holds nothing but tripwires: any
/// program it tried to start (a shell, `git`, the configured command itself)
/// would be one of them and would leave a line in the log.
#[test]
fn ac6_the_tool_starts_no_process() {
    let sandbox = Sandbox::ready("");
    let tripwires = sandbox.root.path().join("tripwires");
    let log = sandbox.root.path().join("spawned.log");
    std::fs::create_dir_all(&tripwires).unwrap();
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
        let path = tripwires.join(name);
        std::fs::write(
            &path,
            format!("#!/bin/sh\necho \"{name} $*\" >> '{}'\n", log.display()),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let ran = sandbox.project.join("command-ran");
    sandbox.write_config(&format!(
        "[commands]\ninstall = \"handoff-tripwire install\"\nbuild = \"touch {ran}\"\ntest = \"touch {ran}\"\nlint = \"touch {ran}\"\ntypecheck = \"touch {ran}\"\nsmoke = \"touch {ran}\"\naudit = \"touch {ran}\"\n",
        ran = ran.display()
    ));
    let configured = format!("touch {}", ran.display());

    // A second project that is not initialized and looks like a linked git
    // worktree: resolving its `.handoff/` the usual way would ask `git`.
    let bare = sandbox.root.path().join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    std::fs::write(
        bare.join(".git"),
        "gitdir: /nonexistent/.git/worktrees/bare\n",
    )
    .unwrap();

    let mut command = sandbox.command();
    command.env("PATH", &tripwires);
    let mut server = Server::start(command, &sandbox.project);
    // Anything the server starts on its own before the first tool call is
    // not what this test is about.
    server.request("tools/list", json!({}));
    let spawned_before = std::fs::read_to_string(&log).unwrap_or_default();

    let without = server.diagnose(json!({}));
    assert_eq!(ids(&without["commands_to_run"]).len(), 5, "{without}");
    let with = server.diagnose(json!({
        "results": [
            { "id": "cmd_install", "command": "handoff-tripwire install", "exit_code": 0 },
            { "id": "cmd_build", "command": configured, "exit_code": 0 },
            { "id": "cmd_test", "command": configured, "exit_code": 1 },
            { "id": "cmd_lint", "command": "something else", "exit_code": 0 },
            { "id": "cmd_typecheck", "command": configured, "error": "timed out after 600s" },
        ]
    }));
    assert_eq!(status(&with, "cmd_build"), "pass");
    assert_eq!(status(&with, "cmd_test"), "fail");
    server.diagnose(json!({ "checks": ["cmd_test"] }));
    let _ = server.doctor(json!({ "checks": ["nope"] }));
    server.project = bare.clone();
    let uninitialized = server.diagnose(json!({}));
    assert_eq!(status(&uninitialized, "handoff_initialized"), "fail");

    let spawned_after = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(
        spawned_after, spawned_before,
        "handoff_doctor started a process"
    );
    assert!(!ran.exists(), "a configured command was executed");
    assert!(!bare.join(".handoff").exists());

    // The tripwire itself works: the CLI, which does run commands, trips it.
    let mut cli = sandbox.command();
    cli.env("PATH", &tripwires)
        .args(["setup", "--check", "--run", "--json"])
        .stdin(Stdio::null());
    let _ = cli.output().expect("cli");
    assert!(
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .contains("sh "),
        "the tripwire must catch a real spawn, or this test proves nothing"
    );
}

// ===========================================================================
// handoff-mcp setup --check
// ===========================================================================

/// done_criteria: `setup --check --json` and `handoff_doctor` answer the same.
#[test]
fn check_json_equals_the_tool_response() {
    // Registered for the user (in the home directory), not in the project.
    let registered_in_home = Sandbox::ready("[commands]\ntest = \"true\"\n");
    std::fs::remove_file(registered_in_home.project.join(".mcp.json")).unwrap();
    std::fs::write(
        registered_in_home.home.join(".claude.json"),
        r#"{"mcpServers":{"handoff":{"type":"stdio","command":"handoff-mcp"}}}"#,
    )
    .unwrap();
    assert_eq!(
        status(&registered_in_home.check_json(&[]).0, "mcp_registered"),
        "pass"
    );

    for sandbox in [
        Sandbox::empty(),
        Sandbox::ready("[commands]\ntest = \"true\"\nlint = \"false\"\nsmoke = \"./s.sh\"\n"),
        Sandbox::ready(""),
        registered_in_home,
    ] {
        let (from_cli, code) = sandbox.check_json(&[]);
        let from_tool = sandbox.server().diagnose(json!({}));
        assert_eq!(from_cli, from_tool);
        let ready = from_cli["ready"].as_bool().expect("ready");
        assert_eq!(code, if ready { 0 } else { 1 }, "{from_cli}");
    }
}

// SPEC-ONB-002 AC7
#[test]
fn ac7_check_run_json_equals_the_tool_given_the_same_results() {
    let sandbox = Sandbox::ready(
        "[commands]\ninstall = \"true\"\nbuild = \"test -d .handoff\"\ntest = \"exit 3\"\nlint = \"test -f no-such-file\"\nformat = \"touch formatted\"\nsmoke = \"touch smoked\"\naudit = \"touch audited\"\n",
    );

    let (from_cli, code) = sandbox.check_json(&["--run"]);

    assert_eq!(code, 1, "{from_cli}");
    assert_eq!(status(&from_cli, "cmd_install"), "pass");
    assert_eq!(status(&from_cli, "cmd_build"), "pass");
    assert_eq!(status(&from_cli, "cmd_test"), "fail");
    assert_eq!(check(&from_cli, "cmd_test")["exit_code"], json!(3));
    assert_eq!(status(&from_cli, "cmd_lint"), "fail");
    assert_eq!(status(&from_cli, "cmd_typecheck"), "skip");
    assert!(from_cli["commands_to_run"].as_array().unwrap().is_empty());
    for name in ["formatted", "smoked", "audited"] {
        assert!(
            !sandbox.project.join(name).exists(),
            "`{name}`: only install/build/test/lint/typecheck are run"
        );
    }

    // Hand the same results to the tool.
    let results: Vec<Value> = from_cli["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c.get("source").is_some())
        .map(|c| {
            assert_eq!(c["source"], json!("executed"), "{c}");
            json!({
                "id": c["id"], "command": c["command"],
                "exit_code": c["exit_code"], "duration_ms": c["duration_ms"],
            })
        })
        .collect();
    assert_eq!(results.len(), 4);
    let from_tool = sandbox.server().diagnose(json!({ "results": results }));

    // SPEC-ONB-092 AC4: only the source differs.
    let mut expected = from_cli.clone();
    for item in expected["checks"].as_array_mut().unwrap() {
        if item.get("source").is_some() {
            item["source"] = json!("reported");
        }
    }
    assert_eq!(from_tool, expected);
    assert_ne!(from_tool, from_cli, "the sources must differ");
}

// SPEC-ONB-002 AC8 / SPEC-ONB-092 AC2
#[test]
fn ac8_run_fails_a_missing_command_and_a_command_that_exceeds_the_timeout() {
    let sandbox = Sandbox::ready(
        "[commands]\nbuild = \"handoff-no-such-command-xyz --all\"\ntest = \"sleep 30\"\n\n[onboarding]\ncommand_timeout_sec = 1\n",
    );
    let started = Instant::now();

    let (report, code) = sandbox.check_json(&["--run"]);

    assert!(
        started.elapsed() < Duration::from_secs(15),
        "the timeout was not enforced: {:?}",
        started.elapsed()
    );
    assert_eq!(code, 1);
    let build = check(&report, "cmd_build");
    assert_eq!(build["status"], json!("fail"));
    assert_eq!(build["exit_code"], json!(127));
    assert_eq!(build["source"], json!("executed"));
    let detail = build["detail"].as_str().unwrap();
    assert!(detail.contains("command not found"), "{detail}");
    assert!(detail.contains("handoff-no-such-command-xyz"), "{detail}");

    let test = check(&report, "cmd_test");
    assert_eq!(test["status"], json!("fail"));
    assert_eq!(test["source"], json!("executed"));
    assert!(
        test["detail"]
            .as_str()
            .unwrap()
            .contains("timed out after 1s"),
        "{test}"
    );
    assert!(test.get("exit_code").is_none(), "{test}");
    assert!(test["duration_ms"].as_u64().unwrap() >= 1000, "{test}");
    assert!(!test["next_step"].as_str().unwrap().is_empty());
}

// SPEC-ONB-092 AC2
/// With nothing on `PATH` the CLI cannot even start the shell. That is a
/// failure with its reason, never a skip and never an exit code.
#[test]
fn spec092_ac2_run_fails_a_command_that_cannot_be_started() {
    let sandbox = Sandbox::ready("[commands]\nbuild = \"true\"\ntest = \"true\"\n");
    let nothing = sandbox.root.path().join("empty-path");
    std::fs::create_dir_all(&nothing).unwrap();

    let output = sandbox
        .command()
        .env("PATH", &nothing)
        .args(["setup", "--check", "--run", "--json"])
        .stdin(Stdio::null())
        .output()
        .expect("failed to run handoff-mcp");

    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).expect("JSON");
    assert_eq!(report["ready"], json!(false));
    for id in ["cmd_build", "cmd_test"] {
        let item = check(&report, id);
        assert_eq!(item["status"], json!("fail"), "{item}");
        assert_eq!(item["source"], json!("executed"), "{item}");
        assert!(item.get("exit_code").is_none(), "{item}");
        assert!(
            item["detail"].as_str().unwrap().contains("could not start"),
            "{item}"
        );
        assert!(!item["next_step"].as_str().unwrap().is_empty(), "{item}");
    }
    assert!(report["commands_to_run"].as_array().unwrap().is_empty());
}

#[test]
fn check_without_run_executes_nothing() {
    let sandbox = Sandbox::ready("[commands]\ntest = \"touch ran\"\nbuild = \"touch ran\"\n");
    let (report, code) = sandbox.check_json(&[]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(status(&report, "cmd_test"), "skip");
    let text = sandbox.cli(&["setup", "--check"]);
    assert_eq!(text.code, 0, "{}", text.stdout);
    assert!(!sandbox.project.join("ran").exists());
}

#[test]
fn run_passes_a_project_whose_commands_succeed() {
    let sandbox = Sandbox::ready("[commands]\ntest = \"test -f CLAUDE.md\"\n");
    let (report, code) = sandbox.check_json(&["--run"]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["ready"], json!(true));
    assert_eq!(status(&report, "cmd_test"), "pass");
    assert_eq!(check(&report, "cmd_test")["source"], json!("executed"));
}

#[test]
fn run_without_a_test_command_is_not_ready() {
    let sandbox = Sandbox::ready("[commands]\nbuild = \"true\"\n");
    assert_eq!(sandbox.check_json(&[]).1, 0);
    let (report, code) = sandbox.check_json(&["--run"]);
    assert_eq!(code, 1, "{report}");
    assert_eq!(status(&report, "cmd_build"), "pass");
    assert_eq!(status(&report, "commands_defined"), "fail");
}

/// `--run` asks for a judgment by running the commands. With no test command
/// that judgment cannot be "ready", even when there was nothing to run.
#[test]
fn run_with_nothing_runnable_and_no_test_command_is_not_ready() {
    let sandbox =
        Sandbox::ready("[commands]\nsmoke = \"touch smoked\"\nformat = \"touch formatted\"\n");
    let (plain, code) = sandbox.check_json(&[]);
    assert_eq!(code, 0, "{plain}");
    assert!(plain["commands_to_run"].as_array().unwrap().is_empty());

    let (report, code) = sandbox.check_json(&["--run"]);
    assert_eq!(code, 1, "{report}");
    assert_eq!(report["ready"], json!(false));
    assert_eq!(status(&report, "commands_defined"), "fail");
    assert!(!sandbox.project.join("smoked").exists());
    assert!(!sandbox.project.join("formatted").exists());
}

// SPEC-ONB-093 AC2
#[test]
fn spec093_ac2_a_hand_written_make_command_decides_cmd_test() {
    let sandbox = Sandbox::ready("[commands]\ntest = \"make test\"\n");
    sandbox.write("Makefile", "test:\n\t@test -f ok-marker\n");

    let (failing, code) = sandbox.check_json(&["--run"]);
    assert_eq!(status(&failing, "cmd_test"), "fail", "{failing}");
    assert_ne!(check(&failing, "cmd_test")["exit_code"], json!(0));
    assert_eq!(code, 1);

    sandbox.write("ok-marker", "");
    let (passing, code) = sandbox.check_json(&["--run"]);
    assert_eq!(status(&passing, "cmd_test"), "pass", "{passing}");
    assert_eq!(check(&passing, "cmd_test")["exit_code"], json!(0));
    assert_eq!(check(&passing, "cmd_test")["command"], json!("make test"));
    assert_eq!(code, 0);
}

// SPEC-ONB-093 AC3
#[test]
fn spec093_ac3_run_goes_through_the_shell() {
    let sandbox = Sandbox::ready(
        "[commands]\ntest = \"printf 'a\\\\nb\\\\n' | grep -c b > count.txt && test \\\"$(cat count.txt)\\\" = 1\"\nbuild = \"cd .handoff && test -f config.toml\"\n",
    );
    let (report, code) = sandbox.check_json(&["--run"]);
    assert_eq!(status(&report, "cmd_test"), "pass", "{report}");
    assert_eq!(status(&report, "cmd_build"), "pass", "{report}");
    assert_eq!(code, 0);
    assert_eq!(
        std::fs::read_to_string(sandbox.project.join("count.txt")).unwrap(),
        "1\n",
        "the command ran in the project directory"
    );
}

#[test]
fn text_mode_prints_the_diagnosis_and_sets_the_exit_code() {
    let sandbox = Sandbox::ready("[commands]\ntest = \"exit 4\"\n");

    let plain = sandbox.cli(&["setup", "--check"]);
    assert_eq!(plain.code, 0, "{}", plain.stdout);
    assert!(plain.stdout.contains("Settings file:"), "{}", plain.stdout);
    assert!(plain.stdout.contains("[skip] cmd_test"), "{}", plain.stdout);
    assert!(plain.stdout.contains("Ready: yes"), "{}", plain.stdout);

    let run = sandbox.cli(&["setup", "--check", "--run"]);
    assert_eq!(run.code, 1, "{}", run.stdout);
    assert!(run.stdout.contains("[fail] cmd_test"), "{}", run.stdout);
    assert!(run.stdout.contains("exit code 4"), "{}", run.stdout);
    assert!(run.stdout.contains("Ready: no"), "{}", run.stdout);
    assert!(
        run.stderr.contains("exit 4"),
        "the command being run is announced on stderr: {}",
        run.stderr
    );
}

#[test]
fn an_unprepared_directory_is_reported_not_ready_instead_of_failing() {
    let sandbox = Sandbox::empty();
    let (report, code) = sandbox.check_json(&["--run"]);
    assert_eq!(code, 1);
    assert_eq!(report["ready"], json!(false));
    assert_eq!(status(&report, "handoff_initialized"), "fail");
    assert!(!sandbox.project.join(".handoff").exists());
    assert!(!sandbox.home.join(".claude").exists());
}

#[test]
fn json_mode_does_not_need_a_home_directory() {
    let sandbox = Sandbox::ready("[commands]\ntest = \"true\"\n");
    let output = sandbox
        .command()
        .env_remove("HOME")
        .args(["setup", "--check", "--json"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let report: Value = serde_json::from_slice(&output.stdout).expect("JSON");
    assert_eq!(status(&report, "mcp_registered"), "pass");
}

#[test]
fn run_and_json_need_check() {
    for args in [
        vec!["setup", "--run"],
        vec!["setup", "--json"],
        vec!["setup", "-y", "--run"],
        vec!["setup", "--uninstall", "--json"],
        vec!["setup", "--claude-md-scaffold", "--run"],
    ] {
        let sandbox = Sandbox::ready("[commands]\ntest = \"touch ran\"\n");
        let output = sandbox.cli(&args);
        assert_eq!(output.code, 1, "{args:?}: {}", output.stdout);
        assert!(
            output.stderr.contains("--check"),
            "{args:?}: {}",
            output.stderr
        );
        assert!(!sandbox.project.join("ran").exists(), "{args:?}");
        assert!(!sandbox.home.join(".claude").exists(), "{args:?}");
    }
}

#[test]
fn setup_help_lists_the_check_flags() {
    let sandbox = Sandbox::empty();
    let output = sandbox.cli(&["setup", "--help"]);
    assert!(output.stdout.contains("--check --run"), "{}", output.stdout);
    assert!(output.stdout.contains("--json"), "{}", output.stdout);
}
