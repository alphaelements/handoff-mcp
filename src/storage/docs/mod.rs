//! Document management: splitting a single authored Markdown body into
//! in-memory sections, and persisting documents to `.handoff/docs/` as a
//! 2-file slug-based pair (v5 rearchitecture,
//! wiki/130-document-management.md §3.1).
//!
//! Layout:
//!
//! ```text
//! .handoff/docs/
//!   _doc.<slug>.json   # document metadata (incl. `sections[]` byte-offset index)
//!   _doc.<slug>.md     # full document body (pure Markdown, never split into files)
//!   injected/
//!     <session-id>.json   # per-session "already injected" sidecar
//! ```
//!
//! `slug` is a human-readable, caller-supplied name (`[a-z0-9-]`, max
//! [`model::MAX_SLUG_LEN`] chars) used purely for file naming so `ls
//! .handoff/docs/` is self-describing. The stable `id` (timestamp-based)
//! stays inside the metadata JSON for family-tree/task-link references;
//! [`find_doc_by_id`] resolves an `id` back to its document when the slug
//! isn't known by the caller.
//!
//! See `wiki/130-document-management.md` §3-4 for the full storage
//! architecture and data model.
//!
//! All writes go through [`crate::storage::atomic_write`] and `docs/` is
//! created lazily on first write (mirrors `src/storage/memory/mod.rs`), so
//! projects created before this feature shipped are unaffected until they
//! first call `doc_save`.

pub mod model;
pub mod reassemble;
pub mod split;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

pub use model::{
    CodeRef, DocMetadata, DocRelation, DocSource, SectionIndex, Verification, VerificationItem,
};

/// Path to the `docs/` directory inside a `.handoff/` dir.
pub fn docs_dir(handoff_dir: &Path) -> PathBuf {
    handoff_dir.join("docs")
}

/// Ensure `docs/` exists, creating it lazily. Mirrors
/// `memory::ensure_memory_dir` so projects initialized before this feature
/// shipped never had a `docs/` dir until the first `doc_save`.
pub fn ensure_docs_dir(handoff_dir: &Path) -> Result<PathBuf> {
    let dir = docs_dir(handoff_dir);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create docs dir: {}", dir.display()))?;
    Ok(dir)
}

/// Validates a `slug`: only `[a-z0-9-]`, length 1..=[`model::MAX_SLUG_LEN`].
/// Used by `doc_save` to reject a bad slug before any file is written.
pub fn validate_slug(slug: &str) -> Result<()> {
    if slug.is_empty() {
        bail!("slug must not be empty");
    }
    if slug.len() > model::MAX_SLUG_LEN {
        bail!(
            "slug '{slug}' exceeds max length of {} characters",
            model::MAX_SLUG_LEN
        );
    }
    if !slug
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        bail!("slug '{slug}' must contain only lowercase letters, digits, and hyphens ([a-z0-9-])");
    }
    Ok(())
}

fn doc_meta_path(handoff_dir: &Path, slug: &str) -> PathBuf {
    docs_dir(handoff_dir).join(format!("_doc.{slug}.json"))
}

fn doc_body_path(handoff_dir: &Path, slug: &str) -> PathBuf {
    docs_dir(handoff_dir).join(format!("_doc.{slug}.md"))
}

/// Write a document's metadata to `_doc.<slug>.json` atomically, creating
/// `docs/` lazily.
pub fn write_doc(handoff_dir: &Path, doc: &DocMetadata) -> Result<PathBuf> {
    ensure_docs_dir(handoff_dir)?;
    let path = doc_meta_path(handoff_dir, &doc.slug);
    let content = serde_json::to_string_pretty(doc).context("Failed to serialize document")?;
    crate::storage::atomic_write(&path, content.as_bytes())
        .with_context(|| format!("Failed to write document: {}", path.display()))?;
    Ok(path)
}

/// Write a document's full body to `_doc.<slug>.md` atomically, creating
/// `docs/` lazily. `body` is written exactly as given — no re-rendering —
/// so it can be read back byte-identical via [`read_doc_body`].
pub fn write_doc_body(handoff_dir: &Path, slug: &str, body: &str) -> Result<PathBuf> {
    ensure_docs_dir(handoff_dir)?;
    let path = doc_body_path(handoff_dir, slug);
    crate::storage::atomic_write(&path, body.as_bytes())
        .with_context(|| format!("Failed to write document body: {}", path.display()))?;
    Ok(path)
}

