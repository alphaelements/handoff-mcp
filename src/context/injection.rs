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

/// Tuning knobs for [`rank_by_bm25_and_scope`].
#[derive(Debug, Clone, Copy)]
pub struct RankConfig {
    /// Candidates scoring below this are dropped before sorting.
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
