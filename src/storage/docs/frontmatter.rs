//! YAML frontmatter <-> [`DocMetadata`] serialize/deserialize (frontmatter
//! migration, t123.1) plus single-file read/write helpers for
//! `_doc.<slug>.md` (frontmatter + body, replacing the old
//! `_doc.<slug>.json` + `_doc.<slug>.md` pair).
//!
//! `sections[]` is deliberately never part of the frontmatter shape: byte
//! offsets are computed fresh from the body on every read (t123.2), so they
//! can never go stale after a manual edit. `version` (schema version) and
//! `slug` (derived from the filename) are likewise omitted from the
//! frontmatter body — both are storage-layer bookkeeping, not document
//! content.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::model::{DocMetadata, DocRelation, DocSource, Verification, DOC_SCHEMA_VERSION};

/// YAML-facing mirror of [`DocMetadata`], minus `version`, `slug`, and
/// `sections` (see module docs), plus alias support on the handful of
/// fields the frontmatter spec defines common aliases for. This is a
/// separate type from `DocMetadata` (rather than reusing it directly with
/// `#[serde(skip)]`) so YAML aliases don't leak into the JSON-era shape any
/// other code still round-trips through `serde_json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FrontmatterDoc {
    id: String,
    #[serde(alias = "name")]
    title: String,
    doc_type: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    scope_paths: Vec<String>,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    children: Vec<String>,
    #[serde(default)]
    related: Vec<DocRelation>,
    #[serde(default = "default_auto_inject")]
    auto_inject: String,
    #[serde(default)]
    task_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    layer: Option<String>,
    #[serde(default)]
    source: FrontmatterSource,
    #[serde(default)]
    has_bom: bool,
    #[serde(default = "default_line_ending")]
    line_ending: String,
    #[serde(default = "default_split_level")]
    split_level: u8,
    #[serde(alias = "date", alias = "created", alias = "publishDate")]
    created_at: String,
    #[serde(alias = "lastmod", alias = "modified", alias = "last_update")]
    updated_at: String,
    #[serde(default)]
    content_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    verification: Option<Verification>,
    /// `description` accepts several common frontmatter aliases on read;
    /// write always emits the canonical `description` key. Not a field on
    /// `DocMetadata` itself (no storage-layer concept of "description" yet)
    /// — captured here purely so a value under any alias round-trips into
    /// `extra["description"]` instead of being silently dropped.
    #[serde(
        default,
        alias = "excerpt",
        alias = "summary",
        alias = "abstract",
        skip_serializing_if = "Option::is_none"
    )]
    description: Option<String>,

    /// Every YAML key not covered by a named field above, preserved for
    /// round-trip fidelity.
    #[serde(flatten)]
    extra: HashMap<String, Value>,
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

/// `source:` sub-block in frontmatter. Deliberately excludes
/// `DocSource::frontmatter`/`frontmatter_trailing_eol` (see module docs
/// header and the `t123.1` design note: these two fields existed only to
/// stash a stripped *user* frontmatter block under the old 2-file format;
/// in the new format the frontmatter itself IS the metadata, so there's
/// nothing left to stash).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct FrontmatterSource {
    #[serde(default)]
    origin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    original_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    canonical_hash: Option<String>,
}

impl TryFrom<&DocMetadata> for FrontmatterDoc {
    type Error = anyhow::Error;

