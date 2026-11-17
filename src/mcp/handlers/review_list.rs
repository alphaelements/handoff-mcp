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
    });
    serde_json::to_string_pretty(&result).map_err(Into::into)
}
