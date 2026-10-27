//! Requirement meta for `handoff_doc_export`'s `enrich` mode (REQ-PDF-030).
//!
//! Builds, for the items of the exported documents, what `handoff_doc_req_list`
//! and `handoff_trace_report` know about them: priority, dev stage, horizontal
//! and vertical coverage, the titled `refines` / `verifies` targets, the
//! implementing tasks and which acceptance criteria a passing verification
//! covers. Reading is strictly read-only (E6: the same in-memory resync the
//! other `trace_*` read tools use), so an export never writes document or task
//! state.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use anyhow::Result;

use super::trace::LoadedTrace;
use super::trace_readonly::load_trace_input_fully_read_only;
use crate::storage::docs::pdf::req_meta::{
    Coverage, CoverageState, LinkRef, ReqMeta, ReqMetaIndex, TaskRef,
};
use crate::storage::docs::pdf::types::{PdfWarning, PdfWarningCode};
use crate::storage::docs::DocMetadata;
use crate::trace::types::{CoverageStatus, ItemState, TaskLinkRole};
use crate::trace::TraceGraph;

/// The meta of the exported documents' items, and why it may be missing.
#[derive(Debug, Default)]
pub(super) struct Enrichment {
    pub index: ReqMetaIndex,
    /// Set when the trace data could not be loaded: the export is then plain.
    pub warning: Option<PdfWarning>,
}

/// Meta for every item of `docs` (the documents about to be exported).
///
/// Documents without items (no `layer`, so no verification matrix) cost
/// nothing: when none of them has an item the trace data is not even loaded.
/// A load failure does not fail the export; it is returned as a warning.
pub(super) fn collect(handoff: &Path, docs: &[&DocMetadata]) -> Enrichment {
    let wanted: HashSet<&str> = docs
        .iter()
        .filter(|d| has_items(d))
        .map(|d| d.id.as_str())
        .collect();
    if wanted.is_empty() {
        return Enrichment::default();
    }
    match build_index(handoff, &wanted) {
        Ok(index) => Enrichment {
            index,
            warning: None,
        },
        Err(e) => Enrichment {
            index: ReqMetaIndex::new(),
            warning: Some(PdfWarning::new(
                PdfWarningCode::EnrichUnavailable,
                format!(
                    "requirement meta blocks were left out because the trace data could not be \
                     loaded: {e:#}"
                ),
            )),
        },
    }
}

fn has_items(doc: &DocMetadata) -> bool {
    doc.verification.as_ref().is_some_and(|v| {
        v.items
            .iter()
            .flat_map(|item| &item.sub_items)
            .any(|sub| sub.stable_id.is_some() && sub.implicit_of.is_none())
    })
}

fn build_index(handoff: &Path, wanted_docs: &HashSet<&str>) -> Result<ReqMetaIndex> {
    let loaded = load_trace_input_fully_read_only(handoff, Vec::new())?.loaded;
    let graph = TraceGraph::build(&loaded.trace_input);
    Ok(index_from(&loaded, &graph, wanted_docs))
}

/// The pure part of [`build_index`]: one [`ReqMeta`] per item (implicit
/// `X#ACn` verification items excluded) of the documents in `wanted_docs`.
fn index_from(
    loaded: &LoadedTrace,
    graph: &TraceGraph,
    wanted_docs: &HashSet<&str>,
) -> ReqMetaIndex {
    let titles = item_titles(&loaded.docs);
    let verifiers = verifiers_by_target(loaded);
    let tasks = implementing_tasks(loaded);

    let mut index = ReqMetaIndex::new();
    for doc in loaded
        .docs
        .iter()
        .filter(|d| wanted_docs.contains(d.id.as_str()))
    {
        let Some(verification) = &doc.verification else {
            continue;
        };
        for sub in verification.items.iter().flat_map(|i| &i.sub_items) {
            let Some(id) = sub.stable_id.as_deref() else {
                continue;
            };
            if sub.implicit_of.is_some() {
                continue;
            }
            let (verified_acs, all_acs_verified) = verified_criteria(id, graph, &verifiers);
            let total_acs = sub.acceptance.len();
            let done_acs = sub
                .acceptance
                .iter()
                .filter(|ac| all_acs_verified || verified_acs.contains(&ac.label))
                .count();
            index.insert(
                id.to_string(),
                ReqMeta {
                    priority: sub.priority.clone(),
                    dev_stage: sub.dev_stage.clone(),
                    horizontal: coverage(
                        graph.item_horizontal(id),
                        (total_acs > 0).then_some((done_acs, total_acs)),
                    ),
                    vertical: coverage(graph.item_vertical(id), None),
                    refines: link_refs(&sub.refines, &titles),
                    verifies: link_refs(&sub.verifies, &titles),
                    tasks: tasks.get(id).cloned().unwrap_or_default(),
                    verified_acs,
                    all_acs_verified,
                },
            );
        }
    }
    index
}

