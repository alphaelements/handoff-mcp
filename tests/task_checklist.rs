//! Integration tests for `handoff_task_checklist` (action="view"),
//! exercised end-to-end through the JSON-RPC `process_line` entry point —
//! the same path the MCP server runs in production (mirrors
//! `tests/tool_doc_graph.rs`).
//!
//! Phase 1 spec: doc-20260712-191142-602891 §3.1
//! ("タスク×ドキュメント連携チェックシート — 改訂仕様 (v2)").

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
                "project_name": "task-checklist-test"
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

/// Creates a task (optionally with done_criteria) and returns its id, parsed
/// from the handler's plain confirmation string
/// `"Created task {id}: {title} [{status}]"`.
fn create_task(dir: &std::path::Path, title: &str, done_criteria: Value) -> String {
    let resp = call(
        dir,
        "handoff_update_task",
        json!({
            "task": {
                "title": title,
                "status": "todo",
                "schedule": { "estimate_hours": 1.0 },
                "done_criteria": done_criteria,
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

fn save_doc(dir: &std::path::Path, slug: &str, title: &str, body: &str, extra: Value) -> String {
    let mut args = json!({
        "slug": slug,
        "title": title,
        "body": body,
    });
    if let Value::Object(extra_map) = extra {
        for (k, v) in extra_map {
            args[k] = v;
        }
    }
    let resp = call(dir, "handoff_doc_save", args);
    assert!(!is_error(&resp), "doc_save failed: {}", payload_text(&resp));
    payload(&resp)["doc_id"].as_str().unwrap().to_string()
}

// ---------------------------------------------------------------------
// no_linked_docs fast-path
// ---------------------------------------------------------------------

#[test]
fn task_checklist_no_linked_docs_returns_fast_path_response() {
    let (_tmp, dir) = setup_project();
    let task_id = create_task(&dir, "Lonely Task", json!([]));

    let resp = call(
        &dir,
        "handoff_task_checklist",
        json!({ "task_id": task_id }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);

    assert_eq!(p["task_id"], task_id);
    assert_eq!(p["title"], "Lonely Task");
    assert_eq!(p["no_linked_docs"], true);
}

#[test]
fn task_checklist_missing_task_is_error() {
    let (_tmp, dir) = setup_project();
    let resp = call(
        &dir,
        "handoff_task_checklist",
        json!({ "task_id": "t-nope" }),
    );
    assert!(is_error(&resp));
}

// ---------------------------------------------------------------------
// view: full verification coverage + combined_readiness + suggested_actions
// ---------------------------------------------------------------------

#[test]
fn task_checklist_view_aggregates_linked_doc_verification() {
    let (_tmp, dir) = setup_project();
    let task_id = create_task(
        &dir,
        "Doc Query Impl",
        json!([
            { "item": "BM25 search implemented", "checked": true },
            { "item": "Token budget control", "checked": false },
        ]),
    );

    let doc_id = save_doc(
        &dir,
        &unique_slug("checklist-spec"),
        "Doc Management Spec",
        "Preamble.\n\n## Section A\n\nBody A.\n\n## Section B\n\nBody B.\n",
        json!({ "doc_type": "spec", "task_ids": [task_id.clone()] }),
    );

    let gen_resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    assert!(!is_error(&gen_resp), "error: {}", payload_text(&gen_resp));

    // Mark section A (seq=1, "Section A") verified with impl/test refs.
    let set_refs_resp = call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id, "action": "set_refs", "fragment_seq": 1,
            "impl_refs": [{ "path": "src/mcp/handlers/docs.rs", "lines": "42-180" }],
            "test_refs": [{ "path": "tests/doc_save.rs" }],
        }),
    );
    assert!(
        !is_error(&set_refs_resp),
        "error: {}",
        payload_text(&set_refs_resp)
    );
    let check_resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "check", "fragment_seq": 1, "reviewer": "ai" }),
    );
    assert!(
        !is_error(&check_resp),
        "error: {}",
        payload_text(&check_resp)
    );

    let resp = call(
        &dir,
        "handoff_task_checklist",
        json!({ "task_id": task_id }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);

    assert_eq!(p["task_id"], task_id);
    assert_eq!(p["title"], "Doc Query Impl");
    assert_eq!(p["no_linked_docs"], false);

    // done_criteria block
    assert_eq!(p["done_criteria"]["progress"]["checked"], 1);
    assert_eq!(p["done_criteria"]["progress"]["total"], 2);
    let items = p["done_criteria"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["item"], "BM25 search implemented");
    assert_eq!(items[0]["checked"], true);
    assert_eq!(items[1]["checked"], false);

    // verification_coverage block
    let docs = p["verification_coverage"]["documents"].as_array().unwrap();
    assert_eq!(docs.len(), 1);
    let doc = &docs[0];
    assert_eq!(doc["doc_id"], doc_id);
    assert_eq!(doc["doc_type"], "spec");
    let v_items = doc["items"].as_array().unwrap();
    assert_eq!(v_items.len(), 3);

    let section_a = v_items
        .iter()
        .find(|i| i["heading"] == "Section A")
        .expect("Section A item");
    assert_eq!(section_a["status"], "verified");
    assert_eq!(section_a["stale"], false);
    assert_eq!(section_a["visual_state"], "verified");
    assert_eq!(
        section_a["impl_refs"][0]["path"],
        "src/mcp/handlers/docs.rs"
    );

    let section_b = v_items
        .iter()
        .find(|i| i["heading"] == "Section B")
        .expect("Section B item");
    assert_eq!(section_b["status"], "pending");
    assert_eq!(section_b["visual_state"], "untouched");

    let overall = &p["verification_coverage"]["overall"];
    assert_eq!(overall["verified"], 1);
    assert_eq!(overall["total"], 3);

    // combined_readiness block
    assert_eq!(p["combined_readiness"]["done_criteria_met"], false);
    assert_eq!(p["combined_readiness"]["verification_complete"], false);
    assert_eq!(p["combined_readiness"]["ready"], false);
    let blockers = p["combined_readiness"]["blockers"].as_array().unwrap();
    assert!(blockers
        .iter()
        .any(|b| b["type"] == "criteria" && b["item"] == "Token budget control"));
    assert!(blockers
        .iter()
        .any(|b| b["type"] == "verification" && b["heading"] == "Section B"));

    // suggested_actions block
    let actions = p["suggested_actions"].as_array().unwrap();
    assert!(!actions.is_empty());
}

#[test]
fn task_checklist_visual_state_in_progress_when_impl_refs_only() {
    let (_tmp, dir) = setup_project();
    let task_id = create_task(&dir, "In Progress Task", json!([]));
    let doc_id = save_doc(
        &dir,
        &unique_slug("checklist-in-progress"),
        "Design Doc",
        "Preamble.\n\n## Section A\n\nBody A.\n",
        json!({ "doc_type": "design", "task_ids": [task_id.clone()] }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id, "action": "set_refs", "fragment_seq": 1,
            "impl_refs": [{ "path": "src/foo.rs" }],
        }),
    );

    let resp = call(
        &dir,
        "handoff_task_checklist",
        json!({ "task_id": task_id }),
    );
    let p = payload(&resp);
    let v_items = p["verification_coverage"]["documents"][0]["items"]
        .as_array()
        .unwrap();
    let section_a = v_items
        .iter()
        .find(|i| i["heading"] == "Section A")
        .unwrap();
    assert_eq!(section_a["visual_state"], "in_progress");
}

