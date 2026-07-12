//! MCP handlers for document management (save / get / list) — P1-6a (t96.1),
//! v5 rearchitecture (2-file slug-based storage,
//! wiki/130-document-management.md §3.1).
//!
//! Builds on the storage layer in `crate::storage::docs` (split into
//! in-memory sections + slug-named `.json`/`.md` pair I/O) and the
//! task<->doc bidirectional link sync in
//! `crate::storage::tasks::sync_doc_task_links`. See
//! `wiki/130-document-management.md` §5.1-§5.3 for the spec.

use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::resolve_project_dir;
use crate::context::injection::{rank_by_bm25_and_scope, RankConfig};
use crate::storage::docs::reassemble::extract_section;
use crate::storage::docs::split::{compute_sections, split};
use crate::storage::docs::{
    delete_doc, delete_doc_body, ensure_docs_dir, find_doc_by_id, read_all_docs, read_doc,
    read_doc_body, validate_slug, write_doc, write_doc_body, CodeRef, DocMetadata, DocRelation,
    Verification, VerificationItem,
};
use crate::storage::ensure_handoff_exists;
use crate::storage::tasks::sync_doc_task_links;

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
pub fn handle_doc_save(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;
    ensure_docs_dir(&handoff)?;

    let body = arguments
        .get("body")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'body' is required"))?;

    let doc_id = arguments.get("doc_id").and_then(|v| v.as_str());
    let existing = match doc_id {
        Some(id) => Some(
            find_doc_by_id(&handoff, id)?
                .ok_or_else(|| anyhow::anyhow!("Document not found: {id}"))?,
        ),
        None => None,
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
            if read_doc(&handoff, &slug)?.is_some() {
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
    doc.source.frontmatter = split_doc.frontmatter.map(str::to_string);
    doc.source.frontmatter_trailing_eol = split_doc.frontmatter_trailing_eol;
    doc.updated_at = now.clone();

    // v5: the full body (after BOM/frontmatter stripping) is written verbatim
    // to `_doc.<slug>.md`; sections are an in-memory byte-offset index into
    // it, computed fresh on every save (no stale-fragment cleanup needed —
    // there is nothing left on disk to clean up per section).
    let body_after_strip: String = split_doc.fragments.iter().map(|f| f.body).collect();
    write_doc_body(&handoff, &slug, &body_after_strip)?;
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

    write_doc(&handoff, &doc)?;

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
            if let Some(mut old_parent) = find_doc_by_id(&handoff, old_parent_id)? {
                let before = old_parent.children.len();
                old_parent.children.retain(|c| c != &id);
                if old_parent.children.len() != before {
                    write_doc(&handoff, &old_parent)?;
                }
            }
        }
        if let Some(new_parent_id) = &doc.parent_id {
            match find_doc_by_id(&handoff, new_parent_id)? {
                Some(mut new_parent) => {
                    if !new_parent.children.iter().any(|c| c == &id) {
                        new_parent.children.push(id.clone());
                        write_doc(&handoff, &new_parent)?;
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

/// Reads a document's full original body: `_doc.<slug>.md` (the
/// post-BOM/frontmatter-stripped body) with the BOM and YAML frontmatter
/// (if any) restored in front of it, exactly as originally authored.
/// Returns `Ok(None)` when the `.md` file is missing (metadata exists but
/// body was deleted out-of-band).
fn read_full_body(handoff: &Path, doc: &DocMetadata) -> Result<Option<String>> {
    let Some(stripped_body) = read_doc_body(handoff, &doc.slug)? else {
        return Ok(None);
    };
    let mut body = stripped_body;
    if let Some(frontmatter) = &doc.source.frontmatter {
        let eol = if doc.line_ending == "crlf" {
            "\r\n"
        } else {
            "\n"
        };
        let trailing_eol = if doc.source.frontmatter_trailing_eol {
            eol
        } else {
            ""
        };
        body = format!("---{eol}{frontmatter}---{trailing_eol}{body}");
    }
    if doc.has_bom {
        body = format!("\u{FEFF}{body}");
    }
    Ok(Some(body))
}

/// `handoff_doc_get` — read a document (by `doc_id` or `slug`) as `full`
/// (the original Markdown body + metadata), `meta` (metadata only), or
/// `section` (one section's body, byte-sliced from `_doc.<slug>.md`).
pub fn handle_doc_get(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;

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
            let doc = resolve_doc(&handoff, doc_id)?
                .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;
            Ok(to_json(&doc_metadata_json(&doc)))
        }
        "section" | "fragment" => {
            let seq = arguments
                .get("seq")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| anyhow::anyhow!("'seq' is required when format='section'"))?
                as usize;
            let doc = resolve_doc(&handoff, doc_id)?
                .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;
            let section =
                doc.sections.iter().find(|s| s.seq == seq).ok_or_else(|| {
                    anyhow::anyhow!("Section not found: doc_id={doc_id} seq={seq}")
                })?;
            let body = read_doc_body(&handoff, &doc.slug)?.ok_or_else(|| {
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
            let doc = resolve_doc(&handoff, doc_id)?
                .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;
            let body = read_full_body(&handoff, &doc)?.unwrap_or_default();
            let mut out = doc_metadata_json(&doc);
            out["body"] = json!(body);
            Ok(to_json(&out))
        }
    }
}

/// `handoff_doc_list` — list/search documents with optional `doc_type` /
/// `tags` (AND) / `task_id` filters, BM25 `query` ranking, and optional
/// reassembled `body` inclusion.
pub fn handle_doc_list(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;

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

    let mut docs = read_all_docs(&handoff)?;
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
        rank_docs_by_query(&handoff, &docs, q)?
    } else {
        (0..docs.len()).collect()
    };

    let mut out_docs = Vec::with_capacity(ordered_indices.len());
    for idx in ordered_indices {
        let d = &docs[idx];
        let mut entry = doc_metadata_json(d);
        if include_body {
            let body = read_full_body(&handoff, d)?.unwrap_or_default();
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

    let corpus = lexsim::Corpus::build(&index_texts);
    let query_tokens = lexsim::tokenize(query);
    let scope_paths: Vec<Vec<String>> = docs.iter().map(|d| d.scope_paths.clone()).collect();
    let config = RankConfig {
        min_score: DOC_QUERY_MIN_SCORE,
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
pub fn handle_doc_delete(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;

    let doc_id = arguments
        .get("doc_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'doc_id' is required"))?;

    let doc = resolve_doc(&handoff, doc_id)?
        .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;

    let mut warnings: Vec<String> = Vec::new();

    delete_doc_body(&handoff, &doc.slug)?;
    delete_doc(&handoff, &doc.slug)?;

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
        if let Some(mut parent) = find_doc_by_id(&handoff, parent_id)? {
            let before = parent.children.len();
            parent.children.retain(|c| c != &doc.id);
            if parent.children.len() != before {
                write_doc(&handoff, &parent)?;
            }
        } else {
            warnings.push(format!("Parent document not found: {parent_id}"));
        }
    }

    for child_id in &doc.children {
        if let Some(mut child) = find_doc_by_id(&handoff, child_id)? {
            child.parent_id = None;
            write_doc(&handoff, &child)?;
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
pub fn handle_doc_reassemble(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;

    let doc_id = arguments
        .get("doc_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'doc_id' is required"))?;

    let doc = resolve_doc(&handoff, doc_id)?
        .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;

    let stripped_body = read_doc_body(&handoff, &doc.slug)?
        .ok_or_else(|| anyhow::anyhow!("Document body file missing for slug '{}'", doc.slug))?;
    let drifted = lexsim::content_hash(&stripped_body) != doc.content_hash;

    let body = read_full_body(&handoff, &doc)?.unwrap_or_default();

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
pub fn handle_doc_tree(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;

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

    let doc = resolve_doc(&handoff, doc_id)?
        .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;

    let mut tree = doc_tree_node_json(&handoff, &doc, include_related)?;

    let parent = match &doc.parent_id {
        Some(parent_id) => find_doc_by_id(&handoff, parent_id)?.map(|p| doc_tree_summary_json(&p)),
        None => None,
    };
    tree["parent"] = parent.unwrap_or(Value::Null);

    tree["children"] = json!(doc_tree_children(
        &handoff,
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
/// otherwise -> "in_review".
fn recompute_verification_status(items: &[VerificationItem]) -> String {
    if items.iter().all(|i| i.status == "pending") {
        "pending".to_string()
    } else if items
        .iter()
        .all(|i| i.status == "verified" || i.status == "skipped")
    {
        "verified".to_string()
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

/// Summary counts used by both `doc_verify`'s mutation response and
/// `doc_verify_status`'s `progress` block.
struct VerificationCounts {
    checked: usize,
    skipped: usize,
    pending: usize,
    total: usize,
    stale: usize,
}

fn count_verification(doc: &DocMetadata, v: &Verification) -> VerificationCounts {
    let checked = v.items.iter().filter(|i| i.status == "verified").count();
    let skipped = v.items.iter().filter(|i| i.status == "skipped").count();
    let pending = v.items.iter().filter(|i| i.status == "pending").count();
    let stale = v.items.iter().filter(|i| item_is_stale(doc, i)).count();
    VerificationCounts {
        checked,
        skipped,
        pending,
        total: v.items.len(),
        stale,
    }
}

/// An item is stale when it was verified at a content_hash that no longer
/// matches its section's current content_hash (spec §3.5) — items never
/// verified (`content_hash_at_verify: None`) are never stale, and items whose
/// section has been removed (sync should have dropped them, but be
/// defensive) are treated as stale so drift is never silently hidden.
fn item_is_stale(doc: &DocMetadata, item: &VerificationItem) -> bool {
    let Some(hash_at_verify) = &item.content_hash_at_verify else {
        return false;
    };
    match doc.sections.iter().find(|s| s.seq == item.fragment_seq) {
        Some(section) => &section.content_hash != hash_at_verify,
        None => true,
    }
}

/// `handoff_doc_verify` — generate/check/skip/sync/set_refs a document's
/// verification matrix (wiki/140-verification-matrix.md §4.1).
pub fn handle_doc_verify(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;

    let doc_id = arguments
        .get("doc_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'doc_id' is required"))?;
    let action = arguments
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'action' is required"))?;

    let mut doc = resolve_doc(&handoff, doc_id)?
        .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;

    let now = chrono::Utc::now().to_rfc3339();

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
                    fragment_seq: s.seq,
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
            let fragment_seq = required_fragment_seq(arguments)?;
            let reviewer = arguments
                .get("reviewer")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let notes = arguments
                .get("notes")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let section_hash = doc
                .sections
                .iter()
                .find(|s| s.seq == fragment_seq)
                .map(|s| s.content_hash.clone());

            let v = verification_mut(&mut doc, doc_id)?;
            let item = find_item_mut(v, fragment_seq, doc_id)?;
            item.status = "verified".to_string();
            item.verified_at = Some(now.clone());
            if reviewer.is_some() {
                item.reviewer = reviewer;
            }
            if let Some(notes) = notes {
                item.notes = notes;
            }
            item.content_hash_at_verify = section_hash;
            v.updated_at = now.clone();
            v.status = recompute_verification_status(&v.items);
        }
        "skip" => {
            let fragment_seq = required_fragment_seq(arguments)?;
            let v = verification_mut(&mut doc, doc_id)?;
            let item = find_item_mut(v, fragment_seq, doc_id)?;
            item.status = "skipped".to_string();
            v.updated_at = now.clone();
            v.status = recompute_verification_status(&v.items);
        }
        "sync" => {
            let sections = doc.sections.clone();
            let v = verification_mut(&mut doc, doc_id)?;
            let current_seqs: std::collections::HashSet<usize> =
                sections.iter().map(|s| s.seq).collect();
            v.items.retain(|i| current_seqs.contains(&i.fragment_seq));
            let existing_seqs: std::collections::HashSet<usize> =
                v.items.iter().map(|i| i.fragment_seq).collect();
            for s in &sections {
                if !existing_seqs.contains(&s.seq) {
                    v.items.push(VerificationItem {
                        fragment_seq: s.seq,
                        heading: s.heading.clone(),
                        status: "pending".to_string(),
                        impl_refs: Vec::new(),
                        test_refs: Vec::new(),
                        reviewer: None,
                        verified_at: None,
                        notes: String::new(),
                        content_hash_at_verify: None,
                    });
                }
            }
            v.items.sort_by_key(|i| i.fragment_seq);
            v.updated_at = now.clone();
            v.status = recompute_verification_status(&v.items);
        }
        "set_refs" => {
            let fragment_seq = required_fragment_seq(arguments)?;
            let impl_refs = arguments.get("impl_refs").map(code_refs_from_value);
            let test_refs = arguments.get("test_refs").map(code_refs_from_value);

            let v = verification_mut(&mut doc, doc_id)?;
            let item = find_item_mut(v, fragment_seq, doc_id)?;
            if let Some(impl_refs) = impl_refs {
                item.impl_refs = impl_refs;
            }
            if let Some(test_refs) = test_refs {
                item.test_refs = test_refs;
            }
            v.updated_at = now.clone();
            v.status = recompute_verification_status(&v.items);
        }
        other => anyhow::bail!(
            "Unknown action '{other}'; expected one of generate, check, skip, sync, set_refs"
        ),
    }

    write_doc(&handoff, &doc)?;

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
    })))
}

fn required_fragment_seq(arguments: &Value) -> Result<usize> {
    arguments
        .get("fragment_seq")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .ok_or_else(|| anyhow::anyhow!("'fragment_seq' is required for this action"))
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
        .find(|i| i.fragment_seq == fragment_seq)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No verification item at fragment_seq={fragment_seq} for document {doc_id}"
            )
        })
}

/// `handoff_doc_verify_status` — verification matrix summary + optional
/// per-item detail with stale detection (wiki/140-verification-matrix.md
/// §4.2).
pub fn handle_doc_verify_status(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;

    let doc_id = arguments
        .get("doc_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'doc_id' is required"))?;
    let include_items = arguments
        .get("include_items")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let doc = resolve_doc(&handoff, doc_id)?
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
                })
            })
            .collect();
        out["items"] = json!(items);
    }

    Ok(to_json(&out))
}

