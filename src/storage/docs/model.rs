//! Data model for documents (wiki/130-document-management.md v5 rearchitecture).
//! One `DocMetadata` per `_doc.<slug>.json`, paired with its full body at
//! `_doc.<slug>.md` (see `super` module docs). Sections are computed
//! in-memory (byte offsets into the body) rather than split into physical
//! fragment files.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Current document schema version. Bump when `DocMetadata` changes shape in
/// a way that needs migration handling on read.
pub const DOC_SCHEMA_VERSION: u32 = 2;

/// Marks that a document's `content_hash`/`source.canonical_hash` were
/// computed via the section-hash composition scheme (t370.15, PR-4,
/// wiki/240-performance-design.md §6) rather than the pre-t370.15 direct
/// `lexsim::content_hash(whole_body)` pass. See
/// [`DocSource::content_hash_scheme`]'s doc comment for the migration
/// handling this enables.
pub const CONTENT_HASH_SCHEME_SECTION_COMPOSED: u32 = 1;

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

    /// V-model layer id (wiki/220-vmodel-integration-design.md §2.1, M1
    /// t360.4): one of the 6 built-in layers (`requirement`, `basic_spec`,
    /// `detailed_spec`, `acceptance`, `system_test`, `unit_test` —
    /// [`super::layer::BUILTIN_LAYERS`]) or a project-defined id. `None` (the
    /// default) means this document has no layer — every pre-M1 document,
    /// and every document an AI has not explicitly assigned a layer to via
    /// `doc_save`'s `layer` argument (the only write path for this field;
    /// per-item `- layer:` overrides in the body are t360.5/t360.6's
    /// concern). `#[serde(skip_serializing_if = "Option::is_none")]` keeps a
    /// `None` document's frontmatter byte-for-byte identical to before this
    /// field existed (NFR-001/002/004 — no spurious diff on re-save).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,

    /// Per-document profile override (wiki/260-vmodel-m2-design.md §2.1,
    /// M2-01, FR-201): one of the 4 built-in profiles
    /// (`minimal`/`standard`/`full`/`bugfix`) or a `[trace.profiles.<name>]`
    /// key. `None` (the default) means "use the project default profile"
    /// (`[trace] profile`, itself falling back to `[trace] layers` / auto —
    /// §2.1's priority order). The only write path is `doc_save`'s
    /// `trace_profile` argument (an empty string clears it, same convention
    /// as `layer` above). Applying this to the document's item *tree*
    /// (refines/verifies-reachable descendants) is M2-03's scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_profile: Option<String>,

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

    /// ATX heading level (1-6) at which this document is split into
    /// sections (frontmatter migration, t123.1/t123.2). Persisted per-doc so
    /// a manually-edited `.md` file recomputes the same section boundaries
    /// on every read. Defaults to
    /// [`super::split::DEFAULT_SPLIT_LEVEL`] for documents saved before this
    /// field existed.
    #[serde(default = "default_split_level")]
    pub split_level: u8,

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
    ///
    /// `None` means "not computed yet" (P-M1, wiki/240-performance-design.md
    /// §4 — t370.8): [`super::read_doc`]/[`super::read_all_docs`] parse
    /// frontmatter and section byte-offsets without paying the
    /// `lexsim::content_hash` cost, since most callers (task-link/dev_stage
    /// propagation, corpus listing by slug) never look at it. Callers that
    /// do need a trustworthy value (staleness/drift checks, `doc_get`
    /// output, `doc_query`'s injection-suppression tracking) must resolve
    /// the document through [`super::read_doc_hashed`] /
    /// [`super::read_doc_with_body_hashed`] / [`super::read_all_docs_hashed`]
    /// instead — deliberately a distinct `Option<String>`, never an empty
    /// string standing in for "not computed", so a caller that forgets to
    /// request the hash gets a `None` it must handle explicitly rather than
    /// a silently-wrong empty hash. Always `Some` immediately before a write
    /// reaches disk (`write_doc_with_body` fills it in if still `None`) —
    /// the on-disk frontmatter field itself stays a plain, always-present
    /// `String` (see `frontmatter::FrontmatterDoc`).
    #[serde(default)]
    pub content_hash: Option<String>,

    /// Verification matrix (wiki/140-verification-matrix.md §3.1). `None` =
    /// matrix not yet generated. Managed through the `handoff_doc_verify`
    /// tool for a non-layer document (`doc_save` never touches this field
    /// for those, so existing on-disk documents without it deserialize to
    /// `None` via `#[serde(default)]`). **On a layer document** (`layer` is
    /// `Some`), `doc_save` DOES rebuild this field on every call — the body
    /// is the source of truth for those items
    /// (wiki/220-vmodel-integration-design.md §2.4/§5, M1 t360.6:
    /// `storage::docs::layer_sync::sync_layer_items`, wired into
    /// `doc_save`/`doc_update_section`/`doc_verify(sync)`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<Verification>,

    /// Unknown/unrecognized frontmatter keys, preserved for round-trip
    /// fidelity (frontmatter migration spec: "extra fields"). Never written
    /// by handoff-mcp itself; only ever populated by parsing a document
    /// whose frontmatter has keys outside the known schema (e.g. hand-edited
    /// or authored by another tool). Not present in the JSON-era on-disk
    /// format, so `#[serde(default)]` keeps old fixtures deserializing.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

fn default_auto_inject() -> String {
    "auto".to_string()
}

fn default_line_ending() -> String {
    "lf".to_string()
}

