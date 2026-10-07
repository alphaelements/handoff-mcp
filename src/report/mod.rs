//! Report engine foundation (FR-513 / SPEC-513).
//!
//! [`ReportEngine`] owns a Handlebars registry preloaded with the built-in
//! templates (embedded in the binary via `include_str!`). A project can
//! override any built-in — or add a new template/partial — by dropping a
//! `<name>.md.hbs` file into `.handoff/templates/`
//! ([`ReportEngine::load_custom_templates`]). Report metadata and rendered
//! Markdown live under `.handoff/reports/` (see [`store`]).
//!
//! This module only provides the engine, metadata model, and approval
//! workflow; the real content of each report type (data collection, layout)
//! is layered on top by replacing the placeholder templates.

pub mod period;
pub mod store;
pub mod verification;
pub mod weekly;

use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use handlebars::{Context as HbContext, Handlebars, Helper, HelperResult, Output, RenderContext};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Directory (inside `.handoff/`) holding per-project template overrides.
pub const TEMPLATES_DIR: &str = "templates";

/// File-name suffix of a template; the part before it is the template name.
const TEMPLATE_SUFFIX: &str = ".md.hbs";

/// Built-in templates as `(template name, source)`. Every [`ReportType`] must
/// have an entry here (enforced by `every_report_type_has_a_builtin_template`).
const BUILTIN_TEMPLATES: &[(&str, &str)] = &[
    (
        "verification",
        include_str!("templates/verification.md.hbs"),
    ),
    ("weekly", include_str!("templates/weekly.md.hbs")),
];

/// Kind of report; selects the template that renders it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportType {
    Verification,
    Weekly,
}

impl ReportType {
    pub const ALL: [ReportType; 2] = [ReportType::Verification, ReportType::Weekly];

    pub fn as_str(self) -> &'static str {
        match self {
            ReportType::Verification => "verification",
            ReportType::Weekly => "weekly",
        }
    }

    /// Name of the template (built-in or `.handoff/templates/` override) that
    /// renders this report type.
    pub fn template_name(self) -> &'static str {
        self.as_str()
    }

    pub fn parse(s: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|t| t.as_str() == s)
            .ok_or_else(|| {
                let valid: Vec<&str> = Self::ALL.iter().map(|t| t.as_str()).collect();
                anyhow!(
                    "Unknown report_type '{s}' (expected one of: {})",
                    valid.join(", ")
                )
            })
    }
}

/// What a report covers. Every field is optional; unknown fields are rejected
/// so a typo (`lable`) is an error rather than a silently empty scope.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportScope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Covered period as an ISO week (`2026-W41`) or a date range
    /// (`2026-10-05..2026-10-11`); see [`period::Period::parse`]. Mutually
    /// exclusive with `from`/`to`. A weekly report resolves it into
    /// `from`/`to` when it is generated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<String>,
    /// Start of the covered period (free-form date string, e.g. `2026-10-01`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// End of the covered period.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<String>,
    /// Verification campaign (`test_run_id`) a verification report is built
    /// from; its checklist supplies the verdicts and evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub campaign: Option<String>,
    /// Verification reports only: keep rows whose result is one of these
    /// (`pass|fail|blocked|waived|pending`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statuses: Vec<String>,
}

/// Approval-workflow state of a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Draft,
    Submitted,
    Approved,
    RevisionRequested,
    Published,
}

impl ReportStatus {
    pub const ALL: [ReportStatus; 5] = [
        ReportStatus::Draft,
        ReportStatus::Submitted,
        ReportStatus::Approved,
        ReportStatus::RevisionRequested,
        ReportStatus::Published,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ReportStatus::Draft => "draft",
            ReportStatus::Submitted => "submitted",
            ReportStatus::Approved => "approved",
            ReportStatus::RevisionRequested => "revision_requested",
            ReportStatus::Published => "published",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|t| t.as_str() == s)
            .ok_or_else(|| {
                let valid: Vec<&str> = Self::ALL.iter().map(|t| t.as_str()).collect();
                anyhow!(
                    "Unknown status '{s}' (expected one of: {})",
                    valid.join(", ")
                )
            })
    }

    /// Whether the workflow allows moving from `self` to `to`. `published` is
    /// part of the model but has no transition into it yet (publishing is
    /// outside this foundation).
    pub fn can_transition_to(self, to: ReportStatus) -> bool {
        matches!(
            (self, to),
            (ReportStatus::Draft, ReportStatus::Submitted)
                | (ReportStatus::RevisionRequested, ReportStatus::Submitted)
                | (ReportStatus::Submitted, ReportStatus::Approved)
                | (ReportStatus::Submitted, ReportStatus::RevisionRequested)
        )
    }
}

