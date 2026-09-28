//! Layer body -> verification matrix synchronization
//! (wiki/220-vmodel-integration-design.md §2.4, M1 t360.6): turns
//! [`parse_layer_body`](super::layer_parse::parse_layer_body)'s (t360.5)
//! pure parse result into a rebuilt [`Verification`] matrix, preserving
//! every runtime field a prior sync or manual `doc_verify` action wrote,
//! across heading moves and section insertions — `fragment_seq`/`index` are
//! never used as the join key across a re-sync (§2.4: "seq と位置添字" must
//! survive a section insertion that shifts every later `seq`).
//!
//! `rebuild_item_task_ids` (§2.5's differential `task_ids` recompute, step 7)
//! is **not** run here — it is t360.7's concern. This module leaves every
//! retained `SubItem.task_ids` value byte-for-byte as it was before the
//! sync, so a caller that wires in t360.7's differential apply afterwards
//! (see [`sync_layer_items`]'s doc comment) has a stable, already-rebuilt
//! matrix to apply it to.

use std::collections::{HashMap, HashSet};

use super::layer::{LayerRegistry, LayerSide};
use super::layer_parse::{default_prefix_table, parse_layer_body};
use super::model::{
    AcRef, CodeRef, DocMetadata, SectionIndex, SubItem, Verification, VerificationItem,
};

/// Label of the freeform item that collects `origin=None` (legacy) SubItems
/// whose containing section heading no longer exists in the body (§2.4 step
/// 4).
const ORPHAN_LABEL: &str = "(orphaned legacy items)";

/// Outcome of one [`sync_layer_items`] call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LayerSyncOutcome {
    /// Human-readable warnings: parser warnings (ID-like heading ignored,
    /// duplicate id), `removed: [ids]` (§2.4 step 6), and orphaned-legacy
    /// notices (§2.4 step 4), in that order.
    pub warnings: Vec<String>,
    /// stable_ids of `origin=body` items that existed before this call and
    /// no longer appear in the body (§2.4 step 6). Runs and task links for a
    /// removed id are left untouched elsewhere — only this `SubItem` entry
    /// disappears.
    pub removed: Vec<String>,
    /// For every id in [`removed`](Self::removed), the `task_ids` its
    /// `SubItem` carried immediately before being dropped (empty `Vec` if it
    /// had none — every removed id gets an entry here, never a missing key).
    /// Rework round 2 (MAJOR fix, wiki/220 §2.5, D3): this does **not**
    /// drive any task-file write — `sync_layer_items_if_needed` only folds
    /// it into an informational warning. The task side is the authority; a
    /// task that still lists a removed id in its own `task_links` stays
    /// linked (a dangling gap surfaced by `trace_report`/`trace_slice`,
    /// removable via `update_task(requirement_ids)`) so that moving a
    /// requirement to another document, or undoing its removal, does not
    /// silently drop the task's link to it.
    pub removed_task_ids: HashMap<String, Vec<String>>,
    /// t360.41 (M-S12 reviewer follow-up to §2.5's `removed`/`removed_task_ids`
    /// pair): stable_ids of `origin=body` items that appear in this sync's
    /// result but were **not** already present as an `origin=body` item
    /// before this call — i.e. every id whose `SubItem` starts this call with
    /// a fresh, empty `task_ids` (`body_owned.remove(id)` missed) because it
    /// is either genuinely new, or *reappearing* (moved back from another
    /// document, or an undone removal). `sync_layer_items` itself has no
    /// memory of which case it is — see [`sync_layer_items_if_needed`]'s doc
    /// comment for how the caller uses this to restore a reappearing item's
    /// `task_ids` from the task side without waiting for the next task
    /// mutation.
    pub added: Vec<String>,
    /// M2 (wiki/260-vmodel-m2-design.md §2.5 step 5, M2-02): stable_ids of
    /// every `origin=body` item (parsed or implicit) whose `def_hash`
    /// differs from what it was immediately before this sync — the input
    /// `suspect_introduced` (§4.11, M2-06) uses to know which downstream
    /// links might now be suspect. A brand-new id (no prior `def_hash` to
    /// compare against) counts as changed. Sorted for determinism (NFR-004).
    pub def_changed: Vec<String>,
    /// `false` when `doc.layer` is unset: `sync_layer_items` is a no-op for
    /// non-layer documents (§5, NFR-001/002) and `doc.verification` is left
    /// completely untouched.
    pub synced: bool,
}

/// Rebuilds `doc.verification` from `body` (§2.4 steps 1-6). No-op (returns
/// `synced: false`, `doc.verification` untouched) when `doc.layer` is
/// `None`.
///
/// Preconditions (the same ones `doc_save`/`doc_update_section` already
/// satisfy before calling this): `doc.sections` reflects `body` (freshly
/// `split()` + `compute_sections()`), and `body` is the document's full
/// post-frontmatter-strip content.
///
/// `registry` is the project's [`LayerRegistry`] (built-ins + valid
/// `[[trace.layer]]` declarations, wiki/260 §2.1, M2-01). `config_id_prefixes`
/// is `Config.trace.id_prefixes` (wiki/220 §2.1). `now` is an RFC3339
/// timestamp supplied by the caller (keeps this module clock-free and
/// deterministically testable, mirroring `DocMetadata::new`).
///
/// Equivalent to [`sync_layer_items_with_options`] with
/// `implicit_acceptance: false` (no implicit acceptance-verification items
/// are materialized) — kept as a distinct, stable-signature entry point so
/// every existing caller (and this module's own M1/M2-01-era tests) keeps
/// compiling and behaving unchanged while the profile-aware caller wiring
/// (resolving the document's effective `implicit_acceptance`, wiki/260 §2.1/
/// §2.5 step 3) is done elsewhere (`sync_layer_items_if_needed`,
/// `src/mcp/handlers/docs.rs`).
pub fn sync_layer_items(
    doc: &mut DocMetadata,
    body: &str,
    registry: &LayerRegistry,
    config_id_prefixes: &HashMap<String, Vec<String>>,
    now: &str,
) -> LayerSyncOutcome {
    sync_layer_items_with_options(doc, body, registry, config_id_prefixes, now, false)
}

