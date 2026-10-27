//! Report engine foundation (FR-513 / SPEC-513).
//!
//! [`ReportEngine`] owns a Handlebars registry preloaded with the built-in
//! templates (embedded in the binary via `include_str!`). A project can
//! override any built-in — or add a new template/partial — by dropping a
//! `<name>.md.hbs` file into `.handoff/templates/`
//! ([`ReportEngine::load_custom_templates`]). Report metadata and rendered
//! Markdown live under `.handoff/reports/` (see [`store`]).
//!
//! This module provides the engine, metadata model, and approval workflow;
//! each report type's data collection lives in its own submodule
//! ([`verification`], [`weekly`], [`inspection`], [`effort`], [`monthly`],
//! [`milestone`], [`defect`], [`completion`], [`daily`], [`rollup`]) next to its built-in template.

pub mod charts;
pub mod completion;
pub mod daily;
pub mod defect;
pub mod effort;
pub mod inspection;
pub mod milestone;
pub mod monthly;
pub mod period;
pub mod rollup;
pub mod sections;
pub mod store;
pub mod trace_diagram;
pub mod verification;
pub mod weekly;

use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use handlebars::{Context as HbContext, Handlebars, Helper, HelperResult, Output, RenderContext};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use sections::{Composed, SectionSpec};

/// Directory (inside `.handoff/`) holding per-project template overrides.
pub const TEMPLATES_DIR: &str = "templates";

/// File-name suffix of a template; the part before it is the template name.
const TEMPLATE_SUFFIX: &str = ".md.hbs";

/// Built-in templates and partials as `(name, source)`. Every [`ReportType`] must
/// have a template entry here (enforced by `every_report_type_has_a_builtin_template`).
const BUILTIN_TEMPLATES: &[(&str, &str)] = &[
    (
        "verification",
        include_str!("templates/verification.md.hbs"),
    ),
    ("weekly", include_str!("templates/weekly.md.hbs")),
    ("inspection", include_str!("templates/inspection.md.hbs")),
    ("effort", include_str!("templates/effort.md.hbs")),
    ("monthly", include_str!("templates/monthly.md.hbs")),
    ("milestone", include_str!("templates/milestone.md.hbs")),
    ("defect", include_str!("templates/defect.md.hbs")),
    ("completion", include_str!("templates/completion.md.hbs")),
    ("daily", include_str!("templates/daily.md.hbs")),
    ("quarterly", include_str!("templates/quarterly.md.hbs")),
    ("yearly", include_str!("templates/yearly.md.hbs")),
    // Partial (not a report type): the optional `data.ai_*` sections every
    // report template includes right after its header block.
    ("ai_sections", include_str!("templates/ai_sections.md.hbs")),
];

/// Kind of report; selects the template that renders it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportType {
    Verification,
    Weekly,
    /// R2: inspection certificate of an approved verification campaign.
    Inspection,
    /// R5: effort report from `time_log.jsonl`.
    Effort,
    /// R4: monthly report (weekly roll-up plus metrics-snapshot trend).
    Monthly,
    /// R6: milestone report (planned vs actual dates, effort, quality).
    Milestone,
    /// R7: defect register (bug tasks and failing trace items).
    Defect,
    /// R8: completion report; only issued once every layer is approved.
    Completion,
    /// One-day report: the day's metrics, change from the previous snapshot
    /// and task status changes.
    Daily,
    /// Quarterly roll-up: one row per calendar month plus the snapshot trend.
    Quarterly,
    /// Yearly roll-up: one row per calendar month and quarter plus the trend.
    Yearly,
}

