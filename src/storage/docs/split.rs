//! Splits a single authored Markdown body into byte-exact fragments at ATX
//! heading boundaries, using `pulldown-cmark`'s byte-offset event stream so
//! that `#`/`##` inside fenced code blocks or block quotes is never
//! misdetected as a heading (see wiki/130-document-management.md §5.1 "M1").
//!
//! The split is purely byte-slicing: heading lines are never re-rendered, so
//! [`reassemble`](super::reassemble::reassemble) can reconstruct the original
//! document exactly by concatenating fragment bodies in `seq` order.

use anyhow::{bail, Result};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag};

use super::model::SectionIndex;

const BOM: &str = "\u{FEFF}";

/// Default ATX heading level at which a document is split into fragments
/// (`##` = level 2). Overridable per call and, once P1-2 storage/config
/// wiring lands, per `config.toml` `doc_split_level`.
pub const DEFAULT_SPLIT_LEVEL: u8 = 2;

/// One fragment produced by [`split`]. `body` is a raw byte slice of the
/// input `body` string passed to `split` — it is never re-rendered, so
/// concatenating all fragments' `body` in `seq` order reproduces the
/// original document (minus a stripped BOM/frontmatter, which `split`
/// reports separately for the caller to persist).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitFragment<'a> {
    /// 0-based position in the document. seq 0 is always the preamble
    /// (text before the first heading at or above `split_level`), even if
    /// empty.
    pub seq: usize,
    /// The heading text (without the leading `#` markers or surrounding
    /// whitespace) that starts this fragment, or `None` for the seq-0
    /// preamble when the document has no heading before it.
    pub heading: Option<String>,
    /// ATX heading level (1-6) that starts this fragment, or 0 for the
    /// seq-0 preamble.
    pub level: u8,
    /// Raw byte slice of this fragment's body, including its own leading
    /// heading line (if any) verbatim.
    pub body: &'a str,
}

/// Result of [`split`]: the fragment list plus document-level facts needed
/// to persist and losslessly reassemble the original body (BOM presence,
/// line-ending style, extracted frontmatter).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitDocument<'a> {
    /// Fragments in `seq` order, covering the document body *after* the BOM
    /// and frontmatter (if any) have been stripped out of `fragments[0]`.
    pub fragments: Vec<SplitFragment<'a>>,
    /// `true` if `body` started with a UTF-8 BOM (`\u{FEFF}`).
    pub has_bom: bool,
    /// `"lf"` or `"crlf"`, detected from the first line ending encountered.
    /// A document with no line endings at all (single line) is `"lf"`.
    pub line_ending: &'static str,
    /// The raw YAML frontmatter block (between the `---` fences, exclusive),
    /// if the document starts with one. Excluded from `fragments[0].body`
    /// per spec §5.1 scope rule 6.
    pub frontmatter: Option<&'a str>,
    /// `true` if the original document had a line ending immediately after
    /// the closing `---` fence (e.g. `"---\ntitle: Foo\n---\nbody"`), `false`
    /// if the closing fence was the last thing in the document (e.g.
    /// `"---\ntitle: Foo\n---"` with nothing after it, not even a newline).
    /// Meaningless when `frontmatter` is `None`. Callers reassembling the
    /// document must conditionally omit the eol after `---` when this is
    /// `false`, or they will invent a byte that was never in the original
    /// body (see `extract_frontmatter` doc comment for why the eol cannot be
    /// inferred from `frontmatter`/`fragments[0]` alone).
    pub frontmatter_trailing_eol: bool,
}

