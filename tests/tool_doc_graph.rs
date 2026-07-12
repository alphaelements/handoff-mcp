//! Integration tests for the document graph tools (`handoff_doc_graph` /
//! `handoff_doc_trace`), exercised end-to-end through the JSON-RPC
//! `process_line` entry point — the same path the MCP server runs in
//! production (mirrors `tests/tool_docs.rs`).

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
                "project_name": "doc-graph-test"
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
// handoff_doc_graph
// ---------------------------------------------------------------------

#[test]
fn doc_graph_explicit_parent_child_and_related_edges() {
    let (_tmp, dir) = setup_project();
    let parent_id = save_doc(
        &dir,
        &unique_slug("graph-parent"),
        "Parent",
        "# P\n\nbody\n",
        json!({}),
    );
    let child_id = save_doc(
        &dir,
        &unique_slug("graph-child"),
        "Child",
        "# C\n\nbody\n",
        json!({ "parent_id": parent_id }),
    );
    let related_target_id = save_doc(
        &dir,
        &unique_slug("graph-related"),
        "Related",
        "# R\n\nbody\n",
        json!({}),
    );
    // Update child to add a `related` link.
    let resp = call(
        &dir,
        "handoff_doc_save",
        json!({
            "doc_id": child_id,
            "body": "# C\n\nbody\n",
            "related": [{ "id": related_target_id, "rel": "implements" }],
        }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));

    let resp = call(&dir, "handoff_doc_graph", json!({}));
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);

    let nodes = p["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 3);
    let parent_node = nodes.iter().find(|n| n["id"] == parent_id).unwrap();
    assert_eq!(parent_node["title"], "Parent");
    assert_eq!(parent_node["doc_type"], "note");
    assert!(parent_node.get("section_count").is_some());

    let edges = p["edges"].as_array().unwrap();
    assert!(edges.iter().any(|e| e["type"] == "parent_child"
        && e["from"] == parent_id
        && e["to"] == child_id
        && e["direction"] == "down"));
    assert!(edges.iter().any(|e| e["type"] == "implements"
        && e["from"] == child_id
        && e["to"] == related_target_id
        && e["direction"] == "forward"));

    let layers = &p["layers"];
    assert!(layers["note"].as_array().unwrap().len() == 3);
}

#[test]
fn doc_graph_implicit_shared_task_edge() {
    let (_tmp, dir) = setup_project();
    let task_resp = call(
        &dir,
        "handoff_update_task",
        json!({ "task": { "title": "Shared Task", "status": "todo", "schedule": { "estimate_hours": 1.0 } } }),
    );
    let task_id = payload_text(&task_resp)
        .strip_prefix("Created task ")
        .and_then(|rest| rest.split(':').next())
        .unwrap()
        .to_string();

    let a_id = save_doc(
        &dir,
        &unique_slug("shared-task-a"),
        "A",
        "# A\n\nbody\n",
        json!({ "task_ids": [task_id.clone()] }),
    );
    let b_id = save_doc(
        &dir,
        &unique_slug("shared-task-b"),
        "B",
        "# B\n\nbody\n",
        json!({ "task_ids": [task_id.clone()] }),
    );

    let resp = call(
        &dir,
        "handoff_doc_graph",
        json!({ "include_implicit": true }),
    );
    let p = payload(&resp);
    let edges = p["edges"].as_array().unwrap();
    let shared_task_edge = edges
        .iter()
        .find(|e| e["type"] == "shared_task" && e["from"] == a_id && e["to"] == b_id)
        .expect("shared_task edge must appear");
    assert_eq!(shared_task_edge["task_ids"], json!([task_id]));
}

#[test]
fn doc_graph_implicit_shared_scope_edge() {
    let (_tmp, dir) = setup_project();
    let a_id = save_doc(
        &dir,
        &unique_slug("shared-scope-a"),
        "A",
        "# A\n\nbody\n",
        json!({ "scope_paths": ["src/mcp/"] }),
    );
    let b_id = save_doc(
        &dir,
        &unique_slug("shared-scope-b"),
        "B",
        "# B\n\nbody\n",
        json!({ "scope_paths": ["src/mcp/", "src/storage/"] }),
    );

    let resp = call(&dir, "handoff_doc_graph", json!({}));
    let p = payload(&resp);
    let edges = p["edges"].as_array().unwrap();
    assert!(edges
        .iter()
        .any(|e| e["type"] == "shared_scope" && e["from"] == a_id && e["to"] == b_id));
}

#[test]
fn doc_graph_include_implicit_false_suppresses_implicit_edges() {
    let (_tmp, dir) = setup_project();
    save_doc(
        &dir,
        &unique_slug("no-implicit-a"),
        "A",
        "# A\n\nbody\n",
        json!({ "scope_paths": ["src/mcp/"] }),
    );
    save_doc(
        &dir,
        &unique_slug("no-implicit-b"),
        "B",
        "# B\n\nbody\n",
        json!({ "scope_paths": ["src/mcp/"] }),
    );

    let resp = call(
        &dir,
        "handoff_doc_graph",
        json!({ "include_implicit": false }),
    );
    let p = payload(&resp);
    let edges = p["edges"].as_array().unwrap();
    assert!(!edges.iter().any(|e| e["type"] == "shared_scope"));
}

