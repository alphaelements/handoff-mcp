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
/// range), `byte_length`, and a content hash of each fragment's body slice.
/// Performs no file I/O — this is purely an in-memory transform.
pub fn compute_sections(split_doc: &SplitDocument<'_>) -> Vec<SectionIndex> {
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
                content_hash: lexsim::content_hash(frag.body),
            };
            offset += frag.body.len();
            section
        })
        .collect()
}

/// A single heading boundary: byte offset (into the frontmatter-stripped
/// body) where the heading line starts, its level, and its trimmed text.
struct HeadingBound {
    start: usize,
    level: u8,
    text: String,
}

/// Runs the pulldown-cmark offset-tracking parser and collects the byte
/// start of every ATX heading whose level is `<= split_level`. Fenced code
/// blocks and block quotes are handled by the parser itself, so a `##`
/// inside a fence never appears here.
fn collect_heading_bounds(text: &str, split_level: u8) -> Vec<HeadingBound> {
    let parser = Parser::new_ext(text, Options::all());
    let mut bounds = Vec::new();
    let mut in_qualifying_heading: Option<(usize, u8)> = None;
    let mut heading_text = String::new();

    for (event, range) in parser.into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let numeric_level = heading_level_to_u8(level);
                if numeric_level <= split_level && is_atx_heading(text, &range) {
                    in_qualifying_heading = Some((range.start, numeric_level));
                    heading_text.clear();
                }
            }
            Event::Text(ref t) | Event::Code(ref t) if in_qualifying_heading.is_some() => {
                heading_text.push_str(t);
            }
            Event::End(pulldown_cmark::TagEnd::Heading(_)) => {
                if let Some((start, level)) = in_qualifying_heading.take() {
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
        let sections = compute_sections(&split_doc);

        assert_eq!(sections.len(), split_doc.fragments.len());
        let mut expected_offset = 0usize;
        for (section, frag) in sections.iter().zip(split_doc.fragments.iter()) {
            assert_eq!(section.seq, frag.seq);
            assert_eq!(section.level, frag.level);
            assert_eq!(section.heading, frag.heading.clone().unwrap_or_default());
            assert_eq!(section.byte_offset, expected_offset);
            assert_eq!(section.byte_length, frag.body.len());
            assert_eq!(section.content_hash, lexsim::content_hash(frag.body));
            expected_offset += frag.body.len();
        }
    }

    #[test]
    fn compute_sections_offsets_slice_the_split_body_correctly() {
        let body = "Preamble.\n\n## A\nBody A\n## B\nBody B\n";
        let split_doc = split(body, 2).unwrap();
        let sections = compute_sections(&split_doc);

        // Reconstruct the after-frontmatter body by concatenating fragments,
        // then verify each section's byte_offset/byte_length slices it back
        // out identically to the fragment body it was computed from.
        let full: String = split_doc.fragments.iter().map(|f| f.body).collect();
        for (section, frag) in sections.iter().zip(split_doc.fragments.iter()) {
            let slice = &full[section.byte_offset..section.byte_offset + section.byte_length];
            assert_eq!(slice, frag.body);
        }
    }

    #[test]
    fn compute_sections_no_headings_is_single_section_at_offset_zero() {
        let body = "Just plain text.\n";
        let split_doc = split(body, 2).unwrap();
        let sections = compute_sections(&split_doc);

        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].seq, 0);
        assert_eq!(sections[0].byte_offset, 0);
        assert_eq!(sections[0].byte_length, body.len());
        assert_eq!(sections[0].heading, "");
        assert_eq!(sections[0].level, 0);
    }
}
