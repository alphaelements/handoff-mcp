//! E6's complete read-only trace-loading sequence (wiki/260-vmodel-m2-design.md
//! §2.1's E6 table, t360.20.8/M2-08): the three read-only counterparts to the
//! writes `handoff_trace_report`/`handoff_trace_slice` are allowed to make
//! before aggregating —
//!
//! 1. a layer document whose body was edited directly on disk (or whose
//!    sync-affecting config changed, E7) is re-synced **in memory only**: its
//!    `def_hash`/`refines`/`verifies`/etc. reflect today's body, but no new
//!    `link_baselines` entry is ever recorded (§2.5's closing paragraph: "メ
//!    モリ上だけの同期では、新しいベースラインを記録しない") and nothing is
//!    written to disk;
//! 2. the latest run-result cache comes from [`runs::load_latest_readonly`]
//!    (merges in any run file the on-disk `_latest.json` predates, but never
//!    writes it), not `runs::sync`;
//! 3. `SubItem.task_ids` is **not** self-repaired — the task-derived value is
//!    used in memory for this call's own `TraceInput`, and every stable_id
//!    whose stored value disagrees with it is reported as a `task_ids_drift`
//!    finding instead (`handoff_trace_lint`'s rule of the same name) rather
//!    than being silently rewritten (`rebuild_item_task_ids_full`, which
//!    writes both the document and `_task_ids_rebuild.json`, is the
//!    write-classified counterpart only `handoff_trace_report`/
//!    `handoff_trace_slice` call).
//!
//! Used by [`super::trace::load_trace_input_read_only`] (so every existing
//! caller — `handoff_trace_suspect`'s `list`/`baseline(dry_run)`,
//! `handoff_trace_impact` — gets E6 compliance for free) and directly by
//! `handoff_trace_lint` (`src/mcp/handlers/trace_lint.rs`, new), which also
//! needs this function's structured `unreadable`/`task_ids_drift`/
//! `id_like_headings` side-channels that the flattening wrapper only folds
//! into plain warning strings.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::Result;

use super::docs::{
    collect_doc_task_links, collect_requirement_task_links, plain_requirement_messages,
    sync_layer_items_local, unreadable_doc_warnings,
};
use super::trace::{load_trace_input_from_docs, LoadedTrace};
use crate::storage::docs::{read_doc_body, DocSet, UnreadableDoc};
use crate::storage::runs;
use crate::trace::lint::TaskIdsDrift;

/// Full result of [`load_trace_input_fully_read_only`].
pub(super) struct ReadOnlyTraceLoad {
    pub(super) loaded: LoadedTrace,
    /// Every non-fatal notice this load produced, in flat string form — the
    /// same shape every other `trace.rs` warnings array uses. Includes the
    /// `unreadable` documents' messages (already also available structured,
    /// below) and the in-memory resync's own warnings (duplicate ids,
    /// removed items, ID-like headings, empty waiver reasons, unlabeled
    /// acceptance bullets, ...).
    pub(super) warnings: Vec<String>,
    pub(super) unreadable: Vec<UnreadableDoc>,
    pub(super) task_ids_drift: Vec<TaskIdsDrift>,
    /// Every in-memory resync warning, attributed to the document slug it
    /// came from — `handoff_trace_lint`'s `id_like_heading`/
    /// `unlabeled_acceptance`/`invalid_waiver`/`attribute_after_body` rules
    /// (wiki/260 §4.3, the last one M3/t377.5) each pattern-match this list
    /// for their own `ParseWarningKind`'s rendered text
    /// (`src/storage/docs/layer_parse.rs`'s `Display` impl) rather than this
    /// module re-deriving a parallel structured type for every parser
    /// warning kind a lint rule might someday want.
    pub(super) per_doc_sync_warnings: Vec<(String, String)>,
    /// Document slugs whose in-memory resync actually ran this call —
    /// `handoff_trace_lint`'s `unsynced_body` rule input (FR-801).
    pub(super) resynced_doc_slugs: HashSet<String>,
}

