//! Deterministic synthetic `.handoff` project generator for performance
//! benchmarks (`tests/perf_budget.rs`, NFR-009, wiki/240-performance-design.md
//! §7). Rust port of the throwaway prototype `tmp/260903-mperf/gen_fixture.py`
//! — same on-disk shape and well-known ids, but built through the storage
//! layer's own public writer functions (`write_task`, `write_doc`,
//! `write_doc_body`, `write_config`) instead of hand-rolled JSON/YAML, so the
//! fixture can never drift from the real on-disk schema.
//!
//! Determinism: for a fixed `FixtureOpts` (including `seed`), [`generate`]
//! produces byte-identical files every run — see the `deterministic_*` tests
//! at the bottom of this file. The RNG is a private xorshift64 (mirrors
//! `tests/context_corpus_bench.rs`), not `rand`, specifically so the output
//! never depends on an upstream crate's algorithm changing between versions.
//!
//! This file is shared via `#[path = "support/perf_fixture.rs"]` /
//! `#[path = "../tests/support/perf_fixture.rs"]` across several independent
//! test/bench binaries (`tests/perf_budget.rs`, `benches/docs_read.rs`, and
//! this file's own `#[cfg(test)]` unit tests), each of which is its own
//! compilation unit and only exercises a subset of this module's public
//! surface — so per-target dead-code warnings here are expected, not a sign
//! of unused production code.
#![allow(dead_code)]

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use handoff_mcp::storage::config::{write_config, Config};
use handoff_mcp::storage::docs::model::{DocMetadata, SubItem, Verification, VerificationItem};
use handoff_mcp::storage::docs::{write_doc, write_doc_body};
use handoff_mcp::storage::tasks::{write_task, DoneCriterion, Schedule, TaskData, TaskLink};

const TS: &str = "2026-09-01T00:00:00.000000000+00:00";

const JA_PHRASES: &[&str] = &[
    "システムは入力を検証し、",
    "ユーザーが操作した場合に",
    "エラーを表示すること。",
    "設定ファイルから読み込む。",
    "タスクの状態を更新する際に",
    "集計結果を再計算する。",
    "一秒以内に応答すること。",
    "層ごとにカバレッジを算出する。",
];

const STATUS_WEIGHTS: &[(&str, u32)] = &[
    ("done", 60),
    ("todo", 25),
    ("in_progress", 10),
    ("review", 5),
];

/// Body language for generated document text — English is ~9x cheaper to
/// tokenize than Japanese under lexsim (wiki/240 §1), so `Lang::Ja` fixtures
/// exercise the JA-heavy NFR-003 scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Ja,
}

#[derive(Debug, Clone)]
pub struct FixtureOpts {
    pub tasks: usize,
    pub docs: usize,
    pub subitems: usize,
    pub children: usize,
    pub seed: u64,
    pub lang: Lang,
}

impl FixtureOpts {
    /// Scale "S": 200 tasks / 20 docs / 500 requirement SubItems
    /// (wiki/240 §2 scale table).
    pub fn s() -> Self {
        Self {
            tasks: 200,
            docs: 20,
            subitems: 500,
            children: 9,
            seed: 1,
            lang: Lang::En,
        }
    }

    /// Scale "M": 1,000 tasks / 100 docs / 2,500 SubItems.
    pub fn m() -> Self {
        Self {
            tasks: 1_000,
            docs: 100,
            subitems: 2_500,
            children: 9,
            seed: 1,
            lang: Lang::En,
        }
    }

    /// Scale "L": 3,000 tasks / 100 docs / 2,500 SubItems.
    pub fn l() -> Self {
        Self {
            tasks: 3_000,
            docs: 100,
            subitems: 2_500,
            children: 9,
            seed: 1,
            lang: Lang::En,
        }
    }

    /// Scale "JA": same volume as M/L's requirement corpus but with only 20
    /// docs and Japanese body text (NFR-003).
    pub fn ja() -> Self {
        Self {
            tasks: 200,
            docs: 20,
            subitems: 2_500,
            children: 9,
            seed: 1,
            lang: Lang::Ja,
        }
    }
}

