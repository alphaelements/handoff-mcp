//! Unit tests for [`super`] (wiki/260-vmodel-m2-design.md §4.4, t360.20.9/
//! M2-09): column ordering/filtering, tree-row construction (descendants,
//! `tasks`/`state`/`suspect`), edges-row construction (suspect matching), and
//! CSV/Markdown rendering (RFC 4180 quoting, `|`/newline escaping).

use std::collections::HashMap;

use crate::trace::types::{TraceInput, TraceItemInput};
use crate::trace::TraceGraph;

use super::*;

fn item(id: &str, doc: &str, layer: &str, refines: &[&str], verifies: &[&str]) -> TraceItemInput {
    TraceItemInput {
        stable_id: id.to_string(),
        doc_id: doc.to_string(),
        layer: Some(layer.to_string()),
        refines: refines.iter().map(|s| s.to_string()).collect(),
        verifies: verifies.iter().map(|s| s.to_string()).collect(),
        method: None,
        has_test_refs: false,
        acceptance_labels: Vec::new(),
        derived: false,
        waived_axes: Vec::new(),
        def_hash: None,
        body_hash: None,
        link_baselines: std::collections::BTreeMap::new(),
    }
}

fn default_registry() -> Vec<crate::storage::docs::layer::RegisteredLayer> {
    TraceInput::default().layer_registry
}

/// REQ-1 --refines-- SPEC-1 --refines-- (none); AT-1 verifies REQ-1, ST-1
/// verifies SPEC-1 — a small but non-trivial 3-level V-shape.
fn v_shape_input() -> TraceInput {
    TraceInput {
        items: vec![
            item("REQ-1", "d1", "requirement", &[], &[]),
            item("REQ-2", "d1", "requirement", &[], &[]),
            item("SPEC-1", "d2", "basic_spec", &["REQ-1"], &[]),
            item("AT-1", "d3", "acceptance", &[], &["REQ-1"]),
            item("ST-1", "d4", "system_test", &[], &["SPEC-1"]),
        ],
        configured_layers: vec![
            "requirement".into(),
            "basic_spec".into(),
            "acceptance".into(),
            "system_test".into(),
        ],
        ..Default::default()
    }
}

// ---------------------------------------------------------------------
// ordered_in_use_layers / default_root_layer / resolve_columns
// ---------------------------------------------------------------------

#[test]
fn ordered_in_use_layers_puts_left_side_before_right_side_each_ascending_by_level() {
    let input = v_shape_input();
    let graph = TraceGraph::build(&input);
    let registry = default_registry();
    let ordered = ordered_in_use_layers(&graph, &registry);
    assert_eq!(
        ordered,
        vec!["requirement", "basic_spec", "acceptance", "system_test"]
    );
}

#[test]
fn ordered_in_use_layers_omits_a_layer_never_used_by_any_item() {
    // detailed_spec/unit_test are built-in but no item in `v_shape_input`
    // uses them — §4.4: "未使用の層の列は出さない".
    let input = v_shape_input();
    let graph = TraceGraph::build(&input);
    let registry = default_registry();
    let ordered = ordered_in_use_layers(&graph, &registry);
    assert!(!ordered.contains(&"detailed_spec".to_string()));
    assert!(!ordered.contains(&"unit_test".to_string()));
}

#[test]
fn default_root_layer_is_the_shallowest_in_use_left_layer() {
    let input = v_shape_input();
    let graph = TraceGraph::build(&input);
    let registry = default_registry();
    let ordered = ordered_in_use_layers(&graph, &registry);
    assert_eq!(
        default_root_layer(&ordered, &registry),
        Some("requirement".to_string())
    );
}

