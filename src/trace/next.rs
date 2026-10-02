//! Next-action derivation (wiki/260-vmodel-m2-design.md §3.5, M2-10,
//! FR-703): a pure function over an already-built [`TraceGraph`] plus its
//! [`TraceInput`] (wiki/240-performance-design.md §5-5's "1 リクエスト内で
//! グラフを1回だけ構築する" — this module never builds its own graph) that
//! ranks "what to do next" across the whole trace corpus (or one task's own
//! slice, §4.5's `task_id?`) into the 8 kinds §3.5's table defines, each with
//! a concrete suggested MCP tool call.
//!
//! `priority`/`dev_stage` are not present on [`TraceItemInput`] itself (only
//! `layer`/`refines`/`verifies`/hash fields are, by design — see that type's
//! own doc comment on why it stays decoupled from `SubItem`) — callers pass
//! them in via [`ItemNextMeta`], gathered from `docs` the same way
//! `trace_lint`'s `collect_item_lint_meta`/`ItemLintMeta` do (duplicated
//! rather than shared: that struct lives in `src/mcp/handlers/trace_lint.rs`,
//! a handler module this pure `src/trace/` module has no other reason to
//! depend on).

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::storage::docs::layer::{LayerSide, RegisteredLayer};

use super::engine::TraceGraph;
use super::types::{
    CoverageStatus, GapKind, SuspectKind, TaskLinkRole, TraceInput, TraceItemInput,
};

/// The next-action kinds, declared in §3.5's rank order (1 = highest
/// priority) — `Ord`'s derived discriminant order doubles as the primary
/// sort key. [`NextAction::rank`] is the 1-based display rank matching
/// §3.5's table; it used to equal `kind as u8 + 1` directly, but M3
/// (wiki/270-vmodel-m3-design.md §4.5, FR-307) added `ManualPending`
/// *sharing* `Rerun`'s rank (3, "`rerun`と同列") rather than getting a rank
/// of its own, so [`NextActionKind::rank`] is now an explicit match instead
/// of a discriminant arithmetic shortcut — every kind after `ManualPending`
/// still displays the same rank number §3.5's table always gave it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, std::hash::Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NextActionKind {
    FixFailing,
    ReviewSuspect,
    Rerun,
    /// M3 (wiki/270-vmodel-m3-design.md §4.5, FR-307): an assigned manual/
    /// visual/review verification item that has never been run. Shares rank
    /// 3 with `Rerun` (both are "go execute this verification" actions) but
    /// is its own `Ord` position so the two kinds don't interleave by
    /// priority/layer within the same rank — every `Rerun` candidate sorts
    /// before every `ManualPending` one, each internally still ordered by
    /// priority -> layer level -> id.
    ManualPending,
    WriteVerification,
    Refine,
    CreateTask,
    FixLink,
    Baseline,
}

impl NextActionKind {
    /// 1-based display rank matching §3.5's table (`fix_failing` = 1 ...
    /// `baseline` = 8); `manual_pending` (M3) shares `rerun`'s rank (3).
    pub fn rank(self) -> u8 {
        match self {
            Self::FixFailing => 1,
            Self::ReviewSuspect => 2,
            Self::Rerun | Self::ManualPending => 3,
            Self::WriteVerification => 4,
            Self::Refine => 5,
            Self::CreateTask => 6,
            Self::FixLink => 7,
            Self::Baseline => 8,
        }
    }
}

/// `item`/`task` priority (`"P0"`..`"P3"`, free-extensible) — missing or an
/// unrecognized value sorts after every known one (§3.5: "priority（P0 →
/// P3）"), same "unknown sorts last, never panics" policy every other
/// priority-aware sort in this crate follows.
fn priority_rank(p: Option<&str>) -> u8 {
    match p {
        Some("P0") => 0,
        Some("P1") => 1,
        Some("P2") => 2,
        Some("P3") => 3,
        _ => 4,
    }
}

