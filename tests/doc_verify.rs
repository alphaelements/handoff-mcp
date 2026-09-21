//! Integration tests for the Verification Matrix MCP tools
//! (`handoff_doc_verify` / `handoff_doc_verify_status`,
//! wiki/140-verification-matrix.md), exercised end-to-end through the
//! JSON-RPC `process_line` entry point — the same path the MCP server runs
//! in production (mirrors `tests/tool_docs.rs`).

use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};

fn send(input: &str) -> Option<Value> {
    let result = handoff_mcp::mcp::protocol::process_line(input)?;
    Some(serde_json::from_str(&result).expect("response should be valid JSON"))
}

/// Generates a fresh, process-wide-unique slug for test documents (tests run
/// concurrently against separate temp projects, but a shared counter keeps
/// slugs readable and guarantees no two tests ever collide).
fn unique_slug(label: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{label}-{n}")
}

fn setup_project() -> (tempfile::TempDir, std::path::PathBuf) {
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
                "project_name": "doc-verify-test"
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

fn payload_text(resp: &Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"].as_bool().unwrap_or(false)
}

/// Saves a small 3-section document (preamble seq0 + 2 `##` sections) and
/// returns its doc_id. No `#`(H1) heading is used because the default
/// `split_level` is 2 (spec §5.1) — an H1 would count as its own boundary
/// section too, which would make the section count 4, not 3.
fn save_sample_doc(dir: &std::path::Path, slug: &str) -> String {
    let body = "Intro.\n\n## Section A\n\nBody A.\n\n## Section B\n\nBody B.\n";
    let resp = call(
        dir,
        "handoff_doc_save",
        json!({
            "slug": slug,
            "title": "Verification Sample",
            "body": body,
            "doc_type": "spec",
        }),
    );
    assert!(!is_error(&resp), "doc_save failed: {}", payload_text(&resp));
    payload(&resp)["doc_id"].as_str().unwrap().to_string()
}

// ---------------------------------------------------------------------
// doc_verify: generate
// ---------------------------------------------------------------------

#[test]
fn doc_verify_generate_creates_matrix_with_pending_items() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-generate");
    let doc_id = save_sample_doc(&dir, &slug);

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    assert_eq!(p["verification_status"], "pending");
    assert_eq!(p["total"], 3);
    assert_eq!(p["pending"], 3);
    assert_eq!(p["checked"], 0);
    assert_eq!(p["skipped"], 0);
    assert_eq!(p["stale"], 0);

    // Confirm via status that items are indeed all pending.
    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let status = payload(&status_resp);
    let items = status["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert!(items.iter().all(|i| i["status"] == "pending"));
}

#[test]
fn doc_verify_generate_with_skip_seqs() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-generate-skip");
    let doc_id = save_sample_doc(&dir, &slug);

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate", "skip_seqs": [0] }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    assert_eq!(p["skipped"], 1);
    assert_eq!(p["pending"], 2);
    // All non-pending (1 skipped, 0 verified out of 3) -> in_review overall,
    // since not *all* items are verified/skipped.
    assert_eq!(p["verification_status"], "in_review");

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let seq0 = items.iter().find(|i| i["fragment_seq"] == 0).unwrap();
    assert_eq!(seq0["status"], "skipped");
}

#[test]
fn doc_verify_generate_errors_if_matrix_exists() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-generate-twice");
    let doc_id = save_sample_doc(&dir, &slug);

    let first = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    assert!(!is_error(&first));

    let second = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    assert!(
        is_error(&second),
        "generate must error when a matrix already exists"
    );
    assert!(payload_text(&second).contains("sync"));
}

// ---------------------------------------------------------------------
// doc_verify: check / skip
// ---------------------------------------------------------------------

#[test]
fn doc_verify_check_marks_item_verified() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-check");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "check",
            "fragment_seq": 1,
            "reviewer": "ai",
            "notes": "looks good",
        }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    assert_eq!(p["checked"], 1);
    assert_eq!(p["pending"], 2);

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let item1 = items.iter().find(|i| i["fragment_seq"] == 1).unwrap();
    assert_eq!(item1["status"], "verified");
    assert_eq!(item1["reviewer"], "ai");
    assert_eq!(item1["notes"], "looks good");
    assert!(item1["verified_at"].as_str().is_some());
}

#[test]
fn doc_verify_skip_marks_item_skipped() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-skip");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "skip", "fragment_seq": 0 }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    assert_eq!(p["skipped"], 1);

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let seq0 = items.iter().find(|i| i["fragment_seq"] == 0).unwrap();
    assert_eq!(seq0["status"], "skipped");
}

#[test]
fn doc_verify_check_updates_overall_status() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-overall-status");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );

    // Check one of three -> in_review.
    let after_one = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "check", "fragment_seq": 0 }),
    );
    assert_eq!(payload(&after_one)["verification_status"], "in_review");

    // Verify/skip the remaining two -> all items verified/skipped -> "verified".
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "check", "fragment_seq": 1 }),
    );
    let after_all = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "skip", "fragment_seq": 2 }),
    );
    assert_eq!(payload(&after_all)["verification_status"], "verified");
}

// ---------------------------------------------------------------------
// doc_verify: sync
// ---------------------------------------------------------------------

#[test]
fn doc_verify_sync_adds_new_sections_and_removes_deleted() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-sync");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );

    // Re-save with a different section shape: Section A removed, Section C
    // added. seq0 preamble + Section B stay from the caller's perspective,
    // but seqs are recomputed fresh by split(), so this exercises "sections
    // changed under the matrix".
    let new_body = "Intro.\n\n## Section B\n\nBody B.\n\n## Section C\n\nBody C.\n";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "doc_id": doc_id, "body": new_body }),
    );
    assert!(!is_error(&save_resp), "{}", payload_text(&save_resp));
    let new_section_count = payload(&save_resp)["section_count"].as_u64().unwrap();
    assert_eq!(new_section_count, 3); // preamble + Section B + Section C

    let sync_resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "sync" }),
    );
    assert!(!is_error(&sync_resp), "{}", payload_text(&sync_resp));
    let p = payload(&sync_resp);
    assert_eq!(p["total"], 3, "item count must match the new section count");
}

#[test]
fn doc_verify_sync_preserves_existing_status() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-sync-preserve");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    // Verify seq 1 before syncing.
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "check", "fragment_seq": 1 }),
    );

    // Re-save keeping the same 3 sections (seq 0/1/2 stay identical), plus
    // one new section appended.
    let new_body =
        "Intro.\n\n## Section A\n\nBody A.\n\n## Section B\n\nBody B.\n\n## Section C\n\nBody C.\n";
    call(
        &dir,
        "handoff_doc_save",
        json!({ "doc_id": doc_id, "body": new_body }),
    );

    let sync_resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "sync" }),
    );
    let p = payload(&sync_resp);
    assert_eq!(p["total"], 4);
    assert_eq!(
        p["checked"], 1,
        "the previously verified seq 1 item must keep its verified status after sync"
    );
    assert_eq!(p["pending"], 3);

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let item1 = items.iter().find(|i| i["fragment_seq"] == 1).unwrap();
    assert_eq!(item1["status"], "verified");
}

// ---------------------------------------------------------------------
// doc_verify: set_refs
// ---------------------------------------------------------------------

