//! Unit tests for [`super::derive_next_actions`] (wiki/260-vmodel-m2-design.md
//! §3.5, M2-10): one test per kind's detection rule, plus the deterministic
//! cross-kind/priority/layer-level/id ordering §3.5's closing sentence
//! specifies.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::Value;

use super::*;
use crate::storage::docs::layer::LayerRegistry;
use crate::trace::types::{TaskLinkRole, TaskRequirementLink, TraceInput, TraceItemInput};

fn item(id: &str, layer: &str, refines: &[&str], verifies: &[&str]) -> TraceItemInput {
    TraceItemInput {
        stable_id: id.to_string(),
        doc_id: "doc".to_string(),
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
        link_baselines: BTreeMap::new(),
        needs: None,
        approval: "draft".to_string(),
    }
}

fn input(items: Vec<TraceItemInput>) -> TraceInput {
    TraceInput {
        items,
        layer_registry: LayerRegistry::build(&[]).all().to_vec(),
        ..Default::default()
    }
}

fn meta_with(pairs: &[(&str, Option<&str>, Option<&str>)]) -> HashMap<String, ItemNextMeta> {
    pairs
        .iter()
        .map(|(id, priority, dev_stage)| {
            (
                id.to_string(),
                ItemNextMeta {
                    priority: priority.map(str::to_string),
                    dev_stage: dev_stage.map(str::to_string),
                    ..Default::default()
                },
            )
        })
        .collect()
}

fn kinds_of(actions: &[NextAction]) -> Vec<NextActionKind> {
    actions.iter().map(|a| a.kind).collect()
}

fn items_of(actions: &[NextAction]) -> Vec<String> {
    actions.iter().map(|a| a.item.clone().unwrap()).collect()
}

#[test]
fn fix_failing_ranks_first_for_a_failing_verifier() {
    let mut inp = input(vec![
        item("REQ-001", "requirement", &[], &[]),
        item("AT-001", "acceptance", &[], &["REQ-001"]),
    ]);
    inp.runs_latest
        .insert("AT-001".to_string(), "fail".to_string());
    let graph = TraceGraph::build(&inp);
    let meta = HashMap::new();

    let (actions, truncated) = derive_next_actions(&graph, &inp, &meta, None, &[], None, None, 10);

    assert!(!truncated);
    // REQ-001 also has no implementing task yet, so a create_task candidate
    // legitimately co-exists — fix_failing must still rank first (kind 1).
    assert_eq!(actions[0].kind, NextActionKind::FixFailing);
    assert_eq!(actions[0].item.as_deref(), Some("AT-001"));
    assert_eq!(actions[0].rank, 1);
    assert_eq!(actions[0].suggest.tool, "handoff_trace_slice");
}

#[test]
fn fix_failing_also_fires_for_blocked() {
    let mut inp = input(vec![
        item("REQ-001", "requirement", &[], &[]),
        item("AT-001", "acceptance", &[], &["REQ-001"]),
    ]);
    inp.runs_latest
        .insert("AT-001".to_string(), "blocked".to_string());
    let graph = TraceGraph::build(&inp);
    let meta = HashMap::new();

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, None, 10);
    assert_eq!(actions[0].kind, NextActionKind::FixFailing);
}

#[test]
fn review_suspect_fires_for_a_link_suspect_only() {
    let upstream = TraceItemInput {
        def_hash: Some("new-hash".to_string()),
        ..item("REQ-001", "requirement", &[], &[])
    };
    let mut baselines = BTreeMap::new();
    baselines.insert("REQ-001".to_string(), "old-hash".to_string());
    let child = TraceItemInput {
        link_baselines: baselines,
        ..item("SPEC-001", "basic_spec", &["REQ-001"], &[])
    };
    let inp = input(vec![upstream, child]);
    let graph = TraceGraph::build(&inp);
    let meta = HashMap::new();

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, None, 10);
    assert_eq!(actions[0].kind, NextActionKind::ReviewSuspect);
    assert_eq!(actions[0].item.as_deref(), Some("SPEC-001"));
    assert_eq!(actions[0].suggest.tool, "handoff_trace_impact");
}

