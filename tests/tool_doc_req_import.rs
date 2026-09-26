//! Integration test for `handoff_doc_req_import` — bulk-generates SubItems
//! from a document's Markdown requirement-tree heading hierarchy
//! (requirements-traceability P1 §4.3-4.4,
//! `.handoff/docs/_doc.req-traceability-mcp-plan.md`), exercised end-to-end
//! through the JSON-RPC `process_line` entry point — the same path the MCP
//! server runs in production (mirrors `tests/tool_doc_req_list.rs`).

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
                "project_name": "doc-req-import-test"
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

const REQ_TREE_BODY: &str = "\
Intro.

## 2. 要件ツリー

### 2.1 基板外形

#### 2.1.1 外形形状定義

##### 2.1.1.1 矩形外形

##### 2.1.1.2 円形外形

## 3. ギャップ分析

| 要件 | 優先度 | 備考 |
|---|---|---|
| 矩形外形 | P0 | 必須 |
| 円形外形 | P2 | 任意 |
";

#[test]
fn req_import_dispatches_through_process_line_and_returns_expected_shape() {
    let (_tmp, dir) = setup_project();

    let resp = call(
        &dir,
        "handoff_doc_save",
        json!({
            "slug": "req-c01-board-setup",
            "title": "Doc req-c01",
            "body": REQ_TREE_BODY,
            "doc_type": "spec",
            "tags": ["requirements"],
        }),
    );
    assert!(!is_error(&resp), "doc_save failed: {}", payload_text(&resp));
    let doc_id = payload(&resp)["doc_id"].as_str().unwrap().to_string();

    // dry_run defaults to true: preview only, no write.
    let resp = call(&dir, "handoff_doc_req_import", json!({ "doc_id": doc_id }));
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);
    assert_eq!(p["would_create"], 2, "{p}");
    assert_eq!(p["would_update"], 0, "{p}");
    let preview = p["preview"].as_array().unwrap();
    assert_eq!(preview.len(), 2);
    assert!(preview.iter().any(|e| e["priority"] == "P0"));
    assert!(preview.iter().any(|e| e["priority"] == "P2"));

    // Confirm dry_run truly wrote nothing.
    let resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id }),
    );
    assert!(is_error(&resp), "no verification matrix should exist yet");

    // dry_run=false actually creates the SubItems.
    let resp = call(
        &dir,
        "handoff_doc_req_import",
        json!({ "doc_id": doc_id, "dry_run": false }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);
    assert_eq!(p["created"], 2, "{p}");

    let resp = call(&dir, "handoff_doc_req_list", json!({}));
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let list = payload(&resp);
    assert_eq!(list["total"], 2, "{list}");
    let ids: Vec<&str> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["stable_id"].as_str().unwrap())
        .collect();
    assert!(ids.iter().all(|id| id.starts_with("C01")), "{ids:?}");
}

#[test]
fn req_import_missing_doc_returns_error() {
    let (_tmp, dir) = setup_project();
    let resp = call(
        &dir,
        "handoff_doc_req_import",
        json!({ "doc_id": "doc-does-not-exist" }),
    );
    assert!(is_error(&resp));
}

