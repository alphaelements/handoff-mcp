//! `handoff_trace_lint`'s pure rule-evaluation core (wiki/260-vmodel-m2-design.md
//! §4.3, t360.20.8/M2-08) — no file I/O of its own (D1, same convention as
//! `suspect.rs`/`task_view.rs`): every input is either an already-built
//! [`TraceGraph`]/[`TraceInput`] or one of the small side-channels
//! `src/mcp/handlers/trace_readonly.rs`'s E6 read-only load already computes
//! while building them (`unreadable`, `task_ids_drift`, `per_doc_sync_warnings`,
//! `resynced_doc_slugs`).

use std::collections::{HashMap, HashSet};

use crate::storage::config::{TraceLintConfig, TraceLintRequireRule};
use crate::storage::docs::{DocMetadata, UnreadableDoc};

use super::engine::TraceGraph;
use super::types::{GapKind, ItemState, SuspectKind, TaskLinkRole, TraceInput};

/// Either a stable_id whose stored `SubItem.task_ids` disagrees with what
/// the task side (`TaskLink { link_type: "requirement" }`, the authority,
/// D3) currently says, or (M2-15, wiki/260 §4.8/FR-601) a document whose
/// stored `DocMetadata.task_ids` disagrees with the task side's
/// `TaskLink { link_type: "doc" }` entries — computed by the E6 read-only
/// load (`src/mcp/handlers/trace_readonly.rs`), consumed here by the
/// `task_ids_drift` rule. `stable_id`/`doc_slug` are mutually exclusive:
/// exactly one of the two constructors below is used to build a value of
/// this type.
///
/// Document-level drift is deliberately reported rather than silently
/// corrected both ways (§4.8: "文書単位の自己修復は追加だけ") — an id present
/// in `stored` with no matching `TaskLink{doc}` on the task side (a task
/// id `doc_save(task_ids=...)` could not resolve, or one removed from the
/// task side by hand) is never dropped by self-repair, only ever reported
/// here, until an explicit `doc_save(task_ids=...)` call removes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskIdsDrift {
    /// `Some` for an item-level drift ([`TaskIdsDrift::item`]); `None` for a
    /// document-level one.
    pub stable_id: Option<String>,
    /// `Some` for a document-level drift ([`TaskIdsDrift::doc`]); `None` for
    /// an item-level one.
    pub doc_slug: Option<String>,
    pub stored: Vec<String>,
    pub derived: Vec<String>,
}

impl TaskIdsDrift {
    /// An item-level drift: `stable_id`'s stored `SubItem.task_ids` vs the
    /// task side's `TaskLink{requirement}` entries.
    pub fn item(stable_id: impl Into<String>, stored: Vec<String>, derived: Vec<String>) -> Self {
        Self {
            stable_id: Some(stable_id.into()),
            doc_slug: None,
            stored,
            derived,
        }
    }

    /// A document-level drift (M2-15): `doc_slug`'s stored
    /// `DocMetadata.task_ids` vs the task side's `TaskLink{doc}` entries.
    pub fn doc(doc_slug: impl Into<String>, stored: Vec<String>, derived: Vec<String>) -> Self {
        Self {
            stable_id: None,
            doc_slug: Some(doc_slug.into()),
            stored,
            derived,
        }
    }
}

/// A lint finding's severity (wiki/260 §4.3). Declaration order (`Info` <
/// `Warning` < `Error`) is deliberately ascending so `Ord`/`max` read as "more
/// severe wins", matching every other priority-ordered enum in this crate
/// (e.g. `ItemState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "error" => Some(Severity::Error),
            "warning" => Some(Severity::Warning),
            "info" => Some(Severity::Info),
            _ => None,
        }
    }
}

/// One `findings[]` entry (wiki/260 §4.3: `{rule, severity, item?, task?,
/// doc?, message}`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintFinding {
    pub rule: String,
    pub severity: Severity,
    pub item: Option<String>,
    pub task: Option<String>,
    pub doc: Option<String>,
    pub message: String,
}

