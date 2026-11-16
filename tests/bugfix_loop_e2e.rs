//! Real-binary E2E for the tool calls the `/bugfix-loop` command is built on
//! (SPEC-ONB-020, SPEC-ONB-021, SPEC-ONB-022).
//!
//! The command is prose the bug-fix coordinator follows
//! (`plugin-task-loop/commands/bugfix-loop.md`); its wording is pinned by
//! `plugin-task-loop/workflows/lib/bugfix-loop-command.test.js`. These tests
//! play the coordinator's part against the real server and a real, tiny
//! project with a seeded bug: every `handoff_*` call is made as the command
//! spells it out, and every test command is really run. What the two agents
//! do (write the test, write the fix) is done here by writing the files.
//!
//! Unix only: the project under test is a set of shell scripts.
#![cfg(unix)]

#[path = "support/onboarding_e2e.rs"]
mod support;

use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use serde_json::{json, Value};
use support::{git, Sandbox, Server};

/// `add` drops the minus sign of its operands: `add -1 -2` prints 3.
const BUGGY_CALC: &str = "#!/bin/sh\n# calc.sh add <a> <b>\na=${2#-}\nb=${3#-}\necho $((a + b))\n";
const FIXED_CALC: &str = "#!/bin/sh\n# calc.sh add <a> <b>\necho $(($2 + $3))\n";
/// A "fix" that makes the negative case pass and breaks the basic one.
const BROKEN_FIX_CALC: &str = "#!/bin/sh\n# calc.sh add <a> <b>\necho $((0 - ${2#-} - ${3#-}))\n";

/// Runs every `tests/*.sh`, or only the one named; exits 1 when one fails.
const RUN_TESTS: &str = "#!/bin/sh\nfail=0\nfor t in tests/${1:-*}.sh; do\n  if sh \"$t\"; then echo \"ok $t\"; else echo \"FAIL $t\"; fail=1; fi\ndone\nexit $fail\n";
const TEST_ADD_BASIC: &str = "[ \"$(./calc.sh add 1 2)\" = 3 ]\n";
const TEST_ADD_NEGATIVE: &str = "[ \"$(./calc.sh add -1 -2)\" = -3 ]\n";

const FULL_SUITE: &str = "./run-tests.sh";
const ONE_TEST: &str = "./run-tests.sh add_negative";

fn write_executable(sandbox: &Sandbox, name: &str, content: &str) {
    sandbox.write(name, content);
    let path = sandbox.project.join(name);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A project with the seeded bug, one passing test and `[commands] test`.
fn buggy_project() -> Sandbox {
    let sandbox = Sandbox::initialized(&format!("[commands]\ntest = \"{FULL_SUITE}\"\n"));
    write_executable(&sandbox, "calc.sh", BUGGY_CALC);
    write_executable(&sandbox, "run-tests.sh", RUN_TESTS);
    sandbox.write("tests/add_basic.sh", TEST_ADD_BASIC);
    sandbox
}

/// Runs `command` the way the coordinator does: through the shell, from the
/// project root. Returns the exit code and the output.
fn run(sandbox: &Sandbox, command: &str) -> (i64, String) {
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(&sandbox.project)
        .output()
        .expect("sh");
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (i64::from(output.status.code().unwrap_or(-1)), text)
}

fn status_word(exit_code: i64) -> &'static str {
    if exit_code == 0 {
        "pass"
    } else {
        "fail"
    }
}

fn append_note(server: &mut Server, task: &str, note: &str) {
    server.ok(
        "handoff_update_task",
        json!({ "task": { "id": task, "notes_append": note } }),
    );
}

fn task(server: &mut Server, id: &str) -> Value {
    server.call("handoff_get_task", json!({ "task_id": id }))
}

fn linked_items(task: &Value) -> Vec<String> {
    task["task_links"]
        .as_array()
        .expect("task_links")
        .iter()
        .filter(|link| link["link_type"] == json!("requirement"))
        .map(|link| link["label"].as_str().expect("label").to_string())
        .collect()
}

