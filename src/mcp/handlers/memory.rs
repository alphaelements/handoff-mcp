//! MCP handlers for the memory feature (save / query / delete).
//!
//! All three return a **parseable JSON string** as their content text, so both
//! the wrapper path and the Claude Code `mcp_tool` hook path can consume them
//! with the same JSON parse. `memory_query` supports per-session diff injection
//! via the `injected/` sidecar (P2); `memory_cleanup` lands in P3.

use anyhow::Result;
use serde_json::{json, Value};

use super::resolve_project_dir;
use crate::storage::ensure_handoff_exists;
use crate::storage::memory::{
    delete_memory, is_valid_memory_kind, new_memory_id, now_rfc3339, read_all_memories,
    read_injected_set, read_memory_by_id, write_injected_set, write_memory, MemoryEntry,
    VALID_MEMORY_KINDS,
};

/// Jaccard threshold above which a save is treated as a near-duplicate and
/// returned as a `conflict` for the AI to merge. P4 moves this into config; P1
/// keeps it a constant matching the spec default.
const MEMORY_DUP_THRESHOLD: f64 = 0.72;

/// BM25 relevance floor for `memory_query`. Conservative default; P4 makes it
/// configurable. Scores below this are not returned.
const MEMORY_QUERY_MIN_SCORE: f64 = 0.5;

/// Default and maximum number of memories returned by a single query.
const MEMORY_QUERY_DEFAULT_LIMIT: usize = 5;

/// Bonus added to a memory's BM25 score when one of its `scope_paths` is a
/// prefix of one of the query's `file_paths`. Ensures file-specific rules are
/// reliably surfaced even when the prompt text barely mentions them.
const SCOPE_PATH_BONUS: f64 = 2.0;

