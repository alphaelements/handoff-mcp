//! Shared BM25 + scope-path ranking used by `memory_query` and (from t96.3)
//! `doc_query`.
//!
//! Extracted from the original `memory_query` implementation
//! (`src/mcp/handlers/memory.rs`) so both features rank candidates the same
//! way: BM25 relevance over a `lexsim::Corpus`, a fixed bonus when a
//! candidate's `scope_paths` prefix-matches one of the query's `file_paths`,
//! a `min_score` floor, then a stable sort + `limit` truncation.

/// One ranked candidate: the index into the caller's original slice, plus its
/// final score (BM25 + scope bonus).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RankItem {
    pub index: usize,
    pub score: f64,
}

/// Tuning knobs for [`rank_by_bm25_and_scope`] and [`rank_with_semantic`].
#[derive(Debug, Clone, Copy)]
pub struct RankConfig {
    /// Candidates scoring below this are dropped before sorting.
    /// [`rank_with_semantic`] additionally accepts a candidate whose
    /// semantic bonus alone clears its separate `semantic_min_score`
    /// parameter, even if the combined score is below `min_score`.
    pub min_score: f64,
    /// Relative threshold (0.0–1.0): after ranking, a candidate is dropped
    /// unless `score >= top_score * relative_threshold`. 0.0 disables.
    pub relative_threshold: f64,
    /// Added to a candidate's BM25 score when [`scope_matches`] is true.
    pub scope_path_bonus: f64,
    /// Max number of items returned (applied after sort, before session diff).
    pub limit: usize,
}

/// True if any `scope` prefix matches any `file` path (substring match on the
/// path, not a strict prefix — mirrors the original `memory_query` behavior).
pub fn scope_matches(scopes: &[String], files: &[String]) -> bool {
    if scopes.is_empty() || files.is_empty() {
        return false;
    }
    scopes
        .iter()
        .any(|scope| files.iter().any(|f| f.contains(scope.as_str())))
}

/// Rank every document in `corpus` against `query_tokens` via weighted BM25,
/// add `config.scope_path_bonus` when `scope_paths[i]` matches `file_paths`,
/// drop anything below `config.min_score`, sort descending by score, and
/// truncate to `config.limit`.
///
/// `scope_paths` and the corpus must be index-aligned (one entry per
/// document); `corpus.len()` and `scope_paths.len()` are expected to match —
/// a mismatch simply means the extra `scope_paths` entries are never
/// consulted (indices beyond `corpus.len()` are not produced).
pub fn rank_by_bm25_and_scope(
    corpus: &lexsim::Corpus,
    query_tokens: &[lexsim::WeightedToken],
    scope_paths: &[Vec<String>],
    file_paths: &[String],
    config: &RankConfig,
) -> Vec<RankItem> {
    let scores = corpus.bm25_scores_weighted_tokens(query_tokens);

    let mut ranked: Vec<RankItem> = scores
        .into_iter()
        .enumerate()
        .map(|(index, mut score)| {
            if let Some(scopes) = scope_paths.get(index) {
                if scope_matches(scopes, file_paths) {
                    score += config.scope_path_bonus;
                }
            }
            RankItem { index, score }
        })
        .filter(|item| item.score > 0.0 && item.score >= config.min_score)
        .collect();

    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Relative threshold: drop candidates whose score is below a fraction of
    // the top hit. Applied after sorting so `ranked[0]` is the best match.
    if config.relative_threshold > 0.0 {
        if let Some(top) = ranked.first() {
            let floor = top.score * config.relative_threshold;
            ranked.retain(|item| item.score >= floor);
        }
    }

    ranked.truncate(config.limit);
    ranked
}

/// Drop already-injected candidates (per the caller's session sidecar) from
/// `ranked`, then truncate the survivors to `limit`.
///
/// `already_injected(index)` receives the original document index (as stored
/// on [`RankItem::index`]) and returns true when that document was already
/// injected into the current session at its current content hash.
pub fn filter_already_injected<F>(
    ranked: Vec<RankItem>,
    already_injected: F,
    limit: usize,
) -> Vec<RankItem>
where
    F: Fn(usize) -> bool,
{
    ranked
        .into_iter()
        .filter(|item| !already_injected(item.index))
        .take(limit)
        .collect()
}

