//! Real-binary E2E for the tool calls session-loop Step 0b (Preflight,
//! SPEC-ONB-003) and Step 0c (Smoke check, SPEC-ONB-030 / SPEC-ONB-031) are
//! built on.
//!
//! The two steps are prose the session manager follows
//! (`plugin-task-loop/commands/session-loop.md`); their wording is pinned by
//! `plugin-task-loop/workflows/lib/session-loop-onboarding.test.js`. These
//! tests play the manager's part against the real server, call by call as the
//! steps spell them out, so a step that names an argument the server rejects,
//! or relies on an answer the server does not give, fails here.
//!
//! Unix only: the commands under test are shell command lines.
#![cfg(unix)]

#[path = "support/onboarding_e2e.rs"]
mod support;

use std::process::Command;
use std::time::Instant;

use serde_json::{json, Value};
use support::{entry, status_of, Sandbox, Server, FILLED_CLAUDE_MD};

/// The constant title Step 0c files its bug task under.
const SMOKE_BUG_TITLE: &str = "[smoke] Smoke check failed";

/// A project in which every file-based check of the diagnosis passes.
fn ready_project(config_tail: &str) -> Sandbox {
    let sandbox = Sandbox::initialized(config_tail);
    sandbox.write("CLAUDE.md", FILLED_CLAUDE_MD);
    sandbox
}

/// What the manager reads at the top of Step 0b.
struct Switches {
    preflight: String,
    smoke_check: bool,
    smoke_command: Option<String>,
    smoke_timeout_sec: u64,
}

/// Step 0b's configuration read, with the defaults the step states for
/// absent keys and its rule that an unknown `preflight` value reads as off.
fn read_switches(server: &mut Server) -> Switches {
    let config = server.call("handoff_get_config", json!({}));
    let preflight = match config["onboarding"]["preflight"].as_str() {
        Some(mode @ ("check" | "run")) => mode.to_string(),
        _ => "off".to_string(),
    };
    Switches {
        preflight,
        smoke_check: config["onboarding"]["smoke_check"]
            .as_bool()
            .unwrap_or(false),
        smoke_command: config["commands"]["smoke"]
            .as_str()
            .filter(|command| !command.trim().is_empty())
            .map(str::to_string),
        smoke_timeout_sec: config["onboarding"]["smoke_timeout_sec"]
            .as_u64()
            .unwrap_or(120),
    }
}

/// Runs `command` the way the manager does: through the shell, from the
/// project root, exactly as given.
fn run_in_project(sandbox: &Sandbox, command: &str) -> (i64, u64, String) {
    let started = Instant::now();
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(&sandbox.project)
        .output()
        .expect("sh");
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (
        i64::from(output.status.code().unwrap_or(-1)),
        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        text,
    )
}

/// Step 0b in `run` mode: ask for the commands, run each one, report back.
/// Returns the verdict and the ids of the commands that were run.
fn preflight_run(sandbox: &Sandbox, server: &mut Server) -> (Value, Vec<String>) {
    let first = server.call("handoff_doctor", json!({}));
    let to_run = first["commands_to_run"]
        .as_array()
        .expect("commands_to_run")
        .clone();
    if to_run.is_empty() {
        return (first, Vec::new());
    }
    let mut ran = Vec::new();
    let results: Vec<Value> = to_run
        .iter()
        .map(|entry| {
            let command = entry["command"].as_str().expect("command");
            let (exit_code, duration_ms, output) = run_in_project(sandbox, command);
            ran.push(entry["id"].as_str().expect("id").to_string());
            json!({
                "id": entry["id"],
                "command": command,
                "exit_code": exit_code,
                "duration_ms": duration_ms,
                "output_tail": output,
            })
        })
        .collect();
    (
        server.call("handoff_doctor", json!({ "results": results })),
        ran,
    )
}

