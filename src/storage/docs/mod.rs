//! Document management: splitting a single authored Markdown body into
//! in-memory sections, and persisting documents to `.handoff/docs/` as a
//! single frontmatter+body Markdown file per document (frontmatter
//! migration, t123.1-t123.3 — supersedes the earlier 2-file
//! `_doc.<slug>.json` + `_doc.<slug>.md` pair, wiki/130-document-management.md
//! §3.1).
//!
//! Layout:
//!
//! ```text
//! .handoff/docs/
//!   _doc.<slug>.md     # YAML frontmatter (metadata) + full document body
//!   injected/
//!     <session-id>.json   # per-session "already injected" sidecar
//! ```
//!
//! `slug` is a human-readable, caller-supplied name (`[a-z0-9-]`, max
//! [`model::MAX_SLUG_LEN`] chars) used purely for file naming so `ls
//! .handoff/docs/` is self-describing. The stable `id` (timestamp-based)
//! stays inside the frontmatter for family-tree/task-link references;
//! [`find_doc_by_id`] resolves an `id` back to its document when the slug
//! isn't known by the caller.
//!
//! `sections[]` is never persisted — [`read_doc`]/[`read_all_docs`] always
//! recompute it fresh from the body via [`split::split`] +
//! [`split::compute_sections`], so a manual edit to the `.md` file can never
//! leave a stale byte-offset index on disk (t123.2). `content_hash` is
//! likewise recomputed on every read (not trusted from frontmatter) so drift
//! detection (`doc_reassemble`, verification staleness) still works after a
//! manual edit.
//!
//! **Migration**: a `_doc.<slug>.json` file next to `_doc.<slug>.md`
//! indicates the old 2-file format. [`read_doc`]/[`read_all_docs`]
//! transparently migrate it in place on first access (t123.3): the JSON
//! metadata is folded into a frontmatter block prepended to the `.md` body,
//! the `.json` file is deleted, and the migration is logged to stderr (this
//! is a stdio-based MCP server, so stdout must stay clean JSON-RPC-only).
//! Callers never need to know whether a document was migrated.
//!
//! See `wiki/130-document-management.md` §3-4 for the full storage
//! architecture and data model.
//!
//! All writes go through [`crate::storage::atomic_write`] and `docs/` is
//! created lazily on first write (mirrors `src/storage/memory/mod.rs`), so
//! projects created before this feature shipped are unaffected until they
//! first call `doc_save`.

pub mod frontmatter;
pub mod model;
pub mod reassemble;
pub mod split;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

