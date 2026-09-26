//! Tier 1 performance budget harness (NFR-008 / NFR-009,
//! `wiki/240-performance-design.md` §6-7).
//!
//! `#[ignore]`d — not part of the default `cargo test` (precedent:
//! `tests/context_corpus_bench.rs`). Run explicitly, one scale at a time:
//!
//! ```text
//! cargo test --release --test perf_budget -- --ignored --test-threads=1 perf_budget_scale_s
//! ```
//!
//! Each op drives the real `handoff-mcp` binary over stdio JSON-RPC (mirrors
//! `tests/stdio_server.rs`'s real-transport approach), one warm-up call plus
//! 7 timed reps, comparing the median (p50) wall time against
//! `tests/perf_budgets.toml`, scaled by `HANDOFF_PERF_SLACK` (shared CI
//! runners: 3.0; default 1.0 for a quiet dev machine).
//!
//! A budget flagged `expected_fail = "reason"` is a known gap (recorded in
//! the printed table as `EXPECTED-FAIL`, non-fatal) rather than a hard test
//! failure — see `wiki/240-performance-design.md` §4 for which follow-up
//! task (t370.2-t370.6) is expected to close each gap. If an
//! `expected_fail` budget is unexpectedly *met*, the table prints a `PROMOTE`
//! notice so a human removes the stale `expected_fail` key.

#[path = "support/perf_fixture.rs"]
mod perf_fixture;

use perf_fixture::{generate, FixtureMeta, FixtureOpts, Lang};

use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

const BUDGETS_TOML: &str = include_str!("perf_budgets.toml");

#[derive(Debug, Deserialize)]
struct BudgetFile {
    #[serde(default)]
    budget: Vec<LatencyBudget>,
    #[serde(default)]
    ratio_budget: Vec<RatioBudget>,
}

#[derive(Debug, Deserialize, Clone)]
struct LatencyBudget {
    op: String,
    ms: f64,
    #[serde(default)]
    expected_fail: Option<String>,
    /// Restricts `expected_fail` to the named scales (e.g. `["L"]`). When
    /// absent, `expected_fail` applies at every scale. Without this, an op
    /// that only overruns at L/JA would also be non-gating at S/M, silently
    /// disabling the Tier 1 regression gate for it.
    #[serde(default)]
    expected_fail_scales: Option<Vec<String>>,
    /// Entries that are *not* expected to appear in every `run_ops` result
    /// map (e.g. the requirement-toggle op needs a free stable_id the
    /// fixture may not always produce, and the two `io_*` pseudo-entries
    /// below are checked against `IoCounters` directly rather than against
    /// a measured latency). Any other budget whose `op` is missing from the
    /// results is a hard failure — a typo/rename in `perf_budgets.toml` or
    /// `run_ops` must not silently disable a gate.
    #[serde(default)]
    optional: bool,
}

impl LatencyBudget {
    /// The `expected_fail` reason if it applies at `scale_name`.
    fn expected_fail_at(&self, scale_name: &str) -> Option<&String> {
        match &self.expected_fail_scales {
            Some(scales) if !scales.iter().any(|s| s == scale_name) => None,
            _ => self.expected_fail.as_ref(),
        }
    }
}

/// Shared status classification for both `[[budget]]` latency checks (line
/// ~543 below) and the `io_*` pseudo-entry `IoCounters` checks (t370.11,
/// wiki/240-performance-design.md §4 P-M4 follow-up).
///
/// Before t370.11, the `io_*` checks read `entry.expected_fail` directly
/// instead of going through [`LatencyBudget::expected_fail_at`] — so an
/// `expected_fail_scales`-restricted reason (meant to apply at, say, JA
/// only) was silently applied at *every* scale, exactly the "achieved
/// scales stay inside expected_fail" bug this task exists to fix:
/// `io_update_task_status_with_links` regressed at M/L/JA but not S, yet
/// its blanket `expected_fail` (no `expected_fail_scales`) hid the M/L
/// regression from the Tier-1 S/M gate too. Centralizing the four-way
/// (within, expected_fail) -> status match here also keeps the io_* checks
/// and the ms-budget check (which already used this same logic inline)
/// from silently drifting apart.
fn classify_io_check<'a>(
    within: bool,
    entry: Option<&'a LatencyBudget>,
    scale_name: &str,
) -> (&'static str, Option<&'a str>) {
    let expected_fail = entry.and_then(|b| b.expected_fail_at(scale_name).map(String::as_str));
    match (within, expected_fail) {
        (true, None) => ("ok", None),
        (true, Some(_)) => ("PROMOTE?", None),
        (false, None) => ("FAIL", None),
        (false, Some(reason)) => ("expected-fail", Some(reason)),
    }
}