/// Splits `body` into fragments at ATX heading boundaries of `split_level`
/// or higher (i.e. `level <= split_level`, since `H1` < `H2` numerically).
///
/// Returns `Err` if `body` mixes LF and CRLF line endings (spec §5.1: mixed
/// CRLF is rejected outright rather than guessed at).
pub fn split(body: &str, split_level: u8) -> Result<SplitDocument<'_>> {
    let line_ending = detect_line_ending(body)?;

    let has_bom = body.starts_with(BOM);
    let after_bom = if has_bom { &body[BOM.len()..] } else { body };

    let (frontmatter, after_frontmatter, frontmatter_byte_len, frontmatter_trailing_eol) =
        extract_frontmatter(after_bom, line_ending);

    let heading_bounds = collect_heading_bounds(after_frontmatter, split_level);

    let mut fragments = Vec::with_capacity(heading_bounds.len() + 1);

    // seq 0: preamble before the first qualifying heading (possibly empty).
    let first_start = heading_bounds
        .first()
        .map(|h| h.start)
        .unwrap_or(after_frontmatter.len());
    fragments.push(SplitFragment {
        seq: 0,
        heading: None,
        level: 0,
        body: &after_frontmatter[..first_start],
    });
    let mut cursor = first_start;

    for (i, bound) in heading_bounds.iter().enumerate() {
        let end = heading_bounds
            .get(i + 1)
            .map(|next| next.start)
            .unwrap_or(after_frontmatter.len());
        fragments.push(SplitFragment {
            seq: i + 1,
            heading: Some(bound.text.clone()),
            level: bound.level,
            body: &after_frontmatter[cursor..end],
        });
        cursor = end;
    }

    // Frontmatter length is reported to the caller via the returned struct;
    // it is intentionally not folded back into any fragment body (spec
    // §5.1 scope rule 6: frontmatter goes to `source.frontmatter`, not
    // fragment seq 0).
    let _ = frontmatter_byte_len;

    Ok(SplitDocument {
        fragments,
        has_bom,
        line_ending,
        frontmatter,
        frontmatter_trailing_eol,
    })
}

/// Converts a [`SplitDocument`]'s fragments into a [`SectionIndex`] manifest
/// (v5, spec §3.1): one entry per fragment, with a cumulative `byte_offset`
/// into the *body as reported by `split`* (i.e. after BOM/frontmatter
/// stripping — the same body callers persist to `_doc.<slug>.md`'s section
/// range), `byte_length`, and a content hash of each fragment's body slice,
/// unless `compute_hash` is `false` — in which case every `content_hash` is
/// left `None` (P-M1, wiki/240-performance-design.md §4, t370.8): the
/// `lexsim::content_hash` pass over every fragment (summing to roughly the
/// whole body's bytes) is skipped entirely for callers that only need the
/// byte-offset/heading structure (e.g. `DocSet`-based task-link/dev_stage
/// propagation), not the hash.
/// Performs no file I/O — this is purely an in-memory transform.
pub fn compute_sections(split_doc: &SplitDocument<'_>, compute_hash: bool) -> Vec<SectionIndex> {
    let mut offset = 0usize;
    split_doc
        .fragments
        .iter()
        .map(|frag| {
            let section = SectionIndex {
                seq: frag.seq,
                heading: frag.heading.clone().unwrap_or_default(),
                level: frag.level,
                byte_offset: offset,
                byte_length: frag.body.len(),
                content_hash: compute_hash.then(|| hash_section_body(frag.body)),
            };
            offset += frag.body.len();
            section
        })
        .collect()
}

/// `lexsim::content_hash` of a single section's body — the one and only seam
/// every per-section hash computation goes through (`compute_sections`'s bulk
/// pass above, and `mcp::handlers::docs::handle_doc_update_section`'s
/// single-section incremental rehash, t370.15 PR-4). Routing both call sites
/// through here, rather than each calling `lexsim::content_hash` directly,
/// lets tests assert exactly how many sections actually paid the expensive
/// NFKC+tokenize cost for a given edit — the "only the changed section is
/// rehashed" acceptance criterion — regardless of which caller triggered it.
pub(crate) fn hash_section_body(text: &str) -> String {
    #[cfg(test)]
    SECTION_HASH_COMPUTE_COUNT.with(|c| c.set(c.get() + 1));
    lexsim::content_hash(text)
}

