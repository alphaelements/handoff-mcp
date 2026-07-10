//! Golden fixture tests for Markdown document splitting and reassembly
//! (`src/storage/docs/split.rs` + `reassemble.rs`).
//!
//! Each fixture asserts:
//! 1. `split()` produces the expected fragment boundaries (heading/level/body).
//! 2. `reassemble()` on the split output reproduces the original bytes exactly
//!    (`save(body) -> reassemble()` is a byte-identical round trip per
//!    wiki/130-document-management.md §8).

use handoff_mcp::storage::docs::reassemble::reassemble;
use handoff_mcp::storage::docs::split::split;

/// Round-trips `body` through split -> reassemble and asserts byte identity.
///
/// `reassemble()` only concatenates fragment bodies; a stripped BOM or YAML
/// frontmatter (reported separately by `split()`) must be re-prepended by
/// the caller, mirroring how `doc_save` / `doc_reassemble` will do it once
/// storage I/O lands (wiki/130-document-management.md §8).
fn assert_roundtrip(body: &str) {
    assert_roundtrip_at_level(body, 2);
}

fn assert_roundtrip_at_level(body: &str, split_level: u8) {
    let split_doc = split(body, split_level).expect("split should succeed");
    let fragment_bodies: Vec<&str> = split_doc.fragments.iter().map(|f| f.body).collect();
    let mut reassembled = reassemble(&fragment_bodies);
    if let Some(frontmatter) = split_doc.frontmatter {
        let eol = if split_doc.line_ending == "crlf" {
            "\r\n"
        } else {
            "\n"
        };
        reassembled = format!("---{eol}{frontmatter}---{reassembled}");
    }
    if split_doc.has_bom {
        reassembled = format!("\u{FEFF}{reassembled}");
    }
    assert_eq!(
        reassembled, body,
        "reassemble(split(body)) + BOM/frontmatter reattachment must equal body byte-for-byte"
    );
}

/// Case 1: fenced code block containing `##` must not be misread as a heading
/// boundary (the reason pulldown-cmark was chosen over regex splitting).
#[test]
fn fenced_code_block_with_hashes_is_not_split() {
    let body = "# Title\n\nIntro.\n\n## Section A\n\nBody A.\n\n```\n## not a heading\n```\n\n## Section B\nBody B.\n";
    let split_doc = split(body, 2).expect("split should succeed");

    // seq0 (empty preamble) + Title (H1) + Section A + Section B == 4
    // fragments; the fenced `##` must not have produced a 5th fragment.
    assert_eq!(split_doc.fragments.len(), 4);
    assert_eq!(split_doc.fragments[1].heading.as_deref(), Some("Title"));
    assert_eq!(split_doc.fragments[2].heading.as_deref(), Some("Section A"));
    assert!(split_doc.fragments[2].body.contains("## not a heading"));
    assert_eq!(split_doc.fragments[3].heading.as_deref(), Some("Section B"));

    assert_roundtrip(body);
}

/// Case 2: YAML frontmatter is extracted separately and excluded from
/// fragment seq 0, per spec §5.1 scope rule 6.
#[test]
fn yaml_frontmatter_is_extracted_and_excluded_from_seq0() {
    let body = "---\ntitle: Foo\ntags: [a, b]\n---\n\n# Title\n\nIntro.\n\n## Sec\nBody.\n";
    let split_doc = split(body, 2).expect("split should succeed");

    let frontmatter = split_doc
        .frontmatter
        .expect("frontmatter should be detected");
    assert!(frontmatter.contains("title: Foo"));
    assert!(!split_doc.fragments[0].body.contains("title: Foo"));

    assert_roundtrip(body);
}

/// Case 3: consecutive headings with no body text between them must still
/// produce a (possibly empty) fragment for the first heading.
#[test]
fn consecutive_headings_with_empty_body() {
    let body = "## A\n## B\nBody B\n";
    let split_doc = split(body, 2).expect("split should succeed");

    // seq0 (empty preamble) + A (empty body) + B.
    assert_eq!(split_doc.fragments.len(), 3);
    assert_eq!(split_doc.fragments[1].heading.as_deref(), Some("A"));
    assert_eq!(split_doc.fragments[1].body, "## A\n");
    assert_eq!(split_doc.fragments[2].heading.as_deref(), Some("B"));

    assert_roundtrip(body);
}

/// Case 4: a UTF-8 BOM at the start of the document must be recorded
/// (`has_bom`) and preserved byte-for-byte on reassembly.
#[test]
fn bom_prefixed_document_round_trips() {
    let body = "\u{FEFF}# Title\n\nIntro.\n\n## Sec\nBody.\n";
    let split_doc = split(body, 2).expect("split should succeed");

    assert!(split_doc.has_bom, "BOM should be detected");
    assert_roundtrip(body);
}

/// Case 5: a document with no headings at all is a single preamble fragment.
#[test]
fn document_with_no_headings_is_single_fragment() {
    let body = "Just plain text.\n\nNo headings here at all.\n";
    let split_doc = split(body, 2).expect("split should succeed");

    assert_eq!(split_doc.fragments.len(), 1);
    assert_eq!(split_doc.fragments[0].seq, 0);
    assert!(split_doc.fragments[0].heading.is_none());
    assert_eq!(split_doc.fragments[0].body, body);

    assert_roundtrip(body);
}

