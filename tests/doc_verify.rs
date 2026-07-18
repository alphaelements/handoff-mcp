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
