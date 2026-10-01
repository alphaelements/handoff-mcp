//! `handoff_trace_record`/`handoff_trace_report`/`handoff_trace_slice`
//! (wiki/220-vmodel-integration-design.md §2.6/§3.1/§3.2/§3.3, FR-302/501/502/
//! 108/105/303/701) — M1 (t360.8/t360.9/t360.10/t360.11). `handoff_trace_record`
//! records one execution batch (a set of `{item, result}` pairs, e.g. one CI
//! run or one manual verification pass) as a single `runs/<run_id>.json` file,
//! refreshes the derived `runs/_latest.json` cache, and (t360.43 S3, §3.1)
//! refreshes `_requirements_summary.json` — but never `_trace_report.json`,
//! see below.
//!
//! `handoff_trace_report`/`handoff_trace_slice` (t360.10/t360.11) build one
//! `crate::trace::TraceGraph` per request (wiki/240-performance-design.md
//! §5-5) from the live storage layer via `crate::trace::adapter`, after first
//! (§2.4) re-syncing any layer document whose body was edited directly (its
//! raw-byte hash no longer matches `source.body_raw_hash`) and (§2.5)
//! self-repairing `SubItem.task_ids` drift via `docs::rebuild_item_task_ids_full`
//! (a no-op unless the §4.3 `tasks_*` fingerprint moved since the last full
//! rebuild).
//!
//! `_trace_report.json` (t360.13, §3.4) is written by [`write_trace_report`],
//! called from `handle_trace_report` only (and CLI `trace report`, which
//! dispatches to the same handler) — see that function's doc comment for why
//! neither the frequent `handoff_update_task`/`handoff_doc_verify`/
//! `handoff_doc_update_section` write paths (which do refresh
//! `_requirements_summary.json` on every call) nor `handle_trace_record`
//! itself (measured ~271ms at L scale once wired in — PR-4's own 100ms
//! budget for that op) also rebuild this file. `handle_trace_history` (the
//! fourth `trace` CLI subcommand, §3.4) is a plain read over `runs/` and
//! never writes anything.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};

use super::docs::{
    collect_all_stable_ids, compute_derived_inputs, rebuild_item_task_ids_full,
    record_derived_write_for_test, resolve_pending_cross_doc_baselines, sync_layer_items_if_needed,
    sync_layer_items_local, write_requirements_summary, write_requirements_summary_with_inputs,
    DerivedInputs,
};
use super::HandlerContext;
use crate::storage::config::{read_config, TraceConfig};
use crate::storage::docs::layer::LayerRegistry;
use crate::storage::docs::layer_parse::{default_prefix_table, parse_layer_body};
use crate::storage::docs::layer_sync::PendingBaseline;
use crate::storage::docs::model::{AcRef, CodeRef, DocMetadata, Waiver};
use crate::storage::docs::{
    ensure_docs_dir, read_all_docs, read_all_docs_with_unreadable, read_doc_body, write_doc, DocSet,
};
use crate::storage::runs::{self, is_valid_result, record_run, LatestCache, RunResultInput};
use crate::storage::tasks::{collect_all_tasks, TaskData};
use crate::trace::profile::resolve_project_profile;
use crate::trace::task_view::compute_task_views;
use crate::trace::{
    adapter, GapKind, LayersSource, Suspect, SuspectKind, TaskLinkRole, TraceGraph, TraceInput,
};

/// `handoff_trace_record` (§3.1). Input: `results: [{item, result, note?,
/// evidence?[]}]` (required, non-empty), `executor_kind?` (`"ai"` | `"human"`,
/// default `"ai"`), `executor_id?`, `commit?` (defaults to `git rev-parse
/// --short HEAD`, empty on failure), `task_id?`. Output: `{run_id, recorded,
/// warnings}`.
pub fn handle_trace_record(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;
    let project_dir = &ctx.project_dir;

    let results_val = arguments
        .get("results")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("'results' (non-empty array) is required"))?;
    if results_val.is_empty() {
        anyhow::bail!("'results' must not be empty");
    }

    let mut warnings = Vec::new();
    let mut inputs = Vec::with_capacity(results_val.len());
    for (i, entry) in results_val.iter().enumerate() {
        let item = entry
            .get("item")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("results[{i}].item is required"))?;
        let result = entry
            .get("result")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("results[{i}].result is required"))?;
        if !is_valid_result(result) {
            anyhow::bail!(
                "results[{i}].result={result:?} is invalid (must be one of pass, fail, blocked, \
                 not_run, skipped)"
            );
        }
        let note = entry.get("note").and_then(|v| v.as_str());
        let evidence: Vec<String> = entry
            .get("evidence")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        inputs.push(RunResultInput {
            item,
            result,
            note,
            evidence,
        });
    }

    let executor_kind = arguments
        .get("executor_kind")
        .and_then(|v| v.as_str())
        .unwrap_or("ai");
    if executor_kind != "ai" && executor_kind != "human" {
        anyhow::bail!("executor_kind={executor_kind:?} must be \"ai\" or \"human\"");
    }
    let executor_id = arguments.get("executor_id").and_then(|v| v.as_str());
    let commit = match arguments.get("commit").and_then(|v| v.as_str()) {
        Some(c) => Some(c.to_string()),
        None => Some(crate::storage::git::short_head_or_empty(project_dir)),
    };
    let task_id = arguments
        .get("task_id")
        .and_then(|v| v.as_str())
        .map(String::from);

    let mut docs = read_all_docs(handoff)?;

    // R-05 (wiki/260-vmodel-m2-design.md §2.5's closing rule, M2-04):
    // `trace_record` is one of the write paths §2.5 names explicitly
    // ("`trace_record`（`runs::find_body_hash`）は保存値をそのまま使ってお
    // り、直接編集後に記録した run が古いハッシュを持つ。M2-04 で直す") —
    // ensure every result's owning document reflects today's `def_hash`/
    // `body_hash` (a direct body edit, or a sync-affecting config change,
    // E7) *before* `record_run` reads them, resyncing (and persisting) it
    // first if not. Bounded to exactly the documents this call's `results`
    // actually reference, never a corpus-wide pass (PR-4's own budget for
    // this op, `tests/perf_budgets.toml`'s `trace_record` entry).
    let target_ids: HashSet<&str> = inputs.iter().map(|r| r.item).collect();
    let mut resynced_doc_ids: Vec<String> = Vec::new();
    for doc in docs.iter_mut() {
        if doc.layer.is_none() {
            continue;
        }
        let owns = doc.verification.as_ref().is_some_and(|v| {
            v.items.iter().flat_map(|i| &i.sub_items).any(|s| {
                s.stable_id
                    .as_deref()
                    .is_some_and(|id| target_ids.contains(id))
            })
        });
        if !owns {
            continue;
        }
        let Some(body) = read_doc_body(handoff, &doc.slug)? else {
            continue;
        };
        let now = chrono::Utc::now().to_rfc3339();
        let mut sync_warnings = Vec::new();
        if sync_layer_items_if_needed(handoff, doc, &body, &now, false, &mut sync_warnings) {
            resynced_doc_ids.push(doc.id.clone());
        }
        warnings.append(&mut sync_warnings);
    }
    for doc_id in &resynced_doc_ids {
        if let Some(doc) = docs.iter().find(|d| &d.id == doc_id) {
            write_doc(handoff, doc)?;
        }
    }

    let (run_id, mut record_warnings) = record_run(
        handoff,
        &docs,
        &inputs,
        executor_kind,
        executor_id,
        commit,
        task_id,
    )?;
    warnings.append(&mut record_warnings);

    // S3 fix (t360.43 M1 review, wiki/220 §3.1: "記録後に `_latest.json` と
    // summary を更新する"). `record_run` above already refreshes
    // `runs/_latest.json`; this call is what was missing —
    // `_requirements_summary.json` was never refreshed by
    // `handoff_trace_record` at all pre-fix, leaving its `inputs.runs_*`
    // fields stale after every recorded run until some unrelated write
    // happened to touch it. `docs` was already read (above, before
    // `record_run`) and is unaffected by it, so this reuses that same
    // corpus rather than paying a second `read_all_docs`. Cheap relative to
    // `_trace_report.json`'s full `TraceGraph` build (measured ~271ms at L
    // scale, see the comment on the *next* paragraph) — `write_requirements_summary`
    // is P-M4 stat-and-compare, not a graph rebuild, so this does not
    // reproduce that regression; see this task's dev report for the
    // measured `trace_record` p50 with this call included.
    write_requirements_summary(handoff, &docs)?;

    // t360.13 (wiki/220 §3.4): `handoff_trace_record` deliberately does
    // **not** also rebuild/write `_trace_report.json` here, even though an
    // earlier revision of this task's design tried exactly that. Measured
    // p50 with the rebuild wired in: ~271ms at L scale (perf_budget, release
    // build) — PR-4's own budget for this op is 100ms (see
    // `tests/perf_budgets.toml`'s `trace_record` entry, "t360.8 ... PR-4
    // target ≤100ms"), so this would have been a ~2.7x regression on a
    // frequent, tight-budget op. `_trace_report.json` is only (re)written
    // from `handle_trace_report`/CLI `trace report` (which already pays the
    // graph-build cost for its own response) — recording a run just leaves
    // the derived file's `inputs` fingerprint stale until the next report
    // call, exactly the staleness handoff-vscode's design already handles
    // (wiki/100 §3.3: detect via the fingerprint, auto-run `trace report`).
    let out = json!({
        "run_id": run_id,
        "recorded": inputs.len(),
        "warnings": warnings,
    });
    Ok(serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string()))
}

/// `handoff_trace_history` (wiki/220 §3.4, CLI `trace history`, VSCode
/// FR-903's execution-history display). Input: `item` (required, the
/// stable_id whose recorded results to list), `limit?` (default 20). Output:
/// `{items: [{run_id, executed_at, executor: {kind, id?}, result, note,
/// evidence, commit}]}`, newest first — a pure read over `.handoff/runs/`,
/// no side effects (never writes `_trace_report.json` or any other derived
/// file).
pub fn handle_trace_history(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;
    let item = arguments
        .get("item")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'item' is required"))?;
    let limit = arguments
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(20) as usize;

    let mut history = runs::history_for_item(handoff, item)?;
    history.truncate(limit);

    let out = json!({ "items": history });
    Ok(serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string()))
}

