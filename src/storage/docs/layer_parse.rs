//! Layer-document body parser (wiki/220-vmodel-integration-design.md §2.2,
//! M1 t360.5): a **pure function** that turns a layer document's Markdown
//! body into item definitions (per-heading ID, title, attributes, body text,
//! `body_hash`) plus warnings. No file I/O.
//!
//! Wiring this into `doc_save` / `doc_update_section` / `sync_layer_items`
//! and writing results into `SubItem` (matrix rebuild, legacy-field
//! preservation, the body-owned write guard) is t360.6's scope — this module
//! only decides *what* a body means, in a shape (`ParsedItem` with line
//! range / heading level / attrs / `body_hash` / warnings) t360.6 can
//! consume directly.

use std::collections::{HashMap, HashSet};

use super::layer::LayerRegistry;
use super::split::collect_all_heading_bounds;

/// One item's known attribute-line values (§2.2's vocabulary: `refines`,
/// `verifies`, `layer`, `priority`, `method`, `test`). Unknown keys, and any
/// bullet-list block after the first one immediately following the heading,
/// are left untouched in the item's `statement` (§2.2: "未知キーの行と2つ目
/// 以降の箇条書きブロックは本文扱い").
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemAttrs {
    /// `- layer: <id>` — per-item layer override (§2.3).
    pub layer: Option<String>,
    /// `- refines: <id>[,<id>...]`, in source order.
    pub refines: Vec<String>,
    /// `- verifies: <id>[,<id>...]`, in source order.
    pub verifies: Vec<String>,
    /// `- priority: <value>` (expected `P0`-`P3`; stored as authored,
    /// unvalidated — validation is a tool-side concern, not the parser's).
    pub priority: Option<String>,
    /// `- method: <value>` (expected `manual`|`auto`|`visual`|`review`;
    /// stored as authored, unvalidated).
    pub method: Option<String>,
    /// Raw `path::name` values from `- test: <value>` lines, in source
    /// order. Multiple lines are allowed (§2.2 "複数行可"). Mapping these
    /// into `SubItem.test_refs: Vec<CodeRef>` is t360.6's concern.
    pub test_refs: Vec<String>,
}

/// One parsed layer item (a single §2.2 item heading).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedItem {
    /// The author-written ID, unchanged (D1/D4 — wiki/220 §1: "作者が見出し
    /// に書いた ID をそのまま stable_id"とする).
    pub id: String,
    /// Heading text after the ID and its `:`/`.` separator (if any), trimmed.
    pub title: String,
    /// ATX heading level (1-6) of this item's own heading.
    pub heading_level: u8,
    /// 1-based line number of this item's heading line.
    pub start_line: usize,
    /// 1-based line number of the last line covered by this item's body
    /// (inclusive) — the line just before the terminating heading, or the
    /// document's last line.
    pub end_line: usize,
    pub attrs: ItemAttrs,
    /// Item body ("statement"): attribute lines removed, leading/trailing
    /// whitespace trimmed, internal formatting kept as authored.
    pub statement: String,
    /// `attrs.layer.or(doc_layer)` (§2.3) — the effective layer this item
    /// lives on, computed here so t360.6 does not have to repeat the rule.
    pub effective_layer: Option<String>,
    /// FNV-1a hex of the normalized `title` + `statement` + `attrs` (§2.3).
    pub body_hash: String,
}

/// The kind of a [`ParseWarning`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseWarningKind {
    /// A heading looks ID-like (`[A-Z]{2,}-\d+...`) but its leading uppercase
    /// run is not an allowed prefix (§2.2, e.g. `HTTP-2`, `UTF-8`,
    /// `ISO-26262`). The heading is treated as an ordinary (non-item)
    /// heading.
    IdLikeHeadingIgnored,
    /// The same ID appears twice in one document; the 2nd (and later)
    /// occurrence is ignored (§2.2).
    DuplicateId,
}