/// Resolves the `implicit_acceptance` boolean [`sync_layer_items_with_options`]
/// needs for `doc` (wiki/260-vmodel-m2-design.md §2.1/§2.5 手順 3: "暗黙の受入
/// 検証項目の実体化は、同期時に**その項目の文書の** `trace_profile`（なければ
/// プロジェクト既定）の `implicit_acceptance` で決める。層同期はグラフを作ら
/// ないので、ツリーの継承は使わない") — a flat, single-document lookup, not
/// the refines/verifies-tree inheritance M2-03 adds to the engine for
/// per-item effective profiles elsewhere.
///
/// Priority: `doc.trace_profile` (per-document override, §2.1) when set and
/// non-empty, else the project default profile (`[trace] profile`). `false`
/// when neither resolves (unknown profile name, or neither configured) —
/// matches `sync_layer_items`'s (this module's pre-M2-02 entry point)
/// always-off behavior when there is nothing to resolve.
pub fn resolve_doc_implicit_acceptance(
    doc: &DocMetadata,
    trace_config: &crate::storage::config::TraceConfig,
    registry: &LayerRegistry,
) -> (bool, Vec<String>) {
    if let Some(name) = doc.trace_profile.as_deref().filter(|s| !s.is_empty()) {
        let (resolved, warnings) =
            crate::trace::profile::resolve_profile_by_name(name, trace_config, registry);
        return (
            resolved.map(|p| p.implicit_acceptance).unwrap_or(false),
            warnings,
        );
    }
    let (resolved, warnings) =
        crate::trace::profile::resolve_project_profile(trace_config, registry);
    (
        resolved.map(|p| p.implicit_acceptance).unwrap_or(false),
        warnings,
    )
}

