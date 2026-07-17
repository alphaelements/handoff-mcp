//! MCP handlers for the memory feature (save / query / delete).
//!
//! All three return a **parseable JSON string** as their content text, so both
//! the wrapper path and the Claude Code `mcp_tool` hook path can consume them
//! with the same JSON parse. `memory_query` supports per-session diff injection
//! via the `injected/` sidecar (P2); `memory_cleanup` lands in P3.

use anyhow::Result;
use serde_json::{json, Value};

use std::path::Path;

use super::resolve_project_dir;
use crate::context::injection::{filter_already_injected, rank_by_bm25_and_scope, RankConfig};
use crate::storage::config::{read_config, SettingsConfig};
use crate::storage::ensure_handoff_exists;
use crate::storage::memory::{
    delete_memory, gc_injected_sets, is_valid_memory_kind, new_memory_id, now_rfc3339,
    read_all_memories, read_injected_set, read_memory_by_id, write_injected_set, write_memory,
    MemoryEntry, VALID_MEMORY_KINDS,
};

/// Bonus added to a memory's BM25 score when one of its `scope_paths` is a
/// prefix of one of the query's `file_paths`. Ensures file-specific rules are
/// reliably surfaced even when the prompt text barely mentions them. Kept a
/// constant (not exposed in config) — it is an internal ranking weight, not a
/// user-facing threshold.
const SCOPE_PATH_BONUS: f64 = 2.0;

/// Load the memory tuning settings from the project's `config.toml`.
///
/// Missing `memory_*` keys (a pre-0.13.0 config) are filled by serde defaults at
/// parse time, so a legacy config still parses cleanly and yields the spec
/// defaults. Only a *genuinely corrupt* config.toml fails the parse — that is a
/// real error and is propagated, not silently swallowed. If the file is absent
/// altogether we fall back to defaults (the same as every other key's default).
fn memory_settings(handoff: &Path) -> Result<SettingsConfig> {
    let path = handoff.join("config.toml");
    if !path.exists() {
        return Ok(SettingsConfig::default());
    }
    Ok(read_config(&path)?.settings)
}

