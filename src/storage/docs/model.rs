//! Data model for documents (wiki/130-document-management.md v5 rearchitecture).
//! One `DocMetadata` per `_doc.<slug>.json`, paired with its full body at
//! `_doc.<slug>.md` (see `super` module docs). Sections are computed
//! in-memory (byte offsets into the body) rather than split into physical
//! fragment files.

use serde::{Deserialize, Serialize};

/// Current document schema version. Bump when `DocMetadata` changes shape in
/// a way that needs migration handling on read.
pub const DOC_SCHEMA_VERSION: u32 = 2;

/// Valid `doc_type` values (spec §4.1, extensible via `config.toml`
/// `settings.doc_types.types` — this list is the storage-layer default set,
/// not an enforced enum, so a project-configured custom type still
/// round-trips through `serde` even if it is not in this list).
pub const VALID_DOC_TYPES: &[&str] = &["spec", "design", "adr", "guide", "note"];

/// Valid `auto_inject` values (spec §7.2.1).
pub const VALID_AUTO_INJECT: &[&str] = &["auto", "full", "outline", "none"];

/// Valid `related[].rel` relationship kinds (spec §4.3).
pub const VALID_RELATIONS: &[&str] = &[
    "supersedes",
    "references",
    "implements",
    "extends",
    "conflicts",
];

/// A document: the family-tree node and section manifest persisted at
/// `_doc.<slug>.json` (spec §4.1, v5). The full Markdown body lives
/// unsplit at `_doc.<slug>.md`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocMetadata {
    /// Schema version (= [`DOC_SCHEMA_VERSION`]).
    pub version: u32,
    /// Stable id: `doc-YYYYMMDD-HHMMSS-NNNNNN`. Kept internally for
    /// family-tree/task-link references; file naming uses `slug` instead.
    pub id: String,
    /// Human-readable file-naming slug (`[a-z0-9-]`, max 60 chars). Required
    /// on creation; used to build `_doc.<slug>.json` / `_doc.<slug>.md`.
    pub slug: String,
    pub title: String,
    /// One of [`VALID_DOC_TYPES`] by convention (spec: `spec | design | adr |
    /// guide | note`), not enforced here — validation belongs to the
    /// `doc_save` handler (t96) so a project-configured custom type can still
    /// be persisted.
    pub doc_type: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub scope_paths: Vec<String>,

    /// Family tree: `None` = root document.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// Ordered child document ids.
    #[serde(default)]
    pub children: Vec<String>,
    /// Sibling/relative relationships (semantic, not structural).
    #[serde(default)]
    pub related: Vec<DocRelation>,

    /// Auto-injection control (spec §7.2.1): `"auto"` | `"full"` |
    /// `"outline"` | `"none"`. Defaults to `"auto"`.
    #[serde(default = "default_auto_inject")]
    pub auto_inject: String,

    /// Task ids this document is linked to (bidirectional — the task side
    /// mirrors this via `TaskLink { link_type: "doc" }`, synced by
    /// `crate::storage::tasks::sync_doc_task_links`).
    #[serde(default)]
    pub task_ids: Vec<String>,

    /// Source tracking for reversibility (spec §4.1 / §8).
    #[serde(default)]
    pub source: DocSource,

    /// `true` when the authored body started with a UTF-8 BOM (spec §5.1
    /// scope rule 6). Computed by [`super::split::split`] and persisted so
    /// callers can restore it losslessly. Defaults to `false` for
    /// documents written before this field existed.
    #[serde(default)]
    pub has_bom: bool,
    /// `"lf"` or `"crlf"` (spec §5.1 scope rule 6), detected by
    /// [`super::split::split`]. Defaults to `"lf"` for backward compat with
    /// documents written before this field existed.
    #[serde(default = "default_line_ending")]
    pub line_ending: String,

    /// Section manifest, in `seq` order (v5: replaces the old `fragments`
    /// physical-file manifest — `sections` are in-memory byte-offset
    /// indexes into `_doc.<slug>.md`, not separate files). Old on-disk
    /// documents that still have a `fragments` key deserialize via the
    /// `alias` below for backward compat.
    #[serde(default, alias = "fragments")]
    pub sections: Vec<SectionIndex>,

    pub created_at: String,
    pub updated_at: String,

    /// FNV-1a hash of the full document body. Used to detect drift after
    /// direct `.md` edits (spec §8.2).
    #[serde(default)]
    pub content_hash: String,
}

