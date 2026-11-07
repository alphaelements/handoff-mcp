use anyhow::Result;
use serde_json::{json, Value};

use super::save_context::{ambiguous_message, record_ownership_event};
use super::HandlerContext;
use crate::storage::session_target::{
    resolve_session_target, Caller, Resolution, Target, TargetOp,
};
use crate::storage::sessions::{merge_sessions, read_session_entries};

pub fn handle(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;
    let sessions_dir = handoff.join("sessions");

    let source_session_ids: Vec<&str> = arguments
        .get("source_session_ids")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("'source_session_ids' parameter is required"))?
        .iter()
        .filter_map(|v| v.as_str())
        .collect();

    if source_session_ids.len() < 2 {
        anyhow::bail!("'source_session_ids' must contain at least 2 session IDs");
    }

    let target_session_id = arguments
        .get("target_session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'target_session_id' parameter is required"))?;

    if !source_session_ids.contains(&target_session_id) {
        anyhow::bail!(
            "target_session_id '{}' must be one of the source_session_ids",
            target_session_id
        );
    }

    let close_sources = arguments
        .get("close_sources")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let force = arguments
        .get("force_other_owner")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let reason = arguments
        .get("reason")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|r| !r.is_empty());
    if force && reason.is_none() {
        anyhow::bail!(
            "force_other_owner requires a non-empty 'reason' (it is recorded in the event log)"
        );
    }

    // Check every session's ownership before touching any of them, so a
    // merge that would close or rewrite another agent's active session fails
    // as a whole instead of half-applying.
    let worktree = ctx.project_dir.to_string_lossy().to_string();
    let caller = Caller {
        agent_id: ctx.agent_id.as_deref(),
        worktree: &worktree,
    };
    let entries = read_session_entries(&sessions_dir)?;
    let mut refused: Vec<String> = Vec::new();
    let mut approved: Vec<Target> = Vec::new();
    for sid in &source_session_ids {
        let is_target = *sid == target_session_id;
        if !is_target && !close_sources {
            // Read only: nothing of this source changes.
            continue;
        }
        let op = if is_target {
            TargetOp::Merge
        } else {
            TargetOp::Close
        };
        match resolve_session_target(&entries, &caller, Some(sid), op, force) {
            Resolution::Target(t) => approved.push(t),
            Resolution::Refused { id, owner } => refused.push(format!("{id} (owner {owner})")),
            Resolution::Ambiguous { ids } => anyhow::bail!(ambiguous_message(&ids)),
            _ => {}
        }
    }
    if !refused.is_empty() {
        anyhow::bail!(
            "Nothing was merged: these active sessions belong to another agent: {}. Pass \
             force_other_owner:true with a reason to merge them anyway.",
            refused.join(", ")
        );
    }
    for t in &approved {
        record_ownership_event(ctx, t, reason);
    }

    let result = merge_sessions(
        &sessions_dir,
        &source_session_ids,
        target_session_id,
        close_sources,
        ctx.agent_id.as_deref(),
    )?;

    let conflicts_json: Vec<Value> = result
        .conflicts
        .iter()
        .map(|c| {
            json!({
                "type": c.conflict_type,
                "description": c.description,
                "session_a": c.session_a,
                "session_b": c.session_b,
            })
        })
        .collect();

    let output_json = json!({
        "merged_session_id": result.merged_session_id,
        "merged_decisions": result.merged_decisions,
        "merged_notes": result.merged_notes,
        "merged_references": result.merged_references,
        "merged_context_pointers": result.merged_context_pointers,
        "conflicts": conflicts_json,
        "closed_sessions": result.closed_sessions,
    });

    let mut output = format!(
        "Merged {} sessions into {}\n\
         Merged: {} decisions, {} notes, {} references, {} context_pointers",
        source_session_ids.len(),
        result.merged_session_id,
        result.merged_decisions,
        result.merged_notes,
        result.merged_references,
        result.merged_context_pointers,
    );

    if !result.conflicts.is_empty() {
        output.push_str(&format!("\nConflicts: {}", result.conflicts.len()));
        for c in &result.conflicts {
            output.push_str(&format!("\n  - {}: {}", c.conflict_type, c.description));
        }
    }

    if !result.closed_sessions.is_empty() {
        output.push_str(&format!(
            "\nClosed source sessions: {}",
            result.closed_sessions.join(", ")
        ));
    }

    for warning in approved.iter().flat_map(|t| &t.warnings) {
        output.push('\n');
        output.push_str(warning);
    }

    output.push_str(&format!(
        "\n\n{}",
        serde_json::to_string_pretty(&output_json)?
    ));

    Ok(output)
}
