//! Real-binary E2E for the release guard (SPEC-ONB-041): what
//! `handoff_update_milestone(status: "released")` does under
//! `[onboarding] release_guard = "off" | "warn" | "block"`, with and without
//! `force`.
//!
//! Unix only: the fixtures use shell command lines and tripwire scripts.
#![cfg(unix)]

use crate::support::onboarding_e2e as support;

use serde_json::{json, Value};
use support::{snapshot, Sandbox, Server};

const COMMANDS: &str =
    "[commands]\ntest = \"npm test\"\nsmoke = \"./smoke.sh\"\naudit = \"npm audit\"\n";
const MANUAL: &str = "[release]\nmanual_checks = [\"docs_updated\"]\n";

/// What `handoff_update_milestone` answered before the guard existed.
const PLAIN_REPLY: &str = "Updated milestone 'v1'";

/// A project with milestone `v1`; `guard` is the `[onboarding]` body (empty
/// for a project without the section).
fn project(guard: &str, config_tail: &str) -> Sandbox {
    let onboarding = if guard.is_empty() {
        String::new()
    } else {
        format!("[onboarding]\n{guard}\n\n")
    };
    Sandbox::initialized(&format!("[milestones.v1]\n\n{onboarding}{config_tail}"))
}

fn task(server: &mut Server, id: &str, status: &str) {
    server.ok(
        "handoff_update_task",
        json!({ "task": {
            "id": id, "title": id, "status": status,
            "schedule": { "milestone": "v1", "estimate_hours": 1.0 },
        } }),
    );
}

fn release(server: &mut Server, force: bool) -> Result<String, String> {
    let mut arguments = json!({ "name": "v1", "status": "released" });
    if force {
        arguments["force"] = json!(true);
    }
    server.call_text("handoff_update_milestone", arguments)
}

fn status(server: &mut Server) -> String {
    let listed = server.call("handoff_list_milestones", json!({}));
    listed["milestones"]["v1"]["status"]
        .as_str()
        .expect("status")
        .to_string()
}

fn passing_results() -> Value {
    json!([
        { "id": "tests_pass", "command": "npm test", "exit_code": 0 },
        { "id": "smoke_pass", "command": "./smoke.sh", "exit_code": 0 },
        { "id": "dependencies_audited", "command": "npm audit", "exit_code": 0 },
    ])
}

/// Makes every item of `v1`'s pre-release check pass: a clean Git
/// repository, one finished task, one verified requirement, passing command
/// results on record and the manual item confirmed.
fn make_everything_pass(sandbox: &Sandbox, server: &mut Server) {
    sandbox.git(&["init", "-q"]);
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
    let check = server.call(
        "handoff_release_check",
        json!({
            "milestone": "v1", "results": passing_results(),
            "confirm": ["manual:docs_updated"],
        }),
    );
    assert_eq!(check["ok"], json!(true), "fixture: {check}");
}

fn warning_lines(reply: &str) -> Vec<&str> {
    reply
        .lines()
        .filter(|line| line.starts_with("Warning: "))
        .collect()
}

// SPEC-ONB-041 AC1
#[test]
fn ac1_warn_releases_and_warns_about_the_unfinished_task() {
    let sandbox = project("release_guard = \"warn\"", "");
    let mut server = sandbox.server();
    task(&mut server, "t1", "done");
    task(&mut server, "t2", "in_progress");

    let reply = release(&mut server, false).expect("warn must not refuse");
    assert_eq!(status(&mut server), "released");
    assert!(reply.starts_with(PLAIN_REPLY), "{reply}");
    let warnings = warning_lines(&reply);
    assert!(
        warnings
            .iter()
            .any(|line| line.contains("tasks_done [fail]") && line.contains("t2 (in_progress)")),
        "{reply}"
    );
    assert!(
        warnings[0].contains("did not pass") && warnings[0].contains("release_guard = \"warn\""),
        "{reply}"
    );
}

