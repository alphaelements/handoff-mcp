use std::collections::HashMap;

use super::*;
use crate::storage::docs::model::{DocMetadata, SubItem, Verification, VerificationItem};
use crate::storage::runs::LatestItemResult;
use crate::storage::tasks::TaskLink;

fn sub_item(stable_id: &str, layer: Option<&str>) -> SubItem {
    SubItem {
        stable_id: Some(stable_id.to_string()),
        layer: layer.map(str::to_string),
        ..Default::default()
    }
}

fn doc_with_items(id: &str, layer: Option<&str>, sub_items: Vec<SubItem>) -> DocMetadata {
    let mut doc = DocMetadata::new(
        id.to_string(),
        format!("{id}-slug"),
        "Doc".to_string(),
        "spec".to_string(),
        "2026-09-27T00:00:00Z".to_string(),
    );
    doc.layer = layer.map(str::to_string);
    doc.verification = Some(Verification {
        status: "pending".to_string(),
        created_at: "2026-09-27T00:00:00Z".to_string(),
        updated_at: "2026-09-27T00:00:00Z".to_string(),
        items: vec![VerificationItem {
            fragment_seq: None,
            heading: "section".to_string(),
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
    doc
}

fn task(id: &str, task_links: Vec<TaskLink>) -> crate::storage::tasks::TaskData {
    crate::storage::tasks::TaskData {
        id: id.to_string(),
        title: id.to_string(),
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
        extra: HashMap::new(),
    }
}

#[test]
fn collect_trace_items_uses_item_layer_override_then_falls_back_to_doc_layer() {
    let doc = doc_with_items(
        "doc-1",
        Some("basic_spec"),
        vec![
            sub_item("SPEC-1", None),
            sub_item("AT-1", Some("acceptance")),
        ],
    );
    let items = collect_trace_items(&[doc]);
    let spec = items.iter().find(|i| i.stable_id == "SPEC-1").unwrap();
    assert_eq!(spec.layer.as_deref(), Some("basic_spec"));
    let at = items.iter().find(|i| i.stable_id == "AT-1").unwrap();
    assert_eq!(at.layer.as_deref(), Some("acceptance"));
}

#[test]
fn collect_layer_doc_ids_only_includes_docs_with_frontmatter_layer_set() {
    let with_layer = doc_with_items("doc-1", Some("requirement"), vec![]);
    let without_layer = doc_with_items("doc-2", None, vec![]);
    let ids = collect_layer_doc_ids(&[with_layer, without_layer]);
    assert!(ids.contains("doc-1"));
    assert!(!ids.contains("doc-2"));
}

#[test]
fn collect_task_links_splits_requirement_and_doc_links_with_role_default() {
    let t = task(
        "t1",
        vec![
            TaskLink {
                target: "doc-1".to_string(),
                link_type: "requirement".to_string(),
                label: Some("REQ-1".to_string()),
                role: None,
                baseline_hash: None,
            },
            TaskLink {
                target: "doc-1".to_string(),
                link_type: "requirement".to_string(),
                label: Some("AT-1".to_string()),
                role: Some("executes".to_string()),
                baseline_hash: None,
            },
            TaskLink {
                target: "doc-2".to_string(),
                link_type: "doc".to_string(),
                label: None,
                role: None,
                baseline_hash: None,
            },
        ],
    );
    let (req_links, doc_links) = collect_task_links(&[t]);
    assert_eq!(req_links.len(), 2);
    let implements = req_links.iter().find(|l| l.stable_id == "REQ-1").unwrap();
    assert_eq!(implements.role, TaskLinkRole::Implements);
    let executes = req_links.iter().find(|l| l.stable_id == "AT-1").unwrap();
    assert_eq!(executes.role, TaskLinkRole::Executes);
    assert_eq!(doc_links.len(), 1);
    assert_eq!(doc_links[0].doc_id, "doc-2");
}

#[test]
fn collect_runs_latest_flattens_the_latest_cache() {
    let mut cache = crate::storage::runs::LatestCache::default();
    cache.items.insert(
        "REQ-1".to_string(),
        LatestItemResult {
            result: "pass".to_string(),
            executed_at: "2026-09-27T00:00:00.000Z".to_string(),
            run_id: "run-1".to_string(),
            body_hash: None,
            def_hash: None,
            note: String::new(),
            evidence: Vec::new(),
            carried_from: None,
        },
    );
    let latest = collect_runs_latest(&cache);
    assert_eq!(latest.get("REQ-1").map(String::as_str), Some("pass"));
}