fn default_split_level() -> u8 {
    super::split::DEFAULT_SPLIT_LEVEL
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
            layer: None,
            trace_profile: None,
            source: DocSource::default(),
            has_bom: false,
            line_ending: default_line_ending(),
            split_level: default_split_level(),
            sections: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
            content_hash: None,
            verification: None,
            extra: HashMap::new(),
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
    /// Canonical-form hash used to detect drift on reassembly. In the
    /// frontmatter format (t123.1+), this is the value persisted at the
    /// *last save* (`source.canonical_hash` in frontmatter, untouched by
    /// `read_doc`'s on-read `content_hash` recompute — t123.2), so comparing
    /// it against the freshly-recomputed top-level `content_hash` is the
    /// drift signal `doc_reassemble` uses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_hash: Option<String>,
    /// FNV-1a (64-bit) hex hash of the document's raw body bytes, as of the
    /// last successful layer sync (wiki/220-vmodel-integration-design.md
    /// §2.4, M1 t360.6, wiki/240-performance-design.md §5-3). This is
    /// **not** `canonical_hash`/`content_hash` (both go through lexsim's
    /// `content_hash` normalization/tokenization) — `body_raw_hash` is a
    /// cheap hash of the exact bytes, used only to detect "did the body
    /// change since the last layer sync" without paying `content_hash`'s
    /// cost on every `doc_save`/`doc_update_section` call. `None` for
    /// non-layer documents and for layer documents saved before this field
    /// existed — a missing value is treated as "direct edit happened,
    /// sync once" by the caller (never as "definitely unchanged").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_raw_hash: Option<String>,
    /// M2 (wiki/260-vmodel-m2-design.md E7/§2.5 step 1, M2-04):
    /// `"<scheme_version>:<fnv1a_hex(sync-affecting config)>"`, recorded at
    /// the same time as [`Self::body_raw_hash`] on every successful layer
    /// sync. `sync_layer_items_if_needed`'s short-circuit ("body byte-
    /// identical to last sync, skip re-parsing") additionally requires this
    /// to still match the *current* stamp — so a project-level change to
    /// something that actually changes a sync's output (the layer registry,
    /// `[trace.id_prefixes]`, the default profile name, or any profile's
    /// `implicit_acceptance`) forces exactly one re-sync of every layer
    /// document on its next `doc_save`/`doc_update_section`/`doc_verify(sync)`/
    /// read-only-tool pass, even though the body's raw bytes never changed.
    /// Deliberately **excludes** settings that do not change a sync's output
    /// (lint rule severities, `done_guard`, display-name-only overrides,
    /// `[trace] layers`) — changing only those must not force a spurious
    /// re-sync (E7: "lint・`done_guard`・表示名・`[trace] layers` は含めない").
    /// `None` for a layer document never synced by an M2-04-or-later binary
    /// (never-synced or M1/pre-M2-04-synced) — treated the same as a mismatch
    /// (always resync once), mirroring `body_raw_hash`'s own "missing means
    /// changed" rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer_sync_stamp: Option<String>,
    /// Which `content_hash`/`canonical_hash` computation scheme produced the
    /// values currently on this document (t370.15, PR-4,
    /// wiki/240-performance-design.md §6): `Some(CONTENT_HASH_SCHEME_SECTION_COMPOSED)`
    /// once this document has been written by a t370.15-or-later binary,
    /// `None` for a document written only by an older binary (or never
    /// rewritten since). `storage::docs::write_doc_with_body` sets this on
    /// every write, so a legacy document self-migrates on its very next
    /// save/update_section.
    ///
    /// Exists because the composition scheme this constant marks produces a
    /// *different* `content_hash` value than the old direct
    /// `lexsim::content_hash(whole_body)` pass, even for byte-identical
    /// content — comparing a freshly-recomputed (always new-scheme, see
    /// `storage::docs::recompute_sections_and_hash`) `content_hash` against a
    /// `canonical_hash` persisted under the *old* scheme would otherwise
    /// report a false "drifted" result for every untouched legacy document
    /// (`mcp::handlers::docs::handle_doc_reassemble`'s drift check). `None`
    /// here tells that check to fall back to computing the legacy-style hash
    /// for the comparison instead, exactly once per document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash_scheme: Option<u32>,
    /// Legacy field (pre-frontmatter-migration, t96): raw YAML frontmatter
    /// block stashed by the old 2-file format when a caller's authored
    /// `body` started with its own `---`-fenced block, so it could be
    /// restored losslessly on `doc_get`/`doc_reassemble`. **Dead in the
    /// frontmatter format** — kept only so a legacy `_doc.<slug>.json`
    /// sidecar still deserializes during migration
    /// (`storage::docs::migrate_legacy_doc`); a document's own frontmatter
    /// is now handoff-owned metadata, so a caller's leading frontmatter
    /// block in `body` is absorbed rather than round-tripped (see
    /// `handle_doc_get`'s `read_full_body`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontmatter: Option<String>,
    /// Legacy field, paired with [`Self::frontmatter`] — see its doc comment.
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
            body_raw_hash: None,
            layer_sync_stamp: None,
            content_hash_scheme: None,
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
    /// FNV-1a hash of this section's body slice. `None` when the caller that
    /// computed this `SectionIndex` didn't request hashes (P-M1, t370.8 —
    /// see [`DocMetadata::content_hash`]'s doc comment); never persisted to
    /// disk either way (`sections[]` is always recomputed fresh from the
    /// body, per this module's docs).
    #[serde(default)]
    pub content_hash: Option<String>,
}

/// Verification matrix for a document (wiki/140-verification-matrix.md §3.1).
/// Persisted inside `DocMetadata::verification`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verification {
    /// Overall status: "pending" | "in_review" | "verified".
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    /// One item per tracked fragment.
    pub items: Vec<VerificationItem>,
}

/// One row in the verification matrix — tracks review state of a single
/// spec fragment (v1) or, since v2 (wiki/140-verification-matrix.md §7), a
/// freeform top-level item not tied to any section (`fragment_seq: None`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationItem {
    /// The section this item tracks. `None` (v2) = a freeform item, not
    /// tied to any document section — see `label`.
    #[serde(default)]
    pub fragment_seq: Option<usize>,
    pub heading: String,
    /// "pending" | "skipped" | "verified".
    pub status: String,
    #[serde(default)]
    pub impl_refs: Vec<CodeRef>,
    #[serde(default)]
    pub test_refs: Vec<CodeRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<String>,
    #[serde(default)]
    pub notes: String,
    /// Fragment content_hash at the time of verification. If the fragment's
    /// current hash differs, this item's review is stale and should be
    /// flagged (`doc_verify_status`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash_at_verify: Option<String>,

    /// v2: item category — `"section"` (default, existing heading-level
    /// items), `"requirement"`, `"visual"`, `"regression"`, `"manual"`
    /// (free-extensible, not an enforced enum).
    #[serde(default = "default_category")]
    pub category: String,
    /// v2: individual requirements tracked within a `category="section"`
    /// item.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sub_items: Vec<SubItem>,
    /// v2: label for a freeform item (`fragment_seq: None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

fn default_category() -> String {
    "section".to_string()
}

