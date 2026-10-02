//! Layer-document body **rendering** (wiki/260-vmodel-m2-design.md §4.7/§4.8,
//! M2-12): the inverse of [`super::layer_parse::parse_layer_body`] — turns
//! one item's id/title/known attributes/statement into the §2.2 Markdown
//! notation, so that parsing the rendered text back reproduces the same
//! item (§4.7: "描画 → 解析の往復で同じ項目になる").
//!
//! A **pure function module**, like `layer_parse` itself: no file I/O, no
//! knowledge of `DocMetadata`/`SubItem`. Callers own reading the target
//! document, splicing in the rendered text, and running it through the real
//! parser/layer-sync. Two production write paths share this module:
//! `handle_doc_save`'s `append_body` (`handoff_trace_scaffold`, M2-12 —
//! always a pure append) and `handoff_trace_update`'s `upsert_item` op
//! (§4.8, M2-14 — a line-range splice for an existing item, or an insertion
//! at a given anchor/doc-end for a new one, via `write_doc_body` directly).
//! This module's [`ItemRenderAttrs`] already covers the full known
//! attribute-key set (M1 + M2), so neither caller needs its own renderer.
//!
//! **Attribute key order** (deliberately fixed, so every rendered item is
//! byte-identical for the same input): `layer`, `refines`, `verifies`,
//! `priority`, `from`, `method`, `test` (repeated), `rationale`, `derived`,
//! `waive-verify`/`waive-refine` (repeated, in `waivers`' own order),
//! `assignee`, `needs`. This is not dictated by §2.2/§4.7/§4.8 (the parser
//! accepts attribute lines in any order) — it is chosen to match §4.7's own
//! worked example (`verifies` / `from` / `method`, in that order) for the
//! common `trace_scaffold`-generated item shape, while still covering the
//! rest of the M1+M2 key set for `trace_update`'s general-purpose use.

use std::collections::BTreeMap;

use super::model::Waiver;

/// One item's renderable attribute set — the union of
/// [`super::layer_parse::ItemAttrs`] (M1) and
/// [`super::layer_parse::ExtAttrs`] (M2), minus the parser's
/// warning/positional bookkeeping (rendering never needs to reproduce a
/// parse warning). Every field empty/`None` renders no attribute block at
/// all (an item can be attribute-free, same as the parser accepts).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemRenderAttrs {
    pub layer: Option<String>,
    pub refines: Vec<String>,
    pub verifies: Vec<String>,
    pub priority: Option<String>,
    /// `- from: <id>` (§2.2's `from` attribute — the scaffold-generation
    /// source, e.g. `REQ-003#AC1`).
    pub from: Option<String>,
    pub method: Option<String>,
    /// Raw `path::name` values, one `- test: <value>` line per entry.
    pub test_refs: Vec<String>,
    pub rationale: Option<String>,
    pub derived: Option<String>,
    /// One `- waive-verify: <reason>` / `- waive-refine: <reason>` line per
    /// entry, in order.
    pub waivers: Vec<Waiver>,
    /// Reserved `assignee`/`needs` keys, rendered in `BTreeMap` (sorted)
    /// order — `assignee` sorts before `needs` alphabetically, matching the
    /// order those two keys are always discussed in (§2.2).
    pub reserved: BTreeMap<String, String>,
}

/// Renders one item's known-attribute bullet block (§2.2's "見出し直後の最初の
/// 箇条書きブロック"), in the fixed order documented on this module. Returns
/// an empty string when every field is empty — callers must not emit a
/// blank attribute block for an attribute-free item.
pub fn render_attrs_block(attrs: &ItemRenderAttrs) -> String {
    let mut out = String::new();
    if let Some(layer) = &attrs.layer {
        out.push_str(&format!("- layer: {layer}\n"));
    }
    if !attrs.refines.is_empty() {
        out.push_str(&format!("- refines: {}\n", attrs.refines.join(", ")));
    }
    if !attrs.verifies.is_empty() {
        out.push_str(&format!("- verifies: {}\n", attrs.verifies.join(", ")));
    }
    if let Some(priority) = &attrs.priority {
        out.push_str(&format!("- priority: {priority}\n"));
    }
    if let Some(from) = &attrs.from {
        out.push_str(&format!("- from: {from}\n"));
    }
    if let Some(method) = &attrs.method {
        out.push_str(&format!("- method: {method}\n"));
    }
    for test in &attrs.test_refs {
        out.push_str(&format!("- test: {test}\n"));
    }
    if let Some(rationale) = &attrs.rationale {
        out.push_str(&format!("- rationale: {rationale}\n"));
    }
    if let Some(derived) = &attrs.derived {
        out.push_str(&format!("- derived: {derived}\n"));
    }
    for waiver in &attrs.waivers {
        let key = if waiver.axis == "verify" {
            "waive-verify"
        } else {
            "waive-refine"
        };
        out.push_str(&format!("- {key}: {}\n", waiver.reason));
    }
    for (key, value) in &attrs.reserved {
        out.push_str(&format!("- {key}: {value}\n"));
    }
    out
}

