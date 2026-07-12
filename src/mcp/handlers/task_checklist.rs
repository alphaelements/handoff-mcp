//! `handoff_task_checklist` — pure-view aggregation of a task's
//! `done_criteria` and its linked documents' verification matrices
//! (doc-20260712-191142-602891 §3.1/§3.2, "タスク×ドキュメント連携チェックシート
//! — 改訂仕様 (v2)"). Phase 1 implements `action="view"`; Phase 2 adds
//! `action="generate"` (doc-20260712-191142-602891 §3.2), which turns a
//! linked spec/design document's level-2 section headings into
//! `done_criteria` items (hardcoded defaults, no config template — spec §3.2
//! "ハードコードデフォルト (config 不要)").
//!
//! `view` writes nothing back to disk: it is a computed view over existing
//! `TaskData.task_links` (`link_type == "doc"`) and each linked document's
//! `DocMetadata.verification` matrix. `generate` also reads only
//! `DocMetadata.sections` (never writes to the document); it writes to the
//! task's `done_criteria` only in `append`/`replace` mode (never in the
//! default `preview` mode).

use anyhow::Result;
use serde_json::{json, Value};

use super::resolve_project_dir;
use crate::storage::docs::{
    batch_resolve_docs, find_doc_by_id, read_doc, DocMetadata, SectionIndex, VerificationItem,
};
use crate::storage::ensure_handoff_exists;
use crate::storage::tasks::{
    find_task_dir_by_id, read_modify_write_task, read_task, suggest_task_id, DoneCriterion,
    TaskData,
};

/// `handoff_task_checklist` entry point: dispatches on `action`
/// (`"view"` default, or `"generate"`).
pub fn handle(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;
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
        "view" => handle_view(arguments, &handoff, &tasks_dir, task_id),
        "generate" => handle_generate(arguments, &handoff, &tasks_dir, task_id),
        other => anyhow::bail!("Unknown action '{other}'; expected 'view' or 'generate'."),
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

    if doc_links.is_empty() {
        return Ok(to_json(&json!({
            "task_id": data.id,
            "title": data.title,
            "no_linked_docs": true,
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
    })))
}

/// Resolve a document by either its file-naming `slug` or its stable `id`
/// (mirrors the private `resolve_doc` helper in `docs.rs`, duplicated here
/// rather than made `pub` there to avoid growing that module's public
/// surface for a single two-line lookup used by only one other handler).
fn resolve_doc_by_slug_or_id(
    handoff: &std::path::Path,
    slug_or_id: &str,
) -> Result<Option<DocMetadata>> {
    if let Some(doc) = read_doc(handoff, slug_or_id)? {
        return Ok(Some(doc));
    }
    find_doc_by_id(handoff, slug_or_id)
}

/// Only `level == 2` sections are eligible for generation (spec §3.2
/// "ハードコードデフォルトルール: level 2 の見出しのみ対象"). `skip_seqs` is the
/// fully-resolved exclusion set built by the caller (`handle_generate`'s
/// `skipped_seqs`), which always includes seq=0 (the preamble) by default
/// plus any caller-supplied `skip_seqs` param values.
fn eligible_sections<'a>(
    sections: &'a [SectionIndex],
    skip_seqs: &[usize],
) -> Vec<&'a SectionIndex> {
    sections
        .iter()
        .filter(|s| s.level == 2 && !skip_seqs.contains(&s.seq))
        .collect()
}

/// Fixed checklist items appended alongside the generated per-section
/// criteria, keyed by `doc_type` (spec §3.2 "ハードコードデフォルト").
fn fixed_items_for_doc_type(doc_type: &str) -> Vec<&'static str> {
    match doc_type {
        "spec" => vec![
            "仕様書の全セクションがカバーされていることを確認",
            "仕様変更があれば doc_save で更新済み",
        ],
        "design" => vec!["設計と実装の乖離がないことを確認"],
        _ => vec![],
    }
}

