//! A scaled-down, deterministic version of the `similarity_audit` example that
//! runs under `cargo test` so the similarity quality bars are enforced in CI on
//! every commit (the example must be invoked manually). The full-scale audit
//! (tens of thousands of comparisons) lives in
//! `examples/similarity_audit.rs`; this asserts the same invariants on a few
//! thousand generated patterns to keep test time low.

use lexsim::{content_hash, jaccard, Corpus};

const DUP_THRESHOLD: f64 = 0.72;

struct Lcg(u64);
impl Lcg {
    fn new(seed: u64) -> Self {
        Lcg(seed)
    }
    fn next_u32(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next_u32() as usize) % n
    }
    fn chance(&mut self, pct: u32) -> bool {
        self.next_u32() % 100 < pct
    }
}

const TOPICS: &[&[&str]] = &[
    &[
        "always use atomic_write for handoff files to avoid torn reads",
        "use atomic_write for every handoff file so readers never see torn data",
        "to avoid torn reads, always write handoff files through atomic_write",
    ],
    &[
        "push with SSH; never embed a PAT in the git remote URL",
        "use SSH for git push and do not put a PAT token in the URL",
        "git push should go over SSH, never with a PAT embedded in the URL",
    ],
    &[
        "every leaf task requires a positive estimate_hours value",
        "leaf tasks must set estimate_hours greater than zero",
        "estimate_hours is mandatory and positive for leaf tasks",
    ],
    &[
        "メモリ機能はセッション間で教訓を引き継ぐ",
        "メモリ機能はセッションをまたいで教訓を保持する",
        "教訓をセッション間で引き継ぐためのメモリ機能",
    ],
    &[
        "git のプッシュは SSH を使い URL に PAT を埋め込まない",
        "プッシュは SSH で行い URL へ PAT を書かない",
        "URL に PAT を埋め込まず SSH でプッシュする",
    ],
    &[
        "一時ファイルは git 管理外の tmp ディレクトリに置く",
        "スクラッチは tmp に置き git では追跡しない",
        "作業用の一時ファイルは git ignore された tmp に置く",
    ],
];

fn near_dup_noise(rng: &mut Lcg, s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    let mut at_word_start = true;
    for ch in s.chars() {
        if ch == ' ' {
            if rng.chance(20) {
                out.push(' ');
            }
            out.push(' ');
            at_word_start = true;
            continue;
        }
        if at_word_start && ch.is_ascii_alphabetic() && rng.chance(30) {
            out.extend(ch.to_uppercase());
        } else {
            out.push(ch);
        }
        at_word_start = false;
    }
    if rng.chance(30) {
        out.push('.');
    }
    out
}

#[test]
fn near_duplicates_always_caught() {
    let mut rng = Lcg::new(0xABCD_0001);
    let mut total = 0usize;
    let mut hit = 0usize;
    let mut min_score = 1.0f64;
    for topic in TOPICS {
        for v in *topic {
            for _ in 0..200 {
                let a = near_dup_noise(&mut rng, v);
                let b = near_dup_noise(&mut rng, v);
                let s = jaccard(&a, &b);
                total += 1;
                if s >= DUP_THRESHOLD {
                    hit += 1;
                }
                if s < min_score {
                    min_score = s;
                }
            }
        }
    }
    let recall = hit as f64 / total as f64;
    assert!(
        recall >= 0.99,
        "near-dup recall too low: {recall:.4} (min score {min_score:.3}, {hit}/{total})"
    );
}

#[test]
fn distinct_memories_never_false_positive() {
    let mut rng = Lcg::new(0xABCD_0002);
    let mut total = 0usize;
    let mut false_pos = 0usize;
    let mut max_score = 0.0f64;
    for _ in 0..20000 {
        let ti = rng.below(TOPICS.len());
        let mut tj = rng.below(TOPICS.len());
        if tj == ti {
            tj = (tj + 1) % TOPICS.len();
        }
        let a = TOPICS[ti][rng.below(TOPICS[ti].len())];
        let b = TOPICS[tj][rng.below(TOPICS[tj].len())];
        let s = jaccard(&near_dup_noise(&mut rng, a), &near_dup_noise(&mut rng, b));
        total += 1;
        if s >= DUP_THRESHOLD {
            false_pos += 1;
        }
        if s > max_score {
            max_score = s;
        }
    }
    let rate = false_pos as f64 / total as f64;
    assert!(
        rate <= 0.01,
        "false-positive rate too high: {rate:.4} (max cross-topic score {max_score:.3})"
    );
}

