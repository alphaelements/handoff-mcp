//! Documents linked to a task, for `handoff_doc_export`'s `task_id`
//! (REQ-PDF-027).
//!
//! A task reaches a document in four ways, all counted:
//!
//! - the document's own `task_ids` (`handoff_doc_save task_ids`);
//! - a task link `link_type: "doc"` whose target is the document's id or slug;
//! - a task link `link_type: "requirement"`, which points at the document
//!   holding the item (`target` = document id) and names the item (`label` =
//!   its stable id) — the document is found by either, so a link whose target
//!   went stale still resolves through the stable id;
//! - a requirement item of the document that lists the task in its own
//!   `task_ids` (the item-side view of the same link).
//!
//! Reading is strictly read-only.

use std::collections::HashSet;

use anyhow::{anyhow, Context, Result};

use super::plan_links::linked_stable_ids;
use super::HandlerContext;
use crate::storage::docs::{read_all_docs_with_unreadable, DocMetadata};
use crate::storage::tasks::{find_task_dir, read_task, TaskData};

/// A task and the documents linked to it.
pub(super) struct TaskDocs {
    pub task_id: String,
    pub task_title: String,
    /// Ids of the linked documents.
    pub doc_ids: HashSet<String>,
}

/// Loads `task_id` and the documents (of those in the project) linked to it.
/// An unknown task is an error: an empty result would look like "no links".
pub(super) fn collect(ctx: &HandlerContext, task_id: &str) -> Result<TaskDocs> {
    let task_dir = find_task_dir(&ctx.handoff_dir.join("tasks"), task_id)?;
    let (task, _status) = read_task(&task_dir)?
        .ok_or_else(|| anyhow!("Task not found: {task_id}"))
        .with_context(|| format!("Cannot read task {task_id}"))?;
    // Documents with unreadable frontmatter have no metadata to match on.
    let (docs, _unreadable) = read_all_docs_with_unreadable(&ctx.handoff_dir)?;
    Ok(TaskDocs {
        doc_ids: linked_doc_ids(&task, &docs),
        task_id: task.id,
        task_title: task.title,
    })
}

/// Ids of the documents in `docs` that `task` is linked to.
fn linked_doc_ids(task: &TaskData, docs: &[DocMetadata]) -> HashSet<String> {
    let links = task.links();
    let stable_ids: HashSet<String> = linked_stable_ids(&links, true).into_iter().collect();
    // A requirement link's target is the document holding the item.
    let doc_ids: HashSet<&str> = links
        .iter()
        .filter(|l| l.link_type == "doc" || l.link_type == "requirement")
        .map(|l| l.target.as_str())
        .collect();
    // A doc link may name the document by slug as well.
    let doc_slugs: HashSet<&str> = links
        .iter()
        .filter(|l| l.link_type == "doc")
        .map(|l| l.target.as_str())
        .collect();

    docs.iter()
        .filter(|doc| {
            doc.task_ids.contains(&task.id)
                || doc_ids.contains(doc.id.as_str())
                || doc_slugs.contains(doc.slug.as_str())
                || has_linked_item(doc, &task.id, &stable_ids)
        })
        .map(|doc| doc.id.clone())
        .collect()
}

/// Whether one of the document's requirement items is linked to the task: its
/// stable id is among the task's requirement links, or it lists the task.
fn has_linked_item(doc: &DocMetadata, task_id: &str, stable_ids: &HashSet<String>) -> bool {
    doc.verification.as_ref().is_some_and(|v| {
        v.items.iter().flat_map(|i| &i.sub_items).any(|sub| {
            sub.task_ids.iter().any(|t| t == task_id)
                || sub
                    .stable_id
                    .as_ref()
                    .is_some_and(|id| stable_ids.contains(id))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::tasks::TaskLink;

    fn doc(id: &str, slug: &str) -> DocMetadata {
        DocMetadata::new(
            id.to_string(),
            slug.to_string(),
            slug.to_string(),
            "spec".to_string(),
            "2026-01-01T00:00:00Z".to_string(),
        )
    }

    fn with_item(mut d: DocMetadata, stable_id: &str, task_ids: &[&str]) -> DocMetadata {
        d.verification = Some(
            serde_json::from_value(serde_json::json!({
                "status": "pending", "created_at": "t", "updated_at": "t",
                "items": [{
                    "heading": "h", "status": "pending",
                    "sub_items": [{
                        "index": 0, "description": "d", "status": "pending",
                        "stable_id": stable_id, "task_ids": task_ids,
                    }],
                }],
            }))
            .unwrap(),
        );
        d
    }

    fn task(links: Vec<TaskLink>) -> TaskData {
        let mut t: TaskData =
            serde_json::from_value(serde_json::json!({ "id": "t1", "title": "T" })).unwrap();
        t.task_links = links;
        t
    }

    fn link(link_type: &str, target: &str, label: Option<&str>) -> TaskLink {
        TaskLink {
            target: target.to_string(),
            link_type: link_type.to_string(),
            label: label.map(str::to_string),
            ..Default::default()
        }
    }

    fn ids(task: &TaskData, docs: &[DocMetadata]) -> Vec<String> {
        let mut v: Vec<String> = linked_doc_ids(task, docs).into_iter().collect();
        v.sort();
        v
    }

    #[test]
    fn a_doc_link_matches_by_id_or_slug() {
        let docs = vec![doc("d1", "one"), doc("d2", "two"), doc("d3", "three")];
        let t = task(vec![link("doc", "d1", None), link("doc", "two", None)]);
        assert_eq!(ids(&t, &docs), ["d1", "d2"]);
    }

    #[test]
    fn the_documents_own_task_ids_count() {
        let mut d = doc("d1", "one");
        d.task_ids = vec!["t1".into()];
        let mut other = doc("d2", "two");
        other.task_ids = vec!["t9".into()];
        assert_eq!(ids(&task(Vec::new()), &[d, other]), ["d1"]);
    }

    #[test]
    fn a_requirement_link_finds_the_document_by_target_or_by_stable_id() {
        let docs = vec![
            with_item(doc("d1", "one"), "REQ-001", &[]),
            with_item(doc("d2", "two"), "REQ-002", &[]),
            with_item(doc("d3", "three"), "REQ-003", &[]),
        ];
        // d1 by label (target stale), d2 by target only.
        let t = task(vec![
            link("requirement", "gone", Some("REQ-001")),
            link("requirement", "d2", Some("REQ-X")),
        ]);
        assert_eq!(ids(&t, &docs), ["d1", "d2"]);
    }

    #[test]
    fn an_item_listing_the_task_counts_even_without_a_task_link() {
        let docs = vec![
            with_item(doc("d1", "one"), "REQ-001", &["t1"]),
            with_item(doc("d2", "two"), "REQ-002", &["t2"]),
        ];
        assert_eq!(ids(&task(Vec::new()), &docs), ["d1"]);
    }

    #[test]
    fn file_and_url_links_never_select_a_document() {
        let docs = vec![doc("d1", "one")];
        let t = task(vec![link("file", "d1", None), link("url", "one", None)]);
        assert!(ids(&t, &docs).is_empty());
    }

    #[test]
    fn an_unlinked_task_selects_nothing() {
        let docs = vec![with_item(doc("d1", "one"), "REQ-001", &[])];
        assert!(ids(&task(Vec::new()), &docs).is_empty());
    }
}
