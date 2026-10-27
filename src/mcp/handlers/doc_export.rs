//! `handoff_doc_export` (REQ-PDF-006): render handoff documents as PDF through
//! the Typst pipeline in [`crate::storage::docs::pdf::export`].
//!
//! Also home of the option parsing and rendering helper `handoff_report`
//! reuses for its `format: "pdf"` (REQ-PDF-007), so both tools share one
//! definition of `output_dir` / `template` / `include_toc`.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};

use super::HandlerContext;
use crate::storage::docs::pdf::deps::{self, Tool};
use crate::storage::docs::pdf::export::{export_pdf, ExportRequest};
use crate::storage::docs::pdf::template::CoverMeta;
use crate::storage::docs::pdf::types::PdfWarning;
use crate::storage::docs::{find_doc_by_id, read_doc_with_body, DocMetadata};

/// The only export format this tool produces.
pub const FORMAT_PDF: &str = "pdf";

/// Options shared by every PDF-producing tool call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PdfOptions {
    /// Absolute directory the PDF is placed in instead of `.handoff/exports/`.
    pub output_dir: Option<PathBuf>,
    /// Template name or path (see `prepare_template`); `None` = the caller's
    /// default.
    pub template: Option<String>,
    /// `Some(b)` forces the template's table-of-contents switch; `None` keeps
    /// the template's own default.
    pub include_toc: Option<bool>,
}

impl PdfOptions {
    /// Reads `output_dir`, `template` and `include_toc` from the tool
    /// arguments. A wrongly typed value is an error, never ignored.
    pub fn from_arguments(ctx: &HandlerContext, arguments: &Value) -> Result<Self> {
        let output_dir = match opt_string(arguments, "output_dir")? {
            Some(dir) => Some(resolve_output_dir(&ctx.project_dir, dir)?),
            None => None,
        };
        let template = opt_string(arguments, "template")?
            .map(str::to_string)
            .filter(|t| !t.trim().is_empty());
        let include_toc = match arguments.get("include_toc") {
            None | Some(Value::Null) => None,
            Some(Value::Bool(b)) => Some(*b),
            Some(_) => bail!("'include_toc' must be a boolean"),
        };
        Ok(Self {
            output_dir,
            template,
            include_toc,
        })
    }
}

/// `Ok(None)` when the key is absent or null; an error when present but not a
/// string.
pub fn opt_string<'a>(arguments: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => bail!("'{key}' must be a string"),
    }
}

/// An absolute `output_dir` is taken as given; a relative one is anchored at
/// the project directory and may not climb out of it with `..` (pass an
/// absolute path to write elsewhere on purpose).
fn resolve_output_dir(project_dir: &Path, raw: &str) -> Result<PathBuf> {
    if raw.trim().is_empty() {
        bail!("'output_dir' must not be empty");
    }
    let path = Path::new(raw);
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        bail!(
            "'output_dir' must not contain '..' when relative ({raw}); use an absolute path to \
             write outside the project directory"
        );
    }
    Ok(project_dir.join(path))
}

/// A PDF that was built.
#[derive(Debug, Clone)]
pub struct RenderedPdf {
    pub pdf_path: PathBuf,
    pub typ_path: PathBuf,
    pub warnings: Vec<PdfWarning>,
}

/// What to render.
pub struct PdfSource<'a> {
    /// Output file stem (`<stem>.pdf`).
    pub slug: &'a str,
    pub markdown: &'a str,
    pub cover: CoverMeta,
    /// Template used when `options.template` is not set.
    pub default_template: Option<&'a str>,
}

/// Fails fast, before any side effect, when the Typst CLI is unusable. The
/// error message carries the install instructions (REQ-PDF-001 AC2).
pub fn require_typst() -> Result<()> {
    deps::require(Tool::Typst)?;
    Ok(())
}

/// Runs the export pipeline for one document and places the PDF per
/// `options`.
pub fn render_pdf(
    ctx: &HandlerContext,
    source: PdfSource<'_>,
    options: &PdfOptions,
) -> Result<RenderedPdf> {
    let mut cover = source.cover;
    cover.show_toc = options.include_toc;
    let outcome = export_pdf(&ExportRequest {
        project_root: &ctx.project_dir,
        handoff_dir: &ctx.handoff_dir,
        doc_slug: source.slug,
        markdown: source.markdown,
        template: options.template.as_deref().or(source.default_template),
        meta: cover,
    })?;
    let pdf_path = match &options.output_dir {
        Some(dir) => relocate_pdf(&outcome.pdf_path, dir)?,
        None => outcome.pdf_path,
    };
    Ok(RenderedPdf {
        pdf_path,
        typ_path: outcome.typ_path,
        warnings: outcome.warnings,
    })
}

