//! MCP handlers for document context injection and bulk import — P1-6c
//! (t96.3): `handoff_doc_query`, `handoff_doc_analyze`, `handoff_doc_import`.
//!
//! `doc_query` mirrors `memory_query`'s ranking + per-session diff-injection
//! pattern (`crate::context::injection`), but injects at **fragment**
//! granularity with a staged `full`/`outline` payload depending on fragment
//! size (wiki/130-document-management.md §5.7, §7.1). `doc_analyze` /
//! `doc_import` implement the read-only-scan -> AI-review -> bulk-write
//! pattern used by `handoff_import_context` for tasks
//! (wiki/130-document-management.md §6.1), applied to Markdown documents.
//!
//! See `wiki/130-document-management.md` §5.7, §6.1 for the full spec.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::resolve_project_dir;
use crate::context::doc_corpus_cache;
use crate::context::injection::{filter_already_injected, rank_by_bm25_and_scope, RankConfig};
use crate::storage::docs::reassemble::extract_section;
use crate::storage::docs::split::{compute_sections, split, DEFAULT_SPLIT_LEVEL};
use crate::storage::docs::{
    docs_dir, ensure_docs_dir, read_all_docs, read_doc, read_doc_body, validate_slug, write_doc,
    write_doc_body, DocMetadata,
};
use crate::storage::ensure_handoff_exists;
use crate::storage::tasks::sync_doc_task_links;

/// Bonus added to a fragment's BM25 score when its parent document's
/// `scope_paths` prefix-matches one of the query's `file_paths`. Mirrors
/// `memory.rs`/`docs.rs`'s `SCOPE_PATH_BONUS`.
const SCOPE_PATH_BONUS: f64 = 2.0;

/// Extra bonus added when a fragment's parent document is linked to the
/// query's `task_id` (spec §5.7 ranking signal #1, "highest weight").
/// Deliberately larger than [`SCOPE_PATH_BONUS`] so a task-linked document
/// reliably outranks a merely scope-matching one.
const TASK_AFFINITY_BONUS: f64 = 5.0;

/// Default relevance floor for `doc_query`. Zero fragments are dropped purely
/// on score — the session-diff + `limit` truncation is what keeps noise down,
/// mirroring `doc_list`'s `DOC_QUERY_MIN_SCORE` rather than
/// `memory_query`'s hook-tuned floor (documents are explicitly authored/
/// imported, not free-form auto-captured notes).
const DOC_QUERY_MIN_SCORE: f64 = 0.0;

/// Default number of fragments `doc_query` returns per call when the caller
/// does not pass `limit`.
const DEFAULT_DOC_QUERY_LIMIT: usize = 5;

/// Fragment body token count at/below which `doc_query` injects the fragment
/// **full** (metadata + entire body); above this it injects **outline**
/// (metadata + heading only). Spec §7.1 default: 300.
const DOC_INLINE_THRESHOLD_TOKENS: usize = 300;

fn new_doc_id() -> String {
    format!("doc-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S-%6f"))
}

fn to_json(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

/// Read a `&[String]` from a JSON string-array argument (missing -> empty).
fn string_array(arguments: &Value, key: &str) -> Vec<String> {
    arguments
        .get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------
// handoff_doc_query
// ---------------------------------------------------------------------

/// One fragment-level "already injected" sidecar
/// (`.handoff/docs/injected/<session>.json`), keyed by `"<doc_id>#<seq>"` ->
/// injected `content_hash`. Deliberately fragment-scoped (not document-scoped
/// like memory's) since `doc_query` injects at fragment granularity — editing
/// one fragment must not suppress re-injection of its unrelated siblings.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DocInjectedSet {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    updated_at: String,
    #[serde(default)]
    injected: BTreeMap<String, String>,
    /// Documents explicitly suppressed via `suppress_doc_ids` +
    /// `suppress_until_changed: true` (spec §7.2.2): key = doc_id, value =
    /// the document's `content_hash` at the moment it was suppressed.
    /// Document-scoped (not fragment-scoped like `injected`) since the
    /// caller suppresses a whole document; the suppression is lifted for
    /// the whole document as soon as *any* of its fragments changes the
    /// document-level `content_hash`.
    #[serde(default)]
    suppressed: BTreeMap<String, String>,
}

impl DocInjectedSet {
    fn new(session_id: String, now: String) -> Self {
        DocInjectedSet {
            session_id,
            updated_at: now,
            injected: BTreeMap::new(),
            suppressed: BTreeMap::new(),
        }
    }

    fn key(doc_id: &str, seq: usize) -> String {
        format!("{doc_id}#{seq}")
    }

    fn already_injected(&self, doc_id: &str, seq: usize, content_hash: &str) -> bool {
        self.injected
            .get(&Self::key(doc_id, seq))
            .map(String::as_str)
            == Some(content_hash)
    }

    fn mark(&mut self, doc_id: &str, seq: usize, content_hash: &str) {
        self.injected
            .insert(Self::key(doc_id, seq), content_hash.to_string());
    }

    /// True when `doc_id` was suppressed at exactly its current
    /// `content_hash` — i.e. it hasn't changed since suppression, so it
    /// stays suppressed.
    fn is_suppressed(&self, doc_id: &str, content_hash: &str) -> bool {
        self.suppressed.get(doc_id).map(String::as_str) == Some(content_hash)
    }

    fn suppress(&mut self, doc_id: &str, content_hash: &str) {
        self.suppressed
            .insert(doc_id.to_string(), content_hash.to_string());
    }
}

fn docs_injected_dir(handoff: &Path) -> PathBuf {
    docs_dir(handoff).join("injected")
}

/// Sanitize a session id into a safe single-path-component filename stem,
/// mirroring `crate::storage::memory::injected`'s scheme (readable prefix +
/// a hash of the raw id, so distinct ids never collide and no path
/// separator/`..` can escape `injected/`).
fn sanitize_session_id(session_id: &str) -> String {
    let mut out = String::with_capacity(session_id.len());
    for ch in session_id.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-') {
            out.push(ch);
        } else if ch == '.' {
            if !out.is_empty() {
                out.push('.');
            }
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_end_matches('.');
    let prefix: String = trimmed.chars().take(96).collect();
    let prefix = if prefix.is_empty() { "anon" } else { &prefix };
    format!("{prefix}-{}", lexsim::fnv1a_hex(session_id.as_bytes()))
}

fn docs_injected_path(handoff: &Path, session_id: &str) -> PathBuf {
    docs_injected_dir(handoff).join(format!("{}.json", sanitize_session_id(session_id)))
}

fn read_docs_injected_set(handoff: &Path, session_id: &str, now: &str) -> DocInjectedSet {
    let path = docs_injected_path(handoff, session_id);
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str::<DocInjectedSet>(&content)
            .unwrap_or_else(|_| DocInjectedSet::new(session_id.to_string(), now.to_string())),
        Err(_) => DocInjectedSet::new(session_id.to_string(), now.to_string()),
    }
}

