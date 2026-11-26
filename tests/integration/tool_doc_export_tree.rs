//! REQ-PDF-026: `handoff_doc_export` with `doc_tree` bundles a document and
//! all its descendants (parent/children family tree) into one PDF, the
//! hierarchy showing in the headings of the table of contents and the body.
//!
//! Driven through the real JSON-RPC entry point. The success-path tests need
//! a Typst CLI (>= 0.12); without it they print a notice and pass, unless
//! `HANDOFF_REQUIRE_TYPST=1` turns that into a failure. The PDF structure is
//! checked with `typst query` on the generated `.typ`. Argument-validation
//! tests never need Typst.

use handoff_mcp::storage::docs::pdf::deps::{detect_in, Tool, ToolStatus};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
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

/// The Typst binary, or `None` (after a notice) when the test should skip.
fn typst_bin() -> Option<PathBuf> {
    // The real CLI, not `detect`: this file uses it as an independent inspector
    // (`typst query` / `typst compile`), which a `bundled-typst` build's
    // built-in compiler cannot be.
    match detect_in(
        Tool::Typst,
        &std::env::var_os("PATH").unwrap_or_default(),
        std::env::var_os("HANDOFF_TYPST_BIN").as_deref(),
    ) {
        ToolStatus::Found { path, .. } => Some(path),
        other => {
            assert!(
                std::env::var_os("HANDOFF_REQUIRE_TYPST").is_none(),
                "HANDOFF_REQUIRE_TYPST is set but Typst is unusable: {other:?}"
            );
            eprintln!("SKIPPED: typst not available ({other:?})");
            None
        }
    }
}

fn setup() -> (TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let pd = dir.path().to_string_lossy().to_string();
    let resp = call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "tree-test" }),
    );
    assert!(!is_error(&resp), "init failed: {}", text(&resp));
    (dir, pd)
}

/// Saves a document under `parent` (a doc id) and returns its id.
fn save_doc(pd: &str, slug: &str, title: &str, body: &str, parent: Option<&str>) -> String {
    let mut args = json!({
        "project_dir": pd, "slug": slug, "title": title, "body": body, "doc_type": "spec",
    });
    if let Some(parent) = parent {
        args["parent_id"] = json!(parent);
    }
    let resp = call("handoff_doc_save", args);
    assert!(!is_error(&resp), "doc_save failed: {}", text(&resp));
    let v: Value = serde_json::from_str(&text(&resp)).unwrap();
    v["doc_id"].as_str().unwrap().to_string()
}

/// root -> [alpha -> [alpha-one], beta]; every body repeats its title as `#`
/// and has one `##` section.
struct Tree {
    root: String,
    alpha: String,
    alpha_one: String,
    beta: String,
}

fn body(title: &str) -> String {
    format!("# {title}\n\nText of {title}.\n\n## Part of {title}\n\nMore.\n")
}

fn family(pd: &str) -> Tree {
    let root = save_doc(pd, "root", "Root", &body("Root"), None);
    let alpha = save_doc(pd, "alpha", "Alpha", &body("Alpha"), Some(&root));
    let alpha_one = save_doc(
        pd,
        "alpha-one",
        "Alpha One",
        &body("Alpha One"),
        Some(&alpha),
    );
    let beta = save_doc(pd, "beta", "Beta", &body("Beta"), Some(&root));
    Tree {
        root,
        alpha,
        alpha_one,
        beta,
    }
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

fn assert_pdf(path: &str) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    assert!(bytes.starts_with(b"%PDF-"), "{path} is not a PDF");
}

