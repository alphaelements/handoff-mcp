//! Real-binary E2E for the `[onboarding]` section and the `beginner` preset
//! (SPEC-ONB-010, SPEC-ONB-091).
//!
//! Every call goes to the built `handoff-mcp` binary: tools through its stdio
//! JSON-RPC transport, CLI commands through `Command`.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

fn binary_command(project: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_handoff-mcp"));
    command
        .current_dir(project)
        // Set inside Claude Code; would redirect calls to another project.
        .env_remove("CLAUDE_PROJECT_DIR")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    command
}

/// The real server, driven over stdio.
struct Server {
    child: Child,
    stdin: ChildStdin,
    responses: Receiver<String>,
    next_id: u64,
    project: PathBuf,
}

/// A tool call's outcome: `Ok(text)` or `Err(error text)`.
type ToolResult = Result<String, String>;

impl Server {
    fn start(project: &Path) -> Self {
        let mut child = binary_command(project)
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

    fn call(&mut self, name: &str, mut arguments: Value) -> ToolResult {
        self.next_id += 1;
        arguments["project_dir"] = json!(self.project.to_string_lossy());
        let request = json!({
            "jsonrpc": "2.0", "id": self.next_id, "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        });
        writeln!(self.stdin, "{request}").expect("write request");
        self.stdin.flush().expect("flush");
        let line = self
            .responses
            .recv_timeout(Duration::from_secs(20))
            .unwrap_or_else(|_| panic!("no response for {name} within 20s"));
        let response: Value = serde_json::from_str(&line).expect("JSON-RPC response");
        assert_eq!(response["id"], self.next_id);
        if let Some(error) = response.get("error").filter(|e| !e.is_null()) {
            return Err(error["message"].as_str().unwrap_or("").to_string());
        }
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string();
        if response["result"]["isError"].as_bool().unwrap_or(false) {
            Err(text)
        } else {
            Ok(text)
        }
    }

    fn call_json(&mut self, name: &str, arguments: Value) -> Value {
        let text = self
            .call(name, arguments)
            .unwrap_or_else(|e| panic!("{name} failed: {e}"));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: not JSON ({e}): {text}"))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn run_cli(project: &Path, args: &[&str]) -> (String, i32) {
    let output = binary_command(project)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("failed to run handoff-mcp");
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        output.status.code().unwrap_or(-1),
    )
}

fn project() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

fn config_path(project: &Path) -> PathBuf {
    project.join(".handoff").join("config.toml")
}

fn config_bytes(project: &Path) -> Vec<u8> {
    std::fs::read(config_path(project)).expect("config.toml")
}

fn config_modified(project: &Path) -> SystemTime {
    std::fs::metadata(config_path(project))
        .unwrap()
        .modified()
        .unwrap()
}

/// The values the `beginner` preset sets, as `handoff_get_config` reports them.
fn assert_beginner_config(config: &Value) {
    assert_eq!(config["trace"]["profile"], json!("minimal"), "{config}");
    assert_eq!(config["trace"]["done_guard"], json!("warn"), "{config}");
    assert_eq!(config["session_loop"]["profile"], json!("standard"));
    let onboarding = &config["onboarding"];
    assert_eq!(onboarding["preset"], json!("beginner"), "{config}");
    assert_eq!(onboarding["preflight"], json!("run"));
    assert_eq!(onboarding["next_step"], json!(true));
    assert_eq!(onboarding["smoke_check"], json!(true));
    assert_eq!(onboarding["release_guard"], json!("warn"));
    assert_eq!(onboarding["security_min_nfr"], json!(true));
}

/// What `handoff_init` wrote before the `[onboarding]` / `[commands]` sections
/// and presets existed, captured from a binary built before they were added.
const PRE_ONBOARDING_INIT_CONFIG: &str = r#"[project]
name = "demo"
description = "A demo"

[settings]
done_task_limit = 10
compact_task_limit = 40
auto_git_summary = true
require_estimate_hours = true
ai_estimate_multiplier = 0.2
schedule_stale_notice = true
layer_missing_notice = true
context_files = []
memory_enabled = true
memory_dup_threshold = 0.72
memory_query_min_score = 2.0
memory_query_relative_threshold = 0.3
memory_query_limit = 5
memory_stale_days = 60
memory_injected_gc_days = 14
timer_provider = "auto"
timer_authority_ttl_secs = 30
timer_idle_timeout_minutes = 10
multi_session = true

[settings.custom_fields]

[dashboard]
scan_dirs = ["~/pro/"]
exclude_patterns = []
max_depth = 5

[worktree]
auto_link = true

[worktree.session_loop]
auto_assign = false
merge_strategy = "merge-commit"
max_concurrent_wts = 4
auto_cleanup = false
"#;

// --- tool schemas ---

/// The schema is how an agent learns the arguments exist.
#[test]
fn tool_schemas_advertise_the_preset_arguments() {
    let dir = project();
    let mut server = Server::start(dir.path());
    writeln!(
        server.stdin,
        "{}",
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })
    )
    .unwrap();
    server.stdin.flush().unwrap();
    let line = server
        .responses
        .recv_timeout(Duration::from_secs(20))
        .expect("tools/list response");
    let response: Value = serde_json::from_str(&line).unwrap();
    let tools = response["result"]["tools"].as_array().expect("tools");
    let schema = |name: &str| -> &Value {
        &tools
            .iter()
            .find(|tool| tool["name"] == json!(name))
            .unwrap_or_else(|| panic!("{name} is not listed"))["inputSchema"]
    };

    let init = schema("handoff_init");
    assert_eq!(
        init["properties"]["preset"]["enum"],
        json!(handoff_mcp::storage::config::ONBOARDING_PRESETS),
        "{init}"
    );
    assert_eq!(init["required"], json!(["project_name"]));

    let update = schema("handoff_update_config");
    assert_eq!(
        update["properties"]["preset"]["enum"],
        json!(handoff_mcp::storage::config::ONBOARDING_PRESETS)
    );
    assert_eq!(update["properties"]["confirm"]["type"], json!("boolean"));
    assert!(update["properties"]["updates"].is_object());
    assert!(
        update.get("required").is_none() || update["required"] == json!([]),
        "`updates` is not required when a preset is given: {update}"
    );
}

// --- SPEC-ONB-010 AC1 ---

#[test]
fn ac1_init_with_the_beginner_preset_sets_every_preset_key() {
    let dir = project();
    let mut server = Server::start(dir.path());

    let reply = server
        .call(
            "handoff_init",
            json!({ "project_name": "demo", "preset": "beginner" }),
        )
        .expect("init with preset");
    assert!(reply.contains("beginner"), "{reply}");

    let config = server.call_json("handoff_get_config", json!({}));
    assert_beginner_config(&config);
    assert_eq!(config["project"]["name"], json!("demo"));
}

#[test]
fn cli_init_accepts_the_preset_flag() {
    let dir = project();
    let project_dir = dir.path().to_string_lossy().to_string();
    let (stdout, code) = run_cli(
        dir.path(),
        &[
            "init",
            "--project-name",
            "demo",
            "--preset",
            "beginner",
            "--project-dir",
            &project_dir,
        ],
    );
    assert_eq!(code, 0, "{stdout}");

    let (stdout, code) = run_cli(
        dir.path(),
        &["config", "get", "--project-dir", &project_dir],
    );
    assert_eq!(code, 0, "{stdout}");
    assert_beginner_config(&serde_json::from_str(&stdout).expect("config JSON"));
}

// --- SPEC-ONB-010 AC2 / AC3 ---

fn project_with_standard_trace_profile(server: &mut Server) {
    server
        .call("handoff_init", json!({ "project_name": "demo" }))
        .expect("init");
    server
        .call(
            "handoff_update_config",
            json!({ "updates": { "trace.profile": "standard" } }),
        )
        .expect("set trace.profile");
}

#[test]
fn ac2_preset_on_a_configured_project_writes_nothing_and_lists_the_changes() {
    let dir = project();
    let mut server = Server::start(dir.path());
    project_with_standard_trace_profile(&mut server);
    let before = config_bytes(dir.path());

    let reply = server.call_json("handoff_update_config", json!({ "preset": "beginner" }));

    assert_eq!(config_bytes(dir.path()), before, "config.toml changed");
    assert_eq!(reply["preset"], json!("beginner"));
    assert_eq!(reply["applied"], json!(false), "{reply}");
    let changes = reply["changes"].as_array().expect("changes");
    let trace_profile = changes
        .iter()
        .find(|change| change["key"] == json!("trace.profile"))
        .unwrap_or_else(|| panic!("trace.profile is not listed: {reply}"));
    assert_eq!(trace_profile["old"], json!("standard"));
    assert_eq!(trace_profile["new"], json!("minimal"));
    assert_eq!(reply["overwrites"], json!(["trace.profile"]), "{reply}");
    assert!(
        reply["message"].as_str().unwrap_or("").contains("confirm"),
        "the reply says how to go ahead: {reply}"
    );
    // The other keys the preset would set are listed too, with no old value.
    let preflight = changes
        .iter()
        .find(|change| change["key"] == json!("onboarding.preflight"))
        .expect("onboarding.preflight is listed");
    assert_eq!(preflight["old"], Value::Null);
    assert_eq!(preflight["new"], json!("run"));
}

#[test]
fn ac3_confirm_writes_the_preset() {
    let dir = project();
    let mut server = Server::start(dir.path());
    project_with_standard_trace_profile(&mut server);
    let before = config_bytes(dir.path());

    let reply = server.call_json(
        "handoff_update_config",
        json!({ "preset": "beginner", "confirm": true }),
    );

    assert_eq!(reply["applied"], json!(true), "{reply}");
    assert_ne!(
        config_bytes(dir.path()),
        before,
        "config.toml was not rewritten"
    );
    let config = server.call_json("handoff_get_config", json!({}));
    assert_beginner_config(&config);
    assert_eq!(
        config["settings"]["require_estimate_hours"],
        json!(true),
        "settings outside the preset are kept"
    );
}

#[test]
fn preset_without_conflicts_is_applied_without_confirmation() {
    let dir = project();
    let mut server = Server::start(dir.path());
    server
        .call("handoff_init", json!({ "project_name": "demo" }))
        .expect("init");

    let reply = server.call_json("handoff_update_config", json!({ "preset": "beginner" }));

    assert_eq!(reply["applied"], json!(true), "{reply}");
    assert_eq!(reply["overwrites"], json!([]));
    assert_eq!(
        reply["changes"].as_array().map(Vec::len),
        Some(9),
        "{reply}"
    );
    assert_beginner_config(&server.call_json("handoff_get_config", json!({})));
}

#[test]
fn applying_the_preset_again_changes_nothing_and_rewrites_nothing() {
    let dir = project();
    let mut server = Server::start(dir.path());
    server
        .call(
            "handoff_init",
            json!({ "project_name": "demo", "preset": "beginner" }),
        )
        .expect("init");
    let before = config_bytes(dir.path());
    let modified = config_modified(dir.path());
    std::thread::sleep(Duration::from_millis(20));

    let reply = server.call_json("handoff_update_config", json!({ "preset": "beginner" }));

    assert_eq!(reply["applied"], json!(true), "{reply}");
    assert_eq!(reply["changes"], json!([]));
    assert_eq!(config_bytes(dir.path()), before);
    assert_eq!(
        config_modified(dir.path()),
        modified,
        "config.toml was rewritten"
    );
}

#[test]
fn cli_config_update_applies_the_preset_with_confirm() {
    let dir = project();
    let project_dir = dir.path().to_string_lossy().to_string();
    let base = ["--project-dir", project_dir.as_str()];
    let cli = |args: &[&str]| run_cli(dir.path(), &[args, &base].concat());
    assert_eq!(cli(&["init", "--project-name", "demo"]).1, 0);
    let (stdout, code) = cli(&[
        "config",
        "update",
        "--updates",
        r#"{"trace.profile":"full"}"#,
    ]);
    assert_eq!(code, 0, "{stdout}");

    let (stdout, code) = cli(&["config", "update", "--preset", "beginner"]);
    assert_eq!(code, 0, "{stdout}");
    let reply: Value = serde_json::from_str(&stdout).expect("JSON reply");
    assert_eq!(reply["applied"], json!(false));
    assert_eq!(reply["overwrites"], json!(["trace.profile"]));

    let (stdout, code) = cli(&["config", "update", "--preset", "beginner", "--confirm"]);
    assert_eq!(code, 0, "{stdout}");
    let reply: Value = serde_json::from_str(&stdout).expect("JSON reply");
    assert_eq!(reply["applied"], json!(true));

    let (stdout, _) = cli(&["config", "get"]);
    assert_beginner_config(&serde_json::from_str(&stdout).expect("config JSON"));
}

// --- SPEC-ONB-010 AC4 ---

#[test]
fn ac4_init_without_a_preset_writes_the_same_config_as_before() {
    let dir = project();
    let mut server = Server::start(dir.path());

    let reply = server
        .call(
            "handoff_init",
            json!({ "project_name": "demo", "description": "A demo" }),
        )
        .expect("init");

    assert_eq!(
        String::from_utf8(config_bytes(dir.path())).unwrap(),
        PRE_ONBOARDING_INIT_CONFIG
    );
    assert_eq!(
        reply,
        format!(
            "Initialized handoff tracking for 'demo' at {}/.handoff/\n\
             Created: config.toml, sessions/, tasks/, memory/",
            std::fs::canonicalize(dir.path()).unwrap().display()
        ),
        "the reply of a preset-less init is unchanged"
    );
}

// --- SPEC-ONB-010 AC5 ---

#[test]
fn ac5_an_unknown_preset_fails_init_and_creates_nothing() {
    for bad in [
        json!("unknown"),
        json!(""),
        json!(true),
        json!(["beginner"]),
    ] {
        let dir = project();
        let mut server = Server::start(dir.path());
        let err = server
            .call(
                "handoff_init",
                json!({ "project_name": "demo", "preset": bad }),
            )
            .expect_err("an unknown preset must fail");
        assert!(
            err.contains("beginner"),
            "{bad}: accepted names are listed: {err}"
        );
        assert!(
            !dir.path().join(".handoff").exists(),
            "{bad}: .handoff/ was created"
        );
    }
}

#[test]
fn ac5_an_unknown_preset_fails_update_config_and_changes_nothing() {
    let dir = project();
    let mut server = Server::start(dir.path());
    server
        .call("handoff_init", json!({ "project_name": "demo" }))
        .expect("init");
    let before = config_bytes(dir.path());

    for arguments in [
        json!({ "preset": "unknown" }),
        json!({ "preset": "unknown", "confirm": true }),
        json!({ "preset": 1 }),
    ] {
        let err = server
            .call("handoff_update_config", arguments.clone())
            .expect_err("an unknown preset must fail");
        assert!(err.contains("beginner"), "{arguments}: {err}");
        assert_eq!(config_bytes(dir.path()), before, "{arguments}");
    }
}

#[test]
fn preset_arguments_that_do_not_fit_together_are_rejected() {
    let dir = project();
    let mut server = Server::start(dir.path());
    server
        .call("handoff_init", json!({ "project_name": "demo" }))
        .expect("init");
    let before = config_bytes(dir.path());

    for (arguments, expected) in [
        (
            json!({ "preset": "beginner", "updates": { "trace.profile": "full" } }),
            "updates",
        ),
        (json!({ "confirm": true, "updates": {} }), "confirm"),
        (json!({ "preset": "beginner", "confirm": "yes" }), "confirm"),
        (json!({}), "updates"),
    ] {
        let err = server
            .call("handoff_update_config", arguments.clone())
            .expect_err("must be rejected");
        assert!(err.contains(expected), "{arguments}: {err}");
        assert_eq!(config_bytes(dir.path()), before, "{arguments}");
    }
}

// --- [onboarding] keys through handoff_update_config ---

#[test]
fn onboarding_keys_are_validated_on_write() {
    let dir = project();
    let mut server = Server::start(dir.path());
    server
        .call("handoff_init", json!({ "project_name": "demo" }))
        .expect("init");
    let before = config_bytes(dir.path());

    for (updates, expected) in [
        (
            json!({ "onboarding.preflight": "sometimes" }),
            "off/check/run",
        ),
        (
            json!({ "onboarding.release_guard": "strict" }),
            "off/warn/block",
        ),
        (json!({ "onboarding.preset": "expert" }), "beginner"),
        (json!({ "onboarding.next_step": "yes" }), "onboarding"),
        (json!({ "onboarding": { "smoke_check": 1 } }), "onboarding"),
    ] {
        let err = server
            .call("handoff_update_config", json!({ "updates": updates }))
            .expect_err("must be rejected");
        assert!(err.contains(expected), "{updates}: {err}");
        assert_eq!(config_bytes(dir.path()), before, "{updates}");
    }

    server
        .call(
            "handoff_update_config",
            json!({ "updates": {
                "onboarding.preflight": "check",
                "onboarding.next_step": true,
                "onboarding.release_guard": "block"
            } }),
        )
        .expect("valid onboarding keys");
    let config = server.call_json("handoff_get_config", json!({}));
    assert_eq!(config["onboarding"]["preflight"], json!("check"));
    assert_eq!(config["onboarding"]["next_step"], json!(true));
    assert_eq!(config["onboarding"]["release_guard"], json!("block"));

    // A typed-key update re-serializes the whole config; the section survives.
    server
        .call(
            "handoff_update_config",
            json!({ "updates": { "settings.done_task_limit": 12 } }),
        )
        .expect("typed key");
    let config = server.call_json("handoff_get_config", json!({}));
    assert_eq!(config["onboarding"]["preflight"], json!("check"));
    assert_eq!(config["onboarding"]["next_step"], json!(true));
}

// --- SPEC-ONB-091 ---

/// A config.toml as a project from before this feature would have it, with a
/// hand-written comment that any rewrite would lose.
fn legacy_project() -> tempfile::TempDir {
    let dir = project();
    let mut server = Server::start(dir.path());
    server
        .call(
            "handoff_init",
            json!({ "project_name": "demo", "description": "A demo" }),
        )
        .expect("init");
    drop(server);
    let legacy = format!("# kept by hand\n{PRE_ONBOARDING_INIT_CONFIG}");
    std::fs::write(config_path(dir.path()), legacy).unwrap();
    dir
}

#[test]
fn spec091_ac3_loading_a_config_without_onboarding_does_not_rewrite_it() {
    let dir = legacy_project();
    let before = config_bytes(dir.path());
    let modified = config_modified(dir.path());
    assert!(!String::from_utf8_lossy(&before).contains("onboarding"));
    std::thread::sleep(Duration::from_millis(20));

    let mut server = Server::start(dir.path());
    for (tool, arguments) in [
        ("handoff_load_context", json!({})),
        ("handoff_get_config", json!({})),
        ("handoff_list_tasks", json!({})),
        ("handoff_list_milestones", json!({})),
        ("handoff_get_metrics", json!({})),
        ("handoff_load_context", json!({})),
    ] {
        server
            .call(tool, arguments)
            .unwrap_or_else(|e| panic!("{tool} failed: {e}"));
        assert_eq!(
            config_bytes(dir.path()),
            before,
            "{tool} changed config.toml"
        );
    }
    drop(server);
    let project_dir = dir.path().to_string_lossy().to_string();
    for args in [
        vec!["config", "get", "--project-dir", project_dir.as_str()],
        vec!["session", "load", "--project-dir", project_dir.as_str()],
        vec!["task", "list", "--project-dir", project_dir.as_str()],
    ] {
        let (stdout, code) = run_cli(dir.path(), &args);
        assert_eq!(code, 0, "{args:?}: {stdout}");
        assert_eq!(
            config_bytes(dir.path()),
            before,
            "{args:?} changed config.toml"
        );
    }

    assert_eq!(
        config_modified(dir.path()),
        modified,
        "config.toml was rewritten with the same content"
    );
}

#[test]
fn spec091_ac2_load_context_adds_no_onboarding_keys_without_the_section() {
    let dir = legacy_project();
    let mut server = Server::start(dir.path());

    let context = server.call_json("handoff_load_context", json!({}));

    let keys: Vec<&String> = context.as_object().expect("object").keys().collect();
    for added in ["next_step", "onboarding", "doctor", "preflight", "commands"] {
        assert!(
            !keys.iter().any(|key| key.as_str() == added),
            "load_context gained `{added}`: {keys:?}"
        );
    }
    let config = server.call_json("handoff_get_config", json!({}));
    assert!(config.get("onboarding").is_none(), "{config}");
    assert!(config.get("commands").is_none(), "{config}");
}

#[test]
fn spec091_defaults_are_off_for_a_section_that_only_sets_other_keys() {
    let dir = project();
    let mut server = Server::start(dir.path());
    server
        .call("handoff_init", json!({ "project_name": "demo" }))
        .expect("init");
    server
        .call(
            "handoff_update_config",
            json!({ "updates": { "onboarding.smoke_check": true } }),
        )
        .expect("set one key");

    let config = server.call_json("handoff_get_config", json!({}));
    let onboarding = config["onboarding"].as_object().expect("section");
    assert_eq!(
        onboarding.keys().collect::<Vec<_>>(),
        vec!["smoke_check"],
        "setting one key must not write defaults for the others"
    );
    let parsed: handoff_mcp::storage::config::Config =
        toml::from_str(&String::from_utf8(config_bytes(dir.path())).unwrap()).unwrap();
    assert!(parsed.onboarding.smoke_check());
    assert_eq!(parsed.onboarding.preflight(), "off");
    assert_eq!(parsed.onboarding.release_guard(), "off");
    assert!(!parsed.onboarding.next_step());
    assert!(!parsed.onboarding.security_min_nfr());
    assert_eq!(parsed.onboarding.preset(), None);
}
