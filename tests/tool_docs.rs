//! Integration tests for the P1-6a document tools (doc_save / doc_get /
//! doc_list), exercised end-to-end through the JSON-RPC `process_line` entry
//! point — the same path the MCP server runs in production (mirrors
//! `tests/tool_memory.rs`).
//!
//! Frontmatter migration (t123.1-t123.3, wiki/130-document-management.md
//! §3.1): documents are stored as a single slug-named `_doc.<slug>.md` file
//! (YAML frontmatter + body) rather than a JSON+MD pair or per-section
//! fragment files, so every `handoff_doc_save` call creating a new document
//! must supply a unique `slug`.

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
                "project_name": "doctest"
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

/// Creates a task and returns its id, parsed from the handler's plain
/// confirmation string `"Created task {id}: {title} [{status}]"`.
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
        .expect("response should start with 'Created task {id}:'")
        .to_string()
}

fn payload_text(resp: &Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

// ---------------------------------------------------------------------
// doc_save: new document creation
// ---------------------------------------------------------------------

#[test]
fn doc_save_creates_new_document_and_sections() {
    let (_tmp, dir) = setup_project();
    let body = "# Title\n\nIntro.\n\n## Section A\n\nBody A.\n\n## Section B\n\nBody B.\n";
    let slug = unique_slug("session-loop");
    let resp = call(
        &dir,
        "handoff_doc_save",
        json!({
            "slug": &slug,
            "title": "Session Loop",
            "body": body,
            "doc_type": "spec",
            "tags": ["session-loop"],
            "scope_paths": ["src/mcp/handlers/"],
        }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    assert!(p["doc_id"].as_str().unwrap().starts_with("doc-"));
    assert_eq!(p["slug"], slug);
    assert_eq!(p["title"], "Session Loop");
    assert_eq!(p["doc_type"], "spec");
    // seq0 (preamble) + Title(H1) + Section A + Section B == 4 sections.
    assert_eq!(p["section_count"], 4);
    assert!(!p["content_hash"].as_str().unwrap_or_default().is_empty());
    assert!(p["warnings"].as_array().unwrap().is_empty());

    // Frontmatter migration: exactly 1 file on disk for this document (no
    // JSON sidecar, no per-section files).
    let docs_dir = dir.join(".handoff/docs");
    assert!(docs_dir.join(format!("_doc.{slug}.md")).exists());
    assert!(!docs_dir.join(format!("_doc.{slug}.json")).exists());
}

#[test]
fn doc_save_new_document_without_slug_is_error() {
    let (_tmp, dir) = setup_project();
    let resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "title": "No Slug", "body": "# H\n\nbody\n" }),
    );
    assert!(
        is_error(&resp),
        "slug must be required for new documents (v5)"
    );
}

#[test]
fn doc_save_rejects_invalid_slug() {
    let (_tmp, dir) = setup_project();
    let resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": "Not Valid!", "title": "Bad Slug", "body": "# H\n\nbody\n" }),
    );
    assert!(is_error(&resp), "slug with invalid characters must error");
}

#[test]
fn doc_save_rejects_duplicate_slug() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("dup-slug");
    let resp1 = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": &slug, "title": "First", "body": "# H\n\nbody\n" }),
    );
    assert!(!is_error(&resp1), "error: {}", payload_text(&resp1));

    let resp2 = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": &slug, "title": "Second", "body": "# H2\n\nbody\n" }),
    );
    assert!(
        is_error(&resp2),
        "creating a second document with the same slug must error"
    );
}

/// The reported `content_hash` must actually reflect the reassembled body:
/// it must match `lexsim::content_hash` of that body, be identical across
/// two saves of the same body, and differ when the body's textual content
/// changes. (A stub/constant hash would pass a "non-empty" check but fail
/// these equality/inequality assertions.)
#[test]
fn doc_save_content_hash_reflects_body_and_changes_with_content() {
    let (_tmp, dir) = setup_project();
    let body_a = "# Title\n\nHello world.\n";
    let resp_a1 = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("hash-doc-a"), "title": "Hash Doc A", "body": body_a }),
    );
    let p_a1 = payload(&resp_a1);
    let expected_hash_a = lexsim::content_hash(body_a);
    assert_eq!(
        p_a1["content_hash"].as_str().unwrap(),
        expected_hash_a,
        "content_hash must equal lexsim::content_hash(reassembled body)"
    );

    // Re-saving the exact same body under a different doc must produce the
    // same hash (determinism).
    let resp_a2 = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("hash-doc-a2"), "title": "Hash Doc A2", "body": body_a }),
    );
    assert_eq!(
        payload(&resp_a2)["content_hash"].as_str().unwrap(),
        expected_hash_a,
        "identical body content must produce an identical content_hash"
    );

    // A body with different textual content must produce a different hash.
    let body_b = "# Title\n\nGoodbye moon.\n";
    let resp_b = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("hash-doc-b"), "title": "Hash Doc B", "body": body_b }),
    );
    let hash_b = payload(&resp_b)["content_hash"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(
        hash_b, expected_hash_a,
        "different body content must produce a different content_hash"
    );
}

#[test]
fn doc_save_requires_title_and_body() {
    let (_tmp, dir) = setup_project();
    let resp = call(&dir, "handoff_doc_save", json!({ "title": "No body" }));
    assert!(is_error(&resp));

    let resp2 = call(&dir, "handoff_doc_save", json!({ "body": "# X\n" }));
    assert!(is_error(&resp2));
}

// ---------------------------------------------------------------------
// doc_get: full / meta / fragment round trip
// ---------------------------------------------------------------------

#[test]
fn doc_save_then_get_full_matches_original_body() {
    let (_tmp, dir) = setup_project();
    let body = "# Title\n\nIntro.\n\n## Section A\n\nBody A.\n";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("roundtrip-doc"), "title": "Roundtrip Doc", "body": body }),
    );
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    let get_resp = call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "full" }),
    );
    assert!(!is_error(&get_resp), "error: {}", payload_text(&get_resp));
    let g = payload(&get_resp);
    assert_eq!(g["body"], body);
    assert_eq!(g["title"], "Roundtrip Doc");
    assert_eq!(g["id"], doc_id);
}

