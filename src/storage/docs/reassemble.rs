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

/// Bounds/UTF-8-char-boundary check shared by [`extract_section`] and
/// [`extract_section_trusted`]: resolves `section`'s byte range against the
/// current `body`, returning `Err` (never panicking) if the range doesn't
/// fit or lands mid-character.
fn section_slice_bounds(body: &str, section: &SectionIndex) -> Result<(usize, usize)> {
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
    Ok((section.byte_offset, end))
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
    let (start, end) = section_slice_bounds(body, section)?;
    let slice = &body[start..end];
    let Some(expected_hash) = &section.content_hash else {
        bail!(
            "section seq={} has no content_hash computed — caller must resolve the document \
             through a `_hashed` read (e.g. read_doc_hashed) before calling extract_section",
            section.seq
        );
    };
    let actual_hash = lexsim::content_hash(slice);
    if actual_hash != *expected_hash {
        bail!(
            "section seq={} content_hash mismatch: expected {}, got {} \
             (document body has drifted since sections were indexed)",
            section.seq,
            expected_hash,
            actual_hash
        );
    }
    Ok(slice)
}

/// Like [`extract_section`], but skips the `content_hash` drift
/// recomputation — a full `lexsim::content_hash` (tokenize) pass over the
/// section's text that `extract_section` pays on *every* call to guard
/// against `body` having drifted out from under `section`'s byte offsets
/// since they were indexed.
///
/// Use this only when `body` and `section` are already guaranteed mutually
/// consistent *by construction*, not merely "probably still fresh" — e.g.
/// both came from the same
/// [`super::read_doc_with_body_hashed`]/[`super::read_all_docs_with_bodies_hashed`]
/// call, whose single-read implementation only ever pairs a `DocMetadata`
/// with the exact body bytes its `sections` were computed from (see that
/// function's doc comment for how it detects and recovers from a torn
/// concurrent write). Still performs the same bounds / UTF-8
/// char-boundary checks as [`extract_section`] (a bug could still produce an
/// out-of-range `SectionIndex`) — only the tokenize-based hash
/// re-verification is skipped, since a caller with the by-construction
/// guarantee above can never observe it fail.
pub fn extract_section_trusted<'a>(body: &'a str, section: &SectionIndex) -> Result<&'a str> {
    let (start, end) = section_slice_bounds(body, section)?;
    Ok(&body[start..end])
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
            content_hash: Some(lexsim::content_hash("## A\nBody A\n")),
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
            content_hash: Some(lexsim::content_hash(body)),
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
            content_hash: Some(lexsim::content_hash(full_body)),
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
            content_hash: Some(lexsim::content_hash(original)),
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

    /// t370.8 (P-M1, wiki/240-performance-design.md §4): a section resolved
    /// through a lazy (non-`_hashed`) read has `content_hash: None` — calling
    /// `extract_section` on it must fail loudly with a clear message, not
    /// silently succeed or panic (the caller forgot to resolve the document
    /// through a `_hashed` read first).
    #[test]
    fn extract_section_errors_clearly_when_content_hash_not_computed() {
        let body = "Preamble.\n## A\nBody A\n";
        let section = SectionIndex {
            seq: 1,
            heading: "A".to_string(),
            level: 2,
            byte_offset: 10,
            byte_length: "## A\nBody A\n".len(),
            content_hash: None,
        };
        let err = extract_section(body, &section).expect_err("must fail, not panic or succeed");
        assert!(
            err.to_string().contains("no content_hash computed"),
            "error must explain the section wasn't resolved with a hash: {err}"
        );
    }

    // -- extract_section_trusted (t370.9, wiki/240-performance-design.md §6
    // PR-5): the bounds-only fast path for callers with a by-construction
    // metadata/body consistency guarantee (`read_all_docs_with_bodies_hashed`
    // et al). --

    #[test]
    fn extract_section_trusted_slices_by_byte_offset() {
        let body = "Preamble.\n## A\nBody A\n";
        let section = SectionIndex {
            seq: 1,
            heading: "A".to_string(),
            level: 2,
            byte_offset: 10,
            byte_length: "## A\nBody A\n".len(),
            content_hash: Some(lexsim::content_hash("## A\nBody A\n")),
        };
        assert_eq!(
            extract_section_trusted(body, &section).unwrap(),
            "## A\nBody A\n"
        );
    }

    /// The whole point of `extract_section_trusted`: unlike `extract_section`,
    /// it must succeed with `content_hash: None` — it never looks at the
    /// field at all.
    #[test]
    fn extract_section_trusted_succeeds_even_when_content_hash_is_none() {
        let body = "Preamble.\n## A\nBody A\n";
        let section = SectionIndex {
            seq: 1,
            heading: "A".to_string(),
            level: 2,
            byte_offset: 10,
            byte_length: "## A\nBody A\n".len(),
            content_hash: None,
        };
        assert_eq!(
            extract_section_trusted(body, &section).unwrap(),
            "## A\nBody A\n"
        );
    }

    /// Documents the accepted trade-off explicitly: `extract_section` would
    /// reject this edited-in-place body (hash mismatch), but
    /// `extract_section_trusted` returns the (now-wrong) slice silently by
    /// design — callers must only use it when body/section are guaranteed
    /// consistent by construction, not for arbitrarily-sourced pairs.
    #[test]
    fn extract_section_trusted_does_not_hash_check_by_design() {
        let original = "## A\nOriginal body\n";
        let section = SectionIndex {
            seq: 1,
            heading: "A".to_string(),
            level: 2,
            byte_offset: 0,
            byte_length: original.len(),
            content_hash: Some(lexsim::content_hash(original)),
        };
        let edited = "## A\nEditedd  body\n";
        assert_eq!(edited.len(), original.len());
        assert!(extract_section(edited, &section).is_err());
        assert_eq!(extract_section_trusted(edited, &section).unwrap(), edited);
    }

    /// Bounds safety must still hold: an out-of-range `SectionIndex` must
    /// error, never panic, exactly like `extract_section`.
    #[test]
    fn extract_section_trusted_errors_when_body_truncated_shorter_than_range() {
        let full_body = "## A\nBody A that is fairly long\n";
        let section = SectionIndex {
            seq: 1,
            heading: "A".to_string(),
            level: 2,
            byte_offset: 0,
            byte_length: full_body.len(),
            content_hash: Some(lexsim::content_hash(full_body)),
        };
        let drifted_body = "## A\n";
        let result = extract_section_trusted(drifted_body, &section);
        assert!(result.is_err(), "expected Err, got {result:?}");
    }
}