/// Re-syncs every layer document whose body was edited directly since its
/// last sync (wiki/220 §2.4: raw-byte FNV-1a hash != `source.body_raw_hash`,
/// **not** `lexsim::content_hash` — wiki/240 §5-3) — the side effect
/// `handoff_trace_report`/`handoff_trace_slice` are allowed to have before
/// aggregating (§2.4: "直接 .md 編集された層文書だけを同期してから集計").
/// `sync_layer_items_if_needed` itself carries the raw-hash short-circuit, so
/// calling it for every layer document on every call only ever *does* work
/// for the ones that actually changed; a document with no recorded
/// `body_raw_hash` yet (never synced) is always treated as changed, matching
/// §2.4's "body_raw_hash がない既存文書は「直接編集あり」とみなして1回同期".
///
/// Loads and flushes its own `DocSet` (one extra full-corpus read/parse
/// beyond the caller's own subsequent `read_all_docs`, acceptable at the
/// PR-7 JA scale — see this task's perf bench) so a resync's writes land on
/// disk before the caller re-reads the corpus for `TraceGraph::build`.
/// Refreshes `_requirements_summary.json` from the same `DocSet` afterward,
/// but only when at least one document actually resynced — mirrors
/// `refresh_after_layer_sync`'s step 7 (§2.4) without its single-document
/// collision-warning framing (this pass may resync more than one document at
/// once); the `duplicate_id` gap `TraceGraph::build` computes from
/// `stable_id_owners` already covers the collision-warning role for the
/// trace-report caller.
///
/// S1 fix (t360.43 M1 review): `summary_inputs_before_load` is captured
/// *before* `DocSet::load` below (its own read), not inside
/// `write_requirements_summary` afterward (the pre-fix shape) — see
/// [`write_requirements_summary_with_inputs`]'s doc comment for the race a
/// post-read fingerprint capture leaves open. The extra `compute_derived_inputs`
/// stat pass this costs on every call (even the common case where
/// `layer_docs` turns out empty and nothing is ever written) is deliberate:
/// there is no way to know in advance whether this call will need to write
/// the summary without first doing the very read whose "before" snapshot
/// this fingerprint must be.
///
/// M2-05 rework (review round 1, MAJOR): also called directly by
/// `trace_suspect`'s write actions (`action="clear"`, `action="baseline"`
/// with `dry_run=false`, `src/mcp/handlers/trace_suspect.rs`) — R-05
/// (wiki/260 §2.5's closing rule, restated at §4.1's end for these two
/// tools) requires a write action to never take a suspect's/unbaselined
/// link's hash from a stale stored `SubItem.def_hash`. `list`/
/// `baseline(dry_run=true)` deliberately do *not* call this (E6's read-only
/// contract — their own in-memory-only resync gap is M2-08's scope, wiki/260
/// §4.1's session-review note), so this is `pub(super)`, not merely private
/// to this file, from this fix onward.
pub(super) fn resync_direct_edited_layer_docs(
    handoff: &Path,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let summary_inputs_before_load = compute_derived_inputs(handoff)?;
    let mut doc_set = DocSet::load(handoff)?;
    let layer_docs: Vec<(String, String)> = doc_set
        .docs()
        .iter()
        .filter(|d| d.layer.is_some())
        .map(|d| (d.id.clone(), d.slug.clone()))
        .collect();
    if layer_docs.is_empty() {
        return Ok(());
    }

    let now = chrono::Utc::now().to_rfc3339();
    let mut any_synced = false;
    // t360.20.28: pass 1 — sync every layer document's *own* items locally
    // (`sync_layer_items_local`, not the plain `sync_layer_items_if_needed`
    // wrapper), deferring each document's cross-document `pending_baselines`
    // to the second pass below instead of resolving them right here one
    // document at a time. Resolving eagerly used to re-read the whole corpus
    // from disk (`resolve_pending_cross_doc_baselines`'s old behavior) — for
    // a sibling layer document that is *also* part of this same batch and
    // has never been through `doc_save` before, disk still shows
    // `verification: None` at that moment (this loop only flushes once, at
    // the very end), so it could never be found as an owner and the link was
    // left unbaselined forever, regardless of loop order.
    let mut pending_by_doc: Vec<(String, Vec<PendingBaseline>)> = Vec::new();
    for (doc_id, slug) in &layer_docs {
        let Some(body) = read_doc_body(handoff, slug)? else {
            continue;
        };
        if let Some(doc) = doc_set.get_mut(doc_id) {
            if let Some(local) = sync_layer_items_local(handoff, doc, &body, &now, false, warnings)
            {
                doc_set.mark_dirty(doc_id);
                any_synced = true;
                if !local.pending.is_empty() {
                    pending_by_doc.push((doc_id.clone(), local.pending));
                }
            }
        }
    }
    // Pass 2: now that every layer document in this batch has already run
    // through its own local sync above (in memory, not yet flushed to
    // disk), resolve every pending cross-document baseline against a
    // snapshot of `doc_set` itself — never by re-reading disk — so a sibling
    // document this batch just synced for the first time is found exactly
    // like an already-synced one would be. `read_config`/`LayerRegistry::build`
    // is computed once here (not per-document) since it doesn't vary within
    // one project.
    if !pending_by_doc.is_empty() {
        let trace_config = read_config(&handoff.join("config.toml"))
            .map(|c| c.trace)
            .unwrap_or_default();
        let registry = LayerRegistry::build(&trace_config.layer);
        let corpus_snapshot: Vec<DocMetadata> = doc_set.docs().to_vec();
        for (doc_id, pending) in &pending_by_doc {
            if let Some(doc) = doc_set.get_mut(doc_id) {
                if let Err(e) = resolve_pending_cross_doc_baselines(
                    handoff,
                    doc,
                    pending,
                    &registry,
                    &trace_config.id_prefixes,
                    &corpus_snapshot,
                ) {
                    warnings.push(format!(
                        "failed to resolve {} cross-document link baseline(s): {e:#}",
                        pending.len()
                    ));
                }
            }
        }
    }
    if any_synced {
        doc_set.flush()?;
        write_requirements_summary_with_inputs(
            handoff,
            doc_set.docs(),
            summary_inputs_before_load,
        )?;
    }
    Ok(())
}

/// Loads everything `crate::trace::adapter::build_trace_input` needs from the
/// live storage layer, after `resync_direct_edited_layer_docs` and the §2.5
/// self-repair have already run — shared by `handle_trace_report` and
/// `handle_trace_slice` so both build their one graph (wiki/240 §5-5) from
/// the exact same loading sequence.
pub(super) struct LoadedTrace {
    pub(super) docs: Vec<DocMetadata>,
    pub(super) tasks: Vec<TaskData>,
    pub(super) latest_cache: LatestCache,
    pub(super) trace_input: TraceInput,
    /// The project's layer registry (built-ins + valid `[[trace.layer]]`
    /// declarations, wiki/260 §2.1, M2-01) — every layer-aware helper in
    /// this file (`side_str`, `default_prefix_table`) uses this instead of
    /// the old direct `BUILTIN_LAYERS`/`builtin_layer` references.
    pub(super) layer_registry: LayerRegistry,
    /// Non-fatal layer/profile config warnings (§2.1: invalid custom layer
    /// declarations, unresolvable profile references) — callers should fold
    /// these into their own response `warnings`.
    pub(super) config_warnings: Vec<String>,
}

pub(super) fn load_trace_input(handoff: &Path, layers_arg: Vec<String>) -> Result<LoadedTrace> {
    let latest_cache = runs::sync(handoff)?;
    // t360.20.22 (M2-S2 tester/reviewer/dev B finding, FR-804/E11): a
    // document whose frontmatter fails to parse must not simply vanish from
    // every trace read path (`handoff_trace_report`/`handoff_trace_slice`/
    // `handoff_trace_suspect`/`handoff_trace_impact`, all of which funnel
    // through this function or `load_trace_input_read_only`) with no trace at
    // all — `handoff_doc_list`'s `unreadable` already applies this FR-804
    // policy; this closes the gap the M2-S2 session found in `trace.rs`'s own
    // corpus read (the plain `read_all_docs` this used to call has no way to
    // report it).
    let (docs, unreadable) = read_all_docs_with_unreadable(handoff)?;
    let mut loaded = load_trace_input_from_docs(handoff, layers_arg, docs, latest_cache)?;
    loaded
        .config_warnings
        .extend(super::docs::unreadable_doc_warnings(&unreadable));
    Ok(loaded)
}

/// Same as [`load_trace_input`], but never calls `runs::sync` — used by
/// `handle_trace_impact` (M2-06 rework round 2 reviewer finding, wiki/260
/// §4.2/E6). `runs::sync` `atomic_write`s `runs/_latest.json` (and rewrites
/// `.handoff/.gitignore` via `ensure_gitignore_entry`) on the cache's first
/// materialization or whenever its `run_id`/`count` bookkeeping no longer
/// reconciles against the files on disk (e.g. right after a `git pull`) —
/// a real write that `handoff_trace_impact` cannot perform while listed in
/// `router.rs`'s `READ_ONLY_TOOLS` (those writes would run outside
/// `WRITE_MUTEX`, defeating the fail-safe contract that classification
/// exists for). `trace_impact`'s only use of `TraceInput.items` is fields
/// derived straight from document/task storage (`def_hash`, `link_baselines`,
/// `refines`/`verifies`, `method`, `has_test_refs`) — never anything derived
/// from `runs_latest` — so a plain, best-effort read of `runs/_latest.json`
/// (same technique as `docs.rs`'s `suspect_introduced_summary`) is exactly as
/// correct for this tool's purposes and never writes: a missing or corrupt
/// cache file simply reads as empty rather than triggering a rebuild.
/// M2-08 (wiki/260-vmodel-m2-design.md §4.1's session-review note, t360.20.8):
/// delegates to [`super::trace_readonly::load_trace_input_fully_read_only`] —
/// E6's complete read-only loading sequence (in-memory-only resync of a
/// directly-edited layer document, `runs::load_latest_readonly` instead of
/// `runs::sync`, and `SubItem.task_ids` resolved from the task side rather
/// than the stored, possibly-drifted value — none of it ever written to
/// disk). Folds that function's own extra warnings (unreadable documents,
/// in-memory resync notices, `task_ids` drift notices) into the returned
/// `LoadedTrace.config_warnings` so every existing caller of this function
/// (`handoff_trace_suspect`'s `list`/`baseline(dry_run)`, `handoff_trace_impact`)
/// keeps compiling and behaving unchanged — this is purely an internal
/// swap of *how* those warnings get produced, not a new field any caller has
/// to additionally read. The richer, structured half of what
/// `load_trace_input_fully_read_only` returns (unreadable list, task_ids
/// drift entries) is for `handoff_trace_lint`'s own rules
/// (`src/mcp/handlers/trace_lint.rs`), which calls that function directly
/// instead of going through this flattening wrapper.
pub(super) fn load_trace_input_read_only(
    handoff: &Path,
    layers_arg: Vec<String>,
) -> Result<LoadedTrace> {
    let read_only = super::trace_readonly::load_trace_input_fully_read_only(handoff, layers_arg)?;
    let mut loaded = read_only.loaded;
    loaded.config_warnings.extend(read_only.warnings);
    // M2-08 rework (reviewer round 1 MAJOR finding): a layer-registry warning
    // already present in `config_warnings` gets re-emitted into
    // `read_only.warnings` once per in-memory-resynced layer document (see
    // `dedup_preserve_order`'s own doc comment) — dedupe so
    // `trace_suspect`'s `list`/`baseline(dry_run)` and `trace_impact`, both
    // of which go through this function, don't report the same warning more
    // than once.
    super::trace_readonly::dedup_preserve_order(&mut loaded.config_warnings);
    Ok(loaded)
}