/// One entry of [`ReportMeta::revision_history`]: a status change (the first
/// entry is the creation, with `from: None`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionEntry {
    pub ts: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<ReportStatus>,
    pub to: ReportStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

/// `.handoff/reports/<report_id>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportMeta {
    pub report_id: String,
    pub report_type: ReportType,
    pub scope: ReportScope,
    /// 1-based; counts reports of the same type and scope.
    pub version: u32,
    pub status: ReportStatus,
    pub generated_at: String,
    #[serde(default)]
    pub reviewer: Option<String>,
    #[serde(default)]
    pub approved_at: Option<String>,
    /// Rendered Markdown, relative to `.handoff/`.
    pub output_path: String,
    #[serde(default)]
    pub revision_history: Vec<RevisionEntry>,
}

/// `{{cell value}}`: prints `value` safely inside a Markdown table cell —
/// `|` is escaped and line breaks become `<br>`. A missing value prints
/// nothing.
fn cell_helper(
    h: &Helper,
    _: &Handlebars,
    _: &HbContext,
    _: &mut RenderContext,
    out: &mut dyn Output,
) -> HelperResult {
    let text = match h.param(0).map(|p| p.value()) {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    };
    let escaped = text
        .replace('|', "\\|")
        .replace("\r\n", "<br>")
        .replace(['\n', '\r'], "<br>");
    out.write(&escaped)?;
    Ok(())
}

/// `{{percent value}}`: prints a number as a percentage (`50%`, `66.7%`); a
/// missing or non-numeric value prints `-`.
fn percent_helper(
    h: &Helper,
    _: &Handlebars,
    _: &HbContext,
    _: &mut RenderContext,
    out: &mut dyn Output,
) -> HelperResult {
    match h.param(0).and_then(|p| p.value().as_f64()) {
        Some(v) if v.fract() == 0.0 => out.write(&format!("{v:.0}%"))?,
        Some(v) => out.write(&format!("{v:.1}%"))?,
        None => out.write("-")?,
    }
    Ok(())
}

/// Handlebars registry plus the built-in (and per-project) templates.
pub struct ReportEngine {
    registry: Handlebars<'static>,
}

impl ReportEngine {
    /// Engine with only the built-in templates registered.
    pub fn new() -> Result<Self> {
        let mut registry = Handlebars::new();
        // Output is Markdown, not HTML: HTML-escaping `<`/`&` would corrupt
        // code spans, tables, and generic types in report data.
        registry.register_escape_fn(handlebars::no_escape);
        registry.register_helper("cell", Box::new(cell_helper));
        registry.register_helper("percent", Box::new(percent_helper));
        for (name, source) in BUILTIN_TEMPLATES {
            registry
                .register_template_string(name, source)
                .with_context(|| format!("Invalid built-in report template '{name}'"))?;
        }
        Ok(Self { registry })
    }

    /// Registers every `*.md.hbs` file in `<handoff_dir>/templates/`, replacing
    /// a built-in of the same name. A missing directory is not an error (no
    /// overrides). A template that fails to compile is an error naming the
    /// file — falling back to the built-in would hide the user's override.
    /// Returns the names registered, sorted.
    pub fn load_custom_templates(&mut self, handoff_dir: &Path) -> Result<Vec<String>> {
        let dir = handoff_dir.join(TEMPLATES_DIR);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut found: Vec<(String, std::path::PathBuf)> = Vec::new();
        for entry in std::fs::read_dir(&dir)
            .with_context(|| format!("Failed to read templates dir: {}", dir.display()))?
        {
            let path = entry
                .with_context(|| format!("Failed to read templates dir: {}", dir.display()))?
                .path();
            let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(name) = file_name.strip_suffix(TEMPLATE_SUFFIX) else {
                continue;
            };
            if name.is_empty() || !path.is_file() {
                continue;
            }
            found.push((name.to_string(), path));
        }
        found.sort();

        let mut names = Vec::with_capacity(found.len());
        for (name, path) in found {
            let source = std::fs::read_to_string(&path)
                .with_context(|| format!("Failed to read template: {}", path.display()))?;
            self.registry
                .register_template_string(&name, source)
                .with_context(|| format!("Invalid custom template {name}{TEMPLATE_SUFFIX}"))?;
            names.push(name);
        }
        Ok(names)
    }