    /// Fails when `doc.content_hash` is `None` — the on-disk frontmatter
    /// schema's `content_hash` is a plain, always-present `String` (P-M1,
    /// wiki/240-performance-design.md §4, t370.8: `DocMetadata.content_hash`
    /// is lazily `Option<String>` in memory, but a value must always be
    /// computed before it reaches disk). Callers write through
    /// `super::write_doc_with_body`, which fills the hash in if still
    /// missing — reaching this error means that invariant was bypassed,
    /// which must fail loudly rather than silently persist an empty hash.
    fn try_from(doc: &DocMetadata) -> Result<Self> {
        let content_hash = doc.content_hash.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "cannot serialize frontmatter for document '{}': content_hash not computed \
                 (write through write_doc/write_doc_with_body, which compute it if missing)",
                doc.id
            )
        })?;
        let mut extra = doc.extra.clone();
        let description = extra
            .remove("description")
            .and_then(|v| v.as_str().map(str::to_string));
        Ok(FrontmatterDoc {
            id: doc.id.clone(),
            title: doc.title.clone(),
            doc_type: doc.doc_type.clone(),
            tags: doc.tags.clone(),
            scope_paths: doc.scope_paths.clone(),
            parent_id: doc.parent_id.clone(),
            children: doc.children.clone(),
            related: doc.related.clone(),
            auto_inject: doc.auto_inject.clone(),
            task_ids: doc.task_ids.clone(),
            layer: doc.layer.clone(),
            source: FrontmatterSource {
                origin: doc.source.origin.clone(),
                original_path: doc.source.original_path.clone(),
                canonical_hash: doc.source.canonical_hash.clone(),
            },
            has_bom: doc.has_bom,
            line_ending: doc.line_ending.clone(),
            split_level: doc.split_level,
            created_at: doc.created_at.clone(),
            updated_at: doc.updated_at.clone(),
            content_hash,
            verification: doc.verification.clone(),
            description,
            extra,
        })
    }
}

impl FrontmatterDoc {
    /// Converts back into a [`DocMetadata`], filling in the storage-layer
    /// fields (`version`, `slug`, `sections`) that don't live in
    /// frontmatter. `slug` is derived by the caller from the filename;
    /// `sections` is always computed fresh by [`super::split::compute_sections`]
    /// after this call.
    fn into_doc_metadata(mut self, slug: String) -> DocMetadata {
        if let Some(description) = self.description.take() {
            self.extra
                .insert("description".to_string(), Value::String(description));
        }
        DocMetadata {
            version: DOC_SCHEMA_VERSION,
            id: self.id,
            slug,
            title: self.title,
            doc_type: self.doc_type,
            tags: self.tags,
            scope_paths: self.scope_paths,
            parent_id: self.parent_id,
            children: self.children,
            related: self.related,
            auto_inject: self.auto_inject,
            task_ids: self.task_ids,
            layer: self.layer,
            source: DocSource {
                origin: self.source.origin,
                original_path: self.source.original_path,
                canonical_hash: self.source.canonical_hash,
                frontmatter: None,
                frontmatter_trailing_eol: true,
            },
            has_bom: self.has_bom,
            line_ending: self.line_ending,
            split_level: self.split_level,
            sections: Vec::new(),
            created_at: self.created_at,
            updated_at: self.updated_at,
            // Trusted as-is here (this is the raw, low-level parse used
            // directly by `write_doc_body`'s "preserve existing frontmatter"
            // path) — `super::read_doc`/`read_doc_with_body` always
            // overwrite this immediately afterward via
            // `recompute_sections_and_hash` (P-M1, t370.8: `Some`/`None`
            // there depending on whether the caller asked for a hash), so
            // what this raw parse puts here doesn't matter for those paths.
            content_hash: Some(self.content_hash),
            verification: self.verification,
            extra: self.extra,
        }
    }
}

/// Converts `doc` into a YAML frontmatter string, **without** the enclosing
/// `---` fences (callers wrap it — see [`write_frontmatter_doc`]). Never
/// includes `sections[]`, `version`, or `slug` (see module docs). Fails if
/// `doc.content_hash` hasn't been computed yet (see
/// `FrontmatterDoc::try_from`'s doc comment).
pub fn serialize_frontmatter(doc: &DocMetadata) -> Result<String> {
    let fm = FrontmatterDoc::try_from(doc)?;
    serde_yaml::to_string(&fm).context("Failed to serialize document frontmatter")
}

