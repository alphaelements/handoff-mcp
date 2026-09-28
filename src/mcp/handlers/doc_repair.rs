//! `handoff_doc_repair_frontmatter` — reports and (optionally) fixes
//! documents whose frontmatter fails to parse (FR-804, E11,
//! wiki/260-vmodel-m2-design.md §4.12/§12 M2-18).
//!
//! Real-world motivation (§12 M2-18, E11): 9 of 209 documents in aelm's
//! `.handoff/docs/` have a `scope_paths:` key immediately followed by a lone
//! `[]` line at the same indentation — non-standard YAML that both
//! `serde_yaml` and PyYAML fail to parse. `read_all_docs`/`DocSet::load`
//! silently skipped such documents before this task (E11: "読めない文書を
//! 黙って捨てない"); `handoff_doc_list`'s `unreadable` field now reports
//! them (`crate::storage::docs::read_all_docs_with_unreadable`), and this
//! tool attempts to fix the specific known non-standard shapes
//! (`frontmatter::repair_known_nonstandard_yaml`) and rewrite the document
//! through the same canonical writer (`write_doc_with_body`) every other
//! `doc_save`-style mutation uses — an unrecognized malformation is reported
//! as still-unrepaired rather than guessed at.

use anyhow::Result;
use serde_json::{json, Value};

use super::HandlerContext;
use crate::storage::docs::frontmatter::{
    deserialize_frontmatter, read_raw_frontmatter_and_body, repair_known_nonstandard_yaml,
};
use crate::storage::docs::{doc_body_path, read_all_docs_with_unreadable, write_doc_with_body};

fn to_json(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

/// `handoff_doc_repair_frontmatter(slug?, dry_run? = true)` — scans every
/// document `handoff_doc_list`'s `unreadable` would report (optionally
/// narrowed to one `slug`) and, for each, attempts the known-shape repair
/// (§4.12). `dry_run` (default `true`) reports what would change without
/// writing anything; `dry_run: false` applies the fix via the same
/// canonical writer every other document mutation uses.
pub fn handle(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;
    let slug_filter = arguments.get("slug").and_then(|v| v.as_str());
    let dry_run = arguments
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let (_, unreadable) = read_all_docs_with_unreadable(handoff)?;

    let mut repaired = Vec::new();
    let mut unrepaired = Vec::new();

    for entry in unreadable {
        if let Some(filter) = slug_filter {
            if entry.slug != filter {
                continue;
            }
        }

        let path = doc_body_path(handoff, &entry.slug);
        let Some((raw_yaml, body)) = read_raw_frontmatter_and_body(&path)? else {
            // File disappeared, or lost its fence, between the scan above
            // and this read (e.g. a concurrent writer) — report rather than
            // panic; the next scan will reflect whatever is true now.
            unrepaired.push(json!({
                "slug": entry.slug,
                "error": "document no longer has a readable frontmatter fence",
            }));
            continue;
        };

        let Some((repaired_yaml, fixes)) = repair_known_nonstandard_yaml(&raw_yaml) else {
            unrepaired.push(json!({ "slug": entry.slug, "error": entry.error }));
            continue;
        };

        match deserialize_frontmatter(&repaired_yaml, &entry.slug) {
            Ok(doc) => {
                if !dry_run {
                    write_doc_with_body(handoff, &doc, &body)?;
                }
                repaired.push(json!({
                    "slug": entry.slug,
                    "applied": !dry_run,
                    "fixes": fixes.into_iter().map(|f| f.description).collect::<Vec<_>>(),
                }));
            }
            Err(e) => {
                // The known-shape normalization applied but the document
                // still doesn't parse (an unrelated, unrecognized error
                // remains) — report the failure that survived, not the
                // original one, so the caller sees what still needs fixing.
                unrepaired.push(json!({
                    "slug": entry.slug,
                    "error": format!("{e:#}"),
                }));
            }
        }
    }

    Ok(to_json(&json!({
        "dry_run": dry_run,
        "repaired": repaired,
        "unrepaired": unrepaired,
    })))
}