#[test]
fn rerun_fires_for_reverify_verifier_whose_target_is_implemented() {
    let req = item("REQ-001", "requirement", &[], &[]);
    let at = item("AT-001", "acceptance", &[], &["REQ-001"]);
    let inp_items = vec![req, at];
    let mut inp = input(inp_items);
    inp.runs_latest
        .insert("AT-001".to_string(), "not_run".to_string());
    let graph = TraceGraph::build(&inp);
    let meta = meta_with(&[("REQ-001", None, Some("implemented"))]);

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, None, 10);
    assert_eq!(actions[0].kind, NextActionKind::Rerun);
    assert_eq!(actions[0].item.as_deref(), Some("AT-001"));
    assert_eq!(actions[0].suggest.tool, "handoff_trace_ingest");
}

#[test]
fn rerun_is_skipped_when_target_is_not_yet_implemented() {
    let req = item("REQ-001", "requirement", &[], &[]);
    let at = item("AT-001", "acceptance", &[], &["REQ-001"]);
    let mut inp = input(vec![req, at]);
    inp.runs_latest
        .insert("AT-001".to_string(), "not_run".to_string());
    let graph = TraceGraph::build(&inp);
    // REQ-001 has no dev_stage meta at all -> defaults to not_started.
    let meta = HashMap::new();

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, None, 10);
    assert!(kinds_of(&actions)
        .iter()
        .all(|k| *k != NextActionKind::Rerun));
}

#[test]
fn write_verification_fires_for_uncovered_horizontal() {
    // REQ-002 has no verifier at all -> horizontal uncovered. REQ-001/AT-001
    // put `acceptance` into the project's in-use (auto-detected) layer set
    // so REQ-002's missing verifier reads as `uncovered`, not `na` (an
    // effective layer set with no `acceptance` at all would make every
    // requirement's horizontal axis `na` instead).
    let inp = input(vec![
        item("REQ-001", "requirement", &[], &[]),
        item("AT-001", "acceptance", &[], &["REQ-001"]),
        item("REQ-002", "requirement", &[], &[]),
    ]);
    let graph = TraceGraph::build(&inp);
    let meta = HashMap::new();

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, None, 10);
    assert!(kinds_of(&actions).contains(&NextActionKind::WriteVerification));
    let a = actions
        .iter()
        .find(|a| a.kind == NextActionKind::WriteVerification)
        .unwrap();
    assert_eq!(a.item.as_deref(), Some("REQ-002"));
    assert_eq!(a.suggest.tool, "handoff_trace_scaffold");
}

#[test]
fn refine_fires_for_uncovered_vertical_when_an_upper_layer_is_in_use() {
    // basic_spec is deeper than requirement; SPEC-001 has no refines, and
    // requirement is "upper" and in use (REQ-001 exists), making SPEC-001's
    // vertical uncovered (no refining child is fine at the deepest level,
    // but refine fires on *this* item having an uncovered vertical itself —
    // use a requirement with no refining child and no implementing task
    // instead, which is the actual vertical-uncovered case for a top item).
    let inp = input(vec![
        item("REQ-001", "requirement", &[], &[]),
        item("AT-001", "acceptance", &[], &["REQ-001"]),
    ]);
    let graph = TraceGraph::build(&inp);
    let meta = HashMap::new();

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, None, 10);
    let refine = actions
        .iter()
        .find(|a| a.kind == NextActionKind::Refine && a.item.as_deref() == Some("REQ-001"));
    assert!(
        refine.is_some(),
        "expected a refine action for REQ-001, got {actions:?}"
    );
    assert_eq!(refine.unwrap().suggest.tool, "handoff_trace_update");
}

#[test]
fn create_task_fires_for_not_started_left_side_item_with_no_implements_task() {
    let inp = input(vec![item("REQ-001", "requirement", &[], &[])]);
    let graph = TraceGraph::build(&inp);
    let meta = HashMap::new();

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, None, 10);
    let a = actions
        .iter()
        .find(|a| a.kind == NextActionKind::CreateTask)
        .expect("expected a create_task action");
    assert_eq!(a.item.as_deref(), Some("REQ-001"));
    assert_eq!(a.suggest.tool, "handoff_trace_tasks");
}

