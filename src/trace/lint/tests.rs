//! Unit tests for [`super::evaluate`] (wiki/260-vmodel-m2-design.md §4.3,
//! M2-08): structural/change/tailoring/drift rules, severity overrides,
//! `[[trace.lint.require]]` policy rules, and the deterministic output
//! order.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::*;
use crate::storage::config::{TraceLintRequireRule, TraceLintRequireWhen};
use crate::storage::docs::UnreadableDoc;
use crate::trace::types::{TaskLinkRole, TaskRequirementLink, TraceInput};

fn item(id: &str) -> crate::trace::types::TraceItemInput {
    crate::trace::types::TraceItemInput {
        stable_id: id.to_string(),
        doc_id: "doc".to_string(),
        layer: Some("requirement".to_string()),
        refines: Vec::new(),
        verifies: Vec::new(),
        method: None,
        has_test_refs: false,
        acceptance_labels: Vec::new(),
        derived: false,
        waived_axes: Vec::new(),
        def_hash: None,
        body_hash: None,
        link_baselines: BTreeMap::new(),
    }
}

fn empty_ctx() -> LintContext<'static> {
    static DOCS: &[DocMetadata] = &[];
    static META: std::sync::OnceLock<HashMap<String, ItemLintMeta>> = std::sync::OnceLock::new();
    static UNREADABLE: &[UnreadableDoc] = &[];
    static DRIFT: &[TaskIdsDrift] = &[];
    static WARNINGS: &[(String, String)] = &[];
    static RESYNCED: std::sync::OnceLock<HashSet<String>> = std::sync::OnceLock::new();
    LintContext {
        docs: DOCS,
        item_meta: META.get_or_init(HashMap::new),
        unreadable: UNREADABLE,
        task_ids_drift: DRIFT,
        per_doc_sync_warnings: WARNINGS,
        resynced_doc_slugs: RESYNCED.get_or_init(HashSet::new),
    }
}

