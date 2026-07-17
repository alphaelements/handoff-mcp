//! Synthetic benchmark for the shared BM25 ranking module (`t93`).
//!
//! Measures corpus-build time and query latency at fragment-store scale
//! (2,000 / 10,000 synthetic fragments, lorem-ipsum + Japanese mixed text) to
//! validate the "file-only, no DB" premise from
//! `wiki/130-document-management.md` §3.1 before `doc_query` (t96.3) is
//! built on top of it.
//!
//! `#[ignore]`d — run manually with:
//! `cargo test --test context_corpus_bench -- --ignored --nocapture`

use handoff_mcp::context::injection::{rank_by_bm25_and_scope, RankConfig};
use handoff_mcp::context::CorpusCache;
use std::time::Instant;

const LOREM_WORDS: &[&str] = &[
    "lorem",
    "ipsum",
    "dolor",
    "sit",
    "amet",
    "consectetur",
    "adipiscing",
    "elit",
    "sed",
    "do",
    "eiusmod",
    "tempor",
    "incididunt",
    "ut",
    "labore",
    "et",
    "dolore",
    "magna",
    "aliqua",
    "enim",
    "ad",
    "minim",
    "veniam",
    "quis",
    "nostrud",
    "exercitation",
    "ullamco",
    "laboris",
    "nisi",
    "aliquip",
    "commodo",
    "consequat",
    "duis",
    "aute",
    "irure",
    "reprehenderit",
    "voluptate",
    "velit",
    "esse",
    "cillum",
    "fugiat",
    "nulla",
    "pariatur",
];

const JAPANESE_WORDS: &[&str] = &[
    "セッション",
    "タスク",
    "設計",
    "実装",
    "テスト",
    "ドキュメント",
    "フラグメント",
    "検索",
    "スコア",
    "生成",
    "管理",
    "同期",
    "変更",
    "確認",
    "手順",
    "仕様",
    "構成",
    "品質",
    "性能",
    "計測",
];

/// Deterministic pseudo-random index generator (xorshift) so the benchmark is
/// reproducible across runs without pulling in a `rand` dependency.
struct Xorshift(u64);

impl Xorshift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[(self.next() as usize) % items.len()]
    }
}

/// Build `n` synthetic fragment texts mixing lorem-ipsum and Japanese tokens,
/// mimicking a document fragment's index text (title + body excerpt).
fn synthetic_fragments(n: usize) -> Vec<String> {
    let mut rng = Xorshift(0x9E3779B97F4A7C15 ^ n as u64);
    (0..n)
        .map(|i| {
            let mut words: Vec<&str> = Vec::with_capacity(40);
            for _ in 0..25 {
                words.push(rng.pick(LOREM_WORDS));
            }
            for _ in 0..15 {
                words.push(rng.pick(JAPANESE_WORDS));
            }
            format!("fragment {i} {}", words.join(" "))
        })
        .collect()
}

fn run_bench(n: usize) {
    let fragments = synthetic_fragments(n);
    let scope_paths: Vec<Vec<String>> = fragments.iter().map(|_| Vec::new()).collect();

    let mut cache = CorpusCache::default();

    let build_start = Instant::now();
    let corpus = cache.get_or_build_corpus(&fragments);
    let build_elapsed = build_start.elapsed();
    assert_eq!(corpus.len(), n);

    let query_tokens = lexsim::tokenize_weighted("設計 実装 lorem ipsum テスト");
    let config = RankConfig {
        min_score: 0.0,
        scope_path_bonus: 2.0,
        limit: 20,
    };

    let query_start = Instant::now();
    let ranked = rank_by_bm25_and_scope(
        cache.get_or_build_corpus(&fragments),
        &query_tokens,
        &scope_paths,
        &[],
        &config,
    );
    let query_elapsed = query_start.elapsed();

    println!(
        "[bench_corpus] n={n:>6}  build={build_elapsed:>10.2?}  query(cached)={query_elapsed:>10.2?}  results={}",
        ranked.len()
    );

    // 500ms budget per wiki §3.1's stated latency target for a single query.
    assert!(
        query_elapsed.as_millis() < 500,
        "query latency {query_elapsed:?} exceeded the 500ms budget for n={n}"
    );
}

#[test]
#[ignore = "manual benchmark — run with `cargo test --test context_corpus_bench -- --ignored --nocapture`"]
fn bench_corpus_2k() {
    run_bench(2_000);
}

#[test]
#[ignore = "manual benchmark — run with `cargo test --test context_corpus_bench -- --ignored --nocapture`"]
fn bench_corpus_10k() {
    run_bench(10_000);
}
