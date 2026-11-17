//! `handoff_review_read` (RB-003) and `handoff_review_update` (RB-004): partial
//! read and partial write of one item section of the User Review Board
//! (`.handoff/review_board.md`).
//!
//! Neither tool returns or rewrites the whole board: read answers with one
//! section, update replaces one section's bytes under an `expected_hash`
//! optimistic lock. The byte-level work lives in
//! [`crate::storage::review_section`].

use anyhow::{bail, Result};
use serde_json::{json, Value};

use super::HandlerContext;
use crate::storage::review_section::{
    read_item_with_warnings, update_item, ReviewUpdates, SectionRef,
};

/// `format` values of `handoff_review_read`.
const FORMAT_FULL: &str = "full";
const FORMAT_META: &str = "meta";

fn optional_str<'a>(arguments: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => bail!("'{key}' must be a string"),
    }
}

pub fn handle_read(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let id = optional_str(arguments, "id")?;
    let seq = match arguments.get("seq") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| anyhow::anyhow!("'seq' must be a non-negative integer"))?,
        ),
    };
    let meta = match optional_str(arguments, "format")?.unwrap_or(FORMAT_FULL) {
        FORMAT_FULL => false,
        FORMAT_META => true,
        other => bail!("Invalid format '{other}': expected '{FORMAT_FULL}' or '{FORMAT_META}'"),
    };

    // `id` wins when both are given (SPEC-RB-007).
    let which = match (id, seq) {
        (Some(id), _) => SectionRef::Id(id),
        (None, Some(seq)) => SectionRef::Seq(seq),
        (None, None) => bail!("either 'id' (e.g. \"R-001\") or 'seq' (0-based) is required"),
    };

    let (item, warnings) = read_item_with_warnings(&ctx.handoff_dir, which)?;
    let mut result = item.to_json(meta);
    result["warnings"] = json!(warnings);
    serde_json::to_string_pretty(&result).map_err(Into::into)
}

