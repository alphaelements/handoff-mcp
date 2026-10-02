//! Change-proposal (delta) persistence (`trace/deltas/`) — wiki/270-vmodel-m3-design.md
//! §2.5 (FR-407, M3-08). `trace_delta(action="create")` (and
//! `trace_update(propose=true)`) append exactly one
//! `.handoff/trace/deltas/<delta_id>.json` file — created with `create_new`
//! (never overwritten, retried with a fresh random suffix on a name
//! collision, NFR-007 — same convention as `runs/<run_id>.json`,
//! `trace/clears/<clear_id>.json`, and `trace/baselines/<baseline_id>.json`).
//! There is no derived `_index.json` cache here (unlike `baselines`):
//! `trace_delta(action="list")`'s own `stale` flag must re-read each
//! candidate's current `def_hash` on every call anyway (§4.2), so a cache of
//! anything *other than* that per-call-fresh comparison would buy nothing.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `executor` field of a [`DeltaRecord`] — same shape as
/// `runs::RunExecutor`/`clears::ClearExecutor`/`baselines::BaselineExecutor`
/// (kept as an independent copy, same rationale as those siblings: unrelated
/// id spaces, no shared cross-module type dependency worth introducing for a
/// 2-field struct).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeltaExecutor {
    /// `"ai"` | `"human"`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// One `op_index`'s unified-diff preview (wiki/270 §2.5's worked example) —
/// `op_index` here is always the *renumbered* (0-based, within this delta's
/// own `ops` array) index, never the original caller-supplied one from the
/// `trace_delta(action="create")`/`trace_update(propose=true)` call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeltaPreview {
    pub op_index: usize,
    pub diff: String,
    pub result: Value,
}

/// One `.handoff/trace/deltas/<delta_id>.json` file's full contents (wiki/270
/// §2.5). `ops` holds the original op objects verbatim (already validated by
/// phase-1 at create time) so `apply`/the remainder-delta split can replay
/// them unchanged through `trace_update`'s own write path later.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeltaRecord {
    pub delta_id: String,
    /// RFC3339 with millisecond precision.
    pub created_at: String,
    pub executor: DeltaExecutor,
    /// `"pending"` | `"applied"` | `"rejected"`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub ops: Vec<Value>,
    pub previews: Vec<DeltaPreview>,
    /// `stable_id -> def_hash at delta-creation time` (§2.5: staleness
    /// detection input for every op's target item(s)).
    #[serde(default)]
    pub baseline_hashes: std::collections::BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_reason: Option<String>,
}

/// Mirrors `storage::runs`/`storage::clears`/`storage::baselines`'s own
/// collision-avoidance random suffix generator (independent copy — see
/// `clears::random_six_digits`'s doc comment for why one id space's
/// generator is never shared with another's).
static DELTA_ID_SEQ: AtomicU64 = AtomicU64::new(0);

fn random_six_digits() -> u32 {
    let seq = DELTA_ID_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id() as u64;
    let mixed =
        nanos ^ pid.wrapping_mul(0x2545_F491_4F6C_DD1D) ^ seq.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    (mixed % 1_000_000) as u32
}

fn format_delta_id(now: DateTime<Utc>, rand_digits: u32) -> String {
    format!("{}-{rand_digits:06}", now.format("%Y%m%d-%H%M%S-%3f"))
}

const CREATE_DELTA_FILE_MAX_ATTEMPTS: u32 = 20;

fn deltas_dir(handoff: &Path) -> PathBuf {
    handoff.join("trace").join("deltas")
}

fn delta_path(handoff: &Path, delta_id: &str) -> PathBuf {
    deltas_dir(handoff).join(format!("{delta_id}.json"))
}

/// Writes `record` to a brand-new `.handoff/trace/deltas/<delta_id>.json`
/// file — `create_new`, retried with a fresh random suffix on an
/// `AlreadyExists` collision (mirrors `storage::baselines::write_baseline_record`).
/// `record.delta_id` is overwritten with the filename actually allocated.
pub fn write_delta_record(handoff: &Path, record: &mut DeltaRecord) -> Result<PathBuf> {
    let dir = deltas_dir(handoff);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create dir: {}", dir.display()))?;

    let now = Utc::now();
    for _ in 0..CREATE_DELTA_FILE_MAX_ATTEMPTS {
        let delta_id = format_delta_id(now, random_six_digits());
        let path = dir.join(format!("{delta_id}.json"));
        record.delta_id = delta_id;
        let content =
            serde_json::to_string_pretty(record).context("Failed to serialize delta record")?;

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
        "Failed to allocate a unique delta_id filename under {} after {CREATE_DELTA_FILE_MAX_ATTEMPTS} attempts",
        dir.display()
    )
}

/// Overwrites an existing `<delta_id>.json` in place (status transitions —
/// `pending -> applied`/`pending -> rejected` — are the only post-create
/// mutation a delta file ever undergoes; tmpfile->rename, same atomicity
/// discipline as `baselines::write_index`).
pub fn write_delta_record_in_place(handoff: &Path, record: &DeltaRecord) -> Result<()> {
    let path = delta_path(handoff, &record.delta_id);
    let content =
        serde_json::to_string_pretty(record).context("Failed to serialize delta record")?;
    crate::storage::atomic_write(&path, content.as_bytes())
        .with_context(|| format!("Failed to write {}", path.display()))
}

