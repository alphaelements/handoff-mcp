//! t-layergen.9 (wiki/430 §3.7, REQ-LAG-006): `derive_next_actions` proposes
//! `update_drifted_spec` / `rerun` follow-ups for items in
//! `TraceInput::code_drift_items`, and carries `plan_update_needed` when a
//! plan item is linked to the drifted item. Proposal only — nothing is created.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::*;
use crate::storage::docs::layer::LayerRegistry;
use crate::trace::types::{TraceInput, TraceItemInput};

fn item(id: &str, layer: &str, verifies: &[&str]) -> TraceItemInput {
    TraceItemInput {
        stable_id: id.to_string(),
        doc_id: format!("doc-{layer}"),
        layer: Some(layer.to_string()),
        refines: Vec::new(),
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

fn input(items: Vec<TraceItemInput>, drift: &[&str]) -> TraceInput {
    TraceInput {
        items,
        layer_registry: LayerRegistry::build(&[]).all().to_vec(),
        code_drift_items: drift.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

fn run(
    inp: &TraceInput,
    meta: &HashMap<String, ItemNextMeta>,
    kinds: Option<&HashSet<NextActionKind>>,
) -> Vec<NextAction> {
    let graph = TraceGraph::build(inp);
    derive_next_actions(&graph, inp, meta, None, &[], None, kinds, 100).0
}

fn of_kind(actions: &[NextAction], kind: NextActionKind) -> Vec<&NextAction> {
    actions.iter().filter(|a| a.kind == kind).collect()
}

fn spec_and_verifier() -> Vec<TraceItemInput> {
    vec![
        item("REQ-001", "requirement", &[]),
        item("AT-001", "acceptance", &["REQ-001"]),
    ]
}

#[test]
fn drifted_left_item_gets_an_update_drifted_spec_action() {
    let inp = input(spec_and_verifier(), &["REQ-001"]);
    let actions = run(&inp, &HashMap::new(), None);

    let drift = of_kind(&actions, NextActionKind::UpdateDriftedSpec);
    assert_eq!(drift.len(), 1);
    assert_eq!(drift[0].item.as_deref(), Some("REQ-001"));
    assert_eq!(drift[0].rank, 3);
    assert_eq!(drift[0].suggest.tool, "handoff_trace_update");
    assert_eq!(drift[0].suggest.arguments["dry_run"], true);
    let op = &drift[0].suggest.arguments["ops"][0];
    assert_eq!(op["op"], "upsert_item");
    assert_eq!(op["doc"], "doc-requirement");
    assert_eq!(op["id"], "REQ-001");
    assert!(drift[0].reason.contains("REQ-001"));
    assert!(drift[0].plan_update_needed.is_empty());
}

#[test]
fn no_drift_means_no_drift_actions() {
    let inp = input(spec_and_verifier(), &[]);
    let actions = run(&inp, &HashMap::new(), None);
    assert!(of_kind(&actions, NextActionKind::UpdateDriftedSpec).is_empty());
    assert!(actions
        .iter()
        .all(|a| !a.reason.to_lowercase().contains("drift")));
}

#[test]
fn drift_of_an_unknown_id_is_ignored() {
    let inp = input(spec_and_verifier(), &["GHOST-1"]);
    let actions = run(&inp, &HashMap::new(), None);
    assert!(of_kind(&actions, NextActionKind::UpdateDriftedSpec).is_empty());
}

#[test]
fn right_side_verifier_of_a_drifted_spec_is_proposed_for_rerun() {
    let inp = input(spec_and_verifier(), &["REQ-001"]);
    let actions = run(&inp, &HashMap::new(), None);

    let reruns = of_kind(&actions, NextActionKind::Rerun);
    assert_eq!(reruns.len(), 1);
    assert_eq!(reruns[0].item.as_deref(), Some("AT-001"));
    assert!(reruns[0].reason.contains("REQ-001"));
    assert_eq!(reruns[0].suggest.tool, "handoff_trace_ingest");
    // update_drifted_spec sorts before rerun within the shared rank 3.
    let kinds: Vec<_> = actions.iter().map(|a| a.kind).collect();
    let spec_pos = kinds
        .iter()
        .position(|k| *k == NextActionKind::UpdateDriftedSpec)
        .unwrap();
    let rerun_pos = kinds
        .iter()
        .position(|k| *k == NextActionKind::Rerun)
        .unwrap();
    assert!(spec_pos < rerun_pos);
}

#[test]
fn a_drifted_verifier_is_proposed_for_rerun_not_spec_update() {
    let inp = input(spec_and_verifier(), &["AT-001"]);
    let actions = run(&inp, &HashMap::new(), None);

    assert!(of_kind(&actions, NextActionKind::UpdateDriftedSpec).is_empty());
    let reruns = of_kind(&actions, NextActionKind::Rerun);
    assert_eq!(reruns.len(), 1);
    assert_eq!(reruns[0].item.as_deref(), Some("AT-001"));
}

#[test]
fn rerun_is_not_duplicated_when_the_verifier_already_needs_one() {
    // AT-001 is `not_run`, drifted REQ-001 is implemented: the ordinary
    // rerun rule already fires for it, so drift must not add a second one.
    let mut inp = input(spec_and_verifier(), &["REQ-001"]);
    inp.runs_latest
        .insert("AT-001".to_string(), "not_run".to_string());
    let meta: HashMap<String, ItemNextMeta> = [(
        "REQ-001".to_string(),
        ItemNextMeta {
            dev_stage: Some("implemented".to_string()),
            ..Default::default()
        },
    )]
    .into();
    let actions = run(&inp, &meta, None);
    assert_eq!(of_kind(&actions, NextActionKind::Rerun).len(), 1);
}

#[test]
fn one_verifier_of_two_drifted_specs_is_proposed_once() {
    let inp = input(
        vec![
            item("REQ-001", "requirement", &[]),
            item("REQ-002", "requirement", &[]),
            item("AT-001", "acceptance", &["REQ-001", "REQ-002"]),
        ],
        &["REQ-001", "REQ-002"],
    );
    let actions = run(&inp, &HashMap::new(), None);
    assert_eq!(
        of_kind(&actions, NextActionKind::UpdateDriftedSpec).len(),
        2
    );
    assert_eq!(of_kind(&actions, NextActionKind::Rerun).len(), 1);
}

#[test]
fn kinds_filter_can_select_only_update_drifted_spec() {
    let inp = input(spec_and_verifier(), &["REQ-001"]);
    let kinds: HashSet<NextActionKind> = [NextActionKind::UpdateDriftedSpec].into();
    let actions = run(&inp, &HashMap::new(), Some(&kinds));
    assert!(!actions.is_empty());
    assert!(actions
        .iter()
        .all(|a| a.kind == NextActionKind::UpdateDriftedSpec));
}

#[test]
fn task_scope_limits_drift_actions_to_the_tasks_items() {
    let inp = input(
        vec![
            item("REQ-001", "requirement", &[]),
            item("REQ-002", "requirement", &[]),
        ],
        &["REQ-001", "REQ-002"],
    );
    let graph = TraceGraph::build(&inp);
    let scope: HashSet<String> = ["REQ-002".to_string()].into();
    let (actions, _) = derive_next_actions(
        &graph,
        &inp,
        &HashMap::new(),
        Some(&scope),
        &[],
        None,
        None,
        100,
    );
    let drift = of_kind(&actions, NextActionKind::UpdateDriftedSpec);
    assert_eq!(drift.len(), 1);
    assert_eq!(drift[0].item.as_deref(), Some("REQ-002"));
}

#[test]
fn plan_update_needed_is_carried_on_the_drift_action() {
    let inp = input(spec_and_verifier(), &["REQ-001"]);
    let meta: HashMap<String, ItemNextMeta> = [(
        "REQ-001".to_string(),
        ItemNextMeta {
            plan_updates: vec![PlanUpdateRef {
                doc_id: "plan-doc".to_string(),
                stable_id: "PLAN-001".to_string(),
                current_dev_stage: "tested".to_string(),
            }],
            ..Default::default()
        },
    )]
    .into();
    let actions = run(&inp, &meta, None);

    let drift = of_kind(&actions, NextActionKind::UpdateDriftedSpec);
    assert_eq!(drift[0].plan_update_needed.len(), 1);
    assert_eq!(drift[0].plan_update_needed[0].stable_id, "PLAN-001");

    let json = serde_json::to_value(drift[0]).unwrap();
    assert_eq!(json["plan_update_needed"][0]["doc_id"], "plan-doc");
    assert_eq!(json["plan_update_needed"][0]["current_dev_stage"], "tested");
    // Other actions do not serialize the field at all.
    let rerun_json = serde_json::to_value(of_kind(&actions, NextActionKind::Rerun)[0]).unwrap();
    assert!(rerun_json.get("plan_update_needed").is_none());
}

#[test]
fn plan_update_needed_is_not_attached_without_drift() {
    let inp = input(spec_and_verifier(), &[]);
    let meta: HashMap<String, ItemNextMeta> = [(
        "REQ-001".to_string(),
        ItemNextMeta {
            plan_updates: vec![PlanUpdateRef {
                doc_id: "plan-doc".to_string(),
                stable_id: "PLAN-001".to_string(),
                current_dev_stage: "tested".to_string(),
            }],
            ..Default::default()
        },
    )]
    .into();
    let actions = run(&inp, &meta, None);
    assert!(actions.iter().all(|a| a.plan_update_needed.is_empty()));
}
