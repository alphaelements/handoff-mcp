//! Persistence and approval workflow for reports under `.handoff/reports/`.
//!
//! Each report is a pair of files: `<report_id>.md` (rendered Markdown) and
//! `<report_id>.json` ([`ReportMeta`]). The Markdown is written first and the
//! metadata last, so a listed report always has its body.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde_json::Value;

use super::{ReportEngine, ReportMeta, ReportScope, ReportStatus, ReportType, RevisionEntry};
use crate::storage::atomic_write;

/// Directory (inside `.handoff/`) holding generated reports.
pub const REPORTS_DIR: &str = "reports";

const MAX_REPORT_ID_LEN: usize = 128;

pub fn reports_dir(handoff_dir: &Path) -> PathBuf {
    handoff_dir.join(REPORTS_DIR)
}

fn meta_path(handoff_dir: &Path, report_id: &str) -> PathBuf {
    reports_dir(handoff_dir).join(format!("{report_id}.json"))
}

fn body_path(handoff_dir: &Path, report_id: &str) -> PathBuf {
    reports_dir(handoff_dir).join(format!("{report_id}.md"))
}

/// Report ids become file names, so only `[A-Za-z0-9_-]` is accepted (no path
/// separators, no `.`/`..`).
pub fn validate_report_id(report_id: &str) -> Result<()> {
    let ok = !report_id.is_empty()
        && report_id.len() <= MAX_REPORT_ID_LEN
        && report_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !ok {
        bail!(
            "Invalid report_id '{report_id}' (use only letters, digits, '-' and '_'; max {MAX_REPORT_ID_LEN} chars)"
        );
    }
    Ok(())
}

/// Result of scanning `.handoff/reports/`.
#[derive(Debug, Default)]
pub struct ReportList {
    /// Newest first (by `generated_at`, then `report_id`).
    pub reports: Vec<ReportMeta>,
    /// Metadata files that could not be used (unreadable, corrupt, or whose
    /// `report_id` does not match the file name).
    pub warnings: Vec<String>,
}

/// Scans `.handoff/reports/*.json`. A missing directory is an empty list.
pub fn list_reports(handoff_dir: &Path) -> Result<ReportList> {
    let dir = reports_dir(handoff_dir);
    let mut list = ReportList::default();
    if !dir.is_dir() {
        return Ok(list);
    }
    for entry in std::fs::read_dir(&dir)
        .with_context(|| format!("Failed to read reports dir: {}", dir.display()))?
    {
        let path = entry
            .with_context(|| format!("Failed to read reports dir: {}", dir.display()))?
            .path();
        let Some(stem) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".json"))
        else {
            continue;
        };
        let file_label = format!("{stem}.json");
        let parsed = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|raw| serde_json::from_str::<ReportMeta>(&raw).map_err(|e| e.to_string()));
        match parsed {
            Ok(meta) if meta.report_id == stem => list.reports.push(meta),
            Ok(meta) => list.warnings.push(format!(
                "Skipped {file_label}: report_id '{}' does not match the file name",
                meta.report_id
            )),
            Err(e) => list.warnings.push(format!("Skipped {file_label}: {e}")),
        }
    }
    list.reports.sort_by(|a, b| {
        b.generated_at
            .cmp(&a.generated_at)
            .then_with(|| b.report_id.cmp(&a.report_id))
    });
    list.warnings.sort();
    Ok(list)
}

/// Reads one report's metadata.
pub fn read_meta(handoff_dir: &Path, report_id: &str) -> Result<ReportMeta> {
    validate_report_id(report_id)?;
    let path = meta_path(handoff_dir, report_id);
    if !path.is_file() {
        bail!("Report '{report_id}' not found");
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read report metadata: {}", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse report metadata: {}", path.display()))
}

/// Reads the rendered Markdown of a report.
pub fn read_body(handoff_dir: &Path, report_id: &str) -> Result<String> {
    validate_report_id(report_id)?;
    let path = body_path(handoff_dir, report_id);
    std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read report body: {}", path.display()))
}

fn write_meta(handoff_dir: &Path, meta: &ReportMeta) -> Result<()> {
    let json = serde_json::to_string_pretty(meta).context("Failed to serialize report metadata")?;
    atomic_write(meta_path(handoff_dir, &meta.report_id), json.as_bytes())
}

/// Picks an unused id `<type>-<YYYYMMDD>-<HHMMSS>[-N]`.
fn allocate_report_id(
    handoff_dir: &Path,
    report_type: ReportType,
    now: chrono::DateTime<Utc>,
) -> String {
    let base = format!("{}-{}", report_type.as_str(), now.format("%Y%m%d-%H%M%S"));
    let taken =
        |id: &str| meta_path(handoff_dir, id).exists() || body_path(handoff_dir, id).exists();
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|id| !taken(id))
        .expect("an unbounded range always yields an unused id")
}

