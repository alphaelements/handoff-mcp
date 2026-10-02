//! Execution records (`runs/`) — wiki/220-vmodel-integration-design.md §2.6
//! (FR-302, D2). M1 (t360.8): one MCP call (`handoff_trace_record`) or one
//! `req_test_sync` ingestion of a layer item's test result appends exactly
//! one `runs/<run_id>.json` file — created with `create_new` (never
//! overwritten, retried with a fresh random suffix on a name collision) —
//! and refreshes the derived `runs/_latest.json` cache (per-item latest
//! result, so a reader never has to rescan every run file to answer "what
//! was this item's most recent result").
//!
//! `runs/_latest.json` is a *derived* file (like
//! `docs/_requirements_summary.json`): never hand-edited, safe to delete
//! (rebuilt from `runs/*.json` on next access), and excluded from git via
//! `.handoff/.gitignore` ([`ensure_latest_cache_gitignored`]).

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One `results[]` entry, as recorded by `handoff_trace_record` or
/// `req_test_sync` (wiki/220 §2.6).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunResultEntry {
    /// The stable_id (or, for a not-yet-resolvable item, whatever string the
    /// caller supplied) this result is for.
    pub item: String,
    /// `pass` | `fail` | `blocked` | `not_run` | `skipped`.
    pub result: String,
    /// The linked SubItem's `body_hash` at record time, filled in
    /// automatically by the tool (never supplied by the caller) — `None`
    /// when `item` doesn't resolve to any SubItem, or the resolved SubItem
    /// has no `body_hash` of its own (non-layer items are not body-owned).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_hash: Option<String>,
    /// M2 (wiki/260-vmodel-m2-design.md §2.3/§4.11/E13, M2-04): the linked
    /// SubItem's `def_hash` at record time, filled in automatically by the
    /// tool alongside `body_hash` — `None` for a non-layer item, an unknown
    /// `item`, or an item never synced by an M2-02-or-later binary.
    /// `trace_suspect`'s (M2-05) `result` suspect check prefers this over
    /// `body_hash` when present (E13: "判定は def_hash（あれば）→ body_hash
    /// の順で比べる" — a priority/attribute-only edit no longer makes a
    /// passing verification item spuriously "re-verify required").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub def_hash: Option<String>,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub evidence: Vec<String>,
    /// M2 (wiki/260-vmodel-m2-design.md §2.3/§4.1, M2-05): the original
    /// run_id this entry's `result` was carried forward from, when this
    /// entry was written by `trace_suspect(action="clear", targets=[{result:
    /// item}])` rather than a fresh execution — the result-suspect clear
    /// path (D2: the authority for a result stays runs-only, so "clearing"
    /// a stale-definition pass means recording one new run entry that
    /// reuses the last result verbatim against the item's now-current
    /// hashes, not mutating the original run file). `None` for every
    /// ordinary recorded result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carried_from: Option<String>,
}

/// `executor` field of a [`RunRecord`] (wiki/220 §2.6).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunExecutor {
    /// `"ai"` | `"human"`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// One `runs/<run_id>.json` file's full contents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunRecord {
    pub run_id: String,
    /// RFC3339 with millisecond precision (`DateTime<Utc>::to_rfc3339_opts`
    /// with `SecondsFormat::Millis`).
    pub executed_at: String,
    pub executor: RunExecutor,
    /// `git rev-parse --short HEAD`, or `""` when unavailable/unsupplied
    /// (wiki/220 §2.6: "失敗時は空").
    #[serde(default)]
    pub commit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// M3 (wiki/270-vmodel-m3-design.md §2.6/§4.4, FR-304): the test run
    /// (`.handoff/trace/test_runs/<test_run_id>.json`) this batch's results
    /// were recorded against, when the caller (`handoff_trace_record`/
    /// `handoff_trace_ingest`) supplied one. `None` for every ordinary
    /// recording not associated with a test run — the field is omitted
    /// entirely from the serialized JSON in that case (same convention as
    /// `task_id`), so a pre-M3 `runs/<run_id>.json` file deserializes
    /// unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_run_id: Option<String>,
    pub results: Vec<RunResultEntry>,
}

/// Caller-supplied input for one `results[]` entry — `body_hash` is
/// deliberately absent (the tool always derives it, wiki/220 §2.6: "ツール
/// が自動で埋める").
#[derive(Debug, Clone)]
pub struct RunResultInput<'a> {
    pub item: &'a str,
    pub result: &'a str,
    pub note: Option<&'a str>,
    pub evidence: Vec<String>,
}

const VALID_RESULTS: &[&str] = &["pass", "fail", "blocked", "not_run", "skipped"];

pub fn is_valid_result(result: &str) -> bool {
    VALID_RESULTS.contains(&result)
}

/// Monotonic per-process counter mixed into the random filename suffix so
/// two records written in the same process within the same millisecond
/// never race on the same `SystemTime` reading alone.
static RUN_ID_SEQ: AtomicU64 = AtomicU64::new(0);

/// A pseudo-random 6-digit suffix for the run_id (wiki/220 §2.6:
/// `<...>-<6桁乱数>`) — collision-avoidance only, not a security boundary,
/// so mixing wall-clock nanoseconds, the process id, and a per-process
/// counter (no external `rand` dependency) is sufficient: a same-millisecond
/// collision within one process is already vanishingly unlikely, and
/// [`write_run_record`] retries with a freshly generated suffix on the rare
/// `AlreadyExists` anyway.
fn random_six_digits() -> u32 {
    let seq = RUN_ID_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id() as u64;
    let mixed =
        nanos ^ pid.wrapping_mul(0x2545_F491_4F6C_DD1D) ^ seq.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    (mixed % 1_000_000) as u32
}

fn format_run_id(now: DateTime<Utc>, rand_digits: u32) -> String {
    format!("{}-{rand_digits:06}", now.format("%Y%m%d-%H%M%S-%3f"))
}

/// Maximum number of `create_new` attempts (each with a freshly generated
/// random suffix) before giving up — a collision this many times running
/// would mean something is systematically wrong (e.g. a broken RNG mix),
/// not ordinary bad luck.
const CREATE_RUN_FILE_MAX_ATTEMPTS: u32 = 20;

/// Writes `record` to a brand-new `runs/<run_id>.json` file — `create_new`
/// (wiki/220 §2.6: never overwrites, retries with a fresh random suffix on
/// an `AlreadyExists` collision), one file per call, append-only in spirit
/// (the file is never modified again after this write). `record.run_id` is
/// overwritten with the filename actually allocated (the caller passes in a
/// zeroed/placeholder value — see [`record_run`], the only caller).
fn write_run_record(handoff: &Path, record: &mut RunRecord, now: DateTime<Utc>) -> Result<PathBuf> {
    let runs_dir = handoff.join("runs");
    std::fs::create_dir_all(&runs_dir)
        .with_context(|| format!("Failed to create dir: {}", runs_dir.display()))?;

    for _ in 0..CREATE_RUN_FILE_MAX_ATTEMPTS {
        let run_id = format_run_id(now, random_six_digits());
        let path = runs_dir.join(format!("{run_id}.json"));
        record.run_id = run_id.clone();
        let content =
            serde_json::to_string_pretty(record).context("Failed to serialize run record")?;

        // Publish atomically: the full content is written and synced to a
        // temp file first (its name does not end in `.json`, so
        // `list_run_files` never picks it up), then hard-linked to the final
        // name. `hard_link` fails with `AlreadyExists` exactly like
        // `create_new` does, but a concurrent `sync()` in another process can
        // never observe the final `<run_id>.json` empty or half-written (a
        // bare `create_new` + `write_all` exposes an empty file between the
        // two calls, which that reader then fails to parse), and a crash
        // mid-write leaves only an orphaned temp file, not a truncated run
        // file that would break every later `sync()`.
        let seq = RUN_ID_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp_path = runs_dir.join(format!(".{run_id}.json.tmp.{}.{seq}", std::process::id()));
        let write_tmp = || -> Result<()> {
            let mut file = std::fs::File::create(&tmp_path)
                .with_context(|| format!("Failed to create {}", tmp_path.display()))?;
            file.write_all(content.as_bytes())
                .with_context(|| format!("Failed to write {}", tmp_path.display()))?;
            file.sync_all()
                .with_context(|| format!("Failed to sync {}", tmp_path.display()))?;
            Ok(())
        };
        if let Err(e) = write_tmp() {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e);
        }
        let linked = std::fs::hard_link(&tmp_path, &path);
        let _ = std::fs::remove_file(&tmp_path);
        match linked {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(e).with_context(|| format!("Failed to create {}", path.display()))
            }
        }
    }
    anyhow::bail!(
        "Failed to allocate a unique run_id filename under {} after {CREATE_RUN_FILE_MAX_ATTEMPTS} attempts",
        runs_dir.display()
    )
}

