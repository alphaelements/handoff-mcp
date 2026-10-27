//! REQ-PDF-033: `handoff_report generate` inserts the traceability link
//! diagram (Mermaid `graph TD`, requirement -> spec -> test) into
//! verification and completion reports, controlled by `include_trace_diagram`.

use std::collections::HashMap;
use std::path::PathBuf;

use handoff_mcp::storage::tasks::{write_task, Schedule, TaskData};
use serde_json::{json, Value};
use tempfile::TempDir;

fn call(name: &str, arguments: Value) -> Value {
    let req = json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments }
    });
    let out = handoff_mcp::mcp::protocol::process_line(&req.to_string()).expect("response");
    serde_json::from_str(&out).expect("valid JSON response")
}

fn text(resp: &Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .expect("text content")
        .to_string()
}

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"].as_bool().unwrap_or(false)
}

fn ok(name: &str, arguments: Value) -> Value {
    let resp = call(name, arguments.clone());
    assert!(
        !is_error(&resp),
        "{name} {arguments} failed: {}",
        text(&resp)
    );
    serde_json::from_str(&text(&resp)).unwrap_or(Value::Null)
}

fn setup() -> (TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let pd = dir.path().to_string_lossy().to_string();
    ok(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "trace-diagram" }),
    );
    (dir, pd)
}

fn handoff(dir: &TempDir) -> PathBuf {
    dir.path().join(".handoff")
}

fn save_doc(pd: &str, slug: &str, layer: &str, body: &str) {
    ok(
        "handoff_doc_save",
        json!({ "project_dir": pd, "slug": slug, "title": slug, "layer": layer, "body": body }),
    );
}

/// REQ-940 (two acceptance tests) and an unrelated REQ-941.
fn seed_trace(pd: &str) {
    save_doc(
        pd,
        "requirements",
        "requirement",
        "# Requirements\n\n### REQ-940 Export\n\nBody.\n\n### REQ-941 Import\n\nBody.\n",
    );
    save_doc(
        pd,
        "acceptance",
        "acceptance",
        "# Acceptance\n\n### AT-940 CSV\n\n- verifies: REQ-940\n\nSteps.\n\n### AT-941 JSON\n\n- verifies: REQ-940\n\nSteps.\n",
    );
}

fn record(pd: &str, item: &str, result: &str) {
    let out = ok(
        "handoff_trace_update",
        json!({"project_dir": pd, "executor_kind": "human", "executor_id": "alice",
               "ops": [{"op": "record", "item": item, "result": result}]}),
    );
    assert!(out.get("failed").is_none(), "{out}");
}

fn generate_raw(pd: &str, report_type: &str, scope: Value, extra: Value) -> Value {
    let mut args = json!({ "project_dir": pd, "action": "generate",
                           "report_type": report_type, "scope": scope });
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    call("handoff_report", args)
}

/// Generates a report and returns its Markdown body.
fn generate(dir: &TempDir, pd: &str, report_type: &str, scope: Value, extra: Value) -> String {
    let resp = generate_raw(pd, report_type, scope, extra);
    assert!(!is_error(&resp), "generate failed: {}", text(&resp));
    let out: Value = serde_json::from_str(&text(&resp)).unwrap();
    let path = out["report"]["output_path"].as_str().unwrap();
    std::fs::read_to_string(handoff(dir).join(path)).unwrap()
}

/// Contents of every ```mermaid fence in `md`.
fn mermaid_blocks(md: &str) -> Vec<String> {
    md.split("```mermaid\n")
        .skip(1)
        .map(|rest| rest.split("```").next().unwrap().to_string())
        .collect()
}

fn trace_diagram(md: &str) -> String {
    let blocks: Vec<String> = mermaid_blocks(md)
        .into_iter()
        .filter(|b| b.starts_with("graph TD"))
        .collect();
    assert_eq!(blocks.len(), 1, "exactly one trace diagram in:\n{md}");
    blocks[0].clone()
}

/// The state class (`class n3 passing`) of the node labelled with `id`.
fn node_class(graph: &str, id: &str) -> String {
    let node = graph
        .lines()
        .find_map(|l| {
            let l = l.trim();
            l.contains(&format!("[\"{id} "))
                .then(|| l.split('[').next().unwrap().to_string())
        })
        .unwrap_or_else(|| panic!("no node {id} in:\n{graph}"));
    graph
        .lines()
        .filter_map(|l| l.trim().strip_prefix("class "))
        .find_map(|l| {
            let (nodes, class) = l.split_once(' ')?;
            nodes
                .split(',')
                .any(|n| n == node)
                .then(|| class.to_string())
        })
        .unwrap_or_else(|| panic!("node {id} has no class in:\n{graph}"))
}

