//! `handoff_trace_delta` (wiki/270-vmodel-m3-design.md §2.5/§4.2, M3-08,
//! FR-407) and `handoff_trace_update(propose=true)`'s shared persistence path
//! (§4.3 — see [`create_delta_from_validated_ops`], called from both this
//! module's own `action="create"` and `trace_update::handle_trace_update`).
//!
//! **ops restriction**: a delta may only propose `upsert_item`/`link`/
//! `unlink`/`set` ops — `record` (a result) and `clear_suspect` (a suspect
//! dismissal) are both "already-happened facts", never something pending
//! human approval (§2.5: "これらは『既に起きた事実の記録』であり、pending に
//! して人の承認を待つ対象ではない").
//!
//! **Validation reuse**: rather than re-implementing `trace_update`'s ~1000
//! lines of phase-1 op validation and unified-diff preview generation a
//! second time, `action="create"` delegates to
//! [`trace_update::handle_trace_update`] itself with `dry_run: true` — the
//! exact code path §2.5 calls out ("`trace_update` の phase-1 検証（E15）を
//! 実行し... unified-diff プレビュー") — and persists that dry-run response's
//! own `applied[]` (which already carries each op's preview) as this delta's
//! `previews`. `action="apply"` delegates the same way with `dry_run: false`
//! to run the real write path (§4.2: "`trace_update` と同じ書き込みパスで適
//! 用").
//!
//! **Staleness**: `baseline_hashes` (stable_id -> `def_hash` as of delta
//! creation) is computed once at `create` time from the same phase-1 corpus
//! snapshot `trace_update`'s own dry run just validated against (so a delta
//! targeting an item an earlier op in the same call just created has no
//! baseline hash — those have no "before" state to go stale). `apply`
//! re-reads the corpus fresh and compares; any mismatch is a staleness
//! warning (`force: true` to proceed anyway).

use std::path::Path;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde_json::{json, Value};

use super::trace_update::handle_trace_update;
use super::HandlerContext;
use crate::storage::deltas::{
    list_all_deltas, read_delta, write_delta_record, write_delta_record_in_place, DeltaExecutor,
    DeltaPreview, DeltaRecord,
};
use crate::storage::docs::read_all_docs;

/// The 4 op kinds a delta may propose (§2.5) — `record`/`clear_suspect` are
/// rejected before any validation even runs.
const ALLOWED_DELTA_OPS: [&str; 4] = ["upsert_item", "link", "unlink", "set"];

/// Rejects any op in `ops` that is not one of [`ALLOWED_DELTA_OPS`] — called
/// both from [`create_delta_from_validated_ops`] (this module's own
/// `action="create"`) and from `trace_update::handle_trace_update` up front
/// when `propose=true`, before that function's own phase-1 validation loop
/// even starts (so a `record`/`clear_suspect` op in a `propose` call is
/// rejected immediately, not only after an otherwise-wasted full dry-run
/// pass).
pub(super) fn reject_ops_not_allowed_in_a_delta(ops: &[Value]) -> Result<()> {
    for (i, op) in ops.iter().enumerate() {
        let kind = op.get("op").and_then(Value::as_str).unwrap_or("");
        if !ALLOWED_DELTA_OPS.contains(&kind) {
            bail!(
                "ops[{i}]: op {kind:?} is not allowed in a delta — only {ALLOWED_DELTA_OPS:?} \
                 may be proposed (record/clear_suspect are already-happened facts, never pending \
                 approval, wiki/270 §2.5)"
            );
        }
    }
    Ok(())
}

/// Every `stable_id -> def_hash` currently on disk, read fresh — used both to
/// seed a brand-new delta's `baseline_hashes` (at `create`/`propose` time)
/// and to detect staleness (at `apply` time).
fn current_def_hashes(handoff: &Path) -> Result<std::collections::HashMap<String, String>> {
    let docs = read_all_docs(handoff)?;
    let mut out = std::collections::HashMap::new();
    for d in &docs {
        let Some(v) = &d.verification else { continue };
        for item in &v.items {
            for sub in &item.sub_items {
                if let (Some(id), Some(hash)) = (&sub.stable_id, &sub.def_hash) {
                    out.insert(id.clone(), hash.clone());
                }
            }
        }
    }
    Ok(out)
}

