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

use super::HandlerContext;
use crate::context::doc_corpus_cache;
use crate::context::injection::{filter_already_injected, rank_with_cached_semantic, RankConfig};
use crate::semantic::semantic_model;
use crate::storage::docs::reassemble::extract_section_trusted;
use crate::storage::docs::split::{compose_doc_hash, compute_sections, split, DEFAULT_SPLIT_LEVEL};
use crate::storage::docs::{
    docs_dir, ensure_docs_dir, read_all_docs, read_all_docs_with_bodies_hashed, read_doc,
    read_doc_body, validate_slug, write_doc, write_doc_body, CodeRef, DocMetadata,
};
use crate::storage::tasks::sync_doc_task_links;
use crate::storage::test_results::{
    match_item_test, parse_cargo_test_jsonl, stable_id_to_test_name_prefix, test_name_module_path,
    TestOutcome,
};

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
///
/// This stays `0.0` even after the t230.5 hybrid (BM25 + semantic) ranking
/// switch: [`rank_with_cached_semantic`] checks this against the
/// lexical+scope base score only (never the semantic bonus), so it continues
/// to mean exactly what it always did — "no floor beyond `score > 0.0`"
/// (see that function's doc comment for why baking the semantic bonus into
/// this check would have broken doc_query's precision).
const DOC_QUERY_MIN_SCORE: f64 = 0.0;

/// Weight applied to the semantic-similarity bonus in
/// [`rank_with_cached_semantic`]: `semantic_weight * (cos + 1.0) / 2.0`, so
/// the max possible bonus (at cos=1.0) equals this constant. Mirrors
/// `memory.rs`'s `SEMANTIC_WEIGHT` (same model, same bonus formula).
const DOC_SEMANTIC_WEIGHT: f64 = 1.0;

/// Independent, lower floor applied to a fragment's semantic bonus **in bonus
/// space** — i.e. against `semantic_weight * (cos + 1.0) / 2.0`
/// (`DOC_SEMANTIC_WEIGHT * (cos + 1.0) / 2.0` at the production weight),
/// **not** against the raw cosine `cos` itself. This lets a lexically-disjoint
/// (BM25=0) cross-lingual fragment surface even though it can never clear
/// [`DOC_QUERY_MIN_SCORE`]'s `> 0.0` lexical/scope gate.
///
/// pub(crate) so `injection.rs`'s unit tests can assert against the real
/// production value instead of a hardcoded stand-in that a future
/// recalibration of this constant could silently drift away from
/// (round-2 rework fix, see below).
///
/// **Calibration history / rework note.** The previous value (`0.72`) was
/// picked by sampling the *raw cosine* range of genuine cross-lingual pairs
/// (~0.77-0.83) and unrelated pairs (~0.50-0.65), then using a number
/// in that gap directly as the filter threshold — but the filter this
/// constant feeds ([`rank_with_cached_semantic`] in `context/injection.rs`)
/// compares against the **bonus** `(cos + 1.0) / 2.0`, not `cos` itself. That
/// unit mismatch meant the real effective cutoff was `cos >= 2*0.72 - 1 =
/// 0.44` — far below the sampled noise ceiling — so unrelated fragments
/// routinely cleared it in practice (found in whole-branch review of t230.5,
/// round 1: unrelated queries like "ログイン画面のボタンの色を変えたい"
/// returned every fragment in a 3-document corpus via `process_line`).
///
/// Recalibrated in bonus space, measured against the **real** end-to-end
/// `doc_texts` shape a live `handoff_doc_query` call actually embeds — not a
/// hand-approximated stand-in. This distinction matters: an earlier
/// recalibration attempt during this same rework approximated a fragment's
/// text as `title + tags + heading + section-body-without-its-own-heading-
/// line`, which reads plausible from this function's own doc comment
/// (`doc.title + doc.tags + section.heading + section.body`) but is *not*
/// what `doc_query`'s `doc_texts` construction (above) actually produces for
/// two reasons the approximation missed: (1) `section.body` (via
/// `extract_section_trusted`) includes the section's own literal Markdown
/// heading line (`"# Restart\n\n..."`), which is then *also* duplicated by
/// the `heading` field placed just before it — so the heading text appears
/// twice; (2) every document additionally produces a **preamble fragment**
/// (`seq` before the first heading) whose `heading` and `body` are both
/// empty, so its `doc_texts` entry is just `"{title}  "` — a real, ranked
/// candidate in its own right, not a hypothetical. Both effects measurably
/// shift the embedding versus the simplified proxy text, which is why this
/// value differs from an earlier draft of this comment (`0.87`, calibrated
/// against the proxy) — that value made
/// `doc_query_cross_lingual_recall_japanese_query_finds_english_section`
/// fail RED against the real handler.
///
/// Measured live (via a temporary debug hook in this function, since removed
/// — reproduce by embedding each `doc_texts` entry against each fixture's
/// query text) across this module's `doc_query_cross_lingual_recall_*` and
/// `doc_query_unrelated_query_returns_no_documents` fixtures:
/// - genuine cross-lingual content fragments (the ones that must surface):
///   bonus 0.857 (JA→EN) and 0.909 (EN→JA)
/// - every other fragment across all three fixtures — the same fixtures'
///   own preamble fragments, the "JavaScript Promises" distractor's preamble
///   and content fragments, and all four fragments against the genuinely
///   unrelated query "ログイン画面のボタンの色を変えたい": bonus 0.784-0.839
///   (highest: the distractor's *preamble* fragment against the JA→EN
///   fixture's query, 0.839 — not its content fragment, underscoring why a
///   naive "genuine content vs distractor content" comparison alone
///   understates the real noise ceiling)
///
/// `0.848` sits in the resulting (0.839, 0.857) gap — under ~0.02 margin on
/// either side, tighter than either of the two prior (proxy-based)
/// calibration attempts assumed. A floor this close to both edges is
/// fragile on its own, so [`rank_with_cached_semantic`]'s
/// `semantic_only_limit` parameter (wired below as
/// [`DOC_SEMANTIC_ONLY_RESCUE_LIMIT`]) adds a second, independent guard that
/// bounds how many fragments per query can surface through this floor
/// alone, rather than relying on the floor's precision in isolation.
pub(crate) const DOC_SEMANTIC_MIN_SCORE: f64 = 0.848;

/// Second precision guard for [`rank_with_cached_semantic`]'s
/// `semantic_only_limit` parameter (round-2 rework fix — see
/// [`DOC_SEMANTIC_MIN_SCORE`]'s doc comment for why a single floor is
/// fragile here): at most this many fragments per `doc_query` call may
/// survive purely on the semantic-bonus floor (i.e. fragments whose
/// lexical+scope base score is `0.0`). A fragment that also clears the
/// lexical/scope gate is never subject to this cap. `2` bounds a single
/// noisy/borderline semantic match to at most a couple of injected
/// fragments per hook call, rather than the unbounded "every fragment in
/// the corpus" failure mode the round-1 review found.
pub(crate) const DOC_SEMANTIC_ONLY_RESCUE_LIMIT: usize = 2;

/// Round-3 rework (MAJOR, whole-branch review of t230.5 round 2): minimum
/// "prose" token count ([`body_prose_token_count`] — a fragment's body with
/// its own ATX heading line(s) stripped out) a fragment must reach to be
/// **eligible** for the semantic-only rescue path at all.
///
/// A single absolute floor on the semantic bonus ([`DOC_SEMANTIC_MIN_SCORE`])
/// cannot separate signal from noise on a realistic corpus: reviewing this
/// branch against a `/tmp` copy of this repository's own `.handoff/docs` (34
/// documents, via the built binary over real stdio JSON-RPC) found that two
/// wholly unrelated queries ("レシピ: カレーの作り方", "今日の夕飯は何にし
/// よう") each returned 2 semantic-only fragments scoring 0.85-0.87 — *above*
/// the fixture-calibrated `DOC_SEMANTIC_MIN_SCORE` (0.848). Every one of
/// those false positives was either a `seq`-0 preamble with an empty body
/// (tokens=0) or a section whose body is just its own heading line with no
/// following prose (tokens 12-18, but all of that count is the heading text
/// itself, `doc_texts` embeds `title + heading + body`, and cosine similarity
/// against a short, near-title-only vector is inherently noisier than against
/// a body with real sentence structure). Neither case carries any real
/// semantic signal about the fragment's *content* — the embedding is
/// dominated by title/heading tokens common across unrelated documents.
///
/// [`handle_doc_query`] zeroes out the semantic embedding for every fragment
/// that fails this eligibility check *before* ranking (rather than passing an
/// eligibility flag through to `rank_with_cached_semantic`), so an ineligible
/// fragment's cosine similarity is always exactly 0 (`0.0 > 0.0` is false),
/// regardless of the query — removing it from the semantic-only rescue path
/// entirely rather than just tightening a shared score floor and risking
/// genuine-but-short cross-lingual matches with it. `5` comfortably excludes
/// both measured noise shapes (0 and, after subtracting heading text,
/// effectively 0 real prose tokens) while sitting well below the genuine
/// cross-lingual fixture's content-only prose (~19 tokens for "Please restart
/// the development server after changing the configuration file.").
const DOC_SEMANTIC_PROSE_MIN_TOKENS: usize = 5;

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
pub fn handle_doc_query(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

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

    // Hashed, paired with each document's body from the same consistent read
    // (t370.9, wiki/240-performance-design.md §6 PR-5): this call tracks
    // injection-suppression by content_hash below (`is_doc_suppressed`/
    // `already_injected`/`mark`/`suppress`), which needs a trustworthy value
    // for every document in the corpus scanned here — the laziness P-M1
    // introduced (wiki/240-performance-design.md §4, t370.8) targets callers
    // (e.g. `DocSet`-based task-link/dev_stage propagation) that never look
    // at content_hash at all, not this one. Reading the body alongside the
    // metadata (rather than a separate `read_doc_body` call per document, as
    // before) also lets the section-extraction loop below use
    // `extract_section_trusted` instead of `extract_section`: the pairing is
    // mutually consistent by construction, so the tokenize-based
    // `content_hash` drift re-verification `extract_section` pays on every
    // section of every call (the dominant cost once `doc_corpus_cache`,
    // t370.2, already made the BM25 corpus-build a cache hit) is provably
    // redundant here.
    let docs = read_all_docs_with_bodies_hashed(handoff)?;
    if docs.is_empty() {
        return Ok(to_json(&json!({ "documents": [], "injected_count": 0 })));
    }

    // Documents suppressed for this call: explicit `suppress_doc_ids`, plus
    // (with a session_id) anything the sidecar remembers as suppressed at
    // its still-current content_hash (spec §7.2.2 "session-scoped temporary
    // suppression, until content_hash changes").
    let now = chrono::Utc::now().to_rfc3339();
    let injected_set = session_id.map(|sid| read_docs_injected_set(handoff, sid, &now));
    let is_doc_suppressed = |doc: &DocMetadata| -> bool {
        if suppress_doc_ids.iter().any(|id| id == &doc.id) {
            return true;
        }
        match &injected_set {
            Some(set) => set.is_suppressed(
                &doc.id,
                doc.content_hash
                    .as_deref()
                    .expect("read_all_docs_with_bodies_hashed always populates content_hash"),
            ),
            None => false,
        }
    };

    let mut candidates: Vec<SectionCandidate> = Vec::new();
    for (doc, body) in &docs {
        if is_doc_suppressed(doc) {
            continue;
        }
        for section in &doc.sections {
            // Best-effort ranking pass over every document: if this one
            // section's recorded byte range is out of bounds for the body
            // (a bug, since `doc`/`body` come from the same read — see
            // `read_all_docs_with_bodies_hashed`'s doc comment), skip just
            // that section rather than failing the whole `doc_query` call
            // for every other unaffected document.
            let Ok(section_body) = extract_section_trusted(body, section) else {
                continue;
            };
            candidates.push(SectionCandidate {
                doc,
                seq: section.seq,
                heading: section.heading.clone(),
                body: section_body.to_string(),
                content_hash: section.content_hash.clone().expect(
                    "read_all_docs_with_bodies_hashed always populates section content_hash",
                ),
            });
        }
    }
    if candidates.is_empty() {
        persist_suppressed_doc_ids(
            handoff,
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
    let model = semantic_model();
    let mut cache = doc_corpus_cache()
        .lock()
        .map_err(|_| anyhow::anyhow!("doc corpus cache mutex poisoned"))?;
    let (corpus, doc_embeddings) = cache.get_or_build_corpus_and_embeddings(&doc_texts, model);

    // Round-3 rework (MAJOR): zero out the semantic embedding for every
    // fragment that fails the `DOC_SEMANTIC_PROSE_MIN_TOKENS` eligibility
    // check (see that constant's doc comment) before ranking. A zeroed
    // embedding always yields `cos <= 0.0`, which `rank_with_cached_semantic`
    // treats as "no semantic bonus" — this removes empty/heading-only
    // fragments from the semantic-only rescue path entirely, rather than
    // relying on `DOC_SEMANTIC_MIN_SCORE` alone to separate them from genuine
    // cross-lingual matches. A plain clone-and-overwrite (not a mutation of
    // the cached `doc_embeddings` itself) so the cache keeps holding each
    // fragment's real embedding for reuse by any future eligible-set change
    // (e.g. the fragment's body growing past the threshold on a later edit).
    let dim = model.dimension();
    let semantic_embeddings: Vec<Vec<f32>> = doc_embeddings
        .iter()
        .zip(candidates.iter())
        .map(|(emb, c)| {
            if body_prose_token_count(&c.body) >= DOC_SEMANTIC_PROSE_MIN_TOKENS {
                emb.clone()
            } else {
                vec![0.0; dim]
            }
        })
        .collect();

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
    let mut ranked = rank_with_cached_semantic(
        corpus,
        &query_tokens,
        &scope_paths,
        &file_paths,
        &semantic_embeddings,
        &text,
        model,
        &rank_config,
        DOC_SEMANTIC_WEIGHT,
        DOC_SEMANTIC_MIN_SCORE,
        DOC_SEMANTIC_ONLY_RESCUE_LIMIT,
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
            let mut set = read_docs_injected_set(handoff, sid, &now);
            set.updated_at = now.clone();
            for item in &fresh {
                let c = &candidates[item.index];
                set.mark(&c.doc.id, c.seq, &c.content_hash);
            }
            write_docs_injected_set(handoff, &set)?;
        }
    }
    persist_suppressed_doc_ids(
        handoff,
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
    docs: &[(DocMetadata, String)],
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
    for (doc, _body) in docs {
        if suppress_doc_ids.iter().any(|id| id == &doc.id) {
            set.suppress(
                &doc.id,
                doc.content_hash
                    .as_deref()
                    .expect("caller resolves docs via read_all_docs_with_bodies_hashed"),
            );
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

/// Count of "prose" tokens in a fragment `body` — i.e. `body` with every ATX
/// heading line (`^#{1,6}\s`, any level, anywhere in the fragment: a fragment
/// can contain nested sub-headings with no prose between them) stripped out,
/// tokenized line-by-line via [`lexsim::estimate_tokens`].
///
/// This is [`DOC_SEMANTIC_PROSE_MIN_TOKENS`]'s eligibility measure: a
/// fragment whose only content is its own heading text (a `seq`-0 preamble
/// that is just the document's `"# Title\n"` line, or a section immediately
/// followed by the next heading with no paragraph in between) has zero real
/// content to be semantically *about* — see that constant's doc comment for
/// the real-corpus evidence this guards against.
fn body_prose_token_count(body: &str) -> usize {
    body.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                None
            } else {
                Some(lexsim::estimate_tokens(trimmed))
            }
        })
        .sum()
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
pub fn handle_doc_analyze(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let project_dir = &ctx.project_dir;
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
    if !scan_root.starts_with(project_dir) {
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
pub fn handle_doc_import(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;
    ensure_docs_dir(handoff)?;

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
    let mut used_slugs: std::collections::HashSet<String> = read_all_docs(handoff)?
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
        let slug = unique_slug(handoff, &mut used_slugs, slug_override, &title, &file)?;

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
        doc.split_level = DEFAULT_SPLIT_LEVEL;

        let body_after_strip: String = split_doc.fragments.iter().map(|f| f.body).collect();
        write_doc_body(handoff, &slug, &body_after_strip)?;
        // false: req_import's own response never reads back per-section
        // content_hash (P-M1, wiki/240-performance-design.md §4, t370.8).
        doc.sections = compute_sections(&split_doc, false);
        // t370.15 (PR-4): `write_doc_with_body` marks every write as carrying
        // a section-composed `content_hash` (`content_hash_scheme`), so the
        // hash persisted here must be composed the same way — a direct
        // `lexsim::content_hash(whole_body)` value under that marker made
        // `handoff_doc_reassemble` report a just-imported, untouched
        // document as `drifted: true`.
        doc.content_hash = Some(compose_doc_hash(&compute_sections(&split_doc, true)));
        doc.source.canonical_hash = doc.content_hash.clone();
        doc.task_ids = task_ids.clone();

        write_doc(handoff, &doc)?;

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
            let parent_slug =
                unique_slug(handoff, &mut used_slugs, None, &parent_title, &parent_title)?;
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
            write_doc(handoff, &parent_doc)?;

            for child_id in &child_doc_ids {
                if let Ok(Some(mut child)) = crate::storage::docs::find_doc_by_id(handoff, child_id)
                {
                    child.parent_id = Some(parent_id.clone());
                    write_doc(handoff, &child)?;
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

/// `dev_stage` fallback used when filtering/sorting/aggregating a `SubItem`
/// that has never had one set (requirements-traceability P0 §3.4 "重要":
/// `dev_stage` が `None` の場合は `"not_started"` としてカウント). Mirrors
/// `docs::UNSET_DEV_STAGE` — kept as a private local constant rather than
/// shared across modules since `docs::aggregate_requirements`'s constant is
/// module-private.
const REQ_LIST_UNSET_DEV_STAGE: &str = "not_started";

/// `handoff_doc_req_status` — cross-document requirements progress summary
/// (requirements-traceability P1 §4.1,
/// `.handoff/docs/_doc.req-traceability-mcp-plan.md`). Filters
/// (`tags`/`priority`/`category`) are applied *before* aggregation, by
/// building a filtered copy of the doc/sub_item tree and feeding it through
/// the same `docs::aggregate_requirements` P0 §3.4 logic
/// `handoff_doc_verify` already uses for `_requirements_summary.json`, so
/// `by_status`/`by_priority`/`by_category`/`coverage` semantics (dev_stage
/// fallback `"not_started"`, priority fallback `"unset"`, category = stable_id
/// prefix) stay identical between the filtered response and the unfiltered
/// cache file.
///
/// The `_requirements_summary.json` side effect (P0 §2.7, §4.1 "副作用: 呼び
/// 出し時に `_requirements_summary.json` を更新") always reflects the full,
/// *unfiltered* aggregate — it is a whole-project cache for the VSCode
/// extension, not a per-call cache of this response.
pub fn handle_doc_req_status(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let tags_filter = string_array(arguments, "tags");
    let priority_filter = arguments.get("priority").and_then(|v| v.as_str());
    let category_filter = arguments.get("category").and_then(|v| v.as_str());

    let all_docs = read_all_docs(handoff)?;

    // Side effect first: the cache file always reflects the unfiltered
    // aggregate across every document, regardless of this call's filters.
    super::docs::write_requirements_summary(handoff, &all_docs)?;

    let filtered_docs: Vec<DocMetadata> = all_docs
        .into_iter()
        .filter(|doc| tags_filter.is_empty() || tags_filter.iter().any(|t| doc.tags.contains(t)))
        .filter_map(|mut doc| {
            let Some(v) = &mut doc.verification else {
                return None;
            };
            for item in &mut v.items {
                item.sub_items.retain(|sub| {
                    if let Some(p) = priority_filter {
                        if sub.priority.as_deref() != Some(p) {
                            return false;
                        }
                    }
                    if let Some(cat) = category_filter {
                        let sub_category = sub
                            .stable_id
                            .as_deref()
                            .and_then(super::docs::category_prefix_from_stable_id);
                        if sub_category != Some(cat) {
                            return false;
                        }
                    }
                    true
                });
            }
            Some(doc)
        })
        .collect();

    let summary = super::docs::aggregate_requirements(&filtered_docs);
    Ok(to_json(&serde_json::to_value(summary)?))
}

/// Default page size for `handoff_doc_req_list` when the caller omits
/// `limit` (P1 §4.2).
const DEFAULT_REQ_LIST_LIMIT: usize = 100;

/// Extracts the `C{n}` category prefix from a `stable_id` (e.g.
/// `"C01-2.1.1.1"` -> `"C01"`), matching `docs::category_prefix_from_stable_id`.
fn req_category_prefix(stable_id: &str) -> Option<&str> {
    stable_id.split('-').next().filter(|s| !s.is_empty())
}

/// Natural-order comparator for `stable_id`s (§4.4, wiki/220 "自然順ソー
/// ト"): walks both strings run-by-run, comparing consecutive digit runs
/// numerically and everything else character-by-character, so
/// `"FR-101"` sorts before `"FR-1001"` — plain `String::cmp` would put
/// `"FR-1001"` first, since byte 5 (`'0'` vs `'1'`) decides it before the
/// rest of the number is ever compared.
pub(crate) fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) => {
                if ca.is_ascii_digit() && cb.is_ascii_digit() {
                    let mut na = String::new();
                    while ai.peek().is_some_and(char::is_ascii_digit) {
                        na.push(ai.next().unwrap());
                    }
                    let mut nb = String::new();
                    while bi.peek().is_some_and(char::is_ascii_digit) {
                        nb.push(bi.next().unwrap());
                    }
                    // Digit-only strings only fail to parse on overflow
                    // (>39 digits) — treated as "very large" rather than
                    // panicking or silently truncating, since requirement
                    // ids never legitimately need numbers that long.
                    let va: u128 = na.parse().unwrap_or(u128::MAX);
                    let vb: u128 = nb.parse().unwrap_or(u128::MAX);
                    match va.cmp(&vb) {
                        Ordering::Equal => match na.len().cmp(&nb.len()) {
                            Ordering::Equal => continue,
                            other => return other,
                        },
                        other => return other,
                    }
                } else {
                    ai.next();
                    bi.next();
                    match ca.cmp(&cb) {
                        Ordering::Equal => continue,
                        other => return other,
                    }
                }
            }
        }
    }
}

/// One flattened `SubItem` (requirement) plus the document/section context
/// it was found in — `handoff_doc_req_list`'s per-item output shape (P1
/// §4.2). `stable_id` is the primary key (`sub_item_index` is included only
/// for back-compat with positional `handoff_doc_verify` addressing).
#[derive(Debug, Clone, Serialize)]
struct RequirementListItem {
    stable_id: String,
    title: String,
    priority: Option<String>,
    dev_stage: Option<String>,
    verification_status: String,
    impl_refs: Vec<CodeRef>,
    test_refs: Vec<CodeRef>,
    doc_id: String,
    doc_slug: String,
    fragment_seq: Option<usize>,
    sub_item_index: usize,
    task_ids: Vec<String>,
}

/// `handoff_doc_req_list` — individual-requirement list across every
/// document's verification matrix, with filter/sort/pagination
/// (requirements-traceability P1 §4.2). Only `SubItem`s without a
/// `stable_id` are skipped (nothing stable to key the item on yet — e.g. a
/// sub_item added before P0's stable_id auto-derivation ran); every other
/// `SubItem` across every doc's `verification.items[].sub_items[]`
/// contributes one item.
pub fn handle_doc_req_list(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let priority_filter = arguments.get("priority").and_then(|v| v.as_str());
    let dev_stage_filter = arguments.get("dev_stage").and_then(|v| v.as_str());
    let category_filter = arguments.get("category").and_then(|v| v.as_str());
    let has_tests_filter = arguments.get("has_tests").and_then(|v| v.as_bool());
    let task_id_filter = arguments.get("task_id").and_then(|v| v.as_str());
    let sort = arguments
        .get("sort")
        .and_then(|v| v.as_str())
        .unwrap_or("stable_id");
    let order = arguments
        .get("order")
        .and_then(|v| v.as_str())
        .unwrap_or("asc");
    let limit = arguments
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(DEFAULT_REQ_LIST_LIMIT);
    let offset = arguments
        .get("offset")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(0);

    let docs = read_all_docs(handoff)?;

    let mut items: Vec<RequirementListItem> = Vec::new();
    for doc in &docs {
        let Some(v) = &doc.verification else {
            continue;
        };
        for verif_item in &v.items {
            for sub in &verif_item.sub_items {
                let Some(stable_id) = sub.stable_id.as_deref() else {
                    continue;
                };

                if let Some(p) = priority_filter {
                    if sub.priority.as_deref() != Some(p) {
                        continue;
                    }
                }
                if let Some(ds) = dev_stage_filter {
                    let actual = sub.dev_stage.as_deref().unwrap_or(REQ_LIST_UNSET_DEV_STAGE);
                    if actual != ds {
                        continue;
                    }
                }
                if let Some(cat) = category_filter {
                    if req_category_prefix(stable_id) != Some(cat) {
                        continue;
                    }
                }
                if let Some(want_tests) = has_tests_filter {
                    let has_tests = !sub.test_refs.is_empty();
                    if has_tests != want_tests {
                        continue;
                    }
                }
                if let Some(tid) = task_id_filter {
                    if !sub.task_ids.iter().any(|t| t == tid) {
                        continue;
                    }
                }

                items.push(RequirementListItem {
                    stable_id: stable_id.to_string(),
                    title: sub.description.clone(),
                    priority: sub.priority.clone(),
                    dev_stage: sub.dev_stage.clone(),
                    verification_status: sub.status.clone(),
                    impl_refs: sub.impl_refs.clone(),
                    test_refs: sub.test_refs.clone(),
                    doc_id: doc.id.clone(),
                    doc_slug: doc.slug.clone(),
                    fragment_seq: verif_item.fragment_seq,
                    sub_item_index: sub.index,
                    task_ids: sub.task_ids.clone(),
                });
            }
        }
    }

    let key_of = |item: &RequirementListItem| -> String {
        match sort {
            "priority" => item.priority.clone().unwrap_or_default(),
            "category" => req_category_prefix(&item.stable_id)
                .unwrap_or("")
                .to_string(),
            "dev_stage" => item
                .dev_stage
                .clone()
                .unwrap_or_else(|| REQ_LIST_UNSET_DEV_STAGE.to_string()),
            _ => item.stable_id.clone(),
        }
    };
    // §4.4 (wiki/220): `stable_id` sorts in natural order (`natural_cmp`),
    // not plain lexicographic `String::cmp` — otherwise `"FR-1001"` sorts
    // before `"FR-101"` (byte 5 is '0' < '1'), which reads as "wrong" to
    // anyone expecting numeric order. Every other `sort` key still ties on
    // `stable_id` too, using the same natural comparator for a stable,
    // human-friendly secondary order.
    items.sort_by(|a, b| {
        let ord = if sort == "stable_id" {
            natural_cmp(&a.stable_id, &b.stable_id)
        } else {
            key_of(a)
                .cmp(&key_of(b))
                .then_with(|| natural_cmp(&a.stable_id, &b.stable_id))
        };
        if order == "desc" {
            ord.reverse()
        } else {
            ord
        }
    });

    let total = items.len();
    let page: Vec<&RequirementListItem> = items.iter().skip(offset).take(limit).collect();

    Ok(to_json(&json!({
        "items": page,
        "total": total,
        "offset": offset,
        "limit": limit,
    })))
}

/// Default section-heading pattern `handoff_doc_req_import` looks for when
/// locating the requirement-tree section (P1 §4.3).
const DEFAULT_REQ_IMPORT_HEADING_PATTERN: &str = "要件ツリー";

/// Default section-heading pattern `handoff_doc_req_import` looks for when
/// locating the gap-analysis table (P1 §4.3).
const DEFAULT_REQ_IMPORT_GAP_TABLE_PATTERN: &str = "ギャップ分析";

/// One Markdown heading line, parsed from a document body.
#[derive(Debug, Clone)]
struct MdHeading {
    /// 1-based line number in the body (for `parse_errors` reporting).
    line: usize,
    /// Number of leading `#` characters.
    level: usize,
    /// Heading text with the leading `#`s and surrounding whitespace
    /// stripped, but any leading section number (e.g. `2.1.1.1`) kept.
    text: String,
}

/// Parses every ATX (`#`...`######`) heading line in `body`. Lines that
/// start with `#` but have no space after the `#` run (e.g. a hashtag in
/// prose) are reported in `parse_errors` and skipped rather than treated as
/// a heading — `derive_stable_id`/description text would otherwise be
/// garbage.
fn parse_markdown_headings(body: &str, parse_errors: &mut Vec<Value>) -> Vec<MdHeading> {
    let mut out = Vec::new();
    for (i, raw_line) in body.lines().enumerate() {
        let line_no = i + 1;
        let trimmed = raw_line.trim_start();
        if !trimmed.starts_with('#') {
            continue;
        }
        let level = trimmed.chars().take_while(|&c| c == '#').count();
        if level == 0 || level > 6 {
            continue;
        }
        let rest = &trimmed[level..];
        if !rest.starts_with(' ') && !rest.is_empty() {
            // e.g. "#tag" — not a heading, just a line starting with '#'.
            parse_errors.push(json!({
                "line": line_no,
                "text": raw_line,
                "reason": "line starts with '#' but has no space after the '#' run; not treated as a heading",
            }));
            continue;
        }
        let text = rest.trim().to_string();
        if text.is_empty() {
            parse_errors.push(json!({
                "line": line_no,
                "text": raw_line,
                "reason": "heading has no text",
            }));
            continue;
        }
        out.push(MdHeading {
            line: line_no,
            level,
            text,
        });
    }
    out
}

/// Slices `headings` down to the sub-tree rooted at a heading whose `text`
/// contains `pattern` (substring match), stopping at the next heading whose
/// level is <= that root heading's level. Returns `None` when no heading
/// matches `pattern`, or (FR-802) every matching heading turns out to have
/// no children at all.
///
/// When multiple headings match `pattern`, candidates are tried shallowest
/// level first (ties broken by document order), and a candidate whose
/// resulting slice would be *empty* is skipped in favor of the next one —
/// aelm's `req-c26-power-electronics.md` has a `##### 5.5 全機能要件ツリー
/// C26 展開` reference heading deep in an appendix section that also
/// happens to contain the substring `"要件ツリー"` (via `全機能要件ツリー`)
/// and has no children of its own; without this fallback it would win the
/// naive "first match" lookup and silently yield zero candidates even
/// though the document's real, populated requirement tree exists earlier
/// (just without the `## 2. 要件ツリー` wrapper heading — see
/// [`find_heading_subsection_by_number`]).
fn find_heading_subsection<'a>(
    headings: &'a [MdHeading],
    pattern: &str,
) -> Option<&'a [MdHeading]> {
    let mut candidate_positions: Vec<usize> = headings
        .iter()
        .enumerate()
        .filter(|(_, h)| h.text.contains(pattern))
        .map(|(i, _)| i)
        .collect();
    candidate_positions.sort_by_key(|&i| (headings[i].level, i));

    for root_pos in candidate_positions {
        let root_level = headings[root_pos].level;
        let end = headings[root_pos + 1..]
            .iter()
            .position(|h| h.level <= root_level)
            .map(|rel| root_pos + 1 + rel)
            .unwrap_or(headings.len());
        let slice = &headings[root_pos + 1..end];
        if !slice.is_empty() {
            return Some(slice);
        }
    }
    None
}

/// FR-802 fallback for `find_heading_subsection`: some aelm `req-c*`
/// documents (observed: `req-c02-design-rules`, `req-c09-drc`,
/// `req-c10-manufacturing-output`, `req-c17-erc`,
/// `req-c26-power-electronics`, `req-c29-collaboration`) number their
/// requirement-tree subsections `### 2.1 ...` / `#### 2.1.1 ...` /
/// `##### 2.1.1.1 ...` but are missing the enclosing `## 2. 要件ツリー`
/// wrapper heading entirely — the document jumps straight from `## 1. 概要`
/// to `## 3. ...`. `find_heading_subsection` finds nothing for these (no
/// heading's *text* contains `"要件ツリー"` anywhere in the document), so
/// `req_import` silently produced zero candidates for six of aelm's 29
/// documents before this fallback existed.
///
/// Tried only when the primary text-pattern lookup fails: finds the first
/// heading whose text starts with `"{number_prefix}."` (e.g. `"2.1 ..."`),
/// and includes it plus everything after it up to the next heading whose
/// level is <= one level *shallower* than that heading's own level — i.e.
/// treats the found heading as if it were itself one level deeper than an
/// (absent) `## {number_prefix}. ...` wrapper, so sibling subsections at
/// the same level (`### 2.2`, `### 2.3`, ...) are still included and only a
/// later top-level heading (`## 3. ...`) ends the slice. Unlike
/// `find_heading_subsection`, the matched heading itself is *included* in
/// the returned slice (there is no separate wrapper heading to exclude).
fn find_heading_subsection_by_number<'a>(
    headings: &'a [MdHeading],
    number_prefix: &str,
) -> Option<&'a [MdHeading]> {
    let needle = format!("{number_prefix}.");
    let start_pos = headings
        .iter()
        .position(|h| h.text.trim_start().starts_with(&needle))?;
    let effective_wrapper_level = headings[start_pos].level.saturating_sub(1);
    let end = headings[start_pos + 1..]
        .iter()
        .position(|h| h.level <= effective_wrapper_level)
        .map(|rel| start_pos + 1 + rel)
        .unwrap_or(headings.len());
    Some(&headings[start_pos..end])
}