/// Well-known ids the bench driver needs to target specific requests
/// (mirrors `bench_meta.json` from the Python prototype).
#[derive(Debug, Clone)]
pub struct FixtureMeta {
    /// Leaf task linked to up to 10 requirement SubItems across docs 0/1;
    /// its co-linked tasks are forced to `done` so toggling its status
    /// changes derived `dev_stage` on every call (worst realistic path).
    pub hot_req_task: String,
    pub hot_req_ids: Vec<String>,
    /// Every doc slug that actually received a requirement-SubItem link to
    /// `hot_req_task` (in practice docs 0 and 1 — see `hot_req_task`'s own
    /// doc comment above — but derived from the real link data rather than
    /// hardcoded, so this stays correct if the generation logic above ever
    /// changes which/how many docs `hot_req_task` links into). A call that
    /// mutates `hot_req_task`'s status/requirement_ids and propagates
    /// `dev_stage` (`update_task_status_with_links`,
    /// `update_task_requirement_ids_toggle`) legitimately reads/writes every
    /// doc in this list, not just `doc_slug` — `io_budget_bytes` in
    /// `tests/perf_budget.rs` (PR-8, t370.12) budgets against this full set.
    pub hot_req_doc_slugs: Vec<String>,
    /// Every other task id `hot_req_task` shares a `SubItem.task_ids` entry
    /// with (deduped) — `propagate_dev_stage_for_task`
    /// (`src/mcp/handlers/docs.rs`) must read each of these tasks' own
    /// status file too (min-of-linked-tasks dev_stage computation), not just
    /// `hot_req_task`'s, so `io_budget_bytes` (PR-8, t370.12) budgets for
    /// their bytes as well.
    pub hot_colinked_tasks: Vec<String>,
    /// Leaf task with no requirement links (baseline for PR-2/PR-9).
    pub plain_task: String,
    /// An unlinked stable_id, for requirement_ids add/remove round-trips.
    pub extra_stable_id: Option<String>,
    pub doc_slug: String,
    pub doc_id: String,
    /// Byte length of `doc_slug`'s generated markdown body at fixture
    /// generation time (frontmatter excluded) — t370.15
    /// (wiki/240-performance-design.md §6 PR-4): `tests/perf_budget.rs`'s
    /// `doc_update_section` budget scales with this value so the budget
    /// itself is a deterministic function of the fixture, not of a measured
    /// timing. English S/M/L fixtures produce ~6.4KB here; the JA fixture
    /// produces ~39.9KB (same section/subitem structure, but Japanese text is
    /// far more expensive per byte for `lexsim::content_hash` to tokenize —
    /// see the correction note on `update_task_status_with_links` in
    /// `tests/perf_budgets.toml`).
    pub doc_body_bytes: usize,
    /// `fragment_seq` / `sub_items` indices used for `doc_verify` calls
    /// against `doc_slug`/`doc_id`.
    pub verify_seq: usize,
    pub verify_idx_a: usize,
    pub verify_idx_b: usize,
    pub section_seq: usize,
    /// M1 t360.6 (wiki/220-vmodel-integration-design.md §2.4, wiki/240 §5-3):
    /// a small, deliberately scale-independent layer document (fixed
    /// [`LAYER_ITEM_COUNT`] items regardless of `FixtureOpts`), used by
    /// `tests/perf_budget.rs`'s `doc_save_layer_metadata`/
    /// `doc_update_section_layer` ops. Scale-independent by design: layer
    /// sync's cost should be dominated by its own item count, not by the
    /// unrelated S/M/L/JA scale knobs (`tasks`/`docs`/`subitems`) — a fixed
    /// size lets those two ops budget the same `ms` at every scale, instead
    /// of inheriting `doc_update_section`'s existing JA `expected_fail` gap
    /// (that gap is about `lexsim::content_hash`'s per-byte JA tokenization
    /// cost on a *large* body, orthogonal to what this task's `body_raw_hash`
    /// optimizes — see wiki/220 §2.4's own scoping of that hash to "is the
    /// body byte-identical since the last sync", not general write-time
    /// hashing).
    pub layer_doc_slug: String,
    pub layer_doc_id: String,
    /// The section (`seq`) covering the whole body — `doc_update_section_layer`
    /// replaces it wholesale, same shape as the plain `doc_update_section` op.
    pub layer_section_seq: usize,
    /// The body language used for [`FixtureMeta::layer_doc_slug`] — mirrors
    /// `FixtureOpts::lang` so `doc_update_section_layer` (`tests/perf_budget.rs`)
    /// can regenerate a same-language body per rep via [`layer_document_body`]
    /// without needing `FixtureOpts` itself threaded through `run_ops`.
    pub layer_lang: Lang,
    /// M1 t360.10/t360.11 (wiki/240-performance-design.md §6 PR-7, NFR-003:
    /// "2,500 項目 / 30 文書"): a dedicated, scale-independent set of 30
    /// layer documents (10 `requirement` / 10 `basic_spec` / 5 `acceptance` /
    /// 5 `system_test`, 2,500 items total — see [`TRACE_*`] constants) used
    /// by `tests/perf_budget.rs`'s `trace_report`/`trace_slice` ops. Added on
    /// top of every S/M/L/JA project (same rationale as `layer_doc_slug`:
    /// this scale is fixed by the spec, not by the S/M/L/JA table), and
    /// never pre-synced (`verification: None`, matching `layer_doc_slug`) —
    /// the first (untimed warm-up) call does the one real
    /// `sync_layer_items` parse of all 2,500 items; every timed rep after
    /// that hits `sync_layer_items_if_needed`'s raw-hash short-circuit.
    pub trace_task_id: String,
    /// A stable_id in the fixture (`REQ-00-000`) with a refining child
    /// (`SPEC-00-000`) and a verifier (`AT-00-000`) — a non-trivial starting
    /// point for `trace_slice`.
    pub trace_slice_item_id: String,
}

/// [`FixtureMeta::trace_task_id`]/[`FixtureMeta::trace_slice_item_id`]'s
/// fixture scale (wiki/240-performance-design.md §6 PR-7's "2,500 項目 / 30
/// 文書"): 10 `requirement` docs x 100 items, 10 paired `basic_spec` docs x
/// 100 items (item `k` in spec doc `d` refines `REQ-{d:02}-{k:03}`), 5
/// `acceptance` docs x 60 items (verifies `REQ-{d:02}-{k:03}` for `d` in
/// 0..5), 5 `system_test` docs x 40 items (verifies `SPEC-{d:02}-{k:03}` for
/// `d` in 0..5) — 30 docs, 1000+1000+300+200 = 2,500 items. The
/// partial-coverage split (only the first 5 of 10 requirement/basic_spec
/// docs get right-side verifiers) deliberately exercises both `covered` and
/// `uncovered` coverage/gap paths, not just an all-green graph.
const TRACE_REQ_DOCS: usize = 10;
const TRACE_REQ_ITEMS_PER_DOC: usize = 100;
const TRACE_SPEC_DOCS: usize = 10;
const TRACE_SPEC_ITEMS_PER_DOC: usize = 100;
const TRACE_AT_DOCS: usize = 5;
const TRACE_AT_ITEMS_PER_DOC: usize = 60;
const TRACE_ST_DOCS: usize = 5;
const TRACE_ST_ITEMS_PER_DOC: usize = 40;

