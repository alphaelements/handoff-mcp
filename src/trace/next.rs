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
    CoverageStatus, GapKind, LayersSource, SuspectKind, TaskLinkRole, TraceInput, TraceItemInput,
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
    /// M3 (wiki/270-vmodel-m3-design.md §4.7, FR-204) `relink_candidate`: a
    /// `unit_test`/task item that still links straight to a `basic_spec`
    /// item even though a `detailed_spec` item now refines that same target
    /// — a candidate for being relinked one level deeper. Not in §3.5's
    /// original 8-kind table (an M3 addition, like `manual_pending`); ranked
    /// after every other kind since it is a structural *suggestion* for a
    /// link that already works, not a correctness problem.
    RelinkCandidate,
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
            Self::RelinkCandidate => 9,
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
    /// stable_id -> its input record (first occurrence wins, same as every
    /// other first-wins lookup over `input.items` in this module).
    items_by_id: HashMap<&'a str, &'a TraceItemInput>,
}

/// Where a suggested write should land in a given layer: an existing
/// document of that layer (its `id`), or — when the project has no document
/// in that layer yet — a new one to create first, under a free `slug`.
enum LayerDoc<'a> {
    Existing(&'a str),
    Missing {
        layer: &'a RegisteredLayer,
        slug: String,
    },
}

/// Longest `doc_save` slug (`validate_slug`'s limit).
const MAX_SLUG_LEN: usize = 60;

impl<'a> Ctx<'a> {
    fn item(&self, id: &str) -> Option<&'a TraceItemInput> {
        self.items_by_id.get(id).copied()
    }

    fn layer_of(&self, id: &str) -> Option<&'a RegisteredLayer> {
        let layer = self.item(id)?.layer.as_deref()?;
        self.registry.iter().find(|r| r.id == layer)
    }

    /// The document new `layer` items should be written into: the document
    /// of the lowest-sorting stable_id already in that layer (deterministic);
    /// else the lowest-sorting still-empty document declared with that
    /// layer; else [`LayerDoc::Missing`] with a slug no document uses yet.
    fn doc_for_layer(&self, layer: &'a RegisteredLayer) -> LayerDoc<'a> {
        let with_items = self
            .input
            .items
            .iter()
            .filter(|i| i.layer.as_deref() == Some(layer.id.as_str()) && !i.doc_id.is_empty())
            .min_by(|a, b| a.stable_id.cmp(&b.stable_id))
            .map(|i| i.doc_id.as_str());
        let empty_layer_doc = || {
            self.input
                .docs
                .iter()
                .filter(|d| d.layer.as_deref() == Some(layer.id.as_str()))
                .min_by(|a, b| a.id.cmp(&b.id))
                .map(|d| d.id.as_str())
        };
        match with_items.or_else(empty_layer_doc) {
            Some(doc) => LayerDoc::Existing(doc),
            None => LayerDoc::Missing {
                layer,
                slug: self.free_slug(&layer.id),
            },
        }
    }

    /// A `[a-z0-9-]` slug derived from `layer_id` that no document uses
    /// (`handoff_doc_save` refuses a slug already in use): the bare form,
    /// else `-2`, `-3`, ...
    fn free_slug(&self, layer_id: &str) -> String {
        let base: String = layer_id
            .to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .take(MAX_SLUG_LEN - 6)
            .collect();
        let taken = |slug: &str| self.input.docs.iter().any(|d| d.slug == slug);
        if !taken(&base) {
            return base;
        }
        (2u32..)
            .map(|n| format!("{base}-{n}"))
            .find(|slug| !taken(slug))
            .expect("an unbounded counter always reaches a free slug")
    }
}