/// Every built-in rule id and its default severity (wiki/260 §4.3's table),
/// overridable per id via `[trace.lint.rules]`.
const BUILTIN_RULES: &[(&str, Severity)] = &[
    ("unverified", Severity::Warning),
    ("unrefined", Severity::Warning),
    ("orphan", Severity::Warning),
    ("task_unlinked", Severity::Warning),
    ("dangling", Severity::Error),
    ("invalid_link", Severity::Error),
    ("cycle", Severity::Error),
    ("duplicate_id", Severity::Error),
    ("suspect_link", Severity::Warning),
    ("suspect_task", Severity::Warning),
    ("stale_result", Severity::Warning),
    ("unbaselined", Severity::Info),
    ("waiver_on_na", Severity::Warning),
    ("unlabeled_acceptance", Severity::Warning),
    ("invalid_waiver", Severity::Warning),
    ("unknown_acceptance_ref", Severity::Warning),
    ("redundant_waiver", Severity::Info),
    ("layer_outside_profile", Severity::Info),
    // M3 (wiki/270-vmodel-m3-design.md §2.1/§3.1, M3-01, FR-202): a verifier
    // whose own layer is not in its target's `needs`/`default_needs` set
    // (OFT's "Unwanted" equivalent) — info by default, overridable.
    ("unwanted_coverage", Severity::Info),
    ("unsynced_body", Severity::Warning),
    ("task_link_dangling", Severity::Warning),
    ("task_ids_drift", Severity::Info),
    ("orphaned_legacy", Severity::Info),
    ("orphan_run", Severity::Info),
    ("id_like_heading", Severity::Info),
    ("frontmatter_invalid", Severity::Error),
    // M3 (wiki/270-vmodel-m3-design.md §4.6, M3-10, FR-504): quality checks —
    // deterministically detectable by string matching alone (§6's perf
    // budget), the LLM-judgment aspects live behind
    // `trace_lint(action="quality_prompt")` instead (`src/trace/quality.rs`).
    ("ambiguous_word", Severity::Info),
    ("missing_acceptance", Severity::Info),
    ("passive_voice_hint", Severity::Info),
    // M3 (t377.5): a `- priority: ...`/`- assignee: ...`-shaped attribute
    // line written after an item's body text (instead of in the leading
    // attribute block right after its heading) is silently never applied —
    // `layer_parse.rs`'s `ParseWarningKind::AttributeAfterBody`, surfaced
    // the same way `id_like_heading`/`unlabeled_acceptance`/`invalid_waiver`
    // already are.
    ("attribute_after_body", Severity::Warning),
];

/// Every `need` value `[[trace.lint.require]]` rules recognize (§4.3's
/// table) — shared between `require_need_satisfied`'s `match` arms and
/// [`validate_lint_config`]'s rejection of an unknown one.
const KNOWN_REQUIRE_NEEDS: &[&str] = &[
    "verified_by",
    "refined_by",
    "implemented_by_task",
    "passing",
    "no_suspect",
    "auto_test",
];

/// True if `id` names either a built-in rule or one of `config.require`'s own
/// `id`s — the id union `[trace.lint.rules]` overrides, the CLI `--rules`
/// flag, and the MCP `rules` argument are all validated against (M2-08
/// rework, reviewer round 1 MAJOR finding: an override/filter naming neither
/// must be rejected, not silently ignored).
pub fn is_known_rule_id(id: &str, config: &TraceLintConfig) -> bool {
    BUILTIN_RULES.iter().any(|(r, _)| *r == id) || config.require.iter().any(|r| r.id == id)
}

/// Rejects a malformed `[trace.lint]` config before [`evaluate`] ever runs
/// (M2-08 rework, reviewer round 1 MAJOR finding). wiki/260 §4.3 reserves CLI
/// exit 2 for "usage/config error", but a broken `[[trace.lint.require]]`
/// entry or `[trace.lint.rules]` override used to vanish silently instead: an
/// entry with an empty `id`/`need` was skipped mid-`evaluate` (this module's
/// own `require` loop, below), an unknown `need`/override severity silently
/// fell back to a default (`require_need_satisfied`/`resolve_severity`), and
/// an override naming an unrecognized rule id was simply never looked up.
/// Every one of those let a `trace lint` CI gate exit 0 with an empty
/// `warnings: []` instead of catching the very policy it was configured to
/// enforce. Called once by `handoff_trace_lint`
/// (`src/mcp/handlers/trace_lint.rs`) right after loading `config.toml`, so
/// every rejection here surfaces as the handler returning `Err`, which
/// `cli.rs`'s `run()` already treats as exit 2 for this one tool.
pub fn validate_lint_config(config: &TraceLintConfig) -> Result<(), String> {
    for rule in &config.require {
        if rule.id.is_empty() {
            return Err("[[trace.lint.require]] has an entry with no `id`".to_string());
        }
        if rule.need.is_empty() || !KNOWN_REQUIRE_NEEDS.contains(&rule.need.as_str()) {
            return Err(format!(
                "[[trace.lint.require]] {:?} has an unknown `need` {:?} (expected one of {:?})",
                rule.id, rule.need, KNOWN_REQUIRE_NEEDS
            ));
        }
        if let Some(severity) = &rule.severity {
            if Severity::parse(severity).is_none() {
                return Err(format!(
                    "[[trace.lint.require]] {:?} has an unknown `severity` {:?} (expected \
                     \"error\", \"warning\", or \"info\")",
                    rule.id, severity
                ));
            }
        }
    }
    for (id, severity) in &config.rules {
        if !is_known_rule_id(id, config) {
            return Err(format!(
                "[trace.lint.rules] overrides unknown rule id {id:?} (not a built-in rule or a \
                 [[trace.lint.require]] id)"
            ));
        }
        if !matches!(severity.as_str(), "error" | "warning" | "info" | "off") {
            return Err(format!(
                "[trace.lint.rules] {id:?} has an unknown severity {severity:?} (expected \
                 \"error\", \"warning\", \"info\", or \"off\")"
            ));
        }
    }
    Ok(())
}

