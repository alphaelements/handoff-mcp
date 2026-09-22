//! MCP handlers for document management (save / get / list) — P1-6a (t96.1),
//! frontmatter migration (t123.1-t123.3, single-file slug-based storage,
//! wiki/130-document-management.md §3.1).
//!
//! Builds on the storage layer in `crate::storage::docs` (on-demand section
//! computation + slug-named `_doc.<slug>.md` frontmatter+body I/O) and the
//! task<->doc bidirectional link sync in
//! `crate::storage::tasks::sync_doc_task_links`. See
//! `wiki/130-document-management.md` §5.1-§5.3 for the spec.

use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::HandlerContext;
use crate::context::injection::{rank_by_bm25_and_scope, RankConfig};
use crate::storage::docs::reassemble::extract_section;
use crate::storage::docs::split::{compute_sections, split};
use crate::storage::docs::{
    delete_doc, delete_doc_body, docs_dir, ensure_docs_dir, find_doc_by_id, read_all_docs,
    read_doc, read_doc_body, validate_slug, write_doc, write_doc_body, CodeRef, DocMetadata,
    DocRelation, SubItem, Verification, VerificationItem,
};
use crate::storage::tasks::{
    find_task_dir_by_id, read_modify_write_task, read_task, sync_doc_task_links, TaskLink,
};

/// Bonus added to a document's BM25 score when one of its `scope_paths` is a
/// prefix of one of the query's `file_paths`. Mirrors `memory.rs`'s
/// `SCOPE_PATH_BONUS` — kept as a separate constant since the two features
/// tune independently even though the value happens to match today.
const SCOPE_PATH_BONUS: f64 = 2.0;

/// Default relevance floor for `doc_list(query=...)`. Kept at 0.0 (no floor)
/// since `doc_list` is an explicit search the caller controls via `query`
/// presence/absence, unlike `memory_query`'s hook-driven auto-injection which
/// needs a floor to avoid noise.
const DOC_QUERY_MIN_SCORE: f64 = 0.0;

fn new_doc_id() -> String {
    format!("doc-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S-%6f"))
}

/// Resolve a document by either its file-naming `slug` or its stable `id`
/// (spec instructs `doc_get`/`doc_delete`/etc. to accept either). Tries the
/// direct slug-keyed file lookup first (cheap, no scan), falling back to a
/// full `id` scan so callers that only recorded a document's `id` (e.g. from
/// a `related`/`parent_id` reference) can still resolve it.
fn resolve_doc(handoff: &Path, slug_or_id: &str) -> Result<Option<DocMetadata>> {
    if let Some(doc) = read_doc(handoff, slug_or_id)? {
        return Ok(Some(doc));
    }
    find_doc_by_id(handoff, slug_or_id)
}

/// `handoff_doc_save` — create or update a document from a full Markdown
/// body: split into in-memory sections, persist the body + metadata as a
/// slug-named pair, and sync the task<->doc bidirectional link.
pub fn handle_doc_save(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;
    ensure_docs_dir(handoff)?;

    let body_arg = arguments.get("body").and_then(|v| v.as_str());
    let append_body_arg = arguments.get("append_body").and_then(|v| v.as_str());
    if body_arg.is_some() && append_body_arg.is_some() {
        anyhow::bail!("'body' and 'append_body' are mutually exclusive");
    }

    let doc_id = arguments.get("doc_id").and_then(|v| v.as_str());
    if append_body_arg.is_some() && doc_id.is_none() {
        anyhow::bail!(
            "'append_body' requires 'doc_id' (appending to a new document is not meaningful)"
        );
    }
    let existing = match doc_id {
        Some(id) => Some(
            find_doc_by_id(handoff, id)?
                .ok_or_else(|| anyhow::anyhow!("Document not found: {id}"))?,
        ),
        None => None,
    };

    // Metadata-only update path (wiki/210-req-traceability-refinement.md
    // §M1): when both `body` and `append_body` are omitted, this is only
    // valid as an update to an existing document (`doc_id` resolved above) —
    // new documents always require a body. The existing body is re-read
    // below (not skipped) so `split()`/`compute_sections()` still run and
    // keep `sections`/`content_hash` consistent with what's on disk, even
    // though nothing textual changed.
    if body_arg.is_none() && append_body_arg.is_none() && existing.is_none() {
        anyhow::bail!("either 'body' or 'append_body' is required");
    }

    // `append_body`: join the appended text onto the existing document's
    // stripped body (read_doc_body — NOT read_full_body, whose BOM
    // restoration would otherwise get re-detected and double-persisted by
    // `split()` below). No separator is inserted when the existing body is
    // empty/missing (spec §3.1 edge case).
    //
    // Metadata-only update (both args None): re-read the existing body
    // verbatim so `split()`/`compute_sections()` below stay consistent with
    // disk, without writing anything back to the body file (`is_metadata_only`
    // gates that skip further down).
    let is_metadata_only = body_arg.is_none() && append_body_arg.is_none();
    let joined_body: String;
    let body: &str = if let Some(append_body) = append_body_arg {
        let existing_doc = existing
            .as_ref()
            .expect("append_body requires doc_id, checked above, so existing is Some");
        let existing_body = read_doc_body(handoff, &existing_doc.slug)?.unwrap_or_default();
        let separator = arguments
            .get("separator")
            .and_then(|v| v.as_str())
            .unwrap_or("\n\n");
        joined_body = if existing_body.is_empty() {
            append_body.to_string()
        } else {
            format!("{existing_body}{separator}{append_body}")
        };
        &joined_body
    } else if is_metadata_only {
        let existing_doc = existing
            .as_ref()
            .expect("metadata-only path requires an existing document, checked above");
        joined_body = read_doc_body(handoff, &existing_doc.slug)?.unwrap_or_default();
        &joined_body
    } else {
        body_arg.expect("body_arg is Some in this branch, checked above")
    };

    // slug: required for new documents, taken from the existing document on
    // update (the `slug` argument is ignored on update — renaming a
    // document's file-naming slug is out of scope for `doc_save`).
    let slug = match &existing {
        Some(d) => d.slug.clone(),
        None => {
            let slug = arguments
                .get("slug")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("'slug' is required for new documents"))?
                .to_string();
            validate_slug(&slug)?;
            if read_doc(handoff, &slug)?.is_some() {
                anyhow::bail!("slug '{slug}' is already in use by another document");
            }
            slug
        }
    };

    let title = arguments
        .get("title")
        .and_then(|v| v.as_str())
        .or(existing.as_ref().map(|d| d.title.as_str()))
        .ok_or_else(|| anyhow::anyhow!("'title' is required for new documents"))?
        .to_string();

    let split_level = arguments
        .get("split_level")
        .and_then(|v| v.as_u64())
        .map(|n| n as u8)
        .unwrap_or(crate::storage::docs::split::DEFAULT_SPLIT_LEVEL);

    let split_doc = split(body, split_level)?;

    let now = chrono::Utc::now().to_rfc3339();
    let id = doc_id.map(str::to_string).unwrap_or_else(new_doc_id);

    let mut doc = match existing {
        Some(mut d) => {
            d.title = title.clone();
            d
        }
        None => {
            let mut d = DocMetadata::new(
                id.clone(),
                slug.clone(),
                title.clone(),
                "note".to_string(),
                now.clone(),
            );
            d.source.origin = "authored".to_string();
            d
        }
    };

    if let Some(doc_type) = arguments.get("doc_type").and_then(|v| v.as_str()) {
        doc.doc_type = doc_type.to_string();
    }
    if let Some(tags) = arguments.get("tags") {
        doc.tags = string_array_value(tags);
    }
    if let Some(scope_paths) = arguments.get("scope_paths") {
        doc.scope_paths = string_array_value(scope_paths);
    }
    let previous_parent_id = doc.parent_id.clone();
    if let Some(parent_id) = arguments.get("parent_id") {
        doc.parent_id = parent_id.as_str().map(str::to_string);
    }
    let mut warnings: Vec<String> = Vec::new();
    if let Some(related) = arguments.get("related").and_then(|v| v.as_array()) {
        let mut malformed_count = 0usize;
        doc.related = related
            .iter()
            .filter_map(|r| {
                let rid = r.get("id").and_then(|v| v.as_str());
                let rel = r.get("rel").and_then(|v| v.as_str());
                match (rid, rel) {
                    (Some(rid), Some(rel)) => Some(DocRelation {
                        id: rid.to_string(),
                        rel: rel.to_string(),
                    }),
                    _ => {
                        malformed_count += 1;
                        None
                    }
                }
            })
            .collect();
        if malformed_count > 0 {
            warnings.push(format!(
                "Ignored {malformed_count} malformed 'related' entr{} (each entry requires string 'id' and 'rel')",
                if malformed_count == 1 { "y" } else { "ies" }
            ));
        }
    }
    if let Some(auto_inject) = arguments.get("auto_inject").and_then(|v| v.as_str()) {
        doc.auto_inject = auto_inject.to_string();
    }

    doc.has_bom = split_doc.has_bom;
    doc.line_ending = split_doc.line_ending.to_string();
    doc.split_level = split_level;
    doc.updated_at = now.clone();

    // v5: the full body (after BOM/frontmatter stripping) is written verbatim
    // to `_doc.<slug>.md`; sections are an in-memory byte-offset index into
    // it, computed fresh on every save (no stale-fragment cleanup needed —
    // there is nothing left on disk to clean up per section). On a
    // metadata-only update (§M1) `body_after_strip` is just the unchanged
    // existing body re-read above — `split()`/`compute_sections()` still run
    // so `sections`/`content_hash` stay consistent, but `write_doc_body` is
    // skipped (nothing textual changed, so there's nothing to persist) and
    // the heading-format warning is suppressed (it would otherwise reproduce
    // on every metadata-only update of a body that predates this check).
    let body_after_strip: String = split_doc.fragments.iter().map(|f| f.body).collect();
    if !is_metadata_only && !body_after_strip.starts_with("# ") {
        warnings.push(
            "body does not start with a level-1 heading — consider adding one for readability"
                .to_string(),
        );
    }
    if !is_metadata_only {
        write_doc_body(handoff, &slug, &body_after_strip)?;
    }
    doc.sections = compute_sections(&split_doc);

    let content_hash = lexsim::content_hash(&body_after_strip);
    doc.content_hash = content_hash.clone();
    doc.source.canonical_hash = Some(content_hash);

    let new_task_ids = arguments
        .get("task_ids")
        .map(string_array_value)
        .unwrap_or_else(|| doc.task_ids.clone());

    if arguments.get("task_ids").is_some() {
        let (link_ids, unlink_ids) = if doc_id.is_some() {
            let previous: Vec<String> = doc.task_ids.clone();
            let link: Vec<String> = new_task_ids
                .iter()
                .filter(|t| !previous.contains(t))
                .cloned()
                .collect();
            let unlink: Vec<String> = previous
                .iter()
                .filter(|t| !new_task_ids.contains(t))
                .cloned()
                .collect();
            (link, unlink)
        } else {
            (new_task_ids.clone(), Vec::new())
        };

        let tasks_dir = handoff.join("tasks");
        let report = sync_doc_task_links(&tasks_dir, &id, &title, &link_ids, &unlink_ids)?;
        if !report.unresolved.is_empty() {
            warnings.push(format!(
                "Could not resolve task id(s) for linking: {}",
                report.unresolved.join(", ")
            ));
        }
        doc.task_ids = new_task_ids;
    }

    write_doc(handoff, &doc)?;

    // Keep the family tree's `children` list in sync with `parent_id`: if the
    // parent changed (including unset -> set on first save), push this doc's
    // id into the new parent's `children` and drop it from the old parent's,
    // mirroring the same "sync the other side" pattern as
    // sync_doc_task_links. A parent id that doesn't resolve is a non-fatal
    // warning, not a rollback — same policy as unresolved task_ids above.
    // `parent_id` references a document's stable `id`, not its `slug`, so
    // resolution goes through `find_doc_by_id`.
    if doc.parent_id != previous_parent_id {
        if let Some(old_parent_id) = &previous_parent_id {
            if let Some(mut old_parent) = find_doc_by_id(handoff, old_parent_id)? {
                let before = old_parent.children.len();
                old_parent.children.retain(|c| c != &id);
                if old_parent.children.len() != before {
                    write_doc(handoff, &old_parent)?;
                }
            }
        }
        if let Some(new_parent_id) = &doc.parent_id {
            match find_doc_by_id(handoff, new_parent_id)? {
                Some(mut new_parent) => {
                    if !new_parent.children.iter().any(|c| c == &id) {
                        new_parent.children.push(id.clone());
                        write_doc(handoff, &new_parent)?;
                    }
                }
                None => warnings.push(format!("Parent document not found: {new_parent_id}")),
            }
        }
    }

    Ok(to_json(&json!({
        "doc_id": id,
        "slug": doc.slug,
        "title": doc.title,
        "doc_type": doc.doc_type,
        "section_count": doc.sections.len(),
        "content_hash": doc.content_hash,
        "warnings": warnings,
    })))
}

/// `handoff_doc_update_section` — replace a single section's body by `seq`
/// without requiring the caller to re-send the whole document (partial
/// update API, t123.4). Computes sections on-demand from the current body
/// (mirrors `read_doc`'s recompute — sections are never trusted from
/// frontmatter), byte-slices out the target section's range, splices in
/// `new_content`, and writes the result back. `expected_hash` is an optional
/// optimistic lock against the section's current `content_hash`.
pub fn handle_doc_update_section(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let doc_id = arguments
        .get("doc_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'doc_id' is required"))?;
    let seq = arguments
        .get("seq")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow::anyhow!("'seq' is required"))? as usize;
    let new_content = arguments
        .get("new_content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'new_content' is required"))?;
    let expected_hash = arguments.get("expected_hash").and_then(|v| v.as_str());

    let mut doc = resolve_doc(handoff, doc_id)?
        .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;

    let body = read_doc_body(handoff, &doc.slug)?
        .ok_or_else(|| anyhow::anyhow!("Document body file missing for slug '{}'", doc.slug))?;
    let split_doc = split(&body, doc.split_level)?;
    let sections = compute_sections(&split_doc);

    let section = sections
        .iter()
        .find(|s| s.seq == seq)
        .ok_or_else(|| anyhow::anyhow!("Section not found: doc_id={doc_id} seq={seq}"))?;

    if let Some(expected) = expected_hash {
        if expected != section.content_hash {
            anyhow::bail!(
                "expected_hash mismatch for doc_id={doc_id} seq={seq}: expected {expected}, \
                 current content_hash is {} — retry with the current hash if this overwrite is \
                 still intended",
                section.content_hash
            );
        }
    }

    // Splice `new_content` into the section's byte range. `extract_section`
    // is not used here (it would re-validate the just-computed hash, which
    // is redundant since `section` was computed from this exact `body`
    // moments ago) — the byte range is sliced directly instead.
    let start = section.byte_offset;
    let end = section.byte_offset + section.byte_length;
    let mut new_body = String::with_capacity(body.len() - (end - start) + new_content.len());
    new_body.push_str(&body[..start]);
    new_body.push_str(new_content);
    new_body.push_str(&body[end..]);

    write_doc_body(handoff, &doc.slug, &new_body)?;

    let new_split_doc = split(&new_body, doc.split_level)?;
    let new_sections = compute_sections(&new_split_doc);
    doc.sections = new_sections.clone();

    let now = chrono::Utc::now().to_rfc3339();
    doc.updated_at = now;

    let content_hash = lexsim::content_hash(&new_body);
    doc.content_hash = content_hash.clone();
    doc.source.canonical_hash = Some(content_hash);

    write_doc(handoff, &doc)?;

    crate::context::doc_corpus_cache()
        .lock()
        .expect("cache")
        .increment_generation();

    let updated_section = new_sections.iter().find(|s| s.seq == seq);
    let verification_stale = doc.verification.as_ref().is_some_and(|v| {
        v.items
            .iter()
            .any(|i| i.fragment_seq == Some(seq) && item_is_stale(&doc, i))
    });

    let mut out = json!({
        "doc_id": doc.id,
        "seq": seq,
        "heading": updated_section.map(|s| s.heading.clone()),
        "content_hash": updated_section.map(|s| s.content_hash.clone()),
        "updated_at": doc.updated_at,
        "section_count": doc.sections.len(),
    });
    if verification_stale {
        out["warnings"] = json!([format!(
            "Verification item at fragment_seq={seq} is now stale (content changed since it was verified)"
        )]);
    }

    Ok(to_json(&out))
}

/// Reads a document's authored content body: the part of `_doc.<slug>.md`
/// *after* handoff's own YAML frontmatter block, with the original UTF-8 BOM
/// (if any) restored in front of it. Returns `Ok(None)` when the `.md` file
/// is missing.
///
/// Frontmatter migration (t123.1): the `.md` file's frontmatter now *is* the
/// document's metadata (handoff-owned), not a user-authored block being
/// losslessly stashed — so unlike the pre-migration 2-file format, a leading
/// YAML block the caller originally passed into `doc_save`'s `body` argument
/// is absorbed into (and superseded by) handoff's own frontmatter, not
/// preserved verbatim. Only the BOM is still restored losslessly.
fn read_full_body(handoff: &Path, doc: &DocMetadata) -> Result<Option<String>> {
    let Some(body) = read_doc_body(handoff, &doc.slug)? else {
        return Ok(None);
    };
    Ok(Some(if doc.has_bom {
        format!("\u{FEFF}{body}")
    } else {
        body
    }))
}

