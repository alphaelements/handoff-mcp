//! User Review Board storage: `.handoff/review_board.md`.
//!
//! The board is a plain Markdown file (no YAML frontmatter) a human edits in
//! any editor. `##` headings are categories, `###` headings are items. Each
//! item heading carries its stable id as an HTML comment
//! (`### Title <!-- rb-id: R-001 -->`) so it is invisible in a Markdown
//! preview; every other property is a visible `- key: value` line the user
//! can change by hand.
//!
//! This module owns the file layout (init and add). Reading, updating and
//! listing sections build on the same layout.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;

use super::config::{push_config_warning, read_config_lenient};
use super::review_assets::{ensure_assets_ignored, normalize_image_path};
use super::review_lock::BoardLock;
use super::review_scan::{
    board_warnings, heading, heading_id_numbers, normalize_mixed_line_endings, split_eol, Fence,
};

/// File name of the board inside `.handoff/`.
pub const BOARD_FILE: &str = "review_board.md";

/// File name of the decisions archive inside `.handoff/`. Resolved items
/// move there; their ids stay reserved so a new item never reuses one.
pub const DECISIONS_FILE: &str = "review_board_decisions.md";

/// Categories written into a freshly initialized board, in order.
pub const DEFAULT_CATEGORIES: [&str; 4] = ["設計判断", "実装確認", "テスト確認", "その他"];

/// Checkbox line every item ends with (unchecked).
pub const CHECKBOX_LINE: &str = "- [ ] 確認済み";

/// Memo line every item ends with; the user writes after the colon.
pub const MEMO_LINE: &str = "メモ:";

/// Path of the board file for a `.handoff/` directory.
pub fn board_path(handoff_dir: &Path) -> PathBuf {
    handoff_dir.join(BOARD_FILE)
}

/// Fields of an item to append.
#[derive(Debug, Default, Clone)]
pub struct NewReviewItem<'a> {
    pub title: &'a str,
    pub category: &'a str,
    pub body: Option<&'a str>,
    pub priority: Option<&'a str>,
    pub assignee: Option<&'a str>,
    pub task_id: Option<&'a str>,
    pub doc_id: Option<&'a str>,
    /// Expected / golden image path (RB-011); written as an attribute line,
    /// not copied and not embedded.
    pub expected_image: Option<&'a str>,
    pub actual_image: Option<&'a str>,
    pub diff_image: Option<&'a str>,
    /// Similarity percentage of the comparison (RB-011).
    pub similarity_pct: Option<f64>,
    /// Pass/fail threshold of the comparison (RB-011).
    pub threshold: Option<f64>,
    pub image_paths: &'a [String],
}

/// Outcome of [`add_item`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddedItem {
    pub id: String,
    pub category: String,
    /// The board file did not exist and was initialized first.
    pub board_created: bool,
    /// No `##` section matched the category, so one was appended.
    pub category_created: bool,
    /// Non-fatal findings about the board (mixed line endings, items with a
    /// missing or corrupted id). The add still happened.
    pub warnings: Vec<String>,
}

/// Creates the board with the default categories when it does not exist.
/// Returns `true` when the file was created, `false` when it already existed
/// (left byte-identical). Runs under the board write lock.
pub fn init_board(handoff_dir: &Path) -> Result<bool> {
    let _lock = BoardLock::acquire(handoff_dir)?;
    create_board_if_missing(handoff_dir)
}

