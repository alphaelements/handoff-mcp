//! M1 trace derivation engine (wiki/220-vmodel-integration-design.md §2.7,
//! t360.9), extended by M2-03 (wiki/260-vmodel-m2-design.md §2.1/§2.2/§3.1)
//! with per-item effective profiles/layers, `derived`/`waive-*` exemptions,
//! horizontal/vertical `partial` (deep coverage), and `X#ACn` acceptance
//! sub-references: a **pure function**, `TraceGraph::build`, over
//! [`TraceInput`]. No file I/O — wiring this into `handoff_trace_report`/
//! `handoff_trace_slice` and the `_trace_report.json` derived file is
//! t360.10/11's (M1) / M2-04's (M2) scope.
//!
//! Builds the graph exactly once (wiki/240-performance-design.md §5-5) and
//! computes every left-side item's `state` via a memoized DP over the
//! `refines` DAG (`TraceGraph::build`'s `Dp` helper) — each item's state is
//! computed at most once regardless of how many ancestors query it, and a
//! `refines` back-edge (cycle) is detected via an in-progress set and
//! reported as a `cycle` gap rather than recursing forever. M2-03 folds the
//! new vertical "deep coverage" classification (wiki/260 §3.1, §11 Q7) into
//! this *same* traversal (the spec's "メモ化 DP の同じ走査で求める") rather
//! than adding a second recursive pass.

use std::collections::{HashMap, HashSet};

use crate::storage::docs::layer::{LayerSide, RegisteredLayer};

use super::suspect;
use super::types::{
    CoverageStatus, Gap, GapKind, InUseLayers, ItemState, LayersSource, Suspect, TaskLinkRole,
    TraceInput, TraceItemInput, UnbaselinedCounts, UnbaselinedLink, UnbaselinedTask, WaiverAxis,
};

/// An item resolved against the project's [`LayerRegistry`](crate::storage::docs::layer::LayerRegistry)
/// (built-ins + `[[trace.layer]]` custom declarations, wiki/260 §2.1,
/// M2-01) — `side`/`level` are `None` when the item has no layer, or a
/// layer id the registry doesn't recognize (wiki/220 §2.1: "層なし").
#[derive(Debug, Clone)]
struct ResolvedItem {
    doc_id: String,
    layer: Option<String>,
    side: Option<LayerSide>,
    level: Option<u8>,
    refines: Vec<String>,
    verifies: Vec<String>,
    /// Left-side item with a `method` or `test_refs` attribute — verifies
    /// itself via its own run result (wiki/220 §2.7's inline verification).
    is_inline: bool,
    /// M2 §2.2/§2.3: this item's declared acceptance-criteria labels (empty
    /// = no acceptance block, horizontal stays the M1 binary classification).
    acceptance_labels: Vec<String>,
    /// M2 §2.2/§3.1: `- derived:` present — suppresses `orphan`.
    derived: bool,
    /// M2 §2.2/§3.1: `- waive-verify:` present — exempts horizontal.
    waived_verify: bool,
    /// M2 §2.2/§3.1: `- waive-refine:` present — exempts vertical.
    waived_refine: bool,
}

impl ResolvedItem {
    fn resolve(raw: &TraceItemInput, registry: &[RegisteredLayer]) -> Self {
        let def = raw
            .layer
            .as_deref()
            .and_then(|l| registry.iter().find(|r| r.id == l));
        let side = def.map(|d| d.side);
        let is_inline =
            side == Some(LayerSide::Left) && (raw.method.is_some() || raw.has_test_refs);
        let waived_verify = raw.waived_axes.contains(&WaiverAxis::Verify);
        let waived_refine = raw.waived_axes.contains(&WaiverAxis::Refine);
        Self {
            doc_id: raw.doc_id.clone(),
            layer: raw.layer.clone(),
            side,
            level: def.map(|d| d.level),
            refines: raw.refines.clone(),
            verifies: raw.verifies.clone(),
            is_inline,
            acceptance_labels: raw.acceptance_labels.clone(),
            derived: raw.derived,
            waived_verify,
            waived_refine,
        }
    }
}

/// Splits a `verifies` entry into its edge target and, when present, the
/// acceptance-criteria sub-reference (wiki/260 §2.2: `"REQ-003#AC2"` links
/// to `REQ-003` for edge-legitimacy purposes; the `#AC2` suffix only narrows
/// which acceptance criterion this verifier counts as covering, §3.1).
fn split_ac_ref(raw: &str) -> (&str, Option<&str>) {
    match raw.split_once('#') {
        Some((base, ac)) if !ac.is_empty() => (base, Some(ac)),
        _ => (raw, None),
    }
}

/// Result of one MCP-request's worth of trace derivation — built once
/// (wiki/240 §5-5), then queried by report/slice callers.
pub struct TraceGraph {
    items: HashMap<String, ResolvedItem>,
    in_use_layers: InUseLayers,
    in_use: HashSet<String>,
    states: HashMap<String, ItemState>,
    coverage: HashMap<String, super::types::LayerCoverage>,
    gaps: Vec<Gap>,
    /// child (refines source) -> valid parent ids (upward).
    refines_parents: HashMap<String, Vec<String>>,
    /// parent (refines target) -> valid child ids (downward, i.e. "items
    /// that legitimately refine this one").
    refines_children: HashMap<String, Vec<String>>,
    /// verifier -> valid left-side targets it verifies (base ids, deduped —
    /// an `X#ACn` sub-reference resolves to the same base target as a plain
    /// `X` reference here; AC-level detail lives in `verified_by_ac` only).
    verifies_targets: HashMap<String, Vec<String>>,
    /// left-side target -> valid verifiers (base ids, deduped).
    verified_by: HashMap<String, Vec<String>>,
    /// M2 §2.1: per-item effective profile names (sorted, deduped) reached
    /// by walking this item's `refines`/`verifies` chain up to its tree
    /// root(s) (§2.1 規則 1, M2-03) — `items[].profile`.
    effective_profile_names: HashMap<String, Vec<String>>,
    /// M2 §3.1: per-item final horizontal/vertical classification (left-side,
    /// in-scope items only) — `items[].coverage`.
    item_horizontal: HashMap<String, CoverageStatus>,
    item_vertical: HashMap<String, CoverageStatus>,
    /// M2 §3.2 (M2-05): see [`super::suspect::compute`].
    suspects: Vec<Suspect>,
    unbaselined: UnbaselinedCounts,
    unbaselined_links: Vec<UnbaselinedLink>,
    unbaselined_tasks: Vec<UnbaselinedTask>,
    reverify: HashSet<String>,
    /// Every item's own run result, resolved independently of aggregation
    /// (wiki/260 §3.4, M2-07 rework): `tasks[].blockers` for an `implements`
    /// link needs the *own* run of an inline-verified left item (its result
    /// never appears in `verified_by`, which only holds other items'
    /// `verifies` edges onto it) — same `own_run_state` semantics as a
    /// right-side verifier (a missing result resolves to `NotRun`; only a
    /// `skipped` result is absent here, deliberately contributing nothing).
    own_states: HashMap<String, ItemState>,
    /// M2 §2.1 規則 3: items within their own effective scope (in-use layer
    /// reached by their profile tree) — the same set [`Dp::in_scope`] uses to
    /// decide whether a verifier/`refines` child may contribute to the
    /// state/vertical DP. Exposed via [`Self::in_scope`] so other
    /// post-processing passes over this graph (`tasks[].blockers`'s
    /// `implements` branch, wiki/260 §3.4, t360.20.31) can apply the exact
    /// same filter instead of re-deriving a parallel notion of scope.
    in_scope_items: HashSet<String>,
}

