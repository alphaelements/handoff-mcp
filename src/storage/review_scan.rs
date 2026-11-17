//! The one Markdown scanner behind every review board reader and writer.
//!
//! `.handoff/review_board.md` is edited by hand, so `review_board` (add),
//! `review_board_items` (list) and `review_section` (read/update) must agree
//! on what counts as a heading, a code fence, an attribute line, the item's
//! checkbox and its memo. They all take those rules from here; a disagreement
//! would show up as a list/read/update mismatch on a hand-edited board.
//!
//! The rules:
//!
//! - A heading is an ATX heading with at most three leading spaces. Headings
//!   inside a fenced code block are body text. A level 1, 2 or 3 heading ends
//!   an item section.
//! - Attribute (`- key: value`), checkbox (`- [ ] ...`) and memo (`メモ:`)
//!   lines start at column 0; the attribute block is the contiguous run of
//!   attribute lines directly under the item heading.
//! - The item's checkbox is the last checkbox line (outside a fence) that has
//!   a memo line after it, so a task list in the body or in the memo is not
//!   mistaken for it. Without a checkbox the memo is the last memo line.

use std::ops::Range;

/// Marker inside the heading's HTML comment that introduces the stable id.
pub const RB_ID_MARKER: &str = "rb-id:";

/// Prefixes that start the memo line. The full-width colon is what a
/// Japanese IME produces, so both spellings are accepted when reading.
pub const MEMO_PREFIXES: [&str; 2] = ["メモ:", "メモ："];

/// Tracks fenced code blocks so headings and footer markers inside them are
/// not mistaken for board structure.
#[derive(Default)]
pub struct Fence {
    open: Option<(char, usize)>,
}

impl Fence {
    /// Feeds one line; `true` when the line is a fence delimiter or inside a
    /// fenced block (and so must not be interpreted as structure).
    pub fn skips(&mut self, line: &str) -> bool {
        let t = line.trim_start_matches(' ');
        let run = |c: char| t.chars().take_while(|&x| x == c).count();
        match self.open {
            Some((c, len)) => {
                let n = run(c);
                if n >= len && t[n * c.len_utf8()..].trim().is_empty() {
                    self.open = None;
                }
                true
            }
            None => {
                for c in ['`', '~'] {
                    let n = run(c);
                    if n >= 3 {
                        self.open = Some((c, n));
                        return true;
                    }
                }
                false
            }
        }
    }

    /// A fence is still open (the text ended inside a code block).
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }
}

/// Per line: whether it is a fence delimiter or inside a fenced block.
pub fn fenced_flags(lines: &[&str]) -> Vec<bool> {
    let mut fence = Fence::default();
    lines.iter().map(|l| fence.skips(l)).collect()
}

