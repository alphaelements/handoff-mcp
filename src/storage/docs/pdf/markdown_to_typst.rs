//! Markdown to Typst markup conversion (REQ-PDF-005 / 008 / 010 / 013).
//!
//! This module converts only the *content* of one handoff document into Typst
//! markup. Page layout (cover, table of contents, header/footer, heading
//! styles) belongs to the Typst template, and the `typst compile` invocation
//! to a later pipeline stage; the intermediate `.typ` is regenerated on every
//! export and is never edited by hand (REQ-PDF-011).
//!
//! # Design
//!
//! * **Metadata never reaches the page** (REQ-PDF-005 AC2). A leading YAML
//!   frontmatter block and the handoff attribute block directly under a
//!   heading (`- priority:`, `- refines:`, ... — the keys recognised by
//!   [`layer_parse::known_attr_key`]) are removed by a line-based pre-pass
//!   before Markdown parsing. Code fences are skipped, so a fenced example of
//!   those lines survives.
//! * **Function-form markup everywhere it matters.** Emphasis, links, lists
//!   and tables are emitted as `#emph[..]`, `#list(..)`, `#table(..)` rather
//!   than `*..*` / `- ..` shorthand: the shorthand is indentation- and
//!   word-boundary-sensitive, which breaks on nested lists and on CJK text
//!   where there are no spaces around emphasis. Every inline call ends with
//!   `;` because Typst would otherwise continue parsing a following `.name`,
//!   `(` or `[` as a field access or extra call arguments.
//! * **Code is data.** Code blocks / inline code are `#raw("..")` string
//!   literals, so no fence-length or backtick escaping problem exists.
//! * **Typst passthrough** (REQ-PDF-010): a fenced block whose info string
//!   starts with `typst` is emitted verbatim.
//! * **Images** are validated against the project root and emitted as
//!   root-absolute paths (`/docs/a.png`); the later `typst compile` step must
//!   therefore pass `--root <project_root>`. A missing, unsupported or
//!   out-of-root image degrades to its path as text plus a [`PdfWarning`]
//!   (REQ-PDF-008 AC3, REQ-PDF-013 AC3) — it never aborts the conversion.
//! * Mermaid blocks are intentionally *not* handled here; they come out as an
//!   ordinary `lang: "mermaid"` code block, which is the REQ-PDF-003 AC2
//!   fallback, so the SVG pipeline can substitute rendered diagrams later.

use std::path::{Component, Path, PathBuf};

use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

use super::req_meta::{
    parse_acceptance_bullet, passthrough_block, AcceptanceItem, ReqMeta, ReqMetaIndex,
};
use super::types::{ConversionResult, PdfWarning, PdfWarningCode};
use crate::storage::docs::layer_parse::{
    find_acceptance_block, is_ac_trigger_line, is_bullet_line, known_attr_key, strip_bullet_marker,
};

/// Image extensions Typst can embed that REQ-PDF-008 AC2 commits to.
const SUPPORTED_IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "svg"];

/// Info-string language that triggers the Typst passthrough (REQ-PDF-010).
const TYPST_PASSTHROUGH_LANG: &str = "typst";

/// Longest HTML snippet echoed back in a warning's `detail`.
const WARNING_DETAIL_MAX_CHARS: usize = 80;

/// Estimated width, in character cells, a table may have before it no longer
/// fits a portrait page and is moved to a landscape one (REQ-PDF-029). A4
/// portrait with the built-in margins holds roughly 85-90 body characters.
const LANDSCAPE_TABLE_WIDTH_CHARS: usize = 100;

/// A single column never counts for more than this many character cells in
/// the width estimate: longer text wraps instead of widening the table.
const COLUMN_WIDTH_ESTIMATE_CAP: usize = 24;

/// Horizontal padding, in character cells, added per column to the estimate.
const COLUMN_PADDING_CHARS: usize = 2;

/// An ID cell longer than this (in character cells) is cut and ends in `…`
/// when its table is laid out as a wide table.
const WIDE_TABLE_ID_MAX_CHARS: usize = 18;

/// A title cell longer than this (in character cells) is cut and ends in `…`
/// when its table is laid out as a wide table.
const WIDE_TABLE_TITLE_MAX_CHARS: usize = 60;

/// Fixed width of an ID column in a wide table.
const WIDE_TABLE_ID_COLUMN_WIDTH: &str = "3.8cm";

/// Text size inside a wide table (the body is 10.5pt).
const WIDE_TABLE_FONT_SIZE: &str = "8pt";

/// Cell padding inside a wide table.
const WIDE_TABLE_INSET: &str = "4pt";

/// Marker appended to a cell that was cut to fit.
const ELLIPSIS: char = '\u{2026}';

/// Header texts (compared case-insensitively) that mark an ID column.
const ID_HEADERS: &[&str] = &["id", "item id", "item_id", "item", "stable id", "stable_id"];

/// Header texts (compared case-insensitively) that mark a title column.
const TITLE_HEADERS: &[&str] = &["title"];

pub struct ConvertOptions<'a> {
    /// Project root: relative image paths resolve against it, and Typst must
    /// be invoked with `--root` set to it.
    pub project_root: &'a Path,
    /// The document is placed under a level-1 section heading with this
    /// title (a bundled document). Its headings then start below that level:
    /// a leading `# <title>` that repeats the section heading is dropped and
    /// the rest keep their levels (`##` stays level 2; a later `#` is raised
    /// to level 2 so it cannot pose as another document); without such a
    /// leading heading the document's headings are moved as a block so its
    /// shallowest one lands on level 2 (a document that starts at `##` keeps
    /// its levels; one that starts at `#` moves one level down). `None`: the
    /// Markdown levels are kept.
    pub section_title: Option<&'a str>,
    /// Meta data of the requirement items (REQ-PDF-030), keyed by stable id.
    /// For a heading that starts with one of these ids, the attribute lines
    /// and the acceptance criteria below it are replaced by one `req-meta()`
    /// call placed directly under the heading. `None`: the attribute lines
    /// are just dropped (REQ-PDF-005).
    pub req_meta: Option<&'a ReqMetaIndex>,
}

impl<'a> ConvertOptions<'a> {
    /// A plain conversion: Markdown heading levels kept, nothing dropped.
    pub fn new(project_root: &'a Path) -> Self {
        Self {
            project_root,
            section_title: None,
            req_meta: None,
        }
    }
}

/// Converts a Markdown document body into Typst markup.
///
/// Infallible by design: every problem is reported as a warning in the
/// returned [`ConversionResult`] and the affected element degrades to text.
pub fn markdown_to_typst(markdown: &str, options: &ConvertOptions<'_>) -> ConversionResult {
    let mut stripped = strip_handoff_metadata(markdown, options.req_meta);
    // Whether the dropped `#` stood for the section heading itself.
    let mut title_dropped = false;
    if let Some(title) = options.section_title {
        if let Some(rest) = drop_leading_title(&stripped, title) {
            stripped = rest;
            title_dropped = true;
        }
    }
    let events: Vec<Event<'_>> = Parser::new_ext(
        &stripped,
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH,
    )
    .collect();
    let headings = match options.section_title {
        None => HeadingShift::NONE,
        Some(_) if title_dropped => HeadingShift::UNDER_SECTION,
        Some(_) => HeadingShift::below_section(shallowest_heading(&events)),
    };
    let mut renderer = Renderer {
        events,
        pos: 0,
        root: options.project_root,
        headings,
        // `None` when the root cannot be canonicalized (e.g. it does not
        // exist); every image then fails the containment check and is
        // reported, rather than being trusted without verification.
        canonical_root: options.project_root.canonicalize().ok(),
        warnings: Vec::new(),
        nesting: 0,
    };
    let mut typst = String::new();
    renderer.blocks(&mut typst);
    ConversionResult {
        typst,
        warnings: renderer.warnings,
    }
}

// -- metadata pre-pass ---------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum AttrScan {
    /// Not directly under a heading.
    Idle,
    /// Just after a heading; blank lines may still precede the attribute block.
    AfterHeading,
    /// Inside the consecutive bullet run that follows a heading.
    InBlock,
}

/// The requirement meta block being collected for the section under the
/// heading just passed (REQ-PDF-030).
struct PendingMeta<'m> {
    meta: &'m ReqMeta,
    /// Byte offset in the output just after the heading line: where the block
    /// goes once the section's acceptance criteria are known.
    insert_at: usize,
    acceptance: Vec<AcceptanceItem>,
    /// The section's acceptance paragraph was taken (only the first counts,
    /// as in the layer parser).
    paragraph_taken: bool,
}

impl PendingMeta<'_> {
    /// Writes the block into `out` (nothing when there is nothing to show).
    fn finish(self, out: &mut String) {
        if let Some(call) = self.meta.render_call(&self.acceptance) {
            out.insert_str(self.insert_at, &format!("\n{}\n", passthrough_block(&call)));
        }
    }
}

/// The meta of the item a heading line names, if it starts with the stable id
/// of one: `## REQ-001 Title`, `## REQ-001: Title`, `## REQ-001.`.
fn heading_meta<'m>(line: &str, index: &'m ReqMetaIndex) -> Option<&'m ReqMeta> {
    let text = line.trim_start().trim_start_matches('#').trim();
    let token = text.split(char::is_whitespace).next()?;
    [
        token,
        token.trim_end_matches([':', '.', '\u{ff1a}']),
        token.split([':', '\u{ff1a}']).next().unwrap_or(token),
    ]
    .into_iter()
    .find_map(|candidate| index.get(candidate))
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Key of an attribute bullet (`- acceptance_criteria: ...` -> the key).
fn bullet_key(line: &str) -> Option<&str> {
    strip_bullet_marker(line)
        .split_once(':')
        .map(|(key, _)| key.trim())
}

