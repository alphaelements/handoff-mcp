//! Shared ranking + corpus-cache infrastructure for full-text search across
//! features (`memory_query` today, `doc_query` from t96.3).
//!
//! See `wiki/130-document-management.md` §3.1 for the design rationale: a
//! single BM25 corpus cache, invalidated by a generation counter bumped on
//! every mutating call (`doc_save`/`doc_delete`/`doc_import`), avoids
//! rebuilding the corpus on every query once fragment counts reach the
//! thousands. `memory_query` stays on the un-cached path (memory counts are
//! small enough that a per-call rebuild is cheap) and only adopts the shared
//! ranking function from [`injection`].

pub mod injection;

use std::sync::{Mutex, OnceLock};

/// Process-wide cache of the document-fragment BM25 corpus, invalidated by a
/// monotonically increasing generation counter.
///
/// `doc_save`/`doc_delete`/`doc_import` call [`CorpusCache::increment_generation`]
/// on every mutation; the next [`CorpusCache::get_or_build_corpus`] call
/// notices its cached generation is stale and rebuilds. The MCP server is
/// single-threaded stdio today, so the `Mutex` is uncontended in practice —
/// it exists for forward compatibility with a future multi-session server.
pub struct CorpusCache {
    generation: u64,
    built_generation: Option<u64>,
    corpus: Option<lexsim::Corpus>,
    doc_texts: Vec<String>,
    /// Bumped every time [`Self::get_or_build_corpus`] actually rebuilds the
    /// corpus (inside its `stale` branch) — including a rebuild triggered by
    /// `doc_texts` changing with `generation` unchanged, which
    /// `built_generation` alone cannot distinguish from "still fresh" (both
    /// would read the same `Some(self.generation)` value before and after).
    /// [`Self::get_or_build_corpus_and_embeddings`] compares this against
    /// [`Self::embeddings_rebuild_count`] to know whether the cached
    /// `doc_embeddings` are still aligned with the current corpus.
    rebuild_count: u64,
    /// The [`Self::rebuild_count`] value at which `doc_embeddings` was last
    /// computed. `None` before the first embeddings build.
    embeddings_rebuild_count: Option<u64>,
    /// Per-document semantic embeddings, index-aligned with `doc_texts`,
    /// computed lazily by [`Self::get_or_build_corpus_and_embeddings`] (t230.5,
    /// wiki/170-lexsim-hybrid-integration.md — doc_query hybrid scoring).
    doc_embeddings: Vec<Vec<f32>>,
}

impl CorpusCache {
    fn new() -> Self {
        CorpusCache {
            generation: 0,
            built_generation: None,
            corpus: None,
            doc_texts: Vec::new(),
            rebuild_count: 0,
            embeddings_rebuild_count: None,
            doc_embeddings: Vec::new(),
        }
    }

    /// Invalidate the cached corpus. Called after any mutation to the
    /// underlying fragment store (`doc_save`, `doc_delete`, `doc_import`).
    pub fn increment_generation(&mut self) {
        self.generation += 1;
    }

    /// Return the cached corpus if it is still current for `doc_texts`,
    /// otherwise rebuild it from `doc_texts` and cache the result.
    ///
    /// `doc_texts` is the caller's current full set of fragment index texts,
    /// supplied fresh on every call (the cache does not own fragment
    /// storage) — only the expensive `lexsim::Corpus::build` step is skipped
    /// when the generation hasn't moved since the last build.
    pub fn get_or_build_corpus(&mut self, doc_texts: &[String]) -> &lexsim::Corpus {
        let stale = self.built_generation != Some(self.generation) || self.doc_texts != doc_texts;
        if stale {
            self.corpus = Some(lexsim::Corpus::build_weighted(doc_texts));
            self.doc_texts = doc_texts.to_vec();
            self.built_generation = Some(self.generation);
            self.rebuild_count += 1;
        }
        self.corpus
            .as_ref()
            .expect("corpus is always populated by the stale branch above")
    }

    /// Like [`Self::get_or_build_corpus`], but also returns the per-document
    /// semantic embeddings for `doc_texts` (t230.5, `doc_query`'s hybrid
    /// BM25 + semantic ranking — see
    /// `crate::context::injection::rank_with_cached_semantic`).
    ///
    /// The embeddings are recomputed only when the corpus itself was
    /// actually rebuilt since the last embeddings computation (tracked via
    /// [`Self::rebuild_count`]/[`Self::embeddings_rebuild_count`], not the
    /// outer `generation` counter directly — see `rebuild_count`'s doc
    /// comment for why). Recomputing is a full pass over every
    /// `doc_texts` entry (`SemanticModelView::embed`, a hash-feature
    /// computation on the order of tens of microseconds per fragment per
    /// wiki/170 §2's `memory_query` measurement) rather than a per-entry
    /// diff — acceptable at `doc_query`'s fragment cardinality (perf_budget's
    /// L scale tops out at a few hundred fragments) and simpler than tracking
    /// per-fragment `(path, len, mtime_ns)` staleness keys, which would only
    /// pay for itself at a fragment count far beyond what a single project's
    /// document corpus reaches in practice.
    pub fn get_or_build_corpus_and_embeddings(
        &mut self,
        doc_texts: &[String],
        model: &lexsim::semantic::SemanticModelView,
    ) -> (&lexsim::Corpus, &[Vec<f32>]) {
        self.get_or_build_corpus(doc_texts);
        if self.embeddings_rebuild_count != Some(self.rebuild_count) {
            let dim = model.dimension();
            self.doc_embeddings = doc_texts
                .iter()
                .map(|t| model.embed(t).unwrap_or_else(|_| vec![0.0; dim]))
                .collect();
            self.embeddings_rebuild_count = Some(self.rebuild_count);
        }
        (
            self.corpus
                .as_ref()
                .expect("corpus is always populated by get_or_build_corpus above"),
            &self.doc_embeddings,
        )
    }