/// `layer`'s level within its side, 1-based top-first (§3.5: "層の level
/// （上位が先）") — a layer absent from the registry (unregistered custom id,
/// or no layer at all) sorts after every registered one, mirroring
/// [`priority_rank`]'s "unknown sorts last" policy.
fn layer_level_rank(registry: &[RegisteredLayer], layer: Option<&str>) -> u8 {
    layer
        .and_then(|l| registry.iter().find(|r| r.id == l))
        .map(|r| r.level)
        .unwrap_or(u8::MAX)
}

/// Per-item data [`TraceItemInput`] doesn't itself carry (wiki/260 §3.5) —
/// gathered by the caller from `docs` (mirrors
/// `src/mcp/handlers/trace_lint.rs`'s `ItemLintMeta`/`collect_item_lint_meta`,
/// duplicated rather than imported: see this module's own doc comment).
#[derive(Debug, Clone, Default)]
pub struct ItemNextMeta {
    pub priority: Option<String>,
    pub dev_stage: Option<String>,
    /// M3 (wiki/270-vmodel-m3-design.md §4.5, FR-307): `SubItem.assignee`,
    /// gathered the same way `priority`/`dev_stage` are — used by both the
    /// `assignee` filter and [`manual_pending_candidates`].
    pub assignee: Option<String>,
}

/// A suggested follow-up MCP tool call (§3.5's table's rightmost column,
/// §4.5's `suggest: {tool, arguments}`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Suggest {
    pub tool: String,
    pub arguments: serde_json::Value,
}

/// One ranked next action (§4.5's `actions[]` entry).
#[derive(Debug, Clone, Serialize)]
pub struct NextAction {
    pub rank: u8,
    pub kind: NextActionKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// Sort input only, not itself meaningful beyond "known value sorts
    /// before unknown" — still surfaced because a caller filtering/reading
    /// the JSON benefits from seeing what drove the ordering.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    pub reason: String,
    pub suggest: Suggest,
}

/// Internal sort key (§3.5: "同じ順位の中では、項目の priority（P0 → P3）→
/// 層の level（上位が先）→ ID の自然順で並べる（決定的）") — `rank` is
/// [`NextActionKind`]'s own `Ord`, so this tuple's lexicographic `Ord` gives
/// exactly the spec's 4-level sort with no custom `Ord` impl needed on
/// [`NextAction`] itself (which holds a `serde_json::Value` that isn't
/// orderable).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SortKey {
    kind: NextActionKind,
    priority_rank: u8,
    layer_level_rank: u8,
    id: String,
}

/// One candidate before it's turned into a [`NextAction`] — keeps the sort
/// key and the action construction separate so [`derive_next_actions`]'s
/// single sort-then-truncate pass stays simple.
struct Candidate {
    key: SortKey,
    action: NextAction,
}

/// Everything every per-kind candidate-scan function needs, bundled to keep
/// their own signatures to a single borrow instead of 5-6 positional
/// parameters each (`clippy::too_many_arguments`).
struct Ctx<'a> {
    graph: &'a TraceGraph,
    input: &'a TraceInput,
    meta: &'a HashMap<String, ItemNextMeta>,
    registry: &'a [RegisteredLayer],
    /// stable_id -> its own raw layer id (straight from [`TraceItemInput`],
    /// unresolved — same rationale as `src/trace/matrix.rs`'s
    /// `collect_item_layers`).
    item_layers: &'a HashMap<String, Option<String>>,
}

impl Ctx<'_> {
    fn meta_for(&self, id: &str) -> ItemNextMeta {
        self.meta.get(id).cloned().unwrap_or_default()
    }

    fn sort_key(&self, kind: NextActionKind, id: &str, m: &ItemNextMeta) -> SortKey {
        let layer = self.item_layers.get(id).cloned().flatten();
        SortKey {
            kind,
            priority_rank: priority_rank(m.priority.as_deref()),
            layer_level_rank: layer_level_rank(self.registry, layer.as_deref()),
            id: id.to_string(),
        }
    }
}

fn suggest(tool: &str, arguments: serde_json::Value) -> Suggest {
    Suggest {
        tool: tool.to_string(),
        arguments,
    }
}