fn own_run_state(runs_latest: &HashMap<String, String>, id: &str) -> Option<ItemState> {
    match runs_latest.get(id).map(String::as_str) {
        Some("pass") => Some(ItemState::Passing),
        Some("fail") => Some(ItemState::Failing),
        Some("blocked") => Some(ItemState::Blocked),
        Some("not_run") | None => Some(ItemState::NotRun),
        // skipped results are excluded from aggregation (wiki/220 §2.7);
        // this returns `None` (no element contributed) rather than
        // defaulting to `NotRun` here — callers with no other possible
        // element (a bare right-side verifier) fold that back to `NotRun`
        // themselves ("他に要素がなければ not_run").
        Some("skipped") => None,
        // Any other string is a caller/data bug (`is_valid_result` should
        // have rejected it before it ever reached `runs/_latest.json`) —
        // `not_run` is the safe, non-panicking fallback: it neither hides a
        // pass/fail nor claims coverage that was never demonstrated.
        Some(_) => Some(ItemState::NotRun),
    }
}

/// A right-side (or inline-left) verifier's own displayed state — always
/// resolves to a concrete state since a verifier has no other element to
/// fall back on ("他に要素がなければ not_run").
fn verifier_state(runs_latest: &HashMap<String, String>, id: &str) -> ItemState {
    own_run_state(runs_latest, id).unwrap_or(ItemState::NotRun)
}

impl TraceGraph {
    pub fn build(input: &TraceInput) -> Self {
        let items = resolve_items(&input.items, &input.layer_registry);
        let in_use = resolve_in_use_layers(input, &items);
        let in_use_set: HashSet<String> = in_use.layers.iter().cloned().collect();

        let mut gaps = Vec::new();
        let (refines_parents, refines_children) = build_refines_edges(&items, &mut gaps);
        let (verifies_targets, verified_by, verified_by_ac) =
            build_verifies_edges(&items, &mut gaps);

        let task_implements = task_implements_set(&input.task_requirement_links);

        // M2-03 (wiki/260 §2.1 規則 1-4): per-item effective profile set and
        // the union of those profiles' layers — replaces the M1 single
        // global `in_use` set for every scope/coverage decision below. With
        // no `doc_profile_overrides` at all this reduces to exactly the
        // project-wide `in_use_set` for every item (full M1 backward
        // compatibility).
        let (effective_layers, effective_profile_names) =
            resolve_effective_layers(input, &items, &refines_parents, &verifies_targets, &in_use);
        let in_scope_items: HashSet<String> = items
            .iter()
            .filter(|(id, it)| {
                it.layer
                    .as_deref()
                    .is_some_and(|l| it.side.is_some() && effective_layers[*id].contains(l))
            })
            .map(|(id, _)| id.clone())
            .collect();
        let deeper_layer_in_use: HashMap<String, bool> = items
            .iter()
            .filter(|(_, it)| it.side == Some(LayerSide::Left))
            .map(|(id, it)| {
                let level = it.level.unwrap_or(0);
                let has_deeper = input.layer_registry.iter().any(|l| {
                    l.side == LayerSide::Left
                        && l.level > level
                        && effective_layers[id].contains(l.id.as_str())
                });
                (id.clone(), has_deeper)
            })
            .collect();

        let mut states: HashMap<String, ItemState> = HashMap::new();
        // Right-side items are leaves: resolve directly, no recursion.
        for (id, item) in &items {
            if item.side == Some(LayerSide::Right) {
                states.insert(id.clone(), verifier_state(&input.runs_latest, id));
            }
        }

        let horizontal = precompute_horizontal_coverage(
            &items,
            &in_scope_items,
            &effective_layers,
            &verified_by_ac,
            &input.layer_registry,
        );

        let mut vertical: HashMap<String, CoverageStatus> = HashMap::new();
        let mut dp = Dp {
            items: &items,
            refines_children: &refines_children,
            verified_by: &verified_by,
            runs_latest: &input.runs_latest,
            horizontal: &horizontal,
            in_scope_items: &in_scope_items,
            task_implements: &task_implements,
            deeper_layer_in_use: &deeper_layer_in_use,
            memo: &mut states,
            vertical: &mut vertical,
            in_progress: HashSet::new(),
            reported_cycles: HashSet::new(),
            gaps: &mut gaps,
            #[cfg(test)]
            memo_misses: 0,
        };
        let left_ids: Vec<String> = items
            .iter()
            .filter(|(_, it)| it.side == Some(LayerSide::Left))
            .map(|(id, _)| id.clone())
            .collect();
        for id in &left_ids {
            dp.resolve(id);
        }

        let coverage_status: HashMap<String, (CoverageStatus, CoverageStatus)> = left_ids
            .iter()
            .filter_map(|id| match (horizontal.get(id), vertical.get(id)) {
                (Some(h), Some(v)) => Some((id.clone(), (*h, *v))),
                _ => None,
            })
            .collect();

        push_unverified_unrefined_orphan_gaps(
            &items,
            &in_scope_items,
            &coverage_status,
            &effective_layers,
            &input.layer_registry,
            &mut gaps,
        );
        push_duplicate_id_gaps(&input.stable_id_owners, &items, &mut gaps);
        push_task_unlinked_gaps(input, &items, &mut gaps);

        gaps.sort_by(|a, b| {
            (
                a.kind as u8,
                a.item.as_deref().unwrap_or(""),
                a.detail.as_str(),
            )
                .cmp(&(
                    b.kind as u8,
                    b.item.as_deref().unwrap_or(""),
                    b.detail.as_str(),
                ))
        });

        let mut coverage =
            aggregate_layer_coverage(&items, &in_scope_items, &coverage_status, &states);

        // M2 §3.2 (M2-05): suspects are derived "グラフ構築のついでに" from
        // the same per-request data — folded into the per-layer aggregate
        // (`coverage[layer].suspect`) right after it, keeping
        // `aggregate_layer_coverage` itself unaware of suspects (it's the
        // one function every existing M1/M2-03 test already exercises in
        // isolation).
        let suspect_derivation = suspect::compute(input, &states);
        aggregate_suspect_counts(&items, &suspect_derivation.suspects, &mut coverage);

        let own_states: HashMap<String, ItemState> = items
            .keys()
            .filter_map(|id| own_run_state(&input.runs_latest, id).map(|state| (id.clone(), state)))
            .collect();

        Self {
            items,
            in_use_layers: in_use,
            in_use: in_use_set,
            states,
            coverage,
            gaps,
            refines_parents,
            refines_children,
            verifies_targets,
            verified_by,
            effective_profile_names,
            item_horizontal: horizontal,
            item_vertical: vertical,
            suspects: suspect_derivation.suspects,
            unbaselined: suspect_derivation.unbaselined,
            unbaselined_links: suspect_derivation.unbaselined_links,
            unbaselined_tasks: suspect_derivation.unbaselined_tasks,
            reverify: suspect_derivation.reverify,
            own_states,
            in_scope_items,
        }
    }