/// `(stable_id -> owning document slug)` and the handful of per-item fields
/// `TraceItemInput`/`TraceGraph` don't themselves carry (priority, approval,
/// doc) — built by the caller from `LoadedTrace.docs` (the same source
/// `trace.rs`'s own `collect_item_meta` reads), kept separate from
/// `TraceInput` itself per that type's own "decoupled from `SubItem`"
/// design note.
#[derive(Debug, Clone, Default)]
pub struct ItemLintMeta {
    pub doc_slug: String,
    pub priority: Option<String>,
    /// `"draft"` | `"review"` | `"approved"` (wiki/270-vmodel-m3-design.md
    /// §2.3, M3-03 — `SubItem.approval` when present, else the M2 §3.3/E12
    /// read-mapping of `SubItem.status`; same priority rule `trace.rs`'s
    /// `approval_str` applies).
    pub approval: String,
    /// `SubItem.description` (the item's title/heading text, §2.2) — the only
    /// piece of item *text* this otherwise structural/metadata map carries.
    /// M3 (wiki/270-vmodel-m3-design.md §4.6, FR-504): `ambiguous_word` and
    /// `passive_voice_hint` pattern-match this field; both rules are
    /// deliberately scoped to the title rather than the full body statement
    /// (not persisted on `SubItem`, only reconstructable by re-parsing the
    /// document body — out of scope for a read-only, metadata-only map).
    pub title: String,
}

/// Resolves rule id -> effective severity, `None` meaning `"off"` (the rule
/// produces no findings at all). An override value this crate doesn't
/// recognize (not one of `error`/`warning`/`info`/`off`) is ignored —
/// fail-safe: keep the rule at its default severity rather than silently
/// dropping it over a config typo.
fn resolve_severity(
    default: Severity,
    rule_id: &str,
    overrides: &HashMap<String, String>,
) -> Option<Severity> {
    match overrides.get(rule_id).map(String::as_str) {
        Some("off") => None,
        Some(other) => Some(Severity::parse(other).unwrap_or(default)),
        None => Some(default),
    }
}

fn gap_kind_rule_id(kind: GapKind) -> &'static str {
    match kind {
        GapKind::Unverified => "unverified",
        GapKind::Unrefined => "unrefined",
        GapKind::Orphan => "orphan",
        GapKind::TaskUnlinked => "task_unlinked",
        GapKind::Dangling => "dangling",
        GapKind::InvalidLink => "invalid_link",
        GapKind::Cycle => "cycle",
        GapKind::DuplicateId => "duplicate_id",
    }
}

fn suspect_kind_rule_id(kind: SuspectKind) -> &'static str {
    match kind {
        SuspectKind::Link => "suspect_link",
        SuspectKind::Task => "suspect_task",
        SuspectKind::Result => "stale_result",
    }
}

/// M1's "(orphaned legacy items)" catch-all heading label
/// (`src/storage/docs/layer_sync.rs`'s private `ORPHAN_LABEL` — duplicated
/// here as a literal rather than exported, since it is a stable, already
/// user-visible string, not an implementation detail that might change
/// independently of this rule's own contract).
const ORPHAN_LEGACY_LABEL: &str = "(orphaned legacy items)";

/// Everything [`evaluate`] needs beyond `graph`/`trace_input` themselves —
/// the E6 read-only load's own side channels
/// (`src/mcp/handlers/trace_readonly.rs`) plus the small per-item metadata
/// map the caller builds from `LoadedTrace.docs`.
pub struct LintContext<'a> {
    pub docs: &'a [DocMetadata],
    pub item_meta: &'a HashMap<String, ItemLintMeta>,
    pub unreadable: &'a [UnreadableDoc],
    pub task_ids_drift: &'a [TaskIdsDrift],
    /// `(doc_slug, rendered ParseWarning text)` for every in-memory resync
    /// this call ran — `id_like_heading`/`unlabeled_acceptance`/
    /// `invalid_waiver` each pattern-match this list for their own parser
    /// warning kind's rendered text (`src/storage/docs/layer_parse.rs`'s
    /// `Display` impl for `ParseWarning`).
    pub per_doc_sync_warnings: &'a [(String, String)],
    /// Document slugs whose in-memory resync actually ran this call (i.e.
    /// `sync_layer_items_local` returned `Some`) — exactly the
    /// `body_raw_hash`/`layer_sync_stamp` mismatch-or-missing condition
    /// `unsynced_body` reports (FR-801).
    pub resynced_doc_slugs: &'a HashSet<String>,
}

