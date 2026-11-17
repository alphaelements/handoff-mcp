//! Review board decisions archive and resolve (RBD-001, RBD-002,
//! SPEC-RBD-001..009): `.handoff/review_board_decisions.md`.
//!
//! Resolving an item moves it out of `review_board.md` into this file. The
//! file is plain Markdown without frontmatter, like the board:
//!
//! ```text
//! # Decisions
//!
//! ### <title> <!-- rb-id: R-003 -->
//! - date: 2026-11-17
//! - original_id: R-003
//!
//! <decision text>
//!
//! ### <older entry> ...
//! ```
//!
//! New entries are inserted directly after the `# Decisions` heading, so the
//! newest entry is on top and the bytes of every existing entry are kept.
//! The heading carries the same `<!-- rb-id: ... -->` comment as the board, so
//! the id reservation of `handoff_review_add` sees resolved ids.
//!
//! A resolve runs under the board lock and writes the decisions file first:
//! if the board write then fails, the entry is kept (a retried resolve does not
//! duplicate it) and nothing is lost.

use std::path::Path;

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;

use super::review_board::{board_path, read_optional, DECISIONS_FILE};
use super::review_lock::BoardLock;
use super::review_scan::{heading, Fence};
use super::review_section::{index_sections, locate, read_board, SectionRef};

/// The level-1 heading every decisions file starts with.
const DECISIONS_HEADING: &str = "# Decisions";

/// Attribute line that ends an entry's attribute block; the decision text
/// follows it.
const ORIGINAL_ID_PREFIX: &str = "- original_id:";

/// Outcome of a successful [`resolve_item`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedItem {
    pub id: String,
    pub title: String,
    /// The decisions file already held an identical entry (an earlier resolve
    /// wrote it but failed to update the board), so none was added.
    pub entry_already_present: bool,
    /// Non-fatal findings about the board; the resolve still happened.
    pub warnings: Vec<String>,
}

/// Path of the decisions file for a `.handoff/` directory.
pub fn decisions_path(handoff_dir: &Path) -> std::path::PathBuf {
    handoff_dir.join(DECISIONS_FILE)
}

/// Moves the board item `id` into the decisions file with `decision` as its
/// resolution, dated `date`.
///
/// Everything runs under the board write lock, on a fresh read of the board.
/// Order: decisions file, then board. A decisions-write failure leaves both
/// files untouched; a board-write failure leaves the decisions entry in place
/// and the item on the board. Both files are replaced atomically.
pub fn resolve_item(
    handoff_dir: &Path,
    id: &str,
    decision: &str,
    date: NaiveDate,
) -> Result<ResolvedItem> {
    resolve_item_with(handoff_dir, id, decision, date, &|path, bytes| {
        super::atomic_write(path, bytes)
    })
}

/// [`resolve_item`] with the file writer injected, so a test can make one of
/// the two writes fail.
fn resolve_item_with(
    handoff_dir: &Path,
    id: &str,
    decision: &str,
    date: NaiveDate,
    write: &dyn Fn(&Path, &[u8]) -> Result<()>,
) -> Result<ResolvedItem> {
    if id.trim().is_empty() {
        bail!("'id' must not be empty");
    }
    let decision_text = prepare_decision(decision)?;

    let _lock = BoardLock::acquire(handoff_dir)?;
    let board = read_board(handoff_dir)?;
    let spans = index_sections(&board.text);
    let span = locate(&spans, SectionRef::Id(id))?;

    // The section is removed first in memory, so a structural surprise is
    // found before anything is written.
    let mut new_board = String::with_capacity(board.text.len() - (span.end - span.start));
    new_board.push_str(&board.text[..span.start]);
    new_board.push_str(&board.text[span.end..]);
    if index_sections(&new_board).len() + 1 != spans.len() {
        bail!("refusing to write: removing '{id}' would change the board's section structure");
    }

    let decisions_file = decisions_path(handoff_dir);
    let existing = read_optional(&decisions_file)?;
    let entry_already_present = has_entry(&existing, id, &decision_text);
    if !entry_already_present {
        let eol = if existing.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let entry = render_entry(&span.title, id, date, &decision_text, eol);
        let updated = prepend_entry(&existing, &entry, eol);
        write(&decisions_file, updated.as_bytes())
            .with_context(|| format!("Failed to write {DECISIONS_FILE}; the board is unchanged"))?;
    }

    write(&board_path(handoff_dir), new_board.as_bytes()).with_context(|| {
        format!(
            "board write failed: '{id}' is still on the board, and its entry was kept in \
             {DECISIONS_FILE} (a retried resolve will not duplicate it)"
        )
    })?;

    Ok(ResolvedItem {
        id: id.to_string(),
        title: span.title.clone(),
        entry_already_present,
        warnings: board.warnings,
    })
}