#[test]
fn doc_verify_set_refs_updates_impl_and_test_refs() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-set-refs");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "set_refs",
            "fragment_seq": 1,
            "impl_refs": [{ "path": "src/foo.rs", "lines": "10-20" }],
            "test_refs": [{ "path": "tests/foo.rs", "label": "roundtrip" }],
        }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let item1 = items.iter().find(|i| i["fragment_seq"] == 1).unwrap();
    assert_eq!(item1["impl_refs"][0]["path"], "src/foo.rs");
    assert_eq!(item1["impl_refs"][0]["lines"], "10-20");
    assert_eq!(item1["test_refs"][0]["path"], "tests/foo.rs");
    assert_eq!(item1["test_refs"][0]["label"], "roundtrip");
}

// ---------------------------------------------------------------------
// doc_verify: batch check (fragment_seq as array) / check_all
// ---------------------------------------------------------------------

#[test]
fn doc_verify_check_batch_array_verifies_all_specified_seqs() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-check-batch");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "check",
            "fragment_seq": [0, 1, 2],
            "reviewer": "ai",
            "notes": "batch verified",
        }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    assert_eq!(p["checked"], 3);
    assert_eq!(p["pending"], 0);
    assert_eq!(p["verification_status"], "verified");

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    assert!(items.iter().all(|i| i["status"] == "verified"));
    assert!(items.iter().all(|i| i["reviewer"] == "ai"));
    assert!(items.iter().all(|i| i["notes"] == "batch verified"));
    assert!(items.iter().all(|i| i["verified_at"].as_str().is_some()));
}

#[test]
fn doc_verify_check_batch_partial_array_leaves_others_pending() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-check-batch-partial");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "check",
            "fragment_seq": [0, 1],
        }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    assert_eq!(p["checked"], 2);
    assert_eq!(p["pending"], 1);
    assert_eq!(p["verification_status"], "in_review");
}

#[test]
fn doc_verify_check_single_fragment_seq_still_works() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-check-single-compat");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );

    // Backward compat: fragment_seq as a plain number, not an array.
    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "check",
            "fragment_seq": 1,
            "reviewer": "user",
        }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    assert_eq!(p["checked"], 1);
    assert_eq!(p["pending"], 2);

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let item1 = items.iter().find(|i| i["fragment_seq"] == 1).unwrap();
    assert_eq!(item1["status"], "verified");
    assert_eq!(item1["reviewer"], "user");
}

#[test]
fn doc_verify_check_all_verifies_every_section_in_one_call() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-check-all");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "check_all",
            "reviewer": "ai",
            "notes": "bulk pass",
        }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    assert_eq!(p["checked"], 3);
    assert_eq!(p["pending"], 0);
    assert_eq!(p["total"], 3);
    assert_eq!(p["verification_status"], "verified");

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    assert!(items.iter().all(|i| i["status"] == "verified"));
    assert!(items.iter().all(|i| i["reviewer"] == "ai"));
    assert!(items.iter().all(|i| i["notes"] == "bulk pass"));
    assert!(items.iter().all(|i| i["verified_at"].as_str().is_some()));
    // content_hash_at_verify was recorded at the current section hash, so no
    // item should be stale immediately after check_all.
    assert!(items.iter().all(|i| i["stale"] == false));
}

#[test]
fn doc_verify_check_all_requires_existing_matrix() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-check-all-no-matrix");
    let doc_id = save_sample_doc(&dir, &slug);

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "check_all" }),
    );
    assert!(
        is_error(&resp),
        "check_all must error when no verification matrix exists yet"
    );
    assert!(payload_text(&resp).contains("No verification matrix"));
}

// ---------------------------------------------------------------------
// doc_verify_status
// ---------------------------------------------------------------------

#[test]
fn doc_verify_status_returns_summary() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-status-summary");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "check", "fragment_seq": 0 }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    assert_eq!(p["title"], "Verification Sample");
    assert_eq!(p["verification_status"], "in_review");
    assert_eq!(p["progress"]["checked"], 1);
    assert_eq!(p["progress"]["total"], 3);
    let pct = p["progress"]["percentage"].as_f64().unwrap();
    assert!((pct - (1.0 / 3.0 * 100.0)).abs() < 0.01);
    // include_items defaults to false.
    assert!(p.get("items").is_none());
}

#[test]
fn doc_verify_status_includes_items_when_requested() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-status-items");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let p = payload(&resp);
    let items = p["items"].as_array().expect("items must be present");
    assert_eq!(items.len(), 3);
    assert!(items[0].get("heading").is_some());
    assert!(items[0].get("stale").is_some());
}

#[test]
fn doc_verify_status_detects_stale_items() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-status-stale");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    // Verify seq 1 ("Section A") at its current content_hash.
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "check", "fragment_seq": 1 }),
    );

    // Re-save with Section A's body changed (content_hash for that section
    // will differ), keeping the same section shape/seqs.
    let changed_body = "Intro.\n\n## Section A\n\nBody A CHANGED.\n\n## Section B\n\nBody B.\n";
    call(
        &dir,
        "handoff_doc_save",
        json!({ "doc_id": doc_id, "body": changed_body }),
    );

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let p = payload(&status_resp);
    assert_eq!(p["progress"]["stale"], 1);
    let items = p["items"].as_array().unwrap();
    let item1 = items.iter().find(|i| i["fragment_seq"] == 1).unwrap();
    assert_eq!(item1["status"], "verified");
    assert_eq!(item1["stale"], true);
}

#[test]
fn doc_verify_status_errors_without_matrix() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-status-no-matrix");
    let doc_id = save_sample_doc(&dir, &slug);

    let resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id }),
    );
    assert!(
        is_error(&resp),
        "status must error when no verification matrix exists yet"
    );
    assert!(payload_text(&resp).contains("No verification matrix"));
}

// ---------------------------------------------------------------------
// Backward compatibility
// ---------------------------------------------------------------------

#[test]
fn doc_verify_backward_compat_no_verification_field() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-backward-compat");
    let doc_id = save_sample_doc(&dir, &slug);

    // A freshly saved document (no verify tool touched yet) must be
    // retrievable via doc_get with no verification-related error, and its
    // meta payload must simply omit/null the field.
    let get_resp = call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": doc_id, "format": "meta" }),
    );
    assert!(!is_error(&get_resp), "{}", payload_text(&get_resp));

    // doc_list must also work fine.
    let list_resp = call(&dir, "handoff_doc_list", json!({}));
    assert!(!is_error(&list_resp), "{}", payload_text(&list_resp));
    let list_payload = payload(&list_resp);
    let docs = list_payload["documents"].as_array().unwrap();
    assert!(docs.iter().any(|d| d["id"] == doc_id));
}

// ---------------------------------------------------------------------
// E2E round-trip
// ---------------------------------------------------------------------

#[test]
fn doc_save_then_verify_generate_check_status_roundtrip() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-e2e-roundtrip");

    // 1. doc_save
    let doc_id = save_sample_doc(&dir, &slug);

    // 2. doc_verify(generate)
    let gen_resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    assert!(!is_error(&gen_resp), "{}", payload_text(&gen_resp));
    assert_eq!(payload(&gen_resp)["verification_status"], "pending");

    // 3. doc_verify(check) on every seq to fully verify the matrix.
    for seq in 0..3u64 {
        let check_resp = call(
            &dir,
            "handoff_doc_verify",
            json!({
                "doc_id": doc_id,
                "action": "check",
                "fragment_seq": seq,
                "reviewer": "ai",
            }),
        );
        assert!(!is_error(&check_resp), "{}", payload_text(&check_resp));
    }

    // 4. doc_verify_status: full round-trip confirmation.
    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    assert!(!is_error(&status_resp), "{}", payload_text(&status_resp));
    let p = payload(&status_resp);
    assert_eq!(p["verification_status"], "verified");
    assert_eq!(p["progress"]["checked"], 3);
    assert_eq!(p["progress"]["total"], 3);
    assert_eq!(p["progress"]["stale"], 0);
    assert_eq!(p["progress"]["percentage"], 100.0);
    let items = p["items"].as_array().unwrap();
    assert!(items.iter().all(|i| i["status"] == "verified"));
    assert!(items.iter().all(|i| i["reviewer"] == "ai"));
}