/// Runs every applicable rule (built-in + `[[trace.lint.require]]`) and
/// returns findings sorted deterministically (severity descending, then rule
/// id, then item/task/doc natural order — wiki/260 §4.3: "出力の順序は
/// （severity → rule → item の自然順）で決定的にする").
pub fn evaluate(
    graph: &TraceGraph,
    trace_input: &TraceInput,
    ctx: &LintContext<'_>,
    config: &TraceLintConfig,
    rules_filter: Option<&HashSet<String>>,
) -> Vec<LintFinding> {
    let wants = |id: &str| rules_filter.is_none_or(|f| f.contains(id));
    let mut out = Vec::new();

    // --- Structural (M1 gaps) ---
    for gap in graph.gaps() {
        let id = gap_kind_rule_id(gap.kind);
        if !wants(id) {
            continue;
        }
        let default = BUILTIN_RULES
            .iter()
            .find(|(r, _)| *r == id)
            .map(|(_, s)| *s)
            .unwrap_or(Severity::Warning);
        let Some(severity) = resolve_severity(default, id, &config.rules) else {
            continue;
        };
        out.push(LintFinding {
            rule: id.to_string(),
            severity,
            item: gap
                .item
                .clone()
                .filter(|_| gap.kind != GapKind::TaskUnlinked),
            task: gap
                .item
                .clone()
                .filter(|_| gap.kind == GapKind::TaskUnlinked),
            doc: gap
                .item
                .as_deref()
                .and_then(|i| ctx.item_meta.get(i))
                .map(|m| m.doc_slug.clone()),
            message: gap.detail.clone(),
        });
    }

    // --- Change (suspect/unbaselined) ---
    for s in graph.suspects() {
        let id = suspect_kind_rule_id(s.kind);
        if !wants(id) {
            continue;
        }
        let default = BUILTIN_RULES
            .iter()
            .find(|(r, _)| *r == id)
            .map(|(_, sev)| *sev)
            .unwrap_or(Severity::Warning);
        let Some(severity) = resolve_severity(default, id, &config.rules) else {
            continue;
        };
        let message = match s.kind {
            SuspectKind::Link => format!(
                "{} {} {} has changed since this {} link's baseline was recorded",
                s.upstream.as_deref().unwrap_or(""),
                s.link_type.as_deref().unwrap_or("refines/verifies"),
                s.item,
                s.link_type.as_deref().unwrap_or("")
            ),
            SuspectKind::Task => format!(
                "{} changed since task {}'s link was baselined",
                s.item,
                s.task.as_deref().unwrap_or("")
            ),
            SuspectKind::Result => format!(
                "{}'s latest recorded pass is against a prior definition",
                s.item
            ),
        };
        out.push(LintFinding {
            rule: id.to_string(),
            severity,
            item: Some(s.item.clone()),
            task: s.task.clone(),
            doc: ctx.item_meta.get(&s.item).map(|m| m.doc_slug.clone()),
            message,
        });
    }

    if wants("unbaselined") {
        if let Some(severity) = resolve_severity(Severity::Info, "unbaselined", &config.rules) {
            for l in graph.unbaselined_links() {
                out.push(LintFinding {
                    rule: "unbaselined".to_string(),
                    severity,
                    item: Some(l.item.clone()),
                    task: None,
                    doc: ctx.item_meta.get(&l.item).map(|m| m.doc_slug.clone()),
                    message: format!(
                        "{} {} {} has no recorded baseline yet",
                        l.item, l.link_type, l.upstream
                    ),
                });
            }
            for t in graph.unbaselined_tasks() {
                out.push(LintFinding {
                    rule: "unbaselined".to_string(),
                    severity,
                    item: Some(t.item.clone()),
                    task: Some(t.task_id.clone()),
                    doc: ctx.item_meta.get(&t.item).map(|m| m.doc_slug.clone()),
                    message: format!(
                        "task {} has no recorded baseline for {} yet",
                        t.task_id, t.item
                    ),
                });
            }
        }
    }

    // unwanted_coverage (M3, wiki/270-vmodel-m3-design.md §2.1/§3.1,
    // FR-202): a verifier whose own layer is not in its target's effective
    // `needs` set — precomputed once during `TraceGraph::build` (same
    // "derive once, let lint.rs just read it" pattern as suspects/
    // unbaselined above).
    if wants("unwanted_coverage") {
        if let Some(severity) = resolve_severity(Severity::Info, "unwanted_coverage", &config.rules)
        {
            for (item_id, verifier_id) in graph.unwanted_coverage() {
                out.push(LintFinding {
                    rule: "unwanted_coverage".to_string(),
                    severity,
                    item: Some(item_id.clone()),
                    task: None,
                    doc: ctx.item_meta.get(item_id).map(|m| m.doc_slug.clone()),
                    message: format!(
                        "{verifier_id} verifies {item_id} from a layer not in its needs"
                    ),
                });
            }
        }
    }

    // --- Tailoring ---
    for item in &trace_input.items {
        let Some(layer) = item.layer.as_deref() else {
            continue;
        };
        let doc = ctx
            .item_meta
            .get(&item.stable_id)
            .map(|m| m.doc_slug.clone());

        // waiver_on_na / redundant_waiver (§3.1's priority order: na -> ...
        // -> waived -> uncovered — a waiver only ever "engages" when the
        // axis would otherwise be uncovered).
        for axis in &item.waived_axes {
            let (status, rule_waiver, rule_redundant) = match axis {
                super::types::WaiverAxis::Verify => (
                    graph.item_horizontal(&item.stable_id),
                    "waiver_on_na",
                    "redundant_waiver",
                ),
                super::types::WaiverAxis::Refine => (
                    graph.item_vertical(&item.stable_id),
                    "waiver_on_na",
                    "redundant_waiver",
                ),
            };
            match status {
                Some(super::types::CoverageStatus::Na) if wants(rule_waiver) => {
                    if let Some(severity) =
                        resolve_severity(Severity::Warning, rule_waiver, &config.rules)
                    {
                        out.push(LintFinding {
                            rule: rule_waiver.to_string(),
                            severity,
                            item: Some(item.stable_id.clone()),
                            task: None,
                            doc: doc.clone(),
                            message: format!(
                                "{} waives an axis whose layer is not in use (n/a) — the waiver \
                                 has no effect",
                                item.stable_id
                            ),
                        });
                    }
                }
                Some(
                    super::types::CoverageStatus::Covered | super::types::CoverageStatus::Partial,
                ) if wants(rule_redundant) => {
                    if let Some(severity) =
                        resolve_severity(Severity::Info, rule_redundant, &config.rules)
                    {
                        out.push(LintFinding {
                            rule: rule_redundant.to_string(),
                            severity,
                            item: Some(item.stable_id.clone()),
                            task: None,
                            doc: doc.clone(),
                            message: format!(
                                "{} is already covered; its waiver is unused",
                                item.stable_id
                            ),
                        });
                    }
                }
                _ => {}
            }
        }

        // unknown_acceptance_ref: a verifies/refines reference `X#ACn` whose
        // target `X` exists but does not declare acceptance criterion `ACn`.
        if wants("unknown_acceptance_ref") {
            for r in item.verifies.iter().chain(item.refines.iter()) {
                let Some((base, label)) = r.split_once('#') else {
                    continue;
                };
                let Some(target) = trace_input.items.iter().find(|i| i.stable_id == base) else {
                    continue; // dangling — already reported by its own rule.
                };
                if !target.acceptance_labels.iter().any(|l| l == label) {
                    if let Some(severity) =
                        resolve_severity(Severity::Warning, "unknown_acceptance_ref", &config.rules)
                    {
                        out.push(LintFinding {
                            rule: "unknown_acceptance_ref".to_string(),
                            severity,
                            item: Some(item.stable_id.clone()),
                            task: None,
                            doc: doc.clone(),
                            message: format!("{r} has no acceptance criterion {label} on {base}"),
                        });
                    }
                }
            }
        }

        // layer_outside_profile: the item's own layer is not within the
        // effective in-use layer set reached by its resolved profile tree
        // (§2.1 規則 1-4, M2-03's `Dp::in_scope`).
        if wants("layer_outside_profile") && !graph.in_scope(&item.stable_id) {
            if let Some(severity) =
                resolve_severity(Severity::Info, "layer_outside_profile", &config.rules)
            {
                out.push(LintFinding {
                    rule: "layer_outside_profile".to_string(),
                    severity,
                    item: Some(item.stable_id.clone()),
                    task: None,
                    doc: doc.clone(),
                    message: format!(
                        "{} is in layer {layer}, which is not in its resolved profile's \
                         effective layer set",
                        item.stable_id
                    ),
                });
            }
        }

        // missing_acceptance (M3, wiki/270 §4.6, FR-504): a `requirement`
        // layer item with no parsed acceptance-criteria block at all. Scoped
        // to the `requirement` layer only — a right-side/verification item
        // is not expected to declare its own acceptance criteria.
        if layer == "requirement"
            && item.acceptance_labels.is_empty()
            && wants("missing_acceptance")
        {
            if let Some(severity) =
                resolve_severity(Severity::Info, "missing_acceptance", &config.rules)
            {
                out.push(LintFinding {
                    rule: "missing_acceptance".to_string(),
                    severity,
                    item: Some(item.stable_id.clone()),
                    task: None,
                    doc: doc.clone(),
                    message: format!(
                        "{} has no acceptance-criteria block (受入基準)",
                        item.stable_id
                    ),
                });
            }
        }

        // ambiguous_word / passive_voice_hint (M3, wiki/270 §4.6, FR-504):
        // plain substring checks against the item's title
        // (`ItemLintMeta::title`, `SubItem.description`) — see
        // `src/trace/quality.rs` for the word lists and detection rules.
        let title = ctx.item_meta.get(&item.stable_id).map(|m| m.title.as_str());
        if let Some(title) = title {
            if wants("ambiguous_word") {
                if let Some(word) = super::quality::find_ambiguous_word(title) {
                    if let Some(severity) =
                        resolve_severity(Severity::Info, "ambiguous_word", &config.rules)
                    {
                        out.push(LintFinding {
                            rule: "ambiguous_word".to_string(),
                            severity,
                            item: Some(item.stable_id.clone()),
                            task: None,
                            doc: doc.clone(),
                            message: format!(
                                "{} uses the ambiguous term {word:?} in its title",
                                item.stable_id
                            ),
                        });
                    }
                }
            }
            if wants("passive_voice_hint") && super::quality::has_passive_voice_hint(title) {
                if let Some(severity) =
                    resolve_severity(Severity::Info, "passive_voice_hint", &config.rules)
                {
                    out.push(LintFinding {
                        rule: "passive_voice_hint".to_string(),
                        severity,
                        item: Some(item.stable_id.clone()),
                        task: None,
                        doc: doc.clone(),
                        message: format!(
                            "{} uses a passive-voice construction in its title",
                            item.stable_id
                        ),
                    });
                }
            }
        }
    }

    // unlabeled_acceptance / invalid_waiver / id_like_heading /
    // attribute_after_body: pattern-match this call's own in-memory resync
    // warnings (rendered `ParseWarning` text,
    // `src/storage/docs/layer_parse.rs`'s `Display` impl).
    for (doc_slug, text) in ctx.per_doc_sync_warnings {
        if text.contains("acceptance bullet has no label, assigned")
            && wants("unlabeled_acceptance")
        {
            if let Some(severity) =
                resolve_severity(Severity::Warning, "unlabeled_acceptance", &config.rules)
            {
                out.push(LintFinding {
                    rule: "unlabeled_acceptance".to_string(),
                    severity,
                    item: None,
                    task: None,
                    doc: Some(doc_slug.clone()),
                    message: text.clone(),
                });
            }
        } else if text.contains("has an empty reason, ignored")
            && (text.contains("waive-verify") || text.contains("waive-refine"))
            && wants("invalid_waiver")
        {
            if let Some(severity) =
                resolve_severity(Severity::Warning, "invalid_waiver", &config.rules)
            {
                out.push(LintFinding {
                    rule: "invalid_waiver".to_string(),
                    severity,
                    item: None,
                    task: None,
                    doc: Some(doc_slug.clone()),
                    message: text.clone(),
                });
            }
        } else if text.contains("ID-like heading ignored") && wants("id_like_heading") {
            if let Some(severity) =
                resolve_severity(Severity::Info, "id_like_heading", &config.rules)
            {
                out.push(LintFinding {
                    rule: "id_like_heading".to_string(),
                    severity,
                    item: None,
                    task: None,
                    doc: Some(doc_slug.clone()),
                    message: text.clone(),
                });
            }
        } else if text.contains("appears after body text and is ignored")
            && wants("attribute_after_body")
        {
            if let Some(severity) =
                resolve_severity(Severity::Warning, "attribute_after_body", &config.rules)
            {
                out.push(LintFinding {
                    rule: "attribute_after_body".to_string(),
                    severity,
                    item: None,
                    task: None,
                    doc: Some(doc_slug.clone()),
                    message: text.clone(),
                });
            }
        }
    }

    // --- Drift (FR-801) ---
    if wants("unsynced_body") {
        if let Some(severity) = resolve_severity(Severity::Warning, "unsynced_body", &config.rules)
        {
            let mut slugs: Vec<&String> = ctx.resynced_doc_slugs.iter().collect();
            slugs.sort();
            for slug in slugs {
                out.push(LintFinding {
                    rule: "unsynced_body".to_string(),
                    severity,
                    item: None,
                    task: None,
                    doc: Some(slug.clone()),
                    message: format!(
                        "{slug} was out of sync with its stored verification matrix (direct \
                         edit, or a sync-affecting config change) — resynced in memory for this \
                         call only"
                    ),
                });
            }
        }
    }

    if wants("task_link_dangling") {
        if let Some(severity) =
            resolve_severity(Severity::Warning, "task_link_dangling", &config.rules)
        {
            let known: HashSet<&str> = trace_input
                .items
                .iter()
                .map(|i| i.stable_id.as_str())
                .collect();
            let mut dangling: Vec<(&str, &str)> = trace_input
                .task_requirement_links
                .iter()
                .filter(|l| !known.contains(l.stable_id.as_str()))
                .map(|l| (l.task_id.as_str(), l.stable_id.as_str()))
                .collect();
            dangling.sort();
            dangling.dedup();
            for (task_id, stable_id) in dangling {
                out.push(LintFinding {
                    rule: "task_link_dangling".to_string(),
                    severity,
                    item: Some(stable_id.to_string()),
                    task: Some(task_id.to_string()),
                    doc: None,
                    message: format!("task {task_id} links {stable_id}, which does not exist"),
                });
            }
        }
    }

    if wants("task_ids_drift") {
        if let Some(severity) = resolve_severity(Severity::Info, "task_ids_drift", &config.rules) {
            for d in ctx.task_ids_drift {
                // M2-15 (wiki/260 §4.8): `d` is either an item-level drift
                // (`stable_id` set) or a document-level one (`doc_slug` set)
                // — mutually exclusive by construction
                // ([`TaskIdsDrift::item`]/[`TaskIdsDrift::doc`]).
                let (item, doc, subject) = match (&d.stable_id, &d.doc_slug) {
                    (Some(id), _) => (
                        Some(id.clone()),
                        ctx.item_meta.get(id).map(|m| m.doc_slug.clone()),
                        id.clone(),
                    ),
                    (None, Some(slug)) => (None, Some(slug.clone()), format!("document {slug}")),
                    (None, None) => continue,
                };
                out.push(LintFinding {
                    rule: "task_ids_drift".to_string(),
                    severity,
                    item,
                    task: None,
                    doc,
                    message: format!(
                        "{subject}'s stored task_ids {:?} disagree with the task side's {:?}",
                        d.stored, d.derived
                    ),
                });
            }
        }
    }

    if wants("orphaned_legacy") {
        if let Some(severity) = resolve_severity(Severity::Info, "orphaned_legacy", &config.rules) {
            for doc in ctx.docs {
                let Some(v) = &doc.verification else {
                    continue;
                };
                for vi in &v.items {
                    if vi.label.as_deref() != Some(ORPHAN_LEGACY_LABEL) {
                        continue;
                    }
                    for sub in &vi.sub_items {
                        let Some(id) = sub.stable_id.clone() else {
                            continue;
                        };
                        out.push(LintFinding {
                            rule: "orphaned_legacy".to_string(),
                            severity,
                            item: Some(id.clone()),
                            task: None,
                            doc: Some(doc.slug.clone()),
                            message: format!(
                                "{id} moved to \"{ORPHAN_LEGACY_LABEL}\" — its containing \
                                 section heading no longer exists"
                            ),
                        });
                    }
                }
            }
        }
    }

    if wants("orphan_run") {
        if let Some(severity) = resolve_severity(Severity::Info, "orphan_run", &config.rules) {
            let known: HashSet<&str> = trace_input
                .items
                .iter()
                .map(|i| i.stable_id.as_str())
                .collect();
            let mut ids: Vec<&String> = trace_input
                .runs_latest
                .keys()
                .filter(|id| !known.contains(id.as_str()))
                .collect();
            ids.sort();
            for id in ids {
                out.push(LintFinding {
                    rule: "orphan_run".to_string(),
                    severity,
                    item: Some(id.clone()),
                    task: None,
                    doc: None,
                    message: format!("a recorded run references {id}, which does not exist"),
                });
            }
        }
    }

    if wants("frontmatter_invalid") {
        if let Some(severity) =
            resolve_severity(Severity::Error, "frontmatter_invalid", &config.rules)
        {
            for u in ctx.unreadable {
                out.push(LintFinding {
                    rule: "frontmatter_invalid".to_string(),
                    severity,
                    item: None,
                    task: None,
                    doc: Some(u.slug.clone()),
                    message: u.error.clone(),
                });
            }
        }
    }

    // --- Policy rules ([[trace.lint.require]]) ---
    for rule in &config.require {
        // Defense-in-depth only: every caller reaching `evaluate` is expected
        // to have already rejected an entry like this via
        // `validate_lint_config` (M2-08 rework) — this `continue` exists so a
        // unit test building a `TraceLintConfig` directly (bypassing that
        // gate) still can't panic or produce a finding with an empty rule id.
        if rule.id.is_empty() || rule.need.is_empty() {
            continue;
        }
        if !wants(&rule.id) {
            continue;
        }
        let default = Severity::Error;
        let configured = rule
            .severity
            .as_deref()
            .and_then(Severity::parse)
            .unwrap_or(default);
        let Some(severity) = resolve_severity(configured, &rule.id, &config.rules) else {
            continue;
        };
        for item in &trace_input.items {
            if !require_when_matches(rule, item, ctx) {
                continue;
            }
            if require_need_satisfied(rule, item, graph, trace_input) {
                continue;
            }
            out.push(LintFinding {
                rule: rule.id.clone(),
                severity,
                item: Some(item.stable_id.clone()),
                task: None,
                doc: ctx
                    .item_meta
                    .get(&item.stable_id)
                    .map(|m| m.doc_slug.clone()),
                message: format!(
                    "{} does not satisfy {} ({})",
                    item.stable_id, rule.id, rule.need
                ),
            });
        }
    }

    sort_findings(&mut out);
    out
}

