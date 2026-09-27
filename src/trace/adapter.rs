//! Builds [`super::types::TraceInput`] from the live storage layer — a
//! `DocSet`'s documents, every task's requirement/doc links, and the
//! `runs/_latest.json` cache. **Not wired into any MCP handler yet**
//! (`handoff_trace_report`/`handoff_trace_slice` wiring is t360.10/11's
//! scope) — this module exists so those tasks have a ready-made, pure
//! conversion function rather than re-deriving the mapping themselves.

use std::collections::HashMap;

use crate::storage::docs::model::DocMetadata;
use crate::storage::runs::LatestCache;
use crate::storage::tasks::TaskData;

use super::types::{TaskDocLink, TaskLinkRole, TaskRequirementLink, TraceInput, TraceItemInput};

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
                out.push(TraceItemInput {
                    stable_id: stable_id.clone(),
                    doc_id: doc.id.clone(),
                    layer: sub.layer.clone().or_else(|| doc.layer.clone()),
                    refines: sub.refines.clone(),
                    verifies: sub.verifies.clone(),
                    method: sub.method.clone(),
                    has_test_refs: !sub.test_refs.is_empty(),
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

/// Assembles a full [`TraceInput`] from already-loaded documents, tasks, the
/// runs cache, project-wide stable_id ownership (t360.2's
/// `collect_all_stable_ids`), and the effective `[trace] layers` config.
/// Takes everything pre-loaded — like [`super::engine`], this does no I/O of
/// its own (wiki/240-performance-design.md §5-5: one graph build per
/// request, from data the caller already loaded once).
pub fn build_trace_input(
    docs: &[DocMetadata],
    tasks: &[TaskData],
    runs_latest: &LatestCache,
    stable_id_owners: HashMap<String, Vec<String>>,
    configured_layers: Vec<String>,
) -> TraceInput {
    let items = collect_trace_items(docs);
    let layer_doc_ids = collect_layer_doc_ids(docs);
    let (task_requirement_links, task_doc_links) = collect_task_links(tasks);
    TraceInput {
        items,
        task_requirement_links,
        task_doc_links,
        layer_doc_ids,
        runs_latest: collect_runs_latest(runs_latest),
        stable_id_owners,
        configured_layers,
    }
}

#[cfg(test)]
mod tests;