/// Every node of a `handoff_list_tasks` tree, children included.
fn flatten(nodes: &Value, out: &mut Vec<Value>) {
    for node in nodes.as_array().into_iter().flatten() {
        out.push(node.clone());
        flatten(&node["children"], out);
    }
}

/// Step 0c's lookup: the open `[smoke]` bug task, if there is one.
fn open_smoke_bug(server: &mut Server) -> Option<String> {
    let listed = server.call("handoff_list_tasks", json!({ "label_filter": "bug" }));
    let mut tasks = Vec::new();
    flatten(&listed["task_tree"], &mut tasks);
    tasks
        .iter()
        .find(|task| {
            task["title"] == json!(SMOKE_BUG_TITLE)
                && !matches!(task["status"].as_str(), Some("done" | "skipped"))
        })
        .map(|task| task["id"].as_str().expect("id").to_string())
}

/// Every task titled as the smoke bug, whatever its status.
fn smoke_bug_count(server: &mut Server) -> usize {
    let listed = server.call("handoff_list_tasks", json!({}));
    let mut tasks = Vec::new();
    flatten(&listed["task_tree"], &mut tasks);
    tasks
        .iter()
        .filter(|task| {
            task["title"]
                .as_str()
                .is_some_and(|title| title.contains("[smoke]"))
        })
        .count()
}

/// Step 0c on a failed smoke check: look the bug task up, file it only when
/// there is none. Returns the task id and whether it was filed by this call.
fn file_smoke_bug_once(server: &mut Server, command: &str, exit_code: i64) -> (String, bool) {
    if let Some(id) = open_smoke_bug(server) {
        return (id, false);
    }
    server.ok(
        "handoff_update_task",
        json!({ "task": {
            "title": SMOKE_BUG_TITLE,
            "status": "todo",
            "priority": "high",
            "labels": ["bug"],
            "notes": format!("Smoke check failed.\nCommand: {command}\nResult: exit code {exit_code}"),
            "done_criteria": [{ "item": "The smoke command exits with code 0" }],
        } }),
    );
    let id = open_smoke_bug(server).expect("the task that was just filed is found by the lookup");
    (id, true)
}

/// One launch of session-loop up to the end of Step 0c. `Proceed` means Step 1
/// would follow; the other outcomes end the session with no Workflow.
#[derive(Debug, PartialEq)]
enum Launch {
    Proceed,
    StoppedByPreflight { failed: Vec<String> },
    StoppedBySmoke { bug_task: String, filed: bool },
}

fn launch(sandbox: &Sandbox, server: &mut Server) -> Launch {
    let switches = read_switches(server);
    if switches.preflight != "off" {
        let verdict = if switches.preflight == "run" {
            preflight_run(sandbox, server).0
        } else {
            server.call("handoff_doctor", json!({}))
        };
        if verdict["ready"] != json!(true) {
            let failed = verdict["checks"]
                .as_array()
                .expect("checks")
                .iter()
                .filter(|check| check["status"] == json!("fail"))
                .map(|check| check["id"].as_str().expect("id").to_string())
                .collect();
            return Launch::StoppedByPreflight { failed };
        }
    }
    if switches.smoke_check {
        if let Some(command) = switches.smoke_command {
            assert!(switches.smoke_timeout_sec >= 1);
            let (exit_code, _, _) = run_in_project(sandbox, &command);
            if exit_code != 0 {
                let (bug_task, filed) = file_smoke_bug_once(server, &command, exit_code);
                return Launch::StoppedBySmoke { bug_task, filed };
            }
        }
    }
    Launch::Proceed
}

// --- SPEC-ONB-030: the smoke command and its timeout in the configuration ---