/// `memory_save` — persist a memory, with AI-driven dedup.
///
/// Resolution order (see spec C):
/// 1. `merge_into` → commit an AI merge (overwrite target, absorb others).
/// 2. exact content-hash match → `duplicate_exact` (no write).
/// 3. near-duplicate (Jaccard ≥ threshold) and not `force` → `conflict` (no
///    write; returns both bodies for the AI to merge).
/// 4. otherwise → new `saved`.
pub fn handle_save(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;

    let text = arguments
        .get("text")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("'text' is required and must be non-empty"))?
        .to_string();

    let kind = arguments
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("lesson")
        .to_string();
    if !is_valid_memory_kind(&kind) {
        anyhow::bail!(
            "Invalid kind '{kind}'. Must be one of: {}",
            VALID_MEMORY_KINDS.join(", ")
        );
    }

    let tags = string_array(arguments, "tags");
    let scope_paths = string_array(arguments, "scope_paths");
    let force = arguments
        .get("force")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // (1) Explicit merge commit.
    if let Some(merge_into) = arguments.get("merge_into").and_then(|v| v.as_str()) {
        let absorb_ids = string_array(arguments, "absorb_ids");
        return commit_merge(
            &handoff,
            merge_into,
            &absorb_ids,
            text,
            kind,
            tags,
            scope_paths,
        );
    }

    let existing = read_all_memories(&handoff)?;
    let new_hash = lexsim::content_hash(&text);

    // (2) Exact duplicate (same canonical content).
    if let Some(dup) = existing.iter().find(|m| m.content_hash == new_hash) {
        return Ok(to_json(&json!({
            "status": "duplicate_exact",
            "existing_id": dup.id,
        })));
    }

    // (3) Near-duplicate: hand both bodies back for AI-driven merge.
    if !force {
        let new_set = lexsim::token_set(&text);
        let mut similar: Vec<Value> = Vec::new();
        for m in &existing {
            let score = lexsim::jaccard_sets(&new_set, &lexsim::token_set(&m.index_text()));
            if score >= MEMORY_DUP_THRESHOLD {
                similar.push(json!({
                    "id": m.id,
                    "text": m.text,
                    "kind": m.kind,
                    "score": round2(score),
                }));
            }
        }
        if !similar.is_empty() {
            similar.sort_by(|a, b| {
                b["score"]
                    .as_f64()
                    .partial_cmp(&a["score"].as_f64())
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            return Ok(to_json(&json!({
                "status": "conflict",
                "new": { "text": text, "kind": kind },
                "similar": similar,
                "instruction": "These are near-duplicates. Merge them and call memory_save again \
                    with merge_into=<id> and absorb_ids=[other ids], or pass force=true to save \
                    separately.",
            })));
        }
    }

    // (4) New memory.
    let id = new_memory_id();
    let entry = MemoryEntry::new(id.clone(), text, kind, tags, scope_paths, now_rfc3339());
    write_memory(&handoff, &entry)?;
    Ok(to_json(&json!({ "status": "saved", "id": id })))
}

/// Commit an AI-driven merge: overwrite `merge_into` with the merged text and
/// delete the absorbed memories, recording them in `superseded_ids`.
fn commit_merge(
    handoff: &std::path::Path,
    merge_into: &str,
    absorb_ids: &[String],
    text: String,
    kind: String,
    tags: Vec<String>,
    scope_paths: Vec<String>,
) -> Result<String> {
    let mut target = read_memory_by_id(handoff, merge_into)?
        .ok_or_else(|| anyhow::anyhow!("merge_into target not found: {merge_into}"))?;

    let now = now_rfc3339();
    target.text = text;
    target.kind = kind;
    if !tags.is_empty() {
        target.tags = tags;
    }
    if !scope_paths.is_empty() {
        target.scope_paths = scope_paths;
    }
    target.content_hash = lexsim::content_hash(&target.text);
    target.updated_at = now;

    let target_id = target.id.clone();
    let mut absorbed: Vec<String> = Vec::new();
    for raw in absorb_ids {
        if raw == &target_id {
            continue; // never absorb the target into itself
        }
        // Resolve to a concrete id (supports prefixes), then delete it.
        if let Some(m) = read_memory_by_id(handoff, raw)? {
            if delete_memory(handoff, &m.id)? {
                absorbed.push(m.id);
            }
        }
    }
    for a in &absorbed {
        if !target.superseded_ids.contains(a) {
            target.superseded_ids.push(a.clone());
        }
    }

    write_memory(handoff, &target)?;
    Ok(to_json(&json!({
        "status": "merged",
        "id": target_id,
        "absorbed_ids": absorbed,
    })))
}

/// `memory_query` — return memories relevant to the current prompt/file.
///
/// BM25 relevance + scope-path boosting, then **per-session diff injection**
/// (spec D): when `session_id` is given, memories already injected this session
/// with the same `content_hash` are filtered out, while an edited memory (new
/// hash) is re-injected. With `mark_injected` (default true) the survivors are
/// recorded in the session sidecar and their `hit_count` / `last_referenced_at`
/// are bumped. Without `session_id` this degrades to plain relevance ranking.
pub fn handle_query(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;

    let text = arguments
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let tool_name = arguments.get("tool_name").and_then(|v| v.as_str());
    let file_paths = string_array(arguments, "file_paths");
    let limit = arguments
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .filter(|n| *n > 0)
        .unwrap_or(MEMORY_QUERY_DEFAULT_LIMIT);
    let session_id = arguments
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let mark_injected = arguments
        .get("mark_injected")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let memories = read_all_memories(&handoff)?;
    if memories.is_empty() {
        return Ok(to_json(&json!({ "memories": [], "injected_count": 0 })));
    }

    // Build the BM25 corpus over each memory's index text (body + tags).
    let docs: Vec<String> = memories.iter().map(|m| m.index_text()).collect();
    let corpus = lexsim::Corpus::build(&docs);

    // Query = prompt text + tool name + file basenames (so a PreToolUse hook
    // that only passes a file path still matches name-related memories).
    let mut query_tokens = lexsim::tokenize(&text);
    if let Some(tn) = tool_name {
        query_tokens.extend(lexsim::tokenize(tn));
    }
    for p in &file_paths {
        query_tokens.extend(lexsim::tokenize(&basename(p)));
    }
    let scores = corpus.bm25_scores_tokens(&query_tokens);

    // Score + scope-path bonus, then threshold and rank.
    let mut ranked: Vec<(usize, f64)> = memories
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let mut s = scores[i];
            if scope_matches(&m.scope_paths, &file_paths) {
                s += SCOPE_PATH_BONUS;
            }
            (i, s)
        })
        .filter(|(_, s)| *s >= MEMORY_QUERY_MIN_SCORE)
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Per-session diff: drop memories already injected this session at the same
    // hash. The `limit` is applied to the *fresh* set so the caller still gets up
    // to `limit` new memories even when earlier prompts already consumed some.
    let now = now_rfc3339();
    let injected_set = session_id.map(|sid| read_injected_set(&handoff, sid, &now));
    let fresh: Vec<(usize, f64)> = ranked
        .into_iter()
        .filter(|(i, _)| match &injected_set {
            Some(set) => {
                let m = &memories[*i];
                !set.already_injected(&m.id, &m.content_hash)
            }
            None => true,
        })
        .take(limit)
        .collect();

    let out: Vec<Value> = fresh
        .iter()
        .map(|(i, s)| {
            let m = &memories[*i];
            json!({
                "id": m.id,
                "text": m.text,
                "kind": m.kind,
                "score": round2(*s),
            })
        })
        .collect();

    // Bookkeeping: record survivors in the sidecar and bump usage stats. Only
    // when we have a session id and marking is enabled (the hook's normal path).
    if mark_injected && !fresh.is_empty() {
        if let Some(sid) = session_id {
            mark_injected_memories(&handoff, sid, &memories, &fresh, &now)?;
        }
    }

    Ok(to_json(&json!({
        "memories": out,
        "injected_count": out.len(),
    })))
}

