//! The `task_orphan` lint rule (REQ-TRC-001, SPEC-TRC-001 / SPEC-TRC-002),
//! through the real `handoff-mcp` binary: the server over stdio JSON-RPC and
//! the `trace lint` CLI.
//!
//! A task with no requirement-type link is reported as a warning, unless it
//! is skipped, labelled `no-trace`, or a parent whose whole subtree is
//! accounted for. A project with no layer document does not use requirement
//! traceability, so nothing is reported there.

use crate::support::onboarding_e2e as support;

use serde_json::{json, Value};
use support::{snapshot, Sandbox, Server};

/// One requirement item to link tasks to.
const REQUIREMENT_BODY: &str = "# Requirements\n\n## Section\n\n### REQ-001 The first requirement\n\n- priority: P1\n\nThe system does the first thing.\n\n受入基準:\n- AC1: it does\n";

fn project() -> (Sandbox, Server) {
    let sandbox = Sandbox::initialized("");
    let mut server = sandbox.server();
    // No estimate rule: the fixtures create tasks straight in `done`.
    server.ok(
        "handoff_update_config",
        json!({ "updates": { "settings.require_estimate_hours": false } }),
    );
    (sandbox, server)
}

fn save_requirement_doc(server: &mut Server) {
    server.ok(
        "handoff_doc_save",
        json!({
            "slug": "req-orphans",
            "title": "Requirements",
            "doc_type": "spec",
            "layer": "requirement",
            "body": REQUIREMENT_BODY,
        }),
    );
}

fn create(server: &mut Server, id: &str, title: &str, extra: Value) {
    let mut task = json!({ "id": id, "title": title });
    for (key, value) in extra.as_object().expect("object") {
        task[key] = value.clone();
    }
    server.ok("handoff_update_task", json!({ "task": task }));
}

fn linked() -> Value {
    json!({ "requirement_ids": ["REQ-001"] })
}

fn none() -> Value {
    json!({})
}

/// The fixture tree. Orphans: `leaf`, `partial`, `partial.2`, `finished`,
/// `deep`, `deep.1.1`.
fn build_tasks(server: &mut Server) {
    create(server, "leaf", "An unlinked leaf", none());
    create(server, "traced", "A linked leaf", linked());
    create(
        server,
        "dropped",
        "A skipped leaf",
        json!({ "status": "skipped" }),
    );
    create(
        server,
        "exempt",
        "A no-trace leaf",
        json!({ "labels": ["chore", "no-trace"] }),
    );
    create(
        server,
        "finished",
        "A done leaf",
        json!({ "status": "done" }),
    );

    create(
        server,
        "covered",
        "A parent whose children are all accounted for",
        none(),
    );
    create(server, "covered.1", "linked child", linked());
    create(
        server,
        "covered.2",
        "no-trace child",
        json!({ "labels": ["no-trace"] }),
    );
    create(
        server,
        "covered.3",
        "skipped child",
        json!({ "status": "skipped" }),
    );

    create(
        server,
        "partial",
        "A parent with one unlinked child",
        none(),
    );
    create(server, "partial.1", "linked child", linked());
    create(server, "partial.2", "unlinked child", none());

    create(server, "deep", "A grandparent of an unlinked leaf", none());
    create(server, "deep.1", "linked child with a child", linked());
    create(server, "deep.1.1", "unlinked grandchild", none());
}

fn lint(server: &mut Server, arguments: Value) -> Value {
    server.call("handoff_trace_lint", arguments)
}

