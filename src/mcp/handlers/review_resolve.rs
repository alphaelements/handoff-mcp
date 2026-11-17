//! `handoff_review_resolve` (RBD-002, SPEC-RBD-005): move a resolved item from
//! the review board to the decisions archive. The file work lives in
//! [`crate::storage::review_decisions`].

use anyhow::{bail, Result};
use serde_json::{json, Value};

use super::review_session_decision;
use super::HandlerContext;
use crate::storage::review_board::DECISIONS_FILE;
use crate::storage::review_decisions::resolve_item;

fn required_str<'a>(arguments: &'a Value, key: &str) -> Result<&'a str> {
    match arguments.get(key) {
        Some(Value::String(s)) => Ok(s.as_str()),
        None | Some(Value::Null) => bail!("'{key}' parameter is required"),
        Some(_) => bail!("'{key}' must be a string"),
    }
}

fn optional_bool(arguments: &Value, key: &str) -> Result<Option<bool>> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => bail!("'{key}' must be a boolean"),
    }
}

pub fn handle(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let id = required_str(arguments, "id")?;
    let decision = required_str(arguments, "decision")?;
    // Validated before the resolve writes anything.
    let record_session_decision =
        optional_bool(arguments, "record_session_decision")?.unwrap_or(false);

    let resolved = resolve_item(
        &ctx.handoff_dir,
        id,
        decision,
        chrono::Utc::now().date_naive(),
    )?;
    let mut warnings = resolved.warnings;
    // RBD-003: the resolve is already done; a missing session is only a warning.
    if record_session_decision {
        warnings.extend(review_session_decision::record(
            ctx,
            &resolved.id,
            &resolved.title,
            decision,
        ));
    }
    let result = json!({
        "id": resolved.id,
        "title": resolved.title,
        "decisions_path": format!(".handoff/{DECISIONS_FILE}"),
        "warnings": warnings,
    });
    serde_json::to_string_pretty(&result).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> (tempfile::TempDir, HandlerContext) {
        let dir = tempfile::tempdir().unwrap();
        let handoff_dir = dir.path().join(".handoff");
        std::fs::create_dir_all(&handoff_dir).unwrap();
        std::fs::write(
            handoff_dir.join("review_board.md"),
            "## X\n\n### A <!-- rb-id: R-001 -->\n- date: 2026-11-17\n\nBody.\n\n- [ ] 確認済み\n\nメモ:\n",
        )
        .unwrap();
        let ctx = HandlerContext {
            agent_id: None,
            project_dir: dir.path().to_path_buf(),
            handoff_dir,
        };
        (dir, ctx)
    }

    #[test]
    fn resolve_is_write_classified() {
        assert!(crate::mcp::router::is_write_tool("handoff_review_resolve"));
    }

    #[test]
    fn resolve_returns_the_id_and_moves_the_item() {
        let (_d, ctx) = ctx();
        let out: Value =
            serde_json::from_str(&handle(&ctx, &json!({"id": "R-001", "decision": "Go"})).unwrap())
                .unwrap();
        assert_eq!(out["id"], "R-001");
        assert_eq!(out["warnings"], json!([]));
        let board = std::fs::read_to_string(ctx.handoff_dir.join("review_board.md")).unwrap();
        assert!(!board.contains("R-001"));
        let dec = std::fs::read_to_string(ctx.handoff_dir.join(DECISIONS_FILE)).unwrap();
        assert!(dec.contains("original_id: R-001"));
    }

    #[test]
    fn argument_errors_change_nothing() {
        let (_d, ctx) = ctx();
        let before = std::fs::read(ctx.handoff_dir.join("review_board.md")).unwrap();
        for args in [
            json!({"decision": "Go"}),
            json!({"id": "R-001"}),
            json!({"id": "R-404", "decision": "Go"}),
            json!({"id": 1, "decision": "Go"}),
            json!({"id": "R-001", "decision": 5}),
        ] {
            assert!(handle(&ctx, &args).is_err(), "{args}");
        }
        assert_eq!(
            std::fs::read(ctx.handoff_dir.join("review_board.md")).unwrap(),
            before
        );
        assert!(!ctx.handoff_dir.join(DECISIONS_FILE).exists());
    }

    #[test]
    fn record_session_decision_appends_to_the_active_session() {
        let (_d, ctx) = ctx();
        std::fs::create_dir_all(ctx.handoff_dir.join("sessions")).unwrap();
        crate::mcp::handlers::save_context::handle(
            &ctx,
            &json!({"summary": "s", "session_status": "active"}),
        )
        .unwrap();
        let out: Value = serde_json::from_str(
            &handle(
                &ctx,
                &json!({"id": "R-001", "decision": "Go", "record_session_decision": true}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(out["warnings"], json!([]));
        let sessions =
            crate::storage::sessions::read_active_sessions(&ctx.handoff_dir.join("sessions"))
                .unwrap();
        assert_eq!(
            sessions[0].decisions,
            vec![json!({
                "decision": "Resolved R-001: A \u{2014} Go",
                "confidence": "confirmed",
                "reason": "User resolved via review board",
            })]
        );
    }

    #[test]
    fn record_session_decision_without_session_warns_but_resolves() {
        let (_d, ctx) = ctx();
        let out: Value = serde_json::from_str(
            &handle(
                &ctx,
                &json!({"id": "R-001", "decision": "Go", "record_session_decision": true}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(out["warnings"].as_array().unwrap().len(), 1);
        assert!(out["warnings"][0]
            .as_str()
            .unwrap()
            .contains("No active session"));
        let board = std::fs::read_to_string(ctx.handoff_dir.join("review_board.md")).unwrap();
        assert!(!board.contains("R-001"));
    }

    #[test]
    fn session_decision_is_not_recorded_by_default_or_when_false() {
        for extra in [json!({}), json!({"record_session_decision": false})] {
            let (_d, ctx) = ctx();
            std::fs::create_dir_all(ctx.handoff_dir.join("sessions")).unwrap();
            crate::mcp::handlers::save_context::handle(
                &ctx,
                &json!({"summary": "s", "session_status": "active"}),
            )
            .unwrap();
            let mut args = json!({"id": "R-001", "decision": "Go"});
            args.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            handle(&ctx, &args).unwrap();
            let sessions =
                crate::storage::sessions::read_active_sessions(&ctx.handoff_dir.join("sessions"))
                    .unwrap();
            assert!(sessions[0].decisions.is_empty());
        }
    }

    #[test]
    fn non_boolean_record_flag_is_rejected_before_any_write() {
        let (_d, ctx) = ctx();
        let args = json!({"id": "R-001", "decision": "Go", "record_session_decision": "yes"});
        assert!(handle(&ctx, &args).is_err());
        assert!(!ctx.handoff_dir.join(DECISIONS_FILE).exists());
    }
}
