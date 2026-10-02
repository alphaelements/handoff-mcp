//! Test run definitions (`trace/test_runs/`) — wiki/270-vmodel-m3-design.md
//! §2.6/§4.4 (FR-304, M3-09). One `handoff_trace_test_run(action="create")`
//! call writes exactly one `.handoff/trace/test_runs/<test_run_id>.json`
//! definition file — created with `create_new` (never overwritten, retried
//! with a fresh random suffix on a name collision, same NFR-007 convention as
//! `runs/<run_id>.json`/`trace/baselines/<baseline_id>.json`). Unlike
//! baselines, there is no derived `_index.json` cache here: `list` (§4.4)
//! simply reads every `test_runs/*.json` file directly (a test run is
//! expected to be a relatively rare, human-scale object — nowhere near the
//! volume of `runs/*.json` that justified `_latest.json`'s incremental
//! cache), and `progress` (§4.4) is a live, uncached aggregation over
//! `runs/*.json` filtered by `test_run_id` (§2.6: "テストランの進捗は
//! `runs/*.json` を `test_run_id` でフィルタして動的に計算する").

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// `scope` of a [`TestRunRecord`] (wiki/270 §2.6) — the same select
/// vocabulary `handoff_trace_next`'s own `layers?`/`kinds?`/`assignee?`
/// filters use, snapshotted at `create` time so a later `list`/`progress`
/// call can still show what this test run was originally scoped to even as
/// the underlying trace graph evolves.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TestRunScope {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
}

/// One `.handoff/trace/test_runs/<test_run_id>.json` file's full contents
/// (wiki/270 §2.6's worked example).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TestRunRecord {
    pub test_run_id: String,
    /// RFC3339 with millisecond precision.
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub scope: TestRunScope,
    pub target_items: Vec<String>,
    pub total_target_count: usize,
}

/// Mirrors `storage::runs`/`storage::baselines`/`storage::clears`'s own
/// collision-avoidance random suffix generator (independent copy — each id
/// space gets its own generator/counter rather than sharing one across
/// modules, same rationale as those modules' own doc comments).
static TEST_RUN_ID_SEQ: AtomicU64 = AtomicU64::new(0);

fn random_six_digits() -> u32 {
    let seq = TEST_RUN_ID_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id() as u64;
    let mixed =
        nanos ^ pid.wrapping_mul(0x2545_F491_4F6C_DD1D) ^ seq.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    (mixed % 1_000_000) as u32
}

fn format_test_run_id(now: DateTime<Utc>, rand_digits: u32) -> String {
    format!("{}-{rand_digits:06}", now.format("%Y%m%d-%H%M%S-%3f"))
}

const CREATE_TEST_RUN_FILE_MAX_ATTEMPTS: u32 = 20;

pub fn test_runs_dir(handoff: &Path) -> PathBuf {
    handoff.join("trace").join("test_runs")
}

/// Writes `record` to a brand-new
/// `.handoff/trace/test_runs/<test_run_id>.json` file — `create_new`, retried
/// with a fresh random suffix on an `AlreadyExists` collision (mirrors
/// `storage::baselines::write_baseline_record`). `record.test_run_id` is
/// overwritten with the filename actually allocated.
pub fn write_test_run_record(handoff: &Path, mut record: TestRunRecord) -> Result<TestRunRecord> {
    let dir = test_runs_dir(handoff);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create dir: {}", dir.display()))?;

    let now = Utc::now();
    for _ in 0..CREATE_TEST_RUN_FILE_MAX_ATTEMPTS {
        let test_run_id = format_test_run_id(now, random_six_digits());
        let path = dir.join(format!("{test_run_id}.json"));
        record.test_run_id = test_run_id;
        let content =
            serde_json::to_string_pretty(&record).context("Failed to serialize test run record")?;

        let mut open_opts = std::fs::OpenOptions::new();
        open_opts.write(true).create_new(true);
        match open_opts.open(&path) {
            Ok(mut file) => {
                file.write_all(content.as_bytes())
                    .with_context(|| format!("Failed to write {}", path.display()))?;
                file.sync_all()
                    .with_context(|| format!("Failed to sync {}", path.display()))?;
                return Ok(record);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(e).with_context(|| format!("Failed to create {}", path.display()))
            }
        }
    }
    anyhow::bail!(
        "Failed to allocate a unique test_run_id filename under {} after {CREATE_TEST_RUN_FILE_MAX_ATTEMPTS} attempts",
        dir.display()
    )
}

