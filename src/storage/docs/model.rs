//! Data model for documents and fragments (wiki/130-document-management.md
//! §4). One `DocMetadata` per `_doc.<id>.json`, one `FragmentMetadata` per
//! `_frag.<doc-id>.<seq>.json` (its paired `_frag.<doc-id>.<seq>.md` holds the
//! body — see `super` module docs).

use serde::{Deserialize, Serialize};

/// Current document schema version. Bump when `DocMetadata` changes shape in
/// a way that needs migration handling on read.
pub const DOC_SCHEMA_VERSION: u32 = 1;

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

/// A document: the family-tree node and fragment manifest persisted at
/// `_doc.<id>.json` (spec §4.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocMetadata {
    /// Schema version (= [`DOC_SCHEMA_VERSION`]).
    pub version: u32,
    /// Stable id: `doc-YYYYMMDD-HHMMSS-NNNNNN`.
    pub id: String,
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
    /// `doc_reassemble` can restore it losslessly. Defaults to `false` for
    /// documents written before this field existed.
    #[serde(default)]
    pub has_bom: bool,
    /// `"lf"` or `"crlf"` (spec §5.1 scope rule 6), detected by
    /// [`super::split::split`]. Defaults to `"lf"` for backward compat with
    /// documents written before this field existed.
    #[serde(default = "default_line_ending")]
    pub line_ending: String,

    /// Fragment manifest, in `seq` order.
    #[serde(default)]
    pub fragments: Vec<FragmentSummary>,

    pub created_at: String,
    pub updated_at: String,

    /// FNV-1a hash of all fragment bodies concatenated in `seq` order (the
    /// reassembled document). Used to detect drift after direct fragment
    /// edits (spec §8.2).
    #[serde(default)]
    pub content_hash: String,
}

fn default_auto_inject() -> String {
    "auto".to_string()
}

fn default_line_ending() -> String {
    "lf".to_string()
}

impl DocMetadata {
    /// Build a fresh document with empty family-tree/fragment fields and
    /// `auto_inject: "auto"`. `now` is an RFC3339 timestamp supplied by the
    /// caller (keeps this module clock-free and testable, mirroring
    /// `MemoryEntry::new`).
    pub fn new(id: String, title: String, doc_type: String, now: String) -> Self {
        DocMetadata {
            version: DOC_SCHEMA_VERSION,
            id,
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
            fragments: Vec::new(),
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// [`super::split::split`] and excluded from fragment seq-0. `None` when
    /// the document has no frontmatter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontmatter: Option<String>,
}

/// One entry in a document's fragment manifest (`DocMetadata::fragments`).
/// The lightweight index counterpart of [`FragmentMetadata`] — enough to
/// render a table of contents without reading every fragment file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FragmentSummary {
    pub seq: usize,
    pub heading: String,
    pub level: u8,
}

/// A fragment's metadata sidecar, persisted at
/// `_frag.<doc-id>.<seq>.json` (spec §4.2). The paired body lives in
/// `_frag.<doc-id>.<seq>.md`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FragmentMetadata {
    /// Schema version (= [`DOC_SCHEMA_VERSION`]).
    pub version: u32,
    pub doc_id: String,
    /// 0-based position in the document. seq 0 is always the preamble.
    pub seq: usize,
    /// Heading text (without `#` markers), or `None` for the seq-0 preamble
    /// when the document has no heading before it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,
    /// ATX heading level (1-6), or 0 for the seq-0 preamble.
    pub level: u8,
    /// FNV-1a hash of this fragment's body.
    pub content_hash: String,
    /// Byte offset of this fragment within the reassembled document.
    /// Diagnostic only — never the reassembly source (spec §5.1 rule 2 /
    /// §8: fragments can be edited/reordered independently after a split).
    #[serde(default)]
    pub byte_offset: usize,
    /// Byte length of this fragment's body.
    #[serde(default)]
    pub byte_length: usize,
}

