//! Agent registration storage: `.handoff/agents/<agent-id>.json`.
//!
//! Tracks which agent processes are active across worktrees so multi-agent
//! sessions can tell which agent owns which lease/claim. See spec sections
//! 3.2 (`AgentRecord`) and 6.1 (heartbeat + GC).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A single agent's registration record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRecord {
    pub agent_id: String,
    pub session_id: Option<String>,
    pub worktree: PathBuf,
    pub branch: Option<String>,
    pub pid: Option<u32>,
    pub registered_at: DateTime<Utc>,
    pub last_heartbeat: DateTime<Utc>,
    pub status: AgentStatus,
    pub claimed_tasks: Vec<String>,
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Active,
    Stale,
    Disconnected,
}

/// Heartbeat age (seconds) after which an agent is considered [`AgentStatus::Stale`]
/// rather than [`AgentStatus::Active`]. Twice this age makes it [`AgentStatus::Disconnected`].
const HEARTBEAT_TTL_SECS: i64 = 1800; // 30 minutes

/// How long a [`AgentStatus::Disconnected`] record is kept before [`gc_agents`] deletes it,
/// counted from the moment it *became* disconnected (not from its last heartbeat).
const GC_RETENTION_DAYS: i64 = 7;

/// Minimum interval between [`update_heartbeat`] writes for the *same process*.
/// Callers (e.g. a tool handler invoked on every request) may call
/// `update_heartbeat` far more often than this; only one write per interval
/// actually hits disk.
const HEARTBEAT_DEBOUNCE_SECS: u64 = 60;

/// Derive a stable agent identifier.
///
/// Prefers `CLAUDE_SESSION_ID` (set by the Claude Code CLI) so an agent's
/// identity survives process restarts within the same CLI session. Falls
/// back to a locally-generated, filesystem-safe timestamp id.
pub fn generate_agent_id() -> String {
    if let Ok(session_id) = std::env::var("CLAUDE_SESSION_ID") {
        if !session_id.is_empty() {
            return session_id;
        }
    }
    let now = Utc::now();
    format!("a-{}", now.format("%Y%m%d-%H%M%S-%6f"))
}

/// Path to the `.handoff/agents/` directory.
pub fn agents_dir(handoff_dir: &Path) -> PathBuf {
    handoff_dir.join("agents")
}

/// Escape characters that are unsafe in a filename (matches the `agent_id`
/// possibly containing `:` or `/`, e.g. from external session id schemes)
/// without needing agent_id generation itself to be constrained.
fn safe_file_stem(agent_id: &str) -> String {
    agent_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn agent_path(handoff_dir: &Path, agent_id: &str) -> PathBuf {
    agents_dir(handoff_dir).join(format!("{}.json", safe_file_stem(agent_id)))
}

/// Persist `record` to `.handoff/agents/<agent-id>.json`, creating the
/// directory if needed. Uses [`crate::storage::atomic_write`] so a reader
/// never observes a partially-written record.
pub fn write_agent(handoff_dir: &Path, record: &AgentRecord) -> Result<()> {
    let dir = agents_dir(handoff_dir);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create agents dir: {}", dir.display()))?;

    let path = agent_path(handoff_dir, &record.agent_id);
    let content = serde_json::to_string_pretty(record).context("Failed to serialize agent")?;
    crate::storage::atomic_write(&path, content.as_bytes())
        .with_context(|| format!("Failed to write agent record: {}", path.display()))
}

/// Read a single agent record. Returns `Ok(None)` if the record (or the
/// `agents/` directory itself) does not exist.
///
/// `safe_file_stem` maps every non-alphanumeric character to `_`, so two
/// distinct ids (e.g. `"agent:1"` and `"agent_1"`) can collide on the same
/// filename. Post-read verification catches that: if the record on disk
/// belongs to a *different* `agent_id` than the one requested, this returns
/// `None` — the same outcome as a genuinely missing record — rather than
/// silently handing back the wrong agent's data.
pub fn read_agent(handoff_dir: &Path, agent_id: &str) -> Result<Option<AgentRecord>> {
    let path = agent_path(handoff_dir, agent_id);
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read agent record: {}", path.display()))?;
    let record: AgentRecord = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse agent record: {}", path.display()))?;
    if record.agent_id != agent_id {
        return Ok(None);
    }
    Ok(Some(record))
}

/// List every agent record under `.handoff/agents/`. Returns an empty vec if
/// the directory does not exist.
pub fn list_agents(handoff_dir: &Path) -> Result<Vec<AgentRecord>> {
    let dir = agents_dir(handoff_dir);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut records = Vec::new();
    for entry in std::fs::read_dir(&dir)
        .with_context(|| format!("Failed to read agents dir: {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read agent record: {}", path.display()))?;
        let record: AgentRecord = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse agent record: {}", path.display()))?;
        records.push(record);
    }
    Ok(records)
}