/// Rank every document in `corpus` against `query_tokens` via weighted BM25,
/// add `config.scope_path_bonus` on scope match, and add a semantic bonus
/// derived from the cosine similarity between `query_text`'s embedding and
/// each `doc_texts[i]`'s embedding. Unlike [`rank_by_bm25_and_scope`], a
/// document with BM25 score 0 can still survive purely on its semantic
/// bonus — this is what enables cross-lingual recall (a memory written in
/// Japanese can match an English query with zero token overlap).
///
/// The semantic bonus is `semantic_weight * (cos + 1.0) / 2.0` when
/// `cos > 0.0`, and exactly `0.0` otherwise (`cos <= 0.0`). This asymmetry is
/// intentional: without it, a semantically *unrelated* document (cos ≈ 0)
/// would still receive `semantic_weight * 0.5`, which is enough to leak past
/// a small `min_score` and produce false positives.
///
/// A candidate survives if it clears **either** floor:
/// - `config.min_score` (the normal BM25 + scope + semantic combined score),
///   or
/// - `semantic_min_score`, compared against the semantic bonus **alone**
///   (before BM25/scope are added in).
///
/// Two independent floors exist because `config.min_score` is tuned as a
/// BM25-precision floor (production default 2.0 — see
/// `default_memory_query_min_score` in `src/storage/config.rs`, raised from
/// an earlier 0.1 specifically to filter noisy small-corpus BM25 matches).
/// `semantic_weight` is bounded (max bonus `semantic_weight` at cos=1.0,
/// default weight 1.0), so a purely semantic (BM25=0) match can never clear
/// `min_score=2.0` on its own — making cross-lingual recall unreachable at
/// the shipped default if it were gated by `min_score` alone. `semantic_min_score`
/// is a separate, lower floor that lets a strong semantic-only match through
/// even when the combined score can't clear the BM25 floor, without lowering
/// `min_score` itself (which would reopen the small-corpus BM25 false-positive
/// hole that floor was raised to close).
///
/// `doc_texts` and the corpus must be index-aligned (one entry per document);
/// a missing `doc_texts[i]` (index out of bounds) contributes no semantic
/// bonus for that document, mirroring how a missing `scope_paths[i]`
/// contributes no scope bonus. Embedding failures (`model.embed` returning
/// `Err`) fall back to a zero vector, which naturally yields `cos <= 0.0` and
/// thus a zero bonus — never a fabricated non-zero similarity.
#[allow(clippy::too_many_arguments)]
pub fn rank_with_semantic(
    corpus: &lexsim::Corpus,
    query_tokens: &[lexsim::WeightedToken],
    scope_paths: &[Vec<String>],
    file_paths: &[String],
    doc_texts: &[String],
    query_text: &str,
    model: &lexsim::semantic::SemanticModelView,
    config: &RankConfig,
    semantic_weight: f64,
    semantic_min_score: f64,
) -> Vec<RankItem> {
    let bm25_scores = corpus.bm25_scores_weighted_tokens(query_tokens);
    let dim = model.dimension();
    let query_emb = model.embed(query_text).unwrap_or_else(|_| vec![0.0; dim]);

    let mut items: Vec<RankItem> = bm25_scores
        .into_iter()
        .enumerate()
        .map(|(index, mut score)| {
            if let Some(scopes) = scope_paths.get(index) {
                if scope_matches(scopes, file_paths) {
                    score += config.scope_path_bonus;
                }
            }
            let mut semantic_bonus = 0.0;
            if let Some(doc_text) = doc_texts.get(index) {
                let doc_emb = model.embed(doc_text).unwrap_or_else(|_| vec![0.0; dim]);
                let cos: f32 = query_emb
                    .iter()
                    .zip(doc_emb.iter())
                    .map(|(a, b)| a * b)
                    .sum();
                if cos > 0.0 {
                    semantic_bonus = semantic_weight * ((cos as f64 + 1.0) / 2.0);
                    score += semantic_bonus;
                }
            }
            (RankItem { index, score }, semantic_bonus)
        })
        .filter(|(item, semantic_bonus)| {
            item.score >= config.min_score || *semantic_bonus >= semantic_min_score
        })
        .map(|(item, _)| item)
        .collect();

    items.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    if config.relative_threshold > 0.0 {
        if let Some(top) = items.first() {
            let floor = top.score * config.relative_threshold;
            items.retain(|item| item.score >= floor);
        }
    }

    items.truncate(config.limit);
    items
}