#[cfg(test)]
mod io_check_scale_gating_tests {
    use super::*;

    fn budget_expected_fail_at(scales: &[&str]) -> LatencyBudget {
        LatencyBudget {
            op: "io_example".to_string(),
            ms: 0.0,
            expected_fail: Some("known issue".to_string()),
            expected_fail_scales: Some(scales.iter().map(|s| s.to_string()).collect()),
            optional: true,
        }
    }

    /// The bug this task fixes: an `expected_fail_scales = ["JA"]` entry
    /// must not swallow a real regression at a scale it doesn't name.
    #[test]
    fn out_of_budget_at_unlisted_scale_is_a_hard_fail_not_expected_fail() {
        let entry = budget_expected_fail_at(&["JA"]);
        let (status, reason) = classify_io_check(false, Some(&entry), "L");
        assert_eq!(status, "FAIL");
        assert_eq!(reason, None);
    }

    #[test]
    fn out_of_budget_at_listed_scale_is_expected_fail() {
        let entry = budget_expected_fail_at(&["JA"]);
        let (status, reason) = classify_io_check(false, Some(&entry), "JA");
        assert_eq!(status, "expected-fail");
        assert_eq!(reason, Some("known issue"));
    }

    #[test]
    fn within_budget_is_ok_or_promote_depending_on_expected_fail() {
        let entry = budget_expected_fail_at(&["JA"]);
        assert_eq!(classify_io_check(true, Some(&entry), "L").0, "ok");
        assert_eq!(classify_io_check(true, Some(&entry), "JA").0, "PROMOTE?");
        assert_eq!(classify_io_check(true, None, "L").0, "ok");
    }

    #[test]
    fn missing_expected_fail_scales_applies_at_every_scale() {
        let entry = LatencyBudget {
            op: "io_example".to_string(),
            ms: 0.0,
            expected_fail: Some("known issue".to_string()),
            expected_fail_scales: None,
            optional: true,
        };
        assert_eq!(
            classify_io_check(false, Some(&entry), "S").0,
            "expected-fail"
        );
        assert_eq!(
            classify_io_check(false, Some(&entry), "JA").0,
            "expected-fail"
        );
    }
}

#[derive(Debug, Deserialize, Clone)]
struct RatioBudget {
    name: String,
    max_ratio: f64,
    #[serde(default)]
    expected_fail: Option<String>,
}

fn slack() -> f64 {
    std::env::var("HANDOFF_PERF_SLACK")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1.0)
}

fn load_budgets() -> BudgetFile {
    toml::from_str(BUDGETS_TOML).expect("tests/perf_budgets.toml must parse")
}

#[derive(Debug, Default, Clone, Copy)]
struct IoCounters {
    rchar: u64,
    wchar: u64,
    syscr: u64,
    syscw: u64,
}

