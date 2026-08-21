use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

fn is_empty_map(m: &HashMap<String, Value>) -> bool {
    m.is_empty()
}

/// Fallback agent identity used by MCP handlers (`claim_release.rs`,
/// `update_task.rs`) when the calling process has never invoked
/// `handoff_load_context` and so carries no real `ctx.agent_id`. Claiming
/// under this sentinel is allowed (there is no prior owner to violate), but
/// it must never be treated as a real, comparably-equal identity for
/// ownership checks: two distinct unregistered callers both resolve to this
/// same literal string, so a plain `==` comparison would let one silently
/// release or claim ownership-sensitive state belonging to the other.
pub const UNKNOWN_IDENTITY: &str = "unknown";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskData {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
    /// Typed document/task/URL links (wiki/130-document-management.md §9.1).
    /// Additive alongside the legacy `links: Vec<String>` field, which is kept
    /// as-is for backward compatibility. Use the `links()` accessor to read a
    /// normalized, deduplicated view of both fields combined.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_links: Vec<TaskLink>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub done_criteria: Vec<DoneCriterion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// Cross-process claim lease (spec 3.3, 6.2, 6.3). Declared before `extra`
    /// (which is `#[serde(flatten)]`) so serde's flatten-loses-to-named-field
    /// precedence deserializes a `"lock"` JSON key into this typed field
    /// instead of letting it fall through into `extra` (spec 3.7.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock: Option<TaskLock>,
    #[serde(flatten, default, skip_serializing_if = "is_empty_map")]
    pub extra: HashMap<String, Value>,
}