/// [`init_board`] for a caller that already holds the [`BoardLock`] (the lock
/// is not reentrant, so taking it again would deadlock).
fn create_board_if_missing(handoff_dir: &Path) -> Result<bool> {
    use std::io::Write;

    let path = board_path(handoff_dir);
    // `create_new` makes "does it exist" and "create it" one step, so a
    // concurrent writer that already put items into the board is never
    // overwritten by the default skeleton.
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(e) => {
            return Err(
                anyhow::Error::new(e).context(format!("Failed to create {}", path.display()))
            )
        }
    };
    file.write_all(default_board().as_bytes())
        .with_context(|| format!("Failed to write {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("Failed to sync {}", path.display()))?;
    Ok(true)
}

/// Appends a new item to its category and returns the assigned id.
///
/// The input is validated before anything is read or written, so a rejected
/// call leaves the board (or its absence) untouched. A missing board is
/// initialized first; a missing category is appended as a new `##` section.
///
/// Everything after validation runs under the board write lock, and the board
/// is read inside it, so a concurrent writer's change is never overwritten. A
/// board with mixed LF/CRLF line endings is rewritten with LF only and the
/// finding is returned in [`AddedItem::warnings`].
pub fn add_item(
    handoff_dir: &Path,
    item: &NewReviewItem<'_>,
    date: NaiveDate,
) -> Result<AddedItem> {
    let valid = ValidItem::new(item)?;
    let _lock = BoardLock::acquire(handoff_dir)?;
    let board_created = create_board_if_missing(handoff_dir)?;

    let path = board_path(handoff_dir);
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let mut warnings = board_warnings(&raw);
    warnings.extend(assignee_warnings(handoff_dir, item.assignee));
    let text = normalize_mixed_line_endings(&raw);
    let decisions = read_optional(&handoff_dir.join(DECISIONS_FILE))?;

    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let id = format_id(next_id_number(&[&text, &decisions])?);
    let block = render_item(&valid, &id, date, eol);
    let (new_text, category_created) = insert_item(&text, valid.category, &block, eol);

    crate::storage::atomic_write(&path, new_text.as_bytes())?;
    // After the write, so a rejected or failed add never touches `.gitignore`.
    ensure_assets_ignored(handoff_dir, &valid.images)?;
    Ok(AddedItem {
        id,
        category: valid.category.to_string(),
        board_created,
        category_created,
        warnings,
    })
}

fn default_board() -> String {
    let mut out = String::from(
        "# Review Board\n\n\
         > Items awaiting the user's review. Tick the checkbox or write a memo under an item to answer it.\n",
    );
    for category in DEFAULT_CATEGORIES {
        out.push_str(&format!("\n## {category}\n"));
    }
    out
}

pub(super) fn read_optional(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(anyhow::Error::new(e).context(format!("Failed to read {}", path.display()))),
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// An [`NewReviewItem`] whose fields passed validation, trimmed and with
/// empty optionals dropped.
struct ValidItem<'a> {
    title: &'a str,
    category: &'a str,
    body: Option<&'a str>,
    attributes: Vec<(&'static str, String)>,
    /// Image paths as written in the board (see [`normalize_image_path`]).
    images: Vec<String>,
}

impl<'a> ValidItem<'a> {
    fn new(item: &NewReviewItem<'a>) -> Result<Self> {
        let title = single_line("title", item.title)?
            .ok_or_else(|| anyhow::anyhow!("'title' must not be empty"))?;
        if title.contains("<!--") || title.contains("-->") {
            bail!("'title' must not contain an HTML comment marker (<!-- or -->)");
        }
        let category = single_line("category", item.category)?
            .ok_or_else(|| anyhow::anyhow!("'category' must not be empty"))?;

        let mut attributes = Vec::new();
        for (key, value) in [
            ("priority", item.priority),
            ("assignee", item.assignee),
            ("task_id", item.task_id),
            ("doc_id", item.doc_id),
            ("expected_image", item.expected_image),
            ("actual_image", item.actual_image),
            ("diff_image", item.diff_image),
        ] {
            if let Some(v) = value.map(|v| single_line(key, v)).transpose()?.flatten() {
                attributes.push((key, v.to_string()));
            }
        }
        for (key, value) in [
            ("similarity_pct", item.similarity_pct),
            ("threshold", item.threshold),
        ] {
            if let Some(n) = value {
                if !n.is_finite() {
                    bail!("'{key}' must be a finite number");
                }
                attributes.push((key, n.to_string()));
            }
        }

        let body = item.body.map(str::trim).filter(|b| !b.is_empty());
        if let Some(body) = body {
            validate_body(body)?;
        }

        let images = item
            .image_paths
            .iter()
            .map(|p| image_path(p).map(|p| normalize_image_path(p).into_owned()))
            .collect::<Result<Vec<_>>>()?;

        Ok(Self {
            title,
            category,
            body,
            attributes,
            images,
        })
    }
}

/// Warnings for an `assignee` key about to be written (RB-012): one when the
/// key is not a `[assignees.<key>]` table of `config.toml`.
///
/// Never an error. An absent or blank key is not validated (a blank value
/// writes nothing or removes the line). A missing, unreadable or roster-less
/// config is an empty roster, so every key is unknown, and an unreadable one
/// adds its own load warning: the write must keep working with a broken
/// hand-edited config. Keys compare exactly (case-sensitive).
pub fn assignee_warnings(handoff_dir: &Path, assignee: Option<&str>) -> Vec<String> {
    let Some(key) = assignee.map(str::trim).filter(|k| !k.is_empty()) else {
        return Vec::new();
    };
    let (config, config_warning) = read_config_lenient(handoff_dir);
    let mut warnings = Vec::new();
    push_config_warning(&mut warnings, config_warning);
    if !config.is_some_and(|c| c.assignees.contains_key(key)) {
        warnings.push(format!(
            "assignee '{key}' is not in the [assignees] roster of config.toml; it was written as given"
        ));
    }
    warnings
}

/// Trims `value`; `None` when nothing is left. A line break would split the
/// field across Markdown lines and corrupt the board's structure.
fn single_line<'a>(field: &str, value: &'a str) -> Result<Option<&'a str>> {
    if value.contains(['\n', '\r']) {
        bail!("'{field}' must be a single line");
    }
    let trimmed = value.trim();
    Ok((!trimmed.is_empty()).then_some(trimmed))
}

