//! Builds [`super::types::TraceInput`] from the live storage layer — a
//! `DocSet`'s documents, every task's requirement/doc links, and the
//! `runs/_latest.json` cache. **Not wired into any MCP handler yet**
//! (`handoff_trace_report`/`handoff_trace_slice` wiring is t360.10/11's
//! scope) — this module exists so those tasks have a ready-made, pure
//! conversion function rather than re-deriving the mapping themselves.

use std::collections::HashMap;

use crate::storage::config::TraceConfig;
use crate::storage::docs::layer::{LayerRegistry, RegisteredLayer};
use crate::storage::docs::model::DocMetadata;
use crate::storage::runs::LatestCache;
use crate::storage::tasks::TaskData;

use super::profile::{resolve_profile_by_name, resolve_project_profile};
use super::types::{
    EffectiveProfile, RunResultHashes, TaskDocLink, TaskLinkRole, TaskRequirementLink, TraceInput,
    TraceItemInput, WaiverAxis,
};

/// `SubItem.approval`/`status` -> the approval axis's resolved value
/// (wiki/270-vmodel-m3-design.md §2.3's priority rule, M3-03) — mirrors
/// `src/mcp/handlers/trace.rs`'s and `src/mcp/handlers/trace_lint.rs`'s own
/// private `approval_str` (duplicated rather than imported, same reasoning as
/// `trace_lint.rs`'s copy: each call site lives in a module with no other
/// reason to depend on another's unrelated responsibilities). `approval:
/// Some(_)` is authoritative (`"draft"`/`"review"`/`"approved"`); `None` falls
/// back to the M2 E12 read-mapping of `status` (`"verified"` -> `"approved"`,
/// else `"draft"`).
fn approval_str(approval: Option<&str>, status: &str) -> &'static str {
    match approval {
        Some("approved") => "approved",
        Some("review") => "review",
        Some("draft") => "draft",
        Some(_) | None => {
            if status == "verified" {
                "approved"
            } else {
                "draft"
            }
        }
    }
}

/// Collects every layer item (any `SubItem` with a `stable_id`) across
/// `docs` into [`TraceItemInput`]s, keyed by the item's effective layer
/// (`sub.layer.or(doc.layer)`, wiki/220 §2.3).
pub fn collect_trace_items(docs: &[DocMetadata]) -> Vec<TraceItemInput> {
    let mut out = Vec::new();
    for doc in docs {
        let Some(verification) = &doc.verification else {
            continue;
        };
        for item in &verification.items {
            for sub in &item.sub_items {
                let Some(stable_id) = &sub.stable_id else {
                    continue;
                };
                let waived_axes = sub
                    .waivers
                    .iter()
                    .filter_map(|w| match w.axis.as_str() {
                        "verify" => Some(WaiverAxis::Verify),
                        "refine" => Some(WaiverAxis::Refine),
                        _ => None,
                    })
                    .collect();
                out.push(TraceItemInput {
                    stable_id: stable_id.clone(),
                    doc_id: doc.id.clone(),
                    layer: sub.layer.clone().or_else(|| doc.layer.clone()),
                    refines: sub.refines.clone(),
                    verifies: sub.verifies.clone(),
                    method: sub.method.clone(),
                    has_test_refs: !sub.test_refs.is_empty(),
                    acceptance_labels: sub.acceptance.iter().map(|a| a.label.clone()).collect(),
                    derived: sub.derived.is_some(),
                    waived_axes,
                    def_hash: sub.def_hash.clone(),
                    body_hash: sub.body_hash.clone(),
                    link_baselines: sub.link_baselines.clone(),
                    needs: sub.needs.clone(),
                    approval: approval_str(sub.approval.as_deref(), &sub.status).to_string(),
                });
            }
        }
    }
    out
}

/// The ids of every document whose frontmatter sets `layer` (wiki/220
/// §2.1) — the "層文書" set the `task_unlinked` gap checks doc-links
/// against.
pub fn collect_layer_doc_ids(docs: &[DocMetadata]) -> std::collections::HashSet<String> {
    docs.iter()
        .filter(|d| d.layer.is_some())
        .map(|d| d.id.clone())
        .collect()
}

/// Task-side requirement links (wiki/220 §2.5, D3 — the task is the
/// authority) and doc links (wiki/220 §2.7's `task_unlinked`), gathered from
/// every task's normalized [`TaskData::links`].
pub fn collect_task_links(tasks: &[TaskData]) -> (Vec<TaskRequirementLink>, Vec<TaskDocLink>) {
    let mut requirement_links = Vec::new();
    let mut doc_links = Vec::new();
    for task in tasks {
        for link in task.links() {
            match link.link_type.as_str() {
                "requirement" => {
                    let Some(stable_id) = link.label else {
                        continue;
                    };
                    // §2.5: role defaults to "implements" when unset (older
                    // links, or a caller that never set it) — the effective
                    // side inference itself is `update_task`'s job
                    // (t360.7); here we only read whatever role is already
                    // stored.
                    let role = match link.role.as_deref() {
                        Some("executes") => TaskLinkRole::Executes,
                        _ => TaskLinkRole::Implements,
                    };
                    requirement_links.push(TaskRequirementLink {
                        task_id: task.id.clone(),
                        stable_id,
                        role,
                        baseline_hash: link.baseline_hash.clone(),
                    });
                }
                "doc" => {
                    doc_links.push(TaskDocLink {
                        task_id: task.id.clone(),
                        doc_id: link.target,
                    });
                }
                _ => {}
            }
        }
    }
    (requirement_links, doc_links)
}

