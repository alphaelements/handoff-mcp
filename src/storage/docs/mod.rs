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
//! leave a stale byte-offset index on disk (t123.2). `content_hash` is never
//! trusted from frontmatter either, but (P-M1, wiki/240-performance-design.md
//! §4, t370.8) it is only *actually* recomputed by [`read_doc_hashed`] /
//! [`read_doc_with_body_hashed`] / [`read_all_docs_hashed`] — the plain
//! [`read_doc`]/[`read_all_docs`] leave it (and every section's
//! `content_hash`) as `None`, skipping the `lexsim::content_hash` pass
//! entirely for callers (e.g. `DocSet`-based task-link/dev_stage
//! propagation) that never look at it.
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

pub mod docset;
pub mod frontmatter;
pub mod layer;
pub mod model;
pub mod reassemble;
pub mod split;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{bail, Context, Result};

pub use docset::{load_mutate_flush_with_retry, DocSet, DocSetConflict};
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
///
/// Explicitly evicts this document's entry from the P-M1 process read cache
/// (see [`DocCacheStamp`]) after writing — a metadata-only rewrite (tags,
/// task_ids, verification, ...) must never be served stale from a prior
/// `read_doc` call.
pub fn write_doc(handoff_dir: &Path, doc: &DocMetadata) -> Result<PathBuf> {
    let body = read_doc_body(handoff_dir, &doc.slug)?.unwrap_or_default();
    write_doc_with_body(handoff_dir, doc, &body)
}

/// Writes a document's metadata (as YAML frontmatter) and body together in a
/// single atomic write, using `body` exactly as given rather than re-reading
/// the current on-disk body first (contrast [`write_doc`], which is for the
/// metadata-only-change case and preserves whatever body is already on disk
/// by reading it back before writing).
///
/// Callers that already hold the document's new body in memory (e.g.
/// `handle_doc_update_section`, which just spliced it) should call this
/// directly instead of `write_doc_body` + `write_doc` — that pair reads the
/// just-written body back off disk and writes the file a second time
/// (P-M3, wiki/240-performance-design.md §4 C7: "同じファイルを2回書き2回
/// fsync"); this collapses both into the one atomic write the file actually
/// needs.
pub fn write_doc_with_body(handoff_dir: &Path, doc: &DocMetadata, body: &str) -> Result<PathBuf> {
    ensure_docs_dir(handoff_dir)?;
    let path = doc_body_path(handoff_dir, &doc.slug);
    // The on-disk frontmatter's `content_hash` field is always a real,
    // present string (`frontmatter::serialize_frontmatter` refuses to write
    // otherwise) — a caller that resolved `doc` through a lazy read (P-M1,
    // t370.8) and never changed the body has `doc.content_hash == None`
    // here, so it must be computed against the exact `body` being written
    // now. Callers that already computed it (e.g. `doc_save`,
    // `handle_doc_update_section`, both of which just hashed the new body
    // themselves) pay no extra cost — this only clones+hashes when missing.
    if doc.content_hash.is_some() {
        frontmatter::write_frontmatter_doc(&path, doc, body)?;
    } else {
        let mut doc_with_hash = doc.clone();
        doc_with_hash.content_hash = Some(lexsim::content_hash(body));
        frontmatter::write_frontmatter_doc(&path, &doc_with_hash, body)?;
    }
    invalidate_doc_cache(&path);
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
    // See write_doc's doc comment: explicit eviction, not just relying on
    // the (len, mtime_ns) stamp changing (P-M1).
    invalidate_doc_cache(&path);
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
    invalidate_doc_cache(&path);
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

    // `write_doc_with_body` (rather than `frontmatter::write_frontmatter_doc`
    // directly) so a legacy JSON sidecar with no `content_hash` key at all
    // (deserializes to `None` via `#[serde(default)]`) still gets a real
    // hash computed against `body` before it reaches disk — P-M1, t370.8:
    // the on-disk field is never left empty/absent.
    write_doc_with_body(handoff_dir, &doc, &body)?;
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

// -- P-M1 process-wide read cache (wiki/240-performance-design.md §4) --
//
// `read_all_docs` (via `read_doc`) is the hot path behind nearly every
// document-touching MCP tool, itself called up to 4x per request (stable_id
// resolution, `find_doc_by_id`'s full-scan fallback, summary regeneration —
// wiki/240 §1) and again on every later request the same long-running
// server process handles. Re-parsing YAML frontmatter and recomputing the
// whole-body + per-section `content_hash` (88-99% of `read_all_docs`'s cost
// per the wiki §1/§3 C1 measurement) on every single call is wasted work
// once a document's bytes stop changing between calls — this cache skips
// the whole parse+hash pass on a hit. Precedent: `context::doc_corpus_cache`
// (`src/context/mod.rs`), same generation-free "hit iff nothing changed"
// shape, keyed here by filesystem stamp rather than a mutation counter
// because documents are read across many independent MCP tool calls, not
// just within one corpus-building pass.

/// Filesystem stamp used to validate a cached [`DocMetadata`] parse without
/// re-reading the file's contents. `(len, mtime_ns)` — the cache key wiki/240
/// §4 P-M1 specifies. A colliding stamp after a genuine content change is
/// not a realistic risk for a real edit (nanosecond mtime resolution on the
/// filesystems this server targets), but the write paths below
/// (`write_doc`, `write_doc_body`, `delete_doc`, `delete_doc_body`,
/// `migrate_legacy_doc`) additionally invalidate their own cache entry
/// explicitly rather than relying on the stamp changing: a same-process
/// write immediately followed by a read must never observe a stale entry,
/// even in the pathological case of a same-length rewrite landing on an
/// identical mtime (coarse-mtime filesystem, or two writes within the same
/// tick).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DocCacheStamp {
    len: u64,
    mtime_ns: u128,
}

