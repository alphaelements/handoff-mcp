//! Baseline snapshots (`trace/baselines/`) — wiki/270-vmodel-m3-design.md
//! §2.4 (FR-405, M3-06). One `trace_baseline(action="create")` call appends
//! exactly one `.handoff/trace/baselines/<baseline_id>.json` file — created
//! with `create_new` (never overwritten, retried with a fresh random suffix
//! on a name collision, NFR-007 — same convention as `runs/<run_id>.json`
//! and `trace/clears/<clear_id>.json`) — and appends one entry to the
//! derived `.handoff/trace/baselines/_index.json` cache (coverage-trend data
//! for handoff-vscode's t143 chart, §5).
//!
//! `_index.json` is written tmpfile -> rename (same atomicity discipline as
//! `runs/_latest.json`) and, when corrupt or missing the `baselines` key,
//! rebuilt from `baselines/*.json` (deduped by `baseline_id`) rather than
//! treated as a hard failure — §2.4: "破損（JSONパース失敗または `baselines`
//! キー欠如）・欠落時は `baselines/*.json` から再構築する".

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `executor` field of a [`BaselineRecord`] — same shape as
/// `runs::RunExecutor`/`clears::ClearExecutor` (kept as an independent copy,
/// same rationale as `clears::ClearExecutor`: unrelated id spaces, no shared
/// cross-module type dependency worth introducing for a 2-field struct).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BaselineExecutor {
    /// `"ai"` | `"human"`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// One `.handoff/trace/baselines/<baseline_id>.json` file's full contents
/// (wiki/270 §2.4). `items`/`coverage_summary`/`gap_counts`/`state_summary`
/// are kept as raw [`Value`] rather than re-typed structs: they are a
/// verbatim lightweight extraction of `_trace_report.json`'s own
/// `items`/`coverage`/`gap_counts` (`crate::mcp::handlers::trace`'s
/// `build_report_items`/`graph.coverage()`/`graph.gap_counts()` — already
/// `Serialize`, but defined for the *response* shape, not re-exported as a
/// deserializable type any other module could reuse) plus a `state_summary`
/// this module derives itself — keeping them as `Value` avoids duplicating
/// that schema a second time only to immediately re-flatten it back to JSON
/// for storage.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BaselineRecord {
    pub baseline_id: String,
    /// RFC3339 with millisecond precision.
    pub created_at: String,
    pub executor: BaselineExecutor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub items: Vec<Value>,
    pub coverage_summary: Value,
    pub gap_counts: Value,
    pub total_items: usize,
    pub state_summary: Value,
}

/// Mirrors `storage::runs`/`storage::clears`'s own collision-avoidance random
/// suffix generator (independent copy — see `clears::random_six_digits`'s
/// doc comment for why one id space's generator is never shared with
/// another's).
static BASELINE_ID_SEQ: AtomicU64 = AtomicU64::new(0);

fn random_six_digits() -> u32 {
    let seq = BASELINE_ID_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id() as u64;
    let mixed =
        nanos ^ pid.wrapping_mul(0x2545_F491_4F6C_DD1D) ^ seq.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    (mixed % 1_000_000) as u32
}

fn format_baseline_id(now: DateTime<Utc>, rand_digits: u32) -> String {
    format!("{}-{rand_digits:06}", now.format("%Y%m%d-%H%M%S-%3f"))
}

const CREATE_BASELINE_FILE_MAX_ATTEMPTS: u32 = 20;

fn baselines_dir(handoff: &Path) -> PathBuf {
    handoff.join("trace").join("baselines")
}

fn index_path(handoff: &Path) -> PathBuf {
    baselines_dir(handoff).join("_index.json")
}

