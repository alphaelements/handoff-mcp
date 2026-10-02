//! Unit tests for [`super::TraceGraph`] (wiki/220-vmodel-integration-design.md
//! §2.7/§7, t360.9): link legitimacy (verifies level condition, refines
//! upward direction), dangling/invalid_link/cycle, state priority + skipped
//! exclusion + inline verification + deep coverage, horizontal/vertical
//! coverage incl. n/a via layer-skip, and every one of the 8 gap kinds.

use std::collections::{HashMap, HashSet};

use super::*;
use crate::trace::types::{
    CoverageStatus, EffectiveProfile, Gap, GapKind, ItemState, LayersSource, TaskDocLink,
    TaskLinkRole, TaskRequirementLink, TraceInput, TraceItemInput, UnbaselinedCounts, WaiverAxis,
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
        acceptance_labels: Vec::new(),
        derived: false,
        waived_axes: Vec::new(),
        def_hash: None,
        body_hash: None,
        link_baselines: std::collections::BTreeMap::new(),
        needs: None,
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
        baseline_hash: None,
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
// `needs` (wiki/270-vmodel-m3-design.md §2.1/§3.1, FR-202): item-level
// coverage requirements gating horizontal/vertical classification.
// ---------------------------------------------------------------------

/// §3.1: `needs: [acceptance]` on `REQ-1` means only an `acceptance`-layer
/// verifier counts toward horizontal coverage. A `system_test` verifier
/// (also pointed at REQ-1, both layers in use) does not count, and `REQ-1`
/// is reported `uncovered` despite having a verifier at all.
#[test]
fn needs_gates_horizontal_coverage_to_the_named_layers_only() {
    let wrong_layer_only = TraceInput {
        items: vec![
            TraceItemInput {
                needs: Some(vec!["acceptance".to_string()]),
                ..item("REQ-1", "d1", "requirement", &[], &[])
            },
            item("ST-1", "d2", "system_test", &[], &["REQ-1"]),
        ],
        configured_layers: vec![
            "requirement".into(),
            "acceptance".into(),
            "basic_spec".into(),
            "system_test".into(),
        ],
        ..Default::default()
    };
    let graph = TraceGraph::build(&wrong_layer_only);
    assert_eq!(
        graph.item_horizontal("REQ-1"),
        Some(CoverageStatus::Uncovered)
    );
    assert!(find_gap(graph.gaps(), GapKind::Unverified, "REQ-1").is_some());
    assert_eq!(
        graph.unwanted_coverage(),
        &[("REQ-1".to_string(), "ST-1".to_string())]
    );

    // The same REQ-1 with an *acceptance*-layer verifier instead: covered,
    // no unwanted_coverage entry.
    let right_layer = TraceInput {
        items: vec![
            TraceItemInput {
                needs: Some(vec!["acceptance".to_string()]),
                ..item("REQ-1", "d1", "requirement", &[], &[])
            },
            item("AT-1", "d2", "acceptance", &[], &["REQ-1"]),
        ],
        configured_layers: vec!["requirement".into(), "acceptance".into()],
        ..Default::default()
    };
    let graph2 = TraceGraph::build(&right_layer);
    assert_eq!(
        graph2.item_horizontal("REQ-1"),
        Some(CoverageStatus::Covered)
    );
    assert!(graph2.unwanted_coverage().is_empty());
}

/// §2.1 3-state: `Some(vec![])` (an authored empty `- needs:`) exempts the
/// item from horizontal coverage entirely — `waived`, not `uncovered`, and
/// no `unverified` gap — even with the pair layer in use and no verifier.
#[test]
fn needs_empty_vec_waives_horizontal_coverage() {
    let input = TraceInput {
        items: vec![TraceItemInput {
            needs: Some(Vec::new()),
            ..item("REQ-1", "d1", "requirement", &[], &[])
        }],
        configured_layers: vec!["requirement".into(), "acceptance".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert_eq!(graph.item_horizontal("REQ-1"), Some(CoverageStatus::Waived));
    assert!(find_gap(graph.gaps(), GapKind::Unverified, "REQ-1").is_none());
}

/// §2.1: an item with `needs: None` (unset) falls back to the project
/// default profile's `default_needs` — here, a project-default profile named
/// "minimal" (`requirement -> [acceptance]`) gates `REQ-1` exactly like an
/// explicit `needs: [acceptance]` would.
#[test]
fn unset_needs_falls_back_to_project_default_needs() {
    let mut project_default_needs = std::collections::BTreeMap::new();
    project_default_needs.insert("requirement".to_string(), vec!["acceptance".to_string()]);
    let input = TraceInput {
        items: vec![
            item("REQ-1", "d1", "requirement", &[], &[]),
            item("ST-1", "d2", "system_test", &[], &["REQ-1"]),
        ],
        configured_layers: vec![
            "requirement".into(),
            "acceptance".into(),
            "basic_spec".into(),
            "system_test".into(),
        ],
        project_default_needs,
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert_eq!(
        graph.item_horizontal("REQ-1"),
        Some(CoverageStatus::Uncovered)
    );
    assert_eq!(
        graph.unwanted_coverage(),
        &[("REQ-1".to_string(), "ST-1".to_string())]
    );
}

/// §2.1: an item's own explicit `needs` overrides `default_needs` — here the
/// project default would require `acceptance`, but `REQ-1` explicitly
/// requires `system_test` instead, so an `acceptance` verifier is the
/// "unwanted" one and `system_test` is what counts.
#[test]
fn explicit_needs_overrides_project_default_needs() {
    let mut project_default_needs = std::collections::BTreeMap::new();
    project_default_needs.insert("requirement".to_string(), vec!["acceptance".to_string()]);
    let input = TraceInput {
        items: vec![
            TraceItemInput {
                needs: Some(vec!["system_test".to_string()]),
                ..item("REQ-1", "d1", "requirement", &[], &[])
            },
            item("AT-1", "d2", "acceptance", &[], &["REQ-1"]),
            item("ST-1", "d3", "system_test", &[], &["REQ-1"]),
        ],
        configured_layers: vec![
            "requirement".into(),
            "acceptance".into(),
            "basic_spec".into(),
            "system_test".into(),
        ],
        project_default_needs,
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert_eq!(
        graph.item_horizontal("REQ-1"),
        Some(CoverageStatus::Covered)
    );
    assert_eq!(
        graph.unwanted_coverage(),
        &[("REQ-1".to_string(), "AT-1".to_string())]
    );
}

/// §3.1's vertical rule: `needs: [basic_spec]` on `REQ-1` means only a
/// `basic_spec`-layer `refines` child counts toward vertical coverage. A
/// child on a different left layer (hypothetically reachable via a custom
/// layer set) would not count — exercised here via the simpler "no children
/// at all, but also no implements task" uncovered baseline plus a
/// basic_spec child that *does* count once needs includes its layer.
#[test]
fn needs_gates_vertical_coverage_to_the_named_lower_layer() {
    let input = TraceInput {
        items: vec![
            TraceItemInput {
                needs: Some(vec!["basic_spec".to_string()]),
                ..item("REQ-1", "d1", "requirement", &[], &[])
            },
            item("SPEC-1", "d2", "basic_spec", &["REQ-1"], &[]),
        ],
        task_requirement_links: vec![implements_link("t1", "SPEC-1")],
        configured_layers: vec!["requirement".into(), "basic_spec".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert_eq!(graph.item_vertical("REQ-1"), Some(CoverageStatus::Covered));
}

/// §2.1 3-state: `Some(vec![])` exempts vertical coverage too — `waived`,
/// not `uncovered`, with a deeper layer in use and no refining child.
#[test]
fn needs_empty_vec_waives_vertical_coverage() {
    let input = TraceInput {
        items: vec![TraceItemInput {
            needs: Some(Vec::new()),
            ..item("REQ-1", "d1", "requirement", &[], &[])
        }],
        configured_layers: vec!["requirement".into(), "basic_spec".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert_eq!(graph.item_vertical("REQ-1"), Some(CoverageStatus::Waived));
    assert!(find_gap(graph.gaps(), GapKind::Unrefined, "REQ-1").is_none());
}

/// An item with no `needs` and no project `default_needs` at all is
/// ungated — `item_effective_needs` returns `None`, and every in-scope
/// verifier counts, exactly like pre-M3 behavior.
#[test]
fn item_with_no_needs_and_no_project_default_is_ungated() {
    let input = TraceInput {
        items: vec![
            item("REQ-1", "d1", "requirement", &[], &[]),
            item("AT-1", "d2", "acceptance", &[], &["REQ-1"]),
        ],
        configured_layers: vec!["requirement".into(), "acceptance".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert_eq!(graph.item_effective_needs("REQ-1"), None);
    assert_eq!(
        graph.item_horizontal("REQ-1"),
        Some(CoverageStatus::Covered)
    );
    assert!(graph.unwanted_coverage().is_empty());
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
            acceptance_labels: vec![],
            derived: false,
            waived_verify: false,
            waived_refine: false,
            needs: None,
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
            acceptance_labels: vec![],
            derived: false,
            waived_verify: false,
            waived_refine: false,
            needs: None,
        },
    );
    let mut refines_children = HashMap::new();
    refines_children.insert("A".to_string(), vec!["B".to_string()]);
    refines_children.insert("B".to_string(), vec!["A".to_string()]);
    let verified_by = HashMap::new();
    let runs_latest = HashMap::new();
    let horizontal = HashMap::new();
    let in_scope_items = set(&["A", "B"]);
    let task_implements = HashSet::new();
    let deeper_layer_in_use = HashMap::new();
    let mut memo = HashMap::new();
    let mut vertical = HashMap::new();
    let mut gaps = Vec::new();
    {
        let mut dp = Dp {
            items: &items,
            refines_children: &refines_children,
            verified_by: &verified_by,
            runs_latest: &runs_latest,
            horizontal: &horizontal,
            in_scope_items: &in_scope_items,
            task_implements: &task_implements,
            deeper_layer_in_use: &deeper_layer_in_use,
            effective_needs: &HashMap::new(),
            memo: &mut memo,
            vertical: &mut vertical,
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
                acceptance_labels: vec![],
                derived: false,
                waived_verify: false,
                waived_refine: false,
                needs: None,
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
            acceptance_labels: vec![],
            derived: false,
            waived_verify: false,
            waived_refine: false,
            needs: None,
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
                acceptance_labels: vec![],
                derived: false,
                waived_verify: false,
                waived_refine: false,
                needs: None,
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
    let horizontal = HashMap::new();
    let mut in_scope_items: HashSet<String> = (0..50).map(|i| format!("REQ-{i}")).collect();
    in_scope_items.insert("BS-1".to_string());
    in_scope_items.extend((0..3).map(|i| format!("DS-{i}")));
    let task_implements = HashSet::new();
    let deeper_layer_in_use = HashMap::new();
    let mut memo = HashMap::new();
    let mut vertical = HashMap::new();
    let mut gaps = Vec::new();
    let mut dp = Dp {
        items: &items,
        refines_children: &refines_children,
        verified_by: &verified_by,
        runs_latest: &runs_latest,
        horizontal: &horizontal,
        in_scope_items: &in_scope_items,
        task_implements: &task_implements,
        deeper_layer_in_use: &deeper_layer_in_use,
        effective_needs: &HashMap::new(),
        memo: &mut memo,
        vertical: &mut vertical,
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

// ---------------------------------------------------------------------
// M2-03 (wiki/260-vmodel-m2-design.md §2.1/§2.2/§3.1): derived, waivers,
// horizontal/vertical `partial`, `X#ACn` sub-references, per-item effective
// profile tree inheritance.
// ---------------------------------------------------------------------

#[test]
fn derived_left_item_suppresses_the_orphan_gap() {
    let mut bs = item("BS-1", "d1", "basic_spec", &[], &[]);
    bs.derived = true;
    let input = TraceInput {
        items: vec![item("REQ-1", "d1", "requirement", &[], &[]), bs],
        configured_layers: vec!["requirement".into(), "basic_spec".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert!(find_gap(graph.gaps(), GapKind::Orphan, "BS-1").is_none());
}

#[test]
fn derived_right_item_suppresses_the_orphan_gap() {
    let mut at = item("AT-1", "d1", "acceptance", &[], &[]);
    at.derived = true;
    let input = TraceInput {
        items: vec![at],
        configured_layers: vec!["acceptance".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert!(find_gap(graph.gaps(), GapKind::Orphan, "AT-1").is_none());
}

#[test]
fn waive_verify_reports_waived_instead_of_uncovered_and_suppresses_the_unverified_gap() {
    let mut req = item("REQ-1", "d1", "requirement", &[], &[]);
    req.waived_axes = vec![WaiverAxis::Verify];
    let input = TraceInput {
        items: vec![req],
        // pair (acceptance) in use, no verifier at all.
        configured_layers: vec!["requirement".into(), "acceptance".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert_eq!(graph.item_horizontal("REQ-1"), Some(CoverageStatus::Waived));
    assert_eq!(graph.coverage()["requirement"].horizontal.waived, 1);
    assert_eq!(graph.coverage()["requirement"].horizontal.uncovered, 0);
    assert!(find_gap(graph.gaps(), GapKind::Unverified, "REQ-1").is_none());
}

#[test]
fn waive_refine_reports_waived_instead_of_uncovered_and_suppresses_the_unrefined_gap() {
    let mut req = item("REQ-1", "d1", "requirement", &[], &[]);
    req.waived_axes = vec![WaiverAxis::Refine];
    let input = TraceInput {
        items: vec![req],
        configured_layers: vec!["requirement".into(), "basic_spec".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert_eq!(graph.item_vertical("REQ-1"), Some(CoverageStatus::Waived));
    assert_eq!(graph.coverage()["requirement"].vertical.waived, 1);
    assert_eq!(graph.coverage()["requirement"].vertical.uncovered, 0);
    assert!(find_gap(graph.gaps(), GapKind::Unrefined, "REQ-1").is_none());
}

#[test]
fn waiver_is_ignored_when_the_axis_is_already_covered_redundant_waiver() {
    // §3.1's priority order: covered/partial always win over waived — a
    // waiver never downgrades an already-satisfied axis (the
    // `redundant_waiver` lint this implies is `trace_lint`'s concern, not
    // implemented here).
    let mut req = item("REQ-1", "d1", "requirement", &[], &[]);
    req.waived_axes = vec![WaiverAxis::Verify];
    let input = TraceInput {
        items: vec![req, item("AT-1", "d1", "acceptance", &[], &["REQ-1"])],
        configured_layers: vec!["requirement".into(), "acceptance".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert_eq!(
        graph.item_horizontal("REQ-1"),
        Some(CoverageStatus::Covered)
    );
}

#[test]
fn verifies_with_ac_subreference_resolves_the_edge_to_the_base_item() {
    let input = TraceInput {
        items: vec![
            item("REQ-1", "d1", "requirement", &[], &[]),
            item("AT-1", "d1", "acceptance", &[], &["REQ-1#AC1"]),
        ],
        configured_layers: vec!["requirement".into(), "acceptance".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert_eq!(graph.verified_by("REQ-1"), &["AT-1".to_string()]);
    assert!(graph.gaps().iter().all(|g| g.kind != GapKind::Dangling));
    assert!(graph.gaps().iter().all(|g| g.kind != GapKind::InvalidLink));
}

#[test]
fn horizontal_partial_when_some_declared_acceptance_criteria_are_unverified() {
    let mut req = item("REQ-1", "d1", "requirement", &[], &[]);
    req.acceptance_labels = vec!["AC1".to_string(), "AC2".to_string()];
    let input = TraceInput {
        items: vec![req, item("AT-1", "d1", "acceptance", &[], &["REQ-1#AC1"])],
        // An implementing task closes vertical coverage so the only gap
        // this scenario could produce is the one this test is about
        // (horizontal) — isolates the assertion below from §3.1's
        // unrelated vertical `unrefined` gap.
        task_requirement_links: vec![implements_link("t1", "REQ-1")],
        configured_layers: vec!["requirement".into(), "acceptance".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert_eq!(
        graph.item_horizontal("REQ-1"),
        Some(CoverageStatus::Partial)
    );
    assert_eq!(graph.coverage()["requirement"].horizontal.partial, 1);
    assert_eq!(graph.coverage()["requirement"].horizontal.covered, 0);
    // `partial` is a softer, distinct classification from the M1 gap list
    // (wiki/260 §3.1) — the `unverified` gate stays exact-`uncovered`-only.
    assert!(graph
        .gaps()
        .iter()
        .all(|g| g.item.as_deref() != Some("REQ-1")));
}

#[test]
fn horizontal_covered_when_every_declared_acceptance_criterion_has_a_verifier() {
    let mut req = item("REQ-1", "d1", "requirement", &[], &[]);
    req.acceptance_labels = vec!["AC1".to_string(), "AC2".to_string()];
    let input = TraceInput {
        items: vec![
            req,
            item("AT-1", "d1", "acceptance", &[], &["REQ-1#AC1"]),
            item("AT-2", "d1", "acceptance", &[], &["REQ-1#AC2"]),
        ],
        configured_layers: vec!["requirement".into(), "acceptance".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert_eq!(
        graph.item_horizontal("REQ-1"),
        Some(CoverageStatus::Covered)
    );
    assert_eq!(graph.coverage()["requirement"].horizontal.covered, 1);
}

#[test]
fn verifying_the_whole_item_wins_over_a_partial_ac_subreference() {
    let mut req = item("REQ-1", "d1", "requirement", &[], &[]);
    req.acceptance_labels = vec!["AC1".to_string(), "AC2".to_string()];
    let input = TraceInput {
        items: vec![
            req,
            // §2.2: authoring both a sub-reference and the whole-item form
            // to the same target — the whole-item form wins.
            item("AT-1", "d1", "acceptance", &[], &["REQ-1#AC1", "REQ-1"]),
        ],
        configured_layers: vec!["requirement".into(), "acceptance".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert_eq!(
        graph.item_horizontal("REQ-1"),
        Some(CoverageStatus::Covered)
    );
    assert_eq!(
        graph.verified_by("REQ-1"),
        &["AT-1".to_string()],
        "still a single deduped edge"
    );
}

#[test]
fn ac_subreference_to_an_undeclared_label_falls_back_to_a_whole_item_reference() {
    // REQ-1 declares no acceptance criteria at all, yet AT-1 references
    // `REQ-1#AC9` — §2.2: "AC2 がない場合は...全体を検証するリンクとして扱う".
    let input = TraceInput {
        items: vec![
            item("REQ-1", "d1", "requirement", &[], &[]),
            item("AT-1", "d1", "acceptance", &[], &["REQ-1#AC9"]),
        ],
        configured_layers: vec!["requirement".into(), "acceptance".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert_eq!(
        graph.item_horizontal("REQ-1"),
        Some(CoverageStatus::Covered)
    );
}

#[test]
fn vertical_partial_when_a_refining_child_is_itself_uncovered_deep_coverage() {
    // §11 Q7 ("deep coverage"): REQ-1 has a refining child (BS-1), so it is
    // not itself `uncovered` — but BS-1 has no children/implementing task of
    // its own and is thus vertically `uncovered`, which classifies REQ-1 as
    // `partial`, not `covered` (M1's `covered` count shrinks under M2).
    let input = TraceInput {
        items: vec![
            item("REQ-1", "d1", "requirement", &[], &[]),
            item("BS-1", "d1", "basic_spec", &["REQ-1"], &[]),
        ],
        configured_layers: vec!["requirement".into(), "basic_spec".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert_eq!(graph.item_vertical("BS-1"), Some(CoverageStatus::Uncovered));
    assert_eq!(graph.item_vertical("REQ-1"), Some(CoverageStatus::Partial));
    assert_eq!(graph.coverage()["requirement"].vertical.partial, 1);
    assert_eq!(graph.coverage()["requirement"].vertical.covered, 0);
    assert!(find_gap(graph.gaps(), GapKind::Unrefined, "REQ-1").is_none());
    assert!(find_gap(graph.gaps(), GapKind::Unrefined, "BS-1").is_some());
}

#[test]
fn doc_profile_override_applies_to_its_whole_reachable_tree_and_out_of_profile_children_drop_scope()
{
    // ROOT-1's document overrides `trace_profile` to a minimal-like profile
    // that doesn't include `basic_spec`. BS-1 lives in a *different*,
    // non-overridden document but is only reachable via ROOT-1's `refines`
    // tree (§11 Q6: "リンクでたどれる要件ツリー") — it inherits ROOT-1's
    // overridden profile, not the project default, even though the project
    // still has `basic_spec` configured project-wide.
    let mut overrides = HashMap::new();
    overrides.insert(
        "root-doc".to_string(),
        EffectiveProfile {
            name: "minimal_like".to_string(),
            layers: vec!["requirement".to_string(), "acceptance".to_string()],
        },
    );
    let input = TraceInput {
        items: vec![
            item("ROOT-1", "root-doc", "requirement", &[], &[]),
            item("BS-1", "other-doc", "basic_spec", &["ROOT-1"], &[]),
        ],
        task_requirement_links: vec![implements_link("t1", "ROOT-1")],
        configured_layers: vec![
            "requirement".into(),
            "basic_spec".into(),
            "acceptance".into(),
        ],
        doc_profile_overrides: overrides,
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert_eq!(graph.item_profile("ROOT-1"), &["minimal_like".to_string()]);
    assert_eq!(graph.item_profile("BS-1"), &["minimal_like".to_string()]);
    // basic_spec isn't in the overridden profile's layers, so BS-1 falls
    // entirely out of scope (§2.1 規則 3) even though the *project* still
    // has basic_spec configured.
    assert!(graph.coverage().get("basic_spec").is_none());
    // ROOT-1's own effective layers (minimal_like) contain no deeper *left*
    // layer than `requirement` itself, so its vertical axis is satisfied by
    // the implementing task alone, regardless of BS-1 dropping out of scope.
    assert_eq!(graph.item_vertical("ROOT-1"), Some(CoverageStatus::Covered));
}

#[test]
fn item_reachable_from_both_a_default_and_an_overridden_root_carries_both_profiles() {
    // §2.1 規則 1's last sentence: "既定の根と上書きの根の両方から届く項目は、
    // 両方のプロファイルを持つ".
    let mut overrides = HashMap::new();
    overrides.insert(
        "doc-b".to_string(),
        EffectiveProfile {
            name: "custom".to_string(),
            layers: vec!["requirement".to_string(), "detailed_spec".to_string()],
        },
    );
    let input = TraceInput {
        items: vec![
            item("REQ-A", "doc-a", "requirement", &[], &[]),
            item("REQ-B", "doc-b", "requirement", &[], &[]),
            item("AT-1", "doc-a", "acceptance", &[], &["REQ-A", "REQ-B"]),
        ],
        configured_layers: vec!["requirement".into(), "acceptance".into()],
        doc_profile_overrides: overrides,
        project_default_profile_name: Some("standard".to_string()),
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let mut names = graph.item_profile("AT-1").to_vec();
    names.sort();
    assert_eq!(names, vec!["custom".to_string(), "standard".to_string()]);
}

#[test]
fn build_wires_suspects_into_the_graph_and_per_layer_coverage() {
    // M2-05 (wiki/260 §3.2): `TraceGraph::build` folds suspect derivation
    // into the same request, and aggregates it per-layer alongside
    // horizontal/vertical/state.
    let upstream = TraceItemInput {
        def_hash: Some("new-hash".to_string()),
        ..item("REQ-001", "doc", "requirement", &[], &[])
    };
    let mut baselines = std::collections::BTreeMap::new();
    baselines.insert("REQ-001".to_string(), "old-hash".to_string());
    let child = TraceItemInput {
        link_baselines: baselines,
        ..item("SPEC-001", "doc", "basic_spec", &["REQ-001"], &[])
    };
    let input = TraceInput {
        items: vec![upstream, child],
        configured_layers: vec!["requirement".into(), "basic_spec".into()],
        ..Default::default()
    };

    let graph = TraceGraph::build(&input);

    assert_eq!(graph.suspects().len(), 1);
    assert_eq!(graph.suspects()[0].item, "SPEC-001");
    assert_eq!(graph.unbaselined_counts(), UnbaselinedCounts::default());
    let cov = graph.coverage().get("basic_spec").unwrap();
    assert_eq!(cov.suspect.links, 1);
    assert_eq!(cov.suspect.items, 1);
    assert_eq!(cov.suspect.tasks, 0);
    assert_eq!(cov.suspect.results, 0);
}

#[test]
fn build_reports_unbaselined_links_for_trace_suspect_baseline() {
    let upstream = TraceItemInput {
        def_hash: Some("hash".to_string()),
        ..item("REQ-001", "doc", "requirement", &[], &[])
    };
    let child = item("SPEC-001", "doc", "basic_spec", &["REQ-001"], &[]);
    let input = TraceInput {
        items: vec![upstream, child],
        configured_layers: vec!["requirement".into(), "basic_spec".into()],
        ..Default::default()
    };

    let graph = TraceGraph::build(&input);

    assert!(graph.suspects().is_empty());
    assert_eq!(graph.unbaselined_counts().links, 1);
    assert_eq!(graph.unbaselined_links().len(), 1);
    assert_eq!(graph.unbaselined_links()[0].item, "SPEC-001");
    assert_eq!(
        graph.unbaselined_links()[0].current_hash.as_deref(),
        Some("hash")
    );
}