impl IoCounters {
    fn delta(&self, before: &IoCounters) -> IoCounters {
        IoCounters {
            rchar: self.rchar.saturating_sub(before.rchar),
            wchar: self.wchar.saturating_sub(before.wchar),
            syscr: self.syscr.saturating_sub(before.syscr),
            syscw: self.syscw.saturating_sub(before.syscw),
        }
    }
}

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Client {
    fn spawn() -> Self {
        let bin = env!("CARGO_BIN_EXE_handoff-mcp");
        let mut child = Command::new(bin)
            .env("HANDOFF_MCP_REQUEST_TIMEOUT_SECS", "900")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn handoff-mcp binary (release build required)");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let mut client = Client {
            child,
            stdin,
            stdout,
            next_id: 0,
        };
        client.rpc(
            "initialize",
            json!({"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "perf_budget", "version": "0"}}),
        );
        client.notify("notifications/initialized");
        client
    }

    fn notify(&mut self, method: &str) {
        let line = json!({"jsonrpc": "2.0", "method": method}).to_string();
        writeln!(self.stdin, "{line}").expect("write notification");
        self.stdin.flush().expect("flush");
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let line =
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
        writeln!(self.stdin, "{line}").expect("write request");
        self.stdin.flush().expect("flush");
        let mut buf = String::new();
        self.stdout.read_line(&mut buf).expect("read response line");
        serde_json::from_str(&buf)
            .unwrap_or_else(|e| panic!("invalid JSON-RPC response: {e}: {buf}"))
    }

    #[cfg(target_os = "linux")]
    fn io(&self) -> IoCounters {
        // PR-8 (wiki §6) is a deterministic, machine-independent budget, so
        // this must fail closed: a missing/unreadable `/proc/<pid>/io` (e.g.
        // a restricted container without `hidepid=0`) must not silently
        // report zero bytes and let the I/O budget pass trivially.
        let pid = self.child.id();
        let text = std::fs::read_to_string(format!("/proc/{pid}/io"))
            .unwrap_or_else(|e| panic!("failed to read /proc/{pid}/io: {e} (PR-8 I/O budget requires this counter on Linux; run in an environment where it is readable, or skip on non-Linux)"));
        let mut out = IoCounters::default();
        let mut saw_rchar = false;
        for line in text.lines() {
            let Some((k, v)) = line.split_once(':') else {
                continue;
            };
            let v: u64 = v
                .trim()
                .parse()
                .unwrap_or_else(|e| panic!("failed to parse /proc/{pid}/io line {line:?}: {e}"));
            match k.trim() {
                "rchar" => {
                    out.rchar = v;
                    saw_rchar = true;
                }
                "wchar" => out.wchar = v,
                "syscr" => out.syscr = v,
                "syscw" => out.syscw = v,
                _ => {}
            }
        }
        assert!(saw_rchar, "/proc/{pid}/io had no rchar line: {text:?}");
        out
    }

    #[cfg(not(target_os = "linux"))]
    fn io(&self) -> IoCounters {
        IoCounters::default()
    }

    /// Call a tool, returning wall time, the I/O delta observed via
    /// `/proc/<pid>/io` (zeroed on non-Linux), and the response text.
    fn call(&mut self, name: &str, args: Value) -> (Duration, IoCounters, String) {
        let before = self.io();
        let t0 = Instant::now();
        let resp = self.rpc("tools/call", json!({"name": name, "arguments": args}));
        let dt = t0.elapsed();
        let after = self.io();
        let result = resp.get("result").cloned().unwrap_or(Value::Null);
        let text = result
            .get("content")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        let is_error = resp.get("error").is_some()
            || result
                .get("isError")
                .and_then(|b| b.as_bool())
                .unwrap_or(false);
        assert!(
            !is_error,
            "{name} failed: error={:?} text={}",
            resp.get("error"),
            &text[..text.len().min(400)]
        );
        (dt, after.delta(&before), text)
    }

    fn close(mut self) {
        drop(self.stdin);
        let _ = self.child.wait();
    }
}

struct OpResult {
    median_ms: f64,
    io_median: IoCounters,
}

/// Run `op` `warmup + reps` times via `call_fn`, discard the warm-up sample,
/// and return the median latency plus the I/O delta of the median-latency
/// sample (matches `tmp/260903-mperf/bench_mcp.py`'s methodology).
fn measure(
    client: &mut Client,
    reps: usize,
    mut call_fn: impl FnMut(&mut Client, usize) -> (Duration, IoCounters),
) -> OpResult {
    let (_, _) = call_fn(client, 0); // warm-up, discarded
    let mut samples: Vec<(f64, IoCounters)> = Vec::with_capacity(reps);
    for i in 0..reps {
        let (dt, io) = call_fn(client, i);
        samples.push((dt.as_secs_f64() * 1000.0, io));
    }
    samples.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let mid = samples[samples.len() / 2];
    OpResult {
        median_ms: mid.0,
        io_median: mid.1,
    }
}

const REPS: usize = 7;

fn run_ops(
    client: &mut Client,
    proj: &std::path::Path,
    meta: &FixtureMeta,
) -> HashMap<String, OpResult> {
    let p = proj.to_string_lossy().to_string();
    let scan_parent = proj
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());
    let mut results = HashMap::new();

    macro_rules! op {
        ($name:literal, $call:expr) => {
            let r = measure(client, REPS, $call);
            results.insert($name.to_string(), r);
        };
    }

    op!("list_tasks", |c: &mut Client, _i| {
        let (dt, io, _) = c.call("handoff_list_tasks", json!({"project_dir": p}));
        (dt, io)
    });
    op!("load_context", |c: &mut Client, _i| {
        let (dt, io, _) = c.call("handoff_load_context", json!({"project_dir": p}));
        (dt, io)
    });
    op!("get_metrics", |c: &mut Client, _i| {
        let (dt, io, _) = c.call("handoff_get_metrics", json!({"project_dir": p}));
        (dt, io)
    });
    op!("dashboard", |c: &mut Client, _i| {
        let (dt, io, _) = c.call(
            "handoff_dashboard",
            json!({"project_dir": p, "scan_dirs": [scan_parent], "max_depth": 2}),
        );
        (dt, io)
    });
    op!("doc_list", |c: &mut Client, _i| {
        let (dt, io, _) = c.call("handoff_doc_list", json!({"project_dir": p}));
        (dt, io)
    });
    op!("doc_req_list", |c: &mut Client, _i| {
        let (dt, io, _) = c.call("handoff_doc_req_list", json!({"project_dir": p}));
        (dt, io)
    });
    op!("doc_req_status", |c: &mut Client, _i| {
        let (dt, io, _) = c.call("handoff_doc_req_status", json!({"project_dir": p}));
        (dt, io)
    });
    // PR-5 (wiki §6) explicitly names the hook path (`doc_query`, UserPromptSubmit
    // / PreToolUse) as the largest real-world cost (800ms real / 2,160ms JA,
    // wiki §2). `mark_injected: false` keeps every rep's cost comparable (no
    // growing sidecar file across reps within one suite run).
    op!("doc_query", |c: &mut Client, _i| {
        let (dt, io, _) = c.call(
            "handoff_doc_query",
            json!({"project_dir": p, "text": "requirement status update", "mark_injected": false}),
        );
        (dt, io)
    });
    op!("update_task_status_no_links", |c: &mut Client, i: usize| {
        let status = if i % 2 == 0 { "in_progress" } else { "todo" };
        let (dt, io, _) = c.call(
            "handoff_update_task",
            json!({"project_dir": p, "task": {"id": meta.plain_task, "status": status}}),
        );
        (dt, io)
    });
    op!(
        "update_task_status_with_links",
        |c: &mut Client, i: usize| {
            let status = if i % 2 == 0 { "in_progress" } else { "todo" };
            let (dt, io, _) = c.call(
                "handoff_update_task",
                json!({"project_dir": p, "task": {"id": meta.hot_req_task, "status": status}}),
            );
            (dt, io)
        }
    );
    // leave `hot` at a stable status before the requirement_ids toggle below
    client.call(
        "handoff_update_task",
        json!({"project_dir": p, "task": {"id": meta.hot_req_task, "status": "todo"}}),
    );
    if let Some(extra) = meta.extra_stable_id.clone() {
        let base_ids = meta.hot_req_ids.clone();
        op!(
            "update_task_requirement_ids_toggle",
            |c: &mut Client, _i| {
                let mut with_extra = base_ids.clone();
                with_extra.push(extra.clone());
                let (dt, io, _) = c.call(
                "handoff_update_task",
                json!({"project_dir": p, "task": {"id": meta.hot_req_task, "requirement_ids": with_extra}}),
            );
                c.call(
                "handoff_update_task",
                json!({"project_dir": p, "task": {"id": meta.hot_req_task, "requirement_ids": base_ids}}),
            );
                (dt, io)
            }
        );
    }
    op!("doc_verify_set_dev_stage", |c: &mut Client, i: usize| {
        let stage = if i % 2 == 0 {
            "in_progress"
        } else {
            "not_started"
        };
        let (dt, io, _) = c.call(
            "handoff_doc_verify",
            json!({
                "project_dir": p, "doc_id": meta.doc_slug, "action": "set_dev_stage",
                "fragment_seq": meta.verify_seq, "sub_item_index": meta.verify_idx_a, "dev_stage": stage,
            }),
        );
        (dt, io)
    });
    // `by_id` variant of the same op (resolved via `find_doc_by_id`'s
    // full-scan fallback, C2 in wiki/240 §3) rather than by slug.
    op!(
        "doc_verify_set_dev_stage_by_id",
        |c: &mut Client, i: usize| {
            let stage = if i % 2 == 0 {
                "in_progress"
            } else {
                "not_started"
            };
            let (dt, io, _) = c.call(
            "handoff_doc_verify",
            json!({
                "project_dir": p, "doc_id": meta.doc_id, "action": "set_dev_stage",
                "fragment_seq": meta.verify_seq, "sub_item_index": meta.verify_idx_a, "dev_stage": stage,
            }),
        );
            (dt, io)
        }
    );
    op!("doc_verify_link_task", |c: &mut Client, i: usize| {
        let task_ids = if i % 2 == 0 {
            vec![meta.plain_task.clone()]
        } else {
            vec![meta.plain_task.clone(), meta.hot_req_task.clone()]
        };
        let (dt, io, _) = c.call(
            "handoff_doc_verify",
            json!({
                "project_dir": p, "doc_id": meta.doc_slug, "action": "link_task",
                "fragment_seq": meta.verify_seq, "sub_item_index": meta.verify_idx_b, "task_ids": task_ids,
            }),
        );
        (dt, io)
    });
    op!("doc_update_section", |c: &mut Client, i: usize| {
        let content = format!(
            "## {0}. Section {0}\n\nedited body {i}\n\n",
            meta.section_seq
        );
        let (dt, io, _) = c.call(
            "handoff_doc_update_section",
            json!({"project_dir": p, "doc_id": meta.doc_slug, "seq": meta.section_seq, "new_content": content}),
        );
        (dt, io)
    });

    results
}

