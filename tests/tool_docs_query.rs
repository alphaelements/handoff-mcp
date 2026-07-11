//! Integration tests for the P1-6c document tools (doc_query / doc_analyze /
//! doc_import), exercised end-to-end through the JSON-RPC `process_line`
//! entry point — the same path the MCP server runs in production (mirrors
//! `tests/tool_docs.rs`).
//!
//! v5 rearchitecture (wiki/130-document-management.md §3.1): every
//! `handoff_doc_save` call creating a new document must supply a unique
//! `slug`.

use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};

fn send(input: &str) -> Option<Value> {
    let result = handoff_mcp::mcp::protocol::process_line(input)?;
    Some(serde_json::from_str(&result).expect("response should be valid JSON"))
}

/// Generates a fresh, process-wide-unique slug for test documents.
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
                "project_name": "docquerytest"
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
    payload_text(&resp)
        .strip_prefix("Created task ")
        .and_then(|rest| rest.split(':').next())
        .expect("response should start with 'Created task {id}:'")
        .to_string()
}

// ---------------------------------------------------------------------
// handoff_doc_query
// ---------------------------------------------------------------------

#[test]
fn doc_query_returns_relevant_fragment_full_when_short() {
    let (_tmp, dir) = setup_project();
    call(
        &dir,
        "handoff_doc_save",
        json!({
            "slug": unique_slug("rust-ownership-guide"),
            "title": "Rust Ownership Guide",
            "body": "# Rust Ownership\n\nBorrow checker rules explained briefly.\n",
        }),
    );
    call(
        &dir,
        "handoff_doc_save",
        json!({
            "slug": unique_slug("javascript-promises"),
            "title": "JavaScript Promises",
            "body": "# JS Promises\n\nAsync await patterns.\n",
        }),
    );

    let resp = call(
        &dir,
        "handoff_doc_query",
        json!({ "text": "explain rust ownership borrow checker" }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    let docs = p["documents"].as_array().unwrap();
    assert!(!docs.is_empty());
    assert_eq!(docs[0]["title"], "Rust Ownership Guide");
    assert_eq!(docs[0]["depth"], "full");
    assert!(docs[0]["body"].as_str().unwrap().contains("Borrow checker"));
    assert_eq!(p["injected_count"], docs.len());
}

#[test]
fn doc_query_stages_outline_for_large_fragments() {
    let (_tmp, dir) = setup_project();
    // Build a fragment body whose estimated token count exceeds the 300
    // token inline threshold.
    let long_body = "word ".repeat(500);
    let body = format!("# Big Section\n\n{long_body}\n");
    call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("huge-doc"), "title": "Huge Doc", "body": body }),
    );

    let resp = call(
        &dir,
        "handoff_doc_query",
        json!({ "text": "big section word" }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    let docs = p["documents"].as_array().unwrap();
    assert!(!docs.is_empty());
    let big = docs
        .iter()
        .find(|d| d["title"] == "Huge Doc")
        .expect("huge doc must be present");
    assert_eq!(big["depth"], "outline");
    assert!(
        big.get("body").is_none(),
        "outline depth must not include the full body"
    );
    assert!(big["outline"].is_array(), "outline must list headings");
}

#[test]
fn doc_query_session_diff_suppresses_repeat_injection() {
    let (_tmp, dir) = setup_project();
    // No heading -> a single seq-0 fragment, so injected_count tracks exactly
    // one fragment (a headed body would also produce an empty seq-0 preamble
    // fragment alongside the heading fragment, doubling the count here).
    call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("session-doc"), "title": "Session Doc", "body": "Unique content xyzzy.\n" }),
    );

    let first = payload(&call(
        &dir,
        "handoff_doc_query",
        json!({ "text": "unique content xyzzy", "session_id": "sess-A" }),
    ));
    assert_eq!(first["injected_count"], 1);

    // Same session, same query: the fragment was already injected at this
    // content_hash, so it must be suppressed on the second call.
    let second = payload(&call(
        &dir,
        "handoff_doc_query",
        json!({ "text": "unique content xyzzy", "session_id": "sess-A" }),
    ));
    assert_eq!(
        second["injected_count"], 0,
        "already-injected fragment must be suppressed within the same session"
    );

    // A different session must see it fresh.
    let other_session = payload(&call(
        &dir,
        "handoff_doc_query",
        json!({ "text": "unique content xyzzy", "session_id": "sess-B" }),
    ));
    assert_eq!(
        other_session["injected_count"], 1,
        "a different session must not be suppressed by session A's sidecar"
    );
}

