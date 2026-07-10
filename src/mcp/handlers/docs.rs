//! MCP handlers for document management (save / get / list) — P1-6a (t96.1).
//!
//! Builds on the storage layer in `crate::storage::docs` (split/reassemble +
//! fragment I/O) and the task<->doc bidirectional link sync in
//! `crate::storage::tasks::sync_doc_task_links`. See
//! `wiki/130-document-management.md` §5.1-§5.3 for the spec.

use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::resolve_project_dir;
use crate::context::injection::{rank_by_bm25_and_scope, RankConfig};
use crate::storage::docs::reassemble::reassemble;
use crate::storage::docs::split::split;
use crate::storage::docs::{
    delete_doc, delete_fragment, ensure_docs_dir, read_all_docs, read_all_fragments, read_doc,
    read_fragment, write_doc, write_fragment, DocMetadata, DocRelation, FragmentMetadata,
    FragmentSummary,
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

/// `handoff_doc_save` — create or update a document from a full Markdown
/// body: split into fragments, persist metadata + fragments, and sync the
/// task<->doc bidirectional link.
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
            read_doc(&handoff, id)?.ok_or_else(|| anyhow::anyhow!("Document not found: {id}"))?,
        ),
        None => None,
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
            let mut d =
                DocMetadata::new(id.clone(), title.clone(), "note".to_string(), now.clone());
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

    // Fragments actually present before this write (so we can delete any
    // stale fragment left over from a longer previous body on update).
    let old_seqs: Vec<usize> = doc.fragments.iter().map(|f| f.seq).collect();

    let mut fragment_summaries = Vec::with_capacity(split_doc.fragments.len());
    let mut fragment_bodies: Vec<&str> = Vec::with_capacity(split_doc.fragments.len());
    for frag in &split_doc.fragments {
        let meta = FragmentMetadata::new(
            id.clone(),
            frag.seq,
            frag.heading.clone(),
            frag.level,
            frag.body,
        );
        write_fragment(&handoff, &meta, frag.body)?;
        fragment_summaries.push(FragmentSummary {
            seq: frag.seq,
            heading: frag.heading.clone().unwrap_or_default(),
            level: frag.level,
        });
        fragment_bodies.push(frag.body);
    }

    let content_hash = lexsim::content_hash(&reassemble(&fragment_bodies));
    doc.content_hash = content_hash.clone();
    doc.source.canonical_hash = Some(content_hash.clone());
    doc.fragments = fragment_summaries;

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
    if doc.parent_id != previous_parent_id {
        if let Some(old_parent_id) = &previous_parent_id {
            if let Some(mut old_parent) = read_doc(&handoff, old_parent_id)? {
                let before = old_parent.children.len();
                old_parent.children.retain(|c| c != &id);
                if old_parent.children.len() != before {
                    write_doc(&handoff, &old_parent)?;
                }
            }
        }
        if let Some(new_parent_id) = &doc.parent_id {
            match read_doc(&handoff, new_parent_id)? {
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

    // Remove fragments that existed before this write but are no longer part
    // of the document (the new body is shorter than the old one).
    let new_seqs: std::collections::HashSet<usize> = doc.fragments.iter().map(|f| f.seq).collect();
    for seq in old_seqs {
        if !new_seqs.contains(&seq) {
            delete_fragment(&handoff, &id, seq)?;
        }
    }

    Ok(to_json(&json!({
        "doc_id": id,
        "title": doc.title,
        "doc_type": doc.doc_type,
        "fragment_count": doc.fragments.len(),
        "content_hash": doc.content_hash,
        "warnings": warnings,
    })))
}

/// `handoff_doc_get` — read a document as `full` (reassembled body +
/// metadata), `meta` (metadata only), or `fragment` (one fragment).
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
            let doc = read_doc(&handoff, doc_id)?
                .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;
            Ok(to_json(&doc_metadata_json(&doc)))
        }
        "fragment" => {
            let seq = arguments
                .get("seq")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| anyhow::anyhow!("'seq' is required when format='fragment'"))?
                as usize;
            let (meta, body) = read_fragment(&handoff, doc_id, seq)?
                .ok_or_else(|| anyhow::anyhow!("Fragment not found: doc_id={doc_id} seq={seq}"))?;
            Ok(to_json(&json!({
                "doc_id": meta.doc_id,
                "seq": meta.seq,
                "heading": meta.heading,
                "level": meta.level,
                "content_hash": meta.content_hash,
                "body": body,
            })))
        }
        _ => {
            let doc = read_doc(&handoff, doc_id)?
                .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;
            let fragments = read_all_fragments(&handoff, doc_id)?;
            let bodies: Vec<&str> = fragments.iter().map(|(_, b)| b.as_str()).collect();
            let mut body = reassemble(&bodies);
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
            let fragments = read_all_fragments(&handoff, &d.id)?;
            let bodies: Vec<&str> = fragments.iter().map(|(_, b)| b.as_str()).collect();
            let mut body = reassemble(&bodies);
            if let Some(frontmatter) = &d.source.frontmatter {
                let eol = if d.line_ending == "crlf" {
                    "\r\n"
                } else {
                    "\n"
                };
                let trailing_eol = if d.source.frontmatter_trailing_eol {
                    eol
                } else {
                    ""
                };
                body = format!("---{eol}{frontmatter}---{trailing_eol}{body}");
            }
            if d.has_bom {
                body = format!("\u{FEFF}{body}");
            }
            entry["body"] = json!(body);
        }
        out_docs.push(entry);
    }

    Ok(to_json(&json!({ "documents": out_docs })))
}

