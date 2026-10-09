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

/// The section-hash-composed whole-document `content_hash` t370.15 (PR-4)
/// produces for `body` — mirrors `storage::docs::mod::tests::expected_content_hash`
/// so integration tests here can assert against the same real value the
/// server computes, not a stub.
fn expected_content_hash(body: &str) -> String {
    use handoff_mcp::storage::docs::split;
    let split_doc = split::split(body, split::DEFAULT_SPLIT_LEVEL).unwrap();
    let sections = split::compute_sections(&split_doc, true);
    split::compose_doc_hash(&sections)
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
    // spec without a `layer`: only the structured DIAG-D001 (t390.2) is expected.
    assert_eq!(warning_codes(&p), vec!["DIAG-D001".to_string()]);
    assert_eq!(p["warnings"].as_array().unwrap().len(), 1);

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
/// it must match the section-hash composition scheme's value for that body
/// (t370.15, PR-4 — see `storage::docs::split::compose_doc_hash`), be
/// identical across two saves of the same body, and differ when the body's
/// textual content changes. (A stub/constant hash would pass a "non-empty"
/// check but fail these equality/inequality assertions.)
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
    let expected_hash_a = expected_content_hash(body_a);
    assert_eq!(
        p_a1["content_hash"].as_str().unwrap(),
        expected_hash_a,
        "content_hash must equal the section-hash-composed value for the reassembled body"
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
// doc_save: metadata-only update path (M1, wiki/210 §M1)
// ---------------------------------------------------------------------

/// `doc_save(doc_id=..., task_ids=[...])` with no `body`/`append_body` must
/// succeed, leave the body and content_hash untouched, and still update
/// task_ids — metadata-only updates should not require re-sending the whole
/// document body.
#[test]
fn doc_save_metadata_only_update_leaves_body_and_hash_unchanged() {
    let (_tmp, dir) = setup_project();
    let body = "# Title\n\nIntro.\n\n## Section A\n\nBody A.\n";
    let slug = unique_slug("metadata-only-doc");
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": &slug, "title": "Metadata Only Doc", "body": body }),
    );
    assert!(
        !is_error(&save_resp),
        "initial save failed: {}",
        payload_text(&save_resp)
    );
    let created = payload(&save_resp);
    let doc_id = created["doc_id"].as_str().unwrap().to_string();
    let original_hash = created["content_hash"].as_str().unwrap().to_string();
    let original_section_count = created["section_count"].as_u64().unwrap();

    let task_id = create_task(&dir, "Linked to metadata-only doc");
    let update_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "doc_id": &doc_id, "task_ids": [task_id.clone()] }),
    );
    assert!(
        !is_error(&update_resp),
        "metadata-only update must succeed without body: {}",
        payload_text(&update_resp)
    );
    let updated = payload(&update_resp);
    assert_eq!(
        updated["content_hash"], original_hash,
        "content_hash must be unchanged when body is omitted"
    );
    assert_eq!(
        updated["section_count"], original_section_count,
        "section_count must be unchanged when body is omitted"
    );

    // Body on disk must be byte-identical (no accidental rewrite/loss).
    let get_resp = call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "full" }),
    );
    assert_eq!(payload(&get_resp)["body"], body);

    // task_ids must actually be linked (not silently dropped).
    let doc_get_meta = call(&dir, "handoff_doc_get", json!({ "doc_id": &doc_id }));
    let task_ids = payload(&doc_get_meta)["task_ids"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        task_ids.iter().any(|t| t == &json!(task_id)),
        "task_ids must be updated by the metadata-only save: {task_ids:?}"
    );
}

/// A new document (no `doc_id`, only `slug`) still requires `body`: omitting
/// both `body` and `append_body` on creation must remain an error.
#[test]
fn doc_save_new_document_still_requires_body() {
    let (_tmp, dir) = setup_project();
    let resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("no-body-new-doc"), "title": "No Body", "tags": ["x"] }),
    );
    assert!(
        is_error(&resp),
        "creating a new document without body/append_body must error"
    );
}

/// `doc_save(doc_id=..., auto_inject=...)` with no body must succeed and
/// actually apply the `auto_inject` change.
#[test]
fn doc_save_metadata_only_update_applies_auto_inject() {
    let (_tmp, dir) = setup_project();
    let body = "# Title\n\nIntro.\n";
    let slug = unique_slug("auto-inject-doc");
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": &slug, "title": "Auto Inject Doc", "body": body }),
    );
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    let update_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "doc_id": &doc_id, "auto_inject": "outline" }),
    );
    assert!(
        !is_error(&update_resp),
        "metadata-only auto_inject update must succeed: {}",
        payload_text(&update_resp)
    );

    let doc_get_meta = call(&dir, "handoff_doc_get", json!({ "doc_id": &doc_id }));
    assert_eq!(payload(&doc_get_meta)["auto_inject"], "outline");
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