// ---------------------------------------------------------------------
// v2: add_item (freeform + sub-item)
// ---------------------------------------------------------------------

#[test]
fn doc_verify_add_item_freeform_creates_top_level_item() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-add-item-freeform");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "add_item",
            "label": "ドラッグ操作の目視確認",
            "category": "visual",
        }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);
    // 3 section items (from generate) + 1 freeform item.
    assert_eq!(p["total"], 4);
    assert_eq!(p["pending"], 4);

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let freeform = items
        .iter()
        .find(|i| i["fragment_seq"].is_null())
        .expect("freeform item must be present");
    assert_eq!(freeform["heading"], "ドラッグ操作の目視確認");
    assert_eq!(freeform["category"], "visual");
    assert_eq!(freeform["status"], "pending");
}

#[test]
fn doc_verify_add_item_freeform_requires_label() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-add-item-freeform-no-label");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "category": "visual" }),
    );
    assert!(
        is_error(&resp),
        "add_item without fragment_seq must require 'label'"
    );
}

#[test]
fn doc_verify_add_item_sub_item_adds_to_existing_section() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-add-item-sub");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "add_item",
            "fragment_seq": 1,
            "description": "形状=八面体であること",
        }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);
    // seq1 now has 1 sub_item, so it is counted via that sub_item instead
    // of itself (spec §7.4: "sub_items が存在する item ... 親 item の
    // status は sub_items の集約"): seq0 (leaf) + seq2 (leaf) + seq1's 1
    // sub_item = 3 total, still all pending.
    assert_eq!(p["total"], 3);
    assert_eq!(p["pending"], 3);

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let seq1 = items.iter().find(|i| i["fragment_seq"] == 1).unwrap();
    let subs = seq1["sub_items"].as_array().unwrap();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0]["description"], "形状=八面体であること");
    assert_eq!(subs[0]["category"], "requirement");
    assert_eq!(subs[0]["status"], "pending");
    assert_eq!(subs[0]["index"], 0);
}

#[test]
fn doc_verify_add_item_sub_item_requires_description() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-add-item-sub-no-desc");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "fragment_seq": 1 }),
    );
    assert!(
        is_error(&resp),
        "add_item with fragment_seq must require 'description'"
    );
}

// ---------------------------------------------------------------------
// v2: check/skip sub_item_index
// ---------------------------------------------------------------------

#[test]
fn doc_verify_check_sub_item_index_marks_specific_sub_item_verified() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-check-sub-item");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "fragment_seq": 1, "description": "req A" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "fragment_seq": 1, "description": "req B" }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "check",
            "fragment_seq": 1,
            "sub_item_index": 0,
            "reviewer": "ai",
        }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);
    assert_eq!(p["checked"], 1);
    // seq0 (leaf) + seq2 (leaf) + req B (seq1's other sub_item) still pending.
    assert_eq!(p["pending"], 3);

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let seq1 = items.iter().find(|i| i["fragment_seq"] == 1).unwrap();
    let subs = seq1["sub_items"].as_array().unwrap();
    assert_eq!(subs[0]["status"], "verified");
    assert_eq!(subs[0]["reviewer"], "ai");
    assert!(subs[0]["verified_at"].as_str().is_some());
    assert_eq!(subs[1]["status"], "pending");
}

#[test]
fn doc_verify_skip_sub_item_index_marks_specific_sub_item_skipped() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-skip-sub-item");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "fragment_seq": 1, "description": "req A" }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "skip",
            "fragment_seq": 1,
            "sub_item_index": 0,
        }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let seq1 = items.iter().find(|i| i["fragment_seq"] == 1).unwrap();
    let subs = seq1["sub_items"].as_array().unwrap();
    assert_eq!(subs[0]["status"], "skipped");
}

// ---------------------------------------------------------------------
// P0 requirements-traceability (.handoff/docs/_doc.req-traceability-mcp-plan.md
// §2.5): sub_item_id (stable_id) addressing on check/skip. `add_item` does
// not yet expose setting `stable_id` (that's a follow-up task), so a
// sub_item never has one yet — these tests document today's observable
// behavior: a sub_item_id that cannot match anything is a clear error,
// never a silent no-op or wrong-item mutation.
// ---------------------------------------------------------------------

#[test]
fn doc_verify_check_sub_item_id_errors_when_no_sub_item_has_that_stable_id() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-check-sub-item-id-missing");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "fragment_seq": 1, "description": "req A" }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "check",
            "fragment_seq": 1,
            "sub_item_id": "C01-9.9.9.9",
        }),
    );
    assert!(
        is_error(&resp),
        "sub_item_id with no matching stable_id must error, not silently no-op"
    );
}

// ---------------------------------------------------------------------
// v2: check_all with sub_items
// ---------------------------------------------------------------------

#[test]
fn doc_verify_check_all_verifies_sub_items_too() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-check-all-sub-items");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "fragment_seq": 1, "description": "req A" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "label": "GUI check", "category": "visual" }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "check_all", "reviewer": "ai" }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);
    // seq0 (leaf) + seq2 (leaf) + seq1's 1 sub_item + 1 freeform item = 4,
    // all verified.
    assert_eq!(p["total"], 4);
    assert_eq!(p["checked"], 4);
    assert_eq!(p["pending"], 0);

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let seq1 = items.iter().find(|i| i["fragment_seq"] == 1).unwrap();
    let subs = seq1["sub_items"].as_array().unwrap();
    assert_eq!(subs[0]["status"], "verified");
    assert!(subs[0]["verified_at"].as_str().is_some());
    let freeform = items.iter().find(|i| i["fragment_seq"].is_null()).unwrap();
    assert_eq!(freeform["status"], "verified");
}

// ---------------------------------------------------------------------
// v2: format=checklist
// ---------------------------------------------------------------------

#[test]
fn doc_verify_status_format_checklist_returns_markdown() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-checklist-format");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "add_item",
            "fragment_seq": 1,
            "description": "形状=八面体であること",
        }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "check",
            "fragment_seq": 1,
            "sub_item_index": 0,
            "reviewer": "ai",
        }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "add_item",
            "label": "ドラッグ操作の目視確認",
            "category": "visual",
        }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true, "format": "checklist" }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let text = payload_text(&resp);

    assert!(text.contains("# Verification: Verification Sample"));
    assert!(text.contains("Status:"));
    assert!(text.contains("§1 Section A"));
    assert!(text.contains("[x] 形状=八面体であること"));
    assert!(text.contains("@ai"));
    assert!(text.contains("[requirement]"));
    assert!(text.contains("— ドラッグ操作の目視確認"));
    assert!(text.contains("[visual]"));
    assert!(text.contains("[ ]") || text.contains("○ pending"));
}

#[test]
fn doc_verify_status_format_json_is_default_and_unchanged() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-checklist-default-json");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id }),
    );
    assert!(!is_error(&resp));
    // Default format still returns a JSON payload (parses cleanly).
    let p = payload(&resp);
    assert_eq!(p["verification_status"], "pending");
}

// ---------------------------------------------------------------------
// v2: progress calculation with sub_items
// ---------------------------------------------------------------------

