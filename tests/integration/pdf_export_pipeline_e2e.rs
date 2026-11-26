//! End-to-end: `export_pdf` (REQ-PDF-001 / REQ-PDF-011) with the real Typst
//! CLI, a real PNG and, where a working `mmdc` exists, real Mermaid diagrams.
//!
//! A plain `cargo test` works on a bare machine: missing tools print a notice
//! and the test passes. Set `HANDOFF_REQUIRE_TYPST=1` / `HANDOFF_REQUIRE_MMDC=1`
//! (as CI should) to turn "tool missing" into a failure.

use std::path::{Path, PathBuf};

use handoff_mcp::storage::docs::pdf::deps::{detect, Tool, ToolStatus};
use handoff_mcp::storage::docs::pdf::export::{export_pdf, ExportRequest};
use handoff_mcp::storage::docs::pdf::template::CoverMeta;
use handoff_mcp::storage::docs::pdf::types::PdfWarningCode;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pdf")
}

fn require(var: &str) -> bool {
    std::env::var_os(var).is_some_and(|v| !v.is_empty() && v != "0")
}

fn typst_available() -> bool {
    match detect(Tool::Typst) {
        ToolStatus::Found { .. } => true,
        other => {
            assert!(
                !require("HANDOFF_REQUIRE_TYPST"),
                "HANDOFF_REQUIRE_TYPST is set but Typst is unusable: {other:?}"
            );
            eprintln!("SKIPPED: typst not available ({other:?})");
            false
        }
    }
}

/// A project directory holding the fixture assets, with `.handoff/` inside.
fn project() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let handoff = root.join(".handoff");
    std::fs::create_dir_all(&handoff).unwrap();
    let assets = root.join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    for name in ["test.png", "test.svg"] {
        std::fs::copy(fixtures().join("assets").join(name), assets.join(name)).unwrap();
    }
    (tmp, root, handoff)
}

fn meta() -> CoverMeta {
    CoverMeta {
        title: "パイプライン E2E".into(),
        date: Some("2026-10-27".into()),
        ..CoverMeta::default()
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn exports_pdf_with_embedded_png_and_regenerates_typ() {
    if !typst_available() {
        return;
    }
    let (_tmp, root, handoff) = project();
    let md = std::fs::read_to_string(fixtures().join("images.md")).unwrap();
    let request = ExportRequest {
        req_meta: None,
        project_root: &root,
        handoff_dir: &handoff,
        doc_slug: "images",
        markdown: &md,
        template: None,
        meta: meta(),
    };

    let out = export_pdf(&request).expect("export");
    assert_eq!(out.pdf_path, handoff.join("exports/images.pdf"));
    assert_eq!(out.typ_path, handoff.join("exports/images.typ"));
    assert!(out.typst_version.is_some(), "version recorded");
    assert!(
        !out.warnings
            .iter()
            .any(|w| w.code == PdfWarningCode::ImageNotFound),
        "{:?}",
        out.warnings
    );

    let pdf = std::fs::read(&out.pdf_path).unwrap();
    assert!(pdf.starts_with(b"%PDF-"));
    // Typst writes `/Subtype/Image` (no space); accept either spelling.
    assert!(
        contains(&pdf, b"/Subtype/Image") || contains(&pdf, b"/Subtype /Image"),
        "PNG must be embedded"
    );

    // The .typ is regenerated, not appended to or kept.
    let first = std::fs::read_to_string(&out.typ_path).unwrap();
    let second_md = "# 別の文書\n\n本文\n";
    let out2 = export_pdf(&ExportRequest {
        req_meta: None,
        markdown: second_md,
        ..request
    })
    .expect("re-export");
    let second = std::fs::read_to_string(&out2.typ_path).unwrap();
    assert_ne!(first, second);
    assert!(second.contains("別の文書") && !second.contains("assets/test.png"));
}

#[test]
fn typst_compile_error_is_reported_and_leaves_no_pdf() {
    if !typst_available() {
        return;
    }
    let (_tmp, root, handoff) = project();
    let md = "# t\n\n```typst\n#unknown-function()\n```\n";
    let err = export_pdf(&ExportRequest {
        req_meta: None,
        project_root: &root,
        handoff_dir: &handoff,
        doc_slug: "bad",
        markdown: md,
        template: None,
        meta: meta(),
    })
    .expect_err("must fail");
    let msg = err.to_string();
    assert!(
        msg.contains("unknown-function") || msg.contains("unknown variable"),
        "{msg}"
    );
    assert!(
        msg.contains("bad.typ"),
        "diagnostics carry file:line: {msg}"
    );
    assert!(!handoff.join("exports/bad.pdf").exists());
}

#[test]
fn mermaid_diagrams_become_figures_and_svg_cache_is_cleaned() {
    if !typst_available() {
        return;
    }
    if !matches!(detect(Tool::Mmdc), ToolStatus::Found { .. }) {
        assert!(
            !require("HANDOFF_REQUIRE_MMDC"),
            "HANDOFF_REQUIRE_MMDC is set but mmdc is missing"
        );
        eprintln!("SKIPPED: mmdc not available");
        return;
    }
    let (_tmp, root, handoff) = project();
    let md = std::fs::read_to_string(fixtures().join("mermaid_diagrams.md")).unwrap();
    let out = export_pdf(&ExportRequest {
        req_meta: None,
        project_root: &root,
        handoff_dir: &handoff,
        doc_slug: "diagrams",
        markdown: &md,
        template: None,
        meta: meta(),
    })
    .expect("export");

    let mermaid_failed = out.warnings.iter().any(|w| {
        matches!(
            w.code,
            PdfWarningCode::MermaidUnavailable | PdfWarningCode::MermaidSyntaxError
        )
    });
    if mermaid_failed {
        // mmdc is installed but its Chrome cannot launch here.
        assert!(
            !require("HANDOFF_REQUIRE_MMDC"),
            "HANDOFF_REQUIRE_MMDC is set but mmdc cannot render: {:?}",
            out.warnings
        );
        eprintln!("SKIPPED: mmdc cannot render here: {:?}", out.warnings);
        return;
    }

    let typ = std::fs::read_to_string(&out.typ_path).unwrap();
    assert_eq!(
        typ.matches("/.handoff/exports/.mermaid-cache/").count(),
        3,
        "{typ}"
    );
    assert!(std::fs::metadata(&out.pdf_path).unwrap().len() > 0);
    assert!(
        !handoff.join("exports/.mermaid-cache").exists(),
        "generated SVGs are removed after the PDF is built"
    );
}