/// A non-fatal issue found while parsing (§2.2/§2.3). Parsing never fails
/// outright — every warning corresponds to a heading that is simply not
/// turned into an item (or, for a duplicate, not turned into a *second*
/// item).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseWarning {
    pub kind: ParseWarningKind,
    /// 1-based line number of the offending heading.
    pub line: usize,
    /// The heading's full trimmed text, for the caller to build a message.
    pub heading: String,
    /// For [`ParseWarningKind::DuplicateId`], the id that was duplicated.
    /// `None` for [`ParseWarningKind::IdLikeHeadingIgnored`].
    pub id: Option<String>,
}

impl std::fmt::Display for ParseWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            ParseWarningKind::IdLikeHeadingIgnored => write!(
                f,
                "line {}: ID-like heading ignored: \"{}\"",
                self.line, self.heading
            ),
            ParseWarningKind::DuplicateId => write!(
                f,
                "line {}: duplicate id \"{}\" ignored: \"{}\"",
                self.line,
                self.id.as_deref().unwrap_or(""),
                self.heading
            ),
        }
    }
}

/// [`parse_layer_body`]'s result: the parsed items (in document order) plus
/// warnings (in document order).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LayerParseResult {
    pub items: Vec<ParsedItem>,
    pub warnings: Vec<ParseWarning>,
}

/// Parses `body` (a layer document's Markdown body, already stripped of any
/// YAML frontmatter — e.g. via [`super::split::split`]) into item
/// definitions.
///
/// `doc_layer` is the document's own `layer` (frontmatter `layer: <id>`), used
/// only to fill each item's `effective_layer` when it has no `- layer:`
/// override of its own (§2.3: `sub.layer.or(doc.layer)`).
///
/// `prefix_table` is the effective ID-prefix allow-list per layer id (see
/// [`default_prefix_table`], built from [`super::layer::id_prefixes_for`]).
/// Recognition matches a heading's leading ID against the **union** of every
/// layer's prefixes in `prefix_table`, not just `doc_layer`'s own list: a
/// single document may mix items from paired layers via `- layer:`
/// overrides (§2.2's worked example puts a `basic_spec`-prefixed `SPEC-012`
/// and a `system_test`-prefixed `ST-040` item in the same body).
pub fn parse_layer_body(
    body: &str,
    doc_layer: Option<&str>,
    prefix_table: &HashMap<String, Vec<String>>,
) -> LayerParseResult {
    let allowed: HashSet<&str> = prefix_table
        .values()
        .flat_map(|prefixes| prefixes.iter().map(String::as_str))
        .collect();

    let headings = collect_all_heading_bounds(body);
    let id_matches: Vec<Option<IdMatch>> = headings
        .iter()
        .map(|h| match_item_id(&h.text, &allowed))
        .collect();

    let line_starts = compute_line_starts(body);

    let mut items = Vec::new();
    let mut warnings = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();

    for (i, heading) in headings.iter().enumerate() {
        let Some(id_match) = &id_matches[i] else {
            if is_id_like_but_disallowed(&heading.text, &allowed) {
                warnings.push(ParseWarning {
                    kind: ParseWarningKind::IdLikeHeadingIgnored,
                    line: line_number(&line_starts, heading.start),
                    heading: heading.text.clone(),
                    id: None,
                });
            }
            continue;
        };

        let start_line = line_number(&line_starts, heading.start);

        if !seen_ids.insert(id_match.id.clone()) {
            warnings.push(ParseWarning {
                kind: ParseWarningKind::DuplicateId,
                line: start_line,
                heading: heading.text.clone(),
                id: Some(id_match.id.clone()),
            });
            continue;
        }

        // Body ends at the next heading that is itself an item (any level —
        // items never implicitly nest), or a non-item heading whose level is
        // <= this item's own level (§2.2).
        let end_byte = headings[i + 1..]
            .iter()
            .zip(id_matches[i + 1..].iter())
            .find(|(h, m)| m.is_some() || h.level <= heading.level)
            .map(|(h, _)| h.start)
            .unwrap_or(body.len());

        let heading_line_end = find_heading_line_end(body, heading.start).min(end_byte);
        let fragment = &body[heading_line_end..end_byte];
        let (attrs, statement) = parse_item_body(fragment);

        let end_line = if end_byte > 0 {
            line_number(&line_starts, end_byte - 1)
        } else {
            start_line
        };

        let effective_layer = attrs
            .layer
            .clone()
            .or_else(|| doc_layer.map(str::to_string));

        let body_hash = compute_body_hash(&id_match.title, &statement, &attrs);

        items.push(ParsedItem {
            id: id_match.id.clone(),
            title: id_match.title.clone(),
            heading_level: heading.level,
            start_line,
            end_line,
            attrs,
            statement,
            effective_layer,
            body_hash,
        });
    }

    LayerParseResult { items, warnings }
}

