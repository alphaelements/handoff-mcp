//! `handoff_trace_update` (wiki/260-vmodel-m2-design.md §4.8, M2-14,
//! FR-702/FR-601's remainder): a single bulk-mutation entry point over 5 op
//! kinds — `upsert_item` (layer-document body), `link`/`unlink` (task-side
//! requirement links, via `docs::apply_requirement_diff_and_propagate`),
//! `set` (an item's runtime fields — `dev_stage`/`approval`/`impl_refs`, plus
//! `priority`/`test_refs` on a non-layer item only), `record` (one merged
//! `runs/<id>.json`, via `trace::handle_trace_record`), `clear_suspect` (one
//! `trace_suspect(action="clear")` call per op, via
//! `trace_suspect::handle_trace_suspect`).
//!
//! **E15 (atomicity)**: every op is validated — input shape, enum values
//! (`role` ∈ implements/executes, `priority` ∈ P0-P3, `result`, `dev_stage`,
//! `approval`), and every referential lookup this op kind actually commits to
//! (document existence, `upsert_item`'s `after` target and its own `id`
//! resolving to a real item heading once rendered, `link`/`unlink`'s `item`
//! and `task`, `set`'s `item`) — *before* anything is written. An `item`
//! lookup is checked against the union of the phase-1 corpus snapshot and
//! every `id` an earlier `upsert_item` op *in this same call* already
//! planned (see [`ItemRegistry`]) — not just the snapshot — so a `link`/`set`
//! op can legally target an item an earlier op in the same call creates
//! (the fixed body-first write order below is what makes this safe to
//! *apply*, not just to validate). `record`'s `item` is deliberately **not**
//! existence-checked here: it is forwarded to `handle_trace_record`, which
//! already has its own established, documented policy of recording an
//! unresolvable `item` anyway with a warning (`runs::record_run`'s
//! `record_run_unknown_stable_id_is_saved_with_a_warning_and_no_body_hash`) —
//! duplicating a stricter check here would make `trace_update`'s `record` op
//! reject inputs the single-op `handoff_trace_record` tool still accepts,
//! for no spec-mandated reason. Validation is pure reads plus in-memory text
//! construction (the `upsert_item` rewrite happens entirely in memory at
//! this stage, which is also what makes `dry_run`'s unified-diff preview
//! possible without a second code path). If any op fails validation, nothing
//! is written and the response's `failed` names the first offending op —
//! the same contract a write-time failure (rare: an `IO`/optimistic-lock
//! error inside one of the 5 category-apply steps below) also uses, except
//! that `applied` there lists whichever earlier *categories* had already
//! been durably written (E15: "ファイルをまたぐトランザクションにはしない...
//! 途中で失敗したら適用済みの ops を返す" — this module cannot roll back a
//! category that already landed on disk, only stop before starting the next
//! one).
//!
//! Writes run in the fixed category order §4.8/E15 specify — 本文
//! (`upsert_item`) → リンク (`link`/`unlink`) → 実行時データ (`set`) → 記録
//! (`record`) → 解除 (`clear_suspect`) — regardless of the input order of
//! `ops` (an op's own `op_index`, preserved through validation, is what the
//! response's `applied[].op_index` echoes back).
//!
//! `upsert_item` across multiple target documents in one call batches them
//! through the same two-pass in-memory sequence `trace::resync_direct_edited_layer_docs`
//! uses (§2.5 step 4: "全対象文書をメモリ上で同期してからベースラインを記録
//! する") — local sync for every touched document first, then cross-document
//! `link_baselines` resolution against an in-memory corpus snapshot that
//! already reflects every touched document's brand-new hash, never a stale
//! on-disk one. Unlike that batch (which never changes any document's body
//! itself, only its derived verification metadata, so it can reuse `DocSet`'s
//! read-mutate-flush cycle as-is), `upsert_item` also changes the body, so
//! each touched document's frontmatter+body are written together in one
//! [`write_doc_with_body`] call per document under its own P-M7-style
//! optimistic-lock check — see [`apply_upsert_ops`]'s doc comment.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::docs::{
    apply_requirement_diff_and_propagate, resolve_pending_cross_doc_baselines_with_bodies,
    suspect_introduced_summary, sync_layer_items_local, write_requirements_summary,
};
use super::trace::handle_trace_record;
use super::trace_suspect::handle_trace_suspect;
use super::HandlerContext;
use crate::storage::config::read_config;
use crate::storage::docs::layer::LayerRegistry;
use crate::storage::docs::layer_parse::{default_prefix_table, parse_layer_body, ParsedItem};
use crate::storage::docs::layer_render::{
    render_acceptance_block, render_item, strip_trailing_acceptance_block, ItemRenderAttrs,
};
use crate::storage::docs::layer_sync::PendingBaseline;
use crate::storage::docs::model::{CodeRef, DocMetadata, Waiver};
use crate::storage::docs::split::{compose_doc_hash, compute_sections, split};
use crate::storage::docs::{
    doc_body_path, find_doc_by_id, read_all_docs, read_doc, read_doc_body, write_doc_with_body,
};
use crate::storage::runs::is_valid_result;
use crate::storage::tasks::find_task_dir_by_id;

const DEFAULT_HEADING_LEVEL: u8 = 3;
const VALID_PRIORITIES: [&str; 4] = ["P0", "P1", "P2", "P3"];

/// `stable_id -> (owning doc_id, owning doc is a layer document)` — built once
/// in [`handle_trace_update`] from the phase-1 corpus snapshot and extended
/// in place as each `upsert_item` op is planned, so a `link`/`unlink`/`set`
/// op later in the *same* call can target an item an earlier op in that same
/// call just created (M2-S10 rework round 2, MAJOR: the fixed body-first
/// write order, §4.8/E15, already makes this safe to *apply* — the item is
/// durably on disk by the time the `link`/`set` category runs — this map is
/// what lets *validation* see it too, instead of only consulting the
/// pre-call snapshot and rejecting a perfectly legal same-call sequence).
type ItemRegistry = HashMap<String, (String, bool)>;

/// Seeds an [`ItemRegistry`] from every `SubItem.stable_id` already present
/// in `docs` — the pre-call state `plan_one_op` extends as `upsert_item` ops
/// are planned. A `stable_id` that (incorrectly) exists in more than one
/// document keeps whichever document is scanned first — the same
/// first-match policy the pre-existing `duplicate_elsewhere` check already
/// tolerates (that's a `trace_lint` `duplicate_id` corpus issue, not this
/// function's job to adjudicate).
fn build_item_registry(docs: &[DocMetadata]) -> ItemRegistry {
    let mut registry = ItemRegistry::new();
    for d in docs {
        let Some(v) = &d.verification else { continue };
        for item in &v.items {
            for sub in &item.sub_items {
                if let Some(id) = &sub.stable_id {
                    registry
                        .entry(id.clone())
                        .or_insert_with(|| (d.id.clone(), d.layer.is_some()));
                }
            }
        }
    }
    registry
}

