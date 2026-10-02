//! Pure input/output types for M1 trace derivation
//! (wiki/220-vmodel-integration-design.md §2.7, t360.9).
//!
//! [`TraceInput`] is deliberately decoupled from `SubItem`/`DocMetadata`/
//! `TaskData` (owned by `src/storage/docs/model.rs` and `src/storage/tasks.rs`,
//! which developer B/C are editing in this session) so [`super::engine`]
//! stays a pure function over plain data: no file I/O, no dependency on the
//! live storage layer beyond the read-only type references here.
//! `super::adapter` shows how a caller (t360.10/11) turns real documents,
//! tasks, and the `runs/_latest.json` cache into this shape.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::storage::docs::layer::{LayerRegistry, RegisteredLayer};

/// One layer item's data as needed for trace derivation (wiki/220 §2.3/§2.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceItemInput {
    pub stable_id: String,
    /// The `id` of the document this item lives in — used by the
    /// `task_unlinked` gap (wiki/220 §2.7) to tell whether a task's item-level
    /// requirement links actually reach into a doc-linked layer document.
    pub doc_id: String,
    /// Effective layer id (`sub.layer.or(doc.layer)`, wiki/220 §2.3). `None`
    /// (or an id `storage::docs::layer::builtin_layer` doesn't recognize) —
    /// "層なし" (wiki/220 §2.1) — excludes the item from every state/
    /// coverage/gap computation below; it remains resolvable as a link
    /// *target* (not `dangling`) but any link relying on its side/level
    /// (e.g. a `refines`/`verifies` edge pointing at it) is `invalid_link`.
    pub layer: Option<String>,
    /// stable_ids of upper left-side items this one refines (wiki/220 §2.3).
    pub refines: Vec<String>,
    /// stable_ids of left-side items this (right-side or inline) item
    /// verifies (wiki/220 §2.3). M2 (wiki/260 §2.2): an entry may carry an
    /// acceptance-criteria sub-reference (`"REQ-003#AC2"`) — the edge itself
    /// is resolved against the part before `#` (`super::engine`'s job), the
    /// suffix only narrows which of the target's `acceptance_labels` this
    /// verifier counts as having covered (§3.1's horizontal `partial`).
    pub verifies: Vec<String>,
    /// `- method: <value>` attribute presence (wiki/220 §2.2) — one of the
    /// two triggers for inline verification (§2.7) on a left-side item.
    pub method: Option<String>,
    /// Whether this item has at least one `- test: <path::name>` reference
    /// (`SubItem.test_refs` non-empty) — the other inline-verification
    /// trigger (§2.7).
    pub has_test_refs: bool,
    /// M2 (wiki/260 §2.2/§2.3): this (left-side) item's parsed
    /// acceptance-criteria labels (`SubItem.acceptance[].label`, e.g.
    /// `["AC1", "AC2"]`). Empty means "no acceptance block" — horizontal
    /// coverage then stays the M1 binary covered/uncovered/na (no `partial`
    /// is possible without a declared AC list, §3.1).
    pub acceptance_labels: Vec<String>,
    /// M2 §2.2/§3.1: `- derived: <reason>` was present — suppresses the
    /// `orphan` gap for this item (the reason text itself is storage-only,
    /// irrelevant to derivation).
    pub derived: bool,
    /// M2 §2.2/§3.1: `- waive-verify:` / `- waive-refine:` axes present on
    /// this item (the reason text is storage-only).
    pub waived_axes: Vec<WaiverAxis>,
    /// M2 (wiki/260 §2.3/§2.4/§3.2, M2-05): `SubItem::def_hash` — `None` for
    /// an item never synced by an M2-02-or-later binary. This item's
    /// *current* hash, compared against every `link`/`task` suspect baseline
    /// that targets it as a whole-item reference (`"X"`, not `"X#ACn"`) and
    /// against a passing verifier's own recorded `def_hash` (`result`
    /// suspect). An `X#ACn` upstream reference instead resolves against the
    /// implicit acceptance-verification item `"X#ACn"`'s own `def_hash`
    /// (which mirrors `ac_hash(X, ACn)` by construction, §2.4/§2.5 step 4) —
    /// looked up via this same field on *that* item, not recomputed here
    /// (`trace::engine`/`trace::suspect` are pure functions with no body
    /// text to re-parse, D1).
    pub def_hash: Option<String>,
    /// M2 (wiki/260 §2.3/E13, M2-05): `SubItem::body_hash` — the `result`
    /// suspect's fallback comparison for a run recorded before this item
    /// ever had a `def_hash` (a pre-M2-02 run, §7 compat).
    pub body_hash: Option<String>,
    /// M2 (wiki/260 §2.3/§2.5/§3.2, M2-05): `SubItem::link_baselines` —
    /// keyed by the literal authored `refines`/`verifies` reference
    /// (`"REQ-003"` or `"REQ-003#AC2"`), value is that reference's hash at
    /// the moment it was first added. A reference present in `refines`/
    /// `verifies` but absent here is "unbaselined" (§7: never silently
    /// backfilled — only `trace_suspect(action="baseline")` does that).
    pub link_baselines: BTreeMap<String, String>,
    /// M3 (wiki/270-vmodel-m3-design.md §2.1, FR-202): `SubItem.needs`'s
    /// 3-state value, carried through verbatim — `None` means "apply the
    /// resolved profile's `default_needs` for this item's layer"
    /// (`super::profile::ResolvedProfile::default_needs`), `Some(vec![])`
    /// means "no coverage required", `Some(non-empty)` names the exact set
    /// of layer ids this item requires coverage from (horizontal
    /// verify-from, vertical refine-from), overriding `default_needs`.
    pub needs: Option<Vec<String>>,
}