fn handle_generate(
    arguments: &Value,
    handoff: &std::path::Path,
    tasks_dir: &std::path::Path,
    task_id: &str,
) -> Result<String> {
    let task_dir = find_task_dir_by_id(tasks_dir, task_id)?
        .ok_or_else(|| anyhow::anyhow!("{}", suggest_task_id(tasks_dir, task_id)))?;
    let (data, _status) = read_task(&task_dir)?
        .ok_or_else(|| anyhow::anyhow!("Task file not found in {}", task_dir.display()))?;

    let doc = match arguments.get("doc_id").and_then(|v| v.as_str()) {
        Some(doc_id) => resolve_doc_by_slug_or_id(handoff, doc_id)?
            .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?,
        None => {
            let doc_links: Vec<_> = data
                .links()
                .into_iter()
                .filter(|l| l.link_type == "doc")
                .collect();
            let docs = batch_resolve_docs(handoff, &doc_links)?;
            docs.into_iter()
                .find(|d| d.doc_type == "spec" || d.doc_type == "design")
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "No 'doc_id' given and task {task_id} has no linked document with doc_type 'spec' or 'design'; pass 'doc_id' explicitly."
                    )
                })?
        }
    };

    let mode = arguments
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("preview");
    if !["preview", "append", "replace"].contains(&mode) {
        anyhow::bail!("Unknown mode '{mode}'; expected 'preview', 'append', or 'replace'.");
    }

    let caller_skip_seqs: Vec<usize> = arguments
        .get("skip_seqs")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_u64())
                .map(|n| n as usize)
                .collect()
        })
        .unwrap_or_default();
    let mut skipped_seqs: Vec<usize> = std::iter::once(0).chain(caller_skip_seqs).collect();
    skipped_seqs.sort_unstable();
    skipped_seqs.dedup();

    let generated_criteria: Vec<Value> = eligible_sections(&doc.sections, &skipped_seqs)
        .into_iter()
        .map(|s| {
            json!({
                "item": format!("[{}§{}] {}", doc.doc_type, s.seq, s.heading),
                "fragment_seq": s.seq,
            })
        })
        .collect();
    let fixed_items = fixed_items_for_doc_type(&doc.doc_type);

    let applied = match mode {
        "preview" => false,
        "append" => {
            apply_generated_criteria(&task_dir, &generated_criteria, false)?;
            true
        }
        "replace" => {
            apply_generated_criteria(&task_dir, &generated_criteria, true)?;
            true
        }
        _ => unreachable!("mode already validated above"),
    };

    Ok(to_json(&json!({
        "task_id": data.id,
        "generated_criteria": generated_criteria,
        "applied": applied,
        "skipped_seqs": skipped_seqs,
        "fixed_items": fixed_items,
    })))
}

/// Writes `generated_criteria` into the task's `done_criteria`: appends when
/// `replace` is `false`, overwrites entirely when `true`. Uses
/// `read_modify_write_task` for optimistic-concurrency safety (mirrors
/// `log_time.rs`), since this is a write path shared with other tools that
/// mutate the same task file (e.g. `handoff_check_criterion`).
fn apply_generated_criteria(
    task_dir: &std::path::Path,
    generated_criteria: &[Value],
    replace: bool,
) -> Result<()> {
    let new_items: Vec<DoneCriterion> = generated_criteria
        .iter()
        .map(|c| DoneCriterion {
            item: c["item"].as_str().unwrap_or_default().to_string(),
            checked: false,
        })
        .collect();

    read_modify_write_task(task_dir, |data, status| {
        if replace {
            data.done_criteria = new_items.clone();
        } else {
            data.done_criteria.extend(new_items.clone());
        }
        data.updated_at = Some(chrono::Utc::now().to_rfc3339());
        Ok(status.to_string())
    })
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
    match doc.sections.iter().find(|s| s.seq == item.fragment_seq) {
        Some(section) => &section.content_hash != hash_at_verify,
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
                actions.push(format!(
                    "handoff_doc_verify(doc_id=\"{}\", action=\"check\", fragment_seq={}) — \"{}\" のレビュー完了時",
                    doc.id, item.fragment_seq, item.heading
                ));
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