/// `handoff_doc_get` — read a document (by `doc_id` or `slug`) as `full`
/// (the original Markdown body + metadata), `meta` (metadata only), or
/// `section` (one section's body, byte-sliced from `_doc.<slug>.md`).
pub fn handle_doc_get(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let doc_id = arguments
        .get("doc_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'doc_id' is required"))?;

    let format = arguments
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("full");

    match format {
        "meta" => {
            let doc = resolve_doc(handoff, doc_id)?
                .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;
            Ok(to_json(&doc_metadata_json(&doc)))
        }
        "section" | "fragment" => {
            let seq = arguments
                .get("seq")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| anyhow::anyhow!("'seq' is required when format='section'"))?
                as usize;
            let doc = resolve_doc(handoff, doc_id)?
                .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;
            let section =
                doc.sections.iter().find(|s| s.seq == seq).ok_or_else(|| {
                    anyhow::anyhow!("Section not found: doc_id={doc_id} seq={seq}")
                })?;
            let body = read_doc_body(handoff, &doc.slug)?.ok_or_else(|| {
                anyhow::anyhow!("Document body file missing for slug '{}'", doc.slug)
            })?;
            let section_body = extract_section(&body, section)?;
            Ok(to_json(&json!({
                "doc_id": doc.id,
                "seq": section.seq,
                "heading": section.heading,
                "level": section.level,
                "content_hash": section.content_hash,
                "body": section_body,
            })))
        }
        _ => {
            let doc = resolve_doc(handoff, doc_id)?
                .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;
            let body = read_full_body(handoff, &doc)?.unwrap_or_default();
            let mut out = doc_metadata_json(&doc);
            out["body"] = json!(body);
            Ok(to_json(&out))
        }
    }
}

/// `handoff_doc_list` — list/search documents with optional `doc_type` /
/// `tags` (AND) / `task_id` filters, BM25 `query` ranking, and optional
/// reassembled `body` inclusion.
pub fn handle_doc_list(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let doc_type = arguments.get("doc_type").and_then(|v| v.as_str());
    let tags = arguments.get("tags").map(string_array_value);
    let task_id = arguments.get("task_id").and_then(|v| v.as_str());
    let include_body = arguments
        .get("include_body")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let query = arguments
        .get("query")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let mut docs = read_all_docs(handoff)?;
    if let Some(dt) = doc_type {
        docs.retain(|d| d.doc_type == dt);
    }
    if let Some(tags) = &tags {
        if !tags.is_empty() {
            docs.retain(|d| tags.iter().all(|t| d.tags.contains(t)));
        }
    }
    if let Some(tid) = task_id {
        docs.retain(|d| d.task_ids.iter().any(|t| t == tid));
    }

    let ordered_indices: Vec<usize> = if let Some(q) = query {
        rank_docs_by_query(handoff, &docs, q)?
    } else {
        (0..docs.len()).collect()
    };

    let mut out_docs = Vec::with_capacity(ordered_indices.len());
    for idx in ordered_indices {
        let d = &docs[idx];
        let mut entry = doc_metadata_json(d);
        if include_body {
            let body = read_full_body(handoff, d)?.unwrap_or_default();
            entry["body"] = json!(body);
        }
        out_docs.push(entry);
    }

    Ok(to_json(&json!({ "documents": out_docs })))
}

/// Ranks `docs` against `query` via BM25 over each document's index text
/// (title + tags + body), returning original-order indices sorted by
/// descending relevance. Corpus is built fresh every call (no cache — the
/// cache is reserved for `doc_query`, t96.3, per the task's own note).
fn rank_docs_by_query(handoff: &Path, docs: &[DocMetadata], query: &str) -> Result<Vec<usize>> {
    let mut index_texts = Vec::with_capacity(docs.len());
    for d in docs {
        let body = read_doc_body(handoff, &d.slug)?.unwrap_or_default();
        let mut text = d.title.clone();
        text.push(' ');
        text.push_str(&d.tags.join(" "));
        text.push(' ');
        text.push_str(&body);
        index_texts.push(text);
    }

    let corpus = lexsim::Corpus::build_weighted(&index_texts);
    let query_tokens = lexsim::tokenize_weighted(query);
    let scope_paths: Vec<Vec<String>> = docs.iter().map(|d| d.scope_paths.clone()).collect();
    let config = RankConfig {
        min_score: DOC_QUERY_MIN_SCORE,
        relative_threshold: 0.0,
        scope_path_bonus: SCOPE_PATH_BONUS,
        limit: docs.len(),
    };
    let ranked = rank_by_bm25_and_scope(&corpus, &query_tokens, &scope_paths, &[], &config);
    Ok(ranked.into_iter().map(|item| item.index).collect())
}

/// `handoff_doc_delete` — delete a document (by `doc_id` or `slug`) and its
/// body file, unlink it from any linked tasks, remove it from its parent's
/// `children`, and orphan (clear `parent_id` on) any of its own children.
/// See `wiki/130-document-management.md` §5.4.
pub fn handle_doc_delete(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let doc_id = arguments
        .get("doc_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'doc_id' is required"))?;

    let doc = resolve_doc(handoff, doc_id)?
        .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;

    let mut warnings: Vec<String> = Vec::new();

    delete_doc_body(handoff, &doc.slug)?;
    delete_doc(handoff, &doc.slug)?;

    if !doc.task_ids.is_empty() {
        let tasks_dir = handoff.join("tasks");
        let report = sync_doc_task_links(&tasks_dir, &doc.id, &doc.title, &[], &doc.task_ids)?;
        if !report.unresolved.is_empty() {
            warnings.push(format!(
                "Could not resolve task id(s) for unlinking: {}",
                report.unresolved.join(", ")
            ));
        }
    }

    if let Some(parent_id) = &doc.parent_id {
        if let Some(mut parent) = find_doc_by_id(handoff, parent_id)? {
            let before = parent.children.len();
            parent.children.retain(|c| c != &doc.id);
            if parent.children.len() != before {
                write_doc(handoff, &parent)?;
            }
        } else {
            warnings.push(format!("Parent document not found: {parent_id}"));
        }
    }

    for child_id in &doc.children {
        if let Some(mut child) = find_doc_by_id(handoff, child_id)? {
            child.parent_id = None;
            write_doc(handoff, &child)?;
        } else {
            warnings.push(format!("Child document not found: {child_id}"));
        }
    }

    crate::context::doc_corpus_cache()
        .lock()
        .expect("cache")
        .increment_generation();

    Ok(to_json(&json!({
        "deleted": true,
        "doc_id": doc.id,
        "section_count": doc.sections.len(),
        "warnings": warnings,
    })))
}

/// `handoff_doc_reassemble` — read a document's (by `doc_id` or `slug`)
/// original Markdown body directly from `_doc.<slug>.md` (v5: the `.md` file
/// already *is* the original document, restoring BOM/frontmatter is the only
/// reassembly step left), and detect drift (the body's current content hash
/// no longer matches the recorded `content_hash` — e.g. edited directly
/// outside `doc_save`). See `wiki/130-document-management.md` §5.5.
pub fn handle_doc_reassemble(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let doc_id = arguments
        .get("doc_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'doc_id' is required"))?;

    let doc = resolve_doc(handoff, doc_id)?
        .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;

    // `doc.content_hash` is recomputed fresh from the body on every read
    // (t123.2), so comparing it to itself would never detect drift.
    // `doc.source.canonical_hash` is the hash persisted at the *last
    // `doc_save`* (untouched by the on-read recompute — see
    // `storage::docs::read_doc`), so that's the correct "was this edited
    // out-of-band since the last save" baseline.
    let drifted = doc.source.canonical_hash.as_deref() != Some(doc.content_hash.as_str());

    let body = read_full_body(handoff, &doc)?.unwrap_or_default();

    let output_path = arguments.get("output_path").and_then(|v| v.as_str());
    let mut out = json!({
        "doc_id": doc.id,
        "body": body,
        "drifted": drifted,
    });
    if let Some(path) = output_path {
        std::fs::write(path, &body)
            .with_context(|| format!("Failed to write reassembled document to {path}"))?;
        out["output_path"] = json!(path);
    }

    Ok(to_json(&out))
}

/// `handoff_doc_tree` — traverse a document's family tree starting from
/// `doc_id`: its immediate parent (if any) plus `depth` levels of children,
/// optionally including its `related` (semantic) links. See
/// `wiki/130-document-management.md` §5.6.
pub fn handle_doc_tree(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let doc_id = arguments
        .get("doc_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'doc_id' is required"))?;

    let depth = arguments
        .get("depth")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_TREE_DEPTH);

    let include_related = arguments
        .get("include_related")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let doc = resolve_doc(handoff, doc_id)?
        .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;

    let mut tree = doc_tree_node_json(handoff, &doc, include_related)?;

    let parent = match &doc.parent_id {
        Some(parent_id) => find_doc_by_id(handoff, parent_id)?.map(|p| doc_tree_summary_json(&p)),
        None => None,
    };
    tree["parent"] = parent.unwrap_or(Value::Null);

    tree["children"] = json!(doc_tree_children(
        handoff,
        &doc.children,
        depth,
        include_related
    )?);

    Ok(to_json(&tree))
}

/// Default depth for `handoff_doc_tree` when `depth` is omitted (spec §5.6).
const DEFAULT_TREE_DEPTH: u64 = 2;

/// Recursively builds the `children` array for [`handle_doc_tree`], descending
/// up to `depth` levels. Missing child documents (broken link) are skipped
/// rather than erroring the whole traversal.
fn doc_tree_children(
    handoff: &Path,
    child_ids: &[String],
    depth: u64,
    include_related: bool,
) -> Result<Vec<Value>> {
    if depth == 0 {
        return Ok(Vec::new());
    }
    let mut out = Vec::with_capacity(child_ids.len());
    for child_id in child_ids {
        let Some(child) = find_doc_by_id(handoff, child_id)? else {
            continue;
        };
        let mut node = doc_tree_node_json(handoff, &child, include_related)?;
        node["children"] = json!(doc_tree_children(
            handoff,
            &child.children,
            depth - 1,
            include_related
        )?);
        out.push(node);
    }
    Ok(out)
}

/// Compact `{id, title, doc_type}` summary used for `parent` and family-tree
/// list entries (`related`) in `doc_tree`'s output.
fn doc_tree_summary_json(doc: &DocMetadata) -> Value {
    json!({
        "id": doc.id,
        "title": doc.title,
        "doc_type": doc.doc_type,
    })
}

/// One node in the `doc_tree` output: id/title/doc_type plus (optionally)
/// `related` summaries (each resolved to `{id, rel, title}`). `children` is
/// populated by the caller afterward.
fn doc_tree_node_json(handoff: &Path, doc: &DocMetadata, include_related: bool) -> Result<Value> {
    let mut related: Vec<Value> = Vec::new();
    if include_related {
        for r in &doc.related {
            // related entries may point cross-tree/cross-project ids that
            // don't resolve locally; that lookup is deferred to a future
            // resolver (spec §10.3) — for now a related id that can't be
            // read from this project's docs/ is a no-op skip, matching the
            // same lenient policy as read_all_docs.
            let Some(target) = find_doc_by_id(handoff, &r.id)? else {
                continue;
            };
            related.push(json!({ "id": r.id, "rel": r.rel, "title": target.title }));
        }
    }
    Ok(json!({
        "id": doc.id,
        "title": doc.title,
        "doc_type": doc.doc_type,
        "children": [],
        "related": related,
    }))
}

/// Recomputes `Verification.status` from its items (wiki/140-verification-matrix.md
/// §3.3): all pending -> "pending"; all verified/skipped -> "verified";
/// otherwise -> "in_review". v2 (§7.4): an item with `sub_items` is judged by
/// its sub_items' aggregate effective status, not its own `status` field —
/// see `item_effective_status`.
fn recompute_verification_status(items: &[VerificationItem]) -> String {
    let statuses: Vec<String> = items.iter().map(item_effective_status).collect();
    if statuses.iter().all(|s| s == "pending") {
        "pending".to_string()
    } else if statuses.iter().all(|s| s == "verified" || s == "skipped") {
        "verified".to_string()
    } else {
        "in_review".to_string()
    }
}

/// v2 (§7.4): the effective status of a `VerificationItem` for the purposes
/// of the parent `Verification.status` rollup. An item with no `sub_items`
/// uses its own `status` unchanged (v1 behavior). An item with `sub_items`
/// is judged by their aggregate: all verified/skipped -> "verified", all
/// pending -> "pending", otherwise -> "in_review" (a partial mix, distinct
/// from "pending").
fn item_effective_status(item: &VerificationItem) -> String {
    if item.sub_items.is_empty() {
        return item.status.clone();
    }
    if item
        .sub_items
        .iter()
        .all(|s| s.status == "verified" || s.status == "skipped")
    {
        "verified".to_string()
    } else if item.sub_items.iter().all(|s| s.status == "pending") {
        "pending".to_string()
    } else {
        "in_review".to_string()
    }
}

/// Parses a JSON array of `{path, lines?, label?}` objects into `CodeRef`s.
/// Entries missing the required `path` are skipped.
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

/// Source file extensions `suggest_refs` scans for impl/test definitions
/// (t124.6). Kept as a small fixed list rather than "every file" so a scan
/// stays fast and doesn't surface binary/asset noise; extend here if a
/// project needs another language.
const SUGGEST_REFS_EXTENSIONS: &[&str] = &["rs", "ts", "tsx", "py", "go", "js", "jsx"];

/// Maximum number of impl_ref/test_ref candidates returned per verification
/// item by `suggest_refs` (spec: "Return at most ~20 suggestions per item to
/// avoid overwhelming output"), applied independently to each of the two
/// lists.
const SUGGEST_REFS_MAX_PER_ITEM: usize = 20;

/// A single scanned definition (function/struct/impl/mod, or a test
/// function) found while walking `scope_paths` — the raw material
/// `suggest_refs` matches against verification item headings.
struct ScannedDefinition {
    /// Path to the source file, relative to the project root (matches the
    /// `path` shape already used by `CodeRef`/`set_refs`).
    rel_path: String,
    /// The identifier name found after the defining keyword (e.g. the `foo`
    /// in `fn foo(...)`), used for the heading fuzzy-match.
    name: String,
    /// 1-based line number the definition starts on, used to build the
    /// `lines` hint on the suggested `CodeRef`.
    line: usize,
}

/// Walks `doc.scope_paths` under `project_dir` and, for every verification
/// item, returns impl/test ref candidates whose definition name fuzzy-
/// matches the item's heading (t124.6). Read-only — never touches the
/// document or the filesystem beyond reading source files.
fn suggest_refs(project_dir: &Path, doc: &DocMetadata, v: &Verification) -> Vec<Value> {
    let files = scan_scope_files(project_dir, &doc.scope_paths);
    let (impl_defs, test_defs) = scan_definitions(project_dir, &files);

    v.items
        .iter()
        .map(|item| {
            let heading = item.label.clone().unwrap_or_else(|| item.heading.clone());
            let keywords = heading_keywords(&heading);

            let suggested_impl_refs = match_definitions(&impl_defs, &keywords);
            let suggested_test_refs = match_definitions(&test_defs, &keywords);

            json!({
                "fragment_seq": item.fragment_seq,
                "heading": item.heading,
                "suggested_impl_refs": suggested_impl_refs,
                "suggested_test_refs": suggested_test_refs,
            })
        })
        .collect()
}

/// Recursively collects every file under `project_dir` whose relative path
/// starts with one of `scope_paths` (prefix match, spec: "Look for files
/// matching scope_paths patterns") and whose extension is in
/// [`SUGGEST_REFS_EXTENSIONS`]. `scope_paths` entries are relative to
/// `project_dir` (e.g. `src/mcp/handlers/`), matching how `scope_paths` is
/// documented and used elsewhere (BM25 scope bonus, shared-scope graph
/// edges).
fn scan_scope_files(project_dir: &Path, scope_paths: &[String]) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for scope in scope_paths {
        let root = project_dir.join(scope);
        walk_dir(&root, &mut out);
    }
    out
}

fn walk_dir(path: &Path, out: &mut Vec<std::path::PathBuf>) {
    if path.is_file() {
        if has_suggest_refs_extension(path) {
            out.push(path.to_path_buf());
        }
        return;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk_dir(&p, out);
        } else if has_suggest_refs_extension(&p) {
            out.push(p);
        }
    }
}

fn has_suggest_refs_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| SUGGEST_REFS_EXTENSIONS.contains(&ext))
}

/// Splits `files` into impl definitions (fn/struct/impl/mod) and test
/// definitions (files under a `tests/` dir, named `*_test.*`/`test_*`, or
/// containing `#[test]`/`#[cfg(test)]`-marked functions), per-file, by
/// scanning each line with the heuristics from the task spec.
fn scan_definitions(
    project_dir: &Path,
    files: &[std::path::PathBuf],
) -> (Vec<ScannedDefinition>, Vec<ScannedDefinition>) {
    let mut impl_defs = Vec::new();
    let mut test_defs = Vec::new();

    for file in files {
        let Ok(content) = std::fs::read_to_string(file) else {
            continue;
        };
        let rel_path = file
            .strip_prefix(project_dir)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
        let is_test_file = is_test_path(&rel_path);

        let mut next_is_test_fn = false;
        for (idx, line) in content.lines().enumerate() {
            let trimmed = line.trim_start();
            let line_no = idx + 1;

            if trimmed.starts_with("#[test]") || trimmed.starts_with("#[cfg(test)]") {
                next_is_test_fn = true;
                continue;
            }

            if let Some(name) = extract_test_fn_name(trimmed) {
                if is_test_file || next_is_test_fn {
                    test_defs.push(ScannedDefinition {
                        rel_path: rel_path.clone(),
                        name,
                        line: line_no,
                    });
                }
                next_is_test_fn = false;
                continue;
            }
            next_is_test_fn = false;

            if let Some(name) = extract_impl_def_name(trimmed) {
                let bucket = if is_test_file {
                    &mut test_defs
                } else {
                    &mut impl_defs
                };
                bucket.push(ScannedDefinition {
                    rel_path: rel_path.clone(),
                    name,
                    line: line_no,
                });
            }
        }
    }

    (impl_defs, test_defs)
}

fn is_test_path(rel_path: &str) -> bool {
    let lower = rel_path.to_ascii_lowercase();
    lower.split('/').any(|seg| seg == "tests" || seg == "test")
        || lower.contains("_test.")
        || lower.contains("/test_")
        || lower.starts_with("test_")
}

/// Recognizes `fn test_*` / `def test_*` test-function definitions
/// (spec: "`fn test_`") regardless of visibility/async modifiers.
fn extract_test_fn_name(trimmed: &str) -> Option<String> {
    for prefix in ["pub async fn ", "pub fn ", "async fn ", "fn ", "def "] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            if rest.trim_start().starts_with("test_") {
                return extract_identifier(rest);
            }
        }
    }
    None
}

