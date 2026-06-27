//! Similarity scoring: Jaccard (symmetric, for dedup) and BM25 (asymmetric,
//! for query relevance). Both share the crate's tokenizer so the same notion of
//! "term" drives dedup and retrieval.

use std::collections::{HashMap, HashSet};

use crate::tokenize::tokenize;

/// BM25 term-frequency saturation parameter (standard default).
const BM25_K1: f64 = 1.2;
/// BM25 length-normalization parameter (standard default).
const BM25_B: f64 = 0.75;

/// Jaccard similarity of two texts: `|A ∩ B| / |A ∪ B|` over their token
/// **sets**. Symmetric, in `[0, 1]`. Used to answer "are these the same
/// memory?". Two empty texts are defined as identical (1.0); an empty vs.
/// non-empty pair is 0.0.
pub fn jaccard(a: &str, b: &str) -> f64 {
    let sa: HashSet<String> = tokenize(a).into_iter().collect();
    let sb: HashSet<String> = tokenize(b).into_iter().collect();
    jaccard_sets(&sa, &sb)
}

/// Jaccard over pre-tokenized sets (avoids re-tokenizing in clustering loops).
pub fn jaccard_sets(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count();
    let union = a.len() + b.len() - inter;
    if union == 0 {
        0.0
    } else {
        inter as f64 / union as f64
    }
}

/// Convenience: token set for one text (deduplicated). Useful for callers doing
/// many pairwise Jaccard comparisons.
pub fn token_set(text: &str) -> HashSet<String> {
    tokenize(text).into_iter().collect()
}

/// A BM25 corpus built over a fixed set of documents. Rebuild per query batch —
/// for the handoff use case (tens to low hundreds of memories) this is
/// sub-millisecond and avoids any persistent-index staleness.
pub struct Corpus {
    /// Per-document term-frequency maps.
    doc_tf: Vec<HashMap<String, u32>>,
    /// Per-document length (total token count).
    doc_len: Vec<f64>,
    /// Document frequency: how many docs contain each term.
    df: HashMap<String, u32>,
    /// Mean document length.
    avgdl: f64,
    /// Number of documents.
    n: usize,
}

impl Corpus {
    /// Build a corpus from document texts. Document `i` in the input maps to
    /// index `i` in [`bm25_scores`](Corpus::bm25_scores) output.
    pub fn build(docs: &[String]) -> Self {
        let mut doc_tf = Vec::with_capacity(docs.len());
        let mut doc_len = Vec::with_capacity(docs.len());
        let mut df: HashMap<String, u32> = HashMap::new();
        let mut total_len = 0.0;

        for doc in docs {
            let toks = tokenize(doc);
            let mut tf: HashMap<String, u32> = HashMap::new();
            for t in &toks {
                *tf.entry(t.clone()).or_insert(0) += 1;
            }
            for term in tf.keys() {
                *df.entry(term.clone()).or_insert(0) += 1;
            }
            doc_len.push(toks.len() as f64);
            total_len += toks.len() as f64;
            doc_tf.push(tf);
        }

        let n = docs.len();
        let avgdl = if n > 0 { total_len / n as f64 } else { 0.0 };

        Corpus {
            doc_tf,
            doc_len,
            df,
            avgdl,
            n,
        }
    }

    /// Number of documents in the corpus.
    pub fn len(&self) -> usize {
        self.n
    }

    /// True when the corpus has no documents.
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// BM25 score of `query` against every document, returned in document order
    /// (index `i` = score of document `i` passed to [`build`](Corpus::build)).
    /// Scores are non-negative; 0.0 means no query term occurs in the document.
    pub fn bm25_scores(&self, query: &str) -> Vec<f64> {
        let query_terms = tokenize(query);
        self.bm25_scores_tokens(&query_terms)
    }