#[test]
fn create_task_is_skipped_when_an_implements_task_already_exists() {
    let mut inp = input(vec![item("REQ-001", "requirement", &[], &[])]);
    inp.task_requirement_links.push(TaskRequirementLink {
        task_id: "t-1".to_string(),
        stable_id: "REQ-001".to_string(),
        role: TaskLinkRole::Implements,
        baseline_hash: None,
    });
    let graph = TraceGraph::build(&inp);
    let meta = HashMap::new();

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, None, 10);
    assert!(actions.iter().all(|a| a.kind != NextActionKind::CreateTask));
}

#[test]
fn fix_link_fires_for_a_dangling_reference() {
    let inp = input(vec![item("SPEC-001", "basic_spec", &["REQ-999"], &[])]);
    let graph = TraceGraph::build(&inp);
    let meta = HashMap::new();

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, None, 10);
    let a = actions
        .iter()
        .find(|a| a.kind == NextActionKind::FixLink)
        .expect("expected a fix_link action for the dangling refines");
    assert_eq!(a.item.as_deref(), Some("SPEC-001"));
}

#[test]
fn baseline_fires_for_an_unbaselined_reference() {
    // refines REQ-001 with no link_baselines entry at all -> unbaselined.
    let inp = input(vec![
        item("REQ-001", "requirement", &[], &[]),
        item("SPEC-001", "basic_spec", &["REQ-001"], &[]),
    ]);
    let graph = TraceGraph::build(&inp);
    let meta = HashMap::new();

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, None, 10);
    let a = actions
        .iter()
        .find(|a| a.kind == NextActionKind::Baseline)
        .expect("expected a baseline action");
    assert_eq!(a.item.as_deref(), Some("SPEC-001"));
    assert_eq!(a.suggest.tool, "handoff_trace_suspect");
}

#[test]
fn ordering_is_kind_then_priority_then_layer_level_then_id() {
    // Two create_task candidates (same kind): REQ-002 (P0) must sort before
    // REQ-001 (P1) despite REQ-001's lexicographically smaller id.
    let inp = input(vec![
        item("REQ-001", "requirement", &[], &[]),
        item("REQ-002", "requirement", &[], &[]),
    ]);
    let graph = TraceGraph::build(&inp);
    let meta = meta_with(&[("REQ-001", Some("P1"), None), ("REQ-002", Some("P0"), None)]);

    let (actions, _) = derive_next_actions(
        &graph,
        &inp,
        &meta,
        None,
        &[],
        None,
        Some(&HashSet::from([NextActionKind::CreateTask])),
        10,
    );
    assert_eq!(items_of(&actions), vec!["REQ-002", "REQ-001"]);
}

#[test]
fn ordering_puts_fix_failing_before_create_task_regardless_of_id() {
    let mut inp = input(vec![
        item("AT-999", "acceptance", &[], &["REQ-001"]),
        item("REQ-001", "requirement", &[], &[]),
    ]);
    inp.runs_latest
        .insert("AT-999".to_string(), "fail".to_string());
    let graph = TraceGraph::build(&inp);
    let meta = HashMap::new();

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, None, 10);
    let fix_pos = actions
        .iter()
        .position(|a| a.kind == NextActionKind::FixFailing);
    let create_pos = actions
        .iter()
        .position(|a| a.kind == NextActionKind::CreateTask);
    assert!(fix_pos.is_some() && create_pos.is_some());
    assert!(
        fix_pos < create_pos,
        "fix_failing (rank 1) must precede create_task (rank 6)"
    );
}

#[test]
fn kinds_filter_restricts_to_the_requested_kinds_only() {
    let inp = input(vec![item("REQ-001", "requirement", &[], &[])]);
    let graph = TraceGraph::build(&inp);
    let meta = HashMap::new();

    let (actions, _) = derive_next_actions(
        &graph,
        &inp,
        &meta,
        None,
        &[],
        None,
        Some(&HashSet::from([NextActionKind::Baseline])),
        10,
    );
    assert!(
        actions.is_empty(),
        "REQ-001 has no unbaselined link, so baseline-only filter yields nothing"
    );
}

#[test]
fn limit_truncates_and_reports_truncated_true() {
    let items: Vec<TraceItemInput> = (1..=5)
        .map(|n| item(&format!("REQ-{n:03}"), "requirement", &[], &[]))
        .collect();
    let inp = input(items);
    let graph = TraceGraph::build(&inp);
    let meta = HashMap::new();

    let (actions, truncated) = derive_next_actions(&graph, &inp, &meta, None, &[], None, None, 2);
    assert_eq!(actions.len(), 2);
    assert!(truncated);
}