/// Rank fragments for `doc_query` via hybrid BM25 + semantic scoring, using
/// **precomputed** per-fragment embeddings (`CorpusCache::
/// get_or_build_corpus_and_embeddings`, t230.5) instead of recomputing an
/// embedding per fragment on every call the way [`rank_with_semantic`] does
/// for `memory_query`'s much smaller candidate count — `doc_query` runs on
/// every prompt/edit hook (wiki/240-performance-design.md §6 PR-5) over a
/// fragment-level corpus that can run into the hundreds, so a per-call
/// embed-everything pass is the cost this cached variant exists to avoid.
///
/// **Filter differs from [`rank_with_semantic`] by design.** That function's
/// filter (`item.score >= config.min_score || semantic_bonus >=
/// semantic_min_score`) is correct when `config.min_score` is itself a
/// meaningful positive floor — `memory_query`'s production default is 2.0,
/// comfortably above the semantic "noise ceiling" a wholly unrelated pair can
/// reach from shared function-word/particle structure alone (~0.6-0.73, see
/// `SEMANTIC_MIN_SCORE`'s doc comment in `memory.rs`). `doc_query`'s
/// `min_score` default is `0.0` (see `DOC_QUERY_MIN_SCORE` in
/// `docs_query.rs`) — the *real* precision floor `rank_by_bm25_and_scope`
/// enforced there was a hardcoded `score > 0.0`, not `config.min_score`.
/// Baking the semantic bonus into that same checked value would let the
/// near-universal noise-level bonus alone push almost every fragment's score
/// past `0.0`, destroying `doc_query`'s precision entirely. This function
/// therefore checks the **lexical+scope base score** (BM25 + scope bonus,
/// *before* the semantic bonus is added) against the `> 0.0 && >=
/// min_score` gate — identical to `rank_by_bm25_and_scope` — and keeps the
/// semantic bonus as a wholly separate escape hatch (`semantic_min_score`),
/// exactly as `rank_with_semantic` intends for cross-lingual recall, just
/// gated correctly for a near-zero base floor. The returned `RankItem::score`
/// still includes the semantic bonus for every surviving item, so it
/// continues to contribute to sort order and the relative-threshold cut.
///
/// `doc_embeddings` and the corpus must be index-aligned (one entry per
/// fragment); a missing `doc_embeddings[i]` (index out of bounds) contributes
/// no semantic bonus for that fragment, mirroring [`rank_with_semantic`]'s
/// handling of a missing `doc_texts[i]`.
///
/// **Second precision guard: `semantic_only_limit`.** A single absolute
/// floor on `semantic_bonus` is fragile on its own — review of t230.5 found
/// that even a well-calibrated floor leaves under a ~0.02 gap (in this bonus
/// space, on the fixture it was calibrated against — see `DOC_SEMANTIC_MIN_SCORE`'s
/// doc comment in `docs_query.rs` for the measured values), and `doc_query`
/// runs on *every* prompt/tool-use hook — an unrelated fragment slipping past
/// the floor on one query is far more consequential here than the same slip
/// in a human-invoked search. As defense in depth, at most `semantic_only_limit`
/// fragments that survive **only** via the semantic floor (i.e. whose
/// lexical+scope base score does not itself clear `config.min_score`) are
/// kept, highest-`semantic_bonus`-first; a fragment that also clears the
/// lexical+scope gate is never subject to this cap. This bounds how many
/// fragments a single noisy/borderline semantic match can inject per query,
/// without narrowing the floor itself (which would cut into genuine
/// cross-lingual recall instead).
///
/// **Third precision guard (round-3 rework): eligibility filtering upstream
/// of this function.** Round 2's whole-branch review found that the fixture
/// this floor and `semantic_only_limit` were calibrated against does not
/// capture the real noise shape: against this repository's own live
/// `.handoff/docs` corpus, empty (`seq`-0 preamble) and heading-only fragments
/// reached semantic-bonus scores *above* the fixture-calibrated floor for
/// wholly unrelated queries — no absolute-score floor or top-N cap on this
/// function's inputs alone could have caught that, since both guards operate
/// on scores these callers had already computed as "high". The caller
/// (`doc_query`'s `handle_doc_query`) now zeroes the embedding for any
/// fragment lacking real prose content *before* calling this function (see
/// `DOC_SEMANTIC_PROSE_MIN_TOKENS` in `docs_query.rs`), so such fragments
/// never reach a nonzero `semantic_bonus` here in the first place — this
/// function's two guards remain the defense for genuinely content-bearing
/// fragments.
#[allow(clippy::too_many_arguments)]
pub fn rank_with_cached_semantic(
    corpus: &lexsim::Corpus,
    query_tokens: &[lexsim::WeightedToken],
    scope_paths: &[Vec<String>],
    file_paths: &[String],
    doc_embeddings: &[Vec<f32>],
    query_text: &str,
    model: &lexsim::semantic::SemanticModelView,
    config: &RankConfig,
    semantic_weight: f64,
    semantic_min_score: f64,
    semantic_only_limit: usize,
) -> Vec<RankItem> {
    let bm25_scores = corpus.bm25_scores_weighted_tokens(query_tokens);
    let dim = model.dimension();
    let query_emb = model.embed(query_text).unwrap_or_else(|_| vec![0.0; dim]);

    let mut candidates: Vec<(RankItem, bool)> = bm25_scores
        .into_iter()
        .enumerate()
        .filter_map(|(index, mut base_score)| {
            if let Some(scopes) = scope_paths.get(index) {
                if scope_matches(scopes, file_paths) {
                    base_score += config.scope_path_bonus;
                }
            }
            let mut semantic_bonus = 0.0;
            if let Some(doc_emb) = doc_embeddings.get(index) {
                let cos: f32 = query_emb
                    .iter()
                    .zip(doc_emb.iter())
                    .map(|(a, b)| a * b)
                    .sum();
                if cos > 0.0 {
                    semantic_bonus = semantic_weight * ((cos as f64 + 1.0) / 2.0);
                }
            }
            let lexical_survivor = base_score > 0.0 && base_score >= config.min_score;
            let semantic_survivor = semantic_bonus >= semantic_min_score;
            if !lexical_survivor && !semantic_survivor {
                return None;
            }
            Some((
                RankItem {
                    index,
                    score: base_score + semantic_bonus,
                },
                // "semantic-only": survives purely on the semantic floor,
                // i.e. did NOT also clear the lexical+scope gate.
                !lexical_survivor,
            ))
        })
        .collect();

    candidates.sort_by(|a, b| {
        b.0.score
            .partial_cmp(&a.0.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    if config.relative_threshold > 0.0 {
        if let Some(top) = candidates.first() {
            let floor = top.0.score * config.relative_threshold;
            candidates.retain(|(item, _)| item.score >= floor);
        }
    }

    let mut semantic_only_kept = 0usize;
    let mut items: Vec<RankItem> = Vec::with_capacity(candidates.len());
    for (item, is_semantic_only) in candidates {
        if is_semantic_only {
            if semantic_only_kept >= semantic_only_limit {
                continue;
            }
            semantic_only_kept += 1;
        }
        items.push(item);
    }

    items.truncate(config.limit);
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn docs() -> Vec<String> {
        vec![
            "rust error handling with Result and anyhow".to_string(),
            "javascript promises and async await".to_string(),
            "rust ownership borrow checker ownership".to_string(),
        ]
    }

    fn default_config() -> RankConfig {
        RankConfig {
            min_score: 0.0,
            relative_threshold: 0.0,
            scope_path_bonus: 2.0,
            limit: 10,
        }
    }

    #[test]
    fn scope_matches_prefix() {
        let scopes = vec!["src/storage/".to_string()];
        let files = vec!["/repo/src/storage/mod.rs".to_string()];
        assert!(scope_matches(&scopes, &files));
        let files2 = vec!["/repo/src/mcp/mod.rs".to_string()];
        assert!(!scope_matches(&scopes, &files2));
    }

    #[test]
    fn scope_matches_empty_inputs_false() {
        assert!(!scope_matches(&[], &["a".to_string()]));
        assert!(!scope_matches(&["a".to_string()], &[]));
    }

    #[test]
    fn rank_by_bm25_orders_relevant_docs_first() {
        let corpus = lexsim::Corpus::build_weighted(&docs());
        let query_tokens = lexsim::tokenize_weighted("rust ownership");
        let scope_paths: Vec<Vec<String>> = vec![vec![], vec![], vec![]];
        let ranked =
            rank_by_bm25_and_scope(&corpus, &query_tokens, &scope_paths, &[], &default_config());

        assert!(!ranked.is_empty());
        assert_eq!(ranked[0].index, 2);
    }

    #[test]
    fn rank_by_bm25_applies_scope_path_bonus() {
        let corpus = lexsim::Corpus::build_weighted(&docs());
        let query_tokens = lexsim::tokenize_weighted("javascript");
        let scope_paths: Vec<Vec<String>> = vec![vec![], vec!["src/web/".to_string()], vec![]];
        let file_paths = vec!["/repo/src/web/app.js".to_string()];
        let config = RankConfig {
            min_score: 0.0,
            relative_threshold: 0.0,
            scope_path_bonus: 2.0,
            limit: 10,
        };
        let ranked =
            rank_by_bm25_and_scope(&corpus, &query_tokens, &scope_paths, &file_paths, &config);
        assert_eq!(ranked[0].index, 1);
        assert!(ranked[0].score >= 2.0);
    }

    #[test]
    fn rank_by_bm25_filters_below_min_score() {
        let corpus = lexsim::Corpus::build_weighted(&docs());
        let query_tokens = lexsim::tokenize_weighted("completely unrelated gibberish zzz");
        let scope_paths: Vec<Vec<String>> = vec![vec![], vec![], vec![]];
        let config = RankConfig {
            min_score: 0.01,
            relative_threshold: 0.0,
            scope_path_bonus: 2.0,
            limit: 10,
        };
        let ranked = rank_by_bm25_and_scope(&corpus, &query_tokens, &scope_paths, &[], &config);
        assert!(ranked.is_empty());
    }

    #[test]
    fn rank_by_bm25_respects_limit() {
        let corpus = lexsim::Corpus::build_weighted(&docs());
        let query_tokens = lexsim::tokenize_weighted("rust javascript");
        let scope_paths: Vec<Vec<String>> = vec![vec![], vec![], vec![]];
        let config = RankConfig {
            min_score: 0.0,
            relative_threshold: 0.0,
            scope_path_bonus: 2.0,
            limit: 1,
        };
        let ranked = rank_by_bm25_and_scope(&corpus, &query_tokens, &scope_paths, &[], &config);
        assert_eq!(ranked.len(), 1);
    }

    #[test]
    fn rank_by_bm25_applies_relative_threshold() {
        let corpus = lexsim::Corpus::build_weighted(&docs());
        let query_tokens = lexsim::tokenize_weighted("rust ownership");
        let scope_paths: Vec<Vec<String>> = vec![vec![], vec![], vec![]];
        let all =
            rank_by_bm25_and_scope(&corpus, &query_tokens, &scope_paths, &[], &default_config());
        assert!(all.len() >= 2, "need at least 2 results to test relative");
        let config_rel = RankConfig {
            min_score: 0.0,
            relative_threshold: 0.95,
            scope_path_bonus: 0.0,
            limit: 10,
        };
        let filtered =
            rank_by_bm25_and_scope(&corpus, &query_tokens, &scope_paths, &[], &config_rel);
        assert!(
            filtered.len() < all.len(),
            "relative threshold should drop low-scoring tail"
        );
        assert_eq!(filtered[0].index, all[0].index, "top hit must survive");
    }

    #[test]
    fn filter_already_injected_drops_marked_and_respects_limit() {
        let ranked = vec![
            RankItem {
                index: 0,
                score: 5.0,
            },
            RankItem {
                index: 1,
                score: 4.0,
            },
            RankItem {
                index: 2,
                score: 3.0,
            },
        ];
        let already = |i: usize| i == 1;
        let out = filter_already_injected(ranked, already, 10);
        assert_eq!(out.iter().map(|i| i.index).collect::<Vec<_>>(), vec![0, 2]);
    }

    #[test]
    fn rank_with_semantic_orders_relevant_docs_first_via_bm25() {
        // Same-language query: BM25 dominates, semantic bonus only adds on top.
        let corpus = lexsim::Corpus::build_weighted(&docs());
        let query_tokens = lexsim::tokenize_weighted("rust ownership");
        let scope_paths: Vec<Vec<String>> = vec![vec![], vec![], vec![]];
        let model = crate::semantic::semantic_model();
        let ranked = rank_with_semantic(
            &corpus,
            &query_tokens,
            &scope_paths,
            &[],
            &docs(),
            "rust ownership",
            model,
            &default_config(),
            1.0,
            0.0,
        );
        assert!(!ranked.is_empty());
        assert_eq!(ranked[0].index, 2);
    }

    #[test]
    fn rank_with_semantic_includes_zero_bm25_doc_via_semantic_bonus() {
        // A query with zero token overlap with any doc still surfaces the
        // semantically closest one, as long as min_score is low enough to be
        // cleared purely by the semantic bonus.
        let doc_list = vec![
            "rust ownership and borrow checker".to_string(),
            "completely unrelated gibberish zzz qqq".to_string(),
        ];
        let corpus = lexsim::Corpus::build_weighted(&doc_list);
        // Query text has no lexical overlap with either doc -> BM25 scores are 0.
        let query_text = "rust ownership and borrow checker";
        let query_tokens = lexsim::tokenize_weighted("xyzxyz nomatch token");
        let scope_paths: Vec<Vec<String>> = vec![vec![], vec![]];
        let model = crate::semantic::semantic_model();

        let bm25_only = corpus.bm25_scores_weighted_tokens(&query_tokens);
        assert!(
            bm25_only.iter().all(|s| *s == 0.0),
            "test setup: BM25 must be 0 for all docs given the mismatched query tokens"
        );

        let config = RankConfig {
            min_score: 0.01,
            relative_threshold: 0.0,
            scope_path_bonus: 2.0,
            limit: 10,
        };
        let ranked = rank_with_semantic(
            &corpus,
            &query_tokens,
            &scope_paths,
            &[],
            &doc_list,
            query_text,
            model,
            &config,
            1.0,
            0.0,
        );
        assert!(
            !ranked.is_empty(),
            "BM25=0 doc should still surface via semantic bonus when min_score is cleared"
        );
        assert_eq!(
            ranked[0].index, 0,
            "the doc identical to the query text should rank first via semantic similarity"
        );
    }

    #[test]
    fn rank_with_semantic_japanese_doc_english_query_gives_nonzero_score() {
        // Mirror of `rank_with_semantic_includes_zero_bm25_doc_via_semantic_bonus`
        // in the opposite language direction: a Japanese memory ranked
        // against an English query with zero lexical token overlap must
        // still receive a strictly positive score from the semantic bonus
        // alone (BM25 contributes nothing).
        let doc_list = vec!["セッション永続化にはatomic_writeを使うべきだ".to_string()];
        let corpus = lexsim::Corpus::build_weighted(&doc_list);
        let query_text = "how to persist session state safely";
        let query_tokens = lexsim::tokenize_weighted(query_text);
        let scope_paths: Vec<Vec<String>> = vec![vec![]];
        let model = crate::semantic::semantic_model();

        let bm25_only = corpus.bm25_scores_weighted_tokens(&query_tokens);
        assert!(
            bm25_only.iter().all(|s| *s == 0.0),
            "test setup: BM25 must be 0 given zero lexical overlap between the Japanese doc and English query"
        );

        let config = RankConfig {
            min_score: 0.01,
            relative_threshold: 0.0,
            scope_path_bonus: 2.0,
            limit: 10,
        };
        let ranked = rank_with_semantic(
            &corpus,
            &query_tokens,
            &scope_paths,
            &[],
            &doc_list,
            query_text,
            model,
            &config,
            1.0,
            0.0,
        );
        assert_eq!(
            ranked.len(),
            1,
            "the Japanese doc must surface for the English query via semantic bonus alone"
        );
        assert!(
            ranked[0].score > 0.0,
            "score must be strictly positive (BM25=0 + semantic bonus > 0), got {}",
            ranked[0].score
        );
    }

    #[test]
    fn rank_with_semantic_clears_production_min_score_via_semantic_only_floor() {
        // Regression for the round-1 BLOCKER: at the shipped production
        // default (`memory_query_min_score = 2.0`), a semantic bonus capped
        // at `semantic_weight` (1.0 at cos=1.0) can NEVER clear `min_score`
        // on its own, so cross-lingual recall (the documented purpose of this
        // function) was unreachable end-to-end. `semantic_min_score` is a
        // second, independent, and lower floor: a BM25=0 document still
        // surfaces if its semantic-only bonus alone clears
        // `semantic_min_score`, even though it can never clear the much
        // higher `config.min_score`. Uses the production `SEMANTIC_MIN_SCORE`
        // (0.87, mirrored here) and a pair with a shared anchor term
        // ("VSCode") — see `SEMANTIC_MIN_SCORE`'s doc comment in
        // src/mcp/handlers/memory.rs for why an anchor-free pair (e.g. plain
        // "session persistence") is not reliably above this floor.
        let doc_list = vec!["timer source of truth is the VSCode extension".to_string()];
        let corpus = lexsim::Corpus::build_weighted(&doc_list);
        // Cross-lingual query: zero token overlap with the doc -> BM25 = 0.
        let query_text = "タイマーの正本はVSCode拡張である";
        let query_tokens = lexsim::tokenize_weighted(query_text);
        let scope_paths: Vec<Vec<String>> = vec![vec![]];
        let model = crate::semantic::semantic_model();

        let bm25_only = corpus.bm25_scores_weighted_tokens(&query_tokens);
        assert!(
            bm25_only.iter().all(|s| *s < 2.0),
            "test setup: BM25 alone must not clear the production min_score"
        );

        // The real production default (src/storage/config.rs
        // default_memory_query_min_score) — not a test-only lowered value.
        let production_config = RankConfig {
            min_score: 2.0,
            relative_threshold: 0.3,
            scope_path_bonus: 2.0,
            limit: 10,
        };
        let ranked = rank_with_semantic(
            &corpus,
            &query_tokens,
            &scope_paths,
            &[],
            &doc_list,
            query_text,
            model,
            &production_config,
            1.0,
            0.87,
        );
        assert_eq!(
            ranked.len(),
            1,
            "cross-lingual match must surface at the production min_score default"
        );
        assert_eq!(ranked[0].index, 0);
    }

    #[test]
    fn rank_with_semantic_semantic_only_floor_still_excludes_weak_matches() {
        // False-positive guard: `semantic_min_score` only rescues a BM25=0
        // document whose semantic bonus clears that floor. A document whose
        // bonus falls short of both `config.min_score` (production 2.0) and
        // `semantic_min_score` must still be excluded — the new floor is not
        // a blanket bypass of filtering.
        let doc_list = vec!["completely unrelated gibberish zzz qqq".to_string()];
        let corpus = lexsim::Corpus::build_weighted(&doc_list);
        let query_text = "rust ownership and borrow checker semantics";
        let query_tokens = lexsim::tokenize_weighted("xyzxyz nomatch token");
        let scope_paths: Vec<Vec<String>> = vec![vec![]];
        let model = crate::semantic::semantic_model();

        let bm25_only = corpus.bm25_scores_weighted_tokens(&query_tokens);
        assert!(bm25_only.iter().all(|s| *s == 0.0));

        let production_config = RankConfig {
            min_score: 2.0,
            relative_threshold: 0.3,
            scope_path_bonus: 2.0,
            limit: 10,
        };
        // A semantic_min_score above what an unrelated pair's bonus can reach
        // (max possible bonus is `semantic_weight` = 1.0 at cos=1.0, so any
        // floor above 1.0 can never be cleared purely by chance).
        let ranked = rank_with_semantic(
            &corpus,
            &query_tokens,
            &scope_paths,
            &[],
            &doc_list,
            query_text,
            model,
            &production_config,
            1.0,
            1.0,
        );
        assert!(
            ranked.is_empty(),
            "unrelated BM25=0 doc must not surface when its semantic bonus can't clear either floor"
        );
    }

    #[test]
    fn rank_with_semantic_gives_zero_bonus_for_non_positive_cosine() {
        // A doc_text identical to the negation-inducing setup is hard to force
        // deterministically with a real model, so instead we verify the
        // contract directly: an out-of-range doc_texts index (no text
        // available) contributes exactly 0 bonus, matching the cos<=0 case,
        // and does not panic or fabricate a score.
        let doc_list = docs();
        let corpus = lexsim::Corpus::build_weighted(&doc_list);
        let query_tokens = lexsim::tokenize_weighted("rust ownership");
        let scope_paths: Vec<Vec<String>> = vec![vec![], vec![], vec![]];
        let model = crate::semantic::semantic_model();

        // Only supply doc_texts for index 0; indices 1 and 2 are missing and
        // must fall back to "no semantic bonus" rather than panicking.
        let partial_doc_texts = vec![doc_list[0].clone()];
        let config = RankConfig {
            min_score: 0.0,
            relative_threshold: 0.0,
            scope_path_bonus: 0.0,
            limit: 10,
        };
        let ranked = rank_with_semantic(
            &corpus,
            &query_tokens,
            &scope_paths,
            &[],
            &partial_doc_texts,
            "rust ownership",
            model,
            &config,
            1.0,
            0.0,
        );
        let raw_bm25 = corpus.bm25_scores_weighted_tokens(&query_tokens);

        let item1 = ranked.iter().find(|i| i.index == 1).unwrap();
        assert_eq!(
            item1.score, raw_bm25[1],
            "doc without a doc_texts entry must receive zero semantic bonus (score == raw BM25)"
        );
    }

    /// Mirrors `docs_query.rs`'s `DOC_QUERY_MIN_SCORE = 0.0` / `SCOPE_PATH_BONUS
    /// = 2.0` production defaults — the scenario `rank_with_cached_semantic`
    /// exists for.
    fn doc_query_like_config() -> RankConfig {
        RankConfig {
            min_score: 0.0,
            relative_threshold: 0.0,
            scope_path_bonus: 2.0,
            limit: 10,
        }
    }

    fn embed_all(texts: &[String], model: &lexsim::semantic::SemanticModelView) -> Vec<Vec<f32>> {
        let dim = model.dimension();
        texts
            .iter()
            .map(|t| model.embed(t).unwrap_or_else(|_| vec![0.0; dim]))
            .collect()
    }

    #[test]
    fn rank_with_cached_semantic_orders_relevant_docs_first_via_bm25() {
        let doc_texts = docs();
        let corpus = lexsim::Corpus::build_weighted(&doc_texts);
        let query_tokens = lexsim::tokenize_weighted("rust ownership");
        let scope_paths: Vec<Vec<String>> = vec![vec![], vec![], vec![]];
        let model = crate::semantic::semantic_model();
        let embeddings = embed_all(&doc_texts, model);

        let ranked = rank_with_cached_semantic(
            &corpus,
            &query_tokens,
            &scope_paths,
            &[],
            &embeddings,
            "rust ownership",
            model,
            &doc_query_like_config(),
            1.0,
            crate::mcp::handlers::docs_query::DOC_SEMANTIC_MIN_SCORE,
            crate::mcp::handlers::docs_query::DOC_SEMANTIC_ONLY_RESCUE_LIMIT,
        );
        assert!(!ranked.is_empty());
        assert_eq!(ranked[0].index, 2);
    }

    #[test]
    fn rank_with_cached_semantic_includes_cross_lingual_match_via_semantic_floor() {
        // Same cross-lingual pairing as
        // `rank_with_semantic_clears_production_min_score_via_semantic_only_floor`
        // in `memory.rs`'s test suite, at doc_query's production defaults
        // (min_score=0.0, and the *actual* production `DOC_SEMANTIC_MIN_SCORE`
        // rather than a hardcoded literal — a future miscalibration of that
        // constant must fail this test, not silently pass it) using
        // precomputed embeddings rather than `doc_texts`. `query_tokens` is
        // deliberately built from a nonsense string (not `query_text`) so the
        // BM25 axis is independently guaranteed zero, isolating the
        // semantic-floor path under test — same decoupling technique as
        // `rank_with_semantic_includes_zero_bm25_doc_via_semantic_bonus`.
        let doc_texts = vec![
            "timer source of truth is the VSCode extension".to_string(),
            "javascript promises and async await".to_string(),
        ];
        let corpus = lexsim::Corpus::build_weighted(&doc_texts);
        let query_text = "タイマーの正本はVSCode拡張である";
        let query_tokens = lexsim::tokenize_weighted("xyzxyz nomatch token");
        let scope_paths: Vec<Vec<String>> = vec![vec![], vec![]];
        let model = crate::semantic::semantic_model();
        let embeddings = embed_all(&doc_texts, model);

        let bm25_only = corpus.bm25_scores_weighted_tokens(&query_tokens);
        assert!(
            bm25_only.iter().all(|s| *s == 0.0),
            "test setup: BM25 must be 0 for all docs given the mismatched query tokens"
        );

        let ranked = rank_with_cached_semantic(
            &corpus,
            &query_tokens,
            &scope_paths,
            &[],
            &embeddings,
            query_text,
            model,
            &doc_query_like_config(),
            1.0,
            crate::mcp::handlers::docs_query::DOC_SEMANTIC_MIN_SCORE,
            crate::mcp::handlers::docs_query::DOC_SEMANTIC_ONLY_RESCUE_LIMIT,
        );
        assert_eq!(
            ranked.len(),
            1,
            "cross-lingual match must surface via the semantic-only floor at doc_query's min_score=0.0 default"
        );
        assert_eq!(ranked[0].index, 0);
    }

    #[test]
    fn rank_with_cached_semantic_excludes_unrelated_zero_bm25_doc_at_min_score_zero() {
        // The regression this function exists to prevent: at doc_query's
        // real `min_score = 0.0`, `rank_with_semantic`'s own filter
        // (`item.score >= config.min_score`) would trivially admit every
        // fragment, since a BM25=0 + noise-level semantic bonus score is
        // still >= 0.0. `rank_with_cached_semantic` must still exclude an
        // unrelated fragment whose semantic bonus falls short of
        // `semantic_min_score` — matching `rank_by_bm25_and_scope`'s
        // original `score > 0.0` precision floor. Same decoupled-tokens
        // setup as the cross-lingual test above, but with a
        // `semantic_min_score` this pairing's noise-level cosine cannot
        // clear (mirrors `rank_with_semantic_semantic_only_floor_still_excludes_weak_matches`).
        //
        // Shaped like a real `doc_query` fragment (`title + tags + heading +
        // body`, matching `docs_query.rs`'s `doc_texts` construction) rather
        // than a bare sentence: round-2 rework found that a bare-sentence
        // "unrelated" fixture's bonus (0.859) lands in an ambiguous zone
        // relative to the real production noise ceiling (~0.839, measured
        // against the actual fragment shape — see `DOC_SEMANTIC_MIN_SCORE`'s
        // doc comment) purely because it lacks the structural boilerplate
        // (title/heading repetition) every real fragment has, which
        // measurably shifts the embedding. A doc_texts-shaped fixture here
        // is a faithful regression guard against `DOC_SEMANTIC_MIN_SCORE`
        // drifting back into unsafe territory; a bare-sentence fixture is
        // not.
        let doc_texts = vec![
            "Completely Unrelated Doc   Gibberish Completely unrelated gibberish zzz qqq.\n"
                .to_string(),
        ];
        let corpus = lexsim::Corpus::build_weighted(&doc_texts);
        let query_text = "rust ownership and borrow checker semantics";
        let query_tokens = lexsim::tokenize_weighted("xyzxyz nomatch token");
        let scope_paths: Vec<Vec<String>> = vec![vec![]];
        let model = crate::semantic::semantic_model();
        let embeddings = embed_all(&doc_texts, model);

        let bm25_only = corpus.bm25_scores_weighted_tokens(&query_tokens);
        assert!(bm25_only.iter().all(|s| *s == 0.0));

        let ranked = rank_with_cached_semantic(
            &corpus,
            &query_tokens,
            &scope_paths,
            &[],
            &embeddings,
            query_text,
            model,
            &doc_query_like_config(),
            1.0,
            crate::mcp::handlers::docs_query::DOC_SEMANTIC_MIN_SCORE,
            crate::mcp::handlers::docs_query::DOC_SEMANTIC_ONLY_RESCUE_LIMIT,
        );
        assert!(
            ranked.is_empty(),
            "an unrelated BM25=0 fragment must not surface just because min_score=0.0 is trivially cleared"
        );
    }

    #[test]
    fn rank_with_cached_semantic_missing_embedding_contributes_zero_bonus() {
        let doc_texts = docs();
        let corpus = lexsim::Corpus::build_weighted(&doc_texts);
        let query_tokens = lexsim::tokenize_weighted("rust ownership");
        let scope_paths: Vec<Vec<String>> = vec![vec![], vec![], vec![]];
        let model = crate::semantic::semantic_model();
        // Only supply an embedding for index 0; indices 1 and 2 are missing.
        let partial_embeddings = vec![embed_all(&doc_texts, model)[0].clone()];
        let config = RankConfig {
            min_score: 0.0,
            relative_threshold: 0.0,
            scope_path_bonus: 0.0,
            limit: 10,
        };

        let ranked = rank_with_cached_semantic(
            &corpus,
            &query_tokens,
            &scope_paths,
            &[],
            &partial_embeddings,
            "rust ownership",
            model,
            &config,
            1.0,
            crate::mcp::handlers::docs_query::DOC_SEMANTIC_MIN_SCORE,
            crate::mcp::handlers::docs_query::DOC_SEMANTIC_ONLY_RESCUE_LIMIT,
        );
        let raw_bm25 = corpus.bm25_scores_weighted_tokens(&query_tokens);
        assert!(
            ranked.iter().all(|i| i.index != 1),
            "doc 1 (javascript, unrelated, missing embedding) must not survive on a zero bonus"
        );
        let item2 = ranked
            .iter()
            .find(|i| i.index == 2)
            .expect("doc 2 (rust, relevant via BM25) must survive despite a missing embedding");
        // `raw_bm25` is a second, independent call to `bm25_scores_weighted_tokens`:
        // its query-term iteration goes through a `HashMap` with a randomized
        // per-call hasher seed, so float summation order (and thus the exact
        // ULP) can differ from the call inside `rank_with_cached_semantic`
        // even though both are mathematically the same sum — hence an epsilon
        // comparison rather than exact equality.
        assert!(
            (item2.score - raw_bm25[2]).abs() < 1e-9,
            "doc without an embeddings entry must receive zero semantic bonus (score == raw BM25 + scope): got {} vs {}",
            item2.score,
            raw_bm25[2]
        );
    }

    /// Round-2 rework fix: `semantic_only_limit` bounds how many fragments
    /// per query may survive **purely** on the semantic floor, even when
    /// more than that many clear it. Three fragments share the exact same
    /// text as the query (`cos == 1.0`, tied bonus, no lexical overlap with
    /// `query_tokens`), so all three are "semantic-only" survivors; a
    /// fourth fragment matches `query_tokens` lexically and must never be
    /// capped regardless of the limit. `Vec::sort_by` is a stable sort, so
    /// among the tied semantic-only survivors the two kept are the first two
    /// in original index order (0, 1); index 2 is the one the cap drops.
    #[test]
    fn rank_with_cached_semantic_caps_semantic_only_survivors_at_limit() {
        let doc_texts = vec![
            "hello world foo bar".to_string(),
            "hello world foo bar".to_string(),
            "hello world foo bar".to_string(),
            "rust ownership borrow checker".to_string(),
        ];
        let corpus = lexsim::Corpus::build_weighted(&doc_texts);
        let query_tokens = lexsim::tokenize_weighted("rust ownership");
        let scope_paths: Vec<Vec<String>> = vec![vec![], vec![], vec![], vec![]];
        let model = crate::semantic::semantic_model();
        let embeddings = embed_all(&doc_texts, model);
        let query_text = "hello world foo bar";

        let ranked = rank_with_cached_semantic(
            &corpus,
            &query_tokens,
            &scope_paths,
            &[],
            &embeddings,
            query_text,
            model,
            &doc_query_like_config(),
            1.0,
            0.5,
            2,
        );

        assert!(
            ranked.iter().any(|i| i.index == 3),
            "the lexical (BM25) survivor must never be subject to the semantic-only cap: {ranked:?}"
        );
        let semantic_only_survivors: Vec<usize> =
            ranked.iter().map(|i| i.index).filter(|&i| i != 3).collect();
        assert_eq!(
            semantic_only_survivors.len(),
            2,
            "3 tied semantic-only fragments cleared the floor but semantic_only_limit=2 must cap them: {ranked:?}"
        );
        assert!(semantic_only_survivors.contains(&0));
        assert!(semantic_only_survivors.contains(&1));
        assert!(
            !semantic_only_survivors.contains(&2),
            "the 3rd tied semantic-only survivor must be the one dropped by the cap (stable-sort order): {ranked:?}"
        );
    }

    #[test]
    fn filter_already_injected_applies_limit_after_filtering() {
        let ranked = vec![
            RankItem {
                index: 0,
                score: 5.0,
            },
            RankItem {
                index: 1,
                score: 4.0,
            },
            RankItem {
                index: 2,
                score: 3.0,
            },
        ];
        let out = filter_already_injected(ranked, |_| false, 2);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].index, 0);
        assert_eq!(out[1].index, 1);
    }
}