/// Case 6: nested headings below `split_level` stay embedded in their parent
/// fragment rather than becoming their own fragment.
#[test]
fn nested_headings_below_split_level_stay_in_parent() {
    let body = "## A\nBody A\n### Nested\nNested body\n## B\nBody B\n";
    let split_doc = split(body, 2).expect("split should succeed");

    assert_eq!(split_doc.fragments.len(), 3);
    assert_eq!(split_doc.fragments[1].heading.as_deref(), Some("A"));
    assert!(split_doc.fragments[1].body.contains("### Nested"));
    assert!(split_doc.fragments[1].body.contains("Nested body"));
    assert_eq!(split_doc.fragments[2].heading.as_deref(), Some("B"));

    assert_roundtrip(body);
}

/// Case 7: CRLF line endings are detected and preserved verbatim (no LF
/// normalization anywhere in the split/reassemble path).
#[test]
fn crlf_line_endings_are_preserved() {
    let body = "# Title\r\n\r\nIntro.\r\n\r\n## Sec\r\nBody.\r\n";
    let split_doc = split(body, 2).expect("split should succeed");

    assert_eq!(split_doc.line_ending, "crlf");
    assert!(split_doc.fragments.iter().any(|f| f.body.contains("\r\n")));
    assert!(
        !split_doc
            .fragments
            .iter()
            .any(|f| f.body.contains('\n') && !f.body.contains("\r\n")),
        "no bare LF should appear in a CRLF document's fragments"
    );

    assert_roundtrip(body);
}

/// Case 8: mixed CRLF/LF within a single document is rejected per spec §5.1
/// scope rule ("mixed CRLF ... はエラー拒否").
#[test]
fn mixed_line_endings_are_rejected() {
    let body = "# Title\r\n\nMixed line endings.\n";
    let err = split(body, 2).expect_err("mixed line endings must be rejected");
    let message = err.to_string().to_lowercase();
    assert!(
        message.contains("line ending") || message.contains("crlf") || message.contains("mixed"),
        "error message should mention the mixed line ending problem: {message}"
    );
}

/// Case 9: ATX heading with trailing whitespace after the heading text is
/// still detected as a boundary, and the raw bytes (incl. trailing
/// whitespace) survive the round trip untouched.
#[test]
fn atx_heading_trailing_whitespace_round_trips() {
    let body = "Intro.\n\n##   Section   \nBody.\n";
    let split_doc = split(body, 2).expect("split should succeed");

    assert_eq!(split_doc.fragments.len(), 2);
    assert_eq!(split_doc.fragments[1].heading.as_deref(), Some("Section"));
    assert!(split_doc.fragments[1].body.starts_with("##   Section   \n"));

    assert_roundtrip(body);
}

/// Case 10: a document with no trailing newline at EOF must still round-trip
/// exactly (no newline is invented or dropped).
#[test]
fn no_trailing_newline_round_trips() {
    let body = "Intro\n\n## Sec\nBody without trailing newline";
    let split_doc = split(body, 2).expect("split should succeed");

    assert_eq!(split_doc.fragments.len(), 2);
    assert_roundtrip(body);
}

/// Case 11: setext headings (`===` / `---` underline style) must NOT be
/// recognized as split boundaries per spec §5.1 scope restriction ("setext
/// 見出し（`===` / `---` 下線方式）は分割境界として認識しない"). They must
/// stay embedded verbatim in the fragment body that contains them.
#[test]
fn setext_headings_are_not_split_boundaries() {
    let body = "Title\n=====\n\nIntro text.\n\nSection\n-------\n\nBody.\n";
    let split_doc = split(body, 2).expect("split should succeed");

    // Only the seq-0 preamble fragment: no ATX heading anywhere in this
    // document, so setext headings must not create additional fragments.
    assert_eq!(
        split_doc.fragments.len(),
        1,
        "setext headings must not be treated as split boundaries"
    );
    assert!(split_doc.fragments[0].body.contains("Title\n====="));
    assert!(split_doc.fragments[0].body.contains("Section\n-------"));

    assert_roundtrip(body);
}

/// Case 12: CRLF line endings combined with YAML frontmatter must extract
/// only the inner YAML (fences excluded) and round-trip byte-identically.
/// Regression test for a line-ending-unaware `strip_prefix("---\n")` that
/// silently fell back to including the fences verbatim for CRLF documents.
#[test]
fn crlf_with_frontmatter_round_trips() {
    let body = "---\r\ntitle: X\r\n---\r\n\r\n# T\r\n\r\nBody\r\n";
    let split_doc = split(body, 2).expect("split should succeed");

    assert_eq!(split_doc.line_ending, "crlf");
    let frontmatter = split_doc
        .frontmatter
        .expect("frontmatter should be detected for CRLF documents");
    assert_eq!(frontmatter, "title: X\r\n");
    assert!(
        !frontmatter.contains("---"),
        "frontmatter must exclude the `---` fences: {frontmatter:?}"
    );

    assert_roundtrip(body);
}

/// Custom `split_level` override: raising `split_level` to 3 makes `###`
/// headings split boundaries too (at `split_level=2` they stay embedded in
/// their parent fragment — see `nested_headings_below_split_level_stay_in_parent`).
#[test]
fn split_level_override_splits_on_h3_instead_of_h2() {
    let body = "## A\nBody A\n### Nested\nNested body\n## B\nBody B\n";
    let split_doc = split(body, 3).expect("split should succeed");

    // seq0 (preamble, empty) + "## A" + "### Nested" (now its own fragment) + "## B"
    assert_eq!(split_doc.fragments.len(), 4);
    assert_eq!(split_doc.fragments[1].heading.as_deref(), Some("A"));
    assert_eq!(split_doc.fragments[1].body, "## A\nBody A\n");
    assert_eq!(split_doc.fragments[2].heading.as_deref(), Some("Nested"));
    assert!(split_doc.fragments[2].body.contains("Nested body"));
    assert_eq!(split_doc.fragments[3].heading.as_deref(), Some("B"));

    assert_roundtrip_at_level(body, 3);
}