fn orphan_findings(result: &Value) -> Vec<&Value> {
    result["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .filter(|f| f["rule"] == "task_orphan")
        .collect()
}

fn orphan_task_ids(result: &Value) -> Vec<&str> {
    orphan_findings(result)
        .iter()
        .map(|f| f["task_id"].as_str().expect("task_id"))
        .collect()
}

const EXPECTED_ORPHANS: [&str; 6] = [
    "deep",
    "deep.1.1",
    "finished",
    "leaf",
    "partial",
    "partial.2",
];

// --- REQ-TRC-001 AC1-AC6, SPEC-TRC-001 AC7 ---

#[test]
fn trace_lint_reports_exactly_the_orphan_tasks_as_warnings() {
    let (_sandbox, mut server) = project();
    save_requirement_doc(&mut server);
    build_tasks(&mut server);

    let result = lint(&mut server, json!({ "rules": ["task_orphan"] }));

    // AC1 (leaf), AC4 (partial + its unlinked child), the done task, and the
    // three-level case; AC2 (dropped), AC3 (covered), AC5 (exempt) are absent.
    assert_eq!(orphan_task_ids(&result), EXPECTED_ORPHANS);
    // `rules: ["task_orphan"]` returns nothing else.
    assert_eq!(
        result["findings"].as_array().unwrap().len(),
        EXPECTED_ORPHANS.len()
    );
    assert_eq!(
        result["counts"],
        json!({ "error": 0, "warning": EXPECTED_ORPHANS.len(), "info": 0 })
    );
    assert_eq!(
        result["exit_code"], 0,
        "a warning does not fail the default fail_on"
    );
}

#[test]
fn each_finding_carries_task_id_title_and_suggested_action() {
    let (_sandbox, mut server) = project();
    save_requirement_doc(&mut server);
    build_tasks(&mut server);

    let result = lint(&mut server, json!({ "rules": ["task_orphan"] }));
    let findings = orphan_findings(&result);
    let leaf = findings
        .iter()
        .find(|f| f["task_id"] == "leaf")
        .expect("the leaf finding");

    assert_eq!(leaf["rule"], "task_orphan");
    assert_eq!(leaf["severity"], "warning");
    assert_eq!(
        leaf["task"], "leaf",
        "the existing `task` field carries the same id"
    );
    assert_eq!(leaf["title"], "An unlinked leaf");
    assert_eq!(leaf["item"], Value::Null);
    assert_eq!(leaf["doc"], Value::Null);
    let action = leaf["suggested_action"].as_str().expect("suggested_action");
    assert!(action.contains("requirement_ids"), "{action}");
    assert!(action.contains("handoff_trace_propose"), "{action}");
    assert!(action.contains("no-trace"), "{action}");
    assert!(action.contains("leaf"), "{action}");
    let message = leaf["message"].as_str().expect("message");
    assert!(
        message.contains("leaf") && message.contains("no requirement link"),
        "{message}"
    );

    for finding in &findings {
        for key in ["task_id", "title", "suggested_action"] {
            assert!(finding[key].is_string(), "{key} missing in {finding}");
        }
    }
}

#[test]
fn the_rule_runs_with_the_other_rules_when_rules_is_omitted() {
    let (_sandbox, mut server) = project();
    save_requirement_doc(&mut server);
    build_tasks(&mut server);
    // A requirement without acceptance criteria: a `missing_acceptance`
    // finding, so the run has findings of another rule to compare with.
    server.ok(
        "handoff_doc_save",
        json!({
            "slug": "req-bare",
            "title": "Bare requirements",
            "doc_type": "spec",
            "layer": "requirement",
            "body": "# Bare requirements\n\n### REQ-002 The second requirement\n\nThe system does the second thing.\n",
        }),
    );

    let result = lint(&mut server, json!({}));
    assert_eq!(orphan_task_ids(&result), EXPECTED_ORPHANS);

    // Findings of the other rules keep their shape: no task detail keys.
    let others: Vec<&Value> = result["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["rule"] != "task_orphan")
        .collect();
    assert!(
        others.iter().any(|f| f["rule"] == "missing_acceptance"),
        "the fixture has findings of other rules: {result}"
    );
    for finding in others {
        let keys: Vec<&String> = finding.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            ["rule", "severity", "item", "task", "doc", "message"],
            "{finding}"
        );
    }

    // A filter that does not name the rule leaves it out.
    let filtered = lint(&mut server, json!({ "rules": ["missing_acceptance"] }));
    assert!(orphan_findings(&filtered).is_empty());
}

#[test]
fn the_rule_can_be_releveled_or_switched_off_in_config() {
    let (sandbox, mut server) = project();
    save_requirement_doc(&mut server);
    build_tasks(&mut server);

    sandbox.append_config("[trace.lint.rules]\ntask_orphan = \"error\"\n");
    let result = lint(&mut server, json!({ "rules": ["task_orphan"] }));
    assert_eq!(result["counts"]["error"], EXPECTED_ORPHANS.len());
    assert_eq!(result["exit_code"], 1);

    let config = sandbox.handoff().join("config.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    std::fs::write(
        &config,
        text.replace("task_orphan = \"error\"", "task_orphan = \"off\""),
    )
    .unwrap();
    let result = lint(&mut server, json!({ "rules": ["task_orphan"] }));
    assert_eq!(result["findings"], json!([]));
    assert_eq!(result["exit_code"], 0);
}

// --- SPEC-TRC-002: scope guard and link types ---

#[test]
fn a_project_without_a_layer_document_reports_no_orphan_until_one_is_saved() {
    let (_sandbox, mut server) = project();
    create(&mut server, "leaf", "An unlinked leaf", none());
    create(&mut server, "other", "Another unlinked leaf", none());
    // A document without a layer does not switch traceability on.
    server.ok(
        "handoff_doc_save",
        json!({ "slug": "notes", "title": "Notes", "doc_type": "note", "body": "# Notes\n\ntext\n" }),
    );

    let before = lint(&mut server, json!({ "rules": ["task_orphan"] }));
    assert_eq!(before["findings"], json!([]));

    save_requirement_doc(&mut server);
    let after = lint(&mut server, json!({ "rules": ["task_orphan"] }));
    assert_eq!(orphan_task_ids(&after), ["leaf", "other"]);
}

#[test]
fn a_task_with_only_a_document_link_is_still_an_orphan() {
    let (_sandbox, mut server) = project();
    save_requirement_doc(&mut server);
    create(
        &mut server,
        "doc-only",
        "Linked to a document, not to an item",
        none(),
    );
    server.ok(
        "handoff_doc_save",
        json!({
            "slug": "design-note",
            "title": "Design note",
            "doc_type": "note",
            "body": "# Design note\n\ntext\n",
            "task_ids": ["doc-only"],
        }),
    );
    let task = server.call("handoff_get_task", json!({ "task_id": "doc-only" }));
    assert_eq!(task["task_links"][0]["link_type"], "doc", "{task}");

    let result = lint(&mut server, json!({ "rules": ["task_orphan"] }));
    assert_eq!(orphan_task_ids(&result), ["doc-only"]);
}

#[test]
fn linking_or_labelling_an_orphan_clears_its_finding() {
    let (_sandbox, mut server) = project();
    save_requirement_doc(&mut server);
    create(&mut server, "a", "To be linked", none());
    create(&mut server, "b", "To be labelled", none());
    assert_eq!(
        orphan_task_ids(&lint(&mut server, json!({ "rules": ["task_orphan"] }))),
        ["a", "b"]
    );

    server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "a", "requirement_ids": ["REQ-001"] } }),
    );
    server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "b", "labels": ["no-trace"] } }),
    );
    let result = lint(&mut server, json!({ "rules": ["task_orphan"] }));
    assert_eq!(result["findings"], json!([]));
}

