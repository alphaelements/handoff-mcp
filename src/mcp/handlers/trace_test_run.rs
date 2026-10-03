//! `handoff_trace_test_run` (wiki/270-vmodel-m3-design.md §2.6/§4.4, M3-09,
//! FR-304).
//!
//! `action="create"` (write) enumerates the trace graph's items matching
//! `scope` (the same `layers?`/`kinds?`/`assignee?` select vocabulary
//! `handoff_trace_next` already exposes) and persists them as a new
//! `.handoff/trace/test_runs/<test_run_id>.json` definition file
//! ([`crate::storage::test_runs`]). Two cost paths, per MR-03: when
//! `scope.kinds` is given, candidate items are the ones
//! [`derive_next_actions`] would surface for those next-action kinds — this
//! needs the same full `TraceGraph` build `handoff_trace_next` itself pays
//! (PR-7 class). When `scope.kinds` is omitted, candidates are simply every
//! layer item whose effective layer/assignee match `scope.layers`/
//! `scope.assignee` — a `SubItem`-only filter over `read_all_docs` with no
//! graph build at all (PR-4 class).
//!
//! `action="list"` and `action="progress"` are both pure reads: `list` over
//! `test_runs/*.json` ([`crate::storage::test_runs::list_test_runs`]),
//! `progress` over `runs/*.json` filtered by `test_run_id`
//! ([`crate::storage::runs::load_latest_readonly`]'s `by_test_run` map) —
//! dynamically recomputed every call, never cached (§2.6: "テストランの進捗
//! は `runs/*.json` を `test_run_id` でフィルタして動的に計算する").

use std::collections::{HashMap, HashSet};

use anyhow::{bail, Result};
use serde_json::{json, Value};

use super::trace_next::parse_kind as parse_next_action_kind;
use super::trace_readonly::load_trace_input_fully_read_only;
use super::HandlerContext;
use crate::storage::docs::{read_all_docs, DocMetadata};
use crate::storage::runs::load_latest_readonly;
use crate::storage::test_runs::{
    find_test_run, list_test_runs, write_test_run_record, TestRunRecord, TestRunScope,
};
use crate::trace::next::{derive_next_actions, ItemNextMeta, NextActionKind};
use crate::trace::TraceGraph;

fn string_array_arg(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// `scope.kinds`'s own vocabulary is `handoff_trace_next`'s `kinds[]`
/// vocabulary (§2.6's worked example uses `"rerun"`/`"fix_failing"`
/// verbatim) — validated the same way `trace_next::handle_trace_next` does,
/// an unknown kind is a hard error rather than silently dropped.
fn parse_kinds_filter(scope: &Value) -> Result<Vec<NextActionKind>> {
    let raw = string_array_arg(scope, "kinds");
    let mut kinds = Vec::with_capacity(raw.len());
    for s in &raw {
        let kind = parse_next_action_kind(s).ok_or_else(|| {
            anyhow::anyhow!(
                "scope.kinds: unknown kind {s:?}; expected one of fix_failing, review_suspect, \
                 rerun, manual_pending, write_verification, refine, create_task, fix_link, \
                 baseline"
            )
        })?;
        kinds.push(kind);
    }
    Ok(kinds)
}

/// Effective layer of one `SubItem` (`sub.layer.or(doc.layer)`, mirrors
/// `crate::trace::adapter::collect_trace_items`) — duplicated here rather
/// than reused because the fast path (no `kinds`) below deliberately avoids
/// building a full [`crate::trace::types::TraceItemInput`]/[`TraceGraph`] at
/// all (MR-03's whole point).
fn effective_layer(doc: &DocMetadata, sub_layer: &Option<String>) -> Option<String> {
    sub_layer.clone().or_else(|| doc.layer.clone())
}

/// MR-03's fast path: every layer item (`SubItem` with a `stable_id`) whose
/// effective layer/assignee match `layers_filter`/`assignee_filter` — a
/// direct `read_all_docs` scan, no `TraceGraph` build. Used by
/// [`handle_create`] only when `scope.kinds` is empty.
fn candidates_by_layer_and_assignee_only(
    docs: &[DocMetadata],
    layers_filter: &[String],
    assignee_filter: Option<&str>,
) -> Vec<String> {
    let layers_set: Option<HashSet<&str>> = if layers_filter.is_empty() {
        None
    } else {
        Some(layers_filter.iter().map(String::as_str).collect())
    };
    let mut out = Vec::new();
    for doc in docs {
        let Some(v) = &doc.verification else {
            continue;
        };
        for item in &v.items {
            for sub in &item.sub_items {
                let Some(stable_id) = &sub.stable_id else {
                    continue;
                };
                if let Some(set) = &layers_set {
                    let layer = effective_layer(doc, &sub.layer);
                    if !layer.as_deref().is_some_and(|l| set.contains(l)) {
                        continue;
                    }
                }
                if let Some(wanted) = assignee_filter {
                    if sub.assignee.as_deref() != Some(wanted) {
                        continue;
                    }
                }
                out.push(stable_id.clone());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// stable_id -> `{priority, dev_stage, assignee}` from `docs` — mirrors
/// `trace_next.rs`'s own `collect_item_next_meta` (duplicated rather than
/// shared across handler modules, same rationale as that function's own doc
/// comment).
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
                });
            }
        }
    }
    out
}