    /// Current generation counter (test/inspection hook).
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

impl Default for CorpusCache {
    fn default() -> Self {
        Self::new()
    }
}

static DOC_CORPUS_CACHE: OnceLock<Mutex<CorpusCache>> = OnceLock::new();

/// The process-wide document corpus cache, created lazily on first access.
pub fn doc_corpus_cache() -> &'static Mutex<CorpusCache> {
    DOC_CORPUS_CACHE.get_or_init(|| Mutex::new(CorpusCache::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_or_build_corpus_reuses_cache_when_generation_unchanged() {
        let mut cache = CorpusCache::new();
        let texts = vec!["alpha beta".to_string(), "gamma delta".to_string()];
        {
            let corpus = cache.get_or_build_corpus(&texts);
            assert_eq!(corpus.len(), 2);
        }
        assert_eq!(cache.built_generation, Some(0));
        // Second call with identical inputs and generation should not rebuild
        // (rebuild is observable only via built_generation staying put).
        let _ = cache.get_or_build_corpus(&texts);
        assert_eq!(cache.built_generation, Some(0));
    }

    #[test]
    fn increment_generation_forces_rebuild() {
        let mut cache = CorpusCache::new();
        let texts = vec!["alpha".to_string()];
        let _ = cache.get_or_build_corpus(&texts);
        assert_eq!(cache.built_generation, Some(0));

        cache.increment_generation();
        assert_eq!(cache.generation(), 1);

        let texts2 = vec!["alpha".to_string(), "beta".to_string()];
        let corpus = cache.get_or_build_corpus(&texts2);
        assert_eq!(corpus.len(), 2);
        assert_eq!(cache.built_generation, Some(1));
    }

    #[test]
    fn get_or_build_corpus_and_embeddings_returns_one_embedding_per_doc_text() {
        let mut cache = CorpusCache::new();
        let texts = vec![
            "rust ownership and borrow checker".to_string(),
            "javascript promises and async await".to_string(),
        ];
        let model = crate::semantic::semantic_model();
        let (corpus, embeddings) = cache.get_or_build_corpus_and_embeddings(&texts, model);
        assert_eq!(corpus.len(), 2);
        assert_eq!(embeddings.len(), 2);
        assert_eq!(embeddings[0].len(), model.dimension());
        assert_eq!(embeddings[1].len(), model.dimension());
    }

    #[test]
    fn get_or_build_corpus_and_embeddings_reuses_cache_when_unchanged() {
        let mut cache = CorpusCache::new();
        let texts = vec!["alpha beta".to_string()];
        let model = crate::semantic::semantic_model();
        let _ = cache.get_or_build_corpus_and_embeddings(&texts, model);
        assert_eq!(cache.embeddings_rebuild_count, Some(1));

        // Second call with identical inputs/generation must not recompute
        // (observable via embeddings_rebuild_count staying put, mirroring
        // built_generation's role in get_or_build_corpus's own test).
        let _ = cache.get_or_build_corpus_and_embeddings(&texts, model);
        assert_eq!(cache.embeddings_rebuild_count, Some(1));
    }

    #[test]
    fn get_or_build_corpus_and_embeddings_recomputes_when_doc_texts_change_without_generation_bump()
    {
        // Regression: `built_generation` alone can't distinguish "still
        // fresh" from "rebuilt for a new doc_texts set at the same
        // generation" (see `rebuild_count`'s doc comment) — this exercises
        // exactly that path.
        let mut cache = CorpusCache::new();
        let model = crate::semantic::semantic_model();
        let texts_a = vec!["alpha".to_string()];
        let _ = cache.get_or_build_corpus_and_embeddings(&texts_a, model);
        assert_eq!(cache.embeddings_rebuild_count, Some(1));

        let texts_b = vec!["alpha".to_string(), "beta".to_string()];
        let (_corpus, embeddings) = cache.get_or_build_corpus_and_embeddings(&texts_b, model);
        assert_eq!(embeddings.len(), 2);
        assert_eq!(cache.embeddings_rebuild_count, Some(2));
    }

    #[test]
    fn get_or_build_corpus_and_embeddings_recomputes_after_generation_bump() {
        let mut cache = CorpusCache::new();
        let model = crate::semantic::semantic_model();
        let texts = vec!["alpha".to_string()];
        let _ = cache.get_or_build_corpus_and_embeddings(&texts, model);
        assert_eq!(cache.embeddings_rebuild_count, Some(1));

        cache.increment_generation();
        let _ = cache.get_or_build_corpus_and_embeddings(&texts, model);
        assert_eq!(
            cache.embeddings_rebuild_count,
            Some(2),
            "a generation bump must force a rebuild even with unchanged doc_texts"
        );
    }

    #[test]
    fn doc_corpus_cache_is_a_shared_singleton() {
        {
            let mut guard = doc_corpus_cache().lock().expect("cache mutex poisoned");
            guard.increment_generation();
        }
        let guard = doc_corpus_cache().lock().expect("cache mutex poisoned");
        assert!(guard.generation() >= 1);
    }
}