/// De-duplicates `items` in place, keeping each value's first occurrence
/// position (M2-08 rework, reviewer round 1 MAJOR finding). Both
/// `load_trace_input_read_only` (`src/mcp/handlers/trace.rs`)'s fold of
/// `read_only.warnings` into `LoadedTrace.config_warnings`, and
/// `handoff_trace_lint`'s own merge of `read_only.warnings` with
/// `read_only.loaded.config_warnings`, need this: a layer-registry warning
/// (e.g. a duplicate `[[trace.layer]]` id) is already present once in
/// `config_warnings` (`load_trace_input_from_docs`'s own
/// `layer_registry.warnings`), but gets re-emitted into `warnings` once per
/// in-memory-resynced layer document (`sync_layer_items_local`, `docs.rs`,
/// rebuilds its own `LayerRegistry` and extends its own warnings with it) —
/// without this, a project with N layer documents and one misconfigured
/// layer declaration would report that one warning N+1 times.
pub(super) fn dedup_preserve_order(items: &mut Vec<String>) {
    let mut seen = HashSet::new();
    items.retain(|w| seen.insert(w.clone()));
}

/// E6's full read-only load (see this module's doc comment). Never writes
/// anything — no `DocSet::flush`, no `runs::sync`, no
/// `rebuild_item_task_ids_full` — by construction: every document mutation
/// below only ever touches the in-memory `DocSet` this function itself drops
/// at the end, and the two owning-system reads it does (`runs::
/// load_latest_readonly`, `collect_requirement_task_links`) are themselves
/// read-only.
pub(super) fn load_trace_input_fully_read_only(
    handoff: &Path,
    layers_arg: Vec<String>,
) -> Result<ReadOnlyTraceLoad> {
    let mut warnings: Vec<String> = Vec::new();
    let mut per_doc_sync_warnings: Vec<(String, String)> = Vec::new();
    let mut resynced_doc_slugs: HashSet<String> = HashSet::new();

    let mut doc_set = DocSet::load(handoff)?;
    let unreadable = doc_set.unreadable().to_vec();
    warnings.extend(unreadable_doc_warnings(&unreadable));

    // E6 (1): memory-only resync of every directly-edited (or
    // sync-affecting-config-changed, E7) layer document.
    let layer_doc_ids: Vec<(String, String)> = doc_set
        .docs()
        .iter()
        .filter(|d| d.layer.is_some())
        .map(|d| (d.id.clone(), d.slug.clone()))
        .collect();
    if !layer_doc_ids.is_empty() {
        let now = chrono::Utc::now().to_rfc3339();
        for (doc_id, slug) in &layer_doc_ids {
            let Some(body) = read_doc_body(handoff, slug)? else {
                continue;
            };
            // §2.5's closing paragraph: a memory-only sync must never record
            // a *new* link baseline — only delete one whose reference no
            // longer exists (already `sync_layer_items_local`'s own
            // behavior, since that part is a pure consequence of the current
            // refines/verifies list, not something this function needs to
            // undo). Snapshot each SubItem's `link_baselines` *keys* before
            // the sync so any key the sync adds (a same-document reference
            // it could resolve locally) can be reverted after — a
            // newly-inserted key is, by construction, one that was not
            // already in this snapshot (an existing baseline's key is never
            // touched by a resync, only ever retained or dropped, see
            // `sync_layer_items_with_options`'s doc comment).
            let before_keys: HashMap<String, HashSet<String>> = doc_set
                .get(doc_id)
                .and_then(|d| d.verification.as_ref())
                .map(|v| {
                    v.items
                        .iter()
                        .flat_map(|i| &i.sub_items)
                        .filter_map(|s| {
                            s.stable_id
                                .clone()
                                .map(|id| (id, s.link_baselines.keys().cloned().collect()))
                        })
                        .collect()
                })
                .unwrap_or_default();

            let Some(doc) = doc_set.get_mut(doc_id) else {
                continue;
            };
            let mut sync_warnings = Vec::new();
            let local =
                sync_layer_items_local(handoff, doc, &body, &now, false, &mut sync_warnings);
            let synced = local.is_some();
            if let Some(local) = &local {
                sync_warnings.extend(plain_requirement_messages(&local.requirement_warnings));
            }
            if synced {
                resynced_doc_slugs.insert(slug.clone());
                if let Some(v) = doc.verification.as_mut() {
                    for item in v.items.iter_mut() {
                        for sub in item.sub_items.iter_mut() {
                            let Some(id) = sub.stable_id.clone() else {
                                continue;
                            };
                            let before = before_keys.get(&id).cloned().unwrap_or_default();
                            sub.link_baselines.retain(|k, _| before.contains(k));
                        }
                    }
                }
            }
            for w in &sync_warnings {
                per_doc_sync_warnings.push((slug.clone(), w.clone()));
            }
            warnings.extend(sync_warnings);
            // Deliberately never `doc_set.mark_dirty`/`flush` — E6's whole
            // point is that none of this ever reaches disk.
        }
    }

    // E6 (3): `SubItem.task_ids` resolved from the task side (the
    // authority, D3) rather than self-repaired on disk — any disagreement is
    // reported as drift, not silently corrected.
    let mut derived_task_ids: HashMap<String, std::collections::BTreeSet<String>> = HashMap::new();
    collect_requirement_task_links(&handoff.join("tasks"), &mut derived_task_ids)?;
    let mut task_ids_drift: Vec<TaskIdsDrift> = Vec::new();
    let doc_ids: Vec<String> = doc_set.docs().iter().map(|d| d.id.clone()).collect();
    for doc_id in &doc_ids {
        let Some(doc) = doc_set.get_mut(doc_id) else {
            continue;
        };
        let Some(v) = doc.verification.as_mut() else {
            continue;
        };
        for item in v.items.iter_mut() {
            for sub in item.sub_items.iter_mut() {
                let Some(id) = sub.stable_id.clone() else {
                    continue;
                };
                let derived: Vec<String> = derived_task_ids
                    .get(&id)
                    .map(|s| s.iter().cloned().collect())
                    .unwrap_or_default();
                let mut stored_sorted = sub.task_ids.clone();
                stored_sorted.sort();
                stored_sorted.dedup();
                if stored_sorted != derived {
                    task_ids_drift.push(TaskIdsDrift::item(id, stored_sorted, derived.clone()));
                    // Used in memory for this call's own TraceInput/graph —
                    // never written back (E6).
                    sub.task_ids = derived;
                }
            }
        }
    }

    // M2-15 (wiki/260 §4.8/FR-601): document-level `task_ids` — like the
    // item-level pass above, resolved from the task side's `TaskLink{doc}`
    // entries (the authority) rather than self-repaired on disk, but
    // **append-only** in both directions here: an id present in
    // `derived` that `doc.task_ids` is missing is added to this call's
    // in-memory view (never written, E6), while an id present in
    // `doc.task_ids` with no matching `TaskLink{doc}` is *kept* (never
    // dropped — §4.8: "文書単位の自己修復は追加だけ") and reported as drift
    // instead, exactly mirroring [`crate::mcp::handlers::docs::rebuild_item_task_ids_full`]'s
    // on-disk counterpart.
    let mut derived_doc_task_ids: HashMap<String, std::collections::BTreeSet<String>> =
        HashMap::new();
    collect_doc_task_links(&handoff.join("tasks"), &mut derived_doc_task_ids)?;
    for doc_id in &doc_ids {
        let Some(doc) = doc_set.get_mut(doc_id) else {
            continue;
        };
        // A document no task declares a `TaskLink{doc}` for at all derives
        // to the empty set — it must still be compared (not skipped), or a
        // document whose *only* `task_ids` entries are orphans (an id M1's
        // `doc_save` could not resolve, or every task-side link removed by
        // hand — §4.8's two named cases) would never be reported.
        let derived: Vec<String> = derived_doc_task_ids
            .get(&doc.id)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();
        let mut stored_sorted = doc.task_ids.clone();
        stored_sorted.sort();
        stored_sorted.dedup();
        if stored_sorted != derived {
            task_ids_drift.push(TaskIdsDrift::doc(
                doc.slug.clone(),
                stored_sorted.clone(),
                derived.clone(),
            ));
            let mut union_sorted = stored_sorted;
            for id in &derived {
                if let Err(pos) = union_sorted.binary_search(id) {
                    union_sorted.insert(pos, id.clone());
                }
            }
            doc.task_ids = union_sorted;
        }
    }

    let docs = doc_set.docs().to_vec();
    let latest_cache = runs::load_latest_readonly(handoff)?;
    let loaded = load_trace_input_from_docs(handoff, layers_arg, docs, latest_cache)?;

    Ok(ReadOnlyTraceLoad {
        loaded,
        warnings,
        unreadable,
        task_ids_drift,
        per_doc_sync_warnings,
        resynced_doc_slugs,
    })
}
