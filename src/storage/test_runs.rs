//! Test run definitions (`trace/test_runs/`) — wiki/270-vmodel-m3-design.md
//! §2.6/§4.4 (FR-304, M3-09). One `handoff_trace_test_run(action="create")`
//! call writes exactly one `.handoff/trace/test_runs/<test_run_id>.json`
//! definition file — created with `create_new` (never overwritten, retried
//! with a fresh random suffix on a name collision, same NFR-007 convention as
//! `runs/<run_id>.json`/`trace/baselines/<baseline_id>.json`). Unlike
//! baselines, there is no derived `_index.json` cache here: `list` (§4.4)
//! simply reads every `test_runs/*.json` file directly (a test run is
//! expected to be a relatively rare, human-scale object — nowhere near the
//! volume of `runs/*.json` that justified `_latest.json`'s incremental
//! cache), and `progress` (§4.4) is a live, uncached aggregation over
//! `runs/*.json` filtered by `test_run_id` (§2.6: "テストランの進捗は
//! `runs/*.json` を `test_run_id` でフィルタして動的に計算する").

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// `scope` of a [`TestRunRecord`] (wiki/270 §2.6) — the same select
/// vocabulary `handoff_trace_next`'s own `layers?`/`kinds?`/`assignee?`
/// filters use, snapshotted at `create` time so a later `list`/`progress`
/// call can still show what this test run was originally scoped to even as
/// the underlying trace graph evolves.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TestRunScope {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
}

/// One `.handoff/trace/test_runs/<test_run_id>.json` file's full contents
/// (wiki/270 §2.6's worked example).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TestRunRecord {
    pub test_run_id: String,
    /// RFC3339 with millisecond precision.
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub scope: TestRunScope,
    pub target_items: Vec<String>,
    pub total_target_count: usize,
    /// Verification-campaign checklist (FR-512/SPEC-512). Empty for a legacy
    /// test run created before the campaign data model existed (and omitted
    /// from the file in that case) — every campaign field below is
    /// `#[serde(default)]` so such a file still parses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checklist: Vec<ChecklistItem>,
    /// Campaign lifecycle state; a legacy record without the field reads as
    /// [`CampaignStatus::Draft`].
    #[serde(default)]
    pub campaign_status: CampaignStatus,
    /// Aggregation of `checklist`, recomputed on every campaign mutation by
    /// [`update_test_run`] (never trusted from a caller).
    #[serde(default)]
    pub progress: CampaignProgress,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_at: Option<String>,
}

/// Lifecycle of a verification campaign (SPEC-512):
/// `draft -> in_progress -> completed -> approved` (`completed` may be
/// reopened back to `in_progress`; `approved` is terminal).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CampaignStatus {
    #[default]
    Draft,
    InProgress,
    Completed,
    Approved,
}

impl CampaignStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            CampaignStatus::Draft => "draft",
            CampaignStatus::InProgress => "in_progress",
            CampaignStatus::Completed => "completed",
            CampaignStatus::Approved => "approved",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "draft" => Some(CampaignStatus::Draft),
            "in_progress" => Some(CampaignStatus::InProgress),
            "completed" => Some(CampaignStatus::Completed),
            "approved" => Some(CampaignStatus::Approved),
            _ => None,
        }
    }

    /// Statuses reachable from `self` through `set_status`.
    fn allowed_next(self) -> &'static [CampaignStatus] {
        match self {
            CampaignStatus::Draft => &[CampaignStatus::InProgress],
            CampaignStatus::InProgress => &[CampaignStatus::Completed],
            CampaignStatus::Completed => &[CampaignStatus::InProgress, CampaignStatus::Approved],
            CampaignStatus::Approved => &[],
        }
    }
}

/// One checklist item's verdict (SPEC-512). `Pending` is the initial state
/// and also what a caller can reset an item to.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CheckResult {
    #[default]
    Pending,
    Pass,
    Fail,
    Blocked,
    Waived,
}

impl CheckResult {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(CheckResult::Pending),
            "pass" => Some(CheckResult::Pass),
            "fail" => Some(CheckResult::Fail),
            "blocked" => Some(CheckResult::Blocked),
            "waived" => Some(CheckResult::Waived),
            _ => None,
        }
    }
}