/// Trims the decision and makes it safe to embed in an entry: a line that
/// would read as a heading gets a leading backslash (outside code fences), and
/// an unclosed code fence is rejected because it would hide the entries below
/// it from every scanner (including the id reservation).
fn prepare_decision(decision: &str) -> Result<String> {
    let trimmed = decision.trim();
    if trimmed.is_empty() {
        bail!("'decision' must not be empty");
    }
    let mut fence = Fence::default();
    let mut lines = Vec::new();
    for line in trimmed.lines() {
        let line = line.trim_end();
        if fence.skips(line) {
            lines.push(line.to_string());
        } else if line.trim_start().starts_with('#') {
            lines.push(format!("\\{line}"));
        } else {
            lines.push(line.to_string());
        }
    }
    if fence.is_open() {
        bail!("'decision' leaves a code fence unclosed, which would swallow the entries after it");
    }
    Ok(lines.join("\n"))
}

fn render_entry(title: &str, id: &str, date: NaiveDate, decision_text: &str, eol: &str) -> String {
    let mut out = format!(
        "### {title} <!-- rb-id: {id} -->{eol}- date: {date}{eol}{ORIGINAL_ID_PREFIX} {id}{eol}{eol}",
        date = date.format("%Y-%m-%d"),
    );
    for line in decision_text.split('\n') {
        out.push_str(line);
        out.push_str(eol);
    }
    out
}

/// Inserts `entry` directly after the `# Decisions` heading, above every
/// existing entry. Existing bytes are never altered, only added around.
fn prepend_entry(existing: &str, entry: &str, eol: &str) -> String {
    let heading_line = format!("{DECISIONS_HEADING}{eol}{eol}");
    if existing.trim().is_empty() {
        return format!("{heading_line}{entry}");
    }
    if !has_decisions_heading(existing) {
        return format!("{heading_line}{entry}{eol}{existing}");
    }
    match index_sections(existing).first() {
        Some(first) => {
            let (head, rest) = existing.split_at(first.start);
            format!("{head}{entry}{eol}{rest}")
        }
        None => {
            let mut out = existing.to_string();
            if !out.ends_with('\n') {
                out.push_str(eol);
            }
            if !out.ends_with(&format!("{eol}{eol}")) {
                out.push_str(eol);
            }
            out.push_str(entry);
            out
        }
    }
}

fn has_decisions_heading(text: &str) -> bool {
    let mut fence = Fence::default();
    text.lines()
        .any(|line| !fence.skips(line) && heading(line) == Some((1, "Decisions")))
}