fn doc_cache_stamp(path: &Path) -> Option<DocCacheStamp> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime_ns = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(DocCacheStamp {
        len: meta.len(),
        mtime_ns,
    })
}

static DOC_READ_CACHE: OnceLock<Mutex<HashMap<PathBuf, (DocCacheStamp, DocMetadata)>>> =
    OnceLock::new();

fn doc_read_cache() -> &'static Mutex<HashMap<PathBuf, (DocCacheStamp, DocMetadata)>> {
    DOC_READ_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Returns a clone of the cached [`DocMetadata`] for `path` iff its cached
/// stamp still matches `stamp` (the file's current `(len, mtime_ns)`) —
/// otherwise `None`, meaning the caller must re-parse from disk.
fn cached_doc(path: &Path, stamp: DocCacheStamp) -> Option<DocMetadata> {
    let cache = doc_read_cache().lock().expect("doc read cache poisoned");
    cache
        .get(path)
        .filter(|(cached_stamp, _)| *cached_stamp == stamp)
        .map(|(_, doc)| doc.clone())
}

fn cache_doc(path: PathBuf, stamp: DocCacheStamp, doc: DocMetadata) {
    doc_read_cache()
        .lock()
        .expect("doc read cache poisoned")
        .insert(path, (stamp, doc));
}

/// Explicitly evicts `path` from the process-wide doc cache. Called by
/// every write/delete path immediately after the filesystem mutation (see
/// [`DocCacheStamp`]'s doc comment for why this can't rely solely on the
/// stamp changing).
fn invalidate_doc_cache(path: &Path) {
    doc_read_cache()
        .lock()
        .expect("doc read cache poisoned")
        .remove(path);
}

/// Test-only inspection hook (mirrors `context::CorpusCache::generation`'s
/// role) — lets tests assert the cache was actually populated/evicted
/// rather than only observing the (identical either way) returned value.
#[cfg(test)]
fn doc_read_cache_contains(path: &Path) -> bool {
    doc_read_cache()
        .lock()
        .expect("doc read cache poisoned")
        .contains_key(path)
}

/// Read one document by exact slug: parses YAML frontmatter from
/// `_doc.<slug>.md`, transparently migrating an old-format
/// `_doc.<slug>.json` + `_doc.<slug>.md` pair in place first if that's what
/// is on disk (t123.3). Always recomputes `sections[]` fresh from the body
/// (t123.2) and `content_hash` from the body's current bytes (drift
/// detection stays correct after a manual edit) before returning — unless a
/// process-cached parse for this exact file `(len, mtime_ns)` is available,
/// in which case that (identically-valued) result is served instead of
/// redoing the parse/hash work (P-M1, wiki/240-performance-design.md §4).
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
    read_doc_impl(handoff_dir, slug, false)
}

/// Like [`read_doc`], but guarantees `doc.content_hash` and every section's
/// `content_hash` are computed (`Some`) rather than left `None` (P-M1,
/// wiki/240-performance-design.md §4, t370.8) — for callers that actually
/// need a trustworthy hash: staleness/drift checks, `doc_get` output,
/// `doc_query`'s injection-suppression tracking.
pub fn read_doc_hashed(handoff_dir: &Path, slug: &str) -> Result<Option<DocMetadata>> {
    read_doc_impl(handoff_dir, slug, true)
}

/// Shared implementation behind [`read_doc`]/[`read_doc_hashed`].
/// `need_hash = false` skips the `lexsim::content_hash` pass entirely
/// (frontmatter parse + section byte-offsets only); `need_hash = true`
/// reproduces `read_doc`'s pre-t370.8 behavior exactly. A process-cache hit
/// whose cached entry doesn't yet carry a hash the caller needs falls
/// through to a full re-parse (so the cache never *downgrades* a caller's
/// request), and the richer (hashed) result then overwrites the cached
/// entry — a later lazy caller for the same stamp gets the hash for free.
fn read_doc_impl(handoff_dir: &Path, slug: &str, need_hash: bool) -> Result<Option<DocMetadata>> {
    let body_path = doc_body_path(handoff_dir, slug);
    let json_path = doc_meta_path(handoff_dir, slug);

    // The stamp is taken once, *before* reading the file, and that same
    // pre-read stamp is what the parsed result is cached under below. If the
    // file is replaced (e.g. by another worktree's server sharing this
    // `.handoff/`) between this stat and the read/parse/hash, the cached
    // entry carries the *old* stamp, so the next lookup sees a mismatch and
    // re-parses — at worst a spurious miss. Stamping after the parse instead
    // would cache the old content under the *new* stamp, serving it stale
    // until the file happens to change again.
    let pre_read_stamp = doc_cache_stamp(&body_path);
    if let Some(stamp) = pre_read_stamp {
        if let Some(doc) = cached_doc(&body_path, stamp) {
            if !need_hash || doc.content_hash.is_some() {
                return Ok(Some(doc));
            }
            // Cached, but without the hash this caller needs — fall through
            // to a full re-parse+hash below rather than serving a `None`.
        }
    }

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

    recompute_sections_and_hash(&mut doc, &body, need_hash);

    if let Some(stamp) = pre_read_stamp {
        cache_doc(body_path, stamp, doc.clone());
    }

    Ok(Some(doc))
}

