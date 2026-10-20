//! Task -> plan-document `SubItem` lookups shared by two cascades:
//!
//! - SPEC-SI-008: `handoff_update_task` reports `plan_update_needed` when a
//!   task moving to `done` is linked to a plan `SubItem`.
//! - SPEC-SI-009: `handoff_auto_schedule` takes a plan `SubItem`'s `order`
//!   as the execution order of the tasks linked to it.
//!
//! A task reaches a `SubItem` through its `link_type: "requirement"`
//! `task_links` whose `label` is the `SubItem`'s `stable_id`. Only documents
//! with `doc_type == "plan"` count.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::storage::docs::model::{DocMetadata, UNSET_DEV_STAGE};
use crate::storage::docs::DocSet;
use crate::storage::tasks::TaskLink;

use super::docs::resolve_stable_ids_from;

/// `doc_type` of an implementation-plan document.
const PLAN_DOC_TYPE: &str = "plan";

/// The `dev_stage` a finished task suggests for its plan `SubItem`.
const SUGGESTED_DEV_STAGE_ON_DONE: &str = "implemented";

/// The `dev_stage` a `SubItem` is treated as having when none is recorded
/// (matches dev_stage propagation's own default).
const DEFAULT_DEV_STAGE: &str = UNSET_DEV_STAGE;

/// A plan-document `SubItem` that a task links to.
#[derive(Debug, PartialEq)]
pub(crate) struct PlanSubItem {
    pub(crate) doc_id: String,
    pub(crate) stable_id: String,
    pub(crate) dev_stage: Option<String>,
    /// The item's `- order: <n>` key; with only per-parent keys
    /// (`- order@<parent>: <n>`) the smallest of them.
    pub(crate) order: Option<u32>,
}

/// The `stable_id`s of the requirement links in `links`, de-duplicated in
/// first-seen order. `include_executes: false` drops `role: "executes"`
/// links (test-execution tasks), mirroring dev_stage propagation.
pub(crate) fn linked_stable_ids(links: &[TaskLink], include_executes: bool) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for link in links {
        if link.link_type != "requirement" {
            continue;
        }
        if !include_executes && link.role.as_deref() == Some("executes") {
            continue;
        }
        if let Some(label) = &link.label {
            if !ids.contains(label) {
                ids.push(label.clone());
            }
        }
    }
    ids
}

/// Resolves `stable_ids` against the plan documents in `docs`. Ids that match
/// no plan `SubItem`, or more than one (ambiguous), are left out.
pub(crate) fn resolve_plan_sub_items(
    docs: &[DocMetadata],
    stable_ids: &[String],
) -> Vec<PlanSubItem> {
    let plan_docs = docs.iter().filter(|d| d.doc_type == PLAN_DOC_TYPE);
    let (resolved, _unresolved, _ambiguous) = resolve_stable_ids_from(plan_docs, stable_ids);
    resolved
        .into_iter()
        .filter_map(|r| {
            let doc = docs.iter().find(|d| d.id == r.doc_id)?;
            let sub = doc
                .verification
                .as_ref()?
                .items
                .iter()
                .flat_map(|i| i.sub_items.iter())
                .find(|s| s.stable_id.as_deref() == Some(r.stable_id.as_str()))?;
            Some(PlanSubItem {
                doc_id: r.doc_id,
                stable_id: r.stable_id,
                dev_stage: sub.dev_stage.clone(),
                order: sub
                    .tree_order
                    .or_else(|| sub.tree_order_per_parent.values().copied().min()),
            })
        })
        .collect()
}

/// SPEC-SI-008: the `plan_update_needed` entries for a task that just moved
/// to `done` — one per plan `SubItem` it links to. Empty (and no document is
/// loaded) when the task has no requirement link.
pub(crate) fn plan_update_needed(handoff_dir: &Path, links: &[TaskLink]) -> Result<Vec<Value>> {
    let stable_ids = linked_stable_ids(links, false);
    if stable_ids.is_empty() {
        return Ok(Vec::new());
    }
    let doc_set = DocSet::load(handoff_dir).context("Failed to load documents")?;
    Ok(resolve_plan_sub_items(doc_set.docs(), &stable_ids)
        .into_iter()
        .map(|item| {
            json!({
                "doc_id": item.doc_id,
                "stable_id": item.stable_id,
                "current_dev_stage": item.dev_stage.as_deref().unwrap_or(DEFAULT_DEV_STAGE),
                "suggested_dev_stage": SUGGESTED_DEV_STAGE_ON_DONE,
            })
        })
        .collect())
}