/// Parses a YAML frontmatter block (the text between the `---` fences,
/// exclusive) back into a [`DocMetadata`]. `slug` is supplied by the caller
/// (derived from the filename, not stored in frontmatter itself).
pub fn deserialize_frontmatter(yaml_str: &str, slug: &str) -> Result<DocMetadata> {
    let fm: FrontmatterDoc =
        serde_yaml::from_str(yaml_str).context("Failed to parse document frontmatter as YAML")?;
    Ok(fm.into_doc_metadata(slug.to_string()))
}

/// Splits a `_doc.<slug>.md` file's raw content into `(frontmatter_yaml,
/// body)`. Returns `None` for the frontmatter half when the content doesn't
/// start with a `---` fence (old-format body-only file, or a document that
/// somehow lost its frontmatter).
fn split_frontmatter_and_body(content: &str) -> (Option<&str>, &str) {
    let Some(after_open) = content.strip_prefix("---\n") else {
        return (None, content);
    };
    // Find the closing fence: a line that is exactly "---" on its own line.
    let mut search_from = 0usize;
    loop {
        let Some(rel_idx) = after_open[search_from..].find("\n---") else {
            return (None, content);
        };
        let idx = search_from + rel_idx;
        // `idx` points at the '\n' right before "---". The fence itself
        // starts at idx+1.
        let fence_start = idx + 1;
        let after_fence = &after_open[fence_start + 3..];
        // The closing fence line must end the line here: either end of
        // string, '\n', or '\r\n'.
        if after_fence.is_empty() {
            return (Some(&after_open[..idx]), "");
        }
        if let Some(rest) = after_fence.strip_prefix('\n') {
            return (Some(&after_open[..idx]), rest);
        }
        if let Some(rest) = after_fence.strip_prefix("\r\n") {
            return (Some(&after_open[..idx]), rest);
        }
        // Not actually a fence line (e.g. "----" or "--- foo") — keep
        // searching past it.
        search_from = fence_start + 3;
    }
}

/// Reads a `_doc.<slug>.md` file and splits it into `(metadata, body)`.
/// Returns `Ok(None)` when the file does not exist. Returns `Ok(Some((doc,
/// body)))` with `doc.sections` empty — callers must compute sections
/// on-demand from `body` (t123.2; see `super::read_doc`).
///
/// Returns `Err` when the file exists, starts with a `---` fence, but the
/// enclosed YAML fails to parse (corrupt frontmatter) — this is distinct
/// from "no frontmatter at all" (which is a migration signal handled by the
/// caller, not an error here).
pub fn read_frontmatter_doc(path: &Path, slug: &str) -> Result<Option<(DocMetadata, String)>> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(e).with_context(|| format!("Failed to read document: {}", path.display()))
        }
    };
    let (Some(fm_yaml), body) = split_frontmatter_and_body(&content) else {
        return Ok(None);
    };
    let doc = deserialize_frontmatter(fm_yaml, slug)?;
    Ok(Some((doc, body.to_string())))
}