/// Convenience: the effective ID-prefix table for every layer in `registry`
/// (built-ins + valid `[[trace.layer]]` declarations, wiki/260 §2.1,
/// M2-01), merged with `config_id_prefixes` (`[trace.id_prefixes]`) via
/// [`LayerRegistry::id_prefixes_for`]. This is what [`parse_layer_body`]
/// expects.
pub fn default_prefix_table(
    registry: &LayerRegistry,
    config_id_prefixes: &HashMap<String, Vec<String>>,
) -> HashMap<String, Vec<String>> {
    registry
        .all()
        .iter()
        .map(|l| {
            (
                l.id.clone(),
                registry.id_prefixes_for(&l.id, config_id_prefixes),
            )
        })
        .collect()
}

// -- ID matching (§2.2) --

struct IdMatch {
    id: String,
    title: String,
}

/// Matches an item ID at the very start of `heading_text`. Grammar (§2.2):
/// `<allowed-prefix>-<digits>[a-z]?` or
/// `<allowed-prefix>-<alnum>-...-<digits>[a-z]?` — i.e. prefix, then one or
/// more `-`-separated alphanumeric segments, where the *last* segment must be
/// digits with an optional single trailing lowercase letter. Returns `None`
/// (no item) when the prefix is not in `allowed`, or the grammar does not
/// match at all (the heading is then either ordinary or, per
/// [`is_id_like_but_disallowed`], warned about as ID-like).
fn match_item_id(heading_text: &str, allowed: &HashSet<&str>) -> Option<IdMatch> {
    let prefix_len = heading_text
        .as_bytes()
        .iter()
        .take_while(|b| b.is_ascii_uppercase())
        .count();
    if prefix_len == 0 {
        return None;
    }
    let prefix = &heading_text[..prefix_len];
    if !allowed.contains(prefix) {
        return None;
    }
    let rest = &heading_text[prefix_len..];
    let after_dash = rest.strip_prefix('-')?;

    let bytes = after_dash.as_bytes();
    let mut pos = 0usize;
    let mut last_seg_start;
    let mut last_seg_end;
    loop {
        let seg_start = pos;
        while pos < bytes.len() && bytes[pos].is_ascii_alphanumeric() {
            pos += 1;
        }
        if pos == seg_start {
            // Empty segment (e.g. a bare trailing '-' with no alnum after
            // it) — not a valid ID.
            return None;
        }
        last_seg_start = seg_start;
        last_seg_end = pos;

        let next_starts_another_segment = pos < bytes.len()
            && bytes[pos] == b'-'
            && pos + 1 < bytes.len()
            && bytes[pos + 1].is_ascii_alphanumeric();
        if next_starts_another_segment {
            pos += 1; // consume '-', loop again for the next segment
            continue;
        }
        break;
    }

    let last_seg = &after_dash[last_seg_start..last_seg_end];
    if !is_final_id_segment(last_seg) {
        return None;
    }

    let id_end_in_rest = 1 + pos; // the leading '-' plus every consumed segment
    let id = format!("{prefix}{}", &rest[..id_end_in_rest]);
    let mut remainder = &rest[id_end_in_rest..];
    // ID 直後の ':' '.' は区切りとして許容する (§2.2).
    if let Some(r) = remainder
        .strip_prefix(':')
        .or_else(|| remainder.strip_prefix('.'))
    {
        remainder = r;
    }
    Some(IdMatch {
        id,
        title: remainder.trim().to_string(),
    })
}

