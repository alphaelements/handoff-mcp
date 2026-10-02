//! `handoff_trace_suspect` (wiki/260-vmodel-m2-design.md §3.2/§4.1, M2-05):
//! `action="list"` (read-only, E6 — reuses `trace.rs`'s
//! [`super::trace::load_trace_input`] directly, without the resync-then-
//! self-repair sequence `handle_trace_report`/`handle_trace_slice` run) lists
//! the graph's 3 suspect kinds; `action="clear"` resolves one or more
//! `targets` to a set of suspects and moves each one's baseline forward
//! (a document's `SubItem.link_baselines` entry, a task's
//! `TaskLink.baseline_hash`, or — for a `result` suspect — a new
//! carried-forward `runs/<run_id>.json` entry, D2), writing one audit file
//! per call (`storage::clears`); `action="baseline"` does the same baseline
//! move for every *unbaselined* reference/task link in scope (dry_run by
//! default, §7's migration path — never touches an already-suspect link).
//!
//! R-05 (rework round 1, MAJOR): unlike `action="list"` and
//! `action="baseline"` with `dry_run=true` (both stay on the E6 read-only
//! `load_trace_input` path above, on purpose), `action="clear"` and
//! `action="baseline"` with `dry_run=false` are write actions and call
//! [`super::trace::resync_direct_edited_layer_docs`] first — a layer doc
//! edited directly on disk (VSCode, git pull) since its last sync must never
//! have its stale stored `def_hash` selected as a suspect, recorded as a new
//! baseline, or written into a clear's audit `to_hash`/a carried-forward
//! run's `def_hash`.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::trace::{load_trace_input, load_trace_input_read_only, resync_direct_edited_layer_docs};
use super::HandlerContext;
use crate::storage::clears::{
    write_clear_record, ClearEvidence, ClearExecutor, ClearRecord, ClearedLink, ClearedResult,
    ClearedTask,
};
use crate::storage::docs::model::DocMetadata;
use crate::storage::docs::{write_doc, SubItem};
use crate::storage::runs;
use crate::storage::tasks::{find_task_dir_by_id, read_modify_write_task_locked};
use crate::trace::{Suspect, SuspectKind, TraceGraph, UnbaselinedLink, UnbaselinedTask};

pub fn handle_trace_suspect(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let action = arguments
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("list");
    match action {
        "list" => handle_list(&ctx.handoff_dir, arguments),
        "clear" => handle_clear(ctx, arguments),
        "baseline" => handle_baseline(&ctx.handoff_dir, arguments),
        other => anyhow::bail!("Unknown action {other:?} (expected 'list' | 'clear' | 'baseline')"),
    }
}

fn suspect_kind_str(kind: SuspectKind) -> &'static str {
    match kind {
        SuspectKind::Link => "link",
        SuspectKind::Task => "task",
        SuspectKind::Result => "result",
    }
}

fn parse_suspect_kind(s: &str) -> Option<SuspectKind> {
    match s {
        "link" => Some(SuspectKind::Link),
        "task" => Some(SuspectKind::Task),
        "result" => Some(SuspectKind::Result),
        _ => None,
    }
}