impl FragmentMetadata {
    /// Build fragment metadata for `body`, computing `content_hash` and
    /// `byte_length` from it. `byte_offset` defaults to 0 — callers that
    /// track cumulative offsets while splitting a document should set it
    /// after construction.
    pub fn new(doc_id: String, seq: usize, heading: Option<String>, level: u8, body: &str) -> Self {
        FragmentMetadata {
            version: DOC_SCHEMA_VERSION,
            doc_id,
            seq,
            heading,
            level,
            content_hash: lexsim::content_hash(body),
            byte_offset: 0,
            byte_length: body.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_metadata_new_defaults() {
        let doc = DocMetadata::new(
            "doc-1".to_string(),
            "Title".to_string(),
            "spec".to_string(),
            "2026-07-11T00:00:00Z".to_string(),
        );
        assert_eq!(doc.version, DOC_SCHEMA_VERSION);
        assert_eq!(doc.auto_inject, "auto");
        assert!(doc.parent_id.is_none());
        assert!(doc.children.is_empty());
        assert!(doc.fragments.is_empty());
    }

    #[test]
    fn fragment_metadata_new_computes_hash_and_length() {
        let meta = FragmentMetadata::new(
            "doc-1".to_string(),
            1,
            Some("Heading".to_string()),
            1,
            "## Heading\n\nBody\n",
        );
        assert_eq!(meta.byte_length, "## Heading\n\nBody\n".len());
        assert_eq!(
            meta.content_hash,
            lexsim::content_hash("## Heading\n\nBody\n")
        );
        assert_eq!(meta.byte_offset, 0);
    }

    #[test]
    fn serde_roundtrip_doc_metadata() {
        let mut doc = DocMetadata::new(
            "doc-1".to_string(),
            "Title".to_string(),
            "spec".to_string(),
            "2026-07-11T00:00:00Z".to_string(),
        );
        doc.related.push(DocRelation {
            id: "doc-2".to_string(),
            rel: "references".to_string(),
        });
        doc.source = DocSource {
            origin: "authored".to_string(),
            original_path: None,
            canonical_hash: Some("abc123".to_string()),
            frontmatter: None,
        };

        let json = serde_json::to_string(&doc).unwrap();
        let back: DocMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(back.related.len(), 1);
        assert_eq!(back.related[0].rel, "references");
        assert_eq!(back.source.canonical_hash.as_deref(), Some("abc123"));
    }

    /// wiki/130-document-management.md §5.1: `split()` computes `has_bom`,
    /// `line_ending`, and `frontmatter` for every authored document — these
    /// must round-trip through storage so a BOM/CRLF/frontmatter document is
    /// never silently corrupted by `doc_save`/`doc_reassemble` (t96).
    #[test]
    fn doc_metadata_persists_bom_line_ending_and_frontmatter() {
        let mut doc = DocMetadata::new(
            "doc-1".to_string(),
            "Title".to_string(),
            "spec".to_string(),
            "2026-07-11T00:00:00Z".to_string(),
        );
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
        let doc = DocMetadata::new(
            "doc-1".to_string(),
            "Title".to_string(),
            "spec".to_string(),
            "2026-07-11T00:00:00Z".to_string(),
        );
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
            "title": "Title",
            "doc_type": "spec",
            "created_at": "2026-07-11T00:00:00Z",
            "updated_at": "2026-07-11T00:00:00Z",
        });
        let back: DocMetadata = serde_json::from_value(old_json).unwrap();
        assert!(!back.has_bom);
        assert_eq!(back.line_ending, "lf");
        assert!(back.source.frontmatter.is_none());
    }

    #[test]
    fn valid_constants_contain_spec_values() {
        assert!(VALID_DOC_TYPES.contains(&"spec"));
        assert!(VALID_DOC_TYPES.contains(&"note"));
        assert!(VALID_AUTO_INJECT.contains(&"outline"));
        assert!(VALID_RELATIONS.contains(&"supersedes"));
    }
}
