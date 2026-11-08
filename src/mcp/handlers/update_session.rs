use anyhow::Result;
use serde_json::Value;

use super::save_context::{ambiguous_message, entry_for, record_ownership_event, write_refusal};
use super::HandlerContext;
use crate::storage::session_target::{resolve_session_target, Caller, Resolution, TargetOp};
use crate::storage::sessions::{modify_session, read_session_entries, SeenSession};

pub fn handle(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;
    let sessions_dir = handoff.join("sessions");

    let target_session_id = arguments.get("session_id").and_then(|v| v.as_str());

    let worktree = ctx.project_dir.to_string_lossy().to_string();
    let caller = Caller {
        agent_id: ctx.agent_id.as_deref(),
        worktree: &worktree,
    };
    let entries = read_session_entries(&sessions_dir)?;
    let mut notes: Vec<String> = Vec::new();
    let target = match resolve_session_target(
        &entries,
        &caller,
        target_session_id,
        TargetOp::Append,
        false,
    ) {
        Resolution::Target(t) => t,
        Resolution::NotFound => {
            let tid = target_session_id.unwrap_or_default();
            anyhow::bail!("session_id '{tid}' not found among active sessions");
        }
        Resolution::NoTarget => {
            anyhow::bail!(
                "No active session of yours in this worktree. Call save_context with \
                 session_status='active' to create one, or pass session_id to update a \
                 specific session."
            );
        }
        Resolution::NeedsExplicitId { ids } => anyhow::bail!(
            "An active session owned by another agent exists in this worktree ({}), and this \
             caller has no agent identity (CLI). Pass session_id to say which session you mean.",
            ids.join(", ")
        ),
        Resolution::Ambiguous { ids } => anyhow::bail!(ambiguous_message(&ids)),
        Resolution::Refused { id, owner } => {
            anyhow::bail!("Session {id} is owned by another agent ({owner}); it was not updated.")
        }
    };
    let entry = entry_for(&entries, &target.id)
        .ok_or_else(|| anyhow::anyhow!("Session '{}' disappeared", target.id))?;
    let seen = SeenSession::of(entry);
    let sid = target.id.as_str();

    let checklist_index = arguments
        .get("checklist_index")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    let checklist_checked = arguments.get("checklist_checked").and_then(|v| v.as_bool());
    let add_checklist_item = arguments.get("add_checklist_item").and_then(|v| v.as_str());
    let add_decision = arguments.get("add_decision");
    let add_handoff_note = arguments.get("add_handoff_note");
    let add_context_pointer = arguments.get("add_context_pointer");

    // The change is applied to the session as it is on disk now (not to the
    // copy read above) and only while its owner is still the one seen, under
    // the sessions lock.
    let (changes, data) = modify_session(&sessions_dir, &seen, |data| {
        let mut changes = Vec::new();

        if let Some(idx) = checklist_index {
            let checked = checklist_checked.unwrap_or(true);
            if idx >= data.checklist.len() {
                anyhow::bail!(
                    "checklist_index {idx} out of range (session has {} items)",
                    data.checklist.len()
                );
            }
            if let Some(item) = data.checklist.get_mut(idx) {
                item["checked"] = serde_json::json!(checked);
                let item_text = item
                    .get("item")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(unknown)");
                changes.push(format!(
                    "checklist[{idx}] '{}' → {}",
                    item_text,
                    if checked { "checked" } else { "unchecked" }
                ));
            }
        }

        if let Some(item_text) = add_checklist_item {
            let owner = arguments
                .get("checklist_owner")
                .and_then(|v| v.as_str())
                .unwrap_or("ai");
            data.checklist.push(serde_json::json!({
                "item": item_text,
                "checked": false,
                "owner": owner
            }));
            changes.push(format!("added checklist item: '{item_text}'"));
        }

        if let Some(decision) = add_decision {
            if decision.is_object() {
                data.decisions.push(decision.clone());
                let desc = decision
                    .get("decision")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(decision)");
                changes.push(format!("added decision: '{desc}'"));
            } else {
                anyhow::bail!("add_decision must be an object with at least a 'decision' field");
            }
        }

        if let Some(note) = add_handoff_note {
            if note.is_object() {
                data.handoff_notes.push(note.clone());
                let text = note
                    .get("note")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(note)");
                changes.push(format!("added handoff_note: '{}'", truncate(text, 60)));
            } else {
                anyhow::bail!("add_handoff_note must be an object with at least a 'note' field");
            }
        }

        if let Some(pointer) = add_context_pointer {
            if pointer.is_object() {
                data.context_pointers.push(pointer.clone());
                let path = pointer
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(path)");
                changes.push(format!("added context_pointer: '{path}'"));
            } else {
                anyhow::bail!("add_context_pointer must be an object with at least a 'path' field");
            }
        }

        if changes.is_empty() {
            anyhow::bail!(
                "No updates specified. Use checklist_index, add_checklist_item, \
                 add_decision, add_handoff_note, or add_context_pointer."
            );
        }
        Ok(changes)
    })
    .map_err(|e| write_refusal(ctx, e, "update"))?;
    record_ownership_event(ctx, &target, None);
    notes.extend(target.warnings.iter().cloned());

    let mut msg = format!("Session {} updated:", sid);
    for c in &changes {
        msg.push_str(&format!("\n  - {c}"));
    }

    let checked_count = data
        .checklist
        .iter()
        .filter(|item| {
            item.get("checked")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .count();
    msg.push_str(&format!(
        "\nChecklist: {}/{} checked",
        checked_count,
        data.checklist.len()
    ));
    for note in &notes {
        msg.push('\n');
        msg.push_str(note);
    }

    Ok(msg)
}

fn truncate(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}