/// `(len, mtime_ns)` of a document's `_doc.<slug>.md` file — the same
/// optimistic-lock fingerprint shape `DocSet` (`docset.rs`'s private
/// `stat_fingerprint`) uses, duplicated here rather than exposed from that
/// module (small, self-contained, no shared state) so `upsert_item`'s
/// phase-1 body read and phase-2 write can detect a concurrent writer in
/// between without routing the whole write through a `DocSet` (which would
/// reintroduce the write_doc_body + DocSet::flush double-write this same
/// rework fixes — see [`apply_upsert_ops`]'s doc comment).
fn stat_doc_fingerprint(handoff: &Path, slug: &str) -> Result<Option<(u64, u64)>> {
    match std::fs::metadata(doc_body_path(handoff, slug)) {
        Ok(meta) => {
            let mtime_ns = meta
                .modified()
                .map(|m| {
                    m.duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos() as u64
                })
                .unwrap_or(0);
            Ok(Some((meta.len(), mtime_ns)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// `handoff_trace_update` entry point. See this module's doc comment.
pub fn handle_trace_update(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let ops_val = arguments
        .get("ops")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("'ops' (non-empty array) is required"))?;
    if ops_val.is_empty() {
        anyhow::bail!("'ops' must not be empty");
    }
    let dry_run = arguments
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let default_task_id = arguments
        .get("task_id")
        .and_then(|v| v.as_str())
        .map(String::from);
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
    let commit = arguments
        .get("commit")
        .and_then(|v| v.as_str())
        .map(String::from);

    let trace_config = read_config(&handoff.join("config.toml"))
        .map(|c| c.trace)
        .unwrap_or_default();
    let registry = LayerRegistry::build(&trace_config.layer);
    let prefix_table = default_prefix_table(&registry, &trace_config.id_prefixes);

    // Phase 1 (E15): validate every op, in input order, before any write.
    // Building each plan is pure reads + in-memory text construction (the
    // `upsert_item` rewrite happens here, in `working_bodies`, which is also
    // what `dry_run`'s diff preview reads from — there is no second
    // "preview" code path).
    let all_docs_snapshot = read_all_docs(handoff)?;
    let mut item_registry = build_item_registry(&all_docs_snapshot);
    let mut working_bodies: HashMap<String, WorkingBody> = HashMap::new();
    let mut plans = Plans::default();
    let mut warnings: Vec<String> = Vec::new();

    for (op_index, raw_op) in ops_val.iter().enumerate() {
        if let Err(msg) = plan_one_op(
            handoff,
            op_index,
            raw_op,
            default_task_id.as_deref(),
            &prefix_table,
            &all_docs_snapshot,
            &mut item_registry,
            &mut working_bodies,
            &mut plans,
            &mut warnings,
        ) {
            return Ok(failure_response(op_index, &msg.to_string(), &warnings));
        }
    }

    if dry_run {
        return Ok(dry_run_response(&plans, &working_bodies, &warnings));
    }

    // Phase 2: apply in the fixed §4.8/E15 category order. Each category
    // function either fully succeeds (appending to `applied`) or returns an
    // error, in which case this function stops immediately — categories not
    // yet reached are never attempted, matching E15's "途中で失敗したら適用
    // 済みの ops を返す" (the categories already durably written stay
    // written; this module does not attempt a cross-file rollback, E15's own
    // "ファイルをまたぐトランザクションにはしない").
    let mut applied: Vec<Value> = Vec::new();
    let now = chrono::Utc::now().to_rfc3339();
    let mut all_def_changed: Vec<String> = Vec::new();

    if let Err(e) = apply_upsert_ops(
        handoff,
        &plans.upserts,
        &working_bodies,
        &all_docs_snapshot,
        &now,
        &mut applied,
        &mut warnings,
        &mut all_def_changed,
    ) {
        return Ok(partial_failure_response(
            plans.upserts.first().map(|p| p.op_index).unwrap_or(0),
            &e.to_string(),
            applied,
            warnings,
        ));
    }

    if let Err(e) = apply_link_ops(handoff, &plans.links, &mut applied, &mut warnings) {
        return Ok(partial_failure_response(
            plans.links.first().map(|p| p.op_index).unwrap_or(0),
            &e.to_string(),
            applied,
            warnings,
        ));
    }

    if let Err(e) = apply_set_ops(handoff, &plans.sets, &now, &mut applied) {
        return Ok(partial_failure_response(
            plans.sets.first().map(|p| p.op_index).unwrap_or(0),
            &e.to_string(),
            applied,
            warnings,
        ));
    }

    if let Err(e) = apply_record_ops(
        ctx,
        &plans.records,
        executor_kind,
        executor_id.as_deref(),
        commit.as_deref(),
        default_task_id.as_deref(),
        &mut applied,
        &mut warnings,
    ) {
        return Ok(partial_failure_response(
            plans.records.first().map(|p| p.op_index).unwrap_or(0),
            &e.to_string(),
            applied,
            warnings,
        ));
    }

    if let Err(e) = apply_clear_ops(
        ctx,
        &plans.clears,
        executor_kind,
        executor_id.as_deref(),
        &mut applied,
        &mut warnings,
    ) {
        return Ok(partial_failure_response(
            plans.clears.first().map(|p| p.op_index).unwrap_or(0),
            &e.to_string(),
            applied,
            warnings,
        ));
    }

    applied.sort_by_key(|v| v.get("op_index").and_then(|i| i.as_u64()).unwrap_or(0));

    let suspect_introduced = if all_def_changed.is_empty() {
        Value::Null
    } else {
        let fresh_docs = read_all_docs(handoff)?;
        suspect_introduced_summary(handoff, &fresh_docs, &all_def_changed)?.unwrap_or(Value::Null)
    };

    let out = json!({
        "applied": applied,
        "warnings": warnings,
        "suspect_introduced": suspect_introduced,
    });
    Ok(serde_json::to_string_pretty(&out)?)
}

fn failure_response(op_index: usize, error: &str, warnings: &[String]) -> String {
    let out = json!({
        "applied": [],
        "failed": {"op_index": op_index, "error": error},
        "warnings": warnings,
    });
    serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string())
}

fn partial_failure_response(
    op_index: usize,
    error: &str,
    applied: Vec<Value>,
    warnings: Vec<String>,
) -> String {
    let out = json!({
        "applied": applied,
        "failed": {"op_index": op_index, "error": error},
        "warnings": warnings,
    });
    serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string())
}

fn dry_run_response(
    plans: &Plans,
    working_bodies: &HashMap<String, WorkingBody>,
    warnings: &[String],
) -> String {
    let mut applied: Vec<Value> = Vec::new();
    for p in &plans.upserts {
        let wb = &working_bodies[&p.doc_id];
        applied.push(json!({
            "op_index": p.op_index,
            "op": "upsert_item",
            "result": {
                "doc": wb.slug,
                "id": p.id,
                "created": p.created,
                "diff": p.diff,
            },
        }));
    }
    for p in &plans.links {
        applied.push(json!({
            "op_index": p.op_index,
            "op": if p.link { "link" } else { "unlink" },
            "result": {"task_id": p.task_id, "item": p.item, "role": p.role},
        }));
    }
    for p in &plans.sets {
        applied.push(json!({
            "op_index": p.op_index,
            "op": "set",
            "result": {
                "item": p.item,
                "dev_stage": p.dev_stage,
                "approval": p.approval,
            },
        }));
    }
    for p in &plans.records {
        applied.push(json!({
            "op_index": p.op_index,
            "op": "record",
            "result": {"item": p.item, "result": p.result},
        }));
    }
    for p in &plans.clears {
        applied.push(json!({
            "op_index": p.op_index,
            "op": "clear_suspect",
            "result": {"target": p.target, "reason": p.reason},
        }));
    }
    applied.sort_by_key(|v| v.get("op_index").and_then(|i| i.as_u64()).unwrap_or(0));
    let out = json!({
        "applied": applied,
        "warnings": warnings,
        "dry_run": true,
    });
    serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string())
}

#[derive(Default)]
struct Plans {
    upserts: Vec<PlannedUpsert>,
    links: Vec<PlannedLink>,
    sets: Vec<PlannedSet>,
    records: Vec<PlannedRecord>,
    clears: Vec<PlannedClear>,
}

struct WorkingBody {
    slug: String,
    current: String,
    /// This document's metadata exactly as the phase-1 snapshot read it
    /// (`all_docs_snapshot`'s own copy) — the base [`apply_upsert_ops`]
    /// mutates in memory (new `content_hash`/sections/verification) before
    /// its single [`write_doc_with_body`] write, never a value re-read from
    /// disk mid-call.
    doc: DocMetadata,
    /// `(len, mtime_ns)` of `_doc.<slug>.md` at the moment this document's
    /// body was first read into `current` (phase 1) — `None` only in the
    /// pathological case where the file vanished between `resolve_doc` and
    /// the stat call. [`apply_upsert_ops`] re-stats and compares against
    /// this right before writing (P-M7-style optimistic lock, M2-S10 rework
    /// round 2 MAJOR fix): a mismatch means another process wrote this
    /// document between validation and write, which must not be silently
    /// overwritten.
    original_fingerprint: Option<(u64, u64)>,
}

struct PlannedUpsert {
    op_index: usize,
    doc_id: String,
    id: String,
    created: bool,
    diff: String,
}

struct PlannedLink {
    op_index: usize,
    link: bool, // true = link, false = unlink
    task_id: String,
    item: String,
    role: Option<String>,
}

struct PlannedSet {
    op_index: usize,
    item: String,
    doc_id: String,
    dev_stage: Option<String>,
    approval: Option<String>,
    impl_refs: Option<Vec<CodeRef>>,
    priority: Option<String>,
    test_refs: Option<Vec<CodeRef>>,
}

struct PlannedRecord {
    op_index: usize,
    item: String,
    result: String,
    note: Option<String>,
    evidence: Vec<String>,
}

struct PlannedClear {
    op_index: usize,
    target: Value,
    reason: String,
}

/// Resolves a document by either its file-naming `slug` or its stable `id`
/// (same small duplicate as `trace_scaffold::resolve_doc_by_slug_or_id` /
/// `task_checklist::resolve_doc_by_slug_or_id` — `docs::resolve_doc` itself
/// stays private, per those modules' own precedent).
fn resolve_doc_by_slug_or_id(handoff: &Path, slug_or_id: &str) -> Result<Option<DocMetadata>> {
    if let Some(doc) = read_doc(handoff, slug_or_id)? {
        return Ok(Some(doc));
    }
    find_doc_by_id(handoff, slug_or_id)
}

fn str_or_none(v: &Value) -> Option<String> {
    v.as_str().and_then(|s| {
        if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        }
    })
}

fn string_list(v: &Value) -> Vec<String> {
    match v {
        Value::Array(arr) => arr
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect(),
        Value::String(s) => vec![s.clone()],
        _ => Vec::new(),
    }
}