#[test]
fn doc_save_then_get_full_by_slug() {
    let (_tmp, dir) = setup_project();
    let body = "# Title\n\nIntro.\n";
    let slug = unique_slug("by-slug-doc");
    call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": &slug, "title": "By Slug Doc", "body": body }),
    );

    let get_resp = call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &slug, "format": "full" }),
    );
    assert!(!is_error(&get_resp), "error: {}", payload_text(&get_resp));
    let g = payload(&get_resp);
    assert_eq!(g["body"], body);
    assert_eq!(g["slug"], slug);
}

/// Frontmatter migration (t123.1): a body with BOM + CRLF + a *user-authored*
/// YAML frontmatter block must still round-trip byte-identically through
/// doc_save -> doc_get(full) for the BOM and the content *after* the
/// frontmatter — but the user's leading frontmatter block itself is now
/// absorbed into (and superseded by) handoff's own frontmatter (the `.md`
/// file's frontmatter is handoff-owned metadata, not a losslessly-stashed
/// passthrough of whatever the caller pasted in). This differs from the
/// pre-migration 2-file format, which preserved the caller's frontmatter
/// block byte-for-byte in `source.frontmatter`.
#[test]
fn doc_save_then_get_full_absorbs_user_frontmatter_preserves_bom_crlf_body() {
    let (_tmp, dir) = setup_project();
    let body = "\u{FEFF}---\r\ntitle: Foo\r\n---\r\n# Title\r\n\r\nBody.\r\n";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("bom-crlf-frontmatter"), "title": "Foo", "body": body }),
    );
    assert!(!is_error(&save_resp), "error: {}", payload_text(&save_resp));
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    let get_resp = call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "full" }),
    );
    assert!(!is_error(&get_resp), "error: {}", payload_text(&get_resp));
    let g = payload(&get_resp);
    assert_eq!(
        g["body"].as_str().unwrap(),
        "\u{FEFF}# Title\r\n\r\nBody.\r\n",
        "BOM must round-trip and the body after the user's (now-absorbed) \
         frontmatter must stay byte-identical, including CRLF"
    );
}

/// Companion case: a document whose *user-authored* body is entirely a
/// frontmatter block with nothing after the closing fence (no trailing
/// newline, no content at all) — once handoff's own frontmatter absorbs it,
/// the remaining body is empty. Must not error and must not invent content.
#[test]
fn doc_save_then_get_full_body_that_was_only_frontmatter_becomes_empty() {
    let (_tmp, dir) = setup_project();
    let body = "---\ntitle: Foo\n---";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("no-trailing-eol"), "title": "Foo", "body": body }),
    );
    assert!(!is_error(&save_resp), "error: {}", payload_text(&save_resp));
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    let get_resp = call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "full" }),
    );
    assert!(!is_error(&get_resp), "error: {}", payload_text(&get_resp));
    let g = payload(&get_resp);
    assert_eq!(
        g["body"].as_str().unwrap(),
        "",
        "a body that was entirely a (now-absorbed) frontmatter block leaves an empty body"
    );

    let list_resp = call(&dir, "handoff_doc_list", json!({ "include_body": true }));
    assert!(!is_error(&list_resp), "error: {}", payload_text(&list_resp));
    let l = payload(&list_resp);
    let doc_entry = l["documents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == doc_id)
        .expect("saved doc should appear in doc_list");
    assert_eq!(doc_entry["body"].as_str().unwrap(), "");
}

#[test]
fn doc_get_meta_returns_metadata_without_body() {
    let (_tmp, dir) = setup_project();
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("meta-doc"), "title": "Meta Doc", "body": "# H\n\nbody\n" }),
    );
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    let get_resp = call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "meta" }),
    );
    assert!(!is_error(&get_resp));
    let g = payload(&get_resp);
    assert_eq!(g["id"], doc_id);
    assert!(g.get("body").is_none(), "meta format must not include body");
}

#[test]
fn doc_get_section_returns_single_section() {
    let (_tmp, dir) = setup_project();
    let body = "# Title\n\nIntro.\n\n## Section A\n\nBody A.\n";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("section-doc"), "title": "Section Doc", "body": body }),
    );
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    let get_resp = call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "section", "seq": 1 }),
    );
    assert!(!is_error(&get_resp), "error: {}", payload_text(&get_resp));
    let g = payload(&get_resp);
    assert_eq!(g["seq"], 1);
    assert_eq!(g["heading"], "Title");
    assert!(g["body"].as_str().unwrap().contains("# Title"));
}

#[test]
fn doc_get_missing_doc_id_is_error() {
    let (_tmp, dir) = setup_project();
    let resp = call(&dir, "handoff_doc_get", json!({ "doc_id": "doc-nope" }));
    assert!(is_error(&resp));
}

/// Regression test for a MAJOR bug found in review: `doc_get(format=section)`
/// used to call `extract_section` unconditionally, which panics via a Rust
/// slice-bounds error when the on-disk body has drifted (e.g. truncated by an
/// out-of-band edit) so it's shorter than a section's recorded
/// byte_offset/byte_length. Because each request runs on its own thread, the
/// panic didn't crash the server, but the result channel send was skipped —
/// no JSON-RPC response was ever produced for that request. Must now return a
/// normal tool-error response instead of hanging/panicking silently.
#[test]
fn doc_get_section_errors_gracefully_when_body_truncated_shorter_than_section() {
    let (_tmp, dir) = setup_project();
    let body = "# Title\n\nIntro.\n\n## Section A\n\nBody A that is long enough.\n";
    let slug = unique_slug("truncated-section-doc");
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": &slug, "title": "Truncated Section Doc", "body": body }),
    );
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    // Directly truncate the document's body on disk, bypassing doc_save, so
    // section 1's recorded byte range no longer fits within the body.
    let body_path = dir.join(".handoff/docs").join(format!("_doc.{slug}.md"));
    std::fs::write(&body_path, "# Ti").unwrap();

    let resp = call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "section", "seq": 1 }),
    );
    assert!(
        is_error(&resp),
        "expected a graceful tool error for drifted/out-of-bounds section, got: {resp:?}"
    );
}

