//! Section-level access to the review board (`.handoff/review_board.md`).
//!
//! The board is a plain Markdown file a human edits by hand, so this module
//! never parses it into a model and re-renders it. It indexes the byte range
//! of every `###` item section, hands back one section's slice for a read,
//! and for an update rewrites only that slice, splicing it back between the
//! untouched bytes before and after it. Every byte outside the target
//! section is therefore preserved exactly, including edits a user made after
//! the caller last read the file.
//!
//! Layout of an item section (see [`super::review_board`]):
//!
//! ```text
//! ### <title> <!-- rb-id: R-001 -->
//! - priority: high            <- attribute lines (contiguous, right below)
//! - date: 2026-11-17
//! <body: free Markdown, may embed ![alt](path) images>
//! - [ ] 確認済み               <- checkbox line
//! メモ: <user memo>            <- memo line (may continue on later lines)
//! ```
//!
//! A section's `content_hash` is the FNV-1a hash of its raw bytes, the same
//! hash function the document system uses for its raw-byte hashes.

use std::fmt;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Number, Value};

use super::review_assets::{ensure_assets_ignored, normalize_image_path};
use super::review_board::{assignee_warnings, board_path, CHECKBOX_LINE, MEMO_LINE};
use super::review_board_items::{detect_status, ReviewStatus};
use super::review_lock::BoardLock;
use super::review_scan::{
    attr_block_end, board_warnings, checkbox_state, fenced_flags, find_footer, heading, memo_rest,
    normalize_mixed_line_endings, parse_attr, parse_item_heading, parse_number, split_eol, Fence,
};

/// Attribute keys in the order the tool writes them. A key the tool adds to
/// an existing section is inserted at its place in this order; keys it does
/// not know keep their position and sort after these.
const ATTR_ORDER: [&str; 10] = [
    "priority",
    "assignee",
    "task_id",
    "doc_id",
    "expected_image",
    "actual_image",
    "diff_image",
    "similarity_pct",
    "threshold",
    "date",
];

/// Alt text used for an embedded image whose path has no usable file stem.
const FALLBACK_IMAGE_ALT: &str = "image";

/// Where one `###` item section sits in the board file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionSpan {
    /// 0-based index among the `###` sections, in document order.
    pub seq: usize,
    /// Stable id from the heading's `<!-- rb-id: ... -->`; `None` for a
    /// section a user added by hand without one.
    pub id: Option<String>,
    pub title: String,
    /// Text of the enclosing `##` heading (empty before the first one).
    pub category: String,
    /// Byte offset of the heading line.
    pub start: usize,
    /// Byte offset one past the section: the next `##`/`###` heading, or EOF.
    pub end: usize,
}

/// One item section read in full (body, memo, content hash). The status
/// vocabulary and the answered/pending rule come from
/// [`super::review_board_items`], shared with `handoff_review_list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSection {
    pub seq: usize,
    pub id: Option<String>,
    pub title: String,
    pub category: String,
    pub body: String,
    pub status: ReviewStatus,
    pub priority: Option<String>,
    pub assignee: Option<String>,
    pub date: Option<String>,
    pub task_id: Option<String>,
    pub doc_id: Option<String>,
    /// Expected / golden image path (RB-011), as written.
    pub expected_image: Option<String>,
    pub actual_image: Option<String>,
    pub diff_image: Option<String>,
    /// `None` when the line is absent, empty or not a number.
    pub similarity_pct: Option<Number>,
    pub threshold: Option<Number>,
    pub images: Vec<String>,
    pub checkbox_checked: bool,
    pub memo: String,
    pub content_hash: String,
    /// Findings about this item alone (a non-numeric `similarity_pct` or
    /// `threshold`); surfaced by [`read_item_with_warnings`].
    pub warnings: Vec<String>,
}

impl ReviewSection {
    /// JSON form returned to MCP callers. `meta` drops `body` and `memo`.
    pub fn to_json(&self, meta: bool) -> Value {
        let mut m = Map::new();
        m.insert("seq".into(), json!(self.seq));
        m.insert("id".into(), json!(self.id));
        m.insert("title".into(), json!(self.title));
        m.insert("category".into(), json!(self.category));
        if !meta {
            m.insert("body".into(), json!(self.body));
        }
        m.insert("status".into(), json!(self.status));
        m.insert("priority".into(), json!(self.priority));
        m.insert("assignee".into(), json!(self.assignee));
        m.insert("date".into(), json!(self.date));
        m.insert("task_id".into(), json!(self.task_id));
        m.insert("doc_id".into(), json!(self.doc_id));
        m.insert("expected_image".into(), json!(self.expected_image));
        m.insert("actual_image".into(), json!(self.actual_image));
        m.insert("diff_image".into(), json!(self.diff_image));
        m.insert("similarity_pct".into(), json!(self.similarity_pct));
        m.insert("threshold".into(), json!(self.threshold));
        m.insert("images".into(), json!(self.images));
        m.insert("checkbox_checked".into(), json!(self.checkbox_checked));
        if !meta {
            m.insert("memo".into(), json!(self.memo));
        }
        m.insert("content_hash".into(), json!(self.content_hash));
        Value::Object(m)
    }
}

/// How a caller addresses one section.
#[derive(Debug, Clone, Copy)]
pub enum SectionRef<'a> {
    Id(&'a str),
    Seq(usize),
}

/// Returned (inside an `anyhow::Error`) when `expected_hash` is stale, so a
/// caller can retry with `current`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashMismatch {
    pub id: String,
    pub expected: String,
    pub current: String,
}

impl fmt::Display for HashMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "expected_hash mismatch for {}: expected {}, current_hash: {} — the section changed \
             since it was read; re-read it with handoff_review_read and retry with the current \
             hash if the overwrite is intended",
            self.id, self.expected, self.current
        )
    }
}

impl std::error::Error for HashMismatch {}

/// Fields `handoff_review_update` may change. `None` leaves the field alone.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReviewUpdates {
    /// Replaces the whole body (image links in it included).
    pub body: Option<String>,
    /// Empty string removes the attribute line.
    pub priority: Option<String>,
    /// Empty string removes the attribute line.
    pub assignee: Option<String>,
    pub checkbox_checked: Option<bool>,
    /// Replaces the memo text; empty clears it.
    pub memo: Option<String>,
    /// Replaces the body's standalone image lines; empty removes them.
    pub images: Option<Vec<String>>,
}

impl ReviewUpdates {
    const KEYS: [&'static str; 6] = [
        "body",
        "priority",
        "assignee",
        "checkbox_checked",
        "memo",
        "images",
    ];

