use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::resolve_project_dir;
use crate::storage::ensure_handoff_exists;

pub fn handle(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;
    let sessions_dir = handoff.join("sessions");

    let status_filter = arguments.get("status_filter").and_then(|v| v.as_str());
    let limit = arguments
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(20) as usize;

    if !sessions_dir.exists() {
        return serde_json::to_string_pretty(&json!([])).map_err(Into::into);
    }

    let mut sessions: Vec<Value> = Vec::new();

    for entry in std::fs::read_dir(&sessions_dir)
        .with_context(|| format!("Failed to read sessions dir: {}", sessions_dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".json") {
            continue;
        }

        let status = extract_session_status(&name);
        if let Some(filter) = status_filter {
            if status != filter {
                continue;
            }
        }

        let content = match std::fs::read_to_string(entry.path()) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let data: Value = match serde_json::from_str(&content) {
            Ok(d) => d,
            Err(_) => continue,
        };

        let id = data
            .get("id")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| synthesize_id_from_filename(&name));

        let summary = data.get("summary").and_then(|v| v.as_str()).unwrap_or("");
        let started_at = data.get("started_at").and_then(|v| v.as_str());
        let ended_at = data.get("ended_at").and_then(|v| v.as_str());
        let branch = data.get("branch").and_then(|v| v.as_str());
        let commit = data.get("commit").and_then(|v| v.as_str());

        let decisions_count = data
            .get("decisions")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);

        let checklist = data.get("checklist").and_then(|v| v.as_array());
        let checklist_count = checklist.map(|a| a.len()).unwrap_or(0);
        let checklist_checked = checklist
            .map(|a| {
                a.iter()
                    .filter(|item| {
                        item.get("checked")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0);

        sessions.push(json!({
            "id": id,
            "status": status,
            "summary": summary,
            "started_at": started_at,
            "ended_at": ended_at,
            "branch": branch,
            "commit": commit,
            "decisions_count": decisions_count,
            "checklist_progress": format!("{}/{}", checklist_checked, checklist_count),
        }));
    }

    sessions.sort_by(|a, b| {
        let a_time = a.get("ended_at").and_then(|v| v.as_str()).unwrap_or("");
        let b_time = b.get("ended_at").and_then(|v| v.as_str()).unwrap_or("");
        b_time.cmp(a_time)
    });

    sessions.truncate(limit);

    serde_json::to_string_pretty(&sessions).map_err(Into::into)
}

fn extract_session_status(filename: &str) -> &str {
    let name = filename.strip_suffix(".json").unwrap_or(filename);
    if name.ends_with(".open") {
        "open"
    } else if name.ends_with(".active") {
        "active"
    } else if name.ends_with(".paused") {
        "paused"
    } else if name.ends_with(".closed") {
        "closed"
    } else {
        "unknown"
    }
}

fn synthesize_id_from_filename(filename: &str) -> String {
    let name = filename.strip_suffix(".json").unwrap_or(filename);
    let base = name
        .strip_suffix(".open")
        .or_else(|| name.strip_suffix(".active"))
        .or_else(|| name.strip_suffix(".paused"))
        .or_else(|| name.strip_suffix(".closed"))
        .unwrap_or(name);
    format!("s-{}", &base[..std::cmp::min(base.len(), 20)])
}
