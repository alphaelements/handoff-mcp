//! Reassembles fragment bodies (in `seq` order) back into a single Markdown
//! document.
//!
//! Per wiki/130-document-management.md §8: reassembly is a plain
//! concatenation of fragment bodies. Heading markers are never re-synthesized
//! (they are already part of each fragment's raw body, preserved verbatim by
//! [`split`](super::split::split)), and `byte_offset`/`byte_length` metadata
//! is diagnostic only — never used as the source of truth for reassembly,
//! since fragments can be edited or reordered independently after a split.

/// Concatenates `fragment_bodies` (already in `seq` order) into a single
/// `String`. This is the exact inverse of [`split`](super::split::split)'s
/// fragment slicing: `reassemble(split(body).fragments.map(|f| f.body)) ==
/// body` for any `body` that does not have a BOM or frontmatter stripped out
/// (callers that persisted `has_bom`/`frontmatter` separately are expected to
/// re-prepend them before calling this, mirroring how `doc_save` /
/// `doc_reassemble` will do it once storage I/O lands).
pub fn reassemble(fragment_bodies: &[&str]) -> String {
    fragment_bodies.concat()
}