fn suspect_to_json(s: &Suspect) -> Value {
    json!({
        "kind": suspect_kind_str(s.kind),
        "item": s.item,
        "upstream": s.upstream,
        "task": s.task,
        "link_type": s.link_type,
        "baseline_hash": s.baseline_hash,
        "current_hash": s.current_hash,
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

/// item -> effective layer, built once from the loaded `TraceInput` (first
/// occurrence wins — same tie-break as `trace::engine::resolve_items`).
fn item_layers(trace_input: &crate::trace::TraceInput) -> HashMap<&str, Option<&str>> {
    let mut out = HashMap::new();
    for item in &trace_input.items {
        out.entry(item.stable_id.as_str())
            .or_insert(item.layer.as_deref());
    }
    out
}

fn handle_list(handoff: &Path, arguments: &Value) -> Result<String> {
    // M2-08 (wiki/260 §4.1's session-review note, t360.20.8): `list` is
    // read-only (E6) — `load_trace_input_read_only` resyncs a
    // directly-edited layer document in memory only, uses
    // `runs::load_latest_readonly` instead of `runs::sync`, and resolves
    // `task_ids` from the task side without writing — none of it visible to
    // disk.
    let loaded = load_trace_input_read_only(handoff, Vec::new())?;
    let graph = TraceGraph::build(&loaded.trace_input);

    let item_filter = arguments.get("item").and_then(|v| v.as_str());
    let task_filter = arguments.get("task_id").and_then(|v| v.as_str());
    let kinds_filter: Option<HashSet<SuspectKind>> = {
        let raw = string_array_arg(arguments, "kinds");
        if raw.is_empty() {
            None
        } else {
            // An unknown kind is rejected rather than silently dropped: a
            // typo (`"links"`) would otherwise filter everything out and
            // read as "no suspects" — the one answer this tool must never
            // give by accident.
            let mut kinds = HashSet::new();
            for s in &raw {
                let kind = parse_suspect_kind(s).ok_or_else(|| {
                    anyhow::anyhow!("unknown kind {s:?} (expected 'link' | 'task' | 'result')")
                })?;
                kinds.insert(kind);
            }
            Some(kinds)
        }
    };
    let layers_filter: Option<HashSet<String>> = {
        let raw = string_array_arg(arguments, "layers");
        if raw.is_empty() {
            None
        } else {
            Some(raw.into_iter().collect())
        }
    };
    let limit = arguments
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(50) as usize;

    let layers = item_layers(&loaded.trace_input);

    let mut filtered: Vec<&Suspect> = graph
        .suspects()
        .iter()
        .filter(|s| item_filter.is_none_or(|f| s.item == f))
        .filter(|s| task_filter.is_none_or(|f| s.task.as_deref() == Some(f)))
        .filter(|s| {
            kinds_filter
                .as_ref()
                .is_none_or(|kinds| kinds.contains(&s.kind))
        })
        .filter(|s| {
            layers_filter.as_ref().is_none_or(|wanted| {
                layers
                    .get(s.item.as_str())
                    .and_then(|l| *l)
                    .is_some_and(|l| wanted.contains(l))
            })
        })
        .collect();

    let mut counts = json!({"link": 0, "task": 0, "result": 0});
    for s in &filtered {
        let key = suspect_kind_str(s.kind);
        counts[key] = json!(counts[key].as_u64().unwrap_or(0) + 1);
    }

    let truncated = filtered.len() > limit;
    filtered.truncate(limit);

    let unbaselined = graph.unbaselined_counts();
    let mut reverify: Vec<&String> = graph.reverify_items().iter().collect();
    reverify.sort();

    // §3.2/§4.1: this tool's response surfaces `reverify` alongside
    // `suspects`/`unbaselined` (the wiki §4.1 example output predates
    // M2-03's `reverify` derivation landing in the same session; M2-07's
    // remaining scope is only the *derived file*'s own `items[].reverify`
    // field, not this tool's response — see this task's dev report).
    let output = json!({
        "suspects": filtered.iter().map(|s| suspect_to_json(s)).collect::<Vec<_>>(),
        "counts": counts,
        "unbaselined": {"links": unbaselined.links, "tasks": unbaselined.tasks},
        "reverify": reverify,
        "truncated": truncated,
    });
    Ok(serde_json::to_string_pretty(&output)?)
}

fn find_sub_item_mut<'a>(doc: &'a mut DocMetadata, stable_id: &str) -> Option<&'a mut SubItem> {
    let v = doc.verification.as_mut()?;
    for item in &mut v.items {
        for sub in &mut item.sub_items {
            if sub.stable_id.as_deref() == Some(stable_id) {
                return Some(sub);
            }
        }
    }
    None
}

/// Owning `doc_id` per stable_id, from an already-loaded `TraceInput` (first
/// occurrence wins, same as [`item_layers`]).
fn doc_id_by_item(trace_input: &crate::trace::TraceInput) -> HashMap<&str, &str> {
    let mut out = HashMap::new();
    for item in &trace_input.items {
        out.entry(item.stable_id.as_str())
            .or_insert(item.doc_id.as_str());
    }
    out
}

/// Sets `item`'s `link_baselines[upstream] = new_hash` in whichever
/// in-memory `docs_by_id` entry owns it, returning the prior value (for the
/// audit `from_hash`) — `None` (caller skips) when the owning document or
/// SubItem can no longer be found (should not happen: the caller only ever
/// passes an `item` the just-built graph already resolved).
fn set_link_baseline(
    docs_by_id: &mut HashMap<String, DocMetadata>,
    doc_ids: &HashMap<&str, &str>,
    touched: &mut HashSet<String>,
    item: &str,
    upstream: &str,
    new_hash: &str,
) -> Option<String> {
    let doc_id = (*doc_ids.get(item)?).to_string();
    let doc = docs_by_id.get_mut(&doc_id)?;
    let sub = find_sub_item_mut(doc, item)?;
    let from = sub.link_baselines.get(upstream).cloned();
    sub.link_baselines
        .insert(upstream.to_string(), new_hash.to_string());
    touched.insert(doc_id);
    from
}

fn set_task_baseline(
    handoff: &Path,
    task_id: &str,
    item: &str,
    new_hash: &str,
) -> Result<Option<String>> {
    let tasks_dir = handoff.join("tasks");
    let task_dir = find_task_dir_by_id(&tasks_dir, task_id)?
        .ok_or_else(|| anyhow::anyhow!("task '{task_id}' not found"))?;
    let mut from_hash = None;
    read_modify_write_task_locked(&task_dir, |data, status| {
        for link in &mut data.task_links {
            if link.link_type == "requirement" && link.label.as_deref() == Some(item) {
                from_hash = link.baseline_hash.clone();
                link.baseline_hash = Some(new_hash.to_string());
            }
        }
        Ok(status.to_string())
    })?;
    Ok(from_hash)
}

/// One resolved `targets[]` entry (wiki/260 §4.1's 6 shapes) — parsed from
/// the raw JSON once, up front, so `handle_clear` can report an invalid
/// target before doing any work.
#[derive(Debug)]
enum ClearTarget {
    Link {
        item: String,
        upstream: String,
    },
    ItemLinks {
        item: String,
    },
    Upstream {
        upstream: String,
    },
    Task {
        task_id: String,
        item: Option<String>,
    },
    Layer {
        layer: String,
    },
    Result {
        item: String,
    },
}

fn parse_clear_target(v: &Value) -> Result<ClearTarget> {
    let item = v.get("item").and_then(|x| x.as_str()).map(String::from);
    let upstream = v.get("upstream").and_then(|x| x.as_str()).map(String::from);
    let task_id = v.get("task_id").and_then(|x| x.as_str()).map(String::from);
    let layer = v.get("layer").and_then(|x| x.as_str()).map(String::from);
    let result = v.get("result").and_then(|x| x.as_str()).map(String::from);

    if let Some(item) = result {
        return Ok(ClearTarget::Result { item });
    }
    if let Some(task_id) = task_id {
        return Ok(ClearTarget::Task { task_id, item });
    }
    if let (Some(item), Some(upstream)) = (&item, &upstream) {
        return Ok(ClearTarget::Link {
            item: item.clone(),
            upstream: upstream.clone(),
        });
    }
    if let Some(item) = item {
        return Ok(ClearTarget::ItemLinks { item });
    }
    if let Some(upstream) = upstream {
        return Ok(ClearTarget::Upstream { upstream });
    }
    if let Some(layer) = layer {
        return Ok(ClearTarget::Layer { layer });
    }
    anyhow::bail!(
        "invalid clear target {v}: expected one of \
         {{item,upstream}}, {{item}}, {{upstream}}, {{task_id,item?}}, {{layer}}, {{result}}"
    )
}

fn select_suspects<'a>(
    target: &ClearTarget,
    suspects: &'a [Suspect],
    layers: &HashMap<&str, Option<&str>>,
) -> Vec<&'a Suspect> {
    match target {
        ClearTarget::Link { item, upstream } => suspects
            .iter()
            .filter(|s| {
                s.kind == SuspectKind::Link
                    && &s.item == item
                    && s.upstream.as_deref() == Some(upstream.as_str())
            })
            .collect(),
        ClearTarget::ItemLinks { item } => suspects
            .iter()
            .filter(|s| s.kind == SuspectKind::Link && &s.item == item)
            .collect(),
        ClearTarget::Upstream { upstream } => suspects
            .iter()
            .filter(|s| {
                s.kind == SuspectKind::Link && s.upstream.as_deref() == Some(upstream.as_str())
            })
            .collect(),
        ClearTarget::Task { task_id, item } => suspects
            .iter()
            .filter(|s| {
                s.kind == SuspectKind::Task
                    && s.task.as_deref() == Some(task_id.as_str())
                    && item.as_deref().is_none_or(|i| s.item == i)
            })
            .collect(),
        ClearTarget::Layer { layer } => suspects
            .iter()
            .filter(|s| {
                layers
                    .get(s.item.as_str())
                    .and_then(|l| *l)
                    .is_some_and(|l| l == layer)
            })
            .collect(),
        ClearTarget::Result { item } => suspects
            .iter()
            .filter(|s| s.kind == SuspectKind::Result && &s.item == item)
            .collect(),
    }
}