/// M2-05 rework (review round 1, MAJOR, wiki/240-performance-design.md §7's
/// "suspect（上流を変えた文書）"): a small, dedicated, scale-independent set
/// of `basic_spec` <- `requirement` links (`SPEC-99-NNN` refines
/// `REQ-99-NNN`) added on top of the 2,500-item trace fixture above,
/// specifically so `tests/perf_budget.rs`'s `trace_suspect_list`/
/// `trace_suspect_clear`/`trace_suspect_baseline` ops have real suspects to
/// find instead of a fixture that never makes anything suspect (the pre-fix
/// gap this task's rework closes — see `tests/perf_budgets.toml`'s
/// `trace_suspect_clear` entry). [`generate`] writes only the *original*
/// (`variant = 0`) requirement text, already in sync; `run_ops` makes these
/// suspect afterward via one untimed `handoff_doc_save` full-body rewrite of
/// [`suspect_req_doc_slug`] to [`suspect_req_body`]'s `variant = 1` — a
/// real, resync-triggering edit (unlike a raw `.md` hand-edit, which
/// `action="list"`/`action="baseline"(dry_run)` deliberately never resync,
/// E6) — so `list`'s own measured probe also sees real, already-synced
/// suspects.
///
/// Sized so `trace_suspect_clear`'s monotonic call counter never overruns
/// the seeded links, in *either* of the two rep counts `tests/perf_budget.rs`
/// runs `run_ops` at: `REPS` (7, the plain per-op budget) and `RATIO_REPS`
/// (21, `run_ops` also runs at this rep count for *every* `check_scale_ratio`
/// call, regardless of which op that ratio check names — t360.20.26, M2-S4
/// reviewer). 1 warm-up + `max(REPS, RATIO_REPS)` timed calls = 22 distinct
/// links needed; see `tests/perf_budget.rs`'s
/// `_SUSPECT_LINK_COUNT_COVERS_MAX_OF_REPS_AND_RATIO_REPS` for the actual
/// compile-time cross-check against those two constants (this constant can't
/// reference them directly — this module is also `#[path]`-shared by
/// `derived_summary_write_discipline.rs`, which never defines `RATIO_REPS`).
pub const SUSPECT_LINK_COUNT: usize = 22;

/// Static floor mirroring [`SUSPECT_LINK_COUNT`]'s doc comment: `max(REPS(7),
/// RATIO_REPS(21)) + 1 = 22` — a compile-time static assert (evaluated for
/// every target this file is compiled into, not gated on `cfg(test)`) so the
/// fixture can never silently shrink below what the real `REPS`/`RATIO_REPS`
/// values in `tests/perf_budget.rs` currently require. The literal `22`
/// here and the cross-check against the live `REPS`/`RATIO_REPS` constants
/// in `tests/perf_budget.rs`'s own static assert must both be updated
/// together if either constant changes.
const _SUSPECT_LINK_COUNT_COVERS_PERF_BUDGET_REPS: () = assert!(
    SUSPECT_LINK_COUNT >= 22,
    "SUSPECT_LINK_COUNT must be >= max(REPS(7), RATIO_REPS(21)) + 1 = 22"
);

pub fn suspect_req_doc_slug() -> &'static str {
    "bench-trace-suspect-req"
}

pub fn suspect_req_doc_id() -> &'static str {
    "doc-20260901-100000-4000"
}

fn suspect_spec_doc_slug() -> &'static str {
    "bench-trace-suspect-spec"
}

/// t360.20.26: `pub` (not just used internally by [`generate_suspect_seed_docs`])
/// because `tests/perf_budget.rs`'s `run_ops` also needs it to target its own
/// sequenced, untimed `handoff_doc_save` call that establishes this doc's
/// real cross-document baselines before either doc is ever touched by a bulk
/// multi-document resync — see that call site's own doc comment for why.
pub fn suspect_spec_doc_id() -> &'static str {
    "doc-20260901-100000-4001"
}

pub fn suspect_req_id(n: usize) -> String {
    format!("REQ-99-{n:03}")
}

pub fn suspect_spec_id(n: usize) -> String {
    format!("SPEC-99-{n:03}")
}

/// [`SUSPECT_LINK_COUNT`]-item body for [`suspect_req_doc_slug`]. `variant`
/// is folded into every item's statement text (mirrors [`layer_document_body`]'s
/// own `variant` parameter) so `variant = 0` (written once by [`generate`])
/// and `variant = 1` (written once, untimed, by `tests/perf_budget.rs`'s
/// `run_ops` right before the `trace_suspect_*` ops) are byte-different —
/// the second write is what actually makes every `SPEC-99-NNN` link
/// suspect (its recorded baseline is `variant = 0`'s hash).
pub fn suspect_req_body(lang: Lang, variant: usize) -> String {
    let mut rng = Xorshift::new(0x5A17_BA5E ^ (variant as u64).wrapping_mul(0x9E37));
    let mut body = String::from("# Trace bench suspect-seed requirement doc\n\n");
    for k in 0..SUSPECT_LINK_COUNT {
        body.push_str(&format!(
            "### {} Synthetic suspect-seed requirement {k}\n\n- priority: P{}\n\n",
            suspect_req_id(k),
            k % 4
        ));
        if lang == Lang::Ja {
            let mut line = format!("rev{variant}: ");
            for _ in 0..3 {
                line.push_str(rng.pick(JA_PHRASES));
            }
            body.push_str(&line);
        } else {
            body.push_str(&format!(
                "rev{variant}: synthetic statement text. {}",
                "lorem ipsum ".repeat(6)
            ));
        }
        body.push_str("\n\n");
    }
    body
}

/// t360.20.26: `pub` for the same reason as [`suspect_spec_doc_id`] — needed
/// by `tests/perf_budget.rs`'s own sequenced `handoff_doc_save` call for this
/// document (the body is unchanged between `generate`'s raw write and that
/// call; the point is to force a real, individually-flushed sync, not to
/// change the content).
pub fn suspect_spec_body(lang: Lang) -> String {
    let mut rng = Xorshift::new(0x5A17_BA5E ^ 0xBEEF);
    let mut body = String::from("# Trace bench suspect-seed basic_spec doc\n\n");
    for k in 0..SUSPECT_LINK_COUNT {
        let refines = format!("refines: {}", suspect_req_id(k));
        body.push_str(&trace_item_block("SPEC", 99, k, &[refines], lang, &mut rng));
    }
    body
}

/// Writes [`SUSPECT_LINK_COUNT`]'s two dedicated documents (original,
/// in-sync `variant = 0` text) — see [`SUSPECT_LINK_COUNT`]'s doc comment.
fn generate_suspect_seed_docs(handoff_dir: &Path, lang: Lang) -> Result<()> {
    write_trace_doc(
        handoff_dir,
        suspect_req_doc_id(),
        suspect_req_doc_slug(),
        "requirement",
        &suspect_req_body(lang, 0),
    )?;
    write_trace_doc(
        handoff_dir,
        suspect_spec_doc_id(),
        suspect_spec_doc_slug(),
        "basic_spec",
        &suspect_spec_body(lang),
    )?;
    Ok(())
}

