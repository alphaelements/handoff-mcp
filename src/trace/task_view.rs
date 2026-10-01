//! Pure derivation of `tasks[]` (wiki/260-vmodel-m2-design.md §3.4/§5.1,
//! M2-07, FR-602/603, t360.20.25): for every task with at least one
//! `requirement`-type link, its linked items grouped by `{layer, role}`
//! (FR-602's `list_tasks(layer=…)`/`_trace_report.json`'s per-task layer
//! slice), and a tally of "blockers" (FR-603) — the reasons a task's
//! requirement links are not yet safe to consider done.
//!
//! This is deliberately a post-processing pass over an already-built
//! [`super::engine::TraceGraph`] (wiki/240-performance-design.md §5-5: one
//! graph build per request) rather than a second traversal of raw storage —
//! §3.4 notes the underlying computation ("逆 verifies 索引"/"run の最新結果")
//! can be done without a full graph in the hot `update_task` done-guard path,
//! but `_trace_report.json`'s `tasks[]` is built from the `trace_report`
//! graph that request already pays for regardless, so reusing
//! [`super::engine::TraceGraph`]'s accessors here (`verified_by`, `state`,
//! `reverify_items`, `suspects`) is both simpler and free of extra I/O.

use std::collections::{BTreeMap, HashSet};

use super::engine::TraceGraph;
use super::types::{ItemState, SuspectKind, TaskLinkRole, TraceInput};

/// One `{layer, role, count}` entry (wiki/260 §5.1's `tasks[].layers`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TaskLayerCount {
    pub layer: String,
    pub role: &'static str,
    pub count: usize,
}

/// `tasks[].blockers` (wiki/260 §3.4/§5.1) — a tally, not a list: how many of
/// this task's linked items are each kind of blocker. Not copied onto
/// `done_criteria` (§3.4: "done_criteria にはコピーしない") — this is a
/// read-only view computed fresh on every `trace_report`/`trace_slice` call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct TaskBlockerCounts {
    pub not_run: usize,
    pub failing: usize,
    pub blocked: usize,
    pub reverify: usize,
    pub suspect: usize,
}

/// One task's full trace view (wiki/260 §5.1's `tasks[]` entry).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TaskTraceView {
    pub task_id: String,
    pub layers: Vec<TaskLayerCount>,
    pub blockers: TaskBlockerCounts,
}

fn role_str(role: TaskLinkRole) -> &'static str {
    match role {
        TaskLinkRole::Implements => "implements",
        TaskLinkRole::Executes => "executes",
    }
}

/// Folds one resolved state into `blockers`'s not_run/failing/blocked tally
/// (§3.4) — shared by [`tally_item_state`] (a verifier's/executor's
/// aggregated [`TraceGraph::state`]) and the implements branch's own-run fold
/// (an inline item's [`TraceGraph::own_state`], which `state` does not
/// substitute for since it also mixes in coverage/child contributions).
fn fold_state(state: Option<ItemState>, blockers: &mut TaskBlockerCounts) {
    match state {
        Some(ItemState::NotRun) => blockers.not_run += 1,
        Some(ItemState::Failing) => blockers.failing += 1,
        Some(ItemState::Blocked) => blockers.blocked += 1,
        _ => {}
    }
}

/// Folds `id`'s own state/reverify status into `blockers` (§3.4's "not_run /
/// failing / blocked のもの、および reverify のもの" — for an `executes`
/// link this is the verification item itself; for an `implements` link this
/// is called once per direct verifier of the implemented item).
fn tally_item_state(graph: &TraceGraph, id: &str, blockers: &mut TaskBlockerCounts) {
    fold_state(graph.state(id), blockers);
    if graph.reverify_items().contains(id) {
        blockers.reverify += 1;
    }
}