fn handle_clear(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let targets_val = arguments
        .get("targets")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
        .ok_or_else(|| anyhow::anyhow!("'targets' (non-empty array) is required"))?;
    let reason = arguments
        .get("reason")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("'reason' is required"))?;
    let executor_kind = arguments
        .get("executor_kind")
        .and_then(|v| v.as_str())
        .unwrap_or("ai");
    if executor_kind != "ai" && executor_kind != "human" {
        anyhow::bail!("executor_kind={executor_kind:?} must be \"ai\" or \"human\"");
    }
    let executor_id = arguments
        .get("executor_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    let evidence = arguments.get("evidence").map(|e| ClearEvidence {
        run_id: e.get("run_id").and_then(|v| v.as_str()).map(String::from),
        commit: e.get("commit").and_then(|v| v.as_str()).map(String::from),
        note: e.get("note").and_then(|v| v.as_str()).map(String::from),
    });

    let targets: Vec<ClearTarget> = targets_val
        .iter()
        .map(parse_clear_target)
        .collect::<Result<_>>()?;

    // R-05 (rework round 1, MAJOR): `clear` is a write action, so it must not
    // select suspects — or record a baseline/audit `to_hash` — from a stale
    // stored `def_hash`. A layer doc edited directly on disk (VSCode, git
    // pull) since its last sync has to be re-synced (with writes allowed)
    // *before* `load_trace_input` reads it, exactly like `trace_record`
    // (`src/mcp/handlers/trace.rs`) already does for its own `results[].item`
    // set — this call is unbounded (every layer doc, like
    // `trace_report`/`trace_slice`) rather than narrowed to `targets`,
    // because `targets` can name a `layer`/`upstream` spanning documents this
    // handler hasn't resolved yet at this point.
    let mut warnings = Vec::new();
    resync_direct_edited_layer_docs(handoff, &mut warnings)?;

    let loaded = load_trace_input(handoff, Vec::new())?;
    let graph = TraceGraph::build(&loaded.trace_input);
    let layers = item_layers(&loaded.trace_input);
    let doc_ids = doc_id_by_item(&loaded.trace_input);

    let mut selected: Vec<&Suspect> = Vec::new();
    let mut seen: HashSet<(SuspectKind, &str, &str, &str)> = HashSet::new();
    for target in &targets {
        for s in select_suspects(target, graph.suspects(), &layers) {
            let key = (
                s.kind,
                s.item.as_str(),
                s.upstream.as_deref().unwrap_or(""),
                s.task.as_deref().unwrap_or(""),
            );
            if seen.insert(key) {
                selected.push(s);
            }
        }
    }

    if selected.is_empty() {
        warnings.push("no suspects matched the given targets".to_string());
    }

    let mut docs_by_id: HashMap<String, DocMetadata> =
        loaded.docs.into_iter().map(|d| (d.id.clone(), d)).collect();
    let mut touched_docs: HashSet<String> = HashSet::new();

    let mut cleared_links = Vec::new();
    let mut cleared_tasks = Vec::new();
    let mut cleared_results = Vec::new();

    for s in &selected {
        match s.kind {
            SuspectKind::Link => {
                let upstream = s.upstream.clone().unwrap_or_default();
                let from = set_link_baseline(
                    &mut docs_by_id,
                    &doc_ids,
                    &mut touched_docs,
                    &s.item,
                    &upstream,
                    &s.current_hash,
                )
                .unwrap_or_else(|| s.baseline_hash.clone());
                cleared_links.push(ClearedLink {
                    child: s.item.clone(),
                    upstream,
                    link_type: s.link_type.clone().unwrap_or_default(),
                    from_hash: from,
                    to_hash: s.current_hash.clone(),
                });
            }
            SuspectKind::Task => {
                let task_id = s.task.clone().unwrap_or_default();
                let from = set_task_baseline(handoff, &task_id, &s.item, &s.current_hash)?
                    .unwrap_or_else(|| s.baseline_hash.clone());
                cleared_tasks.push(ClearedTask {
                    task: task_id,
                    item: s.item.clone(),
                    from_hash: from,
                    to_hash: s.current_hash.clone(),
                });
            }
            SuspectKind::Result => {
                let latest = loaded.latest_cache.items.get(&s.item);
                let Some(latest) = latest else {
                    warnings.push(format!(
                        "{}: no runs/_latest.json entry found, skipped",
                        s.item
                    ));
                    continue;
                };
                let docs_snapshot: Vec<DocMetadata> = docs_by_id.values().cloned().collect();
                let (run_id, mut run_warnings) = runs::record_carried_result(
                    handoff,
                    &docs_snapshot,
                    &s.item,
                    &latest.result,
                    &latest.run_id,
                    executor_kind,
                    executor_id.as_deref(),
                )?;
                warnings.append(&mut run_warnings);
                cleared_results.push(ClearedResult {
                    item: s.item.clone(),
                    run_id,
                });
            }
        }
    }

    for doc_id in &touched_docs {
        if let Some(doc) = docs_by_id.get(doc_id) {
            write_doc(handoff, doc).with_context(|| format!("Failed to write doc '{doc_id}'"))?;
        }
    }

    let has_writes =
        !cleared_links.is_empty() || !cleared_tasks.is_empty() || !cleared_results.is_empty();
    let clear_id = if has_writes {
        let mut record = ClearRecord {
            clear_id: String::new(),
            cleared_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            executor: ClearExecutor {
                kind: executor_kind.to_string(),
                id: executor_id.clone(),
            },
            reason: reason.to_string(),
            evidence: evidence.unwrap_or_default(),
            links: cleared_links.clone(),
            tasks: cleared_tasks.clone(),
            results: cleared_results.clone(),
        };
        write_clear_record(handoff, &mut record)?;
        Some(record.clear_id)
    } else {
        None
    };

    let output = json!({
        "cleared": {
            "links": cleared_links.len(),
            "tasks": cleared_tasks.len(),
            "results": cleared_results.len(),
        },
        "clear_id": clear_id,
        "warnings": warnings,
    });
    Ok(serde_json::to_string_pretty(&output)?)
}

