//! Suspect derivation (wiki/260-vmodel-m2-design.md §3.2/§4.1, M2-05): the 3
//! suspect kinds (`link`/`task`/`result`), the `unbaselined` links/tasks that
//! never got a suspect check in the first place (no baseline recorded yet),
//! and the `reverify` item set — a pure function over an already-loaded
//! [`TraceInput`] plus the `state` map [`super::engine::TraceGraph::build`]
//! already computed, called once per graph build (wiki/240 §5-5's "1
//! リクエスト内でグラフを1回だけ構築する", extended here to "suspect もその
//! ついでに求める", §3.2's own wording).
//!
//! Deliberately does **not** depend on [`super::engine`]'s private
//! `ResolvedItem` — every field this module needs (`def_hash`, `body_hash`,
//! `link_baselines`, `refines`/`verifies`) is already present, unresolved,
//! on [`TraceItemInput`] itself (unlike `layer`/`side`/`level`, which do need
//! registry resolution). This keeps the module a standalone, independently
//! testable pure function, and keeps `engine.rs` from having to expose its
//! internal item representation just for this.

use std::collections::{HashMap, HashSet};

use super::types::{
    ItemState, RunResultHashes, Suspect, SuspectKind, TaskRequirementLink, TraceInput,
    TraceItemInput, UnbaselinedCounts, UnbaselinedLink, UnbaselinedTask,
};

/// Everything [`compute`] returns, gathered once per graph build.
#[derive(Debug, Clone, Default)]
pub struct SuspectDerivation {
    /// Sorted deterministically (kind, item, upstream/task) — never depends
    /// on `TraceInput.items`'/`task_requirement_links`' own (filesystem-
    /// dependent) iteration order (mirrors `TraceGraph::build`'s own `gaps`
    /// sort, wiki/240 §5-5/NFR-004).
    pub suspects: Vec<Suspect>,
    pub unbaselined: UnbaselinedCounts,
    pub unbaselined_links: Vec<UnbaselinedLink>,
    pub unbaselined_tasks: Vec<UnbaselinedTask>,
    /// stable_ids of `state == Passing` verification items whose own result
    /// is suspect, or one of whose `verifies` links is suspect (§3.2's
    /// "再検証要 reverify" — FR-402).
    pub reverify: HashSet<String>,
}

/// Splits a `refines`/`verifies` entry into its edge target and, when
/// present, the acceptance-criteria sub-reference (mirrors
/// `super::engine::split_ac_ref` — duplicated rather than shared across a
/// `pub(super)` boundary, since it is 3 lines and this module must not
/// depend on `engine`'s internals, see this module's doc comment).
fn split_ref(raw: &str) -> (&str, Option<&str>) {
    match raw.split_once('#') {
        Some((base, ac)) if !ac.is_empty() => (base, Some(ac)),
        _ => (raw, None),
    }
}

/// Resolves upstream reference `raw_ref`'s *current* hash (wiki/260 §2.4:
/// `X` -> `def_hash`, `X#ACn` -> `ac_hash(X, ACn)`). An AC-level reference
/// resolves via the implicit acceptance-verification item `"X#ACn"`'s own
/// `def_hash`, which mirrors `ac_hash(X, ACn)` exactly by construction
/// (§2.4/§2.5 step 4) — this module is a pure function with no body text to
/// re-parse (D1), so an AC-level reference with no such implicit item in
/// `by_id` (either the profile has `implicit_acceptance` disabled, or the
/// referenced item was never synced) returns `None`: "can't determine",
/// never a guess. `None` also covers a dangling base (target doesn't
/// resolve to any item at all).
fn resolve_current_upstream_hash(
    raw_ref: &str,
    by_id: &HashMap<&str, &TraceItemInput>,
) -> Option<String> {
    let (base, ac_label) = split_ref(raw_ref);
    let base_item = by_id.get(base)?;
    match ac_label {
        None => base_item.def_hash.clone(),
        Some(label) => {
            let implicit_id = format!("{base}#{label}");
            by_id.get(implicit_id.as_str())?.def_hash.clone()
        }
    }
}

