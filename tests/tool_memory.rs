//! Integration tests for the P1 memory tools (memory_save / memory_query /
//! memory_delete), exercised end-to-end through the JSON-RPC `process_line`
//! entry point — the same path the MCP server runs in production.

use serde_json::{json, Value};
use tempfile::TempDir;

fn send(input: &str) -> Option<Value> {
    let result = handoff_mcp::mcp::protocol::process_line(input)?;
    Some(serde_json::from_str(&result).expect("response should be valid JSON"))
}

fn setup_project() -> (TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let req = json!({
        "jsonrpc": "2.0", "id": 0,
        "method": "tools/call",
        "params": {
            "name": "handoff_init",
            "arguments": {
                "project_dir": dir.to_string_lossy(),
                "project_name": "memtest"
            }
        }
    });
    send(&req.to_string()).unwrap();
    (tmp, dir)
}

fn call(dir: &std::path::Path, name: &str, mut args: Value) -> Value {
    args["project_dir"] = json!(dir.to_string_lossy());
    let req = json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": args }
    });
    send(&req.to_string()).unwrap()
}

/// Parse the JSON-string payload returned in the tool result content.
fn payload(resp: &Value) -> Value {
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("content text");
    serde_json::from_str(text).expect("payload should be a JSON string")
}

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"].as_bool().unwrap_or(false)
}

#[test]
fn init_creates_memory_dir() {
    let (_tmp, dir) = setup_project();
    assert!(dir.join(".handoff/memory").is_dir());
}

#[test]
fn save_new_memory() {
    let (_tmp, dir) = setup_project();
    let resp = call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "always use atomic_write for handoff files", "kind": "rule" }),
    );
    assert!(!is_error(&resp));
    let p = payload(&resp);
    assert_eq!(p["status"], "saved");
    assert!(p["id"].as_str().unwrap().starts_with("m-"));
}

#[test]
fn save_requires_nonempty_text() {
    let (_tmp, dir) = setup_project();
    let resp = call(&dir, "handoff_memory_save", json!({ "text": "   " }));
    assert!(is_error(&resp));
}

#[test]
fn save_rejects_bad_kind() {
    let (_tmp, dir) = setup_project();
    let resp = call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "x", "kind": "nope" }),
    );
    assert!(is_error(&resp));
}

#[test]
fn exact_duplicate_not_rewritten() {
    let (_tmp, dir) = setup_project();
    let text = "use SSH for git push, never embed PAT in the URL";
    let first = payload(&call(&dir, "handoff_memory_save", json!({ "text": text })));
    assert_eq!(first["status"], "saved");

    // Same content (only whitespace/case differs) → duplicate_exact.
    let dup = payload(&call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "Use SSH for git   push, never embed PAT in the URL" }),
    ));
    assert_eq!(dup["status"], "duplicate_exact");
    assert_eq!(dup["existing_id"], first["id"]);
}

#[test]
fn near_duplicate_returns_conflict_with_both_bodies() {
    let (_tmp, dir) = setup_project();
    let a = payload(&call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "the memory feature carries lessons across sessions for the project" }),
    ));
    assert_eq!(a["status"], "saved");

    // Heavily overlapping wording → conflict (not written).
    let b = payload(&call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "the memory feature carries lessons across sessions for this project too" }),
    ));
    assert_eq!(b["status"], "conflict");
    assert!(b["new"]["text"].is_string());
    let similar = b["similar"].as_array().unwrap();
    assert!(!similar.is_empty());
    assert_eq!(similar[0]["id"], a["id"]);
    assert!(similar[0]["score"].as_f64().unwrap() >= 0.72);
}

#[test]
fn force_saves_near_duplicate_separately() {
    let (_tmp, dir) = setup_project();
    call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "the memory feature carries lessons across sessions for the project" }),
    );
    let forced = payload(&call(
        &dir,
        "handoff_memory_save",
        json!({
            "text": "the memory feature carries lessons across sessions for this project too",
            "force": true
        }),
    ));
    assert_eq!(forced["status"], "saved");
}