/// One §2.2-syntax item block: heading + `- priority:` + any `extra_attrs`
/// (e.g. `refines:`/`verifies:`) + a one-line statement. Mirrors
/// `layer_document_body`'s block shape but is parameterized over an
/// id prefix/doc index/item index so [`generate_trace_scale_docs`] can build
/// all four roles from one helper.
fn trace_item_block(
    prefix: &str,
    d: usize,
    k: usize,
    extra_attrs: &[String],
    lang: Lang,
    rng: &mut Xorshift,
) -> String {
    let mut block = format!("### {prefix}-{d:02}-{k:03} Synthetic {prefix} item {d}-{k}\n\n");
    block.push_str(&format!("- priority: P{}\n", k % 4));
    for attr in extra_attrs {
        block.push_str(&format!("- {attr}\n"));
    }
    block.push('\n');
    if lang == Lang::Ja {
        let mut line = String::new();
        for _ in 0..3 {
            line.push_str(rng.pick(JA_PHRASES));
        }
        block.push_str(&line);
    } else {
        block.push_str("Synthetic statement text. ");
        block.push_str(&"lorem ipsum ".repeat(6));
    }
    block.push_str("\n\n");
    block
}

/// Writes [`FixtureMeta::trace_task_id`]'s 30-document, 2,500-item trace
/// fixture (see the `TRACE_*` constants' doc comment) and returns the
/// stable_id `trace_slice_item_id` should point at.
fn generate_trace_scale_docs(handoff_dir: &Path, lang: Lang) -> Result<String> {
    let mut rng = Xorshift::new(0x0007_A0E5_CA1E);

    for d in 0..TRACE_REQ_DOCS {
        let slug = format!("bench-trace-req-{d:02}");
        let doc_id = format!("doc-20260901-100000-{d:04}");
        let mut body = format!("# Trace bench requirement doc {d}\n\n");
        for k in 0..TRACE_REQ_ITEMS_PER_DOC {
            body.push_str(&trace_item_block("REQ", d, k, &[], lang, &mut rng));
        }
        write_trace_doc(handoff_dir, &doc_id, &slug, "requirement", &body)?;
    }
    for d in 0..TRACE_SPEC_DOCS {
        let slug = format!("bench-trace-spec-{d:02}");
        let doc_id = format!("doc-20260901-100000-{:04}", 1000 + d);
        let mut body = format!("# Trace bench basic_spec doc {d}\n\n");
        for k in 0..TRACE_SPEC_ITEMS_PER_DOC {
            let refines = format!("refines: REQ-{d:02}-{k:03}");
            body.push_str(&trace_item_block("SPEC", d, k, &[refines], lang, &mut rng));
        }
        write_trace_doc(handoff_dir, &doc_id, &slug, "basic_spec", &body)?;
    }
    for d in 0..TRACE_AT_DOCS {
        let slug = format!("bench-trace-at-{d:02}");
        let doc_id = format!("doc-20260901-100000-{:04}", 2000 + d);
        let mut body = format!("# Trace bench acceptance doc {d}\n\n");
        for k in 0..TRACE_AT_ITEMS_PER_DOC {
            let verifies = format!("verifies: REQ-{d:02}-{k:03}");
            body.push_str(&trace_item_block(
                "AT",
                d,
                k,
                &[verifies, "method: manual".to_string()],
                lang,
                &mut rng,
            ));
        }
        write_trace_doc(handoff_dir, &doc_id, &slug, "acceptance", &body)?;
    }
    for d in 0..TRACE_ST_DOCS {
        let slug = format!("bench-trace-st-{d:02}");
        let doc_id = format!("doc-20260901-100000-{:04}", 3000 + d);
        let mut body = format!("# Trace bench system_test doc {d}\n\n");
        for k in 0..TRACE_ST_ITEMS_PER_DOC {
            let verifies = format!("verifies: SPEC-{d:02}-{k:03}");
            body.push_str(&trace_item_block(
                "ST",
                d,
                k,
                &[verifies, "method: auto".to_string()],
                lang,
                &mut rng,
            ));
        }
        write_trace_doc(handoff_dir, &doc_id, &slug, "system_test", &body)?;
    }

    Ok("REQ-00-000".to_string())
}

fn write_trace_doc(
    handoff_dir: &Path,
    doc_id: &str,
    slug: &str,
    layer: &str,
    body: &str,
) -> Result<()> {
    let mut doc = DocMetadata::new(
        doc_id.to_string(),
        slug.to_string(),
        format!("Trace bench {layer} doc"),
        "spec".to_string(),
        TS.to_string(),
    );
    doc.layer = Some(layer.to_string());
    write_doc_body(handoff_dir, slug, body)?;
    write_doc(handoff_dir, &doc)?;
    Ok(())
}

/// Fixed item count for [`FixtureMeta::layer_doc_slug`]'s body — see that
/// field's doc comment for why this does not scale with `FixtureOpts`.
pub const LAYER_ITEM_COUNT: usize = 30;

/// Seed for [`layer_document_body`]'s internal RNG — shared between
/// `generate`'s initial write and `tests/perf_budget.rs`'s
/// `doc_update_section_layer` op (which regenerates a fresh `variant` each
/// rep) so both draw from the same deterministic phrase sequence.
pub const LAYER_BODY_SEED: u64 = 0x1357_9BDF;

