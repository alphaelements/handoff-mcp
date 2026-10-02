//! `handoff_trace_baseline` (wiki/270-vmodel-m3-design.md §2.4/§4.1, M3-06,
//! FR-405 create/list part — `diff` is out of scope for this task).
//!
//! `action="create"` regenerates `.handoff/docs/_trace_report.json` via the
//! exact same write path `handoff_trace_report` uses
//! ([`super::trace::handle_trace_report`] — layer-doc resync, `task_ids`
//! self-repair, and the content-unchanged no-op-write guard all apply
//! unchanged, §4.1: "内部で `trace_report` と同じ書き込みパスで再生成する"),
//! then extracts a lightweight snapshot of its `items`/`coverage`/
//! `gap_counts` into a new `.handoff/trace/baselines/<baseline_id>.json` file
//! ([`crate::storage::baselines`]) and appends its `_index.json` entry
//! (coverage-trend data for handoff-vscode's t143 chart, §5). Because
//! `handle_trace_report`'s own write is a no-op when nothing has changed
//! since the last report, a `create` call right after a fresh `trace_report`
//! pays only the I/O to read that already-current file back (§4.1's PR-4
//! fast path); a stale report pays the same full-graph rebuild
//! `handle_trace_report` always would (PR-7).
//!
//! `action="list"` is a pure read over `_index.json`
//! ([`crate::storage::baselines::list_baselines`]) — see that function's own
//! self-healing rebuild-from-files fallback.

use std::path::Path;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde_json::{json, Value};

use super::HandlerContext;
use crate::storage::baselines::{
    create_baseline, list_baselines, BaselineExecutor, BaselineRecord,
};
use crate::storage::docs::docs_dir;
use crate::storage::git::resolve_tags_at_head;

fn trace_report_path(handoff: &Path) -> std::path::PathBuf {
    docs_dir(handoff).join("_trace_report.json")
}

/// Computes `state_summary` (§2.4: `{passing, failing, blocked, not_run,
/// uncovered}`) by tallying each persisted item's own `state` field — the
/// same classification `_trace_report.json`'s `items[].state` already
/// carries (`crate::trace::types::ItemState`), re-derived here from the JSON
/// rather than re-walking `TraceGraph` a second time.
fn state_summary_from_items(items: &[Value]) -> Value {
    let mut passing = 0u64;
    let mut failing = 0u64;
    let mut blocked = 0u64;
    let mut not_run = 0u64;
    let mut uncovered = 0u64;
    for item in items {
        match item.get("state").and_then(Value::as_str) {
            Some("passing") => passing += 1,
            Some("failing") => failing += 1,
            Some("blocked") => blocked += 1,
            Some("not_run") => not_run += 1,
            Some("uncovered") => uncovered += 1,
            _ => {}
        }
    }
    json!({
        "passing": passing,
        "failing": failing,
        "blocked": blocked,
        "not_run": not_run,
        "uncovered": uncovered,
    })
}

/// Extracts the §2.4 lightweight `items[]` shape from one full
/// `_trace_report.json` item entry — only the fields wiki/270 §2.4's worked
/// example lists (`id`, `layer`, `def_hash`, `refines`, `verifies`,
/// `last_run`, `coverage`, `approval`, `suspect`), deliberately dropping the
/// rest (`title`, `tasks`, `doc`, `profile`, ...) that a baseline snapshot
/// has no stated use for — keeping the file small is the point of
/// "lightweight".
fn lightweight_item(full: &Value) -> Value {
    json!({
        "id": full.get("id").cloned().unwrap_or(Value::Null),
        "layer": full.get("layer").cloned().unwrap_or(Value::Null),
        "def_hash": full.get("def_hash").cloned().unwrap_or(Value::Null),
        "refines": full.get("refines").cloned().unwrap_or(json!([])),
        "verifies": full.get("verifies").cloned().unwrap_or(json!([])),
        "last_run": full.get("last_run").cloned().unwrap_or(Value::Null),
        "coverage": full.get("coverage").cloned().unwrap_or(json!({})),
        "approval": full.get("approval").cloned().unwrap_or(Value::Null),
        "suspect": full.get("suspect").cloned().unwrap_or(Value::Null),
    })
}

fn handle_create(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    // Regenerate `_trace_report.json` via the same write path `trace_report`
    // uses (§4.1) — a no-op write when the content hasn't changed since the
    // last call, so this is cheap on the common "already fresh" path.
    super::trace::handle_trace_report(ctx, &json!({ "include_items": true }))?;

    let report_path = trace_report_path(&ctx.handoff_dir);
    let report_bytes = std::fs::read(&report_path)
        .with_context(|| format!("Failed to read {}", report_path.display()))?;
    let report: Value = serde_json::from_slice(&report_bytes)
        .with_context(|| format!("Failed to parse {}", report_path.display()))?;

    let full_items = report
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let items: Vec<Value> = full_items.iter().map(lightweight_item).collect();
    let state_summary = state_summary_from_items(&full_items);
    let coverage_summary = report.get("coverage").cloned().unwrap_or(json!({}));
    let gap_counts = report.get("gap_counts").cloned().unwrap_or(json!({}));

    let tag = match arguments.get("tag").and_then(Value::as_str) {
        Some(t) => Some(t.to_string()),
        // §2.4: "省略時は resolve_tags_at_head() で自動解決（複数なら最初の1
        // つ、なしなら null）".
        None => resolve_tags_at_head(&ctx.project_dir).into_iter().next(),
    };
    let commit = arguments
        .get("commit")
        .and_then(Value::as_str)
        .map(str::to_string);
    let label = arguments
        .get("label")
        .and_then(Value::as_str)
        .map(str::to_string);
    let executor_kind = arguments
        .get("executor_kind")
        .and_then(Value::as_str)
        .unwrap_or("ai")
        .to_string();
    let executor_id = arguments
        .get("executor_id")
        .and_then(Value::as_str)
        .map(str::to_string);

    let record = BaselineRecord {
        baseline_id: String::new(), // filled in by write_baseline_record
        created_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        executor: BaselineExecutor {
            kind: executor_kind,
            id: executor_id,
        },
        tag,
        commit,
        label,
        total_items: items.len(),
        items,
        coverage_summary: coverage_summary.clone(),
        gap_counts: gap_counts.clone(),
        state_summary: state_summary.clone(),
    };

    let persisted = create_baseline(&ctx.handoff_dir, record)?;

    let out = json!({
        "baseline_id": persisted.baseline_id,
        "tag": persisted.tag,
        "items_count": persisted.total_items,
        "coverage_summary": coverage_summary,
        "warnings": Vec::<String>::new(),
    });
    Ok(serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string()))
}

fn handle_list(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let limit = arguments.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
    let (entries, truncated) = list_baselines(&ctx.handoff_dir, limit)?;
    let baselines: Vec<Value> = entries
        .iter()
        .map(|e| {
            json!({
                "baseline_id": e.baseline_id,
                "created_at": e.created_at,
                "tag": e.tag,
                "label": e.label,
                "total_items": e.total_items,
                "coverage_summary": e.coverage_summary,
            })
        })
        .collect();
    let out = json!({
        "baselines": baselines,
        "truncated": truncated,
    });
    Ok(serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string()))
}

pub fn handle_trace_baseline(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let action = arguments
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("create");
    match action {
        "create" => handle_create(ctx, arguments),
        "list" => handle_list(ctx, arguments),
        other => bail!(
            "action={other:?} must be one of \"create\", \"list\" (\"diff\" not yet implemented)"
        ),
    }
}
