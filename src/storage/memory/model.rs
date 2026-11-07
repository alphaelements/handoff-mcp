use serde::{Deserialize, Serialize};

/// Current memory schema version. Bump when `MemoryEntry` changes shape in a way
/// that needs migration handling on read.
///
/// v1 → v2: added `keywords` field (defaults to empty on read).
pub const MEMORY_SCHEMA_VERSION: u32 = 2;

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
///
/// The file only changes through an explicit operation (save / merge / delete).
/// v0.47.2 and earlier also stored `hit_count` / `last_referenced_at` and
/// rewrote the file on every injection; those fields no longer exist. Files that
/// still carry them load fine (serde ignores unknown keys) and the keys are
/// dropped the next time the file is explicitly rewritten, or in bulk by
/// `handoff-mcp memory strip-usage-stats`.
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
    /// Subject keywords — nouns, technical terms, proper nouns that identify what
    /// this memory is *about*. Used for BM25 matching with boosted weight.
    /// Distinct from `tags` (classification labels) and `scope_paths` (file
    /// prefixes). Populated by the AI at save time; defaults to empty for v1
    /// memories.
    #[serde(default)]
    pub keywords: Vec<String>,
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
        keywords: Vec<String>,
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
            keywords,
            scope_paths,
            content_hash,
            created_at: now.clone(),
            updated_at: now,
            superseded_ids: Vec::new(),
        }
    }

    /// Plain-text index for Jaccard similarity: body + tags + keywords.
    pub fn index_text(&self) -> String {
        let mut parts = vec![self.text.clone()];
        if !self.tags.is_empty() {
            parts.push(self.tags.join(" "));
        }
        if !self.keywords.is_empty() {
            parts.push(self.keywords.join(" "));
        }
        parts.join(" ")
    }

    /// Weighted tokens for `Corpus::build_weighted_tokens`. Body + tags are
    /// tokenized at default weight; keywords are added with weight 2.0
    /// (doc-side TF boost), replacing the old 2x-concatenation hack.
    pub fn index_weighted_tokens(&self) -> Vec<lexsim::WeightedToken> {
        let mut base = vec![self.text.clone()];
        if !self.tags.is_empty() {
            base.push(self.tags.join(" "));
        }
        let mut tokens = lexsim::tokenize_weighted(&base.join(" "));
        for kw in &self.keywords {
            for mut wt in lexsim::tokenize_weighted(kw) {
                wt.weight *= 2.0;
                tokens.push(wt);
            }
        }
        tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_usage_stat_fields_are_ignored_on_load_and_never_written() {
        let legacy = serde_json::json!({
            "version": 2, "id": "m-1", "text": "t", "kind": "rule",
            "tags": [], "keywords": [], "scope_paths": [],
            "content_hash": "h",
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-02T00:00:00Z",
            "hit_count": 87, "last_referenced_at": "2026-05-01T00:00:00Z",
            "superseded_ids": []
        });
        let entry: MemoryEntry = serde_json::from_value(legacy).expect("legacy file loads");
        assert_eq!(entry.updated_at, "2026-01-02T00:00:00Z");
        let back = serde_json::to_value(&entry).unwrap();
        assert!(back.get("hit_count").is_none());
        assert!(back.get("last_referenced_at").is_none());
    }
}