// ---------------------------------------------------------------------
// doc_save: update mode (same doc_id) + fragment count changes
// ---------------------------------------------------------------------

#[test]
fn doc_save_update_replaces_sections_and_preserves_created_at() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("update-doc");
    let body1 = "# Title\n\n## A\n\nBody A.\n";
    let save1 = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": &slug, "title": "Update Doc", "body": body1 }),
    );
    let p1 = payload(&save1);
    let doc_id = p1["doc_id"].as_str().unwrap().to_string();
    assert_eq!(p1["section_count"], 3); // seq0 + Title + A

    let meta1 = payload(&call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "meta" }),
    ));
    let created_at = meta1["created_at"].as_str().unwrap().to_string();

    // Update with fewer sections -> the recomputed sections manifest must
    // reflect only what's in the new body.
    let body2 = "# Title\n\nJust a preamble update.\n";
    let save2 = call(
        &dir,
        "handoff_doc_save",
        json!({ "doc_id": &doc_id, "title": "Update Doc", "body": body2 }),
    );
    assert!(!is_error(&save2), "error: {}", payload_text(&save2));
    let p2 = payload(&save2);
    assert_eq!(p2["doc_id"], doc_id);
    assert_eq!(p2["section_count"], 2); // seq0 + Title

    // Frontmatter migration: only the single .md file exists — nothing else
    // to clean up per section.
    let docs_dir = dir.join(".handoff/docs");
    assert!(docs_dir.join(format!("_doc.{slug}.md")).exists());
    assert!(!docs_dir.join(format!("_doc.{slug}.json")).exists());

    let meta2 = payload(&call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "meta" }),
    ));
    assert_eq!(
        meta2["created_at"], created_at,
        "created_at must be preserved on update"
    );

    let get_full = payload(&call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "full" }),
    ));
    assert_eq!(get_full["body"], body2);
}

// ---------------------------------------------------------------------
// doc_list: filters
// ---------------------------------------------------------------------

#[test]
fn doc_list_filters_by_doc_type_tags_and_task_id() {
    let (_tmp, dir) = setup_project();
    let task_id = create_task(&dir, "Linked task");

    call(
        &dir,
        "handoff_doc_save",
        json!({
            "slug": unique_slug("spec-doc"),
            "title": "Spec Doc",
            "body": "# Spec\n\nContent.\n",
            "doc_type": "spec",
            "tags": ["alpha", "beta"],
            "task_ids": [&task_id],
        }),
    );
    call(
        &dir,
        "handoff_doc_save",
        json!({
            "slug": unique_slug("note-doc"),
            "title": "Note Doc",
            "body": "# Note\n\nContent.\n",
            "doc_type": "note",
            "tags": ["beta"],
        }),
    );

    let by_type = payload(&call(
        &dir,
        "handoff_doc_list",
        json!({ "doc_type": "spec" }),
    ));
    let docs = by_type["documents"].as_array().unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0]["title"], "Spec Doc");

    let by_tags = payload(&call(
        &dir,
        "handoff_doc_list",
        json!({ "tags": ["alpha", "beta"] }),
    ));
    let docs2 = by_tags["documents"].as_array().unwrap();
    assert_eq!(docs2.len(), 1);
    assert_eq!(docs2[0]["title"], "Spec Doc");

    let by_tags_and = payload(&call(
        &dir,
        "handoff_doc_list",
        json!({ "tags": ["alpha", "note-only"] }),
    ));
    assert!(
        by_tags_and["documents"].as_array().unwrap().is_empty(),
        "AND semantics: a doc missing one requested tag must not match"
    );

    let by_task = payload(&call(
        &dir,
        "handoff_doc_list",
        json!({ "task_id": &task_id }),
    ));
    let docs3 = by_task["documents"].as_array().unwrap();
    assert_eq!(docs3.len(), 1);
    assert_eq!(docs3[0]["title"], "Spec Doc");

    let all = payload(&call(&dir, "handoff_doc_list", json!({})));
    assert_eq!(all["documents"].as_array().unwrap().len(), 2);
}

#[test]
fn doc_list_include_body_attaches_reassembled_body() {
    let (_tmp, dir) = setup_project();
    let body = "# T\n\nHello world.\n";
    call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("body-doc"), "title": "Body Doc", "body": body }),
    );

    let without = payload(&call(&dir, "handoff_doc_list", json!({})));
    assert!(without["documents"][0].get("body").is_none());

    let with_body = payload(&call(
        &dir,
        "handoff_doc_list",
        json!({ "include_body": true }),
    ));
    assert_eq!(with_body["documents"][0]["body"], body);
}

#[test]
fn doc_list_query_ranks_by_bm25_relevance() {
    let (_tmp, dir) = setup_project();
    call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("rust-ownership-guide"), "title": "Rust Ownership Guide", "body": "# Rust Ownership\n\nBorrow checker rules explained.\n" }),
    );
    call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("javascript-promises"), "title": "JavaScript Promises", "body": "# JS Promises\n\nAsync await patterns.\n" }),
    );

    let resp = payload(&call(
        &dir,
        "handoff_doc_list",
        json!({ "query": "rust ownership borrow" }),
    ));
    let docs = resp["documents"].as_array().unwrap();
    assert!(!docs.is_empty());
    assert_eq!(docs[0]["title"], "Rust Ownership Guide");
}

// ---------------------------------------------------------------------
// task_ids linkage via sync_doc_task_links (bidirectional) + warnings
// ---------------------------------------------------------------------