/// Recognizes impl-style definitions the spec calls out: `fn `, `pub fn `,
/// `struct `, `impl `, `mod `.
fn extract_impl_def_name(trimmed: &str) -> Option<String> {
    for prefix in [
        "pub async fn ",
        "pub fn ",
        "async fn ",
        "fn ",
        "pub struct ",
        "struct ",
        "pub mod ",
        "mod ",
        "impl ",
    ] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return extract_identifier(rest);
        }
    }
    None
}

/// Pulls the leading identifier (`[A-Za-z0-9_]+`) off the start of `rest`,
/// e.g. `"foo(bar: &str) {"` -> `"foo"`, `"Foo<T> for Bar"` -> `"Foo"`.
fn extract_identifier(rest: &str) -> Option<String> {
    let ident: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if ident.is_empty() {
        None
    } else {
        Some(ident)
    }
}

/// Splits a heading into lowercase keyword tokens for the fuzzy match
/// (spec: "case-insensitive substring match"), dropping short/common words
/// that would otherwise match almost every identifier.
fn heading_keywords(heading: &str) -> Vec<String> {
    const STOPWORDS: &[&str] = &["the", "a", "an", "of", "to", "and", "or", "for", "in", "on"];
    heading
        .split(|c: char| !c.is_alphanumeric())
        .map(|w| w.to_ascii_lowercase())
        .filter(|w| w.len() > 2 && !STOPWORDS.contains(&w.as_str()))
        .collect()
}

/// Matches `defs` whose `name` (case-insensitive) contains any of
/// `keywords` as a substring, deduplicates by `(path, name)`, and caps the
/// result at [`SUGGEST_REFS_MAX_PER_ITEM`].
fn match_definitions(defs: &[ScannedDefinition], keywords: &[String]) -> Vec<Value> {
    if keywords.is_empty() {
        return Vec::new();
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for def in defs {
        let name_lower = def.name.to_ascii_lowercase();
        let matches = keywords.iter().any(|kw| name_lower.contains(kw.as_str()));
        if !matches {
            continue;
        }
        let key = (def.rel_path.clone(), def.name.clone());
        if !seen.insert(key) {
            continue;
        }
        out.push(json!({
            "path": def.rel_path,
            "lines": def.line.to_string(),
            "label": def.name,
        }));
        if out.len() >= SUGGEST_REFS_MAX_PER_ITEM {
            break;
        }
    }
    out
}

/// Summary counts used by both `doc_verify`'s mutation response and
/// `doc_verify_status`'s `progress` block.
struct VerificationCounts {
    checked: usize,
    skipped: usize,
    pending: usize,
    total: usize,
    stale: usize,
}

/// v2 (§7.4): counts every leaf verification unit — a top-level item that
/// has no `sub_items` counts itself directly (v1 behavior, includes freeform
/// items), while an item that *does* have `sub_items` counts each sub_item
/// instead of itself (so `total` is "top-level item count (leaf-only) + all
/// sub_items count", matching the spec's "トップレベル + 全 sub_items の合計").
fn count_verification(doc: &DocMetadata, v: &Verification) -> VerificationCounts {
    let mut checked = 0;
    let mut skipped = 0;
    let mut pending = 0;
    let mut stale = 0;

    for item in &v.items {
        if item.sub_items.is_empty() {
            match item.status.as_str() {
                "verified" => checked += 1,
                "skipped" => skipped += 1,
                _ => pending += 1,
            }
        } else {
            for sub in &item.sub_items {
                match sub.status.as_str() {
                    "verified" => checked += 1,
                    "skipped" => skipped += 1,
                    _ => pending += 1,
                }
            }
        }
        if item_is_stale(doc, item) {
            stale += 1;
        }
    }

    VerificationCounts {
        checked,
        skipped,
        pending,
        total: checked + skipped + pending,
        stale,
    }
}

/// An item is stale when it was verified at a content_hash that no longer
/// matches its section's current content_hash (spec §3.5) — items never
/// verified (`content_hash_at_verify: None`) are never stale, items whose
/// section has been removed (sync should have dropped them, but be
/// defensive) are treated as stale so drift is never silently hidden, and
/// freeform items (v2, `fragment_seq: None`) are never stale since they are
/// not tied to any section's content_hash.
fn item_is_stale(doc: &DocMetadata, item: &VerificationItem) -> bool {
    let Some(hash_at_verify) = &item.content_hash_at_verify else {
        return false;
    };
    let Some(fragment_seq) = item.fragment_seq else {
        return false;
    };
    match doc.sections.iter().find(|s| s.seq == fragment_seq) {
        Some(section) => &section.content_hash != hash_at_verify,
        None => true,
    }
}

/// Cross-document requirement progress, keyed by `SubItem.priority`
/// (requirements-traceability P0 §4.1 output shape, reused verbatim as the
/// `_requirements_summary.json` cache written by [`write_requirements_summary`]).
#[derive(Debug, Clone, Default, serde::Serialize)]
pub(crate) struct PrioritySummary {
    pub(crate) total: usize,
    pub(crate) implemented: usize,
    pub(crate) tested: usize,
    pub(crate) verified: usize,
}

/// Cross-document requirement progress, keyed by the `C{n}` prefix of
/// `SubItem.stable_id` (P0 §4.1).
#[derive(Debug, Clone, Default, serde::Serialize)]
pub(crate) struct CategorySummary {
    pub(crate) total: usize,
    pub(crate) implemented: usize,
    pub(crate) coverage_pct: f64,
}

/// Percent of requirements with an impl ref / test ref / `dev_stage ==
/// "verified"`, across every SubItem counted into a [`RequirementsSummary`]
/// (P0 §4.1 `coverage` block).
#[derive(Debug, Clone, Default, serde::Serialize)]
pub(crate) struct CoverageSummary {
    pub(crate) impl_pct: f64,
    pub(crate) test_pct: f64,
    pub(crate) verified_pct: f64,
}

/// Per-task requirement progress (requirements-traceability integration
/// reform §3.2): how many SubItems linked to a given task id are at each
/// `dev_stage`, keyed by that `dev_stage` value (e.g. `"not_started"`,
/// `"implemented"`). A SubItem with no `dev_stage` set counts under
/// [`UNSET_DEV_STAGE`], matching [`aggregate_requirements`]'s own fallback.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub(crate) struct TaskCoverageSummary {
    pub(crate) total: usize,
    #[serde(flatten)]
    pub(crate) by_dev_stage: std::collections::HashMap<String, usize>,
}

/// Cross-document requirement (`SubItem`) aggregate — the same shape
/// `handoff_doc_req_status` (P1 §4.1, `docs_query::handle_doc_req_status`)
/// returns, and what [`write_requirements_summary`] persists to
/// `.handoff/docs/_requirements_summary.json` for the VSCode extension
/// (P0 §2.7, §3.4). `task_coverage` (integration-reform §3.2) is keyed by
/// task id, one entry per task referenced by at least one SubItem's
/// `task_ids`.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub(crate) struct RequirementsSummary {
    pub(crate) total: usize,
    pub(crate) by_status: std::collections::HashMap<String, usize>,
    pub(crate) by_priority: std::collections::HashMap<String, PrioritySummary>,
    pub(crate) by_category: std::collections::HashMap<String, CategorySummary>,
    pub(crate) coverage: CoverageSummary,
    pub(crate) task_coverage: std::collections::HashMap<String, TaskCoverageSummary>,
}

/// `dev_stage` fallback for a `SubItem` that has never had one set (P0
/// §3.4 "重要": `dev_stage` が `None` の場合は `"not_started"` としてカウント).
pub(crate) const UNSET_DEV_STAGE: &str = "not_started";

/// `priority` fallback for a `SubItem` that has no priority assigned yet
/// (P0 §3.4 "重要": `priority` が `None` の場合は `"unset"` としてカウント).
pub(crate) const UNSET_PRIORITY: &str = "unset";

/// Extracts the `C{n}` category prefix from a `stable_id` (e.g.
/// `"C01-2.1.1.1"` -> `"C01"`), per P0 §3.4 ("category は stable_id の接頭辞
/// (C01, C07 等) から抽出"). A `stable_id` with no `-` (or no id at all) has
/// no category and is excluded from `by_category` — there is nothing
/// meaningful to bucket it under.
pub(crate) fn category_prefix_from_stable_id(stable_id: &str) -> Option<&str> {
    stable_id.split('-').next().filter(|s| !s.is_empty())
}

/// Walks every `DocMetadata.verification.items[].sub_items[]` across `docs`
/// and aggregates requirement-level progress (P0 §3.4 / §4.1). Only
/// `SubItem`s count as "requirements" here — top-level `VerificationItem`s
/// without `sub_items` track section-review state, not individual
/// requirements, so they are not part of this aggregate.
pub(crate) fn aggregate_requirements(docs: &[DocMetadata]) -> RequirementsSummary {
    let mut summary = RequirementsSummary::default();
    let mut impl_count = 0usize;
    let mut test_count = 0usize;
    let mut verified_count = 0usize;

    for doc in docs {
        let Some(v) = &doc.verification else {
            continue;
        };
        for item in &v.items {
            for sub in &item.sub_items {
                summary.total += 1;

                let status = sub.dev_stage.as_deref().unwrap_or(UNSET_DEV_STAGE);
                *summary.by_status.entry(status.to_string()).or_insert(0) += 1;

                let priority = sub.priority.as_deref().unwrap_or(UNSET_PRIORITY);
                let p = summary.by_priority.entry(priority.to_string()).or_default();
                p.total += 1;

                let has_impl = !sub.impl_refs.is_empty();
                let has_test = !sub.test_refs.is_empty();
                let is_verified = status == "verified";
                if has_impl {
                    impl_count += 1;
                    p.implemented += 1;
                }
                if has_test {
                    test_count += 1;
                    p.tested += 1;
                }
                if is_verified {
                    verified_count += 1;
                    p.verified += 1;
                }

                if let Some(category) = sub
                    .stable_id
                    .as_deref()
                    .and_then(category_prefix_from_stable_id)
                {
                    let c = summary.by_category.entry(category.to_string()).or_default();
                    c.total += 1;
                    if has_impl {
                        c.implemented += 1;
                    }
                }

                for task_id in &sub.task_ids {
                    let t = summary.task_coverage.entry(task_id.clone()).or_default();
                    t.total += 1;
                    *t.by_dev_stage.entry(status.to_string()).or_insert(0) += 1;
                }
            }
        }
    }

    for c in summary.by_category.values_mut() {
        c.coverage_pct = if c.total == 0 {
            0.0
        } else {
            (c.implemented as f64 / c.total as f64) * 100.0
        };
    }

    let total = summary.total;
    summary.coverage = CoverageSummary {
        impl_pct: percent(impl_count, total),
        test_pct: percent(test_count, total),
        verified_pct: percent(verified_count, total),
    };

    summary
}

fn percent(count: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        (count as f64 / total as f64) * 100.0
    }
}

/// Writes `.handoff/docs/_requirements_summary.json` for the VSCode
/// extension (P0 §2.7 — the extension never calls MCP tools, it only reads
/// `.handoff/` files directly). Called after every `handoff_doc_verify`
/// mutation that can affect requirement progress (P0 §3.4).
///
/// When `docs` has no `SubItem`s to aggregate (no docs at all, or every doc
/// has no verification matrix / no sub_items), no file is written — an
/// empty summary file would be indistinguishable from "not yet computed"
/// to a FileWatcher-based reader, so we simply leave it absent (P0 §3.4).
pub(crate) fn write_requirements_summary(handoff_dir: &Path, docs: &[DocMetadata]) -> Result<()> {
    let summary = aggregate_requirements(docs);
    if summary.total == 0 {
        return Ok(());
    }
    ensure_docs_dir(handoff_dir)?;
    let path = docs_dir(handoff_dir).join("_requirements_summary.json");
    let body = serde_json::to_string_pretty(&summary)
        .context("failed to serialize requirements summary")?;
    crate::storage::atomic_write(&path, body.as_bytes())
        .context("failed to write _requirements_summary.json")?;
    Ok(())
}

/// One `SubItem` resolved by `stable_id` (t330.1), identifying exactly where
/// it lives so a caller can mutate it without re-scanning every doc.
pub(crate) struct ResolvedSubItem {
    pub(crate) doc_id: String,
    pub(crate) fragment_seq: usize,
    pub(crate) sub_item_index: usize,
    pub(crate) stable_id: String,
}

/// Scans every document's verification matrix (same walk as
/// `aggregate_requirements`) for `SubItem`s whose `stable_id` is in
/// `stable_ids`, and resolves each to its `(doc_id, fragment_seq,
/// sub_item_index)` location. `stable_id`s that match no `SubItem` anywhere
/// are returned as `unresolved` (t330.1 spec: non-fatal — the caller reports
/// them as warnings rather than failing the whole call).
pub(crate) fn resolve_stable_ids(
    handoff: &Path,
    stable_ids: &[String],
) -> Result<(Vec<ResolvedSubItem>, Vec<String>)> {
    let docs = read_all_docs(handoff)?;
    let mut resolved = Vec::new();
    let mut remaining: std::collections::HashSet<&str> =
        stable_ids.iter().map(String::as_str).collect();

    for doc in &docs {
        let Some(v) = &doc.verification else {
            continue;
        };
        for item in &v.items {
            let Some(fragment_seq) = item.fragment_seq else {
                continue;
            };
            for sub in &item.sub_items {
                let Some(stable_id) = sub.stable_id.as_deref() else {
                    continue;
                };
                if remaining.remove(stable_id) {
                    resolved.push(ResolvedSubItem {
                        doc_id: doc.id.clone(),
                        fragment_seq,
                        sub_item_index: sub.index,
                        stable_id: stable_id.to_string(),
                    });
                }
            }
        }
    }

    let unresolved: Vec<String> = stable_ids
        .iter()
        .filter(|id| remaining.contains(id.as_str()))
        .cloned()
        .collect();
    Ok((resolved, unresolved))
}

/// Appends (deduped) a `{target: doc_id, link_type: "requirement", label:
/// stable_id}` entry to `task_id`'s `task_links` for every id in `task_ids`.
/// Shared by `link_task` (which replaces `SubItem.task_ids` but always
/// *appends* the reverse link, since other SubItems may still reference the
/// same task) and `link_requirements_to_task` (t330.1, which appends on both
/// sides). Returns the subset of `task_ids` that could not be resolved to a
/// task directory.
fn add_reverse_task_links(
    handoff: &Path,
    doc_id: &str,
    stable_id: Option<&str>,
    task_ids: &[String],
) -> Result<Vec<String>> {
    let tasks_dir = handoff.join("tasks");
    let mut unresolved: Vec<String> = Vec::new();
    for task_id in task_ids {
        let Some(task_dir) = find_task_dir_by_id(&tasks_dir, task_id)? else {
            unresolved.push(task_id.clone());
            continue;
        };
        read_modify_write_task(&task_dir, |data, status| {
            let already_linked = data.task_links.iter().any(|l| {
                l.target == doc_id
                    && l.link_type == "requirement"
                    && l.label.as_deref() == stable_id
            });
            if !already_linked {
                data.task_links.push(TaskLink {
                    target: doc_id.to_string(),
                    link_type: "requirement".to_string(),
                    label: stable_id.map(str::to_string),
                });
                data.updated_at = Some(chrono::Utc::now().to_rfc3339());
            }
            Ok(status.to_string())
        })?;
    }
    Ok(unresolved)
}

/// t323/M4: removes the `{target: doc.id, link_type: "requirement", label:
/// stable_id}` reverse link from each task in `removed_task_ids`'s
/// `task_links`. `link_task` calls this after replacing one `SubItem`'s
/// `task_ids`, for the tasks that fell out of that replacement.
///
/// Each `SubItem` owns exactly one reverse-link entry per task, labeled with
/// its own `stable_id` (see `add_reverse_task_links`, which keys existence
/// checks on `label == stable_id`) — and `stable_id`s are unique per
/// document (`collect_stable_ids`/`derive_stable_id` enforce this on
/// creation). So removing `(doc.id, "requirement", stable_id)` only ever
/// touches the entry this specific `SubItem` created; it can never affect an
/// entry another `SubItem` owns for the same task under its own label (t2
/// linked from a sibling SubItem keeps *that* SubItem's own reverse-link
/// entry — spec wiki/210 M4 test 2). The doc-wide scan below is a defensive
/// guard against that invariant being violated (e.g. duplicate/legacy
/// stable_ids): only skip the removal if some *other* SubItem's `task_ids`
/// still lists the task under the *same* `stable_id` we're about to remove.
///
/// Returns the subset of `removed_task_ids` whose reverse link was actually
/// removed (i.e. a matching `task_links` entry existed to remove and the
/// defensive guard did not block it).
fn remove_stale_reverse_links(
    handoff: &Path,
    doc: &DocMetadata,
    stable_id: &str,
    removed_task_ids: &[String],
) -> Result<Vec<String>> {
    let tasks_dir = handoff.join("tasks");
    let mut removed: Vec<String> = Vec::new();

    // Flatten every SubItem's (stable_id, task_ids) across all fragments so
    // membership checks below are simple linear scans — doc-scale SubItem
    // counts (hundreds, not millions) make this O(n) sufficient (wiki/210
    // design decision).
    let all_sub_items: Vec<(&str, &[String])> = doc
        .verification
        .iter()
        .flat_map(|v| v.items.iter())
        .flat_map(|item| item.sub_items.iter())
        .filter_map(|sub| {
            sub.stable_id
                .as_deref()
                .map(|id| (id, sub.task_ids.as_slice()))
        })
        .collect();

    for task_id in removed_task_ids {
        // Defensive guard only: true whenever another SubItem happens to
        // share this exact stable_id and still references the task — should
        // never occur since stable_ids are unique per document.
        let still_referenced_under_same_label =
            all_sub_items.iter().any(|(other_stable_id, task_ids)| {
                *other_stable_id == stable_id && task_ids.iter().any(|t| t == task_id)
            });
        if still_referenced_under_same_label {
            continue;
        }

        let Some(task_dir) = find_task_dir_by_id(&tasks_dir, task_id)? else {
            continue;
        };
        let mut did_remove = false;
        read_modify_write_task(&task_dir, |data, status| {
            let before = data.task_links.len();
            data.task_links.retain(|l| {
                !(l.target == doc.id
                    && l.link_type == "requirement"
                    && l.label.as_deref() == Some(stable_id))
            });
            if data.task_links.len() != before {
                did_remove = true;
                data.updated_at = Some(chrono::Utc::now().to_rfc3339());
            }
            Ok(status.to_string())
        })?;
        if did_remove {
            removed.push(task_id.clone());
        }
    }

    Ok(removed)
}