#[cfg(test)]
thread_local! {
    static SECTION_HASH_COMPUTE_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Test-only: how many times [`hash_section_body`] has run on this thread
/// since the last [`reset_section_hash_compute_count_for_test`] call.
/// Thread-local (rather than a single process-wide counter) so tests running
/// in parallel on different threads never see each other's counts — mirrors
/// the per-path keying `storage::docs::HASH_COMPUTE_COUNTS` uses for the same
/// reason, adapted to a function with no path to key by.
#[cfg(test)]
pub(crate) fn section_hash_compute_count_for_test() -> usize {
    SECTION_HASH_COMPUTE_COUNT.with(|c| c.get())
}

#[cfg(test)]
pub(crate) fn reset_section_hash_compute_count_for_test() {
    SECTION_HASH_COMPUTE_COUNT.with(|c| c.set(0));
}

/// Composes a whole-document `content_hash` from its sections' individual
/// `content_hash` values (t370.15, PR-4: wiki/240-performance-design.md §6).
/// Replaces an independent `lexsim::content_hash(whole_body)` pass — the
/// expensive part of that call is NFKC normalization + tokenization (worse
/// for Japanese text than English), and every section's hash already pays
/// that cost once each, summing to the same bytes as the whole body. Running
/// a *second*, independent tokenize pass over the whole body just to get a
/// document-level hash was pure waste (wiki/240 §1: "本文全体と各セクションに
/// 2回かける"). This composition step itself is cheap — FNV-1a (via
/// `lexsim::fnv1a_hex`) over the concatenated section hashes, not lexsim
/// tokenization — so it adds negligible cost on top of hashes already paid
/// for.
///
/// Deterministic given the ordered section hash values (seq order, seq-0
/// preamble included) — any change to any section's content changes the
/// composed result. Sections whose `content_hash` is `None` (a
/// `compute_hash: false` list slipped in by a future caller) contribute an
/// empty placeholder rather than panicking; callers that need a trustworthy
/// composed hash must pass a list computed with `compute_hash: true` (same
/// discipline `DocMetadata::content_hash`'s doc comment already requires of
/// its callers).
pub fn compose_doc_hash(sections: &[SectionIndex]) -> String {
    let mut buf = String::new();
    for section in sections {
        buf.push_str(section.content_hash.as_deref().unwrap_or(""));
        buf.push('\n');
    }
    lexsim::fnv1a_hex(buf.as_bytes())
}

/// Rebuilds a document's `sections[]` after `handle_doc_update_section`
/// splices `new_content` into the section at `changed_seq`'s byte range
/// (t370.15, PR-4, wiki/240-performance-design.md §6): reuses unaffected
/// sections' hashes from `old_sections` instead of paying
/// `lexsim::content_hash` again for every section — the splice only ever
/// changes the *bytes* of one section (every other section's body is copied
/// verbatim from the pre-edit body into the new one, byte-for-byte), so an
/// unaffected section's old hash is still correct for the new body.
///
/// Only trusts the reuse when:
/// 1. the edit didn't change the total section *count* (i.e. `new_content`
///    didn't introduce or remove a heading boundary of its own, which would
///    shift every later section's `seq`), and
/// 2. every unaffected position's `(heading, level, byte_length)` still
///    matches its old counterpart exactly (defense in depth against a
///    `sections`/`body` desync from an unrelated bug).
///
/// Any mismatch falls back to hashing every section fresh via
/// `compute_sections(new_split_doc, true)` — still correct (this is exactly
/// what every caller did before this optimization), just not fast for that
/// unusual edit.
///
/// `new_split_doc` must be the result of splitting the *already-spliced* new
/// body; `old_sections` must be the pre-edit section list with real
/// `content_hash` values (i.e. computed with `compute_hash: true`);
/// `new_content` is the exact text spliced in at `changed_seq`.
pub fn compute_sections_after_splice(
    new_split_doc: &SplitDocument<'_>,
    old_sections: &[SectionIndex],
    changed_seq: usize,
    new_content: &str,
) -> Vec<SectionIndex> {
    let candidate = compute_sections(new_split_doc, false);

    let reuse_is_safe = candidate.len() == old_sections.len()
        && candidate.iter().zip(old_sections.iter()).enumerate().all(
            |(i, (new_section, old_section))| {
                i == changed_seq
                    || (new_section.heading == old_section.heading
                        && new_section.level == old_section.level
                        && new_section.byte_length == old_section.byte_length)
            },
        );

    if !reuse_is_safe {
        return compute_sections(new_split_doc, true);
    }

    candidate
        .into_iter()
        .enumerate()
        .map(|(i, mut section)| {
            section.content_hash = if i == changed_seq {
                Some(hash_section_body(new_content))
            } else {
                old_sections[i].content_hash.clone()
            };
            section
        })
        .collect()
}

/// A single heading boundary: byte offset (into the frontmatter-stripped
/// body) where the heading line starts, its level, and its trimmed text.
///
/// `pub(super)` (rather than private): [`layer_parse`](super::layer_parse)
/// reuses [`collect_all_heading_bounds`] so both modules share exactly one
/// fence/block-quote-aware heading scanner (wiki/220-vmodel-integration-design.md
/// §2.2 "フェンス対応": "見出し検出は split.rs と同じフェンス認識を用いる").
pub(super) struct HeadingBound {
    pub(super) start: usize,
    pub(super) level: u8,
    pub(super) text: String,
}

/// Runs the pulldown-cmark offset-tracking parser and collects the byte
/// start of **every** ATX heading (all levels 1-6), fenced code blocks and
/// block quotes handled by the parser itself so a `#`/`##` inside a fence
/// never appears here. [`split`] filters this down to `level <= split_level`
/// via [`collect_heading_bounds`]; [`layer_parse::parse_layer_body`](super::layer_parse::parse_layer_body)
/// consumes the unfiltered list directly, since a layer item heading may be
/// any level (wiki/220 §2.2).
pub(super) fn collect_all_heading_bounds(text: &str) -> Vec<HeadingBound> {
    let parser = Parser::new_ext(text, Options::all());
    let mut bounds = Vec::new();
    let mut in_heading: Option<(usize, u8)> = None;
    let mut heading_text = String::new();

    for (event, range) in parser.into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let numeric_level = heading_level_to_u8(level);
                if is_atx_heading(text, &range) {
                    in_heading = Some((range.start, numeric_level));
                    heading_text.clear();
                }
            }
            Event::Text(ref t) | Event::Code(ref t) if in_heading.is_some() => {
                heading_text.push_str(t);
            }
            Event::End(pulldown_cmark::TagEnd::Heading(_)) => {
                if let Some((start, level)) = in_heading.take() {
                    bounds.push(HeadingBound {
                        start,
                        level,
                        text: heading_text.trim().to_string(),
                    });
                }
            }
            _ => {}
        }
    }

    bounds
}