#[test]
fn related_outscores_unrelated() {
    let mut rng = Lcg::new(0xABCD_0003);
    let mut total = 0usize;
    let mut correct = 0usize;
    for _ in 0..20000 {
        let ti = rng.below(TOPICS.len());
        let v = TOPICS[ti];
        let i = rng.below(v.len());
        let mut j = rng.below(v.len());
        if j == i {
            j = (j + 1) % v.len();
        }
        let same = jaccard(v[i], v[j]);

        let mut tk = rng.below(TOPICS.len());
        if tk == ti {
            tk = (tk + 1) % TOPICS.len();
        }
        let other = TOPICS[tk][rng.below(TOPICS[tk].len())];
        let diff = jaccard(v[i], other);

        total += 1;
        if same > diff {
            correct += 1;
        }
    }
    let sep = correct as f64 / total as f64;
    assert!(
        sep >= 0.95,
        "related>unrelated separation too low: {sep:.4}"
    );
}

#[test]
fn retrieval_ranks_planted_memory_high() {
    let mut rng = Lcg::new(0xABCD_0004);
    let docs: Vec<String> = TOPICS.iter().map(|t| t[0].to_string()).collect();
    let corpus = Corpus::build(&docs);

    let stop = [
        "the", "a", "an", "to", "for", "of", "and", "or", "in", "on", "with", "do", "not", "is",
        "are", "be", "it", "so", "that", "this", "every", "each", "you", "must",
    ];

    let mut total = 0usize;
    let mut rank1 = 0usize;
    let mut top3 = 0usize;
    for (pidx, topic) in TOPICS.iter().enumerate() {
        for variant in *topic {
            for _ in 0..100 {
                // Build a query from ~half the content words of this variant.
                let words: Vec<&str> = variant.split_whitespace().collect();
                let kept: Vec<&str> = words
                    .iter()
                    .copied()
                    .filter(|w| !stop.contains(&w.to_lowercase().as_str()))
                    .collect();
                let pool_src = if kept.is_empty() { &words } else { &kept };
                let mut pool = pool_src.clone();
                let target = (pool.len().max(2) / 2).max(2).min(pool.len());
                let mut chosen = Vec::new();
                for _ in 0..target {
                    if pool.is_empty() {
                        break;
                    }
                    let idx = rng.below(pool.len());
                    chosen.push(pool.remove(idx));
                }
                let q = chosen.join(" ");

                let scores = corpus.bm25_scores(&q);
                let planted = scores[pidx];
                let better = scores
                    .iter()
                    .enumerate()
                    .filter(|(i, &s)| *i != pidx && s > planted)
                    .count();
                let ge = scores
                    .iter()
                    .enumerate()
                    .filter(|(i, &s)| *i != pidx && s >= planted)
                    .count();
                total += 1;
                if planted > 0.0 && better == 0 {
                    rank1 += 1;
                }
                if planted > 0.0 && ge < 3 {
                    top3 += 1;
                }
            }
        }
    }
    let r1 = rank1 as f64 / total as f64;
    let t3 = top3 as f64 / total as f64;
    assert!(r1 >= 0.90, "retrieval rank-1 too low: {r1:.4}");
    assert!(t3 >= 0.98, "retrieval top-3 too low: {t3:.4}");
}

#[test]
fn content_hash_invariant_to_whitespace_and_word_case() {
    let mut rng = Lcg::new(0xABCD_0005);
    for topic in TOPICS {
        for v in *topic {
            let base = content_hash(v);
            for _ in 0..50 {
                // Whitespace + word-initial case noise only — must not change hash.
                let mut out = String::new();
                let mut at_word_start = true;
                for ch in v.chars() {
                    if ch == ' ' {
                        if rng.chance(30) {
                            out.push(' ');
                        }
                        out.push(' ');
                        at_word_start = true;
                        continue;
                    }
                    if at_word_start && ch.is_ascii_alphabetic() && rng.chance(40) {
                        out.extend(ch.to_uppercase());
                    } else {
                        out.push(ch);
                    }
                    at_word_start = false;
                }
                assert_eq!(
                    content_hash(&out),
                    base,
                    "hash changed under benign noise: {out:?}"
                );
            }
        }
    }
}
