use anyhow::Result;
use serde_json::{json, Value};

use super::resolve_project_dir;
use crate::storage::ensure_handoff_exists;
use crate::storage::sessions::merge_sessions;

pub fn handle(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;
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

    let result = merge_sessions(
        &sessions_dir,
        &source_session_ids,
        target_session_id,
        close_sources,
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

    output.push_str(&format!(
        "\n\n{}",
        serde_json::to_string_pretty(&output_json)?
    ));

    Ok(output)
}
