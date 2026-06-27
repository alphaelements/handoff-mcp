use serde::{Deserialize, Serialize};

/// Current memory schema version. Bump when `MemoryEntry` changes shape in a way
/// that needs migration handling on read.
pub const MEMORY_SCHEMA_VERSION: u32 = 1;

/// Valid `kind` values for a memory. Free-form text is rejected so the field
/// stays a small, queryable enumeration.
pub const VALID_MEMORY_KINDS: &[&str] = &["lesson", "rule", "convention", "gotcha"];

/// Returns true if `kind` is one of the accepted memory kinds.
pub fn is_valid_memory_kind(kind: &str) -> bool {
    VALID_MEMORY_KINDS.contains(&kind)
}

/// A single persisted memory: a long-lived, cross-session piece of project
/// knowledge ("lesson / rule / convention / gotcha"). One file per memory under
/// `.handoff/memory/`.
///
/// Token sets are intentionally **not** stored — they are recomputed from `text`
/// on every read so the index can never drift from the body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    /// Schema version (= [`MEMORY_SCHEMA_VERSION`]).
    pub version: u32,
    /// Stable id: `m-YYYYMMDD-HHMMSS-NNNNNN`.
    pub id: String,
    /// The memory body (multilingual).
    pub text: String,
    /// One of [`VALID_MEMORY_KINDS`].
    pub kind: String,
    /// Free-form tags; also fed into the similarity index.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Path prefixes this memory applies to (e.g. `src/storage/`). A query whose
    /// file paths start with one of these gets a relevance boost.
    #[serde(default)]
    pub scope_paths: Vec<String>,
    /// FNV-1a hash of the canonical (tokenized) text. Drives exact-duplicate
    /// detection and per-session re-injection tracking.
    pub content_hash: String,
    /// RFC3339 creation timestamp.
    pub created_at: String,
    /// RFC3339 last-update timestamp.
    pub updated_at: String,
    /// RFC3339 timestamp of the last time this memory was injected into a
    /// session, if ever.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_referenced_at: Option<String>,
    /// Number of times this memory has been injected.
    #[serde(default)]
    pub hit_count: u64,
    /// Ids of memories merged into this one (audit trail for AI-driven merges).
    #[serde(default)]
    pub superseded_ids: Vec<String>,
}

impl MemoryEntry {
    /// Build a new entry with timestamps and content hash filled in. `now` is an
    /// RFC3339 timestamp supplied by the caller (keeps this module clock-free
    /// and testable).
    pub fn new(
        id: String,
        text: String,
        kind: String,
        tags: Vec<String>,
        scope_paths: Vec<String>,
        now: String,
    ) -> Self {
        let content_hash = lexsim::content_hash(&text);
        MemoryEntry {
            version: MEMORY_SCHEMA_VERSION,
            id,
            text,
            kind,
            tags,
            scope_paths,
            content_hash,
            created_at: now.clone(),
            updated_at: now,
            last_referenced_at: None,
            hit_count: 0,
            superseded_ids: Vec::new(),
        }
    }

    /// The text used for similarity: body plus tags (tags carry intent that may
    /// not appear verbatim in the body).
    pub fn index_text(&self) -> String {
        if self.tags.is_empty() {
            self.text.clone()
        } else {
            format!("{} {}", self.text, self.tags.join(" "))
        }
    }
}
