//! Document management: splitting a single authored Markdown body into
//! fragments, reassembling fragments back into a byte-identical body, and
//! persisting both to `.handoff/docs/`.
//!
//! Layout:
//!
//! ```text
//! .handoff/docs/
//!   _doc.<doc-id>.json          # document metadata
//!   _frag.<doc-id>.<seq>.json   # fragment metadata
//!   _frag.<doc-id>.<seq>.md     # fragment body (pure Markdown)
//!   injected/
//!     <session-id>.json         # per-session "already injected" sidecar
//! ```
//!
//! See `wiki/130-document-management.md` §3-4 for the full storage
//! architecture and data model, and §8 for the reversibility guarantee that
//! [`split`](split::split) + [`reassemble`](reassemble::reassemble) implement.
//!
//! All writes go through [`crate::storage::atomic_write`] and `docs/` is
//! created lazily on first write (mirrors `src/storage/memory/mod.rs`), so
//! projects created before this feature shipped are unaffected until they
//! first call `doc_save`. MCP tool wiring (`handoff_doc_*` handlers) is a
//! separate task (P1-3 / t96) and intentionally not implemented here.

pub mod model;
pub mod reassemble;
pub mod split;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub use model::{DocMetadata, DocRelation, DocSource, FragmentMetadata, FragmentSummary};

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

fn doc_meta_path(handoff_dir: &Path, doc_id: &str) -> PathBuf {
    docs_dir(handoff_dir).join(format!("_doc.{doc_id}.json"))
}

fn fragment_meta_path(handoff_dir: &Path, doc_id: &str, seq: usize) -> PathBuf {
    docs_dir(handoff_dir).join(format!("_frag.{doc_id}.{seq}.json"))
}

fn fragment_body_path(handoff_dir: &Path, doc_id: &str, seq: usize) -> PathBuf {
    docs_dir(handoff_dir).join(format!("_frag.{doc_id}.{seq}.md"))
}

/// Write a document's metadata to `_doc.<id>.json` atomically, creating
/// `docs/` lazily.
pub fn write_doc(handoff_dir: &Path, doc: &DocMetadata) -> Result<PathBuf> {
    ensure_docs_dir(handoff_dir)?;
    let path = doc_meta_path(handoff_dir, &doc.id);
    let content = serde_json::to_string_pretty(doc).context("Failed to serialize document")?;
    crate::storage::atomic_write(&path, content.as_bytes())
        .with_context(|| format!("Failed to write document: {}", path.display()))?;
    Ok(path)
}

/// Read one document's metadata by exact id. Returns `Ok(None)` when the file
/// does not exist or fails to parse (lenient read, same policy as memory).
pub fn read_doc(handoff_dir: &Path, doc_id: &str) -> Result<Option<DocMetadata>> {
    let path = doc_meta_path(handoff_dir, doc_id);
    match std::fs::read_to_string(&path) {
        Ok(content) => Ok(serde_json::from_str::<DocMetadata>(&content).ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("Failed to read document: {}", path.display())),
    }
}

/// Read every document in `docs/`, skipping any `_doc.*.json` file that
/// fails to parse. Fragment files (`_frag.*`) and the `injected/`
/// subdirectory are ignored. Returns an empty vec when `docs/` does not
/// exist (uninitialized / feature-untouched projects).
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

/// Delete a document's metadata file by exact id. Returns `Ok(false)` when
/// the file does not exist. Does not touch that document's fragments —
/// callers that want a full delete should also remove fragments via
/// [`delete_fragment`] for every `seq` in `doc.fragments`.
pub fn delete_doc(handoff_dir: &Path, doc_id: &str) -> Result<bool> {
    let path = doc_meta_path(handoff_dir, doc_id);
    if !path.exists() {
        return Ok(false);
    }
    std::fs::remove_file(&path)
        .with_context(|| format!("Failed to delete document: {}", path.display()))?;
    Ok(true)
}

