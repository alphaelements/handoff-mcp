//! Request-scoped snapshot of every document in `docs/`, loaded once and
//! mutated in memory (wiki/240-performance-design.md §4 P-M3). Handlers that
//! touch several documents within a single MCP call — `link_requirements_to_task`,
//! `unlink_requirements_from_task`, `propagate_dev_stage_for_task`
//! (`src/mcp/handlers/docs.rs`) — used to call `read_all_docs`/`find_doc_by_id`
//! once per document group and then re-read the whole corpus a second time to
//! recompute `_requirements_summary.json` (wiki/240 §3 C3/C4). A `DocSet`
//! loads the corpus exactly once, lets callers mutate documents by `id`
//! in-memory, and on [`DocSet::flush`] writes back only the documents that
//! were actually marked dirty — everything else in the request (including
//! the summary aggregation) reads from the same in-memory snapshot instead
//! of touching disk again.
//!
//! Kept deliberately small so the M1 work that follows this task
//! (wiki/220-vmodel-integration-design.md §2.4 — `sync_layer_items` riding on
//! `doc_save`/`doc_update_section`) has a stable place to hang additional
//! post-mutation passes (layer sync, trace derivation) without re-loading
//! the corpus itself: `iter()`/`docs()` expose the loaded snapshot, and
//! `get_mut`/`mark_dirty` are the two primitives any such pass would also
//! need.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{doc_body_path, read_all_docs, write_doc, DocMetadata};

/// `(len, mtime_ns)` snapshot of one document's `_doc.<slug>.md` file at the
/// time [`DocSet::load`] read it — the optimistic-lock fingerprint P-M7
/// (wiki/240-performance-design.md §4, NFR-007) compares against right
/// before writing it back. `None` means the file did not exist at load time
/// (should not happen for anything already in the loaded set, but kept
/// `Option` so a missing/removed file at flush time is a comparable "not the
/// same as before" state rather than a special case).
type Fingerprint = Option<(u64, u64)>;

