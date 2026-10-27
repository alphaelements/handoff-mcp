//! `handoff_doc_export` (REQ-PDF-006): render handoff documents as PDF through
//! the Typst pipeline in [`crate::storage::docs::pdf::export`].
//!
//! Also home of the option parsing and rendering helper `handoff_report`
//! reuses for its `format: "pdf"` (REQ-PDF-007), so both tools share one
//! definition of `output_dir` / `template` / `include_toc` and of the cover
//! metadata arguments `cover_version` / `cover_status` / `cover_organization`.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};

use super::doc_export_enrich;
use super::HandlerContext;
use crate::storage::docs::filter::DocFilter;
use crate::storage::docs::pdf::bundle_layout::{HeaderOptions, PageBreakStyle};
use crate::storage::docs::pdf::deps::{self, Tool};
use crate::storage::docs::pdf::export::{
    export_bundle_pdf, export_pdf, BundleEntry, BundleRequest, ExportRequest,
};
use crate::storage::docs::pdf::gitignore::exports_gitignore_warning;
use crate::storage::docs::pdf::req_meta::ReqMetaIndex;
use crate::storage::docs::pdf::template::CoverMeta;
use crate::storage::docs::pdf::types::PdfWarning;
use crate::storage::docs::{
    find_doc_by_id, read_all_docs_with_unreadable, read_doc_with_body, DocMetadata,
};

/// The only export format this tool produces.
pub const FORMAT_PDF: &str = "pdf";

/// Cover title of a bundle when the caller gives no `bundle_title`.
const DEFAULT_BUNDLE_TITLE: &str = "Document Bundle";

/// File stem of a bundle PDF without a usable `bundle_title`.
const BUNDLE_SLUG_PREFIX: &str = "bundle";

/// Longest title-derived part of a bundle's file stem, in characters.
const BUNDLE_SLUG_MAX_CHARS: usize = 48;

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
    /// Version shown on the cover (`cover_version`, REQ-PDF-025). Documents
    /// carry no version or lifecycle status of their own, so the caller is
    /// the source of these three values.
    pub cover_version: Option<String>,
    /// Lifecycle status shown on the cover (`cover_status`).
    pub cover_status: Option<String>,
    /// Issuing organization shown on the cover (`cover_organization`).
    pub cover_organization: Option<String>,
    /// Meta blocks of the requirement items (REQ-PDF-030). Not an argument:
    /// `handoff_doc_export` fills it in from `enrich` once its documents are
    /// known. `None`: a plain export.
    pub req_meta: Option<ReqMetaIndex>,
}

/// The argument names of the cover metadata, in [`PdfOptions`] field order.
pub const COVER_ARGUMENTS: [&str; 3] = ["cover_version", "cover_status", "cover_organization"];

impl PdfOptions {
    /// Sets the extended cover fields the caller supplied on `cover`; fields
    /// the caller left out keep whatever `cover` already carries.
    fn apply_cover(&self, cover: &mut CoverMeta) {
        for (value, field) in [
            (&self.cover_version, &mut cover.version),
            (&self.cover_status, &mut cover.status),
            (&self.cover_organization, &mut cover.organization),
        ] {
            if value.is_some() {
                field.clone_from(value);
            }
        }
    }