#[test]
fn default_root_layer_is_none_when_no_left_layer_is_in_use() {
    // Only a right-side layer in use (no left-side item at all).
    let input = TraceInput {
        items: vec![item("AT-1", "d1", "acceptance", &[], &[])],
        configured_layers: vec!["acceptance".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let registry = default_registry();
    let ordered = ordered_in_use_layers(&graph, &registry);
    assert_eq!(default_root_layer(&ordered, &registry), None);
}

#[test]
fn resolve_columns_none_returns_every_in_use_layer_unfiltered() {
    let ordered = vec!["requirement".to_string(), "acceptance".to_string()];
    let (columns, warnings) = resolve_columns(&ordered, None);
    assert_eq!(columns, ordered);
    assert!(warnings.is_empty());
}

#[test]
fn resolve_columns_filters_to_requested_preserving_canonical_order() {
    let ordered = vec![
        "requirement".to_string(),
        "basic_spec".to_string(),
        "acceptance".to_string(),
    ];
    let requested = vec!["acceptance".to_string(), "requirement".to_string()];
    let (columns, warnings) = resolve_columns(&ordered, Some(&requested));
    // Canonical order wins, not the caller's argument order.
    assert_eq!(
        columns,
        vec!["requirement".to_string(), "acceptance".to_string()]
    );
    assert!(warnings.is_empty());
}

#[test]
fn resolve_columns_warns_on_an_unknown_or_not_in_use_layer_without_erroring() {
    let ordered = vec!["requirement".to_string()];
    let requested = vec!["requirement".to_string(), "detailed_spec".to_string()];
    let (columns, warnings) = resolve_columns(&ordered, Some(&requested));
    assert_eq!(columns, vec!["requirement".to_string()]);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("detailed_spec"));
}

// ---------------------------------------------------------------------
// build_tree_table
// ---------------------------------------------------------------------

#[test]
fn tree_table_one_row_per_root_item_with_descendants_bucketed_by_layer() {
    let input = v_shape_input();
    let graph = TraceGraph::build(&input);
    let registry = default_registry();
    let item_layers = collect_item_layers(&input.items);
    let ordered = ordered_in_use_layers(&graph, &registry);
    let (columns, _) = resolve_columns(&ordered, None);
    let tasks_by_item: HashMap<String, Vec<String>> = HashMap::new();

    let table = build_tree_table(
        &graph,
        &item_layers,
        &tasks_by_item,
        "requirement",
        &columns,
        true,
    );

    assert_eq!(
        table.headers,
        vec![
            "requirement",
            "basic_spec",
            "acceptance",
            "system_test",
            "tasks",
            "state",
            "suspect"
        ]
    );
    // Two roots: REQ-1 (with descendants) and REQ-2 (isolated).
    assert_eq!(table.rows.len(), 2);

    let req1_row = &table.rows[0];
    assert_eq!(req1_row[0], vec!["REQ-1".to_string()]); // requirement column = itself
    assert_eq!(req1_row[1], vec!["SPEC-1".to_string()]); // basic_spec
    assert_eq!(req1_row[2], vec!["AT-1".to_string()]); // acceptance
    assert_eq!(req1_row[3], vec!["ST-1".to_string()]); // system_test (verifies SPEC-1)
    assert!(req1_row[4].is_empty()); // tasks (none linked)

    let req2_row = &table.rows[1];
    assert_eq!(req2_row[0], vec!["REQ-2".to_string()]);
    assert!(req2_row[1].is_empty());
    assert!(req2_row[2].is_empty());
    assert!(req2_row[3].is_empty());
}

/// Review round 1: neither the tree's `state` column nor the edges'
/// `state` column was asserted anywhere (unit or E2E). Expected values are
/// derived by hand from §4.4 + wiki/220 §2.7, not read back from
/// `graph.state`: ST-1 (verifying SPEC-1) fails while AT-1 (verifying REQ-1
/// directly) passes, so REQ-1's *aggregate* is failing even though its own
/// direct verifier passes; REQ-2 has nothing below it and a deeper layer is
/// in use, so it is uncovered.
#[test]
fn tree_state_is_the_roots_aggregate_and_edges_state_is_the_from_items_state() {
    let mut input = v_shape_input();
    input
        .runs_latest
        .insert("AT-1".to_string(), "pass".to_string());
    input
        .runs_latest
        .insert("ST-1".to_string(), "fail".to_string());
    let graph = TraceGraph::build(&input);
    let registry = default_registry();
    let item_layers = collect_item_layers(&input.items);
    let ordered = ordered_in_use_layers(&graph, &registry);
    let (columns, _) = resolve_columns(&ordered, None);
    let tasks_by_item: HashMap<String, Vec<String>> = HashMap::new();

    let tree = build_tree_table(
        &graph,
        &item_layers,
        &tasks_by_item,
        "requirement",
        &columns,
        false,
    );
    let state_col = tree.headers.iter().position(|h| h == "state").unwrap();
    assert_eq!(tree.rows[0][0], vec!["REQ-1".to_string()]);
    assert_eq!(tree.rows[0][state_col], vec!["failing".to_string()]);
    assert_eq!(tree.rows[1][0], vec!["REQ-2".to_string()]);
    assert_eq!(tree.rows[1][state_col], vec!["uncovered".to_string()]);

    let edges = build_edges_table(&graph, &item_layers);
    let edge_state = |from: &str, to: &str| -> Vec<String> {
        edges
            .rows
            .iter()
            .find(|r| r[0] == vec![from.to_string()] && r[1] == vec![to.to_string()])
            .unwrap_or_else(|| panic!("no edge {from} -> {to}: {:?}", edges.rows))[5]
            .clone()
    };
    assert_eq!(edge_state("AT-1", "REQ-1"), vec!["passing".to_string()]);
    assert_eq!(edge_state("ST-1", "SPEC-1"), vec!["failing".to_string()]);
    assert_eq!(edge_state("SPEC-1", "REQ-1"), vec!["failing".to_string()]);
}

#[test]
fn tree_table_tasks_column_unions_every_descendants_linked_tasks_sorted_deduped() {
    let input = v_shape_input();
    let graph = TraceGraph::build(&input);
    let registry = default_registry();
    let item_layers = collect_item_layers(&input.items);
    let ordered = ordered_in_use_layers(&graph, &registry);
    let (columns, _) = resolve_columns(&ordered, None);

    let mut tasks_by_item: HashMap<String, Vec<String>> = HashMap::new();
    tasks_by_item.insert(
        "REQ-1".to_string(),
        vec!["t2".to_string(), "t1".to_string()],
    );
    tasks_by_item.insert("SPEC-1".to_string(), vec!["t1".to_string()]);

    let table = build_tree_table(
        &graph,
        &item_layers,
        &tasks_by_item,
        "requirement",
        &columns,
        true,
    );
    let req1_row = &table.rows[0];
    // tasks is the 5th column (index 4) given 4 in-use layer columns above.
    assert_eq!(req1_row[4], vec!["t1".to_string(), "t2".to_string()]);
}

#[test]
fn tree_table_omits_tasks_column_when_include_tasks_is_false() {
    let input = v_shape_input();
    let graph = TraceGraph::build(&input);
    let registry = default_registry();
    let item_layers = collect_item_layers(&input.items);
    let ordered = ordered_in_use_layers(&graph, &registry);
    let (columns, _) = resolve_columns(&ordered, None);
    let tasks_by_item: HashMap<String, Vec<String>> = HashMap::new();

    let table = build_tree_table(
        &graph,
        &item_layers,
        &tasks_by_item,
        "requirement",
        &columns,
        false,
    );
    assert_eq!(
        table.headers,
        vec![
            "requirement",
            "basic_spec",
            "acceptance",
            "system_test",
            "state",
            "suspect"
        ]
    );
    assert_eq!(table.rows[0].len(), 6);
}

#[test]
fn tree_table_root_layer_with_no_items_yields_zero_rows_not_an_error() {
    let input = v_shape_input();
    let graph = TraceGraph::build(&input);
    let registry = default_registry();
    let item_layers = collect_item_layers(&input.items);
    let ordered = ordered_in_use_layers(&graph, &registry);
    let (columns, _) = resolve_columns(&ordered, None);
    let tasks_by_item: HashMap<String, Vec<String>> = HashMap::new();

    // "" is the handler's own sentinel for "no left layer in use at all".
    let table = build_tree_table(&graph, &item_layers, &tasks_by_item, "", &columns, true);
    assert!(table.rows.is_empty());
    assert!(!table.headers.is_empty());
}

#[test]
fn tree_table_suspect_count_reflects_only_this_rows_reachable_items() {
    // SPEC-1 refines REQ-1 with a stale link baseline (REQ-1's def_hash
    // changed since the baseline was recorded) — a `link`-kind suspect whose
    // `item` is SPEC-1 (the child), reachable from REQ-1's row only.
    let mut spec = item("SPEC-1", "d2", "basic_spec", &["REQ-1"], &[]);
    spec.link_baselines
        .insert("REQ-1".to_string(), "stale-hash".to_string());
    let mut req1 = item("REQ-1", "d1", "requirement", &[], &[]);
    req1.def_hash = Some("current-hash".to_string());
    let req2 = item("REQ-2", "d1", "requirement", &[], &[]);

    let input = TraceInput {
        items: vec![req1, req2, spec],
        configured_layers: vec!["requirement".into(), "basic_spec".into()],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    assert!(
        graph.suspects().iter().any(|s| s.item == "SPEC-1"),
        "fixture precondition: SPEC-1 must carry a link suspect: {:?}",
        graph.suspects()
    );

    let registry = default_registry();
    let item_layers = collect_item_layers(&input.items);
    let ordered = ordered_in_use_layers(&graph, &registry);
    let (columns, _) = resolve_columns(&ordered, None);
    let tasks_by_item: HashMap<String, Vec<String>> = HashMap::new();

    let table = build_tree_table(
        &graph,
        &item_layers,
        &tasks_by_item,
        "requirement",
        &columns,
        false,
    );
    // Row order: REQ-1 then REQ-2 (sorted ids). suspect is the last column.
    let suspect_col = table.headers.len() - 1;
    assert_eq!(
        table.rows[0][suspect_col],
        vec!["1".to_string()],
        "{:?}",
        table.rows
    );
    assert_eq!(
        table.rows[1][suspect_col],
        vec!["0".to_string()],
        "{:?}",
        table.rows
    );
}

// ---------------------------------------------------------------------
// build_edges_table
// ---------------------------------------------------------------------

#[test]
fn edges_table_one_row_per_resolved_refines_and_verifies_link() {
    let input = v_shape_input();
    let graph = TraceGraph::build(&input);
    let item_layers = collect_item_layers(&input.items);

    let table = build_edges_table(&graph, &item_layers);
    assert_eq!(
        table.headers,
        vec![
            "from",
            "to",
            "link_type",
            "from_layer",
            "to_layer",
            "state",
            "suspect"
        ]
    );
    // SPEC-1 refines REQ-1, AT-1 verifies REQ-1, ST-1 verifies SPEC-1 = 3 edges.
    assert_eq!(table.rows.len(), 3);

    let refines_row = table
        .rows
        .iter()
        .find(|r| r[2] == vec!["refines".to_string()])
        .expect("one refines edge");
    assert_eq!(refines_row[0], vec!["SPEC-1".to_string()]);
    assert_eq!(refines_row[1], vec!["REQ-1".to_string()]);
    assert_eq!(refines_row[3], vec!["basic_spec".to_string()]);
    assert_eq!(refines_row[4], vec!["requirement".to_string()]);
}

#[test]
fn edges_table_marks_suspect_true_only_for_the_matching_link() {
    let mut spec = item("SPEC-1", "d2", "basic_spec", &["REQ-1"], &[]);
    spec.link_baselines
        .insert("REQ-1".to_string(), "stale-hash".to_string());
    let mut req1 = item("REQ-1", "d1", "requirement", &[], &[]);
    req1.def_hash = Some("current-hash".to_string());
    let at1 = item("AT-1", "d3", "acceptance", &[], &["REQ-1"]);

    let input = TraceInput {
        items: vec![req1, spec, at1],
        configured_layers: vec![
            "requirement".into(),
            "basic_spec".into(),
            "acceptance".into(),
        ],
        ..Default::default()
    };
    let graph = TraceGraph::build(&input);
    let item_layers = collect_item_layers(&input.items);

    let table = build_edges_table(&graph, &item_layers);
    let suspect_col = table.headers.len() - 1;
    let refines_row = table
        .rows
        .iter()
        .find(|r| r[0] == vec!["SPEC-1".to_string()] && r[2] == vec!["refines".to_string()])
        .unwrap();
    assert_eq!(refines_row[suspect_col], vec!["true".to_string()]);

    let verifies_row = table
        .rows
        .iter()
        .find(|r| r[0] == vec!["AT-1".to_string()])
        .unwrap();
    assert_eq!(verifies_row[suspect_col], vec!["false".to_string()]);
}

// ---------------------------------------------------------------------
// render_csv / render_markdown
// ---------------------------------------------------------------------

#[test]
fn render_csv_quotes_every_cell_joins_multi_id_with_semicolon_and_uses_lf() {
    let table = MatrixTable {
        headers: vec!["requirement".to_string(), "state".to_string()],
        rows: vec![vec![
            vec!["REQ-1".to_string(), "REQ-2".to_string()],
            vec!["passing".to_string()],
        ]],
    };
    let csv = render_csv(&table);
    assert_eq!(
        csv,
        "\"requirement\",\"state\"\n\"REQ-1; REQ-2\",\"passing\"\n"
    );
    assert!(!csv.contains('\r'));
    assert!(!csv.starts_with('\u{feff}'), "must not emit a BOM");
}

#[test]
fn render_csv_doubles_an_embedded_quote() {
    let table = MatrixTable {
        headers: vec!["col".to_string()],
        rows: vec![vec![vec!["has \"quotes\"".to_string()]]],
    };
    let csv = render_csv(&table);
    assert_eq!(csv, "\"col\"\n\"has \"\"quotes\"\"\"\n");
}

#[test]
fn render_markdown_joins_multi_id_with_br_and_escapes_pipe_and_newline() {
    let table = MatrixTable {
        headers: vec!["requirement".to_string()],
        rows: vec![vec![vec!["REQ-1".to_string(), "REQ-2".to_string()]]],
    };
    let md = render_markdown(&table);
    assert!(md.starts_with("| requirement |\n| --- |\n"));
    assert!(md.contains("REQ-1<br>REQ-2"));

    let table2 = MatrixTable {
        headers: vec!["col".to_string()],
        rows: vec![vec![vec!["a|b\nc".to_string()]]],
    };
    let md2 = render_markdown(&table2);
    assert!(md2.contains("a\\|b<br>c"), "{md2}");
}
