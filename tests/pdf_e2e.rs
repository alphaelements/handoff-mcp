//! End-to-end PDF export (t-pdf-export.9; REQ-PDF-001/002/003/010/011/013).
//!
//! Drives the real pipeline ([`export_pdf_with`]) with a real Typst CLI, and a
//! real `mmdc` where one is usable, over every fixture in
//! `tests/fixtures/pdf/`, then inspects the resulting PDFs.
//!
//! Tool availability (a plain `cargo test` must work on a bare machine, CI
//! must not silently skip):
//! * `typst` missing -> tests print `SKIPPED` and pass, unless
//!   `HANDOFF_REQUIRE_TYPST=1`, which turns it into a failure.
//! * a working `mmdc` + Chrome missing -> the Mermaid-rendering tests skip,
//!   unless `HANDOFF_REQUIRE_MMDC=1`.
//! * poppler's `pdfinfo` / `pdftotext` missing -> page-count / text checks
//!   are skipped, unless `HANDOFF_REQUIRE_PDFTOOLS=1`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use handoff_mcp::storage::docs::pdf::deps::{detect, detect_in, Tool, ToolStatus};
use handoff_mcp::storage::docs::pdf::export::{
    export_pdf_with, ExportOutcome, ExportRequest, EXPORTS_SUBDIR,
};
use handoff_mcp::storage::docs::pdf::template::CoverMeta;
use handoff_mcp::storage::docs::pdf::types::{PdfWarning, PdfWarningCode};

// ---- tool discovery -----------------------------------------------------

fn require_env(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty() && v != "0")
}

fn typst_or_skip() -> Option<PathBuf> {
    match detect(Tool::Typst) {
        ToolStatus::Found { path, .. } => Some(path),
        other => {
            assert!(
                !require_env("HANDOFF_REQUIRE_TYPST"),
                "HANDOFF_REQUIRE_TYPST is set but Typst is unusable: {other:?}"
            );
            eprintln!("SKIPPED: typst not available ({other:?})");
            None
        }
    }
}

/// A `mmdc` that can really render a diagram here (installed *and* its
/// headless Chrome launches), probed once.
fn working_mmdc() -> Option<&'static Path> {
    static PROBE: OnceLock<Option<PathBuf>> = OnceLock::new();
    PROBE
        .get_or_init(|| {
            let ToolStatus::Found { path, .. } = detect(Tool::Mmdc) else {
                return None;
            };
            // mmdc's bundled puppeteer wants its own Chrome download; reuse a
            // system one when the user has not pointed it anywhere.
            if std::env::var_os("PUPPETEER_EXECUTABLE_PATH").is_none() {
                if let ToolStatus::Found { path: chrome, .. } = detect(Tool::ChromeHeadlessShell) {
                    std::env::set_var("PUPPETEER_EXECUTABLE_PATH", chrome);
                }
            }
            let dir = tempfile::tempdir().ok()?;
            let input = dir.path().join("probe.mmd");
            let output = dir.path().join("probe.svg");
            std::fs::write(&input, "flowchart TD\n  A-->B\n").ok()?;
            let ok = Command::new(&path)
                .arg("-i")
                .arg(&input)
                .arg("-o")
                .arg(&output)
                .arg("-q")
                .output()
                .ok()?
                .status
                .success()
                && output.is_file();
            ok.then_some(path)
        })
        .as_deref()
}

fn mmdc_or_skip() -> Option<&'static Path> {
    let mmdc = working_mmdc();
    if mmdc.is_none() {
        assert!(
            !require_env("HANDOFF_REQUIRE_MMDC"),
            "HANDOFF_REQUIRE_MMDC is set but mmdc cannot render diagrams here"
        );
        eprintln!("SKIPPED: no working mmdc (+ Chrome)");
    }
    mmdc
}

// ---- PDF inspection -----------------------------------------------------

fn poppler(tool: &str, args: &[&Path]) -> Option<String> {
    match Command::new(tool).args(args).output() {
        Ok(o) if o.status.success() => Some(String::from_utf8_lossy(&o.stdout).into_owned()),
        _ => {
            assert!(
                !require_env("HANDOFF_REQUIRE_PDFTOOLS"),
                "HANDOFF_REQUIRE_PDFTOOLS is set but `{tool}` is unusable"
            );
            None
        }
    }
}

