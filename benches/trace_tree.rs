//! Requirement-tree performance bench (wiki/300-requirement-tree-design.md,
//! SPEC-TREE-015, t-rtree.7): `handoff_trace_report` and `handoff_trace_tree`
//! over the deterministic 3,000-item synthetic corpus of
//! `benches/support/tree_synth.rs` (DAG with multiple parents, composites,
//! decisions, a depth-25 chain, three layers).
//!
//! Each call goes through the real `handoff-mcp` binary over stdio JSON-RPC,
//! so the numbers include loading the documents from disk, building the
//! trace graph + tree roll-up, and serializing the response. Before the timed
//! runs the bench prints the response sizes and the size of the persisted
//! `_trace_report.json`. Manual/nightly only (not part of `cargo test`):
//!
//! ```text
//! cargo bench --bench trace_tree
//! ```
//!
//! Output determinism is asserted by `tests/trace_tree_determinism.rs`.

#[path = "support/mcp_client.rs"]
mod mcp_client;
#[path = "support/tree_synth.rs"]
mod tree_synth;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use mcp_client::Server;
use serde_json::{json, Value};

const REPORT_FILE: &str = ".handoff/docs/_trace_report.json";

fn bench_tree(c: &mut Criterion) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut server = Server::start(tmp.path());
    server.load_synthetic();

    let calls: [(&str, &str, Value); 4] = [
        ("trace_report", "handoff_trace_report", json!({})),
        (
            "trace_report (include_items)",
            "handoff_trace_report",
            json!({"include_items": true}),
        ),
        ("trace_tree (full)", "handoff_trace_tree", json!({})),
        (
            "trace_tree (depth 3)",
            "handoff_trace_tree",
            json!({"depth": 3}),
        ),
    ];

    println!(
        "synthetic corpus: {} items in {} documents",
        tree_synth::TOTAL_ITEMS,
        tree_synth::TOTAL_DOCS
    );
    for (label, tool, args) in &calls {
        let bytes = server.call_text(tool, args.clone()).len();
        println!("{label}: response {bytes} bytes");
    }
    // Every `handoff_trace_report` call (re)writes the persisted file.
    let persisted = std::fs::metadata(server.project().join(REPORT_FILE))
        .expect("_trace_report.json")
        .len();
    println!("_trace_report.json: {persisted} bytes");

    let mut group = c.benchmark_group("trace_tree_3000");
    group.sample_size(20);
    for (label, tool, args) in &calls {
        group.bench_function(*label, |b| {
            b.iter(|| black_box(server.call_text(tool, args.clone())).len())
        });
    }
    group.finish();
}

criterion_group!(benches, bench_tree);
criterion_main!(benches);