#[test]
fn doc_save_links_task_bidirectionally() {
    let (_tmp, dir) = setup_project();
    let task_id = create_task(&dir, "Task to link");

    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({
            "slug": unique_slug("linked-doc"),
            "title": "Linked Doc",
            "body": "# H\n\nbody\n",
            "task_ids": [&task_id],
        }),
    );
    assert!(!is_error(&save_resp), "error: {}", payload_text(&save_resp));
    let p = payload(&save_resp);
    assert!(p["warnings"].as_array().unwrap().is_empty());

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
        .any(|l| l["target"] == p["doc_id"] && l["link_type"] == "doc"));
}

#[test]
fn doc_save_surfaces_malformed_related_entries_as_warnings() {
    let (_tmp, dir) = setup_project();
    let resp = call(
        &dir,
        "handoff_doc_save",
        json!({
            "slug": unique_slug("bad-related-doc"),
            "title": "Doc with bad related entry",
            "body": "# H\n\nbody\n",
            "related": [
                { "id": "doc-good", "rel": "supersedes" },
                { "id": "doc-missing-rel" },
                { "rel": "missing-id" },
                {},
            ],
        }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    let warnings = p["warnings"].as_array().expect("warnings array");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap_or_default().contains("malformed")
                && w.as_str().unwrap_or_default().contains('3')),
        "3 malformed 'related' entries must be surfaced as a warning, not silently dropped: {warnings:?}"
    );

    let meta = payload(&call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &p["doc_id"], "format": "meta" }),
    ));
    let related = meta["related"].as_array().expect("related array");
    assert_eq!(
        related.len(),
        1,
        "only the well-formed related entry must survive: {related:?}"
    );
    assert_eq!(related[0]["id"], "doc-good");
}

#[test]
fn doc_save_surfaces_unresolved_task_ids_as_warnings() {
    let (_tmp, dir) = setup_project();
    let resp = call(
        &dir,
        "handoff_doc_save",
        json!({
            "slug": unique_slug("bad-link-doc"),
            "title": "Doc with bad link",
            "body": "# H\n\nbody\n",
            "task_ids": ["t-does-not-exist"],
        }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    let warnings = p["warnings"].as_array().expect("warnings array");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap_or_default().contains("t-does-not-exist")),
        "unresolved task id must be surfaced as a warning, not swallowed: {warnings:?}"
    );
}

// ---------------------------------------------------------------------
// doc_delete: cascade delete + task unlink + family tree cleanup
// ---------------------------------------------------------------------

#[test]
fn doc_delete_removes_doc_and_body_from_disk() {
    let (_tmp, dir) = setup_project();
    let body = "# Title\n\n## Section A\n\nBody A.\n";
    let slug = unique_slug("delete-me");
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": &slug, "title": "Delete Me", "body": body }),
    );
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    let docs_dir = dir.join(".handoff/docs");
    assert!(docs_dir.join(format!("_doc.{slug}.md")).exists());
    assert!(!docs_dir.join(format!("_doc.{slug}.json")).exists());

    let del_resp = call(&dir, "handoff_doc_delete", json!({ "doc_id": &doc_id }));
    assert!(!is_error(&del_resp), "error: {}", payload_text(&del_resp));
    let d = payload(&del_resp);
    assert_eq!(d["deleted"], true);
    assert_eq!(d["doc_id"], doc_id);
    // seq0 (preamble) + Title(H1) + Section A == 3 sections.
    assert_eq!(d["section_count"], 3);

    assert!(!docs_dir.join(format!("_doc.{slug}.json")).exists());
    assert!(!docs_dir.join(format!("_doc.{slug}.md")).exists());

    let get_resp = call(&dir, "handoff_doc_get", json!({ "doc_id": &doc_id }));
    assert!(is_error(&get_resp), "document must be gone after delete");
}

#[test]
fn doc_delete_unlinks_task_links() {
    let (_tmp, dir) = setup_project();
    let task_id = create_task(&dir, "Linked task for delete");

    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("linked-delete-doc"), "title": "Linked Delete Doc", "body": "# H\n\nbody\n", "task_ids": [&task_id] }),
    );
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    let del_resp = call(&dir, "handoff_doc_delete", json!({ "doc_id": &doc_id }));
    assert!(!is_error(&del_resp), "error: {}", payload_text(&del_resp));

    let task_resp = payload(&call(
        &dir,
        "handoff_get_task",
        json!({ "task_id": &task_id }),
    ));
    let links = task_resp["task_links"]
        .as_array()
        .or_else(|| task_resp["task"]["task_links"].as_array())
        .expect("task_links present");
    assert!(
        !links
            .iter()
            .any(|l| l["target"] == doc_id && l["link_type"] == "doc"),
        "task_links must be unlinked after doc_delete"
    );
}

#[test]
fn doc_delete_removes_self_from_parent_children_and_orphans_children() {
    let (_tmp, dir) = setup_project();
    let parent_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("parent-doc"), "title": "Parent Doc", "body": "# Parent\n\nbody\n" }),
    );
    let parent_id = payload(&parent_resp)["doc_id"]
        .as_str()
        .unwrap()
        .to_string();

    let child_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("child-doc"), "title": "Child Doc", "body": "# Child\n\nbody\n", "parent_id": &parent_id }),
    );
    let child_id = payload(&child_resp)["doc_id"].as_str().unwrap().to_string();

    let grandchild_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("grandchild-doc"), "title": "Grandchild Doc", "body": "# GC\n\nbody\n", "parent_id": &child_id }),
    );
    let grandchild_id = payload(&grandchild_resp)["doc_id"]
        .as_str()
        .unwrap()
        .to_string();

    // doc_save already wired parent.children <-> child.parent_id when the
    // child/grandchild were saved above; deleting the child must clean up
    // both directions: remove itself from the parent's children, and orphan
    // (clear parent_id on) its own child (the grandchild).
    let del_resp = call(&dir, "handoff_doc_delete", json!({ "doc_id": &child_id }));
    assert!(!is_error(&del_resp), "error: {}", payload_text(&del_resp));

    let grandchild_meta = payload(&call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &grandchild_id, "format": "meta" }),
    ));
    assert!(
        grandchild_meta["parent_id"].is_null(),
        "grandchild's parent_id must be cleared after its parent is deleted"
    );

    // Parent doc must still exist untouched (delete does not cascade upward),
    // and its `children` list must no longer contain the deleted child id.
    let parent_meta = payload(&call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &parent_id, "format": "meta" }),
    ));
    let parent_children = parent_meta["children"].as_array().expect("children array");
    assert!(
        !parent_children.iter().any(|c| c == &child_id),
        "deleted child id must be removed from the parent's children list: {parent_children:?}"
    );
}