/// Page count via `pdfinfo`, `None` when poppler is not installed.
fn page_count(pdf: &Path) -> Option<usize> {
    let info = poppler("pdfinfo", &[pdf])?;
    info.lines()
        .find_map(|l| l.strip_prefix("Pages:"))
        .map(|n| n.trim().parse().expect("pdfinfo Pages: not a number"))
}

/// Extracted text with all whitespace removed (CJK lines wrap anywhere, and
/// pdftotext may or may not put spaces at the breaks), `None` without poppler.
fn pdf_text(pdf: &Path) -> Option<String> {
    let text = poppler("pdftotext", &[pdf, Path::new("-")])?;
    Some(text.split_whitespace().collect())
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect()
}

/// Asserts the PDF is a non-trivial, well-formed file (size, magic, pages).
fn assert_valid_pdf(pdf: &Path) {
    let bytes = std::fs::read(pdf).unwrap_or_else(|e| panic!("{}: {e}", pdf.display()));
    assert!(!bytes.is_empty(), "{} is empty", pdf.display());
    assert!(
        bytes.starts_with(b"%PDF-"),
        "{} lacks the %PDF- magic",
        pdf.display()
    );
    assert!(
        bytes.windows(5).rev().take(1024).any(|w| w == b"%%EOF"),
        "{} lacks the %%EOF trailer",
        pdf.display()
    );
    if let Some(pages) = page_count(pdf) {
        assert!(pages > 0, "{} has no pages", pdf.display());
    }
}

// ---- project scaffolding ------------------------------------------------

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pdf")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A scratch project whose root holds the fixtures (so the fixtures' relative
/// `assets/...` references resolve) plus an empty `.handoff/`.
struct Project {
    dir: tempfile::TempDir,
}

impl Project {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        copy_dir(&fixtures_dir(), dir.path());
        std::fs::create_dir_all(dir.path().join(".handoff")).unwrap();
        Self { dir }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn handoff(&self) -> PathBuf {
        self.root().join(".handoff")
    }

    fn export(
        &self,
        fixture: &str,
        template: Option<&str>,
        typst: &Path,
        mmdc: Option<&Path>,
    ) -> Result<ExportOutcome, anyhow::Error> {
        let markdown = std::fs::read_to_string(fixtures_dir().join(fixture)).unwrap();
        let slug = fixture.trim_end_matches(".md");
        let handoff = self.handoff();
        export_pdf_with(
            &ExportRequest {
                project_root: self.root(),
                handoff_dir: &handoff,
                doc_slug: slug,
                markdown: &markdown,
                template,
                meta: CoverMeta {
                    title: format!("E2E {slug}"),
                    author: Some("handoff-mcp".into()),
                    date: Some("2026-10-27".into()),
                    ..CoverMeta::default()
                },
            },
            typst,
            mmdc,
        )
    }

    fn export_ok(
        &self,
        fixture: &str,
        template: Option<&str>,
        typst: &Path,
        mmdc: Option<&Path>,
    ) -> ExportOutcome {
        self.export(fixture, template, typst, mmdc)
            .unwrap_or_else(|e| panic!("export of {fixture} failed: {e:#}"))
    }
}

fn codes(warnings: &[PdfWarning]) -> Vec<PdfWarningCode> {
    warnings.iter().map(|w| w.code).collect()
}

fn fixture_names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(fixtures_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".md"))
        .collect();
    names.sort();
    assert!(names.len() >= 6, "fixtures went missing: {names:?}");
    names
}

fn assert_mermaid_cache_clean(project: &Project) {
    let cache = project
        .handoff()
        .join(EXPORTS_SUBDIR)
        .join(".mermaid-cache");
    let leftovers = std::fs::read_dir(&cache).map(|rd| rd.count()).unwrap_or(0);
    assert_eq!(leftovers, 0, "temporary Mermaid files left in {cache:?}");
}

// ---- criterion: every fixture exports, PDFs are sound ---------------------

#[test]
fn every_fixture_exports_to_a_valid_pdf() {
    let Some(typst) = typst_or_skip() else { return };
    let project = Project::new();
    // Real mmdc when it works, so mermaid_diagrams.md goes through the whole
    // chain; otherwise the fallback path (covered separately below).
    let mmdc = working_mmdc();
    for fixture in fixture_names() {
        let outcome = project.export_ok(&fixture, None, &typst, mmdc);
        assert!(outcome.typ_path.is_file(), "{fixture}: no .typ written");
        assert_eq!(
            outcome.pdf_path,
            project
                .handoff()
                .join(EXPORTS_SUBDIR)
                .join(fixture.replace(".md", ".pdf"))
        );
        assert_valid_pdf(&outcome.pdf_path);
    }
    assert_mermaid_cache_clean(&project);
}