/// Structured evidence attached to a checklist item. Serialized as
/// `{path, type, caption}`; `path` is project-relative (no `..`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvidenceEntry {
    pub path: String,
    #[serde(rename = "type", default = "default_evidence_type")]
    pub evidence_type: String,
    #[serde(default)]
    pub caption: String,
}

fn default_evidence_type() -> String {
    "file".to_string()
}

impl EvidenceEntry {
    /// Rejects an empty path or one that escapes the project (`..`
    /// component) — a later report engine will embed these files.
    pub fn validate(&self) -> Result<()> {
        if self.path.trim().is_empty() {
            anyhow::bail!("evidence.path must not be empty");
        }
        if Path::new(&self.path)
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            anyhow::bail!("evidence.path must not contain '..': {:?}", self.path);
        }
        if self.evidence_type.trim().is_empty() {
            anyhow::bail!("evidence.type must not be empty");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChecklistItem {
    /// stable_id of the verified item.
    pub item_id: String,
    /// The acceptance criterion text (the item's `SubItem.description`).
    pub acceptance_text: String,
    #[serde(default)]
    pub result: CheckResult,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceEntry>,
    #[serde(default)]
    pub note: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<String>,
}

/// Aggregate over a checklist. `checked` = every non-pending item
/// (`pass + fail + blocked + waived`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CampaignProgress {
    pub total: usize,
    pub checked: usize,
    pub pass: usize,
    pub fail: usize,
    pub blocked: usize,
    pub waived: usize,
    pub pending: usize,
}

pub fn compute_campaign_progress(checklist: &[ChecklistItem]) -> CampaignProgress {
    let mut p = CampaignProgress {
        total: checklist.len(),
        ..CampaignProgress::default()
    };
    for item in checklist {
        match item.result {
            CheckResult::Pending => p.pending += 1,
            CheckResult::Pass => p.pass += 1,
            CheckResult::Fail => p.fail += 1,
            CheckResult::Blocked => p.blocked += 1,
            CheckResult::Waived => p.waived += 1,
        }
    }
    p.checked = p.total - p.pending;
    p
}

/// Builds the initial all-pending checklist for `target_items`, taking each
/// item's criterion text from `descriptions` (stable_id -> description). An
/// item without a description falls back to its own id as the text: the
/// checklist row must still be actionable, and the id is the only label we
/// have for it.
pub fn generate_checklist(
    target_items: &[String],
    descriptions: &std::collections::HashMap<String, String>,
) -> Vec<ChecklistItem> {
    target_items
        .iter()
        .map(|id| ChecklistItem {
            item_id: id.clone(),
            acceptance_text: descriptions.get(id).cloned().unwrap_or_else(|| id.clone()),
            result: CheckResult::Pending,
            evidence: Vec::new(),
            note: String::new(),
            verified_by: None,
            verified_at: None,
        })
        .collect()
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn ensure_mutable(record: &TestRunRecord) -> Result<()> {
    match record.campaign_status {
        CampaignStatus::Draft | CampaignStatus::InProgress => Ok(()),
        s => anyhow::bail!(
            "campaign is {}; checklist is frozen (reopen a completed campaign with \
             set_status=in_progress)",
            s.as_str()
        ),
    }
}

fn find_item_mut<'a>(
    record: &'a mut TestRunRecord,
    item_id: &str,
) -> Result<&'a mut ChecklistItem> {
    record
        .checklist
        .iter_mut()
        .find(|c| c.item_id == item_id)
        .ok_or_else(|| anyhow::anyhow!("checklist item not found: {item_id}"))
}

/// Records `result` (+ optional note/evidence/verifier) on one checklist
/// item. The first recorded check moves a `draft` campaign to
/// `in_progress`. `evidence` is appended to the item's existing evidence.
pub fn record_check(
    record: &mut TestRunRecord,
    item_id: &str,
    result: CheckResult,
    note: Option<String>,
    verified_by: Option<String>,
    evidence: Vec<EvidenceEntry>,
) -> Result<()> {
    ensure_mutable(record)?;
    for e in &evidence {
        e.validate()?;
    }
    let item = find_item_mut(record, item_id)?;
    item.result = result;
    if let Some(n) = note {
        item.note = n;
    }
    item.evidence.extend(evidence);
    if result == CheckResult::Pending {
        item.verified_by = None;
        item.verified_at = None;
    } else {
        item.verified_by = verified_by;
        item.verified_at = Some(now_rfc3339());
    }
    if record.campaign_status == CampaignStatus::Draft && result != CheckResult::Pending {
        record.campaign_status = CampaignStatus::InProgress;
    }
    Ok(())
}

