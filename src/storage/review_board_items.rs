//! Item listing for the User Review Board (`.handoff/review_board.md`).
//!
//! The file layout (owned by [`super::review_board`]) is a plain Markdown
//! file: `##` headings are categories and `###` headings are items. This
//! module turns that layout into [`ReviewItem`]s for `handoff_review_list`.
//! It is read-only and never touches the file.
//!
//! There is deliberately no scanner here: items are located by
//! [`super::review_section::index_sections`] and parsed by
//! [`super::review_section::parse_item`], the same code `handoff_review_read`
//! and `handoff_review_update` use. List and read therefore cannot disagree on
//! an item's status, memo or attributes, however a hand-edited board looks.
//!
//! [`ReviewStatus`] and [`ReviewItem`] are the shared item types; all readers
//! use [`detect_status`] so the answered/pending rule is defined once.

use anyhow::{bail, Result};
use serde::Serialize;
use serde_json::Number;

use super::review_section::{index_sections, parse_item, ReviewSection};

/// Review state of an item.
///
/// `Resolved` items are moved to the decisions file, so parsing
/// `review_board.md` never produces it; it exists so the filter vocabulary
/// and the answer/resolve tools share one enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewStatus {
    Pending,
    Answered,
    Resolved,
}

impl ReviewStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewStatus::Pending => "pending",
            ReviewStatus::Answered => "answered",
            ReviewStatus::Resolved => "resolved",
        }
    }

    /// Parses a user-facing status name. Errors list the accepted values so a
    /// misspelled filter is not silently read as "no filter".
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "pending" => Ok(ReviewStatus::Pending),
            "answered" => Ok(ReviewStatus::Answered),
            "resolved" => Ok(ReviewStatus::Resolved),
            other => bail!("Invalid status '{other}': expected one of pending, answered, resolved"),
        }
    }
}

/// One `###` item parsed from the board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewItem {
    /// Stable id (`R-001`) from the heading comment; `None` for a heading a
    /// user typed by hand without one.
    pub id: Option<String>,
    pub title: String,
    pub status: ReviewStatus,
    pub priority: Option<String>,
    pub assignee: Option<String>,
    pub date: Option<String>,
    pub task_id: Option<String>,
    pub doc_id: Option<String>,
    /// Image comparison attributes (RB-011), as [`ReviewSection`] reads them.
    pub expected_image: Option<String>,
    pub actual_image: Option<String>,
    pub diff_image: Option<String>,
    pub similarity_pct: Option<Number>,
    pub threshold: Option<Number>,
    /// Text of the enclosing `##` heading; `None` before the first one.
    pub category: Option<String>,
    /// The item contains a Markdown image link outside code fences.
    pub has_images: bool,
    /// The item's own checkbox (`- [x] 確認済み`) is checked.
    pub checkbox_checked: bool,
    /// Text the user wrote after `メモ:` (trimmed; empty when none).
    pub memo: String,
}

impl ReviewItem {
    /// The item carries both the expected and the actual image, the pair the
    /// compare view needs. The other comparison attributes alone do not
    /// count, and the Markdown image links of `has_images` are unrelated.
    pub fn has_comparison(&self) -> bool {
        self.expected_image.is_some() && self.actual_image.is_some()
    }
}

/// The status rule shared by every reader: an item is `Answered` when its
/// checkbox is checked or its memo has content, otherwise `Pending`.
pub fn detect_status(checkbox_checked: bool, memo: &str) -> ReviewStatus {
    if checkbox_checked || !memo.trim().is_empty() {
        ReviewStatus::Answered
    } else {
        ReviewStatus::Pending
    }
}

/// Parses every `###` item of a board in file order.
///
/// Sections come from [`index_sections`] and each one is read by
/// [`parse_item`], the same code `handoff_review_read` uses, so list and read
/// can never disagree about an item.
pub fn parse_items(text: &str) -> Vec<ReviewItem> {
    index_sections(text)
        .iter()
        .map(|span| ReviewItem::from(parse_item(&text[span.start..span.end], span)))
        .collect()
}

/// Longest `answer_summary`, counted in Unicode characters so Japanese text
/// is never cut inside a character.
pub const ANSWER_SUMMARY_MAX_CHARS: usize = 100;

/// `answer_summary` of `load_context` for an answered item: the first
/// [`ANSWER_SUMMARY_MAX_CHARS`] characters of the memo, or `checked` when the
/// item is answered by its checkbox alone.
pub fn answer_summary(item: &ReviewItem) -> String {
    if item.memo.trim().is_empty() {
        "checked".to_string()
    } else {
        item.memo.chars().take(ANSWER_SUMMARY_MAX_CHARS).collect()
    }
}

/// An answered item as listed in `handoff_load_context`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AnsweredItem {
    pub id: Option<String>,
    pub title: String,
    pub answer_summary: String,
}