#[test]
fn empty_document_still_yields_a_cover_page_pdf() {
    let Some(typst) = typst_or_skip() else { return };
    let project = Project::new();
    let outcome = project.export_ok("empty.md", None, &typst, None);
    assert!(
        outcome.warnings.is_empty(),
        "unexpected: {:?}",
        outcome.warnings
    );
    assert_valid_pdf(&outcome.pdf_path);
    if let Some(text) = pdf_text(&outcome.pdf_path) {
        assert!(text.contains("E2Eempty"), "cover title missing: {text}");
    }
}

// ---- criterion: Japanese text is really in the PDF -------------------------

#[test]
fn japanese_text_is_extractable_from_the_pdf() {
    let Some(typst) = typst_or_skip() else { return };
    let project = Project::new();
    let outcome = project.export_ok("basic_japanese.md", None, &typst, None);
    assert!(
        outcome.warnings.is_empty(),
        "unexpected: {:?}",
        outcome.warnings
    );
    assert_valid_pdf(&outcome.pdf_path);
    let Some(text) = pdf_text(&outcome.pdf_path) else {
        eprintln!("SKIPPED text check: pdftotext not installed");
        return;
    };
    for needle in [
        "見出し1日本語ドキュメントのテスト",
        "吾輩は猫である",
        "強調（太字）",
        "項目その1",
        "入れ子の手順2-2",
    ] {
        assert!(
            text.contains(&squash(needle)),
            "{needle:?} missing:\n{text}"
        );
    }
    // Frontmatter is metadata, not body text.
    assert!(
        !text.contains("title:日本語基本ドキュメント"),
        "frontmatter leaked into the body:\n{text}"
    );
}

#[test]
fn typst_passthrough_block_is_compiled_not_printed() {
    let Some(typst) = typst_or_skip() else { return };
    let project = Project::new();
    let outcome = project.export_ok("typst_passthrough.md", None, &typst, None);
    assert!(
        outcome.warnings.is_empty(),
        "unexpected: {:?}",
        outcome.warnings
    );
    assert_valid_pdf(&outcome.pdf_path);
    let Some(text) = pdf_text(&outcome.pdf_path) else {
        return;
    };
    // The table cells and figure caption are rendered content...
    for needle in ["プルアップ抵抗", "システム構成", "パススルー後の通常段落"]
    {
        assert!(text.contains(needle), "{needle:?} missing:\n{text}");
    }
    // ...while the Typst source itself must not appear as literal text.
    assert!(!text.contains("#table("), "passthrough printed as code");
    assert!(!text.contains("#figure("), "passthrough printed as code");
}

// ---- criterion: images -------------------------------------------------

#[test]
fn images_fixture_embeds_png_and_svg() {
    let Some(typst) = typst_or_skip() else { return };
    let project = Project::new();
    let outcome = project.export_ok("images.md", None, &typst, None);
    assert!(
        outcome.warnings.is_empty(),
        "unexpected: {:?}",
        outcome.warnings
    );
    assert_valid_pdf(&outcome.pdf_path);
    // The .typ references both assets with root-absolute paths.
    let typ = std::fs::read_to_string(&outcome.typ_path).unwrap();
    assert!(
        typ.contains("image(\"/assets/test.png\"") && typ.contains("image(\"/assets/test.svg\"")
    );
}

// ---- criterion: Mermaid diagrams embedded (needs a working mmdc) ----------