/// MR-03's slow path: every item [`derive_next_actions`] would surface for
/// `kinds_filter`, narrowed by `layers_filter`/`assignee_filter` — needs the
/// same full `TraceGraph` build `handoff_trace_next` itself pays. Returns the
/// deduplicated, sorted set of `item` ids (an item can appear under more than
/// one action kind within the same call; §2.6's `target_items` is a plain id
/// list, not one entry per kind).
fn candidates_via_next_actions(
    ctx: &HandlerContext,
    layers_filter: &[String],
    assignee_filter: Option<&str>,
    kinds_filter: &[NextActionKind],
) -> Result<(Vec<String>, Vec<String>)> {
    let handoff = &ctx.handoff_dir;
    let read_only = load_trace_input_fully_read_only(handoff, Vec::new())?;
    let graph = TraceGraph::build(&read_only.loaded.trace_input);
    let meta = collect_item_next_meta(&read_only.loaded.docs);

    let kinds_set: HashSet<NextActionKind> = kinds_filter.iter().copied().collect();
    // No `limit` cap for a test run's own candidate enumeration (§4.4 names
    // no limit for create) — usize::MAX is `derive_next_actions`'
    // `truncated` escape hatch for "return everything".
    let (actions, _truncated) = derive_next_actions(
        &graph,
        &read_only.loaded.trace_input,
        &meta,
        None,
        layers_filter,
        assignee_filter,
        Some(&kinds_set),
        usize::MAX,
    );

    let mut ids: Vec<String> = actions.into_iter().filter_map(|a| a.item).collect();
    ids.sort();
    ids.dedup();
    Ok((ids, read_only.warnings))
}