// SPEC-ONB-041 AC2
#[test]
fn ac2_block_refuses_and_changes_nothing() {
    let sandbox = project("release_guard = \"block\"", "");
    let mut server = sandbox.server();
    task(&mut server, "t1", "done");
    task(&mut server, "t2", "in_progress");
    server.ok(
        "handoff_update_milestone",
        json!({ "name": "v1", "status": "active" }),
    );
    let before = snapshot(&sandbox.handoff());

    let error = server
        .call_text(
            "handoff_update_milestone",
            json!({ "name": "v1", "status": "released", "description": "shipped" }),
        )
        .expect_err("block must refuse");
    assert!(error.contains("Milestone 'v1' was not released"), "{error}");
    assert!(error.contains("tasks_done [fail]"), "{error}");
    assert!(error.contains("release_guard = \"block\""), "{error}");
    assert!(error.contains("force: true"), "{error}");
    assert_eq!(status(&mut server), "active", "the status did not change");
    assert_eq!(
        snapshot(&sandbox.handoff()),
        before,
        "a refused release writes nothing, not even the other fields of the call"
    );
}

// SPEC-ONB-041 AC3
#[test]
fn ac3_block_with_force_releases_and_still_warns() {
    let sandbox = project("release_guard = \"block\"", "");
    let mut server = sandbox.server();
    task(&mut server, "t1", "in_progress");

    let reply = release(&mut server, true).expect("force overrides block");
    assert_eq!(status(&mut server), "released");
    let warnings = warning_lines(&reply);
    assert!(
        warnings[0].contains("force: true overrides"),
        "the override is visible: {reply}"
    );
    assert!(
        warnings
            .iter()
            .any(|line| line.contains("tasks_done [fail]")),
        "{reply}"
    );
}

// SPEC-ONB-041 AC4
#[test]
fn ac4_when_every_item_passes_the_reply_carries_no_release_warning() {
    for guard in ["warn", "block"] {
        let sandbox = project(
            &format!("release_guard = \"{guard}\""),
            &format!("{COMMANDS}{MANUAL}\n[trace]\nprofile = \"minimal\"\n"),
        );
        let mut server = sandbox.server();
        make_everything_pass(&sandbox, &mut server);

        let reply = release(&mut server, false).expect("nothing to refuse");
        assert_eq!(reply, PLAIN_REPLY, "{guard}");
        assert_eq!(status(&mut server), "released", "{guard}");
    }
}

// SPEC-ONB-041 AC5
#[test]
fn ac5_off_or_absent_answers_exactly_as_before() {
    for guard in [
        "",
        "release_guard = \"off\"",
        // A value this version does not know reads as off (SPEC-ONB-091).
        "release_guard = \"strict\"",
        "next_step = true",
    ] {
        // Everything a guard would object to: an unfinished bug task, a
        // tracked secrets file, commands never run, a manual item waiting.
        let sandbox = project(guard, &format!("{COMMANDS}{MANUAL}"));
        sandbox.git(&["init", "-q"]);
        sandbox.write(".env", "TOKEN=x\n");
        sandbox.git(&["add", ".env"]);
        let mut server = sandbox.server();
        server.ok(
            "handoff_update_task",
            json!({ "task": {
                "id": "t1", "title": "t1", "status": "todo", "labels": ["bug"],
                "schedule": { "milestone": "v1", "estimate_hours": 1.0 },
            } }),
        );
        let config = sandbox.handoff().join("config.toml");
        let before = std::fs::read_to_string(&config).unwrap();

        let reply = release(&mut server, false).expect("no guard");
        assert_eq!(reply, PLAIN_REPLY, "`{guard}`");
        assert_eq!(status(&mut server), "released", "`{guard}`");
        // The only change to the config is the status line.
        let after = std::fs::read_to_string(&config).unwrap();
        assert_eq!(
            after,
            before.replace(
                "[milestones.v1]\n",
                "[milestones.v1]\nstatus = \"released\"\n"
            ),
            "`{guard}`"
        );
        assert!(
            !sandbox.handoff().join("release").exists(),
            "`{guard}`: releasing writes no release record"
        );
    }
}