/// `(level, text after "#... ")` of an ATX heading line (at most three
/// leading spaces), else `None`.
pub fn heading(line: &str) -> Option<(usize, &str)> {
    let rest = line.trim_start_matches(' ');
    if line.len() - rest.len() > 3 {
        return None;
    }
    let level = rest.chars().take_while(|&c| c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let text = &rest[level..];
    match text.chars().next() {
        None => Some((level, "")),
        Some(' ' | '\t') => Some((level, text.trim())),
        Some(_) => None,
    }
}

/// Splits a raw line into its text and its line terminator.
pub fn split_eol(raw: &str) -> (&str, &str) {
    if let Some(t) = raw.strip_suffix("\r\n") {
        (t, "\r\n")
    } else if let Some(t) = raw.strip_suffix('\n') {
        (t, "\n")
    } else {
        (raw, "")
    }
}

/// Splits `Title <!-- rb-id: R-001 -->` into the title and the id. A comment
/// that is not an `rb-id` one stays part of the title.
pub fn parse_item_heading(rest: &str) -> (String, Option<String>) {
    if let Some(open) = rest.rfind("<!--") {
        if let Some(close) = rest[open..].find("-->") {
            let inner = rest[open + 4..open + close].trim();
            if let Some(id) = inner.strip_prefix(RB_ID_MARKER) {
                let id = id.trim();
                if !id.is_empty() {
                    return (rest[..open].trim().to_string(), Some(id.to_string()));
                }
            }
        }
    }
    (rest.trim().to_string(), None)
}

/// The number of an `R-NNN` id (`R-007` -> 7).
pub fn id_number(id: &str) -> Option<u64> {
    let digits: String = id
        .strip_prefix("R-")?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// Every id number carried by an item (`###`) heading of `text`. Ids quoted
/// in body text, code fences or other heading levels do not count.
pub fn heading_id_numbers(text: &str) -> Vec<u64> {
    let mut fence = Fence::default();
    let mut numbers = Vec::new();
    for raw in text.split_inclusive('\n') {
        let (line, _) = split_eol(raw);
        if fence.skips(line) {
            continue;
        }
        if let Some((3, rest)) = heading(line) {
            if let Some(n) = parse_item_heading(rest).1.as_deref().and_then(id_number) {
                numbers.push(n);
            }
        }
    }
    numbers
}

/// `(key, value)` of a `- key: value` attribute line.
pub fn parse_attr(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("- ")?;
    let (key, value) = rest.split_once(':')?;
    let key = key.trim();
    let is_key = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    is_key.then(|| (key, value.trim()))
}

/// `Some(checked)` for a task-list line (`- [ ] ...` / `- [x] ...`).
pub fn checkbox_state(line: &str) -> Option<bool> {
    let rest = line.strip_prefix("- [")?;
    let mut chars = rest.chars();
    let mark = chars.next()?;
    if chars.next()? != ']' {
        return None;
    }
    match mark {
        ' ' => Some(false),
        'x' | 'X' => Some(true),
        _ => None,
    }
}

/// Text after the memo prefix when `line` is the memo line.
pub fn memo_rest(line: &str) -> Option<&str> {
    MEMO_PREFIXES
        .iter()
        .find_map(|p| line.strip_prefix(p))
        .map(str::trim)
}

/// Index one past the attribute block of an item section (`lines[0]` is the
/// heading; the block is the contiguous attribute lines right below it).
pub fn attr_block_end(lines: &[&str]) -> usize {
    let mut end = 1.min(lines.len());
    while end < lines.len() && parse_attr(lines[end]).is_some() {
        end += 1;
    }
    end
}

/// Line indices of an item's checkbox and memo lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Footer {
    pub checkbox: Option<usize>,
    pub memo: Option<usize>,
}

/// Finds the checkbox and memo lines within `range` (indices into `lines`),
/// skipping fenced lines. See the module docs for the rule.
pub fn find_footer(lines: &[&str], fenced: &[bool], range: Range<usize>) -> Footer {
    let usable = |i: &usize| !fenced[*i];
    let memo_after = |from: usize| {
        (from + 1..range.end)
            .filter(usable)
            .find(|&i| memo_rest(lines[i]).is_some())
    };
    let checkboxes: Vec<usize> = range
        .clone()
        .filter(usable)
        .filter(|&i| checkbox_state(lines[i]).is_some())
        .collect();
    for &cb in checkboxes.iter().rev() {
        if let Some(memo) = memo_after(cb) {
            return Footer {
                checkbox: Some(cb),
                memo: Some(memo),
            };
        }
    }
    match checkboxes.last() {
        Some(&cb) => Footer {
            checkbox: Some(cb),
            memo: None,
        },
        None => Footer {
            checkbox: None,
            memo: range
                .rev()
                .filter(usable)
                .find(|&i| memo_rest(lines[i]).is_some()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_accepts_up_to_three_spaces_and_rejects_the_rest() {
        assert_eq!(heading("## A"), Some((2, "A")));
        assert_eq!(heading("   ### A "), Some((3, "A")));
        assert_eq!(heading("    ### A"), None);
        assert_eq!(heading("##A"), None);
        assert_eq!(heading("#"), Some((1, "")));
        assert_eq!(heading("####### x"), None);
    }

    #[test]
    fn fence_is_length_aware_and_hides_headings() {
        let lines = ["a", "````", "```", "### x", "````", "### y"];
        assert_eq!(fenced_flags(&lines), [false, true, true, true, true, false]);
        let mut f = Fence::default();
        f.skips("~~~");
        assert!(f.is_open());
    }

    #[test]
    fn heading_id_numbers_count_item_headings_only() {
        let text = "\
## C <!-- rb-id: R-900 -->
### A <!-- rb-id: R-007 -->
body quoting rb-id: R-9999
```
### Fake <!-- rb-id: R-8888 -->
```
#### Deep <!-- rb-id: R-7777 -->
### B <!-- rb-id: R-012 -->
### Hand typed
";
        assert_eq!(heading_id_numbers(text), vec![7, 12]);
    }

    #[test]
    fn footer_pairs_the_last_checkbox_that_has_a_memo_after_it() {
        let lines = [
            "### T",
            "- [x] body task",
            "- [ ] 確認済み",
            "",
            "メモ: a",
            "- [x] memo task",
        ];
        let fenced = fenced_flags(&lines);
        assert_eq!(
            find_footer(&lines, &fenced, 1..lines.len()),
            Footer {
                checkbox: Some(2),
                memo: Some(4)
            }
        );
    }

    #[test]
    fn footer_without_checkbox_takes_the_last_memo_line() {
        let lines = ["### T", "メモ: quoted", "text", "メモ： real"];
        let fenced = fenced_flags(&lines);
        assert_eq!(
            find_footer(&lines, &fenced, 1..lines.len()),
            Footer {
                checkbox: None,
                memo: Some(3)
            }
        );
    }

    #[test]
    fn footer_ignores_fenced_lines() {
        let lines = ["### T", "```", "- [x] 確認済み", "メモ: x", "```"];
        let fenced = fenced_flags(&lines);
        assert_eq!(
            find_footer(&lines, &fenced, 1..lines.len()),
            Footer {
                checkbox: None,
                memo: None
            }
        );
    }

    #[test]
    fn attr_block_stops_at_the_first_other_line() {
        let lines = [
            "### T",
            "- priority: high",
            "- date: 2026-11-17",
            "",
            "- x: y",
        ];
        assert_eq!(attr_block_end(&lines), 3);
        assert_eq!(attr_block_end(&["### T", "", "- a: b"]), 1);
        assert_eq!(attr_block_end(&[]), 0);
    }
}