#[test]
fn merge_into_overwrites_and_absorbs() {
    let (_tmp, dir) = setup_project();
    let a = payload(&call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "memory carries lessons across sessions", "tags": ["t1"] }),
    ));
    let b = payload(&call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "completely separate gotcha about the gantt chart export", "force": true }),
    ));
    let a_id = a["id"].as_str().unwrap();
    let b_id = b["id"].as_str().unwrap();

    let merged = payload(&call(
        &dir,
        "handoff_memory_save",
        json!({
            "text": "memory carries lessons across sessions; also note the gantt export gotcha",
            "merge_into": a_id,
            "absorb_ids": [b_id]
        }),
    ));
    assert_eq!(merged["status"], "merged");
    assert_eq!(merged["id"], a_id);
    assert_eq!(merged["absorbed_ids"][0], b_id);

    // b is gone, a remains with the new text.
    let del_b = call(&dir, "handoff_memory_delete", json!({ "id": b_id }));
    assert!(
        is_error(&del_b),
        "absorbed memory should already be deleted"
    );
    let q = payload(&call(
        &dir,
        "handoff_memory_query",
        json!({ "text": "gantt export gotcha" }),
    ));
    let mems = q["memories"].as_array().unwrap();
    assert!(mems.iter().any(|m| m["id"] == a_id));
}

#[test]
fn query_returns_relevant_memory() {
    let (_tmp, dir) = setup_project();
    call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "always use atomic_write for handoff files", "force": true }),
    );
    call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "the cat sat on the mat in the warm afternoon sun", "force": true }),
    );

    let q = payload(&call(
        &dir,
        "handoff_memory_query",
        json!({ "text": "atomic write" }),
    ));
    let mems = q["memories"].as_array().unwrap();
    assert!(!mems.is_empty());
    assert!(mems[0]["text"].as_str().unwrap().contains("atomic_write"));
}

#[test]
fn query_japanese_matches() {
    let (_tmp, dir) = setup_project();
    call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "メモリ機能はセッション間で教訓を引き継ぐ", "force": true }),
    );
    call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "ガントチャートでスケジュールを表示する", "force": true }),
    );
    let q = payload(&call(
        &dir,
        "handoff_memory_query",
        json!({ "text": "メモリ機能" }),
    ));
    let mems = q["memories"].as_array().unwrap();
    assert!(!mems.is_empty());
    assert!(mems[0]["text"].as_str().unwrap().contains("メモリ"));
}

#[test]
fn query_scope_path_boosts_file_specific_rule() {
    let (_tmp, dir) = setup_project();
    // A rule scoped to src/storage/ with text that barely mentions storage.
    call(
        &dir,
        "handoff_memory_save",
        json!({
            "text": "remember the special invariant here",
            "scope_paths": ["src/storage/"],
            "force": true
        }),
    );
    // Editing a file under that scope should surface the rule even though the
    // query text doesn't overlap it.
    let q = payload(&call(
        &dir,
        "handoff_memory_query",
        json!({
            "text": "",
            "tool_name": "Edit",
            "file_paths": ["/repo/src/storage/mod.rs"]
        }),
    ));
    let mems = q["memories"].as_array().unwrap();
    assert!(
        mems.iter()
            .any(|m| m["text"].as_str().unwrap().contains("special invariant")),
        "scope-matched memory should be surfaced"
    );
}

#[test]
fn query_empty_store() {
    let (_tmp, dir) = setup_project();
    let q = payload(&call(
        &dir,
        "handoff_memory_query",
        json!({ "text": "anything" }),
    ));
    assert_eq!(q["memories"].as_array().unwrap().len(), 0);
    assert_eq!(q["injected_count"], 0);
}

#[test]
fn delete_by_id_and_prefix() {
    let (_tmp, dir) = setup_project();
    let a = payload(&call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "first memory", "force": true }),
    ));
    let id = a["id"].as_str().unwrap().to_string();

    let del = payload(&call(&dir, "handoff_memory_delete", json!({ "id": id })));
    assert_eq!(del["status"], "deleted");

    // Deleting again → error (not found).
    let again = call(&dir, "handoff_memory_delete", json!({ "id": id }));
    assert!(is_error(&again));
}

#[test]
fn delete_missing_errors() {
    let (_tmp, dir) = setup_project();
    let resp = call(
        &dir,
        "handoff_memory_delete",
        json!({ "id": "m-does-not-exist" }),
    );
    assert!(is_error(&resp));
}

// ---------------------------------------------------------------------------
// P2: per-session diff injection (injected/ sidecar).
// ---------------------------------------------------------------------------