/// A cross-process claim lease on a task, held by one agent at a time.
/// Guards against two agent processes (e.g. across worktrees) working the
/// same task concurrently. See spec 3.3 (`TaskLock`), 6.2 (claim), 6.3
/// (release), 7.1 (flock exclusion).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskLock {
    pub agent_id: String,
    pub session_id: String,
    pub claimed_at: String,
    pub lease_expires_at: String,
    pub lease_ttl_seconds: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Schedule {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate_hours: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_hours: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_hours: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milestone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoneCriterion {
    pub item: String,
    #[serde(default)]
    pub checked: bool,
}

/// A typed link from a task to a document, URL, file, or another task
/// (wiki/130-document-management.md §9.1). `link_type` distinguishes the
/// target kind: `"doc"` (document management fragment), `"url"`, `"file"`,
/// or `"task"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskLink {
    pub target: String,
    pub link_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl TaskData {
    /// Normalized, deduplicated view of every link on this task: the legacy
    /// `links: Vec<String>` field (each entry becomes `TaskLink { link_type:
    /// "file", label: None }`) merged with `task_links`. Dedupes by
    /// `(target, link_type)`, keeping the first occurrence — `task_links`
    /// entries are checked first so a richer (labeled) `task_links` entry
    /// wins over an equivalent bare legacy `links` entry.
    pub fn links(&self) -> Vec<TaskLink> {
        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut result = Vec::with_capacity(self.task_links.len() + self.links.len());

        for link in &self.task_links {
            let key = (link.target.clone(), link.link_type.clone());
            if seen.insert(key) {
                result.push(link.clone());
            }
        }
        for target in &self.links {
            let key = (target.clone(), "file".to_string());
            if seen.insert(key) {
                result.push(TaskLink {
                    target: target.clone(),
                    link_type: "file".to_string(),
                    label: None,
                });
            }
        }

        result
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskIndex {
    pub id: String,
    pub title: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<TaskIndex>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSummary {
    pub total: u32,
    pub by_status: std::collections::HashMap<String, u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_estimate_hours: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_actual_hours: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_rate: Option<f64>,
    #[serde(default)]
    pub overdue_count: u32,
}

const VALID_STATUSES: &[&str] = &[
    "todo",
    "in_progress",
    "review",
    "done",
    "blocked",
    "skipped",
];

const VALID_PRIORITIES: &[&str] = &["low", "medium", "high"];

pub fn is_valid_status(status: &str) -> bool {
    VALID_STATUSES.contains(&status)
}

pub fn is_valid_priority(priority: &str) -> bool {
    VALID_PRIORITIES.contains(&priority)
}

pub fn validate_priority(priority: Option<&str>) -> Result<()> {
    if let Some(p) = priority {
        if !is_valid_priority(p) {
            anyhow::bail!(
                "Invalid priority: '{p}'. Must be one of: {}",
                VALID_PRIORITIES.join(", ")
            );
        }
    }
    Ok(())
}

pub fn is_terminal_status(status: &str) -> bool {
    status == "done" || status == "skipped"
}

pub fn title_to_slug(title: &str) -> String {
    let slug: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else if !c.is_ascii() {
                c
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-').to_string();
    let mut result = String::new();
    let mut prev_dash = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_dash {
                result.push(c);
            }
            prev_dash = true;
        } else {
            result.push(c);
            prev_dash = false;
        }
    }
    if result.is_empty() {
        "task".to_string()
    } else {
        result
    }
}

pub fn find_task_file(task_dir: &Path) -> Result<Option<(PathBuf, String)>> {
    for entry in std::fs::read_dir(task_dir)
        .with_context(|| format!("Failed to read task dir: {}", task_dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(status) = parse_task_filename(&name) {
            return Ok(Some((entry.path(), status)));
        }
    }
    Ok(None)
}

fn parse_task_filename(name: &str) -> Option<String> {
    let name = name.strip_prefix("_task.")?;
    let status = name.strip_suffix(".json")?;
    if is_valid_status(status) {
        Some(status.to_string())
    } else {
        None
    }
}

pub fn read_task(task_dir: &Path) -> Result<Option<(TaskData, String)>> {
    let (file_path, status) = match find_task_file(task_dir)? {
        Some(v) => v,
        None => return Ok(None),
    };
    let content = std::fs::read_to_string(&file_path)
        .with_context(|| format!("Failed to read task: {}", file_path.display()))?;
    let data: TaskData = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse task: {}", file_path.display()))?;
    Ok(Some((data, status)))
}

pub fn write_task(task_dir: &Path, status: &str, data: &TaskData) -> Result<()> {
    let file_path = task_dir.join(format!("_task.{status}.json"));
    let content = serde_json::to_string_pretty(data).context("Failed to serialize task")?;
    crate::storage::atomic_write(&file_path, content.as_bytes())
        .with_context(|| format!("Failed to write task: {}", file_path.display()))?;
    Ok(())
}

/// Read-modify-write a task with optimistic concurrency control.
///
/// Reads the current task, runs `mutate` on a copy, then re-reads just before
/// writing: if the file's `updated_at` changed since the snapshot (another
/// writer — e.g. the VSCode extension — won the race), the whole cycle retries
/// up to `MAX_RETRIES` times. This matches the VSCode side's `updated_at`
/// protocol (wiki/95-concurrency-safety.md) and prevents lost updates that
/// atomic_write alone cannot (atomic_write stops *torn* reads, not *lost*
/// updates).
///
/// `mutate` receives the current `TaskData` and the resolved status, and returns
/// the new status the task should have after the change (usually unchanged).
pub fn read_modify_write_task<F>(task_dir: &Path, mut mutate: F) -> Result<()>
where
    F: FnMut(&mut TaskData, &str) -> Result<String>,
{
    const MAX_RETRIES: usize = 5;

    for attempt in 0..=MAX_RETRIES {
        let (mut data, status) = read_task(task_dir)?
            .ok_or_else(|| anyhow::anyhow!("Task file not found in {}", task_dir.display()))?;
        let snapshot_updated_at = data.updated_at.clone();

        let new_status = mutate(&mut data, &status)?;

        // Re-read to detect a concurrent writer before committing.
        let current_updated_at = read_task(task_dir)?.and_then(|(d, _)| d.updated_at);
        if current_updated_at != snapshot_updated_at {
            // Someone else wrote between our read and write. Retry from scratch.
            if attempt == MAX_RETRIES {
                anyhow::bail!(
                    "Concurrent modification of task in {} after {} retries; aborting to avoid \
                     overwriting another writer's changes.",
                    task_dir.display(),
                    MAX_RETRIES
                );
            }
            continue;
        }

        if new_status != status {
            change_status(task_dir, &new_status)?;
        }
        write_task(task_dir, &new_status, &data)?;
        return Ok(());
    }
    unreachable!("loop returns or bails within MAX_RETRIES iterations")
}

/// Open (creating if absent) the `.lock` file inside `task_dir` used as the
/// cross-process `flock` handle for that task. The task's own JSON file is
/// not used directly because its filename changes with status
/// (`_task.<status>.json`); the directory-scoped `.lock` file stays stable.
fn open_lock_file(task_dir: &Path) -> Result<std::fs::File> {
    let lock_path = task_dir.join(".lock");
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        // Never truncate: the lock file's content is unused (only its flock
        // state matters), so truncating would just be needless I/O — and
        // silences clippy::suspicious_open_options, which otherwise reads
        // `create(true)` without an explicit truncate policy as a possible
        // "did you mean to overwrite?" bug.
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("Failed to open lock file: {}", lock_path.display()))
}

/// Same as [`read_modify_write_task`], but additionally holds an exclusive
/// `flock` on the task directory's `.lock` file for the duration of the
/// read-modify-write cycle. `read_modify_write_task`'s OCC retry loop only
/// protects against concurrent writers *within this process*; `flock`
/// extends that protection across processes (e.g. two MCP server instances
/// in different worktrees), per spec 3.3/7.1.
pub fn read_modify_write_task_locked<F>(task_dir: &Path, mutate: F) -> Result<()>
where
    F: FnMut(&mut TaskData, &str) -> Result<String>,
{
    let lock_file = open_lock_file(task_dir)?;
    lock_file
        .lock_exclusive()
        .with_context(|| format!("Failed to acquire flock on {}", task_dir.display()))?;

    let result = read_modify_write_task(task_dir, mutate);

    let _ = fs2::FileExt::unlock(&lock_file);
    result
}

/// Claim a task for exclusive work by `agent_id`/`session_id`, guarded by a
/// cross-process `flock` (spec 6.2). Fails if the task already carries a
/// non-expired lock held by a different agent. An expired lock is silently
/// overwritten. A `todo`/`blocked` task transitions to `in_progress`; any
/// other status is left as-is (the lock alone marks ownership).
///
/// `handoff_dir` is the project's `.handoff/` directory (the parent of
/// `tasks/`), used only to append a `task.claimed` event to
/// `events.jsonl` (spec 3.6, 6.6) on success.
pub fn claim_task(
    task_dir: &Path,
    agent_id: &str,
    session_id: &str,
    lease_ttl: u64,
    handoff_dir: &Path,
) -> Result<TaskData> {
    let lock_file = open_lock_file(task_dir)?;
    lock_file
        .lock_exclusive()
        .with_context(|| format!("Failed to acquire flock on {}", task_dir.display()))?;

    let result = (|| -> Result<TaskData> {
        let (mut data, status) = read_task(task_dir)?
            .ok_or_else(|| anyhow::anyhow!("Task not found in {}", task_dir.display()))?;

        if let Some(ref existing_lock) = data.lock {
            let expires_at = chrono::DateTime::parse_from_rfc3339(&existing_lock.lease_expires_at)
                .map(|dt| dt.with_timezone(&Utc));
            if let Ok(expires_at) = expires_at {
                if Utc::now() < expires_at {
                    anyhow::bail!(
                        "Task {} is currently claimed by agent {} (session {}). Lease expires at {}.",
                        data.id,
                        existing_lock.agent_id,
                        existing_lock.session_id,
                        existing_lock.lease_expires_at
                    );
                }
                // Lease expired: fall through and overwrite with the new claim.
            }
        }

        let now = Utc::now();
        data.lock = Some(TaskLock {
            agent_id: agent_id.to_string(),
            session_id: session_id.to_string(),
            claimed_at: now.to_rfc3339(),
            lease_expires_at: (now + chrono::Duration::seconds(lease_ttl as i64)).to_rfc3339(),
            lease_ttl_seconds: lease_ttl,
        });
        data.updated_at = Some(now.to_rfc3339());

        let new_status = if status == "todo" || status == "blocked" {
            "in_progress".to_string()
        } else {
            status.clone()
        };

        if new_status != status {
            change_status(task_dir, &new_status)?;
        }
        write_task(task_dir, &new_status, &data)?;

        Ok(data)
    })();

    let _ = fs2::FileExt::unlock(&lock_file);

    if let Ok(ref data) = result {
        // Best-effort: a failure to log the event must not undo (or even
        // fail) an already-committed claim.
        let _ = crate::storage::events::append_event(
            handoff_dir,
            crate::storage::events::EventRecord {
                ts: Utc::now().to_rfc3339(),
                event: "task.claimed".to_string(),
                task_id: Some(data.id.clone()),
                agent_id: Some(agent_id.to_string()),
                session_id: Some(session_id.to_string()),
                detail: Some(format!("lease_ttl_seconds={lease_ttl}")),
            },
        );
    }

    result
}

/// Release a previously-claimed task, clearing its lock and reverting its
/// status to `revert_status` (spec 6.3). Fails (best-effort ownership check)
/// if the task's current lock is held by a different `agent_id` — the lock
/// is left untouched in that case.
///
/// `handoff_dir` is the project's `.handoff/` directory, used only to append
/// a `task.released` event to `events.jsonl` on success.
pub fn release_task(
    task_dir: &Path,
    agent_id: &str,
    revert_status: &str,
    handoff_dir: &Path,
) -> Result<()> {
    let lock_file = open_lock_file(task_dir)?;
    lock_file
        .lock_exclusive()
        .with_context(|| format!("Failed to acquire flock on {}", task_dir.display()))?;

    let result = (|| -> Result<String> {
        let (mut data, status) = read_task(task_dir)?
            .ok_or_else(|| anyhow::anyhow!("Task not found in {}", task_dir.display()))?;

        if let Some(ref lock) = data.lock {
            // An unresolved caller identity (the `UNKNOWN_IDENTITY` sentinel,
            // used when this process never called `handoff_load_context`)
            // must never be trusted as a real, comparably-equal owner: two
            // distinct unregistered callers both collapse to this same
            // literal string, so a plain `==` would let one silently release
            // a lock actually held by the other (or by itself under a
            // different, real identity). Reject unconditionally rather than
            // comparing.
            if agent_id == UNKNOWN_IDENTITY {
                anyhow::bail!(
                    "Task {} is claimed by agent {}. Cannot release with an unresolved \
                     caller identity ({UNKNOWN_IDENTITY}) — call handoff_load_context first \
                     so this process carries a real agent_id.",
                    data.id,
                    lock.agent_id,
                );
            }
            if lock.agent_id != agent_id {
                anyhow::bail!(
                    "Task {} is claimed by agent {}, not {}. Cannot release.",
                    data.id,
                    lock.agent_id,
                    agent_id
                );
            }
        }

        data.lock = None;
        data.updated_at = Some(Utc::now().to_rfc3339());

        if revert_status != status {
            if !is_valid_status(revert_status) {
                anyhow::bail!("Invalid status: {revert_status}");
            }
            change_status(task_dir, revert_status)?;
        }
        write_task(task_dir, revert_status, &data)?;

        Ok(data.id)
    })();

    let _ = fs2::FileExt::unlock(&lock_file);

    if let Ok(ref task_id) = result {
        let _ = crate::storage::events::append_event(
            handoff_dir,
            crate::storage::events::EventRecord {
                ts: Utc::now().to_rfc3339(),
                event: "task.released".to_string(),
                task_id: Some(task_id.clone()),
                agent_id: Some(agent_id.to_string()),
                session_id: None,
                detail: Some(format!("revert_status={revert_status}")),
            },
        );
    }

    result.map(|_| ())
}

/// Recursively scan `tasks_dir` for tasks whose `lock.lease_expires_at` has
/// passed, clear the lock, and revert the task to `todo` status (spec 3.3.5,
/// 7.2). Called "lazily" — only from a small allowlist of MCP handlers (spec
/// 7.2) rather than on a background timer — since this server has no
/// long-lived process to run one in.
///
/// Each expired task is guarded by the same per-task `flock` as
/// `claim_task`/`release_task`, so a concurrent claim racing the scan cannot
/// be silently clobbered: whichever wins the flock acquisition observes a
/// consistent lock state.
///
/// Returns the ids of every task whose lease was found expired and reverted,
/// and appends one `task.expired` event per task to `events.jsonl`.
pub fn scan_expired_leases(tasks_dir: &Path) -> Result<Vec<String>> {
    let handoff_dir = tasks_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| tasks_dir.to_path_buf());
    let mut expired_ids = Vec::new();
    scan_expired_leases_recursive(tasks_dir, &handoff_dir, &mut expired_ids)?;
    Ok(expired_ids)
}

fn scan_expired_leases_recursive(
    dir: &Path,
    handoff_dir: &Path,
    expired_ids: &mut Vec<String>,
) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name.starts_with('_') {
            continue;
        }
        let task_dir = entry.path();

        if let Some(task_id) = expire_lease_if_due(&task_dir, handoff_dir)? {
            expired_ids.push(task_id);
        }

        scan_expired_leases_recursive(&task_dir, handoff_dir, expired_ids)?;
    }
    Ok(())
}

