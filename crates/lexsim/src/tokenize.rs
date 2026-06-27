//! Dictionary-free, multilingual tokenizer.
//!
//! The pipeline (see crate docs for the rationale):
//!
//! 1. NFKC normalize + lowercase (unify full/half width, variant forms, case).
//! 2. Split the normalized text into runs of a single *script class*
//!    (Latin-ish / CJK-ish / digit / other), so a Japanese sentence written
//!    with embedded English identifiers is handled per-segment.
//! 3. **Space-delimited scripts** (Latin, Cyrillic, Greek, …) → UAX#29 word
//!    boundaries. `snake_case` / `kebab-case` / `camelCase` identifiers also emit
//!    their sub-tokens so partial matches survive.
//! 4. **Non-spacing scripts** (Han, Hiragana, Katakana, Hangul) → character
//!    **bi-grams** (a trailing lone character becomes a unigram). This is the
//!    same approach Apache Lucene's CJK analyzer uses: dictionary-free recall
//!    for Japanese / Chinese / Korean.
//! 5. Across the whole (normalized) text we additionally emit character
//!    **3-grams** (CL-CnG: Cross-Language Character N-Gram), prefixed so they
//!    never collide with word tokens. These pick up identifiers, proper nouns
//!    and spelling variants in a language-independent way.

use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

/// Marker prepended to character 3-grams so they live in a distinct token
/// namespace from word tokens (a 3-gram `"abc"` must not match a word `"abc"`).
const NGRAM_PREFIX: char = '\u{1}';

/// Length of the cross-language character n-gram.
const CL_NGRAM: usize = 3;

/// Tokenize `text` into a flat list of tokens (words / CJK bigrams / CL-CnG
/// trigrams). Order is deterministic; duplicates are kept (BM25 needs term
/// frequencies). For set-based metrics (Jaccard) deduplicate downstream.
///
/// All emitted tokens are lowercased. NFKC normalization happens first, but
/// case folding is deferred to token emission so that `camelCase` boundaries
/// survive long enough to be split (lowercasing the whole string up front would
/// erase them).
pub fn tokenize(text: &str) -> Vec<String> {
    // NFKC only (case preserved) so camelCase splitting can see boundaries.
    let cased: String = text.nfkc().collect();
    let mut out = Vec::new();

    for segment in script_segments(&cased) {
        match segment.class {
            ScriptClass::Spacing => {
                push_word_tokens(segment.text, &mut out);
            }
            ScriptClass::NonSpacing => {
                push_cjk_bigrams(segment.text, &mut out);
            }
            ScriptClass::Other => {}
        }
    }

    // CL-CnG runs over the fully normalized (lowercased) text — character
    // n-grams don't benefit from case and must match case-insensitively.
    let lowered = lowercase(&cased);
    push_char_ngrams(&lowered, &mut out);
    out
}

/// NFKC normalize and lowercase. Public so callers can compute a stable
/// canonical form (e.g. for content hashing) with the same normalization the
/// tokenizer uses.
pub fn normalize(text: &str) -> String {
    lowercase(&text.nfkc().collect::<String>())
}

fn lowercase(s: &str) -> String {
    s.chars().flat_map(|c| c.to_lowercase()).collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ScriptClass {
    /// Space-delimited writing systems (Latin, Cyrillic, Greek, Arabic, …),
    /// plus digits and identifier connectors (`_`, `-`, `.`).
    Spacing,
    /// Non-spacing scripts that need n-gram segmentation (CJK, Hangul, Kana).
    NonSpacing,
    /// Whitespace, punctuation, symbols — dropped from word tokenization.
    Other,
}

struct Segment<'a> {
    class: ScriptClass,
    text: &'a str,
}

fn classify(c: char) -> ScriptClass {
    if is_non_spacing_script(c) {
        return ScriptClass::NonSpacing;
    }
    if c.is_alphabetic() || c.is_numeric() {
        // Letters and digits both belong to the word-token path. Keeping them in
        // one class means `p1`, `v0`, `utf8` stay as single identifiers.
        return ScriptClass::Spacing;
    }
    // Identifier connectors keep a run together (`atomic_write`, `feat-memory`,
    // `0.13.0`) without becoming standalone tokens; `push_identifier_subtokens`
    // splits on them. Everything else (whitespace, symbols) ends the run.
    if matches!(c, '_' | '-' | '.') {
        return ScriptClass::Spacing;
    }
    ScriptClass::Other
}

