//! MCP handlers for document management (save / get / list) — P1-6a (t96.1),
//! frontmatter migration (t123.1-t123.3, single-file slug-based storage,
//! wiki/130-document-management.md §3.1).
//!
//! Builds on the storage layer in `crate::storage::docs` (on-demand section
//! computation + slug-named `_doc.<slug>.md` frontmatter+body I/O) and the
//! task<->doc bidirectional link sync in
//! `crate::storage::tasks::sync_doc_task_links`. See
//! `wiki/130-document-management.md` §5.1-§5.3 for the spec.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::HandlerContext;
use crate::context::injection::{rank_by_bm25_and_scope, RankConfig};
use crate::storage::config::read_config;
use crate::storage::docs::layer_sync::sync_layer_items;
use crate::storage::docs::reassemble::extract_section;
use crate::storage::docs::split::{compute_sections, split};
use crate::storage::docs::{
    delete_doc, delete_doc_body, docs_dir, ensure_docs_dir, find_doc_by_id, read_all_docs,
    read_doc, read_doc_body, read_doc_hashed, read_doc_with_body_hashed, validate_slug, write_doc,
    write_doc_body, write_doc_with_body, CodeRef, DocMetadata, DocRelation, DocSet, SubItem,
    Verification, VerificationItem,
};
use crate::storage::tasks::{
    find_task_dir_by_id, read_modify_write_task, read_task, sync_doc_task_links, TaskLink,
};

/// Runs [`sync_layer_items`] against `doc` when it is a layer document
/// (`doc.layer.is_some()`) and its body has actually changed since the last
/// sync — wiki/220-vmodel-integration-design.md §2.4's timing rule ("実行
/// タイミング: doc_save (...) と doc_update_section の最後") plus its
/// performance note (wiki/240-performance-design.md §5-3): a metadata-only
/// `doc_save` whose body is byte-identical to what was last synced (the
/// common case for `set_dev_stage`-style callers that never touch the body)
/// must not pay `parse_layer_body`'s cost again. `source.body_raw_hash`
/// (cheap FNV-1a of the raw bytes, **not** `lexsim::content_hash`) is the
/// comparison key; a document with no recorded `body_raw_hash` yet (never
/// synced, or saved by a pre-t360.6 binary) is always treated as changed —
/// "1回同期して保存" per the spec — never silently skipped.
///
/// No-op for non-layer documents (`sync_layer_items` itself no-ops on
/// `doc.layer.is_none()`, so this wrapper only exists to add the raw-hash
/// short-circuit and read `[trace.id_prefixes]` config on the caller's
/// behalf).
///
/// `structural_change` (rework round 2, MAJOR fix): `sync_layer_items`'s
/// output also depends on `doc.layer` (decides each item's `category`, §2.3)
/// and on `doc.sections` (decided by `split_level` — which section a body
/// item lands under). A metadata-only `doc_save` that changes only `layer`
/// or `split_level`, with the body's raw bytes untouched, would otherwise
/// pass the `body_raw_hash` short-circuit below and silently keep a stale
/// matrix. The caller passes `true` here whenever either of those two
/// document-level inputs actually changed on this call, bypassing the
/// short-circuit regardless of what `body_raw_hash` says.
///
/// Returns whether a sync actually ran (`false` when this is a no-op for a
/// non-layer document, or the short-circuit above applied) — callers use
/// this to gate the (comparatively expensive) post-sync corpus-wide
/// collision check and `_requirements_summary.json` refresh below on an
/// actual resync having happened, not on every `doc_save`/`doc_update_section`
/// call.
fn sync_layer_items_if_needed(
    handoff: &Path,
    doc: &mut DocMetadata,
    body: &str,
    now: &str,
    structural_change: bool,
    warnings: &mut Vec<String>,
) -> bool {
    if doc.layer.is_none() {
        return false;
    }
    let raw_hash = lexsim::fnv1a_hex(body.as_bytes());
    let already_synced = !structural_change
        && doc.verification.is_some()
        && doc.source.body_raw_hash.as_deref() == Some(raw_hash.as_str());
    if already_synced {
        return false;
    }
    let id_prefixes = read_config(&handoff.join("config.toml"))
        .map(|c| c.trace.id_prefixes)
        .unwrap_or_default();
    let outcome = sync_layer_items(doc, body, &id_prefixes, now);
    warnings.extend(outcome.warnings);
    doc.source.body_raw_hash = Some(raw_hash);

    // t360.7 (wiki/220 §2.4 step 7 / §2.5): `rebuild_item_task_ids`'s
    // differential apply for layer sync — unlink the dangling reverse
    // `task_links` of every task that referenced a `SubItem` this sync just
    // dropped (`outcome.removed`). `outcome.removed_task_ids` already knows
    // exactly which task_ids to detach per removed stable_id (captured from
    // the `SubItem` right before it was discarded), so this never scans the
    // task tree — only the (typically empty) handful of tasks that were
    // actually linked to a now-gone requirement. Retained SubItems' task_ids
    // are left untouched here (as `sync_layer_items` already preserves them
    // by stable_id — see its own doc comment): only removal needs handling.
    for (stable_id, task_ids) in &outcome.removed_task_ids {
        if task_ids.is_empty() {
            continue;
        }
        match remove_stale_reverse_links(handoff, &*doc, stable_id, task_ids) {
            Ok(unlinked) => {
                for task_id in unlinked {
                    warnings.push(format!(
                        "rebuild_item_task_ids: requirement {stable_id:?} was removed from the \
                         layer body; unlinked task {task_id}'s reverse link"
                    ));
                }
            }
            Err(e) => warnings.push(format!(
                "rebuild_item_task_ids: failed to unlink task(s) from removed requirement \
                 {stable_id:?}: {e}"
            )),
        }
    }

    if let Some(v) = &doc.verification {
        warnings.extend(duplicate_stable_id_warnings_within_doc(v));
    }

    true
}

/// wiki/220 §4.2 (FR-105), extended to a single document (rework round 2,
/// MAJOR fix): after a layer sync rebuilds `doc.verification`, the same
/// `stable_id` can end up on more than one `SubItem` *within this one
/// document* — most commonly a pre-existing `origin=None` (legacy,
/// `req_import`/`add_item`-authored) SubItem and a freshly parsed
/// `origin=body` SubItem that happen to share an id (a document that had
/// `req_import` run on it before `layer` was ever set, whose body later
/// grows a heading that reuses the same id). `collect_all_stable_ids`'s
/// per-document dedup (`collect_stable_ids` returns a `HashSet`) cannot see
/// this — it only reports collisions *across* documents — so this is a
/// separate, single-document check.
fn duplicate_stable_ids_within_doc(v: &Verification) -> Vec<String> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut dupes: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for item in &v.items {
        for sub in &item.sub_items {
            if let Some(id) = sub.stable_id.as_deref() {
                if !seen.insert(id) {
                    dupes.insert(id.to_string());
                }
            }
        }
    }
    dupes.into_iter().collect()
}

fn duplicate_stable_id_warnings_within_doc(v: &Verification) -> Vec<String> {
    duplicate_stable_ids_within_doc(v)
        .into_iter()
        .map(|id| {
            format!(
                "stable_id {id:?} is assigned to more than one SubItem within this document \
                 (e.g. a legacy item and a body item reusing the same id) — resolve_stable_ids \
                 will treat it as ambiguous until the duplicate is resolved by editing the body \
                 or the legacy item"
            )
        })
        .collect()
}

/// wiki/220 §4.2 (FR-105) applied to the M1 layer sync timing rule (§2.4):
/// called from `handle_doc_save`/`handle_doc_update_section` after a layer
/// sync actually ran (and the document was written to disk) to (1) warn
/// about any `stable_id` this document's body now shares with a *different*
/// document, and (2) refresh `_requirements_summary.json` from the same
/// freshly-loaded `DocSet` — step 7 of §2.4 ("summary regeneration"), which
/// is this task's own responsibility (only `rebuild_item_task_ids` moved to
/// t360.7). Loads the corpus exactly once (P-M3): the same `DocSet` backs
/// both the collision scan and the summary write.
fn refresh_after_layer_sync(
    handoff: &Path,
    own_doc_id: &str,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let doc_set = DocSet::load(handoff)?;
    let mut collisions: Vec<(String, Vec<String>)> = collect_all_stable_ids(doc_set.docs())
        .into_iter()
        .filter(|(_, owners)| owners.len() > 1 && owners.iter().any(|o| o == own_doc_id))
        .collect();
    collisions.sort_by(|a, b| a.0.cmp(&b.0));
    for (id, owners) in collisions {
        let others: Vec<&str> = owners
            .iter()
            .map(String::as_str)
            .filter(|o| *o != own_doc_id)
            .collect();
        warnings.push(format!(
            "stable_id {id:?} already exists in other document(s): {} — resolve_stable_ids will \
             treat it as ambiguous until resolved",
            others.join(", ")
        ));
    }
    write_requirements_summary(handoff, doc_set.docs())
}

/// The exact refusal message every write-guarded `doc_verify` action on a
/// layer document returns (wiki/220-vmodel-integration-design.md §2.3): body-
/// owned `SubItem` fields (`description`, `layer`, `refines`, `verifies`,
/// `method`, `priority`, `test_refs`) are defined by the Markdown body, not
/// writable through `doc_verify` — editing the body and re-saving is the
/// only path.
pub(crate) const LAYER_BODY_EDIT_GUARD_MSG: &str =
    "This is a layer document; body-owned fields (description/layer/refines/verifies/method/priority/test_refs) are defined by the Markdown body — 本文を編集してください (edit the body and save it, rather than calling this action)";

/// Bonus added to a document's BM25 score when one of its `scope_paths` is a
/// prefix of one of the query's `file_paths`. Mirrors `memory.rs`'s
/// `SCOPE_PATH_BONUS` — kept as a separate constant since the two features
/// tune independently even though the value happens to match today.
const SCOPE_PATH_BONUS: f64 = 2.0;

/// Default relevance floor for `doc_list(query=...)`. Kept at 0.0 (no floor)
/// since `doc_list` is an explicit search the caller controls via `query`
/// presence/absence, unlike `memory_query`'s hook-driven auto-injection which
/// needs a floor to avoid noise.
const DOC_QUERY_MIN_SCORE: f64 = 0.0;

fn new_doc_id() -> String {
    format!("doc-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S-%6f"))
}

/// Resolve a document by either its file-naming `slug` or its stable `id`
/// (spec instructs `doc_get`/`doc_delete`/etc. to accept either). Tries the
/// direct slug-keyed file lookup first (cheap, no scan), falling back to a
/// full `id` scan so callers that only recorded a document's `id` (e.g. from
/// a `related`/`parent_id` reference) can still resolve it.
fn resolve_doc(handoff: &Path, slug_or_id: &str) -> Result<Option<DocMetadata>> {
    // Hashed: most call sites of `resolve_doc` operate on exactly one
    // document (doc_get, doc_reassemble, ...) and genuinely need a
    // trustworthy hash, so eagerly computing it costs nothing extra compared
    // to before t370.8 introduced laziness at the corpus-scan level (P-M1,
    // wiki/240-performance-design.md §4). `handoff_doc_verify` is the
    // exception for actions that never look at a hash at all — see
    // [`resolve_doc_for_verify`] (t370.12, PR-4): at JA scale (~1.25MB
    // documents) this function's unconditional `read_doc_hashed` is
    // expensive enough that paying it for e.g. `set_dev_stage`/`link_task`
    // would blow their PR-4 budget for no reason.
    if let Some(doc) = read_doc_hashed(handoff, slug_or_id)? {
        return Ok(Some(doc));
    }
    match find_doc_by_id(handoff, slug_or_id)? {
        Some(doc) => read_doc_hashed(handoff, &doc.slug),
        None => Ok(None),
    }
}

/// Like [`resolve_doc`], but only pays the `lexsim::content_hash` cost when
/// `need_hash` is true — `handoff_doc_verify`'s dispatch (t370.12, PR-4)
/// passes `false` for every action except `check`/`check_all` (the only two
/// that read a section's `content_hash` to record `content_hash_at_verify`).
/// A `false` caller gets `doc.content_hash: None` and every section's
/// `content_hash: None` (P-M1, t370.8) — safe here because those actions
/// never read either field, and the eventual `write_doc` at the end of
/// `handle_doc_verify` still persists a correct hash (t370.12's write-time
/// reuse of this process's already-proven value, or a fresh compute as a
/// fallback — see `storage::docs::write_doc_with_body`).
fn resolve_doc_for_verify(
    handoff: &Path,
    slug_or_id: &str,
    need_hash: bool,
) -> Result<Option<DocMetadata>> {
    if need_hash {
        return resolve_doc(handoff, slug_or_id);
    }
    if let Some(doc) = read_doc(handoff, slug_or_id)? {
        return Ok(Some(doc));
    }
    match find_doc_by_id(handoff, slug_or_id)? {
        Some(doc) => read_doc(handoff, &doc.slug),
        None => Ok(None),
    }
}

/// Whether `handle_doc_verify`'s `action` needs a trustworthy `content_hash`
/// resolved for it (`true`) — i.e. must go through [`resolve_doc_for_verify`]
/// with `need_hash: true` — or can safely take the lazy, no-hash path
/// (`false`).
///
/// This is deliberately a deny-list (default `true`, with an explicit,
/// audited list of actions proven to never read a hash) rather than an
/// allow-list (default `false`, listing only the actions that *do* need
/// one) — t370.12 rework, MINOR, integration feedback round 1: an allow-list
/// here is fail-open-by-omission, since any action added to
/// `handle_doc_verify`'s match block in the future that *does* need a hash
/// would silently default to the lazy path unless a developer remembered to
/// add it to the allow-list too. A deny-list instead fails closed: an
/// unrecognized action (including one not yet written) defaults to `true`
/// (safe, if slightly more expensive) rather than `false` (unsafe).
///
/// Only `"check"`/`"check_all"` read a section's `content_hash` (to record
/// `content_hash_at_verify`, see [`resolve_doc_for_verify`]'s doc comment).
/// Every other currently-known action only mutates `SubItem`/
/// `VerificationItem` metadata fields (`"generate"`, `"skip"`, `"sync"`,
/// `"set_refs"`, `"set_dev_stage"`, `"set_priority"`, `"link_task"`,
/// `"add_item"`, `"backfill_stable_ids"`) or reads no document state at all
/// (`"suggest_refs"`).
fn action_needs_content_hash(action: &str) -> bool {
    !matches!(
        action,
        "generate"
            | "skip"
            | "sync"
            | "set_refs"
            | "set_dev_stage"
            | "set_priority"
            | "link_task"
            | "add_item"
            | "backfill_stable_ids"
            | "suggest_refs"
    )
}

/// Like [`resolve_doc`] (accepts either the file-naming `slug` or the stable
/// `id`), but also returns the document's body from the exact same read as
/// its metadata — see [`read_doc_with_body`]'s doc comment for why callers
/// that byte-slice the body using `DocMetadata.sections` (e.g.
/// `handle_doc_update_section`) need that guarantee instead of resolving the
/// document and reading its body as two independent calls.
fn resolve_doc_with_body(
    handoff: &Path,
    slug_or_id: &str,
) -> Result<Option<(DocMetadata, String)>> {
    // Hashed — see `resolve_doc`'s doc comment: single-document lookup, so
    // no perf regression from always computing the hash here.
    if let Some(pair) = read_doc_with_body_hashed(handoff, slug_or_id)? {
        return Ok(Some(pair));
    }
    let Some(doc) = find_doc_by_id(handoff, slug_or_id)? else {
        return Ok(None);
    };
    read_doc_with_body_hashed(handoff, &doc.slug)
}

/// `handoff_doc_save` — create or update a document from a full Markdown
/// body: split into in-memory sections, persist the body + metadata as a
/// slug-named pair, and sync the task<->doc bidirectional link.
pub fn handle_doc_save(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;
    ensure_docs_dir(handoff)?;

    let body_arg = arguments.get("body").and_then(|v| v.as_str());
    let append_body_arg = arguments.get("append_body").and_then(|v| v.as_str());
    if body_arg.is_some() && append_body_arg.is_some() {
        anyhow::bail!("'body' and 'append_body' are mutually exclusive");
    }

    let doc_id = arguments.get("doc_id").and_then(|v| v.as_str());
    if append_body_arg.is_some() && doc_id.is_none() {
        anyhow::bail!(
            "'append_body' requires 'doc_id' (appending to a new document is not meaningful)"
        );
    }
    let existing = match doc_id {
        Some(id) => Some(
            find_doc_by_id(handoff, id)?
                .ok_or_else(|| anyhow::anyhow!("Document not found: {id}"))?,
        ),
        None => None,
    };

    // Metadata-only update path (wiki/210-req-traceability-refinement.md
    // §M1): when both `body` and `append_body` are omitted, this is only
    // valid as an update to an existing document (`doc_id` resolved above) —
    // new documents always require a body. The existing body is re-read
    // below (not skipped) so `split()`/`compute_sections()` still run and
    // keep `sections`/`content_hash` consistent with what's on disk, even
    // though nothing textual changed.
    if body_arg.is_none() && append_body_arg.is_none() && existing.is_none() {
        anyhow::bail!("either 'body' or 'append_body' is required");
    }

    // `append_body`: join the appended text onto the existing document's
    // stripped body (read_doc_body — NOT read_full_body, whose BOM
    // restoration would otherwise get re-detected and double-persisted by
    // `split()` below). No separator is inserted when the existing body is
    // empty/missing (spec §3.1 edge case).
    //
    // Metadata-only update (both args None): re-read the existing body
    // verbatim so `split()`/`compute_sections()` below stay consistent with
    // disk, without writing anything back to the body file (`is_metadata_only`
    // gates that skip further down).
    let is_metadata_only = body_arg.is_none() && append_body_arg.is_none();
    let joined_body: String;
    let body: &str = if let Some(append_body) = append_body_arg {
        let existing_doc = existing
            .as_ref()
            .expect("append_body requires doc_id, checked above, so existing is Some");
        let existing_body = read_doc_body(handoff, &existing_doc.slug)?.unwrap_or_default();
        let separator = arguments
            .get("separator")
            .and_then(|v| v.as_str())
            .unwrap_or("\n\n");
        joined_body = if existing_body.is_empty() {
            append_body.to_string()
        } else {
            format!("{existing_body}{separator}{append_body}")
        };
        &joined_body
    } else if is_metadata_only {
        let existing_doc = existing
            .as_ref()
            .expect("metadata-only path requires an existing document, checked above");
        joined_body = read_doc_body(handoff, &existing_doc.slug)?.unwrap_or_default();
        &joined_body
    } else {
        body_arg.expect("body_arg is Some in this branch, checked above")
    };

    // slug: required for new documents, taken from the existing document on
    // update (the `slug` argument is ignored on update — renaming a
    // document's file-naming slug is out of scope for `doc_save`).
    let slug = match &existing {
        Some(d) => d.slug.clone(),
        None => {
            let slug = arguments
                .get("slug")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("'slug' is required for new documents"))?
                .to_string();
            validate_slug(&slug)?;
            if read_doc(handoff, &slug)?.is_some() {
                anyhow::bail!("slug '{slug}' is already in use by another document");
            }
            slug
        }
    };

    let title = arguments
        .get("title")
        .and_then(|v| v.as_str())
        .or(existing.as_ref().map(|d| d.title.as_str()))
        .ok_or_else(|| anyhow::anyhow!("'title' is required for new documents"))?
        .to_string();

    // wiki/220-vmodel-integration-design.md §2.4 "付随修正": on an update,
    // omitting `split_level` must keep the existing document's value, not
    // silently reset to the default — otherwise every metadata-only or
    // body-only update that doesn't repeat `split_level` could re-split the
    // body into different sections than the caller last set (and, now that
    // layer sync (t360.6) is wired in, spuriously move layer items between
    // sections).
    let split_level = arguments
        .get("split_level")
        .and_then(|v| v.as_u64())
        .map(|n| n as u8)
        .unwrap_or_else(|| {
            existing
                .as_ref()
                .map(|d| d.split_level)
                .unwrap_or(crate::storage::docs::split::DEFAULT_SPLIT_LEVEL)
        });

    let split_doc = split(body, split_level)?;

    let now = chrono::Utc::now().to_rfc3339();
    let id = doc_id.map(str::to_string).unwrap_or_else(new_doc_id);

    // Rework round 2 (MAJOR fix): captured before `existing` is moved into
    // `doc` below — `sync_layer_items_if_needed`'s raw-body-hash short-circuit
    // only detects body byte changes, but its output also depends on
    // `doc.layer`/`split_level` (see that function's doc comment). Comparing
    // against these lets the call below force a re-sync when either changed,
    // even on a metadata-only save that never touches the body.
    let previous_layer = existing.as_ref().and_then(|d| d.layer.clone());
    let previous_split_level = existing.as_ref().map(|d| d.split_level);

    let mut doc = match existing {
        Some(mut d) => {
            d.title = title.clone();
            d
        }
        None => {
            let mut d = DocMetadata::new(
                id.clone(),
                slug.clone(),
                title.clone(),
                "note".to_string(),
                now.clone(),
            );
            d.source.origin = "authored".to_string();
            d
        }
    };

    if let Some(doc_type) = arguments.get("doc_type").and_then(|v| v.as_str()) {
        doc.doc_type = doc_type.to_string();
    }
    if let Some(tags) = arguments.get("tags") {
        doc.tags = string_array_value(tags);
    }
    if let Some(scope_paths) = arguments.get("scope_paths") {
        doc.scope_paths = string_array_value(scope_paths);
    }
    let previous_parent_id = doc.parent_id.clone();
    if let Some(parent_id) = arguments.get("parent_id") {
        doc.parent_id = parent_id.as_str().map(str::to_string);
    }
    let mut warnings: Vec<String> = Vec::new();
    if let Some(related) = arguments.get("related").and_then(|v| v.as_array()) {
        let mut malformed_count = 0usize;
        doc.related = related
            .iter()
            .filter_map(|r| {
                let rid = r.get("id").and_then(|v| v.as_str());
                let rel = r.get("rel").and_then(|v| v.as_str());
                match (rid, rel) {
                    (Some(rid), Some(rel)) => Some(DocRelation {
                        id: rid.to_string(),
                        rel: rel.to_string(),
                    }),
                    _ => {
                        malformed_count += 1;
                        None
                    }
                }
            })
            .collect();
        if malformed_count > 0 {
            warnings.push(format!(
                "Ignored {malformed_count} malformed 'related' entr{} (each entry requires string 'id' and 'rel')",
                if malformed_count == 1 { "y" } else { "ies" }
            ));
        }
    }
    if let Some(auto_inject) = arguments.get("auto_inject").and_then(|v| v.as_str()) {
        doc.auto_inject = auto_inject.to_string();
    }
    // wiki/220 §2.1: `doc_save`'s `layer` argument is the only AI-facing way
    // to set `DocMetadata.layer` — an empty string clears it (explicit
    // "unset", distinct from omitting the argument, which leaves whatever
    // was already there untouched).
    if let Some(layer) = arguments.get("layer").and_then(|v| v.as_str()) {
        doc.layer = if layer.is_empty() {
            None
        } else {
            Some(layer.to_string())
        };
    }

    doc.has_bom = split_doc.has_bom;
    doc.line_ending = split_doc.line_ending.to_string();
    doc.split_level = split_level;
    doc.updated_at = now.clone();

    // v5: the full body (after BOM/frontmatter stripping) is written verbatim
    // to `_doc.<slug>.md`; sections are an in-memory byte-offset index into
    // it, computed fresh on every save (no stale-fragment cleanup needed —
    // there is nothing left on disk to clean up per section). On a
    // metadata-only update (§M1) `body_after_strip` is just the unchanged
    // existing body re-read above — `split()`/`compute_sections()` still run
    // so `sections`/`content_hash` stay consistent, but `write_doc_body` is
    // skipped (nothing textual changed, so there's nothing to persist) and
    // the heading-format warning is suppressed (it would otherwise reproduce
    // on every metadata-only update of a body that predates this check).
    let body_after_strip: String = split_doc.fragments.iter().map(|f| f.body).collect();
    if !is_metadata_only && !body_after_strip.starts_with("# ") {
        warnings.push(
            "body does not start with a level-1 heading — consider adding one for readability"
                .to_string(),
        );
    }
    if !is_metadata_only {
        write_doc_body(handoff, &slug, &body_after_strip)?;
    }
    // false: doc_save never reads back per-section content_hash (only
    // section_count in its response) — skip the redundant per-section
    // lexsim pass (P-M1, wiki/240-performance-design.md §4, t370.8).
    doc.sections = compute_sections(&split_doc, false);

    let content_hash = lexsim::content_hash(&body_after_strip);
    doc.content_hash = Some(content_hash.clone());
    doc.source.canonical_hash = Some(content_hash);

    let structural_change =
        doc.layer != previous_layer || Some(doc.split_level) != previous_split_level;
    let layer_synced = sync_layer_items_if_needed(
        handoff,
        &mut doc,
        &body_after_strip,
        &now,
        structural_change,
        &mut warnings,
    );

    let new_task_ids = arguments
        .get("task_ids")
        .map(string_array_value)
        .unwrap_or_else(|| doc.task_ids.clone());

    if arguments.get("task_ids").is_some() {
        let (link_ids, unlink_ids) = if doc_id.is_some() {
            let previous: Vec<String> = doc.task_ids.clone();
            let link: Vec<String> = new_task_ids
                .iter()
                .filter(|t| !previous.contains(t))
                .cloned()
                .collect();
            let unlink: Vec<String> = previous
                .iter()
                .filter(|t| !new_task_ids.contains(t))
                .cloned()
                .collect();
            (link, unlink)
        } else {
            (new_task_ids.clone(), Vec::new())
        };

        let tasks_dir = handoff.join("tasks");
        let report = sync_doc_task_links(&tasks_dir, &id, &title, &link_ids, &unlink_ids)?;
        if !report.unresolved.is_empty() {
            warnings.push(format!(
                "Could not resolve task id(s) for linking: {}",
                report.unresolved.join(", ")
            ));
        }
        doc.task_ids = new_task_ids;
    }

    write_doc(handoff, &doc)?;

    if layer_synced {
        refresh_after_layer_sync(handoff, &doc.id, &mut warnings)?;
    }

    // Keep the family tree's `children` list in sync with `parent_id`: if the
    // parent changed (including unset -> set on first save), push this doc's
    // id into the new parent's `children` and drop it from the old parent's,
    // mirroring the same "sync the other side" pattern as
    // sync_doc_task_links. A parent id that doesn't resolve is a non-fatal
    // warning, not a rollback — same policy as unresolved task_ids above.
    // `parent_id` references a document's stable `id`, not its `slug`, so
    // resolution goes through `find_doc_by_id`.
    if doc.parent_id != previous_parent_id {
        if let Some(old_parent_id) = &previous_parent_id {
            if let Some(mut old_parent) = find_doc_by_id(handoff, old_parent_id)? {
                let before = old_parent.children.len();
                old_parent.children.retain(|c| c != &id);
                if old_parent.children.len() != before {
                    write_doc(handoff, &old_parent)?;
                }
            }
        }
        if let Some(new_parent_id) = &doc.parent_id {
            match find_doc_by_id(handoff, new_parent_id)? {
                Some(mut new_parent) => {
                    if !new_parent.children.iter().any(|c| c == &id) {
                        new_parent.children.push(id.clone());
                        write_doc(handoff, &new_parent)?;
                    }
                }
                None => warnings.push(format!("Parent document not found: {new_parent_id}")),
            }
        }
    }

    Ok(to_json(&json!({
        "doc_id": id,
        "slug": doc.slug,
        "title": doc.title,
        "doc_type": doc.doc_type,
        "section_count": doc.sections.len(),
        "content_hash": doc.content_hash,
        "warnings": warnings,
    })))
}

/// Validates that a section's `(byte_offset, byte_length)` actually falls
/// inside `body` on UTF-8 char boundaries before `handle_doc_update_section`
/// byte-slices it, returning the `(start, end)` range on success.
///
/// `resolve_doc_with_body` already guarantees `section` and `body` come from
/// the same read (review round 2 MAJOR fix), so this should never actually
/// trip in production — it is defense in depth against `&body[..start]` /
/// `&body[end..]` panicking on a non-char boundary (likely with JA text) or
/// silently splicing at the wrong offsets if that invariant is ever broken
/// by a future code path.
fn validate_section_splice_range(
    body: &str,
    byte_offset: usize,
    byte_length: usize,
    doc_id: &str,
    seq: usize,
) -> Result<(usize, usize)> {
    let start = byte_offset;
    let end = byte_offset + byte_length;
    if start > end
        || end > body.len()
        || !body.is_char_boundary(start)
        || !body.is_char_boundary(end)
    {
        anyhow::bail!(
            "Section byte range out of sync with document body for doc_id={doc_id} seq={seq} \
             (start={start}, end={end}, body_len={}); retry the update",
            body.len()
        );
    }
    Ok((start, end))
}

/// `handoff_doc_update_section` — replace a single section's body by `seq`
/// without requiring the caller to re-send the whole document (partial
/// update API, t123.4). Computes sections on-demand from the current body
/// (mirrors `read_doc`'s recompute — sections are never trusted from
/// frontmatter), byte-slices out the target section's range, splices in
/// `new_content`, and writes the result back. `expected_hash` is an optional
/// optimistic lock against the section's current `content_hash`.
pub fn handle_doc_update_section(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let doc_id = arguments
        .get("doc_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'doc_id' is required"))?;
    let seq = arguments
        .get("seq")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow::anyhow!("'seq' is required"))? as usize;
    let new_content = arguments
        .get("new_content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'new_content' is required"))?;
    let expected_hash = arguments.get("expected_hash").and_then(|v| v.as_str());

    // `doc.sections` and `body` below come from `resolve_doc_with_body`'s
    // single consistent read (review round 2 MAJOR fix), not from two
    // independent reads — see that function's doc comment. This is what
    // still lets this handler skip a redundant `split()` + `compute_sections()`
    // pass over `body` (manager follow-up to t370.3, wiki/240 §4 C7: that
    // recompute was the dominant cost left in `doc_update_section` after
    // t370.2's read-path cache) without risking a metadata/body desync.
    let (mut doc, body) = resolve_doc_with_body(handoff, doc_id)?
        .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;

    let section = doc
        .sections
        .iter()
        .find(|s| s.seq == seq)
        .ok_or_else(|| anyhow::anyhow!("Section not found: doc_id={doc_id} seq={seq}"))?;

    if let Some(expected) = expected_hash {
        if section.content_hash.as_deref() != Some(expected) {
            anyhow::bail!(
                "expected_hash mismatch for doc_id={doc_id} seq={seq}: expected {expected}, \
                 current content_hash is {} — retry with the current hash if this overwrite is \
                 still intended",
                section.content_hash.as_deref().unwrap_or("(not computed)")
            );
        }
    }

    // Splice `new_content` into the section's byte range. `extract_section`
    // is not used here (it would re-validate the just-computed hash, which
    // is redundant since `section` was computed from this exact `body`) —
    // the byte range is sliced directly instead. Defense in depth on top of
    // `resolve_doc_with_body`'s same-read guarantee: validate the range
    // actually falls inside `body` on UTF-8 char boundaries before slicing,
    // so a `sections`/`body` mismatch from an unrelated code path in the
    // future fails cleanly here instead of panicking on a JA multi-byte
    // boundary or silently corrupting the document.
    let (start, end) = validate_section_splice_range(
        &body,
        section.byte_offset,
        section.byte_length,
        doc_id,
        seq,
    )?;
    let mut new_body = String::with_capacity(body.len() - (end - start) + new_content.len());
    new_body.push_str(&body[..start]);
    new_body.push_str(new_content);
    new_body.push_str(&body[end..]);

    let new_split_doc = split(&new_body, doc.split_level)?;
    // true: the response echoes the updated section's content_hash below,
    // and `expected_hash`'s optimistic lock needs it on the *next* call.
    let new_sections = compute_sections(&new_split_doc, true);
    doc.sections = new_sections.clone();

    let now = chrono::Utc::now().to_rfc3339();
    doc.updated_at = now.clone();

    let content_hash = lexsim::content_hash(&new_body);
    doc.content_hash = Some(content_hash.clone());
    doc.source.canonical_hash = Some(content_hash);

    // wiki/220 §2.4 timing rule: `doc_update_section`'s body change is
    // re-synced at the end of the call, same as `doc_save`. `structural_change:
    // false` — `doc_update_section` never touches `doc.layer`/`split_level`
    // itself (only `doc_save` accepts those arguments), so the raw-body-hash
    // short-circuit alone is always the right check here.
    let mut warnings: Vec<String> = Vec::new();
    let layer_synced =
        sync_layer_items_if_needed(handoff, &mut doc, &new_body, &now, false, &mut warnings);

    // Single atomic write of frontmatter+body together, using `new_body`
    // already in memory (P-M3, wiki/240 §4 C7): the previous
    // `write_doc_body` + `write_doc` pair wrote the same file twice and had
    // `write_doc` read the just-written body back off disk first.
    write_doc_with_body(handoff, &doc, &new_body)?;

    if layer_synced {
        refresh_after_layer_sync(handoff, &doc.id, &mut warnings)?;
    }

    crate::context::doc_corpus_cache()
        .lock()
        .expect("cache")
        .increment_generation();

    let updated_section = new_sections.iter().find(|s| s.seq == seq);
    let verification_stale = doc.verification.as_ref().is_some_and(|v| {
        v.items
            .iter()
            .any(|i| i.fragment_seq == Some(seq) && item_is_stale(&doc, i))
    });

    let mut out = json!({
        "doc_id": doc.id,
        "seq": seq,
        "heading": updated_section.map(|s| s.heading.clone()),
        "content_hash": updated_section.map(|s| s.content_hash.clone()),
        "updated_at": doc.updated_at,
        "section_count": doc.sections.len(),
    });
    if verification_stale {
        warnings.push(format!(
            "Verification item at fragment_seq={seq} is now stale (content changed since it was verified)"
        ));
    }
    if !warnings.is_empty() {
        out["warnings"] = json!(warnings);
    }

    Ok(to_json(&out))
}

/// Reads a document's authored content body: the part of `_doc.<slug>.md`
/// *after* handoff's own YAML frontmatter block, with the original UTF-8 BOM
/// (if any) restored in front of it. Returns `Ok(None)` when the `.md` file
/// is missing.
///
/// Frontmatter migration (t123.1): the `.md` file's frontmatter now *is* the
/// document's metadata (handoff-owned), not a user-authored block being
/// losslessly stashed — so unlike the pre-migration 2-file format, a leading
/// YAML block the caller originally passed into `doc_save`'s `body` argument
/// is absorbed into (and superseded by) handoff's own frontmatter, not
/// preserved verbatim. Only the BOM is still restored losslessly.
fn read_full_body(handoff: &Path, doc: &DocMetadata) -> Result<Option<String>> {
    let Some(body) = read_doc_body(handoff, &doc.slug)? else {
        return Ok(None);
    };
    Ok(Some(if doc.has_bom {
        format!("\u{FEFF}{body}")
    } else {
        body
    }))
}

/// `handoff_doc_get` — read a document (by `doc_id` or `slug`) as `full`
/// (the original Markdown body + metadata), `meta` (metadata only), or
/// `section` (one section's body, byte-sliced from `_doc.<slug>.md`).
pub fn handle_doc_get(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let doc_id = arguments
        .get("doc_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'doc_id' is required"))?;

    let format = arguments
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("full");

    match format {
        "meta" => {
            let doc = resolve_doc(handoff, doc_id)?
                .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;
            Ok(to_json(&doc_metadata_json(&doc)))
        }
        "section" | "fragment" => {
            let seq = arguments
                .get("seq")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| anyhow::anyhow!("'seq' is required when format='section'"))?
                as usize;
            let doc = resolve_doc(handoff, doc_id)?
                .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;
            let section =
                doc.sections.iter().find(|s| s.seq == seq).ok_or_else(|| {
                    anyhow::anyhow!("Section not found: doc_id={doc_id} seq={seq}")
                })?;
            let body = read_doc_body(handoff, &doc.slug)?.ok_or_else(|| {
                anyhow::anyhow!("Document body file missing for slug '{}'", doc.slug)
            })?;
            let section_body = extract_section(&body, section)?;
            Ok(to_json(&json!({
                "doc_id": doc.id,
                "seq": section.seq,
                "heading": section.heading,
                "level": section.level,
                "content_hash": section.content_hash,
                "body": section_body,
            })))
        }
        _ => {
            let doc = resolve_doc(handoff, doc_id)?
                .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;
            let body = read_full_body(handoff, &doc)?.unwrap_or_default();
            let mut out = doc_metadata_json(&doc);
            out["body"] = json!(body);
            Ok(to_json(&out))
        }
    }
}

/// `handoff_doc_list` — list/search documents with optional `doc_type` /
/// `tags` (AND) / `task_id` filters, BM25 `query` ranking, and optional
/// reassembled `body` inclusion.
pub fn handle_doc_list(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let doc_type = arguments.get("doc_type").and_then(|v| v.as_str());
    let tags = arguments.get("tags").map(string_array_value);
    let task_id = arguments.get("task_id").and_then(|v| v.as_str());
    let include_body = arguments
        .get("include_body")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let query = arguments
        .get("query")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let mut docs = read_all_docs(handoff)?;
    if let Some(dt) = doc_type {
        docs.retain(|d| d.doc_type == dt);
    }
    if let Some(tags) = &tags {
        if !tags.is_empty() {
            docs.retain(|d| tags.iter().all(|t| d.tags.contains(t)));
        }
    }
    if let Some(tid) = task_id {
        docs.retain(|d| d.task_ids.iter().any(|t| t == tid));
    }

    let ordered_indices: Vec<usize> = if let Some(q) = query {
        rank_docs_by_query(handoff, &docs, q)?
    } else {
        (0..docs.len()).collect()
    };

    let mut out_docs = Vec::with_capacity(ordered_indices.len());
    for idx in ordered_indices {
        let d = &docs[idx];
        let mut entry = doc_metadata_json(d);
        if include_body {
            let body = read_full_body(handoff, d)?.unwrap_or_default();
            entry["body"] = json!(body);
        }
        out_docs.push(entry);
    }

    Ok(to_json(&json!({ "documents": out_docs })))
}

/// Ranks `docs` against `query` via BM25 over each document's index text
/// (title + tags + body), returning original-order indices sorted by
/// descending relevance. Corpus is built fresh every call (no cache — the
/// cache is reserved for `doc_query`, t96.3, per the task's own note).
fn rank_docs_by_query(handoff: &Path, docs: &[DocMetadata], query: &str) -> Result<Vec<usize>> {
    let mut index_texts = Vec::with_capacity(docs.len());
    for d in docs {
        let body = read_doc_body(handoff, &d.slug)?.unwrap_or_default();
        let mut text = d.title.clone();
        text.push(' ');
        text.push_str(&d.tags.join(" "));
        text.push(' ');
        text.push_str(&body);
        index_texts.push(text);
    }

    let corpus = lexsim::Corpus::build_weighted(&index_texts);
    let query_tokens = lexsim::tokenize_weighted(query);
    let scope_paths: Vec<Vec<String>> = docs.iter().map(|d| d.scope_paths.clone()).collect();
    let config = RankConfig {
        min_score: DOC_QUERY_MIN_SCORE,
        relative_threshold: 0.0,
        scope_path_bonus: SCOPE_PATH_BONUS,
        limit: docs.len(),
    };
    let ranked = rank_by_bm25_and_scope(&corpus, &query_tokens, &scope_paths, &[], &config);
    Ok(ranked.into_iter().map(|item| item.index).collect())
}

/// `handoff_doc_delete` — delete a document (by `doc_id` or `slug`) and its
/// body file, unlink it from any linked tasks, remove it from its parent's
/// `children`, and orphan (clear `parent_id` on) any of its own children.
/// See `wiki/130-document-management.md` §5.4.
pub fn handle_doc_delete(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let doc_id = arguments
        .get("doc_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'doc_id' is required"))?;

    let doc = resolve_doc(handoff, doc_id)?
        .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;

    let mut warnings: Vec<String> = Vec::new();

    delete_doc_body(handoff, &doc.slug)?;
    delete_doc(handoff, &doc.slug)?;

    if !doc.task_ids.is_empty() {
        let tasks_dir = handoff.join("tasks");
        let report = sync_doc_task_links(&tasks_dir, &doc.id, &doc.title, &[], &doc.task_ids)?;
        if !report.unresolved.is_empty() {
            warnings.push(format!(
                "Could not resolve task id(s) for unlinking: {}",
                report.unresolved.join(", ")
            ));
        }
    }

    if let Some(parent_id) = &doc.parent_id {
        if let Some(mut parent) = find_doc_by_id(handoff, parent_id)? {
            let before = parent.children.len();
            parent.children.retain(|c| c != &doc.id);
            if parent.children.len() != before {
                write_doc(handoff, &parent)?;
            }
        } else {
            warnings.push(format!("Parent document not found: {parent_id}"));
        }
    }

    for child_id in &doc.children {
        if let Some(mut child) = find_doc_by_id(handoff, child_id)? {
            child.parent_id = None;
            write_doc(handoff, &child)?;
        } else {
            warnings.push(format!("Child document not found: {child_id}"));
        }
    }

    crate::context::doc_corpus_cache()
        .lock()
        .expect("cache")
        .increment_generation();

    // FR-905 (wiki/220 §4.3): if the deleted document held any requirement
    // SubItems, `_requirements_summary.json` must be refreshed so it stops
    // reflecting a now-deleted document — including deleting the file
    // entirely if this was the last document with any requirements
    // (`write_requirements_summary` handles that). Guarded on the deleted
    // doc actually having had SubItems so a routine delete of a
    // requirement-less document doesn't pay the `read_all_docs` cost.
    let had_requirements = doc
        .verification
        .as_ref()
        .is_some_and(|v| v.items.iter().any(|i| !i.sub_items.is_empty()));
    if had_requirements {
        let all_docs = read_all_docs(handoff)?;
        write_requirements_summary(handoff, &all_docs)?;
    }

    Ok(to_json(&json!({
        "deleted": true,
        "doc_id": doc.id,
        "section_count": doc.sections.len(),
        "warnings": warnings,
    })))
}

/// `handoff_doc_reassemble` — read a document's (by `doc_id` or `slug`)
/// original Markdown body directly from `_doc.<slug>.md` (v5: the `.md` file
/// already *is* the original document, restoring BOM/frontmatter is the only
/// reassembly step left), and detect drift (the body's current content hash
/// no longer matches the recorded `content_hash` — e.g. edited directly
/// outside `doc_save`). See `wiki/130-document-management.md` §5.5.
pub fn handle_doc_reassemble(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let doc_id = arguments
        .get("doc_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'doc_id' is required"))?;

    let doc = resolve_doc(handoff, doc_id)?
        .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;

    // `doc.content_hash` is recomputed fresh from the body on every read
    // (t123.2), so comparing it to itself would never detect drift.
    // `doc.source.canonical_hash` is the hash persisted at the *last
    // `doc_save`* (untouched by the on-read recompute — see
    // `storage::docs::read_doc`), so that's the correct "was this edited
    // out-of-band since the last save" baseline.
    let drifted = doc.source.canonical_hash.as_deref() != doc.content_hash.as_deref();

    let body = read_full_body(handoff, &doc)?.unwrap_or_default();

    let output_path = arguments.get("output_path").and_then(|v| v.as_str());
    let mut out = json!({
        "doc_id": doc.id,
        "body": body,
        "drifted": drifted,
    });
    if let Some(path) = output_path {
        std::fs::write(path, &body)
            .with_context(|| format!("Failed to write reassembled document to {path}"))?;
        out["output_path"] = json!(path);
    }

    Ok(to_json(&out))
}

/// `handoff_doc_tree` — traverse a document's family tree starting from
/// `doc_id`: its immediate parent (if any) plus `depth` levels of children,
/// optionally including its `related` (semantic) links. See
/// `wiki/130-document-management.md` §5.6.
pub fn handle_doc_tree(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let doc_id = arguments
        .get("doc_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'doc_id' is required"))?;

    let depth = arguments
        .get("depth")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_TREE_DEPTH);

    let include_related = arguments
        .get("include_related")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let doc = resolve_doc(handoff, doc_id)?
        .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;

    let mut tree = doc_tree_node_json(handoff, &doc, include_related)?;

    let parent = match &doc.parent_id {
        Some(parent_id) => find_doc_by_id(handoff, parent_id)?.map(|p| doc_tree_summary_json(&p)),
        None => None,
    };
    tree["parent"] = parent.unwrap_or(Value::Null);

    tree["children"] = json!(doc_tree_children(
        handoff,
        &doc.children,
        depth,
        include_related
    )?);

    Ok(to_json(&tree))
}

/// Default depth for `handoff_doc_tree` when `depth` is omitted (spec §5.6).
const DEFAULT_TREE_DEPTH: u64 = 2;

/// Recursively builds the `children` array for [`handle_doc_tree`], descending
/// up to `depth` levels. Missing child documents (broken link) are skipped
/// rather than erroring the whole traversal.
fn doc_tree_children(
    handoff: &Path,
    child_ids: &[String],
    depth: u64,
    include_related: bool,
) -> Result<Vec<Value>> {
    if depth == 0 {
        return Ok(Vec::new());
    }
    let mut out = Vec::with_capacity(child_ids.len());
    for child_id in child_ids {
        let Some(child) = find_doc_by_id(handoff, child_id)? else {
            continue;
        };
        let mut node = doc_tree_node_json(handoff, &child, include_related)?;
        node["children"] = json!(doc_tree_children(
            handoff,
            &child.children,
            depth - 1,
            include_related
        )?);
        out.push(node);
    }
    Ok(out)
}

/// Compact `{id, title, doc_type}` summary used for `parent` and family-tree
/// list entries (`related`) in `doc_tree`'s output.
fn doc_tree_summary_json(doc: &DocMetadata) -> Value {
    json!({
        "id": doc.id,
        "title": doc.title,
        "doc_type": doc.doc_type,
    })
}

/// One node in the `doc_tree` output: id/title/doc_type plus (optionally)
/// `related` summaries (each resolved to `{id, rel, title}`). `children` is
/// populated by the caller afterward.
fn doc_tree_node_json(handoff: &Path, doc: &DocMetadata, include_related: bool) -> Result<Value> {
    let mut related: Vec<Value> = Vec::new();
    if include_related {
        for r in &doc.related {
            // related entries may point cross-tree/cross-project ids that
            // don't resolve locally; that lookup is deferred to a future
            // resolver (spec §10.3) — for now a related id that can't be
            // read from this project's docs/ is a no-op skip, matching the
            // same lenient policy as read_all_docs.
            let Some(target) = find_doc_by_id(handoff, &r.id)? else {
                continue;
            };
            related.push(json!({ "id": r.id, "rel": r.rel, "title": target.title }));
        }
    }
    Ok(json!({
        "id": doc.id,
        "title": doc.title,
        "doc_type": doc.doc_type,
        "children": [],
        "related": related,
    }))
}

/// Recomputes `Verification.status` from its items (wiki/140-verification-matrix.md
/// §3.3): all pending -> "pending"; all verified/skipped -> "verified";
/// otherwise -> "in_review". v2 (§7.4): an item with `sub_items` is judged by
/// its sub_items' aggregate effective status, not its own `status` field —
/// see `item_effective_status`.
pub(crate) fn recompute_verification_status(items: &[VerificationItem]) -> String {
    let statuses: Vec<String> = items.iter().map(item_effective_status).collect();
    if statuses.iter().all(|s| s == "pending") {
        "pending".to_string()
    } else if statuses.iter().all(|s| s == "verified" || s == "skipped") {
        "verified".to_string()
    } else {
        "in_review".to_string()
    }
}

/// v2 (§7.4): the effective status of a `VerificationItem` for the purposes
/// of the parent `Verification.status` rollup. An item with no `sub_items`
/// uses its own `status` unchanged (v1 behavior). An item with `sub_items`
/// is judged by their aggregate: all verified/skipped -> "verified", all
/// pending -> "pending", otherwise -> "in_review" (a partial mix, distinct
/// from "pending").
fn item_effective_status(item: &VerificationItem) -> String {
    if item.sub_items.is_empty() {
        return item.status.clone();
    }
    if item
        .sub_items
        .iter()
        .all(|s| s.status == "verified" || s.status == "skipped")
    {
        "verified".to_string()
    } else if item.sub_items.iter().all(|s| s.status == "pending") {
        "pending".to_string()
    } else {
        "in_review".to_string()
    }
}

/// Parses a JSON array of `{path, lines?, label?}` objects into `CodeRef`s.
/// Entries missing the required `path` are skipped.
fn code_refs_from_value(v: &Value) -> Vec<CodeRef> {
    v.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    let path = r.get("path").and_then(|v| v.as_str())?.to_string();
                    Some(CodeRef {
                        path,
                        lines: r.get("lines").and_then(|v| v.as_str()).map(str::to_string),
                        label: r.get("label").and_then(|v| v.as_str()).map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Source file extensions `suggest_refs` scans for impl/test definitions
/// (t124.6). Kept as a small fixed list rather than "every file" so a scan
/// stays fast and doesn't surface binary/asset noise; extend here if a
/// project needs another language.
const SUGGEST_REFS_EXTENSIONS: &[&str] = &["rs", "ts", "tsx", "py", "go", "js", "jsx"];

/// Maximum number of impl_ref/test_ref candidates returned per verification
/// item by `suggest_refs` (spec: "Return at most ~20 suggestions per item to
/// avoid overwhelming output"), applied independently to each of the two
/// lists.
const SUGGEST_REFS_MAX_PER_ITEM: usize = 20;

/// A single scanned definition (function/struct/impl/mod, or a test
/// function) found while walking `scope_paths` — the raw material
/// `suggest_refs` matches against verification item headings.
struct ScannedDefinition {
    /// Path to the source file, relative to the project root (matches the
    /// `path` shape already used by `CodeRef`/`set_refs`).
    rel_path: String,
    /// The identifier name found after the defining keyword (e.g. the `foo`
    /// in `fn foo(...)`), used for the heading fuzzy-match.
    name: String,
    /// 1-based line number the definition starts on, used to build the
    /// `lines` hint on the suggested `CodeRef`.
    line: usize,
}

/// Walks `doc.scope_paths` under `project_dir` and, for every verification
/// item, returns impl/test ref candidates whose definition name fuzzy-
/// matches the item's heading (t124.6). Read-only — never touches the
/// document or the filesystem beyond reading source files.
fn suggest_refs(project_dir: &Path, doc: &DocMetadata, v: &Verification) -> Vec<Value> {
    let files = scan_scope_files(project_dir, &doc.scope_paths);
    let (impl_defs, test_defs) = scan_definitions(project_dir, &files);

    v.items
        .iter()
        .map(|item| {
            let heading = item.label.clone().unwrap_or_else(|| item.heading.clone());
            let keywords = heading_keywords(&heading);

            let suggested_impl_refs = match_definitions(&impl_defs, &keywords);
            let suggested_test_refs = match_definitions(&test_defs, &keywords);

            json!({
                "fragment_seq": item.fragment_seq,
                "heading": item.heading,
                "suggested_impl_refs": suggested_impl_refs,
                "suggested_test_refs": suggested_test_refs,
            })
        })
        .collect()
}

/// Recursively collects every file under `project_dir` whose relative path
/// starts with one of `scope_paths` (prefix match, spec: "Look for files
/// matching scope_paths patterns") and whose extension is in
/// [`SUGGEST_REFS_EXTENSIONS`]. `scope_paths` entries are relative to
/// `project_dir` (e.g. `src/mcp/handlers/`), matching how `scope_paths` is
/// documented and used elsewhere (BM25 scope bonus, shared-scope graph
/// edges).
fn scan_scope_files(project_dir: &Path, scope_paths: &[String]) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for scope in scope_paths {
        let root = project_dir.join(scope);
        walk_dir(&root, &mut out);
    }
    out
}

fn walk_dir(path: &Path, out: &mut Vec<std::path::PathBuf>) {
    if path.is_file() {
        if has_suggest_refs_extension(path) {
            out.push(path.to_path_buf());
        }
        return;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk_dir(&p, out);
        } else if has_suggest_refs_extension(&p) {
            out.push(p);
        }
    }
}

fn has_suggest_refs_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| SUGGEST_REFS_EXTENSIONS.contains(&ext))
}

/// Splits `files` into impl definitions (fn/struct/impl/mod) and test
/// definitions (files under a `tests/` dir, named `*_test.*`/`test_*`, or
/// containing `#[test]`/`#[cfg(test)]`-marked functions), per-file, by
/// scanning each line with the heuristics from the task spec.
fn scan_definitions(
    project_dir: &Path,
    files: &[std::path::PathBuf],
) -> (Vec<ScannedDefinition>, Vec<ScannedDefinition>) {
    let mut impl_defs = Vec::new();
    let mut test_defs = Vec::new();

    for file in files {
        let Ok(content) = std::fs::read_to_string(file) else {
            continue;
        };
        let rel_path = file
            .strip_prefix(project_dir)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
        let is_test_file = is_test_path(&rel_path);

        let mut next_is_test_fn = false;
        for (idx, line) in content.lines().enumerate() {
            let trimmed = line.trim_start();
            let line_no = idx + 1;

            if trimmed.starts_with("#[test]") || trimmed.starts_with("#[cfg(test)]") {
                next_is_test_fn = true;
                continue;
            }

            if let Some(name) = extract_test_fn_name(trimmed) {
                if is_test_file || next_is_test_fn {
                    test_defs.push(ScannedDefinition {
                        rel_path: rel_path.clone(),
                        name,
                        line: line_no,
                    });
                }
                next_is_test_fn = false;
                continue;
            }
            next_is_test_fn = false;

            if let Some(name) = extract_impl_def_name(trimmed) {
                let bucket = if is_test_file {
                    &mut test_defs
                } else {
                    &mut impl_defs
                };
                bucket.push(ScannedDefinition {
                    rel_path: rel_path.clone(),
                    name,
                    line: line_no,
                });
            }
        }
    }

    (impl_defs, test_defs)
}

fn is_test_path(rel_path: &str) -> bool {
    let lower = rel_path.to_ascii_lowercase();
    lower.split('/').any(|seg| seg == "tests" || seg == "test")
        || lower.contains("_test.")
        || lower.contains("/test_")
        || lower.starts_with("test_")
}

/// Recognizes `fn test_*` / `def test_*` test-function definitions
/// (spec: "`fn test_`") regardless of visibility/async modifiers.
fn extract_test_fn_name(trimmed: &str) -> Option<String> {
    for prefix in ["pub async fn ", "pub fn ", "async fn ", "fn ", "def "] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            if rest.trim_start().starts_with("test_") {
                return extract_identifier(rest);
            }
        }
    }
    None
}

/// Recognizes impl-style definitions the spec calls out: `fn `, `pub fn `,
/// `struct `, `impl `, `mod `.
fn extract_impl_def_name(trimmed: &str) -> Option<String> {
    for prefix in [
        "pub async fn ",
        "pub fn ",
        "async fn ",
        "fn ",
        "pub struct ",
        "struct ",
        "pub mod ",
        "mod ",
        "impl ",
    ] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return extract_identifier(rest);
        }
    }
    None
}

/// Pulls the leading identifier (`[A-Za-z0-9_]+`) off the start of `rest`,
/// e.g. `"foo(bar: &str) {"` -> `"foo"`, `"Foo<T> for Bar"` -> `"Foo"`.
fn extract_identifier(rest: &str) -> Option<String> {
    let ident: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if ident.is_empty() {
        None
    } else {
        Some(ident)
    }
}

/// Splits a heading into lowercase keyword tokens for the fuzzy match
/// (spec: "case-insensitive substring match"), dropping short/common words
/// that would otherwise match almost every identifier.
fn heading_keywords(heading: &str) -> Vec<String> {
    const STOPWORDS: &[&str] = &["the", "a", "an", "of", "to", "and", "or", "for", "in", "on"];
    heading
        .split(|c: char| !c.is_alphanumeric())
        .map(|w| w.to_ascii_lowercase())
        .filter(|w| w.len() > 2 && !STOPWORDS.contains(&w.as_str()))
        .collect()
}

/// Matches `defs` whose `name` (case-insensitive) contains any of
/// `keywords` as a substring, deduplicates by `(path, name)`, and caps the
/// result at [`SUGGEST_REFS_MAX_PER_ITEM`].
fn match_definitions(defs: &[ScannedDefinition], keywords: &[String]) -> Vec<Value> {
    if keywords.is_empty() {
        return Vec::new();
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for def in defs {
        let name_lower = def.name.to_ascii_lowercase();
        let matches = keywords.iter().any(|kw| name_lower.contains(kw.as_str()));
        if !matches {
            continue;
        }
        let key = (def.rel_path.clone(), def.name.clone());
        if !seen.insert(key) {
            continue;
        }
        out.push(json!({
            "path": def.rel_path,
            "lines": def.line.to_string(),
            "label": def.name,
        }));
        if out.len() >= SUGGEST_REFS_MAX_PER_ITEM {
            break;
        }
    }
    out
}

/// Summary counts used by both `doc_verify`'s mutation response and
/// `doc_verify_status`'s `progress` block.
struct VerificationCounts {
    checked: usize,
    skipped: usize,
    pending: usize,
    total: usize,
    stale: usize,
}

/// v2 (§7.4): counts every leaf verification unit — a top-level item that
/// has no `sub_items` counts itself directly (v1 behavior, includes freeform
/// items), while an item that *does* have `sub_items` counts each sub_item
/// instead of itself (so `total` is "top-level item count (leaf-only) + all
/// sub_items count", matching the spec's "トップレベル + 全 sub_items の合計").
fn count_verification(doc: &DocMetadata, v: &Verification) -> VerificationCounts {
    let mut checked = 0;
    let mut skipped = 0;
    let mut pending = 0;
    let mut stale = 0;

    for item in &v.items {
        if item.sub_items.is_empty() {
            match item.status.as_str() {
                "verified" => checked += 1,
                "skipped" => skipped += 1,
                _ => pending += 1,
            }
        } else {
            for sub in &item.sub_items {
                match sub.status.as_str() {
                    "verified" => checked += 1,
                    "skipped" => skipped += 1,
                    _ => pending += 1,
                }
            }
        }
        if item_is_stale(doc, item) {
            stale += 1;
        }
    }

    VerificationCounts {
        checked,
        skipped,
        pending,
        total: checked + skipped + pending,
        stale,
    }
}

/// An item is stale when it was verified at a content_hash that no longer
/// matches its section's current content_hash (spec §3.5) — items never
/// verified (`content_hash_at_verify: None`) are never stale, items whose
/// section has been removed (sync should have dropped them, but be
/// defensive) are treated as stale so drift is never silently hidden, and
/// freeform items (v2, `fragment_seq: None`) are never stale since they are
/// not tied to any section's content_hash.
fn item_is_stale(doc: &DocMetadata, item: &VerificationItem) -> bool {
    let Some(hash_at_verify) = &item.content_hash_at_verify else {
        return false;
    };
    let Some(fragment_seq) = item.fragment_seq else {
        return false;
    };
    match doc.sections.iter().find(|s| s.seq == fragment_seq) {
        Some(section) => section.content_hash.as_deref() != Some(hash_at_verify.as_str()),
        None => true,
    }
}

/// Cross-document requirement progress, keyed by `SubItem.priority`
/// (requirements-traceability P0 §4.1 output shape, reused verbatim as the
/// `_requirements_summary.json` cache written by [`write_requirements_summary`]).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub(crate) struct PrioritySummary {
    pub(crate) total: usize,
    pub(crate) implemented: usize,
    pub(crate) tested: usize,
    pub(crate) verified: usize,
}

/// Cross-document requirement progress, keyed by the `C{n}` prefix of
/// `SubItem.stable_id` (P0 §4.1).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub(crate) struct CategorySummary {
    pub(crate) total: usize,
    pub(crate) implemented: usize,
    pub(crate) coverage_pct: f64,
}

/// Percent of requirements at each `dev_stage` milestone
/// (`implemented` ⊇ `tested` ⊇ `verified`), across every SubItem counted
/// into a [`RequirementsSummary`] (P0 §4.1 `coverage` block).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub(crate) struct CoverageSummary {
    pub(crate) impl_pct: f64,
    pub(crate) test_pct: f64,
    pub(crate) verified_pct: f64,
}

/// Per-task requirement progress (requirements-traceability integration
/// reform §3.2): how many SubItems linked to a given task id are at each
/// `dev_stage`, keyed by that `dev_stage` value (e.g. `"not_started"`,
/// `"implemented"`). A SubItem with no `dev_stage` set counts under
/// [`UNSET_DEV_STAGE`], matching [`aggregate_requirements`]'s own fallback.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub(crate) struct TaskCoverageSummary {
    pub(crate) total: usize,
    #[serde(flatten)]
    pub(crate) by_dev_stage: std::collections::HashMap<String, usize>,
}

/// One flattened requirement in the `items` array of
/// [`RequirementsSummary`], carrying enough data for the VSCode extension
/// to render per-item tables/explorers without re-reading every
/// `_doc.*.md` frontmatter individually.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) struct SummaryRequirementItem {
    pub(crate) stable_id: String,
    pub(crate) title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) priority: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dev_stage: Option<String>,
    pub(crate) verification_status: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) impl_refs: Vec<crate::storage::docs::model::CodeRef>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) test_refs: Vec<crate::storage::docs::model::CodeRef>,
    pub(crate) doc_id: String,
    pub(crate) doc_slug: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) fragment_seq: Option<usize>,
    pub(crate) sub_item_index: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) task_ids: Vec<String>,
    /// `SubItem.category` (wiki/220-vmodel-integration-design.md §2.3, M1
    /// t360.6): `"check"` for a right-side layer item (excluded from every
    /// requirement-count aggregate above — `total`/`by_status`/
    /// `by_priority`/`by_category`/`coverage`/`task_coverage` — since it is
    /// a verification item, not a requirement), `"requirement"` (the
    /// pre-M1 default) or another free-extensible value otherwise.
    pub(crate) category: String,
    /// Effective layer (`sub.layer.or(doc.layer)`, §2.3) this item lives on.
    /// `None` for a non-layer item/document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) layer: Option<String>,
}

/// Cross-document requirement (`SubItem`) aggregate — the same shape
/// `handoff_doc_req_status` (P1 §4.1, `docs_query::handle_doc_req_status`)
/// returns, and what [`write_requirements_summary`] persists to
/// `.handoff/docs/_requirements_summary.json` for the VSCode extension
/// (P0 §2.7, §3.4). `task_coverage` (integration-reform §3.2) is keyed by
/// task id, one entry per task referenced by at least one SubItem's
/// `task_ids`. `items` carries every individual requirement so the VSCode
/// extension can render per-item views without re-reading doc frontmatter.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub(crate) struct RequirementsSummary {
    pub(crate) total: usize,
    pub(crate) by_status: std::collections::HashMap<String, usize>,
    pub(crate) by_priority: std::collections::HashMap<String, PrioritySummary>,
    pub(crate) by_category: std::collections::HashMap<String, CategorySummary>,
    pub(crate) coverage: CoverageSummary,
    pub(crate) task_coverage: std::collections::HashMap<String, TaskCoverageSummary>,
    pub(crate) items: Vec<SummaryRequirementItem>,
}

/// `dev_stage` fallback for a `SubItem` that has never had one set (P0
/// §3.4 "重要": `dev_stage` が `None` の場合は `"not_started"` としてカウント).
pub(crate) const UNSET_DEV_STAGE: &str = "not_started";

/// `priority` fallback for a `SubItem` that has no priority assigned yet
/// (P0 §3.4 "重要": `priority` が `None` の場合は `"unset"` としてカウント).
pub(crate) const UNSET_PRIORITY: &str = "unset";

/// Extracts the `C{n}` category prefix from a `stable_id` (e.g.
/// `"C01-2.1.1.1"` -> `"C01"`), per P0 §3.4 ("category は stable_id の接頭辞
/// (C01, C07 等) から抽出"). A `stable_id` with no `-` (or no id at all) has
/// no category and is excluded from `by_category` — there is nothing
/// meaningful to bucket it under.
pub(crate) fn category_prefix_from_stable_id(stable_id: &str) -> Option<&str> {
    stable_id.split('-').next().filter(|s| !s.is_empty())
}

/// Walks every `DocMetadata.verification.items[].sub_items[]` across `docs`
/// and aggregates requirement-level progress (P0 §3.4 / §4.1). Only
/// `SubItem`s count as "requirements" here — top-level `VerificationItem`s
/// without `sub_items` track section-review state, not individual
/// requirements, so they are not part of this aggregate.
pub(crate) fn aggregate_requirements(docs: &[DocMetadata]) -> RequirementsSummary {
    let mut summary = RequirementsSummary::default();
    let mut impl_count = 0usize;
    let mut test_count = 0usize;
    let mut verified_count = 0usize;

    for doc in docs {
        let Some(v) = &doc.verification else {
            continue;
        };
        for item in &v.items {
            for sub in &item.sub_items {
                let status = sub.dev_stage.as_deref().unwrap_or(UNSET_DEV_STAGE);

                // wiki/220 §2.3/M1 t360.6: a `category == "check"` SubItem
                // is a verification item (right-side layer body item), not
                // a requirement — it is listed in `items` (with its
                // `layer`/`category`) but excluded from every count above.
                let is_check = sub.category == "check";
                if !is_check {
                    summary.total += 1;
                    *summary.by_status.entry(status.to_string()).or_insert(0) += 1;

                    let priority = sub.priority.as_deref().unwrap_or(UNSET_PRIORITY);
                    let p = summary.by_priority.entry(priority.to_string()).or_default();
                    p.total += 1;

                    let is_impl = matches!(status, "implemented" | "tested" | "verified");
                    let is_tested = matches!(status, "tested" | "verified");
                    let is_verified = status == "verified";
                    if is_impl {
                        impl_count += 1;
                        p.implemented += 1;
                    }
                    if is_tested {
                        test_count += 1;
                        p.tested += 1;
                    }
                    if is_verified {
                        verified_count += 1;
                        p.verified += 1;
                    }

                    if let Some(category) = sub
                        .stable_id
                        .as_deref()
                        .and_then(category_prefix_from_stable_id)
                    {
                        let c = summary.by_category.entry(category.to_string()).or_default();
                        c.total += 1;
                        if is_impl {
                            c.implemented += 1;
                        }
                    }

                    for task_id in &sub.task_ids {
                        let t = summary.task_coverage.entry(task_id.clone()).or_default();
                        t.total += 1;
                        *t.by_dev_stage.entry(status.to_string()).or_insert(0) += 1;
                    }
                }

                summary.items.push(SummaryRequirementItem {
                    stable_id: sub.stable_id.clone().unwrap_or_default(),
                    title: sub.description.clone(),
                    priority: sub.priority.clone(),
                    dev_stage: sub.dev_stage.clone(),
                    verification_status: sub.status.clone(),
                    impl_refs: sub.impl_refs.clone(),
                    test_refs: sub.test_refs.clone(),
                    doc_id: doc.id.clone(),
                    doc_slug: doc.slug.clone(),
                    fragment_seq: item.fragment_seq,
                    sub_item_index: sub.index,
                    task_ids: sub.task_ids.clone(),
                    category: sub.category.clone(),
                    layer: sub.layer.clone().or_else(|| doc.layer.clone()),
                });
            }
        }
    }

    for c in summary.by_category.values_mut() {
        c.coverage_pct = if c.total == 0 {
            0.0
        } else {
            (c.implemented as f64 / c.total as f64) * 100.0
        };
    }

    let total = summary.total;
    summary.coverage = CoverageSummary {
        impl_pct: percent(impl_count, total),
        test_pct: percent(test_count, total),
        verified_pct: percent(verified_count, total),
    };

    summary
}

fn percent(count: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        (count as f64 / total as f64) * 100.0
    }
}

/// Input fingerprint recorded alongside every derived file this task's write
/// discipline applies to (`_requirements_summary.json` here; `t360.13`'s
/// `_trace_report.json` reuses [`compute_derived_inputs`] verbatim) — wiki/220
/// §4.3 r3, wiki/240-performance-design.md §4 P-M4.
///
/// Once a derived file is only rewritten when its *content* changes (P-M4),
/// the file's own mtime stops being a valid freshness signal — it can lag
/// arbitrarily far behind the last time an *input* changed. `inputs` is the
/// replacement: a cheap-to-recompute (`stat` only, no file content read)
/// summary of every input this file's content depends on. A reader (MCP
/// itself, or the VSCode extension) recomputes the same fingerprint from the
/// current filesystem state and compares — equal means "still fresh",
/// different means "stale, recompute".
///
/// - `docs_*`: every `_doc.*.md` in `docs/` (the same filter
///   [`crate::storage::docs::read_all_docs`] uses — this excludes
///   `_requirements_summary.json` itself and any other derived file, since
///   none of them match the `_doc.*.md` pattern).
/// - `tasks_*`: every `_task.<status>.json` anywhere under `tasks/`
///   (recursive — child tasks live in nested directories). The reverse
///   `task_ids` link a `SubItem` carries has the task side as its source of
///   truth (D3), so a task-only edit (e.g. `dev_stage` propagation) must
///   also be able to invalidate this fingerprint.
/// - `runs_*`: every file under `runs/` (month subdirectories included)
///   except `_latest.json`. `runs/` is created by M1 (t360.8) and does not
///   exist yet, so a missing directory reports `runs_count: 0`,
///   `runs_max_id: None` rather than erroring — there is nothing to be
///   stale relative to yet.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct DerivedInputs {
    pub(crate) docs_max_mtime_ns: u64,
    pub(crate) docs_count: usize,
    pub(crate) tasks_max_mtime_ns: u64,
    pub(crate) tasks_count: usize,
    pub(crate) runs_count: usize,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub(crate) runs_max_id: Option<String>,
}

fn mtime_ns(meta: &std::fs::Metadata) -> Result<u64> {
    let modified = meta
        .modified()
        .context("file mtime unsupported on this platform")?;
    Ok(modified
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64)
}

fn stat_docs_input(handoff_dir: &Path) -> Result<(u64, usize)> {
    let dir = docs_dir(handoff_dir);
    if !dir.exists() {
        return Ok((0, 0));
    }
    let mut max_ns = 0u64;
    let mut count = 0usize;
    for entry in std::fs::read_dir(&dir)
        .with_context(|| format!("Failed to read docs dir: {}", dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("_doc.") || !name.ends_with(".md") {
            continue;
        }
        max_ns = max_ns.max(mtime_ns(&entry.metadata()?)?);
        count += 1;
    }
    Ok((max_ns, count))
}

/// Below this many top-level `tasks/<id>` subtrees, spawning worker
/// threads costs more than it saves (S scale has ~20-25 top-level dirs;
/// thread-spawn overhead would dominate at that size) — the plain
/// sequential walk stays faster below this floor and is used instead.
const STAT_TASKS_PARALLEL_MIN_TOP_LEVEL_DIRS: usize = 16;

fn stat_tasks_input(tasks_dir: &Path) -> Result<(u64, usize)> {
    if !tasks_dir.exists() {
        return Ok((0, 0));
    }

    // `tasks/<id>` subtrees are independent, so the per-file `stat`
    // syscalls `stat_tasks_input_recursive` issues underneath each one —
    // the dominant cost of this function at L scale (measured ~21ms for
    // 3,000 tasks, ~6,000 total `readdir`/`stat` syscalls) — parallelize
    // cleanly across them (t370.11, wiki/240-performance-design.md §4 P-M4
    // follow-up: this was identified as the second contributor to the
    // `update_task_status_with_links` regression, alongside
    // `write_requirements_summary`'s full-file re-read). A single top-level
    // `read_dir` first splits `tasks_dir` into its immediate subdirectories
    // (each subtree handed to a worker) and any `_task.*.json` files that
    // sit directly in `tasks_dir` itself (a childless task at the root —
    // stat'd on the calling thread, since there is no subtree to hand off).
    let mut top_level_dirs = Vec::new();
    let mut max_ns = 0u64;
    let mut count = 0usize;
    for entry in std::fs::read_dir(tasks_dir)
        .with_context(|| format!("Failed to read dir: {}", tasks_dir.display()))?
    {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let name = entry.file_name().to_string_lossy().to_string();
        if file_type.is_dir() {
            if !name.starts_with('.') {
                top_level_dirs.push(entry.path());
            }
        } else if file_type.is_file() && name.starts_with("_task.") && name.ends_with(".json") {
            max_ns = max_ns.max(mtime_ns(&entry.metadata()?)?);
            count += 1;
        }
    }

    let workers = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
        .min(top_level_dirs.len());
    if top_level_dirs.len() < STAT_TASKS_PARALLEL_MIN_TOP_LEVEL_DIRS || workers <= 1 {
        for dir in &top_level_dirs {
            stat_tasks_input_recursive(dir, &mut max_ns, &mut count)?;
        }
        return Ok((max_ns, count));
    }

    let chunk_size = top_level_dirs.len().div_ceil(workers).max(1);
    let chunk_results: Vec<Result<(u64, usize)>> = std::thread::scope(|scope| {
        let handles: Vec<_> = top_level_dirs
            .chunks(chunk_size)
            .map(|chunk| {
                scope.spawn(move || {
                    let mut chunk_max_ns = 0u64;
                    let mut chunk_count = 0usize;
                    for dir in chunk {
                        stat_tasks_input_recursive(dir, &mut chunk_max_ns, &mut chunk_count)?;
                    }
                    Ok((chunk_max_ns, chunk_count))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join().unwrap_or_else(|_| {
                    Err(anyhow::anyhow!("tasks input stat worker thread panicked"))
                })
            })
            .collect()
    });
    for chunk_result in chunk_results {
        let (chunk_max_ns, chunk_count) = chunk_result?;
        max_ns = max_ns.max(chunk_max_ns);
        count += chunk_count;
    }
    Ok((max_ns, count))
}

fn stat_tasks_input_recursive(dir: &Path, max_ns: &mut u64, count: &mut usize) -> Result<()> {
    for entry in
        std::fs::read_dir(dir).with_context(|| format!("Failed to read dir: {}", dir.display()))?
    {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let name = entry.file_name().to_string_lossy().to_string();
        if file_type.is_dir() {
            if name.starts_with('.') {
                continue;
            }
            stat_tasks_input_recursive(&entry.path(), max_ns, count)?;
        } else if file_type.is_file() && name.starts_with("_task.") && name.ends_with(".json") {
            *max_ns = (*max_ns).max(mtime_ns(&entry.metadata()?)?);
            *count += 1;
        }
    }
    Ok(())
}

fn stat_runs_input(runs_dir: &Path) -> Result<(usize, Option<String>)> {
    if !runs_dir.exists() {
        return Ok((0, None));
    }
    let mut count = 0usize;
    let mut max_name: Option<String> = None;
    stat_runs_input_recursive(runs_dir, &mut count, &mut max_name)?;
    Ok((count, max_name))
}

fn stat_runs_input_recursive(
    dir: &Path,
    count: &mut usize,
    max_name: &mut Option<String>,
) -> Result<()> {
    for entry in
        std::fs::read_dir(dir).with_context(|| format!("Failed to read dir: {}", dir.display()))?
    {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let name = entry.file_name().to_string_lossy().to_string();
        if file_type.is_dir() {
            stat_runs_input_recursive(&entry.path(), count, max_name)?;
        } else if file_type.is_file() && name != "_latest.json" {
            *count += 1;
            if max_name.as_deref().is_none_or(|m| name.as_str() > m) {
                *max_name = Some(name);
            }
        }
    }
    Ok(())
}

/// Computes [`DerivedInputs`] from the current `.handoff/` filesystem state
/// — `stat` only, no file contents read (PR-1's ≤ 50 ms `update_task` budget
/// must not regress). Shared by every derived-file writer that needs this
/// fingerprint (`write_requirements_summary` here, `t360.13`'s
/// `_trace_report.json` writer later).
pub(crate) fn compute_derived_inputs(handoff_dir: &Path) -> Result<DerivedInputs> {
    let (docs_max_mtime_ns, docs_count) = stat_docs_input(handoff_dir)?;
    let (tasks_max_mtime_ns, tasks_count) = stat_tasks_input(&handoff_dir.join("tasks"))?;
    let (runs_count, runs_max_id) = stat_runs_input(&handoff_dir.join("runs"))?;
    Ok(DerivedInputs {
        docs_max_mtime_ns,
        docs_count,
        tasks_max_mtime_ns,
        tasks_count,
        runs_count,
        runs_max_id,
    })
}

/// On-disk shape of `_requirements_summary.json`: the pre-existing
/// [`RequirementsSummary`] fields flattened at the top level (unchanged, so
/// old readers/fixtures keep working), plus the new `inputs` fingerprint
/// (wiki/220 §4.3 r3).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
struct PersistedRequirementsSummary {
    #[serde(flatten)]
    summary: RequirementsSummary,
    inputs: DerivedInputs,
}

/// Filesystem stamp used to validate [`SUMMARY_WRITE_CACHE`]'s cached value
/// without re-reading `_requirements_summary.json`'s ~0.7-1MB body (t370.11:
/// mirrors `storage::docs::mod.rs`'s `DocCacheStamp` — same `(len,
/// mtime_ns)` shape, same "stat matches => trust the cached parse" logic,
/// applied here to the *write*-side change check instead of a read path).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SummaryCacheStamp {
    len: u64,
    mtime_ns: u64,
}

fn summary_cache_stamp(path: &Path) -> Option<SummaryCacheStamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some(SummaryCacheStamp {
        len: meta.len(),
        mtime_ns: mtime_ns(&meta).ok()?,
    })
}

/// In-process cache of the `_requirements_summary.json` content this
/// process itself last wrote or observed, keyed by the file's absolute path
/// (t370.11, wiki/240-performance-design.md §4 P-M4 follow-up).
///
/// Before this cache existed, `write_requirements_summary`'s "skip the
/// write when nothing changed" check (added by t370.4) had to `fs::read`
/// the existing ~0.7-1MB file on *every* call to know what was already on
/// disk — turning a change-detection optimization into the dominant I/O and
/// latency cost of `update_task_status_with_links` at L scale (rchar 115KB
/// -> 829KB, p50 17ms -> 50ms). Since the common case is the *same server
/// process* repeatedly calling this function for the same project (many
/// requests in one session), the content it last wrote/observed is already
/// sitting in memory — re-reading it from disk to compare is redundant
/// whenever the file's `(len, mtime_ns)` still matches what this process
/// left behind. Only a stat mismatch (first call for this path in this
/// process, or genuine external modification) falls back to a full read.
///
/// t370.11 round 2 (rework, reviewer MAJOR on the 17ms -> 50ms -> 27ms
/// residual gap): the value cached here is the *native*
/// [`PersistedRequirementsSummary`] struct, not a `serde_json::Value`. Round
/// 1 still paid for `serde_json::to_value(&persisted)` (building a full
/// `Value` tree — its own heap-allocation-heavy walk of every string/number/
/// map in the ~0.7-1MB aggregate) on *every single call*, plus a second full
/// `to_string` pass when a write was needed, and compared via `Value::eq`
/// (also a full tree walk). Caching the native struct means the hot,
/// same-process, stat-matches path now does a single derived `PartialEq`
/// comparison directly over Rust values — no JSON tree construction, no
/// (de)serialization, no hashing — and pays the cost of `to_string` only
/// once, and only when a write is actually about to happen.
static SUMMARY_WRITE_CACHE: OnceLock<
    Mutex<HashMap<PathBuf, (SummaryCacheStamp, PersistedRequirementsSummary)>>,
> = OnceLock::new();

fn summary_write_cache(
) -> &'static Mutex<HashMap<PathBuf, (SummaryCacheStamp, PersistedRequirementsSummary)>> {
    SUMMARY_WRITE_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Test-only counter of how many times [`write_requirements_summary`] fell
/// back to a full `fs::read` of the existing file (cache miss or stat
/// mismatch) — lets tests assert the cache actually avoids the read on
/// repeat calls, rather than only asserting the externally-visible
/// skip-write behavior (which would pass even if the read still happened).
#[cfg(test)]
static SUMMARY_READ_FALLBACK_COUNTS: OnceLock<Mutex<HashMap<PathBuf, usize>>> = OnceLock::new();

#[cfg(test)]
fn record_summary_read_fallback(path: &Path) {
    *SUMMARY_READ_FALLBACK_COUNTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("summary read fallback counts poisoned")
        .entry(path.to_path_buf())
        .or_insert(0) += 1;
}

#[cfg(test)]
pub(crate) fn summary_read_fallback_count(path: &Path) -> usize {
    SUMMARY_READ_FALLBACK_COUNTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("summary read fallback counts poisoned")
        .get(path)
        .copied()
        .unwrap_or(0)
}

/// Test-only counter of how many times [`write_requirements_summary`] built
/// a `serde_json::Value` tree from a [`PersistedRequirementsSummary`] to
/// compare against an externally-read file (t370.11 round 2) — proves the
/// hot, same-process, stat-matches path never does this (it compares the
/// cached native struct directly via `PartialEq`), distinct from
/// [`SUMMARY_READ_FALLBACK_COUNTS`] which only proves the disk `fs::read`
/// itself is skipped.
#[cfg(test)]
static SUMMARY_COMPARE_SERIALIZE_COUNTS: OnceLock<Mutex<HashMap<PathBuf, usize>>> = OnceLock::new();

#[cfg(test)]
fn record_summary_compare_serialize(path: &Path) {
    *SUMMARY_COMPARE_SERIALIZE_COUNTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("summary compare serialize counts poisoned")
        .entry(path.to_path_buf())
        .or_insert(0) += 1;
}

#[cfg(test)]
pub(crate) fn summary_compare_serialize_count(path: &Path) -> usize {
    SUMMARY_COMPARE_SERIALIZE_COUNTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("summary compare serialize counts poisoned")
        .get(path)
        .copied()
        .unwrap_or(0)
}

/// Test/measurement-only instrumentation (t370.14, wiki/240-performance-design.md
/// §6 PR-8: derived files must be written "at most 1 file, 1 write per
/// request"). An external test process spawning this binary over stdio has
/// no way to see this process's in-memory counters, so when
/// `HANDOFF_MCP_DERIVED_WRITE_LOG` is set to a file path, every *actual*
/// (non-skipped) derived-file write appends one `"{path}\t{bytes}\n"` line
/// to it — letting a test bracket a single request's log growth to assert
/// the discipline directly, and read the exact byte count instead of
/// approximating it via a `stat` before/after (`tests/perf_budget.rs`'s
/// `measure_wchar_split`, `tests/derived_summary_write_discipline.rs`).
/// A no-op (one extra `env::var` lookup, negligible next to the write it
/// accompanies) whenever the env var is unset, i.e. always in normal
/// operation — this never changes production behavior.
pub(crate) fn record_derived_write_for_test(path: &Path, bytes_written: usize) {
    let Ok(log_path) = std::env::var("HANDOFF_MCP_DERIVED_WRITE_LOG") else {
        return;
    };
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let _ = writeln!(f, "{}\t{bytes_written}", path.display());
    }
}

/// Writes `.handoff/docs/_requirements_summary.json` for the VSCode
/// extension (P0 §2.7 — the extension never calls MCP tools, it only reads
/// `.handoff/` files directly). Called after every `handoff_doc_verify`
/// mutation that can affect requirement progress (P0 §3.4), and after every
/// read-only `handoff_doc_req_status` call (P0 §4.1 side effect) — the
/// P-M4 write-discipline check below (wiki/240 §4) is what keeps the latter
/// from rewriting the file on every single read.
///
/// When `docs` has no `SubItem`s to aggregate (no docs at all, or every doc
/// has no verification matrix / no sub_items), no file is written — an
/// empty summary file would be indistinguishable from "not yet computed"
/// to a FileWatcher-based reader, so we simply leave it absent (P0 §3.4). If
/// a summary file from an earlier call (when requirements still existed)
/// is present, it is deleted (FR-905, wiki/220 §4.3: "MCP は SubItem が 0
/// 件になったとき summary ファイルを削除する（古い summary の残留防止）") —
/// otherwise a FileWatcher-based reader (the VSCode extension) would keep
/// showing long-gone requirements after e.g. the owning document is
/// deleted or its matrix is synced down to nothing.
///
/// P-M4 (wiki/240 §4): the file is unformatted (compact) JSON, and is only
/// actually rewritten when its content — aggregate *or* [`DerivedInputs`]
/// fingerprint — differs from what is already on disk. The fingerprint is
/// computed **after** the caller has finished writing whatever documents/
/// tasks this request touched (every call site here already reads `docs`
/// fresh right before calling this), so it reflects the post-write state,
/// not a stale pre-write one.
///
/// t370.11: the change check itself must not re-read the existing file's
/// full body on every call (see [`SUMMARY_WRITE_CACHE`]'s doc comment) — it
/// only falls back to `fs::read` when this process has no cached stamp for
/// `path`, or the file's current `(len, mtime_ns)` no longer matches what
/// this process last wrote/observed (first call, or externally modified).
/// Round 2 (rework): the fast path also never builds a `serde_json::Value`
/// or serializes anything at all — it compares the cached native
/// [`PersistedRequirementsSummary`] against the freshly computed one via
/// `PartialEq`. A `serde_json` pass only happens in the rare slow path
/// (below), for comparison against externally-written bytes, and exactly
/// once more (`to_string`) when a write actually happens.
pub(crate) fn write_requirements_summary(handoff_dir: &Path, docs: &[DocMetadata]) -> Result<()> {
    let summary = aggregate_requirements(docs);
    let path = docs_dir(handoff_dir).join("_requirements_summary.json");
    if summary.total == 0 {
        // No `exists()` pre-check: another server sharing this `.handoff/`
        // may delete it concurrently, so an already-absent file is the
        // desired end state, not an error.
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).context("failed to remove stale _requirements_summary.json"),
        }
        summary_write_cache()
            .lock()
            .expect("summary write cache poisoned")
            .remove(&path);
        return Ok(());
    }
    let inputs = compute_derived_inputs(handoff_dir)?;
    let persisted = PersistedRequirementsSummary { summary, inputs };

    // Fast path: this process's own cached stamp+value for `path`, iff the
    // file's current stat still matches it (P-M4, t370.11 — see
    // `SUMMARY_WRITE_CACHE`'s doc comment for why this is safe: a same-
    // process write always updates the cache below, so a stat match means
    // "no one else touched this file since", and the cached struct is
    // exactly what a full read+reparse would have produced). This compares
    // native Rust values — no JSON tree, no (de)serialization.
    let current_stamp = summary_cache_stamp(&path);
    let cached_unchanged = current_stamp.and_then(|stamp| {
        summary_write_cache()
            .lock()
            .expect("summary write cache poisoned")
            .get(&path)
            .filter(|(cached_stamp, _)| *cached_stamp == stamp)
            .map(|(_, cached_persisted)| *cached_persisted == persisted)
    });

    // P-M4: skip the write entirely when nothing actually changed.
    let unchanged = match cached_unchanged {
        Some(unchanged) => unchanged,
        None => {
            // Cache miss or externally-modified file: fall back to a full
            // read, same as before t370.11 — but this now only happens once
            // per external modification instead of on every call. Compared
            // as parsed `Value`s (not raw bytes) so pre-existing HashMap-
            // keyed fields whose serialized key order is not guaranteed
            // (and an externally written file's own key order, which this
            // process has no control over) do not cause a spurious
            // "changed" verdict.
            #[cfg(test)]
            record_summary_read_fallback(&path);
            match std::fs::read(&path)
                .ok()
                .and_then(|existing_bytes| serde_json::from_slice::<Value>(&existing_bytes).ok())
            {
                Some(existing_value) => {
                    #[cfg(test)]
                    record_summary_compare_serialize(&path);
                    let new_value = serde_json::to_value(&persisted)
                        .context("failed to serialize requirements summary for comparison")?;
                    let matches = existing_value == new_value;
                    if matches {
                        // Seed the cache with what was just observed so the
                        // next unchanged call takes the fast path instead of
                        // re-reading the file every time until some
                        // unrelated write happens (cold start after a
                        // server restart, or an external writer that
                        // produced identical content). `current_stamp` was
                        // taken *before* the read, so a concurrent rewrite
                        // between stat and read only makes the next call's
                        // stamp mismatch — never a stale cache hit.
                        if let Some(stamp) = current_stamp {
                            summary_write_cache()
                                .lock()
                                .expect("summary write cache poisoned")
                                .insert(path, (stamp, persisted));
                        }
                        return Ok(());
                    }
                    false
                }
                None => false,
            }
        }
    };
    if unchanged {
        return Ok(());
    }

    ensure_docs_dir(handoff_dir)?;
    let body =
        serde_json::to_string(&persisted).context("failed to serialize requirements summary")?;
    crate::storage::atomic_write(&path, body.as_bytes())
        .context("failed to write _requirements_summary.json")?;
    record_derived_write_for_test(&path, body.len());

    // Record what we just wrote so the next call in this process can skip
    // both the read and any (de)serialization.
    if let Some(new_stamp) = summary_cache_stamp(&path) {
        summary_write_cache()
            .lock()
            .expect("summary write cache poisoned")
            .insert(path, (new_stamp, persisted));
    }
    Ok(())
}

/// One `SubItem` resolved by `stable_id` (t330.1), identifying exactly where
/// it lives so a caller can mutate it without re-scanning every doc.
/// `fragment_seq` is `Option<usize>` (FR-806 §4.1): freeform `SubItem`s
/// (v2, `VerificationItem.fragment_seq: None`) are addressable by
/// `stable_id` too — they just have no section to index by, so callers use
/// [`resolved_sub_item_mut`] rather than assuming `Some(seq)`.
#[derive(Debug)]
pub(crate) struct ResolvedSubItem {
    pub(crate) doc_id: String,
    /// The owning document's file-naming slug (P-M2, wiki/240 §4): resolved
    /// once here from the same document scan that finds the `SubItem`
    /// itself, so callers that need to re-locate the document (e.g. a
    /// `DocSet` entry vanishing mid-call) can go straight to it via a
    /// direct slug-keyed `read_doc` instead of a `find_doc_by_id` full-corpus
    /// scan fallback.
    pub(crate) doc_slug: String,
    pub(crate) fragment_seq: Option<usize>,
    pub(crate) sub_item_index: usize,
    pub(crate) stable_id: String,
}

/// Scans `docs` (already loaded — no `read_all_docs` call of its own, see
/// [`resolve_stable_ids`] for the read-from-disk wrapper) for `SubItem`s
/// whose `stable_id` is in `stable_ids`, and resolves each to its
/// `(doc_id, doc_slug, fragment_seq, sub_item_index)` location. `stable_id`s
/// that match no `SubItem` anywhere are returned as `unresolved` (t330.1
/// spec: non-fatal — the caller reports them as warnings rather than failing
/// the whole call).
///
/// FR-806 (§4.1): items with `fragment_seq: None` (freeform, v2) are scanned
/// too, not skipped — before that fix, a `stable_id` that only lived on a
/// freeform `SubItem` (e.g. one `handoff_doc_req_import` created before this
/// task, or created via `handoff_doc_verify(action="add_item")` with no
/// `fragment_seq`) could never be resolved at all.
///
/// M0-b (wiki/220-vmodel-integration-design.md §4.2, FR-105): `stable_id`s
/// are only guaranteed unique *within* a document (`derive_stable_id`'s
/// `existing_ids` collision check is per-document) — nothing prevented two
/// different documents from independently minting or hand-authoring the
/// same id. When a requested `stable_id` matches a `SubItem` in more than
/// one document, which one the caller meant is genuinely ambiguous, so it is
/// reported in the third return value (`ambiguous`) and **not** linked to
/// either — silently picking "whichever document came first in the corpus
/// scan" would make `handoff_update_task(requirement_ids=...)` link to a
/// different document depending on file iteration order, which is exactly
/// the kind of non-deterministic behavior this task exists to prevent.
fn resolve_stable_ids_in(
    docs: &[DocMetadata],
    stable_ids: &[String],
) -> (Vec<ResolvedSubItem>, Vec<String>, Vec<String>) {
    let wanted: std::collections::HashSet<&str> = stable_ids.iter().map(String::as_str).collect();
    let mut matches: std::collections::HashMap<&str, Vec<ResolvedSubItem>> =
        std::collections::HashMap::new();

    for doc in docs {
        let Some(v) = &doc.verification else {
            continue;
        };
        for item in &v.items {
            for sub in &item.sub_items {
                let Some(stable_id) = sub.stable_id.as_deref() else {
                    continue;
                };
                let Some(&wanted_key) = wanted.get(stable_id) else {
                    continue;
                };
                matches
                    .entry(wanted_key)
                    .or_default()
                    .push(ResolvedSubItem {
                        doc_id: doc.id.clone(),
                        doc_slug: doc.slug.clone(),
                        fragment_seq: item.fragment_seq,
                        sub_item_index: sub.index,
                        stable_id: stable_id.to_string(),
                    });
            }
        }
    }

    let mut resolved = Vec::new();
    let mut unresolved = Vec::new();
    let mut ambiguous = Vec::new();
    // Dedupe requested ids (mirrors the pre-M0-b `HashSet`-based `remaining`
    // behavior): a `stable_id` repeated in the input is only ever reported
    // once, in whichever bucket it belongs to.
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for id in stable_ids {
        if !seen.insert(id.as_str()) {
            continue;
        }
        match matches.remove(id.as_str()) {
            None => unresolved.push(id.clone()),
            Some(mut hits) if hits.len() == 1 => resolved.push(hits.pop().unwrap()),
            Some(_) => ambiguous.push(id.clone()),
        }
    }
    (resolved, unresolved, ambiguous)
}

/// Requirements-traceability M0-b (wiki/220-vmodel-integration-design.md
/// §4.2, FR-105): every `stable_id` currently assigned to a `SubItem`
/// anywhere in `docs`, mapped to the ids of every document that assigns it.
/// A `stable_id` mapping to more than one document is a cross-document
/// collision — this function only *reports* it (via the `Vec`'s length);
/// callers decide what to do (`req_import`/`add_item` warn but still create,
/// `resolve_stable_ids_in` treats it as ambiguous and links to neither).
///
/// Takes an already-loaded document slice — a `DocSet`'s `docs()`, or a
/// `read_all_docs` pass a caller already needed for another reason — rather
/// than reading `.handoff` itself, so it never adds a corpus scan of its
/// own on top of whatever the caller already did (wiki/220 §4.2's "全走査を
/// 追加しない"). The later M1 layer-sync pass (t360.6) reuses this same
/// function against its own single `DocSet` load.
pub(crate) fn collect_all_stable_ids(
    docs: &[DocMetadata],
) -> std::collections::HashMap<String, Vec<String>> {
    let mut out: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for doc in docs {
        let Some(v) = &doc.verification else {
            continue;
        };
        for stable_id in collect_stable_ids(v) {
            out.entry(stable_id).or_default().push(doc.id.clone());
        }
    }
    out
}

/// M0-b (wiki/220 §4.2, FR-105): reads the whole corpus once and reports
/// (never refuses) when `stable_id` is already assigned to a `SubItem` in a
/// document other than `own_doc_id`. Shared by `handoff_doc_verify`'s
/// `add_item` action and `handoff_doc_req_import` — the two places that mint
/// or reuse a `stable_id` for a single document without already holding a
/// whole-corpus `DocSet` (contrast `apply_requirement_links`/
/// `propagate_dev_stage_for_task`, which already load one and pass
/// `doc_set.docs()` into `collect_all_stable_ids` directly with no
/// additional read).
pub(crate) fn cross_document_collision_warning(
    handoff: &Path,
    own_doc_id: &str,
    stable_id: &str,
) -> Result<Option<String>> {
    let all_docs = read_all_docs(handoff)?;
    let all_ids = collect_all_stable_ids(&all_docs);
    let Some(owners) = all_ids.get(stable_id) else {
        return Ok(None);
    };
    let other_owners: Vec<&str> = owners
        .iter()
        .map(String::as_str)
        .filter(|id| *id != own_doc_id)
        .collect();
    if other_owners.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!(
        "stable_id {stable_id:?} already exists in other document(s): {} — created anyway, but \
         it will be reported as ambiguous by resolve_stable_ids and not linkable until resolved",
        other_owners.join(", ")
    )))
}

/// Reads every document in `handoff` (one `read_all_docs` pass) and resolves
/// `stable_ids` against it — see [`resolve_stable_ids_in`]. Every production
/// call site holds a loaded [`DocSet`] for the request already (P-M3:
/// `link_requirements_to_task` / `unlink_requirements_from_task` /
/// `propagate_dev_stage_for_task` all call [`resolve_stable_ids_in`]
/// directly so the corpus isn't read twice) — this wrapper survives only as
/// a test convenience for cases that don't need a `DocSet`.
#[cfg(test)]
pub(crate) fn resolve_stable_ids(
    handoff: &Path,
    stable_ids: &[String],
) -> Result<(Vec<ResolvedSubItem>, Vec<String>, Vec<String>)> {
    let docs = read_all_docs(handoff)?;
    Ok(resolve_stable_ids_in(&docs, stable_ids))
}

/// Appends (deduped) a `{target: doc_id, link_type: "requirement", label:
/// stable_id}` entry to `task_id`'s `task_links` for every id in `task_ids`.
/// Shared by `link_task` (which replaces `SubItem.task_ids` but always
/// *appends* the reverse link, since other SubItems may still reference the
/// same task) and `link_requirements_to_task` (t330.1, which appends on both
/// sides). Returns the subset of `task_ids` that could not be resolved to a
/// task directory.
fn add_reverse_task_links(
    handoff: &Path,
    doc_id: &str,
    stable_id: Option<&str>,
    task_ids: &[String],
) -> Result<Vec<String>> {
    let tasks_dir = handoff.join("tasks");
    let mut unresolved: Vec<String> = Vec::new();
    for task_id in task_ids {
        let Some(task_dir) = find_task_dir_by_id(&tasks_dir, task_id)? else {
            unresolved.push(task_id.clone());
            continue;
        };
        read_modify_write_task(&task_dir, |data, status| {
            let already_linked = data.task_links.iter().any(|l| {
                l.target == doc_id
                    && l.link_type == "requirement"
                    && l.label.as_deref() == stable_id
            });
            if !already_linked {
                data.task_links.push(TaskLink {
                    target: doc_id.to_string(),
                    link_type: "requirement".to_string(),
                    label: stable_id.map(str::to_string),
                    ..Default::default()
                });
                data.updated_at = Some(chrono::Utc::now().to_rfc3339());
            }
            Ok(status.to_string())
        })?;
    }
    Ok(unresolved)
}

/// t323/M4: removes the `{target: doc.id, link_type: "requirement", label:
/// stable_id}` reverse link from each task in `removed_task_ids`'s
/// `task_links`. `link_task` calls this after replacing one `SubItem`'s
/// `task_ids`, for the tasks that fell out of that replacement.
///
/// Each `SubItem` owns exactly one reverse-link entry per task, labeled with
/// its own `stable_id` (see `add_reverse_task_links`, which keys existence
/// checks on `label == stable_id`) — and `stable_id`s are unique per
/// document (`collect_stable_ids`/`derive_stable_id` enforce this on
/// creation). So removing `(doc.id, "requirement", stable_id)` only ever
/// touches the entry this specific `SubItem` created; it can never affect an
/// entry another `SubItem` owns for the same task under its own label (t2
/// linked from a sibling SubItem keeps *that* SubItem's own reverse-link
/// entry — spec wiki/210 M4 test 2). The doc-wide scan below is a defensive
/// guard against that invariant being violated (e.g. duplicate/legacy
/// stable_ids): only skip the removal if some *other* SubItem's `task_ids`
/// still lists the task under the *same* `stable_id` we're about to remove.
///
/// Returns the subset of `removed_task_ids` whose reverse link was actually
/// removed (i.e. a matching `task_links` entry existed to remove and the
/// defensive guard did not block it).
fn remove_stale_reverse_links(
    handoff: &Path,
    doc: &DocMetadata,
    stable_id: &str,
    removed_task_ids: &[String],
) -> Result<Vec<String>> {
    let tasks_dir = handoff.join("tasks");
    let mut removed: Vec<String> = Vec::new();

    // Flatten every SubItem's (stable_id, task_ids) across all fragments so
    // membership checks below are simple linear scans — doc-scale SubItem
    // counts (hundreds, not millions) make this O(n) sufficient (wiki/210
    // design decision).
    let all_sub_items: Vec<(&str, &[String])> = doc
        .verification
        .iter()
        .flat_map(|v| v.items.iter())
        .flat_map(|item| item.sub_items.iter())
        .filter_map(|sub| {
            sub.stable_id
                .as_deref()
                .map(|id| (id, sub.task_ids.as_slice()))
        })
        .collect();

    for task_id in removed_task_ids {
        // Defensive guard only: true whenever another SubItem happens to
        // share this exact stable_id and still references the task — should
        // never occur since stable_ids are unique per document.
        let still_referenced_under_same_label =
            all_sub_items.iter().any(|(other_stable_id, task_ids)| {
                *other_stable_id == stable_id && task_ids.iter().any(|t| t == task_id)
            });
        if still_referenced_under_same_label {
            continue;
        }

        let Some(task_dir) = find_task_dir_by_id(&tasks_dir, task_id)? else {
            continue;
        };
        let mut did_remove = false;
        read_modify_write_task(&task_dir, |data, status| {
            let before = data.task_links.len();
            data.task_links.retain(|l| {
                !(l.target == doc.id
                    && l.link_type == "requirement"
                    && l.label.as_deref() == Some(stable_id))
            });
            if data.task_links.len() != before {
                did_remove = true;
                data.updated_at = Some(chrono::Utc::now().to_rfc3339());
            }
            Ok(status.to_string())
        })?;
        if did_remove {
            removed.push(task_id.clone());
        }
    }

    Ok(removed)
}

/// One read-modify-write of `task_id`'s own task file that applies every
/// requirement reverse-link addition/removal collected by a single
/// `link_requirements_to_task` or `unlink_requirements_from_task` call
/// (t370.3 / wiki/240 §4 P-M3: those two used to open+rewrite the same task
/// file once per resolved `stable_id`/document instead of once per call).
/// Returns `false` (with nothing applied) when `task_id` doesn't resolve to
/// a task directory; `to_add`/`to_remove` being simultaneously empty is a
/// no-op success (nothing to do, task existence isn't even checked).
/// t360.7 (wiki/220 §2.5): `label` (the stable_id) is the join key for a
/// requirement `task_links` entry — `target` (the owning document id) is
/// only a hint, refreshed opportunistically on add but never part of the
/// match. `to_add` carries the `role` (`"implements"` | `"executes"`,
/// already resolved by the caller — explicit `requirement_roles` override or
/// inferred from the SubItem's effective-layer side) to stamp onto each
/// added/refreshed entry. `to_remove_labels` matches purely by label so a
/// stable_id whose owning `SubItem` no longer resolves (deleted item) can
/// still be unlinked — see [`apply_requirement_links`]'s `unresolved_remove`
/// handling.
fn apply_requirement_reverse_links(
    handoff: &Path,
    task_id: &str,
    to_add: &[(&str, &str, &str)],
    to_remove_labels: &[&str],
) -> Result<bool> {
    if to_add.is_empty() && to_remove_labels.is_empty() {
        return Ok(true);
    }
    let tasks_dir = handoff.join("tasks");
    let Some(task_dir) = find_task_dir_by_id(&tasks_dir, task_id)? else {
        return Ok(false);
    };
    read_modify_write_task(&task_dir, |data, status| {
        for (doc_id, stable_id, role) in to_add {
            match data
                .task_links
                .iter_mut()
                .find(|l| l.link_type == "requirement" && l.label.as_deref() == Some(*stable_id))
            {
                Some(existing) => {
                    existing.target = (*doc_id).to_string();
                    existing.role = Some((*role).to_string());
                }
                None => {
                    data.task_links.push(TaskLink {
                        target: (*doc_id).to_string(),
                        link_type: "requirement".to_string(),
                        label: Some((*stable_id).to_string()),
                        role: Some((*role).to_string()),
                    });
                }
            }
        }
        if !to_remove_labels.is_empty() {
            data.task_links.retain(|l| {
                !(l.link_type == "requirement"
                    && l.label
                        .as_deref()
                        .is_some_and(|lbl| to_remove_labels.contains(&lbl)))
            });
        }
        data.updated_at = Some(chrono::Utc::now().to_rfc3339());
        Ok(status.to_string())
    })?;
    Ok(true)
}

/// `handoff_update_task(task.requirement_roles={...})` for a stable_id whose
/// `requirement_ids` membership is unchanged (t360.7, wiki/220 §2.5): updates
/// only the `role` field of the matching `task_links` entry — no `DocSet`
/// load, no `SubItem.task_ids` mutation, since `role` lives only on the task
/// side. A no-op (no task read-modify-write at all) when `role_changes` is
/// empty or `task_id` doesn't resolve to a task directory.
pub(crate) fn apply_requirement_role_changes(
    handoff: &Path,
    task_id: &str,
    role_changes: &[(String, String)],
) -> Result<()> {
    if role_changes.is_empty() {
        return Ok(());
    }
    let tasks_dir = handoff.join("tasks");
    let Some(task_dir) = find_task_dir_by_id(&tasks_dir, task_id)? else {
        return Ok(());
    };
    read_modify_write_task(&task_dir, |data, status| {
        for (stable_id, role) in role_changes {
            for link in data.task_links.iter_mut() {
                if link.link_type == "requirement"
                    && link.label.as_deref() == Some(stable_id.as_str())
                {
                    link.role = Some(role.clone());
                }
            }
        }
        data.updated_at = Some(chrono::Utc::now().to_rfc3339());
        Ok(status.to_string())
    })
}

/// t360.7 (wiki/220 §2.5 step 7): recomputes one requirement `SubItem`'s
/// `task_ids` membership with respect to a single task — the differential
/// apply every live link-change path runs (`update_task(requirement_ids)`
/// via [`apply_requirement_links`]) rather than rescanning every task's
/// links. The all-tasks-scanning full rebuild is a distinct, separately
/// gated operation reserved for `trace_report` self-repair and the explicit
/// repair tool (`rebuild_item_task_ids_full`). Idempotent: adding an
/// already-present `task_id`, or removing an absent one, is a no-op.
fn rebuild_item_task_ids(sub: &mut SubItem, task_id: &str, add: bool) {
    if add {
        if !sub.task_ids.iter().any(|t| t == task_id) {
            sub.task_ids.push(task_id.to_string());
        }
    } else {
        sub.task_ids.retain(|t| t != task_id);
    }
}

/// `handoff_update_task(task.requirement_ids=[...])`: adds `task_id` to the
/// `SubItem.task_ids` of each `stable_id` in `to_add`, removes it from each
/// `stable_id` in `to_remove`, and mirrors both sides onto the task's own
/// `task_links` in one read-modify-write. [`link_requirements_to_task`] and
/// [`unlink_requirements_from_task`] below are thin one-sided wrappers kept
/// for their existing call sites and unit tests.
///
/// P-M3 (wiki/240 §4, review round 2 MAJOR fix): a single `handoff_update_task`
/// call whose `requirement_ids` both adds and removes stable_ids used to go
/// through the add-only and remove-only helpers as two *separate* calls —
/// each loading its own [`DocSet`] (a full `read_all_docs` pass), each doing
/// its own `task_id` read-modify-write via [`apply_requirement_reverse_links`],
/// and each recomputing/writing `_requirements_summary.json` on its own.
/// Every [`DocSet::flush`] also evicts the documents it just wrote from the
/// P-M1 read cache, so the *second* call's `DocSet::load` would re-parse and
/// re-hash any document the first call had just touched — exactly the
/// JA-scale re-hashing cost this task exists to remove. This function runs
/// the resolve/mutate/flush/reverse-link/summary sequence once for both
/// directions: one `DocSet::load`, one `flush` (only the documents actually
/// touched by either side), one `apply_requirement_reverse_links`
/// read-modify-write of `task_id`'s own file (every addition and removal
/// applied together), and one summary write from that same in-memory
/// `DocSet`.
/// Outcome of one [`apply_requirement_links`] mutation attempt against a
/// single (possibly retried) [`DocSet`] snapshot — see
/// [`load_mutate_flush_with_retry`]. Kept as a plain struct rather than
/// accumulating into the outer function's variables directly, because a
/// retried attempt must *replace* the previous attempt's resolution result,
/// not append to it (the previous attempt's `DocSet` snapshot is discarded
/// wholesale on conflict).
struct LinkMutationOutcome {
    warnings: Vec<String>,
    resolved_add: Vec<ResolvedSubItem>,
    resolved_remove: Vec<ResolvedSubItem>,
    /// `to_remove` stable_ids that resolved to no `SubItem` anywhere in the
    /// corpus (t360.7: typically because the item was deleted from its
    /// owning document/layer body). Still unlinked on the task side by
    /// label — see [`apply_requirement_links`]'s call to
    /// [`apply_requirement_reverse_links`] — even though there is no
    /// `SubItem.task_ids` left to remove `task_id` from.
    unresolved_remove: Vec<String>,
    /// Each resolved add's `SubItem.category` at the moment of linking,
    /// keyed by stable_id — the input to role inference when the caller
    /// (`update_task`) did not supply an explicit `requirement_roles` entry
    /// for that stable_id (§2.5: "role 省略時は実効層の side から推定
    /// （right → executes、それ以外 → implements）"; `category == "check"`
    /// is exactly `layer_sync`'s right-side marker, wiki/220 §2.3).
    add_categories: HashMap<String, String>,
}

/// The `DocSet`-mutation core of a `requirement_ids` add/remove diff —
/// extracted from [`apply_requirement_links`] (t370.10) so the combined
/// diff+propagate path ([`apply_requirement_diff_and_propagate`]) can run it
/// against the *same* `DocSet` snapshot a subsequent
/// [`propagate_dev_stage_within_doc_set`] call also mutates, instead of each
/// paying for its own `DocSet::load`/`flush`. Resolves `to_add`/`to_remove`
/// stable_ids against `doc_set`, mutates each resolved `SubItem.task_ids`
/// (marking the owning document dirty), and returns the resolution outcome
/// — it does **not** touch the task's own `task_links` (that is
/// [`apply_reverse_links_for_outcome`]'s job) or write the summary (the
/// caller decides that once, after whatever else it also did to `doc_set`).
fn mutate_requirement_link_diff(
    doc_set: &mut DocSet,
    task_id: &str,
    to_add: &[String],
    to_remove: &[String],
) -> Result<LinkMutationOutcome> {
    let mut warnings = Vec::new();

    let (resolved_add, unresolved_add, ambiguous_add) =
        resolve_stable_ids_in(doc_set.docs(), to_add);
    if !unresolved_add.is_empty() {
        warnings.push(format!(
            "Could not resolve requirement stable_id(s): {}",
            unresolved_add.join(", ")
        ));
    }
    if !ambiguous_add.is_empty() {
        warnings.push(format!(
            "Requirement stable_id(s) are ambiguous (found in more than one document) and were \
             not linked: {}",
            ambiguous_add.join(", ")
        ));
    }
    let (resolved_remove, unresolved_remove, ambiguous_remove) =
        resolve_stable_ids_in(doc_set.docs(), to_remove);
    if !unresolved_remove.is_empty() {
        warnings.push(format!(
            "Could not resolve requirement stable_id(s) for unlinking: {}",
            unresolved_remove.join(", ")
        ));
    }
    if !ambiguous_remove.is_empty() {
        warnings.push(format!(
            "Requirement stable_id(s) are ambiguous (found in more than one document) and were \
             not unlinked: {}",
            ambiguous_remove.join(", ")
        ));
    }

    // Group by doc_id so each document is mutated (and marked dirty)
    // exactly once per call, even when it holds SubItems on both the add
    // and the remove side.
    let mut by_doc: std::collections::BTreeMap<
        String,
        (Vec<&ResolvedSubItem>, Vec<&ResolvedSubItem>),
    > = std::collections::BTreeMap::new();
    for r in &resolved_add {
        by_doc.entry(r.doc_id.clone()).or_default().0.push(r);
    }
    for r in &resolved_remove {
        by_doc.entry(r.doc_id.clone()).or_default().1.push(r);
    }

    let mut add_categories: HashMap<String, String> = HashMap::new();
    for (doc_id, (adds, removes)) in &by_doc {
        let doc = doc_set.get_mut(doc_id).ok_or_else(|| {
            let slug = adds
                .first()
                .or_else(|| removes.first())
                .map(|r| r.doc_slug.as_str())
                .unwrap_or("?");
            anyhow::anyhow!("Document not found: {doc_id} (slug={slug})")
        })?;
        let v = verification_mut(doc, doc_id)?;
        for r in adds {
            let sub = resolved_sub_item_mut(v, r, doc_id)?;
            add_categories.insert(r.stable_id.clone(), sub.category.clone());
            rebuild_item_task_ids(sub, task_id, true);
        }
        for r in removes {
            let sub = resolved_sub_item_mut(v, r, doc_id)?;
            rebuild_item_task_ids(sub, task_id, false);
        }
        v.updated_at = chrono::Utc::now().to_rfc3339();
        v.status = recompute_verification_status(&v.items);
        doc_set.mark_dirty(doc_id);
    }

    Ok(LinkMutationOutcome {
        warnings,
        resolved_add,
        resolved_remove,
        unresolved_remove,
        add_categories,
    })
}

/// §2.5: role, explicit `requirement_roles` override first, else inferred
/// from the resolved SubItem's effective-layer side (`category == "check"`
/// -> right side -> `"executes"`; anything else, including no layer at all,
/// -> `"implements"`). One entry per `resolved_add`, same order.
fn compute_add_roles(
    resolved_add: &[ResolvedSubItem],
    add_categories: &HashMap<String, String>,
    roles: &HashMap<String, String>,
) -> Vec<String> {
    resolved_add
        .iter()
        .map(|r| {
            roles.get(&r.stable_id).cloned().unwrap_or_else(|| {
                match add_categories.get(&r.stable_id).map(String::as_str) {
                    Some("check") => "executes".to_string(),
                    _ => "implements".to_string(),
                }
            })
        })
        .collect()
}

/// Mirrors a resolved [`LinkMutationOutcome`] onto `task_id`'s own
/// `task_links` in a single `read_modify_write_task` call (t360.7's
/// `apply_requirement_reverse_links`) and returns the accumulated warnings.
/// Deliberately does **not** write `_requirements_summary.json` — callers
/// decide that themselves once, after whatever else they also did to the
/// `DocSet` this outcome came from (t370.10: the combined diff+propagate
/// path must write it at most once per call, not once per sub-step).
fn apply_reverse_links_for_outcome(
    handoff: &Path,
    task_id: &str,
    outcome: &LinkMutationOutcome,
    to_add_roles: &[String],
) -> Result<Vec<String>> {
    let mut warnings = outcome.warnings.clone();
    let to_add_triples: Vec<(&str, &str, &str)> = outcome
        .resolved_add
        .iter()
        .zip(to_add_roles.iter())
        .map(|(r, role)| (r.doc_id.as_str(), r.stable_id.as_str(), role.as_str()))
        .collect();
    // t360.7: unresolved removes (item deleted) are unlinked on the task
    // side too, by label, even though there is no SubItem left to touch —
    // see `apply_requirement_reverse_links`'s doc comment ("key is label").
    let to_remove_labels: Vec<&str> = outcome
        .resolved_remove
        .iter()
        .map(|r| r.stable_id.as_str())
        .chain(outcome.unresolved_remove.iter().map(String::as_str))
        .collect();
    if !apply_requirement_reverse_links(handoff, task_id, &to_add_triples, &to_remove_labels)? {
        // `task_id` is the caller's own task (already resolved by
        // update_task before calling this function), so this should never
        // happen — surfaced as a warning rather than silently dropped in
        // case of a race with a concurrent task deletion.
        warnings.push(format!(
            "Could not resolve task id {task_id} while updating reverse requirement links"
        ));
    }
    Ok(warnings)
}

/// `handoff_update_task(task.requirement_ids=[...], task.requirement_roles={...})`.
/// `roles` maps a stable_id in `to_add` to an explicit `"implements"` |
/// `"executes"` override (t360.7, wiki/220 §2.5); a stable_id in `to_add`
/// with no entry here has its role inferred from its SubItem's effective
/// layer side once resolved (`add_categories`, right -> `"executes"`,
/// otherwise -> `"implements"`). Stable_ids in `to_remove` are ignored by
/// `roles` — a removed link carries no role.
///
/// Used when this call does **not** also change the task's `status` in the
/// same request — see [`apply_requirement_diff_and_propagate`] for the
/// combined path used when it does (t370.10).
pub(crate) fn apply_requirement_links(
    handoff: &Path,
    task_id: &str,
    to_add: &[String],
    to_remove: &[String],
    roles: &HashMap<String, String>,
) -> Result<Vec<String>> {
    if to_add.is_empty() && to_remove.is_empty() {
        return Ok(Vec::new());
    }

    // P-M7 (wiki/240 §4, NFR-007): retries the whole
    // load-resolve-mutate-flush cycle against a freshly reloaded `DocSet` if
    // `flush()` detects another process wrote one of these documents in
    // between (wiki/240 §3 C9 "link/unlink/propagate の文書 RMW には楽観ロッ
    // クがない").
    let (doc_set, outcome) =
        crate::storage::docs::load_mutate_flush_with_retry(handoff, |doc_set| {
            mutate_requirement_link_diff(doc_set, task_id, to_add, to_remove)
        })?;

    let to_add_roles = compute_add_roles(&outcome.resolved_add, &outcome.add_categories, roles);
    let has_add_or_remove = !outcome.resolved_add.is_empty() || !outcome.resolved_remove.is_empty();
    let warnings = apply_reverse_links_for_outcome(handoff, task_id, &outcome, &to_add_roles)?;

    if has_add_or_remove {
        write_requirements_summary(handoff, doc_set.docs())?;
    }

    Ok(warnings)
}

/// One-sided wrapper over [`apply_requirement_links`]: appends (deduped)
/// `task_id` to the `SubItem.task_ids` of each `stable_id` in `stable_ids`
/// and mirrors the reverse `task_links` entry on the task side. Unlike
/// `handoff_doc_verify(action="link_task")`, which *replaces*
/// `SubItem.task_ids` wholesale, this APPENDS: `requirement_ids` is meant to
/// incrementally attach a task to more requirements over time without
/// clobbering links other tasks already hold on the same SubItem (design
/// decision recorded on t330.1). Returns warnings for any stable_id that
/// resolved to no SubItem; those are non-fatal.
pub(crate) fn link_requirements_to_task(
    handoff: &Path,
    task_id: &str,
    stable_ids: &[String],
) -> Result<Vec<String>> {
    apply_requirement_links(handoff, task_id, stable_ids, &[], &HashMap::new())
}

/// One-sided wrapper over [`apply_requirement_links`] — the inverse of
/// [`link_requirements_to_task`]: removes `task_id` from the
/// `SubItem.task_ids` of each `stable_id` in `removed_stable_ids`, and
/// removes the corresponding `task_links` entry on the task side.
///
/// Production code (`update_task.rs`'s `apply_requirement_ids_diff`) calls
/// [`apply_requirement_links`] directly with both `to_add`/`to_remove` in one
/// pass (review round 2 MAJOR fix) rather than this add-only/remove-only
/// pair — this wrapper survives as a `#[cfg(test)]` convenience for tests
/// that only exercise the remove side, mirroring `resolve_stable_ids` above.
#[cfg(test)]
pub(crate) fn unlink_requirements_from_task(
    handoff: &Path,
    task_id: &str,
    removed_stable_ids: &[String],
) -> Result<Vec<String>> {
    apply_requirement_links(handoff, task_id, &[], removed_stable_ids, &HashMap::new())
}

/// Maps a task status to an implied `dev_stage` ordinal for the
/// min-of-linked-tasks computation. Higher = further along.
/// `skipped` returns `None` — excluded from the computation.
fn implied_dev_stage_ord(task_status: &str) -> Option<u8> {
    match task_status {
        "todo" | "blocked" => Some(0),
        "in_progress" => Some(1),
        "review" | "done" => Some(2),
        _ => None,
    }
}

fn dev_stage_from_ord(ord: u8) -> &'static str {
    match ord {
        0 => "not_started",
        1 => "in_progress",
        _ => "implemented",
    }
}

/// Propagates task-status changes to the `dev_stage` of linked requirement
/// SubItems using a min-of-linked-tasks strategy: the SubItem's `dev_stage`
/// is set to the minimum implied `dev_stage` across all non-skipped linked
/// tasks. If a SubItem's current `dev_stage` is `"tested"` or `"verified"`,
/// it is protected (those stages are manual-only).
///
/// Called from `update_task` after a status transition.
///
/// P-M3/P-M5 (wiki/240 §4): loads a single [`DocSet`] instead of resolving
/// stable_ids via a fresh `read_all_docs` and then re-reading each touched
/// document again, writes back only documents whose `dev_stage` actually
/// changed, recomputes the summary from that same in-memory `DocSet`, and
/// memoizes each co-linked task's status the first time it's looked up
/// rather than re-reading the same task file once per `SubItem` that happens
/// to share it.
pub(crate) fn propagate_dev_stage_for_task(handoff: &Path, task_links: &[TaskLink]) -> Result<()> {
    // t360.7 (wiki/220 §2.5): restricted to `role == "implements"` links —
    // `None` (pre-M1 links, and any link a caller never re-inferred a role
    // for) is treated as `"implements"` for backward compatibility; only an
    // explicit `"executes"` role is excluded. A verification/test-execution
    // task completing must never move the requirement it *tests*.
    let requirement_stable_ids: Vec<String> = task_links
        .iter()
        .filter(|l| l.link_type == "requirement")
        .filter(|l| l.role.as_deref() != Some("executes"))
        .filter_map(|l| l.label.clone())
        .collect();
    if requirement_stable_ids.is_empty() {
        return Ok(());
    }

    let tasks_dir = handoff.join("tasks");

    // P-M7 (wiki/240 §4, NFR-007): retries the whole load-resolve-mutate-
    // flush cycle against a freshly reloaded `DocSet` if `flush()` detects a
    // concurrent external write (wiki/240 §3 C9).
    let (doc_set, any_changed) =
        crate::storage::docs::load_mutate_flush_with_retry(handoff, |doc_set| {
            propagate_dev_stage_within_doc_set(doc_set, &tasks_dir, &requirement_stable_ids)
        })?;

    if any_changed {
        write_requirements_summary(handoff, doc_set.docs())?;
    }

    Ok(())
}

/// The `DocSet`-mutation core of dev_stage propagation — extracted from
/// [`propagate_dev_stage_for_task`] (t370.10) so
/// [`apply_requirement_diff_and_propagate`] can run it against the *same*
/// `DocSet` snapshot a preceding [`mutate_requirement_link_diff`] call also
/// mutated in the same request, instead of each paying for its own
/// `DocSet::load`/`flush`/summary write. `requirement_stable_ids` must
/// already be filtered by the caller to the `role != "executes"` set
/// (§2.5) — this function applies no role filtering of its own beyond the
/// belt-and-suspenders `category == "check"` skip below. Returns whether
/// anything actually changed (the caller's summary-write gate).
fn propagate_dev_stage_within_doc_set(
    doc_set: &mut DocSet,
    tasks_dir: &Path,
    requirement_stable_ids: &[String],
) -> Result<bool> {
    if requirement_stable_ids.is_empty() {
        return Ok(false);
    }

    let (resolved, _unresolved, _ambiguous) =
        resolve_stable_ids_in(doc_set.docs(), requirement_stable_ids);
    if resolved.is_empty() {
        return Ok(false);
    }

    // P-M5: build the task_id -> status map lazily, once per distinct
    // task_id per attempt, instead of re-reading the same task file
    // for every SubItem that happens to share it (wiki/240 §4).
    let mut status_cache: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    let mut by_doc: std::collections::BTreeMap<String, Vec<&ResolvedSubItem>> =
        std::collections::BTreeMap::new();
    for r in &resolved {
        by_doc.entry(r.doc_id.clone()).or_default().push(r);
    }

    let mut any_changed = false;

    for (doc_id, items) in by_doc {
        let Some(doc) = doc_set.get_mut(&doc_id) else {
            continue;
        };
        let Some(v) = doc.verification.as_mut() else {
            continue;
        };

        let mut doc_changed = false;
        for r in &items {
            // FR-806 (§4.1): resolve by stable_id (freeform-aware)
            // rather than assuming `Some(fragment_seq)` — missing
            // item/sub_item is treated the same defensive way as
            // before (skip, don't fail the whole propagate call).
            let sub = match resolved_sub_item_mut(v, r, &doc_id) {
                Ok(s) => s,
                Err(_) => continue,
            };

            // §2.5: belt-and-suspenders alongside the caller's `role` filter
            // — a right-side (`category == "check"`) verification item
            // never gets its dev_stage propagated even if some link into it
            // was (incorrectly) tagged `role: "implements"`.
            if sub.category == "check" {
                continue;
            }

            let current = sub.dev_stage.as_deref().unwrap_or("not_started");
            if current == "tested" || current == "verified" {
                continue;
            }

            let mut min_ord: Option<u8> = None;
            for tid in &sub.task_ids {
                let status = match status_cache.get(tid) {
                    Some(s) => s.clone(),
                    None => match task_status_from_dir(tasks_dir, tid) {
                        Ok(s) => {
                            status_cache.insert(tid.clone(), s.clone());
                            s
                        }
                        Err(_) => continue,
                    },
                };
                if let Some(ord) = implied_dev_stage_ord(&status) {
                    min_ord = Some(match min_ord {
                        Some(m) => m.min(ord),
                        None => ord,
                    });
                }
            }

            if let Some(ord) = min_ord {
                let new_stage = dev_stage_from_ord(ord);
                if current != new_stage {
                    sub.dev_stage = Some(new_stage.to_string());
                    doc_changed = true;
                }
            }
        }

        if doc_changed {
            v.updated_at = chrono::Utc::now().to_rfc3339();
            v.status = recompute_verification_status(&v.items);
            doc_set.mark_dirty(&doc_id);
            any_changed = true;
        }
    }

    Ok(any_changed)
}

/// `handoff_update_task` when a single call changes **both** `status` and
/// `requirement_ids` (t370.10, wiki/240 §6 PR-8 revision "1 request, 1 file,
/// at most once"). Before this function existed, such a call ran
/// [`apply_requirement_links`] (its own `DocSet::load`/`flush` and its own
/// `_requirements_summary.json` write) followed by
/// [`propagate_dev_stage_for_task`] (a second, independent `DocSet::load`/
/// `flush` and a second summary write) as two fully separate passes —
/// measured `writes=2` in M-S7. `update_task.rs` now calls this instead
/// whenever both change in the same request: one `DocSet::load`, the
/// link-diff mutation ([`mutate_requirement_link_diff`]) and the dev_stage
/// propagation ([`propagate_dev_stage_within_doc_set`]) applied to that
/// *same* in-memory snapshot in sequence (mutate first, so a stable_id
/// added by this same call already carries `task_id` in its `task_ids` by
/// the time propagation reads it), one `flush`, and — only if either step
/// actually changed something — exactly one summary write.
///
/// `retained_requirement_stable_ids` is the caller-computed set of
/// `"implements"`-role requirement stable_ids this task stays linked to
/// across the update (present in both the old and new `requirement_ids`,
/// already reflecting any `requirement_roles` change applied via
/// `apply_requirement_role_changes` before this call, and already excluding
/// `role == "executes"` links) — combined here with `to_add`'s own
/// (explicit-or-inferred) roles to form the complete propagation set,
/// matching exactly what two separate calls to `apply_requirement_links` +
/// `propagate_dev_stage_for_task` would have covered between them. Removed
/// stable_ids are never propagated, in either the old two-call path or
/// this one — a task that just unlinked from a requirement no longer
/// affects that requirement's dev_stage.
pub(crate) fn apply_requirement_diff_and_propagate(
    handoff: &Path,
    task_id: &str,
    to_add: &[String],
    to_remove: &[String],
    roles: &HashMap<String, String>,
    retained_requirement_stable_ids: &[String],
) -> Result<Vec<String>> {
    let tasks_dir = handoff.join("tasks");

    let (doc_set, (outcome, to_add_roles, dev_stage_changed)) =
        crate::storage::docs::load_mutate_flush_with_retry(handoff, |doc_set| {
            let outcome = mutate_requirement_link_diff(doc_set, task_id, to_add, to_remove)?;
            let to_add_roles =
                compute_add_roles(&outcome.resolved_add, &outcome.add_categories, roles);

            let mut propagate_ids: Vec<String> = retained_requirement_stable_ids.to_vec();
            propagate_ids.extend(
                outcome
                    .resolved_add
                    .iter()
                    .zip(to_add_roles.iter())
                    .filter(|(_, role)| role.as_str() != "executes")
                    .map(|(r, _)| r.stable_id.clone()),
            );

            let dev_stage_changed =
                propagate_dev_stage_within_doc_set(doc_set, &tasks_dir, &propagate_ids)?;

            Ok((outcome, to_add_roles, dev_stage_changed))
        })?;

    let has_add_or_remove = !outcome.resolved_add.is_empty() || !outcome.resolved_remove.is_empty();
    let warnings = apply_reverse_links_for_outcome(handoff, task_id, &outcome, &to_add_roles)?;

    if has_add_or_remove || dev_stage_changed {
        write_requirements_summary(handoff, doc_set.docs())?;
    }

    Ok(warnings)
}

/// Reads the current status of a task by its id. Returns the status string
/// (e.g. "done", "in_progress").
fn task_status_from_dir(tasks_dir: &Path, task_id: &str) -> Result<String> {
    let task_dir = find_task_dir_by_id(tasks_dir, task_id)?
        .ok_or_else(|| anyhow::anyhow!("Task not found: {task_id}"))?;
    let (_data, status) =
        read_task(&task_dir)?.ok_or_else(|| anyhow::anyhow!("Task file not found: {task_id}"))?;
    Ok(status)
}

/// Outcome of one [`rebuild_item_task_ids_full`] call.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) struct FullRebuildOutcome {
    /// `false` when the §4.3 fingerprint gate skipped the rebuild entirely
    /// (no task has changed since the fingerprint recorded in the last-
    /// written `_requirements_summary.json`) — `sub_items_changed`/
    /// `docs_changed` are `0` in that case.
    pub(crate) ran: bool,
    pub(crate) sub_items_changed: usize,
    pub(crate) docs_changed: usize,
}

/// Recursively scans every task under `tasks_dir` and folds each
/// `link_type == "requirement"` `task_links` entry into `by_stable_id`
/// (stable_id -> the set of task ids that currently declare it) —
/// `TaskData.task_links` is the source of truth (D3, wiki/220 §2.5), so this
/// is the authoritative membership every requirement `SubItem.task_ids`
/// should mirror.
fn collect_requirement_task_links(
    tasks_dir: &Path,
    by_stable_id: &mut HashMap<String, std::collections::BTreeSet<String>>,
) -> Result<()> {
    if !tasks_dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(tasks_dir)
        .with_context(|| format!("Failed to read dir: {}", tasks_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let task_dir = entry.path();
        if let Some((data, _status)) = read_task(&task_dir)? {
            for link in &data.task_links {
                if link.link_type == "requirement" {
                    if let Some(label) = &link.label {
                        by_stable_id
                            .entry(label.clone())
                            .or_default()
                            .insert(data.id.clone());
                    }
                }
            }
        }
        collect_requirement_task_links(&task_dir, by_stable_id)?;
    }
    Ok(())
}

/// Reads back the `inputs` fingerprint stored in the last-written
/// `_requirements_summary.json`, if any (tolerant of a missing file, a
/// pre-r3 summary with no `inputs` field, or corrupt JSON — all treated as
/// "no known fingerprint", which makes [`rebuild_item_task_ids_full`] run
/// rather than silently skip on ambiguous state).
fn read_persisted_summary_inputs(handoff: &Path) -> Result<Option<DerivedInputs>> {
    let path = docs_dir(handoff).join("_requirements_summary.json");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("Failed to read {}", path.display())),
    };
    let Ok(value) = serde_json::from_str::<Value>(&content) else {
        return Ok(None);
    };
    let Some(inputs) = value.get("inputs") else {
        return Ok(None);
    };
    Ok(serde_json::from_value::<DerivedInputs>(inputs.clone()).ok())
}

/// t360.7 (wiki/220 §2.5 "全再構築", §4.3 r3): the full, all-tasks-scanning
/// rebuild of every requirement `SubItem.task_ids` from `TaskData.task_links`
/// (the source of truth, D3) — as opposed to the differential apply every
/// live link-change path uses (`apply_requirement_links`'s
/// `rebuild_item_task_ids` helper, and the layer-sync removed-item unlink
/// hook), which only ever touches the handful of stable_ids one call
/// actually changed.
///
/// Reserved for two callers: `trace_report`'s self-repair (t360.10) and the
/// explicit `handoff_doc_repair_task_ids` tool — never a live link-change
/// path, and never unconditionally: gated on the §4.3 input fingerprint
/// (`tasks_max_mtime_ns`/`tasks_count` from [`compute_derived_inputs`])
/// against the fingerprint recorded in the last-written
/// `_requirements_summary.json` ([`read_persisted_summary_inputs`]). Every
/// differential apply already ends by rewriting that summary with a fresh
/// fingerprint (`write_requirements_summary`), so an unchanged fingerprint
/// here means nothing has moved since the corpus was last brought into sync
/// — by either path — making a full rescan redundant. `ran: false` and a
/// no-op in that case; otherwise this rescans every task once, corrects every
/// `SubItem.task_ids` that has drifted from that scan's result, and (only
/// when something actually changed) rewrites the summary.
pub(crate) fn rebuild_item_task_ids_full(handoff: &Path) -> Result<FullRebuildOutcome> {
    let current_inputs = compute_derived_inputs(handoff)?;
    if let Some(persisted) = read_persisted_summary_inputs(handoff)? {
        if persisted.tasks_max_mtime_ns == current_inputs.tasks_max_mtime_ns
            && persisted.tasks_count == current_inputs.tasks_count
        {
            return Ok(FullRebuildOutcome {
                ran: false,
                sub_items_changed: 0,
                docs_changed: 0,
            });
        }
    }

    let mut by_stable_id: HashMap<String, std::collections::BTreeSet<String>> = HashMap::new();
    collect_requirement_task_links(&handoff.join("tasks"), &mut by_stable_id)?;

    let (doc_set, (sub_items_changed, docs_changed)) =
        crate::storage::docs::load_mutate_flush_with_retry(handoff, |doc_set| {
            let doc_ids: Vec<String> = doc_set.docs().iter().map(|d| d.id.clone()).collect();
            let mut sub_items_changed = 0usize;
            let mut docs_changed = 0usize;
            for doc_id in doc_ids {
                let Some(doc) = doc_set.get_mut(&doc_id) else {
                    continue;
                };
                let Some(v) = doc.verification.as_mut() else {
                    continue;
                };
                let mut doc_changed = false;
                for item in v.items.iter_mut() {
                    for sub in item.sub_items.iter_mut() {
                        let Some(stable_id) = sub.stable_id.as_deref() else {
                            continue;
                        };
                        let expected: Vec<String> = by_stable_id
                            .get(stable_id)
                            .map(|ids| ids.iter().cloned().collect())
                            .unwrap_or_default();
                        if sub.task_ids != expected {
                            sub.task_ids = expected;
                            sub_items_changed += 1;
                            doc_changed = true;
                        }
                    }
                }
                if doc_changed {
                    v.updated_at = chrono::Utc::now().to_rfc3339();
                    v.status = recompute_verification_status(&v.items);
                    doc_set.mark_dirty(&doc_id);
                    docs_changed += 1;
                }
            }
            Ok((sub_items_changed, docs_changed))
        })?;

    // Always refresh the summary (even when nothing changed) so its `inputs`
    // fingerprint moves forward to `current_inputs` — that is what makes the
    // next call's gate check meaningful.
    write_requirements_summary(handoff, doc_set.docs())?;

    Ok(FullRebuildOutcome {
        ran: true,
        sub_items_changed,
        docs_changed,
    })
}

/// `handoff_doc_verify` — generate/check/skip/sync/set_refs a document's
/// verification matrix (wiki/140-verification-matrix.md §4.1).
pub fn handle_doc_verify(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let project_dir = &ctx.project_dir;
    let handoff = &ctx.handoff_dir;

    let doc_id = arguments
        .get("doc_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'doc_id' is required"))?;
    let action = arguments
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'action' is required"))?;

    // t370.12 (PR-4, wiki/240-performance-design.md §4/§6): only `check`/
    // `check_all` read a section's `content_hash` (to record
    // `content_hash_at_verify`) — every other action (`set_dev_stage`,
    // `set_priority`, `link_task`, `skip`, `set_refs`, `add_item`, `sync`,
    // `generate`, `backfill_stable_ids`, `suggest_refs`) only mutates
    // `SubItem`/`VerificationItem` metadata fields (or, for `suggest_refs`,
    // reads none at all) and never looks at a hash. Resolving lazily for
    // those lets `write_doc` below reuse this process's already-proven
    // `content_hash` (t370.12's write-time reuse) instead of `resolve_doc`'s
    // unconditional `read_doc_hashed` paying the full `lexsim::content_hash`
    // pass over the whole body on every call — the dominant JA-scale cost
    // this task removes from `doc_verify_set_dev_stage`/`doc_verify_link_task`.
    let need_hash = action_needs_content_hash(action);
    let mut doc = resolve_doc_for_verify(handoff, doc_id, need_hash)?
        .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;

    // wiki/220-vmodel-integration-design.md §2.3 write guard: on a layer
    // document, body-owned `SubItem` fields are defined by the Markdown
    // body and cannot be written through `doc_verify` — only the body
    // (re-saved through `doc_save`, which re-syncs) can change them.
    // `impl_refs`-only `set_refs` calls are allowed (impl_refs is a runtime
    // field); a `set_refs` call that also carries `test_refs` is refused.
    // `set_dev_stage` / `link_task` / `check` / `check_all` / `skip` /
    // `suggest_refs` are unaffected (§2.3: explicitly permitted).
    const LAYER_DOC_GUARDED_ACTIONS: &[&str] = &["add_item", "set_priority", "backfill_stable_ids"];
    if doc.layer.is_some() {
        let refuses = LAYER_DOC_GUARDED_ACTIONS.contains(&action)
            || (action == "set_refs" && arguments.get("test_refs").is_some());
        if refuses {
            anyhow::bail!(LAYER_BODY_EDIT_GUARD_MSG);
        }
    }

    // `suggest_refs` is read-only (it never mutates the verification matrix,
    // only proposes candidates for the caller to feed into `set_refs`), and
    // its response shape (a `suggestions` list) differs from every other
    // action's mutation-count summary — handled separately, before the
    // shared mutate-then-write flow below.
    if action == "suggest_refs" {
        let v = doc.verification.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "No verification matrix exists for document {doc_id}; use action='generate' first"
            )
        })?;
        let suggestions = suggest_refs(project_dir, &doc, v);
        return Ok(to_json(&json!({
            "doc_id": doc.id,
            "suggestions": suggestions,
        })));
    }

    let now = chrono::Utc::now().to_rfc3339();
    let mut warnings: Vec<String> = Vec::new();

    match action {
        "generate" => {
            if doc.verification.is_some() {
                anyhow::bail!(
                    "Verification matrix already exists for document {doc_id}; use action='sync' to re-sync it instead"
                );
            }
            let skip_seqs: Vec<usize> = arguments
                .get("skip_seqs")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_u64())
                        .map(|n| n as usize)
                        .collect()
                })
                .unwrap_or_default();

            let items: Vec<VerificationItem> = doc
                .sections
                .iter()
                .map(|s| VerificationItem {
                    fragment_seq: Some(s.seq),
                    heading: s.heading.clone(),
                    status: if skip_seqs.contains(&s.seq) {
                        "skipped".to_string()
                    } else {
                        "pending".to_string()
                    },
                    impl_refs: Vec::new(),
                    test_refs: Vec::new(),
                    reviewer: None,
                    verified_at: None,
                    notes: String::new(),
                    content_hash_at_verify: None,
                    category: "section".to_string(),
                    sub_items: Vec::new(),
                    label: None,
                })
                .collect();

            doc.verification = Some(Verification {
                status: recompute_verification_status(&items),
                created_at: now.clone(),
                updated_at: now,
                items,
            });
        }
        "check" => {
            let reviewer = arguments
                .get("reviewer")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let notes = arguments
                .get("notes")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let sub_item_id = arguments
                .get("sub_item_id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let sub_item_index = arguments
                .get("sub_item_index")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);

            // FR-806 (§4.1): `fragment_seq` is optional when `sub_item_id`
            // is given — freeform SubItems (`fragment_seq: None`) have no
            // section to batch over, so there is nothing to pass an array
            // of fragment_seqs for. When `fragment_seq` IS given (with or
            // without `sub_item_id`), behavior is unchanged from before this
            // task: batch over the given fragment_seq(s).
            let fragment_seqs: Vec<Option<usize>> = if arguments.get("fragment_seq").is_some() {
                required_fragment_seqs(arguments)?.into_iter().map(Some).collect()
            } else if sub_item_id.is_some() {
                vec![None]
            } else {
                // No fragment_seq and no sub_item_id: surface the original
                // required-fragment_seq error instead of silently no-op-ing.
                required_fragment_seqs(arguments)?.into_iter().map(Some).collect()
            };

            for fragment_seq in fragment_seqs {
                let section_hash = fragment_seq.and_then(|seq| {
                    doc.sections
                        .iter()
                        .find(|s| s.seq == seq)
                        .and_then(|s| s.content_hash.clone())
                });

                let v = verification_mut(&mut doc, doc_id)?;
                let item =
                    locate_item_for_sub_item_action(v, fragment_seq, sub_item_id.as_deref(), doc_id)?;

                if sub_item_id.is_some() || sub_item_index.is_some() {
                    let (sub, warning) = find_sub_item_mut_by_id(
                        item,
                        sub_item_id.as_deref(),
                        sub_item_index,
                        fragment_seq,
                        doc_id,
                    )?;
                    if let Some(w) = warning {
                        warnings.push(w);
                    }
                    sub.status = "verified".to_string();
                    sub.verified_at = Some(now.clone());
                    if reviewer.is_some() {
                        sub.reviewer = reviewer.clone();
                    }
                    if let Some(notes) = &notes {
                        sub.notes = notes.clone();
                    }
                } else {
                    item.status = "verified".to_string();
                    item.verified_at = Some(now.clone());
                    if reviewer.is_some() {
                        item.reviewer = reviewer.clone();
                    }
                    if let Some(notes) = &notes {
                        item.notes = notes.clone();
                    }
                    item.content_hash_at_verify = section_hash;
                }
                v.updated_at = now.clone();
                v.status = recompute_verification_status(&v.items);
            }
        }
        "check_all" => {
            let reviewer = arguments
                .get("reviewer")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let notes = arguments
                .get("notes")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let sections = doc.sections.clone();

            let v = verification_mut(&mut doc, doc_id)?;
            for item in v.items.iter_mut() {
                let section_hash = item.fragment_seq.and_then(|seq| {
                    sections
                        .iter()
                        .find(|s| s.seq == seq)
                        .and_then(|s| s.content_hash.clone())
                });
                item.status = "verified".to_string();
                item.verified_at = Some(now.clone());
                if reviewer.is_some() {
                    item.reviewer = reviewer.clone();
                }
                if let Some(notes) = &notes {
                    item.notes = notes.clone();
                }
                item.content_hash_at_verify = section_hash;

                // v2: check_all also verifies every sub_item (spec §7.2).
                for sub in item.sub_items.iter_mut() {
                    sub.status = "verified".to_string();
                    sub.verified_at = Some(now.clone());
                    if reviewer.is_some() {
                        sub.reviewer = reviewer.clone();
                    }
                }
            }
            v.updated_at = now.clone();
            v.status = recompute_verification_status(&v.items);
        }
        "skip" => {
            // FR-806 (§4.1): fragment_seq is optional when sub_item_id is
            // given (see locate_item_for_sub_item_action).
            let fragment_seq = arguments
                .get("fragment_seq")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            let sub_item_id = arguments
                .get("sub_item_id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let sub_item_index = arguments
                .get("sub_item_index")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);

            let v = verification_mut(&mut doc, doc_id)?;
            let item =
                locate_item_for_sub_item_action(v, fragment_seq, sub_item_id.as_deref(), doc_id)?;
            if sub_item_id.is_some() || sub_item_index.is_some() {
                let (sub, warning) = find_sub_item_mut_by_id(
                    item,
                    sub_item_id.as_deref(),
                    sub_item_index,
                    fragment_seq,
                    doc_id,
                )?;
                if let Some(w) = warning {
                    warnings.push(w);
                }
                sub.status = "skipped".to_string();
            } else {
                item.status = "skipped".to_string();
            }
            v.updated_at = now.clone();
            v.status = recompute_verification_status(&v.items);
        }
        "add_item" => {
            let doc_slug = doc.slug.clone();
            let v = verification_mut(&mut doc, doc_id)?;
            match arguments.get("fragment_seq").and_then(|v| v.as_u64()) {
                None => {
                    // Freeform top-level item (spec §7.2): fragment_seq
                    // omitted/null, label required.
                    let label = arguments
                        .get("label")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "'label' is required for add_item when 'fragment_seq' is omitted"
                            )
                        })?
                        .to_string();
                    let category = arguments
                        .get("category")
                        .and_then(|v| v.as_str())
                        .unwrap_or("visual")
                        .to_string();
                    v.items.push(VerificationItem {
                        fragment_seq: None,
                        heading: label.clone(),
                        status: "pending".to_string(),
                        impl_refs: Vec::new(),
                        test_refs: Vec::new(),
                        reviewer: None,
                        verified_at: None,
                        notes: String::new(),
                        content_hash_at_verify: None,
                        category,
                        sub_items: Vec::new(),
                        label: Some(label),
                    });
                }
                Some(seq) => {
                    // Sub-item on an existing section item (spec §7.2):
                    // fragment_seq given, description required.
                    let fragment_seq = seq as usize;
                    let description = arguments
                        .get("description")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "'description' is required for add_item when 'fragment_seq' is given"
                            )
                        })?
                        .to_string();
                    let category = arguments
                        .get("category")
                        .and_then(|v| v.as_str())
                        .unwrap_or("requirement")
                        .to_string();

                    // Requirements-traceability P0 §2.3 (t300.3): a brand
                    // new SubItem never has a stable_id yet, so mint one —
                    // unless its description fuzzy-matches an existing
                    // SubItem elsewhere in the matrix, in which case it
                    // re-links to that SubItem's (immutable) stable_id
                    // instead of minting a fresh one (§2.3 "再マッチング").
                    let existing_ids = collect_stable_ids(v);
                    let reused_id = find_fuzzy_match_stable_id(v, &description);
                    let (stable_id, warning) = match reused_id {
                        Some(id) => (id, None),
                        None => {
                            let heading = v
                                .items
                                .iter()
                                .find(|i| i.fragment_seq == Some(fragment_seq))
                                .map(|i| i.heading.clone())
                                .unwrap_or_default();
                            let (id, warning) =
                                derive_stable_id(&doc_slug, &heading, &description, &existing_ids);
                            (id, warning)
                        }
                    };
                    if let Some(w) = warning {
                        warnings.push(w);
                    }

                    // M0-b (wiki/220-vmodel-integration-design.md §4.2,
                    // FR-105): `existing_ids`/`derive_stable_id` above only
                    // guard against a collision *within this document* — a
                    // hand-authored or independently-imported `stable_id` in
                    // a different document is invisible to that check. Warn
                    // (never refuse) when the id this call is about to
                    // assign already exists elsewhere, so the ambiguity is
                    // visible before `resolve_stable_ids_in` later refuses
                    // to link either copy.
                    if let Some(w) = cross_document_collision_warning(handoff, doc_id, &stable_id)?
                    {
                        warnings.push(w);
                    }

                    let item = find_item_mut(v, fragment_seq, doc_id)?;
                    let index = item.sub_items.len();
                    item.sub_items.push(SubItem {
                        index,
                        description,
                        category,
                        stable_id: Some(stable_id),
                        ..Default::default()
                    });
                }
            }
            v.updated_at = now.clone();
            v.status = recompute_verification_status(&v.items);
        }
        "sync" if doc.layer.is_some() => {
            // wiki/220 §2.4: on a layer document, `doc_verify(sync)`
            // delegates entirely to the layer sync instead of the plain
            // per-section rebuild below (which knows nothing about body
            // items/stable_ids).
            let body = read_doc_body(handoff, &doc.slug)?.unwrap_or_default();
            let id_prefixes = read_config(&handoff.join("config.toml"))
                .map(|c| c.trace.id_prefixes)
                .unwrap_or_default();
            let outcome = sync_layer_items(&mut doc, &body, &id_prefixes, &now);
            warnings.extend(outcome.warnings);
            // Rework round 2 (MAJOR fix): keep `source.body_raw_hash` in
            // sync with the body this explicit sync just parsed, same as
            // `sync_layer_items_if_needed` does for `doc_save`/
            // `doc_update_section` — otherwise a later metadata-only
            // `doc_save` would see a stale/absent hash and pay a redundant
            // re-sync (or, if a hash happened to already be recorded from an
            // older body, could wrongly skip a sync that this action already
            // superseded).
            doc.source.body_raw_hash = Some(lexsim::fnv1a_hex(body.as_bytes()));
            if let Some(v) = &doc.verification {
                warnings.extend(duplicate_stable_id_warnings_within_doc(v));
            }
        }
        "sync" => {
            let sections = doc.sections.clone();
            let v = verification_mut(&mut doc, doc_id)?;
            let current_seqs: std::collections::HashSet<usize> =
                sections.iter().map(|s| s.seq).collect();
            // Freeform items (fragment_seq=None, v2) are never section-tied,
            // so `sync` always keeps them — only section-tied items whose
            // seq no longer exists are dropped.
            v.items.retain(|i| match i.fragment_seq {
                Some(seq) => current_seqs.contains(&seq),
                None => true,
            });
            let existing_seqs: std::collections::HashSet<usize> =
                v.items.iter().filter_map(|i| i.fragment_seq).collect();
            for s in &sections {
                if !existing_seqs.contains(&s.seq) {
                    v.items.push(VerificationItem {
                        fragment_seq: Some(s.seq),
                        heading: s.heading.clone(),
                        status: "pending".to_string(),
                        impl_refs: Vec::new(),
                        test_refs: Vec::new(),
                        reviewer: None,
                        verified_at: None,
                        notes: String::new(),
                        content_hash_at_verify: None,
                        category: "section".to_string(),
                        sub_items: Vec::new(),
                        label: None,
                    });
                }
            }
            // Sort section-tied items by seq; freeform items (None) sort
            // last, in their prior relative order (stable sort).
            v.items.sort_by_key(|i| (i.fragment_seq.is_none(), i.fragment_seq));
            v.updated_at = now.clone();
            v.status = recompute_verification_status(&v.items);
        }
        "set_refs" => {
            // FR-806 (§4.1): fragment_seq is optional when sub_item_id is
            // given (see locate_item_for_sub_item_action).
            let fragment_seq = arguments
                .get("fragment_seq")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            let impl_refs = arguments.get("impl_refs").map(code_refs_from_value);
            let test_refs = arguments.get("test_refs").map(code_refs_from_value);
            let sub_item_id = arguments
                .get("sub_item_id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let sub_item_index = arguments
                .get("sub_item_index")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);

            let v = verification_mut(&mut doc, doc_id)?;
            let item =
                locate_item_for_sub_item_action(v, fragment_seq, sub_item_id.as_deref(), doc_id)?;
            if sub_item_id.is_some() || sub_item_index.is_some() {
                let (sub, warning) = find_sub_item_mut_by_id(
                    item,
                    sub_item_id.as_deref(),
                    sub_item_index,
                    fragment_seq,
                    doc_id,
                )?;
                if let Some(w) = warning {
                    warnings.push(w);
                }
                if let Some(impl_refs) = impl_refs {
                    sub.impl_refs = impl_refs;
                }
                if let Some(test_refs) = test_refs {
                    sub.test_refs = test_refs;
                }
            } else {
                if let Some(impl_refs) = impl_refs {
                    item.impl_refs = impl_refs;
                }
                if let Some(test_refs) = test_refs {
                    item.test_refs = test_refs;
                }
            }
            v.updated_at = now.clone();
            v.status = recompute_verification_status(&v.items);
        }
        "set_dev_stage" => {
            // FR-806 (§4.1): fragment_seq is optional when sub_item_id is
            // given (see locate_item_for_sub_item_action).
            let fragment_seq = arguments
                .get("fragment_seq")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            let dev_stage = arguments
                .get("dev_stage")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("'dev_stage' is required for set_dev_stage"))?;
            const VALID_DEV_STAGES: [&str; 5] =
                ["not_started", "in_progress", "implemented", "tested", "verified"];
            if !VALID_DEV_STAGES.contains(&dev_stage) {
                anyhow::bail!(
                    "Invalid dev_stage '{dev_stage}'; expected one of {VALID_DEV_STAGES:?}"
                );
            }
            let sub_item_id = arguments
                .get("sub_item_id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let sub_item_index = arguments
                .get("sub_item_index")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            if sub_item_id.is_none() && sub_item_index.is_none() {
                anyhow::bail!(
                    "set_dev_stage requires 'sub_item_id' or 'sub_item_index'; dev_stage is a SubItem-only field"
                );
            }

            let v = verification_mut(&mut doc, doc_id)?;
            let item =
                locate_item_for_sub_item_action(v, fragment_seq, sub_item_id.as_deref(), doc_id)?;
            let (sub, warning) = find_sub_item_mut_by_id(
                item,
                sub_item_id.as_deref(),
                sub_item_index,
                fragment_seq,
                doc_id,
            )?;
            if let Some(w) = warning {
                warnings.push(w);
            }
            sub.dev_stage = Some(dev_stage.to_string());
            v.updated_at = now.clone();
            v.status = recompute_verification_status(&v.items);
        }
        "set_priority" => {
            // FR-806 (§4.1): fragment_seq is optional when sub_item_id is
            // given (see locate_item_for_sub_item_action).
            let fragment_seq = arguments
                .get("fragment_seq")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            let priority = arguments
                .get("priority")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("'priority' is required for set_priority"))?;
            const VALID_PRIORITIES: [&str; 4] = ["P0", "P1", "P2", "P3"];
            if !VALID_PRIORITIES.contains(&priority) {
                anyhow::bail!("Invalid priority '{priority}'; expected one of {VALID_PRIORITIES:?}");
            }
            let sub_item_id = arguments
                .get("sub_item_id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let sub_item_index = arguments
                .get("sub_item_index")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            if sub_item_id.is_none() && sub_item_index.is_none() {
                anyhow::bail!(
                    "set_priority requires 'sub_item_id' or 'sub_item_index'; priority is a SubItem-only field"
                );
            }

            let v = verification_mut(&mut doc, doc_id)?;
            let item =
                locate_item_for_sub_item_action(v, fragment_seq, sub_item_id.as_deref(), doc_id)?;
            let (sub, warning) = find_sub_item_mut_by_id(
                item,
                sub_item_id.as_deref(),
                sub_item_index,
                fragment_seq,
                doc_id,
            )?;
            if let Some(w) = warning {
                warnings.push(w);
            }
            sub.priority = Some(priority.to_string());
            v.updated_at = now.clone();
            v.status = recompute_verification_status(&v.items);
        }
        "link_task" => {
            // FR-806 (§4.1): fragment_seq is optional when sub_item_id is
            // given (see locate_item_for_sub_item_action).
            let fragment_seq = arguments
                .get("fragment_seq")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            let task_ids = arguments
                .get("task_ids")
                .map(string_array_value)
                .ok_or_else(|| anyhow::anyhow!("'task_ids' is required for link_task"))?;
            let sub_item_id = arguments
                .get("sub_item_id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let sub_item_index = arguments
                .get("sub_item_index")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            if sub_item_id.is_none() && sub_item_index.is_none() {
                anyhow::bail!(
                    "link_task requires 'sub_item_id' or 'sub_item_index'; task_ids is a SubItem-only field"
                );
            }

            let v = verification_mut(&mut doc, doc_id)?;
            let item =
                locate_item_for_sub_item_action(v, fragment_seq, sub_item_id.as_deref(), doc_id)?;
            let (sub, warning) = find_sub_item_mut_by_id(
                item,
                sub_item_id.as_deref(),
                sub_item_index,
                fragment_seq,
                doc_id,
            )?;
            if let Some(w) = warning {
                warnings.push(w);
            }
            let stable_id = sub.stable_id.clone();
            let old_task_ids = sub.task_ids.clone();
            sub.task_ids = task_ids.clone();
            v.updated_at = now.clone();
            v.status = recompute_verification_status(&v.items);

            // Reverse link (spec §3.1): every linked task's task_links gets
            // a `{target: doc_id, link_type: "requirement", label: stable_id}`
            // entry, deduped so re-calling link_task with the same task_ids
            // is idempotent.
            let unresolved = add_reverse_task_links(handoff, doc_id, stable_id.as_deref(), &task_ids)?;
            if !unresolved.is_empty() {
                warnings.push(format!(
                    "Could not resolve task id(s) for linking: {}",
                    unresolved.join(", ")
                ));
            }

            // t323/M4: tasks that were previously linked but are not in the
            // new `task_ids` must have this SubItem's reverse-link entry
            // (labeled with its own `stable_id`) removed. A task still
            // linked from a *different* SubItem keeps that sibling's own
            // labeled entry untouched — see `remove_stale_reverse_links`.
            if let Some(stable_id) = stable_id.as_deref() {
                let removed_task_ids: Vec<String> = old_task_ids
                    .iter()
                    .filter(|id| !task_ids.contains(id))
                    .cloned()
                    .collect();
                if !removed_task_ids.is_empty() {
                    remove_stale_reverse_links(handoff, &doc, stable_id, &removed_task_ids)?;
                }
            }
        }
        "backfill_stable_ids" => {
            let doc_slug = doc.slug.clone();
            let v = verification_mut(&mut doc, doc_id)?;
            let mut existing_ids = collect_stable_ids(v);
            let mut backfilled = 0u64;

            for item in v.items.iter_mut() {
                let heading = item.heading.clone();
                for sub in item.sub_items.iter_mut() {
                    if sub.stable_id.is_some() {
                        continue;
                    }
                    let (id, warning) =
                        derive_stable_id(&doc_slug, &heading, &sub.description, &existing_ids);
                    if let Some(w) = warning {
                        warnings.push(w);
                    }
                    existing_ids.insert(id.clone());
                    sub.stable_id = Some(id);
                    backfilled += 1;
                }
            }
            v.updated_at = now.clone();
            v.status = recompute_verification_status(&v.items);

            write_doc(handoff, &doc)?;
            let all_docs = read_all_docs(handoff)?;
            write_requirements_summary(handoff, &all_docs)?;

            let v = doc
                .verification
                .as_ref()
                .expect("verification was just set/mutated above");
            let counts = count_verification(&doc, v);
            return Ok(to_json(&json!({
                "doc_id": doc.id,
                "backfilled": backfilled,
                "verification_status": v.status,
                "checked": counts.checked,
                "skipped": counts.skipped,
                "pending": counts.pending,
                "total": counts.total,
                "stale": counts.stale,
                "warnings": warnings,
            })));
        }
        other => anyhow::bail!(
            "Unknown action '{other}'; expected one of generate, check, check_all, skip, sync, set_refs, set_dev_stage, set_priority, link_task, add_item, backfill_stable_ids, suggest_refs"
        ),
    }

    write_doc(handoff, &doc)?;

    // Requirements-traceability integration-reform §3.2: refresh the
    // VSCode-extension-facing `_requirements_summary.json` cache after any
    // action that can change requirement (SubItem) progress or composition.
    // `add_item` is included here — a new SubItem changes `total`'s
    // composition and the extension's Requirements Explorer should reflect
    // it immediately. This does not double the cost for `req_import`'s bulk
    // path: that handler calls `add_item`'s underlying mutation directly
    // (not through this action dispatch) and refreshes the summary itself
    // exactly once after the whole batch. `backfill_stable_ids` (§3.3) is
    // not listed here — it early-returns above with its own write +
    // summary refresh, since its response shape (a `backfilled` count)
    // differs from every other action's mutation-count summary.
    const SUMMARY_REFRESH_ACTIONS: [&str; 10] = [
        "generate",
        "check",
        "check_all",
        "skip",
        "sync",
        "set_refs",
        "set_dev_stage",
        "set_priority",
        "add_item",
        "link_task",
    ];
    if SUMMARY_REFRESH_ACTIONS.contains(&action) {
        let all_docs = read_all_docs(handoff)?;
        write_requirements_summary(handoff, &all_docs)?;
    }

    let v = doc
        .verification
        .as_ref()
        .expect("verification was just set/mutated above");
    let counts = count_verification(&doc, v);

    Ok(to_json(&json!({
        "doc_id": doc.id,
        "verification_status": v.status,
        "checked": counts.checked,
        "skipped": counts.skipped,
        "pending": counts.pending,
        "total": counts.total,
        "stale": counts.stale,
        "warnings": warnings,
    })))
}

fn required_fragment_seq(arguments: &Value) -> Result<usize> {
    arguments
        .get("fragment_seq")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .ok_or_else(|| anyhow::anyhow!("'fragment_seq' is required for this action"))
}

/// Like `required_fragment_seq`, but accepts `fragment_seq` as either a
/// single number (backward compat) or an array of numbers (batch `check`).
fn required_fragment_seqs(arguments: &Value) -> Result<Vec<usize>> {
    match arguments.get("fragment_seq") {
        Some(Value::Array(arr)) => {
            let seqs: Vec<usize> = arr
                .iter()
                .filter_map(|v| v.as_u64())
                .map(|n| n as usize)
                .collect();
            if seqs.is_empty() {
                anyhow::bail!("'fragment_seq' array must contain at least one section seq");
            }
            Ok(seqs)
        }
        _ => required_fragment_seq(arguments).map(|seq| vec![seq]),
    }
}

fn verification_mut<'a>(doc: &'a mut DocMetadata, doc_id: &str) -> Result<&'a mut Verification> {
    doc.verification.as_mut().ok_or_else(|| {
        anyhow::anyhow!(
            "No verification matrix exists for document {doc_id}; use action='generate' first"
        )
    })
}

/// Locates the `SubItem` a [`ResolvedSubItem`] points to (FR-806 §4.1).
/// Section-tied SubItems (`fragment_seq: Some`) are found directly via
/// their known `(fragment_seq, sub_item_index)` position — same O(1) lookup
/// as before this task. Freeform SubItems (`fragment_seq: None`) have no
/// section to index by, so they are instead resolved by scanning every
/// item's `sub_items` for a matching `stable_id` — this is what lets
/// `link_requirements_to_task` / `unlink_requirements_from_task` /
/// `propagate_dev_stage_for_task` operate on freeform SubItems at all (they
/// used to assume every resolved SubItem had `Some(fragment_seq)`).
fn resolved_sub_item_mut<'a>(
    v: &'a mut Verification,
    r: &ResolvedSubItem,
    doc_id: &str,
) -> Result<&'a mut SubItem> {
    match r.fragment_seq {
        Some(seq) => {
            let item = find_item_mut(v, seq, doc_id)?;
            find_sub_item_mut(item, r.sub_item_index, seq, doc_id)
        }
        None => v
            .items
            .iter_mut()
            .flat_map(|i| i.sub_items.iter_mut())
            .find(|s| s.stable_id.as_deref() == Some(r.stable_id.as_str()))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No freeform sub_item with stable_id={:?} in document {doc_id}",
                    r.stable_id
                )
            }),
    }
}

/// Locates the `VerificationItem` that owns a `handoff_doc_verify` action's
/// target sub_item (FR-806 §4.1): when `fragment_seq` is given, resolution
/// is unchanged from before this task (section-addressed, via
/// `find_item_mut`). When `fragment_seq` is omitted, it is only valid
/// together with `sub_item_id` — every item (including freeform ones,
/// `fragment_seq: None`) is scanned for a `SubItem` carrying that
/// `stable_id`, since there is no section to anchor a positional lookup
/// otherwise (`sub_item_index` alone can't identify *which* item's
/// `sub_items` array to index into without a `fragment_seq`).
fn locate_item_for_sub_item_action<'a>(
    v: &'a mut Verification,
    fragment_seq: Option<usize>,
    sub_item_id: Option<&str>,
    doc_id: &str,
) -> Result<&'a mut VerificationItem> {
    if let Some(seq) = fragment_seq {
        return find_item_mut(v, seq, doc_id);
    }
    let id = sub_item_id.ok_or_else(|| {
        anyhow::anyhow!(
            "'fragment_seq' is required unless 'sub_item_id' is given (to address a freeform sub_item)"
        )
    })?;
    v.items
        .iter_mut()
        .find(|i| {
            i.sub_items
                .iter()
                .any(|s| s.stable_id.as_deref() == Some(id))
        })
        .ok_or_else(|| anyhow::anyhow!("No sub_item with stable_id={id:?} in document {doc_id}"))
}

fn find_item_mut<'a>(
    v: &'a mut Verification,
    fragment_seq: usize,
    doc_id: &str,
) -> Result<&'a mut VerificationItem> {
    v.items
        .iter_mut()
        .find(|i| i.fragment_seq == Some(fragment_seq))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No verification item at fragment_seq={fragment_seq} for document {doc_id}"
            )
        })
}

/// Requirements-traceability P0 (`.handoff/docs/_doc.req-traceability-mcp-plan.md`
/// §2.3, t300.3): derives a `SubItem::stable_id` the first time one is
/// assigned. Returns `(stable_id, warning)` — `warning` is `Some` only when
/// the derived id collided with `existing_ids` and a numeric suffix (`-2`,
/// `-3`, ...) had to be appended to disambiguate it.
///
/// Derivation (never re-run once a `stable_id` exists — callers are
/// responsible for that immutability check; this function always mints):
/// 1. Category prefix from `doc_slug`: `req-c(\d+)...` -> `C{n}` (zero-padded
///    as found, e.g. `req-c01-...` -> `C01`). No match -> the whole slug,
///    uppercased with `-`/`_` normalized to `-`.
/// 2. Requirement-id prefix in `description` (wiki/210-req-traceability-refinement.md
///    §M2): a leading known prefix (`FR`, `NFR`, `REQ`, `CR`, `TR`, `SR`,
///    `UC`, `TC`) + `-\d+` (e.g. `FR-001: USB CDC...` -> `FR-001`). When
///    present, the id is `{category}-{req_id}` (e.g. `C01-FR-001`) —
///    tried *before* the heading/description-number derivation below, since
///    a requirement-id prefix is a stronger signal than a bare leading
///    numeral. Only the known prefix list is recognized; a generic
///    `[A-Z]{1,5}-\d+` pattern is deliberately not used, since it would
///    false-positive on text like `A-1 pin header`.
/// 3. Heading number from `heading`: leading `#`s + optional whitespace,
///    then a leading `\d[\d.]*` run (e.g. `## 2.1 基板外形` -> `2.1`).
/// 4. Description number: a leading `\d[\d.]*` run in `description` (e.g.
///    `2.1.1 外形形状定義` -> `2.1.1`). When present, the id is
///    `{category}-{desc_num}` (the description's own number already
///    subsumes the heading number in practice — e.g. `2.1.1` under heading
///    `2.1`). When absent, the id is `{category}-{heading_num}-{desc_slug}`
///    where `desc_slug` is the description lowercased with every non
///    ASCII-alphanumeric run collapsed to a single `-` (leading/trailing
///    hyphens trimmed), truncated to `slugify`'s default max length.
/// 5. Collision: while the candidate is in `existing_ids`, append `-2`,
///    `-3`, ... and return a warning describing the collision.
pub(crate) fn derive_stable_id(
    doc_slug: &str,
    heading: &str,
    description: &str,
    existing_ids: &std::collections::HashSet<String>,
) -> (String, Option<String>) {
    let category = extract_category_prefix(doc_slug);

    let base = if let Some(req_id) = extract_requirement_id(description.trim()) {
        format!("{category}-{req_id}")
    } else {
        let heading_num = extract_leading_number(heading.trim_start_matches('#').trim());
        let desc_num = extract_leading_number(description.trim());
        match desc_num {
            Some(n) => format!("{category}-{n}"),
            None => {
                let slug = slugify(description, DEFAULT_SLUGIFY_MAX_LEN);
                match heading_num {
                    Some(h) if !slug.is_empty() => format!("{category}-{h}-{slug}"),
                    Some(h) => format!("{category}-{h}"),
                    None if !slug.is_empty() => format!("{category}-{slug}"),
                    None => category.clone(),
                }
            }
        }
    };

    if !existing_ids.contains(&base) {
        return (base, None);
    }

    let mut suffix = 2;
    loop {
        let candidate = format!("{base}-{suffix}");
        if !existing_ids.contains(&candidate) {
            let warning =
                format!("stable_id {base:?} already exists; assigned {candidate:?} instead");
            return (candidate, Some(warning));
        }
        suffix += 1;
    }
}

/// Extracts the `C{n}` category prefix from a `req-c{n}-...`-shaped slug
/// (e.g. `req-c01-board-setup` -> `C01`, preserving the digits as written).
/// Falls back to the whole slug, uppercased with `_`/`-` normalized to `-`,
/// when the `req-c<digits>` pattern isn't found.
fn extract_category_prefix(doc_slug: &str) -> String {
    let lower = doc_slug.to_lowercase();
    if let Some(rest) = lower.strip_prefix("req-c") {
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            return format!("C{digits}");
        }
    }
    doc_slug
        .to_uppercase()
        .chars()
        .map(|c| if c == '_' { '-' } else { c })
        .collect()
}

/// Known requirement-id prefixes recognized by `extract_requirement_id`
/// (wiki/210-req-traceability-refinement.md §M2). Intentionally a fixed
/// allow-list rather than a generic `[A-Z]{1,5}-\d+` pattern — a generic
/// pattern would false-positive on ordinary text like `A-1 pin header`.
const KNOWN_REQUIREMENT_ID_PREFIXES: &[&str] = &["FR", "NFR", "REQ", "CR", "TR", "SR", "UC", "TC"];

/// Extracts a leading `{PREFIX}-{digits}` requirement id from `text` (e.g.
/// `"FR-001: USB CDC..."` -> `Some("FR-001")`), where `PREFIX` is one of
/// `KNOWN_REQUIREMENT_ID_PREFIXES`, matched case-insensitively but returned
/// in the list's canonical (upper) case. Only the longest matching known
/// prefix immediately followed by `-` and one or more ASCII digits at the
/// very start of `text` counts — no match anywhere else in the text is
/// considered, so `"see FR-001"` does not match (avoids over-eager minting
/// from incidental references inside a longer description).
pub(crate) fn extract_requirement_id(text: &str) -> Option<String> {
    // Try longest prefixes first so e.g. `NFR-005` isn't mistakenly matched
    // as `FR` against `NFR-005` slicing from the wrong offset (in practice
    // prefixes are disjoint by spelling, but sorting by length keeps the
    // intent explicit and future-proofs additions like `FR`/`NFRX`).
    let mut prefixes: Vec<&str> = KNOWN_REQUIREMENT_ID_PREFIXES.to_vec();
    prefixes.sort_by_key(|p| std::cmp::Reverse(p.len()));

    let upper = text.to_uppercase();
    for prefix in prefixes {
        let Some(rest) = upper.strip_prefix(prefix) else {
            continue;
        };
        let Some(digits_part) = rest.strip_prefix('-') else {
            continue;
        };
        let digit_len = digits_part
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .count();
        if digit_len == 0 {
            continue;
        }
        // `text` may be non-ASCII (e.g. Japanese) after the numeric run, but
        // the matched prefix+digits span is always pure ASCII, so byte
        // slicing on `text` at this offset is safe.
        let match_len = prefix.len() + 1 + digit_len;
        return Some(text[..match_len].to_uppercase());
    }
    None
}

/// Extracts a leading `\d[\d.]*` numeric run (e.g. `"2.1.1 外形"` -> `Some("2.1.1")`,
/// `"外形"` -> `None`). Trailing `.` on the run is trimmed (e.g. a heading
/// written as `"2.1."` yields `"2.1"`).
fn extract_leading_number(text: &str) -> Option<String> {
    let mut end = 0;
    for (i, c) in text.char_indices() {
        if c.is_ascii_digit() || c == '.' {
            end = i + c.len_utf8();
        } else {
            break;
        }
    }
    if end == 0 {
        return None;
    }
    let num = text[..end].trim_end_matches('.');
    if num.is_empty() || !num.chars().next().unwrap().is_ascii_digit() {
        None
    } else {
        Some(num.to_string())
    }
}

/// Default `max_len` passed to `slugify` by `derive_stable_id` (wiki/210
/// §M2) — keeps minted `stable_id`s readable instead of embedding an entire
/// long description.
const DEFAULT_SLUGIFY_MAX_LEN: usize = 40;

/// Slugifies free text for use as a `stable_id` fallback suffix: lowercased,
/// every run of non-ASCII-alphanumeric characters collapsed to a single `-`,
/// leading/trailing hyphens trimmed, then truncated to at most `max_len`
/// characters at a word (hyphen) boundary — i.e. the last complete
/// hyphen-separated word that still fits is kept, rather than cutting
/// mid-word (wiki/210-req-traceability-refinement.md §M2). Non-ASCII text
/// (e.g. Japanese) has no ASCII-alphanumeric characters at all, so it
/// collapses to an empty string — callers fall back further (heading number
/// alone, or bare category).
fn slugify(text: &str, max_len: usize) -> String {
    let mut out = String::new();
    let mut last_was_hyphen = true; // suppress leading hyphen
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_was_hyphen = false;
        } else if !last_was_hyphen {
            out.push('-');
            last_was_hyphen = true;
        }
    }
    let out = out.trim_end_matches('-').to_string();

    if out.len() <= max_len {
        return out;
    }
    // Truncate to max_len bytes (slug is pure ASCII, so byte length ==
    // char length here), then trim back to the last complete word: drop any
    // trailing partial word after the last '-' within the truncated slice,
    // falling back to a hard byte truncation only if there's no '-' at all
    // within the limit (a single word longer than max_len).
    let truncated = &out[..max_len];
    match truncated.rfind('-') {
        Some(idx) => truncated[..idx].to_string(),
        None => truncated.to_string(),
    }
}

/// Normalizes description text for fuzzy comparison: trims whitespace,
/// drops trailing Japanese/ASCII punctuation (`。`, `.`, `、`, `,`), and
/// lowercases ASCII.
fn normalize_for_match(text: &str) -> String {
    text.trim()
        .trim_end_matches(['。', '.', '、', ',', '！', '!', '？', '?'])
        .to_lowercase()
}

/// Requirements-traceability P0 §2.3 (t300.3): a deliberately simple
/// "fuzzy" match — used by `sync`/`add_item` to decide whether a new
/// SubItem's description should re-link to an existing SubItem's
/// `stable_id` rather than mint a new one. Per the plan (§2.3 note:
/// "lexsim は避ける"), this is plain normalization + substring containment,
/// not edit-distance — sufficient for the common cases (identical text
/// modulo trailing punctuation, or one description being a superset of the
/// other, e.g. a heading-numbered description added in front of existing
/// free text).
pub(crate) fn descriptions_fuzzy_match(a: &str, b: &str) -> bool {
    let na = normalize_for_match(a);
    let nb = normalize_for_match(b);
    if na.is_empty() || nb.is_empty() {
        return false;
    }
    na == nb || na.contains(&nb) || nb.contains(&na)
}

/// Collects every already-assigned `stable_id` across all `sub_items` in the
/// verification matrix (used as the collision set for `derive_stable_id`).
pub(crate) fn collect_stable_ids(v: &Verification) -> std::collections::HashSet<String> {
    v.items
        .iter()
        .flat_map(|i| i.sub_items.iter())
        .filter_map(|s| s.stable_id.clone())
        .collect()
}

/// Requirements-traceability P0 §2.3 (t300.3) "再マッチング": scans every
/// `sub_items` entry in the matrix for one whose description
/// `descriptions_fuzzy_match`es `description`, and returns its `stable_id`
/// so a newly observed SubItem with (near-)identical text re-links to the
/// existing requirement instead of minting a duplicate id. Returns `None`
/// when there is no match, or the match has no `stable_id` yet.
fn find_fuzzy_match_stable_id(v: &Verification, description: &str) -> Option<String> {
    v.items
        .iter()
        .flat_map(|i| i.sub_items.iter())
        .find(|s| descriptions_fuzzy_match(&s.description, description))
        .and_then(|s| s.stable_id.clone())
}

/// v2: finds a `SubItem` by `index` within `item.sub_items` (used by
/// `check`/`skip` when `sub_item_index` is given).
fn find_sub_item_mut<'a>(
    item: &'a mut VerificationItem,
    sub_index: usize,
    fragment_seq: usize,
    doc_id: &str,
) -> Result<&'a mut SubItem> {
    item.sub_items.get_mut(sub_index).ok_or_else(|| {
        anyhow::anyhow!(
            "No sub_item at index={sub_index} for fragment_seq={fragment_seq} on document {doc_id}"
        )
    })
}

/// Requirements-traceability P0 (`.handoff/docs/_doc.req-traceability-mcp-plan.md`
/// §2.5): finds a `SubItem` addressed primarily by its stable `sub_item_id`
/// (`SubItem::stable_id`), falling back to positional `sub_item_index` when
/// no id is given. When both are given and disagree, `sub_item_id` wins and
/// a warning describing the mismatch is returned alongside the match.
///
/// Wired into `check`/`skip` (this task). `set_refs`/`set_dev_stage`/
/// `set_priority` addressing is a follow-up task (t300.2).
///
/// `fragment_seq` is `Option<usize>` (FR-806 §4.1): `None` when the caller
/// addressed the item without one (only valid together with `sub_item_id` —
/// see `locate_item_for_sub_item_action`), used only to phrase error/warning
/// messages.
fn find_sub_item_mut_by_id<'a>(
    item: &'a mut VerificationItem,
    sub_item_id: Option<&str>,
    sub_item_index: Option<usize>,
    fragment_seq: Option<usize>,
    doc_id: &str,
) -> Result<(&'a mut SubItem, Option<String>)> {
    let fragment_desc = match fragment_seq {
        Some(seq) => format!("fragment_seq={seq}"),
        None => "no fragment_seq (freeform item)".to_string(),
    };
    match sub_item_id {
        Some(id) => {
            let by_index_matches = sub_item_index.is_some_and(|idx| {
                item.sub_items.get(idx).and_then(|s| s.stable_id.as_deref()) != Some(id)
            });
            let warning = by_index_matches.then(|| {
                format!(
                    "sub_item_id={id:?} and sub_item_index={:?} were both given and disagree; \
                     sub_item_id takes precedence for {fragment_desc} on document {doc_id}",
                    sub_item_index.unwrap()
                )
            });
            let sub = item
                .sub_items
                .iter_mut()
                .find(|s| s.stable_id.as_deref() == Some(id))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "No sub_item with stable_id={id:?} for {fragment_desc} on document {doc_id}"
                    )
                })?;
            Ok((sub, warning))
        }
        None => {
            let sub_index = sub_item_index.ok_or_else(|| {
                anyhow::anyhow!(
                    "Either 'sub_item_id' or 'sub_item_index' is required to address a sub_item"
                )
            })?;
            let fragment_seq = fragment_seq.ok_or_else(|| {
                anyhow::anyhow!(
                    "'fragment_seq' is required when addressing a sub_item by 'sub_item_index'"
                )
            })?;
            let sub = find_sub_item_mut(item, sub_index, fragment_seq, doc_id)?;
            Ok((sub, None))
        }
    }
}

/// `handoff_doc_repair_task_ids` — the explicit-repair entry point for
/// [`rebuild_item_task_ids_full`] (t360.7, wiki/220 §2.5 / §4.3 r3). Every
/// live link-change path (`handoff_update_task(requirement_ids=...)`,
/// `handoff_doc_verify(action="link_task")`) and layer sync already keep
/// `SubItem.task_ids` in sync differentially as they run; this tool exists
/// for the rare case that state has drifted anyway (manual edits, a bug, a
/// corpus imported from elsewhere) and a caller wants to force a full,
/// all-tasks-scanning resync across every document. Still gated by the same
/// §4.3 `tasks_*` input fingerprint as `trace_report`'s self-repair (t360.10)
/// — a call with nothing changed since the last full-corpus sync is a cheap
/// no-op, not a forced rescan; takes no arguments beyond the standard
/// `project_dir`.
pub fn handle_doc_repair_task_ids(ctx: &HandlerContext, _arguments: &Value) -> Result<String> {
    let outcome = rebuild_item_task_ids_full(&ctx.handoff_dir)?;
    Ok(to_json(&json!({
        "ran": outcome.ran,
        "sub_items_changed": outcome.sub_items_changed,
        "docs_changed": outcome.docs_changed,
    })))
}

/// `handoff_doc_verify_status` — verification matrix summary + optional
/// per-item detail with stale detection (wiki/140-verification-matrix.md
/// §4.2).
pub fn handle_doc_verify_status(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let doc_id = arguments
        .get("doc_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'doc_id' is required"))?;
    let include_items = arguments
        .get("include_items")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let format = arguments
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("json");

    let doc = resolve_doc(handoff, doc_id)?
        .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;

    let v = doc.verification.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "No verification matrix exists for document {doc_id}; use handoff_doc_verify(action='generate') first"
        )
    })?;

    let counts = count_verification(&doc, v);
    let percentage = if counts.total == 0 {
        0.0
    } else {
        (counts.checked + counts.skipped) as f64 / counts.total as f64 * 100.0
    };

    if format == "checklist" {
        return Ok(render_verification_checklist(&doc, v, &counts, percentage));
    }

    let mut out = json!({
        "doc_id": doc.id,
        "title": doc.title,
        "verification_status": v.status,
        "progress": {
            "checked": counts.checked,
            "skipped": counts.skipped,
            "pending": counts.pending,
            "total": counts.total,
            "stale": counts.stale,
            "percentage": percentage,
        },
    });

    if include_items {
        let items: Vec<Value> = v
            .items
            .iter()
            .map(|i| {
                let sub_items: Vec<Value> = i
                    .sub_items
                    .iter()
                    .map(|s| {
                        json!({
                            "index": s.index,
                            "description": s.description,
                            "status": s.status,
                            "reviewer": s.reviewer,
                            "verified_at": s.verified_at,
                            "notes": s.notes,
                            "category": s.category,
                            "stable_id": s.stable_id,
                            "priority": s.priority,
                            "dev_stage": s.dev_stage,
                            "impl_refs": s.impl_refs,
                            "test_refs": s.test_refs,
                            "task_ids": s.task_ids,
                            "depends_on": s.depends_on,
                        })
                    })
                    .collect();
                json!({
                    "fragment_seq": i.fragment_seq,
                    "heading": i.heading,
                    "status": i.status,
                    "stale": item_is_stale(&doc, i),
                    "impl_refs": i.impl_refs,
                    "test_refs": i.test_refs,
                    "reviewer": i.reviewer,
                    "verified_at": i.verified_at,
                    "notes": i.notes,
                    "category": i.category,
                    "sub_items": sub_items,
                    "label": i.label,
                })
            })
            .collect();
        out["items"] = json!(items);
    }

    Ok(to_json(&out))
}

/// Status icon + label used by the `format="checklist"` Markdown rendering
/// (spec §7.3): `✓ verified`, `⊘ skipped`, `○ pending`.
fn status_icon(status: &str) -> String {
    match status {
        "verified" => "✓ verified".to_string(),
        "skipped" => "⊘ skipped".to_string(),
        other => format!("○ {other}"),
    }
}

/// Renders a document's verification matrix as a Markdown checklist (v2,
/// wiki/140-verification-matrix.md §7.3): one `##` block per top-level item
/// (`§{seq} {heading}` for section-tied items, `— {label}` for freeform
/// items), with impl/test refs and a `- [x]`/`- [ ]` checkbox line per
/// sub_item.
fn render_verification_checklist(
    doc: &DocMetadata,
    v: &Verification,
    counts: &VerificationCounts,
    percentage: f64,
) -> String {
    use std::fmt::Write;

    let mut out = String::new();
    let _ = writeln!(out, "# Verification: {}", doc.title);
    let _ = writeln!(
        out,
        "Status: {} ({}/{}, {:.0}%)",
        v.status,
        counts.checked + counts.skipped,
        counts.total,
        percentage
    );
    out.push('\n');

    for item in &v.items {
        let icon = status_icon(&item.status);
        let stale_warning = if item_is_stale(doc, item) {
            " ⚠ stale"
        } else {
            ""
        };

        match item.fragment_seq {
            Some(seq) => {
                let _ = writeln!(out, "## §{seq} {} {icon}{stale_warning}", item.heading);
                if !item.impl_refs.is_empty() {
                    let refs: Vec<String> = item.impl_refs.iter().map(code_ref_display).collect();
                    let _ = writeln!(out, "- impl: {}", refs.join(", "));
                }
                if !item.test_refs.is_empty() {
                    let refs: Vec<String> = item.test_refs.iter().map(code_ref_display).collect();
                    let _ = writeln!(out, "- test: {}", refs.join(", "));
                }
            }
            None => {
                let label = item.label.as_deref().unwrap_or(&item.heading);
                let _ = writeln!(
                    out,
                    "## — {label} {icon}{stale_warning} [{}]",
                    item.category
                );
            }
        }

        for sub in &item.sub_items {
            let checkbox = if sub.status == "verified" { "x" } else { " " };
            match (&sub.reviewer, &sub.verified_at) {
                (Some(reviewer), Some(verified_at)) => {
                    let date = verified_at.split('T').next().unwrap_or(verified_at);
                    let _ = writeln!(
                        out,
                        "- [{checkbox}] {} (@{reviewer}, {date}) [{}]",
                        sub.description, sub.category
                    );
                }
                _ => {
                    let _ = writeln!(out, "- [{checkbox}] {} [{}]", sub.description, sub.category);
                }
            }
        }
        out.push('\n');
    }

    out
}

/// Renders a `CodeRef` for the checklist format: `path` optionally suffixed
/// with `:lines` and/or ` (label)`.
fn code_ref_display(r: &CodeRef) -> String {
    let mut s = r.path.clone();
    if let Some(lines) = &r.lines {
        s.push(':');
        s.push_str(lines);
    }
    if let Some(label) = &r.label {
        s.push_str(" (");
        s.push_str(label);
        s.push(')');
    }
    s
}

/// `handoff_doc_graph` — build a graph of every document in the project:
/// `nodes[]` (one per document, with optional verification progress),
/// `edges[]` (explicit parent_child/related links, plus implicit
/// shared_task/shared_scope links when `include_implicit=true`), and
/// `layers` (doc ids grouped by `doc_type`).
pub fn handle_doc_graph(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let include_implicit = arguments
        .get("include_implicit")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let include_verification = arguments
        .get("include_verification")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let docs = read_all_docs(handoff)?;

    let nodes: Vec<Value> = docs
        .iter()
        .map(|d| doc_graph_node_json(d, include_verification))
        .collect();

    let mut edges = doc_graph_explicit_edges(&docs);
    if include_implicit {
        edges.extend(doc_graph_implicit_edges(&docs));
    }

    let mut layers: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for d in &docs {
        layers
            .entry(d.doc_type.clone())
            .or_default()
            .push(d.id.clone());
    }

    Ok(to_json(&json!({
        "nodes": nodes,
        "edges": edges,
        "layers": layers,
    })))
}

/// Builds one `handoff_doc_graph` node: id/slug/title/doc_type/tags/task_ids
/// /section_count/updated_at, plus `verification_progress` when requested
/// (and the document has a verification matrix).
fn doc_graph_node_json(doc: &DocMetadata, include_verification: bool) -> Value {
    let mut node = json!({
        "id": doc.id,
        "slug": doc.slug,
        "title": doc.title,
        "doc_type": doc.doc_type,
        "tags": doc.tags,
        "task_ids": doc.task_ids,
        "section_count": doc.sections.len(),
        "updated_at": doc.updated_at,
    });
    if include_verification {
        if let Some(v) = &doc.verification {
            let total = v.items.len();
            let verified = v.items.iter().filter(|i| i.status == "verified").count();
            node["verification_progress"] = json!({ "total": total, "verified": verified });
        }
    }
    node
}

/// Explicit edges: `parent_id` (`type="parent_child"`, `direction="down"`,
/// from=parent to=child) and `related[]` (`type=<rel>`,
/// `direction="forward"`, from=this doc to=related target). Related entries
/// pointing at an id not present in `docs` are still emitted — the graph
/// consumer is expected to render dangling links, not silently drop them.
fn doc_graph_explicit_edges(docs: &[DocMetadata]) -> Vec<Value> {
    let mut edges = Vec::new();
    for d in docs {
        if let Some(parent_id) = &d.parent_id {
            edges.push(json!({
                "from": parent_id,
                "to": d.id,
                "type": "parent_child",
                "direction": "down",
            }));
        }
        for r in &d.related {
            edges.push(json!({
                "from": d.id,
                "to": r.id,
                "type": r.rel,
                "direction": "forward",
            }));
        }
    }
    edges
}

/// Implicit edges: `shared_task` (two documents sharing at least one
/// `task_ids` entry — `task_ids` on the edge lists every id shared, not just
/// the first) and `shared_scope` (two documents sharing at least one
/// `scope_paths` entry). Both are unordered/undirected pairs, emitted once
/// per pair (i<j) to avoid duplicating the same relationship in both
/// directions.
fn doc_graph_implicit_edges(docs: &[DocMetadata]) -> Vec<Value> {
    let mut edges = Vec::new();
    for i in 0..docs.len() {
        for j in (i + 1)..docs.len() {
            let a = &docs[i];
            let b = &docs[j];

            let shared_tasks: Vec<String> = a
                .task_ids
                .iter()
                .filter(|t| b.task_ids.contains(t))
                .cloned()
                .collect();
            if !shared_tasks.is_empty() {
                edges.push(json!({
                    "from": a.id,
                    "to": b.id,
                    "type": "shared_task",
                    "task_ids": shared_tasks,
                }));
            }

            let shares_scope = a.scope_paths.iter().any(|p| b.scope_paths.contains(p));
            if shares_scope {
                edges.push(json!({
                    "from": a.id,
                    "to": b.id,
                    "type": "shared_scope",
                }));
            }
        }
    }
    edges
}

/// One entry in a `handoff_doc_trace` `chain[]`/`branches[].docs[]`:
/// `{id, title, doc_type, rel}`. `rel` describes how this doc relates to the
/// previous entry in the chain ("parent", "child", or the `related[].rel`
/// value for a related-doc detour); `None` for the trace's starting doc.
fn doc_trace_item_json(doc: &DocMetadata, rel: Option<&str>) -> Value {
    json!({
        "id": doc.id,
        "title": doc.title,
        "doc_type": doc.doc_type,
        "rel": rel,
    })
}

/// Walks the child->parent chain starting at `doc` (exclusive — `doc` itself
/// is not included), ordered from the immediate parent up to the root.
/// `visited` prevents infinite loops on a cyclic `parent_id` graph; a doc
/// already visited (including `doc` itself) stops the walk rather than
/// erroring.
fn doc_trace_walk_up(
    handoff: &Path,
    doc: &DocMetadata,
    visited: &mut std::collections::HashSet<String>,
) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    let mut current = doc.clone();
    while let Some(parent_id) = current.parent_id.clone() {
        if visited.contains(&parent_id) {
            break;
        }
        let Some(parent) = find_doc_by_id(handoff, &parent_id)? else {
            break;
        };
        visited.insert(parent.id.clone());
        out.push(doc_trace_item_json(&parent, Some("parent")));
        current = parent;
    }
    out.reverse();
    Ok(out)
}

/// Recursively walks parent->children (DFS) starting at `doc` (exclusive).
/// Returns the primary descendant chain (first child at each level) plus any
/// `branches` recorded for multi-child forks. `visited` prevents infinite
/// loops on a cyclic `children` graph.
fn doc_trace_walk_down(
    handoff: &Path,
    doc: &DocMetadata,
    visited: &mut std::collections::HashSet<String>,
    branches: &mut Vec<Value>,
) -> Result<Vec<Value>> {
    let mut children = Vec::new();
    for child_id in &doc.children {
        if visited.contains(child_id) {
            continue;
        }
        if let Some(child) = find_doc_by_id(handoff, child_id)? {
            children.push(child);
        }
    }

    if children.is_empty() {
        return Ok(Vec::new());
    }

    // Fork detection: more than one live (non-visited, resolvable) child at
    // this level. Every child's own sub-chain is recorded under `branches`;
    // the first child's sub-chain also becomes the primary continuation of
    // the returned chain, so a single-child level still reads as a plain
    // linear chain.
    let is_fork = children.len() > 1;
    let mut primary_chain = Vec::new();

    for (idx, child) in children.iter().enumerate() {
        if visited.contains(&child.id) {
            continue;
        }
        visited.insert(child.id.clone());
        let mut sub_chain = vec![doc_trace_item_json(child, Some("child"))];
        sub_chain.extend(doc_trace_walk_down(handoff, child, visited, branches)?);

        if is_fork {
            branches.push(json!({
                "fork_from": doc.id,
                "docs": sub_chain,
            }));
        }
        if idx == 0 {
            primary_chain = sub_chain;
        }
    }

    Ok(primary_chain)
}

/// Appends `related` (implements/references/etc.) detours for every document
/// already present in `chain` (by id), skipping any related id already
/// visited. Related docs are appended once, immediately, as a flat list — a
/// "detour" from the main chain rather than a further recursive expansion.
fn doc_trace_related_detours(
    handoff: &Path,
    chain_doc_ids: &[String],
    visited: &mut std::collections::HashSet<String>,
) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for doc_id in chain_doc_ids {
        let Some(doc) = find_doc_by_id(handoff, doc_id)? else {
            continue;
        };
        for r in &doc.related {
            if visited.contains(&r.id) {
                continue;
            }
            let Some(target) = find_doc_by_id(handoff, &r.id)? else {
                continue;
            };
            visited.insert(target.id.clone());
            out.push(doc_trace_item_json(&target, Some(&r.rel)));
        }
    }
    Ok(out)
}

/// `handoff_doc_trace` — trace a document's family-tree lineage: `up` (walk
/// child->parent), `down` (walk parent->children, DFS), or `both` (merge the
/// up chain + the target + the down chain). `related` docs encountered along
/// the primary chain are appended as detour entries. Multi-child forks in the
/// `down` direction are additionally reported in `branches[]`.
pub fn handle_doc_trace(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let doc_id = arguments
        .get("doc_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'doc_id' is required"))?;
    let direction = arguments
        .get("direction")
        .and_then(|v| v.as_str())
        .unwrap_or("both");

    let doc = resolve_doc(handoff, doc_id)?
        .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_id}"))?;

    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    visited.insert(doc.id.clone());

    let mut branches: Vec<Value> = Vec::new();
    let mut chain: Vec<Value> = Vec::new();

    if direction == "up" || direction == "both" {
        chain.extend(doc_trace_walk_up(handoff, &doc, &mut visited)?);
    }
    chain.push(doc_trace_item_json(&doc, None));
    if direction == "down" || direction == "both" {
        chain.extend(doc_trace_walk_down(
            handoff,
            &doc,
            &mut visited,
            &mut branches,
        )?);
    }

    let chain_doc_ids: Vec<String> = chain
        .iter()
        .filter_map(|v| v["id"].as_str().map(str::to_string))
        .collect();
    chain.extend(doc_trace_related_detours(
        handoff,
        &chain_doc_ids,
        &mut visited,
    )?);

    Ok(to_json(&json!({
        "chain": chain,
        "branches": branches,
    })))
}

fn doc_metadata_json(doc: &DocMetadata) -> Value {
    json!({
        "id": doc.id,
        "slug": doc.slug,
        "title": doc.title,
        "doc_type": doc.doc_type,
        "tags": doc.tags,
        "scope_paths": doc.scope_paths,
        "parent_id": doc.parent_id,
        "children": doc.children,
        "related": doc.related,
        "auto_inject": doc.auto_inject,
        "task_ids": doc.task_ids,
        "layer": doc.layer,
        "has_bom": doc.has_bom,
        "line_ending": doc.line_ending,
        "sections": doc.sections,
        "section_count": doc.sections.len(),
        "created_at": doc.created_at,
        "updated_at": doc.updated_at,
        "content_hash": doc.content_hash,
    })
}

/// Read a `&[String]` from a JSON string-array value (missing/non-array →
/// empty).
fn string_array_value(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn to_json(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

/// Review round 2 MAJOR fix: `validate_section_splice_range` must reject an
/// inconsistent `(body, byte_offset, byte_length)` combination with a clean
/// `Err` rather than let `handle_doc_update_section` panic on
/// `&body[..start]` / `&body[end..]` (non-char-boundary slice, likely with
/// JA text) or silently splice at the wrong offsets.
#[cfg(test)]
mod validate_section_splice_range_tests {
    use super::*;

    #[test]
    fn valid_range_on_ascii_body_succeeds() {
        let preamble = "Intro.\n\n";
        let section_body = "## Heading\nBody.\n";
        let body = format!("{preamble}{section_body}");
        let (start, end) =
            validate_section_splice_range(&body, preamble.len(), section_body.len(), "doc-1", 1)
                .unwrap();
        assert_eq!(&body[start..end], section_body);
    }

    #[test]
    fn valid_range_on_ja_body_succeeds() {
        // "前書き。\n\n" is 4 chars + 2 newlines = 4*3 + 2 = 14 bytes (each JA
        // char is 3 bytes in UTF-8); the `## 見出し` section starts right
        // after it.
        let preamble = "前書き。\n\n";
        let section_body = "## 見出し\n本文。\n";
        let body = format!("{preamble}{section_body}");
        let (start, end) =
            validate_section_splice_range(&body, preamble.len(), section_body.len(), "doc-1", 1)
                .unwrap();
        assert_eq!(&body[start..end], section_body);
    }

    #[test]
    fn end_beyond_body_len_is_a_clean_error_not_a_panic() {
        let body = "Short body.\n";
        let result = validate_section_splice_range(body, 0, body.len() + 100, "doc-1", 1);
        assert!(result.is_err(), "expected Err, got {result:?}");
    }

    #[test]
    fn non_char_boundary_start_is_a_clean_error_not_a_panic() {
        // "日" is a 3-byte UTF-8 char at offset 0 — offset 1 lands mid-char.
        let body = "日本語のテスト\n";
        let result = validate_section_splice_range(body, 1, 3, "doc-1", 1);
        assert!(
            result.is_err(),
            "non-char-boundary start must error, not panic: {result:?}"
        );
    }

    #[test]
    fn non_char_boundary_end_is_a_clean_error_not_a_panic() {
        let body = "日本語のテスト\n";
        // start=0 is a valid boundary, but byte_length=2 lands the end mid-char.
        let result = validate_section_splice_range(body, 0, 2, "doc-1", 1);
        assert!(
            result.is_err(),
            "non-char-boundary end must error, not panic: {result:?}"
        );
    }

    #[test]
    fn zero_length_range_at_end_of_body_is_valid() {
        // byte_offset == body.len(), byte_length == 0: an empty trailing
        // range. `start > end` can never actually occur from non-negative
        // (byte_offset, byte_length) since end = byte_offset + byte_length
        // is always >= start — the `end > body.len()` and char-boundary
        // checks above are what catch real metadata/body desync.
        let body = "Some body text.\n";
        let result = validate_section_splice_range(body, body.len(), 0, "doc-1", 1);
        assert!(result.is_ok(), "start == end == body.len() must be valid");
    }
}

#[cfg(test)]
mod graph_tests {
    use super::*;

    fn doc(id: &str, slug: &str, doc_type: &str) -> DocMetadata {
        DocMetadata::new(
            id.to_string(),
            slug.to_string(),
            format!("Title {id}"),
            doc_type.to_string(),
            "2026-07-12T00:00:00Z".to_string(),
        )
    }

    #[test]
    fn explicit_edges_include_parent_child_and_related() {
        let mut parent = doc("doc-1", "parent", "spec");
        let mut child = doc("doc-2", "child", "design");
        child.parent_id = Some("doc-1".to_string());
        parent.children = vec!["doc-2".to_string()];
        child.related.push(DocRelation {
            id: "doc-3".to_string(),
            rel: "implements".to_string(),
        });
        let other = doc("doc-3", "other", "note");

        let docs = vec![parent, child, other];
        let edges = doc_graph_explicit_edges(&docs);

        assert!(edges.iter().any(|e| e["type"] == "parent_child"
            && e["from"] == "doc-1"
            && e["to"] == "doc-2"
            && e["direction"] == "down"));
        assert!(edges.iter().any(|e| e["type"] == "implements"
            && e["from"] == "doc-2"
            && e["to"] == "doc-3"
            && e["direction"] == "forward"));
    }

    #[test]
    fn implicit_edges_detect_shared_task_ids() {
        let mut a = doc("doc-1", "a", "spec");
        let mut b = doc("doc-2", "b", "spec");
        a.task_ids = vec!["t-1".to_string(), "t-2".to_string()];
        b.task_ids = vec!["t-2".to_string(), "t-3".to_string()];
        let docs = vec![a, b];

        let edges = doc_graph_implicit_edges(&docs);
        let shared_task_edge = edges
            .iter()
            .find(|e| e["type"] == "shared_task")
            .expect("shared_task edge must be generated");
        assert_eq!(shared_task_edge["from"], "doc-1");
        assert_eq!(shared_task_edge["to"], "doc-2");
        assert_eq!(shared_task_edge["task_ids"], json!(["t-2"]));
    }

    #[test]
    fn implicit_edges_detect_shared_scope_paths() {
        let mut a = doc("doc-1", "a", "spec");
        let mut b = doc("doc-2", "b", "spec");
        a.scope_paths = vec!["src/mcp/".to_string()];
        b.scope_paths = vec!["src/mcp/".to_string(), "src/storage/".to_string()];
        let docs = vec![a, b];

        let edges = doc_graph_implicit_edges(&docs);
        assert!(edges
            .iter()
            .any(|e| e["type"] == "shared_scope" && e["from"] == "doc-1" && e["to"] == "doc-2"));
    }

    #[test]
    fn implicit_edges_absent_when_nothing_shared() {
        let a = doc("doc-1", "a", "spec");
        let b = doc("doc-2", "b", "spec");
        let docs = vec![a, b];

        let edges = doc_graph_implicit_edges(&docs);
        assert!(edges.is_empty());
    }

    #[test]
    fn graph_node_json_includes_verification_progress_when_requested() {
        let mut d = doc("doc-1", "a", "spec");
        d.verification = Some(Verification {
            status: "in_review".to_string(),
            created_at: "2026-07-12T00:00:00Z".to_string(),
            updated_at: "2026-07-12T00:00:00Z".to_string(),
            items: vec![
                VerificationItem {
                    fragment_seq: Some(0),
                    heading: String::new(),
                    status: "verified".to_string(),
                    impl_refs: Vec::new(),
                    test_refs: Vec::new(),
                    reviewer: None,
                    verified_at: None,
                    notes: String::new(),
                    content_hash_at_verify: None,
                    category: "section".to_string(),
                    sub_items: Vec::new(),
                    label: None,
                },
                VerificationItem {
                    fragment_seq: Some(1),
                    heading: "H".to_string(),
                    status: "pending".to_string(),
                    impl_refs: Vec::new(),
                    test_refs: Vec::new(),
                    reviewer: None,
                    verified_at: None,
                    notes: String::new(),
                    content_hash_at_verify: None,
                    category: "section".to_string(),
                    sub_items: Vec::new(),
                    label: None,
                },
            ],
        });

        let with_verification = doc_graph_node_json(&d, true);
        assert_eq!(
            with_verification["verification_progress"],
            json!({ "total": 2, "verified": 1 })
        );

        let without_verification = doc_graph_node_json(&d, false);
        assert!(without_verification.get("verification_progress").is_none());
    }

    #[test]
    fn graph_node_json_omits_verification_progress_when_no_matrix() {
        let d = doc("doc-1", "a", "spec");
        let node = doc_graph_node_json(&d, true);
        assert!(node.get("verification_progress").is_none());
    }
}

#[cfg(test)]
mod sub_item_lookup_tests {
    use super::*;

    fn item_with_subs() -> VerificationItem {
        VerificationItem {
            fragment_seq: Some(1),
            heading: "1. 課題".to_string(),
            status: "pending".to_string(),
            impl_refs: Vec::new(),
            test_refs: Vec::new(),
            reviewer: None,
            verified_at: None,
            notes: String::new(),
            content_hash_at_verify: None,
            category: "section".to_string(),
            sub_items: vec![
                SubItem {
                    index: 0,
                    description: "req A".to_string(),
                    stable_id: Some("C01-1.1".to_string()),
                    ..Default::default()
                },
                SubItem {
                    index: 1,
                    description: "req B".to_string(),
                    stable_id: Some("C01-1.2".to_string()),
                    ..Default::default()
                },
            ],
            label: None,
        }
    }

    #[test]
    fn finds_by_stable_id_when_given() {
        let mut item = item_with_subs();
        let (sub, warning) =
            find_sub_item_mut_by_id(&mut item, Some("C01-1.2"), None, Some(1), "doc-1").unwrap();
        assert_eq!(sub.description, "req B");
        assert!(warning.is_none());
    }

    #[test]
    fn falls_back_to_index_when_no_id_given() {
        let mut item = item_with_subs();
        let (sub, warning) =
            find_sub_item_mut_by_id(&mut item, None, Some(0), Some(1), "doc-1").unwrap();
        assert_eq!(sub.description, "req A");
        assert!(warning.is_none());
    }

    #[test]
    fn prefers_stable_id_and_warns_on_index_mismatch() {
        let mut item = item_with_subs();
        // sub_item_id points at "req B" (index 1) but sub_item_index says 0
        // ("req A") — sub_item_id must win, and a warning must be returned.
        let (sub, warning) =
            find_sub_item_mut_by_id(&mut item, Some("C01-1.2"), Some(0), Some(1), "doc-1").unwrap();
        assert_eq!(sub.description, "req B");
        assert!(warning.is_some(), "expected a mismatch warning");
    }

    #[test]
    fn no_warning_when_id_and_index_agree() {
        let mut item = item_with_subs();
        let (sub, warning) =
            find_sub_item_mut_by_id(&mut item, Some("C01-1.1"), Some(0), Some(1), "doc-1").unwrap();
        assert_eq!(sub.description, "req A");
        assert!(warning.is_none());
    }

    #[test]
    fn errors_when_stable_id_not_found() {
        let mut item = item_with_subs();
        let result = find_sub_item_mut_by_id(&mut item, Some("C01-9.9"), None, Some(1), "doc-1");
        assert!(result.is_err());
    }

    #[test]
    fn errors_when_neither_id_nor_index_given() {
        let mut item = item_with_subs();
        let result = find_sub_item_mut_by_id(&mut item, None, None, Some(1), "doc-1");
        assert!(result.is_err());
    }
}

/// Requirements-traceability P0 (`.handoff/docs/_doc.req-traceability-mcp-plan.md`
/// §2.3, t300.3): `derive_stable_id` unit tests — category-prefix extraction,
/// heading-number extraction, description slug fallback, and collision
/// suffixing.
#[cfg(test)]
mod stable_id_derivation_tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn extracts_category_prefix_from_req_c_slug() {
        let existing = HashSet::new();
        let (id, warning) = derive_stable_id(
            "req-c01-board-setup",
            "## 2.1 基板外形",
            "外形形状定義",
            &existing,
        );
        assert!(id.starts_with("C01-"), "expected C01- prefix, got {id:?}");
        assert!(warning.is_none());
    }

    #[test]
    fn falls_back_to_uppercased_slug_when_no_category_pattern() {
        let existing = HashSet::new();
        let (id, _) = derive_stable_id("misc-notes", "## 2.1 基板外形", "外形形状定義", &existing);
        assert!(
            id.starts_with("MISC-NOTES-"),
            "expected uppercased slug prefix, got {id:?}"
        );
    }

    #[test]
    fn extracts_heading_number_from_heading_text() {
        let existing = HashSet::new();
        let (id, _) = derive_stable_id(
            "req-c01-board-setup",
            "## 2.1 基板外形",
            "何らかの説明",
            &existing,
        );
        assert!(
            id.contains("2.1"),
            "expected heading number 2.1 in id, got {id:?}"
        );
    }

    #[test]
    fn extracts_description_number_when_present() {
        let existing = HashSet::new();
        let (id, _) = derive_stable_id(
            "req-c01-board-setup",
            "## 2.1 基板外形",
            "2.1.1 外形形状定義",
            &existing,
        );
        assert_eq!(id, "C01-2.1.1");
    }

    #[test]
    fn slugifies_description_when_no_number_extractable() {
        // Pure-Japanese description has no ASCII-alphanumeric characters to
        // slugify, so it falls back further to the heading number alone —
        // still deterministic and collision-checked.
        let existing = HashSet::new();
        let (id, _) = derive_stable_id(
            "req-c01-board-setup",
            "## 2.1 基板外形",
            "矩形外形",
            &existing,
        );
        assert_eq!(id, "C01-2.1");
    }

    #[test]
    fn slugifies_ascii_description_when_no_number_extractable() {
        let existing = HashSet::new();
        let (id, _) = derive_stable_id(
            "req-c01-board-setup",
            "## 2.1 基板外形",
            "Rectangular Outline!",
            &existing,
        );
        assert_eq!(id, "C01-2.1-rectangular-outline");
        assert!(!id.contains(' '));
        assert!(!id.contains("--"));
    }

    #[test]
    fn appends_suffix_and_warns_on_collision() {
        let mut existing = HashSet::new();
        existing.insert("C01-2.1.1".to_string());
        let (id, warning) = derive_stable_id(
            "req-c01-board-setup",
            "## 2.1 基板外形",
            "2.1.1 外形形状定義",
            &existing,
        );
        assert_eq!(id, "C01-2.1.1-2");
        assert!(warning.is_some(), "expected a collision warning");
    }

    #[test]
    fn appends_incrementing_suffix_on_repeated_collision() {
        let mut existing = HashSet::new();
        existing.insert("C01-2.1.1".to_string());
        existing.insert("C01-2.1.1-2".to_string());
        let (id, warning) = derive_stable_id(
            "req-c01-board-setup",
            "## 2.1 基板外形",
            "2.1.1 外形形状定義",
            &existing,
        );
        assert_eq!(id, "C01-2.1.1-3");
        assert!(warning.is_some());
    }

    #[test]
    fn no_warning_when_no_collision() {
        let existing = HashSet::new();
        let (_, warning) = derive_stable_id(
            "req-c01-board-setup",
            "## 2.1 基板外形",
            "2.1.1 外形形状定義",
            &existing,
        );
        assert!(warning.is_none());
    }
}

/// Requirements-traceability P0 §2.3, t300.3: fuzzy description matching
/// used by `sync`/`add_item` to re-link a new SubItem description to an
/// existing SubItem's `stable_id` instead of minting a fresh one.
#[cfg(test)]
mod fuzzy_match_tests {
    use super::*;

    #[test]
    fn exact_description_matches() {
        assert!(descriptions_fuzzy_match("外形形状定義", "外形形状定義"));
    }

    #[test]
    fn near_identical_descriptions_match_after_normalization() {
        // Trailing punctuation / whitespace differences should not defeat
        // the match — normalization strips them before comparing.
        assert!(descriptions_fuzzy_match(
            "形状=八面体であること",
            "形状=八面体であること。"
        ));
    }

    #[test]
    fn substring_containment_matches() {
        assert!(descriptions_fuzzy_match(
            "外形形状定義",
            "2.1.1 外形形状定義（矩形）"
        ));
    }

    #[test]
    fn unrelated_descriptions_do_not_match() {
        assert!(!descriptions_fuzzy_match(
            "外形形状定義",
            "電源電圧の許容範囲"
        ));
    }
}

#[cfg(test)]
mod requirements_summary_tests {
    use super::*;

    fn doc_with_items(id: &str, slug: &str, items: Vec<VerificationItem>) -> DocMetadata {
        let mut d = DocMetadata::new(
            id.to_string(),
            slug.to_string(),
            format!("Title {id}"),
            "spec".to_string(),
            "2026-09-20T00:00:00Z".to_string(),
        );
        d.verification = Some(Verification {
            status: "in_review".to_string(),
            created_at: "2026-09-20T00:00:00Z".to_string(),
            updated_at: "2026-09-20T00:00:00Z".to_string(),
            items,
        });
        d
    }

    fn section_item(sub_items: Vec<SubItem>) -> VerificationItem {
        VerificationItem {
            fragment_seq: Some(1),
            heading: "1. 要件".to_string(),
            status: "pending".to_string(),
            impl_refs: Vec::new(),
            test_refs: Vec::new(),
            reviewer: None,
            verified_at: None,
            notes: String::new(),
            content_hash_at_verify: None,
            category: "section".to_string(),
            sub_items,
            label: None,
        }
    }

    #[test]
    fn empty_docs_yield_zero_total() {
        let summary = aggregate_requirements(&[]);
        assert_eq!(summary.total, 0);
        assert!(summary.by_status.is_empty());
        assert!(summary.by_priority.is_empty());
        assert!(summary.by_category.is_empty());
        assert_eq!(summary.coverage.impl_pct, 0.0);
    }

    #[test]
    fn doc_with_no_verification_matrix_contributes_nothing() {
        let d = DocMetadata::new(
            "doc-1".to_string(),
            "no-verify".to_string(),
            "Title".to_string(),
            "spec".to_string(),
            "2026-09-20T00:00:00Z".to_string(),
        );
        let summary = aggregate_requirements(&[d]);
        assert_eq!(summary.total, 0);
    }

    #[test]
    fn single_doc_aggregates_status_priority_category_and_coverage() {
        let subs = vec![
            SubItem {
                index: 0,
                description: "req A".to_string(),
                stable_id: Some("C01-1.1".to_string()),
                priority: Some("P0".to_string()),
                dev_stage: Some("implemented".to_string()),
                impl_refs: vec![CodeRef {
                    path: "src/a.rs".to_string(),
                    lines: None,
                    label: None,
                }],
                ..Default::default()
            },
            SubItem {
                index: 1,
                description: "req B".to_string(),
                stable_id: Some("C01-1.2".to_string()),
                priority: None,
                dev_stage: None,
                ..Default::default()
            },
            SubItem {
                index: 2,
                description: "req C".to_string(),
                stable_id: Some("C07-2.1".to_string()),
                priority: Some("P0".to_string()),
                dev_stage: Some("verified".to_string()),
                impl_refs: vec![CodeRef {
                    path: "src/c.rs".to_string(),
                    lines: None,
                    label: None,
                }],
                test_refs: vec![CodeRef {
                    path: "tests/c.rs".to_string(),
                    lines: None,
                    label: None,
                }],
                ..Default::default()
            },
        ];
        let doc = doc_with_items("doc-1", "req-c01", vec![section_item(subs)]);

        let summary = aggregate_requirements(&[doc]);

        assert_eq!(summary.total, 3);

        // by_status: dev_stage=None -> "not_started" fallback.
        assert_eq!(summary.by_status.get("implemented"), Some(&1));
        assert_eq!(summary.by_status.get("not_started"), Some(&1));
        assert_eq!(summary.by_status.get("verified"), Some(&1));

        // by_priority: priority=None -> "unset" fallback.
        let p0 = summary.by_priority.get("P0").expect("P0 bucket");
        assert_eq!(p0.total, 2);
        assert_eq!(p0.implemented, 2);
        assert_eq!(p0.tested, 1);
        assert_eq!(p0.verified, 1);
        let unset = summary.by_priority.get("unset").expect("unset bucket");
        assert_eq!(unset.total, 1);

        // by_category: extracted from stable_id prefix.
        let c01 = summary.by_category.get("C01").expect("C01 bucket");
        assert_eq!(c01.total, 2);
        assert_eq!(c01.implemented, 1);
        assert_eq!(c01.coverage_pct, 50.0);
        let c07 = summary.by_category.get("C07").expect("C07 bucket");
        assert_eq!(c07.total, 1);
        assert_eq!(c07.implemented, 1);
        assert_eq!(c07.coverage_pct, 100.0);

        // coverage: 2/3 impl, 1/3 test, 1/3 verified.
        assert!((summary.coverage.impl_pct - (2.0 / 3.0 * 100.0)).abs() < 1e-9);
        assert!((summary.coverage.test_pct - (1.0 / 3.0 * 100.0)).abs() < 1e-9);
        assert!((summary.coverage.verified_pct - (1.0 / 3.0 * 100.0)).abs() < 1e-9);
    }

    #[test]
    fn multiple_docs_aggregate_across_documents() {
        let doc_a = doc_with_items(
            "doc-a",
            "req-c01",
            vec![section_item(vec![SubItem {
                index: 0,
                description: "req A".to_string(),
                stable_id: Some("C01-1.1".to_string()),
                priority: Some("P1".to_string()),
                dev_stage: Some("tested".to_string()),
                ..Default::default()
            }])],
        );
        let doc_b = doc_with_items(
            "doc-b",
            "req-c07",
            vec![section_item(vec![SubItem {
                index: 0,
                description: "req B".to_string(),
                stable_id: Some("C07-3.1".to_string()),
                priority: Some("P1".to_string()),
                dev_stage: Some("tested".to_string()),
                ..Default::default()
            }])],
        );

        let summary = aggregate_requirements(&[doc_a, doc_b]);

        assert_eq!(summary.total, 2);
        assert_eq!(summary.by_status.get("tested"), Some(&2));
        let p1 = summary.by_priority.get("P1").expect("P1 bucket");
        assert_eq!(p1.total, 2);
        assert_eq!(summary.by_category.len(), 2);
        assert_eq!(summary.by_category.get("C01").unwrap().total, 1);
        assert_eq!(summary.by_category.get("C07").unwrap().total, 1);
    }

    #[test]
    fn write_requirements_summary_skips_file_when_no_requirements() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        write_requirements_summary(&handoff, &[]).unwrap();

        let path = docs_dir(&handoff).join("_requirements_summary.json");
        assert!(
            !path.exists(),
            "no requirements => no file should be written"
        );
    }

    #[test]
    fn write_requirements_summary_writes_file_when_requirements_exist() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let doc = doc_with_items(
            "doc-1",
            "req-c01",
            vec![section_item(vec![SubItem {
                index: 0,
                description: "req A".to_string(),
                stable_id: Some("C01-1.1".to_string()),
                ..Default::default()
            }])],
        );

        write_requirements_summary(&handoff, &[doc]).unwrap();

        let path = docs_dir(&handoff).join("_requirements_summary.json");
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["total"], 1);
        assert_eq!(parsed["by_status"]["not_started"], 1);
    }

    /// FR-905 (wiki/220 §4.3): "MCP は SubItem が 0 件になったとき summary
    /// ファイルを削除する（古い summary の残留防止）" — a prior call that
    /// wrote the cache while requirements existed must not leave a stale
    /// file behind once the last SubItem is gone (e.g. the owning document
    /// was deleted, or its matrix was synced down to nothing). Without this,
    /// a FileWatcher-based reader (the VSCode extension) would keep showing
    /// long-gone requirements.
    #[test]
    fn write_requirements_summary_deletes_stale_file_when_requirements_drop_to_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let doc = doc_with_items(
            "doc-1",
            "req-c01",
            vec![section_item(vec![SubItem {
                index: 0,
                description: "req A".to_string(),
                stable_id: Some("C01-1.1".to_string()),
                ..Default::default()
            }])],
        );
        write_requirements_summary(&handoff, &[doc]).unwrap();
        let path = docs_dir(&handoff).join("_requirements_summary.json");
        assert!(path.exists(), "precondition: summary file must exist");

        // The last SubItem is gone (doc deleted / matrix emptied) — the next
        // refresh call sees zero requirements and must remove the stale file.
        write_requirements_summary(&handoff, &[]).unwrap();

        assert!(
            !path.exists(),
            "stale _requirements_summary.json must be deleted once total requirements reaches 0"
        );
    }

    /// P-M4 (wiki/240-performance-design.md §4, wiki/220 §4.3 r3):
    /// `_requirements_summary.json` must be unformatted JSON (compact, no
    /// indentation) and must carry an `inputs` fingerprint alongside the
    /// pre-existing top-level fields — `total`/`by_status`/... must read
    /// back exactly as before (existing-field stability, checked by
    /// deserializing the written file rather than diffing raw text against a
    /// golden pretty-printed fixture).
    #[test]
    fn write_requirements_summary_is_compact_json_with_inputs_fingerprint() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let doc = doc_with_items(
            "doc-1",
            "req-c01",
            vec![section_item(vec![SubItem {
                index: 0,
                description: "req A".to_string(),
                stable_id: Some("C01-1.1".to_string()),
                ..Default::default()
            }])],
        );
        write_doc(&handoff, &doc).unwrap();

        write_requirements_summary(&handoff, &[doc]).unwrap();

        let path = docs_dir(&handoff).join("_requirements_summary.json");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            !content.contains('\n'),
            "expected unformatted (single-line) JSON, got: {content}"
        );
        let parsed: Value = serde_json::from_str(&content).unwrap();
        // Pre-existing fields must round-trip unchanged.
        assert_eq!(parsed["total"], 1);
        assert_eq!(parsed["by_status"]["not_started"], 1);
        assert_eq!(parsed["items"][0]["stable_id"], "C01-1.1");
        // New fingerprint field, per wiki/220 §4.3 r3.
        let inputs = &parsed["inputs"];
        assert!(inputs["docs_count"].as_u64().unwrap() >= 1);
        assert!(inputs["docs_max_mtime_ns"].as_u64().unwrap() > 0);
        assert_eq!(inputs["tasks_count"], 0);
        assert_eq!(inputs["tasks_max_mtime_ns"], 0);
        assert_eq!(inputs["runs_count"], 0);
        assert!(inputs["runs_max_id"].is_null());
    }

    /// P-M4: a derived-file write is skipped entirely when neither the
    /// aggregate nor the input fingerprint changed — proven via inode
    /// identity (an `atomic_write` create-then-rename always mints a new
    /// inode, so "same inode" can only mean "no write syscall happened").
    #[test]
    fn write_requirements_summary_skips_write_when_nothing_changed() {
        use std::os::unix::fs::MetadataExt;

        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let doc = doc_with_items(
            "doc-1",
            "req-c01",
            vec![section_item(vec![SubItem {
                index: 0,
                description: "req A".to_string(),
                stable_id: Some("C01-1.1".to_string()),
                ..Default::default()
            }])],
        );
        write_doc(&handoff, &doc).unwrap();
        write_requirements_summary(&handoff, std::slice::from_ref(&doc)).unwrap();

        let path = docs_dir(&handoff).join("_requirements_summary.json");
        let ino_before = std::fs::metadata(&path).unwrap().ino();

        // Nothing on disk changed since the previous call (same doc content,
        // no new/touched files) => the fingerprint is identical => no write.
        write_requirements_summary(&handoff, std::slice::from_ref(&doc)).unwrap();
        let ino_after_noop = std::fs::metadata(&path).unwrap().ino();
        assert_eq!(
            ino_before, ino_after_noop,
            "identical inputs must not rewrite the file"
        );

        // A new task file changes `tasks_count`, so the fingerprint really
        // did change this time => the file must be rewritten.
        std::fs::create_dir_all(handoff.join("tasks").join("t1")).unwrap();
        std::fs::write(
            handoff.join("tasks").join("t1").join("_task.todo.json"),
            "{}",
        )
        .unwrap();
        write_requirements_summary(&handoff, &[doc]).unwrap();
        let ino_after_change = std::fs::metadata(&path).unwrap().ino();
        assert_ne!(
            ino_after_noop, ino_after_change,
            "a real fingerprint change must rewrite the file"
        );
    }

    /// t370.11 (wiki/240-performance-design.md §4 P-M4 follow-up): repeat
    /// same-process calls must not keep re-reading the existing
    /// `_requirements_summary.json` body to decide whether to write —
    /// that full-file `fs::read` regressed `update_task_status_with_links`
    /// at L scale (rchar 115KB -> 829KB, p50 17ms -> 50ms). Proven via the
    /// test-only fallback-read counter rather than only the externally-
    /// visible skip-write behavior (which would still pass even if the read
    /// happened on every call).
    #[test]
    fn write_requirements_summary_does_not_reread_file_on_repeat_calls_in_same_process() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();
        let path = docs_dir(&handoff).join("_requirements_summary.json");

        let doc_a = doc_with_items(
            "doc-1",
            "req-c01",
            vec![section_item(vec![SubItem {
                index: 0,
                description: "req A".to_string(),
                stable_id: Some("C01-1.1".to_string()),
                dev_stage: Some("not_started".to_string()),
                ..Default::default()
            }])],
        );
        write_doc(&handoff, &doc_a).unwrap();

        // First call for this path in this process: cache is cold, so a
        // fallback read attempt is expected (the file doesn't exist yet,
        // but the code path is still taken).
        write_requirements_summary(&handoff, std::slice::from_ref(&doc_a)).unwrap();
        assert_eq!(summary_read_fallback_count(&path), 1);

        // A genuinely different aggregate (dev_stage flip) written by this
        // same process must still be detected and written — without a
        // second fallback read, because the in-process cache already holds
        // the previous call's value and the file's stat still matches it.
        let mut doc_b = doc_a.clone();
        doc_b.verification.as_mut().unwrap().items[0].sub_items[0].dev_stage =
            Some("in_progress".to_string());
        write_requirements_summary(&handoff, &[doc_b.clone()]).unwrap();
        assert_eq!(
            summary_read_fallback_count(&path),
            1,
            "a real content change observed via the in-process cache must not trigger a re-read"
        );
        let content_after_change = std::fs::read_to_string(&path).unwrap();
        assert!(content_after_change.contains("in_progress"));

        // Calling again with identical content must still skip both the
        // read and the write, purely via the cached value.
        write_requirements_summary(&handoff, &[doc_b]).unwrap();
        assert_eq!(summary_read_fallback_count(&path), 1);
    }

    /// t370.11 round 2 (rework, reviewer MAJOR: round 1's cache still held
    /// and deep-compared a `serde_json::Value` on every call, which meant
    /// building a full `Value` tree from the ~0.7-1MB aggregate on every
    /// single `write_requirements_summary` call even when the file's stat
    /// matched the cache). The hot, same-process, stat-matches path — every
    /// repeat call, whether the content is unchanged or genuinely changed —
    /// must compare the cached native struct directly and never call
    /// `serde_json::to_value` at all. Only a stat mismatch (an externally
    /// modified file — nothing on disk this process wrote/observed) may
    /// fall back to a `Value`-based comparison, and only once per
    /// modification. Proven via the dedicated counter rather than only the
    /// externally-visible skip-write behavior (which would still pass even
    /// if a `Value` was built and thrown away every time).
    #[test]
    fn write_requirements_summary_does_not_serialize_to_value_on_repeat_calls_in_same_process() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();
        let path = docs_dir(&handoff).join("_requirements_summary.json");

        let doc = doc_with_items(
            "doc-1",
            "req-c01",
            vec![section_item(vec![SubItem {
                index: 0,
                description: "req A".to_string(),
                stable_id: Some("C01-1.1".to_string()),
                dev_stage: Some("not_started".to_string()),
                ..Default::default()
            }])],
        );
        write_doc(&handoff, &doc).unwrap();

        // First call for this path in this process: the file does not exist
        // yet, so there is nothing on disk to compare against — zero
        // `Value` construction even on this cold-cache call.
        write_requirements_summary(&handoff, std::slice::from_ref(&doc)).unwrap();
        assert_eq!(summary_compare_serialize_count(&path), 0);

        // Repeat calls with unchanged content must hit the fast, native-
        // struct-comparison path — still zero `Value` construction.
        write_requirements_summary(&handoff, std::slice::from_ref(&doc)).unwrap();
        write_requirements_summary(&handoff, std::slice::from_ref(&doc)).unwrap();
        assert_eq!(
            summary_compare_serialize_count(&path),
            0,
            "a stat-matching repeat call must compare the cached native struct, not build \
             and compare a serde_json::Value"
        );

        // A genuine change must also be detected via the cheap native
        // comparison (no serialization needed to reach that verdict either).
        let mut doc_changed = doc.clone();
        doc_changed.verification.as_mut().unwrap().items[0].sub_items[0].dev_stage =
            Some("in_progress".to_string());
        write_requirements_summary(&handoff, &[doc_changed.clone()]).unwrap();
        assert_eq!(
            summary_compare_serialize_count(&path),
            0,
            "detecting a real change via the in-process cache must not require building a \
             serde_json::Value either"
        );
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("in_progress"));

        // Only an externally modified file (stat mismatch — the cache can
        // no longer be trusted) falls back to a `Value`-based comparison,
        // and only once, not on every subsequent stat-matching call.
        std::fs::write(&path, b"{\"external\":true}").unwrap();
        write_requirements_summary(&handoff, &[doc_changed.clone()]).unwrap();
        assert_eq!(
            summary_compare_serialize_count(&path),
            1,
            "a stat mismatch must fall back to a Value-based comparison exactly once"
        );
        write_requirements_summary(&handoff, &[doc_changed]).unwrap();
        assert_eq!(
            summary_compare_serialize_count(&path),
            1,
            "the next stat-matching call must go back to the fast native comparison"
        );
    }

    /// t370.11 (reviewer round 2): when the slow path (cold cache — e.g. a
    /// freshly started server process — or a stat mismatch) finds the file
    /// already on disk is identical to the freshly computed aggregate, no
    /// write happens, but the cache must still be seeded with what was
    /// observed. Otherwise every subsequent unchanged call in this process
    /// (typically the read-side `doc_req_status` refresh, which never changes
    /// anything) keeps falling back to a full `fs::read` + `Value` compare
    /// until some unrelated write finally populates the cache.
    #[test]
    fn write_requirements_summary_seeds_cache_when_disk_already_matches() {
        use std::os::unix::fs::MetadataExt;

        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();
        let path = docs_dir(&handoff).join("_requirements_summary.json");

        let doc = doc_with_items(
            "doc-1",
            "req-c01",
            vec![section_item(vec![SubItem {
                index: 0,
                description: "req A".to_string(),
                stable_id: Some("C01-1.1".to_string()),
                ..Default::default()
            }])],
        );
        write_doc(&handoff, &doc).unwrap();
        write_requirements_summary(&handoff, std::slice::from_ref(&doc)).unwrap();
        let ino_before = std::fs::metadata(&path).unwrap().ino();

        // Simulate a process restart: this process no longer remembers what
        // it wrote, but the file on disk is still exactly current.
        summary_write_cache()
            .lock()
            .expect("summary write cache poisoned")
            .remove(&path);
        let fallbacks_before = summary_read_fallback_count(&path);

        write_requirements_summary(&handoff, std::slice::from_ref(&doc)).unwrap();
        assert_eq!(
            summary_read_fallback_count(&path),
            fallbacks_before + 1,
            "a cold cache must fall back to reading the file once"
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().ino(),
            ino_before,
            "an already-current file must not be rewritten"
        );

        write_requirements_summary(&handoff, std::slice::from_ref(&doc)).unwrap();
        write_requirements_summary(&handoff, std::slice::from_ref(&doc)).unwrap();
        assert_eq!(
            summary_read_fallback_count(&path),
            fallbacks_before + 1,
            "after observing a matching file once, repeat unchanged calls must use the cache"
        );
    }

    /// t370.11: the in-process write cache must be invalidated by a stat
    /// mismatch. If another writer (a second MCP server process sharing
    /// this `.handoff/`, or a manual edit) replaced the file after this
    /// process cached it, the next call must fall back to reading the disk
    /// and rewrite the correct aggregate, not trust its stale cached
    /// value and skip the write. Without this test, dropping the stamp
    /// check (always trusting the cache) passes every other test.
    #[test]
    fn write_requirements_summary_rereads_and_rewrites_after_external_modification() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();
        let path = docs_dir(&handoff).join("_requirements_summary.json");

        let doc = doc_with_items(
            "doc-1",
            "req-c01",
            vec![section_item(vec![SubItem {
                index: 0,
                description: "req A".to_string(),
                stable_id: Some("C01-1.1".to_string()),
                ..Default::default()
            }])],
        );
        write_doc(&handoff, &doc).unwrap();
        write_requirements_summary(&handoff, std::slice::from_ref(&doc)).unwrap();
        let expected: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let fallbacks_before = summary_read_fallback_count(&path);

        // External writer replaces the file with different content (and a
        // different length, so the stamp is guaranteed to change).
        std::fs::write(&path, b"{\"external\":true}").unwrap();

        // Same inputs as the cached call: only the disk changed.
        write_requirements_summary(&handoff, std::slice::from_ref(&doc)).unwrap();
        assert_eq!(
            summary_read_fallback_count(&path),
            fallbacks_before + 1,
            "a stat mismatch must fall back to reading the file"
        );
        let on_disk: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            on_disk, expected,
            "an externally replaced summary must be rewritten, not skipped via the stale cache"
        );
    }

    /// t370.11 (wiki/240-performance-design.md §4 P-M4 follow-up):
    /// `stat_tasks_input` parallelizes its per-top-level-subtree stat walk
    /// across worker threads once there are enough top-level task
    /// directories to be worth it (measured ~21ms -> ~4-5ms for 3,000
    /// tasks at L scale). The combine step must still find the *global*
    /// max mtime across every worker's chunk, not just e.g. the last
    /// chunk's — this deliberately puts the newest file in the very last
    /// top-level directory (whichever worker chunk that lands in) so a
    /// merge bug that drops or overwrites earlier chunks' maxima would
    /// make this test fail.
    #[test]
    fn compute_derived_inputs_finds_global_max_mtime_across_many_top_level_task_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();
        let tasks_dir = handoff.join("tasks");

        // Enough top-level directories to exceed `available_parallelism()`
        // on any real machine, forcing at least 2 tasks into the same
        // worker's chunk somewhere — exercising the recursive per-chunk
        // walk, not just one file per thread.
        const TOP_LEVEL_DIRS: usize = 40;
        for i in 0..TOP_LEVEL_DIRS {
            let dir = tasks_dir.join(format!("t{i}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("_task.todo.json"), b"{}").unwrap();
        }
        // The newest file lives in the very last top-level directory
        // created above — give it a distinctly later mtime via
        // `filetime`-free means: write it again after a short sleep so its
        // mtime is strictly greater than every earlier file's.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let newest = tasks_dir
            .join(format!("t{}", TOP_LEVEL_DIRS - 1))
            .join("_task.todo.json");
        std::fs::write(&newest, b"{}").unwrap();
        let expected_max_ns = mtime_ns(&std::fs::metadata(&newest).unwrap()).unwrap();

        let inputs = compute_derived_inputs(&handoff).unwrap();
        assert_eq!(inputs.tasks_count, TOP_LEVEL_DIRS);
        assert_eq!(
            inputs.tasks_max_mtime_ns, expected_max_ns,
            "must find the global max mtime, not just one worker chunk's"
        );
    }

    /// `compute_derived_inputs` (wiki/220 §4.3 r3): an empty `.handoff/`
    /// (no `docs/`, no `tasks/`, no `runs/`) reports every count as zero and
    /// `runs_max_id` as `None` — `runs/` in particular does not exist yet
    /// pre-M1 (t360.8), so this must not error.
    #[test]
    fn compute_derived_inputs_on_empty_handoff_is_all_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let inputs = compute_derived_inputs(&handoff).unwrap();

        assert_eq!(inputs.docs_count, 0);
        assert_eq!(inputs.docs_max_mtime_ns, 0);
        assert_eq!(inputs.tasks_count, 0);
        assert_eq!(inputs.tasks_max_mtime_ns, 0);
        assert_eq!(inputs.runs_count, 0);
        assert_eq!(inputs.runs_max_id, None);
    }

    /// `tasks_*` must count `_task.<status>.json` files recursively (child
    /// tasks live in nested directories), and `runs_*` must count files
    /// under month subdirectories while excluding `_latest.json`.
    #[test]
    fn compute_derived_inputs_counts_nested_tasks_and_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(handoff.join("tasks/t1/t1.1")).unwrap();
        std::fs::write(handoff.join("tasks/t1/_task.todo.json"), "{}").unwrap();
        std::fs::write(handoff.join("tasks/t1/t1.1/_task.done.json"), "{}").unwrap();
        std::fs::create_dir_all(handoff.join("runs/2026-09")).unwrap();
        std::fs::write(handoff.join("runs/2026-09/run-001.json"), "{}").unwrap();
        std::fs::write(handoff.join("runs/2026-09/run-002.json"), "{}").unwrap();
        std::fs::write(handoff.join("runs/_latest.json"), "{}").unwrap();

        let inputs = compute_derived_inputs(&handoff).unwrap();

        assert_eq!(inputs.tasks_count, 2, "must recurse into t1/t1.1");
        assert_eq!(
            inputs.runs_count, 2,
            "must exclude _latest.json from the count"
        );
        assert_eq!(inputs.runs_max_id, Some("run-002.json".to_string()));
    }

    #[test]
    fn coverage_counts_by_dev_stage_not_impl_refs() {
        let subs = vec![
            SubItem {
                index: 0,
                description: "impl no refs".to_string(),
                stable_id: Some("POCHI-1.1".to_string()),
                dev_stage: Some("implemented".to_string()),
                impl_refs: vec![],
                ..Default::default()
            },
            SubItem {
                index: 1,
                description: "tested no refs".to_string(),
                stable_id: Some("POCHI-1.2".to_string()),
                dev_stage: Some("tested".to_string()),
                ..Default::default()
            },
            SubItem {
                index: 2,
                description: "verified no refs".to_string(),
                stable_id: Some("POCHI-1.3".to_string()),
                dev_stage: Some("verified".to_string()),
                ..Default::default()
            },
            SubItem {
                index: 3,
                description: "not started".to_string(),
                stable_id: Some("POCHI-1.4".to_string()),
                dev_stage: None,
                ..Default::default()
            },
        ];
        let doc = doc_with_items("doc-1", "req-pochi", vec![section_item(subs)]);
        let summary = aggregate_requirements(&[doc]);

        assert_eq!(summary.total, 4);

        assert_eq!(summary.by_status.get("implemented"), Some(&1));
        assert_eq!(summary.by_status.get("tested"), Some(&1));
        assert_eq!(summary.by_status.get("verified"), Some(&1));
        assert_eq!(summary.by_status.get("not_started"), Some(&1));

        let cat = summary.by_category.get("POCHI").expect("POCHI bucket");
        assert_eq!(cat.total, 4);
        assert_eq!(cat.implemented, 3, "implemented+tested+verified all count");
        assert_eq!(cat.coverage_pct, 75.0);

        assert_eq!(summary.coverage.impl_pct, 75.0);
        assert_eq!(summary.coverage.test_pct, 50.0);
        assert_eq!(summary.coverage.verified_pct, 25.0);

        let p = summary.by_priority.get("unset").expect("unset priority");
        assert_eq!(p.implemented, 3);
        assert_eq!(p.tested, 2);
        assert_eq!(p.verified, 1);
    }

    /// NFR-005 (wiki/220 §4.3, minimal implementation): `aggregate_requirements`
    /// must produce exactly the aggregate recorded in the shared
    /// `tests/fixtures/summary/` contract fixture — the same fixture the
    /// VSCode extension's TS `summarizeRequirements` is tested against
    /// (handoff-vscode t122), so the two implementations cannot silently
    /// drift apart. See `tests/fixtures/summary/README.md` for the fixture
    /// format and the boundary cases it covers.
    #[derive(serde::Deserialize)]
    struct FixtureDoc {
        id: String,
        slug: String,
        #[serde(default)]
        verification: Option<Verification>,
    }

    #[derive(serde::Deserialize)]
    struct FixtureInput {
        docs: Vec<FixtureDoc>,
    }

    #[test]
    fn aggregate_requirements_matches_shared_fixture() {
        let fixtures_dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/summary");

        let input_json = std::fs::read_to_string(fixtures_dir.join("input.json"))
            .expect("tests/fixtures/summary/input.json must exist");
        let input: FixtureInput =
            serde_json::from_str(&input_json).expect("input.json must match FixtureInput shape");

        let docs: Vec<DocMetadata> = input
            .docs
            .into_iter()
            .map(|fd| {
                // Every DocMetadata field aggregate_requirements does not
                // read (title, doc_type, created_at, ...) is an arbitrary
                // placeholder — only id/slug/verification feed the
                // aggregation.
                let mut doc = DocMetadata::new(
                    fd.id,
                    fd.slug,
                    "fixture".to_string(),
                    "spec".to_string(),
                    "2026-09-26T00:00:00Z".to_string(),
                );
                doc.verification = fd.verification;
                doc
            })
            .collect();

        let actual = serde_json::to_value(aggregate_requirements(&docs))
            .expect("RequirementsSummary must serialize");

        let expected_json = std::fs::read_to_string(fixtures_dir.join("expected_output.json"))
            .expect("tests/fixtures/summary/expected_output.json must exist");
        let expected: Value =
            serde_json::from_str(&expected_json).expect("expected_output.json must be valid JSON");

        // Compared as parsed `Value`s, not raw text: by_status/by_priority/
        // by_category/task_coverage are HashMaps, whose serialized key
        // order is not guaranteed (see README.md).
        assert_eq!(
            actual, expected,
            "aggregate_requirements output must match tests/fixtures/summary/expected_output.json exactly"
        );
    }
}

#[cfg(test)]
mod propagate_dev_stage_tests {
    use super::*;
    use crate::storage::docs::{write_doc, DocMetadata, SubItem, Verification, VerificationItem};
    use crate::storage::tasks::{write_task, TaskData, TaskLink};

    fn setup_handoff(tmp: &std::path::Path) -> std::path::PathBuf {
        let handoff = tmp.join(".handoff");
        std::fs::create_dir_all(handoff.join("tasks")).unwrap();
        std::fs::create_dir_all(handoff.join("docs")).unwrap();
        handoff
    }

    fn make_task(handoff: &std::path::Path, id: &str, status: &str, req_links: &[&str]) {
        let task_dir = handoff.join("tasks").join(id);
        std::fs::create_dir_all(&task_dir).unwrap();
        let task_links: Vec<TaskLink> = req_links
            .iter()
            .map(|stable_id| TaskLink {
                target: "doc-1".to_string(),
                link_type: "requirement".to_string(),
                label: Some(stable_id.to_string()),
                ..Default::default()
            })
            .collect();
        let data = TaskData {
            id: id.to_string(),
            title: format!("Task {id}"),
            notes: None,
            priority: None,
            created_at: None,
            updated_at: None,
            completed_at: None,
            labels: Vec::new(),
            links: Vec::new(),
            task_links,
            done_criteria: Vec::new(),
            schedule: None,
            dependencies: Vec::new(),
            order: None,
            assignee: None,
            lock: None,
            scope_paths: Vec::new(),
            extra: std::collections::HashMap::new(),
        };
        write_task(&task_dir, status, &data).unwrap();
    }

    fn make_doc_with_sub_items(handoff: &std::path::Path, sub_items: Vec<SubItem>) {
        let mut doc = DocMetadata::new(
            "doc-1".to_string(),
            "req-test".to_string(),
            "Test Doc".to_string(),
            "spec".to_string(),
            chrono::Utc::now().to_rfc3339(),
        );
        doc.verification = Some(Verification {
            status: "pending".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            items: vec![VerificationItem {
                fragment_seq: Some(1),
                heading: "Section 1".to_string(),
                status: "pending".to_string(),
                impl_refs: Vec::new(),
                test_refs: Vec::new(),
                reviewer: None,
                verified_at: None,
                notes: String::new(),
                content_hash_at_verify: None,
                category: "section".to_string(),
                sub_items,
                label: None,
            }],
        });
        write_doc(handoff, &doc).unwrap();
    }

    fn read_sub_item_dev_stage(handoff: &std::path::Path, sub_index: usize) -> Option<String> {
        let doc = read_doc(handoff, "req-test").unwrap().unwrap();
        let v = doc.verification.as_ref().unwrap();
        v.items[0].sub_items[sub_index].dev_stage.clone()
    }

    fn task_links_for(stable_ids: &[&str]) -> Vec<TaskLink> {
        stable_ids
            .iter()
            .map(|sid| TaskLink {
                target: "doc-1".to_string(),
                link_type: "requirement".to_string(),
                label: Some(sid.to_string()),
                ..Default::default()
            })
            .collect()
    }

    fn task_link_with_role(stable_id: &str, role: &str) -> TaskLink {
        TaskLink {
            target: "doc-1".to_string(),
            link_type: "requirement".to_string(),
            label: Some(stable_id.to_string()),
            role: Some(role.to_string()),
        }
    }

    #[test]
    fn single_task_done_sets_implemented() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());

        make_task(&handoff, "t1", "done", &["REQ-1"]);
        make_doc_with_sub_items(
            &handoff,
            vec![SubItem {
                index: 0,
                description: "req 1".to_string(),
                stable_id: Some("REQ-1".to_string()),
                task_ids: vec!["t1".to_string()],
                ..Default::default()
            }],
        );

        propagate_dev_stage_for_task(&handoff, &task_links_for(&["REQ-1"])).unwrap();

        assert_eq!(
            read_sub_item_dev_stage(&handoff, 0),
            Some("implemented".to_string())
        );
    }

    /// t360.7 (wiki/220 §2.5): `propagate_dev_stage_for_task` is restricted
    /// to `role == "implements"` (or unset, pre-M1 links) links —
    /// `"executes"` links (a test-execution task completing) must never move
    /// a requirement's `dev_stage`. This is what keeps a verification
    /// (right-side / `unit_test`-style) task's completion from marking the
    /// requirement it merely *tests* as implemented.
    #[test]
    fn executes_role_task_done_does_not_change_dev_stage() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());

        make_task(&handoff, "t1", "done", &["REQ-1"]);
        make_doc_with_sub_items(
            &handoff,
            vec![SubItem {
                index: 0,
                description: "req 1".to_string(),
                stable_id: Some("REQ-1".to_string()),
                task_ids: vec!["t1".to_string()],
                dev_stage: Some("not_started".to_string()),
                ..Default::default()
            }],
        );

        propagate_dev_stage_for_task(&handoff, &[task_link_with_role("REQ-1", "executes")])
            .unwrap();

        assert_eq!(
            read_sub_item_dev_stage(&handoff, 0),
            Some("not_started".to_string()),
            "an 'executes' link must not propagate dev_stage"
        );
    }

    /// Same restriction, verified end-to-end through a check-category
    /// (right-side effective layer) `SubItem` as well as the `role` field —
    /// belt-and-suspenders per §2.5 ("role == implements かつ実効層 left
    /// （または層なし）の項目に限定").
    #[test]
    fn check_category_sub_item_does_not_get_dev_stage_propagated_even_with_implements_role() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());

        make_task(&handoff, "t1", "done", &["ST-1"]);
        make_doc_with_sub_items(
            &handoff,
            vec![SubItem {
                index: 0,
                description: "system test 1".to_string(),
                stable_id: Some("ST-1".to_string()),
                task_ids: vec!["t1".to_string()],
                category: "check".to_string(),
                dev_stage: Some("not_started".to_string()),
                ..Default::default()
            }],
        );

        // Role says "implements" (e.g. an older link never re-inferred), but
        // the SubItem's own category says right-side — the item-level guard
        // must still hold.
        propagate_dev_stage_for_task(&handoff, &[task_link_with_role("ST-1", "implements")])
            .unwrap();

        assert_eq!(
            read_sub_item_dev_stage(&handoff, 0),
            Some("not_started".to_string())
        );
    }

    #[test]
    fn single_task_in_progress_sets_in_progress() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());

        make_task(&handoff, "t1", "in_progress", &["REQ-1"]);
        make_doc_with_sub_items(
            &handoff,
            vec![SubItem {
                index: 0,
                description: "req 1".to_string(),
                stable_id: Some("REQ-1".to_string()),
                task_ids: vec!["t1".to_string()],
                ..Default::default()
            }],
        );

        propagate_dev_stage_for_task(&handoff, &task_links_for(&["REQ-1"])).unwrap();

        assert_eq!(
            read_sub_item_dev_stage(&handoff, 0),
            Some("in_progress".to_string())
        );
    }

    #[test]
    fn multi_task_min_strategy_one_todo() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());

        make_task(&handoff, "t1", "done", &["REQ-1"]);
        make_task(&handoff, "t2", "todo", &["REQ-1"]);
        make_doc_with_sub_items(
            &handoff,
            vec![SubItem {
                index: 0,
                description: "req 1".to_string(),
                stable_id: Some("REQ-1".to_string()),
                task_ids: vec!["t1".to_string(), "t2".to_string()],
                ..Default::default()
            }],
        );

        propagate_dev_stage_for_task(&handoff, &task_links_for(&["REQ-1"])).unwrap();

        assert_eq!(
            read_sub_item_dev_stage(&handoff, 0),
            Some("not_started".to_string()),
            "min of done + todo = not_started"
        );
    }

    #[test]
    fn multi_task_all_done_sets_implemented() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());

        make_task(&handoff, "t1", "done", &["REQ-1"]);
        make_task(&handoff, "t2", "done", &["REQ-1"]);
        make_doc_with_sub_items(
            &handoff,
            vec![SubItem {
                index: 0,
                description: "req 1".to_string(),
                stable_id: Some("REQ-1".to_string()),
                task_ids: vec!["t1".to_string(), "t2".to_string()],
                ..Default::default()
            }],
        );

        propagate_dev_stage_for_task(&handoff, &task_links_for(&["REQ-1"])).unwrap();

        assert_eq!(
            read_sub_item_dev_stage(&handoff, 0),
            Some("implemented".to_string()),
            "all done = implemented"
        );
    }

    #[test]
    fn tested_stage_is_protected() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());

        make_task(&handoff, "t1", "done", &["REQ-1"]);
        make_doc_with_sub_items(
            &handoff,
            vec![SubItem {
                index: 0,
                description: "req 1".to_string(),
                stable_id: Some("REQ-1".to_string()),
                dev_stage: Some("tested".to_string()),
                task_ids: vec!["t1".to_string()],
                ..Default::default()
            }],
        );

        propagate_dev_stage_for_task(&handoff, &task_links_for(&["REQ-1"])).unwrap();

        assert_eq!(
            read_sub_item_dev_stage(&handoff, 0),
            Some("tested".to_string()),
            "tested must not be overwritten"
        );
    }

    #[test]
    fn verified_stage_is_protected() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());

        make_task(&handoff, "t1", "in_progress", &["REQ-1"]);
        make_doc_with_sub_items(
            &handoff,
            vec![SubItem {
                index: 0,
                description: "req 1".to_string(),
                stable_id: Some("REQ-1".to_string()),
                dev_stage: Some("verified".to_string()),
                task_ids: vec!["t1".to_string()],
                ..Default::default()
            }],
        );

        propagate_dev_stage_for_task(&handoff, &task_links_for(&["REQ-1"])).unwrap();

        assert_eq!(
            read_sub_item_dev_stage(&handoff, 0),
            Some("verified".to_string()),
            "verified must not be overwritten"
        );
    }

    #[test]
    fn skipped_task_excluded_from_computation() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());

        make_task(&handoff, "t1", "done", &["REQ-1"]);
        make_task(&handoff, "t2", "skipped", &["REQ-1"]);
        make_doc_with_sub_items(
            &handoff,
            vec![SubItem {
                index: 0,
                description: "req 1".to_string(),
                stable_id: Some("REQ-1".to_string()),
                task_ids: vec!["t1".to_string(), "t2".to_string()],
                ..Default::default()
            }],
        );

        propagate_dev_stage_for_task(&handoff, &task_links_for(&["REQ-1"])).unwrap();

        assert_eq!(
            read_sub_item_dev_stage(&handoff, 0),
            Some("implemented".to_string()),
            "skipped excluded, only done remains = implemented"
        );
    }

    #[test]
    fn no_requirement_links_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());

        let empty_links: Vec<TaskLink> = vec![];
        propagate_dev_stage_for_task(&handoff, &empty_links).unwrap();
    }

    #[test]
    fn multi_task_mixed_in_progress_and_done() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());

        make_task(&handoff, "t1", "done", &["REQ-1"]);
        make_task(&handoff, "t2", "in_progress", &["REQ-1"]);
        make_doc_with_sub_items(
            &handoff,
            vec![SubItem {
                index: 0,
                description: "req 1".to_string(),
                stable_id: Some("REQ-1".to_string()),
                task_ids: vec!["t1".to_string(), "t2".to_string()],
                ..Default::default()
            }],
        );

        propagate_dev_stage_for_task(&handoff, &task_links_for(&["REQ-1"])).unwrap();

        assert_eq!(
            read_sub_item_dev_stage(&handoff, 0),
            Some("in_progress".to_string()),
            "min of done + in_progress = in_progress"
        );
    }

    #[test]
    fn requirements_summary_updated_after_propagation() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());

        make_task(&handoff, "t1", "done", &["REQ-1"]);
        make_doc_with_sub_items(
            &handoff,
            vec![SubItem {
                index: 0,
                description: "req 1".to_string(),
                stable_id: Some("REQ-1".to_string()),
                task_ids: vec!["t1".to_string()],
                ..Default::default()
            }],
        );

        propagate_dev_stage_for_task(&handoff, &task_links_for(&["REQ-1"])).unwrap();

        let path = docs_dir(&handoff).join("_requirements_summary.json");
        assert!(path.exists(), "summary file should be regenerated");
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["by_status"]["implemented"], 1);
    }

    #[test]
    fn deleted_task_in_task_ids_does_not_abort_propagation() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());

        make_task(&handoff, "t1", "done", &["REQ-1"]);
        // t2 is referenced in task_ids but does not exist on disk
        make_doc_with_sub_items(
            &handoff,
            vec![SubItem {
                index: 0,
                description: "req 1".to_string(),
                stable_id: Some("REQ-1".to_string()),
                task_ids: vec!["t1".to_string(), "t-deleted".to_string()],
                ..Default::default()
            }],
        );

        propagate_dev_stage_for_task(&handoff, &task_links_for(&["REQ-1"])).unwrap();

        assert_eq!(
            read_sub_item_dev_stage(&handoff, 0),
            Some("implemented".to_string()),
            "deleted task skipped, remaining done task = implemented"
        );
    }
}

/// FR-806 (§4.1, wiki/220): freeform `SubItem`s (living in a
/// `VerificationItem` with `fragment_seq: None`, e.g. one created by an
/// older `handoff_doc_req_import` run before this task's fix, or via
/// `handoff_doc_verify(action="add_item")` on a hand-authored freeform
/// bucket) used to be invisible to `resolve_stable_ids` — every one of
/// `link_requirements_to_task` / `unlink_requirements_from_task` /
/// `propagate_dev_stage_for_task` therefore treated their `stable_id`s as
/// permanently unresolvable. These tests construct a document with such a
/// freeform item directly (bypassing the (now-fixed) `req_import`/`add_item`
/// paths) to prove the fix covers *any* freeform SubItem already on disk,
/// not just newly-created ones.
#[cfg(test)]
mod freeform_sub_item_resolution_tests {
    use super::*;
    use crate::storage::docs::{write_doc, DocMetadata, SubItem, Verification, VerificationItem};
    use crate::storage::tasks::{write_task, TaskData, TaskLink};

    fn setup_handoff(tmp: &std::path::Path) -> std::path::PathBuf {
        let handoff = tmp.join(".handoff");
        std::fs::create_dir_all(handoff.join("tasks")).unwrap();
        std::fs::create_dir_all(handoff.join("docs")).unwrap();
        handoff
    }

    /// Writes a document whose *only* verification item is freeform
    /// (`fragment_seq: None`), containing `sub_items` directly — the exact
    /// shape `handoff_doc_req_import` used to bootstrap before this task's
    /// fix (see wiki/220 §4.1's repro).
    fn make_doc_with_freeform_sub_items(handoff: &std::path::Path, sub_items: Vec<SubItem>) {
        let mut doc = DocMetadata::new(
            "doc-1".to_string(),
            "req-freeform".to_string(),
            "Test Doc".to_string(),
            "spec".to_string(),
            chrono::Utc::now().to_rfc3339(),
        );
        doc.verification = Some(Verification {
            status: "pending".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            items: vec![VerificationItem {
                fragment_seq: None,
                heading: "要件ツリー".to_string(),
                status: "pending".to_string(),
                impl_refs: Vec::new(),
                test_refs: Vec::new(),
                reviewer: None,
                verified_at: None,
                notes: String::new(),
                content_hash_at_verify: None,
                category: "requirement".to_string(),
                sub_items,
                label: Some("imported requirements".to_string()),
            }],
        });
        write_doc(handoff, &doc).unwrap();
    }

    fn make_task(handoff: &std::path::Path, id: &str, status: &str) {
        let task_dir = handoff.join("tasks").join(id);
        std::fs::create_dir_all(&task_dir).unwrap();
        let data = TaskData {
            id: id.to_string(),
            title: format!("Task {id}"),
            notes: None,
            priority: None,
            created_at: None,
            updated_at: None,
            completed_at: None,
            labels: Vec::new(),
            links: Vec::new(),
            task_links: Vec::new(),
            done_criteria: Vec::new(),
            schedule: None,
            dependencies: Vec::new(),
            order: None,
            assignee: None,
            lock: None,
            scope_paths: Vec::new(),
            extra: std::collections::HashMap::new(),
        };
        write_task(&task_dir, status, &data).unwrap();
    }

    fn read_freeform_sub_item(handoff: &std::path::Path) -> SubItem {
        let doc = read_doc(handoff, "req-freeform").unwrap().unwrap();
        doc.verification.unwrap().items[0].sub_items[0].clone()
    }

    #[test]
    fn resolve_stable_ids_finds_freeform_sub_item() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());
        make_doc_with_freeform_sub_items(
            &handoff,
            vec![SubItem {
                index: 0,
                description: "freeform req".to_string(),
                stable_id: Some("FREEFORM-1".to_string()),
                ..Default::default()
            }],
        );

        let (resolved, unresolved, ambiguous) =
            resolve_stable_ids(&handoff, &["FREEFORM-1".to_string()]).unwrap();
        assert!(unresolved.is_empty(), "unresolved={unresolved:?}");
        assert!(ambiguous.is_empty(), "ambiguous={ambiguous:?}");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].fragment_seq, None);
        assert_eq!(resolved[0].stable_id, "FREEFORM-1");
    }

    #[test]
    fn link_requirements_to_task_links_freeform_sub_item() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());
        make_doc_with_freeform_sub_items(
            &handoff,
            vec![SubItem {
                index: 0,
                description: "freeform req".to_string(),
                stable_id: Some("FREEFORM-1".to_string()),
                ..Default::default()
            }],
        );
        make_task(&handoff, "t1", "todo");

        let warnings =
            link_requirements_to_task(&handoff, "t1", &["FREEFORM-1".to_string()]).unwrap();
        assert!(warnings.is_empty(), "warnings={warnings:?}");

        let sub = read_freeform_sub_item(&handoff);
        assert_eq!(sub.task_ids, vec!["t1".to_string()]);
    }

    #[test]
    fn unlink_requirements_from_task_unlinks_freeform_sub_item() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());
        make_doc_with_freeform_sub_items(
            &handoff,
            vec![SubItem {
                index: 0,
                description: "freeform req".to_string(),
                stable_id: Some("FREEFORM-1".to_string()),
                task_ids: vec!["t1".to_string()],
                ..Default::default()
            }],
        );
        make_task(&handoff, "t1", "todo");

        let warnings =
            unlink_requirements_from_task(&handoff, "t1", &["FREEFORM-1".to_string()]).unwrap();
        assert!(warnings.is_empty(), "warnings={warnings:?}");

        let sub = read_freeform_sub_item(&handoff);
        assert!(sub.task_ids.is_empty());
    }

    #[test]
    fn propagate_dev_stage_for_task_updates_freeform_sub_item() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());
        make_doc_with_freeform_sub_items(
            &handoff,
            vec![SubItem {
                index: 0,
                description: "freeform req".to_string(),
                stable_id: Some("FREEFORM-1".to_string()),
                task_ids: vec!["t1".to_string()],
                ..Default::default()
            }],
        );
        make_task(&handoff, "t1", "done");

        let task_links = vec![TaskLink {
            target: "doc-1".to_string(),
            link_type: "requirement".to_string(),
            label: Some("FREEFORM-1".to_string()),
            ..Default::default()
        }];
        propagate_dev_stage_for_task(&handoff, &task_links).unwrap();

        let sub = read_freeform_sub_item(&handoff);
        assert_eq!(sub.dev_stage, Some("implemented".to_string()));
    }
}

/// Review round 2 MAJOR fix (wiki/240 §4 P-M3): `apply_requirement_links`
/// combines what used to be two separate `link_requirements_to_task` /
/// `unlink_requirements_from_task` calls into one `DocSet` load/flush, one
/// task read-modify-write, and one summary write — the shape
/// `update_task.rs`'s `apply_requirement_ids_diff` now always uses when a
/// single `handoff_update_task(requirement_ids=...)` call both adds and
/// removes stable_ids.
#[cfg(test)]
mod apply_requirement_links_tests {
    use super::*;
    use crate::storage::docs::{write_doc, DocMetadata, SubItem, Verification, VerificationItem};
    use crate::storage::tasks::{task_file_write_count, write_task, TaskData, TaskLink};

    fn setup_handoff(tmp: &std::path::Path) -> std::path::PathBuf {
        let handoff = tmp.join(".handoff");
        std::fs::create_dir_all(handoff.join("tasks")).unwrap();
        std::fs::create_dir_all(handoff.join("docs")).unwrap();
        handoff
    }

    fn make_task(
        handoff: &std::path::Path,
        id: &str,
        req_links: &[(&str, &str)],
    ) -> std::path::PathBuf {
        let task_dir = handoff.join("tasks").join(id);
        std::fs::create_dir_all(&task_dir).unwrap();
        let task_links: Vec<TaskLink> = req_links
            .iter()
            .map(|(doc_id, stable_id)| TaskLink {
                target: doc_id.to_string(),
                link_type: "requirement".to_string(),
                label: Some(stable_id.to_string()),
                ..Default::default()
            })
            .collect();
        let data = TaskData {
            id: id.to_string(),
            title: format!("Task {id}"),
            notes: None,
            priority: None,
            created_at: None,
            updated_at: None,
            completed_at: None,
            labels: Vec::new(),
            links: Vec::new(),
            task_links,
            done_criteria: Vec::new(),
            schedule: None,
            dependencies: Vec::new(),
            order: None,
            assignee: None,
            lock: None,
            scope_paths: Vec::new(),
            extra: std::collections::HashMap::new(),
        };
        write_task(&task_dir, "todo", &data).unwrap();
        task_dir
    }

    fn make_doc_with_sub_item(
        handoff: &std::path::Path,
        doc_id: &str,
        slug: &str,
        stable_id: &str,
        task_ids: Vec<String>,
    ) {
        let mut doc = DocMetadata::new(
            doc_id.to_string(),
            slug.to_string(),
            "Test Doc".to_string(),
            "spec".to_string(),
            chrono::Utc::now().to_rfc3339(),
        );
        doc.verification = Some(Verification {
            status: "pending".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            items: vec![VerificationItem {
                fragment_seq: Some(1),
                heading: "Section 1".to_string(),
                status: "pending".to_string(),
                impl_refs: Vec::new(),
                test_refs: Vec::new(),
                reviewer: None,
                verified_at: None,
                notes: String::new(),
                content_hash_at_verify: None,
                category: "section".to_string(),
                sub_items: vec![SubItem {
                    index: 0,
                    description: "req".to_string(),
                    stable_id: Some(stable_id.to_string()),
                    task_ids,
                    ..Default::default()
                }],
                label: None,
            }],
        });
        write_doc(handoff, &doc).unwrap();
    }

    fn sub_item_task_ids(handoff: &std::path::Path, slug: &str) -> Vec<String> {
        let doc = read_doc(handoff, slug).unwrap().unwrap();
        doc.verification.unwrap().items[0].sub_items[0]
            .task_ids
            .clone()
    }

    #[test]
    fn combined_add_and_remove_updates_both_sub_items_in_one_call() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());

        // REQ-B starts linked to t1; REQ-A starts unlinked. A single call
        // adds REQ-A and removes REQ-B — the "swap one requirement for
        // another" shape `apply_requirement_ids_diff` produces whenever
        // `requirement_ids` both grows and shrinks in the same
        // `handoff_update_task` call.
        make_doc_with_sub_item(&handoff, "doc-a", "req-a", "REQ-A", Vec::new());
        make_doc_with_sub_item(&handoff, "doc-b", "req-b", "REQ-B", vec!["t1".to_string()]);
        let task_dir = make_task(&handoff, "t1", &[("doc-b", "REQ-B")]);
        let writes_before = task_file_write_count(&task_dir);

        let warnings = apply_requirement_links(
            &handoff,
            "t1",
            &["REQ-A".to_string()],
            &["REQ-B".to_string()],
            &HashMap::new(),
        )
        .unwrap();
        assert!(warnings.is_empty(), "warnings={warnings:?}");

        assert_eq!(
            sub_item_task_ids(&handoff, "req-a"),
            vec!["t1".to_string()],
            "REQ-A's SubItem must gain t1"
        );
        assert!(
            sub_item_task_ids(&handoff, "req-b").is_empty(),
            "REQ-B's SubItem must lose t1"
        );

        let (data, _status) = read_task(&task_dir).unwrap().unwrap();
        let req_links: Vec<&TaskLink> = data
            .task_links
            .iter()
            .filter(|l| l.link_type == "requirement")
            .collect();
        assert_eq!(req_links.len(), 1, "task_links={:?}", data.task_links);
        assert_eq!(req_links[0].target, "doc-a");
        assert_eq!(req_links[0].label.as_deref(), Some("REQ-A"));

        assert_eq!(
            task_file_write_count(&task_dir) - writes_before,
            1,
            "combined add+remove must write the task file exactly once, not once per side \
             (the bug this test guards against: link_requirements_to_task and \
             unlink_requirements_from_task each doing their own read-modify-write)"
        );
    }

    #[test]
    fn add_only_call_still_writes_task_file_exactly_once() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());

        make_doc_with_sub_item(&handoff, "doc-a", "req-a", "REQ-A", Vec::new());
        let task_dir = make_task(&handoff, "t1", &[]);
        let writes_before = task_file_write_count(&task_dir);

        apply_requirement_links(&handoff, "t1", &["REQ-A".to_string()], &[], &HashMap::new())
            .unwrap();

        assert_eq!(task_file_write_count(&task_dir) - writes_before, 1);
    }

    fn make_doc_with_sub_item_category(
        handoff: &std::path::Path,
        doc_id: &str,
        slug: &str,
        stable_id: &str,
        category: &str,
    ) {
        let mut doc = DocMetadata::new(
            doc_id.to_string(),
            slug.to_string(),
            "Test Doc".to_string(),
            "spec".to_string(),
            chrono::Utc::now().to_rfc3339(),
        );
        doc.verification = Some(Verification {
            status: "pending".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            items: vec![VerificationItem {
                fragment_seq: Some(1),
                heading: "Section 1".to_string(),
                status: "pending".to_string(),
                impl_refs: Vec::new(),
                test_refs: Vec::new(),
                reviewer: None,
                verified_at: None,
                notes: String::new(),
                content_hash_at_verify: None,
                category: "section".to_string(),
                sub_items: vec![SubItem {
                    index: 0,
                    description: "req".to_string(),
                    stable_id: Some(stable_id.to_string()),
                    category: category.to_string(),
                    ..Default::default()
                }],
                label: None,
            }],
        });
        write_doc(handoff, &doc).unwrap();
    }

    fn requirement_link_role<'a>(links: &'a [TaskLink], label: &str) -> Option<&'a str> {
        links
            .iter()
            .find(|l| l.link_type == "requirement" && l.label.as_deref() == Some(label))
            .and_then(|l| l.role.as_deref())
    }

    /// t360.7 (wiki/220 §2.5): role omitted -> inferred from the resolved
    /// SubItem's effective-layer side. `category == "requirement"` (left
    /// side / no layer) infers `"implements"`.
    #[test]
    fn apply_requirement_links_infers_implements_role_when_omitted_for_left_side_item() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());
        make_doc_with_sub_item_category(&handoff, "doc-a", "req-a", "REQ-A", "requirement");
        make_task(&handoff, "t1", &[]);

        apply_requirement_links(&handoff, "t1", &["REQ-A".to_string()], &[], &HashMap::new())
            .unwrap();

        let (data, _) = read_task(&handoff.join("tasks").join("t1"))
            .unwrap()
            .unwrap();
        assert_eq!(
            requirement_link_role(&data.task_links, "REQ-A"),
            Some("implements")
        );
    }

    /// t360.7 (wiki/220 §2.5): role omitted -> `category == "check"` (right
    /// side, e.g. a system_test/unit_test layer item) infers `"executes"`.
    #[test]
    fn apply_requirement_links_infers_executes_role_when_omitted_for_check_category_item() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());
        make_doc_with_sub_item_category(&handoff, "doc-a", "req-a", "ST-001", "check");
        make_task(&handoff, "t1", &[]);

        apply_requirement_links(
            &handoff,
            "t1",
            &["ST-001".to_string()],
            &[],
            &HashMap::new(),
        )
        .unwrap();

        let (data, _) = read_task(&handoff.join("tasks").join("t1"))
            .unwrap()
            .unwrap();
        assert_eq!(
            requirement_link_role(&data.task_links, "ST-001"),
            Some("executes")
        );
    }

    /// t360.7: an explicit `roles` entry overrides the inferred value even
    /// when it disagrees with the SubItem's effective-layer side.
    #[test]
    fn apply_requirement_links_honors_explicit_role_override() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());
        make_doc_with_sub_item_category(&handoff, "doc-a", "req-a", "REQ-A", "requirement");
        make_task(&handoff, "t1", &[]);

        let mut roles = HashMap::new();
        roles.insert("REQ-A".to_string(), "executes".to_string());
        apply_requirement_links(&handoff, "t1", &["REQ-A".to_string()], &[], &roles).unwrap();

        let (data, _) = read_task(&handoff.join("tasks").join("t1"))
            .unwrap()
            .unwrap();
        assert_eq!(
            requirement_link_role(&data.task_links, "REQ-A"),
            Some("executes")
        );
    }

    /// t360.7 (wiki/220 §2.5, unresolved-link cleanup): a `to_remove`
    /// stable_id that no longer resolves to any `SubItem` (its requirement
    /// item was deleted) must still have its task-side `task_links` entry
    /// removed — the key is `label`, not resolvability (a deleted item has
    /// nothing left to resolve against).
    #[test]
    fn apply_requirement_links_unlinks_task_when_stable_id_no_longer_resolves() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());
        // No document declares "GONE-1" at all (simulates the item having
        // been deleted from its owning document/layer body).
        let task_dir = make_task(&handoff, "t1", &[("doc-a", "GONE-1")]);

        let warnings = apply_requirement_links(
            &handoff,
            "t1",
            &[],
            &["GONE-1".to_string()],
            &HashMap::new(),
        )
        .unwrap();
        assert!(
            warnings.iter().any(|w| w.contains("GONE-1")),
            "warnings={warnings:?}"
        );

        let (data, _) = read_task(&task_dir).unwrap().unwrap();
        assert!(
            !data
                .task_links
                .iter()
                .any(|l| l.link_type == "requirement" && l.label.as_deref() == Some("GONE-1")),
            "task_links={:?}",
            data.task_links
        );
    }

    /// Documents the exact regression this task fixes: calling the add-only
    /// and remove-only wrappers *separately* for one logical
    /// add-and-remove update — the shape `apply_requirement_ids_diff` used
    /// before review round 2 — writes the task file twice. The combined
    /// `apply_requirement_links` call above
    /// (`combined_add_and_remove_updates_both_sub_items_in_one_call`) does
    /// the same logical change in one write. Both wrappers delegate to
    /// `apply_requirement_links` internally, so this is a live comparison
    /// against the current code, not a frozen snapshot of removed code.
    #[test]
    fn separate_add_then_remove_calls_write_the_task_file_twice() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());

        make_doc_with_sub_item(&handoff, "doc-a", "req-a", "REQ-A", Vec::new());
        make_doc_with_sub_item(&handoff, "doc-b", "req-b", "REQ-B", vec!["t1".to_string()]);
        let task_dir = make_task(&handoff, "t1", &[("doc-b", "REQ-B")]);
        let writes_before = task_file_write_count(&task_dir);

        link_requirements_to_task(&handoff, "t1", &["REQ-A".to_string()]).unwrap();
        unlink_requirements_from_task(&handoff, "t1", &["REQ-B".to_string()]).unwrap();

        assert_eq!(
            task_file_write_count(&task_dir) - writes_before,
            2,
            "two separate calls for one logical add+remove still cost two writes — this is \
             exactly why apply_requirement_ids_diff must call apply_requirement_links once \
             instead"
        );
    }

    #[test]
    fn empty_add_and_remove_is_a_no_op_and_does_not_write() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());
        let task_dir = make_task(&handoff, "t1", &[]);
        let writes_before = task_file_write_count(&task_dir);

        let warnings = apply_requirement_links(&handoff, "t1", &[], &[], &HashMap::new()).unwrap();

        assert!(warnings.is_empty());
        assert_eq!(task_file_write_count(&task_dir) - writes_before, 0);
    }
}

/// t360.7 (wiki/220 §2.5 full rebuild / §4.3 r3): `rebuild_item_task_ids_full`
/// is the expensive, all-tasks-scanning repair reserved for `trace_report`
/// self-repair and the explicit `handoff_doc_repair_task_ids` tool — gated on
/// the `tasks_*` input fingerprint so calling it twice in a row with no task
/// changes in between is a no-op.
#[cfg(test)]
mod full_rebuild_tests {
    use super::*;
    use crate::storage::docs::{write_doc, DocMetadata, SubItem, Verification, VerificationItem};
    use crate::storage::tasks::{write_task, TaskData, TaskLink};

    fn setup_handoff(tmp: &std::path::Path) -> std::path::PathBuf {
        let handoff = tmp.join(".handoff");
        std::fs::create_dir_all(handoff.join("tasks")).unwrap();
        std::fs::create_dir_all(handoff.join("docs")).unwrap();
        handoff
    }

    fn make_task_with_links(handoff: &std::path::Path, id: &str, req_links: &[(&str, &str)]) {
        let task_dir = handoff.join("tasks").join(id);
        std::fs::create_dir_all(&task_dir).unwrap();
        let task_links: Vec<TaskLink> = req_links
            .iter()
            .map(|(doc_id, stable_id)| TaskLink {
                target: doc_id.to_string(),
                link_type: "requirement".to_string(),
                label: Some(stable_id.to_string()),
                ..Default::default()
            })
            .collect();
        let data = TaskData {
            id: id.to_string(),
            title: format!("Task {id}"),
            notes: None,
            priority: None,
            created_at: None,
            updated_at: None,
            completed_at: None,
            labels: Vec::new(),
            links: Vec::new(),
            task_links,
            done_criteria: Vec::new(),
            schedule: None,
            dependencies: Vec::new(),
            order: None,
            assignee: None,
            lock: None,
            scope_paths: Vec::new(),
            extra: std::collections::HashMap::new(),
        };
        write_task(&task_dir, "todo", &data).unwrap();
    }

    fn make_doc_with_sub_item(
        handoff: &std::path::Path,
        doc_id: &str,
        slug: &str,
        stable_id: &str,
        task_ids: Vec<String>,
    ) {
        let mut doc = DocMetadata::new(
            doc_id.to_string(),
            slug.to_string(),
            "Test Doc".to_string(),
            "spec".to_string(),
            chrono::Utc::now().to_rfc3339(),
        );
        doc.verification = Some(Verification {
            status: "pending".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            items: vec![VerificationItem {
                fragment_seq: Some(1),
                heading: "Section 1".to_string(),
                status: "pending".to_string(),
                impl_refs: Vec::new(),
                test_refs: Vec::new(),
                reviewer: None,
                verified_at: None,
                notes: String::new(),
                content_hash_at_verify: None,
                category: "section".to_string(),
                sub_items: vec![SubItem {
                    index: 0,
                    description: "req".to_string(),
                    stable_id: Some(stable_id.to_string()),
                    task_ids,
                    ..Default::default()
                }],
                label: None,
            }],
        });
        write_doc(handoff, &doc).unwrap();
    }

    fn sub_item_task_ids(handoff: &std::path::Path, slug: &str) -> Vec<String> {
        let doc = read_doc(handoff, slug).unwrap().unwrap();
        doc.verification.unwrap().items[0].sub_items[0]
            .task_ids
            .clone()
    }

    /// The task side (`TaskData.task_links`) is the source of truth (D3):
    /// a `SubItem.task_ids` that has drifted out of sync (here, missing t1
    /// entirely and carrying a stale "ghost" task_id) is corrected by a full
    /// rebuild.
    #[test]
    fn full_rebuild_recomputes_task_ids_from_task_links_source_of_truth() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());
        make_task_with_links(&handoff, "t1", &[("doc-a", "REQ-A")]);
        make_doc_with_sub_item(
            &handoff,
            "doc-a",
            "req-a",
            "REQ-A",
            vec!["ghost".to_string()],
        );

        let outcome = rebuild_item_task_ids_full(&handoff).unwrap();

        assert!(outcome.ran, "must run when no prior fingerprint exists");
        assert_eq!(outcome.sub_items_changed, 1);
        assert_eq!(sub_item_task_ids(&handoff, "req-a"), vec!["t1".to_string()]);
    }

    /// Calling it a second time with no task changes in between must be a
    /// no-op (§4.3 r3 fingerprint gate: only `trace_report` self-repair /
    /// explicit repair, and only when `tasks_*` differs from last time).
    #[test]
    fn full_rebuild_is_a_no_op_when_tasks_fingerprint_is_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());
        make_task_with_links(&handoff, "t1", &[("doc-a", "REQ-A")]);
        make_doc_with_sub_item(&handoff, "doc-a", "req-a", "REQ-A", Vec::new());

        let first = rebuild_item_task_ids_full(&handoff).unwrap();
        assert!(first.ran);

        let second = rebuild_item_task_ids_full(&handoff).unwrap();
        assert!(
            !second.ran,
            "a second call with no task changes must be a no-op"
        );
    }

    /// Round-2 rework MINOR fix: the fingerprint gate compares *both*
    /// `tasks_max_mtime_ns` AND `tasks_count` (§4.3 r3, done_criteria calls
    /// out both fields explicitly) — but the only fingerprint-change test
    /// above (`full_rebuild_runs_again_after_tasks_fingerprint_changes`) adds
    /// a new task file, which moves both fields together, so a mutation that
    /// dropped the `tasks_count` half of the comparison (keeping only
    /// `tasks_max_mtime_ns`) still passed all three prior tests. This test
    /// isolates the `tasks_count`-only-changed branch: it *removes* the
    /// task with the smaller (non-max) mtime, so `tasks_max_mtime_ns` is
    /// unchanged (the remaining task's file was never touched) while
    /// `tasks_count` drops from 2 to 1.
    #[test]
    fn full_rebuild_runs_again_when_only_tasks_count_changes_but_max_mtime_does_not() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());
        // t1 is written first (smaller mtime) and carries the link under
        // test; t2 is written after a delay so it alone holds the max mtime.
        make_task_with_links(&handoff, "t1", &[("doc-a", "REQ-A")]);
        std::thread::sleep(std::time::Duration::from_millis(20));
        make_task_with_links(&handoff, "t2", &[]);
        make_doc_with_sub_item(&handoff, "doc-a", "req-a", "REQ-A", Vec::new());

        let first = rebuild_item_task_ids_full(&handoff).unwrap();
        assert!(first.ran);
        assert_eq!(sub_item_task_ids(&handoff, "req-a"), vec!["t1".to_string()]);

        let tasks_dir = handoff.join("tasks");
        let before = stat_tasks_input(&tasks_dir).unwrap();

        // Remove t1 (the non-max-mtime task): tasks_count drops 2 -> 1, but
        // tasks_max_mtime_ns is unaffected because t2's file is untouched.
        std::fs::remove_dir_all(tasks_dir.join("t1")).unwrap();

        let after = stat_tasks_input(&tasks_dir).unwrap();
        assert_eq!(
            after.0, before.0,
            "tasks_max_mtime_ns must be unchanged by removing the non-max task"
        );
        assert_eq!(after.1, before.1 - 1, "tasks_count must drop by one");

        let second = rebuild_item_task_ids_full(&handoff).unwrap();
        assert!(
            second.ran,
            "tasks_count alone changed (max_mtime unchanged); a full rebuild must still run"
        );
        assert_eq!(
            sub_item_task_ids(&handoff, "req-a"),
            Vec::<String>::new(),
            "t1's link must be gone from the rebuilt task_ids after t1 was removed"
        );
    }

    /// Once a task changes (moving the `tasks_*` fingerprint), a subsequent
    /// call runs again and reflects the new state.
    #[test]
    fn full_rebuild_runs_again_after_tasks_fingerprint_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());
        make_task_with_links(&handoff, "t1", &[("doc-a", "REQ-A")]);
        make_doc_with_sub_item(&handoff, "doc-a", "req-a", "REQ-A", Vec::new());

        let first = rebuild_item_task_ids_full(&handoff).unwrap();
        assert!(first.ran);
        assert_eq!(sub_item_task_ids(&handoff, "req-a"), vec!["t1".to_string()]);

        // A new task linking the same requirement changes tasks_count.
        make_task_with_links(&handoff, "t2", &[("doc-a", "REQ-A")]);
        let second = rebuild_item_task_ids_full(&handoff).unwrap();
        assert!(second.ran, "tasks_count changed; must run again");
        let ids = sub_item_task_ids(&handoff, "req-a");
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"t1".to_string()));
        assert!(ids.contains(&"t2".to_string()));
    }
}

/// M0-b (wiki/220-vmodel-integration-design.md §4.2, FR-105): a `stable_id`
/// is only guaranteed unique *within* one document (`derive_stable_id`'s
/// `existing_ids` collision check never sees other documents). These tests
/// build two documents that independently carry the same `stable_id` and
/// confirm the corpus-wide collision report (`collect_all_stable_ids`) and
/// the ambiguous-resolution behavior (`resolve_stable_ids_in`,
/// `apply_requirement_links`) this task adds around that pre-existing gap.
#[cfg(test)]
mod stable_id_collision_tests {
    use super::*;
    use crate::storage::docs::{write_doc, DocMetadata, SubItem, Verification, VerificationItem};
    use crate::storage::tasks::{write_task, TaskData};

    fn setup_handoff(tmp: &std::path::Path) -> std::path::PathBuf {
        let handoff = tmp.join(".handoff");
        std::fs::create_dir_all(handoff.join("tasks")).unwrap();
        std::fs::create_dir_all(handoff.join("docs")).unwrap();
        handoff
    }

    fn make_doc_with_sub_item(
        handoff: &std::path::Path,
        doc_id: &str,
        slug: &str,
        stable_id: &str,
    ) {
        let mut doc = DocMetadata::new(
            doc_id.to_string(),
            slug.to_string(),
            "Test Doc".to_string(),
            "spec".to_string(),
            chrono::Utc::now().to_rfc3339(),
        );
        doc.verification = Some(Verification {
            status: "pending".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            items: vec![VerificationItem {
                fragment_seq: Some(1),
                heading: "Section 1".to_string(),
                status: "pending".to_string(),
                impl_refs: Vec::new(),
                test_refs: Vec::new(),
                reviewer: None,
                verified_at: None,
                notes: String::new(),
                content_hash_at_verify: None,
                category: "section".to_string(),
                sub_items: vec![SubItem {
                    index: 0,
                    description: "req".to_string(),
                    stable_id: Some(stable_id.to_string()),
                    ..Default::default()
                }],
                label: None,
            }],
        });
        write_doc(handoff, &doc).unwrap();
    }

    fn make_task(handoff: &std::path::Path, id: &str) -> std::path::PathBuf {
        let task_dir = handoff.join("tasks").join(id);
        std::fs::create_dir_all(&task_dir).unwrap();
        let data = TaskData {
            id: id.to_string(),
            title: format!("Task {id}"),
            notes: None,
            priority: None,
            created_at: None,
            updated_at: None,
            completed_at: None,
            labels: Vec::new(),
            links: Vec::new(),
            task_links: Vec::new(),
            done_criteria: Vec::new(),
            schedule: None,
            dependencies: Vec::new(),
            order: None,
            assignee: None,
            lock: None,
            scope_paths: Vec::new(),
            extra: std::collections::HashMap::new(),
        };
        write_task(&task_dir, "todo", &data).unwrap();
        task_dir
    }

    #[test]
    fn collect_all_stable_ids_reports_every_document_that_owns_an_id() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());
        make_doc_with_sub_item(&handoff, "doc-a", "req-a", "DUP-1");
        make_doc_with_sub_item(&handoff, "doc-b", "req-b", "DUP-1");
        make_doc_with_sub_item(&handoff, "doc-c", "req-c", "UNIQUE-1");

        let docs = read_all_docs(&handoff).unwrap();
        let all = collect_all_stable_ids(&docs);

        let mut dup_owners = all.get("DUP-1").expect("DUP-1 must be present").clone();
        dup_owners.sort();
        assert_eq!(dup_owners, vec!["doc-a".to_string(), "doc-b".to_string()]);
        assert_eq!(all.get("UNIQUE-1").unwrap().len(), 1);
    }

    #[test]
    fn resolve_stable_ids_in_reports_id_owned_by_two_documents_as_ambiguous() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());
        make_doc_with_sub_item(&handoff, "doc-a", "req-a", "DUP-1");
        make_doc_with_sub_item(&handoff, "doc-b", "req-b", "DUP-1");
        make_doc_with_sub_item(&handoff, "doc-c", "req-c", "UNIQUE-1");

        let docs = read_all_docs(&handoff).unwrap();
        let (resolved, unresolved, ambiguous) = resolve_stable_ids_in(
            &docs,
            &[
                "DUP-1".to_string(),
                "UNIQUE-1".to_string(),
                "MISSING-1".to_string(),
            ],
        );

        assert_eq!(ambiguous, vec!["DUP-1".to_string()]);
        assert_eq!(unresolved, vec!["MISSING-1".to_string()]);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].stable_id, "UNIQUE-1");
        assert!(
            !resolved.iter().any(|r| r.stable_id == "DUP-1"),
            "an ambiguous id must not be linked to either owning document: {resolved:?}"
        );
    }

    #[test]
    fn apply_requirement_links_warns_ambiguous_stable_id_and_links_neither_document() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());
        make_doc_with_sub_item(&handoff, "doc-a", "req-a", "DUP-1");
        make_doc_with_sub_item(&handoff, "doc-b", "req-b", "DUP-1");
        make_task(&handoff, "t1");

        let warnings =
            apply_requirement_links(&handoff, "t1", &["DUP-1".to_string()], &[], &HashMap::new())
                .unwrap();

        assert!(
            warnings
                .iter()
                .any(|w| w.contains("ambiguous") && w.contains("DUP-1")),
            "warnings={warnings:?}"
        );
        let doc_a = read_doc(&handoff, "req-a").unwrap().unwrap();
        assert!(doc_a.verification.unwrap().items[0].sub_items[0]
            .task_ids
            .is_empty());
        let doc_b = read_doc(&handoff, "req-b").unwrap().unwrap();
        assert!(doc_b.verification.unwrap().items[0].sub_items[0]
            .task_ids
            .is_empty());
    }

    #[test]
    fn cross_document_collision_warning_reports_other_document_but_none_for_own() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = setup_handoff(tmp.path());
        make_doc_with_sub_item(&handoff, "doc-a", "req-a", "DUP-1");
        make_doc_with_sub_item(&handoff, "doc-b", "req-b", "DUP-1");

        // Another document already owns DUP-1 -> warns.
        let warning = cross_document_collision_warning(&handoff, "doc-a", "DUP-1").unwrap();
        assert!(
            warning.is_some(),
            "expected a warning for a cross-document duplicate"
        );
        assert!(warning.unwrap().contains("doc-b"));

        // An id nobody else owns -> no warning, even for the doc that owns it.
        let warning = cross_document_collision_warning(&handoff, "doc-a", "NOBODY-ELSE").unwrap();
        assert!(warning.is_none());
    }
}

/// t370.12 (wiki/240-performance-design.md §4 P-M1, PR-4): `set_dev_stage` /
/// `link_task` only ever mutate `SubItem`/`VerificationItem` metadata
/// fields, never the document body or a section's `content_hash` — so
/// resolving the document lazily (no `lexsim::content_hash` pass at read
/// time) and reusing this process's already-proven `content_hash` at write
/// time (both paid, pre-fix, on every call at JA scale) must not cost a
/// single `lexsim::content_hash` call once the doc's hash has been proven
/// once. `check`/`check_all` still need a real per-section hash
/// (`content_hash_at_verify`) and must be unaffected.
#[cfg(test)]
mod doc_verify_hash_reuse_tests {
    use super::*;
    use crate::storage::docs::{
        doc_body_path, hash_compute_count, write_doc_with_body, DocMetadata, SubItem, Verification,
        VerificationItem,
    };

    fn ctx(handoff: PathBuf) -> HandlerContext {
        HandlerContext {
            agent_id: None,
            project_dir: handoff.parent().unwrap().to_path_buf(),
            handoff_dir: handoff,
        }
    }

    fn setup() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(handoff.join("docs")).unwrap();
        (tmp, handoff)
    }

    /// Writes a document the way `doc_save` would: `content_hash` already
    /// computed against `body` before the write, so the on-disk value (and
    /// this process's trusted-hash cache entry for it) is proven-correct —
    /// mirrors the state any real document is in immediately after a save.
    fn seed_doc(handoff: &Path, body: &str) {
        let mut doc = DocMetadata::new(
            "doc-1".to_string(),
            "hash-reuse".to_string(),
            "Hash Reuse Test Doc".to_string(),
            "spec".to_string(),
            chrono::Utc::now().to_rfc3339(),
        );
        doc.sections = compute_sections(&split(body, doc.split_level).unwrap(), false);
        doc.content_hash = Some(lexsim::content_hash(body));
        doc.verification = Some(Verification {
            status: "pending".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            items: vec![VerificationItem {
                fragment_seq: Some(1),
                heading: "Section 1".to_string(),
                status: "pending".to_string(),
                impl_refs: Vec::new(),
                test_refs: Vec::new(),
                reviewer: None,
                verified_at: None,
                notes: String::new(),
                content_hash_at_verify: None,
                category: "section".to_string(),
                sub_items: vec![SubItem {
                    index: 0,
                    description: "Test requirement".to_string(),
                    stable_id: Some("C01-1.1".to_string()),
                    dev_stage: Some("not_started".to_string()),
                    ..Default::default()
                }],
                label: None,
            }],
        });
        write_doc_with_body(handoff, &doc, body).unwrap();
    }

    #[test]
    fn set_dev_stage_does_not_recompute_content_hash_once_proven() {
        let (_tmp, handoff) = setup();
        let body = "# Doc\n\n## Section 1\n\nBody one.\n";
        seed_doc(&handoff, body);
        let path = doc_body_path(&handoff, "hash-reuse");
        let before = hash_compute_count(&path);

        let result = handle_doc_verify(
            &ctx(handoff.clone()),
            &json!({
                "doc_id": "hash-reuse", "action": "set_dev_stage",
                "fragment_seq": 1, "sub_item_index": 0, "dev_stage": "in_progress",
            }),
        )
        .unwrap();
        assert!(result.contains("doc-1"));

        assert_eq!(
            hash_compute_count(&path),
            before,
            "set_dev_stage must resolve the document lazily and reuse the already-proven \
             content_hash at write time, never recomputing lexsim::content_hash"
        );

        // Correctness: the persisted content_hash must still be right.
        let reread = read_doc_hashed(&handoff, "hash-reuse").unwrap().unwrap();
        assert_eq!(
            reread.content_hash.as_deref(),
            Some(lexsim::content_hash(body).as_str())
        );
        assert_eq!(
            reread.verification.unwrap().items[0].sub_items[0]
                .dev_stage
                .as_deref(),
            Some("in_progress")
        );
    }

    #[test]
    fn link_task_does_not_recompute_content_hash_once_proven() {
        let (_tmp, handoff) = setup();
        let body = "# Doc\n\n## Section 1\n\nBody one.\n";
        seed_doc(&handoff, body);
        let path = doc_body_path(&handoff, "hash-reuse");
        let before = hash_compute_count(&path);

        handle_doc_verify(
            &ctx(handoff.clone()),
            &json!({
                "doc_id": "hash-reuse", "action": "link_task",
                "fragment_seq": 1, "sub_item_index": 0, "task_ids": [],
            }),
        )
        .unwrap();

        assert_eq!(
            hash_compute_count(&path),
            before,
            "link_task must resolve the document lazily and reuse the already-proven \
             content_hash at write time, never recomputing lexsim::content_hash"
        );

        let reread = read_doc_hashed(&handoff, "hash-reuse").unwrap().unwrap();
        assert_eq!(
            reread.content_hash.as_deref(),
            Some(lexsim::content_hash(body).as_str())
        );
    }

    /// Regression guard: `check` still needs a trustworthy per-section
    /// `content_hash` for `content_hash_at_verify` — it must keep using the
    /// hashed resolve path (unaffected by this task's laziness change).
    #[test]
    fn check_action_still_records_a_real_content_hash_at_verify() {
        let (_tmp, handoff) = setup();
        let body = "# Doc\n\n## Section 1\n\nBody one.\n";
        seed_doc(&handoff, body);

        handle_doc_verify(
            &ctx(handoff.clone()),
            &json!({
                "doc_id": "hash-reuse", "action": "check",
                "sub_item_id": "C01-1.1",
            }),
        )
        .unwrap();

        let reread = read_doc_hashed(&handoff, "hash-reuse").unwrap().unwrap();
        let sub = &reread.verification.unwrap().items[0].sub_items[0];
        assert_eq!(sub.status, "verified");
    }

    /// t370.12 rework (MINOR, integration feedback round 1): `need_hash`
    /// must be a deny-list (default to needing a hash, with a short,
    /// explicit list of actions proven never to read one) rather than an
    /// allow-list (default to *not* needing one) — an allow-list silently
    /// defaults any future action added to `handle_doc_verify`'s match block
    /// to the lazy (no-hash) path unless a developer remembers to add it
    /// here too. `"check"`/`"check_all"` are the only two actions that read
    /// a section's `content_hash` (for `content_hash_at_verify`); every
    /// other currently-known action, plus anything not yet written, must
    /// default to `true`.
    #[test]
    fn action_needs_content_hash_defaults_to_true_for_unknown_actions() {
        assert!(action_needs_content_hash("check"));
        assert!(action_needs_content_hash("check_all"));
        assert!(
            action_needs_content_hash("some_future_action_not_yet_written"),
            "an action this function doesn't recognize must default to needing a hash, not \
             silently skip it"
        );
        for safe in [
            "generate",
            "skip",
            "sync",
            "set_refs",
            "set_dev_stage",
            "set_priority",
            "link_task",
            "add_item",
            "backfill_stable_ids",
            "suggest_refs",
        ] {
            assert!(
                !action_needs_content_hash(safe),
                "{safe} is proven to never read a content_hash and must stay on the lazy path"
            );
        }
    }
}

/// wiki/220-vmodel-integration-design.md §2.4, M1 t360.6: `sync_layer_items`
/// wired into `doc_save`/`doc_update_section`/`doc_verify(action="sync")`.
#[cfg(test)]
mod layer_sync_wiring_tests {
    use super::*;
    use crate::storage::docs::read_doc_hashed;

    fn ctx(handoff: PathBuf) -> HandlerContext {
        HandlerContext {
            agent_id: None,
            project_dir: handoff.parent().unwrap().to_path_buf(),
            handoff_dir: handoff,
        }
    }

    fn setup() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(handoff.join("docs")).unwrap();
        (tmp, handoff)
    }

    /// `doc_save(layer=...)` with a body must populate `verification` from
    /// the body's item headings — the timing rule in §2.4 ("実行タイミング:
    /// doc_save (...) の最後").
    #[test]
    fn doc_save_with_layer_syncs_body_items_into_verification() {
        let (_tmp, handoff) = setup();
        let body = "# Basic spec\n\n### SPEC-001 Lockout\n\n- priority: P1\n\nBody.\n";
        let result = handle_doc_save(
            &ctx(handoff.clone()),
            &json!({
                "slug": "spec-doc",
                "title": "Basic spec doc",
                "body": body,
                "layer": "basic_spec",
            }),
        )
        .unwrap();
        let out: Value = serde_json::from_str(&result).unwrap();
        let doc_id = out["doc_id"].as_str().unwrap();

        let doc = read_doc_hashed(&handoff, "spec-doc").unwrap().unwrap();
        assert_eq!(doc.id, doc_id);
        let v = doc
            .verification
            .expect("layer doc_save must sync a verification matrix");
        let sub = v
            .items
            .iter()
            .flat_map(|i| i.sub_items.iter())
            .find(|s| s.stable_id.as_deref() == Some("SPEC-001"))
            .expect("SPEC-001 synced from body");
        assert_eq!(sub.origin.as_deref(), Some("body"));
        assert_eq!(sub.priority.as_deref(), Some("P1"));
        assert_eq!(sub.category, "requirement");
        assert!(doc.source.body_raw_hash.is_some());
    }

    /// A metadata-only `doc_save` (no `body`/`append_body`) on an already
    /// synced layer document, whose `.md` file was not hand-edited in
    /// between, must not re-run the parse+rebuild pass — the body's raw byte
    /// hash still matches `source.body_raw_hash` from the previous sync
    /// (wiki/240-performance-design.md §5-3). Observable here as: the
    /// existing runtime field set by a manual edit of the in-memory matrix
    /// survives, and the SubItem's `body_hash` (which sync would otherwise
    /// freshly recompute — a harmless no-op here, but demonstrates the skip
    /// indirectly via the retained warnings behavior) round-trips with no
    /// warnings.
    #[test]
    fn metadata_only_doc_save_on_layer_doc_does_not_lose_manual_runtime_edits() {
        let (_tmp, handoff) = setup();
        let body = "# Basic spec\n\n### SPEC-001 Lockout\n\nBody.\n";
        let saved = handle_doc_save(
            &ctx(handoff.clone()),
            &json!({
                "slug": "spec-doc",
                "title": "Basic spec doc",
                "body": body,
                "layer": "basic_spec",
            }),
        )
        .unwrap();
        let out: Value = serde_json::from_str(&saved).unwrap();
        let doc_id = out["doc_id"].as_str().unwrap().to_string();

        // Manually set a runtime field, as `doc_verify(set_dev_stage)` would.
        // Rework round 2 (MAJOR fix): also directly corrupt a *body-owned*
        // field (`description`) to a value that mismatches the actual body
        // text — this is the part that actually distinguishes "the
        // short-circuit skipped the resync" from "dev_stage is restored by
        // stable_id regardless of whether a resync ran at all" (the previous
        // version of this test asserted only `dev_stage`, which round-trips
        // either way and therefore passed even with the short-circuit
        // removed entirely, per rework feedback). If the short-circuit is
        // ever accidentally removed, `sync_layer_items` would reset
        // `description` back to "Lockout" (re-parsed from the unchanged
        // body) on the very next metadata-only save — this test would then
        // fail on the `description` assertion below.
        {
            let mut doc = read_doc_hashed(&handoff, "spec-doc").unwrap().unwrap();
            let v = doc.verification.as_mut().unwrap();
            let sub = v
                .items
                .iter_mut()
                .flat_map(|i| i.sub_items.iter_mut())
                .find(|s| s.stable_id.as_deref() == Some("SPEC-001"))
                .unwrap();
            sub.dev_stage = Some("implemented".to_string());
            sub.description = "MANUALLY EDITED, MISMATCHES THE BODY".to_string();
            write_doc(&handoff, &doc).unwrap();
        }

        // Metadata-only save: no body/append_body, and no layer/split_level
        // change either — the raw-body-hash short-circuit must skip the
        // resync entirely.
        handle_doc_save(
            &ctx(handoff.clone()),
            &json!({ "doc_id": doc_id, "tags": ["x"] }),
        )
        .unwrap();

        let doc = read_doc_hashed(&handoff, "spec-doc").unwrap().unwrap();
        let v = doc.verification.unwrap();
        let sub = v
            .items
            .iter()
            .flat_map(|i| i.sub_items.iter())
            .find(|s| s.stable_id.as_deref() == Some("SPEC-001"))
            .unwrap();
        assert_eq!(
            sub.dev_stage.as_deref(),
            Some("implemented"),
            "metadata-only save must not discard a manually-set runtime field by re-syncing \
             from an unchanged body"
        );
        assert_eq!(
            sub.description, "MANUALLY EDITED, MISMATCHES THE BODY",
            "metadata-only save on an unchanged layer/split_level/body must skip the resync \
             entirely — a body-owned field like description would otherwise be reset back to \
             the body's own text (\"Lockout\")"
        );
    }

    /// `doc_update_section` on a layer document re-syncs the matrix at the
    /// end of the call, picking up a body item added via the section
    /// replacement.
    #[test]
    fn doc_update_section_on_layer_doc_resyncs_new_body_item() {
        let (_tmp, handoff) = setup();
        let body = "# Basic spec\n\n## Section A\n\nOld content.\n";
        let saved = handle_doc_save(
            &ctx(handoff.clone()),
            &json!({
                "slug": "spec-doc",
                "title": "Basic spec doc",
                "body": body,
                "layer": "basic_spec",
            }),
        )
        .unwrap();
        let out: Value = serde_json::from_str(&saved).unwrap();
        let doc_id = out["doc_id"].as_str().unwrap().to_string();

        let doc = read_doc_hashed(&handoff, "spec-doc").unwrap().unwrap();
        let seq = doc
            .sections
            .iter()
            .find(|s| s.heading == "Section A")
            .unwrap()
            .seq;

        handle_doc_update_section(
            &ctx(handoff.clone()),
            &json!({
                "doc_id": doc_id,
                "seq": seq,
                "new_content": "## Section A\n\n### SPEC-002 New item\n\nDetails.\n",
            }),
        )
        .unwrap();

        let doc = read_doc_hashed(&handoff, "spec-doc").unwrap().unwrap();
        let v = doc
            .verification
            .expect("verification must exist after update_section sync");
        assert!(v
            .items
            .iter()
            .flat_map(|i| i.sub_items.iter())
            .any(|s| s.stable_id.as_deref() == Some("SPEC-002")));
    }

    /// `doc_verify(action="sync")` on a layer document delegates to the
    /// layer sync instead of the plain per-section rebuild. The body is
    /// hand-edited on disk (bypassing `doc_save`, so no sync has run yet) to
    /// add a new item and drop an old one *within the same section*: the
    /// plain per-section rebuild only adds/removes whole sections by seq, so
    /// it would neither pick up `SPEC-002` nor drop `SPEC-001` — only the
    /// layer sync does both.
    #[test]
    fn doc_verify_sync_on_layer_doc_delegates_to_layer_sync() {
        let (_tmp, handoff) = setup();
        let body = "# Basic spec\n\n### SPEC-001 Lockout\n\nBody.\n";
        let saved = handle_doc_save(
            &ctx(handoff.clone()),
            &json!({
                "slug": "spec-doc",
                "title": "Basic spec doc",
                "body": body,
                "layer": "basic_spec",
            }),
        )
        .unwrap();
        let out: Value = serde_json::from_str(&saved).unwrap();
        let doc_id = out["doc_id"].as_str().unwrap().to_string();

        // Direct `.md` edit: same single section, SPEC-001 replaced by SPEC-002.
        let edited = "# Basic spec\n\n### SPEC-002 Replacement\n\nBody.\n";
        crate::storage::docs::write_doc_body(&handoff, "spec-doc", edited).unwrap();

        let result = handle_doc_verify(
            &ctx(handoff.clone()),
            &json!({ "doc_id": doc_id, "action": "sync" }),
        )
        .unwrap();
        let out: Value = serde_json::from_str(&result).unwrap();
        assert!(
            out["warnings"].as_array().is_some_and(|w| w
                .iter()
                .any(|w| w.as_str().unwrap_or("").contains("SPEC-001"))),
            "layer sync must report SPEC-001 as removed: {out}"
        );

        let doc = read_doc_hashed(&handoff, "spec-doc").unwrap().unwrap();
        let v = doc.verification.unwrap();
        let ids: Vec<&str> = v
            .items
            .iter()
            .flat_map(|i| i.sub_items.iter())
            .filter_map(|s| s.stable_id.as_deref())
            .collect();
        assert_eq!(ids, vec!["SPEC-002"]);
    }

    /// §2.3 write guard: `add_item` on a layer document is refused with an
    /// error directing the caller to edit the body instead.
    #[test]
    fn add_item_on_layer_doc_is_refused() {
        let (_tmp, handoff) = setup();
        let body = "# Basic spec\n\n### SPEC-001 Lockout\n\nBody.\n";
        let saved = handle_doc_save(
            &ctx(handoff.clone()),
            &json!({
                "slug": "spec-doc",
                "title": "Basic spec doc",
                "body": body,
                "layer": "basic_spec",
            }),
        )
        .unwrap();
        let out: Value = serde_json::from_str(&saved).unwrap();
        let doc_id = out["doc_id"].as_str().unwrap().to_string();
        let doc = read_doc_hashed(&handoff, "spec-doc").unwrap().unwrap();
        let seq = doc.sections[0].seq;

        let err = handle_doc_verify(
            &ctx(handoff.clone()),
            &json!({
                "doc_id": doc_id, "action": "add_item",
                "fragment_seq": seq, "description": "hand-added",
            }),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("本文を編集"),
            "error must direct the caller to edit the body: {err}"
        );
    }

    /// §2.3 write guard: `set_priority` on a layer document is refused.
    #[test]
    fn set_priority_on_layer_doc_is_refused() {
        let (_tmp, handoff) = setup();
        let body = "# Basic spec\n\n### SPEC-001 Lockout\n\nBody.\n";
        let saved = handle_doc_save(
            &ctx(handoff.clone()),
            &json!({
                "slug": "spec-doc",
                "title": "Basic spec doc",
                "body": body,
                "layer": "basic_spec",
            }),
        )
        .unwrap();
        let out: Value = serde_json::from_str(&saved).unwrap();
        let doc_id = out["doc_id"].as_str().unwrap().to_string();

        let err = handle_doc_verify(
            &ctx(handoff.clone()),
            &json!({
                "doc_id": doc_id, "action": "set_priority",
                "sub_item_id": "SPEC-001", "priority": "P0",
            }),
        )
        .unwrap_err();
        assert!(err.to_string().contains("本文を編集"));
    }

    /// §2.3 write guard: `set_refs` with `test_refs` on a layer document is
    /// refused, but `impl_refs`-only is allowed (impl_refs is a runtime
    /// field, not body-owned).
    #[test]
    fn set_refs_with_test_refs_on_layer_doc_is_refused_but_impl_refs_only_is_allowed() {
        let (_tmp, handoff) = setup();
        let body = "# Basic spec\n\n### SPEC-001 Lockout\n\nBody.\n";
        let saved = handle_doc_save(
            &ctx(handoff.clone()),
            &json!({
                "slug": "spec-doc",
                "title": "Basic spec doc",
                "body": body,
                "layer": "basic_spec",
            }),
        )
        .unwrap();
        let out: Value = serde_json::from_str(&saved).unwrap();
        let doc_id = out["doc_id"].as_str().unwrap().to_string();

        let err = handle_doc_verify(
            &ctx(handoff.clone()),
            &json!({
                "doc_id": doc_id, "action": "set_refs",
                "sub_item_id": "SPEC-001", "test_refs": [{"path": "tests/x.rs"}],
            }),
        )
        .unwrap_err();
        assert!(err.to_string().contains("本文を編集"));

        // impl_refs-only must still succeed.
        let result = handle_doc_verify(
            &ctx(handoff.clone()),
            &json!({
                "doc_id": doc_id, "action": "set_refs",
                "sub_item_id": "SPEC-001", "impl_refs": [{"path": "src/x.rs"}],
            }),
        );
        assert!(
            result.is_ok(),
            "impl_refs-only set_refs must be allowed on a layer document: {result:?}"
        );
    }

    /// §2.3 write guard: `backfill_stable_ids` on a layer document is
    /// refused.
    #[test]
    fn backfill_stable_ids_on_layer_doc_is_refused() {
        let (_tmp, handoff) = setup();
        let body = "# Basic spec\n\n### SPEC-001 Lockout\n\nBody.\n";
        let saved = handle_doc_save(
            &ctx(handoff.clone()),
            &json!({
                "slug": "spec-doc",
                "title": "Basic spec doc",
                "body": body,
                "layer": "basic_spec",
            }),
        )
        .unwrap();
        let out: Value = serde_json::from_str(&saved).unwrap();
        let doc_id = out["doc_id"].as_str().unwrap().to_string();

        let err = handle_doc_verify(
            &ctx(handoff.clone()),
            &json!({ "doc_id": doc_id, "action": "backfill_stable_ids" }),
        )
        .unwrap_err();
        assert!(err.to_string().contains("本文を編集"));
    }

    /// Non-layer documents are completely unaffected by any of the above —
    /// existing `add_item`/`set_priority`/`set_refs`/`backfill_stable_ids`
    /// behavior is preserved (NFR-001/002).
    #[test]
    fn non_layer_doc_is_unaffected_by_write_guards() {
        let (_tmp, handoff) = setup();
        let body = "# Doc\n\n## Section 1\n\nBody one.\n";
        let saved = handle_doc_save(
            &ctx(handoff.clone()),
            &json!({ "slug": "plain-doc", "title": "Plain", "body": body }),
        )
        .unwrap();
        let out: Value = serde_json::from_str(&saved).unwrap();
        let doc_id = out["doc_id"].as_str().unwrap().to_string();

        handle_doc_verify(
            &ctx(handoff.clone()),
            &json!({ "doc_id": doc_id, "action": "generate" }),
        )
        .unwrap();

        let result = handle_doc_verify(
            &ctx(handoff.clone()),
            &json!({
                "doc_id": doc_id, "action": "add_item",
                "fragment_seq": 1, "description": "plain requirement",
            }),
        );
        assert!(
            result.is_ok(),
            "add_item on a non-layer document must be unaffected: {result:?}"
        );
    }

    /// Rework round 2 (MAJOR): wiki/220 §2.4 step 7's summary regeneration is
    /// `t360.6`'s own responsibility (only `rebuild_item_task_ids` moved to
    /// t360.7) — a layer `doc_save` that actually re-syncs the matrix must
    /// refresh `_requirements_summary.json` so the VSCode extension's
    /// Requirements Explorer never has to fall back to its own aggregation.
    #[test]
    fn doc_save_on_layer_doc_refreshes_requirements_summary() {
        let (_tmp, handoff) = setup();
        let body = "# Basic spec\n\n### SPEC-001 Lockout\n\n- priority: P1\n\nBody.\n";
        handle_doc_save(
            &ctx(handoff.clone()),
            &json!({
                "slug": "spec-doc",
                "title": "Basic spec doc",
                "body": body,
                "layer": "basic_spec",
            }),
        )
        .unwrap();

        let path = docs_dir(&handoff).join("_requirements_summary.json");
        assert!(
            path.exists(),
            "doc_save on a layer document must write _requirements_summary.json"
        );
        let parsed: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let items = parsed["items"].as_array().expect("items array");
        let spec_001 = items
            .iter()
            .find(|i| i["stable_id"] == "SPEC-001")
            .expect("SPEC-001 present in summary items");
        assert_eq!(spec_001["category"], "requirement");
        assert_eq!(spec_001["layer"], "basic_spec");
    }

    /// Same regression, via `doc_update_section` (§2.4's timing rule applies
    /// to both entry points identically).
    #[test]
    fn doc_update_section_on_layer_doc_refreshes_requirements_summary() {
        let (_tmp, handoff) = setup();
        let body = "# Basic spec\n\n## Section A\n\nOld content.\n";
        let saved = handle_doc_save(
            &ctx(handoff.clone()),
            &json!({
                "slug": "spec-doc",
                "title": "Basic spec doc",
                "body": body,
                "layer": "basic_spec",
            }),
        )
        .unwrap();
        let out: Value = serde_json::from_str(&saved).unwrap();
        let doc_id = out["doc_id"].as_str().unwrap().to_string();
        let doc = read_doc_hashed(&handoff, "spec-doc").unwrap().unwrap();
        let seq = doc
            .sections
            .iter()
            .find(|s| s.heading == "Section A")
            .unwrap()
            .seq;

        handle_doc_update_section(
            &ctx(handoff.clone()),
            &json!({
                "doc_id": doc_id,
                "seq": seq,
                "new_content": "## Section A\n\n### SPEC-002 New item\n\nDetails.\n",
            }),
        )
        .unwrap();

        let path = docs_dir(&handoff).join("_requirements_summary.json");
        assert!(
            path.exists(),
            "doc_update_section on a layer document must write _requirements_summary.json"
        );
        let parsed: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let items = parsed["items"].as_array().expect("items array");
        assert!(items.iter().any(|i| i["stable_id"] == "SPEC-002"));
    }

    /// wiki/220 §4.2 (FR-105) applied to layer sync: two layer documents
    /// whose bodies independently declare the same `stable_id` must warn on
    /// the second `doc_save` — silently creating an ambiguous id (unlinkable
    /// by `resolve_stable_ids`) with no explanation is the bug this guards.
    #[test]
    fn doc_save_layer_doc_cross_document_stable_id_collision_warns() {
        let (_tmp, handoff) = setup();
        handle_doc_save(
            &ctx(handoff.clone()),
            &json!({
                "slug": "spec-doc-a",
                "title": "Spec A",
                "body": "# Spec A\n\n### SPEC-001 First owner\n\nBody.\n",
                "layer": "basic_spec",
            }),
        )
        .unwrap();

        let saved_b = handle_doc_save(
            &ctx(handoff.clone()),
            &json!({
                "slug": "spec-doc-b",
                "title": "Spec B",
                "body": "# Spec B\n\n### SPEC-001 Second owner\n\nBody.\n",
                "layer": "basic_spec",
            }),
        )
        .unwrap();
        let out: Value = serde_json::from_str(&saved_b).unwrap();
        let warnings = out["warnings"].as_array().expect("warnings array");
        assert!(
            warnings
                .iter()
                .any(|w| w.as_str().unwrap_or("").contains("SPEC-001")
                    && w.as_str().unwrap_or("").contains("other document")),
            "saving a second layer document with a colliding stable_id must warn: {warnings:?}"
        );
    }

    /// wiki/220 §4.2 (FR-105) applied *within* a single document: a
    /// pre-existing `origin=None` (legacy) SubItem and a freshly synced
    /// `origin=body` SubItem can end up sharing a `stable_id` (e.g. a
    /// document `req_import`ed before `layer` was ever set, whose body later
    /// grows a heading that reuses the same id) — `collect_all_stable_ids`'s
    /// per-document dedup cannot see this, so it needs its own check.
    #[test]
    fn doc_save_layer_doc_legacy_and_body_stable_id_collision_within_document_warns() {
        let (_tmp, handoff) = setup();
        let body_v1 = "# Basic spec\n\n## Login\n\nOld content.\n";
        let saved = handle_doc_save(
            &ctx(handoff.clone()),
            &json!({
                "slug": "spec-doc",
                "title": "Basic spec doc",
                "body": body_v1,
                "layer": "basic_spec",
            }),
        )
        .unwrap();
        let out: Value = serde_json::from_str(&saved).unwrap();
        let doc_id = out["doc_id"].as_str().unwrap().to_string();

        // Inject a legacy (origin=None) SubItem under "Login" with the same
        // id the next body revision will (re)declare via a heading.
        {
            let mut doc = read_doc_hashed(&handoff, "spec-doc").unwrap().unwrap();
            let v = doc.verification.as_mut().unwrap();
            let item = v.items.iter_mut().find(|i| i.heading == "Login").unwrap();
            item.sub_items.push(SubItem {
                index: item.sub_items.len(),
                description: "hand-authored legacy item".to_string(),
                stable_id: Some("FR-001".to_string()),
                category: "requirement".to_string(),
                ..Default::default()
            });
            write_doc(&handoff, &doc).unwrap();
        }

        let body_v2 =
            "# Basic spec\n\n## Login\n\n### FR-001 Body item reusing the same id\n\nDetails.\n";
        let resynced = handle_doc_save(
            &ctx(handoff.clone()),
            &json!({ "doc_id": doc_id, "body": body_v2 }),
        )
        .unwrap();
        let out: Value = serde_json::from_str(&resynced).unwrap();
        let warnings = out["warnings"].as_array().expect("warnings array");
        assert!(
            warnings
                .iter()
                .any(|w| w.as_str().unwrap_or("").contains("FR-001")),
            "a legacy SubItem and a body item sharing a stable_id within the same document \
             must warn: {warnings:?}"
        );
    }

    /// Rework round 2 (MAJOR): `sync_layer_items_if_needed`'s short-circuit
    /// only compared the raw body hash — but `sync_layer_items`'s output
    /// also depends on `doc.layer` (it decides each item's `category`).
    /// Changing `layer` on a metadata-only `doc_save` (body byte-identical)
    /// must still force a re-sync, or the matrix silently keeps stale
    /// `category` values.
    #[test]
    fn doc_save_changing_layer_forces_resync_even_when_body_is_unchanged() {
        let (_tmp, handoff) = setup();
        let body = "# System test\n\n### ST-001 Lockout works\n\nSteps.\n";
        let saved = handle_doc_save(
            &ctx(handoff.clone()),
            &json!({
                "slug": "st-doc",
                "title": "System test doc",
                "body": body,
                "layer": "basic_spec",
            }),
        )
        .unwrap();
        let out: Value = serde_json::from_str(&saved).unwrap();
        let doc_id = out["doc_id"].as_str().unwrap().to_string();
        let before = read_doc_hashed(&handoff, "st-doc").unwrap().unwrap();
        let sub_before = before
            .verification
            .as_ref()
            .unwrap()
            .items
            .iter()
            .flat_map(|i| i.sub_items.iter())
            .find(|s| s.stable_id.as_deref() == Some("ST-001"))
            .unwrap();
        assert_eq!(sub_before.category, "requirement");

        // Metadata-only save (no body/append_body) that only changes `layer`
        // to a right-side layer — must flip category to "check" even though
        // the body bytes never changed.
        handle_doc_save(
            &ctx(handoff.clone()),
            &json!({ "doc_id": doc_id, "layer": "system_test" }),
        )
        .unwrap();

        let after = read_doc_hashed(&handoff, "st-doc").unwrap().unwrap();
        let sub_after = after
            .verification
            .as_ref()
            .unwrap()
            .items
            .iter()
            .flat_map(|i| i.sub_items.iter())
            .find(|s| s.stable_id.as_deref() == Some("ST-001"))
            .unwrap();
        assert_eq!(
            sub_after.category, "check",
            "changing layer on a metadata-only save must re-sync and flip category, even \
             though the body byte hash is unchanged"
        );
    }

    /// Same short-circuit gap, for `split_level`: changing it re-shapes
    /// `doc.sections` (and therefore layer sync's section-to-item mapping)
    /// without changing a single body byte.
    #[test]
    fn doc_save_changing_split_level_forces_resync_even_when_body_is_unchanged() {
        let (_tmp, handoff) = setup();
        let body =
            "# Basic spec\n\n## Login\n\n### SPEC-001 Lockout\n\nBody.\n\n## Session\n\n### SPEC-002 Timeout\n\nBody.\n";
        let saved = handle_doc_save(
            &ctx(handoff.clone()),
            &json!({
                "slug": "spec-doc",
                "title": "Basic spec doc",
                "body": body,
                "layer": "basic_spec",
                "split_level": 2,
            }),
        )
        .unwrap();
        let out: Value = serde_json::from_str(&saved).unwrap();
        let doc_id = out["doc_id"].as_str().unwrap().to_string();
        // At split_level=2, "## Login" and "## Session" each start their own
        // section, so SPEC-001/SPEC-002 land under two different items.
        let before = read_doc_hashed(&handoff, "spec-doc").unwrap().unwrap();
        let v_before = before.verification.unwrap();
        let heading_of = |v: &Verification, id: &str| -> String {
            v.items
                .iter()
                .find(|i| {
                    i.sub_items
                        .iter()
                        .any(|s| s.stable_id.as_deref() == Some(id))
                })
                .map(|i| i.heading.clone())
                .unwrap_or_else(|| panic!("{id} not found in any item"))
        };
        assert_ne!(
            heading_of(&v_before, "SPEC-001"),
            heading_of(&v_before, "SPEC-002"),
            "split_level=2 must place SPEC-001/SPEC-002 under different sections"
        );

        // Metadata-only save that only changes split_level to 1 (a single
        // top-level "# Basic spec" section covers the whole body) — must
        // re-sync so the matrix reflects the new section shape, even though
        // the body bytes never changed.
        handle_doc_save(
            &ctx(handoff.clone()),
            &json!({ "doc_id": doc_id, "split_level": 1 }),
        )
        .unwrap();

        let after = read_doc_hashed(&handoff, "spec-doc").unwrap().unwrap();
        let v_after = after.verification.unwrap();
        assert_eq!(
            heading_of(&v_after, "SPEC-001"),
            heading_of(&v_after, "SPEC-002"),
            "changing split_level on a metadata-only save must re-sync to the new (merged) \
             section shape, even though the body byte hash is unchanged"
        );
    }
}