/// I/O budget (PR-8, wiki §6): the read volume of `update_task_status_with_links`
/// must stay within (target doc bytes + target task bytes) + 256 KiB.
fn io_budget_bytes(proj: &std::path::Path, meta: &FixtureMeta) -> u64 {
    let doc_path = proj
        .join(".handoff")
        .join("docs")
        .join(format!("_doc.{}.md", meta.doc_slug));
    let task_bytes = find_task_file_size(proj, &meta.hot_req_task).unwrap_or(0);
    let doc_bytes = std::fs::metadata(&doc_path).map(|m| m.len()).unwrap_or(0);
    doc_bytes + task_bytes + 256 * 1024
}

fn find_task_file_size(proj: &std::path::Path, task_id: &str) -> Option<u64> {
    fn walk(dir: &std::path::Path, task_id: &str) -> Option<u64> {
        for entry in std::fs::read_dir(dir).ok()? {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name()?.to_string_lossy().to_string();
                if name == task_id || name.starts_with(&format!("{task_id}-")) {
                    for status in [
                        "done",
                        "todo",
                        "in_progress",
                        "review",
                        "blocked",
                        "skipped",
                    ] {
                        let f = path.join(format!("_task.{status}.json"));
                        if let Ok(m) = std::fs::metadata(&f) {
                            return Some(m.len());
                        }
                    }
                }
                if let Some(found) = walk(&path, task_id) {
                    return Some(found);
                }
            }
        }
        None
    }
    walk(&proj.join(".handoff").join("tasks"), task_id)
}