#[test]
fn same_session_second_query_is_empty() {
    let (_tmp, dir) = setup_project();
    call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "always use atomic_write for handoff files", "force": true }),
    );

    let q1 = payload(&call(
        &dir,
        "handoff_memory_query",
        json!({ "text": "atomic write", "session_id": "sess-A" }),
    ));
    assert_eq!(q1["memories"].as_array().unwrap().len(), 1);
    assert_eq!(q1["injected_count"], 1);

    // Second query in the SAME session for the same memory → already injected.
    let q2 = payload(&call(
        &dir,
        "handoff_memory_query",
        json!({ "text": "atomic write", "session_id": "sess-A" }),
    ));
    assert_eq!(
        q2["memories"].as_array().unwrap().len(),
        0,
        "already-injected memory must not be returned twice in one session"
    );
    assert_eq!(q2["injected_count"], 0);
}

#[test]
fn different_sessions_are_isolated() {
    let (_tmp, dir) = setup_project();
    call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "always use atomic_write for handoff files", "force": true }),
    );

    let a = payload(&call(
        &dir,
        "handoff_memory_query",
        json!({ "text": "atomic write", "session_id": "sess-A" }),
    ));
    assert_eq!(a["memories"].as_array().unwrap().len(), 1);

    // A brand new session has its own empty sidecar → memory shows up again.
    let b = payload(&call(
        &dir,
        "handoff_memory_query",
        json!({ "text": "atomic write", "session_id": "sess-B" }),
    ));
    assert_eq!(
        b["memories"].as_array().unwrap().len(),
        1,
        "a different session must see the memory afresh"
    );
}

#[test]
fn edited_memory_is_reinjected_in_same_session() {
    let (_tmp, dir) = setup_project();
    let saved = payload(&call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "always use atomic_write for handoff files", "force": true }),
    ));
    let id = saved["id"].as_str().unwrap().to_string();

    // First injection in sess-A.
    let q1 = payload(&call(
        &dir,
        "handoff_memory_query",
        json!({ "text": "atomic write", "session_id": "sess-A" }),
    ));
    assert_eq!(q1["memories"].as_array().unwrap().len(), 1);

    // Edit the memory's body (new content_hash) via a merge_into commit.
    let merged = payload(&call(
        &dir,
        "handoff_memory_save",
        json!({
            "text": "always use atomic_write for handoff files — and fsync before rename",
            "merge_into": id,
        }),
    ));
    assert_eq!(merged["status"], "merged");

    // Same session, but the hash changed → re-injected.
    let q2 = payload(&call(
        &dir,
        "handoff_memory_query",
        json!({ "text": "atomic write", "session_id": "sess-A" }),
    ));
    assert_eq!(
        q2["memories"].as_array().unwrap().len(),
        1,
        "an edited memory (new hash) must be re-injected even in the same session"
    );
}

#[test]
fn mark_injected_false_does_not_record() {
    let (_tmp, dir) = setup_project();
    call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "always use atomic_write for handoff files", "force": true }),
    );

    // Probe without recording.
    let q1 = payload(&call(
        &dir,
        "handoff_memory_query",
        json!({ "text": "atomic write", "session_id": "sess-A", "mark_injected": false }),
    ));
    assert_eq!(q1["memories"].as_array().unwrap().len(), 1);

    // Because the first call didn't mark, the memory still shows up.
    let q2 = payload(&call(
        &dir,
        "handoff_memory_query",
        json!({ "text": "atomic write", "session_id": "sess-A", "mark_injected": false }),
    ));
    assert_eq!(
        q2["memories"].as_array().unwrap().len(),
        1,
        "mark_injected=false must not persist the sidecar"
    );
}

#[test]
fn query_without_session_id_does_not_filter() {
    let (_tmp, dir) = setup_project();
    call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "always use atomic_write for handoff files", "force": true }),
    );

    // No session_id → plain relevance ranking, repeatable.
    for _ in 0..2 {
        let q = payload(&call(
            &dir,
            "handoff_memory_query",
            json!({ "text": "atomic write" }),
        ));
        assert_eq!(q["memories"].as_array().unwrap().len(), 1);
    }
}

