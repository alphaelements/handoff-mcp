//! JSON views of the requirement tree (wiki/300-requirement-tree-design.md,
//! SPEC-TREE-007): the per-item tree fields and the project-level `tree` /
//! `totals` / tree-gap blocks.
//!
//! `_trace_report.json` (schema v3), `handoff_trace_report(include_items)` and
//! the planned `handoff_trace_tree` all build their tree output from these
//! functions, over the same [`TreeIndex`] + [`Subtree`] roll-up the engine
//! computed — so every consumer sees the same numbers (NFR-005).

use std::collections::BTreeMap;

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
    /// `{branch parent -> label}` for the item's labelled edges to
    /// `decision` / `select` parents; omitted when there are none (never an
    /// empty map).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conditions: Option<&'a BTreeMap<String, String>>,
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
        conditions: tree.conditions(id).filter(|c| !c.is_empty()),
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
        "select_count": totals.selects,
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
        TreeGapKind::ConditionMissing => "condition_missing",
        TreeGapKind::ConditionDuplicate => "condition_duplicate",
        TreeGapKind::ConditionOnNonBranch => "condition_on_non_branch",
        TreeGapKind::AttrUnknownParent => "attr_unknown_parent",
        TreeGapKind::DecisionArity => "decision_arity",
        TreeGapKind::EmptyBranch => "empty_branch",
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
        TreeGapKind::ConditionMissing,
        TreeGapKind::ConditionDuplicate,
        TreeGapKind::ConditionOnNonBranch,
        TreeGapKind::AttrUnknownParent,
        TreeGapKind::DecisionArity,
        TreeGapKind::EmptyBranch,
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

/// Renders a `handoff_trace_tree` response as indented text (SPEC-TREE-009):
/// one line per node, two spaces of indent per depth,
/// `<ID>[*] [<label>] <title> (<kind>) [<passing>/<target leaves>] <verified %>`.
///
/// `[<label>]` is the branch label of the edge the node is listed under (the
/// node's `condition`); it is left out for roots and unlabelled edges.
///
/// `*` marks a node with several parents (a DAG-shared node). The target
/// leaves are the sub-tree's leaves minus the waived / n/a ones, i.e. the
/// denominator of `verified_ratio`; a node with no target leaf shows `-`
/// for the percentage. A walk over an explicit stack, so a deep tree cannot
/// overflow the call stack. Nodes lacking the roll-up (a `fields`
/// projection) render with `?` placeholders.
pub fn render_text(response: &Value) -> String {
    let mut out = String::new();
    let Some(roots) = response.get("roots").and_then(Value::as_array) else {
        return out;
    };
    let mut stack: Vec<(&Value, usize)> = roots.iter().rev().map(|n| (n, 0)).collect();
    while let Some((node, depth)) = stack.pop() {
        let text = |key: &str| node.get(key).and_then(Value::as_str).unwrap_or("");
        let shared = node
            .get("parents")
            .and_then(Value::as_array)
            .is_some_and(|p| p.len() > 1);
        out.push_str(&"  ".repeat(depth));
        out.push_str(text("id"));
        if shared {
            out.push('*');
        }
        let label = Some(text("condition")).filter(|l| !l.is_empty());
        if let Some(label) = label {
            out.push_str(&format!(" [{label}]"));
        }
        out.push_str(&format!(
            " {} ({}) {}\n",
            text("title"),
            text("kind"),
            text_progress(node.get("subtree"))
        ));
        if let Some(children) = node.get("children").and_then(Value::as_array) {
            stack.extend(children.iter().rev().map(|c| (c, depth + 1)));
        }
    }
    out
}