/// Write budget (companion to PR-8's read budget): the same (doc bytes plus
/// task bytes plus 256 KiB) envelope, applied to `wchar` instead of `rchar`.
/// PR-8 (wiki §6) requires that files whose content didn't change must not
/// be written; until t370.4 lands write discipline, this is tracked as
/// `expected_fail`.
fn wchar_budget_bytes(proj: &std::path::Path, meta: &FixtureMeta) -> u64 {
    io_budget_bytes(proj, meta)
}

/// Runs the full op suite against `opts`, checks every latency budget
/// (scaled by `HANDOFF_PERF_SLACK`) plus the machine-independent PR-8/PR-9
/// budgets (never scaled by slack — wiki §6: "PR-8・PR-9 は機械に依存しない
/// ので、共有ランナーでも厳密に検査できる"), prints a report table, and
/// panics if any non-`expected_fail`/non-`optional` budget was exceeded or
/// unmeasured.
fn run_budget_suite(scale_name: &str, opts: FixtureOpts) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let proj = tmp.path().join("proj");
    let meta = generate(&proj, &opts).expect("generate fixture");
    let io_budget = io_budget_bytes(&proj, &meta);
    let wchar_budget = wchar_budget_bytes(&proj, &meta);

    let mut client = Client::spawn();
    let results = run_ops(&mut client, &proj, &meta);
    client.close();

    let budgets = load_budgets();
    let slack_mult = slack();
    let mut failures: Vec<String> = Vec::new();
    let mut table = String::new();
    table.push_str(&format!(
        "\n=== perf_budget[{scale_name}] tasks={} docs={} subitems={} slack={slack_mult} (slack applies to ms budgets only; I/O and ratio budgets are exact) ===\n",
        opts.tasks, opts.docs, opts.subitems
    ));
    table.push_str(&format!(
        "{:38} {:>10} {:>10} {:>12} {:>10}\n",
        "op", "p50_ms", "budget_ms", "status", "rchar_kb"
    ));

    for budget in &budgets.budget {
        let Some(result) = results.get(&budget.op) else {
            if budget.optional {
                continue;
            }
            failures.push(format!(
                "{}: budget declared in perf_budgets.toml but no matching op was measured \
                 (typo/rename in perf_budgets.toml or run_ops? mark `optional = true` if this \
                 op is legitimately absent at some scales)",
                budget.op
            ));
            continue;
        };
        let effective_budget = budget.ms * slack_mult;
        let within = result.median_ms <= effective_budget;
        let expected_fail = budget.expected_fail_at(scale_name);
        let status = match (within, expected_fail) {
            (true, None) => "ok",
            (true, Some(_)) => "PROMOTE?",
            (false, None) => "FAIL",
            (false, Some(_)) => "expected-fail",
        };
        table.push_str(&format!(
            "{:38} {:>10.1} {:>10.1} {:>12} {:>10}\n",
            budget.op,
            result.median_ms,
            effective_budget,
            status,
            result.io_median.rchar / 1024,
        ));
        if !within && expected_fail.is_none() {
            failures.push(format!(
                "{}: p50 {:.1}ms exceeds budget {:.1}ms (slack {slack_mult})",
                budget.op, result.median_ms, effective_budget
            ));
        }
    }

    // PR-8 I/O budgets, checked against the requirement-linked status change.
    // Not scaled by `HANDOFF_PERF_SLACK`: wiki §6 states PR-8/PR-9 are
    // machine-independent byte/ratio counts, so shared CI runners must be
    // held to the exact same bound as a quiet dev machine.
    if let Some(io_result) = results.get("update_task_status_with_links") {
        let rchar_entry = budgets
            .budget
            .iter()
            .find(|b| b.op == "io_update_task_status_with_links");
        let rchar_within = (io_result.io_median.rchar as f64) <= (io_budget as f64);
        table.push_str(&format!(
            "io: update_task_status_with_links rchar={}KB budget={}KB -> {}\n",
            io_result.io_median.rchar / 1024,
            io_budget / 1024,
            if rchar_within { "ok" } else { "over" }
        ));
        // Not `#[cfg(target_os = "linux")]`-gated: on non-Linux, `io()` always
        // reports zero counters (see `Client::io`), so both checks are
        // trivially true and this branch is dead at runtime rather than
        // needing a compile-time cfg (which would otherwise leave the
        // `*_entry` lookups unused on non-Linux `cargo clippy --all-targets`
        // legs).
        //
        // `classify_io_check` (t370.11) scale-gates `expected_fail` via
        // `expected_fail_scales`, same as the ms-budget check above — a
        // reason restricted to `["JA"]` must not also swallow a real
        // regression at S/M/L.
        match classify_io_check(rchar_within, rchar_entry, scale_name) {
            ("FAIL", _) => failures.push(format!(
                "io_update_task_status_with_links: rchar {}KB exceeds budget {}KB",
                io_result.io_median.rchar / 1024,
                io_budget / 1024
            )),
            ("expected-fail", Some(reason)) => {
                table.push_str(&format!("  expected-fail: {reason}\n"))
            }
            ("PROMOTE?", _) => {
                table.push_str("  PROMOTE?: rchar now within budget — remove expected_fail\n")
            }
            _ => {}
        }

        // Write-side counterpart (wiki §6 PR-8: "内容が変わらないファイルは
        // 書かない"). `wchar` is collected by `Client::io` but was previously
        // never reported or checked.
        let wchar_entry = budgets
            .budget
            .iter()
            .find(|b| b.op == "io_wchar_update_task_status_with_links");
        let wchar_within = (io_result.io_median.wchar as f64) <= (wchar_budget as f64);
        table.push_str(&format!(
            "io: update_task_status_with_links wchar={}KB budget={}KB -> {}\n",
            io_result.io_median.wchar / 1024,
            wchar_budget / 1024,
            if wchar_within { "ok" } else { "over" }
        ));
        match classify_io_check(wchar_within, wchar_entry, scale_name) {
            ("FAIL", _) => failures.push(format!(
                "io_wchar_update_task_status_with_links: wchar {}KB exceeds budget {}KB",
                io_result.io_median.wchar / 1024,
                wchar_budget / 1024
            )),
            ("expected-fail", Some(reason)) => {
                table.push_str(&format!("  expected-fail: {reason}\n"))
            }
            ("PROMOTE?", _) => {
                table.push_str("  PROMOTE?: wchar now within budget — remove expected_fail\n")
            }
            _ => {}
        }
    }

    eprintln!("{table}");
    assert!(
        failures.is_empty(),
        "perf budget(s) exceeded for scale {scale_name}:\n{}\n{table}",
        failures.join("\n")
    );
}

