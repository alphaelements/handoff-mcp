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
    /// Leaf task with no requirement links (baseline for PR-2/PR-9).
    pub plain_task: String,
    /// An unlinked stable_id, for requirement_ids add/remove round-trips.
    pub extra_stable_id: Option<String>,
    pub doc_slug: String,
    pub doc_id: String,
    /// `fragment_seq` / `sub_items` indices used for `doc_verify` calls
    /// against `doc_slug`/`doc_id`.
    pub verify_seq: usize,
    pub verify_idx_a: usize,
    pub verify_idx_b: usize,
    pub section_seq: usize,
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
    let mut extra_stable: Option<String> = None;
    let mut docs_meta: Vec<(String, String)> = Vec::with_capacity(opts.docs); // (slug, id)
    let mut hot_colinked: Vec<String> = Vec::new();
    // Section count of doc 0, used below to pick a `section_seq` for
    // `doc_update_section` that's guaranteed to exist. `PR-9`'s `scale_ratio_d`
    // holds `subitems` fixed while scaling `docs` up, which shrinks `per_doc`
    // (and therefore doc 0's section count) well below the historical
    // default of 3 — see `doc0_sections` below.
    let mut doc0_sections = 1usize;

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

        write_doc_body(&handoff_dir, &slug, &body)?;
        write_doc(&handoff_dir, &doc)?;
        docs_meta.push((slug, doc_id));
    }

    // Co-linked tasks of hot subitems -> done, so toggling `hot` changes
    // derived dev_stage on every call (mirrors gen_fixture.py).
    for t in &hot_colinked {
        status_of.insert(t.clone(), "done");
    }

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
        plain_task: plain,
        extra_stable_id: extra_stable,
        doc_slug,
        doc_id,
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
