//! `lexsim` — a dictionary-free, multilingual lexical similarity engine.
//!
//! It answers two questions with one shared tokenizer:
//!
//! - **"Are these the same?"** → [`jaccard`] (symmetric set similarity), used to
//!   detect near-duplicate memories before saving.
//! - **"Is this relevant to that?"** → [`Corpus::bm25_scores`] (asymmetric
//!   ranking), used to pull the memories relevant to the current prompt/file.
//!
//! And a stable [`content_hash`] for change-detection / re-injection tracking.
//!
//! # Why dictionary-free
//!
//! Morphological dictionaries (e.g. for Japanese) are multi-megabyte and
//! language-specific. `lexsim` instead combines Unicode-standard techniques:
//!
//! - **UAX#29 word boundaries** for space-delimited scripts,
//! - **CJK character bi-grams** (the Apache Lucene approach) for non-spacing
//!   scripts (Japanese / Chinese / Korean),
//! - **NFKC normalization** to unify full/half-width and variant forms,
//! - **CL-CnG** (Cross-Language Character N-Grams) so identifiers, proper nouns
//!   and spelling variants match across languages.
//!
//! The result reaches dictionary-like recall with zero dictionary, in
//! sub-millisecond time for the corpus sizes this targets (tens to hundreds of
//! short documents).
//!
//! # Examples
//!
//! Detect a near-duplicate before saving (symmetric [`jaccard`]):
//!
//! ```
//! use lexsim::jaccard;
//!
//! let existing = "always use atomic_write for handoff files";
//! let incoming = "always use atomic_write when writing handoff files";
//! assert!(jaccard(existing, incoming) > 0.5); // clearly a near-duplicate
//!
//! let unrelated = "the cat sat on the mat";
//! assert!(jaccard(existing, unrelated) < 0.2);
//! ```
//!
//! Rank stored memories by relevance to a query (asymmetric BM25 via [`Corpus`]):
//!
//! ```
//! use lexsim::Corpus;
//!
//! let memories = vec![
//!     "always use atomic_write for handoff files".to_string(),
//!     "configure the milestone schedule and assignee".to_string(),
//! ];
//! let corpus = Corpus::build(&memories);
//! let scores = corpus.bm25_scores("atomic write");
//! assert!(scores[0] > scores[1]); // memory 0 is the most relevant
//! ```
//!
//! The tokenizer is dictionary-free, so a Japanese query matches a Japanese
//! memory — and an English identifier embedded in Japanese text is still found:
//!
//! ```
//! use lexsim::Corpus;
//!
//! let memories = vec![
//!     "atomic_write を必ず使う（torn read 防止）".to_string(),
//!     "ガントチャートの表示設定".to_string(),
//! ];
//! let corpus = Corpus::build(&memories);
//! assert!(corpus.bm25_scores("メモリ atomic_write")[0] > 0.0);
//! ```
//!
//! # Future extension
//!
//! Purely lexical matching is weak on *cross-language synonyms* (the same idea
//! expressed in two languages with no shared tokens). The [`Scorer`] trait
//! marks where an embedding-based stage could be added later without touching
//! callers; only the lexical scorer is implemented today.

mod hash;
mod score;
mod tokenize;

pub use hash::{content_hash, fnv1a_hex};
pub use score::{jaccard, jaccard_sets, token_set, Corpus};
pub use tokenize::{normalize, tokenize};

/// Abstraction over "score these documents against this query". Today the only
/// implementation is lexical BM25 ([`LexicalScorer`]); the trait exists so an
/// embedding-based scorer can be slotted in later behind the same call site.
pub trait Scorer {
    /// Score every document (in the order given to the scorer) against `query`.
    fn scores(&self, query: &str) -> Vec<f64>;
}

/// BM25 lexical scorer over a [`Corpus`].
pub struct LexicalScorer {
    corpus: Corpus,
}

impl LexicalScorer {
    /// Build a lexical scorer from document texts.
    pub fn new(docs: &[String]) -> Self {
        LexicalScorer {
            corpus: Corpus::build(docs),
        }
    }

    /// Access the underlying corpus (e.g. for token-level queries).
    pub fn corpus(&self) -> &Corpus {
        &self.corpus
    }
}

impl Scorer for LexicalScorer {
    fn scores(&self, query: &str) -> Vec<f64> {
        self.corpus.bm25_scores(query)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scorer_trait_object_works() {
        let docs = vec!["atomic write".to_string(), "cat mat".to_string()];
        let scorer: Box<dyn Scorer> = Box::new(LexicalScorer::new(&docs));
        let scores = scorer.scores("atomic");
        assert!(scores[0] > scores[1]);
    }

    #[test]
    fn public_api_surface_callable() {
        // Smoke test that the re-exports compile and run together.
        let _ = tokenize("hello");
        let _ = normalize("HELLO");
        let _ = jaccard("a", "b");
        let _ = content_hash("a");
        let _ = fnv1a_hex(b"a");
        let _ = token_set("a b c");
        let c = Corpus::build(&["a b".to_string()]);
        let _ = c.bm25_scores("a");
    }
}
