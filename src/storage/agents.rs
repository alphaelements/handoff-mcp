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
use fs2::FileExt;
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

/// Open (creating if absent) the `.lock` file next to an agent's record,
/// used as the cross-process `flock` handle guarding read-modify-write
/// updates to that single agent's `claimed_tasks`. Mirrors
/// `storage::tasks::open_lock_file`'s per-task-directory lock file, scoped
/// per-agent instead so two agents' claims never contend on the same lock.
fn open_agent_lock_file(handoff_dir: &Path, agent_id: &str) -> Result<std::fs::File> {
    let dir = agents_dir(handoff_dir);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create agents dir: {}", dir.display()))?;
    let lock_path = dir.join(format!("{}.lock", safe_file_stem(agent_id)));
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        // Never truncate: only the flock state matters, not the file's
        // content (see storage::tasks::open_lock_file for the same choice).
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("Failed to open lock file: {}", lock_path.display()))
}

/// Record that `agent_id` now holds a claim on `task_id`: appends `task_id`
/// to that agent's `claimed_tasks` (no-op if already present, so a retried
/// claim never double-counts). Guarded by a per-agent `flock` so concurrent
/// claims/releases by the same agent id across processes don't race on the
/// record's read-modify-write cycle (spec 7.1's cross-process protection,
/// applied here to agent records the same way `claim_task`/`release_task`
/// apply it to task records).
///
/// Returns `Ok(())` and leaves no trace if `agent_id` has no registered
/// record (e.g. the `UNKNOWN_IDENTITY` sentinel, or a caller that never
/// called `handoff_load_context`) — there is nothing to update, and this is
/// not a failure of the claim itself.
pub fn add_claimed_task(handoff_dir: &Path, agent_id: &str, task_id: &str) -> Result<()> {
    let lock_file = open_agent_lock_file(handoff_dir, agent_id)?;
    lock_file
        .lock_exclusive()
        .with_context(|| format!("Failed to acquire flock for agent {agent_id}"))?;

    let result = (|| -> Result<()> {
        if let Some(mut record) = read_agent(handoff_dir, agent_id)? {
            if !record.claimed_tasks.iter().any(|t| t == task_id) {
                record.claimed_tasks.push(task_id.to_string());
                write_agent(handoff_dir, &record)?;
            }
        }
        Ok(())
    })();

    let _ = fs2::FileExt::unlock(&lock_file);
    result
}

/// Record that `agent_id` no longer holds a claim on `task_id`: removes
/// `task_id` from that agent's `claimed_tasks` (no-op if absent). See
/// [`add_claimed_task`] for the locking and missing-record semantics, which
/// this mirrors.
pub fn remove_claimed_task(handoff_dir: &Path, agent_id: &str, task_id: &str) -> Result<()> {
    let lock_file = open_agent_lock_file(handoff_dir, agent_id)?;
    lock_file
        .lock_exclusive()
        .with_context(|| format!("Failed to acquire flock for agent {agent_id}"))?;

    let result = (|| -> Result<()> {
        if let Some(mut record) = read_agent(handoff_dir, agent_id)? {
            let before = record.claimed_tasks.len();
            record.claimed_tasks.retain(|t| t != task_id);
            if record.claimed_tasks.len() != before {
                write_agent(handoff_dir, &record)?;
            }
        }
        Ok(())
    })();

    let _ = fs2::FileExt::unlock(&lock_file);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_record(agent_id: &str) -> AgentRecord {
        AgentRecord {
            agent_id: agent_id.to_string(),
            session_id: Some("s-1".to_string()),
            worktree: PathBuf::from("/tmp/wt"),
            branch: None,
            pid: None,
            registered_at: Utc::now(),
            last_heartbeat: Utc::now(),
            status: AgentStatus::Active,
            claimed_tasks: Vec::new(),
            metadata: Default::default(),
        }
    }

    #[test]
    fn add_claimed_task_appends_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        write_agent(&handoff_dir, &test_record("agent-1")).unwrap();

        add_claimed_task(&handoff_dir, "agent-1", "t1").unwrap();
        add_claimed_task(&handoff_dir, "agent-1", "t1").unwrap(); // idempotent
        add_claimed_task(&handoff_dir, "agent-1", "t2").unwrap();

        let record = read_agent(&handoff_dir, "agent-1").unwrap().unwrap();
        assert_eq!(
            record.claimed_tasks,
            vec!["t1".to_string(), "t2".to_string()]
        );
    }

    #[test]
    fn add_claimed_task_is_noop_when_agent_record_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        // No agent record registered at all.
        add_claimed_task(&handoff_dir, "ghost-agent", "t1").unwrap();
        assert!(read_agent(&handoff_dir, "ghost-agent").unwrap().is_none());
    }

    #[test]
    fn remove_claimed_task_removes_only_matching_id() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        let mut record = test_record("agent-1");
        record.claimed_tasks = vec!["t1".to_string(), "t2".to_string()];
        write_agent(&handoff_dir, &record).unwrap();

        remove_claimed_task(&handoff_dir, "agent-1", "t1").unwrap();

        let record = read_agent(&handoff_dir, "agent-1").unwrap().unwrap();
        assert_eq!(record.claimed_tasks, vec!["t2".to_string()]);
    }

    #[test]
    fn remove_claimed_task_is_noop_when_task_not_present() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        write_agent(&handoff_dir, &test_record("agent-1")).unwrap();

        // Removing a task that was never claimed must not error.
        remove_claimed_task(&handoff_dir, "agent-1", "t-not-claimed").unwrap();

        let record = read_agent(&handoff_dir, "agent-1").unwrap().unwrap();
        assert!(record.claimed_tasks.is_empty());
    }
}