/// One `- waive-verify:` / `- waive-refine:` axis (wiki/260 §2.2/§2.3,
/// `SubItem::waivers`'s `axis` field, mirrored here without the reason text
/// derivation doesn't need).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaiverAxis {
    Verify,
    Refine,
}

/// A resolved profile's `{name, layers}` (wiki/260 §2.1) as needed by the
/// per-item tree-inheritance rules (§2.1 規則 1-4, M2-03) — a document-level
/// `trace_profile` override resolves to one of these. The *un-named* project
/// default tier is represented separately, via `TraceInput::project_default_profile_name`
/// together with the existing project-wide `resolve_in_use_layers` output,
/// since it can come from raw `[trace] layers` or auto-detection, neither of
/// which has a profile name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveProfile {
    pub name: String,
    pub layers: Vec<String>,
}

/// A task's relationship to a `link_type: "requirement"` target (wiki/220
/// §2.5's `TaskLink.role`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskLinkRole {
    Implements,
    Executes,
}

/// One task-side requirement link (wiki/220 §2.5, D3 — the task is the
/// authority; `SubItem.task_ids` is a derived view this module does not
/// need).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRequirementLink {
    pub task_id: String,
    pub stable_id: String,
    pub role: TaskLinkRole,
    /// M2 (wiki/260 §2.3/§3.2, M2-05): `TaskLink::baseline_hash` — the
    /// linked requirement `SubItem`'s `def_hash` at the moment this link was
    /// first added. `None` (unbaselined, §7) for a pre-M2-04 link, never
    /// silently backfilled.
    pub baseline_hash: Option<String>,
}

/// A `TaskLink { link_type: "doc" }` — a task linked to a document as a
/// whole (wiki/220 §2.7's `task_unlinked` gap: "層文書を doc リンクしている
/// が項目リンクがないタスク").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskDocLink {
    pub task_id: String,
    pub doc_id: String,
}

