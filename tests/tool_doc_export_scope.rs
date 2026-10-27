//! REQ-PDF-021: `handoff_doc_export` with `scope` selects the documents to
//! export by layer / doc_type / milestone / tags / task_id (AND-combined)
//! instead of an explicit `doc_id` / `doc_ids`.
//!
//! Driven through the real JSON-RPC entry point. Success-path tests need a
//! Typst CLI (>= 0.12); without it they print a notice and pass unless
//! `HANDOFF_REQUIRE_TYPST=1` turns that into a failure. Validation and
//! "no match" tests never need Typst.

use handoff_mcp::storage::docs::pdf::deps::{detect, Tool, ToolStatus};
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

fn typst_available() -> bool {
    match detect(Tool::Typst) {
        ToolStatus::Found { .. } => true,
        other => {
            assert!(
                std::env::var_os("HANDOFF_REQUIRE_TYPST").is_none(),
                "HANDOFF_REQUIRE_TYPST is set but Typst is unusable: {other:?}"
            );
            eprintln!("SKIPPED: typst not available ({other:?})");
            false
        }
    }
}

fn setup() -> (TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let pd = dir.path().to_string_lossy().to_string();
    let resp = call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "scope-test" }),
    );
    assert!(!is_error(&resp), "init failed: {}", text(&resp));
    let resp = call(
        "handoff_update_task",
        json!({ "project_dir": pd, "task": { "id": "t1", "title": "Task one", "status": "todo" } }),
    );
    assert!(!is_error(&resp), "task failed: {}", text(&resp));
    (dir, pd)
}

fn save_doc(pd: &str, slug: &str, extra: Value) -> String {
    let mut args = json!({
        "project_dir": pd, "slug": slug, "title": format!("Title {slug}"),
        "body": format!("# Title {slug}\n\nBody of {slug}.\n"),
    });
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    let resp = call("handoff_doc_save", args);
    assert!(!is_error(&resp), "doc_save failed: {}", text(&resp));
    let v: Value = serde_json::from_str(&text(&resp)).unwrap();
    v["doc_id"].as_str().unwrap().to_string()
}

/// a: requirement/spec/v1/[x,y]/t1   b: requirement/spec/v2/[x]
/// c: basic_spec/guide/v1/[x,y]      d: no layer, adr
fn corpus(pd: &str) {
    save_doc(
        pd,
        "doc-a",
        json!({ "doc_type": "spec", "layer": "requirement", "milestone": "v1",
                "tags": ["x", "y"], "task_ids": ["t1"] }),
    );
    save_doc(
        pd,
        "doc-b",
        json!({ "doc_type": "spec", "layer": "requirement", "milestone": "v2", "tags": ["x"] }),
    );
    save_doc(
        pd,
        "doc-c",
        json!({ "doc_type": "guide", "layer": "basic_spec", "milestone": "v1", "tags": ["x", "y"] }),
    );
    save_doc(pd, "doc-d", json!({ "doc_type": "adr" }));
}

fn export(pd: &str, mut args: Value) -> Value {
    args["project_dir"] = json!(pd);
    call("handoff_doc_export", args)
}

fn export_ok(pd: &str, args: Value) -> Value {
    let resp = export(pd, args.clone());
    assert!(!is_error(&resp), "{args} failed: {}", text(&resp));
    serde_json::from_str(&text(&resp)).expect("tool output is JSON")
}

fn export_err(pd: &str, args: Value) -> String {
    let resp = export(pd, args.clone());
    assert!(
        is_error(&resp),
        "{args} should have failed: {}",
        text(&resp)
    );
    text(&resp)
}

fn slugs(out: &Value) -> Vec<String> {
    let mut v: Vec<String> = out["documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["slug"].as_str().unwrap().to_string())
        .collect();
    v.sort();
    v
}

fn assert_pdf(path: &str) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    assert!(bytes.starts_with(b"%PDF-"), "{path} is not a PDF");
}