#[test]
#[ignore = "manual/CI perf gate — run with `cargo test --release --test perf_budget -- --ignored --test-threads=1 perf_budget_scale_s`"]
fn perf_budget_scale_s() {
    run_budget_suite("S", FixtureOpts::s());
}

#[test]
#[ignore = "manual/CI perf gate — run with `cargo test --release --test perf_budget -- --ignored --test-threads=1 perf_budget_scale_m`"]
fn perf_budget_scale_m() {
    run_budget_suite("M", FixtureOpts::m());
}

#[test]
#[ignore = "nightly perf gate (large fixture) — run with `cargo test --release --test perf_budget -- --ignored --test-threads=1 perf_budget_scale_l`"]
fn perf_budget_scale_l() {
    run_budget_suite("L", FixtureOpts::l());
}

#[test]
#[ignore = "nightly perf gate (Japanese body text, NFR-003) — run with `cargo test --release --test perf_budget -- --ignored --test-threads=1 perf_budget_scale_ja`"]
fn perf_budget_scale_ja() {
    run_budget_suite("JA", FixtureOpts::ja());
}

/// t370.8 (wiki/240-performance-design.md §4 P-M1): measures a **brand new**
/// process's very first `doc_list`/`doc_req_status` call against a JA-scale
/// project — the scenario the P-M1 process cache (t370.2) cannot help with
/// (a short-lived CLI invocation, or an MCP server's first request after
/// startup), so this is exactly the case a deferred/lazy `content_hash`
/// (rather than a warm-process cache) targets. Each op gets its own fresh
/// process + fixture (a second call in the same process would be served by
/// the now-warm P-M1 cache, defeating the point of measuring a cold start).
/// Not folded into `run_budget_suite` (which always discards a warm-up rep
/// first, by design — that harness measures steady-state, not cold-start).
#[test]
#[ignore = "nightly perf gate (cold-start, NFR-003/NFR-008) — run with `cargo test --release --test perf_budget -- --ignored --test-threads=1 perf_budget_cold_start_ja`"]
fn perf_budget_cold_start_ja() {
    let cold_doc_list_ms = cold_start_call(&FixtureOpts::ja(), "handoff_doc_list", |_p| json!({}));
    let cold_doc_req_status_ms =
        cold_start_call(&FixtureOpts::ja(), "handoff_doc_req_status", |_p| json!({}));

    let budgets = load_budgets();
    let slack_mult = slack();
    let mut table = String::new();
    table.push_str("\n=== perf_budget[cold-start JA] (no P-M1 cache warm-up — brand new process, first call) ===\n");
    table.push_str(&format!(
        "{:38} {:>10} {:>10} {:>12}\n",
        "op", "p50_ms", "budget_ms", "status"
    ));
    let mut failures: Vec<String> = Vec::new();
    for (op, measured_ms) in [
        ("cold_doc_list", cold_doc_list_ms),
        ("cold_doc_req_status", cold_doc_req_status_ms),
    ] {
        let Some(budget) = budgets.budget.iter().find(|b| b.op == op) else {
            panic!("{op}: no matching entry in tests/perf_budgets.toml");
        };
        let effective_budget = budget.ms * slack_mult;
        let within = measured_ms <= effective_budget;
        let expected_fail = budget.expected_fail_at("JA");
        let status = match (within, expected_fail) {
            (true, None) => "ok",
            (true, Some(_)) => "PROMOTE?",
            (false, None) => "FAIL",
            (false, Some(_)) => "expected-fail",
        };
        table.push_str(&format!(
            "{op:38} {measured_ms:>10.1} {effective_budget:>10.1} {status:>12}\n"
        ));
        if !within && expected_fail.is_none() {
            failures.push(format!(
                "{op}: {measured_ms:.1}ms exceeds cold-start budget {effective_budget:.1}ms (slack {slack_mult})"
            ));
        }
    }
    eprintln!("{table}");
    assert!(
        failures.is_empty(),
        "cold-start perf budget(s) exceeded:\n{}\n{table}",
        failures.join("\n")
    );
}