#[test]
fn injected_query_bumps_hit_count_and_last_referenced() {
    let (_tmp, dir) = setup_project();
    let saved = payload(&call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "always use atomic_write for handoff files", "force": true }),
    ));
    let id = saved["id"].as_str().unwrap().to_string();

    call(
        &dir,
        "handoff_memory_query",
        json!({ "text": "atomic write", "session_id": "sess-A" }),
    );

    // Read the on-disk memory file and confirm usage stats were bumped.
    let path = dir.join(".handoff/memory").join(format!("{id}.json"));
    let raw = std::fs::read_to_string(&path).unwrap();
    let mem: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(mem["hit_count"], 1, "hit_count must increment on injection");
    assert!(
        mem["last_referenced_at"].is_string(),
        "last_referenced_at must be set on injection"
    );
}

#[test]
fn injected_sidecar_written_to_disk() {
    let (_tmp, dir) = setup_project();
    call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "always use atomic_write for handoff files", "force": true }),
    );
    call(
        &dir,
        "handoff_memory_query",
        json!({ "text": "atomic write", "session_id": "sess-A" }),
    );
    // The sidecar filename carries a hash suffix (collision-free), so locate the
    // single .json under injected/ rather than assuming the bare session id.
    let injected_dir = dir.join(".handoff/memory/injected");
    let sidecar = std::fs::read_dir(&injected_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .expect("a session sidecar must be persisted");
    let raw = std::fs::read_to_string(&sidecar).unwrap();
    let set: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(set["session_id"], "sess-A");
    assert_eq!(set["injected"].as_object().unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// P3: memory_cleanup (exact auto-merge + cluster/stale recommendations + gc).
// ---------------------------------------------------------------------------

/// Write a memory file directly so a test can seed states `memory_save` would
/// reject (e.g. two files sharing a content_hash) or back-date timestamps.
fn write_raw_memory(dir: &std::path::Path, mem: &Value) {
    let mem_dir = dir.join(".handoff/memory");
    std::fs::create_dir_all(&mem_dir).unwrap();
    let id = mem["id"].as_str().unwrap();
    std::fs::write(
        mem_dir.join(format!("{id}.json")),
        serde_json::to_string_pretty(mem).unwrap(),
    )
    .unwrap();
}

fn list_memory_ids(dir: &std::path::Path) -> Vec<String> {
    let mem_dir = dir.join(".handoff/memory");
    std::fs::read_dir(&mem_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(str::to_string))
        .collect()
}

#[test]
fn cleanup_merges_exact_duplicates_losslessly() {
    let (_tmp, dir) = setup_project();
    // Two files with the SAME content_hash (same canonical text). memory_save
    // would refuse the second, so seed both raw.
    let hash = lexsim::content_hash("always use atomic_write for handoff files");
    write_raw_memory(
        &dir,
        &json!({
            "version": 1, "id": "m-20260601-000000-000001",
            "text": "always use atomic_write for handoff files", "kind": "rule",
            "tags": [], "scope_paths": [], "content_hash": hash,
            "created_at": "2026-06-01T00:00:00Z", "updated_at": "2026-06-01T00:00:00Z",
            "hit_count": 0, "superseded_ids": []
        }),
    );
    write_raw_memory(
        &dir,
        &json!({
            "version": 1, "id": "m-20260602-000000-000002",
            "text": "always use atomic_write for handoff files", "kind": "rule",
            "tags": [], "scope_paths": [], "content_hash": hash,
            "created_at": "2026-06-02T00:00:00Z", "updated_at": "2026-06-02T00:00:00Z",
            "hit_count": 0, "superseded_ids": []
        }),
    );

    let p = payload(&call(&dir, "handoff_memory_cleanup", json!({})));
    assert_eq!(p["auto_merged_exact"], 1, "one duplicate absorbed");

    // Only the oldest survives, with the other in superseded_ids.
    let ids = list_memory_ids(&dir);
    assert_eq!(ids.len(), 1, "exactly one memory remains");
    assert_eq!(ids[0], "m-20260601-000000-000001", "oldest kept");
    let raw =
        std::fs::read_to_string(dir.join(".handoff/memory/m-20260601-000000-000001.json")).unwrap();
    let kept: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(kept["superseded_ids"][0], "m-20260602-000000-000002");
}

#[test]
fn cleanup_exact_merge_preserves_absorbed_signal() {
    let (_tmp, dir) = setup_project();
    // Same canonical content, but the two entries differ in tags / scope_paths /
    // hit_count / last_referenced_at. The keeper must inherit ALL of it — none of
    // the indexing/scoping signal may be dropped on absorption.
    let hash = lexsim::content_hash("always use atomic_write for handoff files");
    // Ordering is by parsed instant, not by string. The keeper's stamp uses a
    // +09:00 offset that reads "08:00" but is actually 2026-05-31T23:00Z — one
    // hour BEFORE the absorbed file's 2026-06-01T00:00Z. A naive string compare
    // would pick the wrong survivor; the instant parse must keep m-keeper.
    write_raw_memory(
        &dir,
        &json!({
            "version": 1, "id": "m-keeper",
            "text": "always use atomic_write for handoff files", "kind": "rule",
            "tags": ["a"], "scope_paths": ["src/storage/"], "content_hash": hash,
            "created_at": "2026-06-01T08:00:00+09:00", "updated_at": "2026-06-01T08:00:00+09:00",
            "hit_count": 3, "last_referenced_at": "2026-06-10T00:00:00Z",
            "superseded_ids": []
        }),
    );
    write_raw_memory(
        &dir,
        &json!({
            "version": 1, "id": "m-absorbed",
            "text": "always use atomic_write for handoff files", "kind": "rule",
            "tags": ["b"], "scope_paths": ["src/mcp/"], "content_hash": hash,
            "created_at": "2026-06-01T00:00:00Z", "updated_at": "2026-06-01T00:00:00Z",
            "hit_count": 40, "last_referenced_at": "2026-06-20T00:00:00Z",
            "superseded_ids": ["m-ancient"]
        }),
    );

    let p = payload(&call(&dir, "handoff_memory_cleanup", json!({})));
    assert_eq!(p["auto_merged_exact"], 1);

    let ids = list_memory_ids(&dir);
    assert_eq!(
        ids,
        vec!["m-keeper".to_string()],
        "earlier-instant keeper survives (offset parsed, not string-compared)"
    );
    let kept: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join(".handoff/memory/m-keeper.json")).unwrap(),
    )
    .unwrap();

    let tags: Vec<&str> = kept["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        tags.contains(&"a") && tags.contains(&"b"),
        "tags unioned: {tags:?}"
    );
    let scopes: Vec<&str> = kept["scope_paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        scopes.contains(&"src/storage/") && scopes.contains(&"src/mcp/"),
        "scope_paths unioned: {scopes:?}"
    );
    assert_eq!(kept["hit_count"], 43, "hit_count summed (3 + 40)");
    assert_eq!(
        kept["last_referenced_at"], "2026-06-20T00:00:00Z",
        "latest last_referenced_at wins"
    );
    let superseded: Vec<&str> = kept["superseded_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        superseded.contains(&"m-absorbed") && superseded.contains(&"m-ancient"),
        "absorbed id + its prior trail both recorded: {superseded:?}"
    );
}