/// stable id -> title, over every document (link targets live elsewhere).
fn item_titles(docs: &[DocMetadata]) -> HashMap<&str, &str> {
    let mut titles = HashMap::new();
    for doc in docs {
        let Some(verification) = &doc.verification else {
            continue;
        };
        for sub in verification.items.iter().flat_map(|i| &i.sub_items) {
            if let (Some(id), None) = (sub.stable_id.as_deref(), &sub.implicit_of) {
                titles.entry(id).or_insert(sub.description.as_str());
            }
        }
    }
    titles
}

/// `(verifier id, criterion label)` per verified item; `None` when the
/// verifier covers the item as a whole.
type Verifiers<'a> = HashMap<&'a str, Vec<(&'a str, Option<&'a str>)>>;

fn verifiers_by_target(loaded: &LoadedTrace) -> Verifiers<'_> {
    let mut by_target: Verifiers<'_> = HashMap::new();
    for item in &loaded.trace_input.items {
        for raw in &item.verifies {
            let (target, label) = match raw.split_once('#') {
                Some((target, label)) if !label.is_empty() => (target, Some(label)),
                _ => (raw.as_str(), None),
            };
            by_target
                .entry(target)
                .or_default()
                .push((item.stable_id.as_str(), label));
        }
    }
    by_target
}

/// Which acceptance criteria of `id` a *passing* verification item covers,
/// and whether one passes for the item as a whole (or the item is its own
/// passing inline verification).
fn verified_criteria(
    id: &str,
    graph: &TraceGraph,
    verifiers: &Verifiers<'_>,
) -> (BTreeSet<String>, bool) {
    let mut labels = BTreeSet::new();
    let mut whole = graph.is_inline(id) && graph.own_state(id) == Some(ItemState::Passing);
    for (verifier, label) in verifiers.get(id).into_iter().flatten() {
        if graph.state(verifier) != Some(ItemState::Passing) {
            continue;
        }
        match label {
            Some(label) => {
                labels.insert((*label).to_string());
            }
            None => whole = true,
        }
    }
    (labels, whole)
}

fn coverage(status: Option<CoverageStatus>, done: Option<(usize, usize)>) -> Option<Coverage> {
    let state = match status? {
        CoverageStatus::Covered => CoverageState::Covered,
        CoverageStatus::Partial => CoverageState::Partial,
        CoverageStatus::Uncovered => CoverageState::Uncovered,
        CoverageStatus::Waived => CoverageState::Waived,
        // Nothing to cover on this axis: no bar.
        CoverageStatus::Na => return None,
    };
    Some(Coverage {
        state,
        done: done.filter(|_| state == CoverageState::Partial),
    })
}

fn link_refs(ids: &[String], titles: &HashMap<&str, &str>) -> Vec<LinkRef> {
    ids.iter()
        .map(|id| {
            let target = id.split_once('#').map_or(id.as_str(), |(t, _)| t);
            LinkRef {
                id: id.clone(),
                title: titles.get(target).map(|t| (*t).to_string()),
            }
        })
        .collect()
}