#[test]
fn doc_graph_include_verification_adds_progress() {
    let (_tmp, dir) = setup_project();
    let doc_id = save_doc(
        &dir,
        &unique_slug("verify-graph"),
        "Verify Me",
        "# Title\n\n## Section A\n\nBody A.\n\n## Section B\n\nBody B.\n",
        json!({}),
    );
    let gen_resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    assert!(!is_error(&gen_resp), "error: {}", payload_text(&gen_resp));

    // Mark one fragment verified.
    let check_resp = call(
        &dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "check", "fragment_seq": 0, "reviewer": "ai" }),
    );
    assert!(
        !is_error(&check_resp),
        "error: {}",
        payload_text(&check_resp)
    );

    let resp = call(
        &dir,
        "handoff_doc_graph",
        json!({ "include_verification": true }),
    );
    let p = payload(&resp);
    let node = p["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == doc_id)
        .unwrap();
    let progress = &node["verification_progress"];
    assert!(progress["total"].as_u64().unwrap() >= 1);
    assert_eq!(progress["verified"], 1);
}

#[test]
fn doc_graph_layers_group_by_doc_type() {
    let (_tmp, dir) = setup_project();
    save_doc(
        &dir,
        &unique_slug("layer-spec"),
        "Spec Doc",
        "# S\n\nbody\n",
        json!({ "doc_type": "spec" }),
    );
    save_doc(
        &dir,
        &unique_slug("layer-design"),
        "Design Doc",
        "# D\n\nbody\n",
        json!({ "doc_type": "design" }),
    );

    let resp = call(&dir, "handoff_doc_graph", json!({}));
    let p = payload(&resp);
    assert_eq!(p["layers"]["spec"].as_array().unwrap().len(), 1);
    assert_eq!(p["layers"]["design"].as_array().unwrap().len(), 1);
}

// ---------------------------------------------------------------------
// handoff_doc_trace
// ---------------------------------------------------------------------