/// `handoff_doc_save` arguments creating the first (empty) document of
/// `layer` under `slug` — what a suggestion that needs a layer document falls
/// back to when none exists yet.
fn create_layer_doc_suggest(layer: &RegisteredLayer, slug: String) -> Suggest {
    suggest(
        "handoff_doc_save",
        serde_json::json!({
            "slug": slug,
            "title": layer.display_name,
            "layer": layer.id,
            "body": format!("# {}\n", layer.display_name),
        }),
    )
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
        // `trace_ingest` matches test output to items; an item run by hand —
        // an implicit acceptance item, or `method: manual`/`visual`/`review`
        // — has no test output to match, and its result is recorded directly.
        let by_hand = item.implicit_of.is_some()
            || matches!(item.method.as_deref(), Some("manual" | "visual" | "review"));
        let follow_up = if !by_hand {
            suggest(
                "handoff_trace_ingest",
                serde_json::json!({"format": "junit_xml"}),
            )
        } else {
            // `not_run` is a placeholder the executor must replace with the
            // real outcome. It is NOT a no-op: recorded as-is it appends a
            // `not_run` run, which supersedes a stale pass (clearing its
            // result suspect / reverify) without any re-verification.
            suggest(
                "handoff_trace_record",
                serde_json::json!({"results": [{"item": id, "result": "not_run"}]}),
            )
        };
        out.push(candidate(
            ctx,
            NextActionKind::Rerun,
            id,
            format!("{id} needs a fresh run (reverify or never run) — execute, then record"),
            follow_up,
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
            // `not_run` is a placeholder the executor must replace with the
            // real outcome after performing the verification; recorded
            // as-is it would write a spurious `not_run` run.
            suggest(
                "handoff_trace_record",
                serde_json::json!({"results": [{"item": id, "result": "not_run"}]}),
            ),
        ));
    }
    out
}

/// The left-side items whose `axis` classification is `Uncovered` *or*
/// `Partial`, in stable_id order — the shared candidate set of
/// `write_verification` (horizontal) and `refine` (vertical). §3.5:
/// "unverified（partial を含む）"/no such parenthetical for `refine`, but
/// §3.1's "state への folding" note treats `partial` as an uncovered-like
/// element for both axes uniformly, so this module does too for consistency
/// — `graph.item_horizontal`/`item_vertical` already fold waived/na/covered
/// out. A composite without integrated acceptance criteria is excluded
/// (SPEC-TREE-014): it takes its coverage from its leaves, which carry their
/// own candidates — suggesting work on the aggregate would double-count.
fn coverage_gap_ids<'a>(
    ctx: &Ctx,
    ids: &'a HashSet<String>,
    axis: fn(&TraceGraph, &str) -> Option<CoverageStatus>,
) -> Vec<&'a String> {
    let mut sorted_ids: Vec<&String> = ids.iter().collect();
    sorted_ids.sort();
    sorted_ids
        .into_iter()
        .filter(|id| !ctx.graph.is_unintegrated_composite(id))
        .filter(|id| {
            matches!(
                axis(ctx.graph, id),
                Some(CoverageStatus::Uncovered) | Some(CoverageStatus::Partial)
            )
        })
        .collect()
}

/// §3.5's `kind: write_verification` — a left-side item with acceptance
/// criteria whose horizontal coverage is `Uncovered` or `Partial`. The
/// suggestion scaffolds one verification item per criterion into the
/// document of the item's pair layer (`handoff_trace_scaffold` requires
/// `target_doc`); when that layer has no document yet, it creates one.
///
/// An item **without** acceptance criteria is not a candidate: scaffolding
/// generates one verification item per criterion, so there is nothing to
/// generate (the tool would only answer "no acceptance criteria"). Such an
/// item's missing verifier stays visible as `trace_lint`'s `unverified` gap.
fn write_verification_candidates(ctx: &Ctx, ids: &HashSet<String>) -> Vec<Candidate> {
    let mut out = Vec::new();
    for id in coverage_gap_ids(ctx, ids, TraceGraph::item_horizontal) {
        let Some(item) = ctx.item(id) else { continue };
        if item.acceptance_labels.is_empty() {
            continue;
        }
        let Some(pair) = ctx
            .layer_of(id)
            .and_then(|l| ctx.registry.iter().find(|r| r.id == l.pair))
        else {
            continue;
        };
        let suggestion = match ctx.doc_for_layer(pair) {
            LayerDoc::Existing(doc) => suggest(
                "handoff_trace_scaffold",
                serde_json::json!({"items": [id], "target_doc": doc}),
            ),
            LayerDoc::Missing { layer, slug } => create_layer_doc_suggest(layer, slug),
        };
        out.push(candidate(
            ctx,
            NextActionKind::WriteVerification,
            id,
            format!("{id} has unverified acceptance criteria"),
            suggestion,
        ));
    }
    out
}

