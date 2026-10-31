//! `CodeRef.content_hash` support (wiki/430 §3.1, REQ-LAG-002): SHA-256 of
//! the whole file a `CodeRef` points at, recorded when the ref is set and
//! compared again at trace-load time to detect that the code changed under a
//! spec item (`code_drift`).
//!
//! This is the only place that hashes referenced source files. `trace_lint`
//! / `trace_suspect` stay pure (D1): the loader calls
//! [`collect_code_drift_items`] once and hands the result over as
//! `TraceInput::code_drift_items`.
//!
//! A `CodeRef.path` is project-root-relative. An absolute path or one with a
//! `..` component is never read (it could point outside the project), so it
//! simply gets no hash and can never drift.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use super::model::{CodeRef, DocMetadata};

/// What a referenced file looks like right now.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FileState {
    Hash(String),
    /// The file does not exist (or `path` is not a regular, in-project file
    /// path at all — a module path such as `foo::bar` lands here too).
    Missing,
    /// The file exists but could not be read (permissions, I/O error) — the
    /// hash is unknowable, which is not evidence of a change.
    Unreadable,
}

/// The project root a `CodeRef.path` is relative to: the parent of the
/// `.handoff/` directory. A `.handoff/` with no parent (a filesystem root)
/// falls back to itself, which just makes every ref resolve to `Missing`.
pub fn project_root(handoff_dir: &Path) -> &Path {
    handoff_dir.parent().unwrap_or(handoff_dir)
}

/// Resolves `path` below `root`, or `None` when it is absolute or contains a
/// `..` component.
fn resolve(root: &Path, path: &str) -> Option<PathBuf> {
    let rel = Path::new(path);
    let confined = !path.is_empty()
        && rel
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    confined.then(|| root.join(rel))
}

fn file_state(root: &Path, path: &str) -> FileState {
    let Some(full) = resolve(root, path) else {
        return FileState::Missing;
    };
    match std::fs::metadata(&full) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return FileState::Missing,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return FileState::Missing,
        Err(_) => return FileState::Unreadable,
    }
    match std::fs::read(&full) {
        Ok(bytes) => FileState::Hash(format!("{:x}", Sha256::digest(&bytes))),
        Err(_) => FileState::Unreadable,
    }
}

/// SHA-256 (lowercase hex) of the file `path` names below `root`; `None`
/// when there is no such readable file (REQ-LAG-020 AC2: not an error).
pub fn hash_code_file(root: &Path, path: &str) -> Option<String> {
    match file_state(root, path) {
        FileState::Hash(h) => Some(h),
        FileState::Missing | FileState::Unreadable => None,
    }
}

/// (Re)records `content_hash` on every ref from the file's current content,
/// replacing whatever the caller supplied: the hash is always derived here,
/// never trusted from input. A ref whose file does not exist ends up `None`.
pub fn stamp_content_hashes(root: &Path, refs: &mut [CodeRef]) {
    for r in refs {
        r.content_hash = hash_code_file(root, &r.path);
    }
}

/// Whether `r` drifted: it has a recorded hash and the file now has another
/// content (or is gone). `content_hash == None` (pre-feature data) never
/// drifts, and an unreadable file is not treated as changed.
fn ref_drifted(root: &Path, r: &CodeRef) -> bool {
    let Some(recorded) = &r.content_hash else {
        return false;
    };
    match file_state(root, &r.path) {
        FileState::Hash(current) => &current != recorded,
        FileState::Missing => true,
        FileState::Unreadable => false,
    }
}