/// The non-read-only half of [`load_trace_input`]/[`load_trace_input_read_only`]:
/// turns an already-loaded `docs`/`latest_cache` pair into a [`LoadedTrace`] —
/// shared with [`super::trace_readonly::load_trace_input_fully_read_only`]
/// (M2-08), which loads `docs` itself via its own in-memory-only resync
/// sequence (E6) rather than a plain `read_all_docs`.
pub(super) fn load_trace_input_from_docs(
    handoff: &Path,
    layers_arg: Vec<String>,
    docs: Vec<DocMetadata>,
    latest_cache: LatestCache,
) -> Result<LoadedTrace> {
    let mut raw_tasks = Vec::new();
    collect_all_tasks(&handoff.join("tasks"), &mut raw_tasks)?;
    let tasks: Vec<TaskData> = raw_tasks.into_iter().map(|(data, _status)| data).collect();

    let stable_id_owners = collect_all_stable_ids(&docs);
    let trace_config = read_config(&handoff.join("config.toml"))
        .map(|c| c.trace)
        .unwrap_or_default();
    let layer_registry = LayerRegistry::build(&trace_config.layer);
    let configured_layers = if layers_arg.is_empty() {
        trace_config.layers.clone()
    } else {
        layers_arg
    };
    // wiki/260 §2.1 (M2-01): `layers` (explicit, above) ＞ project default
    // profile's `layers` ＞ auto — `adapter::build_trace_input` itself
    // resolves the project default profile's `layers`/name from
    // `trace_config`/`layer_registry` (M2-03), this call only needs the
    // warnings that resolution produces. Per-document `trace_profile` tree
    // inheritance is also M2-03's scope, likewise resolved inside
    // `build_trace_input`.
    let (_resolved_profile, profile_warnings) =
        resolve_project_profile(&trace_config, &layer_registry);
    let mut config_warnings = layer_registry.warnings.clone();
    config_warnings.extend(profile_warnings);

    let trace_input = adapter::build_trace_input(
        &docs,
        &tasks,
        &latest_cache,
        stable_id_owners,
        configured_layers,
        &layer_registry,
        &trace_config,
    );
    Ok(LoadedTrace {
        docs,
        tasks,
        latest_cache,
        trace_input,
        layer_registry,
        config_warnings,
    })
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

/// The exact string every `GapKind` serializes to (its `#[serde(rename_all =
/// "snake_case")]`) — used both to build `gap_counts`' JSON keys (a
/// `HashMap<GapKind, usize>` cannot be used as a `serde_json` map directly:
/// object keys must be strings, and relying on an enum's key-position
/// serialization behavior is fragile) and to parse the `gap_kinds` filter
/// argument back into `GapKind`s.
fn gap_kind_str(kind: GapKind) -> &'static str {
    match kind {
        GapKind::Unverified => "unverified",
        GapKind::Unrefined => "unrefined",
        GapKind::Orphan => "orphan",
        GapKind::Dangling => "dangling",
        GapKind::InvalidLink => "invalid_link",
        GapKind::Cycle => "cycle",
        GapKind::DuplicateId => "duplicate_id",
        GapKind::TaskUnlinked => "task_unlinked",
    }
}

fn parse_gap_kind(s: &str) -> Option<GapKind> {
    match s {
        "unverified" => Some(GapKind::Unverified),
        "unrefined" => Some(GapKind::Unrefined),
        "orphan" => Some(GapKind::Orphan),
        "dangling" => Some(GapKind::Dangling),
        "invalid_link" => Some(GapKind::InvalidLink),
        "cycle" => Some(GapKind::Cycle),
        "duplicate_id" => Some(GapKind::DuplicateId),
        "task_unlinked" => Some(GapKind::TaskUnlinked),
        _ => None,
    }
}

fn task_link_role_str(role: TaskLinkRole) -> &'static str {
    match role {
        TaskLinkRole::Implements => "implements",
        TaskLinkRole::Executes => "executes",
    }
}

/// One item's data as needed to build `handoff_trace_report`'s `items[]`
/// (§3.4) and `handoff_trace_slice`'s `items[]` (§3.3) — everything
/// `crate::trace::TraceGraph` itself does not carry (title, doc identity,
/// priority/dev_stage/category, impl_refs/test_refs), gathered directly from
/// `docs` the same way `aggregate_requirements` does. First occurrence wins
/// on a duplicate `stable_id` (matches `crate::trace::engine::resolve_items`'s
/// own tie-break — the `duplicate_id` gap is what surfaces the collision,
/// not this map silently picking a different winner).
struct ItemMeta {
    doc_id: String,
    doc_slug: String,
    title: String,
    layer: Option<String>,
    refines: Vec<String>,
    verifies: Vec<String>,
    fragment_seq: Option<usize>,
    sub_item_index: usize,
    priority: Option<String>,
    dev_stage: Option<String>,
    category: String,
    impl_refs: Vec<CodeRef>,
    test_refs: Vec<CodeRef>,
    /// M2 (wiki/260 §2.3/§3.3 E12, M2-07): `SubItem.status` ("pending" |
    /// "skipped" | "verified") — read-mapped to `items[].approval` by
    /// [`approval_str`] (`verified` -> `approved`, anything else ->
    /// `draft`).
    status: String,
    /// M2 (wiki/260 §2.4, M2-07): `SubItem.def_hash` — `items[].def_hash`.
    def_hash: Option<String>,
    /// M2 (wiki/260 §2.2/§2.3, M2-07): `SubItem.acceptance` —
    /// `items[].acceptance`.
    acceptance: Vec<AcRef>,
    /// M2 (wiki/260 §2.2/§2.3, M2-07): `SubItem.derived` — `items[].derived`.
    derived: Option<String>,
    /// M2 (wiki/260 §2.2/§2.3, M2-07): `SubItem.waivers` — `items[].waivers`.
    waivers: Vec<Waiver>,
    /// M2 (wiki/260 §2.2/§2.3, M2-07): `SubItem.from` — `items[].from`.
    from: Option<String>,
    /// M2 (wiki/260 §2.5/§2.3, M2-07): `SubItem.implicit_of` —
    /// `items[].implicit_of`.
    implicit_of: Option<String>,
}

fn collect_item_meta(docs: &[DocMetadata]) -> HashMap<String, ItemMeta> {
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
                out.entry(id).or_insert_with(|| ItemMeta {
                    doc_id: doc.id.clone(),
                    doc_slug: doc.slug.clone(),
                    title: sub.description.clone(),
                    layer: sub.layer.clone().or_else(|| doc.layer.clone()),
                    refines: sub.refines.clone(),
                    verifies: sub.verifies.clone(),
                    fragment_seq: item.fragment_seq,
                    sub_item_index: sub.index,
                    priority: sub.priority.clone(),
                    dev_stage: sub.dev_stage.clone(),
                    category: sub.category.clone(),
                    impl_refs: sub.impl_refs.clone(),
                    test_refs: sub.test_refs.clone(),
                    status: sub.status.clone(),
                    def_hash: sub.def_hash.clone(),
                    acceptance: sub.acceptance.clone(),
                    derived: sub.derived.clone(),
                    waivers: sub.waivers.clone(),
                    from: sub.from.clone(),
                    implicit_of: sub.implicit_of.clone(),
                });
            }
        }
    }
    out
}

/// stable_id -> every `{task_id, role}` pair a `requirement` task link
/// declares for it (wiki/220 §2.5) — shared by `handoff_trace_report`'s and
/// `handoff_trace_slice`'s `tasks[]` output.
///
/// t360.42 N4 (M1 adversarial review): each stable_id's task list is sorted
/// by task_id before returning. `trace_input.task_requirement_links`'
/// iteration order ultimately traces back to `collect_all_tasks`'
/// `std::fs::read_dir` order (`src/storage/tasks.rs`), which is OS/
/// filesystem-dependent, not insertion order — left unsorted, the
/// `_trace_report.json` derived file's byte content (t360.13, a VS Code
/// contract fixture) would vary across platforms/filesystems for identical
/// input state, which the contract fixture assumes never happens.
fn tasks_by_item(trace_input: &TraceInput) -> HashMap<String, Vec<(String, &'static str)>> {
    let mut out: HashMap<String, Vec<(String, &'static str)>> = HashMap::new();
    for link in &trace_input.task_requirement_links {
        out.entry(link.stable_id.clone())
            .or_default()
            .push((link.task_id.clone(), task_link_role_str(link.role)));
    }
    for tasks in out.values_mut() {
        tasks.sort_by(|a, b| a.0.cmp(&b.0));
    }
    out
}

fn side_str(registry: &LayerRegistry, layer: Option<&str>) -> Option<&'static str> {
    layer.and_then(|l| registry.get(l)).map(|d| d.side.as_str())
}

/// M2 (wiki/260 §3.3/E12, M2-07): `SubItem.status` read-mapped onto the
/// approval axis — `"verified"` -> `"approved"`, anything else (`"pending"`,
/// `"skipped"`) -> `"draft"`.
fn approval_str(status: &str) -> &'static str {
    if status == "verified" {
        "approved"
    } else {
        "draft"
    }
}

/// `items[].acceptance` (wiki/260 §5.1): `{id, label, kind}` per declared
/// acceptance-criteria bullet, `id` being the sub-reference form
/// (`"REQ-003#AC1"`) a `verifies`/`link_baselines` entry would use to point
/// at this specific AC (§2.2).
fn acceptance_json(item_id: &str, acceptance: &[AcRef]) -> Value {
    Value::Array(
        acceptance
            .iter()
            .map(|a| {
                json!({
                    "id": format!("{item_id}#{}", a.label),
                    "label": a.label,
                    "kind": a.kind,
                })
            })
            .collect(),
    )
}

/// `items[].waivers` (wiki/260 §5.1): `{axis, reason}` per `- waive-verify:`
/// / `- waive-refine:` attribute line (§2.2/§2.3).
fn waivers_json(waivers: &[Waiver]) -> Value {
    Value::Array(
        waivers
            .iter()
            .map(|w| json!({"axis": w.axis, "reason": w.reason}))
            .collect(),
    )
}

/// Groups `graph`'s suspects by their `item` field (wiki/260 §3.2's `kind`
/// discriminates what `item` means: the child for `link`, the linked
/// requirement item for `task`, the verification item itself for `result`)
/// — shared by `items[].suspect` (persisted file / `trace_report`) and
/// `trace_slice`'s own per-item `suspect` field, and by `last_run.stale`
/// (a `result`-kind suspect on this item).
fn group_suspects_by_item(graph: &TraceGraph) -> HashMap<&str, Vec<&Suspect>> {
    let mut by_item: HashMap<&str, Vec<&Suspect>> = HashMap::new();
    for s in graph.suspects() {
        by_item.entry(s.item.as_str()).or_default().push(s);
    }
    by_item
}