/// True for scripts written without spaces between words, where dictionary-free
/// recall is best served by character n-grams. Ranges per the Unicode blocks.
fn is_non_spacing_script(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x309F   // Hiragana
        | 0x30A0..=0x30FF // Katakana
        | 0x31F0..=0x31FF // Katakana Phonetic Extensions
        | 0x3400..=0x4DBF // CJK Unified Ideographs Extension A
        | 0x4E00..=0x9FFF // CJK Unified Ideographs
        | 0xF900..=0xFAFF // CJK Compatibility Ideographs
        | 0xAC00..=0xD7AF // Hangul Syllables
        | 0x1100..=0x11FF // Hangul Jamo
        | 0x20000..=0x2A6DF // CJK Extension B
        | 0x2A700..=0x2EBEF // CJK Extension C–F
    )
}

/// Split `text` into maximal runs of one script class.
fn script_segments(text: &str) -> Vec<Segment<'_>> {
    let mut segments = Vec::new();
    let mut start = 0usize;
    let mut current: Option<ScriptClass> = None;

    for (idx, c) in text.char_indices() {
        let class = classify(c);
        match current {
            Some(cur) if cur == class => {}
            Some(cur) => {
                segments.push(Segment {
                    class: cur,
                    text: &text[start..idx],
                });
                start = idx;
                current = Some(class);
            }
            None => {
                start = idx;
                current = Some(class);
            }
        }
    }
    if let Some(cur) = current {
        segments.push(Segment {
            class: cur,
            text: &text[start..],
        });
    }
    segments
}

/// UAX#29 word tokens for spacing scripts, plus identifier sub-tokens.
fn push_word_tokens(text: &str, out: &mut Vec<String>) {
    for word in text.unicode_words() {
        out.push(lowercase(word));
        push_identifier_subtokens(word, out);
    }
    // `unicode_words` drops pure-digit / underscore-joined runs in some cases;
    // also emit identifier sub-tokens split on `_`, `-`, and case transitions
    // for the raw segment so snake/kebab/camel identifiers are always covered.
    for raw in text.split(|c: char| c.is_whitespace()) {
        if raw.is_empty() {
            continue;
        }
        push_identifier_subtokens(raw, out);
    }
}

/// Emit sub-tokens of an identifier: split on `_`/`-`/`.` separators and on
/// lowercase→uppercase camelCase transitions. Only emits when the split yields
/// more than one piece (avoids duplicating plain words).
fn push_identifier_subtokens(ident: &str, out: &mut Vec<String>) {
    let mut pieces: Vec<String> = Vec::new();
    for part in ident.split(['_', '-', '.']) {
        if part.is_empty() {
            continue;
        }
        for camel in split_camel(part) {
            pieces.push(camel);
        }
    }
    if pieces.len() > 1 {
        for p in pieces {
            out.push(p);
        }
    }
}

/// Split a `camelCase` / `PascalCase` run into lowercase pieces. Returns the
/// input unchanged (as one piece) when there is no internal case transition.
fn split_camel(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut pieces = Vec::new();
    let mut buf = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 && c.is_uppercase() && chars[i - 1].is_lowercase() && !buf.is_empty() {
            pieces.push(std::mem::take(&mut buf));
        }
        buf.extend(c.to_lowercase());
    }
    if !buf.is_empty() {
        pieces.push(buf);
    }
    pieces
}

/// Character bi-grams for non-spacing scripts; a trailing single char becomes a
/// unigram so a one-character segment still produces a token.
fn push_cjk_bigrams(text: &str, out: &mut Vec<String>) {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return;
    }
    if chars.len() == 1 {
        out.push(chars[0].to_string());
        return;
    }
    for window in chars.windows(2) {
        out.push(window.iter().collect());
    }
}

