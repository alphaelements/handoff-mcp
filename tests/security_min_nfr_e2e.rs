//! Real-binary E2E for the tool calls the security minimum of
//! requirement-loop Phase 2 is built on (SPEC-ONB-051).
//!
//! The procedure is prose the coordinator follows
//! (`plugin-task-loop/commands/requirement-loop.md`, pinned by
//! `plugin-task-loop/workflows/lib/requirement-loop-security-min.test.js`).
//! These tests play the coordinator's part against the real server: the
//! switch it reads, the NFR document it writes with the three decisions, and
//! the `nfr_check` record it leaves, so a step that relies on an answer the
//! server does not give fails here.
#![cfg(unix)]

#[path = "support/onboarding_e2e.rs"]
mod support;

use serde_json::{json, Value};
use support::{Sandbox, Server};

/// The three items a decision is required for, as the documents name them.
const ITEMS: [&str; 3] = [
    "Handling of secrets",
    "Validation of external input",
    "Dependency updates",
];

const NFR_SLUG: &str = "demo-nfr";

/// Two NFR items filed for the first two items, written under the rules of
/// Phase 2 step 3 (numeric criterion, method, acceptance criterion).
const FILED_ITEMS: &str = "\
### NFR-SEC-001 Secrets stay out of the repository\n\
- priority: P1\n\
- method: auto\n\n\
API keys and passwords are read from the environment and never committed.\n\n\
Acceptance criteria:\n\
- AC-1: Given the repository at any commit, when the secrets scan of CI runs, then it reports 0 findings.\n\n\
### NFR-SEC-002 External input is validated at the boundary\n\
- priority: P1\n\
- method: auto\n\n\
Every request body is checked against its schema before it is used.\n\n\
Acceptance criteria:\n\
- AC-1: Given a request whose body violates the schema, when it is sent to any of the 12 endpoints, then the response is 400 and no handler code runs.\n";

fn table(rows: [(&str, &str); 3]) -> String {
    let mut text = String::from(
        "## Security minimum\n\n| Item | Decision | Requirement or reason |\n|---|---|---|\n",
    );
    for (item, (decision, detail)) in ITEMS.iter().zip(rows) {
        text.push_str(&format!("| {item} | {decision} | {detail} |\n"));
    }
    text
}

/// Saves the NFR document: created on the first call, updated in place
/// (by `doc_id`) on the following ones. Returns its id.
fn save_nfr_document(
    server: &mut Server,
    doc_id: Option<&str>,
    items: &str,
    table: &str,
) -> String {
    let mut arguments = json!({
        "slug": NFR_SLUG,
        "title": "Non-functional requirements",
        "layer": "requirement",
        "body": format!("# Non-functional requirements\n\n{items}\n{table}"),
    });
    if let Some(doc_id) = doc_id {
        arguments["doc_id"] = json!(doc_id);
    }
    let saved = server.call("handoff_doc_save", arguments);
    saved["doc_id"].as_str().expect("doc_id").to_string()
}

fn nfr_document_body(server: &mut Server) -> String {
    let doc = server.call("handoff_doc_get", json!({ "doc_id": NFR_SLUG }));
    doc["body"].as_str().expect("body").to_string()
}

fn security(report: &Value) -> &Value {
    report["categories"]
        .as_array()
        .expect("categories")
        .iter()
        .find(|category| category["name"] == json!("security"))
        .expect("the security category")
}

/// The coordinator's reading of the `## Security minimum` table:
/// `(item, decision, requirement or reason)` per row.
fn decisions(body: &str) -> Vec<(String, String, String)> {
    let section = body
        .split("## Security minimum")
        .nth(1)
        .expect("the document has the section");
    section
        .lines()
        .filter(|line| line.starts_with("| ") && !line.starts_with("| Item"))
        .map(|line| {
            let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
            (
                cells[0].to_string(),
                cells[1].to_string(),
                cells[2].to_string(),
            )
        })
        .collect()
}

/// The "Gate 2 to 3" check of the security minimum: every one of the three
/// items is a requirement that names an `NFR-` item, or out of scope with a
/// reason. Returns the items that fail it.
fn undecided(body: &str) -> Vec<String> {
    let rows = decisions(body);
    ITEMS
        .iter()
        .filter(|item| {
            !rows.iter().any(|(name, decision, detail)| {
                name == *item
                    && match decision.as_str() {
                        "requirement" => detail.starts_with("NFR-"),
                        "out of scope" => !detail.is_empty(),
                        _ => false,
                    }
            })
        })
        .map(|item| item.to_string())
        .collect()
}

#[test]
fn the_switch_is_read_through_get_config_and_is_absent_by_default() {
    let sandbox = Sandbox::initialized("");
    let mut server = sandbox.server();
    let config = server.call("handoff_get_config", json!({}));
    assert!(config["onboarding"]["security_min_nfr"].is_null());

    server.ok(
        "handoff_update_config",
        json!({ "updates": { "onboarding.security_min_nfr": true } }),
    );
    let config = server.call("handoff_get_config", json!({}));
    assert_eq!(config["onboarding"]["security_min_nfr"], json!(true));

    let error = server.err(
        "handoff_update_config",
        json!({ "updates": { "onboarding.security_min_nfr": "yes" } }),
    );
    assert!(error.contains("onboarding"), "{error}");
}

