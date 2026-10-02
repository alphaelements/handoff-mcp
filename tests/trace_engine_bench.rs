//! JA-scale synthetic benchmark for the M1 trace derivation engine
//! (wiki/220-vmodel-integration-design.md §2.7, wiki/240-performance-design.md
//! §6-7 PR-7, t360.9): 2,500 items across 30 documents with `refines`/
//! `verifies` edges wired across every one of the 6 built-in layers,
//! asserting `TraceGraph::build` completes in under 1 second.
//!
//! `#[ignore]`d — not part of the default `cargo test` (precedent:
//! `tests/context_corpus_bench.rs`). Run explicitly:
//!
//! ```text
//! cargo test --release --test trace_engine_bench -- --ignored --nocapture
//! ```

use std::collections::HashMap;
use std::time::Instant;

use handoff_mcp::trace::engine::TraceGraph;
use handoff_mcp::trace::types::{TaskLinkRole, TaskRequirementLink, TraceInput, TraceItemInput};

const N_ITEMS: usize = 2500;
const N_DOCS: usize = 30;

/// Deterministically builds a JA-scale synthetic trace graph: `requirement`
/// items with no refines, `basic_spec` items each refining a `requirement`,
/// `detailed_spec` items each refining a `basic_spec` (a 3-level left-side
/// chain exercising the DP's recursion), and `acceptance`/`system_test`/
/// `unit_test` items each verifying a left item at or below their level
/// (fan-in: many verifiers can legitimately share one target, exercising
/// `verified_by`'s aggregation without recomputation blowup).
fn synthetic_input() -> TraceInput {
    // 300/450/550/300/450/450 = 2,500, spread round-robin across 30 doc ids.
    let counts: [(&str, usize); 6] = [
        ("requirement", 300),
        ("basic_spec", 450),
        ("detailed_spec", 550),
        ("acceptance", 300),
        ("system_test", 450),
        ("unit_test", 450),
    ];
    debug_assert_eq!(counts.iter().map(|(_, c)| c).sum::<usize>(), N_ITEMS);

    let mut items: Vec<TraceItemInput> = Vec::with_capacity(N_ITEMS);
    let mut ids_by_layer: HashMap<&str, Vec<String>> = HashMap::new();
    let mut global_idx = 0usize;

    for (layer, count) in counts {
        for i in 0..count {
            let id = format!("{}-{i:05}", layer_prefix(layer));
            ids_by_layer.entry(layer).or_default().push(id.clone());
            let doc_id = format!("doc-{}", global_idx % N_DOCS);
            global_idx += 1;

            let (refines, method) = match layer {
                "basic_spec" => {
                    let parents = &ids_by_layer["requirement"];
                    let parent = parents[i % parents.len()].clone();
                    (vec![parent], None)
                }
                "detailed_spec" => {
                    let parents = &ids_by_layer["basic_spec"];
                    let parent = parents[i % parents.len()].clone();
                    // Every 7th detailed_spec item is inline-verified
                    // instead of relying on a unit_test document, exercising
                    // both coverage paths at scale.
                    let method = if i % 7 == 0 {
                        Some("manual".to_string())
                    } else {
                        None
                    };
                    (vec![parent], method)
                }
                _ => (vec![], None),
            };

            let verifies = match layer {
                "acceptance" => {
                    let targets = &ids_by_layer["requirement"];
                    vec![targets[i % targets.len()].clone()]
                }
                "system_test" => {
                    let targets = &ids_by_layer["basic_spec"];
                    vec![targets[i % targets.len()].clone()]
                }
                "unit_test" => {
                    let targets = &ids_by_layer["detailed_spec"];
                    vec![targets[i % targets.len()].clone()]
                }
                _ => vec![],
            };

            items.push(TraceItemInput {
                stable_id: id,
                doc_id,
                layer: Some(layer.to_string()),
                refines,
                verifies,
                method,
                has_test_refs: false,
                acceptance_labels: Vec::new(),
                derived: false,
                waived_axes: Vec::new(),
                def_hash: None,
                body_hash: None,
                link_baselines: std::collections::BTreeMap::new(),
                needs: None,
            });
        }
    }

    let mut runs_latest = HashMap::new();
    for layer in ["acceptance", "system_test", "unit_test"] {
        for (i, id) in ids_by_layer[layer].iter().enumerate() {
            let result = match i % 3 {
                0 => "pass",
                1 => "fail",
                _ => "not_run",
            };
            runs_latest.insert(id.clone(), result.to_string());
        }
    }
    // A handful of inline-verified detailed_spec items also get a run.
    for (i, id) in ids_by_layer["detailed_spec"].iter().enumerate() {
        if i % 7 == 0 {
            runs_latest.insert(id.clone(), "pass".to_string());
        }
    }

    let mut task_requirement_links = Vec::new();
    for layer in ["requirement", "basic_spec", "detailed_spec"] {
        for (i, id) in ids_by_layer[layer].iter().enumerate() {
            if i % 5 == 0 {
                task_requirement_links.push(TaskRequirementLink {
                    task_id: format!("t-{layer}-{i}"),
                    stable_id: id.clone(),
                    role: TaskLinkRole::Implements,
                    baseline_hash: None,
                });
            }
        }
    }

    TraceInput {
        items,
        task_requirement_links,
        task_doc_links: Vec::new(),
        layer_doc_ids: std::collections::HashSet::new(),
        runs_latest,
        stable_id_owners: HashMap::new(),
        // Left empty deliberately: auto-detection from item presence is
        // itself part of what's being timed.
        configured_layers: Vec::new(),
        profile_layers: Vec::new(),
        ..Default::default()
    }
}

fn layer_prefix(layer: &str) -> &'static str {
    match layer {
        "requirement" => "REQ",
        "basic_spec" => "SPEC",
        "detailed_spec" => "DS",
        "acceptance" => "AT",
        "system_test" => "ST",
        "unit_test" => "UT",
        _ => unreachable!(),
    }
}

#[test]
#[ignore]
fn trace_graph_build_under_1s_at_ja_scale_2500_items_30_docs() {
    let input = synthetic_input();

    // Warm-up (first call may pay one-time allocator warm-up cost).
    let _ = TraceGraph::build(&input);

    let start = Instant::now();
    let graph = TraceGraph::build(&input);
    let elapsed = start.elapsed();

    println!(
        "trace_graph_build_under_1s_at_ja_scale_2500_items_30_docs: {:.1} ms ({} items, {} docs)",
        elapsed.as_secs_f64() * 1000.0,
        N_ITEMS,
        N_DOCS
    );

    assert!(
        elapsed.as_secs_f64() < 1.0,
        "PR-7 (wiki/240 §6): trace derivation over {N_ITEMS} items / {N_DOCS} docs took {:.1} ms, budget is < 1000 ms",
        elapsed.as_secs_f64() * 1000.0
    );

    // Sanity: every item got a state, coverage was aggregated for every
    // in-use layer, and the graph is non-trivial (edges actually resolved,
    // not silently empty).
    for layer in [
        "requirement",
        "basic_spec",
        "detailed_spec",
        "acceptance",
        "system_test",
        "unit_test",
    ] {
        assert!(graph.is_layer_in_use(layer));
        let cov = graph
            .coverage()
            .get(layer)
            .unwrap_or_else(|| panic!("layer '{layer}' must have a coverage entry"));
        assert!(cov.total > 0, "layer '{layer}' must have items");
    }
    assert!(graph.state("REQ-00000").is_some());
    assert!(
        !graph.verified_by("REQ-00000").is_empty(),
        "REQ-00000 must have been wired to at least one acceptance verifier by synthetic_input"
    );
}