fn code_refs_from_value(v: &Value) -> Vec<CodeRef> {
    v.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    let path = r.get("path").and_then(|v| v.as_str())?.to_string();
                    Some(CodeRef {
                        path,
                        lines: r.get("lines").and_then(|v| v.as_str()).map(str::to_string),
                        label: r.get("label").and_then(|v| v.as_str()).map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Builds the merged [`ItemRenderAttrs`] for an `upsert_item` op: starts from
/// `existing`'s current attributes (full fidelity, including reserved
/// `assignee`/`needs` keys the op's `attrs` schema doesn't even expose) and
/// overlays only the sub-keys actually present in `attrs_arg` — "渡した部分
/// だけを置き換える" (§4.8) applied at the per-attribute-key granularity
/// (the alternative reading — `attrs` as a single all-or-nothing block
/// replacement — would silently drop an existing `assignee`/`needs`/
/// `rationale` the op never mentioned; see this task's dev report for the
/// full reasoning). Returns the merged attrs plus every `(axis, reason)` pair
/// whose derived/waive-* value is new or changed since `existing` (E4's
/// `waiver_added` warning input). `existing: None` (brand-new item) is
/// itself a "before" of all-`None`/empty, so a creation call that sets
/// `attrs.derived`/`attrs.waive-verify`/`attrs.waive-refine` to a non-empty
/// value *also* reports it here (M2-S10 rework round 2, MAJOR fix: creating
/// an already-waived item used to be the one way to add a waiver with no
/// `waiver_added` warning at all — exactly the silent-AI-waiver E4 exists to
/// prevent).
fn build_render_attrs(
    existing: Option<&ParsedItem>,
    attrs_arg: Option<&Value>,
) -> (ItemRenderAttrs, Vec<(String, String)>) {
    let mut out = match existing {
        Some(item) => ItemRenderAttrs {
            layer: item.attrs.layer.clone(),
            refines: item.attrs.refines.clone(),
            verifies: item.attrs.verifies.clone(),
            priority: item.attrs.priority.clone(),
            from: item.ext_attrs.from.clone(),
            method: item.attrs.method.clone(),
            test_refs: item.attrs.test_refs.clone(),
            rationale: item.ext_attrs.rationale.clone(),
            derived: item.ext_attrs.derived.clone(),
            waivers: item.ext_attrs.waivers.clone(),
            reserved: item.ext_attrs.reserved.clone(),
        },
        None => ItemRenderAttrs::default(),
    };

    let derived_before = out.derived.clone();
    let verify_before = out
        .waivers
        .iter()
        .find(|w| w.axis == "verify")
        .map(|w| w.reason.clone());
    let refine_before = out
        .waivers
        .iter()
        .find(|w| w.axis == "refine")
        .map(|w| w.reason.clone());

    if let Some(attrs) = attrs_arg {
        if let Some(v) = attrs.get("layer") {
            out.layer = str_or_none(v);
        }
        if let Some(v) = attrs.get("refines") {
            out.refines = string_list(v);
        }
        if let Some(v) = attrs.get("verifies") {
            out.verifies = string_list(v);
        }
        if let Some(v) = attrs.get("priority") {
            out.priority = str_or_none(v);
        }
        if let Some(v) = attrs.get("method") {
            out.method = str_or_none(v);
        }
        if let Some(v) = attrs.get("test") {
            out.test_refs = string_list(v);
        }
        if let Some(v) = attrs.get("rationale") {
            out.rationale = str_or_none(v);
        }
        if let Some(v) = attrs.get("derived") {
            out.derived = str_or_none(v);
        }
        if attrs.get("waive-verify").is_some() || attrs.get("waive-refine").is_some() {
            let verify_reason = match attrs.get("waive-verify") {
                Some(v) => str_or_none(v),
                None => verify_before.clone(),
            };
            let refine_reason = match attrs.get("waive-refine") {
                Some(v) => str_or_none(v),
                None => refine_before.clone(),
            };
            let mut new_waivers = Vec::new();
            if let Some(r) = verify_reason {
                new_waivers.push(Waiver {
                    axis: "verify".to_string(),
                    reason: r,
                });
            }
            if let Some(r) = refine_reason {
                new_waivers.push(Waiver {
                    axis: "refine".to_string(),
                    reason: r,
                });
            }
            out.waivers = new_waivers;
        }
    }

    // M2-S10 rework round 2 (MAJOR fix): this diff-based detection used to
    // run only `if existing.is_some()` — exempting a brand-new item from
    // E4's warning entirely, so creating REQ-002 with `derived`/`waive-*`
    // attrs already set in the same `upsert_item` call that creates it never
    // warned at all (the easiest way to slip an unreviewed waiver past E4).
    // No `existing.is_some()` guard is needed here: for a new item `out` was
    // built from `ItemRenderAttrs::default()` above, so `derived_before`/
    // `verify_before`/`refine_before` are already `None` — comparing
    // `out.derived != derived_before` etc. unconditionally correctly fires
    // exactly when a creation call's `attrs` sets a new non-empty value, and
    // still correctly stays silent when an existing item's `upsert_item`
    // call omits `attrs.derived`/`attrs.waive-*` entirely (the match arms
    // above leave `out` equal to `*_before` in that case) or re-sends the
    // exact same reason it already had.
    let mut waiver_added: Vec<(String, String)> = Vec::new();
    if out.derived != derived_before {
        if let Some(reason) = &out.derived {
            waiver_added.push(("derived".to_string(), reason.clone()));
        }
    }
    let verify_after = out
        .waivers
        .iter()
        .find(|w| w.axis == "verify")
        .map(|w| w.reason.clone());
    if verify_after != verify_before {
        if let Some(reason) = &verify_after {
            waiver_added.push(("verify".to_string(), reason.clone()));
        }
    }
    let refine_after = out
        .waivers
        .iter()
        .find(|w| w.axis == "refine")
        .map(|w| w.reason.clone());
    if refine_after != refine_before {
        if let Some(reason) = &refine_after {
            waiver_added.push(("refine".to_string(), reason.clone()));
        }
    }

    (out, waiver_added)
}

/// A minimal unified-diff-style hunk scoped to exactly the lines this one
/// `upsert_item` op changed — not a whole-document diff (which would need a
/// general LCS/Myers implementation this crate has no dependency for). Since
/// this module already knows the precise line range being replaced (or the
/// insertion point for a new item), a single `@@ -start,oldlen +start,newlen
/// @@` hunk is both a faithful unified-diff rendering of this specific
/// change and cheaper than diffing the whole body. `start` is 1-based,
/// matching `ParsedItem::start_line`'s own convention.
fn unified_diff_hunk(start_line: usize, old_lines: &[&str], new_lines: &[&str]) -> String {
    let mut out = format!(
        "@@ -{},{} +{},{} @@\n",
        start_line,
        old_lines.len(),
        start_line,
        new_lines.len()
    );
    for line in old_lines {
        out.push('-');
        out.push_str(line);
        out.push('\n');
    }
    for line in new_lines {
        out.push('+');
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Applies one `upsert_item` op's effect to `body` (already parsed into
/// `parsed` by the caller — re-parsed fresh for every op touching the same
/// document, see `plan_one_op`'s doc comment on `working_bodies`). Returns
/// the new full body text, whether this created a new item, and a unified
/// diff hunk of just the change.
#[allow(clippy::too_many_arguments)] // established codebase convention (see other call sites of this attribute); these are independent op-field values from a single JSON input, not something a struct would meaningfully group without adding indirection for its own sake.
fn apply_upsert_to_body(
    body: &str,
    doc_layer: Option<&str>,
    prefix_table: &HashMap<String, Vec<String>>,
    id: &str,
    title_arg: Option<&str>,
    statement_arg: Option<&str>,
    acceptance_arg: Option<&[(String, String)]>,
    attrs_arg: Option<&Value>,
    after_arg: Option<&str>,
) -> (String, bool, String, Vec<(String, String)>) {
    let parsed = parse_layer_body(body, doc_layer, prefix_table);
    let existing = parsed.items.iter().find(|it| it.id == id);

    let (attrs, waiver_added) = build_render_attrs(existing, attrs_arg);
    let title = title_arg
        .map(str::to_string)
        .unwrap_or_else(|| existing.map(|e| e.title.clone()).unwrap_or_default());
    let base_statement = match statement_arg {
        Some(s) => s.trim_end().to_string(),
        None => existing
            .map(|e| strip_trailing_acceptance_block(&e.statement))
            .unwrap_or_default(),
    };
    let acceptance_items: Vec<(String, String)> = match acceptance_arg {
        Some(v) => v.to_vec(),
        None => existing
            .map(|e| {
                e.acceptance
                    .iter()
                    .map(|ac| (ac.label.clone(), ac.text.clone()))
                    .collect()
            })
            .unwrap_or_default(),
    };
    let ac_block = render_acceptance_block(&acceptance_items);
    let combined_statement = if ac_block.is_empty() {
        base_statement
    } else if base_statement.trim().is_empty() {
        ac_block.trim_end().to_string()
    } else {
        format!("{base_statement}\n\n{}", ac_block.trim_end())
    };

    let heading_level = existing
        .map(|e| e.heading_level)
        .unwrap_or(DEFAULT_HEADING_LEVEL);
    let rendered = render_item(heading_level, id, &title, &attrs, &combined_statement);
    let new_item_lines: Vec<&str> = rendered.trim_end_matches('\n').split('\n').collect();

    // M2-S10 rework round 2 (MAJOR fix): whether the *original* body ends
    // with a trailing newline, captured before any `lines`-based splicing —
    // `body.split('\n')` turns a trailing "\n" into a phantom empty last
    // element, and the append-at-end branch below used to let that phantom
    // element get "consumed" as the new item's separator blank, silently
    // dropping the document's own trailing newline. Restoring it
    // unconditionally (always ending in "\n") would instead *change* the
    // trailing-newline state of a body that never had one — preserving
    // whatever the original had is the only choice that leaves every other
    // byte of an unrelated part of the document untouched.
    let body_ends_with_newline = body.ends_with('\n');
    let mut lines: Vec<&str> = body.split('\n').collect();
    let created;
    let diff;
    if let Some(item) = existing {
        created = false;
        let start = item.start_line - 1;
        // `ParsedItem::end_line` includes this item's own trailing blank
        // separator line (the blank line before the next heading, or
        // nothing extra when this is the last item — see
        // `parse_layer_body`'s `end_byte`/`end_line` derivation). Trimming
        // those trailing blank lines back out of the replaced range (MAJOR
        // fix: the pre-fix code spliced the whole range, including the
        // separator, with `new_item_lines` — which never carries one of its
        // own — permanently dropping the blank line between this item and
        // whatever follows it) leaves them completely untouched in `lines`,
        // so the separator's original formatting (exactly as it was on
        // disk) survives the edit byte-for-byte. Never trims past the
        // heading line itself (`start + 1`).
        let mut end = item.end_line;
        while end > start + 1 && lines[end - 1].trim().is_empty() {
            end -= 1;
        }
        let old_lines: Vec<&str> = lines[start..end].to_vec();
        diff = unified_diff_hunk(item.start_line, &old_lines, &new_item_lines);
        lines.splice(start..end, new_item_lines.iter().copied());
    } else {
        created = true;
        let insertion_idx = after_arg.and_then(|after_id| {
            parsed
                .items
                .iter()
                .find(|it| it.id == after_id)
                .map(|it| it.end_line)
        });
        let insert_at = insertion_idx.unwrap_or(lines.len());
        let diff_start_line = insert_at + 1;
        diff = unified_diff_hunk(diff_start_line, &[], &new_item_lines);
        // Nothing but blank lines (typically just the phantom "" a trailing
        // "\n" leaves in `lines`) from `insert_at` onward means there is
        // nothing to separate the new item *from* on its far side — this is
        // an append at the document's end even when `after` named an item
        // explicitly (`after` = the document's last item: its `end_line`
        // stops before the phantom trailing element, so `insert_at` lands one
        // short of `lines.len()`). Routing that case through the
        // middle-insert branch below glued the new heading directly onto the
        // previous item's last statement line (no blank separator) and left
        // an extra blank line at EOF (session review, round 2).
        let nothing_follows = lines[insert_at.min(lines.len())..]
            .iter()
            .all(|l| l.trim().is_empty());
        if nothing_follows {
            // Append at the absolute end of the document — nothing follows,
            // so a separator is only needed *before* the new item (when the
            // current last line isn't already blank), never after.
            if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                lines.push("");
            }
            lines.extend(new_item_lines.iter().copied());
            // MAJOR fix: restore the original body's own trailing newline —
            // the pre-fix code let it get silently swallowed by the blank
            // separator above (or simply never re-added it), so an append
            // onto a body that ended in "\n" came back without one.
            if body_ends_with_newline {
                lines.push("");
            }
        } else {
            // Inserting right before `insert_at` (another item's heading, or
            // trailing content): the gap immediately before `insert_at`
            // already *is* the preceding item's own trailing blank
            // separator (untouched, per `end_line`'s own derivation above —
            // this zero-width splice never looks at it), so only a
            // *trailing* blank is needed here to separate the new item from
            // whatever follows. MAJOR fix: the pre-fix code additionally
            // prepended a blank line here, producing two blank lines before
            // the new item and none after it.
            // Defensive: a preceding item authored with no blank line before
            // the next heading has no separator to reuse — add one so the
            // new heading never lands directly on a statement line.
            let mut insert_block: Vec<&str> = Vec::new();
            if insert_at > 0 && !lines[insert_at - 1].trim().is_empty() {
                insert_block.push("");
            }
            insert_block.extend(new_item_lines.iter().copied());
            insert_block.push("");
            lines.splice(insert_at..insert_at, insert_block);
        }
    }
    (lines.join("\n"), created, diff, waiver_added)
}

#[allow(clippy::too_many_arguments)]
fn plan_one_op(
    handoff: &Path,
    op_index: usize,
    raw_op: &Value,
    default_task_id: Option<&str>,
    prefix_table: &HashMap<String, Vec<String>>,
    all_docs_snapshot: &[DocMetadata],
    item_registry: &mut ItemRegistry,
    working_bodies: &mut HashMap<String, WorkingBody>,
    plans: &mut Plans,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let op = raw_op
        .get("op")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("ops[{op_index}].op is required"))?;

    match op {
        "upsert_item" => {
            let doc_arg = raw_op
                .get("doc")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("ops[{op_index}].doc is required"))?;
            let id = raw_op
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("ops[{op_index}].id is required"))?
                .to_string();

            let doc = resolve_doc_by_slug_or_id(handoff, doc_arg)?
                .ok_or_else(|| anyhow::anyhow!("ops[{op_index}]: document not found: {doc_arg}"))?;
            if doc.layer.is_none() {
                anyhow::bail!(
                    "ops[{op_index}]: document '{doc_arg}' is not a layer document (no 'layer' \
                     set) — upsert_item writes layer-item body syntax"
                );
            }

            if !working_bodies.contains_key(&doc.id) {
                let original_fingerprint = stat_doc_fingerprint(handoff, &doc.slug)?;
                let original = read_doc_body(handoff, &doc.slug)?.unwrap_or_default();
                working_bodies.insert(
                    doc.id.clone(),
                    WorkingBody {
                        slug: doc.slug.clone(),
                        current: original,
                        doc: doc.clone(),
                        original_fingerprint,
                    },
                );
            }
            // A stable_id that already exists in a *different* document is a
            // pre-existing corpus issue (`trace_lint`'s own `duplicate_id`
            // territory) — not this op's job to refuse, but worth a warning
            // since silently creating a same-id item in `doc_arg` would
            // otherwise look identical to a clean new id.
            let duplicate_elsewhere = all_docs_snapshot.iter().any(|d| {
                d.id != doc.id
                    && d.verification.as_ref().is_some_and(|v| {
                        v.items
                            .iter()
                            .flat_map(|i| &i.sub_items)
                            .any(|s| s.stable_id.as_deref() == Some(id.as_str()))
                    })
            });
            if duplicate_elsewhere {
                warnings.push(format!(
                    "ops[{op_index}]: id '{id}' already exists in another document — this will \
                     create a duplicate stable_id"
                ));
            }

            let title_arg = raw_op.get("title").and_then(|v| v.as_str());
            let statement_arg = raw_op.get("statement").and_then(|v| v.as_str());
            let acceptance_arg: Option<Vec<(String, String)>> = raw_op
                .get("acceptance")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|e| {
                            let label = e.get("label").and_then(|v| v.as_str())?.to_string();
                            let text = e.get("text").and_then(|v| v.as_str())?.to_string();
                            Some((label, text))
                        })
                        .collect()
                });
            let attrs_arg = raw_op.get("attrs");
            // M2-S10 rework round 2 (MAJOR fix): `attrs.priority` used to
            // accept any string unchecked — `doc_verify(action="set_priority")`
            // enforces P0-P3 (see `VALID_PRIORITIES` there), and `upsert_item`
            // must not be a looser back door onto the same `SubItem.priority`
            // field (layer sync derives it straight from this attribute).
            if let Some(p) = attrs_arg
                .and_then(|a| a.get("priority"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                if !VALID_PRIORITIES.contains(&p) {
                    anyhow::bail!(
                        "ops[{op_index}]: invalid attrs.priority {p:?}; expected one of \
                         {VALID_PRIORITIES:?}"
                    );
                }
            }
            let after_arg = raw_op.get("after").and_then(|v| v.as_str());

            let wb = working_bodies.get(&doc.id).expect("just inserted above");
            // M2-S10 rework round 2 (MAJOR fix): `after` used to be silently
            // treated as "insert at the document's end" whenever it didn't
            // resolve — including a typo'd id — instead of failing
            // validation. Checked against this *document's own current
            // working body* (not just the phase-1 snapshot), so `after`
            // referencing an item an earlier `upsert_item` op in this same
            // call just created in this same document still resolves.
            if let Some(after_id) = after_arg {
                let current_items =
                    parse_layer_body(&wb.current, doc.layer.as_deref(), prefix_table);
                if !current_items.items.iter().any(|it| it.id == after_id) {
                    anyhow::bail!(
                        "ops[{op_index}]: after='{after_id}' does not resolve to an existing \
                         item in '{doc_arg}'"
                    );
                }
            }

            let (new_body, created, diff, waiver_added) = apply_upsert_to_body(
                &wb.current,
                doc.layer.as_deref(),
                prefix_table,
                &id,
                title_arg,
                statement_arg,
                acceptance_arg.as_deref(),
                attrs_arg,
                after_arg,
            );
            // M2-S10 rework round 2 (MAJOR fix): `id` used to never be
            // checked against the ID-prefix grammar `parse_layer_body` (and
            // hence every reader of this document) actually recognizes —
            // `upsert_item{id: "zzz bad"}` silently appended a heading that
            // parsed as *no item at all*, reporting `created: true` on every
            // call (nothing was ever actually created) and never producing a
            // `SubItem`. Re-parsing the just-built `new_body` and requiring
            // `id` to resolve there closes that gap for both the "new item"
            // and "edit existing item" cases (an edit can't normally break
            // its own already-valid id, but this also catches a document
            // whose `[trace.id_prefixes]` configuration doesn't actually
            // allow the id anymore).
            let rendered_items = parse_layer_body(&new_body, doc.layer.as_deref(), prefix_table);
            if !rendered_items.items.iter().any(|it| it.id == id) {
                anyhow::bail!(
                    "ops[{op_index}]: id '{id}' does not form a recognized item heading for this \
                     document's layer/ID-prefix configuration (wiki/260 §2.2's grammar) — no item \
                     was written"
                );
            }
            for (axis, reason) in waiver_added {
                warnings.push(format!("waiver_added: {id} {axis} {reason}"));
            }
            working_bodies.get_mut(&doc.id).unwrap().current = new_body;
            item_registry.insert(id.clone(), (doc.id.clone(), true));

            plans.upserts.push(PlannedUpsert {
                op_index,
                doc_id: doc.id.clone(),
                id,
                created,
                diff,
            });
        }
        "link" | "unlink" => {
            let item = raw_op
                .get("item")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow::anyhow!("ops[{op_index}].item is required"))?
                .to_string();
            let task_id = raw_op
                .get("task")
                .and_then(|v| v.as_str())
                .map(String::from)
                .or_else(|| default_task_id.map(String::from))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "ops[{op_index}]: 'task' is required (no op-level 'task' and no \
                         top-level 'task_id' default)"
                    )
                })?;
            if find_task_dir_by_id(&handoff.join("tasks"), &task_id)?.is_none() {
                anyhow::bail!("ops[{op_index}]: task '{task_id}' not found");
            }
            // M2-S10 rework round 2 (MAJOR fix): an override role used to be
            // forwarded unchecked straight into `task_links[].role` —
            // `handoff_update_task(requirement_roles)`'s own
            // `extract_requirement_roles` rejects anything but
            // implements/executes; `trace_update` must not be a looser back
            // door onto the same field.
            let role = raw_op
                .get("role")
                .and_then(|v| v.as_str())
                .map(String::from);
            if let Some(r) = &role {
                if r != "implements" && r != "executes" {
                    anyhow::bail!(
                        "ops[{op_index}]: invalid role {r:?}; expected 'implements' or 'executes'"
                    );
                }
            }
            // M2-S10 rework round 2 (MAJOR fix): an unresolvable `item` used
            // to still appear in `applied` as a success (only a top-level
            // "Could not resolve" warning), so a caller retrying per §4.8's
            // "retry only ops missing from applied" contract would never
            // know this op actually did nothing. Checked against the union
            // of the phase-1 snapshot and every id an earlier `upsert_item`
            // op in this same call already planned (`item_registry`).
            if !item_registry.contains_key(&item) {
                anyhow::bail!("ops[{op_index}]: item '{item}' not found in any document");
            }
            plans.links.push(PlannedLink {
                op_index,
                link: op == "link",
                task_id,
                item,
                role,
            });
        }
        "set" => {
            let item = raw_op
                .get("item")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow::anyhow!("ops[{op_index}].item is required"))?
                .to_string();
            let dev_stage = raw_op
                .get("dev_stage")
                .and_then(|v| v.as_str())
                .map(String::from);
            if let Some(ds) = &dev_stage {
                const VALID: [&str; 5] = [
                    "not_started",
                    "in_progress",
                    "implemented",
                    "tested",
                    "verified",
                ];
                if !VALID.contains(&ds.as_str()) {
                    anyhow::bail!(
                        "ops[{op_index}]: invalid dev_stage {ds:?}; expected one of {VALID:?}"
                    );
                }
            }
            let approval = raw_op
                .get("approval")
                .and_then(|v| v.as_str())
                .map(String::from);
            if let Some(a) = &approval {
                if a != "draft" && a != "approved" {
                    anyhow::bail!(
                        "ops[{op_index}]: invalid approval {a:?}; expected 'draft' or 'approved'"
                    );
                }
            }
            let impl_refs = raw_op.get("impl_refs").map(code_refs_from_value);
            let priority = raw_op
                .get("priority")
                .and_then(|v| v.as_str())
                .map(String::from);
            if let Some(p) = &priority {
                if !VALID_PRIORITIES.contains(&p.as_str()) {
                    anyhow::bail!(
                        "ops[{op_index}]: invalid priority {p:?}; expected one of \
                         {VALID_PRIORITIES:?}"
                    );
                }
            }
            let test_refs = raw_op.get("test_refs").map(code_refs_from_value);

            if dev_stage.is_none()
                && approval.is_none()
                && impl_refs.is_none()
                && priority.is_none()
                && test_refs.is_none()
            {
                anyhow::bail!(
                    "ops[{op_index}]: 'set' requires at least one of dev_stage/approval/\
                     impl_refs/priority/test_refs"
                );
            }

            // M2-S10 rework round 2 (MAJOR fix): resolved against
            // `item_registry` (phase-1 snapshot ∪ ids upserted earlier in
            // this same call), not just `all_docs_snapshot` directly — the
            // body-first write order (§4.8/E15) already makes it safe to
            // `set` an item an earlier `upsert_item` op in this same call
            // just created (it is durably on disk, with a `SubItem`, by the
            // time the `set` category runs); the old snapshot-only lookup
            // rejected that legal sequence at validation time.
            let Some((owner_doc_id, owner_is_layer)) = item_registry.get(&item).cloned() else {
                anyhow::bail!("ops[{op_index}]: item '{item}' not found in any document");
            };
            if owner_is_layer && (priority.is_some() || test_refs.is_some()) {
                anyhow::bail!(
                    "ops[{op_index}]: item '{item}' is on a layer document — priority/test_refs \
                     are body-owned there (only dev_stage/approval/impl_refs can be set directly; \
                     wiki/260 §4.8's table footnote restricts priority/test_refs to non-layer items)"
                );
            }

            plans.sets.push(PlannedSet {
                op_index,
                item,
                doc_id: owner_doc_id,
                dev_stage,
                approval,
                impl_refs,
                priority,
                test_refs,
            });
        }
        "record" => {
            // Deliberately not existence-checked against `item_registry`
            // (M2-S10 rework round 2, MAJOR-finding follow-up): `record` ops
            // are forwarded verbatim to `handle_trace_record`
            // (`apply_record_ops`), which already has its own established,
            // documented policy — same as the single-op `handoff_trace_record`
            // tool — of recording an unresolvable `item` anyway with a
            // warning rather than rejecting it (`runs::record_run`'s
            // `record_run_unknown_stable_id_is_saved_with_a_warning_and_no_body_hash`).
            // Adding a stricter check here would make this op reject inputs
            // the underlying tool still accepts for no spec-mandated reason.
            let item = raw_op
                .get("item")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow::anyhow!("ops[{op_index}].item is required"))?
                .to_string();
            let result = raw_op
                .get("result")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("ops[{op_index}].result is required"))?
                .to_string();
            if !is_valid_result(&result) {
                anyhow::bail!(
                    "ops[{op_index}].result={result:?} is invalid (must be one of pass, fail, \
                     blocked, not_run, skipped)"
                );
            }
            let note = raw_op
                .get("note")
                .and_then(|v| v.as_str())
                .map(String::from);
            let evidence: Vec<String> = raw_op
                .get("evidence")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            plans.records.push(PlannedRecord {
                op_index,
                item,
                result,
                note,
                evidence,
            });
        }
        "clear_suspect" => {
            let reason = raw_op
                .get("reason")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow::anyhow!("ops[{op_index}].reason is required"))?
                .to_string();
            let has_target_key = ["item", "upstream", "task_id", "layer", "result"]
                .iter()
                .any(|k| raw_op.get(*k).and_then(|v| v.as_str()).is_some());
            if !has_target_key {
                anyhow::bail!(
                    "ops[{op_index}]: expected one of item/upstream, item, upstream, task_id, \
                     layer, result (wiki/260 §4.1's clear targets)"
                );
            }
            let target = json!({
                "item": raw_op.get("item"),
                "upstream": raw_op.get("upstream"),
                "task_id": raw_op.get("task_id"),
                "layer": raw_op.get("layer"),
                "result": raw_op.get("result"),
            });
            plans.clears.push(PlannedClear {
                op_index,
                target,
                reason,
            });
        }
        other => anyhow::bail!(
            "ops[{op_index}]: unknown op {other:?} (expected one of upsert_item, link, unlink, \
             set, record, clear_suspect)"
        ),
    }
    Ok(())
}