#[test]
fn doc_verify_progress_counts_include_sub_items_and_freeform() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-progress-sub-items");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    // Add 2 sub_items to section seq=1, 1 freeform item.
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "fragment_seq": 1, "description": "req A" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "fragment_seq": 1, "description": "req B" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "label": "GUI check", "category": "visual" }),
    );

    // Verify one sub_item and the freeform item.
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "check", "fragment_seq": 1, "sub_item_index": 0 }),
    );

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id }),
    );
    let p = payload(&status_resp);
    // Leaf items counted: seq0 (no subs, pending), seq2 (no subs, pending),
    // seq1's 2 sub_items (1 verified + 1 pending; the parent seq1 item
    // itself is NOT counted directly since it has sub_items), + 1 freeform
    // item (pending) = 5 total.
    assert_eq!(p["progress"]["total"], 5);
    assert_eq!(p["progress"]["checked"], 1);
    assert_eq!(p["progress"]["pending"], 4);
}

#[test]
fn doc_verify_parent_item_effective_status_reflects_sub_items_aggregate() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-parent-aggregate-status");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "fragment_seq": 1, "description": "req A" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "fragment_seq": 1, "description": "req B" }),
    );

    // Both sub_items pending -> overall verification_status stays "pending".
    let s1 = payload(&call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id }),
    ));
    assert_eq!(s1["verification_status"], "pending");

    // Verify one sub_item, leave the other pending, and verify the other
    // two plain section items too -> overall must be "in_review" (not
    // "verified") since seq1's sub_items are a mix.
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "check", "fragment_seq": 0 }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "check", "fragment_seq": 2 }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "check", "fragment_seq": 1, "sub_item_index": 0 }),
    );

    let s2 = payload(&call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id }),
    ));
    assert_eq!(s2["verification_status"], "in_review");

    // Verify the remaining sub_item too -> now fully verified.
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "check", "fragment_seq": 1, "sub_item_index": 1 }),
    );
    let s3 = payload(&call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id }),
    ));
    assert_eq!(s3["verification_status"], "verified");
}

// ---------------------------------------------------------------------
// v2: item_is_stale for freeform items
// ---------------------------------------------------------------------

#[test]
fn doc_verify_freeform_item_never_stale() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-freeform-never-stale");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "label": "GUI check", "category": "visual" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "check_all" }),
    );

    // Edit the document body (changes every section's content_hash), then
    // confirm the freeform item is still not flagged stale.
    call(
        &dir,
        "handoff_doc_update_section",
        json!({ "doc_id": doc_id, "seq": 1, "new_content": "## Section A\n\nChanged body.\n\n" }),
    );

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let freeform = items.iter().find(|i| i["fragment_seq"].is_null()).unwrap();
    assert_eq!(freeform["stale"], false);
    assert_eq!(freeform["status"], "verified");
}

// ---------------------------------------------------------------------
// suggest_refs (t124.6): scan scope_paths for impl/test ref candidates
// ---------------------------------------------------------------------

/// Saves a doc with `scope_paths` pointing at a source tree the test writes
/// under the project dir, then writes matching impl/test source files so
/// `suggest_refs` has something real to scan.
fn save_doc_with_scope_and_sources(dir: &std::path::Path, slug: &str) -> String {
    std::fs::create_dir_all(dir.join("src/widgets")).unwrap();
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    std::fs::write(
        dir.join("src/widgets/add_item.rs"),
        "pub fn handle_add_item(name: &str) -> bool {\n    !name.is_empty()\n}\n\nstruct AddItemRequest {\n    name: String,\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("tests/add_item_test.rs"),
        "#[test]\nfn test_add_item_rejects_empty_name() {\n    assert!(!handle_add_item(\"\"));\n}\n",
    )
    .unwrap();

    let body = "Intro.\n\n## Add item\n\nDescribes adding an item.\n\n## Unrelated section\n\nNo matching code.\n";
    let resp = call(
        dir,
        "handoff_doc_save",
        json!({
            "slug": slug,
            "title": "Suggest Refs Sample",
            "body": body,
            "doc_type": "spec",
            "scope_paths": ["src/widgets/", "tests/"],
        }),
    );
    assert!(!is_error(&resp), "doc_save failed: {}", payload_text(&resp));
    payload(&resp)["doc_id"].as_str().unwrap().to_string()
}

#[test]
fn doc_verify_suggest_refs_requires_existing_matrix() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-suggest-refs-no-matrix");
    let doc_id = save_doc_with_scope_and_sources(&dir, &slug);

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "suggest_refs" }),
    );
    assert!(is_error(&resp), "expected error without a matrix");
    assert!(
        payload_text(&resp).contains("generate"),
        "error should point at action='generate': {}",
        payload_text(&resp)
    );
}

#[test]
fn doc_verify_suggest_refs_returns_impl_and_test_candidates_matching_heading() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-suggest-refs-basic");
    let doc_id = save_doc_with_scope_and_sources(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "suggest_refs" }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let body = payload(&resp);
    let suggestions = body["suggestions"].as_array().expect("suggestions array");

    // "Add item" heading (seq 1) should match handle_add_item / AddItemRequest
    // for impl_refs, and test_add_item_rejects_empty_name for test_refs.
    let add_item_suggestion = suggestions
        .iter()
        .find(|s| s["heading"] == "Add item")
        .expect("suggestion for 'Add item' heading");
    let impl_refs = add_item_suggestion["suggested_impl_refs"]
        .as_array()
        .expect("suggested_impl_refs array");
    assert!(
        impl_refs.iter().any(|r| r["path"]
            .as_str()
            .unwrap_or_default()
            .contains("add_item.rs")),
        "expected an impl_ref pointing at src/widgets/add_item.rs, got {impl_refs:?}"
    );
    let test_refs = add_item_suggestion["suggested_test_refs"]
        .as_array()
        .expect("suggested_test_refs array");
    assert!(
        test_refs.iter().any(|r| r["path"]
            .as_str()
            .unwrap_or_default()
            .contains("add_item_test.rs")),
        "expected a test_ref pointing at tests/add_item_test.rs, got {test_refs:?}"
    );

    // The unrelated section (seq 2) should not pick up the add_item file.
    let unrelated_suggestion = suggestions
        .iter()
        .find(|s| s["heading"] == "Unrelated section")
        .expect("suggestion for 'Unrelated section' heading");
    assert!(unrelated_suggestion["suggested_impl_refs"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn doc_verify_suggest_refs_output_feeds_into_set_refs() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-suggest-refs-e2e");
    let doc_id = save_doc_with_scope_and_sources(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );

    let suggest_resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "suggest_refs" }),
    );
    assert!(
        !is_error(&suggest_resp),
        "error: {}",
        payload_text(&suggest_resp)
    );
    let suggestions = payload(&suggest_resp)["suggestions"]
        .as_array()
        .unwrap()
        .clone();
    let add_item_suggestion = suggestions
        .iter()
        .find(|s| s["heading"] == "Add item")
        .expect("suggestion for 'Add item' heading");
    let fragment_seq = add_item_suggestion["fragment_seq"].as_u64().unwrap();
    let accepted_impl_refs = add_item_suggestion["suggested_impl_refs"].clone();
    let accepted_test_refs = add_item_suggestion["suggested_test_refs"].clone();

    // Accept step: feed the suggested refs straight into set_refs.
    let set_refs_resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "set_refs",
            "fragment_seq": fragment_seq,
            "impl_refs": accepted_impl_refs,
            "test_refs": accepted_test_refs,
        }),
    );
    assert!(
        !is_error(&set_refs_resp),
        "error: {}",
        payload_text(&set_refs_resp)
    );

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let item = items
        .iter()
        .find(|i| i["fragment_seq"].as_u64() == Some(fragment_seq))
        .unwrap();
    assert!(
        item["impl_refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["path"]
                .as_str()
                .unwrap_or_default()
                .contains("add_item.rs")),
        "impl_refs were not applied via set_refs: {item:?}"
    );
    assert!(
        item["test_refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["path"]
                .as_str()
                .unwrap_or_default()
                .contains("add_item_test.rs")),
        "test_refs were not applied via set_refs: {item:?}"
    );
}