#[test]
fn spec030_ac1_get_config_returns_the_smoke_command() {
    let sandbox = Sandbox::initialized("[commands]\nsmoke = \"./scripts/smoke.sh --fast\"\n");
    let mut server = sandbox.server();
    let config = server.call("handoff_get_config", json!({}));
    assert_eq!(
        config["commands"]["smoke"],
        json!("./scripts/smoke.sh --fast")
    );

    let switches = read_switches(&mut server);
    assert_eq!(
        switches.smoke_command.as_deref(),
        Some("./scripts/smoke.sh --fast")
    );
    // Not switched on, and no timeout written: the step's defaults apply.
    assert!(!switches.smoke_check);
    assert_eq!(switches.smoke_timeout_sec, 120);
    assert!(config["onboarding"]["smoke_timeout_sec"].is_null());
}

#[test]
fn smoke_timeout_is_written_validated_and_returned() {
    let sandbox = Sandbox::initialized("");
    let mut server = sandbox.server();
    let config_path = sandbox.handoff().join("config.toml");
    let before = std::fs::read(&config_path).unwrap();

    for bad in [json!(0), json!(-5), json!("60"), json!(1.5), json!(true)] {
        let error = server.err(
            "handoff_update_config",
            json!({ "updates": { "onboarding.smoke_timeout_sec": bad } }),
        );
        assert!(error.contains("onboarding"), "{bad}: {error}");
        assert_eq!(std::fs::read(&config_path).unwrap(), before, "{bad}");
    }

    server.ok(
        "handoff_update_config",
        json!({ "updates": { "onboarding.smoke_timeout_sec": 30 } }),
    );
    let config = server.call("handoff_get_config", json!({}));
    assert_eq!(config["onboarding"]["smoke_timeout_sec"], json!(30));
    assert_eq!(read_switches(&mut server).smoke_timeout_sec, 30);
}

// --- SPEC-ONB-003: Preflight ---

#[test]
fn spec003_ac4_without_the_key_or_with_off_the_diagnosis_is_not_asked_for() {
    for tail in [
        "",
        "[onboarding]\npreflight = \"off\"\n",
        // A hand-written value the config layer reads as off.
        "[onboarding]\npreflight = \"sometimes\"\n",
    ] {
        // A project the diagnosis would fail: no CLAUDE.md, no commands.
        let sandbox = Sandbox::initialized(tail);
        let mut server = sandbox.server();
        assert_eq!(read_switches(&mut server).preflight, "off", "{tail}");
        assert_eq!(launch(&sandbox, &mut server), Launch::Proceed, "{tail}");
    }
}

#[test]
fn spec003_ac1_ac2_preflight_run_with_a_failing_test_stops_and_names_the_next_step() {
    let sandbox = ready_project(
        "[commands]\nbuild = \"true\"\ntest = \"echo 2 tests failed; exit 1\"\n\n[onboarding]\npreflight = \"run\"\n",
    );
    let mut server = sandbox.server();

    let (verdict, ran) = preflight_run(&sandbox, &mut server);
    assert_eq!(ran, ["cmd_build", "cmd_test"]);
    assert_eq!(verdict["ready"], json!(false));
    assert_eq!(status_of(&verdict["checks"], "cmd_build"), "pass");
    let failed = entry(&verdict["checks"], "cmd_test");
    assert_eq!(failed["status"], json!("fail"));
    assert_eq!(failed["source"], json!("reported"));
    // What the stop message is built from: the id, the detail and the next step.
    assert!(failed["detail"].as_str().unwrap().contains("exit code 1"));
    assert!(failed["detail"]
        .as_str()
        .unwrap()
        .contains("2 tests failed"));
    assert!(!failed["next_step"].as_str().unwrap().trim().is_empty());

    assert_eq!(
        launch(&sandbox, &mut server),
        Launch::StoppedByPreflight {
            failed: vec!["cmd_test".to_string()]
        }
    );
}