/// M1 t360.4 (wiki/220-vmodel-integration-design.md §2.1): `doc_save`'s
/// `layer` argument is the only AI-facing way to set `DocMetadata.layer`.
/// Also verifies frontmatter round-trip via a fresh `doc_get`.
#[test]
fn doc_save_layer_argument_sets_reads_back_and_clears_with_empty_string() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("layer-doc");
    let save = call(
        &dir,
        "handoff_doc_save",
        json!({
            "slug": &slug,
            "title": "Layer Doc",
            "body": "# Layer Doc\n\nBody.\n",
            "layer": "basic_spec",
        }),
    );
    assert!(!is_error(&save), "error: {}", payload_text(&save));
    let doc_id = payload(&save)["doc_id"].as_str().unwrap().to_string();

    let meta = payload(&call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "meta" }),
    ));
    assert_eq!(meta["layer"], "basic_spec");

    // The frontmatter file itself must carry the layer key (round-trip
    // through disk, not just the in-memory response).
    let doc_path = dir.join(".handoff/docs").join(format!("_doc.{slug}.md"));
    let content = std::fs::read_to_string(&doc_path).unwrap();
    assert!(
        content.contains("layer: basic_spec"),
        "frontmatter must persist layer: {content}"
    );

    // Empty string clears it (wiki/220 §2.1: "空文字で解除").
    let clear = call(
        &dir,
        "handoff_doc_save",
        json!({ "doc_id": &doc_id, "title": "Layer Doc", "layer": "" }),
    );
    assert!(!is_error(&clear), "error: {}", payload_text(&clear));
    let meta_cleared = payload(&call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "meta" }),
    ));
    assert!(
        meta_cleared["layer"].is_null(),
        "empty string must clear layer: {meta_cleared}"
    );
    let content_cleared = std::fs::read_to_string(&doc_path).unwrap();
    assert!(
        !content_cleared.lines().any(|l| l.starts_with("layer:")),
        "cleared layer must not leave a layer: key in frontmatter: {content_cleared}"
    );
}

/// Collects the `code` of every structured warning in a `doc_save` response.
fn warning_codes(save_payload: &Value) -> Vec<String> {
    save_payload["warnings"]
        .as_array()
        .expect("warnings array")
        .iter()
        .filter_map(|w| w.get("code").and_then(|c| c.as_str()).map(str::to_string))
        .collect()
}

/// t390.2 (REQ-VGAP-002): a `spec`/`design` doc saved without a `layer` is
/// invisible to V-model traceability, so `doc_save` must say so with a
/// structured `DIAG-D001` warning; `adr`/`guide`/`note` never take a layer
/// and must stay quiet.
#[test]
fn doc_save_layer_missing_warns_for_spec_and_design_only() {
    let (_tmp, dir) = setup_project();
    for (doc_type, expect_warning) in [
        ("spec", true),
        ("design", true),
        ("adr", false),
        ("guide", false),
        ("note", false),
    ] {
        let save = call(
            &dir,
            "handoff_doc_save",
            json!({
                "slug": unique_slug("layer-missing"),
                "title": "Layer Missing",
                "doc_type": doc_type,
                "body": "# Layer Missing\n\nBody.\n",
            }),
        );
        assert!(!is_error(&save), "error: {}", payload_text(&save));
        let codes = warning_codes(&payload(&save));
        assert_eq!(
            codes.iter().any(|c| c == "DIAG-D001"),
            expect_warning,
            "doc_type={doc_type}: warnings={}",
            payload(&save)["warnings"]
        );
        if expect_warning {
            let w = payload(&save)["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .find(|w| w["code"] == "DIAG-D001")
                .cloned()
                .unwrap();
            assert_eq!(w["severity"], "warning");
            assert!(
                w["fix_hint"].as_str().unwrap().contains("doc_save(layer="),
                "fix_hint must show how to set a layer: {w}"
            );
        }
    }
}

/// A spec doc that does carry a layer gets no `DIAG-D001`.
#[test]
fn doc_save_layer_missing_silent_when_layer_set() {
    let (_tmp, dir) = setup_project();
    let save = call(
        &dir,
        "handoff_doc_save",
        json!({
            "slug": unique_slug("layer-set"),
            "title": "Layer Set",
            "doc_type": "spec",
            "layer": "basic_spec",
            "body": "# Layer Set\n\nBody.\n",
        }),
    );
    assert!(!is_error(&save), "error: {}", payload_text(&save));
    assert!(
        !warning_codes(&payload(&save))
            .iter()
            .any(|c| c == "DIAG-D001"),
        "warnings={}",
        payload(&save)["warnings"]
    );
}

/// t390.7 (REQ-VGAP-005): clearing a layer with `layer=""` does not delete
/// the document; the response must point at `handoff_doc_delete`.
#[test]
fn doc_save_layer_clear_points_at_doc_delete() {
    let (_tmp, dir) = setup_project();
    let save = call(
        &dir,
        "handoff_doc_save",
        json!({
            "slug": unique_slug("layer-clear"),
            "title": "Layer Clear",
            "doc_type": "note",
            "layer": "basic_spec",
            "body": "# Layer Clear\n\nBody.\n",
        }),
    );
    let doc_id = payload(&save)["doc_id"].as_str().unwrap().to_string();
    let clear = call(
        &dir,
        "handoff_doc_save",
        json!({ "doc_id": &doc_id, "title": "Layer Clear", "layer": "" }),
    );
    assert!(!is_error(&clear), "error: {}", payload_text(&clear));
    let warnings = payload(&clear)["warnings"].clone();
    assert!(
        warnings
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().is_some_and(|s| s.contains("handoff_doc_delete"))),
        "layer clear must mention handoff_doc_delete: {warnings}"
    );

    // Not clearing (layer omitted) must not emit the hint.
    let plain = call(
        &dir,
        "handoff_doc_save",
        json!({ "doc_id": &doc_id, "title": "Layer Clear 2" }),
    );
    assert!(
        !payload(&plain)["warnings"]
            .to_string()
            .contains("handoff_doc_delete"),
        "unexpected hint: {}",
        payload(&plain)["warnings"]
    );
}

