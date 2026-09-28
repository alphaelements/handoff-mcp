//! Layer-document body **rendering** (wiki/260-vmodel-m2-design.md §4.7/§4.8,
//! M2-12): the inverse of [`super::layer_parse::parse_layer_body`] — turns
//! one item's id/title/known attributes/statement into the §2.2 Markdown
//! notation, so that parsing the rendered text back reproduces the same
//! item (§4.7: "描画 → 解析の往復で同じ項目になる").
//!
//! A **pure function module**, like `layer_parse` itself: no file I/O, no
//! knowledge of `DocMetadata`/`SubItem`. Callers own reading the target
//! document, appending the rendered text, and running it through the real
//! parser/layer-sync (`handle_doc_save`'s `append_body`, currently the only
//! production write path — `handoff_trace_scaffold`, M2-12). A second
//! caller (`handoff_trace_update`'s `upsert_item` op, §4.8) is M2-14's
//! scope; this module's [`ItemRenderAttrs`] already covers the full known
//! attribute-key set (M1 + M2) so that op does not need a second renderer.
//!
//! **Attribute key order** (deliberately fixed, so every rendered item is
//! byte-identical for the same input): `layer`, `refines`, `verifies`,
//! `priority`, `from`, `method`, `test` (repeated), `rationale`, `derived`,
//! `waive-verify`/`waive-refine` (repeated, in `waivers`' own order),
//! `assignee`, `needs`. This is not dictated by §2.2/§4.7/§4.8 (the parser
//! accepts attribute lines in any order) — it is chosen to match §4.7's own
//! worked example (`verifies` / `from` / `method`, in that order) for the
//! common `trace_scaffold`-generated item shape, while still covering the
//! rest of the M1+M2 key set for `trace_update`'s future general-purpose use.

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
        assert_eq!(
            item.ext_attrs.reserved.get("assignee").map(String::as_str),
            Some("alice")
        );
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
    fn render_item_clamps_out_of_range_heading_levels() {
        let rendered = render_item(0, "REQ-001", "Title", &ItemRenderAttrs::default(), "body");
        assert!(rendered.starts_with("# REQ-001 Title\n"));
        let rendered = render_item(9, "REQ-001", "Title", &ItemRenderAttrs::default(), "body");
        assert!(rendered.starts_with("###### REQ-001 Title\n"));
    }
}