/// Regression test for a MAJOR bug found in review-rework round 1: the
/// preview phase scans *all* `verification.items` (via
/// `v.items.iter().flat_map(|i| i.sub_items.iter())`) when deciding
/// stable_id/fuzzy-match actions, but the apply phase only wrote into
/// `v.items[0]`. If the matching sub_item actually lived in `v.items[1..]`
/// (e.g. one attached to a specific section via `handoff_doc_verify`'s
/// `add_item` action with a `fragment_seq`), the preview correctly reported
/// action="update"/"match" and counted it in `would_update`, but `apply`
/// silently found nothing in `items[0]` to update — no SubItem was actually
/// touched, while the response still claimed `updated: N`. This sets up a
/// verification matrix with multiple `VerificationItem`s (one per document
/// section, via `generate`), attaches a sub_item to the *second* section's
/// item (`v.items[1]`, not `v.items[0]`) whose description exactly matches
/// an import candidate, then asserts that `dry_run=false` actually updates
/// that sub_item in place — not silently skips it while still reporting
/// `updated: 1`.
#[test]
fn req_import_updates_sub_item_living_in_a_non_first_verification_item() {
    let (_tmp, dir) = setup_project();

    let resp = call(
        &dir,
        "handoff_doc_save",
        json!({
            "slug": "req-c01-board-setup",
            "title": "Doc req-c01",
            "body": REQ_TREE_BODY,
            "doc_type": "spec",
            "tags": ["requirements"],
        }),
    );
    assert!(!is_error(&resp), "doc_save failed: {}", payload_text(&resp));
    let doc_id = payload(&resp)["doc_id"].as_str().unwrap().to_string();

    // `generate` creates one VerificationItem per `##` section: seq 0
    // (preamble/"Intro."), seq 1 (要件ツリー), seq 2 (ギャップ分析) -> three
    // items, so v.items[0] is the preamble item, not the requirement-tree
    // item.
    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));

    let resp = call(
        &dir,
        "handoff_doc_verify_status",
        json!({ "doc_id": doc_id, "include_items": true }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let status = payload(&resp);
    let items = status["items"].as_array().unwrap();
    assert!(items.len() >= 2, "expected multiple items: {status}");
    // Confirm the requirement-tree section is not items[0] (it must be a
    // later fragment_seq for this test to actually exercise the bug).
    let req_tree_seq = items
        .iter()
        .find(|it| {
            it["heading"]
                .as_str()
                .unwrap_or_default()
                .contains("要件ツリー")
        })
        .and_then(|it| it["fragment_seq"].as_u64())
        .expect("要件ツリー section present");
    assert!(
        req_tree_seq >= 1,
        "要件ツリー must not be items[0]: {status}"
    );

    // Attach a sub_item to that later item whose description exactly
    // matches an import candidate's leaf heading text ("矩形外形" is the
    // ##### leaf under 2.1.1 外形形状定義).
    let resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "add_item",
            "fragment_seq": req_tree_seq,
            "description": "矩形外形",
        }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));

    // Import should recognize this sub_item via fuzzy/description match and
    // report an update/match — not a create.
    let resp = call(&dir, "handoff_doc_req_import", json!({ "doc_id": doc_id }));
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let preview_resp = payload(&resp);
    let preview = preview_resp["preview"].as_array().unwrap();
    let matched_entry = preview
        .iter()
        .find(|e| e["title"].as_str().unwrap_or_default().contains("矩形外形"))
        .expect("矩形外形 candidate present in preview");
    assert_ne!(
        matched_entry["action"], "create",
        "the pre-existing sub_item must be recognized, not treated as new: {preview_resp}"
    );

    // Now actually apply — the pre-existing sub_item in v.items[1+] must be
    // the one that gets updated; the total count of sub_items across the
    // whole matrix must not grow beyond the 2 leaf candidates (no duplicate
    // created in items[0]).
    let resp = call(
        &dir,
        "handoff_doc_req_import",
        json!({ "doc_id": doc_id, "dry_run": false }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));

    let resp = call(&dir, "handoff_doc_req_list", json!({}));
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let list = payload(&resp);
    assert_eq!(
        list["total"], 2,
        "must not duplicate the pre-existing sub_item into items[0]: {list}"
    );

    // The pre-existing sub_item (originally in v.items[1+], attached via
    // add_item) must now carry the priority derived from the gap table
    // (P0 for 矩形外形), proving the *same* SubItem object was mutated in
    // place rather than a duplicate created elsewhere.
    let updated = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["title"].as_str().unwrap_or_default().contains("矩形外形"))
        .expect("矩形外形 sub_item present");
    assert_eq!(
        updated["priority"], "P0",
        "the pre-existing sub_item (in a non-items[0] VerificationItem) must be the one updated: {updated}"
    );
}
