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

use std::borrow::Cow;
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

/// Warning returned when a board mixes LF and CRLF line endings (SPEC-RB-026).
pub const MIXED_EOL_WARNING: &str =
    "review_board.md mixes LF and CRLF line endings; they are normalized to LF (a write rewrites the file with LF only)";

/// `true` when `text` has both CRLF and bare-LF line terminators. A uniform
/// board (all LF or all CRLF) is not mixed.
pub fn has_mixed_line_endings(text: &str) -> bool {
    let crlf = text.matches("\r\n").count();
    let lf = text.matches('\n').count();
    crlf > 0 && lf > crlf
}

/// `text` with every CRLF turned into LF when the line endings are mixed;
/// uniform text is returned as is, so a pure-CRLF board stays CRLF.
pub fn normalize_mixed_line_endings(text: &str) -> Cow<'_, str> {
    if has_mixed_line_endings(text) {
        Cow::Owned(text.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(text)
    }
}

/// Why an item heading's stable id cannot be used.
enum IdProblem {
    /// No `rb-id` mention at all (a heading a user typed by hand).
    Missing,
    /// An `rb-id` mention that does not parse to a well-formed `R-NNN` id.
    Corrupted,
}

/// `true` for `R-` followed by one or more ASCII digits and nothing else.
fn is_well_formed_id(id: &str) -> bool {
    id.strip_prefix("R-")
        .is_some_and(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()))
}

fn id_problem(rest: &str) -> Result<String, IdProblem> {
    match parse_item_heading(rest).1 {
        Some(id) if is_well_formed_id(&id) => Ok(id),
        Some(_) => Err(IdProblem::Corrupted),
        None if rest.contains(RB_ID_MARKER) => Err(IdProblem::Corrupted),
        None => Err(IdProblem::Missing),
    }
}

/// Warnings about item (`###`) headings whose stable id is missing, corrupted
/// or duplicated (NFR-RB-REL-002). The sections stay in the board and keep
/// their `seq`; nothing is repaired here. `seq` matches the 0-based index the
/// readers use for `###` sections outside code fences.
pub fn id_warnings(text: &str) -> Vec<String> {
    let mut fence = Fence::default();
    let mut warnings = Vec::new();
    let mut seen: Vec<(String, Vec<usize>)> = Vec::new();
    let mut seq = 0;
    for raw in text.split_inclusive('\n') {
        let (line, _) = split_eol(raw);
        if fence.skips(line) {
            continue;
        }
        let Some((3, rest)) = heading(line) else {
            continue;
        };
        let title = parse_item_heading(rest).0;
        match id_problem(rest) {
            Ok(id) => match seen.iter_mut().find(|(known, _)| *known == id) {
                Some((_, seqs)) => seqs.push(seq),
                None => seen.push((id, vec![seq])),
            },
            Err(IdProblem::Missing) => warnings.push(format!(
                "item \"{title}\" (seq {seq}) has no rb-id comment; address it by seq"
            )),
            Err(IdProblem::Corrupted) => warnings.push(format!(
                "item \"{title}\" (seq {seq}) has a corrupted rb-id comment; address it by seq"
            )),
        }
        seq += 1;
    }
    for (id, seqs) in seen.into_iter().filter(|(_, seqs)| seqs.len() > 1) {
        let list: Vec<String> = seqs.iter().map(usize::to_string).collect();
        warnings.push(format!(
            "id {id} appears more than once (seq {}); fix the duplicate heading",
            list.join(", ")
        ));
    }
    warnings
}