/// A body line that is a `#`/`##`/`###` heading would end the item or start a
/// new category or item; an unclosed code fence would swallow everything after the item.
fn validate_body(body: &str) -> Result<()> {
    let mut fence = Fence::default();
    for line in body.lines() {
        if fence.skips(line) {
            continue;
        }
        if matches!(heading(line), Some((1..=3, _))) {
            bail!(
                "'body' must not contain a line starting with '#', '##' or '###' (it would \
                 split the review board); use '####' or deeper, or bold text: {line:?}"
            );
        }
    }
    if fence.is_open() {
        bail!("'body' has an unclosed code fence");
    }
    Ok(())
}

fn image_path(raw: &str) -> Result<&str> {
    let path = single_line("image_paths entry", raw)?
        .ok_or_else(|| anyhow::anyhow!("'image_paths' entries must not be empty"))?;
    if path.contains(['<', '>']) {
        bail!("'image_paths' entry must not contain '<' or '>': {path:?}");
    }
    Ok(path)
}

// ---------------------------------------------------------------------------
// Markdown line scanning
// ---------------------------------------------------------------------------

/// One line of the board with its byte range (`end` includes the line break).
struct BoardLine<'a> {
    end: usize,
    content: &'a str,
    /// Heading level when the line is a heading outside a code fence.
    level: Option<usize>,
    text: &'a str,
}

fn scan(text: &str) -> Vec<BoardLine<'_>> {
    let mut lines = Vec::new();
    let mut fence = Fence::default();
    let mut start = 0;
    for raw in text.split_inclusive('\n') {
        let (content, _) = split_eol(raw);
        let (level, heading_text) = match (fence.skips(content), heading(content)) {
            (false, Some((level, t))) => (Some(level), t),
            _ => (None, ""),
        };
        lines.push(BoardLine {
            end: start + raw.len(),
            content,
            level,
            text: heading_text,
        });
        start += raw.len();
    }
    lines
}

// ---------------------------------------------------------------------------
// Ids
// ---------------------------------------------------------------------------

/// One more than the highest id on an item heading in any of `texts` (the
/// board and the decisions archive, whose ids stay reserved).
fn next_id_number(texts: &[&str]) -> Result<u64> {
    let max = texts
        .iter()
        .flat_map(|t| heading_id_numbers(t))
        .max()
        .unwrap_or(0);
    max.checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("review board id space exhausted"))
}

fn format_id(n: u64) -> String {
    format!("R-{n:03}")
}

// ---------------------------------------------------------------------------
// Rendering and insertion
// ---------------------------------------------------------------------------

/// The item as Markdown lines joined with `eol`, ending with a line break.
///
/// Blocks are separated by blank lines on purpose: a paragraph line directly
/// after a list item would otherwise render as part of that list item.
fn render_item(item: &ValidItem<'_>, id: &str, date: NaiveDate, eol: &str) -> String {
    let mut header = vec![format!("### {} <!-- rb-id: {id} -->", item.title)];
    for (key, value) in &item.attributes {
        header.push(format!("- {key}: {value}"));
    }
    header.push(format!("- date: {}", date.format("%Y-%m-%d")));

    let mut blocks = vec![header.join(eol)];
    if let Some(body) = item.body {
        blocks.push(body.lines().collect::<Vec<_>>().join(eol));
    }
    blocks.extend(item.images.iter().map(|p| image_link(p)));
    blocks.push(CHECKBOX_LINE.to_string());
    blocks.push(MEMO_LINE.to_string());

    let sep = format!("{eol}{eol}");
    format!("{}{eol}", blocks.join(&sep))
}

/// `![name](path)`; a path with spaces or parentheses goes in `<...>`, which
/// CommonMark accepts and every preview renders.
fn image_link(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let alt: String = name.chars().filter(|c| !matches!(c, '[' | ']')).collect();
    if path.contains(|c: char| c.is_whitespace() || matches!(c, '(' | ')')) {
        format!("![{alt}](<{path}>)")
    } else {
        format!("![{alt}]({path})")
    }
}