/// `items[].suspect` (wiki/260 §5.1): the subset of `trace_suspect(action=
/// "list")`'s per-entry shape that makes sense once already keyed by `item`
/// (the `item` field itself is dropped — it is the object key this array
/// lives under).
fn item_suspect_json(by_item: &HashMap<&str, Vec<&Suspect>>, id: &str) -> Value {
    Value::Array(
        by_item
            .get(id)
            .map(|list| {
                list.iter()
                    .map(|s| {
                        json!({
                            "kind": s.kind,
                            "upstream": s.upstream,
                            "task": s.task,
                            "link_type": s.link_type,
                            "baseline_hash": s.baseline_hash,
                            "current_hash": s.current_hash,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
    )
}

/// `last_run.stale` (wiki/260 §5.1): `true` exactly when `id` carries a
/// `result`-kind suspect (§3.2: "最新の合格は変更前の定義に対する結果です",
/// §5.4) — meaningless (and therefore always `false`) when there is no
/// recorded run at all, which the caller already guards via `Option::map`.
fn last_run_is_stale(by_item: &HashMap<&str, Vec<&Suspect>>, id: &str) -> bool {
    by_item
        .get(id)
        .is_some_and(|list| list.iter().any(|s| s.kind == SuspectKind::Result))
}

/// `layer_defs` (wiki/260 §5.1): every registered layer (built-in +
/// `[[trace.layer]]`), with its *effective* `id_prefixes` (defaults +
/// `[trace.id_prefixes]` additions, §2.1) — so a reader (handoff-vscode) can
/// render a project-defined layer without hardcoding the 6 built-ins.
fn layer_defs_json(
    registry: &LayerRegistry,
    id_prefixes_cfg: &HashMap<String, Vec<String>>,
) -> Value {
    let prefix_table = default_prefix_table(registry, id_prefixes_cfg);
    Value::Array(
        registry
            .all()
            .iter()
            .map(|l| {
                json!({
                    "id": l.id,
                    "display_name": l.display_name,
                    "side": l.side.as_str(),
                    "level": l.level,
                    "pair": l.pair,
                    "id_prefixes": prefix_table.get(&l.id).cloned().unwrap_or_default(),
                    "builtin": l.builtin,
                })
            })
            .collect(),
    )
}

/// The only built-in profile with display-name overrides (wiki/260 §2.1's
/// table: bugfix renames `requirement` to "再現条件" and `acceptance` to
/// "回帰テスト" for display purposes only — the layer *ids* are unchanged).
/// Project-defined `[trace.profiles.<name>]` entries have no config surface
/// for display-name overrides (§2.1's TOML example doesn't offer one), so
/// every other profile name (including custom ones) resolves to an empty
/// map here.
fn profile_display_name_overrides(profile_name: &str) -> Value {
    if profile_name == "bugfix" {
        json!({"requirement": "再現条件", "acceptance": "回帰テスト"})
    } else {
        json!({})
    }
}

fn profile_source_str(source: LayersSource) -> &'static str {
    match source {
        LayersSource::Config | LayersSource::Profile => "config",
        LayersSource::Auto => "auto",
    }
}

/// `profile` (wiki/260 §5.1): the project default profile name (`None` when
/// the default comes from raw `[trace] layers`/auto-detection rather than a
/// named profile, §2.1 規則 2), its source, and every document-level
/// `trace_profile` override (§2.1 規則 1) with its display-name overrides
/// (sorted by `doc` slug for determinism, NFR-004).
fn profile_block_json(loaded: &LoadedTrace, graph: &TraceGraph) -> Value {
    let doc_slug_by_id: HashMap<&str, &str> = loaded
        .docs
        .iter()
        .map(|d| (d.id.as_str(), d.slug.as_str()))
        .collect();

    let mut overrides: Vec<(String, Value)> = loaded
        .trace_input
        .doc_profile_overrides
        .iter()
        .map(|(doc_id, p)| {
            let slug = doc_slug_by_id
                .get(doc_id.as_str())
                .copied()
                .unwrap_or(doc_id.as_str());
            (
                slug.to_string(),
                json!({
                    "doc": slug,
                    "profile": p.name,
                    "display_names": profile_display_name_overrides(&p.name),
                }),
            )
        })
        .collect();
    overrides.sort_by(|a, b| a.0.cmp(&b.0));

    json!({
        "project": loaded.trace_input.project_default_profile_name,
        "source": profile_source_str(graph.in_use_layers().source),
        "overrides": overrides.into_iter().map(|(_, v)| v).collect::<Vec<_>>(),
    })
}

/// `suspect_counts` (wiki/260 §5.1): project-wide totals across every kind,
/// plus the number of distinct items carrying at least one suspect, plus the
/// combined (links + tasks) unbaselined count (§3.2/§4.1 — unbaselined links
/// are never suspects themselves, but are the other half of "is this link
/// trustworthy" a reader needs alongside `suspect_counts`).
fn suspect_counts_json(graph: &TraceGraph) -> Value {
    let mut links = 0usize;
    let mut tasks = 0usize;
    let mut results = 0usize;
    let mut items: HashSet<&str> = HashSet::new();
    for s in graph.suspects() {
        match s.kind {
            SuspectKind::Link => links += 1,
            SuspectKind::Task => tasks += 1,
            SuspectKind::Result => results += 1,
        }
        items.insert(s.item.as_str());
    }
    let unbaselined = graph.unbaselined_counts();
    json!({
        "links": links,
        "tasks": tasks,
        "results": results,
        "items": items.len(),
        "unbaselined": unbaselined.links + unbaselined.tasks,
    })
}

/// `tasks[]` (wiki/260 §3.4/§5.1, M2-07/t360.20.25): every task with at
/// least one `requirement`-type link, via the pure
/// `crate::trace::task_view::compute_task_views`.
fn tasks_block_json(loaded: &LoadedTrace, graph: &TraceGraph) -> Value {
    let views = compute_task_views(&loaded.trace_input, graph);
    Value::Array(
        views
            .iter()
            .map(|v| {
                json!({
                    "id": v.task_id,
                    "layers": v.layers,
                    "blockers": v.blockers,
                })
            })
            .collect(),
    )
}

/// Used by `handle_trace_report` (`handle_trace_slice` runs its own
/// resync-then-build sequence without the §2.5 self-repair): re-syncs any
/// directly-edited layer document (§2.4), self-repairs `SubItem.task_ids`
/// drift (§2.5), then loads a fresh [`LoadedTrace`] and builds the one
/// [`TraceGraph`] this request needs (wiki/240 §5-5: "1リクエスト内でグラフ
/// を1回だけ構築する"). Returns any resync/self-repair warnings alongside it
/// so a caller building its own response can fold them into its own
/// `warnings[]` instead of duplicating this sequence. **Not** called from
/// `handle_trace_record` (t360.13 dev report: measured ~271ms at L scale
/// once wired in there, vs. that op's own 100ms PR-4 budget).
///
/// S1 fix (t360.43 M1 review): also returns the `_trace_report.json`
/// `inputs` fingerprint, captured *after* this function's own writes
/// (resync, self-repair above) but *before* [`load_trace_input`] below (the
/// read that determines this response's/the persisted file's actual
/// content). The pre-fix code computed this fingerprint inside
/// `write_trace_report` instead — i.e. *after* `load_trace_input` had
/// already run — which left a window where a third party's write landing
/// between that read and the fingerprint computation would be folded into
/// the fingerprint (making a reader's later recompute match) without ever
/// being folded into the read content itself (`loaded`/`graph`): a stale
/// `_trace_report.json` that permanently reads as "fresh". Capturing it here
/// instead means the worst a racing write can do is make an already-fresh
/// report compare as stale to a future reader (an extra, harmless
/// recompute) — never the reverse. See
/// `write_trace_report_records_a_pre_read_fingerprint_so_a_racing_write_is_never_masked_as_fresh`
/// for the deterministic reproduction.
fn rebuild_trace_graph(
    handoff: &Path,
    layers_arg: Vec<String>,
) -> Result<(LoadedTrace, TraceGraph, Vec<String>, DerivedInputs)> {
    let mut warnings: Vec<String> = Vec::new();

    resync_direct_edited_layer_docs(handoff, &mut warnings)?;

    let rebuild_outcome = rebuild_item_task_ids_full(handoff, false)?;
    if rebuild_outcome.ran && rebuild_outcome.sub_items_changed > 0 {
        warnings.push(format!(
            "self-repair: rebuilt task_ids for {} SubItem(s) across {} document(s) (drifted from \
             task_links)",
            rebuild_outcome.sub_items_changed, rebuild_outcome.docs_changed
        ));
    }

    let report_inputs = compute_derived_inputs(handoff)?;

    let loaded = load_trace_input(handoff, layers_arg)?;
    warnings.extend(loaded.config_warnings.clone());
    let graph = TraceGraph::build(&loaded.trace_input);
    Ok((loaded, graph, warnings, report_inputs))
}

/// `handoff_trace_report` (wiki/220 §3.2, FR-501/502/108/105/303). Input:
/// `layers?: [string]` (overrides `[trace] layers` config for this call
/// only — an empty/omitted array falls back to config, then auto-detection,
/// same as `crate::trace::engine::resolve_in_use_layers`), `gap_kinds?:
/// [string]` (restricts the `gaps[]` list to these kinds only —
/// `gap_counts` always reports every kind, so a caller can see the full
/// breakdown even while viewing a filtered detail list), `limit?` (default
/// 50, truncates `gaps[]` after kind-filtering), `include_items?: bool`
/// (default false, adds an `items[]` array shaped for the future
/// `_trace_report.json`, t360.13).
///
/// Output: `{trace_layers: {in_use, source}, coverage, gaps, gap_counts,
/// warnings, items?}` (§3.2/§3.4).
///
/// Also (re)writes `.handoff/docs/_trace_report.json` (t360.13, wiki/220
/// §3.4) from the same graph this call already builds for its own response —
/// see [`write_trace_report`]'s doc comment for why this is the derived
/// file's write site and not the frequent `handoff_update_task`/`doc_verify`/
/// `doc_update_section` paths (which refresh only `_requirements_summary.json`,
/// per wiki/220 §2.4 step 7, revised 2026-09-27).
pub fn handle_trace_report(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let layers_arg = string_array_arg(arguments, "layers");
    // A non-empty `layers` argument overrides `[trace] layers` for *this
    // call's response only* (§3.2). The graph built from it is not the
    // project's canonical trace view, and `inputs` (§4.3) does not record
    // the override, so persisting it would leave `_trace_report.json`
    // shaped by an ad-hoc layer set while its fingerprint still reads as
    // fresh to every reader (handoff-vscode would render it as the real
    // V-model view). Only a call without an override (re)writes the file.
    let layers_overridden = !layers_arg.is_empty();
    let (loaded, graph, mut warnings, report_inputs) = rebuild_trace_graph(handoff, layers_arg)?;
    if !layers_overridden {
        let trace_config = read_config(&handoff.join("config.toml"))
            .map(|c| c.trace)
            .unwrap_or_default();
        write_trace_report(handoff, &loaded, &graph, report_inputs, &trace_config)?;
    }

    let gap_kind_filter: Option<HashSet<GapKind>> = arguments
        .get("gap_kinds")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().and_then(parse_gap_kind))
                .collect()
        });
    let limit = arguments
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(50) as usize;
    let include_items = arguments
        .get("include_items")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut filtered_gaps: Vec<&crate::trace::Gap> = graph
        .gaps()
        .iter()
        .filter(|g| {
            gap_kind_filter
                .as_ref()
                .is_none_or(|set| set.contains(&g.kind))
        })
        .collect();
    let matching_total = filtered_gaps.len();
    filtered_gaps.truncate(limit);
    if filtered_gaps.len() < matching_total {
        warnings.push(format!(
            "gaps truncated to limit={limit} of {matching_total} matching entries"
        ));
    }

    let mut gap_counts = Map::new();
    for (kind, count) in graph.gap_counts() {
        gap_counts.insert(gap_kind_str(kind).to_string(), json!(count));
    }

    let mut out = json!({
        "trace_layers": {
            "in_use": graph.in_use_layers().layers,
            "source": graph.in_use_layers().source,
        },
        "coverage": graph.coverage(),
        "gaps": filtered_gaps,
        "gap_counts": Value::Object(gap_counts),
        "warnings": warnings,
    });

    if include_items {
        out["items"] = build_report_items(&loaded, &graph);
    }

    Ok(serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string()))
}

/// Schema version for `.handoff/docs/_trace_report.json` (wiki/220 §3.4).
/// Bump this whenever the persisted shape changes in a way a reader
/// (handoff-vscode, `tests/fixtures/trace/`) must react to.
///
/// M2-07 (wiki/260 §5.1/§11 Q2): bumped `1` -> `2` for the full v2 shape
/// (`layer_defs`, `profile`, `coverage.<layer>.suspect`, `items[]`'s
/// `def_hash`/`coverage`/`suspect`/`reverify`/`approval`/`acceptance`/
/// `implicit_of`/`derived`/`waivers`/`from`, `last_run.stale`,
/// `suspect_counts`, `tasks[]`, `inputs.config_fnv` — `next_actions` is
/// M2-10's addition, not part of this bump). §11 Q2's "v2 を書く MCP の
/// リリースは、handoff-vscode の両対応 reader（t131）のリリース後" gate is a
/// *release* sequencing decision, not an implementation one — this task
/// (M2-07) is explicitly instructed to implement the v2 writer now, before
/// that reader ships, but not to cut a release of it (see this task's dev
/// report).
pub(crate) const TRACE_REPORT_SCHEMA_VERSION: u32 = 2;

fn trace_report_path(handoff: &Path) -> PathBuf {
    crate::storage::docs::docs_dir(handoff).join("_trace_report.json")
}

/// The canonical (unfiltered) `_trace_report.json` body: `trace_layers`,
/// `coverage`, `gaps` (every gap, no `gap_kinds`/`limit` truncation —
/// unlike `handle_trace_report`'s own response, this persisted snapshot is
/// not shaped by whatever filters the *calling* request happened to pass),
/// `gap_counts`, `items` (§3.4, always present — this is always "as if
/// `include_items=true`" regardless of the calling request's own
/// `include_items` argument), and (M2-07, wiki/260 §5.1 v2) `layer_defs`,
/// `profile`, `suspect_counts`, `tasks`. `trace_config` is passed in rather
/// than re-derived from `loaded` because `LoadedTrace` does not itself keep
/// the raw `[trace]` config around (only its already-resolved
/// `layer_registry`/`trace_input`) — callers that already read it once for
/// their own purposes (`handle_trace_report`/CLI `trace report`'s id_prefixes
/// lookup) pass that same value through instead of this function re-reading
/// `config.toml` a second time.
fn build_persisted_trace_report_body(
    loaded: &LoadedTrace,
    graph: &TraceGraph,
    trace_config: &TraceConfig,
) -> Value {
    let mut gap_counts = Map::new();
    for (kind, count) in graph.gap_counts() {
        gap_counts.insert(gap_kind_str(kind).to_string(), json!(count));
    }
    json!({
        "trace_layers": {
            "in_use": graph.in_use_layers().layers,
            "source": graph.in_use_layers().source,
        },
        "layer_defs": layer_defs_json(&loaded.layer_registry, &trace_config.id_prefixes),
        "profile": profile_block_json(loaded, graph),
        // `graph.coverage()`'s `LayerCoverage` already carries `suspect`
        // (M2-05, wiki/260 §3.2) alongside `horizontal`/`vertical`/`state` —
        // no v2-specific change needed here beyond what M2-05 already wired.
        "coverage": graph.coverage(),
        "gaps": graph.gaps(),
        "gap_counts": Value::Object(gap_counts),
        "suspect_counts": suspect_counts_json(graph),
        "items": build_report_items(loaded, graph),
        "tasks": tasks_block_json(loaded, graph),
    })
}

/// Writes `.handoff/docs/_trace_report.json` (t360.13, wiki/220 §3.4): the
/// derived file handoff-vscode's V-model view reads instead of duplicating
/// the derivation engine in TypeScript (NFR-005). Same discipline as
/// `docs::write_requirements_summary` (P-M4, wiki/240 §4): no generated
/// timestamp (would change on every write for no reason), unformatted/
/// compact JSON, and an actual write only happens when the content —
/// including the `inputs` fingerprint (§4.3 r3) — differs from what is
/// already on disk.
///
/// `inputs` is supplied by the caller ([`rebuild_trace_graph`]) rather than
/// computed fresh in here (S1 fix, t360.43 M1 review, pre-fix this called
/// `compute_derived_inputs` itself, right at this point — i.e. *after*
/// `load_trace_input` had already read the corpus this response/file's
/// content is built from). See [`rebuild_trace_graph`]'s doc comment for the
/// race that ordering left open and why the fingerprint must instead be
/// captured after this request's own writes but before that read.
///
/// **Not** wired into the frequent `handoff_update_task` / `handoff_doc_verify`
/// / `handoff_doc_update_section` write paths that also refresh
/// `_requirements_summary.json` on every call — wiki/220 §2.4 step 7
/// (revised 2026-09-27) is explicit that this file is *not* written there
/// ("`_trace_report.json` はここでは書かない。§3.4 の書き込み契機を参照"),
/// unlike an earlier revision of that step's wording — **nor** into
/// `handoff_trace_record` (which does, since t360.43 S3, refresh
/// `_requirements_summary.json` itself — a much cheaper write than this
/// file's full `TraceGraph` build; see `handle_trace_record`). Manager
/// decision (M-S11, t360.13): building a full `TraceGraph` measured
/// ~107-180ms at JA/L scale (t360.10 perf bench) — far beyond PR-1's ≤50ms
/// `update_task` budget and PR-4's per-op budgets for those same hot paths,
/// *and* beyond `trace_record`'s own PR-4 budget (100ms — measured ~271ms at
/// L scale when this was wired into `handle_trace_record` during this
/// task's implementation; reverted once measured, see this task's dev
/// report for the numbers). This is therefore only called from
/// `handle_trace_report` (and CLI `trace report`, which dispatches to the
/// same handler), which is already paying the graph-build cost for its own
/// response regardless. Freshness for readers is still guaranteed by the
/// `inputs` fingerprint: a stale `_trace_report.json` is detectable, and
/// handoff-vscode's design (wiki/100 §3.3) already reacts to that by calling
/// `handoff-mcp trace report` itself — including right after a `trace
/// record` call, which is exactly how a fresh `state` reaches
/// `_trace_report.json` in practice.
fn write_trace_report(
    handoff: &Path,
    loaded: &LoadedTrace,
    graph: &TraceGraph,
    inputs: DerivedInputs,
    trace_config: &TraceConfig,
) -> Result<()> {
    let path = trace_report_path(handoff);

    let mut persisted = build_persisted_trace_report_body(loaded, graph, trace_config);
    persisted["schema_version"] = json!(TRACE_REPORT_SCHEMA_VERSION);
    persisted["inputs"] =
        serde_json::to_value(&inputs).context("failed to serialize trace report inputs")?;

    let existing = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    if existing.as_ref() == Some(&persisted) {
        return Ok(());
    }

    ensure_docs_dir(handoff)?;
    let body =
        serde_json::to_string(&persisted).context("failed to serialize _trace_report.json")?;
    crate::storage::atomic_write(&path, body.as_bytes())
        .context("failed to write _trace_report.json")?;
    record_derived_write_for_test(&path, body.len());
    Ok(())
}

/// Builds `handoff_trace_report(include_items=true)`'s `items[]` (§3.4) —
/// shaped so t360.13 can persist this verbatim (plus `schema_version`/
/// `inputs`) into `_trace_report.json`.
fn build_report_items(loaded: &LoadedTrace, graph: &TraceGraph) -> Value {
    let meta = collect_item_meta(&loaded.docs);
    let tasks_by_id = tasks_by_item(&loaded.trace_input);
    let suspects_by_item = group_suspects_by_item(graph);

    let mut ids: Vec<&String> = meta.keys().collect();
    ids.sort();

    let items: Vec<Value> = ids
        .into_iter()
        .map(|id| {
            let m = &meta[id];
            let tasks: Vec<Value> = tasks_by_id
                .get(id)
                .map(|links| {
                    links
                        .iter()
                        .map(|(task_id, role)| json!({"id": task_id, "role": role}))
                        .collect()
                })
                .unwrap_or_default();
            let last_run = loaded.latest_cache.items.get(id).map(|r| {
                json!({
                    "result": r.result,
                    "executed_at": r.executed_at,
                    "run_id": r.run_id,
                    // wiki/260 §5.1 (M2-07): `true` when the latest recorded
                    // result's definition has since changed underneath it
                    // (a `result`-kind suspect, §3.2/§5.4).
                    "stale": last_run_is_stale(&suspects_by_item, id),
                })
            });
            json!({
                "id": id,
                "layer": m.layer,
                "side": side_str(&loaded.layer_registry, m.layer.as_deref()),
                "title": m.title,
                "state": graph.state(id),
                "refines": m.refines,
                "verifies": m.verifies,
                "tasks": tasks,
                "doc": m.doc_id,
                "seq": m.fragment_seq,
                "sub_item_index": m.sub_item_index,
                "priority": m.priority,
                "dev_stage": m.dev_stage,
                "category": m.category,
                "impl_refs": m.impl_refs,
                "test_refs": m.test_refs,
                "last_run": last_run,
                // wiki/260 §2.1 規則 4: the sorted, deduped effective
                // profile name(s) reached by this item's own tree — empty
                // when no named profile applies anywhere in its reachable
                // root set (`TraceGraph::item_profile`, M2-03).
                "profile": graph.item_profile(id),
                // M2-07 (wiki/260 §5.1 v2) additions below.
                "def_hash": m.def_hash,
                "coverage": {
                    "horizontal": graph.item_horizontal(id),
                    "vertical": graph.item_vertical(id),
                },
                "suspect": item_suspect_json(&suspects_by_item, id),
                "reverify": graph.reverify_items().contains(id.as_str()),
                "approval": approval_str(&m.status),
                "acceptance": acceptance_json(id, &m.acceptance),
                "implicit_of": m.implicit_of,
                "derived": m.derived,
                "waivers": waivers_json(&m.waivers),
                "from": m.from,
            })
        })
        .collect();
    Value::Array(items)
}

/// `handoff_trace_slice` (wiki/220 §3.3, FR-701 — progressive disclosure: "AI
/// のコンテキストを節約するのが目的"). Input: exactly one of `task_id` /
/// `item` (task_id's starting set is every stable_id that task has a
/// `requirement` link to, any role); `direction: "up"|"down"|"both"`
/// (default `"both"`; `up` = `refines` toward upper left-side items plus
/// `verifies` toward the left-side target being verified, `down` = `refines`
/// toward refining children plus the verifiers that verify this item —
/// `crate::trace::TraceGraph::refines_parents`/`verifies_targets` for up,
/// `refines_children`/`verified_by` for down); `depth?` (default unlimited —
/// the BFS below only stops at a cycle, via its own visited set);
/// `expand?: [stable_id]` (only these get a `statement`, re-extracted from
/// their layer document's current body — `SubItem` never stores the parsed
/// statement text, only its hash, so this is the one place that pays a
/// re-parse, and only for the ids actually requested); `max_items?` (default
/// 30).
///
/// Output: `{items: [{id, layer, side, title, state, refines, verifies,
/// tasks, statement?}], truncated, warnings}`. `warnings` (t376 rework,
/// review round 1 MAJOR) carries whatever `resync_direct_edited_layer_docs`
/// produced for *this* call — a `removed: [ids]` notice (§2.4 step 6), an
/// unlinked-reverse-link notice, or a duplicate-id collision notice — the
/// same way `handoff_trace_report`'s `warnings[]` already does. This is the
/// only place a caller can ever observe those warnings: the resync writes
/// straight to disk before this handler returns, so a subsequent
/// `trace_report`/`trace_slice` call has nothing left to resync and would
/// silently see none of them.
pub fn handle_trace_slice(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let task_id = arguments.get("task_id").and_then(|v| v.as_str());
    let item_arg = arguments.get("item").and_then(|v| v.as_str());
    match (task_id, item_arg) {
        (None, None) => anyhow::bail!("either 'task_id' or 'item' is required"),
        (Some(_), Some(_)) => anyhow::bail!("'task_id' and 'item' are mutually exclusive"),
        _ => {}
    }

    let direction = arguments
        .get("direction")
        .and_then(|v| v.as_str())
        .unwrap_or("both");
    if !matches!(direction, "up" | "down" | "both") {
        anyhow::bail!("direction={direction:?} must be one of \"up\", \"down\", \"both\"");
    }
    let depth_limit = arguments
        .get("depth")
        .and_then(|v| v.as_u64())
        .map(|d| d as usize);
    let expand: HashSet<String> = string_array_arg(arguments, "expand").into_iter().collect();
    let max_items = arguments
        .get("max_items")
        .and_then(|v| v.as_u64())
        .unwrap_or(30) as usize;

    let mut warnings: Vec<String> = Vec::new();
    resync_direct_edited_layer_docs(handoff, &mut warnings)?;

    let loaded = load_trace_input(handoff, Vec::new())?;
    warnings.extend(loaded.config_warnings.clone());
    let graph = TraceGraph::build(&loaded.trace_input);
    // Computed before `start_ids` so the `item` branch can reject an unknown
    // stable_id the same way the `task_id` branch rejects an unknown task
    // (previously an unknown `item` silently returned `{items: [],
    // truncated: false}`): an id absent from `meta` has no SubItem at all, so
    // a lookup for it can never be a legitimate "no trace neighborhood"
    // answer. (An existing task with zero requirement links, by contrast,
    // legitimately yields an empty `items[]`.)
    let meta = collect_item_meta(&loaded.docs);

    let start_ids: Vec<String> = if let Some(tid) = task_id {
        if !loaded.tasks.iter().any(|t| t.id == tid) {
            anyhow::bail!("Task not found: {tid}");
        }
        let mut ids: Vec<String> = loaded
            .trace_input
            .task_requirement_links
            .iter()
            .filter(|l| l.task_id == tid)
            .map(|l| l.stable_id.clone())
            .collect();
        ids.sort();
        ids.dedup();
        ids
    } else {
        let item = item_arg.expect("checked above: item is Some when task_id is None");
        if !meta.contains_key(item) {
            anyhow::bail!("Item not found: {item}");
        }
        vec![item.to_string()]
    };

    // Narrow to ids that actually have a SubItem (`meta`) *before* truncating
    // to `max_items` — a dangling reference (e.g. a task's
    // `task_requirement_links` entry pointing at a stable_id whose owning
    // document was since deleted, added straight to `start_ids` above
    // without a `meta` check, since the task side is the requirement-link
    // authority per wiki/220 §2.5 D3) must not consume one of the
    // `max_items` slots that only ever gets rendered for real items anyway
    // (the `meta.get(id)?` filter on `items` below already drops it). Without
    // this, a dangling id sitting early in BFS order could push a real item
    // out of `visible`, so `items.len()` would come back below `max_items`
    // even though more real items were reachable.
    let order: Vec<String> = bfs_slice(&graph, &start_ids, direction, depth_limit)
        .into_iter()
        .filter(|id| meta.contains_key(id))
        .collect();
    let truncated = order.len() > max_items;
    let visible: &[String] = if truncated {
        &order[..max_items]
    } else {
        &order[..]
    };

    let tasks_by_id = tasks_by_item(&loaded.trace_input);
    let id_prefixes = read_config(&handoff.join("config.toml"))
        .map(|c| c.trace.id_prefixes)
        .unwrap_or_default();
    let prefix_table = default_prefix_table(&loaded.layer_registry, &id_prefixes);
    let mut body_cache: HashMap<String, Option<String>> = HashMap::new();
    // wiki/260 §4.11/t360.20.25 (M2-07): `trace_slice`'s own items gain
    // `coverage`/`suspect`/`reverify`/`approval` too, not just the persisted
    // `_trace_report.json` (`build_report_items`) — same per-item fields,
    // computed once over this call's own `graph`.
    let suspects_by_item = group_suspects_by_item(&graph);

    let items: Vec<Value> = visible
        .iter()
        .filter_map(|id| {
            let m = meta.get(id)?;
            let tasks: Vec<Value> = tasks_by_id
                .get(id)
                .map(|links| {
                    links
                        .iter()
                        .map(|(task_id, role)| json!({"id": task_id, "role": role}))
                        .collect()
                })
                .unwrap_or_default();
            let statement = if expand.contains(id) {
                item_statement(handoff, m, id, &prefix_table, &mut body_cache)
            } else {
                None
            };

            let mut obj = Map::new();
            obj.insert("id".to_string(), json!(id));
            obj.insert("layer".to_string(), json!(m.layer));
            obj.insert(
                "side".to_string(),
                json!(side_str(&loaded.layer_registry, m.layer.as_deref())),
            );
            obj.insert("title".to_string(), json!(m.title));
            obj.insert("state".to_string(), json!(graph.state(id)));
            obj.insert("refines".to_string(), json!(m.refines));
            obj.insert("verifies".to_string(), json!(m.verifies));
            obj.insert("tasks".to_string(), json!(tasks));
            // wiki/260 §2.1 規則 4 — same effective-profile accessor
            // `build_report_items` uses for `handoff_trace_report`.
            obj.insert("profile".to_string(), json!(graph.item_profile(id)));
            // wiki/260 §4.11/t360.20.25 (M2-07).
            obj.insert(
                "coverage".to_string(),
                json!({
                    "horizontal": graph.item_horizontal(id),
                    "vertical": graph.item_vertical(id),
                }),
            );
            obj.insert(
                "suspect".to_string(),
                item_suspect_json(&suspects_by_item, id),
            );
            obj.insert(
                "reverify".to_string(),
                json!(graph.reverify_items().contains(id.as_str())),
            );
            obj.insert("approval".to_string(), json!(approval_str(&m.status)));
            if let Some(statement) = statement {
                obj.insert("statement".to_string(), json!(statement));
            }
            Some(Value::Object(obj))
        })
        .collect();

    let out = json!({ "items": items, "truncated": truncated, "warnings": warnings });
    Ok(serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string()))
}

