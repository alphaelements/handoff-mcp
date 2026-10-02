//! `handoff_trace_baseline` (wiki/270-vmodel-m3-design.md §2.4/§4.1, M3-06
//! create/list, M3-07 diff — FR-405 in full).
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
//!
//! `action="diff"` (M3-07, FR-405 diff part, §2.4/§4.1) resolves `from`/`to`
//! (each a `baseline_id` or the literal `"current"`) into a
//! [`crate::trace::baseline::BaselineSnapshot`] and hands both to the pure
//! [`crate::trace::baseline::diff_snapshots`] — this handler's own job is
//! only I/O (reading `baselines/<id>.json` or `_trace_report.json`) plus
//! turning an unresolvable `from`/`to` into a `warnings[]` entry rather than
//! a hard error (read-only tools in this module never fail outright on a
//! missing input — see [`handle_list`]'s own fallback discipline).
//! `"current"` reads `_trace_report.json` as-is (no regeneration — unlike
//! `action="create"`, `diff` never writes anything, §4.1: "読み取り専用").

use std::path::Path;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde_json::{json, Value};

use super::HandlerContext;
use crate::storage::baselines::{
    create_baseline, list_baselines, read_baseline, BaselineExecutor, BaselineRecord,
};
use crate::storage::docs::docs_dir;
use crate::storage::git::resolve_tags_at_head;
use crate::trace::baseline::{diff_snapshots, BaselineItemKey, BaselineSnapshot};

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