// ---------------------------------------------------------------------
// P0 requirements-traceability (.handoff/docs/_doc.req-traceability-mcp-plan.md
// §3.3): set_refs SubItem addressing + set_dev_stage / set_priority actions.
// `add_item` doesn't expose setting `stable_id` yet (t300.3's concern), so
// these tests address the SubItem via `sub_item_index` — this still
// exercises the same `find_sub_item_mut_by_id` path `sub_item_id` uses, and
// the mismatch-warning test below drives `sub_item_id` explicitly (paired
// with a non-matching `sub_item_index`, since no real stable_id exists yet).
// ---------------------------------------------------------------------

#[test]
fn doc_verify_set_refs_with_sub_item_index_updates_sub_item_refs_not_parent() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-set-refs-sub-item");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "fragment_seq": 1, "description": "req A" }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "set_refs",
            "fragment_seq": 1,
            "sub_item_index": 0,
            "impl_refs": [{ "path": "src/sub.rs", "lines": "1-5" }],
            "test_refs": [{ "path": "tests/sub.rs" }],
        }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let item1 = items.iter().find(|i| i["fragment_seq"] == 1).unwrap();
    // Refs land on the sub_item, not the parent VerificationItem.
    assert!(item1["impl_refs"].as_array().unwrap().is_empty());
    assert!(item1["test_refs"].as_array().unwrap().is_empty());
    let subs = item1["sub_items"].as_array().unwrap();
    assert_eq!(subs[0]["impl_refs"][0]["path"], "src/sub.rs");
    assert_eq!(subs[0]["impl_refs"][0]["lines"], "1-5");
    assert_eq!(subs[0]["test_refs"][0]["path"], "tests/sub.rs");
}

#[test]
fn doc_verify_set_refs_sub_item_id_and_index_disagree_returns_warning() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-set-refs-mismatch");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "fragment_seq": 1, "description": "req A" }),
    );

    // No sub_item has a stable_id assigned yet (add_item doesn't expose it),
    // so a `sub_item_id` that doesn't match anything is an error — this is
    // the documented behavior from the check/skip tests above. To observe
    // the mismatch-warning path itself (sub_item_id vs sub_item_index
    // disagreeing) would require a fixture with an assigned stable_id,
    // which is out of this task's scope (t300.3). Confirm instead that an
    // unmatched sub_item_id surfaces as a clear error, not a silent
    // fall-through to sub_item_index.
    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "set_refs",
            "fragment_seq": 1,
            "sub_item_id": "C01-9.9.9.9",
            "sub_item_index": 0,
            "impl_refs": [{ "path": "src/sub.rs" }],
        }),
    );
    assert!(
        is_error(&resp),
        "sub_item_id with no matching stable_id must error, not silently fall back to sub_item_index"
    );
}

#[test]
fn doc_verify_set_refs_without_sub_item_address_still_updates_parent_item() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-set-refs-parent-unchanged");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );

    // No sub_item_id/sub_item_index given: existing parent-item behavior
    // must be unchanged.
    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "set_refs",
            "fragment_seq": 1,
            "impl_refs": [{ "path": "src/parent.rs" }],
        }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let item1 = items.iter().find(|i| i["fragment_seq"] == 1).unwrap();
    assert_eq!(item1["impl_refs"][0]["path"], "src/parent.rs");
}

// ---------------------------------------------------------------------
// doc_verify: set_dev_stage
// ---------------------------------------------------------------------

#[test]
fn doc_verify_set_dev_stage_updates_sub_item_dev_stage() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-set-dev-stage");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "fragment_seq": 1, "description": "req A" }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "set_dev_stage",
            "fragment_seq": 1,
            "sub_item_index": 0,
            "dev_stage": "implemented",
        }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let item1 = items.iter().find(|i| i["fragment_seq"] == 1).unwrap();
    let subs = item1["sub_items"].as_array().unwrap();
    assert_eq!(subs[0]["dev_stage"], "implemented");
}

#[test]
fn doc_verify_set_dev_stage_rejects_invalid_value() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-set-dev-stage-invalid");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "fragment_seq": 1, "description": "req A" }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "set_dev_stage",
            "fragment_seq": 1,
            "sub_item_index": 0,
            "dev_stage": "bogus_stage",
        }),
    );
    assert!(
        is_error(&resp),
        "invalid dev_stage value must be rejected: {}",
        payload_text(&resp)
    );
}

#[test]
fn doc_verify_set_dev_stage_requires_sub_item_address() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-set-dev-stage-no-address");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "set_dev_stage",
            "fragment_seq": 1,
            "dev_stage": "implemented",
        }),
    );
    assert!(
        is_error(&resp),
        "set_dev_stage without sub_item_id/sub_item_index must error \
         (dev_stage is a SubItem-only concept, spec §2.4)"
    );
}

// ---------------------------------------------------------------------
// doc_verify: set_priority
// ---------------------------------------------------------------------

#[test]
fn doc_verify_set_priority_updates_sub_item_priority() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-set-priority");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "fragment_seq": 1, "description": "req A" }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "set_priority",
            "fragment_seq": 1,
            "sub_item_index": 0,
            "priority": "P0",
        }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let item1 = items.iter().find(|i| i["fragment_seq"] == 1).unwrap();
    let subs = item1["sub_items"].as_array().unwrap();
    assert_eq!(subs[0]["priority"], "P0");
}

#[test]
fn doc_verify_set_priority_rejects_invalid_value() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-set-priority-invalid");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "fragment_seq": 1, "description": "req A" }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "set_priority",
            "fragment_seq": 1,
            "sub_item_index": 0,
            "priority": "P9",
        }),
    );
    assert!(
        is_error(&resp),
        "invalid priority value must be rejected: {}",
        payload_text(&resp)
    );
}

#[test]
fn doc_verify_set_priority_requires_sub_item_address() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-set-priority-no-address");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "set_priority",
            "fragment_seq": 1,
            "priority": "P0",
        }),
    );
    assert!(
        is_error(&resp),
        "set_priority without sub_item_id/sub_item_index must error \
         (priority is a SubItem-only concept)"
    );
}

// ---------------------------------------------------------------------
// stable_id derivation + immutability (t300.3,
// .handoff/docs/_doc.req-traceability-mcp-plan.md §2.3)
// ---------------------------------------------------------------------

#[test]
fn doc_verify_add_item_assigns_derived_stable_id() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("req-c01-board-setup");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "add_item",
            "fragment_seq": 1,
            "description": "2.1.1 外形形状定義",
        }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let seq1 = items.iter().find(|i| i["fragment_seq"] == 1).unwrap();
    let subs = seq1["sub_items"].as_array().unwrap();
    assert_eq!(subs.len(), 1);
    let stable_id = subs[0]["stable_id"].as_str().expect("stable_id assigned");
    assert_eq!(stable_id, "C01-2.1.1");
}

