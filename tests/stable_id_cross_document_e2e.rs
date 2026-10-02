//! Real-binary E2E coverage for M0-b (wiki/220-vmodel-integration-design.md
//! §4.2, FR-105): `stable_id`s are only guaranteed unique *within* one
//! document — nothing ever prevented two independent documents from minting
//! or hand-authoring the same id. These tests exercise the corpus-wide
//! collision report end to end through the JSON-RPC `process_line` entry
//! point (the same path the MCP server runs in production, mirrors
//! `tests/doc_verify.rs` / `tests/tool_doc_req_import.rs`):
//! - `handoff_doc_verify(action="add_item")` and `handoff_doc_req_import`
//!   warn (never refuse) when the id they are about to assign already
//!   exists in a different document.
//! - `handoff_update_task(requirement_ids=[...])` treats a `stable_id` that
//!   resolves to more than one document as ambiguous and links to neither.

use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};

fn send(input: &str) -> Option<Value> {
    handoff_mcp::mcp::protocol::process_line(input)
        .map(|result| serde_json::from_str(&result).expect("response should be valid JSON"))
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
                "project_name": "stable-id-collision-test"
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

fn save_doc(dir: &std::path::Path, slug: &str, body: &str) -> String {
    let resp = call(
        dir,
        "handoff_doc_save",
        json!({
            "slug": slug,
            "title": format!("Doc {slug}"),
            "body": body,
            "doc_type": "spec",
            "tags": ["requirements"],
        }),
    );
    assert!(!is_error(&resp), "doc_save failed: {}", payload_text(&resp));
    payload(&resp)["doc_id"].as_str().unwrap().to_string()
}

/// Both documents use the same `req-c01-...` category prefix (so
/// `derive_stable_id` derives the same `C01` category for each) and the same
/// `FR-001`-prefixed leaf heading text, so both independently derive the
/// exact same `stable_id` (`C01-FR-001`) — the collision this whole test
/// file is about.
const SECTION_BODY: &str = "Intro.\n\n## Section A\n\nBody A.\n\n## Section B\n\nBody B.\n";

#[test]
fn doc_verify_add_item_warns_when_stable_id_collides_with_another_document() {
    let (_tmp, dir) = setup_project();

    let doc_a = save_doc(&dir, &unique_slug("req-c01-alpha"), SECTION_BODY);
    let doc_b = save_doc(&dir, &unique_slug("req-c01-beta"), SECTION_BODY);

    for doc_id in [&doc_a, &doc_b] {
        let resp = call(
            &dir,
            "handoff_doc_verify",
            json!({ "doc_id": doc_id, "action": "generate" }),
        );
        assert!(!is_error(&resp), "generate failed: {}", payload_text(&resp));
    }

    // First document mints C01-FR-001 with no collision yet.
    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_a,
            "action": "add_item",
            "fragment_seq": 1,
            "description": "FR-001 collision target",
        }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);
    assert!(
        p["warnings"].as_array().unwrap().is_empty(),
        "first assignment of C01-FR-001 must not warn: {p}"
    );

    // Second, independent document derives the identical id.
    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_b,
            "action": "add_item",
            "fragment_seq": 1,
            "description": "FR-001 collision target",
        }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);
    let warnings: Vec<String> = p["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w.as_str().unwrap().to_string())
        .collect();
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("C01-FR-001") && w.contains(&doc_a)),
        "expected a cross-document collision warning naming doc_a, got: {warnings:?}"
    );

    // Not rejected: doc_b's SubItem was still created.
    let status_resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_b, "include_items": true }),
    );
    let status = payload(&status_resp);
    let sub_ids: Vec<&str> = status["items"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|i| i["sub_items"].as_array().unwrap())
        .map(|s| s["stable_id"].as_str().unwrap())
        .collect();
    assert!(sub_ids.contains(&"C01-FR-001"), "sub_ids={sub_ids:?}");
}

const REQ_TREE_WITH_FR_PREFIX: &str = "\
Intro.

## 2. 要件ツリー

### 2.1 Group

#### FR-001 collision via req_import
";

#[test]
fn doc_req_import_warns_when_stable_id_collides_with_another_document() {
    let (_tmp, dir) = setup_project();

    let doc_a = save_doc(&dir, &unique_slug("req-c01-gamma"), REQ_TREE_WITH_FR_PREFIX);
    let doc_b = save_doc(&dir, &unique_slug("req-c01-delta"), REQ_TREE_WITH_FR_PREFIX);

    let resp = call(
        &dir,
        "handoff_doc_req_import",
        json!({ "doc_id": doc_a, "dry_run": false, "priority_source": "none" }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);
    assert_eq!(p["created"], 1, "{p}");
    assert!(
        p["preview"][0]["warning"].is_null(),
        "first import must not warn about a cross-document collision: {p}"
    );

    let resp = call(
        &dir,
        "handoff_doc_req_import",
        json!({ "doc_id": doc_b, "dry_run": false, "priority_source": "none" }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);
    assert_eq!(p["created"], 1, "{p}");
    let warning = p["preview"][0]["warning"]
        .as_str()
        .expect("second import of the same derived id must warn");
    assert!(
        warning.contains("C01-FR-001") && warning.contains(&doc_a),
        "expected a cross-document collision warning naming doc_a, got: {warning}"
    );
}

#[test]
fn update_task_requirement_ids_reports_ambiguous_when_two_documents_share_a_stable_id() {
    let (_tmp, dir) = setup_project();

    let doc_a = save_doc(&dir, &unique_slug("req-c01-epsilon"), SECTION_BODY);
    let doc_b = save_doc(&dir, &unique_slug("req-c01-zeta"), SECTION_BODY);
    for doc_id in [&doc_a, &doc_b] {
        let resp = call(
            &dir,
            "handoff_doc_verify",
            json!({ "doc_id": doc_id, "action": "generate" }),
        );
        assert!(!is_error(&resp), "generate failed: {}", payload_text(&resp));
    }
    for doc_id in [&doc_a, &doc_b] {
        let resp = call(
            &dir,
            "handoff_doc_verify",
            json!({
                "doc_id": doc_id,
                "action": "add_item",
                "fragment_seq": 1,
                "description": "FR-001 ambiguous target",
            }),
        );
        assert!(!is_error(&resp), "{}", payload_text(&resp));
    }

    // update_task(requirement_ids=["C01-FR-001"]) must not silently link to
    // whichever document happens to be scanned first — it must report the
    // id as ambiguous and link to neither.
    let resp = call(
        &dir,
        "handoff_update_task",
        json!({
            "task": {
                "title": "Link an ambiguous requirement",
                "status": "todo",
                "requirement_ids": ["C01-FR-001"],
            }
        }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let response_text = payload_text(&resp);
    assert!(
        response_text.contains("ambiguous") && response_text.contains("C01-FR-001"),
        "expected an ambiguous-stable_id warning, got: {response_text}"
    );

    // Neither document's SubItem must have gained the reverse task_ids link.
    let status_a = payload(&call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_a, "include_items": true }),
    ));
    let status_b = payload(&call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_b, "include_items": true }),
    ));
    for status in [&status_a, &status_b] {
        let task_ids: Vec<&Value> = status["items"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|i| i["sub_items"].as_array().unwrap())
            .flat_map(|s| s["task_ids"].as_array().unwrap())
            .collect();
        assert!(task_ids.is_empty(), "task_ids must stay empty: {status}");
    }
}