impl ReportType {
    pub const ALL: [ReportType; 11] = [
        ReportType::Verification,
        ReportType::Weekly,
        ReportType::Inspection,
        ReportType::Effort,
        ReportType::Monthly,
        ReportType::Milestone,
        ReportType::Defect,
        ReportType::Completion,
        ReportType::Daily,
        ReportType::Quarterly,
        ReportType::Yearly,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ReportType::Verification => "verification",
            ReportType::Weekly => "weekly",
            ReportType::Inspection => "inspection",
            ReportType::Effort => "effort",
            ReportType::Monthly => "monthly",
            ReportType::Milestone => "milestone",
            ReportType::Defect => "defect",
            ReportType::Completion => "completion",
            ReportType::Daily => "daily",
            ReportType::Quarterly => "quarterly",
            ReportType::Yearly => "yearly",
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
    /// Covered period as an ISO week (`2026-W41`), a day (`2026-10-08`), a
    /// quarter (`2026-Q4`), a year (`2026`) or a date range
    /// (`2026-10-05..2026-10-11`); see [`period::Period::parse`]. Mutually
    /// exclusive with `from`/`to`. Periodic reports resolve it into
    /// `from`/`to` when they are generated.
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
    /// Effort reports only: keep time-log entries of this person (the
    /// entry's `agent_id`, else the task's assignee).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// Milestone report only: the milestone (name from `config.toml` or a
    /// task's `schedule.milestone`) the report is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milestone: Option<String>,
}

impl ReportScope {
    /// Names of the fields this scope sets (`label` excluded: it is only a
    /// title and every report type accepts it).
    fn set_fields(&self) -> Vec<&'static str> {
        [
            ("period", self.period.is_some()),
            ("from", self.from.is_some()),
            ("to", self.to.is_some()),
            ("layers", !self.layers.is_empty()),
            ("items", !self.items.is_empty()),
            ("campaign", self.campaign.is_some()),
            ("statuses", !self.statuses.is_empty()),
            ("assignee", self.assignee.is_some()),
            ("milestone", self.milestone.is_some()),
        ]
        .into_iter()
        .filter_map(|(name, set)| set.then_some(name))
        .collect()
    }