/// Builds a §2.2-syntax ("wiki/220-vmodel-integration-design.md") Markdown
/// body with [`LAYER_ITEM_COUNT`] `SPEC-NNN` items (heading + `- priority:`
/// attribute + one statement line each). `variant` is folded into the
/// statement text so successive calls with different `variant`s (e.g. one
/// per `doc_update_section_layer` rep) produce byte-different bodies —
/// otherwise a rep would rewrite the exact content a prior sync already
/// produced, an unrealistically cheap case for `sync_layer_items`'s
/// re-parse cost.
pub fn layer_document_body(lang: Lang, seed: u64, variant: usize) -> String {
    let mut rng = Xorshift::new(seed ^ (variant as u64).wrapping_mul(0x9E37));
    let mut body = String::from("# Bench layer document\n\n");
    for k in 0..LAYER_ITEM_COUNT {
        body.push_str(&format!(
            "### SPEC-{k:03} Synthetic requirement {k}\n\n- priority: P{}\n\n",
            k % 4
        ));
        if lang == Lang::Ja {
            let mut line = format!("rev{variant}: ");
            for _ in 0..4 {
                line.push_str(rng.pick(JA_PHRASES));
            }
            body.push_str(&line);
        } else {
            body.push_str(&format!(
                "rev{variant}: synthetic statement text. {}",
                "lorem ipsum ".repeat(8)
            ));
        }
        body.push_str("\n\n");
    }
    body
}

/// Deterministic xorshift64 PRNG — same choice as
/// `tests/context_corpus_bench.rs`, so fixture output never depends on an
/// external `rand` crate's algorithm/version.
struct Xorshift(u64);

impl Xorshift {
    fn new(seed: u64) -> Self {
        Xorshift(seed ^ 0x9E3779B97F4A7C15)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() as usize) % n.max(1)
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn weighted_status(&mut self) -> &'static str {
        let total: u32 = STATUS_WEIGHTS.iter().map(|(_, w)| w).sum();
        let mut roll = (self.next_u64() % total as u64) as u32;
        for (name, weight) in STATUS_WEIGHTS {
            if roll < *weight {
                return name;
            }
            roll -= weight;
        }
        STATUS_WEIGHTS[0].0
    }
}

struct TaskNode {
    id: String,
    parent: Option<String>,
}