/// Syncs every `upsert_item`-touched document's new body in memory (the same
/// two-pass batching `trace::resync_direct_edited_layer_docs` uses, §2.5 step
/// 4 — local sync first for every touched document, deferring cross-document
/// `pending_baselines`, then resolving every pending baseline against a
/// snapshot that already reflects every touched document's brand-new hashes,
/// never a stale on-disk one), then writes each touched document's
/// frontmatter *and* new body together in exactly one [`write_doc_with_body`]
/// call.
///
/// M2-S10 rework round 2 (MAJOR fix): the pre-fix version of this function
/// called `write_doc_body` (one fsync, writing the new body under the *old*,
/// now-stale, frontmatter) and then let `DocSet::flush()` write the same file
/// a second time via `write_doc` (which reads the body right back off disk
/// and calls `write_doc_with_body` itself) — two writes, two fsyncs, per
/// touched document, contradicting §4.8's "文書の書き込みは1リクエスト1文書
/// 1回（DocSet と P-M7 の楽観ロック）". Worse, that first raw body write
/// carried no optimistic-lock check at all: a concurrent writer's change
/// landing between this call's phase-1 body read and that write was silently
/// discarded, and `DocSet::load`'s own P-M7 fingerprint (taken *after* the
/// unguarded write already landed) could never catch it. This version
/// instead: (1) re-stats every touched document's fingerprint and compares
/// it against the one [`WorkingBody::original_fingerprint`] captured at
/// phase-1 read time, refusing to write *any* of them if even one has
/// changed since (same refuse-the-whole-batch policy as
/// [`crate::storage::docs::DocSet::flush`]'s own [`DocSetConflict`]); (2)
/// mutates an owned, in-memory copy of each touched [`DocMetadata`] (sections/
/// content_hash/`updated_at`, then `sync_layer_items_local`) exactly like
/// `handle_doc_update_section` does for its own single-document body change;
/// (3) writes each one back with a single [`write_doc_with_body`] call.
///
/// [`DocSetConflict`]: crate::storage::docs::DocSetConflict
#[allow(clippy::too_many_arguments)] // established codebase convention (see other call sites of this attribute); these are independent request-shaped values, not something a struct would meaningfully group without adding indirection for its own sake.
fn apply_upsert_ops(
    handoff: &Path,
    plans: &[PlannedUpsert],
    working_bodies: &HashMap<String, WorkingBody>,
    all_docs_snapshot: &[DocMetadata],
    now: &str,
    applied: &mut Vec<Value>,
    warnings: &mut Vec<String>,
    all_def_changed: &mut Vec<String>,
) -> Result<()> {
    if plans.is_empty() {
        return Ok(());
    }

    let mut touched_doc_ids: Vec<String> = Vec::new();
    for p in plans {
        if !touched_doc_ids.contains(&p.doc_id) {
            touched_doc_ids.push(p.doc_id.clone());
        }
    }

    // P-M7-style optimistic lock (MAJOR fix): refuse to write *any* touched
    // document if *any* of them changed on disk since phase 1 read it —
    // matching `DocSet::flush`'s own all-or-nothing conflict policy, so a
    // caller retrying this call against a freshly reloaded snapshot never
    // has to reason about a partially-applied previous attempt.
    let mut conflicts: Vec<&str> = Vec::new();
    for doc_id in &touched_doc_ids {
        let wb = &working_bodies[doc_id];
        let current = stat_doc_fingerprint(handoff, &wb.slug)?;
        if current != wb.original_fingerprint {
            conflicts.push(wb.slug.as_str());
        }
    }
    if !conflicts.is_empty() {
        conflicts.sort_unstable();
        anyhow::bail!(
            "document(s) changed on disk since this trace_update call started, refusing to \
             overwrite: {}",
            conflicts.join(", ")
        );
    }

    // Owned, mutable corpus: every document from the phase-1 snapshot,
    // touched ones about to be replaced by their own freshly-synced copy
    // below. Seeded from `all_docs_snapshot` (read at the very top of
    // `handle_trace_update`, before any op's own validation reads) so an
    // untouched sibling document's metadata — needed by
    // `resolve_pending_cross_doc_baselines`'s own corpus-wide resolution and
    // by `write_requirements_summary`'s aggregation — is exactly what phase 1
    // already validated against.
    let mut corpus: HashMap<String, DocMetadata> = all_docs_snapshot
        .iter()
        .map(|d| (d.id.clone(), d.clone()))
        .collect();

    let mut pending_by_doc: Vec<(String, Vec<PendingBaseline>)> = Vec::new();
    for doc_id in &touched_doc_ids {
        let wb = &working_bodies[doc_id];
        let mut doc = wb.doc.clone();
        // Same sections/content_hash/updated_at refresh `handle_doc_save`
        // performs for a body-changing save — MAJOR fix: the pre-fix
        // version never touched any of these three fields on `upsert_item`,
        // so a touched document's `content_hash`/`source.canonical_hash`
        // kept describing its *pre-upsert* body, and `updated_at` went
        // stale, after a successful `trace_update` call.
        let split_doc = split(&wb.current, doc.split_level)?;
        doc.has_bom = split_doc.has_bom;
        doc.line_ending = split_doc.line_ending.to_string();
        doc.sections = compute_sections(&split_doc, true);
        let content_hash = compose_doc_hash(&doc.sections);
        doc.content_hash = Some(content_hash.clone());
        doc.source.canonical_hash = Some(content_hash);
        doc.updated_at = now.to_string();

        if let Some(local) =
            sync_layer_items_local(handoff, &mut doc, &wb.current, now, false, warnings)
        {
            all_def_changed.extend(local.def_changed.clone());
            if !local.pending.is_empty() {
                pending_by_doc.push((doc_id.clone(), local.pending));
            }
        }
        corpus.insert(doc_id.clone(), doc);
    }

    if !pending_by_doc.is_empty() {
        let trace_config = read_config(&handoff.join("config.toml"))
            .map(|c| c.trace)
            .unwrap_or_default();
        let registry = LayerRegistry::build(&trace_config.layer);
        // §2.5 step 4: every touched document's brand-new hash is already in
        // `corpus` (the loop above just inserted it), so a same-call
        // downstream item's link baseline resolves against *this* snapshot —
        // never a stale on-disk read. `body_overrides` (MAJOR fix, this
        // task's rework round 2): every touched document's body is only
        // ever written to disk once, at the very end of this function — so
        // without the override, resolving a same-call cross-document
        // baseline here would still read each upstream document's *stale*
        // on-disk body (`resolve_upstream_ref_across_corpus` otherwise
        // always reads from disk, never from `corpus_snapshot`'s
        // `DocMetadata`, which has no body field at all).
        let corpus_snapshot: Vec<DocMetadata> = corpus.values().cloned().collect();
        let body_overrides: HashMap<String, String> = touched_doc_ids
            .iter()
            .map(|id| (id.clone(), working_bodies[id].current.clone()))
            .collect();
        for (doc_id, pending) in &pending_by_doc {
            if let Some(doc) = corpus.get_mut(doc_id) {
                if let Err(e) = resolve_pending_cross_doc_baselines_with_bodies(
                    handoff,
                    doc,
                    pending,
                    &registry,
                    &trace_config.id_prefixes,
                    &corpus_snapshot,
                    &body_overrides,
                ) {
                    warnings.push(format!(
                        "failed to resolve {} cross-document link baseline(s): {e:#}",
                        pending.len()
                    ));
                }
            }
        }
    }

    // Single write per touched document (MAJOR fix — see this function's
    // doc comment).
    for doc_id in &touched_doc_ids {
        let wb = &working_bodies[doc_id];
        let doc = &corpus[doc_id];
        write_doc_with_body(handoff, doc, &wb.current)
            .with_context(|| format!("failed to write document '{}'", wb.slug))?;
    }
    let full_corpus: Vec<DocMetadata> = corpus.into_values().collect();
    write_requirements_summary(handoff, &full_corpus)?;

    for p in plans {
        let wb = &working_bodies[&p.doc_id];
        applied.push(json!({
            "op_index": p.op_index,
            "op": "upsert_item",
            "result": {"doc": wb.slug, "id": p.id, "created": p.created, "diff": p.diff},
        }));
    }
    Ok(())
}