/// Full M2 layer sync (wiki/260-vmodel-m2-design.md §2.2-§2.5, M2-02): parses
/// the M2 body notation (acceptance-criteria block, extended attributes,
/// `def_hash`/`ac_hash`) via [`parse_layer_body`] and rebuilds `doc.verification`
/// exactly like [`sync_layer_items`], plus (§2.5 step 3) materializes one
/// implicit acceptance-verification `SubItem` per parsed item's
/// acceptance-criteria bullet when `implicit_acceptance` is `true` — the
/// caller resolves that boolean from the document's effective profile
/// (`trace_profile` override, else the project default) before calling this.
///
/// Recording new [`SubItem::link_baselines`] entries (§2.5 step 4) is **not**
/// this function's job (M2-04) — like every other runtime field, an existing
/// baseline is preserved (via the same `body_owned.remove(id)` restore this
/// module has always used for `task_ids`/`dev_stage`/etc.), never freshly
/// written here.
pub fn sync_layer_items_with_options(
    doc: &mut DocMetadata,
    body: &str,
    registry: &LayerRegistry,
    config_id_prefixes: &HashMap<String, Vec<String>>,
    now: &str,
    implicit_acceptance: bool,
) -> LayerSyncOutcome {
    let Some(doc_layer) = doc.layer.clone() else {
        return LayerSyncOutcome {
            synced: false,
            ..Default::default()
        };
    };

    let prefix_table = default_prefix_table(registry, config_id_prefixes);
    let parsed = parse_layer_body(body, Some(&doc_layer), &prefix_table);
    let mut warnings: Vec<String> = parsed.warnings.iter().map(|w| w.to_string()).collect();

    // Step 1: snapshot runtime state from the existing matrix before it is
    // discarded. Freeform items (fragment_seq=None) are carried over as-is —
    // layer sync only ever rebuilds the section-tied item list.
    let mut body_owned: HashMap<String, SubItem> = HashMap::new();
    let mut legacy_by_heading: HashMap<String, Vec<SubItem>> = HashMap::new();
    let mut runtime_by_heading: HashMap<String, ItemRuntime> = HashMap::new();
    let mut freeform_items: Vec<VerificationItem> = Vec::new();
    let created_at = doc
        .verification
        .as_ref()
        .map(|v| v.created_at.clone())
        .unwrap_or_else(|| now.to_string());

    if let Some(v) = &doc.verification {
        for item in &v.items {
            if item.fragment_seq.is_none() {
                freeform_items.push(item.clone());
                continue;
            }
            runtime_by_heading.insert(item.heading.clone(), ItemRuntime::capture(item));
            for sub in &item.sub_items {
                if sub.origin.as_deref() == Some("body") {
                    if let Some(id) = sub.stable_id.clone() {
                        body_owned.insert(id, sub.clone());
                    }
                } else {
                    legacy_by_heading
                        .entry(item.heading.clone())
                        .or_default()
                        .push(sub.clone());
                }
            }
        }
    }

    // Step 2: fresh VerificationItem per current section (heading-keyed
    // runtime restore) + step 4 (legacy SubItem restore by heading match).
    let mut new_items: Vec<VerificationItem> = doc
        .sections
        .iter()
        .map(|s| {
            let mut item = VerificationItem {
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
            };
            if let Some(rt) = runtime_by_heading.get(&s.heading) {
                rt.apply(&mut item);
            }
            if let Some(legacy) = legacy_by_heading.remove(&s.heading) {
                item.sub_items = legacy;
            }
            item
        })
        .collect();

    // Step 3: place each parsed body item as a SubItem of the section whose
    // byte range contains its heading line, restoring runtime fields
    // (dev_stage, status, reviewer, verified_at, notes, impl_refs) by
    // stable_id.
    // t360.41: snapshot which ids were already `origin=body`-owned *before*
    // this loop consumes `body_owned` — the difference between this and
    // `seen_ids` below is exactly "appeared in this sync's result but wasn't
    // already here", i.e. [`LayerSyncOutcome::added`].
    let body_owned_before: HashSet<String> = body_owned.keys().cloned().collect();
    let line_starts = line_byte_offsets(body);
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut def_changed: Vec<String> = Vec::new();
    for parsed_item in &parsed.items {
        seen_ids.insert(parsed_item.id.clone());
        let start_byte = line_starts
            .get(parsed_item.start_line.saturating_sub(1))
            .copied()
            .unwrap_or(0);
        let Some(target) = section_index_for_byte(&doc.sections, start_byte) else {
            // Defensive only: every heading in `body` falls inside some
            // section by construction of `split()`.
            continue;
        };

        let mut sub = body_owned.remove(&parsed_item.id).unwrap_or_default();
        let old_def_hash = sub.def_hash.clone();
        sub.description = parsed_item.title.clone();
        sub.origin = Some("body".to_string());
        sub.stable_id = Some(parsed_item.id.clone());
        sub.layer = parsed_item.attrs.layer.clone();
        sub.refines = parsed_item.attrs.refines.clone();
        sub.verifies = parsed_item.attrs.verifies.clone();
        sub.method = parsed_item.attrs.method.clone();
        sub.priority = parsed_item.attrs.priority.clone();
        sub.test_refs = parsed_item
            .attrs
            .test_refs
            .iter()
            .map(|t| CodeRef {
                path: t.clone(),
                lines: None,
                label: None,
            })
            .collect();
        sub.body_hash = Some(parsed_item.body_hash.clone());
        // M2 (wiki/260 §2.2/§2.3/§2.4, M2-02): the extended body-notation
        // fields, re-derived from the body every sync just like the M1
        // fields above. `link_baselines`/`implicit_of` are deliberately
        // left as whatever `body_owned.remove` already restored — this
        // function never writes them for an ordinary (non-implicit) item.
        sub.def_hash = Some(parsed_item.def_hash.clone());
        sub.acceptance = parsed_item.acceptance.iter().map(AcRef::from).collect();
        sub.rationale = parsed_item.ext_attrs.rationale.clone();
        sub.derived = parsed_item.ext_attrs.derived.clone();
        sub.waivers = parsed_item.ext_attrs.waivers.clone();
        sub.from = parsed_item.ext_attrs.from.clone();
        sub.reserved_attrs = parsed_item.ext_attrs.reserved.clone();

        let effective_layer = parsed_item.effective_layer.as_deref();
        if let Some(l) = effective_layer {
            if registry.get(l).is_none() {
                warnings.push(format!(
                    "item {}: unknown layer \"{l}\", treated as layer-less for aggregation",
                    parsed_item.id
                ));
            }
        }
        sub.category = category_for_effective_layer(registry, effective_layer);
        if old_def_hash.as_deref() != Some(parsed_item.def_hash.as_str()) {
            def_changed.push(parsed_item.id.clone());
        }

        new_items[target].sub_items.push(sub);

        // Step 3 (§2.5): materialize one implicit acceptance-verification
        // SubItem per acceptance-criteria bullet, right after the parent —
        // only when this document's effective profile has
        // `implicit_acceptance` enabled and the parent's effective layer is
        // a known, paired layer (an unknown layer already warned above; no
        // well-defined pair to place the implicit item on).
        if implicit_acceptance {
            if let Some(pair_id) = effective_layer
                .and_then(|l| registry.get(l))
                .map(|d| d.pair.clone())
            {
                for ac in &parsed_item.acceptance {
                    let implicit_id = format!("{}#{}", parsed_item.id, ac.label);
                    seen_ids.insert(implicit_id.clone());
                    let mut implicit_sub = body_owned.remove(&implicit_id).unwrap_or_default();
                    let old_implicit_def_hash = implicit_sub.def_hash.clone();
                    implicit_sub.description = ac.text.clone();
                    implicit_sub.origin = Some("body".to_string());
                    implicit_sub.stable_id = Some(implicit_id.clone());
                    implicit_sub.layer = Some(pair_id.clone());
                    implicit_sub.refines = Vec::new();
                    implicit_sub.verifies = vec![implicit_id.clone()];
                    implicit_sub.method = None;
                    implicit_sub.priority = None;
                    implicit_sub.test_refs = Vec::new();
                    // An implicit item has no body of its own beyond the
                    // parent's acceptance bullet — `body_hash` (the M1 key
                    // set) has no meaning for it; `def_hash` mirrors
                    // `ac_hash(parent, label)` exactly (§2.4), so a suspect
                    // check on this implicit item's own definition tracks
                    // the same upstream text as the AC-unit link it exists
                    // to verify.
                    implicit_sub.body_hash = None;
                    implicit_sub.def_hash = Some(ac.ac_hash.clone());
                    implicit_sub.acceptance = Vec::new();
                    implicit_sub.implicit_of = Some(parsed_item.id.clone());
                    implicit_sub.category = category_for_effective_layer(registry, Some(&pair_id));
                    if old_implicit_def_hash.as_deref() != Some(ac.ac_hash.as_str()) {
                        def_changed.push(implicit_id.clone());
                    }

                    new_items[target].sub_items.push(implicit_sub);
                }
            }
        }
    }

    // Step 6: origin=body items that existed before and are no longer
    // parsed out of the current body are dropped. Their `task_ids` are
    // captured (not just their ids) before the `SubItem` itself is
    // discarded — see `LayerSyncOutcome::removed_task_ids`.
    let removed_task_ids: HashMap<String, Vec<String>> = body_owned
        .iter()
        .map(|(id, sub)| (id.clone(), sub.task_ids.clone()))
        .collect();
    let mut removed: Vec<String> = body_owned.into_keys().collect();
    removed.sort();
    if !removed.is_empty() {
        warnings.push(format!("removed: [{}]", removed.join(", ")));
    }

    let mut added: Vec<String> = seen_ids.difference(&body_owned_before).cloned().collect();
    added.sort();

    // Remainder of step 4: headings whose section disappeared entirely move
    // their legacy SubItems to the orphan freeform item.
    let mut orphan_headings: Vec<String> = legacy_by_heading.keys().cloned().collect();
    orphan_headings.sort();
    let mut orphan_subs: Vec<SubItem> = Vec::new();
    for heading in &orphan_headings {
        if let Some(subs) = legacy_by_heading.get(heading) {
            warnings.push(format!(
                "section \"{heading}\" no longer exists; moved {} legacy sub-item(s) to \"{ORPHAN_LABEL}\"",
                subs.len()
            ));
            orphan_subs.extend(subs.iter().cloned());
        }
    }
    if !orphan_subs.is_empty() {
        match freeform_items
            .iter_mut()
            .find(|i| i.label.as_deref() == Some(ORPHAN_LABEL))
        {
            Some(existing) => existing.sub_items.extend(orphan_subs),
            None => freeform_items.push(VerificationItem {
                fragment_seq: None,
                heading: ORPHAN_LABEL.to_string(),
                status: "pending".to_string(),
                impl_refs: Vec::new(),
                test_refs: Vec::new(),
                reviewer: None,
                verified_at: None,
                notes: String::new(),
                content_hash_at_verify: None,
                category: "requirement".to_string(),
                sub_items: orphan_subs,
                label: Some(ORPHAN_LABEL.to_string()),
            }),
        }
    }

    // Step 5: reindex every item's sub_items — `index == array position` is
    // the invariant `propagate_dev_stage_for_task`'s stable_id-based
    // resolution and the VSCode writer's `s.index === subItemIndex` rely on.
    for item in new_items.iter_mut().chain(freeform_items.iter_mut()) {
        for (i, sub) in item.sub_items.iter_mut().enumerate() {
            sub.index = i;
        }
    }

    new_items.extend(freeform_items);
    let status = overall_status(&new_items);
    doc.verification = Some(Verification {
        status,
        created_at,
        updated_at: now.to_string(),
        items: new_items,
    });

    def_changed.sort();
    def_changed.dedup();

    LayerSyncOutcome {
        warnings,
        removed,
        removed_task_ids,
        added,
        def_changed,
        synced: true,
    }
}

/// `effective_layer`'s `side` determines the derived `SubItem.category`
/// (§2.3): `right` -> `"check"` (verification items, excluded from
/// `aggregate_requirements`'s requirement counts), `left` or unknown/no
/// layer -> `"requirement"`.
fn category_for_effective_layer(registry: &LayerRegistry, layer: Option<&str>) -> String {
    match layer.and_then(|l| registry.get(l)) {
        Some(def) if matches!(def.side, LayerSide::Right) => "check".to_string(),
        _ => "requirement".to_string(),
    }
}