/// Re-extracts `id`'s current `statement` (§2.2/§2.3) by re-parsing its
/// owning document's live body — `SubItem` only ever stores `body_hash`, not
/// the statement text itself (wiki/220 §2.3), so `expand` is the one path
/// that pays this cost, and only for the ids actually requested (progressive
/// disclosure, FR-701). `body_cache` avoids re-reading/re-parsing the same
/// document's body twice within one `handoff_trace_slice` call when several
/// `expand` ids share a document. `None` for a non-layer item (no `layer` to
/// parse against) or when re-parsing no longer finds this id at all (the
/// document changed between `load_trace_input`'s read and this call — same
/// tolerant-of-drift posture as every other read in this handler).
fn item_statement(
    handoff: &Path,
    item: &ItemMeta,
    id: &str,
    prefix_table: &HashMap<String, Vec<String>>,
    body_cache: &mut HashMap<String, Option<String>>,
) -> Option<String> {
    let layer = item.layer.as_deref()?;
    let body = body_cache
        .entry(item.doc_slug.clone())
        .or_insert_with(|| read_doc_body(handoff, &item.doc_slug).ok().flatten())
        .as_ref()?;
    let parsed = parse_layer_body(body, Some(layer), prefix_table);
    parsed
        .items
        .into_iter()
        .find(|parsed_item| parsed_item.id == id)
        .map(|parsed_item| parsed_item.statement)
}