/// Appends one evidence entry to a checklist item without changing its
/// result.
pub fn add_evidence(record: &mut TestRunRecord, item_id: &str, entry: EvidenceEntry) -> Result<()> {
    ensure_mutable(record)?;
    entry.validate()?;
    find_item_mut(record, item_id)?.evidence.push(entry);
    Ok(())
}

/// Applies a `set_status` transition. `completed` requires a non-empty,
/// fully-checked checklist; `approved` requires `approved_by`.
pub fn transition_status(
    record: &mut TestRunRecord,
    to: CampaignStatus,
    approved_by: Option<String>,
) -> Result<()> {
    let from = record.campaign_status;
    if !from.allowed_next().contains(&to) {
        let allowed: Vec<&str> = from.allowed_next().iter().map(|s| s.as_str()).collect();
        anyhow::bail!(
            "invalid campaign_status transition {} -> {} (allowed from {}: {})",
            from.as_str(),
            to.as_str(),
            from.as_str(),
            if allowed.is_empty() {
                "none".to_string()
            } else {
                allowed.join(", ")
            }
        );
    }
    let progress = compute_campaign_progress(&record.checklist);
    match to {
        CampaignStatus::Completed => {
            if progress.total == 0 {
                anyhow::bail!("cannot complete a campaign with an empty checklist");
            }
            if progress.pending > 0 {
                anyhow::bail!(
                    "cannot complete: {} checklist item(s) still pending",
                    progress.pending
                );
            }
            record.completed_at = Some(now_rfc3339());
        }
        CampaignStatus::Approved => {
            let who = approved_by
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("'approved_by' is required to approve"))?;
            record.approved_by = Some(who);
            record.approved_at = Some(now_rfc3339());
        }
        CampaignStatus::InProgress => {
            // Reopen: the completion stamp no longer holds.
            record.completed_at = None;
        }
        CampaignStatus::Draft => {}
    }
    record.campaign_status = to;
    Ok(())
}

/// Mirrors `storage::runs`/`storage::baselines`/`storage::clears`'s own
/// collision-avoidance random suffix generator (independent copy — each id
/// space gets its own generator/counter rather than sharing one across
/// modules, same rationale as those modules' own doc comments).
static TEST_RUN_ID_SEQ: AtomicU64 = AtomicU64::new(0);

fn random_six_digits() -> u32 {
    let seq = TEST_RUN_ID_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id() as u64;
    let mixed =
        nanos ^ pid.wrapping_mul(0x2545_F491_4F6C_DD1D) ^ seq.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    (mixed % 1_000_000) as u32
}

fn format_test_run_id(now: DateTime<Utc>, rand_digits: u32) -> String {
    format!("{}-{rand_digits:06}", now.format("%Y%m%d-%H%M%S-%3f"))
}

const CREATE_TEST_RUN_FILE_MAX_ATTEMPTS: u32 = 20;

pub fn test_runs_dir(handoff: &Path) -> PathBuf {
    handoff.join("trace").join("test_runs")
}