/// Append the freshly-injected memories to the session sidecar and bump each
/// one's `hit_count` / `last_referenced_at`.
///
/// Ordering matters: the **sidecar is written first**, before the per-memory
/// stat bumps. The sidecar is what suppresses re-injection, so it must be
/// recorded even if a later stat write fails — otherwise a failed bump would
/// skip the sidecar and re-spam the session on the next query (and double-count
/// any memory whose stats were already persisted before the failure). The stat
/// bumps are therefore strictly best-effort: a failure on one memory must not
/// drop the sidecar record or abort the rest.
fn mark_injected_memories(
    handoff: &std::path::Path,
    session_id: &str,
    memories: &[MemoryEntry],
    fresh: &[(usize, f64)],
    now: &str,
) -> Result<()> {
    let mut set = read_injected_set(handoff, session_id, now);
    set.updated_at = now.to_string();
    for (i, _) in fresh {
        let m = &memories[*i];
        set.mark(&m.id, &m.content_hash);
    }
    // (1) Persist the suppression record first — this is the correctness-critical
    // write. If it fails, surface the error (the session state is now unknown).
    write_injected_set(handoff, &set)?;

    // (2) Best-effort usage stats. Re-read each memory so we don't clobber a
    // concurrent edit's other fields. A failure here is non-fatal: the sidecar
    // already recorded the injection, so the worst case is a slightly stale
    // hit_count — never a re-spam or a double count.
    for (i, _) in fresh {
        let m = &memories[*i];
        if let Ok(Some(mut entry)) = read_memory_by_id(handoff, &m.id) {
            entry.hit_count = entry.hit_count.saturating_add(1);
            entry.last_referenced_at = Some(now.to_string());
            let _ = write_memory(handoff, &entry);
        }
    }
    Ok(())
}

/// `memory_delete` — remove a memory by id (AI-driven stale cleanup / tests).
pub fn handle_delete(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;

    let id = arguments
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'id' is required"))?;

    // Resolve prefixes to a concrete id for a friendly delete-by-prefix.
    let resolved = read_memory_by_id(&handoff, id)?
        .ok_or_else(|| anyhow::anyhow!("Memory not found: {id}"))?;
    let deleted = delete_memory(&handoff, &resolved.id)?;
    if !deleted {
        anyhow::bail!("Memory not found: {id}");
    }
    Ok(to_json(&json!({ "status": "deleted", "id": resolved.id })))
}

/// True if any `scope` prefix matches any `file` path.
fn scope_matches(scopes: &[String], files: &[String]) -> bool {
    if scopes.is_empty() || files.is_empty() {
        return false;
    }
    scopes
        .iter()
        .any(|scope| files.iter().any(|f| f.contains(scope.as_str())))
}

/// Last path component of `p` (handles both `/` and `\` separators).
fn basename(p: &str) -> String {
    p.rsplit(['/', '\\']).next().unwrap_or(p).to_string()
}

/// Read a `&[String]` from a JSON string-array argument (missing → empty).
fn string_array(arguments: &Value, key: &str) -> Vec<String> {
    arguments
        .get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn to_json(v: &Value) -> String {
    // Pretty so a human reading the raw tool result can follow it; both hook and
    // wrapper paths parse it the same way.
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basename_handles_separators() {
        assert_eq!(basename("src/storage/mod.rs"), "mod.rs");
        assert_eq!(basename("a\\b\\c.rs"), "c.rs");
        assert_eq!(basename("plain.rs"), "plain.rs");
    }

    #[test]
    fn scope_matches_prefix() {
        let scopes = vec!["src/storage/".to_string()];
        let files = vec!["/repo/src/storage/mod.rs".to_string()];
        assert!(scope_matches(&scopes, &files));
        let files2 = vec!["/repo/src/mcp/mod.rs".to_string()];
        assert!(!scope_matches(&scopes, &files2));
    }

    #[test]
    fn string_array_parsing() {
        let v = json!({ "tags": ["a", "b", 3, "c"] });
        assert_eq!(string_array(&v, "tags"), vec!["a", "b", "c"]);
        assert!(string_array(&v, "missing").is_empty());
    }

    #[test]
    fn round2_works() {
        assert_eq!(round2(1.23456), 1.23);
    }
}