/// The final `-`-separated segment of a candidate ID must be digits with an
/// optional single trailing lowercase ASCII letter (`003`, `001a`, but not
/// `003A`, `abc`, or `01ab`).
fn is_final_id_segment(seg: &str) -> bool {
    let digit_count = seg.chars().take_while(char::is_ascii_digit).count();
    if digit_count == 0 {
        return false;
    }
    match seg.len() - digit_count {
        0 => true,
        1 => seg.as_bytes()[digit_count].is_ascii_lowercase(),
        _ => false,
    }
}

/// §2.2's warning heuristic: a heading whose leading uppercase run (len >= 2)
/// is immediately followed by `-<digit>`, but is *not* an allowed prefix
/// (`HTTP-2`, `UTF-8`, `ISO-26262`). Deliberately independent of
/// [`match_item_id`]'s full grammar — an *allowed* prefix followed by a
/// malformed ID body (e.g. `REQ-abc`) is left as a silent ordinary heading,
/// not a warning, since §2.2 only calls out the "not in the allow list"
/// case.
fn is_id_like_but_disallowed(heading_text: &str, allowed: &HashSet<&str>) -> bool {
    let prefix_len = heading_text
        .as_bytes()
        .iter()
        .take_while(|b| b.is_ascii_uppercase())
        .count();
    if prefix_len < 2 {
        return false;
    }
    if allowed.contains(&heading_text[..prefix_len]) {
        return false;
    }
    heading_text[prefix_len..]
        .strip_prefix('-')
        .and_then(|rest| rest.as_bytes().first().copied())
        .is_some_and(|b: u8| b.is_ascii_digit())
}

// -- Attribute-line parsing (§2.2) --

/// Splits an item's fragment (everything after its own heading line, up to
/// the next terminating heading) into its known attributes and remaining
/// body text ("statement").
fn parse_item_body(fragment: &str) -> (ItemAttrs, String) {
    let lines = split_physical_lines(fragment);
    let mut attrs = ItemAttrs::default();
    let mut removed = vec![false; lines.len()];

    // Only the *first* contiguous bullet-list block immediately following
    // the heading (blank lines before it are fine) is ever scanned for
    // attributes (§2.2: "見出し直後（空行可）の連続した箇条書き"). A blank
    // line breaks the block (design decision, locked by tests): once inside
    // the block, only a strictly consecutive run of bullet lines counts.
    if let Some(first_nonblank) = lines.iter().position(|l| !l.trim().is_empty()) {
        if is_bullet_line(lines[first_nonblank]) {
            let mut idx = first_nonblank;
            while idx < lines.len() && is_bullet_line(lines[idx]) {
                if apply_attr_line(lines[idx], &mut attrs) {
                    removed[idx] = true;
                }
                idx += 1;
            }
        }
    }

    let statement: String = lines
        .iter()
        .zip(removed.iter())
        .filter(|(_, removed)| !**removed)
        .map(|(line, _)| *line)
        .collect();

    (attrs, statement.trim().to_string())
}

fn is_bullet_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ")
}

/// Applies one bullet line as an attribute if its key is known. Returns
/// `true` (and mutates `attrs`) iff it was a known-key line, in which case
/// the caller removes it from the item's `statement`; unknown keys and
/// malformed lines (no `:`) are left as body content (§2.2).
fn apply_attr_line(line: &str, attrs: &mut ItemAttrs) -> bool {
    let trimmed = line.trim_start();
    let content = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
        .unwrap_or(trimmed);
    let Some((key, value)) = content.split_once(':') else {
        return false;
    };
    let value = value.trim();
    match key.trim() {
        "refines" => {
            attrs.refines.extend(split_csv(value));
            true
        }
        "verifies" => {
            attrs.verifies.extend(split_csv(value));
            true
        }
        "layer" => {
            attrs.layer = Some(value.to_string());
            true
        }
        "priority" => {
            attrs.priority = Some(value.to_string());
            true
        }
        "method" => {
            attrs.method = Some(value.to_string());
            true
        }
        "test" => {
            attrs.test_refs.push(value.to_string());
            true
        }
        _ => false,
    }
}