// SPEC-ONB-041 AC6
#[test]
fn ac6_without_a_record_the_commands_are_reported_as_not_verified() {
    let sandbox = project("release_guard = \"warn\"", COMMANDS);
    let mut server = sandbox.server();
    let reply = release(&mut server, false).expect("warn must not refuse");
    assert_eq!(status(&mut server), "released");
    let warnings = warning_lines(&reply);
    for id in ["tests_pass", "smoke_pass", "dependencies_audited"] {
        assert!(
            warnings
                .iter()
                .any(|line| line
                    .contains(&format!("{id} [skip] not verified: no result is on record"))),
            "{id}: {reply}"
        );
    }
    assert!(
        !reply.contains("from the record"),
        "nothing was on record, so no record is dated: {reply}"
    );
    assert!(
        !sandbox.handoff().join("release").exists(),
        "the guard reads the record; it never creates one"
    );
}

/// The command items come from the record of the last
/// `handoff_release_check`, and the reply dates that record.
#[test]
fn recorded_command_results_are_used_and_dated() {
    let sandbox = project("release_guard = \"block\"", COMMANDS);
    sandbox.git(&["init", "-q"]);
    let mut server = sandbox.server();
    let check = server.call(
        "handoff_release_check",
        json!({ "milestone": "v1", "results": [
            { "id": "tests_pass", "command": "npm test", "exit_code": 0 },
            { "id": "smoke_pass", "command": "./smoke.sh", "exit_code": 3 },
        ] }),
    );
    let checked_at = |id: &str| -> String {
        support::entry(&check["items"], id)["checked_at"]
            .as_str()
            .expect("checked_at")
            .to_string()
    };
    let record = sandbox.handoff().join("release/v1.json");
    let recorded = std::fs::read(&record).unwrap();

    let error = release(&mut server, false).expect_err("a recorded failure blocks");
    assert!(
        error.contains(&format!(
            "- smoke_pass [fail] result on record from {}: exit code 3",
            checked_at("smoke_pass")
        )),
        "{error}"
    );
    assert!(
        error.contains("- dependencies_audited [skip] not verified: no result is on record"),
        "{error}"
    );
    assert!(
        !error.contains("- tests_pass"),
        "the recorded pass is accepted: {error}"
    );
    assert!(
        error.contains(&format!(
            "tests_pass (pass, checked {})",
            checked_at("tests_pass")
        )) && error.contains(&format!(
            "smoke_pass (fail, checked {})",
            checked_at("smoke_pass")
        )),
        "every record that was read is dated: {error}"
    );
    assert_eq!(
        std::fs::read(&record).unwrap(),
        recorded,
        "the guard does not touch the record"
    );

    // A record only speaks for the command it was made with.
    let config = sandbox.handoff().join("config.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    std::fs::write(
        &config,
        text.replace("test = \"npm test\"", "test = \"npm run test:all\""),
    )
    .unwrap();
    let error = release(&mut server, false).expect_err("still blocked");
    assert!(
        error.contains("- tests_pass [skip] not verified: the result on record"),
        "{error}"
    );
}

