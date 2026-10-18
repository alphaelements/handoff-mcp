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
        with_acs(item("REQ-002", "requirement", &[], &[]), &["AC1"]),
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
    assert_eq!(a.suggest.arguments["target_doc"], "doc");
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
    // No basic_spec document exists yet, so the suggestion creates one (the
    // update-into-an-existing-document form is covered by
    // `suggest_arguments_name_the_documents_the_tools_require`).
    assert_eq!(refine.unwrap().suggest.tool, "handoff_doc_save");
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

// ---------------------------------------------------------------------
// requirement tree (SPEC-TREE-014): only leaves and integrated composites
// are action candidates
// ---------------------------------------------------------------------

fn child(id: &str, parent: &str) -> TraceItemInput {
    let mut it = with_acs(item(id, "requirement", &[], &[]), &["AC1"]);
    it.tree.parents = vec![parent.to_string()];
    it
}

fn tree_actions(items: Vec<TraceItemInput>) -> Vec<NextAction> {
    let mut inp = input(items);
    inp.configured_layers = vec!["requirement".to_string(), "acceptance".to_string()];
    let graph = TraceGraph::build(&inp);
    let (actions, _) =
        derive_next_actions(&graph, &inp, &HashMap::new(), None, &[], None, None, 100);
    actions
}

fn candidate_items(actions: &[NextAction], kind: NextActionKind) -> Vec<String> {
    actions
        .iter()
        .filter(|a| a.kind == kind)
        .map(|a| a.item.clone().unwrap())
        .collect()
}

#[test]
fn a_composite_without_integrated_acceptance_gets_no_action_but_its_leaves_do() {
    let actions = tree_actions(vec![
        item("REQ-P", "requirement", &[], &[]),
        child("REQ-1", "REQ-P"),
        child("REQ-2", "REQ-P"),
        item("AT-1", "acceptance", &[], &["REQ-1"]),
    ]);
    assert_eq!(
        candidate_items(&actions, NextActionKind::CreateTask),
        vec!["REQ-1", "REQ-2"],
        "{actions:?}"
    );
    assert_eq!(
        candidate_items(&actions, NextActionKind::WriteVerification),
        vec!["REQ-2"],
        "{actions:?}"
    );
    assert!(
        !items_of(&actions).contains(&"REQ-P".to_string()),
        "composite REQ-P must not be an action target: {actions:?}"
    );
}

#[test]
fn a_composite_with_integrated_acceptance_is_still_an_action_candidate() {
    let actions = tree_actions(vec![
        item("REQ-P", "requirement", &[], &[]),
        child("REQ-1", "REQ-P"),
        item("AT-P", "acceptance", &[], &["REQ-P"]),
    ]);
    assert_eq!(
        candidate_items(&actions, NextActionKind::CreateTask),
        vec!["REQ-1", "REQ-P"],
        "{actions:?}"
    );
}

#[test]
fn a_declared_decision_with_children_follows_the_same_rule() {
    use crate::storage::docs::model::TreeKindDecl;
    let mut decision = item("REQ-D", "requirement", &[], &[]);
    decision.tree.kind_decl = Some(TreeKindDecl::Decision);
    let actions = tree_actions(vec![
        decision,
        child("REQ-1", "REQ-D"),
        item("AT-1", "acceptance", &[], &["REQ-1"]),
    ]);
    assert_eq!(
        candidate_items(&actions, NextActionKind::CreateTask),
        vec!["REQ-1"],
        "{actions:?}"
    );
}

// ---------------------------------------------------------------------
// t-vfix.7: every suggest must be executable as-is against its tool's
// published input schema
// ---------------------------------------------------------------------

