//! `handoff_trace_next` (wiki/260-vmodel-m2-design.md §4.5/§3.5, M2-10,
//! FR-703): read-only (E6, same fully read-only load `handoff_trace_lint`/
//! `handoff_trace_matrix`/`handoff_trace_impact` use —
//! `trace_readonly::load_trace_input_fully_read_only`) ranking of "what to do
//! next" across the whole trace graph (or one task's own `requirement`-linked
//! slice, `task_id?`) into §3.5's 8 kinds, each carrying a concrete suggested
//! MCP tool call. All derivation logic (the 8 kinds' detection rules and the
//! deterministic sort) lives in `src/trace/next.rs`'s pure
//! `derive_next_actions` — this module only owns E6's read-only load,
//! argument parsing/validation, and JSON shaping.

use std::collections::{HashMap, HashSet};

use anyhow::{bail, Result};
use serde_json::{json, Value};

use super::plan_links::{linked_stable_ids, resolve_plan_sub_items};
use super::trace_readonly::load_trace_input_fully_read_only;
use super::HandlerContext;
use crate::storage::docs::model::UNSET_DEV_STAGE;
use crate::storage::docs::DocMetadata;
use crate::storage::tasks::TaskData;
use crate::trace::next::{derive_next_actions, ItemNextMeta, NextActionKind, PlanUpdateRef};
use crate::trace::TraceGraph;

/// stable_id -> `{priority, dev_stage}` from `docs` — [`crate::trace::next`]
/// deliberately keeps these off `TraceItemInput` (see that module's own doc
/// comment), so every caller gathers them from storage itself. Mirrors
/// `trace_lint.rs`'s `collect_item_lint_meta`/`ItemLintMeta` (duplicated
/// rather than shared — see this module's own doc comment on why
/// `src/trace/next.rs` doesn't import that handler-module type).
fn collect_item_next_meta(docs: &[DocMetadata]) -> HashMap<String, ItemNextMeta> {
    let mut out = HashMap::new();
    for doc in docs {
        let Some(v) = &doc.verification else {
            continue;
        };
        for item in &v.items {
            for sub in &item.sub_items {
                let Some(id) = sub.stable_id.clone() else {
                    continue;
                };
                out.entry(id).or_insert_with(|| ItemNextMeta {
                    priority: sub.priority.clone(),
                    dev_stage: sub.dev_stage.clone(),
                    assignee: sub.assignee.clone(),
                    ..Default::default()
                });
            }
        }
    }
    out
}

/// t-layergen.9 (REQ-LAG-006): for every code-drifted item, the plan-document
/// `SubItem`s reachable through a (non-`executes`) task that links the drifted
/// item — the same task -> plan `SubItem` lookup `handoff_update_task`'s
/// `plan_update_needed` uses (`plan_links`). Stored on the item's
/// [`ItemNextMeta::plan_updates`], which `derive_next_actions` attaches to its
/// `update_drifted_spec` action as `plan_update_needed`.
fn attach_plan_updates(
    meta: &mut HashMap<String, ItemNextMeta>,
    drifted: &HashSet<String>,
    tasks: &[TaskData],
    docs: &[DocMetadata],
) {
    for id in drifted {
        let mut sibling_ids: Vec<String> = Vec::new();
        for task in tasks {
            let linked = linked_stable_ids(&task.task_links, false);
            if !linked.contains(id) {
                continue;
            }
            for other in linked {
                if &other != id && !sibling_ids.contains(&other) {
                    sibling_ids.push(other);
                }
            }
        }
        if sibling_ids.is_empty() {
            continue;
        }
        let refs: Vec<PlanUpdateRef> = resolve_plan_sub_items(docs, &sibling_ids)
            .into_iter()
            .map(|p| PlanUpdateRef {
                doc_id: p.doc_id,
                stable_id: p.stable_id,
                current_dev_stage: p.dev_stage.unwrap_or_else(|| UNSET_DEV_STAGE.to_string()),
            })
            .collect();
        if !refs.is_empty() {
            meta.entry(id.clone()).or_default().plan_updates = refs;
        }
    }
}

