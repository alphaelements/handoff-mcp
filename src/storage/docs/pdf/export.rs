//! End-to-end Markdown -> PDF orchestration (REQ-PDF-001, REQ-PDF-011).
//!
//! Content truth is the Markdown, layout truth is the Typst template, and the
//! intermediate `.handoff/exports/<slug>.typ` is regenerated on every export.
//! The steps, in order:
//!
//! 1. require the Typst CLI (existence + minimum version);
//! 2. resolve the template and its root-absolute import path;
//! 3. detect Japanese fonts;
//! 4. pre-render Mermaid blocks to SVG (RAII: the SVGs live until the PDF is
//!    built, then are removed);
//! 5. convert the Markdown to Typst, prepend the preamble, write the `.typ`;
//! 6. `typst compile --root <project_root> <slug>.typ <slug>.pdf`.
//!
//! `--root <project_root>` is mandatory: image paths and the template import
//! are project-root-absolute.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use super::bundle_layout::{running_page_rule, section_heading, HeaderOptions, PageBreakStyle};
use super::deps::{self, Tool, ToolStatus, Version};
use super::markdown_to_typst::{markdown_to_typst, ConvertOptions};
use super::mermaid::{
    convert_mermaid_blocks_with, mermaid_cache_dir, MermaidWarning, MMDC_TIMEOUT,
};
use super::req_meta::{ReqMetaIndex, REQ_META_FUNCTION};
use super::template::{
    detect_fonts, ensure_req_meta_part, prepare_template, render_preamble, typst_import_path,
    typst_string, CoverMeta,
};
use super::types::{PdfWarning, PdfWarningCode};
use crate::storage::atomic_write;

/// Directory (inside `.handoff/`) holding the generated `.typ` and `.pdf`.
pub const EXPORTS_SUBDIR: &str = "exports";

/// Longest a single `typst compile` may run. Generous because the first run
/// may build the font cache, but bounded so an offline package fetch (a
/// passthrough block importing `@preview/...`) cannot hang the tool call.
pub const TYPST_COMPILE_TIMEOUT: Duration = Duration::from_secs(120);

/// Poll interval while waiting for `typst` to exit.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Longest slice of Typst's stderr echoed in an error message.
const STDERR_MAX_CHARS: usize = 4000;

/// One export job.
#[derive(Debug, Clone)]
pub struct ExportRequest<'a> {
    /// Passed to `typst compile --root`. Everything the document references
    /// (images, template, `.handoff/`) must live below it.
    pub project_root: &'a Path,
    /// The project's `.handoff/` directory (below `project_root`).
    pub handoff_dir: &'a Path,
    /// Output file stem: `exports/<doc_slug>.typ` / `.pdf`.
    pub doc_slug: &'a str,
    /// Markdown source (frontmatter / handoff metadata is stripped by the
    /// converter).
    pub markdown: &'a str,
    /// Template name, path, or `None` for the default (see
    /// [`prepare_template`]).
    pub template: Option<&'a str>,
    pub meta: CoverMeta,
    /// Meta of the requirement items in `markdown` (REQ-PDF-030): their
    /// headings get a meta block instead of losing their attribute lines.
    /// `None` or empty: a plain export.
    pub req_meta: Option<&'a ReqMetaIndex>,
}

/// A finished export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportOutcome {
    pub typ_path: PathBuf,
    pub pdf_path: PathBuf,
    /// Version of the Typst CLI that produced the PDF, when it reported one.
    pub typst_version: Option<Version>,
    /// Every non-fatal problem from every stage, in pipeline order.
    pub warnings: Vec<PdfWarning>,
}

/// One document of a bundle (REQ-PDF-020).
#[derive(Debug, Clone, Copy)]
pub struct BundleEntry<'a> {
    /// Shown as the document's level-1 section heading, in the table of
    /// contents and (with the others) on the cover. A leading `# <title>` in
    /// the Markdown that repeats it is dropped.
    pub title: &'a str,
    pub markdown: &'a str,
}

/// Several documents merged into one PDF: cover (bundle title + document
/// list), one table of contents covering every document, and each document
/// starting on a new page under its own title.
#[derive(Debug, Clone)]
pub struct BundleRequest<'a> {
    pub project_root: &'a Path,
    pub handoff_dir: &'a Path,
    /// Output file stem: `exports/<slug>.typ` / `.pdf`.
    pub slug: &'a str,
    pub entries: &'a [BundleEntry<'a>],
    pub template: Option<&'a str>,
    /// `meta.title` is the bundle title. `meta.documents` is filled in from
    /// `entries` and any value passed here is replaced.
    pub meta: CoverMeta,
    /// Meta of the requirement items of every entry (REQ-PDF-030); see
    /// [`ExportRequest::req_meta`].
    pub req_meta: Option<&'a ReqMetaIndex>,
    /// How the documents are separated (REQ-PDF-024).
    pub page_break_style: PageBreakStyle,
    /// `Some`: the body pages get a running header / footer showing what the
    /// options select, replacing the template's own. `None`: the template's
    /// header and footer are kept (REQ-PDF-024).
    pub header: Option<HeaderOptions>,
}

/// A piece of the body: Markdown, optionally under a level-1 section heading.
#[derive(Debug, Clone, Copy)]
struct Section<'a> {
    /// `Some`: the section starts a new page with this level-1 heading and its
    /// own headings sit one level below it. `None`: the Markdown is converted
    /// as is.
    heading: Option<&'a str>,
    markdown: &'a str,
}