fn candidate(
    ctx: &Ctx,
    kind: NextActionKind,
    id: &str,
    reason: String,
    suggest: Suggest,
) -> Candidate {
    let m = ctx.meta_for(id);
    Candidate {
        key: ctx.sort_key(kind, id, &m),
        action: NextAction {
            rank: kind.rank(),
            kind,
            item: Some(id.to_string()),
            task: None,
            priority: m.priority,
            reason,
            suggest,
        },
    }
}

/// §3.5's `kind: fix_failing` — a verification item (right-side, or an
/// inline left-side item verifying itself) whose aggregated
/// [`TraceGraph::state`] is `Failing` or `Blocked`.
fn fix_failing_candidates(ctx: &Ctx, ids: &[String]) -> Vec<Candidate> {
    use super::types::ItemState;
    let mut out = Vec::new();
    for id in ids {
        let state = ctx.graph.state(id);
        if !matches!(state, Some(ItemState::Failing) | Some(ItemState::Blocked)) {
            continue;
        }
        let reason = format!(
            "{id} is {} — inspect and fix, then re-record",
            if state == Some(ItemState::Failing) {
                "failing"
            } else {
                "blocked"
            }
        );
        out.push(candidate(
            ctx,
            NextActionKind::FixFailing,
            id,
            reason,
            suggest("handoff_trace_slice", serde_json::json!({"item": id})),
        ));
    }
    out
}

/// §3.5's `kind: review_suspect` — an item carrying at least one `link`-kind
/// suspect (an upstream reference whose baseline no longer matches the
/// upstream's current hash). `task`/`result` suspects are each kind 1/3's
/// own concern instead (a `task` suspect alone, with nothing else wrong, has
/// no dedicated kind here — §3.5's table does not list one, and `trace_lint`'s
/// `suspect_task` rule already surfaces it independently).
fn review_suspect_candidates(ctx: &Ctx, ids: &HashSet<String>) -> Vec<Candidate> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out = Vec::new();
    for s in ctx.graph.suspects() {
        if s.kind != SuspectKind::Link {
            continue;
        }
        if !ids.contains(s.item.as_str()) || !seen.insert(s.item.as_str()) {
            continue;
        }
        let id = &s.item;
        out.push(candidate(
            ctx,
            NextActionKind::ReviewSuspect,
            id,
            format!("{id} has a suspect upstream link — confirm impact, then clear"),
            suggest("handoff_trace_impact", serde_json::json!({"item": id})),
        ));
    }
    out
}

/// Whether `item` is a verification item for [`rerun_candidates`]'s purposes:
/// a right-side item, or a left-side item verifying itself inline
/// (`- method:`/`- test:` attribute present, wiki/220 §2.7).
fn is_verifier(item: &TraceItemInput, registry: &[RegisteredLayer]) -> bool {
    let side = item
        .layer
        .as_deref()
        .and_then(|l| registry.iter().find(|r| r.id == l))
        .map(|r| r.side);
    match side {
        Some(LayerSide::Right) => true,
        Some(LayerSide::Left) | None => item.method.is_some() || item.has_test_refs,
    }
}

/// §3.5's `kind: rerun` — a verification item (right-side, or inline
/// left-side) that is `reverify` or `not_run`, whose linked target is
/// "implemented 以上" (`dev_stage` at or beyond `implemented` — `tested`/
/// `verified` also qualify; a verifier of a `not_started`/`in_progress`
/// target has nothing to run yet). For a right-side item, the "target" is
/// whatever it `verifies`; an item with no `verifies` entry resolved to a
/// known left-side item (dangling, or no `dev_stage` meta at all) is
/// skipped, same "don't guess" policy as every other unresolvable reference
/// in this crate.
fn rerun_candidates(ctx: &Ctx, ids: &HashSet<String>) -> Vec<Candidate> {
    use super::types::ItemState;
    let is_implemented_or_beyond = |stage: Option<&str>| {
        matches!(
            stage.unwrap_or("not_started"),
            "implemented" | "tested" | "verified"
        )
    };
    let mut out = Vec::new();
    for item in &ctx.input.items {
        let id = &item.stable_id;
        if !ids.contains(id.as_str()) || !is_verifier(item, ctx.registry) {
            continue;
        }
        let needs_rerun = ctx.graph.reverify_items().contains(id.as_str())
            || ctx.graph.state(id) == Some(ItemState::NotRun);
        if !needs_rerun {
            continue;
        }
        // Ready to run iff the verified target(s) are implemented or beyond
        // (§3.5: "対象が implemented 以上") — an inline left-side item (no
        // `verifies` entries) verifies its own `dev_stage` instead of a
        // separate target's.
        let ready = if item.verifies.is_empty() {
            is_implemented_or_beyond(ctx.meta.get(id).and_then(|m| m.dev_stage.as_deref()))
        } else {
            item.verifies.iter().any(|v| {
                let base = v.split('#').next().unwrap_or(v.as_str());
                ctx.meta
                    .get(base)
                    .is_some_and(|m| is_implemented_or_beyond(m.dev_stage.as_deref()))
            })
        };
        if !ready {
            continue;
        }
        out.push(candidate(
            ctx,
            NextActionKind::Rerun,
            id,
            format!("{id} needs a fresh run (reverify or never run) — execute, then record"),
            suggest(
                "handoff_trace_ingest",
                serde_json::json!({"format": "junit_xml"}),
            ),
        ));
    }
    out
}

