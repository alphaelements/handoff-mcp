//! Real-binary E2E for the pre-release check: the `handoff_release_check` MCP
//! tool (over stdio JSON-RPC) and `handoff-mcp release-check <milestone>
//! [--run]` (SPEC-ONB-040, SPEC-ONB-050, SPEC-ONB-092), plus the two
//! SPEC-ONB-050 checks of `handoff_doctor`.
//!
//! Unix only: the commands under test are shell command lines.
#![cfg(unix)]

#[path = "support/onboarding_e2e.rs"]
mod support;

use serde_json::{json, Value};
use support::{entry, git, ids, snapshot, status_of, Sandbox, FILLED_CLAUDE_MD};

const MILESTONE: &str = "[milestones.v1]\n";
const COMMANDS: &str =
    "[commands]\ntest = \"npm test\"\nsmoke = \"./smoke.sh\"\naudit = \"npm audit\"\n";
const MANUAL: &str = "[release]\nmanual_checks = [\"docs_updated\"]\n";

fn project(config_tail: &str) -> Sandbox {
    Sandbox::initialized(&format!("{MILESTONE}\n{config_tail}"))
}

fn passing_results() -> Value {
    json!([
        { "id": "tests_pass", "command": "npm test", "exit_code": 0, "duration_ms": 1200 },
        { "id": "smoke_pass", "command": "./smoke.sh", "exit_code": 0 },
        { "id": "dependencies_audited", "command": "npm audit", "exit_code": 0 },
    ])
}

fn record(sandbox: &Sandbox) -> Value {
    let path = sandbox.handoff().join("release/v1.json");
    serde_json::from_str(
        &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    )
    .unwrap()
}

// ===========================================================================
// handoff_release_check over JSON-RPC
// ===========================================================================

#[test]
fn the_tool_is_listed_with_its_arguments() {
    let sandbox = Sandbox::empty();
    let tool = sandbox.server().tool_definition("handoff_release_check");
    let schema = &tool["inputSchema"];
    assert_eq!(schema["required"], json!(["milestone"]));
    let properties = &schema["properties"];
    for name in ["project_dir", "milestone", "results", "confirm"] {
        assert!(properties[name].is_object(), "{name}");
    }
    assert_eq!(
        properties["results"]["items"]["properties"]["id"]["enum"],
        json!(["tests_pass", "smoke_pass", "dependencies_audited"])
    );
    assert_eq!(
        properties["results"]["items"]["required"],
        json!(["id", "command"])
    );
    let description = tool["description"].as_str().unwrap();
    assert!(
        description.contains("never runs a command") && description.contains("commands_to_run"),
        "the description says who runs the commands: {description}"
    );
}

// SPEC-ONB-040 AC1, AC3
#[test]
fn ac1_ac3_without_results_the_commands_are_skipped_and_handed_back() {
    let sandbox = project(&format!("{COMMANDS}{MANUAL}"));
    let before = snapshot(&sandbox.project);
    let report = sandbox
        .server()
        .call("handoff_release_check", json!({ "milestone": "v1" }));

    assert_eq!(report["milestone"], json!("v1"));
    assert_eq!(
        ids(&report["items"]),
        [
            "tasks_done",
            "no_open_bugs",
            "requirements_verified",
            "tests_pass",
            "smoke_pass",
            "secrets_untracked",
            "dependencies_audited",
            "manual:docs_updated",
        ]
    );
    for item in report["items"].as_array().unwrap() {
        let status = item["status"].as_str().expect("status");
        assert!(
            ["pass", "fail", "skip", "manual"].contains(&status),
            "{item}"
        );
        assert!(!item["detail"].as_str().unwrap_or("").is_empty(), "{item}");
    }
    assert_eq!(status_of(&report["items"], "tests_pass"), "skip");
    assert_eq!(status_of(&report["items"], "smoke_pass"), "skip");
    assert_eq!(report["ok"], json!(false));
    assert_eq!(
        report["commands_to_run"],
        json!([
            { "id": "tests_pass", "command": "npm test", "timeout_sec": 600 },
            { "id": "smoke_pass", "command": "./smoke.sh", "timeout_sec": 600 },
            { "id": "dependencies_audited", "command": "npm audit", "timeout_sec": 600 },
        ])
    );
    assert_eq!(
        snapshot(&sandbox.project),
        before,
        "a check without results or confirmations writes nothing"
    );
}

