//! Integration tests for `handoff_doc_req_status` — cross-document
//! requirements progress aggregation (requirements-traceability P1 §4.1,
//! `.handoff/docs/_doc.req-traceability-mcp-plan.md`), exercised end-to-end
//! through the JSON-RPC `process_line` entry point — the same path the MCP
//! server runs in production (mirrors `tests/doc_verify.rs`).

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
                "project_name": "doc-req-status-test"
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
/// (which auto-derives a `stable_id` — see `doc_verify_add_item_assigns_derived_stable_id`
/// in `tests/doc_verify.rs`), then sets `priority`/`dev_stage` on it via
/// `sub_item_index` (the sub_item is always index 0, being the only one
/// added to fragment_seq 1).
fn make_req_doc(
    dir: &std::path::Path,
    slug: &str,
    tags: &[&str],
    description: &str,
    priority: Option<&str>,
    dev_stage: Option<&str>,
    with_impl_ref: bool,
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
            "tags": tags,
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

    if with_impl_ref {
        let resp = call(
            dir,
            "handoff_doc_verify",
            json!({
                "doc_id": doc_id,
                "action": "set_refs",
                "fragment_seq": 1,
                "sub_item_index": 0,
                "impl_refs": [{ "path": "src/x.rs" }],
            }),
        );
        assert!(!is_error(&resp), "set_refs failed: {}", payload_text(&resp));
    }

    doc_id
}

// ---------------------------------------------------------------------
// no filters
// ---------------------------------------------------------------------

#[test]
fn req_status_no_filters_aggregates_across_all_docs() {
    let (_tmp, dir) = setup_project();
    make_req_doc(
        &dir,
        &unique_slug("req-c01"),
        &["requirements"],
        "矩形外形",
        Some("P0"),
        Some("implemented"),
        true,
    );
    make_req_doc(
        &dir,
        &unique_slug("req-c07"),
        &["requirements"],
        "差動ペア間隔",
        Some("P1"),
        Some("tested"),
        true,
    );

    let resp = call(&dir, "handoff_doc_req_status", json!({}));
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);

    assert_eq!(p["total"], 2);
    assert_eq!(p["by_status"]["implemented"], 1);
    assert_eq!(p["by_status"]["tested"], 1);
    assert_eq!(p["by_priority"]["P0"]["total"], 1);
    assert_eq!(p["by_priority"]["P1"]["total"], 1);
    assert_eq!(p["by_category"]["C01"]["total"], 1);
    assert_eq!(p["by_category"]["C07"]["total"], 1);
    assert!(p["coverage"]["impl_pct"].as_f64().unwrap() > 0.0);
}

// ---------------------------------------------------------------------
// tags filter
// ---------------------------------------------------------------------

#[test]
fn req_status_tags_filter_restricts_to_matching_documents() {
    let (_tmp, dir) = setup_project();
    make_req_doc(
        &dir,
        &unique_slug("req-c01"),
        &["requirements", "pcb"],
        "矩形外形",
        Some("P0"),
        Some("implemented"),
        true,
    );
    make_req_doc(
        &dir,
        &unique_slug("notes-doc"),
        &["misc"],
        "無関係項目",
        Some("P2"),
        Some("not_started"),
        false,
    );

    let resp = call(
        &dir,
        "handoff_doc_req_status",
        json!({ "tags": ["requirements"] }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);

    assert_eq!(
        p["total"], 1,
        "only the doc tagged 'requirements' should be counted: {p}"
    );
    assert_eq!(p["by_category"]["C01"]["total"], 1);
    assert!(p["by_category"].get("misc").is_none());
}

// ---------------------------------------------------------------------
// priority filter
// ---------------------------------------------------------------------

#[test]
fn req_status_priority_filter_restricts_to_matching_sub_items() {
    let (_tmp, dir) = setup_project();
    make_req_doc(
        &dir,
        &unique_slug("req-c01"),
        &["requirements"],
        "矩形外形",
        Some("P0"),
        Some("implemented"),
        true,
    );
    make_req_doc(
        &dir,
        &unique_slug("req-c07"),
        &["requirements"],
        "差動ペア間隔",
        Some("P1"),
        Some("tested"),
        true,
    );

    let resp = call(&dir, "handoff_doc_req_status", json!({ "priority": "P0" }));
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);

    assert_eq!(p["total"], 1, "only the P0 sub_item should count: {p}");
    assert_eq!(p["by_priority"]["P0"]["total"], 1);
    assert!(p["by_priority"].get("P1").is_none());
}