pub fn handle_update(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let id = optional_str(arguments, "id")?
        .ok_or_else(|| anyhow::anyhow!("'id' parameter is required"))?;
    let expected_hash = optional_str(arguments, "expected_hash")?
        .ok_or_else(|| anyhow::anyhow!("'expected_hash' parameter is required"))?;
    let updates = ReviewUpdates::from_json(
        arguments
            .get("updates")
            .ok_or_else(|| anyhow::anyhow!("'updates' parameter is required"))?,
    )?;

    let done = update_item(&ctx.handoff_dir, id, expected_hash, &updates)?;
    let result = json!({
        "id": id,
        "updated": done.fields,
        "status": done.item.status,
        "checkbox_checked": done.item.checkbox_checked,
        "content_hash": done.item.content_hash,
        "warnings": done.warnings,
    });
    serde_json::to_string_pretty(&result).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOARD: &str = "\
## 設計判断

### First <!-- rb-id: R-001 -->
- priority: high
- date: 2026-11-17

Body one.

- [ ] 確認済み

メモ:

### Second <!-- rb-id: R-002 -->
- date: 2026-11-17

Body two.

- [ ] 確認済み

メモ:
";

    fn ctx() -> (tempfile::TempDir, HandlerContext) {
        let dir = tempfile::tempdir().unwrap();
        let handoff_dir = dir.path().join(".handoff");
        std::fs::create_dir_all(&handoff_dir).unwrap();
        std::fs::write(handoff_dir.join("review_board.md"), BOARD).unwrap();
        let ctx = HandlerContext {
            agent_id: None,
            project_dir: dir.path().to_path_buf(),
            handoff_dir,
        };
        (dir, ctx)
    }

    fn read(ctx: &HandlerContext, args: Value) -> Value {
        serde_json::from_str(&handle_read(ctx, &args).unwrap()).unwrap()
    }

    #[test]
    fn read_is_read_classified_and_update_is_write_classified() {
        assert!(!crate::mcp::router::is_write_tool("handoff_review_read"));
        assert!(crate::mcp::router::is_write_tool("handoff_review_update"));
    }

    #[test]
    fn read_by_id_returns_the_documented_fields_for_one_section() {
        let (_d, ctx) = ctx();
        let v = read(&ctx, json!({"id": "R-001"}));
        assert_eq!(v["id"], "R-001");
        assert_eq!(v["title"], "First");
        assert_eq!(v["body"], "Body one.");
        assert_eq!(v["status"], "pending");
        assert_eq!(v["priority"], "high");
        assert_eq!(v["date"], "2026-11-17");
        assert_eq!(v["checkbox_checked"], false);
        assert_eq!(v["memo"], "");
        assert_eq!(v["images"], json!([]));
        assert!(v["assignee"].is_null() && v["task_id"].is_null() && v["doc_id"].is_null());
        assert!(v["content_hash"].is_string());
        assert!(!v.to_string().contains("Body two"));
    }

    #[test]
    fn read_by_seq_and_id_precedence() {
        let (_d, ctx) = ctx();
        assert_eq!(read(&ctx, json!({"seq": 1}))["id"], "R-002");
        assert_eq!(read(&ctx, json!({"id": "R-001", "seq": 1}))["id"], "R-001");
    }

    #[test]
    fn meta_format_drops_body_and_memo() {
        let (_d, ctx) = ctx();
        let v = read(&ctx, json!({"id": "R-001", "format": "meta"}));
        assert!(v.get("body").is_none() && v.get("memo").is_none());
        assert!(v["content_hash"].is_string());
    }

    #[test]
    fn read_argument_errors() {
        let (_d, ctx) = ctx();
        for args in [
            json!({}),
            json!({"id": "R-404"}),
            json!({"seq": 9}),
            json!({"seq": -1}),
            json!({"seq": "0"}),
            json!({"id": 1}),
            json!({"id": "R-001", "format": "xml"}),
        ] {
            assert!(handle_read(&ctx, &args).is_err(), "{args}");
        }
    }

    #[test]
    fn update_requires_id_expected_hash_and_updates() {
        let (_d, ctx) = ctx();
        let before = std::fs::read(ctx.handoff_dir.join("review_board.md")).unwrap();
        for args in [
            json!({"expected_hash": "x", "updates": {"memo": "a"}}),
            json!({"id": "R-001", "updates": {"memo": "a"}}),
            json!({"id": "R-001", "expected_hash": "x"}),
            json!({"id": "R-001", "expected_hash": "x", "updates": {}}),
        ] {
            assert!(handle_update(&ctx, &args).is_err(), "{args}");
        }
        assert_eq!(
            std::fs::read(ctx.handoff_dir.join("review_board.md")).unwrap(),
            before
        );
    }

    #[test]
    fn update_round_trip_returns_the_hash_a_read_reports() {
        let (_d, ctx) = ctx();
        let hash = read(&ctx, json!({"id": "R-002"}))["content_hash"]
            .as_str()
            .unwrap()
            .to_string();
        let out: Value = serde_json::from_str(
            &handle_update(
                &ctx,
                &json!({"id": "R-002", "expected_hash": hash, "updates": {"checkbox_checked": true, "memo": "ok"}}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(out["status"], "answered");
        assert_eq!(out["updated"], json!(["checkbox_checked", "memo"]));
        let after = read(&ctx, json!({"id": "R-002"}));
        assert_eq!(after["content_hash"], out["content_hash"]);
        assert_eq!(after["memo"], "ok");
        assert_eq!(read(&ctx, json!({"id": "R-001"}))["body"], "Body one.");
    }

    #[test]
    fn stale_hash_error_carries_the_current_hash() {
        let (_d, ctx) = ctx();
        let current = read(&ctx, json!({"id": "R-001"}))["content_hash"]
            .as_str()
            .unwrap()
            .to_string();
        let err = handle_update(
            &ctx,
            &json!({"id": "R-001", "expected_hash": "deadbeef", "updates": {"memo": "x"}}),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("expected_hash mismatch"), "{err}");
        assert!(err.contains(&current), "{err}");
    }

    #[test]
    fn read_and_update_report_warnings_for_a_mixed_board_and_a_broken_id() {
        let (_d, ctx) = ctx();
        let path = ctx.handoff_dir.join("review_board.md");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("\n### Hand typed\r\n");
        std::fs::write(&path, text).unwrap();

        let v = read(&ctx, json!({"id": "R-001"}));
        let warnings = v["warnings"].as_array().unwrap();
        assert!(warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("LF and CRLF")));
        assert!(warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("Hand typed")));

        let hash = v["content_hash"].as_str().unwrap().to_string();
        let out: Value = serde_json::from_str(
            &handle_update(
                &ctx,
                &json!({"id": "R-001", "expected_hash": hash, "updates": {"memo": "ok"}}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(out["warnings"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_clean_board_returns_an_empty_warnings_array() {
        let (_d, ctx) = ctx();
        assert_eq!(read(&ctx, json!({"id": "R-001"}))["warnings"], json!([]));
    }
}