/// Board counts and the answered items, for `handoff_load_context`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BoardSummary {
    pub pending_count: usize,
    pub answered_count: usize,
    /// Answered items in file order. Resolved items live in the decisions
    /// file, so every answered item still on the board is unresolved.
    pub answered_items: Vec<AnsweredItem>,
}

/// Summarizes a board text using the same parse as `handoff_review_list`.
pub fn summarize_board(text: &str) -> BoardSummary {
    let items = parse_items(text);
    let pending_count = items
        .iter()
        .filter(|i| i.status == ReviewStatus::Pending)
        .count();
    let answered_items: Vec<AnsweredItem> = items
        .iter()
        .filter(|i| i.status == ReviewStatus::Answered)
        .map(|i| AnsweredItem {
            id: i.id.clone(),
            title: i.title.clone(),
            answer_summary: answer_summary(i),
        })
        .collect();
    BoardSummary {
        pending_count,
        answered_count: answered_items.len(),
        answered_items,
    }
}

impl From<ReviewSection> for ReviewItem {
    fn from(section: ReviewSection) -> Self {
        ReviewItem {
            id: section.id,
            title: section.title,
            status: section.status,
            priority: section.priority,
            assignee: section.assignee,
            date: section.date,
            task_id: section.task_id,
            doc_id: section.doc_id,
            expected_image: section.expected_image,
            actual_image: section.actual_image,
            diff_image: section.diff_image,
            similarity_pct: section.similarity_pct,
            threshold: section.threshold,
            category: Some(section.category).filter(|c| !c.is_empty()),
            has_images: !section.images.is_empty(),
            checkbox_checked: section.checkbox_checked,
            memo: section.memo,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(text: &str) -> ReviewItem {
        let mut items = parse_items(text);
        assert_eq!(items.len(), 1, "expected one item in:\n{text}");
        items.remove(0)
    }

    #[test]
    fn heading_with_id_comment_splits_title_and_id() {
        let it = one("## C\n### Pick one <!-- rb-id: R-012 -->\n");
        assert_eq!(it.id.as_deref(), Some("R-012"));
        assert_eq!(it.title, "Pick one");
        assert_eq!(it.category.as_deref(), Some("C"));
    }

    #[test]
    fn heading_without_id_has_none_and_keeps_other_comments_in_the_title() {
        let it = one("### Hand typed\n");
        assert_eq!(it.id, None);
        assert_eq!(it.title, "Hand typed");
        let it = one("### Has <!-- note --> comment\n");
        assert_eq!(it.id, None);
        assert_eq!(it.title, "Has <!-- note --> comment");
        assert_eq!(it.category, None);
    }

    #[test]
    fn attributes_are_read_from_the_block_under_the_heading_only() {
        let it = one(
            "### T <!-- rb-id: R-001 -->\n- priority: high\n- assignee: ryoma\n- task_id: t-1\n- doc_id: d-1\n- date: 2026-11-17\n\nbody\n- priority: low\n",
        );
        assert_eq!(it.priority.as_deref(), Some("high"));
        assert_eq!(it.assignee.as_deref(), Some("ryoma"));
        assert_eq!(it.task_id.as_deref(), Some("t-1"));
        assert_eq!(it.doc_id.as_deref(), Some("d-1"));
        assert_eq!(it.date.as_deref(), Some("2026-11-17"));
    }

    #[test]
    fn empty_attribute_value_is_none_and_user_edits_are_picked_up() {
        let it = one("### T <!-- rb-id: R-001 -->\n- priority:\n- assignee: bob\n");
        assert_eq!(it.priority, None);
        assert_eq!(it.assignee.as_deref(), Some("bob"));
    }

    #[test]
    fn status_rules() {
        let pending = one("### T\n- [ ] 確認済み\n\nメモ:\n");
        assert_eq!(pending.status, ReviewStatus::Pending);
        let checked = one("### T\n- [x] 確認済み\n\nメモ:\n");
        assert_eq!(checked.status, ReviewStatus::Answered);
        assert!(checked.checkbox_checked);
        let upper = one("### T\n- [X] 確認済み\nメモ:\n");
        assert_eq!(upper.status, ReviewStatus::Answered);
        let memo = one("### T\n- [ ] 確認済み\n\nメモ: ok by me\n");
        assert_eq!(memo.status, ReviewStatus::Answered);
        assert_eq!(memo.memo, "ok by me");
        let whitespace_memo = one("### T\n- [ ] 確認済み\nメモ:   \n");
        assert_eq!(whitespace_memo.status, ReviewStatus::Pending);
    }

    #[test]
    fn fullwidth_colon_memo_and_multiline_memo() {
        let it = one("### T\n- [ ] 確認済み\n\nメモ：first\nsecond line\n");
        assert_eq!(it.memo, "first\nsecond line");
        assert_eq!(it.status, ReviewStatus::Answered);
    }

    #[test]
    fn body_task_list_is_not_the_items_checkbox() {
        let it = one("### T\n- [x] body task\n- [ ] 確認済み\n\nメモ:\n");
        assert!(!it.checkbox_checked);
        assert_eq!(it.status, ReviewStatus::Pending);
    }

    #[test]
    fn item_without_checkbox_or_memo_is_pending() {
        assert_eq!(one("### T\n").status, ReviewStatus::Pending);
    }

    #[test]
    fn images_are_detected_outside_fences_only() {
        assert!(one("### T\n![a](x.png)\n").has_images);
        assert!(one("### T\ntext ![a](<my x.png>) more\n").has_images);
        assert!(!one("### T\nno image ![ not a link\n").has_images);
        assert!(!one("### T\n```\n![a](x.png)\n```\n").has_images);
    }

    #[test]
    fn fenced_headings_are_body_and_tilde_fences_work() {
        let items = parse_items("## C\n### A <!-- rb-id: R-001 -->\n~~~\n### x\n## y\n~~~\n### B <!-- rb-id: R-002 -->\n");
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].category.as_deref(), Some("C"));
    }