/// Reads the criteria nested under the `- acceptance_criteria:` bullet at
/// `lines[0]`: the text after its colon (if any) plus every following line
/// indented deeper than the bullet, blank lines between them allowed. Returns
/// the criteria and how many lines were consumed.
fn take_attribute_acceptance(lines: &[&str]) -> (Vec<AcceptanceItem>, usize) {
    let head = lines[0];
    let base = indent_of(head);
    let mut raw: Vec<String> = Vec::new();
    let inline = head
        .split_once(':')
        .map(|(_, rest)| rest.trim())
        .unwrap_or_default();
    if !inline.is_empty() {
        raw.push(inline.to_string());
    }
    let mut consumed = 1;
    let mut i = 1;
    while i < lines.len() {
        let line = lines[i];
        if line.trim().is_empty() {
            i += 1;
            continue;
        }
        if indent_of(line) <= base {
            break;
        }
        if is_bullet_line(line) {
            raw.push(strip_bullet_marker(line).trim().to_string());
        } else if let Some(last) = raw.last_mut() {
            last.push(' ');
            last.push_str(line.trim());
        }
        i += 1;
        consumed = i;
    }
    (
        raw.iter().map(|r| parse_acceptance_bullet(r)).collect(),
        consumed,
    )
}

/// Removes leading YAML frontmatter and the attribute block under each heading.
///
/// The attribute rule mirrors `layer_parse`: after a heading (blank lines
/// allowed), a strictly consecutive run of bullet lines is the attribute
/// block, of which only the known keys are removed.
///
/// With `req_meta` (REQ-PDF-030), a heading that starts with the stable id of
/// an item in the index additionally gets a `req-meta()` call directly under
/// it, and the acceptance criteria of its section - the `- acceptance_criteria:`
/// attribute and the first "受入基準:" paragraph with its bullets - are moved
/// out of the body into that call. Everything inside code fences is left alone.
fn strip_handoff_metadata(markdown: &str, req_meta: Option<&ReqMetaIndex>) -> String {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut start = 0;
    if lines.first().is_some_and(|l| l.trim_end() == "---") {
        if let Some(rel) = lines[1..]
            .iter()
            .position(|l| matches!(l.trim_end(), "---" | "..."))
        {
            start = rel + 2;
        }
    }

    let mut out = String::with_capacity(markdown.len());
    let mut fence: Option<(char, usize)> = None;
    let mut scan = AttrScan::Idle;
    let mut pending: Option<PendingMeta<'_>> = None;
    let mut i = start;
    while i < lines.len() {
        let line = lines[i];
        if let Some((ch, len)) = fence {
            if is_closing_fence(line, ch, len) {
                fence = None;
            }
            push_line(&mut out, line);
            i += 1;
            continue;
        }
        if let Some(open) = opening_fence(line) {
            fence = Some(open);
            scan = AttrScan::Idle;
            push_line(&mut out, line);
            i += 1;
            continue;
        }
        if let Some(section) = pending.as_mut() {
            if !section.paragraph_taken && is_ac_trigger_line(line) {
                if let Some(block) = find_acceptance_block(&lines[i..]).filter(|b| b.start_idx == 0)
                {
                    section.paragraph_taken = true;
                    section
                        .acceptance
                        .extend(block.raw_items.iter().map(|r| parse_acceptance_bullet(r)));
                    i += block.end_idx;
                    continue;
                }
            }
        }
        if scan == AttrScan::AfterHeading && line.trim().is_empty() {
            push_line(&mut out, line);
            i += 1;
            continue;
        }
        if matches!(scan, AttrScan::AfterHeading | AttrScan::InBlock) && is_bullet_line(line) {
            scan = AttrScan::InBlock;
            if let Some(section) = pending
                .as_mut()
                .filter(|_| bullet_key(line) == Some("acceptance_criteria"))
            {
                let (items, consumed) = take_attribute_acceptance(&lines[i..]);
                section.acceptance.extend(items);
                i += consumed;
                continue;
            }
            if known_attr_key(line).is_none() {
                push_line(&mut out, line);
            }
            i += 1;
            continue;
        }
        if is_atx_heading(line) {
            scan = AttrScan::AfterHeading;
            if let Some(section) = pending.take() {
                section.finish(&mut out);
            }
            push_line(&mut out, line);
            pending = req_meta
                .and_then(|index| heading_meta(line, index))
                .map(|meta| PendingMeta {
                    meta,
                    insert_at: out.len(),
                    acceptance: Vec::new(),
                    paragraph_taken: false,
                });
        } else {
            scan = AttrScan::Idle;
            push_line(&mut out, line);
        }
        i += 1;
    }
    if let Some(section) = pending {
        section.finish(&mut out);
    }
    out
}

/// Heading-level mapping for a document placed under a section heading.
#[derive(Debug, Clone, Copy)]
struct HeadingShift {
    /// Levels every heading moves down by (negative: up).
    offset: isize,
    /// Shallowest level a heading may end up at.
    floor: usize,
}

impl HeadingShift {
    /// Markdown levels as they are.
    const NONE: Self = Self {
        offset: 0,
        floor: 1,
    };
    /// The headings move as a block so the shallowest one (`shallowest`, a
    /// Markdown level; `None` when there are no headings) sits one level
    /// below the section heading, i.e. on level 2.
    fn below_section(shallowest: Option<usize>) -> Self {
        Self {
            offset: shallowest.map_or(1, |level| 2 - level as isize),
            floor: 2,
        }
    }
    /// The document's own title heading was dropped for the section heading,
    /// so its children are already one level below it.
    const UNDER_SECTION: Self = Self {
        offset: 0,
        floor: 2,
    };

    fn apply(self, level: usize) -> usize {
        level.saturating_add_signed(self.offset).max(self.floor)
    }
}

/// The shallowest heading level (1-6) among `events`, if there is a heading.
fn shallowest_heading(events: &[Event<'_>]) -> Option<usize> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Start(Tag::Heading { level, .. }) => Some(*level as usize),
            _ => None,
        })
        .min()
}

/// `markdown` without its first non-blank line when that line is the
/// level-1 ATX heading `# <title>` (optional closing `#`s, surrounding
/// whitespace ignored); `None` when it is anything else. `markdown` must
/// already be stripped of frontmatter.
fn drop_leading_title(markdown: &str, title: &str) -> Option<String> {
    let mut lines = markdown.lines();
    let first = lines.by_ref().find(|line| !line.trim().is_empty())?;
    let heading_text = first
        .trim()
        .strip_prefix("# ")
        .map(|rest| rest.trim_end_matches('#').trim());
    if heading_text != Some(title.trim()) {
        return None;
    }
    let mut out = String::with_capacity(markdown.len());
    for line in lines {
        push_line(&mut out, line);
    }
    Some(out)
}

fn push_line(out: &mut String, line: &str) {
    out.push_str(line);
    out.push('\n');
}

fn is_atx_heading(line: &str) -> bool {
    let t = line.trim_start();
    let hashes = t.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&hashes) && t[hashes..].starts_with([' ', '\t'])
}

/// `(fence char, run length)` when `line` opens a fenced code block.
fn opening_fence(line: &str) -> Option<(char, usize)> {
    let t = line.trim_start();
    let ch = t.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let len = t.chars().take_while(|c| *c == ch).count();
    // A backtick fence's info string cannot contain a backtick (that would be
    // inline code such as ```x```).
    (len >= 3 && !(ch == '`' && t[len..].contains('`'))).then_some((ch, len))
}

fn is_closing_fence(line: &str, ch: char, min_len: usize) -> bool {
    let t = line.trim();
    t.chars().count() >= min_len && t.chars().all(|c| c == ch)
}

// -- renderer ------------------------------------------------------------------

struct Renderer<'a> {
    events: Vec<Event<'a>>,
    pos: usize,
    root: &'a Path,
    headings: HeadingShift,
    canonical_root: Option<PathBuf>,
    warnings: Vec<PdfWarning>,
    /// Number of enclosing block containers (list items, block quotes). A
    /// table can only move to its own page at nesting 0, because Typst does
    /// not allow `page` inside a container.
    nesting: usize,
}

/// One rendered table cell.
struct TableCell {
    /// Typst markup of the cell.
    markup: String,
    /// The cell's text when it holds nothing but plain text (no emphasis,
    /// links, code, line breaks), so it can be cut and re-escaped safely.
    plain: Option<String>,
    /// Display width of the cell's visible text, in character cells.
    width: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ColumnRole {
    Id,
    Title,
    Other,
}

impl<'a> Renderer<'a> {
    fn current(&self) -> Option<Event<'a>> {
        self.events.get(self.pos).cloned()
    }

    fn skip_end(&mut self) {
        if matches!(self.events.get(self.pos), Some(Event::End(_))) {
            self.pos += 1;
        }
    }