/// Moves `pdf` into `dir` (created when missing) and returns the new path.
/// Falls back to copy + remove when `dir` is on another filesystem.
fn relocate_pdf(pdf: &Path, dir: &Path) -> Result<PathBuf> {
    if dir.exists() && !dir.is_dir() {
        bail!(
            "'output_dir' {} exists and is not a directory",
            dir.display()
        );
    }
    std::fs::create_dir_all(dir)
        .with_context(|| format!("Cannot create 'output_dir' {}", dir.display()))?;
    let file_name = pdf
        .file_name()
        .ok_or_else(|| anyhow!("PDF path {} has no file name", pdf.display()))?;
    let target = dir.join(file_name);
    if target == pdf {
        return Ok(target);
    }
    if std::fs::rename(pdf, &target).is_err() {
        // `rename` cannot cross filesystems; any other rename failure
        // resurfaces from the copy below with its real cause.
        std::fs::copy(pdf, &target)
            .with_context(|| format!("Cannot write {}", target.display()))?;
        std::fs::remove_file(pdf)
            .with_context(|| format!("Cannot remove the intermediate {}", pdf.display()))?;
    }
    Ok(target)
}

/// A warning as a JSON object, tagged with the document it belongs to.
fn warning_json(warning: &PdfWarning, doc_id: &str) -> Result<Value> {
    let mut value = serde_json::to_value(warning)?;
    if let Value::Object(map) = &mut value {
        map.insert("doc_id".to_string(), json!(doc_id));
    }
    Ok(value)
}

/// The `YYYY-MM-DD` part of an RFC 3339 timestamp, for the cover page.
/// `None` for anything else: the template then falls back to today's date.
pub fn cover_date(timestamp: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|t| t.format("%Y-%m-%d").to_string())
}

/// Looks a document up by slug, else by stable id.
fn load_doc(ctx: &HandlerContext, key: &str) -> Result<(DocMetadata, String)> {
    let handoff = &ctx.handoff_dir;
    if let Some(pair) = read_doc_with_body(handoff, key)? {
        return Ok(pair);
    }
    if let Some(doc) = find_doc_by_id(handoff, key)? {
        if let Some(pair) = read_doc_with_body(handoff, &doc.slug)? {
            return Ok(pair);
        }
    }
    bail!("Document not found: {key}")
}

/// The requested document keys, in order: `doc_id` (one) or `doc_ids` (many).
fn requested_keys(arguments: &Value) -> Result<Vec<String>> {
    let single = opt_string(arguments, "doc_id")?;
    let many = match arguments.get("doc_ids") {
        None | Some(Value::Null) => None,
        Some(Value::Array(items)) => Some(items),
        Some(_) => bail!("'doc_ids' must be an array of strings"),
    };
    match (single, many) {
        (Some(_), Some(_)) => bail!("'doc_id' and 'doc_ids' are mutually exclusive"),
        (None, None) => bail!("'doc_id' or 'doc_ids' is required"),
        (Some(id), None) => Ok(vec![id.to_string()]),
        (None, Some(items)) => {
            if items.is_empty() {
                bail!("'doc_ids' must contain at least one document id");
            }
            items
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| anyhow!("'doc_ids' must be an array of strings"))
                })
                .collect()
        }
    }
}

/// `handoff_doc_export`.
pub fn handle_doc_export(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let format = opt_string(arguments, "format")?.unwrap_or(FORMAT_PDF);
    if format != FORMAT_PDF {
        bail!("Unsupported export format '{format}' (supported: {FORMAT_PDF})");
    }
    let keys = requested_keys(arguments)?;
    let single_request = arguments.get("doc_id").is_some_and(|v| !v.is_null());
    let options = PdfOptions::from_arguments(ctx, arguments)?;

    // Resolve every document before rendering anything, so a bad id costs no
    // Typst run and the install hint below only shows when there is work.
    let mut seen = HashSet::new();
    let mut resolved = Vec::new();
    let mut failures: Vec<(String, String)> = Vec::new();
    for key in keys {
        match load_doc(ctx, &key) {
            Ok((doc, body)) => {
                if seen.insert(doc.id.clone()) {
                    resolved.push((key, doc, body));
                }
            }
            Err(e) => failures.push((key, format!("{e:#}"))),
        }
    }
    if resolved.is_empty() {
        bail!("{}", failure_summary(&failures));
    }
    require_typst()?;

    let mut documents: Vec<Value> = Vec::new();
    let mut warnings: Vec<Value> = Vec::new();
    let mut pdf_paths: Vec<String> = Vec::new();
    for (key, doc, body) in &resolved {
        let source = PdfSource {
            slug: &doc.slug,
            markdown: body,
            cover: CoverMeta {
                title: doc.title.clone(),
                date: cover_date(&doc.updated_at),
                ..CoverMeta::default()
            },
            default_template: None,
        };
        match render_pdf(ctx, source, &options) {
            Ok(pdf) => {
                let doc_warnings = pdf
                    .warnings
                    .iter()
                    .map(|w| warning_json(w, &doc.id))
                    .collect::<Result<Vec<_>>>()?;
                warnings.extend(doc_warnings.iter().cloned());
                pdf_paths.push(pdf.pdf_path.display().to_string());
                documents.push(json!({
                    "doc_id": doc.id,
                    "slug": doc.slug,
                    "title": doc.title,
                    "pdf_path": pdf.pdf_path.display().to_string(),
                    "typ_path": pdf.typ_path.display().to_string(),
                    "warnings": doc_warnings,
                }));
            }
            Err(e) => failures.push((key.clone(), format!("{e:#}"))),
        }
    }
    if pdf_paths.is_empty() {
        bail!("{}", failure_summary(&failures));
    }

    // Failed documents are listed after the exported ones, in request order
    // within each group.
    for (key, error) in &failures {
        documents.push(json!({ "doc_id": key, "error": error }));
    }

    let mut out = Map::new();
    out.insert("format".into(), json!(FORMAT_PDF));
    out.insert("exported".into(), json!(pdf_paths.len()));
    out.insert("failed".into(), json!(failures.len()));
    if single_request {
        // `doc_id` form: the one PDF path is the answer (REQ-PDF-006 AC1).
        out.insert("pdf_path".into(), json!(pdf_paths[0]));
    }
    out.insert("pdf_paths".into(), json!(pdf_paths));
    out.insert("documents".into(), json!(documents));
    out.insert("warnings".into(), json!(warnings));
    Ok(serde_json::to_string_pretty(&Value::Object(out))?)
}