/// wiki/220 §2.4 "付随修正": omitting `split_level` on an *update* must keep
/// the document's existing value — before this fix it silently reset to the
/// default (2) on every metadata-only or body update that didn't explicitly
/// repeat `split_level`, which could re-split the body into different
/// sections than the caller last set.
#[test]
fn doc_save_update_without_split_level_preserves_existing_value() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("split-level-doc");
    let body = "# Title\n\n## A\n\nBody A.\n\n### A.1\n\nNested.\n";
    let save1 = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": &slug, "title": "Split Level Doc", "body": body, "split_level": 3 }),
    );
    let doc_id = payload(&save1)["doc_id"].as_str().unwrap().to_string();
    // split_level=3 splits on `###` too: seq0 + Title + A + A.1 == 4.
    assert_eq!(payload(&save1)["section_count"], 4);

    // Update without repeating `split_level` — must still behave as
    // split_level=3, not silently reset to the default (2).
    let save2 = call(
        &dir,
        "handoff_doc_save",
        json!({ "doc_id": &doc_id, "title": "Split Level Doc", "body": body }),
    );
    assert!(!is_error(&save2), "error: {}", payload_text(&save2));
    assert_eq!(
        payload(&save2)["section_count"],
        4,
        "split_level must be preserved from the existing document when omitted"
    );

    let meta = payload(&call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "meta" }),
    ));
    assert_eq!(meta["section_count"], 4);
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

