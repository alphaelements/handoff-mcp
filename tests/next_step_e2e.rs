//! Real-binary E2E for the recommended next step (SPEC-ONB-011): the
//! `handoff_next_step` MCP tool over stdio JSON-RPC, `handoff-mcp next-step`,
//! and the `next_step` key of `handoff_load_context` under
//! `[onboarding] next_step`.
//!
//! Unix only: the fixtures use tripwire scripts.
#![cfg(unix)]

#[path = "support/onboarding_e2e.rs"]
mod support;

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{json, Value};
use support::{Sandbox, Server, FILLED_CLAUDE_MD};

const COMMANDS: &str = "[commands]\ntest = \"true\"\n";

/// The nine states, in the order they are checked.
const STATES: [&str; 9] = [
    "not_initialized",
    "not_ready",
    "review_pending",
    "in_progress",
    "no_brainstorm",
    "no_requirements",
    "ready_tasks",
    "release_ready",
    "all_done",
];

/// An initialized project that passes the readiness diagnosis: the project
/// sections are in `CLAUDE.md` and a command is defined.
fn ready_project(config_tail: &str) -> Sandbox {
    let sandbox = Sandbox::initialized(&format!("{COMMANDS}\n{config_tail}"));
    sandbox.write("CLAUDE.md", FILLED_CLAUDE_MD);
    sandbox
}

fn task(server: &mut Server, id: &str, status: &str, dependencies: &[&str]) {
    server.ok(
        "handoff_update_task",
        json!({ "task": {
            "id": id, "title": format!("Task {id}"), "status": status,
            "dependencies": dependencies,
            "schedule": { "estimate_hours": 1.0 },
        } }),
    );
}

fn requirement(server: &mut Server) {
    server.ok(
        "handoff_doc_save",
        json!({
            "slug": "reqs", "title": "Requirements", "layer": "requirement",
            "body": "# Requirements\n\n### REQ-1 Login\n\nA user can log in.\n",
        }),
    );
}

fn finalized_brainstorm(server: &mut Server) {
    let started = server.call(
        "handoff_brainstorm_start",
        json!({ "topic": "Temperature monitor", "initial_context": "Watch the fridge" }),
    );
    let id = started["brainstorm_id"].as_str().expect("brainstorm_id");
    let finalized = server.call(
        "handoff_brainstorm_finalize",
        json!({ "brainstorm_id": id, "executive_summary": "Summary", "scope_in": ["alerts"] }),
    );
    assert_eq!(finalized["status"], json!("finalized"), "{finalized}");
}

/// A project in `state` and in no earlier one.
fn project_in(state: &str) -> Sandbox {
    match state {
        "not_initialized" => Sandbox::empty(),
        // Initialized, but `CLAUDE.md` and `[commands]` are missing.
        "not_ready" => Sandbox::initialized(""),
        "review_pending" => {
            let sandbox = ready_project("");
            let mut server = sandbox.server();
            task(&mut server, "t1", "in_progress", &[]);
            task(&mut server, "t2", "review", &[]);
            sandbox
        }
        "in_progress" => {
            let sandbox = ready_project("");
            let mut server = sandbox.server();
            task(&mut server, "t1", "todo", &[]);
            task(&mut server, "t2", "in_progress", &[]);
            sandbox
        }
        "no_brainstorm" => {
            let sandbox = ready_project("");
            // A ready task does not skip the planning states.
            task(&mut sandbox.server(), "t1", "todo", &[]);
            sandbox
        }
        "no_requirements" => {
            let sandbox = ready_project("");
            let mut server = sandbox.server();
            finalized_brainstorm(&mut server);
            task(&mut server, "t1", "todo", &[]);
            sandbox
        }
        "ready_tasks" => {
            let sandbox = ready_project("[milestones.v1]\n");
            let mut server = sandbox.server();
            requirement(&mut server);
            task(&mut server, "t1", "done", &[]);
            task(&mut server, "t2", "todo", &["t1"]);
            task(&mut server, "t3", "todo", &["t2"]);
            sandbox
        }
        "release_ready" => {
            let sandbox = ready_project(
                "[milestones.v0]\nstatus = \"released\"\n\n[milestones.v1]\nstatus = \"active\"\n",
            );
            let mut server = sandbox.server();
            requirement(&mut server);
            task(&mut server, "t1", "done", &[]);
            task(&mut server, "t2", "skipped", &[]);
            sandbox
        }
        "all_done" => {
            let sandbox = ready_project("[milestones.v0]\nstatus = \"released\"\n");
            let mut server = sandbox.server();
            requirement(&mut server);
            task(&mut server, "t1", "done", &[]);
            sandbox
        }
        other => panic!("no fixture for state `{other}`"),
    }
}

