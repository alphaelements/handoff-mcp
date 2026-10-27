//! REQ-PDF-007 follow-up: a report generated earlier can be re-rendered as a
//! PDF at any later time with `handoff_report action=get format=pdf`, driven
//! through the real JSON-RPC entry point.
//!
//! The rendering tests need the Typst CLI; without it they print `SKIPPED`
//! and pass, unless `HANDOFF_REQUIRE_TYPST` is set. The argument-validation
//! tests never need Typst.

use handoff_mcp::storage::docs::pdf::deps::{detect, Tool, ToolStatus};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn call(arguments: Value) -> Value {
    let req = json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "tools/call",
        "params": { "name": "handoff_report", "arguments": arguments }
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

fn report(pd: &str, mut args: Value) -> Value {
    args["project_dir"] = json!(pd);
    call(args)
}

fn report_ok(pd: &str, args: Value) -> Value {
    let resp = report(pd, args.clone());
    assert!(!is_error(&resp), "{args} failed: {}", text(&resp));
    serde_json::from_str(&text(&resp)).expect("tool output is JSON")
}

fn report_err(pd: &str, args: Value) -> String {
    let resp = report(pd, args.clone());
    assert!(is_error(&resp), "{args} should fail: {}", text(&resp));
    text(&resp)
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
    let req = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "handoff_init",
                    "arguments": { "project_dir": pd, "project_name": "get-pdf" } }
    });
    let out = handoff_mcp::mcp::protocol::process_line(&req.to_string()).expect("response");
    let resp: Value = serde_json::from_str(&out).unwrap();
    assert!(!is_error(&resp), "init failed: {}", text(&resp));
    (dir, pd)
}

fn assert_pdf(path: &str) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    assert!(bytes.starts_with(b"%PDF-"), "{path} is not a PDF");
}

/// Generates a weekly report for `period` carrying `marker` in its AI summary.
fn generate_weekly(pd: &str, period: &str, marker: &str) -> String {
    let out = report_ok(
        pd,
        json!({
            "action": "generate", "report_type": "weekly",
            "scope": { "period": period },
            "data": { "ai_summary": marker }
        }),
    );
    out["report"]["report_id"].as_str().unwrap().to_string()
}