fn default_auto_inject() -> String {
    "auto".to_string()
}

fn default_line_ending() -> String {
    "lf".to_string()
}

/// Maximum allowed length of a `slug` (spec §3.1 v5 proposal).
pub const MAX_SLUG_LEN: usize = 60;

impl DocMetadata {
    /// Build a fresh document with empty family-tree/section fields and
    /// `auto_inject: "auto"`. `now` is an RFC3339 timestamp supplied by the
    /// caller (keeps this module clock-free and testable, mirroring
    /// `MemoryEntry::new`).
    pub fn new(id: String, slug: String, title: String, doc_type: String, now: String) -> Self {
        DocMetadata {
            version: DOC_SCHEMA_VERSION,
            id,
            slug,
            title,
            doc_type,
            tags: Vec::new(),
            scope_paths: Vec::new(),
            parent_id: None,
            children: Vec::new(),
            related: Vec::new(),
            auto_inject: default_auto_inject(),
            task_ids: Vec::new(),
            source: DocSource::default(),
            has_bom: false,
            line_ending: default_line_ending(),
            sections: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
            content_hash: String::new(),
        }
    }
}

/// A sibling/relative relationship to another document (spec §4.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocRelation {
    pub id: String,
    /// One of [`VALID_RELATIONS`].
    pub rel: String,
}

/// Source tracking for a document, used to support the reversibility
/// guarantee (spec §4.1 / §8).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocSource {
    /// `"authored"` | `"imported"` | `"split"`. Empty string when unset
    /// (fresh documents created directly via `doc_save` default to
    /// `"authored"` at the handler level).
    #[serde(default)]
    pub origin: String,
    /// Original file path when imported from `wiki/` or `tmp/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_path: Option<String>,
    /// Canonical-form hash used to detect drift on reassembly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_hash: Option<String>,
    /// Raw YAML frontmatter block (spec §5.1 scope rule 6), extracted by
    /// [`super::split::split`] and excluded from section seq-0. `None` when
    /// the document has no frontmatter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontmatter: Option<String>,
    /// `true` if a line ending immediately followed the closing `---` fence
    /// in the originally authored body (see
    /// [`super::split::SplitDocument::frontmatter_trailing_eol`]).
    /// Meaningless when `frontmatter` is `None`. Defaults to `true` so
    /// documents persisted before this field existed keep the pre-fix
    /// reassembly behavior (always re-adding the eol) rather than silently
    /// dropping a byte they never reported not having.
    #[serde(default = "default_frontmatter_trailing_eol")]
    pub frontmatter_trailing_eol: bool,
}

fn default_frontmatter_trailing_eol() -> bool {
    true
}

impl Default for DocSource {
    /// Matches the per-field `#[serde(default = ...)]` values above, so a
    /// document missing the whole `source` key (oldest on-disk schema) and
    /// one missing only `frontmatter_trailing_eol` (this field's own
    /// addition) deserialize identically — both keep the pre-fix reassembly
    /// behavior of always re-adding the eol after the frontmatter fence.
    fn default() -> Self {
        DocSource {
            origin: String::new(),
            original_path: None,
            canonical_hash: None,
            frontmatter: None,
            frontmatter_trailing_eol: default_frontmatter_trailing_eol(),
        }
    }
}