/// Result of [`plan_orders`]: the orders found plus anything that kept the
/// lookup from being complete.
#[derive(Debug, Default)]
pub(crate) struct PlanOrders {
    pub orders: HashMap<String, u32>,
    /// Human-readable reasons the lookup was incomplete (documents that could
    /// not be listed or parsed). Empty when every document was read.
    pub warnings: Vec<String>,
}

/// SPEC-SI-009: `stable_id -> order` for every plan `SubItem` among
/// `stable_ids` that carries an order. No document is loaded when
/// `stable_ids` is empty. Never fails: scheduling worked without documents
/// before plan orders existed, so an unreadable corpus (or a single document)
/// is reported in `warnings` and the orders that could be read are returned.
pub(crate) fn plan_orders(handoff_dir: &Path, stable_ids: &[String]) -> PlanOrders {
    if stable_ids.is_empty() {
        return PlanOrders::default();
    }
    let doc_set = match DocSet::load(handoff_dir) {
        Ok(doc_set) => doc_set,
        Err(e) => {
            return PlanOrders {
                orders: HashMap::new(),
                warnings: vec![format!(
                    "plan order not applied: documents could not be loaded ({e:#})"
                )],
            }
        }
    };
    let warnings = doc_set
        .unreadable()
        .iter()
        .map(|u| {
            format!(
                "plan order may be incomplete: document '{}' is unreadable ({})",
                u.slug, u.error
            )
        })
        .collect();
    let orders = resolve_plan_sub_items(doc_set.docs(), stable_ids)
        .into_iter()
        .filter_map(|item| item.order.map(|o| (item.stable_id, o)))
        .collect();
    PlanOrders { orders, warnings }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::docs::model::{SubItem, Verification, VerificationItem};

    fn link(label: &str, role: Option<&str>) -> TaskLink {
        TaskLink {
            target: "doc-1".to_string(),
            link_type: "requirement".to_string(),
            label: Some(label.to_string()),
            role: role.map(String::from),
            ..Default::default()
        }
    }

    fn sub(stable_id: &str, dev_stage: Option<&str>, order: Option<u32>) -> SubItem {
        let mut s: SubItem = serde_json::from_value(json!({
            "index": 0, "description": "d", "status": "pending",
        }))
        .unwrap();
        s.stable_id = Some(stable_id.to_string());
        s.dev_stage = dev_stage.map(String::from);
        s.tree_order = order;
        s
    }

    fn doc(id: &str, doc_type: &str, subs: Vec<SubItem>) -> DocMetadata {
        let mut d = DocMetadata::new(
            id.to_string(),
            id.to_string(),
            "T".to_string(),
            doc_type.to_string(),
            "2026-01-01T00:00:00Z".to_string(),
        );
        d.verification = Some(Verification {
            status: "pending".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            items: vec![VerificationItem {
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
                sub_items: subs,
                label: None,
            }],
        });
        d
    }

    #[test]
    fn linked_stable_ids_filters_type_role_and_duplicates() {
        let mut other = link("X", None);
        other.link_type = "doc".to_string();
        let links = vec![
            link("A", None),
            link("A", Some("implements")),
            link("B", Some("executes")),
            other,
        ];
        assert_eq!(linked_stable_ids(&links, false), ["A"]);
        assert_eq!(linked_stable_ids(&links, true), ["A", "B"]);
    }

    #[test]
    fn resolve_only_plan_docs() {
        let docs = vec![
            doc("d-spec", "spec", vec![sub("S-1", None, Some(1))]),
            doc(
                "d-plan",
                "plan",
                vec![sub("P-1", Some("in_progress"), Some(4))],
            ),
        ];
        let ids = vec!["S-1".to_string(), "P-1".to_string()];
        let got = resolve_plan_sub_items(&docs, &ids);
        assert_eq!(
            got,
            vec![PlanSubItem {
                doc_id: "d-plan".to_string(),
                stable_id: "P-1".to_string(),
                dev_stage: Some("in_progress".to_string()),
                order: Some(4),
            }]
        );
    }

    #[test]
    fn order_falls_back_to_smallest_per_parent_key() {
        let mut s = sub("P-1", None, None);
        s.tree_order_per_parent.insert("A".to_string(), 7);
        s.tree_order_per_parent.insert("B".to_string(), 3);
        let docs = vec![doc("d-plan", "plan", vec![s])];
        let got = resolve_plan_sub_items(&docs, &["P-1".to_string()]);
        assert_eq!(got[0].order, Some(3));
    }

    #[test]
    fn plan_update_needed_is_empty_without_links_and_skips_doc_load() {
        // A nonexistent handoff dir would fail DocSet::load; empty links must
        // short-circuit before it.
        let got = plan_update_needed(Path::new("/nonexistent/handoff"), &[]).unwrap();
        assert!(got.is_empty());
    }
}
