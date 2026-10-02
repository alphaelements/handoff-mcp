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

use std::collections::{BTreeMap, HashMap, HashSet};

use pulldown_cmark::{Event, Parser as MdParser, Tag, TagEnd};
use unicode_normalization::UnicodeNormalization;

use super::layer::LayerRegistry;
use super::model::{AcRef, Waiver};
use super::split::collect_all_heading_bounds;

/// One item's known **M1** attribute-line values (§2.2's original
/// vocabulary: `refines`, `verifies`, `layer`, `priority`, `method`,
/// `test`). Unknown keys, and any bullet-list block after the first one
/// immediately following the heading, are left untouched in the item's
/// `statement` (§2.2: "未知キーの行と2つ目以降の箇条書きブロックは本文扱い").
///
/// **Frozen at the M1 key set (E14, wiki/260-vmodel-m2-design.md §2.2):**
/// this struct — and the `statement` it is parsed alongside — deliberately
/// do *not* grow the M2 keys (`rationale`/`derived`/`waive-*`/`from`/
/// reserved). Those are recognized separately, into [`ExtAttrs`], so
/// `body_hash` (computed from *this* struct + `statement`) stays
/// byte-identical to M1 even for a document that already had e.g. a literal
/// `- rationale: …` line before M2 existed.
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

/// One item's **M2** attribute-line values (wiki/260-vmodel-m2-design.md
/// §2.2): `rationale`, `derived`, `waive-verify`/`waive-refine`, `from`, and
/// the `assignee`/`needs` keys (both promoted to their own fields in M3 —
/// wiki/270-vmodel-m3-design.md §2.1/§2.2). Parsed independently of
/// [`ItemAttrs`] (see its doc comment for why) but from the same leading
/// bullet-list block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtAttrs {
    /// `- rationale: <text>`.
    pub rationale: Option<String>,
    /// `- derived: <reason>`. `None` when absent *or* when authored with an
    /// empty reason (a parse warning covers the latter — §2.2: "理由が空の
    /// derived / waive-* は warning を出し、その属性を無効にする").
    pub derived: Option<String>,
    /// `- waive-verify: <reason>` / `- waive-refine: <reason>`, in source
    /// order. Same empty-reason-drops-the-attribute rule as `derived`.
    pub waivers: Vec<Waiver>,
    /// `- from: <id>`.
    pub from: Option<String>,
    /// M3 (wiki/270-vmodel-m3-design.md §2.2, FR-307): `- assignee: <key>` —
    /// promoted out of [`Self::reserved`] into its own field. Stored
    /// verbatim (roster-key validation against `config.toml`'s
    /// `[assignees.<key>]` is a tool-side concern — this pure parser has no
    /// config access — t360.40.02's `layer_sync`/`docs.rs` wiring does that),
    /// last-line-wins on repetition (same convention as `layer`/`priority` in
    /// [`ItemAttrs`]).
    pub assignee: Option<String>,
    /// M3 (wiki/270-vmodel-m3-design.md §2.1, FR-202): `- needs:
    /// <id>[,<id>...]` — promoted out of [`Self::reserved`] into its own
    /// field, parsed as a comma-separated list of layer ids (E16: a single
    /// line, no multi-line list notation). Trimmed, empty entries dropped
    /// (e.g. a trailing comma or double comma does not produce a `""`
    /// entry). Last-line-wins on repetition (same convention as
    /// `layer`/`priority` in [`ItemAttrs`]) — a repeated `- needs:` line
    /// replaces, not appends to, the previous one.
    ///
    /// `Some(vec![])` is reachable here (an authored `- needs:` line with an
    /// empty — or comma/whitespace-only — value): this pure parser does not
    /// itself distinguish "explicitly no coverage" from "nothing after the
    /// colon by mistake"; `SubItem.needs`'s 3-state semantics (§2.1) treats
    /// both the same way, as an explicit empty list. Unknown layer ids are
    /// *not* filtered here (this module has no [`super::layer::LayerRegistry`]
    /// access) — that validation, and the resulting warning, is
    /// `layer_sync.rs`'s/the `docs.rs` caller's job (same pattern as
    /// `assignee`'s roster check).
    pub needs: Option<Vec<String>>,
    /// Any other reserved/unrecognized-but-tracked key. Currently empty —
    /// `assignee`/`needs` were the last M2-reserved keys, both now promoted
    /// to their own fields above. Kept for forward compatibility (a future
    /// milestone's new reserved key lands here first, mirroring how
    /// `assignee`/`needs` themselves started).
    pub reserved: BTreeMap<String, String>,
}

/// One parsed acceptance-criteria bullet (§2.2), before it is turned into a
/// storage-facing [`AcRef`] (which drops `text` — D1, the text is
/// re-derived from the body on demand, never duplicated in storage).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedAc {
    pub label: String,
    /// `"gwt"` | `"ears"` | `"text"` (§2.2's classification).
    pub kind: &'static str,
    /// The bullet's own text (label prefix stripped), continuation lines
    /// joined with a single space.
    pub text: String,
    /// `ac_hash(item, label)` (§2.4): FNV-1a of the item's normalized
    /// `title + statement-minus-acceptance-block` combined with this AC's
    /// own normalized text.
    pub ac_hash: String,
}