fn bugfixes_body(server: &mut Server) -> String {
    let doc = server.call("handoff_doc_get", json!({ "doc_id": "bugfixes" }));
    doc["body"].as_str().expect("body").to_string()
}

/// The V-model state of `item` (`passing`, `failing`, `not_run`, ...).
fn item_state(server: &mut Server, item: &str) -> String {
    let report = server.call("handoff_trace_report", json!({ "include_items": true }));
    report["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|entry| entry["id"] == json!(item))
        .unwrap_or_else(|| panic!("no item {item} in {report}"))["state"]
        .as_str()
        .expect("state")
        .to_string()
}

/// Stage 1 "Record" for a new symptom: the bug task, the one `bugfixes`
/// document, the reproduction item linked to the task, and `in_progress`.
/// Returns `(task id, bug number)`.
fn record(server: &mut Server, symptom: &str, reproduction: &str) -> (String, String) {
    let created = server.ok(
        "handoff_update_task",
        json!({ "task": {
            "title": format!("[bug] {symptom}"),
            "status": "todo",
            "labels": ["bug"],
            "notes": symptom,
            "done_criteria": [
                { "item": "The regression test passes" },
                { "item": "The full test suite shows no new failure" },
            ],
        } }),
    );
    let task_id = created
        .split_whitespace()
        .nth(2)
        .expect("`Created task <id>: ...`")
        .trim_end_matches(':')
        .to_string();
    let number = record_item(server, &task_id, symptom, reproduction);
    (task_id, number)
}

/// Stage 1 from step 2 on, for a task that already exists (`task: <id>`).
fn record_item(server: &mut Server, task_id: &str, symptom: &str, reproduction: &str) -> String {
    // The document is created only when it does not exist yet.
    if server
        .call_text(
            "handoff_doc_get",
            json!({ "doc_id": "bugfixes", "format": "meta" }),
        )
        .is_err()
    {
        server.ok(
            "handoff_doc_save",
            json!({
                "slug": "bugfixes",
                "title": "Bug fixes",
                "layer": "requirement",
                "trace_profile": "bugfix",
                "body": "# Bug fixes\n\nOne reproduction item (`REQ-BUG-nnn`) and one regression test item (`AT-BUG-nnn`) per bug.\n",
            }),
        );
    }
    // A task that already has its reproduction item keeps it.
    let existing = linked_items(&task(server, task_id))
        .into_iter()
        .find_map(|label| label.strip_prefix("REQ-BUG-").map(str::to_string));
    let number = existing.clone().unwrap_or_else(|| {
        let highest = bugfixes_body(server)
            .lines()
            .filter_map(|line| line.strip_prefix("### REQ-BUG-"))
            .filter_map(|rest| rest.split_whitespace().next()?.parse::<u32>().ok())
            .max()
            .unwrap_or(0);
        format!("{:03}", highest + 1)
    });
    if existing.is_none() {
        let update = server.call(
            "handoff_trace_update",
            json!({ "ops": [
                { "op": "upsert_item", "doc": "bugfixes", "id": format!("REQ-BUG-{number}"),
                  "title": symptom, "statement": reproduction },
                { "op": "link", "item": format!("REQ-BUG-{number}"), "task": task_id },
            ] }),
        );
        assert!(update.get("failed").is_none(), "{update}");
    }
    server.ok(
        "handoff_update_task",
        json!({ "task": { "id": task_id, "status": "in_progress", "schedule": { "estimate_hours": 0.5 } } }),
    );
    number
}

/// Stage 2 step 1: the full suite before anything is changed.
fn baseline(sandbox: &Sandbox, server: &mut Server, task_id: &str) -> i64 {
    let (exit_code, output) = run(sandbox, FULL_SUITE);
    append_note(
        server,
        task_id,
        &format!(
            "Baseline (full test suite before the fix): {}\nCommand: {FULL_SUITE}\nExit code: {exit_code}\nOutput: {output}",
            status_word(exit_code)
        ),
    );
    exit_code
}

/// Stage 2 step 4, the reproduced case: the failed run goes to the notes, the
/// regression test item is added, linked and recorded as `fail`.
fn record_reproduced(
    server: &mut Server,
    task_id: &str,
    number: &str,
    exit_code: i64,
    output: &str,
) {
    append_note(
        server,
        task_id,
        &format!(
            "Reproduce (before the fix): fail\nCommand: {ONE_TEST}\nExit code: {exit_code}\nOutput: {output}"
        ),
    );
    let update = server.call(
        "handoff_trace_update",
        json!({ "task_id": task_id, "ops": [
            { "op": "upsert_item", "doc": "bugfixes", "id": format!("AT-BUG-{number}"),
              "title": "Regression test: add with negative numbers",
              "statement": "`add -1 -2` prints -3.",
              "attrs": { "layer": "acceptance", "verifies": format!("REQ-BUG-{number}"),
                         "method": "auto", "test": ONE_TEST } },
            { "op": "link", "item": format!("AT-BUG-{number}"), "task": task_id },
            { "op": "record", "item": format!("AT-BUG-{number}"), "result": "fail" },
        ] }),
    );
    assert!(update.get("failed").is_none(), "{update}");
}

/// Stage 6 step 5: the passing run goes to the notes and the regression test
/// item is recorded as `pass`.
fn record_verified(server: &mut Server, task_id: &str, number: &str) {
    append_note(
        server,
        task_id,
        &format!(
            "Verify (after the fix): pass\nCommand: {ONE_TEST}\nExit code: 0\nRelated tests: none"
        ),
    );
    let update = server.call(
        "handoff_trace_update",
        json!({ "task_id": task_id, "ops": [
            { "op": "record", "item": format!("AT-BUG-{number}"), "result": "pass" },
        ] }),
    );
    assert!(update.get("failed").is_none(), "{update}");
}

/// Stage 7 steps 1-3: the full suite after the fix, compared with a passing
/// baseline. Returns whether there is a new failure.
fn regress(sandbox: &Sandbox, server: &mut Server, task_id: &str, baseline_exit: i64) -> bool {
    let (exit_code, output) = run(sandbox, FULL_SUITE);
    // With a passing baseline, any failure now is new.
    let new_failure = baseline_exit == 0 && exit_code != 0;
    append_note(
        server,
        task_id,
        &format!(
            "Regress (full test suite): {}\nCommand: {FULL_SUITE}\nExit code: {exit_code}\nOutput: {output}",
            status_word(exit_code)
        ),
    );
    new_failure
}

/// Stage 7 step 5: check off the criteria that are met, propose the two
/// items for approval, then `done`.
fn close_as_done(server: &mut Server, task_id: &str, number: &str) {
    for index in 0..2 {
        server.ok(
            "handoff_check_criterion",
            json!({ "task_id": task_id, "criterion_index": index, "checked": true }),
        );
    }
    let proposed = server.call(
        "handoff_trace_update",
        json!({ "ops": [
            { "op": "set", "item": format!("REQ-BUG-{number}"), "approval": "review" },
            { "op": "set", "item": format!("AT-BUG-{number}"), "approval": "review" },
        ] }),
    );
    assert!(proposed.get("failed").is_none(), "{proposed}");
    server.ok(
        "handoff_update_task",
        json!({ "task": { "id": task_id, "status": "done" } }),
    );
}

/// The whole loop for the seeded bug, ending in `done`. Returns the task id.
fn fix_the_seeded_bug(sandbox: &Sandbox, server: &mut Server, symptom: &str) -> (String, String) {
    let (task_id, number) = record(
        server,
        symptom,
        "Run `./calc.sh add -1 -2`: it prints 3, expected -3.",
    );
    let baseline_exit = baseline(sandbox, server, &task_id);
    // What the developer agent does in Stage 2: a test, and nothing else.
    sandbox.write("tests/add_negative.sh", TEST_ADD_NEGATIVE);
    let (exit_code, output) = run(sandbox, ONE_TEST);
    assert_ne!(
        exit_code, 0,
        "the seeded bug must make the test fail: {output}"
    );
    record_reproduced(server, &task_id, &number, exit_code, &output);
    // What the developer agent does in Stage 5.
    write_executable(sandbox, "calc.sh", FIXED_CALC);
    let (exit_code, output) = run(sandbox, ONE_TEST);
    assert_eq!(
        exit_code, 0,
        "the fix must make the same test pass: {output}"
    );
    record_verified(server, &task_id, &number);
    assert!(!regress(sandbox, server, &task_id, baseline_exit));
    close_as_done(server, &task_id, &number);
    (task_id, number)
}

// --- SPEC-ONB-020 ---

#[test]
fn spec020_ac1_ac2_ac5_a_fixed_bug_leaves_its_results_in_order_its_items_linked_and_passing() {
    let sandbox = buggy_project();
    let mut server = sandbox.server();
    let (task_id, number) =
        fix_the_seeded_bug(&sandbox, &mut server, "add returns 3 for -1 and -2");
    assert_eq!(number, "001");

    let task = task(&mut server, &task_id);
    assert_eq!(task["status"], json!("done"));
    assert_eq!(task["labels"], json!(["bug"]));

    // AC1: the failure before the fix, the success after it, the full suite — in this order.
    let notes = task["notes"].as_str().expect("notes");
    let positions: Vec<usize> = [
        "Baseline (full test suite before the fix): pass",
        "Reproduce (before the fix): fail",
        "Verify (after the fix): pass",
        "Regress (full test suite): pass",
    ]
    .iter()
    .map(|line| {
        notes
            .find(line)
            .unwrap_or_else(|| panic!("`{line}` is not in the notes:\n{notes}"))
    })
    .collect();
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "{notes}"
    );
    assert!(notes.contains("FAIL tests/add_negative.sh"), "{notes}");
    assert!(notes.contains("Exit code: 1"), "{notes}");

    // AC2: the reproduction item and the regression test item, both linked to the task.
    assert_eq!(linked_items(&task), ["REQ-BUG-001", "AT-BUG-001"]);
    let body = bugfixes_body(&mut server);
    assert!(
        body.contains("### REQ-BUG-001 add returns 3 for -1 and -2"),
        "{body}"
    );
    assert!(body.contains("### AT-BUG-001 Regression test:"), "{body}");
    assert!(body.contains("- verifies: REQ-BUG-001"), "{body}");
    assert!(body.contains(&format!("- test: {ONE_TEST}")), "{body}");
    let meta = server.call(
        "handoff_doc_get",
        json!({ "doc_id": "bugfixes", "format": "meta" }),
    );
    assert_eq!(meta["trace_profile"], json!("bugfix"));
    assert_eq!(meta["layer"], json!("requirement"));

    // AC5: the acceptance-side item is passing.
    assert_eq!(item_state(&mut server, "AT-BUG-001"), "passing");
}