/// Per-task `(to_add, to_remove, roles)` accumulator for [`apply_link_ops`] —
/// named to satisfy `clippy::type_complexity` rather than for reuse
/// elsewhere.
type LinkDiffByTask = HashMap<String, (Vec<String>, Vec<String>, HashMap<String, String>)>;

fn apply_link_ops(
    handoff: &Path,
    plans: &[PlannedLink],
    applied: &mut Vec<Value>,
    warnings: &mut Vec<String>,
) -> Result<()> {
    if plans.is_empty() {
        return Ok(());
    }
    let mut by_task: LinkDiffByTask = HashMap::new();
    for p in plans {
        let entry = by_task.entry(p.task_id.clone()).or_default();
        if p.link {
            entry.0.push(p.item.clone());
            if let Some(role) = &p.role {
                entry.2.insert(p.item.clone(), role.clone());
            }
        } else {
            entry.1.push(p.item.clone());
        }
    }
    for (task_id, (to_add, to_remove, roles)) in &by_task {
        let w =
            apply_requirement_diff_and_propagate(handoff, task_id, to_add, to_remove, roles, &[])?;
        warnings.extend(w);
    }
    for p in plans {
        applied.push(json!({
            "op_index": p.op_index,
            "op": if p.link { "link" } else { "unlink" },
            "result": {"task_id": p.task_id, "item": p.item, "role": p.role},
        }));
    }
    Ok(())
}

fn find_sub_item_mut<'a>(
    doc: &'a mut DocMetadata,
    stable_id: &str,
) -> Option<&'a mut crate::storage::docs::SubItem> {
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

fn apply_set_ops(
    handoff: &Path,
    plans: &[PlannedSet],
    now: &str,
    applied: &mut Vec<Value>,
) -> Result<()> {
    if plans.is_empty() {
        return Ok(());
    }
    let (doc_set, ()) = crate::storage::docs::load_mutate_flush_with_retry(handoff, |doc_set| {
        for p in plans {
            let doc = doc_set
                .get_mut(&p.doc_id)
                .ok_or_else(|| anyhow::anyhow!("document for item '{}' disappeared", p.item))?;
            let sub = find_sub_item_mut(doc, &p.item)
                .ok_or_else(|| anyhow::anyhow!("item '{}' disappeared", p.item))?;
            if let Some(dev_stage) = &p.dev_stage {
                sub.dev_stage = Some(dev_stage.clone());
            }
            if let Some(approval) = &p.approval {
                if approval == "approved" {
                    sub.status = "verified".to_string();
                    sub.verified_at = Some(now.to_string());
                } else {
                    sub.status = "pending".to_string();
                    sub.verified_at = None;
                }
            }
            if let Some(impl_refs) = &p.impl_refs {
                sub.impl_refs = impl_refs.clone();
            }
            if let Some(priority) = &p.priority {
                sub.priority = Some(priority.clone());
            }
            if let Some(test_refs) = &p.test_refs {
                sub.test_refs = test_refs.clone();
            }
            doc_set.mark_dirty(&p.doc_id);
        }
        for doc_id in plans
            .iter()
            .map(|p| &p.doc_id)
            .collect::<std::collections::HashSet<_>>()
        {
            if let Some(doc) = doc_set.get_mut(doc_id) {
                if let Some(v) = doc.verification.as_mut() {
                    v.updated_at = now.to_string();
                    v.status = super::docs::recompute_verification_status(&v.items);
                }
            }
        }
        Ok(())
    })?;
    write_requirements_summary(handoff, doc_set.docs())?;

    for p in plans {
        applied.push(json!({
            "op_index": p.op_index,
            "op": "set",
            "result": {
                "item": p.item,
                "dev_stage": p.dev_stage,
                "approval": p.approval,
            },
        }));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)] // established codebase convention (see other call sites of this attribute); these are independent request-shaped values, not something a struct would meaningfully group without adding indirection for its own sake.
