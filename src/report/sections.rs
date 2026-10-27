//! Custom report composition (REQ-PDF-034): `handoff_report generate`'s
//! `sections` argument picks and orders the sections of a rendered report.
//!
//! The template (built-in or project) is rendered as usual; its Markdown is
//! then split at its level-2 headings (`## ...`, outside code fences) and the
//! requested sections are put back together in the requested order. The text
//! before the first heading (title and document-control block) always stays
//! first, and a `## Warnings` section is never dropped: it carries notes such
//! as a trace diagram that was left out.

use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use serde_json::Value;

use super::ReportEngine;

/// Heading of the section that lists report warnings; always kept.
const WARNINGS_HEADING: &str = "Warnings";
/// Heading of the charts section (`include_charts` controls its content).
const CHARTS_HEADING: &str = "Charts";
/// Heading of the traceability diagram section.
const TRACE_DIAGRAM_HEADING: &str = "Traceability Diagram";

/// Headings `metrics` stands for, in order of preference (the first one the
/// report has is used): the headline numbers differ per report type.
const METRICS_HEADINGS: &[&str] = &["Summary", "Overview", "Schedule"];
/// Headings `task_list` stands for; every one the report has is used.
const TASK_LIST_HEADINGS: &[&str] = &[
    "Completed Tasks",
    "In Progress",
    "Blockers",
    "Tasks",
    "Defects",
];
/// Headings `ai_sections` stands for, in the order the templates print them.
const AI_HEADINGS: &[&str] = &[
    "AI Summary",
    "AI Analysis",
    "AI Risks",
    "AI Recommendations",
];

/// One entry of the `sections` argument.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SectionSpec {
    /// The report's headline numbers (`Summary`, else `Overview`/`Schedule`).
    Metrics,
    /// The Mermaid charts (generated unless `include_charts` is false).
    Charts,
    /// The traceability link diagram (verification and completion reports).
    TraceDiagram,
    /// `data.ai_summary`.
    AiSummary,
    /// `data.ai_analysis`.
    AiAnalysis,
    /// `data.ai_risks`.
    AiRisks,
    /// `data.ai_recommendations`.
    AiRecommendations,
    /// All four AI sections, in their usual order.
    AiSections,
    /// The task tables (`Completed Tasks`, `In Progress`, `Blockers`, `Tasks`,
    /// `Defects`: whichever the report has).
    TaskList,
    /// Any section of the rendered report, by its heading text
    /// (case-insensitive).
    Section { heading: String },
    /// Free content: literal Markdown `body`, and/or a registered `template`
    /// (a `.handoff/templates/<name>.md.hbs` file or a built-in) rendered with
    /// the report's `{report, data}` context. A `title` becomes the heading.
    Custom {
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        body: Option<String>,
        #[serde(default)]
        template: Option<String>,
    },
}