/// Items start as `draft`, which the done guard counts against the task.
/// The loop proposes its two items for approval before it closes the task,
/// so it also finishes in a project that set the guard to `block`.
#[test]
fn spec020_a_fixed_bug_is_closed_under_a_blocking_done_guard_without_force() {
    let sandbox = buggy_project();
    sandbox.append_config("[trace]\ndone_guard = \"block\"\n");
    let mut server = sandbox.server();

    // Up to the last step of Stage 7, with the items still `draft`: refused.
    let (task_id, number) = record(&mut server, "add returns 3 for -1 and -2", "add -1 -2");
    let baseline_exit = baseline(&sandbox, &mut server, &task_id);
    sandbox.write("tests/add_negative.sh", TEST_ADD_NEGATIVE);
    let (exit_code, output) = run(&sandbox, ONE_TEST);
    record_reproduced(&mut server, &task_id, &number, exit_code, &output);
    write_executable(&sandbox, "calc.sh", FIXED_CALC);
    record_verified(&mut server, &task_id, &number);
    assert!(!regress(&sandbox, &mut server, &task_id, baseline_exit));
    for index in 0..2 {
        server.ok(
            "handoff_check_criterion",
            json!({ "task_id": task_id, "criterion_index": index, "checked": true }),
        );
    }
    let refused = server.err(
        "handoff_update_task",
        json!({ "task": { "id": task_id, "status": "done" } }),
    );
    assert!(refused.contains("approval_draft=2"), "{refused}");

    // The step the command prescribes, then `done` without `force`.
    close_as_done(&mut server, &task_id, &number);
    let closed = task(&mut server, &task_id);
    assert_eq!(closed["status"], json!("done"));
    assert_eq!(closed["trace"]["blockers"]["approval_draft"], json!(0));
    // Proposed, not signed off: approval stays the user's act.
    let report = server.call("handoff_trace_report", json!({ "include_items": true }));
    for entry in report["items"].as_array().expect("items") {
        assert_eq!(entry["approval"], json!("review"), "{entry}");
    }
}