#[test]
fn cleanup_can_skip_exact_merges() {
    let (_tmp, dir) = setup_project();
    let hash = lexsim::content_hash("same text here");
    for id in ["m-20260601-000000-000001", "m-20260602-000000-000002"] {
        write_raw_memory(
            &dir,
            &json!({
                "version": 1, "id": id, "text": "same text here", "kind": "lesson",
                "tags": [], "scope_paths": [], "content_hash": hash,
                "created_at": "2026-06-01T00:00:00Z", "updated_at": "2026-06-01T00:00:00Z",
                "hit_count": 0, "superseded_ids": []
            }),
        );
    }
    let p = payload(&call(
        &dir,
        "handoff_memory_cleanup",
        json!({ "apply_exact_merges": false }),
    ));
    assert_eq!(p["auto_merged_exact"], 0);
    assert_eq!(list_memory_ids(&dir).len(), 2, "no merge applied");
}

#[test]
fn cleanup_recommends_similar_clusters() {
    let (_tmp, dir) = setup_project();
    call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "always use atomic_write for handoff files", "force": true }),
    );
    call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "always use atomic_write for handoff files when saving", "force": true }),
    );
    call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "the gantt chart export is a totally different topic", "force": true }),
    );

    let p = payload(&call(&dir, "handoff_memory_cleanup", json!({})));
    let clusters = p["cleanup_recommendations"]["similar_clusters"]
        .as_array()
        .unwrap();
    assert_eq!(clusters.len(), 1, "the two atomic_write memories cluster");
    assert_eq!(clusters[0]["memories"].as_array().unwrap().len(), 2);
    assert!(clusters[0]["max_score"].as_f64().unwrap() >= 0.72);
}