/// §2.2's acceptance-criteria-block trigger line test, duplicated from
/// `layer_parse::is_ac_trigger_line` (private to that module) rather than
/// widening its visibility for this module's single caller — the same
/// small-helper-duplication precedent `trace_propose.rs`/`trace_scaffold.rs`
/// already use for a parser-adjacent one-off (see those modules' own doc
/// comments).
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

fn is_bullet_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ")
}

/// Strips a trailing acceptance-criteria block (§2.2) from `statement`, the
/// inverse half of what `render_item` assembles when a caller renders
/// `statement` + a separately-rendered acceptance block back-to-back.
/// `handoff_trace_update`'s `upsert_item` op (M2-14, §4.8) uses this to
/// recover an existing item's body text *before* its acceptance block when
/// an op updates `title`/`attrs` but leaves `acceptance` untouched (so the
/// existing block is carried over verbatim rather than silently dropped).
/// Returns `statement` trimmed of trailing whitespace, unchanged, when no
/// acceptance-criteria block is found (mirrors `layer_parse::parse_layer_body`'s
/// own trigger-line detection, see [`is_ac_trigger_line`]/[`is_bullet_line`]
/// above).
pub fn strip_trailing_acceptance_block(statement: &str) -> String {
    let lines: Vec<&str> = statement.lines().collect();
    for i in 0..lines.len() {
        if lines[i].trim().is_empty() || !is_ac_trigger_line(lines[i]) {
            continue;
        }
        let mut j = i + 1;
        while j < lines.len() && lines[j].trim().is_empty() {
            j += 1;
        }
        if j < lines.len() && is_bullet_line(lines[j]) {
            return lines[..i].join("\n").trim_end().to_string();
        }
    }
    statement.trim_end().to_string()
}

/// Renders a §2.2 acceptance-criteria block (`受入基準:` + one `- <label>:
/// <text>` bullet per entry, in the given order) — the inverse of
/// `layer_parse::parse_layer_body`'s acceptance-bullet extraction. Returns an
/// empty string for an empty `items` (no block at all, same "don't emit an
/// empty section" convention as [`render_attrs_block`]). Always uses the
/// Japanese heading (`受入基準:`) — §2.2's own three recognized spellings
/// are only a *reading* convenience; every worked example in §2.2/§4.7 that
/// shows the block being authored uses this one, so it is the only one this
/// renderer needs to produce (an existing English-heading block an
/// `upsert_item` op leaves untouched is preserved verbatim by
/// [`strip_trailing_acceptance_block`] never touching it in the first
/// place — this function only runs when the op's `acceptance` field itself
/// is provided, i.e. a deliberate rewrite).
pub fn render_acceptance_block(items: &[(String, String)]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let mut out = String::from("受入基準:\n");
    for (label, text) in items {
        out.push_str(&format!("- {label}: {text}\n"));
    }
    out
}