#[test]
fn scope_ids_restricts_to_one_tasks_linked_items() {
    let inp = input(vec![
        item("REQ-001", "requirement", &[], &[]),
        item("REQ-002", "requirement", &[], &[]),
    ]);
    let graph = TraceGraph::build(&inp);
    let meta = HashMap::new();
    let scope: HashSet<String> = ["REQ-001".to_string()].into_iter().collect();

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, Some(&scope), &[], None, None, 10);
    assert!(actions.iter().all(|a| a.item.as_deref() == Some("REQ-001")));
    assert!(!actions.is_empty());
}

#[test]
fn layers_filter_restricts_to_items_in_those_layers() {
    let inp = input(vec![
        item("REQ-001", "requirement", &[], &[]),
        item("SPEC-001", "basic_spec", &[], &[]),
    ]);
    let graph = TraceGraph::build(&inp);
    let meta = HashMap::new();

    let (actions, _) = derive_next_actions(
        &graph,
        &inp,
        &meta,
        None,
        &["basic_spec".to_string()],
        None,
        None,
        10,
    );
    assert!(actions.iter().all(|a| a.item.as_deref() != Some("REQ-001")));
}

// -- M3: manual_pending kind + assignee filter (wiki/270-vmodel-m3-design.md
// §4.5, FR-307) --

fn item_with_method(id: &str, layer: &str, verifies: &[&str], method: &str) -> TraceItemInput {
    TraceItemInput {
        method: Some(method.to_string()),
        ..item(id, layer, &[], verifies)
    }
}

fn meta_with_assignee(id: &str, assignee: &str) -> HashMap<String, ItemNextMeta> {
    let mut m = HashMap::new();
    m.insert(
        id.to_string(),
        ItemNextMeta {
            assignee: Some(assignee.to_string()),
            ..Default::default()
        },
    );
    m
}

/// §4.5: an assigned manual-method verification item with no run yet fires
/// `manual_pending`, ranked 3 (same as `rerun`).
#[test]
fn manual_pending_fires_for_assigned_manual_item_never_run() {
    let req = item("REQ-001", "requirement", &[], &[]);
    let at = item_with_method("AT-001", "acceptance", &["REQ-001"], "manual");
    let inp = input(vec![req, at]);
    let graph = TraceGraph::build(&inp);
    let meta = meta_with_assignee("AT-001", "ryoma");

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, None, 10);
    let a = actions
        .iter()
        .find(|a| a.kind == NextActionKind::ManualPending)
        .expect("expected a manual_pending action for AT-001");
    assert_eq!(a.item.as_deref(), Some("AT-001"));
    assert_eq!(a.rank, 3, "manual_pending shares rerun's rank (3)");
}

/// §4.5: `visual`/`review` methods also qualify, not just `manual`.
#[test]
fn manual_pending_fires_for_visual_and_review_methods() {
    for method in ["visual", "review"] {
        let req = item("REQ-001", "requirement", &[], &[]);
        let at = item_with_method("AT-001", "acceptance", &["REQ-001"], method);
        let inp = input(vec![req, at]);
        let graph = TraceGraph::build(&inp);
        let meta = meta_with_assignee("AT-001", "ryoma");

        let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, None, 10);
        assert!(
            actions
                .iter()
                .any(|a| a.kind == NextActionKind::ManualPending
                    && a.item.as_deref() == Some("AT-001")),
            "method {method:?} must also qualify for manual_pending"
        );
    }
}

/// §4.5: an `auto` method item never fires `manual_pending`, even when
/// assigned and never run.
#[test]
fn manual_pending_does_not_fire_for_auto_method() {
    let req = item("REQ-001", "requirement", &[], &[]);
    let at = item_with_method("AT-001", "acceptance", &["REQ-001"], "auto");
    let inp = input(vec![req, at]);
    let graph = TraceGraph::build(&inp);
    let meta = meta_with_assignee("AT-001", "ryoma");

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, None, 10);
    assert!(actions
        .iter()
        .all(|a| a.kind != NextActionKind::ManualPending));
}

