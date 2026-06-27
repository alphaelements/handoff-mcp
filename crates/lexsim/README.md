# lexsim

A **dictionary-free, multilingual lexical similarity engine** for Rust:
tokenize + Jaccard + BM25 + a stable content hash, sharing one tokenizer.

It answers two questions with the same notion of "term":

- **"Are these the same?"** → `jaccard` (symmetric set similarity) — detect
  near-duplicates (e.g. before saving a record).
- **"Is this relevant to that?"** → `Corpus::bm25_scores` (asymmetric ranking) —
  pull the items relevant to a query.

Plus `content_hash` for change-detection / re-injection tracking.

## Why dictionary-free

Morphological dictionaries (e.g. for Japanese) are multi-megabyte and
language-specific. `lexsim` instead combines Unicode-standard techniques:

- **UAX#29 word boundaries** for space-delimited scripts,
- **CJK character bi-grams** (the Apache Lucene approach) for non-spacing scripts
  (Japanese / Chinese / Korean),
- **NFKC normalization** to unify full/half-width and variant forms,
- **CL-CnG** (Cross-Language Character N-Grams) so identifiers, proper nouns, and
  spelling variants match across languages.

The result reaches dictionary-like recall with **zero dictionary**, in
sub-millisecond time for the corpus sizes this targets (tens to hundreds of short
documents). Its only dependencies are `unicode-segmentation` and
`unicode-normalization`.

## Install

```toml
[dependencies]
lexsim = "0.1"
```

## Usage

Detect a near-duplicate before saving (symmetric `jaccard`):

```rust
use lexsim::jaccard;

let existing = "always use atomic_write for handoff files";
let incoming = "always use atomic_write when writing handoff files";
assert!(jaccard(existing, incoming) > 0.5); // clearly a near-duplicate

let unrelated = "the cat sat on the mat";
assert!(jaccard(existing, unrelated) < 0.2);
```

Rank stored items by relevance to a query (asymmetric BM25 via `Corpus`):

```rust
use lexsim::Corpus;

let memories = vec![
    "always use atomic_write for handoff files".to_string(),
    "configure the milestone schedule and assignee".to_string(),
];
let corpus = Corpus::build(&memories);
let scores = corpus.bm25_scores("atomic write");
assert!(scores[0] > scores[1]); // memory 0 is the most relevant
```

The tokenizer is dictionary-free, so a Japanese query matches a Japanese
document — and an English identifier embedded in Japanese text is still found:

```rust
use lexsim::Corpus;

let memories = vec![
    "atomic_write を必ず使う（torn read 防止）".to_string(),
    "ガントチャートの表示設定".to_string(),
];
let corpus = Corpus::build(&memories);
assert!(corpus.bm25_scores("メモリ atomic_write")[0] > 0.0);
```

## Public API

| Item | Purpose |
|------|---------|
| `tokenize` / `normalize` | dictionary-free, multilingual tokenizer (NFKC + UAX#29 + CJK bi-grams + CL-CnG) |
| `jaccard` / `jaccard_sets` / `token_set` | symmetric set similarity for dedup |
| `Corpus::build` / `Corpus::bm25_scores` | asymmetric BM25 ranking for retrieval |
| `content_hash` / `fnv1a_hex` | stable content hashing for change detection |
| `Scorer` / `LexicalScorer` | trait + lexical impl; lets an embedding-based scorer slot in later behind one call site |

## Future extension

Purely lexical matching is weak on *cross-language synonyms* (the same idea in
two languages with no shared tokens). The `Scorer` trait marks where an
embedding-based stage could be added later without touching callers; only the
lexical scorer is implemented today.

## License

MIT OR Apache-2.0.

Extracted from [handoff-mcp](https://github.com/alphaelements/handoff-mcp), where
it powers cross-session memory retrieval and dedup.
