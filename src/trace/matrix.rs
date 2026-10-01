//! Pure trace-matrix table construction (wiki/260-vmodel-m2-design.md §4.4,
//! t360.20.9/M2-09): `handoff_trace_matrix`'s domain logic, split into a
//! "tree" view (wiki/230-vmodel-ui-feature-inventory.md INV-614's "トレース表
//! （行 = V字1本）": one row per top-level left-side item, with its
//! reachable descendants bucketed into the project's in-use layer columns)
//! and an "edges" view (one row per resolved `refines`/`verifies` link, for
//! import into an external tool). Every function here is a pure computation
//! over an already-built [`TraceGraph`]/already-loaded item data — no file
//! I/O, no argument parsing. The handler
//! (`src/mcp/handlers/trace_matrix.rs`) owns E6's read-only load, `format`/
//! `shape`/`root_layer`/`layers`/`include_tasks` argument parsing, and the
//! `output_file` write (the one write this read-only tool is allowed to
//! make, §4.4).

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use crate::storage::docs::layer::{LayerSide, RegisteredLayer};

use super::engine::TraceGraph;
use super::types::{ItemState, SuspectKind, TraceItemInput};

/// A rendered matrix, decoupled from the markdown/CSV specifics — each cell
/// may hold 0, 1, or several ids (§4.4: "セルは複数の ID"). [`render_csv`]/
/// [`render_markdown`] own the per-format join separator and escaping.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MatrixTable {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<Vec<String>>>,
}

/// stable_id -> its raw (possibly unregistered/absent) layer id, straight
/// from [`TraceItemInput`] — everything the tree/edges builders below need
/// to bucket an id into a layer column. Deliberately *not* resolved against
/// the layer registry (`engine::ResolvedItem` is private to that module): a
/// plain string compare against each requested column id is all this module
/// needs, and an item whose `layer` names an unregistered id simply never
/// matches any column (same "層なし" treatment every other trace consumer
/// gives it).
pub fn collect_item_layers(items: &[TraceItemInput]) -> HashMap<String, Option<String>> {
    items
        .iter()
        .map(|i| (i.stable_id.clone(), i.layer.clone()))
        .collect()
}

/// The project's in-use layers (`graph.in_use_layers()`), ordered left side
/// first (ascending level, i.e. upper first), then right side (ascending
/// level) — §4.4's column order: "使用中の層（左側を上位から、次に右側を上位
/// から）".
pub fn ordered_in_use_layers(graph: &TraceGraph, registry: &[RegisteredLayer]) -> Vec<String> {
    let in_use: HashSet<&str> = graph
        .in_use_layers()
        .layers
        .iter()
        .map(String::as_str)
        .collect();

    let mut left: Vec<&RegisteredLayer> = registry
        .iter()
        .filter(|l| l.side == LayerSide::Left && in_use.contains(l.id.as_str()))
        .collect();
    left.sort_by_key(|l| l.level);

    let mut right: Vec<&RegisteredLayer> = registry
        .iter()
        .filter(|l| l.side == LayerSide::Right && in_use.contains(l.id.as_str()))
        .collect();
    right.sort_by_key(|l| l.level);

    left.into_iter()
        .chain(right)
        .map(|l| l.id.clone())
        .collect()
}

/// `root_layer`'s default (§4.4: "root_layer? （既定は使用中で最上位の左側
/// 層）") — the shallowest-level left-side layer in `ordered` (already sorted
/// ascending by level within each side by [`ordered_in_use_layers`]), or
/// `None` when no left-side layer is currently in use (an empty/layerless
/// project, §7 — the caller returns an empty tree, not an error, in that
/// case).
pub fn default_root_layer(ordered: &[String], registry: &[RegisteredLayer]) -> Option<String> {
    ordered
        .iter()
        .find(|id| {
            registry
                .iter()
                .any(|l| &l.id == *id && l.side == LayerSide::Left)
        })
        .cloned()
}

