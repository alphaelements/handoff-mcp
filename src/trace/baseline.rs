//! Baseline-to-baseline (or baseline-to-current) diff (wiki/270-vmodel-m3-
//! design.md §2.4/§4.1, M3-07, FR-405 diff part): a pure function over two
//! already-loaded snapshots — no file I/O, no `TraceGraph` rebuild. The I/O
//! side (resolving `from`/`to` to a [`BaselineSnapshot`], reading
//! `baselines/<id>.json` or `_trace_report.json`) lives in
//! `crate::mcp::handlers::trace_baseline`; this module only compares two
//! snapshots already in memory, keeping it as cheap as PR-4 (≤100ms, §4.1)
//! promises — a diff never does more work than parsing the two JSON files
//! its caller already read.
//!
//! **`items[]` shape**: both a `baselines/<id>.json` record's own
//! `items[]` and a `_trace_report.json`'s `items[]` (when `to`/`from` is
//! `"current"`) carry an `id`/`def_hash` pair at minimum — exactly the two
//! fields this module reads off each item ([`lightweight_item`] in
//! `trace_baseline.rs` already narrows a full report item down to the
//! baseline shape before either snapshot reaches here, so this module never
//! needs to know about the extra fields `_trace_report.json`'s items carry
//! that a baseline's don't).
//!
//! **E20 (coverage regression)**: a layer's `horizontal`/`vertical` axis
//! counts as regressed when its `covered` *ratio* (not raw count) drops
//! between `from` and `to` — ratio-based so adding/removing uncovered items
//! elsewhere doesn't itself look like a regression (§2.4's own rationale:
//! "割合ベースの比較は項目数の増減に対して安定する"). An axis with zero
//! items in `to` is never reported (nothing to regress into); an axis that
//! only exists in `from` (the whole layer disappeared from `to`) is also
//! left alone here — `removed`/`state_changes` already surface a
//! layer/item vanishing, and E20 is specifically about *coverage ratio*
//! movement, not presence.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;
use serde_json::Value;

/// One `items[]` entry's minimal shape this module reads — `id` to key the
/// added/removed/changed comparison, `def_hash` for `changed`. Both a
/// baseline record's own lightweight item and a full `_trace_report.json`
/// item carry these two fields under the same names (see module doc), so a
/// caller need not re-shape either one before calling [`diff_snapshots`].
#[derive(Debug, Clone, Default)]
pub struct BaselineItemKey {
    pub id: String,
    pub def_hash: Option<String>,
}

/// Everything [`diff_snapshots`] needs from one side (`from` or `to`) of a
/// baseline diff — gathered by the caller from either a
/// `baselines/<id>.json` record or (when that side is `"current"`) a
/// freshly-extracted `_trace_report.json` snapshot, using the exact same
/// `coverage_summary`/`state_summary` shapes [`crate::storage::baselines::
/// BaselineRecord`] stores (`graph.coverage()` / tallied item states).
#[derive(Debug, Clone, Default)]
pub struct BaselineSnapshot {
    pub items: Vec<BaselineItemKey>,
    /// `{<layer>: {horizontal: {covered,partial,uncovered,waived,na}, vertical: {...}, ...}}`
    /// — same shape `graph.coverage()` serializes to (`src/trace/types.rs`'s
    /// `LayerCoverage`). Kept as `Value` rather than re-typed: a diff only
    /// ever reads `<layer>.<axis>.covered` plus a per-axis total, and a
    /// stored baseline's `coverage_summary` is already this exact shape
    /// on disk — re-typing it here would mean deserializing into a second
    /// struct just to re-flatten straight back into ratio arithmetic.
    pub coverage_summary: Value,
    /// `{passing, failing, blocked, not_run, uncovered}` — same shape
    /// `BaselineRecord::state_summary` stores.
    pub state_summary: Value,
}

/// One `changed[]` entry (§4.1): an item present on both sides whose
/// `def_hash` differs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangedItem {
    pub id: String,
    pub old_hash: Option<String>,
    pub new_hash: Option<String>,
}

/// One `regression[]` entry (§4.1/E20): a `(layer, axis)` pair whose
/// `covered` ratio dropped from `from` to `to`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Regression {
    pub layer: String,
    pub axis: String,
    pub old_pct: f64,
    pub new_pct: f64,
}

/// `trace_baseline(action="diff")`'s full output (§4.1), minus `warnings`
/// (those depend on I/O — e.g. "baseline not found" — which is the caller's
/// concern, not this pure function's).
#[derive(Debug, Clone, Default, Serialize)]
pub struct BaselineDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<ChangedItem>,
    pub regression: Vec<Regression>,
    /// `{<state>: delta}` — only states whose count actually moved are
    /// included (§4.1's worked example: `{"passing": 2, "failing": -1}`).
    pub state_changes: BTreeMap<String, i64>,
}

