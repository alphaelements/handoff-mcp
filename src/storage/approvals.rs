//! Audit files for approval transitions to `"approved"`
//! (wiki/270-vmodel-m3-design.md §2.3, M3-03, FR-406): one
//! `.handoff/trace/approvals/<YYYYMMDD-HHMMSS-mmm>-<6桁乱数>.json` file per
//! `trace_update(set.approval="approved")` call, `create_new` (never
//! overwritten, retried with a fresh random suffix on a filename collision —
//! same convention as `runs/<run_id>.json` and `storage::clears`, NFR-007).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// `executor` field of an [`ApprovalRecord`] — same shape as
/// `clears::ClearExecutor`/`runs::RunExecutor` (kept as an independent copy
/// rather than shared, per this codebase's established convention: each
/// audit record type is self-contained, see `clears.rs`'s own doc comment on
/// `ClearExecutor`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApprovalExecutor {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// One item moved to `approval: "approved"` in this call (wiki/270 §2.3's
/// JSON example).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApprovedItem {
    pub id: String,
    pub from_approval: String,
    pub to_approval: String,
    pub def_hash: Option<String>,
}

/// One `.handoff/trace/approvals/<approval_id>.json` file's full contents
/// (wiki/270-vmodel-m3-design.md §2.3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApprovalRecord {
    pub approval_id: String,
    /// RFC3339 with millisecond precision.
    pub approved_at: String,
    pub executor: ApprovalExecutor,
    pub items: Vec<ApprovedItem>,
}

/// Mirrors `storage::clears`'s own collision-avoidance random suffix
/// generator (same 6-digit, non-cryptographic scheme, NFR-007's "clears/runs
/// と同じ規約" — kept as an independent copy per this module's established
/// convention, see `clears.rs`'s doc comment on `CLEAR_ID_SEQ`).
static APPROVAL_ID_SEQ: AtomicU64 = AtomicU64::new(0);

fn random_six_digits() -> u32 {
    let seq = APPROVAL_ID_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id() as u64;
    let mixed =
        nanos ^ pid.wrapping_mul(0x2545_F491_4F6C_DD1D) ^ seq.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    (mixed % 1_000_000) as u32
}

fn format_approval_id(now: DateTime<Utc>, rand_digits: u32) -> String {
    format!("{}-{rand_digits:06}", now.format("%Y%m%d-%H%M%S-%3f"))
}

const CREATE_APPROVAL_FILE_MAX_ATTEMPTS: u32 = 20;

fn approvals_dir(handoff: &Path) -> PathBuf {
    handoff.join("trace").join("approvals")
}

/// Writes `record` to a brand-new `.handoff/trace/approvals/<approval_id>.json`
/// file — `create_new`, retried with a fresh random suffix on an
/// `AlreadyExists` collision (mirrors `storage::clears::write_clear_record`).
/// `record.approval_id` is overwritten with the filename actually allocated.
pub fn write_approval_record(handoff: &Path, record: &mut ApprovalRecord) -> Result<PathBuf> {
    let dir = approvals_dir(handoff);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create dir: {}", dir.display()))?;

    let now = Utc::now();
    for _ in 0..CREATE_APPROVAL_FILE_MAX_ATTEMPTS {
        let approval_id = format_approval_id(now, random_six_digits());
        let path = dir.join(format!("{approval_id}.json"));
        record.approval_id = approval_id;
        let content =
            serde_json::to_string_pretty(record).context("Failed to serialize approval record")?;

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
        "Failed to allocate a unique approval_id filename under {} after \
         {CREATE_APPROVAL_FILE_MAX_ATTEMPTS} attempts",
        dir.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_approval_record_creates_the_file_and_fills_in_the_allocated_approval_id() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let mut record = ApprovalRecord {
            approval_id: String::new(),
            approved_at: "2026-10-05T14:15:00.123Z".to_string(),
            executor: ApprovalExecutor {
                kind: "human".to_string(),
                id: Some("ryoma".to_string()),
            },
            items: vec![ApprovedItem {
                id: "REQ-003".to_string(),
                from_approval: "review".to_string(),
                to_approval: "approved".to_string(),
                def_hash: Some("a1b2c3d4e5f6g7h8".to_string()),
            }],
        };

        let path = write_approval_record(&handoff, &mut record).unwrap();

        assert!(path.exists());
        assert!(!record.approval_id.is_empty());
        assert!(path.to_string_lossy().contains(&record.approval_id));

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: ApprovalRecord = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.items.len(), 1);
        assert_eq!(parsed.items[0].id, "REQ-003");
        assert_eq!(parsed.items[0].from_approval, "review");
        assert_eq!(parsed.items[0].to_approval, "approved");
    }

    #[test]
    fn write_approval_record_never_overwrites_an_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let base = ApprovalRecord {
            approval_id: String::new(),
            approved_at: "2026-10-05T14:15:00.123Z".to_string(),
            executor: ApprovalExecutor {
                kind: "ai".to_string(),
                id: None,
            },
            items: vec![],
        };

        let mut first = base.clone();
        let path1 = write_approval_record(&handoff, &mut first).unwrap();
        let mut second = base;
        let path2 = write_approval_record(&handoff, &mut second).unwrap();

        assert_ne!(path1, path2);
        assert_ne!(first.approval_id, second.approval_id);
        assert!(path1.exists());
        assert!(path2.exists());
    }
}