/// M3 (wiki/270-vmodel-m3-design.md §4.5, FR-307) `kind: manual_pending` — a
/// verification item that has an `assignee` set, whose `method` is `manual`,
/// `visual`, or `review`, and whose latest run result is `not_run`. Shares
/// rank 3 with [`rerun_candidates`] (both are "go execute this" actions) —
/// unlike `rerun`, this kind does **not** require the verified target to be
/// `implemented` or beyond: an assigned manual/visual/review item with
/// nothing recorded yet is actionable for its assignee regardless of the
/// target's own dev_stage (§4.5 states only the three conditions above).
fn manual_pending_candidates(ctx: &Ctx, ids: &HashSet<String>) -> Vec<Candidate> {
    use super::types::ItemState;
    const QUALIFYING_METHODS: [&str; 3] = ["manual", "visual", "review"];
    let mut out = Vec::new();
    for item in &ctx.input.items {
        let id = &item.stable_id;
        if !ids.contains(id.as_str()) {
            continue;
        }
        let Some(method) = item.method.as_deref() else {
            continue;
        };
        if !QUALIFYING_METHODS.contains(&method) {
            continue;
        }
        let m = ctx.meta_for(id);
        if m.assignee.is_none() {
            continue;
        }
        if ctx.graph.state(id) != Some(ItemState::NotRun) {
            continue;
        }
        let assignee = m.assignee.clone().unwrap_or_default();
        out.push(candidate(
            ctx,
            NextActionKind::ManualPending,
            id,
            format!("{id} is assigned to {assignee} and awaiting its first {method} run"),
            suggest("handoff_trace_record", serde_json::json!({"item": id})),
        ));
    }
    out
}

/// §3.5's `kind: write_verification`/`kind: refine` — a left-side item whose
/// horizontal/vertical classification is `Uncovered` *or* `Partial` (§3.5:
/// "unverified（partial を含む）"/no such parenthetical for `refine`, but
/// §3.1's "state への folding" note treats `partial` as an uncovered-like
/// element for both axes uniformly, so this module does too for consistency
/// — `graph.item_horizontal`/`item_vertical` already fold waived/na/covered
/// out).
fn coverage_gap_candidates(
    ctx: &Ctx,
    ids: &HashSet<String>,
    kind: NextActionKind,
    axis: fn(&TraceGraph, &str) -> Option<CoverageStatus>,
    tool: &str,
    reason_suffix: &str,
) -> Vec<Candidate> {
    let mut out = Vec::new();
    let mut sorted_ids: Vec<&String> = ids.iter().collect();
    sorted_ids.sort();
    for id in sorted_ids {
        let status = axis(ctx.graph, id);
        if !matches!(
            status,
            Some(CoverageStatus::Uncovered) | Some(CoverageStatus::Partial)
        ) {
            continue;
        }
        out.push(candidate(
            ctx,
            kind,
            id,
            format!("{id} {reason_suffix}"),
            suggest(tool, serde_json::json!({"items": [id]})),
        ));
    }
    out
}