/// Renders a new report, writes its Markdown and metadata (status `draft`),
/// and returns the metadata. `version` is one more than the number of
/// existing reports of the same type and scope.
pub fn create_report(
    engine: &ReportEngine,
    handoff_dir: &Path,
    report_type: ReportType,
    scope: ReportScope,
    data: &Value,
    actor: Option<&str>,
) -> Result<ReportMeta> {
    let existing = list_reports(handoff_dir)?;
    let same = existing
        .reports
        .iter()
        .filter(|m| m.report_type == report_type && m.scope == scope)
        .count();
    let version = u32::try_from(same + 1).context("Too many reports of the same type and scope")?;

    let now = Utc::now();
    let report_id = allocate_report_id(handoff_dir, report_type, now);
    let ts = now.to_rfc3339();
    let meta = ReportMeta {
        output_path: format!("{REPORTS_DIR}/{report_id}.md"),
        report_id,
        report_type,
        scope,
        version,
        status: ReportStatus::Draft,
        generated_at: ts.clone(),
        reviewer: None,
        approved_at: None,
        revision_history: vec![RevisionEntry {
            ts,
            from: None,
            to: ReportStatus::Draft,
            actor: actor.map(str::to_string),
            comment: None,
        }],
    };

    let markdown = engine.generate(&meta, data)?;

    std::fs::create_dir_all(reports_dir(handoff_dir)).with_context(|| {
        format!(
            "Failed to create reports dir: {}",
            reports_dir(handoff_dir).display()
        )
    })?;
    let md_path = body_path(handoff_dir, &meta.report_id);
    atomic_write(&md_path, markdown.as_bytes())?;
    if let Err(e) = write_meta(handoff_dir, &meta) {
        // Do not leave an unlisted body behind. Best-effort: the metadata
        // error below is the one the caller needs.
        let _ = std::fs::remove_file(&md_path);
        return Err(e);
    }
    Ok(meta)
}

/// Moves a report to `to`, validating the workflow and appending to
/// `revision_history`. For `approved`/`revision_requested`, `actor` becomes
/// the `reviewer`; `approved_at` is set on approval and cleared otherwise.
pub fn transition(
    handoff_dir: &Path,
    report_id: &str,
    to: ReportStatus,
    actor: Option<&str>,
    comment: Option<&str>,
) -> Result<ReportMeta> {
    let mut meta = read_meta(handoff_dir, report_id)?;
    let from = meta.status;
    if !from.can_transition_to(to) {
        bail!(
            "Report '{report_id}' is {}; it cannot move to {}",
            from.as_str(),
            to.as_str()
        );
    }

    let ts = Utc::now().to_rfc3339();
    if matches!(to, ReportStatus::Approved | ReportStatus::RevisionRequested) {
        meta.reviewer = actor.map(str::to_string);
    }
    meta.approved_at = (to == ReportStatus::Approved).then(|| ts.clone());
    meta.status = to;
    meta.revision_history.push(RevisionEntry {
        ts,
        from: Some(from),
        to,
        actor: actor.map(str::to_string),
        comment: comment.map(str::to_string),
    });
    write_meta(handoff_dir, &meta)?;
    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn create(dir: &Path, t: ReportType) -> ReportMeta {
        let engine = ReportEngine::new().unwrap();
        create_report(&engine, dir, t, ReportScope::default(), &json!({}), None).unwrap()
    }

    #[test]
    fn report_id_validation() {
        for ok in ["a", "weekly-20261008-120000", "A_b-9"] {
            assert!(validate_report_id(ok).is_ok(), "{ok}");
        }
        for bad in ["", "..", "a/b", "a\\b", "a.b", "a b", &"x".repeat(129)] {
            assert!(validate_report_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn ids_never_collide_within_the_same_second() {
        let tmp = tempfile::tempdir().unwrap();
        let ids: std::collections::HashSet<String> = (0..5)
            .map(|_| create(tmp.path(), ReportType::Weekly).report_id)
            .collect();
        assert_eq!(ids.len(), 5);
    }

    #[test]
    fn transition_sets_reviewer_and_approval_time_only_when_approved() {
        let tmp = tempfile::tempdir().unwrap();
        let id = create(tmp.path(), ReportType::Weekly).report_id;
        transition(tmp.path(), &id, ReportStatus::Submitted, None, None).unwrap();
        let rejected = transition(
            tmp.path(),
            &id,
            ReportStatus::RevisionRequested,
            Some("bob"),
            Some("redo"),
        )
        .unwrap();
        assert_eq!(rejected.reviewer.as_deref(), Some("bob"));
        assert!(rejected.approved_at.is_none());
        transition(tmp.path(), &id, ReportStatus::Submitted, None, None).unwrap();
        let approved =
            transition(tmp.path(), &id, ReportStatus::Approved, Some("al"), None).unwrap();
        assert_eq!(approved.reviewer.as_deref(), Some("al"));
        assert!(approved.approved_at.is_some());
    }

    #[test]
    fn failed_transition_leaves_the_file_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let id = create(tmp.path(), ReportType::Weekly).report_id;
        let before = std::fs::read(meta_path(tmp.path(), &id)).unwrap();
        assert!(transition(tmp.path(), &id, ReportStatus::Approved, Some("a"), None).is_err());
        assert_eq!(std::fs::read(meta_path(tmp.path(), &id)).unwrap(), before);
    }

    #[test]
    fn list_skips_mismatched_and_unrelated_files() {
        let tmp = tempfile::tempdir().unwrap();
        let meta = create(tmp.path(), ReportType::Weekly);
        let dir = reports_dir(tmp.path());
        std::fs::copy(
            meta_path(tmp.path(), &meta.report_id),
            dir.join("other-name.json"),
        )
        .unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        let list = list_reports(tmp.path()).unwrap();
        assert_eq!(list.reports.len(), 1);
        assert_eq!(list.warnings.len(), 1);
        assert!(list.warnings[0].contains("other-name.json"));
    }
}