#[test]
fn spec003_ac3_preflight_run_on_a_working_project_is_ready_after_one_report() {
    let sandbox = ready_project(
        "[commands]\nbuild = \"true\"\ntest = \"true\"\nlint = \"true\"\n\n[onboarding]\npreflight = \"run\"\n",
    );
    let mut server = sandbox.server();
    let (verdict, ran) = preflight_run(&sandbox, &mut server);
    assert_eq!(ran, ["cmd_build", "cmd_test", "cmd_lint"]);
    assert_eq!(verdict["ready"], json!(true), "{verdict}");
    // Nothing is left to run: the step never needs a third call.
    assert_eq!(verdict["commands_to_run"], json!([]));
    assert_eq!(launch(&sandbox, &mut server), Launch::Proceed);
}

#[test]
fn spec003_ac5_preflight_check_gives_a_verdict_without_any_command_result() {
    // The test command would fail if it were run; `check` never runs it.
    let sandbox = ready_project(
        "[commands]\ntest = \"touch ran-by-preflight; exit 1\"\n\n[onboarding]\npreflight = \"check\"\n",
    );
    let mut server = sandbox.server();
    let verdict = server.call("handoff_doctor", json!({}));
    assert_eq!(verdict["ready"], json!(true), "{verdict}");
    assert_eq!(status_of(&verdict["checks"], "cmd_test"), "skip");
    assert_eq!(launch(&sandbox, &mut server), Launch::Proceed);
    assert!(!sandbox.project.join("ran-by-preflight").exists());

    // The file-based checks still stop a project that is not set up.
    let sandbox = Sandbox::initialized("[onboarding]\npreflight = \"check\"\n");
    let mut server = sandbox.server();
    match launch(&sandbox, &mut server) {
        Launch::StoppedByPreflight { failed } => {
            assert!(
                failed.contains(&"claude_md_sections".to_string()),
                "{failed:?}"
            );
            assert!(
                failed.contains(&"commands_defined".to_string()),
                "{failed:?}"
            );
        }
        other => panic!("an unprepared project must stop, got {other:?}"),
    }
}

#[test]
fn preflight_run_runs_the_audit_command_and_never_smoke_or_format() {
    let sandbox = ready_project(
        "[commands]\ntest = \"true\"\nformat = \"touch formatted\"\nsmoke = \"touch smoked\"\naudit = \"echo 1 vulnerability; exit 3\"\n\n[onboarding]\npreflight = \"run\"\n",
    );
    let mut server = sandbox.server();
    let (verdict, ran) = preflight_run(&sandbox, &mut server);
    assert_eq!(ran, ["cmd_test", "dependencies_audited"]);
    for name in ["formatted", "smoked"] {
        assert!(!sandbox.project.join(name).exists(), "{name} was run");
    }
    // A failing audit makes the project not ready, like any other command.
    assert_eq!(verdict["ready"], json!(false));
    let audit = entry(&verdict["checks"], "dependencies_audited");
    assert_eq!(audit["status"], json!("fail"));
    assert!(audit["detail"]
        .as_str()
        .unwrap()
        .contains("1 vulnerability"));
    assert!(!audit["next_step"].as_str().unwrap().trim().is_empty());
}

#[test]
fn preflight_run_reports_a_command_that_could_not_finish_as_an_error() {
    let sandbox =
        ready_project("[commands]\ntest = \"sleep 600\"\n\n[onboarding]\npreflight = \"run\"\n");
    let mut server = sandbox.server();
    let first = server.call("handoff_doctor", json!({}));
    let to_run = &first["commands_to_run"][0];
    assert_eq!(to_run["id"], json!("cmd_test"));
    // The step's `{ id, command, error }` shape for a timeout.
    let verdict = server.call(
        "handoff_doctor",
        json!({ "results": [{
            "id": to_run["id"],
            "command": to_run["command"],
            "error": format!("timed out after {}s", to_run["timeout_sec"]),
        }] }),
    );
    assert_eq!(verdict["ready"], json!(false));
    let check = entry(&verdict["checks"], "cmd_test");
    assert_eq!(check["status"], json!("fail"));
    assert!(check["detail"].as_str().unwrap().contains("timed out"));
}

// --- SPEC-ONB-031: Smoke check ---

