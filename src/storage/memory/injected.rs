//! Per-session "already injected" sidecars under `.handoff/memory/injected/`.
//!
//! ```text
//! .handoff/memory/injected/
//!   <hook_session_id>.json   # { session_id, updated_at, injected: { id -> hash } }
//! ```
//!
//! The sidecar is keyed by the **hook's `session_id`** (not the timestamped
//! session file): the hook never sees a session filename, and the id is stable
//! across compaction/resume but changes on `/clear` — which gives us a free
//! per-conversation reset.
//!
//! A memory's value is its `content_hash`. `memory_query` skips a memory whose
//! id is present with the *same* hash (already injected this session) but
//! re-injects when the hash differs (the memory was edited since). All writes go
//! through [`crate::storage::atomic_write`] so a concurrent reader never sees a
//! torn file; reads are lenient (a corrupt sidecar is treated as empty).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::memory_dir;

/// Current sidecar schema version.
pub const INJECTED_SCHEMA_VERSION: u32 = 1;

/// The set of memories already injected into one hook session.
///
/// `injected` maps memory id → the `content_hash` that was injected, so an edited
/// memory (new hash) is re-injected while an unchanged one is skipped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InjectedSet {
    /// Schema version (= [`INJECTED_SCHEMA_VERSION`]).
    #[serde(default = "default_version")]
    pub version: u32,
    /// The hook session id this sidecar belongs to.
    pub session_id: String,
    /// RFC3339 timestamp of the last update.
    pub updated_at: String,
    /// memory id → injected `content_hash`.
    #[serde(default)]
    pub injected: BTreeMap<String, String>,
}

fn default_version() -> u32 {
    INJECTED_SCHEMA_VERSION
}

impl InjectedSet {
    /// A fresh, empty set for `session_id`.
    pub fn new(session_id: String, now: String) -> Self {
        InjectedSet {
            version: INJECTED_SCHEMA_VERSION,
            session_id,
            updated_at: now,
            injected: BTreeMap::new(),
        }
    }

    /// True if `id` was already injected this session with the *same* hash. A
    /// differing hash (edited memory) returns false so it is re-injected.
    pub fn already_injected(&self, id: &str, content_hash: &str) -> bool {
        self.injected.get(id).map(String::as_str) == Some(content_hash)
    }

    /// Record `id`→`content_hash` as injected. Returns true if this changed the
    /// set (new id or a different hash), false if it was already present.
    pub fn mark(&mut self, id: &str, content_hash: &str) -> bool {
        match self.injected.get(id) {
            Some(existing) if existing == content_hash => false,
            _ => {
                self.injected
                    .insert(id.to_string(), content_hash.to_string());
                true
            }
        }
    }
}

/// Path to the `injected/` directory inside a `memory/` dir.
pub fn injected_dir(handoff_dir: &Path) -> PathBuf {
    memory_dir(handoff_dir).join("injected")
}

/// Sanitize a hook session id into a safe, **collision-free** single-path
/// file stem.
///
/// Session ids are opaque strings from the hook environment; we must never let
/// one escape `injected/` (path traversal) or collide with the temp-file naming
/// in `atomic_write`. Two requirements:
///
/// 1. **Safety**: the stem is a single path component — no `/`, `\`, `..`, or
///    leading `.` can survive (each unsafe char becomes `_`, leading dots are
///    dropped).
/// 2. **Uniqueness**: the readable part alone is lossy (`a/b` and `a_b` would
///    map to the same `a_b`), so we always append `-<fnv1a(raw id)>`. Distinct
///    raw ids therefore never share a sidecar, while the stem stays
///    human-recognizable. The hash is over the *raw* id, so it disambiguates
///    even when the readable prefix is identical or empty.
fn sanitize_session_id(session_id: &str) -> String {
    let mut out = String::with_capacity(session_id.len());
    for ch in session_id.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-') {
            out.push(ch);
        } else if ch == '.' {
            // Keep dots only when not leading (avoid hidden files / `..`).
            if !out.is_empty() {
                out.push('.');
            }
        } else {
            out.push('_');
        }
    }
    // Trim trailing dots so we never produce `name.`, and cap the readable part.
    let trimmed = out.trim_end_matches('.');
    let prefix: String = trimmed.chars().take(96).collect();
    let prefix = if prefix.is_empty() { "anon" } else { &prefix };
    // Always disambiguate with a hash of the RAW id (std-only FNV-1a).
    format!("{prefix}-{}", lexsim::fnv1a_hex(session_id.as_bytes()))
}