/// `handoff_doc_graph` — build a graph of every document in the project:
/// `nodes[]` (one per document, with optional verification progress),
/// `edges[]` (explicit parent_child/related links, plus implicit
/// shared_task/shared_scope links when `include_implicit=true`), and
/// `layers` (doc ids grouped by `doc_type`).
pub fn handle_doc_graph(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;

    let include_implicit = arguments
        .get("include_implicit")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let include_verification = arguments
        .get("include_verification")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let docs = read_all_docs(&handoff)?;

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
pub fn handle_doc_trace(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;

    let doc_id = arguments
        .get("doc_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'doc_id' is required"))?;
    let direction = arguments
        .get("direction")
        .and_then(|v| v.as_str())
        .unwrap_or("both");

    let doc = resolve_doc(&handoff, doc_id)?
        .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;

    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    visited.insert(doc.id.clone());

    let mut branches: Vec<Value> = Vec::new();
    let mut chain: Vec<Value> = Vec::new();

    if direction == "up" || direction == "both" {
        chain.extend(doc_trace_walk_up(&handoff, &doc, &mut visited)?);
    }
    chain.push(doc_trace_item_json(&doc, None));
    if direction == "down" || direction == "both" {
        chain.extend(doc_trace_walk_down(
            &handoff,
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
        &handoff,
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
                    fragment_seq: 0,
                    heading: String::new(),
                    status: "verified".to_string(),
                    impl_refs: Vec::new(),
                    test_refs: Vec::new(),
                    reviewer: None,
                    verified_at: None,
                    notes: String::new(),
                    content_hash_at_verify: None,
                },
                VerificationItem {
                    fragment_seq: 1,
                    heading: "H".to_string(),
                    status: "pending".to_string(),
                    impl_refs: Vec::new(),
                    test_refs: Vec::new(),
                    reviewer: None,
                    verified_at: None,
                    notes: String::new(),
                    content_hash_at_verify: None,
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