// ---------------------------------------------------------------------
// category filter
// ---------------------------------------------------------------------

#[test]
fn req_status_category_filter_restricts_by_stable_id_prefix() {
    let (_tmp, dir) = setup_project();
    make_req_doc(
        &dir,
        &unique_slug("req-c01"),
        &["requirements"],
        "矩形外形",
        Some("P0"),
        Some("implemented"),
        true,
    );
    make_req_doc(
        &dir,
        &unique_slug("req-c07"),
        &["requirements"],
        "差動ペア間隔",
        Some("P1"),
        Some("tested"),
        true,
    );

    let resp = call(&dir, "handoff_doc_req_status", json!({ "category": "C01" }));
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);

    assert_eq!(p["total"], 1, "only C01 sub_items should count: {p}");
    assert_eq!(p["by_category"]["C01"]["total"], 1);
    assert!(p["by_category"].get("C07").is_none());
}

// ---------------------------------------------------------------------
// side effect: _requirements_summary.json cache
// ---------------------------------------------------------------------

#[test]
fn req_status_updates_requirements_summary_cache_file_unfiltered() {
    let (_tmp, dir) = setup_project();
    make_req_doc(
        &dir,
        &unique_slug("req-c01"),
        &["requirements"],
        "矩形外形",
        Some("P0"),
        Some("implemented"),
        true,
    );
    make_req_doc(
        &dir,
        &unique_slug("req-c07"),
        &["requirements"],
        "差動ペア間隔",
        Some("P1"),
        Some("tested"),
        true,
    );

    // Filtered call — the cache file must still reflect the FULL unfiltered
    // aggregate (spec §4.1 "注: この副作用更新はフィルタなしの全体集計で行う").
    let resp = call(&dir, "handoff_doc_req_status", json!({ "priority": "P0" }));
    assert!(!is_error(&resp), "{}", payload_text(&resp));

    let cache_path = dir.join(".handoff/docs/_requirements_summary.json");
    assert!(cache_path.exists(), "cache file should be written");
    let content = std::fs::read_to_string(&cache_path).unwrap();
    let cached: Value = serde_json::from_str(&content).unwrap();
    assert_eq!(
        cached["total"], 2,
        "cache must hold the unfiltered total, not the filtered response's total: {cached}"
    );
}

// ---------------------------------------------------------------------
// items array in _requirements_summary.json
// ---------------------------------------------------------------------

#[test]
fn req_status_cache_file_contains_items_array_with_correct_fields() {
    let (_tmp, dir) = setup_project();
    make_req_doc(
        &dir,
        &unique_slug("req-items-c01"),
        &["requirements"],
        "矩形外形",
        Some("P0"),
        Some("implemented"),
        true,
    );
    make_req_doc(
        &dir,
        &unique_slug("req-items-c07"),
        &["requirements"],
        "差動ペア間隔",
        Some("P1"),
        Some("tested"),
        false,
    );

    let resp = call(&dir, "handoff_doc_req_status", json!({}));
    assert!(!is_error(&resp), "{}", payload_text(&resp));

    let cache_path = dir.join(".handoff/docs/_requirements_summary.json");
    let content = std::fs::read_to_string(&cache_path).unwrap();
    let cached: Value = serde_json::from_str(&content).unwrap();

    let items = cached["items"]
        .as_array()
        .expect("items should be an array");
    assert_eq!(items.len(), 2, "should have 2 items: {cached}");

    for item in items {
        assert!(
            item["stable_id"].is_string(),
            "each item must have stable_id"
        );
        assert!(item["title"].is_string(), "each item must have title");
        assert!(item["doc_id"].is_string(), "each item must have doc_id");
        assert!(item["doc_slug"].is_string(), "each item must have doc_slug");
        assert!(
            item["verification_status"].is_string(),
            "each item must have verification_status"
        );
    }

    let implemented: Vec<&Value> = items
        .iter()
        .filter(|i| i["dev_stage"].as_str() == Some("implemented"))
        .collect();
    assert_eq!(
        implemented.len(),
        1,
        "one item should have dev_stage=implemented"
    );
    assert_eq!(implemented[0]["priority"].as_str(), Some("P0"));

    let tested: Vec<&Value> = items
        .iter()
        .filter(|i| i["dev_stage"].as_str() == Some("tested"))
        .collect();
    assert_eq!(tested.len(), 1, "one item should have dev_stage=tested");
    assert_eq!(tested[0]["priority"].as_str(), Some("P1"));
}