/// Path to one session's sidecar file.
pub fn injected_path(handoff_dir: &Path, session_id: &str) -> PathBuf {
    injected_dir(handoff_dir).join(format!("{}.json", sanitize_session_id(session_id)))
}

/// Read the injected set for `session_id`. Returns a fresh empty set when the
/// file does not exist or is unparseable (lenient: a corrupt sidecar must not
/// break injection — at worst a memory is shown twice).
pub fn read_injected_set(handoff_dir: &Path, session_id: &str, now: &str) -> InjectedSet {
    let path = injected_path(handoff_dir, session_id);
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str::<InjectedSet>(&content)
            .unwrap_or_else(|_| InjectedSet::new(session_id.to_string(), now.to_string())),
        Err(_) => InjectedSet::new(session_id.to_string(), now.to_string()),
    }
}

/// Write a session's injected set atomically, creating `injected/` lazily.
pub fn write_injected_set(handoff_dir: &Path, set: &InjectedSet) -> Result<PathBuf> {
    let dir = injected_dir(handoff_dir);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create injected dir: {}", dir.display()))?;
    let path = injected_path(handoff_dir, &set.session_id);
    let content = serde_json::to_string_pretty(set).context("Failed to serialize injected set")?;
    crate::storage::atomic_write(&path, content.as_bytes())
        .with_context(|| format!("Failed to write injected set: {}", path.display()))?;
    Ok(path)
}