/// Check a single task directory's lock and, if its lease has expired, clear
/// the lock and revert to `todo` under the same `flock` protocol as
/// `claim_task`/`release_task`. Returns the task id if it was expired (and
/// thus reverted), `None` otherwise (no task file, no lock, or lock not yet
/// expired).
fn expire_lease_if_due(task_dir: &Path, handoff_dir: &Path) -> Result<Option<String>> {
    // Cheap pre-check without the flock: most tasks have no lock at all, and
    // opening/locking a `.lock` file for every task on every lazy-scan-
    // triggering call would be wasteful.
    let Some((data, _status)) = read_task(task_dir)? else {
        return Ok(None);
    };
    let Some(ref lock) = data.lock else {
        return Ok(None);
    };
    let expires_at = match chrono::DateTime::parse_from_rfc3339(&lock.lease_expires_at) {
        Ok(dt) => dt.with_timezone(&Utc),
        // An unparseable expiry is treated as "not expired" rather than
        // guessed at — a scan should never destroy state it cannot make
        // sense of.
        Err(_) => return Ok(None),
    };
    if Utc::now() < expires_at {
        return Ok(None);
    }

    let lock_file = open_lock_file(task_dir)?;
    lock_file
        .lock_exclusive()
        .with_context(|| format!("Failed to acquire flock on {}", task_dir.display()))?;

    let result = (|| -> Result<Option<String>> {
        // Re-read under the flock: another process may have already claimed
        // or released this task since the pre-check above.
        let (mut data, status) = match read_task(task_dir)? {
            Some(v) => v,
            None => return Ok(None),
        };
        let Some(ref lock) = data.lock else {
            return Ok(None);
        };
        let expires_at = match chrono::DateTime::parse_from_rfc3339(&lock.lease_expires_at) {
            Ok(dt) => dt.with_timezone(&Utc),
            Err(_) => return Ok(None),
        };
        if Utc::now() < expires_at {
            return Ok(None);
        }

        let task_id = data.id.clone();
        data.lock = None;
        data.updated_at = Some(Utc::now().to_rfc3339());

        if status != "todo" {
            change_status(task_dir, "todo")?;
        }
        write_task(task_dir, "todo", &data)?;

        Ok(Some(task_id))
    })();

    let _ = fs2::FileExt::unlock(&lock_file);

    let outcome = result?;
    if let Some(ref task_id) = outcome {
        let _ = crate::storage::events::append_event(
            handoff_dir,
            crate::storage::events::EventRecord {
                ts: Utc::now().to_rfc3339(),
                event: "task.expired".to_string(),
                task_id: Some(task_id.clone()),
                agent_id: None,
                session_id: None,
                detail: Some("lease expired; reverted to todo".to_string()),
            },
        );
    }
    Ok(outcome)
}

