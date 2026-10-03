//! `handoff_doc_repair_frontmatter` — reports and (optionally) fixes
//! documents whose frontmatter fails to parse (FR-804, E11,
//! wiki/260-vmodel-m2-design.md §4.12/§12 M2-18), plus documents that parse
//! fine but are missing `created_at`/`updated_at` (t377.1).
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
//!
//! t377.1: a second, separate real-world shape has the opposite problem — 55
//! of aelm's documents parse *successfully* (frontmatter's `created_at`/
//! `updated_at` now default to `""` rather than hard-failing the whole
//! document, same task) but have no real timestamp on disk at all.
//! `read_doc`/`read_all_docs` paper over this in-memory with an mtime
//! fallback so every *reader* still sees a trustworthy value, but the raw
//! `.md` file itself still has no `created_at:`/`updated_at:` key — this
//! tool's second pass detects that raw-missing state directly (by
//! re-parsing the *unmodified* on-disk YAML, which never applies the
//! in-memory fallback) and, on `dry_run: false`, persists the same
//! mtime-derived value permanently so the fallback only ever has to run
//! once per document.

use anyhow::Result;
use serde_json::{json, Value};

use super::HandlerContext;
use crate::storage::docs::frontmatter::{
    deserialize_frontmatter, read_raw_frontmatter_and_body, repair_known_nonstandard_yaml,
};
use crate::storage::docs::{
    backfill_missing_timestamps_from_mtime, doc_body_path, read_all_docs_with_unreadable,
    write_doc_with_body,
};

fn to_json(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

/// `handoff_doc_repair_frontmatter(slug?, dry_run? = true)` — scans every
/// document `handoff_doc_list`'s `unreadable` would report (optionally
/// narrowed to one `slug`) and, for each, attempts the known-shape repair
/// (§4.12), then scans every successfully-parsed document for a missing
/// `created_at`/`updated_at` (t377.1) and backfills it from the file's own
/// mtime. `dry_run` (default `true`) reports what would change without
/// writing anything; `dry_run: false` applies the fix via the same
/// canonical writer every other document mutation uses.
pub fn handle(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;
    let slug_filter = arguments.get("slug").and_then(|v| v.as_str());
    let dry_run = arguments
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let (docs, unreadable) = read_all_docs_with_unreadable(handoff)?;

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

    // t377.1: documents that parsed fine above (not in `unreadable` at all)
    // may still be missing `created_at`/`updated_at` on disk. Re-parse each
    // one's *raw* YAML directly (bypassing the in-memory mtime fallback
    // `read_all_docs_with_unreadable` already applied to `docs`) so this
    // check reflects what's actually on disk, not what readers already see.
    for doc in &docs {
        if let Some(filter) = slug_filter {
            if doc.slug != filter {
                continue;
            }
        }

        let path = doc_body_path(handoff, &doc.slug);
        let Some((raw_yaml, body)) = read_raw_frontmatter_and_body(&path)? else {
            continue;
        };
        let Ok(mut raw_doc) = deserialize_frontmatter(&raw_yaml, &doc.slug) else {
            continue;
        };
        if raw_doc.created_at.is_empty() || raw_doc.updated_at.is_empty() {
            let missing_created_at = raw_doc.created_at.is_empty();
            let missing_updated_at = raw_doc.updated_at.is_empty();
            backfill_missing_timestamps_from_mtime(&mut raw_doc, &path);
            if !dry_run {
                write_doc_with_body(handoff, &raw_doc, &body)?;
            }
            let mut fixes = Vec::new();
            if missing_created_at {
                fixes.push(format!(
                    "backfilled missing created_at from file mtime ({})",
                    raw_doc.created_at
                ));
            }
            if missing_updated_at {
                fixes.push(format!(
                    "backfilled missing updated_at from file mtime ({})",
                    raw_doc.updated_at
                ));
            }
            repaired.push(json!({
                "slug": doc.slug,
                "applied": !dry_run,
                "fixes": fixes,
            }));
        }
    }

    Ok(to_json(&json!({
        "dry_run": dry_run,
        "repaired": repaired,
        "unrepaired": unrepaired,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::docs::{docs_dir, DocMetadata};
    use crate::storage::docs::{read_doc, write_doc};
    use serde_json::json;
    use tempfile::TempDir;

    fn ctx(handoff: std::path::PathBuf) -> HandlerContext {
        HandlerContext {
            agent_id: None,
            project_dir: handoff.parent().unwrap().to_path_buf(),
            handoff_dir: handoff,
        }
    }

    fn setup() -> (TempDir, std::path::PathBuf) {
        let tmp = TempDir::new().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();
        (tmp, handoff)
    }

    /// t377.1: a document missing `created_at`/`updated_at` entirely (the
    /// real aelm shape) is reported as repairable in `dry_run` (default)
    /// without writing anything, then actually backfilled on disk when
    /// `dry_run: false` is passed explicitly.
    #[test]
    fn repairs_missing_created_at_from_mtime_only_when_dry_run_is_false() {
        let (_tmp, handoff) = setup();
        std::fs::create_dir_all(docs_dir(&handoff)).unwrap();
        let path = docs_dir(&handoff).join("_doc.no-timestamps.md");
        std::fs::write(
            &path,
            "---\nid: doc-1\ntitle: T\ndoc_type: spec\n---\nbody\n",
        )
        .unwrap();
        let c = ctx(handoff.clone());

        // dry_run (default): reports the fix but the file is untouched.
        let out: Value = serde_json::from_str(&handle(&c, &json!({})).unwrap()).unwrap();
        assert_eq!(out["dry_run"], true);
        let repaired = out["repaired"].as_array().unwrap();
        assert_eq!(repaired.len(), 1);
        assert_eq!(repaired[0]["slug"], "no-timestamps");
        assert_eq!(repaired[0]["applied"], false);

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            !raw.contains("created_at:"),
            "dry_run must not write anything: {raw}"
        );

        // dry_run: false actually persists the backfilled timestamp.
        let out: Value =
            serde_json::from_str(&handle(&c, &json!({ "dry_run": false })).unwrap()).unwrap();
        assert_eq!(out["repaired"].as_array().unwrap()[0]["applied"], true);

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains("created_at:"),
            "created_at must be persisted after dry_run=false: {raw}"
        );

        let doc = read_doc(&handoff, "no-timestamps").unwrap().unwrap();
        assert!(!doc.created_at.is_empty());
        assert!(!doc.updated_at.is_empty());
    }

    /// A document that already has both timestamps is left untouched —
    /// nothing to repair, and `write_doc_with_body` must not even be
    /// called for it (which would bump `updated_at`/rewrite the file for
    /// no reason).
    #[test]
    fn leaves_documents_with_both_timestamps_untouched() {
        let (_tmp, handoff) = setup();
        let doc = DocMetadata::new(
            "doc-1".to_string(),
            "has-timestamps".to_string(),
            "T".to_string(),
            "spec".to_string(),
            "2026-01-01T00:00:00Z".to_string(),
        );
        write_doc(&handoff, &doc).unwrap();
        let path = doc_body_path(&handoff, "has-timestamps");
        let before = std::fs::read_to_string(&path).unwrap();

        let c = ctx(handoff);
        let out: Value =
            serde_json::from_str(&handle(&c, &json!({ "dry_run": false })).unwrap()).unwrap();
        assert_eq!(out["repaired"].as_array().unwrap().len(), 0);

        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(before, after, "untouched document must not be rewritten");
    }
}