/// `handoff_update_task(task.requirement_ids=[...])`: removes `task_id` from
/// the `SubItem.task_ids` of each `stable_id` in `removed_stable_ids`, and
/// removes the corresponding `task_links` entry on the task side. This is
/// the inverse of `link_requirements_to_task`.
pub(crate) fn unlink_requirements_from_task(
    handoff: &Path,
    task_id: &str,
    removed_stable_ids: &[String],
) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    if removed_stable_ids.is_empty() {
        return Ok(warnings);
    }

    let (resolved, unresolved) = resolve_stable_ids(handoff, removed_stable_ids)?;
    if !unresolved.is_empty() {
        warnings.push(format!(
            "Could not resolve requirement stable_id(s) for unlinking: {}",
            unresolved.join(", ")
        ));
    }

    let mut by_doc: std::collections::BTreeMap<String, Vec<&ResolvedSubItem>> =
        std::collections::BTreeMap::new();
    for r in &resolved {
        by_doc.entry(r.doc_id.clone()).or_default().push(r);
    }

    for (doc_id, items) in by_doc {
        let mut doc = resolve_doc(handoff, &doc_id)?
            .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;
        let v = verification_mut(&mut doc, &doc_id)?;
        for r in &items {
            let item = find_item_mut(v, r.fragment_seq, &doc_id)?;
            let sub = find_sub_item_mut(item, r.sub_item_index, r.fragment_seq, &doc_id)?;
            sub.task_ids.retain(|t| t != task_id);
        }
        v.updated_at = chrono::Utc::now().to_rfc3339();
        v.status = recompute_verification_status(&v.items);
        write_doc(handoff, &doc)?;

        let tasks_dir = handoff.join("tasks");
        if let Some(task_dir) = find_task_dir_by_id(&tasks_dir, task_id)? {
            read_modify_write_task(&task_dir, |data, status| {
                for r in &items {
                    data.task_links.retain(|l| {
                        !(l.target == doc_id
                            && l.link_type == "requirement"
                            && l.label.as_deref() == Some(r.stable_id.as_str()))
                    });
                }
                data.updated_at = Some(chrono::Utc::now().to_rfc3339());
                Ok(status.to_string())
            })?;
        }
    }

    if !resolved.is_empty() {
        let all_docs = read_all_docs(handoff)?;
        write_requirements_summary(handoff, &all_docs)?;
    }

    Ok(warnings)
}

/// Maps a task status to an implied `dev_stage` ordinal for the
/// min-of-linked-tasks computation. Higher = further along.
/// `skipped` returns `None` — excluded from the computation.
fn implied_dev_stage_ord(task_status: &str) -> Option<u8> {
    match task_status {
        "todo" | "blocked" => Some(0),
        "in_progress" => Some(1),
        "review" | "done" => Some(2),
        _ => None,
    }
}

fn dev_stage_from_ord(ord: u8) -> &'static str {
    match ord {
        0 => "not_started",
        1 => "in_progress",
        _ => "implemented",
    }
}

/// Propagates task-status changes to the `dev_stage` of linked requirement
/// SubItems using a min-of-linked-tasks strategy: the SubItem's `dev_stage`
/// is set to the minimum implied `dev_stage` across all non-skipped linked
/// tasks. If a SubItem's current `dev_stage` is `"tested"` or `"verified"`,
/// it is protected (those stages are manual-only).
///
/// Called from `update_task` after a status transition.
pub(crate) fn propagate_dev_stage_for_task(handoff: &Path, task_links: &[TaskLink]) -> Result<()> {
    let requirement_stable_ids: Vec<String> = task_links
        .iter()
        .filter(|l| l.link_type == "requirement")
        .filter_map(|l| l.label.clone())
        .collect();
    if requirement_stable_ids.is_empty() {
        return Ok(());
    }

    let (resolved, _unresolved) = resolve_stable_ids(handoff, &requirement_stable_ids)?;
    if resolved.is_empty() {
        return Ok(());
    }

    let tasks_dir = handoff.join("tasks");

    let mut by_doc: std::collections::BTreeMap<String, Vec<&ResolvedSubItem>> =
        std::collections::BTreeMap::new();
    for r in &resolved {
        by_doc.entry(r.doc_id.clone()).or_default().push(r);
    }

    let mut any_changed = false;

    for (doc_id, items) in by_doc {
        let mut doc = match resolve_doc(handoff, &doc_id)? {
            Some(d) => d,
            None => continue,
        };
        let v = match doc.verification.as_mut() {
            Some(v) => v,
            None => continue,
        };

        let mut doc_changed = false;
        for r in &items {
            let item = match v
                .items
                .iter_mut()
                .find(|i| i.fragment_seq == Some(r.fragment_seq))
            {
                Some(i) => i,
                None => continue,
            };
            let sub = match item.sub_items.get_mut(r.sub_item_index) {
                Some(s) => s,
                None => continue,
            };

            let current = sub.dev_stage.as_deref().unwrap_or("not_started");
            if current == "tested" || current == "verified" {
                continue;
            }

            let mut min_ord: Option<u8> = None;
            for tid in &sub.task_ids {
                let status = match task_status_from_dir(&tasks_dir, tid) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                if let Some(ord) = implied_dev_stage_ord(&status) {
                    min_ord = Some(match min_ord {
                        Some(m) => m.min(ord),
                        None => ord,
                    });
                }
            }

            if let Some(ord) = min_ord {
                let new_stage = dev_stage_from_ord(ord);
                if current != new_stage {
                    sub.dev_stage = Some(new_stage.to_string());
                    doc_changed = true;
                }
            }
        }

        if doc_changed {
            v.updated_at = chrono::Utc::now().to_rfc3339();
            v.status = recompute_verification_status(&v.items);
            write_doc(handoff, &doc)?;
            any_changed = true;
        }
    }

    if any_changed {
        let all_docs = read_all_docs(handoff)?;
        write_requirements_summary(handoff, &all_docs)?;
    }

    Ok(())
}

/// Reads the current status of a task by its id. Returns the status string
/// (e.g. "done", "in_progress").
fn task_status_from_dir(tasks_dir: &Path, task_id: &str) -> Result<String> {
    let task_dir = find_task_dir_by_id(tasks_dir, task_id)?
        .ok_or_else(|| anyhow::anyhow!("Task not found: {task_id}"))?;
    let (_data, status) =
        read_task(&task_dir)?.ok_or_else(|| anyhow::anyhow!("Task file not found: {task_id}"))?;
    Ok(status)
}

/// `handoff_update_task(task.requirement_ids=[...])` (t330.1): resolves each
/// stable_id to its `SubItem`, appends (deduped) `task_id` to
/// `SubItem.task_ids`, and appends (deduped) the mirrored `TaskLink` on the
/// task side — see `add_reverse_task_links`. Unlike
/// `handoff_doc_verify(action="link_task")`, which *replaces*
/// `SubItem.task_ids` wholesale, this APPENDS: `requirement_ids` is meant to
/// incrementally attach a task to more requirements over time without
/// clobbering links other tasks already hold on the same SubItem (design
/// decision recorded on t330.1). Returns warnings for any stable_id that
/// resolved to no SubItem; those are non-fatal.
pub(crate) fn link_requirements_to_task(
    handoff: &Path,
    task_id: &str,
    stable_ids: &[String],
) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    if stable_ids.is_empty() {
        return Ok(warnings);
    }

    let (resolved, unresolved) = resolve_stable_ids(handoff, stable_ids)?;
    if !unresolved.is_empty() {
        warnings.push(format!(
            "Could not resolve requirement stable_id(s): {}",
            unresolved.join(", ")
        ));
    }

    // Group by doc_id so each document is read-modified-written once even
    // when several resolved SubItems live in the same doc.
    let mut by_doc: std::collections::BTreeMap<String, Vec<&ResolvedSubItem>> =
        std::collections::BTreeMap::new();
    for r in &resolved {
        by_doc.entry(r.doc_id.clone()).or_default().push(r);
    }

    for (doc_id, items) in by_doc {
        let mut doc = resolve_doc(handoff, &doc_id)?
            .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;
        let v = verification_mut(&mut doc, &doc_id)?;
        for r in &items {
            let item = find_item_mut(v, r.fragment_seq, &doc_id)?;
            let sub = find_sub_item_mut(item, r.sub_item_index, r.fragment_seq, &doc_id)?;
            if !sub.task_ids.iter().any(|t| t == task_id) {
                sub.task_ids.push(task_id.to_string());
            }
        }
        v.updated_at = chrono::Utc::now().to_rfc3339();
        v.status = recompute_verification_status(&v.items);
        write_doc(handoff, &doc)?;

        for r in &items {
            let unresolved_tasks = add_reverse_task_links(
                handoff,
                &doc_id,
                Some(r.stable_id.as_str()),
                &[task_id.to_string()],
            )?;
            // `task_id` is the caller's own task (already resolved by
            // update_task before calling this function), so this should
            // never happen — surfaced as a warning rather than silently
            // dropped in case of a race with a concurrent task deletion.
            if !unresolved_tasks.is_empty() {
                warnings.push(format!(
                    "Could not resolve task id {task_id} while linking reverse link for {}",
                    r.stable_id
                ));
            }
        }
    }

    if !resolved.is_empty() {
        let all_docs = read_all_docs(handoff)?;
        write_requirements_summary(handoff, &all_docs)?;
    }

    Ok(warnings)
}

