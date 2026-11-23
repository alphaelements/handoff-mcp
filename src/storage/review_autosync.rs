//! Automatic Review Board sync for task status changes (FR-RB-001, FR-RB-002).
//!
//! `handoff_update_task` calls [`on_status_change`] after a task's status has
//! been written:
//!
//! - a change **to** `review` adds one item to the [`AUTO_CATEGORY`] section
//!   of `review_board.md` (creating the board when it is missing), carrying a
//!   `- task_id:` attribute and the task's unchecked `done_criteria`;
//! - a change **away from** `review` resolves that item into the decisions
//!   archive: `done` -> `approved`, `in_progress` -> `rework`, anything else ->
//!   `superseded`.
//!
//! The item is found by its `task_id` attribute inside [`AUTO_CATEGORY`], so a
//! second move to `review` never duplicates a pending item, and an item the
//! user already resolved by hand (it left the board for the decisions file) is
//! simply not found and left alone. A task that comes back to `review` after
//! its item was resolved gets a fresh item.

use std::path::Path;

use anyhow::Result;
use chrono::NaiveDate;

use super::review_board::{add_item, board_path, read_optional, NewReviewItem};
use super::review_board_items::parse_items;
use super::review_decisions::resolve_item;
use super::tasks::DoneCriterion;

/// Category of the items this module creates and resolves.
pub const AUTO_CATEGORY: &str = "Task Review";

/// The status the Review Board follows.
const REVIEW_STATUS: &str = "review";

/// What an [`on_status_change`] call did, for the `handoff_update_task` reply.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncOutcome {
    /// What was added or resolved (one line each), for logs and tests.
    pub notes: Vec<String>,
    /// Non-fatal findings about the board (mixed line endings, items with a
    /// missing id); the sync still happened. The caller shows these.
    pub warnings: Vec<String>,
}

/// The decision text recorded when a task leaves `review` for `new_status`.
pub fn resolution_for(new_status: &str) -> &'static str {
    match new_status {
        "done" => "approved",
        "in_progress" => "rework",
        _ => "superseded",
    }
}

/// Syncs the board with the status change `from` -> `to` of task `task_id`.
///
/// Does nothing when neither side is `review`. Errors are the caller's to
/// downgrade to a warning: the status change is already committed.
pub fn on_status_change(
    handoff_dir: &Path,
    task_id: &str,
    title: &str,
    criteria: &[DoneCriterion],
    from: &str,
    to: &str,
    date: NaiveDate,
) -> Result<SyncOutcome> {
    if from == to {
        return Ok(SyncOutcome::default());
    }
    if to == REVIEW_STATUS {
        return add_for_task(handoff_dir, task_id, title, criteria, date);
    }
    if from == REVIEW_STATUS {
        return resolve_for_task(handoff_dir, task_id, to, date);
    }
    Ok(SyncOutcome::default())
}

/// Ids of the pending auto items of `task_id` on the board (none when the
/// board does not exist).
fn pending_item_ids(handoff_dir: &Path, task_id: &str) -> Result<Vec<String>> {
    let text = read_optional(&board_path(handoff_dir))?;
    Ok(parse_items(&text)
        .into_iter()
        .filter(|i| i.category.as_deref() == Some(AUTO_CATEGORY))
        .filter(|i| i.task_id.as_deref() == Some(task_id))
        .filter_map(|i| i.id)
        .collect())
}

fn add_for_task(
    handoff_dir: &Path,
    task_id: &str,
    title: &str,
    criteria: &[DoneCriterion],
    date: NaiveDate,
) -> Result<SyncOutcome> {
    let existing = pending_item_ids(handoff_dir, task_id)?;
    if let Some(id) = existing.first() {
        return Ok(SyncOutcome {
            notes: vec![format!(
                "review_board: {id} already tracks {task_id}; no item added"
            )],
            ..Default::default()
        });
    }

    let body = render_body(task_id, criteria);
    let title = single_line_title(title, task_id);
    let added = add_item(
        handoff_dir,
        &NewReviewItem {
            title: &title,
            category: AUTO_CATEGORY,
            body: Some(&body),
            task_id: Some(task_id),
            ..Default::default()
        },
        date,
    )?;
    let notes = vec![format!(
        "review_board: added {} for {task_id} ({AUTO_CATEGORY})",
        added.id
    )];
    Ok(SyncOutcome {
        notes,
        warnings: added.warnings,
    })
}

fn resolve_for_task(
    handoff_dir: &Path,
    task_id: &str,
    to: &str,
    date: NaiveDate,
) -> Result<SyncOutcome> {
    let resolution = resolution_for(to);
    let decision = format!("resolution: {resolution} (task {task_id}: review -> {to})");
    let mut notes = Vec::new();
    let mut warnings = Vec::new();
    for id in pending_item_ids(handoff_dir, task_id)? {
        let resolved = resolve_item(handoff_dir, &id, &decision, date)?;
        notes.push(format!(
            "review_board: resolved {} as {resolution}",
            resolved.id
        ));
        warnings.extend(resolved.warnings);
    }
    Ok(SyncOutcome { notes, warnings })
}