/// Writes `record` to a brand-new
/// `.handoff/trace/test_runs/<test_run_id>.json` file — `create_new`, retried
/// with a fresh random suffix on an `AlreadyExists` collision (mirrors
/// `storage::baselines::write_baseline_record`). `record.test_run_id` is
/// overwritten with the filename actually allocated.
pub fn write_test_run_record(handoff: &Path, mut record: TestRunRecord) -> Result<TestRunRecord> {
    let dir = test_runs_dir(handoff);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create dir: {}", dir.display()))?;

    let now = Utc::now();
    for _ in 0..CREATE_TEST_RUN_FILE_MAX_ATTEMPTS {
        let test_run_id = format_test_run_id(now, random_six_digits());
        let path = dir.join(format!("{test_run_id}.json"));
        record.test_run_id = test_run_id;
        let content =
            serde_json::to_string_pretty(&record).context("Failed to serialize test run record")?;

        let mut open_opts = std::fs::OpenOptions::new();
        open_opts.write(true).create_new(true);
        match open_opts.open(&path) {
            Ok(mut file) => {
                file.write_all(content.as_bytes())
                    .with_context(|| format!("Failed to write {}", path.display()))?;
                file.sync_all()
                    .with_context(|| format!("Failed to sync {}", path.display()))?;
                return Ok(record);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(e).with_context(|| format!("Failed to create {}", path.display()))
            }
        }
    }
    anyhow::bail!(
        "Failed to allocate a unique test_run_id filename under {} after {CREATE_TEST_RUN_FILE_MAX_ATTEMPTS} attempts",
        dir.display()
    )
}

/// Read-modify-write of one existing test run under an exclusive file lock
/// (`<id>.lock`, so concurrent MCP calls on the same campaign serialize),
/// persisted atomically (temp file + rename). `progress` is recomputed from
/// the checklist after `mutate` succeeds; if `mutate` fails nothing is
/// written.
pub fn update_test_run<F>(handoff: &Path, test_run_id: &str, mutate: F) -> Result<TestRunRecord>
where
    F: FnOnce(&mut TestRunRecord) -> Result<()>,
{
    use fs2::FileExt as _;

    if test_run_id.is_empty() || test_run_id.contains(['/', '\\']) || test_run_id.contains("..") {
        anyhow::bail!("Invalid test_run_id: {test_run_id:?}");
    }
    let dir = test_runs_dir(handoff);
    let path = dir.join(format!("{test_run_id}.json"));
    if !path.is_file() {
        anyhow::bail!("Test run not found: {test_run_id}");
    }
    let lock_path = dir.join(format!("{test_run_id}.lock"));
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("Failed to open {}", lock_path.display()))?;
    lock_file
        .lock_exclusive()
        .with_context(|| format!("Failed to lock {}", lock_path.display()))?;

    let result = (|| -> Result<TestRunRecord> {
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let mut record: TestRunRecord = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?;
        mutate(&mut record)?;
        record.progress = compute_campaign_progress(&record.checklist);

        let serialized =
            serde_json::to_string_pretty(&record).context("Failed to serialize test run record")?;
        let tmp_path = dir.join(format!("{test_run_id}.json.tmp"));
        {
            let mut tmp = std::fs::File::create(&tmp_path)
                .with_context(|| format!("Failed to create {}", tmp_path.display()))?;
            tmp.write_all(serialized.as_bytes())
                .with_context(|| format!("Failed to write {}", tmp_path.display()))?;
            tmp.sync_all()
                .with_context(|| format!("Failed to sync {}", tmp_path.display()))?;
        }
        std::fs::rename(&tmp_path, &path)
            .with_context(|| format!("Failed to replace {}", path.display()))?;
        Ok(record)
    })();

    // Unlock failure is harmless: the lock is released when the handle drops.
    drop(lock_file);
    result
}

/// Reads every `test_runs/<id>.json` file (top-level only, no
/// month-subdirectory convention here) and returns them sorted by
/// `created_at` ascending (oldest first) — [`list_test_runs`]'s own
/// "reverse for newest first" stays the single place that decides display
/// order. A file that fails to parse is skipped rather than failing the
/// whole list (self-healing in the same spirit as
/// `storage::baselines::rebuild_index_from_files`, though this module has no
/// separate index file to fall back *from* — every call reads the source
/// files directly).
fn read_all_test_runs(handoff: &Path) -> Result<Vec<TestRunRecord>> {
    let dir = test_runs_dir(handoff);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in
        std::fs::read_dir(&dir).with_context(|| format!("Failed to read dir: {}", dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".json") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(record) = serde_json::from_str::<TestRunRecord>(&content) else {
            continue;
        };
        out.push(record);
    }
    out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    Ok(out)
}