fn require_when_matches(
    rule: &TraceLintRequireRule,
    item: &super::types::TraceItemInput,
    ctx: &LintContext<'_>,
) -> bool {
    if let Some(layer) = &rule.when.layer {
        if item.layer.as_deref() != Some(layer.as_str()) {
            return false;
        }
    }
    let meta = ctx.item_meta.get(&item.stable_id);
    if !rule.when.priority.is_empty() {
        let priority = meta.and_then(|m| m.priority.as_deref());
        if !priority.is_some_and(|p| rule.when.priority.iter().any(|w| w == p)) {
            return false;
        }
    }
    if let Some(method) = &rule.when.method {
        if item.method.as_deref() != Some(method.as_str()) {
            return false;
        }
    }
    if let Some(doc) = &rule.when.doc {
        if meta.map(|m| m.doc_slug.as_str()) != Some(doc.as_str()) {
            return false;
        }
    }
    if let Some(approvals) = &rule.when.approval {
        let approval = meta.map(|m| m.approval.as_str());
        if !approval.is_some_and(|a| approvals.iter().any(|w| w == a)) {
            return false;
        }
    }
    true
}

fn require_need_satisfied(
    rule: &TraceLintRequireRule,
    item: &super::types::TraceItemInput,
    graph: &TraceGraph,
    trace_input: &TraceInput,
) -> bool {
    match rule.need.as_str() {
        "verified_by" => !graph.verified_by(&item.stable_id).is_empty(),
        "refined_by" => !graph.refines_children(&item.stable_id).is_empty(),
        "implemented_by_task" => trace_input
            .task_requirement_links
            .iter()
            .any(|l| l.stable_id == item.stable_id && matches!(l.role, TaskLinkRole::Implements)),
        "passing" => graph.state(&item.stable_id) == Some(ItemState::Passing),
        "no_suspect" => !graph.suspects().iter().any(|s| s.item == item.stable_id),
        "auto_test" => {
            item.method.as_deref() == Some("auto")
                || graph.verified_by(&item.stable_id).iter().any(|v| {
                    trace_input
                        .items
                        .iter()
                        .find(|i| &i.stable_id == v)
                        .is_some_and(|i| i.method.as_deref() == Some("auto"))
                })
        }
        // An unrecognized `need` value can never be satisfied — fail-safe
        // (surfaces as a finding on every matching item rather than silently
        // treating an unknown policy as "nothing to check").
        _ => false,
    }
}

