//! REQ-PDF-027: `handoff_doc_export` with `task_id` exports the documents a
//! task is linked to (its doc links, its requirement links, the documents that
//! list it) as one merged PDF, also inside `scope`.
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

fn ok(resp: Value) -> Value {
    assert!(!is_error(&resp), "tool failed: {}", text(&resp));
    serde_json::from_str(&text(&resp)).unwrap_or(Value::Null)
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

fn save_doc(pd: &str, slug: &str, extra: Value) -> String {
    let mut args = json!({
        "project_dir": pd, "slug": slug, "title": format!("Title {slug}"),
        "body": format!("# Title {slug}\n\nBody of {slug}.\n"),
    });
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    ok(call("handoff_doc_save", args))["doc_id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Tasks t1 ("Build login") and t2; documents:
///   doc-listed   - lists t1 itself (doc_save task_ids), doc_type spec
///   doc-req      - requirement layer, REQ-001 implemented by t1
///   doc-t2       - lists t2
///   doc-unlinked - linked to nobody
fn project() -> (TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let pd = dir.path().to_string_lossy().to_string();
    ok(call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "task-export" }),
    ));
    ok(call(
        "handoff_update_config",
        json!({ "project_dir": pd, "updates": { "trace.profile": "minimal" } }),
    ));
    for (id, title) in [
        ("t1", "Build login"),
        ("t2", "Other work"),
        ("t3", "Lonely"),
    ] {
        ok(call(
            "handoff_update_task",
            json!({ "project_dir": pd, "task": { "id": id, "title": title, "status": "todo" } }),
        ));
    }
    save_doc(
        &pd,
        "doc-listed",
        json!({ "doc_type": "spec", "task_ids": ["t1"] }),
    );
    save_doc(
        &pd,
        "doc-req",
        json!({ "doc_type": "spec", "layer": "requirement",
                "body": "# Requirements\n\n## REQ-001 Login\n\n- priority: P0\n\nLog in.\n" }),
    );
    ok(call(
        "handoff_update_task",
        json!({ "project_dir": pd, "task": { "id": "t1", "requirement_ids": ["REQ-001"] } }),
    ));
    save_doc(
        &pd,
        "doc-t2",
        json!({ "doc_type": "guide", "task_ids": ["t2"] }),
    );
    save_doc(&pd, "doc-unlinked", json!({ "doc_type": "guide" }));
    (dir, pd)
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
fn schema_declares_a_top_level_task_id_alternative_to_the_other_selectors() {
    let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
    let out = handoff_mcp::mcp::protocol::process_line(&req.to_string()).unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    let tool = v["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "handoff_doc_export")
        .unwrap();
    assert_eq!(
        tool["inputSchema"]["properties"]["task_id"]["type"],
        "string"
    );
    let one_of = tool["inputSchema"]["oneOf"].as_array().unwrap();
    assert!(
        one_of
            .iter()
            .any(|alt| alt["required"] == json!(["task_id"])),
        "{one_of:?}"
    );
}

#[test]
fn unknown_task_is_an_error_naming_the_task() {
    let (_d, pd) = project();
    let msg = export_err(&pd, json!({ "task_id": "t-nope" }));
    assert!(msg.contains("t-nope"), "{msg}");
}

#[test]
fn a_task_without_linked_documents_is_an_error_naming_the_task() {
    let (_d, pd) = project();
    let msg = export_err(&pd, json!({ "task_id": "t3" }));
    assert!(msg.contains("No documents") && msg.contains("t3"), "{msg}");
}

#[test]
fn task_id_is_exclusive_with_explicit_document_selectors() {
    let (_d, pd) = project();
    for extra in [
        json!({ "doc_id": "doc-t2" }),
        json!({ "doc_ids": ["doc-t2"] }),
        json!({ "doc_tree": "doc-t2" }),
    ] {
        let mut args = json!({ "task_id": "t1" });
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let msg = export_err(&pd, args);
        assert!(
            msg.contains("task_id") && msg.contains("mutually exclusive"),
            "{msg}"
        );
    }
}

#[test]
fn malformed_task_id_is_rejected_not_ignored() {
    let (_d, pd) = project();
    for (task_id, needle) in [(json!(" "), "blank"), (json!(3), "string")] {
        let msg = export_err(&pd, json!({ "task_id": task_id.clone() }));
        assert!(
            msg.contains("task_id") && msg.contains(needle),
            "{task_id}: {msg}"
        );
    }
}

#[test]
fn task_id_next_to_a_different_scope_task_id_is_a_conflict() {
    let (_d, pd) = project();
    let msg = export_err(
        &pd,
        json!({ "task_id": "t1", "scope": { "task_id": "t2" } }),
    );
    assert!(msg.contains("task_id") && msg.contains("t2"), "{msg}");
}

#[test]
fn task_id_bundles_every_document_the_task_is_linked_to() {
    if !typst_available() {
        return;
    }
    let (_d, pd) = project();
    let out = export_ok(&pd, json!({ "task_id": "t1" }));
    assert_eq!(out["bundle"], true, "{out}");
    assert_eq!(out["exported"], 2);
    assert_eq!(slugs(&out), ["doc-listed", "doc-req"]);
    let title = out["bundle_title"].as_str().unwrap();
    assert!(
        title.contains("t1") && title.contains("Build login"),
        "{title}"
    );
    assert_pdf(out["pdf_path"].as_str().unwrap());
}

#[test]
fn an_explicit_bundle_title_wins_over_the_task_title() {
    if !typst_available() {
        return;
    }
    let (_d, pd) = project();
    let out = export_ok(&pd, json!({ "task_id": "t2", "bundle_title": "T2 pack" }));
    assert_eq!(out["bundle_title"], "T2 pack");
    assert_eq!(slugs(&out), ["doc-t2"]);
    assert!(out["pdf_path"]
        .as_str()
        .unwrap()
        .ends_with("bundle-t2-pack.pdf"));
}

#[test]
fn bundle_false_exports_one_pdf_per_linked_document() {
    if !typst_available() {
        return;
    }
    let (_d, pd) = project();
    let out = export_ok(&pd, json!({ "task_id": "t1", "bundle": false }));
    assert!(out.get("bundle").is_none(), "{out}");
    assert_eq!(slugs(&out), ["doc-listed", "doc-req"]);
    assert_eq!(out["pdf_paths"].as_array().unwrap().len(), 2);
    for p in out["pdf_paths"].as_array().unwrap() {
        assert_pdf(p.as_str().unwrap());
    }
}

#[test]
fn bundle_options_apply_to_a_task_bundle() {
    if !typst_available() {
        return;
    }
    let (_d, pd) = project();
    let out = export_ok(
        &pd,
        json!({ "task_id": "t1", "page_break_style": "section", "header": {} }),
    );
    assert_eq!(out["bundle"], true);
    assert_eq!(out["exported"], 2);
}

#[test]
fn scope_task_id_uses_the_same_link_reach_and_ands_with_other_filters() {
    if !typst_available() {
        return;
    }
    let (_d, pd) = project();
    // Only the requirement-link document passes layer=requirement.
    let out = export_ok(
        &pd,
        json!({ "scope": { "task_id": "t1", "layer": "requirement" }, "bundle": true }),
    );
    assert_eq!(slugs(&out), ["doc-req"]);
    // Top-level task_id may be combined with the other scope filters.
    let out = export_ok(
        &pd,
        json!({ "task_id": "t1", "scope": { "doc_type": "spec" } }),
    );
    assert_eq!(slugs(&out), ["doc-listed", "doc-req"]);
    // The same task_id on both sides is not a conflict.
    let out = export_ok(
        &pd,
        json!({ "task_id": "t1", "scope": { "task_id": "t1", "layer": "requirement" } }),
    );
    assert_eq!(slugs(&out), ["doc-req"]);
}

#[test]
fn a_doc_link_by_slug_selects_the_document() {
    if !typst_available() {
        return;
    }
    let (_d, pd) = project();
    // A bare `doc` link (not made by doc_save) written straight into the task.
    let task_file = std::fs::read_dir(std::path::Path::new(&pd).join(".handoff/tasks"))
        .unwrap()
        .flatten()
        .find(|e| e.file_name().to_string_lossy().starts_with("t3"))
        .map(|e| e.path())
        .expect("t3 dir");
    let file = std::fs::read_dir(&task_file)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("_task.")
        })
        .expect("task file");
    let mut data: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    data["task_links"] = json!([{ "target": "doc-unlinked", "link_type": "doc" }]);
    std::fs::write(&file, serde_json::to_string_pretty(&data).unwrap()).unwrap();

    let out = export_ok(&pd, json!({ "task_id": "t3" }));
    assert_eq!(slugs(&out), ["doc-unlinked"]);
}
