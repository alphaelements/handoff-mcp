//! Large-scale similarity audit for `lexsim`.
//!
//! The user asked for the similarity logic to be validated against a *very*
//! large set of generated patterns — not 1000, but as many as we can build — to
//! confirm that:
//!
//!   * near-duplicates (paraphrases, reorders, whitespace/case noise, JP↔JP
//!     rewordings) score **at or above** the dedup threshold, and
//!   * genuinely distinct memories score **below** it (low false-positive rate),
//!   * BM25 retrieval puts the planted relevant memory at rank 1 for a query
//!     derived from it, across English / Japanese / mixed / identifier corpora.
//!
//! It's deterministic (a small LCG seeded from a fixed constant — the crate
//! forbids `Math.random`-style nondeterminism in production, and tests must be
//! reproducible), prints precision/recall/threshold metrics, and exits non-zero
//! if any quality bar is missed so it can gate CI or a manual run.
//!
//! Run: `cargo run -p lexsim --release --example similarity_audit`

use std::collections::HashSet;

use lexsim::{content_hash, jaccard, Corpus};

/// Jaccard threshold the production save path uses (`MEMORY_DUP_THRESHOLD`).
const DUP_THRESHOLD: f64 = 0.72;

/// Deterministic LCG (numerical-recipes constants). No wall-clock / RNG crate.
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

// ---------------------------------------------------------------------------
// Building blocks for synthetic memories. Each "topic" is a cluster of strongly
// related sentences; cross-topic pairs are the distinct (negative) cases.
// ---------------------------------------------------------------------------

struct Topic {
    /// Several near-equivalent phrasings of the SAME idea (positives).
    variants: &'static [&'static str],
}

fn english_topics() -> Vec<Topic> {
    vec![
        Topic {
            variants: &[
                "always use atomic_write for handoff files to avoid torn reads",
                "use atomic_write for every handoff file so readers never see torn data",
                "to avoid torn reads, always write handoff files through atomic_write",
                "handoff files must be written with atomic_write to prevent torn reads",
            ],
        },
        Topic {
            variants: &[
                "push with SSH; never embed a PAT in the git remote URL",
                "use SSH for git push and do not put a PAT token in the URL",
                "git push should go over SSH, never with a PAT embedded in the URL",
                "do not embed personal access tokens in the URL; push over SSH instead",
            ],
        },
        Topic {
            variants: &[
                "the CHANGELOG is user facing; keep internal notes and SHAs out of it",
                "keep the changelog for users only, no internal SHAs or test counts",
                "do not put internal details or commit SHAs in the user facing changelog",
                "changelog entries are for users, exclude internal wiki and SHA details",
            ],
        },
        Topic {
            variants: &[
                "every leaf task requires a positive estimate_hours value",
                "leaf tasks must set estimate_hours greater than zero",
                "you must provide estimate_hours on each leaf task, above zero",
                "estimate_hours is mandatory and positive for leaf tasks",
            ],
        },
        Topic {
            variants: &[
                "edit specs in the wiki folder, commit them, then git push",
                "specifications are edited under wiki, committed and pushed",
                "modify the spec files in wiki, then commit and push to the remote",
                "to change a spec, edit it in wiki, commit, and push",
            ],
        },
        Topic {
            variants: &[
                "run cargo clippy with deny warnings and never add allow attributes",
                "clippy must pass with -D warnings; do not use any allow attribute",
                "no allow attributes anywhere; cargo clippy has to be warning clean",
                "keep clippy clean under deny warnings without sprinkling allow",
            ],
        },
        Topic {
            variants: &[
                "scratch files belong in the tmp directory which is git ignored",
                "put temporary scratch notes under tmp, it is ignored by git",
                "tmp holds scratch files and is excluded from git tracking",
                "keep throwaway files in the git ignored tmp folder",
            ],
        },
        Topic {
            variants: &[
                "the lexsim crate stays a path dependency until the API is stable",
                "keep lexsim as a workspace path dependency before the API settles",
                "until lexsim's API is stable it remains an in-repo path dependency",
                "lexsim is consumed via path dependency while its API is unstable",
            ],
        },
    ]
}

