//! Tier 2 performance benches (NFR-009, `wiki/240-performance-design.md`
//! §7): criterion micro-benchmarks for the read-path hot spots identified in
//! §3 (C1/C5/C6) — `read_all_docs`, frontmatter parse/serialize,
//! `lexsim::content_hash`, `build_task_index`, `find_task_dir_by_id`, and
//! `aggregate_requirements`. Manual/nightly only (not part of `cargo test`):
//!
//! ```text
//! cargo bench --bench docs_read
//! ```
//!
//! `aggregate_requirements` (also named in wiki/240 §7) is `pub(crate)` in
//! `src/mcp/handlers/docs.rs`, and `RequirementsSummary` (its return type)
//! is `pub(crate)` too — this bench binary is its own compilation unit and
//! only sees `pub` items, so it goes through
//! [`aggregate_requirements_bench_metrics`], a `#[doc(hidden)] pub` wrapper
//! (t370.7) that returns primitive counts rather than promoting the whole
//! `RequirementsSummary` nested type tree to `pub` just for a benchmark.
//!
//! Fixtures reuse the same deterministic generator as `tests/perf_budget.rs`
//! (`tests/support/perf_fixture.rs`) so Tier 1 and Tier 2 numbers are
//! comparable at the same declared scale.

#[path = "../tests/support/perf_fixture.rs"]
mod perf_fixture;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use perf_fixture::{generate, FixtureOpts, Lang};

use handoff_mcp::mcp::handlers::docs::aggregate_requirements_bench_metrics;
use handoff_mcp::storage::docs::frontmatter::{deserialize_frontmatter, serialize_frontmatter};
use handoff_mcp::storage::docs::{read_all_docs, read_all_docs_hashed, DocMetadata};
use handoff_mcp::storage::tasks::{build_task_index, find_task_dir_by_id};

/// M-scale fixture (1,000 tasks / 100 docs / 2,500 SubItems, wiki/240 §2) —
/// large enough that the C1/C5/C6 hot spots dominate, small enough to keep
/// `cargo bench` under a minute per group.
fn m_scale_project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let proj = tmp.path().join("proj");
    generate(&proj, &FixtureOpts::m()).expect("generate M-scale fixture");
    tmp
}

fn bench_read_all_docs(c: &mut Criterion) {
    let tmp = m_scale_project();
    let handoff_dir = tmp.path().join("proj").join(".handoff");
    c.bench_function("read_all_docs (M scale, 100 docs)", |b| {
        b.iter(|| {
            let docs = read_all_docs(black_box(&handoff_dir)).expect("read_all_docs");
            black_box(docs.len())
        })
    });
}

fn bench_frontmatter_round_trip(c: &mut Criterion) {
    let tmp = m_scale_project();
    let handoff_dir = tmp.path().join("proj").join(".handoff");
    // `read_all_docs` (P-M1, t370.8) deliberately leaves `content_hash: None`
    // on every returned doc — `serialize_frontmatter` refuses to serialize
    // that (the on-disk schema's `content_hash` is a required, always-
    // present field). `read_all_docs_hashed` pays the `lexsim::content_hash`
    // pass once, up front, so `doc.content_hash` is `Some` for both the
    // one-shot `serialize_frontmatter` call below and every iteration of the
    // "serialize"/"deserialize" bench functions.
    let docs = read_all_docs_hashed(&handoff_dir).expect("read_all_docs_hashed");
    let doc = docs
        .iter()
        .find(|d| !d.tags.is_empty() && d.doc_type == "spec")
        .expect("at least one requirement doc in the M-scale fixture");
    let yaml = serialize_frontmatter(doc).expect("serialize_frontmatter");

    let mut group = c.benchmark_group("frontmatter");
    group.bench_function("serialize", |b| {
        b.iter(|| serialize_frontmatter(black_box(doc)).expect("serialize_frontmatter"))
    });
    group.bench_function("deserialize", |b| {
        b.iter(|| {
            let parsed: DocMetadata =
                deserialize_frontmatter(black_box(&yaml), black_box(&doc.slug))
                    .expect("deserialize_frontmatter");
            black_box(parsed)
        })
    });
    group.finish();
}