/// BFS over `graph` from `start_ids` walking **one** direction only —
/// `up=true` follows `refines_parents`/`verifies_targets`, `up=false` follows
/// `refines_children`/`verified_by` — stopping at `depth_limit` (`None` =
/// unlimited) or when a node has already been visited within this walk (this
/// is what turns a graph cycle into a terminated traversal rather than an
/// infinite loop). Returns every reached id together with the BFS depth it
/// was first reached at, start ids first at depth 0.
///
/// Kept as a single-direction walk (rather than exploring both directions
/// from every node in one BFS) so `bfs_slice`'s `"both"` case can take the
/// union of two independent one-way walks instead of a single BFS that can
/// turn around partway — e.g. go up to a parent and then back down through
/// every sibling subtree, which used to make `"both"` return the whole
/// connected component instead of "ancestors ∪ descendants" (see
/// `handle_trace_slice`'s doc comment for the up/down rule this must match).
fn bfs_one_direction(
    graph: &TraceGraph,
    start_ids: &[String],
    up: bool,
    depth_limit: Option<usize>,
) -> Vec<(String, usize)> {
    let mut visited: HashSet<String> = HashSet::new();
    let mut order: Vec<(String, usize)> = Vec::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    for id in start_ids {
        if visited.insert(id.clone()) {
            queue.push_back((id.clone(), 0));
        }
    }
    while let Some((id, depth)) = queue.pop_front() {
        order.push((id.clone(), depth));
        let within_depth = depth_limit.is_none_or(|max_depth| depth < max_depth);
        if !within_depth {
            continue;
        }
        let neighbors: Vec<String> = if up {
            graph
                .refines_parents(&id)
                .iter()
                .cloned()
                .chain(graph.verifies_targets(&id).iter().cloned())
                .collect()
        } else {
            graph
                .refines_children(&id)
                .iter()
                .cloned()
                .chain(graph.verified_by(&id).iter().cloned())
                .collect()
        };
        for n in neighbors {
            if visited.insert(n.clone()) {
                queue.push_back((n, depth + 1));
            }
        }
    }
    order
}

