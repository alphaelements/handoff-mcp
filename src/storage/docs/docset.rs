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
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{read_all_docs, write_doc, DocMetadata};

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
        Ok(Self {
            handoff_dir: handoff_dir.to_path_buf(),
            docs,
            index_by_id,
            dirty: HashSet::new(),
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
    pub fn flush(&mut self) -> Result<()> {
        for id in std::mem::take(&mut self.dirty) {
            if let Some(&i) = self.index_by_id.get(&id) {
                write_doc(&self.handoff_dir, &self.docs[i])?;
            }
        }
        Ok(())
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
}