/// Puts `block` at the end of the `##` section named `category`, creating
/// the section at the end of the file when absent. Returns the new text and
/// whether the section was created. Bytes outside the insertion are kept.
fn insert_item(text: &str, category: &str, block: &str, eol: &str) -> (String, bool) {
    let lines = scan(text);
    let Some(head) = lines
        .iter()
        .position(|l| l.level == Some(2) && l.text == category)
    else {
        return (append_category(text, category, block, eol), true);
    };

    let section_end = lines[head + 1..]
        .iter()
        .position(|l| matches!(l.level, Some(1 | 2)))
        .map_or(lines.len(), |n| head + 1 + n);
    let last_content = (head..section_end)
        .rev()
        .find(|&i| !lines[i].content.trim().is_empty())
        .unwrap_or(head);
    let at = lines[last_content].end;

    let mut out = String::with_capacity(text.len() + block.len() + 2 * eol.len());
    out.push_str(&text[..at]);
    if !out.ends_with('\n') {
        out.push_str(eol);
    }
    out.push_str(eol);
    out.push_str(block);
    let rest = &text[at..];
    if !rest.is_empty() && !rest.starts_with(['\n', '\r']) {
        out.push_str(eol);
    }
    out.push_str(rest);
    (out, false)
}

fn append_category(text: &str, category: &str, block: &str, eol: &str) -> String {
    let mut out = String::from(text);
    if !out.is_empty() {
        if !out.ends_with('\n') {
            out.push_str(eol);
        }
        let blank_line_before_end = out.ends_with("\n\n") || out.ends_with("\r\n\r\n");
        if !blank_line_before_end {
            out.push_str(eol);
        }
    }
    out.push_str(&format!("## {category}{eol}{eol}"));
    out.push_str(block);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 17).unwrap()
    }

    fn setup() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let handoff = dir.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();
        (dir, handoff)
    }

    fn read(handoff: &Path) -> String {
        std::fs::read_to_string(board_path(handoff)).unwrap()
    }

    fn item<'a>(title: &'a str, category: &'a str) -> NewReviewItem<'a> {
        NewReviewItem {
            title,
            category,
            ..Default::default()
        }
    }

    fn heading_lines(text: &str, prefix: &str) -> Vec<String> {
        text.lines()
            .filter(|l| l.starts_with(prefix))
            .map(str::to_string)
            .collect()
    }

    // --- init (SPEC-RB-001, SPEC-RB-002) -------------------------------

    #[test]
    fn init_creates_plain_markdown_with_default_categories() {
        let (_d, h) = setup();
        assert!(init_board(&h).unwrap());
        let text = read(&h);
        assert_ne!(text.lines().next().unwrap(), "---");
        assert_eq!(
            heading_lines(&text, "## "),
            vec!["## 設計判断", "## 実装確認", "## テスト確認", "## その他"]
        );
        assert!(
            heading_lines(&text, "###").is_empty(),
            "no item section yet"
        );
    }

    #[test]
    fn init_leaves_an_existing_board_byte_identical() {
        let (_d, h) = setup();
        init_board(&h).unwrap();
        add_item(&h, &item("T", "設計判断"), date()).unwrap();
        let before = std::fs::read(board_path(&h)).unwrap();
        assert!(!init_board(&h).unwrap());
        assert_eq!(std::fs::read(board_path(&h)).unwrap(), before);
    }

    #[test]
    fn init_twice_keeps_each_default_heading_once() {
        let (_d, h) = setup();
        init_board(&h).unwrap();
        init_board(&h).unwrap();
        assert_eq!(heading_lines(&read(&h), "## ").len(), 4);
    }

    // --- add: layout (SPEC-RB-004) -------------------------------------

    #[test]
    fn add_writes_the_full_item_layout() {
        let (_d, h) = setup();
        let paths = vec!["review_board_assets/shot.png".to_string()];
        let added = add_item(
            &h,
            &NewReviewItem {
                title: "Pick the layout",
                category: "設計判断",
                body: Some("Which one?\n\nOption A or B."),
                priority: Some("high"),
                assignee: Some("ryoma"),
                task_id: Some("t-1"),
                doc_id: Some("doc-1"),
                image_paths: &paths,
                ..Default::default()
            },
            date(),
        )
        .unwrap();
        assert_eq!(added.id, "R-001");
        let text = read(&h);
        let expected = "\
### Pick the layout <!-- rb-id: R-001 -->
- priority: high
- assignee: ryoma
- task_id: t-1
- doc_id: doc-1
- date: 2026-11-17

Which one?

Option A or B.

![shot.png](review_board_assets/shot.png)

- [ ] 確認済み

メモ:
";
        assert!(text.contains(expected), "got:\n{text}");
    }

    #[test]
    fn add_without_optional_fields_writes_only_the_date_attribute() {
        let (_d, h) = setup();
        add_item(&h, &item("Plain", "その他"), date()).unwrap();
        let text = read(&h);
        for key in ["priority", "assignee", "task_id", "doc_id"] {
            assert!(!text.contains(&format!("- {key}:")), "{key} must be absent");
        }
        assert!(text.contains("- date: 2026-11-17"));
    }

    #[test]
    fn add_ends_the_section_with_checkbox_then_memo() {
        let (_d, h) = setup();
        add_item(&h, &item("A", "その他"), date()).unwrap();
        let text = read(&h);
        let lines: Vec<&str> = text.lines().collect();
        let n = lines.iter().position(|l| *l == CHECKBOX_LINE).unwrap();
        assert_eq!(lines[n + 1], "");
        assert_eq!(lines[n + 2], MEMO_LINE);
    }

    #[test]
    fn attribute_lines_follow_the_heading_directly() {
        let (_d, h) = setup();
        let mut it = item("A", "その他");
        it.priority = Some("high");
        it.assignee = Some("ryoma");
        add_item(&h, &it, date()).unwrap();
        let lines: Vec<String> = read(&h).lines().map(str::to_string).collect();
        let n = lines.iter().position(|l| l.starts_with("### A")).unwrap();
        assert_eq!(lines[n + 1], "- priority: high");
        assert_eq!(lines[n + 2], "- assignee: ryoma");
    }

    #[test]
    fn image_path_with_spaces_uses_angle_brackets() {
        let (_d, h) = setup();
        let paths = vec!["my shots/a (1).png".to_string()];
        let mut it = item("A", "その他");
        it.image_paths = &paths;
        add_item(&h, &it, date()).unwrap();
        assert!(read(&h).contains("![a (1).png](<my shots/a (1).png>)"));
    }

    // --- add: images (RB-010, SPEC-RB-027..030) -------------------------

    fn text_of(h: &Path) -> String {
        read(h)
    }

    fn gitignore(handoff: &Path) -> Option<String> {
        std::fs::read_to_string(handoff.join(".gitignore")).ok()
    }

    fn add_with_images(h: &Path, paths: &[&str]) -> AddedItem {
        let paths: Vec<String> = paths.iter().map(|p| (*p).to_string()).collect();
        let mut it = item("Img", "その他");
        it.image_paths = &paths;
        add_item(h, &it, date()).unwrap()
    }

    #[test]
    fn handoff_prefixed_asset_path_is_written_relative_to_the_board() {
        let (_d, h) = setup();
        add_with_images(&h, &[".handoff/review_board_assets/shot.png"]);
        let text = read(&h);
        assert!(text.contains("![shot.png](review_board_assets/shot.png)"));
        assert!(!text.contains("(.handoff/"));
    }

    #[test]
    fn project_relative_path_is_stored_as_given_and_resolves_from_handoff() {
        let (d, h) = setup();
        add_with_images(&h, &["../design/mockup.png"]);
        assert!(read(&h).contains("![mockup.png](../design/mockup.png)"));
        std::fs::create_dir_all(d.path().join("design")).unwrap();
        std::fs::write(d.path().join("design/mockup.png"), b"x").unwrap();
        assert!(h.join("../design/mockup.png").is_file());
    }

    #[test]
    fn mixed_image_types_keep_order_between_body_and_checkbox() {
        let (_d, h) = setup();
        add_with_images(&h, &["review_board_assets/a.png", "../design/b.png"]);
        let text = read(&h);
        let a = text.find("![a.png](review_board_assets/a.png)").unwrap();
        let b = text.find("![b.png](../design/b.png)").unwrap();
        let cb = text.find(CHECKBOX_LINE).unwrap();
        assert!(a < b && b < cb);
    }

    #[test]
    fn asset_image_adds_the_assets_dir_to_gitignore_once() {
        let (_d, h) = setup();
        std::fs::write(h.join(".gitignore"), "*.lock\n").unwrap();
        add_with_images(&h, &["review_board_assets/a.png"]);
        add_with_images(&h, &[".handoff/review_board_assets/b.png"]);
        assert_eq!(gitignore(&h).unwrap(), "*.lock\nreview_board_assets/\n");
    }

    #[test]
    fn project_relative_image_leaves_gitignore_and_assets_dir_alone() {
        let (_d, h) = setup();
        add_with_images(&h, &["../design/mockup.png"]);
        assert!(gitignore(&h).is_none());
        assert!(!h.join("review_board_assets").exists());
    }

    #[test]
    fn rejected_item_does_not_touch_gitignore() {
        let (_d, h) = setup();
        let paths = vec!["review_board_assets/a.png".to_string(), "<bad>".to_string()];
        let mut it = item("A", "その他");
        it.image_paths = &paths;
        assert!(add_item(&h, &it, date()).is_err());
        assert!(gitignore(&h).is_none());
    }

    #[test]
    fn mixed_images_are_read_back_and_listed() {
        use crate::storage::review_section::{read_item, SectionRef};
        let (_d, h) = setup();
        let added = add_with_images(
            &h,
            &[
                "review_board_assets/a.png",
                "../design/b.png",
                "my dir/c (1).png",
            ],
        );
        let read = read_item(&h, SectionRef::Id(&added.id)).unwrap();
        assert_eq!(
            read.images,
            vec![
                "review_board_assets/a.png",
                "../design/b.png",
                "my dir/c (1).png"
            ]
        );
        let items = crate::storage::review_board_items::parse_items(&text_of(&h));
        assert_eq!(items.len(), 1);
        assert!(items[0].has_images);
    }

    // --- RB-009: lock, concurrency, mixed line endings (SPEC-RB-024..026) ---

    #[test]
    fn add_creates_the_lock_file_and_init_takes_the_lock_too() {
        let (_d, h) = setup();
        init_board(&h).unwrap();
        assert!(crate::storage::review_lock::lock_path(&h).exists());
    }

    #[test]
    fn add_waits_for_a_held_lock_and_then_succeeds() {
        use std::sync::mpsc;
        use std::time::Duration;
        let (_d, h) = setup();
        let guard = crate::storage::review_lock::BoardLock::acquire(&h).unwrap();
        let (tx, rx) = mpsc::channel();
        let h2 = h.clone();
        let t = std::thread::spawn(move || {
            let added = add_item(&h2, &item("Waits", "その他"), date()).unwrap();
            tx.send(added.id).unwrap();
        });
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "add must wait while the lock is held"
        );
        assert!(
            !board_path(&h).exists(),
            "nothing is written before the lock is free"
        );
        drop(guard);
        assert_eq!(rx.recv_timeout(Duration::from_secs(10)).unwrap(), "R-001");
        t.join().unwrap();
    }

    #[test]
    fn concurrent_adds_get_distinct_ids_and_both_land() {
        let (_d, h) = setup();
        let threads: Vec<_> = (0..8)
            .map(|n| {
                let h = h.clone();
                std::thread::spawn(move || {
                    add_item(&h, &item(&format!("T{n}"), "その他"), date())
                        .unwrap()
                        .id
                })
            })
            .collect();
        let mut ids: Vec<String> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 8, "every add gets its own id");
        let text = read(&h);
        for n in 0..8 {
            assert!(
                text.contains(&format!("### T{n} <!--")),
                "T{n} missing:\n{text}"
            );
        }
    }

    #[test]
    fn mixed_line_endings_are_normalized_to_lf_with_a_warning() {
        let (_d, h) = setup();
        std::fs::write(
            board_path(&h),
            "# Board\r\n\r\n## その他\n\n### Old <!-- rb-id: R-001 -->\r\n",
        )
        .unwrap();
        let added = add_item(&h, &item("A", "その他"), date()).unwrap();
        assert!(
            added.warnings.iter().any(|w| w.contains("LF and CRLF")),
            "{:?}",
            added.warnings
        );
        let bytes = std::fs::read(board_path(&h)).unwrap();
        assert!(!bytes.contains(&b'\r'), "no CR left");
        assert!(read(&h).contains("### A <!-- rb-id: R-002 -->\n"));
    }

    #[test]
    fn uniform_boards_give_no_line_ending_warning() {
        let (_d, h) = setup();
        let added = add_item(&h, &item("A", "その他"), date()).unwrap();
        assert!(added.warnings.is_empty(), "{:?}", added.warnings);
    }

    #[test]
    fn a_board_with_a_missing_id_comment_still_accepts_adds_and_warns() {
        let (_d, h) = setup();
        std::fs::write(board_path(&h), "## その他\n\n### Hand typed\n").unwrap();
        let added = add_item(&h, &item("A", "その他"), date()).unwrap();
        assert_eq!(added.id, "R-001");
        assert!(
            added.warnings.iter().any(|w| w.contains("Hand typed")),
            "{:?}",
            added.warnings
        );
    }

    // --- add: ids (SPEC-RB-005) ----------------------------------------

    #[test]
    fn ids_start_at_r001_and_increment() {
        let (_d, h) = setup();
        let a = add_item(&h, &item("A", "設計判断"), date()).unwrap();
        let b = add_item(&h, &item("B", "実装確認"), date()).unwrap();
        assert_eq!((a.id.as_str(), b.id.as_str()), ("R-001", "R-002"));
    }

    #[test]
    fn id_follows_the_highest_id_in_the_file() {
        let (_d, h) = setup();
        init_board(&h).unwrap();
        let mut text = read(&h);
        text.push_str("\n### Old <!-- rb-id: R-007 -->\n- date: 2026-01-01\n");
        std::fs::write(board_path(&h), text).unwrap();
        let added = add_item(&h, &item("N", "その他"), date()).unwrap();
        assert_eq!(added.id, "R-008");
    }

    #[test]
    fn id_skips_ids_reserved_by_the_decisions_file() {
        let (_d, h) = setup();
        std::fs::write(
            h.join(DECISIONS_FILE),
            "### Done <!-- rb-id: R-004 -->\n- date: 2026-01-01\n",
        )
        .unwrap();
        let added = add_item(&h, &item("N", "その他"), date()).unwrap();
        assert_eq!(added.id, "R-005");
    }

    #[test]
    fn ids_quoted_in_body_text_or_code_fences_do_not_bump_the_counter() {
        let (_d, h) = setup();
        std::fs::write(
            board_path(&h),
            "## その他\n\n### Old <!-- rb-id: R-002 -->\n\nmentions rb-id: R-9999 inline\n\n```\n### Fake <!-- rb-id: R-8888 -->\n```\n\n#### Deep <!-- rb-id: R-7777 -->\n",
        )
        .unwrap();
        std::fs::write(h.join(DECISIONS_FILE), "quote: rb-id: R-5000\n").unwrap();
        let added = add_item(&h, &item("N", "その他"), date()).unwrap();
        assert_eq!(added.id, "R-003");
    }

    #[test]
    fn id_grows_past_three_digits() {
        let (_d, h) = setup();
        init_board(&h).unwrap();
        let mut text = read(&h);
        text.push_str("\n### Old <!-- rb-id: R-999 -->\n- date: 2026-01-01\n");
        std::fs::write(board_path(&h), text).unwrap();
        assert_eq!(
            add_item(&h, &item("N", "その他"), date()).unwrap().id,
            "R-1000"
        );
    }

    // --- add: categories (SPEC-RB-005) ---------------------------------

    #[test]
    fn unknown_category_is_appended_as_a_new_section() {
        let (_d, h) = setup();
        init_board(&h).unwrap();
        let added = add_item(&h, &item("A", "Performance"), date()).unwrap();
        assert!(added.category_created);
        let text = read(&h);
        let lines: Vec<&str> = text.lines().collect();
        let cat = lines.iter().position(|l| *l == "## Performance").unwrap();
        let it = lines.iter().position(|l| l.starts_with("### A")).unwrap();
        assert!(it > cat);
        assert_eq!(heading_lines(&text, "## Performance").len(), 1);
    }

    #[test]
    fn existing_category_gets_the_item_inside_without_a_duplicate_heading() {
        let (_d, h) = setup();
        init_board(&h).unwrap();
        let added = add_item(&h, &item("A", "実装確認"), date()).unwrap();
        assert!(!added.category_created);
        let text = read(&h);
        assert_eq!(heading_lines(&text, "## 実装確認").len(), 1);
        let pos = |needle: &str| text.find(needle).unwrap();
        assert!(pos("## 実装確認") < pos("### A"));
        assert!(pos("### A") < pos("## テスト確認"));
    }

    #[test]
    fn second_item_in_a_category_lands_after_the_first_and_before_the_next_category() {
        let (_d, h) = setup();
        add_item(&h, &item("First", "実装確認"), date()).unwrap();
        add_item(&h, &item("Second", "実装確認"), date()).unwrap();
        let text = read(&h);
        let pos = |needle: &str| text.find(needle).unwrap();
        assert!(pos("### First") < pos("### Second"));
        assert!(pos("### Second") < pos("## テスト確認"));
    }

    #[test]
    fn add_does_not_change_bytes_outside_the_insertion() {
        let (_d, h) = setup();
        add_item(&h, &item("First", "設計判断"), date()).unwrap();
        let before = read(&h);
        add_item(&h, &item("Second", "テスト確認"), date()).unwrap();
        let after = read(&h);
        // Everything up to and including the target heading line, and
        // everything after it, survives untouched around the new item.
        let heading = "## テスト確認\n";
        let head_end = before.find(heading).unwrap() + heading.len();
        assert!(after.starts_with(&before[..head_end]));
        assert!(after.ends_with(&before[head_end..]));
        assert!(after[head_end..after.len() - (before.len() - head_end)].contains("### Second"));
    }

    #[test]
    fn crlf_boards_stay_crlf() {
        let (_d, h) = setup();
        std::fs::write(board_path(&h), "# Board\r\n\r\n## その他\r\n").unwrap();
        add_item(&h, &item("A", "その他"), date()).unwrap();
        let text = read(&h);
        assert!(text.contains("### A <!-- rb-id: R-001 -->\r\n"));
        assert!(!text.replace("\r\n", "").contains('\n'), "no bare LF");
    }

    #[test]
    fn category_match_ignores_surrounding_whitespace() {
        let (_d, h) = setup();
        init_board(&h).unwrap();
        let added = add_item(&h, &item("A", "  その他 "), date()).unwrap();
        assert!(!added.category_created);
    }

    #[test]
    fn headings_inside_code_fences_are_not_categories() {
        let (_d, h) = setup();
        std::fs::write(board_path(&h), "## その他\n\n```\n## Fake\n```\n").unwrap();
        let added = add_item(&h, &item("A", "Fake"), date()).unwrap();
        assert!(added.category_created);
    }

    // --- add: board file missing (SPEC-RB-012) -------------------------

    #[test]
    fn add_initializes_a_missing_board_first() {
        let (_d, h) = setup();
        let added = add_item(&h, &item("A", "設計判断"), date()).unwrap();
        assert!(added.board_created);
        let text = read(&h);
        assert_eq!(heading_lines(&text, "## ").len(), 4);
        assert!(text.contains("### A <!-- rb-id: R-001 -->"));
    }

    #[test]
    fn add_on_an_empty_file_creates_the_category() {
        let (_d, h) = setup();
        std::fs::write(board_path(&h), "").unwrap();
        add_item(&h, &item("A", "X"), date()).unwrap();
        assert!(read(&h).starts_with("## X\n\n### A"));
    }

    // --- add: validation leaves the file untouched (SPEC-RB-004 AC5) ---

    #[test]
    fn invalid_input_is_rejected_and_the_board_is_not_touched() {
        let (_d, h) = setup();
        init_board(&h).unwrap();
        let before = std::fs::read(board_path(&h)).unwrap();
        let bad_img = vec!["a\nb.png".to_string()];
        let unbalanced = "```\nunclosed";
        let cases: Vec<NewReviewItem> = vec![
            item("", "その他"),
            item("   ", "その他"),
            item("A", ""),
            item("A", "two\nlines"),
            item("two\nlines", "その他"),
            item("closes --> comment", "その他"),
            item("opens <!-- comment", "その他"),
            NewReviewItem {
                priority: Some("a\nb"),
                ..item("A", "その他")
            },
            NewReviewItem {
                assignee: Some("a\r\nb"),
                ..item("A", "その他")
            },
            NewReviewItem {
                body: Some("ok\n## Injected category"),
                ..item("A", "その他")
            },
            NewReviewItem {
                body: Some("### Injected item"),
                ..item("A", "その他")
            },
            NewReviewItem {
                body: Some(unbalanced),
                ..item("A", "その他")
            },
            NewReviewItem {
                image_paths: &bad_img,
                ..item("A", "その他")
            },
        ];
        for case in cases {
            assert!(
                add_item(&h, &case, date()).is_err(),
                "should reject {case:?}"
            );
            assert_eq!(std::fs::read(board_path(&h)).unwrap(), before);
        }
    }

    #[test]
    fn invalid_input_does_not_create_a_missing_board() {
        let (_d, h) = setup();
        assert!(add_item(&h, &item("", "その他"), date()).is_err());
        assert!(!board_path(&h).exists());
    }

    #[test]
    fn a_level_one_heading_in_the_body_is_rejected_but_not_inside_a_fence() {
        let (_d, h) = setup();
        let mut it = item("A", "その他");
        it.body = Some("text\n# Title");
        assert!(add_item(&h, &it, date()).is_err());
        it.body = Some("```sh\n# a shell comment\n## also fine\n```");
        add_item(&h, &it, date()).unwrap();
        assert!(read(&h).contains("# a shell comment"));
    }

    #[test]
    fn a_level_one_heading_ends_the_category_section_for_insertion() {
        let (_d, h) = setup();
        std::fs::write(board_path(&h), "## A\n\n# Appendix\n\ntext\n").unwrap();
        add_item(&h, &item("N", "A"), date()).unwrap();
        let text = read(&h);
        assert!(text.find("### N").unwrap() < text.find("# Appendix").unwrap());
    }

    #[test]
    fn deeper_headings_and_fenced_code_are_allowed_in_the_body() {
        let (_d, h) = setup();
        let mut it = item("A", "その他");
        it.body = Some("#### Detail\n\n```rust\nlet x = 1;\n```");
        add_item(&h, &it, date()).unwrap();
        assert!(read(&h).contains("#### Detail"));
    }

    #[test]
    fn no_temp_file_is_left_behind() {
        let (_d, h) = setup();
        add_item(&h, &item("A", "その他"), date()).unwrap();
        let names: Vec<String> = std::fs::read_dir(&h)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        // The board and its lock file only; no `.tmp` leftovers.
        let mut expected = vec![
            BOARD_FILE.to_string(),
            crate::storage::review_lock::LOCK_FILE.to_string(),
        ];
        let mut names = names;
        names.sort();
        expected.sort();
        assert_eq!(names, expected);
    }
}
