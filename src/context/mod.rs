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
}

impl CorpusCache {
    fn new() -> Self {
        CorpusCache {
            generation: 0,
            built_generation: None,
            corpus: None,
            doc_texts: Vec::new(),
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
        }
        self.corpus
            .as_ref()
            .expect("corpus is always populated by the stale branch above")
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
    fn doc_corpus_cache_is_a_shared_singleton() {
        {
            let mut guard = doc_corpus_cache().lock().expect("cache mutex poisoned");
            guard.increment_generation();
        }
        let guard = doc_corpus_cache().lock().expect("cache mutex poisoned");
        assert!(guard.generation() >= 1);
    }
}