#[test]
fn cleanup_recommends_stale_memories() {
    let (_tmp, dir) = setup_project();
    // Created ~6 months ago, never referenced → stale at default 60 days.
    write_raw_memory(
        &dir,
        &json!({
            "version": 1, "id": "m-20251201-000000-000001",
            "text": "an old rule nobody has touched", "kind": "rule",
            "tags": [], "scope_paths": [],
            "content_hash": lexsim::content_hash("an old rule nobody has touched"),
            "created_at": "2025-12-01T00:00:00Z", "updated_at": "2025-12-01T00:00:00Z",
            "hit_count": 0, "superseded_ids": []
        }),
    );
    // A fresh one created now via the normal path → not stale.
    call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "a brand new rule we just learned", "force": true }),
    );

    let p = payload(&call(&dir, "handoff_memory_cleanup", json!({})));
    let stale = p["cleanup_recommendations"]["stale"].as_array().unwrap();
    assert_eq!(stale.len(), 1, "only the December memory is stale");
    assert_eq!(stale[0]["id"], "m-20251201-000000-000001");
}

#[test]
fn cleanup_stale_days_zero_flags_everything() {
    let (_tmp, dir) = setup_project();
    call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "any memory at all", "force": true }),
    );
    let p = payload(&call(
        &dir,
        "handoff_memory_cleanup",
        json!({ "stale_days": 0 }),
    ));
    let stale = p["cleanup_recommendations"]["stale"].as_array().unwrap();
    assert_eq!(
        stale.len(),
        1,
        "stale_days=0 makes even a fresh memory stale"
    );
}