/// §4.5: a manual-method item with no `assignee` set never fires
/// `manual_pending` ("assignee が設定されており" is a hard condition).
#[test]
fn manual_pending_does_not_fire_without_an_assignee() {
    let req = item("REQ-001", "requirement", &[], &[]);
    let at = item_with_method("AT-001", "acceptance", &["REQ-001"], "manual");
    let inp = input(vec![req, at]);
    let graph = TraceGraph::build(&inp);
    let meta = HashMap::new(); // no assignee recorded for AT-001

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, None, 10);
    assert!(actions
        .iter()
        .all(|a| a.kind != NextActionKind::ManualPending));
}

/// §4.5: a manual-method, assigned item whose latest result is NOT `not_run`
/// (already recorded, e.g. `pass`) never fires `manual_pending` — only a
/// never-run item does.
#[test]
fn manual_pending_does_not_fire_once_a_result_is_recorded() {
    let req = item("REQ-001", "requirement", &[], &[]);
    let at = item_with_method("AT-001", "acceptance", &["REQ-001"], "manual");
    let mut inp = input(vec![req, at]);
    inp.runs_latest
        .insert("AT-001".to_string(), "pass".to_string());
    let graph = TraceGraph::build(&inp);
    let meta = meta_with_assignee("AT-001", "ryoma");

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, None, 10);
    assert!(actions
        .iter()
        .all(|a| a.kind != NextActionKind::ManualPending));
}

/// §4.5: `assignee` filter narrows the candidate set to only items whose
/// `ItemNextMeta.assignee` matches exactly; an item assigned to someone else
/// is excluded even though it would otherwise generate a candidate.
#[test]
fn assignee_filter_restricts_to_the_matching_assignee_only() {
    let req1 = item("REQ-001", "requirement", &[], &[]);
    let req2 = item("REQ-002", "requirement", &[], &[]);
    let inp = input(vec![req1, req2]);
    let graph = TraceGraph::build(&inp);
    let mut meta = meta_with_assignee("REQ-001", "ryoma");
    meta.insert(
        "REQ-002".to_string(),
        ItemNextMeta {
            assignee: Some("alice".to_string()),
            ..Default::default()
        },
    );

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], Some("ryoma"), None, 10);
    assert!(!actions.is_empty());
    assert!(actions.iter().all(|a| a.item.as_deref() != Some("REQ-002")));
}

/// `assignee` filter with no matching items at all yields an empty result
/// (not an error) and differs in count from the unfiltered call.
#[test]
fn assignee_filter_with_no_match_yields_fewer_actions_than_unfiltered() {
    let req1 = item("REQ-001", "requirement", &[], &[]);
    let req2 = item("REQ-002", "requirement", &[], &[]);
    let inp = input(vec![req1, req2]);
    let graph = TraceGraph::build(&inp);
    let meta = meta_with_assignee("REQ-001", "ryoma");

    let (unfiltered, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, None, 10);
    let (filtered, _) = derive_next_actions(
        &graph,
        &inp,
        &meta,
        None,
        &[],
        Some("someone-else"),
        None,
        10,
    );
    assert!(filtered.len() < unfiltered.len());
}

// -- M3-13: `relink_candidate` kind (wiki/270-vmodel-m3-design.md §4.7,
// FR-204) --

/// §4.7: `detailed_spec` has been added to the in-use layer set (here, via
/// `configured_layers` so `InUseLayers::source` is `Config`, same as a
/// project that just added `[trace] layers = [..., "detailed_spec", ...]`),
/// `UT-001` (a `unit_test` item) directly `verifies` `SPEC-001` (a
/// `basic_spec` item), and `DS-001` (a `detailed_spec` item) already
/// `refines` that same `SPEC-001` — so `UT-001` is a relink candidate: it
/// should verify `DS-001` instead of reaching straight past it to
/// `SPEC-001`.
#[test]
fn relink_candidate_fires_when_a_detailed_spec_item_already_covers_the_basic_spec_target() {
    let spec = item("SPEC-001", "basic_spec", &[], &[]);
    let ds = item("DS-001", "detailed_spec", &["SPEC-001"], &[]);
    let ut = item("UT-001", "unit_test", &[], &["SPEC-001"]);
    let mut inp = input(vec![spec, ds, ut]);
    inp.configured_layers = vec![
        "requirement".to_string(),
        "basic_spec".to_string(),
        "detailed_spec".to_string(),
        "unit_test".to_string(),
    ];
    let graph = TraceGraph::build(&inp);
    let meta = HashMap::new();

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, None, 10);
    let a = actions
        .iter()
        .find(|a| a.kind == NextActionKind::RelinkCandidate)
        .unwrap_or_else(|| {
            panic!("expected a relink_candidate action for UT-001, got {actions:?}")
        });
    assert_eq!(a.item.as_deref(), Some("UT-001"));
    assert_eq!(a.suggest.tool, "handoff_trace_update");
    assert_eq!(a.suggest.arguments["dry_run"], true);
    let ops = a.suggest.arguments["ops"]
        .as_array()
        .expect("ops array in suggest.arguments");
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0]["op"], "upsert_item");
    assert_eq!(ops[0]["id"], "UT-001");
    assert_eq!(
        ops[0]["attrs"]["verifies"].as_array().unwrap(),
        &[Value::from("DS-001")]
    );
}