/// The layer a `refine` of an item in `layer` writes its child into: the
/// shallowest left-side layer below it, preferring layers the project
/// already uses (an in-use deeper layer is where the refinement belongs even
/// when a shallower unused one sits in between). `None` for the deepest left
/// layer — there is nothing to refine into.
fn refine_target_layer<'a>(
    ctx: &Ctx<'a>,
    layer: &RegisteredLayer,
    in_use: &HashSet<&str>,
) -> Option<&'a RegisteredLayer> {
    let mut deeper: Vec<&RegisteredLayer> = ctx
        .registry
        .iter()
        .filter(|r| r.side == LayerSide::Left && r.level > layer.level)
        .collect();
    deeper.sort_by_key(|r| r.level);
    let in_use_deeper = deeper.iter().find(|r| in_use.contains(r.id.as_str()));
    // Layers inferred from the documents (no `[trace] layers`, no profile)
    // grow as documents are added, so the nearest deeper layer is a fair
    // suggestion. A declared layer set (a profile such as `minimal`/`bugfix`,
    // or `[trace] layers`) is a decision: a layer outside it is not one to
    // refine into — the gap is closed by a task instead.
    let layers_declared = ctx.graph.in_use_layers().source != LayersSource::Auto;
    in_use_deeper
        .or_else(|| {
            if layers_declared {
                None
            } else {
                deeper.first()
            }
        })
        .copied()
}