/// stable_ids of every sub-item (across `docs`) with at least one
/// `impl_refs`/`test_refs` entry that drifted. Each distinct path is hashed
/// once per call.
pub fn collect_code_drift_items(root: &Path, docs: &[DocMetadata]) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut verdict: std::collections::HashMap<(&str, &str), bool> =
        std::collections::HashMap::new();
    for sub in docs
        .iter()
        .filter_map(|d| d.verification.as_ref())
        .flat_map(|v| &v.items)
        .flat_map(|i| &i.sub_items)
    {
        let Some(id) = &sub.stable_id else {
            continue;
        };
        let drifted = sub
            .impl_refs
            .iter()
            .chain(&sub.test_refs)
            .filter_map(|r| r.content_hash.as_deref().map(|h| (r, h)))
            .any(|(r, h)| {
                *verdict
                    .entry((r.path.as_str(), h))
                    .or_insert_with(|| ref_drifted(root, r))
            });
        if drifted {
            out.insert(id.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::docs::model::{SubItem, Verification, VerificationItem};

    fn code_ref(path: &str, hash: Option<&str>) -> CodeRef {
        CodeRef {
            path: path.to_string(),
            lines: None,
            label: None,
            content_hash: hash.map(str::to_string),
        }
    }

    fn doc_with(id: &str, impl_refs: Vec<CodeRef>, test_refs: Vec<CodeRef>) -> DocMetadata {
        let mut doc = DocMetadata::new(
            "doc-1".to_string(),
            "slug".to_string(),
            "T".to_string(),
            "spec".to_string(),
            "2026-01-01T00:00:00Z".to_string(),
        );
        // Built from JSON so the many defaulted fields stay out of the test.
        let sub: SubItem = serde_json::from_value(serde_json::json!({
            "index": 0,
            "description": "d",
            "status": "pending",
            "stable_id": id,
            "impl_refs": impl_refs,
            "test_refs": test_refs,
        }))
        .unwrap();
        let item: VerificationItem = serde_json::from_value(serde_json::json!({
            "heading": "h",
            "status": "pending",
            "sub_items": [sub],
        }))
        .unwrap();
        doc.verification = Some(Verification {
            status: "pending".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            items: vec![item],
        });
        doc
    }

    #[test]
    fn hash_is_the_sha256_hex_of_the_file_content() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "abc").unwrap();

        assert_eq!(
            hash_code_file(dir.path(), "a.rs").as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
    }

    #[test]
    fn missing_file_directory_and_escaping_paths_have_no_hash() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("a.rs"), "abc").unwrap();
        let nested = dir.path().join("proj");
        std::fs::create_dir(&nested).unwrap();

        assert_eq!(hash_code_file(dir.path(), "nope.rs"), None);
        assert_eq!(hash_code_file(dir.path(), "sub"), None);
        assert_eq!(hash_code_file(dir.path(), ""), None);
        assert_eq!(hash_code_file(dir.path(), "tests::foo::bar"), None);
        assert_eq!(hash_code_file(&nested, "../a.rs"), None);
        let abs = dir.path().join("a.rs");
        assert_eq!(hash_code_file(&nested, abs.to_str().unwrap()), None);
    }

    #[test]
    fn stamp_overwrites_supplied_hash_and_clears_it_for_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "abc").unwrap();
        let mut refs = vec![
            code_ref("a.rs", Some("caller-supplied")),
            code_ref("gone.rs", Some("caller-supplied")),
        ];

        stamp_content_hashes(dir.path(), &mut refs);

        assert_eq!(
            refs[0].content_hash.as_deref(),
            hash_code_file(dir.path(), "a.rs").as_deref()
        );
        assert!(refs[0].content_hash.is_some());
        assert_eq!(refs[1].content_hash, None);
    }

    #[test]
    fn drift_detected_after_the_file_changes_not_before() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.rs");
        std::fs::write(&file, "v1").unwrap();
        let hash = hash_code_file(dir.path(), "a.rs");
        let docs = vec![doc_with(
            "SPEC-001",
            vec![code_ref("a.rs", hash.as_deref())],
            vec![],
        )];

        assert!(collect_code_drift_items(dir.path(), &docs).is_empty());

        std::fs::write(&file, "v2").unwrap();
        let drift = collect_code_drift_items(dir.path(), &docs);
        assert_eq!(drift, HashSet::from(["SPEC-001".to_string()]));
    }

    #[test]
    fn drift_is_detected_through_test_refs_and_a_deleted_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("t.rs");
        std::fs::write(&file, "v1").unwrap();
        let hash = hash_code_file(dir.path(), "t.rs");
        let docs = vec![doc_with(
            "SPEC-002",
            vec![],
            vec![code_ref("t.rs", hash.as_deref())],
        )];

        std::fs::remove_file(&file).unwrap();

        assert!(collect_code_drift_items(dir.path(), &docs).contains("SPEC-002"));
    }

    #[test]
    fn a_ref_without_a_recorded_hash_never_drifts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "v1").unwrap();
        let docs = vec![doc_with("SPEC-003", vec![code_ref("a.rs", None)], vec![])];

        std::fs::write(dir.path().join("a.rs"), "v2").unwrap();

        assert!(collect_code_drift_items(dir.path(), &docs).is_empty());
    }

    #[test]
    fn project_root_is_the_parent_of_the_handoff_dir() {
        assert_eq!(
            project_root(Path::new("/work/proj/.handoff")),
            Path::new("/work/proj")
        );
    }
}