/// `handoff_doc_verify` — generate/check/skip/sync/set_refs a document's
/// verification matrix (wiki/140-verification-matrix.md §4.1).
pub fn handle_doc_verify(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let project_dir = &ctx.project_dir;
    let handoff = &ctx.handoff_dir;

    let doc_id = arguments
        .get("doc_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'doc_id' is required"))?;
    let action = arguments
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'action' is required"))?;

    let mut doc = resolve_doc(handoff, doc_id)?
        .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;

    // `suggest_refs` is read-only (it never mutates the verification matrix,
    // only proposes candidates for the caller to feed into `set_refs`), and
    // its response shape (a `suggestions` list) differs from every other
    // action's mutation-count summary — handled separately, before the
    // shared mutate-then-write flow below.
    if action == "suggest_refs" {
        let v = doc.verification.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "No verification matrix exists for document {doc_id}; use action='generate' first"
            )
        })?;
        let suggestions = suggest_refs(project_dir, &doc, v);
        return Ok(to_json(&json!({
            "doc_id": doc.id,
            "suggestions": suggestions,
        })));
    }

    let now = chrono::Utc::now().to_rfc3339();
    let mut warnings: Vec<String> = Vec::new();

    match action {
        "generate" => {
            if doc.verification.is_some() {
                anyhow::bail!(
                    "Verification matrix already exists for document {doc_id}; use action='sync' to re-sync it instead"
                );
            }
            let skip_seqs: Vec<usize> = arguments
                .get("skip_seqs")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_u64())
                        .map(|n| n as usize)
                        .collect()
                })
                .unwrap_or_default();

            let items: Vec<VerificationItem> = doc
                .sections
                .iter()
                .map(|s| VerificationItem {
                    fragment_seq: Some(s.seq),
                    heading: s.heading.clone(),
                    status: if skip_seqs.contains(&s.seq) {
                        "skipped".to_string()
                    } else {
                        "pending".to_string()
                    },
                    impl_refs: Vec::new(),
                    test_refs: Vec::new(),
                    reviewer: None,
                    verified_at: None,
                    notes: String::new(),
                    content_hash_at_verify: None,
                    category: "section".to_string(),
                    sub_items: Vec::new(),
                    label: None,
                })
                .collect();

            doc.verification = Some(Verification {
                status: recompute_verification_status(&items),
                created_at: now.clone(),
                updated_at: now,
                items,
            });
        }
        "check" => {
            let fragment_seqs = required_fragment_seqs(arguments)?;
            let reviewer = arguments
                .get("reviewer")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let notes = arguments
                .get("notes")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let sub_item_id = arguments
                .get("sub_item_id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let sub_item_index = arguments
                .get("sub_item_index")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);

            for fragment_seq in fragment_seqs {
                let section_hash = doc
                    .sections
                    .iter()
                    .find(|s| s.seq == fragment_seq)
                    .map(|s| s.content_hash.clone());

                let v = verification_mut(&mut doc, doc_id)?;
                let item = find_item_mut(v, fragment_seq, doc_id)?;

                if sub_item_id.is_some() || sub_item_index.is_some() {
                    let (sub, warning) = find_sub_item_mut_by_id(
                        item,
                        sub_item_id.as_deref(),
                        sub_item_index,
                        fragment_seq,
                        doc_id,
                    )?;
                    if let Some(w) = warning {
                        warnings.push(w);
                    }
                    sub.status = "verified".to_string();
                    sub.verified_at = Some(now.clone());
                    if reviewer.is_some() {
                        sub.reviewer = reviewer.clone();
                    }
                    if let Some(notes) = &notes {
                        sub.notes = notes.clone();
                    }
                } else {
                    item.status = "verified".to_string();
                    item.verified_at = Some(now.clone());
                    if reviewer.is_some() {
                        item.reviewer = reviewer.clone();
                    }
                    if let Some(notes) = &notes {
                        item.notes = notes.clone();
                    }
                    item.content_hash_at_verify = section_hash;
                }
                v.updated_at = now.clone();
                v.status = recompute_verification_status(&v.items);
            }
        }
        "check_all" => {
            let reviewer = arguments
                .get("reviewer")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let notes = arguments
                .get("notes")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let sections = doc.sections.clone();

            let v = verification_mut(&mut doc, doc_id)?;
            for item in v.items.iter_mut() {
                let section_hash = item.fragment_seq.and_then(|seq| {
                    sections
                        .iter()
                        .find(|s| s.seq == seq)
                        .map(|s| s.content_hash.clone())
                });
                item.status = "verified".to_string();
                item.verified_at = Some(now.clone());
                if reviewer.is_some() {
                    item.reviewer = reviewer.clone();
                }
                if let Some(notes) = &notes {
                    item.notes = notes.clone();
                }
                item.content_hash_at_verify = section_hash;

                // v2: check_all also verifies every sub_item (spec §7.2).
                for sub in item.sub_items.iter_mut() {
                    sub.status = "verified".to_string();
                    sub.verified_at = Some(now.clone());
                    if reviewer.is_some() {
                        sub.reviewer = reviewer.clone();
                    }
                }
            }
            v.updated_at = now.clone();
            v.status = recompute_verification_status(&v.items);
        }
        "skip" => {
            let fragment_seq = required_fragment_seq(arguments)?;
            let sub_item_id = arguments
                .get("sub_item_id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let sub_item_index = arguments
                .get("sub_item_index")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);

            let v = verification_mut(&mut doc, doc_id)?;
            let item = find_item_mut(v, fragment_seq, doc_id)?;
            if sub_item_id.is_some() || sub_item_index.is_some() {
                let (sub, warning) = find_sub_item_mut_by_id(
                    item,
                    sub_item_id.as_deref(),
                    sub_item_index,
                    fragment_seq,
                    doc_id,
                )?;
                if let Some(w) = warning {
                    warnings.push(w);
                }
                sub.status = "skipped".to_string();
            } else {
                item.status = "skipped".to_string();
            }
            v.updated_at = now.clone();
            v.status = recompute_verification_status(&v.items);
        }
        "add_item" => {
            let doc_slug = doc.slug.clone();
            let v = verification_mut(&mut doc, doc_id)?;
            match arguments.get("fragment_seq").and_then(|v| v.as_u64()) {
                None => {
                    // Freeform top-level item (spec §7.2): fragment_seq
                    // omitted/null, label required.
                    let label = arguments
                        .get("label")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "'label' is required for add_item when 'fragment_seq' is omitted"
                            )
                        })?
                        .to_string();
                    let category = arguments
                        .get("category")
                        .and_then(|v| v.as_str())
                        .unwrap_or("visual")
                        .to_string();
                    v.items.push(VerificationItem {
                        fragment_seq: None,
                        heading: label.clone(),
                        status: "pending".to_string(),
                        impl_refs: Vec::new(),
                        test_refs: Vec::new(),
                        reviewer: None,
                        verified_at: None,
                        notes: String::new(),
                        content_hash_at_verify: None,
                        category,
                        sub_items: Vec::new(),
                        label: Some(label),
                    });
                }
                Some(seq) => {
                    // Sub-item on an existing section item (spec §7.2):
                    // fragment_seq given, description required.
                    let fragment_seq = seq as usize;
                    let description = arguments
                        .get("description")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "'description' is required for add_item when 'fragment_seq' is given"
                            )
                        })?
                        .to_string();
                    let category = arguments
                        .get("category")
                        .and_then(|v| v.as_str())
                        .unwrap_or("requirement")
                        .to_string();

                    // Requirements-traceability P0 §2.3 (t300.3): a brand
                    // new SubItem never has a stable_id yet, so mint one —
                    // unless its description fuzzy-matches an existing
                    // SubItem elsewhere in the matrix, in which case it
                    // re-links to that SubItem's (immutable) stable_id
                    // instead of minting a fresh one (§2.3 "再マッチング").
                    let existing_ids = collect_stable_ids(v);
                    let reused_id = find_fuzzy_match_stable_id(v, &description);
                    let (stable_id, warning) = match reused_id {
                        Some(id) => (id, None),
                        None => {
                            let heading = v
                                .items
                                .iter()
                                .find(|i| i.fragment_seq == Some(fragment_seq))
                                .map(|i| i.heading.clone())
                                .unwrap_or_default();
                            let (id, warning) =
                                derive_stable_id(&doc_slug, &heading, &description, &existing_ids);
                            (id, warning)
                        }
                    };
                    if let Some(w) = warning {
                        warnings.push(w);
                    }

                    let item = find_item_mut(v, fragment_seq, doc_id)?;
                    let index = item.sub_items.len();
                    item.sub_items.push(SubItem {
                        index,
                        description,
                        category,
                        stable_id: Some(stable_id),
                        ..Default::default()
                    });
                }
            }
            v.updated_at = now.clone();
            v.status = recompute_verification_status(&v.items);
        }
        "sync" => {
            let sections = doc.sections.clone();
            let v = verification_mut(&mut doc, doc_id)?;
            let current_seqs: std::collections::HashSet<usize> =
                sections.iter().map(|s| s.seq).collect();
            // Freeform items (fragment_seq=None, v2) are never section-tied,
            // so `sync` always keeps them — only section-tied items whose
            // seq no longer exists are dropped.
            v.items.retain(|i| match i.fragment_seq {
                Some(seq) => current_seqs.contains(&seq),
                None => true,
            });
            let existing_seqs: std::collections::HashSet<usize> =
                v.items.iter().filter_map(|i| i.fragment_seq).collect();
            for s in &sections {
                if !existing_seqs.contains(&s.seq) {
                    v.items.push(VerificationItem {
                        fragment_seq: Some(s.seq),
                        heading: s.heading.clone(),
                        status: "pending".to_string(),
                        impl_refs: Vec::new(),
                        test_refs: Vec::new(),
                        reviewer: None,
                        verified_at: None,
                        notes: String::new(),
                        content_hash_at_verify: None,
                        category: "section".to_string(),
                        sub_items: Vec::new(),
                        label: None,
                    });
                }
            }
            // Sort section-tied items by seq; freeform items (None) sort
            // last, in their prior relative order (stable sort).
            v.items.sort_by_key(|i| (i.fragment_seq.is_none(), i.fragment_seq));
            v.updated_at = now.clone();
            v.status = recompute_verification_status(&v.items);
        }
        "set_refs" => {
            let fragment_seq = required_fragment_seq(arguments)?;
            let impl_refs = arguments.get("impl_refs").map(code_refs_from_value);
            let test_refs = arguments.get("test_refs").map(code_refs_from_value);
            let sub_item_id = arguments
                .get("sub_item_id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let sub_item_index = arguments
                .get("sub_item_index")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);

            let v = verification_mut(&mut doc, doc_id)?;
            let item = find_item_mut(v, fragment_seq, doc_id)?;
            if sub_item_id.is_some() || sub_item_index.is_some() {
                let (sub, warning) = find_sub_item_mut_by_id(
                    item,
                    sub_item_id.as_deref(),
                    sub_item_index,
                    fragment_seq,
                    doc_id,
                )?;
                if let Some(w) = warning {
                    warnings.push(w);
                }
                if let Some(impl_refs) = impl_refs {
                    sub.impl_refs = impl_refs;
                }
                if let Some(test_refs) = test_refs {
                    sub.test_refs = test_refs;
                }
            } else {
                if let Some(impl_refs) = impl_refs {
                    item.impl_refs = impl_refs;
                }
                if let Some(test_refs) = test_refs {
                    item.test_refs = test_refs;
                }
            }
            v.updated_at = now.clone();
            v.status = recompute_verification_status(&v.items);
        }
        "set_dev_stage" => {
            let fragment_seq = required_fragment_seq(arguments)?;
            let dev_stage = arguments
                .get("dev_stage")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("'dev_stage' is required for set_dev_stage"))?;
            const VALID_DEV_STAGES: [&str; 5] =
                ["not_started", "in_progress", "implemented", "tested", "verified"];
            if !VALID_DEV_STAGES.contains(&dev_stage) {
                anyhow::bail!(
                    "Invalid dev_stage '{dev_stage}'; expected one of {VALID_DEV_STAGES:?}"
                );
            }
            let sub_item_id = arguments
                .get("sub_item_id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let sub_item_index = arguments
                .get("sub_item_index")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            if sub_item_id.is_none() && sub_item_index.is_none() {
                anyhow::bail!(
                    "set_dev_stage requires 'sub_item_id' or 'sub_item_index'; dev_stage is a SubItem-only field"
                );
            }

            let v = verification_mut(&mut doc, doc_id)?;
            let item = find_item_mut(v, fragment_seq, doc_id)?;
            let (sub, warning) = find_sub_item_mut_by_id(
                item,
                sub_item_id.as_deref(),
                sub_item_index,
                fragment_seq,
                doc_id,
            )?;
            if let Some(w) = warning {
                warnings.push(w);
            }
            sub.dev_stage = Some(dev_stage.to_string());
            v.updated_at = now.clone();
            v.status = recompute_verification_status(&v.items);
        }
        "set_priority" => {
            let fragment_seq = required_fragment_seq(arguments)?;
            let priority = arguments
                .get("priority")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("'priority' is required for set_priority"))?;
            const VALID_PRIORITIES: [&str; 4] = ["P0", "P1", "P2", "P3"];
            if !VALID_PRIORITIES.contains(&priority) {
                anyhow::bail!("Invalid priority '{priority}'; expected one of {VALID_PRIORITIES:?}");
            }
            let sub_item_id = arguments
                .get("sub_item_id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let sub_item_index = arguments
                .get("sub_item_index")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            if sub_item_id.is_none() && sub_item_index.is_none() {
                anyhow::bail!(
                    "set_priority requires 'sub_item_id' or 'sub_item_index'; priority is a SubItem-only field"
                );
            }

            let v = verification_mut(&mut doc, doc_id)?;
            let item = find_item_mut(v, fragment_seq, doc_id)?;
            let (sub, warning) = find_sub_item_mut_by_id(
                item,
                sub_item_id.as_deref(),
                sub_item_index,
                fragment_seq,
                doc_id,
            )?;
            if let Some(w) = warning {
                warnings.push(w);
            }
            sub.priority = Some(priority.to_string());
            v.updated_at = now.clone();
            v.status = recompute_verification_status(&v.items);
        }
        "link_task" => {
            let fragment_seq = required_fragment_seq(arguments)?;
            let task_ids = arguments
                .get("task_ids")
                .map(string_array_value)
                .ok_or_else(|| anyhow::anyhow!("'task_ids' is required for link_task"))?;
            let sub_item_id = arguments
                .get("sub_item_id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let sub_item_index = arguments
                .get("sub_item_index")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            if sub_item_id.is_none() && sub_item_index.is_none() {
                anyhow::bail!(
                    "link_task requires 'sub_item_id' or 'sub_item_index'; task_ids is a SubItem-only field"
                );
            }

            let v = verification_mut(&mut doc, doc_id)?;
            let item = find_item_mut(v, fragment_seq, doc_id)?;
            let (sub, warning) = find_sub_item_mut_by_id(
                item,
                sub_item_id.as_deref(),
                sub_item_index,
                fragment_seq,
                doc_id,
            )?;
            if let Some(w) = warning {
                warnings.push(w);
            }
            let stable_id = sub.stable_id.clone();
            let old_task_ids = sub.task_ids.clone();
            sub.task_ids = task_ids.clone();
            v.updated_at = now.clone();
            v.status = recompute_verification_status(&v.items);

            // Reverse link (spec §3.1): every linked task's task_links gets
            // a `{target: doc_id, link_type: "requirement", label: stable_id}`
            // entry, deduped so re-calling link_task with the same task_ids
            // is idempotent.
            let unresolved = add_reverse_task_links(handoff, doc_id, stable_id.as_deref(), &task_ids)?;
            if !unresolved.is_empty() {
                warnings.push(format!(
                    "Could not resolve task id(s) for linking: {}",
                    unresolved.join(", ")
                ));
            }

            // t323/M4: tasks that were previously linked but are not in the
            // new `task_ids` must have this SubItem's reverse-link entry
            // (labeled with its own `stable_id`) removed. A task still
            // linked from a *different* SubItem keeps that sibling's own
            // labeled entry untouched — see `remove_stale_reverse_links`.
            if let Some(stable_id) = stable_id.as_deref() {
                let removed_task_ids: Vec<String> = old_task_ids
                    .iter()
                    .filter(|id| !task_ids.contains(id))
                    .cloned()
                    .collect();
                if !removed_task_ids.is_empty() {
                    remove_stale_reverse_links(handoff, &doc, stable_id, &removed_task_ids)?;
                }
            }
        }
        "backfill_stable_ids" => {
            let doc_slug = doc.slug.clone();
            let v = verification_mut(&mut doc, doc_id)?;
            let mut existing_ids = collect_stable_ids(v);
            let mut backfilled = 0u64;

            for item in v.items.iter_mut() {
                let heading = item.heading.clone();
                for sub in item.sub_items.iter_mut() {
                    if sub.stable_id.is_some() {
                        continue;
                    }
                    let (id, warning) =
                        derive_stable_id(&doc_slug, &heading, &sub.description, &existing_ids);
                    if let Some(w) = warning {
                        warnings.push(w);
                    }
                    existing_ids.insert(id.clone());
                    sub.stable_id = Some(id);
                    backfilled += 1;
                }
            }
            v.updated_at = now.clone();
            v.status = recompute_verification_status(&v.items);

            write_doc(handoff, &doc)?;
            let all_docs = read_all_docs(handoff)?;
            write_requirements_summary(handoff, &all_docs)?;

            let v = doc
                .verification
                .as_ref()
                .expect("verification was just set/mutated above");
            let counts = count_verification(&doc, v);
            return Ok(to_json(&json!({
                "doc_id": doc.id,
                "backfilled": backfilled,
                "verification_status": v.status,
                "checked": counts.checked,
                "skipped": counts.skipped,
                "pending": counts.pending,
                "total": counts.total,
                "stale": counts.stale,
                "warnings": warnings,
            })));
        }
        other => anyhow::bail!(
            "Unknown action '{other}'; expected one of generate, check, check_all, skip, sync, set_refs, set_dev_stage, set_priority, link_task, add_item, backfill_stable_ids, suggest_refs"
        ),
    }

    write_doc(handoff, &doc)?;

    // Requirements-traceability integration-reform §3.2: refresh the
    // VSCode-extension-facing `_requirements_summary.json` cache after any
    // action that can change requirement (SubItem) progress or composition.
    // `add_item` is included here — a new SubItem changes `total`'s
    // composition and the extension's Requirements Explorer should reflect
    // it immediately. This does not double the cost for `req_import`'s bulk
    // path: that handler calls `add_item`'s underlying mutation directly
    // (not through this action dispatch) and refreshes the summary itself
    // exactly once after the whole batch. `backfill_stable_ids` (§3.3) is
    // not listed here — it early-returns above with its own write +
    // summary refresh, since its response shape (a `backfilled` count)
    // differs from every other action's mutation-count summary.
    const SUMMARY_REFRESH_ACTIONS: [&str; 10] = [
        "generate",
        "check",
        "check_all",
        "skip",
        "sync",
        "set_refs",
        "set_dev_stage",
        "set_priority",
        "add_item",
        "link_task",
    ];
    if SUMMARY_REFRESH_ACTIONS.contains(&action) {
        let all_docs = read_all_docs(handoff)?;
        write_requirements_summary(handoff, &all_docs)?;
    }

    let v = doc
        .verification
        .as_ref()
        .expect("verification was just set/mutated above");
    let counts = count_verification(&doc, v);

    Ok(to_json(&json!({
        "doc_id": doc.id,
        "verification_status": v.status,
        "checked": counts.checked,
        "skipped": counts.skipped,
        "pending": counts.pending,
        "total": counts.total,
        "stale": counts.stale,
        "warnings": warnings,
    })))
}

fn required_fragment_seq(arguments: &Value) -> Result<usize> {
    arguments
        .get("fragment_seq")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .ok_or_else(|| anyhow::anyhow!("'fragment_seq' is required for this action"))
}

/// Like `required_fragment_seq`, but accepts `fragment_seq` as either a
/// single number (backward compat) or an array of numbers (batch `check`).
fn required_fragment_seqs(arguments: &Value) -> Result<Vec<usize>> {
    match arguments.get("fragment_seq") {
        Some(Value::Array(arr)) => {
            let seqs: Vec<usize> = arr
                .iter()
                .filter_map(|v| v.as_u64())
                .map(|n| n as usize)
                .collect();
            if seqs.is_empty() {
                anyhow::bail!("'fragment_seq' array must contain at least one section seq");
            }
            Ok(seqs)
        }
        _ => required_fragment_seq(arguments).map(|seq| vec![seq]),
    }
}

fn verification_mut<'a>(doc: &'a mut DocMetadata, doc_id: &str) -> Result<&'a mut Verification> {
    doc.verification.as_mut().ok_or_else(|| {
        anyhow::anyhow!(
            "No verification matrix exists for document {doc_id}; use action='generate' first"
        )
    })
}

fn find_item_mut<'a>(
    v: &'a mut Verification,
    fragment_seq: usize,
    doc_id: &str,
) -> Result<&'a mut VerificationItem> {
    v.items
        .iter_mut()
        .find(|i| i.fragment_seq == Some(fragment_seq))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No verification item at fragment_seq={fragment_seq} for document {doc_id}"
            )
        })
}

/// Requirements-traceability P0 (`.handoff/docs/_doc.req-traceability-mcp-plan.md`
/// §2.3, t300.3): derives a `SubItem::stable_id` the first time one is
/// assigned. Returns `(stable_id, warning)` — `warning` is `Some` only when
/// the derived id collided with `existing_ids` and a numeric suffix (`-2`,
/// `-3`, ...) had to be appended to disambiguate it.
///
/// Derivation (never re-run once a `stable_id` exists — callers are
/// responsible for that immutability check; this function always mints):
/// 1. Category prefix from `doc_slug`: `req-c(\d+)...` -> `C{n}` (zero-padded
///    as found, e.g. `req-c01-...` -> `C01`). No match -> the whole slug,
///    uppercased with `-`/`_` normalized to `-`.
/// 2. Requirement-id prefix in `description` (wiki/210-req-traceability-refinement.md
///    §M2): a leading known prefix (`FR`, `NFR`, `REQ`, `CR`, `TR`, `SR`,
///    `UC`, `TC`) + `-\d+` (e.g. `FR-001: USB CDC...` -> `FR-001`). When
///    present, the id is `{category}-{req_id}` (e.g. `C01-FR-001`) —
///    tried *before* the heading/description-number derivation below, since
///    a requirement-id prefix is a stronger signal than a bare leading
///    numeral. Only the known prefix list is recognized; a generic
///    `[A-Z]{1,5}-\d+` pattern is deliberately not used, since it would
///    false-positive on text like `A-1 pin header`.
/// 3. Heading number from `heading`: leading `#`s + optional whitespace,
///    then a leading `\d[\d.]*` run (e.g. `## 2.1 基板外形` -> `2.1`).
/// 4. Description number: a leading `\d[\d.]*` run in `description` (e.g.
///    `2.1.1 外形形状定義` -> `2.1.1`). When present, the id is
///    `{category}-{desc_num}` (the description's own number already
///    subsumes the heading number in practice — e.g. `2.1.1` under heading
///    `2.1`). When absent, the id is `{category}-{heading_num}-{desc_slug}`
///    where `desc_slug` is the description lowercased with every non
///    ASCII-alphanumeric run collapsed to a single `-` (leading/trailing
///    hyphens trimmed), truncated to `slugify`'s default max length.
/// 5. Collision: while the candidate is in `existing_ids`, append `-2`,
///    `-3`, ... and return a warning describing the collision.
pub(crate) fn derive_stable_id(
    doc_slug: &str,
    heading: &str,
    description: &str,
    existing_ids: &std::collections::HashSet<String>,
) -> (String, Option<String>) {
    let category = extract_category_prefix(doc_slug);

    let base = if let Some(req_id) = extract_requirement_id(description.trim()) {
        format!("{category}-{req_id}")
    } else {
        let heading_num = extract_leading_number(heading.trim_start_matches('#').trim());
        let desc_num = extract_leading_number(description.trim());
        match desc_num {
            Some(n) => format!("{category}-{n}"),
            None => {
                let slug = slugify(description, DEFAULT_SLUGIFY_MAX_LEN);
                match heading_num {
                    Some(h) if !slug.is_empty() => format!("{category}-{h}-{slug}"),
                    Some(h) => format!("{category}-{h}"),
                    None if !slug.is_empty() => format!("{category}-{slug}"),
                    None => category.clone(),
                }
            }
        }
    };

    if !existing_ids.contains(&base) {
        return (base, None);
    }

    let mut suffix = 2;
    loop {
        let candidate = format!("{base}-{suffix}");
        if !existing_ids.contains(&candidate) {
            let warning =
                format!("stable_id {base:?} already exists; assigned {candidate:?} instead");
            return (candidate, Some(warning));
        }
        suffix += 1;
    }
}

/// Extracts the `C{n}` category prefix from a `req-c{n}-...`-shaped slug
/// (e.g. `req-c01-board-setup` -> `C01`, preserving the digits as written).
/// Falls back to the whole slug, uppercased with `_`/`-` normalized to `-`,
/// when the `req-c<digits>` pattern isn't found.
fn extract_category_prefix(doc_slug: &str) -> String {
    let lower = doc_slug.to_lowercase();
    if let Some(rest) = lower.strip_prefix("req-c") {
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            return format!("C{digits}");
        }
    }
    doc_slug
        .to_uppercase()
        .chars()
        .map(|c| if c == '_' { '-' } else { c })
        .collect()
}

/// Known requirement-id prefixes recognized by `extract_requirement_id`
/// (wiki/210-req-traceability-refinement.md §M2). Intentionally a fixed
/// allow-list rather than a generic `[A-Z]{1,5}-\d+` pattern — a generic
/// pattern would false-positive on ordinary text like `A-1 pin header`.
const KNOWN_REQUIREMENT_ID_PREFIXES: &[&str] = &["FR", "NFR", "REQ", "CR", "TR", "SR", "UC", "TC"];