/// §3.5's `kind: create_task` — a left-side item whose own `dev_stage` is
/// `not_started` (absent defaults to `not_started`, same convention
/// `src/mcp/handlers/docs.rs`'s own dev_stage readers use) and has no task
/// holding an `implements` link to it.
fn create_task_candidates(ctx: &Ctx, ids: &HashSet<String>) -> Vec<Candidate> {
    let implements: HashSet<&str> = ctx
        .input
        .task_requirement_links
        .iter()
        .filter(|l| l.role == TaskLinkRole::Implements)
        .map(|l| l.stable_id.as_str())
        .collect();
    let mut out = Vec::new();
    for item in &ctx.input.items {
        let id = &item.stable_id;
        if !ids.contains(id.as_str()) || implements.contains(id.as_str()) {
            continue;
        }
        let side = item
            .layer
            .as_deref()
            .and_then(|l| ctx.registry.iter().find(|r| r.id == l))
            .map(|r| r.side);
        if side != Some(LayerSide::Left) {
            continue;
        }
        let m = ctx.meta_for(id);
        if m.dev_stage.as_deref().unwrap_or("not_started") != "not_started" {
            continue;
        }
        out.push(candidate(
            ctx,
            NextActionKind::CreateTask,
            id,
            format!("{id} is not_started with no implementing task yet"),
            suggest(
                "handoff_trace_tasks",
                serde_json::json!({"items": [id], "mode": "preview"}),
            ),
        ));
    }
    out
}

/// §3.5's `kind: fix_link` — the structural gap kinds (`dangling`/
/// `invalid_link`/`cycle`/`duplicate_id`/`orphan`), one candidate per gap
/// carrying an `item` (`task_unlinked`'s `item` is a task id, not a trace
/// item, so it is deliberately excluded here — it has no dedicated §3.5 kind
/// at all).
fn fix_link_candidates(ctx: &Ctx, ids: &HashSet<String>) -> Vec<Candidate> {
    const STRUCTURAL: [GapKind; 5] = [
        GapKind::Dangling,
        GapKind::InvalidLink,
        GapKind::Cycle,
        GapKind::DuplicateId,
        GapKind::Orphan,
    ];
    let mut out = Vec::new();
    for gap in ctx.graph.gaps() {
        if !STRUCTURAL.contains(&gap.kind) {
            continue;
        }
        let Some(id) = &gap.item else { continue };
        if !ids.contains(id.as_str()) {
            continue;
        }
        out.push(candidate(
            ctx,
            NextActionKind::FixLink,
            id,
            format!("{id}: {}", gap.detail),
            suggest("handoff_trace_slice", serde_json::json!({"item": id})),
        ));
    }
    out
}

/// §3.5's `kind: baseline` — every unbaselined `refines`/`verifies`
/// reference (§7: never silently backfilled, only `trace_suspect
/// baseline(apply=true)` resolves it).
fn baseline_candidates(ctx: &Ctx, ids: &HashSet<String>) -> Vec<Candidate> {
    let mut out = Vec::new();
    for link in ctx.graph.unbaselined_links() {
        if !ids.contains(link.item.as_str()) {
            continue;
        }
        let id = &link.item;
        out.push(candidate(
            ctx,
            NextActionKind::Baseline,
            id,
            format!("{id} -> {} has no recorded baseline hash", link.upstream),
            suggest(
                "handoff_trace_suspect",
                serde_json::json!({"action": "baseline", "apply": true}),
            ),
        ));
    }
    out
}