/// `pub(super)`: also used by `trace_test_run.rs`'s `scope.kinds` parsing
/// (M3, wiki/270-vmodel-m3-design.md §4.4, FR-304) — the same kind-id
/// vocabulary, so callers must parse it identically rather than maintaining
/// a second copy of this match.
pub(super) fn parse_kind(s: &str) -> Option<NextActionKind> {
    match s {
        "fix_failing" => Some(NextActionKind::FixFailing),
        "review_suspect" => Some(NextActionKind::ReviewSuspect),
        "update_drifted_spec" => Some(NextActionKind::UpdateDriftedSpec),
        "rerun" => Some(NextActionKind::Rerun),
        "manual_pending" => Some(NextActionKind::ManualPending),
        "write_verification" => Some(NextActionKind::WriteVerification),
        "refine" => Some(NextActionKind::Refine),
        "create_task" => Some(NextActionKind::CreateTask),
        "fix_link" => Some(NextActionKind::FixLink),
        "baseline" => Some(NextActionKind::Baseline),
        "relink_candidate" => Some(NextActionKind::RelinkCandidate),
        _ => None,
    }
}

fn string_array_arg(arguments: &Value, key: &str) -> Vec<String> {
    arguments
        .get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

pub fn handle_trace_next(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let task_id = arguments.get("task_id").and_then(|v| v.as_str());
    let layers_filter = string_array_arg(arguments, "layers");
    // M3 (wiki/270-vmodel-m3-design.md §4.5, FR-307): `assignee?` narrows the
    // candidate set to items whose `SubItem.assignee` matches exactly — same
    // "applied before candidate generation" rule `layers_filter` follows.
    let assignee_filter = arguments.get("assignee").and_then(|v| v.as_str());

    // Same "key present but wrong shape must not silently collapse into "no
    // filter"" policy `trace_lint`'s `rules`/`trace_tasks`'s `select.gap_kinds`
    // already apply to their own array-of-enum filters (t360.20.32/34).
    let kinds_filter: Option<HashSet<NextActionKind>> = match arguments.get("kinds") {
        None => None,
        Some(v) => {
            let arr = v
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("kinds: must be an array of kind ids, got {v}"))?;
            let mut kinds = HashSet::new();
            for entry in arr {
                let s = entry.as_str().ok_or_else(|| {
                    anyhow::anyhow!("kinds: every entry must be a string, got {entry}")
                })?;
                let kind = parse_kind(s).ok_or_else(|| {
                    anyhow::anyhow!(
                        "kinds: unknown kind {s:?}; expected one of fix_failing, review_suspect, \
                         update_drifted_spec, rerun, manual_pending, write_verification, refine, create_task, \
                         fix_link, baseline, relink_candidate"
                    )
                })?;
                kinds.insert(kind);
            }
            if kinds.is_empty() {
                bail!("kinds: must not be empty (omit --kinds entirely to run every kind)");
            }
            Some(kinds)
        }
    };

    let limit = arguments
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(10) as usize;

    // E6: fully read-only load — in-memory-only resync of a directly-edited
    // layer document, `runs::load_latest_readonly`, `task_ids` resolved from
    // the task side without self-repair. Never writes anything under
    // `.handoff/`.
    let read_only = load_trace_input_fully_read_only(handoff, Vec::new())?;
    let graph = TraceGraph::build(&read_only.loaded.trace_input);
    let mut meta = collect_item_next_meta(&read_only.loaded.docs);
    attach_plan_updates(
        &mut meta,
        &read_only.loaded.trace_input.code_drift_items,
        &read_only.loaded.tasks,
        &read_only.loaded.docs,
    );

    let scope_ids: Option<HashSet<String>> = match task_id {
        None => None,
        Some(tid) => {
            if !read_only.loaded.tasks.iter().any(|t| t.id == tid) {
                bail!("Task not found: {tid}");
            }
            Some(
                read_only
                    .loaded
                    .trace_input
                    .task_requirement_links
                    .iter()
                    .filter(|l| l.task_id == tid)
                    .map(|l| l.stable_id.clone())
                    .collect(),
            )
        }
    };

    let (actions, truncated) = derive_next_actions(
        &graph,
        &read_only.loaded.trace_input,
        &meta,
        scope_ids.as_ref(),
        &layers_filter,
        assignee_filter,
        kinds_filter.as_ref(),
        limit,
    );

    let out = json!({
        "actions": actions,
        "truncated": truncated,
        "warnings": read_only.warnings,
    });

    Ok(serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string()))
}
