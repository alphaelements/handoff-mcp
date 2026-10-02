//! `handoff_task_checklist` — pure-view aggregation of a task's
//! `done_criteria` and its linked documents' verification matrices
//! (doc-20260712-191142-602891 §3.1/§3.2, "タスク×ドキュメント連携チェックシート
//! — 改訂仕様 (v2)"). `action="generate"` (doc-20260712-191142-602891 §3.2) was
//! removed at the M3 release (wiki/260-vmodel-m2-design.md §4.7/§4.11, M2-12;
//! wiki/270-vmodel-m3-design.md §4.8) — use `handoff_trace_scaffold` instead.
//!
//! `view` writes nothing back to disk: it is a computed view over existing
//! `TaskData.task_links` (`link_type == "doc"`) and each linked document's
//! `DocMetadata.verification` matrix.

use anyhow::Result;
use serde_json::{json, Value};

use super::HandlerContext;
use crate::storage::docs::{batch_resolve_docs, DocMetadata, VerificationItem};
use crate::storage::tasks::{find_task_dir_by_id, read_task, suggest_task_id, TaskData};

/// `handoff_task_checklist` entry point: dispatches on `action` (`"view"`
/// is the only supported action — `"generate"` was removed at the M3
/// release, see the module doc comment above).
pub fn handle(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;
    let tasks_dir = handoff.join("tasks");

    let task_id = arguments
        .get("task_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'task_id' is required"))?;
    let action = arguments
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("view");

    match action {
        "view" => handle_view(arguments, handoff, &tasks_dir, task_id),
        other => anyhow::bail!("Unknown action '{other}'; expected 'view'."),
    }
}

fn handle_view(
    _arguments: &Value,
    handoff: &std::path::Path,
    tasks_dir: &std::path::Path,
    task_id: &str,
) -> Result<String> {
    let task_dir = find_task_dir_by_id(tasks_dir, task_id)?
        .ok_or_else(|| anyhow::anyhow!("{}", suggest_task_id(tasks_dir, task_id)))?;
    let (data, _status) = read_task(&task_dir)?
        .ok_or_else(|| anyhow::anyhow!("Task file not found in {}", task_dir.display()))?;

    let doc_links: Vec<_> = data
        .links()
        .into_iter()
        .filter(|l| l.link_type == "doc")
        .collect();

    // wiki/260-vmodel-m2-design.md §4.11/§3.4 (M2-13): `trace` is independent
    // of `doc_links` (a `requirement`-type `task_links` entry, not a `doc`
    // one) — computed regardless of whether this task has any linked
    // documents at all, including the `no_linked_docs` early return below.
    let trace = super::get_task::load_task_trace_view(handoff, task_id, &data.task_links)?;

    if doc_links.is_empty() {
        return Ok(to_json(&json!({
            "task_id": data.id,
            "title": data.title,
            "no_linked_docs": true,
            "trace": trace,
        })));
    }

    let docs = batch_resolve_docs(handoff, &doc_links)?;

    let done_criteria = done_criteria_json(&data);
    let documents: Vec<Value> = docs.iter().map(doc_coverage_json).collect();
    let overall = overall_progress_json(&docs);
    let combined_readiness = combined_readiness_json(&data, &docs);
    let suggested_actions = suggested_actions_json(&data, &docs);

    Ok(to_json(&json!({
        "task_id": data.id,
        "title": data.title,
        "no_linked_docs": false,
        "done_criteria": done_criteria,
        "verification_coverage": {
            "documents": documents,
            "overall": overall,
        },
        "combined_readiness": combined_readiness,
        "suggested_actions": suggested_actions,
        "trace": trace,
    })))
}

fn done_criteria_json(data: &TaskData) -> Value {
    let items: Vec<Value> = data
        .done_criteria
        .iter()
        .enumerate()
        .map(|(index, c)| {
            json!({
                "index": index,
                "item": c.item,
                "checked": c.checked,
            })
        })
        .collect();
    let checked = data.done_criteria.iter().filter(|c| c.checked).count();
    let total = data.done_criteria.len();
    let percentage = if total == 0 {
        0.0
    } else {
        checked as f64 / total as f64 * 100.0
    };
    json!({
        "items": items,
        "progress": { "checked": checked, "total": total, "percentage": percentage },
    })
}