/// Merges an up-walk and a down-walk from the same start set into one order
/// (§3.3's `"both"` = "the union" rule): each id keeps the shallowest depth
/// it was reached at across the two walks, ties broken by preferring the
/// up-walk (matches a start id, which is depth 0 in both walks, always
/// resolving to a single "up" entry rather than being duplicated), and
/// entries are otherwise ordered by that walk's own BFS insertion order.
/// `handle_trace_slice` truncates the result to `max_items` and sets
/// `truncated` itself — this always returns the *full* union so truncation
/// only ever drops the farthest-away items, never an arbitrary subset.
fn merge_directed_walks(up: Vec<(String, usize)>, down: Vec<(String, usize)>) -> Vec<String> {
    // (depth, walk (0=up, 1=down), insertion index within that walk)
    let mut best: HashMap<String, (usize, u8, usize)> = HashMap::new();
    for (idx, (id, depth)) in up.into_iter().enumerate() {
        best.entry(id)
            .and_modify(|e| {
                if (depth, 0u8) < (e.0, e.1) {
                    *e = (depth, 0, idx);
                }
            })
            .or_insert((depth, 0, idx));
    }
    for (idx, (id, depth)) in down.into_iter().enumerate() {
        best.entry(id)
            .and_modify(|e| {
                if (depth, 1u8) < (e.0, e.1) {
                    *e = (depth, 1, idx);
                }
            })
            .or_insert((depth, 1, idx));
    }
    let mut entries: Vec<(String, usize, u8, usize)> = best
        .into_iter()
        .map(|(id, (depth, walk, idx))| (id, depth, walk, idx))
        .collect();
    entries.sort_by_key(|(_, depth, walk, idx)| (*depth, *walk, *idx));
    entries.into_iter().map(|(id, ..)| id).collect()
}

/// `direction`'s up/down/both rule (§3.3 — see `handle_trace_slice`'s own doc
/// comment): `"up"`/`"down"` are a single one-way `bfs_one_direction` walk;
/// `"both"` is the union of both walks via `merge_directed_walks`, never a
/// single BFS that explores both directions from every visited node (which
/// can turn around and pull in unrelated siblings — see
/// `bfs_one_direction`'s doc comment).
fn bfs_slice(
    graph: &TraceGraph,
    start_ids: &[String],
    direction: &str,
    depth_limit: Option<usize>,
) -> Vec<String> {
    match direction {
        "up" => bfs_one_direction(graph, start_ids, true, depth_limit)
            .into_iter()
            .map(|(id, _)| id)
            .collect(),
        "down" => bfs_one_direction(graph, start_ids, false, depth_limit)
            .into_iter()
            .map(|(id, _)| id)
            .collect(),
        _ => {
            let up_walk = bfs_one_direction(graph, start_ids, true, depth_limit);
            let down_walk = bfs_one_direction(graph, start_ids, false, depth_limit);
            merge_directed_walks(up_walk, down_walk)
        }
    }
}

#[cfg(test)]
mod tasks_by_item_tests {
    use super::*;
    use crate::trace::TaskRequirementLink;

    /// t360.42 N4 (M1 adversarial review): `tasks_by_item`'s per-stable_id
    /// task list must be sorted by task_id, regardless of the order its
    /// `task_requirement_links` input arrives in — that input's order
    /// ultimately traces back to `collect_all_tasks`'s `read_dir` order,
    /// which is OS/filesystem-dependent, and `_trace_report.json` (a VS Code
    /// contract fixture, t360.13) must be byte-identical across platforms
    /// for identical input state.
    #[test]
    fn sorts_each_stable_ids_task_list_by_task_id() {
        let trace_input = TraceInput {
            task_requirement_links: vec![
                TaskRequirementLink {
                    task_id: "t2".to_string(),
                    stable_id: "REQ-001".to_string(),
                    role: TaskLinkRole::Implements,
                    baseline_hash: None,
                },
                TaskRequirementLink {
                    task_id: "t10".to_string(),
                    stable_id: "REQ-001".to_string(),
                    role: TaskLinkRole::Implements,
                    baseline_hash: None,
                },
                TaskRequirementLink {
                    task_id: "t1".to_string(),
                    stable_id: "REQ-001".to_string(),
                    role: TaskLinkRole::Implements,
                    baseline_hash: None,
                },
            ],
            ..Default::default()
        };

        let by_item = tasks_by_item(&trace_input);

        let tasks = by_item.get("REQ-001").expect("REQ-001 entry");
        let ids: Vec<&str> = tasks.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["t1", "t10", "t2"],
            "task list must be sorted by task_id (lexicographic string sort)"
        );
    }
}

#[cfg(test)]
mod write_trace_report_fingerprint_race_tests {
    use super::*;
    use crate::storage::docs::{ensure_docs_dir, write_doc, DocMetadata};
    use tempfile::TempDir;

    fn handoff(tmp: &TempDir) -> PathBuf {
        let dir = tmp.path().join(".handoff");
        ensure_docs_dir(&dir).unwrap();
        dir
    }

    /// S1 (t360.43, M1 adversarial review): `write_trace_report`'s `inputs`
    /// fingerprint must be captured *before* `load_trace_input`'s read (via
    /// `rebuild_trace_graph`), not after it. Proven by manually replaying
    /// `rebuild_trace_graph`'s exact sequence (same private functions this
    /// file's production code calls) with a deterministic external write
    /// injected at the seam under test — a document added strictly *after*
    /// the fingerprint snapshot but *before* the corpus read that builds
    /// this response's/the persisted file's content, no thread/sleep race
    /// needed since the test itself controls the ordering.
    ///
    /// The persisted file's `inputs` must equal the pre-injection snapshot
    /// (proving it was captured too early to have seen the write), and a
    /// reader recomputing the fingerprint fresh from disk afterward must see
    /// a *mismatch* against what got persisted — i.e. this report is at
    /// worst judged (safely) stale, never wrongly judged fresh despite
    /// having missed `doc-race`. Pre-fix, `write_trace_report` computed
    /// `inputs` itself at write time (i.e. after this same read) — that
    /// version of this test would find `persisted_inputs == fresh_inputs`
    /// (both already reflecting the raced-in document), silently masking
    /// the miss.
    #[test]
    fn write_trace_report_records_a_pre_read_fingerprint_so_a_racing_write_is_never_masked_as_fresh(
    ) {
        let tmp = TempDir::new().unwrap();
        let handoff_dir = handoff(&tmp);

        // Replay `rebuild_trace_graph`'s own sequence by hand so a write can
        // be injected at the exact seam under test.
        let mut warnings = Vec::new();
        resync_direct_edited_layer_docs(&handoff_dir, &mut warnings).unwrap();
        rebuild_item_task_ids_full(&handoff_dir, false).unwrap();
        let report_inputs = compute_derived_inputs(&handoff_dir).unwrap();

        // Deterministic "race": an external write lands strictly after the
        // fingerprint was captured, strictly before the read that builds
        // this response's content.
        let doc = DocMetadata::new(
            "doc-race".to_string(),
            "race".to_string(),
            "Racing doc".to_string(),
            "note".to_string(),
            chrono::Utc::now().to_rfc3339(),
        );
        write_doc(&handoff_dir, &doc).unwrap();

        let loaded = load_trace_input(&handoff_dir, Vec::new()).unwrap();
        let graph = TraceGraph::build(&loaded.trace_input);
        write_trace_report(
            &handoff_dir,
            &loaded,
            &graph,
            report_inputs.clone(),
            &TraceConfig::default(),
        )
        .unwrap();

        let persisted: Value =
            serde_json::from_slice(&std::fs::read(trace_report_path(&handoff_dir)).unwrap())
                .unwrap();
        let persisted_inputs: DerivedInputs =
            serde_json::from_value(persisted["inputs"].clone()).unwrap();
        assert_eq!(
            persisted_inputs, report_inputs,
            "persisted inputs must be the pre-read snapshot, not one that already reflects \
             doc-race"
        );

        let fresh_inputs = compute_derived_inputs(&handoff_dir).unwrap();
        assert_ne!(
            fresh_inputs, persisted_inputs,
            "a reader recomputing the fingerprint after the race must detect this report as \
             stale — never silently treat it as fresh despite having missed doc-race"
        );
    }
}

#[cfg(test)]
mod handle_trace_record_summary_tests {
    use super::*;
    use crate::mcp::handlers::HandlerContext;
    use crate::storage::docs::{
        docs_dir, ensure_docs_dir, write_doc, SubItem, Verification, VerificationItem,
    };
    use tempfile::TempDir;

    fn handoff(tmp: &TempDir) -> PathBuf {
        let dir = tmp.path().join(".handoff");
        ensure_docs_dir(&dir).unwrap();
        dir
    }

    fn ctx(handoff_dir: PathBuf) -> HandlerContext {
        HandlerContext {
            agent_id: None,
            project_dir: handoff_dir.parent().unwrap().to_path_buf(),
            handoff_dir,
        }
    }

