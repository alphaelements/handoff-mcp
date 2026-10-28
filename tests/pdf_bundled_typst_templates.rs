//! Template compatibility gate for the bundled Typst compiler (REQ-PDF-041,
//! wiki/390-typst-library-bundling.md §6 Layer 3).
//!
//! Compiles every built-in template with a minimal document through the public
//! export pipeline and checks the PDF is non-empty. When Dependabot bumps the
//! `typst*` crates, this is the test that proves the embedded templates still
//! compile on the new compiler. Unlike the CLI-based E2E suites it never skips:
//! the compiler is linked in, so there is no tool that could be "missing".
//!
//! Run with `cargo test --features bundled-typst --test pdf_bundled_typst_templates`.

#![cfg(feature = "bundled-typst")]

use handoff_mcp::storage::docs::pdf::export::{export_pdf, ExportRequest};
use handoff_mcp::storage::docs::pdf::template::CoverMeta;

/// The built-in template names `prepare_template` resolves without a file.
const BUILTIN_TEMPLATES: [Option<&str>; 3] = [None, Some("report"), Some("spec")];

fn compile_minimal(template: Option<&str>) -> Vec<u8> {
    let tmp = tempfile::tempdir().unwrap();
    let handoff = tmp.path().join(".handoff");
    std::fs::create_dir_all(&handoff).unwrap();
    let request = ExportRequest {
        req_meta: None,
        project_root: tmp.path(),
        handoff_dir: &handoff,
        doc_slug: "minimal",
        markdown: "# Test\n\nHello\n",
        template,
        meta: CoverMeta {
            title: "Template compatibility".into(),
            date: Some("2026-10-28".into()),
            ..CoverMeta::default()
        },
    };
    let outcome = export_pdf(&request)
        .unwrap_or_else(|e| panic!("template {template:?} failed to compile: {e:#}"));
    std::fs::read(&outcome.pdf_path).expect("read exported pdf")
}

#[test]
fn builtin_templates_compile_to_non_empty_pdf() {
    for template in BUILTIN_TEMPLATES {
        let pdf = compile_minimal(template);
        assert!(!pdf.is_empty(), "template {template:?} produced empty PDF");
        assert!(
            pdf.starts_with(b"%PDF-"),
            "template {template:?} output is not a PDF"
        );
    }
}