#[test]
fn doc_delete_missing_doc_is_error() {
    let (_tmp, dir) = setup_project();
    let resp = call(&dir, "handoff_doc_delete", json!({ "doc_id": "doc-nope" }));
    assert!(is_error(&resp));
}

// ---------------------------------------------------------------------
// doc_reassemble: reversibility + drift detection
// ---------------------------------------------------------------------

#[test]
fn doc_reassemble_returns_original_body() {
    let (_tmp, dir) = setup_project();
    let body = "# Title\n\nIntro.\n\n## Section A\n\nBody A.\n";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("reassemble-doc"), "title": "Reassemble Doc", "body": body }),
    );
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    let resp = call(&dir, "handoff_doc_reassemble", json!({ "doc_id": &doc_id }));
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let r = payload(&resp);
    assert_eq!(r["body"], body);
    assert_eq!(r["drifted"], false);
}

/// Frontmatter migration (t123.1-t123.2): a manual edit to a document's `.md`
/// file — editing the body *below* handoff's own frontmatter block, which is
/// the realistic "someone opened the file in an editor" scenario — must
/// still be detected as drift by `doc_reassemble`, and `doc_get`/sections
/// must still work against the edited content (t123.2's on-demand
/// recomputation).
#[test]
fn doc_reassemble_detects_drift_after_direct_body_edit() {
    let (_tmp, dir) = setup_project();
    let body = "# Title\n\nIntro.\n\n## Section A\n\nBody A.\n";
    let slug = unique_slug("drift-doc");
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": &slug, "title": "Drift Doc", "body": body }),
    );
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    // Directly edit the document's body on disk, bypassing doc_save, to
    // simulate an out-of-band edit that leaves content_hash stale. Only the
    // body (after the frontmatter fence) is touched — the frontmatter block
    // itself is preserved verbatim, as a real manual edit in an editor
    // would leave it.
    let body_path = dir.join(".handoff/docs").join(format!("_doc.{slug}.md"));
    let original_content = std::fs::read_to_string(&body_path).unwrap();
    let edited_content =
        original_content.replace("# Title\n\nIntro.", "# Title (edited!)\n\nIntro.");
    assert_ne!(
        edited_content, original_content,
        "test fixture must actually change the body"
    );
    std::fs::write(&body_path, &edited_content).unwrap();

    let resp = call(&dir, "handoff_doc_reassemble", json!({ "doc_id": &doc_id }));
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let r = payload(&resp);
    assert_eq!(
        r["drifted"], true,
        "directly-edited body must be detected as drift"
    );
    assert!(r["body"].as_str().unwrap().contains("edited!"));
}

/// t123.3 migration spec, error-case branch: a `_doc.<slug>.md` file with no
/// YAML frontmatter *and* no paired `_doc.<slug>.json` sidecar is ambiguous
/// (could be a body-only leftover from a partial migration, or a plain file
/// that was never a handoff document) — it must be treated as "not found"
/// with a graceful tool error, not a panic or a false-positive read.
#[test]
fn doc_get_errors_gracefully_when_frontmatter_and_json_sidecar_both_missing() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("no-frontmatter-doc");
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": &slug, "title": "No Frontmatter Doc", "body": "# H\n\nbody\n" }),
    );
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    // Wipe the entire file, including the frontmatter block.
    let body_path = dir.join(".handoff/docs").join(format!("_doc.{slug}.md"));
    std::fs::write(&body_path, "Just plain text, no frontmatter at all.\n").unwrap();

    let resp = call(&dir, "handoff_doc_get", json!({ "doc_id": &doc_id }));
    assert!(
        is_error(&resp),
        "a frontmatter-less body file with no JSON sidecar must be a graceful error, got: {resp:?}"
    );
}

#[test]
fn doc_reassemble_writes_to_output_path() {
    let (_tmp, dir) = setup_project();
    let body = "# Title\n\nIntro.\n";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("output-doc"), "title": "Output Doc", "body": body }),
    );
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    let out_path = dir.join("exported.md");
    let resp = call(
        &dir,
        "handoff_doc_reassemble",
        json!({ "doc_id": &doc_id, "output_path": out_path.to_string_lossy() }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let r = payload(&resp);
    assert_eq!(r["output_path"], out_path.to_string_lossy().to_string());

    let written = std::fs::read_to_string(&out_path).unwrap();
    assert_eq!(written, body);
}

/// Frontmatter migration: `doc_reassemble` still restores the BOM
/// losslessly, but a user-authored leading frontmatter block in the
/// original `body` argument is absorbed into handoff's own frontmatter (see
/// `doc_save_then_get_full_absorbs_user_frontmatter_preserves_bom_crlf_body`)
/// rather than restored — so only the content after it round-trips.
#[test]
fn doc_reassemble_restores_bom_but_absorbs_user_frontmatter() {
    let (_tmp, dir) = setup_project();
    let body = "\u{FEFF}---\ntitle: Foo\n---\n# Title\n\nBody.\n";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("reassemble-bom-frontmatter"), "title": "Foo", "body": body }),
    );
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    let resp = call(&dir, "handoff_doc_reassemble", json!({ "doc_id": &doc_id }));
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let r = payload(&resp);
    assert_eq!(
        r["body"].as_str().unwrap(),
        "\u{FEFF}# Title\n\nBody.\n",
        "BOM restored, user frontmatter absorbed rather than round-tripped"
    );
}

