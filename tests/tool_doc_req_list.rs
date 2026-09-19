//! Integration tests for `handoff_doc_req_list` — individual-requirement
//! list across every document's verification matrix, with filter/sort/
//! pagination (requirements-traceability P1 §4.2,
//! `.handoff/docs/_doc.req-traceability-mcp-plan.md`), exercised end-to-end
//! through the JSON-RPC `process_line` entry point — the same path the MCP
//! server runs in production (mirrors `tests/tool_doc_req_status.rs` and
//! `tests/doc_verify.rs`).

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
                "project_name": "doc-req-list-test"
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

/// Saves a document, generates its verification matrix, adds one sub_item
/// (which auto-derives a `stable_id` — see
/// `doc_verify_add_item_assigns_derived_stable_id` in `tests/doc_verify.rs`),
/// then optionally sets `priority`/`dev_stage`/`impl_refs`+`test_refs` on it
/// via `sub_item_index` (the sub_item is always index 0, being the only one
/// added to fragment_seq 1).
fn make_req_doc(
    dir: &std::path::Path,
    slug: &str,
    description: &str,
    priority: Option<&str>,
    dev_stage: Option<&str>,
    with_test_ref: bool,
) -> String {
    let body = "Intro.\n\n## 1. 要件\n\nBody.\n";
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
    let doc_id = payload(&resp)["doc_id"].as_str().unwrap().to_string();

    let resp = call(
        dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    assert!(!is_error(&resp), "generate failed: {}", payload_text(&resp));

    let resp = call(
        dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "add_item",
            "fragment_seq": 1,
            "description": description,
        }),
    );
    assert!(!is_error(&resp), "add_item failed: {}", payload_text(&resp));

    if let Some(p) = priority {
        let resp = call(
            dir,
            "handoff_doc_verify",
            json!({
                "doc_id": doc_id,
                "action": "set_priority",
                "fragment_seq": 1,
                "sub_item_index": 0,
                "priority": p,
            }),
        );
        assert!(
            !is_error(&resp),
            "set_priority failed: {}",
            payload_text(&resp)
        );
    }

    if let Some(stage) = dev_stage {
        let resp = call(
            dir,
            "handoff_doc_verify",
            json!({
                "doc_id": doc_id,
                "action": "set_dev_stage",
                "fragment_seq": 1,
                "sub_item_index": 0,
                "dev_stage": stage,
            }),
        );
        assert!(
            !is_error(&resp),
            "set_dev_stage failed: {}",
            payload_text(&resp)
        );
    }

    if with_test_ref {
        let resp = call(
            dir,
            "handoff_doc_verify",
            json!({
                "doc_id": doc_id,
                "action": "set_refs",
                "fragment_seq": 1,
                "sub_item_index": 0,
                "impl_refs": [{ "path": "src/x.rs" }],
                "test_refs": [{ "path": "tests/x.rs" }],
            }),
        );
        assert!(!is_error(&resp), "set_refs failed: {}", payload_text(&resp));
    }

    doc_id
}

// ---------------------------------------------------------------------
// dispatch smoke test — proves the mod.rs match arm actually routes here
// ---------------------------------------------------------------------

#[test]
fn req_list_dispatches_through_process_line_and_returns_expected_shape() {
    let (_tmp, dir) = setup_project();
    make_req_doc(
        &dir,
        &unique_slug("req-c01"),
        "矩形外形",
        Some("P0"),
        Some("implemented"),
        true,
    );

    let resp = call(&dir, "handoff_doc_req_list", json!({}));
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);

    assert_eq!(p["total"], 1);
    let items = p["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert_eq!(item["stable_id"], "C01-1");
    assert_eq!(item["title"], "矩形外形");
    assert_eq!(item["priority"], "P0");
    assert_eq!(item["dev_stage"], "implemented");
    assert!(item.get("doc_id").is_some());
    assert!(item.get("doc_slug").is_some());
    assert!(item.get("sub_item_index").is_some());
    assert_eq!(p["limit"], 100);
    assert_eq!(p["offset"], 0);
}

// ---------------------------------------------------------------------
// filters
// ---------------------------------------------------------------------

#[test]
fn req_list_priority_filter_restricts_to_matching_sub_items() {
    let (_tmp, dir) = setup_project();
    make_req_doc(
        &dir,
        &unique_slug("req-c01"),
        "矩形外形",
        Some("P0"),
        Some("implemented"),
        true,
    );
    make_req_doc(
        &dir,
        &unique_slug("req-c07"),
        "差動ペア間隔",
        Some("P1"),
        Some("tested"),
        true,
    );

    let resp = call(&dir, "handoff_doc_req_list", json!({ "priority": "P0" }));
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);

    assert_eq!(p["total"], 1, "only the P0 sub_item should count: {p}");
    assert_eq!(p["items"][0]["priority"], "P0");
}

