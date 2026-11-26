//! E2E integration tests for the frontmatter migration (t123) exercised
//! through the JSON-RPC `process_line` entry point — the same path the MCP
//! server runs in production.
//!
//! Covers:
//! - Legacy JSON+MD pair → frontmatter single-file automatic migration via
//!   MCP tools (`doc_get`, `doc_list`, `doc_save` update)
//! - Markdown hand-edit resilience: `doc_get(format=section)` after direct
//!   out-of-band `.md` file modification
//! - Migration + verify matrix interaction
//! - `read_all_docs` transparent migration through `doc_list`

use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};

fn send(input: &str) -> Option<Value> {
    let result = handoff_mcp::mcp::protocol::process_line(input)?;
    Some(serde_json::from_str(&result).expect("response should be valid JSON"))
}

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
                "project_name": "migration-e2e"
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

/// Writes a legacy JSON+MD pair on disk (the pre-migration 2-file format),
/// bypassing MCP tools entirely — simulates what existing projects have
/// before upgrading to the frontmatter format.
fn write_legacy_doc(
    dir: &std::path::Path,
    slug: &str,
    doc_id: &str,
    title: &str,
    body: &str,
    extra_json: Option<Value>,
) {
    let docs_dir = dir.join(".handoff/docs");
    std::fs::create_dir_all(&docs_dir).unwrap();

    let mut meta = json!({
        "version": 2,
        "id": doc_id,
        "slug": slug,
        "title": title,
        "doc_type": "spec",
        "tags": ["legacy"],
        "scope_paths": [],
        "parent_id": null,
        "children": [],
        "related": [],
        "auto_inject": "auto",
        "task_ids": [],
        "source": { "origin": "authored", "canonical_hash": "" },
        "has_bom": false,
        "line_ending": "lf",
        "sections": [],
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-02T00:00:00Z",
        "content_hash": "old-hash-will-be-recomputed",
    });
    if let Some(extra) = extra_json {
        if let (Some(base), Some(ext)) = (meta.as_object_mut(), extra.as_object()) {
            for (k, v) in ext {
                base.insert(k.clone(), v.clone());
            }
        }
    }

    std::fs::write(
        docs_dir.join(format!("_doc.{slug}.json")),
        serde_json::to_vec_pretty(&meta).unwrap(),
    )
    .unwrap();
    std::fs::write(docs_dir.join(format!("_doc.{slug}.md")), body).unwrap();
}

// =====================================================================
// 1. Legacy → New format migration via MCP tools
// =====================================================================

#[test]
fn legacy_doc_migrated_transparently_by_doc_get_full() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("legacy-get-full");
    let doc_id = format!("doc-legacy-{slug}");
    let body = "# Legacy Title\n\nIntro.\n\n## Section A\n\nBody A.\n";
    write_legacy_doc(&dir, &slug, &doc_id, "Legacy Title", body, None);

    let resp = call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "full" }),
    );
    assert!(!is_error(&resp), "doc_get failed: {}", payload_text(&resp));
    let p = payload(&resp);
    assert_eq!(p["id"], doc_id);
    assert_eq!(p["title"], "Legacy Title");
    assert_eq!(p["body"].as_str().unwrap(), body);
    assert_eq!(p["doc_type"], "spec");

    // On-disk: JSON sidecar gone, MD has frontmatter
    let docs_dir = dir.join(".handoff/docs");
    assert!(
        !docs_dir.join(format!("_doc.{slug}.json")).exists(),
        "JSON sidecar must be deleted after migration"
    );
    let md_content = std::fs::read_to_string(docs_dir.join(format!("_doc.{slug}.md"))).unwrap();
    assert!(
        md_content.starts_with("---\n"),
        "migrated MD must start with YAML frontmatter fence"
    );
}

#[test]
fn legacy_doc_migrated_transparently_by_doc_get_meta() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("legacy-get-meta");
    let doc_id = format!("doc-legacy-{slug}");
    let body = "# Meta Legacy\n\nContent.\n";
    write_legacy_doc(&dir, &slug, &doc_id, "Meta Legacy", body, None);

    let resp = call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "meta" }),
    );
    assert!(
        !is_error(&resp),
        "doc_get meta failed: {}",
        payload_text(&resp)
    );
    let p = payload(&resp);
    assert_eq!(p["id"], doc_id);
    assert_eq!(p["title"], "Meta Legacy");
    assert_eq!(p["tags"][0], "legacy");
    assert!(p.get("body").is_none(), "meta format must not include body");
}