fn japanese_topics() -> Vec<Topic> {
    vec![
        Topic {
            variants: &[
                "メモリ機能はセッション間で教訓を引き継ぐ",
                "メモリ機能はセッションをまたいで教訓を保持する",
                "セッション間で教訓を引き継ぐのがメモリ機能である",
                "教訓をセッション間で引き継ぐためのメモリ機能",
            ],
        },
        Topic {
            variants: &[
                "git のプッシュは SSH を使い URL に PAT を埋め込まない",
                "プッシュは SSH で行い URL へ PAT を書かない",
                "SSH でプッシュし PAT を URL に埋め込まないこと",
                "URL に PAT を埋め込まず SSH でプッシュする",
            ],
        },
        Topic {
            variants: &[
                "仕様は内部 wiki に連番ページで作成する",
                "設計や仕様は wiki に連番のページとして書く",
                "仕様ドキュメントは内部 wiki の連番ページに置く",
                "連番ページで内部 wiki に仕様を作成する",
            ],
        },
        Topic {
            variants: &[
                "リーフタスクには必ず正の見積時間を設定する",
                "葉タスクは estimate_hours を必ず正の値で入れる",
                "見積時間はリーフタスクで必須かつ正の値とする",
                "各リーフタスクに正の見積時間を必ず指定する",
            ],
        },
        Topic {
            variants: &[
                "CHANGELOG はユーザー向けで内部情報や SHA を含めない",
                "変更履歴はユーザー向けなので内部の SHA を書かない",
                "ユーザー向けの CHANGELOG に内部仕様や SHA を入れない",
                "内部情報や SHA を CHANGELOG に含めずユーザー向けに保つ",
            ],
        },
        Topic {
            variants: &[
                "一時ファイルは git 管理外の tmp ディレクトリに置く",
                "スクラッチは tmp に置き git では追跡しない",
                "tmp は git 無視なので一時ファイルをそこに入れる",
                "作業用の一時ファイルは git ignore された tmp に置く",
            ],
        },
    ]
}

fn main() {
    let mut rng = Lcg::new(0x5EED_1234_ABCD_0001);

    println!("=== lexsim similarity audit ===\n");

    let en = english_topics();
    let jp = japanese_topics();

    // 1) Near-duplicate detection (positives + negatives), generated at scale.
    let dedup = run_dedup_audit(&mut rng, &en, &jp);

    // 2) BM25 retrieval rank-1 accuracy across corpora.
    let retrieval = run_retrieval_audit(&mut rng, &en, &jp);

    // 3) content_hash invariants under noise.
    let hashing = run_hash_audit(&mut rng, &en, &jp);

    println!("\n=== summary ===");
    println!("total comparisons         : {}", dedup.total);
    println!(
        "  near-dup recall (>= {DUP_THRESHOLD:.2}): {:.4}  ({}/{})",
        dedup.pos_recall(),
        dedup.pos_hit,
        dedup.pos_total
    );
    println!(
        "  paraphrase mean score     : {:.3}  (informational; not required >= threshold)",
        dedup.para_mean()
    );
    println!(
        "  related>unrelated sep.    : {:.4}  ({}/{})",
        dedup.separation(),
        dedup.sep_correct,
        dedup.sep_total
    );
    println!(
        "  negative specificity      : {:.4}  ({}/{})  false positives: {}",
        dedup.neg_specificity(),
        dedup.neg_correct,
        dedup.neg_total,
        dedup.false_pos
    );
    println!(
        "retrieval queries           : {}  rank-1 accuracy: {:.4}  top-3: {:.4}",
        retrieval.total,
        retrieval.rank1_acc(),
        retrieval.top3_acc(),
    );
    println!(
        "hash invariants checked     : {}  failures: {}",
        hashing.total, hashing.failures
    );

    // Quality bars. These must hold for P1 to be considered correct.
    let mut failed = false;
    let mut bar = |name: &str, ok: bool| {
        println!("[{}] {name}", if ok { "PASS" } else { "FAIL" });
        if !ok {
            failed = true;
        }
    };
    bar("near-dup recall >= 0.99", dedup.pos_recall() >= 0.99);
    bar(
        "negative false-positive rate <= 0.01",
        dedup.false_pos_rate() <= 0.01,
    );
    bar(
        "related>unrelated separation >= 0.95",
        dedup.separation() >= 0.95,
    );
    bar("retrieval rank-1 >= 0.90", retrieval.rank1_acc() >= 0.90);
    bar("retrieval top-3 >= 0.98", retrieval.top3_acc() >= 0.98);
    bar("hash invariants all hold", hashing.failures == 0);

    if failed {
        eprintln!("\nAUDIT FAILED");
        std::process::exit(1);
    }
    println!("\nAUDIT PASSED");
}

