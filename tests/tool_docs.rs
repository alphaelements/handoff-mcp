//! Integration tests for the P1-6a document tools (doc_save / doc_get /
//! doc_list), exercised end-to-end through the JSON-RPC `process_line` entry
//! point — the same path the MCP server runs in production (mirrors
//! `tests/tool_memory.rs`).

use serde_json::{json, Value};

fn send(input: &str) -> Option<Value> {
    let result = handoff_mcp::mcp::protocol::process_line(input)?;
    Some(serde_json::from_str(&result).expect("response should be valid JSON"))
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
fn doc_save_creates_new_document_and_fragments() {
    let (_tmp, dir) = setup_project();
    let body = "# Title\n\nIntro.\n\n## Section A\n\nBody A.\n\n## Section B\n\nBody B.\n";
    let resp = call(
        &dir,
        "handoff_doc_save",
        json!({
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
    assert_eq!(p["title"], "Session Loop");
    assert_eq!(p["doc_type"], "spec");
    // seq0 (preamble) + Title(H1) + Section A + Section B == 4 fragments.
    assert_eq!(p["fragment_count"], 4);
    assert!(!p["content_hash"].as_str().unwrap_or_default().is_empty());
    assert!(p["warnings"].as_array().unwrap().is_empty());

    // Fragment files actually exist on disk.
    let docs_dir = dir.join(".handoff/docs");
    assert!(docs_dir
        .join(format!("_doc.{}.json", p["doc_id"].as_str().unwrap()))
        .exists());
    assert!(docs_dir
        .join(format!("_frag.{}.0.md", p["doc_id"].as_str().unwrap()))
        .exists());
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
        json!({ "title": "Hash Doc A", "body": body_a }),
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
        json!({ "title": "Hash Doc A2", "body": body_a }),
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
        json!({ "title": "Hash Doc B", "body": body_b }),
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
        json!({ "title": "Roundtrip Doc", "body": body }),
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

/// Regression: a body with BOM + CRLF + YAML frontmatter must round-trip
/// byte-identically through doc_save -> doc_get(full). Previously
/// `extract_frontmatter`'s reported `after_frontmatter` still carried the
/// line ending following the closing `---` fence (pulldown-cmark's
/// `MetadataBlock` range never includes it), and the handler unconditionally
/// re-added `---{eol}` on reassembly, doubling that line ending right before
/// the first heading.
#[test]
fn doc_save_then_get_full_round_trips_bom_crlf_frontmatter_byte_identical() {
    let (_tmp, dir) = setup_project();
    let body = "\u{FEFF}---\r\ntitle: Foo\r\n---\r\n# Title\r\n\r\nBody.\r\n";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "title": "Foo", "body": body }),
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
        body,
        "doc_save -> doc_get(full) must be byte-identical for BOM+CRLF+frontmatter bodies"
    );
}

/// Regression: a document whose body ends exactly at the closing frontmatter
/// fence (no trailing newline, no content after it at all) must not gain a
/// spurious trailing newline through doc_save -> doc_get(full). Previously
/// the handler unconditionally re-inserted `---{eol}` on reassembly even when
/// `split::SplitDocument` reported no eol had followed the original closing
/// fence, since there was no field to distinguish the two cases.
#[test]
fn doc_save_then_get_full_round_trips_frontmatter_with_no_trailing_eol() {
    let (_tmp, dir) = setup_project();
    let body = "---\ntitle: Foo\n---";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "title": "Foo", "body": body }),
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
        body,
        "doc_get(full) must not invent a trailing newline after the frontmatter fence"
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
    assert_eq!(
        doc_entry["body"].as_str().unwrap(),
        body,
        "doc_list(include_body=true) must not invent a trailing newline either"
    );
}

#[test]
fn doc_get_meta_returns_metadata_without_body() {
    let (_tmp, dir) = setup_project();
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "title": "Meta Doc", "body": "# H\n\nbody\n" }),
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
fn doc_get_fragment_returns_single_fragment() {
    let (_tmp, dir) = setup_project();
    let body = "# Title\n\nIntro.\n\n## Section A\n\nBody A.\n";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "title": "Frag Doc", "body": body }),
    );
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    let get_resp = call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "fragment", "seq": 1 }),
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

// ---------------------------------------------------------------------
// doc_save: update mode (same doc_id) + fragment count changes
// ---------------------------------------------------------------------

#[test]
fn doc_save_update_replaces_fragments_and_preserves_created_at() {
    let (_tmp, dir) = setup_project();
    let body1 = "# Title\n\n## A\n\nBody A.\n";
    let save1 = call(
        &dir,
        "handoff_doc_save",
        json!({ "title": "Update Doc", "body": body1 }),
    );
    let p1 = payload(&save1);
    let doc_id = p1["doc_id"].as_str().unwrap().to_string();
    assert_eq!(p1["fragment_count"], 3); // seq0 + Title + A

    let meta1 = payload(&call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "meta" }),
    ));
    let created_at = meta1["created_at"].as_str().unwrap().to_string();

    // Update with fewer sections -> old fragments beyond new count must be gone.
    let body2 = "# Title\n\nJust a preamble update.\n";
    let save2 = call(
        &dir,
        "handoff_doc_save",
        json!({ "doc_id": &doc_id, "title": "Update Doc", "body": body2 }),
    );
    assert!(!is_error(&save2), "error: {}", payload_text(&save2));
    let p2 = payload(&save2);
    assert_eq!(p2["doc_id"], doc_id);
    assert_eq!(p2["fragment_count"], 2); // seq0 + Title

    let docs_dir = dir.join(".handoff/docs");
    assert!(
        !docs_dir.join(format!("_frag.{doc_id}.2.md")).exists(),
        "stale fragment from the old (longer) body must be deleted"
    );

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
        json!({ "title": "Body Doc", "body": body }),
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
        json!({ "title": "Rust Ownership Guide", "body": "# Rust Ownership\n\nBorrow checker rules explained.\n" }),
    );
    call(
        &dir,
        "handoff_doc_save",
        json!({ "title": "JavaScript Promises", "body": "# JS Promises\n\nAsync await patterns.\n" }),
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

#[test]
fn doc_save_update_unlinks_removed_task_ids() {
    let (_tmp, dir) = setup_project();
    let task_a = create_task(&dir, "Task A");
    let task_b = create_task(&dir, "Task B");

    let save1 = call(
        &dir,
        "handoff_doc_save",
        json!({ "title": "Multi-link Doc", "body": "# H\n\nbody\n", "task_ids": [&task_a, &task_b] }),
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