/// `offsets[n]` = byte offset where line `n+1` (1-based) starts.
fn line_byte_offsets(body: &str) -> Vec<usize> {
    let mut offsets = vec![0usize];
    for (i, b) in body.bytes().enumerate() {
        if b == b'\n' {
            offsets.push(i + 1);
        }
    }
    offsets
}

/// The index into `sections` (parallel to `doc.sections`, which
/// [`sync_layer_items`]'s `new_items` is built from 1:1) whose byte range
/// contains `byte`. Falls back to the last section when `byte` lands exactly
/// at the end of the body (no trailing newline) and no section's half-open
/// range technically contains it.
fn section_index_for_byte(sections: &[SectionIndex], byte: usize) -> Option<usize> {
    sections
        .iter()
        .position(|s| byte >= s.byte_offset && byte < s.byte_offset + s.byte_length)
        .or(if sections.is_empty() {
            None
        } else {
            Some(sections.len() - 1)
        })
}

/// Runtime fields captured from an existing `VerificationItem` (§2.4 step 2:
/// "status, reviewer, verified_at, notes, content_hash_at_verify, impl_refs,
/// test_refs"), keyed by heading and restored onto the freshly rebuilt item
/// with the same heading.
struct ItemRuntime {
    status: String,
    reviewer: Option<String>,
    verified_at: Option<String>,
    notes: String,
    content_hash_at_verify: Option<String>,
    impl_refs: Vec<CodeRef>,
    test_refs: Vec<CodeRef>,
}

impl ItemRuntime {
    fn capture(item: &VerificationItem) -> Self {
        ItemRuntime {
            status: item.status.clone(),
            reviewer: item.reviewer.clone(),
            verified_at: item.verified_at.clone(),
            notes: item.notes.clone(),
            content_hash_at_verify: item.content_hash_at_verify.clone(),
            impl_refs: item.impl_refs.clone(),
            test_refs: item.test_refs.clone(),
        }
    }

    fn apply(&self, item: &mut VerificationItem) {
        item.status = self.status.clone();
        item.reviewer = self.reviewer.clone();
        item.verified_at = self.verified_at.clone();
        item.notes = self.notes.clone();
        item.content_hash_at_verify = self.content_hash_at_verify.clone();
        item.impl_refs = self.impl_refs.clone();
        item.test_refs = self.test_refs.clone();
    }
}

