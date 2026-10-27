//! REQ-PDF-020: `handoff_doc_export` with `bundle: true` merges several
//! documents into one PDF (cover with the document list, one table of
//! contents, a page break and a titled section per document).
//!
//! Driven through the real JSON-RPC entry point. The success-path tests need
//! a Typst CLI (>= 0.12); without it they print a notice and pass, unless
//! `HANDOFF_REQUIRE_TYPST=1` turns that into a failure. The PDF structure is
//! checked with `typst query` on the generated `.typ` (headings with their
//! page numbers, the outline element), so no PDF text extractor is needed.
//! Argument-validation tests never need Typst.

use handoff_mcp::storage::docs::pdf::deps::{detect, Tool, ToolStatus};
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
    match detect(Tool::Typst) {
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
        json!({ "project_dir": pd, "project_name": "bundle-test" }),
    );
    assert!(!is_error(&resp), "init failed: {}", text(&resp));
    (dir, pd)
}

fn save_doc(pd: &str, slug: &str, title: &str, body: &str) -> String {
    let resp = call(
        "handoff_doc_save",
        json!({ "project_dir": pd, "slug": slug, "title": title, "body": body, "doc_type": "spec" }),
    );
    assert!(!is_error(&resp), "doc_save failed: {}", text(&resp));
    let v: Value = serde_json::from_str(&text(&resp)).unwrap();
    v["doc_id"].as_str().unwrap().to_string()
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

/// `(level, text)` of every heading in the body. The outline's own title
/// ("目次" / "Contents") is a heading too and is not part of the content.
fn headings(typst: &Path, root: &str, typ: &str) -> Vec<(u64, String)> {
    query(typst, root, typ, "heading")
        .iter()
        .map(|h| (h["level"].as_u64().unwrap(), content_text(&h["body"])))
        .filter(|(_, t)| t != "目次" && t != "Contents")
        .collect()
}

/// Number of pages the generated `.typ` renders to (one SVG per page).
fn page_count(typst: &Path, root: &str, typ: &str) -> usize {
    let out = tempfile::tempdir().unwrap();
    let pattern = out.path().join("p-{p}.svg");
    let run = Command::new(typst)
        .args(["compile", "--root", root, "--format", "svg", typ])
        .arg(&pattern)
        .output()
        .expect("run typst compile");
    assert!(
        run.status.success(),
        "typst compile failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    std::fs::read_dir(out.path()).unwrap().count()
}

const JP_BODY: &str =
    "# 設計書\n\n## 概要\n\n日本語の本文です。\n\n### 詳細\n\n- 項目一\n- 項目二\n";
const EN_BODY: &str = "# Plan\n\nPlain intro.\n\n## Scope\n\nText.\n";

fn three_docs(pd: &str) -> Vec<String> {
    vec![
        save_doc(pd, "design", "設計書", JP_BODY),
        save_doc(pd, "plan", "Plan", EN_BODY),
        save_doc(pd, "notes", "Notes", "Just a note, no headings.\n"),
    ]
}

// ---- schema ----------------------------------------------------------------

#[test]
fn schema_declares_bundle_and_bundle_title() {
    let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
    let out = handoff_mcp::mcp::protocol::process_line(&req.to_string()).unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    let tool = v["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "handoff_doc_export")
        .expect("handoff_doc_export is listed");
    let props = &tool["inputSchema"]["properties"];
    assert_eq!(props["bundle"]["type"], "boolean");
    assert_eq!(props["bundle"]["default"], false);
    assert_eq!(props["bundle_title"]["type"], "string");
}

// ---- argument validation (no Typst needed) -----------------------------------

#[test]
fn bundle_needs_doc_ids_not_doc_id() {
    let (_d, pd) = setup();
    let msg = export_err(&pd, json!({ "doc_id": "a", "bundle": true }));
    assert!(msg.contains("bundle") && msg.contains("doc_ids"), "{msg}");
}

#[test]
fn bundle_must_be_boolean() {
    let (_d, pd) = setup();
    let msg = export_err(&pd, json!({ "doc_ids": ["a"], "bundle": "yes" }));
    assert!(msg.contains("bundle") && msg.contains("boolean"), "{msg}");
}

#[test]
fn bundle_title_must_be_a_string() {
    let (_d, pd) = setup();
    let msg = export_err(
        &pd,
        json!({ "doc_ids": ["a"], "bundle": true, "bundle_title": 3 }),
    );
    assert!(msg.contains("bundle_title"), "{msg}");
}

#[test]
fn bundle_title_without_bundle_is_rejected_not_ignored() {
    let (_d, pd) = setup();
    let msg = export_err(&pd, json!({ "doc_ids": ["a"], "bundle_title": "X" }));
    assert!(
        msg.contains("bundle_title") && msg.contains("bundle"),
        "{msg}"
    );
}

#[test]
fn bundle_of_only_unknown_documents_is_an_error_naming_them() {
    let (_d, pd) = setup();
    let msg = export_err(
        &pd,
        json!({ "doc_ids": ["nope-1", "nope-2"], "bundle": true }),
    );
    assert!(msg.contains("nope-1") && msg.contains("nope-2"), "{msg}");
}

// ---- bundle PDF (needs Typst) ----------------------------------------------------

#[test]
fn bundle_merges_documents_into_one_pdf_with_unified_toc_and_page_breaks() {
    let Some(typst) = typst_bin() else { return };
    let (_d, pd) = setup();
    let ids = three_docs(&pd);
    let out = export_ok(
        &pd,
        json!({ "doc_ids": ids, "bundle": true, "bundle_title": "Release Pack" }),
    );

    // One PDF for the whole bundle.
    assert_eq!(out["format"], "pdf");
    assert_eq!(out["bundle"], true);
    assert_eq!(out["bundle_title"], "Release Pack");
    assert_eq!(out["exported"], 3);
    assert_eq!(out["failed"], 0);
    let pdf = out["pdf_path"].as_str().expect("pdf_path");
    assert_eq!(out["pdf_paths"], json!([pdf]));
    assert!(pdf.ends_with("exports/bundle-release-pack.pdf"), "{pdf}");
    assert_pdf(pdf);

    // Included documents, in request order.
    let docs = out["documents"].as_array().unwrap();
    let slugs: Vec<_> = docs.iter().map(|d| d["slug"].as_str().unwrap()).collect();
    assert_eq!(slugs, ["design", "plan", "notes"]);
    assert_eq!(docs[0]["doc_id"], json!(ids[0]));

    // Cover: bundle title + document list in the generated source.
    let typ = out["typ_path"].as_str().expect("typ_path");
    let source = std::fs::read_to_string(typ).unwrap();
    assert!(source.contains("title: \"Release Pack\""), "{source}");
    assert!(
        source.contains("documents: (\"設計書\", \"Plan\", \"Notes\", )"),
        "{source}"
    );

    // Unified table of contents: the template's outline exists once.
    assert_eq!(query(&typst, &pd, typ, "outline").len(), 1);

    // Titled sections: every document title is a level-1 heading, in order;
    // their own headings sit below. The duplicate `# title` of the Markdown
    // is not repeated.
    let h = headings(&typst, &pd, typ);
    let titles: Vec<_> = h
        .iter()
        .filter(|(l, _)| *l == 1)
        .map(|(_, t)| t.as_str())
        .collect();
    assert_eq!(titles, ["設計書", "Plan", "Notes"], "{h:?}");
    // 2nd-level headings come from the documents' `##`, 3rd from `###`.
    let sub: Vec<_> = h
        .iter()
        .filter(|(l, _)| *l > 1)
        .map(|(l, t)| (*l, t.as_str()))
        .collect();
    assert_eq!(sub, [(2, "概要"), (3, "詳細"), (2, "Scope")], "{h:?}");

    // Page breaks: cover + table of contents + one page per (short) document.
    assert_eq!(page_count(&typst, &pd, typ), 5);
}

#[test]
fn bundle_without_a_title_uses_a_default_and_a_plain_file_name() {
    if typst_bin().is_none() {
        return;
    }
    let (_d, pd) = setup();
    let ids = three_docs(&pd);
    let out = export_ok(&pd, json!({ "doc_ids": [ids[0], ids[1]], "bundle": true }));
    let pdf = out["pdf_path"].as_str().unwrap();
    assert!(pdf.ends_with("exports/bundle.pdf"), "{pdf}");
    assert_pdf(pdf);
    assert_eq!(out["bundle_title"], "Document Bundle");
}

#[test]
fn bundle_of_one_document_and_duplicates_collapse_to_one_section() {
    let Some(typst) = typst_bin() else { return };
    let (_d, pd) = setup();
    let ids = three_docs(&pd);
    let out = export_ok(
        &pd,
        json!({ "doc_ids": [ids[1], "plan", ids[1]], "bundle": true }),
    );
    assert_eq!(out["exported"], 1);
    let typ = out["typ_path"].as_str().unwrap();
    let level1 = headings(&typst, &pd, typ)
        .into_iter()
        .filter(|(l, _)| *l == 1)
        .count();
    assert_eq!(level1, 1);
}

#[test]
fn bundle_reports_an_unknown_document_and_still_bundles_the_rest() {
    if typst_bin().is_none() {
        return;
    }
    let (_d, pd) = setup();
    let ids = three_docs(&pd);
    let out = export_ok(
        &pd,
        json!({ "doc_ids": [ids[0], "no-such-doc", ids[1]], "bundle": true }),
    );
    assert_eq!(out["exported"], 2);
    assert_eq!(out["failed"], 1);
    let docs = out["documents"].as_array().unwrap();
    assert_eq!(docs.len(), 3);
    assert_eq!(docs[2]["doc_id"], "no-such-doc");
    assert!(docs[2]["error"].as_str().unwrap().contains("no-such-doc"));
    assert_pdf(out["pdf_path"].as_str().unwrap());
}

#[test]
fn include_toc_false_removes_the_unified_table_of_contents() {
    let Some(typst) = typst_bin() else { return };
    let (_d, pd) = setup();
    let ids = three_docs(&pd);
    let out = export_ok(
        &pd,
        json!({ "doc_ids": ids, "bundle": true, "include_toc": false }),
    );
    let typ = out["typ_path"].as_str().unwrap();
    assert!(query(&typst, &pd, typ, "outline").is_empty());
    // Cover + one page per document.
    assert_eq!(page_count(&typst, &pd, typ), 4);
    assert_pdf(out["pdf_path"].as_str().unwrap());
}

#[test]
fn bundle_honours_output_dir_and_every_builtin_template() {
    if typst_bin().is_none() {
        return;
    }
    let (dir, pd) = setup();
    let ids = three_docs(&pd);
    for template in ["default", "report", "spec"] {
        let target = dir.path().join(format!("out-{template}"));
        let out = export_ok(
            &pd,
            json!({
                "doc_ids": ids, "bundle": true, "template": template,
                "output_dir": target.to_string_lossy(),
            }),
        );
        let pdf = out["pdf_path"].as_str().unwrap();
        assert!(Path::new(pdf).starts_with(&target), "{pdf}");
        assert_pdf(pdf);
        let source = std::fs::read_to_string(out["typ_path"].as_str().unwrap()).unwrap();
        assert!(source.contains("documents:"), "{template}: {source}");
    }
}

#[test]
fn bundle_with_a_template_that_predates_the_document_list_still_exports() {
    if typst_bin().is_none() {
        return;
    }
    let (dir, pd) = setup();
    let ids = three_docs(&pd);
    let old = dir.path().join("old.typ");
    std::fs::write(
        &old,
        "#let conf(title: none, show-toc: true, ..args, doc) = { set document(title: title); doc }\n",
    )
    .unwrap();
    let out = export_ok(
        &pd,
        json!({ "doc_ids": ids, "bundle": true, "template": old.to_string_lossy() }),
    );
    assert_pdf(out["pdf_path"].as_str().unwrap());
    let warnings = out["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|w| w["code"] == "template_outdated"),
        "{warnings:?}"
    );
}

#[test]
fn titles_with_typst_markup_characters_are_shown_literally() {
    let Some(typst) = typst_bin() else { return };
    let (_d, pd) = setup();
    let tricky = "Rate #1 [draft] \"q\" *x* $y$ \\ _z_ <lbl> @ref";
    let a = save_doc(&pd, "tricky", tricky, "body\n");
    let b = save_doc(&pd, "plain", "Plain", "body\n");
    let out = export_ok(&pd, json!({ "doc_ids": [a, b], "bundle": true }));
    let typ = out["typ_path"].as_str().unwrap();
    let level1: Vec<_> = headings(&typst, &pd, typ)
        .into_iter()
        .filter(|(l, _)| *l == 1)
        .map(|(_, t)| t)
        .collect();
    assert_eq!(level1, [tricky.to_string(), "Plain".to_string()]);
}

#[test]
fn bundle_does_not_change_single_document_batch_behaviour() {
    if typst_bin().is_none() {
        return;
    }
    let (_d, pd) = setup();
    let ids = three_docs(&pd);
    let out = export_ok(&pd, json!({ "doc_ids": [ids[0], ids[1]], "bundle": false }));
    assert_eq!(out["exported"], 2);
    assert_eq!(out["pdf_paths"].as_array().unwrap().len(), 2);
    assert!(out.get("bundle").is_none() || out["bundle"] == false);
}

// ---- REQ-PDF-024: page_break_style and header ---------------------------------

#[test]
fn schema_declares_page_break_style_and_header() {
    let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
    let out = handoff_mcp::mcp::protocol::process_line(&req.to_string()).unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    let tool = v["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "handoff_doc_export")
        .expect("handoff_doc_export is listed");
    let props = &tool["inputSchema"]["properties"];
    assert_eq!(props["page_break_style"]["type"], "string");
    assert_eq!(
        props["page_break_style"]["enum"],
        json!(["break", "section", "none"])
    );
    assert_eq!(props["page_break_style"]["default"], "break");
    assert_eq!(props["header"]["type"], "object");
    for key in ["show_title", "show_page_number", "show_date"] {
        assert_eq!(
            props["header"]["properties"][key]["type"], "boolean",
            "{key}"
        );
    }
    assert_eq!(props["header"]["additionalProperties"], false);
}

#[test]
fn layout_arguments_are_validated_before_anything_is_rendered() {
    let (_d, pd) = setup();
    for (extra, needle) in [
        (
            json!({ "page_break_style": "wide" }),
            "break, section, none",
        ),
        (json!({ "page_break_style": 3 }), "page_break_style"),
        (json!({ "header": "yes" }), "'header' must be an object"),
        (json!({ "header": { "show_titel": true } }), "show_titel"),
        (json!({ "header": { "show_date": 1 } }), "header.show_date"),
    ] {
        let mut args = json!({ "doc_ids": ["a"], "bundle": true });
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let msg = export_err(&pd, args);
        assert!(msg.contains(needle), "{extra}: {msg}");
    }
}

#[test]
fn layout_arguments_without_bundle_are_rejected_not_ignored() {
    let (_d, pd) = setup();
    for extra in [
        json!({ "page_break_style": "none" }),
        json!({ "header": { "show_title": false } }),
    ] {
        let mut args = json!({ "doc_ids": ["a"] });
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let msg = export_err(&pd, args);
        assert!(msg.contains("only applies with 'bundle'"), "{extra}: {msg}");
    }
    // A single document gets the same refusal rather than a silent no-op.
    let msg = export_err(&pd, json!({ "doc_id": "a", "page_break_style": "section" }));
    assert!(msg.contains("page_break_style"), "{msg}");
}

/// Body pages' running header texts (one per page), via the label the
/// generated header carries.
fn header_texts(typst: &Path, root: &str, typ: &str) -> Vec<String> {
    query(typst, root, typ, "<handoff-running-header>")
        .iter()
        .map(content_text)
        .collect()
}

fn footer_count(typst: &Path, root: &str, typ: &str) -> usize {
    query(typst, root, typ, "<handoff-running-footer>").len()
}

fn level1_titles(typst: &Path, root: &str, typ: &str) -> Vec<String> {
    headings(typst, root, typ)
        .into_iter()
        .filter(|(l, _)| *l == 1)
        .map(|(_, t)| t)
        .collect()
}

#[test]
fn page_break_style_controls_the_separation_of_documents() {
    let Some(typst) = typst_bin() else { return };
    let (_d, pd) = setup();
    let ids = three_docs(&pd);
    let pages_and_titles = |style: Option<&str>| {
        let mut args = json!({ "doc_ids": ids, "bundle": true });
        if let Some(style) = style {
            args["page_break_style"] = json!(style);
        }
        let out = export_ok(&pd, args);
        assert_pdf(out["pdf_path"].as_str().unwrap());
        let typ = out["typ_path"].as_str().unwrap();
        (
            page_count(&typst, &pd, typ),
            level1_titles(&typst, &pd, typ),
        )
    };
    let titles = ["設計書", "Plan", "Notes"];

    // Default and explicit "break": cover + contents + one page each.
    assert_eq!(
        pages_and_titles(None),
        (5, titles.map(String::from).to_vec())
    );
    assert_eq!(
        pages_and_titles(Some("break")),
        (5, titles.map(String::from).to_vec())
    );
    // "section": a divider page (title only) before each document's pages.
    assert_eq!(
        pages_and_titles(Some("section")),
        (2 + 3 * 2, titles.map(String::from).to_vec())
    );
    // "none": the short documents follow each other on one page.
    assert_eq!(
        pages_and_titles(Some("none")),
        (3, titles.map(String::from).to_vec())
    );
}

#[test]
fn header_option_shows_the_documents_title_date_and_page_number() {
    let Some(typst) = typst_bin() else { return };
    let (_d, pd) = setup();
    let ids = three_docs(&pd);
    let out = export_ok(
        &pd,
        json!({ "doc_ids": ids, "bundle": true, "bundle_title": "Pack", "header": {} }),
    );
    assert_pdf(out["pdf_path"].as_str().unwrap());
    let typ = out["typ_path"].as_str().unwrap();

    // One header and one footer per body page (3 documents, one page each);
    // the cover and the table of contents are not touched.
    let headers = header_texts(&typst, &pd, typ);
    assert_eq!(headers.len(), 3, "{headers:?}");
    for (header, title) in headers.iter().zip(["設計書", "Plan", "Notes"]) {
        assert!(header.starts_with(title), "{title}: {header:?}");
        let date = header.strip_prefix(title).unwrap();
        let parts: Vec<_> = date.split('-').collect();
        assert!(
            parts.len() == 3 && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit())),
            "date part of {header:?}"
        );
    }
    assert_eq!(footer_count(&typst, &pd, typ), 3);
}

#[test]
fn header_switches_remove_exactly_their_part() {
    let Some(typst) = typst_bin() else { return };
    let (_d, pd) = setup();
    let ids = three_docs(&pd);
    let run = |header: Value| {
        let out = export_ok(
            &pd,
            json!({ "doc_ids": ids, "bundle": true, "header": header }),
        );
        assert_pdf(out["pdf_path"].as_str().unwrap());
        out["typ_path"].as_str().unwrap().to_string()
    };

    // No title: only the date remains in the header.
    let typ = run(json!({ "show_title": false }));
    let headers = header_texts(&typst, &pd, &typ);
    assert_eq!(headers.len(), 3);
    assert!(
        headers.iter().all(|h| h.len() == 10 && !h.contains("Plan")),
        "{headers:?}"
    );
    assert_eq!(footer_count(&typst, &pd, &typ), 3);

    // No date: only the title.
    let typ = run(json!({ "show_date": false }));
    assert_eq!(header_texts(&typst, &pd, &typ), ["設計書", "Plan", "Notes"]);

    // No page number: headers stay, the footer goes.
    let typ = run(json!({ "show_page_number": false }));
    assert_eq!(header_texts(&typst, &pd, &typ).len(), 3);
    assert_eq!(footer_count(&typst, &pd, &typ), 0);

    // Only the page number.
    let typ = run(json!({ "show_title": false, "show_date": false }));
    assert!(header_texts(&typst, &pd, &typ).is_empty());
    assert_eq!(footer_count(&typst, &pd, &typ), 3);

    // Nothing at all.
    let typ = run(json!({
        "show_title": false, "show_date": false, "show_page_number": false
    }));
    assert!(header_texts(&typst, &pd, &typ).is_empty());
    assert_eq!(footer_count(&typst, &pd, &typ), 0);
}

#[test]
fn without_the_header_option_the_template_keeps_its_own_header() {
    let Some(typst) = typst_bin() else { return };
    let (_d, pd) = setup();
    let ids = three_docs(&pd);
    let out = export_ok(&pd, json!({ "doc_ids": ids, "bundle": true }));
    let typ = out["typ_path"].as_str().unwrap();
    assert!(header_texts(&typst, &pd, typ).is_empty());
    assert_eq!(footer_count(&typst, &pd, typ), 0);
}

#[test]
fn the_header_follows_the_document_across_its_pages() {
    let Some(typst) = typst_bin() else { return };
    let (_d, pd) = setup();
    let long: String = (0..120)
        .map(|i| format!("Paragraph number {i} of the long document.\n\n"))
        .collect();
    let ids = vec![
        save_doc(&pd, "long", "Long Doc", &long),
        save_doc(&pd, "after", "After", "short\n"),
    ];
    let out = export_ok(
        &pd,
        json!({ "doc_ids": ids, "bundle": true, "header": { "show_date": false } }),
    );
    let typ = out["typ_path"].as_str().unwrap();
    let headers = header_texts(&typst, &pd, typ);
    assert!(headers.len() > 2, "{headers:?}");
    // Continuation pages of the long document still name it; the last page
    // belongs to the next one.
    assert!(
        headers[..headers.len() - 1].iter().all(|h| h == "Long Doc"),
        "{headers:?}"
    );
    assert_eq!(headers.last().unwrap(), "After");
}

#[test]
fn layout_works_with_every_builtin_template() {
    if typst_bin().is_none() {
        return;
    }
    let (_d, pd) = setup();
    let ids = three_docs(&pd);
    for template in ["default", "report", "spec"] {
        for style in ["break", "section", "none"] {
            let out = export_ok(
                &pd,
                json!({
                    "doc_ids": ids, "bundle": true, "template": template,
                    "page_break_style": style, "header": { "show_date": false },
                }),
            );
            assert_pdf(out["pdf_path"].as_str().unwrap());
        }
    }
}

#[test]
fn layout_works_without_a_table_of_contents() {
    let Some(typst) = typst_bin() else { return };
    let (_d, pd) = setup();
    let ids = three_docs(&pd);
    let out = export_ok(
        &pd,
        json!({ "doc_ids": ids, "bundle": true, "include_toc": false, "header": {} }),
    );
    let typ = out["typ_path"].as_str().unwrap();
    assert_pdf(out["pdf_path"].as_str().unwrap());
    assert_eq!(page_count(&typst, &pd, typ), 4);
    assert_eq!(header_texts(&typst, &pd, typ).len(), 3);
}