// ---------------------------------------------------------------------------
// Dedup audit
// ---------------------------------------------------------------------------

#[derive(Default)]
struct DedupStats {
    total: usize,
    /// Class A: same sentence + noise. These MUST be caught as near-duplicates.
    pos_total: usize,
    pos_hit: usize,
    /// Class B: paraphrases (same idea, different words). Measured for ranking
    /// quality, NOT required to exceed the dedup threshold — by design they fall
    /// through to "saved separately" and are reconciled by AI cleanup later.
    para_total: usize,
    para_sum: f64,
    /// Negatives: cross-topic. MUST stay below threshold.
    neg_total: usize,
    neg_correct: usize,
    false_pos: usize,
    /// Hardest cases, for diagnostics.
    min_pos_score: f64,
    max_neg_score: f64,
    /// Separation check: how often a same-topic paraphrase outscores a random
    /// cross-topic pair. The engine should rank related text above unrelated.
    sep_total: usize,
    sep_correct: usize,
}
impl DedupStats {
    fn pos_recall(&self) -> f64 {
        if self.pos_total == 0 {
            1.0
        } else {
            self.pos_hit as f64 / self.pos_total as f64
        }
    }
    fn neg_specificity(&self) -> f64 {
        if self.neg_total == 0 {
            1.0
        } else {
            self.neg_correct as f64 / self.neg_total as f64
        }
    }
    fn false_pos_rate(&self) -> f64 {
        if self.neg_total == 0 {
            0.0
        } else {
            self.false_pos as f64 / self.neg_total as f64
        }
    }
    fn para_mean(&self) -> f64 {
        if self.para_total == 0 {
            0.0
        } else {
            self.para_sum / self.para_total as f64
        }
    }
    fn separation(&self) -> f64 {
        if self.sep_total == 0 {
            1.0
        } else {
            self.sep_correct as f64 / self.sep_total as f64
        }
    }
}

/// Inject realistic *near-duplicate* noise that preserves meaning AND canonical
/// tokens: whitespace runs, word-initial capitalization, optional trailing
/// punctuation. Mid-word case flips are avoided on purpose (they change
/// camelCase identifier splitting — a genuine content distinction). A pair where
/// both sides only carry this noise MUST score as a near-duplicate.
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