/// `[passing/target] percent` of one node's sub-tree.
fn text_progress(subtree: Option<&Value>) -> String {
    let count = |value: Option<&Value>| value.and_then(Value::as_u64);
    let Some(subtree) = subtree else {
        return "[?/?] ?".to_string();
    };
    let leaves = count(subtree.get("leaves"));
    let waived = count(subtree.pointer("/excluded/waived"));
    let na = count(subtree.pointer("/excluded/na"));
    let passing = count(subtree.pointer("/state/passing"));
    let (Some(leaves), Some(waived), Some(na), Some(passing)) = (leaves, waived, na, passing)
    else {
        return "[?/?] ?".to_string();
    };
    let target = leaves.saturating_sub(waived + na);
    let percent = subtree
        .pointer("/progress/verified_ratio")
        .and_then(Value::as_f64)
        .map_or_else(|| "-".to_string(), |ratio| format!("{:.1}%", ratio * 100.0));
    format!("[{passing}/{target}] {percent}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, parents: &[&str], children: Value) -> Value {
        json!({
            "id": id, "title": format!("T {id}"), "kind": "leaf", "parents": parents,
            "subtree": {
                "leaves": 3, "excluded": {"waived": 1, "na": 0},
                "state": {"passing": 1}, "progress": {"verified_ratio": 0.5},
            },
            "children": children,
        })
    }

    #[test]
    fn text_has_one_indented_line_per_node_and_stars_shared_nodes() {
        let response = json!({"roots": [
            node("A", &[], json!([
                node("B", &["A"], json!([node("D", &["B", "C"], json!([]))])),
                node("C", &["A"], json!(null)),
            ])),
        ]});
        assert_eq!(
            render_text(&response),
            "A T A (leaf) [1/2] 50.0%\n\
             \x20 B T B (leaf) [1/2] 50.0%\n\
             \x20   D* T D (leaf) [1/2] 50.0%\n\
             \x20 C T C (leaf) [1/2] 50.0%\n"
        );
    }

    #[test]
    fn text_shows_the_branch_label_of_the_edge_a_node_is_emitted_under() {
        let mut yes = node("D", &["A", "B"], json!([]));
        yes["condition"] = json!("yes");
        let mut case = node("D", &["A", "B"], json!(null));
        case["condition"] = json!("case:2");
        let mut empty_label = node("E", &["A"], json!([]));
        empty_label["condition"] = json!("");
        let response = json!({"roots": [
            node("A", &[], json!([yes, empty_label])),
            node("B", &[], json!([case])),
        ]});
        assert_eq!(
            render_text(&response),
            "A T A (leaf) [1/2] 50.0%\n\
             \x20 D* [yes] T D (leaf) [1/2] 50.0%\n\
             \x20 E T E (leaf) [1/2] 50.0%\n\
             B T B (leaf) [1/2] 50.0%\n\
             \x20 D* [case:2] T D (leaf) [1/2] 50.0%\n"
        );
    }

    #[test]
    fn text_shows_a_dash_without_a_ratio_and_placeholders_without_a_roll_up() {
        let mut empty = node("A", &[], json!([]));
        empty["subtree"]["progress"]["verified_ratio"] = json!(null);
        let bare = json!({"id": "B", "title": "b", "kind": "leaf", "parents": [], "children": []});
        let response = json!({"roots": [empty, bare]});
        assert_eq!(
            render_text(&response),
            "A T A (leaf) [1/2] -\nB b (leaf) [?/?] ?\n"
        );
    }

    #[test]
    fn text_of_a_response_without_roots_is_empty() {
        assert_eq!(render_text(&json!({"roots": []})), "");
        assert_eq!(render_text(&json!({})), "");
    }

    #[test]
    fn text_of_a_deep_chain_does_not_recurse() {
        let mut chain = node("N0", &[], json!([]));
        // Built flat then linked iteratively would still nest the Value, so
        // keep this depth below serde_json's own drop recursion comfort zone.
        for n in 1..500 {
            chain = node(&format!("N{n}"), &[], json!([chain]));
        }
        let text = render_text(&json!({"roots": [chain]}));
        assert_eq!(text.lines().count(), 500);
    }
}