/// Mirrors `crate::mcp::handlers::docs::recompute_verification_status` /
/// `item_effective_status` exactly (kept as a local copy rather than a
/// cross-dependency from `storage` back into `mcp::handlers`, since this
/// module must stay a pure, handler-independent transform per its module
/// doc).
fn overall_status(items: &[VerificationItem]) -> String {
    let statuses: Vec<String> = items.iter().map(item_effective_status).collect();
    if statuses.iter().all(|s| s == "pending") {
        "pending".to_string()
    } else if statuses.iter().all(|s| s == "verified" || s == "skipped") {
        "verified".to_string()
    } else {
        "in_review".to_string()
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::docs::model::DocMetadata;

    fn layer_doc(layer: &str, body: &str, split_level: u8) -> DocMetadata {
        let mut doc = DocMetadata::new(
            "doc-1".to_string(),
            "layer-doc".to_string(),
            "Layer doc".to_string(),
            "spec".to_string(),
            "2026-09-27T00:00:00Z".to_string(),
        );
        doc.layer = Some(layer.to_string());
        doc.split_level = split_level;
        let split_doc = super::super::split::split(body, split_level).unwrap();
        doc.sections = super::super::split::compute_sections(&split_doc, false);
        doc
    }

    fn prefixes() -> HashMap<String, Vec<String>> {
        HashMap::new()
    }

    fn registry() -> LayerRegistry {
        LayerRegistry::build(&[])
    }

    /// §2.4 steps 1-3/5: a fresh sync on a doc with no prior matrix creates
    /// one origin=body SubItem per body heading item, under the section that
    /// contains it, with index reassigned from 0.
    #[test]
    fn first_sync_creates_body_owned_sub_items_under_their_section() {
        let body =
            "# Basic spec\n\n## Login\n\n### SPEC-012 Lockout\n\n- priority: P1\n\nBody text.\n";
        let mut doc = layer_doc("basic_spec", body, 2);
        let outcome = sync_layer_items(
            &mut doc,
            body,
            &registry(),
            &prefixes(),
            "2026-09-27T00:00:00Z",
        );
        assert!(outcome.synced);
        assert!(outcome.warnings.is_empty());
        let v = doc.verification.expect("verification created");
        let login_item = v
            .items
            .iter()
            .find(|i| i.heading == "Login")
            .expect("Login section item exists");
        assert_eq!(login_item.sub_items.len(), 1);
        let sub = &login_item.sub_items[0];
        assert_eq!(sub.stable_id.as_deref(), Some("SPEC-012"));
        assert_eq!(sub.description, "Lockout");
        assert_eq!(sub.origin.as_deref(), Some("body"));
        assert_eq!(sub.category, "requirement");
        assert_eq!(sub.priority.as_deref(), Some("P1"));
        assert_eq!(sub.index, 0);
    }

    /// §2.3: effective layer `right` (e.g. `system_test`) -> category
    /// `"check"`.
    #[test]
    fn right_side_effective_layer_gets_check_category() {
        let body = "# System test\n\n### ST-040 Lockout works\n\n- verifies: SPEC-012\n\nSteps.\n";
        let mut doc = layer_doc("system_test", body, 1);
        sync_layer_items(
            &mut doc,
            body,
            &registry(),
            &prefixes(),
            "2026-09-27T00:00:00Z",
        );
        let v = doc.verification.unwrap();
        let item = v
            .items
            .iter()
            .find(|i| i.heading == "System test")
            .expect("System test section item exists");
        let sub = &item.sub_items[0];
        assert_eq!(sub.category, "check");
        assert_eq!(sub.verifies, vec!["SPEC-012".to_string()]);
    }

    /// §2.4 step 2 (seq-shift safety): inserting a new section *before* an
    /// existing one shifts every later `seq`, but the previously-recorded
    /// runtime fields (dev_stage/status/reviewer/notes/impl_refs) on both the
    /// item and its SubItem must survive because the join key is heading /
    /// stable_id, not `seq`/`index`.
    #[test]
    fn section_insertion_shifting_seq_preserves_runtime_fields() {
        let body_v1 = "# Basic spec\n\n## Login\n\n### SPEC-012 Lockout\n\nBody.\n";
        let mut doc = layer_doc("basic_spec", body_v1, 2);
        sync_layer_items(
            &mut doc,
            body_v1,
            &registry(),
            &prefixes(),
            "2026-09-27T00:00:00Z",
        );

        // Manually record runtime state as doc_verify actions would.
        {
            let v = doc.verification.as_mut().unwrap();
            let item = v.items.iter_mut().find(|i| i.heading == "Login").unwrap();
            item.reviewer = Some("ai".to_string());
            item.notes = "reviewed once".to_string();
            let sub = &mut item.sub_items[0];
            sub.dev_stage = Some("implemented".to_string());
            sub.status = "verified".to_string();
            sub.impl_refs = vec![CodeRef {
                path: "src/lockout.rs".to_string(),
                lines: None,
                label: None,
            }];
        }
        let old_login_seq = doc
            .verification
            .as_ref()
            .unwrap()
            .items
            .iter()
            .find(|i| i.heading == "Login")
            .unwrap()
            .fragment_seq;

        // v2: a brand-new section is inserted *before* "Login", shifting its
        // seq by 1 (and every seq after it).
        let body_v2 =
            "# Basic spec\n\n## Intro\n\nNew section.\n\n## Login\n\n### SPEC-012 Lockout\n\nBody.\n";
        let split_doc = super::super::split::split(body_v2, 2).unwrap();
        doc.sections = super::super::split::compute_sections(&split_doc, false);
        let outcome = sync_layer_items(
            &mut doc,
            body_v2,
            &registry(),
            &prefixes(),
            "2026-09-27T00:01:00Z",
        );
        assert!(outcome.removed.is_empty());

        let v = doc.verification.unwrap();
        let login_item = v.items.iter().find(|i| i.heading == "Login").unwrap();
        assert_ne!(
            login_item.fragment_seq, old_login_seq,
            "seq must actually have shifted for this test to be meaningful"
        );
        assert_eq!(login_item.reviewer.as_deref(), Some("ai"));
        assert_eq!(login_item.notes, "reviewed once");
        let sub = &login_item.sub_items[0];
        assert_eq!(sub.stable_id.as_deref(), Some("SPEC-012"));
        assert_eq!(sub.dev_stage.as_deref(), Some("implemented"));
        assert_eq!(sub.status, "verified");
        assert_eq!(sub.impl_refs[0].path, "src/lockout.rs");
        assert_eq!(sub.index, 0);
    }

    /// §2.4 step 6: an item removed from the body is dropped from the
    /// matrix and reported in `removed`/`warnings`, without touching
    /// anything else.
    #[test]
    fn removed_body_item_is_dropped_and_reported() {
        let body_v1 = "# Basic spec\n\n### SPEC-001 One\n\nA.\n\n### SPEC-002 Two\n\nB.\n";
        let mut doc = layer_doc("basic_spec", body_v1, 1);
        sync_layer_items(
            &mut doc,
            body_v1,
            &registry(),
            &prefixes(),
            "2026-09-27T00:00:00Z",
        );

        let body_v2 = "# Basic spec\n\n### SPEC-001 One\n\nA.\n";
        let split_doc = super::super::split::split(body_v2, 1).unwrap();
        doc.sections = super::super::split::compute_sections(&split_doc, false);
        let outcome = sync_layer_items(
            &mut doc,
            body_v2,
            &registry(),
            &prefixes(),
            "2026-09-27T00:01:00Z",
        );

        assert_eq!(outcome.removed, vec!["SPEC-002".to_string()]);
        assert!(outcome.warnings.iter().any(|w| w.contains("SPEC-002")));
        let v = doc.verification.unwrap();
        let ids: Vec<&str> = v
            .items
            .iter()
            .flat_map(|i| i.sub_items.iter())
            .filter_map(|s| s.stable_id.as_deref())
            .collect();
        assert_eq!(ids, vec!["SPEC-001"]);
    }

    /// wiki/220 §2.5 (rework round 2, MAJOR fix): a removed body item's
    /// `task_ids` (its source-of-truth-mirroring reverse-link cache) must
    /// still be reported to the caller in `removed_task_ids`, keyed by
    /// stable_id — `sync_layer_items_if_needed` folds this into an
    /// informational warning only; it must never unlink the tasks' own
    /// `task_links` (the task side is the authority, D3).
    #[test]
    fn removed_body_item_reports_its_task_ids_for_unlink() {
        let body_v1 = "# Basic spec\n\n### SPEC-001 One\n\nA.\n\n### SPEC-002 Two\n\nB.\n";
        let mut doc = layer_doc("basic_spec", body_v1, 1);
        sync_layer_items(
            &mut doc,
            body_v1,
            &registry(),
            &prefixes(),
            "2026-09-27T00:00:00Z",
        );

        // Simulate SPEC-002 having a task linked to it (as `link_task` /
        // `update_task(requirement_ids)` would have set).
        {
            let v = doc.verification.as_mut().unwrap();
            let sub = v
                .items
                .iter_mut()
                .flat_map(|i| i.sub_items.iter_mut())
                .find(|s| s.stable_id.as_deref() == Some("SPEC-002"))
                .unwrap();
            sub.task_ids = vec!["t1".to_string(), "t2".to_string()];
        }

        let body_v2 = "# Basic spec\n\n### SPEC-001 One\n\nA.\n";
        let split_doc = super::super::split::split(body_v2, 1).unwrap();
        doc.sections = super::super::split::compute_sections(&split_doc, false);
        let outcome = sync_layer_items(
            &mut doc,
            body_v2,
            &registry(),
            &prefixes(),
            "2026-09-27T00:01:00Z",
        );

        assert_eq!(outcome.removed, vec!["SPEC-002".to_string()]);
        assert_eq!(
            outcome.removed_task_ids.get("SPEC-002"),
            Some(&vec!["t1".to_string(), "t2".to_string()])
        );
    }

    /// A removed item that had no linked tasks reports an empty (not
    /// missing) entry — callers rely on this to skip the unlink cheaply
    /// without a separate existence check.
    #[test]
    fn removed_body_item_with_no_linked_tasks_reports_empty_task_ids() {
        let body_v1 = "# Basic spec\n\n### SPEC-001 One\n\nA.\n\n### SPEC-002 Two\n\nB.\n";
        let mut doc = layer_doc("basic_spec", body_v1, 1);
        sync_layer_items(
            &mut doc,
            body_v1,
            &registry(),
            &prefixes(),
            "2026-09-27T00:00:00Z",
        );

        let body_v2 = "# Basic spec\n\n### SPEC-001 One\n\nA.\n";
        let split_doc = super::super::split::split(body_v2, 1).unwrap();
        doc.sections = super::super::split::compute_sections(&split_doc, false);
        let outcome = sync_layer_items(
            &mut doc,
            body_v2,
            &registry(),
            &prefixes(),
            "2026-09-27T00:01:00Z",
        );

        assert_eq!(
            outcome.removed_task_ids.get("SPEC-002"),
            Some(&Vec::<String>::new())
        );
    }

    /// §2.4 step 4: a pre-existing `origin=None` (legacy, e.g.
    /// `add_item`-created) SubItem under a section survives an ordinary
    /// re-sync of that same section untouched.
    #[test]
    fn legacy_sub_item_is_preserved_when_its_section_still_exists() {
        let body = "# Basic spec\n\n## Login\n\n### SPEC-012 Lockout\n\nBody.\n";
        let mut doc = layer_doc("basic_spec", body, 2);
        sync_layer_items(
            &mut doc,
            body,
            &registry(),
            &prefixes(),
            "2026-09-27T00:00:00Z",
        );
        {
            let v = doc.verification.as_mut().unwrap();
            let item = v.items.iter_mut().find(|i| i.heading == "Login").unwrap();
            item.sub_items.push(SubItem {
                index: item.sub_items.len(),
                description: "hand-authored legacy item".to_string(),
                stable_id: Some("C01-9.9".to_string()),
                ..Default::default()
            });
        }
        let outcome = sync_layer_items(
            &mut doc,
            body,
            &registry(),
            &prefixes(),
            "2026-09-27T00:01:00Z",
        );
        assert!(outcome.warnings.is_empty());
        let v = doc.verification.unwrap();
        let item = v.items.iter().find(|i| i.heading == "Login").unwrap();
        assert_eq!(item.sub_items.len(), 2);
        assert!(item
            .sub_items
            .iter()
            .any(|s| s.stable_id.as_deref() == Some("C01-9.9")));
    }

    /// §2.4 step 4: when a legacy item's containing section disappears
    /// entirely, its SubItems move to the "(orphaned legacy items)" freeform
    /// item with a warning, instead of being silently dropped.
    #[test]
    fn legacy_sub_item_moves_to_orphan_bucket_when_section_removed() {
        let body_v1 = "# Basic spec\n\n## Login\n\n### SPEC-012 Lockout\n\nBody.\n";
        let mut doc = layer_doc("basic_spec", body_v1, 2);
        sync_layer_items(
            &mut doc,
            body_v1,
            &registry(),
            &prefixes(),
            "2026-09-27T00:00:00Z",
        );
        {
            let v = doc.verification.as_mut().unwrap();
            let item = v.items.iter_mut().find(|i| i.heading == "Login").unwrap();
            item.sub_items.push(SubItem {
                index: item.sub_items.len(),
                description: "hand-authored legacy item".to_string(),
                stable_id: Some("C01-9.9".to_string()),
                ..Default::default()
            });
        }

        let body_v2 = "# Basic spec\n\nIntro only, no more Login section.\n";
        let split_doc = super::super::split::split(body_v2, 2).unwrap();
        doc.sections = super::super::split::compute_sections(&split_doc, false);
        let outcome = sync_layer_items(
            &mut doc,
            body_v2,
            &registry(),
            &prefixes(),
            "2026-09-27T00:01:00Z",
        );

        assert!(outcome
            .warnings
            .iter()
            .any(|w| w.contains("Login") && w.contains(ORPHAN_LABEL)));
        let v = doc.verification.unwrap();
        let orphan = v
            .items
            .iter()
            .find(|i| i.label.as_deref() == Some(ORPHAN_LABEL))
            .expect("orphan bucket created");
        assert_eq!(orphan.sub_items.len(), 1);
        assert_eq!(orphan.sub_items[0].stable_id.as_deref(), Some("C01-9.9"));
        assert_eq!(orphan.sub_items[0].index, 0);
    }

    /// Non-layer documents (`doc.layer: None`) are completely untouched —
    /// NFR-001/002.
    #[test]
    fn no_op_when_doc_has_no_layer() {
        let mut doc = DocMetadata::new(
            "doc-1".to_string(),
            "plain-doc".to_string(),
            "Plain doc".to_string(),
            "spec".to_string(),
            "2026-09-27T00:00:00Z".to_string(),
        );
        let body = "# Title\n\nSome text.\n";
        let split_doc = super::super::split::split(body, 2).unwrap();
        doc.sections = super::super::split::compute_sections(&split_doc, false);
        let outcome = sync_layer_items(
            &mut doc,
            body,
            &registry(),
            &prefixes(),
            "2026-09-27T00:00:00Z",
        );
        assert!(!outcome.synced);
        assert!(doc.verification.is_none());
    }

    // -- M2 body notation fields (wiki/260-vmodel-m2-design.md §2.2-§2.4, M2-02) --

    /// §2.3: a synced `origin=body` item carries `def_hash`/`acceptance`/
    /// `rationale`/`derived`/`waivers`/`from`/`reserved_attrs`, re-derived
    /// from the body every sync just like the M1 fields.
    #[test]
    fn sync_populates_m2_body_notation_fields_on_the_sub_item() {
        let body = "# Basic spec\n\n### SPEC-020 監査ログの保存形式\n\n\
- refines: REQ-003\n\
- rationale: 総当たり攻撃の抑止\n\
- derived: 実装方式から必要になった項目\n\
- waive-verify: 文言のみのため目視レビューで代替\n\
- from: REQ-003#AC1\n\
- assignee: alice\n\n\
本文。\n\n受入基準:\n- AC1: 条件1\n";
        let mut doc = layer_doc("basic_spec", body, 1);
        sync_layer_items(
            &mut doc,
            body,
            &registry(),
            &prefixes(),
            "2026-09-27T00:00:00Z",
        );
        let v = doc.verification.unwrap();
        let sub = v
            .items
            .iter()
            .flat_map(|i| i.sub_items.iter())
            .find(|s| s.stable_id.as_deref() == Some("SPEC-020"))
            .expect("SPEC-020 exists");
        assert!(sub.def_hash.is_some());
        assert_eq!(sub.acceptance.len(), 1);
        assert_eq!(sub.acceptance[0].label, "AC1");
        assert_eq!(sub.rationale.as_deref(), Some("総当たり攻撃の抑止"));
        assert_eq!(sub.derived.as_deref(), Some("実装方式から必要になった項目"));
        assert_eq!(sub.waivers.len(), 1);
        assert_eq!(sub.waivers[0].axis, "verify");
        assert_eq!(sub.from.as_deref(), Some("REQ-003#AC1"));
        assert_eq!(
            sub.reserved_attrs.get("assignee").map(String::as_str),
            Some("alice")
        );
    }

    /// §2.5 step 3: `sync_layer_items` (the plain, M1-compatible entry
    /// point) never materializes implicit acceptance-verification items —
    /// only `sync_layer_items_with_options(.., implicit_acceptance: true)`
    /// does.
    #[test]
    fn plain_sync_layer_items_never_materializes_implicit_acceptance_items() {
        let body = "# Req\n\n### REQ-003 ログイン失敗時のアカウントロック\n\n本文。\n\n\
受入基準:\n- AC1: 条件1\n";
        let mut doc = layer_doc("requirement", body, 1);
        sync_layer_items(
            &mut doc,
            body,
            &registry(),
            &prefixes(),
            "2026-09-27T00:00:00Z",
        );
        let v = doc.verification.unwrap();
        let ids: Vec<Option<&str>> = v
            .items
            .iter()
            .flat_map(|i| i.sub_items.iter())
            .map(|s| s.stable_id.as_deref())
            .collect();
        assert_eq!(ids, vec![Some("REQ-003")]);
    }

    /// §2.5 step 3: with `implicit_acceptance: true`, one implicit
    /// acceptance-verification `SubItem` is materialized per
    /// acceptance-criteria bullet, right after its parent, on the parent's
    /// paired layer, with `verifies: ["REQ-003#AC1"]` and
    /// `def_hash == ac_hash(REQ-003, AC1)`.
    #[test]
    fn implicit_acceptance_materializes_one_sub_item_per_ac_after_the_parent() {
        let body = "# Req\n\n### REQ-003 ログイン失敗時のアカウントロック\n\n本文。\n\n\
受入基準:\n- AC1: 条件1\n- AC2: 条件2\n";
        let mut doc = layer_doc("requirement", body, 1);
        let outcome = sync_layer_items_with_options(
            &mut doc,
            body,
            &registry(),
            &prefixes(),
            "2026-09-27T00:00:00Z",
            true,
        );
        assert!(outcome.synced);
        let v = doc.verification.unwrap();
        let subs: Vec<&SubItem> = v.items.iter().flat_map(|i| i.sub_items.iter()).collect();
        let ids: Vec<Option<&str>> = subs.iter().map(|s| s.stable_id.as_deref()).collect();
        assert_eq!(
            ids,
            vec![Some("REQ-003"), Some("REQ-003#AC1"), Some("REQ-003#AC2")],
            "implicit items must directly follow their parent, in AC order"
        );

        let parent = subs
            .iter()
            .find(|s| s.stable_id.as_deref() == Some("REQ-003"))
            .unwrap();
        let ac1 = subs
            .iter()
            .find(|s| s.stable_id.as_deref() == Some("REQ-003#AC1"))
            .unwrap();
        assert_eq!(ac1.implicit_of.as_deref(), Some("REQ-003"));
        assert_eq!(ac1.verifies, vec!["REQ-003#AC1".to_string()]);
        assert_eq!(ac1.origin.as_deref(), Some("body"));
        // requirement's pair is acceptance (right side) -> category "check".
        assert_eq!(ac1.category, "check");
        assert_eq!(ac1.layer.as_deref(), Some("acceptance"));
        assert!(
            ac1.def_hash.is_some(),
            "def_hash must be set (exact value checked in the dedicated test below)"
        );
        assert!(parent.acceptance.iter().any(|a| a.label == "AC1"));
    }

    /// §2.4: the implicit item's own `def_hash` equals `ac_hash(parent,
    /// label)` exactly — this is what lets a suspect check on the implicit
    /// item track the same upstream text as the AC-unit link it verifies.
    #[test]
    fn implicit_acceptance_def_hash_equals_parent_ac_hash() {
        use super::super::layer_parse::{default_prefix_table, parse_layer_body};

        let body = "# Req\n\n### REQ-003 タイトル\n\n本文。\n\n受入基準:\n- AC1: 条件1\n";
        let mut doc = layer_doc("requirement", body, 1);
        sync_layer_items_with_options(
            &mut doc,
            body,
            &registry(),
            &prefixes(),
            "2026-09-27T00:00:00Z",
            true,
        );
        let v = doc.verification.unwrap();
        let ac1 = v
            .items
            .iter()
            .flat_map(|i| i.sub_items.iter())
            .find(|s| s.stable_id.as_deref() == Some("REQ-003#AC1"))
            .unwrap();

        let table = default_prefix_table(&registry(), &prefixes());
        let parsed = parse_layer_body(body, Some("requirement"), &table);
        let expected = parsed.items[0].acceptance[0].ac_hash.clone();
        assert_eq!(ac1.def_hash.as_deref(), Some(expected.as_str()));
    }

    /// §2.5 step 3: toggling `implicit_acceptance` from true to false drops
    /// the previously-materialized implicit items and reports them via
    /// `removed` (M1's step 6 behavior, unchanged for these ids).
    #[test]
    fn implicit_acceptance_disabled_after_being_enabled_removes_the_implicit_items() {
        let body = "# Req\n\n### REQ-003 タイトル\n\n本文。\n\n受入基準:\n- AC1: 条件1\n";
        let mut doc = layer_doc("requirement", body, 1);
        sync_layer_items_with_options(
            &mut doc,
            body,
            &registry(),
            &prefixes(),
            "2026-09-27T00:00:00Z",
            true,
        );
        let outcome = sync_layer_items_with_options(
            &mut doc,
            body,
            &registry(),
            &prefixes(),
            "2026-09-27T00:01:00Z",
            false,
        );
        assert_eq!(outcome.removed, vec!["REQ-003#AC1".to_string()]);
        let v = doc.verification.unwrap();
        let ids: Vec<Option<&str>> = v
            .items
            .iter()
            .flat_map(|i| i.sub_items.iter())
            .map(|s| s.stable_id.as_deref())
            .collect();
        assert_eq!(ids, vec![Some("REQ-003")]);
    }

    /// §2.3: `link_baselines` (a runtime field this module never writes) and
    /// `implicit_of` (written only for implicit items themselves) survive an
    /// ordinary re-sync of an unrelated attribute, via the same
    /// `body_owned.remove(id)` restore M1 already relies on for
    /// `task_ids`/`dev_stage`.
    #[test]
    fn link_baselines_on_an_ordinary_item_survive_a_resync() {
        let body_v1 = "# Req\n\n### REQ-003 タイトル\n\n本文。\n";
        let mut doc = layer_doc("requirement", body_v1, 1);
        sync_layer_items(
            &mut doc,
            body_v1,
            &registry(),
            &prefixes(),
            "2026-09-27T00:00:00Z",
        );
        {
            let v = doc.verification.as_mut().unwrap();
            let sub = v
                .items
                .iter_mut()
                .flat_map(|i| i.sub_items.iter_mut())
                .find(|s| s.stable_id.as_deref() == Some("REQ-003"))
                .unwrap();
            sub.link_baselines
                .insert("SPEC-020".to_string(), "abc123".to_string());
        }

        // Re-sync after an unrelated body change (priority added).
        let body_v2 = "# Req\n\n### REQ-003 タイトル\n\n- priority: P1\n\n本文。\n";
        let split_doc = super::super::split::split(body_v2, 1).unwrap();
        doc.sections = super::super::split::compute_sections(&split_doc, false);
        sync_layer_items(
            &mut doc,
            body_v2,
            &registry(),
            &prefixes(),
            "2026-09-27T00:01:00Z",
        );

        let v = doc.verification.unwrap();
        let sub = v
            .items
            .iter()
            .flat_map(|i| i.sub_items.iter())
            .find(|s| s.stable_id.as_deref() == Some("REQ-003"))
            .unwrap();
        assert_eq!(
            sub.link_baselines.get("SPEC-020").map(String::as_str),
            Some("abc123"),
            "link_baselines must survive an unrelated resync (M2-04 writes it, this module preserves it)"
        );
    }

    // -- def_changed (§2.5 step 5) --

    /// §2.5 step 5: a brand-new item (no prior `def_hash`) counts as changed.
    #[test]
    fn def_changed_includes_a_brand_new_item_on_first_sync() {
        let body = "# Req\n\n### REQ-050 タイトル\n\n本文。\n";
        let mut doc = layer_doc("requirement", body, 1);
        let outcome = sync_layer_items(
            &mut doc,
            body,
            &registry(),
            &prefixes(),
            "2026-09-27T00:00:00Z",
        );
        assert_eq!(outcome.def_changed, vec!["REQ-050".to_string()]);
    }

    /// §2.5 step 5: a resync that doesn't touch the item's `def_hash`-input
    /// (title/statement-minus-acceptance/acceptance) — only an M1 attribute
    /// (`priority`) changes — must NOT report that item in `def_changed`
    /// (§2.4: `def_hash` deliberately ignores attributes).
    #[test]
    fn def_changed_excludes_an_item_whose_def_hash_input_is_unaffected() {
        let body_v1 = "# Req\n\n### REQ-051 タイトル\n\n本文。\n";
        let mut doc = layer_doc("requirement", body_v1, 1);
        sync_layer_items(
            &mut doc,
            body_v1,
            &registry(),
            &prefixes(),
            "2026-09-27T00:00:00Z",
        );

        let body_v2 = "# Req\n\n### REQ-051 タイトル\n\n- priority: P1\n\n本文。\n";
        let split_doc = super::super::split::split(body_v2, 1).unwrap();
        doc.sections = super::super::split::compute_sections(&split_doc, false);
        let outcome = sync_layer_items(
            &mut doc,
            body_v2,
            &registry(),
            &prefixes(),
            "2026-09-27T00:01:00Z",
        );
        assert!(
            outcome.def_changed.is_empty(),
            "an attribute-only change must not appear in def_changed: {:?}",
            outcome.def_changed
        );
    }

    /// §2.5 step 5: changing the item's acceptance criteria reports both the
    /// parent item and its materialized implicit acceptance-verification
    /// item in `def_changed`.
    #[test]
    fn def_changed_reports_parent_and_implicit_item_when_acceptance_changes() {
        let body_v1 = "# Req\n\n### REQ-052 タイトル\n\n本文。\n\n受入基準:\n- AC1: 条件1\n";
        let mut doc = layer_doc("requirement", body_v1, 1);
        sync_layer_items_with_options(
            &mut doc,
            body_v1,
            &registry(),
            &prefixes(),
            "2026-09-27T00:00:00Z",
            true,
        );

        let body_v2 = "# Req\n\n### REQ-052 タイトル\n\n本文。\n\n受入基準:\n- AC1: 条件1変更\n";
        let split_doc = super::super::split::split(body_v2, 1).unwrap();
        doc.sections = super::super::split::compute_sections(&split_doc, false);
        let outcome = sync_layer_items_with_options(
            &mut doc,
            body_v2,
            &registry(),
            &prefixes(),
            "2026-09-27T00:01:00Z",
            true,
        );
        assert_eq!(
            outcome.def_changed,
            vec!["REQ-052".to_string(), "REQ-052#AC1".to_string()]
        );
    }

    /// Rework round 2 (BLOCKER fix, wiki/260 §2.1/§2.5 手順 3): a
    /// document-level `trace_profile` override resolves to that profile's
    /// own `implicit_acceptance`, taking priority over the project default.
    #[test]
    fn resolve_doc_implicit_acceptance_uses_doc_trace_profile_override_over_project_default() {
        use crate::storage::config::TraceConfig;

        let mut doc = layer_doc("requirement", "# Req\n", 1);
        doc.trace_profile = Some("minimal".to_string());
        let trace_config = TraceConfig {
            profile: Some("standard".to_string()), // implicit_acceptance: false
            ..Default::default()
        };
        let (implicit, warnings) =
            resolve_doc_implicit_acceptance(&doc, &trace_config, &registry());
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert!(
            implicit,
            "doc-level 'minimal' override must win over project 'standard'"
        );
    }

    /// Inverse of the test above (session review round 2): a doc-level
    /// override that resolves to `implicit_acceptance: false` must also win
    /// over a project default that resolves to `true` — the override
    /// replaces the project default, it is not OR-ed with it.
    #[test]
    fn resolve_doc_implicit_acceptance_doc_override_false_beats_project_default_true() {
        use crate::storage::config::TraceConfig;

        let mut doc = layer_doc("requirement", "# Req\n", 1);
        doc.trace_profile = Some("standard".to_string()); // implicit_acceptance: false
        let trace_config = TraceConfig {
            profile: Some("minimal".to_string()), // implicit_acceptance: true
            ..Default::default()
        };
        let (implicit, warnings) =
            resolve_doc_implicit_acceptance(&doc, &trace_config, &registry());
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert!(
            !implicit,
            "doc-level 'standard' override must win over project 'minimal'"
        );
    }

    /// Falls back to the project default profile (`[trace] profile`) when
    /// the document has no `trace_profile` override of its own.
    #[test]
    fn resolve_doc_implicit_acceptance_falls_back_to_project_default_profile() {
        use crate::storage::config::TraceConfig;

        let doc = layer_doc("requirement", "# Req\n", 1);
        let trace_config = TraceConfig {
            profile: Some("bugfix".to_string()), // implicit_acceptance: true
            ..Default::default()
        };
        let (implicit, warnings) =
            resolve_doc_implicit_acceptance(&doc, &trace_config, &registry());
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert!(
            implicit,
            "project default 'bugfix' must resolve implicit_acceptance = true"
        );
    }

    /// `false` when neither the document nor the project has a profile
    /// configured — matches `sync_layer_items`'s (5-arg, always-off) M1
    /// behavior when there is nothing to resolve.
    #[test]
    fn resolve_doc_implicit_acceptance_defaults_to_false_when_unconfigured() {
        use crate::storage::config::TraceConfig;

        let doc = layer_doc("requirement", "# Req\n", 1);
        let trace_config = TraceConfig::default();
        let (implicit, warnings) =
            resolve_doc_implicit_acceptance(&doc, &trace_config, &registry());
        assert!(warnings.is_empty());
        assert!(!implicit);
    }
}