/// Every file under `dir`: path, content and modification time.
fn fingerprint(dir: &Path) -> Vec<(PathBuf, Vec<u8>, SystemTime)> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
                files.push((path.clone(), std::fs::read(&path).unwrap(), modified));
            }
        }
    }
    files.sort();
    files
}

fn next_step(server: &mut Server) -> Value {
    server.call("handoff_next_step", json!({}))
}

#[test]
fn the_tool_is_listed_as_a_read_only_tool_of_its_own() {
    let sandbox = Sandbox::empty();
    let tool = sandbox.server().tool_definition("handoff_next_step");
    let properties = tool["inputSchema"]["properties"].as_object().unwrap();
    assert_eq!(properties.keys().collect::<Vec<_>>(), ["project_dir"]);
    assert!(tool["inputSchema"].get("required").is_none());
    let description = tool["description"].as_str().unwrap();
    for expected in STATES {
        assert!(description.contains(expected), "{expected}: {description}");
    }
    assert!(
        description.contains("Read-only") && description.contains("handoff_trace_next"),
        "it says it writes nothing and how it differs from handoff_trace_next: {description}"
    );
}

// SPEC-ONB-011 AC1, AC2, AC3 and the done criterion "nothing under .handoff/
// changes".
#[test]
fn ac1_ac2_ac3_each_of_the_nine_states_is_reported_twice_alike_and_nothing_is_written() {
    for state in STATES {
        let sandbox = project_in(state);
        let before = fingerprint(&sandbox.project);
        let mut server = sandbox.server();

        let first = next_step(&mut server);
        assert_eq!(first["state"], json!(state), "{first}");
        assert_eq!(
            first.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["state", "action", "facts"],
            "{state}"
        );
        // AC1: exactly one action; AC2: its command is not empty.
        let action = first["action"].as_object().expect("one action object");
        assert_eq!(
            action.keys().collect::<Vec<_>>(),
            ["title", "command", "reason"],
            "{state}"
        );
        for (field, value) in action {
            assert!(
                !value.as_str().unwrap_or("").trim().is_empty(),
                "{state}: empty {field}"
            );
        }
        let command = action["command"].as_str().unwrap();
        let is_slash_command = command.starts_with('/') && !command.contains(' ');
        let is_tool_call = command.starts_with("handoff_") && command.ends_with(')');
        let is_request = command.contains(' ') && command.ends_with('.');
        assert!(
            is_slash_command || is_tool_call || is_request,
            "{state}: `{command}` is not a slash command, a tool call or a request"
        );
        assert!(first["facts"].is_object(), "{state}");

        // AC3: the same answer again, and nothing changed on disk.
        let second = next_step(&mut server);
        assert_eq!(first, second, "{state}");
        drop(server);
        assert_eq!(
            fingerprint(&sandbox.project),
            before,
            "{state}: handoff_next_step wrote something"
        );
        // A new process answers the same.
        assert_eq!(next_step(&mut sandbox.server()), first, "{state}");
    }
}