/// First-occurrence-wins index of `items` by `stable_id` (mirrors
/// `engine::resolve_items`'s own tie-break — a `duplicate_id` gap, not this
/// module's concern, is what surfaces the collision).
fn index_items(items: &[TraceItemInput]) -> HashMap<&str, &TraceItemInput> {
    let mut out = HashMap::with_capacity(items.len());
    for item in items {
        out.entry(item.stable_id.as_str()).or_insert(item);
    }
    out
}

fn link_suspects_and_unbaselined(
    by_id: &HashMap<&str, &TraceItemInput>,
    suspects: &mut Vec<Suspect>,
    unbaselined_links: &mut Vec<UnbaselinedLink>,
) {
    let mut ids: Vec<&str> = by_id.keys().copied().collect();
    ids.sort_unstable();
    for id in ids {
        let it = by_id[id];
        let refs = it
            .refines
            .iter()
            .map(|r| (r, "refines"))
            .chain(it.verifies.iter().map(|r| (r, "verifies")));
        for (raw_ref, link_type) in refs {
            // A dangling reference (base target doesn't resolve to any item)
            // is already reported as a `dangling` gap elsewhere — not this
            // module's concern, and it can never have a meaningful baseline
            // or current hash, so it is skipped entirely here (neither
            // suspect nor unbaselined).
            let (base, _) = split_ref(raw_ref);
            if !by_id.contains_key(base) {
                continue;
            }
            match it.link_baselines.get(raw_ref) {
                None => unbaselined_links.push(UnbaselinedLink {
                    item: id.to_string(),
                    upstream: raw_ref.clone(),
                    link_type,
                    current_hash: resolve_current_upstream_hash(raw_ref, by_id),
                }),
                Some(baseline) => {
                    if let Some(current) = resolve_current_upstream_hash(raw_ref, by_id) {
                        if &current != baseline {
                            suspects.push(Suspect {
                                kind: SuspectKind::Link,
                                item: id.to_string(),
                                upstream: Some(raw_ref.clone()),
                                task: None,
                                link_type: Some(link_type.to_string()),
                                baseline_hash: baseline.clone(),
                                current_hash: current,
                            });
                        }
                    }
                    // `current == None`: upstream (or its AC) can't be
                    // resolved right now (e.g. `implicit_acceptance` off) —
                    // "can't determine", not a suspect, per this function's
                    // doc comment.
                }
            }
        }
    }
}

fn task_suspects_and_unbaselined(
    by_id: &HashMap<&str, &TraceItemInput>,
    links: &[TaskRequirementLink],
    suspects: &mut Vec<Suspect>,
    unbaselined_tasks: &mut Vec<UnbaselinedTask>,
) {
    for link in links {
        let Some(item) = by_id.get(link.stable_id.as_str()) else {
            // Task links to an unresolvable stable_id: nothing to compare
            // against (a `duplicate_id`/dangling data-quality concern lives
            // elsewhere, not here).
            continue;
        };
        match &link.baseline_hash {
            None => unbaselined_tasks.push(UnbaselinedTask {
                task_id: link.task_id.clone(),
                item: link.stable_id.clone(),
                current_hash: item.def_hash.clone(),
            }),
            Some(baseline) => {
                if let Some(current) = &item.def_hash {
                    if current != baseline {
                        suspects.push(Suspect {
                            kind: SuspectKind::Task,
                            item: link.stable_id.clone(),
                            upstream: None,
                            task: Some(link.task_id.clone()),
                            link_type: None,
                            baseline_hash: baseline.clone(),
                            current_hash: current.clone(),
                        });
                    }
                }
            }
        }
    }
}