impl From<&ParsedAc> for AcRef {
    fn from(ac: &ParsedAc) -> Self {
        AcRef {
            label: ac.label.clone(),
            kind: ac.kind.to_string(),
        }
    }
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
    /// Item body ("statement"): **M1** attribute lines removed,
    /// leading/trailing whitespace trimmed, internal formatting kept as
    /// authored. Feeds `body_hash` only (E14) — still includes the
    /// acceptance-criteria block and every M2 attribute line verbatim.
    pub statement: String,
    /// `attrs.layer.or(doc_layer)` (§2.3) — the effective layer this item
    /// lives on, computed here so t360.6 does not have to repeat the rule.
    pub effective_layer: Option<String>,
    /// FNV-1a hex of the normalized `title` + `statement` + `attrs` (§2.3,
    /// M1 key set only — E14, see [`ItemAttrs`]'s doc comment).
    pub body_hash: String,

    /// M2 (wiki/260-vmodel-m2-design.md §2.2): this item's parsed extended
    /// attributes (`rationale`/`derived`/`waive-*`/`from`/reserved).
    pub ext_attrs: ExtAttrs,
    /// M2 §2.2: this item's parsed acceptance-criteria bullets, in source
    /// (accepted) order.
    pub acceptance: Vec<ParsedAc>,
    /// M2 §2.4: FNV-1a hex of the normalized `{title,
    /// statement-minus-acceptance-block, acceptance list (label:text, label
    /// order)}` — attributes (M1 and M2 alike) excluded from the input.
    pub def_hash: String,
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
    /// M2 §2.2: an acceptance-criteria bullet had no `AC<n>:` label, so a
    /// position-based one was assigned — reordering the bullets would then
    /// change which label this AC gets.
    AcLabelPositional,
    /// M2 §2.2: an acceptance-criteria bullet's label duplicated an earlier
    /// one in the same item; the 2nd (and later) occurrence is ignored.
    AcDuplicateLabel,
    /// M2 §2.2: a `- derived: <reason>` / `- waive-verify: <reason>` /
    /// `- waive-refine: <reason>` line had an empty reason; the attribute is
    /// dropped rather than stored with no explanation.
    EmptyReason,
}