/// v2 (wiki/140-verification-matrix.md §7.1): one individual requirement
/// tracked within a section-level `VerificationItem::sub_items`.
///
/// Requirements-traceability P0 (`.handoff/docs/_doc.req-traceability-mcp-plan.md`
/// §3.1) adds `stable_id`/`priority`/`dev_stage`/`impl_refs`/`test_refs` on
/// top of the existing verification-review fields. All new fields are
/// `Option`/`Vec` with `#[serde(default)]` so pre-existing `_doc.*.json`
/// files without them still deserialize.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubItem {
    /// 0-based position within the parent item's `sub_items`.
    pub index: usize,
    pub description: String,
    /// "pending" | "skipped" | "verified" (verification review status —
    /// distinct from `dev_stage`, which tracks implementation progress).
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<String>,
    #[serde(default)]
    pub notes: String,
    /// "requirement" (default) | "visual" | "manual" | ... (free-extensible).
    #[serde(default = "default_sub_category")]
    pub category: String,

    /// Stable requirement ID (e.g. `"C01-2.1.1.1"`), immutable once
    /// assigned. `None` until derived/assigned (P0 §2.3 — derivation is
    /// t300.3's concern; this field is just storage).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stable_id: Option<String>,
    /// "P0" | "P1" | "P2" | "P3", free-extensible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    /// "not_started" | "in_progress" | "implemented" | "tested" | "verified"
    /// (P0 §2.4). Distinct from `status` (verification review).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dev_stage: Option<String>,
    /// Requirement-level implementation locations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub impl_refs: Vec<CodeRef>,
    /// Requirement-level test locations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test_refs: Vec<CodeRef>,

    /// Task ids related to the implementation of this requirement
    /// (requirements-traceability integration reform §3.1). Bidirectional —
    /// the task side mirrors this via `TaskLink { link_type: "requirement" }`,
    /// synced by `handoff_doc_verify(action="link_task")`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_ids: Vec<String>,
    /// Reserved for future use: stable_ids of other requirements this one
    /// depends on (requirements-traceability integration reform §3.1).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,

    /// `"body"` when this `SubItem` was created/is maintained by layer body
    /// parsing (wiki/220-vmodel-integration-design.md §2.3, M1 t360.4 —
    /// parsing itself is t360.5's concern; this field is just storage).
    /// `None` = a pre-M1 `SubItem` (freeform or `req_import`-derived), whose
    /// definition fields remain tool-writable as before. Written by the
    /// tool, never by body content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Per-item layer override (`- layer: <id>` attribute line, §2.2/§2.3).
    /// The item's *effective* layer is `layer.or(doc.layer)` (§2.3) — that
    /// resolution, and the body-owned write guard on this field, are
    /// t360.5/t360.6's concern; M1 t360.4 only adds the storage slot.
    /// Written by the body (origin=body items) or left `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    /// stable_ids of upper (lower `level`) left-side items this one refines
    /// (§2.3/§2.7 `refines`). Body-owned once origin=body.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refines: Vec<String>,
    /// stable_ids of left-side items this (right-side or inline) item
    /// verifies (§2.3/§2.7 `verifies`). Body-owned once origin=body.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verifies: Vec<String>,
    /// Verification method: `"manual"` | `"auto"` | `"visual"` | `"review"`
    /// (§2.2's attribute-line vocabulary). Body-owned once origin=body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// FNV-1a hash of title + statement + attributes, normalized per §2.3
    /// ("連続空白→1つ、前後空白除去、改行統一"). Written by the tool (layer
    /// sync, t360.6) and read by `trace_record`/M2 suspect to tell whether a
    /// recorded result still matches the item's current definition. `None`
    /// until first computed. **Frozen at the M1 key set** (E14,
    /// wiki/260-vmodel-m2-design.md §2.2): M2's body parser recognizes more
    /// attribute keys (`rationale`/`derived`/`waive-*`/`from`/reserved), but
    /// `body_hash`'s own statement continues to strip only the M1 keys
    /// (`refines`/`verifies`/`layer`/`priority`/`method`/`test`), so a
    /// document written under M1 with e.g. a literal `- rationale: …` line
    /// (then just ordinary body text) hashes identically under M2 — no
    /// existing run result becomes spuriously suspect on upgrade.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_hash: Option<String>,

    /// M2 (wiki/260-vmodel-m2-design.md §2.3/§2.4, M2-02): FNV-1a hash of
    /// `{title, statement-minus-acceptance-block, acceptance list}`,
    /// NFKC-normalized (`unicode-normalization`, not lexsim's `normalize` —
    /// §2.4). Distinct from `body_hash`: `def_hash` reacts to acceptance
    /// criteria and ignores the M1 attribute set entirely (no
    /// `refines`/`verifies`/`layer`/`priority`/`method`/`test` in the
    /// input); it is `trace_suspect`'s (M2-05) input, not `trace_record`'s.
    /// Written by the tool (layer sync); `None` until first computed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub def_hash: Option<String>,
    /// M2 §2.2/§2.3: the item's parsed acceptance-criteria block (an
    /// "受入基準:" paragraph followed by a bullet list), one entry per
    /// accepted bullet — deliberately holds only `{label, kind}`, not the AC
    /// text itself (D1, mirrors `statement`: re-derived from the body on
    /// demand rather than duplicated in storage).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptance: Vec<AcRef>,
    /// M2 §2.2 `- rationale: <free text>` attribute line — a one-line
    /// justification for this item, body-owned once `origin=body`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    /// M2 §2.2 `- derived: <reason>` attribute line: this item has no
    /// upstream link on purpose (a right-side item with no `verifies`, or a
    /// left-side item with no `refines`) and `<reason>` explains why — used
    /// to suppress the `orphan` gap for this item (§3.1). Body-owned once
    /// `origin=body`. A line with an empty reason is dropped (with a parse
    /// warning) rather than stored as `Some("")`, since an unexplained
    /// `derived` defeats the "reason付き" requirement (§2.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived: Option<String>,
    /// M2 §2.2 `- waive-verify: <reason>` / `- waive-refine: <reason>`
    /// attribute lines: an explained exemption from horizontal
    /// (`verify`)/vertical (`refine`) coverage for this item. Body-owned
    /// once `origin=body`. Same empty-reason-is-dropped rule as `derived`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waivers: Vec<Waiver>,
    /// M2 §2.2 `- from: <id>` attribute line: the id of the item this one
    /// was scaffolded from (`handoff_trace_scaffold`, FR-305, M2-12).
    /// Body-owned once `origin=body`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// M2 §2.5 step 3: the parent item's stable_id, for an *implicit*
    /// acceptance-verification `SubItem` this layer sync materialized from
    /// one of the parent's acceptance-criteria entries (`implicit_acceptance`
    /// profile setting). `None` for every ordinary body item. Written by the
    /// tool, never by body content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub implicit_of: Option<String>,
    /// M2 §2.2 reserved attribute key (`needs` FR-202): stored verbatim (key
    /// -> raw value) with no interpretation in M2 — a future milestone gives
    /// it meaning. Body-owned once `origin=body`. `BTreeMap` for a
    /// deterministic key order (NFR-004).
    ///
    /// M3 (wiki/270-vmodel-m3-design.md §2.1/§2.2, FR-202/FR-307): `assignee`
    /// and `needs` have both been promoted out of this map into their own
    /// fields ([`Self::assignee`]/[`Self::needs`]) — this map no longer ever
    /// holds an `"assignee"` or `"needs"` key for a document parsed under the
    /// M3 binary (a pre-M3 on-disk value, if any, is simply left here
    /// untouched until the next sync overwrites it).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reserved_attrs: BTreeMap<String, String>,
    /// M3 (wiki/270-vmodel-m3-design.md §2.2, FR-307): `- assignee: <key>`
    /// attribute line, promoted out of [`Self::reserved_attrs`] into its own
    /// field. `<key>` is expected to match a `[assignees.<key>]` roster entry
    /// in `config.toml` (the same namespace the task side uses, E24) — an
    /// unregistered key is still stored as authored (never rejected at parse
    /// time) but produces a parse warning (§2.2). Body-owned once
    /// `origin=body`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// M3 (wiki/270-vmodel-m3-design.md §2.1, FR-202): `- needs: <id>[,
    /// <id>...]` attribute line, promoted out of [`Self::reserved_attrs`]
    /// into its own field with a deliberate **3-state semantics** (§2.1):
    ///
    /// - `None` (the attribute line is absent): the profile's
    ///   `default_needs` applies for this item's effective layer
    ///   (`TraceProfileConfig::default_needs`/its natural derivation,
    ///   `src/trace/profile.rs`).
    /// - `Some(vec![])` (`- needs:` authored with an empty value): no
    ///   coverage is required at all — an explicit exemption, distinct from
    ///   "unset".
    /// - `Some(non_empty)`: exactly these layer ids are required, overriding
    ///   `default_needs` for this item.
    ///
    /// Each entry is a layer id (unknown ids are dropped with a parse
    /// warning — §2.1, `src/storage/docs/layer_parse.rs`). Body-owned once
    /// `origin=body`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs: Option<Vec<String>>,
    /// M2 §2.3/§2.5 (E2): for every upstream reference this item's
    /// `refines`/`verifies` currently holds (keyed by the literal authored
    /// value — `"REQ-003"` or `"REQ-003#AC2"`), the upstream's hash *at the
    /// time this link was first added* (`def_hash` for a whole-item
    /// reference, `ac_hash` for an `X#ACn` one) — the suspect baseline
    /// (§2.4/§4.1). A reference with no entry here is "unbaselined" (never
    /// silently backfilled with the current hash — only
    /// `trace_suspect(action="baseline")` does that, M2-05). Recording new
    /// entries on sync is **M2-04's** scope, not M2-02's — this layer sync
    /// only *preserves* whatever a prior sync/baseline action already wrote,
    /// the same way it already preserves `task_ids`/`dev_stage`
    /// (`body_owned.remove(id)` restores the whole prior `SubItem`, and this
    /// map is never touched by this module for an ordinary parsed item).
    /// `BTreeMap` for a deterministic key order (NFR-004).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub link_baselines: BTreeMap<String, String>,
    /// M3 (wiki/270-vmodel-m3-design.md §2.3, FR-406): the approval axis's
    /// own field, superseding the M2 E12 read-mapping of `status`/`reviewer`/
    /// `verified_at` onto `approval`. Three values: `"draft"` | `"review"` |
    /// `"approved"`.
    ///
    /// **Priority rule (§2.3)**: when this field is `Some`, it is the sole
    /// authority — `status` is ignored entirely (no bidirectional sync back
    /// to `status`/`reviewer`/`verified_at`, which remain untouched for M2
    /// compatibility only). When this field is `None` (an item never written
    /// by an M3 binary, or a fixture from before M3), the E12 read-mapping
    /// applies instead: `status: "verified"` -> `"approved"`, anything else
    /// -> `"draft"` (see `approval_str` in `src/mcp/handlers/trace.rs`).
    ///
    /// Transitions (§2.3): `draft -> review` and `draft -> approved` (direct)
    /// are both allowed via `trace_update(set.approval=...)`; `review ->
    /// approved` additionally stamps `approved_hash`/`approved_by`/
    /// `approved_at` and writes an audit file
    /// (`.handoff/trace/approvals/<id>.json`, `src/storage/approvals.rs`).
    /// `approved`/`review -> draft` also happens *automatically* on layer
    /// sync when `def_hash` changes (§3.2) — `approved_hash` is deliberately
    /// **not** cleared on that automatic reset, so it remains readable as
    /// "the def_hash as of the last approval".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<String>,
    /// M3 (wiki/270-vmodel-m3-design.md §2.3, FR-406): the `def_hash`
    /// snapshot taken at the moment this item was last moved to
    /// `approval: "approved"`. Never cleared by the automatic
    /// approved/review -> draft reset (§2.3/§3.2) — it is a historical
    /// "hash as of last approval" marker, not a liveness flag paired with
    /// `approval`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_hash: Option<String>,
    /// M3 (wiki/270-vmodel-m3-design.md §2.3, FR-406): the approver
    /// (`executor_id`) recorded at the same moment as `approved_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<String>,
    /// M3 (wiki/270-vmodel-m3-design.md §2.3, FR-406): the ISO 8601
    /// timestamp recorded at the same moment as `approved_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_at: Option<String>,
}

