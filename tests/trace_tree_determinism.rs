//! Determinism and shape checks of the requirement-tree output over the
//! 3,000-item synthetic corpus (wiki/300-requirement-tree-design.md,
//! SPEC-TREE-015, t-rtree.7).
//!
//! The corpus comes from the pure generator in
//! `benches/support/tree_synth.rs` (shared with the `trace_tree` bench) and is
//! saved into fresh projects through the real `handoff-mcp` binary. Same
//! input twice must give the same `handoff_trace_report` /
//! `handoff_trace_tree` output and the same persisted `_trace_report.json` —
//! neither HashMap iteration order nor anything else may leak into them.
//! Timing is measured by the bench, not asserted here:
//!
//! ```text
//! cargo bench --bench trace_tree
//! ```

#[path = "../benches/support/mcp_client.rs"]
mod mcp_client;
#[path = "../benches/support/tree_synth.rs"]
mod tree_synth;

use std::collections::{BTreeMap, BTreeSet, HashSet};

use mcp_client::Server;
use serde_json::{json, Value};

const REPORT_FILE: &str = ".handoff/docs/_trace_report.json";

/// Volatile fields: wall-clock stamps / file mtimes that legitimately differ
/// between runs (`inputs.docs_max_mtime_ns` is the staleness fingerprint).
const VOLATILE_KEYS: &[&str] = &["generated_at", "docs_max_mtime_ns"];

fn is_doc_id(s: &str) -> bool {
    s.starts_with("doc-") && s.len() > 4 && s[4..].chars().all(|c| c.is_ascii_digit() || c == '-')
}

fn collect_doc_ids(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::String(s) if is_doc_id(s) => {
            out.insert(s.clone());
        }
        Value::Array(a) => a.iter().for_each(|x| collect_doc_ids(x, out)),
        Value::Object(m) => m.values().for_each(|x| collect_doc_ids(x, out)),
        _ => {}
    }
}

fn rewrite(v: &mut Value, doc_ids: &BTreeMap<String, String>) {
    match v {
        Value::String(s) => {
            if let Some(stable) = doc_ids.get(s.as_str()) {
                *s = stable.clone();
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| rewrite(x, doc_ids)),
        Value::Object(m) => {
            for key in VOLATILE_KEYS {
                m.remove(*key);
            }
            m.values_mut().for_each(|x| rewrite(x, doc_ids));
        }
        _ => {}
    }
}

/// Document ids embed their creation time, so two fresh projects differ in
/// them. Documents are created in the same order each time, so renaming each
/// distinct id to its rank gives a comparable form.
fn normalize(mut v: Value) -> Value {
    let mut ids = BTreeSet::new();
    collect_doc_ids(&v, &mut ids);
    let renamed: BTreeMap<String, String> = ids
        .into_iter()
        .enumerate()
        .map(|(rank, id)| (id, format!("DOC#{rank:03}")))
        .collect();
    rewrite(&mut v, &renamed);
    v
}

/// JSON-pointer-ish path of the first difference, or `None` when equal.
fn first_diff(a: &Value, b: &Value, path: &str) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            if x.keys().ne(y.keys()) {
                return Some(format!(
                    "{path}: keys {:?} != {:?}",
                    x.keys().collect::<Vec<_>>(),
                    y.keys().collect::<Vec<_>>()
                ));
            }
            x.iter()
                .find_map(|(k, v)| first_diff(v, &y[k], &format!("{path}/{k}")))
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                return Some(format!("{path}: len {} != {}", x.len(), y.len()));
            }
            x.iter()
                .zip(y)
                .enumerate()
                .find_map(|(i, (l, r))| first_diff(l, r, &format!("{path}/{i}")))
        }
        _ if a == b => None,
        _ => Some(format!("{path}: {a} != {b}")),
    }
}

fn assert_same(a: Value, b: Value, what: &str) {
    if let Some(diff) = first_diff(&normalize(a), &normalize(b), "") {
        panic!("{what} differs at {diff}");
    }
}

/// Everything the tree output is made of, from one project.
struct Outputs {
    report: Value,
    report_text: String,
    persisted: Value,
    tree: Value,
    tree_text: String,
}

fn snapshot(server: &mut Server) -> Outputs {
    let report_text = server.call_text(
        "handoff_trace_report",
        json!({"include_items": true, "limit": 5000}),
    );
    let report: Value = serde_json::from_str(&report_text).expect("report json");
    let persisted_text = std::fs::read_to_string(server.project().join(REPORT_FILE))
        .expect("persisted _trace_report.json");
    let persisted = serde_json::from_str(&persisted_text).expect("persisted report json");
    let tree_text = server.call_text("handoff_trace_tree", json!({}));
    let tree = serde_json::from_str(&tree_text).expect("tree json");
    Outputs {
        report,
        report_text,
        persisted,
        tree,
        tree_text,
    }
}

fn fresh_project() -> (tempfile::TempDir, Server) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut server = Server::start(tmp.path());
    server.load_synthetic();
    (tmp, server)
}

#[test]
fn synthetic_corpus_is_pure_and_has_the_declared_size() {
    let first = tree_synth::synth_docs();
    assert_eq!(first, tree_synth::synth_docs(), "generator must be pure");
    assert_eq!(first.len(), tree_synth::TOTAL_DOCS);
    let headings: usize = first
        .iter()
        .map(|d| d.body.lines().filter(|l| l.starts_with("### ")).count())
        .sum();
    assert_eq!(headings, tree_synth::TOTAL_ITEMS);
    let slugs: HashSet<&str> = first.iter().map(|d| d.slug.as_str()).collect();
    assert_eq!(slugs.len(), first.len(), "slugs must be unique");
}