/// Resolve a task id (e.g. `"t1.2"`) to its on-disk directory. Thin wrapper
/// over [`find_task_dir_by_id`] that returns a not-found error instead of
/// `Ok(None)`, matching the shape MCP handlers want (a `?`-able `Result`
/// rather than an extra `Option` unwrap at every call site).
pub fn find_task_dir(tasks_dir: &Path, task_id: &str) -> Result<PathBuf> {
    find_task_dir_by_id(tasks_dir, task_id)?
        .ok_or_else(|| anyhow::anyhow!("{}", suggest_task_id(tasks_dir, task_id)))
}

pub fn change_status(task_dir: &Path, new_status: &str) -> Result<()> {
    if !is_valid_status(new_status) {
        anyhow::bail!("Invalid status: {new_status}");
    }

    let (old_path, old_status) = find_task_file(task_dir)?
        .ok_or_else(|| anyhow::anyhow!("No task file found in {}", task_dir.display()))?;

    if old_status == new_status {
        return Ok(());
    }

    let new_path = task_dir.join(format!("_task.{new_status}.json"));
    std::fs::rename(&old_path, &new_path).with_context(|| {
        format!(
            "Failed to rename {} -> {}",
            old_path.display(),
            new_path.display()
        )
    })?;

    Ok(())
}