#[test]
fn req_status_response_also_contains_items_array() {
    let (_tmp, dir) = setup_project();
    make_req_doc(
        &dir,
        &unique_slug("req-resp-c01"),
        &["requirements"],
        "矩形外形",
        Some("P0"),
        Some("implemented"),
        true,
    );

    let resp = call(&dir, "handoff_doc_req_status", json!({}));
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);

    let items = p["items"]
        .as_array()
        .expect("response should have items array");
    assert_eq!(items.len(), 1);
    assert!(items[0]["stable_id"].is_string());
    assert_eq!(items[0]["dev_stage"].as_str(), Some("implemented"));
}

#[test]
fn req_status_filtered_response_items_are_filtered() {
    let (_tmp, dir) = setup_project();
    make_req_doc(
        &dir,
        &unique_slug("req-filt-c01"),
        &["requirements"],
        "矩形外形",
        Some("P0"),
        Some("implemented"),
        true,
    );
    make_req_doc(
        &dir,
        &unique_slug("req-filt-c07"),
        &["requirements"],
        "差動ペア間隔",
        Some("P1"),
        Some("tested"),
        true,
    );

    let resp = call(&dir, "handoff_doc_req_status", json!({ "priority": "P0" }));
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);

    let items = p["items"]
        .as_array()
        .expect("filtered response should have items");
    assert_eq!(items.len(), 1, "should only contain P0 item: {p}");
    assert_eq!(items[0]["priority"].as_str(), Some("P0"));

    // Cache file should still have ALL items unfiltered
    let cache_path = dir.join(".handoff/docs/_requirements_summary.json");
    let content = std::fs::read_to_string(&cache_path).unwrap();
    let cached: Value = serde_json::from_str(&content).unwrap();
    let cached_items = cached["items"].as_array().unwrap();
    assert_eq!(
        cached_items.len(),
        2,
        "cache should have all items unfiltered"
    );
}

// ---------------------------------------------------------------------
// performance: many documents
// ---------------------------------------------------------------------

#[test]
fn req_status_responds_within_one_second_for_29_documents() {
    let (_tmp, dir) = setup_project();
    for i in 0..29 {
        make_req_doc(
            &dir,
            &unique_slug(&format!("req-c{i:02}")),
            &["requirements"],
            &format!("要件 {i}"),
            Some("P0"),
            Some("implemented"),
            true,
        );
    }

    let start = std::time::Instant::now();
    let resp = call(&dir, "handoff_doc_req_status", json!({}));
    let elapsed = start.elapsed();

    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);
    assert_eq!(p["total"], 29);
    assert!(
        elapsed.as_secs_f64() < 1.0,
        "expected < 1s for 29 docs, got {elapsed:?}"
    );
}

// ---------------------------------------------------------------------
// P-M4 write discipline (wiki/240-performance-design.md §4): a read-only
// handoff_doc_req_status call must not rewrite _requirements_summary.json
// when the cache is already fresh (t370.4).
// ---------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn req_status_does_not_rewrite_cache_file_when_already_fresh() {
    use std::os::unix::fs::MetadataExt;

    let (_tmp, dir) = setup_project();
    make_req_doc(
        &dir,
        &unique_slug("req-fresh"),
        &["requirements"],
        "要件",
        Some("P0"),
        Some("implemented"),
        true,
    );

    let resp = call(&dir, "handoff_doc_req_status", json!({}));
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let cache_path = dir.join(".handoff/docs/_requirements_summary.json");
    assert!(cache_path.exists(), "cache file should be written");
    let ino_before = std::fs::metadata(&cache_path).unwrap().ino();

    // Nothing changed in between (no doc/task writes) -> the fingerprint
    // recorded in the cache is still fresh -> this read-only call must not
    // rewrite the file.
    let resp = call(&dir, "handoff_doc_req_status", json!({}));
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let ino_after = std::fs::metadata(&cache_path).unwrap().ino();

    assert_eq!(
        ino_before, ino_after,
        "a fresh cache must not be rewritten by a read-only handoff_doc_req_status call"
    );
}