/// The requirement-tree section number `find_heading_subsection_by_number`
/// looks for when the default `## 2. 要件ツリー` wrapper heading is missing
/// — matches every aelm document surveyed for wiki/250 (the tree is always
/// numbered "2", even on the six documents missing the wrapper itself).
const DEFAULT_REQ_IMPORT_HEADING_NUMBER_FALLBACK: &str = "2";

/// Converts a 1-based line number (as recorded on [`MdHeading::line`]) to a
/// byte offset within `body`, by summing the byte length of every earlier
/// line including its own line terminator. Used to determine which
/// `doc.sections` entry (`SectionIndex::byte_offset`/`byte_length`, measured
/// against this same frontmatter-stripped body) contains a given heading —
/// `handoff_doc_req_import`'s FR-806 (§4.1) "place items in the section that
/// contains the heading" placement.
fn line_to_byte_offset(body: &str, line: usize) -> usize {
    if line <= 1 {
        return 0;
    }
    let mut offset = 0usize;
    for (i, l) in body.split_inclusive('\n').enumerate() {
        if i + 1 == line {
            return offset;
        }
        offset += l.len();
    }
    offset
}

/// Finds the `doc.sections` entry whose byte range contains the heading at
/// `line` — the "section that includes the heading" `handoff_doc_req_import`
/// attaches newly-imported `SubItem`s to (FR-806 §4.1). Works whether the
/// heading is itself a section-level (`##`, the default split level) heading
/// — in which case it starts that very section's byte range — or nested
/// deeper inside one, since a parent section's byte range spans everything
/// up to the next section-level heading. Falls back to the last section when
/// `line`'s byte offset is past every recorded range (defensive; should not
/// happen for a heading that was actually parsed out of `body`).
fn section_seq_containing_line(
    sections: &[crate::storage::docs::SectionIndex],
    body: &str,
    line: usize,
) -> Option<usize> {
    let byte_offset = line_to_byte_offset(body, line);
    sections
        .iter()
        .find(|s| byte_offset >= s.byte_offset && byte_offset < s.byte_offset + s.byte_length)
        .or_else(|| sections.last())
        .map(|s| s.seq)
}

/// One candidate `SubItem` derived from the requirement-tree heading
/// sub-section, before merging against any existing verification matrix.
#[derive(Debug, Clone)]
struct ReqImportCandidate {
    /// Heading text of the immediate parent heading, used as the "heading"
    /// input to `derive_stable_id` (mirrors `generate`'s section heading).
    parent_heading: String,
    /// The requirement heading's own text (used as `SubItem.description`).
    description: String,
}

/// Extracts the deepest-level headings within `subsection` as `SubItem`
/// candidates (P1 §4.3 "heading level が最深のものを SubItem とする"). The
/// "deepest level" is computed per this subsection, not globally, so a
/// requirement tree that bottoms out at `####` in one branch and `#####` in
/// another still captures both leaves.
///
/// A leaf is any heading with no following heading at a strictly deeper
/// level before the next heading at <= its own level.
fn extract_leaf_candidates(subsection: &[MdHeading]) -> Vec<ReqImportCandidate> {
    let mut out = Vec::new();
    for (i, h) in subsection.iter().enumerate() {
        let has_deeper_child = subsection[i + 1..]
            .iter()
            .take_while(|next| next.level > h.level)
            .any(|next| next.level > h.level);
        if has_deeper_child {
            continue;
        }
        // Nearest ancestor (previous heading with a strictly shallower level).
        let parent_heading = subsection[..i]
            .iter()
            .rev()
            .find(|prev| prev.level < h.level)
            .map(|prev| prev.text.clone())
            .unwrap_or_default();
        out.push(ReqImportCandidate {
            parent_heading,
            description: h.text.clone(),
        });
    }
    out
}

/// One parsed Markdown table's rows, as collected by [`parse_gap_table`]:
/// each entry is `(1-based line number, cell texts)`, header row included.
type GapTableLines = Vec<(usize, Vec<String>)>;

/// Parses a single `|`-delimited Markdown table row into trimmed cell
/// strings. Returns `None` for lines that aren't table rows at all (no
/// `|`), so callers can distinguish "not a table line" from "a row with
/// unexpected column count" (the latter is still returned — column-count
/// mismatches are handled by the caller, not silently dropped here).
fn parse_table_row(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if !trimmed.contains('|') {
        return None;
    }
    let inner = trimmed.trim_start_matches('|').trim_end_matches('|');
    Some(inner.split('|').map(|c| c.trim().to_string()).collect())
}

/// A row's `separator` line (`|---|---|` or `| :--- | ---: |`) — every cell
/// consists only of `-`, `:`, and whitespace.
fn is_table_separator_row(cells: &[String]) -> bool {
    !cells.is_empty()
        && cells
            .iter()
            .all(|c| !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':'))
}

/// One row of the gap-analysis table: its ID column (if a column matching
/// one of the FR-802 header synonyms was found), its name/description
/// column (used for fuzzy-matching against a requirement's description),
/// its extracted priority, and its extracted `dev_stage` (FR-805).
#[derive(Debug, Clone)]
struct GapTableRow {
    /// Value of the ID column (`要件ID` / `ID` / `REQ ID` / ... — see
    /// [`detect_gap_table_columns`]), Markdown bold-stripped and trimmed.
    /// `None` when the table has no recognizable ID column, or this row's
    /// cell in it is empty.
    id: Option<String>,
    /// Value of the name/description column (`要件名` / `要件` / `機能` /
    /// ... — see [`detect_gap_table_columns`]), used to fuzzy-match this
    /// row against a candidate's heading-derived description.
    name: String,
    priority: Option<String>,
    /// `dev_stage` derived from either a dedicated implementation-status
    /// column (`実装状態` / `現状` / ...) or, when no such column exists,
    /// from the remainder of the priority cell after its `P0`..`P3` token
    /// is removed (FR-805 "優先度欄が状態を兼ねる" — e.g. `"P0=出荷済"`).
    dev_stage: Option<String>,
}

/// Recognized priority tokens (P1 §4.3 "P0/P1/P2/P3 を抽出").
const PRIORITY_TOKENS: [&str; 4] = ["P0", "P1", "P2", "P3"];

/// Strips Markdown bold markers (`**`) from `s` for comparison purposes —
/// aelm's gap tables sometimes bold an entire ID/name cell (e.g.
/// `"**PRE-2.2.2**"`, `"**FULL**"`) to call out an exceptional row (FR-802).
/// Only used internally for ID/name matching; the original heading text is
/// still what ends up in a `SubItem.description`.
fn strip_md_bold(s: &str) -> String {
    s.replace("**", "").trim().to_string()
}

/// Column indices resolved from a gap-table header row (FR-802 "表ヘッダー
/// の揺れ（列マッピング指定）に対応する").
#[derive(Debug, Clone, Copy, Default)]
struct GapTableColumns {
    id: Option<usize>,
    name: Option<usize>,
    priority: Option<usize>,
    /// Implementation-status column (`実装状態` / `現状` / `ステータス` /
    /// `status`), distinct from the priority column (FR-805).
    status: Option<usize>,
}

/// One `column_map` entry (review-rework round 2 MAJOR, FR-802 "表ヘッダー
/// の揺れ（列マッピング指定）に対応する" — the built-in synonym list in
/// [`detect_gap_table_columns`] cannot cover every external document's
/// wording, e.g. `重要度`/`Pri.` for priority or `項目` for name). A caller
/// supplies either the header's literal text, or the column's 0-based
/// index directly.
#[derive(Debug, Clone)]
enum ColumnRef {
    Index(usize),
    Header(String),
}

/// Parsed `column_map` argument — each field is independently optional.
/// When a field is given, it overrides [`detect_gap_table_columns`]'s
/// synonym-based auto-detection for that field only (a `column_map` that
/// only sets `priority` still gets auto-detected `id`/`name`/`status`).
#[derive(Debug, Clone, Default)]
struct ColumnMapOverride {
    id: Option<ColumnRef>,
    name: Option<ColumnRef>,
    priority: Option<ColumnRef>,
    status: Option<ColumnRef>,
}

fn parse_column_ref(v: &Value) -> Option<ColumnRef> {
    match v {
        Value::String(s) => Some(ColumnRef::Header(s.clone())),
        Value::Number(n) => n.as_u64().map(|i| ColumnRef::Index(i as usize)),
        _ => None,
    }
}

/// Parses the `column_map` tool argument (an object with optional
/// `id`/`name`/`priority`/`status` keys, each either a header string or a
/// 0-based column-index number). Absent or malformed => every field is
/// `None`, i.e. no override (falls through entirely to auto-detection).
fn parse_column_map(arguments: &Value) -> ColumnMapOverride {
    let Some(obj) = arguments.get("column_map").and_then(|v| v.as_object()) else {
        return ColumnMapOverride::default();
    };
    ColumnMapOverride {
        id: obj.get("id").and_then(parse_column_ref),
        name: obj.get("name").and_then(parse_column_ref),
        priority: obj.get("priority").and_then(parse_column_ref),
        status: obj.get("status").and_then(parse_column_ref),
    }
}

/// Resolves one `column_map` override entry against an actual header row:
/// an index is used as-is (when in bounds), a header string is matched
/// against each header cell (bold-stripped, trimmed, case-insensitive) —
/// exact match first, then substring containment (covers a header cell
/// that carries extra decoration around the mapped text).
fn resolve_column_ref(r: &ColumnRef, header: &[String]) -> Option<usize> {
    match r {
        ColumnRef::Index(i) => (*i < header.len()).then_some(*i),
        ColumnRef::Header(h) => {
            let target = strip_md_bold(h).trim().to_lowercase();
            if target.is_empty() {
                return None;
            }
            header
                .iter()
                .position(|c| strip_md_bold(c).trim().to_lowercase() == target)
                .or_else(|| {
                    header
                        .iter()
                        .position(|c| strip_md_bold(c).trim().to_lowercase().contains(&target))
                })
        }
    }
}

/// Maps a gap table's header cells to the (id, name, priority, status)
/// column indices it recognizes, tolerating the header-text synonyms
/// observed across aelm's `req-c*` documents (`要件ID` / `ID` / `REQ ID`,
/// `要件名` / `要件` / `機能`, `優先度` / `Priority`, `実装状態` / `現状` /
/// `ステータス` / `status`). Falls back to "the first column that is
/// neither the id nor the priority column" for `name` when no header cell
/// matches a known synonym — this preserves the pre-FR-802 behavior for
/// tables that only have a name + priority column (no explicit ID column),
/// e.g. this module's own `REQ_TREE_BODY` test fixture's `| 要件 | 優先度 |
/// 備考 |` table.
///
/// `column_map` (review-rework round 2 MAJOR) is resolved first, per field
/// — a field's override wins when it resolves to a real column in *this*
/// table's header; otherwise (override absent, or its header text/index
/// doesn't resolve against this particular table) that field falls through
/// to the synonym-based auto-detection below, so one `column_map` can still
/// usefully apply across a document with multiple differently-shaped
/// tables.
fn detect_gap_table_columns(header: &[String], column_map: &ColumnMapOverride) -> GapTableColumns {
    let norm: Vec<String> = header.iter().map(|c| strip_md_bold(c)).collect();

    let priority = column_map
        .priority
        .as_ref()
        .and_then(|r| resolve_column_ref(r, header))
        .or_else(|| {
            norm.iter().position(|c| {
                let lc = c.to_lowercase();
                lc.contains("優先度") || lc.contains("priority")
            })
        });
    let id = column_map
        .id
        .as_ref()
        .and_then(|r| resolve_column_ref(r, header))
        .or_else(|| norm.iter().position(|c| c.to_lowercase().contains("id")));
    let name = column_map
        .name
        .as_ref()
        .and_then(|r| resolve_column_ref(r, header))
        .or_else(|| norm.iter().position(|c| c.contains("要件名")))
        .or_else(|| {
            norm.iter().enumerate().find_map(|(i, c)| {
                let is_id = Some(i) == id;
                let is_priority = Some(i) == priority;
                (!is_id && !is_priority && (c.contains("要件") || c.contains("機能"))).then_some(i)
            })
        })
        .or_else(|| {
            norm.iter()
                .enumerate()
                .find_map(|(i, _)| (Some(i) != id && Some(i) != priority).then_some(i))
        });
    let status = column_map
        .status
        .as_ref()
        .and_then(|r| resolve_column_ref(r, header))
        .or_else(|| norm.iter().position(|c| c.contains("実装状態")))
        .or_else(|| norm.iter().position(|c| c.contains("現状")))
        .or_else(|| norm.iter().position(|c| c.contains("ステータス")))
        .or_else(|| {
            norm.iter()
                .position(|c| c.to_lowercase().contains("status"))
        });

    GapTableColumns {
        id,
        name,
        priority,
        status,
    }
}