/// Write one fragment as its two-file pair: `_frag.<doc-id>.<seq>.json`
/// (metadata) and `_frag.<doc-id>.<seq>.md` (body, pure Markdown). Both
/// writes are atomic; `docs/` is created lazily. `body` is written exactly
/// as given — no re-rendering — so [`read_all_fragments`] concatenated in
/// `seq` order round-trips through [`reassemble::reassemble`].
pub fn write_fragment(handoff_dir: &Path, meta: &FragmentMetadata, body: &str) -> Result<()> {
    ensure_docs_dir(handoff_dir)?;

    let meta_path = fragment_meta_path(handoff_dir, &meta.doc_id, meta.seq);
    let meta_content =
        serde_json::to_string_pretty(meta).context("Failed to serialize fragment metadata")?;
    crate::storage::atomic_write(&meta_path, meta_content.as_bytes())
        .with_context(|| format!("Failed to write fragment metadata: {}", meta_path.display()))?;

    let body_path = fragment_body_path(handoff_dir, &meta.doc_id, meta.seq);
    crate::storage::atomic_write(&body_path, body.as_bytes())
        .with_context(|| format!("Failed to write fragment body: {}", body_path.display()))?;

    Ok(())
}

/// Read one fragment (metadata + body) by `doc_id`/`seq`. Returns `Ok(None)`
/// when either half of the pair is missing or the metadata fails to parse —
/// a fragment is only valid when both files are present and consistent.
pub fn read_fragment(
    handoff_dir: &Path,
    doc_id: &str,
    seq: usize,
) -> Result<Option<(FragmentMetadata, String)>> {
    let meta_path = fragment_meta_path(handoff_dir, doc_id, seq);
    let meta_content = match std::fs::read_to_string(&meta_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(e).with_context(|| {
                format!("Failed to read fragment metadata: {}", meta_path.display())
            })
        }
    };
    let Ok(meta) = serde_json::from_str::<FragmentMetadata>(&meta_content) else {
        return Ok(None);
    };

    let body_path = fragment_body_path(handoff_dir, doc_id, seq);
    let body = match std::fs::read_to_string(&body_path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(e)
                .with_context(|| format!("Failed to read fragment body: {}", body_path.display()))
        }
    };

    Ok(Some((meta, body)))
}

/// Read every fragment belonging to `doc_id`, sorted by `seq` ascending. A
/// fragment whose pair is incomplete or unparseable is skipped leniently
/// (same policy as [`read_all_docs`]) rather than failing the whole read —
/// concatenating the returned bodies in order reproduces the original
/// document only when no fragment was skipped.
pub fn read_all_fragments(
    handoff_dir: &Path,
    doc_id: &str,
) -> Result<Vec<(FragmentMetadata, String)>> {
    let dir = docs_dir(handoff_dir);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let prefix = format!("_frag.{doc_id}.");
    let mut seqs: Vec<usize> = Vec::new();
    for entry in std::fs::read_dir(&dir)
        .with_context(|| format!("Failed to read docs dir: {}", dir.display()))?
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(rest) = name.strip_prefix(&prefix) else {
            continue;
        };
        let Some(seq_str) = rest.strip_suffix(".json") else {
            continue;
        };
        if let Ok(seq) = seq_str.parse::<usize>() {
            seqs.push(seq);
        }
    }
    seqs.sort_unstable();
    seqs.dedup();

    let mut fragments = Vec::with_capacity(seqs.len());
    for seq in seqs {
        if let Some(pair) = read_fragment(handoff_dir, doc_id, seq)? {
            fragments.push(pair);
        }
        // Incomplete/unparseable pair: skip silently (lenient read).
    }
    Ok(fragments)
}