/// One item's cached latest result, as stored in `runs/_latest.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LatestItemResult {
    pub result: String,
    pub executed_at: String,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_hash: Option<String>,
    /// M2 (wiki/260 §2.3/E13, M2-04) — mirrors [`RunResultEntry::def_hash`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub def_hash: Option<String>,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub evidence: Vec<String>,
    /// M2 (wiki/260 §2.3/§4.1, M2-05) — mirrors
    /// [`RunResultEntry::carried_from`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carried_from: Option<String>,
}

/// On-disk shape of `runs/_latest.json` (wiki/220 §2.6, wiki/240 §5-4): a
/// derived cache of each item's most recent result plus the bookkeeping
/// (`max_run_id`, `count`) that lets the next reconciliation pass ([`sync`])
/// decide, from a `readdir` alone (no per-file `stat`), whether it can
/// ingest just the new-named files or must fall back to a full rescan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct LatestCache {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_run_id: Option<String>,
    #[serde(default)]
    pub count: usize,
    #[serde(default)]
    pub items: HashMap<String, LatestItemResult>,
    /// M3 (wiki/270-vmodel-m3-design.md §2.6/§4.4, FR-304): per-`test_run_id`
    /// latest result, keyed the same way `items` is (per item, "greatest
    /// `(executed_at, run_id)` wins" — [`merge_run_into_latest`]), but scoped
    /// to only the run files recorded against that test run
    /// (`RunRecord.test_run_id`). Purely additive relative to M2's
    /// `_latest.json` shape (MR-01: "既存の `items` フィールドの構造は変更
    /// しない") — every pre-M3 consumer that only reads `items` (
    /// `load_latest_readonly`'s callers, `trace_suspect`'s result-suspect
    /// check, ...) is unaffected by this field's presence.
    #[serde(default)]
    pub by_test_run: HashMap<String, HashMap<String, LatestItemResult>>,
}

fn latest_cache_path(handoff: &Path) -> PathBuf {
    handoff.join("runs").join("_latest.json")
}

fn read_latest_cache(handoff: &Path) -> Option<LatestCache> {
    let content = std::fs::read_to_string(latest_cache_path(handoff)).ok()?;
    serde_json::from_str(&content).ok()
}

/// One `runs/*.json` file on disk, as seen by a `readdir` pass — carries no
/// stat info (P-M8/§5-4: "ファイルごとの stat はしない"), only what a
/// directory listing gives for free: the file's path and its `run_id`
/// (derived from the filename, stripping `.json`).
struct RunFileEntry {
    run_id: String,
    path: PathBuf,
}

/// Recursively lists every `runs/*.json` file (month subdirectories
/// included, wiki/220 §2.6: "月ごとのサブディレクトリ（`runs/YYYYMM/`）を
/// 許す"), excluding `_latest.json` — mirrors
/// `src/mcp/handlers/docs.rs`'s `stat_runs_input_recursive`, but this one
/// needs the actual filenames/paths (to read just the new-named ones),
/// not only a max/count summary.
fn list_run_files(dir: &Path) -> Result<Vec<RunFileEntry>> {
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    list_run_files_recursive(dir, &mut out)?;
    Ok(out)
}

fn list_run_files_recursive(dir: &Path, out: &mut Vec<RunFileEntry>) -> Result<()> {
    for entry in
        std::fs::read_dir(dir).with_context(|| format!("Failed to read dir: {}", dir.display()))?
    {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if file_type.is_dir() {
            list_run_files_recursive(&path, out)?;
        } else if file_type.is_file() && name != "_latest.json" {
            if let Some(run_id) = name.strip_suffix(".json") {
                out.push(RunFileEntry {
                    run_id: run_id.to_string(),
                    path,
                });
            }
        }
    }
    Ok(())
}

/// Merges one run file's `results[]` (in array order) into `items`, applying
/// wiki/220 §2.6's "最新結果" rule: per item, the result with the greatest
/// `(executed_at, run_id)` pair wins; a tie (only possible for two entries
/// within the *same* run file, which share an identical `executed_at` and
/// `run_id`) is won by whichever comes later in `results[]` ("同時刻は
/// results 配列内の後勝ち"). `>=` (not `>`) is what makes same-run repeats
/// resolve to "last one in the array" — every entry from one run compares
/// equal to its predecessor on `(executed_at, run_id)`, so each later one
/// unconditionally overwrites.
fn merge_result_into_map(
    items: &mut HashMap<String, LatestItemResult>,
    run: &RunRecord,
    r: &RunResultEntry,
) {
    let candidate_key = (run.executed_at.as_str(), run.run_id.as_str());
    let should_replace = match items.get(&r.item) {
        None => true,
        Some(existing) => {
            candidate_key >= (existing.executed_at.as_str(), existing.run_id.as_str())
        }
    };
    if should_replace {
        items.insert(
            r.item.clone(),
            LatestItemResult {
                result: r.result.clone(),
                executed_at: run.executed_at.clone(),
                run_id: run.run_id.clone(),
                body_hash: r.body_hash.clone(),
                def_hash: r.def_hash.clone(),
                note: r.note.clone(),
                evidence: r.evidence.clone(),
                carried_from: r.carried_from.clone(),
            },
        );
    }
}

/// Merges one run file's `results[]` into both the always-updated top-level
/// `items` map and, when `run.test_run_id` is `Some` (M3, wiki/270 §2.6/§4.4,
/// FR-304), the same run's scoped entry under `by_test_run` — same
/// "greatest `(executed_at, run_id)` wins per item" rule
/// ([`merge_result_into_map`]) applied independently to each map, so an item
/// recorded both inside and outside a test run still resolves correctly in
/// each of its own scopes.
fn merge_run_into_latest(
    items: &mut HashMap<String, LatestItemResult>,
    by_test_run: &mut HashMap<String, HashMap<String, LatestItemResult>>,
    run: &RunRecord,
) {
    for r in &run.results {
        merge_result_into_map(items, run, r);
        if let Some(test_run_id) = &run.test_run_id {
            let scoped = by_test_run.entry(test_run_id.clone()).or_default();
            merge_result_into_map(scoped, run, r);
        }
    }
}

/// One entry of an item's execution history (`handoff_trace_history` /
/// CLI `trace history`, wiki/220 §3.4), newest first.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RunHistoryEntry {
    pub run_id: String,
    pub executed_at: String,
    pub executor: RunExecutor,
    pub result: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub commit: String,
}

/// Every recorded result for `item` across `runs/` (month subdirectories
/// included, `_latest.json` excluded — same file set `sync` scans), newest
/// first: ordered by `(executed_at, run_id)` descending, the same
/// "greatest wins" key `merge_run_into_latest` uses for `runs/_latest.json`.
/// Unlike that cache (one entry per item, incrementally maintained), this is
/// a full scan of every run file — `trace history` is an on-demand lookup
/// (wiki/220 §3.4, VSCode FR-903's execution-history display), not a
/// per-write hot path, so there is no incremental per-item index to
/// maintain for it.
pub fn history_for_item(handoff: &Path, item: &str) -> Result<Vec<RunHistoryEntry>> {
    let runs_dir = handoff.join("runs");
    let files = list_run_files(&runs_dir)?;
    let mut out = Vec::new();
    for f in &files {
        let run = read_run_record(&f.path)?;
        for r in &run.results {
            if r.item == item {
                out.push(RunHistoryEntry {
                    run_id: run.run_id.clone(),
                    executed_at: run.executed_at.clone(),
                    executor: run.executor.clone(),
                    result: r.result.clone(),
                    note: r.note.clone(),
                    evidence: r.evidence.clone(),
                    commit: run.commit.clone(),
                });
            }
        }
    }
    out.sort_by(|a, b| {
        (b.executed_at.as_str(), b.run_id.as_str())
            .cmp(&(a.executed_at.as_str(), a.run_id.as_str()))
    });
    Ok(out)
}