#[test]
fn req_list_dev_stage_filter_restricts_to_matching_sub_items() {
    let (_tmp, dir) = setup_project();
    make_req_doc(
        &dir,
        &unique_slug("req-c01"),
        "矩形外形",
        Some("P0"),
        Some("implemented"),
        true,
    );
    make_req_doc(
        &dir,
        &unique_slug("req-c07"),
        "差動ペア間隔",
        Some("P1"),
        Some("tested"),
        true,
    );

    let resp = call(
        &dir,
        "handoff_doc_req_list",
        json!({ "dev_stage": "tested" }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);

    assert_eq!(
        p["total"], 1,
        "only the 'tested' sub_item should count: {p}"
    );
    assert_eq!(p["items"][0]["dev_stage"], "tested");
}

#[test]
fn req_list_category_filter_restricts_by_stable_id_prefix() {
    let (_tmp, dir) = setup_project();
    make_req_doc(
        &dir,
        &unique_slug("req-c01"),
        "矩形外形",
        Some("P0"),
        Some("implemented"),
        true,
    );
    make_req_doc(
        &dir,
        &unique_slug("req-c07"),
        "差動ペア間隔",
        Some("P1"),
        Some("tested"),
        true,
    );

    let resp = call(&dir, "handoff_doc_req_list", json!({ "category": "C07" }));
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);

    assert_eq!(p["total"], 1, "only C07 sub_items should count: {p}");
    assert_eq!(p["items"][0]["stable_id"], "C07-1");
}

#[test]
fn req_list_has_tests_false_filter_excludes_items_with_test_refs() {
    let (_tmp, dir) = setup_project();
    make_req_doc(
        &dir,
        &unique_slug("req-c01"),
        "矩形外形",
        Some("P0"),
        Some("implemented"),
        true,
    );
    make_req_doc(
        &dir,
        &unique_slug("req-c07"),
        "差動ペア間隔",
        Some("P1"),
        Some("tested"),
        false,
    );

    let resp = call(&dir, "handoff_doc_req_list", json!({ "has_tests": false }));
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);

    assert_eq!(p["total"], 1, "{p}");
    assert!(p["items"][0]["test_refs"].as_array().unwrap().is_empty());
}

// ---------------------------------------------------------------------
// sort + order
// ---------------------------------------------------------------------

#[test]
fn req_list_sort_by_stable_id_desc_orders_results() {
    let (_tmp, dir) = setup_project();
    make_req_doc(
        &dir,
        &unique_slug("req-c01"),
        "矩形外形",
        Some("P0"),
        Some("implemented"),
        true,
    );
    make_req_doc(
        &dir,
        &unique_slug("req-c07"),
        "差動ペア間隔",
        Some("P1"),
        Some("tested"),
        true,
    );

    let resp = call(
        &dir,
        "handoff_doc_req_list",
        json!({ "sort": "stable_id", "order": "desc" }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);

    let ids: Vec<&str> = p["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["stable_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["C07-1", "C01-1"]);
}

// ---------------------------------------------------------------------
// pagination
// ---------------------------------------------------------------------

#[test]
fn req_list_limit_and_offset_paginate_after_sort() {
    let (_tmp, dir) = setup_project();
    make_req_doc(
        &dir,
        &unique_slug("req-c01"),
        "矩形外形",
        Some("P0"),
        Some("implemented"),
        true,
    );
    make_req_doc(
        &dir,
        &unique_slug("req-c07"),
        "差動ペア間隔",
        Some("P1"),
        Some("tested"),
        true,
    );

    let resp = call(
        &dir,
        "handoff_doc_req_list",
        json!({ "sort": "stable_id", "order": "asc", "limit": 1, "offset": 1 }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);

    assert_eq!(p["total"], 2, "total reflects pre-pagination count: {p}");
    assert_eq!(p["items"].as_array().unwrap().len(), 1);
    assert_eq!(p["items"][0]["stable_id"], "C07-1");
    assert_eq!(p["limit"], 1);
    assert_eq!(p["offset"], 1);
}

// ---------------------------------------------------------------------
// empty result
// ---------------------------------------------------------------------

#[test]
fn req_list_empty_result_returns_items_empty_and_total_zero() {
    let (_tmp, dir) = setup_project();
    make_req_doc(
        &dir,
        &unique_slug("req-c01"),
        "矩形外形",
        Some("P0"),
        Some("implemented"),
        true,
    );

    let resp = call(&dir, "handoff_doc_req_list", json!({ "priority": "P3" }));
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);

    assert_eq!(p["total"], 0);
    assert_eq!(p["items"].as_array().unwrap().len(), 0);
}
