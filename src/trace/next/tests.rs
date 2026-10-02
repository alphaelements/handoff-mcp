//! Unit tests for [`super::derive_next_actions`] (wiki/260-vmodel-m2-design.md
//! §3.5, M2-10): one test per kind's detection rule, plus the deterministic
//! cross-kind/priority/layer-level/id ordering §3.5's closing sentence
//! specifies.

use std::collections::{BTreeMap, HashMap, HashSet};

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

    let (actions, truncated) = derive_next_actions(&graph, &inp, &meta, None, &[], None, 10);

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

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, 10);
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

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, 10);
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

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, 10);
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

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, 10);
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

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, 10);
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

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, 10);
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

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, 10);
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

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, 10);
    assert!(actions.iter().all(|a| a.kind != NextActionKind::CreateTask));
}

#[test]
fn fix_link_fires_for_a_dangling_reference() {
    let inp = input(vec![item("SPEC-001", "basic_spec", &["REQ-999"], &[])]);
    let graph = TraceGraph::build(&inp);
    let meta = HashMap::new();

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, 10);
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

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, 10);
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

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, 10);
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

    let (actions, truncated) = derive_next_actions(&graph, &inp, &meta, None, &[], None, 2);
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

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, Some(&scope), &[], None, 10);
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
        10,
    );
    assert!(actions.iter().all(|a| a.item.as_deref() != Some("REQ-001")));
}
