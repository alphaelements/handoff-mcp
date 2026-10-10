//! V-model layer lifecycle status (FR-510 / SPEC-510).
//!
//! Each in-use layer carries one of five lifecycle statuses. Three are
//! *derived* purely from the layer's aggregate (`coverage.<layer>.state`
//! counts plus the number of gaps attributed to the layer); two
//! (`under_review`, `approved`) are *explicit* human operations persisted
//! outside this module (`storage::layer_status`) and only honoured while the
//! derived base status is `verified`. The moment a layer stops being
//! `verified` (a failing/blocked/not-run item or a gap appears) an explicit
//! record is demoted: the layer reports `in_progress` (or `not_started`) and
//! the caller is told to drop the record via [`LayerStatusReport::demoted`],
//! so fixing the regression later does not silently re-approve the layer.
//!
//! Pure functions only — no file I/O.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use super::types::{Gap, LayerCoverage};

/// Lifecycle status of one layer, ordered from least to most mature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerStatus {
    NotStarted,
    InProgress,
    UnderReview,
    Verified,
    Approved,
}

/// The two statuses that are set by an explicit operation rather than
/// derived (persisted by `storage::layer_status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    UnderReview,
    Approved,
}

impl From<ReviewStatus> for LayerStatus {
    fn from(s: ReviewStatus) -> Self {
        match s {
            ReviewStatus::UnderReview => LayerStatus::UnderReview,
            ReviewStatus::Approved => LayerStatus::Approved,
        }
    }
}

/// Whole-project status: the lowest layer status, with "every layer
/// approved" spelled `complete`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectStatus {
    NotStarted,
    InProgress,
    UnderReview,
    Verified,
    Complete,
}

/// Result of [`derive_layer_statuses`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerStatusReport {
    /// One entry per in-use layer, in the order the layers were given.
    pub statuses: Vec<(String, LayerStatus)>,
    /// In-use layers whose explicit record no longer applies (the layer is
    /// not `verified` any more) and must be removed from the store.
    pub demoted: Vec<String>,
}

/// The derived (explicit-record-free) status of one layer.
///
/// - no items at all -> `not_started`
/// - every item `passing` and no gap attributed to the layer -> `verified`
/// - anything else (a `not_run`/`failing`/`blocked`/`uncovered` item or a
///   gap) -> `in_progress`
pub fn derive_base_status(coverage: &LayerCoverage, layer_gap_count: usize) -> LayerStatus {
    if coverage.total == 0 {
        LayerStatus::NotStarted
    } else if coverage.state.passing == coverage.total && layer_gap_count == 0 {
        LayerStatus::Verified
    } else {
        LayerStatus::InProgress
    }
}

/// Derives every in-use layer's status, applying `explicit` records only to
/// layers whose base status is `verified` (see module docs).
pub fn derive_layer_statuses(
    in_use: &[String],
    coverage: &HashMap<String, LayerCoverage>,
    gaps: &[Gap],
    explicit: &BTreeMap<String, ReviewStatus>,
) -> LayerStatusReport {
    let mut statuses = Vec::with_capacity(in_use.len());
    let mut demoted = Vec::new();
    for layer in in_use {
        let layer_gaps = gaps
            .iter()
            .filter(|g| g.layer.as_deref() == Some(layer.as_str()))
            .count();
        let cov = coverage.get(layer).copied().unwrap_or_default();
        let base = derive_base_status(&cov, layer_gaps);
        let status = match explicit.get(layer) {
            Some(review) if base == LayerStatus::Verified => LayerStatus::from(*review),
            Some(_) => {
                demoted.push(layer.clone());
                base
            }
            None => base,
        };
        statuses.push((layer.clone(), status));
    }
    LayerStatusReport { statuses, demoted }
}

/// `complete` when every layer is `approved`; otherwise the lowest layer
/// status. No layers at all reads as `not_started`.
pub fn project_status(statuses: &[(String, LayerStatus)]) -> ProjectStatus {
    match statuses.iter().map(|(_, s)| *s).min() {
        None | Some(LayerStatus::NotStarted) => ProjectStatus::NotStarted,
        Some(LayerStatus::InProgress) => ProjectStatus::InProgress,
        Some(LayerStatus::UnderReview) => ProjectStatus::UnderReview,
        Some(LayerStatus::Verified) => ProjectStatus::Verified,
        Some(LayerStatus::Approved) => ProjectStatus::Complete,
    }
}

#[cfg(test)]
mod tests;