/// Item body: the unchecked criteria as plain bullets (not checkboxes, which
/// the board reserves for the item's own confirmation line).
fn render_body(task_id: &str, criteria: &[DoneCriterion]) -> String {
    let unchecked: Vec<String> = criteria
        .iter()
        .filter(|c| !c.checked)
        .map(|c| fold_whitespace(&c.item))
        .filter(|c| !c.is_empty())
        .collect();
    if unchecked.is_empty() {
        return format!("Task {task_id} moved to review. No unchecked done_criteria.");
    }
    let mut body = format!("Task {task_id} moved to review. Unchecked done_criteria:\n");
    for criterion in unchecked {
        body.push_str(&format!("\n- {criterion}"));
    }
    body
}

/// A task title as a board item title: one line, no HTML comment marker, and
/// never empty (the board rejects those).
fn single_line_title(title: &str, task_id: &str) -> String {
    let folded = fold_whitespace(title)
        .replace("<!--", "<!- -")
        .replace("-->", "- ->");
    if folded.is_empty() {
        task_id.to_string()
    } else {
        folded
    }
}

fn fold_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::review_board::{init_board, DECISIONS_FILE};
    use crate::storage::review_decisions::decisions_path;

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 23).unwrap()
    }

    fn setup() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let h = dir.path().join(".handoff");
        std::fs::create_dir_all(&h).unwrap();
        (dir, h)
    }

    fn crit(item: &str, checked: bool) -> DoneCriterion {
        DoneCriterion {
            item: item.to_string(),
            checked,
        }
    }

    fn board(h: &Path) -> String {
        std::fs::read_to_string(board_path(h)).unwrap()
    }

    fn decisions(h: &Path) -> String {
        std::fs::read_to_string(decisions_path(h)).unwrap_or_default()
    }

    fn to_review(h: &Path, id: &str, criteria: &[DoneCriterion]) -> SyncOutcome {
        on_status_change(
            h,
            id,
            "Fix the thing",
            criteria,
            "in_progress",
            "review",
            date(),
        )
        .unwrap()
    }

    fn leave_review(h: &Path, id: &str, to: &str) -> SyncOutcome {
        on_status_change(h, id, "Fix the thing", &[], "review", to, date()).unwrap()
    }

    #[test]
    fn review_creates_the_board_and_adds_an_item_with_task_id_and_unchecked_criteria() {
        let (_d, h) = setup();
        assert!(!board_path(&h).exists());
        let outcome = to_review(
            &h,
            "t1",
            &[crit("tests pass", false), crit("lint ok", true)],
        );
        assert!(outcome.notes[0].contains("R-001"), "{:?}", outcome.notes);
        let text = board(&h);
        assert!(text.contains("## Task Review"));
        assert!(text.contains("### Fix the thing <!-- rb-id: R-001 -->"));
        assert!(text.contains("- task_id: t1"));
        assert!(text.contains("- tests pass"));
        assert!(!text.contains("lint ok"), "checked criteria are omitted");
    }

    #[test]
    fn review_reuses_an_existing_board_and_keeps_its_items() {
        let (_d, h) = setup();
        init_board(&h).unwrap();
        add_item(
            &h,
            &NewReviewItem {
                title: "Manual",
                category: "その他",
                ..Default::default()
            },
            date(),
        )
        .unwrap();
        to_review(&h, "t1", &[]);
        let text = board(&h);
        assert!(text.contains("### Manual <!-- rb-id: R-001 -->"));
        assert!(text.contains("### Fix the thing <!-- rb-id: R-002 -->"));
        assert!(text.contains("No unchecked done_criteria"));
    }

    #[test]
    fn second_review_transition_does_not_duplicate_the_item() {
        let (_d, h) = setup();
        to_review(&h, "t1", &[]);
        let outcome = to_review(&h, "t1", &[]);
        assert!(outcome.notes[0].contains("already tracks"));
        assert_eq!(board(&h).matches("task_id: t1").count(), 1);
    }

    #[test]
    fn different_tasks_each_get_their_own_item() {
        let (_d, h) = setup();
        to_review(&h, "t1", &[]);
        to_review(&h, "t2", &[]);
        let text = board(&h);
        assert_eq!(text.matches("task_id: t1").count(), 1);
        assert_eq!(text.matches("task_id: t2").count(), 1);
    }

    #[test]
    fn a_task_id_that_is_a_prefix_of_another_is_not_confused() {
        let (_d, h) = setup();
        to_review(&h, "t10", &[]);
        let outcome = to_review(&h, "t1", &[]);
        assert!(outcome.notes[0].contains("added"), "{:?}", outcome.notes);
        assert_eq!(board(&h).matches("### Fix the thing").count(), 2);
    }

    #[test]
    fn review_to_done_resolves_as_approved() {
        let (_d, h) = setup();
        to_review(&h, "t1", &[]);
        let outcome = leave_review(&h, "t1", "done");
        assert!(outcome.notes[0].contains("approved"), "{:?}", outcome.notes);
        assert!(!board(&h).contains("task_id: t1"));
        let dec = decisions(&h);
        assert!(dec.contains("original_id: R-001"));
        assert!(dec.contains("resolution: approved"));
    }

    #[test]
    fn review_to_in_progress_resolves_as_rework() {
        let (_d, h) = setup();
        to_review(&h, "t1", &[]);
        leave_review(&h, "t1", "in_progress");
        assert!(decisions(&h).contains("resolution: rework"));
    }

    #[test]
    fn review_to_other_statuses_resolve_as_superseded() {
        for to in ["todo", "blocked", "skipped"] {
            let (_d, h) = setup();
            to_review(&h, "t1", &[]);
            leave_review(&h, "t1", to);
            assert!(decisions(&h).contains("resolution: superseded"), "{to}");
        }
    }

    #[test]
    fn manually_resolved_item_is_not_overwritten() {
        let (_d, h) = setup();
        to_review(&h, "t1", &[]);
        resolve_item(&h, "R-001", "Manual verdict: ship it", date()).unwrap();
        let before = decisions(&h);
        let outcome = leave_review(&h, "t1", "in_progress");
        assert!(outcome.notes.is_empty(), "{:?}", outcome.notes);
        assert_eq!(decisions(&h), before);
        assert!(!decisions(&h).contains("resolution: rework"));
    }

    #[test]
    fn leaving_review_without_a_board_or_item_is_a_no_op() {
        let (_d, h) = setup();
        let outcome = leave_review(&h, "t1", "done");
        assert!(outcome.notes.is_empty());
        assert!(!board_path(&h).exists());
        assert!(!h.join(DECISIONS_FILE).exists());
    }

    #[test]
    fn leaving_review_only_resolves_the_matching_task() {
        let (_d, h) = setup();
        to_review(&h, "t1", &[]);
        to_review(&h, "t2", &[]);
        leave_review(&h, "t1", "done");
        let text = board(&h);
        assert!(!text.contains("task_id: t1"));
        assert!(text.contains("task_id: t2"));
    }

    #[test]
    fn re_entering_review_after_resolution_adds_a_fresh_item() {
        let (_d, h) = setup();
        to_review(&h, "t1", &[]);
        leave_review(&h, "t1", "in_progress");
        let outcome = to_review(&h, "t1", &[]);
        assert!(outcome.notes[0].contains("R-002"), "{:?}", outcome.notes);
    }

    #[test]
    fn items_in_other_categories_with_the_same_task_id_are_left_alone() {
        let (_d, h) = setup();
        add_item(
            &h,
            &NewReviewItem {
                title: "Hand made",
                category: "設計判断",
                task_id: Some("t1"),
                ..Default::default()
            },
            date(),
        )
        .unwrap();
        let outcome = to_review(&h, "t1", &[]);
        assert!(outcome.notes[0].contains("added"));
        leave_review(&h, "t1", "done");
        assert!(board(&h).contains("### Hand made"));
    }

    #[test]
    fn transitions_not_involving_review_do_nothing() {
        let (_d, h) = setup();
        for (from, to) in [
            ("todo", "in_progress"),
            ("in_progress", "done"),
            ("review", "review"),
        ] {
            let outcome = on_status_change(&h, "t1", "T", &[], from, to, date()).unwrap();
            assert!(outcome.notes.is_empty());
        }
        assert!(!board_path(&h).exists());
    }

    #[test]
    fn awkward_titles_and_criteria_still_produce_a_valid_item() {
        let (_d, h) = setup();
        on_status_change(
            &h,
            "t1",
            "multi\nline <!-- evil --> title",
            &[crit("# not a heading\nsecond line", false)],
            "todo",
            "review",
            date(),
        )
        .unwrap();
        let text = board(&h);
        assert!(text.contains("### multi line <!- - evil - -> title <!-- rb-id: R-001 -->"));
        assert!(text.contains("- # not a heading second line"));
        let items = parse_items(&text);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].task_id.as_deref(), Some("t1"));
    }

    #[test]
    fn empty_title_falls_back_to_the_task_id() {
        let (_d, h) = setup();
        on_status_change(&h, "t9", "  ", &[], "todo", "review", date()).unwrap();
        assert!(board(&h).contains("### t9 <!-- rb-id: R-001 -->"));
    }

    #[test]
    fn resolution_mapping() {
        assert_eq!(resolution_for("done"), "approved");
        assert_eq!(resolution_for("in_progress"), "rework");
        assert_eq!(resolution_for("todo"), "superseded");
    }
}