fn result_suspects(
    by_id: &HashMap<&str, &TraceItemInput>,
    runs_latest: &HashMap<String, String>,
    runs_latest_hashes: &HashMap<String, RunResultHashes>,
    suspects: &mut Vec<Suspect>,
) {
    let mut ids: Vec<&String> = runs_latest.keys().collect();
    ids.sort_unstable();
    for id in ids {
        if runs_latest.get(id).map(String::as_str) != Some("pass") {
            continue;
        }
        let Some(item) = by_id.get(id.as_str()) else {
            continue;
        };
        let recorded = runs_latest_hashes.get(id);
        // §4.11/E13: prefer `def_hash` when the recorded entry has one, else
        // fall back to `body_hash` (a pre-M2-02 run never had a `def_hash`
        // at all).
        let (baseline, current) = match recorded.and_then(|r| r.def_hash.clone()) {
            Some(recorded_def) => (Some(recorded_def), item.def_hash.clone()),
            None => match recorded.and_then(|r| r.body_hash.clone()) {
                Some(recorded_body) => (Some(recorded_body), item.body_hash.clone()),
                None => (None, None),
            },
        };
        if let (Some(baseline), Some(current)) = (baseline, current) {
            if baseline != current {
                suspects.push(Suspect {
                    kind: SuspectKind::Result,
                    item: (*id).clone(),
                    upstream: None,
                    task: None,
                    link_type: None,
                    baseline_hash: baseline,
                    current_hash: current,
                });
            }
        }
    }
}

fn suspect_sort_key(s: &Suspect) -> (u8, &str, &str, &str) {
    let kind = match s.kind {
        SuspectKind::Link => 0,
        SuspectKind::Task => 1,
        SuspectKind::Result => 2,
    };
    (
        kind,
        s.item.as_str(),
        s.upstream.as_deref().unwrap_or(""),
        s.task.as_deref().unwrap_or(""),
    )
}

/// Computes every suspect/unbaselined/reverify entry for one graph build
/// (wiki/260 §3.2). `states` is `TraceGraph::build`'s own memoized-DP output
/// (only `Passing` items are eligible for `reverify`, FR-402).
pub fn compute(input: &TraceInput, states: &HashMap<String, ItemState>) -> SuspectDerivation {
    let by_id = index_items(&input.items);

    let mut suspects = Vec::new();
    let mut unbaselined_links = Vec::new();
    let mut unbaselined_tasks = Vec::new();

    link_suspects_and_unbaselined(&by_id, &mut suspects, &mut unbaselined_links);
    task_suspects_and_unbaselined(
        &by_id,
        &input.task_requirement_links,
        &mut suspects,
        &mut unbaselined_tasks,
    );
    result_suspects(
        &by_id,
        &input.runs_latest,
        &input.runs_latest_hashes,
        &mut suspects,
    );

    suspects.sort_by(|a, b| suspect_sort_key(a).cmp(&suspect_sort_key(b)));

    let result_suspect_items: HashSet<&str> = suspects
        .iter()
        .filter(|s| s.kind == SuspectKind::Result)
        .map(|s| s.item.as_str())
        .collect();
    let verifies_link_suspect_items: HashSet<&str> = suspects
        .iter()
        .filter(|s| s.kind == SuspectKind::Link && s.link_type.as_deref() == Some("verifies"))
        .map(|s| s.item.as_str())
        .collect();

    let reverify: HashSet<String> = states
        .iter()
        .filter(|(id, state)| {
            **state == ItemState::Passing
                && (result_suspect_items.contains(id.as_str())
                    || verifies_link_suspect_items.contains(id.as_str()))
        })
        .map(|(id, _)| id.clone())
        .collect();

    let unbaselined = UnbaselinedCounts {
        links: unbaselined_links.len(),
        tasks: unbaselined_tasks.len(),
    };

    SuspectDerivation {
        suspects,
        unbaselined,
        unbaselined_links,
        unbaselined_tasks,
        reverify,
    }
}

#[cfg(test)]
mod tests;