/// Priority order (highest first): stale > skipped > verified > implemented
/// > in_progress > untouched (doc-20260712-191142-602891 §3.1 table).
fn visual_state(doc: &DocMetadata, item: &VerificationItem) -> &'static str {
    if item_is_stale(doc, item) {
        return "stale";
    }
    match item.status.as_str() {
        "skipped" => "skipped",
        "verified" => "verified",
        "pending" => {
            let has_impl = !item.impl_refs.is_empty();
            let has_test = !item.test_refs.is_empty();
            if has_impl && has_test {
                "implemented"
            } else if has_impl {
                "in_progress"
            } else {
                "untouched"
            }
        }
        _ => "untouched",
    }
}

/// An item is stale when it was verified at a content_hash that no longer
/// matches its section's current content_hash. Mirrors
/// `crate::mcp::handlers::docs::item_is_stale` (not reused directly since
/// that helper is private to `docs.rs`; duplicated here rather than exposed
/// publicly to avoid growing that already-1492-line file's public surface
/// for a single one-line predicate).
fn item_is_stale(doc: &DocMetadata, item: &VerificationItem) -> bool {
    let Some(hash_at_verify) = &item.content_hash_at_verify else {
        return false;
    };
    let Some(fragment_seq) = item.fragment_seq else {
        // Freeform items (v2, fragment_seq=None) are never stale: they are
        // not tied to any section's content_hash.
        return false;
    };
    match doc.sections.iter().find(|s| s.seq == fragment_seq) {
        Some(section) => section.content_hash.as_deref() != Some(hash_at_verify.as_str()),
        None => true,
    }
}

fn doc_coverage_json(doc: &DocMetadata) -> Value {
    let empty_items: Vec<VerificationItem> = Vec::new();
    let items = doc
        .verification
        .as_ref()
        .map(|v| &v.items)
        .unwrap_or(&empty_items);

    let items_json: Vec<Value> = items
        .iter()
        .map(|i| {
            json!({
                "fragment_seq": i.fragment_seq,
                "heading": i.heading,
                "status": i.status,
                "stale": item_is_stale(doc, i),
                "visual_state": visual_state(doc, i),
                "impl_refs": i.impl_refs,
                "test_refs": i.test_refs,
            })
        })
        .collect();

    let verified = items.iter().filter(|i| i.status == "verified").count();
    let pending = items.iter().filter(|i| i.status == "pending").count();
    let skipped = items.iter().filter(|i| i.status == "skipped").count();
    let stale = items.iter().filter(|i| item_is_stale(doc, i)).count();
    let total = items.len();
    let percentage = if total == 0 {
        0.0
    } else {
        (verified + skipped) as f64 / total as f64 * 100.0
    };

    json!({
        "doc_id": doc.id,
        "slug": doc.slug,
        "title": doc.title,
        "doc_type": doc.doc_type,
        "items": items_json,
        "progress": {
            "verified": verified,
            "pending": pending,
            "skipped": skipped,
            "stale": stale,
            "total": total,
            "percentage": percentage,
        },
    })
}

fn overall_progress_json(docs: &[DocMetadata]) -> Value {
    let mut verified = 0;
    let mut pending = 0;
    let mut stale = 0;
    let mut total = 0;
    for doc in docs {
        if let Some(v) = &doc.verification {
            verified += v.items.iter().filter(|i| i.status == "verified").count();
            pending += v.items.iter().filter(|i| i.status == "pending").count();
            stale += v.items.iter().filter(|i| item_is_stale(doc, i)).count();
            total += v.items.len();
        }
    }
    let percentage = if total == 0 {
        0.0
    } else {
        verified as f64 / total as f64 * 100.0
    };
    json!({ "verified": verified, "pending": pending, "stale": stale, "total": total, "percentage": percentage })
}