#[test]
fn doc_query_mark_injected_false_does_not_suppress_next_call() {
    let (_tmp, dir) = setup_project();
    // No heading -> a single seq-0 fragment (see comment in the sibling test
    // above for why a headed body would double injected_count here).
    call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("no-mark-doc"), "title": "No Mark Doc", "body": "Quokka wombat content.\n" }),
    );

    let first = payload(&call(
        &dir,
        "handoff_doc_query",
        json!({ "text": "quokka wombat content", "session_id": "sess-C", "mark_injected": false }),
    ));
    assert_eq!(first["injected_count"], 1);

    let second = payload(&call(
        &dir,
        "handoff_doc_query",
        json!({ "text": "quokka wombat content", "session_id": "sess-C" }),
    ));
    assert_eq!(
        second["injected_count"], 1,
        "mark_injected=false on the first call must not suppress the second"
    );
}

#[test]
fn doc_query_boosts_task_linked_document() {
    let (_tmp, dir) = setup_project();
    let task_id = create_task(&dir, "Session loop task");

    call(
        &dir,
        "handoff_doc_save",
        json!({
            "slug": unique_slug("unrelated-widgets-doc"),
            "title": "Unrelated Doc About Widgets",
            "body": "# Widgets\n\nWidgets and gadgets and gizmos.\n",
        }),
    );
    call(
        &dir,
        "handoff_doc_save",
        json!({
            "slug": unique_slug("task-linked-widgets-doc"),
            "title": "Task Linked Doc About Widgets",
            "body": "# Widgets\n\nWidgets and gadgets and gizmos too.\n",
            "task_ids": [&task_id],
        }),
    );

    let resp = payload(&call(
        &dir,
        "handoff_doc_query",
        json!({ "text": "widgets gadgets gizmos", "task_id": &task_id }),
    ));
    let docs = resp["documents"].as_array().unwrap();
    assert!(!docs.is_empty());
    assert_eq!(
        docs[0]["title"], "Task Linked Doc About Widgets",
        "the task-linked document must outrank the otherwise-similar unlinked one"
    );
}

#[test]
fn doc_query_empty_corpus_returns_empty_result() {
    let (_tmp, dir) = setup_project();
    let resp = call(&dir, "handoff_doc_query", json!({ "text": "anything" }));
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    assert!(p["documents"].as_array().unwrap().is_empty());
    assert_eq!(p["injected_count"], 0);
}

#[test]
fn doc_query_suppress_doc_ids_excludes_from_result() {
    let (_tmp, dir) = setup_project();
    let saved = payload(&call(
        &dir,
        "handoff_doc_save",
        json!({
            "slug": unique_slug("suppressed-doc"),
            "title": "Suppressed Doc",
            "body": "# Suppressed\n\nMongoose badger content.\n",
        }),
    ));
    let doc_id = saved["doc_id"].as_str().unwrap().to_string();
    call(
        &dir,
        "handoff_doc_save",
        json!({
            "slug": unique_slug("other-doc"),
            "title": "Other Doc",
            "body": "# Other\n\nMongoose badger content also.\n",
        }),
    );

    let resp = payload(&call(
        &dir,
        "handoff_doc_query",
        json!({ "text": "mongoose badger content", "suppress_doc_ids": [&doc_id] }),
    ));
    let docs = resp["documents"].as_array().unwrap();
    assert!(
        docs.iter().all(|d| d["doc_id"] != doc_id),
        "suppressed doc_id must not appear in results: {docs:?}"
    );
    assert!(
        docs.iter().any(|d| d["title"] == "Other Doc"),
        "non-suppressed doc must still be returned: {docs:?}"
    );
}