    /// Reads `output_dir`, `template`, `include_toc` and the `cover_*`
    /// values from the tool arguments. A wrongly typed value is an error,
    /// never ignored; a blank cover value counts as not given.
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
        let cover_value = |key: &str| -> Result<Option<String>> {
            Ok(opt_string(arguments, key)?
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string))
        };
        Ok(Self {
            output_dir,
            template,
            include_toc,
            cover_version: cover_value(COVER_ARGUMENTS[0])?,
            cover_status: cover_value(COVER_ARGUMENTS[1])?,
            cover_organization: cover_value(COVER_ARGUMENTS[2])?,
            req_meta: None,
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

/// Argument names of the bundle layout, which only apply with `bundle: true`.
pub const LAYOUT_ARGUMENTS: [&str; 2] = ["page_break_style", "header"];

/// Keys of the `header` object, in documentation order.
const HEADER_KEYS: [&str; 3] = ["show_title", "show_page_number", "show_date"];

/// Page layout of a bundle PDF (REQ-PDF-024): `page_break_style` and
/// `header`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BundleLayout {
    pub page_break_style: PageBreakStyle,
    /// `None`: the template keeps its own header and footer.
    pub header: Option<HeaderOptions>,
}

impl BundleLayout {
    /// Reads `page_break_style` and `header`. A wrongly typed or unknown value
    /// is an error, never ignored; so is giving either without
    /// `bundle: true`, where they would have no effect.
    pub fn from_arguments(arguments: &Value, bundle: bool) -> Result<Self> {
        if !bundle {
            for key in LAYOUT_ARGUMENTS {
                if arguments.get(key).is_some_and(|v| !v.is_null()) {
                    bail!("'{key}' only applies with 'bundle': true");
                }
            }
            return Ok(Self::default());
        }
        let page_break_style = match opt_string(arguments, "page_break_style")? {
            None => PageBreakStyle::default(),
            Some(name) => PageBreakStyle::parse(name).ok_or_else(|| {
                anyhow!(
                    "'page_break_style' must be one of: {} (got {name:?})",
                    PageBreakStyle::NAMES.join(", ")
                )
            })?,
        };
        let header = match arguments.get("header") {
            None | Some(Value::Null) => None,
            Some(Value::Object(map)) => Some(Self::header_options(map)?),
            Some(_) => bail!("'header' must be an object ({})", HEADER_KEYS.join(", ")),
        };
        Ok(Self {
            page_break_style,
            header,
        })
    }

    fn header_options(map: &Map<String, Value>) -> Result<HeaderOptions> {
        if let Some(unknown) = map.keys().find(|k| !HEADER_KEYS.contains(&k.as_str())) {
            bail!(
                "unknown 'header' option '{unknown}' (supported: {})",
                HEADER_KEYS.join(", ")
            );
        }
        let switch = |key: &str| -> Result<bool> {
            match map.get(key) {
                None | Some(Value::Null) => Ok(true),
                Some(Value::Bool(b)) => Ok(*b),
                Some(_) => bail!("'header.{key}' must be a boolean"),
            }
        };
        Ok(HeaderOptions {
            show_title: switch("show_title")?,
            show_page_number: switch("show_page_number")?,
            show_date: switch("show_date")?,
        })
    }
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
    options.apply_cover(&mut cover);
    let outcome = export_pdf(&ExportRequest {
        project_root: &ctx.project_dir,
        handoff_dir: &ctx.handoff_dir,
        doc_slug: source.slug,
        markdown: source.markdown,
        template: options.template.as_deref().or(source.default_template),
        meta: cover,
        req_meta: options.req_meta.as_ref(),
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

/// Runs the export pipeline for a bundle of documents and places the PDF per
/// `options` (REQ-PDF-020). `cover.title` is the bundle title.
pub fn render_bundle_pdf(
    ctx: &HandlerContext,
    slug: &str,
    entries: &[BundleEntry<'_>],
    cover: CoverMeta,
    layout: BundleLayout,
    options: &PdfOptions,
) -> Result<RenderedPdf> {
    let mut meta = cover;
    meta.show_toc = options.include_toc;
    options.apply_cover(&mut meta);
    let outcome = export_bundle_pdf(&BundleRequest {
        project_root: &ctx.project_dir,
        handoff_dir: &ctx.handoff_dir,
        slug,
        entries,
        template: options.template.as_deref(),
        meta,
        req_meta: options.req_meta.as_ref(),
        page_break_style: layout.page_break_style,
        header: layout.header,
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

/// File stem of a bundle PDF: `bundle-<title>` with the title lower-cased and
/// every run of characters that is not a letter or digit collapsed to `-`
/// (letters of any script are kept), or plain `bundle` when nothing is left.
/// Never contains a path separator or a leading dot.
fn bundle_slug(title: Option<&str>) -> String {
    let mut part = String::new();
    for c in title.unwrap_or_default().chars() {
        if c.is_alphanumeric() {
            part.extend(c.to_lowercase());
        } else if !part.ends_with('-') {
            part.push('-');
        }
    }
    let part: String = part
        .trim_matches('-')
        .chars()
        .take(BUNDLE_SLUG_MAX_CHARS)
        .collect();
    let part = part.trim_end_matches('-');
    if part.is_empty() {
        BUNDLE_SLUG_PREFIX.to_string()
    } else {
        format!("{BUNDLE_SLUG_PREFIX}-{part}")
    }
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

/// The filters a `scope` object may carry (REQ-PDF-021).
const SCOPE_KEYS: [&str; 5] = ["layer", "doc_type", "milestone", "tags", "task_id"];

/// A non-blank string filter of `scope`; blank or wrongly typed is an error.
fn scope_string(scope: &Map<String, Value>, key: &str) -> Result<Option<String>> {
    match scope.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s.trim().to_string())),
        Some(Value::String(_)) => bail!("'scope.{key}' must not be blank"),
        Some(_) => bail!("'scope.{key}' must be a string"),
    }
}

/// Parses the `scope` argument (REQ-PDF-021). `Ok(None)` when it is absent.
/// An unknown key, a wrongly typed value or a scope without any condition is
/// an error: ignoring it would export every document.
fn parse_scope(arguments: &Value) -> Result<Option<DocFilter>> {
    let scope = match arguments.get("scope") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Object(map)) => map,
        Some(_) => bail!("'scope' must be an object"),
    };
    if let Some(unknown) = scope.keys().find(|k| !SCOPE_KEYS.contains(&k.as_str())) {
        bail!(
            "Unknown 'scope' filter '{unknown}' (supported: {})",
            SCOPE_KEYS.join(", ")
        );
    }
    let tags = match scope.get("tags") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| match v.as_str().map(str::trim) {
                Some(t) if !t.is_empty() => Ok(t.to_string()),
                _ => bail!("'scope.tags' must be an array of non-blank strings"),
            })
            .collect::<Result<Vec<_>>>()?,
        Some(_) => bail!("'scope.tags' must be an array of strings"),
    };
    let filter = DocFilter {
        doc_type: scope_string(scope, "doc_type")?,
        tags,
        task_id: scope_string(scope, "task_id")?,
        layer: scope_string(scope, "layer")?,
        milestone: scope_string(scope, "milestone")?,
    };
    if filter.is_empty() {
        bail!(
            "'scope' needs at least one filter ({})",
            SCOPE_KEYS.join(", ")
        );
    }
    Ok(Some(filter))
}

/// Slugs of the documents matching `filter`, oldest first (ties by slug) so
/// the order of a bundle does not depend on directory enumeration. Documents
/// whose frontmatter cannot be read have no metadata to filter on and are
/// not candidates (`handoff_doc_list` lists them as `unreadable`).
fn scoped_keys(ctx: &HandlerContext, filter: &DocFilter, arguments: &Value) -> Result<Vec<String>> {
    let (mut docs, _unreadable) = read_all_docs_with_unreadable(&ctx.handoff_dir)?;
    filter.retain(&mut docs);
    if docs.is_empty() {
        let scope = arguments.get("scope").cloned().unwrap_or(Value::Null);
        bail!("No documents match 'scope' {scope}");
    }
    docs.sort_by(|a, b| (&a.created_at, &a.slug).cmp(&(&b.created_at, &b.slug)));
    Ok(docs.into_iter().map(|d| d.slug).collect())
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

/// `bool` argument that defaults to `false`; a wrongly typed value is an error.
fn flag(arguments: &Value, key: &str) -> Result<bool> {
    flag_or(arguments, key, false)
}

/// `bool` argument with a default; a wrongly typed value is an error.
fn flag_or(arguments: &Value, key: &str, default: bool) -> Result<bool> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => bail!("'{key}' must be a boolean"),
    }
}

/// A requested document that was found.
struct Resolved {
    key: String,
    doc: DocMetadata,
    body: String,
}

/// Resolves every requested document before anything is rendered, so a bad id
/// costs no Typst run. Duplicates (same document by id or slug) count once.
/// Documents that cannot be read are returned as `(key, reason)` failures.
fn resolve_documents(
    ctx: &HandlerContext,
    keys: Vec<String>,
) -> (Vec<Resolved>, Vec<(String, String)>) {
    let mut seen = HashSet::new();
    let mut resolved = Vec::new();
    let mut failures = Vec::new();
    for key in keys {
        match load_doc(ctx, &key) {
            Ok((doc, body)) => {
                if seen.insert(doc.id.clone()) {
                    resolved.push(Resolved { key, doc, body });
                }
            }
            Err(e) => failures.push((key, format!("{e:#}"))),
        }
    }
    (resolved, failures)
}

/// `handoff_doc_export`.
pub fn handle_doc_export(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let format = opt_string(arguments, "format")?.unwrap_or(FORMAT_PDF);
    if format != FORMAT_PDF {
        bail!("Unsupported export format '{format}' (supported: {FORMAT_PDF})");
    }
    let bundle = flag(arguments, "bundle")?;
    let bundle_title = opt_string(arguments, "bundle_title")?
        .map(str::trim)
        .filter(|t| !t.is_empty());
    if !bundle && arguments.get("bundle_title").is_some_and(|v| !v.is_null()) {
        bail!("'bundle_title' only applies with 'bundle': true");
    }
    let layout = BundleLayout::from_arguments(arguments, bundle)?;
    let scope = parse_scope(arguments)?;
    let selects_documents = |key: &str| arguments.get(key).is_some_and(|v| !v.is_null());
    if scope.is_some() && (selects_documents("doc_id") || selects_documents("doc_ids")) {
        bail!("'scope' is mutually exclusive with 'doc_id' and 'doc_ids'");
    }
    let keys = match &scope {
        Some(filter) => scoped_keys(ctx, filter, arguments)?,
        None => requested_keys(arguments)?,
    };
    let single_request = selects_documents("doc_id");
    if bundle && single_request {
        bail!("'bundle' merges several documents: pass 'doc_ids', not 'doc_id'");
    }
    let enrich = flag_or(arguments, "enrich", true)?;
    let mut options = PdfOptions::from_arguments(ctx, arguments)?;

    let (resolved, failures) = resolve_documents(ctx, keys);
    if resolved.is_empty() {
        bail!("{}", failure_summary(&failures));
    }
    require_typst()?;

    // Not tied to one document, so it is reported once, without a `doc_id`.
    let mut shared_warnings: Vec<PdfWarning> = Vec::new();
    if enrich {
        let docs: Vec<&DocMetadata> = resolved.iter().map(|r| &r.doc).collect();
        let enrichment = doc_export_enrich::collect(&ctx.handoff_dir, &docs);
        options.req_meta = Some(enrichment.index);
        shared_warnings.extend(enrichment.warning);
    }

    if bundle {
        export_bundle(
            ctx,
            &resolved,
            failures,
            bundle_title,
            layout,
            &options,
            &shared_warnings,
        )
    } else {
        export_each(
            ctx,
            &resolved,
            failures,
            single_request,
            &options,
            &shared_warnings,
        )
    }
}

/// One PDF per document (REQ-PDF-006).
fn export_each(
    ctx: &HandlerContext,
    resolved: &[Resolved],
    mut failures: Vec<(String, String)>,
    single_request: bool,
    options: &PdfOptions,
    shared_warnings: &[PdfWarning],
) -> Result<String> {
    let mut documents: Vec<Value> = Vec::new();
    let mut warnings: Vec<Value> = Vec::new();
    let mut pdf_paths: Vec<String> = Vec::new();
    for Resolved { key, doc, body } in resolved {
        let source = PdfSource {
            slug: &doc.slug,
            markdown: body,
            cover: CoverMeta {
                title: doc.title.clone(),
                date: cover_date(&doc.updated_at),
                doc_id: Some(doc.id.clone()),
                ..CoverMeta::default()
            },
            default_template: None,
        };
        match render_pdf(ctx, source, options) {
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

    for w in shared_warnings {
        warnings.push(serde_json::to_value(w)?);
    }
    // Not tied to one document, so it is reported once, without a `doc_id`
    // (REQ-PDF-014 AC4). Only worth saying once something was written there.
    if let Some(w) = exports_gitignore_warning(&ctx.project_dir, &ctx.handoff_dir) {
        warnings.push(serde_json::to_value(&w)?);
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

/// All documents in one PDF (REQ-PDF-020). `exported` counts the documents
/// merged in; the single PDF is `pdf_path` (and the only `pdf_paths` entry).
/// Warnings come from one shared run, so none carries a `doc_id`.
fn export_bundle(
    ctx: &HandlerContext,
    resolved: &[Resolved],
    failures: Vec<(String, String)>,
    bundle_title: Option<&str>,
    layout: BundleLayout,
    options: &PdfOptions,
    shared_warnings: &[PdfWarning],
) -> Result<String> {
    let entries: Vec<BundleEntry<'_>> = resolved
        .iter()
        .map(|r| BundleEntry {
            title: &r.doc.title,
            markdown: &r.body,
        })
        .collect();
    let title = bundle_title.unwrap_or(DEFAULT_BUNDLE_TITLE);
    let slug = bundle_slug(bundle_title);
    let pdf = render_bundle_pdf(
        ctx,
        &slug,
        &entries,
        CoverMeta {
            title: title.to_string(),
            ..CoverMeta::default()
        },
        layout,
        options,
    )?;

    let mut warnings = pdf
        .warnings
        .iter()
        .chain(shared_warnings)
        .map(serde_json::to_value)
        .collect::<serde_json::Result<Vec<_>>>()?;
    if let Some(w) = exports_gitignore_warning(&ctx.project_dir, &ctx.handoff_dir) {
        warnings.push(serde_json::to_value(&w)?);
    }

    let mut documents: Vec<Value> = resolved
        .iter()
        .map(|r| json!({ "doc_id": r.doc.id, "slug": r.doc.slug, "title": r.doc.title }))
        .collect();
    for (key, error) in &failures {
        documents.push(json!({ "doc_id": key, "error": error }));
    }

    let pdf_path = pdf.pdf_path.display().to_string();
    let out = json!({
        "format": FORMAT_PDF,
        "bundle": true,
        "bundle_title": title,
        "exported": resolved.len(),
        "failed": failures.len(),
        "pdf_path": pdf_path,
        "typ_path": pdf.typ_path.display().to_string(),
        "pdf_paths": [pdf_path],
        "documents": documents,
        "warnings": warnings,
    });
    Ok(serde_json::to_string_pretty(&out)?)
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
    fn cover_options_are_read_trimmed_and_applied_over_the_cover() {
        let c = ctx(Path::new("/p"));
        let opts = PdfOptions::from_arguments(
            &c,
            &json!({ "cover_version": " v1 ", "cover_status": "  ", "cover_organization": "Acme" }),
        )
        .unwrap();
        assert_eq!(opts.cover_version.as_deref(), Some("v1"));
        assert_eq!(opts.cover_status, None);
        let mut cover = CoverMeta {
            status: Some("kept".into()),
            ..CoverMeta::default()
        };
        opts.apply_cover(&mut cover);
        assert_eq!(cover.version.as_deref(), Some("v1"));
        assert_eq!(cover.status.as_deref(), Some("kept"));
        assert_eq!(cover.organization.as_deref(), Some("Acme"));
        assert!(PdfOptions::from_arguments(&c, &json!({ "cover_status": 1 })).is_err());
    }

    #[test]
    fn layout_defaults_to_break_and_the_templates_header() {
        let l = BundleLayout::from_arguments(&json!({}), true).unwrap();
        assert_eq!(l, BundleLayout::default());
        assert_eq!(l.page_break_style, PageBreakStyle::Break);
        assert_eq!(l.header, None);
        let l = BundleLayout::from_arguments(
            &json!({ "page_break_style": null, "header": null }),
            true,
        )
        .unwrap();
        assert_eq!(l, BundleLayout::default());
    }

    #[test]
    fn layout_reads_every_page_break_style() {
        for (name, want) in [
            ("break", PageBreakStyle::Break),
            ("section", PageBreakStyle::Section),
            ("none", PageBreakStyle::Continuous),
        ] {
            let l =
                BundleLayout::from_arguments(&json!({ "page_break_style": name }), true).unwrap();
            assert_eq!(l.page_break_style, want, "{name}");
        }
    }

    #[test]
    fn layout_rejects_a_bad_page_break_style_naming_the_choices() {
        let err = BundleLayout::from_arguments(&json!({ "page_break_style": "wide" }), true)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("page_break_style") && err.contains("break, section, none"),
            "{err}"
        );
        assert!(BundleLayout::from_arguments(&json!({ "page_break_style": 1 }), true).is_err());
    }

    #[test]
    fn layout_header_switches_default_to_shown() {
        let l = BundleLayout::from_arguments(&json!({ "header": {} }), true).unwrap();
        assert_eq!(l.header, Some(HeaderOptions::default()));
        let l = BundleLayout::from_arguments(
            &json!({ "header": { "show_title": false, "show_date": null } }),
            true,
        )
        .unwrap();
        assert_eq!(
            l.header,
            Some(HeaderOptions {
                show_title: false,
                show_page_number: true,
                show_date: true,
            })
        );
    }

    #[test]
    fn layout_rejects_malformed_headers() {
        for (bad, needle) in [
            (json!({ "header": true }), "'header' must be an object"),
            (json!({ "header": { "show_titel": true } }), "show_titel"),
            (
                json!({ "header": { "show_date": "yes" } }),
                "header.show_date",
            ),
        ] {
            let err = BundleLayout::from_arguments(&bad, true)
                .unwrap_err()
                .to_string();
            assert!(err.contains(needle), "{bad}: {err}");
        }
    }

    #[test]
    fn layout_without_bundle_is_rejected_not_ignored() {
        for args in [
            json!({ "page_break_style": "none" }),
            json!({ "header": {} }),
        ] {
            let err = BundleLayout::from_arguments(&args, false)
                .unwrap_err()
                .to_string();
            assert!(err.contains("bundle"), "{args}: {err}");
        }
        assert_eq!(
            BundleLayout::from_arguments(&json!({ "header": null }), false).unwrap(),
            BundleLayout::default()
        );
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
    fn bundle_slug_is_a_safe_file_stem_derived_from_the_title() {
        assert_eq!(bundle_slug(None), "bundle");
        assert_eq!(bundle_slug(Some("")), "bundle");
        assert_eq!(bundle_slug(Some("Release Pack")), "bundle-release-pack");
        assert_eq!(
            bundle_slug(Some("  v1.2 / Q3: Plan!! ")),
            "bundle-v1-2-q3-plan"
        );
        assert_eq!(bundle_slug(Some("リリース 資料")), "bundle-リリース-資料");
        // Nothing that survives can name another place.
        for evil in ["../../etc/passwd", "..", "/", "a\\b", ".hidden", "\u{0}\n"] {
            let slug = bundle_slug(Some(evil));
            assert!(
                !slug.contains(['/', '\\', '\0']) && !slug.starts_with('.') && !slug.contains(".."),
                "{evil:?} -> {slug:?}"
            );
            assert!(slug.starts_with("bundle"), "{slug}");
        }
        assert_eq!(bundle_slug(Some("!!!")), "bundle");
    }

    #[test]
    fn bundle_slug_is_capped() {
        let slug = bundle_slug(Some(&"x".repeat(500)));
        assert_eq!(
            slug.chars().count(),
            "bundle-".len() + BUNDLE_SLUG_MAX_CHARS
        );
        let cut_on_a_separator = format!("{}-{}", "x".repeat(BUNDLE_SLUG_MAX_CHARS - 1), "y");
        assert!(!bundle_slug(Some(&cut_on_a_separator)).ends_with('-'));
    }

    #[test]
    fn flag_defaults_to_false_and_rejects_other_types() {
        assert!(!flag(&json!({}), "bundle").unwrap());
        assert!(!flag(&json!({ "bundle": null }), "bundle").unwrap());
        assert!(flag(&json!({ "bundle": true }), "bundle").unwrap());
        assert!(flag(&json!({ "bundle": "true" }), "bundle").is_err());
    }

    #[test]
    fn flag_or_uses_its_default_and_rejects_other_types() {
        assert!(flag_or(&json!({}), "enrich", true).unwrap());
        assert!(flag_or(&json!({ "enrich": null }), "enrich", true).unwrap());
        assert!(!flag_or(&json!({ "enrich": false }), "enrich", true).unwrap());
        assert!(flag_or(&json!({ "enrich": 1 }), "enrich", true).is_err());
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