/// Writes `record` to a brand-new `.handoff/trace/baselines/<baseline_id>.json`
/// file — `create_new`, retried with a fresh random suffix on an
/// `AlreadyExists` collision (mirrors `storage::clears::write_clear_record`).
/// `record.baseline_id` is overwritten with the filename actually allocated.
pub fn write_baseline_record(handoff: &Path, record: &mut BaselineRecord) -> Result<PathBuf> {
    let dir = baselines_dir(handoff);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create dir: {}", dir.display()))?;

    let now = Utc::now();
    for _ in 0..CREATE_BASELINE_FILE_MAX_ATTEMPTS {
        let baseline_id = format_baseline_id(now, random_six_digits());
        let path = dir.join(format!("{baseline_id}.json"));
        record.baseline_id = baseline_id;
        let content =
            serde_json::to_string_pretty(record).context("Failed to serialize baseline record")?;

        let mut open_opts = std::fs::OpenOptions::new();
        open_opts.write(true).create_new(true);
        match open_opts.open(&path) {
            Ok(mut file) => {
                file.write_all(content.as_bytes())
                    .with_context(|| format!("Failed to write {}", path.display()))?;
                file.sync_all()
                    .with_context(|| format!("Failed to sync {}", path.display()))?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(e).with_context(|| format!("Failed to create {}", path.display()))
            }
        }
    }
    anyhow::bail!(
        "Failed to allocate a unique baseline_id filename under {} after {CREATE_BASELINE_FILE_MAX_ATTEMPTS} attempts",
        dir.display()
    )
}

/// One `_index.json` entry (wiki/270 §2.4) — the coverage-trend summary
/// handoff-vscode's t143 chart reads, without ever opening the full
/// `baselines/<id>.json` (which also carries the full `items[]`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BaselineIndexEntry {
    pub baseline_id: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub coverage_summary: Value,
    pub state_summary: Value,
    pub total_items: usize,
}

impl From<&BaselineRecord> for BaselineIndexEntry {
    fn from(r: &BaselineRecord) -> Self {
        BaselineIndexEntry {
            baseline_id: r.baseline_id.clone(),
            created_at: r.created_at.clone(),
            tag: r.tag.clone(),
            label: r.label.clone(),
            coverage_summary: r.coverage_summary.clone(),
            state_summary: r.state_summary.clone(),
            total_items: r.total_items,
        }
    }
}

/// On-disk shape of `_index.json` (wiki/270 §2.4: `{"baselines": [...]}`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
struct BaselineIndex {
    #[serde(default)]
    baselines: Vec<BaselineIndexEntry>,
}