/// `typst query <typ> <selector>` as JSON.
fn query(typst: &Path, root: &str, typ: &str, selector: &str) -> Vec<Value> {
    let out = Command::new(typst)
        .args(["query", "--root", root, typ, selector])
        .output()
        .expect("run typst query");
    assert!(
        out.status.success(),
        "typst query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("query output is JSON")
}

/// Flattened plain text of a Typst content value from `typst query`.
fn content_text(v: &Value) -> String {
    match v {
        Value::Object(m) => {
            let mut s = String::new();
            if let Some(Value::String(t)) = m.get("text") {
                s.push_str(t);
            }
            for key in ["body", "children"] {
                if let Some(inner) = m.get(key) {
                    s.push_str(&content_text(inner));
                }
            }
            s
        }
        Value::Array(items) => items.iter().map(content_text).collect(),
        _ => String::new(),
    }
}

/// `(level, text)` of every heading in the body (the outline's own title is
/// not part of the content).
fn headings(typst: &Path, root: &str, typ: &str) -> Vec<(u64, String)> {
    query(typst, root, typ, "heading")
        .iter()
        .map(|h| (h["level"].as_u64().unwrap(), content_text(&h["body"])))
        .filter(|(_, t)| t != "目次" && t != "Contents")
        .collect()
}

fn heading(level: u64, title: &str) -> (u64, String) {
    (level, title.to_string())
}

fn slugs(out: &Value) -> Vec<String> {
    out["documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["slug"].as_str().unwrap().to_string())
        .collect()
}

// ---- schema and validation -------------------------------------------------

#[test]
fn schema_declares_doc_tree_as_a_fourth_way_to_select_documents() {
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
        tool["inputSchema"]["properties"]["doc_tree"]["type"],
        "string"
    );
    let one_of = tool["inputSchema"]["oneOf"].to_string();
    assert!(one_of.contains("doc_tree"), "{one_of}");
    assert!(
        tool["description"].as_str().unwrap().contains("doc_tree"),
        "description does not mention doc_tree"
    );
}

#[test]
fn doc_tree_is_exclusive_with_the_other_selectors() {
    let (_d, pd) = setup();
    for extra in [
        json!({ "doc_id": "a" }),
        json!({ "doc_ids": ["a"] }),
        json!({ "scope": { "layer": "requirement" } }),
    ] {
        let mut args = json!({ "doc_tree": "root" });
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let msg = export_err(&pd, args);
        assert!(
            msg.contains("doc_tree") && msg.contains("mutually exclusive"),
            "{msg}"
        );
    }
}

#[test]
fn doc_tree_must_be_a_non_blank_string() {
    let (_d, pd) = setup();
    for bad in [json!(3), json!(["root"])] {
        let msg = export_err(&pd, json!({ "doc_tree": bad.clone() }));
        assert!(msg.contains("doc_tree"), "{bad}: {msg}");
    }
    let msg = export_err(&pd, json!({ "doc_tree": "  " }));
    assert!(msg.contains("doc_tree"), "{msg}");
}

#[test]
fn doc_tree_always_bundles_so_bundle_false_is_rejected() {
    let (_d, pd) = setup();
    family(&pd);
    let msg = export_err(&pd, json!({ "doc_tree": "root", "bundle": false }));
    assert!(msg.contains("doc_tree") && msg.contains("bundle"), "{msg}");
}

#[test]
fn unknown_root_is_an_error_naming_it() {
    let (_d, pd) = setup();
    let msg = export_err(&pd, json!({ "doc_tree": "no-such-doc" }));
    assert!(msg.contains("no-such-doc"), "{msg}");
}

#[test]
fn layout_arguments_are_validated_with_doc_tree() {
    let (_d, pd) = setup();
    family(&pd);
    let msg = export_err(
        &pd,
        json!({ "doc_tree": "root", "page_break_style": "sideways" }),
    );
    assert!(msg.contains("page_break_style"), "{msg}");
}

// ---- export ----------------------------------------------------------------

#[test]
fn tree_is_bundled_depth_first_with_headings_one_level_down_per_generation() {
    let Some(typst) = typst_bin() else { return };
    let (_d, pd) = setup();
    let t = family(&pd);
    let out = export_ok(&pd, json!({ "doc_tree": "root" }));

    assert_eq!(out["bundle"], true);
    assert_eq!(out["exported"], 4);
    assert_eq!(out["failed"], 0);
    assert_eq!(out["bundle_title"], "Root", "defaults to the root's title");
    assert_eq!(slugs(&out), ["root", "alpha", "alpha-one", "beta"]);
    let docs = out["documents"].as_array().unwrap();
    let depths: Vec<_> = docs.iter().map(|d| d["depth"].as_u64().unwrap()).collect();
    assert_eq!(depths, [0, 1, 2, 1]);
    assert_eq!(docs[0]["doc_id"], t.root.as_str());
    assert_eq!(docs[1]["doc_id"], t.alpha.as_str());
    assert_eq!(docs[2]["doc_id"], t.alpha_one.as_str());
    assert_eq!(docs[3]["doc_id"], t.beta.as_str());
    assert_eq!(out["doc_tree"], "root");

    let path = out["pdf_path"].as_str().unwrap();
    assert_pdf(path);
    assert!(path.ends_with("bundle-root.pdf"), "{path}");

    // The same headings feed the table of contents and the body.
    let typ = out["typ_path"].as_str().unwrap();
    assert_eq!(
        headings(&typst, &pd, typ),
        [
            heading(1, "Root"),
            heading(2, "Part of Root"),
            heading(2, "Alpha"),
            heading(3, "Part of Alpha"),
            heading(3, "Alpha One"),
            heading(4, "Part of Alpha One"),
            heading(2, "Beta"),
            heading(3, "Part of Beta"),
        ]
    );
}

#[test]
fn a_subtree_can_be_exported_by_slug_or_id_and_a_leaf_alone() {
    let Some(typst) = typst_bin() else { return };
    let (_d, pd) = setup();
    let t = family(&pd);

    // By id, starting below the top: Alpha is the new level-1 root.
    let out = export_ok(&pd, json!({ "doc_tree": t.alpha, "bundle_title": "Sub" }));
    assert_eq!(slugs(&out), ["alpha", "alpha-one"]);
    assert_eq!(out["bundle_title"], "Sub");
    assert!(out["pdf_path"]
        .as_str()
        .unwrap()
        .ends_with("bundle-sub.pdf"));
    let typ = out["typ_path"].as_str().unwrap();
    let levels: Vec<_> = headings(&typst, &pd, typ)
        .into_iter()
        .filter(|(_, t)| !t.starts_with("Part"))
        .collect();
    assert_eq!(levels, [heading(1, "Alpha"), heading(2, "Alpha One")]);

    // A leaf is a tree of one.
    let out = export_ok(&pd, json!({ "doc_tree": "beta" }));
    assert_eq!(out["exported"], 1);
    assert_eq!(slugs(&out), ["beta"]);
    assert_pdf(out["pdf_path"].as_str().unwrap());
}

#[test]
fn bundle_options_apply_to_a_tree() {
    if typst_bin().is_none() {
        return;
    }
    let (_d, pd) = setup();
    family(&pd);
    let out = export_ok(
        &pd,
        json!({ "doc_tree": "root", "bundle": true, "page_break_style": "none",
                "header": {}, "cover_status": "Draft" }),
    );
    assert_pdf(out["pdf_path"].as_str().unwrap());
    let typ = std::fs::read_to_string(out["typ_path"].as_str().unwrap()).unwrap();
    assert!(typ.contains("Draft"), "cover status missing");
    // `none`: the documents follow each other, no break before the children.
    assert!(
        !typ.contains("#pagebreak(weak: true)\n#heading(level: 2)"),
        "{typ}"
    );
}

#[test]
fn running_header_shows_the_title_of_the_nested_document() {
    let Some(typst) = typst_bin() else { return };
    let (_d, pd) = setup();
    family(&pd);
    let out = export_ok(&pd, json!({ "doc_tree": "root", "header": {} }));
    let typ = out["typ_path"].as_str().unwrap();
    let headers: Vec<String> = query(&typst, &pd, typ, "<handoff-running-header>")
        .iter()
        .map(content_text)
        .collect();
    // One body page per document, each headed by its own title.
    assert_eq!(headers.len(), 4, "{headers:?}");
    for (header, title) in headers.iter().zip(["Root", "Alpha", "Alpha One", "Beta"]) {
        // The title, then the date (YYYY-MM-DD): "Alpha" must not be "Alpha One".
        let date = header
            .strip_prefix(title)
            .unwrap_or_else(|| panic!("{title}: {header:?}"));
        let parts: Vec<_> = date.split('-').collect();
        assert!(
            parts.len() == 3 && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit())),
            "{title}: date part of {header:?}"
        );
    }
}