fn apply_record_ops(
    ctx: &HandlerContext,
    plans: &[PlannedRecord],
    executor_kind: &str,
    executor_id: Option<&str>,
    commit: Option<&str>,
    task_id: Option<&str>,
    applied: &mut Vec<Value>,
    warnings: &mut Vec<String>,
) -> Result<()> {
    if plans.is_empty() {
        return Ok(());
    }
    let results: Vec<Value> = plans
        .iter()
        .map(|p| {
            json!({
                "item": p.item,
                "result": p.result,
                "note": p.note,
                "evidence": p.evidence,
            })
        })
        .collect();
    let mut args = json!({
        "results": results,
        "executor_kind": executor_kind,
        "executor_id": executor_id,
        "commit": commit,
    });
    if let Some(tid) = task_id {
        args["task_id"] = json!(tid);
    }
    let out = handle_trace_record(ctx, &args)?;
    let parsed: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
    let run_id = parsed.get("run_id").cloned().unwrap_or(Value::Null);
    if let Some(ws) = parsed.get("warnings").and_then(|v| v.as_array()) {
        for w in ws {
            if let Some(s) = w.as_str() {
                warnings.push(s.to_string());
            }
        }
    }
    for p in plans {
        applied.push(json!({
            "op_index": p.op_index,
            "op": "record",
            "result": {"item": p.item, "result": p.result, "run_id": run_id},
        }));
    }
    Ok(())
}

