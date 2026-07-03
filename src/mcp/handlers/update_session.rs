use anyhow::{Context, Result};
use serde_json::Value;

use super::resolve_project_dir;
use crate::storage::ensure_handoff_exists;
use crate::storage::sessions::{read_active_sessions, SessionData};

pub fn handle(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;
    let sessions_dir = handoff.join("sessions");

    let target_session_id = arguments.get("session_id").and_then(|v| v.as_str());

    let active = read_active_sessions(&sessions_dir)?;
    if active.is_empty() {
        anyhow::bail!(
            "No active session. Call save_context with session_status='active' to create one first."
        );
    }

    let session = if let Some(tid) = target_session_id {
        active
            .iter()
            .find(|s| {
                s.id.as_deref()
                    .is_some_and(|id| id == tid || id.starts_with(tid) || tid.starts_with(id))
            })
            .ok_or_else(|| anyhow::anyhow!("session_id '{tid}' not found among active sessions"))?
    } else if active.len() == 1 {
        &active[0]
    } else {
        active.last().unwrap()
    };
    let sid = session.id.as_deref().unwrap_or("");

    let checklist_index = arguments
        .get("checklist_index")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    let checklist_checked = arguments.get("checklist_checked").and_then(|v| v.as_bool());
    let add_checklist_item = arguments.get("add_checklist_item").and_then(|v| v.as_str());
    let add_decision = arguments.get("add_decision");
    let add_handoff_note = arguments.get("add_handoff_note");
    let add_context_pointer = arguments.get("add_context_pointer");

    let mut data = session.clone();
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

    write_active_session_data(&sessions_dir, sid, &data)?;

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

    Ok(msg)
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

fn write_active_session_data(
    sessions_dir: &std::path::Path,
    session_id: &str,
    data: &SessionData,
) -> Result<()> {
    let suffix = ".active.json";

    for entry in std::fs::read_dir(sessions_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(suffix) {
            continue;
        }

        let content = std::fs::read_to_string(entry.path())
            .with_context(|| format!("Failed to read session: {}", entry.path().display()))?;
        let existing: SessionData = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse session: {}", entry.path().display()))?;

        let file_id = existing.id.as_deref().unwrap_or("");
        if file_id != session_id {
            continue;
        }

        let updated = serde_json::to_string_pretty(data).context("Failed to serialize session")?;
        crate::storage::atomic_write(entry.path(), updated.as_bytes())
            .with_context(|| format!("Failed to write session: {}", entry.path().display()))?;
        return Ok(());
    }

    anyhow::bail!("Active session '{session_id}' not found on disk")
}