fn write_docs_injected_set(handoff: &Path, set: &DocInjectedSet) -> Result<()> {
    std::fs::create_dir_all(docs_injected_dir(handoff))?;
    let path = docs_injected_path(handoff, &set.session_id);
    let content = serde_json::to_string_pretty(set)?;
    crate::storage::atomic_write(&path, content.as_bytes())?;
    Ok(())
}

/// One section candidate flattened out of every document's section manifest,
/// used to build the BM25 corpus and carry the parent doc's ranking-relevant
/// fields (scope_paths/task_ids) alongside each section. `body` is the
/// byte-sliced section text (v5: extracted in-memory from `_doc.<slug>.md`
/// via `sections[].byte_offset`/`byte_length`, not read from a separate
/// fragment file).
struct SectionCandidate<'a> {
    doc: &'a DocMetadata,
    seq: usize,
    heading: String,
    body: String,
    content_hash: String,
}

/// `handoff_doc_query` — inject document fragments relevant to the current
/// prompt/file/task, staged `full` (body) or `outline` (heading only)
/// depending on fragment size (spec §5.7, §7.1).
pub fn handle_doc_query(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;

    let text = arguments
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let file_paths = string_array(arguments, "file_paths");
    let task_id = arguments
        .get("task_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let session_id = arguments
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let limit = arguments
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_DOC_QUERY_LIMIT);
    let mark_injected = arguments
        .get("mark_injected")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let suppress_doc_ids = string_array(arguments, "suppress_doc_ids");
    let suppress_until_changed = arguments
        .get("suppress_until_changed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let docs = read_all_docs(&handoff)?;
    if docs.is_empty() {
        return Ok(to_json(&json!({ "documents": [], "injected_count": 0 })));
    }

    // Documents suppressed for this call: explicit `suppress_doc_ids`, plus
    // (with a session_id) anything the sidecar remembers as suppressed at
    // its still-current content_hash (spec §7.2.2 "session-scoped temporary
    // suppression, until content_hash changes").
    let now = chrono::Utc::now().to_rfc3339();
    let injected_set = session_id.map(|sid| read_docs_injected_set(&handoff, sid, &now));
    let is_doc_suppressed = |doc: &DocMetadata| -> bool {
        if suppress_doc_ids.iter().any(|id| id == &doc.id) {
            return true;
        }
        match &injected_set {
            Some(set) => set.is_suppressed(&doc.id, &doc.content_hash),
            None => false,
        }
    };

    let mut candidates: Vec<SectionCandidate> = Vec::new();
    for doc in &docs {
        if is_doc_suppressed(doc) {
            continue;
        }
        let Some(body) = read_doc_body(&handoff, &doc.slug)? else {
            continue;
        };
        for section in &doc.sections {
            // Best-effort ranking pass over every document: if this one
            // section's recorded byte range has drifted from the body
            // currently on disk (out-of-band edit), skip just that section
            // rather than failing the whole `doc_query` call for every
            // other unaffected document.
            let Ok(section_body) = extract_section(&body, section) else {
                continue;
            };
            candidates.push(SectionCandidate {
                doc,
                seq: section.seq,
                heading: section.heading.clone(),
                body: section_body.to_string(),
                content_hash: section.content_hash.clone(),
            });
        }
    }
    if candidates.is_empty() {
        persist_suppressed_doc_ids(
            &handoff,
            session_id,
            &docs,
            &suppress_doc_ids,
            suppress_until_changed,
            &now,
        )?;
        return Ok(to_json(&json!({ "documents": [], "injected_count": 0 })));
    }

    // Index text per section: heading + body (title/tags folded in so a
    // query for the doc's title still surfaces its sections).
    let doc_texts: Vec<String> = candidates
        .iter()
        .map(|c| {
            let mut t = c.doc.title.clone();
            t.push(' ');
            t.push_str(&c.doc.tags.join(" "));
            t.push(' ');
            if !c.heading.is_empty() {
                t.push_str(&c.heading);
                t.push(' ');
            }
            t.push_str(&c.body);
            t
        })
        .collect();

    // `lexsim::Corpus` is not `Clone`, so ranking happens while the cache's
    // mutex guard (and thus a live `&Corpus` borrow) is held. The MCP server
    // is single-threaded stdio (see `crate::context` module docs), so this
    // is never contended in practice.
    let mut cache = doc_corpus_cache()
        .lock()
        .map_err(|_| anyhow::anyhow!("doc corpus cache mutex poisoned"))?;
    let corpus = cache.get_or_build_corpus(&doc_texts);

    let mut query_tokens = lexsim::tokenize_weighted(&text);
    for p in &file_paths {
        query_tokens.extend(lexsim::tokenize_weighted(&basename(p)));
    }

    let scope_paths: Vec<Vec<String>> = candidates
        .iter()
        .map(|c| c.doc.scope_paths.clone())
        .collect();
    let rank_config = RankConfig {
        min_score: DOC_QUERY_MIN_SCORE,
        relative_threshold: 0.0,
        scope_path_bonus: SCOPE_PATH_BONUS,
        limit: candidates.len(),
    };
    let mut ranked = rank_by_bm25_and_scope(
        corpus,
        &query_tokens,
        &scope_paths,
        &file_paths,
        &rank_config,
    );
    drop(cache);

    // Task-affinity bonus (spec §5.7 signal #1, highest weight): applied
    // after the shared ranker since it is doc_query-specific.
    if let Some(tid) = task_id {
        for item in &mut ranked {
            if candidates[item.index].doc.task_ids.iter().any(|t| t == tid) {
                item.score += TASK_AFFINITY_BONUS;
            }
        }
        ranked.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    let already_injected = |i: usize| match &injected_set {
        Some(set) => {
            let c = &candidates[i];
            set.already_injected(&c.doc.id, c.seq, &c.content_hash)
        }
        None => false,
    };
    let fresh = filter_already_injected(ranked, already_injected, limit);

    let out: Vec<Value> = fresh
        .iter()
        .map(|item| {
            let c = &candidates[item.index];
            let tokens = lexsim::estimate_tokens(&c.body);
            let depth = if tokens <= DOC_INLINE_THRESHOLD_TOKENS {
                "full"
            } else {
                "outline"
            };
            let mut entry = json!({
                "doc_id": c.doc.id,
                "title": c.doc.title,
                "doc_type": c.doc.doc_type,
                "fragment_seq": c.seq,
                "heading": c.heading,
                "task_ids": c.doc.task_ids,
                "depth": depth,
                "tokens": tokens,
                "score": round2(item.score),
            });
            if depth == "full" {
                entry["body"] = json!(c.body);
            } else {
                // outline: heading only, plus the sibling table of contents
                // so the AI can pick a seq to fetch via doc_get(format="section").
                entry["outline"] = json!(c
                    .doc
                    .sections
                    .iter()
                    .map(|s| json!({ "seq": s.seq, "heading": s.heading, "level": s.level }))
                    .collect::<Vec<_>>());
            }
            entry
        })
        .collect();

    if mark_injected && !fresh.is_empty() {
        if let Some(sid) = session_id {
            let mut set = read_docs_injected_set(&handoff, sid, &now);
            set.updated_at = now.clone();
            for item in &fresh {
                let c = &candidates[item.index];
                set.mark(&c.doc.id, c.seq, &c.content_hash);
            }
            write_docs_injected_set(&handoff, &set)?;
        }
    }
    persist_suppressed_doc_ids(
        &handoff,
        session_id,
        &docs,
        &suppress_doc_ids,
        suppress_until_changed,
        &now,
    )?;

    Ok(to_json(&json!({
        "documents": out,
        "injected_count": out.len(),
    })))
}

/// Record `suppress_doc_ids` in the session's `injected/` sidecar as
/// "suppressed at this content_hash" (spec §7.2.2), when
/// `suppress_until_changed` is requested. A no-op when there's no
/// `session_id`, no `suppress_doc_ids`, or `suppress_until_changed` is
/// false — called from both `handle_doc_query`'s early-out (candidates
/// empty, e.g. every candidate got suppressed) and its normal return path,
/// so the suppression sticks either way.
fn persist_suppressed_doc_ids(
    handoff: &Path,
    session_id: Option<&str>,
    docs: &[DocMetadata],
    suppress_doc_ids: &[String],
    suppress_until_changed: bool,
    now: &str,
) -> Result<()> {
    if !suppress_until_changed || suppress_doc_ids.is_empty() {
        return Ok(());
    }
    let Some(sid) = session_id else {
        return Ok(());
    };
    let mut set = read_docs_injected_set(handoff, sid, now);
    set.updated_at = now.to_string();
    for doc in docs {
        if suppress_doc_ids.iter().any(|id| id == &doc.id) {
            set.suppress(&doc.id, &doc.content_hash);
        }
    }
    write_docs_injected_set(handoff, &set)?;
    Ok(())
}

fn basename(p: &str) -> String {
    p.rsplit(['/', '\\']).next().unwrap_or(p).to_string()
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

// ---------------------------------------------------------------------
// handoff_doc_analyze
// ---------------------------------------------------------------------

/// Regex-free scan of `body` for `[text](target)` Markdown links.
fn extract_markdown_links(body: &str) -> Vec<(String, String)> {
    let mut links = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            if let Some(close_bracket) = body[i + 1..].find(']') {
                let close_bracket = i + 1 + close_bracket;
                if body.as_bytes().get(close_bracket + 1) == Some(&b'(') {
                    if let Some(close_paren) = body[close_bracket + 2..].find(')') {
                        let close_paren = close_bracket + 2 + close_paren;
                        let text = body[i + 1..close_bracket].to_string();
                        let target = body[close_bracket + 2..close_paren].to_string();
                        links.push((text, target));
                        i = close_paren + 1;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    links
}

/// Detect a `doc_type` from a title/body via keyword scan (spec §6.1 table).
fn detect_doc_type(title: &str, body: &str) -> String {
    let hay = format!("{title} {body}").to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| hay.contains(n));
    if has(&["要求", "requirement"]) {
        "spec".to_string()
    } else if has(&["設計", "design"]) {
        "design".to_string()
    } else if has(&["テスト", "test"]) {
        "test-spec".to_string()
    } else if has(&["adr", "決定"]) {
        "adr".to_string()
    } else if has(&["ガイド", "guide"]) {
        "guide".to_string()
    } else {
        "note".to_string()
    }
}

/// Detect tags from frontmatter (if present) and heading tokens.
fn detect_tags(frontmatter: Option<&str>, headings: &[String]) -> Vec<String> {
    let mut tags = Vec::new();
    if let Some(fm) = frontmatter {
        for line in fm.lines() {
            if let Some(rest) = line.trim_start().strip_prefix("tags:") {
                let rest = rest.trim();
                let list = rest.trim_start_matches('[').trim_end_matches(']');
                for part in list.split(',') {
                    let t = part.trim().trim_matches('"').trim_matches('\'');
                    if !t.is_empty() {
                        tags.push(t.to_string());
                    }
                }
            }
        }
    }
    for h in headings {
        for tok in lexsim::tokenize(h) {
            // `tokenize` also emits internal cross-language character n-grams
            // (marker-prefixed) alongside real word tokens — useful for BM25
            // matching, but not for a human-facing tag list.
            if tok.len() > 1 && !lexsim::is_cl_ngram(&tok) && !tags.contains(&tok) {
                tags.push(tok);
            }
        }
    }
    tags
}

/// Detect candidate `scope_paths` from inline-code / fenced-code file paths
/// (must contain `/` and end in a recognizable extension).
fn detect_scope_paths(body: &str) -> Vec<String> {
    const EXTENSIONS: &[&str] = &[
        ".rs", ".ts", ".tsx", ".js", ".jsx", ".toml", ".json", ".md", ".py", ".go",
    ];
    let mut found = Vec::new();
    let mut token = String::new();
    let flush = |token: &mut String, found: &mut Vec<String>| {
        if token.contains('/') && EXTENSIONS.iter().any(|e| token.ends_with(e)) {
            let cleaned = token.trim_matches(|c: char| {
                !c.is_ascii_alphanumeric() && c != '/' && c != '.' && c != '_' && c != '-'
            });
            if !cleaned.is_empty() && !found.contains(&cleaned.to_string()) {
                found.push(cleaned.to_string());
            }
        }
        token.clear();
    };
    for ch in body.chars() {
        if ch.is_whitespace() || matches!(ch, '`' | '(' | ')' | '[' | ']' | ',') {
            flush(&mut token, &mut found);
        } else {
            token.push(ch);
        }
    }
    flush(&mut token, &mut found);
    found
}

/// One file's automatic analysis, ready either for direct import or for
/// AI review (`needs_review`) when its confidence is low or it has
/// unresolvable signals.
struct AnalyzedFile {
    file: String,
    title: String,
    body: String,
    doc_type: String,
    tags: Vec<String>,
    scope_paths: Vec<String>,
    links: Vec<(String, String)>,
    parent_dir: Option<String>,
    index_text: String,
}

/// Collect every heading (`#`..`######`) in `body`, in document order, paired
/// with its text (without the leading `#` markers).
fn extract_headings(body: &str) -> Vec<String> {
    body.lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with('#') {
                let text = trimmed.trim_start_matches('#').trim();
                if !text.is_empty() {
                    return Some(text.to_string());
                }
            }
            None
        })
        .collect()
}

fn analyze_one_file(root: &Path, path: &Path) -> Result<AnalyzedFile> {
    let body = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Failed to read {}: {e}", path.display()))?;
    let headings = extract_headings(&body);
    let title = headings.first().cloned().unwrap_or_else(|| file_stem(path));

    let split_doc = split(&body, DEFAULT_SPLIT_LEVEL).ok();
    let frontmatter = split_doc.as_ref().and_then(|d| d.frontmatter);

    let doc_type = detect_doc_type(&title, &body);
    let tags = detect_tags(frontmatter, &headings);
    let scope_paths = detect_scope_paths(&body);
    let links = extract_markdown_links(&body);

    let rel = path.strip_prefix(root).unwrap_or(path);
    let file = rel.to_string_lossy().replace('\\', "/");
    let parent_dir = rel
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| format!("{}/", p.to_string_lossy().replace('\\', "/")));

    let index_text = format!("{title} {}", tags.join(" "));

    Ok(AnalyzedFile {
        file,
        title,
        body,
        doc_type,
        tags,
        scope_paths,
        links,
        parent_dir,
        index_text,
    })
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

/// Collect every `*.md` file under `path` (or just `path` itself if it is a
/// file). `recursive=false` limits a directory scan to its immediate
/// children.
fn collect_markdown_files(path: &Path, recursive: bool) -> Result<Vec<PathBuf>> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    if !path.is_dir() {
        anyhow::bail!("Path not found: {}", path.display());
    }
    let mut files = Vec::new();
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)
            .map_err(|e| anyhow::anyhow!("Failed to read dir {}: {e}", dir.display()))?
        {
            let entry = entry?;
            let p = entry.path();
            if p.is_dir() {
                if recursive {
                    stack.push(p);
                }
            } else if p.extension().and_then(|e| e.to_str()) == Some("md") {
                files.push(p);
            }
        }
    }
    files.sort();
    Ok(files)
}