/// One parsed acceptance-criteria bullet (wiki/260-vmodel-m2-design.md
/// §2.2/§2.3), stored on [`SubItem::acceptance`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcRef {
    /// `"AC1"`, `"AC2"`, ... — either the authored `AC<n>:` label, or a
    /// position-assigned one (§2.2: reordering the bullets then changes
    /// which label a position-assigned AC gets — a parse warning covers
    /// this).
    pub label: String,
    /// `"gwt"` (Given/When/Then) | `"ears"` (WHEN/WHILE/WHERE/IF … SHALL) |
    /// `"text"` (neither pattern) — §2.2's classification, used only by
    /// scaffold generation (M2-12) to split into steps/expected-result.
    pub kind: String,
}

/// One parsed `- waive-verify:`/`- waive-refine:` attribute line
/// (wiki/260-vmodel-m2-design.md §2.2/§2.3), stored on [`SubItem::waivers`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Waiver {
    /// `"verify"` (from `waive-verify`) | `"refine"` (from `waive-refine`).
    pub axis: String,
    /// The (non-empty) reason authored after the `:` — required by §2.2; a
    /// waiver line with an empty reason is dropped at parse time rather than
    /// stored with an empty reason here.
    pub reason: String,
}

fn default_sub_category() -> String {
    "requirement".to_string()
}