fn read_run_record(path: &Path) -> Result<RunRecord> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    serde_json::from_str(&content).with_context(|| format!("Failed to parse {}", path.display()))
}

/// Idempotently appends `entry` to `.handoff/.gitignore` (creating the file
/// if absent) unless it already contains that exact line — the user's own
/// `.handoff/` is expected to be its own independent git repo (see project
/// memory), so a derived cache like `runs/_latest.json` needs its own
/// gitignore entry there, same as any other build artifact.
pub(crate) fn ensure_gitignore_entry(handoff: &Path, entry: &str) -> Result<()> {
    let path = handoff.join(".gitignore");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == entry) {
        return Ok(());
    }
    let mut new_content = existing;
    if !new_content.is_empty() && !new_content.ends_with('\n') {
        new_content.push('\n');
    }
    new_content.push_str(entry);
    new_content.push('\n');
    crate::storage::atomic_write(&path, new_content.as_bytes())
        .with_context(|| format!("Failed to write {}", path.display()))
}

/// Pure reconciliation core shared by [`sync`] (which persists the result)
/// and [`load_latest_readonly`] (wiki/260-vmodel-m2-design.md §2.1/E6, M2-08
/// — which never does): given the current `runs/` directory listing (`files`)
/// and whatever cache was last persisted (`cached`, `None` if missing/absent),
/// computes the reconciled [`LatestCache`] by reading only the run files that
/// genuinely need reading — same "new-named files only, else full rebuild"
/// rule either caller applies. No I/O beyond reading the individual run
/// files `files` already names (the directory listing itself is the caller's
/// job, since `sync` also needs `runs_dir` for its own `create_dir_all`).
fn reconcile_latest_cache(
    files: &[RunFileEntry],
    cached: Option<&LatestCache>,
) -> Result<LatestCache> {
    let current_count = files.len();
    let current_max_run_id = files.iter().map(|f| f.run_id.as_str()).max();

    let (mut items, mut by_test_run, reconciled) = match cached {
        Some(cache) => {
            let new_files: Vec<&RunFileEntry> = files
                .iter()
                .filter(|f| Some(f.run_id.as_str()) > cache.max_run_id.as_deref())
                .collect();
            if cache.count + new_files.len() == current_count {
                let mut items = cache.items.clone();
                let mut by_test_run = cache.by_test_run.clone();
                for f in &new_files {
                    let run = read_run_record(&f.path)?;
                    merge_run_into_latest(&mut items, &mut by_test_run, &run);
                }
                (items, by_test_run, true)
            } else {
                (HashMap::new(), HashMap::new(), false)
            }
        }
        None => (HashMap::new(), HashMap::new(), false),
    };

    if !reconciled {
        // Full rebuild: cache missing or its bookkeeping no longer
        // reconciles with the current directory listing.
        items = HashMap::new();
        by_test_run = HashMap::new();
        for f in files {
            let run = read_run_record(&f.path)?;
            merge_run_into_latest(&mut items, &mut by_test_run, &run);
        }
    }

    Ok(LatestCache {
        max_run_id: current_max_run_id.map(str::to_string),
        count: current_count,
        items,
        by_test_run,
    })
}

/// Reconciles `runs/_latest.json` against the current `runs/` filesystem
/// state (wiki/220 §2.6, wiki/240 §5-4): a single `readdir` (recursive over
/// any month subdirectories) is compared against the persisted cache's
/// `(max_run_id, count)` — no per-file `stat`. When every file beyond
/// `max_run_id` accounts for the entire gap between the cache's `count` and
/// the current total, only those new-named files are read and merged in
/// (the common case: this process, or another one sharing `.handoff/`, just
/// wrote one more run). Otherwise (cache missing, or the counts don't
/// reconcile — e.g. an older-named file appeared via `git pull`, or a file
/// was deleted) the whole corpus is read and the cache rebuilt from
/// scratch. Writes `_latest.json` only when its content actually changed
/// (P-M4 discipline, `record_derived_write_for_test` instrumentation), and
/// ensures the file is gitignored the first time it's written.
pub fn sync(handoff: &Path) -> Result<LatestCache> {
    let runs_dir = handoff.join("runs");
    let files = list_run_files(&runs_dir)?;
    let cached = read_latest_cache(handoff);
    let new_cache = reconcile_latest_cache(&files, cached.as_ref())?;

    if cached.as_ref() != Some(&new_cache) {
        let path = latest_cache_path(handoff);
        // `record_run`'s own write path always `create_dir_all`s `runs/`
        // before writing a run file, but `sync()` is also called standalone
        // now (`handoff_trace_report`/`handoff_trace_slice`, t360.10/t360.11)
        // on a project that may never have recorded a run yet — `runs/`
        // itself might not exist. `atomic_write`'s temp file creation fails
        // outright if its parent directory is missing, so ensure it here
        // rather than assuming a prior `record_run` call already did.
        std::fs::create_dir_all(&runs_dir)
            .with_context(|| format!("Failed to create dir: {}", runs_dir.display()))?;
        let content =
            serde_json::to_string(&new_cache).context("Failed to serialize runs/_latest.json")?;
        crate::storage::atomic_write(&path, content.as_bytes())
            .with_context(|| format!("Failed to write {}", path.display()))?;
        crate::mcp::handlers::docs::record_derived_write_for_test(&path, content.len());
        ensure_gitignore_entry(handoff, "/runs/_latest.json")?;
    }

    Ok(new_cache)
}

/// E6's read-only counterpart to [`sync`] (wiki/260-vmodel-m2-design.md §2.1's
/// E6 table, item 2; M2-08): reads `runs/_latest.json` and merges in whatever
/// run files the directory listing shows beyond it (the exact same
/// [`reconcile_latest_cache`] core `sync` uses), but **never writes anything**
/// — not `runs/_latest.json` itself, not `.handoff/.gitignore`. A missing or
/// corrupt `_latest.json` is treated as `cached: None` (the same "full
/// rebuild from every run file" path `sync` takes for a missing cache), so
/// this never fails outright just because the cache has not been
/// materialized yet; it simply costs a full `runs/` scan that first time,
/// exactly like `sync` would, just without persisting the result.
///
/// Used by every read-only trace entry point (`trace_readonly`'s fully-E6
/// loader, `handoff_trace_impact`) so a `handoff_trace_lint`/
/// `handoff_trace_suspect(list|baseline dry_run)`/`handoff_trace_impact` call
/// can never be the first call to materialize `runs/_latest.json` — that
/// write is reserved for a write-classified tool (`handoff_trace_record`,
/// `handoff_trace_report`/`handoff_trace_slice`'s own resync sequence, ...).
pub fn load_latest_readonly(handoff: &Path) -> Result<LatestCache> {
    let runs_dir = handoff.join("runs");
    let files = list_run_files(&runs_dir)?;
    let cached = read_latest_cache(handoff);
    reconcile_latest_cache(&files, cached.as_ref())
}

/// `body_hash`/`def_hash` pair resolved for one `item` by [`find_item_hashes`].
struct ItemHashes {
    body_hash: Option<String>,
    def_hash: Option<String>,
}

/// Resolves `stable_id`'s current `{body_hash, def_hash}` by scanning `docs`
/// (already loaded once by the caller — no `read_all_docs` call of its own)
/// for a `SubItem` whose `stable_id` matches. `None` (outer) means no
/// SubItem anywhere has this stable_id at all (wiki/220 §2.6: "未知の
/// stable_id を含む記録は保存し warning を返す" — the caller uses this to
/// decide whether to emit that warning); `Some(ItemHashes { .. })`'s own
/// fields are independently `None` when the resolved item carries no
/// `body_hash`/`def_hash` of its own (non-layer items are not body-owned;
/// `def_hash` (M2-04, wiki/260 §2.3) is `None` for an item never synced by an
/// M2-02-or-later binary).
fn find_item_hashes(
    docs: &[crate::storage::docs::DocMetadata],
    stable_id: &str,
) -> Option<ItemHashes> {
    for doc in docs {
        let Some(v) = doc.verification.as_ref() else {
            continue;
        };
        for item in &v.items {
            for sub in &item.sub_items {
                if sub.stable_id.as_deref() == Some(stable_id) {
                    return Some(ItemHashes {
                        body_hash: sub.body_hash.clone(),
                        def_hash: sub.def_hash.clone(),
                    });
                }
            }
        }
    }
    None
}