/// Everything one pipeline run needs, whether it renders one document or a
/// bundle.
struct Job<'a> {
    project_root: &'a Path,
    handoff_dir: &'a Path,
    slug: &'a str,
    template: Option<&'a str>,
    meta: CoverMeta,
    sections: Vec<Section<'a>>,
    req_meta: Option<&'a ReqMetaIndex>,
    page_break_style: PageBreakStyle,
    header: Option<HeaderOptions>,
}

/// Export using the Typst / mmdc found through [`deps`] (env override, then
/// `PATH`).
///
/// A missing or too-old Typst is an error whose message carries the install
/// instructions (REQ-PDF-001 AC). A missing or unusable `mmdc` is not: the
/// Mermaid blocks stay code blocks and a warning is returned.
pub fn export_pdf(request: &ExportRequest<'_>) -> Result<ExportOutcome> {
    run_with_detected_tools(|typst, mmdc| export_pdf_with(request, typst, mmdc))
}

/// [`export_pdf`] with explicit tool locations (`mmdc == None` leaves Mermaid
/// blocks as code blocks).
pub fn export_pdf_with(
    request: &ExportRequest<'_>,
    typst: &Path,
    mmdc: Option<&Path>,
) -> Result<ExportOutcome> {
    export_pdf_inner(request, typst, mmdc, MMDC_TIMEOUT, TYPST_COMPILE_TIMEOUT)
}

/// Export several documents as one PDF (REQ-PDF-020), with the tools found
/// through [`deps`] like [`export_pdf`].
pub fn export_bundle_pdf(request: &BundleRequest<'_>) -> Result<ExportOutcome> {
    run_with_detected_tools(|typst, mmdc| export_bundle_pdf_with(request, typst, mmdc))
}

/// [`export_bundle_pdf`] with explicit tool locations.
pub fn export_bundle_pdf_with(
    request: &BundleRequest<'_>,
    typst: &Path,
    mmdc: Option<&Path>,
) -> Result<ExportOutcome> {
    run_job(
        bundle_job(request),
        typst,
        mmdc,
        MMDC_TIMEOUT,
        TYPST_COMPILE_TIMEOUT,
    )
}

fn bundle_job<'a>(request: &BundleRequest<'a>) -> Job<'a> {
    let mut meta = request.meta.clone();
    meta.documents = request
        .entries
        .iter()
        .map(|e| e.title.to_string())
        .collect();
    Job {
        project_root: request.project_root,
        handoff_dir: request.handoff_dir,
        slug: request.slug,
        template: request.template,
        meta,
        sections: request
            .entries
            .iter()
            .map(|e| Section {
                heading: Some(e.title),
                markdown: e.markdown,
            })
            .collect(),
        req_meta: request.req_meta,
        page_break_style: request.page_break_style,
        header: request.header,
    }
}

/// Finds Typst (required) and mmdc (optional) and hands them to `run`.
fn run_with_detected_tools(
    run: impl FnOnce(&Path, Option<&Path>) -> Result<ExportOutcome>,
) -> Result<ExportOutcome> {
    let typst = deps::require(Tool::Typst)?;
    let (mmdc, mmdc_problem) = match deps::detect(Tool::Mmdc) {
        ToolStatus::Found { path, .. } => (Some(path), None),
        ToolStatus::NotFound => (None, None),
        status => (
            None,
            Some(
                deps::DepError {
                    tool: Tool::Mmdc,
                    status,
                }
                .to_string(),
            ),
        ),
    };
    let mut outcome = run(&typst.path, mmdc.as_deref())?;
    outcome.typst_version = typst.version;
    if let Some(problem) = mmdc_problem {
        // Replace the generic "not found" text the Mermaid stage produced for
        // this case with the real reason (too old / not runnable).
        for warning in &mut outcome.warnings {
            if warning.code == PdfWarningCode::MermaidUnavailable {
                warning.message = format!("Mermaid diagrams left as code blocks. {problem}");
            }
        }
    }
    Ok(outcome)
}

fn export_pdf_inner(
    request: &ExportRequest<'_>,
    typst: &Path,
    mmdc: Option<&Path>,
    mmdc_timeout: Duration,
    compile_timeout: Duration,
) -> Result<ExportOutcome> {
    let job = Job {
        project_root: request.project_root,
        handoff_dir: request.handoff_dir,
        slug: request.doc_slug,
        template: request.template,
        meta: request.meta.clone(),
        sections: vec![Section {
            heading: None,
            markdown: request.markdown,
        }],
        req_meta: request.req_meta,
        // A single document has no sections to separate and keeps the
        // template's own header and footer.
        page_break_style: PageBreakStyle::default(),
        header: None,
    };
    run_job(job, typst, mmdc, mmdc_timeout, compile_timeout)
}

