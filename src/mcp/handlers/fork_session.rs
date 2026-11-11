use anyhow::Result;
use serde_json::{json, Value};

use super::save_context::append_session_event_with_detail;
use super::HandlerContext;
use crate::storage::session_target::effective_owner;
use crate::storage::sessions::{fork_session, read_session_by_id, ForkSpec};

pub fn handle(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;
    let sessions_dir = handoff.join("sessions");

    let source_session_id = arguments
        .get("source_session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'source_session_id' parameter is required"))?;

    let summary = arguments
        .get("summary")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'summary' parameter is required"))?;

    let label = arguments.get("label").and_then(|v| v.as_str());
    let timeline = arguments.get("timeline").and_then(|v| v.as_str());

    let related_task_ids: Vec<String> = arguments
        .get("related_task_ids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let default_inherit = vec![
        "decisions",
        "context_pointers",
        "references",
        "handoff_notes",
        "environment",
    ];
    let inherit: Vec<&str> = arguments
        .get("inherit")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or(default_inherit);

    let source = read_session_by_id(&sessions_dir, source_session_id)?.ok_or_else(|| {
        anyhow::anyhow!(
            "Source session not found: {source_session_id}. \
                 Use handoff_list_sessions to see available session IDs."
        )
    })?;

    let forked = fork_session(
        &sessions_dir,
        &source,
        ForkSpec {
            summary,
            label,
            timeline,
            related_task_ids,
            inherit: &inherit,
        },
        &crate::storage::sessions::ForkOwner {
            agent_id: ctx.agent_id.as_deref(),
            worktree: &ctx.project_dir.to_string_lossy(),
            scope: crate::storage::detect_session_scope(&ctx.project_dir),
        },
    )?;

    let forked_id = forked.id.as_deref().unwrap_or("unknown");
    // The fork takes the caller's owner, worktree and scope; what the source
    // had is kept in the event so the hand-off stays traceable.
    append_session_event_with_detail(
        handoff,
        "session.created",
        forked.id.as_deref(),
        ctx.agent_id.as_deref(),
        Some(format!(
            "forked_from={source_session_id}; source_owner={}; source_worktree={}; source_scope={}",
            effective_owner(&source).unwrap_or("none"),
            source.worktree.as_deref().unwrap_or("none"),
            source.scope.as_deref().unwrap_or("none"),
        )),
    );

    let result = json!({
        "session_id": forked_id,
        "parent_session_id": source_session_id,
        "status": "active",
        "inherited": inherit,
    });

    let mut output = format!(
        "Forked session: {}\nSession ID: {}\nParent: {}\nStatus: active\nInherited: {}",
        summary,
        forked_id,
        source_session_id,
        inherit.join(", "),
    );
    if let Some(tl) = forked.timeline.as_deref() {
        output.push_str(&format!("\nTimeline: {tl}"));
    }
    if let Some(lbl) = forked.label.as_deref() {
        output.push_str(&format!("\nLabel: {lbl}"));
    }
    output.push_str(&format!("\n\n{}", serde_json::to_string_pretty(&result)?));

    Ok(output)
}