#[test]
fn spec031_ac5_without_smoke_check_the_smoke_command_is_not_run() {
    for tail in [
        "[commands]\nsmoke = \"touch smoked; exit 1\"\n",
        "[commands]\nsmoke = \"touch smoked; exit 1\"\n\n[onboarding]\nsmoke_check = false\n",
    ] {
        let sandbox = Sandbox::initialized(tail);
        let mut server = sandbox.server();
        assert_eq!(launch(&sandbox, &mut server), Launch::Proceed, "{tail}");
        assert!(!sandbox.project.join("smoked").exists(), "{tail}");
        assert_eq!(smoke_bug_count(&mut server), 0, "{tail}");
    }
}

#[test]
fn spec031_ac4_an_undefined_smoke_command_runs_nothing_and_files_nothing() {
    for tail in [
        "[onboarding]\nsmoke_check = true\n",
        // Blank is what clearing the key leaves behind.
        "[commands]\nsmoke = \"  \"\n\n[onboarding]\nsmoke_check = true\n",
    ] {
        let sandbox = Sandbox::initialized(tail);
        let mut server = sandbox.server();
        let switches = read_switches(&mut server);
        assert!(switches.smoke_check, "{tail}");
        assert_eq!(switches.smoke_command, None, "{tail}");
        assert_eq!(launch(&sandbox, &mut server), Launch::Proceed, "{tail}");
        assert_eq!(smoke_bug_count(&mut server), 0, "{tail}");
    }
}

#[test]
fn spec031_ac2_ac3_a_failing_smoke_check_files_one_bug_task_however_often_it_is_launched() {
    let sandbox = Sandbox::initialized(
        "[commands]\nsmoke = \"echo cannot start; exit 7\"\n\n[onboarding]\nsmoke_check = true\n",
    );
    let mut server = sandbox.server();
    assert_eq!(open_smoke_bug(&mut server), None);

    let first = launch(&sandbox, &mut server);
    let Launch::StoppedBySmoke { bug_task, filed } = first else {
        panic!("a failing smoke check must stop the launch, got {first:?}");
    };
    assert!(filed, "the first failed launch files the bug task");

    let task = server.call("handoff_get_task", json!({ "task_id": bug_task }));
    assert_eq!(task["title"], json!(SMOKE_BUG_TITLE));
    assert_eq!(task["status"], json!("todo"));
    assert_eq!(task["labels"], json!(["bug"]));
    assert!(task["notes"].as_str().unwrap().contains("exit code 7"));
    assert!(task["notes"]
        .as_str()
        .unwrap()
        .contains("echo cannot start; exit 7"));

    // Launched again, and again by a fresh server process: still one task.
    assert_eq!(
        launch(&sandbox, &mut server),
        Launch::StoppedBySmoke {
            bug_task: bug_task.clone(),
            filed: false
        }
    );
    drop(server);
    let mut server = sandbox.server();
    assert_eq!(
        launch(&sandbox, &mut server),
        Launch::StoppedBySmoke {
            bug_task: bug_task.clone(),
            filed: false
        }
    );
    assert_eq!(smoke_bug_count(&mut server), 1);

    // Being worked on, it is still the open bug task.
    for status in ["in_progress", "review", "blocked"] {
        server.ok(
            "handoff_update_task",
            json!({ "task": { "id": bug_task, "status": status, "schedule": { "estimate_hours": 1.0 } } }),
        );
        assert_eq!(
            open_smoke_bug(&mut server),
            Some(bug_task.clone()),
            "{status}"
        );
    }
}