/// [`collect_all_heading_bounds`] filtered to headings whose level is `<=
/// split_level` (this crate's own fragment-splitting use).
fn collect_heading_bounds(text: &str, split_level: u8) -> Vec<HeadingBound> {
    collect_all_heading_bounds(text)
        .into_iter()
        .filter(|h| h.level <= split_level)
        .collect()
}

/// Returns `true` if the heading `range` (as reported by pulldown-cmark's
/// offset iterator) begins with a literal `#` in the source `text`.
///
/// pulldown-cmark emits the identical `Event::Start(Tag::Heading { level,
/// .. })` for both ATX (`## Foo`) and setext (`Foo\n---`) headings, with no
/// syntax discriminator on the event itself. Per spec §5.1 scope restriction,
/// setext headings must never be treated as split boundaries (they stay
/// embedded in the enclosing fragment's body), so this raw-byte check is the
/// only reliable way to reject them: an ATX heading's byte range always
/// starts at the line's leading `#`, whereas a setext heading's range starts
/// at the title text itself (the underline is a separate, later span).
fn is_atx_heading(text: &str, range: &std::ops::Range<usize>) -> bool {
    text.as_bytes().get(range.start).is_some_and(|&b| b == b'#')
}

fn heading_level_to_u8(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Detects the document's line-ending style. Returns an error if both `\r\n`
/// and a bare `\n` (not preceded by `\r`) appear in the same document.
fn detect_line_ending(body: &str) -> Result<&'static str> {
    let bytes = body.as_bytes();
    let mut saw_crlf = false;
    let mut saw_lone_lf = false;

    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            let preceded_by_cr = i > 0 && bytes[i - 1] == b'\r';
            if preceded_by_cr {
                saw_crlf = true;
            } else {
                saw_lone_lf = true;
            }
        }
    }

    if saw_crlf && saw_lone_lf {
        bail!("document mixes CRLF and LF line endings; mixed line endings are not supported");
    }

    Ok(if saw_crlf { "crlf" } else { "lf" })
}