/// wiki/260 §3.4/§5.1 (M2-07/t360.20.25): one [`TaskTraceView`] per task that
/// has at least one `requirement`-type link, sorted by task id (NFR-004
/// determinism — `TraceInput::task_requirement_links`' own order ultimately
/// traces back to filesystem `read_dir` order, same reasoning as
/// `handlers/trace.rs`'s `tasks_by_item`).
pub fn compute_task_views(input: &TraceInput, graph: &TraceGraph) -> Vec<TaskTraceView> {
    let item_layer: std::collections::HashMap<&str, &str> = input
        .items
        .iter()
        .filter_map(|i| i.layer.as_deref().map(|l| (i.stable_id.as_str(), l)))
        .collect();

    // Every `{task_id, item}` pair that is itself a `task`-kind suspect
    // (§3.2) — precomputed once so the per-link loop below is O(1) per
    // lookup instead of re-scanning `graph.suspects()` for every link.
    let task_suspect_pairs: HashSet<(&str, &str)> = graph
        .suspects()
        .iter()
        .filter(|s| s.kind == SuspectKind::Task)
        .filter_map(|s| s.task.as_deref().map(|t| (t, s.item.as_str())))
        .collect();

    let mut by_task: BTreeMap<&str, Vec<(&str, TaskLinkRole)>> = BTreeMap::new();
    for link in &input.task_requirement_links {
        by_task
            .entry(link.task_id.as_str())
            .or_default()
            .push((link.stable_id.as_str(), link.role));
    }

    by_task
        .into_iter()
        .map(|(task_id, links)| {
            let mut layer_counts: BTreeMap<(String, &'static str), usize> = BTreeMap::new();
            let mut blockers = TaskBlockerCounts::default();

            for (item, role) in &links {
                let role = *role;
                if let Some(layer) = item_layer.get(item) {
                    *layer_counts
                        .entry((layer.to_string(), role_str(role)))
                        .or_insert(0) += 1;
                }
                match role {
                    TaskLinkRole::Implements => {
                        // §3.4: "implements リンクなら、その項目を直接検証する
                        // 項目（インライン・暗黙 AC を含む）のうち not_run /
                        // failing / blocked のもの、および reverify のもの"
                        for verifier in graph.verified_by(item) {
                            tally_item_state(graph, verifier, &mut blockers);
                        }
                        // An inline-verified left item (`- method:` /
                        // test_refs) verifies itself via its own run, which
                        // never appears in `verified_by` (that only holds
                        // *other* items' `verifies` edges onto it) — fold its
                        // own run and reverify membership in separately
                        // (rework round 1, MAJOR).
                        if graph.is_inline(item) {
                            fold_state(graph.own_state(item), &mut blockers);
                            if graph.reverify_items().contains(*item) {
                                blockers.reverify += 1;
                            }
                        }
                    }
                    TaskLinkRole::Executes => {
                        // §3.4: "executes リンクなら、その検証項目自身の結果が
                        // pass 以外か reverify のもの"
                        tally_item_state(graph, item, &mut blockers);
                    }
                }
                // §3.4: "どちらも、task suspect（そのタスクのリンク自身）を
                // 含む"
                if task_suspect_pairs.contains(&(task_id, *item)) {
                    blockers.suspect += 1;
                }
            }

            let layers = layer_counts
                .into_iter()
                .map(|((layer, role), count)| TaskLayerCount { layer, role, count })
                .collect();

            TaskTraceView {
                task_id: task_id.to_string(),
                layers,
                blockers,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::docs::layer::LayerRegistry;
    use crate::trace::types::{TaskRequirementLink, TraceItemInput};

    fn item(id: &str, layer: &str) -> TraceItemInput {
        TraceItemInput {
            stable_id: id.to_string(),
            doc_id: "doc-1".to_string(),
            layer: Some(layer.to_string()),
            refines: Vec::new(),
            verifies: Vec::new(),
            method: None,
            has_test_refs: false,
            acceptance_labels: Vec::new(),
            derived: false,
            waived_axes: Vec::new(),
            def_hash: None,
            body_hash: None,
            link_baselines: Default::default(),
        }
    }

    fn base_input() -> TraceInput {
        TraceInput {
            layer_registry: LayerRegistry::build(&[]).all().to_vec(),
            ..Default::default()
        }
    }

    #[test]
    fn implements_task_is_blocked_by_its_items_not_run_verifier() {
        let mut input = base_input();
        input.items = vec![item("REQ-001", "requirement"), item("AT-001", "acceptance")];
        let mut verifies_item = input.items[1].clone();
        verifies_item.verifies = vec!["REQ-001".to_string()];
        input.items[1] = verifies_item;
        input.task_requirement_links = vec![TaskRequirementLink {
            task_id: "t1".to_string(),
            stable_id: "REQ-001".to_string(),
            role: TaskLinkRole::Implements,
            baseline_hash: None,
        }];

        let graph = TraceGraph::build(&input);
        let views = compute_task_views(&input, &graph);

        assert_eq!(views.len(), 1);
        let v = &views[0];
        assert_eq!(v.task_id, "t1");
        assert_eq!(
            v.layers,
            vec![TaskLayerCount {
                layer: "requirement".to_string(),
                role: "implements",
                count: 1,
            }]
        );
        assert_eq!(v.blockers.not_run, 1);
        assert_eq!(v.blockers.failing, 0);
    }

    #[test]
    fn implements_task_is_blocked_by_its_own_inline_verification() {
        // wiki/260 §3.4: "implements リンクなら、その項目を直接検証する項目
        // （インライン・暗黙 AC を含む）のうち not_run / failing / blocked の
        // もの" — an inline-verified left item (`- method:` or test_refs)
        // verifies itself via its own run, which never appears in
        // `graph.verified_by(item)`. A task `implements`-linking such an
        // item must still see that item's own failing run as a blocker.
        let mut input = base_input();
        let mut req = item("REQ-001", "requirement");
        req.method = Some("manual".to_string());
        input.items = vec![req];
        input
            .runs_latest
            .insert("REQ-001".to_string(), "fail".to_string());
        input.task_requirement_links = vec![TaskRequirementLink {
            task_id: "t1".to_string(),
            stable_id: "REQ-001".to_string(),
            role: TaskLinkRole::Implements,
            baseline_hash: None,
        }];

        let graph = TraceGraph::build(&input);
        let views = compute_task_views(&input, &graph);

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].blockers.failing, 1);
        assert_eq!(views[0].blockers.not_run, 0);
    }

    #[test]
    fn tasks_with_no_requirement_links_are_absent() {
        let input = base_input();
        let graph = TraceGraph::build(&input);
        assert!(compute_task_views(&input, &graph).is_empty());
    }

    #[test]
    fn executes_task_blocker_reflects_its_own_items_state() {
        let mut input = base_input();
        input.items = vec![item("REQ-001", "requirement"), item("AT-001", "acceptance")];
        let mut verifies_item = input.items[1].clone();
        verifies_item.verifies = vec!["REQ-001".to_string()];
        input.items[1] = verifies_item;
        input
            .runs_latest
            .insert("AT-001".to_string(), "fail".to_string());
        input.task_requirement_links = vec![TaskRequirementLink {
            task_id: "t2".to_string(),
            stable_id: "AT-001".to_string(),
            role: TaskLinkRole::Executes,
            baseline_hash: None,
        }];

        let graph = TraceGraph::build(&input);
        let views = compute_task_views(&input, &graph);

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].blockers.failing, 1);
        assert_eq!(views[0].blockers.not_run, 0);
    }

    #[test]
    fn tasks_are_sorted_by_id() {
        let mut input = base_input();
        input.items = vec![item("REQ-001", "requirement")];
        input.task_requirement_links = vec![
            TaskRequirementLink {
                task_id: "t9".to_string(),
                stable_id: "REQ-001".to_string(),
                role: TaskLinkRole::Implements,
                baseline_hash: None,
            },
            TaskRequirementLink {
                task_id: "t2".to_string(),
                stable_id: "REQ-001".to_string(),
                role: TaskLinkRole::Implements,
                baseline_hash: None,
            },
        ];
        let graph = TraceGraph::build(&input);
        let views = compute_task_views(&input, &graph);
        assert_eq!(
            views.iter().map(|v| v.task_id.as_str()).collect::<Vec<_>>(),
            vec!["t2", "t9"]
        );
    }
}