/// Reads every `baselines/<id>.json` file (top-level only — unlike
/// `runs/`, wiki/270 defines no month-subdirectory convention for
/// baselines) and rebuilds a [`BaselineIndex`] from them, deduped by
/// `baseline_id` (§2.4: "`baseline_id` で重複排除") and ordered by
/// `created_at` ascending (oldest first) so [`append_index_entry`]'s own
/// append-at-end convention and [`list_baselines`]'s "reverse for newest
/// first" both stay consistent regardless of which path produced the
/// in-memory list.
fn rebuild_index_from_files(handoff: &Path) -> Result<BaselineIndex> {
    let dir = baselines_dir(handoff);
    if !dir.exists() {
        return Ok(BaselineIndex::default());
    }
    let mut entries: Vec<BaselineIndexEntry> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut read_entries = Vec::new();
    for entry in
        std::fs::read_dir(&dir).with_context(|| format!("Failed to read dir: {}", dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "_index.json" || !name.ends_with(".json") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(record) = serde_json::from_str::<BaselineRecord>(&content) else {
            continue;
        };
        read_entries.push(record);
    }
    read_entries.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    for record in &read_entries {
        if seen.insert(record.baseline_id.clone()) {
            entries.push(BaselineIndexEntry::from(record));
        }
    }
    Ok(BaselineIndex { baselines: entries })
}

/// Reads `_index.json`, treating a missing file, unparseable JSON, or a
/// parseable JSON object with no `baselines` key as "needs rebuilding"
/// (§2.4) rather than an error — [`append_index_entry`]/[`list_baselines`]
/// both fall back to [`rebuild_index_from_files`] in that case.
fn read_index(handoff: &Path) -> Option<BaselineIndex> {
    let content = std::fs::read_to_string(index_path(handoff)).ok()?;
    let value: Value = serde_json::from_str(&content).ok()?;
    if !value.get("baselines").is_some_and(Value::is_array) {
        return None;
    }
    serde_json::from_value(value).ok()
}

/// Writes `index` to `_index.json` via tmpfile -> rename (§2.4: same
/// atomicity as `runs/_latest.json`).
fn write_index(handoff: &Path, index: &BaselineIndex) -> Result<()> {
    let dir = baselines_dir(handoff);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create dir: {}", dir.display()))?;
    let path = index_path(handoff);
    let content = serde_json::to_string(index).context("Failed to serialize _index.json")?;
    crate::storage::atomic_write(&path, content.as_bytes())
        .with_context(|| format!("Failed to write {}", path.display()))
}

/// Appends `record`'s index entry to `_index.json` (wiki/270 §2.4: "ベース
/// ライン作成時にエントリを追記"). Starts from the current on-disk index when
/// it parses and carries a `baselines` key, otherwise rebuilds it from
/// `baselines/*.json` first (self-healing — the appended record's own file
/// has already been written by the time this runs, so the rebuild also picks
/// it up; the explicit push below only matters on the fast/already-valid
/// path, but is harmless — and necessary — when the rebuild predates this
/// call's own write... see [`create_baseline`] for why this function is only
/// ever called *after* [`write_baseline_record`]).
fn append_index_entry(handoff: &Path, record: &BaselineRecord) -> Result<()> {
    let mut index = match read_index(handoff) {
        Some(index) => index,
        None => rebuild_index_from_files(handoff)?,
    };
    if !index
        .baselines
        .iter()
        .any(|e| e.baseline_id == record.baseline_id)
    {
        index.baselines.push(BaselineIndexEntry::from(record));
    }
    write_index(handoff, &index)
}

/// Creates one baseline: writes `baselines/<baseline_id>.json`
/// ([`write_baseline_record`]) then appends its index entry
/// ([`append_index_entry`]) — the two steps `trace_baseline(action="create")`
/// always performs together. Returns the persisted record (with its
/// allocated `baseline_id`).
pub fn create_baseline(handoff: &Path, mut record: BaselineRecord) -> Result<BaselineRecord> {
    write_baseline_record(handoff, &mut record)?;
    append_index_entry(handoff, &record)?;
    Ok(record)
}

/// `trace_baseline(action="list")` (wiki/270 §4.1): the `limit` most recent
/// index entries, newest first. Falls back to rebuilding from
/// `baselines/*.json` the same way [`append_index_entry`] does when
/// `_index.json` is missing/corrupt — a read-only list call must never fail
/// outright just because the derived cache hasn't been materialized yet.
pub fn list_baselines(handoff: &Path, limit: usize) -> Result<(Vec<BaselineIndexEntry>, bool)> {
    let index = match read_index(handoff) {
        Some(index) => index,
        None => rebuild_index_from_files(handoff)?,
    };
    let mut entries = index.baselines;
    // Index is stored oldest-first (append order); callers want newest
    // first.
    entries.reverse();
    let truncated = entries.len() > limit;
    entries.truncate(limit);
    Ok((entries, truncated))
}

/// Reads one `baselines/<baseline_id>.json` record by id (M3-07,
/// `trace_baseline(action="diff")`'s `from`/`to` resolution, wiki/270
/// §4.1) — `Ok(None)` when no such file exists (not an error: the caller
/// turns a missing `from`/`to` into a `warnings[]` entry, same "read-only
/// call must never fail outright" discipline [`list_baselines`] already
/// follows).
pub fn read_baseline(handoff: &Path, baseline_id: &str) -> Result<Option<BaselineRecord>> {
    let path = baselines_dir(handoff).join(format!("{baseline_id}.json"));
    match std::fs::read_to_string(&path) {
        Ok(content) => {
            let record = serde_json::from_str(&content)
                .with_context(|| format!("Failed to parse {}", path.display()))?;
            Ok(Some(record))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("Failed to read {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record(label: &str) -> BaselineRecord {
        BaselineRecord {
            baseline_id: String::new(),
            created_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            executor: BaselineExecutor {
                kind: "human".to_string(),
                id: Some("ryoma".to_string()),
            },
            tag: Some("v1.0.0".to_string()),
            commit: Some("a1b2c3d".to_string()),
            label: Some(label.to_string()),
            items: vec![serde_json::json!({"id": "REQ-001"})],
            coverage_summary: serde_json::json!({"requirement": {"horizontal": {"covered": 1}}}),
            gap_counts: serde_json::json!({"unverified": 0}),
            total_items: 1,
            state_summary: serde_json::json!({"passing": 1}),
        }
    }

    #[test]
    fn write_baseline_record_creates_the_file_and_fills_in_the_allocated_id() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let mut record = sample_record("Sprint 5");
        let path = write_baseline_record(&handoff, &mut record).unwrap();

        assert!(path.exists());
        assert!(!record.baseline_id.is_empty());
        assert!(path.to_string_lossy().contains(&record.baseline_id));

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: BaselineRecord = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.label.as_deref(), Some("Sprint 5"));
        assert_eq!(parsed.tag.as_deref(), Some("v1.0.0"));
        assert_eq!(parsed.items.len(), 1);
    }

    #[test]
    fn write_baseline_record_never_overwrites_an_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let mut first = sample_record("a");
        let path1 = write_baseline_record(&handoff, &mut first).unwrap();
        let mut second = sample_record("b");
        let path2 = write_baseline_record(&handoff, &mut second).unwrap();

        assert_ne!(path1, path2);
        assert_ne!(first.baseline_id, second.baseline_id);
        assert!(path1.exists());
        assert!(path2.exists());
    }

    #[test]
    fn create_baseline_appends_an_index_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let persisted = create_baseline(&handoff, sample_record("first")).unwrap();

        let (entries, truncated) = list_baselines(&handoff, 20).unwrap();
        assert!(!truncated);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].baseline_id, persisted.baseline_id);
        assert_eq!(entries[0].label.as_deref(), Some("first"));
        assert_eq!(entries[0].total_items, 1);
    }

    #[test]
    fn list_baselines_returns_newest_first_and_respects_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let first = create_baseline(&handoff, sample_record("first")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let second = create_baseline(&handoff, sample_record("second")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let third = create_baseline(&handoff, sample_record("third")).unwrap();

        let (entries, truncated) = list_baselines(&handoff, 2).unwrap();
        assert!(truncated);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].baseline_id, third.baseline_id);
        assert_eq!(entries[1].baseline_id, second.baseline_id);
        assert!(entries.iter().all(|e| e.baseline_id != first.baseline_id));
    }

    #[test]
    fn list_baselines_rebuilds_from_files_when_index_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        create_baseline(&handoff, sample_record("only")).unwrap();
        std::fs::remove_file(index_path(&handoff)).unwrap();

        let (entries, truncated) = list_baselines(&handoff, 20).unwrap();
        assert!(!truncated);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label.as_deref(), Some("only"));
    }

    #[test]
    fn list_baselines_rebuilds_from_files_when_index_is_corrupt_json() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        create_baseline(&handoff, sample_record("only")).unwrap();
        std::fs::write(index_path(&handoff), "{ not valid json").unwrap();

        let (entries, _truncated) = list_baselines(&handoff, 20).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label.as_deref(), Some("only"));
    }

    #[test]
    fn list_baselines_rebuilds_from_files_when_baselines_key_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        create_baseline(&handoff, sample_record("only")).unwrap();
        std::fs::write(index_path(&handoff), "{\"other_key\": []}").unwrap();

        let (entries, _truncated) = list_baselines(&handoff, 20).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label.as_deref(), Some("only"));
    }

    #[test]
    fn rebuild_from_files_dedups_by_baseline_id() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let record = create_baseline(&handoff, sample_record("dup-source")).unwrap();
        // Simulate a corrupted index whose rebuild path runs twice over the
        // exact same on-disk baseline file set — must not duplicate it.
        let index = rebuild_index_from_files(&handoff).unwrap();
        assert_eq!(index.baselines.len(), 1);
        assert_eq!(index.baselines[0].baseline_id, record.baseline_id);

        let index_again = rebuild_index_from_files(&handoff).unwrap();
        assert_eq!(index_again.baselines.len(), 1);
    }

    #[test]
    fn read_baseline_returns_the_persisted_record_by_id() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let persisted = create_baseline(&handoff, sample_record("findme")).unwrap();

        let found = read_baseline(&handoff, &persisted.baseline_id)
            .unwrap()
            .expect("record must be found");
        assert_eq!(found.baseline_id, persisted.baseline_id);
        assert_eq!(found.label.as_deref(), Some("findme"));
    }

    #[test]
    fn read_baseline_returns_none_for_an_unknown_id() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let found = read_baseline(&handoff, "does-not-exist").unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn list_baselines_on_empty_project_returns_empty_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let (entries, truncated) = list_baselines(&handoff, 20).unwrap();
        assert!(entries.is_empty());
        assert!(!truncated);
    }
}
