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

use std::collections::{BTreeMap, HashMap, HashSet};

use super::adapter::collect_trace_items;
use super::engine::TraceGraph;
use super::profile::resolve_project_profile;
use super::types::{
    ItemState, RunResultHashes, SuspectKind, TaskLinkRole, TaskRequirementLink, TraceInput,
    TraceItemInput,
};
use crate::storage::config::TraceConfig;
use crate::storage::docs::layer::LayerRegistry;
use crate::storage::docs::model::DocMetadata;
use crate::storage::runs::LatestCache;
use crate::storage::tasks::TaskLink;

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
                        // failing / blocked のもの、および reverify のもの" —
                        // skip a verifier that is itself out of its effective
                        // scope (rework, t360.20.31), matching the state DP's
                        // own `Dp::in_scope` filter on `verified_by` so a
                        // verifier whose layer isn't actually in use can't be
                        // a blocker here either (it is `n/a`, not a silent
                        // pull-down).
                        for verifier in graph.verified_by(item) {
                            if !graph.in_scope(verifier) {
                                continue;
                            }
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

/// §3.4's lightweight done-guard / `list_tasks`/`get_task`/`task_checklist
/// (view)` computation (M2-13, FR-602/603/604): the same [`TaskTraceView`]
/// [`compute_task_views`] derives for `trace_report`'s project-wide graph,
/// but for exactly **one** task, without paying for that whole-project
/// graph.
///
/// The only thing this function does differently from `trace_report`'s own
/// call site (`src/mcp/handlers/trace.rs`) is *what* [`TraceInput`] it feeds
/// into the same, unmodified [`TraceGraph::build`] + [`compute_task_views`]:
///
/// - `items`: not the whole project's — just this task's own linked
///   stable_ids, plus every item elsewhere in the project whose `verifies`
///   references one of them, plus each such verifier's *other* `verifies`
///   targets (its `reverify` status depends on all of its links — review
///   round 1 fix) (found by the one unavoidable full pass over
///   `docs`, below — §3.4: "逆 verifies 索引...SubItem の走査 O(項目数)".
///   This pass is cheap: it only inspects already-parsed, already-in-memory
///   `SubItem`s (`docs` is a caller-supplied, already-loaded `DocSet`/slice —
///   no file I/O here), unlike `trace_report`'s own `TraceGraph::build`,
///   which additionally runs a per-item profile-tree walk, a memoized DP
///   over every left-side item, coverage aggregation, and suspect derivation
///   across the *entire* project's item set. Restricting the item set first
///   makes all of those the same `TraceGraph::build` call already does, but
///   over a handful of items instead of the whole project — not a
///   reimplementation of any of it.
/// - `task_requirement_links`: only this task's own — never
///   `load_trace_input`'s `collect_all_tasks`, which reads every *other*
///   task's file from disk just to learn its id (the dominant cost this
///   function exists to avoid, PR-1/PR-6).
/// - `runs_latest`/`runs_latest_hashes`: looked up directly from the
///   caller's already-loaded `runs::LatestCache` (E6's
///   `runs::load_latest_readonly`, never `runs::sync`) for just the items
///   found above — `HashMap::get` lookups, not a second file scan.
///
/// Known, documented scope gap: a document's `trace_profile` override
/// (§2.1 規則 1-4, M2-03) is not applied — `TraceInput::doc_profile_overrides`
/// is always empty here, so a verifier reachable only through an
/// *overridden* document's profile tree is evaluated against the project's
/// own default effective layers instead of that override (with no
/// overrides at all — the common case — `TraceGraph::build`'s own
/// `resolve_effective_layers` reduces to exactly this anyway, so this only
/// diverges from the full graph's answer for a project that actually
/// configures per-document `trace_profile`). `trace_report`/`trace_slice`
/// remain the authoritative full view for that case; this function backs
/// only the advisory/done-guard surface (§4.11), which already states the
/// same scope limit.
///
/// Returns `None` when `task_links` has no `requirement`-type link at all
/// (§3.4/§4.11: "requirement リンクのないタスクは何もしない", PR-2's "リンク
/// なし" fast path).
pub fn compute_task_blockers_for_task(
    docs: &[DocMetadata],
    layer_registry: &LayerRegistry,
    trace_config: &TraceConfig,
    runs_latest_cache: &LatestCache,
    task_id: &str,
    task_links: &[TaskLink],
) -> Option<TaskTraceView> {
    let requirement_links: Vec<&TaskLink> = task_links
        .iter()
        .filter(|l| l.link_type == "requirement" && l.label.is_some())
        .collect();
    if requirement_links.is_empty() {
        return None;
    }

    let all_items = collect_trace_items(docs);
    let mut by_id: HashMap<&str, &TraceItemInput> = HashMap::with_capacity(all_items.len());
    for item in &all_items {
        by_id.entry(item.stable_id.as_str()).or_insert(item);
    }

    let target_ids: HashSet<&str> = requirement_links
        .iter()
        .map(|l| l.label.as_deref().unwrap())
        .collect();

    // The one unavoidable full scan (§3.4): a `verifies` edge can originate
    // from any item in the project, so finding "who verifies my linked
    // item(s)" needs a pass over every item's `verifies` list — a plain
    // string compare per reference, no parsing/hashing/DP.
    let mut mini_ids: HashSet<&str> = target_ids.clone();
    let mut verifiers: Vec<&TraceItemInput> = Vec::new();
    for item in &all_items {
        for raw in &item.verifies {
            let base = match raw.split_once('#') {
                Some((b, ac)) if !ac.is_empty() => b,
                _ => raw.as_str(),
            };
            if target_ids.contains(base) {
                mini_ids.insert(item.stable_id.as_str());
                verifiers.push(item);
                break;
            }
        }
    }
    // A verifier's `reverify` status (§3.4's blocker) depends on *every*
    // one of its `verifies` links, not just the ones onto this task's own
    // items: a stale link to some other target makes it `reverify` too
    // (`suspect::compute`'s `verifies`-link suspects). Those other targets
    // (and any implicit `X#ACn` item an AC-level reference resolves
    // through) must be in the mini graph, or the link would look dangling
    // here and silently drop the suspect the full graph reports.
    for verifier in verifiers {
        for raw in &verifier.verifies {
            let base = raw.split_once('#').map_or(raw.as_str(), |(b, _)| b);
            for id in [base, raw.as_str()] {
                if let Some(target) = by_id.get(id) {
                    mini_ids.insert(target.stable_id.as_str());
                }
            }
        }
    }

    let mini_items: Vec<TraceItemInput> = mini_ids
        .iter()
        .filter_map(|id| by_id.get(id).map(|i| (*i).clone()))
        .collect();

    // Mirrors `adapter::collect_task_links`'s `"requirement"` branch,
    // restricted to this one task — see this function's own doc comment for
    // why re-deriving just this (instead of calling `collect_task_links`
    // over every task) is the point.
    let task_requirement_links: Vec<TaskRequirementLink> = requirement_links
        .iter()
        .map(|l| TaskRequirementLink {
            task_id: task_id.to_string(),
            stable_id: l.label.clone().unwrap(),
            role: match l.role.as_deref() {
                Some("executes") => TaskLinkRole::Executes,
                _ => TaskLinkRole::Implements,
            },
            baseline_hash: l.baseline_hash.clone(),
        })
        .collect();

    let mut runs_latest: HashMap<String, String> = HashMap::new();
    let mut runs_latest_hashes: HashMap<String, RunResultHashes> = HashMap::new();
    for id in &mini_ids {
        if let Some(latest) = runs_latest_cache.items.get(*id) {
            runs_latest.insert((*id).to_string(), latest.result.clone());
            runs_latest_hashes.insert(
                (*id).to_string(),
                RunResultHashes {
                    def_hash: latest.def_hash.clone(),
                    body_hash: latest.body_hash.clone(),
                },
            );
        }
    }

    let configured_layers =
        resolve_effective_in_use_layers(trace_config, layer_registry, &all_items);

    let trace_input = TraceInput {
        items: mini_items,
        task_requirement_links,
        runs_latest,
        runs_latest_hashes,
        layer_registry: layer_registry.all().to_vec(),
        configured_layers,
        ..Default::default()
    };

    let graph = TraceGraph::build(&trace_input);
    compute_task_views(&trace_input, &graph).into_iter().next()
}

/// The project's effective in-use layer set (wiki/260 §2.1's "layers ＞
/// profile ＞ auto"), resolved cheaply for [`compute_task_blockers_for_task`]
/// without [`super::engine::resolve_in_use_layers`] (private to `engine.rs`,
/// and bundled into the full `TraceGraph::build` this function is deliberately
/// not calling on the whole project): `[trace] layers` and project-default-
/// profile resolution are both pure functions over config (no item scan at
/// all); only the final "auto" fallback needs a scan, and `all_items` (every
/// item in the project) is exactly what `TraceGraph::build`'s own auto
/// detection would scan too — this mirrors that branch exactly (same
/// registry-order output).
fn resolve_effective_in_use_layers(
    trace_config: &TraceConfig,
    registry: &LayerRegistry,
    all_items: &[TraceItemInput],
) -> Vec<String> {
    if !trace_config.layers.is_empty() {
        return trace_config.layers.clone();
    }
    if let (Some(profile), _warnings) = resolve_project_profile(trace_config, registry) {
        if !profile.layers.is_empty() {
            return profile.layers;
        }
    }
    let present: HashSet<&str> = all_items
        .iter()
        .filter_map(|i| i.layer.as_deref())
        .collect();
    registry
        .all()
        .iter()
        .filter(|l| present.contains(l.id.as_str()))
        .map(|l| l.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
            needs: None,
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
    fn implements_task_ignores_a_verifier_outside_the_in_use_profile() {
        // t360.20.31: the state/vertical DP (`Dp::in_scope`, engine.rs)
        // skips a verifier whose own layer isn't in the effective (in-use)
        // layer set — it's `n/a`, not a silent pull-down. `tasks[].blockers`
        // must apply the identical filter instead of treating every
        // declared `verifies` edge as a blocker regardless of scope.
        let mut input = base_input();
        // `[trace] layers` configured to only "requirement"/"acceptance" —
        // "unit_test" (UT-001's own layer) is out of the effective scope.
        input.configured_layers = vec!["requirement".to_string(), "acceptance".to_string()];
        input.items = vec![item("REQ-001", "requirement"), item("UT-001", "unit_test")];
        let mut verifies_item = input.items[1].clone();
        verifies_item.verifies = vec!["REQ-001".to_string()];
        input.items[1] = verifies_item;
        input
            .runs_latest
            .insert("UT-001".to_string(), "fail".to_string());
        input.task_requirement_links = vec![TaskRequirementLink {
            task_id: "t1".to_string(),
            stable_id: "REQ-001".to_string(),
            role: TaskLinkRole::Implements,
            baseline_hash: None,
        }];

        let graph = TraceGraph::build(&input);
        assert!(!graph.in_scope("UT-001"));
        let views = compute_task_views(&input, &graph);

        assert_eq!(views.len(), 1);
        // The out-of-scope verifier's `fail` result must not be counted —
        // neither as `failing` nor (its absence) as `not_run`.
        assert_eq!(views[0].blockers.failing, 0);
        assert_eq!(views[0].blockers.not_run, 0);
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

/// M2-13 (wiki/260 §3.4/§4.11, t360.20.13): [`compute_task_blockers_for_task`]
/// must reach the exact same answer [`compute_task_views`] would over a
/// full project graph, for the one task under test — these tests build a
/// multi-document project (so the verifier lives in a *different* document
/// than the item it verifies, exercising the cross-document reverse-index
/// scan) and compare against what the full-graph path already validates.
#[cfg(test)]
mod lightweight_tests {
    use super::*;
    use crate::storage::docs::model::{DocMetadata, SubItem, Verification, VerificationItem};
    use crate::storage::runs::LatestItemResult;

    const TS: &str = "2026-01-01T00:00:00Z";

    fn doc_with_subs(doc_id: &str, layer: Option<&str>, subs: Vec<SubItem>) -> DocMetadata {
        let mut doc = DocMetadata::new(
            doc_id.to_string(),
            doc_id.to_string(),
            doc_id.to_string(),
            "spec".to_string(),
            TS.to_string(),
        );
        doc.layer = layer.map(str::to_string);
        doc.verification = Some(Verification {
            status: "pending".to_string(),
            created_at: TS.to_string(),
            updated_at: TS.to_string(),
            items: vec![VerificationItem {
                fragment_seq: Some(1),
                heading: "Section 1".to_string(),
                status: "pending".to_string(),
                impl_refs: Vec::new(),
                test_refs: Vec::new(),
                reviewer: None,
                verified_at: None,
                notes: String::new(),
                content_hash_at_verify: None,
                category: "section".to_string(),
                sub_items: subs,
                label: None,
            }],
        });
        doc
    }

    fn sub(stable_id: &str, layer: Option<&str>) -> SubItem {
        SubItem {
            stable_id: Some(stable_id.to_string()),
            layer: layer.map(str::to_string),
            ..Default::default()
        }
    }

    fn task_link(stable_id: &str, role: Option<&str>, baseline_hash: Option<&str>) -> TaskLink {
        TaskLink {
            target: stable_id.to_string(),
            link_type: "requirement".to_string(),
            label: Some(stable_id.to_string()),
            role: role.map(str::to_string),
            baseline_hash: baseline_hash.map(str::to_string),
        }
    }

    fn latest(result: &str) -> LatestItemResult {
        LatestItemResult {
            result: result.to_string(),
            executed_at: TS.to_string(),
            run_id: "run-1".to_string(),
            body_hash: None,
            def_hash: None,
            note: String::new(),
            evidence: Vec::new(),
            carried_from: None,
        }
    }

    #[test]
    fn returns_none_when_task_has_no_requirement_links() {
        let docs = vec![doc_with_subs(
            "doc-1",
            Some("requirement"),
            vec![sub("REQ-001", None)],
        )];
        let registry = LayerRegistry::build(&[]);
        let trace_config = TraceConfig::default();
        let cache = LatestCache::default();
        let links = vec![TaskLink {
            target: "some-doc".to_string(),
            link_type: "doc".to_string(),
            ..Default::default()
        }];
        assert!(compute_task_blockers_for_task(
            &docs,
            &registry,
            &trace_config,
            &cache,
            "t1",
            &links
        )
        .is_none());
    }

    #[test]
    fn implements_link_counts_a_not_run_cross_document_verifier() {
        // REQ-001 lives in doc "requirements"; its verifier AT-001 lives in
        // a *different* document "acceptance" — the cross-document scan
        // this lightweight path must still perform (§3.4).
        let mut verifier = sub("AT-001", Some("acceptance"));
        verifier.verifies = vec!["REQ-001".to_string()];
        let docs = vec![
            doc_with_subs(
                "requirements",
                Some("requirement"),
                vec![sub("REQ-001", None)],
            ),
            doc_with_subs("acceptance", Some("acceptance"), vec![verifier]),
        ];
        let registry = LayerRegistry::build(&[]);
        let trace_config = TraceConfig::default();
        let cache = LatestCache::default(); // AT-001 has no recorded run -> not_run.
        let links = vec![task_link("REQ-001", None, None)];

        let view =
            compute_task_blockers_for_task(&docs, &registry, &trace_config, &cache, "t1", &links)
                .expect("task has a requirement link");

        assert_eq!(view.task_id, "t1");
        assert_eq!(
            view.layers,
            vec![TaskLayerCount {
                layer: "requirement".to_string(),
                role: "implements",
                count: 1,
            }]
        );
        assert_eq!(view.blockers.not_run, 1);
        assert_eq!(view.blockers.failing, 0);
    }

    #[test]
    fn implements_link_counts_a_failing_verifier() {
        let mut verifier = sub("AT-001", Some("acceptance"));
        verifier.verifies = vec!["REQ-001".to_string()];
        let docs = vec![
            doc_with_subs(
                "requirements",
                Some("requirement"),
                vec![sub("REQ-001", None)],
            ),
            doc_with_subs("acceptance", Some("acceptance"), vec![verifier]),
        ];
        let registry = LayerRegistry::build(&[]);
        let trace_config = TraceConfig::default();
        let mut cache = LatestCache::default();
        cache.items.insert("AT-001".to_string(), latest("fail"));
        let links = vec![task_link("REQ-001", None, None)];

        let view =
            compute_task_blockers_for_task(&docs, &registry, &trace_config, &cache, "t1", &links)
                .unwrap();
        assert_eq!(view.blockers.failing, 1);
        assert_eq!(view.blockers.not_run, 0);
    }

    #[test]
    fn executes_link_reflects_the_linked_items_own_result() {
        let docs = vec![doc_with_subs(
            "acceptance",
            Some("acceptance"),
            vec![sub("AT-001", None)],
        )];
        let registry = LayerRegistry::build(&[]);
        let trace_config = TraceConfig::default();
        let mut cache = LatestCache::default();
        cache.items.insert("AT-001".to_string(), latest("fail"));
        let links = vec![task_link("AT-001", Some("executes"), None)];

        let view =
            compute_task_blockers_for_task(&docs, &registry, &trace_config, &cache, "t1", &links)
                .unwrap();
        assert_eq!(view.blockers.failing, 1);
    }

    #[test]
    fn task_suspect_counted_when_baseline_hash_mismatches_current_def_hash() {
        let mut req = sub("REQ-001", None);
        req.def_hash = Some("current-hash".to_string());
        let docs = vec![doc_with_subs(
            "requirements",
            Some("requirement"),
            vec![req],
        )];
        let registry = LayerRegistry::build(&[]);
        let trace_config = TraceConfig::default();
        let cache = LatestCache::default();
        let links = vec![task_link("REQ-001", None, Some("stale-hash"))];

        let view =
            compute_task_blockers_for_task(&docs, &registry, &trace_config, &cache, "t1", &links)
                .unwrap();
        assert_eq!(view.blockers.suspect, 1);
    }

    #[test]
    fn no_task_suspect_when_baseline_hash_matches_current_def_hash() {
        let mut req = sub("REQ-001", None);
        req.def_hash = Some("current-hash".to_string());
        let docs = vec![doc_with_subs(
            "requirements",
            Some("requirement"),
            vec![req],
        )];
        let registry = LayerRegistry::build(&[]);
        let trace_config = TraceConfig::default();
        let cache = LatestCache::default();
        let links = vec![task_link("REQ-001", None, Some("current-hash"))];

        let view =
            compute_task_blockers_for_task(&docs, &registry, &trace_config, &cache, "t1", &links)
                .unwrap();
        assert_eq!(view.blockers.suspect, 0);
    }

    /// The full-graph answer [`compute_task_blockers_for_task`] promises to
    /// reproduce: `compute_task_views` over a `TraceInput` built from *every*
    /// item in `docs` (what `trace_report` feeds it), restricted to `task_id`.
    fn full_graph_view(
        docs: &[DocMetadata],
        registry: &LayerRegistry,
        trace_config: &TraceConfig,
        cache: &LatestCache,
        task_id: &str,
        links: &[TaskLink],
    ) -> Option<TaskTraceView> {
        let items = collect_trace_items(docs);
        let configured_layers = resolve_effective_in_use_layers(trace_config, registry, &items);
        let input = TraceInput {
            task_requirement_links: links
                .iter()
                .filter(|l| l.link_type == "requirement")
                .map(|l| TaskRequirementLink {
                    task_id: task_id.to_string(),
                    stable_id: l.label.clone().unwrap(),
                    role: match l.role.as_deref() {
                        Some("executes") => TaskLinkRole::Executes,
                        _ => TaskLinkRole::Implements,
                    },
                    baseline_hash: l.baseline_hash.clone(),
                })
                .collect(),
            runs_latest: cache
                .items
                .iter()
                .map(|(k, v)| (k.clone(), v.result.clone()))
                .collect(),
            runs_latest_hashes: cache
                .items
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        RunResultHashes {
                            def_hash: v.def_hash.clone(),
                            body_hash: v.body_hash.clone(),
                        },
                    )
                })
                .collect(),
            items,
            layer_registry: registry.all().to_vec(),
            configured_layers,
            ..Default::default()
        };
        let graph = TraceGraph::build(&input);
        compute_task_views(&input, &graph)
            .into_iter()
            .find(|v| v.task_id == task_id)
    }

    #[test]
    fn verifier_reverify_via_a_stale_link_to_another_target_matches_the_full_graph() {
        // AT-001 verifies both REQ-001 (this task's link) and REQ-002 (not
        // linked to this task). Its `verifies` link to REQ-002 is stale
        // (baseline "old" vs REQ-002's current def_hash "new"), so the full
        // graph marks the passing AT-001 `reverify` — a blocker for any
        // task implementing REQ-001 (§3.4: direct verifiers that are
        // reverify). The lightweight path must not lose that just because
        // REQ-002 itself is outside the task's own link set.
        let mut req1 = sub("REQ-001", None);
        req1.def_hash = Some("h1".to_string());
        let mut req2 = sub("REQ-002", None);
        req2.def_hash = Some("new".to_string());
        let mut verifier = sub("AT-001", Some("acceptance"));
        verifier.verifies = vec!["REQ-001".to_string(), "REQ-002".to_string()];
        verifier
            .link_baselines
            .insert("REQ-001".to_string(), "h1".to_string());
        verifier
            .link_baselines
            .insert("REQ-002".to_string(), "old".to_string());
        let docs = vec![
            doc_with_subs("requirements", Some("requirement"), vec![req1, req2]),
            doc_with_subs("acceptance", Some("acceptance"), vec![verifier]),
        ];
        let registry = LayerRegistry::build(&[]);
        let trace_config = TraceConfig::default();
        let mut cache = LatestCache::default();
        cache.items.insert("AT-001".to_string(), latest("pass"));
        let links = vec![task_link("REQ-001", None, Some("h1"))];

        let full = full_graph_view(&docs, &registry, &trace_config, &cache, "t1", &links)
            .expect("full graph has a view for t1");
        assert_eq!(full.blockers.reverify, 1, "fixture precondition: {full:?}");

        let light =
            compute_task_blockers_for_task(&docs, &registry, &trace_config, &cache, "t1", &links)
                .unwrap();
        assert_eq!(light, full);
    }

    #[test]
    fn implements_verifier_outside_configured_layers_is_not_a_blocker() {
        // `[trace] layers` restricts in-use to "requirement"/"acceptance" —
        // a verifier on an out-of-scope layer ("unit_test") must not count
        // (mirrors t360.20.31's full-graph behavior, §3.4).
        let mut verifier = sub("UT-001", Some("unit_test"));
        verifier.verifies = vec!["REQ-001".to_string()];
        let docs = vec![
            doc_with_subs(
                "requirements",
                Some("requirement"),
                vec![sub("REQ-001", None)],
            ),
            doc_with_subs("unit", Some("unit_test"), vec![verifier]),
        ];
        let registry = LayerRegistry::build(&[]);
        let trace_config = TraceConfig {
            layers: vec!["requirement".to_string(), "acceptance".to_string()],
            ..Default::default()
        };
        let mut cache = LatestCache::default();
        cache.items.insert("UT-001".to_string(), latest("fail"));
        let links = vec![task_link("REQ-001", None, None)];

        let view =
            compute_task_blockers_for_task(&docs, &registry, &trace_config, &cache, "t1", &links)
                .unwrap();
        assert_eq!(view.blockers.failing, 0);
        assert_eq!(view.blockers.not_run, 0);
    }
}