    /// Renders sibling blocks until the enclosing element's `End` event (left
    /// unconsumed for the caller) or the end of the document.
    fn blocks(&mut self, out: &mut String) {
        while let Some(event) = self.current() {
            match event {
                Event::End(_) => return,
                Event::Start(tag) if !is_inline_tag(&tag) => {
                    self.pos += 1;
                    self.block(tag, out);
                }
                Event::Rule => {
                    self.pos += 1;
                    begin_block(out);
                    out.push_str("#line(length: 100%)\n\n");
                }
                Event::Html(html) => {
                    self.pos += 1;
                    self.html_block(&html, out);
                }
                // Bare inline content: the text of a tight list item.
                _ if is_inline_event(&event) => {
                    begin_block(out);
                    self.inline_run(out);
                    out.push('\n');
                }
                _ => self.pos += 1,
            }
        }
    }

    fn block(&mut self, tag: Tag<'a>, out: &mut String) {
        match tag {
            Tag::Paragraph => {
                if self.try_figure(out) {
                    return;
                }
                begin_block(out);
                self.inline_run(out);
                self.skip_end();
                out.push_str("\n\n");
            }
            Tag::Heading { level, .. } => {
                begin_block(out);
                out.push_str(&"=".repeat(self.headings.apply(level as usize)));
                out.push(' ');
                self.inline_run(out);
                self.skip_end();
                out.push_str("\n\n");
            }
            Tag::BlockQuote(_) => {
                begin_block(out);
                let mut inner = String::new();
                self.nesting += 1;
                self.blocks(&mut inner);
                self.nesting -= 1;
                self.skip_end();
                out.push_str("#quote(block: true)[");
                out.push_str(inner.trim_end());
                out.push_str("]\n\n");
            }
            Tag::CodeBlock(kind) => self.code_block(&kind, out),
            Tag::List(first) => self.list(first, out),
            Tag::Table(alignments) => self.table(&alignments, out),
            Tag::HtmlBlock => {
                let mut html = String::new();
                while let Some(event) = self.current() {
                    match event {
                        Event::Html(text) | Event::Text(text) => html.push_str(&text),
                        _ => break,
                    }
                    self.pos += 1;
                }
                self.skip_end();
                self.html_block(&html, out);
            }
            // Items, definition lists, footnotes, metadata blocks: not
            // enabled or not meaningful here; keep the children's content.
            _ => {
                self.blocks(out);
                self.skip_end();
            }
        }
    }

    // -- code ------------------------------------------------------------------