/// Narrows `ordered_in_use` to `requested` (preserving `ordered_in_use`'s
/// canonical order, §4.4) when the caller passed a `layers` filter.
/// `requested` ids that are unknown or not currently in use are dropped with
/// a warning rather than rejected outright — the same "設定エラーは無効化、
/// warning で報告" posture every other trace tool's `layers`/`rules`-style
/// filter takes (wiki/260 §2.1), so a typo'd `--layers` value degrades to
/// "fewer columns than expected" rather than an opaque hard failure.
/// `requested = None` (the argument omitted) returns every in-use layer,
/// unfiltered — §4.4's "未使用の層の列は出さない" baseline.
pub fn resolve_columns(
    ordered_in_use: &[String],
    requested: Option<&[String]>,
) -> (Vec<String>, Vec<String>) {
    let Some(requested) = requested else {
        return (ordered_in_use.to_vec(), Vec::new());
    };
    let requested_set: HashSet<&str> = requested.iter().map(String::as_str).collect();
    let columns: Vec<String> = ordered_in_use
        .iter()
        .filter(|id| requested_set.contains(id.as_str()))
        .cloned()
        .collect();
    let found: HashSet<&str> = columns.iter().map(String::as_str).collect();
    let warnings: Vec<String> = requested
        .iter()
        .filter(|id| !found.contains(id.as_str()))
        .map(|id| format!("layers: {id:?} is not a currently in-use layer, ignored"))
        .collect();
    (columns, warnings)
}

fn item_state_str(state: ItemState) -> &'static str {
    match state {
        ItemState::Passing => "passing",
        ItemState::Uncovered => "uncovered",
        ItemState::NotRun => "not_run",
        ItemState::Blocked => "blocked",
        ItemState::Failing => "failing",
    }
}

/// Every id reachable downward from `root` (inclusive): refining children
/// (deeper left-side items) and verifiers (right-side, or inline-left items
/// verifying themselves don't add extra nodes since they have no further
/// `verified_by`), then recursively the same from each — the "V字1本" one
/// row of §4.4 traces out (wiki/230 INV-614). A DAG-shared descendant (two
/// left items both refined by the same child, or an item verified from two
/// different left-side targets) is visited once per call but can
/// legitimately appear in more than one root's row — this mirrors
/// `TraceGraph`'s own graph (not strictly a tree) rather than forcing single
/// ownership of a shared node.
fn collect_downward(graph: &TraceGraph, root: &str) -> Vec<String> {
    let mut visited: HashSet<String> = HashSet::new();
    let mut order = Vec::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.push_back(root.to_string());
    visited.insert(root.to_string());
    while let Some(id) = queue.pop_front() {
        order.push(id.clone());
        let mut next: Vec<String> = graph.refines_children(&id).to_vec();
        next.extend(graph.verified_by(&id).iter().cloned());
        for n in next {
            if visited.insert(n.clone()) {
                queue.push_back(n);
            }
        }
    }
    order
}

fn tree_headers(columns: &[String], include_tasks: bool) -> Vec<String> {
    let mut headers: Vec<String> = columns.to_vec();
    if include_tasks {
        headers.push("tasks".to_string());
    }
    headers.push("state".to_string());
    headers.push("suspect".to_string());
    headers
}