/// `handoff_doc_analyze` — read-only scan of a file or directory, producing
/// a conditioning report (spec §6.1 step 1). Never writes anything.
pub fn handle_doc_analyze(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let raw_path = arguments
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'path' is required"))?;
    let recursive = arguments
        .get("recursive")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let flatten = arguments
        .get("flatten")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let scan_root = project_dir.join(raw_path);
    let scan_root = std::fs::canonicalize(&scan_root)
        .map_err(|e| anyhow::anyhow!("Invalid path '{raw_path}': {e}"))?;
    if !scan_root.starts_with(&project_dir) {
        anyhow::bail!("path '{}' resolves outside the project directory", raw_path);
    }
    // The root used for relative-path reporting: the scan target's parent
    // when it's a single file, or the directory itself.
    let report_root = if scan_root.is_file() {
        scan_root
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| scan_root.clone())
    } else {
        scan_root.clone()
    };

    let files = collect_markdown_files(&scan_root, recursive)?;
    let mut analyzed = Vec::with_capacity(files.len());
    for f in &files {
        analyzed.push(analyze_one_file(&report_root, f)?);
    }

    // Link analysis: classify each link as internal (matches another
    // scanned file's path or heading), external (URL), or broken. Headings
    // are collected once across every scanned file (not just the linking
    // file) so a link can target a heading in a sibling document.
    let known_files: Vec<&str> = analyzed.iter().map(|a| a.file.as_str()).collect();
    let known_headings: Vec<String> = analyzed
        .iter()
        .flat_map(|a| extract_headings(&a.body))
        .collect();

    let mut auto_resolved = Vec::new();
    let mut needs_review = Vec::new();

    for a in &analyzed {
        let mut broken_links = Vec::new();
        for (text, target) in &a.links {
            if target.starts_with("http://") || target.starts_with("https://") {
                continue; // external — not reported as an issue
            }
            let target_path = target.split('#').next().unwrap_or(target);
            let is_internal = target_path.is_empty()
                || known_files
                    .iter()
                    .any(|f| f.ends_with(target_path) || target_path.ends_with(f))
                || known_headings.iter().any(|h| target.contains(h.as_str()));
            if !is_internal {
                broken_links.push((text.clone(), target.clone()));
            }
        }
        for (text, target) in &broken_links {
            needs_review.push(json!({
                "file": a.file,
                "issue": "broken_link",
                "detail": format!("Link '{text}' -> '{target}' does not match any scanned file or heading"),
                "suggestion": { "action": "link_to" },
                "context": format!("link text: '{text}'"),
            }));
        }

        let confidence = if broken_links.is_empty() { 0.9 } else { 0.5 };
        auto_resolved.push(json!({
            "file": a.file,
            "title": a.title,
            "doc_type": a.doc_type,
            "tags": a.tags,
            "scope_paths": a.scope_paths,
            "confidence": confidence,
            "suggested_slug": slugify(&a.title),
        }));
    }

    // Near-duplicate detection: pairwise Jaccard similarity over index text.
    for i in 0..analyzed.len() {
        for j in (i + 1)..analyzed.len() {
            let score = lexsim::jaccard(&analyzed[i].index_text, &analyzed[j].index_text);
            if score >= 0.7 {
                needs_review.push(json!({
                    "file": analyzed[i].file,
                    "issue": "near_duplicate",
                    "detail": format!(
                        "{} and {} have similarity {:.2}",
                        analyzed[i].file, analyzed[j].file, score
                    ),
                    "suggestion": { "action": "merge_or_reference" },
                }));
            }
        }
    }

    let mut proposed_tree = serde_json::Map::new();
    if !flatten {
        let mut by_parent: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for a in &analyzed {
            if let Some(parent) = &a.parent_dir {
                by_parent
                    .entry(parent.clone())
                    .or_default()
                    .push(a.file.clone());
            }
        }
        for (parent, children) in by_parent {
            proposed_tree.insert(parent, json!({ "children": children, "doc_type": "note" }));
        }
    }

    Ok(to_json(&json!({
        "files_scanned": analyzed.len(),
        "auto_resolved": auto_resolved,
        "needs_review": needs_review,
        "proposed_tree": Value::Object(proposed_tree),
    })))
}