/// FR-804/E11 (wiki/260-vmodel-m2-design.md §4.12, M2-18): the real aelm
/// corpus shape (`scope_paths:` immediately followed by a lone `[]` line —
/// 9 of 209 real documents) must be reported in `doc_list`'s `unreadable`
/// field, not silently dropped, and `handoff_doc_repair_frontmatter` must be
/// able to fix it and return it to the normal listing.
#[test]
fn doc_list_reports_unreadable_frontmatter_and_repair_tool_fixes_it() {
    let (_tmp, dir) = setup_project();
    call(
        &dir,
        "handoff_doc_save",
        json!({
            "slug": unique_slug("good-doc"),
            "title": "Good Doc",
            "body": "# Good\n",
            "doc_type": "spec",
        }),
    );

    let handoff = dir.join(".handoff");
    let bad_path = handoff.join("docs").join("_doc.aelm-shape.md");
    std::fs::write(
        &bad_path,
        "---\nid: doc-aelm-shape\ntitle: Aelm Shape\ndoc_type: spec\ntags:\n\
         - specification\nscope_paths:\n[]\nparent_id: null\nchildren: []\n\
         related: []\nauto_inject: auto\ntask_ids: []\nsource:\n  origin: authored\n\
         has_bom: false\nline_ending: lf\nsplit_level: 2\n\
         created_at: 2026-01-01T00:00:00Z\nupdated_at: 2026-01-01T00:00:00Z\n\
         content_hash: abc123\n---\n# Aelm Shape\n",
    )
    .unwrap();

    // 1. doc_list must report it as unreadable, not silently drop it, and
    //    the well-formed document must still be listed.
    let listed = payload(&call(&dir, "handoff_doc_list", json!({})));
    assert_eq!(listed["documents"].as_array().unwrap().len(), 1);
    let unreadable = listed["unreadable"].as_array().unwrap();
    assert_eq!(unreadable.len(), 1);
    assert_eq!(unreadable[0]["slug"], "aelm-shape");
    assert!(unreadable[0]["error"].as_str().unwrap().contains("YAML"));
    assert!(unreadable[0]["line"].is_number());

    // 2. dry_run (default) must report what it would fix without touching
    //    the file.
    let before_bytes = std::fs::read(&bad_path).unwrap();
    let dry = payload(&call(&dir, "handoff_doc_repair_frontmatter", json!({})));
    assert_eq!(dry["dry_run"], true);
    let repaired = dry["repaired"].as_array().unwrap();
    assert_eq!(repaired.len(), 1);
    assert_eq!(repaired[0]["slug"], "aelm-shape");
    assert_eq!(repaired[0]["applied"], false);
    assert_eq!(
        std::fs::read(&bad_path).unwrap(),
        before_bytes,
        "dry_run must not write anything"
    );

    // 3. dry_run=false actually applies the fix.
    let applied = payload(&call(
        &dir,
        "handoff_doc_repair_frontmatter",
        json!({ "dry_run": false }),
    ));
    assert_eq!(applied["dry_run"], false);
    let applied_list = applied["repaired"].as_array().unwrap();
    assert_eq!(applied_list.len(), 1);
    assert_eq!(applied_list[0]["applied"], true);

    // 4. the document must be back in the normal listing, no longer
    //    unreadable, and its self-check-written frontmatter must itself
    //    parse (round trip via a second doc_list call).
    let after = payload(&call(&dir, "handoff_doc_list", json!({})));
    assert_eq!(after["documents"].as_array().unwrap().len(), 2);
    assert!(after["unreadable"].as_array().unwrap().is_empty());
    let titles: Vec<&str> = after["documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["title"].as_str().unwrap())
        .collect();
    assert!(titles.contains(&"Aelm Shape"));
}

#[test]
fn doc_repair_frontmatter_slug_filter_only_touches_the_named_document() {
    let (_tmp, dir) = setup_project();
    let handoff = dir.join(".handoff");
    std::fs::create_dir_all(handoff.join("docs")).unwrap();
    for slug in ["bad-one", "bad-two"] {
        std::fs::write(
            handoff.join("docs").join(format!("_doc.{slug}.md")),
            format!(
                "---\nid: doc-{slug}\ntitle: T\ndoc_type: spec\nscope_paths:\n[]\n\
                 created_at: 2026-01-01T00:00:00Z\nupdated_at: 2026-01-01T00:00:00Z\n---\nbody\n"
            ),
        )
        .unwrap();
    }

    let resp = payload(&call(
        &dir,
        "handoff_doc_repair_frontmatter",
        json!({ "slug": "bad-one", "dry_run": false }),
    ));
    let repaired = resp["repaired"].as_array().unwrap();
    assert_eq!(repaired.len(), 1);
    assert_eq!(repaired[0]["slug"], "bad-one");

    let listed = payload(&call(&dir, "handoff_doc_list", json!({})));
    assert_eq!(
        listed["unreadable"].as_array().unwrap().len(),
        1,
        "bad-two must remain unreadable — the slug filter must not touch it"
    );
    assert_eq!(listed["unreadable"][0]["slug"], "bad-two");
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

/// M2-15 (wiki/260 §4.8/FR-601): `doc.task_ids` is derived from the task
/// side it just wrote (`TaskLink{link_type:"doc"}`), not echoed back
/// verbatim from the caller's argument — a task id that fails to resolve
/// gets no reverse link, so it must not be left stuck in `doc.task_ids`
/// either (previously it was, a standing document-level drift that nothing
/// but another `doc_save` with a narrower list could ever clear).
#[test]
fn doc_save_task_ids_omits_an_unresolved_id_instead_of_echoing_it_back() {
    let (_tmp, dir) = setup_project();
    let task_id = create_task(&dir, "Real task");
    let resp = call(
        &dir,
        "handoff_doc_save",
        json!({
            "slug": unique_slug("bad-link-doc-derive"),
            "title": "Doc with one bad link",
            "body": "# H\n\nbody\n",
            "task_ids": [&task_id, "t-does-not-exist"],
        }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let doc_id = payload(&resp)["doc_id"].as_str().unwrap().to_string();

    let doc_get_meta = call(&dir, "handoff_doc_get", json!({ "doc_id": &doc_id }));
    let task_ids = payload(&doc_get_meta)["task_ids"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        task_ids,
        vec![json!(task_id)],
        "the resolved id must be kept and the unresolved one dropped, not echoed back: {task_ids:?}"
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

// ---------------------------------------------------------------------
// doc_update_section: partial section update API (t123.4)
// ---------------------------------------------------------------------

#[test]
fn doc_update_section_replaces_section_and_doc_get_reflects_it() {
    let (_tmp, dir) = setup_project();
    // split_level=2 (default) includes the H1 as its own fragment (seq 1);
    // "## Section A" is seq 2.
    let body = "# Title\n\nIntro.\n\n## Section A\n\nOld body A.\n";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("update-section-doc"), "title": "Update Section Doc", "body": body }),
    );
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    let update_resp = call(
        &dir,
        "handoff_doc_update_section",
        json!({
            "doc_id": &doc_id,
            "seq": 2,
            "new_content": "## Section A\n\nNew body A.\n",
        }),
    );
    assert!(
        !is_error(&update_resp),
        "error: {}",
        payload_text(&update_resp)
    );
    let u = payload(&update_resp);
    assert_eq!(u["seq"], 2);
    assert_eq!(u["heading"], "Section A");

    let get_resp = call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "section", "seq": 2 }),
    );
    let g = payload(&get_resp);
    assert_eq!(g["body"], "## Section A\n\nNew body A.\n");

    let full_resp = call(&dir, "handoff_doc_get", json!({ "doc_id": &doc_id }));
    let full = payload(&full_resp);
    assert!(full["body"].as_str().unwrap().contains("New body A."));
    assert!(!full["body"].as_str().unwrap().contains("Old body A."));
}

