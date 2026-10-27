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

use super::deps::{self, Tool, ToolStatus, Version};
use super::markdown_to_typst::{markdown_to_typst, ConvertOptions};
use super::mermaid::{
    convert_mermaid_blocks_with, mermaid_cache_dir, MermaidWarning, MMDC_TIMEOUT,
};
use super::template::{
    detect_fonts, prepare_template, render_preamble, typst_import_path, CoverMeta,
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

/// Export using the Typst / mmdc found through [`deps`] (env override, then
/// `PATH`).
///
/// A missing or too-old Typst is an error whose message carries the install
/// instructions (REQ-PDF-001 AC). A missing or unusable `mmdc` is not: the
/// Mermaid blocks stay code blocks and a warning is returned.
pub fn export_pdf(request: &ExportRequest<'_>) -> Result<ExportOutcome> {
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
    let mut outcome = export_pdf_with(request, &typst.path, mmdc.as_deref())?;
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

/// [`export_pdf`] with explicit tool locations (`mmdc == None` leaves Mermaid
/// blocks as code blocks).
pub fn export_pdf_with(
    request: &ExportRequest<'_>,
    typst: &Path,
    mmdc: Option<&Path>,
) -> Result<ExportOutcome> {
    export_pdf_inner(request, typst, mmdc, MMDC_TIMEOUT, TYPST_COMPILE_TIMEOUT)
}

fn export_pdf_inner(
    request: &ExportRequest<'_>,
    typst: &Path,
    mmdc: Option<&Path>,
    mmdc_timeout: Duration,
    compile_timeout: Duration,
) -> Result<ExportOutcome> {
    validate_slug(request.doc_slug)?;
    let mut warnings = Vec::new();

    let template = prepare_template(request.handoff_dir, request.project_root, request.template)?;
    let import_path = typst_import_path(request.project_root, &template.path)?;

    let fonts = detect_fonts(typst)?;
    if let Some(message) = fonts.missing_warning() {
        warnings.push(PdfWarning::new(
            PdfWarningCode::JapaneseFontMissing,
            message,
        ));
    }

    // Held until the PDF is built; Drop removes the generated SVGs.
    let mermaid = convert_mermaid_blocks_with(
        request.markdown,
        &mermaid_cache_dir(request.handoff_dir),
        mmdc,
        mmdc_timeout,
    );
    warnings.extend(mermaid.warnings.iter().map(mermaid_warning));

    let body = markdown_to_typst(
        &mermaid.markdown,
        &ConvertOptions {
            project_root: request.project_root,
        },
    );
    warnings.extend(body.warnings);

    let source = format!(
        "{}{}",
        render_preamble(&import_path, &request.meta, &fonts),
        body.typst
    );

    let exports_dir = request.handoff_dir.join(EXPORTS_SUBDIR);
    std::fs::create_dir_all(&exports_dir)
        .with_context(|| format!("Failed to create {}", exports_dir.display()))?;
    let typ_path = exports_dir.join(format!("{}.typ", request.doc_slug));
    let pdf_path = exports_dir.join(format!("{}.pdf", request.doc_slug));
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
        request.project_root,
        &typ_path,
        &pdf_path,
        compile_timeout,
    )?;

    // Explicit so a cleanup failure is not silently lost; the PDF is already
    // built, so it is a warning-grade problem, not an export failure. The
    // Drop would retry, but it cannot report.
    let mut mermaid = mermaid;
    if let Err(e) = mermaid.cleanup() {
        warnings.push(
            PdfWarning::new(
                PdfWarningCode::MermaidUnavailable,
                "temporary Mermaid SVG files could not be removed",
            )
            .with_detail(e.to_string()),
        );
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

    #[test]
    fn quote_stderr_truncates_long_output() {
        let long = "e".repeat(STDERR_MAX_CHARS + 50);
        let quoted = quote_stderr(&long);
        assert!(quoted.ends_with("..."));
        assert_eq!(quote_stderr("  \n"), "");
    }
}