    pub fn in_use_layers(&self) -> &InUseLayers {
        &self.in_use_layers
    }

    pub fn is_layer_in_use(&self, layer_id: &str) -> bool {
        self.in_use.contains(layer_id)
    }

    pub fn has_item(&self, stable_id: &str) -> bool {
        self.items.contains_key(stable_id)
    }

    pub fn state(&self, stable_id: &str) -> Option<ItemState> {
        self.states.get(stable_id).copied()
    }

    /// wiki/220 §2.7: a left-side item with a `method` or `test_refs`
    /// attribute verifies itself via its own run result. `tasks[].blockers`
    /// (wiki/260 §3.4, M2-07 rework) needs this predicate to know when an
    /// `implements`-linked item's own run (not just its verifiers') must be
    /// folded into the tally.
    pub fn is_inline(&self, stable_id: &str) -> bool {
        self.items.get(stable_id).is_some_and(|it| it.is_inline)
    }

    /// This item's own run result, resolved independently of the aggregated
    /// [`Self::state`] (wiki/260 §3.4, M2-07 rework) — `NotRun` when there
    /// is no result; `None` only when the latest result is `skipped`
    /// (contributes nothing, same semantics as `own_run_state`).
    pub fn own_state(&self, stable_id: &str) -> Option<ItemState> {
        self.own_states.get(stable_id).copied()
    }

    /// M2 §2.1 規則 3: is `stable_id` within its own effective scope (the
    /// in-use layer set reached by its profile tree)? The same predicate
    /// [`Dp::in_scope`] applies to a verifier/`refines` child before letting
    /// it contribute to the state/vertical DP — `tasks[].blockers`'s
    /// `implements` branch (wiki/260 §3.4, t360.20.31) uses this to apply
    /// the identical filter to `verified_by(item)` instead of treating every
    /// declared verifier as a blocker regardless of whether its layer is
    /// actually in use.
    pub fn in_scope(&self, stable_id: &str) -> bool {
        self.in_scope_items.contains(stable_id)
    }

    pub fn coverage(&self) -> &HashMap<String, super::types::LayerCoverage> {
        &self.coverage
    }

    pub fn gaps(&self) -> &[Gap] {
        &self.gaps
    }

    pub fn gap_counts(&self) -> HashMap<GapKind, usize> {
        let mut counts = HashMap::new();
        for gap in &self.gaps {
            *counts.entry(gap.kind).or_insert(0) += 1;
        }
        counts
    }