    /// Parses the `updates` argument. Unknown keys and wrongly typed values
    /// are errors: dropping them would report a success that changed nothing.
    pub fn from_json(value: &Value) -> Result<Self> {
        let obj = value.as_object().context("'updates' must be an object")?;
        if let Some(unknown) = obj.keys().find(|k| !Self::KEYS.contains(&k.as_str())) {
            bail!(
                "unknown key '{unknown}' in 'updates' (allowed: {})",
                Self::KEYS.join(", ")
            );
        }
        let string = |key: &str| -> Result<Option<String>> {
            match obj.get(key) {
                None => Ok(None),
                Some(Value::String(s)) => Ok(Some(s.clone())),
                Some(_) => bail!("'updates.{key}' must be a string"),
            }
        };
        let checkbox_checked = match obj.get("checkbox_checked") {
            None => None,
            Some(Value::Bool(b)) => Some(*b),
            Some(_) => bail!("'updates.checkbox_checked' must be a boolean"),
        };
        let images = match obj.get("images") {
            None => None,
            Some(Value::Array(items)) => Some(
                items
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_string)
                            .context("'updates.images' must be an array of strings")
                    })
                    .collect::<Result<Vec<_>>>()?,
            ),
            Some(_) => bail!("'updates.images' must be an array of strings"),
        };
        let updates = Self {
            body: string("body")?,
            priority: string("priority")?,
            assignee: string("assignee")?,
            checkbox_checked,
            memo: string("memo")?,
            images,
        };
        if updates == Self::default() {
            bail!(
                "'updates' is empty (give at least one of: {})",
                Self::KEYS.join(", ")
            );
        }
        Ok(updates)
    }

    /// Names of the fields this update sets, in a stable order.
    pub fn field_names(&self) -> Vec<&'static str> {
        let mut names = Vec::new();
        if self.body.is_some() {
            names.push("body");
        }
        if self.priority.is_some() {
            names.push("priority");
        }
        if self.assignee.is_some() {
            names.push("assignee");
        }
        if self.checkbox_checked.is_some() {
            names.push("checkbox_checked");
        }
        if self.memo.is_some() {
            names.push("memo");
        }
        if self.images.is_some() {
            names.push("images");
        }
        names
    }

    /// Rejects values that would corrupt the board's structure when written.
    fn validate(&self) -> Result<()> {
        for (key, value) in [("priority", &self.priority), ("assignee", &self.assignee)] {
            if let Some(v) = value {
                if v.contains(['\n', '\r']) {
                    bail!("'updates.{key}' must be a single line");
                }
            }
        }
        if let Some(body) = &self.body {
            validate_free_text("body", body)?;
        }
        if let Some(memo) = &self.memo {
            validate_free_text("memo", memo)?;
            if let Some(line) = memo.lines().find(|l| checkbox_state(l).is_some()) {
                bail!(
                    "'updates.memo' must not contain a checkbox line (found '{line}'): it would \
                     be read back as the item's own checkbox"
                );
            }
        }
        if let Some(images) = &self.images {
            for path in images {
                image_link(path)?;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Public operations
// ---------------------------------------------------------------------------

/// Indexes every `###` item section of `text` (byte ranges, ids, categories).
pub fn index_sections(text: &str) -> Vec<SectionSpan> {
    let mut spans: Vec<SectionSpan> = Vec::new();
    let mut category = String::new();
    let mut fence = Fence::default();
    let mut open = false;
    let mut offset = 0;

    for raw in text.split_inclusive('\n') {
        let start = offset;
        offset += raw.len();
        let (line, _) = split_eol(raw);
        if fence.skips(line) {
            continue;
        }
        let Some((level, rest)) = heading(line) else {
            continue;
        };
        if !(1..=3).contains(&level) {
            continue;
        }
        if open {
            if let Some(last) = spans.last_mut() {
                last.end = start;
            }
            open = false;
        }
        if level == 2 {
            category = rest.trim().to_string();
        } else if level == 3 {
            let (title, id) = parse_item_heading(rest);
            spans.push(SectionSpan {
                seq: spans.len(),
                id,
                title,
                category: category.clone(),
                start,
                end: text.len(),
            });
            open = true;
        }
    }
    spans
}

/// Reads one section of the board. Returns only that section's data; the
/// rest of the file is never part of the result.
pub fn read_item(handoff_dir: &Path, which: SectionRef<'_>) -> Result<ReviewSection> {
    read_item_with_warnings(handoff_dir, which).map(|(item, _)| item)
}

/// [`read_item`] plus the board's warnings (mixed line endings, items with a
/// missing, corrupted or duplicated id). Reading takes no lock. A board with
/// mixed line endings is read as LF, the same view a writer works on, so the
/// `content_hash` returned here is the one `update_item` accepts.
pub fn read_item_with_warnings(
    handoff_dir: &Path,
    which: SectionRef<'_>,
) -> Result<(ReviewSection, Vec<String>)> {
    let board = read_board(handoff_dir)?;
    let spans = index_sections(&board.text);
    let span = locate(&spans, which)?;
    let item = parse_item(&board.text[span.start..span.end], span);
    let mut warnings = board.warnings;
    warnings.extend(item.warnings.iter().cloned());
    Ok((item, warnings))
}

/// Result of a successful [`update_item`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatedItem {
    pub item: ReviewSection,
    pub fields: Vec<&'static str>,
    /// Non-fatal findings about the board; the update still happened.
    pub warnings: Vec<String>,
}

/// Applies `updates` to the section with stable id `id`, provided its current
/// content hash equals `expected_hash`. Only that section's bytes change; the
/// file is rewritten atomically (temp file + rename).
///
/// A stale `expected_hash` yields a [`HashMismatch`] error and leaves the file
/// untouched.
///
/// The board is re-read inside the write lock ([`BoardLock`]), so the hash is
/// checked against the current bytes of the target section only: edits to other
/// sections made before or while the lock was awaited are kept. A board with
/// mixed line endings is written back with LF only.
pub fn update_item(
    handoff_dir: &Path,
    id: &str,
    expected_hash: &str,
    updates: &ReviewUpdates,
) -> Result<UpdatedItem> {
    updates.validate()?;
    let _lock = BoardLock::acquire(handoff_dir)?;
    let Board { text, warnings } = read_board(handoff_dir)?;
    let spans = index_sections(&text);
    let span = locate(&spans, SectionRef::Id(id))?;

    let section = &text[span.start..span.end];
    let current = section_hash(section);
    if current != expected_hash {
        return Err(HashMismatch {
            id: id.to_string(),
            expected: expected_hash.to_string(),
            current,
        }
        .into());
    }

    let new_section = apply_updates(section, updates)?;
    let mut new_text = String::with_capacity(text.len() - section.len() + new_section.len());
    new_text.push_str(&text[..span.start]);
    new_text.push_str(&new_section);
    new_text.push_str(&text[span.end..]);

    // The splice must leave the board's structure intact: same sections, same
    // ids, and the target still addressable. Refuse to write anything else.
    let new_spans = index_sections(&new_text);
    let same_shape =
        new_spans.len() == spans.len() && new_spans.iter().zip(&spans).all(|(a, b)| a.id == b.id);
    if !same_shape {
        bail!("refusing to write: the update would change the board's section structure");
    }

    super::atomic_write(board_path(handoff_dir), new_text.as_bytes())
        .context("Failed to write review_board.md")?;
    if let Some(images) = &updates.images {
        let written: Vec<_> = images.iter().map(|p| normalize_image_path(p)).collect();
        ensure_assets_ignored(handoff_dir, &written)?;
    }

    let new_span = &new_spans[span.seq];
    let item = parse_item(&new_text[new_span.start..new_span.end], new_span);
    let mut warnings = warnings;
    warnings.extend(assignee_warnings(handoff_dir, updates.assignee.as_deref()));
    Ok(UpdatedItem {
        item,
        fields: updates.field_names(),
        warnings,
    })
}

/// FNV-1a hash of a section's raw bytes.
pub fn section_hash(section: &str) -> String {
    lexsim::fnv1a_hex(section.as_bytes())
}

// ---------------------------------------------------------------------------
// Locating and reading
// ---------------------------------------------------------------------------

/// The board text as readers and writers see it, with its warnings.
pub(super) struct Board {
    /// Mixed LF/CRLF turned into LF; a uniform board is as on disk.
    pub(super) text: String,
    pub(super) warnings: Vec<String>,
}

pub(super) fn read_board(handoff_dir: &Path) -> Result<Board> {
    let path = board_path(handoff_dir);
    match std::fs::read_to_string(&path) {
        Ok(raw) => Ok(Board {
            warnings: board_warnings(&raw),
            text: normalize_mixed_line_endings(&raw).into_owned(),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!(
                "review board not found at {} — add an item first to create it",
                path.display()
            )
        }
        Err(e) => Err(e).with_context(|| format!("Failed to read {}", path.display())),
    }
}

pub(super) fn locate<'a>(
    spans: &'a [SectionSpan],
    which: SectionRef<'_>,
) -> Result<&'a SectionSpan> {
    match which {
        SectionRef::Id(id) => {
            let mut matches = spans.iter().filter(|s| s.id.as_deref() == Some(id));
            let first = matches
                .next()
                .with_context(|| format!("review item '{id}' not found"))?;
            if matches.next().is_some() {
                bail!("review item id '{id}' appears more than once in the board; fix the duplicate heading first");
            }
            Ok(first)
        }
        SectionRef::Seq(seq) => spans.get(seq).with_context(|| {
            format!(
                "review item seq {seq} is out of range (the board has {} item section(s))",
                spans.len()
            )
        }),
    }
}

/// One line of a section with its original terminator.
#[derive(Debug, Clone)]
struct Line {
    text: String,
    eol: String,
}

fn to_lines(section: &str) -> Vec<Line> {
    section
        .split_inclusive('\n')
        .map(|raw| {
            let (text, eol) = split_eol(raw);
            Line {
                text: text.to_string(),
                eol: eol.to_string(),
            }
        })
        .collect()
}

fn join_lines(lines: &[Line]) -> String {
    lines
        .iter()
        .map(|l| format!("{}{}", l.text, l.eol))
        .collect()
}

/// Where the parts of a section are, as indices into its lines.
struct Layout {
    /// Index one past the attribute block (the heading is line 0).
    attr_end: usize,
    /// Index one past the last non-blank line.
    content_end: usize,
    checkbox: Option<usize>,
    memo: Option<usize>,
    /// First line of the footer; the body is `attr_end..footer_start`.
    footer_start: usize,
}

fn layout(lines: &[Line]) -> Layout {
    let texts = line_texts(lines);
    let fenced = fenced_flags(&texts);
    let attr_end = attr_block_end(&texts);
    let content_end = lines
        .iter()
        .rposition(|l| !l.text.trim().is_empty())
        .map_or(lines.len(), |i| i + 1);
    let footer = find_footer(&texts, &fenced, attr_end..content_end.max(attr_end));
    let footer_start = [footer.checkbox, footer.memo]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(content_end);
    Layout {
        attr_end,
        content_end,
        checkbox: footer.checkbox,
        memo: footer.memo,
        footer_start,
    }
}

fn line_texts(lines: &[Line]) -> Vec<&str> {
    lines.iter().map(|l| l.text.as_str()).collect()
}

/// Per line: fence delimiter or inside a fenced block.
fn fenced_lines(lines: &[Line]) -> Vec<bool> {
    fenced_flags(&line_texts(lines))
}

pub(super) fn parse_item(section: &str, span: &SectionSpan) -> ReviewSection {
    let lines = to_lines(section);
    let lay = layout(&lines);
    let attr = |key: &str| {
        lines[1.min(lines.len())..lay.attr_end]
            .iter()
            .filter_map(|l| parse_attr(&l.text))
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.to_string())
            .filter(|v| !v.is_empty())
    };
    let mut warnings = Vec::new();
    let mut number = |key: &'static str| {
        let raw = attr(key)?;
        let parsed = parse_number(&raw);
        if parsed.is_none() {
            warnings.push(format!(
                "item {} has a non-numeric {key} ({raw:?}); it is read as null",
                span.id.as_deref().unwrap_or(&span.title)
            ));
        }
        parsed
    };
    let similarity_pct = number("similarity_pct");
    let threshold = number("threshold");
    let body = trimmed_blank_lines(&lines[lay.attr_end..lay.footer_start]);
    let checkbox_checked = lay
        .checkbox
        .and_then(|i| checkbox_state(&lines[i].text))
        .unwrap_or(false);
    let memo = lay.memo.map_or_else(String::new, |i| {
        let first = memo_rest(&lines[i].text).unwrap_or_default();
        let mut parts = vec![first];
        parts.extend(
            lines[i + 1..lay.content_end]
                .iter()
                .map(|l| l.text.as_str()),
        );
        parts.join("\n").trim().to_string()
    });
    let status = detect_status(checkbox_checked, &memo);

    let fenced = fenced_lines(&lines);
    let images = lines
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(i, _)| !fenced[*i])
        .flat_map(|(_, l)| image_spans(&l.text).into_iter().map(|s| s.path))
        .collect();

    ReviewSection {
        seq: span.seq,
        id: span.id.clone(),
        title: span.title.clone(),
        category: span.category.clone(),
        body,
        status,
        priority: attr("priority"),
        assignee: attr("assignee"),
        date: attr("date"),
        task_id: attr("task_id"),
        doc_id: attr("doc_id"),
        expected_image: attr("expected_image"),
        actual_image: attr("actual_image"),
        diff_image: attr("diff_image"),
        similarity_pct,
        threshold,
        images,
        checkbox_checked,
        memo,
        content_hash: section_hash(section),
        warnings,
    }
}