/// Builds the `shape: "tree"` table (§4.4): one row per `root_layer` item,
/// columns = `columns` (already filtered/ordered by the caller via
/// [`resolve_columns`]) plus, when `include_tasks`, a `tasks` column, then
/// always `state` and `suspect`. `root_layer` naming no in-use item (e.g. the
/// handler's own "no left layer in use" sentinel, or simply a layer with no
/// items) yields a correctly-headered, zero-row table rather than an error —
/// callers never need a separate empty-table path.
///
/// `state` is the root item's own [`TraceGraph::state`] — already the
/// aggregate of its whole reachable subtree (`TraceGraph::build`'s memoized
/// DP folds every refining child's and verifier's state into its parent's,
/// wiki/220-vmodel-integration-design.md §2.7), so no separate aggregation
/// is needed here. `suspect` is the count of [`TraceGraph::suspects`]
/// entries whose relevant item (`Suspect::item` — see that field's own doc
/// comment for what it means per `kind`) is one of this row's reachable
/// ids.
pub fn build_tree_table(
    graph: &TraceGraph,
    item_layers: &HashMap<String, Option<String>>,
    tasks_by_item: &HashMap<String, Vec<String>>,
    root_layer: &str,
    columns: &[String],
    include_tasks: bool,
) -> MatrixTable {
    let headers = tree_headers(columns, include_tasks);

    let mut roots: Vec<&String> = item_layers
        .iter()
        .filter(|(_, layer)| layer.as_deref() == Some(root_layer))
        .map(|(id, _)| id)
        .collect();
    roots.sort();

    let rows: Vec<Vec<Vec<String>>> = roots
        .into_iter()
        .map(|root| {
            let descendants = collect_downward(graph, root);
            let descendant_set: HashSet<&str> = descendants.iter().map(String::as_str).collect();

            let mut row: Vec<Vec<String>> = columns
                .iter()
                .map(|col| {
                    let mut ids: Vec<String> = descendants
                        .iter()
                        .filter(|id| {
                            item_layers.get(*id).and_then(|l| l.as_deref()) == Some(col.as_str())
                        })
                        .cloned()
                        .collect();
                    ids.sort();
                    ids
                })
                .collect();

            if include_tasks {
                let mut tasks: BTreeSet<String> = BTreeSet::new();
                for id in &descendants {
                    if let Some(ts) = tasks_by_item.get(id) {
                        tasks.extend(ts.iter().cloned());
                    }
                }
                row.push(tasks.into_iter().collect());
            }

            let state_cell = graph
                .state(root)
                .map(|s| vec![item_state_str(s).to_string()])
                .unwrap_or_default();
            row.push(state_cell);

            let suspect_count = graph
                .suspects()
                .iter()
                .filter(|s| descendant_set.contains(s.item.as_str()))
                .count();
            row.push(vec![suspect_count.to_string()]);

            row
        })
        .collect();

    MatrixTable { headers, rows }
}

fn edge_row(
    graph: &TraceGraph,
    from: &str,
    to: &str,
    link_type: &'static str,
    item_layers: &HashMap<String, Option<String>>,
    link_suspects: &HashSet<(String, String, String)>,
) -> Vec<Vec<String>> {
    let from_layer = item_layers.get(from).cloned().flatten();
    let to_layer = item_layers.get(to).cloned().flatten();
    let state_cell = graph
        .state(from)
        .map(|s| vec![item_state_str(s).to_string()])
        .unwrap_or_default();
    let suspect =
        link_suspects.contains(&(from.to_string(), to.to_string(), link_type.to_string()));
    vec![
        vec![from.to_string()],
        vec![to.to_string()],
        vec![link_type.to_string()],
        from_layer.map(|l| vec![l]).unwrap_or_default(),
        to_layer.map(|l| vec![l]).unwrap_or_default(),
        state_cell,
        vec![suspect.to_string()],
    ]
}

