//! t-vfix.5: how `derive_next_actions` treats implicit acceptance items
//! (`REQ-001#AC1`), task scoping, and what it suggests under a profile that
//! has no deeper left layer or for a manual criterion.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::*;
use crate::storage::docs::layer::LayerRegistry;
use crate::trace::types::{TaskLinkRole, TaskRequirementLink, TraceInput, TraceItemInput};

fn item(id: &str, layer: &str) -> TraceItemInput {
    TraceItemInput {
        stable_id: id.to_string(),
        doc_id: "doc".to_string(),
        layer: Some(layer.to_string()),
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

/// `REQ-001` (one acceptance criterion) plus its implicit item.
fn req_with_implicit(req: &str) -> Vec<TraceItemInput> {
    let mut parent = item(req, "requirement");
    parent.acceptance_labels = vec!["AC1".to_string()];
    let implicit_id = format!("{req}#AC1");
    let mut implicit = item(&implicit_id, "acceptance");
    implicit.verifies = vec![implicit_id.clone()];
    implicit.implicit_of = Some(req.to_string());
    vec![parent, implicit]
}

fn input(items: Vec<TraceItemInput>) -> TraceInput {
    TraceInput {
        items,
        layer_registry: LayerRegistry::build(&[]).all().to_vec(),
        ..Default::default()
    }
}

fn implemented(ids: &[&str]) -> HashMap<String, ItemNextMeta> {
    ids.iter()
        .map(|id| {
            (
                id.to_string(),
                ItemNextMeta {
                    dev_stage: Some("implemented".to_string()),
                    ..Default::default()
                },
            )
        })
        .collect()
}

fn scope(ids: &[&str]) -> HashSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

#[test]
fn task_scope_includes_the_implicit_items_of_the_items_it_implements() {
    let mut items = req_with_implicit("REQ-001");
    items.extend(req_with_implicit("REQ-002"));
    let mut inp = input(items);
    inp.task_requirement_links = vec![TaskRequirementLink {
        task_id: "t1".to_string(),
        stable_id: "REQ-001".to_string(),
        role: TaskLinkRole::Implements,
        baseline_hash: None,
    }];
    let graph = TraceGraph::build(&inp);
    let meta = implemented(&["REQ-001", "REQ-002"]);

    let (all, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, None, 100);
    let reruns = |actions: &[NextAction]| -> Vec<String> {
        actions
            .iter()
            .filter(|a| a.kind == NextActionKind::Rerun)
            .filter_map(|a| a.item.clone())
            .collect()
    };
    assert_eq!(reruns(&all), vec!["REQ-001#AC1", "REQ-002#AC1"]);

    let scoped = scope(&["REQ-001"]);
    let (task, _) = derive_next_actions(&graph, &inp, &meta, Some(&scoped), &[], None, None, 100);
    assert_eq!(
        reruns(&task),
        vec!["REQ-001#AC1"],
        "the task's own implicit verification is in scope, another item's is not: {task:?}"
    );
}

#[test]
fn manual_criteria_are_recorded_not_ingested() {
    // An implicit acceptance item and an explicit `method: manual` item are
    // executed by hand; only an item that is not run by hand has test output
    // for `trace_ingest` to match.
    let mut items = req_with_implicit("REQ-001");
    let mut manual = item("AT-002", "acceptance");
    manual.method = Some("manual".to_string());
    manual.verifies = vec!["REQ-001".to_string()];
    let mut auto = item("AT-003", "acceptance");
    auto.method = Some("auto".to_string());
    auto.has_test_refs = true;
    auto.verifies = vec!["REQ-001".to_string()];
    items.extend([manual, auto]);
    let inp = input(items);
    let graph = TraceGraph::build(&inp);
    let meta = implemented(&["REQ-001"]);

    let (actions, _) = derive_next_actions(&graph, &inp, &meta, None, &[], None, None, 100);
    let tool_for = |id: &str| -> String {
        actions
            .iter()
            .find(|a| a.kind == NextActionKind::Rerun && a.item.as_deref() == Some(id))
            .unwrap_or_else(|| panic!("no rerun for {id}: {actions:?}"))
            .suggest
            .tool
            .clone()
    };
    assert_eq!(tool_for("REQ-001#AC1"), "handoff_trace_record");
    assert_eq!(tool_for("AT-002"), "handoff_trace_record");
    assert_eq!(tool_for("AT-003"), "handoff_trace_ingest");

    let record = actions
        .iter()
        .find(|a| a.item.as_deref() == Some("AT-002") && a.kind == NextActionKind::Rerun)
        .unwrap();
    assert_eq!(record.suggest.arguments["results"][0]["item"], "AT-002");
}

#[test]
fn refine_is_not_suggested_when_the_profile_has_no_deeper_left_layer() {
    // `minimal`: requirement + acceptance only. There is no layer to refine
    // into, so the gap is closed by a task (`create_task`), never by `refine`.
    let mut inp = input(vec![item("REQ-001", "requirement")]);
    inp.profile_layers = vec!["requirement".to_string(), "acceptance".to_string()];
    let graph = TraceGraph::build(&inp);

    let (actions, _) =
        derive_next_actions(&graph, &inp, &HashMap::new(), None, &[], None, None, 100);
    assert!(
        actions.iter().all(|a| a.kind != NextActionKind::Refine),
        "{actions:?}"
    );
    assert!(
        actions
            .iter()
            .any(|a| a.kind == NextActionKind::CreateTask && a.item.as_deref() == Some("REQ-001")),
        "{actions:?}"
    );
}

#[test]
fn refine_is_still_suggested_when_the_profile_includes_a_deeper_left_layer() {
    let mut inp = input(vec![item("REQ-001", "requirement")]);
    inp.profile_layers = vec![
        "requirement".to_string(),
        "basic_spec".to_string(),
        "acceptance".to_string(),
    ];
    let graph = TraceGraph::build(&inp);

    let (actions, _) =
        derive_next_actions(&graph, &inp, &HashMap::new(), None, &[], None, None, 100);
    assert!(
        actions
            .iter()
            .any(|a| a.kind == NextActionKind::Refine && a.item.as_deref() == Some("REQ-001")),
        "{actions:?}"
    );
}