pub use model::{
    CodeRef, DocMetadata, DocRelation, DocSource, SectionIndex, SubItem, Verification,
    VerificationItem,
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

/// Legacy JSON sidecar path (`_doc.<slug>.json`). Only used by the
/// migration path (t123.3) — new writes never create this file.
fn doc_meta_path(handoff_dir: &Path, slug: &str) -> PathBuf {
    docs_dir(handoff_dir).join(format!("_doc.{slug}.json"))
}

fn doc_body_path(handoff_dir: &Path, slug: &str) -> PathBuf {
    docs_dir(handoff_dir).join(format!("_doc.{slug}.md"))
}

/// Write a document's metadata as YAML frontmatter into `_doc.<slug>.md`,
/// atomically, creating `docs/` lazily. Preserves whatever body currently
/// exists on disk for this slug (callers that also change the body must
/// call [`write_doc_body`] first — this is `doc_save`'s existing write
/// order). A brand new document with no body on disk yet is written with an
/// empty body.
///
/// `doc.sections` is never persisted (t123.2) regardless of what it holds
/// in memory when this is called.
pub fn write_doc(handoff_dir: &Path, doc: &DocMetadata) -> Result<PathBuf> {
    ensure_docs_dir(handoff_dir)?;
    let path = doc_body_path(handoff_dir, &doc.slug);
    let body = read_doc_body(handoff_dir, &doc.slug)?.unwrap_or_default();
    frontmatter::write_frontmatter_doc(&path, doc, &body)?;
    Ok(path)
}

/// Write a document's full body to `_doc.<slug>.md` atomically, creating
/// `docs/` lazily. `body` is written exactly as given — no re-rendering —
/// so it can be read back byte-identical via [`read_doc_body`].
///
/// This preserves whatever frontmatter already exists on disk for this
/// slug (or writes no frontmatter at all for a brand-new file — the
/// subsequent [`write_doc`] call in `doc_save`'s write order fills it in).
/// Writing only the body without ever following up with [`write_doc`]
/// would leave a frontmatter-less `.md` file, which reads back as "no
/// frontmatter" (migration-signal territory) rather than a valid document —
/// callers must always pair this with a `write_doc` call.
pub fn write_doc_body(handoff_dir: &Path, slug: &str, body: &str) -> Result<PathBuf> {
    ensure_docs_dir(handoff_dir)?;
    let path = doc_body_path(handoff_dir, slug);
    let existing_doc = frontmatter::read_frontmatter_doc(&path, slug)?.map(|(doc, _)| doc);
    let content = match existing_doc {
        Some(doc) => {
            let fm_yaml = frontmatter::serialize_frontmatter(&doc)?;
            format!("---\n{fm_yaml}---\n{body}")
        }
        None => body.to_string(),
    };
    crate::storage::atomic_write(&path, content.as_bytes())
        .with_context(|| format!("Failed to write document body: {}", path.display()))?;
    Ok(path)
}

/// Read a document's full body from `_doc.<slug>.md` — the part *after* the
/// YAML frontmatter block. Returns `Ok(None)` when the file does not exist.
/// A file with no frontmatter (old-format body-only file, or a plain `.md`
/// dropped in by hand) returns its entire content as the body.
pub fn read_doc_body(handoff_dir: &Path, slug: &str) -> Result<Option<String>> {
    let path = doc_body_path(handoff_dir, slug);
    match frontmatter::read_frontmatter_doc(&path, slug) {
        Ok(Some((_, body))) => Ok(Some(body)),
        Ok(None) => match std::fs::read_to_string(&path) {
            Ok(content) => Ok(Some(content)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => {
                Err(e).with_context(|| format!("Failed to read document body: {}", path.display()))
            }
        },
        Err(e) => Err(e),
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

/// Migrates an old-format document (`_doc.<slug>.json` + `_doc.<slug>.md`
/// body-only file) to the new single-file frontmatter format, in place
/// (t123.3): reads the JSON metadata, reads the existing (frontmatter-less)
/// body, writes a new `_doc.<slug>.md` with the metadata folded into a
/// YAML frontmatter block prepended to that body, then deletes the `.json`
/// sidecar. Logs the migration to stderr. Returns the migrated
/// [`DocMetadata`] (with `sections` still empty — the caller computes those
/// fresh, same as any other read).
fn migrate_legacy_doc(handoff_dir: &Path, slug: &str) -> Result<DocMetadata> {
    let json_path = doc_meta_path(handoff_dir, slug);
    let json_content = std::fs::read_to_string(&json_path).with_context(|| {
        format!(
            "Failed to read legacy document metadata: {}",
            json_path.display()
        )
    })?;
    let mut doc: DocMetadata = serde_json::from_str(&json_content).with_context(|| {
        format!(
            "Failed to parse legacy document metadata: {}",
            json_path.display()
        )
    })?;
    // sections/version are storage-layer bookkeeping the frontmatter format
    // no longer persists (t123.2) — clear here so the migrated file starts
    // clean, matching what any other `write_doc` call would produce.
    doc.sections = Vec::new();

    let body_path = doc_body_path(handoff_dir, slug);
    let body = std::fs::read_to_string(&body_path).with_context(|| {
        format!(
            "Failed to read legacy document body: {}",
            body_path.display()
        )
    })?;

    frontmatter::write_frontmatter_doc(&body_path, &doc, &body)?;
    std::fs::remove_file(&json_path).with_context(|| {
        format!(
            "Failed to delete legacy document metadata after migration: {}",
            json_path.display()
        )
    })?;

    eprintln!(
        "handoff-mcp: migrated document '{slug}' (id={}) from JSON+MD sidecar format to \
         frontmatter MD",
        doc.id
    );

    Ok(doc)
}

/// Read one document by exact slug: parses YAML frontmatter from
/// `_doc.<slug>.md`, transparently migrating an old-format
/// `_doc.<slug>.json` + `_doc.<slug>.md` pair in place first if that's what
/// is on disk (t123.3). Always recomputes `sections[]` fresh from the body
/// (t123.2) and `content_hash` from the body's current bytes (drift
/// detection stays correct after a manual edit) before returning.
///
/// Returns `Ok(None)` when:
/// - neither `_doc.<slug>.md` nor `_doc.<slug>.json` exists, or
/// - `_doc.<slug>.md` exists with no frontmatter and no `.json` sidecar
///   (a body-only leftover from a partially-completed migration, or a
///   plain `.md` file dropped in by hand — logged as a warning, not
///   silently ignored, since it's ambiguous whether this was ever meant to
///   be a handoff document).
///
/// Returns `Err` when a `.md` file has a `---` fence but the enclosed YAML
/// fails to parse (corrupt frontmatter — a genuine error, not a migration
/// signal), or when a legacy JSON sidecar exists but fails to parse/migrate.
pub fn read_doc(handoff_dir: &Path, slug: &str) -> Result<Option<DocMetadata>> {
    let body_path = doc_body_path(handoff_dir, slug);
    let json_path = doc_meta_path(handoff_dir, slug);

    let parsed = frontmatter::read_frontmatter_doc(&body_path, slug)?;
    let (mut doc, body) = match parsed {
        Some((doc, body)) => (doc, body),
        None => {
            if !body_path.exists() {
                return Ok(None);
            }
            if !json_path.exists() {
                eprintln!(
                    "handoff-mcp: document body file '{}' has no YAML frontmatter and no \
                     legacy JSON sidecar to migrate from — skipping",
                    body_path.display()
                );
                return Ok(None);
            }
            let migrated = migrate_legacy_doc(handoff_dir, slug)?;
            let body = read_doc_body(handoff_dir, slug)?.unwrap_or_default();
            (migrated, body)
        }
    };

    recompute_sections_and_hash(&mut doc, &body);
    Ok(Some(doc))
}

/// Recomputes `doc.sections` and `doc.content_hash` from `body` (t123.2):
/// sections are never trusted from frontmatter (always empty there), and
/// content_hash is recomputed rather than trusted so drift detection
/// (`doc_reassemble`, verification staleness) reflects the body's actual
/// current bytes even after a manual out-of-band edit.
fn recompute_sections_and_hash(doc: &mut DocMetadata, body: &str) {
    if let Ok(split_doc) = split::split(body, doc.split_level) {
        doc.sections = split::compute_sections(&split_doc);
    }
    doc.content_hash = lexsim::content_hash(body);
}

/// Read every document in `docs/`: every `_doc.*.md` file (parsed via
/// [`read_doc`], which transparently migrates any paired legacy `.json`
/// sidecar first — t123.3). The `injected/` subdirectory is ignored. A
/// `.md` file that fails to parse under [`read_doc`] (corrupt frontmatter,
/// or a body-only leftover with no `.json` to migrate from) is skipped
/// silently/with a warning respectively, same policy as [`read_doc`] itself
/// applies per-file. Returns an empty vec when `docs/` does not exist
/// (uninitialized / feature-untouched projects).
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
        if !name.starts_with("_doc.") || !name.ends_with(".md") {
            continue;
        }
        let Some(slug) = name
            .strip_prefix("_doc.")
            .and_then(|s| s.strip_suffix(".md"))
        else {
            continue;
        };
        match read_doc(handoff_dir, slug) {
            Ok(Some(doc)) => docs.push(doc),
            Ok(None) => {}
            // Corrupt frontmatter / failed migration: skip silently
            // (lenient read, mirrors memory) rather than failing the whole
            // listing over one bad file.
            Err(_) => {}
        }
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

/// Delete a document's metadata by exact slug. In the single-file
/// frontmatter format, metadata and body live in the same
/// `_doc.<slug>.md` file, so this is equivalent to [`delete_doc_body`] —
/// kept as a separate function (rather than folding callers onto one) so
/// existing call sites that call both (`doc_delete`'s "delete body, then
/// delete metadata" order) keep working unchanged: the second call is a
/// no-op `Ok(false)` once the first has removed the file. Also removes a
/// leftover legacy `_doc.<slug>.json` sidecar, if one still exists
/// (e.g. a document deleted mid-migration). Returns `Ok(false)` when
/// neither file existed.
pub fn delete_doc(handoff_dir: &Path, slug: &str) -> Result<bool> {
    let md_path = doc_body_path(handoff_dir, slug);
    let json_path = doc_meta_path(handoff_dir, slug);
    let mut deleted = false;
    if md_path.exists() {
        std::fs::remove_file(&md_path)
            .with_context(|| format!("Failed to delete document: {}", md_path.display()))?;
        deleted = true;
    }
    if json_path.exists() {
        std::fs::remove_file(&json_path).with_context(|| {
            format!(
                "Failed to delete legacy document metadata: {}",
                json_path.display()
            )
        })?;
        deleted = true;
    }
    Ok(deleted)
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

        // v6 (frontmatter migration): sections are never persisted — they
        // are recomputed on every read from the body written via
        // `write_doc_body`, so a real body with a heading is needed here to
        // exercise that recomputation instead of hand-setting `sections`.
        let body = "Preamble.\r\n\r\n## アーキテクチャ\r\nSection body.\r\n";
        write_doc_body(&h, "session-loop-verification", body).unwrap();
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
        assert_eq!(
            back.sections.len(),
            2,
            "sections recomputed on read: {:?}",
            back.sections
        );
        assert_eq!(back.sections[1].heading, "アーキテクチャ");
        assert_eq!(back.auto_inject, "auto");
        assert!(back.parent_id.is_none());
        assert!(back.has_bom, "has_bom must round-trip through write/read");
        assert_eq!(back.line_ending, "crlf");

        // Exactly one file on disk for this document (single-file
        // frontmatter format — no JSON sidecar).
        assert!(docs_dir(&h)
            .join("_doc.session-loop-verification.md")
            .exists());
        assert!(!docs_dir(&h)
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
        // A lone legacy `.json` sidecar with no paired `.md` body is not a
        // migratable document (read_all_docs only iterates `.md` files) —
        // it must simply be ignored, not crash the scan.
        std::fs::write(docs_dir(&h).join("_doc.doc-bad.json"), b"{not json").unwrap();

        let all = read_all_docs(&h).unwrap();
        assert_eq!(all.len(), 1, "lone json-only file ignored");
        assert_eq!(all[0].id, "doc-good");
    }

    /// A lone legacy `_doc.*.json` file with no paired `_doc.*.md` body
    /// cannot be migrated (t123.3's migration reads both halves) — it is
    /// simply invisible to `read_all_docs`/`read_doc`, same as any other
    /// non-`.md` file in `docs/`. This is distinct from the "real" migration
    /// path exercised by [`read_doc_migrates_legacy_json_md_pair_in_place`],
    /// which requires both files to be present.
    #[test]
    fn read_all_docs_ignores_lone_legacy_json_with_no_paired_md() {
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
            "a lone json sidecar with no paired .md body is not readable/migratable"
        );
        let all = read_all_docs(&h).unwrap();
        assert_eq!(
            all.len(),
            1,
            "the lone json file must be silently skipped, not surfaced as an error or a warning"
        );
        assert_eq!(all[0].id, "doc-good");
    }

    /// t123.3: a genuine old-format `_doc.<slug>.json` + `_doc.<slug>.md`
    /// pair is transparently migrated in place on first `read_doc` access —
    /// the JSON metadata is folded into a YAML frontmatter block prepended
    /// to the existing body, and the `.json` sidecar is deleted.
    #[test]
    fn read_doc_migrates_legacy_json_md_pair_in_place() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        ensure_docs_dir(&h).unwrap();
        let slug = "legacy-doc";
        let legacy_json = serde_json::json!({
            "version": 2,
            "id": "doc-legacy-1",
            "slug": slug,
            "title": "Legacy Doc",
            "doc_type": "spec",
            "tags": ["old-format"],
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-02T00:00:00Z",
            "content_hash": "stale-hash-will-be-recomputed",
        });
        std::fs::write(
            docs_dir(&h).join(format!("_doc.{slug}.json")),
            serde_json::to_vec(&legacy_json).unwrap(),
        )
        .unwrap();
        let body = "# Legacy Doc\n\n## Old Section\n\nBody text.\n";
        std::fs::write(docs_dir(&h).join(format!("_doc.{slug}.md")), body).unwrap();

        let migrated = read_doc(&h, slug).unwrap().expect("must migrate and read");
        assert_eq!(migrated.id, "doc-legacy-1");
        assert_eq!(migrated.title, "Legacy Doc");
        assert_eq!(migrated.tags, vec!["old-format".to_string()]);
        assert_eq!(
            migrated.sections.len(),
            3,
            "sections recomputed fresh from body post-migration (seq0 preamble + H1 + H2): {:?}",
            migrated.sections
        );
        assert_eq!(migrated.content_hash, lexsim::content_hash(body));

        // The .json sidecar must be gone; the .md file must now carry
        // frontmatter (starts with "---\n").
        assert!(!docs_dir(&h).join(format!("_doc.{slug}.json")).exists());
        let new_content =
            std::fs::read_to_string(docs_dir(&h).join(format!("_doc.{slug}.md"))).unwrap();
        assert!(new_content.starts_with("---\n"));

        // Re-reading must be stable (idempotent) and not re-migrate.
        let reread = read_doc(&h, slug).unwrap().expect("must still read");
        assert_eq!(reread.id, migrated.id);
        assert_eq!(reread.sections.len(), 3);
    }

    /// The migration path is also exercised transparently through
    /// `read_all_docs`, so a directory with a mix of already-migrated and
    /// legacy documents surfaces every document once, in the new format.
    #[test]
    fn read_all_docs_migrates_legacy_pairs_transparently() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_doc(&h, &sample_doc("doc-new", "already-new")).unwrap();
        write_doc_body(&h, "already-new", "# New\n\nBody.\n").unwrap();

        let legacy_json = serde_json::json!({
            "version": 2,
            "id": "doc-legacy-2",
            "slug": "legacy-two",
            "title": "Legacy Two",
            "doc_type": "note",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
        });
        std::fs::write(
            docs_dir(&h).join("_doc.legacy-two.json"),
            serde_json::to_vec(&legacy_json).unwrap(),
        )
        .unwrap();
        std::fs::write(
            docs_dir(&h).join("_doc.legacy-two.md"),
            "# Legacy Two\n\nBody.\n",
        )
        .unwrap();

        let all = read_all_docs(&h).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|d| d.id == "doc-new"));
        assert!(all.iter().any(|d| d.id == "doc-legacy-2"));
        assert!(!docs_dir(&h).join("_doc.legacy-two.json").exists());
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