/// stable id -> the tasks that implement it, in link order. The task side is
/// the authority for these links (`SubItem.task_ids` is only a derived view).
fn implementing_tasks(loaded: &LoadedTrace) -> HashMap<&str, Vec<TaskRef>> {
    let titles: HashMap<&str, &str> = loaded
        .tasks
        .iter()
        .map(|t| (t.id.as_str(), t.title.as_str()))
        .collect();
    let mut by_item: HashMap<&str, Vec<TaskRef>> = HashMap::new();
    for link in &loaded.trace_input.task_requirement_links {
        if link.role != TaskLinkRole::Implements {
            continue;
        }
        by_item
            .entry(link.stable_id.as_str())
            .or_default()
            .push(TaskRef {
                id: link.task_id.clone(),
                title: titles.get(link.task_id.as_str()).map(|t| (*t).to_string()),
                status: loaded.trace_input.task_status.get(&link.task_id).cloned(),
            });
    }
    by_item
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::docs::model::{SubItem, Verification, VerificationItem};

    fn doc_with(items: Vec<SubItem>) -> DocMetadata {
        let mut doc = DocMetadata::new(
            "doc-1".to_string(),
            "reqs".to_string(),
            "Reqs".to_string(),
            "spec".to_string(),
            "2026-10-27T00:00:00Z".to_string(),
        );
        doc.verification = Some(Verification {
            status: "pending".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            items: vec![VerificationItem {
                fragment_seq: Some(1),
                heading: "h".to_string(),
                status: "pending".to_string(),
                impl_refs: Vec::new(),
                test_refs: Vec::new(),
                reviewer: None,
                verified_at: None,
                notes: String::new(),
                content_hash_at_verify: None,
                category: "requirement".to_string(),
                sub_items: items,
                label: None,
            }],
        });
        doc
    }

    fn sub(id: Option<&str>, implicit_of: Option<&str>) -> SubItem {
        SubItem {
            stable_id: id.map(str::to_string),
            implicit_of: implicit_of.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn a_document_has_items_when_a_non_implicit_sub_item_has_a_stable_id() {
        assert!(has_items(&doc_with(vec![sub(Some("REQ-1"), None)])));
        assert!(!has_items(&doc_with(vec![sub(None, None)])));
        assert!(!has_items(&doc_with(vec![sub(
            Some("REQ-1#AC1"),
            Some("REQ-1")
        )])));
        let no_matrix = DocMetadata::new(
            "d".to_string(),
            "s".to_string(),
            "t".to_string(),
            "spec".to_string(),
            "2026-10-27T00:00:00Z".to_string(),
        );
        assert!(!has_items(&no_matrix));
    }

    #[test]
    fn documents_without_items_cost_no_trace_load_and_no_warning() {
        // The directory does not exist: loading it would fail, so reaching a
        // clean result proves nothing was loaded.
        let missing = Path::new("/nonexistent/handoff-dir");
        let plain = doc_with(vec![sub(None, None)]);
        let e = collect(missing, &[&plain]);
        assert!(e.index.is_empty());
        assert!(e.warning.is_none());
    }

    #[test]
    fn an_unloadable_trace_becomes_a_warning_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        // `docs` is a file, so the document set cannot be read.
        std::fs::write(tmp.path().join("docs"), "x").unwrap();
        let doc = doc_with(vec![sub(Some("REQ-1"), None)]);
        let e = collect(tmp.path(), &[&doc]);
        assert!(e.index.is_empty());
        let w = e.warning.expect("a warning");
        assert_eq!(w.code, PdfWarningCode::EnrichUnavailable);
        assert!(
            w.message.contains("trace data could not be loaded"),
            "{}",
            w.message
        );
    }

    #[test]
    fn coverage_maps_the_trace_status_and_keeps_the_ratio_only_for_partial() {
        let partial = coverage(Some(CoverageStatus::Partial), Some((1, 3))).unwrap();
        assert_eq!(partial.state, CoverageState::Partial);
        assert_eq!(partial.done, Some((1, 3)));
        let covered = coverage(Some(CoverageStatus::Covered), Some((3, 3))).unwrap();
        assert_eq!(covered.state, CoverageState::Covered);
        assert_eq!(covered.done, None);
        assert_eq!(
            coverage(Some(CoverageStatus::Waived), None).unwrap().state,
            CoverageState::Waived
        );
        assert_eq!(
            coverage(Some(CoverageStatus::Uncovered), None)
                .unwrap()
                .state,
            CoverageState::Uncovered
        );
        // Nothing to cover, or not in the graph: no bar.
        assert!(coverage(Some(CoverageStatus::Na), None).is_none());
        assert!(coverage(None, None).is_none());
    }

    #[test]
    fn link_titles_resolve_through_the_base_id_of_a_criterion_reference() {
        let titles = HashMap::from([("REQ-1", "Login")]);
        let refs = link_refs(
            &[
                "REQ-1".to_string(),
                "REQ-1#AC2".to_string(),
                "REQ-9".to_string(),
            ],
            &titles,
        );
        assert_eq!(refs[0].title.as_deref(), Some("Login"));
        assert_eq!(refs[1].title.as_deref(), Some("Login"));
        assert_eq!(refs[1].id, "REQ-1#AC2");
        assert_eq!(refs[2].title, None);
    }
}