    /// BM25 over pre-tokenized query terms (lets callers inject extra terms,
    /// e.g. file basenames, without re-tokenizing).
    pub fn bm25_scores_tokens(&self, query_terms: &[String]) -> Vec<f64> {
        let mut scores = vec![0.0; self.n];
        if self.n == 0 || self.avgdl == 0.0 {
            return scores;
        }

        // Deduplicate query terms — repeated query terms shouldn't multiply a
        // document's score beyond BM25's per-term contribution.
        let unique_terms: HashSet<&String> = query_terms.iter().collect();

        for term in unique_terms {
            let df = match self.df.get(term) {
                Some(&df) if df > 0 => df as f64,
                _ => continue,
            };
            // BM25 IDF with the +1 inside the log to keep it non-negative.
            let idf = (((self.n as f64 - df + 0.5) / (df + 0.5)) + 1.0).ln();

            for (i, tf_map) in self.doc_tf.iter().enumerate() {
                let tf = match tf_map.get(term) {
                    Some(&tf) => tf as f64,
                    None => continue,
                };
                let dl = self.doc_len[i];
                let denom = tf + BM25_K1 * (1.0 - BM25_B + BM25_B * dl / self.avgdl);
                scores[i] += idf * (tf * (BM25_K1 + 1.0)) / denom;
            }
        }
        scores
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jaccard_identical_is_one() {
        assert!((jaccard("hello world", "hello world") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn jaccard_disjoint_is_low() {
        // Completely different content scores low (CL n-grams may add a tiny
        // overlap, but it must be well under 0.5).
        assert!(jaccard("hello world", "zzz qqq") < 0.2);
    }

    #[test]
    fn jaccard_partial_overlap() {
        let s = jaccard("atomic write always", "atomic write never");
        assert!(s > 0.3 && s < 1.0, "got {s}");
    }

    #[test]
    fn jaccard_empty_both_identical() {
        assert_eq!(jaccard("", ""), 1.0);
    }

    #[test]
    fn jaccard_empty_one_side_zero() {
        assert_eq!(jaccard("hello", ""), 0.0);
    }

    #[test]
    fn jaccard_japanese_near_duplicate() {
        let s = jaccard(
            "メモリ機能はセッション間で教訓を引き継ぐ",
            "メモリ機能はセッション間で教訓を保持する",
        );
        assert!(s > 0.5, "near-duplicate JP should score high, got {s}");
    }

    #[test]
    fn bm25_ranks_relevant_doc_first() {
        let docs = vec![
            "always use atomic_write for handoff files".to_string(),
            "the cat sat on the mat in the sun".to_string(),
            "configure the milestone schedule and assignee".to_string(),
        ];
        let corpus = Corpus::build(&docs);
        let scores = corpus.bm25_scores("atomic write");
        // Doc 0 must outrank the others.
        assert!(scores[0] > scores[1]);
        assert!(scores[0] > scores[2]);
    }

    #[test]
    fn bm25_japanese_query() {
        let docs = vec![
            "メモリ機能はセッション間で教訓を引き継ぐ".to_string(),
            "ガントチャートでスケジュールを表示する".to_string(),
        ];
        let corpus = Corpus::build(&docs);
        let scores = corpus.bm25_scores("メモリ");
        assert!(scores[0] > scores[1], "JP query should match doc 0");
    }

    #[test]
    fn bm25_no_match_is_zero() {
        let docs = vec!["hello world".to_string()];
        let corpus = Corpus::build(&docs);
        let scores = corpus.bm25_scores("completely unrelated query xyzzy");
        assert_eq!(scores[0], 0.0);
    }

    #[test]
    fn bm25_empty_corpus() {
        let corpus = Corpus::build(&[]);
        assert!(corpus.is_empty());
        assert!(corpus.bm25_scores("anything").is_empty());
    }

    #[test]
    fn bm25_cross_language_identifier_match() {
        // An English identifier embedded in a Japanese memory is found by an
        // English query via shared word tokens.
        let docs = vec![
            "atomic_write を必ず使う（torn read 防止）".to_string(),
            "ガントチャートの表示設定".to_string(),
        ];
        let corpus = Corpus::build(&docs);
        let scores = corpus.bm25_scores("atomic_write");
        assert!(scores[0] > scores[1]);
        assert!(scores[0] > 0.0);
    }
}