// SPEC-ONB-040 AC2
#[test]
fn ac2_an_unfinished_task_fails_tasks_done_and_is_referenced() {
    let sandbox = project("");
    let mut server = sandbox.server();
    for (id, status) in [("t1", "done"), ("t2", "in_progress")] {
        server.ok(
            "handoff_update_task",
            json!({ "task": {
                "id": id, "title": id, "status": status,
                "schedule": { "milestone": "v1", "estimate_hours": 1.0 },
            } }),
        );
    }
    let report = server.call("handoff_release_check", json!({ "milestone": "v1" }));
    let tasks_done = entry(&report["items"], "tasks_done");
    assert_eq!(tasks_done["status"], json!("fail"));
    assert_eq!(tasks_done["refs"], json!(["t2"]));
}

// SPEC-ONB-040 AC4 / done_criteria: a confirmation is recorded and the next
// call passes.
#[test]
fn ac4_a_manual_item_is_manual_until_confirmed_and_the_confirmation_is_kept() {
    let sandbox = project(MANUAL);
    let mut server = sandbox.server();
    let before = server.call("handoff_release_check", json!({ "milestone": "v1" }));
    assert_eq!(status_of(&before["items"], "manual:docs_updated"), "manual");
    assert!(!sandbox.handoff().join("release").exists());

    let confirming = server.call(
        "handoff_release_check",
        json!({ "milestone": "v1", "confirm": ["manual:docs_updated"] }),
    );
    assert_eq!(
        status_of(&confirming["items"], "manual:docs_updated"),
        "pass"
    );
    let confirmed_at = record(&sandbox)["manual"]["manual:docs_updated"]["confirmed_at"].clone();
    assert!(confirmed_at.is_string(), "{}", record(&sandbox));

    // A second call, in a new server process, with nothing passed in.
    drop(server);
    let again = sandbox
        .server()
        .call("handoff_release_check", json!({ "milestone": "v1" }));
    let manual = entry(&again["items"], "manual:docs_updated");
    assert_eq!(manual["status"], json!("pass"));
    assert_eq!(manual["checked_at"], confirmed_at);
}

// SPEC-ONB-040 AC5
#[test]
fn ac5_an_unknown_milestone_is_an_error() {
    let sandbox = project(COMMANDS);
    let error = sandbox
        .server()
        .err("handoff_release_check", json!({ "milestone": "v9" }));
    assert!(error.contains("Milestone 'v9' not found"), "{error}");
    assert!(error.contains("v1"), "{error}");
    assert!(!sandbox.handoff().join("release").exists());
}

// SPEC-ONB-040 AC6
#[test]
fn ac6_reported_results_are_recorded_with_status_exit_code_source_and_time() {
    let sandbox = project(COMMANDS);
    let report = sandbox.server().call(
        "handoff_release_check",
        json!({ "milestone": "v1", "results": [
            { "id": "tests_pass", "command": "npm test", "exit_code": 0 },
            { "id": "smoke_pass", "command": "./smoke.sh", "exit_code": 2, "output_tail": "boom" },
        ] }),
    );
    assert_eq!(status_of(&report["items"], "tests_pass"), "pass");
    assert_eq!(status_of(&report["items"], "smoke_pass"), "fail");
    assert_eq!(status_of(&report["items"], "dependencies_audited"), "skip");
    assert_eq!(ids(&report["commands_to_run"]), ["dependencies_audited"]);

    let record = record(&sandbox);
    let commands = record["commands"].as_object().expect("commands");
    assert_eq!(
        commands.keys().collect::<Vec<_>>(),
        ["smoke_pass", "tests_pass"],
        "only the commands a result was given for"
    );
    for (id, status, exit_code) in [("tests_pass", "pass", 0), ("smoke_pass", "fail", 2)] {
        let command = &commands[id];
        assert_eq!(command["status"], json!(status), "{command}");
        assert_eq!(command["exit_code"], json!(exit_code), "{command}");
        assert_eq!(command["source"], json!("reported"), "{command}");
        assert!(
            chrono::DateTime::parse_from_rfc3339(command["checked_at"].as_str().unwrap()).is_ok(),
            "{command}"
        );
    }
}