#[test]
fn doc_reassemble_missing_doc_is_error() {
    let (_tmp, dir) = setup_project();
    let resp = call(
        &dir,
        "handoff_doc_reassemble",
        json!({ "doc_id": "doc-nope" }),
    );
    assert!(is_error(&resp));
}

// ---------------------------------------------------------------------
// doc_tree: family-tree traversal
// ---------------------------------------------------------------------

#[test]
fn doc_tree_returns_parent_and_children() {
    let (_tmp, dir) = setup_project();
    let parent_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("tree-parent"), "title": "Tree Parent", "body": "# Parent\n\nbody\n" }),
    );
    let parent_id = payload(&parent_resp)["doc_id"]
        .as_str()
        .unwrap()
        .to_string();

    let child_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("tree-child"), "title": "Tree Child", "body": "# Child\n\nbody\n", "parent_id": &parent_id }),
    );
    let child_id = payload(&child_resp)["doc_id"].as_str().unwrap().to_string();

    let tree_resp = call(&dir, "handoff_doc_tree", json!({ "doc_id": &parent_id }));
    assert!(!is_error(&tree_resp), "error: {}", payload_text(&tree_resp));
    let t = payload(&tree_resp);
    assert_eq!(t["id"], parent_id);
    assert_eq!(t["title"], "Tree Parent");
    assert!(t["parent"].is_null());
    let children = t["children"].as_array().expect("children array");
    assert_eq!(children.len(), 1);
    assert_eq!(children[0]["id"], child_id);
    assert_eq!(children[0]["title"], "Tree Child");
}

#[test]
fn doc_tree_from_child_includes_parent_info() {
    let (_tmp, dir) = setup_project();
    let parent_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("tree-parent-2"), "title": "Tree Parent 2", "body": "# Parent\n\nbody\n" }),
    );
    let parent_id = payload(&parent_resp)["doc_id"]
        .as_str()
        .unwrap()
        .to_string();

    let child_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("tree-child-2"), "title": "Tree Child 2", "body": "# Child\n\nbody\n", "parent_id": &parent_id }),
    );
    let child_id = payload(&child_resp)["doc_id"].as_str().unwrap().to_string();

    let tree_resp = call(&dir, "handoff_doc_tree", json!({ "doc_id": &child_id }));
    assert!(!is_error(&tree_resp), "error: {}", payload_text(&tree_resp));
    let t = payload(&tree_resp);
    assert_eq!(t["id"], child_id);
    assert_eq!(t["parent"]["id"], parent_id);
    assert_eq!(t["parent"]["title"], "Tree Parent 2");
}

/// The `depth` parameter must truncate traversal: with a 3-level chain
/// (parent -> child -> grandchild), `depth: 1` must include the child but not
/// the grandchild, and `depth: 0` must include neither (children array
/// empty).
#[test]
fn doc_tree_depth_truncates_traversal() {
    let (_tmp, dir) = setup_project();
    let parent_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("depth-parent"), "title": "Depth Parent", "body": "# Parent\n\nbody\n" }),
    );
    let parent_id = payload(&parent_resp)["doc_id"]
        .as_str()
        .unwrap()
        .to_string();

    let child_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("depth-child"), "title": "Depth Child", "body": "# Child\n\nbody\n", "parent_id": &parent_id }),
    );
    let child_id = payload(&child_resp)["doc_id"].as_str().unwrap().to_string();

    call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("depth-grandchild"), "title": "Depth Grandchild", "body": "# GC\n\nbody\n", "parent_id": &child_id }),
    );

    // depth: 1 -> child present, grandchild absent.
    let tree_depth1 = payload(&call(
        &dir,
        "handoff_doc_tree",
        json!({ "doc_id": &parent_id, "depth": 1 }),
    ));
    let children1 = tree_depth1["children"].as_array().expect("children array");
    assert_eq!(children1.len(), 1);
    assert_eq!(children1[0]["id"], child_id);
    let grandchildren1 = children1[0]["children"]
        .as_array()
        .expect("grandchildren array");
    assert!(
        grandchildren1.is_empty(),
        "depth: 1 must not descend into the grandchild level: {grandchildren1:?}"
    );

    // depth: 0 -> no children at all.
    let tree_depth0 = payload(&call(
        &dir,
        "handoff_doc_tree",
        json!({ "doc_id": &parent_id, "depth": 0 }),
    ));
    let children0 = tree_depth0["children"].as_array().expect("children array");
    assert!(
        children0.is_empty(),
        "depth: 0 must return no children: {children0:?}"
    );
}

#[test]
fn doc_tree_includes_related_when_requested() {
    let (_tmp, dir) = setup_project();
    let a_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("related-a"), "title": "Related A", "body": "# A\n\nbody\n" }),
    );
    let a_id = payload(&a_resp)["doc_id"].as_str().unwrap().to_string();

    let b_resp = call(
        &dir,
        "handoff_doc_save",
        json!({
            "slug": unique_slug("related-b"),
            "title": "Related B",
            "body": "# B\n\nbody\n",
            "related": [{ "id": &a_id, "rel": "references" }],
        }),
    );
    let b_id = payload(&b_resp)["doc_id"].as_str().unwrap().to_string();

    let tree_no_related = payload(&call(
        &dir,
        "handoff_doc_tree",
        json!({ "doc_id": &b_id, "include_related": false }),
    ));
    assert!(
        tree_no_related["related"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true),
        "related must be empty when include_related is false"
    );

    let tree_related = payload(&call(
        &dir,
        "handoff_doc_tree",
        json!({ "doc_id": &b_id, "include_related": true }),
    ));
    let related = tree_related["related"].as_array().expect("related array");
    assert_eq!(related.len(), 1);
    assert_eq!(related[0]["id"], a_id);
    assert_eq!(related[0]["title"], "Related A");
}

#[test]
fn doc_tree_missing_doc_is_error() {
    let (_tmp, dir) = setup_project();
    let resp = call(&dir, "handoff_doc_tree", json!({ "doc_id": "doc-nope" }));
    assert!(is_error(&resp));
}