/// Spawns a brand-new fixture + a brand-new `handoff-mcp` process, makes
/// exactly *one* call to `tool` (no warm-up — that is the point: this
/// measures the very first request a fresh process ever answers), and
/// returns its wall-clock latency in milliseconds.
fn cold_start_call(
    opts: &FixtureOpts,
    tool: &str,
    args: impl FnOnce(&std::path::Path) -> Value,
) -> f64 {
    let tmp = tempfile::tempdir().expect("tempdir");
    let proj = tmp.path().join("proj");
    generate(&proj, opts).expect("generate fixture");
    let mut client = Client::spawn();
    let arguments = {
        let mut a = args(&proj);
        a["project_dir"] = json!(proj.to_string_lossy());
        a
    };
    let (dt, _io, _text) = client.call(tool, arguments);
    client.close();
    dt.as_secs_f64() * 1000.0
}

/// PR-9 (wiki §6): scaling task count 200 -> 3,000 (15x) must not move
/// `update_task_status_no_links` p50 by more than the budgeted ratio.
#[test]
#[ignore = "nightly scale-ratio gate — run with `cargo test --release --test perf_budget -- --ignored --test-threads=1 perf_budget_scale_ratio_n`"]
fn perf_budget_scale_ratio_n() {
    let small = FixtureOpts {
        tasks: 200,
        docs: 20,
        subitems: 500,
        children: 9,
        seed: 1,
        lang: Lang::En,
    };
    let large = FixtureOpts {
        tasks: 3_000,
        ..small.clone()
    };
    check_scale_ratio("scale_ratio_n", small, large, "update_task_status_no_links");
}

