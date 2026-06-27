//! Persistence for project memories under `.handoff/memory/`.
//!
//! Layout:
//!
//! ```text
//! .handoff/memory/
//!   m-YYYYMMDD-HHMMSS-NNNNNN.json   # one memory per file
//!   injected/
//!     <hook_session_id>.json        # per-session "already injected" sidecar (P2)
//! ```
//!
//! All writes go through [`crate::storage::atomic_write`] so the VSCode
//! extension (which reads `.handoff/`) never observes a torn file. Reads are
//! lenient: a single corrupt or unparseable file is skipped, not fatal, so one
//! bad memory can't break the whole feature.

pub mod injected;
pub mod model;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub use injected::{
    gc_injected_sets, injected_path, read_injected_set, write_injected_set, InjectedSet,
};
pub use model::{is_valid_memory_kind, MemoryEntry, VALID_MEMORY_KINDS};

/// Path to the `memory/` directory inside a `.handoff/` dir.
pub fn memory_dir(handoff_dir: &Path) -> PathBuf {
    handoff_dir.join("memory")
}

/// Generate a fresh memory id (`m-YYYYMMDD-HHMMSS-NNNNNN`) from the current time.
pub fn new_memory_id() -> String {
    let now = chrono::Utc::now();
    format!("m-{}", now.format("%Y%m%d-%H%M%S-%6f"))
}

/// Current time as an RFC3339 string (helper so handlers don't import chrono).
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Ensure `memory/` exists, creating it lazily. This is what makes the feature
/// backward-compatible with projects initialized before v0.13.0: they never had
/// a `memory/` dir, and the first `memory_save` creates it.
pub fn ensure_memory_dir(handoff_dir: &Path) -> Result<PathBuf> {
    let dir = memory_dir(handoff_dir);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create memory dir: {}", dir.display()))?;
    Ok(dir)
}

/// Write a memory to `memory/<id>.json` atomically.
pub fn write_memory(handoff_dir: &Path, entry: &MemoryEntry) -> Result<PathBuf> {
    let dir = ensure_memory_dir(handoff_dir)?;
    let file_path = dir.join(format!("{}.json", entry.id));
    let content = serde_json::to_string_pretty(entry).context("Failed to serialize memory")?;
    crate::storage::atomic_write(&file_path, content.as_bytes())
        .with_context(|| format!("Failed to write memory: {}", file_path.display()))?;
    Ok(file_path)
}

/// Read every memory in `memory/`, skipping any file that fails to parse. The
/// `injected/` subdirectory and any non-`.json` files are ignored. Returns an
/// empty vec when the directory does not exist (uninitialized projects).
pub fn read_all_memories(handoff_dir: &Path) -> Result<Vec<MemoryEntry>> {
    let dir = memory_dir(handoff_dir);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .with_context(|| format!("Failed to read memory dir: {}", dir.display()))?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());

    let mut memories = Vec::new();
    for entry in entries {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".json") || name.starts_with('.') {
            continue;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if let Ok(mem) = serde_json::from_str::<MemoryEntry>(&content) {
            memories.push(mem);
        }
        // Unparseable file: skip silently (lenient read).
    }
    Ok(memories)
}

/// Read one memory by id. Matches the full id first, then falls back to a unique
/// prefix match (mirrors the referral lookup ergonomics). Returns `Ok(None)`
/// when nothing matches, and errors on an ambiguous prefix.
pub fn read_memory_by_id(handoff_dir: &Path, id: &str) -> Result<Option<MemoryEntry>> {
    let memories = read_all_memories(handoff_dir)?;
    let mut prefix_matches: Vec<MemoryEntry> = Vec::new();
    for mem in memories {
        if mem.id == id {
            return Ok(Some(mem));
        }
        if mem.id.starts_with(id) {
            prefix_matches.push(mem);
        }
    }
    match prefix_matches.len() {
        0 => Ok(None),
        1 => Ok(prefix_matches.into_iter().next()),
        n => {
            anyhow::bail!("Ambiguous memory id prefix '{id}' matches {n} memories; use the full id")
        }
    }
}

/// Delete a memory file by exact id. Returns `Ok(false)` when the file does not
/// exist.
pub fn delete_memory(handoff_dir: &Path, id: &str) -> Result<bool> {
    let path = memory_dir(handoff_dir).join(format!("{id}.json"));
    if !path.exists() {
        return Ok(false);
    }
    std::fs::remove_file(&path)
        .with_context(|| format!("Failed to delete memory: {}", path.display()))?;
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

    fn sample(id: &str, text: &str) -> MemoryEntry {
        MemoryEntry::new(
            id.to_string(),
            text.to_string(),
            "lesson".to_string(),
            vec![],
            vec![],
            now_rfc3339(),
        )
    }

    #[test]
    fn write_then_read_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        let mem = sample("m-1", "always use atomic_write");
        write_memory(&h, &mem).unwrap();

        let all = read_all_memories(&h).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, "m-1");
        assert_eq!(all[0].text, "always use atomic_write");
        assert_eq!(all[0].content_hash, mem.content_hash);
    }

    #[test]
    fn read_missing_dir_is_empty() {
        let tmp = TempDir::new().unwrap();
        let h = tmp.path().join(".handoff");
        std::fs::create_dir_all(&h).unwrap();
        assert!(read_all_memories(&h).unwrap().is_empty());
    }

    #[test]
    fn lenient_read_skips_corrupt_file() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_memory(&h, &sample("m-1", "good")).unwrap();
        // Drop a non-JSON file into memory/.
        std::fs::write(memory_dir(&h).join("m-bad.json"), b"{not json").unwrap();

        let all = read_all_memories(&h).unwrap();
        assert_eq!(all.len(), 1, "corrupt file must be skipped, good one kept");
    }

    #[test]
    fn read_by_id_exact_and_prefix() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_memory(&h, &sample("m-20260627-aaa", "a")).unwrap();
        write_memory(&h, &sample("m-20260627-bbb", "b")).unwrap();

        assert!(read_memory_by_id(&h, "m-20260627-aaa").unwrap().is_some());
        // Unique prefix
        let p = read_memory_by_id(&h, "m-20260627-a").unwrap();
        assert_eq!(p.unwrap().id, "m-20260627-aaa");
    }

    #[test]
    fn read_by_id_ambiguous_prefix_errors() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_memory(&h, &sample("m-x1", "a")).unwrap();
        write_memory(&h, &sample("m-x2", "b")).unwrap();
        assert!(read_memory_by_id(&h, "m-x").is_err());
    }

    #[test]
    fn delete_removes_file() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        write_memory(&h, &sample("m-1", "a")).unwrap();
        assert!(delete_memory(&h, "m-1").unwrap());
        assert!(read_all_memories(&h).unwrap().is_empty());
        // Second delete reports not-found.
        assert!(!delete_memory(&h, "m-1").unwrap());
    }

    #[test]
    fn lazy_dir_creation() {
        let tmp = TempDir::new().unwrap();
        let h = tmp.path().join(".handoff");
        std::fs::create_dir_all(&h).unwrap();
        assert!(!memory_dir(&h).exists());
        write_memory(&h, &sample("m-1", "a")).unwrap();
        assert!(memory_dir(&h).exists());
    }
}