#[test]
fn legacy_doc_migrated_transparently_by_doc_get_section() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("legacy-get-section");
    let doc_id = format!("doc-legacy-{slug}");
    let body = "Preamble.\n\n## Section A\n\nBody A.\n\n## Section B\n\nBody B.\n";
    write_legacy_doc(&dir, &slug, &doc_id, "Section Legacy", body, None);

    // seq 0 = preamble, seq 1 = Section A, seq 2 = Section B
    let resp = call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "section", "seq": 1 }),
    );
    assert!(
        !is_error(&resp),
        "doc_get section failed: {}",
        payload_text(&resp)
    );
    let p = payload(&resp);
    assert_eq!(p["seq"], 1);
    assert_eq!(p["heading"], "Section A");
    assert!(p["body"].as_str().unwrap().contains("Body A."));
}

#[test]
fn legacy_doc_appears_in_doc_list() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("legacy-list");
    let doc_id = format!("doc-legacy-{slug}");
    let body = "# Listed\n\nContent.\n";
    write_legacy_doc(&dir, &slug, &doc_id, "Listed Legacy", body, None);

    let resp = call(&dir, "handoff_doc_list", json!({}));
    assert!(!is_error(&resp), "doc_list failed: {}", payload_text(&resp));
    let p = payload(&resp);
    let docs = p["documents"].as_array().unwrap();
    let found = docs.iter().find(|d| d["id"] == doc_id);
    assert!(
        found.is_some(),
        "legacy doc must appear in doc_list after migration"
    );
    assert_eq!(found.unwrap()["title"], "Listed Legacy");
}

#[test]
fn legacy_doc_update_via_doc_save_preserves_created_at() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("legacy-update");
    let doc_id = format!("doc-legacy-{slug}");
    let body = "# Old Body\n\nOriginal.\n";
    write_legacy_doc(&dir, &slug, &doc_id, "Update Legacy", body, None);

    // First read triggers migration
    let get1 = call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "full" }),
    );
    assert!(!is_error(&get1));
    let created_at = payload(&get1)["created_at"].as_str().unwrap().to_string();

    // Update via doc_save
    let new_body = "# New Body\n\nUpdated content.\n\n## Added Section\n\nNew.\n";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "doc_id": &doc_id, "body": new_body }),
    );
    assert!(
        !is_error(&save_resp),
        "doc_save failed: {}",
        payload_text(&save_resp)
    );

    let get2 = call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "full" }),
    );
    assert!(!is_error(&get2));
    let p = payload(&get2);
    assert_eq!(p["body"].as_str().unwrap(), new_body);
    assert_eq!(
        p["created_at"].as_str().unwrap(),
        created_at,
        "created_at must be preserved through migration + update"
    );
}

#[test]
fn legacy_doc_with_tags_and_task_ids_migrates_all_metadata() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("legacy-rich-meta");
    let doc_id = format!("doc-legacy-{slug}");
    let body = "# Rich\n\nBody.\n";
    write_legacy_doc(
        &dir,
        &slug,
        &doc_id,
        "Rich Metadata",
        body,
        Some(json!({
            "tags": ["architecture", "review"],
            "scope_paths": ["src/mcp/", "tests/"],
        })),
    );

    let resp = call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "meta" }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    let tags: Vec<&str> = p["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(tags.contains(&"architecture"));
    assert!(tags.contains(&"review"));
    let scopes: Vec<&str> = p["scope_paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(scopes.contains(&"src/mcp/"));
    assert!(scopes.contains(&"tests/"));
}

#[test]
fn legacy_doc_migration_is_idempotent_on_reread() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("legacy-idempotent");
    let doc_id = format!("doc-legacy-{slug}");
    let body = "# Idem\n\nBody.\n";
    write_legacy_doc(&dir, &slug, &doc_id, "Idempotent", body, None);

    let resp1 = call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "full" }),
    );
    assert!(!is_error(&resp1));
    let p1 = payload(&resp1);

    let resp2 = call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "full" }),
    );
    assert!(!is_error(&resp2));
    let p2 = payload(&resp2);

    assert_eq!(p1["id"], p2["id"]);
    assert_eq!(p1["title"], p2["title"]);
    assert_eq!(p1["body"], p2["body"]);
    assert_eq!(p1["content_hash"], p2["content_hash"]);
}