/// Minimal JSON-Schema checker for the subset the tool definitions use
/// (`type`, `enum`, `required`, `properties` — an unknown key is an error,
/// since a typo'd argument would otherwise be silently ignored by the
/// handler — `items`, `additionalProperties` as a schema, and a
/// `oneOf` of `required`/`not.required` branches).
fn schema_errors(schema: &Value, value: &Value, path: &str, errs: &mut Vec<String>) {
    if let Some(t) = schema.get("type").and_then(Value::as_str) {
        let ok = match t {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "boolean" => value.is_boolean(),
            "integer" => value.is_i64() || value.is_u64(),
            "number" => value.is_number(),
            _ => true,
        };
        if !ok {
            errs.push(format!("{path}: expected {t}, got {value}"));
            return;
        }
    }
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
        if !allowed.contains(value) {
            errs.push(format!("{path}: {value} not in enum {allowed:?}"));
        }
    }
    if let Some(obj) = value.as_object() {
        if let Some(props) = schema.get("properties").and_then(Value::as_object) {
            for (k, v) in obj {
                match props.get(k) {
                    Some(sub) => schema_errors(sub, v, &format!("{path}.{k}"), errs),
                    None => errs.push(format!("{path}: unknown property {k:?}")),
                }
            }
        }
        if let Some(extra) = schema.get("additionalProperties").filter(|a| a.is_object()) {
            for (k, v) in obj {
                let declared = schema
                    .get("properties")
                    .and_then(Value::as_object)
                    .is_some_and(|p| p.contains_key(k));
                if !declared {
                    schema_errors(extra, v, &format!("{path}.{k}"), errs);
                }
            }
        }
        if let Some(req) = schema.get("required").and_then(Value::as_array) {
            for r in req.iter().filter_map(Value::as_str) {
                if !obj.contains_key(r) {
                    errs.push(format!("{path}: missing required property {r:?}"));
                }
            }
        }
        if let Some(branches) = schema.get("oneOf").and_then(Value::as_array) {
            let matching = branches
                .iter()
                .filter(|b| {
                    let required_ok = b.get("required").and_then(Value::as_array).is_none_or(|r| {
                        r.iter()
                            .filter_map(Value::as_str)
                            .all(|k| obj.contains_key(k))
                    });
                    let not_ok = b
                        .pointer("/not/required")
                        .and_then(Value::as_array)
                        .is_none_or(|r| {
                            !r.iter()
                                .filter_map(Value::as_str)
                                .all(|k| obj.contains_key(k))
                        });
                    required_ok && not_ok
                })
                .count();
            if matching != 1 {
                errs.push(format!("{path}: matches {matching} oneOf branches, want 1"));
            }
        }
    }
    if let (Some(arr), Some(items)) = (value.as_array(), schema.get("items")) {
        for (i, v) in arr.iter().enumerate() {
            schema_errors(items, v, &format!("{path}[{i}]"), errs);
        }
    }
}

fn tool_schema(name: &str) -> Value {
    crate::mcp::tools::all_tool_definitions()
        .into_iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("suggest names unknown tool {name}"))
        .input_schema
}

fn assert_suggest_matches_schema(a: &NextAction) {
    let schema = tool_schema(&a.suggest.tool);
    let mut errs = Vec::new();
    schema_errors(&schema, &a.suggest.arguments, "$", &mut errs);
    assert!(
        errs.is_empty(),
        "{:?} suggest for {:?} violates {}'s schema: {errs:?}\nargs: {}",
        a.kind,
        a.item,
        a.suggest.tool,
        a.suggest.arguments
    );
}

fn in_doc(mut it: TraceItemInput, doc: &str) -> TraceItemInput {
    it.doc_id = doc.to_string();
    it
}

fn with_acs(mut it: TraceItemInput, labels: &[&str]) -> TraceItemInput {
    it.acceptance_labels = labels.iter().map(|s| s.to_string()).collect();
    it
}

const ALL_KINDS: [NextActionKind; 10] = [
    NextActionKind::FixFailing,
    NextActionKind::ReviewSuspect,
    NextActionKind::Rerun,
    NextActionKind::ManualPending,
    NextActionKind::WriteVerification,
    NextActionKind::Refine,
    NextActionKind::CreateTask,
    NextActionKind::FixLink,
    NextActionKind::Baseline,
    NextActionKind::RelinkCandidate,
];

