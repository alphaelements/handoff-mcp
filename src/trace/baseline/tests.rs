use super::*;
use serde_json::json;

fn item(id: &str, hash: &str) -> BaselineItemKey {
    BaselineItemKey {
        id: id.to_string(),
        def_hash: Some(hash.to_string()),
    }
}

fn snapshot(
    items: Vec<BaselineItemKey>,
    coverage_summary: Value,
    state_summary: Value,
) -> BaselineSnapshot {
    BaselineSnapshot {
        items,
        coverage_summary,
        state_summary,
    }
}

fn empty_coverage() -> Value {
    json!({})
}

fn empty_state() -> Value {
    json!({})
}

#[test]
fn added_lists_items_only_present_in_to() {
    let from = snapshot(vec![item("REQ-001", "h1")], empty_coverage(), empty_state());
    let to = snapshot(
        vec![item("REQ-001", "h1"), item("REQ-002", "h2")],
        empty_coverage(),
        empty_state(),
    );

    let diff = diff_snapshots(&from, &to);
    assert_eq!(diff.added, vec!["REQ-002".to_string()]);
    assert!(diff.removed.is_empty());
    assert!(diff.changed.is_empty());
}

#[test]
fn removed_lists_items_only_present_in_from() {
    let from = snapshot(
        vec![item("REQ-001", "h1"), item("REQ-002", "h2")],
        empty_coverage(),
        empty_state(),
    );
    let to = snapshot(vec![item("REQ-001", "h1")], empty_coverage(), empty_state());

    let diff = diff_snapshots(&from, &to);
    assert_eq!(diff.removed, vec!["REQ-002".to_string()]);
    assert!(diff.added.is_empty());
    assert!(diff.changed.is_empty());
}

#[test]
fn changed_lists_items_present_on_both_sides_with_a_different_def_hash() {
    let from = snapshot(vec![item("REQ-001", "h1")], empty_coverage(), empty_state());
    let to = snapshot(vec![item("REQ-001", "h2")], empty_coverage(), empty_state());

    let diff = diff_snapshots(&from, &to);
    assert_eq!(diff.changed.len(), 1);
    assert_eq!(diff.changed[0].id, "REQ-001");
    assert_eq!(diff.changed[0].old_hash.as_deref(), Some("h1"));
    assert_eq!(diff.changed[0].new_hash.as_deref(), Some("h2"));
    assert!(diff.added.is_empty());
    assert!(diff.removed.is_empty());
}

#[test]
fn changed_is_empty_when_def_hash_is_identical() {
    let from = snapshot(vec![item("REQ-001", "h1")], empty_coverage(), empty_state());
    let to = snapshot(vec![item("REQ-001", "h1")], empty_coverage(), empty_state());

    let diff = diff_snapshots(&from, &to);
    assert!(diff.changed.is_empty());
}

#[test]
fn state_changes_only_reports_states_whose_count_actually_moved() {
    let from_state =
        json!({"passing": 5, "failing": 1, "blocked": 0, "not_run": 2, "uncovered": 0});
    let to_state = json!({"passing": 7, "failing": 0, "blocked": 0, "not_run": 2, "uncovered": 0});
    let from = snapshot(vec![], empty_coverage(), from_state);
    let to = snapshot(vec![], empty_coverage(), to_state);

    let diff = diff_snapshots(&from, &to);
    assert_eq!(diff.state_changes.get("passing"), Some(&2));
    assert_eq!(diff.state_changes.get("failing"), Some(&-1));
    assert!(!diff.state_changes.contains_key("blocked"));
    assert!(!diff.state_changes.contains_key("not_run"));
    assert!(!diff.state_changes.contains_key("uncovered"));
}

/// E20: a layer's axis regresses when its `covered` *ratio* drops, even if
/// the raw `covered` count stays the same (because total item count grew).
#[test]
fn regression_flags_a_layer_axis_whose_covered_ratio_dropped() {
    let from_cov = json!({
        "requirement": {
            "horizontal": {"covered": 8, "partial": 2, "uncovered": 0, "waived": 0, "na": 0},
            "vertical": {"covered": 10, "partial": 0, "uncovered": 0, "waived": 0, "na": 0}
        }
    });
    // horizontal: covered ratio drops from 0.8 to 8/12 = 0.667 (more
    // uncovered items added) even though the raw covered count (8) didn't
    // change. vertical stays at 1.0 (not a regression).
    let to_cov = json!({
        "requirement": {
            "horizontal": {"covered": 8, "partial": 2, "uncovered": 2, "waived": 0, "na": 0},
            "vertical": {"covered": 10, "partial": 0, "uncovered": 0, "waived": 0, "na": 0}
        }
    });
    let from = snapshot(vec![], from_cov, empty_state());
    let to = snapshot(vec![], to_cov, empty_state());

    let diff = diff_snapshots(&from, &to);
    assert_eq!(diff.regression.len(), 1, "{:?}", diff.regression);
    assert_eq!(diff.regression[0].layer, "requirement");
    assert_eq!(diff.regression[0].axis, "horizontal");
    assert!((diff.regression[0].old_pct - 0.8).abs() < 1e-9);
    assert!((diff.regression[0].new_pct - (8.0 / 12.0)).abs() < 1e-9);
}