#[test]
fn doc_save_update_unlinks_removed_task_ids() {
    let (_tmp, dir) = setup_project();
    let task_a = create_task(&dir, "Task A");
    let task_b = create_task(&dir, "Task B");

    let save1 = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("multi-link-doc"), "title": "Multi-link Doc", "body": "# H\n\nbody\n", "task_ids": [&task_a, &task_b] }),
    );
    let doc_id = payload(&save1)["doc_id"].as_str().unwrap().to_string();

    // Update: only keep task_a linked.
    let save2 = call(
        &dir,
        "handoff_doc_save",
        json!({ "doc_id": &doc_id, "title": "Multi-link Doc", "body": "# H\n\nbody\n", "task_ids": [&task_a] }),
    );
    assert!(!is_error(&save2), "error: {}", payload_text(&save2));

    let task_b_resp = payload(&call(
        &dir,
        "handoff_get_task",
        json!({ "task_id": &task_b }),
    ));
    let links_b = task_b_resp["task_links"]
        .as_array()
        .or_else(|| task_b_resp["task"]["task_links"].as_array())
        .expect("task_links present");
    assert!(
        !links_b
            .iter()
            .any(|l| l["target"] == doc_id && l["link_type"] == "doc"),
        "task_b must be unlinked after the update dropped it from task_ids"
    );

    let task_a_resp = payload(&call(
        &dir,
        "handoff_get_task",
        json!({ "task_id": &task_a }),
    ));
    let links_a = task_a_resp["task_links"]
        .as_array()
        .or_else(|| task_a_resp["task"]["task_links"].as_array())
        .expect("task_links present");
    assert!(
        links_a
            .iter()
            .any(|l| l["target"] == doc_id && l["link_type"] == "doc"),
        "task_a must remain linked"
    );
}

// ---------------------------------------------------------------------
// doc_save: append_body (t120.1)
// ---------------------------------------------------------------------

#[test]
fn doc_save_append_body_joins_with_default_separator() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("append-doc");
    let save1 = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": &slug, "title": "ADRs", "body": "# Architecture Decision Records\n\n## ADR-001: Redis Session\n\nBody 1.\n" }),
    );
    assert!(!is_error(&save1), "error: {}", payload_text(&save1));
    let doc_id = payload(&save1)["doc_id"].as_str().unwrap().to_string();

    let save2 = call(
        &dir,
        "handoff_doc_save",
        json!({ "doc_id": &doc_id, "append_body": "## ADR-002: GraphQL over REST\n\nBody 2.\n" }),
    );
    assert!(!is_error(&save2), "error: {}", payload_text(&save2));

    let full = payload(&call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "full" }),
    ));
    let body = full["body"].as_str().unwrap();
    assert!(body.contains("## ADR-001: Redis Session"));
    assert!(body.contains("## ADR-002: GraphQL over REST"));
    assert_eq!(
        body,
        "# Architecture Decision Records\n\n## ADR-001: Redis Session\n\nBody 1.\n\n\n## ADR-002: GraphQL over REST\n\nBody 2.\n",
        "default separator '\\n\\n' must be inserted between existing body and append_body"
    );
}

#[test]
fn doc_save_append_body_custom_separator() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("append-sep-doc");
    let save1 = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": &slug, "title": "Sep Doc", "body": "# Title\n\nFirst.\n" }),
    );
    let doc_id = payload(&save1)["doc_id"].as_str().unwrap().to_string();

    let save2 = call(
        &dir,
        "handoff_doc_save",
        json!({ "doc_id": &doc_id, "append_body": "Second.\n", "separator": "\n---\n\n" }),
    );
    assert!(!is_error(&save2), "error: {}", payload_text(&save2));

    let full = payload(&call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "full" }),
    ));
    assert_eq!(
        full["body"].as_str().unwrap(),
        "# Title\n\nFirst.\n\n---\n\nSecond.\n"
    );
}

#[test]
fn doc_save_body_and_append_body_are_mutually_exclusive() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("exclusive-doc");
    let save1 = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": &slug, "title": "Exclusive Doc", "body": "# Title\n\nBody.\n" }),
    );
    let doc_id = payload(&save1)["doc_id"].as_str().unwrap().to_string();

    let resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "doc_id": &doc_id, "body": "# Title\n\nNew.\n", "append_body": "## More\n\nStuff.\n" }),
    );
    assert!(
        is_error(&resp),
        "body and append_body must be mutually exclusive"
    );
}

#[test]
fn doc_save_append_body_without_doc_id_is_error() {
    let (_tmp, dir) = setup_project();
    let resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("no-doc-id"), "title": "New", "append_body": "## Section\n\nBody.\n" }),
    );
    assert!(
        is_error(&resp),
        "append_body without doc_id must error (no target document to append to)"
    );
}

#[test]
fn doc_save_append_body_without_body_or_append_body_is_error() {
    let (_tmp, dir) = setup_project();
    let resp = call(&dir, "handoff_doc_save", json!({ "title": "Nothing" }));
    assert!(
        is_error(&resp),
        "neither body nor append_body given must error"
    );
}

#[test]
fn doc_save_append_body_to_empty_existing_body_skips_separator() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("empty-append-doc");
    let save1 = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": &slug, "title": "Empty Doc", "body": "" }),
    );
    assert!(!is_error(&save1), "error: {}", payload_text(&save1));
    let doc_id = payload(&save1)["doc_id"].as_str().unwrap().to_string();

    let save2 = call(
        &dir,
        "handoff_doc_save",
        json!({ "doc_id": &doc_id, "append_body": "# Title\n\nContent.\n" }),
    );
    assert!(!is_error(&save2), "error: {}", payload_text(&save2));

    let full = payload(&call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "full" }),
    ));
    assert_eq!(full["body"].as_str().unwrap(), "# Title\n\nContent.\n");
}