/// Generate a deterministic `.handoff` project under `proj_dir` (created
/// fresh — any existing contents are removed first) and return the
/// well-known ids the bench driver needs.
pub fn generate(proj_dir: &Path, opts: &FixtureOpts) -> Result<FixtureMeta> {
    if proj_dir.exists() {
        fs::remove_dir_all(proj_dir)
            .with_context(|| format!("removing stale fixture dir {}", proj_dir.display()))?;
    }
    let handoff_dir = proj_dir.join(".handoff");
    for sub in ["tasks", "docs", "sessions", "memory", "referrals", "agents"] {
        fs::create_dir_all(handoff_dir.join(sub))?;
    }
    write_config(
        &handoff_dir.join("config.toml"),
        &Config::new("bench", "perf fixture"),
    )?;

    let mut rng = Xorshift::new(opts.seed);

    // ---- task tree: top-level parents, each with `children` leaf children.
    let per = opts.children + 1;
    let n_top = (opts.tasks / per).max(1);
    let mut nodes: Vec<TaskNode> = Vec::with_capacity(opts.tasks);
    let mut status_of: HashMap<String, &'static str> = HashMap::new();
    let mut top = 0usize;
    while nodes.len() < opts.tasks {
        top += 1;
        let parent_id = format!("t{top}");
        nodes.push(TaskNode {
            id: parent_id.clone(),
            parent: None,
        });
        status_of.insert(parent_id.clone(), "done");
        if top <= n_top {
            for c in 1..=opts.children {
                if nodes.len() >= opts.tasks {
                    break;
                }
                let cid = format!("{parent_id}.{c}");
                let st = rng.weighted_status();
                status_of.insert(cid.clone(), st);
                nodes.push(TaskNode {
                    id: cid,
                    parent: Some(parent_id.clone()),
                });
            }
        }
    }

    let leaves: Vec<&TaskNode> = {
        let with_parent: Vec<&TaskNode> = nodes.iter().filter(|n| n.parent.is_some()).collect();
        if with_parent.is_empty() {
            nodes.iter().collect()
        } else {
            with_parent
        }
    };
    // Well-known tasks come from the last leaves (recent-task realism: a
    // recursive `find_task_dir_by_id` scan has to walk most of the tree).
    let hot = leaves[leaves.len() - 1].id.clone();
    let plain = leaves[leaves.len() - 2].id.clone();
    status_of.insert(hot.clone(), "todo");
    status_of.insert(plain.clone(), "todo");

    // ---- docs & requirement sub_items ----
    let n_req_docs = if opts.docs >= 4 {
        (opts.docs / 2).max(1)
    } else {
        opts.docs
    };
    let per_doc = opts.subitems / n_req_docs.max(1);
    let rem = opts.subitems.saturating_sub(per_doc * n_req_docs);

    let link_pool: Vec<String> = leaves
        .iter()
        .map(|n| n.id.clone())
        .filter(|id| id != &hot && id != &plain)
        .collect();

    let mut links_by_task: HashMap<String, Vec<TaskLink>> = HashMap::new();
    let mut hot_ids: Vec<String> = Vec::new();
    let mut hot_doc_slugs: Vec<String> = Vec::new();
    let mut extra_stable: Option<String> = None;
    let mut docs_meta: Vec<(String, String)> = Vec::with_capacity(opts.docs); // (slug, id)
    let mut hot_colinked: Vec<String> = Vec::new();
    // Section count of doc 0, used below to pick a `section_seq` for
    // `doc_update_section` that's guaranteed to exist. `PR-9`'s `scale_ratio_d`
    // holds `subitems` fixed while scaling `docs` up, which shrinks `per_doc`
    // (and therefore doc 0's section count) well below the historical
    // default of 3 — see `doc0_sections` below.
    let mut doc0_sections = 1usize;
    // t370.15 (PR-4, wiki/240-performance-design.md §6): byte length of doc
    // 0's generated markdown body (the exact text `doc_update_section`'s
    // write-time `lexsim::content_hash` pass tokenizes) — captured here at
    // generation time so `tests/perf_budget.rs`'s size-scaled `doc_update_section`
    // budget is a deterministic function of the fixture, never of a measured
    // timing. Frontmatter YAML is intentionally excluded (it isn't part of
    // the hashed body).
    let mut doc0_body_bytes = 0usize;

    for di in 0..opts.docs {
        let slug = format!("bench-doc-{di:03}");
        let doc_id = format!("doc-20260901-000000-{di:06}");
        let is_req = di < n_req_docs;
        let cat = format!("C{di:02}");
        let n_items = if is_req {
            per_doc + usize::from(di < rem)
        } else {
            0
        };
        let sections = if is_req {
            n_items.div_ceil(10).max(1)
        } else {
            12
        };
        if di == 0 {
            doc0_sections = sections;
        }

        let mut body = format!("# Bench document {di}\n\n");
        body.push_str(&"Preamble text. ".repeat(20));
        body.push('\n');
        let mut verification_items = Vec::new();
        let mut idx_global = 0usize;
        for s in 1..=sections {
            body.push_str(&format!("## {s}. Section {s}\n\n"));
            for k in 0..10 {
                if opts.lang == Lang::Ja {
                    let mut line = format!("- 要件{k}: ");
                    for _ in 0..4 {
                        line.push_str(rng.pick(JA_PHRASES));
                    }
                    body.push_str(&line);
                } else {
                    body.push_str(&format!(
                        "- requirement text line {k} {}",
                        "lorem ipsum ".repeat(8)
                    ));
                }
                body.push('\n');
            }
            body.push('\n');

            if is_req {
                let mut sub_items = Vec::new();
                for k in 0..10 {
                    if idx_global >= n_items {
                        break;
                    }
                    let sid = format!("{cat}-FR-{s}.{k}");
                    let mut sub = SubItem {
                        index: k,
                        description: format!("FR {s}.{k} synthetic requirement"),
                        status: "pending".to_string(),
                        category: "requirement".to_string(),
                        stable_id: Some(sid.clone()),
                        priority: Some(["P0", "P1", "P2", "P3"][rng.below(4)].to_string()),
                        dev_stage: Some("not_started".to_string()),
                        ..SubItem::default()
                    };
                    let mut tids: Vec<String> = Vec::new();
                    if di < 2 && s == 1 && k < 5 {
                        tids.push(hot.clone());
                        tids.push(rng.pick(&link_pool).clone());
                        hot_ids.push(sid.clone());
                        if !hot_doc_slugs.contains(&slug) {
                            hot_doc_slugs.push(slug.clone());
                        }
                    } else if di == 0 && s == 2 && k == 0 {
                        extra_stable = Some(sid.clone());
                    } else {
                        tids.push(rng.pick(&link_pool).clone());
                        if rng.unit() < 0.2 {
                            tids.push(rng.pick(&link_pool).clone());
                        }
                    }
                    if !tids.is_empty() {
                        tids.sort();
                        tids.dedup();
                        for t in &tids {
                            links_by_task.entry(t.clone()).or_default().push(TaskLink {
                                target: doc_id.clone(),
                                link_type: "requirement".to_string(),
                                label: Some(sid.clone()),
                                ..Default::default()
                            });
                            if tids.contains(&hot) && t != &hot {
                                hot_colinked.push(t.clone());
                            }
                        }
                        sub.task_ids = tids;
                    }
                    sub_items.push(sub);
                    idx_global += 1;
                }
                verification_items.push(VerificationItem {
                    fragment_seq: Some(s),
                    heading: format!("{s}. Section {s}"),
                    status: "pending".to_string(),
                    category: "section".to_string(),
                    sub_items,
                    ..zero_verification_item()
                });
            }
        }

        let mut doc = DocMetadata::new(
            doc_id.clone(),
            slug.clone(),
            format!("Bench doc {di}"),
            if is_req { "spec" } else { "design" }.to_string(),
            TS.to_string(),
        );
        doc.tags = vec![
            "bench".to_string(),
            if is_req { "requirements" } else { "design" }.to_string(),
        ];
        if is_req {
            doc.verification = Some(Verification {
                status: "pending".to_string(),
                created_at: TS.to_string(),
                updated_at: TS.to_string(),
                items: verification_items,
            });
        }

        if di == 0 {
            doc0_body_bytes = body.len();
        }
        write_doc_body(&handoff_dir, &slug, &body)?;
        write_doc(&handoff_dir, &doc)?;
        docs_meta.push((slug, doc_id));
    }

    // ---- one small, scale-independent layer document (M1 t360.6 perf
    // bench: doc_save/doc_update_section on a layer document) ----
    let layer_doc_slug = "bench-layer-doc".to_string();
    let layer_doc_id = "doc-20260901-000000-900000".to_string();
    let layer_body = layer_document_body(opts.lang, LAYER_BODY_SEED, 0);
    let mut layer_doc = DocMetadata::new(
        layer_doc_id.clone(),
        layer_doc_slug.clone(),
        "Bench layer document".to_string(),
        "spec".to_string(),
        TS.to_string(),
    );
    layer_doc.layer = Some("basic_spec".to_string());
    write_doc_body(&handoff_dir, &layer_doc_slug, &layer_body)?;
    write_doc(&handoff_dir, &layer_doc)?;

    // ---- M1 t360.10/t360.11 trace-scale fixture (2,500 items / 30 docs,
    // wiki/240 §6 PR-7) ----
    let trace_slice_item_id = generate_trace_scale_docs(&handoff_dir, opts.lang)?;
    generate_suspect_seed_docs(&handoff_dir, opts.lang)?;
    let trace_task_id = "t-trace-bench".to_string();
    {
        let dir = handoff_dir
            .join("tasks")
            .join(format!("{trace_task_id}-bench-trace-task"));
        fs::create_dir_all(&dir)?;
        let data = TaskData {
            id: trace_task_id.clone(),
            title: "Trace bench task".to_string(),
            notes: None,
            priority: Some("medium".to_string()),
            created_at: Some(TS.to_string()),
            updated_at: Some(TS.to_string()),
            completed_at: None,
            labels: vec!["bench".to_string()],
            links: Vec::new(),
            task_links: vec![TaskLink {
                target: "doc-20260901-100000-0000".to_string(),
                link_type: "requirement".to_string(),
                label: Some(trace_slice_item_id.clone()),
                role: Some("implements".to_string()),
                baseline_hash: None,
            }],
            done_criteria: Vec::new(),
            schedule: None,
            dependencies: Vec::new(),
            order: None,
            assignee: None,
            lock: None,
            scope_paths: Vec::new(),
            extra: HashMap::new(),
        };
        write_task(&dir, "todo", &data)?;
    }

    // Co-linked tasks of hot subitems -> done, so toggling `hot` changes
    // derived dev_stage on every call (mirrors gen_fixture.py).
    for t in &hot_colinked {
        status_of.insert(t.clone(), "done");
    }
    hot_colinked.sort();
    hot_colinked.dedup();

    // ---- write tasks (parents before children so directories exist) ----
    let mut dir_of: HashMap<String, std::path::PathBuf> = HashMap::new();
    let mut rng_notes = Xorshift::new(opts.seed ^ 0xABCD);
    for node in &nodes {
        let status = status_of.get(node.id.as_str()).copied().unwrap_or("todo");
        let slug = format!("bench-task-{}", node.id.replace('.', "-"));
        let base = match &node.parent {
            None => handoff_dir.join("tasks"),
            Some(p) => dir_of
                .get(p)
                .cloned()
                .unwrap_or_else(|| handoff_dir.join("tasks")),
        };
        let dir = base.join(format!("{}-{slug}", node.id));
        fs::create_dir_all(&dir)?;
        dir_of.insert(node.id.clone(), dir.clone());

        let notes_reps = 5 + rng_notes.below(36);
        let data = TaskData {
            id: node.id.clone(),
            title: format!("Bench task {}", node.id),
            notes: Some(
                "Synthetic task notes. "
                    .repeat(notes_reps)
                    .trim()
                    .to_string(),
            ),
            priority: Some(["high", "medium", "low"][rng_notes.below(3)].to_string()),
            created_at: Some(TS.to_string()),
            updated_at: Some(TS.to_string()),
            completed_at: if status == "done" {
                Some(TS.to_string())
            } else {
                None
            },
            labels: vec![
                "bench".to_string(),
                ["core", "ui", "docs"][rng_notes.below(3)].to_string(),
            ],
            links: Vec::new(),
            task_links: links_by_task.get(&node.id).cloned().unwrap_or_default(),
            done_criteria: (0..3)
                .map(|i| DoneCriterion {
                    item: format!("criterion {i}"),
                    checked: status == "done",
                })
                .collect(),
            schedule: Some(Schedule {
                start_date: Some("2026-09-01".to_string()),
                estimate_hours: Some(2.0),
                actual_hours: Some(1.0),
                milestone: Some("m1".to_string()),
                ..Schedule::default()
            }),
            dependencies: Vec::new(),
            order: None,
            assignee: None,
            lock: None,
            scope_paths: Vec::new(),
            extra: HashMap::new(),
        };
        write_task(&dir, status, &data)?;
    }

    let (doc_slug, doc_id) = docs_meta[0].clone();
    Ok(FixtureMeta {
        hot_req_task: hot,
        hot_req_ids: hot_ids,
        hot_req_doc_slugs: hot_doc_slugs,
        hot_colinked_tasks: hot_colinked,
        plain_task: plain,
        extra_stable_id: extra_stable,
        doc_slug,
        doc_id,
        doc_body_bytes: doc0_body_bytes,
        verify_seq: 1,
        verify_idx_a: 9,
        verify_idx_b: 8,
        // Historically 3, unconditionally — kept as the preferred value (all
        // scale presets have >= 3 sections in doc 0, so this doesn't change
        // their generated output/determinism), but capped to whatever doc 0
        // actually has so a low-density fixture (e.g. `scale_ratio_d`'s
        // `large`, which fixes `subitems` while growing `docs`) doesn't ask
        // `doc_update_section` for a section number that doesn't exist.
        section_seq: doc0_sections.clamp(1, 3),
        layer_doc_slug,
        layer_doc_id,
        // The layer document's body starts directly with its own `# Bench
        // layer document` H1 (no text before it), so at the default
        // `split_level` (2) that heading's own section is seq 1 (seq 0 is
        // the empty preamble before it) — mirrors `sync_layer_items`'s own
        // test fixtures (`src/storage/docs/layer_sync.rs`) rather than
        // hardcoding a number here disconnected from `split()`'s actual
        // behavior.
        layer_section_seq: 1,
        layer_lang: opts.lang,
        trace_task_id,
        trace_slice_item_id,
    })
}

