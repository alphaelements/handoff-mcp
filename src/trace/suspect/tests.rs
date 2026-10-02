//! Unit tests for [`super::compute`] (wiki/260-vmodel-m2-design.md §3.2,
//! M2-05): the 3 suspect kinds, unbaselined, and reverify.

use std::collections::{BTreeMap, HashMap};

use super::*;
use crate::trace::types::{TaskLinkRole, TaskRequirementLink};

fn item(id: &str) -> TraceItemInput {
    TraceItemInput {
        stable_id: id.to_string(),
        doc_id: "doc".to_string(),
        layer: Some("requirement".to_string()),
        refines: Vec::new(),
        verifies: Vec::new(),
        method: None,
        has_test_refs: false,
        acceptance_labels: Vec::new(),
        derived: false,
        waived_axes: Vec::new(),
        def_hash: None,
        body_hash: None,
        link_baselines: BTreeMap::new(),
        needs: None,
    }
}

fn states(pairs: &[(&str, ItemState)]) -> HashMap<String, ItemState> {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

#[test]
fn link_suspect_when_upstream_def_hash_changed_since_baseline() {
    let upstream = TraceItemInput {
        def_hash: Some("new-hash".to_string()),
        ..item("REQ-001")
    };
    let mut baselines = BTreeMap::new();
    baselines.insert("REQ-001".to_string(), "old-hash".to_string());
    let child = TraceItemInput {
        refines: vec!["REQ-001".to_string()],
        link_baselines: baselines,
        ..item("SPEC-001")
    };
    let input = TraceInput {
        items: vec![upstream, child],
        ..Default::default()
    };

    let out = compute(&input, &HashMap::new());

    assert_eq!(out.suspects.len(), 1);
    let s = &out.suspects[0];
    assert_eq!(s.kind, SuspectKind::Link);
    assert_eq!(s.item, "SPEC-001");
    assert_eq!(s.upstream.as_deref(), Some("REQ-001"));
    assert_eq!(s.link_type.as_deref(), Some("refines"));
    assert_eq!(s.baseline_hash, "old-hash");
    assert_eq!(s.current_hash, "new-hash");
    assert_eq!(out.unbaselined, UnbaselinedCounts::default());
}

#[test]
fn link_not_suspect_when_current_hash_matches_baseline() {
    let upstream = TraceItemInput {
        def_hash: Some("same-hash".to_string()),
        ..item("REQ-001")
    };
    let mut baselines = BTreeMap::new();
    baselines.insert("REQ-001".to_string(), "same-hash".to_string());
    let child = TraceItemInput {
        refines: vec!["REQ-001".to_string()],
        link_baselines: baselines,
        ..item("SPEC-001")
    };
    let input = TraceInput {
        items: vec![upstream, child],
        ..Default::default()
    };

    let out = compute(&input, &HashMap::new());

    assert!(out.suspects.is_empty());
}

#[test]
fn unbaselined_link_is_not_a_suspect() {
    // A reference present in `refines` but absent from `link_baselines`
    // (e.g. an M1-era link, §7) is counted as unbaselined, never a suspect.
    let upstream = TraceItemInput {
        def_hash: Some("hash".to_string()),
        ..item("REQ-001")
    };
    let child = TraceItemInput {
        refines: vec!["REQ-001".to_string()],
        ..item("SPEC-001")
    };
    let input = TraceInput {
        items: vec![upstream, child],
        ..Default::default()
    };

    let out = compute(&input, &HashMap::new());

    assert!(out.suspects.is_empty());
    assert_eq!(out.unbaselined.links, 1);
    assert_eq!(out.unbaselined_links.len(), 1);
    assert_eq!(out.unbaselined_links[0].item, "SPEC-001");
    assert_eq!(out.unbaselined_links[0].upstream, "REQ-001");
    assert_eq!(
        out.unbaselined_links[0].current_hash.as_deref(),
        Some("hash")
    );
}

#[test]
fn ac_level_link_resolves_against_the_implicit_items_def_hash() {
    // `REQ-001#AC1`'s own `def_hash` mirrors `ac_hash(REQ-001, AC1)` by
    // construction (§2.4/§2.5 step 4) — an AC-level reference's *current*
    // hash resolves through that implicit item, not through the parent's
    // own (whole-item) `def_hash`.
    let parent = TraceItemInput {
        def_hash: Some("parent-hash".to_string()),
        acceptance_labels: vec!["AC1".to_string()],
        ..item("REQ-001")
    };
    let implicit = TraceItemInput {
        def_hash: Some("ac1-hash-v2".to_string()),
        ..item("REQ-001#AC1")
    };
    let mut baselines = BTreeMap::new();
    baselines.insert("REQ-001#AC1".to_string(), "ac1-hash-v1".to_string());
    let verifier = TraceItemInput {
        verifies: vec!["REQ-001#AC1".to_string()],
        link_baselines: baselines,
        ..item("AT-001")
    };
    let input = TraceInput {
        items: vec![parent, implicit, verifier],
        ..Default::default()
    };

    let out = compute(&input, &HashMap::new());

    assert_eq!(out.suspects.len(), 1);
    let s = &out.suspects[0];
    assert_eq!(s.kind, SuspectKind::Link);
    assert_eq!(s.item, "AT-001");
    assert_eq!(s.upstream.as_deref(), Some("REQ-001#AC1"));
    assert_eq!(s.baseline_hash, "ac1-hash-v1");
    assert_eq!(s.current_hash, "ac1-hash-v2");
}

#[test]
fn ac_level_link_cannot_be_determined_without_an_implicit_item() {
    // implicit_acceptance disabled (or the implicit item was never synced):
    // no `REQ-001#AC1` item exists to resolve the current ac_hash against —
    // "can't determine", not a suspect, and not unbaselined either (it *has*
    // a baseline, we just can't check it right now).
    let parent = TraceItemInput {
        def_hash: Some("parent-hash".to_string()),
        acceptance_labels: vec!["AC1".to_string()],
        ..item("REQ-001")
    };
    let mut baselines = BTreeMap::new();
    baselines.insert("REQ-001#AC1".to_string(), "ac1-hash-v1".to_string());
    let verifier = TraceItemInput {
        verifies: vec!["REQ-001#AC1".to_string()],
        link_baselines: baselines,
        ..item("AT-001")
    };
    let input = TraceInput {
        items: vec![parent, verifier],
        ..Default::default()
    };

    let out = compute(&input, &HashMap::new());

    assert!(out.suspects.is_empty());
    assert_eq!(out.unbaselined, UnbaselinedCounts::default());
}

#[test]
fn task_suspect_when_linked_items_def_hash_changed() {
    let item = TraceItemInput {
        def_hash: Some("v2".to_string()),
        ..item("REQ-001")
    };
    let input = TraceInput {
        items: vec![item],
        task_requirement_links: vec![TaskRequirementLink {
            task_id: "t1".to_string(),
            stable_id: "REQ-001".to_string(),
            role: TaskLinkRole::Implements,
            baseline_hash: Some("v1".to_string()),
        }],
        ..Default::default()
    };

    let out = compute(&input, &HashMap::new());

    assert_eq!(out.suspects.len(), 1);
    let s = &out.suspects[0];
    assert_eq!(s.kind, SuspectKind::Task);
    assert_eq!(s.item, "REQ-001");
    assert_eq!(s.task.as_deref(), Some("t1"));
    assert_eq!(s.baseline_hash, "v1");
    assert_eq!(s.current_hash, "v2");
}

#[test]
fn unbaselined_task_link_is_not_a_suspect() {
    let item = TraceItemInput {
        def_hash: Some("v1".to_string()),
        ..item("REQ-001")
    };
    let input = TraceInput {
        items: vec![item],
        task_requirement_links: vec![TaskRequirementLink {
            task_id: "t1".to_string(),
            stable_id: "REQ-001".to_string(),
            role: TaskLinkRole::Implements,
            baseline_hash: None,
        }],
        ..Default::default()
    };

    let out = compute(&input, &HashMap::new());

    assert!(out.suspects.is_empty());
    assert_eq!(out.unbaselined.tasks, 1);
    assert_eq!(out.unbaselined_tasks[0].task_id, "t1");
    assert_eq!(out.unbaselined_tasks[0].item, "REQ-001");
    assert_eq!(out.unbaselined_tasks[0].current_hash.as_deref(), Some("v1"));
}

#[test]
fn result_suspect_when_passing_runs_def_hash_no_longer_matches() {
    let verifier = TraceItemInput {
        def_hash: Some("v2".to_string()),
        ..item("AT-001")
    };
    let mut runs_latest = HashMap::new();
    runs_latest.insert("AT-001".to_string(), "pass".to_string());
    let mut runs_latest_hashes = HashMap::new();
    runs_latest_hashes.insert(
        "AT-001".to_string(),
        RunResultHashes {
            def_hash: Some("v1".to_string()),
            body_hash: None,
        },
    );
    let input = TraceInput {
        items: vec![verifier],
        runs_latest,
        runs_latest_hashes,
        ..Default::default()
    };

    let out = compute(&input, &HashMap::new());

    assert_eq!(out.suspects.len(), 1);
    let s = &out.suspects[0];
    assert_eq!(s.kind, SuspectKind::Result);
    assert_eq!(s.item, "AT-001");
    assert_eq!(s.baseline_hash, "v1");
    assert_eq!(s.current_hash, "v2");
}

#[test]
fn result_not_suspect_when_latest_is_not_pass() {
    let verifier = TraceItemInput {
        def_hash: Some("v2".to_string()),
        ..item("AT-001")
    };
    let mut runs_latest = HashMap::new();
    runs_latest.insert("AT-001".to_string(), "fail".to_string());
    let mut runs_latest_hashes = HashMap::new();
    runs_latest_hashes.insert(
        "AT-001".to_string(),
        RunResultHashes {
            def_hash: Some("v1".to_string()),
            body_hash: None,
        },
    );
    let input = TraceInput {
        items: vec![verifier],
        runs_latest,
        runs_latest_hashes,
        ..Default::default()
    };

    let out = compute(&input, &HashMap::new());

    assert!(out.suspects.is_empty());
}

#[test]
fn result_falls_back_to_body_hash_when_recorded_def_hash_absent() {
    // A pre-M2-02 run never had a `def_hash` at all (§7/E13).
    let verifier = TraceItemInput {
        body_hash: Some("b2".to_string()),
        ..item("AT-001")
    };
    let mut runs_latest = HashMap::new();
    runs_latest.insert("AT-001".to_string(), "pass".to_string());
    let mut runs_latest_hashes = HashMap::new();
    runs_latest_hashes.insert(
        "AT-001".to_string(),
        RunResultHashes {
            def_hash: None,
            body_hash: Some("b1".to_string()),
        },
    );
    let input = TraceInput {
        items: vec![verifier],
        runs_latest,
        runs_latest_hashes,
        ..Default::default()
    };

    let out = compute(&input, &HashMap::new());

    assert_eq!(out.suspects.len(), 1);
    assert_eq!(out.suspects[0].baseline_hash, "b1");
    assert_eq!(out.suspects[0].current_hash, "b2");
}

#[test]
fn reverify_when_passing_item_has_its_own_result_suspect() {
    let verifier = TraceItemInput {
        def_hash: Some("v2".to_string()),
        ..item("AT-001")
    };
    let mut runs_latest = HashMap::new();
    runs_latest.insert("AT-001".to_string(), "pass".to_string());
    let mut runs_latest_hashes = HashMap::new();
    runs_latest_hashes.insert(
        "AT-001".to_string(),
        RunResultHashes {
            def_hash: Some("v1".to_string()),
            body_hash: None,
        },
    );
    let input = TraceInput {
        items: vec![verifier],
        runs_latest,
        runs_latest_hashes,
        ..Default::default()
    };

    let out = compute(&input, &states(&[("AT-001", ItemState::Passing)]));

    assert!(out.reverify.contains("AT-001"));
}

#[test]
fn reverify_when_passing_verifier_has_a_suspect_verifies_link() {
    let upstream = TraceItemInput {
        def_hash: Some("new-hash".to_string()),
        ..item("REQ-001")
    };
    let mut baselines = BTreeMap::new();
    baselines.insert("REQ-001".to_string(), "old-hash".to_string());
    let verifier = TraceItemInput {
        verifies: vec!["REQ-001".to_string()],
        link_baselines: baselines,
        ..item("AT-001")
    };
    let input = TraceInput {
        items: vec![upstream, verifier],
        ..Default::default()
    };

    let out = compute(&input, &states(&[("AT-001", ItemState::Passing)]));

    assert!(out.reverify.contains("AT-001"));
}

#[test]
fn reverify_not_set_for_a_refines_only_link_suspect_on_a_passing_item() {
    // FR-402: reverify only looks at `verifies` links, not `refines`.
    let upstream = TraceItemInput {
        def_hash: Some("new-hash".to_string()),
        ..item("REQ-001")
    };
    let mut baselines = BTreeMap::new();
    baselines.insert("REQ-001".to_string(), "old-hash".to_string());
    let child = TraceItemInput {
        refines: vec!["REQ-001".to_string()],
        link_baselines: baselines,
        ..item("SPEC-001")
    };
    let input = TraceInput {
        items: vec![upstream, child],
        ..Default::default()
    };

    let out = compute(&input, &states(&[("SPEC-001", ItemState::Passing)]));

    assert!(!out.reverify.contains("SPEC-001"));
}

#[test]
fn reverify_not_set_when_item_is_not_passing() {
    let verifier = TraceItemInput {
        def_hash: Some("v2".to_string()),
        ..item("AT-001")
    };
    let mut runs_latest = HashMap::new();
    runs_latest.insert("AT-001".to_string(), "pass".to_string());
    let mut runs_latest_hashes = HashMap::new();
    runs_latest_hashes.insert(
        "AT-001".to_string(),
        RunResultHashes {
            def_hash: Some("v1".to_string()),
            body_hash: None,
        },
    );
    let input = TraceInput {
        items: vec![verifier],
        runs_latest,
        runs_latest_hashes,
        ..Default::default()
    };

    // Not marked Passing in `states` (e.g. failing elsewhere) — not
    // eligible for reverify regardless of its own result suspect.
    let out = compute(&input, &states(&[("AT-001", ItemState::Failing)]));

    assert!(!out.reverify.contains("AT-001"));
}

#[test]
fn dangling_refines_target_is_neither_suspect_nor_unbaselined() {
    let child = TraceItemInput {
        refines: vec!["REQ-999".to_string()],
        ..item("SPEC-001")
    };
    let input = TraceInput {
        items: vec![child],
        ..Default::default()
    };

    let out = compute(&input, &HashMap::new());

    assert!(out.suspects.is_empty());
    assert_eq!(out.unbaselined, UnbaselinedCounts::default());
}
