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
        "memory_save",
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
    let resp = call(&dir, "memory_save", json!({ "text": "   " }));
    assert!(is_error(&resp));
}

#[test]
fn save_rejects_bad_kind() {
    let (_tmp, dir) = setup_project();
    let resp = call(&dir, "memory_save", json!({ "text": "x", "kind": "nope" }));
    assert!(is_error(&resp));
}

#[test]
fn exact_duplicate_not_rewritten() {
    let (_tmp, dir) = setup_project();
    let text = "use SSH for git push, never embed PAT in the URL";
    let first = payload(&call(&dir, "memory_save", json!({ "text": text })));
    assert_eq!(first["status"], "saved");

    // Same content (only whitespace/case differs) → duplicate_exact.
    let dup = payload(&call(
        &dir,
        "memory_save",
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
        "memory_save",
        json!({ "text": "the memory feature carries lessons across sessions for the project" }),
    ));
    assert_eq!(a["status"], "saved");

    // Heavily overlapping wording → conflict (not written).
    let b = payload(&call(
        &dir,
        "memory_save",
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
        "memory_save",
        json!({ "text": "the memory feature carries lessons across sessions for the project" }),
    );
    let forced = payload(&call(
        &dir,
        "memory_save",
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
        "memory_save",
        json!({ "text": "memory carries lessons across sessions", "tags": ["t1"] }),
    ));
    let b = payload(&call(
        &dir,
        "memory_save",
        json!({ "text": "completely separate gotcha about the gantt chart export", "force": true }),
    ));
    let a_id = a["id"].as_str().unwrap();
    let b_id = b["id"].as_str().unwrap();

    let merged = payload(&call(
        &dir,
        "memory_save",
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
    let del_b = call(&dir, "memory_delete", json!({ "id": b_id }));
    assert!(
        is_error(&del_b),
        "absorbed memory should already be deleted"
    );
    let q = payload(&call(
        &dir,
        "memory_query",
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
        "memory_save",
        json!({ "text": "always use atomic_write for handoff files", "force": true }),
    );
    call(
        &dir,
        "memory_save",
        json!({ "text": "the cat sat on the mat in the warm afternoon sun", "force": true }),
    );

    let q = payload(&call(
        &dir,
        "memory_query",
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
        "memory_save",
        json!({ "text": "メモリ機能はセッション間で教訓を引き継ぐ", "force": true }),
    );
    call(
        &dir,
        "memory_save",
        json!({ "text": "ガントチャートでスケジュールを表示する", "force": true }),
    );
    let q = payload(&call(&dir, "memory_query", json!({ "text": "メモリ機能" })));
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
        "memory_save",
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
        "memory_query",
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
    let q = payload(&call(&dir, "memory_query", json!({ "text": "anything" })));
    assert_eq!(q["memories"].as_array().unwrap().len(), 0);
    assert_eq!(q["injected_count"], 0);
}

#[test]
fn delete_by_id_and_prefix() {
    let (_tmp, dir) = setup_project();
    let a = payload(&call(
        &dir,
        "memory_save",
        json!({ "text": "first memory", "force": true }),
    ));
    let id = a["id"].as_str().unwrap().to_string();

    let del = payload(&call(&dir, "memory_delete", json!({ "id": id })));
    assert_eq!(del["status"], "deleted");

    // Deleting again → error (not found).
    let again = call(&dir, "memory_delete", json!({ "id": id }));
    assert!(is_error(&again));
}

#[test]
fn delete_missing_errors() {
    let (_tmp, dir) = setup_project();
    let resp = call(&dir, "memory_delete", json!({ "id": "m-does-not-exist" }));
    assert!(is_error(&resp));
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
        "memory_save",
        json!({ "text": "lazily created", "force": true }),
    );
    assert!(!is_error(&resp));
    assert!(
        dir.join(".handoff/memory").is_dir(),
        "memory/ created lazily"
    );
}