/// Extracts a leading `{PREFIX}-{digits}` requirement id from `text` (e.g.
/// `"FR-001: USB CDC..."` -> `Some("FR-001")`), where `PREFIX` is one of
/// `KNOWN_REQUIREMENT_ID_PREFIXES`, matched case-insensitively but returned
/// in the list's canonical (upper) case. Only the longest matching known
/// prefix immediately followed by `-` and one or more ASCII digits at the
/// very start of `text` counts — no match anywhere else in the text is
/// considered, so `"see FR-001"` does not match (avoids over-eager minting
/// from incidental references inside a longer description).
fn extract_requirement_id(text: &str) -> Option<String> {
    // Try longest prefixes first so e.g. `NFR-005` isn't mistakenly matched
    // as `FR` against `NFR-005` slicing from the wrong offset (in practice
    // prefixes are disjoint by spelling, but sorting by length keeps the
    // intent explicit and future-proofs additions like `FR`/`NFRX`).
    let mut prefixes: Vec<&str> = KNOWN_REQUIREMENT_ID_PREFIXES.to_vec();
    prefixes.sort_by_key(|p| std::cmp::Reverse(p.len()));

    let upper = text.to_uppercase();
    for prefix in prefixes {
        let Some(rest) = upper.strip_prefix(prefix) else {
            continue;
        };
        let Some(digits_part) = rest.strip_prefix('-') else {
            continue;
        };
        let digit_len = digits_part
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .count();
        if digit_len == 0 {
            continue;
        }
        // `text` may be non-ASCII (e.g. Japanese) after the numeric run, but
        // the matched prefix+digits span is always pure ASCII, so byte
        // slicing on `text` at this offset is safe.
        let match_len = prefix.len() + 1 + digit_len;
        return Some(text[..match_len].to_uppercase());
    }
    None
}

/// Extracts a leading `\d[\d.]*` numeric run (e.g. `"2.1.1 外形"` -> `Some("2.1.1")`,
/// `"外形"` -> `None`). Trailing `.` on the run is trimmed (e.g. a heading
/// written as `"2.1."` yields `"2.1"`).
fn extract_leading_number(text: &str) -> Option<String> {
    let mut end = 0;
    for (i, c) in text.char_indices() {
        if c.is_ascii_digit() || c == '.' {
            end = i + c.len_utf8();
        } else {
            break;
        }
    }
    if end == 0 {
        return None;
    }
    let num = text[..end].trim_end_matches('.');
    if num.is_empty() || !num.chars().next().unwrap().is_ascii_digit() {
        None
    } else {
        Some(num.to_string())
    }
}

/// Default `max_len` passed to `slugify` by `derive_stable_id` (wiki/210
/// §M2) — keeps minted `stable_id`s readable instead of embedding an entire
/// long description.
const DEFAULT_SLUGIFY_MAX_LEN: usize = 40;

/// Slugifies free text for use as a `stable_id` fallback suffix: lowercased,
/// every run of non-ASCII-alphanumeric characters collapsed to a single `-`,
/// leading/trailing hyphens trimmed, then truncated to at most `max_len`
/// characters at a word (hyphen) boundary — i.e. the last complete
/// hyphen-separated word that still fits is kept, rather than cutting
/// mid-word (wiki/210-req-traceability-refinement.md §M2). Non-ASCII text
/// (e.g. Japanese) has no ASCII-alphanumeric characters at all, so it
/// collapses to an empty string — callers fall back further (heading number
/// alone, or bare category).
fn slugify(text: &str, max_len: usize) -> String {
    let mut out = String::new();
    let mut last_was_hyphen = true; // suppress leading hyphen
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_was_hyphen = false;
        } else if !last_was_hyphen {
            out.push('-');
            last_was_hyphen = true;
        }
    }
    let out = out.trim_end_matches('-').to_string();

    if out.len() <= max_len {
        return out;
    }
    // Truncate to max_len bytes (slug is pure ASCII, so byte length ==
    // char length here), then trim back to the last complete word: drop any
    // trailing partial word after the last '-' within the truncated slice,
    // falling back to a hard byte truncation only if there's no '-' at all
    // within the limit (a single word longer than max_len).
    let truncated = &out[..max_len];
    match truncated.rfind('-') {
        Some(idx) => truncated[..idx].to_string(),
        None => truncated.to_string(),
    }
}

/// Normalizes description text for fuzzy comparison: trims whitespace,
/// drops trailing Japanese/ASCII punctuation (`。`, `.`, `、`, `,`), and
/// lowercases ASCII.
fn normalize_for_match(text: &str) -> String {
    text.trim()
        .trim_end_matches(['。', '.', '、', ',', '！', '!', '？', '?'])
        .to_lowercase()
}

/// Requirements-traceability P0 §2.3 (t300.3): a deliberately simple
/// "fuzzy" match — used by `sync`/`add_item` to decide whether a new
/// SubItem's description should re-link to an existing SubItem's
/// `stable_id` rather than mint a new one. Per the plan (§2.3 note:
/// "lexsim は避ける"), this is plain normalization + substring containment,
/// not edit-distance — sufficient for the common cases (identical text
/// modulo trailing punctuation, or one description being a superset of the
/// other, e.g. a heading-numbered description added in front of existing
/// free text).
pub(crate) fn descriptions_fuzzy_match(a: &str, b: &str) -> bool {
    let na = normalize_for_match(a);
    let nb = normalize_for_match(b);
    if na.is_empty() || nb.is_empty() {
        return false;
    }
    na == nb || na.contains(&nb) || nb.contains(&na)
}

/// Collects every already-assigned `stable_id` across all `sub_items` in the
/// verification matrix (used as the collision set for `derive_stable_id`).
pub(crate) fn collect_stable_ids(v: &Verification) -> std::collections::HashSet<String> {
    v.items
        .iter()
        .flat_map(|i| i.sub_items.iter())
        .filter_map(|s| s.stable_id.clone())
        .collect()
}

/// Requirements-traceability P0 §2.3 (t300.3) "再マッチング": scans every
/// `sub_items` entry in the matrix for one whose description
/// `descriptions_fuzzy_match`es `description`, and returns its `stable_id`
/// so a newly observed SubItem with (near-)identical text re-links to the
/// existing requirement instead of minting a duplicate id. Returns `None`
/// when there is no match, or the match has no `stable_id` yet.
fn find_fuzzy_match_stable_id(v: &Verification, description: &str) -> Option<String> {
    v.items
        .iter()
        .flat_map(|i| i.sub_items.iter())
        .find(|s| descriptions_fuzzy_match(&s.description, description))
        .and_then(|s| s.stable_id.clone())
}

/// v2: finds a `SubItem` by `index` within `item.sub_items` (used by
/// `check`/`skip` when `sub_item_index` is given).
fn find_sub_item_mut<'a>(
    item: &'a mut VerificationItem,
    sub_index: usize,
    fragment_seq: usize,
    doc_id: &str,
) -> Result<&'a mut SubItem> {
    item.sub_items.get_mut(sub_index).ok_or_else(|| {
        anyhow::anyhow!(
            "No sub_item at index={sub_index} for fragment_seq={fragment_seq} on document {doc_id}"
        )
    })
}

/// Requirements-traceability P0 (`.handoff/docs/_doc.req-traceability-mcp-plan.md`
/// §2.5): finds a `SubItem` addressed primarily by its stable `sub_item_id`
/// (`SubItem::stable_id`), falling back to positional `sub_item_index` when
/// no id is given. When both are given and disagree, `sub_item_id` wins and
/// a warning describing the mismatch is returned alongside the match.
///
/// Wired into `check`/`skip` (this task). `set_refs`/`set_dev_stage`/
/// `set_priority` addressing is a follow-up task (t300.2).
fn find_sub_item_mut_by_id<'a>(
    item: &'a mut VerificationItem,
    sub_item_id: Option<&str>,
    sub_item_index: Option<usize>,
    fragment_seq: usize,
    doc_id: &str,
) -> Result<(&'a mut SubItem, Option<String>)> {
    match sub_item_id {
        Some(id) => {
            let by_index_matches = sub_item_index.is_some_and(|idx| {
                item.sub_items.get(idx).and_then(|s| s.stable_id.as_deref()) != Some(id)
            });
            let warning = by_index_matches.then(|| {
                format!(
                    "sub_item_id={id:?} and sub_item_index={:?} were both given and disagree; \
                     sub_item_id takes precedence for fragment_seq={fragment_seq} on document {doc_id}",
                    sub_item_index.unwrap()
                )
            });
            let sub = item
                .sub_items
                .iter_mut()
                .find(|s| s.stable_id.as_deref() == Some(id))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "No sub_item with stable_id={id:?} for fragment_seq={fragment_seq} on document {doc_id}"
                    )
                })?;
            Ok((sub, warning))
        }
        None => {
            let sub_index = sub_item_index.ok_or_else(|| {
                anyhow::anyhow!(
                    "Either 'sub_item_id' or 'sub_item_index' is required to address a sub_item"
                )
            })?;
            let sub = find_sub_item_mut(item, sub_index, fragment_seq, doc_id)?;
            Ok((sub, None))
        }
    }
}

/// `handoff_doc_verify_status` — verification matrix summary + optional
/// per-item detail with stale detection (wiki/140-verification-matrix.md
/// §4.2).
pub fn handle_doc_verify_status(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let doc_id = arguments
        .get("doc_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'doc_id' is required"))?;
    let include_items = arguments
        .get("include_items")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let format = arguments
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("json");

    let doc = resolve_doc(handoff, doc_id)?
        .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;

    let v = doc.verification.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "No verification matrix exists for document {doc_id}; use handoff_doc_verify(action='generate') first"
        )
    })?;

    let counts = count_verification(&doc, v);
    let percentage = if counts.total == 0 {
        0.0
    } else {
        (counts.checked + counts.skipped) as f64 / counts.total as f64 * 100.0
    };

    if format == "checklist" {
        return Ok(render_verification_checklist(&doc, v, &counts, percentage));
    }

    let mut out = json!({
        "doc_id": doc.id,
        "title": doc.title,
        "verification_status": v.status,
        "progress": {
            "checked": counts.checked,
            "skipped": counts.skipped,
            "pending": counts.pending,
            "total": counts.total,
            "stale": counts.stale,
            "percentage": percentage,
        },
    });

    if include_items {
        let items: Vec<Value> = v
            .items
            .iter()
            .map(|i| {
                let sub_items: Vec<Value> = i
                    .sub_items
                    .iter()
                    .map(|s| {
                        json!({
                            "index": s.index,
                            "description": s.description,
                            "status": s.status,
                            "reviewer": s.reviewer,
                            "verified_at": s.verified_at,
                            "notes": s.notes,
                            "category": s.category,
                            "stable_id": s.stable_id,
                            "priority": s.priority,
                            "dev_stage": s.dev_stage,
                            "impl_refs": s.impl_refs,
                            "test_refs": s.test_refs,
                            "task_ids": s.task_ids,
                            "depends_on": s.depends_on,
                        })
                    })
                    .collect();
                json!({
                    "fragment_seq": i.fragment_seq,
                    "heading": i.heading,
                    "status": i.status,
                    "stale": item_is_stale(&doc, i),
                    "impl_refs": i.impl_refs,
                    "test_refs": i.test_refs,
                    "reviewer": i.reviewer,
                    "verified_at": i.verified_at,
                    "notes": i.notes,
                    "category": i.category,
                    "sub_items": sub_items,
                    "label": i.label,
                })
            })
            .collect();
        out["items"] = json!(items);
    }

    Ok(to_json(&out))
}

/// Status icon + label used by the `format="checklist"` Markdown rendering
/// (spec §7.3): `✓ verified`, `⊘ skipped`, `○ pending`.
fn status_icon(status: &str) -> String {
    match status {
        "verified" => "✓ verified".to_string(),
        "skipped" => "⊘ skipped".to_string(),
        other => format!("○ {other}"),
    }
}

/// Renders a document's verification matrix as a Markdown checklist (v2,
/// wiki/140-verification-matrix.md §7.3): one `##` block per top-level item
/// (`§{seq} {heading}` for section-tied items, `— {label}` for freeform
/// items), with impl/test refs and a `- [x]`/`- [ ]` checkbox line per
/// sub_item.
fn render_verification_checklist(
    doc: &DocMetadata,
    v: &Verification,
    counts: &VerificationCounts,
    percentage: f64,
) -> String {
    use std::fmt::Write;

    let mut out = String::new();
    let _ = writeln!(out, "# Verification: {}", doc.title);
    let _ = writeln!(
        out,
        "Status: {} ({}/{}, {:.0}%)",
        v.status,
        counts.checked + counts.skipped,
        counts.total,
        percentage
    );
    out.push('\n');

    for item in &v.items {
        let icon = status_icon(&item.status);
        let stale_warning = if item_is_stale(doc, item) {
            " ⚠ stale"
        } else {
            ""
        };

        match item.fragment_seq {
            Some(seq) => {
                let _ = writeln!(out, "## §{seq} {} {icon}{stale_warning}", item.heading);
                if !item.impl_refs.is_empty() {
                    let refs: Vec<String> = item.impl_refs.iter().map(code_ref_display).collect();
                    let _ = writeln!(out, "- impl: {}", refs.join(", "));
                }
                if !item.test_refs.is_empty() {
                    let refs: Vec<String> = item.test_refs.iter().map(code_ref_display).collect();
                    let _ = writeln!(out, "- test: {}", refs.join(", "));
                }
            }
            None => {
                let label = item.label.as_deref().unwrap_or(&item.heading);
                let _ = writeln!(
                    out,
                    "## — {label} {icon}{stale_warning} [{}]",
                    item.category
                );
            }
        }

        for sub in &item.sub_items {
            let checkbox = if sub.status == "verified" { "x" } else { " " };
            match (&sub.reviewer, &sub.verified_at) {
                (Some(reviewer), Some(verified_at)) => {
                    let date = verified_at.split('T').next().unwrap_or(verified_at);
                    let _ = writeln!(
                        out,
                        "- [{checkbox}] {} (@{reviewer}, {date}) [{}]",
                        sub.description, sub.category
                    );
                }
                _ => {
                    let _ = writeln!(out, "- [{checkbox}] {} [{}]", sub.description, sub.category);
                }
            }
        }
        out.push('\n');
    }

    out
}

/// Renders a `CodeRef` for the checklist format: `path` optionally suffixed
/// with `:lines` and/or ` (label)`.
fn code_ref_display(r: &CodeRef) -> String {
    let mut s = r.path.clone();
    if let Some(lines) = &r.lines {
        s.push(':');
        s.push_str(lines);
    }
    if let Some(label) = &r.label {
        s.push_str(" (");
        s.push_str(label);
        s.push(')');
    }
    s
}

/// `handoff_doc_graph` — build a graph of every document in the project:
/// `nodes[]` (one per document, with optional verification progress),
/// `edges[]` (explicit parent_child/related links, plus implicit
/// shared_task/shared_scope links when `include_implicit=true`), and
/// `layers` (doc ids grouped by `doc_type`).
pub fn handle_doc_graph(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let include_implicit = arguments
        .get("include_implicit")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let include_verification = arguments
        .get("include_verification")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let docs = read_all_docs(handoff)?;

    let nodes: Vec<Value> = docs
        .iter()
        .map(|d| doc_graph_node_json(d, include_verification))
        .collect();

    let mut edges = doc_graph_explicit_edges(&docs);
    if include_implicit {
        edges.extend(doc_graph_implicit_edges(&docs));
    }

    let mut layers: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for d in &docs {
        layers
            .entry(d.doc_type.clone())
            .or_default()
            .push(d.id.clone());
    }

    Ok(to_json(&json!({
        "nodes": nodes,
        "edges": edges,
        "layers": layers,
    })))
}

/// Builds one `handoff_doc_graph` node: id/slug/title/doc_type/tags/task_ids
/// /section_count/updated_at, plus `verification_progress` when requested
/// (and the document has a verification matrix).
fn doc_graph_node_json(doc: &DocMetadata, include_verification: bool) -> Value {
    let mut node = json!({
        "id": doc.id,
        "slug": doc.slug,
        "title": doc.title,
        "doc_type": doc.doc_type,
        "tags": doc.tags,
        "task_ids": doc.task_ids,
        "section_count": doc.sections.len(),
        "updated_at": doc.updated_at,
    });
    if include_verification {
        if let Some(v) = &doc.verification {
            let total = v.items.len();
            let verified = v.items.iter().filter(|i| i.status == "verified").count();
            node["verification_progress"] = json!({ "total": total, "verified": verified });
        }
    }
    node
}

/// Explicit edges: `parent_id` (`type="parent_child"`, `direction="down"`,
/// from=parent to=child) and `related[]` (`type=<rel>`,
/// `direction="forward"`, from=this doc to=related target). Related entries
/// pointing at an id not present in `docs` are still emitted — the graph
/// consumer is expected to render dangling links, not silently drop them.
fn doc_graph_explicit_edges(docs: &[DocMetadata]) -> Vec<Value> {
    let mut edges = Vec::new();
    for d in docs {
        if let Some(parent_id) = &d.parent_id {
            edges.push(json!({
                "from": parent_id,
                "to": d.id,
                "type": "parent_child",
                "direction": "down",
            }));
        }
        for r in &d.related {
            edges.push(json!({
                "from": d.id,
                "to": r.id,
                "type": r.rel,
                "direction": "forward",
            }));
        }
    }
    edges
}

/// Implicit edges: `shared_task` (two documents sharing at least one
/// `task_ids` entry — `task_ids` on the edge lists every id shared, not just
/// the first) and `shared_scope` (two documents sharing at least one
/// `scope_paths` entry). Both are unordered/undirected pairs, emitted once
/// per pair (i<j) to avoid duplicating the same relationship in both
/// directions.
fn doc_graph_implicit_edges(docs: &[DocMetadata]) -> Vec<Value> {
    let mut edges = Vec::new();
    for i in 0..docs.len() {
        for j in (i + 1)..docs.len() {
            let a = &docs[i];
            let b = &docs[j];

            let shared_tasks: Vec<String> = a
                .task_ids
                .iter()
                .filter(|t| b.task_ids.contains(t))
                .cloned()
                .collect();
            if !shared_tasks.is_empty() {
                edges.push(json!({
                    "from": a.id,
                    "to": b.id,
                    "type": "shared_task",
                    "task_ids": shared_tasks,
                }));
            }

            let shares_scope = a.scope_paths.iter().any(|p| b.scope_paths.contains(p));
            if shares_scope {
                edges.push(json!({
                    "from": a.id,
                    "to": b.id,
                    "type": "shared_scope",
                }));
            }
        }
    }
    edges
}