fn run_job(
    job: Job<'_>,
    typst: &Path,
    mmdc: Option<&Path>,
    mmdc_timeout: Duration,
    compile_timeout: Duration,
) -> Result<ExportOutcome> {
    validate_slug(job.slug)?;
    let mut warnings = Vec::new();

    let template = prepare_template(job.handoff_dir, job.project_root, job.template)?;
    let import_path = typst_import_path(job.project_root, &template.path)?;
    // REQ-PDF-030: the meta block comes from its own template part, imported
    // by the generated file, so it does not depend on the page template.
    let req_meta = job.req_meta.filter(|index| !index.is_empty());
    let req_meta_import = match req_meta {
        Some(_) => {
            let part = ensure_req_meta_part(job.handoff_dir)?;
            Some(format!(
                "#import {}: {REQ_META_FUNCTION}\n\n",
                typst_string(&typst_import_path(job.project_root, &part)?)
            ))
        }
        None => None,
    };

    let fonts = detect_fonts(typst)?;
    if let Some(message) = fonts.missing_warning() {
        warnings.push(PdfWarning::new(
            PdfWarningCode::JapaneseFontMissing,
            message,
        ));
    }

    let mut body = String::new();
    if let Some(header) = job.header {
        body.push_str(&running_page_rule(
            header,
            &job.meta.title,
            job.meta.date.as_deref(),
        ));
    }
    // Held until the PDF is built; Drop removes the generated SVGs.
    let mut mermaid_outputs = Vec::with_capacity(job.sections.len());
    for section in &job.sections {
        let mermaid = convert_mermaid_blocks_with(
            section.markdown,
            &mermaid_cache_dir(job.handoff_dir),
            mmdc,
            mmdc_timeout,
        );
        warnings.extend(mermaid.warnings.iter().map(mermaid_warning));

        let mut options = ConvertOptions::new(job.project_root);
        options.req_meta = req_meta;
        if let Some(title) = section.heading {
            body.push_str(&section_heading(title, job.page_break_style));
            options.section_title = Some(title);
        }
        let converted = markdown_to_typst(&mermaid.markdown, &options);
        warnings.extend(converted.warnings);
        body.push_str(&converted.typst);
        mermaid_outputs.push(mermaid);
    }

    let template_source = std::fs::read_to_string(&template.path)
        .with_context(|| format!("Failed to read template {}", template.path.display()))?;
    let meta = job.meta.clone().restrict_to_template(&template_source);
    if meta.documents.is_empty() && !job.meta.documents.is_empty() {
        warnings.push(
            PdfWarning::new(
                PdfWarningCode::TemplateOutdated,
                "the template's `conf` does not declare `documents`, so the cover page lacks \
                 the document list; add the parameter, or delete the generated template from \
                 .handoff/templates/ to get the current one",
            )
            .with_detail(template.path.display().to_string()),
        );
    }

    let dropped: Vec<&str> = [
        ("doc-id", &job.meta.doc_id, &meta.doc_id),
        ("version", &job.meta.version, &meta.version),
        ("status", &job.meta.status, &meta.status),
        ("organization", &job.meta.organization, &meta.organization),
    ]
    .into_iter()
    .filter(|(_, wanted, kept)| wanted.is_some() && kept.is_none())
    .map(|(name, _, _)| name)
    .collect();
    if !dropped.is_empty() {
        warnings.push(
            PdfWarning::new(
                PdfWarningCode::TemplateOutdated,
                format!(
                    "the template's `conf` does not declare {}, so the cover page lacks {}; add \
                     the parameter(s), or delete the generated template from .handoff/templates/ \
                     to get the current one",
                    dropped.join(", "),
                    if dropped.len() == 1 { "it" } else { "them" },
                ),
            )
            .with_detail(template.path.display().to_string()),
        );
    }

    let source = format!(
        "{}{}{}",
        render_preamble(&import_path, &meta, &fonts),
        req_meta_import.unwrap_or_default(),
        body
    );

    let exports_dir = job.handoff_dir.join(EXPORTS_SUBDIR);
    std::fs::create_dir_all(&exports_dir)
        .with_context(|| format!("Failed to create {}", exports_dir.display()))?;
    let typ_path = exports_dir.join(format!("{}.typ", job.slug));
    let pdf_path = exports_dir.join(format!("{}.pdf", job.slug));
    atomic_write(&typ_path, source.as_bytes())
        .with_context(|| format!("Failed to write {}", typ_path.display()))?;

    // A failed compile must not leave an earlier export looking current
    // (REQ-PDF-013 AC2: no PDF is produced). NotFound is the normal case.
    match std::fs::remove_file(&pdf_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e).with_context(|| format!("Failed to remove stale {}", pdf_path.display()))
        }
    }

    compile(
        typst,
        job.project_root,
        &typ_path,
        &pdf_path,
        compile_timeout,
    )?;

    // Explicit so a cleanup failure is not silently lost; the PDF is already
    // built, so it is a warning-grade problem, not an export failure. The
    // Drop would retry, but it cannot report.
    for mermaid in &mut mermaid_outputs {
        if let Err(e) = mermaid.cleanup() {
            warnings.push(
                PdfWarning::new(
                    PdfWarningCode::MermaidUnavailable,
                    "temporary Mermaid SVG files could not be removed",
                )
                .with_detail(e.to_string()),
            );
        }
    }

    Ok(ExportOutcome {
        typ_path,
        pdf_path,
        typst_version: None,
        warnings,
    })
}

/// A slug becomes a file stem inside `exports/`: it must not be able to name
/// anything else.
fn validate_slug(slug: &str) -> Result<()> {
    let bad = slug.trim().is_empty()
        || slug.starts_with('.')
        || slug.contains(['/', '\\', '\0'])
        || slug.chars().any(char::is_control);
    if bad {
        bail!(
            "Invalid document slug {slug:?}: it must be a non-empty file name without path \
             separators and not starting with '.'"
        );
    }
    Ok(())
}

fn mermaid_warning(warning: &MermaidWarning) -> PdfWarning {
    match warning {
        MermaidWarning::MmdcNotFound { blocks } => {
            PdfWarning::new(PdfWarningCode::MermaidUnavailable, warning.to_string())
                .with_detail(format!("{blocks} block(s)"))
        }
        // `mmdc` rejected the diagram (its stderr, which names the syntax
        // problem, is in the message) or timed out.
        MermaidWarning::RenderFailed { index, .. } => {
            PdfWarning::new(PdfWarningCode::MermaidSyntaxError, warning.to_string())
                .with_detail(format!("mermaid block #{index}"))
        }
    }
}