#[test]
fn dangling_reference_is_reported_as_an_error_by_default() {
    let child = crate::trace::types::TraceItemInput {
        refines: vec!["REQ-999".to_string()],
        ..item("SPEC-001")
    };
    let input = TraceInput {
        items: vec![child],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let ctx = empty_ctx();
    let config = crate::storage::config::TraceLintConfig::default();

    let findings = evaluate(&graph, &input, &ctx, &config, None);

    let dangling: Vec<_> = findings.iter().filter(|f| f.rule == "dangling").collect();
    assert_eq!(dangling.len(), 1);
    assert_eq!(dangling[0].severity, Severity::Error);
    assert_eq!(dangling[0].item.as_deref(), Some("SPEC-001"));
}

#[test]
fn severity_override_downgrades_a_rule_and_off_disables_it() {
    let child = crate::trace::types::TraceItemInput {
        refines: vec!["REQ-999".to_string()],
        ..item("SPEC-001")
    };
    let input = TraceInput {
        items: vec![child],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let ctx = empty_ctx();

    let mut config = crate::storage::config::TraceLintConfig::default();
    config
        .rules
        .insert("dangling".to_string(), "info".to_string());
    let findings = evaluate(&graph, &input, &ctx, &config, None);
    let dangling = findings.iter().find(|f| f.rule == "dangling").unwrap();
    assert_eq!(dangling.severity, Severity::Info);

    config
        .rules
        .insert("dangling".to_string(), "off".to_string());
    let findings = evaluate(&graph, &input, &ctx, &config, None);
    assert!(
        !findings.iter().any(|f| f.rule == "dangling"),
        "severity=off must suppress the rule entirely: {findings:?}"
    );
}

#[test]
fn rules_filter_restricts_evaluation_to_the_named_ids() {
    let req = item("REQ-001");
    let child = crate::trace::types::TraceItemInput {
        refines: vec!["REQ-999".to_string()],
        ..item("SPEC-001")
    };
    let input = TraceInput {
        items: vec![req, child],
        // `acceptance` (requirement's paired right-side layer) must be explicitly in use
        // so REQ-001's horizontal axis is genuinely `uncovered` (->
        // `unverified`) rather than `na` (no paired verification layer in
        // use at all, §2.1 — "使用中でない層は対象外").
        configured_layers: vec!["requirement".to_string(), "acceptance".to_string()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let ctx = empty_ctx();
    let config = crate::storage::config::TraceLintConfig::default();

    let only_unverified: HashSet<String> = ["unverified".to_string()].into_iter().collect();
    let findings = evaluate(&graph, &input, &ctx, &config, Some(&only_unverified));

    assert!(findings.iter().all(|f| f.rule == "unverified"));
    assert!(
        !findings.is_empty(),
        "REQ-001 with no verifier must still produce at least one unverified finding"
    );
}

#[test]
fn findings_are_sorted_by_severity_then_rule_then_item_natural_order() {
    let req2 = crate::trace::types::TraceItemInput {
        refines: vec!["REQ-999".to_string()],
        ..item("SPEC-2")
    };
    let req10 = crate::trace::types::TraceItemInput {
        refines: vec!["REQ-999".to_string()],
        ..item("SPEC-10")
    };
    let unverified_req = item("REQ-001");
    let input = TraceInput {
        items: vec![req10, req2, unverified_req],
        configured_layers: vec!["requirement".to_string(), "acceptance".to_string()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let ctx = empty_ctx();
    let config = crate::storage::config::TraceLintConfig::default();

    let findings = evaluate(&graph, &input, &ctx, &config, None);

    // `dangling` (error) must sort before `unverified` (warning); within
    // `dangling`, SPEC-2 (natural order) must sort before SPEC-10.
    let rule_order: Vec<&str> = findings.iter().map(|f| f.rule.as_str()).collect();
    let first_dangling = rule_order.iter().position(|r| *r == "dangling").unwrap();
    let first_unverified = rule_order.iter().position(|r| *r == "unverified").unwrap();
    assert!(
        first_dangling < first_unverified,
        "error severity must sort before warning: {rule_order:?}"
    );
    let dangling_items: Vec<&str> = findings
        .iter()
        .filter(|f| f.rule == "dangling")
        .map(|f| f.item.as_deref().unwrap())
        .collect();
    assert_eq!(
        dangling_items,
        vec!["SPEC-2", "SPEC-10"],
        "natural order must put SPEC-2 before SPEC-10 (not lexicographic)"
    );
}

#[test]
fn suspect_link_is_reported_as_a_warning_finding() {
    let mut baselines = BTreeMap::new();
    baselines.insert("REQ-001".to_string(), "old-hash".to_string());
    let upstream = crate::trace::types::TraceItemInput {
        def_hash: Some("new-hash".to_string()),
        ..item("REQ-001")
    };
    let child = crate::trace::types::TraceItemInput {
        refines: vec!["REQ-001".to_string()],
        link_baselines: baselines,
        ..item("SPEC-001")
    };
    let input = TraceInput {
        items: vec![upstream, child],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let ctx = empty_ctx();
    let config = crate::storage::config::TraceLintConfig::default();

    let findings = evaluate(&graph, &input, &ctx, &config, None);
    let f = findings
        .iter()
        .find(|f| f.rule == "suspect_link")
        .expect("suspect_link finding");
    assert_eq!(f.severity, Severity::Warning);
    assert_eq!(f.item.as_deref(), Some("SPEC-001"));
}

#[test]
fn unbaselined_link_is_reported_as_info() {
    let upstream = crate::trace::types::TraceItemInput {
        def_hash: Some("hash".to_string()),
        ..item("REQ-001")
    };
    let child = crate::trace::types::TraceItemInput {
        refines: vec!["REQ-001".to_string()],
        ..item("SPEC-001")
    };
    let input = TraceInput {
        items: vec![upstream, child],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let ctx = empty_ctx();
    let config = crate::storage::config::TraceLintConfig::default();

    let findings = evaluate(&graph, &input, &ctx, &config, None);
    let f = findings
        .iter()
        .find(|f| f.rule == "unbaselined")
        .expect("unbaselined finding");
    assert_eq!(f.severity, Severity::Info);
    assert_eq!(f.item.as_deref(), Some("SPEC-001"));
}

#[test]
fn frontmatter_invalid_reports_every_unreadable_document() {
    let input = TraceInput::default();
    let graph = TraceGraph::build(&input);
    let unreadable = vec![UnreadableDoc {
        slug: "broken".to_string(),
        error: "YAML parse error".to_string(),
        line: Some(3),
    }];
    let item_meta = HashMap::new();
    let drift: Vec<TaskIdsDrift> = Vec::new();
    let warnings: Vec<(String, String)> = Vec::new();
    let resynced = HashSet::new();
    let ctx = LintContext {
        docs: &[],
        item_meta: &item_meta,
        unreadable: &unreadable,
        task_ids_drift: &drift,
        per_doc_sync_warnings: &warnings,
        resynced_doc_slugs: &resynced,
    };
    let config = crate::storage::config::TraceLintConfig::default();

    let findings = evaluate(&graph, &input, &ctx, &config, None);
    let f = findings
        .iter()
        .find(|f| f.rule == "frontmatter_invalid")
        .expect("frontmatter_invalid finding");
    assert_eq!(f.severity, Severity::Error);
    assert_eq!(f.doc.as_deref(), Some("broken"));
    assert_eq!(f.message, "YAML parse error");
}

#[test]
fn task_ids_drift_finding_comes_from_the_supplied_drift_list() {
    let input = TraceInput {
        items: vec![item("REQ-001")],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let drift = vec![TaskIdsDrift::item(
        "REQ-001",
        vec!["t1".to_string()],
        vec!["t2".to_string()],
    )];
    let item_meta = HashMap::new();
    let unreadable: Vec<UnreadableDoc> = Vec::new();
    let warnings: Vec<(String, String)> = Vec::new();
    let resynced = HashSet::new();
    let ctx = LintContext {
        docs: &[],
        item_meta: &item_meta,
        unreadable: &unreadable,
        task_ids_drift: &drift,
        per_doc_sync_warnings: &warnings,
        resynced_doc_slugs: &resynced,
    };
    let config = crate::storage::config::TraceLintConfig::default();

    let findings = evaluate(&graph, &input, &ctx, &config, None);
    let f = findings
        .iter()
        .find(|f| f.rule == "task_ids_drift")
        .expect("task_ids_drift finding");
    assert_eq!(f.severity, Severity::Info);
    assert_eq!(f.item.as_deref(), Some("REQ-001"));
}

/// M2-15 (wiki/260 §4.8/FR-601): a document-level drift (`doc.task_ids` vs
/// the task side's `TaskLink{doc}` entries) is reported by the same
/// `task_ids_drift` rule, but with `item: None` / `doc: Some(doc_slug)`
/// instead — the document-level self-repair never removes a stored id with
/// no matching `TaskLink{doc}`, so this is the only place that disagreement
/// ever surfaces.
#[test]
fn task_ids_drift_finding_reports_document_level_drift() {
    let input = TraceInput::default();
    let graph = TraceGraph::build(&input);
    let drift = vec![TaskIdsDrift::doc(
        "some-doc-slug",
        vec!["t1".to_string(), "t-orphan".to_string()],
        vec!["t1".to_string()],
    )];
    let item_meta = HashMap::new();
    let unreadable: Vec<UnreadableDoc> = Vec::new();
    let warnings: Vec<(String, String)> = Vec::new();
    let resynced = HashSet::new();
    let ctx = LintContext {
        docs: &[],
        item_meta: &item_meta,
        unreadable: &unreadable,
        task_ids_drift: &drift,
        per_doc_sync_warnings: &warnings,
        resynced_doc_slugs: &resynced,
    };
    let config = crate::storage::config::TraceLintConfig::default();

    let findings = evaluate(&graph, &input, &ctx, &config, None);
    let f = findings
        .iter()
        .find(|f| f.rule == "task_ids_drift")
        .expect("task_ids_drift finding");
    assert_eq!(f.severity, Severity::Info);
    assert_eq!(f.item, None, "document-level drift carries no item id");
    assert_eq!(f.doc.as_deref(), Some("some-doc-slug"));
    assert!(
        f.message.contains("some-doc-slug"),
        "message should name the document: {}",
        f.message
    );
}

#[test]
fn task_link_dangling_reports_a_task_link_to_a_nonexistent_item() {
    let input = TraceInput {
        items: vec![item("REQ-001")],
        task_requirement_links: vec![TaskRequirementLink {
            task_id: "t1".to_string(),
            stable_id: "REQ-999".to_string(),
            role: TaskLinkRole::Implements,
            baseline_hash: None,
        }],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let ctx = empty_ctx();
    let config = crate::storage::config::TraceLintConfig::default();

    let findings = evaluate(&graph, &input, &ctx, &config, None);
    let f = findings
        .iter()
        .find(|f| f.rule == "task_link_dangling")
        .expect("task_link_dangling finding");
    assert_eq!(f.severity, Severity::Warning);
    assert_eq!(f.task.as_deref(), Some("t1"));
    assert_eq!(f.item.as_deref(), Some("REQ-999"));
}

#[test]
fn unsynced_body_reports_every_resynced_doc_slug() {
    let input = TraceInput::default();
    let graph = TraceGraph::build(&input);
    let item_meta = HashMap::new();
    let unreadable: Vec<UnreadableDoc> = Vec::new();
    let drift: Vec<TaskIdsDrift> = Vec::new();
    let warnings: Vec<(String, String)> = Vec::new();
    let resynced: HashSet<String> = ["req-doc".to_string()].into_iter().collect();
    let ctx = LintContext {
        docs: &[],
        item_meta: &item_meta,
        unreadable: &unreadable,
        task_ids_drift: &drift,
        per_doc_sync_warnings: &warnings,
        resynced_doc_slugs: &resynced,
    };
    let config = crate::storage::config::TraceLintConfig::default();

    let findings = evaluate(&graph, &input, &ctx, &config, None);
    let f = findings
        .iter()
        .find(|f| f.rule == "unsynced_body")
        .expect("unsynced_body finding");
    assert_eq!(f.severity, Severity::Warning);
    assert_eq!(f.doc.as_deref(), Some("req-doc"));
}

#[test]
fn id_like_heading_and_unlabeled_acceptance_and_invalid_waiver_come_from_per_doc_sync_warnings() {
    let input = TraceInput::default();
    let graph = TraceGraph::build(&input);
    let item_meta = HashMap::new();
    let unreadable: Vec<UnreadableDoc> = Vec::new();
    let drift: Vec<TaskIdsDrift> = Vec::new();
    let warnings = vec![
        (
            "doc-a".to_string(),
            "line 5: ID-like heading ignored: \"FOO-1 bar\"".to_string(),
        ),
        (
            "doc-b".to_string(),
            "line 6: item \"REQ-001\" acceptance bullet has no label, assigned \"AC1\" by \
             position"
                .to_string(),
        ),
        (
            "doc-c".to_string(),
            "line 7: item \"REQ-002\" attribute \"waive-verify\" has an empty reason, ignored"
                .to_string(),
        ),
    ];
    let resynced = HashSet::new();
    let ctx = LintContext {
        docs: &[],
        item_meta: &item_meta,
        unreadable: &unreadable,
        task_ids_drift: &drift,
        per_doc_sync_warnings: &warnings,
        resynced_doc_slugs: &resynced,
    };
    let config = crate::storage::config::TraceLintConfig::default();

    let findings = evaluate(&graph, &input, &ctx, &config, None);

    let id_like = findings
        .iter()
        .find(|f| f.rule == "id_like_heading")
        .expect("id_like_heading finding");
    assert_eq!(id_like.severity, Severity::Info);
    assert_eq!(id_like.doc.as_deref(), Some("doc-a"));

    let unlabeled = findings
        .iter()
        .find(|f| f.rule == "unlabeled_acceptance")
        .expect("unlabeled_acceptance finding");
    assert_eq!(unlabeled.severity, Severity::Warning);
    assert_eq!(unlabeled.doc.as_deref(), Some("doc-b"));

    let invalid_waiver = findings
        .iter()
        .find(|f| f.rule == "invalid_waiver")
        .expect("invalid_waiver finding");
    assert_eq!(invalid_waiver.severity, Severity::Warning);
    assert_eq!(invalid_waiver.doc.as_deref(), Some("doc-c"));
}

#[test]
fn unknown_acceptance_ref_warns_when_the_target_lacks_the_ac_label() {
    let upstream = crate::trace::types::TraceItemInput {
        acceptance_labels: vec!["AC1".to_string()],
        ..item("REQ-001")
    };
    let verifier = crate::trace::types::TraceItemInput {
        verifies: vec!["REQ-001#AC2".to_string()],
        ..item("ST-001")
    };
    let input = TraceInput {
        items: vec![upstream, verifier],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let ctx = empty_ctx();
    let config = crate::storage::config::TraceLintConfig::default();

    let findings = evaluate(&graph, &input, &ctx, &config, None);
    let f = findings
        .iter()
        .find(|f| f.rule == "unknown_acceptance_ref")
        .expect("unknown_acceptance_ref finding");
    assert_eq!(f.severity, Severity::Warning);
    assert_eq!(f.item.as_deref(), Some("ST-001"));
}

#[test]
fn require_rule_flags_an_item_matching_when_but_not_satisfying_need() {
    let req = crate::trace::types::TraceItemInput {
        method: Some("manual".to_string()),
        ..item("REQ-001")
    };
    let input = TraceInput {
        items: vec![req],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let mut item_meta = HashMap::new();
    item_meta.insert(
        "REQ-001".to_string(),
        ItemLintMeta {
            doc_slug: "req-doc".to_string(),
            priority: Some("P0".to_string()),
            approval: "draft".to_string(),
        },
    );
    let unreadable: Vec<UnreadableDoc> = Vec::new();
    let drift: Vec<TaskIdsDrift> = Vec::new();
    let warnings: Vec<(String, String)> = Vec::new();
    let resynced = HashSet::new();
    let ctx = LintContext {
        docs: &[],
        item_meta: &item_meta,
        unreadable: &unreadable,
        task_ids_drift: &drift,
        per_doc_sync_warnings: &warnings,
        resynced_doc_slugs: &resynced,
    };
    let mut config = crate::storage::config::TraceLintConfig::default();
    config.require.push(TraceLintRequireRule {
        id: "p0-needs-verification".to_string(),
        when: TraceLintRequireWhen {
            layer: Some("requirement".to_string()),
            priority: vec!["P0".to_string(), "P1".to_string()],
            method: None,
            doc: None,
            approval: None,
        },
        need: "verified_by".to_string(),
        severity: Some("error".to_string()),
    });

    let findings = evaluate(&graph, &input, &ctx, &config, None);
    let f = findings
        .iter()
        .find(|f| f.rule == "p0-needs-verification")
        .expect("p0-needs-verification finding (REQ-001 has no verifier)");
    assert_eq!(f.severity, Severity::Error);
    assert_eq!(f.item.as_deref(), Some("REQ-001"));
}

#[test]
fn require_rule_is_satisfied_once_the_item_has_a_verifier() {
    let req = crate::trace::types::TraceItemInput {
        method: None,
        ..item("REQ-001")
    };
    let verifier = crate::trace::types::TraceItemInput {
        layer: Some("system_test".to_string()),
        verifies: vec!["REQ-001".to_string()],
        method: Some("manual".to_string()),
        ..item("ST-001")
    };
    let input = TraceInput {
        items: vec![req, verifier],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let mut item_meta = HashMap::new();
    item_meta.insert(
        "REQ-001".to_string(),
        ItemLintMeta {
            doc_slug: "req-doc".to_string(),
            priority: Some("P0".to_string()),
            approval: "draft".to_string(),
        },
    );
    let unreadable: Vec<UnreadableDoc> = Vec::new();
    let drift: Vec<TaskIdsDrift> = Vec::new();
    let warnings: Vec<(String, String)> = Vec::new();
    let resynced = HashSet::new();
    let ctx = LintContext {
        docs: &[],
        item_meta: &item_meta,
        unreadable: &unreadable,
        task_ids_drift: &drift,
        per_doc_sync_warnings: &warnings,
        resynced_doc_slugs: &resynced,
    };
    let mut config = crate::storage::config::TraceLintConfig::default();
    config.require.push(TraceLintRequireRule {
        id: "p0-needs-verification".to_string(),
        when: TraceLintRequireWhen {
            layer: Some("requirement".to_string()),
            priority: vec!["P0".to_string()],
            method: None,
            doc: None,
            approval: None,
        },
        need: "verified_by".to_string(),
        severity: None,
    });

    let findings = evaluate(&graph, &input, &ctx, &config, None);
    assert!(
        !findings.iter().any(|f| f.rule == "p0-needs-verification"),
        "REQ-001 has a verifier, so the require rule must not fire: {findings:?}"
    );
}

/// `evaluate`'s own defensive `continue` (a unit test bypassing
/// `validate_lint_config`) must still never panic or produce a finding with
/// an empty rule id — but the real gate is `validate_lint_config` itself
/// (next tests), which every caller is expected to run first and which must
/// *reject*, not silently accept, an entry like this.
#[test]
fn evaluate_itself_never_panics_on_an_invalid_require_rule_entry() {
    let input = TraceInput {
        items: vec![item("REQ-001")],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let ctx = empty_ctx();
    let mut config = crate::storage::config::TraceLintConfig::default();
    config.require.push(TraceLintRequireRule::default());

    let findings = evaluate(&graph, &input, &ctx, &config, None);
    assert!(!findings.iter().any(|f| f.rule.is_empty()));
}

#[test]
fn validate_lint_config_rejects_a_require_entry_with_no_id_or_need() {
    let mut config = crate::storage::config::TraceLintConfig::default();
    config.require.push(TraceLintRequireRule::default());

    let err = validate_lint_config(&config).expect_err("empty id/need must be rejected");
    assert!(err.contains("id"), "{err}");
}

#[test]
fn validate_lint_config_rejects_a_require_entry_with_an_unknown_need() {
    let mut config = crate::storage::config::TraceLintConfig::default();
    config.require.push(TraceLintRequireRule {
        id: "p0-needs-verification".to_string(),
        when: TraceLintRequireWhen::default(),
        need: "verified".to_string(), // typo of "verified_by"
        severity: None,
    });

    let err = validate_lint_config(&config).expect_err("unknown need must be rejected");
    assert!(err.contains("need"), "{err}");
    assert!(err.contains("verified"), "{err}");
}

#[test]
fn validate_lint_config_rejects_a_require_entry_with_an_unknown_severity() {
    let mut config = crate::storage::config::TraceLintConfig::default();
    config.require.push(TraceLintRequireRule {
        id: "p0-needs-verification".to_string(),
        when: TraceLintRequireWhen::default(),
        need: "verified_by".to_string(),
        severity: Some("critical".to_string()),
    });

    let err = validate_lint_config(&config).expect_err("unknown severity must be rejected");
    assert!(err.contains("severity"), "{err}");
}

#[test]
fn validate_lint_config_rejects_an_override_for_an_unknown_rule_id() {
    let mut config = crate::storage::config::TraceLintConfig::default();
    config
        .rules
        .insert("unverfied".to_string(), "error".to_string()); // typo of "unverified"

    let err = validate_lint_config(&config).expect_err("unknown rule id override must be rejected");
    assert!(err.contains("unverfied"), "{err}");
}

#[test]
fn validate_lint_config_rejects_an_override_with_an_unknown_severity_value() {
    let mut config = crate::storage::config::TraceLintConfig::default();
    config
        .rules
        .insert("unverified".to_string(), "critical".to_string());

    let err = validate_lint_config(&config).expect_err("unknown severity value must be rejected");
    assert!(err.contains("critical"), "{err}");
}

#[test]
fn validate_lint_config_accepts_an_override_naming_a_require_rules_own_id() {
    let mut config = crate::storage::config::TraceLintConfig::default();
    config.require.push(TraceLintRequireRule {
        id: "p0-needs-verification".to_string(),
        when: TraceLintRequireWhen::default(),
        need: "verified_by".to_string(),
        severity: None,
    });
    config
        .rules
        .insert("p0-needs-verification".to_string(), "warning".to_string());

    assert!(validate_lint_config(&config).is_ok());
}

#[test]
fn validate_lint_config_accepts_the_default_empty_config() {
    let config = crate::storage::config::TraceLintConfig::default();
    assert!(validate_lint_config(&config).is_ok());
}

#[test]
fn is_known_rule_id_recognizes_both_builtin_and_require_ids() {
    let mut config = crate::storage::config::TraceLintConfig::default();
    config.require.push(TraceLintRequireRule {
        id: "p0-needs-verification".to_string(),
        when: TraceLintRequireWhen::default(),
        need: "verified_by".to_string(),
        severity: None,
    });

    assert!(is_known_rule_id("unverified", &config));
    assert!(is_known_rule_id("p0-needs-verification", &config));
    assert!(!is_known_rule_id("unverfied", &config));
}