#[test]
fn mermaid_diagrams_are_embedded_as_images_with_labels() {
    let Some(typst) = typst_or_skip() else { return };
    let Some(mmdc) = mmdc_or_skip() else { return };
    let project = Project::new();
    let outcome = project.export_ok("mermaid_diagrams.md", None, &typst, Some(mmdc));
    assert!(
        outcome.warnings.is_empty(),
        "unexpected: {:?}",
        outcome.warnings
    );
    assert_valid_pdf(&outcome.pdf_path);

    let typ = std::fs::read_to_string(&outcome.typ_path).unwrap();
    assert_eq!(
        typ.matches(".mermaid-cache/mermaid-").count(),
        3,
        "expected 3 embedded diagrams in:\n{typ}"
    );
    assert!(
        !typ.contains("flowchart TD"),
        "diagram left as code:\n{typ}"
    );
    assert_mermaid_cache_clean(&project);

    if let Some(text) = pdf_text(&outcome.pdf_path) {
        // The diagram source is gone from the page...
        for source_marker in ["flowchartTD", "sequenceDiagram", "stateDiagram-v2"] {
            assert!(
                !text.contains(source_marker),
                "{source_marker} still printed as code:\n{text}"
            );
        }
        // ...and the diagrams' own labels are drawn (SVG <text>, not the
        // foreignObject HTML labels Typst cannot render).
        for label in ["開始", "条件判定", "ユーザー", "データベース", "Running"] {
            assert!(
                text.contains(label),
                "diagram label {label:?} missing:\n{text}"
            );
        }
        assert!(text.contains("図の前後にも通常の本文が残ること"));
    }
}

// ---- criterion: graceful degradation without mmdc -------------------------

#[test]
fn without_mmdc_mermaid_stays_a_code_block_with_a_warning() {
    let Some(typst) = typst_or_skip() else { return };
    // The detection layer reports "not installed" for an empty PATH...
    assert_eq!(
        detect_in(Tool::Mmdc, std::ffi::OsStr::new(""), None),
        ToolStatus::NotFound
    );
    // ...and the pipeline handed no mmdc still produces a PDF.
    let project = Project::new();
    let outcome = project.export_ok("mermaid_diagrams.md", None, &typst, None);

    assert_eq!(
        codes(&outcome.warnings),
        [PdfWarningCode::MermaidUnavailable]
    );
    assert!(
        outcome.warnings[0].message.contains("mmdc"),
        "warning should name mmdc: {:?}",
        outcome.warnings[0]
    );
    assert_valid_pdf(&outcome.pdf_path);
    let typ = std::fs::read_to_string(&outcome.typ_path).unwrap();
    assert!(
        !typ.contains(".mermaid-cache"),
        "no SVG can exist without mmdc"
    );
    if let Some(text) = pdf_text(&outcome.pdf_path) {
        for source_marker in ["flowchartTD", "sequenceDiagram", "stateDiagram-v2"] {
            assert!(
                text.contains(source_marker),
                "{source_marker} (code block fallback) missing:\n{text}"
            );
        }
        assert!(text.contains("図の前後にも通常の本文が残ること"));
    }
}

#[test]
fn an_unrunnable_mmdc_degrades_like_a_missing_one() {
    let Some(typst) = typst_or_skip() else { return };
    let project = Project::new();
    let outcome = project.export_ok(
        "mermaid_diagrams.md",
        None,
        &typst,
        Some(Path::new("/nonexistent/mmdc")),
    );
    assert_eq!(
        codes(&outcome.warnings),
        [PdfWarningCode::MermaidSyntaxError; 3],
        "every block fails individually: {:?}",
        outcome.warnings
    );
    assert_valid_pdf(&outcome.pdf_path);
}

// ---- criterion: edge cases warn, never fail ---------------------------------

fn assert_edge_case_warnings(outcome: &ExportOutcome, mermaid_code: PdfWarningCode) {
    assert_valid_pdf(&outcome.pdf_path);
    let found = codes(&outcome.warnings);
    assert!(
        found.contains(&PdfWarningCode::ImageNotFound),
        "missing-image warning absent: {:?}",
        outcome.warnings
    );
    let image_warning = outcome
        .warnings
        .iter()
        .find(|w| w.code == PdfWarningCode::ImageNotFound)
        .unwrap();
    assert!(
        image_warning
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("does-not-exist.png"),
        "warning should name the file: {image_warning:?}"
    );
    assert!(
        found.contains(&mermaid_code),
        "{mermaid_code:?} absent: {:?}",
        outcome.warnings
    );
    if let Some(text) = pdf_text(&outcome.pdf_path) {
        // Surrounding content survives the bad elements.
        assert!(text.contains("最後の段落"), "trailing text lost:\n{text}");
        assert!(text.contains("見出しのみの節"), "heading lost:\n{text}");
    }
}