/// PR-9 (wiki §6): "S を固定して D を 20 → 100（5倍）にしても単項目変更は
/// 1.5 倍以内" — the requirement SubItem count (S) is held fixed at 500 and
/// only the document count (D) scales 20 -> 100; NFR-008 names document
/// count, not SubItem count, as the axis under test here (that's
/// `scale_ratio_n`'s job via `tasks`). `subitems` is intentionally *not*
/// overridden on `large` (it must stay 500, inherited from `small`).
#[test]
#[ignore = "nightly scale-ratio gate — run with `cargo test --release --test perf_budget -- --ignored --test-threads=1 perf_budget_scale_ratio_d`"]
fn perf_budget_scale_ratio_d() {
    let small = FixtureOpts {
        tasks: 200,
        docs: 20,
        subitems: 500,
        children: 9,
        seed: 1,
        lang: Lang::En,
    };
    let large = FixtureOpts {
        docs: 100,
        ..small.clone()
    };
    check_scale_ratio("scale_ratio_d", small, large, "doc_verify_set_dev_stage");
}

/// PR-9 ratio check — not scaled by `HANDOFF_PERF_SLACK`: wiki §6 states
/// PR-8/PR-9 are machine-independent (a ratio of two on-machine timings, not
/// an absolute latency), so shared CI runners are held to the same bound
/// (1.5x) as a quiet dev machine.
fn check_scale_ratio(ratio_name: &str, small: FixtureOpts, large: FixtureOpts, op: &str) {
    let small_ms = measure_single_scale(&small, op);
    let large_ms = measure_single_scale(&large, op);
    let ratio = large_ms / small_ms;

    let budgets = load_budgets();
    let entry = budgets
        .ratio_budget
        .iter()
        .find(|r| r.name == ratio_name)
        .unwrap_or_else(|| panic!("no ratio_budget entry named {ratio_name} in perf_budgets.toml"));
    let max_ratio = entry.max_ratio;
    let within = ratio <= max_ratio;

    eprintln!(
        "\n=== perf_budget[{ratio_name}] op={op} small_p50={small_ms:.1}ms large_p50={large_ms:.1}ms ratio={ratio:.2} budget={max_ratio:.2} ===\n"
    );

    match (within, &entry.expected_fail) {
        (true, None) => {}
        (true, Some(_reason)) => {
            eprintln!("{ratio_name}: PROMOTE? — ratio now within budget, remove expected_fail");
        }
        (false, Some(reason)) => {
            eprintln!("{ratio_name}: expected-fail ({reason})");
        }
        (false, None) => panic!(
            "{ratio_name}: ratio {ratio:.2} exceeds budget {max_ratio:.2} (op={op}, small={small_ms:.1}ms, large={large_ms:.1}ms)"
        ),
    }
}

fn measure_single_scale(opts: &FixtureOpts, op: &str) -> f64 {
    let tmp = tempfile::tempdir().expect("tempdir");
    let proj = tmp.path().join("proj");
    let meta = generate(&proj, opts).expect("generate fixture");
    let mut client = Client::spawn();
    let results = run_ops(&mut client, &proj, &meta);
    client.close();
    results
        .get(op)
        .unwrap_or_else(|| panic!("op {op} was not measured"))
        .median_ms
}