// ---------------------------------------------------------------------
// handoff_doc_import
// ---------------------------------------------------------------------

/// `handoff_doc_import` — bulk-write an analyzed payload (spec §6.1 step 3).
/// Applies `overrides`, splits + persists every file as a document, wires up
/// `proposed_tree` parent/child relationships, links `task_ids` to every
/// imported document, and bumps the doc corpus cache generation.
pub fn handle_doc_import(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;
    ensure_docs_dir(&handoff)?;

    let analyzed = arguments
        .get("analyzed")
        .ok_or_else(|| anyhow::anyhow!("'analyzed' is required"))?;
    let auto_resolved = analyzed
        .get("auto_resolved")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if auto_resolved.is_empty() {
        anyhow::bail!("'analyzed.auto_resolved' must contain at least one file entry");
    }

    let overrides: BTreeMap<String, Value> = arguments
        .get("overrides")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|o| {
                    o.get("file")
                        .and_then(|f| f.as_str())
                        .map(|f| (f.to_string(), o.clone()))
                })
                .collect()
        })
        .unwrap_or_default();

    let task_ids = string_array(arguments, "task_ids");

    let now = chrono::Utc::now().to_rfc3339();
    let mut warnings: Vec<String> = Vec::new();
    let mut imported_docs: Vec<Value> = Vec::new();
    let mut file_to_doc_id: BTreeMap<String, String> = BTreeMap::new();
    // Slugs claimed so far by this import batch, seeded with every slug
    // already on disk — `unique_slug` disambiguates against both.
    let mut used_slugs: std::collections::HashSet<String> = read_all_docs(&handoff)?
        .into_iter()
        .map(|d| d.slug)
        .collect();

    // Pass 1: validate every entry has a resolvable body before writing
    // anything (mirrors handoff_import_context's validate-then-write
    // pattern — a rejection mid-batch must not leave a half-written tree).
    for entry in &auto_resolved {
        let file = entry.get("file").and_then(|v| v.as_str()).ok_or_else(|| {
            anyhow::anyhow!("Each 'analyzed.auto_resolved' entry requires 'file'")
        })?;
        if entry.get("body").and_then(|v| v.as_str()).is_none() {
            anyhow::bail!(
                "'analyzed.auto_resolved' entry for '{file}' is missing 'body' \
                 (doc_import writes from the payload; it does not re-read the filesystem)"
            );
        }
    }

    // Pass 2: write every document. `file`/`body` are re-extracted with the
    // same `ok_or_else` shape as pass 1 (never `unwrap()`) even though pass 1
    // already rejected any entry missing either — belt-and-suspenders against
    // the two passes ever drifting apart.
    for entry in &auto_resolved {
        let file = entry
            .get("file")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Each 'analyzed.auto_resolved' entry requires 'file'"))?
            .to_string();
        let body = entry.get("body").and_then(|v| v.as_str()).ok_or_else(|| {
            anyhow::anyhow!("'analyzed.auto_resolved' entry for '{file}' is missing 'body'")
        })?;
        let override_entry = overrides.get(&file);

        let title = override_entry
            .and_then(|o| o.get("title"))
            .and_then(|v| v.as_str())
            .or_else(|| entry.get("title").and_then(|v| v.as_str()))
            .unwrap_or(&file)
            .to_string();
        let doc_type = override_entry
            .and_then(|o| o.get("doc_type"))
            .and_then(|v| v.as_str())
            .or_else(|| entry.get("doc_type").and_then(|v| v.as_str()))
            .unwrap_or("note")
            .to_string();
        let tags: Vec<String> = override_entry
            .and_then(|o| o.get("tags"))
            .or_else(|| entry.get("tags"))
            .map(string_array_value)
            .unwrap_or_default();
        let scope_paths: Vec<String> = override_entry
            .and_then(|o| o.get("scope_paths"))
            .or_else(|| entry.get("scope_paths"))
            .map(string_array_value)
            .unwrap_or_default();

        let slug_override = override_entry
            .and_then(|o| o.get("slug"))
            .and_then(|v| v.as_str())
            .or_else(|| entry.get("suggested_slug").and_then(|v| v.as_str()));
        let slug = unique_slug(&handoff, &mut used_slugs, slug_override, &title, &file)?;

        let split_doc = split(body, DEFAULT_SPLIT_LEVEL)?;
        let id = new_doc_id();

        let mut doc = DocMetadata::new(
            id.clone(),
            slug.clone(),
            title.clone(),
            doc_type.clone(),
            now.clone(),
        );
        doc.tags = tags;
        doc.scope_paths = scope_paths;
        doc.source.origin = "imported".to_string();
        doc.source.original_path = Some(file.clone());
        doc.has_bom = split_doc.has_bom;
        doc.line_ending = split_doc.line_ending.to_string();
        doc.source.frontmatter = split_doc.frontmatter.map(str::to_string);
        doc.source.frontmatter_trailing_eol = split_doc.frontmatter_trailing_eol;

        let body_after_strip: String = split_doc.fragments.iter().map(|f| f.body).collect();
        write_doc_body(&handoff, &slug, &body_after_strip)?;
        doc.sections = compute_sections(&split_doc);
        doc.content_hash = lexsim::content_hash(&body_after_strip);
        doc.source.canonical_hash = Some(doc.content_hash.clone());
        doc.task_ids = task_ids.clone();

        write_doc(&handoff, &doc)?;

        file_to_doc_id.insert(file.clone(), id.clone());
        imported_docs.push(json!({
            "doc_id": id,
            "slug": doc.slug,
            "title": doc.title,
            "section_count": doc.sections.len(),
        }));
    }

    // Pass 3: apply proposed_tree parent/child relationships (best-effort —
    // an unresolvable parent directory entry is reported, not fatal).
    if let Some(tree) = analyzed.get("proposed_tree").and_then(|v| v.as_object()) {
        for (parent_key, node) in tree {
            let children = node
                .get("children")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let child_doc_ids: Vec<String> = children
                .iter()
                .filter_map(|c| c.as_str())
                .filter_map(|f| file_to_doc_id.get(f).cloned())
                .collect();
            if child_doc_ids.is_empty() {
                continue;
            }
            // Synthesize a parent document for the directory grouping when
            // one doesn't already exist among the imported files.
            let parent_id = new_doc_id();
            let parent_title = parent_key.trim_end_matches('/').to_string();
            let parent_slug = unique_slug(
                &handoff,
                &mut used_slugs,
                None,
                &parent_title,
                &parent_title,
            )?;
            let mut parent_doc = DocMetadata::new(
                parent_id.clone(),
                parent_slug,
                parent_title,
                node.get("doc_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("note")
                    .to_string(),
                now.clone(),
            );
            parent_doc.source.origin = "imported".to_string();
            parent_doc.children = child_doc_ids.clone();
            write_doc(&handoff, &parent_doc)?;

            for child_id in &child_doc_ids {
                if let Ok(Some(mut child)) =
                    crate::storage::docs::find_doc_by_id(&handoff, child_id)
                {
                    child.parent_id = Some(parent_id.clone());
                    write_doc(&handoff, &child)?;
                }
            }
            imported_docs.push(json!({
                "doc_id": parent_id,
                "slug": parent_doc.slug,
                "title": parent_doc.title,
                "section_count": 0,
            }));
        }
    }

    // Pass 4: link every imported document (leaves + synthesized parents) to
    // task_ids, if any.
    if !task_ids.is_empty() {
        let tasks_dir = handoff.join("tasks");
        for doc_val in &imported_docs {
            let doc_id = doc_val["doc_id"].as_str().unwrap_or_default();
            let title = doc_val["title"].as_str().unwrap_or_default();
            let report = sync_doc_task_links(&tasks_dir, doc_id, title, &task_ids, &[])?;
            if !report.unresolved.is_empty() {
                warnings.push(format!(
                    "Could not resolve task id(s) for linking doc {doc_id}: {}",
                    report.unresolved.join(", ")
                ));
            }
        }
    }

    // Invalidate the doc corpus cache so the next doc_query sees the import.
    {
        let mut cache = doc_corpus_cache()
            .lock()
            .map_err(|_| anyhow::anyhow!("doc corpus cache mutex poisoned"))?;
        cache.increment_generation();
    }

    Ok(to_json(&json!({
        "imported_count": imported_docs.len(),
        "documents": imported_docs,
        "warnings": warnings,
    })))
}

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