/// Garbage-collect sidecars whose `updated_at` is older than `max_age_days`.
/// Returns the number of files removed. A sidecar with an unparseable
/// `updated_at` is left alone (we can't tell its age). `now` is supplied so the
/// caller controls the clock (testable).
pub fn gc_injected_sets(
    handoff_dir: &Path,
    max_age_days: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<usize> {
    let dir = injected_dir(handoff_dir);
    if !dir.exists() {
        return Ok(0);
    }
    let cutoff = now - chrono::Duration::days(max_age_days);
    let mut removed = 0usize;
    for entry in std::fs::read_dir(&dir)
        .with_context(|| format!("Failed to read injected dir: {}", dir.display()))?
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".json") || name.starts_with('.') {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(set) = serde_json::from_str::<InjectedSet>(&content) else {
            continue; // unparseable → leave it (can't determine age)
        };
        let Ok(updated) = chrono::DateTime::parse_from_rfc3339(&set.updated_at) else {
            continue; // bad timestamp → leave it
        };
        if updated.with_timezone(&chrono::Utc) < cutoff && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
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

    #[test]
    fn read_missing_is_empty() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        let set = read_injected_set(&h, "sess-1", "2026-06-27T00:00:00Z");
        assert_eq!(set.session_id, "sess-1");
        assert!(set.injected.is_empty());
    }

    #[test]
    fn write_then_read_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        let mut set = InjectedSet::new("sess-1".to_string(), "2026-06-27T00:00:00Z".to_string());
        assert!(set.mark("m-1", "hashA"));
        write_injected_set(&h, &set).unwrap();

        let back = read_injected_set(&h, "sess-1", "now");
        assert_eq!(back.injected.get("m-1").map(String::as_str), Some("hashA"));
    }

    #[test]
    fn already_injected_same_hash_true_diff_hash_false() {
        let mut set = InjectedSet::new("s".to_string(), "now".to_string());
        set.mark("m-1", "hashA");
        assert!(set.already_injected("m-1", "hashA"));
        assert!(!set.already_injected("m-1", "hashB")); // edited → re-inject
        assert!(!set.already_injected("m-2", "hashA")); // never injected
    }

    #[test]
    fn mark_reports_change() {
        let mut set = InjectedSet::new("s".to_string(), "now".to_string());
        assert!(set.mark("m-1", "h1")); // new id
        assert!(!set.mark("m-1", "h1")); // unchanged
        assert!(set.mark("m-1", "h2")); // hash changed
    }

    #[test]
    fn sessions_are_isolated() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        let mut a = InjectedSet::new("A".to_string(), "now".to_string());
        a.mark("m-1", "h");
        write_injected_set(&h, &a).unwrap();

        let b = read_injected_set(&h, "B", "now");
        assert!(
            b.injected.is_empty(),
            "session B must not see session A's set"
        );
    }

    #[test]
    fn sanitize_blocks_traversal_and_separators() {
        // Path separators are neutralized so the result is always a single flat
        // filename component — no traversal possible.
        for evil in [
            "../../etc/passwd",
            "a/b\\c",
            "..",
            "../",
            "foo/../bar",
            ".hidden",
            "",
            "...",
        ] {
            let s = sanitize_session_id(evil);
            assert!(!s.contains('/'), "{evil:?} -> {s:?} still has /");
            assert!(!s.contains('\\'), "{evil:?} -> {s:?} still has \\");
            assert_ne!(s, "..", "{evil:?} -> {s:?} is parent ref");
            assert!(!s.starts_with('.'), "{evil:?} -> {s:?} is hidden");
            assert!(!s.is_empty(), "{evil:?} -> empty stem");
        }
        // The readable prefix is preserved; a hash suffix is always appended.
        assert!(sanitize_session_id("3f9a-1b2c-4d5e").starts_with("3f9a-1b2c-4d5e-"));
        assert!(sanitize_session_id(".hidden").starts_with("hidden-"));
    }

    #[test]
    fn sanitize_is_collision_free_for_distinct_ids() {
        // Lossy normalization alone would collide these; the raw-id hash suffix
        // keeps every distinct id on its own sidecar.
        let pairs = [("a/b", "a_b"), ("...", ""), ("a.b", "a_b"), ("X/Y", "X.Y")];
        for (x, y) in pairs {
            assert_ne!(
                sanitize_session_id(x),
                sanitize_session_id(y),
                "{x:?} and {y:?} must not share a sidecar"
            );
        }
        // Same id is stable (round-trips to the same file).
        assert_eq!(sanitize_session_id("sess-A"), sanitize_session_id("sess-A"));
    }

    #[test]
    fn lenient_read_treats_corrupt_as_empty() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        let dir = injected_dir(&h);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(injected_path(&h, "sess-1"), b"{not json").unwrap();

        let set = read_injected_set(&h, "sess-1", "now");
        assert!(set.injected.is_empty());
    }

    #[test]
    fn gc_removes_old_keeps_fresh() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        let old = InjectedSet::new("old".to_string(), "2026-01-01T00:00:00Z".to_string());
        let fresh = InjectedSet::new("fresh".to_string(), "2026-06-27T00:00:00Z".to_string());
        write_injected_set(&h, &old).unwrap();
        write_injected_set(&h, &fresh).unwrap();

        let now = chrono::DateTime::parse_from_rfc3339("2026-06-28T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let removed = gc_injected_sets(&h, 14, now).unwrap();
        assert_eq!(removed, 1, "only the January sidecar is past 14 days");
        assert!(read_injected_set(&h, "old", "now").injected.is_empty());
        assert!(injected_path(&h, "fresh").exists());
    }

    #[test]
    fn gc_on_missing_dir_is_zero() {
        let tmp = TempDir::new().unwrap();
        let h = handoff(&tmp);
        let now = chrono::Utc::now();
        assert_eq!(gc_injected_sets(&h, 14, now).unwrap(), 0);
    }
}