/// Read a document's full body from `_doc.<slug>.md`. Returns `Ok(None)`
/// when the file does not exist.
pub fn read_doc_body(handoff_dir: &Path, slug: &str) -> Result<Option<String>> {
    let path = doc_body_path(handoff_dir, slug);
    match std::fs::read_to_string(&path) {
        Ok(content) => Ok(Some(content)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => {
            Err(e).with_context(|| format!("Failed to read document body: {}", path.display()))
        }
    }
}

/// Delete a document's body file (`_doc.<slug>.md`) by exact slug. Returns
/// `Ok(false)` when the file does not exist.
pub fn delete_doc_body(handoff_dir: &Path, slug: &str) -> Result<bool> {
    let path = doc_body_path(handoff_dir, slug);
    if !path.exists() {
        return Ok(false);
    }
    std::fs::remove_file(&path)
        .with_context(|| format!("Failed to delete document body: {}", path.display()))?;
    Ok(true)
}

/// Read one document's metadata by exact slug. Returns `Ok(None)` when the
/// file does not exist or fails to parse (lenient read, same policy as
/// memory).
///
/// Caution: `slug` is a required (non-`#[serde(default)]`) field on
/// [`DocMetadata`], so this is **not** a backward-compat path for real v4
/// documents (which had no `slug` field and used per-fragment physical
/// files). A genuine v4 `_doc.*.json` fails to deserialize under the v5
/// schema and is silently treated as `Ok(None)` here — i.e. it would
/// disappear from `doc_get`/`doc_list`/`doc_query` with no warning, not be
/// gracefully migrated. This repo's own migration plan
/// (wiki/130-document-management.md, "移行" section) deliberately scoped v4
/// migration out because no real v4 documents exist outside dev test data;
/// if that assumption ever changes, a real migration path is needed before
/// pointing this binary at a directory with genuine v4 documents.
pub fn read_doc(handoff_dir: &Path, slug: &str) -> Result<Option<DocMetadata>> {
    let path = doc_meta_path(handoff_dir, slug);
    match std::fs::read_to_string(&path) {
        Ok(content) => Ok(serde_json::from_str::<DocMetadata>(&content).ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("Failed to read document: {}", path.display())),
    }
}

/// Read every document in `docs/`, skipping any `_doc.*.json` file that
/// fails to parse. `.md` body files and the `injected/` subdirectory are
/// ignored. Returns an empty vec when `docs/` does not exist (uninitialized
/// / feature-untouched projects).
///
/// See the caution on [`read_doc`]: a real v4 document (no `slug` field)
/// fails to parse under the v5 schema and is skipped here silently, same as
/// any other corrupt file — it is not migrated or surfaced as a warning.
pub fn read_all_docs(handoff_dir: &Path) -> Result<Vec<DocMetadata>> {
    let dir = docs_dir(handoff_dir);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .with_context(|| format!("Failed to read docs dir: {}", dir.display()))?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());

    let mut docs = Vec::new();
    for entry in entries {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("_doc.") || !name.ends_with(".json") {
            continue;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if let Ok(doc) = serde_json::from_str::<DocMetadata>(&content) {
            docs.push(doc);
        }
        // Unparseable file: skip silently (lenient read, mirrors memory).
    }
    Ok(docs)
}

/// Find a document by its stable `id` (not its file-naming `slug`), by
/// scanning every `_doc.*.json` in `docs/`. Used for backward-compat
/// lookups where a caller only has the `id` (e.g. family-tree
/// `parent_id`/`related[].id`, task-link reverse lookups) and not the slug.
/// Returns `Ok(None)` if no document with that `id` exists.
pub fn find_doc_by_id(handoff_dir: &Path, doc_id: &str) -> Result<Option<DocMetadata>> {
    let docs = read_all_docs(handoff_dir)?;
    Ok(docs.into_iter().find(|d| d.id == doc_id))
}

/// Resolve every `link_type == "doc"` entry in `task_links` to its
/// [`DocMetadata`], in one pass over `docs/` (via [`read_all_docs`]) rather
/// than one [`find_doc_by_id`] scan per link. `task_links[].target` holds the
/// document's stable `id` (see `crate::storage::tasks::sync_doc_task_links`),
/// so lookup is by `id`, not `slug`. Links whose target doesn't resolve to an
/// existing document (stale/dangling link) are silently skipped — callers
/// that need to detect that should compare the input link count against the
/// output length themselves.
pub fn batch_resolve_docs(
    handoff_dir: &Path,
    task_links: &[crate::storage::tasks::TaskLink],
) -> Result<Vec<DocMetadata>> {
    let doc_ids: Vec<&str> = task_links
        .iter()
        .filter(|l| l.link_type == "doc")
        .map(|l| l.target.as_str())
        .collect();
    if doc_ids.is_empty() {
        return Ok(Vec::new());
    }

    let all_docs = read_all_docs(handoff_dir)?;
    Ok(doc_ids
        .iter()
        .filter_map(|id| all_docs.iter().find(|d| &d.id == id).cloned())
        .collect())
}

/// Delete a document's metadata file by exact slug. Returns `Ok(false)` when
/// the file does not exist. Does not touch the document's body file —
/// callers that want a full delete should also call [`delete_doc_body`].
pub fn delete_doc(handoff_dir: &Path, slug: &str) -> Result<bool> {
    let path = doc_meta_path(handoff_dir, slug);
    if !path.exists() {
        return Ok(false);
    }
    std::fs::remove_file(&path)
        .with_context(|| format!("Failed to delete document: {}", path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn handoff(tmp: &TempDir) -> PathBuf {
        let dir = tmp.path().join(".handoff");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_doc(id: &str, slug: &str) -> DocMetadata {
        DocMetadata::new(
            id.to_string(),
            slug.to_string(),
            "Session Loop Verification".to_string(),
            "spec".to_string(),
            "2026-07-11T14:30:00Z".to_string(),
        )
    }

    #[test]
    fn validate_slug_accepts_lowercase_digits_hyphen() {
        assert!(validate_slug("doc-management-spec").is_ok());
        assert!(validate_slug("a").is_ok());
        assert!(validate_slug("a1-b2").is_ok());
    }

    #[test]
    fn validate_slug_rejects_empty() {
        assert!(validate_slug("").is_err());
    }

    #[test]
    fn validate_slug_rejects_uppercase_and_underscore_and_space() {
        assert!(validate_slug("Doc-Spec").is_err());
        assert!(validate_slug("doc_spec").is_err());
        assert!(validate_slug("doc spec").is_err());
    }

    #[test]
    fn validate_slug_rejects_over_max_length() {
        let too_long = "a".repeat(model::MAX_SLUG_LEN + 1);
        assert!(validate_slug(&too_long).is_err());
        let exactly_max = "a".repeat(model::MAX_SLUG_LEN);
        assert!(validate_slug(&exactly_max).is_ok());
    }

    #[test]
    fn write_then_read_doc_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        let mut doc = sample_doc("doc-1", "session-loop-verification");
        doc.tags = vec!["session-loop".to_string(), "verification".to_string()];
        doc.scope_paths = vec!["src/mcp/handlers/".to_string()];
        doc.task_ids = vec!["T-79".to_string()];
        doc.has_bom = true;
        doc.line_ending = "crlf".to_string();
        doc.source.frontmatter = Some("title: Foo\n".to_string());
        doc.sections = vec![
            SectionIndex {
                seq: 0,
                heading: String::new(),
                level: 0,
                byte_offset: 0,
                byte_length: 10,
                content_hash: "hash0".to_string(),
            },
            SectionIndex {
                seq: 1,
                heading: "アーキテクチャ".to_string(),
                level: 1,
                byte_offset: 10,
                byte_length: 20,
                content_hash: "hash1".to_string(),
            },
        ];

        write_doc(&h, &doc).unwrap();

        let back = read_doc(&h, "session-loop-verification")
            .unwrap()
            .expect("doc must exist");
        assert_eq!(back.id, "doc-1");
        assert_eq!(back.slug, "session-loop-verification");
        assert_eq!(back.title, "Session Loop Verification");
        assert_eq!(back.doc_type, "spec");
        assert_eq!(back.tags, doc.tags);
        assert_eq!(back.scope_paths, doc.scope_paths);
        assert_eq!(back.task_ids, doc.task_ids);
        assert_eq!(back.sections.len(), 2);
        assert_eq!(back.sections[1].heading, "アーキテクチャ");
        assert_eq!(back.sections[1].byte_offset, 10);
        assert_eq!(back.auto_inject, "auto");
        assert!(back.parent_id.is_none());
        assert!(back.has_bom, "has_bom must round-trip through write/read");
        assert_eq!(back.line_ending, "crlf");
        assert_eq!(
            back.source.frontmatter.as_deref(),
            Some("title: Foo\n"),
            "source.frontmatter must round-trip through write/read"
        );

        // File is named by slug, not by id.
        assert!(docs_dir(&h)
            .join("_doc.session-loop-verification.json")
            .exists());
    }

    #[test]
    fn read_doc_missing_is_none() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        assert!(read_doc(&h, "doc-nope").unwrap().is_none());
    }

    #[test]
    fn read_all_docs_missing_dir_is_empty() {
        let tmp = TempDir::new().unwrap();
        let h = tmp.path().join(".handoff");
        std::fs::create_dir_all(&h).unwrap();
        assert!(read_all_docs(&h).unwrap().is_empty());
    }

    #[test]
    fn read_all_docs_skips_corrupt_and_ignores_body_files() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_doc(&h, &sample_doc("doc-good", "doc-good")).unwrap();
        std::fs::write(docs_dir(&h).join("_doc.doc-bad.json"), b"{not json").unwrap();
        // A body file sitting alongside must never be mistaken for metadata.
        write_doc_body(&h, "doc-good", "body text").unwrap();

        let all = read_all_docs(&h).unwrap();
        assert_eq!(all.len(), 1, "corrupt doc skipped, body file ignored");
        assert_eq!(all[0].id, "doc-good");
    }

    /// Caution (found in review): a genuine v4 `_doc.*.json` file (no
    /// `slug` field — `slug` is new, required, in v5; `fragments` entries
    /// with no `byte_offset`/`byte_length`/`content_hash`) fails to
    /// deserialize as `DocMetadata` and is treated exactly like a corrupt
    /// file: skipped silently, with no warning. This is a deliberate,
    /// documented trade-off (wiki/130-document-management.md's migration
    /// section states no real v4 documents exist outside dev test data), not
    /// a graceful migration path — asserting it here so a regression toward
    /// "v4 docs silently vanish" is caught, and so the behavior stays
    /// documented in code rather than only in review notes.
    #[test]
    fn read_all_docs_silently_skips_real_v4_file_missing_slug() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_doc(&h, &sample_doc("doc-good", "doc-good")).unwrap();

        let real_v4_json = serde_json::json!({
            "version": 1,
            "id": "doc-v4",
            "title": "Old Spec",
            "doc_type": "spec",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "fragments": [
                { "seq": 0, "heading": "", "level": 0 }
            ],
        });
        std::fs::write(
            docs_dir(&h).join("_doc.old-spec.json"),
            serde_json::to_vec(&real_v4_json).unwrap(),
        )
        .unwrap();

        assert!(
            read_doc(&h, "old-spec").unwrap().is_none(),
            "a real v4 doc (no slug field) must not be readable under the v5 schema"
        );
        let all = read_all_docs(&h).unwrap();
        assert_eq!(
            all.len(),
            1,
            "the v4 doc must be silently skipped, not surfaced as an error or a warning"
        );
        assert_eq!(all[0].id, "doc-good");
    }

    #[test]
    fn find_doc_by_id_scans_all_docs() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_doc(&h, &sample_doc("doc-1", "human-readable-slug")).unwrap();

        let found = find_doc_by_id(&h, "doc-1")
            .unwrap()
            .expect("doc must be found by id");
        assert_eq!(found.slug, "human-readable-slug");

        assert!(find_doc_by_id(&h, "doc-nope").unwrap().is_none());
    }

    #[test]
    fn delete_doc_removes_file_and_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_doc(&h, &sample_doc("doc-1", "doc-1")).unwrap();
        assert!(delete_doc(&h, "doc-1").unwrap());
        assert!(read_doc(&h, "doc-1").unwrap().is_none());
        assert!(!delete_doc(&h, "doc-1").unwrap());
    }

    #[test]
    fn lazy_dir_creation_on_first_doc_write() {
        let tmp = TempDir::new().unwrap();
        let h = tmp.path().join(".handoff");
        std::fs::create_dir_all(&h).unwrap();
        assert!(!docs_dir(&h).exists());
        write_doc(&h, &sample_doc("doc-1", "doc-1")).unwrap();
        assert!(docs_dir(&h).exists());
    }

    #[test]
    fn write_then_read_doc_body_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_doc_body(&h, "my-slug", "# Title\n\nBody text.\n").unwrap();

        let back = read_doc_body(&h, "my-slug").unwrap().expect("body exists");
        assert_eq!(back, "# Title\n\nBody text.\n");
        assert!(docs_dir(&h).join("_doc.my-slug.md").exists());
    }

    #[test]
    fn read_doc_body_missing_is_none() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        assert!(read_doc_body(&h, "nope").unwrap().is_none());
    }

    #[test]
    fn delete_doc_body_removes_file_and_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_doc_body(&h, "my-slug", "body").unwrap();
        assert!(delete_doc_body(&h, "my-slug").unwrap());
        assert!(read_doc_body(&h, "my-slug").unwrap().is_none());
        assert!(!delete_doc_body(&h, "my-slug").unwrap());
    }

    #[test]
    fn lazy_dir_creation_on_first_doc_body_write() {
        let tmp = TempDir::new().unwrap();
        let h = tmp.path().join(".handoff");
        std::fs::create_dir_all(&h).unwrap();
        assert!(!docs_dir(&h).exists());
        write_doc_body(&h, "my-slug", "body").unwrap();
        assert!(docs_dir(&h).exists());
    }

    #[test]
    fn batch_resolve_docs_resolves_doc_links_by_id() {
        use crate::storage::tasks::TaskLink;

        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_doc(&h, &sample_doc("doc-1", "doc-one")).unwrap();
        write_doc(&h, &sample_doc("doc-2", "doc-two")).unwrap();

        let links = vec![
            TaskLink {
                target: "doc-1".to_string(),
                link_type: "doc".to_string(),
                label: None,
            },
            TaskLink {
                target: "doc-2".to_string(),
                link_type: "doc".to_string(),
                label: None,
            },
        ];

        let resolved = batch_resolve_docs(&h, &links).unwrap();
        assert_eq!(resolved.len(), 2);
        let ids: Vec<&str> = resolved.iter().map(|d| d.id.as_str()).collect();
        assert!(ids.contains(&"doc-1"));
        assert!(ids.contains(&"doc-2"));
    }

    #[test]
    fn batch_resolve_docs_ignores_non_doc_link_types() {
        use crate::storage::tasks::TaskLink;

        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_doc(&h, &sample_doc("doc-1", "doc-one")).unwrap();

        let links = vec![
            TaskLink {
                target: "doc-1".to_string(),
                link_type: "doc".to_string(),
                label: None,
            },
            TaskLink {
                target: "https://example.com".to_string(),
                link_type: "url".to_string(),
                label: None,
            },
        ];

        let resolved = batch_resolve_docs(&h, &links).unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].id, "doc-1");
    }

    #[test]
    fn batch_resolve_docs_skips_dangling_links() {
        use crate::storage::tasks::TaskLink;

        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_doc(&h, &sample_doc("doc-1", "doc-one")).unwrap();

        let links = vec![
            TaskLink {
                target: "doc-1".to_string(),
                link_type: "doc".to_string(),
                label: None,
            },
            TaskLink {
                target: "doc-missing".to_string(),
                link_type: "doc".to_string(),
                label: None,
            },
        ];

        let resolved = batch_resolve_docs(&h, &links).unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].id, "doc-1");
    }

    #[test]
    fn batch_resolve_docs_empty_links_is_empty_without_reading_docs_dir() {
        let tmp = TempDir::new().unwrap();
        let h = tmp.path().join(".handoff");
        std::fs::create_dir_all(&h).unwrap();
        // docs/ dir does not exist at all — must not error.
        assert!(!docs_dir(&h).exists());
        assert!(batch_resolve_docs(&h, &[]).unwrap().is_empty());
    }
}