/// E20: an increase in `partial` at the expense of `covered` is also a
/// regression (the design note: "partial の増加も退行とみなす（covered が
/// 減るため）") — covered count itself dropping is the simplest case.
#[test]
fn regression_flags_covered_decreasing_in_favor_of_partial() {
    let from_cov = json!({
        "acceptance": {
            "horizontal": {"covered": 10, "partial": 0, "uncovered": 0, "waived": 0, "na": 0},
            "vertical": {"covered": 10, "partial": 0, "uncovered": 0, "waived": 0, "na": 0}
        }
    });
    let to_cov = json!({
        "acceptance": {
            "horizontal": {"covered": 8, "partial": 2, "uncovered": 0, "waived": 0, "na": 0},
            "vertical": {"covered": 10, "partial": 0, "uncovered": 0, "waived": 0, "na": 0}
        }
    });
    let from = snapshot(vec![], from_cov, empty_state());
    let to = snapshot(vec![], to_cov, empty_state());

    let diff = diff_snapshots(&from, &to);
    assert_eq!(diff.regression.len(), 1);
    assert_eq!(diff.regression[0].axis, "horizontal");
}

#[test]
fn regression_is_empty_when_coverage_ratio_improves_or_stays_equal() {
    let from_cov = json!({
        "requirement": {
            "horizontal": {"covered": 5, "partial": 0, "uncovered": 5, "waived": 0, "na": 0},
            "vertical": {"covered": 5, "partial": 0, "uncovered": 5, "waived": 0, "na": 0}
        }
    });
    let to_cov = json!({
        "requirement": {
            "horizontal": {"covered": 10, "partial": 0, "uncovered": 0, "waived": 0, "na": 0},
            "vertical": {"covered": 5, "partial": 0, "uncovered": 5, "waived": 0, "na": 0}
        }
    });
    let from = snapshot(vec![], from_cov, empty_state());
    let to = snapshot(vec![], to_cov, empty_state());

    let diff = diff_snapshots(&from, &to);
    assert!(diff.regression.is_empty(), "{:?}", diff.regression);
}

/// A layer/axis with zero items on the `to` side (nothing to compute a
/// ratio over) must never be reported as a regression.
#[test]
fn regression_skips_an_axis_with_zero_items_on_the_to_side() {
    let from_cov = json!({
        "requirement": {
            "horizontal": {"covered": 5, "partial": 0, "uncovered": 0, "waived": 0, "na": 0}
        }
    });
    let to_cov = json!({
        "requirement": {
            "horizontal": {"covered": 0, "partial": 0, "uncovered": 0, "waived": 0, "na": 0}
        }
    });
    let from = snapshot(vec![], from_cov, empty_state());
    let to = snapshot(vec![], to_cov, empty_state());

    let diff = diff_snapshots(&from, &to);
    assert!(diff.regression.is_empty());
}

#[test]
fn diff_of_identical_snapshots_is_empty() {
    let cov = json!({
        "requirement": {
            "horizontal": {"covered": 5, "partial": 0, "uncovered": 0, "waived": 0, "na": 0},
            "vertical": {"covered": 5, "partial": 0, "uncovered": 0, "waived": 0, "na": 0}
        }
    });
    let state = json!({"passing": 5, "failing": 0, "blocked": 0, "not_run": 0, "uncovered": 0});
    let items = vec![item("REQ-001", "h1"), item("REQ-002", "h2")];

    let from = snapshot(items.clone(), cov.clone(), state.clone());
    let to = snapshot(items, cov, state);

    let diff = diff_snapshots(&from, &to);
    assert!(diff.added.is_empty());
    assert!(diff.removed.is_empty());
    assert!(diff.changed.is_empty());
    assert!(diff.regression.is_empty());
    assert!(diff.state_changes.is_empty());
}