/// The JSON payload every memory tool returns when `settings.memory_enabled` is
/// false: a benign no-op the hook paths can parse, never an error (a disabled
/// feature must not make automatic UserPromptSubmit/SessionStart hooks noisy).
/// Carries `disabled: true` plus empty equivalents of each tool's normal shape
/// so a caller that reads `memories` / `auto_merged_exact` still sees a valid
/// structure.
fn disabled_payload() -> String {
    to_json(&json!({
        "disabled": true,
        "memories": [],
        "injected_count": 0,
        "auto_merged_exact": 0,
        "cleanup_recommendations": { "similar_clusters": [], "stale": [] },
        "injected_sidecars_removed": 0,
    }))
}

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
    let settings = memory_settings(&handoff)?;
    if !settings.memory_enabled {
        return Ok(disabled_payload());
    }

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
    let keywords = string_array(arguments, "keywords");
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
            keywords,
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
        let dup_threshold = settings.memory_dup_threshold;
        // Build the new memory's index text (body + tags + keywords) so the
        // Jaccard comparison is symmetric with the existing memories' index_text().
        let mut new_index = text.clone();
        if !tags.is_empty() {
            new_index.push(' ');
            new_index.push_str(&tags.join(" "));
        }
        if !keywords.is_empty() {
            let kw = keywords.join(" ");
            new_index.push(' ');
            new_index.push_str(&kw);
            new_index.push(' ');
            new_index.push_str(&kw);
        }
        let new_set = lexsim::token_set(&new_index);
        let mut similar: Vec<Value> = Vec::new();
        for m in &existing {
            let score = lexsim::jaccard_sets(&new_set, &lexsim::token_set(&m.index_text()));
            if score >= dup_threshold {
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
    let entry = MemoryEntry::new(
        id.clone(),
        text,
        kind,
        tags,
        keywords,
        scope_paths,
        now_rfc3339(),
    );
    write_memory(&handoff, &entry)?;
    Ok(to_json(&json!({ "status": "saved", "id": id })))
}

/// Commit an AI-driven merge: overwrite `merge_into` with the merged text and
/// delete the absorbed memories, recording them in `superseded_ids`.
#[allow(clippy::too_many_arguments)]
fn commit_merge(
    handoff: &std::path::Path,
    merge_into: &str,
    absorb_ids: &[String],
    text: String,
    kind: String,
    tags: Vec<String>,
    keywords: Vec<String>,
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
    if !keywords.is_empty() {
        target.keywords = keywords;
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
        if let Some(m) = read_memory_by_id(handoff, raw)? {
            // Fold the absorbed memory's metadata into the target before
            // deleting, so keywords/tags/scope_paths are not lost.
            for kw in &m.keywords {
                if !target.keywords.contains(kw) {
                    target.keywords.push(kw.clone());
                }
            }
            for t in &m.tags {
                if !target.tags.contains(t) {
                    target.tags.push(t.clone());
                }
            }
            for s in &m.scope_paths {
                if !target.scope_paths.contains(s) {
                    target.scope_paths.push(s.clone());
                }
            }
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
    let settings = memory_settings(&handoff)?;
    if !settings.memory_enabled {
        return Ok(disabled_payload());
    }
    let tool_name = arguments.get("tool_name").and_then(|v| v.as_str());
    let file_paths = string_array(arguments, "file_paths");
    let limit = arguments
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .filter(|n| *n > 0)
        .unwrap_or(settings.memory_query_limit as usize);
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

    // Score + scope-path bonus, then threshold and rank (shared with doc_query).
    let scope_paths: Vec<Vec<String>> = memories.iter().map(|m| m.scope_paths.clone()).collect();
    let rank_config = RankConfig {
        min_score: settings.memory_query_min_score,
        scope_path_bonus: SCOPE_PATH_BONUS,
        // Rank without truncating yet — the session diff below needs the full
        // ranked order so it can backfill past already-injected memories.
        limit: memories.len(),
    };
    let ranked = rank_by_bm25_and_scope(
        &corpus,
        &query_tokens,
        &scope_paths,
        &file_paths,
        &rank_config,
    );

    // Per-session diff: drop memories already injected this session at the same
    // hash. The `limit` is applied to the *fresh* set so the caller still gets up
    // to `limit` new memories even when earlier prompts already consumed some.
    let now = now_rfc3339();
    let injected_set = session_id.map(|sid| read_injected_set(&handoff, sid, &now));
    let already_injected = |i: usize| match &injected_set {
        Some(set) => {
            let m = &memories[i];
            set.already_injected(&m.id, &m.content_hash)
        }
        None => false,
    };
    let fresh = filter_already_injected(ranked, already_injected, limit);

    let out: Vec<Value> = fresh
        .iter()
        .map(|item| {
            let m = &memories[item.index];
            let mut entry = json!({
                "id": m.id,
                "text": m.text,
                "kind": m.kind,
                "score": round2(item.score),
            });
            if !m.keywords.is_empty() {
                entry["keywords"] = json!(m.keywords);
            }
            entry
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
    fresh: &[crate::context::injection::RankItem],
    now: &str,
) -> Result<()> {
    let mut set = read_injected_set(handoff, session_id, now);
    set.updated_at = now.to_string();
    for item in fresh {
        let m = &memories[item.index];
        set.mark(&m.id, &m.content_hash);
    }
    // (1) Persist the suppression record first — this is the correctness-critical
    // write. If it fails, surface the error (the session state is now unknown).
    write_injected_set(handoff, &set)?;

    // (2) Best-effort usage stats. Re-read each memory so we don't clobber a
    // concurrent edit's other fields. A failure here is non-fatal: the sidecar
    // already recorded the injection, so the worst case is a slightly stale
    // hit_count — never a re-spam or a double count.
    for item in fresh {
        let m = &memories[item.index];
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
    if !memory_settings(&handoff)?.memory_enabled {
        return Ok(disabled_payload());
    }

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

/// `memory_cleanup` — SessionStart housekeeping (spec C).
///
/// Three independent passes over the memory store:
///
/// 1. **Exact duplicates** (identical `content_hash`) are merged *silently and
///    losslessly*: the oldest memory in each hash group is kept, the others are
///    deleted and recorded in its `superseded_ids`. This is the only mutating
///    pass, gated by `apply_exact_merges` (default true).
/// 2. **Near-duplicate clusters** (Jaccard ≥ threshold, grouped by union-find)
///    and **stale** memories (`last_referenced_at` — or `created_at` if never
///    referenced — older than `stale_days`) are returned as *recommendations*
///    for the AI to act on with `memory_save(merge_into=…)` / `memory_delete`.
///    This pass never writes.
/// 3. The `injected/` sidecars older than the gc window are removed.
///
/// Returns a JSON string
/// `{"auto_merged_exact":n,"cleanup_recommendations":{"similar_clusters":[…],
/// "stale":[…]},"injected_sidecars_removed":k}`.
pub fn handle_cleanup(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;

    let settings = memory_settings(&handoff)?;
    if !settings.memory_enabled {
        return Ok(disabled_payload());
    }
    let apply_exact_merges = arguments
        .get("apply_exact_merges")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let stale_days = arguments
        .get("stale_days")
        .and_then(|v| v.as_i64())
        .filter(|n| *n >= 0)
        .unwrap_or(settings.memory_stale_days);

    let now = chrono::Utc::now();
    let now_str = now.to_rfc3339();

    // (1) Exact-duplicate auto-merge (lossless). Re-reads memories afterward so
    // later passes see the merged store.
    let auto_merged_exact = if apply_exact_merges {
        merge_exact_duplicates(&handoff, &now_str)?
    } else {
        0
    };

    let memories = read_all_memories(&handoff)?;

    // (2a) Near-duplicate clusters (recommendation only).
    let similar_clusters = similar_clusters(&memories, settings.memory_dup_threshold);

    // (2b) Stale memories (recommendation only).
    let stale = stale_memories(&memories, stale_days, now);

    // (3) Garbage-collect old per-session sidecars.
    let injected_sidecars_removed =
        gc_injected_sets(&handoff, settings.memory_injected_gc_days, now)?;

    Ok(to_json(&json!({
        "auto_merged_exact": auto_merged_exact,
        "cleanup_recommendations": {
            "similar_clusters": similar_clusters,
            "stale": stale,
        },
        "injected_sidecars_removed": injected_sidecars_removed,
    })))
}

/// Merge memories that share an identical `content_hash`. The oldest entry in
/// each group (by parsed `created_at`, ties broken by id) is kept; the rest are
/// absorbed into it and their files deleted. Returns the number of memories
/// absorbed (files removed).
///
/// "Lossless" means **no signal is dropped**, not byte-identical text: the
/// canonical content is identical by construction (same hash over the tokenized
/// text), so the keeper's body already represents every absorbed memory's
/// meaning. To avoid losing the *other* fields, the keeper inherits the union of
/// every absorbed memory's `tags` / `scope_paths` / `superseded_ids`, the sum of
/// their `hit_count`, and the latest `last_referenced_at`.
///
/// Failure safety (mirrors `mark_injected_memories`): the keeper is rewritten
/// with the full merged metadata and `superseded_ids` **before** any duplicate
/// file is deleted, so the audit trail is durable even if a later delete fails.
/// Per-duplicate deletes are then best-effort — a failure on one leaves an
/// orphaned-but-superseded file (re-absorbed on the next run) rather than a lost
/// record.
fn merge_exact_duplicates(handoff: &std::path::Path, now: &str) -> Result<usize> {
    use std::collections::BTreeMap;

    let memories = read_all_memories(handoff)?;
    // Group by content_hash, preserving discovery order within each group.
    let mut groups: BTreeMap<String, Vec<MemoryEntry>> = BTreeMap::new();
    for m in memories {
        groups.entry(m.content_hash.clone()).or_default().push(m);
    }

    let mut absorbed_total = 0usize;
    for (_hash, mut group) in groups {
        if group.len() < 2 {
            continue;
        }
        // Keep the oldest as the canonical survivor. Parse created_at to an
        // instant so differing RFC3339 offsets order by true time, not by string
        // (ties — including unparseable stamps — broken by id for determinism).
        group.sort_by(|a, b| {
            parse_instant(&a.created_at)
                .cmp(&parse_instant(&b.created_at))
                .then_with(|| a.id.cmp(&b.id))
        });
        let mut keeper = group.remove(0);

        // Fold every absorbed memory's signal into the keeper (union of tags /
        // scope_paths / superseded_ids; summed hit_count; latest reference).
        for dup in &group {
            if !keeper.superseded_ids.contains(&dup.id) {
                keeper.superseded_ids.push(dup.id.clone());
            }
            for sid in &dup.superseded_ids {
                if sid != &keeper.id && !keeper.superseded_ids.contains(sid) {
                    keeper.superseded_ids.push(sid.clone());
                }
            }
            for t in &dup.tags {
                if !keeper.tags.contains(t) {
                    keeper.tags.push(t.clone());
                }
            }
            for kw in &dup.keywords {
                if !keeper.keywords.contains(kw) {
                    keeper.keywords.push(kw.clone());
                }
            }
            for s in &dup.scope_paths {
                if !keeper.scope_paths.contains(s) {
                    keeper.scope_paths.push(s.clone());
                }
            }
            keeper.hit_count = keeper.hit_count.saturating_add(dup.hit_count);
            keeper.last_referenced_at = latest_timestamp(
                keeper.last_referenced_at.take(),
                dup.last_referenced_at.clone(),
            );
        }
        keeper.updated_at = now.to_string();

        // (1) Persist the merged keeper FIRST — this is the durable audit record.
        write_memory(handoff, &keeper)?;

        // (2) Best-effort delete the absorbed files. A failure here is non-fatal:
        // the keeper already records the absorption, so a surviving dup is simply
        // re-absorbed next run.
        for dup in &group {
            if matches!(delete_memory(handoff, &dup.id), Ok(true)) {
                absorbed_total += 1;
            }
        }
    }
    Ok(absorbed_total)
}

/// Parse an RFC3339 timestamp to a UTC instant for ordering. Unparseable stamps
/// sort as the epoch (treated as "oldest") so they don't crash ordering; the id
/// tie-break keeps the result deterministic.
fn parse_instant(s: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::DateTime::<chrono::Utc>::MIN_UTC)
}

/// Return the later of two optional RFC3339 timestamps (an unparseable one loses;
/// `None` loses to `Some`).
fn latest_timestamp(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(x), Some(y)) => {
            if parse_instant(&y) > parse_instant(&x) {
                Some(y)
            } else {
                Some(x)
            }
        }
        (Some(x), None) => Some(x),
        (None, b) => b,
    }
}

/// Group memories into near-duplicate clusters via Jaccard ≥ threshold using
/// union-find, and emit each multi-member cluster as a recommendation. Exact
/// duplicates have already been merged by the time this runs, so a cluster here
/// is genuinely "similar but not identical" — the AI decides whether to merge.
fn similar_clusters(memories: &[MemoryEntry], dup_threshold: f64) -> Vec<Value> {
    let n = memories.len();
    if n < 2 {
        return Vec::new();
    }

    // Precompute each memory's token set once (O(n) tokenizations, not O(n²)).
    let token_sets: Vec<_> = memories
        .iter()
        .map(|m| lexsim::token_set(&m.index_text()))
        .collect();

    let mut uf = UnionFind::new(n);
    let mut pair_scores: std::collections::HashMap<(usize, usize), f64> =
        std::collections::HashMap::new();
    for i in 0..n {
        for j in (i + 1)..n {
            let score = lexsim::jaccard_sets(&token_sets[i], &token_sets[j]);
            if score >= dup_threshold {
                uf.union(i, j);
                pair_scores.insert((i, j), score);
            }
        }
    }

    // Bucket indices by their union-find root.
    let mut clusters: std::collections::BTreeMap<usize, Vec<usize>> =
        std::collections::BTreeMap::new();
    for i in 0..n {
        clusters.entry(uf.find(i)).or_default().push(i);
    }

    let mut out: Vec<Value> = Vec::new();
    for (_root, members) in clusters {
        if members.len() < 2 {
            continue;
        }
        // Representative pair score = the max similarity within the cluster, so
        // the AI sees how tight it is.
        let mut max_score = 0.0_f64;
        for a in 0..members.len() {
            for b in (a + 1)..members.len() {
                let (lo, hi) = (members[a].min(members[b]), members[a].max(members[b]));
                if let Some(s) = pair_scores.get(&(lo, hi)) {
                    if *s > max_score {
                        max_score = *s;
                    }
                }
            }
        }
        let entries: Vec<Value> = members
            .iter()
            .map(|&i| {
                let m = &memories[i];
                json!({ "id": m.id, "text": m.text, "kind": m.kind })
            })
            .collect();
        out.push(json!({
            "max_score": round2(max_score),
            "memories": entries,
        }));
    }
    // Tightest clusters first.
    out.sort_by(|a, b| {
        b["max_score"]
            .as_f64()
            .partial_cmp(&a["max_score"].as_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

/// Flag memories not referenced for `stale_days` (using `last_referenced_at`, or
/// `created_at` when never referenced). Recommendation only — the AI decides
/// whether a stale memory is obsolete or simply rarely relevant.
fn stale_memories(
    memories: &[MemoryEntry],
    stale_days: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<Value> {
    let cutoff = now - chrono::Duration::days(stale_days);
    let mut out: Vec<(chrono::DateTime<chrono::Utc>, Value)> = Vec::new();
    for m in memories {
        // The reference point: last injection if any, else creation time.
        let stamp = m.last_referenced_at.as_deref().unwrap_or(&m.created_at);
        let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(stamp) else {
            continue; // unparseable timestamp → can't judge age, skip
        };
        let parsed = parsed.with_timezone(&chrono::Utc);
        if parsed < cutoff {
            out.push((
                parsed,
                json!({
                    "id": m.id,
                    "text": m.text,
                    "kind": m.kind,
                    "hit_count": m.hit_count,
                    "last_referenced_at": m.last_referenced_at,
                    "created_at": m.created_at,
                }),
            ));
        }
    }
    // Oldest (most stale) first.
    out.sort_by_key(|(stamp, _)| *stamp);
    out.into_iter().map(|(_, v)| v).collect()
}

/// Minimal union-find (disjoint set) with path compression and union by size,
/// used to cluster near-duplicate memories.
struct UnionFind {
    parent: Vec<usize>,
    size: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        UnionFind {
            parent: (0..n).collect(),
            size: vec![1; n],
        }
    }

    fn find(&mut self, x: usize) -> usize {
        let mut root = x;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        // Path compression.
        let mut cur = x;
        while self.parent[cur] != root {
            let next = self.parent[cur];
            self.parent[cur] = root;
            cur = next;
        }
        root
    }

    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return;
        }
        // Union by size: attach the smaller tree under the larger.
        let (big, small) = if self.size[ra] >= self.size[rb] {
            (ra, rb)
        } else {
            (rb, ra)
        };
        self.parent[small] = big;
        self.size[big] += self.size[small];
    }
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

    /// The spec-default Jaccard threshold, used to drive the pure clustering
    /// helper in unit tests (the configurable value lives in `SettingsConfig`).
    const DEFAULT_DUP_THRESHOLD: f64 = 0.72;

    #[test]
    fn basename_handles_separators() {
        assert_eq!(basename("src/storage/mod.rs"), "mod.rs");
        assert_eq!(basename("a\\b\\c.rs"), "c.rs");
        assert_eq!(basename("plain.rs"), "plain.rs");
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

    fn mem(id: &str, text: &str, created: &str, last_ref: Option<&str>) -> MemoryEntry {
        let mut m = MemoryEntry::new(
            id.to_string(),
            text.to_string(),
            "lesson".to_string(),
            vec![],
            vec![],
            vec![],
            created.to_string(),
        );
        m.last_referenced_at = last_ref.map(str::to_string);
        m
    }

    #[test]
    fn union_find_groups_transitively() {
        let mut uf = UnionFind::new(5);
        uf.union(0, 1);
        uf.union(1, 2);
        // {0,1,2} share a root; 3 and 4 are singletons.
        assert_eq!(uf.find(0), uf.find(2));
        assert_ne!(uf.find(0), uf.find(3));
        assert_ne!(uf.find(3), uf.find(4));
    }

    #[test]
    fn similar_clusters_groups_near_duplicates() {
        let now = now_rfc3339();
        let a = mem(
            "m-a",
            "always use atomic_write for handoff files",
            &now,
            None,
        );
        let b = mem(
            "m-b",
            "always use atomic_write for handoff files when saving",
            &now,
            None,
        );
        let c = mem(
            "m-c",
            "the gantt chart export is totally unrelated",
            &now,
            None,
        );
        let clusters = similar_clusters(&[a, b, c], DEFAULT_DUP_THRESHOLD);
        assert_eq!(clusters.len(), 1, "only a+b cluster; c stands alone");
        let members = clusters[0]["memories"].as_array().unwrap();
        assert_eq!(members.len(), 2);
        assert!(clusters[0]["max_score"].as_f64().unwrap() >= DEFAULT_DUP_THRESHOLD);
    }

    #[test]
    fn similar_clusters_empty_when_all_distinct() {
        let now = now_rfc3339();
        let a = mem("m-a", "use atomic_write everywhere", &now, None);
        let b = mem("m-b", "render the gantt chart schedule", &now, None);
        assert!(similar_clusters(&[a, b], DEFAULT_DUP_THRESHOLD).is_empty());
    }

    #[test]
    fn stale_uses_last_referenced_then_created() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-06-27T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        // Never referenced, created 100 days ago → stale.
        let old = mem("m-old", "ancient rule", "2026-03-19T00:00:00Z", None);
        // Created long ago but referenced yesterday → NOT stale.
        let touched = mem(
            "m-fresh",
            "recently used rule",
            "2026-01-01T00:00:00Z",
            Some("2026-06-26T00:00:00Z"),
        );
        let stale = stale_memories(&[old, touched], 60, now);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0]["id"], "m-old");
    }

    #[test]
    fn stale_skips_unparseable_timestamp() {
        let now = chrono::Utc::now();
        let mut bad = mem("m-bad", "x", "not-a-date", None);
        bad.created_at = "not-a-date".to_string();
        assert!(stale_memories(&[bad], 0, now).is_empty());
    }
}