/// Records one execution batch (`handoff_trace_record`, or one
/// `req_test_sync` ingestion of layer-item test results) as a single
/// `runs/<run_id>.json` file, refreshes `runs/_latest.json`, and returns the
/// allocated `run_id` plus any non-fatal warnings (unknown stable_ids,
/// invalid `result` values already filtered out by the caller).
///
/// `docs` is the already-loaded document corpus (no `read_all_docs` call of
/// its own — P-M1/P-M3 discipline: callers that already loaded it for their
/// own purposes, e.g. `req_test_sync`, must not pay for a second read).
///
/// `test_run_id` (M3, wiki/270-vmodel-m3-design.md §2.6/§4.4, FR-304): when
/// `Some`, this batch is also attributed to that test run — recorded on
/// [`RunRecord::test_run_id`] and folded into `runs/_latest.json`'s
/// `by_test_run` map ([`merge_run_into_latest`]) alongside the always-updated
/// top-level `items` map (§2.6: "拡張は加算的変更...既存の `items`
/// フィールドの構造は変更しない").
#[allow(clippy::too_many_arguments)]
pub fn record_run(
    handoff: &Path,
    docs: &[crate::storage::docs::DocMetadata],
    results: &[RunResultInput],
    executor_kind: &str,
    executor_id: Option<&str>,
    commit: Option<String>,
    task_id: Option<String>,
    test_run_id: Option<String>,
) -> Result<(String, Vec<String>)> {
    let now = Utc::now();
    let mut warnings = Vec::new();

    let result_entries: Vec<RunResultEntry> = results
        .iter()
        .map(|r| {
            let (body_hash, def_hash) = match find_item_hashes(docs, r.item) {
                Some(hashes) => (hashes.body_hash, hashes.def_hash),
                None => {
                    warnings.push(format!(
                        "{}: unknown item (no SubItem with this stable_id) — recorded anyway",
                        r.item
                    ));
                    (None, None)
                }
            };
            RunResultEntry {
                item: r.item.to_string(),
                result: r.result.to_string(),
                body_hash,
                def_hash,
                note: r.note.unwrap_or("").to_string(),
                evidence: r.evidence.clone(),
                carried_from: None,
            }
        })
        .collect();

    let mut record = RunRecord {
        run_id: String::new(), // filled in by write_run_record with the allocated filename
        executed_at: now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        executor: RunExecutor {
            kind: executor_kind.to_string(),
            id: executor_id.map(str::to_string),
        },
        commit: commit.unwrap_or_default(),
        task_id,
        test_run_id,
        results: result_entries,
    };

    write_run_record(handoff, &mut record, now)?;
    // The run file above is the durable record; `_latest.json` is only a
    // derived cache (rebuilt from `runs/*.json` on the next successful
    // sync). A cache-refresh failure (e.g. an unparseable foreign `.json`
    // under `runs/`) must not turn an already-persisted record into an
    // error response — a caller retrying on that error would record the
    // same batch twice. Surface it as a warning instead.
    if let Err(e) = sync(handoff) {
        warnings.push(format!(
            "run {} recorded, but refreshing runs/_latest.json failed: {e:#}",
            record.run_id
        ));
    }

    Ok((record.run_id, warnings))
}