/// Typed blockers (doc-20260712-191142-602891 §3.1 M7: "blockers は typed
/// objects"): one entry per unchecked done_criteria item
/// (`{type:"criteria", index, item}`), one per non-verified/non-skipped
/// verification item (`{type:"verification", doc_id, doc_slug, fragment_seq,
/// heading}`), and one per linked doc that has no verification matrix at all
/// (`{type:"verification_missing", doc_id, doc_slug}` — mirrors
/// `handle_doc_verify_status`'s hard error on the same condition, so a doc
/// that never had `action="generate"` run cannot silently count as ready).
fn combined_readiness_json(data: &TaskData, docs: &[DocMetadata]) -> Value {
    let mut blockers = Vec::new();

    for (index, c) in data.done_criteria.iter().enumerate() {
        if !c.checked {
            blockers.push(json!({ "type": "criteria", "index": index, "item": c.item }));
        }
    }

    for doc in docs {
        let Some(v) = &doc.verification else {
            blockers.push(json!({
                "type": "verification_missing",
                "doc_id": doc.id,
                "doc_slug": doc.slug,
            }));
            continue;
        };
        for item in &v.items {
            let resolved = item.status == "verified" || item.status == "skipped";
            if !resolved || item_is_stale(doc, item) {
                blockers.push(json!({
                    "type": "verification",
                    "doc_id": doc.id,
                    "doc_slug": doc.slug,
                    "fragment_seq": item.fragment_seq,
                    "heading": item.heading,
                }));
            }
        }
    }

    let done_criteria_met =
        !data.done_criteria.is_empty() && data.done_criteria.iter().all(|c| c.checked);
    let verification_complete = docs.iter().all(|d| match &d.verification {
        None => false,
        Some(v) => v
            .items
            .iter()
            .all(|i| (i.status == "verified" || i.status == "skipped") && !item_is_stale(d, i)),
    });
    let ready = done_criteria_met && verification_complete;

    json!({
        "done_criteria_met": done_criteria_met,
        "verification_complete": verification_complete,
        "ready": ready,
        "blockers": blockers,
    })
}

/// Advisory next-action hints (doc-20260712-191142-602891 §3.1 / §2.3: no
/// auto-sync, `suggested_actions` presents next steps for the caller to run
/// itself). One suggestion per unresolved verification item, one per doc
/// with no verification matrix yet, and one per unchecked done_criteria
/// item, each naming the concrete tool call to make.
fn suggested_actions_json(data: &TaskData, docs: &[DocMetadata]) -> Vec<String> {
    let mut actions = Vec::new();

    for doc in docs {
        let Some(v) = &doc.verification else {
            actions.push(format!(
                "handoff_doc_verify(doc_id=\"{}\", action=\"generate\") — \"{}\" の検証マトリクスがまだ存在しない",
                doc.id, doc.title
            ));
            continue;
        };
        for item in &v.items {
            let resolved = item.status == "verified" || item.status == "skipped";
            if !resolved || item_is_stale(doc, item) {
                let action = match item.fragment_seq {
                    Some(seq) => format!(
                        "handoff_doc_verify(doc_id=\"{}\", action=\"check\", fragment_seq={}) — \"{}\" のレビュー完了時",
                        doc.id, seq, item.heading
                    ),
                    None => format!(
                        "handoff_doc_verify(doc_id=\"{}\", action=\"check\", fragment_seq=null, label=\"{}\") — \"{}\" のレビュー完了時 (フリーフォーム項目)",
                        doc.id, item.label.as_deref().unwrap_or(&item.heading), item.heading
                    ),
                };
                actions.push(action);
            }
        }
    }

    for (index, c) in data.done_criteria.iter().enumerate() {
        if !c.checked {
            actions.push(format!(
                "handoff_check_criterion(task_id=\"{}\", criterion_index={}) — \"{}\" 完了時",
                data.id, index, c.item
            ));
        }
    }

    actions
}

fn to_json(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}