fn handle_baseline(handoff: &Path, arguments: &Value) -> Result<String> {
    let dry_run = arguments
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let scope_doc = arguments
        .get("scope")
        .and_then(|s| s.get("doc"))
        .and_then(|v| v.as_str());
    let scope_layer = arguments
        .get("scope")
        .and_then(|s| s.get("layer"))
        .and_then(|v| v.as_str());

    let mut warnings = Vec::new();
    // R-05 (rework round 1, MAJOR): only the write path (`dry_run=false`)
    // needs — and is allowed — to resync a directly-edited layer doc before
    // recording a baseline (see `handle_clear`'s matching comment above and
    // `resync_direct_edited_layer_docs`'s doc comment for why `dry_run=true`
    // deliberately skips this, M2-08's scope). `dry_run=true` instead stays
    // on the E6 read-only path (`load_trace_input_read_only`, M2-08): its own
    // in-memory-only resync still reflects a direct edit for this preview,
    // just without ever writing it.
    let loaded = if dry_run {
        load_trace_input_read_only(handoff, Vec::new())?
    } else {
        resync_direct_edited_layer_docs(handoff, &mut warnings)?;
        load_trace_input(handoff, Vec::new())?
    };
    // t360.20.22 (FR-804/E11): fold in `load_trace_input*`'s own
    // `config_warnings` (an unreadable document, an in-memory resync notice)
    // instead of silently dropping them — this response already has a
    // `warnings` field (§4.1's documented `baseline` output shape), unlike
    // `list`'s undocumented-for-warnings shape above.
    warnings.extend(loaded.config_warnings.clone());
    let graph = TraceGraph::build(&loaded.trace_input);
    let layers = item_layers(&loaded.trace_input);
    let doc_ids = doc_id_by_item(&loaded.trace_input);

    // `scope.doc` accepts a document id or slug (the tool description's
    // contract) — resolved to the id `doc_ids` is keyed by. An unmatched
    // value is reported instead of silently previewing/applying 0.
    let scope_doc: Option<String> =
        scope_doc.map(
            |d| match loaded.docs.iter().find(|doc| doc.id == d || doc.slug == d) {
                Some(doc) => doc.id.clone(),
                None => {
                    warnings.push(format!(
                        "scope.doc {d:?} matches no document (by id or slug)"
                    ));
                    d.to_string()
                }
            },
        );
    let scope_doc = scope_doc.as_deref();

    let in_scope_link = |l: &UnbaselinedLink| -> bool {
        scope_doc.is_none_or(|d| doc_ids.get(l.item.as_str()).is_some_and(|id| *id == d))
            && scope_layer.is_none_or(|lay| {
                layers
                    .get(l.item.as_str())
                    .and_then(|x| *x)
                    .is_some_and(|x| x == lay)
            })
    };
    let in_scope_task = |t: &UnbaselinedTask| -> bool {
        scope_doc.is_none_or(|d| doc_ids.get(t.item.as_str()).is_some_and(|id| *id == d))
            && scope_layer.is_none_or(|lay| {
                layers
                    .get(t.item.as_str())
                    .and_then(|x| *x)
                    .is_some_and(|x| x == lay)
            })
    };

    let resolvable_links: Vec<&UnbaselinedLink> = graph
        .unbaselined_links()
        .iter()
        .filter(|l| in_scope_link(l) && l.current_hash.is_some())
        .collect();
    let resolvable_tasks: Vec<&UnbaselinedTask> = graph
        .unbaselined_tasks()
        .iter()
        .filter(|t| in_scope_task(t) && t.current_hash.is_some())
        .collect();

    // Unbaselined entries whose current hash can't be determined yet (e.g.
    // an `X#ACn` reference with no implicit item, or an upstream not synced
    // yet) are counted by `list`'s `unbaselined` but can't be baselined —
    // say so, so the leftover count is explained.
    let unresolvable = graph
        .unbaselined_links()
        .iter()
        .filter(|l| in_scope_link(l) && l.current_hash.is_none())
        .count()
        + graph
            .unbaselined_tasks()
            .iter()
            .filter(|t| in_scope_task(t) && t.current_hash.is_none())
            .count();
    if unresolvable > 0 {
        warnings.push(format!(
            "{unresolvable} unbaselined link(s) skipped: their current hash can't be determined yet"
        ));
    }

    if !dry_run {
        let mut docs_by_id: HashMap<String, DocMetadata> =
            loaded.docs.into_iter().map(|d| (d.id.clone(), d)).collect();
        let mut touched_docs: HashSet<String> = HashSet::new();

        for l in &resolvable_links {
            let hash = l.current_hash.clone().expect("filtered to Some above");
            set_link_baseline(
                &mut docs_by_id,
                &doc_ids,
                &mut touched_docs,
                &l.item,
                &l.upstream,
                &hash,
            );
        }
        for doc_id in &touched_docs {
            if let Some(doc) = docs_by_id.get(doc_id) {
                write_doc(handoff, doc)
                    .with_context(|| format!("Failed to write doc '{doc_id}'"))?;
            }
        }
        for t in &resolvable_tasks {
            let hash = t.current_hash.clone().expect("filtered to Some above");
            if let Err(e) = set_task_baseline(handoff, &t.task_id, &t.item, &hash) {
                warnings.push(format!("{}: {e:#}", t.task_id));
            }
        }
    }

    let output = json!({
        "baselined": {
            "links": resolvable_links.len(),
            "tasks": resolvable_tasks.len(),
            "results": 0,
        },
        "dry_run": dry_run,
        "warnings": warnings,
    });
    Ok(serde_json::to_string_pretty(&output)?)
}