/// Joins `lines` after dropping leading and trailing blank lines.
fn trimmed_blank_lines(lines: &[Line]) -> String {
    let is_blank = |l: &Line| l.text.trim().is_empty();
    let start = lines
        .iter()
        .position(|l| !is_blank(l))
        .unwrap_or(lines.len());
    let end = lines
        .iter()
        .rposition(|l| !is_blank(l))
        .map_or(start, |i| i + 1);
    lines[start..end]
        .iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// Applying updates
// ---------------------------------------------------------------------------

/// Rewrites one section's text with `updates` applied.
fn apply_updates(section: &str, updates: &ReviewUpdates) -> Result<String> {
    let mut lines = to_lines(section);
    if lines.is_empty() {
        bail!("empty section");
    }
    let eol = lines
        .iter()
        .map(|l| l.eol.as_str())
        .find(|e| !e.is_empty())
        .unwrap_or("\n")
        .to_string();
    // Normalise a missing final terminator so inserted lines can follow it;
    // it is restored below so an unterminated last section stays unterminated.
    let unterminated = lines.last().is_some_and(|l| l.eol.is_empty());
    if unterminated {
        if let Some(last) = lines.last_mut() {
            last.eol.clone_from(&eol);
        }
    }

    if let Some(body) = &updates.body {
        set_body(&mut lines, &eol, body);
    }
    if let Some(images) = &updates.images {
        set_images(&mut lines, &eol, images)?;
    }
    if let Some(priority) = &updates.priority {
        set_attr(&mut lines, &eol, "priority", priority.trim());
    }
    if let Some(assignee) = &updates.assignee {
        set_attr(&mut lines, &eol, "assignee", assignee.trim());
    }
    if let Some(checked) = updates.checkbox_checked {
        set_checkbox(&mut lines, &eol, checked);
    }
    if let Some(memo) = &updates.memo {
        set_memo(&mut lines, &eol, memo);
    }

    if unterminated {
        if let Some(last) = lines.last_mut() {
            last.eol.clear();
        }
    }
    Ok(join_lines(&lines))
}

fn new_line(text: impl Into<String>, eol: &str) -> Line {
    Line {
        text: text.into(),
        eol: eol.to_string(),
    }
}

/// Splits free text into lines, dropping leading and trailing blank lines.
fn text_lines(text: &str) -> Vec<String> {
    let normalized = text.replace("\r\n", "\n");
    let mut lines: Vec<&str> = normalized.split('\n').collect();
    while lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    lines.into_iter().map(str::to_string).collect()
}

fn attr_rank(key: &str) -> usize {
    ATTR_ORDER
        .iter()
        .position(|k| *k == key)
        .unwrap_or(ATTR_ORDER.len())
}

fn set_attr(lines: &mut Vec<Line>, eol: &str, key: &str, value: &str) {
    let lay = layout(lines);
    let attr_lines = 1.min(lines.len())..lay.attr_end;
    let existing = attr_lines
        .clone()
        .find(|&i| parse_attr(&lines[i].text).is_some_and(|(k, _)| k == key));
    match (existing, value.is_empty()) {
        (Some(i), true) => {
            lines.remove(i);
        }
        (Some(i), false) => lines[i].text = format!("- {key}: {value}"),
        (None, true) => {}
        (None, false) => {
            let rank = attr_rank(key);
            let at = attr_lines
                .clone()
                .find(|&i| parse_attr(&lines[i].text).is_some_and(|(k, _)| attr_rank(k) > rank))
                .unwrap_or(lay.attr_end);
            lines.insert(at, new_line(format!("- {key}: {value}"), eol));
        }
    }
}

fn set_body(lines: &mut Vec<Line>, eol: &str, body: &str) {
    let lay = layout(lines);
    let region = lay.attr_end..lay.footer_start;
    let is_blank = |l: &Line| l.text.trim().is_empty();
    let old = &lines[region.clone()];
    // Keep the blank-line framing the section already had around its body.
    let lead = old.iter().take_while(|l| is_blank(l)).count();
    let trail = if lead == old.len() {
        0
    } else {
        old.iter().rev().take_while(|l| is_blank(l)).count()
    };
    let mut replacement: Vec<Line> = Vec::new();
    replacement.extend(old[..lead].iter().cloned());
    replacement.extend(text_lines(body).into_iter().map(|t| new_line(t, eol)));
    replacement.extend(old[old.len() - trail..].iter().cloned());
    lines.splice(region, replacement);
}

fn set_images(lines: &mut Vec<Line>, eol: &str, images: &[String]) -> Result<()> {
    let new_links = images
        .iter()
        .map(|p| image_link(&normalize_image_path(p)))
        .collect::<Result<Vec<_>>>()?;
    let lay = layout(lines);
    let fenced = fenced_lines(lines);
    let drop: Vec<usize> = (lay.attr_end..lay.footer_start)
        .filter(|&i| !fenced[i] && is_standalone_image_line(&lines[i].text))
        .collect();
    for i in drop.into_iter().rev() {
        lines.remove(i);
    }
    let lay = layout(lines);
    let at = lines[lay.attr_end..lay.footer_start]
        .iter()
        .rposition(|l| !l.text.trim().is_empty())
        .map_or(lay.attr_end, |i| lay.attr_end + i + 1);
    let inserted: Vec<Line> = new_links.into_iter().map(|t| new_line(t, eol)).collect();
    lines.splice(at..at, inserted);
    Ok(())
}

fn set_checkbox(lines: &mut Vec<Line>, eol: &str, checked: bool) {
    let lay = layout(lines);
    match lay.checkbox {
        Some(i) => {
            // Leave the line byte-identical when it already has the state.
            if checkbox_state(&lines[i].text) != Some(checked) {
                let mark = if checked { 'x' } else { ' ' };
                // "- [" is 3 ASCII bytes; the mark is 1 byte; the rest follows.
                let rest = lines[i].text[4..].to_string();
                lines[i].text = format!("- [{mark}{rest}");
            }
        }
        None => {
            let line = if checked {
                CHECKBOX_LINE.replacen("[ ]", "[x]", 1)
            } else {
                CHECKBOX_LINE.to_string()
            };
            let at = lay.memo.unwrap_or(lay.content_end);
            lines.insert(at, new_line(line, eol));
        }
    }
}

fn set_memo(lines: &mut Vec<Line>, eol: &str, memo: &str) {
    let lay = layout(lines);
    let mut memo_lines = text_lines(memo).into_iter();
    let first = match memo_lines.next() {
        Some(t) => format!("{MEMO_LINE} {t}"),
        None => MEMO_LINE.to_string(),
    };
    let mut replacement = vec![new_line(first, eol)];
    replacement.extend(memo_lines.map(|t| new_line(t, eol)));
    let start = lay.memo.unwrap_or(lay.content_end);
    lines.splice(start..lay.content_end.max(start), replacement);
}

// ---------------------------------------------------------------------------
// Free-text validation
// ---------------------------------------------------------------------------

/// Rejects free text that would add a `##`/`###` heading or leave a code fence
/// open, either of which would restructure the board.
fn validate_free_text(field: &str, text: &str) -> Result<()> {
    let mut fence = Fence::default();
    for line in text.lines() {
        if fence.skips(line) {
            continue;
        }
        if let Some((level, _)) = heading(line) {
            if level == 2 || level == 3 {
                bail!(
                    "'updates.{field}' must not contain a level-{level} heading (found '{line}'): \
                     it would split the item"
                );
            }
        }
    }
    if fence.is_open() {
        bail!("'updates.{field}' leaves a code fence unclosed, which would swallow the sections after it");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------

struct ImageSpan {
    start: usize,
    end: usize,
    path: String,
}

/// Markdown image links `![alt](path)` / `![alt](<path with spaces>)` in a line.
fn image_spans(line: &str) -> Vec<ImageSpan> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(p) = line[i..].find("![") {
        let start = i + p;
        let alt_start = start + 2;
        i = alt_start;
        let Some(q) = line[alt_start..].find("](") else {
            continue;
        };
        let alt = &line[alt_start..alt_start + q];
        if alt.contains(['[', ']']) {
            continue;
        }
        let target_start = alt_start + q + 2;
        let target = line[target_start..].trim_start();
        let lead = line[target_start..].len() - target.len();
        let (path, consumed) = if let Some(angled) = target.strip_prefix('<') {
            let Some(close) = angled.find('>') else {
                continue;
            };
            (&angled[..close], 1 + close + 1)
        } else {
            let end = target
                .find(|c: char| c.is_whitespace() || c == ')')
                .unwrap_or(target.len());
            (&target[..end], end)
        };
        let Some(close_paren) = target[consumed..].find(')') else {
            continue;
        };
        let end = target_start + lead + consumed + close_paren + 1;
        if !path.is_empty() {
            out.push(ImageSpan {
                start,
                end,
                path: path.to_string(),
            });
        }
        i = end;
    }
    out
}

/// A line made of nothing but image links.
fn is_standalone_image_line(line: &str) -> bool {
    let spans = image_spans(line);
    if spans.is_empty() {
        return false;
    }
    let mut rest = String::new();
    let mut at = 0;
    for s in &spans {
        rest.push_str(&line[at..s.start]);
        at = s.end;
    }
    rest.push_str(&line[at..]);
    rest.trim().is_empty()
}

/// Markdown image link for `path`, alt text from the file name.
fn image_link(path: &str) -> Result<String> {
    if path.trim().is_empty() {
        bail!("image path must not be empty");
    }
    if path.chars().any(char::is_control) {
        bail!("image path must not contain control characters or line breaks: {path:?}");
    }
    if path.contains('>') {
        bail!("image path must not contain '>': {path:?}");
    }
    let name = Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .unwrap_or(FALLBACK_IMAGE_ALT);
    let alt: String = name.chars().filter(|c| !matches!(c, '[' | ']')).collect();
    let target = if path.contains(|c: char| c.is_whitespace() || c == '(' || c == ')') {
        format!("<{path}>")
    } else {
        path.to_string()
    };
    Ok(format!("![{alt}]({target})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board_text() -> String {
        "\
## 設計判断

### First <!-- rb-id: R-001 -->
- priority: high
- assignee: ryoma
- date: 2026-11-17

Body one.
![shot.png](review_board_assets/shot.png)

- [ ] 確認済み

メモ:

### Second <!-- rb-id: R-002 -->
- date: 2026-11-17

Body two.

- [x] 確認済み

メモ: looks good

## その他

### Third <!-- rb-id: R-003 -->
- date: 2026-11-18

- [ ] 確認済み

メモ: needs a call
second memo line
"
        .to_string()
    }

    fn setup(text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let handoff = dir.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();
        std::fs::write(board_path(&handoff), text).unwrap();
        (dir, handoff)
    }

    fn file(handoff: &Path) -> String {
        std::fs::read_to_string(board_path(handoff)).unwrap()
    }

    fn hash_of(handoff: &Path, id: &str) -> String {
        read_item(handoff, SectionRef::Id(id)).unwrap().content_hash
    }

    fn upd() -> ReviewUpdates {
        ReviewUpdates::default()
    }

    // --- indexing ------------------------------------------------------

    #[test]
    fn index_finds_item_sections_with_ids_categories_and_contiguous_ranges() {
        let text = board_text();
        let spans = index_sections(&text);
        let ids: Vec<_> = spans.iter().map(|s| s.id.clone().unwrap()).collect();
        assert_eq!(ids, ["R-001", "R-002", "R-003"]);
        assert_eq!(spans[0].category, "設計判断");
        assert_eq!(spans[2].category, "その他");
        assert_eq!(spans[0].title, "First");
        assert_eq!(spans[0].end, spans[1].start);
        assert_eq!(spans[2].end, text.len());
        assert!(text[spans[0].start..].starts_with("### First"));
        // The `## その他` heading belongs to no item section.
        assert!(!text[spans[1].start..spans[1].end].contains("## その他"));
    }

    #[test]
    fn headings_inside_code_fences_are_not_sections() {
        let text = "## C\n\n### Real <!-- rb-id: R-001 -->\n```\n### Fake <!-- rb-id: R-002 -->\n## Fake cat\n```\n- [ ] 確認済み\nメモ:\n";
        let spans = index_sections(text);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].end, text.len());
    }

    #[test]
    fn section_without_rb_id_has_no_id_but_a_seq() {
        let spans = index_sections("## C\n### By hand\n- [ ] 確認済み\n");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].id, None);
        assert_eq!(spans[0].title, "By hand");
    }

    // --- read ----------------------------------------------------------

    #[test]
    fn read_by_id_returns_only_that_section() {
        let (_d, h) = setup(&board_text());
        let item = read_item(&h, SectionRef::Id("R-001")).unwrap();
        assert_eq!(item.title, "First");
        assert_eq!(item.priority.as_deref(), Some("high"));
        assert_eq!(item.assignee.as_deref(), Some("ryoma"));
        assert_eq!(item.date.as_deref(), Some("2026-11-17"));
        assert_eq!(item.category, "設計判断");
        assert!(item.body.contains("Body one."));
        assert!(!item.body.contains("Body two."));
        assert!(!item.body.contains("確認済み"), "footer is not body");
        let json = item.to_json(false).to_string();
        assert!(!json.contains("Body two") && !json.contains("Third"));
    }

    #[test]
    fn read_by_seq_is_zero_based_over_item_sections() {
        let (_d, h) = setup(&board_text());
        assert_eq!(
            read_item(&h, SectionRef::Seq(0)).unwrap().id.as_deref(),
            Some("R-001")
        );
        assert_eq!(
            read_item(&h, SectionRef::Seq(2)).unwrap().id.as_deref(),
            Some("R-003")
        );
    }

    #[test]
    fn read_reports_images_and_ignores_images_in_code_fences() {
        let text = "## C\n### A <!-- rb-id: R-001 -->\n![a](x/a.png) text ![b](<my dir/b.png>)\n```\n![no](fenced.png)\n```\n- [ ] 確認済み\nメモ:\n";
        let (_d, h) = setup(text);
        let item = read_item(&h, SectionRef::Id("R-001")).unwrap();
        assert_eq!(item.images, ["x/a.png", "my dir/b.png"]);
    }

    #[test]
    fn read_without_images_has_an_empty_list() {
        let (_d, h) = setup(&board_text());
        assert!(read_item(&h, SectionRef::Id("R-002"))
            .unwrap()
            .images
            .is_empty());
    }

    #[test]
    fn status_is_pending_answered_by_checkbox_or_by_memo() {
        let (_d, h) = setup(&board_text());
        let first = read_item(&h, SectionRef::Id("R-001")).unwrap();
        assert_eq!(
            (first.status, first.checkbox_checked),
            (ReviewStatus::Pending, false)
        );
        assert_eq!(first.memo, "");
        let second = read_item(&h, SectionRef::Id("R-002")).unwrap();
        assert!(second.checkbox_checked);
        assert_eq!(second.status, ReviewStatus::Answered);
        assert_eq!(second.memo, "looks good");
        let third = read_item(&h, SectionRef::Id("R-003")).unwrap();
        assert!(!third.checkbox_checked, "unchecked box");
        assert_eq!(third.status, ReviewStatus::Answered, "memo alone answers");
        assert_eq!(third.memo, "needs a call\nsecond memo line");
    }

    #[test]
    fn full_width_colon_memo_and_uppercase_x_are_understood() {
        let text = "## C\n### A <!-- rb-id: R-001 -->\n- [X] 確認済み\nメモ：はい\n";
        let (_d, h) = setup(text);
        let item = read_item(&h, SectionRef::Id("R-001")).unwrap();
        assert!(item.checkbox_checked);
        assert_eq!(item.memo, "はい");
    }

    #[test]
    fn meta_json_omits_body_and_memo_but_keeps_the_hash() {
        let (_d, h) = setup(&board_text());
        let meta = read_item(&h, SectionRef::Id("R-002"))
            .unwrap()
            .to_json(true);
        assert!(meta.get("body").is_none() && meta.get("memo").is_none());
        assert!(meta["content_hash"].as_str().is_some_and(|s| s.len() == 16));
        assert_eq!(meta["status"], "answered");
    }

    #[test]
    fn content_hash_is_fnv1a_of_the_section_bytes_and_stable() {
        let text = board_text();
        let (_d, h) = setup(&text);
        let span = &index_sections(&text)[1];
        let expected = lexsim::fnv1a_hex(&text.as_bytes()[span.start..span.end]);
        assert_eq!(hash_of(&h, "R-002"), expected);
        assert_eq!(hash_of(&h, "R-002"), hash_of(&h, "R-002"));
        assert_ne!(hash_of(&h, "R-001"), hash_of(&h, "R-002"));
    }

    #[test]
    fn read_errors_name_the_requested_value() {
        let (_d, h) = setup(&board_text());
        let e = read_item(&h, SectionRef::Id("R-099"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("R-099"), "{e}");
        let e = read_item(&h, SectionRef::Seq(7)).unwrap_err().to_string();
        assert!(e.contains('7') && e.contains('3'), "{e}");
    }

    #[test]
    fn read_of_a_missing_board_is_an_error_and_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let h = dir.path().join(".handoff");
        std::fs::create_dir_all(&h).unwrap();
        let e = read_item(&h, SectionRef::Seq(0)).unwrap_err().to_string();
        assert!(e.contains("not found"), "{e}");
        assert!(!board_path(&h).exists());
    }

    #[test]
    fn read_never_modifies_the_file() {
        let (_d, h) = setup(&board_text());
        let before = std::fs::read(board_path(&h)).unwrap();
        read_item(&h, SectionRef::Id("R-001")).unwrap();
        assert_eq!(std::fs::read(board_path(&h)).unwrap(), before);
    }

    #[test]
    fn duplicate_ids_are_an_error_not_a_silent_pick() {
        let text = "## C\n### A <!-- rb-id: R-001 -->\n- [ ] 確認済み\n### B <!-- rb-id: R-001 -->\n- [ ] 確認済み\n";
        let (_d, h) = setup(text);
        let e = read_item(&h, SectionRef::Id("R-001"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("more than once"), "{e}");
    }

    // --- update: isolation ---------------------------------------------

    #[test]
    fn update_changes_only_the_target_section_bytes() {
        let text = board_text();
        let (_d, h) = setup(&text);
        let spans = index_sections(&text);
        let hash = hash_of(&h, "R-002");
        update_item(
            &h,
            "R-002",
            &hash,
            &ReviewUpdates {
                body: Some("New body.".into()),
                ..upd()
            },
        )
        .unwrap();
        let after = file(&h);
        assert!(after.starts_with(&text[..spans[1].start]));
        assert!(after.ends_with(&text[spans[1].end..]));
        assert!(after.contains("New body.") && !after.contains("Body two."));
    }

    #[test]
    fn a_users_edit_to_another_section_survives_an_update() {
        let (_d, h) = setup(&board_text());
        let hash_b = hash_of(&h, "R-002");
        // The user edits section A (R-001) after the AI read B.
        let edited = file(&h).replace(
            "- [ ] 確認済み\n\nメモ:\n\n### Second",
            "- [x] 確認済み\n\nメモ: user wrote this\n\n### Second",
        );
        assert_ne!(edited, file(&h));
        std::fs::write(board_path(&h), &edited).unwrap();
        update_item(
            &h,
            "R-002",
            &hash_b,
            &ReviewUpdates {
                priority: Some("low".into()),
                ..upd()
            },
        )
        .unwrap();
        let a = read_item(&h, SectionRef::Id("R-001")).unwrap();
        assert!(a.checkbox_checked);
        assert_eq!(a.memo, "user wrote this");
        let after = file(&h);
        let a_span = &index_sections(&after)[0];
        let before_span = &index_sections(&edited)[0];
        assert_eq!(
            &after[a_span.start..a_span.end],
            &edited[before_span.start..before_span.end]
        );
    }

    #[test]
    fn checkbox_only_update_leaves_everything_else_in_the_section_unchanged() {
        let (_d, h) = setup(&board_text());
        let before = read_item(&h, SectionRef::Id("R-001")).unwrap();
        let out = update_item(
            &h,
            "R-001",
            &before.content_hash,
            &ReviewUpdates {
                checkbox_checked: Some(true),
                ..upd()
            },
        )
        .unwrap();
        assert!(file(&h).contains("- [x] 確認済み"));
        let after = out.item;
        assert!(after.checkbox_checked);
        assert_eq!(after.body, before.body);
        assert_eq!(after.priority, before.priority);
        assert_eq!(after.memo, before.memo);
        assert_eq!(after.images, before.images);
        assert_eq!(out.fields, ["checkbox_checked"]);
    }

    #[test]
    fn unchecking_and_idempotent_checking_work() {
        let (_d, h) = setup(&board_text());
        let hash = hash_of(&h, "R-002");
        let out = update_item(
            &h,
            "R-002",
            &hash,
            &ReviewUpdates {
                checkbox_checked: Some(true),
                ..upd()
            },
        )
        .unwrap();
        assert_eq!(
            out.item.content_hash, hash,
            "already checked: bytes unchanged"
        );
        let out = update_item(
            &h,
            "R-002",
            &hash,
            &ReviewUpdates {
                checkbox_checked: Some(false),
                ..upd()
            },
        )
        .unwrap();
        assert!(!out.item.checkbox_checked);
        assert!(file(&h).contains("- [ ] 確認済み\n\nメモ: looks good"));
    }

    // --- update: hash --------------------------------------------------

    #[test]
    fn stale_hash_returns_the_current_hash_and_does_not_write() {
        let (_d, h) = setup(&board_text());
        let before = std::fs::read(board_path(&h)).unwrap();
        let current = hash_of(&h, "R-001");
        let err = update_item(
            &h,
            "R-001",
            "0000000000000000",
            &ReviewUpdates {
                memo: Some("x".into()),
                ..upd()
            },
        )
        .unwrap_err();
        let mismatch = err.downcast_ref::<HashMismatch>().expect("typed error");
        assert_eq!(mismatch.current, current);
        assert!(err.to_string().contains(&current));
        assert_eq!(std::fs::read(board_path(&h)).unwrap(), before);
    }

    #[test]
    fn update_hash_matches_a_following_read() {
        let (_d, h) = setup(&board_text());
        let hash = hash_of(&h, "R-001");
        let out = update_item(
            &h,
            "R-001",
            &hash,
            &ReviewUpdates {
                memo: Some("done".into()),
                ..upd()
            },
        )
        .unwrap();
        assert_ne!(out.item.content_hash, hash);
        assert_eq!(hash_of(&h, "R-001"), out.item.content_hash);
    }

    #[test]
    fn unknown_id_is_an_error_and_does_not_write() {
        let (_d, h) = setup(&board_text());
        let before = std::fs::read(board_path(&h)).unwrap();
        let e = update_item(
            &h,
            "R-404",
            "abc",
            &ReviewUpdates {
                memo: Some("x".into()),
                ..upd()
            },
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("R-404"), "{e}");
        assert_eq!(std::fs::read(board_path(&h)).unwrap(), before);
    }

    #[test]
    fn successful_update_leaves_no_temp_file_behind() {
        let (_d, h) = setup(&board_text());
        let hash = hash_of(&h, "R-001");
        update_item(
            &h,
            "R-001",
            &hash,
            &ReviewUpdates {
                memo: Some("x".into()),
                ..upd()
            },
        )
        .unwrap();
        let mut names: Vec<String> = std::fs::read_dir(&h)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        // The board and its write lock only; no `.tmp` leftovers.
        assert_eq!(
            names,
            [
                crate::storage::review_lock::LOCK_FILE,
                crate::storage::review_board::BOARD_FILE
            ]
        );
    }

    // --- update: field semantics ---------------------------------------

    #[test]
    fn priority_is_replaced_added_in_order_and_removed() {
        let (_d, h) = setup(&board_text());
        // Replace.
        let hash = hash_of(&h, "R-001");
        update_item(
            &h,
            "R-001",
            &hash,
            &ReviewUpdates {
                priority: Some("low".into()),
                ..upd()
            },
        )
        .unwrap();
        assert_eq!(
            read_item(&h, SectionRef::Id("R-001"))
                .unwrap()
                .priority
                .as_deref(),
            Some("low")
        );
        // Add to a section that has only `date`: lands before the date line.
        let hash = hash_of(&h, "R-002");
        update_item(
            &h,
            "R-002",
            &hash,
            &ReviewUpdates {
                priority: Some("high".into()),
                assignee: Some("ryoma".into()),
                ..upd()
            },
        )
        .unwrap();
        assert!(file(&h).contains("### Second <!-- rb-id: R-002 -->\n- priority: high\n- assignee: ryoma\n- date: 2026-11-17\n"));
        // Remove with an empty string.
        let hash = hash_of(&h, "R-002");
        update_item(
            &h,
            "R-002",
            &hash,
            &ReviewUpdates {
                priority: Some(String::new()),
                ..upd()
            },
        )
        .unwrap();
        assert_eq!(
            read_item(&h, SectionRef::Id("R-002")).unwrap().priority,
            None
        );
        assert!(file(&h).contains("### Second <!-- rb-id: R-002 -->\n- assignee: ryoma\n- date"));
    }

    #[test]
    fn body_update_keeps_attributes_footer_and_blank_line_framing() {
        let (_d, h) = setup(&board_text());
        let hash = hash_of(&h, "R-002");
        update_item(
            &h,
            "R-002",
            &hash,
            &ReviewUpdates {
                body: Some("Line 1\nLine 2\n".into()),
                ..upd()
            },
        )
        .unwrap();
        assert!(file(&h).contains(
            "### Second <!-- rb-id: R-002 -->\n- date: 2026-11-17\n\nLine 1\nLine 2\n\n- [x] 確認済み\n\nメモ: looks good\n"
        ));
    }

    #[test]
    fn empty_body_clears_it() {
        let (_d, h) = setup(&board_text());
        let hash = hash_of(&h, "R-002");
        let out = update_item(
            &h,
            "R-002",
            &hash,
            &ReviewUpdates {
                body: Some(String::new()),
                ..upd()
            },
        )
        .unwrap();
        assert_eq!(out.item.body, "");
        assert!(out.item.checkbox_checked);
    }

    #[test]
    fn memo_update_replaces_a_multi_line_memo_and_can_clear_it() {
        let (_d, h) = setup(&board_text());
        let hash = hash_of(&h, "R-003");
        let out = update_item(
            &h,
            "R-003",
            &hash,
            &ReviewUpdates {
                memo: Some("one\ntwo".into()),
                ..upd()
            },
        )
        .unwrap();
        assert_eq!(out.item.memo, "one\ntwo");
        assert!(file(&h).ends_with("メモ: one\ntwo\n"));
        let out = update_item(
            &h,
            "R-003",
            &out.item.content_hash,
            &ReviewUpdates {
                memo: Some(String::new()),
                ..upd()
            },
        )
        .unwrap();
        assert_eq!(out.item.memo, "");
        assert_eq!(out.item.status, ReviewStatus::Pending);
        assert!(file(&h).ends_with("メモ:\n"));
    }

    #[test]
    fn images_update_replaces_standalone_image_lines_only() {
        let (_d, h) = setup(&board_text());
        let hash = hash_of(&h, "R-001");
        let out = update_item(
            &h,
            "R-001",
            &hash,
            &ReviewUpdates {
                images: Some(vec!["assets/new one.png".into(), "../design/m.png".into()]),
                ..upd()
            },
        )
        .unwrap();
        assert_eq!(out.item.images, ["assets/new one.png", "../design/m.png"]);
        assert!(out.item.body.contains("Body one."));
        assert!(!file(&h).contains("shot.png"));
        // Clearing removes them.
        let out = update_item(
            &h,
            "R-001",
            &out.item.content_hash,
            &ReviewUpdates {
                images: Some(vec![]),
                ..upd()
            },
        )
        .unwrap();
        assert!(out.item.images.is_empty());
        assert_eq!(out.item.body, "Body one.");
    }

    #[test]
    fn images_update_normalizes_asset_paths_and_ignores_the_assets_dir() {
        let (_d, h) = setup(&board_text());
        let hash = hash_of(&h, "R-001");
        let out = update_item(
            &h,
            "R-001",
            &hash,
            &ReviewUpdates {
                images: Some(vec![
                    ".handoff/review_board_assets/a.png".into(),
                    "../design/m.png".into(),
                ]),
                ..upd()
            },
        )
        .unwrap();
        assert_eq!(
            out.item.images,
            ["review_board_assets/a.png", "../design/m.png"]
        );
        assert_eq!(
            std::fs::read_to_string(h.join(".gitignore")).unwrap(),
            "review_board_assets/\n"
        );
    }

    #[test]
    fn images_update_with_only_project_relative_paths_leaves_gitignore_alone() {
        let (_d, h) = setup(&board_text());
        let hash = hash_of(&h, "R-001");
        update_item(
            &h,
            "R-001",
            &hash,
            &ReviewUpdates {
                images: Some(vec!["../design/m.png".into()]),
                ..upd()
            },
        )
        .unwrap();
        assert!(!h.join(".gitignore").exists());
    }

    #[test]
    fn missing_footer_is_created_when_checkbox_or_memo_is_set() {
        let text = "## C\n### A <!-- rb-id: R-001 -->\n- date: 2026-11-17\nbody\n";
        let (_d, h) = setup(text);
        let hash = hash_of(&h, "R-001");
        let out = update_item(
            &h,
            "R-001",
            &hash,
            &ReviewUpdates {
                checkbox_checked: Some(true),
                memo: Some("ok".into()),
                ..upd()
            },
        )
        .unwrap();
        assert_eq!(file(&h), "## C\n### A <!-- rb-id: R-001 -->\n- date: 2026-11-17\nbody\n- [x] 確認済み\nメモ: ok\n");
        assert_eq!(out.item.body, "body");
    }

    #[test]
    fn last_section_without_trailing_newline_stays_unterminated() {
        let text = "## C\n### A <!-- rb-id: R-001 -->\n- [ ] 確認済み\nメモ:";
        let (_d, h) = setup(text);
        let hash = hash_of(&h, "R-001");
        update_item(
            &h,
            "R-001",
            &hash,
            &ReviewUpdates {
                memo: Some("x".into()),
                ..upd()
            },
        )
        .unwrap();
        assert_eq!(
            file(&h),
            "## C\n### A <!-- rb-id: R-001 -->\n- [ ] 確認済み\nメモ: x"
        );
    }

    #[test]
    fn crlf_sections_stay_crlf() {
        let text = "## C\r\n### A <!-- rb-id: R-001 -->\r\n- [ ] 確認済み\r\nメモ:\r\n";
        let (_d, h) = setup(text);
        let hash = hash_of(&h, "R-001");
        update_item(
            &h,
            "R-001",
            &hash,
            &ReviewUpdates {
                priority: Some("high".into()),
                memo: Some("a\nb".into()),
                ..upd()
            },
        )
        .unwrap();
        let after = file(&h);
        assert_eq!(after, "## C\r\n### A <!-- rb-id: R-001 -->\r\n- priority: high\r\n- [ ] 確認済み\r\nメモ: a\r\nb\r\n");
    }

    #[test]
    fn heading_text_and_id_comment_are_untouched_by_updates() {
        let (_d, h) = setup(&board_text());
        let hash = hash_of(&h, "R-001");
        update_item(
            &h,
            "R-001",
            &hash,
            &ReviewUpdates {
                body: Some("x".into()),
                memo: Some("y".into()),
                ..upd()
            },
        )
        .unwrap();
        assert!(file(&h).contains("### First <!-- rb-id: R-001 -->\n"));
    }

    // --- update: validation --------------------------------------------

    #[test]
    fn structure_breaking_input_is_rejected_without_writing() {
        let (_d, h) = setup(&board_text());
        let before = std::fs::read(board_path(&h)).unwrap();
        let hash = hash_of(&h, "R-001");
        let bad = [
            ReviewUpdates {
                body: Some("ok\n### Injected <!-- rb-id: R-9 -->".into()),
                ..upd()
            },
            ReviewUpdates {
                body: Some("## Injected".into()),
                ..upd()
            },
            ReviewUpdates {
                body: Some("```\nnever closed".into()),
                ..upd()
            },
            ReviewUpdates {
                memo: Some("## Injected".into()),
                ..upd()
            },
            ReviewUpdates {
                memo: Some("- [x] fake box".into()),
                ..upd()
            },
            ReviewUpdates {
                priority: Some("a\nb".into()),
                ..upd()
            },
            ReviewUpdates {
                assignee: Some("a\rb".into()),
                ..upd()
            },
            ReviewUpdates {
                images: Some(vec!["a\nb.png".into()]),
                ..upd()
            },
            ReviewUpdates {
                images: Some(vec![String::new()]),
                ..upd()
            },
        ];
        for u in &bad {
            assert!(update_item(&h, "R-001", &hash, u).is_err(), "{u:?}");
        }
        assert_eq!(std::fs::read(board_path(&h)).unwrap(), before);
    }

    #[test]
    fn headings_inside_a_closed_fence_in_the_body_are_allowed() {
        let (_d, h) = setup(&board_text());
        let hash = hash_of(&h, "R-001");
        let out = update_item(
            &h,
            "R-001",
            &hash,
            &ReviewUpdates {
                body: Some("```md\n### sample\n```".into()),
                ..upd()
            },
        )
        .unwrap();
        assert!(out.item.body.contains("### sample"));
        assert_eq!(index_sections(&file(&h)).len(), 3);
    }

    #[test]
    fn from_json_rejects_unknown_keys_wrong_types_and_empty_updates() {
        assert!(ReviewUpdates::from_json(&json!({"title": "x"})).is_err());
        assert!(ReviewUpdates::from_json(&json!({"body": 1})).is_err());
        assert!(ReviewUpdates::from_json(&json!({"checkbox_checked": "yes"})).is_err());
        assert!(ReviewUpdates::from_json(&json!({"images": "a.png"})).is_err());
        assert!(ReviewUpdates::from_json(&json!({"images": [1]})).is_err());
        assert!(ReviewUpdates::from_json(&json!({})).is_err());
        assert!(ReviewUpdates::from_json(&json!("x")).is_err());
        let ok = ReviewUpdates::from_json(
            &json!({"memo": "m", "checkbox_checked": true, "images": ["a.png"]}),
        )
        .unwrap();
        assert_eq!(ok.field_names(), ["checkbox_checked", "memo", "images"]);
    }

    #[test]
    fn image_link_quotes_paths_with_spaces_and_parentheses() {
        assert_eq!(image_link("a/b.png").unwrap(), "![b.png](a/b.png)");
        assert_eq!(
            image_link("a b/c (1).png").unwrap(),
            "![c (1).png](<a b/c (1).png>)"
        );
        let spans = image_spans("![c (1).png](<a b/c (1).png>)");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].path, "a b/c (1).png");
    }

    // --- RB-008: hybrid metadata (SPEC-RB-021..023) -----------------------

    #[test]
    fn hand_edited_attribute_changes_the_value_but_not_the_id() {
        let (_d, h) = setup(&board_text());
        let edited = file(&h).replace("- priority: high", "- priority: low");
        std::fs::write(board_path(&h), edited).unwrap();
        let item = read_item(&h, SectionRef::Id("R-001")).unwrap();
        assert_eq!(item.priority.as_deref(), Some("low"));
        assert_eq!(item.id.as_deref(), Some("R-001"));

        let edited = file(&h).replace("- assignee: ryoma", "- assignee: someone");
        std::fs::write(board_path(&h), edited).unwrap();
        let item = read_item(&h, SectionRef::Id("R-001")).unwrap();
        assert_eq!(item.assignee.as_deref(), Some("someone"));
    }

    #[test]
    fn update_priority_keeps_the_rb_id_comment_on_the_heading() {
        let (_d, h) = setup(&board_text());
        let hash = hash_of(&h, "R-001");
        update_item(
            &h,
            "R-001",
            &hash,
            &ReviewUpdates {
                priority: Some("low".into()),
                ..upd()
            },
        )
        .unwrap();
        assert!(file(&h).contains("### First <!-- rb-id: R-001 -->\n- priority: low\n"));
    }

    #[test]
    fn all_five_attribute_keys_are_read_and_a_body_list_line_is_not_an_attribute() {
        let text = "## C\n\n### A <!-- rb-id: R-001 -->\n- priority: p\n- assignee: a\n- date: 2026-01-02\n- task_id: t-1\n- doc_id: d-1\n\nBody\n- priority: low\n\n- [ ] 確認済み\n\nメモ:\n";
        let (_d, h) = setup(text);
        let item = read_item(&h, SectionRef::Id("R-001")).unwrap();
        assert_eq!(item.priority.as_deref(), Some("p"));
        assert_eq!(item.assignee.as_deref(), Some("a"));
        assert_eq!(item.date.as_deref(), Some("2026-01-02"));
        assert_eq!(item.task_id.as_deref(), Some("t-1"));
        assert_eq!(item.doc_id.as_deref(), Some("d-1"));
        assert!(item.body.contains("- priority: low"));
    }

    #[test]
    fn broken_comment_does_not_block_later_items_and_is_warned() {
        let text = "## C\n\n### First <!-- rb-id: R-001 -->\n- date: 2026-01-01\n\n### Broken <!-- rb-id: R-002\n- date: 2026-01-01\n\n### Third <!-- rb-id: R-003 -->\n- date: 2026-01-01\n";
        let (_d, h) = setup(text);
        let (third, warnings) = read_item_with_warnings(&h, SectionRef::Id("R-003")).unwrap();
        assert_eq!(third.title, "Third");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("Broken") && warnings[0].contains("corrupted"));
        let broken = read_item(&h, SectionRef::Seq(1)).unwrap();
        assert_eq!(broken.id, None);
    }

    #[test]
    fn an_intact_board_reads_without_warnings() {
        let (_d, h) = setup(&board_text());
        let (_, warnings) = read_item_with_warnings(&h, SectionRef::Id("R-001")).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    // --- RB-009: lock, concurrency, mixed line endings (SPEC-RB-024..026) ---

    #[test]
    fn update_waits_for_a_held_lock_and_reads_inside_it() {
        use std::sync::mpsc;
        use std::time::Duration;
        let (_d, h) = setup(&board_text());
        let hash = hash_of(&h, "R-001");
        let guard = crate::storage::review_lock::BoardLock::acquire(&h).unwrap();
        let (tx, rx) = mpsc::channel();
        let h2 = h.clone();
        let t = std::thread::spawn(move || {
            let r = update_item(
                &h2,
                "R-001",
                &hash,
                &ReviewUpdates {
                    memo: Some("done".into()),
                    ..ReviewUpdates::default()
                },
            );
            tx.send(r.is_ok()).unwrap();
        });
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "must wait"
        );
        // A user edit of ANOTHER section while the writer is blocked: the
        // writer re-reads inside the lock, so the edit survives (SPEC-RB-025 AC3).
        let edited = file(&h).replace("Body two.", "Body two, edited by hand.");
        std::fs::write(board_path(&h), edited).unwrap();
        drop(guard);
        assert!(rx.recv_timeout(Duration::from_secs(10)).unwrap());
        t.join().unwrap();
        let text = file(&h);
        assert!(text.contains("Body two, edited by hand."));
        assert!(text.contains("メモ: done"));
    }

    #[test]
    fn concurrent_updates_to_different_sections_both_land() {
        let (_d, h) = setup(&board_text());
        let ids = ["R-001", "R-002", "R-003"];
        let hashes: Vec<String> = ids.iter().map(|id| hash_of(&h, id)).collect();
        let threads: Vec<_> = ids
            .iter()
            .zip(hashes)
            .map(|(id, hash)| {
                let h = h.clone();
                let id = id.to_string();
                std::thread::spawn(move || {
                    update_item(
                        &h,
                        &id,
                        &hash,
                        &ReviewUpdates {
                            memo: Some(format!("memo for {id}")),
                            ..ReviewUpdates::default()
                        },
                    )
                    .unwrap();
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let text = file(&h);
        for id in ids {
            assert!(
                text.contains(&format!("メモ: memo for {id}")),
                "{id}:\n{text}"
            );
        }
    }

    #[test]
    fn concurrent_add_and_update_both_land() {
        use crate::storage::review_board::{add_item, NewReviewItem};
        let (_d, h) = setup(&board_text());
        let hash = hash_of(&h, "R-001");
        let h1 = h.clone();
        let updater = std::thread::spawn(move || {
            update_item(
                &h1,
                "R-001",
                &hash,
                &ReviewUpdates {
                    memo: Some("updated".into()),
                    ..ReviewUpdates::default()
                },
            )
            .unwrap();
        });
        let h2 = h.clone();
        let adder = std::thread::spawn(move || {
            add_item(
                &h2,
                &NewReviewItem {
                    title: "Fresh",
                    category: "その他",
                    ..Default::default()
                },
                chrono::NaiveDate::from_ymd_opt(2026, 11, 17).unwrap(),
            )
            .unwrap();
        });
        updater.join().unwrap();
        adder.join().unwrap();
        let text = file(&h);
        assert!(text.contains("メモ: updated"));
        assert!(text.contains("### Fresh <!-- rb-id: R-004 -->"));
    }

    #[test]
    fn a_failed_update_releases_the_lock_for_the_next_writer() {
        let (_d, h) = setup(&board_text());
        let hash = hash_of(&h, "R-001");
        let memo = ReviewUpdates {
            memo: Some("x".into()),
            ..upd()
        };
        let before = std::fs::read(board_path(&h)).unwrap();
        let err = update_item(&h, "R-001", "stale", &memo).unwrap_err();
        assert!(err.downcast_ref::<HashMismatch>().is_some());
        assert_eq!(std::fs::read(board_path(&h)).unwrap(), before);
        // Would block forever if the failed call leaked the lock.
        update_item(&h, "R-001", &hash, &memo).unwrap();
    }

    #[test]
    fn mixed_line_endings_are_normalized_on_read_and_write_with_a_warning() {
        let mixed = board_text().replacen(
            "### First <!-- rb-id: R-001 -->\n",
            "### First <!-- rb-id: R-001 -->\r\n",
            1,
        );
        let (_d, h) = setup(&mixed);
        // Read: LF content, warning, not an error.
        let (item, warnings) = read_item_with_warnings(&h, SectionRef::Id("R-002")).unwrap();
        assert!(
            warnings.iter().any(|w| w.contains("LF and CRLF")),
            "{warnings:?}"
        );
        assert!(!item.body.contains('\r'));
        // The hash a reader sees is the one the writer accepts.
        let hash = item.content_hash.clone();
        let done = update_item(
            &h,
            "R-002",
            &hash,
            &ReviewUpdates {
                memo: Some("ok".into()),
                ..upd()
            },
        )
        .unwrap();
        assert!(done.warnings.iter().any(|w| w.contains("LF and CRLF")));
        assert!(!std::fs::read(board_path(&h)).unwrap().contains(&b'\r'));
    }

    #[test]
    fn uniform_lf_board_has_no_line_ending_warning_and_uniform_crlf_stays_crlf() {
        let (_d, h) = setup(&board_text());
        let hash = hash_of(&h, "R-001");
        let done = update_item(
            &h,
            "R-001",
            &hash,
            &ReviewUpdates {
                memo: Some("m".into()),
                ..upd()
            },
        )
        .unwrap();
        assert!(done.warnings.is_empty(), "{:?}", done.warnings);

        let crlf = board_text().replace('\n', "\r\n");
        let (_d2, h2) = setup(&crlf);
        let hash = hash_of(&h2, "R-001");
        let done = update_item(
            &h2,
            "R-001",
            &hash,
            &ReviewUpdates {
                memo: Some("m".into()),
                ..upd()
            },
        )
        .unwrap();
        assert!(done.warnings.is_empty());
        let text = file(&h2);
        assert!(text.contains("\r\n") && !text.replace("\r\n", "").contains('\n'));
    }
}
