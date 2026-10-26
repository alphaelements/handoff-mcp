//! `handoff_brainstorm_*` — the brainstorming session tools
//! (wiki/360-brainstorm-feature-spec.md §4-§6, §10).
//!
//! A brainstorm session is three artifacts kept in step:
//!
//! * the **session state** `.handoff/brainstorm/<brainstorm_id>.json` — the
//!   source of truth for turn count, coverage, decisions and phase, so a
//!   session resumes after a process restart (§10);
//! * the **deliberation log**, a plain handoff-doc (no layer) that gains one
//!   `## Turn N` section per `handoff_brainstorm_turn`;
//! * the **report**, a handoff-doc rendered from the state on every change. It
//!   has no layer while the session is open; `handoff_brainstorm_finalize`
//!   files it under the `brainstorm` layer (when the project declares it) and
//!   renders scope, priority and hint entries as `BS-<KIND>-NNN` items so
//!   requirement docs can `refines` them (wiki/360 §9). The `## 6. 調査結果` block is the one part it does
//!   not own — the research workflow writes there — so each re-render carries
//!   it over verbatim.
//!
//! Documents are written through [`super::docs::handle_doc_save`] so section
//! indexing, caches and link syncing stay in one place. The state file is
//! written last: if a document write fails, the state still describes what the
//! caller was told, and a retry replays the turn.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::docs::{handle_doc_delete, handle_doc_save};
use super::HandlerContext;

/// Sub-directory of `.handoff/` holding one `<brainstorm_id>.json` per session.
const BRAINSTORM_DIR: &str = "brainstorm";
const ID_PREFIX: &str = "bs-";
/// Upper bound on a topic tag; keeps log headings and the state file tidy.
const MAX_TAG_LEN: usize = 64;

/// `project_type` prefix (case-insensitive) that marks a GUI project
/// (REQ-GUI-BS-001); e.g. `gui-app`, `gui-desktop`, `gui-mobile`.
const GUI_PROJECT_TYPE_PREFIX: &str = "gui-";

/// Topics promoted to recommended for a GUI project, in the order the
/// coordinator should raise them (REQ-GUI-BS-001). They are advice returned
/// by start/status, not part of the required-coverage tracker.
const GUI_RECOMMENDED_TOPICS: [&str; 4] = [
    "ui-ux-vision",
    "wireframe-review",
    "visual-identity",
    "gui-verification-strategy",
];

/// Upper bound on a `project_type` / milestone name.
const MAX_LABEL_LEN: usize = 64;

/// Cell text for a milestone with no scope items assigned.
const MILESTONE_SCOPE_EMPTY: &str = "—";

/// Required coverage areas, in presentation order (spec §5.1).
const REQUIRED_TOPICS: [&str; 5] = [
    "vision",
    "motivation",
    "target-users",
    "core-value",
    "scope",
];

/// Heading of the GUI design overview, rendered for GUI projects only.
const GUI_SECTION_HEADING: &str = "## 8. GUI 設計概要";

/// Layer id the finalized report is filed under (wiki/360 §9.1).
const BRAINSTORM_LAYER: &str = "brainstorm";
/// Stable-id prefix of the report's traceable items; it must match the
/// `id_prefixes` of the project's `brainstorm` layer declaration.
const BS_ID_PREFIX: &str = "BS";

const REPORT_PENDING: &str = "_(未記入)_";
const SCOPE_KIND: &str = "SCOPE";
const PRIORITY_KIND: &str = "PRIORITY";
const HINT_KIND: &str = "HINT";
const REPORT_RESEARCH_HEADING: &str = "## 6. 調査結果";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CoverageStatus {
    NotStarted,
    InProgress,
    Covered,
}

impl CoverageStatus {
    fn parse(s: &str) -> Result<Self> {
        match s {
            "not_started" => Ok(Self::NotStarted),
            "in_progress" => Ok(Self::InProgress),
            "covered" => Ok(Self::Covered),
            other => anyhow::bail!(
                "invalid coverage status '{other}' (expected not_started, in_progress or covered)"
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Dialogue,
    Research,
    Convergence,
    Finalized,
}

impl Phase {
    /// Parses a phase a caller may *move to* via `handoff_brainstorm_turn`;
    /// `finalized` is reachable only through `handoff_brainstorm_finalize`.
    fn parse_settable(s: &str) -> Result<Self> {
        match s {
            "dialogue" => Ok(Self::Dialogue),
            "research" => Ok(Self::Research),
            "convergence" => Ok(Self::Convergence),
            "finalized" => anyhow::bail!(
                "'phase' cannot be set to finalized; call handoff_brainstorm_finalize"
            ),
            other => anyhow::bail!(
                "invalid phase '{other}' (expected dialogue, research or convergence)"
            ),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Decision {
    turn: u32,
    topic_tag: String,
    text: String,
}

/// What `handoff_brainstorm_finalize` was given; kept in the state so the
/// report can be re-rendered from the state alone.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FinalReport {
    executive_summary: String,
    scope_in: Vec<String>,
    #[serde(default)]
    scope_out: Vec<String>,
    /// Scope items per target milestone (REQ-MS-PIPE-001); empty without
    /// `target_milestones`.
    #[serde(default)]
    milestone_scope: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    undecided: Vec<String>,
    #[serde(default)]
    priorities: Vec<String>,
    #[serde(default)]
    research_topics: Vec<String>,
    #[serde(default)]
    requirement_hints: Vec<String>,
    /// GUI projects only (REQ-GUI-BS-002): rendered in `## 8. GUI 設計概要`.
    #[serde(default)]
    screens: Vec<Screen>,
    #[serde(default)]
    screen_flow: Vec<String>,
    #[serde(default)]
    ux_policy: Vec<String>,
    finalized_at: String,
}

impl FinalReport {
    /// Whether any of the GUI-only inputs (screens, flow, UX policy) was given.
    fn has_gui_input(&self) -> bool {
        !(self.screens.is_empty() && self.screen_flow.is_empty() && self.ux_policy.is_empty())
    }
}

/// One row of the GUI report's screen list.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Screen {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    function: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    priority: Option<String>,
    /// Path or link to the wireframe / mock the screen was discussed with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wireframe_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BrainstormState {
    brainstorm_id: String,
    topic: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project_name: Option<String>,
    /// Free-form project kind (REQ-GUI-BS-001); `gui-*` promotes GUI topics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project_type: Option<String>,
    /// Milestones this brainstorm scopes work for (REQ-MS-PIPE-001), in the
    /// caller's order; empty when not given.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    target_milestones: Vec<String>,
    status: Phase,
    log_doc_id: String,
    report_doc_id: String,
    coverage: BTreeMap<String, CoverageStatus>,
    /// Non-required topics in the order they joined the tracker.
    #[serde(default)]
    optional_topics: Vec<String>,
    #[serde(default)]
    decisions: Vec<Decision>,
    #[serde(default)]
    open_questions: Vec<String>,
    /// Questions that were open and have since been answered (moved out of
    /// `open_questions` by `resolved_questions` on a turn).
    #[serde(default)]
    resolved_questions: Vec<String>,
    #[serde(default)]
    topic_turns: BTreeMap<String, u32>,
    turn_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    final_report: Option<FinalReport>,
    created_at: String,
    updated_at: String,
}

impl BrainstormState {
    fn new(
        brainstorm_id: String,
        topic: String,
        project_name: Option<String>,
        project_type: Option<String>,
        target_milestones: Vec<String>,
        log_doc_id: String,
        now: String,
    ) -> Self {
        let coverage = REQUIRED_TOPICS
            .iter()
            .map(|t| (t.to_string(), CoverageStatus::NotStarted))
            .collect();
        BrainstormState {
            brainstorm_id,
            topic,
            project_name,
            project_type,
            target_milestones,
            status: Phase::Dialogue,
            log_doc_id,
            // Filled in once the report document exists (see `handle_start`).
            report_doc_id: String::new(),
            coverage,
            optional_topics: Vec::new(),
            decisions: Vec::new(),
            open_questions: Vec::new(),
            resolved_questions: Vec::new(),
            topic_turns: BTreeMap::new(),
            turn_count: 0,
            final_report: None,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    /// Adds `tag` to the tracker as an optional topic if it is not tracked yet.
    fn ensure_topic(&mut self, tag: &str) {
        if !self.coverage.contains_key(tag) {
            self.coverage
                .insert(tag.to_string(), CoverageStatus::NotStarted);
            self.optional_topics.push(tag.to_string());
        }
    }

    /// Tracked topics: required ones first (spec order), then optional ones in
    /// the order they joined.
    fn topics_in_order(&self) -> impl Iterator<Item = &str> {
        REQUIRED_TOPICS
            .iter()
            .copied()
            .chain(self.optional_topics.iter().map(String::as_str))
    }

    fn uncovered_topics(&self) -> Vec<&str> {
        self.topics_in_order()
            .filter(|t| self.coverage.get(*t) != Some(&CoverageStatus::Covered))
            .collect()
    }

    fn uncovered_required(&self) -> Vec<&str> {
        REQUIRED_TOPICS
            .iter()
            .copied()
            .filter(|t| self.coverage.get(*t) != Some(&CoverageStatus::Covered))
            .collect()
    }

    fn required_covered(&self) -> bool {
        self.uncovered_required().is_empty()
    }

    fn coverage_json(&self) -> Value {
        let mut map = Map::new();
        for topic in self.topics_in_order() {
            if let Some(status) = self.coverage.get(topic) {
                map.insert(topic.to_string(), json!(status));
            }
        }
        Value::Object(map)
    }

    /// Whether the session's `project_type` matches `gui-*`.
    fn is_gui(&self) -> bool {
        self.project_type
            .as_deref()
            .is_some_and(is_gui_project_type)
    }

    /// Topics to raise in addition to the required five; empty unless
    /// `project_type` is a GUI type.
    fn recommended_topics(&self) -> Vec<&'static str> {
        match &self.project_type {
            Some(t) if is_gui_project_type(t) => GUI_RECOMMENDED_TOPICS.to_vec(),
            _ => Vec::new(),
        }
    }

    /// `project_type`, `target_milestones` and `recommended_topics`, present
    /// only when set so a session without them reports exactly as before.
    fn extension_json(&self) -> Map<String, Value> {
        let mut map = Map::new();
        if let Some(t) = &self.project_type {
            map.insert("project_type".into(), json!(t));
        }
        if !self.target_milestones.is_empty() {
            map.insert("target_milestones".into(), json!(self.target_milestones));
        }
        let recommended = self.recommended_topics();
        if !recommended.is_empty() {
            map.insert("recommended_topics".into(), json!(recommended));
        }
        map
    }

    fn ensure_open(&self) -> Result<()> {
        if self.status == Phase::Finalized {
            anyhow::bail!(
                "Brainstorm session {} is already finalized; start a new session instead",
                self.brainstorm_id
            );
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Input parsing
// ---------------------------------------------------------------------------

fn required_str(arguments: &Value, key: &str) -> Result<String> {
    let value = arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    value
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("'{key}' parameter is required"))
}

fn optional_str(arguments: &Value, key: &str) -> Result<Option<String>> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            let s = s.trim();
            Ok((!s.is_empty()).then(|| s.to_string()))
        }
        Some(_) => anyhow::bail!("'{key}' must be a string"),
    }
}

/// An optional array of strings; blank entries are dropped, a non-string entry
/// is an error (silently skipping it would lose what the caller meant to record).
fn string_list(arguments: &Value, key: &str) -> Result<Vec<String>> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let s = item
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("'{key}' must be an array of strings"))?
                    .trim();
                if !s.is_empty() {
                    out.push(s.to_string());
                }
            }
            Ok(out)
        }
        Some(_) => anyhow::bail!("'{key}' must be an array of strings"),
    }
}