#[test]
fn spec020_the_regression_item_is_failing_between_reproduce_and_verify() {
    let sandbox = buggy_project();
    let mut server = sandbox.server();
    let (task_id, number) = record(&mut server, "add drops the minus sign", "add -1 -2");
    baseline(&sandbox, &mut server, &task_id);
    sandbox.write("tests/add_negative.sh", TEST_ADD_NEGATIVE);
    let (exit_code, output) = run(&sandbox, ONE_TEST);
    record_reproduced(&mut server, &task_id, &number, exit_code, &output);
    assert_eq!(item_state(&mut server, "AT-BUG-001"), "failing");
    assert_eq!(task(&mut server, &task_id)["status"], json!("in_progress"));
}

#[test]
fn spec020_ac6_a_second_bug_adds_items_to_the_same_document() {
    let sandbox = buggy_project();
    let mut server = sandbox.server();
    fix_the_seeded_bug(&sandbox, &mut server, "add returns 3 for -1 and -2");
    let documents_before = server.call("handoff_doc_list", json!({}));

    let (second, number) = record(
        &mut server,
        "add prints nothing for an empty operand",
        "Run `./calc.sh add 1 ''`.",
    );
    assert_eq!(number, "002");
    let documents_after = server.call("handoff_doc_list", json!({}));
    let slugs = |list: &Value| -> Vec<String> {
        list["documents"]
            .as_array()
            .expect("documents")
            .iter()
            .map(|doc| doc["slug"].as_str().expect("slug").to_string())
            .collect()
    };
    assert_eq!(slugs(&documents_before), ["bugfixes"]);
    assert_eq!(slugs(&documents_after), ["bugfixes"]);

    let body = bugfixes_body(&mut server);
    for heading in ["### REQ-BUG-001 ", "### AT-BUG-001 ", "### REQ-BUG-002 "] {
        assert!(body.contains(heading), "`{heading}` is missing:\n{body}");
    }
    assert_eq!(linked_items(&task(&mut server, &second)), ["REQ-BUG-002"]);
    // The first bug's result is untouched by the second one.
    assert_eq!(item_state(&mut server, "AT-BUG-001"), "passing");
}