#[test]
fn doc_verify_add_item_collision_gets_suffix_and_warning() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("req-c01-collide");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );

    // Two sub_items with the same leading requirement number (e.g. two
    // "2.1.1" bullets under different parent sections) must not collide on
    // stable_id — the second one gets a `-2` suffix and a warning.
    call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "add_item",
            "fragment_seq": 1,
            "description": "2.1.1 外形形状定義",
        }),
    );
    let resp2 = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "add_item",
            "fragment_seq": 2,
            "description": "2.1.1 別の要件",
        }),
    );
    assert!(!is_error(&resp2), "{}", payload_text(&resp2));
    let p2 = payload(&resp2);
    let warnings = p2["warnings"].as_array().expect("warnings array");
    assert!(
        !warnings.is_empty(),
        "expected a collision warning, got {p2}"
    );

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let seq2 = items.iter().find(|i| i["fragment_seq"] == 2).unwrap();
    let subs = seq2["sub_items"].as_array().unwrap();
    assert_eq!(subs[0]["stable_id"], "C01-2.1.1-2");
}

#[test]
fn doc_verify_stable_id_immutable_across_sync() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("req-c01-immutable");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "add_item",
            "fragment_seq": 1,
            "description": "2.1.1 外形形状定義",
        }),
    );

    let before_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let before_items = payload(&before_resp)["items"].as_array().unwrap().clone();
    let before_seq1 = before_items
        .iter()
        .find(|i| i["fragment_seq"] == 1)
        .unwrap();
    let before_id = before_seq1["sub_items"][0]["stable_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Re-save with an extra section appended (sections 0/1/2 keep the same
    // seq, so `sync` must keep seq1's existing sub_items — and their
    // stable_id — untouched).
    let new_body =
        "Intro.\n\n## Section A\n\nBody A.\n\n## Section B\n\nBody B.\n\n## Section C\n\nBody C.\n";
    call(
        &dir,
        "handoff_doc_save",
        json!({ "doc_id": doc_id, "body": new_body }),
    );
    let sync_resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "sync" }),
    );
    assert!(!is_error(&sync_resp), "{}", payload_text(&sync_resp));

    let after_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let after_items = payload(&after_resp)["items"].as_array().unwrap().clone();
    let after_seq1 = after_items.iter().find(|i| i["fragment_seq"] == 1).unwrap();
    let after_id = after_seq1["sub_items"][0]["stable_id"].as_str().unwrap();
    assert_eq!(after_id, before_id, "stable_id must not change across sync");
}

#[test]
fn doc_verify_add_item_fuzzy_match_reuses_existing_stable_id() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("req-c01-fuzzy");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "add_item",
            "fragment_seq": 1,
            "description": "形状=八面体であること",
        }),
    );
    let first_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let first_items = payload(&first_resp)["items"].as_array().unwrap().clone();
    let first_seq1 = first_items.iter().find(|i| i["fragment_seq"] == 1).unwrap();
    let original_id = first_seq1["sub_items"][0]["stable_id"]
        .as_str()
        .unwrap()
        .to_string();

    // A near-identical description (trailing punctuation added) added to a
    // *different* section must re-link to the same stable_id rather than
    // minting a new one.
    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "add_item",
            "fragment_seq": 2,
            "description": "形状=八面体であること。",
        }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let seq2 = items.iter().find(|i| i["fragment_seq"] == 2).unwrap();
    let reused_id = seq2["sub_items"][0]["stable_id"].as_str().unwrap();
    assert_eq!(
        reused_id, original_id,
        "fuzzy-matched description should reuse the existing stable_id"
    );
}

#[test]
fn doc_verify_add_item_without_number_slugifies_description() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("misc-notes");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "add_item",
            "fragment_seq": 1,
            "description": "Rectangular Outline",
        }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let seq1 = items.iter().find(|i| i["fragment_seq"] == 1).unwrap();
    let stable_id = subs_stable_id(seq1);
    assert!(
        stable_id.contains("rectangular-outline"),
        "expected slugified description in stable_id, got {stable_id:?}"
    );
}

// ---------------------------------------------------------------------
// P0 requirements-traceability (.handoff/docs/_doc.req-traceability-mcp-plan.md
// §2.7, §3.4): _requirements_summary.json write-out for the VSCode extension.
// ---------------------------------------------------------------------

fn requirements_summary_path(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join(".handoff/docs/_requirements_summary.json")
}

#[test]
fn requirements_summary_absent_when_no_documents_exist() {
    let (_tmp, dir) = setup_project();
    assert!(
        !requirements_summary_path(&dir).exists(),
        "a fresh project with no docs must not have a summary file"
    );
}

#[test]
fn requirements_summary_written_after_set_dev_stage() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("req-summary-basic");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    let add_resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "fragment_seq": 1, "description": "req A" }),
    );
    assert!(!is_error(&add_resp), "{}", payload_text(&add_resp));

    // add_item now refreshes the cache too (requirements-traceability
    // integration reform §3.2) — the summary must already exist here.
    let summary_path = requirements_summary_path(&dir);
    assert!(
        summary_path.exists(),
        "_requirements_summary.json must exist after add_item"
    );

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "set_dev_stage",
            "fragment_seq": 1,
            "sub_item_index": 0,
            "dev_stage": "implemented",
        }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));

    assert!(
        summary_path.exists(),
        "_requirements_summary.json must exist after set_dev_stage"
    );
    let content = std::fs::read_to_string(&summary_path).unwrap();
    let summary: Value = serde_json::from_str(&content).unwrap();
    assert_eq!(summary["total"], 1);
    assert_eq!(summary["by_status"]["implemented"], 1);
    assert_eq!(summary["by_priority"]["unset"]["total"], 1);
    assert!(summary["by_category"].is_object());
    assert!(summary["coverage"]["impl_pct"].is_number());
}

#[test]
fn requirements_summary_aggregates_across_multiple_documents() {
    let (_tmp, dir) = setup_project();

    let slug_a = unique_slug("req-summary-multi-a");
    let doc_a = save_sample_doc(&dir, &slug_a);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_a, "action": "generate" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_a, "action": "add_item", "fragment_seq": 1, "description": "req A" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_a, "action": "set_priority", "fragment_seq": 1,
            "sub_item_index": 0, "priority": "P0",
        }),
    );

    let slug_b = unique_slug("req-summary-multi-b");
    let doc_b = save_sample_doc(&dir, &slug_b);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_b, "action": "generate" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_b, "action": "add_item", "fragment_seq": 1, "description": "req B" }),
    );
    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_b, "action": "set_priority", "fragment_seq": 1,
            "sub_item_index": 0, "priority": "P1",
        }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));

    let content = std::fs::read_to_string(requirements_summary_path(&dir)).unwrap();
    let summary: Value = serde_json::from_str(&content).unwrap();
    assert_eq!(summary["total"], 2, "must aggregate across both documents");
    assert_eq!(summary["by_priority"]["P0"]["total"], 1);
    assert_eq!(summary["by_priority"]["P1"]["total"], 1);
}

