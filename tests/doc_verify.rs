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