#[test]
fn spec020_ac3_a_symptom_that_cannot_be_reproduced_blocks_the_task_and_leaves_the_product_alone() {
    let sandbox = buggy_project();
    let mut server = sandbox.server();
    let product_before = std::fs::read(sandbox.project.join("calc.sh")).unwrap();

    let (task_id, _) = record(
        &mut server,
        "add returns the wrong sum for 2 and 2",
        "Run `./calc.sh add 2 2`: reported to print 5.",
    );
    baseline(&sandbox, &mut server, &task_id);
    // The test the developer writes for this symptom passes at once.
    sandbox.write("tests/add_two_two.sh", "[ \"$(./calc.sh add 2 2)\" = 4 ]\n");
    let (exit_code, output) = run(&sandbox, "./run-tests.sh add_two_two");
    assert_eq!(exit_code, 0, "{output}");

    server.ok(
        "handoff_update_task",
        json!({ "task": { "id": task_id, "status": "blocked", "notes_append":
            "Reproduce: could not reproduce\nTried: tests/add_two_two.sh with ./run-tests.sh add_two_two, exit code 0\nNeeded to continue: the exact input" } }),
    );

    let task = task(&mut server, &task_id);
    assert_eq!(task["status"], json!("blocked"));
    let notes = task["notes"].as_str().unwrap();
    assert!(notes.contains("Reproduce: could not reproduce"), "{notes}");
    for absent in [
        "Reproduce (before the fix)",
        "Verify (after the fix)",
        "Regress (",
    ] {
        assert!(
            !notes.contains(absent),
            "`{absent}` must not be recorded:\n{notes}"
        );
    }
    // No regression test item, no result, and the product is byte-identical.
    assert_eq!(linked_items(&task), ["REQ-BUG-001"]);
    assert!(!bugfixes_body(&mut server).contains("### AT-BUG-"));
    assert_eq!(
        std::fs::read(sandbox.project.join("calc.sh")).unwrap(),
        product_before
    );
}