impl SectionSpec {
    /// Parses the `sections` argument. Errors name the offending entry.
    pub fn parse_list(value: &Value) -> Result<Vec<SectionSpec>> {
        let Value::Array(entries) = value else {
            bail!("'sections' must be an array of {{type, ...}} objects");
        };
        if entries.is_empty() {
            bail!("'sections' must not be empty (omit it for the full report)");
        }
        entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                reject_unknown_keys(entry).map_err(|e| anyhow!("Invalid sections[{i}]: {e}"))?;
                let spec: SectionSpec = serde_json::from_value(entry.clone())
                    .map_err(|e| anyhow!("Invalid sections[{i}]: {e}"))?;
                spec.validate()
                    .map_err(|e| anyhow!("Invalid sections[{i}]: {e}"))?;
                Ok(spec)
            })
            .collect()
    }

    fn validate(&self) -> Result<()> {
        match self {
            SectionSpec::Section { heading } if heading.trim().is_empty() => {
                bail!("'heading' must not be empty")
            }
            SectionSpec::Custom {
                body: None,
                template: None,
                ..
            } => bail!("a custom section needs a 'body' and/or a 'template'"),
            _ => Ok(()),
        }
    }

    /// The report headings this entry selects (`Custom` selects none).
    fn headings(&self) -> Vec<&str> {
        match self {
            SectionSpec::Metrics => METRICS_HEADINGS.to_vec(),
            SectionSpec::Charts => vec![CHARTS_HEADING],
            SectionSpec::TraceDiagram => vec![TRACE_DIAGRAM_HEADING],
            SectionSpec::AiSummary => vec![AI_HEADINGS[0]],
            SectionSpec::AiAnalysis => vec![AI_HEADINGS[1]],
            SectionSpec::AiRisks => vec![AI_HEADINGS[2]],
            SectionSpec::AiRecommendations => vec![AI_HEADINGS[3]],
            SectionSpec::AiSections => AI_HEADINGS.to_vec(),
            SectionSpec::TaskList => TASK_LIST_HEADINGS.to_vec(),
            SectionSpec::Section { heading } => vec![heading.trim()],
            SectionSpec::Custom { .. } => Vec::new(),
        }
    }

    /// Whether this entry shows the section called `heading`.
    fn selects(&self, heading: &str) -> bool {
        self.headings()
            .iter()
            .any(|h| h.eq_ignore_ascii_case(heading))
    }

    /// Name used in warnings.
    fn label(&self) -> String {
        match self {
            SectionSpec::Metrics => "metrics".into(),
            SectionSpec::Charts => "charts".into(),
            SectionSpec::TraceDiagram => "trace_diagram".into(),
            SectionSpec::AiSummary => "ai_summary".into(),
            SectionSpec::AiAnalysis => "ai_analysis".into(),
            SectionSpec::AiRisks => "ai_risks".into(),
            SectionSpec::AiRecommendations => "ai_recommendations".into(),
            SectionSpec::AiSections => "ai_sections".into(),
            SectionSpec::TaskList => "task_list".into(),
            SectionSpec::Section { heading } => format!("section '{}'", heading.trim()),
            SectionSpec::Custom { title, .. } => {
                format!("custom '{}'", title.as_deref().unwrap_or(""))
            }
        }
    }
}

/// Whether `sections` shows the charts section.
pub fn selects_charts(sections: &[SectionSpec]) -> bool {
    sections.iter().any(|s| s.selects(CHARTS_HEADING))
}

/// Whether `sections` shows the traceability diagram section.
pub fn selects_trace_diagram(sections: &[SectionSpec]) -> bool {
    sections.iter().any(|s| s.selects(TRACE_DIAGRAM_HEADING))
}

/// serde ignores unknown keys on the unit variants of an internally tagged
/// enum even with `deny_unknown_fields`, so a typo such as `{type: "metrics",
/// heading: ...}` would pass silently; check the keys against the type here.
fn reject_unknown_keys(entry: &Value) -> Result<()> {
    let (Some(map), Some(kind)) = (entry.as_object(), entry.get("type").and_then(Value::as_str))
    else {
        return Ok(());
    };
    let allowed: &[&str] = match kind {
        "section" => &["type", "heading"],
        "custom" => &["type", "title", "body", "template"],
        _ => &["type"],
    };
    if let Some(key) = map.keys().find(|k| !allowed.contains(&k.as_str())) {
        let expected = if allowed.len() > 1 {
            allowed[1..].join(", ")
        } else {
            "none besides 'type'".to_string()
        };
        bail!("unknown field '{key}' for type '{kind}' (allowed: {expected})");
    }
    Ok(())
}

/// A rendered report split at its level-2 headings.
struct Split {
    /// Everything before the first heading.
    preamble: String,
    /// `(heading text, full section text including the heading line)`.
    sections: Vec<(String, String)>,
}

fn is_fence(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("```") || t.starts_with("~~~")
}

