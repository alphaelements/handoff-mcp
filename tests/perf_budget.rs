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
    /// t370.15 (PR-4, wiki/240-performance-design.md §6, user decision
    /// 2026-09-27 案(c) round 1/2): below this many bytes of the target
    /// document's *body* (`FixtureMeta::doc_body_bytes`), `ms` applies
    /// unchanged. Above it, [`LatencyBudget::effective_ms_budget`] adds
    /// `size_extra_ms_per_mib` per MiB of excess — an allowance for a
    /// genuinely huge single document, whose write-time hashing cost is
    /// proportional to body size. `None` (the default for every op, as of
    /// t370.15 round 3) means no size scaling — `ms` always applies as-is.
    ///
    /// No current `[[budget]]` entry in `tests/perf_budgets.toml` sets both
    /// this and `size_extra_ms_per_mib` any more: round 3 replaced
    /// `doc_update_section`'s per-write `lexsim::content_hash(whole_body)`
    /// pass with a per-*section* rehash + cheap FNV-1a composition
    /// (`storage::docs::split::compose_doc_hash`/`compute_sections_after_splice`),
    /// which closed the JA gap this scaling mechanism existed to work around
    /// without needing a size-based allowance at all (see
    /// `tests/perf_budgets.toml`'s `doc_update_section` entry for the full
    /// history). The mechanism itself is left in place — tested below — as
    /// reusable infrastructure for a future op whose cost is genuinely
    /// proportional to a single document's size.
    #[serde(default)]
    size_threshold_bytes: Option<u64>,
    /// See `size_threshold_bytes`. Both fields must be `Some` for scaling to
    /// apply — a budget with only one of the two set is treated as
    /// unscaled (`effective_ms_budget` falls back to plain `ms`).
    #[serde(default)]
    size_extra_ms_per_mib: Option<f64>,
}

impl LatencyBudget {
    /// The `expected_fail` reason if it applies at `scale_name`.
    fn expected_fail_at(&self, scale_name: &str) -> Option<&String> {
        match &self.expected_fail_scales {
            Some(scales) if !scales.iter().any(|s| s == scale_name) => None,
            _ => self.expected_fail.as_ref(),
        }
    }

    /// The `ms` budget to use before `HANDOFF_PERF_SLACK` scaling, given the
    /// target document's body size in bytes (`None` when the op isn't tied
    /// to a specific document, e.g. `doc_body_bytes` doesn't apply to it).
    ///
    /// t370.15: only a budget with both `size_threshold_bytes` and
    /// `size_extra_ms_per_mib` set (currently just `doc_update_section`)
    /// scales; every other budget always returns plain `ms`, so passing
    /// `Some(doc_body_bytes)` unconditionally from the caller is safe and
    /// keeps the caller from having to know which ops are size-scaled.
    fn effective_ms_budget(&self, doc_body_bytes: Option<u64>) -> f64 {
        match (
            self.size_threshold_bytes,
            self.size_extra_ms_per_mib,
            doc_body_bytes,
        ) {
            (Some(threshold), Some(rate_per_mib), Some(body_bytes)) if body_bytes > threshold => {
                let excess_mib = (body_bytes - threshold) as f64 / (1024.0 * 1024.0);
                self.ms + excess_mib * rate_per_mib
            }
            _ => self.ms,
        }
    }
}

#[cfg(test)]
mod size_scaled_budget_tests {
    use super::*;

    /// A synthetic size-scaled budget exercising the `effective_ms_budget`
    /// mechanism directly (no production `[[budget]]` entry uses it as of
    /// t370.15 round 3 — see that field's doc comment). `262144` (256KiB) is
    /// the task's own example T for "a genuinely huge single document",
    /// deliberately far above any current S/M/L/JA fixture's single-document
    /// body size (JA's ~39.9KB, the largest, stays comfortably below it).
    fn scaled_budget() -> LatencyBudget {
        LatencyBudget {
            op: "doc_update_section".to_string(),
            ms: 100.0,
            expected_fail: None,
            expected_fail_scales: None,
            optional: false,
            size_threshold_bytes: Some(262_144),
            size_extra_ms_per_mib: Some(7_000.0),
        }
    }

    #[test]
    fn body_at_or_below_threshold_keeps_plain_ms_budget() {
        let b = scaled_budget();
        assert_eq!(b.effective_ms_budget(Some(6_561)), 100.0);
        assert_eq!(b.effective_ms_budget(Some(262_144)), 100.0);
    }

    /// Regression guard for the round-1 mistake: the JA fixture's actual
    /// measured `doc_slug` body (~39.9KB, see
    /// `tests/support/perf_fixture.rs::doc_body_bytes_matches_actual_doc_slug_body_on_disk`)
    /// must stay at the plain, unscaled `ms` budget — it is an ordinary-sized
    /// document, not the huge single document 案(c) was approved to relax
    /// the budget for.
    #[test]
    fn ja_fixture_body_size_is_not_large_enough_to_scale() {
        let b = scaled_budget();
        assert_eq!(b.effective_ms_budget(Some(40_826)), 100.0);
    }

    #[test]
    fn body_above_threshold_adds_proportional_extra_ms() {
        let b = scaled_budget();
        // Exactly 1 MiB past the threshold: extra = 1 * 7000 ms/MiB = 7000ms.
        let budget = b.effective_ms_budget(Some(262_144 + 1_048_576));
        assert_eq!(budget, 7_100.0);
    }

    #[test]
    fn missing_doc_body_bytes_keeps_plain_ms_budget() {
        let b = scaled_budget();
        assert_eq!(b.effective_ms_budget(None), 100.0);
    }

    #[test]
    fn unscaled_budget_ignores_body_size_entirely() {
        let b = LatencyBudget {
            op: "list_tasks".to_string(),
            ms: 50.0,
            expected_fail: None,
            expected_fail_scales: None,
            optional: false,
            size_threshold_bytes: None,
            size_extra_ms_per_mib: None,
        };
        assert_eq!(b.effective_ms_budget(Some(10_000_000)), 50.0);
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
            size_threshold_bytes: None,
            size_extra_ms_per_mib: None,
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
            size_threshold_bytes: None,
            size_extra_ms_per_mib: None,
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
        Self::spawn_inner(None)
    }

    /// Same as [`Client::spawn`], but also sets `HANDOFF_MCP_DERIVED_WRITE_LOG`
    /// (t370.14, wiki/240-performance-design.md §6 PR-8) so
    /// `measure_wchar_split` can read exact per-call derived-file write
    /// sizes/counts from the log instead of approximating them via a `stat`
    /// before/after each call.
    fn spawn_with_derived_log(log_path: &std::path::Path) -> Self {
        Self::spawn_inner(Some(log_path))
    }