#[test]
fn verification_report_has_a_colour_coded_trace_diagram() {
    let (dir, pd) = setup();
    seed_trace(&pd);
    record(&pd, "AT-940", "pass");
    record(&pd, "AT-941", "fail");
    let md = generate(&dir, &pd, "verification", json!({}), json!({}));

    assert!(md.contains("## Traceability Diagram"), "{md}");
    let g = trace_diagram(&md);
    assert!(g.contains("subgraph L0[\"requirement\"]"), "{g}");
    assert!(g.contains("subgraph L1[\"acceptance\"]"), "{g}");
    assert!(g.contains("REQ-940 Export"), "{g}");
    assert_eq!(g.matches("-.->|verifies|").count(), 2, "{g}");
    assert_eq!(node_class(&g, "AT-940"), "passing", "{g}");
    assert_eq!(node_class(&g, "AT-941"), "failing", "{g}");
    assert_eq!(node_class(&g, "REQ-941"), "not_run", "{g}");
    // The coverage pie is still there next to it.
    assert!(md.contains("pie showData"), "{md}");
}

#[test]
fn include_trace_diagram_false_leaves_it_out() {
    let (dir, pd) = setup();
    seed_trace(&pd);
    let md = generate(
        &dir,
        &pd,
        "verification",
        json!({}),
        json!({ "include_trace_diagram": false }),
    );
    assert!(!md.contains("graph TD"), "{md}");
    assert!(!md.contains("Traceability Diagram"), "{md}");
    // include_charts does not control the diagram.
    let md = generate(
        &dir,
        &pd,
        "verification",
        json!({}),
        json!({ "include_charts": false }),
    );
    assert!(md.contains("graph TD"), "{md}");
    assert!(!md.contains("pie showData"), "{md}");
}

#[test]
fn scope_items_narrow_the_diagram_to_connected_chains() {
    let (dir, pd) = setup();
    seed_trace(&pd);
    let md = generate(
        &dir,
        &pd,
        "verification",
        json!({ "items": ["REQ-941"] }),
        json!({}),
    );
    let g = trace_diagram(&md);
    assert!(g.contains("REQ-941") && !g.contains("REQ-940"), "{g}");
}

#[test]
fn data_trace_diagram_from_the_caller_wins() {
    let (dir, pd) = setup();
    seed_trace(&pd);
    let md = generate(
        &dir,
        &pd,
        "verification",
        json!({}),
        json!({ "data": { "trace_diagram": "graph TD\n    X --> Y" } }),
    );
    assert_eq!(trace_diagram(&md), "graph TD\n    X --> Y\n");
}

#[test]
fn argument_must_be_boolean_and_only_applies_to_generate() {
    let (_dir, pd) = setup();
    seed_trace(&pd);
    let resp = generate_raw(
        &pd,
        "verification",
        json!({}),
        json!({ "include_trace_diagram": "yes" }),
    );
    assert!(is_error(&resp), "{}", text(&resp));
    assert!(
        text(&resp).contains("include_trace_diagram"),
        "{}",
        text(&resp)
    );

    let resp = call(
        "handoff_report",
        json!({ "project_dir": pd, "action": "list", "include_trace_diagram": true }),
    );
    assert!(is_error(&resp), "{}", text(&resp));
    assert!(
        text(&resp).contains("include_trace_diagram"),
        "{}",
        text(&resp)
    );
}

#[test]
fn report_types_without_a_diagram_are_unaffected() {
    let (dir, pd) = setup();
    seed_trace(&pd);
    let md = generate(&dir, &pd, "defect", json!({}), json!({}));
    assert!(!md.contains("graph TD"), "{md}");
}

