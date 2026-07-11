//! v5 (spec §3.1): a document's `.md` file *is* the original body, so
//! reassembly from physical fragment files is no longer needed. What
//! remains useful is byte-offset section slicing: given a document's full
//! body and a [`SectionIndex`](super::model::SectionIndex) entry (as
//! computed by [`super::split::compute_sections`]), extract just that
//! section's text.

use anyhow::{bail, Result};

use super::model::SectionIndex;

/// Concatenates `fragment_bodies` (already in order) into a single `String`.
/// Kept as a small utility for callers that still hold a list of body slices
/// (e.g. `split::SplitDocument::fragments`) and want to reconstruct the full
/// body without going through file I/O.
pub fn reassemble(fragment_bodies: &[&str]) -> String {
    fragment_bodies.concat()
}

/// Extracts one section's text from `body` by byte-offset slice, per
/// `section.byte_offset`/`section.byte_length`.
///
/// `body` must be the same byte sequence the section indexes were computed
/// against (i.e. the document body after BOM/frontmatter stripping — the
/// same body persisted to `_doc.<slug>.md`). Since `body` is read fresh from
/// disk on every call, it can drift out from under `section` (edited
/// out-of-band, e.g. truncated) between when `sections` was computed and
/// when this is called — so this returns `Err` rather than panicking:
///
/// - `Err` if `section`'s byte range doesn't fit within `body` at all (would
///   otherwise panic on slice-index-out-of-bounds), or lands on a non-UTF8
///   char boundary.
/// - `Err` if the range fits but `section.content_hash` doesn't match the
///   hash of the extracted slice (body changed but still long enough to
///   slice) — this is the drift check the doc comment used to merely
///   recommend callers perform themselves; it's now built in so every
///   caller gets it for free.
pub fn extract_section<'a>(body: &'a str, section: &SectionIndex) -> Result<&'a str> {
    let end = section
        .byte_offset
        .checked_add(section.byte_length)
        .filter(|&end| end <= body.len())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "section seq={} byte range [{}, {}) is out of bounds for body of length {} \
                 (document body has drifted since sections were indexed)",
                section.seq,
                section.byte_offset,
                section.byte_offset + section.byte_length,
                body.len()
            )
        })?;
    if !body.is_char_boundary(section.byte_offset) || !body.is_char_boundary(end) {
        bail!(
            "section seq={} byte range [{}, {}) does not fall on a UTF-8 character \
             boundary in the current body (document body has drifted)",
            section.seq,
            section.byte_offset,
            end
        );
    }
    let slice = &body[section.byte_offset..end];
    let actual_hash = lexsim::content_hash(slice);
    if actual_hash != section.content_hash {
        bail!(
            "section seq={} content_hash mismatch: expected {}, got {} \
             (document body has drifted since sections were indexed)",
            section.seq,
            section.content_hash,
            actual_hash
        );
    }
    Ok(slice)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reassemble_concatenates_bodies() {
        assert_eq!(reassemble(&["a", "b", "c"]), "abc");
        assert_eq!(reassemble(&[]), "");
    }

    #[test]
    fn extract_section_slices_by_byte_offset() {
        let body = "Preamble.\n## A\nBody A\n";
        let section = SectionIndex {
            seq: 1,
            heading: "A".to_string(),
            level: 2,
            byte_offset: 10,
            byte_length: "## A\nBody A\n".len(),
            content_hash: lexsim::content_hash("## A\nBody A\n"),
        };
        assert_eq!(extract_section(body, &section).unwrap(), "## A\nBody A\n");
    }

    #[test]
    fn extract_section_at_start_of_body() {
        let body = "Preamble text.\n";
        let section = SectionIndex {
            seq: 0,
            heading: String::new(),
            level: 0,
            byte_offset: 0,
            byte_length: body.len(),
            content_hash: lexsim::content_hash(body),
        };
        assert_eq!(extract_section(body, &section).unwrap(), body);
    }

    /// Regression test for a MAJOR bug found in review: `extract_section`
    /// used to slice `body[byte_offset..byte_offset+byte_length]`
    /// unconditionally, which panics with a slice-bounds error when `body`
    /// has drifted (e.g. truncated by an out-of-band edit) so it's shorter
    /// than the section's recorded range. Both `doc_get(format=section)` and
    /// `doc_query` call this with a `body` freshly read from disk on every
    /// request, so a panic here broke the JSON-RPC contract silently (no
    /// response ever sent for that request). Must return `Err`, not panic.
    #[test]
    fn extract_section_errors_when_body_truncated_shorter_than_range() {
        let full_body = "## A\nBody A that is fairly long\n";
        let section = SectionIndex {
            seq: 1,
            heading: "A".to_string(),
            level: 2,
            byte_offset: 0,
            byte_length: full_body.len(),
            content_hash: lexsim::content_hash(full_body),
        };
        // Simulate drift: body on disk was truncated independently of the
        // stored section index.
        let drifted_body = "## A\n";
        let result = extract_section(drifted_body, &section);
        assert!(result.is_err(), "expected Err, got {result:?}");
    }

    /// Companion drift case: body is long enough to slice without an
    /// out-of-bounds panic, but the content at that range no longer matches
    /// what was indexed (edited in place). Must be caught by the
    /// `content_hash` check, not silently return stale/wrong text.
    #[test]
    fn extract_section_errors_when_body_edited_in_place_hash_mismatch() {
        let original = "## A\nOriginal body\n";
        let section = SectionIndex {
            seq: 1,
            heading: "A".to_string(),
            level: 2,
            byte_offset: 0,
            byte_length: original.len(),
            content_hash: lexsim::content_hash(original),
        };
        let edited = "## A\nEditedd  body\n";
        assert_eq!(
            edited.len(),
            original.len(),
            "test fixture must keep byte_length identical to isolate the hash check"
        );
        let result = extract_section(edited, &section);
        assert!(
            result.is_err(),
            "expected Err on hash mismatch, got {result:?}"
        );
    }
}