    /// Errors when the scope sets a field outside `supported`, so a field a
    /// report type would silently ignore is a mistake the caller sees rather
    /// than a quietly wider report. `hint` names what does apply.
    pub(crate) fn require_only(
        &self,
        report_type: ReportType,
        supported: &[&str],
        hint: &str,
    ) -> Result<()> {
        let unsupported: Vec<&str> = self
            .set_fields()
            .into_iter()
            .filter(|f| !supported.contains(f))
            .collect();
        if unsupported.is_empty() {
            return Ok(());
        }
        bail!(
            "scope.{} not supported for a {} report ({hint})",
            unsupported.join(", scope."),
            report_type.as_str()
        )
    }
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
    /// reached only from `approved` (the `publish` action) and is terminal.
    pub fn can_transition_to(self, to: ReportStatus) -> bool {
        matches!(
            (self, to),
            (ReportStatus::Draft, ReportStatus::Submitted)
                | (ReportStatus::RevisionRequested, ReportStatus::Submitted)
                | (ReportStatus::Submitted, ReportStatus::Approved)
                | (ReportStatus::Submitted, ReportStatus::RevisionRequested)
                | (ReportStatus::Approved, ReportStatus::Published)
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
    /// Template the report was rendered with when it is not the one named
    /// after `report_type` (REQ-PDF-035).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
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

/// `{{signed value}}`: prints a number with an explicit `+` when positive
/// (`+2.0`, `-1.5`, `0.0`); a missing or non-numeric value prints `-`.
fn signed_helper(
    h: &Helper,
    _: &Handlebars,
    _: &HbContext,
    _: &mut RenderContext,
    out: &mut dyn Output,
) -> HelperResult {
    match h.param(0).map(|p| p.value()) {
        Some(Value::Number(n)) if n.as_f64().is_some_and(|v| v > 0.0) => {
            out.write(&format!("+{n}"))?
        }
        Some(Value::Number(n)) => out.write(&n.to_string())?,
        _ => out.write("-")?,
    }
    Ok(())
}

/// Handlebars registry plus the built-in (and per-project) templates.
pub struct ReportEngine {
    registry: Handlebars<'static>,
    /// Names loaded from `.handoff/templates/`, sorted.
    custom: Vec<String>,
}

/// The templates a [`ReportEngine`] can render with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TemplateCatalog {
    /// Built-in report templates (one per [`ReportType`]).
    pub builtin: Vec<String>,
    /// Built-in partials that templates include (not report templates).
    pub partials: Vec<String>,
    /// Project templates found in `.handoff/templates/`.
    pub custom: Vec<CustomTemplate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CustomTemplate {
    pub name: String,
    /// Relative to `.handoff/`.
    pub path: String,
    /// Replaces the built-in template or partial of the same name.
    pub overrides_builtin: bool,
}

/// A template name given by a caller: it is looked up by name only, never as
/// a path, so separators and a leading `.` are refused.
pub fn validate_template_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !ok {
        bail!(
            "Invalid template name '{name}' (use only letters, digits, '-', '_' and '.', not starting with '.')"
        );
    }
    Ok(())
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
        registry.register_helper("signed", Box::new(signed_helper));
        for (name, source) in BUILTIN_TEMPLATES {
            registry
                .register_template_string(name, source)
                .with_context(|| format!("Invalid built-in report template '{name}'"))?;
        }
        Ok(Self {
            registry,
            custom: Vec::new(),
        })
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
        self.custom.clone_from(&names);
        Ok(names)
    }

    /// Built-in and project templates, for listing.
    pub fn catalog(&self) -> TemplateCatalog {
        let report_names: Vec<&str> = ReportType::ALL.iter().map(|t| t.template_name()).collect();
        let is_builtin = |name: &str| BUILTIN_TEMPLATES.iter().any(|(n, _)| *n == name);
        TemplateCatalog {
            builtin: report_names.iter().map(|n| n.to_string()).collect(),
            partials: BUILTIN_TEMPLATES
                .iter()
                .map(|(n, _)| *n)
                .filter(|n| !report_names.contains(n))
                .map(str::to_string)
                .collect(),
            custom: self
                .custom
                .iter()
                .map(|name| CustomTemplate {
                    path: format!("{TEMPLATES_DIR}/{name}{TEMPLATE_SUFFIX}"),
                    overrides_builtin: is_builtin(name),
                    name: name.clone(),
                })
                .collect(),
        }
    }

    /// Errors with a message listing what exists unless a template called
    /// `name` is registered. Project templates win over built-ins of the same
    /// name, so "registered" means: `.handoff/templates/` first, then built-in.
    pub fn require_template(&self, name: &str) -> Result<()> {
        validate_template_name(name)?;
        if self.registry.has_template(name) {
            return Ok(());
        }
        let catalog = self.catalog();
        let custom: Vec<&str> = catalog.custom.iter().map(|c| c.name.as_str()).collect();
        bail!(
            "Report template '{name}' not found: looked for `.handoff/{TEMPLATES_DIR}/{name}{TEMPLATE_SUFFIX}`, then the built-in templates. Built-in: {}. Project templates: {}.",
            catalog.builtin.join(", "),
            if custom.is_empty() {
                "(none)".to_string()
            } else {
                custom.join(", ")
            }
        )
    }

    /// Renders the registered template `name` with `context`.
    pub fn render_named(&self, name: &str, context: &Value) -> Result<String> {
        self.require_template(name)?;
        self.registry
            .render(name, context)
            .with_context(|| format!("Failed to render template '{name}'"))
    }

    /// Renders the Markdown for `meta`: its `template` when set, else the
    /// template named after `meta.report_type`. The template sees
    /// `{ report: <meta>, data: <data> }`.
    pub fn generate(&self, meta: &ReportMeta, data: &Value) -> Result<String> {
        Ok(self.render(meta, data, None)?.markdown)
    }

    /// Like [`generate`](Self::generate), recomposed from `sections` when
    /// given (see [`sections`]).
    pub fn render(
        &self,
        meta: &ReportMeta,
        data: &Value,
        sections: Option<&[SectionSpec]>,
    ) -> Result<Composed> {
        let template = meta
            .template
            .as_deref()
            .unwrap_or_else(|| meta.report_type.template_name());
        if !self.registry.has_template(template) {
            bail!(
                "No template registered for report type '{}'",
                meta.report_type.as_str()
            );
        }
        let context = json!({ "report": meta, "data": data });
        let markdown = self
            .registry
            .render(template, &context)
            .with_context(|| format!("Failed to render template '{template}'"))?;
        match sections {
            None => Ok(Composed {
                markdown,
                warnings: Vec::new(),
            }),
            Some(specs) => sections::compose(self, &markdown, specs, &context),
        }
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
            template: None,
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
            (Approved, Published),
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
    fn every_report_type_renders_ai_sections_in_order() {
        let engine = ReportEngine::new().unwrap();
        let data = json!({
            "ai_recommendations": "REC-TEXT",
            "ai_risks": "RISK-TEXT",
            "ai_analysis": "ANALYSIS-TEXT",
            "ai_summary": "SUMMARY-TEXT <b> & more",
        });
        for t in ReportType::ALL {
            let md = engine.generate(&meta(t), &data).unwrap();
            let positions: Vec<usize> = [
                "## AI Summary\n\nSUMMARY-TEXT <b> & more\n",
                "## AI Analysis\n\nANALYSIS-TEXT\n",
                "## AI Risks\n\nRISK-TEXT\n",
                "## AI Recommendations\n\nREC-TEXT\n",
            ]
            .iter()
            .map(|needle| {
                md.find(needle)
                    .unwrap_or_else(|| panic!("{} lacks {needle:?}:\n{md}", t.as_str()))
            })
            .collect();
            assert!(
                positions.windows(2).all(|w| w[0] < w[1]),
                "{}: wrong order:\n{md}",
                t.as_str()
            );
        }
    }

    #[test]
    fn ai_sections_are_hidden_without_ai_keys_and_shown_individually() {
        let engine = ReportEngine::new().unwrap();
        for t in ReportType::ALL {
            let md = engine.generate(&meta(t), &json!({})).unwrap();
            assert!(!md.contains("## AI "), "{}:\n{md}", t.as_str());
            let md = engine
                .generate(&meta(t), &json!({ "ai_summary": "", "ai_risks": "R" }))
                .unwrap();
            assert!(!md.contains("## AI Summary"), "{}:\n{md}", t.as_str());
            assert!(md.contains("## AI Risks\n\nR\n"), "{}:\n{md}", t.as_str());
            assert!(!md.contains("## AI Analysis"), "{}", t.as_str());
        }
    }

    #[test]
    fn signed_prints_an_explicit_plus_only_for_positive_numbers() {
        let mut engine = ReportEngine::new().unwrap();
        engine
            .registry
            .register_template_string(
                "weekly",
                "{{signed data.a}}|{{signed data.b}}|{{signed data.c}}|{{signed data.d}}",
            )
            .unwrap();
        let out = engine
            .generate(
                &meta(ReportType::Weekly),
                &json!({ "a": 2.5, "b": -1.0, "c": 0.0, "d": null }),
            )
            .unwrap();
        assert_eq!(out, "+2.5|-1.0|0.0|-");
    }

    #[test]
    fn meta_template_selects_the_template_and_unknown_names_list_the_options() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(TEMPLATES_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mine.md.hbs"), "MINE {{report.report_type}}").unwrap();
        let mut engine = ReportEngine::new().unwrap();
        engine.load_custom_templates(tmp.path()).unwrap();

        let mut m = meta(ReportType::Weekly);
        m.template = Some("mine".into());
        assert_eq!(engine.generate(&m, &json!({})).unwrap(), "MINE weekly");

        let msg = engine.require_template("absent").unwrap_err().to_string();
        assert!(msg.contains(".handoff/templates/absent.md.hbs"), "{msg}");
        assert!(msg.contains("weekly") && msg.contains("mine"), "{msg}");
        assert!(engine.require_template("../x").is_err());
        assert!(engine.require_template("mine").is_ok());
        assert!(engine.require_template("weekly").is_ok());
    }

    #[test]
    fn catalog_separates_report_templates_partials_and_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(TEMPLATES_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("weekly.md.hbs"), "w").unwrap();
        std::fs::write(dir.join("zed.md.hbs"), "z").unwrap();
        let mut engine = ReportEngine::new().unwrap();
        engine.load_custom_templates(tmp.path()).unwrap();
        let c = engine.catalog();
        assert_eq!(c.builtin.len(), ReportType::ALL.len());
        assert_eq!(c.partials, vec!["ai_sections"]);
        assert_eq!(
            c.custom
                .iter()
                .map(|t| (t.name.as_str(), t.overrides_builtin))
                .collect::<Vec<_>>(),
            vec![("weekly", true), ("zed", false)]
        );
    }

    #[test]
    fn periodic_report_types_are_registered_and_listed() {
        for name in ["daily", "quarterly", "yearly"] {
            let t = ReportType::parse(name).unwrap();
            assert_eq!(t.as_str(), name);
            assert_eq!(t.template_name(), name);
            assert!(ReportType::ALL.contains(&t));
        }
        let err = ReportType::parse("hourly").unwrap_err().to_string();
        assert!(
            err.contains("daily") && err.contains("quarterly") && err.contains("yearly"),
            "{err}"
        );
        let engine = ReportEngine::new().unwrap();
        assert_eq!(engine.catalog().builtin.len(), ReportType::ALL.len());
    }

    #[test]
    fn scope_rejects_unknown_fields() {
        let r: Result<ReportScope, _> = serde_json::from_value(json!({ "lable": "x" }));
        assert!(r.is_err());
    }
}