/// §4.7: without a corresponding `detailed_spec` item refining `SPEC-001`,
/// `UT-001`'s direct `verifies: SPEC-001` link is not a relink candidate —
/// there is nothing to relink it to yet.
#[test]
fn relink_candidate_does_not_fire_without_a_corresponding_detailed_spec_item() {
    let spec = item("SPEC-001", "basic_spec", &[], &[]);
    let ut = item("UT-001", "unit_test", &[], &["SPEC-001"]);
    let mut inp = input(vec![spec, ut]);
    inp.configured_layers = vec![
        "requirement".to_string(),
        "basic_spec".to_string(),
        "detailed_spec".to_string(),
        "unit_test".to_string(),
    ];
    let graph = TraceGraph::build(&inp);
    let meta = HashMap::new();

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, None, 10);
    assert!(actions
        .iter()
        .all(|a| a.kind != NextActionKind::RelinkCandidate));
}

/// §4.7: when `detailed_spec` is not in the in-use layer set at all (no
/// `configured_layers`, nothing synced in that layer), a `unit_test` ->
/// `basic_spec` direct link is never flagged even if a `detailed_spec` item
/// happens to exist in the corpus (e.g. leftover from a different profile
/// scope) — the detection is gated on `detailed_spec` actually being in use.
#[test]
fn relink_candidate_does_not_fire_when_detailed_spec_is_not_in_use() {
    let spec = item("SPEC-001", "basic_spec", &[], &[]);
    let ut = item("UT-001", "unit_test", &[], &["SPEC-001"]);
    let inp = input(vec![spec, ut]);
    // No `configured_layers`/`profile_layers` -> Auto-detected from items
    // actually present, which here is only {basic_spec, unit_test} — so
    // `detailed_spec` is not in use even though its registry entry exists.
    let graph = TraceGraph::build(&inp);
    let meta = HashMap::new();

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, None, 10);
    assert!(actions
        .iter()
        .all(|a| a.kind != NextActionKind::RelinkCandidate));
}

/// A task (not just a `unit_test` item) that `implements` `SPEC-001` directly
/// is also a relink candidate once `DS-001` exists — §4.7 names both
/// `unit_test` / タスク as the detection target.
#[test]
fn relink_candidate_fires_for_a_task_that_implements_the_basic_spec_item_directly() {
    let spec = item("SPEC-001", "basic_spec", &[], &[]);
    let ds = item("DS-001", "detailed_spec", &["SPEC-001"], &[]);
    let mut inp = input(vec![spec, ds]);
    inp.configured_layers = vec!["basic_spec".to_string(), "detailed_spec".to_string()];
    inp.task_requirement_links.push(TaskRequirementLink {
        task_id: "t-1".to_string(),
        stable_id: "SPEC-001".to_string(),
        role: TaskLinkRole::Implements,
        baseline_hash: None,
    });
    let graph = TraceGraph::build(&inp);
    let meta = HashMap::new();

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, None, 10);
    let a = actions
        .iter()
        .find(|a| a.kind == NextActionKind::RelinkCandidate && a.task.as_deref() == Some("t-1"))
        .unwrap_or_else(|| {
            panic!("expected a relink_candidate action for task t-1, got {actions:?}")
        });
    assert_eq!(a.item.as_deref(), Some("SPEC-001"));
}