/// `typst compile --root <root> <input> <output>`; Typst's own diagnostics
/// (file, line, message) are returned verbatim on failure (REQ-PDF-013 AC2).
fn compile(
    typst: &Path,
    root: &Path,
    input: &Path,
    output: &Path,
    timeout: Duration,
) -> Result<()> {
    let mut child = Command::new(typst)
        .arg("compile")
        .arg("--root")
        .arg(root)
        .arg(input)
        .arg(output)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to start {}", typst.display()))?;

    // Drain stderr concurrently so a chatty compile cannot block on a full pipe.
    let mut stderr = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = stderr.as_mut() {
            // A read error only means less of stderr is quoted in the error.
            let _ = pipe.read_to_end(&mut buf);
        }
        String::from_utf8_lossy(&buf).into_owned()
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait().context("Failed to wait for typst")? {
            Some(status) => break Some(status),
            None if Instant::now() >= deadline => {
                // Kill/reap failures only matter if the process lingers; the
                // timeout error below is what the caller needs.
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(POLL_INTERVAL),
        }
    };
    let Some(status) = status else {
        // Not joined: a grandchild that inherited the pipe could keep it open
        // and turn the timeout into a hang. The detached reader ends when the
        // pipe closes.
        bail!(
            "`typst compile` timed out after {}s; no PDF was produced.",
            timeout.as_secs_f32()
        );
    };
    // Ends once typst (and any grandchildren) close stderr.
    let stderr_text = reader.join().unwrap_or_default();
    if !status.success() {
        bail!(
            "`typst compile` failed ({status}); no PDF was produced.{}",
            quote_stderr(&stderr_text)
        );
    }
    if !output.is_file() {
        bail!(
            "`typst compile` reported success but {} was not created",
            output.display()
        );
    }
    Ok(())
}