#[test]
fn completion_report_has_the_trace_diagram() {
    let (dir, pd) = setup();
    seed_trace(&pd);
    record(&pd, "AT-940", "pass");
    record(&pd, "AT-941", "pass");
    save_doc(
        &pd,
        "acceptance-extra",
        "acceptance",
        "# Extra\n\n### AT-942 Import\n\n- verifies: REQ-941\n\nSteps.\n",
    );
    record(&pd, "AT-942", "pass");
    save_doc(
        &pd,
        "requirements-extra",
        "requirement",
        "# Extra\n\n### REQ-942 Waived\n\n- waive-verify: reviewed by eye\n\nBody.\n",
    );
    let h = handoff(&dir);
    for id in ["REQ-940", "REQ-941", "REQ-942"] {
        let task_id = format!("impl-{}", id.to_lowercase());
        let task_dir = h.join("tasks").join(&task_id);
        std::fs::create_dir_all(&task_dir).unwrap();
        let data = TaskData {
            id: task_id.clone(),
            title: format!("Implement {id}"),
            notes: None,
            priority: None,
            created_at: Some("2026-10-01T09:00:00+00:00".into()),
            updated_at: None,
            completed_at: Some("2026-10-03T09:00:00+00:00".into()),
            labels: vec![],
            links: vec![],
            task_links: vec![],
            done_criteria: vec![],
            schedule: Some(Schedule {
                estimate_hours: Some(2.0),
                ..Schedule::default()
            }),
            dependencies: vec![],
            order: None,
            assignee: None,
            lock: None,
            scope_paths: vec![],
            extra: HashMap::new(),
        };
        write_task(&task_dir, "done", &data).unwrap();
        ok(
            "handoff_update_task",
            json!({"project_dir": pd, "task": {"id": task_id, "requirement_ids": [id]}}),
        );
    }
    let applied = ok(
        "handoff_trace_update",
        json!({"project_dir": pd, "executor_kind": "human", "executor_id": "reviewer",
        "ops": [
            {"op": "set_layer_status", "layer": "requirement", "status": "approved"},
            {"op": "set_layer_status", "layer": "acceptance", "status": "approved"},
        ]}),
    );
    assert!(applied.get("failed").is_none(), "{applied}");

    let md = generate(&dir, &pd, "completion", json!({}), json!({}));
    assert!(md.contains("## Traceability Diagram"), "{md}");
    let g = trace_diagram(&md);
    assert!(g.contains("REQ-940") && g.contains("AT-941"), "{g}");
    assert!(g.contains("-.->|verifies|"), "{g}");

    let md = generate(
        &dir,
        &pd,
        "completion",
        json!({}),
        json!({ "include_trace_diagram": false }),
    );
    assert!(!md.contains("graph TD"), "{md}");
}

#[test]
fn three_layer_chain_shows_refines_and_layer_order() {
    let (dir, pd) = setup();
    save_doc(
        &pd,
        "requirements",
        "requirement",
        "# Requirements\n\n### REQ-950 Export\n\nBody.\n",
    );
    save_doc(
        &pd,
        "basic-spec",
        "basic_spec",
        "# Basic spec\n\n### SPEC-950 Export format\n\n- refines: REQ-950\n\nBody.\n",
    );
    save_doc(
        &pd,
        "acceptance",
        "acceptance",
        "# Acceptance\n\n### AT-950 CSV\n\n- verifies: SPEC-950\n\nSteps.\n",
    );
    record(&pd, "AT-950", "pass");
    let md = generate(&dir, &pd, "verification", json!({}), json!({}));
    let g = trace_diagram(&md);

    let req = g.find("subgraph L0[\"requirement\"]").expect(&g);
    let spec = g.find("subgraph L1[\"basic_spec\"]").expect(&g);
    let at = g.find("subgraph L2[\"acceptance\"]").expect(&g);
    assert!(req < spec && spec < at, "{g}");
    assert_eq!(g.matches("-->|refines|").count(), 1, "{g}");
    assert_eq!(g.matches("-.->|verifies|").count(), 1, "{g}");
    assert_eq!(node_class(&g, "AT-950"), "passing", "{g}");
}

fn oversized_requirements(pd: &str) {
    let mut body = String::from("# Requirements\n\n");
    for n in 0..151 {
        body.push_str(&format!("### REQ-{} Item\n\nBody.\n\n", 2000 + n));
    }
    save_doc(pd, "requirements", "requirement", &body);
}

#[test]
fn too_large_diagram_is_reported_in_existing_warnings() {
    let (dir, pd) = setup();
    oversized_requirements(&pd);
    let md = generate(
        &dir,
        &pd,
        "verification",
        json!({}),
        json!({ "data": { "warnings": ["caller note"] } }),
    );
    assert!(!md.contains("graph TD"), "{md}");
    assert!(md.contains("caller note"), "{md}");
    assert!(md.contains("exceed the limit"), "{md}");
}

#[test]
fn non_array_warnings_in_data_is_rejected_instead_of_overwritten() {
    let (_dir, pd) = setup();
    oversized_requirements(&pd);
    let resp = generate_raw(
        &pd,
        "verification",
        json!({}),
        json!({ "data": { "warnings": "oops" } }),
    );
    assert!(is_error(&resp), "{}", text(&resp));
    assert!(text(&resp).contains("warnings"), "{}", text(&resp));
}
