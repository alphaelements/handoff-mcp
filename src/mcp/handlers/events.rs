//! `handoff_events` tool (spec 3.6.3, 4.2 FR-2.5): query
//! `.handoff/events.jsonl` with optional `since`/`task_id`/`agent_id`/
//! `event_type`/`limit` filters.
//!
//! This is a thin argument-parsing wrapper around
//! [`crate::storage::events::read_events`], which does the actual
//! read+filter+truncate work.

use anyhow::Result;
use serde_json::Value;

use super::HandlerContext;
use crate::storage::events::{read_events, EventFilters};

pub fn handle(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let filters = EventFilters {
        since: arguments
            .get("since")
            .and_then(|v| v.as_str())
            .map(String::from),
        task_id: arguments
            .get("task_id")
            .and_then(|v| v.as_str())
            .map(String::from),
        agent_id: arguments
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(String::from),
        event_type: arguments
            .get("event_type")
            .and_then(|v| v.as_str())
            .map(String::from),
        limit: arguments
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize),
    };

    let events = read_events(&ctx.handoff_dir, &filters)?;

    let result = serde_json::json!({
        "events": events,
        "total": events.len(),
    });

    serde_json::to_string_pretty(&result).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::events::{append_event, EventRecord};
    use std::path::PathBuf;

    fn ctx(handoff_dir: PathBuf) -> HandlerContext {
        HandlerContext {
            agent_id: None,
            project_dir: handoff_dir.parent().unwrap().to_path_buf(),
            handoff_dir,
        }
    }

    #[test]
    fn handle_returns_empty_when_no_log() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path().join(".handoff"));
        let out = handle(&c, &serde_json::json!({})).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["total"], 0);
    }

    #[test]
    fn handle_applies_filters_from_arguments() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff_dir).unwrap();

        append_event(
            &handoff_dir,
            EventRecord {
                ts: "2026-08-21T00:00:00Z".to_string(),
                event: "task.claimed".to_string(),
                task_id: Some("t1".to_string()),
                agent_id: Some("a1".to_string()),
                session_id: None,
                detail: None,
            },
        )
        .unwrap();
        append_event(
            &handoff_dir,
            EventRecord {
                ts: "2026-08-21T00:01:00Z".to_string(),
                event: "task.released".to_string(),
                task_id: Some("t1".to_string()),
                agent_id: Some("a1".to_string()),
                session_id: None,
                detail: None,
            },
        )
        .unwrap();

        let c = ctx(handoff_dir);
        let out = handle(&c, &serde_json::json!({"event_type": "task.released"})).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["total"], 1);
        assert_eq!(parsed["events"][0]["event"], "task.released");
    }
}