/// Extracts the target stable_id(s) an op's `baseline_hashes` entry should be
/// recorded/checked against — `upsert_item.id`, `link`/`unlink`/`set.item`.
fn op_target_ids(op: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for key in ["id", "item"] {
        if let Some(id) = op.get(key).and_then(Value::as_str) {
            out.push(id.to_string());
        }
    }
    out
}

/// Shared `action="create"` / `trace_update(propose=true)` path (§4.2/§4.3):
/// runs `ops` through `trace_update`'s own `dry_run: true` phase-1 validation
/// and preview generation, then persists the result as a new pending
/// `.handoff/trace/deltas/<delta_id>.json`. `extra_update_args` forwards
/// `task_id` (a link/unlink default) through to the underlying `dry_run`
/// call unchanged. Returns `Err` (nothing written) when validation itself
/// fails — same "first offending op" contract `trace_update` uses.
pub fn create_delta_from_validated_ops(
    ctx: &HandlerContext,
    ops: &[Value],
    description: Option<String>,
    executor_kind: &str,
    executor_id: Option<&str>,
    extra_update_args: &Value,
) -> Result<DeltaRecord> {
    if ops.is_empty() {
        bail!("'ops' must not be empty");
    }
    reject_ops_not_allowed_in_a_delta(ops)?;

    let mut dry_run_args = extra_update_args.clone();
    if !dry_run_args.is_object() {
        dry_run_args = json!({});
    }
    dry_run_args["ops"] = json!(ops);
    dry_run_args["dry_run"] = json!(true);
    dry_run_args["executor_kind"] = json!(executor_kind);
    if let Some(id) = executor_id {
        dry_run_args["executor_id"] = json!(id);
    }

    let dry_run_out = handle_trace_update(ctx, &dry_run_args)?;
    let dry_run_val: Value = serde_json::from_str(&dry_run_out)
        .context("Failed to parse trace_update dry-run response")?;
    if let Some(failed) = dry_run_val.get("failed") {
        let error = failed
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("validation failed")
            .to_string();
        let op_index = failed.get("op_index").and_then(Value::as_u64).unwrap_or(0);
        bail!("ops[{op_index}]: {error}");
    }

    let applied = dry_run_val
        .get("applied")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let current_hashes = current_def_hashes(&ctx.handoff_dir)?;
    let mut baseline_hashes = std::collections::BTreeMap::new();
    for op in ops {
        for id in op_target_ids(op) {
            if let Some(hash) = current_hashes.get(&id) {
                baseline_hashes.insert(id, hash.clone());
            }
        }
    }

    let previews: Vec<DeltaPreview> = applied
        .iter()
        .enumerate()
        .map(|(i, a)| DeltaPreview {
            op_index: i,
            diff: a
                .get("result")
                .and_then(|r| r.get("diff"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            result: a.get("result").cloned().unwrap_or(Value::Null),
        })
        .collect();

    let mut record = DeltaRecord {
        delta_id: String::new(),
        created_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        executor: DeltaExecutor {
            kind: executor_kind.to_string(),
            id: executor_id.map(str::to_string),
        },
        status: "pending".to_string(),
        description,
        ops: ops.to_vec(),
        previews,
        baseline_hashes,
        resolved_at: None,
        resolved_by: None,
        resolution_reason: None,
    };
    write_delta_record(&ctx.handoff_dir, &mut record)?;
    Ok(record)
}

fn handle_create(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let ops = arguments
        .get("ops")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("'ops' (non-empty array) is required"))?;
    let description = arguments
        .get("description")
        .and_then(Value::as_str)
        .map(String::from);
    let executor_kind = arguments
        .get("executor_kind")
        .and_then(Value::as_str)
        .unwrap_or("ai");
    if executor_kind != "ai" && executor_kind != "human" {
        bail!("executor_kind={executor_kind:?} must be \"ai\" or \"human\"");
    }
    let executor_id = arguments.get("executor_id").and_then(Value::as_str);

    let record = create_delta_from_validated_ops(
        ctx,
        &ops,
        description,
        executor_kind,
        executor_id,
        &json!({}),
    )?;

    let previews: Vec<Value> = record
        .previews
        .iter()
        .map(|p| json!({"op_index": p.op_index, "diff": p.diff}))
        .collect();
    let out = json!({
        "delta_id": record.delta_id,
        "ops_count": record.ops.len(),
        "previews": previews,
        "warnings": Vec::<String>::new(),
    });
    Ok(serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string()))
}