/// Ranks `docs` against `query` via BM25 over each document's index text
/// (title + tags + fragment bodies), returning original-order indices sorted
/// by descending relevance. Corpus is built fresh every call (no cache — the
/// cache is reserved for `doc_query`, t96.3, per the task's own note).
fn rank_docs_by_query(handoff: &Path, docs: &[DocMetadata], query: &str) -> Result<Vec<usize>> {
    let mut index_texts = Vec::with_capacity(docs.len());
    for d in docs {
        let fragments = read_all_fragments(handoff, &d.id)?;
        let mut text = d.title.clone();
        text.push(' ');
        text.push_str(&d.tags.join(" "));
        for (_, body) in &fragments {
            text.push(' ');
            text.push_str(body);
        }
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

/// `handoff_doc_delete` — delete a document and all its fragments, unlink it
/// from any linked tasks, remove it from its parent's `children`, and orphan
/// (clear `parent_id` on) any of its own children. See
/// `wiki/130-document-management.md` §5.4.
pub fn handle_doc_delete(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;

    let doc_id = arguments
        .get("doc_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'doc_id' is required"))?;

    let doc = read_doc(&handoff, doc_id)?
        .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;

    let mut warnings: Vec<String> = Vec::new();

    for frag in &doc.fragments {
        delete_fragment(&handoff, doc_id, frag.seq)?;
    }

    delete_doc(&handoff, doc_id)?;

    if !doc.task_ids.is_empty() {
        let tasks_dir = handoff.join("tasks");
        let report = sync_doc_task_links(&tasks_dir, doc_id, &doc.title, &[], &doc.task_ids)?;
        if !report.unresolved.is_empty() {
            warnings.push(format!(
                "Could not resolve task id(s) for unlinking: {}",
                report.unresolved.join(", ")
            ));
        }
    }

    if let Some(parent_id) = &doc.parent_id {
        if let Some(mut parent) = read_doc(&handoff, parent_id)? {
            let before = parent.children.len();
            parent.children.retain(|c| c != doc_id);
            if parent.children.len() != before {
                write_doc(&handoff, &parent)?;
            }
        } else {
            warnings.push(format!("Parent document not found: {parent_id}"));
        }
    }

    for child_id in &doc.children {
        if let Some(mut child) = read_doc(&handoff, child_id)? {
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
        "doc_id": doc_id,
        "fragment_count": doc.fragments.len(),
        "warnings": warnings,
    })))
}

/// `handoff_doc_reassemble` — reassemble a document's fragments (in `seq`
/// order) back into its original Markdown body, restoring BOM/frontmatter and
/// detecting drift (a fragment whose on-disk body no longer matches its
/// recorded `content_hash`). See `wiki/130-document-management.md` §5.5.
pub fn handle_doc_reassemble(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;

    let doc_id = arguments
        .get("doc_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'doc_id' is required"))?;

    let doc = read_doc(&handoff, doc_id)?
        .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;

    let fragments = read_all_fragments(&handoff, doc_id)?;
    let drifted = fragments
        .iter()
        .any(|(meta, body)| meta.content_hash != lexsim::content_hash(body));

    let bodies: Vec<&str> = fragments.iter().map(|(_, b)| b.as_str()).collect();
    let mut body = reassemble(&bodies);
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

    let output_path = arguments.get("output_path").and_then(|v| v.as_str());
    let mut out = json!({
        "doc_id": doc_id,
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

    let doc = read_doc(&handoff, doc_id)?
        .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;

    let mut tree = doc_tree_node_json(&handoff, &doc, include_related)?;

    let parent = match &doc.parent_id {
        Some(parent_id) => read_doc(&handoff, parent_id)?.map(|p| doc_tree_summary_json(&p)),
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
        let Some(child) = read_doc(handoff, child_id)? else {
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
            // same lenient policy as read_all_docs/read_all_fragments.
            let Some(target) = read_doc(handoff, &r.id)? else {
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

fn doc_metadata_json(doc: &DocMetadata) -> Value {
    json!({
        "id": doc.id,
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
        "fragments": doc.fragments,
        "fragment_count": doc.fragments.len(),
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