/// Only the move to `released` is guarded.
#[test]
fn other_status_changes_and_an_already_released_milestone_are_not_checked() {
    let sandbox = project("release_guard = \"block\"", COMMANDS);
    let mut server = sandbox.server();
    task(&mut server, "t1", "todo");
    for target in ["active", "completed", "frozen"] {
        let reply = server
            .call_text(
                "handoff_update_milestone",
                json!({ "name": "v1", "status": target }),
            )
            .unwrap_or_else(|e| panic!("{target}: {e}"));
        assert_eq!(reply, PLAIN_REPLY, "{target}");
    }
    let reply = server
        .call_text(
            "handoff_update_milestone",
            json!({ "name": "v1", "description": "no status in this call" }),
        )
        .expect("no status change");
    assert_eq!(reply, PLAIN_REPLY);

    release(&mut server, true).expect("forced");
    // Already released: editing it, or naming the same status again, is not
    // a release.
    for arguments in [
        json!({ "name": "v1", "status": "released" }),
        json!({ "name": "v1", "status": "released", "date": "2026-12-01" }),
        json!({ "name": "v1", "color": "#ff0000" }),
    ] {
        let reply = server
            .call_text("handoff_update_milestone", arguments.clone())
            .unwrap_or_else(|e| panic!("{arguments}: {e}"));
        assert_eq!(reply, PLAIN_REPLY, "{arguments}");
    }
    // Leaving it is not guarded either (it needs force for the lifecycle).
    let reply = server
        .call_text(
            "handoff_update_milestone",
            json!({ "name": "v1", "status": "cancelled" }),
        )
        .expect("cancelled is always allowed");
    assert_eq!(reply, PLAIN_REPLY);
}

/// A transition the lifecycle rejects is rejected for that reason, before
/// the guard is asked.
#[test]
fn a_lifecycle_violation_is_reported_as_such() {
    let sandbox = project("release_guard = \"block\"", "");
    let mut server = sandbox.server();
    task(&mut server, "t1", "todo");
    server.ok(
        "handoff_update_milestone",
        json!({ "name": "v1", "status": "cancelled" }),
    );
    let error = release(&mut server, false).expect_err("cancelled -> released needs force");
    assert!(
        error.contains("not a forward lifecycle transition"),
        "{error}"
    );
    // With force the lifecycle gives way and so does the guard.
    let reply = release(&mut server, true).expect("forced");
    assert!(reply.contains("tasks_done [fail]"), "{reply}");
    assert_eq!(status(&mut server), "released");
}

/// A check that cannot be carried out is not a pass.
#[test]
fn a_broken_record_blocks_under_block_and_warns_under_warn() {
    for (guard, refused) in [("block", true), ("warn", false)] {
        let sandbox = project(&format!("release_guard = \"{guard}\""), COMMANDS);
        std::fs::create_dir_all(sandbox.handoff().join("release")).unwrap();
        std::fs::write(sandbox.handoff().join("release/v1.json"), "{ broken").unwrap();
        let mut server = sandbox.server();
        match release(&mut server, false) {
            Err(error) => {
                assert!(refused, "{guard}: {error}");
                assert!(
                    error.contains("the pre-release check could not be carried out")
                        && error.contains("v1.json"),
                    "{error}"
                );
                assert_eq!(status(&mut server), "planning");
            }
            Ok(reply) => {
                assert!(!refused, "{guard}: {reply}");
                assert!(
                    reply.contains("Warning: release check could not be carried out")
                        && reply.contains("v1.json"),
                    "{reply}"
                );
                assert_eq!(status(&mut server), "released");
            }
        }
        assert_eq!(
            std::fs::read(sandbox.handoff().join("release/v1.json")).unwrap(),
            b"{ broken"
        );
    }
}

/// The dependency warnings of the same call are kept, before the guard's.
#[test]
fn release_warnings_follow_the_other_warnings_of_the_call() {
    let sandbox = project("release_guard = \"warn\"", "");
    let mut server = sandbox.server();
    task(&mut server, "t1", "todo");
    let reply = server
        .call_text(
            "handoff_update_milestone",
            json!({ "name": "v1", "status": "released", "depends_on": ["nope"] }),
        )
        .expect("warn");
    let warnings = warning_lines(&reply);
    assert!(warnings[0].contains("depends_on 'nope'"), "{reply}");
    assert!(warnings[1].contains("release check:"), "{reply}");
}

