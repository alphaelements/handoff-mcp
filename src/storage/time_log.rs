//! Append-only time log (FR-501 / SPEC-501).
//!
//! Every `handoff_log_time` call appends one entry to
//! `.handoff/time_log.jsonl` (JSON Lines: one compact JSON object per line,
//! newline-terminated), alongside the cumulative `actual_hours` update. The
//! log preserves the *time series* that `actual_hours` (a running total)
//! discards, so reports can attribute effort to a day/agent. Like
//! `events.jsonl` it is append-only and never read-modify-written, so a plain
//! `O_APPEND` write is used instead of [`crate::storage::atomic_write`].
//!
//! Entries have two origins, told apart by [`TimeLogEntry::source`]:
//! `handoff_log_time` (no `source`, `hours > 0`) and a direct
//! `schedule.actual_hours` set via `handoff_update_task` /
//! `handoff_bulk_update_tasks` ([`SOURCE_DIRECT_UPDATE`]), which logs the
//! *difference* and may therefore be negative (a correction). Report
//! aggregations simply sum `hours`, so a correction nets out the earlier
//! over-count.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// File name of the log inside the `.handoff/` directory.
pub const TIME_LOG_FILE: &str = "time_log.jsonl";

/// [`TimeLogEntry::source`] marker for an entry written because
/// `schedule.actual_hours` was set directly (not through `handoff_log_time`).
pub const SOURCE_DIRECT_UPDATE: &str = "direct_update";

/// Differences below this are float noise, not a change worth logging.
const HOURS_EPSILON: f64 = 1e-9;

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
    /// `None` for a `handoff_log_time` entry; [`SOURCE_DIRECT_UPDATE`] for a
    /// delta recorded because `actual_hours` was set directly. Such an entry
    /// is a correction when `hours` is negative.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl TimeLogEntry {
    /// The entry recording a direct `actual_hours` change from `before`
    /// (`None` counts as 0) to `after`, or `None` when nothing changed.
    /// `hours` is the signed difference; a decrease is a correction.
    pub fn for_direct_actual_hours_change(
        ts: String,
        task_id: &str,
        before: Option<f64>,
        after: f64,
        agent_id: Option<&str>,
    ) -> Option<TimeLogEntry> {
        let before_hours = before.unwrap_or(0.0);
        // Rounded to the epsilon grid so f64 subtraction noise
        // (4.1 - 1.5 = 2.5999999999999996) does not leak into the log.
        let delta = ((after - before_hours) / HOURS_EPSILON).round() * HOURS_EPSILON;
        if delta.abs() < HOURS_EPSILON {
            return None;
        }
        let kind = if delta < 0.0 {
            "correction"
        } else {
            "increase"
        };
        Some(TimeLogEntry {
            ts,
            task_id: task_id.to_string(),
            hours: delta,
            agent_id: agent_id.map(str::to_string),
            note: Some(format!(
                "actual_hours set directly: {before_hours} -> {after} ({kind})"
            )),
            source: Some(SOURCE_DIRECT_UPDATE.to_string()),
        })
    }
}

/// Appends the [`TimeLogEntry::for_direct_actual_hours_change`] entry for a
/// committed direct `actual_hours` change. Returns whether an entry was
/// written (`false` when the value did not actually change).
pub fn record_direct_actual_hours_change(
    handoff_dir: &Path,
    task_id: &str,
    before: Option<f64>,
    after: f64,
    agent_id: Option<&str>,
) -> Result<bool> {
    let Some(entry) = TimeLogEntry::for_direct_actual_hours_change(
        chrono::Utc::now().to_rfc3339(),
        task_id,
        before,
        after,
        agent_id,
    ) else {
        return Ok(false);
    };
    append_time_log(handoff_dir, &entry)?;
    Ok(true)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(before: Option<f64>, after: f64) -> Option<TimeLogEntry> {
        TimeLogEntry::for_direct_actual_hours_change(
            "2026-10-20T00:00:00+00:00".to_string(),
            "t1",
            before,
            after,
            Some("a1"),
        )
    }

    #[test]
    fn an_increase_is_logged_as_a_positive_direct_update_delta() {
        let e = entry(Some(1.0), 3.5).unwrap();
        assert_eq!(e.hours, 2.5);
        assert_eq!(e.source.as_deref(), Some(SOURCE_DIRECT_UPDATE));
        assert_eq!(e.agent_id.as_deref(), Some("a1"));
        assert!(e.note.unwrap().contains("increase"));
    }

    #[test]
    fn a_decrease_is_logged_as_a_negative_correction() {
        let e = entry(Some(5.0), 3.0).unwrap();
        assert_eq!(e.hours, -2.0);
        assert!(e.note.unwrap().contains("correction"));
    }

    #[test]
    fn the_delta_is_free_of_float_subtraction_noise() {
        // 4.1 - 1.5 is 2.5999999999999996 in raw f64 arithmetic.
        assert_eq!(entry(Some(1.5), 4.1).unwrap().hours, 2.6);
        assert_eq!(entry(Some(4.1), 1.5).unwrap().hours, -2.6);
    }

    #[test]
    fn an_unset_actual_hours_counts_as_zero() {
        assert_eq!(entry(None, 1.5).unwrap().hours, 1.5);
    }

    #[test]
    fn an_unchanged_value_logs_nothing() {
        assert!(entry(Some(2.0), 2.0).is_none());
        assert!(entry(None, 0.0).is_none());
        assert!(entry(Some(0.1 + 0.2), 0.3).is_none());
    }

    #[test]
    fn source_round_trips_and_is_omitted_for_a_log_time_entry() {
        let dir = tempfile::tempdir().unwrap();
        let plain = TimeLogEntry {
            ts: "2026-10-20T00:00:00+00:00".to_string(),
            task_id: "t1".to_string(),
            hours: 1.0,
            agent_id: None,
            note: None,
            source: None,
        };
        append_time_log(dir.path(), &plain).unwrap();
        assert!(record_direct_actual_hours_change(dir.path(), "t1", Some(1.0), 0.5, None).unwrap());

        let raw = std::fs::read_to_string(dir.path().join(TIME_LOG_FILE)).unwrap();
        let first = raw.lines().next().unwrap();
        assert!(!first.contains("source"), "{first}");

        let log = read_time_log(dir.path()).unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0], plain);
        assert_eq!(log[1].hours, -0.5);
        assert_eq!(log[1].source.as_deref(), Some(SOURCE_DIRECT_UPDATE));
    }

    #[test]
    fn a_legacy_line_without_source_still_parses() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(TIME_LOG_FILE),
            "{\"ts\":\"2026-10-20T00:00:00+00:00\",\"task_id\":\"t1\",\"hours\":2.0}\n",
        )
        .unwrap();
        let log = read_time_log(dir.path()).unwrap();
        assert_eq!(log[0].source, None);
    }
}
