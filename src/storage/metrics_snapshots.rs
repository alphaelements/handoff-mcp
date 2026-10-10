//! Reader for the daily metrics snapshots in
//! `.handoff/metrics_snapshots/<YYYY-MM-DD>.json` (FR-503 / SPEC-503; written
//! by `handoff_snapshot_metrics` and `handoff_save_context`).

use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

/// Directory (under `.handoff/`) holding one metrics snapshot per UTC day.
pub const SNAPSHOT_DIR: &str = "metrics_snapshots";

/// Snapshots read from disk plus a note for every file that could not be used.
#[derive(Debug, Default)]
pub struct SnapshotRead {
    /// Envelopes (`{schema_version, date, captured_at, metrics}`), oldest
    /// `date` first.
    pub snapshots: Vec<Value>,
    /// One message per unreadable, malformed, or date-less snapshot file.
    pub warnings: Vec<String>,
}

/// Reads every `*.json` snapshot. A missing directory means no snapshots; an
/// unusable file is reported in `warnings` and skipped, since a trend report
/// is still meaningful without one day's point.
pub fn read_snapshots(handoff_dir: &Path) -> Result<SnapshotRead> {
    let dir = handoff_dir.join(SNAPSHOT_DIR);
    let mut out = SnapshotRead::default();
    if !dir.is_dir() {
        return Ok(out);
    }
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(&dir)
        .with_context(|| format!("Failed to read snapshots dir: {}", dir.display()))?
    {
        let path = entry
            .with_context(|| format!("Failed to read snapshots dir: {}", dir.display()))?
            .path();
        if path.extension().is_some_and(|e| e == "json") && path.is_file() {
            paths.push(path);
        }
    }
    paths.sort();

    for path in paths {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let parsed = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|s| serde_json::from_str::<Value>(&s).map_err(|e| e.to_string()));
        match parsed {
            Ok(v) if v.get("date").and_then(Value::as_str).is_some() => out.snapshots.push(v),
            Ok(_) => out.warnings.push(format!(
                "metrics snapshot {name} has no 'date' and was skipped"
            )),
            Err(e) => out.warnings.push(format!(
                "metrics snapshot {name} is unreadable ({e}) and was skipped"
            )),
        }
    }
    out.snapshots
        .sort_by(|a, b| a["date"].as_str().cmp(&b["date"].as_str()));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn put(dir: &Path, name: &str, body: &str) {
        let d = dir.join(SNAPSHOT_DIR);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(name), body).unwrap();
    }

    #[test]
    fn missing_directory_means_no_snapshots() {
        let tmp = tempfile::tempdir().unwrap();
        let read = read_snapshots(tmp.path()).unwrap();
        assert!(read.snapshots.is_empty() && read.warnings.is_empty());
    }

    #[test]
    fn snapshots_are_sorted_by_date_and_bad_files_become_warnings() {
        let tmp = tempfile::tempdir().unwrap();
        put(
            tmp.path(),
            "b.json",
            &json!({"date": "2026-10-02"}).to_string(),
        );
        put(
            tmp.path(),
            "a.json",
            &json!({"date": "2026-10-01"}).to_string(),
        );
        put(tmp.path(), "broken.json", "{ not json");
        put(
            tmp.path(),
            "nodate.json",
            &json!({"metrics": {}}).to_string(),
        );
        put(tmp.path(), "notes.txt", "ignored");

        let read = read_snapshots(tmp.path()).unwrap();
        let dates: Vec<&str> = read
            .snapshots
            .iter()
            .map(|s| s["date"].as_str().unwrap())
            .collect();
        assert_eq!(dates, ["2026-10-01", "2026-10-02"]);
        assert_eq!(read.warnings.len(), 2, "{:?}", read.warnings);
        assert!(read.warnings.iter().any(|w| w.contains("broken.json")));
        assert!(read.warnings.iter().any(|w| w.contains("nodate.json")));
    }
}