#[test]
fn doc_update_section_optimistic_lock_correct_hash_succeeds() {
    let (_tmp, dir) = setup_project();
    let body = "# Title\n\n## Section A\n\nBody A.\n";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("lock-ok-doc"), "title": "Lock Ok Doc", "body": body }),
    );
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    let section = payload(&call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "section", "seq": 1 }),
    ));
    let expected_hash = section["content_hash"].as_str().unwrap().to_string();

    let update_resp = call(
        &dir,
        "handoff_doc_update_section",
        json!({
            "doc_id": &doc_id,
            "seq": 1,
            "new_content": "## Section A\n\nUpdated.\n",
            "expected_hash": expected_hash,
        }),
    );
    assert!(
        !is_error(&update_resp),
        "correct expected_hash must allow the update: {}",
        payload_text(&update_resp)
    );
}

#[test]
fn doc_update_section_optimistic_lock_wrong_hash_fails_with_current_hash() {
    let (_tmp, dir) = setup_project();
    let body = "# Title\n\n## Section A\n\nBody A.\n";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("lock-fail-doc"), "title": "Lock Fail Doc", "body": body }),
    );
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    let update_resp = call(
        &dir,
        "handoff_doc_update_section",
        json!({
            "doc_id": &doc_id,
            "seq": 2,
            "new_content": "## Section A\n\nShould not apply.\n",
            "expected_hash": "definitely-wrong-hash",
        }),
    );
    assert!(
        is_error(&update_resp),
        "wrong expected_hash must be rejected"
    );
    let err_text = payload_text(&update_resp);
    let section = payload(&call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "section", "seq": 2 }),
    ));
    let current_hash = section["content_hash"].as_str().unwrap();
    assert!(
        err_text.contains(current_hash),
        "error message must include the current hash for retry, got: {err_text}"
    );

    // Content must be unchanged after the rejected update.
    assert!(section["body"].as_str().unwrap().contains("Body A."));
}

#[test]
fn doc_update_section_updates_updated_at() {
    let (_tmp, dir) = setup_project();
    let body = "# Title\n\n## Section A\n\nBody A.\n";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("updated-at-doc"), "title": "Updated At Doc", "body": body }),
    );
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();
    let before = payload(&call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "meta" }),
    ))["updated_at"]
        .as_str()
        .unwrap()
        .to_string();

    std::thread::sleep(std::time::Duration::from_millis(10));

    call(
        &dir,
        "handoff_doc_update_section",
        json!({
            "doc_id": &doc_id,
            "seq": 1,
            "new_content": "## Section A\n\nUpdated body.\n",
        }),
    );

    let after = payload(&call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "meta" }),
    ))["updated_at"]
        .as_str()
        .unwrap()
        .to_string();

    assert_ne!(before, after, "updated_at must change after section update");
}