/// One line per failed document: `<key>: <reason>`.
fn failure_summary(failures: &[(String, String)]) -> String {
    failures
        .iter()
        .map(|(key, error)| format!("{key}: {error}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(dir: &Path) -> HandlerContext {
        HandlerContext {
            agent_id: None,
            project_dir: dir.to_path_buf(),
            handoff_dir: dir.join(".handoff"),
        }
    }

    #[test]
    fn relative_output_dir_is_anchored_at_the_project() {
        let got = resolve_output_dir(Path::new("/p"), "out/pdf").unwrap();
        assert_eq!(got, Path::new("/p/out/pdf"));
    }

    #[test]
    fn relative_output_dir_may_not_climb_out() {
        let err = resolve_output_dir(Path::new("/p"), "a/../../x").unwrap_err();
        assert!(err.to_string().contains(".."), "{err}");
    }

    #[test]
    fn absolute_output_dir_is_kept() {
        let got = resolve_output_dir(Path::new("/p"), "/elsewhere/pdf").unwrap();
        assert_eq!(got, Path::new("/elsewhere/pdf"));
    }

    #[test]
    fn blank_output_dir_is_rejected() {
        assert!(resolve_output_dir(Path::new("/p"), "  ").is_err());
    }

    #[test]
    fn options_ignore_blank_template_and_reject_bad_types() {
        let c = ctx(Path::new("/p"));
        let ok = PdfOptions::from_arguments(&c, &json!({ "template": "  ", "include_toc": false }))
            .unwrap();
        assert_eq!(ok.template, None);
        assert_eq!(ok.include_toc, Some(false));
        assert!(PdfOptions::from_arguments(&c, &json!({ "template": 3 })).is_err());
        assert!(PdfOptions::from_arguments(&c, &json!({ "include_toc": 1 })).is_err());
        assert!(PdfOptions::from_arguments(&c, &json!({ "output_dir": true })).is_err());
    }

    #[test]
    fn cover_date_takes_the_date_part_or_nothing() {
        assert_eq!(
            cover_date("2026-10-27T06:35:21.181905779+00:00").as_deref(),
            Some("2026-10-27")
        );
        assert_eq!(cover_date("yesterday"), None);
    }

    #[test]
    fn relocate_moves_the_file_and_creates_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = dir.path().join("a.pdf");
        std::fs::write(&pdf, b"%PDF-").unwrap();
        let moved = relocate_pdf(&pdf, &dir.path().join("x/y")).unwrap();
        assert_eq!(moved, dir.path().join("x/y/a.pdf"));
        assert!(moved.exists() && !pdf.exists());
    }

    #[test]
    fn relocate_into_the_same_directory_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = dir.path().join("a.pdf");
        std::fs::write(&pdf, b"%PDF-").unwrap();
        assert_eq!(relocate_pdf(&pdf, dir.path()).unwrap(), pdf);
        assert!(pdf.exists());
    }

    #[test]
    fn relocate_refuses_a_file_as_directory() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = dir.path().join("a.pdf");
        std::fs::write(&pdf, b"%PDF-").unwrap();
        let file = dir.path().join("occupied");
        std::fs::write(&file, b"x").unwrap();
        let err = relocate_pdf(&pdf, &file).unwrap_err();
        assert!(err.to_string().contains("output_dir"), "{err}");
        assert!(pdf.exists());
    }

    #[test]
    fn requested_keys_validates_shape() {
        assert_eq!(
            requested_keys(&json!({ "doc_id": "a" })).unwrap(),
            ["a".to_string()]
        );
        assert_eq!(
            requested_keys(&json!({ "doc_ids": ["a", "b"] })).unwrap(),
            ["a".to_string(), "b".to_string()]
        );
        assert!(requested_keys(&json!({})).is_err());
        assert!(requested_keys(&json!({ "doc_ids": [] })).is_err());
        assert!(requested_keys(&json!({ "doc_ids": "a" })).is_err());
        assert!(requested_keys(&json!({ "doc_id": "a", "doc_ids": ["b"] })).is_err());
    }
}