/// Everything [`super::engine::TraceGraph::build`] needs, gathered once per
/// request (wiki/240-performance-design.md §5-5: "1 リクエスト内でグラフを
/// 1回だけ構築する").
#[derive(Debug, Clone)]
pub struct TraceInput {
    pub items: Vec<TraceItemInput>,
    pub task_requirement_links: Vec<TaskRequirementLink>,
    pub task_doc_links: Vec<TaskDocLink>,
    /// ids of documents whose frontmatter sets `layer` (wiki/220 §2.1) — the
    /// "層文書" a `task_doc_links` entry might point at.
    pub layer_doc_ids: std::collections::HashSet<String>,
    /// Latest recorded result per stable_id (`runs/_latest.json`'s
    /// `items[id].result`, t360.8's `LatestCache`), one of `pass`/`fail`/
    /// `blocked`/`not_run`/`skipped`.
    pub runs_latest: HashMap<String, String>,
    /// M2 (wiki/260 §3.2/E13, M2-05): the `{def_hash, body_hash}` pair
    /// recorded alongside each `runs_latest` entry's result — a *separate*
    /// map (not a richer `runs_latest` value type) so every existing
    /// state-DP caller/test that treats `runs_latest` as a plain result
    /// string keeps compiling unchanged. Only consulted for an id whose
    /// `runs_latest` entry is `"pass"` (`trace_suspect`'s `result`-kind
    /// check, §3.2).
    pub runs_latest_hashes: HashMap<String, RunResultHashes>,
    /// stable_id -> owning document ids, project-wide (t360.2's
    /// `collect_all_stable_ids` output shape, reused verbatim — a stable_id
    /// mapping to more than one document is a `duplicate_id` gap).
    pub stable_id_owners: HashMap<String, Vec<String>>,
    /// `[trace] layers` config (wiki/220 §2.1). Empty = not configured;
    /// falls through to `profile_layers`, then auto-detection from `items`
    /// (wiki/260 §2.1, M2-01: "layers ＞ profile ＞ auto").
    pub configured_layers: Vec<String>,
    /// The project default profile's resolved `layers` list (wiki/260 §2.1),
    /// used only when `configured_layers` is empty. Empty = no profile
    /// applies (or the profile declares no layers), falling through to
    /// auto-detection. This is the project-wide (not per-item/tree) profile
    /// resolution — per-item effective-layer/tree-inheritance is M2-03's
    /// scope (§2.1 regla 1-4).
    pub profile_layers: Vec<String>,
    /// The project's full layer registry (built-ins + valid `[[trace.layer]]`
    /// declarations, wiki/260 §2.1, M2-01) — every layer lookup in
    /// [`super::engine`] goes through this instead of the old direct
    /// `storage::docs::layer::BUILTIN_LAYERS`/`builtin_layer` references, so
    /// a project-defined layer is resolved exactly like a built-in one.
    pub layer_registry: Vec<RegisteredLayer>,
    /// M2 §2.1 規則 1: per-document `trace_profile` override, already
    /// resolved to `{name, layers}` (unresolvable overrides — unknown name,
    /// cycle — are simply absent here, so their document falls back to the
    /// project default tier, same "設定エラーは無効化" policy as everywhere
    /// else in §2.1; the caller that builds this map is responsible for
    /// surfacing the warning `resolve_profile_by_name` already returns).
    /// Keyed by `doc_id` (not stable_id) — the override applies to every
    /// root item that document owns.
    pub doc_profile_overrides: HashMap<String, EffectiveProfile>,
    /// M2 §2.1 規則 1/2: the project default profile's own name, when it
    /// comes from a *named* profile (`[trace] profile = "..."`). `None` when
    /// the project default instead comes from raw `[trace] layers` or
    /// auto-detection (regla 2: `[trace] layers` explicit still serves as
    /// "プロジェクト既定の使用層" for tree-inheritance purposes, it is just
    /// unnamed — `profile_layers`/auto-detected layers already carry that
    /// value via the existing `resolve_in_use_layers` project-wide
    /// computation, this field only supplies the *name* half for
    /// `items[].profile`).
    pub project_default_profile_name: Option<String>,
    /// FR-202 (wiki/270 §2.1): the project default profile's resolved
    /// `default_needs` (`<layer id> -> [required layer id, ...]`), used as
    /// an item's effective coverage requirement when its own
    /// `TraceItemInput::needs` is `None`. Empty when no project default
    /// profile applies (same "falls through to no requirement" policy as an
    /// empty `profile_layers`). A per-document `trace_profile` override's own
    /// `default_needs` is **not** separately tracked here — §2.1 ties
    /// `default_needs` to "the profile", and M3-01's scope is activating
    /// `needs`/`default_needs` themselves, not extending the existing
    /// per-document tree-inheritance machinery (`doc_profile_overrides`) to
    /// a second profile-level map; a document with its own `trace_profile`
    /// override still falls back to the *project* default's `default_needs`
    /// when an item under it has no explicit `needs`.
    pub project_default_needs: BTreeMap<String, Vec<String>>,
}