/// Extracts a [`BaselineSnapshot`] from a [`BaselineRecord`]'s own
/// `items[]`/`coverage_summary`/`state_summary` — the `from`/`to` resolution
/// path for any side naming an actual `baseline_id`.
fn snapshot_from_record(record: &BaselineRecord) -> BaselineSnapshot {
    BaselineSnapshot {
        items: record
            .items
            .iter()
            .map(|full| BaselineItemKey {
                id: full
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                def_hash: full
                    .get("def_hash")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
            .collect(),
        coverage_summary: record.coverage_summary.clone(),
        state_summary: record.state_summary.clone(),
    }
}

/// Resolves one `from`/`to` side of `action="diff"`: either a `baseline_id`
/// (read from `baselines/<id>.json`) or the literal `"current"` (read
/// as-is from `_trace_report.json` — §4.1: diff is read-only, so unlike
/// `action="create"` this never regenerates the report first). Returns
/// `Ok(None)` plus a pushed `warnings` entry when the side cannot be
/// resolved, rather than failing the whole call — same discipline
/// [`handle_list`]'s self-healing fallback uses.
fn resolve_side(
    ctx: &HandlerContext,
    side_name: &str,
    side_value: &str,
    warnings: &mut Vec<String>,
) -> Result<Option<BaselineSnapshot>> {
    if side_value == "current" {
        let report_path = trace_report_path(&ctx.handoff_dir);
        let Ok(report_bytes) = std::fs::read(&report_path) else {
            warnings.push(format!(
                "{side_name}=\"current\": {} does not exist yet (run trace_report or \
                 trace_baseline create first)",
                report_path.display()
            ));
            return Ok(None);
        };
        let report: Value = serde_json::from_slice(&report_bytes)
            .with_context(|| format!("Failed to parse {}", report_path.display()))?;
        let full_items = report
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let items: Vec<BaselineItemKey> = full_items
            .iter()
            .map(|full| BaselineItemKey {
                id: full
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                def_hash: full
                    .get("def_hash")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
            .collect();
        let state_summary = state_summary_from_items(&full_items);
        let coverage_summary = report.get("coverage").cloned().unwrap_or(json!({}));
        return Ok(Some(BaselineSnapshot {
            items,
            coverage_summary,
            state_summary,
        }));
    }

    match read_baseline(&ctx.handoff_dir, side_value)? {
        Some(record) => Ok(Some(snapshot_from_record(&record))),
        None => {
            warnings.push(format!("{side_name}=\"{side_value}\": baseline not found"));
            Ok(None)
        }
    }
}

fn handle_diff(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let from_arg = arguments
        .get("from")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("'from' (baseline_id or \"current\") is required"))?;
    let to_arg = arguments
        .get("to")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("'to' (baseline_id or \"current\") is required"))?;

    let mut warnings: Vec<String> = Vec::new();
    let from_snapshot = resolve_side(ctx, "from", from_arg, &mut warnings)?;
    let to_snapshot = resolve_side(ctx, "to", to_arg, &mut warnings)?;

    let (Some(from_snapshot), Some(to_snapshot)) = (from_snapshot, to_snapshot) else {
        let out = json!({
            "added": Vec::<String>::new(),
            "removed": Vec::<String>::new(),
            "changed": Vec::<Value>::new(),
            "regression": Vec::<Value>::new(),
            "state_changes": json!({}),
            "warnings": warnings,
        });
        return Ok(serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string()));
    };

    let diff = diff_snapshots(&from_snapshot, &to_snapshot);
    let out = json!({
        "added": diff.added,
        "removed": diff.removed,
        "changed": diff.changed,
        "regression": diff.regression,
        "state_changes": diff.state_changes,
        "warnings": warnings,
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
        "diff" => handle_diff(ctx, arguments),
        other => bail!("action={other:?} must be one of \"create\", \"list\", \"diff\""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::handlers::docs::handle_doc_save;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn ctx(handoff: PathBuf) -> HandlerContext {
        HandlerContext {
            agent_id: None,
            project_dir: handoff.parent().unwrap().to_path_buf(),
            handoff_dir: handoff,
        }
    }

    fn setup() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();
        (tmp, handoff)
    }

    /// Creates `slug` on first call, updates its body on every later call
    /// (`doc_save`'s own update path keys off `doc_id`, not `slug` — see
    /// `handle_doc_save`'s own doc comment on why `slug` is ignored on
    /// update). Returns the resolved `doc_id`, cached by the caller across
    /// calls.
    fn save_req_doc(c: &HandlerContext, doc_id: &mut Option<String>, slug: &str, body: &str) {
        let mut args = json!({
            "title": "Requirements",
            "layer": "requirement",
            "body": body,
        });
        match doc_id {
            Some(id) => args["doc_id"] = json!(id),
            None => args["slug"] = json!(slug),
        }
        let out: Value = serde_json::from_str(&handle_doc_save(c, &args).unwrap()).unwrap();
        *doc_id = Some(out["doc_id"].as_str().unwrap().to_string());
    }

    #[test]
    fn diff_requires_both_from_and_to() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff);

        let err = handle_trace_baseline(&c, &json!({"action": "diff", "to": "current"}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("'from'"), "{err}");

        let err = handle_trace_baseline(&c, &json!({"action": "diff", "from": "current"}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("'to'"), "{err}");
    }

    #[test]
    fn diff_reports_a_warning_instead_of_erroring_on_an_unknown_baseline_id() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff);

        let out: Value = serde_json::from_str(
            &handle_trace_baseline(
                &c,
                &json!({"action": "diff", "from": "does-not-exist", "to": "current"}),
            )
            .unwrap(),
        )
        .unwrap();
        let warnings = out["warnings"].as_array().unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.as_str().unwrap().contains("not found")),
            "{out}"
        );
        assert!(out["added"].as_array().unwrap().is_empty());
    }

    #[test]
    fn diff_reports_a_warning_when_current_has_no_trace_report_yet() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff);

        let out: Value = serde_json::from_str(
            &handle_trace_baseline(
                &c,
                &json!({"action": "diff", "from": "current", "to": "current"}),
            )
            .unwrap(),
        )
        .unwrap();
        let warnings = out["warnings"].as_array().unwrap();
        assert!(!warnings.is_empty(), "{out}");
    }

    #[test]
    fn diff_between_two_baselines_reports_added_and_state_changes() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff);
        let mut doc_id = None;
        save_req_doc(
            &c,
            &mut doc_id,
            "req-doc",
            "# Requirements\n\n### REQ-001 First\n\nBody.\n",
        );

        let first: Value = serde_json::from_str(
            &handle_trace_baseline(&c, &json!({"action": "create", "label": "first"})).unwrap(),
        )
        .unwrap();
        let first_id = first["baseline_id"].as_str().unwrap().to_string();

        // Add a second item between the two baselines.
        save_req_doc(
            &c,
            &mut doc_id,
            "req-doc",
            "# Requirements\n\n### REQ-001 First\n\nBody.\n\n### REQ-002 Second\n\nBody.\n",
        );
        let second: Value = serde_json::from_str(
            &handle_trace_baseline(&c, &json!({"action": "create", "label": "second"})).unwrap(),
        )
        .unwrap();
        let second_id = second["baseline_id"].as_str().unwrap().to_string();

        let diff: Value = serde_json::from_str(
            &handle_trace_baseline(
                &c,
                &json!({"action": "diff", "from": first_id, "to": second_id}),
            )
            .unwrap(),
        )
        .unwrap();
        let added = diff["added"].as_array().unwrap();
        assert_eq!(
            added
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["REQ-002"],
            "{diff}"
        );
        assert!(diff["removed"].as_array().unwrap().is_empty());
        // REQ-002 starts uncovered (no verifier yet) -> state_summary's
        // `uncovered` count must have grown by 1 between the two baselines.
        assert_eq!(diff["state_changes"]["uncovered"], 1, "{diff}");
    }

    #[test]
    fn diff_detects_a_changed_def_hash() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff);
        let mut doc_id = None;
        save_req_doc(
            &c,
            &mut doc_id,
            "req-doc",
            "# Requirements\n\n### REQ-001 First\n\nOriginal body.\n",
        );

        let first: Value =
            serde_json::from_str(&handle_trace_baseline(&c, &json!({"action": "create"})).unwrap())
                .unwrap();
        let first_id = first["baseline_id"].as_str().unwrap().to_string();

        save_req_doc(
            &c,
            &mut doc_id,
            "req-doc",
            "# Requirements\n\n### REQ-001 First\n\nChanged body.\n",
        );
        let second: Value =
            serde_json::from_str(&handle_trace_baseline(&c, &json!({"action": "create"})).unwrap())
                .unwrap();
        let second_id = second["baseline_id"].as_str().unwrap().to_string();

        let diff: Value = serde_json::from_str(
            &handle_trace_baseline(
                &c,
                &json!({"action": "diff", "from": first_id, "to": second_id}),
            )
            .unwrap(),
        )
        .unwrap();
        let changed = diff["changed"].as_array().unwrap();
        assert_eq!(changed.len(), 1, "{diff}");
        assert_eq!(changed[0]["id"], "REQ-001");
        assert_ne!(changed[0]["old_hash"], changed[0]["new_hash"]);
    }

    #[test]
    fn diff_against_current_compares_the_live_trace_report() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff);
        let mut doc_id = None;
        save_req_doc(
            &c,
            &mut doc_id,
            "req-doc",
            "# Requirements\n\n### REQ-001 First\n\nBody.\n",
        );

        let baseline: Value =
            serde_json::from_str(&handle_trace_baseline(&c, &json!({"action": "create"})).unwrap())
                .unwrap();
        let baseline_id = baseline["baseline_id"].as_str().unwrap().to_string();

        save_req_doc(
            &c,
            &mut doc_id,
            "req-doc",
            "# Requirements\n\n### REQ-001 First\n\nBody.\n\n### REQ-002 Second\n\nBody.\n",
        );
        // Regenerate _trace_report.json without creating a second baseline.
        super::super::trace::handle_trace_report(&c, &json!({"include_items": true})).unwrap();

        let diff: Value = serde_json::from_str(
            &handle_trace_baseline(
                &c,
                &json!({"action": "diff", "from": baseline_id, "to": "current"}),
            )
            .unwrap(),
        )
        .unwrap();
        let added = diff["added"].as_array().unwrap();
        assert_eq!(
            added
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["REQ-002"],
            "{diff}"
        );
    }
}