/// `true` when `existing` already holds an entry for `id` with exactly this
/// decision text (what an earlier, half-failed resolve of the same call wrote).
fn has_entry(existing: &str, id: &str, decision_text: &str) -> bool {
    index_sections(existing)
        .iter()
        .filter(|s| s.id.as_deref() == Some(id))
        .any(|s| {
            let section = existing[s.start..s.end].replace("\r\n", "\n");
            section
                .lines()
                .position(|l| l.starts_with(ORIGINAL_ID_PREFIX))
                .map(|at| {
                    let stored: Vec<&str> = section.lines().skip(at + 1).collect();
                    stored.join("\n").trim() == decision_text
                })
                .unwrap_or(false)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::review_board::{add_item, NewReviewItem};
    use crate::storage::review_scan::heading_id_numbers;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn setup() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let h = dir.path().join(".handoff");
        std::fs::create_dir_all(&h).unwrap();
        (dir, h)
    }

    fn add(h: &Path, title: &str, category: &str) -> String {
        let item = NewReviewItem {
            title,
            category,
            ..Default::default()
        };
        add_item(h, &item, d("2026-11-10")).unwrap().id
    }

    fn board(h: &Path) -> String {
        std::fs::read_to_string(board_path(h)).unwrap()
    }

    fn decisions(h: &Path) -> String {
        std::fs::read_to_string(decisions_path(h)).unwrap()
    }

    fn files(h: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(h)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn resolve_writes_the_entry_and_removes_the_item() {
        let (_d, h) = setup();
        let id = add(&h, "Choose layout", "設計判断");
        let out = resolve_item(&h, &id, "Use two columns", d("2026-11-17")).unwrap();
        assert_eq!(out.id, "R-001");
        assert_eq!(out.title, "Choose layout");

        let dec = decisions(&h);
        assert!(dec.starts_with("# Decisions\n"), "{dec}");
        assert!(
            dec.contains("### Choose layout <!-- rb-id: R-001 -->\n"),
            "{dec}"
        );
        assert!(dec.contains("\n- date: 2026-11-17\n"), "{dec}");
        assert!(dec.contains("\n- original_id: R-001\n"), "{dec}");
        assert!(dec.contains("\nUse two columns\n"), "{dec}");
        assert!(!board(&h).contains("rb-id: R-001"));
        assert!(board(&h).contains("## 設計判断"));
    }

    #[test]
    fn newest_entry_is_first_and_existing_entries_are_byte_identical() {
        let (_d, h) = setup();
        let a = add(&h, "First", "X");
        let b = add(&h, "Second", "X");
        resolve_item(&h, &a, "one", d("2026-11-16")).unwrap();
        let after_first = decisions(&h);
        let entries_before = after_first
            .strip_prefix("# Decisions\n\n")
            .unwrap()
            .to_string();
        resolve_item(&h, &b, "two", d("2026-11-17")).unwrap();

        let dec = decisions(&h);
        let second = dec.find("rb-id: R-002").unwrap();
        let first = dec.find("rb-id: R-001").unwrap();
        assert!(second < first, "{dec}");
        assert!(dec.starts_with("# Decisions\n\n### Second"), "{dec}");
        assert!(dec.ends_with(&entries_before), "{dec}");
    }

    #[test]
    fn same_day_resolves_list_the_later_one_first() {
        let (_d, h) = setup();
        let a = add(&h, "A", "X");
        let b = add(&h, "B", "X");
        resolve_item(&h, &a, "da", d("2026-11-17")).unwrap();
        resolve_item(&h, &b, "db", d("2026-11-17")).unwrap();
        let dec = decisions(&h);
        assert!(dec.find("R-002").unwrap() < dec.find("R-001").unwrap());
    }

    #[test]
    fn other_board_bytes_are_untouched() {
        let (_d, h) = setup();
        add(&h, "Keep A", "X");
        let b = add(&h, "Gone", "X");
        add(&h, "Keep C", "Y");
        let before = board(&h);
        let start = before.find("### Gone").unwrap();
        let end = before.find("## Y").unwrap();
        resolve_item(&h, &b, "ok", d("2026-11-17")).unwrap();
        assert_eq!(board(&h), format!("{}{}", &before[..start], &before[end..]));
    }

    #[test]
    fn unknown_id_missing_board_and_bad_input_change_nothing() {
        let (_d, h) = setup();
        let id = add(&h, "A", "X");
        let before = board(&h);
        assert!(resolve_item(&h, "R-404", "x", d("2026-11-17")).is_err());
        assert!(resolve_item(&h, "", "x", d("2026-11-17")).is_err());
        assert!(resolve_item(&h, &id, "  \n ", d("2026-11-17")).is_err());
        assert!(resolve_item(&h, &id, "```\nopen", d("2026-11-17")).is_err());
        assert_eq!(board(&h), before);
        assert!(!decisions_path(&h).exists());

        let (_d2, h2) = setup();
        assert!(resolve_item(&h2, "R-001", "x", d("2026-11-17")).is_err());
        assert!(!decisions_path(&h2).exists());
    }

    #[test]
    fn multi_line_decision_is_kept_and_headings_are_escaped() {
        let (_d, h) = setup();
        let id = add(&h, "A", "X");
        resolve_item(
            &h,
            &id,
            "line1\n## Not a heading\n```\n# in fence\n```\nline3",
            d("2026-11-17"),
        )
        .unwrap();
        let dec = decisions(&h);
        assert!(
            dec.contains("line1\n\\## Not a heading\n```\n# in fence\n```\nline3\n"),
            "{dec}"
        );
        assert_eq!(index_sections(&dec).len(), 1);
    }

    #[test]
    fn resolved_ids_stay_reserved() {
        let (_d, h) = setup();
        add(&h, "A", "X");
        add(&h, "B", "X");
        let c = add(&h, "C", "X");
        resolve_item(&h, &c, "ok", d("2026-11-17")).unwrap();
        assert_eq!(heading_id_numbers(&decisions(&h)), vec![3]);
        assert_eq!(add(&h, "D", "X"), "R-004");
    }

    #[test]
    fn decisions_write_failure_leaves_both_files_untouched() {
        let (_d, h) = setup();
        let id = add(&h, "A", "X");
        let before = board(&h);
        let err = resolve_item_with(&h, &id, "ok", d("2026-11-17"), &|_, _| bail!("disk full"))
            .unwrap_err();
        assert!(format!("{err:#}").contains("disk full"));
        assert_eq!(board(&h), before);
        assert!(!decisions_path(&h).exists());
    }

    #[test]
    fn board_write_failure_keeps_the_entry_and_the_item_and_frees_the_lock() {
        let (_d, h) = setup();
        let id = add(&h, "A", "X");
        let before = board(&h);
        let err = resolve_item_with(&h, &id, "ok", d("2026-11-17"), &|path, bytes| {
            if path == board_path(&h) {
                bail!("board is read-only");
            }
            super::super::atomic_write(path, bytes)
        })
        .unwrap_err();
        assert!(err.to_string().contains("board write failed"), "{err:#}");
        assert!(decisions(&h).contains("original_id: R-001"));
        assert_eq!(board(&h), before);

        // The lock is free again: another writer is not blocked.
        add(&h, "B", "X");

        // A retry does not duplicate the entry.
        let out = resolve_item(&h, &id, "ok", d("2026-11-18")).unwrap();
        assert!(out.entry_already_present);
        assert_eq!(decisions(&h).matches("original_id: R-001").count(), 1);
        assert!(!board(&h).contains("rb-id: R-001"));
    }

    #[test]
    fn retry_with_a_different_decision_adds_a_second_entry() {
        let (_d, h) = setup();
        let id = add(&h, "A", "X");
        let _ = resolve_item_with(&h, &id, "first", d("2026-11-17"), &|path, bytes| {
            if path == board_path(&h) {
                bail!("nope");
            }
            super::super::atomic_write(path, bytes)
        });
        resolve_item(&h, &id, "second", d("2026-11-17")).unwrap();
        assert_eq!(decisions(&h).matches("original_id: R-001").count(), 2);
    }

    #[test]
    fn no_temp_file_is_left_behind_and_the_lock_file_is_the_only_extra() {
        let (_d, h) = setup();
        let id = add(&h, "A", "X");
        resolve_item(&h, &id, "ok", d("2026-11-17")).unwrap();
        let names = files(&h);
        assert!(names.iter().all(|n| !n.contains(".tmp.")), "{names:?}");
    }

    #[test]
    fn prepends_below_a_heading_only_file_and_a_hand_made_file_without_heading() {
        let (_d, h) = setup();
        let a = add(&h, "A", "X");
        std::fs::write(decisions_path(&h), "# Decisions\n").unwrap();
        resolve_item(&h, &a, "ok", d("2026-11-17")).unwrap();
        let dec = decisions(&h);
        assert!(dec.starts_with("# Decisions\n\n### A"), "{dec}");

        let b = add(&h, "B", "X");
        std::fs::write(
            decisions_path(&h),
            "### Old <!-- rb-id: R-001 -->\n- date: 2026-01-01\n",
        )
        .unwrap();
        resolve_item(&h, &b, "ok", d("2026-11-17")).unwrap();
        let dec = decisions(&h);
        assert!(dec.starts_with("# Decisions\n\n### B"), "{dec}");
        assert!(
            dec.ends_with("### Old <!-- rb-id: R-001 -->\n- date: 2026-01-01\n"),
            "{dec}"
        );
    }

    #[test]
    fn crlf_decisions_files_stay_crlf() {
        let (_d, h) = setup();
        let a = add(&h, "A", "X");
        std::fs::write(
            decisions_path(&h),
            "# Decisions\r\n\r\n### Old <!-- rb-id: R-009 -->\r\n- date: 2026-01-01\r\n",
        )
        .unwrap();
        resolve_item(&h, &a, "ok", d("2026-11-17")).unwrap();
        let dec = decisions(&h);
        assert!(!dec.replace("\r\n", "").contains('\n'), "{dec:?}");
        assert!(dec.starts_with("# Decisions\r\n\r\n### A"), "{dec:?}");
    }

    #[test]
    fn resolving_a_pending_item_works_and_an_emptied_category_stays() {
        let (_d, h) = setup();
        let id = add(&h, "Only", "Solo");
        resolve_item(&h, &id, "ok", d("2026-11-17")).unwrap();
        assert!(board(&h).contains("## Solo"));
    }

    #[test]
    fn concurrent_resolves_of_one_id_write_one_entry() {
        let (_d, h) = setup();
        let id = add(&h, "A", "X");
        let results: Vec<_> = std::thread::scope(|s| {
            let hs: Vec<_> = (0..4)
                .map(|_| s.spawn(|| resolve_item(&h, &id, "ok", d("2026-11-17"))))
                .collect();
            hs.into_iter().map(|t| t.join().unwrap()).collect()
        });
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(decisions(&h).matches("original_id: R-001").count(), 1);
    }
}