#[test]
fn mixed_legacy_and_new_docs_all_appear_in_doc_list() {
    let (_tmp, dir) = setup_project();

    // New-format doc via doc_save
    let new_slug = unique_slug("mixed-new");
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({
            "slug": &new_slug,
            "title": "New Format",
            "body": "# New\n\nBody.\n",
        }),
    );
    assert!(!is_error(&save_resp));
    let new_doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    // Legacy-format doc on disk
    let legacy_slug = unique_slug("mixed-legacy");
    let legacy_doc_id = format!("doc-legacy-{legacy_slug}");
    write_legacy_doc(
        &dir,
        &legacy_slug,
        &legacy_doc_id,
        "Legacy Format",
        "# Legacy\n\nBody.\n",
        None,
    );

    let list_resp = call(&dir, "handoff_doc_list", json!({}));
    assert!(!is_error(&list_resp), "error: {}", payload_text(&list_resp));
    let docs = payload(&list_resp)["documents"].as_array().unwrap().clone();
    assert!(docs.iter().any(|d| d["id"] == new_doc_id));
    assert!(docs.iter().any(|d| d["id"] == legacy_doc_id.as_str()));
}

// =====================================================================
// 2. Markdown hand-edit resilience
// =====================================================================

#[test]
fn hand_edit_body_then_doc_get_section_reflects_new_content() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("hand-edit-section");
    let body = "Preamble.\n\n## Section A\n\nOriginal A.\n\n## Section B\n\nOriginal B.\n";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": &slug, "title": "Hand Edit Test", "body": body }),
    );
    assert!(!is_error(&save_resp));
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    // Directly edit the .md file, replacing Section A's content
    let md_path = dir.join(".handoff/docs").join(format!("_doc.{slug}.md"));
    let on_disk = std::fs::read_to_string(&md_path).unwrap();
    let edited = on_disk.replace("Original A.", "Hand-edited A content.");
    std::fs::write(&md_path, &edited).unwrap();

    // doc_get(section) must reflect the hand-edited content
    let resp = call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "section", "seq": 1 }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    assert_eq!(p["heading"], "Section A");
    assert!(
        p["body"]
            .as_str()
            .unwrap()
            .contains("Hand-edited A content."),
        "section body must reflect the hand-edited file content"
    );
}

#[test]
fn hand_edit_add_section_then_doc_get_full_shows_new_section_count() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("hand-edit-add");
    let body = "Preamble.\n\n## Section A\n\nBody A.\n";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": &slug, "title": "Add Section Test", "body": body }),
    );
    assert!(!is_error(&save_resp));
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();
    let original_sections = payload(&save_resp)["section_count"].as_u64().unwrap();
    assert_eq!(original_sections, 2); // preamble + Section A

    // Hand-edit: append a new section to the .md file
    let md_path = dir.join(".handoff/docs").join(format!("_doc.{slug}.md"));
    let on_disk = std::fs::read_to_string(&md_path).unwrap();
    let edited = format!("{on_disk}\n## Section B\n\nHand-added B.\n");
    std::fs::write(&md_path, &edited).unwrap();

    let get_resp = call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "full" }),
    );
    assert!(!is_error(&get_resp));
    let p = payload(&get_resp);
    let sections = p["sections"].as_array().unwrap();
    assert_eq!(
        sections.len(),
        3,
        "sections must be recomputed to include hand-added section"
    );
    assert!(sections.iter().any(|s| s["heading"] == "Section B"));
}

#[test]
fn hand_edit_remove_section_then_doc_get_section_adjusts() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("hand-edit-remove");
    let body = "Preamble.\n\n## Section A\n\nBody A.\n\n## Section B\n\nBody B.\n";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": &slug, "title": "Remove Section Test", "body": body }),
    );
    assert!(!is_error(&save_resp));
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    // Hand-edit: remove Section A entirely
    let md_path = dir.join(".handoff/docs").join(format!("_doc.{slug}.md"));
    let on_disk = std::fs::read_to_string(&md_path).unwrap();
    let edited = on_disk.replace("## Section A\n\nBody A.\n\n", "");
    std::fs::write(&md_path, &edited).unwrap();

    let get_resp = call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "full" }),
    );
    assert!(!is_error(&get_resp));
    let p = payload(&get_resp);
    let sections = p["sections"].as_array().unwrap();
    assert_eq!(
        sections.len(),
        2,
        "sections must reflect removal (preamble + Section B only)"
    );

    // seq 1 is now Section B (renumbered)
    let sec_resp = call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "section", "seq": 1 }),
    );
    assert!(!is_error(&sec_resp));
    assert_eq!(payload(&sec_resp)["heading"], "Section B");
}

