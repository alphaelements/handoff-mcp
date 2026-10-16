//! JSON views of the requirement tree (wiki/300-requirement-tree-design.md,
//! SPEC-TREE-007): the per-item tree fields and the project-level `tree` /
//! `totals` / tree-gap blocks.
//!
//! `_trace_report.json` (schema v3), `handoff_trace_report(include_items)` and
//! the planned `handoff_trace_tree` all build their tree output from these
//! functions, over the same [`TreeIndex`] + [`Subtree`] roll-up the engine
//! computed — so every consumer sees the same numbers (NFR-005).

use serde::Serialize;
use serde_json::{json, Value};

use super::tree::{Subtree, TreeGapKind, TreeKind, TreeTotals};
use super::types::TraceItemInput;
use super::TraceGraph;

/// One item's tree fields in the report.
#[derive(Debug, Clone, Serialize)]
pub struct ItemTreeView<'a> {
    /// Direct, validated parents (a root has none).
    pub parents: &'a [String],
    /// The `- parent:` declaration verbatim, `None` when the item declares
    /// none. Differs from `parents` only for rejected declarations
    /// (dangling / cross-layer / cyclic parents).
    pub parent_declared: Option<&'a [String]>,
    /// Direct children, in the stable sibling order.
    pub children: &'a [String],
    pub kind: TreeKind,
    /// The item's own `- order:` key.
    pub order: Option<u32>,
    /// `None` only for an item with no layer, which takes no part in the
    /// tree.
    pub subtree: Option<&'a Subtree>,
}

/// `id`'s tree fields. `input` is the item's own input record (declared
/// parents, order); an item without a layer is a standalone leaf.
pub fn item_tree_view<'a>(
    graph: &'a TraceGraph,
    input: Option<&'a TraceItemInput>,
    id: &str,
) -> ItemTreeView<'a> {
    let tree = graph.tree();
    let declared = input
        .map(|i| i.tree.parents.as_slice())
        .filter(|p| !p.is_empty());
    ItemTreeView {
        parents: tree.parents(id),
        parent_declared: declared,
        children: tree.children(id),
        kind: graph.tree_kind(id).unwrap_or(TreeKind::Leaf),
        order: input.and_then(|i| i.tree.order),
        subtree: graph.subtree(id),
    }
}

/// The top-level `tree` block: the family-tree index.
pub fn tree_block(graph: &TraceGraph) -> Value {
    let totals = graph.tree_totals();
    json!({
        "roots": graph.tree().roots_by_layer,
        "max_depth": totals.max_depth,
        "composite_count": totals.composites,
        "decision_count": totals.decisions,
        "leaf_count": totals.leaves,
        "dag_shared_count": graph.tree().dag_shared_count(),
    })
}

/// The top-level `totals` block (de-duplicated across DAG-shared nodes).
pub fn totals(graph: &TraceGraph) -> &TreeTotals {
    graph.tree_totals()
}

/// The snake_case name of a tree gap kind, as it appears in `gaps[]` and
/// `gap_counts`.
pub fn tree_gap_kind_str(kind: TreeGapKind) -> &'static str {
    match kind {
        TreeGapKind::ParentDangling => "parent_dangling",
        TreeGapKind::ParentCrossLayer => "parent_cross_layer",
        TreeGapKind::TreeCycle => "tree_cycle",
        TreeGapKind::SelfParent => "self_parent",
        TreeGapKind::EmptyGroup => "empty_group",
        TreeGapKind::KindConflict => "kind_conflict",
    }
}

/// Parses a name produced by [`tree_gap_kind_str`].
pub fn parse_tree_gap_kind(name: &str) -> Option<TreeGapKind> {
    [
        TreeGapKind::ParentDangling,
        TreeGapKind::ParentCrossLayer,
        TreeGapKind::TreeCycle,
        TreeGapKind::SelfParent,
        TreeGapKind::EmptyGroup,
        TreeGapKind::KindConflict,
    ]
    .into_iter()
    .find(|k| tree_gap_kind_str(*k) == name)
}

/// The tree-structure findings shaped like `gaps[]` entries
/// (`{kind, item, layer?, detail}`).
pub fn tree_gaps_json(graph: &TraceGraph) -> Vec<Value> {
    graph
        .tree_gaps()
        .iter()
        .map(|g| {
            let mut entry = json!({
                "kind": tree_gap_kind_str(g.kind),
                "item": g.item,
                "detail": g.detail,
            });
            if let Some(layer) = graph.tree().layer(&g.item) {
                entry["layer"] = json!(layer);
            }
            entry
        })
        .collect()
}