#[test]
fn doc_trace_direction_up_walks_parent_chain() {
    let (_tmp, dir) = setup_project();
    let root_id = save_doc(
        &dir,
        &unique_slug("trace-root"),
        "Root",
        "# R\n\nbody\n",
        json!({}),
    );
    let mid_id = save_doc(
        &dir,
        &unique_slug("trace-mid"),
        "Mid",
        "# M\n\nbody\n",
        json!({ "parent_id": root_id }),
    );
    let leaf_id = save_doc(
        &dir,
        &unique_slug("trace-leaf"),
        "Leaf",
        "# L\n\nbody\n",
        json!({ "parent_id": mid_id }),
    );

    let resp = call(
        &dir,
        "handoff_doc_trace",
        json!({ "doc_id": leaf_id, "direction": "up" }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    let chain = p["chain"].as_array().unwrap();
    let ids: Vec<&str> = chain.iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        vec![root_id.as_str(), mid_id.as_str(), leaf_id.as_str()]
    );
}

#[test]
fn doc_trace_direction_down_walks_children_dfs() {
    let (_tmp, dir) = setup_project();
    let root_id = save_doc(
        &dir,
        &unique_slug("trace-down-root"),
        "Root",
        "# R\n\nbody\n",
        json!({}),
    );
    let child_id = save_doc(
        &dir,
        &unique_slug("trace-down-child"),
        "Child",
        "# C\n\nbody\n",
        json!({ "parent_id": root_id }),
    );
    let grandchild_id = save_doc(
        &dir,
        &unique_slug("trace-down-grandchild"),
        "Grandchild",
        "# G\n\nbody\n",
        json!({ "parent_id": child_id }),
    );

    let resp = call(
        &dir,
        "handoff_doc_trace",
        json!({ "doc_id": root_id, "direction": "down" }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    let chain = p["chain"].as_array().unwrap();
    let ids: Vec<&str> = chain.iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        vec![root_id.as_str(), child_id.as_str(), grandchild_id.as_str()]
    );
}

#[test]
fn doc_trace_direction_both_merges_up_and_down() {
    let (_tmp, dir) = setup_project();
    let root_id = save_doc(
        &dir,
        &unique_slug("trace-both-root"),
        "Root",
        "# R\n\nbody\n",
        json!({}),
    );
    let mid_id = save_doc(
        &dir,
        &unique_slug("trace-both-mid"),
        "Mid",
        "# M\n\nbody\n",
        json!({ "parent_id": root_id }),
    );
    let leaf_id = save_doc(
        &dir,
        &unique_slug("trace-both-leaf"),
        "Leaf",
        "# L\n\nbody\n",
        json!({ "parent_id": mid_id }),
    );

    let resp = call(
        &dir,
        "handoff_doc_trace",
        json!({ "doc_id": mid_id, "direction": "both" }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    let chain = p["chain"].as_array().unwrap();
    let ids: Vec<&str> = chain.iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        vec![root_id.as_str(), mid_id.as_str(), leaf_id.as_str()]
    );
}

#[test]
fn doc_trace_default_direction_is_both() {
    let (_tmp, dir) = setup_project();
    let root_id = save_doc(
        &dir,
        &unique_slug("trace-default-root"),
        "Root",
        "# R\n\nbody\n",
        json!({}),
    );
    let leaf_id = save_doc(
        &dir,
        &unique_slug("trace-default-leaf"),
        "Leaf",
        "# L\n\nbody\n",
        json!({ "parent_id": root_id }),
    );

    let resp = call(&dir, "handoff_doc_trace", json!({ "doc_id": leaf_id }));
    let p = payload(&resp);
    let chain = p["chain"].as_array().unwrap();
    let ids: Vec<&str> = chain.iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec![root_id.as_str(), leaf_id.as_str()]);
}

#[test]
fn doc_trace_includes_related_docs_in_expansion() {
    let (_tmp, dir) = setup_project();
    let target_id = save_doc(
        &dir,
        &unique_slug("trace-rel-target"),
        "Target",
        "# T\n\nbody\n",
        json!({}),
    );
    let root_id = save_doc(
        &dir,
        &unique_slug("trace-rel-root"),
        "Root",
        "# R\n\nbody\n",
        json!({ "related": [{ "id": target_id, "rel": "references" }] }),
    );

    let resp = call(
        &dir,
        "handoff_doc_trace",
        json!({ "doc_id": root_id, "direction": "down" }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    let chain = p["chain"].as_array().unwrap();
    let related_entry = chain
        .iter()
        .find(|c| c["id"] == target_id)
        .expect("related doc must be included in the trace expansion");
    assert_eq!(related_entry["rel"], "references");
}

#[test]
fn doc_trace_branches_reported_for_multi_child_forks() {
    let (_tmp, dir) = setup_project();
    let root_id = save_doc(
        &dir,
        &unique_slug("trace-fork-root"),
        "Root",
        "# R\n\nbody\n",
        json!({}),
    );
    let child_a_id = save_doc(
        &dir,
        &unique_slug("trace-fork-a"),
        "Child A",
        "# A\n\nbody\n",
        json!({ "parent_id": root_id }),
    );
    let child_b_id = save_doc(
        &dir,
        &unique_slug("trace-fork-b"),
        "Child B",
        "# B\n\nbody\n",
        json!({ "parent_id": root_id }),
    );

    let resp = call(
        &dir,
        "handoff_doc_trace",
        json!({ "doc_id": root_id, "direction": "down" }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    let branches = p["branches"].as_array().unwrap();
    assert_eq!(branches.len(), 2, "one branch entry per forked child");
    let fork_ids: Vec<&str> = branches
        .iter()
        .map(|b| b["fork_from"].as_str().unwrap())
        .collect();
    assert!(fork_ids.iter().all(|id| *id == root_id));

    let branch_doc_ids: Vec<&str> = branches
        .iter()
        .flat_map(|b| b["docs"].as_array().unwrap())
        .map(|d| d["id"].as_str().unwrap())
        .collect();
    assert!(branch_doc_ids.contains(&child_a_id.as_str()));
    assert!(branch_doc_ids.contains(&child_b_id.as_str()));
}

#[test]
fn doc_trace_cycle_detection_prevents_infinite_loop() {
    let (_tmp, dir) = setup_project();
    let a_id = save_doc(
        &dir,
        &unique_slug("trace-cycle-a"),
        "A",
        "# A\n\nbody\n",
        json!({}),
    );
    let b_id = save_doc(
        &dir,
        &unique_slug("trace-cycle-b"),
        "B",
        "# B\n\nbody\n",
        json!({ "parent_id": a_id }),
    );
    // Force a cycle: make A's parent B (A -> parent B -> parent A).
    let resp = call(
        &dir,
        "handoff_doc_save",
        json!({ "doc_id": a_id, "body": "# A\n\nbody\n", "parent_id": b_id }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));

    // direction=up from b should terminate rather than loop forever.
    let resp = call(
        &dir,
        "handoff_doc_trace",
        json!({ "doc_id": b_id, "direction": "up" }),
    );
    assert!(!is_error(&resp), "error: {}", payload_text(&resp));
    let p = payload(&resp);
    let chain = p["chain"].as_array().unwrap();
    // Must terminate with a bounded chain (not hang/blow the stack).
    assert!(chain.len() <= 3);
}

#[test]
fn doc_trace_missing_doc_is_error() {
    let (_tmp, dir) = setup_project();
    let resp = call(&dir, "handoff_doc_trace", json!({ "doc_id": "doc-nope" }));
    assert!(is_error(&resp));
}

#[test]
fn doc_trace_appears_in_tools_list() {
    let (_tmp, dir) = setup_project();
    let req = json!({
        "jsonrpc": "2.0", "id": 5,
        "method": "tools/list",
        "params": {}
    });
    let _ = &dir;
    let resp = send(&req.to_string()).unwrap();
    let tools = resp["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == "handoff_doc_graph"));
    assert!(tools.iter().any(|t| t["name"] == "handoff_doc_trace"));
}