    fn spawn_inner(derived_log: Option<&std::path::Path>) -> Self {
        let bin = env!("CARGO_BIN_EXE_handoff-mcp");
        let mut cmd = Command::new(bin);
        cmd.env("HANDOFF_MCP_REQUEST_TIMEOUT_SECS", "900")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(log_path) = derived_log {
            cmd.env("HANDOFF_MCP_DERIVED_WRITE_LOG", log_path);
        }
        let mut child = cmd
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
    reps: usize,
) -> HashMap<String, OpResult> {
    let p = proj.to_string_lossy().to_string();
    let scan_parent = proj
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());
    let mut results = HashMap::new();

    macro_rules! op {
        ($name:literal, $call:expr) => {
            let r = measure(client, reps, $call);
            results.insert($name.to_string(), r);
        };
    }

    // t360.20.26 (M2-S4 reviewer's "trace_suspect_clear perf op inspection")
    // used to need two sequenced, untimed single-document `handoff_doc_save`
    // calls here to individually flush `generate_suspect_seed_docs`'s
    // dedicated REQ/SPEC documents before `trace_report`/`trace_slice` below
    // ever ran the *bulk* `resync_direct_edited_layer_docs` pass on them —
    // that pass used to re-read the whole corpus fresh from *disk* to
    // resolve each cross-document `refines`/`verifies` baseline
    // (`resolve_pending_cross_doc_baselines`), so a sibling document synced
    // in the very same batch (never flushed until the end) could never be
    // found as an owner, leaving every `SPEC-99-NNN -> REQ-99-NNN` link
    // unbaselined forever (`action="list"` reported `unbaselined.links: 1522`
    // and 0 suspects with this workaround removed, pre-fix).
    //
    // t360.20.28 fixed the root cause: `resync_direct_edited_layer_docs`
    // now resolves cross-document baselines against its own in-memory
    // `DocSet`, after every layer document in the batch has already run
    // through its local sync once — never by re-reading disk. The very
    // first `trace_report`/`trace_slice` call below is once again the real,
    // representative "first bulk resync of a from-scratch corpus" this op
    // is meant to measure (PR-7), so the pre-warming workaround that used to
    // sit here is gone.
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
    // M2-18 (wiki/260-vmodel-m2-design.md §4.12/§6, FR-804): the default
    // (dry_run) call scans the same corpus `doc_list` already does — no
    // extra read beyond what `read_all_docs_with_unreadable` shares with it
    // — so it is budgeted the same as `doc_list` itself (PR-5).
    op!("doc_repair_frontmatter_dry_run", |c: &mut Client, _i| {
        let (dt, io, _) = c.call("handoff_doc_repair_frontmatter", json!({"project_dir": p}));
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

    // t360.20.13 (M2-13, wiki/260-vmodel-m2-design.md §3.4/§6 PR-1):
    // `update_task_status_done_guard_{warn,block}` — the done guard's own
    // added cost on top of `update_task_status_with_links` above, for a
    // `todo` -> `review` transition (the guarded direction; `review` ->
    // `todo` is untimed cleanup, same `update_task_requirement_ids_toggle`
    // pattern below). `hot_req_task`'s own linked SubItems carry no `layer`
    // (this is the M1-style base fixture, not the dedicated 2,500-item
    // `trace` fixture `trace_task_id` uses) — `compute_task_blockers_for_task`
    // still runs its full reverse-verifies scan and `TraceGraph::build` over
    // the small per-task item set either way (§3.4: "グラフ全体を作らずにで
    // きる" the point of this op is to measure *that* path's cost, which is
    // independent of whether a real blocker is actually found; finding zero
    // blockers lets the transition succeed every rep, keeping this a stable
    // `todo` <-> `review` round-trip). `estimate_hours` is already set on
    // every fixture task (`layer_document_body`'s own generation above), and
    // `review` never needs `done_criteria` checked (`validate_done_transition`
    // only gates `done`), so the round-trip never fails validation.
    op!(
        "update_task_status_done_guard_warn",
        |c: &mut Client, _i| {
            // `[trace] done_guard` defaults to `"warn"` (no `config.toml`
            // `[trace]` section at all in this fixture) — no config write
            // needed for this op.
            let (dt, io, _) = c.call(
                "handoff_update_task",
                json!({"project_dir": p, "task": {"id": meta.hot_req_task, "status": "review"}}),
            );
            c.call(
                "handoff_update_task",
                json!({"project_dir": p, "task": {"id": meta.hot_req_task, "status": "todo"}}),
            );
            (dt, io)
        }
    );
    {
        // `block` mode needs its own `[trace] done_guard = "block"` —
        // written directly to `config.toml` (not through an MCP call, so it
        // isn't itself part of any measured op) and reverted immediately
        // after this one op, so every other op in this suite keeps seeing
        // the default `"warn"`.
        let config_path = proj.join(".handoff").join("config.toml");
        let mut config = handoff_mcp::storage::config::read_config(&config_path)
            .expect("read config.toml for done_guard block setup");
        config.trace.done_guard = "block".to_string();
        handoff_mcp::storage::config::write_config(&config_path, &config)
            .expect("write config.toml with done_guard=block");

        op!(
            "update_task_status_done_guard_block",
            |c: &mut Client, _i| {
                let (dt, io, _) = c.call(
                    "handoff_update_task",
                    json!({"project_dir": p, "task": {"id": meta.hot_req_task, "status": "review"}}),
                );
                c.call(
                    "handoff_update_task",
                    json!({"project_dir": p, "task": {"id": meta.hot_req_task, "status": "todo"}}),
                );
                (dt, io)
            }
        );

        let mut config = handoff_mcp::storage::config::read_config(&config_path)
            .expect("read config.toml to revert done_guard");
        config.trace.done_guard = "warn".to_string();
        handoff_mcp::storage::config::write_config(&config_path, &config)
            .expect("revert config.toml done_guard to warn");
    }
    // t360.20.13 rework (review round 2 MAJOR, wiki/260 §3.4/§4.11, PR-6
    // ≤50ms): `get_task`/`task_checklist(view)`'s own `trace` field
    // (`load_task_trace_view`, `src/mcp/handlers/get_task.rs`) and
    // `list_tasks(layer=...)`'s filter (`src/mcp/handlers/list_tasks.rs`)
    // each pay for a `DocSet::load` (full-corpus read) that a plain
    // `get_task`/`task_checklist`/`list_tasks` call never did before M2-13 —
    // measured the same way PR-1's `update_task_status_done_guard_{warn,block}`
    // already is above, so a regression in either new path is caught.
    // `meta.hot_req_task` already carries a `requirement`-type `task_links`
    // entry (set up earlier in this function), so `get_task`/`task_checklist`
    // take the `trace`-computing branch rather than its PR-2 no-link fast
    // path.
    op!("get_task_with_trace", |c: &mut Client, _i| {
        let (dt, io, _) = c.call(
            "handoff_get_task",
            json!({"project_dir": p, "task_id": meta.hot_req_task}),
        );
        (dt, io)
    });
    op!("task_checklist_view_with_trace", |c: &mut Client, _i| {
        let (dt, io, _) = c.call(
            "handoff_task_checklist",
            json!({"project_dir": p, "task_id": meta.hot_req_task}),
        );
        (dt, io)
    });
    op!("list_tasks_layer_filter", |c: &mut Client, _i| {
        let (dt, io, _) = c.call(
            "handoff_list_tasks",
            json!({"project_dir": p, "layer": "requirement"}),
        );
        (dt, io)
    });

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

        // t370.10 (PR-3): `update_task_requirement_ids_toggle` above spends
        // two separate `handoff_update_task` round trips per rep (an
        // add-only call, then a remove-only call) — neither call on its own
        // exercises the add+remove-in-one-call "swap" diff path
        // (`apply_requirement_ids_diff`'s P-M3 single-DocSet-pass, and
        // t370.10's own combined status+requirement_ids path when
        // `swapped`'s status also changes — see
        // `run_ops`/`update_task_status_with_links` for that case, which
        // reuses the same `hot_req_task`). This op does exactly one
        // `handoff_update_task` call per rep whose `requirement_ids` both
        // drops one of `hot_req_ids` and adds `extra_stable_id` — a genuine
        // single-call swap, alternating direction each rep so every rep is a
        // real diff (not a no-op repeat of the previous rep's state).
        if !base_ids.is_empty() {
            let mut swapped_ids = base_ids.clone();
            swapped_ids.pop();
            swapped_ids.push(extra.clone());
            op!(
                "update_task_requirement_ids_swap",
                |c: &mut Client, i: usize| {
                    let ids = if i % 2 == 0 {
                        swapped_ids.clone()
                    } else {
                        base_ids.clone()
                    };
                    let (dt, io, _) = c.call(
                    "handoff_update_task",
                    json!({"project_dir": p, "task": {"id": meta.hot_req_task, "requirement_ids": ids}}),
                );
                    (dt, io)
                }
            );
        }
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
    // t360.8 (wiki/220 §2.6/§3.1, PR-4 target ≤100ms): one handoff_trace_record
    // call recording one result against a real, resolvable stable_id — the
    // corpus-wide body_hash lookup (`storage::runs::record_run`'s scan of
    // `read_all_docs`) plus the runs/_latest.json sync are the two costs this
    // op measures.
    if let Some(stable_id) = meta.hot_req_ids.first().cloned() {
        op!("trace_record", |c: &mut Client, i: usize| {
            let result = if i % 2 == 0 { "pass" } else { "fail" };
            let (dt, io, _) = c.call(
                "handoff_trace_record",
                json!({"project_dir": p, "results": [{"item": stable_id, "result": result}]}),
            );
            (dt, io)
        });
    }
    // M2-11 (wiki/260-vmodel-m2-design.md §4.6, PR-4 target ≤100ms): one
    // handoff_trace_ingest call with a single matched cargo_json result
    // against the fixture's scale-independent layer document
    // (`FixtureMeta::layer_doc_slug`'s `SPEC-000`, matched via the legacy
    // stable_id->prefix convention, stage 3 — no `test:` attribute in that
    // fixture). Exercises the same corpus-wide candidate scan +
    // `record_run` cost `trace_record` measures, plus this op's own parsing
    // and matching pass.
    op!("trace_ingest", |c: &mut Client, _i: usize| {
        let (dt, io, _) = c.call(
            "handoff_trace_ingest",
            json!({
                "project_dir": p,
                "format": "cargo_json",
                "output": "{\"type\":\"test\",\"event\":\"ok\",\"name\":\"test_spec_000_case\"}",
            }),
        );
        (dt, io)
    });
    // M2-12 (wiki/260-vmodel-m2-design.md §4.7, PR-4 target ≤100ms):
    // `mode="preview"` against the fixed 2,500-item trace fixture's
    // `bench-trace-req-00` (100 items, no acceptance-criteria blocks — the
    // fixture predates M2-02) as `doc` and `bench-trace-at-00` as
    // `target_doc` (both always generated, regardless of S/M/L/JA scale —
    // `FixtureMeta`'s `generate_trace_scale_docs` doc comment). No item
    // there actually has an acceptance-criteria block, so `generated` is
    // always empty and nothing is written even in `apply` mode — this
    // deliberately measures trace_scaffold's *read* cost, which is what
    // dominates regardless of how many items end up generated: the
    // project-wide `existing_from`/`existing_ids` scan (same shape as
    // `trace_record`/`trace_ingest`'s own corpus-wide scans above, already
    // proven within budget at this scale) plus a re-parse of the two named
    // documents (100 + 60 items) to resolve `target_doc`'s heading level and
    // `doc`'s eligible source items. `apply` mode's own extra cost (one
    // document write + layer sync) is not measured by a second op here —
    // it is exactly the same `doc_save`(`append_body`)-driven write path
    // the `doc_update_section`/`doc_save_layer_metadata` ops below already
    // budget under PR-4.
    op!("trace_scaffold", |c: &mut Client, _i: usize| {
        let (dt, io, _) = c.call(
            "handoff_trace_scaffold",
            json!({
                "project_dir": p,
                "doc": "bench-trace-req-00",
                "target_doc": "bench-trace-at-00",
                "mode": "preview",
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
    // M1 t360.6 (wiki/220-vmodel-integration-design.md §2.4, wiki/240 §5-3):
    // `doc_save_layer_metadata` is a metadata-only save (no body/append_body)
    // on the fixture's small, scale-independent layer document
    // (`FixtureMeta::layer_doc_slug` — fixed item count regardless of S/M/L/JA,
    // see its doc comment). The first (untimed warm-up) call performs the
    // document's only real `sync_layer_items` parse+rebuild and records
    // `source.body_raw_hash`; every timed rep after that is a byte-identical
    // body, so `sync_layer_items_if_needed`'s raw-hash short-circuit should
    // keep this as cheap as any other metadata-only PR-4 op
    // (`doc_verify_set_dev_stage` et al.) rather than re-parsing on every call.
    op!("doc_save_layer_metadata", |c: &mut Client, _i: usize| {
        let (dt, io, _) = c.call(
            "handoff_doc_save",
            json!({"project_dir": p, "doc_id": meta.layer_doc_id, "tags": ["bench-layer"]}),
        );
        (dt, io)
    });
    // `doc_update_section_layer`: the worst case for the same document —
    // every rep replaces the whole section with a byte-different body
    // (`layer_document_body`'s `variant` = `i`), so `sync_layer_items` always
    // does a real reparse+rebuild pass. Deliberately scale-independent (same
    // fixed `LAYER_ITEM_COUNT` at every S/M/L/JA scale) so this op's budget
    // does not inherit the plain `doc_update_section` op's existing JA
    // `expected_fail` (that gap is `lexsim::content_hash`'s per-byte JA
    // tokenization cost on a *large*, scale-proportional body — orthogonal to
    // what `body_raw_hash` optimizes here, see `FixtureMeta::layer_doc_slug`'s
    // doc comment).
    op!("doc_update_section_layer", |c: &mut Client, i: usize| {
        let content =
            perf_fixture::layer_document_body(meta.layer_lang, perf_fixture::LAYER_BODY_SEED, i);
        let (dt, io, _) = c.call(
            "handoff_doc_update_section",
            json!({"project_dir": p, "doc_id": meta.layer_doc_slug, "seq": meta.layer_section_seq, "new_content": content}),
        );
        (dt, io)
    });

    // M1 t360.10 (wiki/220-vmodel-integration-design.md §3.2, wiki/240 §6
    // PR-7, NFR-003 "2,500 項目 / 30 文書"): full trace derivation report over
    // `FixtureMeta::trace_task_id`'s 30-document, 2,500-item fixture. The
    // first (untimed warm-up) call pays the one real `sync_layer_items` parse
    // of every document (`body_raw_hash` unset yet); every timed rep after
    // that hits the raw-hash short-circuit, so this measures graph-build +
    // aggregation cost, not parse cost (the parse-cost worst case is exactly
    // what `doc_update_section_layer` above already covers on the small,
    // scale-independent single-doc fixture).
    op!("trace_report", |c: &mut Client, _i: usize| {
        let (dt, io, _) = c.call(
            "handoff_trace_report",
            json!({"project_dir": p, "include_items": true}),
        );
        (dt, io)
    });
    // M1 t360.11 (wiki/220 §3.3, wiki/240 §6 PR-7): progressive-disclosure
    // neighborhood slice from `trace_slice_item_id` (`REQ-00-000`, which has
    // both a refining child and a verifier — a non-trivial `both`-direction
    // traversal), `max_items` left at its default (30) so this also exercises
    // the BFS's own truncation path on a large graph.
    op!("trace_slice", |c: &mut Client, _i: usize| {
        let (dt, io, _) = c.call(
            "handoff_trace_slice",
            json!({"project_dir": p, "item": meta.trace_slice_item_id, "direction": "both"}),
        );
        (dt, io)
    });
    // M2-05 rework (review round 1, MAJOR, wiki/240 §7's "suspect（上流を変
    // えた文書）"): make `perf_fixture::SUSPECT_LINK_COUNT` `SPEC-99-NNN`
    // links suspect via one untimed, real (resync-triggering)
    // `handoff_doc_save` full-body rewrite of `suspect_req_doc_slug` — this
    // runs *after* `trace_report`/`trace_slice` above have already paid the
    // one-time full-corpus sync (so their own measured medians are
    // unaffected) and *before* the `trace_suspect_*` ops below, so both
    // `list` (which never resyncs, E6) and `clear`/`baseline` (which do,
    // this task's own fix) see the same real, already-synced suspects.
    client.call(
        "handoff_doc_save",
        json!({
            "project_dir": p,
            "doc_id": perf_fixture::suspect_req_doc_id(),
            "body": perf_fixture::suspect_req_body(meta.layer_lang, 1),
        }),
    );

    // M2-05 (wiki/260-vmodel-m2-design.md §3.2/§6, PR-7 "< 1 s"):
    // `action="list"` folds suspect derivation into the same
    // `TraceGraph::build` `trace_report`/`trace_slice` above also use
    // ("グラフ構築のついでに", an O(リンク数) suspect scan) — but goes
    // through `load_trace_input` directly instead of `rebuild_trace_graph`
    // (E6/§4.1 "読み取り専用": no resync-direct-edited-docs, no `task_ids`
    // self-repair, no `_trace_report.json` write), so it measures markedly
    // cheaper than either of those two on this same fixed 2,500-item/
    // 30-document fixture — see `perf_budgets.toml`'s entry for the actual
    // numbers. Now (rework round 1) exercises `perf_fixture::SUSPECT_LINK_COUNT`
    // real suspects, seeded just above.
    op!("trace_suspect_list", |c: &mut Client, _i: usize| {
        let (dt, io, _) = c.call(
            "handoff_trace_suspect",
            json!({"project_dir": p, "action": "list"}),
        );
        (dt, io)
    });
    // M2-05 rework (review round 1, MAJOR, wiki/260 §4.1, PR-4 target
    // ≤100ms): `action="clear"` against one exact, real, still-suspect
    // `{item, upstream}` link per call — a genuine 1-document RMW + 1
    // audit-file write every time, not the "no suspect actually matches"
    // read-only probe this op used pre-fix. `measure()` calls this closure
    // once as an untimed warm-up (its own internal index 0) and then once
    // per timed rep (`0..reps`, i.e. index 0 *again* for the first timed
    // rep) — a plain `_i`-keyed target would let the warm-up and the first
    // timed rep both consume the *same* link, undermeasuring rep 0. A
    // monotonic `Cell` counter instead advances on every single call
    // (warm-up included), so every call consumes a distinct link from
    // `perf_fixture::SUSPECT_LINK_COUNT` — see that constant's own doc
    // comment for the `>= max(REPS, RATIO_REPS) + 1` invariant this relies
    // on (t360.20.26: `run_ops` also executes this op, at `RATIO_REPS`
    // reps, whenever *any* op's `check_scale_ratio` budget is checked — not
    // just when `trace_suspect_clear` itself is the op under test — so this
    // closure must never run out of real, distinct suspect links to target).
    //
    // t360.20.26 (M2-S4 reviewer): assert the response actually reports
    // exactly one cleared link, so a future fixture/index mismatch that
    // makes `n` overrun the seeded links surfaces as a hard failure here
    // (a "no suspect actually matches" no-op) instead of silently measuring
    // a cheaper, unrepresentative read-only probe.
    let suspect_clear_call_index = std::cell::Cell::new(0usize);
    op!("trace_suspect_clear", |c: &mut Client, _i: usize| {
        let n = suspect_clear_call_index.get();
        suspect_clear_call_index.set(n + 1);
        let (dt, io, text) = c.call(
            "handoff_trace_suspect",
            json!({
                "project_dir": p, "action": "clear",
                "targets": [{
                    "item": perf_fixture::suspect_spec_id(n),
                    "upstream": perf_fixture::suspect_req_id(n),
                }],
                "reason": "perf_budget probe (real suspect match + RMW + audit write)",
            }),
        );
        let resp: Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("trace_suspect_clear: invalid JSON response: {e}: {text}"));
        let cleared_links = resp
            .get("cleared")
            .and_then(|c| c.get("links"))
            .and_then(|v| v.as_u64());
        assert_eq!(
            cleared_links,
            Some(1),
            "trace_suspect_clear call #{n} (item={}, upstream={}) must clear exactly 1 \
             real suspect link, not a no-op — response was: {text}",
            perf_fixture::suspect_spec_id(n),
            perf_fixture::suspect_req_id(n),
        );
        (dt, io)
    });
    // M2-05 rework (review round 1, MAJOR, wiki/260 §6): `trace_suspect
    // baseline` is grouped under the same PR-7 "< 1 s" row as `trace_report`
    // / `trace_slice` / `trace_suspect list` — no op previously existed for
    // it at all. `dry_run` (the default, and the only mode this probe uses —
    // `dry_run=false`'s extra per-link RMW cost is the same small,
    // already-budgeted write shape `trace_suspect_clear` above and
    // `doc_update_section`/`update_task` elsewhere in this file cover)
    // shares the same `load_trace_input`+`TraceGraph::build` cost `list`
    // does, scanning `graph.unbaselined_links()`/`unbaselined_tasks()`
    // instead of `graph.suspects()` — this fixture's corpus is fully
    // baselined by the time this runs (every link either gets a real
    // baseline at its first sync or is one of the `SUSPECT_LINK_COUNT`
    // *stale*-but-still-baselined links above, never literally unbaselined),
    // so `baselined.links/tasks` are 0 here; the measured cost is still the
    // representative one (the full O(links) scan), not a 0-cost short
    // circuit for an empty scope.
    op!("trace_suspect_baseline", |c: &mut Client, _i: usize| {
        let (dt, io, _) = c.call(
            "handoff_trace_suspect",
            json!({"project_dir": p, "action": "baseline"}),
        );
        (dt, io)
    });

    // M2-06 (wiki/260-vmodel-m2-design.md §4.2/§6, PR-7 "< 1 s"): `item` mode
    // with neither `proposed` nor `proposed_file` (the "assume changed"
    // path, §4.2) against `trace_slice_item_id` — an item with both a
    // refining child and a verifier (`FixtureMeta::trace_slice_item_id`'s own
    // doc comment), so this exercises a non-trivial `would_suspect`/
    // `rerun_candidates`/`potential` walk, not a leaf with no dependents.
    // Stays on `load_trace_input_read_only` (E6, rework round 2 fix — plain
    // read of `runs/_latest.json`, never `runs::sync`'s possible write),
    // never `rebuild_trace_graph` — no resync, no `task_ids` self-repair, no
    // derived-file write of any kind.
    op!("trace_impact", |c: &mut Client, _i: usize| {
        let (dt, io, _) = c.call(
            "handoff_trace_impact",
            json!({"project_dir": p, "item": meta.trace_slice_item_id}),
        );
        (dt, io)
    });

    // M2-08 (wiki/260-vmodel-m2-design.md §4.3/§6, PR-7 "< 1 s"): `handoff_trace_lint`
    // grouped under the same PR-7 row as `trace_report`/`trace_slice`/
    // `trace_suspect list`/`trace_suspect baseline`/`trace_impact` above —
    // E6's fully read-only load (`trace_readonly::load_trace_input_fully_read_only`:
    // in-memory-only layer-doc resync, `runs::load_latest_readonly`, `task_ids`
    // resolved from the task side without self-repair) plus one
    // `TraceGraph::build` and a single O(gaps + suspects + items) rule-evaluation
    // pass over the same 2,500-item/30-document fixture. No arguments
    // (every built-in rule runs), so this also exercises the full rule set
    // at fixture scale, not a `rules`-filtered subset.
    op!("trace_lint", |c: &mut Client, _i: usize| {
        let (dt, io, _) = c.call("handoff_trace_lint", json!({"project_dir": p}));
        (dt, io)
    });

    // M2-17 (wiki/260-vmodel-m2-design.md §4.10/§6, PR-5 "≤ 150 ms"):
    // `task_id` mode against `meta.trace_task_id` — the same 2,500-item/
    // 30-doc trace-scale fixture `trace_impact`/`trace_suspect_list` above
    // measure against, so the `candidates` scan (lexical prefilter over
    // every item title, then a bounded semantic rerank) is exercised at the
    // fixture's full corpus size, not a handful of items. No `runs::sync`, no layer
    // resync, no derived-file write at all (read_all_docs + read_config +
    // one task read only) — expected to land well under every other PR-7
    // op's 1000ms budget given its own, tighter 150ms target.
    op!("trace_propose", |c: &mut Client, _i: usize| {
        let (dt, io, _) = c.call(
            "handoff_trace_propose",
            json!({"project_dir": p, "task_id": meta.trace_task_id}),
        );
        (dt, io)
    });

    // M2-09 (wiki/260-vmodel-m2-design.md §4.4/§6, PR-7 "< 1 s"): default
    // `shape: "tree"`/`format: "csv"` (no `layers`/`root_layer` override) —
    // the heaviest shape this op has, since every one of the fixture's
    // root-layer (`requirement`) items gets its own `collect_downward` BFS
    // over `graph.refines_children`/`graph.verified_by`, together visiting
    // every valid edge in the 2,500-item/30-document fixture exactly once in
    // aggregate (same total edge-traversal cost `TraceGraph::build`'s own
    // gap/coverage pass already pays, just walked a second time). Stays on
    // the same E6 fully-read-only load (`trace_readonly::
    // load_trace_input_fully_read_only`) `trace_lint`/`trace_impact` above
    // use, so this is expected to land close to `trace_lint`'s cost plus one
    // additional linear BFS pass — not `trace_report`'s resync+self-repair+
    // file-write cost.
    op!("trace_matrix", |c: &mut Client, _i: usize| {
        let (dt, io, _) = c.call(
            "handoff_trace_matrix",
            json!({"project_dir": p, "format": "csv"}),
        );
        (dt, io)
    });

    results
}

/// I/O budget (PR-8, wiki §6): the read volume of `update_task_status_with_links`
/// must stay within (target doc bytes for every doc `hot_req_task`'s
/// requirement links actually touch, plus target task bytes for
/// `hot_req_task` and every task it shares a SubItem with) + 256 KiB.
///
/// t370.12 rework (MAJOR, integration feedback round 1): this previously
/// only budgeted for `meta.doc_slug` (`docs_meta[0]`) and `hot_req_task`'s
/// own file. Two real gaps: (1) `hot_req_task`'s requirement links
/// deliberately span two docs (`FixtureMeta::hot_req_task`'s doc comment —
/// docs 0/1), so `propagate_dev_stage_for_task` legitimately reads/writes
/// both when the task's status/requirement_ids change; (2)
/// `propagate_dev_stage_for_task`'s min-of-linked-tasks dev_stage
/// computation reads every co-linked task's own status file too
/// (`hot_colinked_tasks`), not just `hot_req_task`'s. Both are
/// budget-formula/fixture-linkage mismatches, not redundant reads of the
/// same file — summing every doc/task actually touched (rather than
/// hardcoding one of each) fixes the formula to match reality.
fn io_budget_bytes(proj: &std::path::Path, meta: &FixtureMeta) -> u64 {
    let docs_bytes: u64 = meta
        .hot_req_doc_slugs
        .iter()
        .map(|slug| {
            let doc_path = proj
                .join(".handoff")
                .join("docs")
                .join(format!("_doc.{slug}.md"));
            std::fs::metadata(&doc_path).map(|m| m.len()).unwrap_or(0)
        })
        .sum();
    let task_bytes: u64 = std::iter::once(&meta.hot_req_task)
        .chain(meta.hot_colinked_tasks.iter())
        .map(|t| find_task_file_size(proj, t).unwrap_or(0))
        .sum();
    docs_bytes + task_bytes + 256 * 1024
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
///
/// t370.14 (decision 2026-09-27 案(a), wiki/240-performance-design.md §6
/// PR-8): this budget is checked against `wchar` with derived-file
/// (`_requirements_summary.json` today) rewrites *excluded* — a derived
/// file's size scales with the whole requirements corpus (P-M4 note in
/// `tests/perf_budgets.toml`: up to ~0.7-0.9MB at M/L/JA's SubItem counts),
/// not with the single target doc/task this per-item envelope is about, so
/// folding it into this budget would either make the budget meaningless
/// (huge) or permanently unmeetable (small). Derived files instead follow a
/// separate discipline (wiki §4 P-M4): unchanged content is never
/// rewritten, and at most one write happens per request — see
/// `measure_wchar_split` below and `tests/derived_summary_write_discipline.rs`.
fn wchar_budget_bytes(proj: &std::path::Path, meta: &FixtureMeta) -> u64 {
    io_budget_bytes(proj, meta)
}

/// Reads the `HANDOFF_MCP_DERIVED_WRITE_LOG` file a `Client` spawned via
/// [`Client::spawn_with_derived_log`] writes to
/// (`src/mcp/handlers/docs.rs`'s `record_derived_write_for_test`): one
/// `"{path}\t{bytes}"` line per actual (non-skipped) derived-file write,
/// across the process's whole lifetime. Bracketing a single request's slice
/// of this log (by line count before/after that one call) gives an exact
/// write count and byte size for that request — precise, unlike a `stat`
/// before/after (which can't distinguish "written once" from "written twice
/// back to the same final content").
struct DerivedWriteLog {
    path: std::path::PathBuf,
}

impl DerivedWriteLog {
    fn new(path: std::path::PathBuf) -> Self {
        let _ = std::fs::remove_file(&path); // start from a clean, known-empty log
        DerivedWriteLog { path }
    }

    /// Every write logged so far, as `(path, bytes_written)` pairs, in the
    /// order they happened.
    fn entries(&self) -> Vec<(String, u64)> {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|line| {
                let (p, b) = line.split_once('\t')?;
                Some((p.to_string(), b.parse().ok()?))
            })
            .collect()
    }
}

/// Median `wchar` after subtracting the derived-file bytes this call wrote,
/// plus the median derived-file byte count (reported, not budget-gated —
/// see `wchar_budget_bytes`'s doc comment for why).
struct WcharSplit {
    non_derived_median: u64,
    derived_median: u64,
}

/// PR-8 write-budget companion (t370.14): re-runs
/// `update_task_status_with_links` (warm-up + `REPS`, same shape as
/// [`measure`]) in isolation from `run_ops`'s shared measurement — using a
/// *separate* `Client` spawned with `HANDOFF_MCP_DERIVED_WRITE_LOG` set —
/// pairing each call's `/proc/<pid>/io` wchar delta with the exact
/// derived-file write(s) that same call caused (per [`DerivedWriteLog`]).
/// Also asserts PR-8's "at most 1 file, 1 write per request" discipline
/// directly, across every rep (not just the reported median): this op's
/// fixture setup deliberately makes every alternating status change flip
/// the linked SubItem's derived `dev_stage` (`FixtureMeta::hot_req_task`'s
/// doc comment), so every rep here is expected to write the derived file
/// *exactly* once, never more.
fn measure_wchar_split(
    client: &mut Client,
    proj: &std::path::Path,
    meta: &FixtureMeta,
    log: &DerivedWriteLog,
) -> WcharSplit {
    let p = proj.to_string_lossy().to_string();
    let run_one = |client: &mut Client, i: usize| -> (u64, u64, usize) {
        let status = if i % 2 == 0 { "in_progress" } else { "todo" };
        let before = log.entries().len();
        let (_dt, io, _) = client.call(
            "handoff_update_task",
            json!({"project_dir": p, "task": {"id": meta.hot_req_task, "status": status}}),
        );
        let entries = log.entries();
        let new_entries = &entries[before..];
        let derived: u64 = new_entries.iter().map(|(_, bytes)| *bytes).sum();
        (io.wchar, derived, new_entries.len())
    };

    run_one(client, 0); // warm-up, discarded (mirrors `measure`)
    let mut samples: Vec<(u64, u64, usize)> = Vec::with_capacity(REPS);
    for i in 0..REPS {
        samples.push(run_one(client, i));
    }

    for (_, _, writes) in &samples {
        assert!(
            *writes <= 1,
            "update_task_status_with_links wrote the derived requirements summary {writes} \
             times in a single request — PR-8 (wiki/240 §6) requires at most 1 write per \
             request"
        );
    }

    samples.sort_by_key(|(wchar, _, _)| *wchar);
    let (median_wchar, median_derived, _) = samples[samples.len() / 2];
    WcharSplit {
        non_derived_median: median_wchar.saturating_sub(median_derived),
        derived_median: median_derived,
    }
}

#[cfg(test)]
mod io_budget_bytes_tests {
    use super::*;

    /// t370.12 rework (MAJOR follow-up, integration feedback round 1):
    /// `propagate_dev_stage_for_task` reads every co-linked task's status
    /// file too (min-of-linked-tasks dev_stage computation over
    /// `SubItem.task_ids`, `src/mcp/handlers/docs.rs`), not just
    /// `hot_req_task`'s own — `io_budget_bytes` must budget for
    /// `hot_colinked_tasks` bytes as well, or the formula still undercounts
    /// real I/O even after the doc-count fix.
    #[test]
    fn sums_bytes_for_colinked_tasks_too() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        let meta = generate(&proj, &FixtureOpts::s()).expect("generate fixture");
        assert!(!meta.hot_colinked_tasks.is_empty());

        let mut expected_task_bytes = find_task_file_size(&proj, &meta.hot_req_task).unwrap();
        for t in &meta.hot_colinked_tasks {
            expected_task_bytes += find_task_file_size(&proj, t).unwrap();
        }
        let expected_docs_bytes: u64 = meta
            .hot_req_doc_slugs
            .iter()
            .map(|slug| {
                let p = proj
                    .join(".handoff")
                    .join("docs")
                    .join(format!("_doc.{slug}.md"));
                std::fs::metadata(&p).unwrap().len()
            })
            .sum();

        let budget = io_budget_bytes(&proj, &meta);
        assert_eq!(
            budget,
            expected_docs_bytes + expected_task_bytes + 256 * 1024,
            "io_budget_bytes must also cover every hot_colinked_tasks entry's file bytes"
        );
    }

    /// t370.12 rework (MAJOR, integration feedback round 1): `io_budget_bytes`
    /// must sum bytes for *every* doc `hot_req_task`'s requirement links
    /// touch (`meta.hot_req_doc_slugs`), not just `meta.doc_slug` — the JA
    /// fixture's `hot_req_task` genuinely spans two ~105KB documents, so a
    /// budget that only accounts for one was never going to fit.
    #[test]
    fn sums_bytes_for_every_hot_req_doc_not_just_doc_slug() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        let meta = generate(&proj, &FixtureOpts::s()).expect("generate fixture");

        assert!(
            meta.hot_req_doc_slugs.len() >= 2,
            "fixture must link hot_req_task across at least 2 docs for this test to be \
             meaningful (got {:?})",
            meta.hot_req_doc_slugs
        );
        assert!(
            meta.hot_req_doc_slugs
                .iter()
                .any(|slug| slug != &meta.doc_slug),
            "doc_slug alone must not already cover every hot_req_doc_slugs entry, or this \
             test can't distinguish the fixed formula from the old one"
        );

        let expected_docs_bytes: u64 = meta
            .hot_req_doc_slugs
            .iter()
            .map(|slug| {
                let p = proj
                    .join(".handoff")
                    .join("docs")
                    .join(format!("_doc.{slug}.md"));
                std::fs::metadata(&p).unwrap().len()
            })
            .sum();
        let expected_task_bytes: u64 = std::iter::once(&meta.hot_req_task)
            .chain(meta.hot_colinked_tasks.iter())
            .map(|t| find_task_file_size(&proj, t).unwrap())
            .sum();

        let budget = io_budget_bytes(&proj, &meta);
        assert_eq!(
            budget,
            expected_docs_bytes + expected_task_bytes + 256 * 1024,
            "io_budget_bytes must cover every doc in hot_req_doc_slugs, not just doc_slug"
        );
    }
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
    let results = run_ops(&mut client, &proj, &meta, REPS);
    client.close();

    // t370.14: a fresh `Client` (own process, own `HANDOFF_MCP_DERIVED_WRITE_LOG`)
    // dedicated to the derived-file wchar split — kept separate from `run_ops`'s
    // shared measurement above so this doesn't disturb its op set or add the
    // log env var to every other op's process.
    let derived_log = DerivedWriteLog::new(tmp.path().join("derived_writes.log"));
    let mut wchar_client = Client::spawn_with_derived_log(&derived_log.path);
    let wchar_split = measure_wchar_split(&mut wchar_client, &proj, &meta, &derived_log);
    wchar_client.close();

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
        // t370.15: `Some(meta.doc_body_bytes)` is passed unconditionally —
        // `effective_ms_budget` only scales a budget that has opted in via
        // both `size_threshold_bytes`/`size_extra_ms_per_mib` (currently
        // just `doc_update_section`), so every other op's budget is
        // untouched by this.
        let effective_budget =
            budget.effective_ms_budget(Some(meta.doc_body_bytes as u64)) * slack_mult;
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
    }

    // Write-side counterpart (wiki §6 PR-8: "内容が変わらないファイルは
    // 書かない"). t370.14 (decision 2026-09-27 案(a)): checked against
    // `wchar_split.non_derived_median` — the derived
    // `_requirements_summary.json` rewrite this same call also causes is
    // excluded from the budget (see `wchar_budget_bytes`'s doc comment) and
    // reported separately instead. Measured via a dedicated `Client`
    // (`measure_wchar_split` above), not `results`, so this check does not
    // depend on `update_task_status_with_links` being present in `results`.
    let wchar_entry = budgets
        .budget
        .iter()
        .find(|b| b.op == "io_wchar_update_task_status_with_links");
    let wchar_within = (wchar_split.non_derived_median as f64) <= (wchar_budget as f64);
    table.push_str(&format!(
        "io: update_task_status_with_links wchar={}KB (derived {}KB excluded, written \
         separately per-request — see PR-8 write discipline) budget={}KB -> {}\n",
        wchar_split.non_derived_median / 1024,
        wchar_split.derived_median / 1024,
        wchar_budget / 1024,
        if wchar_within { "ok" } else { "over" }
    ));
    match classify_io_check(wchar_within, wchar_entry, scale_name) {
        ("FAIL", _) => failures.push(format!(
            "io_wchar_update_task_status_with_links: non-derived wchar {}KB exceeds budget \
             {}KB (derived {}KB excluded)",
            wchar_split.non_derived_median / 1024,
            wchar_budget / 1024,
            wchar_split.derived_median / 1024
        )),
        ("expected-fail", Some(reason)) => table.push_str(&format!("  expected-fail: {reason}\n")),
        ("PROMOTE?", _) => {
            table.push_str("  PROMOTE?: wchar now within budget — remove expected_fail\n")
        }
        _ => {}
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

/// Reps used for [`check_scale_ratio`]'s two [`measure_single_scale`] calls —
/// 3x [`REPS`] (t370.14). M-S6 tester report: `scale_ratio_d`
/// (`doc_verify_set_dev_stage`) intermittently failed with both sides
/// measured at 6-13ms (ratio 1.62/2.29 vs the 1.50 budget) — this op is
/// deliberately fast (that's the point of the P-M2/P-M3 fix it guards), so
/// its p50 sits close to scheduler-jitter/container-noise territory where 7
/// samples' median is not yet stable. More reps (taking the same p50-of-N
/// approach, just with a larger N) tightens that without touching what's
/// actually being asserted.
const RATIO_REPS: usize = REPS * 3;

/// t360.20.26 (M2-S4 reviewer): `run_ops` (and therefore its
/// `trace_suspect_clear` op) runs at `RATIO_REPS` reps whenever *any* op's
/// `check_scale_ratio` budget is checked (`measure_single_scale` -> `run_ops`
/// runs every registered op, not just the one the ratio check is named
/// after) — not only at plain `REPS`. `perf_fixture::SUSPECT_LINK_COUNT` must
/// therefore cover a total of `max(REPS, RATIO_REPS) plus one` distinct calls
/// (warm-up plus reps, `measure`'s own indexing), not just `REPS plus one`,
/// or a `check_scale_ratio` run overruns the seeded suspect links and
/// `trace_suspect_clear`'s "cleared exactly 1 link" assertion above starts
/// failing (or, pre-that-assertion, silently degrades into an unrepresentative
/// no-op read). This is a real cross-file compile-time check — see
/// `perf_fixture::SUSPECT_LINK_COUNT`'s own doc comment for why its value
/// can't just be *this* expression directly (that module is also `#[path]`-
/// shared by `derived_summary_write_discipline.rs`, which never defines
/// `RATIO_REPS`).
const _SUSPECT_LINK_COUNT_COVERS_MAX_OF_REPS_AND_RATIO_REPS: () = assert!(
    perf_fixture::SUSPECT_LINK_COUNT > (if REPS > RATIO_REPS { REPS } else { RATIO_REPS }),
    "perf_fixture::SUSPECT_LINK_COUNT must be >= max(REPS, RATIO_REPS) + 1 — bump it to match \
     if REPS or RATIO_REPS grows"
);

/// Below this, a single-digit-ms measurement's *ratio* to another
/// single-digit-ms measurement is dominated by measurement noise, not by
/// the scaling behavior PR-9 exists to catch (t370.14, same M-S6 report as
/// `RATIO_REPS`'s doc comment). Below the floor, the check falls back to an
/// absolute-ms difference instead of a ratio. This is not a loosened
/// budget: for any measurement at or above the floor, the 1.5x bound is
/// exactly as strict as before. `NOISE_FLOOR_MS * (max_ratio - 1.0)` is
/// simply the absolute slack the 1.5x ratio itself would already permit at
/// exactly the floor magnitude — expressing the same bound in a form that
/// isn't swamped by relative jitter on tiny absolute numbers.
const NOISE_FLOOR_MS: f64 = 10.0;

/// PR-9 ratio check — not scaled by `HANDOFF_PERF_SLACK`: wiki §6 states
/// PR-8/PR-9 are machine-independent (a ratio of two on-machine timings, not
/// an absolute latency), so shared CI runners are held to the same bound
/// (1.5x) as a quiet dev machine.
fn check_scale_ratio(ratio_name: &str, small: FixtureOpts, large: FixtureOpts, op: &str) {
    let small_ms = measure_single_scale(&small, op, RATIO_REPS);
    let large_ms = measure_single_scale(&large, op, RATIO_REPS);

    let budgets = load_budgets();
    let entry = budgets
        .ratio_budget
        .iter()
        .find(|r| r.name == ratio_name)
        .unwrap_or_else(|| panic!("no ratio_budget entry named {ratio_name} in perf_budgets.toml"));
    let max_ratio = entry.max_ratio;

    // Below the noise floor, compare via absolute ms difference instead of a
    // ratio (see `NOISE_FLOOR_MS`'s doc comment) — same effective bound,
    // just not expressed as a ratio of two numbers this close to
    // measurement noise.
    let below_noise_floor = small_ms < NOISE_FLOOR_MS && large_ms < NOISE_FLOOR_MS;
    let abs_slack_ms = NOISE_FLOOR_MS * (max_ratio - 1.0);
    let ratio = large_ms / small_ms;
    let (within, metric) = if below_noise_floor {
        (
            (large_ms - small_ms) <= abs_slack_ms,
            format!(
                "diff {:.1}ms (noise floor: both < {NOISE_FLOOR_MS}ms, slack {abs_slack_ms:.1}ms)",
                large_ms - small_ms
            ),
        )
    } else {
        (
            ratio <= max_ratio,
            format!("ratio {ratio:.2} (budget {max_ratio:.2})"),
        )
    };

    eprintln!(
        "\n=== perf_budget[{ratio_name}] op={op} small_p50={small_ms:.1}ms large_p50={large_ms:.1}ms {metric} ===\n"
    );

    match (within, &entry.expected_fail) {
        (true, None) => {}
        (true, Some(_reason)) => {
            eprintln!("{ratio_name}: PROMOTE? — now within budget, remove expected_fail");
        }
        (false, Some(reason)) => {
            eprintln!("{ratio_name}: expected-fail ({reason})");
        }
        (false, None) => panic!(
            "{ratio_name}: {metric} exceeded (op={op}, small={small_ms:.1}ms, large={large_ms:.1}ms)"
        ),
    }
}

fn measure_single_scale(opts: &FixtureOpts, op: &str, reps: usize) -> f64 {
    let tmp = tempfile::tempdir().expect("tempdir");
    let proj = tmp.path().join("proj");
    let meta = generate(&proj, opts).expect("generate fixture");
    let mut client = Client::spawn();
    let results = run_ops(&mut client, &proj, &meta, reps);
    client.close();
    results
        .get(op)
        .unwrap_or_else(|| panic!("op {op} was not measured"))
        .median_ms
}
