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
        ac_hashes: Default::default(),
        implicit_of: None,
        derived: false,
        waived_axes: Vec::new(),
        def_hash: None,
        body_hash: None,
        link_baselines: BTreeMap::new(),
        needs: None,
        approval: "draft".to_string(),
        tree: Default::default(),
    }
}

fn empty_ctx() -> LintContext<'static> {
    static DOCS: &[DocMetadata] = &[];
    static META: std::sync::OnceLock<HashMap<String, ItemLintMeta>> = std::sync::OnceLock::new();
    static UNREADABLE: &[UnreadableDoc] = &[];
    static DRIFT: &[TaskIdsDrift] = &[];
    static WARNINGS: &[(String, String)] = &[];
    static RESYNCED: std::sync::OnceLock<HashSet<String>> = std::sync::OnceLock::new();
    static TREE_TEXT: std::sync::OnceLock<HashMap<String, ItemTreeText>> =
        std::sync::OnceLock::new();
    LintContext {
        docs: DOCS,
        item_meta: META.get_or_init(HashMap::new),
        unreadable: UNREADABLE,
        task_ids_drift: DRIFT,
        per_doc_sync_warnings: WARNINGS,
        resynced_doc_slugs: RESYNCED.get_or_init(HashSet::new),
        tree_text: TREE_TEXT.get_or_init(HashMap::new),
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
    // t-vfix.6: the message reads child -> upstream (the upstream is what
    // changed), not "REQ-001 refines SPEC-001".
    assert_eq!(
        f.message,
        "SPEC-001 refines REQ-001, which has changed since this refines link's baseline was recorded"
    );
}

