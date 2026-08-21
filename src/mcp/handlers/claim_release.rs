//! `handoff_claim_task` / `handoff_release_task` / `handoff_reclaim_task`
//! handlers (spec 6.2, 6.3; multi-WT spec FR-2.3).
//!
//! Wraps [`crate::storage::tasks::claim_task`] and
//! [`crate::storage::tasks::release_task`], which do the actual flock-guarded
//! read-modify-write. This module only resolves arguments and the task
//! directory, and formats the result for the tool-call response.
//! `handle_reclaim` performs its own flock-guarded read-modify-write inline
//! (see its doc comment) rather than through `release_task`, since it
//! intentionally skips the ownership check `release_task` enforces.

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::Value;

use super::HandlerContext;
use crate::storage::tasks::UNKNOWN_IDENTITY;

/// Default lease TTL (seconds) applied when `lease_ttl` is omitted: 30 minutes.
const DEFAULT_LEASE_TTL_SECONDS: u64 = 1800;

// `UNKNOWN_IDENTITY` (fallback agent/session identity used when the caller
// does not yet carry one) is defined in `storage::tasks` and re-exported
// here so ownership-check code (`release_task`) and this handler agree on
// the exact sentinel value. `ctx.agent_id` is `None` until this process has
// called `handoff_load_context` (see `crate::mcp::router::set_agent_id`);
// `session_id` has no equivalent context field at all yet, so it is always
// taken from `arguments` with this same fallback.

pub fn handle_claim(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let tasks_dir = ctx.handoff_dir.join("tasks");
    // Lazy scan (spec 3.3.5, 7.2): reclaim any expired leases before this
    // claim attempt reads lock state, so a stale expired lock never blocks
    // (or is raced against) a fresh claim.
    let _ = crate::storage::tasks::scan_expired_leases(&tasks_dir);

    let task_id = arguments
        .get("task_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'task_id' is required"))?;
    let lease_ttl = arguments
        .get("lease_ttl")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_LEASE_TTL_SECONDS);

    let agent_id = ctx.agent_id.as_deref().unwrap_or(UNKNOWN_IDENTITY);
    let session_id = arguments
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or(UNKNOWN_IDENTITY);

    let task_dir = crate::storage::tasks::find_task_dir(&tasks_dir, task_id)?;

    let data = crate::storage::tasks::claim_task(
        &task_dir,
        agent_id,
        session_id,
        lease_ttl,
        &ctx.handoff_dir,
    )?;

    serde_json::to_string_pretty(&data).map_err(Into::into)
}

pub fn handle_release(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let tasks_dir = ctx.handoff_dir.join("tasks");
    // Lazy scan (spec 3.3.5, 7.2): see handle_claim.
    let _ = crate::storage::tasks::scan_expired_leases(&tasks_dir);

    let task_id = arguments
        .get("task_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'task_id' is required"))?;
    let reason = arguments.get("reason").and_then(|v| v.as_str());
    let revert_status = arguments
        .get("revert_status")
        .and_then(|v| v.as_str())
        .unwrap_or("todo");

    if !crate::storage::tasks::is_valid_status(revert_status) {
        anyhow::bail!("Invalid status: {revert_status}");
    }

    let agent_id = ctx.agent_id.as_deref().unwrap_or(UNKNOWN_IDENTITY);

    let task_dir = crate::storage::tasks::find_task_dir(&tasks_dir, task_id)?;

    crate::storage::tasks::release_task(&task_dir, agent_id, revert_status, &ctx.handoff_dir)?;

    let mut msg = format!("Task {task_id} released successfully.");
    if let Some(r) = reason {
        msg.push_str(&format!(" Reason: {r}"));
    }
    Ok(msg)
}