    /// S3 (t360.43 M1 review, wiki/220 §3.1: "出力: `{run_id, recorded,
    /// warnings}`。記録後に `_latest.json` と summary を更新する。"):
    /// `handoff_trace_record` must refresh `_requirements_summary.json`
    /// after recording the run — pre-fix, it only ever refreshed
    /// `runs/_latest.json`, leaving the summary's `inputs.runs_*`
    /// fingerprint stale after every recorded run until some unrelated call
    /// happened to touch it.
    #[test]
    fn handle_trace_record_refreshes_the_requirements_summary() {
        let tmp = TempDir::new().unwrap();
        let handoff_dir = handoff(&tmp);

        let mut doc = crate::storage::docs::DocMetadata::new(
            "doc-1".to_string(),
            "req".to_string(),
            "Requirements".to_string(),
            "spec".to_string(),
            chrono::Utc::now().to_rfc3339(),
        );
        doc.verification = Some(Verification {
            status: "in_review".to_string(),
            created_at: "2026-09-20T00:00:00Z".to_string(),
            updated_at: "2026-09-20T00:00:00Z".to_string(),
            items: vec![VerificationItem {
                fragment_seq: Some(1),
                heading: "heading".to_string(),
                status: "pending".to_string(),
                impl_refs: Vec::new(),
                test_refs: Vec::new(),
                reviewer: None,
                verified_at: None,
                notes: String::new(),
                content_hash_at_verify: None,
                category: "requirement".to_string(),
                sub_items: vec![SubItem {
                    index: 0,
                    description: "REQ-001".to_string(),
                    stable_id: Some("REQ-001".to_string()),
                    ..Default::default()
                }],
                label: None,
            }],
        });
        write_doc(&handoff_dir, &doc).unwrap();

        assert!(
            !docs_dir(&handoff_dir)
                .join("_requirements_summary.json")
                .exists(),
            "precondition: no summary before this call"
        );

        let c = ctx(handoff_dir.clone());
        handle_trace_record(
            &c,
            &json!({ "results": [{"item": "REQ-001", "result": "pass"}] }),
        )
        .unwrap();

        let summary_path = docs_dir(&handoff_dir).join("_requirements_summary.json");
        assert!(
            summary_path.exists(),
            "handoff_trace_record must (re)write _requirements_summary.json"
        );
        let summary: Value =
            serde_json::from_str(&std::fs::read_to_string(summary_path).unwrap()).unwrap();
        assert_eq!(summary["total"], 1);
    }

    /// R-05 (wiki/260-vmodel-m2-design.md §2.5's closing rule, M2-04):
    /// `handoff_trace_record` must resync a layer document whose body was
    /// hand-edited directly (bypassing `doc_save`) *before* resolving the
    /// recorded item's `body_hash`/`def_hash` — recording against the
    /// stale, pre-edit stored value (M1's `find_body_hash` bug this task
    /// fixes) would make the freshly-recorded "pass" immediately look like a
    /// stale result once the document is next synced.
    #[test]
    fn handle_trace_record_resyncs_a_hand_edited_layer_doc_before_recording() {
        use crate::mcp::handlers::docs::handle_doc_save;
        use crate::storage::docs::{read_doc_body, read_doc_hashed, write_doc_body};

        let tmp = TempDir::new().unwrap();
        let handoff_dir = handoff(&tmp);
        let c = ctx(handoff_dir.clone());

        let body_v1 = "# Req\n\n### REQ-100 タイトル\n\n本文v1。\n";
        handle_doc_save(
            &c,
            &json!({
                "slug": "req-doc",
                "title": "Req doc",
                "body": body_v1,
                "layer": "requirement",
            }),
        )
        .unwrap();
        let synced_v1 = read_doc_hashed(&handoff_dir, "req-doc").unwrap().unwrap();
        let def_hash_v1 = synced_v1
            .verification
            .unwrap()
            .items
            .iter()
            .flat_map(|i| i.sub_items.iter())
            .find(|s| s.stable_id.as_deref() == Some("REQ-100"))
            .unwrap()
            .def_hash
            .clone()
            .unwrap();

        // Hand-edit the body directly (bypassing doc_save/doc_update_section
        // entirely) — the on-disk `_doc.req-doc.md`'s frontmatter still
        // records the v1 `body_raw_hash`/`def_hash`.
        let body_v2 = "# Req\n\n### REQ-100 タイトル\n\n本文v2（変更後）。\n";
        write_doc_body(&handoff_dir, "req-doc", body_v2).unwrap();
        assert_eq!(
            read_doc_body(&handoff_dir, "req-doc").unwrap().unwrap(),
            body_v2,
            "sanity: the hand edit actually landed on disk"
        );

        handle_trace_record(
            &c,
            &json!({ "results": [{"item": "REQ-100", "result": "pass"}] }),
        )
        .unwrap();

        let latest = crate::storage::runs::sync(&handoff_dir).unwrap();
        let latest_item = latest.items.get("REQ-100").unwrap();
        assert_ne!(
            latest_item.def_hash.as_deref(),
            Some(def_hash_v1.as_str()),
            "the recorded def_hash must reflect the hand-edited v2 body, not the stale v1 value"
        );

        // The document itself must now also be resynced on disk (not just
        // read in-memory for the run) — a later doc_save must not pay a
        // redundant resync for a body it already knows about.
        let resynced = read_doc_hashed(&handoff_dir, "req-doc").unwrap().unwrap();
        let resynced_def_hash = resynced
            .verification
            .unwrap()
            .items
            .iter()
            .flat_map(|i| i.sub_items.iter())
            .find(|s| s.stable_id.as_deref() == Some("REQ-100"))
            .unwrap()
            .def_hash
            .clone()
            .unwrap();
        assert_eq!(
            latest_item.def_hash.as_deref(),
            Some(resynced_def_hash.as_str())
        );
    }

    /// t360.20.29 (M2-S6 reviewer finding): `handoff_trace_record`'s own
    /// per-document resync loop (this file, `handle_trace_record`) must also
    /// resolve a cross-document baseline against an upstream item added to a
    /// *different*, not-yet-resynced layer document by a direct body edit —
    /// the same bug `resolve_upstream_ref_across_corpus`'s fix (`docs.rs`)
    /// closes for the single-`doc_save` path, reproduced here through
    /// `trace_record`'s own resync call instead.
    #[test]
    fn handle_trace_record_resolves_a_cross_document_baseline_against_a_hand_edited_unsynced_upstream_doc(
    ) {
        use crate::mcp::handlers::docs::handle_doc_save;
        use crate::storage::docs::{read_doc_hashed, write_doc_body};

        let tmp = TempDir::new().unwrap();
        let handoff_dir = handoff(&tmp);
        let c = ctx(handoff_dir.clone());

        // req-doc is synced once with only REQ-003 — its stored
        // `verification` has no SubItem for REQ-300 yet.
        let req_body_v1 = "# Req\n\n### REQ-003 ログイン失敗時のロック\n\n本文。\n";
        handle_doc_save(
            &c,
            &json!({
                "slug": "req-doc",
                "title": "Req doc",
                "body": req_body_v1,
                "layer": "requirement",
            }),
        )
        .unwrap();

        // st-doc is synced once with ST-041 carrying no `verifies` yet (so
        // `trace_record`'s `owns` check below finds a known SubItem for it).
        let st_body_v1 = "# System test\n\n### ST-041 新規確認\n\n手順。\n";
        handle_doc_save(
            &c,
            &json!({
                "slug": "st-doc",
                "title": "System test doc",
                "body": st_body_v1,
                "layer": "system_test",
            }),
        )
        .unwrap();

        // Direct body edits (bypassing doc_save/sync entirely) on *both*
        // documents: req-doc gains REQ-300, st-doc gains `verifies: REQ-300`
        // on ST-041. Neither document's stored `verification` reflects these
        // edits yet — `handle_trace_record`'s own resync loop (triggered by
        // recording a result for ST-041) is what discovers both.
        let req_body_v2 =
            "# Req\n\n### REQ-003 ログイン失敗時のロック\n\n本文。\n\n### REQ-300 新しい要件\n\n本文。\n";
        write_doc_body(&handoff_dir, "req-doc", req_body_v2).unwrap();
        let st_body_v2 = "# System test\n\n### ST-041 新規確認\n\n- verifies: REQ-300\n\n手順。\n";
        write_doc_body(&handoff_dir, "st-doc", st_body_v2).unwrap();

        handle_trace_record(
            &c,
            &json!({ "results": [{"item": "ST-041", "result": "pass"}] }),
        )
        .unwrap();

        let st_doc = read_doc_hashed(&handoff_dir, "st-doc").unwrap().unwrap();
        let st_041 = st_doc
            .verification
            .unwrap()
            .items
            .into_iter()
            .flat_map(|i| i.sub_items)
            .find(|s| s.stable_id.as_deref() == Some("ST-041"))
            .unwrap();
        assert!(
            st_041.link_baselines.contains_key("REQ-300"),
            "ST-041's baseline for REQ-300 must be recorded by trace_record's own resync even \
             though req-doc's stored verification had not yet been resynced since REQ-300 was \
             added by a direct body edit (t360.20.29) — got link_baselines={:?}",
            st_041.link_baselines
        );
    }
}

/// t360.20.22 (M2-S2 tester/reviewer/dev B finding, FR-804/E11): every
/// `trace.rs` read path that loads the corpus (`load_trace_input*`, shared by
/// `handoff_trace_report`/`handoff_trace_slice`/`handoff_trace_suspect`/
/// `handoff_trace_impact`) must report a sibling document whose frontmatter
/// fails to parse instead of silently dropping it — the plain `read_all_docs`
/// this file used pre-fix has no way to report it at all.
#[cfg(test)]
mod unreadable_doc_reporting_tests {
    use super::*;
    use crate::mcp::handlers::HandlerContext;
    use crate::storage::docs::{docs_dir, ensure_docs_dir};
    use tempfile::TempDir;

    fn handoff(tmp: &TempDir) -> PathBuf {
        let dir = tmp.path().join(".handoff");
        ensure_docs_dir(&dir).unwrap();
        dir
    }

    fn ctx(handoff_dir: PathBuf) -> HandlerContext {
        HandlerContext {
            agent_id: None,
            project_dir: handoff_dir.parent().unwrap().to_path_buf(),
            handoff_dir,
        }
    }

    #[test]
    fn handle_trace_report_reports_an_unreadable_document_in_warnings() {
        let tmp = TempDir::new().unwrap();
        let handoff_dir = handoff(&tmp);
        // Same corrupt-frontmatter shape as
        // `read_all_docs_with_unreadable_reports_corrupt_frontmatter_alongside_good_docs`
        // (`src/storage/docs/mod.rs`).
        std::fs::write(
            docs_dir(&handoff_dir).join("_doc.broken-trace-sibling.md"),
            "---\nid: doc-broken\ntitle: T\ndoc_type: spec\nscope_paths:\n[]\n\
             created_at: 2026-01-01T00:00:00Z\nupdated_at: 2026-01-01T00:00:00Z\n---\nbody\n",
        )
        .unwrap();

        let c = ctx(handoff_dir);
        let result = handle_trace_report(&c, &json!({})).unwrap();
        let out: Value = serde_json::from_str(&result).unwrap();
        let warnings: Vec<String> = out["warnings"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|v| v.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            warnings.iter().any(|w| w.contains("broken-trace-sibling")),
            "handoff_trace_report must report the unreadable document (FR-804) instead of \
             silently dropping it from read_all_docs — got warnings: {warnings:?}"
        );
    }
}