    /// Renders the Markdown for `meta.report_type`. The template sees
    /// `{ report: <meta>, data: <data> }`.
    pub fn generate(&self, meta: &ReportMeta, data: &Value) -> Result<String> {
        let template = meta.report_type.template_name();
        if !self.registry.has_template(template) {
            bail!(
                "No template registered for report type '{}'",
                meta.report_type.as_str()
            );
        }
        let context = json!({ "report": meta, "data": data });
        self.registry
            .render(template, &context)
            .with_context(|| format!("Failed to render template '{template}'"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(report_type: ReportType) -> ReportMeta {
        ReportMeta {
            report_id: "r-1".into(),
            report_type,
            scope: ReportScope::default(),
            version: 1,
            status: ReportStatus::Draft,
            generated_at: "2026-10-08T00:00:00+00:00".into(),
            reviewer: None,
            approved_at: None,
            output_path: "reports/r-1.md".into(),
            revision_history: Vec::new(),
        }
    }

    #[test]
    fn every_report_type_has_a_builtin_template() {
        let engine = ReportEngine::new().unwrap();
        for t in ReportType::ALL {
            assert!(
                engine.registry.has_template(t.template_name()),
                "missing built-in template for {}",
                t.as_str()
            );
            engine.generate(&meta(t), &json!({})).unwrap();
        }
    }

    #[test]
    fn report_type_and_status_round_trip_through_parse() {
        for t in ReportType::ALL {
            assert_eq!(ReportType::parse(t.as_str()).unwrap(), t);
            assert_eq!(serde_json::to_value(t).unwrap(), json!(t.as_str()));
        }
        for s in ReportStatus::ALL {
            assert_eq!(ReportStatus::parse(s.as_str()).unwrap(), s);
            assert_eq!(serde_json::to_value(s).unwrap(), json!(s.as_str()));
        }
        assert!(ReportType::parse("x").is_err());
        assert!(ReportStatus::parse("x").is_err());
    }

    #[test]
    fn transition_table() {
        use ReportStatus::*;
        let allowed = [
            (Draft, Submitted),
            (RevisionRequested, Submitted),
            (Submitted, Approved),
            (Submitted, RevisionRequested),
        ];
        for from in ReportStatus::ALL {
            for to in ReportStatus::ALL {
                assert_eq!(
                    from.can_transition_to(to),
                    allowed.contains(&(from, to)),
                    "{from:?} -> {to:?}"
                );
            }
        }
    }

    #[test]
    fn output_is_not_html_escaped() {
        let mut engine = ReportEngine::new().unwrap();
        engine
            .registry
            .register_template_string("weekly", "{{data.x}}")
            .unwrap();
        let out = engine
            .generate(&meta(ReportType::Weekly), &json!({ "x": "<a & b>" }))
            .unwrap();
        assert_eq!(out, "<a & b>");
    }

    #[test]
    fn custom_templates_override_and_add_by_file_name() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(TEMPLATES_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("weekly.md.hbs"), "W:{{> footer}}").unwrap();
        std::fs::write(dir.join("footer.md.hbs"), "end").unwrap();
        std::fs::write(dir.join("ignored.txt"), "x").unwrap();
        std::fs::write(dir.join(".md.hbs"), "x").unwrap();

        let mut engine = ReportEngine::new().unwrap();
        let names = engine.load_custom_templates(tmp.path()).unwrap();
        assert_eq!(names, vec!["footer", "weekly"]);
        let out = engine
            .generate(&meta(ReportType::Weekly), &json!({}))
            .unwrap();
        assert_eq!(out, "W:end");
    }

    #[test]
    fn missing_templates_dir_means_no_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        let mut engine = ReportEngine::new().unwrap();
        assert!(engine.load_custom_templates(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn invalid_custom_template_names_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(TEMPLATES_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("weekly.md.hbs"), "{{#if}} unclosed").unwrap();
        let mut engine = ReportEngine::new().unwrap();
        let err = engine.load_custom_templates(tmp.path()).unwrap_err();
        assert!(format!("{err:#}").contains("weekly.md.hbs"), "{err:#}");
    }

    #[test]
    fn scope_rejects_unknown_fields() {
        let r: Result<ReportScope, _> = serde_json::from_value(json!({ "lable": "x" }));
        assert!(r.is_err());
    }
}