/// `true` when any of `record.baseline_hashes` no longer matches the
/// corresponding entry in `current_hashes` (an id with no current hash at
/// all — e.g. deleted — also counts as stale).
fn is_stale(
    record: &DeltaRecord,
    current_hashes: &std::collections::HashMap<String, String>,
) -> bool {
    record.baseline_hashes.iter().any(|(id, baseline_hash)| {
        current_hashes
            .get(id)
            .map(|current| current != baseline_hash)
            .unwrap_or(true)
    })
}

fn handle_list(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let status_filter = arguments
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("pending");
    if !["pending", "applied", "rejected", "all"].contains(&status_filter) {
        bail!("status={status_filter:?} must be one of \"pending\", \"applied\", \"rejected\", \"all\"");
    }
    let limit = arguments.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;

    let mut records = list_all_deltas(&ctx.handoff_dir)?;
    if status_filter != "all" {
        records.retain(|r| r.status == status_filter);
    }
    // Newest first.
    records.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let truncated = records.len() > limit;
    records.truncate(limit);

    let current_hashes = current_def_hashes(&ctx.handoff_dir)?;
    let deltas: Vec<Value> = records
        .iter()
        .map(|r| {
            json!({
                "delta_id": r.delta_id,
                "created_at": r.created_at,
                "status": r.status,
                "description": r.description,
                "ops_count": r.ops.len(),
                "stale": is_stale(r, &current_hashes),
            })
        })
        .collect();

    let out = json!({
        "deltas": deltas,
        "truncated": truncated,
    });
    Ok(serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string()))
}