/// Writes a single `_doc.<slug>.md` file: YAML frontmatter (fenced by
/// `---`) followed by `body` verbatim. `doc.sections` is never
/// serialized (see module docs) regardless of what it currently holds.
pub fn write_frontmatter_doc(path: &Path, doc: &DocMetadata, body: &str) -> Result<()> {
    let fm_yaml = serialize_frontmatter(doc)?;
    let content = format!("---\n{fm_yaml}---\n{body}");
    crate::storage::atomic_write(path, content.as_bytes())
        .with_context(|| format!("Failed to write document: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::docs::model::{CodeRef, VerificationItem};

    fn sample_doc() -> DocMetadata {
        let mut doc = DocMetadata::new(
            "doc-20260718-120000-123456".to_string(),
            "my-slug".to_string(),
            "Document Title".to_string(),
            "spec".to_string(),
            "2026-07-18T12:00:00Z".to_string(),
        );
        doc.tags = vec!["auth".to_string(), "security".to_string()];
        doc.scope_paths = vec!["src/auth/".to_string()];
        doc.task_ids = vec!["t42".to_string(), "t43".to_string()];
        doc.parent_id = Some("doc-20260710-000000-000001".to_string());
        doc.related = vec![DocRelation {
            id: "doc-other".to_string(),
            rel: "supersedes".to_string(),
        }];
        doc.source.origin = "authored".to_string();
        doc.source.canonical_hash = Some("abc123".to_string());
        doc.content_hash = Some("def456".to_string());
        doc.updated_at = "2026-07-18T15:30:00Z".to_string();
        doc
    }

    #[test]
    fn serialize_frontmatter_roundtrips_core_fields() {
        let doc = sample_doc();
        let yaml = serialize_frontmatter(&doc).unwrap();
        let back = deserialize_frontmatter(&yaml, &doc.slug).unwrap();

        assert_eq!(back.id, doc.id);
        assert_eq!(back.slug, doc.slug);
        assert_eq!(back.title, doc.title);
        assert_eq!(back.doc_type, doc.doc_type);
        assert_eq!(back.tags, doc.tags);
        assert_eq!(back.scope_paths, doc.scope_paths);
        assert_eq!(back.task_ids, doc.task_ids);
        assert_eq!(back.parent_id, doc.parent_id);
        assert_eq!(back.related, doc.related);
        assert_eq!(back.source.origin, doc.source.origin);
        assert_eq!(back.source.canonical_hash, doc.source.canonical_hash);
        assert_eq!(back.content_hash, doc.content_hash);
        assert_eq!(back.created_at, doc.created_at);
        assert_eq!(back.updated_at, doc.updated_at);
    }

    /// M1 t360.4 (wiki/220-vmodel-integration-design.md §2.1): `doc_save`'s
    /// `layer` argument is the only AI-facing way to set `DocMetadata.layer`;
    /// this test only exercises the frontmatter round-trip that argument
    /// ultimately persists through.
    #[test]
    fn layer_roundtrips_through_yaml_frontmatter_when_set() {
        let mut doc = sample_doc();
        doc.layer = Some("basic_spec".to_string());
        let yaml = serialize_frontmatter(&doc).unwrap();
        assert!(
            yaml.contains("layer: basic_spec"),
            "layer must appear in frontmatter when set: {yaml}"
        );
        let back = deserialize_frontmatter(&yaml, &doc.slug).unwrap();
        assert_eq!(back.layer, doc.layer);
    }

    /// NFR-001/002/004 (wiki/220 §5, §2.1 "未設定の文書は従来どおり"): a
    /// document that never had its layer set must not gain a `layer:` key —
    /// otherwise every pre-M1 document would show a spurious frontmatter
    /// diff the first time handoff-mcp re-saves it.
    #[test]
    fn layer_is_absent_from_frontmatter_when_unset() {
        let doc = sample_doc();
        assert_eq!(doc.layer, None, "sample_doc must start with no layer set");
        let yaml = serialize_frontmatter(&doc).unwrap();
        assert!(
            !yaml.lines().any(|l| l.starts_with("layer:")),
            "layer key must be absent from frontmatter when unset: {yaml}"
        );
        let back = deserialize_frontmatter(&yaml, &doc.slug).unwrap();
        assert_eq!(back.layer, None);
    }

    #[test]
    fn serialize_frontmatter_never_includes_sections_version_or_slug() {
        let mut doc = sample_doc();
        doc.sections = vec![super::super::model::SectionIndex {
            seq: 0,
            heading: String::new(),
            level: 0,
            byte_offset: 0,
            byte_length: 10,
            content_hash: Some("h".to_string()),
        }];
        let yaml = serialize_frontmatter(&doc).unwrap();
        assert!(
            !yaml.contains("sections"),
            "sections[] must never be written to frontmatter: {yaml}"
        );
        assert!(
            !yaml.contains("version:"),
            "schema version must not be in frontmatter: {yaml}"
        );
        assert!(
            !yaml.lines().any(|l| l.starts_with("slug:")),
            "slug must not be in frontmatter (derived from filename): {yaml}"
        );
    }

    #[test]
    fn verification_round_trips_through_yaml_frontmatter() {
        let mut doc = sample_doc();
        doc.verification = Some(Verification {
            status: "in_progress".to_string(),
            created_at: "2026-07-18T12:00:00Z".to_string(),
            updated_at: "2026-07-18T12:00:00Z".to_string(),
            items: vec![VerificationItem {
                fragment_seq: Some(1),
                heading: "Section Title".to_string(),
                status: "verified".to_string(),
                impl_refs: vec![CodeRef {
                    path: "src/foo.rs".to_string(),
                    lines: Some("10-50".to_string()),
                    label: None,
                }],
                test_refs: vec![CodeRef {
                    path: "tests/foo.rs".to_string(),
                    lines: None,
                    label: None,
                }],
                reviewer: None,
                verified_at: Some("2026-07-18T12:00:00Z".to_string()),
                notes: String::new(),
                content_hash_at_verify: Some("abc123".to_string()),
                category: "section".to_string(),
                sub_items: Vec::new(),
                label: None,
            }],
        });

        let yaml = serialize_frontmatter(&doc).unwrap();
        let back = deserialize_frontmatter(&yaml, &doc.slug).unwrap();
        let v = back.verification.expect("verification must round-trip");
        assert_eq!(v.status, "in_progress");
        assert_eq!(v.items.len(), 1);
        assert_eq!(v.items[0].fragment_seq, Some(1));
        assert_eq!(v.items[0].impl_refs[0].path, "src/foo.rs");
        assert_eq!(v.items[0].impl_refs[0].lines.as_deref(), Some("10-50"));
        assert_eq!(v.items[0].test_refs[0].path, "tests/foo.rs");
        assert_eq!(v.items[0].content_hash_at_verify.as_deref(), Some("abc123"));
    }

    #[test]
    fn sub_item_stable_id_round_trips_through_yaml() {
        use crate::storage::docs::model::SubItem;

        let mut doc = sample_doc();
        doc.verification = Some(Verification {
            status: "pending".to_string(),
            created_at: "2026-09-21T00:00:00Z".to_string(),
            updated_at: "2026-09-21T00:00:00Z".to_string(),
            items: vec![VerificationItem {
                fragment_seq: Some(1),
                heading: "Section".to_string(),
                status: "pending".to_string(),
                impl_refs: Vec::new(),
                test_refs: Vec::new(),
                reviewer: None,
                verified_at: None,
                notes: String::new(),
                content_hash_at_verify: None,
                category: "section".to_string(),
                sub_items: vec![SubItem {
                    index: 0,
                    description: "Test requirement".to_string(),
                    stable_id: Some("C01-1.1".to_string()),
                    dev_stage: Some("in_progress".to_string()),
                    priority: Some("P0".to_string()),
                    ..Default::default()
                }],
                label: None,
            }],
        });

        let yaml = serialize_frontmatter(&doc).unwrap();
        assert!(
            yaml.contains("stable_id"),
            "stable_id must appear in YAML output: {yaml}"
        );
        assert!(
            yaml.contains("C01-1.1"),
            "stable_id value must appear in YAML output: {yaml}"
        );
        assert!(
            yaml.contains("dev_stage"),
            "dev_stage must appear in YAML output: {yaml}"
        );
        assert!(
            yaml.contains("priority"),
            "priority must appear in YAML output: {yaml}"
        );

        let back = deserialize_frontmatter(&yaml, &doc.slug).unwrap();
        let v = back.verification.expect("verification must round-trip");
        assert_eq!(v.items[0].sub_items.len(), 1);
        let si = &v.items[0].sub_items[0];
        assert_eq!(si.stable_id.as_deref(), Some("C01-1.1"));
        assert_eq!(si.dev_stage.as_deref(), Some("in_progress"));
        assert_eq!(si.priority.as_deref(), Some("P0"));
    }

    #[test]
    fn deserialize_frontmatter_accepts_documented_aliases() {
        let yaml = "id: doc-1\n\
                     title: T\n\
                     doc_type: note\n\
                     date: 2026-01-01T00:00:00Z\n\
                     lastmod: 2026-01-02T00:00:00Z\n\
                     excerpt: A short summary\n";
        let doc = deserialize_frontmatter(yaml, "slug-1").unwrap();
        assert_eq!(doc.created_at, "2026-01-01T00:00:00Z");
        assert_eq!(doc.updated_at, "2026-01-02T00:00:00Z");
        assert_eq!(
            doc.extra.get("description").and_then(|v| v.as_str()),
            Some("A short summary")
        );
    }

    #[test]
    fn deserialize_frontmatter_preserves_unknown_keys_in_extra() {
        let yaml = "id: doc-1\n\
                     title: T\n\
                     doc_type: note\n\
                     created_at: 2026-01-01T00:00:00Z\n\
                     updated_at: 2026-01-01T00:00:00Z\n\
                     custom_field: hello\n\
                     another: 42\n";
        let doc = deserialize_frontmatter(yaml, "slug-1").unwrap();
        assert_eq!(
            doc.extra.get("custom_field").and_then(|v| v.as_str()),
            Some("hello")
        );
        assert_eq!(doc.extra.get("another").and_then(|v| v.as_i64()), Some(42));
    }

    #[test]
    fn extra_fields_round_trip_through_write_then_read() {
        let mut doc = sample_doc();
        doc.extra.insert(
            "custom_field".to_string(),
            Value::String("hello".to_string()),
        );
        let yaml = serialize_frontmatter(&doc).unwrap();
        assert!(yaml.contains("custom_field"));
        let back = deserialize_frontmatter(&yaml, &doc.slug).unwrap();
        assert_eq!(
            back.extra.get("custom_field").and_then(|v| v.as_str()),
            Some("hello")
        );
    }

    #[test]
    fn write_then_read_frontmatter_doc_roundtrip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("_doc.my-slug.md");
        let doc = sample_doc();
        let body = "# Document Title\n\n## Section 1\n\nBody text.\n";

        write_frontmatter_doc(&path, &doc, body).unwrap();
        let (back_doc, back_body) = read_frontmatter_doc(&path, &doc.slug)
            .unwrap()
            .expect("file must exist");

        assert_eq!(back_doc.id, doc.id);
        assert_eq!(back_doc.title, doc.title);
        assert_eq!(back_body, body);
        assert!(
            back_doc.sections.is_empty(),
            "sections must not be persisted/parsed from frontmatter"
        );
    }

    #[test]
    fn read_frontmatter_doc_missing_file_is_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("_doc.nope.md");
        assert!(read_frontmatter_doc(&path, "nope").unwrap().is_none());
    }

    #[test]
    fn read_frontmatter_doc_without_frontmatter_is_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("_doc.old-format.md");
        std::fs::write(&path, "# Just a plain body\n\nNo frontmatter here.\n").unwrap();
        assert!(
            read_frontmatter_doc(&path, "old-format").unwrap().is_none(),
            "a body-only file (no leading '---' fence) must be treated as \
             'no frontmatter', not an error — the caller decides whether \
             that's an old-format migration or a genuine error case"
        );
    }

    #[test]
    fn read_frontmatter_doc_corrupt_yaml_is_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("_doc.corrupt.md");
        std::fs::write(&path, "---\nid: [unterminated\n---\nbody\n").unwrap();
        assert!(read_frontmatter_doc(&path, "corrupt").is_err());
    }

    #[test]
    fn write_frontmatter_doc_body_survives_headings_that_look_like_yaml() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("_doc.tricky.md");
        let doc = sample_doc();
        // Body containing a line that looks like a closing fence prefix
        // ("---") must not confuse the frontmatter/body split.
        let body = "# Title\n\n---\n\nA horizontal rule inside the body.\n";

        write_frontmatter_doc(&path, &doc, body).unwrap();
        let (_, back_body) = read_frontmatter_doc(&path, &doc.slug).unwrap().unwrap();
        assert_eq!(back_body, body);
    }
}
