//! `handoff_trace_record`/`handoff_trace_report`/`handoff_trace_slice`
//! (wiki/220-vmodel-integration-design.md §2.6/§3.1/§3.2/§3.3, FR-302/501/502/
//! 108/105/303/701) — M1 (t360.8/t360.9/t360.10/t360.11). `handoff_trace_record`
//! records one execution batch (a set of `{item, result}` pairs, e.g. one CI
//! run or one manual verification pass) as a single `runs/<run_id>.json` file
//! and refreshes the derived `runs/_latest.json` cache.
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
    record_derived_write_for_test, sync_layer_items_if_needed, write_requirements_summary,
};
use super::HandlerContext;
use crate::storage::config::read_config;
use crate::storage::docs::layer::builtin_layer;
use crate::storage::docs::layer_parse::{default_prefix_table, parse_layer_body};
use crate::storage::docs::model::{CodeRef, DocMetadata};
use crate::storage::docs::{ensure_docs_dir, read_all_docs, read_doc_body, DocSet};
use crate::storage::runs::{self, is_valid_result, record_run, LatestCache, RunResultInput};
use crate::storage::tasks::{collect_all_tasks, TaskData};
use crate::trace::{adapter, GapKind, TaskLinkRole, TraceGraph, TraceInput};

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

    let docs = read_all_docs(handoff)?;
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
fn resync_direct_edited_layer_docs(handoff: &Path, warnings: &mut Vec<String>) -> Result<()> {
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
    for (doc_id, slug) in layer_docs {
        let Some(body) = read_doc_body(handoff, &slug)? else {
            continue;
        };
        if let Some(doc) = doc_set.get_mut(&doc_id) {
            if sync_layer_items_if_needed(handoff, doc, &body, &now, false, warnings) {
                doc_set.mark_dirty(&doc_id);
                any_synced = true;
            }
        }
    }
    if any_synced {
        doc_set.flush()?;
        write_requirements_summary(handoff, doc_set.docs())?;
    }
    Ok(())
}

/// Loads everything `crate::trace::adapter::build_trace_input` needs from the
/// live storage layer, after `resync_direct_edited_layer_docs` and the §2.5
/// self-repair have already run — shared by `handle_trace_report` and
/// `handle_trace_slice` so both build their one graph (wiki/240 §5-5) from
/// the exact same loading sequence.
struct LoadedTrace {
    docs: Vec<DocMetadata>,
    tasks: Vec<TaskData>,
    latest_cache: LatestCache,
    trace_input: TraceInput,
}

fn load_trace_input(handoff: &Path, layers_arg: Vec<String>) -> Result<LoadedTrace> {
    let docs = read_all_docs(handoff)?;

    let mut raw_tasks = Vec::new();
    collect_all_tasks(&handoff.join("tasks"), &mut raw_tasks)?;
    let tasks: Vec<TaskData> = raw_tasks.into_iter().map(|(data, _status)| data).collect();

    let latest_cache = runs::sync(handoff)?;
    let stable_id_owners = collect_all_stable_ids(&docs);
    let configured_layers = if layers_arg.is_empty() {
        read_config(&handoff.join("config.toml"))
            .map(|c| c.trace.layers)
            .unwrap_or_default()
    } else {
        layers_arg
    };

    let trace_input = adapter::build_trace_input(
        &docs,
        &tasks,
        &latest_cache,
        stable_id_owners,
        configured_layers,
    );
    Ok(LoadedTrace {
        docs,
        tasks,
        latest_cache,
        trace_input,
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
                });
            }
        }
    }
    out
}