fn handle_apply(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let delta_id = arguments
        .get("delta_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("'delta_id' is required"))?;
    let force = arguments
        .get("force")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let executor_kind = arguments
        .get("executor_kind")
        .and_then(Value::as_str)
        .unwrap_or("ai");
    if executor_kind != "ai" && executor_kind != "human" {
        bail!("executor_kind={executor_kind:?} must be \"ai\" or \"human\"");
    }
    let executor_id = arguments
        .get("executor_id")
        .and_then(Value::as_str)
        .map(String::from);

    let mut record = read_delta(&ctx.handoff_dir, delta_id)?
        .ok_or_else(|| anyhow::anyhow!("delta '{delta_id}' not found"))?;
    if record.status != "pending" {
        bail!(
            "delta '{delta_id}' is not pending (status={:?}) — only a pending delta can be applied",
            record.status
        );
    }

    let current_hashes = current_def_hashes(&ctx.handoff_dir)?;
    let mut warnings: Vec<String> = Vec::new();
    if is_stale(&record, &current_hashes) {
        if !force {
            bail!(
                "delta '{delta_id}' is stale — one or more target items changed since this delta \
                 was created (use force=true to apply anyway)"
            );
        }
        warnings.push(format!(
            "delta '{delta_id}' was stale (one or more target items changed since creation) — \
             applied anyway because force=true"
        ));
    }

    // op_indices (default: every op) selects *this delta's own* 0-based
    // ops[] positions — not the original caller's `trace_delta(create)` call
    // positions (those are the same thing only for a brand-new delta that
    // was never partially applied before).
    let total_ops = record.ops.len();
    let op_indices: Vec<usize> = match arguments.get("op_indices").and_then(Value::as_array) {
        Some(arr) => {
            let mut idxs: Vec<usize> = arr
                .iter()
                .filter_map(|v| v.as_u64().map(|n| n as usize))
                .collect();
            for &i in &idxs {
                if i >= total_ops {
                    bail!("op_indices: index {i} is out of range (delta has {total_ops} ops)");
                }
            }
            idxs.sort_unstable();
            idxs.dedup();
            idxs
        }
        None => (0..total_ops).collect(),
    };

    let selected_ops: Vec<Value> = op_indices.iter().map(|&i| record.ops[i].clone()).collect();
    let remaining_ops: Vec<Value> = (0..total_ops)
        .filter(|i| !op_indices.contains(i))
        .map(|i| record.ops[i].clone())
        .collect();

    let write_args = json!({
        "ops": selected_ops,
        "dry_run": false,
        "executor_kind": executor_kind,
        "executor_id": executor_id,
    });
    let write_out = handle_trace_update(ctx, &write_args)?;
    let write_val: Value =
        serde_json::from_str(&write_out).context("Failed to parse trace_update write response")?;
    if let Some(failed) = write_val.get("failed") {
        let error = failed
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("apply failed")
            .to_string();
        bail!("delta '{delta_id}': op application failed: {error}");
    }
    if let Some(ws) = write_val.get("warnings").and_then(Value::as_array) {
        for w in ws {
            if let Some(s) = w.as_str() {
                warnings.push(s.to_string());
            }
        }
    }
    let applied_results = write_val
        .get("applied")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    record.status = "applied".to_string();
    record.resolved_at = Some(now.clone());
    record.resolved_by = Some(match &executor_id {
        Some(id) => format!("{executor_kind}:{id}"),
        None => executor_kind.to_string(),
    });
    write_delta_record_in_place(&ctx.handoff_dir, &record)?;

    // §2.5/§4.2: a partial apply (op_indices narrower than the full set)
    // leaves the remaining ops as a brand-new pending delta, renumbered from
    // 0 — the original delta is still marked "applied" (it no longer
    // represents anything actionable on its own; the remainder delta is
    // where the still-pending ops now live).
    let remainder_delta_id = if remaining_ops.is_empty() {
        None
    } else {
        let remainder_previews: Vec<DeltaPreview> = remaining_ops
            .iter()
            .enumerate()
            .filter_map(|(new_idx, op)| {
                record
                    .previews
                    .iter()
                    .find(|p| record.ops.get(p.op_index) == Some(op))
                    .map(|p| DeltaPreview {
                        op_index: new_idx,
                        diff: p.diff.clone(),
                        result: p.result.clone(),
                    })
            })
            .collect();
        let remainder_baseline_hashes: std::collections::BTreeMap<String, String> = remaining_ops
            .iter()
            .flat_map(op_target_ids)
            .filter_map(|id| record.baseline_hashes.get(&id).cloned().map(|h| (id, h)))
            .collect();
        let mut remainder = DeltaRecord {
            delta_id: String::new(),
            created_at: now.clone(),
            executor: record.executor.clone(),
            status: "pending".to_string(),
            description: record
                .description
                .as_ref()
                .map(|d| format!("{d} (remainder after partial apply of {delta_id})")),
            ops: remaining_ops,
            previews: remainder_previews,
            baseline_hashes: remainder_baseline_hashes,
            resolved_at: None,
            resolved_by: None,
            resolution_reason: None,
        };
        write_delta_record(&ctx.handoff_dir, &mut remainder)?;
        Some(remainder.delta_id)
    };

    let out = json!({
        "applied": applied_results,
        "remainder_delta_id": remainder_delta_id,
        "warnings": warnings,
    });
    Ok(serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string()))
}