#[test]
fn task_checklist_visual_state_stale_takes_priority() {
    let (_tmp, dir) = setup_project();
    let task_id = create_task(&dir, "Stale Task", json!([]));
    let doc_id = save_doc(
        &dir,
        &unique_slug("checklist-stale"),
        "Spec Doc",
        "Preamble.\n\n## Section A\n\nOriginal body.\n",
        json!({ "doc_type": "spec", "task_ids": [task_id.clone()] }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "check", "fragment_seq": 1 }),
    );

    // Edit the document body so the verified section's content_hash drifts.
    let resp = call(
        &dir,
        "handoff_doc_save",
        json!({
            "doc_id": doc_id,
            "body": "Preamble.\n\n## Section A\n\nChanged body!\n",
        }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));

    let resp = call(
        &dir,
        "handoff_task_checklist",
        json!({ "task_id": task_id }),
    );
    let p = payload(&resp);
    let v_items = p["verification_coverage"]["documents"][0]["items"]
        .as_array()
        .unwrap();
    let section_a = v_items
        .iter()
        .find(|i| i["heading"] == "Section A")
        .unwrap();
    assert_eq!(section_a["stale"], true);
    assert_eq!(section_a["visual_state"], "stale");
}

#[test]
fn task_checklist_linked_doc_without_verification_matrix_blocks_readiness() {
    let (_tmp, dir) = setup_project();
    let task_id = create_task(
        &dir,
        "Ungenerated Verification Task",
        json!([{ "item": "Only criterion", "checked": true }]),
    );
    let doc_id = save_doc(
        &dir,
        &unique_slug("checklist-no-matrix"),
        "Spec Without Matrix",
        "Preamble.\n\n## Section A\n\nBody A.\n",
        json!({ "doc_type": "spec", "task_ids": [task_id.clone()] }),
    );
    // Note: no handoff_doc_verify(action="generate") call — the linked doc
    // has no verification matrix at all.

    let resp = call(
        &dir,
        "handoff_task_checklist",
        json!({ "task_id": task_id }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);

    // done_criteria are all checked, but the missing verification matrix
    // must still block readiness (regression: previously `None => true`
    // silently reported ready=true here).
    assert_eq!(p["combined_readiness"]["done_criteria_met"], true);
    assert_eq!(p["combined_readiness"]["verification_complete"], false);
    assert_eq!(p["combined_readiness"]["ready"], false);

    let blockers = p["combined_readiness"]["blockers"].as_array().unwrap();
    let missing_blocker = blockers
        .iter()
        .find(|b| b["type"] == "verification_missing")
        .expect("expected a verification_missing blocker");
    assert_eq!(missing_blocker["doc_id"], doc_id);

    let actions = p["suggested_actions"].as_array().unwrap();
    assert!(actions.iter().any(|a| {
        let s = a.as_str().unwrap_or_default();
        s.contains("action=\"generate\"") && s.contains(&doc_id)
    }));
}

#[test]
fn task_checklist_default_action_is_view() {
    let (_tmp, dir) = setup_project();
    let task_id = create_task(&dir, "Default Action Task", json!([]));
    let resp = call(
        &dir,
        "handoff_task_checklist",
        json!({ "task_id": task_id }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
}

// ---------------------------------------------------------------------
// generate: preview / append / replace / skip_seqs / fixed_items by doc_type
// ---------------------------------------------------------------------

/// Builds a task with two existing done_criteria and a linked spec doc with
/// two level-2 sections ("Section A", "Section B"), returning `(task_id,
/// doc_id)`.
fn setup_generate_fixture(dir: &std::path::Path) -> (String, String) {
    let task_id = create_task(
        dir,
        "Generate Fixture Task",
        json!([{ "item": "Pre-existing criterion", "checked": true }]),
    );
    let doc_id = save_doc(
        dir,
        &unique_slug("generate-spec"),
        "Generate Spec",
        "Preamble.\n\n## Section A\n\nBody A.\n\n## Section B\n\nBody B.\n",
        json!({ "doc_type": "spec", "task_ids": [task_id.clone()] }),
    );
    (task_id, doc_id)
}

#[test]
fn task_checklist_generate_preview_returns_items_without_modifying_task() {
    let (_tmp, dir) = setup_project();
    let (task_id, doc_id) = setup_generate_fixture(&dir);

    let resp = call(
        &dir,
        "handoff_task_checklist",
        json!({ "task_id": task_id, "action": "generate", "doc_id": doc_id, "mode": "preview" }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);

    assert_eq!(p["task_id"], task_id);
    assert_eq!(p["applied"], false);
    let items = p["generated_criteria"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["item"], "[spec§1] Section A");
    assert_eq!(items[0]["fragment_seq"], 1);
    assert_eq!(items[1]["item"], "[spec§2] Section B");
    assert_eq!(items[1]["fragment_seq"], 2);
    assert_eq!(p["skipped_seqs"], json!([0]));
    let fixed_items = p["fixed_items"].as_array().unwrap();
    assert!(fixed_items
        .iter()
        .any(|i| i == "仕様書の全セクションがカバーされていることを確認"));
    assert!(fixed_items
        .iter()
        .any(|i| i == "仕様変更があれば doc_save で更新済み"));

    // preview must not modify the task's done_criteria.
    let view_resp = call(
        &dir,
        "handoff_task_checklist",
        json!({ "task_id": task_id }),
    );
    let view = payload(&view_resp);
    assert_eq!(view["done_criteria"]["progress"]["total"], 1);
}

#[test]
fn task_checklist_generate_append_adds_to_existing_done_criteria() {
    let (_tmp, dir) = setup_project();
    let (task_id, doc_id) = setup_generate_fixture(&dir);

    let resp = call(
        &dir,
        "handoff_task_checklist",
        json!({ "task_id": task_id, "action": "generate", "doc_id": doc_id, "mode": "append" }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    assert_eq!(p["applied"], true);

    let view_resp = call(
        &dir,
        "handoff_task_checklist",
        json!({ "task_id": task_id }),
    );
    let view = payload(&view_resp);
    let items = view["done_criteria"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["item"], "Pre-existing criterion");
    assert_eq!(items[0]["checked"], true);
    assert_eq!(items[1]["item"], "[spec§1] Section A");
    assert_eq!(items[1]["checked"], false);
    assert_eq!(items[2]["item"], "[spec§2] Section B");
}

#[test]
fn task_checklist_generate_replace_overwrites_done_criteria() {
    let (_tmp, dir) = setup_project();
    let (task_id, doc_id) = setup_generate_fixture(&dir);

    let resp = call(
        &dir,
        "handoff_task_checklist",
        json!({ "task_id": task_id, "action": "generate", "doc_id": doc_id, "mode": "replace" }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    assert_eq!(p["applied"], true);

    let view_resp = call(
        &dir,
        "handoff_task_checklist",
        json!({ "task_id": task_id }),
    );
    let view = payload(&view_resp);
    let items = view["done_criteria"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["item"], "[spec§1] Section A");
    assert_eq!(items[1]["item"], "[spec§2] Section B");
    // Pre-existing criterion must be gone.
    assert!(!items.iter().any(|i| i["item"] == "Pre-existing criterion"));
}

#[test]
fn task_checklist_generate_skip_seqs_excludes_specified_sections() {
    let (_tmp, dir) = setup_project();
    let (task_id, doc_id) = setup_generate_fixture(&dir);

    let resp = call(
        &dir,
        "handoff_task_checklist",
        json!({
            "task_id": task_id, "action": "generate", "doc_id": doc_id,
            "mode": "preview", "skip_seqs": [2],
        }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);

    let items = p["generated_criteria"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["item"], "[spec§1] Section A");
    assert_eq!(p["skipped_seqs"], json!([0, 2]));
}

#[test]
fn task_checklist_generate_fixed_items_differ_by_doc_type() {
    let (_tmp, dir) = setup_project();
    let task_id = create_task(&dir, "Design Fixture Task", json!([]));
    let design_doc_id = save_doc(
        &dir,
        &unique_slug("generate-design"),
        "Generate Design",
        "Preamble.\n\n## Section A\n\nBody A.\n",
        json!({ "doc_type": "design", "task_ids": [task_id.clone()] }),
    );
    let resp = call(
        &dir,
        "handoff_task_checklist",
        json!({ "task_id": task_id, "action": "generate", "doc_id": design_doc_id, "mode": "preview" }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    assert_eq!(
        p["fixed_items"],
        json!(["設計と実装の乖離がないことを確認"])
    );

    let task_id2 = create_task(&dir, "Guide Fixture Task", json!([]));
    let guide_doc_id = save_doc(
        &dir,
        &unique_slug("generate-guide"),
        "Generate Guide",
        "Preamble.\n\n## Section A\n\nBody A.\n",
        json!({ "doc_type": "guide", "task_ids": [task_id2.clone()] }),
    );
    let resp2 = call(
        &dir,
        "handoff_task_checklist",
        json!({ "task_id": task_id2, "action": "generate", "doc_id": guide_doc_id, "mode": "preview" }),
    );
    assert!(!is_error(&resp2), "error: {}", payload_text(&resp2));
    let p2 = payload(&resp2);
    assert_eq!(p2["fixed_items"], json!([]));
}

#[test]
fn task_checklist_generate_defaults_to_preview_mode() {
    let (_tmp, dir) = setup_project();
    let (task_id, doc_id) = setup_generate_fixture(&dir);

    let resp = call(
        &dir,
        "handoff_task_checklist",
        json!({ "task_id": task_id, "action": "generate", "doc_id": doc_id }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    assert_eq!(p["applied"], false);
}

#[test]
fn task_checklist_generate_without_doc_id_auto_selects_spec_link() {
    let (_tmp, dir) = setup_project();
    let (task_id, doc_id) = setup_generate_fixture(&dir);

    let resp = call(
        &dir,
        "handoff_task_checklist",
        json!({ "task_id": task_id, "action": "generate", "mode": "preview" }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    let items = p["generated_criteria"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    let _ = doc_id;
}

#[test]
fn task_checklist_appears_in_tools_list() {
    let (_tmp, dir) = setup_project();
    let req = json!({
        "jsonrpc": "2.0", "id": 5,
        "method": "tools/list",
        "params": {}
    });
    let _ = &dir;
    let resp = send(&req.to_string()).unwrap();
    let tools = resp["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == "handoff_task_checklist"));
}