// --- read-only, CLI ---

#[test]
fn the_lint_call_writes_nothing() {
    let (sandbox, mut server) = project();
    save_requirement_doc(&mut server);
    build_tasks(&mut server);
    // One call first, so derived files an earlier tool call may leave are
    // already in place.
    lint(&mut server, json!({ "rules": ["task_orphan"] }));

    let before = snapshot(&sandbox.handoff());
    let result = lint(&mut server, json!({ "rules": ["task_orphan"] }));
    assert_eq!(orphan_task_ids(&result), EXPECTED_ORPHANS);
    assert_eq!(snapshot(&sandbox.handoff()), before);
}

#[test]
fn the_cli_lists_orphan_tasks_and_accepts_the_rule_id() {
    let (sandbox, mut server) = project();
    save_requirement_doc(&mut server);
    build_tasks(&mut server);
    drop(server);

    let text = sandbox.cli(&[
        "trace",
        "lint",
        "--rules",
        "task_orphan",
        "--format",
        "text",
    ]);
    assert_eq!(text.code, 0, "{}{}", text.stdout, text.stderr);
    // `--format text` prints one `severity[rule] subject: message` line each.
    let lines: Vec<&str> = text.stdout.lines().collect();
    assert_eq!(lines.len(), EXPECTED_ORPHANS.len(), "{}", text.stdout);
    assert!(
        lines[3].starts_with("warning[task_orphan] leaf: "),
        "{}",
        text.stdout
    );

    let failing = sandbox.cli(&[
        "trace",
        "lint",
        "--rules",
        "task_orphan",
        "--fail-on",
        "warning",
    ]);
    assert_eq!(failing.code, 1, "{}{}", failing.stdout, failing.stderr);
}

// --- a layer document that cannot be read ---

/// The lint reports an unreadable document under its own rule
/// (`frontmatter_invalid`, an error). With no readable layer document left
/// it reports no orphan: that error is what says the check is incomplete.
#[test]
fn an_unreadable_layer_document_is_reported_by_its_own_rule() {
    let (sandbox, mut server) = project();
    save_requirement_doc(&mut server);
    create(&mut server, "leaf", "An unlinked leaf", none());
    let path = sandbox.handoff().join("docs/_doc.req-orphans.md");
    let text = std::fs::read_to_string(&path).expect("the layer document");
    std::fs::write(&path, text.replacen("---\n", "---\n: : broken [\n", 1)).unwrap();

    let result = lint(&mut server, json!({}));
    let rules: Vec<&str> = result["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .map(|f| f["rule"].as_str().unwrap())
        .collect();
    assert!(rules.contains(&"frontmatter_invalid"), "{result}");
    assert!(!rules.contains(&"task_orphan"), "{result}");
    assert_eq!(result["exit_code"], 1, "{result}");
}
