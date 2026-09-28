//! M1 trace derivation engine (wiki/220-vmodel-integration-design.md §2.7,
//! t360.9): a **pure function**, `TraceGraph::build`, over [`TraceInput`].
//! No file I/O — wiring this into `handoff_trace_report`/`handoff_trace_slice`
//! and the `_trace_report.json` derived file is t360.10/11's scope.
//!
//! Builds the graph exactly once (wiki/240-performance-design.md §5-5) and
//! computes every left-side item's `state` via a memoized DP over the
//! `refines` DAG (`TraceGraph::build`'s `Dp` helper) — each item's state is
//! computed at most once regardless of how many ancestors query it, and a
//! `refines` back-edge (cycle) is detected via an in-progress set and
//! reported as a `cycle` gap rather than recursing forever.

use std::collections::{HashMap, HashSet};

use crate::storage::docs::layer::{LayerSide, RegisteredLayer};

use super::types::{
    CoverageStatus, Gap, GapKind, InUseLayers, ItemState, LayersSource, TaskLinkRole, TraceInput,
    TraceItemInput,
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
        Self {
            doc_id: raw.doc_id.clone(),
            layer: raw.layer.clone(),
            side,
            level: def.map(|d| d.level),
            refines: raw.refines.clone(),
            verifies: raw.verifies.clone(),
            is_inline,
        }
    }

    fn in_scope(&self, in_use: &HashSet<String>) -> bool {
        self.layer
            .as_deref()
            .is_some_and(|l| self.side.is_some() && in_use.contains(l))
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
    /// verifier -> valid left-side targets it verifies.
    verifies_targets: HashMap<String, Vec<String>>,
    /// left-side target -> valid verifiers.
    verified_by: HashMap<String, Vec<String>>,
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
        let (verifies_targets, verified_by) = build_verifies_edges(&items, &mut gaps);

        let task_implements = task_implements_set(&input.task_requirement_links);

        let left_levels_in_use = left_levels_in_use(&in_use_set, &input.layer_registry);

        let mut states: HashMap<String, ItemState> = HashMap::new();
        // Right-side items are leaves: resolve directly, no recursion.
        for (id, item) in &items {
            if item.side == Some(LayerSide::Right) {
                states.insert(id.clone(), verifier_state(&input.runs_latest, id));
            }
        }

        let coverage_status = precompute_coverage(
            &items,
            &in_use_set,
            &verified_by,
            &refines_children,
            &task_implements,
            &left_levels_in_use,
            &input.layer_registry,
        );

        let mut dp = Dp {
            items: &items,
            refines_children: &refines_children,
            verified_by: &verified_by,
            runs_latest: &input.runs_latest,
            coverage_status: &coverage_status,
            in_use: &in_use_set,
            memo: &mut states,
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

        push_unverified_unrefined_orphan_gaps(
            &items,
            &in_use_set,
            &coverage_status,
            &left_levels_in_use,
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

        let coverage = aggregate_layer_coverage(&items, &in_use_set, &coverage_status, &states);

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
/// from `items`, M2-01). The profile tier here is project-wide only — a
/// per-document `trace_profile` override and its tree-inheritance onto
/// descendant items is M2-03's scope (§2.1 規則 1-4).
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

fn left_levels_in_use(in_use: &HashSet<String>, registry: &[RegisteredLayer]) -> HashSet<u8> {
    registry
        .iter()
        .filter(|l| l.side == LayerSide::Left && in_use.contains(l.id.as_str()))
        .map(|l| l.level)
        .collect()
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
/// `precompute_coverage`, `Dp::resolve`). They are project-wide data-quality
/// diagnostics: a broken reference or a structurally illegal link is wrong
/// regardless of whether the current `[trace] layers` view happens to
/// include that layer today, and hiding it behind scope would let a real
/// authoring mistake resurface silently the moment scope changes.
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

fn build_verifies_edges(
    items: &HashMap<String, ResolvedItem>,
    gaps: &mut Vec<Gap>,
) -> (HashMap<String, Vec<String>>, HashMap<String, Vec<String>>) {
    let mut targets_of: HashMap<String, Vec<String>> = HashMap::new();
    let mut verified_by: HashMap<String, Vec<String>> = HashMap::new();
    let mut ids: Vec<&String> = items.keys().collect();
    ids.sort();
    for id in ids {
        let verifier = &items[id];
        for target_id in &verifier.verifies {
            match items.get(target_id) {
                None => gaps.push(Gap {
                    kind: GapKind::Dangling,
                    item: Some(id.clone()),
                    layer: verifier.layer.clone(),
                    detail: format!("verifies target '{target_id}' does not resolve to any item"),
                }),
                Some(target) => {
                    if verifies_edge_valid(verifier, target) {
                        targets_of
                            .entry(id.clone())
                            .or_default()
                            .push(target_id.clone());
                        verified_by
                            .entry(target_id.clone())
                            .or_default()
                            .push(id.clone());
                    } else {
                        gaps.push(Gap {
                            kind: GapKind::InvalidLink,
                            item: Some(id.clone()),
                            layer: verifier.layer.clone(),
                            detail: format!(
                                "verifies target '{target_id}' is not a left-side item at or below this verifier's level"
                            ),
                        });
                    }
                }
            }
        }
    }
    (targets_of, verified_by)
}

fn task_implements_set(links: &[super::types::TaskRequirementLink]) -> HashSet<String> {
    links
        .iter()
        .filter(|l| l.role == TaskLinkRole::Implements)
        .map(|l| l.stable_id.clone())
        .collect()
}

/// (horizontal, vertical) coverage status per left-side, in-use item —
/// computed once, ahead of the state DP (the DP needs it as an element
/// input) and reused again for the `unverified`/`unrefined` gaps.
fn precompute_coverage(
    items: &HashMap<String, ResolvedItem>,
    in_use: &HashSet<String>,
    verified_by: &HashMap<String, Vec<String>>,
    refines_children: &HashMap<String, Vec<String>>,
    task_implements: &HashSet<String>,
    left_levels_in_use: &HashSet<u8>,
    registry: &[RegisteredLayer],
) -> HashMap<String, (CoverageStatus, CoverageStatus)> {
    let mut out = HashMap::new();
    for (id, item) in items {
        if item.side != Some(LayerSide::Left) || !item.in_scope(in_use) {
            continue;
        }
        let layer_id = item.layer.as_deref().expect("in_scope implies layer set");
        let def = registry
            .iter()
            .find(|r| r.id == layer_id)
            .expect("in_scope implies a registered layer");

        // A verifier/child living in a layer that isn't in use must not
        // count toward coverage either — same "使用中でない層は対象外"
        // policy the state DP applies (wiki/220 §2.1), kept consistent here
        // so coverage and state never disagree about the same item.
        let has_verifier = verified_by.get(id).is_some_and(|vs| {
            vs.iter()
                .any(|v| items.get(v).is_some_and(|it| it.in_scope(in_use)))
        });
        let horizontal = if has_verifier || item.is_inline {
            CoverageStatus::Covered
        } else if in_use.contains(&def.pair) {
            CoverageStatus::Uncovered
        } else {
            CoverageStatus::Na
        };

        let level = def.level;
        let deeper_layer_in_use = left_levels_in_use.iter().any(|&l| l > level);
        let has_child = refines_children.get(id).is_some_and(|cs| {
            cs.iter()
                .any(|c| items.get(c).is_some_and(|it| it.in_scope(in_use)))
        });
        let has_impl_task = task_implements.contains(id);
        let vertical_covered = if deeper_layer_in_use {
            has_child || has_impl_task
        } else {
            has_impl_task
        };
        let vertical = if vertical_covered {
            CoverageStatus::Covered
        } else {
            CoverageStatus::Uncovered
        };

        out.insert(id.clone(), (horizontal, vertical));
    }
    out
}

/// Memoized DP over the `refines` DAG (wiki/240 §5-5): each left-side item's
/// state is resolved at most once via `resolve`, with `in_progress` acting
/// as the "訪問済み集合" that turns a `refines` back-edge into a `cycle` gap
/// instead of infinite recursion.
struct Dp<'a> {
    items: &'a HashMap<String, ResolvedItem>,
    refines_children: &'a HashMap<String, Vec<String>>,
    verified_by: &'a HashMap<String, Vec<String>>,
    runs_latest: &'a HashMap<String, String>,
    coverage_status: &'a HashMap<String, (CoverageStatus, CoverageStatus)>,
    /// Layers currently in use (wiki/220 §2.1) — a verifier or a refines
    /// child living in a layer that is *not* in use must not feed this
    /// item's state (it is n/a, not a silent uncovered/not_run pull-down).
    in_use: &'a HashSet<String>,
    memo: &'a mut HashMap<String, ItemState>,
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
        self.items
            .get(id)
            .is_some_and(|it| it.in_scope(self.in_use))
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
        if let Some(children) = self.refines_children.get(id).cloned() {
            for child in &children {
                if !self.in_scope(child) {
                    // Out-of-use layer: n/a, not a state contributor
                    // (wiki/220 §2.1: "使用中でない層は対象外").
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
            }
        }
        if let Some((horizontal, vertical)) = self.coverage_status.get(id) {
            if *horizontal == CoverageStatus::Uncovered {
                elements.push(ItemState::Uncovered);
            }
            if *vertical == CoverageStatus::Uncovered {
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
    in_use: &HashSet<String>,
    coverage_status: &HashMap<String, (CoverageStatus, CoverageStatus)>,
    left_levels_in_use: &HashSet<u8>,
    gaps: &mut Vec<Gap>,
) {
    let mut ids: Vec<&String> = items.keys().collect();
    ids.sort();
    for id in ids {
        let item = &items[id];
        if !item.in_scope(in_use) {
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
                let level = item.level.expect("in_scope implies level resolved");
                let upper_layer_in_use = left_levels_in_use.iter().any(|&l| l < level);
                if upper_layer_in_use && item.refines.is_empty() {
                    gaps.push(Gap {
                        kind: GapKind::Orphan,
                        item: Some(id.clone()),
                        layer: item.layer.clone(),
                        detail: "an upper left-side layer is in use but this item has no refines"
                            .to_string(),
                    });
                }
            }
            Some(LayerSide::Right) if item.verifies.is_empty() => {
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
    in_use: &HashSet<String>,
    coverage_status: &HashMap<String, (CoverageStatus, CoverageStatus)>,
    states: &HashMap<String, ItemState>,
) -> HashMap<String, super::types::LayerCoverage> {
    let mut out: HashMap<String, super::types::LayerCoverage> = HashMap::new();
    for (id, item) in items {
        let Some(layer_id) = item.layer.as_deref() else {
            continue;
        };
        if !in_use.contains(layer_id) || item.side.is_none() {
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

#[cfg(test)]
mod tests;