fn bench_content_hash(c: &mut Criterion) {
    // Bodies at JA-scale density (wiki §2: JA body ~1.25 MB) for both
    // languages, so the ~9x tokenization cost gap (wiki §1) is visible in
    // the same benchmark group.
    let tmp = tempfile::tempdir().expect("tempdir");
    let proj_en = tmp.path().join("en");
    generate(
        &proj_en,
        &FixtureOpts {
            tasks: 10,
            docs: 2,
            subitems: 100,
            children: 3,
            seed: 1,
            lang: Lang::En,
        },
    )
    .expect("generate en fixture");
    let proj_ja = tmp.path().join("ja");
    generate(
        &proj_ja,
        &FixtureOpts {
            tasks: 10,
            docs: 2,
            subitems: 100,
            children: 3,
            seed: 1,
            lang: Lang::Ja,
        },
    )
    .expect("generate ja fixture");

    let docs_en = read_all_docs(&proj_en.join(".handoff")).expect("read_all_docs en");
    let docs_ja = read_all_docs(&proj_ja.join(".handoff")).expect("read_all_docs ja");
    let body_en =
        handoff_mcp::storage::docs::read_doc_body(&proj_en.join(".handoff"), &docs_en[0].slug)
            .expect("read_doc_body en")
            .expect("body present");
    let body_ja =
        handoff_mcp::storage::docs::read_doc_body(&proj_ja.join(".handoff"), &docs_ja[0].slug)
            .expect("read_doc_body ja")
            .expect("body present");

    let mut group = c.benchmark_group("content_hash");
    group.bench_function("en", |b| {
        b.iter(|| lexsim::content_hash(black_box(&body_en)))
    });
    group.bench_function("ja", |b| {
        b.iter(|| lexsim::content_hash(black_box(&body_ja)))
    });
    group.finish();
}

fn bench_build_task_index(c: &mut Criterion) {
    let tmp = m_scale_project();
    let tasks_dir = tmp.path().join("proj").join(".handoff").join("tasks");
    c.bench_function("build_task_index (M scale, 1000 tasks)", |b| {
        b.iter(|| {
            let (index, summary) =
                build_task_index(black_box(&tasks_dir), 10).expect("build_task_index");
            black_box((index.len(), summary.total))
        })
    });
}

fn bench_find_task_dir_by_id(c: &mut Criterion) {
    let tmp = m_scale_project();
    let tasks_dir = tmp.path().join("proj").join(".handoff").join("tasks");
    // Worst realistic case: a leaf under the *last* top-level parent (M
    // scale: 1,000 tasks / 9 children each -> t1..t100, so the last node is
    // t100.9), so the recursive scan (C5, wiki §3) has to walk most of the
    // tree before finding a match.
    let target = "t100.9";
    c.bench_function("find_task_dir_by_id (M scale, recent leaf)", |b| {
        b.iter(|| {
            let found = find_task_dir_by_id(black_box(&tasks_dir), black_box(target))
                .expect("find_task_dir_by_id");
            black_box(found)
        })
    });
}

fn bench_aggregate_requirements(c: &mut Criterion) {
    let tmp = m_scale_project();
    let handoff_dir = tmp.path().join("proj").join(".handoff");
    // `aggregate_requirements` never reads `content_hash` (only
    // `verification.items[].sub_items[]`), so the plain (unhashed)
    // `read_all_docs` is the right fixture read here, same as every other
    // `DocSet`-based caller (wiki/240 §4 P-M1).
    let docs = read_all_docs(&handoff_dir).expect("read_all_docs");
    c.bench_function("aggregate_requirements (M scale, 100 docs)", |b| {
        b.iter(|| black_box(aggregate_requirements_bench_metrics(black_box(&docs))))
    });
}

criterion_group!(
    benches,
    bench_read_all_docs,
    bench_frontmatter_round_trip,
    bench_content_hash,
    bench_build_task_index,
    bench_find_task_dir_by_id,
    bench_aggregate_requirements,
);
criterion_main!(benches);