/// Extracts a leading YAML frontmatter block (`---\n...\n---\n`) using
/// pulldown-cmark's `MetadataBlock` event, so the same fenced-code-aware
/// parser that drives heading detection also drives frontmatter detection
/// (no separate ad hoc regex).
///
/// Returns `(frontmatter_text, remaining_body, frontmatter_byte_len,
/// trailing_eol_present)`. When there is no frontmatter, returns `(None,
/// text, 0, false)` unchanged.
///
/// `line_ending` (`"lf"` or `"crlf"`, as already detected from the whole
/// document by [`detect_line_ending`]) selects the fence delimiter
/// (`"---\n"` vs `"---\r\n"`) so that a CRLF document's opening fence is
/// actually recognized instead of falling through to the `unwrap_or(block)`
/// fallback, which would otherwise leave both `---` fences embedded in the
/// reported frontmatter and corrupt the byte-identical round trip.
///
/// pulldown-cmark's `MetadataBlock` range ends right at the closing `---`
/// fence and never includes the line ending that follows it (verified across
/// LF/CRLF and with/without trailing content): e.g. for
/// `"---\ntitle: Foo\n---\n# Title\n"`, `range.end` is `18` (just past the
/// closing `---`), leaving the `\n` at byte 18 unconsumed and still present
/// at the start of `after_frontmatter`. If left as-is, the caller
/// (`handle_doc_get`/`handle_doc_list`) which re-adds `---{eol}` after the
/// frontmatter on reassembly would double that line ending. So this function
/// consumes one extra `eol` past `range.end` here, when present, to keep
/// `after_frontmatter` exactly what followed the frontmatter block's own
/// trailing newline. Whether that extra `eol` was actually present is
/// reported back as `trailing_eol_present`, since a caller re-adding
/// `---{eol}` on reassembly cannot otherwise tell a document that ended
/// exactly at the closing fence (no eol at all) apart from one that had
/// content following it (see [`SplitDocument::frontmatter_trailing_eol`]).
fn extract_frontmatter<'a>(
    text: &'a str,
    line_ending: &'static str,
) -> (Option<&'a str>, &'a str, usize, bool) {
    let parser = Parser::new_ext(text, Options::all());
    // Only the very first event can be a metadata block (pulldown-cmark only
    // recognizes YAML frontmatter at the start of the document), so a single
    // peek is enough.
    let Some((event, range)) = parser.into_offset_iter().next() else {
        return (None, text, 0, false);
    };
    if !matches!(event, Event::Start(Tag::MetadataBlock(_))) {
        return (None, text, 0, false);
    }
    // `range` for the Start event already covers the whole block
    // (`---\n...\n---`), since pulldown-cmark treats metadata blocks as an
    // atomic (non-nested) event span, but excludes the line ending after the
    // closing fence (see doc comment above).
    let block = &text[range.clone()];
    let eol = if line_ending == "crlf" { "\r\n" } else { "\n" };
    let fence_with_eol = format!("---{eol}");
    let inner = block
        .strip_prefix(fence_with_eol.as_str())
        .and_then(|s| {
            s.strip_suffix(fence_with_eol.as_str())
                .or_else(|| s.strip_suffix("---"))
        })
        .unwrap_or(block);
    let trailing_eol_present = text[range.end..].starts_with(eol);
    let end = if trailing_eol_present {
        range.end + eol.len()
    } else {
        range.end
    };
    (Some(inner), &text[end..], end, trailing_eol_present)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_sections_assigns_cumulative_byte_offsets() {
        let body = "Preamble.\n\n## A\nBody A\n## B\nBody B\n";
        let split_doc = split(body, 2).unwrap();
        let sections = compute_sections(&split_doc, true);

        assert_eq!(sections.len(), split_doc.fragments.len());
        let mut expected_offset = 0usize;
        for (section, frag) in sections.iter().zip(split_doc.fragments.iter()) {
            assert_eq!(section.seq, frag.seq);
            assert_eq!(section.level, frag.level);
            assert_eq!(section.heading, frag.heading.clone().unwrap_or_default());
            assert_eq!(section.byte_offset, expected_offset);
            assert_eq!(section.byte_length, frag.body.len());
            assert_eq!(
                section.content_hash.as_deref(),
                Some(lexsim::content_hash(frag.body).as_str())
            );
            expected_offset += frag.body.len();
        }
    }

    #[test]
    fn compute_sections_offsets_slice_the_split_body_correctly() {
        let body = "Preamble.\n\n## A\nBody A\n## B\nBody B\n";
        let split_doc = split(body, 2).unwrap();
        let sections = compute_sections(&split_doc, true);

        // Reconstruct the after-frontmatter body by concatenating fragments,
        // then verify each section's byte_offset/byte_length slices it back
        // out identically to the fragment body it was computed from.
        let full: String = split_doc.fragments.iter().map(|f| f.body).collect();
        for (section, frag) in sections.iter().zip(split_doc.fragments.iter()) {
            let slice = &full[section.byte_offset..section.byte_offset + section.byte_length];
            assert_eq!(slice, frag.body);
        }
    }

    /// P-M1 (wiki/240-performance-design.md §4, t370.8): `compute_hash =
    /// false` must skip the `lexsim::content_hash` pass entirely (every
    /// section's `content_hash` stays `None`) while still computing the
    /// byte-offset/heading structure exactly as the hashed path does.
    #[test]
    fn compute_sections_with_compute_hash_false_leaves_content_hash_none() {
        let body = "Preamble.\n\n## A\nBody A\n## B\nBody B\n";
        let split_doc = split(body, 2).unwrap();
        let hashed = compute_sections(&split_doc, true);
        let lazy = compute_sections(&split_doc, false);

        assert_eq!(lazy.len(), hashed.len());
        for section in &lazy {
            assert_eq!(
                section.content_hash, None,
                "compute_hash=false must never compute a content_hash"
            );
        }
        // Structure (everything except content_hash) must be identical
        // between the two calls.
        for (h, l) in hashed.iter().zip(lazy.iter()) {
            assert_eq!(h.seq, l.seq);
            assert_eq!(h.heading, l.heading);
            assert_eq!(h.level, l.level);
            assert_eq!(h.byte_offset, l.byte_offset);
            assert_eq!(h.byte_length, l.byte_length);
            assert!(h.content_hash.is_some());
        }
    }

    #[test]
    fn compute_sections_no_headings_is_single_section_at_offset_zero() {
        let body = "Just plain text.\n";
        let split_doc = split(body, 2).unwrap();
        let sections = compute_sections(&split_doc, true);

        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].seq, 0);
        assert_eq!(sections[0].byte_offset, 0);
        assert_eq!(sections[0].byte_length, body.len());
        assert_eq!(sections[0].heading, "");
        assert_eq!(sections[0].level, 0);
    }

    // -- t370.15 (PR-4, wiki/240-performance-design.md §6): whole-document
    // `content_hash` composed from section hashes rather than an independent
    // `lexsim::content_hash(whole_body)` pass --

    /// [`compose_doc_hash`] is a pure, deterministic function of the ordered
    /// section hashes: the same section hash list always composes to the
    /// same document hash, including the seq-0 preamble.
    #[test]
    fn compose_doc_hash_is_deterministic_and_includes_preamble() {
        let body = "Preamble.\n\n## A\nBody A\n## B\nBody B\n";
        let split_doc = split(body, 2).unwrap();
        let sections = compute_sections(&split_doc, true);

        let first = compose_doc_hash(&sections);
        let second = compose_doc_hash(&sections);
        assert_eq!(
            first, second,
            "must be a pure function of the section hashes"
        );

        // Dropping the seq-0 preamble section changes the composed hash —
        // proves the preamble is actually folded in, not skipped.
        let without_preamble = compose_doc_hash(&sections[1..]);
        assert_ne!(
            first, without_preamble,
            "composed hash must depend on the seq-0 preamble section too"
        );
    }

    /// Changing exactly one section's content_hash (simulating an edit to
    /// just that section) changes the composed document hash — the whole
    /// point of using this as a document-level change signal.
    #[test]
    fn compose_doc_hash_changes_when_any_section_hash_changes() {
        let body = "Preamble.\n\n## A\nBody A\n## B\nBody B\n";
        let split_doc = split(body, 2).unwrap();
        let mut sections = compute_sections(&split_doc, true);
        let before = compose_doc_hash(&sections);

        // Mutate only section B's hash (as if only it had been rehashed after
        // an edit) — the other sections' hashes are untouched.
        sections[2].content_hash = Some("deadbeefdeadbeef".to_string());
        let after = compose_doc_hash(&sections);

        assert_ne!(
            before, after,
            "composing must react to a single changed section hash"
        );
    }

    /// Two structurally-identical section lists (same hashes in the same
    /// order) compose to the same document hash even if they come from two
    /// unrelated `compute_sections` calls — i.e. this doesn't leak anything
    /// beyond the hash values themselves (no hidden dependency on `doc` id,
    /// timestamps, etc.).
    #[test]
    fn compose_doc_hash_depends_only_on_section_hashes_not_identity() {
        let body = "Preamble.\n\n## A\nBody A\n";
        let split_doc = split(body, 2).unwrap();
        let sections_a = compute_sections(&split_doc, true);
        let sections_b = compute_sections(&split_doc, true);

        assert_eq!(compose_doc_hash(&sections_a), compose_doc_hash(&sections_b));
    }

    /// `compute_sections` must route every section's hash computation
    /// through [`hash_section_body`] (not call `lexsim::content_hash`
    /// directly) — this is the seam
    /// `mcp::handlers::docs::handle_doc_update_section`'s incremental rehash
    /// also uses, so tests can assert exactly how many sections actually paid
    /// the tokenize cost for a given edit (t370.15 PR-4 acceptance
    /// criterion).
    #[test]
    fn compute_sections_hashing_goes_through_hash_section_body_counter() {
        reset_section_hash_compute_count_for_test();
        let body = "Preamble.\n\n## A\nBody A\n## B\nBody B\n";
        let split_doc = split(body, 2).unwrap();
        let sections = compute_sections(&split_doc, true);

        assert_eq!(
            section_hash_compute_count_for_test(),
            sections.len(),
            "one hash_section_body call per section when compute_hash=true"
        );

        reset_section_hash_compute_count_for_test();
        let _ = compute_sections(&split_doc, false);
        assert_eq!(
            section_hash_compute_count_for_test(),
            0,
            "compute_hash=false must never call hash_section_body"
        );
    }

    // -- t370.15 (PR-4): `compute_sections_after_splice` — only the edited
    // section is rehashed after `handle_doc_update_section` splices new text
    // in --

    #[test]
    fn compute_sections_after_splice_reuses_unaffected_section_hashes() {
        let old_body = "Preamble.\n\n## A\nBody A\n## B\nBody B\n## C\nBody C\n";
        let old_split = split(old_body, 2).unwrap();
        let old_sections = compute_sections(&old_split, true);

        // Splice new text into section B (seq 2) only — same heading, same
        // number of fragments overall.
        let new_content = "## B\nUpdated body B\n";
        let new_body = "Preamble.\n\n## A\nBody A\n".to_string() + new_content + "## C\nBody C\n";
        let new_split = split(&new_body, 2).unwrap();

        reset_section_hash_compute_count_for_test();
        let new_sections = compute_sections_after_splice(&new_split, &old_sections, 2, new_content);

        assert_eq!(
            section_hash_compute_count_for_test(),
            1,
            "only the edited section's body may be tokenized, not the other two"
        );
        assert_eq!(new_sections.len(), old_sections.len());
        // Unaffected sections keep their pre-edit hash exactly.
        assert_eq!(new_sections[0].content_hash, old_sections[0].content_hash);
        assert_eq!(new_sections[1].content_hash, old_sections[1].content_hash);
        assert_eq!(new_sections[3].content_hash, old_sections[3].content_hash);
        // The edited section's hash reflects its new content, not the old.
        assert_eq!(
            new_sections[2].content_hash,
            Some(hash_section_body(new_content))
        );
        assert_ne!(new_sections[2].content_hash, old_sections[2].content_hash);
    }

    /// When the spliced-in content introduces its own extra heading boundary
    /// (shifting every later section's `seq`), the fast reuse path is not
    /// safe — every section must be rehashed fresh instead of misattributing
    /// a stale hash to a section that's no longer at the same position.
    #[test]
    fn compute_sections_after_splice_falls_back_when_section_count_changes() {
        let old_body = "Preamble.\n\n## A\nBody A\n## B\nBody B\n";
        let old_split = split(old_body, 2).unwrap();
        let old_sections = compute_sections(&old_split, true);

        // Splicing this into section A's range adds a *new* heading, so the
        // new body has one more section than the old one.
        let new_content = "## A\nBody A\n## A2\nInserted section\n";
        let new_body = new_content.to_string() + "## B\nBody B\n";
        let new_split = split(&new_body, 2).unwrap();

        reset_section_hash_compute_count_for_test();
        let new_sections = compute_sections_after_splice(&new_split, &old_sections, 1, new_content);

        assert_eq!(
            new_sections.len(),
            new_split.fragments.len(),
            "fallback still returns one SectionIndex per fragment of the new body"
        );
        assert_eq!(
            section_hash_compute_count_for_test(),
            new_sections.len(),
            "fallback must rehash every section fresh, not reuse any stale positional hash"
        );
        assert!(new_sections.iter().all(|s| s.content_hash.is_some()));
    }

    /// Defense in depth: even when the section *count* matches, a structural
    /// mismatch at an unaffected position (heading/level/byte_length changed
    /// unexpectedly) must also fall back to a full rehash rather than trust
    /// a positionally-reused hash that might not actually describe the same
    /// bytes anymore.
    #[test]
    fn compute_sections_after_splice_falls_back_on_structural_mismatch_at_unaffected_position() {
        let body = "Preamble.\n\n## A\nBody A\n## B\nBody B\n";
        let split_doc = split(body, 2).unwrap();
        let mut old_sections = compute_sections(&split_doc, true);
        // Corrupt an *unaffected* section's recorded heading so it no longer
        // matches what `compute_sections_after_splice` will see for the same
        // position in the new split — simulates the "sections desynced from
        // body" edge case the structural check guards against.
        old_sections[1].heading = "Tampered".to_string();

        reset_section_hash_compute_count_for_test();
        let new_content = "## B\nUpdated body B\n";
        let new_body = "Preamble.\n\n## A\nBody A\n".to_string() + new_content;
        let new_split = split(&new_body, 2).unwrap();
        let new_sections = compute_sections_after_splice(&new_split, &old_sections, 2, new_content);

        assert_eq!(
            section_hash_compute_count_for_test(),
            new_sections.len(),
            "a structural mismatch at an unaffected position must trigger a full rehash"
        );
    }
}