#[cfg(test)]
mod parse_clear_target_tests {
    use super::*;

    #[test]
    fn result_target_takes_priority_over_every_other_key() {
        let t = parse_clear_target(&json!({"result": "REQ-001", "item": "SPEC-001"})).unwrap();
        assert!(matches!(t, ClearTarget::Result { item } if item == "REQ-001"));
    }

    #[test]
    fn task_id_with_item_narrows_to_one_task_link() {
        let t = parse_clear_target(&json!({"task_id": "t1", "item": "REQ-001"})).unwrap();
        assert!(matches!(
            t,
            ClearTarget::Task { task_id, item } if task_id == "t1" && item.as_deref() == Some("REQ-001")
        ));
    }

    #[test]
    fn task_id_alone_selects_every_link_of_that_task() {
        let t = parse_clear_target(&json!({"task_id": "t1"})).unwrap();
        assert!(
            matches!(t, ClearTarget::Task { task_id, item } if task_id == "t1" && item.is_none())
        );
    }

    #[test]
    fn item_and_upstream_together_select_one_exact_link() {
        let t = parse_clear_target(&json!({"item": "SPEC-001", "upstream": "REQ-001"})).unwrap();
        assert!(matches!(
            t,
            ClearTarget::Link { item, upstream } if item == "SPEC-001" && upstream == "REQ-001"
        ));
    }

    #[test]
    fn item_alone_selects_all_of_its_suspect_links() {
        let t = parse_clear_target(&json!({"item": "SPEC-001"})).unwrap();
        assert!(matches!(t, ClearTarget::ItemLinks { item } if item == "SPEC-001"));
    }

    #[test]
    fn upstream_alone_selects_every_link_pointing_at_it() {
        let t = parse_clear_target(&json!({"upstream": "REQ-001"})).unwrap();
        assert!(matches!(t, ClearTarget::Upstream { upstream } if upstream == "REQ-001"));
    }

    #[test]
    fn layer_alone_selects_every_suspect_in_that_layer() {
        let t = parse_clear_target(&json!({"layer": "requirement"})).unwrap();
        assert!(matches!(t, ClearTarget::Layer { layer } if layer == "requirement"));
    }

    #[test]
    fn empty_target_is_rejected() {
        let err = parse_clear_target(&json!({})).unwrap_err();
        assert!(err.to_string().contains("invalid clear target"));
    }
}