fn stat_fingerprint(path: &Path) -> Result<Fingerprint> {
    match std::fs::metadata(path) {
        Ok(meta) => {
            // `0` here (platform doesn't report mtime at all, or reports one
            // before the Unix epoch — both practically never happen on a
            // real filesystem) is a safe default *for this specific use*:
            // both the load-time and flush-time calls degrade to the same
            // constant, so the `(len, mtime_ns)` comparison below still
            // correctly detects a length change and simply loses precision
            // on mtime-only changes, rather than making `flush()` fail
            // outright on such a platform. This is not the same tradeoff as
            // `compute_derived_inputs`'s `mtime_ns` (wiki/220 §4.3's
            // `inputs.docs_max_mtime_ns`), which *is* serialized out to an
            // external reader and bails loudly on the same failure instead.
            let mtime_ns = meta
                .modified()
                .map(|m| {
                    m.duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos() as u64
                })
                .unwrap_or(0);
            Ok(Some((meta.len(), mtime_ns)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Returned by [`DocSet::flush`] when one or more dirty documents' files
/// changed on disk since [`DocSet::load`] — some other process (an external
/// MCP server sharing this `.handoff/`, the VSCode writer, a hand edit)
/// wrote to the same file in between. Flushing anyway would silently
/// discard that other write, so `flush()` refuses instead: it writes none
/// of the conflicting documents and reports every conflicting `id` here so
/// a caller can reload and retry (see [`load_mutate_flush_with_retry`])
/// rather than the conflict being swallowed into "just overwrite it".
#[derive(Debug)]
pub struct DocSetConflict {
    pub doc_ids: Vec<String>,
}

impl fmt::Display for DocSetConflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "document(s) changed on disk since this DocSet was loaded, refusing to overwrite: {}",
            self.doc_ids.join(", ")
        )
    }
}

impl std::error::Error for DocSetConflict {}

/// A snapshot of every document in `docs/`, loaded once via [`DocSet::load`].
/// Documents are looked up by their stable `id` (not file-naming `slug`) in
/// O(1) via an in-memory index built at load time, so callers that resolve a
/// batch of `id`s up front (e.g. `resolve_stable_ids`) never need to fall
/// back to a per-lookup full-corpus scan (`find_doc_by_id`).
pub struct DocSet {
    handoff_dir: PathBuf,
    docs: Vec<DocMetadata>,
    index_by_id: HashMap<String, usize>,
    dirty: HashSet<String>,
    /// Optimistic-lock fingerprints (P-M7), keyed by `id`, captured at
    /// `load()` and refreshed after each successful `flush()` write.
    snapshot: HashMap<String, Fingerprint>,
}

impl DocSet {
    /// Loads every document in `docs/` exactly once for the lifetime of this
    /// `DocSet` (via [`read_all_docs`]).
    pub fn load(handoff_dir: &Path) -> Result<Self> {
        let docs = read_all_docs(handoff_dir)?;
        let index_by_id = docs
            .iter()
            .enumerate()
            .map(|(i, d)| (d.id.clone(), i))
            .collect();
        let mut snapshot = HashMap::new();
        for doc in &docs {
            let fp = stat_fingerprint(&doc_body_path(handoff_dir, &doc.slug))?;
            snapshot.insert(doc.id.clone(), fp);
        }
        Ok(Self {
            handoff_dir: handoff_dir.to_path_buf(),
            docs,
            index_by_id,
            dirty: HashSet::new(),
            snapshot,
        })
    }

    /// Read-only lookup by stable `id` — O(1), no scan.
    pub fn get(&self, id: &str) -> Option<&DocMetadata> {
        self.index_by_id.get(id).map(|&i| &self.docs[i])
    }

    /// Mutable lookup by stable `id` — O(1), no scan. Does **not** mark the
    /// document dirty by itself (a caller that ends up not changing
    /// anything through the reference should not force a write) — call
    /// [`DocSet::mark_dirty`] once a real change has been made.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut DocMetadata> {
        let &i = self.index_by_id.get(id)?;
        Some(&mut self.docs[i])
    }

    /// Marks `id` as changed since load (or the last [`DocSet::flush`]) —
    /// `flush()` will write it back. A no-op if `id` isn't in the set.
    pub fn mark_dirty(&mut self, id: &str) {
        if self.index_by_id.contains_key(id) {
            self.dirty.insert(id.to_string());
        }
    }

    /// Iterates every loaded document (post-mutation, if any `get_mut` calls
    /// happened first) — the in-memory equivalent of `read_all_docs`, used
    /// e.g. to recompute `_requirements_summary.json` from this same
    /// snapshot instead of re-reading the corpus.
    pub fn iter(&self) -> impl Iterator<Item = &DocMetadata> {
        self.docs.iter()
    }

    /// The full loaded/mutated document slice.
    pub fn docs(&self) -> &[DocMetadata] {
        &self.docs
    }

    /// Writes back every document marked dirty since load (or the last
    /// flush) via `write_doc`, then clears the dirty set. Documents never
    /// marked dirty are never written — the whole point of P-M3 ("変更され
    /// た文書だけを書く").
    ///
    /// P-M7 (wiki/240 §4, NFR-007): before writing anything, re-stats every
    /// dirty document's file and compares against the fingerprint recorded
    /// at `load()` (or the previous `flush()`). Any mismatch means another
    /// process wrote that file in between — `flush()` writes **none** of
    /// the dirty documents in that case (not just skips the conflicting
    /// one, so a caller retrying the whole mutation against a freshly
    /// reloaded `DocSet` never has to reason about a partially-applied
    /// previous attempt) and returns [`DocSetConflict`] instead of silently
    /// overwriting the other write.
    pub fn flush(&mut self) -> Result<()> {
        if self.dirty.is_empty() {
            return Ok(());
        }

        let mut conflicts = Vec::new();
        for id in &self.dirty {
            let Some(&i) = self.index_by_id.get(id) else {
                continue;
            };
            let current = stat_fingerprint(&doc_body_path(&self.handoff_dir, &self.docs[i].slug))?;
            if self.snapshot.get(id) != Some(&current) {
                conflicts.push(id.clone());
            }
        }
        if !conflicts.is_empty() {
            conflicts.sort();
            return Err(DocSetConflict { doc_ids: conflicts }.into());
        }

        for id in std::mem::take(&mut self.dirty) {
            if let Some(&i) = self.index_by_id.get(&id) {
                write_doc(&self.handoff_dir, &self.docs[i])?;
                let fp = stat_fingerprint(&doc_body_path(&self.handoff_dir, &self.docs[i].slug))?;
                self.snapshot.insert(id, fp);
            }
        }
        Ok(())
    }
}

/// Maximum number of reload-and-retry attempts [`load_mutate_flush_with_retry`]
/// makes after a [`DocSetConflict`] before giving up and returning the
/// conflict to the caller as an error — retries are for the ordinary case of
/// racing one other concurrent writer, not an unbounded spin under sustained
/// contention (P-M7, NFR-007: bounded, fails loudly rather than looping
/// forever or silently dropping a change).
const DOC_SET_MAX_RETRIES: u32 = 3;

/// Loads a fresh [`DocSet`], runs `mutate` against it, and flushes — retrying
/// the *entire* load/mutate/flush cycle from a freshly reloaded `DocSet` up
/// to [`DOC_SET_MAX_RETRIES`] times when `flush()` reports a
/// [`DocSetConflict`] (P-M7, wiki/240 §4). `mutate` must derive its changes
/// only from the `DocSet` it is given (plus whatever inputs the caller
/// closes over) — never from state left over by a previous attempt — so that
/// re-running it against the post-conflict on-disk state is a correct
/// "reapply", not a re-application of stale reads. This is what
/// `link_requirements_to_task`/`unlink_requirements_from_task`/
/// `propagate_dev_stage_for_task` (`src/mcp/handlers/docs.rs`) use for their
/// document read-modify-write (wiki/240 §3 C9: "link/unlink/propagate の文書
/// RMW には楽観ロックがない").
///
/// Exhausting the retry budget returns the last [`DocSetConflict`] as an
/// error rather than silently giving up — a caller must not treat an
/// unresolved conflict as success.
pub fn load_mutate_flush_with_retry<F, T>(handoff_dir: &Path, mut mutate: F) -> Result<(DocSet, T)>
where
    F: FnMut(&mut DocSet) -> Result<T>,
{
    let mut attempt = 0u32;
    loop {
        let mut doc_set = DocSet::load(handoff_dir)?;
        let out = mutate(&mut doc_set)?;
        match doc_set.flush() {
            Ok(()) => return Ok((doc_set, out)),
            Err(e) => {
                let is_conflict = e.downcast_ref::<DocSetConflict>().is_some();
                if is_conflict && attempt < DOC_SET_MAX_RETRIES {
                    attempt += 1;
                    continue;
                }
                return Err(e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::docs::{ensure_docs_dir, read_doc, write_doc as storage_write_doc};
    use tempfile::TempDir;

    fn handoff(tmp: &TempDir) -> PathBuf {
        let dir = tmp.path().join(".handoff");
        ensure_docs_dir(&dir).unwrap();
        dir
    }

    fn sample_doc(id: &str, slug: &str) -> DocMetadata {
        DocMetadata::new(
            id.to_string(),
            slug.to_string(),
            "DocSet test doc".to_string(),
            "note".to_string(),
            "2026-09-26T00:00:00Z".to_string(),
        )
    }

    #[test]
    fn load_indexes_every_document_by_id() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        storage_write_doc(&h, &sample_doc("doc-1", "one")).unwrap();
        storage_write_doc(&h, &sample_doc("doc-2", "two")).unwrap();

        let set = DocSet::load(&h).unwrap();
        assert_eq!(set.get("doc-1").unwrap().slug, "one");
        assert_eq!(set.get("doc-2").unwrap().slug, "two");
        assert!(set.get("doc-nope").is_none());
    }

    #[test]
    fn flush_writes_only_dirty_documents() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        storage_write_doc(&h, &sample_doc("doc-1", "one")).unwrap();
        storage_write_doc(&h, &sample_doc("doc-2", "two")).unwrap();

        let mut set = DocSet::load(&h).unwrap();
        set.get_mut("doc-1")
            .unwrap()
            .tags
            .push("touched".to_string());
        set.mark_dirty("doc-1");
        // doc-2 is mutated in memory but never marked dirty — flush() must
        // not write it. The in-memory change is what makes a regression
        // observable: if flush() rewrote every loaded document, this tag
        // would reach disk (an unchanged in-memory copy would be written
        // back byte-identical and the assertion below could not tell).
        set.get_mut("doc-2")
            .unwrap()
            .tags
            .push("not-persisted".to_string());
        set.flush().unwrap();

        let one = read_doc(&h, "one").unwrap().unwrap();
        assert_eq!(one.tags, vec!["touched".to_string()]);
        let two = read_doc(&h, "two").unwrap().unwrap();
        assert!(
            two.tags.is_empty(),
            "doc-2 was never marked dirty, must not have been rewritten with stale in-memory state"
        );
    }

    #[test]
    fn mark_dirty_on_unknown_id_is_a_no_op() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        let mut set = DocSet::load(&h).unwrap();
        set.mark_dirty("doc-does-not-exist");
        // Must not panic and flush() must not error.
        set.flush().unwrap();
    }

    /// P-M7 (wiki/240-performance-design.md §4, NFR-007): a document `flush`
    /// must not blindly overwrite a file another process (e.g. the VSCode
    /// writer) modified after this `DocSet` was loaded — that would silently
    /// discard the other writer's change. `flush()` re-stats each dirty
    /// document's file right before writing and refuses (returns a
    /// [`DocSetConflict`], not a generic error swallowed into a default) if
    /// its `(len, mtime_ns)` no longer matches what was recorded at `load`
    /// time.
    #[test]
    fn flush_detects_conflicting_external_write_and_does_not_overwrite_it() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        storage_write_doc(&h, &sample_doc("doc-1", "one")).unwrap();

        let mut set = DocSet::load(&h).unwrap();
        set.get_mut("doc-1")
            .unwrap()
            .tags
            .push("from-docset".to_string());
        set.mark_dirty("doc-1");

        // Simulate a concurrent external writer (e.g. VSCode) touching the
        // same file after `load()` but before this `flush()`.
        std::thread::sleep(std::time::Duration::from_millis(5));
        let mut external = sample_doc("doc-1", "one");
        external.tags.push("from-external-writer".to_string());
        storage_write_doc(&h, &external).unwrap();

        let err = set
            .flush()
            .expect_err("a conflicting external write must be reported, not silently overwritten");
        let conflict = err
            .downcast_ref::<DocSetConflict>()
            .expect("flush() must return a DocSetConflict, not a generic error");
        assert_eq!(conflict.doc_ids, vec!["doc-1".to_string()]);

        let on_disk = read_doc(&h, "one").unwrap().unwrap();
        assert_eq!(
            on_disk.tags,
            vec!["from-external-writer".to_string()],
            "the external writer's content must survive untouched"
        );
    }

    /// `load_mutate_flush_with_retry` (P-M7): on a detected conflict, it
    /// reloads a fresh `DocSet` and re-runs the mutation closure against the
    /// post-conflict state, so neither side's change is lost — the retry
    /// finishes successfully instead of surfacing the conflict to the
    /// caller.
    #[test]
    fn load_mutate_flush_with_retry_recovers_from_one_external_write_without_losing_either_change()
    {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        storage_write_doc(&h, &sample_doc("doc-1", "one")).unwrap();

        let mut attempts = 0u32;
        let (doc_set, ()) = load_mutate_flush_with_retry(&h, |set| {
            attempts += 1;
            set.get_mut("doc-1")
                .unwrap()
                .tags
                .push("from-docset".to_string());
            set.mark_dirty("doc-1");

            // Only on the *first* attempt, race in an external write after
            // this closure already read/mutated its own in-memory copy but
            // before `flush()` re-stats the file — deterministic (no sleep
            // races): the closure itself is the injection point.
            if attempts == 1 {
                let mut external = sample_doc("doc-1", "one");
                external.tags.push("from-external-writer".to_string());
                storage_write_doc(&h, &external).unwrap();
            }
            Ok(())
        })
        .unwrap();

        assert_eq!(attempts, 2, "must retry exactly once after the conflict");
        let on_disk = read_doc(&h, "one").unwrap().unwrap();
        assert_eq!(
            on_disk.tags,
            vec![
                "from-external-writer".to_string(),
                "from-docset".to_string()
            ],
            "retry must preserve the external writer's change AND apply ours"
        );
        assert!(doc_set
            .get("doc-1")
            .unwrap()
            .tags
            .contains(&"from-docset".to_string()));
    }
}