/// Delete an agent's record file. A no-op (not an error) if it does not exist.
pub fn delete_agent(handoff_dir: &Path, agent_id: &str) -> Result<()> {
    let path = agent_path(handoff_dir, agent_id);
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("Failed to delete agent record: {}", path.display()))?;
    }
    Ok(())
}

/// Derive the current [`AgentStatus`] for `record` as of `now`, based purely
/// on heartbeat age. This is a pure computation: callers decide whether/when
/// to persist the result (see [`update_heartbeat`] and [`gc_agents`]).
pub fn compute_status(record: &AgentRecord, now: DateTime<Utc>) -> AgentStatus {
    let elapsed = (now - record.last_heartbeat).num_seconds();
    if elapsed <= HEARTBEAT_TTL_SECS {
        AgentStatus::Active
    } else if elapsed <= HEARTBEAT_TTL_SECS * 2 {
        AgentStatus::Stale
    } else {
        AgentStatus::Disconnected
    }
}

/// Timestamp of the last successful (non-debounced) heartbeat write in this
/// process. Shared across all agent ids: the debounce is a per-process rate
/// limit on disk writes, not a per-agent one, matching the spec's intent of
/// bounding how often a single running agent touches its own record.
fn last_heartbeat_write() -> &'static Mutex<Option<Instant>> {
    static LOCK: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(None))
}

/// Refresh `agent_id`'s `last_heartbeat` to now and mark it [`AgentStatus::Active`],
/// debounced to at most once every [`HEARTBEAT_DEBOUNCE_SECS`] per process.
///
/// Returns `Ok(true)` if a write happened, `Ok(false)` if it was skipped
/// (debounced, or the agent record does not exist).
pub fn update_heartbeat(handoff_dir: &Path, agent_id: &str) -> Result<bool> {
    let mut last = last_heartbeat_write()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(t) = *last {
        if t.elapsed().as_secs() < HEARTBEAT_DEBOUNCE_SECS {
            return Ok(false);
        }
    }

    if let Some(mut record) = read_agent(handoff_dir, agent_id)? {
        record.last_heartbeat = Utc::now();
        record.status = AgentStatus::Active;
        write_agent(handoff_dir, &record)?;
        *last = Some(Instant::now());
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Delete [`AgentStatus::Disconnected`] records that have stayed disconnected
/// for at least [`GC_RETENTION_DAYS`], counted from the moment they crossed
/// into `Disconnected` (i.e. `last_heartbeat + 2 * HEARTBEAT_TTL_SECS`), not
/// from `last_heartbeat` itself. Returns the ids removed.
pub fn gc_agents(handoff_dir: &Path) -> Result<Vec<String>> {
    let now = Utc::now();
    let mut removed = Vec::new();
    for record in list_agents(handoff_dir)? {
        let status = compute_status(&record, now);
        if status == AgentStatus::Disconnected {
            let disconnected_since =
                record.last_heartbeat + chrono::Duration::seconds(HEARTBEAT_TTL_SECS * 2);
            if (now - disconnected_since).num_days() >= GC_RETENTION_DAYS {
                delete_agent(handoff_dir, &record.agent_id)?;
                removed.push(record.agent_id.clone());
            }
        }
    }
    Ok(removed)
}