#[test]
fn spec020_ac4_a_new_failure_in_the_full_suite_keeps_the_task_from_being_done() {
    let sandbox = buggy_project();
    let mut server = sandbox.server();
    let (task_id, number) = record(&mut server, "add returns 3 for -1 and -2", "add -1 -2");
    let baseline_exit = baseline(&sandbox, &mut server, &task_id);
    assert_eq!(baseline_exit, 0);
    sandbox.write("tests/add_negative.sh", TEST_ADD_NEGATIVE);
    let (exit_code, output) = run(&sandbox, ONE_TEST);
    record_reproduced(&mut server, &task_id, &number, exit_code, &output);

    // A fix that passes the reproduction test and breaks a test that passed before.
    write_executable(&sandbox, "calc.sh", BROKEN_FIX_CALC);
    assert_eq!(run(&sandbox, ONE_TEST).0, 0);
    record_verified(&mut server, &task_id, &number);
    assert!(regress(&sandbox, &mut server, &task_id, baseline_exit));

    // The coordinator checks only what is met; the suite criterion is not.
    server.ok(
        "handoff_check_criterion",
        json!({ "task_id": task_id, "criterion_index": 0, "checked": true }),
    );
    let task_now = task(&mut server, &task_id);
    assert_eq!(task_now["status"], json!("in_progress"));
    let notes = task_now["notes"].as_str().unwrap();
    assert!(notes.contains("Regress (full test suite): fail"), "{notes}");
    assert!(notes.contains("FAIL tests/add_basic.sh"), "{notes}");

    // Even a coordinator that tried could not close it: the server refuses
    // `done` while the suite criterion is unchecked.
    let refused = server.err(
        "handoff_update_task",
        json!({ "task": { "id": task_id, "status": "done" } }),
    );
    assert!(
        refused.contains("The full test suite shows no new failure"),
        "{refused}"
    );
    assert_eq!(task(&mut server, &task_id)["status"], json!("in_progress"));
}

#[test]
fn spec020_an_existing_bug_task_is_used_as_it_is_and_keeps_its_items_on_a_second_run() {
    let sandbox = buggy_project();
    let mut server = sandbox.server();
    // The task `/session-loop` files when the smoke check fails.
    server.ok(
        "handoff_update_task",
        json!({ "task": {
            "id": "t7",
            "title": "[smoke] Smoke check failed",
            "status": "todo",
            "priority": "high",
            "labels": ["bug"],
            "notes": "Smoke check failed.\nCommand: ./smoke.sh\nResult: exit code 3",
            "done_criteria": [{ "item": "The smoke command exits with code 0" }],
        } }),
    );
    let number = record_item(&mut server, "t7", "Smoke check failed", "Run `./smoke.sh`.");
    assert_eq!(number, "001");
    let first = task(&mut server, "t7");
    assert_eq!(first["title"], json!("[smoke] Smoke check failed"));
    assert_eq!(first["status"], json!("in_progress"));
    assert_eq!(linked_items(&first), ["REQ-BUG-001"]);

    // Started again on the same task: no second reproduction item.
    let again = record_item(&mut server, "t7", "Smoke check failed", "Run `./smoke.sh`.");
    assert_eq!(again, "001");
    assert_eq!(linked_items(&task(&mut server, "t7")), ["REQ-BUG-001"]);
    assert_eq!(
        bugfixes_body(&mut server).matches("### REQ-BUG-").count(),
        1
    );
    let tasks = server.call("handoff_list_tasks", json!({ "label_filter": "bug" }));
    assert_eq!(tasks["task_tree"].as_array().unwrap().len(), 1, "{tasks}");
}

