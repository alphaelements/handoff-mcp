//! `handoff_review_list` tool (RB-005): a status summary of every item on the
//! User Review Board (`.handoff/review_board.md`).
//!
//! Read-only: the board is parsed, never written, and a missing board file is
//! an empty result rather than an error (nothing is created).

use anyhow::{bail, Result};
use serde_json::{json, Value};

use super::HandlerContext;
use crate::storage::review_board::board_path;
use crate::storage::review_board_items::{parse_items, ReviewItem, ReviewStatus};
use crate::storage::review_scan::{board_warnings, normalize_mixed_line_endings};

/// Reads an optional string argument; present-but-not-a-string is an error so
/// a mistyped filter is not silently ignored.
fn optional_str<'a>(arguments: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => bail!("'{key}' must be a string"),
    }
}

fn item_json(item: &ReviewItem) -> Value {
    json!({
        "id": item.id,
        "title": item.title,
        "status": item.status,
        "priority": item.priority,
        "assignee": item.assignee,
        "date": item.date,
        "has_images": item.has_images,
        "has_comparison": item.has_comparison(),
        "expected_image": item.expected_image,
        "actual_image": item.actual_image,
        "diff_image": item.diff_image,
        "similarity_pct": item.similarity_pct,
        "threshold": item.threshold,
        "category": item.category,
    })
}

pub fn handle(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let status_filter = optional_str(arguments, "status_filter")?
        .map(ReviewStatus::parse)
        .transpose()?;
    let assignee_filter = optional_str(arguments, "assignee_filter")?;
    let category_filter = optional_str(arguments, "category_filter")?.map(str::trim);

    let path = board_path(&ctx.handoff_dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        // No board yet is a normal state (SPEC-RB-013): report it as empty.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(anyhow::Error::new(e).context(format!("Failed to read {}", path.display())))
        }
    };

    // Same LF view as the readers and writers of a single section.
    let warnings = board_warnings(&text);
    let text = normalize_mixed_line_endings(&text);

    let items: Vec<ReviewItem> = parse_items(&text)
        .into_iter()
        .filter(|i| status_filter.is_none_or(|s| i.status == s))
        .filter(|i| assignee_filter.is_none_or(|a| i.assignee.as_deref() == Some(a)))
        .filter(|i| category_filter.is_none_or(|c| i.category.as_deref() == Some(c)))
        .collect();

    let count = |s: ReviewStatus| items.iter().filter(|i| i.status == s).count();
    let result = json!({
        "items": items.iter().map(item_json).collect::<Vec<_>>(),
        "summary": {
            "total": items.len(),
            "pending": count(ReviewStatus::Pending),
            "answered": count(ReviewStatus::Answered),
        },
        "warnings": warnings,
    });
    serde_json::to_string_pretty(&result).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with(board: Option<&str>) -> (tempfile::TempDir, HandlerContext) {
        let dir = tempfile::tempdir().unwrap();
        let handoff_dir = dir.path().join(".handoff");
        std::fs::create_dir_all(&handoff_dir).unwrap();
        if let Some(board) = board {
            std::fs::write(board_path(&handoff_dir), board).unwrap();
        }
        let ctx = HandlerContext {
            agent_id: None,
            project_dir: dir.path().to_path_buf(),
            handoff_dir,
        };
        (dir, ctx)
    }

    fn list(ctx: &HandlerContext) -> Value {
        serde_json::from_str(&handle(ctx, &json!({})).unwrap()).unwrap()
    }

    #[test]
    fn a_missing_id_is_warned_and_the_other_items_are_still_listed() {
        let (_d, ctx) = ctx_with(Some(
            "## C\n\n### A <!-- rb-id: R-001 -->\n\n### By hand\n\n### B <!-- rb-id: R-003 -->\n",
        ));
        let v = list(&ctx);
        assert_eq!(v["items"].as_array().unwrap().len(), 3);
        let warnings = v["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].as_str().unwrap().contains("By hand"));
    }

    #[test]
    fn an_intact_board_and_a_missing_board_list_without_warnings() {
        let (_d, ctx) = ctx_with(Some("## C\n\n### A <!-- rb-id: R-001 -->\n"));
        assert_eq!(list(&ctx)["warnings"], json!([]));
        let (_d2, ctx2) = ctx_with(None);
        assert_eq!(list(&ctx2)["warnings"], json!([]));
    }

    #[test]
    fn a_mixed_line_ending_board_is_listed_with_a_warning() {
        let (_d, ctx) = ctx_with(Some("## C\r\n\n### A <!-- rb-id: R-001 -->\n"));
        let v = list(&ctx);
        assert_eq!(v["items"].as_array().unwrap().len(), 1);
        assert!(v["warnings"][0].as_str().unwrap().contains("LF and CRLF"));
    }
}