#[test]
fn schema_declares_scope_with_its_filters() {
    let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
    let out = handoff_mcp::mcp::protocol::process_line(&req.to_string()).unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    let tool = v["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "handoff_doc_export")
        .unwrap();
    let scope = &tool["inputSchema"]["properties"]["scope"];
    assert_eq!(scope["type"], "object");
    for key in ["layer", "doc_type", "milestone", "tags", "task_id"] {
        assert!(scope["properties"].get(key).is_some(), "scope.{key}");
    }
    // `scope` replaces doc_id/doc_ids, so the schema may not demand them.
    let one_of = tool["inputSchema"]["oneOf"].to_string();
    assert!(one_of.contains("scope"), "{one_of}");
}

#[test]
fn scope_without_any_match_is_an_error_naming_the_scope() {
    let (_d, pd) = setup();
    corpus(&pd);
    let msg = export_err(
        &pd,
        json!({ "scope": { "layer": "requirement", "milestone": "v9" } }),
    );
    assert!(
        msg.contains("scope") && msg.contains("v9") && msg.contains("No documents"),
        "{msg}"
    );
}

#[test]
fn filters_are_anded_so_a_partial_match_is_not_enough() {
    let (_d, pd) = setup();
    corpus(&pd);
    // doc-c has milestone v1 and tags x,y but layer basic_spec; doc-b has
    // layer requirement but milestone v2.
    let msg = export_err(
        &pd,
        json!({ "scope": { "layer": "requirement", "milestone": "v1", "doc_type": "guide" } }),
    );
    assert!(msg.contains("No documents"), "{msg}");
}

#[test]
fn scope_is_exclusive_with_doc_id_and_doc_ids() {
    let (_d, pd) = setup();
    for extra in [json!({ "doc_id": "a" }), json!({ "doc_ids": ["a"] })] {
        let mut args = json!({ "scope": { "layer": "requirement" } });
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let msg = export_err(&pd, args);
        assert!(
            msg.contains("scope") && msg.contains("mutually exclusive"),
            "{msg}"
        );
    }
}

#[test]
fn malformed_scope_is_rejected_not_ignored() {
    let (_d, pd) = setup();
    for (scope, needle) in [
        (json!("requirement"), "object"),
        (json!({}), "at least one"),
        (json!({ "layer": 3 }), "layer"),
        (json!({ "tags": "x" }), "tags"),
        (json!({ "tags": [1] }), "tags"),
        (json!({ "layer": " " }), "layer"),
        (json!({ "colour": "red" }), "colour"),
    ] {
        let msg = export_err(&pd, json!({ "scope": scope.clone() }));
        assert!(msg.contains(needle), "{scope}: {msg}");
    }
}

#[test]
fn scope_exports_one_pdf_per_matching_document() {
    if !typst_available() {
        return;
    }
    let (_d, pd) = setup();
    corpus(&pd);
    let out = export_ok(&pd, json!({ "scope": { "doc_type": "spec" } }));
    assert_eq!(slugs(&out), ["doc-a", "doc-b"]);
    assert_eq!(out["exported"], 2);
    assert!(out.get("pdf_path").is_none(), "{out}");
    for p in out["pdf_paths"].as_array().unwrap() {
        assert_pdf(p.as_str().unwrap());
    }
}

#[test]
fn scope_with_bundle_merges_the_filtered_documents_into_one_pdf() {
    if !typst_available() {
        return;
    }
    let (_d, pd) = setup();
    corpus(&pd);
    let out = export_ok(
        &pd,
        json!({ "scope": { "milestone": "v1", "tags": ["x", "y"] },
                "bundle": true, "bundle_title": "Milestone v1" }),
    );
    assert_eq!(out["bundle"], true);
    assert_eq!(out["exported"], 2);
    assert_eq!(slugs(&out), ["doc-a", "doc-c"]);
    let pdf = out["pdf_path"].as_str().unwrap();
    assert!(pdf.ends_with("exports/bundle-milestone-v1.pdf"), "{pdf}");
    assert_pdf(pdf);
}

#[test]
fn every_filter_selects_on_its_own() {
    if !typst_available() {
        return;
    }
    let (_d, pd) = setup();
    corpus(&pd);
    for (scope, expected) in [
        (json!({ "layer": "basic_spec" }), vec!["doc-c"]),
        (json!({ "doc_type": "adr" }), vec!["doc-d"]),
        (json!({ "milestone": "v2" }), vec!["doc-b"]),
        (json!({ "tags": ["y"] }), vec!["doc-a", "doc-c"]),
        (json!({ "task_id": "t1" }), vec!["doc-a"]),
    ] {
        let out = export_ok(&pd, json!({ "scope": scope.clone(), "bundle": true }));
        assert_eq!(slugs(&out), expected, "{scope}");
    }
}