#[test]
fn doc_query_suppress_doc_ids_absent_does_not_affect_existing_behavior() {
    let (_tmp, dir) = setup_project();
    call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("plain-doc"), "title": "Plain Doc", "body": "# Plain\n\nCapybara otter content.\n" }),
    );

    let resp = payload(&call(
        &dir,
        "handoff_doc_query",
        json!({ "text": "capybara otter content" }),
    ));
    let docs = resp["documents"].as_array().unwrap();
    assert!(
        docs.iter().any(|d| d["title"] == "Plain Doc"),
        "without suppress_doc_ids, existing behavior must be unaffected: {docs:?}"
    );
}

#[test]
fn doc_query_suppress_until_changed_persists_and_reinjects_on_change() {
    let (_tmp, dir) = setup_project();
    let saved = payload(&call(
        &dir,
        "handoff_doc_save",
        json!({
            "slug": unique_slug("suppress-until-changed-doc"),
            "title": "Suppress Until Changed Doc",
            "body": "# Heading\n\nAxolotl narwhal content.\n",
        }),
    ));
    let doc_id = saved["doc_id"].as_str().unwrap().to_string();

    // First call: explicitly suppress + record suppression for future calls.
    let first = payload(&call(
        &dir,
        "handoff_doc_query",
        json!({
            "text": "axolotl narwhal content",
            "session_id": "sess-suppress",
            "suppress_doc_ids": [&doc_id],
            "suppress_until_changed": true,
        }),
    ));
    let first_docs = first["documents"].as_array().unwrap();
    assert!(first_docs.iter().all(|d| d["doc_id"] != doc_id));

    // Second call: no suppress_doc_ids passed this time, but the sidecar
    // should still recall the suppression since content_hash is unchanged.
    let second = payload(&call(
        &dir,
        "handoff_doc_query",
        json!({ "text": "axolotl narwhal content", "session_id": "sess-suppress" }),
    ));
    let second_docs = second["documents"].as_array().unwrap();
    assert!(
        second_docs.iter().all(|d| d["doc_id"] != doc_id),
        "suppress_until_changed must persist suppression across calls until content changes: {second_docs:?}"
    );

    // Now change the document's content; the content_hash changes, so the
    // suppression must no longer apply.
    call(
        &dir,
        "handoff_doc_save",
        json!({
            "doc_id": &doc_id,
            "title": "Suppress Until Changed Doc",
            "body": "# Heading\n\nAxolotl narwhal content updated.\n",
        }),
    );

    let third = payload(&call(
        &dir,
        "handoff_doc_query",
        json!({ "text": "axolotl narwhal content updated", "session_id": "sess-suppress" }),
    ));
    let third_docs = third["documents"].as_array().unwrap();
    assert!(
        third_docs.iter().any(|d| d["doc_id"] == doc_id),
        "once content_hash changes, the doc must be re-injected: {third_docs:?}"
    );
}

// ---------------------------------------------------------------------
// handoff_doc_analyze
// ---------------------------------------------------------------------

#[test]
fn doc_analyze_scans_directory_and_detects_doc_type() {
    let (tmp, dir) = setup_project();
    let scan_dir = tmp.path().join("proj").join("specs");
    std::fs::create_dir_all(&scan_dir).unwrap();
    std::fs::write(
        scan_dir.join("auth.md"),
        "# 認証設計書\n\n設計の詳細をここに記述する。\n",
    )
    .unwrap();
    std::fs::write(
        scan_dir.join("notes.md"),
        "# Random Notes\n\nJust some notes.\n",
    )
    .unwrap();

    let resp = call(&dir, "handoff_doc_analyze", json!({ "path": "specs" }));
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    assert_eq!(p["files_scanned"], 2);
    let auto_resolved = p["auto_resolved"].as_array().unwrap();
    assert_eq!(auto_resolved.len(), 2);

    let auth = auto_resolved
        .iter()
        .find(|f| f["file"].as_str().unwrap().ends_with("auth.md"))
        .expect("auth.md must be scanned");
    assert_eq!(auth["doc_type"], "design");

    let notes = auto_resolved
        .iter()
        .find(|f| f["file"].as_str().unwrap().ends_with("notes.md"))
        .expect("notes.md must be scanned");
    assert_eq!(notes["doc_type"], "note");
}