#[test]
fn hand_edit_changes_content_hash_detected_by_doc_reassemble() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("hand-edit-drift");
    let body = "Preamble.\n\n## Section A\n\nBody A.\n";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": &slug, "title": "Drift Test", "body": body }),
    );
    assert!(!is_error(&save_resp));
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    // Hand-edit the body
    let md_path = dir.join(".handoff/docs").join(format!("_doc.{slug}.md"));
    let on_disk = std::fs::read_to_string(&md_path).unwrap();
    let edited = on_disk.replace("Body A.", "Edited body.");
    std::fs::write(&md_path, &edited).unwrap();

    let reassemble_resp = call(&dir, "handoff_doc_reassemble", json!({ "doc_id": &doc_id }));
    assert!(
        !is_error(&reassemble_resp),
        "error: {}",
        payload_text(&reassemble_resp)
    );
    let p = payload(&reassemble_resp);
    assert_eq!(
        p["drifted"], true,
        "reassemble must detect drift after hand-edit"
    );
}

#[test]
fn hand_edit_then_doc_update_section_works_on_recomputed_sections() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("hand-edit-then-update");
    let body = "Preamble.\n\n## Section A\n\nOriginal A.\n\n## Section B\n\nOriginal B.\n";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": &slug, "title": "Update After Edit", "body": body }),
    );
    assert!(!is_error(&save_resp));
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    // Hand-edit: modify Section A
    let md_path = dir.join(".handoff/docs").join(format!("_doc.{slug}.md"));
    let on_disk = std::fs::read_to_string(&md_path).unwrap();
    let edited = on_disk.replace("Original A.", "Hand-edited A.");
    std::fs::write(&md_path, &edited).unwrap();

    // Use doc_update_section on seq 2 (Section B) — must work with
    // recomputed sections from the hand-edited body
    let update_resp = call(
        &dir,
        "handoff_doc_update_section",
        json!({
            "doc_id": &doc_id,
            "seq": 2,
            "new_content": "## Section B\n\nReplaced via API.\n",
        }),
    );
    assert!(
        !is_error(&update_resp),
        "doc_update_section must work after hand-edit: {}",
        payload_text(&update_resp)
    );

    // Verify both sections
    let get_a = call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "section", "seq": 1 }),
    );
    assert!(payload(&get_a)["body"]
        .as_str()
        .unwrap()
        .contains("Hand-edited A."));

    let get_b = call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "section", "seq": 2 }),
    );
    assert!(payload(&get_b)["body"]
        .as_str()
        .unwrap()
        .contains("Replaced via API."));
}

// =====================================================================
// 3. Migration + verification matrix interaction
// =====================================================================

#[test]
fn legacy_doc_can_generate_verify_matrix_after_migration() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("legacy-verify");
    let doc_id = format!("doc-legacy-{slug}");
    let body = "Preamble.\n\n## Spec A\n\nRequirement A.\n\n## Spec B\n\nRequirement B.\n";
    write_legacy_doc(&dir, &slug, &doc_id, "Verifiable Legacy", body, None);

    // Generate verification matrix on the legacy doc (triggers migration first)
    let gen_resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": &doc_id, "action": "generate" }),
    );
    assert!(
        !is_error(&gen_resp),
        "verify generate must work on migrated legacy doc: {}",
        payload_text(&gen_resp)
    );
    let p = payload(&gen_resp);
    assert_eq!(p["total"], 3); // preamble + Spec A + Spec B
    assert_eq!(p["verification_status"], "pending");

    // Check a section
    let check_resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": &doc_id,
            "action": "check",
            "fragment_seq": 1,
            "reviewer": "ai",
        }),
    );
    assert!(!is_error(&check_resp));
    assert_eq!(payload(&check_resp)["checked"], 1);
}

// =====================================================================
// 4. Hand-edit + verify staleness detection
// =====================================================================