#[test]
fn cleanup_empty_store_is_clean() {
    let (_tmp, dir) = setup_project();
    let p = payload(&call(&dir, "handoff_memory_cleanup", json!({})));
    assert_eq!(p["auto_merged_exact"], 0);
    assert_eq!(
        p["cleanup_recommendations"]["similar_clusters"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        p["cleanup_recommendations"]["stale"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn cleanup_gcs_old_injected_sidecars() {
    let (_tmp, dir) = setup_project();
    // Seed an old sidecar (older than the 14-day gc window) directly.
    let injected_dir = dir.join(".handoff/memory/injected");
    std::fs::create_dir_all(&injected_dir).unwrap();
    std::fs::write(
        injected_dir.join("old-sess.json"),
        serde_json::to_string_pretty(&json!({
            "version": 1, "session_id": "old-sess",
            "updated_at": "2025-01-01T00:00:00Z", "injected": {}
        }))
        .unwrap(),
    )
    .unwrap();

    let p = payload(&call(&dir, "handoff_memory_cleanup", json!({})));
    assert_eq!(p["injected_sidecars_removed"], 1, "old sidecar gc'd");
    assert!(!injected_dir.join("old-sess.json").exists());
}

// ---------------------------------------------------------------------------
// P4: memory_* settings are read from config.toml (handoff_update_config).
// ---------------------------------------------------------------------------

/// Set a single settings key via handoff_update_config.
fn set_config(dir: &std::path::Path, key: &str, value: Value) -> Value {
    call(
        dir,
        "handoff_update_config",
        json!({ "updates": { key: value } }),
    )
}

/// Read the full config JSON via handoff_get_config.
fn get_config(dir: &std::path::Path) -> Value {
    payload(&call(dir, "handoff_get_config", json!({})))
}

#[test]
fn get_config_exposes_memory_defaults() {
    let (_tmp, dir) = setup_project();
    let cfg = get_config(&dir);
    let s = &cfg["settings"];
    assert_eq!(s["memory_enabled"], true);
    assert_eq!(s["memory_dup_threshold"], 0.72);
    assert_eq!(s["memory_query_min_score"], 0.5);
    assert_eq!(s["memory_query_limit"], 5);
    assert_eq!(s["memory_stale_days"], 60);
    assert_eq!(s["memory_injected_gc_days"], 14);
}

#[test]
fn update_config_roundtrips_memory_settings() {
    let (_tmp, dir) = setup_project();
    assert!(!is_error(&set_config(
        &dir,
        "settings.memory_enabled",
        json!(false)
    )));
    assert!(!is_error(&set_config(
        &dir,
        "settings.memory_dup_threshold",
        json!(0.9)
    )));
    assert!(!is_error(&set_config(
        &dir,
        "settings.memory_query_min_score",
        json!(1.5)
    )));
    assert!(!is_error(&set_config(
        &dir,
        "settings.memory_query_limit",
        json!(2)
    )));
    assert!(!is_error(&set_config(
        &dir,
        "settings.memory_stale_days",
        json!(30)
    )));
    assert!(!is_error(&set_config(
        &dir,
        "settings.memory_injected_gc_days",
        json!(7)
    )));

    let s = get_config(&dir)["settings"].clone();
    assert_eq!(s["memory_enabled"], false);
    assert_eq!(s["memory_dup_threshold"], 0.9);
    assert_eq!(s["memory_query_min_score"], 1.5);
    assert_eq!(s["memory_query_limit"], 2);
    assert_eq!(s["memory_stale_days"], 30);
    assert_eq!(s["memory_injected_gc_days"], 7);
}

#[test]
fn config_dup_threshold_controls_conflict() {
    let (_tmp, dir) = setup_project();
    // Two moderately-overlapping bodies that exceed the default 0.72 → conflict.
    let a = json!({ "text": "the memory feature carries lessons across sessions for the project" });
    let b = json!({ "text": "the memory feature carries lessons across sessions for this project too" });

    // Raise the threshold so the same pair is now treated as distinct.
    set_config(&dir, "settings.memory_dup_threshold", json!(0.99));
    assert_eq!(
        payload(&call(&dir, "handoff_memory_save", a.clone()))["status"],
        "saved"
    );
    assert_eq!(
        payload(&call(&dir, "handoff_memory_save", b.clone()))["status"],
        "saved",
        "above a 0.99 threshold the near-duplicate is no longer a conflict"
    );
}

#[test]
fn config_query_limit_caps_results() {
    let (_tmp, dir) = setup_project();
    for n in 0..3 {
        call(
            &dir,
            "handoff_memory_save",
            json!({ "text": format!("atomic write rule number {n} for handoff files"), "force": true }),
        );
    }
    set_config(&dir, "settings.memory_query_limit", json!(1));
    let q = payload(&call(
        &dir,
        "handoff_memory_query",
        json!({ "text": "atomic write rule" }),
    ));
    assert_eq!(
        q["memories"].as_array().unwrap().len(),
        1,
        "config memory_query_limit must cap the result count"
    );
}

#[test]
fn config_stale_days_controls_cleanup() {
    let (_tmp, dir) = setup_project();
    // A memory created ~40 days before "now" — fresh under the default 60-day
    // window, but stale once the window is tightened to 30 days.
    let now = chrono::Utc::now();
    let created = (now - chrono::Duration::days(40)).to_rfc3339();
    write_raw_memory(
        &dir,
        &json!({
            "version": 1, "id": "m-40d",
            "text": "a forty day old rule", "kind": "rule",
            "tags": [], "scope_paths": [],
            "content_hash": lexsim::content_hash("a forty day old rule"),
            "created_at": created, "updated_at": created,
            "hit_count": 0, "superseded_ids": []
        }),
    );

    // Default 60-day window → not stale.
    let p1 = payload(&call(&dir, "handoff_memory_cleanup", json!({})));
    assert_eq!(
        p1["cleanup_recommendations"]["stale"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "40-day memory is fresh under the default 60-day window"
    );

    // Tighten the configured window to 30 days → now stale.
    set_config(&dir, "settings.memory_stale_days", json!(30));
    let p2 = payload(&call(&dir, "handoff_memory_cleanup", json!({})));
    assert_eq!(
        p2["cleanup_recommendations"]["stale"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "config memory_stale_days=30 must flag the 40-day memory"
    );
}

#[test]
fn explicit_stale_days_arg_overrides_config() {
    let (_tmp, dir) = setup_project();
    set_config(&dir, "settings.memory_stale_days", json!(5));
    call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "a brand new rule we just saved", "force": true }),
    );
    // Config says 5 days (would NOT flag a fresh memory), but the explicit arg
    // stale_days=0 must win and flag everything.
    let p = payload(&call(
        &dir,
        "handoff_memory_cleanup",
        json!({ "stale_days": 0 }),
    ));
    assert_eq!(
        p["cleanup_recommendations"]["stale"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "explicit stale_days arg overrides the configured default"
    );
}

#[test]
fn memory_settings_work_on_legacy_config() {
    // A project whose config.toml predates the memory keys must still default
    // correctly (serde defaults) — get_config returns the defaults and save works.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("legacy-cfg");
    std::fs::create_dir_all(dir.join(".handoff/sessions")).unwrap();
    std::fs::create_dir_all(dir.join(".handoff/tasks")).unwrap();
    std::fs::write(
        dir.join(".handoff/config.toml"),
        "[project]\nname = \"legacy\"\n",
    )
    .unwrap();

    // get_config reflects the raw file (memory keys absent here), but the memory
    // handlers apply serde defaults when they read it, so save still works.
    let resp = call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "works on legacy config", "force": true }),
    );
    assert!(!is_error(&resp));
    let p = payload(&resp);
    assert_eq!(p["status"], "saved");
}

#[test]
fn memory_enabled_false_gates_all_tools() {
    let (_tmp, dir) = setup_project();
    // Seed a memory while enabled, then disable the feature.
    let saved = payload(&call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "a real memory worth keeping", "force": true }),
    ));
    let id = saved["id"].as_str().unwrap().to_string();
    set_config(&dir, "settings.memory_enabled", json!(false));

    // save → benign disabled no-op (no error, nothing written).
    let s = payload(&call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "should not be stored while disabled", "force": true }),
    ));
    assert_eq!(s["disabled"], true);
    assert!(s["id"].is_null(), "disabled save must not return a new id");

    // query → empty.
    let q = payload(&call(
        &dir,
        "handoff_memory_query",
        json!({ "text": "a real memory worth keeping" }),
    ));
    assert_eq!(q["disabled"], true);
    assert_eq!(q["memories"].as_array().unwrap().len(), 0);

    // cleanup → no-op shape.
    let c = payload(&call(&dir, "handoff_memory_cleanup", json!({})));
    assert_eq!(c["disabled"], true);
    assert_eq!(c["auto_merged_exact"], 0);

    // delete → disabled, and the original memory is untouched.
    let d = payload(&call(&dir, "handoff_memory_delete", json!({ "id": id })));
    assert_eq!(d["disabled"], true);

    // Re-enable and confirm the original memory survived the disabled window.
    set_config(&dir, "settings.memory_enabled", json!(true));
    let q2 = payload(&call(
        &dir,
        "handoff_memory_query",
        json!({ "text": "a real memory worth keeping" }),
    ));
    assert_eq!(
        q2["memories"].as_array().unwrap().len(),
        1,
        "disabling must not have written or deleted anything"
    );
}