#[test]
fn doc_save_append_body_recomputes_sections_and_content_hash() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("resection-doc");
    let save1 = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": &slug, "title": "Resection Doc", "body": "# Title\n\n## A\n\nBody A.\n" }),
    );
    let p1 = payload(&save1);
    let doc_id = p1["doc_id"].as_str().unwrap().to_string();
    let hash1 = p1["content_hash"].as_str().unwrap().to_string();
    let sections1 = p1["section_count"].as_u64().unwrap();

    let save2 = call(
        &dir,
        "handoff_doc_save",
        json!({ "doc_id": &doc_id, "append_body": "## B\n\nBody B.\n" }),
    );
    let p2 = payload(&save2);
    assert_ne!(
        p2["content_hash"].as_str().unwrap(),
        hash1,
        "content_hash must change after append_body"
    );
    assert_eq!(
        p2["section_count"].as_u64().unwrap(),
        sections1 + 1,
        "sections[] must be recomputed to include the appended section"
    );

    let meta = payload(&call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "meta" }),
    ));
    let sections = meta["sections"].as_array().unwrap();
    assert!(
        sections.iter().any(|s| s["heading"] == "B"),
        "new section 'B' must appear in sections[]"
    );
}

#[test]
fn doc_save_append_body_preserves_title_by_default_but_allows_override() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("title-preserve-doc");
    let save1 = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": &slug, "title": "Original Title", "body": "# Title\n\nBody.\n" }),
    );
    let doc_id = payload(&save1)["doc_id"].as_str().unwrap().to_string();

    // append_body without title -> title preserved.
    let save2 = call(
        &dir,
        "handoff_doc_save",
        json!({ "doc_id": &doc_id, "append_body": "## More\n\nStuff.\n" }),
    );
    assert_eq!(payload(&save2)["title"], "Original Title");

    // append_body with explicit title -> title updated.
    let save3 = call(
        &dir,
        "handoff_doc_save",
        json!({ "doc_id": &doc_id, "append_body": "## Even More\n\nStuff.\n", "title": "Updated Title" }),
    );
    assert_eq!(payload(&save3)["title"], "Updated Title");
}

#[test]
fn doc_save_append_body_preserves_verification_matrix_tail_append() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("verify-append-doc");
    let save1 = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": &slug, "title": "Verify Append Doc", "body": "# Title\n\n## A\n\nBody A.\n" }),
    );
    let doc_id = payload(&save1)["doc_id"].as_str().unwrap().to_string();

    let gen_resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": &doc_id, "action": "generate" }),
    );
    assert!(!is_error(&gen_resp), "error: {}", payload_text(&gen_resp));

    let check_resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": &doc_id, "action": "check", "fragment_seq": 1 }),
    );
    assert!(
        !is_error(&check_resp),
        "error: {}",
        payload_text(&check_resp)
    );

    // Tail-append a new section: existing fragment_seq 0/1 must remain
    // stable (verified status preserved) since preceding headings are
    // untouched.
    let save2 = call(
        &dir,
        "handoff_doc_save",
        json!({ "doc_id": &doc_id, "append_body": "## B\n\nBody B.\n" }),
    );
    assert!(!is_error(&save2), "error: {}", payload_text(&save2));

    let status = payload(&call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": &doc_id, "include_items": true }),
    ));
    let items = status["items"].as_array().unwrap();
    let item1 = items
        .iter()
        .find(|i| i["fragment_seq"] == 1)
        .expect("fragment_seq 1 must still exist after tail append");
    assert_eq!(
        item1["status"], "verified",
        "verification status for untouched preceding sections must survive a tail append"
    );
}

// ---------------------------------------------------------------------
// doc_save: h1 soft warning (t120.2)
// ---------------------------------------------------------------------

#[test]
fn doc_save_warns_when_body_does_not_start_with_h1() {
    let (_tmp, dir) = setup_project();
    let resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("no-h1-doc"), "title": "No H1 Doc", "body": "## Section only\n\nNo top-level heading.\n" }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    let warnings = p["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|w| w
            .as_str()
            .unwrap_or_default()
            .contains("does not start with a level-1 heading")),
        "warnings must include the soft h1 warning, got: {warnings:?}"
    );
}

#[test]
fn doc_save_no_h1_warning_when_body_starts_with_h1() {
    let (_tmp, dir) = setup_project();
    let resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("has-h1-doc"), "title": "Has H1 Doc", "body": "# Proper Title\n\nContent.\n" }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let warnings = payload(&resp)["warnings"].as_array().unwrap().clone();
    assert!(
        !warnings
            .iter()
            .any(|w| w.as_str().unwrap_or_default().contains("level-1 heading")),
        "no h1 warning expected when body starts with '# ', got: {warnings:?}"
    );
}

#[test]
fn doc_save_append_body_h1_warning_judged_on_combined_body() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("append-h1-doc");
    let save1 = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": &slug, "title": "Append H1 Doc", "body": "# Title\n\nIntro.\n" }),
    );
    let doc_id = payload(&save1)["doc_id"].as_str().unwrap().to_string();

    // Combined body still starts with '# Title' -> no warning even though
    // append_body itself starts with '##'.
    let save2 = call(
        &dir,
        "handoff_doc_save",
        json!({ "doc_id": &doc_id, "append_body": "## More\n\nStuff.\n" }),
    );
    assert!(!is_error(&save2), "error: {}", payload_text(&save2));
    let warnings = payload(&save2)["warnings"].as_array().unwrap().clone();
    assert!(
        !warnings
            .iter()
            .any(|w| w.as_str().unwrap_or_default().contains("level-1 heading")),
        "combined body starts with h1, so no warning expected, got: {warnings:?}"
    );
}

#[test]
fn doc_save_does_not_reject_body_without_h1() {
    let (_tmp, dir) = setup_project();
    // Saving must succeed (only a warning, never a hard rejection).
    let resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("soft-warn-doc"), "title": "Soft Warn Doc", "body": "No heading at all, just text.\n" }),
    );
    assert!(
        !is_error(&resp),
        "missing h1 must only produce a warning, not reject the save"
    );
}