/// wiki/260 §4.3: "出力の順序は（severity → rule → item の自然順）で決定的に
/// する" — severity descending (error first), then rule id, then item, via a
/// natural-order comparator so e.g. `"FR-2"` sorts before `"FR-10"`. The same
/// rule `src/mcp/handlers/docs_query.rs`'s `natural_cmp` applies to
/// stable_ids for `handoff_doc_req_list` is duplicated here (not imported) to
/// keep this pure derivation module decoupled from the MCP handler layer
/// (this module's own doc comment: "no file I/O of its own").
fn sort_findings(findings: &mut [LintFinding]) {
    findings.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| a.rule.cmp(&b.rule))
            .then_with(|| {
                let ai = a.item.as_deref().unwrap_or("");
                let bi = b.item.as_deref().unwrap_or("");
                natural_cmp(ai, bi)
            })
    });
}

#[cfg(test)]
mod tests;

/// See [`sort_findings`]'s doc comment.
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) => {
                if ca.is_ascii_digit() && cb.is_ascii_digit() {
                    let mut na = String::new();
                    while ai.peek().is_some_and(char::is_ascii_digit) {
                        na.push(ai.next().unwrap());
                    }
                    let mut nb = String::new();
                    while bi.peek().is_some_and(char::is_ascii_digit) {
                        nb.push(bi.next().unwrap());
                    }
                    let va: u64 = na.parse().unwrap_or(0);
                    let vb: u64 = nb.parse().unwrap_or(0);
                    match va.cmp(&vb) {
                        Ordering::Equal => continue,
                        other => return other,
                    }
                }
                match ca.cmp(&cb) {
                    Ordering::Equal => {
                        ai.next();
                        bi.next();
                        continue;
                    }
                    other => return other,
                }
            }
        }
    }
}