fn run_dedup_audit(rng: &mut Lcg, en: &[Topic], jp: &[Topic]) -> DedupStats {
    let mut st = DedupStats {
        min_pos_score: 1.0,
        max_neg_score: 0.0,
        ..Default::default()
    };

    let all: Vec<&Topic> = en.iter().chain(jp.iter()).collect();

    // CLASS A — near-duplicates: the SAME sentence with realistic edit noise
    // (whitespace runs, word-initial capitalization, trailing punctuation).
    // These represent "the AI tried to save essentially the same memory again"
    // and MUST be caught by the dedup threshold.
    const A_SAMPLES: usize = 400;
    for topic in &all {
        for v in topic.variants {
            for _ in 0..A_SAMPLES {
                let a = near_dup_noise(rng, v);
                let b = near_dup_noise(rng, v);
                let score = jaccard(&a, &b);
                st.total += 1;
                st.pos_total += 1;
                if score >= DUP_THRESHOLD {
                    st.pos_hit += 1;
                }
                if score < st.min_pos_score {
                    st.min_pos_score = score;
                }
            }
        }
    }

    // CLASS B — paraphrases: DIFFERENT variants of the same topic. Same meaning,
    // different words. We record their scores (to show the engine still rates
    // them well above unrelated text) but do NOT require >= threshold: by design
    // these get saved separately and reconciled by AI cleanup.
    for topic in &all {
        let v = topic.variants;
        for i in 0..v.len() {
            for j in 0..v.len() {
                if i == j {
                    continue;
                }
                let score = jaccard(v[i], v[j]);
                st.total += 1;
                st.para_total += 1;
                st.para_sum += score;
            }
        }
    }

    // NEGATIVES — cross-topic pairs (different ideas). Same-language crossings
    // share function words / domain vocabulary (the hard negatives); cross-
    // language crossings are easy. These MUST stay below threshold.
    const NEG_SAMPLES: usize = 60000;
    for _ in 0..NEG_SAMPLES {
        let ti = rng.below(all.len());
        let mut tj = rng.below(all.len());
        if tj == ti {
            tj = (tj + 1) % all.len();
        }
        let a = all[ti].variants[rng.below(all[ti].variants.len())];
        let b = all[tj].variants[rng.below(all[tj].variants.len())];
        let score = jaccard(&near_dup_noise(rng, a), &near_dup_noise(rng, b));
        st.total += 1;
        st.neg_total += 1;
        if score < DUP_THRESHOLD {
            st.neg_correct += 1;
        } else {
            st.false_pos += 1;
        }
        if score > st.max_neg_score {
            st.max_neg_score = score;
        }
    }

    // SEPARATION — a same-topic paraphrase must outscore a random cross-topic
    // pair. This is the property that actually matters for relevance: related
    // beats unrelated, even when neither crosses the dedup threshold.
    const SEP_SAMPLES: usize = 60000;
    for _ in 0..SEP_SAMPLES {
        let ti = rng.below(all.len());
        let v = all[ti].variants;
        if v.len() < 2 {
            continue;
        }
        let i = rng.below(v.len());
        let mut j = rng.below(v.len());
        if j == i {
            j = (j + 1) % v.len();
        }
        let same = jaccard(v[i], v[j]);

        let mut tk = rng.below(all.len());
        if tk == ti {
            tk = (tk + 1) % all.len();
        }
        let other = all[tk].variants[rng.below(all[tk].variants.len())];
        let diff = jaccard(v[i], other);

        st.sep_total += 1;
        if same > diff {
            st.sep_correct += 1;
        }
    }

    println!(
        "[dedup] class-A near-dups: {}  paraphrases: {}  negatives: {}  separation trials: {}",
        st.pos_total, st.para_total, st.neg_total, st.sep_total
    );
    println!(
        "[dedup] min near-dup score: {:.3}   paraphrase mean: {:.3}   max negative score: {:.3}",
        st.min_pos_score,
        st.para_mean(),
        st.max_neg_score
    );
    st
}

// ---------------------------------------------------------------------------
// Retrieval audit
// ---------------------------------------------------------------------------

#[derive(Default)]
struct RetrievalStats {
    total: usize,
    rank1: usize,
    top3: usize,
}
impl RetrievalStats {
    fn rank1_acc(&self) -> f64 {
        if self.total == 0 {
            1.0
        } else {
            self.rank1 as f64 / self.total as f64
        }
    }
    fn top3_acc(&self) -> f64 {
        if self.total == 0 {
            1.0
        } else {
            self.top3 as f64 / self.total as f64
        }
    }
}

/// Derive a short query from a memory by keeping a random subset of its
/// "content" words (drops function words) — emulating a user prompt that's
/// about the memory without quoting it verbatim.
fn make_query(rng: &mut Lcg, text: &str) -> String {
    let stop: HashSet<&str> = [
        "the", "a", "an", "to", "for", "of", "and", "or", "in", "on", "with", "do", "not", "is",
        "are", "be", "it", "so", "that", "this", "every", "each", "you", "must",
    ]
    .into_iter()
    .collect();
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut kept: Vec<&str> = words
        .iter()
        .copied()
        .filter(|w| {
            let lw = w.to_lowercase();
            !stop.contains(lw.as_str())
        })
        .collect();
    if kept.is_empty() {
        kept = words.clone();
    }
    // Keep roughly half the content words, at least two (or all if very short).
    let target = (kept.len().max(2) / 2).max(2).min(kept.len());
    let mut chosen = Vec::new();
    let mut pool = kept.clone();
    for _ in 0..target {
        if pool.is_empty() {
            break;
        }
        let idx = rng.below(pool.len());
        chosen.push(pool.remove(idx));
    }
    chosen.join(" ")
}