#[test]
fn doc_update_section_marks_verification_item_stale() {
    let (_tmp, dir) = setup_project();
    let body = "# Title\n\n## Section A\n\nBody A.\n";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("stale-verify-doc"), "title": "Stale Verify Doc", "body": body }),
    );
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

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

    let status_before = payload(&call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": &doc_id, "include_items": true }),
    ));
    let item_before = status_before["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["fragment_seq"] == 1)
        .unwrap();
    assert_eq!(item_before["stale"], false);

    let update_resp = call(
        &dir,
        "handoff_doc_update_section",
        json!({
            "doc_id": &doc_id,
            "seq": 1,
            "new_content": "## Section A\n\nChanged content invalidating verification.\n",
        }),
    );
    assert!(
        !is_error(&update_resp),
        "error: {}",
        payload_text(&update_resp)
    );

    let status_after = payload(&call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": &doc_id, "include_items": true }),
    ));
    let item_after = status_after["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["fragment_seq"] == 1)
        .unwrap();
    assert_eq!(
        item_after["stale"], true,
        "verification item must become stale after its section content changes"
    );
}

/// t360.42 S4 (M1 adversarial review): `skip`/`set_*` actions load the
/// document via the lazy, no-hash path (`resolve_doc_for_verify(need_hash:
/// false)`), so every section's `content_hash` is `None` in that in-memory
/// snapshot — including sections belonging to items `check`ed earlier in a
/// completely separate call. A `None` section hash must be treated as
/// "cannot determine staleness" (not stale), not "different from
/// `content_hash_at_verify`" (stale): checking seq1, then skipping seq2,
/// must not make the `skip` mutation response's `stale` count include the
/// already-checked (and, per a follow-up `doc_verify_status` call, genuinely
/// not-stale) seq1 item.
#[test]
fn skip_action_response_does_not_falsely_count_a_checked_item_as_stale() {
    let (_tmp, dir) = setup_project();
    let body = "# Title\n\n## Section A\n\nBody A.\n\n## Section B\n\nBody B.\n";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("skip-stale-doc"), "title": "Skip Stale Doc", "body": body }),
    );
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

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

    let skip_resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": &doc_id, "action": "skip", "fragment_seq": 2 }),
    );
    assert!(!is_error(&skip_resp), "error: {}", payload_text(&skip_resp));
    let skip_payload = payload(&skip_resp);
    assert_eq!(
        skip_payload["stale"], 0,
        "the skip mutation response must not falsely count the already-checked \
         item as stale just because this lazy action loaded the doc without \
         hashes: {skip_payload}"
    );

    let status = payload(&call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": &doc_id, "include_items": true }),
    ));
    assert_eq!(
        status["progress"]["stale"], 0,
        "doc_verify_status must agree there is no real staleness: {status}"
    );
}

#[test]
fn doc_update_section_shows_drift_in_doc_reassemble() {
    let (_tmp, dir) = setup_project();
    let body = "# Title\n\n## Section A\n\nBody A.\n";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("drift-doc"), "title": "Drift Doc", "body": body }),
    );
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    let reassemble_before = payload(&call(
        &dir,
        "handoff_doc_reassemble",
        json!({ "doc_id": &doc_id }),
    ));
    assert_eq!(reassemble_before["drifted"], false);

    // NOTE: `handoff_doc_update_section` goes through the normal write_doc_body
    // + write_doc path (same as doc_save), so it updates source.canonical_hash
    // in step with content_hash, meaning `doc_reassemble`'s own drift signal
    // (source.canonical_hash vs content_hash) stays `false` right after this
    // update — the same behavior doc_save has. What we can verify here is
    // that the reassembled body reflects the new content.
    call(
        &dir,
        "handoff_doc_update_section",
        json!({
            "doc_id": &doc_id,
            "seq": 1,
            "new_content": "## Section A\n\nNew body via update_section.\n",
        }),
    );

    let reassemble_after = payload(&call(
        &dir,
        "handoff_doc_reassemble",
        json!({ "doc_id": &doc_id }),
    ));
    assert_eq!(
        reassemble_after["drifted"], false,
        "handoff_doc_update_section writes through write_doc, so canonical_hash tracks content_hash just like doc_save"
    );
    assert!(reassemble_after["body"]
        .as_str()
        .unwrap()
        .contains("New body via update_section."));

    // Drift IS detected when the file is edited fully out-of-band afterward,
    // confirming reassemble's drift check still works post-update.
    let slug = payload(&call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "meta" }),
    ))["slug"]
        .as_str()
        .unwrap()
        .to_string();
    let body_path = dir.join(".handoff/docs").join(format!("_doc.{slug}.md"));
    let mut current = std::fs::read_to_string(&body_path).unwrap();
    current.push_str("\nOut of band edit.\n");
    std::fs::write(&body_path, current).unwrap();

    let reassemble_final = payload(&call(
        &dir,
        "handoff_doc_reassemble",
        json!({ "doc_id": &doc_id }),
    ));
    assert_eq!(reassemble_final["drifted"], true);
}