#[test]
fn synthetic_workspace_exercises_dag_composite_decision_and_depth() {
    let (_tmp, mut server) = fresh_project();
    let out = snapshot(&mut server);

    let tree = &out.report["tree"];
    assert!(
        tree["max_depth"].as_u64().unwrap() >= 20,
        "deep tree expected: {tree}"
    );
    assert!(tree["composite_count"].as_u64().unwrap() > 0, "{tree}");
    assert!(tree["decision_count"].as_u64().unwrap() > 0, "{tree}");
    assert!(tree["dag_shared_count"].as_u64().unwrap() > 0, "{tree}");
    assert_eq!(
        out.report["totals"]["nodes"].as_u64(),
        Some(tree_synth::TOTAL_ITEMS as u64)
    );
    assert_eq!(
        out.report["items"].as_array().unwrap().len(),
        tree_synth::TOTAL_ITEMS
    );
    // The persisted file carries the same tree and totals as the response.
    assert_eq!(out.persisted["tree"], out.report["tree"]);
    assert_eq!(out.persisted["totals"], out.report["totals"]);
    // A multi-parent item really lists several parents.
    assert!(out.report["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["parents"].as_array().is_some_and(|p| p.len() > 1)));
}

#[test]
fn trace_report_and_trace_tree_are_deterministic() {
    let (_tmp_a, mut server_a) = fresh_project();
    let first = snapshot(&mut server_a);
    let second = snapshot(&mut server_a);

    // Same project, called twice: byte-identical responses, and the
    // persisted file only differs by its timestamp.
    assert_eq!(
        first.report_text, second.report_text,
        "report, same project"
    );
    assert_eq!(first.tree_text, second.tree_text, "tree, same project");
    assert_same(
        first.persisted.clone(),
        second.persisted,
        "persisted report, same project",
    );

    // Two independent projects built from the same input.
    let (_tmp_b, mut server_b) = fresh_project();
    let other = snapshot(&mut server_b);
    assert_same(first.report, other.report, "report, fresh project");
    assert_same(first.tree, other.tree, "tree, fresh project");
    assert_same(
        first.persisted,
        other.persisted,
        "persisted report, fresh project",
    );
}

/// `TraceGraph::coverage` is a `HashMap`; with only a handful of layers a
/// random iteration order can coincide with the sorted one, so comparing two
/// runs is a probabilistic guard. Assert the sorted order directly instead.
#[test]
fn coverage_layers_are_emitted_in_sorted_order() {
    let (_tmp, mut server) = fresh_project();
    let out = snapshot(&mut server);
    for (what, doc) in [("response", &out.report), ("persisted", &out.persisted)] {
        let keys: Vec<&String> = doc["coverage"].as_object().unwrap().keys().collect();
        assert!(keys.len() >= 3, "{what}: expected several layers: {keys:?}");
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "{what}: coverage keys must be sorted");
    }
}

#[test]
fn roots_and_children_follow_the_sibling_order() {
    let (_tmp, mut server) = fresh_project();
    let out = snapshot(&mut server);
    let items = out.report["items"].as_array().unwrap();
    let by_id: BTreeMap<&str, &Value> = items
        .iter()
        .map(|i| (i["id"].as_str().unwrap(), i))
        .collect();

    // Items that carry an `order@<parent>` override are sorted by a
    // parent-specific key the report does not expose; leave them out of the
    // comparison (removing items never reorders the others).
    let overridden: HashSet<String> = tree_synth::synth_docs()
        .iter()
        .flat_map(|d| {
            let mut ids = Vec::new();
            let mut current = None;
            for line in d.body.lines() {
                if let Some(h) = line.strip_prefix("### ") {
                    current = h.split_whitespace().next().map(str::to_string);
                } else if line.starts_with("- order@") {
                    ids.extend(current.clone());
                }
            }
            ids
        })
        .collect();
    assert!(!overridden.is_empty(), "corpus must exercise order@");

    // (order.is_none(), order, doc, id): `doc` ids are monotonic in document
    // creation order, which is slug order for the synthetic documents.
    let key = |id: &str| {
        let item = by_id[id];
        (
            item["order"].is_null(),
            item["order"].as_u64(),
            item["doc"].as_str().unwrap().to_string(),
            id.to_string(),
        )
    };
    let assert_sorted = |ids: Vec<&str>, what: &str| {
        let keys: Vec<_> = ids
            .iter()
            .filter(|id| !overridden.contains(**id))
            .map(|id| key(id))
            .collect();
        assert!(
            keys.windows(2).all(|w| w[0] <= w[1]),
            "{what} not in sibling order: {keys:?}"
        );
    };

    let roots = out.report["tree"]["roots"].as_object().unwrap();
    for (layer, ids) in roots {
        let ids: Vec<&str> = ids
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_sorted(ids, &format!("roots of {layer}"));
    }
    let mut checked = 0;
    for item in items {
        let children: Vec<&str> = item["children"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        if children.len() > 1 {
            checked += 1;
            assert_sorted(children, &format!("children of {}", item["id"]));
        }
    }
    assert!(checked > 0, "some item must have several children");
}