/// Reads every `test_runs/<id>.json` file (top-level only, no
/// month-subdirectory convention here) and returns them sorted by
/// `created_at` ascending (oldest first) — [`list_test_runs`]'s own
/// "reverse for newest first" stays the single place that decides display
/// order. A file that fails to parse is skipped rather than failing the
/// whole list (self-healing in the same spirit as
/// `storage::baselines::rebuild_index_from_files`, though this module has no
/// separate index file to fall back *from* — every call reads the source
/// files directly).
fn read_all_test_runs(handoff: &Path) -> Result<Vec<TestRunRecord>> {
    let dir = test_runs_dir(handoff);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in
        std::fs::read_dir(&dir).with_context(|| format!("Failed to read dir: {}", dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".json") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(record) = serde_json::from_str::<TestRunRecord>(&content) else {
            continue;
        };
        out.push(record);
    }
    out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    Ok(out)
}

/// `handoff_trace_test_run(action="list")` (wiki/270 §4.4): the `limit` most
/// recent test run definitions, newest first. Returns `(entries, truncated)`
/// — `truncated` is `true` when more exist beyond `limit`.
pub fn list_test_runs(handoff: &Path, limit: usize) -> Result<(Vec<TestRunRecord>, bool)> {
    let mut entries = read_all_test_runs(handoff)?;
    entries.reverse();
    let truncated = entries.len() > limit;
    entries.truncate(limit);
    Ok((entries, truncated))
}

/// `handoff_trace_test_run(action="progress"/action="create")`'s lookup of
/// one test run's own definition by id — `None` when no such
/// `test_runs/<test_run_id>.json` file exists.
pub fn find_test_run(handoff: &Path, test_run_id: &str) -> Result<Option<TestRunRecord>> {
    let path = test_runs_dir(handoff).join(format!("{test_run_id}.json"));
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let record = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(Some(record))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(label: &str, target_items: Vec<&str>) -> TestRunRecord {
        TestRunRecord {
            test_run_id: String::new(),
            created_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            label: Some(label.to_string()),
            scope: TestRunScope {
                layers: vec!["acceptance".to_string(), "system_test".to_string()],
                kinds: vec!["rerun".to_string(), "fix_failing".to_string()],
                assignee: Some("ryoma".to_string()),
            },
            target_items: target_items.into_iter().map(String::from).collect(),
            total_target_count: 0, // overwritten below per-call to match target_items
        }
    }

    #[test]
    fn write_test_run_record_creates_the_file_and_fills_in_the_allocated_id() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let mut record = sample("Sprint 5 regression", vec!["AT-REQ-001-1", "ST-040"]);
        record.total_target_count = record.target_items.len();
        let persisted = write_test_run_record(&handoff, record).unwrap();

        assert!(!persisted.test_run_id.is_empty());
        let path = test_runs_dir(&handoff).join(format!("{}.json", persisted.test_run_id));
        assert!(path.exists());

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: TestRunRecord = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.label.as_deref(), Some("Sprint 5 regression"));
        assert_eq!(parsed.scope.layers, vec!["acceptance", "system_test"]);
        assert_eq!(parsed.scope.kinds, vec!["rerun", "fix_failing"]);
        assert_eq!(parsed.scope.assignee.as_deref(), Some("ryoma"));
        assert_eq!(parsed.target_items, vec!["AT-REQ-001-1", "ST-040"]);
        assert_eq!(parsed.total_target_count, 2);
    }

    #[test]
    fn write_test_run_record_never_overwrites_an_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let first = write_test_run_record(&handoff, sample("a", vec!["X-1"])).unwrap();
        let second = write_test_run_record(&handoff, sample("b", vec!["X-2"])).unwrap();

        assert_ne!(first.test_run_id, second.test_run_id);
        assert!(test_runs_dir(&handoff)
            .join(format!("{}.json", first.test_run_id))
            .exists());
        assert!(test_runs_dir(&handoff)
            .join(format!("{}.json", second.test_run_id))
            .exists());
    }

    #[test]
    fn list_test_runs_returns_newest_first_and_respects_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let first = write_test_run_record(&handoff, sample("first", vec!["X-1"])).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let second = write_test_run_record(&handoff, sample("second", vec!["X-2"])).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let third = write_test_run_record(&handoff, sample("third", vec!["X-3"])).unwrap();

        let (entries, truncated) = list_test_runs(&handoff, 2).unwrap();
        assert!(truncated);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].test_run_id, third.test_run_id);
        assert_eq!(entries[1].test_run_id, second.test_run_id);
        assert!(entries.iter().all(|e| e.test_run_id != first.test_run_id));
    }

    #[test]
    fn list_test_runs_on_empty_project_returns_empty_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let (entries, truncated) = list_test_runs(&handoff, 20).unwrap();
        assert!(entries.is_empty());
        assert!(!truncated);
    }

    #[test]
    fn find_test_run_returns_none_for_an_unknown_id() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        assert!(find_test_run(&handoff, "ghost").unwrap().is_none());
    }

    #[test]
    fn find_test_run_returns_the_matching_record() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let persisted = write_test_run_record(&handoff, sample("only", vec!["X-1"])).unwrap();
        let found = find_test_run(&handoff, &persisted.test_run_id)
            .unwrap()
            .expect("must find the record just written");
        assert_eq!(found.label.as_deref(), Some("only"));
    }
}