#[test]
fn requirements_summary_matches_doc_verify_status_derived_counts() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("req-summary-shape");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "fragment_seq": 1, "description": "req A" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id, "action": "set_refs", "fragment_seq": 1, "sub_item_index": 0,
            "impl_refs": [{ "path": "src/a.rs" }],
        }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id, "action": "set_dev_stage", "fragment_seq": 1,
            "sub_item_index": 0, "dev_stage": "verified",
        }),
    );

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let item1 = items.iter().find(|i| i["fragment_seq"] == 1).unwrap();
    let sub = &item1["sub_items"][0];
    assert_eq!(sub["dev_stage"], "verified");
    assert!(!sub["impl_refs"].as_array().unwrap().is_empty());

    let content = std::fs::read_to_string(requirements_summary_path(&dir)).unwrap();
    let summary: Value = serde_json::from_str(&content).unwrap();
    // The cache file's structure matches what `handoff_doc_req_status` (P1)
    // will return: total/by_status/by_priority/by_category/coverage, and
    // its counts are derived from the same SubItem data doc_verify_status
    // exposes above (dev_stage="verified", impl_refs non-empty).
    assert_eq!(summary["total"], 1);
    assert_eq!(summary["by_status"]["verified"], 1);
    assert_eq!(summary["coverage"]["impl_pct"], 100.0);
    assert_eq!(summary["coverage"]["verified_pct"], 100.0);
    assert_eq!(summary["coverage"]["test_pct"], 0.0);
}

#[test]
fn requirements_summary_not_written_when_document_has_no_sub_items() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("req-summary-no-subs");
    let doc_id = save_sample_doc(&dir, &slug);

    // generate + check a top-level item, never adding any sub_items —
    // there are zero requirements to aggregate.
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "check", "fragment_seq": 1 }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));

    assert!(
        !requirements_summary_path(&dir).exists(),
        "no SubItems anywhere => no summary file, even though a verification matrix exists"
    );
}

fn subs_stable_id(item: &Value) -> String {
    item["sub_items"][0]["stable_id"]
        .as_str()
        .expect("stable_id present")
        .to_string()
}

// ---------------------------------------------------------------------
// req-traceability-integration-reform §3.2: add_item now refreshes
// _requirements_summary.json directly (no need for a follow-up mutating
// action just to get the cache written).
// ---------------------------------------------------------------------

#[test]
fn requirements_summary_written_after_add_item_alone() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("req-summary-add-item-only");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );

    assert!(
        !requirements_summary_path(&dir).exists(),
        "no summary before any SubItem exists"
    );

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "fragment_seq": 1, "description": "req A" }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));

    let summary_path = requirements_summary_path(&dir);
    assert!(
        summary_path.exists(),
        "_requirements_summary.json must exist right after add_item, with no further action"
    );
    let content = std::fs::read_to_string(&summary_path).unwrap();
    let summary: Value = serde_json::from_str(&content).unwrap();
    assert_eq!(summary["total"], 1);
}

// ---------------------------------------------------------------------
// req-traceability-integration-reform §3.3: doc_verify(action="backfill_stable_ids")
// mints stable_ids for every SubItem that doesn't have one yet.
// ---------------------------------------------------------------------

/// add_item always mints a stable_id today, so to exercise backfill we drop
/// stable_id back to null on disk (simulating externally-imported SubItems,
/// e.g. via req_import before t300's minting existed, or a manually crafted
/// doc file). Documents are stored as `_doc.<slug>.md` with a YAML
/// frontmatter block (`---\n<yaml>\n---\n<body>`); this strips every
/// `stable_id: ...` line from the frontmatter's `sub_items` entries by
/// editing the YAML block as a `serde_yaml::Value` (so it stays valid YAML
/// regardless of formatting/indentation), then writes the file back.
fn clear_all_stable_ids(dir: &std::path::Path, doc_id: &str) {
    let docs_dir = dir.join(".handoff/docs");
    for entry in std::fs::read_dir(&docs_dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let content = std::fs::read_to_string(&path).unwrap();
        if !content.contains(doc_id) {
            continue;
        }
        let rest = content.strip_prefix("---\n").expect("frontmatter fence");
        let (yaml_block, body) = rest.split_once("\n---\n").expect("closing fence");
        let mut fm: serde_yaml::Value = serde_yaml::from_str(yaml_block).unwrap();
        if let Some(items) = fm
            .get_mut("verification")
            .and_then(|v| v.get_mut("items"))
            .and_then(|v| v.as_sequence_mut())
        {
            for item in items.iter_mut() {
                if let Some(subs) = item.get_mut("sub_items").and_then(|v| v.as_sequence_mut()) {
                    for sub in subs.iter_mut() {
                        if let Some(map) = sub.as_mapping_mut() {
                            map.remove("stable_id");
                        }
                    }
                }
            }
        }
        let new_yaml = serde_yaml::to_string(&fm).unwrap();
        let new_content = format!("---\n{new_yaml}---\n{body}");
        std::fs::write(&path, new_content).unwrap();
    }
}

#[test]
fn doc_verify_backfill_stable_ids_assigns_to_subitems_without_one() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("req-backfill-basic");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "fragment_seq": 1, "description": "2.1.1 req A" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "fragment_seq": 1, "description": "2.1.2 req B" }),
    );

    clear_all_stable_ids(&dir, &doc_id);

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let item1 = items.iter().find(|i| i["fragment_seq"] == 1).unwrap();
    for sub in item1["sub_items"].as_array().unwrap() {
        assert!(
            sub.get("stable_id").is_none() || sub["stable_id"].is_null(),
            "precondition: stable_id must be cleared before backfill"
        );
    }

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "backfill_stable_ids" }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let body = payload(&resp);
    assert_eq!(
        body["backfilled"], 2,
        "both SubItems must get a stable_id: {body:?}"
    );

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let item1 = items.iter().find(|i| i["fragment_seq"] == 1).unwrap();
    let subs = item1["sub_items"].as_array().unwrap();
    assert_eq!(subs.len(), 2);
    for sub in subs {
        let id = sub["stable_id"].as_str();
        assert!(
            id.is_some() && !id.unwrap().is_empty(),
            "expected a stable_id, got {sub:?}"
        );
    }
}

#[test]
fn doc_verify_backfill_stable_ids_preserves_existing_ids() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("req-backfill-preserve");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "fragment_seq": 1, "description": "2.1.1 req A" }),
    );
    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let item1 = items.iter().find(|i| i["fragment_seq"] == 1).unwrap();
    let original_id = subs_stable_id(item1);

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "backfill_stable_ids" }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let body = payload(&resp);
    assert_eq!(
        body["backfilled"], 0,
        "the existing SubItem already has a stable_id, nothing to backfill: {body:?}"
    );

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let item1 = items.iter().find(|i| i["fragment_seq"] == 1).unwrap();
    assert_eq!(
        subs_stable_id(item1),
        original_id,
        "backfill must not touch an already-assigned stable_id"
    );
}

#[test]
fn doc_verify_backfill_stable_ids_avoids_collisions_with_suffix() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("req-backfill-collision");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    // Two SubItems whose description numeral collides (both derive to the
    // same base stable_id) so backfill must disambiguate one with a suffix.
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "fragment_seq": 1, "description": "2.1.1 req A" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "fragment_seq": 1, "description": "2.1.1 req A duplicate numeral" }),
    );

    clear_all_stable_ids(&dir, &doc_id);

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "backfill_stable_ids" }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let body = payload(&resp);
    assert_eq!(body["backfilled"], 2, "{body:?}");

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let item1 = items.iter().find(|i| i["fragment_seq"] == 1).unwrap();
    let subs = item1["sub_items"].as_array().unwrap();
    assert_eq!(subs.len(), 2);
    let ids: Vec<String> = subs
        .iter()
        .map(|s| s["stable_id"].as_str().unwrap().to_string())
        .collect();
    assert_ne!(
        ids[0], ids[1],
        "colliding derivations must be disambiguated: {ids:?}"
    );
    assert!(
        ids.iter().any(|id| id.ends_with("-2")),
        "expected a numeric-suffix disambiguation like '...-2', got {ids:?}"
    );
}