/// The next free `<PREFIX>-<n>` id in `layer`, skipping every id already in
/// the corpus or already handed out by an earlier candidate of this call
/// (`taken`), so two refine suggestions can never target the same new item.
fn next_free_id(ctx: &Ctx, layer: &RegisteredLayer, taken: &mut HashSet<String>) -> Option<String> {
    let prefix = layer.default_id_prefixes.first()?;
    let lead = format!("{prefix}-");
    let highest = ctx
        .input
        .items
        .iter()
        .map(|i| i.stable_id.as_str())
        .chain(taken.iter().map(String::as_str))
        .filter_map(|id| id.strip_prefix(lead.as_str()))
        .filter_map(|tail| tail.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    let id = format!("{lead}{:03}", highest + 1);
    taken.insert(id.clone());
    Some(id)
}

/// §3.5's `kind: refine` — a left-side item whose vertical coverage is
/// `Uncovered` or `Partial`. The suggestion previews (`dry_run`) a new item
/// in the next deeper layer's document that `refines` this one; when the
/// deeper layer has no document yet it creates one. Skipped for an item in
/// the deepest left layer, which has no deeper layer to refine into (its
/// vertical gap is closed by an implementing task — `create_task`).
fn refine_candidates(ctx: &Ctx, ids: &HashSet<String>) -> Vec<Candidate> {
    let in_use: HashSet<&str> = ctx
        .graph
        .in_use_layers()
        .layers
        .iter()
        .map(String::as_str)
        .collect();
    let mut taken = HashSet::new();
    let mut out = Vec::new();
    for id in coverage_gap_ids(ctx, ids, TraceGraph::item_vertical) {
        let Some(layer) = ctx.layer_of(id) else {
            continue;
        };
        let Some(target) = refine_target_layer(ctx, layer, &in_use) else {
            continue;
        };
        let suggestion = match ctx.doc_for_layer(target) {
            LayerDoc::Existing(doc) => {
                let Some(new_id) = next_free_id(ctx, target, &mut taken) else {
                    continue;
                };
                suggest(
                    "handoff_trace_update",
                    serde_json::json!({
                        "dry_run": true,
                        "ops": [{
                            "op": "upsert_item",
                            "doc": doc,
                            "id": new_id,
                            "title": format!("Refinement of {id}"),
                            "attrs": {"refines": [id]},
                        }],
                    }),
                )
            }
            LayerDoc::Missing { layer, slug } => create_layer_doc_suggest(layer, slug),
        };
        out.push(candidate(
            ctx,
            NextActionKind::Refine,
            id,
            format!("{id} has no refining child"),
            suggestion,
        ));
    }
    out
}

/// §3.5's `kind: create_task` — a left-side item (a leaf, or a composite/
/// decision with integrated acceptance criteria, SPEC-TREE-014) whose own `dev_stage` is
/// `not_started` (absent defaults to `not_started`, same convention
/// `src/mcp/handlers/docs.rs`'s own dev_stage readers use), has no task
/// holding an `implements` link to it, and is not already vertically
/// `covered` through its refining children.
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
        // SPEC-TREE-014: tasks belong on leaves (or on a composite/decision
        // that carries its own integrated acceptance criteria).
        if ctx.graph.is_unintegrated_composite(id) {
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
        // A vertically covered item (deeper layer in use and a refining
        // child exists, or an active implementing task) has its work tracked
        // on those children: a task on the parent would duplicate it. Only a
        // `partial`/`uncovered` vertical still calls for a task here.
        if ctx.graph.item_vertical(id) == Some(CoverageStatus::Covered) {
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
/// baseline(dry_run=false)` resolves it).
fn baseline_candidates(ctx: &Ctx, ids: &HashSet<String>) -> Vec<Candidate> {
    let mut out = Vec::new();
    for link in ctx.graph.unbaselined_links() {
        if !ids.contains(link.item.as_str()) {
            continue;
        }
        let id = &link.item;
        // `dry_run` defaults to true on this tool, so the write has to be
        // asked for explicitly; scoped to the item's own document when it is
        // known so a suggestion for one item does not baseline the project.
        let mut arguments = serde_json::json!({"action": "baseline", "dry_run": false});
        if let Some(item) = ctx.item(id).filter(|i| !i.doc_id.is_empty()) {
            arguments["scope"] = serde_json::json!({"doc": item.doc_id});
        }
        out.push(candidate(
            ctx,
            NextActionKind::Baseline,
            id,
            format!("{id} -> {} has no recorded baseline hash", link.upstream),
            suggest("handoff_trace_suspect", arguments),
        ));
    }
    out
}

/// M3 (wiki/270-vmodel-m3-design.md §4.7, FR-204) `kind: relink_candidate` —
/// detects a `unit_test` item (or task) that still links straight to a
/// `basic_spec` item (`verifies`/`implements`) even though a `detailed_spec`
/// item now `refines` that same `basic_spec` target. Fires only once
/// `detailed_spec` is itself in the project's in-use layer set
/// ([`TraceGraph::in_use_layers`]) — "途中からの層追加" (§4.7) means a project
/// that used to stop at `basic_spec`/`unit_test` and has only just grown a
/// `detailed_spec` tier; before that addition there is nothing to relink to,
/// so no candidate should ever appear.
///
/// Implemented generically over every left-side layer pair one level
/// deeper rather than hardcoding the `basic_spec`/`detailed_spec` ids
/// verbatim — a `full` profile's `detailed_spec` -> (hypothetical deeper
/// custom layer) relationship would need the identical rule, and the
/// registry already carries `side`/`level` for exactly this purpose (same
/// "don't hardcode built-in ids where the registry already answers the
/// question" convention [`is_verifier`]/`create_task_candidates`'s `side`
/// lookups follow).
///
/// A `unit_test`-side verifier gets a `trace_update(upsert_item, dry_run=true)`
/// suggest rewriting its own `verifies` attribute (§4.7's literal wording) —
/// a task's `implements` link is not a layer-item body attribute at all
/// (`TaskLink`s live on the task side, D3), so a task candidate instead gets
/// a `handoff_update_task` suggest rewriting `requirement_ids`/
/// `requirement_roles` to point at the `detailed_spec` item, the actual tool
/// that owns that link (wiki/220 §2.5, D3).
fn relink_candidate_candidates(ctx: &Ctx, ids: &HashSet<String>) -> Vec<Candidate> {
    let mut out = Vec::new();

    let left_layer_of = |id: &str| -> Option<&RegisteredLayer> {
        ctx.layer_of(id).filter(|r| r.side == LayerSide::Left)
    };

    // basic_spec-like id -> every detailed_spec-like id that refines it, in
    // stable_id order (§4.7: "対応する detailed_spec 項目がすでに存在する
    // もの"). Several can exist; the first one the relinking party may
    // legally link to is the one suggested (see `may_link_to`).
    let mut deeper_refiners: HashMap<&str, Vec<&str>> = HashMap::new();
    let in_use_layers: HashSet<&str> = ctx
        .graph
        .in_use_layers()
        .layers
        .iter()
        .map(String::as_str)
        .collect();
    let mut ds_items: Vec<&TraceItemInput> = ctx
        .input
        .items
        .iter()
        .filter(|i| {
            left_layer_of(&i.stable_id).is_some_and(|r| in_use_layers.contains(r.id.as_str()))
        })
        .collect();
    ds_items.sort_by(|a, b| a.stable_id.cmp(&b.stable_id));
    for ds in &ds_items {
        let Some(ds_layer) = left_layer_of(&ds.stable_id) else {
            continue;
        };
        for parent in &ds.refines {
            let Some(parent_layer) = left_layer_of(parent) else {
                continue;
            };
            if parent_layer.level + 1 != ds_layer.level {
                continue;
            }
            let refiners = deeper_refiners.entry(parent.as_str()).or_default();
            if !refiners.contains(&ds.stable_id.as_str()) {
                refiners.push(&ds.stable_id);
            }
        }
    }

    if deeper_refiners.is_empty() {
        return out;
    }

    // unit_test-side direct verifiers of a relinkable basic_spec-like target.
    // A `verifies` edge is only legal from a right-side item whose level is at
    // least the target's (the engine's `verifies_edge_valid`), so a verifier
    // is only pointed at a deeper item it may actually verify — otherwise the
    // "improvement" would itself be an `invalid_link`.
    let may_verify = |verifier: &TraceItemInput, target: &str| -> bool {
        let (Some(v), Some(t)) = (ctx.layer_of(&verifier.stable_id), left_layer_of(target)) else {
            return false;
        };
        v.side == LayerSide::Right && v.level >= t.level
    };
    for item in &ctx.input.items {
        let id = &item.stable_id;
        if !ids.contains(id.as_str()) {
            continue;
        }
        let mut seen_bases: HashSet<&str> = HashSet::new();
        for target in &item.verifies {
            let base = target.split('#').next().unwrap_or(target.as_str());
            if !seen_bases.insert(base) {
                continue;
            }
            let Some(ds_id) = deeper_refiners
                .get(base)
                .and_then(|ds| ds.iter().find(|d| may_verify(item, d)))
            else {
                continue;
            };
            // Swap only the relinked target; every other `verifies` entry is
            // kept (attrs.verifies replaces the whole list). An `#AC` sub-ref
            // names a criterion of the old target, so it does not carry over.
            let mut new_verifies: Vec<&str> = Vec::new();
            for v in &item.verifies {
                let b = v.split('#').next().unwrap_or(v.as_str());
                let entry = if b == base { *ds_id } else { v.as_str() };
                if !new_verifies.contains(&entry) {
                    new_verifies.push(entry);
                }
            }
            out.push(candidate(
                ctx,
                NextActionKind::RelinkCandidate,
                id,
                format!(
                    "{id} verifies {base} directly, but {ds_id} now refines {base} — consider \
                     relinking {id} to verify {ds_id} instead"
                ),
                suggest(
                    "handoff_trace_update",
                    serde_json::json!({
                        "dry_run": true,
                        "ops": [{
                            "op": "upsert_item",
                            "doc": item.doc_id,
                            "id": id,
                            "attrs": {"verifies": new_verifies},
                        }],
                    }),
                ),
            ));
        }
    }

    // Tasks directly `implements`-linked to a relinkable basic_spec-like
    // target. `requirement_ids` REPLACES a task's whole link set, so the
    // suggestion carries the task's other links (and their roles) over.
    for link in &ctx.input.task_requirement_links {
        if link.role != TaskLinkRole::Implements {
            continue;
        }
        if !ids.contains(link.stable_id.as_str()) {
            continue;
        }
        let Some(ds_id) = deeper_refiners
            .get(link.stable_id.as_str())
            .and_then(|ds| ds.first())
        else {
            continue;
        };
        let mut requirement_ids: Vec<&str> = Vec::new();
        let mut requirement_roles = serde_json::Map::new();
        for l in ctx
            .input
            .task_requirement_links
            .iter()
            .filter(|l| l.task_id == link.task_id)
        {
            let (target, role) = if l.stable_id == link.stable_id {
                (*ds_id, TaskLinkRole::Implements)
            } else {
                (l.stable_id.as_str(), l.role)
            };
            if requirement_ids.contains(&target) {
                continue;
            }
            requirement_ids.push(target);
            let role = match role {
                TaskLinkRole::Implements => "implements",
                TaskLinkRole::Executes => "executes",
            };
            requirement_roles.insert(target.to_string(), serde_json::Value::from(role));
        }
        let m = ctx.meta_for(&link.stable_id);
        out.push(Candidate {
            key: ctx.sort_key(NextActionKind::RelinkCandidate, &link.stable_id, &m),
            action: NextAction {
                rank: NextActionKind::RelinkCandidate.rank(),
                kind: NextActionKind::RelinkCandidate,
                item: Some(link.stable_id.clone()),
                task: Some(link.task_id.clone()),
                priority: m.priority,
                reason: format!(
                    "task {} implements {} directly, but {ds_id} now refines it — consider \
                     relinking the task to {ds_id} instead",
                    link.task_id, link.stable_id
                ),
                suggest: suggest(
                    "handoff_update_task",
                    serde_json::json!({
                        "task": {
                            "id": link.task_id,
                            "requirement_ids": requirement_ids,
                            "requirement_roles": serde_json::Value::Object(requirement_roles),
                        },
                    }),
                ),
            },
        });
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

    // A task's scope is the items it links; an implicit acceptance item
    // (`REQ-001#AC1`) is part of the item it verifies (nothing links a task
    // to it directly), so it comes along with its parent.
    let implicit_parent: HashMap<&str, &str> = input
        .items
        .iter()
        .filter_map(|i| Some((i.stable_id.as_str(), i.implicit_of.as_deref()?)))
        .collect();
    let all_ids: HashSet<String> = input
        .items
        .iter()
        .map(|i| i.stable_id.clone())
        .filter(|id| {
            scope_ids.is_none_or(|s| {
                s.contains(id)
                    || implicit_parent
                        .get(id.as_str())
                        .is_some_and(|parent| s.contains(*parent))
            })
        })
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
        items_by_id: {
            let mut by_id: HashMap<&str, &TraceItemInput> = HashMap::new();
            for it in &input.items {
                by_id.entry(it.stable_id.as_str()).or_insert(it);
            }
            by_id
        },
    };

    let mut candidates: Vec<Candidate> = Vec::new();
    candidates.extend(fix_failing_candidates(&ctx, &ordered_ids));
    candidates.extend(review_suspect_candidates(&ctx, &all_ids));
    candidates.extend(rerun_candidates(&ctx, &all_ids));
    candidates.extend(manual_pending_candidates(&ctx, &all_ids));
    candidates.extend(write_verification_candidates(&ctx, &all_ids));
    candidates.extend(refine_candidates(&ctx, &all_ids));
    candidates.extend(create_task_candidates(&ctx, &all_ids));
    candidates.extend(fix_link_candidates(&ctx, &all_ids));
    candidates.extend(baseline_candidates(&ctx, &all_ids));
    candidates.extend(relink_candidate_candidates(&ctx, &all_ids));

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

#[cfg(test)]
mod implicit_tests;