/// t370.15 (PR-4, wiki/240-performance-design.md §6): a document written by
/// a pre-t370.15 binary has `content_hash`/`source.canonical_hash` computed
/// via the old direct `lexsim::content_hash(whole_body)` pass and no
/// `source.content_hash_scheme` marker at all. `handoff_doc_reassemble`'s
/// drift check must not misinterpret the resulting scheme mismatch (fresh
/// reads always compose from section hashes now) as real drift — an
/// untouched legacy document must still report `drifted: false`. The
/// migration is self-healing: after any write (e.g. `doc_update_section`),
/// the document carries the new scheme's marker and a genuine out-of-band
/// edit is still correctly detected as drift.
#[test]
fn doc_reassemble_does_not_false_positive_drift_on_legacy_scheme_document() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("legacy-scheme-doc");
    let body = "# Legacy Doc\n\n## Section A\n\nBody A.\n";
    let old_scheme_hash = lexsim::content_hash(body);

    // Hand-write a `_doc.<slug>.md` exactly as a pre-t370.15 binary would
    // have: `content_hash`/`source.canonical_hash` both the *old* direct
    // whole-body lexsim hash, no `content_hash_scheme` key in `source:` at
    // all.
    let doc_id = "doc-20260101-000000-000001";
    let frontmatter = format!(
        "---\n\
         id: {doc_id}\n\
         title: Legacy Scheme Doc\n\
         doc_type: spec\n\
         created_at: \"2026-01-01T00:00:00Z\"\n\
         updated_at: \"2026-01-01T00:00:00Z\"\n\
         content_hash: \"{old_scheme_hash}\"\n\
         source:\n  canonical_hash: \"{old_scheme_hash}\"\n\
         ---\n"
    );
    let docs_dir = dir.join(".handoff/docs");
    std::fs::create_dir_all(&docs_dir).unwrap();
    std::fs::write(
        docs_dir.join(format!("_doc.{slug}.md")),
        format!("{frontmatter}{body}"),
    )
    .unwrap();

    let reassemble = payload(&call(
        &dir,
        "handoff_doc_reassemble",
        json!({ "doc_id": doc_id }),
    ));
    assert_eq!(
        reassemble["drifted"], false,
        "an untouched legacy-scheme document must never be reported as drifted just because \
         fresh reads now compose the hash differently: {reassemble}"
    );
    assert_eq!(reassemble["body"], body);

    // A genuine out-of-band edit to the same legacy document must still be
    // detected — the compatibility fallback must not mask real drift either.
    let mut edited = std::fs::read_to_string(docs_dir.join(format!("_doc.{slug}.md"))).unwrap();
    edited.push_str("\nOut of band edit.\n");
    std::fs::write(docs_dir.join(format!("_doc.{slug}.md")), edited).unwrap();

    let reassemble_after_edit = payload(&call(
        &dir,
        "handoff_doc_reassemble",
        json!({ "doc_id": doc_id }),
    ));
    assert_eq!(
        reassemble_after_edit["drifted"], true,
        "a real out-of-band edit to a legacy-scheme document must still be detected as drift"
    );

    // The very next write (doc_update_section) migrates the document forward
    // — after this, a fresh, untouched reassemble stays `false` via the
    // normal (non-fallback) path too.
    call(
        &dir,
        "handoff_doc_update_section",
        json!({
            "doc_id": doc_id,
            "seq": 1,
            "new_content": "## Section A\n\nMigrated body.\n",
        }),
    );
    let reassemble_after_migration = payload(&call(
        &dir,
        "handoff_doc_reassemble",
        json!({ "doc_id": doc_id }),
    ));
    assert_eq!(reassemble_after_migration["drifted"], false);
}

#[test]
fn doc_update_section_seq_not_found_is_error() {
    let (_tmp, dir) = setup_project();
    let body = "# Title\n\n## Section A\n\nBody A.\n";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("seq-nf-doc"), "title": "Seq NF Doc", "body": body }),
    );
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    let update_resp = call(
        &dir,
        "handoff_doc_update_section",
        json!({
            "doc_id": &doc_id,
            "seq": 99,
            "new_content": "## Nonexistent\n\nBody.\n",
        }),
    );
    assert!(is_error(&update_resp), "seq not found must be an error");
}

#[test]
fn doc_update_section_empty_content_deletes_section() {
    let (_tmp, dir) = setup_project();
    let body = "# Title\n\n## Section A\n\nBody A.\n\n## Section B\n\nBody B.\n";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("empty-content-doc"), "title": "Empty Content Doc", "body": body }),
    );
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    let update_resp = call(
        &dir,
        "handoff_doc_update_section",
        json!({
            "doc_id": &doc_id,
            "seq": 2,
            "new_content": "",
        }),
    );
    assert!(
        !is_error(&update_resp),
        "empty new_content must be allowed (deletes the section): {}",
        payload_text(&update_resp)
    );

    let full = payload(&call(&dir, "handoff_doc_get", json!({ "doc_id": &doc_id })));
    let full_body = full["body"].as_str().unwrap();
    assert!(!full_body.contains("Section A"));
    assert!(full_body.contains("Section B"));
}