/// One entry in a document's section manifest (`DocMetadata::sections`),
/// v5 (spec §3.1): an in-memory byte-offset index into `_doc.<slug>.md`,
/// replacing the v4 `FragmentSummary` (which paired with physical
/// `_frag.*` files) and the old `FragmentMetadata` sidecar entirely.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SectionIndex {
    /// 0-based position in the document. seq 0 is always the preamble.
    pub seq: usize,
    /// Heading text (without `#` markers), empty string for the seq-0
    /// preamble when the document has no heading before it.
    pub heading: String,
    /// ATX heading level (1-6), or 0 for the seq-0 preamble.
    pub level: u8,
    /// Byte offset of this section within the document body (the file at
    /// `_doc.<slug>.md`, after BOM/frontmatter stripping).
    pub byte_offset: usize,
    /// Byte length of this section's body.
    pub byte_length: usize,
    /// FNV-1a hash of this section's body slice.
    pub content_hash: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_doc() -> DocMetadata {
        DocMetadata::new(
            "doc-1".to_string(),
            "my-slug".to_string(),
            "Title".to_string(),
            "spec".to_string(),
            "2026-07-11T00:00:00Z".to_string(),
        )
    }

    #[test]
    fn doc_metadata_new_defaults() {
        let doc = new_doc();
        assert_eq!(doc.version, DOC_SCHEMA_VERSION);
        assert_eq!(doc.slug, "my-slug");
        assert_eq!(doc.auto_inject, "auto");
        assert!(doc.parent_id.is_none());
        assert!(doc.children.is_empty());
        assert!(doc.sections.is_empty());
    }

    #[test]
    fn section_index_holds_byte_offset_length_and_hash() {
        let body = "## Heading\n\nBody\n";
        let section = SectionIndex {
            seq: 1,
            heading: "Heading".to_string(),
            level: 2,
            byte_offset: 5,
            byte_length: body.len(),
            content_hash: lexsim::content_hash(body),
        };
        assert_eq!(section.byte_length, body.len());
        assert_eq!(section.content_hash, lexsim::content_hash(body));
        assert_eq!(section.byte_offset, 5);
    }

    #[test]
    fn serde_roundtrip_doc_metadata() {
        let mut doc = new_doc();
        doc.related.push(DocRelation {
            id: "doc-2".to_string(),
            rel: "references".to_string(),
        });
        doc.source = DocSource {
            origin: "authored".to_string(),
            original_path: None,
            canonical_hash: Some("abc123".to_string()),
            frontmatter: None,
            frontmatter_trailing_eol: true,
        };

        let json = serde_json::to_string(&doc).unwrap();
        let back: DocMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(back.related.len(), 1);
        assert_eq!(back.related[0].rel, "references");
        assert_eq!(back.source.canonical_hash.as_deref(), Some("abc123"));
        assert_eq!(back.slug, "my-slug");
    }

    /// wiki/130-document-management.md §5.1: `split()` computes `has_bom`,
    /// `line_ending`, and `frontmatter` for every authored document — these
    /// must round-trip through storage so a BOM/CRLF/frontmatter document is
    /// never silently corrupted by `doc_save` (t96).
    #[test]
    fn doc_metadata_persists_bom_line_ending_and_frontmatter() {
        let mut doc = new_doc();
        doc.has_bom = true;
        doc.line_ending = "crlf".to_string();
        doc.source.frontmatter = Some("title: Foo\n".to_string());

        let json = serde_json::to_string(&doc).unwrap();
        let back: DocMetadata = serde_json::from_str(&json).unwrap();
        assert!(back.has_bom);
        assert_eq!(back.line_ending, "crlf");
        assert_eq!(back.source.frontmatter.as_deref(), Some("title: Foo\n"));
    }

    #[test]
    fn doc_metadata_new_defaults_bom_and_line_ending() {
        let doc = new_doc();
        assert!(!doc.has_bom);
        assert_eq!(doc.line_ending, "lf");
        assert!(doc.source.frontmatter.is_none());
    }

    /// Old on-disk documents written before this field existed must still
    /// deserialize (backward compat via `#[serde(default)]`).
    #[test]
    fn doc_metadata_deserializes_without_bom_line_ending_fields() {
        let old_json = serde_json::json!({
            "version": 1,
            "id": "doc-1",
            "slug": "doc-1",
            "title": "Title",
            "doc_type": "spec",
            "created_at": "2026-07-11T00:00:00Z",
            "updated_at": "2026-07-11T00:00:00Z",
        });
        let back: DocMetadata = serde_json::from_value(old_json).unwrap();
        assert!(!back.has_bom);
        assert_eq!(back.line_ending, "lf");
        assert!(back.source.frontmatter.is_none());
        assert!(
            back.source.frontmatter_trailing_eol,
            "pre-fix on-disk documents always had the eol re-added on reassembly; \
             the default must preserve that behavior rather than silently drop a byte"
        );
    }

    /// `#[serde(alias = "fragments")]` on `DocMetadata::sections` lets a
    /// *new-shaped* payload (full `SectionIndex` fields: byte_offset,
    /// byte_length, content_hash) round-trip whether it's keyed `sections`
    /// or (legacy key name) `fragments`.
    #[test]
    fn doc_metadata_sections_field_accepts_fragments_alias_key() {
        let via_alias_key = serde_json::json!({
            "version": 2,
            "id": "doc-1",
            "slug": "doc-1",
            "title": "Title",
            "doc_type": "spec",
            "created_at": "2026-07-11T00:00:00Z",
            "updated_at": "2026-07-11T00:00:00Z",
            "fragments": [
                { "seq": 0, "heading": "", "level": 0, "byte_offset": 0, "byte_length": 10, "content_hash": "abc" }
            ],
        });
        let back: DocMetadata = serde_json::from_value(via_alias_key).unwrap();
        assert_eq!(back.sections.len(), 1);
        assert_eq!(back.sections[0].byte_length, 10);
    }

    /// Caution (found in review): a **real** v4 on-disk document has no
    /// `byte_offset`/`byte_length`/`content_hash` in its `fragments` entries
    /// (v4's `FragmentSummary` shape was just `{seq, heading, level}`) *and*
    /// has no `slug` field at all (`slug` is new in v5, required, with no
    /// `#[serde(default)]`). Both gaps make a genuine v4 file fail to
    /// deserialize as `DocMetadata` — the `alias = "fragments"` above only
    /// helps if the *rest* of the v5 shape (crucially `slug` and full
    /// `SectionIndex` fields) is already present. This is **not** a
    /// migration path: `storage::docs::read_doc`/`read_all_docs` treat a
    /// failed parse as "skip silently" (same policy as any corrupt file),
    /// so a real v4 document would vanish from `doc_get`/`doc_list`/
    /// `doc_query` with no warning. Deliberately out of scope per
    /// wiki/130-document-management.md's migration section (no real v4
    /// documents exist outside dev test data) — documented here so the gap
    /// isn't mistaken for a safety net if that assumption ever changes.
    #[test]
    fn doc_metadata_rejects_real_v4_shape_missing_slug_and_byte_fields() {
        let real_v4_shape = serde_json::json!({
            "version": 1,
            "id": "doc-1",
            "title": "Title",
            "doc_type": "spec",
            "created_at": "2026-07-11T00:00:00Z",
            "updated_at": "2026-07-11T00:00:00Z",
            "fragments": [
                { "seq": 0, "heading": "", "level": 0 }
            ],
        });
        let result: Result<DocMetadata, _> = serde_json::from_value(real_v4_shape);
        assert!(
            result.is_err(),
            "a real v4 document (no slug, no byte_offset/byte_length/content_hash) \
             must fail to deserialize under the v5 schema, not silently succeed \
             with data loss (empty sections) — verifying this fails loudly here so \
             read_doc's lenient Ok(None) fallback is a deliberate, documented \
             trade-off rather than an invisible one"
        );
    }

    #[test]
    fn valid_constants_contain_spec_values() {
        assert!(VALID_DOC_TYPES.contains(&"spec"));
        assert!(VALID_DOC_TYPES.contains(&"note"));
        assert!(VALID_AUTO_INJECT.contains(&"outline"));
        assert!(VALID_RELATIONS.contains(&"supersedes"));
    }
}