/// Maps aelm's varied "実装状態"/"現状" free text to a `SubItem.dev_stage`
/// value (FR-805). Checked in this specific order because e.g. `"実装済み
/// (verified: ...)"` must resolve to `"verified"`, not `"implemented"` (a
/// plain `.contains("実装済")` check would fire first if it ran before the
/// `"verified"` check). Returns `None` for free text this table doesn't
/// recognize (e.g. `"FULL"`/`"PARTIAL"` cost-estimate columns, or narrative
/// text) — callers leave `dev_stage` unset rather than guess.
fn map_impl_status_to_dev_stage(text: &str) -> Option<&'static str> {
    let t = strip_md_bold(text);
    if t.is_empty() {
        return None;
    }
    if t.contains("verified") || t.contains("検証済") {
        Some("verified")
    } else if t.contains("部分実装") {
        Some("in_progress")
    } else if t.contains("実装済") || t.contains("出荷済") {
        Some("implemented")
    } else if t.contains("未実装")
        || t.contains("未定義")
        || t.contains("GAP-GREENFIELD")
        || t.contains("GAP-DESIGNED")
        || t.contains('❌')
        || t.contains("対象外")
        || t.contains("不要")
    {
        // "対象外"/"不要" ("out of scope"/"not needed") has no dedicated
        // dev_stage value in the not_started/in_progress/implemented/
        // tested/verified enum — mapped to "not_started" (no work has
        // been done on it) rather than left unset, matching the same
        // "no gap" semantics as an unimplemented item.
        Some("not_started")
    } else {
        None
    }
}

/// Removes the first case-insensitive occurrence of `token` from `cell` and
/// trims common separator punctuation left behind (FR-805 "P0=出荷済" style
/// merged priority/status cells), returning the remainder for
/// [`map_impl_status_to_dev_stage`]. Returns an empty string when `token`
/// isn't found (should not happen — callers only pass a `token` they just
/// matched in `cell`).
fn strip_priority_token(cell: &str, token: &str) -> String {
    let upper = cell.to_uppercase();
    let Some(pos) = upper.find(token) else {
        return String::new();
    };
    let mut remainder = String::with_capacity(cell.len());
    remainder.push_str(&cell[..pos]);
    remainder.push_str(&cell[pos + token.len()..]);
    remainder
        .trim_matches(|c: char| c.is_whitespace() || matches!(c, '=' | '(' | ')' | ':' | '：'))
        .to_string()
}

/// Finds the gap-analysis section (first heading containing `pattern`) and
/// parses every Markdown table within it that has a recognizable priority
/// column (FR-802: earlier, unrelated tables — e.g. a "優先度凡例" legend or
/// a dependency list — are skipped in favor of the actual per-item gap
/// table(s); a "## 4." section is frequently split into several `### 4.N`
/// subsections, each with its own qualifying table — e.g. aelm's
/// `req-c02-design-rules.md` has 6 — and every one of them contributes
/// rows, not just the first). Rows with fewer cells than their own table's
/// header, or with no recognizable `P0`..`P3` token in the priority column,
/// are still recorded (id/name/dev_stage may still be usable) but reported
/// via `parse_errors`. Returns an empty `Vec` when no gap-analysis heading,
/// or no table with a priority column, is found — this is not itself an
/// error (priority_source may still be "manual"/"none", or the doc may
/// simply lack a per-item gap table, e.g. a narrative-only "## 4."
/// section).
fn parse_gap_table(
    body: &str,
    headings: &[MdHeading],
    pattern: &str,
    parse_errors: &mut Vec<Value>,
    column_map: &ColumnMapOverride,
) -> Vec<GapTableRow> {
    let Some(root_pos) = headings.iter().position(|h| h.text.contains(pattern)) else {
        return Vec::new();
    };
    let root_level = headings[root_pos].level;
    let section_end_line = headings[root_pos + 1..]
        .iter()
        .find(|h| h.level <= root_level)
        .map(|h| h.line)
        .unwrap_or(usize::MAX);
    let section_start_line = headings[root_pos].line;

    let lines: Vec<&str> = body.lines().collect();
    let mut tables: Vec<GapTableLines> = Vec::new();
    let mut current: GapTableLines = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let line_no = i + 1;
        if line_no <= section_start_line || line_no >= section_end_line {
            continue;
        }
        if let Some(cells) = parse_table_row(line) {
            current.push((line_no, cells));
        } else if !current.is_empty() {
            tables.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tables.push(current);
    }

    // FR-802: scan every table in the section and keep every one whose
    // header has a recognizable priority column *and* at least 3 columns —
    // each is "a" gap table, not just the first one found. The
    // column-count check matters because aelm's `## 4.` sections routinely
    // open with a 2-column "優先度凡例" (`優先度 | 定義`) or status legend
    // (`状態 | 意味`) table *before* the real per-item table(s) — those
    // legend tables also have a header cell literally named "優先度" (so
    // `columns.priority` alone can't tell them apart from a real gap
    // table), but every real per-item gap table observed across aelm's 29
    // documents has >= 3 columns (id/name + priority + status/notes/...),
    // while every legend table observed has exactly 2. A single "## 4."
    // section is frequently split into several `### 4.N` subsections, each
    // with its own qualifying table (e.g. aelm's `req-c02-design-rules.md`
    // has 6) — every one of them is kept and contributes rows below.
    let qualifying: Vec<(&GapTableLines, GapTableColumns)> = tables
        .iter()
        .filter_map(|table| {
            let (_, header) = table.first()?;
            let columns = detect_gap_table_columns(header, column_map);
            (columns.priority.is_some() && header.len() >= 3).then_some((table, columns))
        })
        .collect();

    if qualifying.is_empty() {
        if !tables.is_empty() {
            parse_errors.push(json!({
                "line": tables[0].first().map(|(l, _)| *l).unwrap_or(section_start_line),
                "text": tables[0].first().map(|(_, c)| c.join(" | ")).unwrap_or_default(),
                "reason": "no table in the gap-analysis section has a recognizable '優先度'/'Priority' column",
            }));
        }
        return Vec::new();
    }

    let mut rows = Vec::new();
    for (table_lines, columns) in &qualifying {
        let (_, header) = &table_lines[0];
        let priority_col = columns
            .priority
            .expect("qualifying table has a priority column");

        for (line_no, cells) in table_lines.iter().skip(1) {
            if is_table_separator_row(cells) {
                continue;
            }
            if cells.len() != header.len() || cells.len() <= priority_col {
                parse_errors.push(json!({
                    "line": line_no,
                    "text": cells.join(" | "),
                    "reason": "table row has a different column count than the header",
                }));
                continue;
            }
            let priority_cell = &cells[priority_col];
            let priority_cell_upper = priority_cell.to_uppercase();
            let priority = PRIORITY_TOKENS
                .iter()
                .find(|tok| priority_cell_upper.contains(*tok))
                .map(|tok| tok.to_string());
            if priority.is_none() {
                parse_errors.push(json!({
                    "line": line_no,
                    "text": cells.join(" | "),
                    "reason": "no P0/P1/P2/P3 token found in the priority column",
                }));
            }

            let id = columns
                .id
                .and_then(|i| cells.get(i))
                .map(|c| strip_md_bold(c))
                .filter(|s| !s.is_empty());
            // Falls back to the first non-priority/non-id cell when no
            // header synonym matched (`detect_gap_table_columns`'s own
            // fallback already picks *some* index for `name` in that
            // case) — preserves the pre-FR-802 "name-only" table behavior.
            let name = columns
                .name
                .and_then(|i| cells.get(i))
                .map(|c| strip_md_bold(c))
                .unwrap_or_default();

            // FR-805: dev_stage from a dedicated status column when
            // present; otherwise, when priority/status share one cell
            // (e.g. "P0=出荷済"), from whatever text remains after removing
            // the matched P-token.
            let dev_stage = if let Some(sc) = columns.status {
                cells.get(sc).and_then(|c| map_impl_status_to_dev_stage(c))
            } else if let Some(tok) = &priority {
                let remainder = strip_priority_token(priority_cell, tok);
                if remainder.is_empty() {
                    None
                } else {
                    map_impl_status_to_dev_stage(&remainder)
                }
            } else {
                None
            }
            .map(|s| s.to_string());

            rows.push(GapTableRow {
                id,
                name,
                priority,
                dev_stage,
            });
        }
    }
    rows
}

/// Extracts a leading ID-like token from `text` for gap-table row ID exact
/// matching (FR-802): the first whitespace-delimited word, when it contains
/// at least one ASCII digit and consists only of ASCII alphanumerics plus
/// `.`/`-`/`_`. Covers aelm's varied ID shapes (`2.1.1.1`, `C02-G01`,
/// `REQ-C19.1.1.1`, `GAP-3DV-001`) without hardcoding a prefix allow-list
/// the way `docs::extract_requirement_id` does — that function is
/// intentionally narrow (`FR`/`NFR`/...) to avoid minting false-positive
/// `stable_id`s from ordinary prose. This function's result is only ever
/// compared for *exact* equality against an explicit gap-table ID-column
/// cell, so a broader heuristic here cannot itself mis-fire: it just fails
/// to find a matching row, falling through to fuzzy name matching.
fn extract_leading_id_token(text: &str) -> Option<String> {
    let first = text.split_whitespace().next()?;
    let looks_like_id = first.chars().any(|c| c.is_ascii_digit())
        && first
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    looks_like_id.then(|| first.trim_end_matches('.').to_string())
}

/// Looks up a gap-table row for a candidate `description` (§4.4, wiki/220
/// "ギャップ表照合: ID 完全一致を最優先"; FR-802 broadens the ID match beyond
/// `docs::extract_requirement_id`'s FR/NFR-only allow-list). A row whose ID
/// column exactly equals [`extract_leading_id_token`]'s extraction from
/// `description` wins first; only when no row's ID matches does this fall
/// back to `docs::descriptions_fuzzy_match` against the row's *name* column
/// specifically (FR-802 — matching against the ID column instead, as a
/// pre-FR-802 table with both `要件ID` and `要件名` columns would have done,
/// broke fuzzy matching whenever a doc's gap-table IDs use a different
/// numbering scheme than its requirement-tree heading numbers).
fn match_gap_table_row<'a>(rows: &'a [GapTableRow], description: &str) -> Option<&'a GapTableRow> {
    if let Some(desc_id) = extract_leading_id_token(description) {
        // A table with no dedicated ID column (only name + priority, e.g.
        // this module's `ID_COLLISION_BODY`/`REQ_TREE_BODY` test fixtures)
        // still puts an ID-shaped token directly in the name cell (`"FR-001"`,
        // not a longer sentence) — extract it the same way for an exact
        // token-vs-token comparison. This is deliberately *not* a substring
        // check: `descriptions_fuzzy_match`'s containment rule would
        // otherwise mis-fire on prefix collisions like `"FR-001"` being a
        // literal substring of `"NFR-001"`.
        if let Some(row) = rows.iter().find(|r| {
            let row_id = r.id.clone().or_else(|| extract_leading_id_token(&r.name));
            row_id
                .as_deref()
                .is_some_and(|id| id.eq_ignore_ascii_case(&desc_id))
        }) {
            return Some(row);
        }
    }
    rows.iter()
        .find(|r| super::docs::descriptions_fuzzy_match(&r.name, description))
}

/// `handoff_doc_req_import` — bulk-generates `SubItem`s (with `stable_id`
/// and, optionally, `priority`) from a document's Markdown requirement-tree
/// heading hierarchy, merging against any existing verification matrix
/// (requirements-traceability P1 §4.3-4.4,
/// `.handoff/docs/_doc.req-traceability-mcp-plan.md`).
///
/// Merge rules (§4.4): `stable_id` match -> update (priority/description
/// only, `dev_stage`/refs preserved); description fuzzy match (≥ substring
/// containment, reusing `docs::descriptions_fuzzy_match`) -> re-link
/// existing `stable_id`; no match -> create; existing `SubItem` not present
/// in the import source -> left untouched, reported as an `orphan` warning
/// (never deleted).
///
/// `dry_run` (default `true`) returns a preview without writing. When
/// `false`, the document's verification matrix is updated in place (a
/// `category="requirement"` `VerificationItem` with `fragment_seq: None` is
/// created on first import if the document has no verification matrix yet)
/// and `docs::write_requirements_summary` refreshes the VSCode-extension
/// cache file.
pub fn handle_doc_req_import(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let doc_id = arguments
        .get("doc_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'doc_id' is required"))?;
    let dry_run = arguments
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let priority_source = arguments
        .get("priority_source")
        .and_then(|v| v.as_str())
        .unwrap_or("gap_table");
    let heading_pattern = arguments
        .get("heading_pattern")
        .and_then(|v| v.as_str())
        .unwrap_or(DEFAULT_REQ_IMPORT_HEADING_PATTERN);
    let gap_table_pattern = arguments
        .get("gap_table_pattern")
        .and_then(|v| v.as_str())
        .unwrap_or(DEFAULT_REQ_IMPORT_GAP_TABLE_PATTERN);
    // Review-rework round 2 MAJOR (FR-802): explicit column mapping,
    // overriding `detect_gap_table_columns`'s built-in header synonyms per
    // field when given.
    let column_map = parse_column_map(arguments);

    let mut doc = crate::storage::docs::find_doc_by_id(handoff, doc_id)?
        .or(read_doc(handoff, doc_id)?)
        .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;

    // wiki/220-vmodel-integration-design.md §2.3 write guard: a layer
    // document's SubItems are defined by the Markdown body (parsed by
    // `sync_layer_items`, wired into `doc_save`/`doc_update_section`), not
    // by `req_import`'s gap-table/heading-driven extraction — refuse rather
    // than create SubItems `sync_layer_items` would then have no record of
    // (and would treat as `origin=None` legacy items on the next sync).
    if doc.layer.is_some() {
        anyhow::bail!(super::docs::LAYER_BODY_EDIT_GUARD_MSG);
    }
    let body = read_doc_body(handoff, &doc.slug)?.unwrap_or_default();

    let mut parse_errors: Vec<Value> = Vec::new();
    let headings = parse_markdown_headings(&body, &mut parse_errors);

    // FR-802: fall back to `find_heading_subsection_by_number` when no
    // heading's text contains `heading_pattern` at all — some aelm
    // documents number their requirement-tree subsections without the
    // enclosing `## 2. 要件ツリー` wrapper heading (see that function's doc
    // comment for the observed documents).
    let mut used_number_fallback = false;
    let subsection = find_heading_subsection(&headings, heading_pattern).or_else(|| {
        used_number_fallback = true;
        find_heading_subsection_by_number(&headings, DEFAULT_REQ_IMPORT_HEADING_NUMBER_FALLBACK)
    });
    let Some(subsection) = subsection else {
        return Ok(to_json(&json!({
            "doc_id": doc.id,
            "would_create": 0,
            "would_update": 0,
            "would_skip": 0,
            "parse_errors": parse_errors,
            "preview": [],
            "warnings": [format!(
                "no heading containing {heading_pattern:?} found; nothing to import"
            )],
        })));
    };

    if used_number_fallback {
        parse_errors.push(json!({
            "line": subsection.first().map(|h| h.line).unwrap_or(0),
            "text": subsection.first().map(|h| h.text.clone()).unwrap_or_default(),
            "reason": format!(
                "no heading containing {heading_pattern:?} found; used numeric-prefix \
                 fallback ({DEFAULT_REQ_IMPORT_HEADING_NUMBER_FALLBACK:?}) because this \
                 document's requirement-tree subsections have no enclosing wrapper heading"
            ),
        }));
    }

    let candidates = extract_leaf_candidates(subsection);

    let gap_rows = if priority_source == "gap_table" {
        parse_gap_table(
            &body,
            &headings,
            gap_table_pattern,
            &mut parse_errors,
            &column_map,
        )
    } else {
        Vec::new()
    };

    // Existing sub_items across the whole matrix, for merge decisions +
    // stable_id collision detection.
    let mut existing_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(v) = &doc.verification {
        existing_ids = super::docs::collect_stable_ids(v);
    }
    let mut matched_existing_stable_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    // M0-b (wiki/220-vmodel-integration-design.md §4.2, FR-105): a single
    // whole-corpus read up front, reused for every "create" candidate below
    // — not one `read_all_docs` per candidate — to warn (never refuse) when
    // a freshly-minted `stable_id` collides with one already assigned in a
    // *different* document. `existing_ids` above only guards against
    // collisions within this document.
    let cross_doc_stable_ids = super::docs::collect_all_stable_ids(&read_all_docs(handoff)?);

    #[derive(Debug, Clone, Serialize)]
    struct PreviewEntry {
        stable_id: String,
        title: String,
        priority: Option<String>,
        /// FR-805: the `dev_stage` this import would apply. `None` here
        /// means "no change" — either the gap table had no recognizable
        /// implementation-status text, or (on `update`/`match`) the
        /// existing `SubItem` already has a `dev_stage` set and the merge
        /// rule (§ below) leaves it untouched rather than overwriting
        /// manually-tracked progress with a possibly-stale document import.
        #[serde(skip_serializing_if = "Option::is_none")]
        dev_stage: Option<String>,
        action: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        warning: Option<String>,
    }

    let mut preview: Vec<PreviewEntry> = Vec::new();
    let mut would_create = 0usize;
    let mut would_update = 0usize;
    let would_skip = 0usize;

    for cand in &candidates {
        let matched_row = if priority_source == "gap_table" {
            match_gap_table_row(&gap_rows, &cand.description)
        } else {
            None
        };
        let priority = matched_row.and_then(|r| r.priority.clone());
        let dev_stage = matched_row.and_then(|r| r.dev_stage.clone());

        // 1. stable_id match: does any existing sub_item's own derived id
        //    coincide? We derive the "natural" id the same way `generate`
        //    would, then check whether that id already exists.
        let (derived_id, derive_warning) = super::docs::derive_stable_id(
            &doc.slug,
            &cand.parent_heading,
            &cand.description,
            &existing_ids,
        );

        let existing_sub = doc.verification.as_ref().and_then(|v| {
            v.items
                .iter()
                .flat_map(|i| i.sub_items.iter())
                .find(|s| s.stable_id.as_deref() == Some(derived_id.as_str()))
        });

        if let Some(existing) = existing_sub {
            // stable_id already present verbatim -> update.
            matched_existing_stable_ids.insert(derived_id.clone());
            would_update += 1;
            preview.push(PreviewEntry {
                stable_id: derived_id.clone(),
                title: cand.description.clone(),
                priority: priority.clone().or_else(|| existing.priority.clone()),
                // FR-805 merge rule: fill only when the existing SubItem has
                // no dev_stage yet — never downgrade manually-tracked
                // progress (e.g. "verified") back to whatever this
                // (possibly stale) document import's gap table currently
                // says.
                dev_stage: existing
                    .dev_stage
                    .is_none()
                    .then(|| dev_stage.clone())
                    .flatten(),
                action: "update".to_string(),
                warning: None,
            });
            continue;
        }

        // 2. description fuzzy match against any existing sub_item -> that
        //    sub_item is re-linked (P1 §4.4): if it already has a
        //    stable_id, reuse it; if not (it predates stable_id
        //    assignment), it gets newly assigned the id we just derived.
        let fuzzy_match = doc.verification.as_ref().and_then(|v| {
            v.items
                .iter()
                .flat_map(|i| i.sub_items.iter())
                .find(|s| super::docs::descriptions_fuzzy_match(&s.description, &cand.description))
        });

        if let Some(existing) = fuzzy_match {
            let matched_id = existing
                .stable_id
                .clone()
                .unwrap_or_else(|| derived_id.clone());
            matched_existing_stable_ids.insert(matched_id.clone());
            existing_ids.insert(matched_id.clone());
            would_update += 1;
            preview.push(PreviewEntry {
                stable_id: matched_id,
                title: cand.description.clone(),
                priority,
                // Same FR-805 merge rule as the stable_id-match branch above.
                dev_stage: existing
                    .dev_stage
                    .is_none()
                    .then(|| dev_stage.clone())
                    .flatten(),
                action: "match".to_string(),
                warning: Some(
                    "matched an existing sub_item by description; verify before trusting"
                        .to_string(),
                ),
            });
            continue;
        }

        // 3. no match -> create.
        existing_ids.insert(derived_id.clone());
        matched_existing_stable_ids.insert(derived_id.clone());
        would_create += 1;
        let mut warning = derive_warning;
        if priority.is_none() && priority_source == "gap_table" {
            let gap_warning = "gap_table に対応エントリなし — priority 未設定".to_string();
            warning = Some(match warning {
                Some(w) => format!("{w}; {gap_warning}"),
                None => gap_warning,
            });
        }
        // M0-b (wiki/220 §4.2, FR-105): warn (never refuse) when this
        // freshly-derived id already belongs to a SubItem in a different
        // document.
        if let Some(owners) = cross_doc_stable_ids.get(&derived_id) {
            let other_owners: Vec<&str> = owners
                .iter()
                .map(String::as_str)
                .filter(|id| *id != doc.id)
                .collect();
            if !other_owners.is_empty() {
                let cross_doc_warning = format!(
                    "stable_id {derived_id:?} already exists in other document(s): {} — \
                     created anyway, but it will be reported as ambiguous by resolve_stable_ids \
                     and not linkable until resolved",
                    other_owners.join(", ")
                );
                warning = Some(match warning {
                    Some(w) => format!("{w}; {cross_doc_warning}"),
                    None => cross_doc_warning,
                });
            }
        }
        preview.push(PreviewEntry {
            stable_id: derived_id,
            title: cand.description.clone(),
            priority,
            dev_stage,
            action: "create".to_string(),
            warning,
        });
    }

    // Orphans: existing sub_items with a stable_id that wasn't touched by
    // this import pass. Never deleted — reported only.
    let mut orphan_warnings: Vec<String> = Vec::new();
    if let Some(v) = &doc.verification {
        for sub in v.items.iter().flat_map(|i| i.sub_items.iter()) {
            let Some(id) = &sub.stable_id else { continue };
            if !matched_existing_stable_ids.contains(id) {
                orphan_warnings.push(format!(
                    "existing sub_item {id:?} ({:?}) not present in import source; left untouched",
                    sub.description
                ));
            }
        }
    }

    if dry_run {
        let mut out = json!({
            "doc_id": doc.id,
            "would_create": would_create,
            "would_update": would_update,
            "would_skip": would_skip,
            "parse_errors": parse_errors,
            "preview": preview,
        });
        if !orphan_warnings.is_empty() {
            out["warnings"] = json!(orphan_warnings);
        }
        return Ok(to_json(&out));
    }

    // Apply: write creates/updates into the verification matrix.
    let now = chrono::Utc::now().to_rfc3339();
    if doc.verification.is_none() {
        // FR-806 (§4.1): auto-generate the *full* section-based matrix
        // (mirrors `handoff_doc_verify(action="generate")`) instead of
        // bootstrapping a single freeform bucket. Before this fix, every
        // SubItem created by a first-time import landed in a
        // `fragment_seq: None` item and was therefore unresolvable by
        // `resolve_stable_ids` (see wiki/220 §4.1's repro) — placing new
        // SubItems in a real section from the start keeps them addressable
        // by stable_id immediately (`handoff_update_task(requirement_ids)`
        // etc.).
        let items: Vec<crate::storage::docs::VerificationItem> = doc
            .sections
            .iter()
            .map(|s| crate::storage::docs::VerificationItem {
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
            })
            .collect();
        doc.verification = Some(crate::storage::docs::Verification {
            status: super::docs::recompute_verification_status(&items),
            created_at: now.clone(),
            updated_at: now.clone(),
            items,
        });
    }

    // FR-806 (§4.1 "見出しを含むセクションの item に配置する"): new SubItems
    // attach to the section whose byte range contains the matched
    // `heading_pattern` heading — found by converting that heading's line
    // number to a byte offset and locating the `doc.sections` entry
    // covering it (works whether the heading is itself a section-level
    // (`##`) heading or nested deeper inside one).
    let target_index = headings
        .iter()
        .find(|h| h.text.contains(heading_pattern))
        .and_then(|h| section_seq_containing_line(&doc.sections, &body, h.line));

    // review-rework round 2 MAJOR: only resolve (and, as a last resort,
    // create) a target item when this pass actually has something to
    // create. Every prior implementation computed/created a target item
    // unconditionally, so a document whose matrix already has every
    // stable_id (update/match-only re-imports — exactly what FR-806 users
    // do after upgrading) still pushed a brand-new freeform "imported
    // requirements" item on *every* call, piling up empty items. Legacy
    // documents (imported before FR-806, whose matrix is a single freeform
    // bucket with no `fragment_seq` matching any section) hit this on
    // every re-import.
    let needs_create_target = preview.iter().any(|e| e.action == "create");
    let v = doc.verification.as_mut().unwrap();
    let target_item_pos: Option<usize> = if needs_create_target {
        Some(
            target_index
                .and_then(|seq| v.items.iter().position(|i| i.fragment_seq == Some(seq)))
                .or_else(|| {
                    // Legacy fallback: reuse the pre-existing freeform
                    // "imported requirements" bucket (from before FR-806,
                    // or from a prior run of this same fallback) instead of
                    // creating a duplicate one alongside it.
                    v.items.iter().position(|i| {
                        i.fragment_seq.is_none()
                            && i.label.as_deref() == Some("imported requirements")
                    })
                })
                .unwrap_or_else(|| {
                    // Defensive last resort (should not happen for a
                    // freshly auto-generated matrix: `target_index`, when
                    // `Some`, always names a section that either already
                    // had an item, or was just created above from the same
                    // `doc.sections` read) — land in a fresh freeform
                    // bucket rather than losing the import silently.
                    // Freeform SubItems are fully addressable by stable_id
                    // since this task's `resolve_stable_ids` fix.
                    v.items.push(crate::storage::docs::VerificationItem {
                        fragment_seq: None,
                        heading: heading_pattern.to_string(),
                        status: "pending".to_string(),
                        impl_refs: Vec::new(),
                        test_refs: Vec::new(),
                        reviewer: None,
                        verified_at: None,
                        notes: String::new(),
                        content_hash_at_verify: None,
                        category: "requirement".to_string(),
                        sub_items: Vec::new(),
                        label: Some("imported requirements".to_string()),
                    });
                    v.items.len() - 1
                }),
        )
    } else {
        None
    };
    for entry in &preview {
        match entry.action.as_str() {
            "create" => {
                let target_item = &mut v.items[target_item_pos
                    .expect("action==\"create\" implies needs_create_target was true")];
                target_item.sub_items.push(crate::storage::docs::SubItem {
                    index: target_item.sub_items.len(),
                    description: entry.title.clone(),
                    stable_id: Some(entry.stable_id.clone()),
                    priority: entry.priority.clone(),
                    dev_stage: entry.dev_stage.clone(),
                    ..Default::default()
                });
            }
            "update" | "match" => {
                // Preview scans sub_items across *every* VerificationItem
                // (`v.items.iter().flat_map(...)`) when deciding
                // stable_id/fuzzy-match actions, so apply must search that
                // same full scope — a matched sub_item may live in any
                // section's VerificationItem (e.g. one attached via
                // `handoff_doc_verify`'s `add_item` with a `fragment_seq`),
                // not just v.items[0]. Searching only items[0] here would
                // silently no-op the update while the response still
                // reports it as counted (review-rework round 1 MAJOR).
                if let Some(sub) = v
                    .items
                    .iter_mut()
                    .flat_map(|i| i.sub_items.iter_mut())
                    .find(|s| s.stable_id.as_deref() == Some(entry.stable_id.as_str()))
                {
                    sub.description = entry.title.clone();
                    if entry.priority.is_some() {
                        sub.priority = entry.priority.clone();
                    }
                    if entry.dev_stage.is_some() {
                        sub.dev_stage = entry.dev_stage.clone();
                    }
                } else {
                    // "match" case: the fuzzy-matched sub_item didn't have
                    // this stable_id yet (it may have had none, or a
                    // different one) — find it by description instead and
                    // assign the (possibly new) stable_id.
                    if let Some(sub) = v
                        .items
                        .iter_mut()
                        .flat_map(|i| i.sub_items.iter_mut())
                        .find(|s| {
                            super::docs::descriptions_fuzzy_match(&s.description, &entry.title)
                        })
                    {
                        sub.stable_id = Some(entry.stable_id.clone());
                        sub.description = entry.title.clone();
                        if entry.priority.is_some() {
                            sub.priority = entry.priority.clone();
                        }
                        if entry.dev_stage.is_some() {
                            sub.dev_stage = entry.dev_stage.clone();
                        }
                    }
                }
            }
            _ => {}
        }
    }
    v.updated_at = now;

    write_doc(handoff, &doc)?;
    let all_docs = read_all_docs(handoff)?;
    super::docs::write_requirements_summary(handoff, &all_docs)?;

    let mut out = json!({
        "doc_id": doc.id,
        "created": would_create,
        "updated": would_update,
        "skipped": would_skip,
        "parse_errors": parse_errors,
        "preview": preview,
    });
    if !orphan_warnings.is_empty() {
        out["warnings"] = json!(orphan_warnings);
    }
    Ok(to_json(&out))
}

