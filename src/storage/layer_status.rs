//! Explicit layer review records (FR-510 / SPEC-510):
//! `.handoff/trace/layer_status.json` holds the layers a human (or agent)
//! moved to `under_review` / `approved`. The derived statuses
//! (`not_started` / `in_progress` / `verified`) are never stored — they are
//! recomputed from the trace graph on every `handoff_trace_report`; see
//! `crate::trace::layer_status` for how a record is honoured or demoted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::trace::layer_status::ReviewStatus;

const SCHEMA_VERSION: u32 = 1;

/// Who performed the operation (same shape as `approvals::ApprovalExecutor`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LayerStatusExecutor {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// One layer's explicit record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LayerReviewRecord {
    pub status: ReviewStatus,
    /// RFC3339 time the status was set.
    pub updated_at: String,
    pub executor: LayerStatusExecutor,
}

/// The whole file: layer id -> record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct LayerStatusStore {
    #[serde(default)]
    pub layers: BTreeMap<String, LayerReviewRecord>,
}

#[derive(Serialize, Deserialize)]
struct StoreFile {
    schema_version: u32,
    #[serde(default)]
    layers: BTreeMap<String, LayerReviewRecord>,
}

impl LayerStatusStore {
    /// layer id -> explicit status, the shape `derive_layer_statuses` takes.
    pub fn review_statuses(&self) -> BTreeMap<String, ReviewStatus> {
        self.layers
            .iter()
            .map(|(k, v)| (k.clone(), v.status))
            .collect()
    }
}

pub fn layer_status_path(handoff: &Path) -> PathBuf {
    handoff.join("trace").join("layer_status.json")
}

/// Reads the store. A missing file is an empty store (nothing was ever
/// approved); an unreadable or malformed file is an error — treating it as
/// empty would let the next write silently discard every approval.
pub fn read_layer_status_store(handoff: &Path) -> Result<LayerStatusStore> {
    let path = layer_status_path(handoff);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LayerStatusStore::default())
        }
        Err(e) => return Err(e).with_context(|| format!("Failed to read {}", path.display())),
    };
    let file: StoreFile = serde_json::from_slice(&bytes)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(LayerStatusStore {
        layers: file.layers,
    })
}

/// Atomically writes the store; an empty store removes the file instead.
pub fn write_layer_status_store(handoff: &Path, store: &LayerStatusStore) -> Result<()> {
    let path = layer_status_path(handoff);
    if store.layers.is_empty() {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("Failed to remove {}", path.display())),
        };
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create dir: {}", parent.display()))?;
    }
    let body = serde_json::to_string_pretty(&StoreFile {
        schema_version: SCHEMA_VERSION,
        layers: store.layers.clone(),
    })
    .context("Failed to serialize layer_status.json")?;
    super::atomic_write(&path, body.as_bytes())
        .with_context(|| format!("Failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(status: ReviewStatus) -> LayerReviewRecord {
        LayerReviewRecord {
            status,
            updated_at: "2026-10-08T00:00:00Z".to_string(),
            executor: LayerStatusExecutor {
                kind: "human".to_string(),
                id: Some("ryoma".to_string()),
            },
        }
    }

    #[test]
    fn missing_file_reads_as_an_empty_store() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            read_layer_status_store(tmp.path()).unwrap(),
            LayerStatusStore::default()
        );
    }

    #[test]
    fn store_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = LayerStatusStore::default();
        store
            .layers
            .insert("requirement".to_string(), record(ReviewStatus::Approved));
        write_layer_status_store(tmp.path(), &store).unwrap();
        assert_eq!(read_layer_status_store(tmp.path()).unwrap(), store);
        assert_eq!(
            store.review_statuses().get("requirement"),
            Some(&ReviewStatus::Approved)
        );
    }

    #[test]
    fn writing_an_empty_store_removes_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = LayerStatusStore::default();
        store
            .layers
            .insert("design".to_string(), record(ReviewStatus::UnderReview));
        write_layer_status_store(tmp.path(), &store).unwrap();
        assert!(layer_status_path(tmp.path()).exists());
        write_layer_status_store(tmp.path(), &LayerStatusStore::default()).unwrap();
        assert!(!layer_status_path(tmp.path()).exists());
        write_layer_status_store(tmp.path(), &LayerStatusStore::default()).unwrap();
    }

    #[test]
    fn malformed_file_is_an_error_not_an_empty_store() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("trace")).unwrap();
        std::fs::write(layer_status_path(tmp.path()), b"{not json").unwrap();
        assert!(read_layer_status_store(tmp.path()).is_err());
    }
}