impl Default for SubItem {
    fn default() -> Self {
        Self {
            index: 0,
            description: String::new(),
            status: "pending".to_string(),
            reviewer: None,
            verified_at: None,
            notes: String::new(),
            category: default_sub_category(),
            stable_id: None,
            priority: None,
            dev_stage: Some("not_started".to_string()),
            impl_refs: Vec::new(),
            test_refs: Vec::new(),
            task_ids: Vec::new(),
            depends_on: Vec::new(),
            origin: None,
            layer: None,
            refines: Vec::new(),
            verifies: Vec::new(),
            method: None,
            body_hash: None,
            def_hash: None,
            acceptance: Vec::new(),
            rationale: None,
            derived: None,
            waivers: Vec::new(),
            from: None,
            implicit_of: None,
            reserved_attrs: BTreeMap::new(),
            assignee: None,
            needs: None,
            link_baselines: BTreeMap::new(),
            approval: None,
            approved_hash: None,
            approved_by: None,
            approved_at: None,
        }
    }
}

/// A reference to a source code location.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeRef {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
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
            content_hash: Some(lexsim::content_hash(body)),
        };
        assert_eq!(section.byte_length, body.len());
        assert_eq!(
            section.content_hash.as_deref(),
            Some(lexsim::content_hash(body).as_str())
        );
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
            body_raw_hash: None,
            layer_sync_stamp: None,
            content_hash_scheme: None,
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

    /// wiki/140-verification-matrix.md §3.4: existing on-disk documents
    /// without a `verification` key must deserialize with `verification:
    /// None` — `doc_save` and the pre-verification-matrix on-disk schema
    /// must be unaffected by this addition.
    #[test]
    fn doc_metadata_new_defaults_verification_to_none() {
        let doc = new_doc();
        assert!(doc.verification.is_none());
    }

    #[test]
    fn doc_metadata_deserializes_without_verification_field() {
        let old_json = serde_json::json!({
            "version": 2,
            "id": "doc-1",
            "slug": "doc-1",
            "title": "Title",
            "doc_type": "spec",
            "created_at": "2026-07-11T00:00:00Z",
            "updated_at": "2026-07-11T00:00:00Z",
        });
        let back: DocMetadata = serde_json::from_value(old_json).unwrap();
        assert!(back.verification.is_none());
    }

    #[test]
    fn verification_round_trips_through_doc_metadata() {
        let mut doc = new_doc();
        doc.verification = Some(Verification {
            status: "in_review".to_string(),
            created_at: "2026-07-11T10:00:00Z".to_string(),
            updated_at: "2026-07-11T14:30:00Z".to_string(),
            items: vec![VerificationItem {
                fragment_seq: Some(2),
                heading: "1. 課題".to_string(),
                status: "verified".to_string(),
                impl_refs: vec![CodeRef {
                    path: "src/storage/docs/mod.rs".to_string(),
                    lines: Some("42-180".to_string()),
                    label: Some("DocStore".to_string()),
                }],
                test_refs: vec![CodeRef {
                    path: "tests/doc_save.rs".to_string(),
                    lines: None,
                    label: Some("doc_save roundtrip".to_string()),
                }],
                reviewer: Some("ai".to_string()),
                verified_at: Some("2026-07-11T14:30:00Z".to_string()),
                notes: String::new(),
                content_hash_at_verify: Some("abc123".to_string()),
                category: "section".to_string(),
                sub_items: Vec::new(),
                label: None,
            }],
        });

        let json = serde_json::to_string(&doc).unwrap();
        let back: DocMetadata = serde_json::from_str(&json).unwrap();
        let v = back.verification.expect("verification must round-trip");
        assert_eq!(v.status, "in_review");
        assert_eq!(v.items.len(), 1);
        assert_eq!(v.items[0].fragment_seq, Some(2));
        assert_eq!(v.items[0].impl_refs[0].path, "src/storage/docs/mod.rs");
        assert_eq!(
            v.items[0].test_refs[0].label.as_deref(),
            Some("doc_save roundtrip")
        );
    }

    /// wiki/140-verification-matrix.md §7.1 (v2 extension): a v1
    /// `VerificationItem` (plain-number `fragment_seq`, no `category` /
    /// `sub_items` / `label`) must still deserialize, defaulting
    /// `category` to `"section"`, `sub_items` to empty, and `label` to
    /// `None` — v1 behavior is fully preserved.
    #[test]
    fn verification_item_v1_json_deserializes_with_v2_defaults() {
        let v1_item = serde_json::json!({
            "fragment_seq": 2,
            "heading": "1. 課題",
            "status": "verified",
            "reviewer": "ai",
            "verified_at": "2026-07-11T14:30:00Z",
        });
        let item: VerificationItem = serde_json::from_value(v1_item).unwrap();
        assert_eq!(item.fragment_seq, Some(2));
        assert_eq!(item.category, "section");
        assert!(item.sub_items.is_empty());
        assert!(item.label.is_none());
    }

    #[test]
    fn sub_item_defaults_category_to_requirement() {
        let json = serde_json::json!({
            "index": 0,
            "description": "形状=八面体であること",
            "status": "pending",
        });
        let sub: SubItem = serde_json::from_value(json).unwrap();
        assert_eq!(sub.category, "requirement");
        assert!(sub.notes.is_empty());
        assert!(sub.reviewer.is_none());
    }

    #[test]
    fn verification_item_supports_freeform_fragment_seq_none() {
        let item = VerificationItem {
            fragment_seq: None,
            heading: "ドラッグ操作の目視確認".to_string(),
            status: "pending".to_string(),
            impl_refs: Vec::new(),
            test_refs: Vec::new(),
            reviewer: None,
            verified_at: None,
            notes: String::new(),
            content_hash_at_verify: None,
            category: "visual".to_string(),
            sub_items: Vec::new(),
            label: Some("ドラッグ操作の目視確認".to_string()),
        };
        let json = serde_json::to_string(&item).unwrap();
        let back: VerificationItem = serde_json::from_str(&json).unwrap();
        assert!(back.fragment_seq.is_none());
        assert_eq!(back.label.as_deref(), Some("ドラッグ操作の目視確認"));
        assert_eq!(back.category, "visual");
    }

    #[test]
    fn verification_item_sub_items_round_trip() {
        let mut item = VerificationItem {
            fragment_seq: Some(2),
            heading: "1. 課題".to_string(),
            status: "in_review".to_string(),
            impl_refs: Vec::new(),
            test_refs: Vec::new(),
            reviewer: None,
            verified_at: None,
            notes: String::new(),
            content_hash_at_verify: None,
            category: "section".to_string(),
            sub_items: Vec::new(),
            label: None,
        };
        item.sub_items.push(SubItem {
            index: 0,
            description: "形状=八面体であること".to_string(),
            status: "verified".to_string(),
            reviewer: Some("ai".to_string()),
            verified_at: Some("2026-07-11T14:30:00Z".to_string()),
            ..Default::default()
        });

        let json = serde_json::to_string(&item).unwrap();
        let back: VerificationItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back.sub_items.len(), 1);
        assert_eq!(back.sub_items[0].description, "形状=八面体であること");
        assert_eq!(back.sub_items[0].status, "verified");
    }

    #[test]
    fn sub_item_default_has_expected_initial_values() {
        let s = SubItem::default();
        assert_eq!(s.index, 0);
        assert_eq!(s.description, "");
        assert_eq!(s.status, "pending");
        assert_eq!(s.reviewer, None);
        assert_eq!(s.verified_at, None);
        assert_eq!(s.notes, "");
        assert_eq!(s.category, "requirement");
        assert_eq!(s.stable_id, None);
        assert_eq!(s.priority, None);
        assert_eq!(s.dev_stage.as_deref(), Some("not_started"));
        assert!(s.impl_refs.is_empty());
        assert!(s.test_refs.is_empty());
    }

    #[test]
    fn sub_item_deserializes_from_json_without_new_fields() {
        // Backward compat: `_doc.*.json` files written before P0 (requirements
        // traceability) don't have stable_id/priority/dev_stage/impl_refs/
        // test_refs at all. They must still deserialize successfully.
        let json = r#"{
            "index": 2,
            "description": "既存のサブ項目",
            "status": "verified",
            "notes": "",
            "category": "requirement"
        }"#;
        let sub: SubItem = serde_json::from_str(json).unwrap();
        assert_eq!(sub.index, 2);
        assert_eq!(sub.description, "既存のサブ項目");
        assert_eq!(sub.status, "verified");
        assert_eq!(sub.stable_id, None);
        assert_eq!(sub.priority, None);
        assert_eq!(sub.dev_stage, None);
        assert!(sub.impl_refs.is_empty());
        assert!(sub.test_refs.is_empty());
    }

    /// Requirements-traceability integration reform §3.1: `task_ids` and
    /// `depends_on` must round-trip through serde like every other SubItem
    /// field.
    #[test]
    fn sub_item_task_ids_and_depends_on_round_trip() {
        let sub = SubItem {
            task_ids: vec!["t42".to_string(), "t43".to_string()],
            depends_on: vec!["C01-1.1".to_string()],
            ..Default::default()
        };
        let json = serde_json::to_string(&sub).unwrap();
        let back: SubItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back.task_ids, vec!["t42".to_string(), "t43".to_string()]);
        assert_eq!(back.depends_on, vec!["C01-1.1".to_string()]);
    }

    /// A `SubItem` default has empty `task_ids`/`depends_on`.
    #[test]
    fn sub_item_default_has_empty_task_ids_and_depends_on() {
        let s = SubItem::default();
        assert!(s.task_ids.is_empty());
        assert!(s.depends_on.is_empty());
    }

    /// Backward compat: existing on-disk SubItems written before this field
    /// existed have no `task_ids`/`depends_on` key at all and must still
    /// deserialize successfully.
    #[test]
    fn sub_item_deserializes_without_task_ids_and_depends_on() {
        let json = r#"{
            "index": 0,
            "description": "既存のサブ項目",
            "status": "verified",
            "stable_id": "C01-1.1"
        }"#;
        let sub: SubItem = serde_json::from_str(json).unwrap();
        assert_eq!(sub.stable_id.as_deref(), Some("C01-1.1"));
        assert!(sub.task_ids.is_empty());
        assert!(sub.depends_on.is_empty());
    }

    /// Backward compat (NFR-001/002, wiki/220-vmodel-integration-design.md
    /// §2.3/§5, M1 t360.4): a pre-M1 on-disk `SubItem` has none of
    /// `origin`/`layer`/`refines`/`verifies`/`method`/`body_hash` — every one
    /// must default (`None`/empty `Vec`) rather than fail to parse.
    #[test]
    fn sub_item_deserializes_without_m1_layer_fields() {
        let json = r#"{
            "index": 0,
            "description": "既存のサブ項目",
            "status": "verified",
            "stable_id": "C01-1.1"
        }"#;
        let sub: SubItem = serde_json::from_str(json).unwrap();
        assert_eq!(sub.origin, None);
        assert_eq!(sub.layer, None);
        assert!(sub.refines.is_empty());
        assert!(sub.verifies.is_empty());
        assert_eq!(sub.method, None);
        assert_eq!(sub.body_hash, None);
    }

    /// M1 t360.4 (wiki/220 §2.3): every new field round-trips through
    /// `serde_json` once set (the shape the frontmatter YAML layer reuses).
    #[test]
    fn sub_item_m1_layer_fields_round_trip_through_json() {
        let sub = SubItem {
            index: 0,
            description: "SPEC-012 ログイン失敗時のアカウントロック".to_string(),
            stable_id: Some("SPEC-012".to_string()),
            origin: Some("body".to_string()),
            layer: Some("basic_spec".to_string()),
            refines: vec!["REQ-003".to_string()],
            verifies: vec!["ST-040".to_string()],
            method: Some("manual".to_string()),
            body_hash: Some("a1b2c3d4".to_string()),
            ..Default::default()
        };
        let json = serde_json::to_string(&sub).unwrap();
        let back: SubItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back.origin.as_deref(), Some("body"));
        assert_eq!(back.layer.as_deref(), Some("basic_spec"));
        assert_eq!(back.refines, vec!["REQ-003".to_string()]);
        assert_eq!(back.verifies, vec!["ST-040".to_string()]);
        assert_eq!(back.method.as_deref(), Some("manual"));
        assert_eq!(back.body_hash.as_deref(), Some("a1b2c3d4"));
    }

    /// NFR-004 (no spurious diff): a `SubItem` with every M1 field left
    /// unset must serialize identically to a pre-M1 `SubItem` — none of the
    /// new keys should appear.
    #[test]
    fn sub_item_m1_layer_fields_absent_from_json_when_unset() {
        let sub = SubItem {
            index: 0,
            description: "既存のサブ項目".to_string(),
            stable_id: Some("C01-1.1".to_string()),
            ..Default::default()
        };
        let json = serde_json::to_string(&sub).unwrap();
        for key in [
            "origin",
            "layer",
            "refines",
            "verifies",
            "method",
            "body_hash",
        ] {
            assert!(
                !json.contains(&format!("\"{key}\"")),
                "unset M1 field '{key}' must not appear in serialized SubItem: {json}"
            );
        }
    }

    /// wiki/260-vmodel-m2-design.md §2.3, M2-02: a pre-M2 on-disk `SubItem`
    /// has none of the new M2 body-notation fields — every one must default
    /// (`None`/empty) rather than fail to parse.
    #[test]
    fn sub_item_deserializes_without_m2_body_notation_fields() {
        let json = r#"{
            "index": 0,
            "description": "既存のサブ項目",
            "status": "verified",
            "stable_id": "SPEC-012"
        }"#;
        let sub: SubItem = serde_json::from_str(json).unwrap();
        assert_eq!(sub.def_hash, None);
        assert!(sub.acceptance.is_empty());
        assert_eq!(sub.rationale, None);
        assert_eq!(sub.derived, None);
        assert!(sub.waivers.is_empty());
        assert_eq!(sub.from, None);
        assert_eq!(sub.implicit_of, None);
        assert!(sub.reserved_attrs.is_empty());
        assert!(sub.link_baselines.is_empty());
    }

    /// M2-02: every new field round-trips through `serde_json` once set.
    #[test]
    fn sub_item_m2_body_notation_fields_round_trip_through_json() {
        let mut reserved = BTreeMap::new();
        reserved.insert("assignee".to_string(), "alice".to_string());
        reserved.insert("needs".to_string(), "REQ-001".to_string());
        let mut baselines = BTreeMap::new();
        baselines.insert("REQ-003".to_string(), "a1b2c3d4".to_string());
        baselines.insert("REQ-003#AC1".to_string(), "deadbeef".to_string());

        let sub = SubItem {
            index: 0,
            description: "SPEC-012 ログイン失敗時のアカウントロック".to_string(),
            stable_id: Some("SPEC-012".to_string()),
            origin: Some("body".to_string()),
            def_hash: Some("f00dcafe".to_string()),
            acceptance: vec![
                AcRef {
                    label: "AC1".to_string(),
                    kind: "gwt".to_string(),
                },
                AcRef {
                    label: "AC2".to_string(),
                    kind: "ears".to_string(),
                },
            ],
            rationale: Some("総当たり攻撃の抑止".to_string()),
            derived: Some("実装方式から必要になった項目".to_string()),
            waivers: vec![Waiver {
                axis: "verify".to_string(),
                reason: "文言のみのため目視レビューで代替".to_string(),
            }],
            from: Some("REQ-003#AC1".to_string()),
            implicit_of: Some("REQ-003".to_string()),
            reserved_attrs: reserved,
            link_baselines: baselines,
            ..Default::default()
        };
        let json = serde_json::to_string(&sub).unwrap();
        let back: SubItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back.def_hash.as_deref(), Some("f00dcafe"));
        assert_eq!(back.acceptance.len(), 2);
        assert_eq!(back.acceptance[0].label, "AC1");
        assert_eq!(back.acceptance[0].kind, "gwt");
        assert_eq!(back.rationale.as_deref(), Some("総当たり攻撃の抑止"));
        assert_eq!(
            back.derived.as_deref(),
            Some("実装方式から必要になった項目")
        );
        assert_eq!(back.waivers.len(), 1);
        assert_eq!(back.waivers[0].axis, "verify");
        assert_eq!(back.from.as_deref(), Some("REQ-003#AC1"));
        assert_eq!(back.implicit_of.as_deref(), Some("REQ-003"));
        assert_eq!(
            back.reserved_attrs.get("assignee").map(String::as_str),
            Some("alice")
        );
        assert_eq!(
            back.link_baselines.get("REQ-003#AC1").map(String::as_str),
            Some("deadbeef")
        );
    }

    /// M3 (wiki/270-vmodel-m3-design.md §2.2, FR-307): `assignee` is its own
    /// field now, independent of `reserved_attrs`, and round-trips through
    /// `serde_json`.
    #[test]
    fn sub_item_assignee_field_round_trips_through_json() {
        let sub = SubItem {
            index: 0,
            description: "REQ-003".to_string(),
            stable_id: Some("REQ-003".to_string()),
            assignee: Some("ryoma".to_string()),
            ..Default::default()
        };
        let json = serde_json::to_string(&sub).unwrap();
        let back: SubItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back.assignee.as_deref(), Some("ryoma"));
        assert!(
            !back.reserved_attrs.contains_key("assignee"),
            "assignee must not also appear in reserved_attrs: {json}"
        );
    }

    /// M3 (wiki/270-vmodel-m3-design.md §2.2): a `SubItem` with `assignee`
    /// unset must not serialize the key at all (NFR-004, no spurious diff).
    #[test]
    fn sub_item_assignee_absent_from_json_when_unset() {
        let sub = SubItem {
            index: 0,
            description: "既存のサブ項目".to_string(),
            stable_id: Some("C01-1.1".to_string()),
            ..Default::default()
        };
        let json = serde_json::to_string(&sub).unwrap();
        assert!(
            !json.contains("\"assignee\""),
            "unset assignee must not appear in serialized SubItem: {json}"
        );
    }

    /// M3 (wiki/270-vmodel-m3-design.md §2.3, FR-406): the approval axis's
    /// own fields round-trip through `serde_json`.
    #[test]
    fn sub_item_approval_fields_round_trip_through_json() {
        let sub = SubItem {
            index: 0,
            description: "REQ-003".to_string(),
            stable_id: Some("REQ-003".to_string()),
            approval: Some("approved".to_string()),
            approved_hash: Some("a1b2c3d4".to_string()),
            approved_by: Some("ryoma".to_string()),
            approved_at: Some("2026-10-05T14:15:00.123Z".to_string()),
            ..Default::default()
        };
        let json = serde_json::to_string(&sub).unwrap();
        let back: SubItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back.approval.as_deref(), Some("approved"));
        assert_eq!(back.approved_hash.as_deref(), Some("a1b2c3d4"));
        assert_eq!(back.approved_by.as_deref(), Some("ryoma"));
        assert_eq!(
            back.approved_at.as_deref(),
            Some("2026-10-05T14:15:00.123Z")
        );
    }

    /// M3 (wiki/270-vmodel-m3-design.md §2.3): a `SubItem` with the approval
    /// fields unset must not serialize any of them (NFR-004, no spurious
    /// diff) — this is also the shape a pre-M3 on-disk fixture has, so this
    /// doubles as the "deserializes without the new fields" case the other
    /// M3 fields (`assignee`/`needs`) each have their own test for.
    #[test]
    fn sub_item_approval_fields_absent_from_json_when_unset() {
        let sub = SubItem {
            index: 0,
            description: "既存のサブ項目".to_string(),
            stable_id: Some("C01-1.1".to_string()),
            ..Default::default()
        };
        let json = serde_json::to_string(&sub).unwrap();
        assert!(
            !json.contains("\"approval\""),
            "unset approval must not appear in serialized SubItem: {json}"
        );
        assert!(!json.contains("\"approved_hash\""));
        assert!(!json.contains("\"approved_by\""));
        assert!(!json.contains("\"approved_at\""));

        // A pre-M3 `_doc.*.json` fixture (no approval fields at all) must
        // still deserialize cleanly — the E12 compat read-mapping then
        // applies at the `approval_str` call site
        // (`src/mcp/handlers/trace.rs`), not here.
        let pre_m3_json = r#"{
            "index": 0,
            "description": "既存のサブ項目",
            "status": "verified",
            "stable_id": "C01-1.1"
        }"#;
        let back: SubItem = serde_json::from_str(pre_m3_json).unwrap();
        assert!(back.approval.is_none());
        assert!(back.approved_hash.is_none());
    }

    /// M3 compat (wiki/270-vmodel-m3-design.md §7): a pre-M3 on-disk
    /// `SubItem` with `assignee` stored under `reserved_attrs` (M2's
    /// behavior) still deserializes cleanly — the new `assignee` field
    /// simply defaults to `None` until the next layer sync re-derives it
    /// from the body (layer_sync.rs's concern, not model.rs's).
    #[test]
    fn sub_item_deserializes_pre_m3_reserved_attrs_assignee_without_the_new_field() {
        let json = r#"{
            "index": 0,
            "description": "既存のサブ項目",
            "status": "verified",
            "stable_id": "SPEC-012",
            "reserved_attrs": {"assignee": "alice"}
        }"#;
        let sub: SubItem = serde_json::from_str(json).unwrap();
        assert_eq!(sub.assignee, None);
        assert_eq!(
            sub.reserved_attrs.get("assignee").map(String::as_str),
            Some("alice")
        );
    }

    /// M3 (wiki/270-vmodel-m3-design.md §2.1, FR-202): `needs` is its own
    /// field now, independent of `reserved_attrs`, and round-trips through
    /// `serde_json` when `Some(non-empty)`.
    #[test]
    fn sub_item_needs_field_round_trips_through_json_when_non_empty() {
        let sub = SubItem {
            index: 0,
            description: "REQ-003".to_string(),
            stable_id: Some("REQ-003".to_string()),
            needs: Some(vec!["acceptance".to_string(), "system_test".to_string()]),
            ..Default::default()
        };
        let json = serde_json::to_string(&sub).unwrap();
        let back: SubItem = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.needs,
            Some(vec!["acceptance".to_string(), "system_test".to_string()])
        );
        assert!(
            !back.reserved_attrs.contains_key("needs"),
            "needs must not also appear in reserved_attrs: {json}"
        );
    }

    /// M3 §2.1's 3-state semantics: `Some(vec![])` ("- needs:" authored with
    /// an empty value, i.e. explicit "no coverage required") must be
    /// distinguishable from `None` (unset, falls back to the profile's
    /// `default_needs`) — both round-trip through JSON without collapsing
    /// into each other.
    #[test]
    fn sub_item_needs_empty_vec_round_trips_distinct_from_none() {
        let sub = SubItem {
            index: 0,
            description: "REQ-100".to_string(),
            stable_id: Some("REQ-100".to_string()),
            needs: Some(Vec::new()),
            ..Default::default()
        };
        let json = serde_json::to_string(&sub).unwrap();
        let back: SubItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back.needs, Some(Vec::new()));
        assert_ne!(back.needs, None);
    }

    /// M3 (wiki/270-vmodel-m3-design.md §2.1): a `SubItem` with `needs`
    /// unset (`None`) must not serialize the key at all (NFR-004, no
    /// spurious diff) — this is the "apply default_needs" state, not an
    /// authored empty list.
    #[test]
    fn sub_item_needs_absent_from_json_when_unset() {
        let sub = SubItem {
            index: 0,
            description: "既存のサブ項目".to_string(),
            stable_id: Some("C01-1.1".to_string()),
            ..Default::default()
        };
        let json = serde_json::to_string(&sub).unwrap();
        assert!(
            !json.contains("\"needs\""),
            "unset needs must not appear in serialized SubItem: {json}"
        );
    }

    /// M3 compat (wiki/270-vmodel-m3-design.md §7): a pre-M3 on-disk
    /// `SubItem` with `needs` stored under `reserved_attrs` (M2's behavior)
    /// still deserializes cleanly — the new `needs` field simply defaults to
    /// `None` until the next layer sync re-derives it from the body
    /// (layer_sync.rs's concern, not model.rs's).
    #[test]
    fn sub_item_deserializes_pre_m3_reserved_attrs_needs_without_the_new_field() {
        let json = r#"{
            "index": 0,
            "description": "既存のサブ項目",
            "status": "verified",
            "stable_id": "SPEC-012",
            "reserved_attrs": {"needs": "acceptance"}
        }"#;
        let sub: SubItem = serde_json::from_str(json).unwrap();
        assert_eq!(sub.needs, None);
        assert_eq!(
            sub.reserved_attrs.get("needs").map(String::as_str),
            Some("acceptance")
        );
    }

    /// NFR-004 (no spurious diff): a `SubItem` with every M2 field left
    /// unset must serialize identically to a pre-M2 `SubItem` — none of the
    /// new keys should appear.
    #[test]
    fn sub_item_m2_body_notation_fields_absent_from_json_when_unset() {
        let sub = SubItem {
            index: 0,
            description: "既存のサブ項目".to_string(),
            stable_id: Some("C01-1.1".to_string()),
            ..Default::default()
        };
        let json = serde_json::to_string(&sub).unwrap();
        for key in [
            "def_hash",
            "acceptance",
            "rationale",
            "derived",
            "waivers",
            "from",
            "implicit_of",
            "reserved_attrs",
            "link_baselines",
        ] {
            assert!(
                !json.contains(&format!("\"{key}\"")),
                "unset M2 field '{key}' must not appear in serialized SubItem: {json}"
            );
        }
    }

    #[test]
    fn valid_constants_contain_spec_values() {
        assert!(VALID_DOC_TYPES.contains(&"spec"));
        assert!(VALID_DOC_TYPES.contains(&"note"));
        assert!(VALID_AUTO_INJECT.contains(&"outline"));
        assert!(VALID_RELATIONS.contains(&"supersedes"));
    }
}