/// One `runs_latest` entry's recorded `{def_hash, body_hash}` twin
/// (wiki/260 §3.2/E13, M2-05) — see [`TraceInput::runs_latest_hashes`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunResultHashes {
    pub def_hash: Option<String>,
    pub body_hash: Option<String>,
}

impl Default for TraceInput {
    /// Defaults `layer_registry` to the built-in-only registry (not an empty
    /// `Vec`) so every existing test/caller that only ever used the 6
    /// built-in layers and relies on `..Default::default()` keeps working
    /// unchanged — an empty registry would silently strip every item of its
    /// side/level resolution.
    fn default() -> Self {
        TraceInput {
            items: Vec::new(),
            task_requirement_links: Vec::new(),
            task_doc_links: Vec::new(),
            layer_doc_ids: std::collections::HashSet::new(),
            runs_latest: HashMap::new(),
            runs_latest_hashes: HashMap::new(),
            stable_id_owners: HashMap::new(),
            configured_layers: Vec::new(),
            profile_layers: Vec::new(),
            layer_registry: LayerRegistry::build(&[]).all().to_vec(),
            doc_profile_overrides: HashMap::new(),
            project_default_profile_name: None,
            project_default_needs: BTreeMap::new(),
        }
    }
}

/// Verification state (wiki/220 §2.7). Declaration order is deliberately
/// the priority order from lowest to highest (`Passing` < `Uncovered` <
/// `NotRun` < `Blocked` < `Failing`) so the derived `Ord` lets callers just
/// take `.max()` over an element list — the spec's "優先順位: failing >
/// blocked > not_run > uncovered > passing" read as an ascending `Ord`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemState {
    Passing,
    Uncovered,
    NotRun,
    Blocked,
    Failing,
}

/// One coverage-axis classification for a single left-side item (wiki/220
/// §2.7's horizontal/vertical rules).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    Covered,
    /// M2 (wiki/260 §3.1, §11 Q7): partially satisfied — horizontally, some
    /// but not all of the item's declared acceptance criteria are verified;
    /// vertically ("deep coverage"), a refining child exists but one of its
    /// descendants is itself `uncovered`/`partial`. Never produced when the
    /// item has no acceptance criteria (horizontal) — that case stays the
    /// M1 binary covered/uncovered.
    Partial,
    /// M2 (wiki/260 §3.1): an explained `waive-verify`/`waive-refine`
    /// exemption applies, *and* the axis would otherwise be `uncovered`
    /// (§3.1's priority order: a waiver never overrides an already
    /// `covered`/`partial` result — that combination is `redundant_waiver`,
    /// a lint concern, not a reclassification).
    Waived,
    Uncovered,
    /// The layer needed to satisfy this axis is not in use (wiki/220 §2.1:
    /// "使用中でない層は対象外（n/a）であり、ギャップとして数えない").
    Na,
}

/// Per-layer tally of one coverage axis (wiki/260 §3.1/§5.1 v2: `{covered,
/// partial, uncovered, waived, na}` — v1's `covered` was v2's
/// `covered`+`partial`, v1's `uncovered` was v2's `uncovered`+`waived`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct CoverageCounts {
    pub covered: usize,
    pub partial: usize,
    pub uncovered: usize,
    pub waived: usize,
    pub na: usize,
}

impl CoverageCounts {
    pub(super) fn record(&mut self, status: CoverageStatus) {
        match status {
            CoverageStatus::Covered => self.covered += 1,
            CoverageStatus::Partial => self.partial += 1,
            CoverageStatus::Uncovered => self.uncovered += 1,
            CoverageStatus::Waived => self.waived += 1,
            CoverageStatus::Na => self.na += 1,
        }
    }
}

/// Per-layer tally of `state` across every item in that layer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct StateCounts {
    pub passing: usize,
    pub failing: usize,
    pub blocked: usize,
    pub not_run: usize,
    pub uncovered: usize,
}

impl StateCounts {
    pub(super) fn record(&mut self, state: ItemState) {
        match state {
            ItemState::Passing => self.passing += 1,
            ItemState::Failing => self.failing += 1,
            ItemState::Blocked => self.blocked += 1,
            ItemState::NotRun => self.not_run += 1,
            ItemState::Uncovered => self.uncovered += 1,
        }
    }
}