/// Reads one document's metadata *and* body from a single consistent
/// snapshot — unlike calling [`read_doc`] and [`read_doc_body`] separately,
/// which are two independent reads of `_doc.<slug>.md` that can straddle a
/// concurrent writer (e.g. another worktree's server sharing this
/// `.handoff/`, the same scenario the P-M1 cache docs above call out).
///
/// This is what [`crate::mcp::handlers::docs::handle_doc_update_section`]
/// needs (review round 2 MAJOR fix, wiki/240-performance-design.md §4 P-M3):
/// it byte-slices `body` using the returned `DocMetadata.sections`' offsets,
/// so those offsets and that body must always come from the exact same
/// bytes — a metadata read at one instant paired with a body read at a
/// later, possibly-different instant could desync `sections` from `body`,
/// which risks a byte-boundary panic on the slice, a wrong splice, or an
/// `expected_hash` optimistic-lock check validated against a snapshot other
/// than the one actually being overwritten.
///
/// `frontmatter::read_frontmatter_doc` does one `std::fs::read_to_string`
/// and derives both the metadata and the body from that single string, so
/// `doc`/`body` below are always mutually consistent by construction. The
/// only thing *not* guaranteed by construction is whether a process-cached
/// `sections`/`content_hash` (computed by some earlier call) still describes
/// *this* read — that's only trusted when the file's `(len, mtime_ns)` stamp
/// is unchanged from immediately before this read to immediately after,
/// i.e. no writer could have landed mid-read.
pub fn read_doc_with_body(handoff_dir: &Path, slug: &str) -> Result<Option<(DocMetadata, String)>> {
    read_doc_with_body_impl(handoff_dir, slug, false)
}

/// Like [`read_doc_with_body`], but guarantees `doc.content_hash` and every
/// section's `content_hash` are computed (`Some`) — see [`read_doc_hashed`]
/// (P-M1, wiki/240-performance-design.md §4, t370.8).
pub fn read_doc_with_body_hashed(
    handoff_dir: &Path,
    slug: &str,
) -> Result<Option<(DocMetadata, String)>> {
    read_doc_with_body_impl(handoff_dir, slug, true)
}

fn read_doc_with_body_impl(
    handoff_dir: &Path,
    slug: &str,
    need_hash: bool,
) -> Result<Option<(DocMetadata, String)>> {
    let body_path = doc_body_path(handoff_dir, slug);
    let json_path = doc_meta_path(handoff_dir, slug);

    let pre_read_stamp = doc_cache_stamp(&body_path);

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

    let post_read_stamp = doc_cache_stamp(&body_path);
    let stamp_stable_across_read =
        matches!((pre_read_stamp, post_read_stamp), (Some(a), Some(b)) if a == b);

    if stamp_stable_across_read {
        let stamp = pre_read_stamp.expect("checked by stamp_stable_across_read");
        if let Some(cached) = cached_doc(&body_path, stamp) {
            if !need_hash || cached.content_hash.is_some() {
                return Ok(Some((cached, body)));
            }
            // Cached, but without the hash this caller needs — fall through
            // to a full recompute below rather than serving a `None`.
        }
    }

    recompute_sections_and_hash(&mut doc, &body, need_hash);

    if stamp_stable_across_read {
        let stamp = pre_read_stamp.expect("checked by stamp_stable_across_read");
        cache_doc(body_path, stamp, doc.clone());
    }

    Ok(Some((doc, body)))
}

/// Recomputes `doc.sections` from `body` (t123.2): sections are never
/// trusted from frontmatter (always empty there). When `compute_hash` is
/// `true`, also recomputes `doc.content_hash` (and every section's
/// `content_hash`) from `body`'s actual current bytes rather than trusting
/// whatever was on disk — so drift detection (`doc_reassemble`,
/// verification staleness) reflects reality even after a manual out-of-band
/// edit. When `false` (P-M1, wiki/240-performance-design.md §4, t370.8),
/// `content_hash` is left `None` on both `doc` and every section — the
/// `lexsim::content_hash` pass is skipped entirely for callers that don't
/// need it (see [`DocMetadata::content_hash`]'s doc comment).
fn recompute_sections_and_hash(doc: &mut DocMetadata, body: &str, compute_hash: bool) {
    if let Ok(split_doc) = split::split(body, doc.split_level) {
        doc.sections = split::compute_sections(&split_doc, compute_hash);
    }
    doc.content_hash = compute_hash.then(|| lexsim::content_hash(body));
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
    read_all_docs_impl(handoff_dir, false)
}

/// Like [`read_all_docs`], but guarantees every returned document's (and its
/// sections') `content_hash` is computed (`Some`) — for corpus-wide
/// consumers that genuinely need it (e.g. `doc_query`'s injection-
/// suppression tracking, `task_checklist`'s staleness detection via
/// `batch_resolve_docs`). Pays the full `lexsim::content_hash` cost per
/// document, same as `read_all_docs` did before t370.8; callers that only
/// need frontmatter/section structure (e.g. `DocSet`-based task-link/
/// dev_stage propagation) should keep using the plain [`read_all_docs`]
/// (P-M1, wiki/240-performance-design.md §4).
pub fn read_all_docs_hashed(handoff_dir: &Path) -> Result<Vec<DocMetadata>> {
    read_all_docs_impl(handoff_dir, true)
}

