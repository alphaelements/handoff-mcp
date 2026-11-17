//! Session-decision bridge for `handoff_review_resolve` (RBD-003).
//!
//! When a review item is resolved with `record_session_decision=true`, the
//! decision is also appended to the caller's active session, through the same
//! path as `handoff_update_session add_decision`. The resolve itself has
//! already succeeded by then, so a failure here is reported as a warning for
//! the resolve response instead of failing the call.

use serde_json::{json, Value};

use super::{update_session, HandlerContext};

/// Confidence recorded on a decision the user made on the review board.
const DECISION_CONFIDENCE: &str = "confirmed";
/// Reason recorded on a decision the user made on the review board.
const DECISION_REASON: &str = "User resolved via review board";

/// Builds the session decision entry for a resolved item.
pub fn session_decision(item_id: &str, title: &str, decision_text: &str) -> Value {
    json!({
        "decision": format!("Resolved {item_id}: {title} \u{2014} {decision_text}"),
        "confidence": DECISION_CONFIDENCE,
        "reason": DECISION_REASON,
    })
}

/// Appends the resolved item's decision to the active session.
///
/// Returns `None` when it was recorded, or `Some(warning)` when it could not be
/// (no active session of the caller, session owned by another agent, write
/// failure, ...). The resolve must still complete in that case, so the error is
/// surfaced as the warning text rather than propagated.
pub fn record(
    ctx: &HandlerContext,
    item_id: &str,
    title: &str,
    decision_text: &str,
) -> Option<String> {
    let args = json!({ "add_decision": session_decision(item_id, title, decision_text) });
    match update_session::handle(ctx, &args) {
        Ok(_) => None,
        Err(e) => Some(format!(
            "Warning: the decision was not recorded in the session decisions: {e:#}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::handlers::save_context;
    use crate::storage::sessions::read_active_sessions;
    use tempfile::TempDir;

    fn setup() -> (TempDir, HandlerContext) {
        let tmp = TempDir::new().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(handoff.join("sessions")).unwrap();
        let ctx = HandlerContext {
            agent_id: None,
            project_dir: tmp.path().to_path_buf(),
            handoff_dir: handoff,
        };
        (tmp, ctx)
    }

    fn start_session(ctx: &HandlerContext) {
        save_context::handle(ctx, &json!({ "summary": "s", "session_status": "active" })).unwrap();
    }

    #[test]
    fn decision_entry_has_the_specified_format() {
        let d = session_decision("R-007", "Pick a store", "Use files");
        assert_eq!(
            d["decision"],
            "Resolved R-007: Pick a store \u{2014} Use files"
        );
        assert_eq!(d["confidence"], "confirmed");
        assert_eq!(d["reason"], "User resolved via review board");
    }

    #[test]
    fn records_decision_in_the_active_session() {
        let (_tmp, ctx) = setup();
        start_session(&ctx);

        assert_eq!(record(&ctx, "R-001", "T", "Go"), None);

        let sessions = read_active_sessions(&ctx.handoff_dir.join("sessions")).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0].decisions,
            vec![session_decision("R-001", "T", "Go")]
        );
    }

    #[test]
    fn appends_after_existing_decisions() {
        let (_tmp, ctx) = setup();
        start_session(&ctx);
        assert_eq!(record(&ctx, "R-001", "A", "x"), None);
        assert_eq!(record(&ctx, "R-002", "B", "y"), None);

        let sessions = read_active_sessions(&ctx.handoff_dir.join("sessions")).unwrap();
        assert_eq!(sessions[0].decisions.len(), 2);
        assert_eq!(
            sessions[0].decisions[1]["decision"],
            "Resolved R-002: B \u{2014} y"
        );
    }

    #[test]
    fn no_active_session_yields_a_warning_not_an_error() {
        let (_tmp, ctx) = setup();
        let warning = record(&ctx, "R-001", "T", "Go").expect("warning expected");
        assert!(warning.starts_with("Warning:"), "{warning}");
        assert!(warning.contains("No active session"), "{warning}");
    }
}