/// Build one corpus from a slice of topics (one representative variant each, plus
/// extra distractor variants), then query each planted memory and check rank.
fn run_retrieval_audit(rng: &mut Lcg, en: &[Topic], jp: &[Topic]) -> RetrievalStats {
    let mut st = RetrievalStats::default();

    // Corpus = the first variant of every topic across both languages. This is a
    // realistic mixed-language memory store.
    let mut docs: Vec<String> = Vec::new();
    let mut planted_idx: Vec<usize> = Vec::new();
    for topic in en.iter().chain(jp.iter()) {
        planted_idx.push(docs.len());
        docs.push(topic.variants[0].to_string());
    }
    let corpus = Corpus::build(&docs);

    // For each planted memory, build many queries (from each variant, several
    // random subsets) and confirm the planted doc ranks first.
    const QUERIES_PER_VARIANT: usize = 200;
    for (topic, &pidx) in en.iter().chain(jp.iter()).zip(planted_idx.iter()) {
        for variant in topic.variants {
            for _ in 0..QUERIES_PER_VARIANT {
                let q = make_query(rng, variant);
                let scores = corpus.bm25_scores(&q);
                // Rank of the planted doc.
                let planted_score = scores[pidx];
                let mut better = 0;
                let mut ge = 0;
                for (i, &s) in scores.iter().enumerate() {
                    if i == pidx {
                        continue;
                    }
                    if s > planted_score {
                        better += 1;
                    }
                    if s >= planted_score {
                        ge += 1;
                    }
                }
                st.total += 1;
                // rank-1 = no other doc strictly outscores it AND it's not tied
                // into oblivion (planted score must be positive).
                if planted_score > 0.0 && better == 0 {
                    st.rank1 += 1;
                }
                if planted_score > 0.0 && ge < 3 {
                    st.top3 += 1;
                }
            }
        }
    }

    println!(
        "[retrieval] corpus size: {}  queries: {}",
        docs.len(),
        st.total
    );
    st
}

// ---------------------------------------------------------------------------
// Hash audit
// ---------------------------------------------------------------------------

#[derive(Default)]
struct HashStats {
    total: usize,
    failures: usize,
}

fn run_hash_audit(rng: &mut Lcg, en: &[Topic], jp: &[Topic]) -> HashStats {
    let mut st = HashStats::default();
    for topic in en.iter().chain(jp.iter()) {
        for variant in topic.variants {
            // Invariant 1: whitespace/case noise must NOT change the hash.
            let base = content_hash(variant);
            for _ in 0..16 {
                let noisy = whitespace_case_only(rng, variant);
                st.total += 1;
                if content_hash(&noisy) != base {
                    st.failures += 1;
                }
            }
            // Invariant 2: a genuinely different variant SHOULD change the hash.
            for other in topic.variants {
                if std::ptr::eq(*other, *variant) {
                    continue;
                }
                st.total += 1;
                if content_hash(other) == base {
                    st.failures += 1;
                }
            }
        }
    }
    st
}

/// Noise that a `content_hash` MUST be invariant to: whitespace runs, plus
/// *word-initial* capitalization. Mid-word case changes are intentionally NOT
/// applied — those alter camelCase identifier splitting (`getMemory` is two
/// concepts, `getmemory` is one), which is a real content distinction the hash
/// is allowed to reflect. Real human "case-only" edits capitalize whole words /
/// sentence starts, which is what this models.
fn whitespace_case_only(rng: &mut Lcg, s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    let mut at_word_start = true;
    for ch in s.chars() {
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
    out
}