/// Reads one `<delta_id>.json` record, or `Ok(None)` if it doesn't exist.
pub fn read_delta(handoff: &Path, delta_id: &str) -> Result<Option<DeltaRecord>> {
    let path = delta_path(handoff, delta_id);
    match std::fs::read_to_string(&path) {
        Ok(content) => {
            let record: DeltaRecord = serde_json::from_str(&content)
                .with_context(|| format!("Failed to parse {}", path.display()))?;
            Ok(Some(record))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("Failed to read {}", path.display())),
    }
}

/// Every `trace/deltas/*.json` record, in no particular order (callers sort
/// as needed — `trace_delta(action="list")` sorts newest-first via
/// `created_at`). Skips a corrupt/unparseable file rather than failing the
/// whole read (same self-healing posture `baselines::rebuild_index_from_files`
/// takes toward one bad entry).
pub fn list_all_deltas(handoff: &Path) -> Result<Vec<DeltaRecord>> {
    let dir = deltas_dir(handoff);
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
        let Ok(record) = serde_json::from_str::<DeltaRecord>(&content) else {
            continue;
        };
        out.push(record);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record(description: &str) -> DeltaRecord {
        DeltaRecord {
            delta_id: String::new(),
            created_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            executor: DeltaExecutor {
                kind: "ai".to_string(),
                id: Some("s-test".to_string()),
            },
            status: "pending".to_string(),
            description: Some(description.to_string()),
            ops: vec![serde_json::json!({
                "op": "upsert_item", "doc": "req-main", "id": "REQ-100",
                "attrs": {"priority": "P1"}
            })],
            previews: vec![DeltaPreview {
                op_index: 0,
                diff: "--- a/REQ-100\n+++ b/REQ-100\n".to_string(),
                result: serde_json::json!({"doc": "req-main", "id": "REQ-100"}),
            }],
            baseline_hashes: std::collections::BTreeMap::from([(
                "REQ-100".to_string(),
                "a1b2c3d4".to_string(),
            )]),
            resolved_at: None,
            resolved_by: None,
            resolution_reason: None,
        }
    }

    #[test]
    fn write_delta_record_creates_the_file_and_fills_in_the_allocated_id() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let mut record = sample_record("add acceptance criteria");
        let path = write_delta_record(&handoff, &mut record).unwrap();

        assert!(path.exists());
        assert!(!record.delta_id.is_empty());
        assert!(path.to_string_lossy().contains(&record.delta_id));

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: DeltaRecord = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.status, "pending");
        assert_eq!(parsed.ops.len(), 1);
        assert_eq!(parsed.previews.len(), 1);
        assert_eq!(
            parsed.baseline_hashes.get("REQ-100").map(String::as_str),
            Some("a1b2c3d4")
        );
    }

    #[test]
    fn write_delta_record_never_overwrites_an_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let mut first = sample_record("a");
        let path1 = write_delta_record(&handoff, &mut first).unwrap();
        let mut second = sample_record("b");
        let path2 = write_delta_record(&handoff, &mut second).unwrap();

        assert_ne!(path1, path2);
        assert_ne!(first.delta_id, second.delta_id);
        assert!(path1.exists());
        assert!(path2.exists());
    }

    #[test]
    fn read_delta_returns_none_for_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        assert!(read_delta(&handoff, "does-not-exist").unwrap().is_none());
    }

    #[test]
    fn read_delta_round_trips_a_written_record() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let mut record = sample_record("round trip");
        write_delta_record(&handoff, &mut record).unwrap();

        let read_back = read_delta(&handoff, &record.delta_id).unwrap().unwrap();
        assert_eq!(read_back, record);
    }

    #[test]
    fn write_delta_record_in_place_updates_status_transition() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let mut record = sample_record("to be applied");
        write_delta_record(&handoff, &mut record).unwrap();

        record.status = "applied".to_string();
        record.resolved_at = Some("2026-10-03T00:00:00.000Z".to_string());
        record.resolved_by = Some("ai:s-test".to_string());
        write_delta_record_in_place(&handoff, &record).unwrap();

        let read_back = read_delta(&handoff, &record.delta_id).unwrap().unwrap();
        assert_eq!(read_back.status, "applied");
        assert_eq!(read_back.resolved_by.as_deref(), Some("ai:s-test"));
    }

    #[test]
    fn list_all_deltas_on_empty_project_returns_empty_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let deltas = list_all_deltas(&handoff).unwrap();
        assert!(deltas.is_empty());
    }

    #[test]
    fn list_all_deltas_returns_every_written_record() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let mut first = sample_record("first");
        write_delta_record(&handoff, &mut first).unwrap();
        let mut second = sample_record("second");
        write_delta_record(&handoff, &mut second).unwrap();

        let deltas = list_all_deltas(&handoff).unwrap();
        assert_eq!(deltas.len(), 2);
        let ids: Vec<&str> = deltas.iter().map(|d| d.delta_id.as_str()).collect();
        assert!(ids.contains(&first.delta_id.as_str()));
        assert!(ids.contains(&second.delta_id.as_str()));
    }

    #[test]
    fn list_all_deltas_skips_corrupt_files() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let mut record = sample_record("good");
        write_delta_record(&handoff, &mut record).unwrap();
        std::fs::write(deltas_dir(&handoff).join("corrupt.json"), "{ not valid").unwrap();

        let deltas = list_all_deltas(&handoff).unwrap();
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].delta_id, record.delta_id);
    }
}