#[test]
fn spec031_a_finished_smoke_bug_task_does_not_hide_a_new_failure() {
    let sandbox = Sandbox::initialized(
        "[commands]\nsmoke = \"exit 1\"\n\n[onboarding]\nsmoke_check = true\n",
    );
    let mut server = sandbox.server();
    let Launch::StoppedBySmoke {
        bug_task: first, ..
    } = launch(&sandbox, &mut server)
    else {
        panic!("must stop");
    };
    server.ok(
        "handoff_check_criterion",
        json!({ "task_id": first, "criterion_index": 0, "checked": true }),
    );
    server.ok(
        "handoff_update_task",
        json!({ "task": { "id": first, "status": "done", "schedule": { "estimate_hours": 1.0 } } }),
    );
    assert_eq!(open_smoke_bug(&mut server), None);

    let Launch::StoppedBySmoke {
        bug_task: second,
        filed,
    } = launch(&sandbox, &mut server)
    else {
        panic!("must stop");
    };
    assert!(
        filed,
        "a new failure after the old one was fixed is a new task"
    );
    assert_ne!(first, second);
    assert_eq!(open_smoke_bug(&mut server), Some(second));
}

#[test]
fn spec031_the_smoke_bug_task_is_found_when_it_was_moved_under_a_parent() {
    let sandbox = Sandbox::initialized(
        "[commands]\nsmoke = \"exit 1\"\n\n[onboarding]\nsmoke_check = true\n",
    );
    let mut server = sandbox.server();
    server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "t1", "title": "Stabilize the app" } }),
    );
    let Launch::StoppedBySmoke { bug_task, .. } = launch(&sandbox, &mut server) else {
        panic!("must stop");
    };
    server.ok(
        "handoff_update_task",
        json!({ "task": { "id": bug_task }, "move_to": "t1" }),
    );
    let Launch::StoppedBySmoke { filed, .. } = launch(&sandbox, &mut server) else {
        panic!("must stop");
    };
    assert!(!filed, "the lookup searches the children too");
    assert_eq!(smoke_bug_count(&mut server), 1);
}

#[test]
fn spec031_ac1_a_passing_smoke_check_lets_the_launch_proceed_and_files_nothing() {
    let sandbox = Sandbox::initialized(
        "[commands]\nsmoke = \"touch smoked\"\n\n[onboarding]\nsmoke_check = true\nsmoke_timeout_sec = 30\n",
    );
    let mut server = sandbox.server();
    assert_eq!(launch(&sandbox, &mut server), Launch::Proceed);
    assert!(sandbox.project.join("smoked").exists());
    assert_eq!(smoke_bug_count(&mut server), 0);
}

/// The records Steps 0b and 0c write (SPEC-ONB-003 `ready=true`,
/// SPEC-ONB-031 AC1 / AC4) and the close that ends a stopped session, with
/// the arguments the steps spell out.
#[test]
fn the_session_calls_of_steps_0b_and_0c_are_accepted_and_recorded() {
    let sandbox = Sandbox::initialized("");
    let mut server = sandbox.server();
    server.ok(
        "handoff_save_context",
        json!({ "session_status": "active", "summary": "Session 1: demo" }),
    );
    for note in [
        "Preflight (run): ready — 9 pass, 4 skip, 0 fail",
        "Smoke check: pass — ./smoke.sh (3 s)",
        "Smoke check: skip — [commands] smoke is not defined, nothing was run",
    ] {
        server.ok(
            "handoff_update_session",
            json!({ "add_handoff_note": { "category": "context", "note": note } }),
        );
    }
    let context = server.ok("handoff_load_context", json!({}));
    for note in [
        "Preflight (run): ready",
        "Smoke check: pass — ./smoke.sh (3 s)",
        "Smoke check: skip",
    ] {
        assert!(context.contains(note), "`{note}` is not in: {context}");
    }

    server.ok(
        "handoff_save_context",
        json!({
            "session_status": "closed",
            "summary": "Smoke check: fail — ./smoke.sh (exit code 7), bug task t1",
            "handoff_notes": [{ "category": "suggestion", "note": "Run /bugfix-loop task: t1" }],
        }),
    );
    let sessions = server.ok("handoff_list_sessions", json!({}));
    assert!(sessions.contains("Smoke check: fail"), "{sessions}");
    assert!(!sessions.contains("\"status\": \"active\""), "{sessions}");
}