#[test]
fn doc_update_section_seq_zero_preamble_is_allowed() {
    let (_tmp, dir) = setup_project();
    let body = "# Title\n\nOld preamble.\n\n## Section A\n\nBody A.\n";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": unique_slug("seq-zero-doc"), "title": "Seq Zero Doc", "body": body }),
    );
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    let update_resp = call(
        &dir,
        "handoff_doc_update_section",
        json!({
            "doc_id": &doc_id,
            "seq": 0,
            "new_content": "# Title\n\nNew preamble.\n\n",
        }),
    );
    assert!(
        !is_error(&update_resp),
        "seq=0 (preamble) must be updatable: {}",
        payload_text(&update_resp)
    );

    let full = payload(&call(&dir, "handoff_doc_get", json!({ "doc_id": &doc_id })));
    assert!(full["body"].as_str().unwrap().contains("New preamble."));
}

/// t360.42 S3 (M1 adversarial review, wiki/220 §4.3 "派生ファイルは最後 /
/// derived files last"): a single `doc_save` call that both triggers a layer
/// sync (new doc, `layer` set for the first time -> `structural_change`) and
/// sets `parent_id` (writing the parent document's `children` list) used to
/// write the parent doc *after* `refresh_after_layer_sync` had already
/// written the derived `_requirements_summary.json` — so the summary's own
/// `inputs.docs_max_mtime_ns`/`docs_count` fingerprint, computed before the
/// parent write, no longer matched the actual on-disk document set the
/// instant that same call finished. This must never happen: the persisted
/// fingerprint must always describe "every document this call wrote",
/// verified here by independently re-stat'ing every `_doc.*.md` file after
/// the call and comparing against the summary's own recorded fingerprint.
#[test]
fn doc_save_writes_summary_last_even_with_both_layer_sync_and_parent_link_in_one_call() {
    let (_tmp, dir) = setup_project();

    let parent_saved = call(
        &dir,
        "handoff_doc_save",
        json!({
            "slug": unique_slug("s3-parent-doc"),
            "title": "S3 Parent Doc",
            "body": "# Parent\n\nParent body.\n",
        }),
    );
    let parent_doc_id = payload(&parent_saved)["doc_id"]
        .as_str()
        .unwrap()
        .to_string();

    // A brand-new document with `layer` set from the start (structural
    // change -> layer sync runs) AND `parent_id` given in the very same
    // call (parent doc gets its `children` list updated).
    let body = "# Child Spec\n\n### REQ-901 Something\n\nBody.\n";
    let child_saved = call(
        &dir,
        "handoff_doc_save",
        json!({
            "slug": unique_slug("s3-child-doc"),
            "title": "S3 Child Doc",
            "body": body,
            "layer": "basic_spec",
            "parent_id": &parent_doc_id,
        }),
    );
    assert!(
        !is_error(&child_saved),
        "error: {}",
        payload_text(&child_saved)
    );

    // Independently re-derive the docs_* fingerprint by re-stat'ing every
    // `_doc.*.md` file right now, exactly as `compute_derived_inputs`
    // itself would.
    let docs_dir = dir.join(".handoff").join("docs");
    let mut actual_max_ns: u128 = 0;
    let mut actual_count: usize = 0;
    for entry in std::fs::read_dir(&docs_dir).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("_doc.") || !name.ends_with(".md") {
            continue;
        }
        let mtime = entry
            .metadata()
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        actual_max_ns = actual_max_ns.max(mtime);
        actual_count += 1;
    }

    let summary_path = docs_dir.join("_requirements_summary.json");
    let summary: Value =
        serde_json::from_str(&std::fs::read_to_string(&summary_path).unwrap()).unwrap();
    let persisted_max_ns = summary["inputs"]["docs_max_mtime_ns"].as_u64().unwrap() as u128;
    let persisted_count = summary["inputs"]["docs_count"].as_u64().unwrap() as usize;

    assert_eq!(
        persisted_count, actual_count,
        "persisted docs_count must match the actual on-disk document set \
         right after this call"
    );
    assert_eq!(
        persisted_max_ns, actual_max_ns,
        "persisted docs_max_mtime_ns must match the actual max mtime right \
         after this call — a mismatch means the summary (a derived file) was \
         written before some other write this same call made (here: the \
         parent doc's children-list update), violating \"derived files last\""
    );
}