/// §3.5/§4.5: derives every next action across the candidate item set (the
/// whole corpus, or one task's own `requirement`-linked ids when
/// `scope_ids` narrows the call, §4.5's `task_id?`), optionally restricted to
/// `layers_filter`/`kinds_filter`, then sorts deterministically (kind rank ->
/// priority -> layer level -> stable_id natural order, §3.5's closing
/// sentence) and truncates to `limit`.
///
/// `layers_filter`, when non-empty, restricts the candidate set to items
/// whose own effective layer is one of these (§4.5's `layers?[]`) — applied
/// *before* candidate generation, not as a post-filter, so e.g. a
/// `create_task` candidate for an item outside the requested layers never
/// displaces one inside it before `limit` truncation.
///
/// `assignee_filter` (M3, wiki/270-vmodel-m3-design.md §4.5, FR-307), when
/// `Some`, restricts the candidate set to items whose `ItemNextMeta.assignee`
/// matches exactly — applied the same "before candidate generation" way
/// `layers_filter` is, for the same displacement-avoidance reason.
#[allow(clippy::too_many_arguments)] // established codebase convention (see other call sites of this attribute, e.g. src/mcp/handlers/trace_update.rs); these are independent filter/scope values the caller (handoff_trace_next) passes straight through from its own flat JSON arguments — not something a struct would meaningfully group without adding indirection for its own sake.
pub fn derive_next_actions(
    graph: &TraceGraph,
    input: &TraceInput,
    meta: &HashMap<String, ItemNextMeta>,
    scope_ids: Option<&HashSet<String>>,
    layers_filter: &[String],
    assignee_filter: Option<&str>,
    kinds_filter: Option<&HashSet<NextActionKind>>,
    limit: usize,
) -> (Vec<NextAction>, bool) {
    let registry = input.layer_registry.as_slice();
    let item_layers: HashMap<String, Option<String>> = input
        .items
        .iter()
        .map(|i| (i.stable_id.clone(), i.layer.clone()))
        .collect();

    let layers_filter_set: Option<HashSet<&str>> = if layers_filter.is_empty() {
        None
    } else {
        Some(layers_filter.iter().map(String::as_str).collect())
    };

    let all_ids: HashSet<String> = input
        .items
        .iter()
        .map(|i| i.stable_id.clone())
        .filter(|id| scope_ids.is_none_or(|s| s.contains(id)))
        .filter(|id| {
            layers_filter_set.as_ref().is_none_or(|set| {
                item_layers
                    .get(id)
                    .and_then(|l| l.as_deref())
                    .is_some_and(|l| set.contains(l))
            })
        })
        .filter(|id| {
            assignee_filter.is_none_or(|wanted| {
                meta.get(id)
                    .and_then(|m| m.assignee.as_deref())
                    .is_some_and(|a| a == wanted)
            })
        })
        .collect();
    let ordered_ids: Vec<String> = {
        let mut v: Vec<String> = all_ids.iter().cloned().collect();
        v.sort();
        v
    };

    let ctx = Ctx {
        graph,
        input,
        meta,
        registry,
        item_layers: &item_layers,
    };

    let mut candidates: Vec<Candidate> = Vec::new();
    candidates.extend(fix_failing_candidates(&ctx, &ordered_ids));
    candidates.extend(review_suspect_candidates(&ctx, &all_ids));
    candidates.extend(rerun_candidates(&ctx, &all_ids));
    candidates.extend(manual_pending_candidates(&ctx, &all_ids));
    candidates.extend(coverage_gap_candidates(
        &ctx,
        &all_ids,
        NextActionKind::WriteVerification,
        TraceGraph::item_horizontal,
        "handoff_trace_scaffold",
        "has unverified acceptance criteria",
    ));
    candidates.extend(coverage_gap_candidates(
        &ctx,
        &all_ids,
        NextActionKind::Refine,
        TraceGraph::item_vertical,
        "handoff_trace_update",
        "has no refining child",
    ));
    candidates.extend(create_task_candidates(&ctx, &all_ids));
    candidates.extend(fix_link_candidates(&ctx, &all_ids));
    candidates.extend(baseline_candidates(&ctx, &all_ids));

    if let Some(kinds) = kinds_filter {
        candidates.retain(|c| kinds.contains(&c.key.kind));
    }

    candidates.sort_by(cmp_candidates);

    let mut actions: Vec<NextAction> = candidates.into_iter().map(|c| c.action).collect();
    let truncated = actions.len() > limit;
    actions.truncate(limit);
    (actions, truncated)
}

fn cmp_candidates(a: &Candidate, b: &Candidate) -> Ordering {
    a.key.cmp(&b.key)
}

#[cfg(test)]
mod tests;