/// `VerificationItem::default()` doesn't exist (no `#[derive(Default)]`), so
/// this fills every field the struct literal above doesn't set explicitly.
fn zero_verification_item() -> VerificationItem {
    VerificationItem {
        fragment_seq: None,
        heading: String::new(),
        status: "pending".to_string(),
        impl_refs: Vec::new(),
        test_refs: Vec::new(),
        reviewer: None,
        verified_at: None,
        notes: String::new(),
        content_hash_at_verify: None,
        category: "section".to_string(),
        sub_items: Vec::new(),
        label: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_tree(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            for entry in fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, root, out);
                } else {
                    let rel = path
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .to_string();
                    out.push((rel, fs::read(&path).unwrap()));
                }
            }
        }
        walk(dir, dir, &mut out);
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    #[test]
    fn same_seed_produces_byte_identical_fixture() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        let opts = FixtureOpts {
            tasks: 30,
            docs: 4,
            subitems: 20,
            children: 3,
            seed: 42,
            lang: Lang::En,
        };
        let meta_a = generate(&a, &opts).unwrap();
        let meta_b = generate(&b, &opts).unwrap();

        assert_eq!(meta_a.hot_req_task, meta_b.hot_req_task);
        assert_eq!(meta_a.plain_task, meta_b.plain_task);
        assert_eq!(meta_a.hot_req_ids, meta_b.hot_req_ids);
        assert_eq!(meta_a.extra_stable_id, meta_b.extra_stable_id);

        let tree_a = read_tree(&a);
        let tree_b = read_tree(&b);
        assert_eq!(
            tree_a.len(),
            tree_b.len(),
            "same seed must produce the same file set"
        );
        for ((path_a, bytes_a), (path_b, bytes_b)) in tree_a.iter().zip(tree_b.iter()) {
            assert_eq!(path_a, path_b);
            assert_eq!(
                bytes_a, bytes_b,
                "file {path_a} must be byte-identical for the same seed"
            );
        }
    }

    #[test]
    fn different_seed_changes_content() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        let mut opts = FixtureOpts {
            tasks: 30,
            docs: 4,
            subitems: 20,
            children: 3,
            seed: 1,
            lang: Lang::En,
        };
        generate(&a, &opts).unwrap();
        opts.seed = 2;
        generate(&b, &opts).unwrap();

        let tree_a = read_tree(&a);
        let tree_b = read_tree(&b);
        assert_ne!(
            tree_a, tree_b,
            "different seeds should not produce identical fixtures"
        );
    }

    /// t370.12 rework (MAJOR, integration feedback round 1): `io_budget_bytes`
    /// (tests/perf_budget.rs, PR-8) must budget for every document
    /// `hot_req_task`'s requirement links actually span, not just `doc_slug`
    /// (docs_meta[0]) — this pins down that `hot_req_doc_slugs` is populated
    /// with exactly the docs the generation logic above links `hot` into
    /// (docs 0 and 1, per `hot_req_task`'s own doc comment) at every scale
    /// preset actually exercised by `perf_budget.rs`.
    #[test]
    fn hot_req_doc_slugs_covers_every_doc_hot_task_is_linked_into() {
        for opts in [
            FixtureOpts::s(),
            FixtureOpts::m(),
            FixtureOpts::l(),
            FixtureOpts::ja(),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().join("proj");
            let meta = generate(&dir, &opts).unwrap();
            assert_eq!(
                meta.hot_req_doc_slugs,
                vec!["bench-doc-000".to_string(), "bench-doc-001".to_string()],
                "hot_req_task's requirement links span docs 0/1 at every scale preset \
                 (tasks={}, docs={}, subitems={})",
                opts.tasks,
                opts.docs,
                opts.subitems
            );
            for slug in &meta.hot_req_doc_slugs {
                let path = dir
                    .join(".handoff")
                    .join("docs")
                    .join(format!("_doc.{slug}.md"));
                assert!(
                    path.exists(),
                    "hot_req_doc_slugs entry {slug} must name a document that actually exists \
                     on disk"
                );
            }
        }
    }

    /// t370.12 rework (MAJOR follow-up, integration feedback round 1):
    /// `propagate_dev_stage_for_task` reads every co-linked task's status
    /// file (min-of-linked-tasks dev_stage computation), not just
    /// `hot_req_task`'s own — `hot_colinked_tasks` must actually be
    /// populated (not silently empty) for `io_budget_bytes` to budget for
    /// that I/O.
    #[test]
    fn hot_colinked_tasks_is_non_empty_and_excludes_hot_req_task_itself() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("proj");
        let meta = generate(&dir, &FixtureOpts::s()).unwrap();
        assert!(
            !meta.hot_colinked_tasks.is_empty(),
            "hot_req_task's SubItems each link a second task — hot_colinked_tasks must \
             capture at least one"
        );
        assert!(
            !meta.hot_colinked_tasks.contains(&meta.hot_req_task),
            "hot_colinked_tasks must exclude hot_req_task itself"
        );
    }

    /// t370.15 (PR-4, wiki/240-performance-design.md §6): `doc_body_bytes`
    /// must reflect the *actual* on-disk body bytes for `doc_slug`, not a
    /// stale/duplicated computation — `tests/perf_budget.rs`'s size-scaled
    /// `doc_update_section` budget trusts this value directly. Also pins the
    /// English-vs-Japanese size relationship the budget's threshold/rate
    /// depend on: S/M/L stay well under a "tens of KB" normal-document size,
    /// JA is markedly larger for the same section/subitem structure.
    #[test]
    fn doc_body_bytes_matches_actual_doc_slug_body_on_disk() {
        use handoff_mcp::storage::docs::read_doc_body;

        for (opts, min_kb, max_kb) in [
            (FixtureOpts::s(), 4, 12),
            (FixtureOpts::m(), 4, 12),
            (FixtureOpts::l(), 4, 12),
            (FixtureOpts::ja(), 30, 55),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().join("proj");
            let meta = generate(&dir, &opts).unwrap();
            let on_disk = read_doc_body(&dir.join(".handoff"), &meta.doc_slug)
                .unwrap()
                .expect("doc_slug body must exist on disk");
            assert_eq!(
                meta.doc_body_bytes,
                on_disk.len(),
                "doc_body_bytes must equal the actual on-disk body length for {:?}",
                opts.lang
            );
            let kb = meta.doc_body_bytes / 1024;
            assert!(
                (min_kb..=max_kb).contains(&kb),
                "{:?}: doc_body_bytes {kb}KB out of expected range {min_kb}..={max_kb}KB",
                opts.lang
            );
        }
    }

    #[test]
    fn ja_lang_produces_japanese_body_text() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("proj");
        let opts = FixtureOpts {
            tasks: 10,
            docs: 2,
            subitems: 10,
            children: 3,
            seed: 1,
            lang: Lang::Ja,
        };
        let meta = generate(&dir, &opts).unwrap();
        let body = fs::read_to_string(
            dir.join(".handoff")
                .join("docs")
                .join(format!("_doc.{}.md", meta.doc_slug)),
        )
        .unwrap();
        assert!(
            body.contains("要件"),
            "ja fixture body should contain Japanese requirement text"
        );
    }

    #[test]
    fn scale_presets_match_wiki_240_table() {
        assert_eq!(
            (
                FixtureOpts::s().tasks,
                FixtureOpts::s().docs,
                FixtureOpts::s().subitems
            ),
            (200, 20, 500)
        );
        assert_eq!(
            (
                FixtureOpts::m().tasks,
                FixtureOpts::m().docs,
                FixtureOpts::m().subitems
            ),
            (1_000, 100, 2_500)
        );
        assert_eq!(
            (
                FixtureOpts::l().tasks,
                FixtureOpts::l().docs,
                FixtureOpts::l().subitems
            ),
            (3_000, 100, 2_500)
        );
        assert_eq!(
            (
                FixtureOpts::ja().tasks,
                FixtureOpts::ja().docs,
                FixtureOpts::ja().subitems
            ),
            (200, 20, 2_500)
        );
        assert_eq!(FixtureOpts::ja().lang, Lang::Ja);
    }
}
