//! Traceability link diagram for verification / completion reports
//! (REQ-PDF-033).
//!
//! [`trace_diagram`] turns the trace report's `items[]` (`handoff_trace_report`
//! with `include_items`) into one Mermaid `graph TD`: a subgraph per trace
//! layer in V-model order (requirement -> spec -> test), an edge per
//! `refines` (parent to child) and `verifies` (verified item to verifier)
//! link, and a node colour per item state.

use std::collections::{BTreeSet, HashMap};

use serde_json::Value;

use super::charts::sanitize;
use super::ReportScope;

/// Largest diagram that is still readable on a page; a bigger trace graph is
/// reported as [`TraceDiagram::TooLarge`] instead of drawn.
pub const MAX_NODES: usize = 150;

/// Longest title shown inside a node.
pub const MAX_TITLE_CHARS: usize = 40;

/// Outcome of [`trace_diagram`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceDiagram {
    /// Mermaid source without the code fence, ending in a newline.
    Graph(String),
    /// No item to draw (no items, or the scope selects none).
    Empty,
    /// More than [`MAX_NODES`] items would be drawn.
    TooLarge { nodes: usize },
}

/// Node classes, in drawing order: the item state they stand for, and the
/// Mermaid `classDef` style. `uncovered` and an unknown state share the gray
/// `not_run` class: nothing has been run for them.
const CLASSES: [(&str, &str); 4] = [
    ("passing", "fill:#c8e6c9,stroke:#2e7d32,color:#000"),
    ("failing", "fill:#ffcdd2,stroke:#c62828,color:#000"),
    ("blocked", "fill:#ffe0b2,stroke:#ef6c00,color:#000"),
    ("not_run", "fill:#e0e0e0,stroke:#757575,color:#000"),
];

const CLASS_NOT_RUN: &str = "not_run";

const LINK_REFINES: &str = "refines";
const LINK_VERIFIES: &str = "verifies";

/// One drawn item.
struct Node<'a> {
    id: &'a str,
    layer: Option<&'a str>,
    title: &'a str,
    class: &'static str,
    refines: Vec<&'a str>,
    verifies: Vec<&'a str>,
}

fn str_list<'a>(item: &'a Value, key: &str) -> Vec<&'a str> {
    item.get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

fn class_of(state: Option<&str>) -> &'static str {
    CLASSES
        .iter()
        .map(|(name, _)| *name)
        .find(|name| Some(*name) == state)
        .unwrap_or(CLASS_NOT_RUN)
}

/// Free text as a node / subgraph label: Mermaid syntax characters and
/// markup (`<` `>` `[` `]` and backticks, which could also close the code
/// fence) are dropped, whitespace collapses, long text is cut.
fn label(text: &str, max_chars: usize) -> String {
    let clean = sanitize(text)
        .chars()
        .filter(|c| !matches!(c, '<' | '>' | '[' | ']' | '`'))
        .collect::<String>();
    if clean.chars().count() > max_chars {
        let cut: String = clean.chars().take(max_chars).collect();
        format!("{}…", cut.trim_end())
    } else {
        clean
    }
}

fn parse_nodes(trace_report: &Value) -> Vec<Node<'_>> {
    trace_report
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            Some(Node {
                id: item.get("id")?.as_str()?,
                layer: item.get("layer").and_then(Value::as_str),
                title: item
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                class: class_of(item.get("state").and_then(Value::as_str)),
                refines: str_list(item, "refines"),
                verifies: str_list(item, "verifies"),
            })
        })
        .collect()
}

