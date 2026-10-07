//! Append-only time log (FR-501 / SPEC-501).
//!
//! Every `handoff_log_time` call appends one entry to
//! `.handoff/time_log.jsonl` (JSON Lines: one compact JSON object per line,
//! newline-terminated), alongside the cumulative `actual_hours` update. The
//! log preserves the *time series* that `actual_hours` (a running total)
//! discards, so reports can attribute effort to a day/agent. Like
//! `events.jsonl` it is append-only and never read-modify-written, so a plain
//! `O_APPEND` write is used instead of [`crate::storage::atomic_write`].

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// File name of the log inside the `.handoff/` directory.
pub const TIME_LOG_FILE: &str = "time_log.jsonl";

/// One line of `.handoff/time_log.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimeLogEntry {
    /// RFC 3339 / ISO 8601 timestamp of the `handoff_log_time` call.
    pub ts: String,
    pub task_id: String,
    pub hours: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Append `entry` to `<handoff_dir>/time_log.jsonl`, creating the file if
/// absent.
pub fn append_time_log(handoff_dir: &Path, entry: &TimeLogEntry) -> Result<()> {
    use std::io::Write;
    let path = handoff_dir.join(TIME_LOG_FILE);
    let line = serde_json::to_string(entry).context("Failed to serialize time log entry")? + "\n";
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("Failed to open time log: {}", path.display()))?;
    file.write_all(line.as_bytes())
        .with_context(|| format!("Failed to write time log: {}", path.display()))?;
    Ok(())
}

/// Read every entry of `<handoff_dir>/time_log.jsonl` in append order.
///
/// A missing file is an empty log (no time logged yet). A line that is not a
/// valid entry (partial write, corruption) is skipped rather than failing the
/// whole read, mirroring [`crate::storage::events::read_events`].
pub fn read_time_log(handoff_dir: &Path) -> Result<Vec<TimeLogEntry>> {
    let path = handoff_dir.join(TIME_LOG_FILE);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read time log: {}", path.display()))?;
    Ok(content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<TimeLogEntry>(line).ok())
        .collect())
}