/// Derives a candidate slug (`[a-z0-9-]`, max
/// [`crate::storage::docs::model::MAX_SLUG_LEN`]) from `text` by
/// lowercasing, replacing any run of non-`[a-z0-9]` characters with a single
/// hyphen, and trimming leading/trailing hyphens. Falls back to `"doc"` if
/// the result would otherwise be empty (e.g. `text` is entirely
/// non-ASCII/punctuation), so [`unique_slug`] always has a non-empty base to
/// disambiguate.
fn slugify(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_was_hyphen = true; // suppress a leading hyphen
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_hyphen = false;
        } else if !last_was_hyphen {
            out.push('-');
            last_was_hyphen = true;
        }
    }
    let trimmed = out.trim_end_matches('-');
    let truncated: String = trimmed
        .chars()
        .take(crate::storage::docs::model::MAX_SLUG_LEN)
        .collect();
    let truncated = truncated.trim_end_matches('-');
    if truncated.is_empty() {
        "doc".to_string()
    } else {
        truncated.to_string()
    }
}

/// Picks a slug for a `doc_import` entry: an explicit override/suggestion if
/// given (still slugified/validated), else derived from `title`, falling
/// back to `file` when the title slugifies to nothing usable. Disambiguates
/// against `used_slugs` (every slug already on disk plus every slug already
/// claimed earlier in this same import batch) by appending `-2`, `-3`, …
/// Registers the chosen slug into `used_slugs` before returning it.
fn unique_slug(
    handoff: &Path,
    used_slugs: &mut std::collections::HashSet<String>,
    preferred: Option<&str>,
    title: &str,
    file: &str,
) -> Result<String> {
    let base = match preferred {
        Some(p) => slugify(p),
        None => {
            let from_title = slugify(title);
            if from_title == "doc" {
                slugify(file)
            } else {
                from_title
            }
        }
    };
    validate_slug(&base)?;

    if !used_slugs.contains(&base) && read_doc(handoff, &base)?.is_none() {
        used_slugs.insert(base.clone());
        return Ok(base);
    }

    // Reserve room for the "-N" disambiguation suffix up front: truncate
    // `base` so every candidate `format!("{truncated}-{n}")` fits within
    // `MAX_SLUG_LEN`, even for the largest `n` we're willing to try. Without
    // this, a `base` already at `MAX_SLUG_LEN` chars makes every candidate
    // exceed the limit and previously caused an unbounded loop (never
    // returning, never erroring — a live CPU-pinning bug found in review).
    const MAX_ATTEMPTS: usize = 999;
    let max_suffix_len = format!("-{MAX_ATTEMPTS}").len();
    let max_base_len = crate::storage::docs::model::MAX_SLUG_LEN - max_suffix_len;
    let truncated_base = if base.len() > max_base_len {
        &base[..max_base_len]
    } else {
        &base[..]
    };

    for n in 2..=MAX_ATTEMPTS {
        let candidate = format!("{truncated_base}-{n}");
        if !used_slugs.contains(&candidate) && read_doc(handoff, &candidate)?.is_none() {
            used_slugs.insert(candidate.clone());
            return Ok(candidate);
        }
    }
    bail!(
        "could not find a unique slug for base '{base}' after {MAX_ATTEMPTS} attempts; \
         pick a more specific slug override"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Regression test for a MAJOR bug found in review: when `base` is
    /// already `MAX_SLUG_LEN` chars long and taken, every disambiguation
    /// candidate `format!("{base}-{n}")` is longer than `MAX_SLUG_LEN`, so
    /// the old `for n in 2..` loop's length guard rejected every candidate
    /// and looped forever (never reaching `unreachable!()`), spinning a CPU
    /// core for the life of the process. `unique_slug` must instead reserve
    /// suffix room by truncating `base` and return a real `Err` if
    /// disambiguation is exhausted, never hang.
    #[test]
    fn unique_slug_disambiguates_when_base_is_at_max_length() {
        let tmp = TempDir::new().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(handoff.join("docs")).unwrap();

        let base = "a".repeat(crate::storage::docs::model::MAX_SLUG_LEN);
        let mut used_slugs: std::collections::HashSet<String> = std::collections::HashSet::new();
        used_slugs.insert(base.clone());

        let slug = unique_slug(&handoff, &mut used_slugs, Some(&base), "Title", "file.md")
            .expect("must disambiguate instead of hanging or erroring");
        assert!(slug.len() <= crate::storage::docs::model::MAX_SLUG_LEN);
        assert_ne!(slug, base);
        assert!(used_slugs.contains(&slug));
    }

    /// When every disambiguation slot is already taken, `unique_slug` must
    /// return a real `Err` promptly rather than looping forever or panicking
    /// via `unreachable!()`.
    #[test]
    fn unique_slug_errors_instead_of_hanging_when_exhausted() {
        let tmp = TempDir::new().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(handoff.join("docs")).unwrap();

        let base = "x".repeat(crate::storage::docs::model::MAX_SLUG_LEN);
        let mut used_slugs: std::collections::HashSet<String> = std::collections::HashSet::new();
        used_slugs.insert(base.clone());
        // Pre-claim every disambiguated candidate the truncated-base scheme
        // could produce, forcing exhaustion.
        let max_suffix_len = "-999".len();
        let max_base_len = crate::storage::docs::model::MAX_SLUG_LEN - max_suffix_len;
        let truncated = &base[..max_base_len];
        for n in 2..=999 {
            used_slugs.insert(format!("{truncated}-{n}"));
        }

        let result = unique_slug(&handoff, &mut used_slugs, Some(&base), "Title", "file.md");
        assert!(
            result.is_err(),
            "expected Err on exhaustion, got {result:?}"
        );
    }

    #[test]
    fn extract_markdown_links_finds_pairs() {
        let body = "See [the guide](./guide.md) and [ext](https://example.com).";
        let links = extract_markdown_links(body);
        assert_eq!(links.len(), 2);
        assert_eq!(
            links[0],
            ("the guide".to_string(), "./guide.md".to_string())
        );
        assert_eq!(
            links[1],
            ("ext".to_string(), "https://example.com".to_string())
        );
    }

    #[test]
    fn extract_markdown_links_ignores_unmatched_brackets() {
        let body = "An array literal [1, 2, 3] is not a link.";
        assert!(extract_markdown_links(body).is_empty());
    }

    #[test]
    fn detect_doc_type_keyword_scan() {
        assert_eq!(detect_doc_type("要求仕様書", ""), "spec");
        assert_eq!(detect_doc_type("Design Doc", ""), "design");
        assert_eq!(detect_doc_type("Test Plan", ""), "test-spec");
        assert_eq!(detect_doc_type("ADR-001", ""), "adr");
        assert_eq!(detect_doc_type("Setup Guide", ""), "guide");
        assert_eq!(detect_doc_type("Random notes", ""), "note");
    }

    #[test]
    fn detect_tags_from_frontmatter_and_headings() {
        let fm = "title: Foo\ntags: [alpha, beta]\n";
        let headings = vec!["Session Loop".to_string()];
        let tags = detect_tags(Some(fm), &headings);
        assert!(tags.contains(&"alpha".to_string()));
        assert!(tags.contains(&"beta".to_string()));
    }

    /// `lexsim::tokenize` emits internal cross-language character n-grams
    /// (marker-prefixed, `is_cl_ngram() == true`) alongside real word tokens
    /// — the doc comment on `is_cl_ngram` says they are "useful for matching
    /// but not for human-facing output". `detect_tags` produces a
    /// human-facing `tags` list, so it must filter them out.
    #[test]
    fn detect_tags_excludes_internal_cl_ngram_tokens() {
        let headings = vec!["Real Binary".to_string()];
        let tags = detect_tags(None, &headings);
        assert!(
            tags.iter().all(|t| !lexsim::is_cl_ngram(t)),
            "detect_tags must not leak internal CL-CnG tokens into human-facing tags: {tags:?}"
        );
    }

    #[test]
    fn detect_scope_paths_finds_code_paths() {
        let body = "See `src/mcp/handlers/docs.rs` and also plain text.";
        let paths = detect_scope_paths(body);
        assert!(paths.iter().any(|p| p.contains("src/mcp/handlers/docs.rs")));
    }

    #[test]
    fn detect_scope_paths_ignores_plain_words() {
        let body = "Just some words without any paths here.";
        assert!(detect_scope_paths(body).is_empty());
    }

    #[test]
    fn doc_injected_set_already_injected_tracks_per_fragment() {
        let mut set = DocInjectedSet::new("s".to_string(), "now".to_string());
        set.mark("doc-1", 0, "hashA");
        assert!(set.already_injected("doc-1", 0, "hashA"));
        assert!(!set.already_injected("doc-1", 0, "hashB"));
        assert!(!set.already_injected("doc-1", 1, "hashA"));
    }

    #[test]
    fn doc_injected_set_is_suppressed_tracks_content_hash() {
        let mut set = DocInjectedSet::new("s".to_string(), "now".to_string());
        set.suppress("doc-1", "hashA");
        assert!(set.is_suppressed("doc-1", "hashA"));
        // Content changed since suppression -> no longer suppressed.
        assert!(!set.is_suppressed("doc-1", "hashB"));
        // A different, never-suppressed doc is unaffected.
        assert!(!set.is_suppressed("doc-2", "hashA"));
    }

    #[test]
    fn sanitize_session_id_blocks_traversal() {
        // Path separators are neutralized so the result is always a single
        // flat filename component. The readable prefix may still contain
        // dots (mirrors `storage::memory::injected::sanitize_session_id`),
        // but the mandatory hash suffix guarantees the full stem is never
        // exactly ".." or "." and never starts with a dot (not hidden).
        for evil in ["../../etc/passwd", "a/b\\c", "..", "../", "foo/../bar"] {
            let s = sanitize_session_id(evil);
            assert!(!s.contains('/'), "{evil:?} -> {s:?} still has /");
            assert!(!s.contains('\\'), "{evil:?} -> {s:?} still has \\");
            assert_ne!(s, "..", "{evil:?} -> {s:?} is a parent ref");
            assert!(!s.starts_with('.'), "{evil:?} -> {s:?} is hidden");
        }
    }
}