#[test]
fn doc_analyze_detects_broken_link_in_needs_review() {
    let (tmp, dir) = setup_project();
    let scan_dir = tmp.path().join("proj").join("specs2");
    std::fs::create_dir_all(&scan_dir).unwrap();
    std::fs::write(
        scan_dir.join("a.md"),
        "# Doc A\n\nSee [missing target](./does-not-exist.md) for details.\n",
    )
    .unwrap();

    let resp = call(&dir, "handoff_doc_analyze", json!({ "path": "specs2" }));
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    let needs_review = p["needs_review"].as_array().unwrap();
    assert!(
        needs_review.iter().any(|r| r["issue"] == "broken_link"
            && r["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("does-not-exist.md")),
        "a link to a non-scanned file must be flagged broken_link: {needs_review:?}"
    );
}

#[test]
fn doc_analyze_single_file_does_not_write_anything() {
    let (tmp, dir) = setup_project();
    let file = tmp.path().join("proj").join("standalone.md");
    std::fs::write(&file, "# Standalone\n\nContent.\n").unwrap();

    let resp = call(
        &dir,
        "handoff_doc_analyze",
        json!({ "path": "standalone.md" }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    assert_eq!(p["files_scanned"], 1);

    let list = payload(&call(&dir, "handoff_doc_list", json!({})));
    assert!(
        list["documents"].as_array().unwrap().is_empty(),
        "doc_analyze must be read-only: no document should exist after it runs"
    );
}

#[test]
fn doc_analyze_missing_path_is_error() {
    let (_tmp, dir) = setup_project();
    let resp = call(
        &dir,
        "handoff_doc_analyze",
        json!({ "path": "does-not-exist" }),
    );
    assert!(is_error(&resp));
}

#[test]
fn doc_analyze_flatten_skips_proposed_tree() {
    let (tmp, dir) = setup_project();
    let scan_dir = tmp.path().join("proj").join("flat");
    std::fs::create_dir_all(&scan_dir).unwrap();
    std::fs::write(scan_dir.join("one.md"), "# One\n\nBody.\n").unwrap();

    let resp = payload(&call(
        &dir,
        "handoff_doc_analyze",
        json!({ "path": "flat", "flatten": true }),
    ));
    assert!(
        resp["proposed_tree"].as_object().unwrap().is_empty(),
        "flatten=true must skip tree inference"
    );
}

// ---------------------------------------------------------------------
// handoff_doc_import
// ---------------------------------------------------------------------

#[test]
fn doc_import_writes_documents_from_analyzed_payload() {
    let (_tmp, dir) = setup_project();
    let analyzed = json!({
        "auto_resolved": [
            {
                "file": "guide.md",
                "title": "Setup Guide",
                "doc_type": "guide",
                "tags": ["setup"],
                "body": "# Setup Guide\n\nHow to set things up.\n"
            }
        ],
        "needs_review": [],
        "proposed_tree": {}
    });

    let resp = call(&dir, "handoff_doc_import", json!({ "analyzed": analyzed }));
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    assert_eq!(p["imported_count"], 1);
    let docs = p["documents"].as_array().unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0]["title"], "Setup Guide");
    assert!(docs[0]["doc_id"].as_str().unwrap().starts_with("doc-"));

    let doc_id = docs[0]["doc_id"].as_str().unwrap().to_string();
    let meta = payload(&call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "meta" }),
    ));
    assert_eq!(meta["doc_type"], "guide");
    assert_eq!(meta["tags"], json!(["setup"]));

    let full = payload(&call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "full" }),
    ));
    assert_eq!(full["body"], "# Setup Guide\n\nHow to set things up.\n");
}