fn split_markdown(markdown: &str) -> Split {
    let mut preamble = String::new();
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut in_fence = false;
    for line in markdown.split_inclusive('\n') {
        if is_fence(line) {
            in_fence = !in_fence;
        }
        let heading = (!in_fence)
            .then(|| line.strip_prefix("## "))
            .flatten()
            .map(|h| h.trim().to_string());
        match (heading, sections.last_mut()) {
            (Some(h), _) => sections.push((h, line.to_string())),
            (None, Some((_, text))) => text.push_str(line),
            (None, None) => preamble.push_str(line),
        }
    }
    Split { preamble, sections }
}

/// Result of [`compose`].
#[derive(Debug, Default)]
pub struct Composed {
    pub markdown: String,
    /// Entries that showed nothing, with what the report does have.
    pub warnings: Vec<String>,
}

/// Rebuilds `rendered` from `specs`; `context` is what `custom` templates see.
pub fn compose(
    engine: &ReportEngine,
    rendered: &str,
    specs: &[SectionSpec],
    context: &Value,
) -> Result<Composed> {
    let split = split_markdown(rendered);
    let available: Vec<&str> = split.sections.iter().map(|(h, _)| h.as_str()).collect();
    let mut used = vec![false; split.sections.len()];
    let mut parts: Vec<String> = vec![split.preamble.trim_end().to_string()];
    let mut warnings = Vec::new();

    for spec in specs {
        if let SectionSpec::Custom {
            title,
            body,
            template,
        } = spec
        {
            let mut text = String::new();
            if let Some(title) = title {
                text.push_str(&format!("## {}\n\n", title.trim()));
            }
            if let Some(body) = body {
                text.push_str(body.trim_end());
                text.push('\n');
            }
            if let Some(template) = template {
                text.push_str(engine.render_named(template, context)?.trim_end());
                text.push('\n');
            }
            parts.push(text.trim_end().to_string());
            continue;
        }

        let wanted = spec.headings();
        let is_metrics = matches!(spec, SectionSpec::Metrics);
        let mut shown = false;
        for heading in wanted {
            let Some(i) = split
                .sections
                .iter()
                .position(|(h, _)| h.eq_ignore_ascii_case(heading))
            else {
                continue;
            };
            shown = true;
            if !used[i] {
                used[i] = true;
                parts.push(split.sections[i].1.trim_end().to_string());
            }
            if is_metrics {
                break;
            }
        }
        if !shown {
            warnings.push(format!(
                "sections: {} matched nothing in this report (it has: {})",
                spec.label(),
                if available.is_empty() {
                    "no sections".to_string()
                } else {
                    available.join(", ")
                }
            ));
        }
    }

    for (i, (heading, text)) in split.sections.iter().enumerate() {
        if !used[i] && heading.eq_ignore_ascii_case(WARNINGS_HEADING) {
            parts.push(text.trim_end().to_string());
        }
    }

    parts.retain(|p| !p.is_empty());
    let mut markdown = parts.join("\n\n");
    markdown.push('\n');
    Ok(Composed { markdown, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn specs(v: Value) -> Vec<SectionSpec> {
        SectionSpec::parse_list(&v).unwrap()
    }

    const DOC: &str = "# Title\n\n- id: 1\n\n## Summary\n\nsum\n\n## Charts\n\n```mermaid\n## not a heading\n```\n\n## Completed Tasks\n\ndone\n\n## Warnings\n\n- w\n";

    #[test]
    fn split_ignores_headings_inside_code_fences() {
        let s = split_markdown(DOC);
        let headings: Vec<&str> = s.sections.iter().map(|(h, _)| h.as_str()).collect();
        assert_eq!(
            headings,
            ["Summary", "Charts", "Completed Tasks", "Warnings"]
        );
        assert_eq!(s.preamble, "# Title\n\n- id: 1\n\n");
        assert!(s.sections[1].1.contains("## not a heading"));
    }

    #[test]
    fn level_three_headings_stay_inside_their_section() {
        let s = split_markdown("## A\n\n### Sub\n\nx\n\n## B\n");
        assert_eq!(s.sections.len(), 2);
        assert!(s.sections[0].1.contains("### Sub"));
    }

    #[test]
    fn compose_orders_sections_and_keeps_preamble_and_warnings() {
        let engine = ReportEngine::new().unwrap();
        let out = compose(
            &engine,
            DOC,
            &specs(json!([{ "type": "task_list" }, { "type": "metrics" }])),
            &json!({}),
        )
        .unwrap();
        assert_eq!(
            out.markdown,
            "# Title\n\n- id: 1\n\n## Completed Tasks\n\ndone\n\n## Summary\n\nsum\n\n## Warnings\n\n- w\n"
        );
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn compose_does_not_repeat_a_section_selected_twice() {
        let engine = ReportEngine::new().unwrap();
        let out = compose(
            &engine,
            DOC,
            &specs(json!([
                { "type": "section", "heading": "summary" },
                { "type": "metrics" },
            ])),
            &json!({}),
        )
        .unwrap();
        assert_eq!(out.markdown.matches("## Summary").count(), 1);
    }

    #[test]
    fn compose_warns_with_the_available_headings() {
        let engine = ReportEngine::new().unwrap();
        let out = compose(
            &engine,
            DOC,
            &specs(json!([{ "type": "section", "heading": "Nope" }])),
            &json!({}),
        )
        .unwrap();
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0].contains("section 'Nope'"));
        assert!(out.warnings[0].contains("Summary, Charts, Completed Tasks, Warnings"));
    }

    #[test]
    fn metrics_uses_only_the_first_matching_heading() {
        let engine = ReportEngine::new().unwrap();
        let out = compose(
            &engine,
            "# T\n\n## Overview\n\no\n\n## Summary\n\ns\n",
            &specs(json!([{ "type": "metrics" }])),
            &json!({}),
        )
        .unwrap();
        assert!(out.markdown.contains("## Summary"));
        assert!(!out.markdown.contains("## Overview"));
    }

    #[test]
    fn custom_body_title_and_template() {
        let engine = ReportEngine::new().unwrap();
        let out = compose(
            &engine,
            "# T\n",
            &specs(json!([
                { "type": "custom", "title": "Notes", "body": "plain {{x}}" },
                { "type": "custom", "template": "ai_sections" },
            ])),
            &json!({ "data": { "ai_risks": "R" } }),
        )
        .unwrap();
        assert!(out.markdown.contains("## Notes\n\nplain {{x}}\n"));
        assert!(out.markdown.contains("## AI Risks\n\nR\n"));
    }

    #[test]
    fn custom_with_an_unknown_template_is_an_error() {
        let engine = ReportEngine::new().unwrap();
        let err = compose(
            &engine,
            "# T\n",
            &specs(json!([{ "type": "custom", "template": "nope" }])),
            &json!({}),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("nope"));
    }

    #[test]
    fn parse_list_rejects_bad_entries() {
        for bad in [
            json!("x"),
            json!([]),
            json!([{ "type": "bogus" }]),
            json!([{ "type": "section" }]),
            json!([{ "type": "section", "heading": " " }]),
            json!([{ "type": "custom" }]),
            json!([{ "type": "metrics", "extra": true }]),
            json!(["metrics"]),
        ] {
            assert!(SectionSpec::parse_list(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn selects_charts_and_trace_diagram() {
        assert!(selects_charts(&specs(json!([{ "type": "charts" }]))));
        assert!(selects_charts(&specs(
            json!([{ "type": "section", "heading": "charts" }])
        )));
        assert!(!selects_charts(&specs(json!([{ "type": "metrics" }]))));
        assert!(selects_trace_diagram(&specs(
            json!([{ "type": "trace_diagram" }])
        )));
        assert!(!selects_trace_diagram(&specs(
            json!([{ "type": "charts" }])
        )));
    }
}