/// Normalizes a file path for `handoff_doc_req_impact` matching: converts
/// backslashes to `/`, strips a leading `./`, and strips a trailing `/` —
/// so `impl_refs`/`test_refs`/`scope_paths` entries recorded with slightly
/// different spelling (e.g. `"src/x.rs"` vs `"./src/x.rs"`) still compare
/// equal to the queried file path (P2 §5.2 "重要": normalized comparison).
///
/// `pub(super)` (M2-06, wiki/260-vmodel-m2-design.md §4.2): shared with
/// `handoff_trace_impact`'s `file`/`git_diff` entry point
/// (`src/mcp/handlers/trace_impact.rs`), which reuses this exact
/// normalization so a path spelled differently in the two tools' inputs
/// still matches the same recorded refs.
pub(super) fn normalize_req_impact_path(p: &str) -> String {
    let replaced = p.replace('\\', "/");
    let stripped = replaced.strip_prefix("./").unwrap_or(&replaced);
    stripped.trim_end_matches('/').to_string()
}

/// Runs `git diff HEAD --name-only` in `project_dir` and returns the
/// changed file paths (relative to the repo root), used by
/// `handoff_doc_req_impact`'s `git_diff: true` mode (P2 §5.2). Returns an
/// empty list (rather than erroring) when the directory is not a git repo
/// or has no commits yet — `handoff_doc_req_impact` simply reports no
/// affected requirements in that case instead of failing the call.
///
/// `pub(super)` (M2-06): shared with `handoff_trace_impact`'s `git_diff:
/// true` entry point.
pub(super) fn git_diff_changed_files(project_dir: &Path) -> Vec<String> {
    let output = match std::process::Command::new("git")
        .args(["diff", "HEAD", "--name-only"])
        .current_dir(project_dir)
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// One requirement affected by a change to a target file
/// (`handoff_doc_req_impact`, P2 §5.2). `match_type` is `"impl_ref"` /
/// `"test_ref"` (direct — the target file is one of the SubItem's own
/// refs) or `"scope_path"` (indirect — the target file falls under the
/// owning document's `scope_paths`, but isn't itself listed as a ref).
///
/// `pub(super)` (M2-06, wiki/260-vmodel-m2-design.md §4.2): also used by
/// `handoff_trace_impact`'s `file`/`git_diff` entry point, which reports
/// [`find_affected_requirements`]'s matches directly as its own `changed`
/// set (see that tool's doc comment for why "実装が変わった" needs no
/// `def_hash` comparison the other two entry points do).
#[derive(Debug, Clone, Serialize)]
pub(super) struct AffectedRequirement {
    pub(super) stable_id: String,
    pub(super) title: String,
    pub(super) priority: Option<String>,
    pub(super) dev_stage: Option<String>,
    pub(super) match_type: &'static str,
    pub(super) doc_slug: String,
}

/// The file-matching core of `handoff_doc_req_impact` (P2 §5.2), factored out
/// (M2-06, wiki/260-vmodel-m2-design.md §4.2) so `handoff_trace_impact`'s
/// `file`/`git_diff` entry point can reuse the exact same 3-stage match
/// (`impl_ref` / `test_ref` direct, `scope_path` indirect) instead of
/// re-implementing it. `normalized_targets` must already be
/// [`normalize_req_impact_path`]-normalized (both callers do this once,
/// up front, rather than re-normalizing per SubItem). See
/// [`handle_doc_req_impact`]'s doc comment for the "most specific match
/// wins, at most one `AffectedRequirement` per SubItem per target file"
/// contract this preserves verbatim.
pub(super) fn find_affected_requirements(
    docs: &[DocMetadata],
    normalized_targets: &[String],
) -> Vec<AffectedRequirement> {
    let mut affected: Vec<AffectedRequirement> = Vec::new();

    for doc in docs {
        let doc_scope_paths: Vec<String> = doc
            .scope_paths
            .iter()
            .map(|p| normalize_req_impact_path(p))
            .collect();

        let Some(v) = &doc.verification else {
            continue;
        };
        for item in &v.items {
            for sub in &item.sub_items {
                let Some(stable_id) = sub.stable_id.as_deref() else {
                    continue;
                };

                let match_type = normalized_targets.iter().find_map(|target| {
                    if sub
                        .impl_refs
                        .iter()
                        .any(|r| normalize_req_impact_path(&r.path) == *target)
                    {
                        Some("impl_ref")
                    } else if sub
                        .test_refs
                        .iter()
                        .any(|r| normalize_req_impact_path(&r.path) == *target)
                    {
                        Some("test_ref")
                    } else if doc_scope_paths
                        .iter()
                        .any(|scope| target.starts_with(scope.as_str()))
                    {
                        Some("scope_path")
                    } else {
                        None
                    }
                });

                if let Some(match_type) = match_type {
                    affected.push(AffectedRequirement {
                        stable_id: stable_id.to_string(),
                        title: sub.description.clone(),
                        priority: sub.priority.clone(),
                        dev_stage: sub.dev_stage.clone(),
                        match_type,
                        doc_slug: doc.slug.clone(),
                    });
                }
            }
        }
    }

    affected.sort_by(|a, b| a.stable_id.cmp(&b.stable_id));
    affected
}

/// `handoff_doc_req_impact` — reverse-trace impact analysis: given a file
/// (or every file changed per `git diff HEAD`), finds every requirement
/// (`SubItem` with a `stable_id`) whose `impl_refs`/`test_refs` reference
/// that file directly, or whose owning document's `scope_paths` covers it
/// indirectly (requirements-traceability P2 §5.2,
/// `.handoff/docs/_doc.req-traceability-mcp-plan.md`).
///
/// `file` takes priority over `git_diff` when both are given (P2 §5.2
/// "重要"). Exactly one target-file source is required; neither given is an
/// error. When a SubItem matches a target file on more than one axis (e.g.
/// both an `impl_ref` and the doc's `scope_paths`), only the most specific
/// match is reported — direct ref matches (`impl_ref`/`test_ref`) take
/// priority over the indirect `scope_path` match, and a SubItem contributes
/// at most one `AffectedRequirement` per target file.
pub fn handle_doc_req_impact(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;
    let project_dir = &ctx.project_dir;

    let file_arg = arguments.get("file").and_then(|v| v.as_str());
    let git_diff = arguments
        .get("git_diff")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let target_files: Vec<String> = if let Some(f) = file_arg {
        vec![f.to_string()]
    } else if git_diff {
        git_diff_changed_files(project_dir)
    } else {
        bail!("handoff_doc_req_impact requires either 'file' or 'git_diff: true'");
    };
    let normalized_targets: Vec<String> = target_files
        .iter()
        .map(|f| normalize_req_impact_path(f))
        .collect();

    let docs = read_all_docs(handoff)?;
    let affected = find_affected_requirements(&docs, &normalized_targets);

    Ok(to_json(&json!({
        "affected_requirements": affected,
        "total": affected.len(),
    })))
}

/// `confidence` above which a [`ReqScanSuggestion`] counts toward
/// `auto_linkable` in `handoff_doc_req_scan`'s response (P2 §5.1: confidence
/// greater than 0.8, per `.handoff/docs/_doc.req-traceability-mcp-plan.md`
/// and the task's `patterns` doc comments below).
const REQ_SCAN_AUTO_LINKABLE_THRESHOLD: f64 = 0.8;

/// Confidence assigned to a `test_name`-pattern match — the test function
/// name encodes the `stable_id` positionally, which is reliable but not as
/// explicit as a `comment` match (P2 §5.1).
const REQ_SCAN_CONFIDENCE_TEST_NAME: f64 = 0.9;

/// Confidence assigned to a `comment`-pattern match (`// Implements:
/// C07-2.3.1.1`) — an explicit, unambiguous statement of the requirement id
/// (P2 §5.1).
const REQ_SCAN_CONFIDENCE_COMMENT: f64 = 0.95;

/// Confidence assigned to a `symbol`-pattern match — a fuzzy filename/symbol
/// vs. description match, deliberately below
/// [`REQ_SCAN_AUTO_LINKABLE_THRESHOLD`] so it is never auto-linkable (P2
/// §5.1).
const REQ_SCAN_CONFIDENCE_SYMBOL: f64 = 0.6;

/// One auto-discovered candidate link between a `stable_id` and a source
/// location, returned by `handoff_doc_req_scan` as a suggestion only — the
/// tool never writes `impl_refs`/`test_refs` itself (P2 §5.1, task
/// instructions "重要": "提案として返し自動適用しない").
#[derive(Debug, Clone, Serialize)]
struct ReqScanSuggestion {
    stable_id: String,
    match_type: &'static str,
    #[serde(rename = "match")]
    matched_text: String,
    file: String,
    line: usize,
    confidence: f64,
    ref_type: &'static str,
}

/// Classifies a scanned file path as a test location (`"test"`) or an
/// implementation location (`"impl"`) for [`ReqScanSuggestion::ref_type`],
/// by checking for a `tests/`/`test/` path segment or a `_test`/`test_`
/// stem — mirrors the informal convention already used across this repo's
/// own `tests/` layout and `#[cfg(test)] mod tests` inline modules.
fn classify_ref_type(path: &Path) -> &'static str {
    let is_test_dir = path.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        s == "tests" || s == "test"
    });
    let stem_is_test = path
        .file_stem()
        .map(|s| {
            let s = s.to_string_lossy();
            s.ends_with("_test") || s.ends_with("_tests") || s.starts_with("test_")
        })
        .unwrap_or(false);
    if is_test_dir || stem_is_test {
        "test"
    } else {
        "impl"
    }
}

/// Recursively collects every regular file under `root` into `out`. Missing
/// directories yield no files (not an error) — `handoff_doc_req_scan`'s
/// contract for a nonexistent `scope_paths` entry (task instructions
/// "重要": "scope_paths が存在しない場合は空の suggestions を返す
/// (エラーではない)"). Unreadable subdirectories are skipped silently for
/// the same reason, rather than failing the whole scan over one bad path.
/// No `walkdir` dependency is available in this crate, so this is a manual
/// `std::fs::read_dir` recursion (task instructions "重要").
fn collect_files_recursive(root: &Path, out: &mut Vec<PathBuf>) {
    if root.is_file() {
        out.push(root.to_path_buf());
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut children: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    children.sort();
    for child in children {
        if child.is_dir() {
            collect_files_recursive(&child, out);
        } else if child.is_file() {
            out.push(child);
        }
    }
}

/// Extracts every Rust test function name (`fn test_xxx(...)`) from a
/// source line, per the `test_name` pattern (P2 §5.1: `fn\s+(test_[a-z]\w*)`
/// — implemented by hand since this crate has no `regex` dependency, per
/// task instructions "重要"). Only the bare identifier is returned; `<...>`
/// generics and `(...)` params are not present in a fn name so no stripping
/// is needed beyond stopping at the first non-identifier character.
fn extract_test_fn_names(line: &str) -> Vec<String> {
    let mut names = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while let Some(rel) = line[i..].find("fn ") {
        let start = i + rel + 3;
        let mut j = start;
        while j < bytes.len() && (bytes[j] as char).is_whitespace() {
            j += 1;
        }
        let name_start = j;
        while j < bytes.len() {
            let c = bytes[j] as char;
            if c.is_ascii_alphanumeric() || c == '_' {
                j += 1;
            } else {
                break;
            }
        }
        let name = &line[name_start..j];
        if name.starts_with("test_") && name.len() > "test_".len() {
            names.push(name.to_string());
        }
        i = j.max(start);
        if i <= name_start {
            break;
        }
    }
    names
}

/// Extracts every `Implements:`/`Requirement:`/`Req:` comment annotation
/// from a source line, per the `comment` pattern (P2 §5.1: `(?://|#|/\*|\*)?
/// \s*(?:Implements|Requirement|Req):\s*([A-Z]\d+-[\w.-]+)` — implemented by
/// hand since this crate has no `regex` dependency, per task instructions
/// "重要"). Returns the raw id text after the keyword (e.g. `"C07-2.3.1.1"`
/// from `"// Implements: C07-2.3.1.1"`), trimmed of trailing punctuation and
/// whitespace.
fn extract_comment_req_ids(line: &str) -> Vec<String> {
    const KEYWORDS: [&str; 3] = ["Implements:", "Requirement:", "Req:"];
    let mut ids = Vec::new();
    for kw in KEYWORDS {
        let mut search_from = 0;
        while let Some(rel) = line[search_from..].find(kw) {
            let after = search_from + rel + kw.len();
            let rest = line[after..].trim_start();
            let id: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '.' || *c == '_')
                .collect();
            let id = id.trim_end_matches(['.', '-']).to_string();
            if !id.is_empty()
                && id.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && id.contains('-')
            {
                ids.push(id);
            }
            search_from = after;
        }
    }
    ids
}

