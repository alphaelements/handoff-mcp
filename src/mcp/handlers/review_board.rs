//! `handoff_review_init` and `handoff_review_add` tools (RB-001, RB-002): the
//! write side that creates the User Review Board (`.handoff/review_board.md`)
//! and appends items to it.

use anyhow::{bail, Result};
use serde_json::{json, Value};

use super::HandlerContext;
use crate::storage::review_board::{add_item, board_path, init_board, NewReviewItem, BOARD_FILE};

/// Reads an optional string argument; present-but-not-a-string is an error so
/// a mistyped value is not silently dropped.
fn optional_str<'a>(arguments: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => bail!("'{key}' must be a string"),
    }
}

fn required_str<'a>(arguments: &'a Value, key: &str) -> Result<&'a str> {
    optional_str(arguments, key)?.ok_or_else(|| anyhow::anyhow!("'{key}' parameter is required"))
}

fn string_array<'a>(arguments: &'a Value, key: &str) -> Result<Vec<&'a str>> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .ok_or_else(|| anyhow::anyhow!("'{key}' must be an array of strings"))
            })
            .collect(),
        Some(_) => bail!("'{key}' must be an array of strings"),
    }
}

/// `handoff_review_init`: creates the board with the default categories when
/// it does not exist; an existing board is left untouched.
pub fn handle_init(ctx: &HandlerContext, _arguments: &Value) -> Result<String> {
    let created = init_board(&ctx.handoff_dir)?;
    let path = board_path(&ctx.handoff_dir);
    let result = json!({
        "created": created,
        "path": format!(".handoff/{BOARD_FILE}"),
        "exists": path.exists(),
    });
    serde_json::to_string_pretty(&result).map_err(Into::into)
}

/// `handoff_review_add`: appends one item to its category (created when it
/// does not exist yet) and returns the new stable id.
pub fn handle_add(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let title = required_str(arguments, "title")?;
    let category = required_str(arguments, "category")?;
    let image_paths: Vec<String> = string_array(arguments, "image_paths")?
        .into_iter()
        .map(str::to_string)
        .collect();

    let item = NewReviewItem {
        title,
        category,
        body: optional_str(arguments, "body")?,
        priority: optional_str(arguments, "priority")?,
        assignee: optional_str(arguments, "assignee")?,
        task_id: optional_str(arguments, "task_id")?,
        doc_id: optional_str(arguments, "doc_id")?,
        image_paths: &image_paths,
    };
    let added = add_item(&ctx.handoff_dir, &item, chrono::Utc::now().date_naive())?;

    let result = json!({
        "id": added.id,
        "category": added.category,
        "category_created": added.category_created,
        "board_created": added.board_created,
        "path": format!(".handoff/{BOARD_FILE}"),
    });
    serde_json::to_string_pretty(&result).map_err(Into::into)
}
