use std::collections::{BTreeMap, HashMap};

use super::*;
use crate::trace::types::{Gap, GapKind, LayerCoverage, StateCounts};

fn cov(total: usize, state: StateCounts) -> LayerCoverage {
    LayerCoverage {
        total,
        state,
        ..LayerCoverage::default()
    }
}

fn all_passing(n: usize) -> LayerCoverage {
    cov(
        n,
        StateCounts {
            passing: n,
            ..StateCounts::default()
        },
    )
}

fn gap(layer: &str) -> Gap {
    Gap {
        kind: GapKind::Unverified,
        item: Some("REQ-001".to_string()),
        layer: Some(layer.to_string()),
        detail: "x".to_string(),
    }
}

fn layers(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

#[test]
fn base_status_is_not_started_when_the_layer_has_no_items() {
    assert_eq!(
        derive_base_status(&LayerCoverage::default(), 0),
        LayerStatus::NotStarted
    );
}

#[test]
fn base_status_is_in_progress_when_any_item_is_not_run_failing_or_blocked() {
    for state in [
        StateCounts {
            passing: 2,
            not_run: 1,
            ..StateCounts::default()
        },
        StateCounts {
            passing: 2,
            failing: 1,
            ..StateCounts::default()
        },
        StateCounts {
            passing: 2,
            blocked: 1,
            ..StateCounts::default()
        },
    ] {
        assert_eq!(
            derive_base_status(&cov(3, state), 0),
            LayerStatus::InProgress
        );
    }
}

#[test]
fn base_status_is_in_progress_when_an_item_is_uncovered() {
    let c = cov(
        3,
        StateCounts {
            passing: 2,
            uncovered: 1,
            ..StateCounts::default()
        },
    );
    assert_eq!(derive_base_status(&c, 0), LayerStatus::InProgress);
}

#[test]
fn base_status_is_verified_only_when_all_passing_and_gap_free() {
    assert_eq!(
        derive_base_status(&all_passing(3), 0),
        LayerStatus::Verified
    );
    assert_eq!(
        derive_base_status(&all_passing(3), 1),
        LayerStatus::InProgress
    );
}

#[test]
fn explicit_approval_is_honoured_on_a_verified_layer() {
    let coverage = HashMap::from([("requirement".to_string(), all_passing(2))]);
    let explicit = BTreeMap::from([("requirement".to_string(), ReviewStatus::Approved)]);
    let r = derive_layer_statuses(&layers(&["requirement"]), &coverage, &[], &explicit);
    assert_eq!(
        r.statuses,
        vec![("requirement".to_string(), LayerStatus::Approved)]
    );
    assert!(r.demoted.is_empty());
}

#[test]
fn explicit_under_review_is_honoured_on_a_verified_layer() {
    let coverage = HashMap::from([("requirement".to_string(), all_passing(2))]);
    let explicit = BTreeMap::from([("requirement".to_string(), ReviewStatus::UnderReview)]);
    let r = derive_layer_statuses(&layers(&["requirement"]), &coverage, &[], &explicit);
    assert_eq!(r.statuses[0].1, LayerStatus::UnderReview);
}

#[test]
fn approved_layer_is_demoted_to_in_progress_when_a_failing_item_appears() {
    let coverage = HashMap::from([(
        "requirement".to_string(),
        cov(
            2,
            StateCounts {
                passing: 1,
                failing: 1,
                ..StateCounts::default()
            },
        ),
    )]);
    let explicit = BTreeMap::from([("requirement".to_string(), ReviewStatus::Approved)]);
    let r = derive_layer_statuses(&layers(&["requirement"]), &coverage, &[], &explicit);
    assert_eq!(r.statuses[0].1, LayerStatus::InProgress);
    assert_eq!(r.demoted, vec!["requirement".to_string()]);
}

#[test]
fn approved_layer_is_demoted_when_a_gap_appears() {
    let coverage = HashMap::from([("requirement".to_string(), all_passing(2))]);
    let explicit = BTreeMap::from([("requirement".to_string(), ReviewStatus::Approved)]);
    let r = derive_layer_statuses(
        &layers(&["requirement"]),
        &coverage,
        &[gap("requirement")],
        &explicit,
    );
    assert_eq!(r.statuses[0].1, LayerStatus::InProgress);
    assert_eq!(r.demoted, vec!["requirement".to_string()]);
}

#[test]
fn gaps_of_other_layers_do_not_affect_a_layer() {
    let coverage = HashMap::from([
        ("requirement".to_string(), all_passing(2)),
        ("design".to_string(), all_passing(1)),
    ]);
    let r = derive_layer_statuses(
        &layers(&["requirement", "design"]),
        &coverage,
        &[gap("design")],
        &BTreeMap::new(),
    );
    assert_eq!(r.statuses[0].1, LayerStatus::Verified);
    assert_eq!(r.statuses[1].1, LayerStatus::InProgress);
}

#[test]
fn explicit_record_on_an_empty_layer_is_demoted_to_not_started() {
    let explicit = BTreeMap::from([("requirement".to_string(), ReviewStatus::Approved)]);
    let r = derive_layer_statuses(&layers(&["requirement"]), &HashMap::new(), &[], &explicit);
    assert_eq!(r.statuses[0].1, LayerStatus::NotStarted);
    assert_eq!(r.demoted, vec!["requirement".to_string()]);
}

#[test]
fn explicit_record_for_a_layer_not_in_use_is_ignored_and_not_demoted() {
    let explicit = BTreeMap::from([("ghost".to_string(), ReviewStatus::Approved)]);
    let r = derive_layer_statuses(&layers(&["requirement"]), &HashMap::new(), &[], &explicit);
    assert_eq!(r.statuses.len(), 1);
    assert!(r.demoted.is_empty());
}

#[test]
fn project_status_is_complete_only_when_every_layer_is_approved() {
    let s = vec![
        ("a".to_string(), LayerStatus::Approved),
        ("b".to_string(), LayerStatus::Approved),
    ];
    assert_eq!(project_status(&s), ProjectStatus::Complete);
}

#[test]
fn project_status_follows_the_lowest_layer_status() {
    let s = vec![
        ("a".to_string(), LayerStatus::Approved),
        ("b".to_string(), LayerStatus::Verified),
    ];
    assert_eq!(project_status(&s), ProjectStatus::Verified);
    let s = vec![
        ("a".to_string(), LayerStatus::Verified),
        ("b".to_string(), LayerStatus::InProgress),
        ("c".to_string(), LayerStatus::NotStarted),
    ];
    assert_eq!(project_status(&s), ProjectStatus::NotStarted);
    let s = vec![
        ("a".to_string(), LayerStatus::UnderReview),
        ("b".to_string(), LayerStatus::Approved),
    ];
    assert_eq!(project_status(&s), ProjectStatus::UnderReview);
}

#[test]
fn project_status_with_no_layers_is_not_started() {
    assert_eq!(project_status(&[]), ProjectStatus::NotStarted);
}

#[test]
fn statuses_serialize_as_snake_case() {
    assert_eq!(
        serde_json::to_string(&LayerStatus::UnderReview).unwrap(),
        "\"under_review\""
    );
    assert_eq!(
        serde_json::to_string(&ProjectStatus::Complete).unwrap(),
        "\"complete\""
    );
}