#[test]
fn the_bugfixes_document_needs_no_specification_layer_under_a_stricter_project_profile() {
    // The project asks for basic_spec and system_test; the document's own
    // `bugfix` profile asks only for the regression test.
    let sandbox = buggy_project();
    sandbox.append_config("[trace]\nprofile = \"standard\"\n");
    let mut server = sandbox.server();
    fix_the_seeded_bug(&sandbox, &mut server, "add returns 3 for -1 and -2");
    let lint = server.call("handoff_trace_lint", json!({}));
    assert_eq!(lint["counts"]["error"], json!(0), "{lint}");
    let bug_findings: Vec<&Value> = lint["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .filter(|finding| {
            finding["rule"] == json!("missing_profile_layer")
                || finding["rule"] == json!("unverified")
                || finding["rule"] == json!("orphan")
        })
        .collect();
    assert!(bug_findings.is_empty(), "{bug_findings:?}");
}

// --- SPEC-ONB-021 ---

/// A project whose requirement names `calc.sh` as its implementation and
/// whose acceptance item names the test that checks it.
fn project_with_a_traced_requirement() -> Sandbox {
    let sandbox = buggy_project();
    let mut server = sandbox.server();
    server.ok(
        "handoff_doc_save",
        json!({
            "slug": "calc-requirements",
            "title": "Calculator requirements",
            "layer": "requirement",
            "body": "# Calculator requirements\n\n### REQ-001 The calculator adds two integers\n\n- priority: P1\n\n`add a b` prints the sum.\n\n### AT-001 Addition of two positive integers\n\n- layer: acceptance\n- verifies: REQ-001\n- method: auto\n- test: ./run-tests.sh add_basic\n\n`add 1 2` prints 3.\n\n### AT-002 Addition is reviewed by a person\n\n- layer: acceptance\n- verifies: REQ-001\n- method: manual\n\nA reviewer reads the output.\n",
        }),
    );
    let set = server.call(
        "handoff_trace_update",
        json!({ "ops": [{ "op": "set", "item": "REQ-001", "impl_refs": [{ "path": "calc.sh" }] }] }),
    );
    assert!(set.get("failed").is_none(), "{set}");
    sandbox
}