#[test]
fn corrupt_config_propagates_error_not_silent_default() {
    let (_tmp, dir) = setup_project();
    // Make config.toml unparseable. A memory tool must surface a real error
    // rather than silently falling back to defaults and hiding the corruption.
    std::fs::write(
        dir.join(".handoff/config.toml"),
        "this is = not valid = toml [[[",
    )
    .unwrap();
    let resp = call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "anything", "force": true }),
    );
    assert!(
        is_error(&resp),
        "a corrupt config.toml must produce an error, not a silent default"
    );
}

#[test]
fn update_config_rejects_mistyped_memory_setting() {
    let (_tmp, dir) = setup_project();
    // A string where a number is required must be rejected, not silently dropped.
    let resp = set_config(&dir, "settings.memory_dup_threshold", json!("high"));
    assert!(is_error(&resp), "mistyped value must be rejected");
    // And an out-of-range numeric value is still rejected.
    let oob = set_config(&dir, "settings.memory_dup_threshold", json!(2.0));
    assert!(is_error(&oob), "out-of-range threshold must be rejected");
    // The valid setting is unchanged (still the default).
    assert_eq!(get_config(&dir)["settings"]["memory_dup_threshold"], 0.72);
}

#[test]
fn lazy_memory_dir_for_legacy_project() {
    // Simulate a project initialized before v0.13.0 (no memory/ dir).
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("legacy");
    std::fs::create_dir_all(dir.join(".handoff/sessions")).unwrap();
    std::fs::create_dir_all(dir.join(".handoff/tasks")).unwrap();
    // Minimal config so ensure_handoff_exists passes.
    std::fs::write(
        dir.join(".handoff/config.toml"),
        "[project]\nname = \"legacy\"\n",
    )
    .unwrap();
    assert!(!dir.join(".handoff/memory").exists());

    let resp = call(
        &dir,
        "handoff_memory_save",
        json!({ "text": "lazily created", "force": true }),
    );
    assert!(!is_error(&resp));
    assert!(
        dir.join(".handoff/memory").is_dir(),
        "memory/ created lazily"
    );
}