fn split_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Splits `text` into physical lines, each *including* its own trailing
/// line terminator (if any) — so re-concatenating a filtered subset
/// reproduces the original bytes exactly for every kept line.
fn split_physical_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            lines.push(&text[start..=i]);
            start = i + 1;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

// -- Line-number lookup --

/// Byte offset of the start of each 1-based line (`line_starts[0] == 0`).
fn compute_line_starts(body: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, &b) in body.as_bytes().iter().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// 1-based line number containing byte `offset`.
fn line_number(line_starts: &[usize], offset: usize) -> usize {
    line_starts.partition_point(|&s| s <= offset)
}

/// Byte offset right after the end of the heading line starting at
/// `heading_start` (i.e. the start of the next line, or `body.len()` if the
/// heading has no trailing newline).
fn find_heading_line_end(body: &str, heading_start: usize) -> usize {
    match body[heading_start..].find('\n') {
        Some(rel) => heading_start + rel + 1,
        None => body.len(),
    }
}

// -- body_hash (§2.3) --

fn compute_body_hash(title: &str, statement: &str, attrs: &ItemAttrs) -> String {
    let combined = format!("{title}\n{statement}\n{}", attrs_repr(attrs));
    lexsim::fnv1a_hex(normalize_for_hash(&combined).as_bytes())
}

/// Deterministic (fixed field order, independent of source attribute-line
/// order) textual representation of `attrs`, for inclusion in `body_hash`.
fn attrs_repr(attrs: &ItemAttrs) -> String {
    let mut parts = Vec::new();
    if let Some(layer) = &attrs.layer {
        parts.push(format!("layer:{layer}"));
    }
    if !attrs.refines.is_empty() {
        parts.push(format!("refines:{}", attrs.refines.join(",")));
    }
    if !attrs.verifies.is_empty() {
        parts.push(format!("verifies:{}", attrs.verifies.join(",")));
    }
    if let Some(priority) = &attrs.priority {
        parts.push(format!("priority:{priority}"));
    }
    if let Some(method) = &attrs.method {
        parts.push(format!("method:{method}"));
    }
    if !attrs.test_refs.is_empty() {
        parts.push(format!("test:{}", attrs.test_refs.join(",")));
    }
    parts.join(";")
}