/// Every warning about the board text: line endings first, then ids.
pub fn board_warnings(text: &str) -> Vec<String> {
    let mut warnings = Vec::new();
    if has_mixed_line_endings(text) {
        warnings.push(MIXED_EOL_WARNING.to_string());
    }
    warnings.extend(id_warnings(text));
    warnings
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

/// Image comparison attributes (RB-011) in the order the tool writes them.
/// The first three hold paths, the last two numbers.
pub const COMPARISON_KEYS: [&str; 5] = [
    "expected_image",
    "actual_image",
    "diff_image",
    "similarity_pct",
    "threshold",
];

/// The value of a numeric attribute (`similarity_pct`, `threshold`). An
/// integer stays an integer (`99`, not `99.0`). `None` for anything that is
/// not a finite number, so a hand-edited `high` or `NaN` never reaches JSON.
pub fn parse_number(raw: &str) -> Option<serde_json::Number> {
    let raw = raw.trim();
    if let Ok(n) = raw.parse::<i64>() {
        return Some(n.into());
    }
    raw.parse::<f64>()
        .ok()
        .and_then(serde_json::Number::from_f64)
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
    fn parse_number_keeps_integers_and_rejects_non_finite_text() {
        assert_eq!(parse_number("99"), Some(99.into()));
        assert_eq!(parse_number(" 98.5 "), serde_json::Number::from_f64(98.5));
        assert_eq!(parse_number("105"), Some(105.into()));
        for bad in ["", "high", "NaN", "inf", "-inf", "9x"] {
            assert_eq!(parse_number(bad), None, "{bad:?}");
        }
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

    // --- RB-008 / RB-009 (SPEC-RB-021..023, SPEC-RB-026) ---------------

    #[test]
    fn intact_ids_produce_no_warnings() {
        let text = "## C\n\n### A <!-- rb-id: R-001 -->\n- date: 2026-01-01\n\n### B <!-- rb-id: R-002 -->\n";
        assert!(id_warnings(text).is_empty());
    }

    #[test]
    fn missing_id_comment_is_warned_with_title_and_seq() {
        let text = "### A <!-- rb-id: R-001 -->\n### Hand typed\n";
        let w = id_warnings(text);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(
            w[0].contains("Hand typed") && w[0].contains("seq 1"),
            "{w:?}"
        );
        assert!(w[0].contains("no rb-id"), "{w:?}");
    }

    #[test]
    fn corrupted_id_comments_are_warned() {
        for rest in [
            "Broken <!-- rb-id: R-002",
            "Empty <!-- rb-id: -->",
            "Bare rb-id: R-002",
            "Odd <!-- rb-id: banana -->",
            "Trailing <!-- rb-id: R-12x -->",
        ] {
            let w = id_warnings(&format!("### {rest}\n"));
            assert_eq!(w.len(), 1, "{rest}: {w:?}");
            assert!(w[0].contains("corrupted"), "{rest}: {w:?}");
        }
    }

    #[test]
    fn duplicate_ids_are_warned_once() {
        let text = "### A <!-- rb-id: R-001 -->\n### B <!-- rb-id: R-001 -->\n";
        let w = id_warnings(text);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(
            w[0].contains("R-001") && w[0].contains("more than once"),
            "{w:?}"
        );
    }

    #[test]
    fn id_warnings_ignore_fences_and_other_levels() {
        let text = "## Category\n#### deep\n```\n### Fake\n```\n";
        assert!(id_warnings(text).is_empty());
    }

    #[test]
    fn mixed_line_endings_are_detected_only_when_both_kinds_occur() {
        assert!(has_mixed_line_endings("a\r\nb\n"));
        assert!(has_mixed_line_endings("a\nb\r\n"));
        assert!(!has_mixed_line_endings("a\nb\n"));
        assert!(!has_mixed_line_endings("a\r\nb\r\n"));
        assert!(!has_mixed_line_endings(""));
    }

    #[test]
    fn normalize_converts_only_mixed_text() {
        assert_eq!(normalize_mixed_line_endings("a\r\nb\n").as_ref(), "a\nb\n");
        assert_eq!(
            normalize_mixed_line_endings("a\r\nb\r\n").as_ref(),
            "a\r\nb\r\n"
        );
        assert_eq!(normalize_mixed_line_endings("a\nb\n").as_ref(), "a\nb\n");
    }

    #[test]
    fn board_warnings_combine_eol_and_id_findings() {
        assert!(board_warnings("### A <!-- rb-id: R-001 -->\n").is_empty());
        let w = board_warnings("### A <!-- rb-id: R-001 -->\r\n### B\n");
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w.iter().any(|m| m == MIXED_EOL_WARNING));
    }
}