fn quote_stderr(stderr: &str) -> String {
    let text = stderr.trim();
    if text.is_empty() {
        return String::new();
    }
    let shown: String = text.chars().take(STDERR_MAX_CHARS).collect();
    let ellipsis = if text.chars().count() > STDERR_MAX_CHARS {
        "\n..."
    } else {
        ""
    };
    format!("\n{shown}{ellipsis}")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A shell script standing in for `typst`. `fonts` prints
    /// `font_lines`; `compile` runs `compile_body` with `$@` being the
    /// arguments after `compile`.
    fn fake_typst(dir: &Path, font_lines: &str, compile_body: &str) -> PathBuf {
        let path = dir.join("fake-typst");
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n  fonts) printf '%s\\n' '{font_lines}' ;;\n  compile) shift\n{compile_body}\n ;;\nesac\n"
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// `compile` body: log args, then write the PDF (last argument).
    const OK_COMPILE: &str =
        "echo \"$@\" > \"$LOG\"; for last; do :; done; echo '%PDF-fake' > \"$last\"";

    struct Fixture {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        handoff: PathBuf,
        bin: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let handoff = root.join(".handoff");
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&handoff).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        Fixture {
            root,
            handoff,
            bin,
            _tmp: tmp,
        }
    }

    fn request<'a>(f: &'a Fixture, slug: &'a str, markdown: &'a str) -> ExportRequest<'a> {
        ExportRequest {
            project_root: &f.root,
            handoff_dir: &f.handoff,
            doc_slug: slug,
            markdown,
            template: None,
            meta: CoverMeta {
                title: "Doc".into(),
                ..CoverMeta::default()
            },
            req_meta: None,
        }
    }

    /// Compile body that logs args to `<bin>/args.log`.
    fn logging_compile(f: &Fixture) -> String {
        OK_COMPILE.replace("$LOG", &f.bin.join("args.log").display().to_string())
    }

    #[test]
    fn writes_typ_and_runs_compile_with_root() {
        let f = fixture();
        let typst = fake_typst(&f.bin, "Noto Sans CJK JP", &logging_compile(&f));
        let outcome = export_pdf_with(&request(&f, "my-doc", "# Title\n\nbody\n"), &typst, None)
            .expect("export");

        assert_eq!(outcome.typ_path, f.handoff.join("exports/my-doc.typ"));
        assert_eq!(outcome.pdf_path, f.handoff.join("exports/my-doc.pdf"));
        assert!(outcome.pdf_path.is_file());
        assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);

        let typ = std::fs::read_to_string(&outcome.typ_path).unwrap();
        assert!(
            typ.starts_with("#import \"/.handoff/templates/default.typ\": conf\n#show: conf.with("),
            "{typ}"
        );
        assert!(typ.contains("= Title"), "{typ}");

        let args = std::fs::read_to_string(f.bin.join("args.log")).unwrap();
        assert_eq!(
            args.trim(),
            format!(
                "--root {} {} {}",
                f.root.display(),
                outcome.typ_path.display(),
                outcome.pdf_path.display()
            )
        );
    }

    fn p0_index() -> ReqMetaIndex {
        use crate::storage::docs::pdf::req_meta::ReqMeta;
        ReqMetaIndex::from([(
            "REQ-001".to_string(),
            ReqMeta {
                priority: Some("P0".into()),
                ..ReqMeta::default()
            },
        )])
    }

    #[test]
    fn enriched_export_imports_the_meta_part_and_calls_it_under_the_heading() {
        let f = fixture();
        let typst = fake_typst(&f.bin, "Noto Sans CJK JP", &logging_compile(&f));
        let index = p0_index();
        let md = "## REQ-001 T\n\n- priority: P0\n\nbody\n";
        let mut req = request(&f, "r", md);
        req.req_meta = Some(&index);
        let out = export_pdf_with(&req, &typst, None).expect("export");
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);

        let typ = std::fs::read_to_string(&out.typ_path).unwrap();
        assert!(
            typ.contains("#import \"/.handoff/templates/parts/req-meta.typ\": req-meta\n"),
            "{typ}"
        );
        assert!(
            typ.contains("== REQ-001 T\n\n#req-meta(priority: \"P0\")\n"),
            "{typ}"
        );
        assert!(f.handoff.join("templates/parts/req-meta.typ").is_file());
    }

    #[test]
    fn plain_export_neither_imports_nor_creates_the_meta_part() {
        let f = fixture();
        let typst = fake_typst(&f.bin, "Noto Sans CJK JP", &logging_compile(&f));
        let index = p0_index();
        let md = "## REQ-001 T\n\n- priority: P0\n\nbody\n";
        // `None` and an empty index both mean "not enriched".
        let empty = ReqMetaIndex::new();
        for req_meta in [None, Some(&empty)] {
            let mut req = request(&f, "r", md);
            req.req_meta = req_meta;
            let out = export_pdf_with(&req, &typst, None).expect("export");
            let typ = std::fs::read_to_string(&out.typ_path).unwrap();
            assert!(!typ.contains("req-meta"), "{typ}");
            assert!(!f.handoff.join("templates/parts").exists());
        }
        drop(index);
    }

    #[test]
    fn a_customised_meta_part_is_used_as_is() {
        let f = fixture();
        let typst = fake_typst(&f.bin, "Noto Sans CJK JP", &logging_compile(&f));
        let part = f.handoff.join("templates/parts/req-meta.typ");
        std::fs::create_dir_all(part.parent().unwrap()).unwrap();
        std::fs::write(&part, "#let req-meta(..a) = []\n").unwrap();
        let index = p0_index();
        let mut req = request(&f, "r", "## REQ-001 T\n");
        req.req_meta = Some(&index);
        export_pdf_with(&req, &typst, None).expect("export");
        assert_eq!(
            std::fs::read_to_string(&part).unwrap(),
            "#let req-meta(..a) = []\n"
        );
    }

    #[test]
    fn enriched_bundle_puts_a_meta_block_under_each_documents_item() {
        let f = fixture();
        let typst = fake_typst(&f.bin, "Noto Sans CJK JP", &logging_compile(&f));
        let index = p0_index();
        let entries = [BundleEntry {
            title: "Alpha",
            markdown: "# Alpha\n\n## REQ-001 T\n\n- priority: P0\n",
        }];
        let mut req = bundle(&f, "bundle-x", &entries);
        req.req_meta = Some(&index);
        let out = export_bundle_pdf_with(&req, &typst, None).expect("bundle");
        let typ = std::fs::read_to_string(&out.typ_path).unwrap();
        assert!(
            typ.contains("== REQ-001 T\n\n#req-meta(priority: \"P0\")\n"),
            "{typ}"
        );
        assert_eq!(
            typ.matches("templates/parts/req-meta.typ").count(),
            1,
            "{typ}"
        );
    }

    #[test]
    fn typ_is_regenerated_on_every_export() {
        let f = fixture();
        let typst = fake_typst(&f.bin, "Noto Sans CJK JP", &logging_compile(&f));
        export_pdf_with(&request(&f, "d", "first version\n"), &typst, None).unwrap();
        let out = export_pdf_with(&request(&f, "d", "second version\n"), &typst, None).unwrap();
        let typ = std::fs::read_to_string(out.typ_path).unwrap();
        assert!(typ.contains("second version") && !typ.contains("first version"));
    }

    #[test]
    fn rejects_slugs_that_could_escape_exports_dir() {
        let f = fixture();
        let typst = fake_typst(&f.bin, "x", &logging_compile(&f));
        for slug in ["", "  ", "a/b", "..", "../x", ".hidden", "a\\b", "a\nb"] {
            let err = export_pdf_with(&request(&f, slug, "x"), &typst, None)
                .expect_err(&format!("slug {slug:?} must be rejected"));
            assert!(err.to_string().contains("Invalid document slug"), "{err}");
        }
        assert!(
            !f.handoff.join("exports").exists(),
            "nothing may be written"
        );
    }

    #[test]
    fn compile_failure_returns_typst_diagnostics_and_leaves_no_pdf() {
        let f = fixture();
        let ok = fake_typst(&f.bin, "Noto Sans CJK JP", &logging_compile(&f));
        let first = export_pdf_with(&request(&f, "d", "ok\n"), &ok, None).unwrap();
        assert!(first.pdf_path.is_file());

        let failing = fake_typst(
            &f.bin,
            "Noto Sans CJK JP",
            "echo 'error: unknown variable: foo' >&2; echo '  --> d.typ:12:3' >&2; exit 1",
        );
        let err = export_pdf_with(&request(&f, "d", "bad\n"), &failing, None).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown variable: foo"), "{msg}");
        assert!(msg.contains("d.typ:12:3"), "{msg}");
        assert!(!first.pdf_path.exists(), "stale PDF must be removed");
    }

    #[test]
    fn compile_timeout_is_reported() {
        let f = fixture();
        let slow = fake_typst(&f.bin, "Noto Sans CJK JP", "sleep 5");
        let started = Instant::now();
        let err = export_pdf_inner(
            &request(&f, "d", "x\n"),
            &slow,
            None,
            MMDC_TIMEOUT,
            Duration::from_millis(200),
        )
        .unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn success_without_output_file_is_an_error() {
        let f = fixture();
        let liar = fake_typst(&f.bin, "Noto Sans CJK JP", "exit 0");
        let err = export_pdf_with(&request(&f, "d", "x\n"), &liar, None).unwrap_err();
        assert!(err.to_string().contains("was not created"), "{err}");
    }

    #[test]
    fn missing_japanese_font_is_a_warning_not_an_error() {
        let f = fixture();
        let typst = fake_typst(&f.bin, "DejaVu Sans", &logging_compile(&f));
        let out = export_pdf_with(&request(&f, "d", "x\n"), &typst, None).unwrap();
        let codes: Vec<_> = out.warnings.iter().map(|w| w.code).collect();
        assert_eq!(codes, [PdfWarningCode::JapaneseFontMissing]);
    }

    #[test]
    fn unknown_template_is_an_error_before_anything_is_written() {
        let f = fixture();
        let typst = fake_typst(&f.bin, "Noto Sans CJK JP", &logging_compile(&f));
        let mut req = request(&f, "d", "x\n");
        req.template = Some("nope");
        let err = export_pdf_with(&req, &typst, None).unwrap_err();
        assert!(err.to_string().contains("nope"), "{err}");
        assert!(!f.handoff.join("exports").exists());
    }

    #[test]
    fn template_outside_project_root_is_an_error() {
        let f = fixture();
        let typst = fake_typst(&f.bin, "Noto Sans CJK JP", &logging_compile(&f));
        let outside = f.bin.join("t.typ");
        std::fs::write(&outside, "#let conf(..a, body) = body").unwrap();
        let mut req = request(&f, "d", "x\n");
        let spec = outside.display().to_string();
        req.template = Some(&spec);
        let err = export_pdf_with(&req, &typst, None).unwrap_err();
        assert!(err.to_string().contains("outside the Typst root"), "{err}");
    }

    #[test]
    fn missing_mmdc_keeps_mermaid_as_code_and_warns() {
        let f = fixture();
        let typst = fake_typst(&f.bin, "Noto Sans CJK JP", &logging_compile(&f));
        let md = "```mermaid\nflowchart TD\n A-->B\n```\n";
        let out = export_pdf_with(&request(&f, "d", md), &typst, None).unwrap();
        let codes: Vec<_> = out.warnings.iter().map(|w| w.code).collect();
        assert_eq!(codes, [PdfWarningCode::MermaidUnavailable]);
        let typ = std::fs::read_to_string(out.typ_path).unwrap();
        assert!(typ.contains("flowchart TD"), "{typ}");
    }

    /// `mmdc` stand-in: writes a tiny SVG to the `-o` target.
    fn fake_mmdc(dir: &Path, succeed: bool) -> PathBuf {
        let path = dir.join("fake-mmdc");
        let body = if succeed {
            "while [ $# -gt 0 ]; do [ \"$1\" = -o ] && out=\"$2\"; shift; done\n\
             echo '<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\"/>' > \"$out\""
        } else {
            "echo 'Parse error on line 2' >&2; exit 1"
        };
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn mermaid_svg_exists_during_compile_and_is_removed_after() {
        let f = fixture();
        let seen = f.bin.join("seen.log");
        // While compiling, record the cache dir listing.
        let body = format!(
            "ls '{}' > '{}'; {}",
            mermaid_cache_dir(&f.handoff).display(),
            seen.display(),
            logging_compile(&f)
        );
        let typst = fake_typst(&f.bin, "Noto Sans CJK JP", &body);
        let mmdc = fake_mmdc(&f.bin, true);
        let md = "```mermaid\nflowchart TD\n A-->B\n```\n";
        let out = export_pdf_with(&request(&f, "d", md), &typst, Some(&mmdc)).unwrap();
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);

        let during = std::fs::read_to_string(&seen).unwrap();
        assert!(
            during.contains(".svg"),
            "svg must exist at compile: {during:?}"
        );
        let typ = std::fs::read_to_string(&out.typ_path).unwrap();
        assert!(
            typ.contains("image(\"/.handoff/exports/.mermaid-cache/"),
            "{typ}"
        );
        assert!(
            !mermaid_cache_dir(&f.handoff).exists(),
            "SVG cache must be cleaned after export"
        );
    }

    #[test]
    fn mermaid_svg_is_removed_when_compile_fails() {
        let f = fixture();
        let typst = fake_typst(&f.bin, "Noto Sans CJK JP", "exit 1");
        let mmdc = fake_mmdc(&f.bin, true);
        let md = "```mermaid\nflowchart TD\n A-->B\n```\n";
        export_pdf_with(&request(&f, "d", md), &typst, Some(&mmdc)).unwrap_err();
        assert!(!mermaid_cache_dir(&f.handoff).exists());
    }

    #[test]
    fn mermaid_syntax_error_becomes_structured_warning_and_code_block() {
        let f = fixture();
        let typst = fake_typst(&f.bin, "Noto Sans CJK JP", &logging_compile(&f));
        let mmdc = fake_mmdc(&f.bin, false);
        let md = "```mermaid\nnot a diagram\n```\n";
        let out = export_pdf_with(&request(&f, "d", md), &typst, Some(&mmdc)).unwrap();
        assert_eq!(out.warnings.len(), 1);
        assert_eq!(out.warnings[0].code, PdfWarningCode::MermaidSyntaxError);
        assert!(out.warnings[0].message.contains("Parse error"));
        let typ = std::fs::read_to_string(out.typ_path).unwrap();
        assert!(typ.contains("not a diagram"), "{typ}");
    }

    #[test]
    fn converter_warnings_are_merged_after_mermaid_warnings() {
        let f = fixture();
        let typst = fake_typst(&f.bin, "Noto Sans CJK JP", &logging_compile(&f));
        let md = "```mermaid\nA-->B\n```\n\n![x](missing.png)\n";
        let out = export_pdf_with(&request(&f, "d", md), &typst, None).unwrap();
        let codes: Vec<_> = out.warnings.iter().map(|w| w.code).collect();
        assert_eq!(
            codes,
            [
                PdfWarningCode::MermaidUnavailable,
                PdfWarningCode::ImageNotFound
            ]
        );
    }

    #[test]
    fn mermaid_warning_mapping_carries_detail() {
        let w = mermaid_warning(&MermaidWarning::RenderFailed {
            index: 2,
            reason: "boom".into(),
        });
        assert_eq!(w.code, PdfWarningCode::MermaidSyntaxError);
        assert_eq!(w.detail.as_deref(), Some("mermaid block #2"));
        assert!(w.message.contains("boom"));
    }

    // -- bundle (REQ-PDF-020) -------------------------------------------

    fn bundle<'a>(
        f: &'a Fixture,
        slug: &'a str,
        entries: &'a [BundleEntry<'a>],
    ) -> BundleRequest<'a> {
        BundleRequest {
            project_root: &f.root,
            handoff_dir: &f.handoff,
            slug,
            entries,
            template: None,
            meta: CoverMeta {
                title: "Release Pack".into(),
                ..CoverMeta::default()
            },
            req_meta: None,
            page_break_style: PageBreakStyle::default(),
            header: None,
        }
    }

    #[test]
    fn bundle_is_one_typ_with_cover_list_page_breaks_and_titled_sections() {
        let f = fixture();
        let typst = fake_typst(&f.bin, "Noto Sans CJK JP", &logging_compile(&f));
        let entries = [
            BundleEntry {
                title: "Alpha",
                markdown: "# Alpha\n\nfirst body\n\n## Part\n",
            },
            BundleEntry {
                title: "Beta \"B\"",
                markdown: "---\nid: x\n---\n# Beta \"B\"\n\nsecond body\n",
            },
        ];
        let out = export_bundle_pdf_with(&bundle(&f, "bundle-pack", &entries), &typst, None)
            .expect("bundle");

        assert_eq!(out.pdf_path, f.handoff.join("exports/bundle-pack.pdf"));
        assert!(out.pdf_path.is_file());
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);

        let typ = std::fs::read_to_string(&out.typ_path).unwrap();
        // Cover: bundle title and the document list go to the template.
        assert!(typ.contains("  title: \"Release Pack\",\n"), "{typ}");
        assert!(
            typ.contains("  documents: (\"Alpha\", \"Beta \\\"B\\\"\", ),\n"),
            "{typ}"
        );
        // Each document: page break, own title as level-1 heading, its
        // headings one level lower, the duplicate `# title` dropped.
        let body = typ.split(")\n\n").nth(1).expect("body after preamble");
        assert_eq!(body.matches("#pagebreak(weak: true)").count(), 2, "{typ}");
        let alpha = body.find("#heading(level: 1)[#\"Alpha\"]").expect("alpha");
        let beta = body
            .find("#heading(level: 1)[#\"Beta \\\"B\\\"\"]")
            .expect("beta");
        assert!(alpha < beta);
        assert!(body.contains("first body") && body.contains("second body"));
        assert!(body.lines().any(|l| l == "== Part"), "{typ}");
        assert!(
            !body.contains("= Alpha\n") && !body.contains("= Beta"),
            "{typ}"
        );
        assert!(body.find("first body").unwrap() < beta, "{typ}");
        assert!(beta < body.find("second body").unwrap(), "{typ}");
        // One compile for the whole bundle.
        let args = std::fs::read_to_string(f.bin.join("args.log")).unwrap();
        assert_eq!(args.lines().count(), 1, "{args}");
    }

    // -- bundle layout (REQ-PDF-024) ----------------------------------

    fn export_layout(
        style: PageBreakStyle,
        header: Option<HeaderOptions>,
    ) -> (String, ExportOutcome) {
        let f = fixture();
        let typst = fake_typst(&f.bin, "Noto Sans CJK JP", &logging_compile(&f));
        let entries = [
            BundleEntry {
                title: "Alpha",
                markdown: "a body",
            },
            BundleEntry {
                title: "Beta",
                markdown: "b body",
            },
        ];
        let mut req = bundle(&f, "layout", &entries);
        req.page_break_style = style;
        req.header = header;
        let out = export_bundle_pdf_with(&req, &typst, None).expect("bundle");
        (std::fs::read_to_string(&out.typ_path).unwrap(), out)
    }

    #[test]
    fn bundle_page_break_style_selects_how_documents_are_separated() {
        let (brk, _) = export_layout(PageBreakStyle::Break, None);
        assert_eq!(brk.matches("#pagebreak(weak: true)").count(), 2, "{brk}");
        assert_eq!(brk.matches("#page(").count(), 0, "{brk}");

        let (section, _) = export_layout(PageBreakStyle::Section, None);
        assert_eq!(
            section.matches("#pagebreak(weak: true)").count(),
            2,
            "{section}"
        );
        assert_eq!(
            section.matches("#page(header: none").count(),
            2,
            "{section}"
        );

        let (none, _) = export_layout(PageBreakStyle::Continuous, None);
        assert!(!none.contains("#pagebreak"), "{none}");
        assert!(none.contains("#heading(level: 1)[#\"Alpha\"]"), "{none}");
        assert!(none.contains("#heading(level: 1)[#\"Beta\"]"), "{none}");
    }

    #[test]
    fn bundle_header_option_adds_the_running_page_rule_before_the_body() {
        let (typ, _) = export_layout(PageBreakStyle::Break, Some(HeaderOptions::default()));
        let rule = typ.find("#set page(").expect("page rule");
        let first = typ.find("#heading(level: 1)").expect("first heading");
        assert!(rule < first, "{typ}");
        assert_eq!(typ.matches("#set page(").count(), 1, "{typ}");
        // The fallback title is the bundle title.
        assert!(typ.contains("\"Release Pack\""), "{typ}");
    }

    #[test]
    fn bundle_without_header_option_leaves_the_templates_header_alone() {
        let (typ, _) = export_layout(PageBreakStyle::Break, None);
        assert!(!typ.contains("#set page("), "{typ}");
    }

    #[test]
    fn bundle_header_uses_the_cover_date_when_there_is_one() {
        let f = fixture();
        let typst = fake_typst(&f.bin, "Noto Sans CJK JP", &logging_compile(&f));
        let entries = [BundleEntry {
            title: "A",
            markdown: "x",
        }];
        let mut req = bundle(&f, "dated", &entries);
        req.meta.date = Some("2026-01-02".into());
        req.header = Some(HeaderOptions::default());
        let out = export_bundle_pdf_with(&req, &typst, None).unwrap();
        let typ = std::fs::read_to_string(&out.typ_path).unwrap();
        assert!(typ.contains("[#\"2026-01-02\"]"), "{typ}");
    }

    #[test]
    fn a_single_document_export_never_gets_the_running_page_rule() {
        let f = fixture();
        let typst = fake_typst(&f.bin, "Noto Sans CJK JP", &logging_compile(&f));
        let out = export_pdf_with(
            &ExportRequest {
                project_root: &f.root,
                handoff_dir: &f.handoff,
                doc_slug: "single",
                markdown: "# T\n\nbody\n",
                template: None,
                meta: CoverMeta {
                    title: "T".into(),
                    ..CoverMeta::default()
                },
                req_meta: None,
            },
            &typst,
            None,
        )
        .unwrap();
        let typ = std::fs::read_to_string(&out.typ_path).unwrap();
        assert!(!typ.contains("#set page("), "{typ}");
        assert!(!typ.contains("#pagebreak"), "{typ}");
    }

    #[test]
    fn bundle_replaces_a_caller_supplied_document_list() {
        let f = fixture();
        let typst = fake_typst(&f.bin, "Noto Sans CJK JP", &logging_compile(&f));
        let entries = [BundleEntry {
            title: "Only",
            markdown: "x\n",
        }];
        let mut req = bundle(&f, "b", &entries);
        req.meta.documents = vec!["Stale".into()];
        let out = export_bundle_pdf_with(&req, &typst, None).unwrap();
        let typ = std::fs::read_to_string(out.typ_path).unwrap();
        assert!(typ.contains("documents: (\"Only\", )"), "{typ}");
        assert!(!typ.contains("Stale"), "{typ}");
    }

    #[test]
    fn bundle_with_an_outdated_template_warns_and_omits_the_list() {
        let f = fixture();
        let typst = fake_typst(&f.bin, "Noto Sans CJK JP", &logging_compile(&f));
        let old = f.root.join("old.typ");
        std::fs::write(&old, "#let conf(title: none, doc) = doc\n").unwrap();
        let entries = [BundleEntry {
            title: "A",
            markdown: "x\n",
        }];
        let mut req = bundle(&f, "b", &entries);
        let spec = old.display().to_string();
        req.template = Some(&spec);
        let out = export_bundle_pdf_with(&req, &typst, None).unwrap();
        let codes: Vec<_> = out.warnings.iter().map(|w| w.code).collect();
        assert_eq!(codes, [PdfWarningCode::TemplateOutdated]);
        let typ = std::fs::read_to_string(out.typ_path).unwrap();
        assert!(!typ.contains("documents:"), "{typ}");
    }

    #[test]
    fn bundle_merges_warnings_and_cleans_every_documents_mermaid_svg() {
        let f = fixture();
        let seen = f.bin.join("seen.log");
        let body = format!(
            "ls '{}' > '{}'; {}",
            mermaid_cache_dir(&f.handoff).display(),
            seen.display(),
            logging_compile(&f)
        );
        let typst = fake_typst(&f.bin, "Noto Sans CJK JP", &body);
        let mmdc = fake_mmdc(&f.bin, true);
        let diagram = "```mermaid\nflowchart TD\n A-->B\n```\n";
        let first = format!("{diagram}\n![x](missing.png)\n");
        let entries = [
            BundleEntry {
                title: "One",
                markdown: &first,
            },
            BundleEntry {
                title: "Two",
                markdown: diagram,
            },
        ];
        let out = export_bundle_pdf_with(&bundle(&f, "b", &entries), &typst, Some(&mmdc)).unwrap();
        let codes: Vec<_> = out.warnings.iter().map(|w| w.code).collect();
        assert_eq!(codes, [PdfWarningCode::ImageNotFound]);
        let during = std::fs::read_to_string(&seen).unwrap();
        assert_eq!(during.matches(".svg").count(), 2, "{during:?}");
        assert!(!mermaid_cache_dir(&f.handoff).exists());
    }

    #[test]
    fn bundle_rejects_a_slug_that_could_escape_exports_dir() {
        let f = fixture();
        let typst = fake_typst(&f.bin, "x", &logging_compile(&f));
        let entries = [BundleEntry {
            title: "A",
            markdown: "x",
        }];
        let err = export_bundle_pdf_with(&bundle(&f, "../x", &entries), &typst, None).unwrap_err();
        assert!(err.to_string().contains("Invalid document slug"), "{err}");
    }

    #[test]
    fn quote_stderr_truncates_long_output() {
        let long = "e".repeat(STDERR_MAX_CHARS + 50);
        let quoted = quote_stderr(&long);
        assert!(quoted.ends_with("..."));
        assert_eq!(quote_stderr("  \n"), "");
    }
}