#[test]
fn doc_import_applies_overrides() {
    let (_tmp, dir) = setup_project();
    let analyzed = json!({
        "auto_resolved": [
            {
                "file": "auth.md",
                "title": "Auth",
                "doc_type": "note",
                "tags": [],
                "body": "# Auth\n\nAuth details.\n"
            }
        ],
        "needs_review": [],
        "proposed_tree": {}
    });
    let overrides = json!([
        { "file": "auth.md", "doc_type": "design", "tags": ["auth", "security"] }
    ]);

    let resp = payload(&call(
        &dir,
        "handoff_doc_import",
        json!({ "analyzed": analyzed, "overrides": overrides }),
    ));
    let doc_id = resp["documents"][0]["doc_id"].as_str().unwrap().to_string();

    let meta = payload(&call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "meta" }),
    ));
    assert_eq!(
        meta["doc_type"], "design",
        "override must replace the auto-detected doc_type"
    );
    assert_eq!(meta["tags"], json!(["auth", "security"]));
}

/// Regression test for a MAJOR bug found in review: `doc_import`'s
/// `unique_slug` helper used to loop forever (never returning, never
/// hitting its own `unreachable!()`) when an already-taken base slug was
/// exactly `MAX_SLUG_LEN` (60) chars, because every disambiguation
/// candidate `"{base}-{n}"` then exceeded the length limit and was rejected.
/// Reproduces the reviewer's exact live repro: save a doc with a 60-char
/// slug, then `doc_import` an override with that same 60-char slug. Must
/// complete promptly (this test itself has no timeout mechanism, so a
/// regression to the old infinite loop would hang the whole test binary —
/// that in itself is the point: this test existing and finishing is the
/// proof the bug is fixed).
#[test]
fn doc_import_disambiguates_slug_collision_at_max_length_without_hanging() {
    let (_tmp, dir) = setup_project();
    let taken_slug = "a".repeat(60);

    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": &taken_slug, "title": "Taken", "body": "# Taken\n\nBody.\n" }),
    );
    assert!(!is_error(&save_resp), "error: {}", payload_text(&save_resp));

    let analyzed = json!({
        "auto_resolved": [
            {
                "file": "collide.md",
                "title": "Collide",
                "doc_type": "note",
                "tags": [],
                "body": "# Collide\n\nBody.\n"
            }
        ],
        "needs_review": [],
        "proposed_tree": {}
    });
    let overrides = json!([{ "file": "collide.md", "slug": &taken_slug }]);

    let resp = call(
        &dir,
        "handoff_doc_import",
        json!({ "analyzed": analyzed, "overrides": overrides }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let result = payload(&resp);
    let new_slug = result["documents"][0]["slug"].as_str().unwrap();
    assert_ne!(
        new_slug, taken_slug,
        "slug must be disambiguated, not reused"
    );
    assert!(new_slug.len() <= 60);
}

#[test]
fn doc_import_links_task_ids_bidirectionally() {
    let (_tmp, dir) = setup_project();
    let task_id = create_task(&dir, "Import target task");
    let analyzed = json!({
        "auto_resolved": [
            { "file": "a.md", "title": "A", "doc_type": "note", "tags": [], "body": "# A\n\nBody.\n" }
        ],
        "needs_review": [],
        "proposed_tree": {}
    });

    let resp = payload(&call(
        &dir,
        "handoff_doc_import",
        json!({ "analyzed": analyzed, "task_ids": [&task_id] }),
    ));
    assert!(resp["warnings"].as_array().unwrap().is_empty());
    let doc_id = resp["documents"][0]["doc_id"].as_str().unwrap().to_string();

    let task_resp = payload(&call(
        &dir,
        "handoff_get_task",
        json!({ "task_id": &task_id }),
    ));
    let links = task_resp["task_links"]
        .as_array()
        .or_else(|| task_resp["task"]["task_links"].as_array())
        .expect("task_links present");
    assert!(links
        .iter()
        .any(|l| l["target"] == doc_id && l["link_type"] == "doc"));
}

#[test]
fn doc_import_requires_body_in_payload() {
    let (_tmp, dir) = setup_project();
    let analyzed = json!({
        "auto_resolved": [
            { "file": "no-body.md", "title": "No Body", "doc_type": "note", "tags": [] }
        ],
        "needs_review": [],
        "proposed_tree": {}
    });

    let resp = call(&dir, "handoff_doc_import", json!({ "analyzed": analyzed }));
    assert!(
        is_error(&resp),
        "import without a body for a file must be rejected, not silently skipped"
    );
}

#[test]
fn doc_import_rejects_empty_auto_resolved() {
    let (_tmp, dir) = setup_project();
    let analyzed = json!({ "auto_resolved": [], "needs_review": [], "proposed_tree": {} });
    let resp = call(&dir, "handoff_doc_import", json!({ "analyzed": analyzed }));
    assert!(is_error(&resp));
}

#[test]
fn doc_import_bumps_corpus_generation_so_doc_query_sees_new_docs() {
    let (_tmp, dir) = setup_project();
    // Prime the corpus cache with an empty/other-doc query first.
    call(&dir, "handoff_doc_query", json!({ "text": "anything" }));

    let analyzed = json!({
        "auto_resolved": [
            {
                "file": "fresh.md",
                "title": "Freshly Imported",
                "doc_type": "note",
                "tags": [],
                "body": "# Freshly Imported\n\nBrand new zylophone content.\n"
            }
        ],
        "needs_review": [],
        "proposed_tree": {}
    });
    call(&dir, "handoff_doc_import", json!({ "analyzed": analyzed }));

    let resp = payload(&call(
        &dir,
        "handoff_doc_query",
        json!({ "text": "brand new zylophone content" }),
    ));
    let docs = resp["documents"].as_array().unwrap();
    assert!(
        docs.iter().any(|d| d["title"] == "Freshly Imported"),
        "doc_query must see the freshly imported document (corpus cache invalidated): {docs:?}"
    );
}

// ---------------------------------------------------------------------
// End-to-end: analyze -> import -> query round trip
// ---------------------------------------------------------------------

#[test]
fn analyze_then_import_then_query_round_trip() {
    let (tmp, dir) = setup_project();
    let scan_dir = tmp.path().join("proj").join("importme");
    std::fs::create_dir_all(&scan_dir).unwrap();
    std::fs::write(
        scan_dir.join("architecture.md"),
        "# Architecture Guide\n\nExplains the ferret-shaped module boundaries.\n",
    )
    .unwrap();

    let analyzed = payload(&call(
        &dir,
        "handoff_doc_analyze",
        json!({ "path": "importme" }),
    ));

    // Attach body (doc_analyze's report doesn't carry it; the AI/test driver
    // re-reads the scanned file, exactly as the real flow would).
    let file_path = scan_dir.join("architecture.md");
    let body = std::fs::read_to_string(&file_path).unwrap();
    let mut analyzed_with_body = analyzed.clone();
    for entry in analyzed_with_body["auto_resolved"].as_array_mut().unwrap() {
        entry["body"] = json!(body);
    }

    let import_resp = payload(&call(
        &dir,
        "handoff_doc_import",
        json!({ "analyzed": analyzed_with_body }),
    ));
    assert_eq!(import_resp["imported_count"], 1);

    let query_resp = payload(&call(
        &dir,
        "handoff_doc_query",
        json!({ "text": "ferret-shaped module boundaries" }),
    ));
    let docs = query_resp["documents"].as_array().unwrap();
    assert!(
        docs.iter().any(|d| d["title"] == "Architecture Guide"),
        "the imported document must be queryable via doc_query: {docs:?}"
    );
}