pub fn next_child_id(parent_dir: &Path, parent_id: &str) -> Result<String> {
    let mut max_n: u32 = 0;

    if parent_dir.exists() {
        for entry in std::fs::read_dir(parent_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(n) = extract_child_number(&name, parent_id) {
                max_n = max_n.max(n);
            }
        }
    }

    Ok(format!("{parent_id}.{}", max_n + 1))
}

pub fn next_top_level_id(tasks_dir: &Path) -> Result<String> {
    let mut max_n: u32 = 0;

    if tasks_dir.exists() {
        for entry in std::fs::read_dir(tasks_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(n) = extract_top_level_number(&name) {
                max_n = max_n.max(n);
            }
        }
    }

    Ok(format!("t{}", max_n + 1))
}

fn extract_child_number(dir_name: &str, parent_id: &str) -> Option<u32> {
    let prefix = format!("{parent_id}.");
    let rest = dir_name.strip_prefix(&prefix)?;
    let num_part = rest.split('-').next()?;
    num_part.parse().ok()
}

fn extract_top_level_number(dir_name: &str) -> Option<u32> {
    let rest = dir_name.strip_prefix('t')?;
    let num_part = rest.split('-').next()?;
    if num_part.contains('.') {
        return None;
    }
    num_part.parse().ok()
}

pub fn find_task_dir_by_id(tasks_dir: &Path, task_id: &str) -> Result<Option<PathBuf>> {
    find_task_dir_recursive(tasks_dir, task_id)
}

fn dir_name_could_match(dir_name: &str, task_id: &str) -> bool {
    dir_name == task_id
        || (dir_name.starts_with(task_id) && dir_name.as_bytes().get(task_id.len()) == Some(&b'-'))
}

fn find_task_dir_recursive(dir: &Path, task_id: &str) -> Result<Option<PathBuf>> {
    if !dir.exists() {
        return Ok(None);
    }
    let mut candidates = Vec::new();
    let mut other_subdirs = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if dir_name_could_match(&name, task_id) {
            candidates.push(entry.path());
        } else {
            other_subdirs.push(entry.path());
        }
    }
    // Verify candidates by reading the JSON id field.
    for candidate in &candidates {
        if let Some((data, _)) = read_task(candidate)? {
            if data.id == task_id {
                return Ok(Some(candidate.clone()));
            }
        }
    }
    // Recurse into all subdirectories (candidates that didn't match + others).
    for subdir in candidates.into_iter().chain(other_subdirs) {
        if let Some(found) = find_task_dir_recursive(&subdir, task_id)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

fn collect_all_ids_recursive(dir: &Path, ids: &mut Vec<String>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        if let Some((data, _)) = read_task(&entry.path())? {
            ids.push(data.id);
        }
        collect_all_ids_recursive(&entry.path(), ids)?;
    }
    Ok(())
}

pub fn suggest_task_id(tasks_dir: &Path, requested_id: &str) -> String {
    let mut all_ids = Vec::new();
    let _ = collect_all_ids_recursive(tasks_dir, &mut all_ids);
    if all_ids.is_empty() {
        return format!("Task not found: '{requested_id}'. No tasks exist yet.");
    }
    let mut scored: Vec<(&str, usize)> = all_ids
        .iter()
        .filter_map(|id| {
            let score = fuzzy_score(requested_id, id);
            if score > 0 {
                Some((id.as_str(), score))
            } else {
                None
            }
        })
        .collect();
    scored.sort_by_key(|&(_, s)| Reverse(s));
    scored.truncate(5);

    if scored.is_empty() {
        return format!(
            "Task not found: '{requested_id}'. Use handoff_list_tasks to see available task IDs."
        );
    }
    let suggestions: Vec<String> = scored.iter().map(|(id, _)| format!("  - {id}")).collect();
    format!(
        "Task not found: '{requested_id}'. Did you mean one of these?\n{}\n\
         Use handoff_list_tasks to see all task IDs.",
        suggestions.join("\n")
    )
}