#[test]
fn a_cycle_in_the_tree_is_visited_once() {
    let Some(_typst) = typst_bin() else { return };
    let (_d, pd) = setup();
    let t = family(&pd);
    // Make Root a child of Alpha One: root -> alpha -> alpha-one -> root.
    let resp = call(
        "handoff_doc_save",
        json!({ "project_dir": pd, "doc_id": t.root, "parent_id": t.alpha_one }),
    );
    assert!(!is_error(&resp), "{}", text(&resp));
    let out = export_ok(&pd, json!({ "doc_tree": "root" }));
    assert_eq!(out["exported"], 4);
    let mut s = slugs(&out);
    s.sort();
    assert_eq!(s, ["alpha", "alpha-one", "beta", "root"]);
}

#[test]
fn a_dangling_child_is_reported_and_the_rest_is_bundled() {
    let Some(_typst) = typst_bin() else { return };
    let (_d, pd) = setup();
    family(&pd);
    // Beta is a leaf (`children: []`); give it a child that does not exist.
    let file = Path::new(&pd).join(".handoff/docs/_doc.beta.md");
    let md = std::fs::read_to_string(&file).unwrap();
    assert!(md.contains("\nchildren: []\n"), "{md}");
    std::fs::write(
        &file,
        md.replacen(
            "\nchildren: []\n",
            "\nchildren:\n- doc-20000101-000000-000000\n",
            1,
        ),
    )
    .unwrap();

    let out = export_ok(&pd, json!({ "doc_tree": "root" }));
    assert_eq!(out["exported"], 4);
    assert_eq!(out["failed"], 1);
    let last = out["documents"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["doc_id"], "doc-20000101-000000-000000");
    assert!(
        last["error"].as_str().unwrap().contains("not found"),
        "{last}"
    );
}