#[test]
fn each_state_recommends_its_documented_operation() {
    let expected = [
        ("not_initialized", "handoff_init(project_name: \"project\")"),
        ("review_pending", "handoff_get_task(task_id: \"t2\")"),
        ("in_progress", "handoff_get_task(task_id: \"t2\")"),
        ("no_brainstorm", "/brainstorm-loop"),
        ("no_requirements", "/requirement-loop"),
        ("ready_tasks", "/session-loop"),
        ("release_ready", "handoff_release_check(milestone: \"v1\")"),
        ("all_done", "/brainstorm-loop"),
    ];
    for (state, command) in expected {
        let sandbox = project_in(state);
        let step = next_step(&mut sandbox.server());
        assert_eq!(step["action"]["command"], json!(command), "{step}");
    }

    // not_ready: what the diagnosis says about its first failed check.
    let sandbox = project_in("not_ready");
    let mut server = sandbox.server();
    let step = next_step(&mut server);
    let doctor = server.call("handoff_doctor", json!({}));
    let first_failed = doctor["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["status"] == json!("fail"))
        .expect("a failed check");
    assert_eq!(step["action"]["command"], first_failed["next_step"]);
    assert_eq!(first_failed["id"], json!("claude_md_sections"));
    assert_eq!(
        step["facts"],
        json!({
            "initialized": true,
            "doctor": { "ready": false, "failed": ["claude_md_sections", "commands_defined"] },
        })
    );
}

#[test]
fn the_facts_are_the_counts_the_decision_looked_at() {
    let step = next_step(&mut project_in("not_initialized").server());
    assert_eq!(step["facts"], json!({ "initialized": false }));

    let step = next_step(&mut project_in("ready_tasks").server());
    assert_eq!(
        step["facts"],
        json!({
            "initialized": true,
            "doctor": { "ready": true, "failed": [] },
            "tasks": {
                "total": 3, "by_status": { "done": 1, "todo": 2 },
                "unfinished": 2, "ready": 1,
            },
            "active_sessions": 0,
            "brainstorm_reports": 0,
            "requirement_items": 1,
        })
    );
    assert!(
        step["action"]["reason"]
            .as_str()
            .unwrap()
            .contains("1 todo task(s) have no unfinished dependency; the first is t2 (Task t2)"),
        "{step}"
    );

    let step = next_step(&mut project_in("no_requirements").server());
    assert_eq!(step["facts"]["brainstorm_reports"], json!(1));
    assert_eq!(step["facts"]["requirement_items"], json!(0));

    let step = next_step(&mut project_in("release_ready").server());
    assert_eq!(
        step["facts"]["milestones"],
        json!({ "total": 2, "unreleased": ["v1"] })
    );
}

/// An active session keeps the project "in progress" when no task is.
#[test]
fn an_active_session_without_a_task_in_progress_is_in_progress() {
    let sandbox = ready_project("");
    let mut server = sandbox.server();
    requirement(&mut server);
    task(&mut server, "t1", "todo", &[]);
    assert_eq!(next_step(&mut server)["state"], json!("ready_tasks"));

    server.ok(
        "handoff_save_context",
        json!({ "summary": "Working on login", "session_status": "active" }),
    );
    let step = next_step(&mut server);
    assert_eq!(step["state"], json!("in_progress"), "{step}");
    assert_eq!(step["facts"]["active_sessions"], json!(1));
    let command = step["action"]["command"].as_str().unwrap();
    assert!(
        command.starts_with("handoff_get_session(session_id: \""),
        "{command}"
    );
}

// SPEC-ONB-011 AC4
#[test]
fn ac4_load_context_carries_the_same_next_step_when_it_is_switched_on() {
    for state in ["not_ready", "review_pending", "ready_tasks", "all_done"] {
        let sandbox = project_in(state);
        sandbox.append_config("[onboarding]\nnext_step = true\n");
        let mut server = sandbox.server();
        let context = server.call("handoff_load_context", json!({}));
        let embedded = context
            .get("next_step")
            .unwrap_or_else(|| panic!("{state}: no next_step key in {context}"));
        assert_eq!(embedded["state"], json!(state));
        assert_eq!(
            *embedded,
            next_step(&mut server),
            "{state}: the key holds what handoff_next_step answers"
        );
    }
}

// SPEC-ONB-011 AC5
#[test]
fn ac5_load_context_has_no_next_step_key_unless_it_is_switched_on() {
    for onboarding in [
        "",
        "[onboarding]\nnext_step = false\n",
        "[onboarding]\npreflight = \"check\"\n",
    ] {
        let sandbox = project_in("ready_tasks");
        sandbox.append_config(onboarding);
        let mut server = sandbox.server();
        let context = server.call("handoff_load_context", json!({}));
        assert!(
            context.get("next_step").is_none(),
            "`{onboarding}`: {context}"
        );
    }
}

/// Switching it on adds the one key and changes nothing else.
#[test]
fn switching_next_step_on_only_adds_the_key() {
    let off = project_in("ready_tasks");
    let on = project_in("ready_tasks");
    on.append_config("[onboarding]\nnext_step = true\n");
    let without = off.server().call("handoff_load_context", json!({}));
    let with = on.server().call("handoff_load_context", json!({}));
    let keys =
        |context: &Value| -> Vec<String> { context.as_object().unwrap().keys().cloned().collect() };
    let mut expected = keys(&without);
    expected.push("next_step".to_string());
    assert_eq!(keys(&with), expected, "the key comes last");
    for key in ["project", "task_tree", "task_summary", "session_guidance"] {
        assert_eq!(with[key], without[key], "{key}");
    }
}

/// What the tool cannot determine is said in `warnings`, not hidden, and the
/// context still loads.
#[test]
fn load_context_reports_a_next_step_it_cannot_determine_as_a_warning() {
    let sandbox = project_in("no_brainstorm");
    sandbox.append_config("[onboarding]\nnext_step = true\n");
    let brainstorm = sandbox.handoff().join("brainstorm");
    std::fs::create_dir_all(&brainstorm).unwrap();
    std::fs::write(brainstorm.join("bs-broken.json"), "{ broken").unwrap();
    let mut server = sandbox.server();

    let error = server.err("handoff_next_step", json!({}));
    assert!(error.contains("bs-broken.json"), "{error}");

    let context = server.call("handoff_load_context", json!({}));
    assert!(context.get("next_step").is_none(), "{context}");
    assert_eq!(context["project"], json!("demo"));
    let warnings = context["warnings"].to_string();
    assert!(
        warnings.contains("next_step could not be determined")
            && warnings.contains("bs-broken.json"),
        "{warnings}"
    );
}

#[test]
fn an_unknown_argument_is_rejected() {
    let sandbox = project_in("no_brainstorm");
    let error = sandbox
        .server()
        .err("handoff_next_step", json!({ "milestone": "v1" }));
    assert!(error.contains("unknown argument(s): milestone"), "{error}");
}

#[test]
fn a_project_that_is_not_initialized_stays_uninitialized() {
    let sandbox = Sandbox::empty();
    let step = next_step(&mut sandbox.server());
    assert_eq!(step["state"], json!("not_initialized"));
    assert!(!sandbox.handoff().exists());
}

// SPEC-ONB-091: none of the new tools starts a process.
#[test]
fn the_tool_starts_no_process() {
    // A Git repository with a linked worktree, tracked files, documents, a
    // brainstorm, tasks and a session: every group the tool can read.
    let sandbox = ready_project("[milestones.v1]\n");
    sandbox.git(&["init", "-q"]);
    sandbox.write("a.txt", "x");
    // `CLAUDE.md` is left untracked, so the worktree has none.
    sandbox.git(&["add", "a.txt"]);
    sandbox.git(&["commit", "-q", "-m", "x"]);
    let linked = sandbox.root.path().join("linked");
    sandbox.git(&["worktree", "add", "-q", linked.to_str().unwrap()]);
    let linked = std::fs::canonicalize(&linked).unwrap();
    {
        let mut setup = sandbox.server();
        requirement(&mut setup);
        finalized_brainstorm(&mut setup);
        task(&mut setup, "t1", "done", &[]);
    }

    let tripwires = sandbox.tripwires();
    let mut command = sandbox.command();
    command.env("PATH", &tripwires.dir);
    let mut server = Server::start(command, &sandbox.project);
    // Anything the server starts on its own before the first tool call is
    // not what this test is about.
    server.request("tools/list", json!({}));
    let spawned_before = tripwires.spawned();

    let step = next_step(&mut server);
    assert_eq!(step["state"], json!("release_ready"), "{step}");
    // From the linked worktree, which shares the primary's `.handoff/`.
    server.project = linked.clone();
    let from_worktree = next_step(&mut server);
    assert_eq!(
        from_worktree["state"],
        json!("not_ready"),
        "{from_worktree}"
    );
    assert_eq!(from_worktree["facts"]["initialized"], json!(true));
    // And where there is nothing to read.
    let bare = sandbox.root.path().join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    server.project = std::fs::canonicalize(&bare).unwrap();
    assert_eq!(next_step(&mut server)["state"], json!("not_initialized"));

    assert_eq!(
        tripwires.spawned(),
        spawned_before,
        "handoff_next_step started a process"
    );
    assert!(
        !linked.join(".handoff").exists(),
        "no link is created in the worktree"
    );

    // The tripwire itself works: the CLI with --run trips it.
    let mut cli = sandbox.command();
    cli.env("PATH", &tripwires.dir)
        .args(["release-check", "v1", "--run"]);
    let _ = Sandbox::output(&mut cli);
    assert!(
        tripwires.spawned().contains("sh "),
        "the tripwire must catch a real spawn, or this test proves nothing"
    );
}

// ===========================================================================
// handoff-mcp next-step
// ===========================================================================

#[test]
fn the_cli_prints_what_the_tool_answers() {
    for state in ["not_initialized", "not_ready", "ready_tasks"] {
        let sandbox = project_in(state);
        let before = fingerprint(&sandbox.project);
        let output = sandbox.cli(&["next-step"]);
        assert_eq!(
            output.code, 0,
            "{state}: {}{}",
            output.stdout, output.stderr
        );
        let from_cli = output.json();
        assert_eq!(from_cli["state"], json!(state));
        assert_eq!(from_cli, next_step(&mut sandbox.server()), "{state}");
        assert_eq!(fingerprint(&sandbox.project), before, "{state}");
    }
}

#[test]
fn the_cli_honors_project_dir_and_is_listed_in_help() {
    let sandbox = project_in("ready_tasks");
    let elsewhere = Sandbox::empty();
    let mut command = elsewhere.command();
    command.args([
        "next-step",
        "--project-dir",
        sandbox.project.to_str().unwrap(),
    ]);
    let output = Sandbox::output(&mut command);
    assert_eq!(output.json()["state"], json!("ready_tasks"));

    let top = sandbox.cli(&["--help"]);
    assert!(top.stdout.contains("next-step"), "{}", top.stdout);
    let help = sandbox.cli(&["next-step", "--help"]);
    assert_eq!(help.code, 0);
    assert!(help.stdout.contains("next-step"), "{}", help.stdout);
}