fn fuzzy_score(query: &str, candidate: &str) -> usize {
    let q = query.to_lowercase();
    let c = candidate.to_lowercase();
    if c == q {
        return 100;
    }
    if c.starts_with(&q) || q.starts_with(&c) {
        return 80;
    }
    if c.contains(&q) || q.contains(&c) {
        return 60;
    }
    let q_parts: Vec<&str> = q.split(['-', '.', '_']).collect();
    let c_parts: Vec<&str> = c.split(['-', '.', '_']).collect();
    let matching = q_parts.iter().filter(|p| c_parts.contains(p)).count();
    if matching > 0 {
        return 20 * matching;
    }
    0
}

pub fn build_task_index(
    tasks_dir: &Path,
    done_task_limit: u32,
) -> Result<(Vec<TaskIndex>, TaskSummary)> {
    let mut tree = Vec::new();
    let mut summary = TaskSummary {
        total: 0,
        by_status: HashMap::new(),
        total_estimate_hours: None,
        total_actual_hours: None,
        completion_rate: None,
        overdue_count: 0,
    };
    let mut done_count: u32 = 0;
    let mut estimate_sum: f64 = 0.0;
    let mut actual_sum: f64 = 0.0;
    let mut has_hours = false;
    let today = Utc::now().format("%Y-%m-%d").to_string();

    build_index_recursive(
        tasks_dir,
        &mut tree,
        &mut summary,
        &mut done_count,
        done_task_limit,
        &mut estimate_sum,
        &mut actual_sum,
        &mut has_hours,
        &today,
    )?;

    if has_hours {
        summary.total_estimate_hours = Some(estimate_sum);
        summary.total_actual_hours = Some(actual_sum);
    }

    if summary.total > 0 {
        let done = *summary.by_status.get("done").unwrap_or(&0) as f64;
        let skipped = *summary.by_status.get("skipped").unwrap_or(&0) as f64;
        summary.completion_rate = Some((done + skipped) / summary.total as f64);
    }

    Ok((tree, summary))
}

#[allow(clippy::too_many_arguments)]
fn build_index_recursive(
    dir: &Path,
    tree: &mut Vec<TaskIndex>,
    summary: &mut TaskSummary,
    done_count: &mut u32,
    done_task_limit: u32,
    estimate_sum: &mut f64,
    actual_sum: &mut f64,
    has_hours: &mut bool,
    today: &str,
) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }

    let mut entries: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }

        let task_dir = entry.path();
        let (data, status) = match read_task(&task_dir)? {
            Some(v) => v,
            None => continue,
        };

        summary.total += 1;
        *summary.by_status.entry(status.clone()).or_insert(0) += 1;

        if let Some(ref sched) = data.schedule {
            if let Some(est) = sched.estimate_hours {
                *estimate_sum += est;
                *has_hours = true;
            }
            if let Some(act) = sched.actual_hours {
                *actual_sum += act;
                *has_hours = true;
            }
            if let Some(ref due) = sched.due_date {
                if !is_terminal_status(&status) && due.as_str() < today {
                    summary.overdue_count += 1;
                }
            }
        }

        if is_terminal_status(&status) {
            *done_count += 1;
            if *done_count > done_task_limit {
                continue;
            }
        }

        let mut children = Vec::new();
        build_index_recursive(
            &task_dir,
            &mut children,
            summary,
            done_count,
            done_task_limit,
            estimate_sum,
            actual_sum,
            has_hours,
            today,
        )?;

        tree.push(TaskIndex {
            id: data.id,
            title: data.title,
            status,
            schedule: data.schedule,
            dependencies: data.dependencies,
            order: data.order,
            assignee: data.assignee,
            children,
        });
    }

    Ok(())
}

pub fn validate_dependencies(tasks_dir: &Path, task_id: &str, new_deps: &[String]) -> Result<()> {
    let dep_graph = build_dependency_graph(tasks_dir)?;

    let mut graph = dep_graph;
    graph.insert(task_id.to_string(), new_deps.to_vec());

    let mut visited = HashSet::new();
    let mut stack = HashSet::new();

    if has_cycle(&graph, task_id, &mut visited, &mut stack) {
        anyhow::bail!(
            "Circular dependency detected: setting dependencies {:?} on task {task_id} would create a cycle",
            new_deps
        );
    }

    Ok(())
}

/// Validate dependencies for a whole batch of tasks that do not exist yet.
///
/// `validate_dependencies` adds one node to the on-disk graph, which is all an
/// update needs. An import creates a tree in one call, so its tasks may depend on
/// each other: checking them one at a time would reject a legitimate dependency
/// on a sibling (not yet written) and miss a cycle that lives entirely inside the
/// batch (neither end written). Merge every pending node in first, then search.
///
/// A dependency naming a task that exists nowhere is left alone — it contributes
/// no edge, and `validate_dependencies` tolerates it too.
pub fn validate_dependencies_batch(
    tasks_dir: &Path,
    pending: &[(String, Vec<String>)],
) -> Result<()> {
    let mut graph = build_dependency_graph(tasks_dir)?;
    for (id, deps) in pending {
        graph.insert(id.clone(), deps.clone());
    }

    for (id, deps) in pending {
        let mut visited = HashSet::new();
        let mut stack = HashSet::new();
        if has_cycle(&graph, id, &mut visited, &mut stack) {
            anyhow::bail!(
                "Circular dependency detected: setting dependencies {deps:?} on task {id} would create a cycle"
            );
        }
    }

    Ok(())
}

