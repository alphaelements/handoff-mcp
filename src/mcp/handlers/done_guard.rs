//! The done guard's blocker computation (wiki/260 §3.4, wiki/430 §3.6).
//!
//! `handoff_update_task` / `handoff_bulk_update_tasks` consult this when a
//! task enters `review`/`done`. Beyond the per-link verification blockers
//! (`TaskBlockerCounts`) it folds in two spec-sync checks, each switchable
//! through `[trace.done_guard]`:
//!
//! - `check_profile_layers`: a linked item whose effective profile requires a
//!   layer nobody provides (`missing_profile_layer` lint, LINT-W030);
//! - `check_code_drift`: a linked item whose `impl_refs`/`test_refs` file
//!   changed since its `content_hash` was recorded (`code_drift`).
//!
//! `check_suspects` does not add work, it decides whether the suspect tally
//! `TaskBlockerCounts` already carries counts as a blocker.

use std::collections::{HashMap, HashSet};

use crate::storage::config::{DoneGuardConfig, TraceConfig};
use crate::storage::docs::code_hash;
use crate::storage::docs::layer::LayerRegistry;
use crate::storage::docs::model::DocMetadata;
use crate::storage::runs::LatestCache;
use crate::storage::tasks::TaskLink;
use crate::trace::adapter::{build_trace_input, collect_trace_items};
use crate::trace::lint::{evaluate, LintContext};
use crate::trace::task_view::with_tree_descendants;
use crate::trace::{compute_task_blockers_for_task, TaskBlockerCounts, TraceGraph};

const MISSING_PROFILE_LAYER_RULE: &str = "missing_profile_layer";

/// Everything the done guard found for one task.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DoneGuardFindings {
    /// The per-link tally; `suspect` is zeroed when `check_suspects` is off.
    pub blockers: TaskBlockerCounts,
    /// Linked items (sorted) lacking a layer their profile requires.
    pub missing_profile_layer: Vec<String>,
    /// Linked items (sorted) whose referenced code changed.
    pub code_drift: Vec<String>,
}

impl DoneGuardFindings {
    pub fn total(&self) -> usize {
        let b = &self.blockers;
        b.not_run
            + b.failing
            + b.blocked
            + b.reverify
            + b.suspect
            + b.approval_draft
            + self.missing_profile_layer.len()
            + self.code_drift.len()
    }
}

/// Computes `task_id`'s findings against `links` (the post-diff requirement
/// link set), or `None` when it has no requirement link at all.
pub(crate) fn compute_findings(
    docs: &[DocMetadata],
    registry: &LayerRegistry,
    trace_config: &TraceConfig,
    runs_cache: &LatestCache,
    handoff_dir: &std::path::Path,
    task_id: &str,
    links: &[TaskLink],
) -> Option<DoneGuardFindings> {
    let view =
        compute_task_blockers_for_task(docs, registry, trace_config, runs_cache, task_id, links)?;
    let settings = &trace_config.done_guard;
    let mut findings = DoneGuardFindings {
        blockers: view.blockers,
        ..Default::default()
    };
    if !settings.check_suspects {
        findings.blockers.suspect = 0;
    }
    if settings.check_profile_layers || settings.check_code_drift {
        let linked = linked_scope(docs, links);
        if settings.check_profile_layers {
            findings.missing_profile_layer =
                missing_profile_layer_items(docs, registry, trace_config, &linked);
        }
        if settings.check_code_drift {
            findings.code_drift = code_hash::collect_code_drift_items_in(
                code_hash::project_root(handoff_dir),
                docs,
                &linked,
            )
            .into_iter()
            .collect();
            findings.code_drift.sort();
        }
    }
    Some(findings)
}

/// The task's linked stable_ids plus every decomposition descendant (a
/// composite is judged by its leaves).
fn linked_scope(docs: &[DocMetadata], links: &[TaskLink]) -> HashSet<String> {
    let items = collect_trace_items(docs);
    let linked: HashSet<&str> = links
        .iter()
        .filter(|l| l.link_type == "requirement")
        .filter_map(|l| l.label.as_deref())
        .collect();
    with_tree_descendants(&items, &linked)
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// `missing_profile_layer` lint findings restricted to `scope`. Builds the
/// project trace graph, so it is skipped outright when nothing in the project
/// declares a layer requirement (no profile `default_needs`, no document
/// override, no item-level `needs`) — the lint cannot fire then.
fn missing_profile_layer_items(
    docs: &[DocMetadata],
    registry: &LayerRegistry,
    trace_config: &TraceConfig,
    scope: &HashSet<String>,
) -> Vec<String> {
    let trace_input = build_trace_input(
        docs,
        &[],
        &LatestCache::default(),
        crate::mcp::handlers::docs::collect_all_stable_ids(docs),
        trace_config.layers.clone(),
        registry,
        trace_config,
    );
    let declares_needs = !trace_input.project_default_needs.is_empty()
        || !trace_input.doc_profile_overrides.is_empty()
        || trace_input.items.iter().any(|i| i.needs.is_some());
    if !declares_needs {
        return Vec::new();
    }
    let graph = TraceGraph::build(&trace_input);
    let item_meta = HashMap::new();
    let resynced = HashSet::new();
    let tree_text = HashMap::new();
    let ctx = LintContext {
        docs,
        item_meta: &item_meta,
        unreadable: &[],
        task_ids_drift: &[],
        per_doc_sync_warnings: &[],
        resynced_doc_slugs: &resynced,
        tree_text: &tree_text,
    };
    let only: HashSet<String> = [MISSING_PROFILE_LAYER_RULE.to_string()].into();
    let mut items: Vec<String> =
        evaluate(&graph, &trace_input, &ctx, &trace_config.lint, Some(&only))
            .into_iter()
            .filter_map(|f| f.item)
            .filter(|id| scope.contains(id))
            .collect();
    items.sort();
    items.dedup();
    items
}

/// One-line human summary, or `None` when nothing blocks.
pub(crate) fn blocker_message(task_id: &str, findings: &DoneGuardFindings) -> Option<String> {
    let total = findings.total();
    if total == 0 {
        return None;
    }
    let b = &findings.blockers;
    let mut msg = format!(
        "done_guard: task {task_id} has {total} outstanding blocker(s) among its requirement \
         links (not_run={}, failing={}, blocked={}, reverify={}, suspect={}, approval_draft={}, \
         missing_profile_layer={}, code_drift={}).",
        b.not_run,
        b.failing,
        b.blocked,
        b.reverify,
        b.suspect,
        b.approval_draft,
        findings.missing_profile_layer.len(),
        findings.code_drift.len()
    );
    if !findings.missing_profile_layer.is_empty() {
        msg.push_str(&format!(
            " Items missing a profile-required layer: {}.",
            findings.missing_profile_layer.join(", ")
        ));
    }
    if !findings.code_drift.is_empty() {
        msg.push_str(&format!(
            " Items whose referenced code changed (code_drift): {}.",
            findings.code_drift.join(", ")
        ));
    }
    Some(msg)
}

/// `[trace] done_guard` mode with the fail-safe fallback: any value other
/// than `block`/`off` (a typo, a future mode) reads as `warn` — never
/// silently stricter, never silently disabled.
pub(crate) fn effective_mode(settings: &DoneGuardConfig) -> &str {
    match settings.mode.as_str() {
        m @ ("block" | "off") => m,
        _ => "warn",
    }
}