/// `handoff_reclaim_task` handler (multi-WT spec FR-2.3): forcibly clears a
/// task's lease regardless of which agent holds it or whether it has
/// expired. Unlike [`handle_release`], this performs no ownership check —
/// it is a management operation for recovering a task whose owning agent
/// crashed, disconnected, or is otherwise stuck, without waiting out the
/// lease TTL.
pub fn handle_reclaim(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let tasks_dir = ctx.handoff_dir.join("tasks");

    let task_id = arguments
        .get("task_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'task_id' is required"))?;
    let reason = arguments.get("reason").and_then(|v| v.as_str());

    let reclaimer_agent_id = ctx.agent_id.as_deref().unwrap_or(UNKNOWN_IDENTITY);

    let task_dir = crate::storage::tasks::find_task_dir(&tasks_dir, task_id)?;

    let lock_file = crate::storage::tasks::open_lock_file(&task_dir)?;
    fs2::FileExt::lock_exclusive(&lock_file)
        .with_context(|| format!("Failed to acquire flock on {}", task_dir.display()))?;

    let result = (|| -> Result<Option<String>> {
        let (mut data, status) = crate::storage::tasks::read_task(&task_dir)?
            .ok_or_else(|| anyhow::anyhow!("Task not found in {}", task_dir.display()))?;

        let previous_owner = match &data.lock {
            Some(lock) => lock.agent_id.clone(),
            None => anyhow::bail!("Task {task_id} is not locked (no active claim to reclaim)."),
        };

        data.lock = None;
        data.updated_at = Some(Utc::now().to_rfc3339());

        if status != "todo" {
            crate::storage::tasks::change_status(&task_dir, "todo")?;
        }
        crate::storage::tasks::write_task(&task_dir, "todo", &data)?;

        Ok(Some(previous_owner))
    })();

    let _ = fs2::FileExt::unlock(&lock_file);

    let previous_owner = result?;

    let detail = match reason {
        Some(r) => format!("reason: {r}"),
        None => "reason: (none given)".to_string(),
    };
    let _ = crate::storage::events::append_event(
        &ctx.handoff_dir,
        crate::storage::events::EventRecord {
            ts: Utc::now().to_rfc3339(),
            event: "task.reclaimed".to_string(),
            task_id: Some(task_id.to_string()),
            agent_id: Some(reclaimer_agent_id.to_string()),
            session_id: None,
            detail: Some(detail),
        },
    );

    let mut msg = format!(
        "Task {task_id} reclaimed successfully (previous owner: {}).",
        previous_owner.unwrap_or_default()
    );
    if let Some(r) = reason {
        msg.push_str(&format!(" Reason: {r}"));
    }
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ctx(handoff_dir: PathBuf, agent_id: Option<&str>) -> HandlerContext {
        HandlerContext {
            agent_id: agent_id.map(String::from),
            project_dir: handoff_dir.parent().unwrap().to_path_buf(),
            handoff_dir,
        }
    }

    fn write_todo_task(tasks_dir: &std::path::Path, id: &str, dir_name: &str) {
        let task_dir = tasks_dir.join(dir_name);
        std::fs::create_dir_all(&task_dir).unwrap();
        let data = crate::storage::tasks::TaskData {
            id: id.to_string(),
            title: "Test".to_string(),
            notes: None,
            priority: None,
            created_at: None,
            updated_at: None,
            completed_at: None,
            labels: Vec::new(),
            links: Vec::new(),
            task_links: Vec::new(),
            done_criteria: Vec::new(),
            schedule: None,
            dependencies: Vec::new(),
            order: None,
            assignee: None,
            lock: None,
            extra: Default::default(),
        };
        crate::storage::tasks::write_task(&task_dir, "todo", &data).unwrap();
    }

    #[test]
    fn handle_claim_requires_task_id() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path().join(".handoff"), Some("agent-1"));
        let err = handle_claim(&c, &serde_json::json!({})).unwrap_err();
        assert!(err.to_string().contains("task_id"));
    }

    #[test]
    fn handle_claim_and_release_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        let tasks_dir = handoff_dir.join("tasks");
        write_todo_task(&tasks_dir, "t1", "t1-test");

        let c = ctx(handoff_dir.clone(), Some("agent-1"));
        let result = handle_claim(
            &c,
            &serde_json::json!({"task_id": "t1", "session_id": "s-1"}),
        )
        .unwrap();
        assert!(result.contains("\"agent_id\": \"agent-1\""));

        let release_result = handle_release(&c, &serde_json::json!({"task_id": "t1"})).unwrap();
        assert!(release_result.contains("released successfully"));

        let (data, status) = crate::storage::tasks::read_task(&tasks_dir.join("t1-test"))
            .unwrap()
            .unwrap();
        assert!(data.lock.is_none());
        assert_eq!(status, "todo");
    }

    #[test]
    fn handle_release_invalid_revert_status_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        let tasks_dir = handoff_dir.join("tasks");
        write_todo_task(&tasks_dir, "t1", "t1-test");

        let c = ctx(handoff_dir, Some("agent-1"));
        handle_claim(&c, &serde_json::json!({"task_id": "t1"})).unwrap();

        let err = handle_release(
            &c,
            &serde_json::json!({"task_id": "t1", "revert_status": "not_a_status"}),
        )
        .unwrap_err();
        assert!(err.to_string().contains("Invalid status"));
    }

    #[test]
    fn handle_reclaim_requires_task_id() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path().join(".handoff"), Some("admin"));
        let err = handle_reclaim(&c, &serde_json::json!({})).unwrap_err();
        assert!(err.to_string().contains("task_id"));
    }

    #[test]
    fn handle_reclaim_forcibly_releases_lock_ignoring_ownership() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        let tasks_dir = handoff_dir.join("tasks");
        write_todo_task(&tasks_dir, "t1", "t1-test");

        // agent-1 claims it (non-expired lease).
        let owner_ctx = ctx(handoff_dir.clone(), Some("agent-1"));
        handle_claim(
            &owner_ctx,
            &serde_json::json!({"task_id": "t1", "session_id": "s-1"}),
        )
        .unwrap();

        // A different agent (the "reclaimer") forcibly reclaims it — this
        // must succeed even though the lease has not expired and the
        // reclaimer is not the owner (unlike handle_release, which would
        // reject this).
        let reclaimer_ctx = ctx(handoff_dir.clone(), Some("reclaimer-agent"));
        let result = handle_reclaim(
            &reclaimer_ctx,
            &serde_json::json!({"task_id": "t1", "reason": "owner crashed"}),
        )
        .unwrap();
        assert!(result.contains("reclaimed successfully"));
        assert!(result.contains("agent-1"));

        let (data, status) = crate::storage::tasks::read_task(&tasks_dir.join("t1-test"))
            .unwrap()
            .unwrap();
        assert!(data.lock.is_none());
        assert_eq!(status, "todo");

        // events.jsonl records the reclaim with the reclaiming agent's id
        // and the reason.
        let events_content = std::fs::read_to_string(handoff_dir.join("events.jsonl")).unwrap();
        let reclaim_line = events_content
            .lines()
            .find(|l| l.contains("task.reclaimed"))
            .expect("task.reclaimed event should be recorded");
        assert!(reclaim_line.contains("reclaimer-agent"));
        assert!(reclaim_line.contains("owner crashed"));
    }

    #[test]
    fn handle_reclaim_errors_when_task_is_not_locked() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        let tasks_dir = handoff_dir.join("tasks");
        write_todo_task(&tasks_dir, "t1", "t1-test");

        let c = ctx(handoff_dir, Some("admin"));
        let err = handle_reclaim(&c, &serde_json::json!({"task_id": "t1"})).unwrap_err();
        assert!(err.to_string().contains("not locked"));
    }
}