fn apply_clear_ops(
    ctx: &HandlerContext,
    plans: &[PlannedClear],
    executor_kind: &str,
    executor_id: Option<&str>,
    applied: &mut Vec<Value>,
    warnings: &mut Vec<String>,
) -> Result<()> {
    for p in plans {
        let args = json!({
            "action": "clear",
            "targets": [p.target],
            "reason": p.reason,
            "executor_kind": executor_kind,
            "executor_id": executor_id,
        });
        let out = handle_trace_suspect(ctx, &args)?;
        let parsed: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
        if let Some(ws) = parsed.get("warnings").and_then(|v| v.as_array()) {
            for w in ws {
                if let Some(s) = w.as_str() {
                    warnings.push(s.to_string());
                }
            }
        }
        applied.push(json!({
            "op_index": p.op_index,
            "op": "clear_suspect",
            "result": {
                "cleared": parsed.get("cleared").cloned().unwrap_or(Value::Null),
                "clear_id": parsed.get("clear_id").cloned().unwrap_or(Value::Null),
            },
        }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::handlers::docs::handle_doc_save;
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

    /// Finds a `SubItem` by `stable_id` anywhere in a reloaded document's
    /// verification matrix — a document's items aren't laid out predictably
    /// by index (a preamble section with no layer items of its own, e.g.
    /// `fragment_seq: Some(0)`, commonly occupies index 0).
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

    fn make_task(handoff: &std::path::Path, id: &str) -> std::path::PathBuf {
        use crate::storage::tasks::{write_task, TaskData};
        let task_dir = handoff.join("tasks").join(id);
        std::fs::create_dir_all(&task_dir).unwrap();
        let data = TaskData {
            id: id.to_string(),
            title: format!("Task {id}"),
            notes: None,
            priority: None,
            created_at: None,
            updated_at: None,
            completed_at: None,
            labels: Vec::new(),
            links: Vec::new(),
            task_links: Vec::new(),
            done_criteria: Vec::new(),
            schedule: None,
            dependencies: Vec::new(),
            order: None,
            assignee: None,
            lock: None,
            scope_paths: Vec::new(),
            extra: std::collections::HashMap::new(),
        };
        write_task(&task_dir, "todo", &data).unwrap();
        task_dir
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

    #[test]
    fn upsert_item_creates_a_new_item_in_an_empty_doc() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement");
        write_doc(&handoff, &doc).unwrap();
        handle_doc_save(
            &c,
            &json!({"doc_id": "doc-req", "body": "# Requirements\n"}),
        )
        .unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"ops": [{"op": "upsert_item", "doc": "req-doc", "id": "REQ-001",
                    "title": "Something", "statement": "A requirement."}]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(out.get("failed").is_none(), "{out}");
        let applied = out["applied"].as_array().unwrap();
        assert_eq!(applied.len(), 1, "{out}");
        assert_eq!(applied[0]["result"]["created"], true);

        let body = crate::storage::docs::read_doc_body(&handoff, "req-doc")
            .unwrap()
            .unwrap();
        assert!(body.contains("REQ-001"), "{body}");
        assert!(body.contains("A requirement."), "{body}");
    }

    #[test]
    fn upsert_item_replaces_only_an_existing_items_own_lines() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement");
        write_doc(&handoff, &doc).unwrap();
        let body = "# Requirements\n\n### REQ-001 Old title\n\nOld statement.\n\n### REQ-002 Other\n\nOther statement.\n";
        handle_doc_save(&c, &json!({"doc_id": "doc-req", "body": body})).unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"ops": [{"op": "upsert_item", "doc": "req-doc", "id": "REQ-001",
                    "statement": "New statement."}]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(out.get("failed").is_none(), "{out}");
        assert_eq!(out["applied"][0]["result"]["created"], false);

        let new_body = crate::storage::docs::read_doc_body(&handoff, "req-doc")
            .unwrap()
            .unwrap();
        assert!(
            new_body.contains("Old title"),
            "title preserved: {new_body}"
        );
        assert!(new_body.contains("New statement."), "{new_body}");
        assert!(!new_body.contains("Old statement."), "{new_body}");
        assert!(new_body.contains("REQ-002"), "{new_body}");
        assert!(new_body.contains("Other statement."), "{new_body}");
    }

    #[test]
    fn upsert_item_dry_run_does_not_write_and_returns_a_diff() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement");
        write_doc(&handoff, &doc).unwrap();
        let body = "# Requirements\n\n### REQ-001 Title\n\nStatement.\n";
        handle_doc_save(&c, &json!({"doc_id": "doc-req", "body": body})).unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"dry_run": true, "ops": [{"op": "upsert_item", "doc": "req-doc",
                    "id": "REQ-001", "statement": "Changed."}]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(out["dry_run"], true, "{out}");
        let diff = out["applied"][0]["result"]["diff"].as_str().unwrap();
        assert!(diff.contains("-Statement."), "{diff}");
        assert!(diff.contains("+Changed."), "{diff}");

        let unchanged = crate::storage::docs::read_doc_body(&handoff, "req-doc")
            .unwrap()
            .unwrap();
        assert_eq!(unchanged, body, "dry_run must not write anything");
    }

    #[test]
    fn validation_failure_writes_nothing_and_reports_the_failing_op_index() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement");
        write_doc(&handoff, &doc).unwrap();
        let body = "# Requirements\n\n### REQ-001 Title\n\nStatement.\n";
        handle_doc_save(&c, &json!({"doc_id": "doc-req", "body": body})).unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"ops": [
                    {"op": "upsert_item", "doc": "req-doc", "id": "REQ-001", "statement": "Changed."},
                    {"op": "record", "item": "REQ-001", "result": "not-a-real-result"},
                ]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(out["failed"]["op_index"], 1, "{out}");
        assert!(out["applied"].as_array().unwrap().is_empty(), "{out}");

        let unchanged = crate::storage::docs::read_doc_body(&handoff, "req-doc")
            .unwrap()
            .unwrap();
        assert_eq!(
            unchanged, body,
            "a later op's validation failure must prevent every write"
        );
    }

    #[test]
    fn set_op_updates_dev_stage_and_approval_fields() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement");
        write_doc(&handoff, &doc).unwrap();
        let body = "# Requirements\n\n### REQ-001 Title\n\nStatement.\n";
        handle_doc_save(&c, &json!({"doc_id": "doc-req", "body": body})).unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"ops": [{"op": "set", "item": "REQ-001", "dev_stage": "implemented",
                    "approval": "approved"}]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(out.get("failed").is_none(), "{out}");

        let doc = crate::storage::docs::read_doc(&handoff, "req-doc")
            .unwrap()
            .unwrap();
        let sub = find_sub_item(&doc, "REQ-001").clone();
        assert_eq!(sub.dev_stage.as_deref(), Some("implemented"));
        assert_eq!(sub.status, "verified");
        assert!(sub.verified_at.is_some());
    }

    #[test]
    fn set_op_on_layer_item_rejects_priority_and_test_refs() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement");
        write_doc(&handoff, &doc).unwrap();
        let body = "# Requirements\n\n### REQ-001 Title\n\nStatement.\n";
        handle_doc_save(&c, &json!({"doc_id": "doc-req", "body": body})).unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"ops": [{"op": "set", "item": "REQ-001", "priority": "P1"}]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(out["failed"]["op_index"], 0, "{out}");
    }

    #[test]
    fn record_ops_are_merged_into_one_run_file() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-at", "at-doc", "acceptance");
        write_doc(&handoff, &doc).unwrap();
        let body = "# Acceptance\n\n### AT-001 Title\n\n- verifies: REQ-001\n\nStatement.\n\n\
                    ### AT-002 Other\n\n- verifies: REQ-001\n\nStatement.\n";
        handle_doc_save(&c, &json!({"doc_id": "doc-at", "body": body})).unwrap();

        // Two record ops, deliberately not adjacent in input order — §4.8
        // requires every record op in the call to land in exactly one run
        // file, so a per-op `handle_trace_record` call must fail this test.
        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"ops": [
                    {"op": "record", "item": "AT-001", "result": "pass"},
                    {"op": "set", "item": "AT-001", "dev_stage": "tested"},
                    {"op": "record", "item": "AT-002", "result": "fail"},
                ]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(out.get("failed").is_none(), "{out}");
        let applied = out["applied"].as_array().unwrap();
        let run_ids: Vec<&str> = applied
            .iter()
            .filter(|a| a["op"] == "record")
            .map(|a| a["result"]["run_id"].as_str().unwrap())
            .collect();
        assert_eq!(run_ids.len(), 2, "{out}");
        assert_eq!(run_ids[0], run_ids[1], "both records share one run: {out}");

        let runs_dir = handoff.join("runs");
        let run_files: Vec<String> = std::fs::read_dir(&runs_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| !n.starts_with('_'))
            .collect();
        assert_eq!(
            run_files,
            vec![format!("{}.json", run_ids[0])],
            "exactly one run file must be written"
        );
    }

    #[test]
    fn multi_doc_upsert_uses_the_new_upstream_hash_as_baseline() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let req_doc = layer_doc("doc-req", "req-doc", "requirement");
        write_doc(&handoff, &req_doc).unwrap();
        handle_doc_save(
            &c,
            &json!({"doc_id": "doc-req", "body": "# Requirements\n\n### REQ-001 Title\n\nOld statement.\n"}),
        )
        .unwrap();
        let spec_doc = layer_doc("doc-spec", "spec-doc", "basic_spec");
        write_doc(&handoff, &spec_doc).unwrap();
        handle_doc_save(&c, &json!({"doc_id": "doc-spec", "body": "# Specs\n"})).unwrap();

        // Upsert REQ-001 (changing its def_hash) and add SPEC-001 refining it
        // in the *same* trace_update call — the new SPEC-001 link's baseline
        // must be REQ-001's brand-new hash, not the pre-call one.
        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"ops": [
                    {"op": "upsert_item", "doc": "req-doc", "id": "REQ-001", "statement": "New statement."},
                    {"op": "upsert_item", "doc": "spec-doc", "id": "SPEC-001", "title": "Spec",
                     "statement": "Spec text.", "attrs": {"refines": ["REQ-001"]}},
                ]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(out.get("failed").is_none(), "{out}");

        let req_doc = crate::storage::docs::read_doc(&handoff, "req-doc")
            .unwrap()
            .unwrap();
        let req_hash = find_sub_item(&req_doc, "REQ-001").def_hash.clone().unwrap();

        let spec_doc = crate::storage::docs::read_doc(&handoff, "spec-doc")
            .unwrap()
            .unwrap();
        let baseline = find_sub_item(&spec_doc, "SPEC-001")
            .link_baselines
            .get("REQ-001")
            .cloned()
            .unwrap();
        assert_eq!(
            baseline, req_hash,
            "new link's baseline must be the same-call new hash"
        );
    }

    #[test]
    fn link_op_links_an_item_to_a_task_with_a_baseline_hash() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement");
        write_doc(&handoff, &doc).unwrap();
        handle_doc_save(
            &c,
            &json!({"doc_id": "doc-req", "body": "# Requirements\n\n### REQ-001 Title\n\nStatement.\n"}),
        )
        .unwrap();
        make_task(&handoff, "t1");

        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"ops": [{"op": "link", "item": "REQ-001", "task": "t1"}]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(out.get("failed").is_none(), "{out}");

        let (task_data, _status) =
            crate::storage::tasks::read_task(&handoff.join("tasks").join("t1"))
                .unwrap()
                .unwrap();
        let link = task_data
            .task_links
            .iter()
            .find(|l| l.link_type == "requirement" && l.label.as_deref() == Some("REQ-001"))
            .expect("link must exist");
        assert!(link.baseline_hash.is_some(), "{link:?}");
    }

    #[test]
    fn unlink_op_removes_the_task_link() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement");
        write_doc(&handoff, &doc).unwrap();
        handle_doc_save(
            &c,
            &json!({"doc_id": "doc-req", "body": "# Requirements\n\n### REQ-001 Title\n\nStatement.\n"}),
        )
        .unwrap();
        make_task(&handoff, "t1");
        handle_trace_update(
            &c,
            &json!({"ops": [{"op": "link", "item": "REQ-001", "task": "t1"}]}),
        )
        .unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"ops": [{"op": "unlink", "item": "REQ-001", "task": "t1"}]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(out.get("failed").is_none(), "{out}");

        let (task_data, _status) =
            crate::storage::tasks::read_task(&handoff.join("tasks").join("t1"))
                .unwrap()
                .unwrap();
        assert!(task_data
            .task_links
            .iter()
            .all(|l| !(l.link_type == "requirement" && l.label.as_deref() == Some("REQ-001"))));
    }

    #[test]
    fn link_op_without_a_task_id_fails_validation_and_writes_nothing() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let out: Value = serde_json::from_str(
            &handle_trace_update(&c, &json!({"ops": [{"op": "link", "item": "REQ-001"}]})).unwrap(),
        )
        .unwrap();
        assert_eq!(out["failed"]["op_index"], 0, "{out}");
    }

    #[test]
    fn clear_suspect_op_delegates_to_trace_suspect_clear() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let req_doc = layer_doc("doc-req", "req-doc", "requirement");
        write_doc(&handoff, &req_doc).unwrap();
        handle_doc_save(
            &c,
            &json!({"doc_id": "doc-req", "body": "# Requirements\n\n### REQ-001 Title\n\nStatement.\n"}),
        )
        .unwrap();
        let spec_doc = layer_doc("doc-spec", "spec-doc", "basic_spec");
        write_doc(&handoff, &spec_doc).unwrap();
        handle_doc_save(
            &c,
            &json!({"doc_id": "doc-spec", "body": "### SPEC-001 Spec\n\n- refines: REQ-001\n\nSpec text.\n"}),
        )
        .unwrap();
        // Make REQ-001 -> SPEC-001 a real link-suspect: edit REQ-001's body
        // directly (no baseline update), forcing its def_hash to move.
        handle_doc_save(
            &c,
            &json!({"doc_id": "doc-req", "body": "# Requirements\n\n### REQ-001 Title\n\nChanged statement.\n"}),
        )
        .unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"ops": [{"op": "clear_suspect", "item": "SPEC-001", "upstream": "REQ-001",
                    "reason": "text-only change"}]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(out.get("failed").is_none(), "{out}");
        assert_eq!(out["applied"][0]["result"]["cleared"]["links"], 1, "{out}");

        let clears_dir = handoff.join("trace").join("clears");
        let count = std::fs::read_dir(&clears_dir)
            .map(|d| d.count())
            .unwrap_or(0);
        assert_eq!(count, 1, "exactly one audit file must be written");
    }

    // -- M2-S10 rework round 2 (review round 1 MAJOR findings) --

    #[test]
    fn link_op_with_an_invalid_role_fails_validation() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement");
        write_doc(&handoff, &doc).unwrap();
        handle_doc_save(
            &c,
            &json!({"doc_id": "doc-req", "body": "# Requirements\n\n### REQ-001 Title\n\nStatement.\n"}),
        )
        .unwrap();
        make_task(&handoff, "t1");

        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"ops": [{"op": "link", "item": "REQ-001", "task": "t1", "role": "bogus"}]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(out["failed"]["op_index"], 0, "{out}");

        let (task_data, _status) =
            crate::storage::tasks::read_task(&handoff.join("tasks").join("t1"))
                .unwrap()
                .unwrap();
        assert!(
            task_data.task_links.is_empty(),
            "an invalid role must never reach task_links: {:?}",
            task_data.task_links
        );
    }

    #[test]
    fn link_op_on_an_unresolvable_item_fails_validation_and_is_not_applied() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        make_task(&handoff, "t1");

        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"ops": [{"op": "link", "item": "NOPE-001", "task": "t1"}]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(out["failed"]["op_index"], 0, "{out}");
        assert!(
            out["applied"].as_array().unwrap().is_empty(),
            "an op on an unresolvable item must never appear in applied: {out}"
        );

        let (task_data, _status) =
            crate::storage::tasks::read_task(&handoff.join("tasks").join("t1"))
                .unwrap()
                .unwrap();
        assert!(
            task_data.task_links.is_empty(),
            "{:?}",
            task_data.task_links
        );
    }

    #[test]
    fn set_op_with_an_invalid_priority_fails_validation() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        // A non-layer document (no `layer` set): `set.priority` is allowed
        // there, so the only thing under test is the enum check itself.
        let doc = Doc::new(
            "doc-plain".to_string(),
            "plain-doc".to_string(),
            "Plain".to_string(),
            "note".to_string(),
            "2026-09-28T00:00:00Z".to_string(),
        );
        write_doc(&handoff, &doc).unwrap();
        use crate::storage::docs::{SubItem, Verification, VerificationItem};
        let mut doc = crate::storage::docs::read_doc(&handoff, "plain-doc")
            .unwrap()
            .unwrap();
        doc.verification = Some(Verification {
            status: "in_review".to_string(),
            created_at: "2026-09-28T00:00:00Z".to_string(),
            updated_at: "2026-09-28T00:00:00Z".to_string(),
            items: vec![VerificationItem {
                fragment_seq: Some(0),
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
                    description: "ITEM-001".to_string(),
                    stable_id: Some("ITEM-001".to_string()),
                    ..Default::default()
                }],
                label: None,
            }],
        });
        write_doc(&handoff, &doc).unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"ops": [{"op": "set", "item": "ITEM-001", "priority": "high"}]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(out["failed"]["op_index"], 0, "{out}");

        let reloaded = crate::storage::docs::read_doc(&handoff, "plain-doc")
            .unwrap()
            .unwrap();
        assert_eq!(
            find_sub_item(&reloaded, "ITEM-001").priority,
            None,
            "an invalid priority must never be persisted"
        );
    }

    #[test]
    fn upsert_item_attrs_priority_invalid_fails_validation_and_writes_nothing() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement");
        write_doc(&handoff, &doc).unwrap();
        let body = "# Requirements\n\n### REQ-001 Title\n\nStatement.\n";
        handle_doc_save(&c, &json!({"doc_id": "doc-req", "body": body})).unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"ops": [{"op": "upsert_item", "doc": "req-doc", "id": "REQ-001",
                    "attrs": {"priority": "high"}}]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(out["failed"]["op_index"], 0, "{out}");

        let unchanged = crate::storage::docs::read_doc_body(&handoff, "req-doc")
            .unwrap()
            .unwrap();
        assert_eq!(
            unchanged, body,
            "nothing must be written on validation failure"
        );
    }

    #[test]
    fn upsert_item_with_an_id_that_does_not_match_any_allowed_prefix_fails_validation() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement");
        write_doc(&handoff, &doc).unwrap();
        let body = "# Requirements\n";
        handle_doc_save(&c, &json!({"doc_id": "doc-req", "body": body})).unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"ops": [{"op": "upsert_item", "doc": "req-doc", "id": "zzz bad",
                    "title": "T", "statement": "S."}]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(out["failed"]["op_index"], 0, "{out}");

        let unchanged = crate::storage::docs::read_doc_body(&handoff, "req-doc")
            .unwrap()
            .unwrap();
        assert_eq!(unchanged, body, "an unrecognizable id must write nothing");

        let reloaded = crate::storage::docs::read_doc(&handoff, "req-doc")
            .unwrap()
            .unwrap();
        assert!(
            reloaded
                .verification
                .map(|v| v.items.iter().all(|i| i.sub_items.is_empty()))
                .unwrap_or(true),
            "no SubItem must ever be created from an unrecognizable id"
        );
    }

    #[test]
    fn upsert_item_with_an_unknown_after_id_fails_validation() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement");
        write_doc(&handoff, &doc).unwrap();
        let body = "# Requirements\n\n### REQ-001 Title\n\nStatement.\n";
        handle_doc_save(&c, &json!({"doc_id": "doc-req", "body": body})).unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"ops": [{"op": "upsert_item", "doc": "req-doc", "id": "REQ-002",
                    "title": "New", "statement": "New text.", "after": "REQ-999"}]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(out["failed"]["op_index"], 0, "{out}");

        let unchanged = crate::storage::docs::read_doc_body(&handoff, "req-doc")
            .unwrap()
            .unwrap();
        assert_eq!(unchanged, body, "an unresolvable after must write nothing");
    }

    #[test]
    fn set_op_on_an_item_created_earlier_in_the_same_call_succeeds() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement");
        write_doc(&handoff, &doc).unwrap();
        handle_doc_save(
            &c,
            &json!({"doc_id": "doc-req", "body": "# Requirements\n"}),
        )
        .unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"ops": [
                    {"op": "upsert_item", "doc": "req-doc", "id": "REQ-001", "title": "New",
                     "statement": "New text."},
                    {"op": "set", "item": "REQ-001", "dev_stage": "implemented"},
                ]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(out.get("failed").is_none(), "{out}");

        let doc = crate::storage::docs::read_doc(&handoff, "req-doc")
            .unwrap()
            .unwrap();
        assert_eq!(
            find_sub_item(&doc, "REQ-001").dev_stage.as_deref(),
            Some("implemented")
        );
    }

    #[test]
    fn link_op_on_an_item_created_earlier_in_the_same_call_succeeds() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement");
        write_doc(&handoff, &doc).unwrap();
        handle_doc_save(
            &c,
            &json!({"doc_id": "doc-req", "body": "# Requirements\n"}),
        )
        .unwrap();
        make_task(&handoff, "t1");

        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"ops": [
                    {"op": "upsert_item", "doc": "req-doc", "id": "REQ-001", "title": "New",
                     "statement": "New text."},
                    {"op": "link", "item": "REQ-001", "task": "t1"},
                ]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(out.get("failed").is_none(), "{out}");

        let (task_data, _status) =
            crate::storage::tasks::read_task(&handoff.join("tasks").join("t1"))
                .unwrap()
                .unwrap();
        assert!(task_data
            .task_links
            .iter()
            .any(|l| l.link_type == "requirement" && l.label.as_deref() == Some("REQ-001")));
    }

    #[test]
    fn waiver_added_fires_when_creating_a_new_item_with_a_waiver() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement");
        write_doc(&handoff, &doc).unwrap();
        handle_doc_save(
            &c,
            &json!({"doc_id": "doc-req", "body": "# Requirements\n"}),
        )
        .unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"ops": [{"op": "upsert_item", "doc": "req-doc", "id": "REQ-002",
                    "title": "New", "statement": "New text.",
                    "attrs": {"waive-verify": "not yet verifiable"}}]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(out.get("failed").is_none(), "{out}");
        let warnings = out["warnings"].as_array().unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.as_str().unwrap().contains("waiver_added: REQ-002 verify")),
            "creating an already-waived item must still warn: {out}"
        );
    }

    #[test]
    fn waiver_added_fires_when_adding_a_waiver_to_an_existing_item() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement");
        write_doc(&handoff, &doc).unwrap();
        let body = "# Requirements\n\n### REQ-001 Title\n\nStatement.\n";
        handle_doc_save(&c, &json!({"doc_id": "doc-req", "body": body})).unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"ops": [{"op": "upsert_item", "doc": "req-doc", "id": "REQ-001",
                    "attrs": {"derived": "inferred from REQ-000"}}]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(out.get("failed").is_none(), "{out}");
        let warnings = out["warnings"].as_array().unwrap();
        assert!(
            warnings.iter().any(|w| w
                .as_str()
                .unwrap()
                .contains("waiver_added: REQ-001 derived")),
            "{out}"
        );
    }

    #[test]
    fn waiver_added_fires_when_changing_an_existing_waiver_reason() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement");
        write_doc(&handoff, &doc).unwrap();
        let body =
            "# Requirements\n\n### REQ-001 Title\n\n- waive-verify: first reason\n\nStatement.\n";
        handle_doc_save(&c, &json!({"doc_id": "doc-req", "body": body})).unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"ops": [{"op": "upsert_item", "doc": "req-doc", "id": "REQ-001",
                    "attrs": {"waive-verify": "second reason"}}]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(out.get("failed").is_none(), "{out}");
        let warnings = out["warnings"].as_array().unwrap();
        assert!(
            warnings.iter().any(|w| w
                .as_str()
                .unwrap()
                .contains("waiver_added: REQ-001 verify second reason")),
            "{out}"
        );
    }

    #[test]
    fn waiver_added_does_not_fire_when_resending_the_same_waiver_value() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement");
        write_doc(&handoff, &doc).unwrap();
        let body =
            "# Requirements\n\n### REQ-001 Title\n\n- waive-verify: same reason\n\nStatement.\n";
        handle_doc_save(&c, &json!({"doc_id": "doc-req", "body": body})).unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"ops": [{"op": "upsert_item", "doc": "req-doc", "id": "REQ-001",
                    "attrs": {"waive-verify": "same reason"}}]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(out.get("failed").is_none(), "{out}");
        let warnings = out["warnings"].as_array().unwrap();
        assert!(
            warnings
                .iter()
                .all(|w| !w.as_str().unwrap().contains("waiver_added")),
            "resending the exact same waiver value must not warn: {out}"
        );
    }

    #[test]
    fn upsert_item_replace_middle_item_preserves_the_exact_body_including_separators() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement");
        write_doc(&handoff, &doc).unwrap();
        let body = "# Requirements\n\n### REQ-001 Old title\n\nOld statement.\n\n\
                    ### REQ-002 Other\n\nOther statement.\n";
        handle_doc_save(&c, &json!({"doc_id": "doc-req", "body": body})).unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"ops": [{"op": "upsert_item", "doc": "req-doc", "id": "REQ-001",
                    "statement": "New statement."}]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(out.get("failed").is_none(), "{out}");

        let new_body = crate::storage::docs::read_doc_body(&handoff, "req-doc")
            .unwrap()
            .unwrap();
        assert_eq!(
            new_body,
            "# Requirements\n\n### REQ-001 Old title\n\nNew statement.\n\n\
             ### REQ-002 Other\n\nOther statement.\n",
            "exactly one blank line must separate REQ-001 from REQ-002, unchanged"
        );
    }

    #[test]
    fn upsert_item_insert_after_middle_item_has_exactly_one_blank_line_on_each_side() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement");
        write_doc(&handoff, &doc).unwrap();
        let body = "# Requirements\n\n### REQ-001 Old title\n\nOld statement.\n\n\
                    ### REQ-002 Other\n\nOther statement.\n";
        handle_doc_save(&c, &json!({"doc_id": "doc-req", "body": body})).unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"ops": [{"op": "upsert_item", "doc": "req-doc", "id": "REQ-003",
                    "title": "New", "statement": "New text.", "after": "REQ-001"}]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(out.get("failed").is_none(), "{out}");

        let new_body = crate::storage::docs::read_doc_body(&handoff, "req-doc")
            .unwrap()
            .unwrap();
        assert_eq!(
            new_body,
            "# Requirements\n\n### REQ-001 Old title\n\nOld statement.\n\n\
             ### REQ-003 New\n\nNew text.\n\n\
             ### REQ-002 Other\n\nOther statement.\n",
            "exactly one blank line must separate the new item from each neighbor"
        );
    }

    #[test]
    fn upsert_item_append_at_end_preserves_the_documents_trailing_newline() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement");
        write_doc(&handoff, &doc).unwrap();
        let body = "# Requirements\n\n### REQ-001 Title\n\nStatement.\n";
        handle_doc_save(&c, &json!({"doc_id": "doc-req", "body": body})).unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"ops": [{"op": "upsert_item", "doc": "req-doc", "id": "REQ-002",
                    "title": "New", "statement": "New text."}]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(out.get("failed").is_none(), "{out}");

        let new_body = crate::storage::docs::read_doc_body(&handoff, "req-doc")
            .unwrap()
            .unwrap();
        assert_eq!(
            new_body,
            "# Requirements\n\n### REQ-001 Title\n\nStatement.\n\n### REQ-002 New\n\nNew text.\n",
            "the document's own trailing newline must survive an append"
        );
        assert!(new_body.ends_with('\n'), "{new_body:?}");
        assert!(!new_body.ends_with("\n\n"), "{new_body:?}");
    }

    /// Session review round 2: `after` naming the document's *last* item
    /// (whose `end_line` stops before the phantom element a trailing "\n"
    /// leaves) must behave exactly like an append — one blank line before
    /// the new heading, the document's single trailing newline preserved.
    /// The round-2 splice used to glue the heading onto "Statement." and
    /// leave "\n\n" at EOF.
    #[test]
    fn upsert_item_insert_after_the_last_item_separates_and_keeps_one_trailing_newline() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement");
        write_doc(&handoff, &doc).unwrap();
        let body = "# Requirements\n\n### REQ-001 Title\n\nStatement.\n";
        handle_doc_save(&c, &json!({"doc_id": "doc-req", "body": body})).unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"ops": [{"op": "upsert_item", "doc": "req-doc", "id": "REQ-002",
                    "title": "New", "statement": "New text.", "after": "REQ-001"}]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(out.get("failed").is_none(), "{out}");

        let new_body = crate::storage::docs::read_doc_body(&handoff, "req-doc")
            .unwrap()
            .unwrap();
        assert_eq!(
            new_body,
            "# Requirements\n\n### REQ-001 Title\n\nStatement.\n\n### REQ-002 New\n\nNew text.\n",
        );
    }

    #[test]
    fn upsert_item_refreshes_content_hash_and_updated_at() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement");
        write_doc(&handoff, &doc).unwrap();
        let body = "# Requirements\n\n### REQ-001 Title\n\nStatement.\n";
        handle_doc_save(&c, &json!({"doc_id": "doc-req", "body": body})).unwrap();
        // `read_doc` (unlike `read_doc_hashed`) always discards `content_hash`
        // to `None` on read regardless of what's on disk (P-M1 perf
        // short-circuit) — `read_doc_hashed` is required here to actually
        // observe the persisted value.
        let before = crate::storage::docs::read_doc_hashed(&handoff, "req-doc")
            .unwrap()
            .unwrap();

        // Ensure the timestamp granularity (RFC3339 seconds) actually moves.
        std::thread::sleep(std::time::Duration::from_millis(1100));

        let out: Value = serde_json::from_str(
            &handle_trace_update(
                &c,
                &json!({"ops": [{"op": "upsert_item", "doc": "req-doc", "id": "REQ-001",
                    "statement": "Changed."}]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(out.get("failed").is_none(), "{out}");

        let after = crate::storage::docs::read_doc_hashed(&handoff, "req-doc")
            .unwrap()
            .unwrap();
        assert_ne!(
            after.content_hash, before.content_hash,
            "content_hash must reflect the new body, not the stale pre-upsert one"
        );
        assert_ne!(
            after.source.canonical_hash, before.source.canonical_hash,
            "source.canonical_hash must be refreshed too"
        );
        assert_ne!(
            after.updated_at, before.updated_at,
            "updated_at must move on a body-changing upsert_item"
        );
    }

    /// M2-S10 rework round 2 (MAJOR fix): a document that changed on disk
    /// between this call's phase-1 body read and `apply_upsert_ops`'s write
    /// must not be silently overwritten — exercised directly against the
    /// private write function (there is no public hook to pause
    /// `handle_trace_update` mid-call), simulating a concurrent writer by
    /// mutating the document between capturing the fingerprint and calling
    /// `apply_upsert_ops`.
    #[test]
    fn apply_upsert_ops_refuses_to_write_when_the_document_changed_on_disk_since_validation() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement");
        write_doc(&handoff, &doc).unwrap();
        let body = "# Requirements\n\n### REQ-001 Title\n\nStatement.\n";
        handle_doc_save(&c, &json!({"doc_id": "doc-req", "body": body})).unwrap();

        // Phase-1-equivalent: capture the fingerprint and the resolved
        // DocMetadata exactly as `plan_one_op` would.
        let stale_doc = crate::storage::docs::read_doc(&handoff, "req-doc")
            .unwrap()
            .unwrap();
        let stale_fingerprint = stat_doc_fingerprint(&handoff, "req-doc").unwrap();

        // Simulate a concurrent writer landing between phase 1 and phase 2.
        std::thread::sleep(std::time::Duration::from_millis(10));
        let concurrent_body =
            "# Requirements\n\n### REQ-001 Title\n\nConcurrently-written statement.\n";
        handle_doc_save(&c, &json!({"doc_id": "doc-req", "body": concurrent_body})).unwrap();

        let mut working_bodies: HashMap<String, WorkingBody> = HashMap::new();
        working_bodies.insert(
            "doc-req".to_string(),
            WorkingBody {
                slug: "req-doc".to_string(),
                current: "# Requirements\n\n### REQ-001 Title\n\nMy own new statement.\n"
                    .to_string(),
                doc: stale_doc.clone(),
                original_fingerprint: stale_fingerprint,
            },
        );
        let plans = vec![PlannedUpsert {
            op_index: 0,
            doc_id: "doc-req".to_string(),
            id: "REQ-001".to_string(),
            created: false,
            diff: String::new(),
        }];
        let all_docs_snapshot = vec![stale_doc];
        let now = chrono::Utc::now().to_rfc3339();
        let mut applied = Vec::new();
        let mut warnings = Vec::new();
        let mut all_def_changed = Vec::new();

        let result = apply_upsert_ops(
            &handoff,
            &plans,
            &working_bodies,
            &all_docs_snapshot,
            &now,
            &mut applied,
            &mut warnings,
            &mut all_def_changed,
        );
        assert!(
            result.is_err(),
            "a document that changed on disk since phase 1 must refuse the write"
        );

        let on_disk = crate::storage::docs::read_doc_body(&handoff, "req-doc")
            .unwrap()
            .unwrap();
        assert_eq!(
            on_disk, concurrent_body,
            "the concurrent writer's content must survive untouched — nothing applied here"
        );
    }
}