#[test]
fn ac1_every_item_ends_as_an_nfr_item_or_a_reason_in_the_document() {
    let sandbox = Sandbox::initialized("[onboarding]\nsecurity_min_nfr = true\n");
    let mut server = sandbox.server();
    save_nfr_document(
        &mut server,
        None,
        FILED_ITEMS,
        &table([
            ("requirement", "NFR-SEC-001"),
            ("requirement", "NFR-SEC-002"),
            ("out of scope", "The tool has no third-party dependencies"),
        ]),
    );

    // The record the block asks for when an item was ruled out.
    let report = server.call(
        "handoff_trace_lint",
        json!({ "action": "nfr_check", "review": { "security": "applicable" } }),
    );
    assert_eq!(security(&report)["status"], json!("applicable"));
    assert_eq!(
        security(&report)["items"],
        json!(["NFR-SEC-001", "NFR-SEC-002"])
    );
    assert_eq!(report["unclassified"], json!([]));

    // The document holds a decision for each of the three items.
    let body = nfr_document_body(&mut server);
    let rows = decisions(&body);
    assert_eq!(
        rows.iter().map(|row| row.0.as_str()).collect::<Vec<_>>(),
        ITEMS
    );
    assert_eq!(rows[2].1, "out of scope");
    assert_eq!(rows[2].2, "The tool has no third-party dependencies");
    assert_eq!(undecided(&body), Vec::<String>::new());

    // The table is plain text of the document: it adds no item and no finding.
    let lint = server.call("handoff_trace_lint", json!({}));
    assert_eq!(lint["counts"]["error"], json!(0), "{lint}");
    let findings = lint["findings"].as_array().expect("findings");
    assert!(
        !findings
            .iter()
            .any(|finding| finding["rule"] == json!("id_like_heading")),
        "{lint}"
    );
    let quiet = server.call(
        "handoff_trace_lint",
        json!({ "rules": ["missing_acceptance", "ambiguous_word"] }),
    );
    assert_eq!(quiet["findings"], json!([]), "{quiet}");
}

#[test]
fn ac1_three_items_ruled_out_are_recorded_as_not_applicable_and_the_record_stays() {
    let sandbox = Sandbox::initialized("[onboarding]\nsecurity_min_nfr = true\n");
    let mut server = sandbox.server();
    save_nfr_document(
        &mut server,
        None,
        "",
        &table([
            ("out of scope", "The script reads no credentials"),
            ("out of scope", "It takes no input from outside the machine"),
            ("out of scope", "It has no third-party dependencies"),
        ]),
    );
    let report = server.call(
        "handoff_trace_lint",
        json!({ "action": "nfr_check", "review": { "security": "not_applicable" } }),
    );
    assert_eq!(security(&report)["status"], json!("not_applicable"));
    assert_eq!(security(&report)["items"], json!([]));

    // Phase 2 step 4 calls nfr_check again without `review`: the decision is kept.
    let again = server.call("handoff_trace_lint", json!({ "action": "nfr_check" }));
    assert_eq!(security(&again)["status"], json!("not_applicable"));
    assert_eq!(
        undecided(&nfr_document_body(&mut server)),
        Vec::<String>::new()
    );
}

#[test]
fn ac2_an_item_without_a_decision_fails_the_gate_check() {
    let sandbox = Sandbox::initialized("[onboarding]\nsecurity_min_nfr = true\n");
    let mut server = sandbox.server();
    let doc_id = save_nfr_document(
        &mut server,
        None,
        FILED_ITEMS,
        &table([
            ("requirement", "NFR-SEC-001"),
            ("requirement", "NFR-SEC-002"),
            ("undecided", ""),
        ]),
    );
    assert_eq!(
        undecided(&nfr_document_body(&mut server)),
        ["Dependency updates"]
    );

    // Neither "out of scope" without a reason nor "requirement" without an item passes.
    for (decision, detail) in [("out of scope", ""), ("requirement", "to be written")] {
        save_nfr_document(
            &mut server,
            Some(&doc_id),
            FILED_ITEMS,
            &table([
                ("requirement", "NFR-SEC-001"),
                ("requirement", "NFR-SEC-002"),
                (decision, detail),
            ]),
        );
        assert_eq!(
            undecided(&nfr_document_body(&mut server)),
            ["Dependency updates"],
            "{decision} / {detail}"
        );
    }

    // "undecided" is not a status the nfr_check record accepts either.
    let error = server.err(
        "handoff_trace_lint",
        json!({ "action": "nfr_check", "review": { "security": "undecided" } }),
    );
    assert!(error.contains("invalid status"), "{error}");
}

#[test]
fn ac3_without_the_switch_nfr_check_derives_the_status_as_before() {
    let sandbox = Sandbox::initialized("");
    let mut server = sandbox.server();
    // No NFR document, no review: security is simply not reviewed yet.
    let report = server.call("handoff_trace_lint", json!({ "action": "nfr_check" }));
    assert_eq!(security(&report)["status"], json!("not_reviewed"));

    // With a security NFR and no review, the status is derived from the items.
    save_nfr_document(&mut server, None, FILED_ITEMS, "");
    let report = server.call("handoff_trace_lint", json!({ "action": "nfr_check" }));
    assert_eq!(security(&report)["status"], json!("applicable"));
}