/// Whether `project_type` matches the `gui-*` pattern (case-insensitive, with
/// something after the dash).
fn is_gui_project_type(project_type: &str) -> bool {
    let t = project_type.to_ascii_lowercase();
    t.strip_prefix(GUI_PROJECT_TYPE_PREFIX)
        .is_some_and(|rest| !rest.is_empty())
}

/// A one-line label (project type, milestone name): bounded and free of
/// control characters so it is safe in the state file, JSON and Markdown cells.
fn validate_label(key: &str, label: &str) -> Result<()> {
    if label.chars().count() > MAX_LABEL_LEN {
        anyhow::bail!("'{key}' entry '{label}' is longer than {MAX_LABEL_LEN} characters");
    }
    if label.chars().any(char::is_control) {
        anyhow::bail!("'{key}' entry must not contain control characters");
    }
    Ok(())
}

/// The optional `target_milestones` array: trimmed, blanks dropped, each entry a
/// valid label, duplicates refused (a repeated milestone would render twice).
fn parse_target_milestones(arguments: &Value) -> Result<Vec<String>> {
    let milestones = string_list(arguments, "target_milestones")?;
    for (i, m) in milestones.iter().enumerate() {
        validate_label("target_milestones", m)?;
        if milestones[..i].contains(m) {
            anyhow::bail!("'target_milestones' lists '{m}' more than once");
        }
    }
    Ok(milestones)
}

/// The optional `project_type` string (see [`validate_label`]).
fn parse_project_type(arguments: &Value) -> Result<Option<String>> {
    let project_type = optional_str(arguments, "project_type")?;
    if let Some(t) = &project_type {
        validate_label("project_type", t)?;
    }
    Ok(project_type)
}

/// The optional finalize `milestone_scope` object `{milestone: [items]}`.
/// Every key must be one of the session's `target_milestones`: a scope for a
/// milestone the table has no row for would be silently lost.
fn parse_milestone_scope(
    arguments: &Value,
    target_milestones: &[String],
) -> Result<BTreeMap<String, Vec<String>>> {
    let map = match arguments.get("milestone_scope") {
        None | Some(Value::Null) => return Ok(BTreeMap::new()),
        Some(Value::Object(map)) => map,
        Some(_) => anyhow::bail!("'milestone_scope' must be an object of {{milestone: [items]}}"),
    };
    let mut out = BTreeMap::new();
    for (milestone, items) in map {
        if !target_milestones.contains(milestone) {
            anyhow::bail!(
                "'milestone_scope' names milestone '{milestone}' which is not in this session's \
                 target_milestones ({}); pass target_milestones to handoff_brainstorm_start",
                if target_milestones.is_empty() {
                    "none given".to_string()
                } else {
                    target_milestones.join(", ")
                }
            );
        }
        out.insert(
            milestone.clone(),
            string_list(&json!({ "milestone_scope": items }), "milestone_scope")?,
        );
    }
    Ok(out)
}

/// Validates a caller-supplied session id. The id becomes a file name, so
/// anything but `bs-` followed by lowercase alphanumerics and dashes is refused.
fn validate_brainstorm_id(id: &str) -> Result<()> {
    let ok = id.len() > ID_PREFIX.len()
        && id.starts_with(ID_PREFIX)
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !ok {
        anyhow::bail!(
            "invalid brainstorm_id '{id}' (expected the 'bs-...' id returned by handoff_brainstorm_start)"
        );
    }
    Ok(())
}

/// Normalizes a topic tag (trim + lowercase) and rejects anything that would
/// corrupt the `[tag]` log heading.
fn normalize_tag(raw: &str) -> Result<String> {
    let tag = raw.trim().to_lowercase();
    if tag.is_empty() {
        anyhow::bail!("topic tag must not be empty");
    }
    if tag.chars().count() > MAX_TAG_LEN {
        anyhow::bail!("topic tag '{tag}' is longer than {MAX_TAG_LEN} characters");
    }
    if tag.chars().any(|c| c.is_control() || c == '[' || c == ']') {
        anyhow::bail!("topic tag '{tag}' must not contain control characters or brackets");
    }
    Ok(tag)
}

fn parse_coverage_update(arguments: &Value) -> Result<Vec<(String, CoverageStatus)>> {
    match arguments.get("coverage_update") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Object(map)) => map
            .iter()
            .map(|(topic, status)| {
                let status = status.as_str().ok_or_else(|| {
                    anyhow::anyhow!("'coverage_update.{topic}' must be a status string")
                })?;
                Ok((normalize_tag(topic)?, CoverageStatus::parse(status)?))
            })
            .collect(),
        Some(_) => anyhow::bail!("'coverage_update' must be an object of {{topic: status}}"),
    }
}

// ---------------------------------------------------------------------------
// Markdown helpers
// ---------------------------------------------------------------------------

/// An optional array of screen objects `{name, function?, priority?,
/// wireframe_ref?}`. A malformed entry is an error rather than a skipped row:
/// dropping it would silently lose a screen the caller meant to record.
fn parse_screens(arguments: &Value) -> Result<Vec<Screen>> {
    const SHAPE: &str =
        "'screens' must be an array of {name, function?, priority?, wireframe_ref?} objects";
    let items = match arguments.get("screens") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(items)) => items,
        Some(_) => anyhow::bail!(SHAPE),
    };
    items
        .iter()
        .map(|item| {
            if !item.is_object() {
                anyhow::bail!(SHAPE);
            }
            Ok(Screen {
                name: required_str(item, "name").map_err(|_| {
                    anyhow::anyhow!("each 'screens' entry needs a non-empty 'name'")
                })?,
                function: optional_str(item, "function")?,
                priority: optional_str(item, "priority")?,
                wireframe_ref: optional_str(item, "wireframe_ref")?,
            })
        })
        .collect()
}

