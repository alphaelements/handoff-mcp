//! REQ-PDF-006 / REQ-PDF-007: `handoff_doc_export` and `handoff_report`
//! `format: "pdf"`, driven through the real JSON-RPC entry point.
//!
//! The success-path tests need a Typst CLI (>= 0.12). When it is not
//! installed they print a notice and pass, so a plain `cargo test` works on
//! machines without it; set `HANDOFF_REQUIRE_TYPST=1` (as CI should) to turn
//! "typst missing" into a failure instead. Argument-validation tests never
//! need Typst.

use handoff_mcp::storage::docs::pdf::deps::{detect, Tool, ToolStatus};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
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

/// `true` when the test may run; `false` (after a notice) when Typst is
/// unavailable and the test should be skipped.
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
        json!({ "project_dir": pd, "project_name": "export-test" }),
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

const JP_BODY: &str = "# 設計書\n\n## 概要\n\n日本語の本文です。\n\n- 項目一\n- 項目二\n";

#[test]
fn tool_is_registered_with_pdf_options() {
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
    for key in [
        "doc_id",
        "doc_ids",
        "format",
        "output_dir",
        "template",
        "include_toc",
    ] {
        assert!(props.get(key).is_some(), "schema lacks {key}");
    }
}

#[test]
fn report_schema_accepts_format_and_pdf_options() {
    let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
    let out = handoff_mcp::mcp::protocol::process_line(&req.to_string()).unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    let tool = v["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "handoff_report")
        .unwrap();
    let props = &tool["inputSchema"]["properties"];
    for key in ["format", "output_dir", "template", "include_toc"] {
        assert!(props.get(key).is_some(), "report schema lacks {key}");
    }
}

// ---- argument validation (no Typst needed) --------------------------------

#[test]
fn requires_doc_id_or_doc_ids() {
    let (_d, pd) = setup();
    let msg = export_err(&pd, json!({ "format": "pdf" }));
    assert!(msg.contains("doc_id") && msg.contains("doc_ids"), "{msg}");
}

#[test]
fn doc_id_and_doc_ids_are_mutually_exclusive() {
    let (_d, pd) = setup();
    let msg = export_err(&pd, json!({ "doc_id": "a", "doc_ids": ["b"] }));
    assert!(msg.contains("mutually exclusive"), "{msg}");
}

#[test]
fn empty_doc_ids_is_rejected() {
    let (_d, pd) = setup();
    let msg = export_err(&pd, json!({ "doc_ids": [] }));
    assert!(msg.contains("doc_ids"), "{msg}");
}

#[test]
fn non_string_doc_ids_entry_is_rejected() {
    let (_d, pd) = setup();
    let msg = export_err(&pd, json!({ "doc_ids": ["a", 3] }));
    assert!(msg.contains("doc_ids"), "{msg}");
}

#[test]
fn unsupported_format_is_rejected() {
    let (_d, pd) = setup();
    let msg = export_err(&pd, json!({ "doc_id": "a", "format": "docx" }));
    assert!(msg.contains("docx") && msg.contains("pdf"), "{msg}");
}

#[test]
fn output_dir_with_parent_component_is_rejected() {
    let (_d, pd) = setup();
    let msg = export_err(&pd, json!({ "doc_id": "a", "output_dir": "../outside" }));
    assert!(msg.contains("output_dir") && msg.contains(".."), "{msg}");
}

#[test]
fn include_toc_must_be_boolean() {
    let (_d, pd) = setup();
    let msg = export_err(&pd, json!({ "doc_id": "a", "include_toc": "yes" }));
    assert!(msg.contains("include_toc"), "{msg}");
}

#[test]
fn unknown_document_is_an_error_for_a_single_doc_id() {
    let (_d, pd) = setup();
    let msg = export_err(&pd, json!({ "doc_id": "no-such-doc" }));
    assert!(msg.contains("no-such-doc"), "{msg}");
}

// ---- REQ-PDF-006 AC1: single document --------------------------------------

#[test]
fn single_doc_is_exported_to_exports_dir() {
    if !typst_available() {
        return;
    }
    let (dir, pd) = setup();
    let doc_id = save_doc(&pd, "design-doc", "設計書", JP_BODY);

    let out = export_ok(&pd, json!({ "doc_id": doc_id, "format": "pdf" }));

    let pdf = out["pdf_path"].as_str().expect("pdf_path");
    assert!(
        pdf.ends_with(".handoff/exports/design-doc.pdf"),
        "unexpected path {pdf}"
    );
    assert_pdf(pdf);
    assert_eq!(out["format"], "pdf");
    assert_eq!(out["exported"], 1);
    assert_eq!(out["failed"], 0);
    assert_eq!(out["documents"][0]["doc_id"], doc_id);
    assert_eq!(out["documents"][0]["slug"], "design-doc");
    assert!(out["warnings"].is_array());
    assert!(dir.path().join(".handoff/exports/design-doc.typ").exists());
}

#[test]
fn doc_can_be_addressed_by_slug_and_format_defaults_to_pdf() {
    if !typst_available() {
        return;
    }
    let (_d, pd) = setup();
    save_doc(&pd, "by-slug", "By slug", JP_BODY);

    let out = export_ok(&pd, json!({ "doc_id": "by-slug" }));
    assert_pdf(out["pdf_path"].as_str().unwrap());
}

// ---- REQ-PDF-006 AC2: batch -------------------------------------------------

#[test]
fn batch_exports_one_pdf_per_document() {
    if !typst_available() {
        return;
    }
    let (_d, pd) = setup();
    let a = save_doc(&pd, "doc-a", "A", "# A\n\nalpha\n");
    let b = save_doc(&pd, "doc-b", "B", "# B\n\nbeta\n");
    let c = save_doc(&pd, "doc-c", "C", "# C\n\ngamma\n");

    let out = export_ok(&pd, json!({ "doc_ids": [a, b, c], "format": "pdf" }));

    assert_eq!(out["exported"], 3);
    assert_eq!(out["failed"], 0);
    let docs = out["documents"].as_array().unwrap();
    assert_eq!(docs.len(), 3);
    let slugs: Vec<&str> = docs.iter().map(|d| d["slug"].as_str().unwrap()).collect();
    assert_eq!(slugs, ["doc-a", "doc-b", "doc-c"], "input order is kept");
    for d in docs {
        assert_pdf(d["pdf_path"].as_str().unwrap());
    }
    assert!(out.get("pdf_path").is_none(), "pdf_path is single-doc only");
    assert_eq!(out["pdf_paths"].as_array().unwrap().len(), 3);
}

#[test]
fn batch_reports_a_bad_id_without_aborting_the_rest() {
    if !typst_available() {
        return;
    }
    let (_d, pd) = setup();
    let a = save_doc(&pd, "good-doc", "Good", "# Good\n\nbody\n");

    let out = export_ok(&pd, json!({ "doc_ids": [a, "ghost-doc"] }));

    assert_eq!(out["exported"], 1);
    assert_eq!(out["failed"], 1);
    let docs = out["documents"].as_array().unwrap();
    assert_pdf(docs[0]["pdf_path"].as_str().unwrap());
    assert_eq!(docs[1]["doc_id"], "ghost-doc");
    assert!(docs[1]["error"].as_str().unwrap().contains("ghost-doc"));
    assert!(docs[1].get("pdf_path").is_none());
}

#[test]
fn batch_where_every_document_fails_is_an_error() {
    let (_d, pd) = setup();
    let msg = export_err(&pd, json!({ "doc_ids": ["ghost-1", "ghost-2"] }));
    assert!(msg.contains("ghost-1") && msg.contains("ghost-2"), "{msg}");
}

#[test]
fn duplicate_ids_are_exported_once() {
    if !typst_available() {
        return;
    }
    let (_d, pd) = setup();
    let a = save_doc(&pd, "dup-doc", "Dup", "# Dup\n\nbody\n");
    let out = export_ok(&pd, json!({ "doc_ids": [a.clone(), a, "dup-doc"] }));
    assert_eq!(out["exported"], 1);
    assert_eq!(out["documents"].as_array().unwrap().len(), 1);
}

// ---- REQ-PDF-006 AC3: options ----------------------------------------------

#[test]
fn output_dir_relative_is_resolved_against_the_project() {
    if !typst_available() {
        return;
    }
    let (dir, pd) = setup();
    save_doc(&pd, "out-doc", "Out", "# Out\n\nbody\n");

    let out = export_ok(
        &pd,
        json!({ "doc_id": "out-doc", "output_dir": "build/pdfs" }),
    );

    let pdf = out["pdf_path"].as_str().unwrap();
    let expected = dir.path().join("build/pdfs/out-doc.pdf");
    assert_eq!(
        std::fs::canonicalize(pdf).unwrap(),
        std::fs::canonicalize(&expected).unwrap()
    );
    assert_pdf(pdf);
    assert!(
        !dir.path().join(".handoff/exports/out-doc.pdf").exists(),
        "the PDF lives in output_dir only"
    );
}

#[test]
fn output_dir_absolute_is_honoured() {
    if !typst_available() {
        return;
    }
    let (_d, pd) = setup();
    save_doc(&pd, "abs-doc", "Abs", "# Abs\n\nbody\n");
    let elsewhere = tempfile::tempdir().unwrap();
    let target: PathBuf = elsewhere.path().join("pdfs");

    let out = export_ok(
        &pd,
        json!({ "doc_id": "abs-doc", "output_dir": target.to_string_lossy() }),
    );

    assert_eq!(
        std::fs::canonicalize(out["pdf_path"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(target.join("abs-doc.pdf")).unwrap()
    );
}

#[test]
fn output_dir_that_is_a_file_is_an_error() {
    let (dir, pd) = setup();
    save_doc(&pd, "file-doc", "File", "# File\n\nbody\n");
    std::fs::write(dir.path().join("occupied"), "x").unwrap();
    let msg = export_err(
        &pd,
        json!({ "doc_id": "file-doc", "output_dir": "occupied" }),
    );
    assert!(
        msg.contains("output_dir") || msg.contains("occupied"),
        "{msg}"
    );
}

#[test]
fn template_name_selects_a_builtin_template() {
    if !typst_available() {
        return;
    }
    let (dir, pd) = setup();
    save_doc(&pd, "tpl-doc", "Tpl", "# Tpl\n\nbody\n");

    let out = export_ok(&pd, json!({ "doc_id": "tpl-doc", "template": "report" }));
    assert_pdf(out["pdf_path"].as_str().unwrap());
    let typ = std::fs::read_to_string(dir.path().join(".handoff/exports/tpl-doc.typ")).unwrap();
    assert!(typ.contains("templates/report.typ"), "{typ}");
}

#[test]
fn unknown_template_is_an_error_naming_it() {
    if !typst_available() {
        return;
    }
    let (_d, pd) = setup();
    save_doc(&pd, "tpl-bad", "Bad", "# Bad\n\nbody\n");
    let msg = export_err(
        &pd,
        json!({ "doc_id": "tpl-bad", "template": "nonexistent" }),
    );
    assert!(msg.contains("nonexistent"), "{msg}");
}

#[test]
fn include_toc_false_turns_the_table_of_contents_off() {
    if !typst_available() {
        return;
    }
    let (dir, pd) = setup();
    save_doc(&pd, "toc-doc", "Toc", "# Toc\n\n## Section\n\nbody\n");
    let typ_path = dir.path().join(".handoff/exports/toc-doc.typ");

    let out = export_ok(&pd, json!({ "doc_id": "toc-doc", "include_toc": false }));
    assert_pdf(out["pdf_path"].as_str().unwrap());
    let typ = std::fs::read_to_string(&typ_path).unwrap();
    assert!(typ.contains("show-toc: false"), "{typ}");

    let out = export_ok(&pd, json!({ "doc_id": "toc-doc", "include_toc": true }));
    assert_pdf(out["pdf_path"].as_str().unwrap());
    let typ = std::fs::read_to_string(&typ_path).unwrap();
    assert!(typ.contains("show-toc: true"), "{typ}");
}

#[test]
fn omitted_include_toc_leaves_the_template_default() {
    if !typst_available() {
        return;
    }
    let (dir, pd) = setup();
    save_doc(&pd, "toc-default", "Toc", "# Toc\n\nbody\n");
    export_ok(&pd, json!({ "doc_id": "toc-default" }));
    let typ = std::fs::read_to_string(dir.path().join(".handoff/exports/toc-default.typ")).unwrap();
    assert!(!typ.contains("show-toc"), "{typ}");
}

// ---- warnings (REQ-PDF-013 AC4) ---------------------------------------------

#[test]
fn conversion_warnings_are_returned_with_their_document() {
    if !typst_available() {
        return;
    }
    let (_d, pd) = setup();
    let doc_id = save_doc(
        &pd,
        "warn-doc",
        "Warn",
        "# Warn\n\n![missing](assets/not-there.png)\n",
    );

    let out = export_ok(&pd, json!({ "doc_id": "warn-doc" }));

    let warnings = out["warnings"].as_array().unwrap();
    let w = warnings
        .iter()
        .find(|w| w["code"] == "image_not_found")
        .unwrap_or_else(|| panic!("no image_not_found warning in {warnings:?}"));
    assert_eq!(w["doc_id"], doc_id);
    let doc_warnings = out["documents"][0]["warnings"].as_array().unwrap();
    assert_eq!(
        doc_warnings.len(),
        warnings.iter().filter(|w| w["doc_id"] == doc_id).count()
    );
}

// ---- REQ-PDF-014 AC4: exports/ must be git-ignored --------------------------

fn exports_warnings(out: &Value) -> Vec<&Value> {
    out["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "exports_not_gitignored")
        .collect()
}

#[test]
fn warns_once_when_exports_is_not_gitignored() {
    if !typst_available() {
        return;
    }
    let (_d, pd) = setup();
    save_doc(&pd, "ign-a", "A", "# A\n\ntext\n");
    save_doc(&pd, "ign-b", "B", "# B\n\ntext\n");

    let out = export_ok(&pd, json!({ "doc_ids": ["ign-a", "ign-b"] }));

    let found = exports_warnings(&out);
    assert_eq!(found.len(), 1, "{:?}", out["warnings"]);
    assert!(found[0].get("doc_id").is_none(), "{:?}", found[0]);
    assert!(found[0]["message"].as_str().unwrap().contains("exports"));
}

#[test]
fn no_gitignore_warning_when_exports_is_ignored() {
    if !typst_available() {
        return;
    }
    let (d, pd) = setup();
    std::fs::write(d.path().join(".handoff/.gitignore"), "/exports/\n").unwrap();
    save_doc(&pd, "ign-ok", "OK", "# OK\n\ntext\n");

    let out = export_ok(&pd, json!({ "doc_id": "ign-ok" }));

    assert!(exports_warnings(&out).is_empty(), "{:?}", out["warnings"]);
}

// ---- REQ-PDF-013 AC2: compile errors ---------------------------------------

#[test]
fn typst_compile_error_is_returned_and_no_pdf_is_left() {
    if !typst_available() {
        return;
    }
    let (dir, pd) = setup();
    save_doc(
        &pd,
        "broken-typst",
        "Broken",
        "# Broken\n\n```typst\n#let x = (\n```\n",
    );
    let msg = export_err(&pd, json!({ "doc_id": "broken-typst" }));
    assert!(msg.to_lowercase().contains("error"), "{msg}");
    assert!(!dir
        .path()
        .join(".handoff/exports/broken-typst.pdf")
        .exists());
}

// ---- REQ-PDF-007: handoff_report format=pdf ----------------------------------

fn report(pd: &str, mut args: Value) -> Value {
    args["project_dir"] = json!(pd);
    call("handoff_report", args)
}

fn report_ok(pd: &str, args: Value) -> Value {
    let resp = report(pd, args.clone());
    assert!(!is_error(&resp), "{args} failed: {}", text(&resp));
    serde_json::from_str(&text(&resp)).expect("tool output is JSON")
}

fn assert_report_pdf(pd: &str, out: &Value, report_type: &str) {
    let pdf = out["pdf"]["pdf_path"].as_str().expect("pdf.pdf_path");
    assert_pdf(pdf);
    let id = out["report"]["report_id"].as_str().unwrap();
    assert!(
        Path::new(pdf).file_stem().unwrap().to_string_lossy() == id,
        "{pdf} is named after report {id}"
    );
    assert!(out["pdf"]["warnings"].is_array());
    assert_eq!(out["report"]["report_type"], report_type);
    // The Markdown report is still stored as before.
    assert!(Path::new(pd)
        .join(".handoff")
        .join(out["report"]["output_path"].as_str().unwrap())
        .exists());
}

#[test]
fn weekly_report_can_be_rendered_as_pdf() {
    if !typst_available() {
        return;
    }
    let (_d, pd) = setup();
    let out = report_ok(
        &pd,
        json!({ "action": "generate", "report_type": "weekly", "format": "pdf" }),
    );
    assert_report_pdf(&pd, &out, "weekly");
}

#[test]
fn verification_report_can_be_rendered_as_pdf() {
    if !typst_available() {
        return;
    }
    let (_d, pd) = setup();
    let out = report_ok(
        &pd,
        json!({ "action": "generate", "report_type": "verification", "format": "pdf" }),
    );
    assert_report_pdf(&pd, &out, "verification");
}

#[test]
fn milestone_report_can_be_rendered_as_pdf() {
    if !typst_available() {
        return;
    }
    let (_d, pd) = setup();
    let resp = call(
        "handoff_add_milestone",
        json!({ "project_dir": pd, "name": "beta", "date": "2026-09-30", "description": "Public beta" }),
    );
    assert!(!is_error(&resp), "add_milestone failed: {}", text(&resp));
    let out = report_ok(
        &pd,
        json!({
            "action": "generate", "report_type": "milestone",
            "scope": { "milestone": "beta" }, "format": "pdf"
        }),
    );
    assert_report_pdf(&pd, &out, "milestone");
}

#[test]
fn report_pdf_honours_output_dir_and_include_toc() {
    if !typst_available() {
        return;
    }
    let (dir, pd) = setup();
    let out = report_ok(
        &pd,
        json!({
            "action": "generate", "report_type": "weekly", "format": "pdf",
            "output_dir": "deliverables", "include_toc": false
        }),
    );
    let pdf = out["pdf"]["pdf_path"].as_str().unwrap();
    assert_pdf(pdf);
    assert!(
        std::fs::canonicalize(pdf)
            .unwrap()
            .starts_with(std::fs::canonicalize(dir.path().join("deliverables")).unwrap()),
        "{pdf}"
    );
}

#[test]
fn existing_report_can_be_rendered_with_get() {
    if !typst_available() {
        return;
    }
    let (_d, pd) = setup();
    let generated = report_ok(
        &pd,
        json!({ "action": "generate", "report_type": "weekly" }),
    );
    assert!(generated.get("pdf").is_none(), "markdown is the default");
    let id = generated["report"]["report_id"].as_str().unwrap();

    let got = report_ok(
        &pd,
        json!({ "action": "get", "report_id": id, "format": "pdf" }),
    );
    assert!(got["body"].is_string(), "get keeps its normal fields");
    assert_pdf(got["pdf"]["pdf_path"].as_str().unwrap());
}

#[test]
fn report_format_markdown_is_the_unchanged_default() {
    let (_d, pd) = setup();
    let out = report_ok(
        &pd,
        json!({ "action": "generate", "report_type": "weekly", "format": "markdown" }),
    );
    assert!(out.get("pdf").is_none());
}

#[test]
fn report_rejects_an_unknown_format() {
    let (_d, pd) = setup();
    let resp = report(
        &pd,
        json!({ "action": "generate", "report_type": "weekly", "format": "docx" }),
    );
    assert!(is_error(&resp));
    assert!(text(&resp).contains("docx"));
}

#[test]
fn pdf_options_without_pdf_format_are_rejected() {
    let (_d, pd) = setup();
    let resp = report(
        &pd,
        json!({ "action": "generate", "report_type": "weekly", "output_dir": "x" }),
    );
    assert!(is_error(&resp), "{}", text(&resp));
    assert!(text(&resp).contains("output_dir"));
}

#[test]
fn format_pdf_is_refused_for_actions_that_do_not_render() {
    let (_d, pd) = setup();
    let resp = report(&pd, json!({ "action": "list", "format": "pdf" }));
    assert!(is_error(&resp), "{}", text(&resp));
    assert!(text(&resp).contains("format"));
}