/// `covered` ratio for one `{covered,partial,uncovered,waived,na}` axis
/// object — `covered / (covered+partial+uncovered+waived+na)`, `None` when
/// the axis has zero items (nothing to compute a ratio over, and §4.1 never
/// reports a `None` ratio as a regression — see module doc).
fn covered_ratio(axis: &Value) -> Option<f64> {
    let get = |key: &str| axis.get(key).and_then(Value::as_u64).unwrap_or(0);
    let covered = get("covered");
    let total = covered + get("partial") + get("uncovered") + get("waived") + get("na");
    if total == 0 {
        None
    } else {
        Some(covered as f64 / total as f64)
    }
}

/// Every `(layer, axis)` pair present in `summary` (§4.1 axes: `horizontal`,
/// `vertical`).
fn layer_axis_pairs(summary: &Value) -> Vec<(String, &'static str)> {
    const AXES: [&str; 2] = ["horizontal", "vertical"];
    let Some(obj) = summary.as_object() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (layer, layer_val) in obj {
        for axis in AXES {
            if layer_val.get(axis).is_some() {
                out.push((layer.clone(), axis));
            }
        }
    }
    out
}

fn compute_regressions(from: &Value, to: &Value) -> Vec<Regression> {
    let mut out = Vec::new();
    for (layer, axis) in layer_axis_pairs(to) {
        let new_axis = &to[&layer][axis];
        let Some(new_pct) = covered_ratio(new_axis) else {
            continue;
        };
        let old_pct = from
            .get(&layer)
            .and_then(|l| l.get(axis))
            .and_then(covered_ratio);
        if let Some(old_pct) = old_pct {
            if new_pct < old_pct {
                out.push(Regression {
                    layer: layer.clone(),
                    axis: axis.to_string(),
                    old_pct,
                    new_pct,
                });
            }
        }
    }
    out.sort_by(|a, b| a.layer.cmp(&b.layer).then(a.axis.cmp(&b.axis)));
    out
}

const STATE_KEYS: [&str; 5] = ["passing", "failing", "blocked", "not_run", "uncovered"];

fn compute_state_changes(from: &Value, to: &Value) -> BTreeMap<String, i64> {
    let mut out = BTreeMap::new();
    for key in STATE_KEYS {
        let old = from.get(key).and_then(Value::as_i64).unwrap_or(0);
        let new = to.get(key).and_then(Value::as_i64).unwrap_or(0);
        let delta = new - old;
        if delta != 0 {
            out.insert(key.to_string(), delta);
        }
    }
    out
}

/// Diffs `from` against `to` (§4.1: `added`/`removed`/`changed` are always
/// relative to `to` being the "newer" side — an item only in `to` is
/// `added`, an item only in `from` is `removed`).
pub fn diff_snapshots(from: &BaselineSnapshot, to: &BaselineSnapshot) -> BaselineDiff {
    let from_by_id: HashMap<&str, &BaselineItemKey> =
        from.items.iter().map(|i| (i.id.as_str(), i)).collect();
    let to_by_id: HashMap<&str, &BaselineItemKey> =
        to.items.iter().map(|i| (i.id.as_str(), i)).collect();

    let mut added: Vec<String> = to_by_id
        .keys()
        .filter(|id| !from_by_id.contains_key(*id))
        .map(|id| id.to_string())
        .collect();
    added.sort();

    let mut removed: Vec<String> = from_by_id
        .keys()
        .filter(|id| !to_by_id.contains_key(*id))
        .map(|id| id.to_string())
        .collect();
    removed.sort();

    let mut changed: Vec<ChangedItem> = from_by_id
        .iter()
        .filter_map(|(id, from_item)| {
            let to_item = to_by_id.get(id)?;
            if from_item.def_hash != to_item.def_hash {
                Some(ChangedItem {
                    id: id.to_string(),
                    old_hash: from_item.def_hash.clone(),
                    new_hash: to_item.def_hash.clone(),
                })
            } else {
                None
            }
        })
        .collect();
    changed.sort_by(|a, b| a.id.cmp(&b.id));

    let regression = compute_regressions(&from.coverage_summary, &to.coverage_summary);
    let state_changes = compute_state_changes(&from.state_summary, &to.state_summary);

    BaselineDiff {
        added,
        removed,
        changed,
        regression,
        state_changes,
    }
}

#[cfg(test)]
mod tests;
