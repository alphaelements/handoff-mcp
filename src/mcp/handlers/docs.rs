//! MCP handlers for document management (save / get / list) — P1-6a (t96.1).
//!
//! Builds on the storage layer in `crate::storage::docs` (split/reassemble +
//! fragment I/O) and the task<->doc bidirectional link sync in
//! `crate::storage::tasks::sync_doc_task_links`. See
//! `wiki/130-document-management.md` §5.1-§5.3 for the spec.

use std::path::Path;

use anyhow::Result;
use serde_json::{json, Value};

use super::resolve_project_dir;
use crate::context::injection::{rank_by_bm25_and_scope, RankConfig};
use crate::storage::docs::reassemble::reassemble;
use crate::storage::docs::split::split;
use crate::storage::docs::{
    delete_fragment, ensure_docs_dir, read_all_docs, read_all_fragments, read_doc, read_fragment,
    write_doc, write_fragment, DocMetadata, DocRelation, FragmentMetadata, FragmentSummary,
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