/// §2.3's `body_hash` normalization: consecutive whitespace (any mix of
/// spaces/tabs/CR/LF, so this also unifies CRLF vs LF) collapses to a single
/// space, then the result is trimmed.
fn normalize_for_hash(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_was_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_was_space {
                out.push(' ');
            }
            prev_was_space = true;
        } else {
            out.push(ch);
            prev_was_space = false;
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Built from just the 6 built-ins (no `[trace.id_prefixes]` additions)
    /// — matches every fixture below unless a test says otherwise.
    fn builtin_table() -> HashMap<String, Vec<String>> {
        default_prefix_table(&LayerRegistry::build(&[]), &HashMap::new())
    }

    fn parse(body: &str) -> LayerParseResult {
        parse_layer_body(body, Some("requirement"), &builtin_table())
    }

    // -- ID recognition --

    #[test]
    fn recognizes_simple_and_colon_separated_ids() {
        let result = parse("## REQ-003\nBody one.\n\n## SPEC-012: Title text\nBody two.\n");
        assert_eq!(result.items.len(), 2);
        assert_eq!(result.items[0].id, "REQ-003");
        assert_eq!(result.items[0].title, "");
        assert_eq!(result.items[1].id, "SPEC-012");
        assert_eq!(result.items[1].title, "Title text");
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn period_separator_is_also_allowed_right_after_the_id() {
        let result = parse("## REQ-003. タイトル\n本文。\n");
        assert_eq!(result.items[0].id, "REQ-003");
        assert_eq!(result.items[0].title, "タイトル");
    }

    #[test]
    fn recognizes_trailing_lowercase_letter_form() {
        let result = parse("## AT-001a ログイン成功\n手順...\n");
        assert_eq!(result.items[0].id, "AT-001a");
        assert_eq!(result.items[0].title, "ログイン成功");
    }

    #[test]
    fn recognizes_multi_segment_alnum_then_digits_form() {
        let result = parse("## ST-LOGIN-01 5回失敗でロックされる\n手順...\n");
        assert_eq!(result.items[0].id, "ST-LOGIN-01");
        assert_eq!(result.items[0].title, "5回失敗でロックされる");
    }

    #[test]
    fn http2_utf8_iso26262_are_never_misdetected_as_items_but_warn_as_id_like() {
        let result = parse(
            "## HTTP-2 対応\nテキスト。\n\n## UTF-8 encoding\ntext.\n\n## ISO-26262 compliance\ntext.\n",
        );
        assert!(
            result.items.is_empty(),
            "none of HTTP-2/UTF-8/ISO-26262 must become items: {:?}",
            result.items
        );
        assert_eq!(result.warnings.len(), 3);
        for w in &result.warnings {
            assert_eq!(w.kind, ParseWarningKind::IdLikeHeadingIgnored);
        }
        assert_eq!(result.warnings[0].heading, "HTTP-2 対応");
        assert_eq!(result.warnings[1].heading, "UTF-8 encoding");
        assert_eq!(result.warnings[2].heading, "ISO-26262 compliance");
    }

    #[test]
    fn ordinary_heading_with_no_uppercase_id_shape_gets_no_warning() {
        let result = parse("## Overview\nSome text.\n");
        assert!(result.items.is_empty());
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn allowed_prefix_with_malformed_id_body_is_silently_ordinary_no_warning() {
        // "REQ" is allowed, but "REQ-abc" fails the digit-based ID grammar.
        // §2.2 only calls out "not in the allow list" for the ID-like
        // warning, so this is left as an ordinary heading with no warning
        // (documented decision).
        let result = parse("## REQ-abc Not An Id\ntext\n");
        assert!(result.items.is_empty());
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn config_added_prefix_is_recognized() {
        let mut config = HashMap::new();
        config.insert("requirement".to_string(), vec!["UC".to_string()]);
        let table = default_prefix_table(&LayerRegistry::build(&[]), &config);
        let result = parse_layer_body("## UC-001 ログイン\n本文\n", Some("requirement"), &table);
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].id, "UC-001");
    }

    // -- fence handling (shared with split.rs) --

    #[test]
    fn heading_like_line_inside_a_fenced_code_block_is_ignored() {
        let body = "## REQ-001\n本文\n\n```\n## REQ-999 Fake\n```\n\n## REQ-002\n本文2\n";
        let result = parse(body);
        let ids: Vec<&str> = result.items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, vec!["REQ-001", "REQ-002"]);
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn tilde_fence_is_also_respected() {
        let body = "## REQ-001\n本文\n\n~~~\n## REQ-999 Fake\n~~~\n\n## REQ-002\n本文2\n";
        let result = parse(body);
        let ids: Vec<&str> = result.items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, vec!["REQ-001", "REQ-002"]);
    }

    #[test]
    fn setext_heading_is_never_a_boundary_and_stays_in_body() {
        // Matches split.rs's own treatment: a setext heading is not an ATX
        // heading, so it can never be an item boundary, item or otherwise.
        let body = "## REQ-001\nIntro\n\nSetext Title\n---\nMore body.\n";
        let result = parse(body);
        assert_eq!(result.items.len(), 1);
        assert!(result.items[0].statement.contains("Setext Title"));
        assert!(result.items[0].statement.contains("More body."));
    }

    // -- attribute lines --

    #[test]
    fn parses_refines_verifies_layer_priority_method_and_multiline_test() {
        let body = "## ST-040 5回失敗でロックされる\n\n\
- verifies: SPEC-012\n\
- refines: REQ-001, REQ-002\n\
- layer: system_test\n\
- priority: P1\n\
- method: manual\n\
- test: tests/lock.rs::case_a\n\
- test: tests/lock.rs::case_b\n\
\n\
手順: 誤パスワードで5回ログインする。\n\
期待結果: 6回目は拒否される。\n";
        let result = parse(body);
        let item = &result.items[0];
        assert_eq!(item.attrs.verifies, vec!["SPEC-012"]);
        assert_eq!(item.attrs.refines, vec!["REQ-001", "REQ-002"]);
        assert_eq!(item.attrs.layer.as_deref(), Some("system_test"));
        assert_eq!(item.attrs.priority.as_deref(), Some("P1"));
        assert_eq!(item.attrs.method.as_deref(), Some("manual"));
        assert_eq!(
            item.attrs.test_refs,
            vec!["tests/lock.rs::case_a", "tests/lock.rs::case_b"]
        );
        assert!(item.statement.contains("手順"));
        assert!(item.statement.contains("期待結果"));
        assert!(!item.statement.contains("verifies"));
        assert_eq!(item.effective_layer.as_deref(), Some("system_test"));
    }

    #[test]
    fn item_without_layer_override_inherits_doc_layer() {
        let result = parse("## REQ-001 Title\nBody.\n");
        assert_eq!(result.items[0].attrs.layer, None);
        assert_eq!(
            result.items[0].effective_layer.as_deref(),
            Some("requirement")
        );
    }

    #[test]
    fn unknown_attribute_key_stays_in_body() {
        let body = "## REQ-001\n\n- refines: NONE\n- owner: alice\n\nBody text.\n";
        let result = parse(body);
        let item = &result.items[0];
        assert_eq!(item.attrs.refines, vec!["NONE"]);
        assert!(item.statement.contains("owner: alice"));
    }

    #[test]
    fn second_bullet_block_is_body_not_attributes() {
        let body = "## REQ-001\n\n- priority: P1\n\nSome text.\n\n- refines: REQ-999\n";
        let result = parse(body);
        let item = &result.items[0];
        assert_eq!(item.attrs.priority.as_deref(), Some("P1"));
        assert!(item.attrs.refines.is_empty());
        assert!(item.statement.contains("- refines: REQ-999"));
    }

    #[test]
    fn body_text_before_a_list_disqualifies_attribute_recognition_entirely() {
        let body = "## REQ-001\n\nIntro text.\n\n- priority: P1\n";
        let result = parse(body);
        let item = &result.items[0];
        assert_eq!(item.attrs.priority, None);
        assert!(item.statement.contains("- priority: P1"));
    }

    // -- body range / nesting / duplicates --

    #[test]
    fn deeper_non_item_heading_stays_inside_the_parent_items_statement() {
        let body = "## REQ-001\nIntro.\n\n#### Notes\nMore detail.\n\n## REQ-002\nNext.\n";
        let result = parse(body);
        assert_eq!(result.items.len(), 2);
        assert!(result.items[0].statement.contains("Notes"));
        assert!(result.items[0].statement.contains("More detail."));
    }

    #[test]
    fn same_or_higher_level_ordinary_heading_terminates_the_item() {
        let body = "## REQ-001\nIntro.\n\n## Ordinary Section\nUnrelated.\n";
        let result = parse(body);
        assert_eq!(result.items.len(), 1);
        assert!(!result.items[0].statement.contains("Unrelated"));
    }

    #[test]
    fn nested_item_heading_ends_the_parent_body_without_implicit_nesting() {
        let body = "## REQ-001\nIntro.\n\n### REQ-002\nChild.\n";
        let result = parse(body);
        assert_eq!(result.items.len(), 2);
        assert!(!result.items[0].statement.contains("Child"));
        assert_eq!(result.items[1].statement, "Child.");
    }

    #[test]
    fn duplicate_id_is_warned_and_second_occurrence_ignored() {
        let body = "## REQ-001 First\nA.\n\n## REQ-001 Second\nB.\n";
        let result = parse(body);
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].title, "First");
        assert_eq!(result.warnings.len(), 1);
        assert_eq!(result.warnings[0].kind, ParseWarningKind::DuplicateId);
        assert_eq!(result.warnings[0].id.as_deref(), Some("REQ-001"));
    }

    // -- line ranges --

    #[test]
    fn start_and_end_line_cover_the_items_own_span() {
        let body = "Preamble\n## REQ-001\nLine3\nLine4\n## REQ-002\nLine6\n";
        let result = parse(body);
        assert_eq!(result.items[0].start_line, 2);
        assert_eq!(result.items[0].end_line, 4);
        assert_eq!(result.items[1].start_line, 5);
        assert_eq!(result.items[1].end_line, 6);
    }

    // -- CRLF / tabs / full-width text --

    #[test]
    fn crlf_body_is_handled_and_hashes_equal_to_the_lf_equivalent() {
        let lf = "## REQ-001 Title\nLine one.\nLine two.\n";
        let crlf = "## REQ-001 Title\r\nLine one.\r\nLine two.\r\n";
        let lf_result = parse(lf);
        let crlf_result = parse(crlf);
        assert_eq!(lf_result.items[0].id, crlf_result.items[0].id);
        assert_eq!(lf_result.items[0].body_hash, crlf_result.items[0].body_hash);
    }

    #[test]
    fn tabs_and_extra_whitespace_normalize_to_the_same_body_hash() {
        let a = parse("## REQ-001 Title\nBody text here.\n");
        let b = parse("## REQ-001 Title\nBody   text\there.\n");
        assert_eq!(a.items[0].body_hash, b.items[0].body_hash);
    }

    #[test]
    fn full_width_japanese_body_is_handled_without_panics() {
        let result =
            parse("## REQ-001 全角テスト\n５回連続で認証に失敗したアカウントをロックする。\n");
        assert_eq!(result.items[0].title, "全角テスト");
        assert!(result.items[0].statement.contains("ロックする"));
    }

    #[test]
    fn atx_closing_hashes_are_stripped_from_the_heading_text() {
        let result = parse("## REQ-001 Title ##\nBody.\n");
        assert_eq!(result.items[0].id, "REQ-001");
        assert_eq!(result.items[0].title, "Title");
    }

    // -- body_hash sensitivity --

    #[test]
    fn body_hash_changes_when_statement_changes() {
        let a = parse("## REQ-001 Title\nOriginal body.\n");
        let b = parse("## REQ-001 Title\nChanged body.\n");
        assert_ne!(a.items[0].body_hash, b.items[0].body_hash);
    }

    #[test]
    fn body_hash_changes_when_an_attribute_changes() {
        let a = parse("## REQ-001 Title\n\n- priority: P1\n\nBody.\n");
        let b = parse("## REQ-001 Title\n\n- priority: P2\n\nBody.\n");
        assert_ne!(a.items[0].body_hash, b.items[0].body_hash);
    }

    // -- performance smoke test (NFR-003) --

    /// A single parse pass over a ~1.25MB Japanese-scale body must stay well
    /// under the 1s trace-aggregation budget it feeds into (NFR-003). This
    /// is a smoke/regression bound (10x margin), not a tight perf assertion;
    /// see the dev report for the actually-measured wall time.
    #[test]
    fn parses_ja_scale_body_within_perf_budget() {
        let mut body = String::with_capacity(1_300_000);
        let mut n = 0usize;
        while body.len() < 1_250_000 {
            body.push_str(&format!(
                "## REQ-{n:04} 全角の要件タイトルです\n\n- priority: P1\n- refines: REQ-0000\n\n\
                 これは日本語の本文です。連続する空白や全角記号を含みます。　テスト。\n\n"
            ));
            n += 1;
        }
        let table = builtin_table();
        let start = std::time::Instant::now();
        let result = parse_layer_body(&body, Some("requirement"), &table);
        let elapsed = start.elapsed();
        assert!(!result.items.is_empty());
        assert!(
            elapsed.as_secs_f64() < 1.0,
            "parse_layer_body took {elapsed:?} for a {}-byte body, budget is 1s",
            body.len()
        );
    }
}
