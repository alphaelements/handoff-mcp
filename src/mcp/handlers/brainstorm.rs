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

/// Required coverage areas, in presentation order (spec §5.1).
const REQUIRED_TOPICS: [&str; 5] = [
    "vision",
    "motivation",
    "target-users",
    "core-value",
    "scope",
];

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
    #[serde(default)]
    undecided: Vec<String>,
    #[serde(default)]
    priorities: Vec<String>,
    #[serde(default)]
    research_topics: Vec<String>,
    #[serde(default)]
    requirement_hints: Vec<String>,
    finalized_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BrainstormState {
    brainstorm_id: String,
    topic: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project_name: Option<String>,
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
        log_doc_id: String,
        report_doc_id: String,
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
            status: Phase::Dialogue,
            log_doc_id,
            report_doc_id,
            coverage,
            optional_topics: Vec::new(),
            decisions: Vec::new(),
            open_questions: Vec::new(),
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

/// Decisions tagged with anything other than the topics that own a dedicated
/// report section (technical decisions, constraints, hardware, security,
/// regulatory, custom tags); they fill `## 7. 技術的決定事項とリスク`.
fn constraint_decisions(state: &BrainstormState) -> String {
    const OWNED: [&str; 5] = [
        "vision",
        "motivation",
        "target-users",
        "core-value",
        "scope",
    ];
    let lines: Vec<String> = state
        .decisions
        .iter()
        .filter(|d| !OWNED.contains(&d.topic_tag.as_str()))
        .map(|d| format!("- [{}] {}", d.topic_tag, one_line(&d.text)))
        .collect();
    lines.join("\n")
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
         ### 5.3 未決定事項\n\n{undecided}\n\n\
         {research}\n\n\
         ## 7. 技術的決定事項とリスク\n\n{constraints}\n\n\
         ## 8. 優先順位\n\n{priorities}\n\n\
         ## 9. 要件定義への申し送り\n\n\
         ### 9.1 requirement-loop へのインプット\n\n{hints}\n\n\
         ### 9.2 research-loop へのリサーチ課題\n\n{research_topics}\n",
        title = one_line(&state.topic),
        summary = or_pending(
            fin.map(|f| escape_headings(&f.executive_summary))
                .unwrap_or_default()
        ),
        vision = or_pending(decisions_for(state, &["vision", "motivation"], true)),
        users = or_pending(decisions_for(state, &["target-users"], false)),
        value = or_pending(decisions_for(state, &["core-value"], false)),
        scope_in = items(SCOPE_KIND, |f| &f.scope_in),
        scope_out = list(|f| &f.scope_out),
        constraints = or_pending(constraint_decisions(state)),
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
        log_doc_id.clone(),
        String::new(),
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

    to_json(&json!({
        "brainstorm_id": id,
        "log_doc_id": log_doc_id,
        "report_doc_id": report_doc_id,
        "coverage": state.coverage_json(),
    }))
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
        let open_before = state.open_questions.len();
        state
            .open_questions
            .retain(|q| !resolved_questions.contains(q));
        let questions_resolved = state.open_questions.len() != open_before;
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

    to_json(&json!({
        "brainstorm_id": state.brainstorm_id,
        "topic": state.topic,
        "phase": state.status,
        "turn_count": state.turn_count,
        "coverage": state.coverage_json(),
        "uncovered_topics": state.uncovered_topics(),
        "required_covered": state.required_covered(),
        "decisions": decisions,
        "open_questions": state.open_questions,
        "topics": topics,
        "log_doc_id": state.log_doc_id,
        "report_doc_id": state.report_doc_id,
        "created_at": state.created_at,
        "updated_at": state.updated_at,
    }))
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
        undecided: string_list(arguments, "undecided")?,
        priorities: string_list(arguments, "priorities")?,
        research_topics: string_list(arguments, "research_topics")?,
        requirement_hints: string_list(arguments, "requirement_hints")?,
        finalized_at: now.clone(),
    };

    with_session_lock(handoff, &id, || {
        let mut state = load_state(handoff, &id)?;
        state.ensure_open()?;

        let mut warnings: Vec<String> = match state.uncovered_required().as_slice() {
            [] => Vec::new(),
            missing => vec![format!(
                "required topics not covered: {}",
                missing.join(", ")
            )],
        };
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
}