/// One in-use layer's aggregate (wiki/220 §2.7's output shape: "層ごとに
/// `{total, horizontal, vertical, state}`").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct LayerCoverage {
    pub total: usize,
    pub horizontal: CoverageCounts,
    pub vertical: CoverageCounts,
    pub state: StateCounts,
    /// M2 (wiki/260 §3.2, M2-05): "層ごとの集計に `suspect: {links, tasks,
    /// results, items}` を加える" — counts every suspect whose relevant item
    /// (the child of a `link` suspect, the linked item of a `task` suspect,
    /// the item itself for a `result` suspect) resolves to this layer.
    pub suspect: SuspectCounts,
}

/// [`LayerCoverage::suspect`]'s per-kind tally, plus the number of distinct
/// items carrying at least one suspect of any kind in this layer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct SuspectCounts {
    pub links: usize,
    pub tasks: usize,
    pub results: usize,
    pub items: usize,
}

/// One of the 3 suspect kinds (wiki/260 §3.2, M2-05).
#[derive(Debug, Clone, Copy, PartialEq, Eq, std::hash::Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuspectKind {
    /// A child's `refines`/`verifies` reference whose baseline no longer
    /// matches the upstream's current hash.
    Link,
    /// A task's `implements`/`executes` requirement link whose
    /// `baseline_hash` no longer matches the linked item's current
    /// `def_hash`.
    Task,
    /// A verification item's latest recorded `pass` whose recorded
    /// `def_hash` (or, absent that, `body_hash`) no longer matches the
    /// item's current value.
    Result,
}

/// One suspect entry (wiki/260 §4.1's `trace_suspect(action="list")` output
/// shape: `{kind, item, upstream?, task?, link_type, baseline_hash,
/// current_hash}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Suspect {
    pub kind: SuspectKind,
    /// `link`/`result`: the item itself. `task`: the linked requirement
    /// item (not the task id — see `task` below).
    pub item: String,
    /// `link` only: the literal authored upstream reference (`"REQ-003"` or
    /// `"REQ-003#AC2"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    /// `task` only: the task id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// `link` only: `"refines"` | `"verifies"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_type: Option<String>,
    pub baseline_hash: String,
    pub current_hash: String,
}

/// Per-kind unbaselined tallies (wiki/260 §3.2/§4.1/§7): a reference/link
/// present but never given a baseline hash — never counted as a suspect,
/// only resolved by `trace_suspect(action="baseline")`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct UnbaselinedCounts {
    pub links: usize,
    pub tasks: usize,
}

/// One `refines`/`verifies` reference with no `link_baselines` entry
/// (wiki/260 §2.5 step 4/§4.1's `baseline` action) — `current_hash` is
/// `None` when the upstream can't be resolved yet (dangling, or an
/// unresolvable `X#ACn` sub-reference, §2.5 step 4's "上流が未解決...記録
/// しない").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnbaselinedLink {
    pub item: String,
    pub upstream: String,
    pub link_type: &'static str,
    pub current_hash: Option<String>,
}

/// One `TaskLink { link_type: "requirement" }` with no `baseline_hash`
/// (wiki/260 §4.1's `baseline` action).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnbaselinedTask {
    pub task_id: String,
    pub item: String,
    pub current_hash: Option<String>,
}

/// The 8 gap kinds (wiki/220 §2.7/§7, FR-502/108/105).
#[derive(Debug, Clone, Copy, PartialEq, Eq, std::hash::Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapKind {
    Unverified,
    Unrefined,
    Orphan,
    Dangling,
    InvalidLink,
    Cycle,
    DuplicateId,
    TaskUnlinked,
}

/// One gap-report entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Gap {
    pub kind: GapKind,
    /// The stable_id this gap is about, or (for `task_unlinked`) the task
    /// id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    pub detail: String,
}

/// Where the "使用中の層" set (wiki/220 §2.1) came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayersSource {
    Config,
    /// The project default profile's `layers` (wiki/260 §2.1, M2-01) — used
    /// when `[trace] layers` is unset but a profile resolved a non-empty
    /// layer list.
    Profile,
    Auto,
}

/// The resolved set of in-use layers plus its provenance (wiki/220 §3.2's
/// `trace_layers: {in_use, source}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InUseLayers {
    pub layers: Vec<String>,
    pub source: LayersSource,
}