/// Delete one fragment's two-file pair by exact `doc_id`/`seq`. Returns
/// `Ok(false)` when neither file existed to begin with (delete is
/// idempotent: a missing `.md` after the `.json` was already removed still
/// counts as "was here, now gone" rather than an error).
pub fn delete_fragment(handoff_dir: &Path, doc_id: &str, seq: usize) -> Result<bool> {
    let meta_path = fragment_meta_path(handoff_dir, doc_id, seq);
    let body_path = fragment_body_path(handoff_dir, doc_id, seq);
    let existed = meta_path.exists() || body_path.exists();

    if meta_path.exists() {
        std::fs::remove_file(&meta_path).with_context(|| {
            format!(
                "Failed to delete fragment metadata: {}",
                meta_path.display()
            )
        })?;
    }
    if body_path.exists() {
        std::fs::remove_file(&body_path)
            .with_context(|| format!("Failed to delete fragment body: {}", body_path.display()))?;
    }
    Ok(existed)
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

    fn sample_doc(id: &str) -> DocMetadata {
        DocMetadata::new(
            id.to_string(),
            "Session Loop Verification".to_string(),
            "spec".to_string(),
            "2026-07-11T14:30:00Z".to_string(),
        )
    }

    fn sample_fragment(
        doc_id: &str,
        seq: usize,
        heading: Option<&str>,
        level: u8,
    ) -> FragmentMetadata {
        FragmentMetadata::new(
            doc_id.to_string(),
            seq,
            heading.map(str::to_string),
            level,
            "fragment body",
        )
    }

    #[test]
    fn write_then_read_doc_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        let mut doc = sample_doc("doc-1");
        doc.tags = vec!["session-loop".to_string(), "verification".to_string()];
        doc.scope_paths = vec!["src/mcp/handlers/".to_string()];
        doc.task_ids = vec!["T-79".to_string()];
        doc.has_bom = true;
        doc.line_ending = "crlf".to_string();
        doc.source.frontmatter = Some("title: Foo\n".to_string());
        doc.fragments = vec![
            FragmentSummary {
                seq: 0,
                heading: "(前文)".to_string(),
                level: 0,
            },
            FragmentSummary {
                seq: 1,
                heading: "アーキテクチャ".to_string(),
                level: 1,
            },
        ];

        write_doc(&h, &doc).unwrap();

        let back = read_doc(&h, "doc-1").unwrap().expect("doc must exist");
        assert_eq!(back.id, "doc-1");
        assert_eq!(back.title, "Session Loop Verification");
        assert_eq!(back.doc_type, "spec");
        assert_eq!(back.tags, doc.tags);
        assert_eq!(back.scope_paths, doc.scope_paths);
        assert_eq!(back.task_ids, doc.task_ids);
        assert_eq!(back.fragments.len(), 2);
        assert_eq!(back.fragments[1].heading, "アーキテクチャ");
        assert_eq!(back.auto_inject, "auto");
        assert!(back.parent_id.is_none());
        assert!(back.has_bom, "has_bom must round-trip through write/read");
        assert_eq!(back.line_ending, "crlf");
        assert_eq!(
            back.source.frontmatter.as_deref(),
            Some("title: Foo\n"),
            "source.frontmatter must round-trip through write/read"
        );
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
    fn read_all_docs_skips_corrupt_and_ignores_fragments() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_doc(&h, &sample_doc("doc-good")).unwrap();
        std::fs::write(docs_dir(&h).join("_doc.doc-bad.json"), b"{not json").unwrap();
        // A fragment pair sitting alongside must never be mistaken for a doc.
        write_fragment(&h, &sample_fragment("doc-good", 0, None, 0), "body").unwrap();

        let all = read_all_docs(&h).unwrap();
        assert_eq!(all.len(), 1, "corrupt doc skipped, fragment files ignored");
        assert_eq!(all[0].id, "doc-good");
    }

    #[test]
    fn delete_doc_removes_file_and_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_doc(&h, &sample_doc("doc-1")).unwrap();
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
        write_doc(&h, &sample_doc("doc-1")).unwrap();
        assert!(docs_dir(&h).exists());
    }

    #[test]
    fn write_then_read_fragment_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        let meta = sample_fragment("doc-1", 1, Some("アーキテクチャ"), 1);
        write_fragment(&h, &meta, "## アーキテクチャ\n\nBody text.\n").unwrap();

        let (back_meta, back_body) = read_fragment(&h, "doc-1", 1)
            .unwrap()
            .expect("fragment exists");
        assert_eq!(back_meta.doc_id, "doc-1");
        assert_eq!(back_meta.seq, 1);
        assert_eq!(back_meta.heading.as_deref(), Some("アーキテクチャ"));
        assert_eq!(back_meta.level, 1);
        assert_eq!(back_body, "## アーキテクチャ\n\nBody text.\n");
    }

    #[test]
    fn read_fragment_missing_pair_is_none() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        assert!(read_fragment(&h, "doc-1", 0).unwrap().is_none());

        // Only metadata written, body missing -> still None (pair required).
        let meta = sample_fragment("doc-1", 0, None, 0);
        ensure_docs_dir(&h).unwrap();
        let meta_path = fragment_meta_path(&h, "doc-1", 0);
        std::fs::write(&meta_path, serde_json::to_string_pretty(&meta).unwrap()).unwrap();
        assert!(read_fragment(&h, "doc-1", 0).unwrap().is_none());
    }

    #[test]
    fn read_all_fragments_seq_order_and_roundtrip_via_reassemble() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        // Write out of order to prove the reader sorts by seq, not write order.
        write_fragment(
            &h,
            &sample_fragment("doc-1", 2, Some("エラー処理"), 2),
            "## エラー処理\n\nHandle errors.\n",
        )
        .unwrap();
        write_fragment(&h, &sample_fragment("doc-1", 0, None, 0), "Preamble.\n\n").unwrap();
        write_fragment(
            &h,
            &sample_fragment("doc-1", 1, Some("アーキテクチャ"), 1),
            "## アーキテクチャ\n\nBody text.\n",
        )
        .unwrap();

        let fragments = read_all_fragments(&h, "doc-1").unwrap();
        let seqs: Vec<usize> = fragments.iter().map(|(m, _)| m.seq).collect();
        assert_eq!(seqs, vec![0, 1, 2]);

        let bodies: Vec<&str> = fragments.iter().map(|(_, b)| b.as_str()).collect();
        let reassembled = reassemble::reassemble(&bodies);
        assert_eq!(
            reassembled,
            "Preamble.\n\n## アーキテクチャ\n\nBody text.\n## エラー処理\n\nHandle errors.\n"
        );
    }

    #[test]
    fn read_all_fragments_scoped_to_doc_id_and_skips_incomplete_pair() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_fragment(&h, &sample_fragment("doc-a", 0, None, 0), "a-body").unwrap();
        write_fragment(&h, &sample_fragment("doc-b", 0, None, 0), "b-body").unwrap();
        // Drop an orphaned metadata file for doc-a seq 1 with no matching .md.
        ensure_docs_dir(&h).unwrap();
        let orphan_meta = sample_fragment("doc-a", 1, Some("Orphan"), 1);
        std::fs::write(
            fragment_meta_path(&h, "doc-a", 1),
            serde_json::to_string_pretty(&orphan_meta).unwrap(),
        )
        .unwrap();

        let a_fragments = read_all_fragments(&h, "doc-a").unwrap();
        assert_eq!(
            a_fragments.len(),
            1,
            "orphaned metadata-only pair is skipped"
        );
        assert_eq!(a_fragments[0].0.seq, 0);
        assert_eq!(a_fragments[0].1, "a-body");

        let b_fragments = read_all_fragments(&h, "doc-b").unwrap();
        assert_eq!(b_fragments.len(), 1);
        assert_eq!(b_fragments[0].1, "b-body");
    }

    #[test]
    fn read_all_fragments_missing_dir_is_empty() {
        let tmp = TempDir::new().unwrap();
        let h = tmp.path().join(".handoff");
        std::fs::create_dir_all(&h).unwrap();
        assert!(read_all_fragments(&h, "doc-1").unwrap().is_empty());
    }

    #[test]
    fn delete_fragment_removes_both_files_and_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_fragment(&h, &sample_fragment("doc-1", 0, None, 0), "body").unwrap();
        assert!(delete_fragment(&h, "doc-1", 0).unwrap());
        assert!(read_fragment(&h, "doc-1", 0).unwrap().is_none());
        assert!(!delete_fragment(&h, "doc-1", 0).unwrap());
    }

    #[test]
    fn lazy_dir_creation_on_first_fragment_write() {
        let tmp = TempDir::new().unwrap();
        let h = tmp.path().join(".handoff");
        std::fs::create_dir_all(&h).unwrap();
        assert!(!docs_dir(&h).exists());
        write_fragment(&h, &sample_fragment("doc-1", 0, None, 0), "body").unwrap();
        assert!(docs_dir(&h).exists());
    }
}