/// `handoff_doc_req_scan` — scans source files under `scope_paths` for
/// discoverable links to `SubItem.stable_id`s, returned as ranked
/// suggestions only (requirements-traceability P2 §5.1,
/// `.handoff/docs/_doc.req-traceability-mcp-plan.md`). Never writes
/// `impl_refs`/`test_refs` itself — that remains a `handoff_doc_verify
/// set_refs` follow-up call once a human/AI confirms a suggestion (task
/// instructions "重要": "提案として返し自動適用しない").
///
/// Scope resolution: `doc_id` given -> only that document's `SubItem`s are
/// scan targets (and its own `scope_paths` are the default scan paths when
/// the caller omits `scope_paths`); `doc_id` omitted -> every document's
/// `SubItem`s are targets. A `scope_paths` entry that doesn't exist on disk
/// contributes no files rather than erroring (task instructions "重要").
///
/// Patterns (default: all three) — see [`extract_test_fn_names`]
/// (`test_name`, confidence [`REQ_SCAN_CONFIDENCE_TEST_NAME`]),
/// [`extract_comment_req_ids`] (`comment`, confidence
/// [`REQ_SCAN_CONFIDENCE_COMMENT`]), and the `symbol` branch below (fuzzy
/// filename-stem vs. description match via `docs::descriptions_fuzzy_match`,
/// confidence [`REQ_SCAN_CONFIDENCE_SYMBOL`] — deliberately below
/// [`REQ_SCAN_AUTO_LINKABLE_THRESHOLD`]).
///
/// `auto_linkable` counts suggestions with `confidence >
/// `[`REQ_SCAN_AUTO_LINKABLE_THRESHOLD`]` (P2 §5.1).
pub fn handle_doc_req_scan(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let doc_id_filter = arguments.get("doc_id").and_then(|v| v.as_str());
    let scope_paths_arg = string_array(arguments, "scope_paths");
    let patterns_arg = string_array(arguments, "patterns");
    let patterns: Vec<String> = if patterns_arg.is_empty() {
        vec![
            "test_name".to_string(),
            "comment".to_string(),
            "symbol".to_string(),
        ]
    } else {
        patterns_arg
    };

    let all_docs = read_all_docs(handoff)?;
    let target_docs: Vec<&DocMetadata> = match doc_id_filter {
        Some(id) => all_docs
            .iter()
            .filter(|d| d.id == id || d.slug == id)
            .collect(),
        None => all_docs.iter().collect(),
    };

    // Collect (stable_id, description) pairs to match against, across every
    // target document's verification matrix.
    let mut targets: Vec<(String, String)> = Vec::new();
    for doc in &target_docs {
        let Some(v) = &doc.verification else { continue };
        for item in &v.items {
            for sub in &item.sub_items {
                if let Some(stable_id) = &sub.stable_id {
                    targets.push((stable_id.clone(), sub.description.clone()));
                }
            }
        }
    }

    // scope_paths: explicit argument, else the union of every target
    // document's own `scope_paths` (P2 §5.1 input schema description
    // "defaults to doc's scope_paths").
    let scope_paths: Vec<String> = if !scope_paths_arg.is_empty() {
        scope_paths_arg
    } else {
        let mut paths = Vec::new();
        for doc in &target_docs {
            for p in &doc.scope_paths {
                if !paths.contains(p) {
                    paths.push(p.clone());
                }
            }
        }
        paths
    };

    let project_dir = &ctx.project_dir;
    let mut files: Vec<PathBuf> = Vec::new();
    for sp in &scope_paths {
        let path = Path::new(sp);
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            project_dir.join(path)
        };
        collect_files_recursive(&resolved, &mut files);
    }

    let mut suggestions: Vec<ReqScanSuggestion> = Vec::new();

    for file in &files {
        let Ok(content) = std::fs::read_to_string(file) else {
            continue;
        };
        let ref_type = classify_ref_type(file);
        let display_path = file.to_string_lossy().to_string();

        for (line_no, line) in content.lines().enumerate() {
            let line_number = line_no + 1;

            if patterns.iter().any(|p| p == "test_name") {
                for fn_name in extract_test_fn_names(line) {
                    for (stable_id, _desc) in &targets {
                        let prefix = stable_id_to_test_name_prefix(stable_id);
                        if fn_name.starts_with(&prefix) {
                            suggestions.push(ReqScanSuggestion {
                                stable_id: stable_id.clone(),
                                match_type: "test_name",
                                matched_text: fn_name.clone(),
                                file: display_path.clone(),
                                line: line_number,
                                confidence: REQ_SCAN_CONFIDENCE_TEST_NAME,
                                ref_type,
                            });
                        }
                    }
                }
            }

            if patterns.iter().any(|p| p == "comment") {
                for req_id in extract_comment_req_ids(line) {
                    if let Some((stable_id, _desc)) = targets.iter().find(|(sid, _)| sid == &req_id)
                    {
                        suggestions.push(ReqScanSuggestion {
                            stable_id: stable_id.clone(),
                            match_type: "comment",
                            matched_text: req_id.clone(),
                            file: display_path.clone(),
                            line: line_number,
                            confidence: REQ_SCAN_CONFIDENCE_COMMENT,
                            ref_type,
                        });
                    }
                }
            }
        }

        if patterns.iter().any(|p| p == "symbol") {
            let stem = file
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            for (stable_id, desc) in &targets {
                if super::docs::descriptions_fuzzy_match(&stem, desc)
                    || super::docs::descriptions_fuzzy_match(desc, &stem)
                {
                    suggestions.push(ReqScanSuggestion {
                        stable_id: stable_id.clone(),
                        match_type: "symbol",
                        matched_text: stem.clone(),
                        file: display_path.clone(),
                        line: 1,
                        confidence: REQ_SCAN_CONFIDENCE_SYMBOL,
                        ref_type,
                    });
                }
            }
        }
    }

    let auto_linkable = suggestions
        .iter()
        .filter(|s| s.confidence > REQ_SCAN_AUTO_LINKABLE_THRESHOLD)
        .count();
    let total = suggestions.len();

    Ok(to_json(&json!({
        "suggestions": suggestions,
        "total": total,
        "auto_linkable": auto_linkable,
    })))
}

/// One requirement whose `test_refs` were updated by
/// `handoff_doc_req_test_sync`, reported back to the caller (P3 §6.1).
#[derive(Debug, Clone, Serialize)]
struct ReqTestSyncUpdate {
    stable_id: String,
    test_result: &'static str,
    test_name: String,
}