fn read_all_docs_impl(handoff_dir: &Path, need_hash: bool) -> Result<Vec<DocMetadata>> {
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
        match read_doc_impl(handoff_dir, slug, need_hash) {
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

// -- P-M2 process-wide id -> slug index (wiki/240-performance-design.md §4) --
//
// `find_doc_by_id` used to scan+parse every document on every single call —
// a full `read_all_docs` pass just to resolve one stable `id` to its
// file-naming `slug`. Once the P-M1 read cache above is warm the *parse* per
// file is cheap, but the *scan itself* (iterating every `_doc.*.md` entry,
// one cache-stamp check per file) still costs O(doc count) per lookup, and
// `link_requirements_to_task` / `unlink_requirements_from_task` /
// `propagate_dev_stage_for_task` each did this once per distinct document
// touched (wiki/240 §3 C2). This index remembers `id -> slug` per
// `handoff_dir` (a single long-running server process can serve more than
// one project directory across its lifetime — a global id->slug map with no
// directory key would let one project's index entry resolve a same-valued
// `id` in a *different* project's `docs/`) so any lookup for an id already
// seen for that directory is O(1) plus one direct slug-keyed `read_doc`
// (itself normally a P-M1 cache hit) instead of another full scan.
static DOC_ID_INDEX: OnceLock<Mutex<HashMap<PathBuf, HashMap<String, String>>>> = OnceLock::new();

fn doc_id_index() -> &'static Mutex<HashMap<PathBuf, HashMap<String, String>>> {
    DOC_ID_INDEX.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Test-only per-directory counter of how many times [`find_doc_by_id`] fell
/// all the way through to a full-corpus rebuild for that directory — lets
/// tests assert that a second lookup for an id already seen is served from
/// the index rather than re-scanning. Keyed by directory (not a single
/// global counter) so it stays accurate under `cargo test`'s default
/// multi-threaded, shared-process execution, where unrelated tests rebuild
/// the index for their own unrelated temp directories concurrently.
#[cfg(test)]
static DOC_ID_INDEX_REBUILD_COUNTS: OnceLock<Mutex<HashMap<PathBuf, usize>>> = OnceLock::new();

#[cfg(test)]
fn doc_id_index_rebuild_count(handoff_dir: &Path) -> usize {
    DOC_ID_INDEX_REBUILD_COUNTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("doc id index rebuild counts poisoned")
        .get(handoff_dir)
        .copied()
        .unwrap_or(0)
}

/// Re-scans every document in `handoff_dir` (the same pass `read_all_docs`
/// already needs) and rebuilds that directory's id -> slug index entry from
/// scratch, returning the listing so the one caller that needs both
/// ([`find_doc_by_id`]'s miss/self-heal path) doesn't pay for a second full
/// scan. Other directories' index entries are untouched.
fn rebuild_doc_id_index(handoff_dir: &Path) -> Result<Vec<DocMetadata>> {
    #[cfg(test)]
    {
        let mut counts = DOC_ID_INDEX_REBUILD_COUNTS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .expect("doc id index rebuild counts poisoned");
        *counts.entry(handoff_dir.to_path_buf()).or_insert(0) += 1;
    }

    let docs = read_all_docs(handoff_dir)?;
    let mut by_dir = doc_id_index().lock().expect("doc id index poisoned");
    let index = by_dir.entry(handoff_dir.to_path_buf()).or_default();
    index.clear();
    for doc in &docs {
        index.insert(doc.id.clone(), doc.slug.clone());
    }
    Ok(docs)
}

/// Find a document by its stable `id` (not its file-naming `slug`). Used for
/// backward-compat lookups where a caller only has the `id` (e.g.
/// family-tree `parent_id`/`related[].id`, task-link reverse lookups) and
/// not the slug. Returns `Ok(None)` if no document with that `id` exists.
///
/// Resolves via the process-wide id->slug index (P-M2) when possible: an
/// index hit is verified by re-reading the candidate slug and confirming its
/// `id` still matches (protects against a stale entry — e.g. the document
/// was deleted and the slug reused — without trusting the index blindly). A
/// miss or a stale hit triggers [`rebuild_doc_id_index`], a full scan that
/// also repopulates the index for every other id in one pass, so only the
/// first lookup for a given id (or the first lookup after a doc is deleted
/// or newly created) pays the full-scan cost.
pub fn find_doc_by_id(handoff_dir: &Path, doc_id: &str) -> Result<Option<DocMetadata>> {
    let cached_slug = doc_id_index()
        .lock()
        .expect("doc id index poisoned")
        .get(handoff_dir)
        .and_then(|index| index.get(doc_id))
        .cloned();
    if let Some(slug) = cached_slug {
        if let Some(doc) = read_doc(handoff_dir, &slug)? {
            if doc.id == doc_id {
                return Ok(Some(doc));
            }
        }
        // Stale entry — fall through to a full rebuild below.
    }
    let docs = rebuild_doc_id_index(handoff_dir)?;
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

    // Hashed: `task_checklist`'s `view` action feeds these into
    // `item_is_stale`, which needs a trustworthy per-section `content_hash`
    // to detect drift (P-M1, wiki/240-performance-design.md §4, t370.8).
    let all_docs = read_all_docs_hashed(handoff_dir)?;
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
        invalidate_doc_cache(&md_path);
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

        let migrated = read_doc_hashed(&h, slug)
            .unwrap()
            .expect("must migrate and read");
        assert_eq!(migrated.id, "doc-legacy-1");
        assert_eq!(migrated.title, "Legacy Doc");
        assert_eq!(migrated.tags, vec!["old-format".to_string()]);
        assert_eq!(
            migrated.sections.len(),
            3,
            "sections recomputed fresh from body post-migration (seq0 preamble + H1 + H2): {:?}",
            migrated.sections
        );
        assert_eq!(
            migrated.content_hash.as_deref(),
            Some(lexsim::content_hash(body).as_str())
        );

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

    /// P-M2 (wiki/240 §4): a second `find_doc_by_id` lookup for an id already
    /// resolved must be served from the process-wide id->slug index — not by
    /// re-scanning the whole `docs/` directory again.
    #[test]
    fn find_doc_by_id_second_lookup_for_same_id_does_not_rescan() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_doc(&h, &sample_doc("doc-cache-1", "cache-slug-one")).unwrap();
        write_doc(&h, &sample_doc("doc-cache-2", "cache-slug-two")).unwrap();

        assert_eq!(doc_id_index_rebuild_count(&h), 0, "no lookups yet");

        let first = find_doc_by_id(&h, "doc-cache-1").unwrap().unwrap();
        assert_eq!(first.slug, "cache-slug-one");
        let after_first = doc_id_index_rebuild_count(&h);
        assert_eq!(
            after_first, 1,
            "first lookup for an unseen id must rebuild once"
        );

        // A second lookup for a *different* id already captured by the same
        // rebuild (doc-cache-2) must be an index hit too, not a second scan.
        let second = find_doc_by_id(&h, "doc-cache-2").unwrap().unwrap();
        assert_eq!(second.slug, "cache-slug-two");
        assert_eq!(
            doc_id_index_rebuild_count(&h),
            after_first,
            "doc-cache-2 was already indexed by the first rebuild — must not rescan"
        );

        // Repeating the very same lookup again must also stay an index hit.
        let _ = find_doc_by_id(&h, "doc-cache-1").unwrap().unwrap();
        assert_eq!(
            doc_id_index_rebuild_count(&h),
            after_first,
            "repeat lookup of an already-indexed id must not rescan"
        );
    }

    /// A stale index entry (slug now holds a *different* document, e.g. the
    /// original was deleted and the slug reused) must not be trusted: the
    /// verifying re-read detects the id mismatch and falls back to a rebuild.
    #[test]
    fn find_doc_by_id_stale_index_entry_falls_back_to_rebuild() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_doc(&h, &sample_doc("doc-stale-old", "reused-slug")).unwrap();
        assert!(find_doc_by_id(&h, "doc-stale-old").unwrap().is_some());
        let after_first = doc_id_index_rebuild_count(&h);

        delete_doc(&h, "reused-slug").unwrap();
        write_doc(&h, &sample_doc("doc-stale-new", "reused-slug")).unwrap();

        assert!(
            find_doc_by_id(&h, "doc-stale-old").unwrap().is_none(),
            "stale index entry must not resolve the old id to the new document"
        );
        assert_eq!(
            doc_id_index_rebuild_count(&h),
            after_first + 1,
            "id mismatch on the cached slug must trigger exactly one rebuild"
        );
        let new = find_doc_by_id(&h, "doc-stale-new").unwrap().unwrap();
        assert_eq!(new.slug, "reused-slug");
        assert_eq!(
            doc_id_index_rebuild_count(&h),
            after_first + 1,
            "the rebuild above already indexed doc-stale-new"
        );
    }

    /// The id->slug index must be scoped per `handoff_dir` — a long-running
    /// server process serving more than one project directory must not let
    /// one project's cached id->slug mapping resolve a same-valued `id` in a
    /// *different* project's `docs/` (e.g. two fixtures both using `doc-1`
    /// as their stable id, a common pattern in this very test module).
    #[test]
    fn find_doc_by_id_index_is_isolated_per_handoff_dir() {
        let tmp_a = TempDir::new().unwrap();
        let tmp_b = TempDir::new().unwrap();
        let h_a = handoff(&tmp_a);
        let h_b = handoff(&tmp_b);
        write_doc(&h_a, &sample_doc("doc-shared-id", "slug-in-a")).unwrap();
        write_doc(&h_b, &sample_doc("doc-shared-id", "slug-in-b")).unwrap();

        let from_a = find_doc_by_id(&h_a, "doc-shared-id").unwrap().unwrap();
        let from_b = find_doc_by_id(&h_b, "doc-shared-id").unwrap().unwrap();
        assert_eq!(from_a.slug, "slug-in-a");
        assert_eq!(from_b.slug, "slug-in-b");
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
                ..Default::default()
            },
            TaskLink {
                target: "doc-2".to_string(),
                link_type: "doc".to_string(),
                label: None,
                ..Default::default()
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
                ..Default::default()
            },
            TaskLink {
                target: "https://example.com".to_string(),
                link_type: "url".to_string(),
                label: None,
                ..Default::default()
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
                ..Default::default()
            },
            TaskLink {
                target: "doc-missing".to_string(),
                link_type: "doc".to_string(),
                label: None,
                ..Default::default()
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

    // -- P-M1 process-wide read cache (wiki/240-performance-design.md §4) --

    /// A second `read_doc` call for the same unchanged file must be served
    /// from the process-wide cache keyed by `(path, len, mtime_ns)` rather
    /// than re-parsing frontmatter and recomputing sections/content_hash —
    /// verified via the `doc_read_cache_contains` test hook rather than
    /// timing (timing is covered by `tests/perf_budget.rs`).
    #[test]
    fn read_doc_populates_process_cache_on_first_read() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_doc(&h, &sample_doc("doc-1", "cached-doc")).unwrap();
        write_doc_body(&h, "cached-doc", "Body.\n").unwrap();
        let path = doc_body_path(&h, "cached-doc");

        assert!(
            !doc_read_cache_contains(&path),
            "cache empty before any read"
        );
        let first = read_doc_hashed(&h, "cached-doc").unwrap().unwrap();
        assert!(
            doc_read_cache_contains(&path),
            "cache populated after read_doc"
        );

        let second = read_doc_hashed(&h, "cached-doc").unwrap().unwrap();
        assert_eq!(second.content_hash, first.content_hash);
    }

    /// The cached value is the *same* value `read_doc` always computed
    /// (t123.2's `recompute_sections_and_hash`) — caching must not change
    /// what gets returned, only how often it's recomputed. Pins
    /// `content_hash` / section `content_hash` to `lexsim::content_hash`
    /// directly, both on the first (uncached) read and the second (cached)
    /// one, so a future change to the cache plumbing can't silently start
    /// returning a different hash meaning (done_criteria: hash semantics
    /// unchanged).
    #[test]
    fn read_doc_cached_content_hash_matches_uncached_value() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        let body = "Preamble.\n\n## Heading\nSection body.\n";
        write_doc(&h, &sample_doc("doc-1", "hash-check")).unwrap();
        write_doc_body(&h, "hash-check", body).unwrap();

        let first = read_doc_hashed(&h, "hash-check").unwrap().unwrap();
        assert_eq!(
            first.content_hash.as_deref(),
            Some(lexsim::content_hash(body).as_str())
        );
        assert_eq!(
            first.sections[1].content_hash.as_deref(),
            Some(lexsim::content_hash("## Heading\nSection body.\n").as_str())
        );

        // Second read must be served from cache but return identical values.
        let second = read_doc_hashed(&h, "hash-check").unwrap().unwrap();
        assert_eq!(second.content_hash, first.content_hash);
        assert_eq!(second.sections, first.sections);
    }

    /// An out-of-band edit (bypassing `write_doc_body` entirely, e.g. a
    /// manual `.md` edit — same scenario t123.2's drift detection targets)
    /// changes both length and mtime. The cache's `(path, len, mtime_ns)`
    /// key must miss and `read_doc` must reflect the new body, not the
    /// stale cached one.
    #[test]
    fn read_doc_reparses_after_external_edit_changes_len_and_mtime() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_doc(&h, &sample_doc("doc-1", "ext-edit")).unwrap();
        write_doc_body(&h, "ext-edit", "Old body.\n").unwrap();
        let first = read_doc_hashed(&h, "ext-edit").unwrap().unwrap();
        assert_eq!(
            first.content_hash.as_deref(),
            Some(lexsim::content_hash("Old body.\n").as_str())
        );

        // Bypass write_doc_body's own explicit cache invalidation on purpose
        // — this must simulate a *genuinely external* edit that the cache
        // only catches via the (len, mtime_ns) stamp, not via any of this
        // module's own write-path bookkeeping.
        let path = doc_body_path(&h, "ext-edit");
        frontmatter::write_frontmatter_doc(&path, &first, "New, longer external body.\n").unwrap();

        let second = read_doc_hashed(&h, "ext-edit").unwrap().unwrap();
        assert_eq!(
            second.content_hash.as_deref(),
            Some(lexsim::content_hash("New, longer external body.\n").as_str()),
            "external edit must force re-parse, not serve the stale cached content_hash"
        );
    }

    /// wiki/240-performance-design.md §4 P-M1 explicit caution: a
    /// same-process write must never be served stale from the cache even in
    /// the pathological case where the rewritten file happens to land on
    /// the exact same `(len, mtime_ns)` as what's cached (forced here via
    /// `File::set_modified`, removing any dependency on real filesystem
    /// mtime resolution) — `write_doc_body` must invalidate the cache entry
    /// explicitly rather than relying on the stamp changing.
    #[test]
    fn write_doc_body_invalidates_cache_even_with_identical_len_and_mtime() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_doc(&h, &sample_doc("doc-1", "same-stamp")).unwrap();
        write_doc_body(&h, "same-stamp", "AAAA\n").unwrap();
        let path = doc_body_path(&h, "same-stamp");
        let mtime_before = std::fs::metadata(&path).unwrap().modified().unwrap();

        let first = read_doc_hashed(&h, "same-stamp").unwrap().unwrap();
        assert_eq!(
            first.content_hash.as_deref(),
            Some(lexsim::content_hash("AAAA\n").as_str())
        );

        // Same-length rewrite through the sanctioned write path, then force
        // the mtime back to the exact instant it was before the rewrite.
        write_doc_body(&h, "same-stamp", "BBBB\n").unwrap();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(mtime_before).unwrap();

        let second = read_doc_hashed(&h, "same-stamp").unwrap().unwrap();
        assert_eq!(
            second.content_hash.as_deref(),
            Some(lexsim::content_hash("BBBB\n").as_str()),
            "write_doc_body must invalidate the cache even when (len, mtime_ns) collides \
             with the previous entry"
        );
    }

    /// `write_doc` (frontmatter-only rewrite, body untouched) must also
    /// invalidate its cache entry — a stale cached `DocMetadata` would
    /// otherwise keep returning old `tags`/`task_ids`/etc. after a metadata
    /// update.
    #[test]
    fn write_doc_invalidates_cache() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        let mut doc = sample_doc("doc-1", "meta-update");
        write_doc(&h, &doc).unwrap();
        write_doc_body(&h, "meta-update", "Body.\n").unwrap();

        let first = read_doc(&h, "meta-update").unwrap().unwrap();
        assert!(first.tags.is_empty());

        doc.tags = vec!["updated".to_string()];
        write_doc(&h, &doc).unwrap();

        let second = read_doc(&h, "meta-update").unwrap().unwrap();
        assert_eq!(second.tags, vec!["updated".to_string()]);
    }

    /// `delete_doc_body` must evict the cache entry so a slug reused after
    /// deletion (same path) never resurrects the deleted document's cached
    /// metadata.
    #[test]
    fn delete_doc_body_invalidates_cache() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_doc(&h, &sample_doc("doc-1", "to-delete")).unwrap();
        write_doc_body(&h, "to-delete", "Body.\n").unwrap();
        let path = doc_body_path(&h, "to-delete");

        let _ = read_doc(&h, "to-delete").unwrap().unwrap();
        assert!(doc_read_cache_contains(&path));

        delete_doc_body(&h, "to-delete").unwrap();
        assert!(
            !doc_read_cache_contains(&path),
            "cache entry must be evicted on delete"
        );
        assert!(read_doc(&h, "to-delete").unwrap().is_none());
    }

    // -- read_doc_with_body (review round 2 MAJOR fix, wiki/240 §4 P-M3) --

    /// The metadata and body returned by `read_doc_with_body` must always be
    /// mutually consistent: every section's byte range must fall within
    /// `body`, land on a UTF-8 char boundary (JA content — multi-byte
    /// headings), and the last section's end must equal `body.len()`.
    #[test]
    fn read_doc_with_body_sections_are_consistent_with_returned_body() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        let body = "前書き。\n\n## 第一節\n本文A。\n\n## 第二節\n本文B。\n";
        write_doc(&h, &sample_doc("doc-1", "ja-consistency")).unwrap();
        write_doc_body(&h, "ja-consistency", body).unwrap();

        let (doc, returned_body) = read_doc_with_body(&h, "ja-consistency").unwrap().unwrap();
        assert_eq!(returned_body, body);
        assert!(!doc.sections.is_empty());
        for section in &doc.sections {
            let start = section.byte_offset;
            let end = section.byte_offset + section.byte_length;
            assert!(
                start <= returned_body.len() && end <= returned_body.len(),
                "section range out of bounds: {start}..{end}, body len {}",
                returned_body.len()
            );
            assert!(
                returned_body.is_char_boundary(start) && returned_body.is_char_boundary(end),
                "section range must fall on UTF-8 char boundaries for JA content: {start}..{end}"
            );
        }
        let last = doc.sections.last().unwrap();
        assert_eq!(
            last.byte_offset + last.byte_length,
            returned_body.len(),
            "last section must end exactly at body.len()"
        );
    }

    /// Mirrors `read_doc`'s own contract: the body returned is the document's
    /// authored body (frontmatter stripped), matching `read_doc_body`.
    #[test]
    fn read_doc_with_body_body_matches_read_doc_body() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_doc(&h, &sample_doc("doc-1", "body-match")).unwrap();
        write_doc_body(&h, "body-match", "Some body.\n").unwrap();

        let (_doc, body) = read_doc_with_body(&h, "body-match").unwrap().unwrap();
        assert_eq!(body, read_doc_body(&h, "body-match").unwrap().unwrap());
    }

    /// Missing document is `Ok(None)`, same as `read_doc`.
    #[test]
    fn read_doc_with_body_missing_is_none() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        assert!(read_doc_with_body(&h, "does-not-exist").unwrap().is_none());
    }

    /// A second call for the same unchanged file is served from the P-M1
    /// cache for `sections`/`content_hash` (verified via
    /// `doc_read_cache_contains`, same technique as
    /// `read_doc_populates_process_cache_on_first_read`), while still
    /// returning a body read fresh on every call.
    #[test]
    fn read_doc_with_body_reuses_cached_sections_when_stamp_unchanged() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_doc(&h, &sample_doc("doc-1", "cache-reuse")).unwrap();
        write_doc_body(&h, "cache-reuse", "Body one.\n").unwrap();
        let path = doc_body_path(&h, "cache-reuse");

        let (first, _) = read_doc_with_body_hashed(&h, "cache-reuse")
            .unwrap()
            .unwrap();
        assert!(doc_read_cache_contains(&path));

        let (second, body2) = read_doc_with_body_hashed(&h, "cache-reuse")
            .unwrap()
            .unwrap();
        assert_eq!(second.content_hash, first.content_hash);
        assert_eq!(body2, "Body one.\n");
    }

    /// After a write through the sanctioned write path (which invalidates
    /// the cache explicitly), `read_doc_with_body` must reflect the new
    /// content, not a stale cached one — same invariant `read_doc` already
    /// upholds.
    #[test]
    fn read_doc_with_body_reflects_write_after_cache_invalidation() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_doc(&h, &sample_doc("doc-1", "post-write")).unwrap();
        write_doc_body(&h, "post-write", "Old body.\n").unwrap();

        let (first, body1) = read_doc_with_body_hashed(&h, "post-write")
            .unwrap()
            .unwrap();
        assert_eq!(body1, "Old body.\n");

        write_doc_body(&h, "post-write", "New, longer body.\n").unwrap();

        let (second, body2) = read_doc_with_body_hashed(&h, "post-write")
            .unwrap()
            .unwrap();
        assert_eq!(body2, "New, longer body.\n");
        assert_ne!(second.content_hash, first.content_hash);
    }

    // -- t370.8: deferred content_hash (P-M1, wiki/240-performance-design.md §4) --

    /// The plain (non-`_hashed`) read functions must never pay the
    /// `lexsim::content_hash` cost: `content_hash` stays `None` on both the
    /// document and every section, while the byte-offset/heading structure
    /// is still fully computed.
    #[test]
    fn read_doc_and_read_all_docs_default_to_no_content_hash() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_doc(&h, &sample_doc("doc-1", "lazy-doc")).unwrap();
        write_doc_body(&h, "lazy-doc", "Preamble.\n\n## A\nBody A\n").unwrap();

        let doc = read_doc(&h, "lazy-doc").unwrap().unwrap();
        assert_eq!(
            doc.content_hash, None,
            "read_doc must not compute content_hash by default"
        );
        assert!(!doc.sections.is_empty());
        for section in &doc.sections {
            assert_eq!(section.content_hash, None);
        }

        let all = read_all_docs(&h).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].content_hash, None);
    }

    /// `read_doc_hashed`/`read_all_docs_hashed` must guarantee a real
    /// `content_hash` on the document and every section, matching
    /// `lexsim::content_hash` exactly (same value semantics as before
    /// t370.8 introduced laziness).
    #[test]
    fn read_doc_hashed_and_read_all_docs_hashed_always_populate_content_hash() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        let body = "Preamble.\n\n## A\nBody A\n";
        write_doc(&h, &sample_doc("doc-1", "hashed-doc")).unwrap();
        write_doc_body(&h, "hashed-doc", body).unwrap();

        let doc = read_doc_hashed(&h, "hashed-doc").unwrap().unwrap();
        assert_eq!(
            doc.content_hash.as_deref(),
            Some(lexsim::content_hash(body).as_str())
        );
        assert!(doc.sections.iter().all(|s| s.content_hash.is_some()));

        let all = read_all_docs_hashed(&h).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].content_hash, doc.content_hash);
    }

    /// A lazily-read document (`content_hash: None` in memory, e.g. loaded
    /// via `DocSet` for a metadata-only change) must still end up with a
    /// real, correct `content_hash` on disk once written — `write_doc`/
    /// `write_doc_with_body` compute it against the exact body being
    /// persisted rather than writing an empty/missing value (P-M1, t370.8:
    /// "空文字列を「未計算」の意味で流用しない" — the on-disk field is never
    /// left empty or absent).
    #[test]
    fn write_doc_fills_in_missing_content_hash_before_persisting() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        let body = "Preamble.\n\n## A\nBody A\n";
        write_doc(&h, &sample_doc("doc-1", "fill-hash")).unwrap();
        write_doc_body(&h, "fill-hash", body).unwrap();

        // Simulate a DocSet-style read: no hash requested.
        let mut doc = read_doc(&h, "fill-hash").unwrap().unwrap();
        assert_eq!(doc.content_hash, None);
        doc.tags = vec!["touched".to_string()];
        write_doc(&h, &doc).unwrap();

        // The persisted frontmatter must carry a real, correct hash even
        // though the in-memory `doc` passed to `write_doc` never had one.
        let reread = read_doc_hashed(&h, "fill-hash").unwrap().unwrap();
        assert_eq!(
            reread.content_hash.as_deref(),
            Some(lexsim::content_hash(body).as_str())
        );
        assert_eq!(reread.tags, vec!["touched".to_string()]);
    }

    /// The process read cache must never *downgrade* a `_hashed` request: a
    /// lazy read (e.g. `DocSet::load` during `update_task` link propagation)
    /// caches the document with `content_hash: None`; a later
    /// `read_doc_hashed`/`read_doc_with_body_hashed` for the same unchanged
    /// file (same cache stamp) must still return a real hash, not serve the
    /// cached `None` (which would make `doc_verify check` record
    /// `content_hash_at_verify: None` and `doc_reassemble` report false
    /// drift). The upgraded (hashed) entry then serves later lazy reads.
    #[test]
    fn hashed_read_after_lazy_read_is_not_downgraded_by_cache() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        let body = "Preamble.\n\n## A\nBody A\n";
        write_doc(&h, &sample_doc("doc-1", "upgrade-doc")).unwrap();
        write_doc_body(&h, "upgrade-doc", body).unwrap();
        let path = doc_body_path(&h, "upgrade-doc");

        let lazy = read_doc(&h, "upgrade-doc").unwrap().unwrap();
        assert_eq!(lazy.content_hash, None);
        assert!(
            doc_read_cache_contains(&path),
            "lazy read must populate the cache so the next read is a cache hit"
        );

        let hashed = read_doc_hashed(&h, "upgrade-doc").unwrap().unwrap();
        assert_eq!(
            hashed.content_hash.as_deref(),
            Some(lexsim::content_hash(body).as_str()),
            "read_doc_hashed must not serve a cached lazy (None) entry"
        );
        assert!(hashed.sections.iter().all(|s| s.content_hash.is_some()));

        let (with_body, _) = read_doc_with_body_hashed(&h, "upgrade-doc")
            .unwrap()
            .unwrap();
        assert_eq!(with_body.content_hash, hashed.content_hash);

        // Same for the with-body variant starting from a fresh lazy entry.
        write_doc_body(&h, "upgrade-doc", body).unwrap();
        let (lazy2, _) = read_doc_with_body(&h, "upgrade-doc").unwrap().unwrap();
        assert_eq!(lazy2.content_hash, None);
        let (hashed2, _) = read_doc_with_body_hashed(&h, "upgrade-doc")
            .unwrap()
            .unwrap();
        assert_eq!(
            hashed2.content_hash.as_deref(),
            Some(lexsim::content_hash(body).as_str()),
            "read_doc_with_body_hashed must not serve a cached lazy (None) entry"
        );
    }
}