fn all_layers() -> Vec<String> {
    [
        "requirement",
        "basic_spec",
        "detailed_spec",
        "acceptance",
        "system_test",
        "unit_test",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// One corpus that makes every next-action kind fire at least once.
fn every_kind_corpus() -> (TraceInput, HashMap<String, ItemNextMeta>) {
    let mut suspect_up = in_doc(item("REQ-005", "requirement", &[], &[]), "d-req");
    suspect_up.def_hash = Some("new-hash".to_string());
    let mut suspect_down = in_doc(item("SPEC-002", "basic_spec", &["REQ-005"], &[]), "d-spec");
    suspect_down
        .link_baselines
        .insert("REQ-005".to_string(), "old-hash".to_string());
    let mut manual = in_doc(
        item_with_method("AT-003", "acceptance", &["REQ-004"], "manual"),
        "d-at",
    );
    manual.method = Some("manual".to_string());

    let mut inp = input(vec![
        // verified by AT-001 (failing) -> fix_failing; has a refining child.
        with_acs(
            in_doc(item("REQ-001", "requirement", &[], &[]), "d-req"),
            &["AC1"],
        ),
        in_doc(item("AT-001", "acceptance", &[], &["REQ-001"]), "d-at"),
        in_doc(item("SPEC-001", "basic_spec", &["REQ-001"], &[]), "d-spec"),
        in_doc(item("DS-001", "detailed_spec", &["SPEC-001"], &[]), "d-ds"),
        // relink candidate (unit_test level 3 >= detailed_spec level 3) ...
        in_doc(item("UT-001", "unit_test", &[], &["SPEC-001"]), "d-ut"),
        // ... but not a system_test verifier (level 2 < 3).
        in_doc(item("ST-001", "system_test", &[], &["SPEC-001"]), "d-st"),
        // unverified, unrefined, not started, with acceptance criteria.
        with_acs(
            in_doc(item("REQ-003", "requirement", &[], &[]), "d-req"),
            &["AC1"],
        ),
        // rerun + manual_pending target.
        in_doc(item("REQ-004", "requirement", &[], &[]), "d-req"),
        in_doc(item("AT-002", "acceptance", &[], &["REQ-004"]), "d-at"),
        manual,
        suspect_up,
        suspect_down,
        // dangling refines -> fix_link.
        in_doc(item("SPEC-003", "basic_spec", &["NOPE-1"], &[]), "d-spec"),
    ]);
    inp.configured_layers = all_layers();
    inp.runs_latest
        .insert("AT-001".to_string(), "fail".to_string());
    inp.runs_latest
        .insert("AT-002".to_string(), "not_run".to_string());
    inp.runs_latest
        .insert("AT-003".to_string(), "not_run".to_string());
    inp.task_requirement_links.push(TaskRequirementLink {
        task_id: "t-1".to_string(),
        stable_id: "SPEC-001".to_string(),
        role: TaskLinkRole::Implements,
        baseline_hash: None,
    });
    inp.task_requirement_links.push(TaskRequirementLink {
        task_id: "t-1".to_string(),
        stable_id: "REQ-001".to_string(),
        role: TaskLinkRole::Implements,
        baseline_hash: None,
    });
    inp.task_status
        .insert("t-1".to_string(), "in_progress".to_string());

    let mut meta = meta_with(&[("REQ-004", None, Some("implemented"))]);
    meta.insert(
        "AT-003".to_string(),
        ItemNextMeta {
            assignee: Some("bob".to_string()),
            ..Default::default()
        },
    );
    (inp, meta)
}

fn every_kind_actions() -> Vec<NextAction> {
    let (inp, meta) = every_kind_corpus();
    let graph = TraceGraph::build(&inp);
    derive_next_actions(&graph, &inp, &meta, None, &[], None, None, 1000).0
}

#[test]
fn the_corpus_exercises_every_kind() {
    let actions = every_kind_actions();
    for kind in ALL_KINDS {
        assert!(
            actions.iter().any(|a| a.kind == kind),
            "no {kind:?} action in {actions:#?}"
        );
    }
}

#[test]
fn every_suggest_validates_against_its_tools_schema() {
    for a in every_kind_actions() {
        assert_suggest_matches_schema(&a);
    }
}

#[test]
fn suggest_arguments_name_the_documents_the_tools_require() {
    let actions = every_kind_actions();
    let of = |kind, item: &str| {
        actions
            .iter()
            .find(|a| a.kind == kind && a.item.as_deref() == Some(item))
            .unwrap_or_else(|| panic!("no {kind:?} for {item}"))
    };
    // write_verification scaffolds into the pair layer's own document.
    let wv = of(NextActionKind::WriteVerification, "REQ-003");
    assert_eq!(wv.suggest.tool, "handoff_trace_scaffold");
    assert_eq!(wv.suggest.arguments["target_doc"], "d-at");
    assert_eq!(
        wv.suggest.arguments["items"],
        serde_json::json!(["REQ-003"])
    );
    // refine creates the child in the next deeper layer's document.
    let rf = of(NextActionKind::Refine, "REQ-003");
    assert_eq!(rf.suggest.tool, "handoff_trace_update");
    let op = &rf.suggest.arguments["ops"][0];
    assert_eq!(op["op"], "upsert_item");
    assert_eq!(op["doc"], "d-spec");
    assert_eq!(op["attrs"]["refines"], serde_json::json!(["REQ-003"]));
    assert!(op["id"].as_str().unwrap().starts_with("SPEC-"), "{op}");
    // baseline is scoped to the item's own document and actually applies.
    let bl = actions
        .iter()
        .find(|a| a.kind == NextActionKind::Baseline && a.item.as_deref() == Some("SPEC-001"))
        .unwrap();
    assert_eq!(bl.suggest.arguments["action"], "baseline");
    assert_eq!(bl.suggest.arguments["dry_run"], false);
    assert_eq!(bl.suggest.arguments["scope"]["doc"], "d-spec");
    // manual_pending records via `results`, with a non-committal result.
    let mp = of(NextActionKind::ManualPending, "AT-003");
    assert_eq!(mp.suggest.arguments["results"][0]["item"], "AT-003");
    assert_eq!(mp.suggest.arguments["results"][0]["result"], "not_run");
    // relink names the verifier's own document.
    let rl = of(NextActionKind::RelinkCandidate, "UT-001");
    assert_eq!(rl.suggest.arguments["ops"][0]["doc"], "d-ut");
}

#[test]
fn refine_and_write_verification_fall_back_to_creating_the_missing_document() {
    // No acceptance document and no basic_spec document exist anywhere.
    let mut inp = input(vec![with_acs(
        in_doc(item("REQ-001", "requirement", &[], &[]), "d-req"),
        &["AC1"],
    )]);
    inp.configured_layers = vec![
        "requirement".to_string(),
        "basic_spec".to_string(),
        "acceptance".to_string(),
    ];
    let graph = TraceGraph::build(&inp);
    let (actions, _) =
        derive_next_actions(&graph, &inp, &HashMap::new(), None, &[], None, None, 100);
    for kind in [NextActionKind::WriteVerification, NextActionKind::Refine] {
        let a = actions
            .iter()
            .find(|a| a.kind == kind)
            .unwrap_or_else(|| panic!("no {kind:?} in {actions:?}"));
        assert_eq!(a.suggest.tool, "handoff_doc_save", "{kind:?}");
        assert_suggest_matches_schema(a);
    }
    let wv = actions
        .iter()
        .find(|a| a.kind == NextActionKind::WriteVerification)
        .unwrap();
    assert_eq!(wv.suggest.arguments["layer"], "acceptance");
}

fn docs_ref(id: &str, slug: &str, layer: Option<&str>) -> crate::trace::types::DocRefInput {
    crate::trace::types::DocRefInput {
        id: id.to_string(),
        slug: slug.to_string(),
        layer: layer.map(str::to_string),
    }
}

fn refine_and_wv_input() -> TraceInput {
    let mut inp = input(vec![with_acs(
        in_doc(item("REQ-001", "requirement", &[], &[]), "d-req"),
        &["AC1"],
    )]);
    inp.configured_layers = vec![
        "requirement".to_string(),
        "basic_spec".to_string(),
        "acceptance".to_string(),
    ];
    inp
}

#[test]
fn refine_and_write_verification_reuse_an_existing_empty_layer_document() {
    // A basic_spec / acceptance document exists but has no item yet: the
    // layer is "missing" an item, not a document, so the suggestion must
    // write into it instead of trying to create a second one.
    let mut inp = refine_and_wv_input();
    inp.docs = vec![
        docs_ref("d-req", "req", Some("requirement")),
        docs_ref("d-bs", "basic-spec", Some("basic_spec")),
        docs_ref("d-at", "acceptance", Some("acceptance")),
    ];
    let graph = TraceGraph::build(&inp);
    let (actions, _) =
        derive_next_actions(&graph, &inp, &HashMap::new(), None, &[], None, None, 100);
    let refine = actions
        .iter()
        .find(|a| a.kind == NextActionKind::Refine)
        .unwrap_or_else(|| panic!("no refine in {actions:?}"));
    assert_eq!(refine.suggest.tool, "handoff_trace_update");
    assert_eq!(refine.suggest.arguments["ops"][0]["doc"], "d-bs");
    assert_suggest_matches_schema(refine);
    let wv = actions
        .iter()
        .find(|a| a.kind == NextActionKind::WriteVerification)
        .unwrap_or_else(|| panic!("no write_verification in {actions:?}"));
    assert_eq!(wv.suggest.tool, "handoff_trace_scaffold");
    assert_eq!(wv.suggest.arguments["target_doc"], "d-at");
    assert_suggest_matches_schema(wv);
}

#[test]
fn created_layer_document_slug_never_collides_with_an_existing_document() {
    // Documents already own the natural slugs (one without a layer, one
    // with another layer): `handoff_doc_save` would refuse a reused slug.
    let mut inp = refine_and_wv_input();
    inp.docs = vec![
        docs_ref("d-req", "req", Some("requirement")),
        docs_ref("d-notes", "basic-spec", None),
        docs_ref("d-other", "basic-spec-2", Some("requirement")),
        docs_ref("d-notes2", "acceptance", Some("detailed_spec")),
    ];
    let graph = TraceGraph::build(&inp);
    let (actions, _) =
        derive_next_actions(&graph, &inp, &HashMap::new(), None, &[], None, None, 100);
    let refine = actions
        .iter()
        .find(|a| a.kind == NextActionKind::Refine)
        .unwrap_or_else(|| panic!("no refine in {actions:?}"));
    assert_eq!(refine.suggest.tool, "handoff_doc_save");
    assert_eq!(refine.suggest.arguments["layer"], "basic_spec");
    assert_eq!(refine.suggest.arguments["slug"], "basic-spec-3");
    let wv = actions
        .iter()
        .find(|a| a.kind == NextActionKind::WriteVerification)
        .unwrap_or_else(|| panic!("no write_verification in {actions:?}"));
    assert_eq!(wv.suggest.tool, "handoff_doc_save");
    assert_eq!(wv.suggest.arguments["slug"], "acceptance-2");
    assert_suggest_matches_schema(wv);
}

#[test]
fn refine_is_not_suggested_for_an_item_in_the_deepest_left_layer() {
    // detailed_spec has no deeper left layer to refine into.
    let mut inp = input(vec![in_doc(
        item("DS-001", "detailed_spec", &[], &[]),
        "d-ds",
    )]);
    inp.configured_layers = vec!["detailed_spec".to_string(), "unit_test".to_string()];
    let graph = TraceGraph::build(&inp);
    let (actions, _) =
        derive_next_actions(&graph, &inp, &HashMap::new(), None, &[], None, None, 100);
    assert!(
        actions.iter().all(|a| a.kind != NextActionKind::Refine),
        "{actions:?}"
    );
}

#[test]
fn write_verification_is_not_suggested_for_an_item_without_acceptance_criteria() {
    let inp = input(vec![
        with_acs(item("REQ-001", "requirement", &[], &[]), &["AC1"]),
        item("AT-001", "acceptance", &[], &["REQ-001"]),
        item("REQ-002", "requirement", &[], &[]),
    ]);
    let graph = TraceGraph::build(&inp);
    let (actions, _) =
        derive_next_actions(&graph, &inp, &HashMap::new(), None, &[], None, None, 100);
    assert_eq!(
        graph.item_horizontal("REQ-002"),
        Some(CoverageStatus::Uncovered),
        "precondition: REQ-002 is genuinely unverified"
    );
    assert!(
        candidate_items(&actions, NextActionKind::WriteVerification).is_empty(),
        "REQ-002 has nothing to scaffold from: {actions:?}"
    );
}

#[test]
fn relink_candidate_skips_a_verifier_whose_level_is_below_the_deeper_item() {
    // system_test (level 2) may verify basic_spec (2) but not detailed_spec
    // (3): relinking ST-001 to DS-001 would itself be an invalid_link.
    let mut inp = input(vec![
        item("SPEC-001", "basic_spec", &[], &[]),
        item("DS-001", "detailed_spec", &["SPEC-001"], &[]),
        item("ST-001", "system_test", &[], &["SPEC-001"]),
    ]);
    inp.configured_layers = all_layers();
    let graph = TraceGraph::build(&inp);
    let (actions, _) =
        derive_next_actions(&graph, &inp, &HashMap::new(), None, &[], None, None, 100);
    assert!(
        candidate_items(&actions, NextActionKind::RelinkCandidate).is_empty(),
        "{actions:?}"
    );
}

#[test]
fn relink_candidate_picks_a_deeper_item_the_verifier_may_legally_verify() {
    // Two deeper refiners exist; the verifier's level allows detailed_spec.
    let mut inp = input(vec![
        item("SPEC-001", "basic_spec", &[], &[]),
        item("DS-002", "detailed_spec", &["SPEC-001"], &[]),
        item("DS-001", "detailed_spec", &["SPEC-001"], &[]),
        item("UT-001", "unit_test", &[], &["SPEC-001#AC1", "SPEC-009"]),
    ]);
    inp.configured_layers = all_layers();
    let graph = TraceGraph::build(&inp);
    let (actions, _) =
        derive_next_actions(&graph, &inp, &HashMap::new(), None, &[], None, None, 100);
    let a = actions
        .iter()
        .find(|a| a.kind == NextActionKind::RelinkCandidate)
        .expect("relink candidate");
    // Only the relinked target changes; the rest of `verifies` is preserved.
    assert_eq!(
        a.suggest.arguments["ops"][0]["attrs"]["verifies"],
        serde_json::json!(["DS-001", "SPEC-009"])
    );
}

#[test]
fn relink_candidate_for_a_task_preserves_the_tasks_other_links() {
    let mut inp = input(vec![
        item("SPEC-001", "basic_spec", &[], &[]),
        item("DS-001", "detailed_spec", &["SPEC-001"], &[]),
        item("REQ-009", "requirement", &[], &[]),
    ]);
    inp.configured_layers = all_layers();
    for (id, role) in [
        ("SPEC-001", TaskLinkRole::Implements),
        ("REQ-009", TaskLinkRole::Executes),
    ] {
        inp.task_requirement_links.push(TaskRequirementLink {
            task_id: "t-1".to_string(),
            stable_id: id.to_string(),
            role,
            baseline_hash: None,
        });
    }
    let graph = TraceGraph::build(&inp);
    let (actions, _) =
        derive_next_actions(&graph, &inp, &HashMap::new(), None, &[], None, None, 100);
    let a = actions
        .iter()
        .find(|a| a.kind == NextActionKind::RelinkCandidate && a.task.as_deref() == Some("t-1"))
        .expect("task relink candidate");
    assert_suggest_matches_schema(a);
    // requirement_ids REPLACES the task's whole link set, so the unrelated
    // REQ-009 link must be carried over, not dropped.
    let task = &a.suggest.arguments["task"];
    assert_eq!(
        task["requirement_ids"],
        serde_json::json!(["DS-001", "REQ-009"])
    );
    assert_eq!(task["requirement_roles"]["DS-001"], "implements");
    assert_eq!(task["requirement_roles"]["REQ-009"], "executes");
}

#[test]
fn create_task_is_skipped_when_the_item_is_vertically_covered_by_a_tasked_child() {
    let mut inp = input(vec![
        item("REQ-001", "requirement", &[], &[]),
        item("SPEC-001", "basic_spec", &["REQ-001"], &[]),
    ]);
    inp.configured_layers = vec!["requirement".to_string(), "basic_spec".to_string()];
    inp.task_requirement_links.push(TaskRequirementLink {
        task_id: "t-1".to_string(),
        stable_id: "SPEC-001".to_string(),
        role: TaskLinkRole::Implements,
        baseline_hash: None,
    });
    inp.task_status
        .insert("t-1".to_string(), "in_progress".to_string());
    let graph = TraceGraph::build(&inp);
    assert_eq!(
        graph.item_vertical("REQ-001"),
        Some(CoverageStatus::Covered),
        "precondition"
    );
    let (actions, _) =
        derive_next_actions(&graph, &inp, &HashMap::new(), None, &[], None, None, 100);
    assert!(
        candidate_items(&actions, NextActionKind::CreateTask).is_empty(),
        "{actions:?}"
    );
}

#[test]
fn create_task_still_fires_when_the_childs_vertical_is_partial() {
    // The only task is still `todo` -> SPEC-001 and REQ-001 are partial.
    let mut inp = input(vec![
        item("REQ-001", "requirement", &[], &[]),
        item("SPEC-001", "basic_spec", &["REQ-001"], &[]),
        item("SPEC-002", "basic_spec", &["REQ-001"], &[]),
    ]);
    inp.configured_layers = vec!["requirement".to_string(), "basic_spec".to_string()];
    inp.task_requirement_links.push(TaskRequirementLink {
        task_id: "t-1".to_string(),
        stable_id: "SPEC-001".to_string(),
        role: TaskLinkRole::Implements,
        baseline_hash: None,
    });
    inp.task_status
        .insert("t-1".to_string(), "todo".to_string());
    let graph = TraceGraph::build(&inp);
    assert_eq!(
        graph.item_vertical("REQ-001"),
        Some(CoverageStatus::Partial),
        "precondition"
    );
    let (actions, _) =
        derive_next_actions(&graph, &inp, &HashMap::new(), None, &[], None, None, 100);
    let items = candidate_items(&actions, NextActionKind::CreateTask);
    assert!(items.contains(&"REQ-001".to_string()), "{items:?}");
    // SPEC-001 already has its (todo) implementing task.
    assert!(!items.contains(&"SPEC-001".to_string()), "{items:?}");
}

#[test]
fn trace_next_and_test_run_kind_enums_cover_every_kind() {
    let defs = crate::mcp::tools::all_tool_definitions();
    let enum_of = |tool: &str, pointer: &str| -> Vec<String> {
        let d = defs.iter().find(|t| t.name == tool).unwrap();
        d.input_schema
            .pointer(pointer)
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("{tool} {pointer}"))
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    let next = enum_of("handoff_trace_next", "/properties/kinds/items/enum");
    let run = enum_of(
        "handoff_trace_test_run",
        "/properties/scope/properties/kinds/items/enum",
    );
    for kind in ALL_KINDS {
        let name = serde_json::to_value(kind).unwrap();
        let name = name.as_str().unwrap();
        assert!(
            next.iter().any(|k| k == name),
            "trace_next enum lacks {name}"
        );
        assert!(
            run.iter().any(|k| k == name),
            "trace_test_run enum lacks {name}"
        );
    }
    for d in defs
        .iter()
        .filter(|t| t.name == "handoff_trace_next" || t.name == "handoff_trace_test_run")
    {
        assert!(
            d.description.contains("relink_candidate"),
            "{} description",
            d.name
        );
    }
}