/// `trace_suspect(action="clear", targets=[{result: item}])`
/// (wiki/260-vmodel-m2-design.md §4.1, M2-05): records one new
/// `runs/<run_id>.json` entry that reuses `item`'s last recorded `result`
/// value verbatim, against `item`'s *current* `{def_hash, body_hash}` (the
/// new baseline the clear moves the `result` suspect to) — `carried_from`
/// records the original run_id whose result this carries forward. D2's
/// "結果の正本は runs だけ" is preserved: nothing about the original run file
/// is ever modified, this only appends a new one.
pub fn record_carried_result(
    handoff: &Path,
    docs: &[crate::storage::docs::DocMetadata],
    item: &str,
    result: &str,
    carried_from: &str,
    executor_kind: &str,
    executor_id: Option<&str>,
) -> Result<(String, Vec<String>)> {
    let now = Utc::now();
    let mut warnings = Vec::new();

    let (body_hash, def_hash) = match find_item_hashes(docs, item) {
        Some(hashes) => (hashes.body_hash, hashes.def_hash),
        None => {
            warnings.push(format!(
                "{item}: unknown item (no SubItem with this stable_id) — recorded anyway"
            ));
            (None, None)
        }
    };

    let mut record = RunRecord {
        run_id: String::new(), // filled in by write_run_record with the allocated filename
        executed_at: now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        executor: RunExecutor {
            kind: executor_kind.to_string(),
            id: executor_id.map(str::to_string),
        },
        commit: String::new(),
        task_id: None,
        // `trace_suspect(action="clear", targets=[{result: item}])`'s carried-
        // forward entry is never recorded against a test run — this call site
        // has no `test_run_id` argument of its own (M3 §2.6 only wires it
        // into `trace_record`/`trace_ingest`'s fresh-execution paths).
        test_run_id: None,
        results: vec![RunResultEntry {
            item: item.to_string(),
            result: result.to_string(),
            body_hash,
            def_hash,
            note: String::new(),
            evidence: Vec::new(),
            carried_from: Some(carried_from.to_string()),
        }],
    };

    write_run_record(handoff, &mut record, now)?;
    if let Err(e) = sync(handoff) {
        warnings.push(format!(
            "run {} recorded, but refreshing runs/_latest.json failed: {e:#}",
            record.run_id
        ));
    }

    Ok((record.run_id, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::docs::{write_doc, DocMetadata, SubItem, Verification, VerificationItem};

    fn setup(tmp: &std::path::Path) -> PathBuf {
        let handoff = tmp.join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();
        handoff
    }

    fn doc_with_sub_item(handoff: &Path, doc_id: &str, stable_id: &str, body_hash: Option<&str>) {
        doc_with_sub_item_hashes(handoff, doc_id, stable_id, body_hash, None);
    }

    /// M2-04: like [`doc_with_sub_item`], but also sets `def_hash`.
    fn doc_with_sub_item_hashes(
        handoff: &Path,
        doc_id: &str,
        stable_id: &str,
        body_hash: Option<&str>,
        def_hash: Option<&str>,
    ) {
        let now = Utc::now().to_rfc3339();
        let mut doc = DocMetadata::new(
            doc_id.to_string(),
            doc_id.to_string(),
            "Doc".to_string(),
            "spec".to_string(),
            now.clone(),
        );
        doc.verification = Some(Verification {
            status: "pending".to_string(),
            created_at: now.clone(),
            updated_at: now,
            items: vec![VerificationItem {
                fragment_seq: Some(1),
                heading: "Section 1".to_string(),
                status: "pending".to_string(),
                impl_refs: Vec::new(),
                test_refs: Vec::new(),
                reviewer: None,
                verified_at: None,
                notes: String::new(),
                content_hash_at_verify: None,
                category: "section".to_string(),
                sub_items: vec![SubItem {
                    index: 0,
                    description: "item".to_string(),
                    stable_id: Some(stable_id.to_string()),
                    body_hash: body_hash.map(String::from),
                    def_hash: def_hash.map(String::from),
                    ..Default::default()
                }],
                label: None,
            }],
        });
        write_doc(handoff, &doc).unwrap();
    }

    /// M3 (wiki/270-vmodel-m3-design.md §2.6, FR-304): `RunRecord.test_run_id`
    /// serializes only when `Some` (mirrors `task_id`'s own convention) — a
    /// pre-M3 run file with no `test_run_id` key at all must still deserialize
    /// (via `#[serde(default)]`).
    #[test]
    fn run_record_test_run_id_round_trips_and_is_omitted_when_none() {
        let with_id = RunRecord {
            run_id: "r1".to_string(),
            executed_at: "2026-01-01T00:00:00.000Z".to_string(),
            executor: RunExecutor {
                kind: "ai".to_string(),
                id: None,
            },
            commit: String::new(),
            task_id: None,
            test_run_id: Some("20261007-140000-111-222333".to_string()),
            results: vec![],
        };
        let json = serde_json::to_string(&with_id).unwrap();
        assert!(json.contains("\"test_run_id\":\"20261007-140000-111-222333\""));
        let parsed: RunRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed.test_run_id.as_deref(),
            Some("20261007-140000-111-222333")
        );

        let without_id = RunRecord {
            test_run_id: None,
            ..with_id
        };
        let json_no_id = serde_json::to_string(&without_id).unwrap();
        assert!(
            !json_no_id.contains("test_run_id"),
            "must be omitted when None: {json_no_id}"
        );

        // A pre-M3 run file with no `test_run_id` key at all must still parse.
        let legacy = r#"{"run_id":"r2","executed_at":"2026-01-01T00:00:00.000Z","executor":{"kind":"ai"},"commit":"","results":[]}"#;
        let parsed_legacy: RunRecord = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed_legacy.test_run_id, None);
    }

    #[test]
    fn is_valid_result_accepts_only_the_five_spec_values() {
        for v in ["pass", "fail", "blocked", "not_run", "skipped"] {
            assert!(is_valid_result(v), "{v} should be valid");
        }
        for v in ["passed", "PASS", "", "ok"] {
            assert!(!is_valid_result(v), "{v} should be invalid");
        }
    }

    #[test]
    fn record_run_creates_a_run_file_with_run_id_matching_the_spec_format() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup(tmp.path());
        doc_with_sub_item(&handoff, "doc-a", "ST-001", Some("abc123"));

        let inputs = vec![RunResultInput {
            item: "ST-001",
            result: "pass",
            note: Some("looks good"),
            evidence: vec!["tests/e2e.rs::case".to_string()],
        }];
        let (run_id, warnings) = record_run(
            &handoff,
            &read_docs_for_test(&handoff),
            &inputs,
            "ai",
            Some("agent-1"),
            Some("abc1234".to_string()),
            Some("t1".to_string()),
            None,
        )
        .unwrap();

        assert!(
            warnings.is_empty(),
            "known stable_id must not warn: {warnings:?}"
        );
        // <YYYYMMDD-HHMMSS-mmm>-<6-digit-rand>
        let re_parts: Vec<&str> = run_id.split('-').collect();
        assert_eq!(
            re_parts.len(),
            4,
            "run_id must have 4 dash-separated groups: {run_id}"
        );
        assert_eq!(re_parts[0].len(), 8, "date group: {run_id}");
        assert_eq!(re_parts[1].len(), 6, "time group: {run_id}");
        assert_eq!(re_parts[2].len(), 3, "millis group: {run_id}");
        assert_eq!(re_parts[3].len(), 6, "random group: {run_id}");

        let path = handoff.join("runs").join(format!("{run_id}.json"));
        let record: RunRecord =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(record.run_id, run_id);
        assert_eq!(record.executor.kind, "ai");
        assert_eq!(record.executor.id.as_deref(), Some("agent-1"));
        assert_eq!(record.commit, "abc1234");
        assert_eq!(record.task_id.as_deref(), Some("t1"));
        assert_eq!(record.results.len(), 1);
        assert_eq!(record.results[0].item, "ST-001");
        assert_eq!(record.results[0].result, "pass");
        assert_eq!(record.results[0].body_hash.as_deref(), Some("abc123"));
        assert_eq!(record.results[0].note, "looks good");
        assert_eq!(
            record.results[0].evidence,
            vec!["tests/e2e.rs::case".to_string()]
        );
    }

    /// M2-04 (wiki/260-vmodel-m2-design.md §2.3/E13): `record_run` fills in
    /// `def_hash` alongside `body_hash`, from the same resolved SubItem.
    #[test]
    fn record_run_fills_in_def_hash_alongside_body_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup(tmp.path());
        doc_with_sub_item_hashes(&handoff, "doc-a", "ST-001", Some("abc123"), Some("d3f456"));

        let inputs = vec![RunResultInput {
            item: "ST-001",
            result: "pass",
            note: None,
            evidence: vec![],
        }];
        let (run_id, warnings) = record_run(
            &handoff,
            &read_docs_for_test(&handoff),
            &inputs,
            "ai",
            None,
            Some(String::new()),
            None,
            None,
        )
        .unwrap();
        assert!(warnings.is_empty());

        let path = handoff.join("runs").join(format!("{run_id}.json"));
        let record: RunRecord =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(record.results[0].body_hash.as_deref(), Some("abc123"));
        assert_eq!(record.results[0].def_hash.as_deref(), Some("d3f456"));

        let latest = read_latest_cache(&handoff).unwrap();
        let latest_item = latest.items.get("ST-001").unwrap();
        assert_eq!(latest_item.def_hash.as_deref(), Some("d3f456"));
    }

    #[test]
    fn record_run_unknown_stable_id_is_saved_with_a_warning_and_no_body_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup(tmp.path());
        // No documents at all — every item is unresolvable.

        let inputs = vec![RunResultInput {
            item: "GHOST-1",
            result: "fail",
            note: None,
            evidence: vec![],
        }];
        let (run_id, warnings) = record_run(
            &handoff,
            &[],
            &inputs,
            "ai",
            None,
            Some(String::new()),
            None,
            None,
        )
        .unwrap();

        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("GHOST-1"));

        let path = handoff.join("runs").join(format!("{run_id}.json"));
        let record: RunRecord =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(record.results[0].body_hash, None);
    }

    #[test]
    fn record_run_is_gated_by_create_new_never_overwrites() {
        // Two records written back to back must land in two distinct files —
        // proves the random-suffix + retry path never collides in practice,
        // and that write_run_record never silently overwrites an existing
        // run (`create_new`, not `create`/`write`).
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup(tmp.path());
        let inputs = vec![RunResultInput {
            item: "ST-001",
            result: "pass",
            note: None,
            evidence: vec![],
        }];
        let (run_id_a, _) = record_run(
            &handoff,
            &[],
            &inputs,
            "ai",
            None,
            Some(String::new()),
            None,
            None,
        )
        .unwrap();
        let (run_id_b, _) = record_run(
            &handoff,
            &[],
            &inputs,
            "ai",
            None,
            Some(String::new()),
            None,
            None,
        )
        .unwrap();

        assert_ne!(run_id_a, run_id_b);
        assert!(handoff
            .join("runs")
            .join(format!("{run_id_a}.json"))
            .exists());
        assert!(handoff
            .join("runs")
            .join(format!("{run_id_b}.json"))
            .exists());
    }

    /// M1 t360.10/t360.11: `handoff_trace_report`/`handoff_trace_slice` call
    /// `sync()` directly (they never go through `record_run`, which is the
    /// only other caller and always `create_dir_all`s `runs/` first via its
    /// own write path) on a brand-new project where `.handoff/runs/` has
    /// never been created. `sync()` must not assume that directory already
    /// exists just because it's about to write `_latest.json` into it.
    #[test]
    fn sync_creates_the_runs_directory_when_it_does_not_exist_yet() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup(tmp.path());
        assert!(!handoff.join("runs").exists());

        let cache = sync(&handoff).expect("sync must not fail when runs/ is missing");
        assert_eq!(cache.count, 0);
        assert!(handoff.join("runs").join("_latest.json").exists());
    }

    #[test]
    fn sync_full_rebuild_when_no_cache_exists_computes_latest_per_item() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup(tmp.path());
        let inputs_a = vec![RunResultInput {
            item: "ST-001",
            result: "fail",
            note: None,
            evidence: vec![],
        }];
        record_run(
            &handoff,
            &[],
            &inputs_a,
            "ai",
            None,
            Some(String::new()),
            None,
            None,
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let inputs_b = vec![RunResultInput {
            item: "ST-001",
            result: "pass",
            note: None,
            evidence: vec![],
        }];
        record_run(
            &handoff,
            &[],
            &inputs_b,
            "ai",
            None,
            Some(String::new()),
            None,
            None,
        )
        .unwrap();

        // Delete _latest.json to force sync() to rebuild from scratch.
        std::fs::remove_file(handoff.join("runs").join("_latest.json")).unwrap();
        let cache = sync(&handoff).unwrap();

        assert_eq!(cache.count, 2);
        assert_eq!(cache.items.get("ST-001").unwrap().result, "pass");
    }

    /// wiki/220 §2.6/wiki/240 §5-4: "月ごとのサブディレクトリ
    /// （`runs/YYYYMM/`）を許す" — a run file placed under a month
    /// subdirectory (rather than directly in `runs/`) must still be
    /// discovered by [`list_run_files`]/[`sync`], exactly like a top-level
    /// file.
    #[test]
    fn sync_discovers_run_files_under_month_subdirectories() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup(tmp.path());
        let month_dir = handoff.join("runs").join("202609");
        std::fs::create_dir_all(&month_dir).unwrap();
        let record = RunRecord {
            run_id: "20260915-120000-000-000001".to_string(),
            executed_at: "2026-09-15T12:00:00.000Z".to_string(),
            executor: RunExecutor {
                kind: "ai".to_string(),
                id: None,
            },
            commit: String::new(),
            task_id: None,
            test_run_id: None,
            results: vec![RunResultEntry {
                item: "ST-050".to_string(),
                result: "pass".to_string(),
                body_hash: None,
                def_hash: None,
                note: String::new(),
                carried_from: None,
                evidence: vec![],
            }],
        };
        std::fs::write(
            month_dir.join("20260915-120000-000-000001.json"),
            serde_json::to_string_pretty(&record).unwrap(),
        )
        .unwrap();

        let cache = sync(&handoff).unwrap();
        assert_eq!(
            cache.count, 1,
            "the month-subdirectory file must be counted"
        );
        assert_eq!(cache.items.get("ST-050").unwrap().result, "pass");
    }

    /// N6 (t360.43 M1 review): `list_run_files_recursive` (and therefore
    /// `sync`) must never pick up an `atomic_write`/`write_run_record`
    /// in-flight temp file — `.{file_name}.tmp.{pid}.{seq}`, staged in the
    /// same directory before the final rename/hard-link. Already true today
    /// via the stricter `name.strip_suffix(".json")` filter (the temp name's
    /// suffix is never `.json`), but pinned by a dedicated test rather than
    /// left as an implicit consequence of an unrelated filter — see
    /// `crate::mcp::handlers::docs::stat_runs_input_recursive`'s sibling fix
    /// (this same task) for the counterpart that *did* need an explicit
    /// dot-prefix exclusion.
    #[test]
    fn sync_never_counts_a_dot_prefixed_in_flight_temp_file_under_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup(tmp.path());
        std::fs::create_dir_all(handoff.join("runs")).unwrap();
        std::fs::write(handoff.join("runs/20260915-120000-000-000001.json"), "{}").unwrap();
        std::fs::write(
            handoff.join("runs/.20260915-120000-000-000002.json.tmp.999.1"),
            "{\"incomplete",
        )
        .unwrap();

        let files = list_run_files(&handoff.join("runs")).unwrap();
        assert_eq!(
            files.len(),
            1,
            "the dot-prefixed temp file must not be counted as a run file"
        );
    }

    /// M3 (wiki/270-vmodel-m3-design.md §2.6/§4.4, FR-304, MR-01): a run
    /// recorded with `test_run_id` must be folded into `runs/_latest.json`'s
    /// `by_test_run[test_run_id]` map *in addition to* the always-updated
    /// top-level `items` map — an ordinary run (no `test_run_id`) must leave
    /// `by_test_run` untouched.
    #[test]
    fn sync_folds_a_test_run_scoped_result_into_by_test_run_additively() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup(tmp.path());
        let inputs = vec![RunResultInput {
            item: "ST-001",
            result: "fail",
            note: None,
            evidence: vec![],
        }];
        // Ordinary run, no test_run_id.
        record_run(
            &handoff,
            &[],
            &inputs,
            "ai",
            None,
            Some(String::new()),
            None,
            None,
        )
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(5));
        let inputs_scoped = vec![RunResultInput {
            item: "ST-002",
            result: "pass",
            note: None,
            evidence: vec![],
        }];
        record_run(
            &handoff,
            &[],
            &inputs_scoped,
            "ai",
            None,
            Some(String::new()),
            None,
            Some("tr-1".to_string()),
        )
        .unwrap();

        let cache = read_latest_cache(&handoff).unwrap();
        assert_eq!(
            cache.items.get("ST-001").unwrap().result,
            "fail",
            "top-level items must still carry the ordinary (non-test-run) result"
        );
        assert_eq!(
            cache.items.get("ST-002").unwrap().result,
            "pass",
            "top-level items must also carry the test-run-scoped result (additive)"
        );
        let scoped = cache.by_test_run.get("tr-1").expect("tr-1 scope present");
        assert_eq!(
            scoped.len(),
            1,
            "only the test-run-scoped item must appear here"
        );
        assert_eq!(scoped.get("ST-002").unwrap().result, "pass");
        assert!(
            !cache.by_test_run.contains_key(""),
            "an ordinary run must never create a by_test_run entry"
        );
    }

    #[test]
    fn sync_incrementally_ingests_only_new_named_files_when_count_reconciles() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup(tmp.path());
        let inputs = vec![RunResultInput {
            item: "ST-001",
            result: "fail",
            note: None,
            evidence: vec![],
        }];
        let (run_id_a, _) = record_run(
            &handoff,
            &[],
            &inputs,
            "ai",
            None,
            Some(String::new()),
            None,
            None,
        )
        .unwrap();
        let cache_after_first = read_latest_cache(&handoff).unwrap();
        assert_eq!(
            cache_after_first.max_run_id.as_deref(),
            Some(run_id_a.as_str())
        );
        assert_eq!(cache_after_first.count, 1);

        std::thread::sleep(std::time::Duration::from_millis(5));
        let inputs_b = vec![RunResultInput {
            item: "ST-002",
            result: "pass",
            note: None,
            evidence: vec![],
        }];
        // record_run's own internal sync() call already exercises the
        // incremental path (this is exactly what production does) — assert
        // its result directly rather than calling sync() a third time.
        let (run_id_b, _) = record_run(
            &handoff,
            &[],
            &inputs_b,
            "ai",
            None,
            Some(String::new()),
            None,
            None,
        )
        .unwrap();

        let cache = read_latest_cache(&handoff).unwrap();
        assert_eq!(cache.count, 2);
        assert_eq!(cache.max_run_id.as_deref(), Some(run_id_b.as_str()));
        assert_eq!(cache.items.get("ST-001").unwrap().result, "fail");
        assert_eq!(cache.items.get("ST-002").unwrap().result, "pass");
    }

    #[test]
    fn sync_falls_back_to_full_rebuild_when_count_does_not_reconcile() {
        // Simulates "an older-named file appeared via git pull": a file
        // whose run_id sorts *before* the cache's recorded max_run_id is
        // added directly (bypassing record_run, so the cache is never told
        // about it via the normal one-more-file path) — count no longer
        // reconciles with cache.count + (new-named files), so the next
        // sync() must fall back to a full rescan rather than missing it.
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup(tmp.path());
        let inputs = vec![RunResultInput {
            item: "ST-002",
            result: "pass",
            note: None,
            evidence: vec![],
        }];
        record_run(
            &handoff,
            &[],
            &inputs,
            "ai",
            None,
            Some(String::new()),
            None,
            None,
        )
        .unwrap();
        let cache_before = read_latest_cache(&handoff).unwrap();
        assert_eq!(cache_before.count, 1);

        // Write an old-named run file directly (lexicographically smaller
        // run_id than anything record_run would generate "now").
        let old_record = RunRecord {
            run_id: "20000101-000000-000-000001".to_string(),
            executed_at: "2000-01-01T00:00:00.000Z".to_string(),
            executor: RunExecutor {
                kind: "ai".to_string(),
                id: None,
            },
            commit: String::new(),
            task_id: None,
            test_run_id: None,
            results: vec![RunResultEntry {
                item: "ST-003".to_string(),
                result: "blocked".to_string(),
                body_hash: None,
                def_hash: None,
                note: String::new(),
                carried_from: None,
                evidence: vec![],
            }],
        };
        std::fs::write(
            handoff.join("runs").join("20000101-000000-000-000001.json"),
            serde_json::to_string_pretty(&old_record).unwrap(),
        )
        .unwrap();

        let cache = sync(&handoff).unwrap();
        assert_eq!(cache.count, 2, "full rebuild must now see both files");
        assert_eq!(cache.items.get("ST-002").unwrap().result, "pass");
        assert_eq!(
            cache.items.get("ST-003").unwrap().result,
            "blocked",
            "the old-named file's own result must be picked up by the fallback full rebuild"
        );
    }

    #[test]
    fn merge_prefers_greater_executed_at_regardless_of_run_id() {
        let mut items = HashMap::new();
        let mut by_test_run = HashMap::new();
        let earlier = RunRecord {
            run_id: "20260101-000000-000-999999".to_string(),
            executed_at: "2026-01-01T00:00:00.000Z".to_string(),
            executor: RunExecutor {
                kind: "ai".to_string(),
                id: None,
            },
            commit: String::new(),
            task_id: None,
            test_run_id: None,
            results: vec![RunResultEntry {
                item: "ST-001".to_string(),
                result: "fail".to_string(),
                body_hash: None,
                def_hash: None,
                note: String::new(),
                carried_from: None,
                evidence: vec![],
            }],
        };
        let later = RunRecord {
            run_id: "20260101-000000-000-000001".to_string(),
            executed_at: "2026-01-02T00:00:00.000Z".to_string(),
            executor: RunExecutor {
                kind: "ai".to_string(),
                id: None,
            },
            commit: String::new(),
            task_id: None,
            test_run_id: None,
            results: vec![RunResultEntry {
                item: "ST-001".to_string(),
                result: "pass".to_string(),
                body_hash: None,
                def_hash: None,
                note: String::new(),
                carried_from: None,
                evidence: vec![],
            }],
        };
        merge_run_into_latest(&mut items, &mut by_test_run, &earlier);
        merge_run_into_latest(&mut items, &mut by_test_run, &later);
        assert_eq!(
            items.get("ST-001").unwrap().result,
            "pass",
            "a strictly later executed_at must win even with a lexicographically smaller run_id"
        );
    }

    #[test]
    fn merge_breaks_same_executed_at_tie_by_run_id_lexicographic_order() {
        let mut items = HashMap::new();
        let mut by_test_run = HashMap::new();
        let lower_run_id = RunRecord {
            run_id: "20260101-000000-000-000001".to_string(),
            executed_at: "2026-01-01T00:00:00.000Z".to_string(),
            executor: RunExecutor {
                kind: "ai".to_string(),
                id: None,
            },
            commit: String::new(),
            task_id: None,
            test_run_id: None,
            results: vec![RunResultEntry {
                item: "ST-001".to_string(),
                result: "fail".to_string(),
                body_hash: None,
                def_hash: None,
                note: String::new(),
                carried_from: None,
                evidence: vec![],
            }],
        };
        let higher_run_id = RunRecord {
            run_id: "20260101-000000-000-000002".to_string(),
            executed_at: "2026-01-01T00:00:00.000Z".to_string(),
            executor: RunExecutor {
                kind: "ai".to_string(),
                id: None,
            },
            commit: String::new(),
            task_id: None,
            test_run_id: None,
            results: vec![RunResultEntry {
                item: "ST-001".to_string(),
                result: "pass".to_string(),
                body_hash: None,
                def_hash: None,
                note: String::new(),
                carried_from: None,
                evidence: vec![],
            }],
        };
        // Process the higher run_id first, to prove the *comparison*
        // (not merge call order) decides the winner.
        merge_run_into_latest(&mut items, &mut by_test_run, &higher_run_id);
        merge_run_into_latest(&mut items, &mut by_test_run, &lower_run_id);
        assert_eq!(items.get("ST-001").unwrap().result, "pass");
    }

    #[test]
    fn merge_within_one_run_lets_a_later_array_entry_win_a_same_key_tie() {
        // Same run_id, same executed_at, same item repeated twice in one
        // run's results[] — "同時刻は results 配列内の後勝ち".
        let mut items = HashMap::new();
        let mut by_test_run = HashMap::new();
        let run = RunRecord {
            run_id: "20260101-000000-000-000001".to_string(),
            executed_at: "2026-01-01T00:00:00.000Z".to_string(),
            executor: RunExecutor {
                kind: "ai".to_string(),
                id: None,
            },
            commit: String::new(),
            task_id: None,
            test_run_id: None,
            results: vec![
                RunResultEntry {
                    item: "ST-001".to_string(),
                    result: "fail".to_string(),
                    body_hash: None,
                    def_hash: None,
                    note: String::new(),
                    carried_from: None,
                    evidence: vec![],
                },
                RunResultEntry {
                    item: "ST-001".to_string(),
                    result: "pass".to_string(),
                    body_hash: None,
                    def_hash: None,
                    note: String::new(),
                    carried_from: None,
                    evidence: vec![],
                },
            ],
        };
        merge_run_into_latest(&mut items, &mut by_test_run, &run);
        assert_eq!(items.get("ST-001").unwrap().result, "pass");
    }

    #[test]
    fn sync_skips_the_write_when_nothing_changed_since_last_sync() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup(tmp.path());
        let inputs = vec![RunResultInput {
            item: "ST-1",
            result: "pass",
            note: None,
            evidence: vec![],
        }];
        record_run(
            &handoff,
            &[],
            &inputs,
            "ai",
            None,
            Some(String::new()),
            None,
            None,
        )
        .unwrap();

        let path = handoff.join("runs").join("_latest.json");
        let before_bytes = std::fs::read(&path).unwrap();
        let before_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(20));
        let cache = sync(&handoff).unwrap();
        let after_bytes = std::fs::read(&path).unwrap();
        let after_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();

        assert_eq!(before_bytes, after_bytes);
        assert_eq!(
            before_mtime, after_mtime,
            "sync() must not rewrite _latest.json when nothing changed since the last sync"
        );
        assert_eq!(cache.count, 1);
    }

    /// M2-08 (wiki/260-vmodel-m2-design.md §2.1's E6 table, item 2):
    /// [`load_latest_readonly`] must merge in a run file written *after* the
    /// last `_latest.json` materialization without ever writing anything
    /// itself — the read-only counterpart to `sync`'s incremental-merge path.
    #[test]
    fn load_latest_readonly_merges_a_new_run_file_without_writing_anything() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup(tmp.path());
        let inputs_a = vec![RunResultInput {
            item: "ST-001",
            result: "fail",
            note: None,
            evidence: vec![],
        }];
        record_run(
            &handoff,
            &[],
            &inputs_a,
            "ai",
            None,
            Some(String::new()),
            None,
            None,
        )
        .unwrap();
        // `record_run` already materialized `_latest.json` with ST-001=fail.
        // Write a second run file directly (bypassing `record_run`, which
        // would also refresh the cache) to simulate a run landing via git
        // pull/an external writer — `_latest.json` is now stale relative to
        // the directory listing, the exact drift `load_latest_readonly` must
        // reconcile in memory without persisting it.
        std::thread::sleep(std::time::Duration::from_millis(5));
        let inputs_b = vec![RunResultInput {
            item: "ST-001",
            result: "pass",
            note: None,
            evidence: vec![],
        }];
        let (run_id_b, _) = record_run(
            &handoff,
            &[],
            &inputs_b,
            "ai",
            None,
            Some(String::new()),
            None,
            None,
        )
        .unwrap();

        // Roll `_latest.json` back to the pre-run_id_b state by re-running a
        // full rebuild from only the first run file — simulates "the cache on
        // disk predates this run file" without deleting the run file itself.
        let latest_path = handoff.join("runs").join("_latest.json");
        let stale_cache = LatestCache {
            max_run_id: Some(
                run_id_b
                    .chars()
                    .take(run_id_b.len() - 1)
                    .collect::<String>(),
            ),
            count: 1,
            items: {
                let mut m = HashMap::new();
                m.insert(
                    "ST-001".to_string(),
                    LatestItemResult {
                        result: "fail".to_string(),
                        executed_at: "2020-01-01T00:00:00Z".to_string(),
                        run_id: "stale".to_string(),
                        body_hash: None,
                        def_hash: None,
                        note: String::new(),
                        evidence: vec![],
                        carried_from: None,
                    },
                );
                m
            },
            by_test_run: HashMap::new(),
        };
        std::fs::write(&latest_path, serde_json::to_string(&stale_cache).unwrap()).unwrap();
        let before_bytes = std::fs::read(&latest_path).unwrap();

        let cache = load_latest_readonly(&handoff).unwrap();

        let after_bytes = std::fs::read(&latest_path).unwrap();
        assert_eq!(
            before_bytes, after_bytes,
            "load_latest_readonly must never write runs/_latest.json"
        );
        assert_eq!(cache.count, 2, "must reconcile in memory to the real count");
        assert_eq!(
            cache.items.get("ST-001").unwrap().result,
            "pass",
            "must merge in the run file the stale on-disk cache predates"
        );
    }

    /// E6: a project with run files but no `_latest.json` at all (never
    /// materialized) must still resolve via a full in-memory rebuild, not
    /// fail outright.
    #[test]
    fn load_latest_readonly_full_rebuild_when_no_cache_file_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup(tmp.path());
        let inputs = vec![RunResultInput {
            item: "ST-001",
            result: "pass",
            note: None,
            evidence: vec![],
        }];
        record_run(
            &handoff,
            &[],
            &inputs,
            "ai",
            None,
            Some(String::new()),
            None,
            None,
        )
        .unwrap();
        let latest_path = handoff.join("runs").join("_latest.json");
        std::fs::remove_file(&latest_path).unwrap();

        let cache = load_latest_readonly(&handoff).unwrap();

        assert_eq!(cache.count, 1);
        assert_eq!(cache.items.get("ST-001").unwrap().result, "pass");
        assert!(
            !latest_path.exists(),
            "load_latest_readonly must not materialize _latest.json as a side effect"
        );
    }

    #[test]
    fn ensure_gitignore_entry_is_idempotent_and_preserves_existing_content() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup(tmp.path());
        std::fs::write(handoff.join(".gitignore"), "existing-line\n").unwrap();

        ensure_gitignore_entry(&handoff, "/runs/_latest.json").unwrap();
        ensure_gitignore_entry(&handoff, "/runs/_latest.json").unwrap();

        let content = std::fs::read_to_string(handoff.join(".gitignore")).unwrap();
        assert_eq!(
            content.matches("/runs/_latest.json").count(),
            1,
            "must not duplicate the entry across repeated calls: {content:?}"
        );
        assert!(
            content.contains("existing-line"),
            "must preserve pre-existing content: {content:?}"
        );
    }

    #[test]
    fn record_run_writes_a_gitignore_entry_for_the_latest_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup(tmp.path());
        let inputs = vec![RunResultInput {
            item: "ST-001",
            result: "pass",
            note: None,
            evidence: vec![],
        }];
        record_run(
            &handoff,
            &[],
            &inputs,
            "ai",
            None,
            Some(String::new()),
            None,
            None,
        )
        .unwrap();

        let gitignore = std::fs::read_to_string(handoff.join(".gitignore")).unwrap();
        assert!(gitignore.contains("/runs/_latest.json"));
    }

    /// A cache-refresh failure after the run file is already persisted must
    /// not surface as an error (a retrying caller would record the batch
    /// twice) — it is returned as a warning, and the run file exists.
    #[test]
    fn record_run_keeps_the_persisted_record_and_warns_when_latest_sync_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup(tmp.path());
        std::fs::create_dir_all(handoff.join("runs")).unwrap();
        std::fs::write(handoff.join("runs").join("not-a-run.json"), "{ broken").unwrap();

        let inputs = vec![RunResultInput {
            item: "ST-001",
            result: "pass",
            note: None,
            evidence: vec![],
        }];
        let (run_id, warnings) = record_run(
            &handoff,
            &[],
            &inputs,
            "ai",
            None,
            Some(String::new()),
            None,
            None,
        )
        .expect("an already-persisted record must not be reported as a failure");

        assert!(handoff.join("runs").join(format!("{run_id}.json")).exists());
        assert!(
            warnings
                .iter()
                .any(|w| w.contains(&run_id) && w.contains("_latest.json")),
            "the cache-refresh failure must be surfaced as a warning: {warnings:?}"
        );
    }

    /// The publish step leaves no temp file behind and the final file holds
    /// the complete record.
    #[test]
    fn write_run_record_leaves_no_temp_files_and_publishes_complete_content() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup(tmp.path());
        let mut record = RunRecord {
            run_id: String::new(),
            executed_at: "2026-01-01T00:00:00.000Z".to_string(),
            executor: RunExecutor {
                kind: "ai".to_string(),
                id: None,
            },
            commit: String::new(),
            task_id: None,
            test_run_id: None,
            results: vec![],
        };
        let path = write_run_record(&handoff, &mut record, Utc::now()).unwrap();
        let entries: Vec<String> = std::fs::read_dir(handoff.join("runs"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(entries.len(), 1, "no temp file may remain: {entries:?}");
        let parsed: RunRecord =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed, record);
    }

    fn read_docs_for_test(handoff: &Path) -> Vec<crate::storage::docs::DocMetadata> {
        crate::storage::docs::read_all_docs(handoff).unwrap()
    }

    /// t360.13 (wiki/220 §3.4, `trace history`): every recorded result for
    /// the requested item, newest first, across multiple run files —
    /// including a run that also mentions a different item (which must be
    /// excluded).
    #[test]
    fn history_for_item_returns_every_matching_result_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup(tmp.path());
        record_run(
            &handoff,
            &[],
            &[RunResultInput {
                item: "ST-001",
                result: "fail",
                note: Some("first attempt"),
                evidence: vec![],
            }],
            "ai",
            None,
            Some("aaa1111".to_string()),
            None,
            None,
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        // A run recording a different item must not show up in ST-001's
        // history.
        record_run(
            &handoff,
            &[],
            &[RunResultInput {
                item: "ST-002",
                result: "pass",
                note: None,
                evidence: vec![],
            }],
            "ai",
            None,
            Some(String::new()),
            None,
            None,
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        record_run(
            &handoff,
            &[],
            &[RunResultInput {
                item: "ST-001",
                result: "pass",
                note: Some("fixed"),
                evidence: vec!["tests/e2e.rs::case".to_string()],
            }],
            "human",
            Some("qa-1"),
            Some("bbb2222".to_string()),
            None,
            None,
        )
        .unwrap();

        let history = history_for_item(&handoff, "ST-001").unwrap();
        assert_eq!(
            history.len(),
            2,
            "must only include ST-001 entries: {history:?}"
        );
        assert_eq!(history[0].result, "pass", "newest first: {history:?}");
        assert_eq!(history[0].note, "fixed");
        assert_eq!(history[0].commit, "bbb2222");
        assert_eq!(history[0].executor.kind, "human");
        assert_eq!(history[0].executor.id.as_deref(), Some("qa-1"));
        assert_eq!(history[0].evidence, vec!["tests/e2e.rs::case".to_string()]);
        assert_eq!(history[1].result, "fail", "oldest last: {history:?}");
        assert_eq!(history[1].note, "first attempt");
        assert_eq!(history[1].commit, "aaa1111");
    }

    #[test]
    fn history_for_item_is_empty_for_an_unknown_item_or_missing_runs_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup(tmp.path());
        assert!(history_for_item(&handoff, "GHOST").unwrap().is_empty());

        record_run(
            &handoff,
            &[],
            &[RunResultInput {
                item: "ST-001",
                result: "pass",
                note: None,
                evidence: vec![],
            }],
            "ai",
            None,
            Some(String::new()),
            None,
            None,
        )
        .unwrap();
        assert!(history_for_item(&handoff, "GHOST").unwrap().is_empty());
    }
}