/// `handoff_trace_test_run(action="list")` (wiki/270 §4.4): the `limit` most
/// recent test run definitions, newest first. Returns `(entries, truncated)`
/// — `truncated` is `true` when more exist beyond `limit`.
pub fn list_test_runs(handoff: &Path, limit: usize) -> Result<(Vec<TestRunRecord>, bool)> {
    let mut entries = read_all_test_runs(handoff)?;
    entries.reverse();
    let truncated = entries.len() > limit;
    entries.truncate(limit);
    Ok((entries, truncated))
}

/// `handoff_trace_test_run(action="progress"/action="create")`'s lookup of
/// one test run's own definition by id — `None` when no such
/// `test_runs/<test_run_id>.json` file exists.
pub fn find_test_run(handoff: &Path, test_run_id: &str) -> Result<Option<TestRunRecord>> {
    let path = test_runs_dir(handoff).join(format!("{test_run_id}.json"));
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let record = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(Some(record))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(label: &str, target_items: Vec<&str>) -> TestRunRecord {
        TestRunRecord {
            test_run_id: String::new(),
            created_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            label: Some(label.to_string()),
            scope: TestRunScope {
                layers: vec!["acceptance".to_string(), "system_test".to_string()],
                kinds: vec!["rerun".to_string(), "fix_failing".to_string()],
                assignee: Some("ryoma".to_string()),
            },
            target_items: target_items.into_iter().map(String::from).collect(),
            total_target_count: 0, // overwritten below per-call to match target_items
            checklist: Vec::new(),
            campaign_status: CampaignStatus::Draft,
            progress: CampaignProgress::default(),
            completed_at: None,
            approved_by: None,
            approved_at: None,
        }
    }

    #[test]
    fn write_test_run_record_creates_the_file_and_fills_in_the_allocated_id() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let mut record = sample("Sprint 5 regression", vec!["AT-REQ-001-1", "ST-040"]);
        record.total_target_count = record.target_items.len();
        let persisted = write_test_run_record(&handoff, record).unwrap();

        assert!(!persisted.test_run_id.is_empty());
        let path = test_runs_dir(&handoff).join(format!("{}.json", persisted.test_run_id));
        assert!(path.exists());

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: TestRunRecord = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.label.as_deref(), Some("Sprint 5 regression"));
        assert_eq!(parsed.scope.layers, vec!["acceptance", "system_test"]);
        assert_eq!(parsed.scope.kinds, vec!["rerun", "fix_failing"]);
        assert_eq!(parsed.scope.assignee.as_deref(), Some("ryoma"));
        assert_eq!(parsed.target_items, vec!["AT-REQ-001-1", "ST-040"]);
        assert_eq!(parsed.total_target_count, 2);
    }

    #[test]
    fn write_test_run_record_never_overwrites_an_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let first = write_test_run_record(&handoff, sample("a", vec!["X-1"])).unwrap();
        let second = write_test_run_record(&handoff, sample("b", vec!["X-2"])).unwrap();

        assert_ne!(first.test_run_id, second.test_run_id);
        assert!(test_runs_dir(&handoff)
            .join(format!("{}.json", first.test_run_id))
            .exists());
        assert!(test_runs_dir(&handoff)
            .join(format!("{}.json", second.test_run_id))
            .exists());
    }

    #[test]
    fn list_test_runs_returns_newest_first_and_respects_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let first = write_test_run_record(&handoff, sample("first", vec!["X-1"])).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let second = write_test_run_record(&handoff, sample("second", vec!["X-2"])).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let third = write_test_run_record(&handoff, sample("third", vec!["X-3"])).unwrap();

        let (entries, truncated) = list_test_runs(&handoff, 2).unwrap();
        assert!(truncated);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].test_run_id, third.test_run_id);
        assert_eq!(entries[1].test_run_id, second.test_run_id);
        assert!(entries.iter().all(|e| e.test_run_id != first.test_run_id));
    }

    #[test]
    fn list_test_runs_on_empty_project_returns_empty_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let (entries, truncated) = list_test_runs(&handoff, 20).unwrap();
        assert!(entries.is_empty());
        assert!(!truncated);
    }

    #[test]
    fn find_test_run_returns_none_for_an_unknown_id() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        assert!(find_test_run(&handoff, "ghost").unwrap().is_none());
    }

    #[test]
    fn find_test_run_returns_the_matching_record() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();

        let persisted = write_test_run_record(&handoff, sample("only", vec!["X-1"])).unwrap();
        let found = find_test_run(&handoff, &persisted.test_run_id)
            .unwrap()
            .expect("must find the record just written");
        assert_eq!(found.label.as_deref(), Some("only"));
    }
    // ---- verification campaign (FR-512 / SPEC-512) ----

    fn campaign(items: &[(&str, &str)]) -> TestRunRecord {
        let ids: Vec<String> = items.iter().map(|(id, _)| id.to_string()).collect();
        let descriptions: std::collections::HashMap<String, String> = items
            .iter()
            .map(|(id, text)| (id.to_string(), text.to_string()))
            .collect();
        let mut record = sample("campaign", ids.iter().map(String::as_str).collect());
        record.total_target_count = ids.len();
        record.checklist = generate_checklist(&ids, &descriptions);
        record.progress = compute_campaign_progress(&record.checklist);
        record
    }

    fn shot(path: &str) -> EvidenceEntry {
        EvidenceEntry {
            path: path.to_string(),
            evidence_type: "screenshot".to_string(),
            caption: "after login".to_string(),
        }
    }

    #[test]
    fn legacy_record_without_campaign_fields_still_parses_as_draft() {
        let legacy = r#"{
            "test_run_id": "20260101-000000-000-000001",
            "created_at": "2026-01-01T00:00:00.000Z",
            "scope": {},
            "target_items": ["X-1"],
            "total_target_count": 1
        }"#;
        let parsed: TestRunRecord = serde_json::from_str(legacy).unwrap();
        assert!(parsed.checklist.is_empty());
        assert_eq!(parsed.campaign_status, CampaignStatus::Draft);
        assert_eq!(parsed.progress, CampaignProgress::default());
    }

    #[test]
    fn generate_checklist_uses_descriptions_and_falls_back_to_the_id() {
        let mut d = std::collections::HashMap::new();
        d.insert("AT-1".to_string(), "Login works".to_string());
        let list = generate_checklist(&["AT-1".to_string(), "AT-2".to_string()], &d);
        assert_eq!(list[0].acceptance_text, "Login works");
        assert_eq!(list[1].acceptance_text, "AT-2");
        assert!(list.iter().all(|c| c.result == CheckResult::Pending));
    }

    #[test]
    fn evidence_serializes_with_type_key() {
        let v = serde_json::to_value(shot("evidence/a.png")).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"path": "evidence/a.png", "type": "screenshot", "caption": "after login"})
        );
        let back: EvidenceEntry = serde_json::from_value(serde_json::json!({"path": "p"})).unwrap();
        assert_eq!(back.evidence_type, "file");
        assert_eq!(back.caption, "");
    }

    #[test]
    fn evidence_validation_rejects_empty_and_traversal() {
        assert!(shot("a/b.png").validate().is_ok());
        assert!(shot("").validate().is_err());
        assert!(shot("../secret").validate().is_err());
        assert!(shot("a/../../b").validate().is_err());
        let mut e = shot("a.png");
        e.evidence_type = " ".to_string();
        assert!(e.validate().is_err());
    }

    #[test]
    fn progress_counts_every_bucket() {
        let mut r = campaign(&[("A", "a"), ("B", "b"), ("C", "c"), ("D", "d"), ("E", "e")]);
        record_check(&mut r, "A", CheckResult::Pass, None, None, vec![]).unwrap();
        record_check(&mut r, "B", CheckResult::Fail, None, None, vec![]).unwrap();
        record_check(&mut r, "C", CheckResult::Blocked, None, None, vec![]).unwrap();
        record_check(&mut r, "D", CheckResult::Waived, None, None, vec![]).unwrap();
        let p = compute_campaign_progress(&r.checklist);
        assert_eq!(
            p,
            CampaignProgress {
                total: 5,
                checked: 4,
                pass: 1,
                fail: 1,
                blocked: 1,
                waived: 1,
                pending: 1
            }
        );
    }

    #[test]
    fn record_check_stamps_verifier_moves_draft_to_in_progress_and_appends_evidence() {
        let mut r = campaign(&[("A", "a")]);
        assert_eq!(r.campaign_status, CampaignStatus::Draft);
        record_check(
            &mut r,
            "A",
            CheckResult::Pass,
            Some("ok".to_string()),
            Some("ryoma".to_string()),
            vec![shot("e/1.png")],
        )
        .unwrap();
        assert_eq!(r.campaign_status, CampaignStatus::InProgress);
        let item = &r.checklist[0];
        assert_eq!(item.result, CheckResult::Pass);
        assert_eq!(item.note, "ok");
        assert_eq!(item.verified_by.as_deref(), Some("ryoma"));
        assert!(item.verified_at.is_some());
        record_check(
            &mut r,
            "A",
            CheckResult::Pass,
            None,
            None,
            vec![shot("e/2.png")],
        )
        .unwrap();
        assert_eq!(r.checklist[0].evidence.len(), 2);
        assert_eq!(
            r.checklist[0].note, "ok",
            "note untouched when not supplied"
        );
    }

    #[test]
    fn record_check_back_to_pending_clears_verifier_and_keeps_draft() {
        let mut r = campaign(&[("A", "a")]);
        record_check(
            &mut r,
            "A",
            CheckResult::Pending,
            None,
            Some("x".into()),
            vec![],
        )
        .unwrap();
        assert_eq!(r.campaign_status, CampaignStatus::Draft);
        record_check(
            &mut r,
            "A",
            CheckResult::Pass,
            None,
            Some("x".into()),
            vec![],
        )
        .unwrap();
        record_check(&mut r, "A", CheckResult::Pending, None, None, vec![]).unwrap();
        assert!(r.checklist[0].verified_at.is_none());
        assert!(r.checklist[0].verified_by.is_none());
    }

    #[test]
    fn record_check_unknown_item_and_bad_evidence_leave_record_unchanged() {
        let mut r = campaign(&[("A", "a")]);
        assert!(record_check(&mut r, "ghost", CheckResult::Pass, None, None, vec![]).is_err());
        assert!(record_check(
            &mut r,
            "A",
            CheckResult::Pass,
            None,
            None,
            vec![shot("../x")]
        )
        .is_err());
        assert_eq!(r.checklist[0].result, CheckResult::Pending);
        assert_eq!(r.campaign_status, CampaignStatus::Draft);
    }

    #[test]
    fn add_evidence_keeps_result() {
        let mut r = campaign(&[("A", "a")]);
        add_evidence(&mut r, "A", shot("e/1.png")).unwrap();
        assert_eq!(r.checklist[0].evidence.len(), 1);
        assert_eq!(r.checklist[0].result, CheckResult::Pending);
        assert!(add_evidence(&mut r, "A", shot("")).is_err());
        assert!(add_evidence(&mut r, "ghost", shot("e/2.png")).is_err());
    }

    #[test]
    fn status_happy_path_draft_to_approved() {
        let mut r = campaign(&[("A", "a")]);
        transition_status(&mut r, CampaignStatus::InProgress, None).unwrap();
        record_check(&mut r, "A", CheckResult::Pass, None, None, vec![]).unwrap();
        transition_status(&mut r, CampaignStatus::Completed, None).unwrap();
        assert!(r.completed_at.is_some());
        transition_status(&mut r, CampaignStatus::Approved, Some("boss".into())).unwrap();
        assert_eq!(r.campaign_status, CampaignStatus::Approved);
        assert_eq!(r.approved_by.as_deref(), Some("boss"));
        assert!(r.approved_at.is_some());
    }

    #[test]
    fn status_rejects_skips_backwards_and_terminal_moves() {
        let mut r = campaign(&[("A", "a")]);
        assert!(transition_status(&mut r, CampaignStatus::Completed, None).is_err());
        assert!(transition_status(&mut r, CampaignStatus::Approved, Some("b".into())).is_err());
        assert!(transition_status(&mut r, CampaignStatus::Draft, None).is_err());
        transition_status(&mut r, CampaignStatus::InProgress, None).unwrap();
        assert!(transition_status(&mut r, CampaignStatus::InProgress, None).is_err());
        record_check(&mut r, "A", CheckResult::Waived, None, None, vec![]).unwrap();
        transition_status(&mut r, CampaignStatus::Completed, None).unwrap();
        transition_status(&mut r, CampaignStatus::Approved, Some("b".into())).unwrap();
        for to in [
            CampaignStatus::Draft,
            CampaignStatus::InProgress,
            CampaignStatus::Completed,
        ] {
            assert!(transition_status(&mut r, to, None).is_err(), "{to:?}");
        }
    }

    #[test]
    fn complete_requires_a_fully_checked_non_empty_checklist() {
        let mut r = campaign(&[("A", "a"), ("B", "b")]);
        transition_status(&mut r, CampaignStatus::InProgress, None).unwrap();
        record_check(&mut r, "A", CheckResult::Pass, None, None, vec![]).unwrap();
        let err = transition_status(&mut r, CampaignStatus::Completed, None).unwrap_err();
        assert!(err.to_string().contains("pending"), "{err}");
        assert_eq!(r.campaign_status, CampaignStatus::InProgress);

        let mut empty = campaign(&[]);
        transition_status(&mut empty, CampaignStatus::InProgress, None).unwrap();
        assert!(transition_status(&mut empty, CampaignStatus::Completed, None).is_err());
    }

    #[test]
    fn approve_requires_approver_and_reopen_clears_completion_and_unfreezes() {
        let mut r = campaign(&[("A", "a")]);
        record_check(&mut r, "A", CheckResult::Pass, None, None, vec![]).unwrap();
        transition_status(&mut r, CampaignStatus::Completed, None).unwrap();
        assert!(record_check(&mut r, "A", CheckResult::Fail, None, None, vec![]).is_err());
        assert!(add_evidence(&mut r, "A", shot("e/1.png")).is_err());
        assert!(transition_status(&mut r, CampaignStatus::Approved, None).is_err());
        assert!(transition_status(&mut r, CampaignStatus::Approved, Some("  ".into())).is_err());
        assert_eq!(r.campaign_status, CampaignStatus::Completed);

        transition_status(&mut r, CampaignStatus::InProgress, None).unwrap();
        assert!(r.completed_at.is_none());
        record_check(&mut r, "A", CheckResult::Fail, None, None, vec![]).unwrap();
    }

    #[test]
    fn update_test_run_persists_and_recomputes_progress() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();
        let persisted =
            write_test_run_record(&handoff, campaign(&[("A", "a"), ("B", "b")])).unwrap();

        let updated = update_test_run(&handoff, &persisted.test_run_id, |r| {
            record_check(r, "A", CheckResult::Pass, None, None, vec![])
        })
        .unwrap();
        assert_eq!(updated.progress.pass, 1);
        assert_eq!(updated.progress.pending, 1);

        let reread = find_test_run(&handoff, &persisted.test_run_id)
            .unwrap()
            .unwrap();
        assert_eq!(reread, updated);
        assert_eq!(reread.campaign_status, CampaignStatus::InProgress);
    }

    #[test]
    fn update_test_run_failure_writes_nothing_and_unknown_or_unsafe_ids_error() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();
        let persisted = write_test_run_record(&handoff, campaign(&[("A", "a")])).unwrap();

        let before = find_test_run(&handoff, &persisted.test_run_id)
            .unwrap()
            .unwrap();
        let err = update_test_run(&handoff, &persisted.test_run_id, |r| {
            record_check(r, "A", CheckResult::Pass, None, None, vec![])?;
            anyhow::bail!("boom")
        });
        assert!(err.is_err());
        let after = find_test_run(&handoff, &persisted.test_run_id)
            .unwrap()
            .unwrap();
        assert_eq!(before, after);

        assert!(update_test_run(&handoff, "ghost", |_| Ok(())).is_err());
        assert!(update_test_run(&handoff, "../x", |_| Ok(())).is_err());
        assert!(update_test_run(&handoff, "", |_| Ok(())).is_err());
    }

    #[test]
    fn list_and_read_all_ignore_lock_and_tmp_files() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();
        let persisted = write_test_run_record(&handoff, campaign(&[("A", "a")])).unwrap();
        update_test_run(&handoff, &persisted.test_run_id, |_| Ok(())).unwrap();
        let (entries, _) = list_test_runs(&handoff, 10).unwrap();
        assert_eq!(entries.len(), 1);
    }
}