/// Collapses a value to one line (titles, bullets).
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Escapes level-1/2 ATX headings in user text so it cannot open a new
/// document section (docs are split at `##`) or masquerade as a title.
fn escape_headings(text: &str) -> String {
    text.lines()
        .map(|line| {
            let t = line.trim_start();
            let is_h1_or_h2 = t == "#" || t == "##" || t.starts_with("# ") || t.starts_with("## ");
            if is_h1_or_h2 {
                format!("\\{line}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn bullets(items: &[String]) -> String {
    items
        .iter()
        .map(|i| format!("- {}", one_line(i)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Renders `entries` as traceable report items, one `#### BS-<kind>-NNN <text>`
/// heading each (wiki/360 §9.3), numbered in list order. Ids are stable for a
/// finalized report because the lists are fixed at finalize time. The text is
/// collapsed to one line so a multi-line entry cannot open a heading of its own.
fn bs_items(kind: &str, entries: &[String]) -> String {
    entries
        .iter()
        .enumerate()
        .map(|(i, e)| format!("#### {BS_ID_PREFIX}-{kind}-{:03} {}", i + 1, one_line(e)))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The ids [`bs_items`] renders for `entries`.
fn bs_item_ids(kind: &str, entries: &[String]) -> Vec<String> {
    (1..=entries.len())
        .map(|n| format!("{BS_ID_PREFIX}-{kind}-{n:03}"))
        .collect()
}

fn or_pending(body: String) -> String {
    if body.trim().is_empty() {
        REPORT_PENDING.to_string()
    } else {
        body
    }
}

/// Extracts the level-2 block starting at the line equal to `heading` and
/// running to (excluding) the next level-2 heading; `None` when absent.
fn extract_h2_block(body: &str, heading: &str) -> Option<String> {
    let mut lines = body.lines();
    let mut block = Vec::new();
    for line in lines.by_ref() {
        if line.trim_end() == heading {
            block.push(line);
            break;
        }
    }
    if block.is_empty() {
        return None;
    }
    for line in lines {
        if line.starts_with("## ") {
            break;
        }
        block.push(line);
    }
    Some(block.join("\n").trim_end().to_string())
}

fn default_research_block() -> String {
    format!(
        "{REPORT_RESEARCH_HEADING}\n\n### 6.1 技術的実現可能性\n\n{p}\n\n### 6.2 既存ソリューション\n\n{p}\n\n### 6.3 競合分析\n\n{p}\n\n### 6.4 法務・規制\n\n{p}",
        p = REPORT_PENDING
    )
}

fn decisions_for(state: &BrainstormState, tags: &[&str], show_tag: bool) -> String {
    let lines: Vec<String> = state
        .decisions
        .iter()
        .filter(|d| tags.contains(&d.topic_tag.as_str()))
        .map(|d| {
            if show_tag {
                format!("- [{}] {}", d.topic_tag, one_line(&d.text))
            } else {
                format!("- {}", one_line(&d.text))
            }
        })
        .collect();
    lines.join("\n")
}

/// Topics that own a dedicated report section (§2-§5); decisions under any
/// other tag (hardware, security, regulatory, custom tags) are classified by
/// content into §5.4 or §7.
const OWNED_TOPICS: [&str; 5] = [
    "vision",
    "motivation",
    "target-users",
    "core-value",
    "scope",
];

/// Keywords marking a decision as a risk, constraint or open concern. They are
/// checked first and include the negated forms ("未決定") of the design
/// keywords, so "決定" never wins over its own negation.
const RISK_KEYWORDS: [&str; 11] = [
    "リスク",
    "制約",
    "注意",
    "懸念",
    "制限",
    "要検討",
    "要確認",
    "未決定",
    "未確定",
    "未定",
    "課題",
];

/// Keywords marking a decision as a settled design choice.
const TECHNICAL_KEYWORDS: [&str; 7] = ["決定", "確定", "採用", "採択", "選定", "使用する", "構成"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecisionKind {
    /// A settled design choice; rendered in §5.4.
    Technical,
    /// A risk, constraint or unclassifiable note; rendered in §7.
    Risk,
}

/// Classifies a decision by what it says rather than by its topic tag. Risk
/// keywords win over design keywords, and text with neither stays in §7: the
/// conservative bucket is the one a reader scans for problems.
fn classify_decision(text: &str) -> DecisionKind {
    if RISK_KEYWORDS.iter().any(|k| text.contains(k)) {
        DecisionKind::Risk
    } else if TECHNICAL_KEYWORDS.iter().any(|k| text.contains(k)) {
        DecisionKind::Technical
    } else {
        DecisionKind::Risk
    }
}

/// Decisions outside the section-owning topics that are of the given kind,
/// as tagged bullet lines.
fn classified_decisions(state: &BrainstormState, kind: DecisionKind) -> String {
    let lines: Vec<String> = state
        .decisions
        .iter()
        .filter(|d| !OWNED_TOPICS.contains(&d.topic_tag.as_str()))
        .filter(|d| classify_decision(&d.text) == kind)
        .map(|d| format!("- [{}] {}", d.topic_tag, one_line(&d.text)))
        .collect();
    lines.join("\n")
}

/// Settled design choices (`### 5.4`); empty when there are none, in which case
/// the subsection is omitted rather than shown as a placeholder.
fn technical_decisions_block(state: &BrainstormState) -> String {
    let body = classified_decisions(state, DecisionKind::Technical);
    if body.is_empty() {
        String::new()
    } else {
        format!("\n\n### 5.4 確定した技術的決定\n\n{body}")
    }
}

/// Escapes a Markdown table cell: pipes would split it, newlines end the row.
fn table_cell(text: &str) -> String {
    one_line(text).replace('|', "\\|")
}

/// `### 5.5` — scope per target milestone, one row per milestone in
/// `target_milestones` order; empty when the session has no target milestones.
fn milestone_scope_block(state: &BrainstormState) -> String {
    if state.target_milestones.is_empty() {
        return String::new();
    }
    let fin = state.final_report.as_ref();
    let rows: Vec<String> = state
        .target_milestones
        .iter()
        .map(|m| {
            let items = fin
                .and_then(|f| f.milestone_scope.get(m))
                .map(|v| {
                    v.iter()
                        .map(|i| table_cell(i))
                        .collect::<Vec<_>>()
                        .join("<br>")
                })
                .unwrap_or_default();
            let cell = match (items.is_empty(), fin) {
                (false, _) => items,
                (true, Some(_)) => MILESTONE_SCOPE_EMPTY.to_string(),
                (true, None) => REPORT_PENDING.to_string(),
            };
            format!("| {} | {cell} |", table_cell(m))
        })
        .collect();
    format!(
        "\n\n### 5.5 マイルストーン別スコープ\n\n| マイルストーン | スコープ項目 |\n| --- | --- |\n{}",
        rows.join("\n")
    )
}

/// Risks, constraints and unclassified notes; fill `## 7. 技術的決定事項とリスク`.
fn risks_and_constraints(state: &BrainstormState) -> String {
    classified_decisions(state, DecisionKind::Risk)
}

/// A screen-table cell: [`table_cell`] of the text, `-` when absent or blank.
fn screen_cell(text: Option<&str>) -> String {
    match text.map(table_cell) {
        Some(cell) if !cell.is_empty() => cell,
        _ => "-".to_string(),
    }
}

/// The screen list as a table; empty when there are no screens, so the caller
/// shows the pending placeholder instead of a header-only table.
fn screens_table(screens: &[Screen]) -> String {
    if screens.is_empty() {
        return String::new();
    }
    let mut lines = vec![
        "| 名前 | 機能 | 優先度 | ワイヤーフレーム参照 |".to_string(),
        "|---|---|---|---|".to_string(),
    ];
    lines.extend(screens.iter().map(|s| {
        format!(
            "| {} | {} | {} | {} |",
            screen_cell(Some(&s.name)),
            screen_cell(s.function.as_deref()),
            screen_cell(s.priority.as_deref()),
            screen_cell(s.wireframe_ref.as_deref()),
        )
    }));
    lines.join("\n")
}

/// `## 8. GUI 設計概要` (REQ-GUI-BS-002) with its trailing blank line, or the
/// empty string for a project that is not `gui-*`.
fn gui_section(state: &BrainstormState) -> String {
    if !state.is_gui() {
        return String::new();
    }
    let fin = state.final_report.as_ref();
    let screens = or_pending(fin.map(|f| screens_table(&f.screens)).unwrap_or_default());
    let flow = or_pending(fin.map(|f| bullets(&f.screen_flow)).unwrap_or_default());
    let ux = or_pending(fin.map(|f| bullets(&f.ux_policy)).unwrap_or_default());
    format!(
        "{GUI_SECTION_HEADING}\n\n\
         ### 8.1 画面一覧\n\n{screens}\n\n\
         ### 8.2 画面遷移図\n\n{flow}\n\n\
         ### 8.3 UX 方針\n\n{ux}\n\n"
    )
}

/// Renders the whole report from the state. `research_block` is the existing
/// `## 6. 調査結果` block to carry over (see the module docs).
fn render_report(state: &BrainstormState, research_block: Option<&str>) -> String {
    let fin = state.final_report.as_ref();
    let list = |pick: fn(&FinalReport) -> &Vec<String>| -> String {
        or_pending(fin.map(|f| bullets(pick(f))).unwrap_or_default())
    };
    let items = |kind: &str, pick: fn(&FinalReport) -> &Vec<String>| -> String {
        or_pending(fin.map(|f| bs_items(kind, pick(f))).unwrap_or_default())
    };
    let scope_dialogue = decisions_for(state, &["scope"], false);
    let scope_intro = if scope_dialogue.is_empty() {
        String::new()
    } else {
        format!("{scope_dialogue}\n\n")
    };
    let undecided = match fin {
        Some(f) => or_pending(bullets(&f.undecided)),
        None => or_pending(bullets(&state.open_questions)),
    };
    let research = research_block
        .map(str::to_string)
        .unwrap_or_else(default_research_block);

    format!(
        "# ブレストレポート: {title}\n\n\
         ## 1. エグゼクティブサマリー\n\n{summary}\n\n\
         ## 2. ビジョンと動機\n\n{vision}\n\n\
         ## 3. ターゲットユーザーと利用シーン\n\n{users}\n\n\
         ## 4. コア価値と差別化\n\n{value}\n\n\
         ## 5. スコープ\n\n{scope_intro}\
         ### 5.1 In-scope\n\n{scope_in}\n\n\
         ### 5.2 Out-of-scope\n\n{scope_out}\n\n\
         ### 5.3 未決定事項\n\n{undecided}{technical}{milestones}\n\n\
         {research}\n\n\
         ## 7. 技術的決定事項とリスク\n\n{constraints}\n\n\
         {gui}\
         ## {n_priorities}. 優先順位\n\n{priorities}\n\n\
         ## {n_handoff}. 要件定義への申し送り\n\n\
         ### {n_handoff}.1 requirement-loop へのインプット\n\n{hints}\n\n\
         ### {n_handoff}.2 research-loop へのリサーチ課題\n\n{research_topics}\n",
        title = one_line(&state.topic),
        gui = gui_section(state),
        // A GUI project's §8 pushes the sections after it down by one.
        n_priorities = if state.is_gui() { 9 } else { 8 },
        n_handoff = if state.is_gui() { 10 } else { 9 },
        summary = or_pending(
            fin.map(|f| escape_headings(&f.executive_summary))
                .unwrap_or_default()
        ),
        vision = or_pending(decisions_for(state, &["vision", "motivation"], true)),
        users = or_pending(decisions_for(state, &["target-users"], false)),
        value = or_pending(decisions_for(state, &["core-value"], false)),
        scope_in = items(SCOPE_KIND, |f| &f.scope_in),
        scope_out = list(|f| &f.scope_out),
        technical = technical_decisions_block(state),
        milestones = milestone_scope_block(state),
        constraints = or_pending(risks_and_constraints(state)),
        priorities = items(PRIORITY_KIND, |f| &f.priorities),
        hints = items(HINT_KIND, |f| &f.requirement_hints),
        research_topics = list(|f| &f.research_topics),
    )
}

fn render_log_header(topic: &str, initial_context: Option<&str>) -> String {
    let mut out = format!("# ブレスト検討ログ: {}\n", one_line(topic));
    if let Some(ctx) = initial_context {
        out.push_str(&format!(
            "\n## 初期コンテキスト\n\n{}\n",
            escape_headings(ctx)
        ));
    }
    out
}

fn render_log_entry(
    turn: u32,
    timestamp: &str,
    tag: &str,
    summary: &str,
    decisions: &[String],
    open_questions: &[String],
) -> String {
    let mut out = format!(
        "## Turn {turn} — {timestamp} [{tag}]\n\n{}\n",
        escape_headings(summary)
    );
    if !decisions.is_empty() {
        out.push_str(&format!("\n**確定事項**:\n\n{}\n", bullets(decisions)));
    }
    if !open_questions.is_empty() {
        out.push_str(&format!("\n**未解決**:\n\n{}\n", bullets(open_questions)));
    }
    out
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

fn brainstorm_dir(handoff: &Path) -> PathBuf {
    handoff.join(BRAINSTORM_DIR)
}

fn state_path(handoff: &Path, id: &str) -> PathBuf {
    brainstorm_dir(handoff).join(format!("{id}.json"))
}

fn load_state(handoff: &Path, id: &str) -> Result<BrainstormState> {
    validate_brainstorm_id(id)?;
    let path = state_path(handoff, id);
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            anyhow::bail!("Brainstorm session not found: {id}")
        }
        Err(e) => {
            return Err(e)
                .with_context(|| format!("Failed to read brainstorm state: {}", path.display()))
        }
    };
    serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse brainstorm state: {}", path.display()))
}

fn save_state(handoff: &Path, state: &BrainstormState) -> Result<()> {
    let dir = brainstorm_dir(handoff);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create brainstorm dir: {}", dir.display()))?;
    let content = serde_json::to_string_pretty(state)
        .with_context(|| "Failed to serialize brainstorm state")?;
    crate::storage::atomic_write(
        state_path(handoff, &state.brainstorm_id),
        content.as_bytes(),
    )
}

/// Runs `f` while holding a cross-process exclusive lock on the session, so two
/// server processes cannot interleave a read-modify-write of the same state.
fn with_session_lock<T>(handoff: &Path, id: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let dir = brainstorm_dir(handoff);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create brainstorm dir: {}", dir.display()))?;
    let lock_path = dir.join(format!("{id}.lock"));
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("Failed to open lock file: {}", lock_path.display()))?;
    fs2::FileExt::lock_exclusive(&lock)
        .with_context(|| format!("Failed to lock {}", lock_path.display()))?;
    // The lock is released when `lock` is dropped at the end of this scope.
    f()
}

fn generate_brainstorm_id() -> String {
    let now = Utc::now();
    format!(
        "{ID_PREFIX}{}-{:06}",
        now.format("%Y%m%d-%H%M%S"),
        now.timestamp_subsec_micros()
    )
}

// ---------------------------------------------------------------------------
// Document plumbing
// ---------------------------------------------------------------------------

/// Calls `handoff_doc_save` and returns its parsed JSON response.
fn save_doc(ctx: &HandlerContext, arguments: Value) -> Result<Value> {
    let response = handle_doc_save(ctx, &arguments)?;
    serde_json::from_str(&response)
        .with_context(|| format!("handoff_doc_save returned non-JSON output: {response}"))
}

fn doc_id_of(saved: &Value) -> Result<String> {
    saved
        .get("doc_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("handoff_doc_save response has no doc_id: {saved}"))
}

fn append_to_log(ctx: &HandlerContext, state: &BrainstormState, entry: &str) -> Result<()> {
    save_doc(
        ctx,
        json!({ "doc_id": state.log_doc_id, "append_body": entry }),
    )?;
    Ok(())
}

/// Reads the body of the report document currently on disk.
///
/// A report whose body file is missing is an error: re-rendering from nothing
/// would silently reset the research block the workflow wrote there.
fn read_report_body(ctx: &HandlerContext, state: &BrainstormState) -> Result<String> {
    let handoff = &ctx.handoff_dir;
    let doc = crate::storage::docs::find_doc_by_id(handoff, &state.report_doc_id)?
        .ok_or_else(|| anyhow::anyhow!("Report document not found: {}", state.report_doc_id))?;
    crate::storage::docs::read_doc_body(handoff, &doc.slug)?.ok_or_else(|| {
        anyhow::anyhow!(
            "Report document {} has no body file; refusing to re-render it over a possibly \
             lost research block",
            state.report_doc_id
        )
    })
}

/// Whether the report body still has placeholders in its research block (§6),
/// i.e. the coordinator has not filed 6.1-6.4 yet. An absent block counts as
/// unfilled: the next render falls back to the all-placeholder default.
fn research_unfilled(report_body: &str) -> bool {
    extract_h2_block(report_body, REPORT_RESEARCH_HEADING)
        .is_none_or(|block| block.contains(REPORT_PENDING))
}

/// Re-renders the report from `state`, carrying over the research block of the
/// report currently on disk. `layer` is set on the document when given.
fn rewrite_report(
    ctx: &HandlerContext,
    state: &BrainstormState,
    layer: Option<&str>,
) -> Result<()> {
    let existing = read_report_body(ctx, state)?;
    let research = extract_h2_block(&existing, REPORT_RESEARCH_HEADING);
    let body = render_report(state, research.as_deref());
    let mut args = json!({ "doc_id": state.report_doc_id, "body": body });
    if let Some(layer) = layer {
        args["layer"] = json!(layer);
    }
    save_doc(ctx, args)?;
    Ok(())
}

/// Whether the project's `[[trace.layer]]` registry declares the `brainstorm`
/// layer. A config that cannot be read counts as "not declared": the lenient
/// reader already degrades to defaults, and finalize reports the missing layer
/// in its warnings rather than failing the whole call.
fn brainstorm_layer_declared(handoff: &Path) -> bool {
    let (trace_config, _warning) = crate::storage::config::read_trace_config_lenient(handoff);
    crate::storage::docs::layer::LayerRegistry::build(&trace_config.layer)
        .get(BRAINSTORM_LAYER)
        .is_some()
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

fn to_json(v: &Value) -> Result<String> {
    serde_json::to_string_pretty(v).context("Failed to serialize response")
}

pub fn handle_start(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;
    let topic = required_str(arguments, "topic")?;
    let initial_context = optional_str(arguments, "initial_context")?;
    let project_name = optional_str(arguments, "project_name")?;
    let project_type = parse_project_type(arguments)?;
    let target_milestones = parse_target_milestones(arguments)?;

    let id = generate_brainstorm_id();
    if state_path(handoff, &id).exists() {
        anyhow::bail!("Brainstorm id collision for {id}; retry");
    }
    let title_topic = one_line(&topic);

    let log_doc_id = doc_id_of(&save_doc(
        ctx,
        json!({
            "slug": format!("{id}-log"),
            "title": format!("ブレスト検討ログ: {title_topic}"),
            "body": render_log_header(&topic, initial_context.as_deref()),
            "doc_type": "note",
            "tags": ["brainstorm", "brainstorm-log"],
            "auto_inject": "none",
        }),
    )?)?;

    let state = BrainstormState::new(
        id.clone(),
        topic,
        project_name,
        project_type,
        target_milestones,
        log_doc_id.clone(),
        now_rfc3339(),
    );
    let report_created = save_doc(
        ctx,
        json!({
            "slug": format!("{id}-report"),
            "title": format!("ブレストレポート: {title_topic}"),
            "body": render_report(&state, None),
            "doc_type": "note",
            "tags": ["brainstorm", "brainstorm-report"],
        }),
    )
    .and_then(|saved| doc_id_of(&saved));
    let report_doc_id = match report_created {
        Ok(doc_id) => doc_id,
        Err(e) => return Err(rollback(ctx, &[&log_doc_id], e)),
    };

    let state = BrainstormState {
        report_doc_id: report_doc_id.clone(),
        ..state
    };
    if let Err(e) = save_state(handoff, &state) {
        return Err(rollback(ctx, &[&log_doc_id, &report_doc_id], e));
    }

    let mut response = json!({
        "brainstorm_id": id,
        "log_doc_id": log_doc_id,
        "report_doc_id": report_doc_id,
        "coverage": state.coverage_json(),
    });
    response
        .as_object_mut()
        .expect("json! object literal")
        .extend(state.extension_json());
    to_json(&response)
}

/// Deletes the documents a failed `handoff_brainstorm_start` already created,
/// so a failed start leaves nothing behind. Returns the original error, with
/// any cleanup failure appended so it is not lost.
fn rollback(ctx: &HandlerContext, doc_ids: &[&str], cause: anyhow::Error) -> anyhow::Error {
    let failures: Vec<String> = doc_ids
        .iter()
        .filter_map(|id| {
            handle_doc_delete(ctx, &json!({ "doc_id": id }))
                .err()
                .map(|e| format!("{id}: {e}"))
        })
        .collect();
    if failures.is_empty() {
        cause
    } else {
        cause.context(format!(
            "rollback of already-created documents also failed ({})",
            failures.join("; ")
        ))
    }
}

pub fn handle_turn(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;
    let id = required_str(arguments, "brainstorm_id")?;
    let tag = normalize_tag(&required_str(arguments, "topic_tag")?)?;
    let summary = required_str(arguments, "summary")?;
    let decisions = string_list(arguments, "decisions")?;
    let open_questions = string_list(arguments, "open_questions")?;
    let resolved_questions = string_list(arguments, "resolved_questions")?;
    let coverage_update = parse_coverage_update(arguments)?;
    let phase = optional_str(arguments, "phase")?
        .map(|p| Phase::parse_settable(&p))
        .transpose()?;

    with_session_lock(handoff, &id, || {
        let mut state = load_state(handoff, &id)?;
        state.ensure_open()?;

        let turn = state.turn_count + 1;
        state.turn_count = turn;
        *state.topic_turns.entry(tag.clone()).or_insert(0) += 1;

        state.ensure_topic(&tag);
        let status = state
            .coverage
            .get_mut(&tag)
            .expect("ensure_topic registered the tag");
        if *status == CoverageStatus::NotStarted {
            *status = CoverageStatus::InProgress;
        }
        if !decisions.is_empty() {
            *status = CoverageStatus::Covered;
        }
        for text in &decisions {
            let already = state
                .decisions
                .iter()
                .any(|d| d.topic_tag == tag && d.text == *text);
            if !already {
                state.decisions.push(Decision {
                    turn,
                    topic_tag: tag.clone(),
                    text: text.clone(),
                });
            }
        }
        for question in &open_questions {
            if !state.open_questions.contains(question) {
                state.open_questions.push(question.clone());
            }
        }
        // A resolved question that was never open is ignored: the caller's
        // intent ("this is no longer open") already holds.
        let (resolved_now, still_open): (Vec<String>, Vec<String>) = state
            .open_questions
            .drain(..)
            .partition(|q| resolved_questions.contains(q));
        state.open_questions = still_open;
        let questions_resolved = !resolved_now.is_empty();
        for question in resolved_now {
            if !state.resolved_questions.contains(&question) {
                state.resolved_questions.push(question);
            }
        }
        for (topic, status) in coverage_update {
            state.ensure_topic(&topic);
            state.coverage.insert(topic, status);
        }
        if let Some(phase) = phase {
            state.status = phase;
        }
        state.updated_at = now_rfc3339();

        let entry = render_log_entry(
            turn,
            &state.updated_at,
            &tag,
            &summary,
            &decisions,
            &open_questions,
        );
        // Report first: it is idempotent to re-render, so a failure here (or
        // in the log append) leaves nothing a retry would duplicate.
        if !decisions.is_empty() || questions_resolved {
            rewrite_report(ctx, &state, None)?;
        }
        append_to_log(ctx, &state, &entry)?;
        save_state(handoff, &state)?;

        to_json(&json!({
            "turn_number": turn,
            "coverage": state.coverage_json(),
            "uncovered_topics": state.uncovered_topics(),
            "required_covered": state.required_covered(),
            "phase": state.status,
        }))
    })
}

pub fn handle_status(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let id = required_str(arguments, "brainstorm_id")?;
    let state = load_state(&ctx.handoff_dir, &id)?;

    let topics: Vec<Value> = state
        .topics_in_order()
        .map(|topic| {
            json!({
                "topic": topic,
                "required": REQUIRED_TOPICS.contains(&topic),
                "status": state.coverage.get(topic),
                "turns": state.topic_turns.get(topic).copied().unwrap_or(0),
                "decisions": state.decisions.iter().filter(|d| d.topic_tag == topic).count(),
            })
        })
        .collect();
    let decisions: Vec<&str> = state.decisions.iter().map(|d| d.text.as_str()).collect();

    let mut response = json!({
        "brainstorm_id": state.brainstorm_id,
        "topic": state.topic,
        "phase": state.status,
        "turn_count": state.turn_count,
        "coverage": state.coverage_json(),
        "uncovered_topics": state.uncovered_topics(),
        "required_covered": state.required_covered(),
        "decisions": decisions,
        "open_questions": state.open_questions,
        "resolved_questions": state.resolved_questions,
        "topics": topics,
        "log_doc_id": state.log_doc_id,
        "report_doc_id": state.report_doc_id,
        "created_at": state.created_at,
        "updated_at": state.updated_at,
    });
    response
        .as_object_mut()
        .expect("json! object literal")
        .extend(state.extension_json());
    to_json(&response)
}

pub fn handle_finalize(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;
    let id = required_str(arguments, "brainstorm_id")?;
    let executive_summary = required_str(arguments, "executive_summary")?;
    let scope_in = string_list(arguments, "scope_in")?;
    if scope_in.is_empty() {
        anyhow::bail!("'scope_in' parameter is required (at least one in-scope item)");
    }
    let now = now_rfc3339();
    let final_report = FinalReport {
        executive_summary,
        scope_in,
        scope_out: string_list(arguments, "scope_out")?,
        milestone_scope: BTreeMap::new(),
        undecided: string_list(arguments, "undecided")?,
        priorities: string_list(arguments, "priorities")?,
        research_topics: string_list(arguments, "research_topics")?,
        requirement_hints: string_list(arguments, "requirement_hints")?,
        screens: parse_screens(arguments)?,
        screen_flow: string_list(arguments, "screen_flow")?,
        ux_policy: string_list(arguments, "ux_policy")?,
        finalized_at: now.clone(),
    };

    with_session_lock(handoff, &id, || {
        let mut state = load_state(handoff, &id)?;
        state.ensure_open()?;
        // Checked against the session's milestones, so it needs the state; it
        // runs before anything is written so a refusal leaves the session open.
        let mut final_report = final_report;
        final_report.milestone_scope = parse_milestone_scope(arguments, &state.target_milestones)?;

        let mut warnings: Vec<String> = match state.uncovered_required().as_slice() {
            [] => Vec::new(),
            missing => vec![format!(
                "required topics not covered: {}",
                missing.join(", ")
            )],
        };
        if !state.is_gui() && final_report.has_gui_input() {
            // The report has no GUI section to put them in; say so rather than
            // dropping them silently.
            final_report.screens.clear();
            final_report.screen_flow.clear();
            final_report.ux_policy.clear();
            warnings.push(format!(
                "screens / screen_flow / ux_policy ignored: this session is not a gui-* project \
                 (project_type: {})",
                state.project_type.as_deref().unwrap_or("not set")
            ));
        }
        let layer = brainstorm_layer_declared(handoff).then_some(BRAINSTORM_LAYER);
        if layer.is_none() {
            warnings.push(format!(
                "the '{BRAINSTORM_LAYER}' layer is not declared in config.toml, so the report \
                 has no layer and its {BS_ID_PREFIX}-* items are not traceable yet; declare a \
                 [[trace.layer]] with id = \"{BRAINSTORM_LAYER}\", side = \"left\", level = 0, \
                 verification = false, id_prefixes = [\"{BS_ID_PREFIX}\"], then run \
                 handoff_doc_save(doc_id = \"{}\", layer = \"{BRAINSTORM_LAYER}\")",
                state.report_doc_id
            ));
        }
        if research_unfilled(&read_report_body(ctx, &state)?) {
            warnings.push(
                "research section (§6) contains unfilled subsections — the coordinator should \
                 fill §6.1-§6.4 with research results before finalizing (see brainstorm-execute \
                 SKILL.md §5)"
                    .to_string(),
            );
        }
        let item_ids: Vec<String> = [
            (SCOPE_KIND, &final_report.scope_in),
            (PRIORITY_KIND, &final_report.priorities),
            (HINT_KIND, &final_report.requirement_hints),
        ]
        .into_iter()
        .flat_map(|(kind, entries)| bs_item_ids(kind, entries))
        .collect();

        // A question answered during the dialogue is not undecided, even if the
        // caller's list still carries it.
        final_report
            .undecided
            .retain(|q| !state.resolved_questions.contains(q));
        state.final_report = Some(final_report.clone());
        state.status = Phase::Finalized;
        state.updated_at = now.clone();

        rewrite_report(ctx, &state, layer)?;
        append_to_log(
            ctx,
            &state,
            &format!(
                "## 完了 — {now}\n\nブレストを finalize しました（{} ターン）。レポート: {}\n",
                state.turn_count, state.report_doc_id
            ),
        )?;
        save_state(handoff, &state)?;

        to_json(&json!({
            "report_doc_id": state.report_doc_id,
            "log_doc_id": state.log_doc_id,
            "status": state.status,
            "layer": layer,
            "item_ids": item_ids,
            "warnings": warnings,
        }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> (tempfile::TempDir, HandlerContext) {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff_dir).unwrap();
        let ctx = HandlerContext {
            agent_id: None,
            project_dir: tmp.path().to_path_buf(),
            handoff_dir,
        };
        (tmp, ctx)
    }

    fn call(
        f: fn(&HandlerContext, &Value) -> Result<String>,
        c: &HandlerContext,
        a: Value,
    ) -> Value {
        serde_json::from_str(&f(c, &a).unwrap()).unwrap()
    }

    fn start(c: &HandlerContext) -> Value {
        call(
            handle_start,
            c,
            json!({ "topic": "IoT 温度監視", "initial_context": "冷蔵庫を見たい" }),
        )
    }

    fn body_of(c: &HandlerContext, doc_id: &str) -> String {
        let doc = crate::storage::docs::find_doc_by_id(&c.handoff_dir, doc_id)
            .unwrap()
            .unwrap();
        crate::storage::docs::read_doc_body(&c.handoff_dir, &doc.slug)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn id_validation_accepts_generated_ids_and_rejects_path_tricks() {
        validate_brainstorm_id(&generate_brainstorm_id()).unwrap();
        for bad in [
            "",
            "bs-",
            "../x",
            "bs-../x",
            "bs-A",
            "bs-a/b",
            "x-1",
            "bs-a.json",
        ] {
            assert!(
                validate_brainstorm_id(bad).is_err(),
                "{bad} must be refused"
            );
        }
    }

    #[test]
    fn tag_normalization() {
        assert_eq!(normalize_tag("  Vision ").unwrap(), "vision");
        assert!(normalize_tag("").is_err());
        assert!(normalize_tag("a[b]").is_err());
        assert!(normalize_tag("a\nb").is_err());
        assert!(normalize_tag(&"x".repeat(MAX_TAG_LEN + 1)).is_err());
    }

    #[test]
    fn escape_headings_neutralizes_h1_and_h2_only() {
        let out = escape_headings("ok\n## Evil\n  # Title\n### fine\n#hashtag");
        assert_eq!(out, "ok\n\\## Evil\n\\  # Title\n### fine\n#hashtag");
    }

    #[test]
    fn extract_h2_block_stops_at_next_h2_but_keeps_h3() {
        let body = "# T\n\n## 5. A\n\nx\n\n## 6. 調査結果\n\n### 6.1 a\n\nfoo\n\n## 7. B\n\ny\n";
        let block = extract_h2_block(body, REPORT_RESEARCH_HEADING).unwrap();
        assert_eq!(block, "## 6. 調査結果\n\n### 6.1 a\n\nfoo");
        assert!(extract_h2_block("# T\n", REPORT_RESEARCH_HEADING).is_none());
    }

    #[test]
    fn start_creates_state_and_two_layerless_docs() {
        let (_t, c) = ctx();
        let out = start(&c);
        let id = out["brainstorm_id"].as_str().unwrap();
        assert!(id.starts_with("bs-"));
        assert_eq!(out["coverage"].as_object().unwrap().len(), 5);
        assert!(state_path(&c.handoff_dir, id).is_file());
        for key in ["log_doc_id", "report_doc_id"] {
            let doc =
                crate::storage::docs::find_doc_by_id(&c.handoff_dir, out[key].as_str().unwrap())
                    .unwrap()
                    .unwrap();
            assert!(doc.layer.is_none(), "{key} must have no layer");
        }
        let log = body_of(&c, out["log_doc_id"].as_str().unwrap());
        assert!(log.starts_with("# ブレスト検討ログ: IoT 温度監視"), "{log}");
        assert!(log.contains("冷蔵庫を見たい"));
    }

    #[test]
    fn start_requires_topic() {
        let (_t, c) = ctx();
        assert!(handle_start(&c, &json!({})).is_err());
        assert!(handle_start(&c, &json!({ "topic": "  " })).is_err());
        assert!(handle_start(&c, &json!({ "topic": "x", "initial_context": 3 })).is_err());
    }

    #[test]
    fn turn_without_decisions_is_in_progress_and_leaves_report_untouched() {
        let (_t, c) = ctx();
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        let report_before = body_of(&c, s["report_doc_id"].as_str().unwrap());
        let t = call(
            handle_turn,
            &c,
            json!({ "brainstorm_id": id, "topic_tag": "Vision", "summary": "s" }),
        );
        assert_eq!(t["turn_number"], 1);
        assert_eq!(t["coverage"]["vision"], "in_progress");
        assert_eq!(t["required_covered"], false);
        assert_eq!(
            report_before,
            body_of(&c, s["report_doc_id"].as_str().unwrap())
        );
    }

    #[test]
    fn turn_with_decisions_covers_topic_and_updates_report_sections() {
        let (_t, c) = ctx();
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        for (tag, d) in [
            ("target-users", "工場の保守担当"),
            ("regulatory", "HACCP に準拠"),
        ] {
            call(
                handle_turn,
                &c,
                json!({ "brainstorm_id": id, "topic_tag": tag, "summary": "s", "decisions": [d] }),
            );
        }
        let report = body_of(&c, s["report_doc_id"].as_str().unwrap());
        let users = extract_h2_block(&report, "## 3. ターゲットユーザーと利用シーン").unwrap();
        assert!(users.contains("工場の保守担当"), "{report}");
        let risks = extract_h2_block(&report, "## 7. 技術的決定事項とリスク").unwrap();
        assert!(risks.contains("[regulatory] HACCP に準拠"), "{report}");
        let st = call(handle_status, &c, json!({ "brainstorm_id": id }));
        assert_eq!(st["coverage"]["target-users"], "covered");
        assert_eq!(st["coverage"]["regulatory"], "covered");
        assert_eq!(st["decisions"], json!(["工場の保守担当", "HACCP に準拠"]));
    }

    #[test]
    fn required_covered_flips_when_all_five_are_covered() {
        let (_t, c) = ctx();
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        let mut last = Value::Null;
        for tag in REQUIRED_TOPICS {
            last = call(
                handle_turn,
                &c,
                json!({ "brainstorm_id": id, "topic_tag": tag, "summary": "s", "decisions": ["d"] }),
            );
        }
        assert_eq!(last["required_covered"], true);
        assert_eq!(last["uncovered_topics"], json!([]));
    }

    #[test]
    fn covered_topic_is_not_downgraded_by_a_later_discussion_turn() {
        let (_t, c) = ctx();
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        call(
            handle_turn,
            &c,
            json!({ "brainstorm_id": id, "topic_tag": "scope", "summary": "s", "decisions": ["d"] }),
        );
        let t = call(
            handle_turn,
            &c,
            json!({ "brainstorm_id": id, "topic_tag": "scope", "summary": "again" }),
        );
        assert_eq!(t["coverage"]["scope"], "covered");
    }

    #[test]
    fn coverage_update_overrides_and_can_add_optional_topics() {
        let (_t, c) = ctx();
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        let t = call(
            handle_turn,
            &c,
            json!({
                "brainstorm_id": id, "topic_tag": "vision", "summary": "s",
                "coverage_update": { "motivation": "covered", "Security": "in_progress" }
            }),
        );
        assert_eq!(t["coverage"]["motivation"], "covered");
        assert_eq!(t["coverage"]["security"], "in_progress");
        assert!(handle_turn(
            &c,
            &json!({
                "brainstorm_id": id, "topic_tag": "vision", "summary": "s",
                "coverage_update": { "scope": "done" }
            })
        )
        .is_err());
    }

    #[test]
    fn invalid_turn_does_not_consume_a_turn_number() {
        let (_t, c) = ctx();
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        assert!(handle_turn(
            &c,
            &json!({ "brainstorm_id": id, "topic_tag": "vision", "summary": "s", "decisions": [1] })
        )
        .is_err());
        let st = call(handle_status, &c, json!({ "brainstorm_id": id }));
        assert_eq!(st["turn_count"], 0);
    }

    #[test]
    fn phase_can_be_moved_but_not_to_finalized() {
        let (_t, c) = ctx();
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        let t = call(
            handle_turn,
            &c,
            json!({ "brainstorm_id": id, "topic_tag": "vision", "summary": "s", "phase": "research" }),
        );
        assert_eq!(t["phase"], "research");
        let st = call(handle_status, &c, json!({ "brainstorm_id": id }));
        assert_eq!(st["phase"], "research");
        for bad in ["finalized", "nonsense"] {
            assert!(handle_turn(
                &c,
                &json!({ "brainstorm_id": id, "topic_tag": "vision", "summary": "s", "phase": bad })
            )
            .is_err());
        }
    }

    #[test]
    fn summary_headings_cannot_split_the_log_into_fake_sections() {
        let (_t, c) = ctx();
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        call(
            handle_turn,
            &c,
            json!({ "brainstorm_id": id, "topic_tag": "vision", "summary": "a\n## Turn 99 — fake" }),
        );
        let log = body_of(&c, s["log_doc_id"].as_str().unwrap());
        assert!(!log.lines().any(|l| l.starts_with("## Turn 99")), "{log}");
        assert!(log.contains("\\## Turn 99"), "{log}");
    }

    #[test]
    fn finalize_preserves_research_block_written_by_the_workflow() {
        let (_t, c) = ctx();
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        let report_id = s["report_doc_id"].as_str().unwrap();
        // The research workflow edits the report body directly.
        let report = body_of(&c, report_id);
        let edited = report.replacen(
            "### 6.1 技術的実現可能性\n\n_(未記入)_",
            "### 6.1 技術的実現可能性\n\nLoRaWAN で届く",
            1,
        );
        assert_ne!(report, edited);
        save_doc(&c, json!({ "doc_id": report_id, "body": edited })).unwrap();

        call(
            handle_finalize,
            &c,
            json!({ "brainstorm_id": id, "executive_summary": "要約", "scope_in": ["a"] }),
        );
        let final_report = body_of(&c, report_id);
        assert!(final_report.contains("LoRaWAN で届く"), "{final_report}");
        assert!(final_report.contains("要約"));
    }

    #[test]
    fn finalize_warns_when_the_research_section_is_still_unfilled() {
        let (_t, c) = ctx();
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        let fin = call(handle_finalize, &c, finalize_args(id));
        let warnings = fin["warnings"].to_string();
        assert!(warnings.contains("§6"), "{warnings}");
        assert!(warnings.contains("unfilled"), "{warnings}");
        // A warning only: the session is still finalized.
        assert_eq!(fin["status"], "finalized");
    }

    #[test]
    fn finalize_does_not_warn_when_the_research_section_is_filled() {
        let (_t, c) = ctx();
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        let report_id = s["report_doc_id"].as_str().unwrap();
        // Only the research block is filled in; other sections keep their placeholders.
        let research = extract_h2_block(&body_of(&c, report_id), REPORT_RESEARCH_HEADING).unwrap();
        let filled_research = research.replace(REPORT_PENDING, "調査済み");
        let edited = body_of(&c, report_id).replace(&research, &filled_research);
        save_doc(&c, json!({ "doc_id": report_id, "body": edited })).unwrap();
        let fin = call(handle_finalize, &c, finalize_args(id));
        assert!(!fin["warnings"].to_string().contains("§6"), "{fin}");
    }

    #[test]
    fn finalize_warns_when_only_some_research_subsections_are_filled() {
        let (_t, c) = ctx();
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        let report_id = s["report_doc_id"].as_str().unwrap();
        let report = body_of(&c, report_id);
        let edited = report.replacen(
            "### 6.1 技術的実現可能性\n\n_(未記入)_",
            "### 6.1 技術的実現可能性\n\nLoRaWAN で届く",
            1,
        );
        save_doc(&c, json!({ "doc_id": report_id, "body": edited })).unwrap();
        let fin = call(handle_finalize, &c, finalize_args(id));
        assert!(fin["warnings"].to_string().contains("§6"), "{fin}");
    }

    #[test]
    fn report_section_seven_is_named_technical_decisions_and_risks() {
        let (_t, c) = ctx();
        let s = start(&c);
        let report = body_of(&c, s["report_doc_id"].as_str().unwrap());
        assert!(report.contains("## 7. 技術的決定事項とリスク"), "{report}");
        assert!(!report.contains("リスクと制約"), "{report}");
    }

    #[test]
    fn finalize_validates_and_closes_the_session() {
        let (_t, c) = ctx();
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        assert!(handle_finalize(&c, &json!({ "brainstorm_id": id, "scope_in": ["a"] })).is_err());
        assert!(handle_finalize(
            &c,
            &json!({ "brainstorm_id": id, "executive_summary": "x" })
        )
        .is_err());
        assert!(handle_finalize(
            &c,
            &json!({ "brainstorm_id": id, "executive_summary": "x", "scope_in": [] })
        )
        .is_err());

        let fin = call(
            handle_finalize,
            &c,
            json!({ "brainstorm_id": id, "executive_summary": "x", "scope_in": ["a"], "priorities": ["p1", "p2"] }),
        );
        assert_eq!(fin["status"], "finalized");
        assert!(fin["warnings"][0].as_str().unwrap().contains("vision"));
        let report = body_of(&c, s["report_doc_id"].as_str().unwrap());
        assert!(
            report.contains("#### BS-PRIORITY-001 p1\n\n#### BS-PRIORITY-002 p2"),
            "{report}"
        );

        let err = handle_finalize(
            &c,
            &json!({ "brainstorm_id": id, "executive_summary": "x", "scope_in": ["a"] }),
        )
        .unwrap_err();
        assert!(err.to_string().contains("already finalized"));
        assert!(handle_turn(
            &c,
            &json!({ "brainstorm_id": id, "topic_tag": "vision", "summary": "s" })
        )
        .is_err());
    }

    #[test]
    fn status_of_unknown_session_is_not_found() {
        let (_t, c) = ctx();
        let err = handle_status(&c, &json!({ "brainstorm_id": "bs-20000101-000000-000000" }))
            .unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
    }

    #[test]
    fn corrupt_state_file_is_an_error_not_a_blank_session() {
        let (_t, c) = ctx();
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        std::fs::write(state_path(&c.handoff_dir, id), "{not json").unwrap();
        let err = handle_status(&c, &json!({ "brainstorm_id": id })).unwrap_err();
        assert!(err.to_string().contains("parse"), "{err}");
    }

    #[test]
    fn state_is_restored_from_disk_by_a_fresh_load() {
        let (_t, c) = ctx();
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        call(
            handle_turn,
            &c,
            json!({ "brainstorm_id": id, "topic_tag": "hardware", "summary": "s", "decisions": ["d"], "open_questions": ["q"] }),
        );
        let state = load_state(&c.handoff_dir, id).unwrap();
        assert_eq!(state.turn_count, 1);
        assert_eq!(state.optional_topics, vec!["hardware".to_string()]);
        assert_eq!(state.open_questions, vec!["q".to_string()]);
        assert_eq!(state.coverage["hardware"], CoverageStatus::Covered);
    }

    #[test]
    fn resolved_questions_are_removed_from_open_questions() {
        let (_t, c) = ctx();
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        call(
            handle_turn,
            &c,
            json!({ "brainstorm_id": id, "topic_tag": "scope", "summary": "s", "open_questions": ["q1", "q2"] }),
        );
        call(
            handle_turn,
            &c,
            json!({ "brainstorm_id": id, "topic_tag": "scope", "summary": "s", "resolved_questions": ["q1"] }),
        );
        let state = load_state(&c.handoff_dir, id).unwrap();
        assert_eq!(state.open_questions, vec!["q2".to_string()]);
        let status = call(handle_status, &c, json!({ "brainstorm_id": id }));
        assert_eq!(status["open_questions"], json!(["q2"]));
    }

    #[test]
    fn resolving_an_unknown_question_is_silently_ignored() {
        let (_t, c) = ctx();
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        call(
            handle_turn,
            &c,
            json!({ "brainstorm_id": id, "topic_tag": "scope", "summary": "s", "open_questions": ["q1"] }),
        );
        call(
            handle_turn,
            &c,
            json!({ "brainstorm_id": id, "topic_tag": "scope", "summary": "s", "resolved_questions": ["never asked"] }),
        );
        let state = load_state(&c.handoff_dir, id).unwrap();
        assert_eq!(state.open_questions, vec!["q1".to_string()]);
    }

    #[test]
    fn resolved_questions_leave_the_report_undecided_section() {
        let (_t, c) = ctx();
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        let report_id = s["report_doc_id"].as_str().unwrap();
        call(
            handle_turn,
            &c,
            json!({ "brainstorm_id": id, "topic_tag": "scope", "summary": "s", "decisions": ["d"], "open_questions": ["pending-q", "kept-q"] }),
        );
        assert!(body_of(&c, report_id).contains("pending-q"));
        // A turn without decisions must still re-render the report.
        call(
            handle_turn,
            &c,
            json!({ "brainstorm_id": id, "topic_tag": "scope", "summary": "s", "resolved_questions": ["pending-q"] }),
        );
        let report = body_of(&c, report_id);
        assert!(!report.contains("pending-q"), "{report}");
        assert!(report.contains("kept-q"), "{report}");
    }

    fn section(report: &str, heading: &str) -> String {
        extract_h2_block(report, heading).unwrap_or_else(|| panic!("{heading} missing: {report}"))
    }

    fn turn_with_decisions(c: &HandlerContext, id: &str, tag: &str, decisions: Value) {
        call(
            handle_turn,
            c,
            json!({ "brainstorm_id": id, "topic_tag": tag, "summary": "s", "decisions": decisions }),
        );
    }

    #[test]
    fn classify_decision_uses_content_keywords_and_risk_wins_over_decision() {
        assert_eq!(
            classify_decision("デュアルユニット構成を採用する"),
            DecisionKind::Technical
        );
        assert_eq!(
            classify_decision("PSRAM を使用する"),
            DecisionKind::Technical
        );
        assert_eq!(
            classify_decision("技適の取得が必要なためリスクあり"),
            DecisionKind::Risk
        );
        // "決定" is a design keyword but "未決定" says the opposite.
        assert_eq!(classify_decision("電源方式は未決定"), DecisionKind::Risk);
        // Both kinds of keyword present: the conservative bucket wins.
        assert_eq!(
            classify_decision("PSRAM を採用するが容量に制限がある"),
            DecisionKind::Risk
        );
        // No keyword: conservative default.
        assert_eq!(classify_decision("バッテリは2本"), DecisionKind::Risk);
    }

    #[test]
    fn technical_decisions_go_to_section_five_and_risks_to_section_seven() {
        let (_t, c) = ctx();
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        let report_id = s["report_doc_id"].as_str().unwrap();
        turn_with_decisions(
            &c,
            id,
            "hardware",
            json!(["デュアルユニット構成を採用する", "PSRAM の容量に制限がある"]),
        );
        turn_with_decisions(
            &c,
            id,
            "regulatory",
            json!(["技適は取得済みモジュールを選定"]),
        );
        turn_with_decisions(&c, id, "security", json!(["通信の暗号化は要検討"]));
        turn_with_decisions(&c, id, "misc", json!(["キーワードなしのメモ"]));
        let fin = call(handle_finalize, &c, finalize_args(id));
        assert_eq!(fin["status"], "finalized");

        let report = body_of(&c, report_id);
        let scope = section(&report, "## 5. スコープ");
        assert!(scope.contains("### 5.4 確定した技術的決定"), "{scope}");
        assert!(
            scope.contains("[hardware] デュアルユニット構成を採用する"),
            "{scope}"
        );
        assert!(
            scope.contains("[regulatory] 技適は取得済みモジュールを選定"),
            "{scope}"
        );
        assert!(!scope.contains("PSRAM の容量"), "{scope}");

        let risks = section(&report, "## 7. 技術的決定事項とリスク");
        assert!(
            risks.contains("[hardware] PSRAM の容量に制限がある"),
            "{risks}"
        );
        assert!(risks.contains("[security] 通信の暗号化は要検討"), "{risks}");
        assert!(risks.contains("[misc] キーワードなしのメモ"), "{risks}");
        assert!(!risks.contains("デュアルユニット"), "{risks}");
        assert!(!risks.contains("技適"), "{risks}");
    }

    #[test]
    fn section_five_has_no_technical_subsection_without_technical_decisions() {
        let (_t, c) = ctx();
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        turn_with_decisions(&c, id, "hardware", json!(["電池寿命に懸念"]));
        call(handle_finalize, &c, finalize_args(id));
        let report = body_of(&c, s["report_doc_id"].as_str().unwrap());
        assert!(!report.contains("### 5.4"), "{report}");
        assert!(section(&report, "## 7. 技術的決定事項とリスク").contains("電池寿命に懸念"));
    }

    #[test]
    fn owned_topics_are_not_reclassified_by_keywords() {
        let (_t, c) = ctx();
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        turn_with_decisions(
            &c,
            id,
            "vision",
            json!(["冷蔵庫の温度を採用する視点で見る"]),
        );
        let report = body_of(&c, s["report_doc_id"].as_str().unwrap());
        assert!(
            section(&report, "## 2. ビジョンと動機").contains("冷蔵庫の温度を採用する視点で見る"),
            "{report}"
        );
        assert!(!report.contains("### 5.4"), "{report}");
    }

    #[test]
    fn resolved_questions_are_moved_to_resolved_questions() {
        let (_t, c) = ctx();
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        call(
            handle_turn,
            &c,
            json!({ "brainstorm_id": id, "topic_tag": "scope", "summary": "s", "open_questions": ["q1", "q2"] }),
        );
        call(
            handle_turn,
            &c,
            json!({ "brainstorm_id": id, "topic_tag": "scope", "summary": "s", "resolved_questions": ["q1", "never asked", "q1"] }),
        );
        let state = load_state(&c.handoff_dir, id).unwrap();
        assert_eq!(state.open_questions, vec!["q2".to_string()]);
        // Only questions that were actually open move; no duplicates.
        assert_eq!(state.resolved_questions, vec!["q1".to_string()]);
        let status = call(handle_status, &c, json!({ "brainstorm_id": id }));
        assert_eq!(status["resolved_questions"], json!(["q1"]));
    }

    #[test]
    fn finalize_moves_resolved_open_questions_out_of_undecided() {
        let (_t, c) = ctx();
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        call(
            handle_turn,
            &c,
            json!({ "brainstorm_id": id, "topic_tag": "scope", "summary": "s", "open_questions": ["q1", "q2"] }),
        );
        call(
            handle_turn,
            &c,
            json!({ "brainstorm_id": id, "topic_tag": "scope", "summary": "s", "resolved_questions": ["q1"] }),
        );
        let mut args = finalize_args(id);
        args["undecided"] = json!(["q1", "q2", "q3"]);
        call(handle_finalize, &c, args);
        let report = body_of(&c, s["report_doc_id"].as_str().unwrap());
        let scope = section(&report, "## 5. スコープ");
        assert!(!scope.contains("q1"), "{scope}");
        assert!(scope.contains("q2") && scope.contains("q3"), "{scope}");
        let state = load_state(&c.handoff_dir, id).unwrap();
        assert_eq!(
            state.final_report.unwrap().undecided,
            vec!["q2".to_string(), "q3".to_string()]
        );
    }

    #[test]
    fn finalize_keeps_all_research_subsections_from_both_streams() {
        let (_t, c) = ctx();
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        let report_id = s["report_doc_id"].as_str().unwrap();
        let research = "## 6. 調査結果\n\n### 6.1 技術的実現可能性\n\nA-feasibility\n\n#### 追加ファセット\n\nA-custom\n\n### 6.2 既存ソリューション\n\nA-existing\n\n### 6.3 競合分析\n\nA-competitors\n\n### 6.4 法務・規制\n\nB-regulatory";
        let report = body_of(&c, report_id);
        let default = extract_h2_block(&report, REPORT_RESEARCH_HEADING).unwrap();
        save_doc(
            &c,
            json!({ "doc_id": report_id, "body": report.replace(&default, research) }),
        )
        .unwrap();
        // A later turn and the finalize both re-render the report.
        turn_with_decisions(&c, id, "hardware", json!(["PSRAM を採用する"]));
        let fin = call(handle_finalize, &c, finalize_args(id));
        assert!(!fin["warnings"].to_string().contains("§6"), "{fin}");
        let after = body_of(&c, report_id);
        assert_eq!(section(&after, REPORT_RESEARCH_HEADING), research);
        // §6 stays between §5 and §7.
        let p6 = after.find("## 6. 調査結果").unwrap();
        assert!(after.find("## 5. スコープ").unwrap() < p6);
        assert!(p6 < after.find("## 7. ").unwrap());
    }

    /// Declares the `brainstorm` origin layer (wiki/360 §9.1) in the temp
    /// project's `config.toml`.
    fn declare_brainstorm_layer(c: &HandlerContext) {
        std::fs::write(
            c.handoff_dir.join("config.toml"),
            r#"[project]
name = "t"

[[trace.layer]]
id = "brainstorm"
name = "ブレインストーミング"
side = "left"
position = 0
level = 0
verification = false
id_prefixes = ["BS"]
"#,
        )
        .unwrap();
    }

    fn finalize_args(id: &str) -> Value {
        json!({
            "brainstorm_id": id,
            "executive_summary": "要約",
            "scope_in": ["オフライン同期", "アラート通知"],
            "priorities": ["P1 を先に"],
            "requirement_hints": ["温度は 1 分間隔"],
            "research_topics": ["LoRaWAN の到達距離"],
        })
    }

    #[test]
    fn finalize_renders_scope_priority_and_hint_entries_as_bs_items() {
        let (_t, c) = ctx();
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        call(handle_finalize, &c, finalize_args(id));
        let report = body_of(&c, s["report_doc_id"].as_str().unwrap());
        for expected in [
            "#### BS-SCOPE-001 オフライン同期",
            "#### BS-SCOPE-002 アラート通知",
            "#### BS-PRIORITY-001 P1 を先に",
            "#### BS-HINT-001 温度は 1 分間隔",
        ] {
            assert!(report.contains(expected), "missing {expected}: {report}");
        }
        // Research topics are tasks for research-loop, not traceable items.
        assert!(!report.contains("BS-RESEARCH"), "{report}");
    }

    #[test]
    fn finalize_sets_brainstorm_layer_when_the_registry_declares_it() {
        let (_t, c) = ctx();
        declare_brainstorm_layer(&c);
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        let fin = call(handle_finalize, &c, finalize_args(id));
        let doc = crate::storage::docs::find_doc_by_id(
            &c.handoff_dir,
            s["report_doc_id"].as_str().unwrap(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(doc.layer.as_deref(), Some("brainstorm"));
        assert_eq!(fin["layer"], "brainstorm");
        assert!(
            !fin["warnings"].to_string().contains("not declared"),
            "{fin}"
        );
        // The log doc is never a traceability layer.
        let log =
            crate::storage::docs::find_doc_by_id(&c.handoff_dir, s["log_doc_id"].as_str().unwrap())
                .unwrap()
                .unwrap();
        assert!(log.layer.is_none());
    }

    #[test]
    fn finalize_without_a_declared_layer_leaves_it_unset_and_says_how_to_fix() {
        let (_t, c) = ctx();
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        let fin = call(handle_finalize, &c, finalize_args(id));
        let doc = crate::storage::docs::find_doc_by_id(
            &c.handoff_dir,
            s["report_doc_id"].as_str().unwrap(),
        )
        .unwrap()
        .unwrap();
        assert!(doc.layer.is_none());
        assert!(fin["layer"].is_null());
        let warnings = fin["warnings"].to_string();
        assert!(warnings.contains("layer"), "{warnings}");
        assert!(warnings.contains("brainstorm"), "{warnings}");
    }

    #[test]
    fn turn_errors_without_consuming_a_turn_when_the_report_document_is_gone() {
        let (_t, c) = ctx();
        let s = start(&c);
        let id = s["brainstorm_id"].as_str().unwrap();
        let report_id = s["report_doc_id"].as_str().unwrap();
        let doc = crate::storage::docs::find_doc_by_id(&c.handoff_dir, report_id)
            .unwrap()
            .unwrap();
        let body_file = crate::storage::docs::doc_body_path(&c.handoff_dir, &doc.slug);
        std::fs::remove_file(&body_file).unwrap();
        let err = handle_turn(
            &c,
            &json!({ "brainstorm_id": id, "topic_tag": "scope", "summary": "s", "decisions": ["d"] }),
        )
        .unwrap_err();
        assert!(err.to_string().contains("Report document"), "{err}");
        // Nothing was committed to the session state.
        let st = call(handle_status, &c, json!({ "brainstorm_id": id }));
        assert_eq!(st["turn_count"], 0);
    }
    // -- REQ-GUI-BS-002: §8 GUI design overview ------------------------------

    fn start_gui(c: &HandlerContext, project_type: &str) -> Value {
        call(
            handle_start,
            c,
            json!({ "topic": "家計簿アプリ", "project_type": project_type }),
        )
    }

    fn gui_finalize_args(id: &str) -> Value {
        let mut args = finalize_args(id);
        args["screens"] = json!([
            {
                "name": "ホーム",
                "function": "今月の収支サマリ",
                "priority": "P0",
                "wireframe_ref": "wireframes/home.png"
            },
            { "name": "設定", "function": "通知 | 通貨の設定" }
        ]);
        args["screen_flow"] = json!(["ホーム → 入力", "ホーム → 設定"]);
        args["ux_policy"] = json!(["片手操作を最優先", "色だけに頼らない"]);
        args
    }

    #[test]
    fn gui_project_finalize_renders_section_eight_with_screen_table() {
        let (_t, c) = ctx();
        let s = start_gui(&c, "gui-app");
        let id = s["brainstorm_id"].as_str().unwrap();
        let fin = call(handle_finalize, &c, gui_finalize_args(id));
        assert!(!fin["warnings"].to_string().contains("screens"), "{fin}");
        let report = body_of(&c, s["report_doc_id"].as_str().unwrap());
        let gui = section(&report, "## 8. GUI 設計概要");
        for expected in [
            "### 8.1 画面一覧",
            "| 名前 | 機能 | 優先度 | ワイヤーフレーム参照 |",
            "|---|---|---|---|",
            "| ホーム | 今月の収支サマリ | P0 | wireframes/home.png |",
            "### 8.2 画面遷移図",
            "- ホーム → 入力",
            "### 8.3 UX 方針",
            "- 片手操作を最優先",
        ] {
            assert!(gui.contains(expected), "missing {expected}: {gui}");
        }
        // Cell separators in user text cannot add a column; blank cells are dashes.
        assert!(
            gui.contains("| 設定 | 通知 \\| 通貨の設定 | - | - |"),
            "{gui}"
        );
        // The sections after it shift down by one.
        for expected in [
            "## 9. 優先順位",
            "## 10. 要件定義への申し送り",
            "### 10.1 requirement-loop へのインプット",
            "### 10.2 research-loop へのリサーチ課題",
        ] {
            assert!(report.contains(expected), "missing {expected}: {report}");
        }
        assert!(!report.contains("## 8. 優先順位"), "{report}");
    }

    #[test]
    fn gui_project_without_screens_shows_pending_placeholders() {
        let (_t, c) = ctx();
        let s = start_gui(&c, "gui-desktop");
        let id = s["brainstorm_id"].as_str().unwrap();
        call(handle_finalize, &c, finalize_args(id));
        let report = body_of(&c, s["report_doc_id"].as_str().unwrap());
        let gui = section(&report, "## 8. GUI 設計概要");
        assert!(gui.contains("### 8.1 画面一覧"), "{gui}");
        assert!(gui.contains(REPORT_PENDING), "{gui}");
        assert!(!gui.contains("| 名前 |"), "no empty table: {gui}");
    }

    #[test]
    fn non_gui_project_finalize_has_no_section_eight_gui() {
        let (_t, c) = ctx();
        for s in [start(&c), start_gui(&c, "web-service")] {
            let id = s["brainstorm_id"].as_str().unwrap();
            let fin = call(handle_finalize, &c, gui_finalize_args(id));
            let report = body_of(&c, s["report_doc_id"].as_str().unwrap());
            assert!(!report.contains("GUI 設計概要"), "{report}");
            assert!(report.contains("## 8. 優先順位"), "{report}");
            assert!(report.contains("## 9. 要件定義への申し送り"), "{report}");
            assert!(
                !report.contains("ホーム"),
                "GUI input must not leak: {report}"
            );
            // The caller is told the GUI input was not used.
            assert!(
                fin["warnings"].to_string().contains("not a gui-* project"),
                "{fin}"
            );
        }
    }

    #[test]
    fn finalize_rejects_malformed_screens() {
        let (_t, c) = ctx();
        let s = start_gui(&c, "gui-app");
        let id = s["brainstorm_id"].as_str().unwrap();
        for bad in [
            json!("ホーム"),
            json!(["ホーム"]),
            json!([{ "function": "名前なし" }]),
            json!([{ "name": "ホーム", "priority": 1 }]),
        ] {
            let mut args = finalize_args(id);
            args["screens"] = bad.clone();
            assert!(handle_finalize(&c, &args).is_err(), "accepted {bad}");
        }
        // A rejected finalize leaves the session open.
        let st = call(handle_status, &c, json!({ "brainstorm_id": id }));
        assert_ne!(st["phase"], "finalized");
    }

    #[test]
    fn gui_section_sits_between_section_seven_and_the_priorities() {
        let (_t, c) = ctx();
        let s = start_gui(&c, "gui-app");
        let id = s["brainstorm_id"].as_str().unwrap();
        call(handle_finalize, &c, gui_finalize_args(id));
        let report = body_of(&c, s["report_doc_id"].as_str().unwrap());
        let p6 = report.find("## 6. 調査結果").unwrap();
        let p7 = report.find("## 7. 技術的決定事項とリスク").unwrap();
        let p8 = report.find("## 8. GUI 設計概要").unwrap();
        assert!(p6 < p7 && p7 < p8, "{report}");
    }

    fn start_with(c: &HandlerContext, extra: Value) -> Result<String> {
        let mut args = json!({ "topic": "GUI アプリ" });
        for (k, v) in extra.as_object().unwrap() {
            args[k] = v.clone();
        }
        handle_start(c, &args)
    }

    fn started(c: &HandlerContext, extra: Value) -> Value {
        serde_json::from_str(&start_with(c, extra).unwrap()).unwrap()
    }

    #[test]
    fn gui_project_type_pattern_matches_gui_prefix_only() {
        assert!(is_gui_project_type("gui-app"));
        assert!(is_gui_project_type("gui-desktop"));
        assert!(is_gui_project_type("gui-mobile"));
        assert!(is_gui_project_type("GUI-App"));
        assert!(!is_gui_project_type("desktop"));
        assert!(!is_gui_project_type("gui"));
        assert!(!is_gui_project_type("gui-"));
        assert!(!is_gui_project_type("cli-gui-app"));
        assert!(!is_gui_project_type("web-app"));
    }

    #[test]
    fn start_with_gui_project_type_recommends_gui_topics_and_persists_it() {
        let (_t, c) = ctx();
        let out = started(&c, json!({ "project_type": " gui-app " }));
        assert_eq!(out["project_type"], "gui-app");
        assert_eq!(
            out["recommended_topics"],
            json!([
                "ui-ux-vision",
                "wireframe-review",
                "visual-identity",
                "gui-verification-strategy"
            ])
        );
        // Required coverage is untouched.
        assert_eq!(out["coverage"].as_object().unwrap().len(), 5);
        let id = out["brainstorm_id"].as_str().unwrap();
        let state = load_state(&c.handoff_dir, id).unwrap();
        assert_eq!(state.project_type.as_deref(), Some("gui-app"));
        let st = call(handle_status, &c, json!({ "brainstorm_id": id }));
        assert_eq!(st["project_type"], "gui-app");
        assert_eq!(st["recommended_topics"], out["recommended_topics"]);
    }

    #[test]
    fn start_with_non_gui_project_type_recommends_nothing() {
        let (_t, c) = ctx();
        let out = started(&c, json!({ "project_type": "desktop" }));
        assert_eq!(out["project_type"], "desktop");
        assert!(out.get("recommended_topics").is_none(), "{out}");
    }

    #[test]
    fn start_without_new_params_is_unchanged() {
        let (_t, c) = ctx();
        let out = start(&c);
        for key in ["project_type", "target_milestones", "recommended_topics"] {
            assert!(out.get(key).is_none(), "{key} leaked into {out}");
        }
        let id = out["brainstorm_id"].as_str().unwrap();
        let st = call(handle_status, &c, json!({ "brainstorm_id": id }));
        for key in ["project_type", "target_milestones", "recommended_topics"] {
            assert!(st.get(key).is_none(), "{key} leaked into {st}");
        }
        call(handle_finalize, &c, finalize_args(id));
        let report = body_of(&c, out["report_doc_id"].as_str().unwrap());
        assert!(!report.contains("マイルストーン別"), "{report}");
    }

    #[test]
    fn start_rejects_malformed_project_type_and_milestones() {
        let (_t, c) = ctx();
        for bad in [
            json!({ "project_type": 5 }),
            json!({ "project_type": "gui-app\nx" }),
            json!({ "target_milestones": "v0-1" }),
            json!({ "target_milestones": [1] }),
            json!({ "target_milestones": ["v0-1", "v0-1"] }),
            json!({ "target_milestones": ["v0\n1"] }),
        ] {
            assert!(
                start_with(&c, bad.clone()).is_err(),
                "{bad} must be refused"
            );
        }
        // Nothing was left behind by the refused starts.
        assert!(!brainstorm_dir(&c.handoff_dir).exists());
    }

    #[test]
    fn target_milestones_are_persisted_and_restored() {
        let (_t, c) = ctx();
        let out = started(&c, json!({ "target_milestones": ["v0-1", " v0-2 ", ""] }));
        assert_eq!(out["target_milestones"], json!(["v0-1", "v0-2"]));
        let id = out["brainstorm_id"].as_str().unwrap();
        let state = load_state(&c.handoff_dir, id).unwrap();
        assert_eq!(state.target_milestones, vec!["v0-1", "v0-2"]);
        let st = call(handle_status, &c, json!({ "brainstorm_id": id }));
        assert_eq!(st["target_milestones"], json!(["v0-1", "v0-2"]));
    }

    #[test]
    fn finalize_renders_milestone_scope_table_in_section_5() {
        let (_t, c) = ctx();
        let out = started(&c, json!({ "target_milestones": ["v0-1", "v0-2", "v0-3"] }));
        let id = out["brainstorm_id"].as_str().unwrap();
        let mut args = finalize_args(id);
        args["milestone_scope"] = json!({
            "v0-2": ["検索 | 絞り込み", "通知"],
            "v0-1": ["ログイン"]
        });
        call(handle_finalize, &c, args);
        let report = body_of(&c, out["report_doc_id"].as_str().unwrap());
        let scope = section(&report, "## 5. スコープ");
        assert!(
            scope.contains("### 5.5 マイルストーン別スコープ"),
            "{scope}"
        );
        assert!(
            scope.contains("| マイルストーン | スコープ項目 |"),
            "{scope}"
        );
        // Rows follow target_milestones order, not the argument's key order.
        let r1 = scope.find("| v0-1 | ログイン |").expect(&scope);
        let r2 = scope
            .find("| v0-2 | 検索 \\| 絞り込み<br>通知 |")
            .expect(&scope);
        let r3 = scope.find("| v0-3 | — |").expect(&scope);
        assert!(r1 < r2 && r2 < r3, "{scope}");
        // Persisted for re-rendering.
        let state = load_state(&c.handoff_dir, id).unwrap();
        assert_eq!(
            state.final_report.unwrap().milestone_scope["v0-1"],
            vec!["ログイン"]
        );
    }

    #[test]
    fn finalize_with_target_milestones_but_no_scope_lists_every_milestone_as_empty() {
        let (_t, c) = ctx();
        let out = started(&c, json!({ "target_milestones": ["v0-1"] }));
        let id = out["brainstorm_id"].as_str().unwrap();
        call(handle_finalize, &c, finalize_args(id));
        let report = body_of(&c, out["report_doc_id"].as_str().unwrap());
        assert!(report.contains("| v0-1 | — |"), "{report}");
    }

    #[test]
    fn finalize_rejects_milestone_scope_for_unknown_milestone_or_without_targets() {
        let (_t, c) = ctx();
        let out = started(&c, json!({ "target_milestones": ["v0-1"] }));
        let id = out["brainstorm_id"].as_str().unwrap();
        let mut args = finalize_args(id);
        args["milestone_scope"] = json!({ "v9": ["x"] });
        let e = handle_finalize(&c, &args).unwrap_err().to_string();
        assert!(e.contains("v9") && e.contains("target_milestones"), "{e}");
        // The refused finalize did not close the session.
        let st = call(handle_status, &c, json!({ "brainstorm_id": id }));
        assert_ne!(st["phase"], "finalized");

        let plain = start(&c);
        let mut args = finalize_args(plain["brainstorm_id"].as_str().unwrap());
        args["milestone_scope"] = json!({ "v0-1": ["x"] });
        let e = handle_finalize(&c, &args).unwrap_err().to_string();
        assert!(e.contains("target_milestones"), "{e}");
    }
}