fn report_ids(pd: &str) -> Vec<String> {
    let mut ids: Vec<String> = report_ok(pd, json!({ "action": "list" }))["reports"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["report_id"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    ids
}

#[test]
fn an_older_report_is_rendered_from_its_stored_body_not_regenerated() {
    if !typst_available() {
        return;
    }
    let (dir, pd) = setup();
    let old = generate_weekly(&pd, "2026-W30", "OLDMARKER summary of July");
    // Later reports exist; the old one must still be the one rendered.
    let newer = generate_weekly(&pd, "2026-W41", "NEWMARKER summary of October");
    assert_ne!(old, newer);

    let before = report_ok(&pd, json!({ "action": "get", "report_id": old }));
    let ids_before = report_ids(&pd);

    let got = report_ok(
        &pd,
        json!({ "action": "get", "report_id": old, "format": "pdf" }),
    );
    let pdf_path = got["pdf"]["pdf_path"].as_str().unwrap();
    assert_pdf(pdf_path);
    assert_eq!(
        Path::new(pdf_path).file_name().unwrap().to_string_lossy(),
        format!("{old}.pdf")
    );
    assert!(
        std::fs::canonicalize(pdf_path)
            .unwrap()
            .starts_with(std::fs::canonicalize(dir.path().join(".handoff/exports")).unwrap()),
        "{pdf_path}"
    );

    // The Typst source was built from the stored body of the OLD report.
    let typ = std::fs::read_to_string(got["pdf"]["typ_path"].as_str().unwrap()).unwrap();
    assert!(typ.contains("OLDMARKER"), "{typ}");
    assert!(!typ.contains("NEWMARKER"), "{typ}");

    // Nothing was regenerated, re-versioned or re-statused.
    assert_eq!(got["report"], before["report"]);
    assert_eq!(got["body"], before["body"]);
    assert_eq!(report_ids(&pd), ids_before);
    let after = report_ok(&pd, json!({ "action": "get", "report_id": old }));
    assert_eq!(after, before);
}

#[test]
fn published_reports_keep_their_status_when_rendered() {
    if !typst_available() {
        return;
    }
    let (_d, pd) = setup();
    let id = generate_weekly(&pd, "2026-W30", "approved summary");
    report_ok(&pd, json!({ "action": "submit", "report_id": id }));
    report_ok(
        &pd,
        json!({ "action": "approve", "report_id": id, "reviewer": "alice" }),
    );
    report_ok(&pd, json!({ "action": "publish", "report_id": id }));
    let before = report_ok(&pd, json!({ "action": "get", "report_id": id }));
    assert_eq!(before["report"]["status"], "published");

    let got = report_ok(
        &pd,
        json!({ "action": "get", "report_id": id, "format": "pdf" }),
    );
    assert_pdf(got["pdf"]["pdf_path"].as_str().unwrap());
    assert_eq!(got["report"], before["report"]);
}

#[test]
fn get_pdf_honours_output_dir_and_cover_options() {
    if !typst_available() {
        return;
    }
    let (dir, pd) = setup();
    let id = generate_weekly(&pd, "2026-W30", "cover summary");
    let got = report_ok(
        &pd,
        json!({
            "action": "get", "report_id": id, "format": "pdf",
            "output_dir": "archive", "include_toc": false,
            "cover_version": "v9.9", "cover_status": "Archived",
            "cover_organization": "Acme Corp"
        }),
    );
    let pdf = got["pdf"]["pdf_path"].as_str().unwrap();
    assert_pdf(pdf);
    assert!(
        std::fs::canonicalize(pdf)
            .unwrap()
            .starts_with(std::fs::canonicalize(dir.path().join("archive")).unwrap()),
        "{pdf}"
    );
    let typ = std::fs::read_to_string(got["pdf"]["typ_path"].as_str().unwrap()).unwrap();
    for needle in ["v9.9", "Archived", "Acme Corp"] {
        assert!(typ.contains(needle), "{needle} missing from {typ}");
    }
}

#[test]
fn get_pdf_works_for_the_scheduled_report_types() {
    if !typst_available() {
        return;
    }
    let (_d, pd) = setup();
    let cases = [
        ("weekly", json!({ "period": "2026-W30" })),
        ("monthly", json!({ "period": "2026-07" })),
        ("daily", json!({ "period": "2026-07-22" })),
        ("quarterly", json!({ "period": "2026-Q3" })),
        ("yearly", json!({ "period": "2025" })),
    ];
    for (report_type, scope) in cases {
        let out = report_ok(
            &pd,
            json!({ "action": "generate", "report_type": report_type, "scope": scope }),
        );
        let id = out["report"]["report_id"].as_str().unwrap();
        let got = report_ok(
            &pd,
            json!({ "action": "get", "report_id": id, "format": "pdf" }),
        );
        assert_pdf(got["pdf"]["pdf_path"].as_str().unwrap());
    }
}

#[test]
fn get_pdf_for_an_unknown_report_fails_and_writes_nothing() {
    let (dir, pd) = setup();
    let msg = report_err(
        &pd,
        json!({ "action": "get", "report_id": "weekly-does-not-exist", "format": "pdf" }),
    );
    assert!(msg.contains("weekly-does-not-exist"), "{msg}");
    let exports: PathBuf = dir.path().join(".handoff").join("exports");
    assert!(
        !exports.exists() || std::fs::read_dir(&exports).unwrap().next().is_none(),
        "no PDF may be written for a missing report"
    );
}

#[test]
fn get_pdf_rejects_unknown_formats_and_pdf_options_without_pdf() {
    let (_d, pd) = setup();
    let id = generate_weekly(&pd, "2026-W30", "x");
    let msg = report_err(
        &pd,
        json!({ "action": "get", "report_id": id, "format": "docx" }),
    );
    assert!(msg.contains("docx"), "{msg}");
    let msg = report_err(
        &pd,
        json!({ "action": "get", "report_id": id, "output_dir": "x" }),
    );
    assert!(msg.contains("output_dir"), "{msg}");
}