/// Builds the `shape: "edges"` table (§4.4): one row per resolved
/// `refines`/`verifies` link in `graph` — only legitimate, already-validated
/// edges (`graph.refines_parents`/`graph.verifies_targets`); a dangling or
/// level-invalid reference never reaches those, it surfaces as a
/// `dangling`/`invalid_link` gap instead (`handoff_trace_lint`'s concern,
/// not this export's).
///
/// `state` is the edge's own `from` item's [`TraceGraph::state`] (for a
/// `refines` edge, the refining child's own aggregate state; for a
/// `verifies` edge, the verifier's own state — for a leaf right-side item
/// that is just its own run result) — the status most directly attached to
/// *this* link's origin, not the (possibly multi-child) target's aggregate.
/// `suspect` is `true` exactly when a `link`-kind suspect
/// (`Suspect::kind == SuspectKind::Link`) matches this edge's `{item: from,
/// link_type, upstream}` — an `upstream` reference's `#ACn` acceptance
/// sub-reference suffix is stripped before comparing against `to`, the same
/// base-id resolution `super::engine`'s edge-building applies when it
/// resolves the edge itself.
pub fn build_edges_table(
    graph: &TraceGraph,
    item_layers: &HashMap<String, Option<String>>,
) -> MatrixTable {
    let headers = vec![
        "from".to_string(),
        "to".to_string(),
        "link_type".to_string(),
        "from_layer".to_string(),
        "to_layer".to_string(),
        "state".to_string(),
        "suspect".to_string(),
    ];

    let link_suspects: HashSet<(String, String, String)> = graph
        .suspects()
        .iter()
        .filter(|s| s.kind == SuspectKind::Link)
        .filter_map(|s| {
            let upstream = s.upstream.as_deref()?;
            let link_type = s.link_type.clone()?;
            let base = upstream.split('#').next().unwrap_or(upstream);
            Some((s.item.clone(), base.to_string(), link_type))
        })
        .collect();

    let mut ids: Vec<&String> = item_layers.keys().collect();
    ids.sort();

    let mut rows: Vec<Vec<Vec<String>>> = Vec::new();
    for id in ids {
        for parent in graph.refines_parents(id) {
            rows.push(edge_row(
                graph,
                id,
                parent,
                "refines",
                item_layers,
                &link_suspects,
            ));
        }
        for target in graph.verifies_targets(id) {
            rows.push(edge_row(
                graph,
                id,
                target,
                "verifies",
                item_layers,
                &link_suspects,
            ));
        }
    }

    MatrixTable { headers, rows }
}

/// RFC 4180 CSV (§4.4): every cell quoted (even plain-looking content), BOM-
/// free, `\n` line endings. A `"` inside a value is escaped by doubling it
/// (RFC 4180 §2.7, the only escape the format itself requires). A cell
/// holding more than one id joins them with `"; "` (§4.4: "CSV は '; '
/// 区切り").
pub fn render_csv(table: &MatrixTable) -> String {
    fn quote(s: &str) -> String {
        format!("\"{}\"", s.replace('"', "\"\""))
    }
    fn cell(values: &[String]) -> String {
        quote(&values.join("; "))
    }

    let mut out = String::new();
    let header_cells: Vec<String> = table.headers.iter().map(|h| quote(h)).collect();
    out.push_str(&header_cells.join(","));
    out.push('\n');
    for row in &table.rows {
        let cells: Vec<String> = row.iter().map(|c| cell(c)).collect();
        out.push_str(&cells.join(","));
        out.push('\n');
    }
    out
}

/// Markdown table (§4.4): a cell holding more than one id, or a value
/// containing a literal newline, joins/renders with `<br>`; a literal `|` is
/// escaped as `\|` so it can never be misread as a column boundary (§4.4:
/// "Markdown は `|` と改行をエスケープする").
pub fn render_markdown(table: &MatrixTable) -> String {
    fn escape(s: &str) -> String {
        s.replace('|', "\\|").replace('\n', "<br>")
    }
    fn cell(values: &[String]) -> String {
        values
            .iter()
            .map(|v| escape(v))
            .collect::<Vec<_>>()
            .join("<br>")
    }

    let mut out = String::new();
    out.push_str("| ");
    out.push_str(
        &table
            .headers
            .iter()
            .map(|h| escape(h))
            .collect::<Vec<_>>()
            .join(" | "),
    );
    out.push_str(" |\n");
    out.push_str("| ");
    out.push_str(&vec!["---"; table.headers.len()].join(" | "));
    out.push_str(" |\n");
    for row in &table.rows {
        out.push_str("| ");
        out.push_str(&row.iter().map(|c| cell(c)).collect::<Vec<_>>().join(" | "));
        out.push_str(" |\n");
    }
    out
}

#[cfg(test)]
mod tests;