/// One entry in a `handoff_doc_trace` `chain[]`/`branches[].docs[]`:
/// `{id, title, doc_type, rel}`. `rel` describes how this doc relates to the
/// previous entry in the chain ("parent", "child", or the `related[].rel`
/// value for a related-doc detour); `None` for the trace's starting doc.
fn doc_trace_item_json(doc: &DocMetadata, rel: Option<&str>) -> Value {
    json!({
        "id": doc.id,
        "title": doc.title,
        "doc_type": doc.doc_type,
        "rel": rel,
    })
}

/// Walks the child->parent chain starting at `doc` (exclusive — `doc` itself
/// is not included), ordered from the immediate parent up to the root.
/// `visited` prevents infinite loops on a cyclic `parent_id` graph; a doc
/// already visited (including `doc` itself) stops the walk rather than
/// erroring.
fn doc_trace_walk_up(
    handoff: &Path,
    doc: &DocMetadata,
    visited: &mut std::collections::HashSet<String>,
) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    let mut current = doc.clone();
    while let Some(parent_id) = current.parent_id.clone() {
        if visited.contains(&parent_id) {
            break;
        }
        let Some(parent) = find_doc_by_id(handoff, &parent_id)? else {
            break;
        };
        visited.insert(parent.id.clone());
        out.push(doc_trace_item_json(&parent, Some("parent")));
        current = parent;
    }
    out.reverse();
    Ok(out)
}

/// Recursively walks parent->children (DFS) starting at `doc` (exclusive).
/// Returns the primary descendant chain (first child at each level) plus any
/// `branches` recorded for multi-child forks. `visited` prevents infinite
/// loops on a cyclic `children` graph.
fn doc_trace_walk_down(
    handoff: &Path,
    doc: &DocMetadata,
    visited: &mut std::collections::HashSet<String>,
    branches: &mut Vec<Value>,
) -> Result<Vec<Value>> {
    let mut children = Vec::new();
    for child_id in &doc.children {
        if visited.contains(child_id) {
            continue;
        }
        if let Some(child) = find_doc_by_id(handoff, child_id)? {
            children.push(child);
        }
    }

    if children.is_empty() {
        return Ok(Vec::new());
    }

    // Fork detection: more than one live (non-visited, resolvable) child at
    // this level. Every child's own sub-chain is recorded under `branches`;
    // the first child's sub-chain also becomes the primary continuation of
    // the returned chain, so a single-child level still reads as a plain
    // linear chain.
    let is_fork = children.len() > 1;
    let mut primary_chain = Vec::new();

    for (idx, child) in children.iter().enumerate() {
        if visited.contains(&child.id) {
            continue;
        }
        visited.insert(child.id.clone());
        let mut sub_chain = vec![doc_trace_item_json(child, Some("child"))];
        sub_chain.extend(doc_trace_walk_down(handoff, child, visited, branches)?);

        if is_fork {
            branches.push(json!({
                "fork_from": doc.id,
                "docs": sub_chain,
            }));
        }
        if idx == 0 {
            primary_chain = sub_chain;
        }
    }

    Ok(primary_chain)
}

/// Appends `related` (implements/references/etc.) detours for every document
/// already present in `chain` (by id), skipping any related id already
/// visited. Related docs are appended once, immediately, as a flat list — a
/// "detour" from the main chain rather than a further recursive expansion.
fn doc_trace_related_detours(
    handoff: &Path,
    chain_doc_ids: &[String],
    visited: &mut std::collections::HashSet<String>,
) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for doc_id in chain_doc_ids {
        let Some(doc) = find_doc_by_id(handoff, doc_id)? else {
            continue;
        };
        for r in &doc.related {
            if visited.contains(&r.id) {
                continue;
            }
            let Some(target) = find_doc_by_id(handoff, &r.id)? else {
                continue;
            };
            visited.insert(target.id.clone());
            out.push(doc_trace_item_json(&target, Some(&r.rel)));
        }
    }
    Ok(out)
}

/// `handoff_doc_trace` — trace a document's family-tree lineage: `up` (walk
/// child->parent), `down` (walk parent->children, DFS), or `both` (merge the
/// up chain + the target + the down chain). `related` docs encountered along
/// the primary chain are appended as detour entries. Multi-child forks in the
/// `down` direction are additionally reported in `branches[]`.
pub fn handle_doc_trace(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let doc_id = arguments
        .get("doc_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'doc_id' is required"))?;
    let direction = arguments
        .get("direction")
        .and_then(|v| v.as_str())
        .unwrap_or("both");

    let doc = resolve_doc(handoff, doc_id)?
        .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;

    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    visited.insert(doc.id.clone());

    let mut branches: Vec<Value> = Vec::new();
    let mut chain: Vec<Value> = Vec::new();

    if direction == "up" || direction == "both" {
        chain.extend(doc_trace_walk_up(handoff, &doc, &mut visited)?);
    }
    chain.push(doc_trace_item_json(&doc, None));
    if direction == "down" || direction == "both" {
        chain.extend(doc_trace_walk_down(
            handoff,
            &doc,
            &mut visited,
            &mut branches,
        )?);
    }

    let chain_doc_ids: Vec<String> = chain
        .iter()
        .filter_map(|v| v["id"].as_str().map(str::to_string))
        .collect();
    chain.extend(doc_trace_related_detours(
        handoff,
        &chain_doc_ids,
        &mut visited,
    )?);

    Ok(to_json(&json!({
        "chain": chain,
        "branches": branches,
    })))
}

fn doc_metadata_json(doc: &DocMetadata) -> Value {
    json!({
        "id": doc.id,
        "slug": doc.slug,
        "title": doc.title,
        "doc_type": doc.doc_type,
        "tags": doc.tags,
        "scope_paths": doc.scope_paths,
        "parent_id": doc.parent_id,
        "children": doc.children,
        "related": doc.related,
        "auto_inject": doc.auto_inject,
        "task_ids": doc.task_ids,
        "has_bom": doc.has_bom,
        "line_ending": doc.line_ending,
        "sections": doc.sections,
        "section_count": doc.sections.len(),
        "created_at": doc.created_at,
        "updated_at": doc.updated_at,
        "content_hash": doc.content_hash,
    })
}

/// Read a `&[String]` from a JSON string-array value (missing/non-array →
/// empty).
fn string_array_value(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn to_json(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

#[cfg(test)]
mod graph_tests {
    use super::*;

    fn doc(id: &str, slug: &str, doc_type: &str) -> DocMetadata {
        DocMetadata::new(
            id.to_string(),
            slug.to_string(),
            format!("Title {id}"),
            doc_type.to_string(),
            "2026-07-12T00:00:00Z".to_string(),
        )
    }

    #[test]
    fn explicit_edges_include_parent_child_and_related() {
        let mut parent = doc("doc-1", "parent", "spec");
        let mut child = doc("doc-2", "child", "design");
        child.parent_id = Some("doc-1".to_string());
        parent.children = vec!["doc-2".to_string()];
        child.related.push(DocRelation {
            id: "doc-3".to_string(),
            rel: "implements".to_string(),
        });
        let other = doc("doc-3", "other", "note");

        let docs = vec![parent, child, other];
        let edges = doc_graph_explicit_edges(&docs);

        assert!(edges.iter().any(|e| e["type"] == "parent_child"
            && e["from"] == "doc-1"
            && e["to"] == "doc-2"
            && e["direction"] == "down"));
        assert!(edges.iter().any(|e| e["type"] == "implements"
            && e["from"] == "doc-2"
            && e["to"] == "doc-3"
            && e["direction"] == "forward"));
    }

    #[test]
    fn implicit_edges_detect_shared_task_ids() {
        let mut a = doc("doc-1", "a", "spec");
        let mut b = doc("doc-2", "b", "spec");
        a.task_ids = vec!["t-1".to_string(), "t-2".to_string()];
        b.task_ids = vec!["t-2".to_string(), "t-3".to_string()];
        let docs = vec![a, b];

        let edges = doc_graph_implicit_edges(&docs);
        let shared_task_edge = edges
            .iter()
            .find(|e| e["type"] == "shared_task")
            .expect("shared_task edge must be generated");
        assert_eq!(shared_task_edge["from"], "doc-1");
        assert_eq!(shared_task_edge["to"], "doc-2");
        assert_eq!(shared_task_edge["task_ids"], json!(["t-2"]));
    }

    #[test]
    fn implicit_edges_detect_shared_scope_paths() {
        let mut a = doc("doc-1", "a", "spec");
        let mut b = doc("doc-2", "b", "spec");
        a.scope_paths = vec!["src/mcp/".to_string()];
        b.scope_paths = vec!["src/mcp/".to_string(), "src/storage/".to_string()];
        let docs = vec![a, b];

        let edges = doc_graph_implicit_edges(&docs);
        assert!(edges
            .iter()
            .any(|e| e["type"] == "shared_scope" && e["from"] == "doc-1" && e["to"] == "doc-2"));
    }

    #[test]
    fn implicit_edges_absent_when_nothing_shared() {
        let a = doc("doc-1", "a", "spec");
        let b = doc("doc-2", "b", "spec");
        let docs = vec![a, b];

        let edges = doc_graph_implicit_edges(&docs);
        assert!(edges.is_empty());
    }

    #[test]
    fn graph_node_json_includes_verification_progress_when_requested() {
        let mut d = doc("doc-1", "a", "spec");
        d.verification = Some(Verification {
            status: "in_review".to_string(),
            created_at: "2026-07-12T00:00:00Z".to_string(),
            updated_at: "2026-07-12T00:00:00Z".to_string(),
            items: vec![
                VerificationItem {
                    fragment_seq: Some(0),
                    heading: String::new(),
                    status: "verified".to_string(),
                    impl_refs: Vec::new(),
                    test_refs: Vec::new(),
                    reviewer: None,
                    verified_at: None,
                    notes: String::new(),
                    content_hash_at_verify: None,
                    category: "section".to_string(),
                    sub_items: Vec::new(),
                    label: None,
                },
                VerificationItem {
                    fragment_seq: Some(1),
                    heading: "H".to_string(),
                    status: "pending".to_string(),
                    impl_refs: Vec::new(),
                    test_refs: Vec::new(),
                    reviewer: None,
                    verified_at: None,
                    notes: String::new(),
                    content_hash_at_verify: None,
                    category: "section".to_string(),
                    sub_items: Vec::new(),
                    label: None,
                },
            ],
        });

        let with_verification = doc_graph_node_json(&d, true);
        assert_eq!(
            with_verification["verification_progress"],
            json!({ "total": 2, "verified": 1 })
        );

        let without_verification = doc_graph_node_json(&d, false);
        assert!(without_verification.get("verification_progress").is_none());
    }

    #[test]
    fn graph_node_json_omits_verification_progress_when_no_matrix() {
        let d = doc("doc-1", "a", "spec");
        let node = doc_graph_node_json(&d, true);
        assert!(node.get("verification_progress").is_none());
    }
}

#[cfg(test)]
mod sub_item_lookup_tests {
    use super::*;

    fn item_with_subs() -> VerificationItem {
        VerificationItem {
            fragment_seq: Some(1),
            heading: "1. 課題".to_string(),
            status: "pending".to_string(),
            impl_refs: Vec::new(),
            test_refs: Vec::new(),
            reviewer: None,
            verified_at: None,
            notes: String::new(),
            content_hash_at_verify: None,
            category: "section".to_string(),
            sub_items: vec![
                SubItem {
                    index: 0,
                    description: "req A".to_string(),
                    stable_id: Some("C01-1.1".to_string()),
                    ..Default::default()
                },
                SubItem {
                    index: 1,
                    description: "req B".to_string(),
                    stable_id: Some("C01-1.2".to_string()),
                    ..Default::default()
                },
            ],
            label: None,
        }
    }

    #[test]
    fn finds_by_stable_id_when_given() {
        let mut item = item_with_subs();
        let (sub, warning) =
            find_sub_item_mut_by_id(&mut item, Some("C01-1.2"), None, 1, "doc-1").unwrap();
        assert_eq!(sub.description, "req B");
        assert!(warning.is_none());
    }

    #[test]
    fn falls_back_to_index_when_no_id_given() {
        let mut item = item_with_subs();
        let (sub, warning) = find_sub_item_mut_by_id(&mut item, None, Some(0), 1, "doc-1").unwrap();
        assert_eq!(sub.description, "req A");
        assert!(warning.is_none());
    }

    #[test]
    fn prefers_stable_id_and_warns_on_index_mismatch() {
        let mut item = item_with_subs();
        // sub_item_id points at "req B" (index 1) but sub_item_index says 0
        // ("req A") — sub_item_id must win, and a warning must be returned.
        let (sub, warning) =
            find_sub_item_mut_by_id(&mut item, Some("C01-1.2"), Some(0), 1, "doc-1").unwrap();
        assert_eq!(sub.description, "req B");
        assert!(warning.is_some(), "expected a mismatch warning");
    }

    #[test]
    fn no_warning_when_id_and_index_agree() {
        let mut item = item_with_subs();
        let (sub, warning) =
            find_sub_item_mut_by_id(&mut item, Some("C01-1.1"), Some(0), 1, "doc-1").unwrap();
        assert_eq!(sub.description, "req A");
        assert!(warning.is_none());
    }

    #[test]
    fn errors_when_stable_id_not_found() {
        let mut item = item_with_subs();
        let result = find_sub_item_mut_by_id(&mut item, Some("C01-9.9"), None, 1, "doc-1");
        assert!(result.is_err());
    }

    #[test]
    fn errors_when_neither_id_nor_index_given() {
        let mut item = item_with_subs();
        let result = find_sub_item_mut_by_id(&mut item, None, None, 1, "doc-1");
        assert!(result.is_err());
    }
}

/// Requirements-traceability P0 (`.handoff/docs/_doc.req-traceability-mcp-plan.md`
/// §2.3, t300.3): `derive_stable_id` unit tests — category-prefix extraction,
/// heading-number extraction, description slug fallback, and collision
/// suffixing.
#[cfg(test)]
mod stable_id_derivation_tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn extracts_category_prefix_from_req_c_slug() {
        let existing = HashSet::new();
        let (id, warning) = derive_stable_id(
            "req-c01-board-setup",
            "## 2.1 基板外形",
            "外形形状定義",
            &existing,
        );
        assert!(id.starts_with("C01-"), "expected C01- prefix, got {id:?}");
        assert!(warning.is_none());
    }

    #[test]
    fn falls_back_to_uppercased_slug_when_no_category_pattern() {
        let existing = HashSet::new();
        let (id, _) = derive_stable_id("misc-notes", "## 2.1 基板外形", "外形形状定義", &existing);
        assert!(
            id.starts_with("MISC-NOTES-"),
            "expected uppercased slug prefix, got {id:?}"
        );
    }

    #[test]
    fn extracts_heading_number_from_heading_text() {
        let existing = HashSet::new();
        let (id, _) = derive_stable_id(
            "req-c01-board-setup",
            "## 2.1 基板外形",
            "何らかの説明",
            &existing,
        );
        assert!(
            id.contains("2.1"),
            "expected heading number 2.1 in id, got {id:?}"
        );
    }

    #[test]
    fn extracts_description_number_when_present() {
        let existing = HashSet::new();
        let (id, _) = derive_stable_id(
            "req-c01-board-setup",
            "## 2.1 基板外形",
            "2.1.1 外形形状定義",
            &existing,
        );
        assert_eq!(id, "C01-2.1.1");
    }

    #[test]
    fn slugifies_description_when_no_number_extractable() {
        // Pure-Japanese description has no ASCII-alphanumeric characters to
        // slugify, so it falls back further to the heading number alone —
        // still deterministic and collision-checked.
        let existing = HashSet::new();
        let (id, _) = derive_stable_id(
            "req-c01-board-setup",
            "## 2.1 基板外形",
            "矩形外形",
            &existing,
        );
        assert_eq!(id, "C01-2.1");
    }

    #[test]
    fn slugifies_ascii_description_when_no_number_extractable() {
        let existing = HashSet::new();
        let (id, _) = derive_stable_id(
            "req-c01-board-setup",
            "## 2.1 基板外形",
            "Rectangular Outline!",
            &existing,
        );
        assert_eq!(id, "C01-2.1-rectangular-outline");
        assert!(!id.contains(' '));
        assert!(!id.contains("--"));
    }

    #[test]
    fn appends_suffix_and_warns_on_collision() {
        let mut existing = HashSet::new();
        existing.insert("C01-2.1.1".to_string());
        let (id, warning) = derive_stable_id(
            "req-c01-board-setup",
            "## 2.1 基板外形",
            "2.1.1 外形形状定義",
            &existing,
        );
        assert_eq!(id, "C01-2.1.1-2");
        assert!(warning.is_some(), "expected a collision warning");
    }

    #[test]
    fn appends_incrementing_suffix_on_repeated_collision() {
        let mut existing = HashSet::new();
        existing.insert("C01-2.1.1".to_string());
        existing.insert("C01-2.1.1-2".to_string());
        let (id, warning) = derive_stable_id(
            "req-c01-board-setup",
            "## 2.1 基板外形",
            "2.1.1 外形形状定義",
            &existing,
        );
        assert_eq!(id, "C01-2.1.1-3");
        assert!(warning.is_some());
    }

    #[test]
    fn no_warning_when_no_collision() {
        let existing = HashSet::new();
        let (_, warning) = derive_stable_id(
            "req-c01-board-setup",
            "## 2.1 基板外形",
            "2.1.1 外形形状定義",
            &existing,
        );
        assert!(warning.is_none());
    }
}

/// Requirements-traceability P0 §2.3, t300.3: fuzzy description matching
/// used by `sync`/`add_item` to re-link a new SubItem description to an
/// existing SubItem's `stable_id` instead of minting a fresh one.
#[cfg(test)]
mod fuzzy_match_tests {
    use super::*;

    #[test]
    fn exact_description_matches() {
        assert!(descriptions_fuzzy_match("外形形状定義", "外形形状定義"));
    }

    #[test]
    fn near_identical_descriptions_match_after_normalization() {
        // Trailing punctuation / whitespace differences should not defeat
        // the match — normalization strips them before comparing.
        assert!(descriptions_fuzzy_match(
            "形状=八面体であること",
            "形状=八面体であること。"
        ));
    }

    #[test]
    fn substring_containment_matches() {
        assert!(descriptions_fuzzy_match(
            "外形形状定義",
            "2.1.1 外形形状定義（矩形）"
        ));
    }

    #[test]
    fn unrelated_descriptions_do_not_match() {
        assert!(!descriptions_fuzzy_match(
            "外形形状定義",
            "電源電圧の許容範囲"
        ));
    }
}

#[cfg(test)]
mod requirements_summary_tests {
    use super::*;

    fn doc_with_items(id: &str, slug: &str, items: Vec<VerificationItem>) -> DocMetadata {
        let mut d = DocMetadata::new(
            id.to_string(),
            slug.to_string(),
            format!("Title {id}"),
            "spec".to_string(),
            "2026-09-20T00:00:00Z".to_string(),
        );
        d.verification = Some(Verification {
            status: "in_review".to_string(),
            created_at: "2026-09-20T00:00:00Z".to_string(),
            updated_at: "2026-09-20T00:00:00Z".to_string(),
            items,
        });
        d
    }

    fn section_item(sub_items: Vec<SubItem>) -> VerificationItem {
        VerificationItem {
            fragment_seq: Some(1),
            heading: "1. 要件".to_string(),
            status: "pending".to_string(),
            impl_refs: Vec::new(),
            test_refs: Vec::new(),
            reviewer: None,
            verified_at: None,
            notes: String::new(),
            content_hash_at_verify: None,
            category: "section".to_string(),
            sub_items,
            label: None,
        }
    }

    #[test]
    fn empty_docs_yield_zero_total() {
        let summary = aggregate_requirements(&[]);
        assert_eq!(summary.total, 0);
        assert!(summary.by_status.is_empty());
        assert!(summary.by_priority.is_empty());
        assert!(summary.by_category.is_empty());
        assert_eq!(summary.coverage.impl_pct, 0.0);
    }

    #[test]
    fn doc_with_no_verification_matrix_contributes_nothing() {
        let d = DocMetadata::new(
            "doc-1".to_string(),
            "no-verify".to_string(),
            "Title".to_string(),
            "spec".to_string(),
            "2026-09-20T00:00:00Z".to_string(),
        );
        let summary = aggregate_requirements(&[d]);
        assert_eq!(summary.total, 0);
    }

    #[test]
    fn single_doc_aggregates_status_priority_category_and_coverage() {
        let subs = vec![
            SubItem {
                index: 0,
                description: "req A".to_string(),
                stable_id: Some("C01-1.1".to_string()),
                priority: Some("P0".to_string()),
                dev_stage: Some("implemented".to_string()),
                impl_refs: vec![CodeRef {
                    path: "src/a.rs".to_string(),
                    lines: None,
                    label: None,
                }],
                ..Default::default()
            },
            SubItem {
                index: 1,
                description: "req B".to_string(),
                stable_id: Some("C01-1.2".to_string()),
                priority: None,
                dev_stage: None,
                ..Default::default()
            },
            SubItem {
                index: 2,
                description: "req C".to_string(),
                stable_id: Some("C07-2.1".to_string()),
                priority: Some("P0".to_string()),
                dev_stage: Some("verified".to_string()),
                impl_refs: vec![CodeRef {
                    path: "src/c.rs".to_string(),
                    lines: None,
                    label: None,
                }],
                test_refs: vec![CodeRef {
                    path: "tests/c.rs".to_string(),
                    lines: None,
                    label: None,
                }],
                ..Default::default()
            },
        ];
        let doc = doc_with_items("doc-1", "req-c01", vec![section_item(subs)]);

        let summary = aggregate_requirements(&[doc]);

        assert_eq!(summary.total, 3);

        // by_status: dev_stage=None -> "not_started" fallback.
        assert_eq!(summary.by_status.get("implemented"), Some(&1));
        assert_eq!(summary.by_status.get("not_started"), Some(&1));
        assert_eq!(summary.by_status.get("verified"), Some(&1));

        // by_priority: priority=None -> "unset" fallback.
        let p0 = summary.by_priority.get("P0").expect("P0 bucket");
        assert_eq!(p0.total, 2);
        assert_eq!(p0.implemented, 2);
        assert_eq!(p0.tested, 1);
        assert_eq!(p0.verified, 1);
        let unset = summary.by_priority.get("unset").expect("unset bucket");
        assert_eq!(unset.total, 1);

        // by_category: extracted from stable_id prefix.
        let c01 = summary.by_category.get("C01").expect("C01 bucket");
        assert_eq!(c01.total, 2);
        assert_eq!(c01.implemented, 1);
        assert_eq!(c01.coverage_pct, 50.0);
        let c07 = summary.by_category.get("C07").expect("C07 bucket");
        assert_eq!(c07.total, 1);
        assert_eq!(c07.implemented, 1);
        assert_eq!(c07.coverage_pct, 100.0);

        // coverage: 2/3 impl, 1/3 test, 1/3 verified.
        assert!((summary.coverage.impl_pct - (2.0 / 3.0 * 100.0)).abs() < 1e-9);
        assert!((summary.coverage.test_pct - (1.0 / 3.0 * 100.0)).abs() < 1e-9);
        assert!((summary.coverage.verified_pct - (1.0 / 3.0 * 100.0)).abs() < 1e-9);
    }

    #[test]
    fn multiple_docs_aggregate_across_documents() {
        let doc_a = doc_with_items(
            "doc-a",
            "req-c01",
            vec![section_item(vec![SubItem {
                index: 0,
                description: "req A".to_string(),
                stable_id: Some("C01-1.1".to_string()),
                priority: Some("P1".to_string()),
                dev_stage: Some("tested".to_string()),
                ..Default::default()
            }])],
        );
        let doc_b = doc_with_items(
            "doc-b",
            "req-c07",
            vec![section_item(vec![SubItem {
                index: 0,
                description: "req B".to_string(),
                stable_id: Some("C07-3.1".to_string()),
                priority: Some("P1".to_string()),
                dev_stage: Some("tested".to_string()),
                ..Default::default()
            }])],
        );

        let summary = aggregate_requirements(&[doc_a, doc_b]);

        assert_eq!(summary.total, 2);
        assert_eq!(summary.by_status.get("tested"), Some(&2));
        let p1 = summary.by_priority.get("P1").expect("P1 bucket");
        assert_eq!(p1.total, 2);
        assert_eq!(summary.by_category.len(), 2);
        assert_eq!(summary.by_category.get("C01").unwrap().total, 1);
        assert_eq!(summary.by_category.get("C07").unwrap().total, 1);
    }

    #[test]
    fn write_requirements_summary_skips_file_when_no_requirements() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        write_requirements_summary(&handoff, &[]).unwrap();

        let path = docs_dir(&handoff).join("_requirements_summary.json");
        assert!(
            !path.exists(),
            "no requirements => no file should be written"
        );
    }

    #[test]
    fn write_requirements_summary_writes_file_when_requirements_exist() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let doc = doc_with_items(
            "doc-1",
            "req-c01",
            vec![section_item(vec![SubItem {
                index: 0,
                description: "req A".to_string(),
                stable_id: Some("C01-1.1".to_string()),
                ..Default::default()
            }])],
        );

        write_requirements_summary(&handoff, &[doc]).unwrap();

        let path = docs_dir(&handoff).join("_requirements_summary.json");
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["total"], 1);
        assert_eq!(parsed["by_status"]["not_started"], 1);
    }
}