/// Derives the `CodeRef.path` recorded for a matched test result: the test
/// name's module path (everything before the last `::`), or the full name
/// when there is no `::` separator (task §2c implies a source-location-like
/// path; `cargo test --format json` gives no file/line, so the module path
/// is the closest available proxy).
/// `handoff_doc_req_test_sync` — ingests `cargo test --format json` JSONL
/// output and records pass/fail against matching SubItems' `test_refs`,
/// across every document's verification matrix (requirements-traceability
/// P3 §6.1, `.handoff/docs/_doc.req-traceability-mcp-plan.md`).
///
/// Matching reuses [`stable_id_to_test_name_prefix`] (task instructions
/// §"重要": "req_scan の stable_id_to_test_name_prefix を再利用する。新規に
/// 作らない。") — a test name matches the first stable_id (in
/// document/verification-matrix order) whose derived prefix it starts with.
///
/// For each matched test, the target SubItem's `test_refs` is updated in
/// place (§2c): an existing `CodeRef` whose label already references that
/// exact test name (`"pass: {name}"` or `"fail: {name}"`) has its label
/// replaced; otherwise a new `CodeRef` is appended with
/// [`test_name_module_path`] as `path` and the same label. There is no
/// `dry_run` — the sync always applies (task instructions §"重要": "常に
/// 適用").
///
/// `test_output` takes priority over `test_output_file` when both are given;
/// omitting both is an error (task instructions §"重要").
pub fn handle_doc_req_test_sync(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let test_output_arg = arguments.get("test_output").and_then(|v| v.as_str());
    let test_output_file_arg = arguments.get("test_output_file").and_then(|v| v.as_str());

    let input: String = if let Some(s) = test_output_arg {
        s.to_string()
    } else if let Some(path) = test_output_file_arg {
        std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read test_output_file {path:?}: {e}"))?
    } else {
        bail!("handoff_doc_req_test_sync requires either 'test_output' or 'test_output_file'");
    };

    // M2-11 (wiki/260-vmodel-m2-design.md §4.6): `handoff_doc_req_test_sync`
    // delegates its cargo-JSON parsing to `storage::test_results` — the same
    // parser `handoff_trace_ingest` uses. `Skipped` (libtest `event ==
    // "ignored"`, a M2 addition the pre-M2 parser never produced at all) is
    // filtered out here so this tool's counts/matching stay byte-identical
    // to before M2: an ignored test was, and remains, invisible to
    // req_test_sync (not counted in `matched`/`unmatched`, never written).
    let test_results: Vec<_> = parse_cargo_test_jsonl(&input)
        .into_iter()
        .filter(|r| r.outcome != TestOutcome::Skipped)
        .collect();

    let mut docs = read_all_docs(handoff)?;

    let mut matched = 0usize;
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut updated: Vec<ReqTestSyncUpdate> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut touched_doc_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    // t360.8 (wiki/220 §2.6): layer items are body-owned for test_refs, so
    // their result is recorded as a run instead — batched into a single
    // `runs/<run_id>.json` for this whole `req_test_sync` call (not one file
    // per matched test) and applied after the main loop below.
    let mut layer_run_matches: Vec<(String, bool, String)> = Vec::new();

    for result in &test_results {
        let test_name = &result.name;
        let did_pass = result.outcome == TestOutcome::Pass;
        'docs: for doc in &mut docs {
            // wiki/220-vmodel-integration-design.md §2.6: for a layer item
            // (origin=body, or any SubItem on a layer document — `test_refs`
            // is body-owned once a document has a `layer`), `req_test_sync`
            // must not write a `test_refs` label — that field is defined by
            // the body's `- test:` attribute, not by this tool. §2.6's
            // "run として記録する" replacement (`handoff_trace_record`) is a
            // separate, not-yet-built tool (M1 t360.8+); until it lands,
            // this match is reported (so the caller isn't left guessing
            // whether the test ran) but not persisted as a `test_refs`
            // write, and callers are warned it needs `trace_record` instead.
            let doc_layer = doc.layer.clone();
            let Some(v) = &mut doc.verification else {
                continue;
            };
            for sub in v.items.iter_mut().flat_map(|i| i.sub_items.iter_mut()) {
                let Some(stable_id) = sub.stable_id.clone() else {
                    continue;
                };
                // wiki/260 §4.6's 3-stage match: an item's declared `- test:`
                // values (stages 1-2) take priority, falling through to M1's
                // legacy stable_id->prefix convention (stage 3, unconditional
                // — reused verbatim via `storage::test_results`) so a layer
                // item with no `test` attribute keeps matching exactly as
                // before M2. Only layer items contribute `test_attrs`: a
                // layer-less item's `test_refs` also holds the `pass:`/
                // `fail:` labels this same sync writes back onto it, so
                // treating those as "declared" values would stop a test
                // from matching again once it had already been recorded
                // once (M2-01 rework, same fix as `handoff_trace_ingest`'s
                // `collect_candidates`) — layer-less items stay stage-3-only,
                // unconditional on `test_refs`'s contents (§4.6: "M1 どおり").
                let is_layer_item = doc_layer.is_some() || sub.origin.as_deref() == Some("body");
                let test_attrs: Vec<String> = if is_layer_item {
                    sub.test_refs.iter().map(|r| r.path.clone()).collect()
                } else {
                    Vec::new()
                };
                if match_item_test(&test_attrs, &stable_id, test_name).is_none() {
                    continue;
                }

                if is_layer_item {
                    layer_run_matches.push((stable_id.clone(), did_pass, test_name.clone()));
                } else {
                    let label = if did_pass {
                        format!("pass: {test_name}")
                    } else {
                        format!("fail: {test_name}")
                    };
                    let existing = sub.test_refs.iter_mut().find(|r| {
                        r.label.as_deref().is_some_and(|l| {
                            l.ends_with(test_name.as_str())
                                && (l.starts_with("pass: ") || l.starts_with("fail: "))
                        })
                    });
                    match existing {
                        Some(coderef) => coderef.label = Some(label),
                        None => sub.test_refs.push(CodeRef {
                            path: test_name_module_path(test_name).to_string(),
                            lines: None,
                            label: Some(label),
                        }),
                    }
                    touched_doc_ids.insert(doc.id.clone());
                }

                matched += 1;
                if did_pass {
                    passed += 1;
                } else {
                    failed += 1;
                }
                updated.push(ReqTestSyncUpdate {
                    stable_id,
                    test_result: if did_pass { "pass" } else { "fail" },
                    test_name: test_name.clone(),
                });
                break 'docs;
            }
        }
    }

    let unmatched = test_results.len() - matched;

    for doc in &docs {
        if touched_doc_ids.contains(&doc.id) {
            write_doc(handoff, doc)?;
        }
    }
    let all_docs = read_all_docs(handoff)?;

    // t360.8 (wiki/220 §2.6): every layer-item match from this call becomes
    // one run entry in a single `runs/<run_id>.json` file — after `all_docs`
    // above so each entry's `body_hash` reflects the just-written state, not
    // a pre-write snapshot.
    //
    // S3 fix (t360.43 M1 review, wiki/220 §3.1 "記録後に `_latest.json` と
    // summary を更新する"): this run must be recorded *before*
    // `write_requirements_summary` below, not after — the pre-fix order
    // wrote the summary first, so a `handoff_doc_req_status`/`trace_report`
    // read landing between the two writes could observe a
    // `_requirements_summary.json` whose `inputs.runs_*` fields already lag
    // behind a run this same request was about to record.
    let mut run_id: Option<String> = None;
    if !layer_run_matches.is_empty() {
        let inputs: Vec<crate::storage::runs::RunResultInput> = layer_run_matches
            .iter()
            .map(
                |(stable_id, did_pass, test_name)| crate::storage::runs::RunResultInput {
                    item: stable_id.as_str(),
                    result: if *did_pass { "pass" } else { "fail" },
                    note: None,
                    evidence: vec![test_name.clone()],
                },
            )
            .collect();
        let commit = crate::storage::git::short_head_or_empty(&ctx.project_dir);
        let (recorded_run_id, run_warnings) = crate::storage::runs::record_run(
            handoff,
            &all_docs,
            &inputs,
            "ai",
            None,
            Some(commit),
            None,
        )?;
        warnings.extend(run_warnings);
        run_id = Some(recorded_run_id);
    }

    super::docs::write_requirements_summary(handoff, &all_docs)?;

    Ok(to_json(&json!({
        "matched": matched,
        "passed": passed,
        "failed": failed,
        "unmatched": unmatched,
        "updated_requirements": updated,
        "run_id": run_id,
        "warnings": warnings,
    })))
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

    /// Round-3 rework (MAJOR, whole-branch review of t230.5): `body_prose_token_count`
    /// is the eligibility gate that keeps empty/heading-only fragments (real
    /// `.handoff/docs` preambles and heading-only sections found by the
    /// reviewer to reach semantic-bonus scores of 0.85-0.87 for wholly
    /// unrelated queries — higher than any fixture-calibrated floor) out of
    /// `doc_query`'s semantic-only rescue path entirely, rather than relying
    /// on a single absolute-score floor to separate them from genuine
    /// cross-lingual matches.
    #[test]
    fn body_prose_token_count_is_zero_for_empty_body() {
        assert_eq!(body_prose_token_count(""), 0);
    }

    #[test]
    fn body_prose_token_count_is_zero_for_heading_only_body() {
        // Mirrors the real preamble ("# Title\n") and heading-only-section
        // ("## Some Heading\n") shapes the reviewer found scoring as noise.
        assert_eq!(body_prose_token_count("# Some Document Title\n"), 0);
        assert_eq!(body_prose_token_count("## Some Heading\n\n"), 0);
    }

    #[test]
    fn body_prose_token_count_counts_content_beyond_the_heading_line() {
        let body = "# Restart\n\nPlease restart the development server after changing the configuration file.\n";
        assert!(
            body_prose_token_count(body) >= DOC_SEMANTIC_PROSE_MIN_TOKENS,
            "a real content section must clear the eligibility threshold"
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

#[cfg(test)]
mod doc_req_list_tests {
    use super::*;
    use crate::storage::docs::{SubItem, Verification, VerificationItem};
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

    fn sub_item(stable_id: &str, priority: Option<&str>, dev_stage: Option<&str>) -> SubItem {
        SubItem {
            index: 0,
            description: format!("desc {stable_id}"),
            stable_id: Some(stable_id.to_string()),
            priority: priority.map(str::to_string),
            dev_stage: dev_stage.map(str::to_string),
            ..Default::default()
        }
    }

    fn sub_item_with_refs(
        stable_id: &str,
        priority: Option<&str>,
        dev_stage: Option<&str>,
        impl_refs: Vec<CodeRef>,
        test_refs: Vec<CodeRef>,
    ) -> SubItem {
        SubItem {
            impl_refs,
            test_refs,
            ..sub_item(stable_id, priority, dev_stage)
        }
    }

    fn section_item(fragment_seq: usize, sub_items: Vec<SubItem>) -> VerificationItem {
        VerificationItem {
            fragment_seq: Some(fragment_seq),
            heading: format!("heading {fragment_seq}"),
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

    fn seed_two_docs(handoff: &Path) {
        let doc_a = doc_with_items(
            "doc-a",
            "req-c01",
            vec![section_item(
                1,
                vec![
                    sub_item("C01-1.1", Some("P0"), Some("implemented")),
                    sub_item_with_refs(
                        "C01-1.2",
                        Some("P1"),
                        Some("tested"),
                        vec![CodeRef {
                            path: "src/a.rs".to_string(),
                            lines: None,
                            label: None,
                        }],
                        vec![CodeRef {
                            path: "tests/a.rs".to_string(),
                            lines: None,
                            label: None,
                        }],
                    ),
                ],
            )],
        );
        let doc_b = doc_with_items(
            "doc-b",
            "req-c07",
            vec![section_item(2, vec![sub_item("C07-2.1", Some("P0"), None)])],
        );
        write_doc(handoff, &doc_a).unwrap();
        write_doc(handoff, &doc_b).unwrap();
    }

    #[test]
    fn no_filters_returns_all_requirements() {
        let (_tmp, handoff) = setup();
        seed_two_docs(&handoff);
        let c = ctx(handoff);
        let out: Value =
            serde_json::from_str(&handle_doc_req_list(&c, &json!({})).unwrap()).unwrap();
        assert_eq!(out["total"], 3);
        assert_eq!(out["items"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn priority_filter_narrows_results() {
        let (_tmp, handoff) = setup();
        seed_two_docs(&handoff);
        let c = ctx(handoff);
        let out: Value =
            serde_json::from_str(&handle_doc_req_list(&c, &json!({ "priority": "P0" })).unwrap())
                .unwrap();
        assert_eq!(out["total"], 2);
        for item in out["items"].as_array().unwrap() {
            assert_eq!(item["priority"], "P0");
        }
    }

    #[test]
    fn dev_stage_filter_treats_none_as_not_started() {
        let (_tmp, handoff) = setup();
        seed_two_docs(&handoff);
        let c = ctx(handoff);
        let out: Value = serde_json::from_str(
            &handle_doc_req_list(&c, &json!({ "dev_stage": "not_started" })).unwrap(),
        )
        .unwrap();
        assert_eq!(out["total"], 1);
        assert_eq!(out["items"][0]["stable_id"], "C07-2.1");
    }

    #[test]
    fn category_filter_matches_stable_id_prefix() {
        let (_tmp, handoff) = setup();
        seed_two_docs(&handoff);
        let c = ctx(handoff);
        let out: Value =
            serde_json::from_str(&handle_doc_req_list(&c, &json!({ "category": "C07" })).unwrap())
                .unwrap();
        assert_eq!(out["total"], 1);
        assert_eq!(out["items"][0]["stable_id"], "C07-2.1");
    }

    #[test]
    fn has_tests_false_filter_excludes_items_with_test_refs() {
        let (_tmp, handoff) = setup();
        seed_two_docs(&handoff);
        let c = ctx(handoff);
        let out: Value =
            serde_json::from_str(&handle_doc_req_list(&c, &json!({ "has_tests": false })).unwrap())
                .unwrap();
        assert_eq!(out["total"], 2);
        for item in out["items"].as_array().unwrap() {
            assert!(item["test_refs"].as_array().unwrap().is_empty());
        }
    }

    #[test]
    fn has_tests_true_filter_includes_only_items_with_test_refs() {
        let (_tmp, handoff) = setup();
        seed_two_docs(&handoff);
        let c = ctx(handoff);
        let out: Value =
            serde_json::from_str(&handle_doc_req_list(&c, &json!({ "has_tests": true })).unwrap())
                .unwrap();
        assert_eq!(out["total"], 1);
        assert_eq!(out["items"][0]["stable_id"], "C01-1.2");
    }

    #[test]
    fn sort_by_stable_id_desc_orders_results() {
        let (_tmp, handoff) = setup();
        seed_two_docs(&handoff);
        let c = ctx(handoff);
        let out: Value = serde_json::from_str(
            &handle_doc_req_list(&c, &json!({ "sort": "stable_id", "order": "desc" })).unwrap(),
        )
        .unwrap();
        let ids: Vec<&str> = out["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["stable_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["C07-2.1", "C01-1.2", "C01-1.1"]);
    }

    // §4.4 (wiki/220 "自然順ソート"): stable_id sort must be numeric-natural,
    // not byte-lexicographic — plain `String::cmp` puts "FR-1001" before
    // "FR-101" (the 6th byte, '0' vs '1', decides it before the rest of the
    // number is compared), which reads as wrong to anyone expecting numeric
    // order.
    #[test]
    fn sort_by_stable_id_asc_is_natural_not_lexicographic() {
        let (_tmp, handoff) = setup();
        let doc = doc_with_items(
            "doc-1",
            "req-natural",
            vec![section_item(
                1,
                vec![
                    sub_item("FR-1001", None, None),
                    sub_item("FR-101", None, None),
                    sub_item("FR-2", None, None),
                ],
            )],
        );
        write_doc(&handoff, &doc).unwrap();
        let c = ctx(handoff);

        let out: Value = serde_json::from_str(
            &handle_doc_req_list(&c, &json!({ "sort": "stable_id", "order": "asc" })).unwrap(),
        )
        .unwrap();
        let ids: Vec<&str> = out["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["stable_id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            vec!["FR-2", "FR-101", "FR-1001"],
            "expected natural numeric order, got {ids:?}"
        );
    }

    #[test]
    fn sort_by_priority_asc_orders_results() {
        let (_tmp, handoff) = setup();
        seed_two_docs(&handoff);
        let c = ctx(handoff);
        let out: Value = serde_json::from_str(
            &handle_doc_req_list(&c, &json!({ "sort": "priority", "order": "asc" })).unwrap(),
        )
        .unwrap();
        let priorities: Vec<&str> = out["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["priority"].as_str().unwrap())
            .collect();
        let mut sorted = priorities.clone();
        sorted.sort();
        assert_eq!(priorities, sorted);
    }

    #[test]
    fn pagination_limit_and_offset_slice_results() {
        let (_tmp, handoff) = setup();
        seed_two_docs(&handoff);
        let c = ctx(handoff);
        let out: Value = serde_json::from_str(
            &handle_doc_req_list(
                &c,
                &json!({ "sort": "stable_id", "order": "asc", "limit": 1, "offset": 1 }),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(out["total"], 3, "total reflects pre-pagination count");
        assert_eq!(out["items"].as_array().unwrap().len(), 1);
        assert_eq!(out["items"][0]["stable_id"], "C01-1.2");
        assert_eq!(out["limit"], 1);
        assert_eq!(out["offset"], 1);
    }

    #[test]
    fn default_limit_is_100() {
        let (_tmp, handoff) = setup();
        seed_two_docs(&handoff);
        let c = ctx(handoff);
        let out: Value =
            serde_json::from_str(&handle_doc_req_list(&c, &json!({})).unwrap()).unwrap();
        assert_eq!(out["limit"], 100);
        assert_eq!(out["offset"], 0);
    }

    #[test]
    fn empty_result_returns_items_empty_and_total_zero() {
        let (_tmp, handoff) = setup();
        seed_two_docs(&handoff);
        let c = ctx(handoff);
        let out: Value =
            serde_json::from_str(&handle_doc_req_list(&c, &json!({ "priority": "P3" })).unwrap())
                .unwrap();
        assert_eq!(out["total"], 0);
        assert_eq!(out["items"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn no_docs_at_all_returns_empty_without_error() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff);
        let out: Value =
            serde_json::from_str(&handle_doc_req_list(&c, &json!({})).unwrap()).unwrap();
        assert_eq!(out["total"], 0);
        assert_eq!(out["items"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn item_carries_full_traceability_fields() {
        let (_tmp, handoff) = setup();
        seed_two_docs(&handoff);
        let c = ctx(handoff);
        let out: Value = serde_json::from_str(
            &handle_doc_req_list(&c, &json!({ "category": "C01", "sort": "stable_id" })).unwrap(),
        )
        .unwrap();
        let item = &out["items"][0];
        assert_eq!(item["stable_id"], "C01-1.1");
        assert_eq!(item["title"], "desc C01-1.1");
        assert_eq!(item["doc_id"], "doc-a");
        assert_eq!(item["doc_slug"], "req-c01");
        assert_eq!(item["fragment_seq"], 1);
        assert_eq!(item["sub_item_index"], 0);
        assert!(item["impl_refs"].is_array());
        assert!(item["test_refs"].is_array());
    }
}

#[cfg(test)]
mod doc_req_import_tests {
    use super::*;
    use crate::storage::docs::{SubItem, Verification, VerificationItem};
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

    const REQ_TREE_BODY: &str = "\
# req-c01-board-setup

## 1. 概要

Some preamble text.

## 2. 要件ツリー

### 2.1 基板外形

#### 2.1.1 外形形状定義

##### 2.1.1.1 矩形外形

##### 2.1.1.2 円形外形

## 3. ギャップ分析

| 要件 | 優先度 | 備考 |
|---|---|---|
| 矩形外形 | P0 | 必須 |
| 円形外形 | P2 | 任意 |
";

    fn seed_doc(handoff: &Path, id: &str, slug: &str, body: &str) -> DocMetadata {
        let d = DocMetadata::new(
            id.to_string(),
            slug.to_string(),
            format!("Title {id}"),
            "spec".to_string(),
            "2026-09-20T00:00:00Z".to_string(),
        );
        write_doc(handoff, &d).unwrap();
        write_doc_body(handoff, slug, body).unwrap();
        // Re-read so the returned DocMetadata reflects whatever write_doc
        // actually persisted (mirrors how the handler itself loads it).
        read_doc(handoff, slug).unwrap().unwrap()
    }

    /// wiki/220-vmodel-integration-design.md §2.3 write guard: `req_import`
    /// on a layer document is refused, even with `dry_run=true` — its
    /// SubItems are defined by the body (`sync_layer_items`), not by
    /// heading/gap-table extraction.
    #[test]
    fn req_import_on_layer_doc_is_refused() {
        let (_tmp, handoff) = setup();
        let doc = seed_doc(&handoff, "doc-1", "req-c01-board-setup", REQ_TREE_BODY);
        let mut doc = doc;
        doc.layer = Some("requirement".to_string());
        write_doc(&handoff, &doc).unwrap();
        let c = ctx(handoff.clone());

        let err =
            handle_doc_req_import(&c, &json!({ "doc_id": "doc-1", "dry_run": true })).unwrap_err();
        assert!(
            err.to_string().contains("本文を編集"),
            "error must direct the caller to edit the body: {err}"
        );
    }

    #[test]
    fn dry_run_default_returns_preview_without_writing() {
        let (_tmp, handoff) = setup();
        seed_doc(&handoff, "doc-1", "req-c01-board-setup", REQ_TREE_BODY);
        let c = ctx(handoff.clone());

        let out: Value = serde_json::from_str(
            &handle_doc_req_import(&c, &json!({ "doc_id": "doc-1" })).unwrap(),
        )
        .unwrap();

        assert_eq!(out["would_create"], 2, "two leaf headings under 要件ツリー");
        assert_eq!(out["would_update"], 0);
        assert_eq!(out["would_skip"], 0);
        assert_eq!(out["preview"].as_array().unwrap().len(), 2);
        for entry in out["preview"].as_array().unwrap() {
            assert_eq!(entry["action"], "create");
        }

        // dry_run must not persist anything.
        let doc = read_doc(&handoff, "req-c01-board-setup").unwrap().unwrap();
        assert!(doc.verification.is_none());
    }

    #[test]
    fn dry_run_false_actually_creates_sub_items() {
        let (_tmp, handoff) = setup();
        seed_doc(&handoff, "doc-1", "req-c01-board-setup", REQ_TREE_BODY);
        let c = ctx(handoff.clone());

        let out: Value = serde_json::from_str(
            &handle_doc_req_import(&c, &json!({ "doc_id": "doc-1", "dry_run": false })).unwrap(),
        )
        .unwrap();
        assert_eq!(out["created"], 2);

        let doc = read_doc(&handoff, "req-c01-board-setup").unwrap().unwrap();
        let v = doc.verification.expect("verification matrix must exist");
        let sub_items: Vec<&SubItem> = v.items.iter().flat_map(|i| i.sub_items.iter()).collect();
        assert_eq!(sub_items.len(), 2);
        assert!(sub_items
            .iter()
            .all(|s| s.stable_id.is_some() && s.stable_id.as_deref().unwrap().starts_with("C01")));

        // Cache file refreshed.
        let cache_path = docs_dir(&handoff).join("_requirements_summary.json");
        assert!(cache_path.exists());
    }

    // FR-806 (§4.1, wiki/220 "index == 配列位置 の不変条件"): a second import
    // pass that adds a new SubItem to a section which already has one (from
    // the first pass) must give the new SubItem an `index` that continues
    // from the existing one's position (`target_item.sub_items.len()` at
    // push time), not restart at 0 — this is the invariant `add_item` and
    // `req_import`'s bulk-create both rely on.
    #[test]
    fn second_import_appends_new_sub_item_index_after_existing_ones() {
        let (_tmp, handoff) = setup();
        seed_doc(&handoff, "doc-1", "req-c01-board-setup", REQ_TREE_BODY);
        let c = ctx(handoff.clone());

        // First pass creates the 2 leaf requirements (index 0, 1).
        let out1: Value = serde_json::from_str(
            &handle_doc_req_import(&c, &json!({ "doc_id": "doc-1", "dry_run": false })).unwrap(),
        )
        .unwrap();
        assert_eq!(out1["created"], 2);

        // A third leaf heading is added under the same "要件ツリー" section.
        let extended_body = REQ_TREE_BODY.replace(
            "## 3. ギャップ分析",
            "##### 2.1.1.3 三角外形\n\n## 3. ギャップ分析",
        );
        write_doc_body(&handoff, "req-c01-board-setup", &extended_body).unwrap();
        // Re-derive `doc.sections` from the new body the same way a real
        // `doc_save` would, so req_import's own `doc.sections` read (used
        // for auto-generate / section placement) reflects the edit.
        let mut doc = read_doc(&handoff, "req-c01-board-setup").unwrap().unwrap();
        let split_doc = crate::storage::docs::split::split(&extended_body, DEFAULT_SPLIT_LEVEL)
            .expect("body must split cleanly");
        doc.sections = compute_sections(&split_doc, false);
        write_doc(&handoff, &doc).unwrap();

        let out2: Value = serde_json::from_str(
            &handle_doc_req_import(&c, &json!({ "doc_id": "doc-1", "dry_run": false })).unwrap(),
        )
        .unwrap();
        assert_eq!(out2["created"], 1, "only the new leaf should be created");

        let doc = read_doc(&handoff, "req-c01-board-setup").unwrap().unwrap();
        let v = doc.verification.unwrap();
        let req_tree_item = v
            .items
            .iter()
            .find(|i| i.heading.contains("要件ツリー"))
            .unwrap();
        assert_eq!(req_tree_item.sub_items.len(), 3);
        for (position, sub) in req_tree_item.sub_items.iter().enumerate() {
            assert_eq!(
                sub.index, position,
                "SubItem.index must equal its array position after a second import: {:?}",
                req_tree_item.sub_items
            );
        }
    }

    // FR-806 (§4.1, wiki/220): first-time import on a matrix-less document
    // used to bootstrap a single `fragment_seq: None` freeform bucket for
    // every new SubItem — which `resolve_stable_ids` (pre-fix) skipped
    // entirely, so `handoff_update_task(requirement_ids=[...])` could never
    // resolve them ("Could not resolve requirement stable_id(s)" for every
    // id). This test asserts the fixed behavior: the full section-based
    // matrix is auto-generated (one item per `doc.sections` entry, like
    // `action="generate"`), and new SubItems land in the item for the
    // section that contains the matched heading_pattern heading (here,
    // "## 2. 要件ツリー") — not a synthetic freeform item.
    #[test]
    fn first_import_on_matrixless_doc_auto_generates_and_places_in_matched_section() {
        let (_tmp, handoff) = setup();
        seed_doc(&handoff, "doc-1", "req-c01-board-setup", REQ_TREE_BODY);
        let c = ctx(handoff.clone());

        let out: Value = serde_json::from_str(
            &handle_doc_req_import(&c, &json!({ "doc_id": "doc-1", "dry_run": false })).unwrap(),
        )
        .unwrap();
        assert_eq!(out["created"], 2);

        let doc = read_doc(&handoff, "req-c01-board-setup").unwrap().unwrap();
        let v = doc.verification.expect("verification matrix must exist");

        // Auto-generate mirrors action="generate": one item per section
        // (seq 0 preamble, "1. 概要", "2. 要件ツリー", "3. ギャップ分析").
        assert_eq!(v.items.len(), doc.sections.len());
        assert!(
            v.items.iter().all(|i| i.fragment_seq.is_some()),
            "every auto-generated item must be section-tied, not freeform: {:?}",
            v.items.iter().map(|i| i.fragment_seq).collect::<Vec<_>>()
        );

        let req_tree_item = v
            .items
            .iter()
            .find(|i| i.heading.contains("要件ツリー"))
            .expect("a section-tied item for the '要件ツリー' heading must exist");
        assert_eq!(
            req_tree_item.sub_items.len(),
            2,
            "both imported SubItems must land in the section containing the matched heading"
        );
        assert!(v
            .items
            .iter()
            .filter(|i| !std::ptr::eq(*i, req_tree_item))
            .all(|i| i.sub_items.is_empty()));

        // The whole point: every newly-imported SubItem must now be
        // resolvable by stable_id (freeform items used to make this
        // impossible).
        let stable_ids: Vec<String> = req_tree_item
            .sub_items
            .iter()
            .map(|s| s.stable_id.clone().unwrap())
            .collect();
        let (resolved, unresolved, ambiguous) =
            crate::mcp::handlers::docs::resolve_stable_ids(&handoff, &stable_ids).unwrap();
        assert!(
            unresolved.is_empty(),
            "expected every imported stable_id to resolve, got unresolved={unresolved:?}"
        );
        assert!(ambiguous.is_empty(), "ambiguous={ambiguous:?}");
        assert_eq!(resolved.len(), 2);
    }

    #[test]
    fn gap_table_assigns_priority_by_fuzzy_match() {
        let (_tmp, handoff) = setup();
        seed_doc(&handoff, "doc-1", "req-c01-board-setup", REQ_TREE_BODY);
        let c = ctx(handoff.clone());

        let out: Value = serde_json::from_str(
            &handle_doc_req_import(&c, &json!({ "doc_id": "doc-1" })).unwrap(),
        )
        .unwrap();

        let preview = out["preview"].as_array().unwrap();
        let rect = preview
            .iter()
            .find(|e| e["title"].as_str().unwrap().contains("矩形外形"))
            .unwrap();
        assert_eq!(rect["priority"], "P0");
        let circle = preview
            .iter()
            .find(|e| e["title"].as_str().unwrap().contains("円形外形"))
            .unwrap();
        assert_eq!(circle["priority"], "P2");
    }

    // §4.4 (wiki/220 "ギャップ表照合: ID 完全一致を最優先"): a gap-table row
    // named "FR-001" must never be fuzzy-matched against an unrelated
    // "NFR-001" requirement just because "FR-001" is a literal substring of
    // "NFR-001" — each id must get its own row's priority.
    const ID_COLLISION_BODY: &str = "\
# req-ids

## 2. 要件ツリー

### FR-001 ログイン機能

### NFR-001 応答性能

## 3. ギャップ分析

| 要件 | 優先度 | 備考 |
|---|---|---|
| FR-001 | P0 | 必須 |
| NFR-001 | P2 | 任意 |
";

    #[test]
    fn gap_table_exact_id_match_does_not_cross_assign_prefix_substring() {
        let (_tmp, handoff) = setup();
        seed_doc(&handoff, "doc-1", "req-ids", ID_COLLISION_BODY);
        let c = ctx(handoff.clone());

        let out: Value = serde_json::from_str(
            &handle_doc_req_import(&c, &json!({ "doc_id": "doc-1" })).unwrap(),
        )
        .unwrap();

        let preview = out["preview"].as_array().unwrap();
        let fr = preview
            .iter()
            .find(|e| e["title"].as_str().unwrap().starts_with("FR-001"))
            .expect("FR-001 candidate must be present");
        assert_eq!(
            fr["priority"], "P0",
            "FR-001 row must not be mis-assigned to NFR-001's priority: {preview:?}"
        );
        let nfr = preview
            .iter()
            .find(|e| e["title"].as_str().unwrap().starts_with("NFR-001"))
            .expect("NFR-001 candidate must be present");
        assert_eq!(
            nfr["priority"], "P2",
            "NFR-001 row must not be mis-assigned to FR-001's priority: {preview:?}"
        );
    }

    #[test]
    fn heading_pattern_customization_finds_alternate_section_name() {
        let (_tmp, handoff) = setup();
        let body = "\
# doc

## Custom Requirements Section

### Leaf One

### Leaf Two
";
        seed_doc(&handoff, "doc-1", "misc-doc", body);
        let c = ctx(handoff);

        let out: Value = serde_json::from_str(
            &handle_doc_req_import(
                &c,
                &json!({ "doc_id": "doc-1", "heading_pattern": "Custom Requirements" }),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(out["would_create"], 2);
    }

    #[test]
    fn parse_errors_reported_for_malformed_gap_table_row() {
        let (_tmp, handoff) = setup();
        // The gap table's second data row has fewer cells than the header
        // (only 2 columns instead of 3) — a genuine column-count mismatch,
        // which must surface in parse_errors rather than being silently
        // skipped.
        let body = "\
## 要件ツリー

### Leaf A

## ギャップ分析

| 要件 | 優先度 | 備考 |
|---|---|---|
| Leaf A | P0 |
";
        seed_doc(&handoff, "doc-1", "misc-doc", body);
        let c = ctx(handoff);

        let out: Value = serde_json::from_str(
            &handle_doc_req_import(&c, &json!({ "doc_id": "doc-1" })).unwrap(),
        )
        .unwrap();
        assert!(
            !out["parse_errors"].as_array().unwrap().is_empty(),
            "expected a parse_errors entry for the short row: {out}"
        );
    }

    #[test]
    fn merges_stable_id_match_as_update_preserving_dev_stage_and_refs() {
        let (_tmp, handoff) = setup();
        let mut d = DocMetadata::new(
            "doc-1".to_string(),
            "req-c01-board-setup".to_string(),
            "Title".to_string(),
            "spec".to_string(),
            "2026-09-20T00:00:00Z".to_string(),
        );
        d.verification = Some(Verification {
            status: "in_review".to_string(),
            created_at: "2026-09-20T00:00:00Z".to_string(),
            updated_at: "2026-09-20T00:00:00Z".to_string(),
            items: vec![VerificationItem {
                fragment_seq: None,
                heading: "imported requirements".to_string(),
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
                    description: "2.1.1.1 矩形外形".to_string(),
                    stable_id: Some("C01-2.1.1.1".to_string()),
                    priority: Some("P3".to_string()),
                    dev_stage: Some("implemented".to_string()),
                    impl_refs: vec![CodeRef {
                        path: "src/board.rs".to_string(),
                        lines: None,
                        label: None,
                    }],
                    ..Default::default()
                }],
                label: Some("imported requirements".to_string()),
            }],
        });
        write_doc(&handoff, &d).unwrap();
        write_doc_body(&handoff, "req-c01-board-setup", REQ_TREE_BODY).unwrap();

        let c = ctx(handoff.clone());
        let out: Value = serde_json::from_str(
            &handle_doc_req_import(&c, &json!({ "doc_id": "doc-1", "dry_run": false })).unwrap(),
        )
        .unwrap();
        assert_eq!(out["updated"], 1, "matches existing stable_id C01-2.1.1.1");
        assert_eq!(out["created"], 1, "the circle leaf is still new");

        let doc = read_doc(&handoff, "req-c01-board-setup").unwrap().unwrap();
        let v = doc.verification.unwrap();
        let updated = v.items[0]
            .sub_items
            .iter()
            .find(|s| s.stable_id.as_deref() == Some("C01-2.1.1.1"))
            .unwrap();
        assert_eq!(
            updated.priority.as_deref(),
            Some("P0"),
            "priority refreshed from gap table"
        );
        assert_eq!(
            updated.dev_stage.as_deref(),
            Some("implemented"),
            "dev_stage preserved across update"
        );
        assert_eq!(
            updated.impl_refs.len(),
            1,
            "impl_refs preserved across update"
        );
    }

    #[test]
    fn merges_fuzzy_description_match_relinks_stable_id() {
        let (_tmp, handoff) = setup();
        let mut d = DocMetadata::new(
            "doc-1".to_string(),
            "req-c01-board-setup".to_string(),
            "Title".to_string(),
            "spec".to_string(),
            "2026-09-20T00:00:00Z".to_string(),
        );
        d.verification = Some(Verification {
            status: "in_review".to_string(),
            created_at: "2026-09-20T00:00:00Z".to_string(),
            updated_at: "2026-09-20T00:00:00Z".to_string(),
            items: vec![VerificationItem {
                fragment_seq: None,
                heading: "imported requirements".to_string(),
                status: "pending".to_string(),
                impl_refs: Vec::new(),
                test_refs: Vec::new(),
                reviewer: None,
                verified_at: None,
                notes: String::new(),
                content_hash_at_verify: None,
                category: "requirement".to_string(),
                // Deliberately no stable_id yet, but text matches the
                // "2.1.1.1 矩形外形" leaf heading.
                sub_items: vec![SubItem {
                    index: 0,
                    description: "2.1.1.1 矩形外形".to_string(),
                    stable_id: None,
                    dev_stage: Some("in_progress".to_string()),
                    ..Default::default()
                }],
                label: Some("imported requirements".to_string()),
            }],
        });
        write_doc(&handoff, &d).unwrap();
        write_doc_body(&handoff, "req-c01-board-setup", REQ_TREE_BODY).unwrap();

        let c = ctx(handoff.clone());
        let out: Value = serde_json::from_str(
            &handle_doc_req_import(&c, &json!({ "doc_id": "doc-1", "dry_run": false })).unwrap(),
        )
        .unwrap();
        assert_eq!(out["updated"], 1, "fuzzy-matched to existing sub_item");

        let doc = read_doc(&handoff, "req-c01-board-setup").unwrap().unwrap();
        let v = doc.verification.unwrap();
        let matched = v.items[0]
            .sub_items
            .iter()
            .find(|s| s.description.contains("矩形外形"))
            .unwrap();
        assert!(
            matched.stable_id.is_some(),
            "fuzzy-matched sub_item must now have a stable_id assigned"
        );
        assert_eq!(
            matched.dev_stage.as_deref(),
            Some("in_progress"),
            "dev_stage preserved for fuzzy-matched sub_item"
        );
    }

    #[test]
    fn orphan_sub_item_reported_but_not_deleted() {
        let (_tmp, handoff) = setup();
        let mut d = DocMetadata::new(
            "doc-1".to_string(),
            "req-c01-board-setup".to_string(),
            "Title".to_string(),
            "spec".to_string(),
            "2026-09-20T00:00:00Z".to_string(),
        );
        d.verification = Some(Verification {
            status: "in_review".to_string(),
            created_at: "2026-09-20T00:00:00Z".to_string(),
            updated_at: "2026-09-20T00:00:00Z".to_string(),
            items: vec![VerificationItem {
                fragment_seq: None,
                heading: "imported requirements".to_string(),
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
                    description: "obsolete requirement no longer in the tree".to_string(),
                    stable_id: Some("C01-9.9.9.9".to_string()),
                    ..Default::default()
                }],
                label: Some("imported requirements".to_string()),
            }],
        });
        write_doc(&handoff, &d).unwrap();
        write_doc_body(&handoff, "req-c01-board-setup", REQ_TREE_BODY).unwrap();

        let c = ctx(handoff.clone());
        let out: Value = serde_json::from_str(
            &handle_doc_req_import(&c, &json!({ "doc_id": "doc-1", "dry_run": false })).unwrap(),
        )
        .unwrap();

        let warnings = out["warnings"].as_array().cloned().unwrap_or_default();
        assert!(
            warnings
                .iter()
                .any(|w| w.as_str().unwrap().contains("C01-9.9.9.9")),
            "orphan stable_id must be reported in warnings: {warnings:?}"
        );

        // Orphan must still exist afterward — never deleted.
        let doc = read_doc(&handoff, "req-c01-board-setup").unwrap().unwrap();
        let v = doc.verification.unwrap();
        assert!(v
            .items
            .iter()
            .flat_map(|i| i.sub_items.iter())
            .any(|s| s.stable_id.as_deref() == Some("C01-9.9.9.9")));
    }

    #[test]
    fn empty_result_when_requirement_tree_section_not_found() {
        let (_tmp, handoff) = setup();
        let body = "# doc\n\n## Some Other Section\n\nNo requirement tree here.\n";
        seed_doc(&handoff, "doc-1", "misc-doc", body);
        let c = ctx(handoff);

        let out: Value = serde_json::from_str(
            &handle_doc_req_import(&c, &json!({ "doc_id": "doc-1" })).unwrap(),
        )
        .unwrap();
        assert_eq!(out["would_create"], 0);
        assert_eq!(out["would_update"], 0);
        assert_eq!(out["preview"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn missing_doc_id_returns_error() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff);
        let result = handle_doc_req_import(&c, &json!({ "doc_id": "does-not-exist" }));
        assert!(result.is_err());
    }

    // review-rework round 2 MAJOR regression: `handle_doc_req_import`
    // (dry_run=false) against a document whose matrix is a *legacy*
    // freeform-only bucket (the shape every doc imported before FR-806 has:
    // a single `fragment_seq: None` item holding every SubItem) must not
    // pile up a fresh, empty freeform item on every re-import once the
    // bucket already holds every stable_id — i.e. once every preview action
    // is "update"/"match" and none is "create". Before the fix,
    // `target_item_pos` was resolved (and, on the legacy-shape fallback
    // path, a brand-new item pushed) on *every* call regardless of whether
    // anything needed a target to create into: 3 re-imports turned 1 item
    // into 4, with the 3 new ones permanently empty.
    #[test]
    fn reimporting_into_legacy_freeform_matrix_does_not_pile_up_items() {
        let (_tmp, handoff) = setup();
        seed_doc(&handoff, "doc-1", "req-c01-board-setup", REQ_TREE_BODY);
        let c = ctx(handoff.clone());

        // Discover the stable_ids/titles the import would derive for the
        // two leaf headings, without writing anything yet.
        let preview: Value = serde_json::from_str(
            &handle_doc_req_import(&c, &json!({ "doc_id": "doc-1" })).unwrap(),
        )
        .unwrap();
        let derived: Vec<(String, String)> = preview["preview"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e["stable_id"].as_str().unwrap().to_string(),
                    e["title"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(derived.len(), 2, "{preview}");

        // Simulate the legacy pre-FR-806 matrix shape: a single freeform
        // bucket already holding both requirements under their derived
        // stable_ids, so a re-import matches every candidate by stable_id
        // (action == "update") and never needs to create anything.
        let mut doc = read_doc(&handoff, "req-c01-board-setup").unwrap().unwrap();
        let sub_items: Vec<SubItem> = derived
            .iter()
            .enumerate()
            .map(|(i, (id, title))| SubItem {
                index: i,
                description: title.clone(),
                stable_id: Some(id.clone()),
                ..Default::default()
            })
            .collect();
        doc.verification = Some(Verification {
            status: "pending".to_string(),
            created_at: "2026-09-20T00:00:00Z".to_string(),
            updated_at: "2026-09-20T00:00:00Z".to_string(),
            items: vec![VerificationItem {
                fragment_seq: None,
                heading: "要件ツリー".to_string(),
                status: "pending".to_string(),
                impl_refs: Vec::new(),
                test_refs: Vec::new(),
                reviewer: None,
                verified_at: None,
                notes: String::new(),
                content_hash_at_verify: None,
                category: "requirement".to_string(),
                sub_items,
                label: Some("imported requirements".to_string()),
            }],
        });
        write_doc(&handoff, &doc).unwrap();

        for _ in 0..3 {
            let out: Value = serde_json::from_str(
                &handle_doc_req_import(&c, &json!({ "doc_id": "doc-1", "dry_run": false }))
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(out["created"], 0, "{out}");
        }

        let doc = read_doc(&handoff, "req-c01-board-setup").unwrap().unwrap();
        let v = doc.verification.unwrap();
        assert_eq!(
            v.items.len(),
            1,
            "must not pile up freeform items on re-import: {:?}",
            v.items
        );
        assert_eq!(v.items[0].sub_items.len(), 2, "{:?}", v.items[0].sub_items);
        for (position, sub) in v.items[0].sub_items.iter().enumerate() {
            assert_eq!(
                sub.index, position,
                "SubItem.index must equal its array position: {:?}",
                v.items[0].sub_items
            );
        }
    }

    // --- t360.30 (FR-802/FR-805, aelm requirement-doc import) ---

    /// FR-802: a heading elsewhere in the document that incidentally
    /// contains the pattern substring (e.g. an appendix heading
    /// `"全機能要件ツリー C26 展開"` containing `"要件ツリー"`) but has no
    /// children of its own must not win over the real, populated tree
    /// section — even when the real section lacks the `## 2. 要件ツリー`
    /// wrapper and is only found via the numeric-prefix fallback.
    #[test]
    fn incidental_pattern_match_with_no_children_is_skipped_in_favor_of_numeric_fallback() {
        let (_tmp, handoff) = setup();
        let body = "\
# req-c26-power-electronics

## 1. 概要

Some preamble.

### 2.1 厚銅設計

#### 2.1.1 銅箔厚パラメータ

### 2.2 大電流配線

#### 2.2.1 電流密度計算

## 3. aelm適応判断

Not part of the tree.

## 4. ギャップ分析表

| 要件ID | 要件名 | 優先度 |
|---|---|---|
| 2.1.1 | 銅箔厚パラメータ | P1 |
| 2.2.1 | 電流密度計算 | P2 |

## 5. 実装アーキテクチャメモ

##### 5.5 全機能要件ツリー C26 展開

（このセクションには子見出しがない — 本文中の参照のみ）
";
        seed_doc(&handoff, "doc-1", "req-c26-power-electronics", body);
        let c = ctx(handoff);

        let out: Value = serde_json::from_str(
            &handle_doc_req_import(&c, &json!({ "doc_id": "doc-1" })).unwrap(),
        )
        .unwrap();
        assert_eq!(
            out["would_create"], 2,
            "must find the real tree via the numeric fallback, not the empty incidental match: {out}"
        );
    }

    /// FR-802: aelm's `req-c02-design-rules.md` (and 5 other documents
    /// surveyed for wiki/250) number their requirement-tree subsections
    /// `### 2.1 ...` but have no enclosing `## 2. 要件ツリー` wrapper heading
    /// at all — the document jumps straight from `## 1. 概要` to
    /// `## 3. ...`. Before the numeric-prefix fallback, `req_import`
    /// produced zero candidates for a document shaped like this.
    #[test]
    fn missing_wrapper_heading_falls_back_to_numeric_prefix_scan() {
        let (_tmp, handoff) = setup();
        let body = "\
# req-c02-design-rules

## 1. 概要

Some preamble.

### 2.1 グローバル設計制約

#### 2.1.1 銅箔制約

##### 2.1.1.1 最小クリアランス

##### 2.1.1.2 最小トレース幅

### 2.2 ネットクラス

#### 2.2.1 ネットクラス定義

## 3. aelm適応判断

Not part of the tree.

## 4. ギャップ分析表

| 要件ID | 要件名 | 優先度 |
|---|---|---|
| 2.1.1.1 | 最小クリアランス | P0 |
| 2.1.1.2 | 最小トレース幅 | P1 |
| 2.2.1 | ネットクラス定義 | P2 |
";
        seed_doc(&handoff, "doc-1", "req-c02-design-rules", body);
        let c = ctx(handoff);

        let out: Value = serde_json::from_str(
            &handle_doc_req_import(&c, &json!({ "doc_id": "doc-1" })).unwrap(),
        )
        .unwrap();
        assert_eq!(out["would_create"], 3, "{out}");
        let preview = out["preview"].as_array().unwrap();
        let priorities: std::collections::HashSet<&str> = preview
            .iter()
            .map(|e| e["priority"].as_str().unwrap())
            .collect();
        assert_eq!(
            priorities,
            std::collections::HashSet::from(["P0", "P1", "P2"]),
            "{preview:?}"
        );
        // The fallback must surface as an informational parse_errors entry,
        // not silently.
        let parse_errors = out["parse_errors"].as_array().unwrap();
        assert!(
            parse_errors.iter().any(|e| e["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("numeric-prefix")),
            "{parse_errors:?}"
        );
    }

    /// FR-802: a gap table with *dedicated* `要件ID`/`要件名` columns whose ID
    /// scheme is unrelated to the requirement-tree's own heading numbering
    /// (aelm's `req-c02-design-rules.md`: headings are numbered `2.1.1.1`,
    /// but the gap table's `要件ID` column uses mnemonic ids like `C02-G01`)
    /// must still resolve priority via the *name* column, not by
    /// mis-treating the ID column's text as the match key (the pre-FR-802
    /// `row_text = "first non-priority cell"` heuristic picked the ID
    /// column here, which never matches any heading-derived description).
    /// Also exercises FR-805's dedicated `実装状態` column deriving
    /// `dev_stage` independently of the `優先度` column.
    #[test]
    fn gap_table_with_dedicated_id_and_name_columns_matches_via_name_and_derives_dev_stage() {
        let (_tmp, handoff) = setup();
        let body = "\
# req-c02-design-rules

## 2. 要件ツリー

### 2.1 グローバル設計制約

#### 2.1.1 銅箔制約

##### 2.1.1.1 最小クリアランス（Minimum Clearance）

## 4. ギャップ分析表

| 要件ID | 要件名 | 実装状態 | 優先度 | 備考 |
|---|---|---|---|---|
| C02-G01 | 最小クリアランス | 実装済 | P0 | 必須 |
";
        seed_doc(&handoff, "doc-1", "req-c02-design-rules", body);
        let c = ctx(handoff);

        let out: Value = serde_json::from_str(
            &handle_doc_req_import(&c, &json!({ "doc_id": "doc-1" })).unwrap(),
        )
        .unwrap();
        let preview = out["preview"].as_array().unwrap();
        assert_eq!(preview.len(), 1, "{preview:?}");
        assert_eq!(preview[0]["priority"], "P0", "{preview:?}");
        assert_eq!(preview[0]["dev_stage"], "implemented", "{preview:?}");
    }

    /// FR-802: when a gap table's ID column value exactly matches a
    /// candidate's heading-derived leading ID token, that ID match must win
    /// even when the name column's wording is completely unrelated (no
    /// fuzzy/substring match possible) — aelm's `req-c19-power-integrity.md`
    /// headings embed mnemonic ids directly (`REQ-C19.1.1.1 銅箔抵抗モデル`),
    /// a shape `docs::extract_requirement_id`'s FR/NFR-only allow-list does
    /// not recognize (it requires digits immediately after the matched
    /// prefix, and `REQ-C19...` has a letter there).
    #[test]
    fn gap_table_id_exact_match_wins_despite_unrelated_name_text() {
        let (_tmp, handoff) = setup();
        let body = "\
# req-c19-power-integrity

## 2. 要件ツリー

### 2.1 DC解析

#### REQ-C19.1.1.1 銅箔抵抗モデル

## 4. ギャップ分析表

| 要件ID | 要件名 | 優先度 |
|---|---|---|
| REQ-C19.1.1.1 | Copper Resistance Model | P1 |
";
        seed_doc(&handoff, "doc-1", "req-c19-power-integrity", body);
        let c = ctx(handoff);

        let out: Value = serde_json::from_str(
            &handle_doc_req_import(&c, &json!({ "doc_id": "doc-1" })).unwrap(),
        )
        .unwrap();
        let preview = out["preview"].as_array().unwrap();
        assert_eq!(preview.len(), 1, "{preview:?}");
        assert_eq!(
            preview[0]["priority"], "P1",
            "ID exact match must fire despite the name column being unrelated text: {preview:?}"
        );
    }

    /// FR-805: a merged priority/status cell (aelm's
    /// `req-c06-placement.md` "優先度" column literally contains `"P0=出荷済"`)
    /// must split into `priority: "P0"` and `dev_stage: "implemented"` when
    /// the table has no dedicated implementation-status column.
    #[test]
    fn gap_table_merged_priority_status_cell_splits_into_priority_and_dev_stage() {
        let (_tmp, handoff) = setup();
        let body = "\
# req-c06-placement

## 2. 要件ツリー

### 2.1 移動

#### 2.1.1 フットプリントドラッグ移動

## 4. ギャップ分析表

| 要件ID | 要件名 | 優先度 |
|---|---|---|
| C06-M01-a | フットプリントドラッグ移動 | P0=出荷済 |
";
        seed_doc(&handoff, "doc-1", "req-c06-placement", body);
        let c = ctx(handoff);

        let out: Value = serde_json::from_str(
            &handle_doc_req_import(&c, &json!({ "doc_id": "doc-1" })).unwrap(),
        )
        .unwrap();
        let preview = out["preview"].as_array().unwrap();
        assert_eq!(preview.len(), 1, "{preview:?}");
        assert_eq!(preview[0]["priority"], "P0", "{preview:?}");
        assert_eq!(preview[0]["dev_stage"], "implemented", "{preview:?}");
    }

    /// FR-805 merge rule: re-importing must never downgrade a `dev_stage`
    /// that has already progressed past whatever a (possibly stale)
    /// document's gap table currently says — only fills `dev_stage` when
    /// the existing SubItem has none set yet.
    #[test]
    fn dev_stage_merge_rule_does_not_downgrade_existing_verified_sub_item() {
        let (_tmp, handoff) = setup();
        let body = "\
# req-c01-board-setup

## 2. 要件ツリー

### 2.1 基板外形

#### 2.1.1 外形形状定義

##### 2.1.1.1 矩形外形

## 4. ギャップ分析表

| 要件ID | 要件名 | 実装状態 | 優先度 |
|---|---|---|---|
| 2.1.1.1 | 矩形外形 | 未実装 | P0 |
";
        let doc = seed_doc(&handoff, "doc-1", "req-c01-board-setup", body);
        let mut doc = doc;
        doc.verification = Some(Verification {
            status: "pending".to_string(),
            created_at: "2026-09-20T00:00:00Z".to_string(),
            updated_at: "2026-09-20T00:00:00Z".to_string(),
            items: vec![VerificationItem {
                fragment_seq: None,
                heading: "要件ツリー".to_string(),
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
                    description: "2.1.1.1 矩形外形".to_string(),
                    stable_id: Some("C01-2.1.1.1".to_string()),
                    dev_stage: Some("verified".to_string()),
                    ..Default::default()
                }],
                label: Some("imported requirements".to_string()),
            }],
        });
        write_doc(&handoff, &doc).unwrap();
        let c = ctx(handoff.clone());

        let out: Value = serde_json::from_str(
            &handle_doc_req_import(&c, &json!({ "doc_id": "doc-1", "dry_run": false })).unwrap(),
        )
        .unwrap();
        assert_eq!(out["updated"], 1, "{out}");

        let doc = read_doc(&handoff, "req-c01-board-setup").unwrap().unwrap();
        let sub = &doc.verification.unwrap().items[0].sub_items[0];
        assert_eq!(
            sub.dev_stage.as_deref(),
            Some("verified"),
            "manually-tracked dev_stage must not be downgraded by a stale gap-table import"
        );
        assert_eq!(
            sub.priority.as_deref(),
            Some("P0"),
            "priority still updates from the gap table (unchanged pre-existing behavior)"
        );
    }

    /// FR-802: gap-table cells that are entirely Markdown-bolded (aelm's
    /// `req-c01-board-setup.md` bolds exceptional rows like
    /// `"**PRE-2.2.2**"`) must still match on both the ID and name columns.
    #[test]
    fn bold_wrapped_gap_table_id_and_name_cells_are_still_matched() {
        let (_tmp, handoff) = setup();
        let body = "\
# req-c01-board-setup

## 2. 要件ツリー

### 2.2 レイヤースタック定義

#### PRE-2.2.2 LayerKind::Dielectric バリアント追加

## 4. ギャップ分析表

| 要件ID | 要件名 | 優先度 |
|---|---|---|
| **PRE-2.2.2** | **LayerKind::Dielectric バリアント追加** | P2 |
";
        seed_doc(&handoff, "doc-1", "req-c01-board-setup", body);
        let c = ctx(handoff);

        let out: Value = serde_json::from_str(
            &handle_doc_req_import(&c, &json!({ "doc_id": "doc-1" })).unwrap(),
        )
        .unwrap();
        let preview = out["preview"].as_array().unwrap();
        assert_eq!(preview.len(), 1, "{preview:?}");
        assert_eq!(preview[0]["priority"], "P2", "{preview:?}");
    }

    /// FR-802: aelm's `## 4. ギャップ分析表` sections routinely open with a
    /// 2-column "優先度凡例" legend table (`優先度 | 定義`) before the real
    /// per-item gap table — the legend table's header cell is also
    /// literally named "優先度", so the pre-existing "use the first table in
    /// the section" behavior would have picked the legend table (whose rows
    /// are `P0`/`P1`/... definitions, not real items) instead of the real
    /// one that follows it.
    #[test]
    fn gap_table_scan_skips_a_preceding_priority_legend_table() {
        let (_tmp, handoff) = setup();
        let body = "\
# req-c01-board-setup

## 2. 要件ツリー

### 2.1 基板外形

#### 2.1.1 外形形状定義

##### 2.1.1.1 矩形外形

## 4. ギャップ分析表

### 優先度凡例

| 優先度 | 定義 |
|--------|------|
| P0 | 出荷済み（実装完了） |
| P3 | アドバンスド |

### 4.1 検証状態について

| 要件ID | 要件名 | 優先度 |
|---|---|---|
| 2.1.1.1 | 矩形外形 | P0 |
";
        seed_doc(&handoff, "doc-1", "req-c01-board-setup", body);
        let c = ctx(handoff);

        let out: Value = serde_json::from_str(
            &handle_doc_req_import(&c, &json!({ "doc_id": "doc-1" })).unwrap(),
        )
        .unwrap();
        let preview = out["preview"].as_array().unwrap();
        assert_eq!(preview.len(), 1, "{preview:?}");
        assert_eq!(
            preview[0]["priority"], "P0",
            "must match the real per-item table, not misfire off the legend table: {preview:?}"
        );
    }

    /// FR-802: aelm's `## 4. ギャップ分析表` section is frequently split into
    /// several `### 4.N ...` subsections, each with its *own* per-item gap
    /// table (observed on `req-c02-design-rules.md`: 6 separate tables under
    /// one `## 4.` heading) — not one single table for the whole section.
    /// Before this fix, `parse_gap_table` picked only the *first* qualifying
    /// table in the section and ignored the rest, so priority/dev_stage for
    /// every row in a later subsection's table was silently dropped even
    /// though the row's id/name matched a real requirement-tree candidate.
    #[test]
    fn gap_table_scan_merges_rows_from_every_subsection_table() {
        let (_tmp, handoff) = setup();
        let body = "\
# req-c02-design-rules

## 2. 要件ツリー

### 2.1 グローバル設計制約

#### 2.1.1 銅箔制約

##### 2.1.1.1 最小クリアランス

### 2.2 ネットクラス

#### 2.2.1 ネットクラス定義

## 4. ギャップ分析表

### 4.1 グローバル設計制約

| 要件ID | 要件名 | 優先度 |
|---|---|---|
| 2.1.1.1 | 最小クリアランス | P0 |

### 4.2 ネットクラス

| 要件ID | 要件名 | 優先度 |
|---|---|---|
| 2.2.1 | ネットクラス定義 | P2 |
";
        seed_doc(&handoff, "doc-1", "req-c02-design-rules", body);
        let c = ctx(handoff);

        let out: Value = serde_json::from_str(
            &handle_doc_req_import(&c, &json!({ "doc_id": "doc-1" })).unwrap(),
        )
        .unwrap();
        let preview = out["preview"].as_array().unwrap();
        assert_eq!(preview.len(), 2, "{preview:?}");
        let by_title: std::collections::HashMap<&str, &Value> = preview
            .iter()
            .map(|e| (e["title"].as_str().unwrap(), e))
            .collect();
        assert_eq!(
            by_title["2.1.1.1 最小クリアランス"]["priority"], "P0",
            "row from the first subsection's table (4.1) must still match: {preview:?}"
        );
        assert_eq!(
            by_title["2.2.1 ネットクラス定義"]["priority"], "P2",
            "row from a later subsection's table (4.2) must also match, not just the first table in the section: {preview:?}"
        );
    }

    /// review-rework round 2 MAJOR (FR-802 §2 "表ヘッダーの揺れ（列マッピング
    /// 指定）に対応する"): a header cell not in the built-in synonym list
    /// (`重要度` for priority, `項目` for name) must still be usable via an
    /// explicit `column_map` override — without it, this table's priority
    /// column would never be recognized (`detect_gap_table_columns` has no
    /// `重要度` synonym) and the whole table would be skipped as a
    /// non-qualifying (< recognizable priority column) table.
    #[test]
    fn column_map_header_override_recognizes_unsynonymized_priority_column() {
        let (_tmp, handoff) = setup();
        let body = "\
# req-external

## 2. 要件ツリー

### 2.1 外部書式

#### 2.1.1 独自ヘッダーの項目

## 4. ギャップ分析表

| 項目 | 重要度 | 備考 |
|---|---|---|
| 独自ヘッダーの項目 | P1 | - |
";
        seed_doc(&handoff, "doc-1", "req-external", body);
        let c = ctx(handoff);

        // Without column_map, the built-in synonyms recognize neither
        // header cell as a priority column, so nothing qualifies.
        let baseline: Value = serde_json::from_str(
            &handle_doc_req_import(&c, &json!({ "doc_id": "doc-1" })).unwrap(),
        )
        .unwrap();
        assert_eq!(
            baseline["preview"][0]["priority"],
            Value::Null,
            "baseline (no column_map): '重要度'/'項目' are not built-in synonyms, priority must stay unset: {baseline:?}"
        );

        let out: Value = serde_json::from_str(
            &handle_doc_req_import(
                &c,
                &json!({
                    "doc_id": "doc-1",
                    "column_map": { "priority": "重要度", "name": "項目" },
                }),
            )
            .unwrap(),
        )
        .unwrap();
        let preview = out["preview"].as_array().unwrap();
        assert_eq!(preview.len(), 1, "{preview:?}");
        assert_eq!(
            preview[0]["priority"], "P1",
            "column_map header-string override must resolve '重要度' to the priority column: {preview:?}"
        );
    }

    /// `column_map` also accepts a 0-based column index instead of a header
    /// string (FR-802 review feedback: "値はヘッダー文字列（または 0 始まりの
    /// 列番号）とする").
    #[test]
    fn column_map_column_index_override_recognizes_unsynonymized_priority_column() {
        let (_tmp, handoff) = setup();
        let body = "\
# req-external-2

## 2. 要件ツリー

### 2.1 外部書式

#### 2.1.1 独自ヘッダーの項目2

## 4. ギャップ分析表

| 項目 | Pri. | 備考 |
|---|---|---|
| 独自ヘッダーの項目2 | P2 | - |
";
        seed_doc(&handoff, "doc-1", "req-external-2", body);
        let c = ctx(handoff);

        let out: Value = serde_json::from_str(
            &handle_doc_req_import(
                &c,
                &json!({
                    "doc_id": "doc-1",
                    "column_map": { "priority": 1 },
                }),
            )
            .unwrap(),
        )
        .unwrap();
        let preview = out["preview"].as_array().unwrap();
        assert_eq!(preview.len(), 1, "{preview:?}");
        assert_eq!(
            preview[0]["priority"], "P2",
            "column_map index override must resolve column 1 to the priority column: {preview:?}"
        );
    }
}

#[cfg(test)]
mod doc_req_scan_tests {
    use super::*;
    use crate::storage::docs::{SubItem, Verification, VerificationItem};
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

    fn sub_item(stable_id: &str) -> SubItem {
        SubItem {
            index: 0,
            description: format!("desc {stable_id}"),
            stable_id: Some(stable_id.to_string()),
            ..Default::default()
        }
    }

    fn section_item(sub_items: Vec<SubItem>) -> VerificationItem {
        VerificationItem {
            fragment_seq: Some(1),
            heading: "heading".to_string(),
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

    /// test_name pattern: `test_c01_2_1_1_1_...` -> stable_id `C01-2.1.1.1`
    /// (P2 §5.1, instructions §"パターン test_name"), confidence 0.9.
    #[test]
    fn scan_matches_stable_id_by_test_name_pattern() {
        let (_tmp, handoff) = setup();
        let doc = doc_with_items(
            "doc-a",
            "req-c01",
            vec![section_item(vec![sub_item("C01-2.1.1.1")])],
        );
        write_doc(&handoff, &doc).unwrap();

        let scan_dir = handoff.parent().unwrap().join("tests_src");
        std::fs::create_dir_all(&scan_dir).unwrap();
        std::fs::write(
            scan_dir.join("routing_test.rs"),
            "#[test]\nfn test_c01_2_1_1_1_rectangular_outline() {\n    assert!(true);\n}\n",
        )
        .unwrap();

        let c = ctx(handoff);
        let out: Value = serde_json::from_str(
            &handle_doc_req_scan(
                &c,
                &json!({
                    "scope_paths": [scan_dir.to_string_lossy()],
                    "patterns": ["test_name"],
                }),
            )
            .unwrap(),
        )
        .unwrap();

        let suggestions = out["suggestions"].as_array().unwrap();
        assert_eq!(suggestions.len(), 1, "suggestions: {suggestions:?}");
        assert_eq!(suggestions[0]["stable_id"], "C01-2.1.1.1");
        assert_eq!(suggestions[0]["match_type"], "test_name");
        assert_eq!(suggestions[0]["confidence"], 0.9);
        assert_eq!(suggestions[0]["ref_type"], "test");
        assert_eq!(out["total"], 1);
        assert_eq!(out["auto_linkable"], 1);
    }

    /// comment pattern: `// Implements: C07-2.3.1.1` matches the stable_id
    /// directly (P2 §5.1, instructions §"パターン comment"), confidence 0.95.
    #[test]
    fn scan_matches_stable_id_by_comment_pattern() {
        let (_tmp, handoff) = setup();
        let doc = doc_with_items(
            "doc-b",
            "req-c07",
            vec![section_item(vec![sub_item("C07-2.3.1.1")])],
        );
        write_doc(&handoff, &doc).unwrap();

        let scan_dir = handoff.parent().unwrap().join("src_impl");
        std::fs::create_dir_all(&scan_dir).unwrap();
        std::fs::write(
            scan_dir.join("router.rs"),
            "// Implements: C07-2.3.1.1\nfn route() {}\n",
        )
        .unwrap();

        let c = ctx(handoff);
        let out: Value = serde_json::from_str(
            &handle_doc_req_scan(
                &c,
                &json!({
                    "scope_paths": [scan_dir.to_string_lossy()],
                    "patterns": ["comment"],
                }),
            )
            .unwrap(),
        )
        .unwrap();

        let suggestions = out["suggestions"].as_array().unwrap();
        assert_eq!(suggestions.len(), 1, "suggestions: {suggestions:?}");
        assert_eq!(suggestions[0]["stable_id"], "C07-2.3.1.1");
        assert_eq!(suggestions[0]["match_type"], "comment");
        assert_eq!(suggestions[0]["confidence"], 0.95);
        assert_eq!(suggestions[0]["ref_type"], "impl");
        assert_eq!(out["auto_linkable"], 1);
    }

    /// `auto_linkable` only counts suggestions with confidence > 0.8 — a
    /// low-confidence `symbol` match must not be counted even though it is
    /// still returned as a suggestion.
    #[test]
    fn auto_linkable_counts_only_high_confidence_suggestions() {
        let (_tmp, handoff) = setup();
        let doc = doc_with_items(
            "doc-a",
            "req-c01",
            vec![section_item(vec![sub_item("C01-2.1.1.1")])],
        );
        write_doc(&handoff, &doc).unwrap();

        let scan_dir = handoff.parent().unwrap().join("mixed_src");
        std::fs::create_dir_all(&scan_dir).unwrap();
        std::fs::write(scan_dir.join("a.rs"), "fn test_c01_2_1_1_1_outline() {}\n").unwrap();
        // `sub_item("C01-2.1.1.1")`'s description is `"desc C01-2.1.1.1"`
        // (see the `sub_item` helper below) — the filename stem must
        // substring-match it (case-insensitively) for
        // `docs::descriptions_fuzzy_match` to fire.
        std::fs::write(scan_dir.join("desc c01-2.1.1.1.rs"), "fn unrelated() {}\n").unwrap();

        let c = ctx(handoff);
        let out: Value = serde_json::from_str(
            &handle_doc_req_scan(
                &c,
                &json!({
                    "scope_paths": [scan_dir.to_string_lossy()],
                    "patterns": ["test_name", "symbol"],
                }),
            )
            .unwrap(),
        )
        .unwrap();

        let suggestions = out["suggestions"].as_array().unwrap();
        let total = out["total"].as_u64().unwrap();
        let auto_linkable = out["auto_linkable"].as_u64().unwrap();
        assert_eq!(total, suggestions.len() as u64);
        let high_conf_count = suggestions
            .iter()
            .filter(|s| s["confidence"].as_f64().unwrap() > 0.8)
            .count() as u64;
        assert_eq!(auto_linkable, high_conf_count);
        assert!(
            auto_linkable < total,
            "expected at least one low-confidence suggestion excluded from auto_linkable: {out:?}"
        );
    }

    /// The scan is suggestions-only: it must never mutate the document's
    /// verification matrix (no impl_refs/test_refs written, no stable_id
    /// changed) — confirmed by re-reading the doc after the call.
    #[test]
    fn scan_does_not_mutate_the_document() {
        let (_tmp, handoff) = setup();
        let doc = doc_with_items(
            "doc-a",
            "req-c01",
            vec![section_item(vec![sub_item("C01-2.1.1.1")])],
        );
        write_doc(&handoff, &doc).unwrap();

        let scan_dir = handoff.parent().unwrap().join("tests_src2");
        std::fs::create_dir_all(&scan_dir).unwrap();
        std::fs::write(
            scan_dir.join("routing_test.rs"),
            "fn test_c01_2_1_1_1_outline() {}\n",
        )
        .unwrap();

        let c = ctx(handoff);
        handle_doc_req_scan(
            &c,
            &json!({
                "scope_paths": [scan_dir.to_string_lossy()],
                "patterns": ["test_name"],
            }),
        )
        .unwrap();

        let reloaded = crate::storage::docs::read_doc(&c.handoff_dir, "req-c01")
            .unwrap()
            .unwrap();
        let sub = &reloaded.verification.unwrap().items[0].sub_items[0];
        assert!(sub.impl_refs.is_empty());
        assert!(sub.test_refs.is_empty());
        assert_eq!(sub.stable_id.as_deref(), Some("C01-2.1.1.1"));
    }

    /// A non-existent `scope_paths` entry must yield empty suggestions, not
    /// an error (instructions §"重要": "scope_paths が存在しない場合は空の
    /// suggestions を返す (エラーではない)").
    #[test]
    fn nonexistent_scope_path_returns_empty_suggestions_not_error() {
        let (_tmp, handoff) = setup();
        let doc = doc_with_items(
            "doc-a",
            "req-c01",
            vec![section_item(vec![sub_item("C01-2.1.1.1")])],
        );
        write_doc(&handoff, &doc).unwrap();

        let c = ctx(handoff);
        let result = handle_doc_req_scan(
            &c,
            &json!({
                "scope_paths": ["/does/not/exist/anywhere"],
                "patterns": ["test_name", "comment", "symbol"],
            }),
        );
        assert!(result.is_ok());
        let out: Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(out["suggestions"].as_array().unwrap().len(), 0);
        assert_eq!(out["total"], 0);
        assert_eq!(out["auto_linkable"], 0);
    }

    /// `doc_id` restricts the scan target to only that document's
    /// SubItems — a matching test name for a stable_id belonging to a
    /// different document must not produce a suggestion.
    #[test]
    fn doc_id_filter_restricts_to_that_documents_sub_items() {
        let (_tmp, handoff) = setup();
        let doc_a = doc_with_items(
            "doc-a",
            "req-c01",
            vec![section_item(vec![sub_item("C01-2.1.1.1")])],
        );
        let doc_b = doc_with_items(
            "doc-b",
            "req-c07",
            vec![section_item(vec![sub_item("C07-2.3.1.1")])],
        );
        write_doc(&handoff, &doc_a).unwrap();
        write_doc(&handoff, &doc_b).unwrap();

        let scan_dir = handoff.parent().unwrap().join("scoped_src");
        std::fs::create_dir_all(&scan_dir).unwrap();
        std::fs::write(scan_dir.join("a.rs"), "fn test_c01_2_1_1_1_outline() {}\n").unwrap();
        std::fs::write(
            scan_dir.join("b.rs"),
            "fn test_c07_2_3_1_1_something() {}\n",
        )
        .unwrap();

        let c = ctx(handoff);
        let out: Value = serde_json::from_str(
            &handle_doc_req_scan(
                &c,
                &json!({
                    "doc_id": "doc-a",
                    "scope_paths": [scan_dir.to_string_lossy()],
                    "patterns": ["test_name"],
                }),
            )
            .unwrap(),
        )
        .unwrap();

        let suggestions = out["suggestions"].as_array().unwrap();
        assert_eq!(suggestions.len(), 1, "suggestions: {suggestions:?}");
        assert_eq!(suggestions[0]["stable_id"], "C01-2.1.1.1");
    }
}

#[cfg(test)]
mod doc_req_test_sync_tests {
    use super::*;
    use crate::storage::docs::{SubItem, Verification, VerificationItem};
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

    fn sub_item(stable_id: &str) -> SubItem {
        SubItem {
            index: 0,
            description: format!("desc {stable_id}"),
            stable_id: Some(stable_id.to_string()),
            ..Default::default()
        }
    }

    fn section_item(sub_items: Vec<SubItem>) -> VerificationItem {
        VerificationItem {
            fragment_seq: Some(1),
            heading: "heading".to_string(),
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

    /// `parse_cargo_test_jsonl` extracts `(name, passed)` from `type=="test"`
    /// lines only, and silently skips malformed JSON lines and `type=="suite"`
    /// summary lines mixed into the same input (task §4: "正常な JSONL + 不正行混在").
    #[test]
    fn parse_cargo_test_jsonl_skips_malformed_and_suite_lines() {
        let input = concat!(
            "{\"type\":\"test\",\"event\":\"ok\",\"name\":\"tests::test_c01_2_1_1_1_rect\"}\n",
            "not valid json at all\n",
            "{\"type\":\"suite\",\"event\":\"ok\",\"passed\":10,\"failed\":1}\n",
            "{\"type\":\"test\",\"event\":\"failed\",\"name\":\"tests::test_c07_routing_a_star\"}\n",
            "\n",
        );

        let results = parse_cargo_test_jsonl(input);

        assert_eq!(
            results,
            vec![
                crate::storage::test_results::ParsedTestResult {
                    name: "tests::test_c01_2_1_1_1_rect".to_string(),
                    outcome: TestOutcome::Pass,
                },
                crate::storage::test_results::ParsedTestResult {
                    name: "tests::test_c07_routing_a_star".to_string(),
                    outcome: TestOutcome::Fail,
                },
            ]
        );
    }

    /// A matched `event=="ok"` test is recorded as `pass` against the
    /// SubItem whose `stable_id` derives the matching `test_name` prefix,
    /// and the summary counts it in both `matched` and `passed`.
    #[test]
    fn test_sync_matches_ok_event_as_pass() {
        let (_tmp, handoff) = setup();
        let doc = doc_with_items(
            "doc-a",
            "req-c01",
            vec![section_item(vec![sub_item("C01-2.1.1.1")])],
        );
        write_doc(&handoff, &doc).unwrap();

        let c = ctx(handoff.clone());
        let input = "{\"type\":\"test\",\"event\":\"ok\",\"name\":\"tests::test_c01_2_1_1_1_rect_outline\"}\n";
        let out: Value = serde_json::from_str(
            &handle_doc_req_test_sync(&c, &json!({ "test_output": input })).unwrap(),
        )
        .unwrap();

        assert_eq!(out["matched"], 1);
        assert_eq!(out["passed"], 1);
        assert_eq!(out["failed"], 0);
        assert_eq!(out["unmatched"], 0);
        let updated = out["updated_requirements"].as_array().unwrap();
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0]["stable_id"], "C01-2.1.1.1");
        assert_eq!(updated[0]["test_result"], "pass");
        assert_eq!(
            updated[0]["test_name"],
            "tests::test_c01_2_1_1_1_rect_outline"
        );
    }

    /// A matched `event=="failed"` test is recorded as `fail`, counted in
    /// `matched` and `failed` (not `passed`).
    #[test]
    fn test_sync_matches_failed_event_as_fail() {
        let (_tmp, handoff) = setup();
        let doc = doc_with_items(
            "doc-b",
            "req-c07",
            vec![section_item(vec![sub_item("C07-2.5.1.1")])],
        );
        write_doc(&handoff, &doc).unwrap();

        let c = ctx(handoff.clone());
        let input =
            "{\"type\":\"test\",\"event\":\"failed\",\"name\":\"tests::test_c07_2_5_1_1_router\"}\n";
        let out: Value = serde_json::from_str(
            &handle_doc_req_test_sync(&c, &json!({ "test_output": input })).unwrap(),
        )
        .unwrap();

        assert_eq!(out["matched"], 1);
        assert_eq!(out["passed"], 0);
        assert_eq!(out["failed"], 1);
        let updated = out["updated_requirements"].as_array().unwrap();
        assert_eq!(updated[0]["test_result"], "fail");
    }

    /// A test name that matches no SubItem's derived prefix contributes to
    /// `unmatched`, not `matched`/`passed`/`failed`.
    #[test]
    fn test_sync_counts_unmatched_tests() {
        let (_tmp, handoff) = setup();
        let doc = doc_with_items(
            "doc-c",
            "req-c09",
            vec![section_item(vec![sub_item("C09-1.1.1.1")])],
        );
        write_doc(&handoff, &doc).unwrap();

        let c = ctx(handoff.clone());
        let input = concat!(
            "{\"type\":\"test\",\"event\":\"ok\",\"name\":\"tests::test_unrelated_helper\"}\n",
            "{\"type\":\"test\",\"event\":\"ok\",\"name\":\"tests::test_c09_1_1_1_1_thing\"}\n",
        );
        let out: Value = serde_json::from_str(
            &handle_doc_req_test_sync(&c, &json!({ "test_output": input })).unwrap(),
        )
        .unwrap();

        assert_eq!(out["matched"], 1);
        assert_eq!(out["passed"], 1);
        assert_eq!(out["unmatched"], 1);
    }

    /// A matched test result is actually persisted onto the SubItem's
    /// `test_refs` on disk (task §2c: "matched したテストの pass/fail を
    /// 対応する SubItem の test_refs に記録") — no `dry_run` exists, so the
    /// sync always applies.
    #[test]
    fn test_sync_persists_test_refs_onto_disk() {
        let (_tmp, handoff) = setup();
        let doc = doc_with_items(
            "doc-d",
            "req-c11",
            vec![section_item(vec![sub_item("C11-3.2.1.1")])],
        );
        write_doc(&handoff, &doc).unwrap();

        let c = ctx(handoff.clone());
        let input =
            "{\"type\":\"test\",\"event\":\"ok\",\"name\":\"router_tests::test_c11_3_2_1_1_dfa\"}\n";
        handle_doc_req_test_sync(&c, &json!({ "test_output": input })).unwrap();

        let reloaded = read_doc(&handoff, "req-c11").unwrap().unwrap();
        let sub = &reloaded.verification.unwrap().items[0].sub_items[0];
        assert_eq!(sub.test_refs.len(), 1, "test_refs: {:?}", sub.test_refs);
        assert_eq!(sub.test_refs[0].path, "router_tests");
        assert_eq!(
            sub.test_refs[0].label.as_deref(),
            Some("pass: router_tests::test_c11_3_2_1_1_dfa")
        );
    }

    /// M2-01 rework consistency (`handoff_trace_ingest`'s same fix): a
    /// layer-less item's own previously-written `pass:`/`fail:` label must
    /// never be treated as a "declared `test` attribute" for stage-1/2
    /// matching — only stage 3 (M1's legacy stable_id-prefix convention)
    /// applies to layer-less items, so a second sync on the same test still
    /// matches after the first sync already wrote a label.
    #[test]
    fn test_sync_matches_again_after_a_prior_sync_wrote_its_own_label() {
        let (_tmp, handoff) = setup();
        let doc = doc_with_items(
            "doc-i",
            "req-c12",
            vec![section_item(vec![sub_item("C12-1.1.1.1")])],
        );
        write_doc(&handoff, &doc).unwrap();

        let c = ctx(handoff.clone());
        let ok_input =
            "{\"type\":\"test\",\"event\":\"ok\",\"name\":\"tests::test_c12_1_1_1_1_widget\"}\n";
        handle_doc_req_test_sync(&c, &json!({ "test_output": ok_input })).unwrap();

        let fail_input =
            "{\"type\":\"test\",\"event\":\"failed\",\"name\":\"tests::test_c12_1_1_1_1_widget\"}\n";
        let out: Value = serde_json::from_str(
            &handle_doc_req_test_sync(&c, &json!({ "test_output": fail_input })).unwrap(),
        )
        .unwrap();

        assert_eq!(out["matched"], 1, "expected a repeat match: {out}");
        assert_eq!(out["failed"], 1);

        let reloaded = read_doc(&handoff, "req-c12").unwrap().unwrap();
        let sub = &reloaded.verification.unwrap().items[0].sub_items[0];
        assert!(
            sub.test_refs
                .iter()
                .any(|r| r.label.as_deref() == Some("fail: tests::test_c12_1_1_1_1_widget")),
            "expected the label to be updated to fail: {:?}",
            sub.test_refs
        );
    }

    /// wiki/220-vmodel-integration-design.md §2.6: a matched test result for
    /// a SubItem on a layer document must not be written to `test_refs`
    /// (body-owned) — instead (t360.8) it is recorded as a run
    /// (`runs/<run_id>.json` + `runs/_latest.json`), still reported as
    /// matched/passed, and the owning document is not rewritten.
    #[test]
    fn test_sync_records_a_run_instead_of_test_refs_for_layer_doc_sub_item() {
        let (_tmp, handoff) = setup();
        let mut doc = doc_with_items(
            "doc-layer",
            "req-layer",
            vec![section_item(vec![sub_item("ST-001")])],
        );
        doc.layer = Some("system_test".to_string());
        write_doc(&handoff, &doc).unwrap();

        let c = ctx(handoff.clone());
        let input = "{\"type\":\"test\",\"event\":\"ok\",\"name\":\"tests::test_st_001\"}\n";
        let result: Value = serde_json::from_str(
            &handle_doc_req_test_sync(&c, &json!({ "test_output": input })).unwrap(),
        )
        .unwrap();
        assert_eq!(result["matched"], 1);
        assert_eq!(result["passed"], 1);
        assert!(
            result["warnings"].as_array().unwrap().is_empty(),
            "a resolvable layer-item match must not warn: {:?}",
            result["warnings"]
        );
        let run_id = result["run_id"].as_str().expect("run_id must be present");

        let reloaded = read_doc(&handoff, "req-layer").unwrap().unwrap();
        let sub = &reloaded.verification.unwrap().items[0].sub_items[0];
        assert!(
            sub.test_refs.is_empty(),
            "test_refs must not be written on a layer document's SubItem: {:?}",
            sub.test_refs
        );

        let run_content =
            std::fs::read_to_string(handoff.join("runs").join(format!("{run_id}.json"))).unwrap();
        let run_json: Value = serde_json::from_str(&run_content).unwrap();
        assert_eq!(run_json["results"][0]["item"], "ST-001");
        assert_eq!(run_json["results"][0]["result"], "pass");

        let latest_content =
            std::fs::read_to_string(handoff.join("runs").join("_latest.json")).unwrap();
        let latest_json: Value = serde_json::from_str(&latest_content).unwrap();
        assert_eq!(latest_json["items"]["ST-001"]["result"], "pass");
    }

    /// S3 (t360.43 M1 review, wiki/220 §3.1 "記録後に `_latest.json` と
    /// summary を更新する"): the run this call records must land on disk
    /// *before* `_requirements_summary.json` is (re)written, not after — the
    /// pre-fix order wrote the summary first. Proven by an observable
    /// consequence of the ordering rather than by instrumenting call order
    /// directly: `_requirements_summary.json`'s `inputs.runs_count`/
    /// `runs_max_id` fingerprint is computed from a `runs/` directory scan
    /// (`compute_derived_inputs`), so if the summary were written *before*
    /// the run file exists, its persisted fingerprint would still show the
    /// pre-run state (`runs_count` one less, `runs_max_id` not yet this
    /// call's `run_id`) even though a run was in fact recorded moments
    /// later in the very same request.
    #[test]
    fn test_sync_records_the_run_before_writing_the_summary_so_its_inputs_fingerprint_already_reflects_it(
    ) {
        let (_tmp, handoff) = setup();
        let mut doc = doc_with_items(
            "doc-layer-2",
            "req-layer-2",
            vec![section_item(vec![sub_item("ST-002")])],
        );
        doc.layer = Some("system_test".to_string());
        write_doc(&handoff, &doc).unwrap();

        let c = ctx(handoff.clone());
        let input = "{\"type\":\"test\",\"event\":\"ok\",\"name\":\"tests::test_st_002\"}\n";
        let result: Value = serde_json::from_str(
            &handle_doc_req_test_sync(&c, &json!({ "test_output": input })).unwrap(),
        )
        .unwrap();
        let run_id = result["run_id"].as_str().expect("run_id must be present");

        let summary_content =
            std::fs::read_to_string(docs_dir(&handoff).join("_requirements_summary.json")).unwrap();
        let summary_json: Value = serde_json::from_str(&summary_content).unwrap();
        assert_eq!(
            summary_json["inputs"]["runs_count"], 1,
            "the summary's persisted fingerprint must already count the run this same call \
             just recorded"
        );
        assert_eq!(
            summary_json["inputs"]["runs_max_id"],
            format!("{run_id}.json"),
            "the summary's persisted fingerprint must already name this call's own run_id"
        );
    }

    /// `test_output` takes priority over `test_output_file` when both are
    /// given (task §"重要": "test_output と test_output_file の両方指定時:
    /// test_output を優先").
    #[test]
    fn test_sync_prefers_test_output_over_file_when_both_given() {
        let (tmp, handoff) = setup();
        let doc = doc_with_items(
            "doc-e",
            "req-c13",
            vec![section_item(vec![sub_item("C13-1.1.1.1")])],
        );
        write_doc(&handoff, &doc).unwrap();

        let file_path = tmp.path().join("from_file.jsonl");
        std::fs::write(
            &file_path,
            "{\"type\":\"test\",\"event\":\"failed\",\"name\":\"tests::test_should_not_be_used\"}\n",
        )
        .unwrap();

        let c = ctx(handoff);
        let inline_input =
            "{\"type\":\"test\",\"event\":\"ok\",\"name\":\"tests::test_c13_1_1_1_1_used\"}\n";
        let out: Value = serde_json::from_str(
            &handle_doc_req_test_sync(
                &c,
                &json!({
                    "test_output": inline_input,
                    "test_output_file": file_path.to_string_lossy(),
                }),
            )
            .unwrap(),
        )
        .unwrap();

        assert_eq!(out["matched"], 1);
        let updated = out["updated_requirements"].as_array().unwrap();
        assert_eq!(updated[0]["test_name"], "tests::test_c13_1_1_1_1_used");
    }

    /// Omitting both `test_output` and `test_output_file` is an error (task
    /// §"重要": "両方なしはエラー").
    #[test]
    fn test_sync_errors_when_neither_input_given() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff);

        let result = handle_doc_req_test_sync(&c, &json!({}));

        assert!(result.is_err(), "expected error, got {result:?}");
    }
}