    #[test]
    fn categories_apply_to_following_items_in_file_order() {
        let items = parse_items("# Board\n## A\n### 1\n### 2\n## B\n### 3\n");
        let cats: Vec<_> = items.iter().map(|i| i.category.clone().unwrap()).collect();
        assert_eq!(cats, ["A", "A", "B"]);
        let titles: Vec<_> = items.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, ["1", "2", "3"]);
    }

    #[test]
    fn deeper_headings_stay_in_the_item_and_crlf_is_handled() {
        let items = parse_items(
            "## C\r\n### A <!-- rb-id: R-001 -->\r\n#### sub\r\n- [x] 確認済み\r\nメモ:\r\n",
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].status, ReviewStatus::Answered);
        assert_eq!(items[0].title, "A");
    }

    #[test]
    fn empty_and_headingless_text_has_no_items() {
        assert!(parse_items("").is_empty());
        assert!(parse_items("just text\n- [x] 確認済み\n").is_empty());
    }

    #[test]
    fn memo_line_in_the_body_is_not_the_items_memo() {
        // The item's footer is its own checkbox and the memo after it; a body
        // line that merely starts with the memo marker must not answer it.
        let text = "## C\n### T <!-- rb-id: R-001 -->\n- date: 2026-11-17\n\nhello\nメモ: sneaky body line\n\n- [ ] 確認済み\n\nメモ:\n";
        let it = one(text);
        assert_eq!(it.status, ReviewStatus::Pending);
        assert_eq!(it.memo, "");
    }

    #[test]
    fn list_and_read_agree_on_status_for_awkward_bodies() {
        use crate::storage::review_section::{index_sections, parse_item};
        let boards = [
            "## C\n### A <!-- rb-id: R-001 -->\n- date: d\n\nhello\nメモ: sneaky\n\n- [ ] 確認済み\n\nメモ:\n",
            "## C\n### A <!-- rb-id: R-001 -->\n- date: d\n\n- [x] body task\n\n- [ ] 確認済み\n\nメモ:\n",
            "## C\n### A <!-- rb-id: R-001 -->\n\n- priority: high\n\n- [ ] 確認済み\nメモ: done\n",
            "# top\n### A <!-- rb-id: R-001 -->\nbody\n# Level one\n- [x] 確認済み\n",
        ];
        for text in boards {
            let spans = index_sections(text);
            let listed = parse_items(text);
            assert_eq!(spans.len(), listed.len(), "{text}");
            for (span, item) in spans.iter().zip(&listed) {
                let read = parse_item(&text[span.start..span.end], span);
                assert_eq!(read.status, item.status, "status differs in:\n{text}");
                assert_eq!(read.memo, item.memo, "memo differs in:\n{text}");
                assert_eq!(read.priority, item.priority, "priority differs in:\n{text}");
            }
        }
    }

    #[test]
    fn a_level_one_heading_ends_the_item_so_its_text_is_not_the_memo() {
        let items = parse_items("## C\n### A\n- [ ] 確認済み\n\nメモ:\n\n# Appendix\ntext\n");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].memo, "");
        assert_eq!(items[0].status, ReviewStatus::Pending);
    }

    #[test]
    fn list_and_read_agree_on_a_hand_edited_board() {
        let text = "\
## C

### One <!-- rb-id: R-001 -->

- priority: high
- [x] body task
- [ ] 確認済み

メモ: done
- [x] memo task

### Two <!-- rb-id: R-002 -->
```
メモ: fenced
```
- [x] 確認済み
";
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(super::super::review_board::board_path(dir.path()), text).unwrap();
        for item in parse_items(text) {
            let id = item.id.clone().unwrap();
            let read = super::super::review_section::read_item(
                dir.path(),
                super::super::review_section::SectionRef::Id(&id),
            )
            .unwrap();
            assert_eq!(read.status, item.status, "{id}");
            assert_eq!(read.memo, item.memo, "{id}");
            assert_eq!(read.checkbox_checked, item.checkbox_checked, "{id}");
            assert_eq!(read.priority, item.priority, "{id}");
        }
        let items = parse_items(text);
        assert_eq!(
            items[0].priority, None,
            "blank line ends the attribute block"
        );
        assert!(!items[0].checkbox_checked);
        assert_eq!(items[0].memo, "done\n- [x] memo task");
        assert!(items[1].checkbox_checked);
    }

    #[test]
    fn answer_detection_edge_cases() {
        // Single character memo is content; tab-only memo is empty.
        assert_eq!(
            one("### T\n- [ ] 確認済み\nメモ: y\n").status,
            ReviewStatus::Answered
        );
        assert_eq!(
            one("### T\n- [ ] 確認済み\nメモ:\t \t\n").status,
            ReviewStatus::Pending
        );
        // [x] with an empty memo: answered, memo empty.
        let it = one("### T\n- [x] 確認済み\nメモ:\n");
        assert_eq!(it.status, ReviewStatus::Answered);
        assert_eq!(it.memo, "");
        // [x] together with a memo.
        assert_eq!(
            one("### T\n- [x] 確認済み\nメモ: ok\n").status,
            ReviewStatus::Answered
        );
    }

    #[test]
    fn checkbox_in_body_or_fence_or_neighbour_does_not_answer() {
        let items = parse_items(
            "## C\n### A <!-- rb-id: R-001 -->\n- [x] sample step\n```\n- [x] fenced\n```\n- [ ] 確認済み\nメモ:\n### B <!-- rb-id: R-002 -->\n- [ ] 確認済み\nメモ:\n",
        );
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].status, ReviewStatus::Pending);
        assert_eq!(items[1].status, ReviewStatus::Pending);
        let items = parse_items(
            "### A <!-- rb-id: R-001 -->\n- [x] 確認済み\nメモ:\n### B <!-- rb-id: R-002 -->\n- [ ] 確認済み\nメモ:\n",
        );
        assert_eq!(items[0].status, ReviewStatus::Answered);
        assert_eq!(items[1].status, ReviewStatus::Pending);
    }

    #[test]
    fn summary_counts_and_lists_only_answered_in_file_order() {
        let text = "## C\n\
### A <!-- rb-id: R-002 -->\n- [x] 確認済み\nメモ:\n\
### B <!-- rb-id: R-003 -->\n- [ ] 確認済み\nメモ:\n\
### C <!-- rb-id: R-005 -->\n- [ ] 確認済み\nメモ: この方針で進めてください\n\
### D <!-- rb-id: R-006 -->\n- [x] 確認済み\nメモ: OK\n";
        let s = summarize_board(text);
        assert_eq!(s.pending_count, 1);
        assert_eq!(s.answered_count, 3);
        let ids: Vec<_> = s.answered_items.iter().map(|i| i.id.as_deref()).collect();
        assert_eq!(ids, [Some("R-002"), Some("R-005"), Some("R-006")]);
        assert_eq!(s.answered_items[0].answer_summary, "checked");
        assert_eq!(
            s.answered_items[1].answer_summary,
            "この方針で進めてください"
        );
        assert_eq!(s.answered_items[1].title, "C");
        assert_eq!(s.answered_items[2].answer_summary, "OK");
    }

    #[test]
    fn summary_truncates_by_characters_not_bytes() {
        let memo: String = "あ".repeat(120);
        let s = summarize_board(&format!(
            "### A <!-- rb-id: R-001 -->\n- [ ] 確認済み\nメモ: {memo}\n"
        ));
        assert_eq!(s.answered_items[0].answer_summary.chars().count(), 100);
        let exact = "x".repeat(100);
        let s = summarize_board(&format!("### A\n- [ ] 確認済み\nメモ: {exact}\n"));
        assert_eq!(s.answered_items[0].answer_summary, exact);
        assert_eq!(s.answered_items[0].id, None);
    }

    #[test]
    fn empty_board_summary_is_zero() {
        let s = summarize_board("# Board\n## C\n");
        assert_eq!((s.pending_count, s.answered_count), (0, 0));
        assert!(s.answered_items.is_empty());
    }

    #[test]
    fn status_parse_and_names() {
        assert_eq!(
            ReviewStatus::parse("pending").unwrap(),
            ReviewStatus::Pending
        );
        assert_eq!(
            ReviewStatus::parse("resolved").unwrap().as_str(),
            "resolved"
        );
        assert!(ReviewStatus::parse("done").is_err());
    }
}