/// Stage 3 for one file: `(requirement ids, verification item ids, related tests)`.
fn impact(server: &mut Server, file: &str) -> (Vec<String>, Vec<String>, Vec<String>) {
    let strings = |value: &Value| -> Vec<String> {
        value
            .as_array()
            .into_iter()
            .flatten()
            .map(|entry| entry.as_str().expect("string").to_string())
            .collect()
    };
    let affected = server.call("handoff_doc_req_impact", json!({ "file": file }));
    let requirements: Vec<String> = affected["affected_requirements"]
        .as_array()
        .expect("affected_requirements")
        .iter()
        .map(|entry| entry["stable_id"].as_str().expect("stable_id").to_string())
        .collect();
    let traced = server.call("handoff_trace_impact", json!({ "file": file }));
    let verification = strings(&traced["rerun_candidates"]);
    // The `test` attribute of each verification item found.
    let with_tests = server.call("handoff_doc_req_list", json!({ "has_tests": true }));
    let related_tests = with_tests["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| verification.contains(&item["stable_id"].as_str().unwrap().to_string()))
        .flat_map(|item| item["test_refs"].as_array().unwrap().clone())
        .map(|test_ref| test_ref["path"].as_str().expect("path").to_string())
        .collect();
    (requirements, verification, related_tests)
}

#[test]
fn spec021_ac1_ac3_a_file_named_in_impl_refs_brings_its_requirement_and_its_test() {
    let sandbox = project_with_a_traced_requirement();
    let mut server = sandbox.server();
    let (requirements, verification, related_tests) = impact(&mut server, "calc.sh");
    // AC1: the requirement id that goes into the Gate presentation.
    assert_eq!(requirements, ["REQ-001"]);
    // AC3: the `test` of each verification item joins the Verify run set; an
    // item without one is listed and has nothing to run.
    assert_eq!(verification, ["AT-001", "AT-002"]);
    assert_eq!(related_tests, ["./run-tests.sh add_basic"]);
    // The related test is a command the coordinator can run as it is.
    assert_eq!(run(&sandbox, &related_tests[0]).0, 0);
}

#[test]
fn spec021_ac2_a_file_nothing_references_has_an_empty_impact_scope() {
    let sandbox = project_with_a_traced_requirement();
    let mut server = sandbox.server();
    let (requirements, verification, related_tests) = impact(&mut server, "README.md");
    assert!(requirements.is_empty() && verification.is_empty() && related_tests.is_empty());
    // Nothing at all is traced in a project without requirement documents.
    let sandbox = buggy_project();
    let mut server = sandbox.server();
    let (requirements, verification, related_tests) = impact(&mut server, "calc.sh");
    assert!(requirements.is_empty() && verification.is_empty() && related_tests.is_empty());
}

#[test]
fn spec021_the_impact_calls_change_nothing() {
    let sandbox = project_with_a_traced_requirement();
    let mut server = sandbox.server();
    let before = bytes_of_handoff(&sandbox);
    impact(&mut server, "calc.sh");
    impact(&mut server, "README.md");
    assert_eq!(bytes_of_handoff(&sandbox), before);
}

/// Every file under `.handoff/docs` and `.handoff/tasks`, with its content.
fn bytes_of_handoff(sandbox: &Sandbox) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push((
                    path.to_string_lossy().to_string(),
                    std::fs::read(&path).unwrap(),
                ));
            }
        }
    }
    let mut out = Vec::new();
    for name in ["docs", "tasks"] {
        walk(&sandbox.handoff().join(name), &mut out);
    }
    out.sort();
    out
}

// --- SPEC-ONB-022 ---

#[test]
fn spec022_ac1_until_the_gate_only_test_code_and_handoff_state_differ() {
    let sandbox = buggy_project();
    sandbox.write(".gitignore", "/.handoff/\n");
    sandbox.git(&["init", "-q", "-b", "main"]);
    sandbox.git(&["add", "-A"]);
    sandbox.git(&["commit", "-q", "-m", "baseline"]);
    let mut server = sandbox.server();

    // Stages 1-3, then "Abort" at the Gate with "Keep the added tests".
    let (task_id, number) = record(&mut server, "add returns 3 for -1 and -2", "add -1 -2");
    baseline(&sandbox, &mut server, &task_id);
    sandbox.write("tests/add_negative.sh", TEST_ADD_NEGATIVE);
    let (exit_code, output) = run(&sandbox, ONE_TEST);
    record_reproduced(&mut server, &task_id, &number, exit_code, &output);
    impact(&mut server, "calc.sh");
    server.ok(
        "handoff_update_task",
        json!({ "task": { "id": task_id, "status": "todo", "notes_append": "Aborted at the Gate: not now" } }),
    );

    // The step-3 check of Stage 2, and AC1: nothing but test code differs
    // (`.handoff/` is not tracked by the project's repository).
    let status = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(&sandbox.project)
        .output()
        .expect("git status");
    let changed = String::from_utf8_lossy(&status.stdout).to_string();
    assert_eq!(changed.trim(), "?? tests/add_negative.sh", "{changed}");
    git(&sandbox.project, &["diff", "--quiet", "--", "calc.sh"]);

    // The bug is still there, and the records say so.
    let task = task(&mut server, &task_id);
    assert_eq!(task["status"], json!("todo"));
    assert!(task["notes"]
        .as_str()
        .unwrap()
        .contains("Aborted at the Gate: not now"));
    assert_eq!(item_state(&mut server, "AT-BUG-001"), "failing");
}