#[test]
fn hand_edit_after_verify_marks_affected_section_stale() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("hand-edit-stale");
    let body = "Preamble.\n\n## Section A\n\nBody A.\n\n## Section B\n\nBody B.\n";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": &slug, "title": "Stale After Edit", "body": body }),
    );
    assert!(!is_error(&save_resp));
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    // Generate matrix and verify Section A
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": &doc_id, "action": "generate" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": &doc_id, "action": "check", "fragment_seq": 1 }),
    );

    // Hand-edit Section A
    let md_path = dir.join(".handoff/docs").join(format!("_doc.{slug}.md"));
    let on_disk = std::fs::read_to_string(&md_path).unwrap();
    let edited = on_disk.replace("Body A.", "Body A CHANGED.");
    std::fs::write(&md_path, &edited).unwrap();

    // Need to re-save so the content_hash updates and staleness is detected
    let edited_body = std::fs::read_to_string(&md_path).unwrap();
    // Extract the body after frontmatter
    let body_start = edited_body.find("---\n").unwrap() + 4;
    let body_rest = &edited_body[body_start..];
    let body_start2 = body_rest.find("---\n").unwrap() + 4;
    let actual_body = &body_rest[body_start2..];
    let save2 = call(
        &dir,
        "handoff_doc_save",
        json!({ "doc_id": &doc_id, "body": actual_body }),
    );
    assert!(!is_error(&save2));

    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": &doc_id, "include_items": true }),
    );
    assert!(!is_error(&status_resp));
    let p = payload(&status_resp);
    let items = p["items"].as_array().unwrap();
    let sec_a = items.iter().find(|i| i["fragment_seq"] == 1).unwrap();
    assert_eq!(
        sec_a["stale"], true,
        "Section A must be marked stale after content changed"
    );
}

// =====================================================================
// 5. Edge cases
// =====================================================================

#[test]
fn hand_edit_frontmatter_directly_does_not_break_doc_get() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("edit-frontmatter");
    let body = "# Title\n\nBody.\n";
    let save_resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "slug": &slug, "title": "FM Edit", "body": body }),
    );
    assert!(!is_error(&save_resp));
    let doc_id = payload(&save_resp)["doc_id"].as_str().unwrap().to_string();

    // Hand-edit the frontmatter: add a custom key
    let md_path = dir.join(".handoff/docs").join(format!("_doc.{slug}.md"));
    let on_disk = std::fs::read_to_string(&md_path).unwrap();
    let edited = on_disk.replacen("---\n", "---\ncustom_key: custom_value\n", 1);
    std::fs::write(&md_path, &edited).unwrap();

    let get_resp = call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "full" }),
    );
    assert!(
        !is_error(&get_resp),
        "doc_get must tolerate hand-edited frontmatter: {}",
        payload_text(&get_resp)
    );
    let p = payload(&get_resp);
    assert_eq!(p["id"], doc_id);
    assert_eq!(p["body"].as_str().unwrap(), body);
}

#[test]
fn legacy_doc_with_old_sections_field_migrates_correctly() {
    let (_tmp, dir) = setup_project();
    let slug = unique_slug("legacy-sections");
    let doc_id = format!("doc-legacy-{slug}");
    let body = "Preamble.\n\n## Old A\n\nBody A.\n";
    // Legacy format had `sections` with full SectionIndex entries — these
    // are cleared by the migration path and recomputed fresh from the body.
    write_legacy_doc(
        &dir,
        &slug,
        &doc_id,
        "With Sections",
        body,
        Some(json!({
            "sections": [
                { "seq": 0, "heading": "", "level": 0, "byte_offset": 0, "byte_length": 11, "content_hash": "stale" },
                { "seq": 1, "heading": "Old A", "level": 2, "byte_offset": 11, "byte_length": 19, "content_hash": "stale" },
            ],
        })),
    );

    let resp = call(
        &dir,
        "handoff_doc_get",
        json!({ "doc_id": &doc_id, "format": "full" }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    let sections = p["sections"].as_array().unwrap();
    assert_eq!(
        sections.len(),
        2,
        "sections must be recomputed fresh (not carried from legacy sidecar)"
    );
    assert_eq!(sections[1]["heading"], "Old A");
    // content_hash must be recomputed, not the stale value from the sidecar
    assert_ne!(sections[1]["content_hash"], "stale");
}