#[test]
fn suspect_verifies_link_message_names_the_verifier_first() {
    let mut baselines = BTreeMap::new();
    baselines.insert("FR-001".to_string(), "old-hash".to_string());
    let upstream = crate::trace::types::TraceItemInput {
        def_hash: Some("new-hash".to_string()),
        ..item("FR-001")
    };
    let verifier = crate::trace::types::TraceItemInput {
        verifies: vec!["FR-001".to_string()],
        link_baselines: baselines,
        ..item("AT-001")
    };
    let input = TraceInput {
        items: vec![upstream, verifier],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let findings = evaluate(
        &graph,
        &input,
        &empty_ctx(),
        &crate::storage::config::TraceLintConfig::default(),
        None,
    );
    let f = findings
        .iter()
        .find(|f| f.rule == "suspect_link")
        .expect("suspect_link finding");
    assert!(
        f.message
            .starts_with("AT-001 verifies FR-001, which has changed"),
        "{}",
        f.message
    );
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

/// M3 (wiki/270-vmodel-m3-design.md §2.1/§3.1, M3-01, FR-202): a verifier
/// from a layer not in its target's `needs` set is reported as
/// `unwanted_coverage` (info by default), distinct from the (separately
/// still-uncovered) `unverified` gap for the needed layer.
#[test]
fn unwanted_coverage_reports_a_verifier_outside_the_needs_set() {
    let req = crate::trace::types::TraceItemInput {
        layer: Some("requirement".to_string()),
        needs: Some(vec!["acceptance".to_string()]),
        ..item("REQ-001")
    };
    let verifier = crate::trace::types::TraceItemInput {
        layer: Some("system_test".to_string()),
        verifies: vec!["REQ-001".to_string()],
        ..item("ST-001")
    };
    let input = TraceInput {
        items: vec![req, verifier],
        configured_layers: vec![
            "requirement".to_string(),
            "acceptance".to_string(),
            "basic_spec".to_string(),
            "system_test".to_string(),
        ],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let ctx = empty_ctx();
    let config = crate::storage::config::TraceLintConfig::default();

    let findings = evaluate(&graph, &input, &ctx, &config, None);
    let f = findings
        .iter()
        .find(|f| f.rule == "unwanted_coverage")
        .expect("unwanted_coverage finding");
    assert_eq!(f.severity, Severity::Info);
    assert_eq!(f.item.as_deref(), Some("REQ-001"));
    assert!(f.message.contains("ST-001"));
}

/// `unwanted_coverage` is absent when every verifier's layer is in the
/// target's `needs` set.
#[test]
fn unwanted_coverage_absent_when_every_verifier_is_in_the_needs_set() {
    let req = crate::trace::types::TraceItemInput {
        layer: Some("requirement".to_string()),
        needs: Some(vec!["acceptance".to_string()]),
        ..item("REQ-002")
    };
    let verifier = crate::trace::types::TraceItemInput {
        layer: Some("acceptance".to_string()),
        verifies: vec!["REQ-002".to_string()],
        ..item("AT-002")
    };
    let input = TraceInput {
        items: vec![req, verifier],
        configured_layers: vec!["requirement".to_string(), "acceptance".to_string()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let ctx = empty_ctx();
    let config = crate::storage::config::TraceLintConfig::default();

    let findings = evaluate(&graph, &input, &ctx, &config, None);
    assert!(!findings.iter().any(|f| f.rule == "unwanted_coverage"));
}

/// `[trace.lint.rules] unwanted_coverage = "error"` changes the severity
/// (§3.1: "severity は info から開始。`[trace.lint.rules]` で error に変更可
/// 能").
#[test]
fn unwanted_coverage_severity_is_overridable() {
    let req = crate::trace::types::TraceItemInput {
        layer: Some("requirement".to_string()),
        needs: Some(vec!["acceptance".to_string()]),
        ..item("REQ-003")
    };
    let verifier = crate::trace::types::TraceItemInput {
        layer: Some("system_test".to_string()),
        verifies: vec!["REQ-003".to_string()],
        ..item("ST-003")
    };
    let input = TraceInput {
        items: vec![req, verifier],
        configured_layers: vec![
            "requirement".to_string(),
            "acceptance".to_string(),
            "basic_spec".to_string(),
            "system_test".to_string(),
        ],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let ctx = empty_ctx();
    let mut config = crate::storage::config::TraceLintConfig::default();
    config
        .rules
        .insert("unwanted_coverage".to_string(), "error".to_string());

    let findings = evaluate(&graph, &input, &ctx, &config, None);
    let f = findings
        .iter()
        .find(|f| f.rule == "unwanted_coverage")
        .expect("unwanted_coverage finding");
    assert_eq!(f.severity, Severity::Error);
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
        tree_text: &HashMap::new(),
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
        tree_text: &HashMap::new(),
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
        tree_text: &HashMap::new(),
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
        tree_text: &HashMap::new(),
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
        tree_text: &HashMap::new(),
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

/// M3 (t377.5): `attribute_after_body` surfaces `layer_parse.rs`'s
/// `ParseWarningKind::AttributeAfterBody` the same way `id_like_heading`/
/// `unlabeled_acceptance`/`invalid_waiver` surface their own
/// `ParseWarningKind`s — a pattern-match on the rendered `Display` text in
/// `per_doc_sync_warnings`, not a new structured side-channel.
#[test]
fn attribute_after_body_comes_from_per_doc_sync_warnings() {
    let input = TraceInput::default();
    let graph = TraceGraph::build(&input);
    let item_meta = HashMap::new();
    let unreadable: Vec<UnreadableDoc> = Vec::new();
    let drift: Vec<TaskIdsDrift> = Vec::new();
    let warnings = vec![(
        "doc-d".to_string(),
        "line 8: item \"REQ-030\" attribute line \"priority\" appears after body text and is \
         ignored (attribute lines must be in the first bullet block right after the heading)"
            .to_string(),
    )];
    let resynced = HashSet::new();
    let ctx = LintContext {
        docs: &[],
        item_meta: &item_meta,
        unreadable: &unreadable,
        task_ids_drift: &drift,
        per_doc_sync_warnings: &warnings,
        resynced_doc_slugs: &resynced,
        tree_text: &HashMap::new(),
    };
    let config = crate::storage::config::TraceLintConfig::default();

    let findings = evaluate(&graph, &input, &ctx, &config, None);

    let finding = findings
        .iter()
        .find(|f| f.rule == "attribute_after_body")
        .expect("attribute_after_body finding");
    assert_eq!(finding.severity, Severity::Warning);
    assert_eq!(finding.doc.as_deref(), Some("doc-d"));
    assert!(finding.message.contains("priority"));
}

/// The same rule must be suppressible via `rules` filter / `[trace.lint.rules]`
/// override like any other built-in — verified by turning it `"off"`.
#[test]
fn attribute_after_body_can_be_turned_off() {
    let input = TraceInput::default();
    let graph = TraceGraph::build(&input);
    let item_meta = HashMap::new();
    let unreadable: Vec<UnreadableDoc> = Vec::new();
    let drift: Vec<TaskIdsDrift> = Vec::new();
    let warnings = vec![(
        "doc-d".to_string(),
        "line 8: item \"REQ-030\" attribute line \"priority\" appears after body text and is \
         ignored (attribute lines must be in the first bullet block right after the heading)"
            .to_string(),
    )];
    let resynced = HashSet::new();
    let ctx = LintContext {
        docs: &[],
        item_meta: &item_meta,
        unreadable: &unreadable,
        task_ids_drift: &drift,
        per_doc_sync_warnings: &warnings,
        resynced_doc_slugs: &resynced,
        tree_text: &HashMap::new(),
    };
    let mut config = crate::storage::config::TraceLintConfig::default();
    config
        .rules
        .insert("attribute_after_body".to_string(), "off".to_string());

    let findings = evaluate(&graph, &input, &ctx, &config, None);
    assert!(!findings.iter().any(|f| f.rule == "attribute_after_body"));
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
            title: String::new(),
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
        tree_text: &HashMap::new(),
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
            title: String::new(),
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
        tree_text: &HashMap::new(),
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

/// M3-05 (wiki/270-vmodel-m3-design.md §2.3/§4.3, FR-406): `when.approval =
/// ["review", "approved"]` matches an item in either state — here `REQ-001`
/// is `review`, so the require rule fires (its `no_suspect` need is
/// deliberately unmet by a suspect pre-seeded in `item_meta`... actually this
/// rule only inspects `when`, so any `need` that is unmet suffices; `passing`
/// with no recorded run is simplest).
fn evaluate_with_approval_when(approval: &str, when_values: &[&str]) -> Vec<LintFinding> {
    let req = item("REQ-001");
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
            priority: None,
            approval: approval.to_string(),
            title: String::new(),
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
        tree_text: &HashMap::new(),
    };
    let mut config = crate::storage::config::TraceLintConfig::default();
    config.require.push(TraceLintRequireRule {
        id: "approval-gated".to_string(),
        when: TraceLintRequireWhen {
            layer: None,
            priority: Vec::new(),
            method: None,
            doc: None,
            approval: Some(when_values.iter().map(|s| s.to_string()).collect()),
        },
        // REQ-001 has no recorded run at all -> never `passing`, so the rule
        // fires whenever `when` matches.
        need: "passing".to_string(),
        severity: None,
    });

    evaluate(&graph, &input, &ctx, &config, None)
}

#[test]
fn require_when_approval_array_matches_either_of_its_listed_values() {
    for approval in ["review", "approved"] {
        let findings = evaluate_with_approval_when(approval, &["review", "approved"]);
        assert!(
            findings.iter().any(|f| f.rule == "approval-gated"),
            "when.approval=[review,approved] must match approval={approval}: {findings:?}"
        );
    }
}

#[test]
fn require_when_approval_array_does_not_match_a_value_outside_the_list() {
    let findings = evaluate_with_approval_when("draft", &["review", "approved"]);
    assert!(
        !findings.iter().any(|f| f.rule == "approval-gated"),
        "when.approval=[review,approved] must not match approval=draft: {findings:?}"
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

// --- M3-10 (wiki/270-vmodel-m3-design.md §4.6, FR-504): quality rules ---

fn ctx_with_titles(titles: &[(&str, &str)]) -> LintContext<'static> {
    let mut item_meta = HashMap::new();
    for (id, title) in titles {
        item_meta.insert(
            id.to_string(),
            ItemLintMeta {
                doc_slug: "doc".to_string(),
                priority: None,
                approval: "draft".to_string(),
                title: title.to_string(),
            },
        );
    }
    let leaked_meta: &'static HashMap<String, ItemLintMeta> = Box::leak(Box::new(item_meta));
    static DOCS: &[DocMetadata] = &[];
    static UNREADABLE: &[UnreadableDoc] = &[];
    static DRIFT: &[TaskIdsDrift] = &[];
    static WARNINGS: &[(String, String)] = &[];
    static RESYNCED: std::sync::OnceLock<HashSet<String>> = std::sync::OnceLock::new();
    static TREE_TEXT: std::sync::OnceLock<HashMap<String, ItemTreeText>> =
        std::sync::OnceLock::new();
    LintContext {
        docs: DOCS,
        item_meta: leaked_meta,
        unreadable: UNREADABLE,
        task_ids_drift: DRIFT,
        per_doc_sync_warnings: WARNINGS,
        resynced_doc_slugs: RESYNCED.get_or_init(HashSet::new),
        tree_text: TREE_TEXT.get_or_init(HashMap::new),
    }
}

#[test]
fn ambiguous_word_flags_a_japanese_ambiguous_term_in_the_title() {
    let req = item("REQ-900");
    let input = TraceInput {
        items: vec![req],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let ctx = ctx_with_titles(&[("REQ-900", "ログは適切に記録される")]);
    let config = crate::storage::config::TraceLintConfig::default();

    let findings = evaluate(&graph, &input, &ctx, &config, None);
    let f = findings
        .iter()
        .find(|f| f.rule == "ambiguous_word")
        .expect("ambiguous_word finding");
    assert_eq!(f.severity, Severity::Info);
    assert_eq!(f.item.as_deref(), Some("REQ-900"));
}

#[test]
fn ambiguous_word_flags_an_english_ambiguous_term_in_the_title_case_insensitively() {
    let req = item("REQ-901");
    let input = TraceInput {
        items: vec![req],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let ctx = ctx_with_titles(&[("REQ-901", "Retry As Needed on failure")]);
    let config = crate::storage::config::TraceLintConfig::default();

    let findings = evaluate(&graph, &input, &ctx, &config, None);
    assert!(
        findings
            .iter()
            .any(|f| f.rule == "ambiguous_word" && f.item.as_deref() == Some("REQ-901")),
        "{findings:?}"
    );
}

#[test]
fn ambiguous_word_is_silent_for_an_unambiguous_title() {
    let req = item("REQ-902");
    let input = TraceInput {
        items: vec![req],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let ctx = ctx_with_titles(&[("REQ-902", "The system shall log every request")]);
    let config = crate::storage::config::TraceLintConfig::default();

    let findings = evaluate(&graph, &input, &ctx, &config, None);
    assert!(
        !findings.iter().any(|f| f.rule == "ambiguous_word"),
        "{findings:?}"
    );
}

#[test]
fn missing_acceptance_flags_a_requirement_with_no_acceptance_block() {
    let req = item("REQ-910"); // layer: requirement, acceptance_labels: []
    let input = TraceInput {
        items: vec![req],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let ctx = empty_ctx();
    let config = crate::storage::config::TraceLintConfig::default();

    let findings = evaluate(&graph, &input, &ctx, &config, None);
    let f = findings
        .iter()
        .find(|f| f.rule == "missing_acceptance")
        .expect("missing_acceptance finding");
    assert_eq!(f.severity, Severity::Info);
    assert_eq!(f.item.as_deref(), Some("REQ-910"));
}

#[test]
fn missing_acceptance_is_silent_once_acceptance_labels_are_present() {
    let req = crate::trace::types::TraceItemInput {
        acceptance_labels: vec!["AC1".to_string()],
        ..item("REQ-911")
    };
    let input = TraceInput {
        items: vec![req],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let ctx = empty_ctx();
    let config = crate::storage::config::TraceLintConfig::default();

    let findings = evaluate(&graph, &input, &ctx, &config, None);
    assert!(
        !findings.iter().any(|f| f.rule == "missing_acceptance"),
        "{findings:?}"
    );
}

#[test]
fn missing_acceptance_does_not_apply_outside_the_requirement_layer() {
    let item_at = crate::trace::types::TraceItemInput {
        layer: Some("acceptance".to_string()),
        ..item("AT-910")
    };
    let input = TraceInput {
        items: vec![item_at],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let ctx = empty_ctx();
    let config = crate::storage::config::TraceLintConfig::default();

    let findings = evaluate(&graph, &input, &ctx, &config, None);
    assert!(
        !findings.iter().any(|f| f.rule == "missing_acceptance"),
        "{findings:?}"
    );
}

#[test]
fn passive_voice_hint_flags_a_japanese_passive_construction() {
    let req = item("REQ-920");
    let input = TraceInput {
        items: vec![req],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let ctx = ctx_with_titles(&[("REQ-920", "データは自動的に削除される")]);
    let config = crate::storage::config::TraceLintConfig::default();

    let findings = evaluate(&graph, &input, &ctx, &config, None);
    let f = findings
        .iter()
        .find(|f| f.rule == "passive_voice_hint")
        .expect("passive_voice_hint finding");
    assert_eq!(f.severity, Severity::Info);
    assert_eq!(f.item.as_deref(), Some("REQ-920"));
}

#[test]
fn passive_voice_hint_flags_an_english_passive_construction() {
    let req = item("REQ-921");
    let input = TraceInput {
        items: vec![req],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let ctx = ctx_with_titles(&[("REQ-921", "The request is processed by the server")]);
    let config = crate::storage::config::TraceLintConfig::default();

    let findings = evaluate(&graph, &input, &ctx, &config, None);
    assert!(
        findings
            .iter()
            .any(|f| f.rule == "passive_voice_hint" && f.item.as_deref() == Some("REQ-921")),
        "{findings:?}"
    );
}

#[test]
fn passive_voice_hint_is_silent_for_an_active_voice_title() {
    let req = item("REQ-922");
    let input = TraceInput {
        items: vec![req],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let ctx = ctx_with_titles(&[("REQ-922", "The server processes the request")]);
    let config = crate::storage::config::TraceLintConfig::default();

    let findings = evaluate(&graph, &input, &ctx, &config, None);
    assert!(
        !findings.iter().any(|f| f.rule == "passive_voice_hint"),
        "{findings:?}"
    );
}

#[test]
fn quality_rules_can_be_turned_off_via_config_overrides() {
    let req = item("REQ-930");
    let input = TraceInput {
        items: vec![req],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let ctx = ctx_with_titles(&[("REQ-930", "適切に処理される")]);
    let mut config = crate::storage::config::TraceLintConfig::default();
    config
        .rules
        .insert("ambiguous_word".to_string(), "off".to_string());
    config
        .rules
        .insert("passive_voice_hint".to_string(), "off".to_string());
    config
        .rules
        .insert("missing_acceptance".to_string(), "off".to_string());

    let findings = evaluate(&graph, &input, &ctx, &config, None);
    assert!(
        !findings.iter().any(
            |f| ["ambiguous_word", "passive_voice_hint", "missing_acceptance"]
                .contains(&f.rule.as_str())
        ),
        "{findings:?}"
    );
}

#[test]
fn quality_rules_are_known_rule_ids() {
    let config = crate::storage::config::TraceLintConfig::default();
    assert!(is_known_rule_id("ambiguous_word", &config));
    assert!(is_known_rule_id("missing_acceptance", &config));
    assert!(is_known_rule_id("passive_voice_hint", &config));
}

// --- Requirement tree (SPEC-TREE-010): the 11 tree lint rules ---

use crate::storage::docs::model::TreeKindDecl;
use crate::trace::types::TreeItemInput;

const TREE_RULES: [&str; 17] = [
    "parent_cross_layer",
    "parent_dangling",
    "tree_cycle",
    "self_parent",
    "empty_group",
    "kind_conflict",
    "invalid_order",
    "dotted_id",
    "leaf_without_acceptance",
    "shallow_leaf",
    "id_parent_mismatch",
    "condition_missing",
    "condition_duplicate",
    "condition_on_non_branch",
    "attr_unknown_parent",
    "decision_arity",
    "empty_branch",
];

fn tree_item(id: &str, parents: &[&str]) -> crate::trace::types::TraceItemInput {
    crate::trace::types::TraceItemInput {
        tree: TreeItemInput {
            parents: parents.iter().map(|p| p.to_string()).collect(),
            ..Default::default()
        },
        ..item(id)
    }
}

fn with_kind(
    mut it: crate::trace::types::TraceItemInput,
    kind: TreeKindDecl,
) -> crate::trace::types::TraceItemInput {
    it.tree.kind_decl = Some(kind);
    it
}

fn with_ac(mut it: crate::trace::types::TraceItemInput) -> crate::trace::types::TraceItemInput {
    it.acceptance_labels = vec!["AC1".to_string()];
    it
}

fn tree_ctx(text: Vec<(&str, ItemTreeText)>) -> LintContext<'static> {
    let map: HashMap<String, ItemTreeText> = text
        .into_iter()
        .map(|(id, t)| (id.to_string(), t))
        .collect();
    let leaked: &'static HashMap<String, ItemTreeText> = Box::leak(Box::new(map));
    LintContext {
        tree_text: leaked,
        ..empty_ctx()
    }
}

fn lint(
    items: Vec<crate::trace::types::TraceItemInput>,
    ctx: &LintContext<'_>,
) -> Vec<LintFinding> {
    let input = TraceInput {
        items,
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let config = crate::storage::config::TraceLintConfig::default();
    evaluate(&graph, &input, ctx, &config, None)
}

fn of_rule<'a>(findings: &'a [LintFinding], rule: &str) -> Vec<&'a LintFinding> {
    findings.iter().filter(|f| f.rule == rule).collect()
}

#[test]
fn every_tree_rule_is_a_known_rule_id_and_multiple_parent_is_not() {
    let config = crate::storage::config::TraceLintConfig::default();
    for rule in TREE_RULES {
        assert!(is_known_rule_id(rule, &config), "{rule} must be known");
    }
    assert!(
        !is_known_rule_id("multiple_parent", &config),
        "a DAG allows several parents, so there is no multiple_parent rule"
    );
}

#[test]
fn tree_gap_rules_are_reported_with_their_default_severities() {
    let mut cross = tree_item("SPEC-001", &["REQ-001"]);
    cross.layer = Some("basic_spec".to_string());
    let items = vec![
        item("REQ-001"),
        cross,
        tree_item("REQ-002", &["REQ-404"]),
        tree_item("REQ-003", &["REQ-003"]),
        tree_item("REQ-004", &["REQ-005"]),
        tree_item("REQ-005", &["REQ-004"]),
        with_kind(item("REQ-006"), TreeKindDecl::Group),
        with_kind(tree_item("REQ-007", &[]), TreeKindDecl::Leaf),
        tree_item("REQ-008", &["REQ-007"]),
    ];

    let findings = lint(items, &empty_ctx());

    let expect = [
        ("parent_cross_layer", Severity::Error, vec!["SPEC-001"]),
        ("parent_dangling", Severity::Error, vec!["REQ-002"]),
        ("self_parent", Severity::Error, vec!["REQ-003"]),
        ("tree_cycle", Severity::Error, vec!["REQ-004", "REQ-005"]),
        ("empty_group", Severity::Warning, vec!["REQ-006"]),
        ("kind_conflict", Severity::Warning, vec!["REQ-007"]),
    ];
    for (rule, severity, items) in expect {
        let got = of_rule(&findings, rule);
        let got_items: Vec<&str> = got.iter().filter_map(|f| f.item.as_deref()).collect();
        assert_eq!(got_items, items, "{rule}: {findings:?}");
        assert!(got.iter().all(|f| f.severity == severity), "{rule}");
        assert!(got.iter().all(|f| !f.message.is_empty()), "{rule}");
    }
}

#[test]
fn self_parent_does_not_also_report_tree_cycle() {
    let findings = lint(vec![tree_item("REQ-001", &["REQ-001"])], &empty_ctx());
    assert_eq!(of_rule(&findings, "self_parent").len(), 1);
    assert!(of_rule(&findings, "tree_cycle").is_empty(), "{findings:?}");
}

#[test]
fn tree_gap_findings_carry_the_owning_document_slug() {
    let mut item_meta = HashMap::new();
    item_meta.insert(
        "REQ-002".to_string(),
        ItemLintMeta {
            doc_slug: "req-doc".to_string(),
            priority: None,
            approval: "draft".to_string(),
            title: String::new(),
        },
    );
    let leaked: &'static HashMap<String, ItemLintMeta> = Box::leak(Box::new(item_meta));
    let ctx = LintContext {
        item_meta: leaked,
        ..empty_ctx()
    };
    let findings = lint(vec![tree_item("REQ-002", &["REQ-404"])], &ctx);
    let f = of_rule(&findings, "parent_dangling");
    assert_eq!(f[0].doc.as_deref(), Some("req-doc"));
}

#[test]
fn a_valid_tree_produces_no_tree_rule_findings() {
    let items = vec![
        with_ac(item("REQ-001")),
        with_ac(tree_item("REQ-001-001", &["REQ-001"])),
        with_ac(tree_item("REQ-001-002", &["REQ-001"])),
    ];
    let findings = lint(items, &empty_ctx());
    for rule in TREE_RULES {
        assert!(
            of_rule(&findings, rule).is_empty(),
            "{rule} must not fire: {findings:?}"
        );
    }
}

#[test]
fn invalid_order_and_dotted_id_come_from_the_item_tree_text() {
    let ctx = tree_ctx(vec![
        (
            "REQ-001",
            ItemTreeText {
                invalid_order: vec!["line 3: item \"REQ-001\" has an invalid order".to_string()],
                ..Default::default()
            },
        ),
        (
            "REQ-002",
            ItemTreeText {
                dotted_heading: true,
                ..Default::default()
            },
        ),
        ("REQ-003", ItemTreeText::default()),
    ]);
    let items = vec![
        with_ac(item("REQ-001")),
        with_ac(item("REQ-002")),
        with_ac(item("REQ-003")),
    ];

    let findings = lint(items, &ctx);

    let order = of_rule(&findings, "invalid_order");
    assert_eq!(order.len(), 1);
    assert_eq!(order[0].item.as_deref(), Some("REQ-001"));
    assert_eq!(order[0].severity, Severity::Warning);
    assert!(order[0].message.contains("invalid order"));
    let dotted = of_rule(&findings, "dotted_id");
    assert_eq!(dotted.len(), 1);
    assert_eq!(dotted[0].item.as_deref(), Some("REQ-002"));
    assert_eq!(dotted[0].severity, Severity::Warning);
}

#[test]
fn leaf_without_acceptance_flags_only_a_bare_left_side_leaf() {
    let mut right = item("ST-001");
    right.layer = Some("system_test".to_string());
    let mut inline = item("REQ-005");
    inline.method = Some("auto".to_string());
    let mut verified = item("REQ-006");
    verified.refines = Vec::new();
    let mut verifier = item("AT-001");
    verifier.layer = Some("acceptance".to_string());
    verifier.verifies = vec!["REQ-006".to_string()];
    let items = vec![
        item("REQ-001"),                                    // bare leaf -> flagged
        with_ac(item("REQ-002")),                           // has acceptance
        item("REQ-003"),                                    // composite (has child)
        tree_item("REQ-003-001", &["REQ-003"]),             // bare leaf -> flagged
        with_kind(item("REQ-004"), TreeKindDecl::Decision), // decision, not a leaf
        inline,                                             // inline verification
        verified,                                           // verified by AT-001
        verifier,                                           // right side layer
        right,                                              // right side layer
    ];

    let findings = lint(items, &empty_ctx());

    let flagged: Vec<&str> = of_rule(&findings, "leaf_without_acceptance")
        .iter()
        .filter_map(|f| f.item.as_deref())
        .collect();
    assert_eq!(flagged, vec!["REQ-001", "REQ-003-001"], "{findings:?}");
    assert!(of_rule(&findings, "leaf_without_acceptance")
        .iter()
        .all(|f| f.severity == Severity::Warning));
}

#[test]
fn leaf_without_acceptance_exempts_a_leaf_that_waives_verification() {
    let mut waived = item("REQ-001");
    waived.waived_axes = vec![crate::trace::types::WaiverAxis::Verify];
    let mut refine_waived = item("REQ-002");
    refine_waived.waived_axes = vec![crate::trace::types::WaiverAxis::Refine];

    let findings = lint(vec![waived, refine_waived], &empty_ctx());

    let flagged: Vec<&str> = of_rule(&findings, "leaf_without_acceptance")
        .iter()
        .filter_map(|f| f.item.as_deref())
        .collect();
    // waive-verify exempts the leaf; waive-refine says nothing about verification.
    assert_eq!(flagged, vec!["REQ-002"], "{findings:?}");
}

#[test]
fn shallow_leaf_flags_a_leaf_whose_body_joins_behaviours() {
    let text = |s: &str| ItemTreeText {
        statement: s.to_string(),
        ..Default::default()
    };
    let ctx = tree_ctx(vec![
        ("REQ-001", text("ログインし、かつ履歴を保存する")),
        ("REQ-002", text("The system shall log in and save history")),
        ("REQ-003", text("The system shall follow the standard")),
        ("REQ-004", text("ログインと履歴を保存する")),
        ("REQ-005", text("ログインし、かつ履歴を保存する")),
        ("REQ-006", text("A as well as B")),
    ]);
    let items = vec![
        with_ac(item("REQ-001")),
        with_ac(item("REQ-002")),
        with_ac(item("REQ-003")),
        with_ac(item("REQ-004")),
        with_ac(item("REQ-005")),
        with_ac(item("REQ-006")),
        with_ac(tree_item("REQ-005-001", &["REQ-005"])),
    ];

    let findings = lint(items, &ctx);

    let flagged: Vec<&str> = of_rule(&findings, "shallow_leaf")
        .iter()
        .filter_map(|f| f.item.as_deref())
        .collect();
    // REQ-003: "standard" contains "and" but not as a word; REQ-005 is a
    // composite (it has a child), so it is not a leaf.
    assert_eq!(
        flagged,
        vec!["REQ-001", "REQ-002", "REQ-006"],
        "{findings:?}"
    );
    assert!(of_rule(&findings, "shallow_leaf")
        .iter()
        .all(|f| f.severity == Severity::Info));
}

#[test]
fn id_parent_mismatch_flags_a_child_shaped_id_without_its_prefix_parent() {
    let mut other_layer = item("SPEC-001");
    other_layer.layer = Some("basic_spec".to_string());
    let mut cross_named = item("SPEC-001-001");
    cross_named.layer = Some("basic_spec".to_string());
    let items = vec![
        with_ac(item("REQ-001")),
        with_ac(item("REQ-001-001")), // X exists, no parent -> flagged
        with_ac(tree_item("REQ-001-002", &["REQ-001"])), // has the parent
        with_ac(tree_item("REQ-001-003", &["REQ-001", "REQ-009"])), // parent among several
        with_ac(item("REQ-010-001")), // X = REQ-010 does not exist
        with_ac(item("REQ-002")),     // not shaped <X>-<n>
        other_layer,
        cross_named,                   // SPEC-001 exists, same layer -> flagged
        with_ac(item("REQ-001-004a")), // suffix is not all digits
    ];

    let findings = lint(items, &empty_ctx());

    let flagged: Vec<&str> = of_rule(&findings, "id_parent_mismatch")
        .iter()
        .filter_map(|f| f.item.as_deref())
        .collect();
    assert_eq!(flagged, vec!["REQ-001-001", "SPEC-001-001"], "{findings:?}");
    assert!(of_rule(&findings, "id_parent_mismatch")
        .iter()
        .all(|f| f.severity == Severity::Info));
}

#[test]
fn id_parent_mismatch_ignores_a_child_shaped_id_whose_base_is_on_another_layer() {
    let mut other_layer = item("REQ-001-001");
    other_layer.layer = Some("basic_spec".to_string());
    let items = vec![with_ac(item("REQ-001")), with_ac(other_layer)];

    let findings = lint(items, &empty_ctx());

    // A parent must be on the same layer (parent_cross_layer), so a base that
    // lives on a different layer can never be the intended parent.
    assert!(
        of_rule(&findings, "id_parent_mismatch").is_empty(),
        "{findings:?}"
    );
}

#[test]
fn tree_rules_honor_severity_overrides_off_and_the_rules_filter() {
    let items = vec![tree_item("REQ-002", &["REQ-404"]), item("REQ-001")];
    let input = TraceInput {
        items,
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let ctx = empty_ctx();

    let mut config = crate::storage::config::TraceLintConfig::default();
    config
        .rules
        .insert("parent_dangling".to_string(), "info".to_string());
    config
        .rules
        .insert("leaf_without_acceptance".to_string(), "off".to_string());
    let findings = evaluate(&graph, &input, &ctx, &config, None);
    assert_eq!(
        of_rule(&findings, "parent_dangling")[0].severity,
        Severity::Info
    );
    assert!(of_rule(&findings, "leaf_without_acceptance").is_empty());

    let filter: HashSet<String> = ["leaf_without_acceptance".to_string()].into();
    let config = crate::storage::config::TraceLintConfig::default();
    let findings = evaluate(&graph, &input, &ctx, &config, Some(&filter));
    assert!(of_rule(&findings, "parent_dangling").is_empty());
    assert_eq!(of_rule(&findings, "leaf_without_acceptance").len(), 2);
}

#[test]
fn every_tree_gap_kind_is_a_registered_lint_rule() {
    // A tree gap that reaches `gaps[]` but is missing from the registry would
    // be dropped from lint output, and `rules: ["<kind>"]` / a
    // `[trace.lint.rules]` override naming it would be rejected as unknown.
    for kind in crate::trace::tree::TreeGapKind::ALL {
        let rule = crate::trace::tree_view::tree_gap_kind_str(kind);
        let severity = BUILTIN_RULES
            .iter()
            .find(|(r, _)| *r == rule)
            .map(|(_, s)| *s)
            .unwrap_or_else(|| panic!("tree gap '{rule}' is not in BUILTIN_RULES"));
        assert!(
            is_known_rule_id(rule, &TraceLintConfig::default()),
            "{rule} must be accepted as a rule id"
        );
        let expected = match kind {
            crate::trace::tree::TreeGapKind::ParentDangling
            | crate::trace::tree::TreeGapKind::ParentCrossLayer
            | crate::trace::tree::TreeGapKind::TreeCycle
            | crate::trace::tree::TreeGapKind::SelfParent => Severity::Error,
            _ => Severity::Warning,
        };
        assert_eq!(severity, expected, "default severity of {rule}");
    }
}

/// t-vfix.9: a composite's `waive-verify` and `redundant_waiver` agree: the
/// waiver is honoured while no child is covered (no `unverified`, no
/// `redundant_waiver`), and reported as unneeded once the children cover it.
#[test]
fn composite_waive_verify_is_redundant_only_once_children_cover_it() {
    use crate::trace::types::WaiverAxis;
    let build = |verified_child: bool| {
        let mut parent = item("REQ-P");
        parent.waived_axes = vec![WaiverAxis::Verify];
        let mut child = item("REQ-1");
        child.tree.parents = vec!["REQ-P".to_string()];
        let mut items = vec![parent, child];
        if verified_child {
            let mut at = item("AT-1");
            at.layer = Some("acceptance".to_string());
            at.verifies = vec!["REQ-1".to_string()];
            items.push(at);
        }
        TraceInput {
            items,
            configured_layers: vec!["requirement".into(), "acceptance".into()],
            ..Default::default()
        }
    };
    let run = |input: &TraceInput| {
        let graph = TraceGraph::build(input);
        let config = crate::storage::config::TraceLintConfig::default();
        evaluate(&graph, input, &empty_ctx(), &config, None)
    };

    let uncovered = run(&build(false));
    assert!(
        uncovered.iter().all(|f| f.rule != "redundant_waiver"),
        "{uncovered:?}"
    );
    assert!(
        !uncovered
            .iter()
            .any(|f| f.rule == "unverified" && f.item.as_deref() == Some("REQ-P")),
        "{uncovered:?}"
    );

    let covered = run(&build(true));
    let redundant: Vec<_> = covered
        .iter()
        .filter(|f| f.rule == "redundant_waiver")
        .collect();
    assert_eq!(redundant.len(), 1, "{covered:?}");
    assert!(
        redundant[0].message.contains("children"),
        "{}",
        redundant[0].message
    );
    assert!(
        !covered
            .iter()
            .any(|f| f.rule == "unverified" && f.item.as_deref() == Some("REQ-P")),
        "{covered:?}"
    );
}