#[cfg(test)]
mod propagate_dev_stage_tests {
    use super::*;
    use crate::storage::docs::{write_doc, DocMetadata, SubItem, Verification, VerificationItem};
    use crate::storage::tasks::{write_task, TaskData, TaskLink};

    fn setup_handoff(tmp: &std::path::Path) -> std::path::PathBuf {
        let handoff = tmp.join(".handoff");
        std::fs::create_dir_all(handoff.join("tasks")).unwrap();
        std::fs::create_dir_all(handoff.join("docs")).unwrap();
        handoff
    }

    fn make_task(handoff: &std::path::Path, id: &str, status: &str, req_links: &[&str]) {
        let task_dir = handoff.join("tasks").join(id);
        std::fs::create_dir_all(&task_dir).unwrap();
        let task_links: Vec<TaskLink> = req_links
            .iter()
            .map(|stable_id| TaskLink {
                target: "doc-1".to_string(),
                link_type: "requirement".to_string(),
                label: Some(stable_id.to_string()),
            })
            .collect();
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
            task_links,
            done_criteria: Vec::new(),
            schedule: None,
            dependencies: Vec::new(),
            order: None,
            assignee: None,
            lock: None,
            scope_paths: Vec::new(),
            extra: std::collections::HashMap::new(),
        };
        write_task(&task_dir, status, &data).unwrap();
    }

    fn make_doc_with_sub_items(handoff: &std::path::Path, sub_items: Vec<SubItem>) {
        let mut doc = DocMetadata::new(
            "doc-1".to_string(),
            "req-test".to_string(),
            "Test Doc".to_string(),
            "spec".to_string(),
            chrono::Utc::now().to_rfc3339(),
        );
        doc.verification = Some(Verification {
            status: "pending".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            items: vec![VerificationItem {
                fragment_seq: Some(1),
                heading: "Section 1".to_string(),
                status: "pending".to_string(),
                impl_refs: Vec::new(),
                test_refs: Vec::new(),
                reviewer: None,
                verified_at: None,
                notes: String::new(),
                content_hash_at_verify: None,
                category: "section".to_string(),
                sub_items,
                label: None,
            }],
        });
        write_doc(handoff, &doc).unwrap();
    }

    fn read_sub_item_dev_stage(handoff: &std::path::Path, sub_index: usize) -> Option<String> {
        let doc = read_doc(handoff, "req-test").unwrap().unwrap();
        let v = doc.verification.as_ref().unwrap();
        v.items[0].sub_items[sub_index].dev_stage.clone()
    }

    fn task_links_for(stable_ids: &[&str]) -> Vec<TaskLink> {
        stable_ids
            .iter()
            .map(|sid| TaskLink {
                target: "doc-1".to_string(),
                link_type: "requirement".to_string(),
                label: Some(sid.to_string()),
            })
            .collect()
    }

    #[test]
    fn single_task_done_sets_implemented() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());

        make_task(&handoff, "t1", "done", &["REQ-1"]);
        make_doc_with_sub_items(
            &handoff,
            vec![SubItem {
                index: 0,
                description: "req 1".to_string(),
                stable_id: Some("REQ-1".to_string()),
                task_ids: vec!["t1".to_string()],
                ..Default::default()
            }],
        );

        propagate_dev_stage_for_task(&handoff, &task_links_for(&["REQ-1"])).unwrap();

        assert_eq!(
            read_sub_item_dev_stage(&handoff, 0),
            Some("implemented".to_string())
        );
    }

    #[test]
    fn single_task_in_progress_sets_in_progress() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());

        make_task(&handoff, "t1", "in_progress", &["REQ-1"]);
        make_doc_with_sub_items(
            &handoff,
            vec![SubItem {
                index: 0,
                description: "req 1".to_string(),
                stable_id: Some("REQ-1".to_string()),
                task_ids: vec!["t1".to_string()],
                ..Default::default()
            }],
        );

        propagate_dev_stage_for_task(&handoff, &task_links_for(&["REQ-1"])).unwrap();

        assert_eq!(
            read_sub_item_dev_stage(&handoff, 0),
            Some("in_progress".to_string())
        );
    }

    #[test]
    fn multi_task_min_strategy_one_todo() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());

        make_task(&handoff, "t1", "done", &["REQ-1"]);
        make_task(&handoff, "t2", "todo", &["REQ-1"]);
        make_doc_with_sub_items(
            &handoff,
            vec![SubItem {
                index: 0,
                description: "req 1".to_string(),
                stable_id: Some("REQ-1".to_string()),
                task_ids: vec!["t1".to_string(), "t2".to_string()],
                ..Default::default()
            }],
        );

        propagate_dev_stage_for_task(&handoff, &task_links_for(&["REQ-1"])).unwrap();

        assert_eq!(
            read_sub_item_dev_stage(&handoff, 0),
            Some("not_started".to_string()),
            "min of done + todo = not_started"
        );
    }

    #[test]
    fn multi_task_all_done_sets_implemented() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());

        make_task(&handoff, "t1", "done", &["REQ-1"]);
        make_task(&handoff, "t2", "done", &["REQ-1"]);
        make_doc_with_sub_items(
            &handoff,
            vec![SubItem {
                index: 0,
                description: "req 1".to_string(),
                stable_id: Some("REQ-1".to_string()),
                task_ids: vec!["t1".to_string(), "t2".to_string()],
                ..Default::default()
            }],
        );

        propagate_dev_stage_for_task(&handoff, &task_links_for(&["REQ-1"])).unwrap();

        assert_eq!(
            read_sub_item_dev_stage(&handoff, 0),
            Some("implemented".to_string()),
            "all done = implemented"
        );
    }

    #[test]
    fn tested_stage_is_protected() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());

        make_task(&handoff, "t1", "done", &["REQ-1"]);
        make_doc_with_sub_items(
            &handoff,
            vec![SubItem {
                index: 0,
                description: "req 1".to_string(),
                stable_id: Some("REQ-1".to_string()),
                dev_stage: Some("tested".to_string()),
                task_ids: vec!["t1".to_string()],
                ..Default::default()
            }],
        );

        propagate_dev_stage_for_task(&handoff, &task_links_for(&["REQ-1"])).unwrap();

        assert_eq!(
            read_sub_item_dev_stage(&handoff, 0),
            Some("tested".to_string()),
            "tested must not be overwritten"
        );
    }

    #[test]
    fn verified_stage_is_protected() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());

        make_task(&handoff, "t1", "in_progress", &["REQ-1"]);
        make_doc_with_sub_items(
            &handoff,
            vec![SubItem {
                index: 0,
                description: "req 1".to_string(),
                stable_id: Some("REQ-1".to_string()),
                dev_stage: Some("verified".to_string()),
                task_ids: vec!["t1".to_string()],
                ..Default::default()
            }],
        );

        propagate_dev_stage_for_task(&handoff, &task_links_for(&["REQ-1"])).unwrap();

        assert_eq!(
            read_sub_item_dev_stage(&handoff, 0),
            Some("verified".to_string()),
            "verified must not be overwritten"
        );
    }

    #[test]
    fn skipped_task_excluded_from_computation() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());

        make_task(&handoff, "t1", "done", &["REQ-1"]);
        make_task(&handoff, "t2", "skipped", &["REQ-1"]);
        make_doc_with_sub_items(
            &handoff,
            vec![SubItem {
                index: 0,
                description: "req 1".to_string(),
                stable_id: Some("REQ-1".to_string()),
                task_ids: vec!["t1".to_string(), "t2".to_string()],
                ..Default::default()
            }],
        );

        propagate_dev_stage_for_task(&handoff, &task_links_for(&["REQ-1"])).unwrap();

        assert_eq!(
            read_sub_item_dev_stage(&handoff, 0),
            Some("implemented".to_string()),
            "skipped excluded, only done remains = implemented"
        );
    }

    #[test]
    fn no_requirement_links_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());

        let empty_links: Vec<TaskLink> = vec![];
        propagate_dev_stage_for_task(&handoff, &empty_links).unwrap();
    }

    #[test]
    fn multi_task_mixed_in_progress_and_done() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());

        make_task(&handoff, "t1", "done", &["REQ-1"]);
        make_task(&handoff, "t2", "in_progress", &["REQ-1"]);
        make_doc_with_sub_items(
            &handoff,
            vec![SubItem {
                index: 0,
                description: "req 1".to_string(),
                stable_id: Some("REQ-1".to_string()),
                task_ids: vec!["t1".to_string(), "t2".to_string()],
                ..Default::default()
            }],
        );

        propagate_dev_stage_for_task(&handoff, &task_links_for(&["REQ-1"])).unwrap();

        assert_eq!(
            read_sub_item_dev_stage(&handoff, 0),
            Some("in_progress".to_string()),
            "min of done + in_progress = in_progress"
        );
    }

    #[test]
    fn requirements_summary_updated_after_propagation() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());

        make_task(&handoff, "t1", "done", &["REQ-1"]);
        make_doc_with_sub_items(
            &handoff,
            vec![SubItem {
                index: 0,
                description: "req 1".to_string(),
                stable_id: Some("REQ-1".to_string()),
                task_ids: vec!["t1".to_string()],
                ..Default::default()
            }],
        );

        propagate_dev_stage_for_task(&handoff, &task_links_for(&["REQ-1"])).unwrap();

        let path = docs_dir(&handoff).join("_requirements_summary.json");
        assert!(path.exists(), "summary file should be regenerated");
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["by_status"]["implemented"], 1);
    }

    #[test]
    fn deleted_task_in_task_ids_does_not_abort_propagation() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());

        make_task(&handoff, "t1", "done", &["REQ-1"]);
        // t2 is referenced in task_ids but does not exist on disk
        make_doc_with_sub_items(
            &handoff,
            vec![SubItem {
                index: 0,
                description: "req 1".to_string(),
                stable_id: Some("REQ-1".to_string()),
                task_ids: vec!["t1".to_string(), "t-deleted".to_string()],
                ..Default::default()
            }],
        );

        propagate_dev_stage_for_task(&handoff, &task_links_for(&["REQ-1"])).unwrap();

        assert_eq!(
            read_sub_item_dev_stage(&handoff, 0),
            Some("implemented".to_string()),
            "deleted task skipped, remaining done task = implemented"
        );
    }
}