/// stable_id -> every `{task_id, role}` pair a `requirement` task link
/// declares for it (wiki/220 §2.5) — shared by `handoff_trace_report`'s and
/// `handoff_trace_slice`'s `tasks[]` output.
fn tasks_by_item(trace_input: &TraceInput) -> HashMap<String, Vec<(String, &'static str)>> {
    let mut out: HashMap<String, Vec<(String, &'static str)>> = HashMap::new();
    for link in &trace_input.task_requirement_links {
        out.entry(link.stable_id.clone())
            .or_default()
            .push((link.task_id.clone(), task_link_role_str(link.role)));
    }
    out
}

fn side_str(layer: Option<&str>) -> Option<&'static str> {
    layer.and_then(builtin_layer).map(|d| d.side.as_str())
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
fn rebuild_trace_graph(
    handoff: &Path,
    layers_arg: Vec<String>,
) -> Result<(LoadedTrace, TraceGraph, Vec<String>)> {
    let mut warnings: Vec<String> = Vec::new();

    resync_direct_edited_layer_docs(handoff, &mut warnings)?;

    let rebuild_outcome = rebuild_item_task_ids_full(handoff)?;
    if rebuild_outcome.ran && rebuild_outcome.sub_items_changed > 0 {
        warnings.push(format!(
            "self-repair: rebuilt task_ids for {} SubItem(s) across {} document(s) (drifted from \
             task_links)",
            rebuild_outcome.sub_items_changed, rebuild_outcome.docs_changed
        ));
    }

    let loaded = load_trace_input(handoff, layers_arg)?;
    let graph = TraceGraph::build(&loaded.trace_input);
    Ok((loaded, graph, warnings))
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
/// file's write site (not the frequent `handoff_update_task`/`doc_verify`/
/// `doc_update_section` paths wiki/220 §2.4 step 7 nominally names).
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
    let (loaded, graph, mut warnings) = rebuild_trace_graph(handoff, layers_arg)?;
    if !layers_overridden {
        write_trace_report(handoff, &loaded, &graph)?;
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
pub(crate) const TRACE_REPORT_SCHEMA_VERSION: u32 = 1;

fn trace_report_path(handoff: &Path) -> PathBuf {
    crate::storage::docs::docs_dir(handoff).join("_trace_report.json")
}

/// The canonical (unfiltered) `_trace_report.json` body: `trace_layers`,
/// `coverage`, `gaps` (every gap, no `gap_kinds`/`limit` truncation —
/// unlike `handle_trace_report`'s own response, this persisted snapshot is
/// not shaped by whatever filters the *calling* request happened to pass),
/// `gap_counts`, and `items` (§3.4, always present — this is always "as if
/// `include_items=true`" regardless of the calling request's own
/// `include_items` argument).
fn build_persisted_trace_report_body(loaded: &LoadedTrace, graph: &TraceGraph) -> Value {
    let mut gap_counts = Map::new();
    for (kind, count) in graph.gap_counts() {
        gap_counts.insert(gap_kind_str(kind).to_string(), json!(count));
    }
    json!({
        "trace_layers": {
            "in_use": graph.in_use_layers().layers,
            "source": graph.in_use_layers().source,
        },
        "coverage": graph.coverage(),
        "gaps": graph.gaps(),
        "gap_counts": Value::Object(gap_counts),
        "items": build_report_items(loaded, graph),
    })
}

/// Writes `.handoff/docs/_trace_report.json` (t360.13, wiki/220 §3.4): the
/// derived file handoff-vscode's V-model view reads instead of duplicating
/// the derivation engine in TypeScript (NFR-005). Same discipline as
/// `docs::write_requirements_summary` (P-M4, wiki/240 §4): no generated
/// timestamp (would change on every write for no reason), unformatted/
/// compact JSON, and an actual write only happens when the content —
/// including the `inputs` fingerprint (§4.3 r3) — differs from what is
/// already on disk. `inputs` is computed fresh right here, i.e. after
/// whatever documents/tasks/runs this request already wrote (§4.3: "その
/// リクエストで書く文書・タスクをすべて書き終えた後に inputs を計算し、
/// 派生ファイルを最後に書く").
///
/// **Not** wired into the frequent `handoff_update_task` / `handoff_doc_verify`
/// / `handoff_doc_update_section` write paths that also refresh
/// `_requirements_summary.json` on every call, even though wiki/220 §2.4
/// step 7 nominally asks for the same trigger ("summary と同じ契機で書く") —
/// **nor** into `handoff_trace_record`. Manager decision (M-S11, t360.13):
/// building a full `TraceGraph` measured ~107-180ms at JA/L scale (t360.10
/// perf bench) — far beyond PR-1's ≤50ms `update_task` budget and PR-4's
/// per-op budgets for those same hot paths, *and* beyond `trace_record`'s
/// own PR-4 budget (100ms — measured ~271ms at L scale when this was wired
/// into `handle_trace_record` during this task's implementation; reverted
/// once measured, see this task's dev report for the numbers). This is
/// therefore only called from `handle_trace_report` (and CLI `trace report`,
/// which dispatches to the same handler), which is already paying the
/// graph-build cost for its own response regardless. Freshness for readers
/// is still guaranteed by the `inputs` fingerprint: a stale
/// `_trace_report.json` is detectable, and handoff-vscode's design (wiki/100
/// §3.3) already reacts to that by calling `handoff-mcp trace report`
/// itself — including right after a `trace record` call, which is exactly
/// how a fresh `state` reaches `_trace_report.json` in practice.
fn write_trace_report(handoff: &Path, loaded: &LoadedTrace, graph: &TraceGraph) -> Result<()> {
    let path = trace_report_path(handoff);
    let inputs = compute_derived_inputs(handoff)?;

    let mut persisted = build_persisted_trace_report_body(loaded, graph);
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
                })
            });
            json!({
                "id": id,
                "layer": m.layer,
                "side": side_str(m.layer.as_deref()),
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
/// tasks, statement?}], truncated}`.
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

    let order = bfs_slice(&graph, &start_ids, direction, depth_limit);
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
    let prefix_table = default_prefix_table(&id_prefixes);
    let mut body_cache: HashMap<String, Option<String>> = HashMap::new();

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
            obj.insert("side".to_string(), json!(side_str(m.layer.as_deref())));
            obj.insert("title".to_string(), json!(m.title));
            obj.insert("state".to_string(), json!(graph.state(id)));
            obj.insert("refines".to_string(), json!(m.refines));
            obj.insert("verifies".to_string(), json!(m.verifies));
            obj.insert("tasks".to_string(), json!(tasks));
            if let Some(statement) = statement {
                obj.insert("statement".to_string(), json!(statement));
            }
            Some(Value::Object(obj))
        })
        .collect();

    let out = json!({ "items": items, "truncated": truncated });
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