fn handle_create(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let scope = arguments.get("scope").cloned().unwrap_or(json!({}));
    let layers_filter = string_array_arg(&scope, "layers");
    let assignee_filter = scope
        .get("assignee")
        .and_then(Value::as_str)
        .map(str::to_string);
    let kinds_filter = parse_kinds_filter(&scope)?;
    let label = arguments
        .get("label")
        .and_then(Value::as_str)
        .map(str::to_string);

    let (target_items, warnings) = if kinds_filter.is_empty() {
        // PR-4 fast path (MR-03): layers/assignee-only filtering needs no
        // graph build, just a corpus scan.
        let docs = read_all_docs(&ctx.handoff_dir)?;
        let ids = candidates_by_layer_and_assignee_only(
            &docs,
            &layers_filter,
            assignee_filter.as_deref(),
        );
        (ids, Vec::new())
    } else {
        // PR-7 path (MR-03): scope.kinds requires the same full graph build
        // handoff_trace_next itself pays.
        candidates_via_next_actions(
            ctx,
            &layers_filter,
            assignee_filter.as_deref(),
            &kinds_filter,
        )?
    };

    // §2.6's worked example stores `scope.kinds` back in the same lowercase
    // snake_case vocabulary the caller supplied (`"rerun"`, not `"Rerun"`) —
    // `NextActionKind`'s own `Serialize` (`#[serde(rename_all =
    // "snake_case")]`) already gives exactly that string, so round-trip
    // through `serde_json::to_value` instead of `Debug`'s spelling.
    let kinds_strs: Vec<String> = kinds_filter
        .iter()
        .map(|k| {
            serde_json::to_value(k)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default()
        })
        .collect();

    let record = TestRunRecord {
        test_run_id: String::new(), // filled in by write_test_run_record
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        label,
        scope: TestRunScope {
            layers: layers_filter,
            kinds: kinds_strs,
            assignee: assignee_filter,
        },
        total_target_count: target_items.len(),
        target_items: target_items.clone(),
    };

    let persisted = write_test_run_record(&ctx.handoff_dir, record)?;

    let out = json!({
        "test_run_id": persisted.test_run_id,
        "target_items": persisted.target_items,
        "total_target_count": persisted.total_target_count,
        "warnings": warnings,
    });
    Ok(serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string()))
}

fn handle_list(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let limit = arguments.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
    let (entries, truncated) = list_test_runs(&ctx.handoff_dir, limit)?;
    let test_runs: Vec<Value> = entries
        .iter()
        .map(|e| {
            json!({
                "test_run_id": e.test_run_id,
                "created_at": e.created_at,
                "label": e.label,
                "total_target_count": e.total_target_count,
            })
        })
        .collect();
    let out = json!({
        "test_runs": test_runs,
        "truncated": truncated,
    });
    Ok(serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string()))
}

/// Pure aggregation core of `action="progress"` — given the test run's own
/// `target_items` and the current `runs/_latest.json`'s
/// `by_test_run[test_run_id]` scope (already filtered to results recorded
/// against this test run), tallies each spec-named bucket (§2.6/§4.4:
/// `{total, executed, pass, fail, blocked, skipped, not_run,
/// progress_pct}`) plus `remaining` (every target item with no recorded
/// result yet against this test run, `not_run` included — a target that was
/// explicitly recorded `not_run` is "executed" in the sense that someone
/// looked at it and recorded a verdict, but still outstanding work, so it
/// counts in both `not_run` and `remaining`).
fn compute_progress(
    target_items: &[String],
    scoped_results: &HashMap<String, crate::storage::runs::LatestItemResult>,
) -> Value {
    let total = target_items.len();
    let mut pass = 0usize;
    let mut fail = 0usize;
    let mut blocked = 0usize;
    let mut skipped = 0usize;
    let mut not_run = 0usize;
    let mut remaining: Vec<String> = Vec::new();

    for id in target_items {
        match scoped_results.get(id).map(|r| r.result.as_str()) {
            Some("pass") => pass += 1,
            Some("fail") => {
                fail += 1;
                remaining.push(id.clone());
            }
            Some("blocked") => {
                blocked += 1;
                remaining.push(id.clone());
            }
            Some("skipped") => skipped += 1,
            // "not_run"/unrecognized/missing all collapse to the same
            // not_run+remaining bucket — an unrecognized result string
            // should never reach here in practice (`is_valid_result` gates
            // every write path), but failing the whole progress tally over
            // one malformed run file would be worse than treating it as
            // outstanding work.
            Some("not_run") | Some(_) | None => {
                not_run += 1;
                remaining.push(id.clone());
            }
        }
    }
    let executed = total - not_run;
    let progress_pct = if total == 0 {
        0.0
    } else {
        (executed as f64 / total as f64) * 100.0
    };

    json!({
        "total": total,
        "executed": executed,
        "pass": pass,
        "fail": fail,
        "blocked": blocked,
        "skipped": skipped,
        "not_run": not_run,
        "progress_pct": progress_pct,
        "remaining": remaining,
    })
}