/// Renders one full item block: an ATX heading (`level` `#`s, `id`, a space,
/// `title`), a blank line, the attribute block (if any) followed by a blank
/// line, then `statement` (trimmed, one trailing newline). `level` is
/// 1-6 (ATX heading depth); callers are responsible for picking a level
/// consistent with the target document's existing items (`layer_parse`
/// itself does not care — item recognition is driven by the id grammar, not
/// heading depth).
pub fn render_item(
    level: u8,
    id: &str,
    title: &str,
    attrs: &ItemRenderAttrs,
    statement: &str,
) -> String {
    let level = level.clamp(1, 6) as usize;
    let mut out = String::new();
    out.push_str(&"#".repeat(level));
    out.push(' ');
    out.push_str(id);
    if !title.is_empty() {
        out.push(' ');
        out.push_str(title);
    }
    out.push('\n');
    out.push('\n');

    let attrs_block = render_attrs_block(attrs);
    if !attrs_block.is_empty() {
        out.push_str(&attrs_block);
        out.push('\n');
    }

    out.push_str(statement.trim());
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::docs::layer::LayerRegistry;
    use crate::storage::docs::layer_parse::{default_prefix_table, parse_layer_body};
    use std::collections::HashMap;

    fn prefix_table() -> HashMap<String, Vec<String>> {
        let registry = LayerRegistry::build(&[]);
        default_prefix_table(&registry, &HashMap::new())
    }

    #[test]
    fn render_attrs_block_matches_the_spec_example_order() {
        let attrs = ItemRenderAttrs {
            verifies: vec!["REQ-003".to_string()],
            from: Some("REQ-003#AC1".to_string()),
            method: Some("manual".to_string()),
            ..Default::default()
        };
        assert_eq!(
            render_attrs_block(&attrs),
            "- verifies: REQ-003\n- from: REQ-003#AC1\n- method: manual\n"
        );
    }

    #[test]
    fn render_attrs_block_is_empty_for_a_default_attrs() {
        assert_eq!(render_attrs_block(&ItemRenderAttrs::default()), "");
    }

    #[test]
    fn render_item_produces_the_spec_shape() {
        let attrs = ItemRenderAttrs {
            verifies: vec!["REQ-003".to_string()],
            from: Some("REQ-003#AC1".to_string()),
            method: Some("manual".to_string()),
            ..Default::default()
        };
        let rendered = render_item(
            3,
            "AT-REQ-003-1",
            "5回目の失敗でロックされる",
            &attrs,
            "手順: 同一アカウントで4回失敗済み. 5回目に失敗する\n期待結果: アカウントがロックされる",
        );
        assert_eq!(
            rendered,
            "### AT-REQ-003-1 5回目の失敗でロックされる\n\n\
             - verifies: REQ-003\n- from: REQ-003#AC1\n- method: manual\n\n\
             手順: 同一アカウントで4回失敗済み. 5回目に失敗する\n期待結果: アカウントがロックされる\n"
        );
    }

    /// §4.7's own round-trip requirement: rendering an item then parsing it
    /// back must reproduce the same id/title/attrs/statement.
    #[test]
    fn render_then_parse_round_trips_verifies_from_method() {
        let attrs = ItemRenderAttrs {
            verifies: vec!["REQ-003".to_string()],
            from: Some("REQ-003#AC1".to_string()),
            method: Some("manual".to_string()),
            ..Default::default()
        };
        let statement = "手順: (記入)\n期待結果: アカウントがロックされる";
        let rendered = render_item(3, "AT-REQ-003-1", "Locked", &attrs, statement);
        let body = format!("# Doc\n\n{rendered}");

        let parsed = parse_layer_body(&body, Some("acceptance"), &prefix_table());
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
        assert_eq!(parsed.items.len(), 1);
        let item = &parsed.items[0];
        assert_eq!(item.id, "AT-REQ-003-1");
        assert_eq!(item.title, "Locked");
        assert_eq!(item.attrs.verifies, vec!["REQ-003".to_string()]);
        assert_eq!(item.attrs.method.as_deref(), Some("manual"));
        assert_eq!(item.ext_attrs.from.as_deref(), Some("REQ-003#AC1"));
        // E14 (wiki/260 §2.2): `ParsedItem::statement` is the *M1-only*
        // pass — it strips just the M1 key set (refines/verifies/layer/
        // priority/method/test), so an M2-only attribute line like `- from:
        // ...` is left in place as ordinary body text (feeds `body_hash`,
        // not `def_hash`). This is the documented M1 compat behavior, not a
        // round-trip defect — the M2 attribute itself still round-trips
        // correctly via `ext_attrs.from` (asserted above).
        assert_eq!(
            item.statement,
            format!("- from: REQ-003#AC1\n\n{statement}")
        );
    }

    /// Round-trip for the full attribute set (M1 + M2), rendered in this
    /// module's fixed order.
    #[test]
    fn render_then_parse_round_trips_every_known_attribute() {
        let mut reserved = BTreeMap::new();
        reserved.insert("assignee".to_string(), "alice".to_string());
        reserved.insert("needs".to_string(), "REQ-001".to_string());
        let attrs = ItemRenderAttrs {
            layer: Some("basic_spec".to_string()),
            refines: vec!["REQ-001".to_string(), "REQ-002".to_string()],
            verifies: vec!["ST-001".to_string()],
            priority: Some("P1".to_string()),
            from: Some("REQ-001#AC1".to_string()),
            method: Some("auto".to_string()),
            test_refs: vec!["mod::case_a".to_string(), "mod::case_b".to_string()],
            rationale: Some("audit finding".to_string()),
            derived: Some("no upstream requirement yet".to_string()),
            waivers: vec![
                Waiver {
                    axis: "verify".to_string(),
                    reason: "reviewed manually".to_string(),
                },
                Waiver {
                    axis: "refine".to_string(),
                    reason: "spike only".to_string(),
                },
            ],
            reserved,
        };
        let statement = "Statement body text.";
        let rendered = render_item(3, "SPEC-100", "Full attrs", &attrs, statement);
        let body = format!("# Doc\n\n{rendered}");

        let parsed = parse_layer_body(&body, None, &prefix_table());
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
        let item = &parsed.items[0];
        assert_eq!(item.attrs.layer.as_deref(), Some("basic_spec"));
        assert_eq!(
            item.attrs.refines,
            vec!["REQ-001".to_string(), "REQ-002".to_string()]
        );
        assert_eq!(item.attrs.verifies, vec!["ST-001".to_string()]);
        assert_eq!(item.attrs.priority.as_deref(), Some("P1"));
        assert_eq!(item.attrs.method.as_deref(), Some("auto"));
        assert_eq!(
            item.attrs.test_refs,
            vec!["mod::case_a".to_string(), "mod::case_b".to_string()]
        );
        assert_eq!(item.ext_attrs.from.as_deref(), Some("REQ-001#AC1"));
        assert_eq!(item.ext_attrs.rationale.as_deref(), Some("audit finding"));
        assert_eq!(
            item.ext_attrs.derived.as_deref(),
            Some("no upstream requirement yet")
        );
        assert_eq!(item.ext_attrs.waivers.len(), 2);
        assert_eq!(item.ext_attrs.waivers[0].axis, "verify");
        assert_eq!(item.ext_attrs.waivers[1].axis, "refine");
        // M3 (wiki/270-vmodel-m3-design.md §2.2, FR-307): `assignee` parses
        // into its own `ExtAttrs` field now, not `reserved` — this renderer
        // module's own `ItemRenderAttrs.reserved` is unchanged (it is a
        // generic key/value rendering input, not tied to the parser's
        // `ExtAttrs` shape), so feeding it an `"assignee"` entry still
        // renders the same `- assignee: alice` line; only the parse-side
        // assertion target moves.
        assert_eq!(item.ext_attrs.assignee.as_deref(), Some("alice"));
        assert_eq!(
            item.ext_attrs.reserved.get("needs").map(String::as_str),
            Some("REQ-001")
        );
        // Same E14 M1-statement note as the test above: every M2-only
        // attribute line (from/rationale/derived/waive-*/reserved) stays in
        // `statement` verbatim, in this module's rendered order.
        assert_eq!(
            item.statement,
            format!(
                "- from: REQ-001#AC1\n\
                 - rationale: audit finding\n\
                 - derived: no upstream requirement yet\n\
                 - waive-verify: reviewed manually\n\
                 - waive-refine: spike only\n\
                 - assignee: alice\n\
                 - needs: REQ-001\n\n{statement}"
            )
        );
    }

    #[test]
    fn strip_trailing_acceptance_block_removes_the_block_and_trims() {
        let statement = "Statement text.\n\n受入基準:\n- AC1: one\n- AC2: two\n";
        assert_eq!(
            strip_trailing_acceptance_block(statement),
            "Statement text."
        );
    }

    #[test]
    fn strip_trailing_acceptance_block_is_a_no_op_without_a_block() {
        let statement = "Just a plain statement.\nSecond line.";
        assert_eq!(
            strip_trailing_acceptance_block(statement),
            "Just a plain statement.\nSecond line."
        );
    }

    #[test]
    fn strip_trailing_acceptance_block_recognizes_english_heading() {
        let statement = "Body.\n\nAcceptance Criteria:\n- AC1: one\n";
        assert_eq!(strip_trailing_acceptance_block(statement), "Body.");
    }

    #[test]
    fn render_acceptance_block_renders_labeled_bullets_in_order() {
        let items = vec![
            ("AC1".to_string(), "first condition".to_string()),
            ("AC2".to_string(), "second condition".to_string()),
        ];
        assert_eq!(
            render_acceptance_block(&items),
            "受入基準:\n- AC1: first condition\n- AC2: second condition\n"
        );
    }

    #[test]
    fn render_acceptance_block_is_empty_for_no_items() {
        assert_eq!(render_acceptance_block(&[]), "");
    }

    /// Round-trip: rendering a statement + acceptance block, then stripping
    /// the acceptance block back off, reproduces the original (trimmed)
    /// statement — the two halves `upsert_item` composes a full item body
    /// from must invert each other.
    #[test]
    fn render_acceptance_then_strip_round_trips_the_statement() {
        let items = vec![("AC1".to_string(), "Given a When b Then c".to_string())];
        let block = render_acceptance_block(&items);
        let combined = format!("Body text.\n\n{block}");
        assert_eq!(strip_trailing_acceptance_block(&combined), "Body text.");
    }

    #[test]
    fn render_item_clamps_out_of_range_heading_levels() {
        let rendered = render_item(0, "REQ-001", "Title", &ItemRenderAttrs::default(), "body");
        assert!(rendered.starts_with("# REQ-001 Title\n"));
        let rendered = render_item(9, "REQ-001", "Title", &ItemRenderAttrs::default(), "body");
        assert!(rendered.starts_with("###### REQ-001 Title\n"));
    }
}