/// `runs/_latest.json`'s per-item latest result (t360.8's [`LatestCache`]),
/// flattened to the `stable_id -> result` map [`TraceInput::runs_latest`]
/// needs.
pub fn collect_runs_latest(cache: &LatestCache) -> HashMap<String, String> {
    cache
        .items
        .iter()
        .map(|(id, latest)| (id.clone(), latest.result.clone()))
        .collect()
}

/// M2 (wiki/260 §3.2/E13, M2-05): `runs/_latest.json`'s per-item
/// `{def_hash, body_hash}` twin, flattened to the `stable_id ->
/// RunResultHashes` map [`TraceInput::runs_latest_hashes`] needs — a
/// separate pass over the same [`LatestCache`] [`collect_runs_latest`]
/// already reads, so a caller that doesn't need `trace_suspect`'s
/// `result`-kind check can skip calling this (kept as its own function
/// rather than folded into `collect_runs_latest`'s return value, which
/// every existing call site destructures as a plain
/// `HashMap<String, String>`).
pub fn collect_runs_latest_hashes(cache: &LatestCache) -> HashMap<String, RunResultHashes> {
    cache
        .items
        .iter()
        .map(|(id, latest)| {
            (
                id.clone(),
                RunResultHashes {
                    def_hash: latest.def_hash.clone(),
                    body_hash: latest.body_hash.clone(),
                },
            )
        })
        .collect()
}

/// M2-03 (wiki/260 §2.1 規則 1): every document's `trace_profile` override,
/// resolved to `{name, layers}` via [`resolve_profile_by_name`] — a document
/// with no override, an empty override, or one that doesn't resolve (unknown
/// name, `extends` cycle) is simply absent here, so its root items fall back
/// to the project default tier (§2.1's "設定エラーは無効化" policy; the
/// warning `resolve_profile_by_name` returns is not re-surfaced by this
/// function — `handlers/trace.rs` already collects the *project-wide*
/// profile/layer warnings via the same underlying calls, M2-01).
pub fn collect_doc_profile_overrides(
    docs: &[DocMetadata],
    trace_config: &TraceConfig,
    registry: &LayerRegistry,
) -> HashMap<String, EffectiveProfile> {
    let mut out = HashMap::new();
    for doc in docs {
        let Some(name) = doc.trace_profile.as_deref().filter(|s| !s.is_empty()) else {
            continue;
        };
        let (resolved, _warnings) = resolve_profile_by_name(name, trace_config, registry);
        if let Some(profile) = resolved {
            out.insert(
                doc.id.clone(),
                EffectiveProfile {
                    name: profile.name,
                    layers: profile.layers,
                },
            );
        }
    }
    out
}

/// M2-03 (wiki/260 §2.1 規則 2/4): the project default profile's own name,
/// when it resolves from a *named* profile (`[trace] profile = "..."`).
/// `None` when the default instead comes from raw `[trace] layers` or
/// auto-detection — both still serve as the project default *layer set*
/// (already carried by `TraceInput::profile_layers`/auto-detection), they
/// just have no profile name to attach to `items[].profile`.
pub fn project_default_profile_name(
    trace_config: &TraceConfig,
    registry: &LayerRegistry,
) -> Option<String> {
    resolve_project_profile(trace_config, registry)
        .0
        .map(|p| p.name)
}

/// Assembles a full [`TraceInput`] from already-loaded documents, tasks, the
/// runs cache, project-wide stable_id ownership (t360.2's
/// `collect_all_stable_ids`), the effective `[trace] layers` config (the only
/// piece a caller can override per-call via the `layers` MCP argument, hence
/// still a separate parameter), the project's [`LayerRegistry`] (built-ins +
/// `[[trace.layer]]`), and the raw `[trace]`/`[trace.profiles.*]` config —
/// this function derives the project default profile's resolved `layers`
/// (wiki/260 §2.1, M2-01) *and* name, plus every document's `trace_profile`
/// override (M2-03 §2.1 規則 1-4 — `resolve_effective_layers`'s
/// tree-inheritance input), from `trace_config`/`registry` itself rather than
/// taking them as separate arguments (`clippy::too_many_arguments`).
/// Takes everything pre-loaded — like [`super::engine`], this does no I/O of
/// its own (wiki/240-performance-design.md §5-5: one graph build per
/// request, from data the caller already loaded once).
pub fn build_trace_input(
    docs: &[DocMetadata],
    tasks: &[TaskData],
    runs_latest: &LatestCache,
    stable_id_owners: HashMap<String, Vec<String>>,
    configured_layers: Vec<String>,
    registry: &LayerRegistry,
    trace_config: &TraceConfig,
) -> TraceInput {
    let items = collect_trace_items(docs);
    let layer_doc_ids = collect_layer_doc_ids(docs);
    let (task_requirement_links, task_doc_links) = collect_task_links(tasks);
    let layer_registry: Vec<RegisteredLayer> = registry.all().to_vec();
    let doc_profile_overrides = collect_doc_profile_overrides(docs, trace_config, registry);
    let (resolved_default_profile, _warnings) = resolve_project_profile(trace_config, registry);
    let profile_layers = resolved_default_profile
        .as_ref()
        .map(|p| p.layers.clone())
        .unwrap_or_default();
    let project_default_needs = resolved_default_profile
        .as_ref()
        .map(|p| p.default_needs.clone())
        .unwrap_or_default();
    let project_default_profile_name = resolved_default_profile.map(|p| p.name);
    TraceInput {
        items,
        task_requirement_links,
        task_doc_links,
        layer_doc_ids,
        runs_latest: collect_runs_latest(runs_latest),
        runs_latest_hashes: collect_runs_latest_hashes(runs_latest),
        stable_id_owners,
        configured_layers,
        profile_layers,
        layer_registry,
        doc_profile_overrides,
        project_default_profile_name,
        project_default_needs,
    }
}

#[cfg(test)]
mod tests;
