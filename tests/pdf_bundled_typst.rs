//! REQ-PDF-040: with the `bundled-typst` feature the PDF is compiled by the
//! Typst compiler inside the binary — no `typst` executable is involved. The
//! path handed to the `*_with` entry points is deliberately one that does not
//! exist, so any attempt to spawn it fails the test.
#![cfg(feature = "bundled-typst")]

use std::path::{Path, PathBuf};

use handoff_mcp::storage::docs::pdf::export::{export_pdf_with, ExportOutcome, ExportRequest};
use handoff_mcp::storage::docs::pdf::template::{detect_fonts, CoverMeta};
use handoff_mcp::storage::docs::pdf::types::PdfWarningCode;

const NO_SUCH_TYPST: &str = "/nonexistent/typst";

struct Project {
    tmp: tempfile::TempDir,
}

impl Project {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let assets = tmp.path().join("assets");
        std::fs::create_dir_all(&assets).unwrap();
        std::fs::create_dir_all(tmp.path().join(".handoff")).unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pdf/assets");
        std::fs::copy(fixtures.join("test.png"), assets.join("test.png")).unwrap();
        Self { tmp }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    fn handoff(&self) -> PathBuf {
        self.root().join(".handoff")
    }

    fn export(
        &self,
        slug: &str,
        markdown: &str,
        template: Option<&str>,
    ) -> anyhow::Result<ExportOutcome> {
        let handoff = self.handoff();
        export_pdf_with(
            &ExportRequest {
                project_root: self.root(),
                handoff_dir: &handoff,
                doc_slug: slug,
                markdown,
                template,
                meta: CoverMeta {
                    title: "Bundled".into(),
                    date: Some("2026-10-28".into()),
                    ..CoverMeta::default()
                },
                req_meta: None,
            },
            Path::new(NO_SUCH_TYPST),
            None,
        )
    }
}

#[test]
fn exports_a_pdf_without_any_typst_executable() {
    let project = Project::new();
    let out = project
        .export("plain", "# 見出し\n\nHello, world.\n", None)
        .expect("bundled export");
    let pdf = std::fs::read(&out.pdf_path).unwrap();
    assert!(pdf.starts_with(b"%PDF-"));
    assert!(pdf.windows(5).any(|w| w == b"%%EOF"), "complete PDF");
    assert_eq!(out.pdf_path, project.handoff().join("exports/plain.pdf"));
}

#[test]
fn embeds_a_root_absolute_image() {
    let project = Project::new();
    let out = project
        .export("img", "# Image\n\n![logo](/assets/test.png)\n", None)
        .expect("export with image");
    assert!(
        !out.warnings
            .iter()
            .any(|w| w.code == PdfWarningCode::ImageNotFound),
        "{:?}",
        out.warnings
    );
    let pdf = std::fs::read(&out.pdf_path).unwrap();
    let has = |needle: &[u8]| pdf.windows(needle.len()).any(|w| w == needle);
    assert!(
        has(b"/Subtype/Image") || has(b"/Subtype /Image"),
        "PNG must be embedded"
    );
}

#[test]
fn a_custom_template_from_handoff_templates_is_compiled() {
    let project = Project::new();
    // Generates default.typ, report.typ, spec.typ under .handoff/templates/.
    project.export("seed", "# seed\n", None).unwrap();
    let templates = project.handoff().join("templates");
    let mut custom = std::fs::read_to_string(templates.join("default.typ")).unwrap();
    custom.push_str("\n#let custom-marker = 1\n");
    std::fs::write(templates.join("mine.typ"), custom).unwrap();

    let out = project
        .export("custom", "# Custom\n\nbody\n", Some("mine"))
        .expect("custom template export");
    let typ = std::fs::read_to_string(&out.typ_path).unwrap();
    assert!(typ.contains("/.handoff/templates/mine.typ"), "{typ}");
    assert!(std::fs::read(&out.pdf_path).unwrap().starts_with(b"%PDF-"));
}

#[test]
fn a_broken_custom_template_fails_with_its_own_file_and_line() {
    let project = Project::new();
    project.export("seed", "# seed\n", None).unwrap();
    let templates = project.handoff().join("templates");
    let mut broken = std::fs::read_to_string(templates.join("default.typ")).unwrap();
    broken.push_str("\n#no-such-helper()\n");
    std::fs::write(templates.join("broken.typ"), broken).unwrap();

    let err = project
        .export("broken", "# Broken\n", Some("broken"))
        .expect_err("a template that does not compile must fail the export");
    let msg = format!("{err:#}");
    assert!(msg.contains("/.handoff/templates/broken.typ:"), "{msg}");
    assert!(msg.contains("unknown variable"), "{msg}");
    assert!(
        !project.handoff().join("exports/broken.pdf").exists(),
        "no PDF on failure"
    );
}

#[test]
fn a_compile_error_is_reported_with_typsts_diagnostics_and_leaves_no_stale_pdf() {
    let project = Project::new();
    let first = project.export("doc", "# ok\n", None).unwrap();
    assert!(first.pdf_path.is_file());

    let err = project
        .export("doc", "# bad\n\n```typst\n#unknown-function()\n```\n", None)
        .expect_err("invalid Typst must fail");
    let msg = format!("{err:#}");
    assert!(msg.contains("no PDF was produced"), "{msg}");
    assert!(msg.contains("error: unknown variable"), "{msg}");
    assert!(msg.contains("/.handoff/exports/doc.typ:"), "{msg}");
    assert!(!first.pdf_path.exists(), "stale PDF left behind");
}

#[test]
fn font_detection_needs_no_typst_executable() {
    // The explicit path is ignored: the families come from the compiler's own
    // font search. What it selects is covered by
    // tests/pdf_bundled_typst_fonts.rs (a Japanese family in TYPST_FONT_PATHS).
    detect_fonts(Path::new(NO_SUCH_TYPST)).expect("font search");
}