#[test]
fn edge_cases_without_mmdc_warn_and_still_produce_a_pdf() {
    let Some(typst) = typst_or_skip() else { return };
    let project = Project::new();
    let outcome = project.export_ok("edge_cases.md", None, &typst, None);
    assert_edge_case_warnings(&outcome, PdfWarningCode::MermaidUnavailable);
}

#[test]
fn edge_cases_with_real_mmdc_report_bad_diagrams_as_warnings() {
    let Some(typst) = typst_or_skip() else { return };
    let Some(mmdc) = mmdc_or_skip() else { return };
    let project = Project::new();
    let outcome = project.export_ok("edge_cases.md", None, &typst, Some(mmdc));
    assert_edge_case_warnings(&outcome, PdfWarningCode::MermaidSyntaxError);
    assert_mermaid_cache_clean(&project);
}

// ---- criterion: templates (t-pdf-export.3) -----------------------------------

#[test]
fn built_in_templates_render_japanese_through_the_pipeline() {
    let Some(typst) = typst_or_skip() else { return };
    let project = Project::new();
    for template in [None, Some("default"), Some("report"), Some("spec")] {
        let outcome = project.export_ok("basic_japanese.md", template, &typst, None);
        assert!(
            outcome.warnings.is_empty(),
            "{template:?}: unexpected {:?}",
            outcome.warnings
        );
        assert_valid_pdf(&outcome.pdf_path);
        if let Some(text) = pdf_text(&outcome.pdf_path) {
            assert!(
                text.contains("吾輩は猫である"),
                "{template:?}: Japanese body missing:\n{text}"
            );
            assert!(
                text.contains("テスト太郎") || text.contains("handoff-mcp"),
                "{template:?}: cover author missing:\n{text}"
            );
        }
    }
}

#[test]
fn custom_template_path_is_used_by_the_pipeline() {
    let Some(typst) = typst_or_skip() else { return };
    let project = Project::new();
    std::fs::create_dir_all(project.root().join("design")).unwrap();
    std::fs::write(
        project.root().join("design/mine.typ"),
        "#let conf(title: none, doc, ..args) = { [CUSTOM-MARKER #title]; doc }\n",
    )
    .unwrap();

    let outcome = project.export_ok("basic_japanese.md", Some("design/mine.typ"), &typst, None);
    assert_valid_pdf(&outcome.pdf_path);
    let typ = std::fs::read_to_string(&outcome.typ_path).unwrap();
    assert!(
        typ.contains("/design/mine.typ"),
        "import path wrong:\n{typ}"
    );
    if let Some(text) = pdf_text(&outcome.pdf_path) {
        assert!(
            text.contains("CUSTOM-MARKER"),
            "custom template not applied:\n{text}"
        );
        assert!(text.contains("吾輩は猫である"));
    }
}

#[test]
fn missing_custom_template_is_an_error_not_a_silent_default() {
    let Some(typst) = typst_or_skip() else { return };
    let project = Project::new();
    let err = project
        .export("basic_japanese.md", Some("design/nope.typ"), &typst, None)
        .expect_err("a template that does not exist must fail");
    assert!(format!("{err:#}").contains("nope.typ"), "{err:#}");
    assert!(
        !project
            .handoff()
            .join(EXPORTS_SUBDIR)
            .join("basic_japanese.pdf")
            .exists(),
        "no PDF may be produced on failure"
    );
}

// ---- failure modes ---------------------------------------------------------

#[test]
fn typst_compile_error_is_reported_and_leaves_no_stale_pdf() {
    let Some(typst) = typst_or_skip() else { return };
    let project = Project::new();
    // Good export first, so there is an earlier PDF that must not survive.
    let first = project.export_ok("basic_japanese.md", None, &typst, None);
    assert!(first.pdf_path.is_file());

    // An unclosed passthrough call makes Typst itself fail.
    let handoff = project.handoff();
    let broken = "# 壊れた\n\n```typst\n#table(columns: \n```\n";
    let err = export_pdf_with(
        &ExportRequest {
            project_root: project.root(),
            handoff_dir: &handoff,
            doc_slug: "basic_japanese",
            markdown: broken,
            template: None,
            meta: CoverMeta {
                title: "broken".into(),
                ..CoverMeta::default()
            },
        },
        &typst,
        None,
    )
    .expect_err("invalid Typst must fail the export");
    assert!(
        format!("{err:#}").contains("typst compile"),
        "error should come from typst: {err:#}"
    );
    assert!(!first.pdf_path.exists(), "stale PDF left behind");
}
