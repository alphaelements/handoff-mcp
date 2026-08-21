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

/// Default number of events [`read_events`] returns when `limit` is unset,
/// matching the `handoff_events` tool's documented default.
pub const DEFAULT_EVENT_LIMIT: usize = 100;

/// Filter criteria for [`read_events`]. All fields are optional; an unset
/// field imposes no constraint. `Default` (all `None`) returns every event
/// up to [`DEFAULT_EVENT_LIMIT`].
#[derive(Debug, Clone, Default)]
pub struct EventFilters {
    /// Only events with `ts >= since` (ISO 8601 string comparison, which is
    /// lexicographically correct for the RFC 3339 `Z`-suffixed timestamps
    /// this log always writes).
    pub since: Option<String>,
    pub task_id: Option<String>,
    pub agent_id: Option<String>,
    pub event_type: Option<String>,
    /// Maximum number of events to return, keeping the *most recent* ones.
    /// Defaults to [`DEFAULT_EVENT_LIMIT`] when `None`.
    pub limit: Option<usize>,
}

/// Read `<handoff_dir>/events.jsonl`, apply `filters`, and return the
/// matching records in original (chronological append) order.
///
/// Reads the whole file into memory and filters in Rust rather than doing
/// anything smarter (index, streaming, reverse-seek): `events.jsonl` is
/// expected to stay small for the lifetime of a project (see module doc),
/// so this trades a bit of memory for simplicity.
///
/// Missing file returns an empty vec (not an error) — a fresh project with
/// no lease/session activity yet is a normal, not exceptional, state.
///
/// Each line is parsed independently; a malformed line (partial write,
/// corruption, or content this build's `EventRecord` cannot deserialize) is
/// skipped rather than failing the whole read, so one bad line never hides
/// every event around it. This also gives forward/backward compatibility:
/// a line from an older or newer `EventRecord` shape that still has the
/// fields this build knows about parses fine (unknown extra fields are
/// ignored by serde by default); only a line that's not valid JSON at all,
/// or missing a required field (`ts`/`event`), is dropped.
///
/// `limit` (default [`DEFAULT_EVENT_LIMIT`]) keeps the most recent N
/// *matching* events, applied after all other filters, and the result is
/// returned oldest-first (i.e. still chronological) so callers reading the
/// output top-to-bottom see events in the order they happened.
pub fn read_events(handoff_dir: &Path, filters: &EventFilters) -> Result<Vec<EventRecord>> {
    let path = handoff_dir.join("events.jsonl");
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read events log: {}", path.display()))?;

    let mut events: Vec<EventRecord> = content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<EventRecord>(line).ok())
        .filter(|e| {
            filters
                .since
                .as_deref()
                .is_none_or(|since| e.ts.as_str() >= since)
        })
        .filter(|e| {
            filters
                .task_id
                .as_deref()
                .is_none_or(|id| e.task_id.as_deref() == Some(id))
        })
        .filter(|e| {
            filters
                .agent_id
                .as_deref()
                .is_none_or(|id| e.agent_id.as_deref() == Some(id))
        })
        .filter(|e| filters.event_type.as_deref().is_none_or(|ty| e.event == ty))
        .collect();

    let limit = filters.limit.unwrap_or(DEFAULT_EVENT_LIMIT);
    if events.len() > limit {
        events = events.split_off(events.len() - limit);
    }

    Ok(events)
}