/// A non-fatal issue found while parsing (§2.2/§2.3). Parsing never fails
/// outright — every warning corresponds to a heading that is simply not
/// turned into an item (or, for a duplicate, not turned into a *second*
/// item).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseWarning {
    pub kind: ParseWarningKind,
    /// 1-based line number of the offending heading (or, for an
    /// item-interior warning — `AcLabelPositional`/`AcDuplicateLabel`/
    /// `EmptyReason` — the item's own heading line: pinpointing the exact
    /// interior line is not worth a second line-number pass for a
    /// non-fatal warning).
    pub line: usize,
    /// The heading's full trimmed text, for the caller to build a message.
    pub heading: String,
    /// For [`ParseWarningKind::DuplicateId`], the id that was duplicated.
    /// For an item-interior M2 warning, the owning item's id. `None` for
    /// [`ParseWarningKind::IdLikeHeadingIgnored`].
    pub id: Option<String>,
    /// Extra context for an M2 item-interior warning: the assigned/
    /// duplicated AC label (`AcLabelPositional`/`AcDuplicateLabel`) or the
    /// empty attribute's key (`EmptyReason`, e.g. `"derived"` /
    /// `"waive-verify"`). `None` for the M1 warning kinds.
    pub detail: Option<String>,
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
            ParseWarningKind::AcLabelPositional => write!(
                f,
                "line {}: item \"{}\" acceptance bullet has no label, assigned \"{}\" by position",
                self.line,
                self.id.as_deref().unwrap_or(""),
                self.detail.as_deref().unwrap_or("")
            ),
            ParseWarningKind::AcDuplicateLabel => write!(
                f,
                "line {}: item \"{}\" duplicate acceptance label \"{}\" ignored",
                self.line,
                self.id.as_deref().unwrap_or(""),
                self.detail.as_deref().unwrap_or("")
            ),
            ParseWarningKind::EmptyReason => write!(
                f,
                "line {}: item \"{}\" attribute \"{}\" has an empty reason, ignored",
                self.line,
                self.id.as_deref().unwrap_or(""),
                self.detail.as_deref().unwrap_or("")
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
                    detail: None,
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
                detail: None,
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
        let mut parsed_fragment = parse_item_body_full(fragment);
        let def_statement_for_ac = parsed_fragment.def_statement.clone();
        for ac in &mut parsed_fragment.acceptance {
            ac.ac_hash = compute_ac_hash(&id_match.title, &def_statement_for_ac, &ac.text);
        }

        for w in &parsed_fragment.item_warnings {
            warnings.push(ParseWarning {
                kind: w.kind,
                line: start_line,
                heading: heading.text.clone(),
                id: Some(id_match.id.clone()),
                detail: w.detail.clone(),
            });
        }

        let end_line = if end_byte > 0 {
            line_number(&line_starts, end_byte - 1)
        } else {
            start_line
        };

        let effective_layer = parsed_fragment
            .attrs
            .layer
            .clone()
            .or_else(|| doc_layer.map(str::to_string));

        let body_hash = compute_body_hash(
            &id_match.title,
            &parsed_fragment.statement,
            &parsed_fragment.attrs,
        );
        let def_hash = compute_def_hash(
            &id_match.title,
            &parsed_fragment.def_statement,
            &parsed_fragment.acceptance,
        );

        items.push(ParsedItem {
            id: id_match.id.clone(),
            title: id_match.title.clone(),
            heading_level: heading.level,
            start_line,
            end_line,
            attrs: parsed_fragment.attrs,
            statement: parsed_fragment.statement,
            effective_layer,
            body_hash,
            ext_attrs: parsed_fragment.ext,
            acceptance: parsed_fragment.acceptance,
            def_hash,
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

// -- M2 extended attribute-line / acceptance-block parsing (§2.2) --

/// One non-fatal, item-interior issue found while parsing an item's M2
/// attributes/acceptance block — turned into a full [`ParseWarning`] by the
/// caller ([`parse_layer_body`]), which has the item's heading/line/id.
struct ItemWarningRaw {
    kind: ParseWarningKind,
    detail: Option<String>,
}

/// [`parse_item_body_full`]'s result.
struct ParsedFragment {
    /// M1 attributes (unchanged behavior — feeds `body_hash`).
    attrs: ItemAttrs,
    /// M1 statement (unchanged behavior — feeds `body_hash`, E14).
    statement: String,
    /// Statement with **both** M1 and M2 attribute lines removed, and the
    /// acceptance-criteria block (if any) removed — feeds `def_hash`/
    /// `ac_hash` (§2.4).
    def_statement: String,
    ext: ExtAttrs,
    acceptance: Vec<ParsedAc>,
    item_warnings: Vec<ItemWarningRaw>,
}

/// Extends [`parse_item_body`]'s M1-only pass with M2's extended attributes
/// and acceptance-criteria block (wiki/260-vmodel-m2-design.md §2.2),
/// **without** changing what M1's own pass produces (`attrs`/`statement`,
/// both computed exactly as `parse_item_body` would — this function calls
/// it internally rather than duplicating its logic, so the two can never
/// drift apart).
fn parse_item_body_full(fragment: &str) -> ParsedFragment {
    let (attrs, statement) = parse_item_body(fragment);

    let lines = split_physical_lines(fragment);
    let mut ext = ExtAttrs::default();
    let mut item_warnings = Vec::new();
    let mut removed_ext = vec![false; lines.len()];

    // Same block-extent rule as M1's own pass (first contiguous bullet-list
    // block right after the heading) — re-derived here rather than shared
    // with `parse_item_body`, since the M1 pass's `removed` mask (which
    // lines are attribute lines) is not returned to callers.
    if let Some(first_nonblank) = lines.iter().position(|l| !l.trim().is_empty()) {
        if is_bullet_line(lines[first_nonblank]) {
            let mut idx = first_nonblank;
            while idx < lines.len() && is_bullet_line(lines[idx]) {
                if let Some(outcome) = apply_extended_attr_line(lines[idx], &mut ext) {
                    removed_ext[idx] = true;
                    if let ExtAttrOutcome::EmptyReason(key) = outcome {
                        item_warnings.push(ItemWarningRaw {
                            kind: ParseWarningKind::EmptyReason,
                            detail: Some(key),
                        });
                    }
                }
                idx += 1;
            }
        }
    }

    // M1's own removal mask, recomputed the same way `parse_item_body` does
    // internally (cheap — a handful of short lines), so `def_statement` can
    // combine it with `removed_ext` above without `parse_item_body` having
    // to expose its private mask.
    let mut removed_m1 = vec![false; lines.len()];
    {
        let mut probe_attrs = ItemAttrs::default();
        if let Some(first_nonblank) = lines.iter().position(|l| !l.trim().is_empty()) {
            if is_bullet_line(lines[first_nonblank]) {
                let mut idx = first_nonblank;
                while idx < lines.len() && is_bullet_line(lines[idx]) {
                    if apply_attr_line(lines[idx], &mut probe_attrs) {
                        removed_m1[idx] = true;
                    }
                    idx += 1;
                }
            }
        }
    }

    let acceptance_block = find_acceptance_block(&lines);
    let mut removed_ac = vec![false; lines.len()];
    if let Some(block) = &acceptance_block {
        for flag in &mut removed_ac[block.start_idx..block.end_idx] {
            *flag = true;
        }
    }

    let def_statement: String = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| !removed_m1[*i] && !removed_ext[*i] && !removed_ac[*i])
        .map(|(_, line)| *line)
        .collect();
    let def_statement = def_statement.trim().to_string();

    let mut acceptance = Vec::new();
    if let Some(block) = acceptance_block {
        let mut seen_labels: HashSet<String> = HashSet::new();
        let mut position = 0usize;
        for raw in &block.raw_items {
            position += 1;
            let (label, text, positional) = match extract_ac_label(raw) {
                Some((label, text)) => (label, text, false),
                None => (format!("AC{position}"), raw.trim().to_string(), true),
            };
            if !seen_labels.insert(label.clone()) {
                item_warnings.push(ItemWarningRaw {
                    kind: ParseWarningKind::AcDuplicateLabel,
                    detail: Some(label.clone()),
                });
                continue;
            }
            if positional {
                item_warnings.push(ItemWarningRaw {
                    kind: ParseWarningKind::AcLabelPositional,
                    detail: Some(label.clone()),
                });
            }
            let kind = classify_ac_kind(&text);
            // `ac_hash` needs the item's `title` (§2.4), not yet known here
            // (this function only sees the fragment after the heading) —
            // filled in by the caller (`parse_layer_body`) via
            // `compute_ac_hash` once `title` is available.
            acceptance.push(ParsedAc {
                label,
                kind,
                text,
                ac_hash: String::new(),
            });
        }
    }

    ParsedFragment {
        attrs,
        statement,
        def_statement,
        ext,
        acceptance,
        item_warnings,
    }
}

/// Outcome of successfully matching an M2 extended attribute key.
enum ExtAttrOutcome {
    /// Matched and stored.
    Stored,
    /// Matched but the reason was empty (`derived`/`waive-verify`/
    /// `waive-refine`) — the attribute is *not* stored; `String` is the key
    /// name for the warning.
    EmptyReason(String),
}

/// Applies one bullet line as an M2 extended attribute if its key is known.
/// Mirrors [`apply_attr_line`]'s contract (returns `None` for an unknown key
/// or a malformed line, in which case the caller leaves the line as body
/// content) but for the M2/M3 key set (`rationale`/`derived`/`waive-verify`/
/// `waive-refine`/`from`/`assignee`/`needs`).
fn apply_extended_attr_line(line: &str, ext: &mut ExtAttrs) -> Option<ExtAttrOutcome> {
    let trimmed = line.trim_start();
    let content = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
        .unwrap_or(trimmed);
    let (key, value) = content.split_once(':')?;
    let key = key.trim();
    let value = value.trim();
    match key {
        "rationale" => {
            ext.rationale = Some(value.to_string());
            Some(ExtAttrOutcome::Stored)
        }
        "derived" => {
            if value.is_empty() {
                Some(ExtAttrOutcome::EmptyReason("derived".to_string()))
            } else {
                ext.derived = Some(value.to_string());
                Some(ExtAttrOutcome::Stored)
            }
        }
        "waive-verify" | "waive-refine" => {
            let axis = if key == "waive-verify" {
                "verify"
            } else {
                "refine"
            };
            if value.is_empty() {
                Some(ExtAttrOutcome::EmptyReason(key.to_string()))
            } else {
                ext.waivers.push(Waiver {
                    axis: axis.to_string(),
                    reason: value.to_string(),
                });
                Some(ExtAttrOutcome::Stored)
            }
        }
        "from" => {
            ext.from = Some(value.to_string());
            Some(ExtAttrOutcome::Stored)
        }
        "assignee" => {
            ext.assignee = Some(value.to_string());
            Some(ExtAttrOutcome::Stored)
        }
        "needs" => {
            ext.needs = Some(
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect(),
            );
            Some(ExtAttrOutcome::Stored)
        }
        _ => None,
    }
}

/// One located acceptance-criteria block within an item's fragment lines.
struct AcceptanceBlock {
    /// Index (into the fragment's physical-line array) of the trigger
    /// paragraph line (e.g. "受入基準:").
    start_idx: usize,
    /// One past the last line consumed by the block's bullets.
    end_idx: usize,
    /// Each bullet's raw text (marker stripped, continuation lines joined
    /// with a single space), in source order.
    raw_items: Vec<String>,
}

/// §2.2: a line whose text, once emphasis markers and a trailing `:`/`：`
/// are stripped, case-insensitively equals "受入基準"/"Acceptance criteria"/
/// "Acceptance".
fn is_ac_trigger_line(line: &str) -> bool {
    let t = line.trim();
    let t = t.trim_matches(|c: char| c == '*' || c == '_' || c == '#');
    let t = t.trim();
    let t = t
        .strip_suffix(':')
        .or_else(|| t.strip_suffix('：'))
        .unwrap_or(t);
    let t = t.trim();
    t == "受入基準"
        || t.eq_ignore_ascii_case("acceptance criteria")
        || t.eq_ignore_ascii_case("acceptance")
}

/// Finds the first acceptance-criteria block in `lines` (§2.2): a trigger
/// line (see [`is_ac_trigger_line`]) whose next non-blank line is a bullet.
/// A bullet's own continuation lines are any following non-blank,
/// non-bullet lines (§2.2: "字下げした継続行は同じ箇条書きに含める" —
/// indentation itself is not checked beyond "not itself a new bullet",
/// matching how [`is_bullet_line`] already ignores leading whitespace). The
/// block ends at the first blank line after its first bullet, or at the end
/// of `lines`.
fn find_acceptance_block(lines: &[&str]) -> Option<AcceptanceBlock> {
    for i in 0..lines.len() {
        if lines[i].trim().is_empty() || !is_ac_trigger_line(lines[i]) {
            continue;
        }
        let mut j = i + 1;
        while j < lines.len() && lines[j].trim().is_empty() {
            j += 1;
        }
        if j >= lines.len() || !is_bullet_line(lines[j]) {
            continue;
        }
        let mut raw_items = Vec::new();
        let mut k = j;
        while k < lines.len() && is_bullet_line(lines[k]) {
            let mut text = strip_bullet_marker(lines[k]).trim().to_string();
            k += 1;
            while k < lines.len() && !lines[k].trim().is_empty() && !is_bullet_line(lines[k]) {
                text.push(' ');
                text.push_str(lines[k].trim());
                k += 1;
            }
            raw_items.push(text);
        }
        return Some(AcceptanceBlock {
            start_idx: i,
            end_idx: k,
            raw_items,
        });
    }
    None
}

fn strip_bullet_marker(line: &str) -> &str {
    let trimmed = line.trim_start();
    trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
        .unwrap_or(trimmed)
}

/// Recognizes a leading `AC<digits>:` label (case-insensitive `AC`) at the
/// start of a raw acceptance-bullet text, returning `(label, remaining
/// text)` — `label` is always normalized to `AC<digits>` (uppercase),
/// `text` has the label prefix and its following whitespace trimmed.
fn extract_ac_label(raw: &str) -> Option<(String, String)> {
    if raw.len() < 3 || !raw.is_char_boundary(2) || !raw[..2].eq_ignore_ascii_case("ac") {
        return None;
    }
    let rest = &raw[2..];
    let digit_len = rest.chars().take_while(char::is_ascii_digit).count();
    if digit_len == 0 {
        return None;
    }
    let digits = &rest[..digit_len];
    let after_digits = &rest[digit_len..];
    let after_colon = after_digits.strip_prefix(':')?;
    Some((format!("AC{digits}"), after_colon.trim().to_string()))
}

/// §2.2's kind classification: `Given … When … Then` (case-insensitive, in
/// that order) -> `"gwt"`; starting with `WHEN`/`WHILE`/`WHERE`/`IF`
/// (case-insensitive) and containing `SHALL` -> `"ears"`; otherwise
/// `"text"`.
fn classify_ac_kind(text: &str) -> &'static str {
    let lower = text.to_lowercase();
    if let (Some(gi), Some(wi), Some(ti)) =
        (lower.find("given"), lower.find("when"), lower.find("then"))
    {
        if gi < wi && wi < ti {
            return "gwt";
        }
    }
    let starts_ears = ["when", "while", "where", "if"]
        .iter()
        .any(|kw| lower.trim_start().starts_with(kw));
    if starts_ears && lower.contains("shall") {
        return "ears";
    }
    "text"
}

// -- def_hash / ac_hash normalization (§2.4) --

/// Walks `md` as Markdown (pulldown-cmark) and concatenates the plain text
/// of every text run, inline-code span, and link destination URL — the same
/// normalization §2.4 asks for ("インライン要素をたどり、テキスト・インライン
/// コード・リンク先 URL を連結する"), so emphasis markers, list bullets, and
/// line-wrap differences never affect `def_hash`/`ac_hash`.
fn markdown_plain_text(md: &str) -> String {
    let mut out = String::new();
    for event in MdParser::new(md) {
        match event {
            Event::Text(t) | Event::Code(t) => {
                out.push_str(&t);
                out.push(' ');
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                out.push_str(&dest_url);
                out.push(' ');
            }
            Event::SoftBreak | Event::HardBreak => out.push(' '),
            Event::End(TagEnd::Paragraph)
            | Event::End(TagEnd::Heading(_))
            | Event::End(TagEnd::Item) => {
                out.push(' ');
            }
            _ => {}
        }
    }
    out
}

/// §2.4's full normalization pipeline: Markdown-aware plain-text extraction
/// -> NFKC (`unicode-normalization`) -> collapse consecutive whitespace to a
/// single space -> trim.
fn normalize_markdown_text(md: &str) -> String {
    let plain = markdown_plain_text(md);
    let nfkc: String = plain.nfkc().collect();
    let mut out = String::with_capacity(nfkc.len());
    let mut prev_was_space = false;
    for ch in nfkc.chars() {
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

/// §2.4: `def_hash` = FNV-1a of the normalized `{title,
/// statement-minus-acceptance-block, acceptance list (label order,
/// "label:text")}`. Attributes (M1 and M2 alike) are never part of the
/// input — `def_statement` already has them removed (see
/// [`parse_item_body_full`]).
fn compute_def_hash(title: &str, def_statement: &str, acceptance: &[ParsedAc]) -> String {
    let head = format!(
        "{} {}",
        normalize_markdown_text(title),
        normalize_markdown_text(def_statement)
    );
    let ac_repr: Vec<String> = acceptance
        .iter()
        .map(|a| format!("{}:{}", a.label, normalize_markdown_text(&a.text)))
        .collect();
    let combined = format!("{head} {}", ac_repr.join(" "));
    lexsim::fnv1a_hex(combined.as_bytes())
}

/// §2.4: `ac_hash(X, ACn)` = FNV-1a of the normalized `{title,
/// statement-minus-acceptance-block}` combined with this one AC's own
/// normalized text — deliberately excludes every *other* AC's text, so
/// editing `AC2` never changes `ac_hash(X, AC1)`.
fn compute_ac_hash(title: &str, def_statement: &str, ac_text: &str) -> String {
    let head = format!(
        "{} {}",
        normalize_markdown_text(title),
        normalize_markdown_text(def_statement)
    );
    let combined = format!("{head} {}", normalize_markdown_text(ac_text));
    lexsim::fnv1a_hex(combined.as_bytes())
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

    // -- M2 extended attributes (wiki/260-vmodel-m2-design.md §2.2) --

    #[test]
    fn parses_rationale_derived_waive_and_from_attributes() {
        let body = "## SPEC-020 監査ログの保存形式\n\n\
- refines: REQ-003\n\
- rationale: 総当たり攻撃の抑止\n\
- derived: 実装方式から必要になった項目\n\
- waive-verify: 文言のみのため目視レビューで代替\n\
- waive-refine: 上位要件なし\n\
- from: REQ-003#AC1\n\n\
本文テキスト。\n";
        let result = parse(body);
        let item = &result.items[0];
        assert_eq!(
            item.ext_attrs.rationale.as_deref(),
            Some("総当たり攻撃の抑止")
        );
        assert_eq!(
            item.ext_attrs.derived.as_deref(),
            Some("実装方式から必要になった項目")
        );
        assert_eq!(item.ext_attrs.waivers.len(), 2);
        assert_eq!(item.ext_attrs.waivers[0].axis, "verify");
        assert_eq!(
            item.ext_attrs.waivers[0].reason,
            "文言のみのため目視レビューで代替"
        );
        assert_eq!(item.ext_attrs.waivers[1].axis, "refine");
        assert_eq!(item.ext_attrs.from.as_deref(), Some("REQ-003#AC1"));
        assert!(item.statement.contains("本文テキスト"));
        // E14/M1 compat: `statement` (which feeds `body_hash`) is the M1
        // statement — M1 never recognized these keys, so their lines stay
        // in `statement` verbatim, exactly as they would have under M1.
        assert!(
            item.statement.contains("rationale"),
            "M2 attribute lines must stay in the M1 (body_hash) statement, E14: {}",
            item.statement
        );
        // They ARE removed from the def-hash statement, though (§2.4).
        assert!(!item.def_hash.is_empty());
    }

    /// M3 (wiki/270-vmodel-m3-design.md §2.1/§2.2, FR-202/FR-307): `assignee`
    /// and `needs` are both their own `ExtAttrs` fields now (promoted out of
    /// `reserved`).
    #[test]
    fn parses_assignee_attribute_as_its_own_field_and_needs_as_a_csv_list() {
        let body = "## REQ-003\n\n- assignee: alice\n- needs: acceptance, system_test\n\n本文。\n";
        let result = parse(body);
        let item = &result.items[0];
        assert_eq!(item.ext_attrs.assignee.as_deref(), Some("alice"));
        assert!(
            !item.ext_attrs.reserved.contains_key("assignee"),
            "assignee must no longer live in reserved: {:?}",
            item.ext_attrs.reserved
        );
        assert_eq!(
            item.ext_attrs.needs,
            Some(vec!["acceptance".to_string(), "system_test".to_string()])
        );
        assert!(
            !item.ext_attrs.reserved.contains_key("needs"),
            "needs must no longer live in reserved: {:?}",
            item.ext_attrs.reserved
        );
    }

    /// E16: `needs` is a single comma-separated line — extra whitespace
    /// around commas is trimmed, and a trailing/doubled comma does not
    /// produce a spurious empty entry.
    #[test]
    fn needs_csv_trims_whitespace_and_drops_empty_entries() {
        let body = "## REQ-010\n\n- needs: acceptance ,  system_test ,,\n\n本文。\n";
        let result = parse(body);
        assert_eq!(
            result.items[0].ext_attrs.needs,
            Some(vec!["acceptance".to_string(), "system_test".to_string()])
        );
    }

    /// §2.1 3-state semantics: an authored `- needs:` line with nothing
    /// after the colon parses as `Some(vec![])` — distinct from the
    /// attribute being absent entirely (`None`, checked by
    /// `ext_attrs_default_has_no_needs_or_assignee` below).
    #[test]
    fn empty_needs_value_parses_as_some_empty_vec_not_none() {
        let body = "## REQ-011\n\n- needs:\n\n本文。\n";
        let result = parse(body);
        assert_eq!(result.items[0].ext_attrs.needs, Some(Vec::new()));
    }

    /// The 3rd state: no `- needs:` line at all leaves `ext_attrs.needs` as
    /// `None` (falls back to the profile's `default_needs` — a tool-side
    /// concern, not this parser's).
    #[test]
    fn missing_needs_line_leaves_ext_attrs_needs_as_none() {
        let body = "## REQ-012\n\n本文のみ。\n";
        let result = parse(body);
        assert_eq!(result.items[0].ext_attrs.needs, None);
    }

    /// Repeating `- needs:` lines: last-line-wins, same convention as
    /// `layer`/`priority` in `ItemAttrs`.
    #[test]
    fn repeated_needs_line_last_one_wins() {
        let body = "## REQ-013\n\n- needs: acceptance\n- needs: system_test\n\n本文。\n";
        let result = parse(body);
        assert_eq!(
            result.items[0].ext_attrs.needs,
            Some(vec!["system_test".to_string()])
        );
    }

    #[test]
    fn empty_derived_reason_is_dropped_with_warning() {
        let body = "## SPEC-020\n\n- derived:\n\n本文。\n";
        let result = parse(body);
        assert_eq!(result.items[0].ext_attrs.derived, None);
        assert!(result
            .warnings
            .iter()
            .any(|w| w.kind == ParseWarningKind::EmptyReason
                && w.detail.as_deref() == Some("derived")));
    }

    #[test]
    fn empty_waiver_reason_is_dropped_with_warning() {
        let body = "## ST-051\n\n- waive-verify:\n\n本文。\n";
        let result = parse(body);
        assert!(result.items[0].ext_attrs.waivers.is_empty());
        assert!(result
            .warnings
            .iter()
            .any(|w| w.kind == ParseWarningKind::EmptyReason
                && w.detail.as_deref() == Some("waive-verify")));
    }

    // -- M2 acceptance-criteria block (§2.2) --

    #[test]
    fn parses_acceptance_criteria_block_with_labeled_bullets() {
        let body = "## REQ-003 ログイン失敗時のアカウントロック\n\n\
5回連続で認証に失敗したアカウントを15分間ロックする。\n\n\
受入基準:\n\
- AC1: Given 同一アカウントで4回失敗済み When 5回目に失敗する Then アカウントがロックされる\n\
- AC2: WHEN アカウントがロック中 THE SYSTEM SHALL 正しいパスワードでもログインを拒否する\n";
        let result = parse(body);
        let item = &result.items[0];
        assert_eq!(item.acceptance.len(), 2);
        assert_eq!(item.acceptance[0].label, "AC1");
        assert_eq!(item.acceptance[0].kind, "gwt");
        assert_eq!(item.acceptance[1].label, "AC2");
        assert_eq!(item.acceptance[1].kind, "ears");
        assert!(result.warnings.is_empty());
        // The acceptance block is excluded from the def-hash statement...
        assert!(!item.def_hash.is_empty());
        // ...but stays inside the body_hash statement (E14/M1 compat).
        assert!(item.statement.contains("受入基準"));
        assert!(item.statement.contains("AC1"));
    }

    #[test]
    fn unlabeled_acceptance_bullet_gets_positional_label_with_warning() {
        let body = "## REQ-010\n\n本文。\n\n受入基準:\n- 一つ目の条件\n- 二つ目の条件\n";
        let result = parse(body);
        let item = &result.items[0];
        assert_eq!(item.acceptance.len(), 2);
        assert_eq!(item.acceptance[0].label, "AC1");
        assert_eq!(item.acceptance[1].label, "AC2");
        assert_eq!(
            result
                .warnings
                .iter()
                .filter(|w| w.kind == ParseWarningKind::AcLabelPositional)
                .count(),
            2
        );
    }

    #[test]
    fn duplicate_acceptance_label_is_ignored_with_warning() {
        let body = "## REQ-011\n\n本文。\n\n受入基準:\n- AC1: 最初の条件\n- AC1: 二つ目（重複）\n";
        let result = parse(body);
        let item = &result.items[0];
        assert_eq!(item.acceptance.len(), 1);
        assert_eq!(item.acceptance[0].text, "最初の条件");
        assert!(result
            .warnings
            .iter()
            .any(|w| w.kind == ParseWarningKind::AcDuplicateLabel
                && w.detail.as_deref() == Some("AC1")));
    }

    #[test]
    fn acceptance_criteria_trigger_accepts_emphasis_and_english_variants() {
        let body = "## REQ-012\n\n本文。\n\n**Acceptance Criteria:**\n- AC1: 条件\n";
        let result = parse(body);
        assert_eq!(result.items[0].acceptance.len(), 1);
    }

    #[test]
    fn plain_text_acceptance_bullet_is_classified_as_text() {
        let body = "## REQ-013\n\n本文。\n\n受入基準:\n- AC1: 特に構造のない説明文\n";
        let result = parse(body);
        assert_eq!(result.items[0].acceptance[0].kind, "text");
    }

    // -- def_hash / ac_hash (§2.4) --

    #[test]
    fn def_hash_ignores_m1_and_m2_attributes() {
        let a = parse("## REQ-020 タイトル\n\n- priority: P1\n- rationale: 理由A\n\n本文。\n");
        let b = parse("## REQ-020 タイトル\n\n- priority: P2\n- rationale: 理由B\n\n本文。\n");
        assert_eq!(
            a.items[0].def_hash, b.items[0].def_hash,
            "def_hash must not react to attribute-only changes (§2.4)"
        );
    }

    /// M3 (wiki/270-vmodel-m3-design.md, "注意"): `needs` is excluded from
    /// `def_hash`'s input set just like every other M2 attribute key (E14:
    /// `def_hash`/`body_hash` are computed from `ItemAttrs`'s *M1* key set
    /// only — `rationale`/`derived`/`waive-*`/`from`/`assignee`/`needs`
    /// never participate) — changing only `needs` must not change
    /// `def_hash`. (`body_hash` *does* change here, as expected: the
    /// `- needs:` line itself stays in the M1 `statement` text verbatim,
    /// same as `rationale`/`assignee` already do, per
    /// `def_hash_ignores_m1_and_m2_attributes`'s own M1-compat reasoning.)
    #[test]
    fn def_hash_ignores_needs_attribute() {
        let a = parse("## REQ-024 タイトル\n\n- needs: acceptance\n\n本文。\n");
        let b = parse("## REQ-024 タイトル\n\n- needs: acceptance, system_test\n\n本文。\n");
        assert_eq!(
            a.items[0].def_hash, b.items[0].def_hash,
            "def_hash must not react to a needs-only change"
        );
    }

    #[test]
    fn def_hash_changes_when_acceptance_criteria_change() {
        let a = parse("## REQ-021\n\n本文。\n\n受入基準:\n- AC1: 条件A\n");
        let b = parse("## REQ-021\n\n本文。\n\n受入基準:\n- AC1: 条件B\n");
        assert_ne!(a.items[0].def_hash, b.items[0].def_hash);
    }

    #[test]
    fn def_hash_unaffected_by_emphasis_and_whitespace_wrapping() {
        let a = parse("## REQ-022 Title\n\nSome important text here.\n");
        let b = parse("## REQ-022 Title\n\nSome **important**   text\nhere.\n");
        assert_eq!(
            a.items[0].def_hash, b.items[0].def_hash,
            "markdown emphasis/wrapping must not affect def_hash (§2.4)"
        );
    }

    #[test]
    fn ac_hash_changes_only_for_its_own_ac_when_a_sibling_ac_changes() {
        let a = parse("## REQ-023\n\n本文。\n\n受入基準:\n- AC1: 条件1\n- AC2: 条件2\n");
        let b = parse("## REQ-023\n\n本文。\n\n受入基準:\n- AC1: 条件1\n- AC2: 条件2変更\n");
        assert_eq!(
            a.items[0].acceptance[0].ac_hash, b.items[0].acceptance[0].ac_hash,
            "editing AC2 must not change AC1's ac_hash"
        );
        assert_ne!(
            a.items[0].acceptance[1].ac_hash,
            b.items[0].acceptance[1].ac_hash
        );
    }

    #[test]
    fn ac_hash_changes_when_the_items_own_statement_changes() {
        let a = parse("## REQ-024\n\n本文A。\n\n受入基準:\n- AC1: 条件1\n");
        let b = parse("## REQ-024\n\n本文B変更。\n\n受入基準:\n- AC1: 条件1\n");
        assert_ne!(
            a.items[0].acceptance[0].ac_hash, b.items[0].acceptance[0].ac_hash,
            "editing the item's own body (outside acceptance) must invalidate every AC-unit link (FR-401)"
        );
    }

    // -- body_hash compat (E14, M1 key set only) --

    /// wiki/260-vmodel-m2-design.md §2.2/E14: an M1 fixture that happened to
    /// have literal `- rationale:` / `- assignee:` / `- derived:` lines
    /// (unrecognized by M1, so they stayed in the M1 `statement`) must hash
    /// identically once parsed by the M2 parser — M2 recognizing those keys
    /// now must never change `body_hash`, or every pre-existing `trace_record`
    /// run for such an item becomes spuriously suspect.
    #[test]
    fn body_hash_unchanged_for_m1_fixture_with_now_recognized_attribute_lines() {
        let body = "## REQ-030 Title\n\n\
- priority: P1\n\
- rationale: 総当たり攻撃の抑止\n\
- assignee: alice\n\
- derived: 実装方式から必要になった項目\n\n\
本文テキスト。\n";
        // The M1-era hash: computed the exact way M1's `compute_body_hash`
        // always has — title + (M1-attribute-lines-only-stripped) statement
        // + the M1 attrs repr. `rationale`/`assignee`/`derived` were *not*
        // recognized keys under M1, so they remained part of the M1
        // statement verbatim.
        let m1_statement = "- rationale: 総当たり攻撃の抑止\n- assignee: alice\n- derived: 実装方式から必要になった項目\n\n本文テキスト。";
        let m1_attrs = ItemAttrs {
            priority: Some("P1".to_string()),
            ..Default::default()
        };
        let expected_m1_hash = compute_body_hash("Title", m1_statement, &m1_attrs);

        let result = parse(body);
        assert_eq!(
            result.items[0].body_hash, expected_m1_hash,
            "body_hash must equal the M1-era hash even though M2 now recognizes \
             rationale/assignee/derived as attribute keys"
        );
    }

    #[test]
    fn verifies_with_ac_subreference_is_preserved_as_authored() {
        let body = "## ST-051\n\n- verifies: REQ-003#AC2\n\n本文。\n";
        let result = parse(body);
        assert_eq!(result.items[0].attrs.verifies, vec!["REQ-003#AC2"]);
    }
}