// SPEC-ONB-092 AC3 / SPEC-ONB-040: `ok` needs every item to pass.
#[test]
fn spec092_ac3_ok_is_true_only_without_a_single_skip() {
    let sandbox = project(&format!(
        "{COMMANDS}{MANUAL}\n[trace]\nprofile = \"minimal\"\n"
    ));
    sandbox.git(&["init", "-q"]);
    let mut server = sandbox.server();
    server.ok(
        "handoff_doc_save",
        json!({
            "slug": "reqs", "title": "Requirements", "layer": "requirement",
            "body": "# Requirements\n\n### REQ-1 Login\n\n- milestone: v1\n\nA user can log in.\n",
        }),
    );
    server.ok(
        "handoff_doc_save",
        json!({
            "slug": "acceptance", "title": "Acceptance", "layer": "acceptance",
            "body": "# Acceptance\n\n### AT-1 Login works\n\n- verifies: REQ-1\n\nLog in.\n",
        }),
    );
    server.ok(
        "handoff_update_task",
        json!({ "task": {
            "id": "t1", "title": "Login", "status": "done", "requirement_ids": ["REQ-1"],
            "schedule": { "milestone": "v1", "estimate_hours": 1.0 },
        } }),
    );
    server.ok(
        "handoff_trace_record",
        json!({ "results": [{ "item": "AT-1", "result": "pass" }] }),
    );

    let all = server.call(
        "handoff_release_check",
        json!({
            "milestone": "v1", "results": passing_results(),
            "confirm": ["manual:docs_updated"],
        }),
    );
    for item in all["items"].as_array().unwrap() {
        assert_eq!(item["status"], json!("pass"), "{item}");
    }
    assert_eq!(all["ok"], json!(true));

    // The same project; the audit result is not reported this time.
    let one_skip = server.call(
        "handoff_release_check",
        json!({ "milestone": "v1", "results": [
            { "id": "tests_pass", "command": "npm test", "exit_code": 0 },
            { "id": "smoke_pass", "command": "./smoke.sh", "exit_code": 0 },
        ] }),
    );
    let not_passing: Vec<(&str, &str)> = one_skip["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["status"] != json!("pass"))
        .map(|item| {
            (
                item["id"].as_str().unwrap(),
                item["status"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(not_passing, [("dependencies_audited", "skip")]);
    assert_eq!(
        one_skip["ok"],
        json!(false),
        "one skipped item keeps ok false"
    );
}

#[test]
fn requests_that_cannot_be_honored_are_errors_and_record_nothing() {
    let sandbox = project(&format!("{COMMANDS}{MANUAL}"));
    let before = snapshot(&sandbox.project);
    let mut server = sandbox.server();
    for (arguments, expected) in [
        (json!({}), "`milestone` is required"),
        (
            json!({ "milestone": "v1", "result": [] }),
            "unknown argument(s): result",
        ),
        (
            json!({ "milestone": "v1", "results": [{ "id": "tests_pass", "command": "npm test", "exitCode": 0 }] }),
            "results[0].exitCode",
        ),
        (
            json!({ "milestone": "v1", "results": [{ "id": "cmd_test", "command": "npm test", "exit_code": 0 }] }),
            "is not a command check",
        ),
        (
            json!({ "milestone": "v1", "confirm": ["manual:nope"] }),
            "is not a manual check",
        ),
    ] {
        let error = server.err("handoff_release_check", arguments.clone());
        assert!(error.contains(expected), "{arguments}: {error}");
    }
    assert_eq!(snapshot(&sandbox.project), before);
}

#[test]
fn a_project_that_is_not_initialized_is_an_error_and_stays_uninitialized() {
    let sandbox = Sandbox::empty();
    let error = sandbox
        .server()
        .err("handoff_release_check", json!({ "milestone": "v1" }));
    assert!(error.contains("handoff_init"), "{error}");
    assert!(!sandbox.handoff().exists());
}

// ===========================================================================
// secrets_untracked: tracked, untracked, not a repository (SPEC-ONB-050)
// ===========================================================================

/// The three situations of the done_criteria, each answered by both tools.
#[test]
fn spec050_env_tracked_untracked_and_outside_git() {
    // 1. `.env` is tracked.
    let tracked = project("");
    tracked.git(&["init", "-q"]);
    tracked.write(".env", "TOKEN=secret\n");
    tracked.write(".env.example", "TOKEN=\n");
    tracked.git(&["add", ".env", ".env.example"]);
    tracked.git(&["commit", "-q", "-m", "oops"]);

    // 2. `.env` exists but is ignored; only the template is tracked.
    let untracked = project("");
    untracked.git(&["init", "-q"]);
    untracked.write(".env", "TOKEN=secret\n");
    untracked.write(".env.example", "TOKEN=\n");
    untracked.write(".gitignore", ".env\n");
    untracked.git(&["add", ".gitignore", ".env.example"]);
    untracked.git(&["commit", "-q", "-m", "ok"]);

    // 3. Not a Git repository at all.
    let outside = project("");
    outside.write(".env", "TOKEN=secret\n");

    for (sandbox, expected) in [(&tracked, "fail"), (&untracked, "pass"), (&outside, "skip")] {
        let mut server = sandbox.server();
        let release = server.call("handoff_release_check", json!({ "milestone": "v1" }));
        let doctor = server.call(
            "handoff_doctor",
            json!({ "checks": ["handoff_initialized", "secrets_untracked"] }),
        );
        for (tool, secrets) in [
            (
                "handoff_release_check",
                entry(&release["items"], "secrets_untracked"),
            ),
            (
                "handoff_doctor",
                entry(&doctor["checks"], "secrets_untracked"),
            ),
        ] {
            assert_eq!(secrets["status"], json!(expected), "{tool}: {secrets}");
            let detail = secrets["detail"].as_str().unwrap();
            assert!(
                detail.contains("contents were not scanned"),
                "{tool}: {detail}"
            );
            match expected {
                "fail" => {
                    assert!(detail.contains("`.env`"), "{tool}: {detail}");
                    assert!(!detail.contains(".env.example"), "{tool}: {detail}");
                }
                "skip" => assert!(
                    detail.contains("not in a Git repository"),
                    "{tool}: {detail}"
                ),
                _ => {}
            }
        }
        assert_eq!(
            doctor["ready"],
            json!(expected != "fail"),
            "only a tracked secrets file lowers ready: {doctor}"
        );
        assert_eq!(release["ok"], json!(false));
    }
}

// SPEC-ONB-050 AC7
#[test]
fn spec050_ac7_configured_patterns_replace_the_defaults() {
    let sandbox = project("[onboarding]\nsecret_file_patterns = [\"*.tfstate\"]\n");
    sandbox.git(&["init", "-q"]);
    sandbox.write(".env", "x");
    sandbox.write("infra/prod.tfstate", "x");
    sandbox.git(&["add", ".env"]);
    let mut server = sandbox.server();
    let report = server.call("handoff_release_check", json!({ "milestone": "v1" }));
    assert_eq!(status_of(&report["items"], "secrets_untracked"), "pass");

    sandbox.git(&["add", "infra"]);
    let report = server.call("handoff_release_check", json!({ "milestone": "v1" }));
    let secrets = entry(&report["items"], "secrets_untracked");
    assert_eq!(secrets["status"], json!("fail"));
    assert!(secrets["detail"]
        .as_str()
        .unwrap()
        .contains("`infra/prod.tfstate`"));
}

// SPEC-ONB-050 AC5, AC6
#[test]
fn spec050_ac5_ac6_the_audit_is_skipped_until_reported_and_fails_on_a_nonzero_exit() {
    let undefined = project("[commands]\ntest = \"npm test\"\n");
    let report = undefined
        .server()
        .call("handoff_release_check", json!({ "milestone": "v1" }));
    let audit = entry(&report["items"], "dependencies_audited");
    assert_eq!(audit["status"], json!("skip"));
    assert!(audit["detail"].as_str().unwrap().contains("not defined"));

    let defined = project(COMMANDS);
    let mut server = defined.server();
    let report = server.call(
        "handoff_release_check",
        json!({ "milestone": "v1", "results": [{
            "id": "dependencies_audited", "command": "npm audit", "exit_code": 1,
            "output_tail": "2 high severity vulnerabilities",
        }] }),
    );
    let audit = entry(&report["items"], "dependencies_audited");
    assert_eq!(audit["status"], json!("fail"));
    assert!(audit["detail"]
        .as_str()
        .unwrap()
        .contains("2 high severity vulnerabilities"));
}

// ===========================================================================
// handoff_doctor: all 13 checks (SPEC-ONB-002), audit handed out and judged
// ===========================================================================

#[test]
fn the_doctor_reports_all_thirteen_checks_and_judges_a_reported_audit() {
    let sandbox = project("[commands]\ntest = \"true\"\naudit = \"npm audit\"\n");
    sandbox.write("CLAUDE.md", FILLED_CLAUDE_MD);
    sandbox.git(&["init", "-q"]);
    let mut server = sandbox.server();
    let report = server.call("handoff_doctor", json!({}));
    assert_eq!(
        ids(&report["checks"]),
        [
            "handoff_initialized",
            "mcp_registered",
            "claude_md_sections",
            "claude_md_todo",
            "commands_defined",
            "cmd_install",
            "cmd_build",
            "cmd_test",
            "cmd_lint",
            "cmd_typecheck",
            "smoke_defined",
            "secrets_untracked",
            "dependencies_audited",
        ]
    );
    assert_eq!(status_of(&report["checks"], "secrets_untracked"), "pass");
    assert_eq!(status_of(&report["checks"], "dependencies_audited"), "skip");
    assert_eq!(
        report["commands_to_run"],
        json!([
            { "id": "cmd_test", "command": "true", "timeout_sec": 600 },
            { "id": "dependencies_audited", "command": "npm audit", "timeout_sec": 600 },
        ])
    );

    let judged = server.call(
        "handoff_doctor",
        json!({ "results": [
            { "id": "cmd_test", "command": "true", "exit_code": 0 },
            { "id": "dependencies_audited", "command": "npm audit", "exit_code": 1,
              "output_tail": "1 critical vulnerability" },
        ] }),
    );
    let audit = entry(&judged["checks"], "dependencies_audited");
    assert_eq!(audit["status"], json!("fail"));
    assert_eq!(audit["exit_code"], json!(1));
    assert_eq!(audit["source"], json!("reported"));
    assert!(audit["detail"]
        .as_str()
        .unwrap()
        .contains("1 critical vulnerability"));
    assert_eq!(judged["ready"], json!(false));

    // The terminal gives the same thirteen.
    let cli = sandbox.cli(&["setup", "--check", "--json"]).json();
    assert_eq!(ids(&cli["checks"]), ids(&report["checks"]));
}

/// `setup --check --run` runs the audit command with the others and judges
/// it, as the tool does for a reported result.
#[test]
fn check_run_runs_and_judges_the_audit_command() {
    let sandbox = project(
        "[commands]\ntest = \"true\"\naudit = \"echo 2 vulnerabilities; touch audited; exit 1\"\n",
    );
    sandbox.write("CLAUDE.md", FILLED_CLAUDE_MD);
    let output = sandbox.cli(&["setup", "--check", "--run", "--json"]);
    let report = output.json();
    assert!(sandbox.project.join("audited").exists());
    let audit = entry(&report["checks"], "dependencies_audited");
    assert_eq!(audit["status"], json!("fail"));
    assert_eq!(audit["source"], json!("executed"));
    assert!(audit["detail"]
        .as_str()
        .unwrap()
        .contains("2 vulnerabilities"));
    assert_eq!(report["ready"], json!(false));
    assert_eq!(output.code, 1);
    assert!(report["commands_to_run"].as_array().unwrap().is_empty());
}

// ===========================================================================
// No process is started (SPEC-ONB-040 AC7)
// ===========================================================================

/// The server runs with a `PATH` that holds nothing but tripwires: a shell,
/// `git` or a configured command it tried to start would leave a line.
#[test]
fn ac7_the_tool_starts_no_process() {
    let ran = "command-ran";
    let sandbox = project(&format!(
        "[commands]\ntest = \"touch {ran}\"\nsmoke = \"handoff-tripwire smoke\"\naudit = \"touch {ran}\"\n\n{MANUAL}"
    ));
    sandbox.git(&["init", "-q"]);
    sandbox.write(".env", "x");
    sandbox.git(&["add", ".env"]);
    // A linked worktree of the repository: finding its `.handoff/` and its
    // index the usual way would ask `git`.
    sandbox.write("a.txt", "x");
    sandbox.git(&["add", "a.txt"]);
    sandbox.git(&["commit", "-q", "-m", "x"]);
    let linked = sandbox.root.path().join("linked");
    sandbox.git(&["worktree", "add", "-q", linked.to_str().unwrap()]);
    let linked = std::fs::canonicalize(&linked).unwrap();
    assert!(!linked.join(".handoff").exists());

    {
        // Requirement documents and a task, so every item has work to do.
        let mut setup = sandbox.server();
        setup.ok(
            "handoff_doc_save",
            json!({
                "slug": "reqs", "title": "Requirements", "layer": "requirement",
                "body": "# Requirements\n\n### REQ-1 Login\n\n- milestone: v1\n\nA.\n",
            }),
        );
        setup.ok(
            "handoff_update_task",
            json!({ "task": {
                "id": "t1", "title": "Login", "status": "todo", "labels": ["bug"],
                "schedule": { "milestone": "v1" },
            } }),
        );
    }

    let tripwires = sandbox.tripwires();
    let mut command = sandbox.command();
    command.env("PATH", &tripwires.dir);
    let mut server = support::Server::start(command, &sandbox.project);
    // Anything the server starts on its own before the first tool call is
    // not what this test is about.
    server.request("tools/list", json!({}));
    let spawned_before = tripwires.spawned();

    let first = server.call("handoff_release_check", json!({ "milestone": "v1" }));
    assert_eq!(ids(&first["commands_to_run"]).len(), 3, "{first}");
    assert_eq!(status_of(&first["items"], "secrets_untracked"), "fail");
    assert_eq!(status_of(&first["items"], "requirements_verified"), "fail");
    let touch = format!("touch {ran}");
    let judged = server.call(
        "handoff_release_check",
        json!({
            "milestone": "v1",
            "results": [
                { "id": "tests_pass", "command": touch, "exit_code": 0 },
                { "id": "smoke_pass", "command": "something else", "exit_code": 0 },
                { "id": "dependencies_audited", "command": touch, "error": "timed out after 600s" },
            ],
            "confirm": ["manual:docs_updated"],
        }),
    );
    assert_eq!(status_of(&judged["items"], "tests_pass"), "pass");
    assert_eq!(status_of(&judged["items"], "smoke_pass"), "fail");
    let _ = server.call_text("handoff_release_check", json!({ "milestone": "nope" }));
    // From the linked worktree, which shares the primary's `.handoff/`.
    server.project = linked.clone();
    let from_worktree = server.call("handoff_release_check", json!({ "milestone": "v1" }));
    assert_eq!(
        status_of(&from_worktree["items"], "manual:docs_updated"),
        "pass",
        "the worktree reads the primary checkout's record: {from_worktree}"
    );

    assert_eq!(
        tripwires.spawned(),
        spawned_before,
        "handoff_release_check started a process"
    );
    assert!(!sandbox.project.join(ran).exists());
    assert!(!linked.join(ran).exists());
    assert!(
        !linked.join(".handoff").exists(),
        "no link is created in the worktree"
    );

    // The tripwire itself works: the CLI with --run, which does run
    // commands, trips it.
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
// handoff-mcp release-check
// ===========================================================================

#[test]
fn the_cli_prints_the_same_check_as_the_tool() {
    let sandbox = project(&format!("{COMMANDS}{MANUAL}"));
    sandbox.git(&["init", "-q"]);
    let before = snapshot(&sandbox.handoff());
    let output = sandbox.cli(&["release-check", "v1"]);
    assert_eq!(output.code, 1, "not ok: {}", output.stdout);
    let from_cli = output.json();
    let from_tool = sandbox
        .server()
        .call("handoff_release_check", json!({ "milestone": "v1" }));
    assert_eq!(from_cli, from_tool);
    assert_eq!(
        snapshot(&sandbox.handoff()),
        before,
        "without --run or --confirm nothing is written"
    );
}

#[test]
fn run_executes_the_commands_through_the_shell_and_records_them_as_executed() {
    let sandbox = project(
        "[commands]\ntest = \"echo tested > tested.txt && test -s tested.txt\"\nsmoke = \"exit 4\"\n",
    );
    let output = sandbox.cli(&["release-check", "v1", "--run"]);
    let report = output.json();
    assert!(
        output.stderr.contains("running tests_pass")
            && output.stderr.contains("running smoke_pass"),
        "progress goes to stderr: {}",
        output.stderr
    );
    assert_eq!(
        std::fs::read_to_string(sandbox.project.join("tested.txt")).unwrap(),
        "tested\n",
        "the command ran in the project directory, through the shell"
    );
    let tests = entry(&report["items"], "tests_pass");
    assert_eq!(tests["status"], json!("pass"));
    assert_eq!(tests["source"], json!("executed"));
    let smoke = entry(&report["items"], "smoke_pass");
    assert_eq!(smoke["status"], json!("fail"));
    assert!(smoke["detail"].as_str().unwrap().contains("exit code 4"));
    assert_eq!(status_of(&report["items"], "dependencies_audited"), "skip");
    assert!(report["commands_to_run"].as_array().unwrap().is_empty());
    assert_eq!(output.code, 1);

    let record = record(&sandbox);
    assert_eq!(
        record["commands"]["tests_pass"]["source"],
        json!("executed")
    );
    assert_eq!(record["commands"]["tests_pass"]["exit_code"], json!(0));
    assert_eq!(record["commands"]["smoke_pass"]["exit_code"], json!(4));
    assert_eq!(record["commands"]["smoke_pass"]["status"], json!("fail"));
}

#[test]
fn run_stops_a_command_that_exceeds_the_time_limit() {
    let sandbox =
        project("[commands]\ntest = \"sleep 30\"\n\n[onboarding]\ncommand_timeout_sec = 1\n");
    let started = std::time::Instant::now();
    let report = sandbox.cli(&["release-check", "v1", "--run"]).json();
    assert!(started.elapsed() < std::time::Duration::from_secs(15));
    let tests = entry(&report["items"], "tests_pass");
    assert_eq!(tests["status"], json!("fail"));
    assert!(
        tests["detail"]
            .as_str()
            .unwrap()
            .contains("timed out after 1s"),
        "{tests}"
    );
}

#[test]
fn the_exit_code_is_zero_only_when_the_check_is_ok() {
    // Nothing to skip: no requirement, so that item alone stays skipped.
    let sandbox = project("[commands]\ntest = \"true\"\nsmoke = \"true\"\naudit = \"true\"\n");
    sandbox.git(&["init", "-q"]);
    let output = sandbox.cli(&["release-check", "v1", "--run"]);
    let report = output.json();
    let not_passing: Vec<&str> = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["status"] != json!("pass"))
        .map(|item| item["id"].as_str().unwrap())
        .collect();
    assert_eq!(not_passing, ["requirements_verified"]);
    assert_eq!(output.code, 1, "one skipped item is not ok");

    // With a verified requirement, every item passes.
    sandbox.append_config("[trace]\nprofile = \"minimal\"\n");
    let mut server = sandbox.server();
    server.ok(
        "handoff_doc_save",
        json!({
            "slug": "reqs", "title": "Requirements", "layer": "requirement", "milestone": "v1",
            "body": "# Requirements\n\n### REQ-1 Login\n\n- test: tests/login.rs::works\n\nA.\n",
        }),
    );
    server.ok(
        "handoff_update_task",
        json!({ "task": {
            "id": "t1", "title": "Login", "status": "done", "requirement_ids": ["REQ-1"],
            "schedule": { "milestone": "v1", "estimate_hours": 1.0 },
        } }),
    );
    server.ok(
        "handoff_trace_record",
        json!({ "results": [{ "item": "REQ-1", "result": "pass" }] }),
    );
    drop(server);
    let output = sandbox.cli(&["release-check", "v1", "--run"]);
    let report = output.json();
    assert_eq!(report["ok"], json!(true), "{report}");
    assert_eq!(output.code, 0);
}

#[test]
fn the_cli_confirms_manual_items() {
    let sandbox = project(MANUAL);
    let report = sandbox
        .cli(&["release-check", "v1", "--confirm", "manual:docs_updated"])
        .json();
    assert_eq!(status_of(&report["items"], "manual:docs_updated"), "pass");
    assert!(record(&sandbox)["manual"]["manual:docs_updated"].is_object());
}

#[test]
fn cli_usage_errors_exit_with_two_and_run_nothing() {
    let sandbox = project("[commands]\ntest = \"touch ran\"\n");
    for (args, expected) in [
        (vec!["release-check"], "milestone"),
        (vec!["release-check", "v9"], "Milestone 'v9' not found"),
        (
            vec!["release-check", "v1", "--rum"],
            "unknown argument '--rum'",
        ),
        (
            vec!["release-check", "v1", "v2"],
            "unexpected argument 'v2'",
        ),
        (vec!["release-check", "v1", "--confirm"], "--confirm needs"),
        (
            vec!["release-check", "v1", "--run", "--confirm", "manual:nope"],
            "cannot be confirmed",
        ),
    ] {
        let output = sandbox.cli(&args);
        assert_eq!(output.code, 2, "{args:?}: {}", output.stdout);
        let error = output.json()["error"].as_str().unwrap_or("").to_string();
        assert!(error.contains(expected), "{args:?}: {error}");
    }
    assert!(
        !sandbox.project.join("ran").exists(),
        "a rejected request must not run a command"
    );
}

#[test]
fn the_cli_honors_project_dir_and_shows_help() {
    let sandbox = project(MANUAL);
    let elsewhere = Sandbox::empty();
    let mut command = elsewhere.command();
    command.args([
        "release-check",
        "v1",
        "--project-dir",
        sandbox.project.to_str().unwrap(),
    ]);
    let output = Sandbox::output(&mut command);
    assert_eq!(output.json()["milestone"], json!("v1"));

    for args in [["release-check", "--help"], ["release-check", "-h"]] {
        let help = sandbox.cli(&args);
        assert_eq!(help.code, 0);
        assert!(
            help.stdout.contains("release-check <milestone>") && help.stdout.contains("--run"),
            "{}",
            help.stdout
        );
    }
    let top = sandbox.cli(&["--help"]);
    assert!(top.stdout.contains("release-check"), "{}", top.stdout);
}

/// A linked worktree answers from the primary checkout's `.handoff/`, like
/// `handoff_doctor`, and for its own tracked files.
#[test]
fn a_linked_worktree_uses_the_primary_checkouts_handoff_directory() {
    let sandbox = project(MANUAL);
    sandbox.git(&["init", "-q"]);
    sandbox.write("a.txt", "x");
    sandbox.git(&["add", "a.txt"]);
    sandbox.git(&["commit", "-q", "-m", "x"]);
    let linked = sandbox.root.path().join("linked");
    sandbox.git(&["worktree", "add", "-q", linked.to_str().unwrap()]);
    std::fs::write(linked.join("leak.pem"), "x").unwrap();
    git(&linked, &["add", "leak.pem"]);

    let mut server = sandbox.server();
    let primary = server.call("handoff_release_check", json!({ "milestone": "v1" }));
    assert_eq!(status_of(&primary["items"], "secrets_untracked"), "pass");
    server.project = std::fs::canonicalize(&linked).unwrap();
    let worktree = server.call(
        "handoff_release_check",
        json!({ "milestone": "v1", "confirm": ["manual:docs_updated"] }),
    );
    let secrets = entry(&worktree["items"], "secrets_untracked");
    assert_eq!(secrets["status"], json!("fail"), "{secrets}");
    assert!(secrets["detail"].as_str().unwrap().contains("`leak.pem`"));
    assert!(
        record(&sandbox)["manual"]["manual:docs_updated"].is_object(),
        "the confirmation is recorded in the primary checkout"
    );
}