fn handle_progress(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let test_run_id = arguments
        .get("test_run_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("'test_run_id' is required"))?;

    let Some(definition) = find_test_run(&ctx.handoff_dir, test_run_id)? else {
        bail!("Test run not found: {test_run_id}");
    };

    let latest = load_latest_readonly(&ctx.handoff_dir)?;
    let empty = HashMap::new();
    let scoped_results = latest.by_test_run.get(test_run_id).unwrap_or(&empty);

    let mut out = compute_progress(&definition.target_items, scoped_results);
    out["test_run_id"] = json!(test_run_id);
    out["warnings"] = json!(Vec::<String>::new());
    Ok(serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string()))
}

pub fn handle_trace_test_run(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let action = arguments
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("create");
    match action {
        "create" => handle_create(ctx, arguments),
        "list" => handle_list(ctx, arguments),
        "progress" => handle_progress(ctx, arguments),
        other => bail!("action={other:?} must be one of \"create\", \"list\", \"progress\""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::runs::LatestItemResult;

    fn result(value: &str) -> LatestItemResult {
        LatestItemResult {
            result: value.to_string(),
            executed_at: "2026-01-01T00:00:00.000Z".to_string(),
            run_id: "r1".to_string(),
            body_hash: None,
            def_hash: None,
            note: String::new(),
            evidence: vec![],
            carried_from: None,
        }
    }

    #[test]
    fn compute_progress_tallies_every_bucket_and_lists_remaining() {
        let target_items = vec![
            "A".to_string(),
            "B".to_string(),
            "C".to_string(),
            "D".to_string(),
            "E".to_string(),
        ];
        let mut scoped = HashMap::new();
        scoped.insert("A".to_string(), result("pass"));
        scoped.insert("B".to_string(), result("fail"));
        scoped.insert("C".to_string(), result("blocked"));
        scoped.insert("D".to_string(), result("skipped"));
        // "E" has no recorded result at all -> not_run.

        let out = compute_progress(&target_items, &scoped);
        assert_eq!(out["total"], 5);
        assert_eq!(out["pass"], 1);
        assert_eq!(out["fail"], 1);
        assert_eq!(out["blocked"], 1);
        assert_eq!(out["skipped"], 1);
        assert_eq!(out["not_run"], 1);
        assert_eq!(out["executed"], 4, "total - not_run");
        let remaining: Vec<String> = out["remaining"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            remaining,
            vec!["B", "C", "E"],
            "fail/blocked/not_run remain"
        );
    }

    #[test]
    fn compute_progress_on_empty_target_items_is_all_zero_not_a_division_error() {
        let out = compute_progress(&[], &HashMap::new());
        assert_eq!(out["total"], 0);
        assert_eq!(out["progress_pct"], 0.0);
        assert!(out["remaining"].as_array().unwrap().is_empty());
    }

    #[test]
    fn compute_progress_explicit_not_run_counts_as_not_run_and_remaining() {
        let target_items = vec!["A".to_string()];
        let mut scoped = HashMap::new();
        scoped.insert("A".to_string(), result("not_run"));
        let out = compute_progress(&target_items, &scoped);
        assert_eq!(out["not_run"], 1);
        assert_eq!(out["executed"], 0);
        assert_eq!(
            out["remaining"].as_array().unwrap(),
            &vec![Value::String("A".to_string())]
        );
    }

    #[test]
    fn compute_progress_all_pass_is_100_percent_with_nothing_remaining() {
        let target_items = vec!["A".to_string(), "B".to_string()];
        let mut scoped = HashMap::new();
        scoped.insert("A".to_string(), result("pass"));
        scoped.insert("B".to_string(), result("pass"));
        let out = compute_progress(&target_items, &scoped);
        assert_eq!(out["progress_pct"], 100.0);
        assert!(out["remaining"].as_array().unwrap().is_empty());
    }
}