fn build_dependency_graph(tasks_dir: &Path) -> Result<HashMap<String, Vec<String>>> {
    let mut graph = HashMap::new();
    build_dep_graph_recursive(tasks_dir, &mut graph)?;
    Ok(graph)
}

/// One task that lists `task_id` in its own `dependencies` — a "downstream"
/// task whose scope may be where an apparently-missing piece of `task_id` is
/// actually meant to land.
///
/// Carries `notes` and `done_criteria` (not just id/title/status) so a
/// reviewer or tester can judge whether this dependent actually documents the
/// deferred connection in a single `handoff_get_task` call, rather than
/// having to call `handoff_get_task` again per dependent for the fields that
/// matter to that judgment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependentTask {
    pub id: String,
    pub title: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub done_criteria: Vec<DoneCriterion>,
}

/// Find every task that depends on `task_id`, across the whole project.
///
/// `TaskData.dependencies` only records the forward edge (what a task waits
/// on); nothing on disk records the reverse edge (what waits on a task). A
/// reviewer checking whether an apparently-unwired piece of `task_id` is
/// actually intentional — deferred to a later task in the same breakdown —
/// has no way to find that later task without scanning every task in the
/// project by hand. This is that scan, exposed once so `handoff_get_task` can
/// answer it in one call.
pub fn find_dependents(tasks_dir: &Path, task_id: &str) -> Result<Vec<DependentTask>> {
    let mut all = Vec::new();
    collect_all_tasks(tasks_dir, &mut all)?;

    let mut dependents: Vec<DependentTask> = all
        .into_iter()
        .filter(|(data, _status)| data.dependencies.iter().any(|d| d == task_id))
        .map(|(data, status)| DependentTask {
            id: data.id,
            title: data.title,
            status,
            notes: data.notes,
            done_criteria: data.done_criteria,
        })
        .collect();

    dependents.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(dependents)
}

pub(crate) fn collect_all_tasks(dir: &Path, out: &mut Vec<(TaskData, String)>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name.starts_with('_') {
            continue;
        }
        let task_dir = entry.path();
        if let Some((data, status)) = read_task(&task_dir)? {
            collect_all_tasks(&task_dir, out)?;
            out.push((data, status));
        }
    }
    Ok(())
}

fn build_dep_graph_recursive(dir: &Path, graph: &mut HashMap<String, Vec<String>>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name.starts_with('_') {
            continue;
        }
        let task_dir = entry.path();
        if let Some((data, _)) = read_task(&task_dir)? {
            graph.insert(data.id.clone(), data.dependencies.clone());
            build_dep_graph_recursive(&task_dir, graph)?;
        }
    }
    Ok(())
}

fn has_cycle(
    graph: &HashMap<String, Vec<String>>,
    node: &str,
    visited: &mut HashSet<String>,
    stack: &mut HashSet<String>,
) -> bool {
    if stack.contains(node) {
        return true;
    }
    if visited.contains(node) {
        return false;
    }
    visited.insert(node.to_string());
    stack.insert(node.to_string());

    if let Some(deps) = graph.get(node) {
        for dep in deps {
            if has_cycle(graph, dep, visited, stack) {
                return true;
            }
        }
    }

    stack.remove(node);
    false
}

pub fn validate_done_transition(task_dir: &Path, data: &TaskData) -> Result<()> {
    for criterion in &data.done_criteria {
        if !criterion.checked {
            anyhow::bail!(
                "Cannot mark task {} as done: done_criteria item '{}' is not checked",
                data.id,
                criterion.item
            );
        }
    }

    check_children_terminal(task_dir, &data.id)?;

    Ok(())
}

pub fn validate_skipped_transition(task_dir: &Path, data: &TaskData) -> Result<()> {
    check_children_terminal(task_dir, &data.id)?;
    Ok(())
}

