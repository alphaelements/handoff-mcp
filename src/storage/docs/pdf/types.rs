//! Shared value types for the PDF export pipeline (REQ-PDF-013).
//!
//! Every non-fatal problem found while converting a document is reported as a
//! [`PdfWarning`] instead of aborting the export, and the whole list is
//! returned to the caller in the tool response's `warnings` array
//! (REQ-PDF-013 AC4).

use serde::Serialize;

/// Machine-readable category of a [`PdfWarning`]. Serialized as `snake_case`
/// so MCP clients can branch on `code` without parsing `message`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PdfWarningCode {
    /// An image reference points at a file that does not exist
    /// (REQ-PDF-008 AC3). The converter emits the path as placeholder text.
    ImageNotFound,
    /// An image reference uses a format other than SVG / PNG / JPEG
    /// (REQ-PDF-008 AC2) or a remote URL, which Typst cannot embed.
    ImageUnsupported,
    /// An image reference resolves outside the project root, so Typst could
    /// not read it and it is not embedded.
    ImageOutsideProject,
    /// Raw HTML in the Markdown has no Typst equivalent and is emitted as
    /// literal text.
    HtmlNotSupported,
    /// A Mermaid block could not be rendered and was kept as a code block
    /// (REQ-PDF-003 AC2, REQ-PDF-013 AC1).
    MermaidUnavailable,
    /// A Mermaid block has a syntax error and was kept as a code block
    /// (REQ-PDF-013 AC1).
    MermaidSyntaxError,
    /// Typst reports no Japanese-capable font, so Japanese text would render
    /// as missing-glyph boxes (REQ-PDF-002).
    JapaneseFontMissing,
    /// `.handoff/exports/` is not git-ignored (REQ-PDF-014 AC4).
    ExportsNotGitignored,
    /// The selected template predates a feature the export needs (a `conf`
    /// parameter it does not declare), so that part of the output is left out
    /// (REQ-PDF-020: the bundle cover's document list).
    TemplateOutdated,
    /// The requirement meta blocks of an enriched export could not be built
    /// (the trace data failed to load), so the export is plain (REQ-PDF-030).
    EnrichUnavailable,
}

/// One structured, non-fatal problem found during export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PdfWarning {
    pub code: PdfWarningCode,
    /// Human-readable explanation.
    pub message: String,
    /// The offending value (image path, HTML snippet, ...), when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl PdfWarning {
    pub fn new(code: PdfWarningCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// Output of [`super::markdown_to_typst::markdown_to_typst`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConversionResult {
    /// Typst markup for the document *body* (no template, no `#import`).
    ///
    /// Image paths inside it are project-root-absolute (`/docs/a.png`), so the
    /// `typst compile` invocation must pass `--root <project_root>`.
    pub typst: String,
    pub warnings: Vec<PdfWarning>,
}