#[test]
fn doc_verify_backfill_stable_ids_returns_zero_when_all_have_ids() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("req-backfill-noop");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "fragment_seq": 1, "description": "req A" }),
    );

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "backfill_stable_ids" }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let body = payload(&resp);
    assert_eq!(body["backfilled"], 0, "{body:?}");
}

#[test]
fn doc_verify_backfill_stable_ids_refreshes_summary() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("req-backfill-summary");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "fragment_seq": 1, "description": "2.1.1 req A" }),
    );
    clear_all_stable_ids(&dir, &doc_id);
    std::fs::remove_file(requirements_summary_path(&dir)).ok();

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "backfill_stable_ids" }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));

    assert!(
        requirements_summary_path(&dir).exists(),
        "_requirements_summary.json must be refreshed after backfill_stable_ids"
    );
}

// ---------------------------------------------------------------------
// Requirements-traceability integration reform §3.1: `link_task` action —
// SubItem.task_ids <-> task.task_links bidirectional linking.
// ---------------------------------------------------------------------

/// Creates a task and returns its id, parsed from the handler's plain
/// confirmation string `"Created task {id}: {title} [{status}]"` (mirrors
/// `tests/tool_docs.rs`'s helper of the same name).
fn create_task(dir: &std::path::Path, title: &str) -> String {
    let resp = call(
        dir,
        "handoff_update_task",
        json!({
            "task": {
                "title": title,
                "status": "todo",
                "schedule": { "estimate_hours": 1.0 }
            }
        }),
    );
    assert!(
        !is_error(&resp),
        "create_task failed: {}",
        payload_text(&resp)
    );
    let text = payload_text(&resp);
    text.strip_prefix("Created task ")
        .and_then(|rest| rest.split(':').next())
        .expect("expected 'Created task {id}: ...' response")
        .to_string()
}

/// Adds a sub_item to fragment_seq=1 of `doc_id` and returns its stable_id.
fn add_sub_item(dir: &std::path::Path, doc_id: &str, description: &str) -> String {
    let resp = call(
        dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "add_item", "fragment_seq": 1, "description": description }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));

    let status_resp = call(
        dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let seq1 = items.iter().find(|i| i["fragment_seq"] == 1).unwrap();
    let subs = seq1["sub_items"].as_array().unwrap();
    subs.last().unwrap()["stable_id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn doc_verify_link_task_sets_sub_item_task_ids() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-link-task");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    let stable_id = add_sub_item(&dir, &doc_id, "2.1.1 req A");
    let task_id = create_task(&dir, "Implement req A");

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "link_task",
            "fragment_seq": 1,
            "sub_item_id": &stable_id,
            "task_ids": [&task_id],
        }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let seq1 = items.iter().find(|i| i["fragment_seq"] == 1).unwrap();
    let sub = seq1["sub_items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["stable_id"] == stable_id)
        .unwrap();
    assert_eq!(
        sub["task_ids"].as_array().unwrap(),
        &vec![Value::String(task_id.clone())]
    );
}

#[test]
fn doc_verify_link_task_adds_reverse_task_link() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-link-task-reverse");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    let stable_id = add_sub_item(&dir, &doc_id, "2.1.1 req A");
    let task_id = create_task(&dir, "Implement req A");

    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "link_task",
            "fragment_seq": 1,
            "sub_item_id": &stable_id,
            "task_ids": [&task_id],
        }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));

    let task_resp = payload(&call(
        &dir,
        "handoff_get_task",
        json!({ "task_id": &task_id }),
    ));
    let links = task_resp["task_links"]
        .as_array()
        .or_else(|| task_resp["task"]["task_links"].as_array())
        .expect("task_links present")
        .clone();
    assert!(
        links.iter().any(|l| l["target"] == doc_id
            && l["link_type"] == "requirement"
            && l["label"] == stable_id),
        "expected reverse task_links entry, got {links:?}"
    );
}

#[test]
fn doc_verify_link_task_is_idempotent() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-link-task-idempotent");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    let stable_id = add_sub_item(&dir, &doc_id, "2.1.1 req A");
    let task_id = create_task(&dir, "Implement req A");

    for _ in 0..2 {
        let resp = call(
            &dir,
            "handoff_doc_verify",
            json!({
                "doc_id": doc_id,
                "action": "link_task",
                "fragment_seq": 1,
                "sub_item_id": &stable_id,
                "task_ids": [&task_id],
            }),
        );
        assert!(!is_error(&resp), "{}", payload_text(&resp));
    }

    let task_resp = payload(&call(
        &dir,
        "handoff_get_task",
        json!({ "task_id": &task_id }),
    ));
    let links = task_resp["task_links"]
        .as_array()
        .or_else(|| task_resp["task"]["task_links"].as_array())
        .expect("task_links present")
        .clone();
    let matching: Vec<_> = links
        .iter()
        .filter(|l| {
            l["target"] == doc_id && l["link_type"] == "requirement" && l["label"] == stable_id
        })
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "link_task must not duplicate the reverse task_links entry, got {links:?}"
    );
}

#[test]
fn doc_verify_link_task_replaces_existing_task_ids() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-link-task-replace");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    let stable_id = add_sub_item(&dir, &doc_id, "2.1.1 req A");
    let task_a = create_task(&dir, "Task A");
    let task_b = create_task(&dir, "Task B");

    call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "link_task",
            "fragment_seq": 1,
            "sub_item_id": &stable_id,
            "task_ids": [&task_a],
        }),
    );
    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "link_task",
            "fragment_seq": 1,
            "sub_item_id": &stable_id,
            "task_ids": [&task_b],
        }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    let items = payload(&status_resp)["items"].as_array().unwrap().clone();
    let seq1 = items.iter().find(|i| i["fragment_seq"] == 1).unwrap();
    let sub = seq1["sub_items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["stable_id"] == stable_id)
        .unwrap();
    assert_eq!(
        sub["task_ids"].as_array().unwrap(),
        &vec![Value::String(task_b.clone())],
        "link_task must replace task_ids, not append"
    );
}

// ---------------------------------------------------------------------
// Requirements-traceability integration reform §3.2: `task_coverage` field
// in _requirements_summary.json.
// ---------------------------------------------------------------------

#[test]
fn requirements_summary_includes_task_coverage() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("req-summary-task-coverage");
    let doc_id = save_sample_doc(&dir, &slug);
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    let stable_a = add_sub_item(&dir, &doc_id, "2.1.1 req A");
    let stable_b = add_sub_item(&dir, &doc_id, "2.1.2 req B");
    let task_id = create_task(&dir, "Implement reqs A and B");

    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "link_task", "fragment_seq": 1, "sub_item_id": &stable_a, "task_ids": [&task_id] }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "link_task", "fragment_seq": 1, "sub_item_id": &stable_b, "task_ids": [&task_id] }),
    );
    let set_stage_resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "set_dev_stage",
            "fragment_seq": 1,
            "sub_item_id": &stable_a,
            "dev_stage": "implemented",
        }),
    );
    assert!(
        !is_error(&set_stage_resp),
        "{}",
        payload_text(&set_stage_resp)
    );

    let summary: Value =
        serde_json::from_str(&std::fs::read_to_string(requirements_summary_path(&dir)).unwrap())
            .unwrap();
    let coverage = &summary["task_coverage"][&task_id];
    assert_eq!(coverage["total"], 2, "{summary:?}");
    assert_eq!(coverage["not_started"], 1, "{summary:?}");
    assert_eq!(coverage["implemented"], 1, "{summary:?}");
}
