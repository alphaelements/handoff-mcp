//! Audit files for `trace_suspect(action="clear")`
//! (wiki/260-vmodel-m2-design.md §4.1, M2-05): one `.handoff/trace/clears/
//! <YYYYMMDD-HHMMSS-mmm>-<6桁乱数>.json` file per call, `create_new` (never
//! overwritten, retried with a fresh random suffix on a filename collision —
//! same convention as `runs/<run_id>.json`, NFR-007).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// `executor` field of a [`ClearRecord`] — same shape as `runs::RunExecutor`
/// (not reused directly: that type lives in a different module and this
/// avoids a cross-module type dependency for what is otherwise a
/// self-contained audit record).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClearExecutor {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// `evidence` field of a [`ClearRecord`] (wiki/260 §4.1's `clear` input).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ClearEvidence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl ClearEvidence {
    fn is_empty(&self) -> bool {
        self.run_id.is_none() && self.commit.is_none() && self.note.is_none()
    }
}

/// One cleared `link`-kind suspect (§3.2/§4.1).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClearedLink {
    pub child: String,
    pub upstream: String,
    /// `"refines"` | `"verifies"`.
    #[serde(rename = "type")]
    pub link_type: String,
    pub from_hash: String,
    pub to_hash: String,
}

/// One cleared `task`-kind suspect.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClearedTask {
    pub task: String,
    pub item: String,
    pub from_hash: String,
    pub to_hash: String,
}

/// One cleared `result`-kind suspect — the audit trail points at the new
/// carried-forward `runs/<run_id>.json` entry (D2: the result's own
/// authority stays runs-only).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClearedResult {
    pub item: String,
    pub run_id: String,
}

/// One `.handoff/trace/clears/<clear_id>.json` file's full contents
/// (wiki/260 §4.1).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClearRecord {
    pub clear_id: String,
    /// RFC3339 with millisecond precision.
    pub cleared_at: String,
    pub executor: ClearExecutor,
    pub reason: String,
    #[serde(default, skip_serializing_if = "ClearEvidence::is_empty")]
    pub evidence: ClearEvidence,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<ClearedLink>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<ClearedTask>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub results: Vec<ClearedResult>,
}

/// Mirrors `storage::runs`'s own collision-avoidance random suffix
/// generator (same 6-digit, non-cryptographic scheme, NFR-007's "runs と
/// 同じ規約" — kept as an independent copy rather than exported from `runs`,
/// since the two id spaces are unrelated and a future change to one's
/// collision strategy should not silently affect the other).
static CLEAR_ID_SEQ: AtomicU64 = AtomicU64::new(0);

fn random_six_digits() -> u32 {
    let seq = CLEAR_ID_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id() as u64;
    let mixed =
        nanos ^ pid.wrapping_mul(0x2545_F491_4F6C_DD1D) ^ seq.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    (mixed % 1_000_000) as u32
}

fn format_clear_id(now: DateTime<Utc>, rand_digits: u32) -> String {
    format!("{}-{rand_digits:06}", now.format("%Y%m%d-%H%M%S-%3f"))
}

const CREATE_CLEAR_FILE_MAX_ATTEMPTS: u32 = 20;

fn clears_dir(handoff: &Path) -> PathBuf {
    handoff.join("trace").join("clears")
}

/// Writes `record` to a brand-new `.handoff/trace/clears/<clear_id>.json`
/// file — `create_new`, retried with a fresh random suffix on an
/// `AlreadyExists` collision (mirrors `storage::runs::write_run_record`).
/// `record.clear_id` is overwritten with the filename actually allocated.
pub fn write_clear_record(handoff: &Path, record: &mut ClearRecord) -> Result<PathBuf> {
    let dir = clears_dir(handoff);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create dir: {}", dir.display()))?;

    let now = Utc::now();
    for _ in 0..CREATE_CLEAR_FILE_MAX_ATTEMPTS {
        let clear_id = format_clear_id(now, random_six_digits());
        let path = dir.join(format!("{clear_id}.json"));
        record.clear_id = clear_id;
        let content =
            serde_json::to_string_pretty(record).context("Failed to serialize clear record")?;

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
        "Failed to allocate a unique clear_id filename under {} after {CREATE_CLEAR_FILE_MAX_ATTEMPTS} attempts",
        dir.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_clear_record_creates_the_file_and_fills_in_the_allocated_clear_id() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let mut record = ClearRecord {
            clear_id: String::new(),
            cleared_at: "2026-09-30T10:15:00.123Z".to_string(),
            executor: ClearExecutor {
                kind: "human".to_string(),
                id: Some("ryoma".to_string()),
            },
            reason: "text edit only, meaning unchanged".to_string(),
            evidence: ClearEvidence {
                commit: Some("a1b2c3d".to_string()),
                ..Default::default()
            },
            links: vec![ClearedLink {
                child: "SPEC-012".to_string(),
                upstream: "REQ-003".to_string(),
                link_type: "refines".to_string(),
                from_hash: "9f".to_string(),
                to_hash: "7c".to_string(),
            }],
            tasks: vec![],
            results: vec![],
        };

        let path = write_clear_record(&handoff, &mut record).unwrap();

        assert!(path.exists());
        assert!(!record.clear_id.is_empty());
        assert!(path.to_string_lossy().contains(&record.clear_id));

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: ClearRecord = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.reason, "text edit only, meaning unchanged");
        assert_eq!(parsed.links.len(), 1);
        assert_eq!(parsed.links[0].child, "SPEC-012");
        assert!(parsed.tasks.is_empty());
        assert!(parsed.results.is_empty());
    }

    #[test]
    fn write_clear_record_never_overwrites_an_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let base = ClearRecord {
            clear_id: String::new(),
            cleared_at: "2026-09-30T10:15:00.123Z".to_string(),
            executor: ClearExecutor {
                kind: "ai".to_string(),
                id: None,
            },
            reason: "r".to_string(),
            evidence: ClearEvidence::default(),
            links: vec![],
            tasks: vec![],
            results: vec![],
        };

        let mut first = base.clone();
        let path1 = write_clear_record(&handoff, &mut first).unwrap();
        let mut second = base;
        let path2 = write_clear_record(&handoff, &mut second).unwrap();

        assert_ne!(path1, path2);
        assert_ne!(first.clear_id, second.clear_id);
        assert!(path1.exists());
        assert!(path2.exists());
    }
}