// SPEC-ONB-041 AC7
#[test]
fn ac7_releasing_starts_no_process() {
    let ran = "command-ran";
    for (guard, force) in [("warn", false), ("block", false), ("block", true)] {
        let sandbox = project(
            &format!("release_guard = \"{guard}\""),
            &format!(
                "[commands]\ntest = \"touch {ran}\"\nsmoke = \"handoff-tripwire smoke\"\naudit = \"touch {ran}\"\n\n{MANUAL}"
            ),
        );
        sandbox.git(&["init", "-q"]);
        sandbox.write(".env", "x");
        sandbox.git(&["add", ".env"]);
        {
            // Requirement documents, tasks and a record, so every item has
            // something to read.
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
            setup.call(
                "handoff_release_check",
                json!({ "milestone": "v1", "results": [
                    { "id": "tests_pass", "command": format!("touch {ran}"), "exit_code": 0 },
                ] }),
            );
        }

        let tripwires = sandbox.tripwires();
        let mut command = sandbox.command();
        command.env("PATH", &tripwires.dir);
        let mut server = Server::start(command, &sandbox.project);
        // Anything the server starts on its own before the first tool call
        // is not what this test is about.
        server.request("tools/list", json!({}));
        let spawned_before = tripwires.spawned();
        let record = std::fs::read(sandbox.handoff().join("release/v1.json")).unwrap();

        let outcome = release(&mut server, force);
        match (guard, force) {
            ("block", false) => {
                let error = outcome.expect_err("blocked");
                assert!(error.contains("secrets_untracked [fail]"), "{error}");
                assert!(error.contains("requirements_verified [fail]"), "{error}");
            }
            _ => {
                let reply = outcome.expect("released");
                assert!(reply.contains("smoke_pass [skip]"), "{reply}");
                assert!(reply.contains("tests_pass (pass, checked "), "{reply}");
            }
        }

        assert_eq!(
            tripwires.spawned(),
            spawned_before,
            "{guard} force={force}: releasing a milestone started a process"
        );
        assert!(!sandbox.project.join(ran).exists(), "{guard}");
        assert_eq!(
            std::fs::read(sandbox.handoff().join("release/v1.json")).unwrap(),
            record,
            "{guard}: the record is only read"
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
}

/// The terminal goes through the same guard: a refused release is an error
/// exit, a warned one prints its warnings.
#[test]
fn the_cli_releases_through_the_same_guard() {
    let sandbox = project("release_guard = \"block\"", "");
    {
        let mut server = sandbox.server();
        task(&mut server, "t1", "todo");
    }
    let refused = sandbox.cli(&[
        "milestone",
        "update",
        "--name",
        "v1",
        "--status",
        "released",
    ]);
    assert_ne!(refused.code, 0, "{}", refused.stdout);
    let said = format!("{}{}", refused.stdout, refused.stderr);
    assert!(said.contains("Milestone 'v1' was not released"), "{said}");
    assert!(said.contains("tasks_done [fail]"), "{said}");

    let forced = sandbox.cli(&[
        "milestone",
        "update",
        "--name",
        "v1",
        "--status",
        "released",
        "--force",
        "true",
    ]);
    assert_eq!(forced.code, 0, "{}{}", forced.stdout, forced.stderr);
    assert!(
        forced.stdout.contains("Updated milestone 'v1'")
            && forced.stdout.contains("Warning: release check:"),
        "{}",
        forced.stdout
    );
}

/// The tool says what releasing a milestone involves.
#[test]
fn the_tool_description_mentions_the_guard() {
    let sandbox = Sandbox::empty();
    let tool = sandbox.server().tool_definition("handoff_update_milestone");
    let description = tool["description"].as_str().unwrap();
    assert!(
        description.contains("release_guard") && description.contains("handoff_release_check"),
        "{description}"
    );
    let force = tool["inputSchema"]["properties"]["force"]["description"]
        .as_str()
        .unwrap();
    assert!(force.contains("release_guard"), "{force}");
}