/// Returns true if the task directory contains at least one child task
/// (a non-`_`/`.`-prefixed subdirectory holding a task file).
pub fn task_has_children(task_dir: &Path) -> Result<bool> {
    if !task_dir.exists() {
        return Ok(false);
    }
    for entry in std::fs::read_dir(task_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('_') || name.starts_with('.') {
            continue;
        }
        if find_task_file(&entry.path())?.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether a task in the given status requires an effort estimate.
/// Parent tasks (with children) and blocked/skipped/todo tasks are exempt;
/// this only covers the status dimension. `todo` is exempt so that parent
/// tasks can be created before their children exist (the children turn the
/// parent from a leaf into a parent, removing the estimate requirement).
/// The estimate is enforced when the task moves to `in_progress` or later.
pub fn status_requires_estimate(status: &str) -> bool {
    matches!(status, "in_progress" | "review" | "done")
}

/// Validate that a leaf task carries an `estimate_hours` when the project
/// requires it. `has_children` lets the caller skip parent tasks.
///
/// The rejection names the offending task and shows a ready-to-send payload,
/// so a caller that forgot the estimate can fix it in one retry instead of
/// discovering the required shape by trial and error. `is_create` tailors that
/// payload: creating a task also needs `title`, whereas an update must not
/// imply the stored title should be overwritten.
pub fn validate_estimate_required(
    require_estimate_hours: bool,
    id: &str,
    title: &str,
    status: &str,
    has_children: bool,
    is_create: bool,
    schedule: Option<&Schedule>,
) -> Result<()> {
    if !require_estimate_hours || has_children || !status_requires_estimate(status) {
        return Ok(());
    }
    let has_estimate = schedule
        .and_then(|s| s.estimate_hours)
        .is_some_and(|h| h > 0.0);
    if !has_estimate {
        // Mirror the rejected call, so the example can be resent as-is. Build it
        // with serde_json rather than string formatting: a title carrying a quote,
        // a backslash, or a control character must still serialize to valid JSON.
        // A create also needs `title`; an update must not imply overwriting it.
        let mut example = serde_json::Map::new();
        example.insert("id".into(), Value::String(id.to_string()));
        if is_create {
            example.insert("title".into(), Value::String(title.to_string()));
        }
        example.insert(
            "schedule".into(),
            serde_json::json!({ "estimate_hours": 2.0 }),
        );
        let example = Value::Object(example);
        anyhow::bail!(
            "Task '{id}' ({title}) requires an effort estimate: \
             schedule.estimate_hours (hours, > 0) is mandatory for a leaf task \
             in status '{status}'.\n\
             Estimate the raw human-effort hours — do not pre-multiply by the \
             AI-effort multiplier; that is applied at aggregation time.\n\
             Resend with, for example:\n  \
             {example}\n\
             Exempt from this rule: parent tasks (any task with children), and \
             tasks in status 'todo', 'blocked', or 'skipped'.\n\
             To disable this requirement project-wide, set \
             settings.require_estimate_hours = false."
        );
    }
    Ok(())
}

fn check_children_terminal(task_dir: &Path, parent_id: &str) -> Result<()> {
    if !task_dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(task_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('_') || name.starts_with('.') {
            continue;
        }
        if let Some((_, status)) = find_task_file(&entry.path())? {
            if !is_terminal_status(&status) {
                anyhow::bail!(
                    "Cannot mark task {parent_id} as done/skipped: child task in directory '{}' has status '{status}'",
                    name
                );
            }
        }
    }
    Ok(())
}

/// Result of [`sync_doc_task_links`]: reports which of the requested task ids
/// could not be resolved to an existing task, so the caller (e.g. `doc_save`)
/// can surface a warning to the user instead of the sync silently doing
/// nothing for a mistyped id.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SyncReport {
    /// Task ids passed in `link_task_ids` or `unlink_task_ids` that did not
    /// resolve to an existing task directory. Order-preserved, may contain
    /// ids from either list combined.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<String>,
}

/// Synchronize the task side of a task<->document bidirectional link
/// (wiki/130-document-management.md §9.2). Intended to be called by the
/// `handoff_doc_save` handler (t96) after it has written its own doc-side
/// `task_ids` field, so the two stay in agreement.
///
/// For every task id in `link_task_ids`, push-if-absent a
/// `TaskLink { target: doc_id, link_type: "doc", label: Some(doc_title) }`
/// into that task's `task_links` (deduped by `(target, link_type)`).
/// For every task id in `unlink_task_ids`, remove any `task_links` entry
/// with `target == doc_id && link_type == "doc"`.
///
/// A task id that does not resolve to an existing task directory is not
/// treated as a hard error: the caller (doc_save) may be syncing a batch of
/// task_ids where one was mistyped, and a document write should not be
/// rolled back over an unrelated task lookup failure. Instead it is
/// collected into the returned [`SyncReport::unresolved`] list so the caller
/// can report it back to the user (partial success + report, not a silent
/// skip).
pub fn sync_doc_task_links(
    tasks_dir: &Path,
    doc_id: &str,
    doc_title: &str,
    link_task_ids: &[String],
    unlink_task_ids: &[String],
) -> Result<SyncReport> {
    let mut unresolved = Vec::new();

    for task_id in link_task_ids {
        let Some(task_dir) = find_task_dir_by_id(tasks_dir, task_id)? else {
            unresolved.push(task_id.clone());
            continue;
        };
        read_modify_write_task(&task_dir, |data, status| {
            let already_linked = data
                .task_links
                .iter()
                .any(|l| l.target == doc_id && l.link_type == "doc");
            if !already_linked {
                data.task_links.push(TaskLink {
                    target: doc_id.to_string(),
                    link_type: "doc".to_string(),
                    label: Some(doc_title.to_string()),
                });
                data.updated_at = Some(Utc::now().to_rfc3339());
            }
            Ok(status.to_string())
        })?;
    }

    for task_id in unlink_task_ids {
        let Some(task_dir) = find_task_dir_by_id(tasks_dir, task_id)? else {
            unresolved.push(task_id.clone());
            continue;
        };
        read_modify_write_task(&task_dir, |data, status| {
            let before = data.task_links.len();
            data.task_links
                .retain(|l| !(l.target == doc_id && l.link_type == "doc"));
            if data.task_links.len() != before {
                data.updated_at = Some(Utc::now().to_rfc3339());
            }
            Ok(status.to_string())
        })?;
    }

    Ok(SyncReport { unresolved })
}