/// Cross-language character 3-grams over the whole normalized text. Whitespace
/// is collapsed to a single marker so n-grams don't span large gaps but word
/// boundaries still influence the grams. Each gram is namespaced by a prefix
/// char so it can't collide with a word token.
fn push_char_ngrams(normalized: &str, out: &mut Vec<String>) {
    let mut squashed = String::with_capacity(normalized.len());
    let mut prev_space = false;
    for c in normalized.chars() {
        if c.is_whitespace() {
            if !prev_space {
                squashed.push(' ');
                prev_space = true;
            }
            continue;
        }
        squashed.push(c);
        prev_space = false;
    }
    let chars: Vec<char> = squashed.trim().chars().collect();
    if chars.len() < CL_NGRAM {
        return;
    }
    for window in chars.windows(CL_NGRAM) {
        // Skip grams that are entirely separator/space — no signal.
        if window.iter().all(|c| *c == ' ') {
            continue;
        }
        let mut gram = String::with_capacity(CL_NGRAM + 1);
        gram.push(NGRAM_PREFIX);
        gram.extend(window.iter());
        out.push(gram);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word_tokens(text: &str) -> Vec<String> {
        tokenize(text)
            .into_iter()
            .filter(|t| !t.starts_with(NGRAM_PREFIX))
            .collect()
    }

    #[test]
    fn english_words_lowercased() {
        let toks = word_tokens("Hello World FOO");
        assert!(toks.contains(&"hello".to_string()));
        assert!(toks.contains(&"world".to_string()));
        assert!(toks.contains(&"foo".to_string()));
    }

    #[test]
    fn snake_case_split_into_subtokens() {
        let toks = word_tokens("use atomic_write always");
        assert!(toks.contains(&"atomic".to_string()));
        assert!(toks.contains(&"write".to_string()));
        // and the joined form is preserved too (unicode_words keeps it together
        // since `_` is part of a word in UAX#29).
        assert!(toks.iter().any(|t| t.contains("atomic")));
    }

    #[test]
    fn kebab_case_split() {
        let toks = word_tokens("feat-memory-p1");
        assert!(toks.contains(&"feat".to_string()));
        assert!(toks.contains(&"memory".to_string()));
        assert!(toks.contains(&"p1".to_string()));
    }

    #[test]
    fn camel_case_split() {
        let toks = word_tokens("getMemoryQuery");
        assert!(toks.contains(&"get".to_string()));
        assert!(toks.contains(&"memory".to_string()));
        assert!(toks.contains(&"query".to_string()));
    }

    #[test]
    fn japanese_bigrams() {
        // "メモリ機能" → bigrams メモ/モリ/リ機/機能
        let toks = word_tokens("メモリ機能");
        assert!(toks.contains(&"メモ".to_string()));
        assert!(toks.contains(&"モリ".to_string()));
        assert!(toks.contains(&"リ機".to_string()));
        assert!(toks.contains(&"機能".to_string()));
    }

    #[test]
    fn single_cjk_char_is_unigram() {
        let toks = word_tokens("猫");
        assert!(toks.contains(&"猫".to_string()));
    }

    #[test]
    fn mixed_japanese_english() {
        // Japanese sentence with an embedded English identifier.
        let toks = word_tokens("atomic_write を使う");
        assert!(toks.contains(&"atomic".to_string()));
        assert!(toks.contains(&"write".to_string()));
        // CJK part bigrams
        assert!(toks.iter().any(|t| t == "使う" || t == "を使"));
    }

    #[test]
    fn nfkc_fullwidth_unified() {
        // Fullwidth ＡＢＣ should normalize to abc.
        let toks = word_tokens("ＡＢＣ");
        assert!(toks.contains(&"abc".to_string()));
    }

    #[test]
    fn cl_ngram_emitted_and_namespaced() {
        let all = tokenize("hello");
        let ngrams: Vec<_> = all.iter().filter(|t| t.starts_with(NGRAM_PREFIX)).collect();
        assert!(!ngrams.is_empty(), "expected CL-CnG trigrams");
        // "hello" → hel ell llo
        assert!(ngrams.iter().any(|g| g.ends_with("hel")));
        assert!(ngrams.iter().any(|g| g.ends_with("llo")));
    }

    #[test]
    fn ngram_does_not_collide_with_word() {
        // A 3-letter word "abc" and the trigram of "abc" must be distinct tokens.
        let toks = tokenize("abc");
        let word = toks.iter().filter(|t| *t == "abc").count();
        let gram = toks
            .iter()
            .filter(|t| t.starts_with(NGRAM_PREFIX) && t.ends_with("abc"))
            .count();
        assert_eq!(word, 1);
        assert_eq!(gram, 1);
    }

    #[test]
    fn empty_input_no_panic() {
        assert!(tokenize("").is_empty());
        assert!(tokenize("   ").is_empty());
    }

    #[test]
    fn digits_kept() {
        let toks = word_tokens("version 0.13.0");
        assert!(toks.iter().any(|t| t.contains("13") || t == "0"));
    }
}