/// Indices of the nodes to draw: those matching `scope.layers` /
/// `scope.items` (all when neither narrows), plus everything linked to them
/// directly or transitively, so a scoped diagram still shows the whole
/// requirement -> spec -> test chains it belongs to.
fn select(nodes: &[Node<'_>], scope: &ReportScope) -> Vec<usize> {
    if scope.layers.is_empty() && scope.items.is_empty() {
        return (0..nodes.len()).collect();
    }
    let index: HashMap<&str, usize> = nodes.iter().enumerate().map(|(i, n)| (n.id, i)).collect();
    let mut neighbours: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (i, node) in nodes.iter().enumerate() {
        for target in node.refines.iter().chain(&node.verifies) {
            if let Some(&j) = index.get(target) {
                neighbours[i].push(j);
                neighbours[j].push(i);
            }
        }
    }
    let mut selected = vec![false; nodes.len()];
    let mut queue: Vec<usize> = (0..nodes.len())
        .filter(|&i| {
            let node = &nodes[i];
            (scope.layers.is_empty()
                || node
                    .layer
                    .is_some_and(|l| scope.layers.iter().any(|s| s == l)))
                && (scope.items.is_empty() || scope.items.iter().any(|s| s == node.id))
        })
        .collect();
    for &i in &queue {
        selected[i] = true;
    }
    while let Some(i) = queue.pop() {
        for &j in &neighbours[i] {
            if !selected[j] {
                selected[j] = true;
                queue.push(j);
            }
        }
    }
    (0..nodes.len()).filter(|&i| selected[i]).collect()
}

/// Layers of the drawn nodes: the trace report's in-use layers in their
/// V-model order, then any other layer in order of appearance.
fn layer_order<'a>(trace_report: &'a Value, drawn: &[&Node<'a>]) -> Vec<&'a str> {
    let mut layers: Vec<&str> = trace_report["trace_layers"]["in_use"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|l| drawn.iter().any(|n| n.layer == Some(*l)))
        .collect();
    for layer in drawn.iter().filter_map(|n| n.layer) {
        if !layers.contains(&layer) {
            layers.push(layer);
        }
    }
    layers
}

/// The traceability diagram of `trace_report` (see the module docs),
/// narrowed by the report scope's `layers` / `items`.
pub fn trace_diagram(trace_report: &Value, scope: &ReportScope) -> TraceDiagram {
    let all = parse_nodes(trace_report);
    let selected = select(&all, scope);
    if selected.is_empty() {
        return TraceDiagram::Empty;
    }
    if selected.len() > MAX_NODES {
        return TraceDiagram::TooLarge {
            nodes: selected.len(),
        };
    }
    let drawn: Vec<&Node<'_>> = selected.iter().map(|&i| &all[i]).collect();
    let number: HashMap<&str, usize> = drawn.iter().enumerate().map(|(n, d)| (d.id, n)).collect();
    let node_line = |n: usize| {
        let node = drawn[n];
        let title = label(node.title, MAX_TITLE_CHARS);
        format!("n{n}[\"{} {title}\"]", label(node.id, usize::MAX))
    };

    let mut out = String::from("graph TD\n");
    let layers = layer_order(trace_report, &drawn);
    for (k, layer) in layers.iter().enumerate() {
        out.push_str(&format!(
            "    subgraph L{k}[\"{}\"]\n",
            label(layer, usize::MAX)
        ));
        for n in (0..drawn.len()).filter(|&n| drawn[n].layer == Some(*layer)) {
            out.push_str(&format!("        {}\n", node_line(n)));
        }
        out.push_str("    end\n");
    }
    for n in (0..drawn.len()).filter(|&n| drawn[n].layer.is_none()) {
        out.push_str(&format!("    {}\n", node_line(n)));
    }

    let mut edges: BTreeSet<(usize, usize, &str)> = BTreeSet::new();
    for (n, node) in drawn.iter().enumerate() {
        for (links, kind) in [
            (&node.refines, LINK_REFINES),
            (&node.verifies, LINK_VERIFIES),
        ] {
            for from in links.iter().filter_map(|id| number.get(id)) {
                edges.insert((*from, n, kind));
            }
        }
    }
    for (from, to, kind) in edges {
        let arrow = if kind == LINK_VERIFIES { "-.->" } else { "-->" };
        out.push_str(&format!("    n{from} {arrow}|{kind}| n{to}\n"));
    }

    for (class, style) in CLASSES {
        let members: Vec<String> = (0..drawn.len())
            .filter(|&n| drawn[n].class == class)
            .map(|n| format!("n{n}"))
            .collect();
        if !members.is_empty() {
            out.push_str(&format!("    classDef {class} {style}\n"));
            out.push_str(&format!("    class {} {class}\n", members.join(",")));
        }
    }
    TraceDiagram::Graph(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(id: &str, layer: &str, state: &str, refines: &[&str], verifies: &[&str]) -> Value {
        json!({ "id": id, "layer": layer, "title": format!("Title of {id}"),
                "state": state, "refines": refines, "verifies": verifies })
    }

    fn report(items: Vec<Value>) -> Value {
        json!({ "trace_layers": { "in_use": ["requirement", "spec", "unit_test"] },
                "items": items })
    }

    fn graph(report: &Value, scope: &ReportScope) -> String {
        match trace_diagram(report, scope) {
            TraceDiagram::Graph(g) => g,
            other => panic!("expected a graph, got {other:?}"),
        }
    }

    fn chain() -> Value {
        report(vec![
            item("REQ-1", "requirement", "passing", &[], &[]),
            item("SPEC-1", "spec", "failing", &["REQ-1"], &[]),
            item("UT-1", "unit_test", "not_run", &[], &["SPEC-1"]),
        ])
    }

    #[test]
    fn draws_requirement_spec_test_hierarchy_with_link_kinds() {
        let g = graph(&chain(), &ReportScope::default());
        assert!(g.starts_with("graph TD\n"), "{g}");
        let req = g.find("subgraph L0[\"requirement\"]").expect(&g);
        let spec = g.find("subgraph L1[\"spec\"]").expect(&g);
        let test = g.find("subgraph L2[\"unit_test\"]").expect(&g);
        assert!(req < spec && spec < test, "{g}");
        assert!(g.contains("n0[\"REQ-1 Title of REQ-1\"]"), "{g}");
        assert!(g.contains("n0 -->|refines| n1\n"), "{g}");
        assert!(g.contains("n1 -.->|verifies| n2\n"), "{g}");
    }

    #[test]
    fn colours_nodes_by_state() {
        let g = graph(&chain(), &ReportScope::default());
        assert!(g.contains("class n0 passing\n"), "{g}");
        assert!(g.contains("class n1 failing\n"), "{g}");
        assert!(g.contains("class n2 not_run\n"), "{g}");
        assert!(g.contains("classDef passing fill:#c8e6c9"), "{g}");
        assert!(g.contains("classDef failing fill:#ffcdd2"), "{g}");
        assert!(g.contains("classDef not_run fill:#e0e0e0"), "{g}");
    }

    #[test]
    fn uncovered_and_unknown_states_are_gray_and_blocked_is_orange() {
        let r = report(vec![
            item("A", "requirement", "uncovered", &[], &[]),
            item("B", "requirement", "blocked", &[], &[]),
            json!({ "id": "C", "layer": "requirement", "title": "c" }),
        ]);
        let g = graph(&r, &ReportScope::default());
        assert!(g.contains("class n0,n2 not_run\n"), "{g}");
        assert!(g.contains("class n1 blocked\n"), "{g}");
    }

    #[test]
    fn links_to_unknown_items_are_skipped() {
        let r = report(vec![item("SPEC-1", "spec", "passing", &["GONE"], &[])]);
        let g = graph(&r, &ReportScope::default());
        assert!(!g.contains("-->"), "{g}");
        assert!(!g.contains("GONE"), "{g}");
    }

    #[test]
    fn free_text_cannot_break_out_of_the_node_label() {
        let r = report(vec![json!({
            "id": "REQ-1", "layer": "requirement", "state": "passing",
            "title": "a \"quoted\" <b>bold</b> [x] `tick` #h; 50%\nnext",
        })]);
        let g = graph(&r, &ReportScope::default());
        let label = g
            .lines()
            .find(|l| l.trim_start().starts_with("n0["))
            .expect(&g);
        assert_eq!(
            label.trim(),
            "n0[\"REQ-1 a quoted bbold/b x tick h 50 next\"]"
        );
    }

    #[test]
    fn long_titles_are_truncated() {
        let r = report(vec![json!({
            "id": "R", "layer": "requirement", "state": "passing",
            "title": "x".repeat(MAX_TITLE_CHARS + 20),
        })]);
        let g = graph(&r, &ReportScope::default());
        assert!(
            g.contains(&format!("R {}…\"]", "x".repeat(MAX_TITLE_CHARS))),
            "{g}"
        );
    }

    #[test]
    fn scope_layers_select_items_plus_their_connected_chain() {
        let mut items = vec![item("OTHER", "requirement", "passing", &[], &[])];
        items.extend(chain()["items"].as_array().unwrap().clone());
        let r = report(items);
        let scope = ReportScope {
            layers: vec!["spec".into()],
            ..ReportScope::default()
        };
        let g = graph(&r, &scope);
        assert!(
            g.contains("REQ-1") && g.contains("SPEC-1") && g.contains("UT-1"),
            "{g}"
        );
        assert!(!g.contains("OTHER"), "{g}");
    }

    #[test]
    fn scope_items_select_the_named_items_chain() {
        let mut items = vec![item("OTHER", "requirement", "passing", &[], &[])];
        items.extend(chain()["items"].as_array().unwrap().clone());
        let scope = ReportScope {
            items: vec!["OTHER".into()],
            ..ReportScope::default()
        };
        let g = graph(&report(items), &scope);
        assert!(g.contains("OTHER") && !g.contains("REQ-1"), "{g}");
    }

    #[test]
    fn layers_missing_from_in_use_follow_the_in_use_ones() {
        let r = json!({
            "trace_layers": { "in_use": ["requirement"] },
            "items": [ item("X", "weird", "passing", &[], &[]),
                       item("R", "requirement", "passing", &[], &[]) ] });
        let g = graph(&r, &ReportScope::default());
        assert!(
            g.find("subgraph L0[\"requirement\"]") < g.find("subgraph L1[\"weird\"]"),
            "{g}"
        );
    }

    #[test]
    fn nothing_to_draw_is_empty() {
        assert_eq!(
            trace_diagram(&report(vec![]), &ReportScope::default()),
            TraceDiagram::Empty
        );
        assert_eq!(
            trace_diagram(&json!({}), &ReportScope::default()),
            TraceDiagram::Empty
        );
        let scope = ReportScope {
            items: vec!["NOPE".into()],
            ..ReportScope::default()
        };
        assert_eq!(trace_diagram(&chain(), &scope), TraceDiagram::Empty);
    }

    #[test]
    fn oversized_graphs_are_not_drawn() {
        let items = (0..=MAX_NODES)
            .map(|i| item(&format!("R{i}"), "requirement", "passing", &[], &[]))
            .collect();
        assert_eq!(
            trace_diagram(&report(items), &ReportScope::default()),
            TraceDiagram::TooLarge {
                nodes: MAX_NODES + 1
            }
        );
    }
}