fn handle_reject(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let delta_id = arguments
        .get("delta_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("'delta_id' is required"))?;
    let reason = arguments
        .get("reason")
        .and_then(Value::as_str)
        .map(String::from);
    let executor_kind = arguments
        .get("executor_kind")
        .and_then(Value::as_str)
        .unwrap_or("ai");
    if executor_kind != "ai" && executor_kind != "human" {
        bail!("executor_kind={executor_kind:?} must be \"ai\" or \"human\"");
    }
    let executor_id = arguments.get("executor_id").and_then(Value::as_str);

    let mut record = read_delta(&ctx.handoff_dir, delta_id)?
        .ok_or_else(|| anyhow::anyhow!("delta '{delta_id}' not found"))?;
    if record.status != "pending" {
        bail!(
            "delta '{delta_id}' is not pending (status={:?}) — only a pending delta can be \
             rejected",
            record.status
        );
    }

    record.status = "rejected".to_string();
    record.resolved_at = Some(Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    record.resolved_by = Some(match executor_id {
        Some(id) => format!("{executor_kind}:{id}"),
        None => executor_kind.to_string(),
    });
    record.resolution_reason = reason;
    write_delta_record_in_place(&ctx.handoff_dir, &record)?;

    let out = json!({
        "delta_id": record.delta_id,
        "status": "rejected",
        "warnings": Vec::<String>::new(),
    });
    Ok(serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string()))
}

pub fn handle_trace_delta(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let action = arguments
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("list");
    match action {
        "create" => handle_create(ctx, arguments),
        "list" => handle_list(ctx, arguments),
        "apply" => handle_apply(ctx, arguments),
        "reject" => handle_reject(ctx, arguments),
        other => {
            bail!("action={other:?} must be one of \"create\", \"list\", \"apply\", \"reject\"")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::handlers::docs::handle_doc_save;
    use crate::mcp::handlers::trace_update::handle_trace_update;
    use crate::storage::docs::{write_doc, DocMetadata as Doc};
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

    fn layer_doc(id: &str, slug: &str, layer: &str) -> Doc {
        let mut doc = Doc::new(
            id.to_string(),
            slug.to_string(),
            format!("Title {id}"),
            "spec".to_string(),
            "2026-09-28T00:00:00Z".to_string(),
        );
        doc.layer = Some(layer.to_string());
        doc
    }

    fn find_sub_item<'a>(doc: &'a Doc, stable_id: &str) -> &'a crate::storage::docs::SubItem {
        doc.verification
            .as_ref()
            .unwrap()
            .items
            .iter()
            .flat_map(|i| &i.sub_items)
            .find(|s| s.stable_id.as_deref() == Some(stable_id))
            .unwrap_or_else(|| panic!("stable_id {stable_id} not found"))
    }

    /// Writes an empty `req-doc` layer document and creates `REQ-001` in it
    /// via a real `trace_update` call (so its `def_hash` is exactly what the
    /// real sync path would compute) — the shared fixture every test below
    /// starts from.
    fn setup_with_req_001(handoff: &std::path::Path) -> HandlerContext {
        let c = ctx(handoff.to_path_buf());
        let doc = layer_doc("doc-req", "req-doc", "requirement");
        write_doc(handoff, &doc).unwrap();
        handle_doc_save(
            &c,
            &json!({"doc_id": "doc-req", "body": "# Requirements\n"}),
        )
        .unwrap();
        handle_trace_update(
            &c,
            &json!({"ops": [{"op": "upsert_item", "doc": "req-doc", "id": "REQ-001",
                "title": "Something", "statement": "A requirement."}]}),
        )
        .unwrap();
        c
    }

    #[test]
    fn create_rejects_record_ops() {
        let (_tmp, handoff) = setup();
        let c = setup_with_req_001(&handoff);

        let result = handle_trace_delta(
            &c,
            &json!({"action": "create", "ops": [{"op": "record", "item": "REQ-001", "result": "pass"}]}),
        );
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not allowed in a delta"), "{err}");
        assert!(list_all_deltas(&handoff).unwrap().is_empty());
    }

    #[test]
    fn create_persists_a_pending_delta_with_previews_and_baseline_hashes() {
        let (_tmp, handoff) = setup();
        let c = setup_with_req_001(&handoff);

        let out: Value = serde_json::from_str(
            &handle_trace_delta(
                &c,
                &json!({
                    "action": "create",
                    "description": "bump priority",
                    "ops": [{"op": "set", "item": "REQ-001", "attrs": {}, "dev_stage": "in_progress"}],
                }),
            )
            .unwrap(),
        )
        .unwrap();
        let delta_id = out["delta_id"].as_str().unwrap().to_string();
        assert_eq!(out["ops_count"], 1);
        assert_eq!(out["previews"].as_array().unwrap().len(), 1);

        let record = read_delta(&handoff, &delta_id).unwrap().unwrap();
        assert_eq!(record.status, "pending");
        assert_eq!(record.description.as_deref(), Some("bump priority"));
        assert_eq!(record.ops.len(), 1);
        assert!(record.baseline_hashes.contains_key("REQ-001"));
    }

    #[test]
    fn create_fails_validation_without_writing_a_delta_file() {
        let (_tmp, handoff) = setup();
        let c = setup_with_req_001(&handoff);

        let result = handle_trace_delta(
            &c,
            &json!({"action": "create", "ops": [{"op": "set", "item": "DOES-NOT-EXIST", "dev_stage": "in_progress"}]}),
        );
        assert!(result.is_err());
        let deltas = list_all_deltas(&handoff).unwrap();
        assert!(deltas.is_empty());
    }

    #[test]
    fn list_defaults_to_pending_and_reports_not_stale_right_after_create() {
        let (_tmp, handoff) = setup();
        let c = setup_with_req_001(&handoff);

        handle_trace_delta(
            &c,
            &json!({"action": "create", "ops": [{"op": "set", "item": "REQ-001", "dev_stage": "in_progress"}]}),
        )
        .unwrap();

        let out: Value =
            serde_json::from_str(&handle_trace_delta(&c, &json!({"action": "list"})).unwrap())
                .unwrap();
        let deltas = out["deltas"].as_array().unwrap();
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0]["status"], "pending");
        assert_eq!(deltas[0]["stale"], false);
    }

    #[test]
    fn list_reports_stale_after_the_target_items_def_hash_changes() {
        let (_tmp, handoff) = setup();
        let c = setup_with_req_001(&handoff);

        let create_out: Value = serde_json::from_str(
            &handle_trace_delta(
                &c,
                &json!({"action": "create", "ops": [{"op": "set", "item": "REQ-001", "dev_stage": "in_progress"}]}),
            )
            .unwrap(),
        )
        .unwrap();
        let delta_id = create_out["delta_id"].as_str().unwrap().to_string();

        // Change REQ-001's body (changes its def_hash) via a direct
        // trace_update call, independent of the pending delta above.
        handle_trace_update(
            &c,
            &json!({"ops": [{"op": "upsert_item", "doc": "req-doc", "id": "REQ-001",
                "statement": "A changed requirement."}]}),
        )
        .unwrap();

        let out: Value =
            serde_json::from_str(&handle_trace_delta(&c, &json!({"action": "list"})).unwrap())
                .unwrap();
        let deltas = out["deltas"].as_array().unwrap();
        let entry = deltas.iter().find(|d| d["delta_id"] == delta_id).unwrap();
        assert_eq!(entry["stale"], true);
    }

    #[test]
    fn apply_writes_the_ops_and_marks_the_delta_applied() {
        let (_tmp, handoff) = setup();
        let c = setup_with_req_001(&handoff);

        let create_out: Value = serde_json::from_str(
            &handle_trace_delta(
                &c,
                &json!({"action": "create", "ops": [{"op": "set", "item": "REQ-001", "dev_stage": "in_progress"}]}),
            )
            .unwrap(),
        )
        .unwrap();
        let delta_id = create_out["delta_id"].as_str().unwrap().to_string();

        let apply_out: Value = serde_json::from_str(
            &handle_trace_delta(&c, &json!({"action": "apply", "delta_id": delta_id})).unwrap(),
        )
        .unwrap();
        assert_eq!(apply_out["applied"].as_array().unwrap().len(), 1);
        assert!(apply_out["remainder_delta_id"].is_null());

        let record = read_delta(&handoff, &delta_id).unwrap().unwrap();
        assert_eq!(record.status, "applied");
        assert!(record.resolved_at.is_some());

        let doc = crate::storage::docs::read_doc(&handoff, "req-doc")
            .unwrap()
            .unwrap();
        assert_eq!(
            find_sub_item(&doc, "REQ-001").dev_stage.as_deref(),
            Some("in_progress")
        );
    }

    #[test]
    fn apply_rejects_a_stale_delta_without_force() {
        let (_tmp, handoff) = setup();
        let c = setup_with_req_001(&handoff);

        let create_out: Value = serde_json::from_str(
            &handle_trace_delta(
                &c,
                &json!({"action": "create", "ops": [{"op": "set", "item": "REQ-001", "dev_stage": "in_progress"}]}),
            )
            .unwrap(),
        )
        .unwrap();
        let delta_id = create_out["delta_id"].as_str().unwrap().to_string();

        handle_trace_update(
            &c,
            &json!({"ops": [{"op": "upsert_item", "doc": "req-doc", "id": "REQ-001",
                "statement": "A changed requirement."}]}),
        )
        .unwrap();

        let result = handle_trace_delta(&c, &json!({"action": "apply", "delta_id": delta_id}));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("stale"));

        // force=true applies it anyway.
        let apply_out: Value = serde_json::from_str(
            &handle_trace_delta(
                &c,
                &json!({"action": "apply", "delta_id": delta_id, "force": true}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(apply_out["applied"].as_array().unwrap().len(), 1);
        let warnings = apply_out["warnings"].as_array().unwrap();
        assert!(warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("stale")));
    }

    #[test]
    fn apply_with_op_indices_partially_applies_and_creates_a_renumbered_remainder_delta() {
        let (_tmp, handoff) = setup();
        let c = setup_with_req_001(&handoff);
        // A second item to target with op 1.
        handle_trace_update(
            &c,
            &json!({"ops": [{"op": "upsert_item", "doc": "req-doc", "id": "REQ-002",
                "title": "Second", "statement": "Another requirement."}]}),
        )
        .unwrap();

        let create_out: Value = serde_json::from_str(
            &handle_trace_delta(
                &c,
                &json!({
                    "action": "create",
                    "ops": [
                        {"op": "set", "item": "REQ-001", "dev_stage": "in_progress"},
                        {"op": "set", "item": "REQ-002", "dev_stage": "in_progress"},
                    ],
                }),
            )
            .unwrap(),
        )
        .unwrap();
        let delta_id = create_out["delta_id"].as_str().unwrap().to_string();

        let apply_out: Value = serde_json::from_str(
            &handle_trace_delta(
                &c,
                &json!({"action": "apply", "delta_id": delta_id, "op_indices": [0]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(apply_out["applied"].as_array().unwrap().len(), 1);
        let remainder_id = apply_out["remainder_delta_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_ne!(remainder_id, delta_id);

        let original = read_delta(&handoff, &delta_id).unwrap().unwrap();
        assert_eq!(original.status, "applied");

        let remainder = read_delta(&handoff, &remainder_id).unwrap().unwrap();
        assert_eq!(remainder.status, "pending");
        assert_eq!(remainder.ops.len(), 1);
        // Renumbered from 0.
        assert_eq!(remainder.previews[0].op_index, 0);
        assert_eq!(remainder.ops[0]["item"], "REQ-002");

        // REQ-001 was applied, REQ-002 was not.
        let doc = crate::storage::docs::read_doc(&handoff, "req-doc")
            .unwrap()
            .unwrap();
        assert_eq!(
            find_sub_item(&doc, "REQ-001").dev_stage.as_deref(),
            Some("in_progress")
        );
        assert_eq!(
            find_sub_item(&doc, "REQ-002").dev_stage.as_deref(),
            Some("not_started")
        );

        // The remainder delta can itself be applied later.
        let apply_remainder_out: Value = serde_json::from_str(
            &handle_trace_delta(&c, &json!({"action": "apply", "delta_id": remainder_id})).unwrap(),
        )
        .unwrap();
        assert_eq!(apply_remainder_out["applied"].as_array().unwrap().len(), 1);
        let doc = crate::storage::docs::read_doc(&handoff, "req-doc")
            .unwrap()
            .unwrap();
        assert_eq!(
            find_sub_item(&doc, "REQ-002").dev_stage.as_deref(),
            Some("in_progress")
        );
    }

    #[test]
    fn apply_rejects_an_already_applied_delta() {
        let (_tmp, handoff) = setup();
        let c = setup_with_req_001(&handoff);

        let create_out: Value = serde_json::from_str(
            &handle_trace_delta(
                &c,
                &json!({"action": "create", "ops": [{"op": "set", "item": "REQ-001", "dev_stage": "in_progress"}]}),
            )
            .unwrap(),
        )
        .unwrap();
        let delta_id = create_out["delta_id"].as_str().unwrap().to_string();
        handle_trace_delta(&c, &json!({"action": "apply", "delta_id": delta_id})).unwrap();

        let result = handle_trace_delta(&c, &json!({"action": "apply", "delta_id": delta_id}));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not pending"));
    }

    #[test]
    fn reject_marks_the_delta_rejected_with_a_reason_and_blocks_a_later_apply() {
        let (_tmp, handoff) = setup();
        let c = setup_with_req_001(&handoff);

        let create_out: Value = serde_json::from_str(
            &handle_trace_delta(
                &c,
                &json!({"action": "create", "ops": [{"op": "set", "item": "REQ-001", "dev_stage": "in_progress"}]}),
            )
            .unwrap(),
        )
        .unwrap();
        let delta_id = create_out["delta_id"].as_str().unwrap().to_string();

        let reject_out: Value = serde_json::from_str(
            &handle_trace_delta(
                &c,
                &json!({"action": "reject", "delta_id": delta_id, "reason": "not needed"}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(reject_out["status"], "rejected");

        let record = read_delta(&handoff, &delta_id).unwrap().unwrap();
        assert_eq!(record.status, "rejected");
        assert_eq!(record.resolution_reason.as_deref(), Some("not needed"));

        let result = handle_trace_delta(&c, &json!({"action": "apply", "delta_id": delta_id}));
        assert!(result.is_err());
    }

    #[test]
    fn trace_update_propose_creates_a_delta_without_writing_the_body() {
        let (_tmp, handoff) = setup();
        let c = setup_with_req_001(&handoff);

        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"propose": true, "ops": [{"op": "set", "item": "REQ-001", "dev_stage": "in_progress"}]}),
            )
            .unwrap(),
        )
        .unwrap();
        let delta_id = out["delta_id"].as_str().unwrap().to_string();
        assert_eq!(out["propose"], true);

        let record = read_delta(&handoff, &delta_id).unwrap().unwrap();
        assert_eq!(record.status, "pending");

        // Nothing was actually written to the document.
        let doc = crate::storage::docs::read_doc(&handoff, "req-doc")
            .unwrap()
            .unwrap();
        assert_eq!(
            find_sub_item(&doc, "REQ-001").dev_stage.as_deref(),
            Some("not_started")
        );
    }

    #[test]
    fn trace_update_rejects_dry_run_and_propose_together() {
        let (_tmp, handoff) = setup();
        let c = setup_with_req_001(&handoff);

        let result = handle_trace_update(
            &c,
            &json!({"dry_run": true, "propose": true, "ops": [{"op": "set", "item": "REQ-001", "dev_stage": "in_progress"}]}),
        );
        assert!(result.is_err());
    }

    #[test]
    fn trace_update_propose_rejects_record_ops_before_writing_a_delta() {
        let (_tmp, handoff) = setup();
        let c = setup_with_req_001(&handoff);

        let result = handle_trace_update(
            &c,
            &json!({"propose": true, "ops": [{"op": "record", "item": "REQ-001", "result": "pass"}]}),
        );
        assert!(result.is_err());
        assert!(list_all_deltas(&handoff).unwrap().is_empty());
    }
}
