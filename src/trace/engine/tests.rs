//! Unit tests for [`super::TraceGraph`] (wiki/220-vmodel-integration-design.md
//! §2.7/§7, t360.9): link legitimacy (verifies level condition, refines
//! upward direction), dangling/invalid_link/cycle, state priority + skipped
//! exclusion + inline verification + deep coverage, horizontal/vertical
//! coverage incl. n/a via layer-skip, and every one of the 8 gap kinds.

use std::collections::{HashMap, HashSet};

use super::*;
use crate::trace::types::{
    Gap, GapKind, ItemState, LayersSource, TaskDocLink, TaskLinkRole, TaskRequirementLink,
    TraceInput, TraceItemInput,
};

fn item(id: &str, doc: &str, layer: &str, refines: &[&str], verifies: &[&str]) -> TraceItemInput {
    TraceItemInput {
        stable_id: id.to_string(),
        doc_id: doc.to_string(),
        layer: Some(layer.to_string()),
        refines: refines.iter().map(|s| s.to_string()).collect(),
        verifies: verifies.iter().map(|s| s.to_string()).collect(),
        method: None,
        has_test_refs: false,
    }
}

fn inline_item(id: &str, doc: &str, layer: &str, refines: &[&str]) -> TraceItemInput {
    TraceItemInput {
        method: Some("manual".to_string()),
        ..item(id, doc, layer, refines, &[])
    }
}

fn runs(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn implements_link(task_id: &str, stable_id: &str) -> TaskRequirementLink {
    TaskRequirementLink {
        task_id: task_id.to_string(),
        stable_id: stable_id.to_string(),
        role: TaskLinkRole::Implements,
    }
}

fn find_gap<'a>(gaps: &'a [Gap], kind: GapKind, id: &str) -> Option<&'a Gap> {
    gaps.iter()
        .find(|g| g.kind == kind && g.item.as_deref() == Some(id))
}

fn set(ids: &[&str]) -> HashSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

// ---------------------------------------------------------------------
// Link legitimacy
// ---------------------------------------------------------------------

