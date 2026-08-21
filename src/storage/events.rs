//! Minimal append-only event log (spec 3.6, 6.6).
//!
//! Phase 1 records a small set of lease lifecycle events (`task.claimed`,
//! `task.released`, `task.expired`) to `.handoff/events.jsonl` in JSON Lines
//! format: one compact JSON object per line, newline-terminated. Unlike
//! task/session/config files, this log is append-only and never
//! read-modify-written, so it does not go through [`crate::storage::atomic_write`]
//! — a plain `O_APPEND` write is both sufficient and avoids the temp-file +
//! rename overhead for what is expected to be a high-frequency, small write.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One line of `.handoff/events.jsonl`. `task_id`/`agent_id`/`session_id`/
/// `detail` are optional so a future event kind that doesn't need one of
/// them (e.g. a project-level event with no `task_id`) doesn't have to
/// invent a placeholder value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRecord {
    pub ts: String,
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Append one `EventRecord` to `<handoff_dir>/events.jsonl`, creating the
/// file if absent. Best-effort ordering only relies on single-writer-at-a-time
/// append semantics of `O_APPEND`; concurrent writers may interleave, but each
/// individual `write_all` call here is a single `serde_json::to_string` line
/// plus a trailing `\n`, kept small enough that POSIX/Windows append writes
/// are not torn in practice for this log's purposes.
pub fn append_event(handoff_dir: &Path, event: EventRecord) -> Result<()> {
    use std::io::Write;
    let path = handoff_dir.join("events.jsonl");
    let line = serde_json::to_string(&event).context("Failed to serialize event")? + "\n";
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("Failed to open events log: {}", path.display()))?;
    file.write_all(line.as_bytes())
        .with_context(|| format!("Failed to write events log: {}", path.display()))?;
    Ok(())
}