    pub fn refines_parents(&self, stable_id: &str) -> &[String] {
        self.refines_parents
            .get(stable_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn refines_children(&self, stable_id: &str) -> &[String] {
        self.refines_children
            .get(stable_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn verifies_targets(&self, stable_id: &str) -> &[String] {
        self.verifies_targets
            .get(stable_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn verified_by(&self, stable_id: &str) -> &[String] {
        self.verified_by
            .get(stable_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// M2 §2.1 規則 4: the sorted, deduped effective profile names reached
    /// by this item's tree (`items[].profile`) — empty when no named
    /// profile applies anywhere in its reachable root set (project default
    /// via raw `[trace] layers`/auto-detection, or the item has no layer).
    pub fn item_profile(&self, stable_id: &str) -> &[String] {
        self.effective_profile_names
            .get(stable_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// M2 §3.1: this item's final horizontal classification — `None` for a
    /// right-side/layerless item, or a left-side item that is itself out of
    /// its own effective scope.
    pub fn item_horizontal(&self, stable_id: &str) -> Option<CoverageStatus> {
        self.item_horizontal.get(stable_id).copied()
    }

    /// M2 §3.1: this item's final vertical ("deep coverage") classification
    /// — same availability rule as [`Self::item_horizontal`].
    pub fn item_vertical(&self, stable_id: &str) -> Option<CoverageStatus> {
        self.item_vertical.get(stable_id).copied()
    }

    /// M2 §3.2/§4.1 (M2-05): every suspect this graph found, sorted
    /// deterministically (kind, item, upstream/task — see
    /// `suspect::suspect_sort_key`).
    pub fn suspects(&self) -> &[Suspect] {
        &self.suspects
    }

    /// M2 §3.2/§7 (M2-05): counts of `refines`/`verifies` references and
    /// task requirement links that have no baseline hash yet — never
    /// suspects themselves (§7: never silently backfilled).
    pub fn unbaselined_counts(&self) -> UnbaselinedCounts {
        self.unbaselined
    }

    /// M2 §4.1's `trace_suspect(action="baseline")` input: every unbaselined
    /// `refines`/`verifies` reference, with its current hash when
    /// resolvable (`None` = can't determine yet, not written).
    pub fn unbaselined_links(&self) -> &[UnbaselinedLink] {
        &self.unbaselined_links
    }

    /// M2 §4.1's `trace_suspect(action="baseline")` input: every unbaselined
    /// task requirement link, with the linked item's current `def_hash` when
    /// resolvable.
    pub fn unbaselined_tasks(&self) -> &[UnbaselinedTask] {
        &self.unbaselined_tasks
    }

    /// M2 §3.2 (FR-402): stable_ids of `Passing` verification items that
    /// need re-verification (their own `result` is suspect, or one of their
    /// `verifies` links is suspect). Does not affect `state` (§11 Q1).
    pub fn reverify_items(&self) -> &HashSet<String> {
        &self.reverify
    }
}

fn resolve_items(
    raw: &[TraceItemInput],
    registry: &[RegisteredLayer],
) -> HashMap<String, ResolvedItem> {
    let mut items = HashMap::with_capacity(raw.len());
    for item in raw {
        // First occurrence wins (wiki/220 §4.2's collision reporting is a
        // separate, project-wide concern fed via `stable_id_owners` — see
        // `push_duplicate_id_gaps`; the graph itself just needs one
        // definition per id to reason about).
        items
            .entry(item.stable_id.clone())
            .or_insert_with(|| ResolvedItem::resolve(item, registry));
    }
    items
}

/// Resolves the "使用中の層" set per wiki/260 §2.1's priority order
/// (`[trace] layers` explicit ＞ project default profile's `layers` ＞ auto
/// from `items`, M2-01). This project-wide tier is also the fallback profile
/// every root without its own document override resolves to (M2-03's
/// `resolve_effective_layers`, §2.1 規則 1-2).
fn resolve_in_use_layers(input: &TraceInput, items: &HashMap<String, ResolvedItem>) -> InUseLayers {
    if !input.configured_layers.is_empty() {
        let mut seen = HashSet::new();
        let layers = input
            .configured_layers
            .iter()
            .filter(|l| seen.insert((*l).clone()))
            .cloned()
            .collect();
        return InUseLayers {
            layers,
            source: LayersSource::Config,
        };
    }
    if !input.profile_layers.is_empty() {
        let mut seen = HashSet::new();
        let layers = input
            .profile_layers
            .iter()
            .filter(|l| seen.insert((*l).clone()))
            .cloned()
            .collect();
        return InUseLayers {
            layers,
            source: LayersSource::Profile,
        };
    }
    let present: HashSet<&str> = items
        .values()
        .filter_map(|it| it.layer.as_deref())
        .collect();
    let layers = input
        .layer_registry
        .iter()
        .filter(|l| present.contains(l.id.as_str()))
        .map(|l| l.id.clone())
        .collect();
    InUseLayers {
        layers,
        source: LayersSource::Auto,
    }
}

/// One document-rooted profile as consumed by the per-item tree walk below —
/// `name: None` is the (possibly unnamed) project default tier.
#[derive(Clone)]
struct RootProfile {
    name: Option<String>,
    layers: HashSet<String>,
}

/// M2-03 (wiki/260 §2.1 規則 1-4, §11 Q6: "リンクでたどれる要件ツリー"):
/// for every item, walks `refines` upward (a right-side/verifier item
/// instead walks its `verifies` targets, per 規則 1's parenthetical) until it
/// reaches root(s) with no further parent, collects each root's own document
/// profile (its `trace_profile` override, else the project default tier),
/// and unions the reached profiles' `layers` (規則 2: "厳しい側に倒す") —
/// this item's "実効使用層". Also returns the sorted, deduped set of *named*
/// profiles reached (`items[].profile`, 規則 4).
///
/// With `input.doc_profile_overrides` empty (no project uses `trace_profile`
/// yet), every item's only reachable root profile is the project default, so
/// this reduces to exactly `project_default.layers` for every item — full M1
/// backward compatibility.
fn resolve_effective_layers(
    input: &TraceInput,
    items: &HashMap<String, ResolvedItem>,
    refines_parents: &HashMap<String, Vec<String>>,
    verifies_targets: &HashMap<String, Vec<String>>,
    project_default: &InUseLayers,
) -> (
    HashMap<String, HashSet<String>>,
    HashMap<String, Vec<String>>,
) {
    let default_profile = RootProfile {
        name: input.project_default_profile_name.clone(),
        layers: project_default.layers.iter().cloned().collect(),
    };

    let mut roots_memo: HashMap<String, HashSet<String>> = HashMap::new();
    let mut ids: Vec<String> = items.keys().cloned().collect();
    ids.sort();
    for id in &ids {
        let mut in_progress = HashSet::new();
        compute_roots(
            id,
            items,
            refines_parents,
            verifies_targets,
            &mut roots_memo,
            &mut in_progress,
        );
    }

    let mut effective_layers = HashMap::with_capacity(items.len());
    let mut effective_profile_names = HashMap::with_capacity(items.len());
    for id in &ids {
        let item = &items[id];
        if item.side.is_none() {
            continue;
        }
        let roots = roots_memo.get(id).cloned().unwrap_or_default();
        let mut layers: HashSet<String> = HashSet::new();
        let mut names: Vec<String> = Vec::new();
        let mut seen_profiles: HashSet<Option<String>> = HashSet::new();
        for root in &roots {
            let Some(root_item) = items.get(root) else {
                continue;
            };
            let profile = input
                .doc_profile_overrides
                .get(&root_item.doc_id)
                .map(|p| RootProfile {
                    name: Some(p.name.clone()),
                    layers: p.layers.iter().cloned().collect(),
                })
                .unwrap_or_else(|| default_profile.clone());
            if seen_profiles.insert(profile.name.clone()) {
                layers.extend(profile.layers.iter().cloned());
                if let Some(name) = &profile.name {
                    names.push(name.clone());
                }
            }
        }
        if roots.is_empty() {
            // Defensive fallback (should not happen — every item is at
            // minimum its own root): never silently exclude an item from
            // every layer.
            layers = default_profile.layers.clone();
        }
        names.sort();
        names.dedup();
        effective_layers.insert(id.clone(), layers);
        effective_profile_names.insert(id.clone(), names);
    }
    (effective_layers, effective_profile_names)
}

/// Cycle-safe, memoized root-finder for [`resolve_effective_layers`]: a
/// left-side item's root set is its `refines` parents' root sets (or itself,
/// if it has none); a right-side item's root set is its `verifies` targets'
/// root sets (or itself, if it has none) — 規則 1's "検証項目は verifies 先
/// の項目の集合を使う". A `refines`/`verifies` cycle (already reported as a
/// `cycle` gap elsewhere) simply contributes nothing further through the
/// back-edge, mirroring the state DP's own cycle handling.
fn compute_roots(
    id: &str,
    items: &HashMap<String, ResolvedItem>,
    refines_parents: &HashMap<String, Vec<String>>,
    verifies_targets: &HashMap<String, Vec<String>>,
    memo: &mut HashMap<String, HashSet<String>>,
    in_progress: &mut HashSet<String>,
) -> HashSet<String> {
    if let Some(cached) = memo.get(id) {
        return cached.clone();
    }
    if !in_progress.insert(id.to_string()) {
        return HashSet::new();
    }
    let result = match items.get(id).map(|it| it.side) {
        Some(Some(LayerSide::Left)) => match refines_parents.get(id) {
            Some(parents) if !parents.is_empty() => {
                let mut acc = HashSet::new();
                for parent in parents {
                    acc.extend(compute_roots(
                        parent,
                        items,
                        refines_parents,
                        verifies_targets,
                        memo,
                        in_progress,
                    ));
                }
                acc
            }
            _ => std::iter::once(id.to_string()).collect(),
        },
        Some(Some(LayerSide::Right)) => match verifies_targets.get(id) {
            Some(targets) if !targets.is_empty() => {
                let mut acc = HashSet::new();
                for target in targets {
                    acc.extend(compute_roots(
                        target,
                        items,
                        refines_parents,
                        verifies_targets,
                        memo,
                        in_progress,
                    ));
                }
                acc
            }
            _ => std::iter::once(id.to_string()).collect(),
        },
        _ => HashSet::new(),
    };
    in_progress.remove(id);
    memo.insert(id.to_string(), result.clone());
    result
}

/// A `refines` edge is legitimate iff both ends are left-side items and the
/// source is strictly deeper (higher `level`) than the target (wiki/220
/// §2.7: "refines: 左側項目 → より上位（level が小さい）の左側項目").
fn refines_edge_valid(child: &ResolvedItem, parent: &ResolvedItem) -> bool {
    child.side == Some(LayerSide::Left)
        && parent.side == Some(LayerSide::Left)
        && matches!((child.level, parent.level), (Some(c), Some(p)) if c > p)
}

/// A `verifies` edge is legitimate iff the source is right-side, the target
/// is left-side, and the verifier's level is at least the target's (wiki/220
/// §2.7: "verifies: 右側項目 → 左側項目、かつ検証項目の level ≥ 対象の
/// level" — this single `>=` condition already covers both "pair 層" and
/// "より下位の検証層" verifiers).
fn verifies_edge_valid(verifier: &ResolvedItem, target: &ResolvedItem) -> bool {
    verifier.side == Some(LayerSide::Right)
        && target.side == Some(LayerSide::Left)
        && matches!((verifier.level, target.level), (Some(v), Some(t)) if v >= t)
}

/// `dangling`/`invalid_link` gaps below are **not** filtered by in-use scope
/// (unlike coverage/state, which skip out-of-use-layer contributors —
/// `precompute_horizontal_coverage`, `Dp::resolve`). They are project-wide
/// data-quality diagnostics: a broken reference or a structurally illegal
/// link is wrong regardless of whether the current `[trace] layers` view
/// happens to include that layer today, and hiding it behind scope would let
/// a real authoring mistake resurface silently the moment scope changes.
fn build_refines_edges(
    items: &HashMap<String, ResolvedItem>,
    gaps: &mut Vec<Gap>,
) -> (HashMap<String, Vec<String>>, HashMap<String, Vec<String>>) {
    let mut parents_of: HashMap<String, Vec<String>> = HashMap::new();
    let mut children_of: HashMap<String, Vec<String>> = HashMap::new();
    let mut ids: Vec<&String> = items.keys().collect();
    ids.sort();
    for id in ids {
        let child = &items[id];
        for parent_id in &child.refines {
            match items.get(parent_id) {
                None => gaps.push(Gap {
                    kind: GapKind::Dangling,
                    item: Some(id.clone()),
                    layer: child.layer.clone(),
                    detail: format!("refines target '{parent_id}' does not resolve to any item"),
                }),
                Some(parent) => {
                    if refines_edge_valid(child, parent) {
                        parents_of
                            .entry(id.clone())
                            .or_default()
                            .push(parent_id.clone());
                        children_of
                            .entry(parent_id.clone())
                            .or_default()
                            .push(id.clone());
                    } else {
                        gaps.push(Gap {
                            kind: GapKind::InvalidLink,
                            item: Some(id.clone()),
                            layer: child.layer.clone(),
                            detail: format!(
                                "refines target '{parent_id}' is not a strictly upper left-side item"
                            ),
                        });
                    }
                }
            }
        }
    }
    (parents_of, children_of)
}

/// verifier -> `(base target id, Some(ac_label) for an `X#ACn` sub-ref)`,
/// deduped per verifier+base-target (§2.2's "全体を優先する") — the AC-level
/// detail `precompute_horizontal_coverage` needs for §3.1's horizontal
/// `partial`, on top of the base-id-only `targets_of`/`verified_by` maps
/// [`build_verifies_edges`] also returns.
type VerifiedByAc = HashMap<String, Vec<(String, Option<String>)>>;

/// [`build_verifies_edges`]'s 3 return maps: base-id-only `targets_of`
/// (verifier -> targets), `verified_by` (target -> verifiers), and the
/// AC-level [`VerifiedByAc`] detail.
type VerifiesEdges = (
    HashMap<String, Vec<String>>,
    HashMap<String, Vec<String>>,
    VerifiedByAc,
);

/// Resolves every `verifies` entry (plain `"X"` or M2's `"X#ACn"` sub-ref,
/// wiki/260 §2.2) into: (1) `targets_of`/`verified_by`, deduped **base**-id
/// maps kept identical in shape to M1 (existing consumers — `trace.rs`'s
/// slice/impact neighbor walk, `verifies_targets`/`verified_by` accessors —
/// see only whole-item ids); (2) `verified_by_ac`, the AC-level detail
/// `precompute_horizontal_coverage` needs for §3.1's horizontal `partial`.
/// Per verifier+target, a plain `"X"` reference always wins over an `"X#ACn"`
/// one to the same base (§2.2: "同じ項目の中で...両方を書いた場合は...全体を
/// 優先する"). An `"X#ACn"` whose `ACn` isn't in `X`'s declared
/// `acceptance_labels` falls back to a whole-item reference (§2.2:
/// "AC2 がない場合は...全体を検証するリンクとして扱う"; the accompanying
/// `unknown_acceptance_ref` lint is `trace_lint`'s concern, not implemented
/// here — no lint-output type exists in this codebase yet).
fn build_verifies_edges(
    items: &HashMap<String, ResolvedItem>,
    gaps: &mut Vec<Gap>,
) -> VerifiesEdges {
    let mut targets_of: HashMap<String, Vec<String>> = HashMap::new();
    let mut verified_by: HashMap<String, Vec<String>> = HashMap::new();
    let mut verified_by_ac: HashMap<String, Vec<(String, Option<String>)>> = HashMap::new();
    let mut ids: Vec<&String> = items.keys().collect();
    ids.sort();
    for id in ids {
        let verifier = &items[id];
        // Per-verifier (base target -> ac label reached, `None` = whole-item)
        // dedup, so a duplicate/"both forms" reference to the same base only
        // ever contributes one edge/one `verified_by_ac` entry.
        let mut per_target: std::collections::BTreeMap<&str, Option<String>> =
            std::collections::BTreeMap::new();
        for raw_target in &verifier.verifies {
            let (base_id, ac_label) = split_ac_ref(raw_target);
            match items.get(base_id) {
                None => gaps.push(Gap {
                    kind: GapKind::Dangling,
                    item: Some(id.clone()),
                    layer: verifier.layer.clone(),
                    detail: format!("verifies target '{raw_target}' does not resolve to any item"),
                }),
                Some(target) => {
                    if !verifies_edge_valid(verifier, target) {
                        gaps.push(Gap {
                            kind: GapKind::InvalidLink,
                            item: Some(id.clone()),
                            layer: verifier.layer.clone(),
                            detail: format!(
                                "verifies target '{raw_target}' is not a left-side item at or below this verifier's level"
                            ),
                        });
                        continue;
                    }
                    // §2.2: an AC reference whose label the target doesn't
                    // declare falls back to a whole-item reference.
                    let effective_ac =
                        ac_label.filter(|ac| target.acceptance_labels.iter().any(|l| l == ac));
                    let entry = per_target
                        .entry(base_id)
                        .or_insert_with(|| effective_ac.map(str::to_string));
                    if effective_ac.is_none() {
                        // A whole-item reference always wins, whichever
                        // order the raw entries were authored in.
                        *entry = None;
                    }
                }
            }
        }
        for (base_id, ac_label) in per_target {
            targets_of
                .entry(id.clone())
                .or_default()
                .push(base_id.to_string());
            verified_by
                .entry(base_id.to_string())
                .or_default()
                .push(id.clone());
            verified_by_ac
                .entry(base_id.to_string())
                .or_default()
                .push((id.clone(), ac_label));
        }
    }
    (targets_of, verified_by, verified_by_ac)
}

fn task_implements_set(links: &[super::types::TaskRequirementLink]) -> HashSet<String> {
    links
        .iter()
        .filter(|l| l.role == TaskLinkRole::Implements)
        .map(|l| l.stable_id.clone())
        .collect()
}

/// wiki/260 §3.1's horizontal classification (na → covered → partial →
/// waived → uncovered, restated as: verifier-based coverage first, then the
/// no-verifier na/waived/uncovered split) for every left-side, in-scope
/// item — computed once, ahead of the state/vertical DP (which needs it as
/// an element input) and reused again for the `unverified` gap.
fn precompute_horizontal_coverage(
    items: &HashMap<String, ResolvedItem>,
    in_scope_items: &HashSet<String>,
    effective_layers: &HashMap<String, HashSet<String>>,
    verified_by_ac: &HashMap<String, Vec<(String, Option<String>)>>,
    registry: &[RegisteredLayer],
) -> HashMap<String, CoverageStatus> {
    let mut out = HashMap::new();
    for (id, item) in items {
        if item.side != Some(LayerSide::Left) || !in_scope_items.contains(id) {
            continue;
        }
        let layer_id = item.layer.as_deref().expect("in_scope implies layer set");
        let def = registry
            .iter()
            .find(|r| r.id == layer_id)
            .expect("in_scope implies a registered layer");

        // A verifier living in a layer that isn't in *its own* effective
        // scope must not count toward coverage either — same "使用中でない
        // 層は対象外" policy the state DP applies (wiki/220 §2.1), kept
        // consistent here so coverage and state never disagree.
        let mut whole_covered = item.is_inline;
        let mut covered_acs: HashSet<&str> = HashSet::new();
        if let Some(verifiers) = verified_by_ac.get(id) {
            for (verifier_id, ac_label) in verifiers {
                if !in_scope_items.contains(verifier_id) {
                    continue;
                }
                match ac_label {
                    None => whole_covered = true,
                    Some(label) => {
                        covered_acs.insert(label.as_str());
                    }
                }
            }
        }

        let status = if whole_covered {
            CoverageStatus::Covered
        } else if !covered_acs.is_empty() {
            if item.acceptance_labels.is_empty()
                || item
                    .acceptance_labels
                    .iter()
                    .all(|l| covered_acs.contains(l.as_str()))
            {
                CoverageStatus::Covered
            } else {
                CoverageStatus::Partial
            }
        } else if effective_layers[id].contains(&def.pair) {
            if item.waived_verify {
                CoverageStatus::Waived
            } else {
                CoverageStatus::Uncovered
            }
        } else {
            CoverageStatus::Na
        };
        out.insert(id.clone(), status);
    }
    out
}

/// Memoized DP over the `refines` DAG (wiki/240 §5-5): each left-side item's
/// state *and* vertical coverage classification (wiki/260 §3.1's "deep
/// coverage", M2-03) are resolved together, at most once, via `resolve`,
/// with `in_progress` acting as the "訪問済み集合" that turns a `refines`
/// back-edge into a `cycle` gap instead of infinite recursion.
struct Dp<'a> {
    items: &'a HashMap<String, ResolvedItem>,
    refines_children: &'a HashMap<String, Vec<String>>,
    verified_by: &'a HashMap<String, Vec<String>>,
    runs_latest: &'a HashMap<String, String>,
    horizontal: &'a HashMap<String, CoverageStatus>,
    /// Items currently within their own effective scope (wiki/260 §2.1
    /// 規則 3, M2-03) — a verifier or a refines child *not* in this set must
    /// not feed this item's state/vertical (n/a, not a silent pull-down).
    in_scope_items: &'a HashSet<String>,
    task_implements: &'a HashSet<String>,
    /// Per (left) item: is there an in-use-for-*this item* left layer deeper
    /// than its own level (wiki/220 §2.1's "より下位の層")?
    deeper_layer_in_use: &'a HashMap<String, bool>,
    memo: &'a mut HashMap<String, ItemState>,
    vertical: &'a mut HashMap<String, CoverageStatus>,
    in_progress: HashSet<String>,
    reported_cycles: HashSet<String>,
    gaps: &'a mut Vec<Gap>,
    /// Test-only instrumentation: counts actual (non-memo-hit) computations,
    /// so a test can assert each item is resolved exactly once regardless of
    /// fan-in (wiki/240 §5-5's memoized DP).
    #[cfg(test)]
    memo_misses: usize,
}

impl Dp<'_> {
    fn in_scope(&self, id: &str) -> bool {
        self.in_scope_items.contains(id)
    }

    /// wiki/260 §3.1's vertical rule, folded into the same traversal as
    /// `state` — `in_scope_children`'s own `vertical` entries are already
    /// memoized by the time this runs (they were each `resolve`d earlier in
    /// this same call, per the children loop in [`Self::resolve`]).
    fn compute_vertical(&self, id: &str, in_scope_children: &[String]) -> CoverageStatus {
        let item = &self.items[id];
        let has_impl_task = self.task_implements.contains(id);
        let has_child = !in_scope_children.is_empty();
        let deeper_layer_in_use = *self.deeper_layer_in_use.get(id).unwrap_or(&false);
        let base_covered = if deeper_layer_in_use {
            has_child || has_impl_task
        } else {
            has_impl_task
        };
        let status = if !base_covered {
            CoverageStatus::Uncovered
        } else if has_child {
            let any_bad = in_scope_children.iter().any(|c| {
                matches!(
                    self.vertical.get(c),
                    Some(CoverageStatus::Uncovered) | Some(CoverageStatus::Partial)
                )
            });
            if any_bad {
                CoverageStatus::Partial
            } else {
                CoverageStatus::Covered
            }
        } else {
            CoverageStatus::Covered
        };
        match status {
            CoverageStatus::Uncovered if item.waived_refine => CoverageStatus::Waived,
            other => other,
        }
    }

    fn resolve(&mut self, id: &str) -> ItemState {
        if let Some(state) = self.memo.get(id) {
            return *state;
        }
        #[cfg(test)]
        {
            self.memo_misses += 1;
        }
        self.in_progress.insert(id.to_string());

        // `saw_skipped` records whether at least one contributing element
        // (a right-side verifier's own run, or this item's own inline run)
        // was `skipped` — the spec excludes a skipped result from the max()
        // aggregation, but if that leaves *no* element at all the fallback
        // is `not_run`, not `uncovered` (wiki/220 §2.7: "他に要素がなければ
        // not_run").
        let mut elements: Vec<ItemState> = Vec::new();
        let mut saw_skipped = false;
        if let Some(verifiers) = self.verified_by.get(id) {
            for v in verifiers {
                if !self.in_scope(v) {
                    continue;
                }
                match own_run_state(self.runs_latest, v) {
                    Some(state) => elements.push(state),
                    None => saw_skipped = true,
                }
            }
        }
        let is_inline = self.items.get(id).is_some_and(|it| it.is_inline);
        if is_inline {
            match own_run_state(self.runs_latest, id) {
                Some(state) => elements.push(state),
                None => saw_skipped = true,
            }
        }
        let mut in_scope_children: Vec<String> = Vec::new();
        if let Some(children) = self.refines_children.get(id).cloned() {
            for child in &children {
                if !self.in_scope(child) {
                    // Out-of-use layer: n/a, not a state/vertical
                    // contributor (wiki/220 §2.1: "使用中でない層は対象外").
                    continue;
                }
                if self.in_progress.contains(child) {
                    if self.reported_cycles.insert(child.clone()) {
                        let layer = self.items.get(child).and_then(|it| it.layer.clone());
                        self.gaps.push(Gap {
                            kind: GapKind::Cycle,
                            item: Some(child.clone()),
                            layer,
                            detail: format!(
                                "refines cycle: '{child}' is already being resolved (reached again via '{id}')"
                            ),
                        });
                    }
                    continue;
                }
                elements.push(self.resolve(child));
                in_scope_children.push(child.clone());
            }
        }

        if self.in_scope(id) {
            let vertical_status = self.compute_vertical(id, &in_scope_children);
            self.vertical.insert(id.to_string(), vertical_status);
            if matches!(
                vertical_status,
                CoverageStatus::Uncovered | CoverageStatus::Partial
            ) {
                elements.push(ItemState::Uncovered);
            }
            if matches!(
                self.horizontal.get(id),
                Some(CoverageStatus::Uncovered) | Some(CoverageStatus::Partial)
            ) {
                elements.push(ItemState::Uncovered);
            }
        }

        let result = elements.into_iter().max().unwrap_or(if saw_skipped {
            ItemState::NotRun
        } else {
            ItemState::Uncovered
        });
        self.memo.insert(id.to_string(), result);
        self.in_progress.remove(id);
        result
    }
}

fn push_unverified_unrefined_orphan_gaps(
    items: &HashMap<String, ResolvedItem>,
    in_scope_items: &HashSet<String>,
    coverage_status: &HashMap<String, (CoverageStatus, CoverageStatus)>,
    effective_layers: &HashMap<String, HashSet<String>>,
    registry: &[RegisteredLayer],
    gaps: &mut Vec<Gap>,
) {
    let mut ids: Vec<&String> = items.keys().collect();
    ids.sort();
    for id in ids {
        let item = &items[id];
        if !in_scope_items.contains(id) {
            continue;
        }
        match item.side {
            Some(LayerSide::Left) => {
                let (horizontal, vertical) = coverage_status
                    .get(id)
                    .copied()
                    .unwrap_or((CoverageStatus::Na, CoverageStatus::Uncovered));
                if horizontal == CoverageStatus::Uncovered {
                    gaps.push(Gap {
                        kind: GapKind::Unverified,
                        item: Some(id.clone()),
                        layer: item.layer.clone(),
                        detail: "no valid verifier and not inline-verified".to_string(),
                    });
                }
                if vertical == CoverageStatus::Uncovered {
                    gaps.push(Gap {
                        kind: GapKind::Unrefined,
                        item: Some(id.clone()),
                        layer: item.layer.clone(),
                        detail: "no refining child and no implementing task".to_string(),
                    });
                }
                if !item.derived {
                    let level = item.level.expect("in_scope implies level resolved");
                    let upper_layer_in_use = registry.iter().any(|l| {
                        l.side == LayerSide::Left
                            && l.level < level
                            && effective_layers[id].contains(l.id.as_str())
                    });
                    if upper_layer_in_use && item.refines.is_empty() {
                        gaps.push(Gap {
                            kind: GapKind::Orphan,
                            item: Some(id.clone()),
                            layer: item.layer.clone(),
                            detail:
                                "an upper left-side layer is in use but this item has no refines"
                                    .to_string(),
                        });
                    }
                }
            }
            Some(LayerSide::Right) if item.verifies.is_empty() && !item.derived => {
                gaps.push(Gap {
                    kind: GapKind::Orphan,
                    item: Some(id.clone()),
                    layer: item.layer.clone(),
                    detail: "right-side item has no verifies target".to_string(),
                });
            }
            Some(LayerSide::Right) | None => {}
        }
    }
}

fn push_duplicate_id_gaps(
    stable_id_owners: &HashMap<String, Vec<String>>,
    items: &HashMap<String, ResolvedItem>,
    gaps: &mut Vec<Gap>,
) {
    let mut ids: Vec<&String> = stable_id_owners.keys().collect();
    ids.sort();
    for id in ids {
        let owners = &stable_id_owners[id];
        if owners.len() > 1 {
            gaps.push(Gap {
                kind: GapKind::DuplicateId,
                item: Some(id.clone()),
                layer: items.get(id).and_then(|it| it.layer.clone()),
                detail: format!(
                    "stable_id '{id}' is assigned in {} documents: {}",
                    owners.len(),
                    owners.join(", ")
                ),
            });
        }
    }
}

fn push_task_unlinked_gaps(
    input: &TraceInput,
    items: &HashMap<String, ResolvedItem>,
    gaps: &mut Vec<Gap>,
) {
    let mut linked_docs_by_task: HashMap<&str, HashSet<&str>> = HashMap::new();
    for link in &input.task_requirement_links {
        if let Some(item) = items.get(&link.stable_id) {
            linked_docs_by_task
                .entry(link.task_id.as_str())
                .or_default()
                .insert(item.doc_id.as_str());
        }
    }
    let mut doc_links: Vec<&super::types::TaskDocLink> = input.task_doc_links.iter().collect();
    doc_links.sort_by(|a, b| {
        (a.task_id.as_str(), a.doc_id.as_str()).cmp(&(b.task_id.as_str(), b.doc_id.as_str()))
    });
    for link in doc_links {
        if !input.layer_doc_ids.contains(&link.doc_id) {
            continue;
        }
        let has_item_link = linked_docs_by_task
            .get(link.task_id.as_str())
            .is_some_and(|docs| docs.contains(link.doc_id.as_str()));
        if !has_item_link {
            gaps.push(Gap {
                kind: GapKind::TaskUnlinked,
                item: Some(link.task_id.clone()),
                layer: None,
                detail: format!(
                    "task '{}' doc-links layer document '{}' but has no item-level requirement link into it",
                    link.task_id, link.doc_id
                ),
            });
        }
    }
}

fn aggregate_layer_coverage(
    items: &HashMap<String, ResolvedItem>,
    in_scope_items: &HashSet<String>,
    coverage_status: &HashMap<String, (CoverageStatus, CoverageStatus)>,
    states: &HashMap<String, ItemState>,
) -> HashMap<String, super::types::LayerCoverage> {
    let mut out: HashMap<String, super::types::LayerCoverage> = HashMap::new();
    for (id, item) in items {
        let Some(layer_id) = item.layer.as_deref() else {
            continue;
        };
        if !in_scope_items.contains(id) {
            continue;
        }
        let entry = out.entry(layer_id.to_string()).or_default();
        entry.total += 1;
        if let Some(state) = states.get(id) {
            entry.state.record(*state);
        }
        match item.side {
            Some(LayerSide::Left) => {
                if let Some((h, v)) = coverage_status.get(id) {
                    entry.horizontal.record(*h);
                    entry.vertical.record(*v);
                }
            }
            Some(LayerSide::Right) => {
                entry.horizontal.record(CoverageStatus::Na);
                entry.vertical.record(CoverageStatus::Na);
            }
            None => {}
        }
    }
    out
}

/// M2 §3.2 (M2-05): folds `suspects` into `coverage[layer].suspect` —
/// `suspect.item` (the child for `link`, the linked requirement item for
/// `task`, the item itself for `result`) resolves this suspect's layer.
/// Only counted when that layer already has a `coverage` entry (i.e. the
/// item is in its own effective scope, §2.1 規則 3 — the same rule every
/// other per-layer aggregate in this module already follows), never
/// silently creating a new layer entry.
fn aggregate_suspect_counts(
    items: &HashMap<String, ResolvedItem>,
    suspects: &[Suspect],
    coverage: &mut HashMap<String, super::types::LayerCoverage>,
) {
    let mut items_with_suspect: HashMap<&str, HashSet<&str>> = HashMap::new();
    for suspect in suspects {
        let Some(layer) = items.get(&suspect.item).and_then(|it| it.layer.as_deref()) else {
            continue;
        };
        let Some(entry) = coverage.get_mut(layer) else {
            continue;
        };
        match suspect.kind {
            super::types::SuspectKind::Link => entry.suspect.links += 1,
            super::types::SuspectKind::Task => entry.suspect.tasks += 1,
            super::types::SuspectKind::Result => entry.suspect.results += 1,
        }
        items_with_suspect
            .entry(layer)
            .or_default()
            .insert(suspect.item.as_str());
    }
    for (layer, item_ids) in items_with_suspect {
        if let Some(entry) = coverage.get_mut(layer) {
            entry.suspect.items = item_ids.len();
        }
    }
}

#[cfg(test)]
mod tests;