    fn code_block(&mut self, kind: &CodeBlockKind<'_>, out: &mut String) {
        let mut code = String::new();
        while let Some(Event::Text(text)) = self.current() {
            code.push_str(&text);
            self.pos += 1;
        }
        self.skip_end();
        let code = code.replace("\r\n", "\n");
        let lang = match kind {
            CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or(""),
            CodeBlockKind::Indented => "",
        };
        begin_block(out);
        if lang.eq_ignore_ascii_case(TYPST_PASSTHROUGH_LANG) {
            let body = code.trim_end_matches('\n');
            if !body.is_empty() {
                out.push_str(body);
                out.push_str("\n\n");
            }
            return;
        }
        let code = code.strip_suffix('\n').unwrap_or(&code);
        let lang: String = lang
            .chars()
            .filter(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '+' | '#' | '.'))
            .collect();
        out.push_str("#raw(block: true, ");
        if !lang.is_empty() {
            out.push_str("lang: ");
            out.push_str(&typst_string(&lang));
            out.push_str(", ");
        }
        out.push_str(&typst_string(code));
        out.push_str(")\n\n");
    }

    // -- lists -----------------------------------------------------------------

    /// `true` when any item of the list whose `Start` was just consumed has a
    /// paragraph as a direct child (CommonMark's "loose" list).
    fn list_is_loose(&self) -> bool {
        let mut depth = 0usize;
        for event in &self.events[self.pos..] {
            match event {
                Event::Start(tag) => {
                    depth += 1;
                    if depth == 2 && matches!(tag, Tag::Paragraph) {
                        return true;
                    }
                }
                Event::End(_) => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
        false
    }

    fn list(&mut self, first: Option<u64>, out: &mut String) {
        let loose = self.list_is_loose();
        let mut items: Vec<String> = Vec::new();
        while let Some(Event::Start(Tag::Item)) = self.current() {
            self.pos += 1;
            let mut item = String::new();
            self.nesting += 1;
            self.blocks(&mut item);
            self.nesting -= 1;
            self.skip_end();
            items.push(item.trim_end().to_string());
        }
        self.skip_end();

        begin_block(out);
        out.push_str(if first.is_some() {
            "#enum(\n"
        } else {
            "#list(\n"
        });
        if loose {
            out.push_str("  tight: false,\n");
        }
        if let Some(start) = first.filter(|n| *n != 1) {
            out.push_str(&format!("  start: {start},\n"));
        }
        for item in &items {
            out.push_str("  [");
            out.push_str(item);
            out.push_str("],\n");
        }
        out.push_str(")\n\n");
    }

    // -- tables ----------------------------------------------------------------

    fn table_cells(&mut self, end: TagEnd) -> Vec<TableCell> {
        let mut cells = Vec::new();
        while let Some(event) = self.current() {
            match event {
                Event::Start(Tag::TableCell) => {
                    self.pos += 1;
                    let (plain, width) = self.cell_text();
                    let mut markup = String::new();
                    self.inline_run(&mut markup);
                    self.skip_end();
                    cells.push(TableCell {
                        markup,
                        plain,
                        width,
                    });
                }
                Event::End(e) if e == end => {
                    self.pos += 1;
                    break;
                }
                _ => break,
            }
        }
        cells
    }

    /// Looks ahead over the cell that starts at the current position (without
    /// consuming it) and returns its plain text, if it is plain, together with
    /// the display width of all its visible text.
    fn cell_text(&self) -> (Option<String>, usize) {
        let mut text = String::new();
        let mut width = 0;
        let mut plain = true;
        for event in &self.events[self.pos..] {
            match event {
                Event::End(TagEnd::TableCell) => break,
                Event::Text(t) => {
                    text.push_str(t);
                    width += display_width(t);
                }
                Event::Code(t) => {
                    plain = false;
                    width += display_width(t);
                }
                Event::Start(_) | Event::End(_) => plain = false,
                _ => plain = false,
            }
        }
        (plain.then_some(text), width)
    }

    fn table(&mut self, alignments: &[Alignment], out: &mut String) {
        let columns = alignments.len().max(1);
        let mut header: Vec<TableCell> = Vec::new();
        let mut rows: Vec<Vec<TableCell>> = Vec::new();
        while let Some(event) = self.current() {
            match event {
                Event::Start(Tag::TableHead) => {
                    self.pos += 1;
                    header = self.table_cells(TagEnd::TableHead);
                }
                Event::Start(Tag::TableRow) => {
                    self.pos += 1;
                    rows.push(self.table_cells(TagEnd::TableRow));
                }
                _ => break,
            }
        }
        self.skip_end();

        let roles = (0..columns)
            .map(|i| column_role(header.get(i)))
            .collect::<Vec<_>>();
        // A table that would not fit a portrait page gets its own landscape
        // page, fixed-width ID columns, shared-width title columns and cut
        // ID/title text (REQ-PDF-029). Inside a container this is impossible,
        // so such tables keep the plain layout.
        let wide = self.nesting == 0
            && estimated_width(&header, &rows, &roles, columns) > LANDSCAPE_TABLE_WIDTH_CHARS;
        if wide {
            for row in &mut rows {
                for (cell, role) in row.iter_mut().zip(&roles) {
                    shorten_cell(cell, *role);
                }
            }
            for (cell, role) in header.iter_mut().zip(&roles) {
                shorten_cell(cell, *role);
            }
        }

        let align = alignments
            .iter()
            .map(|a| match a {
                Alignment::None => "auto",
                Alignment::Left => "left",
                Alignment::Center => "center",
                Alignment::Right => "right",
            })
            .collect::<Vec<_>>();
        let tuple = |items: &[&str]| {
            if items.len() == 1 {
                format!("({},)", items[0])
            } else {
                format!("({})", items.join(", "))
            }
        };
        let row_line = |cells: &[TableCell]| {
            (0..columns)
                .map(|i| format!("[{}]", cells.get(i).map_or("", |c| c.markup.as_str())))
                .collect::<Vec<_>>()
        };
        let widths = roles
            .iter()
            .map(|role| match (wide, role) {
                (true, ColumnRole::Id) => WIDE_TABLE_ID_COLUMN_WIDTH,
                (true, ColumnRole::Title) => "1fr",
                _ => "auto",
            })
            .collect::<Vec<_>>();

        begin_block(out);
        let indent = if wide {
            out.push_str("#page(flipped: true)[\n");
            out.push_str(&format!(
                "#set text(size: {WIDE_TABLE_FONT_SIZE})\n#set table(inset: {WIDE_TABLE_INSET})\n"
            ));
            "  "
        } else {
            ""
        };
        out.push_str("#table(\n");
        out.push_str(&format!("{indent}  columns: {},\n", tuple(&widths)));
        out.push_str(&format!("{indent}  align: {},\n", tuple(&align)));
        out.push_str(&format!(
            "{indent}  table.header({}),\n",
            row_line(&header).join(", ")
        ));
        for row in &rows {
            out.push_str(&format!("{indent}  {},\n", row_line(row).join(", ")));
        }
        out.push_str(")\n");
        if wide {
            out.push_str("]\n");
        }
        out.push('\n');
    }

    // -- inline ----------------------------------------------------------------

    /// Renders consecutive inline events, stopping at the first block `Start`
    /// or any `End` (left unconsumed).
    fn inline_run(&mut self, out: &mut String) {
        while let Some(event) = self.current() {
            match event {
                Event::Text(text) => {
                    self.pos += 1;
                    push_escaped(out, &text);
                }
                Event::Code(code) => {
                    self.pos += 1;
                    out.push_str("#raw(");
                    out.push_str(&typst_string(&code));
                    out.push_str(");");
                }
                Event::SoftBreak => {
                    self.pos += 1;
                    self.soft_break(out);
                }
                Event::HardBreak => {
                    self.pos += 1;
                    out.push_str("\\\n");
                }
                Event::InlineHtml(html) => {
                    self.pos += 1;
                    self.inline_html(&html, out);
                }
                Event::Start(Tag::Emphasis) => self.wrapped("#emph[", out),
                Event::Start(Tag::Strong) => self.wrapped("#strong[", out),
                Event::Start(Tag::Strikethrough) => self.wrapped("#strike[", out),
                Event::Start(Tag::Link { dest_url, .. }) => {
                    self.pos += 1;
                    if is_external_link(&dest_url) {
                        out.push_str("#link(");
                        out.push_str(&typst_string(&dest_url));
                        out.push_str(")[");
                        self.inline_run(out);
                        out.push_str("];");
                    } else {
                        // Relative / fragment links have no target in a PDF;
                        // keep the visible text only.
                        self.inline_run(out);
                    }
                    self.skip_end();
                }
                Event::Start(Tag::Image { dest_url, .. }) => {
                    self.pos += 1;
                    let alt = self.plain_text_until_end();
                    match self.resolve_image(&dest_url) {
                        // `image` is block-level in Typst; boxing keeps it in the line.
                        Some(path) => out.push_str(&format!("#box({});", image_call(&path, &alt))),
                        None => push_escaped(out, &dest_url),
                    }
                }
                _ => return,
            }
        }
    }

    fn wrapped(&mut self, open: &str, out: &mut String) {
        self.pos += 1;
        out.push_str(open);
        self.inline_run(out);
        self.skip_end();
        out.push_str("];");
    }

    /// A line break inside a paragraph is a space in Markdown, but Typst would
    /// render that space between two CJK characters, where it does not belong.
    fn soft_break(&self, out: &mut String) {
        let next = match self.events.get(self.pos) {
            Some(Event::Text(text)) => text.chars().next(),
            _ => None,
        };
        let joins_cjk = out.chars().last().is_some_and(is_cjk) && next.is_some_and(is_cjk);
        if !joins_cjk {
            out.push('\n');
        }
    }

    /// Text content of an element whose `Start` was just consumed; consumes
    /// through its matching `End`.
    fn plain_text_until_end(&mut self) -> String {
        let mut text = String::new();
        let mut depth = 0usize;
        while let Some(event) = self.current() {
            self.pos += 1;
            match event {
                Event::Start(_) => depth += 1,
                Event::End(_) if depth == 0 => break,
                Event::End(_) => depth -= 1,
                Event::Text(t) | Event::Code(t) => text.push_str(&t),
                Event::SoftBreak | Event::HardBreak => text.push(' '),
                _ => {}
            }
        }
        text
    }

    // -- images ----------------------------------------------------------------

    /// An image that is the only content of its paragraph becomes a figure
    /// (captioned by its alt text). Returns `false`, consuming nothing, when
    /// the paragraph is not a lone image.
    fn try_figure(&mut self, out: &mut String) -> bool {
        let Some(Event::Start(Tag::Image { dest_url, .. })) = self.current() else {
            return false;
        };
        let mut depth = 0usize;
        let mut image_end = None;
        for (offset, event) in self.events[self.pos..].iter().enumerate() {
            match event {
                Event::Start(_) => depth += 1,
                Event::End(_) => {
                    depth -= 1;
                    if depth == 0 {
                        image_end = Some(self.pos + offset);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(image_end) = image_end else {
            return false;
        };
        if !matches!(
            self.events.get(image_end + 1),
            Some(Event::End(TagEnd::Paragraph))
        ) {
            return false;
        }

        self.pos += 1;
        let alt = self.plain_text_until_end();
        self.skip_end();
        begin_block(out);
        match self.resolve_image(&dest_url) {
            Some(path) => {
                out.push_str("#figure(");
                out.push_str(&image_call(&path, &alt));
                if !alt.is_empty() {
                    out.push_str(", caption: [");
                    push_escaped(out, &alt);
                    out.push(']');
                }
                out.push(')');
            }
            None => push_escaped(out, &dest_url),
        }
        out.push_str("\n\n");
        true
    }

    /// Validates an image reference and returns its project-root-absolute
    /// Typst path, or `None` (after recording a warning) when it must degrade
    /// to a text placeholder.
    fn resolve_image(&mut self, dest: &str) -> Option<String> {
        if is_remote_image(dest) {
            self.warn(
                PdfWarningCode::ImageUnsupported,
                "remote or embedded image URLs cannot be embedded in the PDF",
                dest,
            );
            return None;
        }
        let supported = Path::new(dest)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| {
                SUPPORTED_IMAGE_EXTENSIONS
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case(e))
            });
        if !supported {
            self.warn(
                PdfWarningCode::ImageUnsupported,
                "unsupported image format (supported: SVG, PNG, JPEG)",
                dest,
            );
            return None;
        }

        let mut candidates = vec![dest.to_string()];
        if let Some(decoded) = percent_decode(dest).filter(|d| d != dest) {
            candidates.push(decoded);
        }
        for candidate in &candidates {
            let Some(relative) = self.project_relative(candidate) else {
                self.warn(
                    PdfWarningCode::ImageOutsideProject,
                    "image path resolves outside the project root",
                    dest,
                );
                return None;
            };
            let full = self.root.join(&relative);
            if !full.is_file() {
                continue;
            }
            let inside = match (full.canonicalize(), &self.canonical_root) {
                (Ok(real), Some(root)) => real.starts_with(root),
                _ => false,
            };
            if !inside {
                self.warn(
                    PdfWarningCode::ImageOutsideProject,
                    "image path resolves outside the project root",
                    dest,
                );
                return None;
            }
            let parts: Vec<String> = relative
                .components()
                .filter_map(|c| match c {
                    Component::Normal(p) => Some(p.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect();
            return Some(format!("/{}", parts.join("/")));
        }
        self.warn(
            PdfWarningCode::ImageNotFound,
            "image file not found; its path is shown instead",
            dest,
        );
        None
    }

    /// Lexically normalizes `path` against the project root and returns it as
    /// a root-relative path, or `None` if it climbs above the root.
    fn project_relative(&self, path: &str) -> Option<PathBuf> {
        let path = Path::new(path);
        let rooted: &Path = if path.is_absolute() {
            // An absolute path inside the project is accepted as-is; any
            // other leading `/` means "relative to the project root".
            path.strip_prefix(self.root)
                .ok()
                .or_else(|| {
                    self.canonical_root
                        .as_deref()
                        .and_then(|r| path.strip_prefix(r).ok())
                })
                .unwrap_or(path)
        } else {
            path
        };
        let mut normalized = PathBuf::new();
        for component in rooted.components() {
            match component {
                Component::Normal(part) => normalized.push(part),
                Component::CurDir | Component::RootDir => {}
                Component::ParentDir => {
                    if !normalized.pop() {
                        return None;
                    }
                }
                Component::Prefix(_) => return None,
            }
        }
        Some(normalized)
    }

    // -- html ------------------------------------------------------------------

    fn html_block(&mut self, html: &str, out: &mut String) {
        let visible = strip_html_comments(html);
        let visible = visible.trim();
        if visible.is_empty() {
            return;
        }
        self.warn_html(visible);
        begin_block(out);
        push_escaped(out, visible);
        out.push_str("\n\n");
    }

    fn inline_html(&mut self, html: &str, out: &mut String) {
        let visible = strip_html_comments(html);
        let visible = visible.trim();
        if visible.is_empty() {
            return;
        }
        let tag: String = visible
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        if matches!(tag.as_str(), "<br>" | "<br/>") {
            out.push_str("\\\n");
            return;
        }
        self.warn_html(visible);
        push_escaped(out, visible);
    }

    fn warn_html(&mut self, html: &str) {
        let snippet: String = html.chars().take(WARNING_DETAIL_MAX_CHARS).collect();
        self.warn(
            PdfWarningCode::HtmlNotSupported,
            "raw HTML has no PDF equivalent and is shown as literal text",
            &snippet,
        );
    }

    fn warn(&mut self, code: PdfWarningCode, message: &str, detail: &str) {
        self.warnings
            .push(PdfWarning::new(code, message).with_detail(detail));
    }
}

// -- helpers ---------------------------------------------------------------------

fn is_inline_tag(tag: &Tag<'_>) -> bool {
    matches!(
        tag,
        Tag::Emphasis | Tag::Strong | Tag::Strikethrough | Tag::Link { .. } | Tag::Image { .. }
    )
}

fn is_inline_event(event: &Event<'_>) -> bool {
    match event {
        Event::Text(_)
        | Event::Code(_)
        | Event::SoftBreak
        | Event::HardBreak
        | Event::InlineHtml(_) => true,
        Event::Start(tag) => is_inline_tag(tag),
        _ => false,
    }
}

/// Display width in character cells: East Asian text takes two cells.
fn display_width(text: &str) -> usize {
    text.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum()
}

fn column_role(header: Option<&TableCell>) -> ColumnRole {
    let Some(text) = header.and_then(|c| c.plain.as_deref()) else {
        return ColumnRole::Other;
    };
    let text = text.trim().to_lowercase();
    if ID_HEADERS.contains(&text.as_str()) {
        ColumnRole::Id
    } else if TITLE_HEADERS.contains(&text.as_str()) {
        ColumnRole::Title
    } else {
        ColumnRole::Other
    }
}

/// Rough width of a table in character cells: per column the widest cell
/// (ID and title cells as they would be after cutting, everything capped so a
/// long free-text column wraps rather than widening the table) plus padding.
fn estimated_width(
    header: &[TableCell],
    rows: &[Vec<TableCell>],
    roles: &[ColumnRole],
    columns: usize,
) -> usize {
    (0..columns)
        .map(|i| {
            let widest = std::iter::once(header)
                .chain(rows.iter().map(Vec::as_slice))
                .filter_map(|row| row.get(i))
                .map(|cell| cell.width)
                .max()
                .unwrap_or(0);
            let limit = match roles[i] {
                ColumnRole::Id => WIDE_TABLE_ID_MAX_CHARS,
                ColumnRole::Title => WIDE_TABLE_TITLE_MAX_CHARS,
                ColumnRole::Other => usize::MAX,
            };
            widest.min(limit).min(COLUMN_WIDTH_ESTIMATE_CAP) + COLUMN_PADDING_CHARS
        })
        .sum()
}

/// Cuts a plain ID / title cell to its limit and ends it with `…`. Cells with
/// markup are left alone: cutting inside markup could break it.
fn shorten_cell(cell: &mut TableCell, role: ColumnRole) {
    let limit = match role {
        ColumnRole::Id => WIDE_TABLE_ID_MAX_CHARS,
        ColumnRole::Title => WIDE_TABLE_TITLE_MAX_CHARS,
        ColumnRole::Other => return,
    };
    let Some(text) = cell.plain.as_deref() else {
        return;
    };
    if display_width(text) <= limit {
        return;
    }
    // Leave one cell for the ellipsis itself.
    let mut kept = String::new();
    let mut width = 0;
    for c in text.chars() {
        let w = display_width(c.encode_utf8(&mut [0; 4]));
        if width + w > limit - 1 {
            break;
        }
        kept.push(c);
        width += w;
    }
    let kept = kept.trim_end();
    let mut markup = String::new();
    push_escaped(&mut markup, kept);
    markup.push(ELLIPSIS);
    cell.width = display_width(kept) + 1;
    cell.markup = markup;
    cell.plain = None;
}

/// Starts a new top-level construct on its own line.
fn begin_block(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

fn image_call(path: &str, alt: &str) -> String {
    if alt.is_empty() {
        format!("image({})", typst_string(path))
    } else {
        format!("image({}, alt: {})", typst_string(path), typst_string(alt))
    }
}

fn is_external_link(dest: &str) -> bool {
    let lower = dest.to_ascii_lowercase();
    ["http://", "https://", "mailto:", "tel:"]
        .iter()
        .any(|p| lower.starts_with(p))
}

/// `scheme://...`, scheme-relative `//host/...` and `data:` references.
fn is_remote_image(dest: &str) -> bool {
    if dest.starts_with("//") || dest.to_ascii_lowercase().starts_with("data:") {
        return true;
    }
    dest.find("://").is_some_and(|i| {
        i > 0
            && dest[..i]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    })
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

/// Removes every `<!-- ... -->` span (an unterminated one swallows the rest).
fn strip_html_comments(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{3000}'..='\u{303F}'   // CJK symbols and punctuation
        | '\u{3040}'..='\u{30FF}' // Hiragana, Katakana
        | '\u{3400}'..='\u{4DBF}' // CJK extension A
        | '\u{4E00}'..='\u{9FFF}' // CJK unified ideographs
        | '\u{AC00}'..='\u{D7AF}' // Hangul syllables
        | '\u{FF00}'..='\u{FFEF}' // Fullwidth forms
    )
}

/// Quotes `s` as a Typst string literal. Literal newlines are kept (Typst
/// allows them), so line numbers in compiler errors stay meaningful.
fn typst_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\r' => {}
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Whether the text being appended starts a Typst line/content block, where
/// `=`, `-`, `+`, `/` and `1.` would be read as heading / list / term markup.
fn at_block_start(line_so_far: &str) -> bool {
    let t = line_so_far.trim_start();
    // An unescaped trailing `[` is the opener of a content block we emitted.
    t.is_empty() || (t.ends_with('[') && !t.ends_with("\\["))
}

/// Appends literal text, escaping everything Typst markup would interpret.
fn push_escaped(out: &mut String, text: &str) {
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let line_start = out.rfind('\n').map_or(0, |i| i + 1);
        let line = &out[line_start..];
        let escape = match c {
            '\\' | '#' | '$' | '*' | '_' | '`' | '<' | '@' | '~' | '[' | ']' => true,
            '=' | '+' => at_block_start(line),
            '-' => at_block_start(line) || matches!(chars.peek(), Some('-' | '?')),
            '/' => at_block_start(line) || chars.peek() == Some(&'/'),
            '.' => {
                let before_digits = line.trim_end_matches(|d: char| d.is_ascii_digit());
                before_digits.len() < line.len() && at_block_start(before_digits)
            }
            _ => false,
        };
        match c {
            '\r' => {}
            _ if escape => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::docs::pdf::types::PdfWarningCode;
    use std::fs;
    use tempfile::TempDir;

    fn convert(md: &str) -> ConversionResult {
        let dir = TempDir::new().unwrap();
        markdown_to_typst(md, &ConvertOptions::new(dir.path()))
    }

    fn typst(md: &str) -> String {
        convert(md).typst
    }

    fn typst_in_section(md: &str, section_title: Option<&str>) -> String {
        let dir = TempDir::new().unwrap();
        markdown_to_typst(
            md,
            &ConvertOptions {
                section_title,
                ..ConvertOptions::new(dir.path())
            },
        )
        .typst
    }

    // -- bundle support: headings under a section heading ------------------

    #[test]
    fn without_a_section_title_levels_are_kept() {
        assert_eq!(
            typst_in_section("# a\n\n## b\n", None),
            typst("# a\n\n## b\n")
        );
    }

    #[test]
    fn a_matching_leading_title_is_dropped_and_children_keep_their_levels() {
        let md = "---\nid: x\n---\n\n# My Title\n\ntext\n\n## Part\n\n### Sub\n";
        assert_eq!(
            typst_in_section(md, Some("My Title")),
            "text\n\n== Part\n\n=== Sub\n\n"
        );
    }

    #[test]
    fn a_later_h1_is_raised_below_the_section_level_not_dropped() {
        let md = "# My Title\n\ntext\n\n# My Title\n\n# Other\n";
        assert_eq!(
            typst_in_section(md, Some("My Title")),
            "text\n\n== My Title\n\n== Other\n\n"
        );
    }

    #[test]
    fn without_a_matching_leading_title_the_shallowest_heading_lands_on_level_two() {
        let md = "# Other\n\n## b\n\n###### f\n";
        assert_eq!(
            typst_in_section(md, Some("My Title")),
            "== Other\n\n=== b\n\n======= f\n\n"
        );
        // Deeper than level 1, or not first: not the title.
        assert_eq!(
            typst_in_section("## My Title\n", Some("My Title")),
            "== My Title\n\n"
        );
        assert_eq!(
            typst_in_section("text\n\n# My Title\n", Some("My Title")),
            "text\n\n== My Title\n\n"
        );
    }

    #[test]
    fn a_document_starting_at_level_two_keeps_its_levels_under_the_section() {
        // No skipped level in the unified TOC (was `2.0.1 Part`).
        assert_eq!(
            typst_in_section("## Part\n\ntext\n\n### Sub\n", Some("My Title")),
            "== Part\n\ntext\n\n=== Sub\n\n"
        );
        // Moved as a block, relative to the shallowest level.
        assert_eq!(
            typst_in_section("### a\n\n#### b\n", Some("My Title")),
            "== a\n\n=== b\n\n"
        );
        // A `#` inside a code block is not a heading.
        let out = typst_in_section("```\n# x\n```\n\n## Part\n", Some("My Title"));
        assert!(
            out.ends_with("== Part\n\n") && !out.contains("=== Part"),
            "{out}"
        );
    }

    #[test]
    fn title_match_ignores_surrounding_whitespace_and_closing_hashes() {
        assert_eq!(
            typst_in_section("\n# My Title #\n\nx\n", Some(" My Title ")),
            "x\n\n"
        );
    }

    #[test]
    fn the_attribute_block_under_a_dropped_title_goes_too() {
        let md = "# T\n- priority: p1\n\nbody\n";
        assert_eq!(typst_in_section(md, Some("T")), "body\n\n");
    }

    // -- headings / paragraphs ------------------------------------------

    #[test]
    fn headings_h1_to_h6_map_to_equals_signs() {
        let out = typst("# a\n\n## b\n\n### c\n\n#### d\n\n##### e\n\n###### f\n");
        assert_eq!(
            out,
            "= a\n\n== b\n\n=== c\n\n==== d\n\n===== e\n\n====== f\n\n"
        );
    }

    #[test]
    fn paragraphs_are_separated_by_blank_lines() {
        assert_eq!(typst("one\n\ntwo\n"), "one\n\ntwo\n\n");
    }

    #[test]
    fn typst_special_characters_in_text_are_escaped() {
        let out = typst("a #b $c *d_ `e` 1 < 2 @g ~h [i] \\\\ j\n");
        assert_eq!(
            out,
            "a \\#b \\$c \\*d\\_ #raw(\"e\"); 1 \\< 2 \\@g \\~h \\[i\\] \\\\ j\n\n"
        );
    }

    #[test]
    fn line_leading_markup_characters_are_escaped() {
        // A paragraph starting with a literal number-dot, or a text that
        // would otherwise become a Typst heading/list/term at line start.
        assert_eq!(typst("2024\\. report\n"), "2024\\. report\n\n");
        assert_eq!(typst("\\= not heading\n"), "\\= not heading\n\n");
        assert_eq!(typst("\\- not list\n"), "\\- not list\n\n");
        assert_eq!(typst("\\+ not list\n"), "\\+ not list\n\n");
        assert_eq!(typst("\\/ not term\n"), "\\/ not term\n\n");
    }

    #[test]
    fn double_slash_and_dashes_do_not_form_comments_or_dashes() {
        assert_eq!(
            typst("see http://x.y a -- b\n"),
            "see http:\\//x.y a \\-- b\n\n"
        );
    }

    // -- inline ------------------------------------------------------------

    #[test]
    fn emphasis_strong_strike_use_function_form() {
        let out = typst("*a* **b** ~~c~~\n");
        assert_eq!(out, "#emph[a]; #strong[b]; #strike[c];\n\n");
    }

    #[test]
    fn emphasis_adjacent_to_cjk_text_is_robust() {
        let out = typst("日本語**強調**です\n");
        assert_eq!(out, "日本語#strong[強調];です\n\n");
    }

    #[test]
    fn inline_code_is_a_raw_string() {
        let out = typst("use `a\"b\\c` here\n");
        assert_eq!(out, "use #raw(\"a\\\"b\\\\c\"); here\n\n");
    }

    #[test]
    fn external_links_become_typst_links() {
        let out = typst("[site](https://example.com/a?b=1) and <https://x.org>\n");
        assert_eq!(
            out,
            "#link(\"https://example.com/a?b=1\")[site]; and #link(\"https://x.org\")[https:\\//x.org];\n\n"
        );
    }

    #[test]
    fn non_external_links_keep_only_their_text() {
        let out = typst("[a](#frag) [b](other.md) [c](mailto:x@y.z)\n");
        assert_eq!(out, "a b #link(\"mailto:x@y.z\")[c];\n\n");
    }

    #[test]
    fn soft_break_becomes_space_between_latin_but_joins_cjk() {
        assert_eq!(typst("a\nb\n"), "a\nb\n\n");
        assert_eq!(typst("日本語\nの改行\n"), "日本語の改行\n\n");
    }

    #[test]
    fn hard_break_becomes_typst_linebreak() {
        assert_eq!(typst("a  \nb\n"), "a\\\nb\n\n");
    }

    // -- block structures --------------------------------------------------

    #[test]
    fn unordered_list_uses_list_function() {
        let out = typst("- a\n- b\n");
        assert_eq!(out, "#list(\n  [a],\n  [b],\n)\n\n");
    }

    #[test]
    fn ordered_list_keeps_start_number() {
        let out = typst("1. a\n2. b\n");
        assert_eq!(out, "#enum(\n  [a],\n  [b],\n)\n\n");
        let out = typst("3. a\n4. b\n");
        assert!(out.starts_with("#enum(\n  start: 3,\n"), "{out}");
    }

    #[test]
    fn nested_lists_nest_function_calls() {
        let out = typst("- a\n  - b\n");
        assert!(out.contains("[a\n#list("), "{out}");
        assert!(out.contains("[b]"), "{out}");
    }

    #[test]
    fn loose_list_sets_tight_false() {
        let out = typst("- a\n\n- b\n");
        assert!(out.starts_with("#list(\n  tight: false,\n"), "{out}");
    }

    #[test]
    fn blockquote_uses_quote_function() {
        assert_eq!(typst("> q\n"), "#quote(block: true)[q]\n\n");
    }

    #[test]
    fn thematic_break_becomes_line() {
        assert_eq!(
            typst("a\n\n***\n\nb\n"),
            "a\n\n#line(length: 100%)\n\nb\n\n"
        );
    }

    #[test]
    fn table_has_header_alignment_and_cells() {
        let out = typst("| A | B | C |\n|:--|:-:|--:|\n| 1 | **2** | |\n");
        assert_eq!(
            out,
            "#table(\n  columns: (auto, auto, auto),\n  align: (left, center, right),\n  table.header([A], [B], [C]),\n  [1], [#strong[2];], [],\n)\n\n"
        );
    }

    #[test]
    fn table_without_alignment_uses_auto() {
        let out = typst("| A |\n|---|\n| 1 |\n");
        assert!(out.contains("columns: (auto,),"), "{out}");
        assert!(out.contains("align: (auto,),"), "{out}");
    }

    /// An 8-column verification-results table with `n` data rows.
    fn verification_table(n: usize, title: &str) -> String {
        let mut md = String::from(
            "| ID | Title | Layer | Result | Date | Verifier | Evidence | Note |\n\
             |----|-------|-------|--------|------|----------|----------|------|\n",
        );
        for i in 0..n {
            md.push_str(&format!(
                "| REQ-PDF-{i:03} | {title} | requirements | pass | 2026-10-27T11:35:54Z | alice | [run](https://example.com/runs/{i}) | checked by hand |\n"
            ));
        }
        md
    }

    #[test]
    fn narrow_table_stays_on_the_portrait_page() {
        let out = typst("| A | B |\n|---|---|\n| 1 | 2 |\n");
        assert!(!out.contains("#page("), "{out}");
    }

    #[test]
    fn wide_table_moves_to_a_landscape_page() {
        let out = typst(&verification_table(3, "short"));
        assert!(out.starts_with("#page(flipped: true)[\n"), "{out}");
        assert!(out.trim_end().ends_with(")\n]"), "{out}");
        assert_eq!(out.matches("#page(").count(), 1, "{out}");
        assert!(out.contains("#set text(size: 8pt)"), "{out}");
    }

    #[test]
    fn wide_table_fixes_id_column_and_shares_title_width() {
        let out = typst(&verification_table(1, "short"));
        assert!(
            out.contains("columns: (3.8cm, 1fr, auto, auto, auto, auto, auto, auto),"),
            "{out}"
        );
    }

    #[test]
    fn wide_table_cuts_long_ids_and_titles_with_an_ellipsis() {
        let long_title = "x".repeat(200);
        let md = verification_table(1, &long_title)
            .replace("REQ-PDF-000", "REQ-VERY-LONG-IDENTIFIER-000");
        let out = typst(&md);
        assert!(!out.contains(&long_title), "{out}");
        assert!(
            out.contains(&format!("[{}\u{2026}]", "x".repeat(59))),
            "{out}"
        );
        assert!(
            out.contains("[REQ-VERY-LONG-IDE\u{2026}]")
                || out.contains("[REQ-VERY-LONG-ID\u{2026}]"),
            "{out}"
        );
        // Columns that are not ID / title keep their full text.
        assert!(out.contains("[checked by hand]"), "{out}");
    }

    #[test]
    fn wide_table_cuts_japanese_titles_by_display_width() {
        let title = "あ".repeat(100);
        let out = typst(&verification_table(1, &title));
        // 59 cells of room: 29 full-width characters, then the ellipsis.
        assert!(
            out.contains(&format!("[{}\u{2026}]", "あ".repeat(29))),
            "{out}"
        );
    }

    #[test]
    fn wide_table_leaves_cells_with_markup_untouched() {
        let long = "y".repeat(200);
        let md = verification_table(1, &format!("**{long}**"));
        let out = typst(&md);
        assert!(out.contains(&format!("#strong[{long}];")), "{out}");
    }

    #[test]
    fn short_ids_and_titles_are_not_cut() {
        let out = typst(&verification_table(1, "A fine title"));
        assert!(out.contains("[REQ-PDF-000]"), "{out}");
        assert!(out.contains("[A fine title]"), "{out}");
        assert!(!out.contains('\u{2026}'), "{out}");
    }

    #[test]
    fn fifty_plus_rows_render_in_one_landscape_table() {
        let out = typst(&verification_table(60, "title"));
        assert_eq!(out.matches("#page(").count(), 1, "{out}");
        assert_eq!(out.matches("[REQ-PDF-").count(), 60, "{out}");
        assert!(out.contains("[REQ-PDF-059]"), "{out}");
    }

    #[test]
    fn many_narrow_columns_stay_portrait() {
        let out = typst(
            "| Layer | Status | Total | Pass | Fail | Blocked | Waived | Pending | Coverage |\n\
             |---|---|--:|--:|--:|--:|--:|--:|--:|\n\
             | requirements | approved | 10 | 8 | 1 | 0 | 1 | 0 | 88.9% |\n",
        );
        assert!(!out.contains("#page("), "{out}");
    }

    #[test]
    fn wide_table_inside_a_list_is_not_moved_to_its_own_page() {
        let table = verification_table(1, "short");
        let md = format!(
            "- item\n\n{}",
            table
                .lines()
                .map(|l| format!("  {l}\n"))
                .collect::<String>()
        );
        let out = typst(&md);
        assert!(out.contains("#table("), "{out}");
        assert!(!out.contains("#page("), "{out}");
    }

    #[test]
    fn code_block_is_a_raw_block_with_language() {
        let out = typst("```rust\nfn main() {\n    println!(\"hi\\n\");\n}\n```\n");
        assert_eq!(
            out,
            "#raw(block: true, lang: \"rust\", \"fn main() {\n    println!(\\\"hi\\\\n\\\");\n}\")\n\n"
        );
    }

    #[test]
    fn code_block_without_language_has_no_lang_argument() {
        let out = typst("```\nplain\n```\n");
        assert_eq!(out, "#raw(block: true, \"plain\")\n\n");
    }

    #[test]
    fn mermaid_block_stays_a_code_block() {
        let out = typst("```mermaid\ngraph TD; A-->B;\n```\n");
        assert_eq!(
            out,
            "#raw(block: true, lang: \"mermaid\", \"graph TD; A-->B;\")\n\n"
        );
    }

    // -- metadata ----------------------------------------------------------

    #[test]
    fn leading_frontmatter_is_stripped() {
        let out = typst("---\nid: doc-1\ntitle: T\n---\n\n# Head\n");
        assert_eq!(out, "= Head\n\n");
    }

    #[test]
    fn unterminated_leading_rule_is_not_frontmatter() {
        let out = typst("---\n\ntext\n");
        assert!(out.contains("#line(length: 100%)"), "{out}");
        assert!(out.contains("text"), "{out}");
    }

    #[test]
    fn attribute_block_after_heading_is_stripped() {
        let md = "## REQ-001 Title\n\n- priority: high\n- refines: REQ-000\n- milestone: m1\n\nBody text.\n";
        let out = typst(md);
        assert_eq!(out, "== REQ-001 Title\n\nBody text.\n\n");
    }

    #[test]
    fn unknown_bullets_after_heading_are_kept() {
        let md = "## H\n\n- priority: high\n- plain item\n";
        let out = typst(md);
        assert!(!out.contains("priority"), "{out}");
        assert!(out.contains("plain item"), "{out}");
    }

    #[test]
    fn acceptance_criteria_content_is_kept() {
        let md = "## H\n\n- priority: high\n- acceptance_criteria:\n  - AC1: works\n";
        let out = typst(md);
        assert!(!out.contains("priority"), "{out}");
        assert!(out.contains("acceptance\\_criteria"), "{out}");
        assert!(out.contains("AC1: works"), "{out}");
    }

    #[test]
    fn attribute_like_bullets_elsewhere_are_kept() {
        let md = "## H\n\nIntro.\n\n- priority: keep me\n";
        assert!(typst(md).contains("priority: keep me"));
    }

    #[test]
    fn attribute_like_lines_inside_code_fences_are_kept() {
        let md = "## H\n\n```\n## X\n- priority: p\n```\n";
        let out = typst(md);
        assert!(out.contains("- priority: p"), "{out}");
    }

    // -- requirement meta blocks (REQ-PDF-030) -----------------------------

    fn index(entries: &[(&str, ReqMeta)]) -> ReqMetaIndex {
        entries
            .iter()
            .map(|(id, meta)| (id.to_string(), meta.clone()))
            .collect()
    }

    fn p0() -> ReqMeta {
        ReqMeta {
            priority: Some("P0".into()),
            ..ReqMeta::default()
        }
    }

    fn enriched(md: &str, index: &ReqMetaIndex) -> String {
        let dir = TempDir::new().unwrap();
        markdown_to_typst(
            md,
            &ConvertOptions {
                req_meta: Some(index),
                ..ConvertOptions::new(dir.path())
            },
        )
        .typst
    }

    #[test]
    fn enriched_heading_gets_a_meta_block_in_place_of_its_attribute_lines() {
        let md = "## REQ-001 Title\n\n- priority: P0\n- refines: REQ-000\n\nBody text.\n";
        let out = enriched(md, &index(&[("REQ-001", p0())]));
        assert_eq!(
            out,
            "== REQ-001 Title\n\n#req-meta(priority: \"P0\")\n\nBody text.\n\n"
        );
    }

    #[test]
    fn without_an_index_the_attribute_lines_are_just_dropped() {
        let md = "## REQ-001 Title\n\n- priority: P0\n\nBody text.\n";
        assert_eq!(typst(md), "== REQ-001 Title\n\nBody text.\n\n");
    }

    #[test]
    fn heading_without_an_index_entry_is_left_plain() {
        let md = "## REQ-002 Other\n\n- priority: P1\n\nBody.\n";
        let out = enriched(md, &index(&[("REQ-001", p0())]));
        assert_eq!(out, "== REQ-002 Other\n\nBody.\n\n");
    }

    #[test]
    fn id_may_be_followed_by_a_colon_or_stand_alone_in_the_heading() {
        let idx = index(&[("REQ-001", p0())]);
        for heading in ["## REQ-001: Title", "## REQ-001. Title", "## REQ-001"] {
            let out = enriched(&format!("{heading}\n\ntext\n"), &idx);
            assert!(
                out.contains("#req-meta(priority: \"P0\")"),
                "{heading}: {out}"
            );
        }
    }

    #[test]
    fn meta_with_nothing_to_show_adds_no_block() {
        let md = "## REQ-001 T\n\n- priority: P0\n\ntext\n";
        let out = enriched(md, &index(&[("REQ-001", ReqMeta::default())]));
        assert_eq!(out, "== REQ-001 T\n\ntext\n\n");
    }

    #[test]
    fn attribute_form_acceptance_criteria_move_into_the_meta_block() {
        let md = "## REQ-001 T\n\n- priority: P0\n- acceptance_criteria:\n  - AC1(text): first\n  - [x] AC2: second\n\nBody.\n";
        let out = enriched(md, &index(&[("REQ-001", p0())]));
        assert!(
            out.contains(
                "acceptance: ((label: \"AC1\", text: \"first\", checked: false), \
                 (label: \"AC2\", text: \"second\", checked: true), )"
            ),
            "{out}"
        );
        assert!(!out.contains("acceptance\\_criteria"), "{out}");
        assert!(
            !out.contains("first\n"),
            "criteria text must not repeat: {out}"
        );
        assert!(out.ends_with("Body.\n\n"), "{out}");
    }

    #[test]
    fn verified_criteria_are_checked() {
        let mut meta = p0();
        meta.verified_acs.insert("AC1".into());
        let md = "## REQ-001 T\n\n- acceptance_criteria:\n  - AC1: a\n  - AC2: b\n";
        let out = enriched(md, &index(&[("REQ-001", meta)]));
        assert!(
            out.contains("(label: \"AC1\", text: \"a\", checked: true)"),
            "{out}"
        );
        assert!(
            out.contains("(label: \"AC2\", text: \"b\", checked: false)"),
            "{out}"
        );
    }

    #[test]
    fn acceptance_paragraph_form_moves_into_the_meta_block() {
        let md =
            "## REQ-001 T\n\nIntro.\n\n受入基準:\n- AC1: one\n- AC2: two\n  continued\n\nAfter.\n";
        let out = enriched(md, &index(&[("REQ-001", p0())]));
        assert!(
            out.contains(
                "acceptance: ((label: \"AC1\", text: \"one\", checked: false), \
                 (label: \"AC2\", text: \"two continued\", checked: false), )"
            ),
            "{out}"
        );
        assert!(out.contains("Intro."), "{out}");
        assert!(out.contains("After."), "{out}");
        assert!(!out.contains("受入基準"), "{out}");
        // The block sits directly under the heading, before the intro.
        assert!(
            out.find("#req-meta").unwrap() < out.find("Intro.").unwrap(),
            "{out}"
        );
    }

    #[test]
    fn acceptance_paragraph_of_an_item_without_meta_stays_in_the_body() {
        let md = "## REQ-009 T\n\n受入基準:\n- AC1: one\n";
        let out = enriched(md, &index(&[("REQ-001", p0())]));
        assert!(out.contains("受入基準"), "{out}");
        assert!(out.contains("AC1: one"), "{out}");
    }

    #[test]
    fn acceptance_lookalike_inside_a_code_fence_is_kept() {
        let md = "## REQ-001 T\n\n```\n受入基準:\n- AC1: x\n```\n";
        let out = enriched(md, &index(&[("REQ-001", p0())]));
        assert!(out.contains("受入基準"), "{out}");
        assert!(!out.contains("acceptance:"), "{out}");
    }

    #[test]
    fn each_section_gets_its_own_block_and_criteria() {
        let md = "## REQ-001 A\n\n- acceptance_criteria:\n  - AC1: a1\n\n## REQ-002 B\n\n- acceptance_criteria:\n  - AC1: b1\n\ntail\n";
        let out = enriched(md, &index(&[("REQ-001", p0()), ("REQ-002", p0())]));
        let first = out.find("text: \"a1\"").unwrap();
        let second = out.find("text: \"b1\"").unwrap();
        let heading_b = out.find("REQ-002 B").unwrap();
        assert!(first < heading_b && heading_b < second, "{out}");
        assert_eq!(out.matches("#req-meta(").count(), 2, "{out}");
    }

    #[test]
    fn a_title_in_the_meta_cannot_close_the_passthrough_fence() {
        let meta = ReqMeta {
            tasks: vec![crate::storage::docs::pdf::req_meta::TaskRef {
                id: "t1".into(),
                title: Some("evil ``` title".into()),
                status: None,
            }],
            ..ReqMeta::default()
        };
        let out = enriched("## REQ-001 T\n\ntext\n", &index(&[("REQ-001", meta)]));
        assert!(out.contains("evil ``` title"), "{out}");
        assert!(out.contains("text"), "{out}");
    }

    #[test]
    fn a_dropped_section_title_keeps_its_meta_block() {
        let md = "# REQ-001 T\n\n- priority: P0\n\nBody.\n";
        let dir = TempDir::new().unwrap();
        let idx = index(&[("REQ-001", p0())]);
        let out = markdown_to_typst(
            md,
            &ConvertOptions {
                section_title: Some("REQ-001 T"),
                req_meta: Some(&idx),
                ..ConvertOptions::new(dir.path())
            },
        )
        .typst;
        assert_eq!(out, "#req-meta(priority: \"P0\")\n\nBody.\n\n");
    }

    // -- images ------------------------------------------------------------

    fn convert_in(root: &Path, md: &str) -> ConversionResult {
        markdown_to_typst(md, &ConvertOptions::new(root))
    }

    fn project_with_image(rel: &str) -> TempDir {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"x").unwrap();
        dir
    }

    #[test]
    fn existing_relative_image_resolves_from_project_root() {
        let dir = project_with_image("docs/img/a.png");
        let r = convert_in(dir.path(), "![diagram](docs/img/a.png)\n");
        assert_eq!(
            r.typst,
            "#figure(image(\"/docs/img/a.png\", alt: \"diagram\"), caption: [diagram])\n\n"
        );
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn image_without_alt_has_no_caption() {
        let dir = project_with_image("a.svg");
        let r = convert_in(dir.path(), "![](./a.svg)\n");
        assert_eq!(r.typst, "#figure(image(\"/a.svg\"))\n\n");
    }

    #[test]
    fn inline_image_in_a_paragraph_is_an_inline_image() {
        let dir = project_with_image("a.jpeg");
        let r = convert_in(dir.path(), "see ![x](a.jpeg) here\n");
        assert_eq!(
            r.typst,
            "see #box(image(\"/a.jpeg\", alt: \"x\")); here\n\n"
        );
    }

    #[test]
    fn dot_dot_segments_inside_root_are_normalized() {
        let dir = project_with_image("img/a.png");
        let r = convert_in(dir.path(), "![](docs/../img/a.png)\n");
        assert!(r.typst.contains("image(\"/img/a.png\")"), "{}", r.typst);
    }

    #[test]
    fn percent_encoded_image_path_is_decoded() {
        let dir = project_with_image("my img.png");
        let r = convert_in(dir.path(), "![](my%20img.png)\n");
        assert!(r.typst.contains("image(\"/my img.png\")"), "{}", r.typst);
    }

    #[test]
    fn project_absolute_image_path_is_accepted() {
        let dir = project_with_image("img/a.png");
        let abs = dir.path().join("img/a.png");
        let r = convert_in(dir.path(), &format!("![](<{}>)\n", abs.display()));
        assert!(r.typst.contains("image(\"/img/a.png\")"), "{}", r.typst);
        let r = convert_in(dir.path(), "![](/img/a.png)\n");
        assert!(r.typst.contains("image(\"/img/a.png\")"), "{}", r.typst);
    }

    #[test]
    fn missing_image_becomes_placeholder_with_warning() {
        let dir = TempDir::new().unwrap();
        let r = convert_in(dir.path(), "![alt](img/none.png)\n");
        assert_eq!(r.typst, "img/none.png\n\n");
        assert_eq!(r.warnings.len(), 1);
        assert_eq!(r.warnings[0].code, PdfWarningCode::ImageNotFound);
        assert_eq!(r.warnings[0].detail.as_deref(), Some("img/none.png"));
    }

    #[test]
    fn missing_inline_image_placeholder_is_escaped_text() {
        let dir = TempDir::new().unwrap();
        let r = convert_in(dir.path(), "a ![](img/no_one.png) b\n");
        assert_eq!(r.typst, "a img/no\\_one.png b\n\n");
    }

    #[test]
    fn directory_named_like_an_image_is_not_found() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("d.png")).unwrap();
        let r = convert_in(dir.path(), "![](d.png)\n");
        assert_eq!(r.warnings[0].code, PdfWarningCode::ImageNotFound);
    }

    #[test]
    fn image_escaping_the_project_root_is_rejected() {
        let dir = TempDir::new().unwrap();
        let outer = dir.path().join("outer.png");
        fs::write(&outer, b"x").unwrap();
        let root = dir.path().join("proj");
        fs::create_dir_all(&root).unwrap();
        let r = convert_in(&root, "![](../outer.png)\n");
        assert_eq!(r.warnings.len(), 1);
        assert_eq!(r.warnings[0].code, PdfWarningCode::ImageOutsideProject);
        assert!(!r.typst.contains("image("), "{}", r.typst);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_pointing_outside_the_root_is_rejected() {
        let dir = TempDir::new().unwrap();
        let outer = dir.path().join("outer.png");
        fs::write(&outer, b"x").unwrap();
        let root = dir.path().join("proj");
        fs::create_dir_all(&root).unwrap();
        std::os::unix::fs::symlink(&outer, root.join("link.png")).unwrap();
        let r = convert_in(&root, "![](link.png)\n");
        assert_eq!(r.warnings[0].code, PdfWarningCode::ImageOutsideProject);
    }

    #[test]
    fn unsupported_extension_and_remote_urls_warn() {
        let dir = project_with_image("a.bmp");
        let r = convert_in(dir.path(), "![](a.bmp)\n\n![](https://x.org/a.png)\n");
        assert_eq!(r.warnings.len(), 2);
        assert!(r
            .warnings
            .iter()
            .all(|w| w.code == PdfWarningCode::ImageUnsupported));
        assert!(!r.typst.contains("image("), "{}", r.typst);
    }

    #[test]
    fn image_extension_match_is_case_insensitive() {
        let dir = project_with_image("A.PNG");
        let r = convert_in(dir.path(), "![](A.PNG)\n");
        assert!(r.typst.contains("image(\"/A.PNG\")"), "{}", r.typst);
    }

    // -- typst passthrough ---------------------------------------------------

    #[test]
    fn typst_block_is_passed_through_verbatim() {
        let md = "before\n\n```typst\n$ f_c = 1 / (2 pi R C) $\n\n#table(columns: 2, [a], [b])\n```\n\nafter\n";
        let out = typst(md);
        assert_eq!(
            out,
            "before\n\n$ f_c = 1 / (2 pi R C) $\n\n#table(columns: 2, [a], [b])\n\nafter\n\n"
        );
    }

    #[test]
    fn typst_block_does_not_affect_surrounding_conversion() {
        let md = "*a*\n\n```typst\n#pagebreak()\n```\n\n*b*\n";
        let out = typst(md);
        assert_eq!(out, "#emph[a];\n\n#pagebreak()\n\n#emph[b];\n\n");
    }

    #[test]
    fn typst_block_info_string_is_case_insensitive_and_ignores_attrs() {
        let out = typst("```Typst extra\n#x\n```\n");
        assert_eq!(out, "#x\n\n");
    }

    #[test]
    fn typst_block_inside_a_list_item_is_passed_through() {
        let out = typst("- a\n\n  ```typst\n  $ x^2 $\n  ```\n");
        assert!(out.contains("$ x^2 $"), "{out}");
        assert!(!out.contains("#raw"), "{out}");
    }

    // -- html ------------------------------------------------------------------

    #[test]
    fn html_comments_are_dropped_silently() {
        let r = convert("a <!-- hidden --> b\n\n<!-- block -->\n\nc\n");
        assert!(!r.typst.contains("hidden"), "{}", r.typst);
        assert!(!r.typst.contains("block"), "{}", r.typst);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn br_tag_becomes_linebreak() {
        let r = convert("a<br>b<br/>c\n");
        assert_eq!(r.typst, "a\\\nb\\\nc\n\n");
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn other_html_is_kept_as_text_with_warning() {
        let r = convert("<details>\nx\n</details>\n\nand <span>y</span>\n");
        assert!(r.typst.contains("<details>"), "{}", r.typst);
        assert!(r
            .warnings
            .iter()
            .all(|w| w.code == PdfWarningCode::HtmlNotSupported));
        assert!(!r.warnings.is_empty());
    }

    #[test]
    fn literal_percent_signs_in_image_paths_do_not_break_decoding() {
        let dir = project_with_image("100%.png");
        let r = convert_in(dir.path(), "![](100%.png)\n");
        assert!(r.typst.contains("image(\"/100%.png\")"), "{}", r.typst);
        let r = convert_in(dir.path(), "![](50%zz.png)\n");
        assert_eq!(r.warnings[0].code, PdfWarningCode::ImageNotFound);
    }

    #[test]
    fn crlf_input_converts_like_lf() {
        let r = convert("---\r\nid: x\r\n---\r\n# T\r\n\r\n```typst\r\n$ x $\r\n```\r\n");
        assert_eq!(r.typst, "= T\n\n$ x $\n\n");
    }

    #[test]
    fn empty_typst_block_emits_nothing() {
        assert_eq!(typst("a\n\n```typst\n```\n\nb\n"), "a\n\nb\n\n");
    }

    #[test]
    fn empty_input_gives_empty_output() {
        let r = convert("");
        assert_eq!(r.typst, "");
        assert!(r.warnings.is_empty());
    }
}