#[test]
fn verifies_link_valid_when_verifier_level_at_or_below_target_level() {
    let input = TraceInput {
        items: vec![
            item("REQ-1", "d1", "requirement", &[], &[]),
            item("AT-1", "d1", "acceptance", &[], &["REQ-1"]),
            item("ST-1", "d1", "system_test", &[], &["REQ-1"]),
        ],
        configured_layers: vec![
            "requirement".into(),
            "acceptance".into(),
            "system_test".into(),
        ],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    // A deeper right layer (system_test, level 2) may verify a shallower
    // left item (requirement, level 1) — "検証項目の level ≥ 対象の level".
    assert_eq!(
        graph.verified_by("REQ-1"),
        &["AT-1".to_string(), "ST-1".to_string()]
    );
    assert!(graph.gaps().iter().all(|g| g.kind != GapKind::InvalidLink));
}

#[test]
fn verifies_link_invalid_when_verifier_level_below_target_level() {
    let input = TraceInput {
        items: vec![
            item("BS-1", "d1", "basic_spec", &[], &[]),
            // acceptance is level 1, basic_spec is level 2: verifier is not
            // deep enough to legitimately verify a level-2 target.
            item("AT-1", "d1", "acceptance", &[], &["BS-1"]),
        ],
        configured_layers: vec!["basic_spec".into(), "acceptance".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert!(graph.verified_by("BS-1").is_empty());
    assert!(find_gap(graph.gaps(), GapKind::InvalidLink, "AT-1").is_some());
}

#[test]
fn refines_link_valid_only_left_to_strictly_upper_left_item() {
    let input = TraceInput {
        items: vec![
            item("REQ-1", "d1", "requirement", &[], &[]),
            item("BS-1", "d1", "basic_spec", &["REQ-1"], &[]),
            item("DS-1", "d1", "detailed_spec", &["BS-1"], &[]),
        ],
        configured_layers: vec![
            "requirement".into(),
            "basic_spec".into(),
            "detailed_spec".into(),
        ],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert_eq!(graph.refines_children("REQ-1"), &["BS-1".to_string()]);
    assert_eq!(graph.refines_children("BS-1"), &["DS-1".to_string()]);
    assert!(graph.gaps().iter().all(|g| g.kind != GapKind::InvalidLink));
}

#[test]
fn refines_link_invalid_when_wrong_direction_or_wrong_side() {
    let input = TraceInput {
        items: vec![
            item("REQ-1", "d1", "requirement", &[], &[]),
            item("DS-1", "d1", "detailed_spec", &[], &[]),
            // basic_spec (level 2) "refines" detailed_spec (level 3): wrong
            // direction (target must be *upper*, i.e. lower level).
            item("BS-1", "d1", "basic_spec", &["DS-1"], &[]),
            // acceptance is right-side: refines is left-only.
            item("AT-1", "d1", "acceptance", &["REQ-1"], &[]),
        ],
        configured_layers: vec![
            "requirement".into(),
            "basic_spec".into(),
            "detailed_spec".into(),
            "acceptance".into(),
        ],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert!(graph.refines_children("DS-1").is_empty());
    assert!(graph.refines_children("REQ-1").is_empty());
    assert!(find_gap(graph.gaps(), GapKind::InvalidLink, "BS-1").is_some());
    assert!(find_gap(graph.gaps(), GapKind::InvalidLink, "AT-1").is_some());
}

#[test]
fn unknown_layer_target_is_not_dangling_but_makes_the_link_invalid() {
    let mut custom = item("XX-1", "d1", "totally_custom_layer", &[], &[]);
    custom.layer = Some("totally_custom_layer".to_string());
    let input = TraceInput {
        items: vec![
            item("REQ-1", "d1", "requirement", &[], &[]),
            item("BS-1", "d1", "basic_spec", &["XX-1"], &[]),
            custom,
        ],
        configured_layers: vec!["requirement".into(), "basic_spec".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert!(find_gap(graph.gaps(), GapKind::Dangling, "BS-1").is_none());
    assert!(find_gap(graph.gaps(), GapKind::InvalidLink, "BS-1").is_some());
}

#[test]
fn unknown_id_reference_is_dangling_for_both_refines_and_verifies() {
    let input = TraceInput {
        items: vec![
            item("BS-1", "d1", "basic_spec", &["REQ-MISSING"], &[]),
            item("AT-1", "d1", "acceptance", &[], &["REQ-ALSO-MISSING"]),
        ],
        configured_layers: vec!["basic_spec".into(), "acceptance".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert!(find_gap(graph.gaps(), GapKind::Dangling, "BS-1").is_some());
    assert!(find_gap(graph.gaps(), GapKind::Dangling, "AT-1").is_some());
}

// ---------------------------------------------------------------------
// State priority, skipped exclusion, inline verification, deep coverage
// ---------------------------------------------------------------------

#[test]
fn state_priority_failing_beats_blocked_beats_not_run_beats_uncovered_beats_passing() {
    assert!(ItemState::Failing > ItemState::Blocked);
    assert!(ItemState::Blocked > ItemState::NotRun);
    assert!(ItemState::NotRun > ItemState::Uncovered);
    assert!(ItemState::Uncovered > ItemState::Passing);
}

#[test]
fn verifier_own_state_maps_run_results_and_treats_skipped_as_not_run_when_no_other_element() {
    let make = |result: &str| TraceInput {
        items: vec![
            item("REQ-X", "d1", "requirement", &[], &[]),
            item("AT-X", "d1", "acceptance", &[], &["REQ-X"]),
        ],
        runs_latest: runs(&[("AT-X", result)]),
        configured_layers: vec!["requirement".into(), "acceptance".into()],
        ..Default::default()
    };
    let g = TraceGraph::build(&make("pass"));
    assert_eq!(g.state("AT-X"), Some(ItemState::Passing));
    let g = TraceGraph::build(&make("fail"));
    assert_eq!(g.state("AT-X"), Some(ItemState::Failing));
    let g = TraceGraph::build(&make("blocked"));
    assert_eq!(g.state("AT-X"), Some(ItemState::Blocked));
    let g = TraceGraph::build(&make("not_run"));
    assert_eq!(g.state("AT-X"), Some(ItemState::NotRun));
    // skipped, with no other element to fall back on -> not_run, not
    // "excluded into nothing" (wiki/220 §2.7: "他に要素がなければ not_run").
    let g = TraceGraph::build(&make("skipped"));
    assert_eq!(g.state("AT-X"), Some(ItemState::NotRun));
}

#[test]
fn left_item_state_ignores_a_skipped_verifier_when_another_verifier_passes() {
    // REQ-1 verified by AT-1=pass and AT-2=skipped, both coverage axes
    // already covered — the skipped result must be excluded from the max()
    // aggregation entirely (wiki/220 §2.7: "skipped の結果は集約から除外
    // する"), leaving only AT-1's `pass` as REQ-1's state.
    let input = TraceInput {
        items: vec![
            item("REQ-1", "d1", "requirement", &[], &[]),
            item("AT-1", "d1", "acceptance", &[], &["REQ-1"]),
            item("AT-2", "d1", "acceptance", &[], &["REQ-1"]),
        ],
        runs_latest: runs(&[("AT-1", "pass"), ("AT-2", "skipped")]),
        task_requirement_links: vec![implements_link("t1", "REQ-1")],
        configured_layers: vec!["requirement".into(), "acceptance".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert_eq!(graph.coverage()["requirement"].horizontal.covered, 1);
    assert_eq!(graph.coverage()["requirement"].vertical.covered, 1);
    assert_eq!(graph.state("REQ-1"), Some(ItemState::Passing));
}

#[test]
fn left_item_state_is_not_run_when_its_only_verifier_result_is_skipped() {
    // REQ-1's only verifier is skipped: horizontal is still `covered` (the
    // link exists), but with no other element to aggregate the item falls
    // back to `not_run`, not `uncovered` (wiki/220 §2.7: "他に要素がなければ
    // not_run").
    let input = TraceInput {
        items: vec![
            item("REQ-1", "d1", "requirement", &[], &[]),
            item("AT-1", "d1", "acceptance", &[], &["REQ-1"]),
        ],
        runs_latest: runs(&[("AT-1", "skipped")]),
        task_requirement_links: vec![implements_link("t1", "REQ-1")],
        configured_layers: vec!["requirement".into(), "acceptance".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert_eq!(graph.coverage()["requirement"].horizontal.covered, 1);
    assert_eq!(graph.state("REQ-1"), Some(ItemState::NotRun));
}

#[test]
fn left_item_state_is_not_run_when_inline_verification_result_is_skipped() {
    // An inline-verified left item whose only run is skipped, with an
    // implements task closing vertical coverage: horizontal is `covered`
    // through the inline route, but the state falls back to `not_run`
    // (wiki/220 §2.7), not `uncovered` (which would contradict a `covered`
    // horizontal axis).
    let input = TraceInput {
        items: vec![inline_item("SPEC-1", "d1", "basic_spec", &[])],
        runs_latest: runs(&[("SPEC-1", "skipped")]),
        task_requirement_links: vec![implements_link("t1", "SPEC-1")],
        configured_layers: vec!["basic_spec".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert_eq!(graph.coverage()["basic_spec"].horizontal.covered, 1);
    assert_eq!(graph.state("SPEC-1"), Some(ItemState::NotRun));
}

#[test]
fn inline_verification_lets_a_left_item_verify_itself_without_a_right_side_document() {
    let input = TraceInput {
        items: vec![inline_item("SPEC-1", "d1", "basic_spec", &[])],
        runs_latest: runs(&[("SPEC-1", "pass")]),
        task_requirement_links: vec![implements_link("t1", "SPEC-1")],
        // system_test (basic_spec's pair) intentionally not in use: only
        // inline verification closes horizontal coverage here.
        configured_layers: vec!["basic_spec".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert_eq!(graph.state("SPEC-1"), Some(ItemState::Passing));
    assert!(graph
        .gaps()
        .iter()
        .all(|g| g.item.as_deref() != Some("SPEC-1")));
}

#[test]
fn deep_coverage_requires_every_refining_descendant_to_also_pass() {
    let base = |st_result: &str| TraceInput {
        items: vec![
            item("REQ-1", "d1", "requirement", &[], &[]),
            item("BS-1", "d1", "basic_spec", &["REQ-1"], &[]),
            item("AT-1", "d1", "acceptance", &[], &["REQ-1"]),
            item("ST-1", "d1", "system_test", &[], &["BS-1"]),
        ],
        runs_latest: runs(&[("AT-1", "pass"), ("ST-1", st_result)]),
        task_requirement_links: vec![implements_link("t1", "BS-1")],
        configured_layers: vec![
            "requirement".into(),
            "basic_spec".into(),
            "acceptance".into(),
            "system_test".into(),
        ],
        ..Default::default()
    };
    let passing = TraceGraph::build(&base("pass"));
    assert_eq!(passing.state("BS-1"), Some(ItemState::Passing));
    assert_eq!(
        passing.state("REQ-1"),
        Some(ItemState::Passing),
        "REQ-1's own verifier passed and its only refining child also passed"
    );

    let failing = TraceGraph::build(&base("fail"));
    assert_eq!(failing.state("BS-1"), Some(ItemState::Failing));
    assert_eq!(
        failing.state("REQ-1"),
        Some(ItemState::Failing),
        "a failing descendant must fail the ancestor even though REQ-1's own verifier passed"
    );
}

// ---------------------------------------------------------------------
// Coverage: horizontal / vertical / n/a via layer-skip
// ---------------------------------------------------------------------

#[test]
fn horizontal_is_na_when_pair_layer_unused_and_uncovered_when_pair_layer_used() {
    // pair (unit_test) not in use, no verifier, not inline -> n/a, no gap.
    let na_input = TraceInput {
        items: vec![item("DS-1", "d1", "detailed_spec", &[], &[])],
        configured_layers: vec!["detailed_spec".into()],
        ..Default::default()
    };
    let na_graph = TraceGraph::build(&na_input);
    let cov = &na_graph.coverage()["detailed_spec"];
    assert_eq!(cov.horizontal.na, 1);
    assert_eq!(cov.horizontal.uncovered, 0);
    assert!(find_gap(na_graph.gaps(), GapKind::Unverified, "DS-1").is_none());

    // pair (unit_test) in use, still no verifier -> uncovered, gap fires.
    let uncovered_input = TraceInput {
        items: vec![item("DS-1", "d1", "detailed_spec", &[], &[])],
        configured_layers: vec!["detailed_spec".into(), "unit_test".into()],
        ..Default::default()
    };
    let uncovered_graph = TraceGraph::build(&uncovered_input);
    let cov = &uncovered_graph.coverage()["detailed_spec"];
    assert_eq!(cov.horizontal.uncovered, 1);
    assert!(find_gap(uncovered_graph.gaps(), GapKind::Unverified, "DS-1").is_some());
}

#[test]
fn vertical_covered_by_implements_task_at_the_deepest_used_left_layer() {
    let without_task = TraceInput {
        items: vec![item("DS-1", "d1", "detailed_spec", &[], &[])],
        configured_layers: vec!["detailed_spec".into()],
        ..Default::default()
    };
    let g = TraceGraph::build(&without_task);
    assert_eq!(g.coverage()["detailed_spec"].vertical.uncovered, 1);
    assert!(find_gap(g.gaps(), GapKind::Unrefined, "DS-1").is_some());

    let with_task = TraceInput {
        items: vec![item("DS-1", "d1", "detailed_spec", &[], &[])],
        task_requirement_links: vec![implements_link("t1", "DS-1")],
        configured_layers: vec!["detailed_spec".into()],
        ..Default::default()
    };
    let g = TraceGraph::build(&with_task);
    assert_eq!(g.coverage()["detailed_spec"].vertical.covered, 1);
    assert!(find_gap(g.gaps(), GapKind::Unrefined, "DS-1").is_none());
}

#[test]
fn right_side_layers_report_na_for_both_coverage_axes() {
    let input = TraceInput {
        items: vec![
            item("REQ-1", "d1", "requirement", &[], &[]),
            item("AT-1", "d1", "acceptance", &[], &["REQ-1"]),
        ],
        configured_layers: vec!["requirement".into(), "acceptance".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let cov = &graph.coverage()["acceptance"];
    assert_eq!(cov.total, 1);
    assert_eq!(cov.horizontal.na, 1);
    assert_eq!(cov.vertical.na, 1);
}

// ---------------------------------------------------------------------
// The 8 gap kinds
// ---------------------------------------------------------------------

#[test]
fn gap_orphan_for_left_item_missing_refines_when_upper_layer_in_use() {
    let input = TraceInput {
        items: vec![
            item("REQ-1", "d1", "requirement", &[], &[]),
            item("BS-1", "d1", "basic_spec", &[], &[]),
        ],
        configured_layers: vec!["requirement".into(), "basic_spec".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert!(find_gap(graph.gaps(), GapKind::Orphan, "BS-1").is_some());
    // requirement (level 1) has no upper layer of its own -> never orphan.
    assert!(find_gap(graph.gaps(), GapKind::Orphan, "REQ-1").is_none());
}

#[test]
fn gap_orphan_for_right_item_missing_verifies() {
    let input = TraceInput {
        items: vec![item("AT-1", "d1", "acceptance", &[], &[])],
        configured_layers: vec!["acceptance".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert!(find_gap(graph.gaps(), GapKind::Orphan, "AT-1").is_some());
}

#[test]
fn gap_duplicate_id_from_project_wide_stable_id_owners() {
    let mut owners = HashMap::new();
    owners.insert(
        "REQ-1".to_string(),
        vec!["doc-a".to_string(), "doc-b".to_string()],
    );
    owners.insert("REQ-2".to_string(), vec!["doc-a".to_string()]);
    let input = TraceInput {
        items: vec![item("REQ-1", "doc-a", "requirement", &[], &[])],
        stable_id_owners: owners,
        configured_layers: vec!["requirement".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let gap = find_gap(graph.gaps(), GapKind::DuplicateId, "REQ-1")
        .expect("REQ-1 owned by 2 docs must be a duplicate_id gap");
    assert!(gap.detail.contains("doc-a"));
    assert!(gap.detail.contains("doc-b"));
    assert!(find_gap(graph.gaps(), GapKind::DuplicateId, "REQ-2").is_none());
}

#[test]
fn gap_task_unlinked_when_task_doc_links_a_layer_document_without_any_item_link() {
    let input = TraceInput {
        items: vec![item("REQ-1", "layer-doc", "requirement", &[], &[])],
        task_doc_links: vec![TaskDocLink {
            task_id: "t1".to_string(),
            doc_id: "layer-doc".to_string(),
        }],
        layer_doc_ids: HashSet::from(["layer-doc".to_string()]),
        configured_layers: vec!["requirement".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert!(find_gap(graph.gaps(), GapKind::TaskUnlinked, "t1").is_some());
}

#[test]
fn gap_task_unlinked_absent_when_task_has_an_item_level_link_into_the_same_doc() {
    let input = TraceInput {
        items: vec![item("REQ-1", "layer-doc", "requirement", &[], &[])],
        task_requirement_links: vec![implements_link("t1", "REQ-1")],
        task_doc_links: vec![TaskDocLink {
            task_id: "t1".to_string(),
            doc_id: "layer-doc".to_string(),
        }],
        layer_doc_ids: HashSet::from(["layer-doc".to_string()]),
        configured_layers: vec!["requirement".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert!(find_gap(graph.gaps(), GapKind::TaskUnlinked, "t1").is_none());
}

#[test]
fn gap_task_unlinked_absent_when_the_doc_link_target_is_not_a_layer_document() {
    let input = TraceInput {
        items: vec![item("REQ-1", "plain-doc", "requirement", &[], &[])],
        task_doc_links: vec![TaskDocLink {
            task_id: "t1".to_string(),
            doc_id: "plain-doc".to_string(),
        }],
        // plain-doc is deliberately absent from layer_doc_ids.
        configured_layers: vec!["requirement".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert!(find_gap(graph.gaps(), GapKind::TaskUnlinked, "t1").is_none());
}

#[test]
fn gap_cycle_truncates_recursion_via_the_in_progress_set() {
    // A true `refines` cycle cannot arise from M1's 3 fixed left-side levels
    // (an edge only exists when the source's level is strictly greater than
    // the target's, so any walk through valid edges has strictly increasing
    // levels and can never revisit a node) — so this test exercises the DP
    // helper directly with a hand-built 2-node mutual `refines_children`
    // map, the same defensive shape a future M2 (arbitrary layer depth,
    // wiki/220 §1.1) could produce. This is the safety net the spec asks
    // for regardless of whether M1's fixed depth can trigger it today.
    let mut items = HashMap::new();
    items.insert(
        "A".to_string(),
        ResolvedItem {
            doc_id: "d1".to_string(),
            layer: Some("requirement".to_string()),
            side: Some(LayerSide::Left),
            level: Some(1),
            refines: vec!["B".to_string()],
            verifies: vec![],
            is_inline: false,
        },
    );
    items.insert(
        "B".to_string(),
        ResolvedItem {
            doc_id: "d1".to_string(),
            layer: Some("requirement".to_string()),
            side: Some(LayerSide::Left),
            level: Some(1),
            refines: vec!["A".to_string()],
            verifies: vec![],
            is_inline: false,
        },
    );
    let mut refines_children = HashMap::new();
    refines_children.insert("A".to_string(), vec!["B".to_string()]);
    refines_children.insert("B".to_string(), vec!["A".to_string()]);
    let verified_by = HashMap::new();
    let runs_latest = HashMap::new();
    let coverage_status = HashMap::new();
    let in_use = set(&["requirement"]);
    let mut memo = HashMap::new();
    let mut gaps = Vec::new();
    {
        let mut dp = Dp {
            items: &items,
            refines_children: &refines_children,
            verified_by: &verified_by,
            runs_latest: &runs_latest,
            coverage_status: &coverage_status,
            in_use: &in_use,
            memo: &mut memo,
            in_progress: HashSet::new(),
            reported_cycles: HashSet::new(),
            gaps: &mut gaps,
            memo_misses: 0,
        };
        // Must terminate (no stack overflow / infinite loop) and memoize
        // both ends of the cycle exactly once.
        let state_a = dp.resolve("A");
        assert_eq!(state_a, ItemState::Uncovered);
    }
    assert_eq!(
        memo.len(),
        2,
        "both cycle participants must still be memoized"
    );
    assert_eq!(
        gaps.iter().filter(|g| g.kind == GapKind::Cycle).count(),
        1,
        "exactly one back-edge is detected for a 2-node cycle"
    );
}

// ---------------------------------------------------------------------
// In-use layer resolution (config override vs. auto-detection)
// ---------------------------------------------------------------------

#[test]
fn in_use_layers_auto_detected_from_item_presence_when_not_configured() {
    let input = TraceInput {
        items: vec![
            item("REQ-1", "d1", "requirement", &[], &[]),
            item("AT-1", "d1", "acceptance", &[], &["REQ-1"]),
        ],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert_eq!(graph.in_use_layers().source, LayersSource::Auto);
    assert_eq!(
        graph.in_use_layers().layers,
        vec!["requirement".to_string(), "acceptance".to_string()]
    );
    assert!(!graph.is_layer_in_use("basic_spec"));
}

#[test]
fn in_use_layers_explicit_config_overrides_auto_detection() {
    let input = TraceInput {
        items: vec![item("REQ-1", "d1", "requirement", &[], &[])],
        // Explicitly excludes "requirement" even though an item exists
        // there, and includes "basic_spec" even though nothing uses it yet.
        configured_layers: vec!["basic_spec".to_string()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert_eq!(graph.in_use_layers().source, LayersSource::Config);
    assert_eq!(graph.in_use_layers().layers, vec!["basic_spec".to_string()]);
    assert!(!graph.is_layer_in_use("requirement"));
    // requirement is no longer in use, so its coverage/gaps are skipped
    // entirely (wiki/220 §2.1: "使用中でない層は...ギャップとして数えない").
    assert!(graph.coverage().get("requirement").is_none());
    assert!(graph
        .gaps()
        .iter()
        .all(|g| g.item.as_deref() != Some("REQ-1")));
}

#[test]
fn out_of_use_layer_items_do_not_feed_the_state_of_in_use_items() {
    // configured_layers explicitly excludes basic_spec, but a basic_spec
    // item still structurally refines the in-use REQ-1 (e.g. a document
    // written before basic_spec was dropped from scope). REQ-1 is fully
    // covered on its own (AT-1 passes, an implements task closes vertical),
    // so its state must be `passing`, not pulled down to `uncovered` by an
    // out-of-scope child that coverage/gaps already treat as n/a (wiki/220
    // §2.1: "使用中でない層は対象外").
    let input = TraceInput {
        items: vec![
            item("REQ-1", "d1", "requirement", &[], &[]),
            item("AT-1", "d1", "acceptance", &[], &["REQ-1"]),
            item("BS-1", "d1", "basic_spec", &["REQ-1"], &[]),
        ],
        runs_latest: runs(&[("AT-1", "pass")]),
        task_requirement_links: vec![implements_link("t1", "REQ-1")],
        configured_layers: vec!["requirement".into(), "acceptance".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert_eq!(graph.coverage()["requirement"].horizontal.covered, 1);
    assert_eq!(graph.coverage()["requirement"].vertical.covered, 1);
    assert!(find_gap(graph.gaps(), GapKind::Unrefined, "REQ-1").is_none());
    assert_eq!(
        graph.state("REQ-1"),
        Some(ItemState::Passing),
        "an out-of-scope refines child must not pull an otherwise-covered item down to uncovered"
    );
}

#[test]
fn out_of_use_layer_verifier_result_does_not_feed_the_state_of_in_use_items() {
    // system_test is excluded from `[trace] layers`, but ST-1 still
    // structurally verifies REQ-1 (level 2 >= 1, a valid edge) and its
    // latest run failed. REQ-1 is fully covered in scope (AT-1 passes, an
    // implements task closes vertical), so the out-of-scope failing run must
    // not pull its state to `failing` (wiki/220 §2.1).
    let input = TraceInput {
        items: vec![
            item("REQ-1", "d1", "requirement", &[], &[]),
            item("AT-1", "d1", "acceptance", &[], &["REQ-1"]),
            item("ST-1", "d1", "system_test", &[], &["REQ-1"]),
        ],
        runs_latest: runs(&[("AT-1", "pass"), ("ST-1", "fail")]),
        task_requirement_links: vec![implements_link("t1", "REQ-1")],
        configured_layers: vec!["requirement".into(), "acceptance".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert_eq!(
        graph.state("REQ-1"),
        Some(ItemState::Passing),
        "an out-of-scope verifier's failing run must not fail an in-scope item"
    );
}

#[test]
fn out_of_use_layer_children_and_verifiers_do_not_create_coverage() {
    // The coverage-side twin of the state-side scope rule above: basic_spec
    // (the middle left layer) and acceptance (requirement's pair) are both
    // excluded from `[trace] layers`, while detailed_spec (a deeper left
    // layer) stays in use. REQ-1's only structural child (BS-1) and only
    // verifier (AT-1) live in excluded layers, so neither may count as
    // coverage (wiki/220 §2.1: "使用中でない層は対象外"): vertical must be
    // `uncovered` (a deeper left layer is in use, no in-scope child, no
    // implements task) and horizontal must be `n/a` (pair unused, no
    // in-scope verifier, not inline).
    let input = TraceInput {
        items: vec![
            item("REQ-1", "d1", "requirement", &[], &[]),
            item("BS-1", "d1", "basic_spec", &["REQ-1"], &[]),
            item("AT-1", "d1", "acceptance", &[], &["REQ-1"]),
        ],
        runs_latest: runs(&[("AT-1", "pass")]),
        configured_layers: vec!["requirement".into(), "detailed_spec".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let cov = &graph.coverage()["requirement"];
    assert_eq!(
        cov.vertical.uncovered, 1,
        "an out-of-scope refines child must not close vertical coverage"
    );
    assert!(find_gap(graph.gaps(), GapKind::Unrefined, "REQ-1").is_some());
    assert_eq!(
        cov.horizontal.na, 1,
        "an out-of-scope verifier must not close horizontal coverage"
    );
    assert_eq!(cov.horizontal.covered, 0);
    assert_eq!(graph.state("REQ-1"), Some(ItemState::Uncovered));
}

#[test]
fn horizontal_covered_by_a_deeper_right_side_layer_when_the_pair_layer_is_unused() {
    // requirement's pair is acceptance, but acceptance is not in use here.
    // system_test (level 2) is deep enough to legitimately verify a
    // requirement (level 1) item per `verifies_edge_valid`'s `>=` rule, so
    // it closes horizontal coverage on its own (wiki/220 §2.7: "pair 未使用
    // ならインラインか下位検証層の verifier で covered").
    let input = TraceInput {
        items: vec![
            item("REQ-1", "d1", "requirement", &[], &[]),
            item("ST-1", "d1", "system_test", &[], &["REQ-1"]),
        ],
        runs_latest: runs(&[("ST-1", "pass")]),
        task_requirement_links: vec![implements_link("t1", "REQ-1")],
        configured_layers: vec!["requirement".into(), "system_test".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert_eq!(graph.coverage()["requirement"].horizontal.covered, 1);
    assert!(find_gap(graph.gaps(), GapKind::Unverified, "REQ-1").is_none());
    assert_eq!(graph.state("REQ-1"), Some(ItemState::Passing));
}

// ---------------------------------------------------------------------
// Memoization: a wide shared subgraph is computed once, not once per parent.
// ---------------------------------------------------------------------

#[test]
fn shared_refining_child_is_resolved_once_regardless_of_how_many_parents_reference_it() {
    // One basic_spec item refining into 50 different requirement parents —
    // without memoization this would redo the child's own (non-trivial)
    // computation 50 times.
    let mut items = vec![item("BS-1", "d1", "basic_spec", &[], &[])];
    items[0].refines = (0..50).map(|i| format!("REQ-{i}")).collect();
    for i in 0..50 {
        items.push(item(&format!("REQ-{i}"), "d1", "requirement", &[], &[]));
    }
    let input = TraceInput {
        items,
        task_requirement_links: vec![implements_link("t1", "BS-1")],
        configured_layers: vec!["requirement".into(), "basic_spec".into()],
        ..Default::default()
    };
    let start = std::time::Instant::now();
    let graph = TraceGraph::build(&input);
    assert!(start.elapsed() < std::time::Duration::from_millis(200));
    for i in 0..50 {
        assert!(graph.state(&format!("REQ-{i}")).is_some());
    }
}

#[test]
fn memoization_resolves_each_item_exactly_once_despite_wide_fan_in() {
    // BS-1 refines 50 different requirement parents, so walking
    // `refines_children` from any of those 50 REQ-i's perspective recurses
    // into the *same* BS-1 node — and BS-1 itself has 3 refining DS
    // children, adding a second shared layer. Resolving all 50 REQ-i must
    // still only compute BS-1 and each DS-i once each (54 distinct items
    // total), not once per REQ-i that happens to reach them
    // (wiki/240-performance-design.md §5-5's memoized DP).
    let mut items: HashMap<String, ResolvedItem> = HashMap::new();
    for i in 0..50 {
        items.insert(
            format!("REQ-{i}"),
            ResolvedItem {
                doc_id: "d1".to_string(),
                layer: Some("requirement".to_string()),
                side: Some(LayerSide::Left),
                level: Some(1),
                refines: vec![],
                verifies: vec![],
                is_inline: false,
            },
        );
    }
    items.insert(
        "BS-1".to_string(),
        ResolvedItem {
            doc_id: "d1".to_string(),
            layer: Some("basic_spec".to_string()),
            side: Some(LayerSide::Left),
            level: Some(2),
            refines: (0..50).map(|i| format!("REQ-{i}")).collect(),
            verifies: vec![],
            is_inline: false,
        },
    );
    for i in 0..3 {
        items.insert(
            format!("DS-{i}"),
            ResolvedItem {
                doc_id: "d1".to_string(),
                layer: Some("detailed_spec".to_string()),
                side: Some(LayerSide::Left),
                level: Some(3),
                refines: vec!["BS-1".to_string()],
                verifies: vec![],
                is_inline: false,
            },
        );
    }
    let mut refines_children: HashMap<String, Vec<String>> = HashMap::new();
    for i in 0..50 {
        refines_children.insert(format!("REQ-{i}"), vec!["BS-1".to_string()]);
    }
    refines_children.insert(
        "BS-1".to_string(),
        (0..3).map(|i| format!("DS-{i}")).collect(),
    );
    let verified_by = HashMap::new();
    let runs_latest = HashMap::new();
    let coverage_status = HashMap::new();
    let in_use = set(&["requirement", "basic_spec", "detailed_spec"]);
    let mut memo = HashMap::new();
    let mut gaps = Vec::new();
    let mut dp = Dp {
        items: &items,
        refines_children: &refines_children,
        verified_by: &verified_by,
        runs_latest: &runs_latest,
        coverage_status: &coverage_status,
        in_use: &in_use,
        memo: &mut memo,
        in_progress: HashSet::new(),
        reported_cycles: HashSet::new(),
        gaps: &mut gaps,
        memo_misses: 0,
    };
    for i in 0..50 {
        dp.resolve(&format!("REQ-{i}"));
    }
    assert_eq!(
        dp.memo_misses, 54,
        "50 REQ + 1 BS-1 + 3 DS = 54 distinct items must each be computed exactly once, \
         regardless of the 50-way fan-in into BS-1"
    );
}
