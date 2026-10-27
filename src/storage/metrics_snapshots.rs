//! Reader for the daily metrics snapshots in
//! `.handoff/metrics_snapshots/<YYYY-MM-DD>.json` (FR-503 / SPEC-503; written
//! by `handoff_snapshot_metrics` and `handoff_save_context`), and the
//! point-in-time task-state restore built on them (REQ-RPT-004).
//!
//! # Snapshot schema
//!
//! * `schema_version = 1`: `{schema_version, date, captured_at, metrics}`.
//! * `schema_version = 2`: v1 plus `tasks`, a flat list of [`TaskSnapshot`]
//!   (`id`, `title`, `status`, `assignee`, `priority`) covering every task of
//!   the tree at capture time.
//!
//! A v1 file stays readable: [`MetricsSnapshot::tasks`] is empty and
//! [`MetricsSnapshot::has_task_list`] is `false`.
//!
//! # Restoring the task state of a past day ([`get_snapshot_at`])
//!
//! The state "as of" day `D` is the state at the end of `D` (UTC).
//!
//! 1. The nearest v2 snapshot dated `D` or earlier is the base, and the
//!    `task.status_changed` events in `events.jsonl` between its capture and
//!    the end of `D` are replayed on top (`source = snapshot_plus_events`,
//!    not exact). A snapshot dated exactly `D` that no later event touched is
//!    returned as is (`source = snapshot`, exact); one captured mid-day with
//!    later changes on `D` is replayed like an earlier base.
//! 2. With no usable v2 snapshot at or before `D`, the state is estimated
//!    backwards from the current task tree: for every task with a
//!    `task.status_changed` event after `D`, the `from` status of its first
//!    such event is its status at the end of `D` (`source =
//!    events_estimate`).
//!
//! Both replays are estimates unless a same-day snapshot was untouched:
//! `events.jsonl` only records status transitions, so a task created after
//! the base snapshot that never changed status (and the title/assignee/
//! priority of any task that was not in the base snapshot, or an edit made
//! after it) cannot be recovered. A task deleted after `D` is missing from
//! the current tree and therefore cannot be restored by the events estimate
//! (case 2); only a snapshot that captured it can. Such gaps are surfaced in
//! [`TaskStateAt::warnings`] where they are detectable, and `exact` is
//! `false`.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::events::{read_events, EventFilters, EVENT_TASK_STATUS_CHANGED};
use super::tasks::{build_task_index, TaskIndex};

/// First snapshot `schema_version` that carries the `tasks` list.
pub const TASK_LIST_SCHEMA_VERSION: u32 = 2;

/// One task as recorded in a snapshot's `tasks` list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSnapshot {
    pub id: String,
    pub title: String,
    pub status: String,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
}

/// Typed view of a snapshot envelope; works for both schema versions.
#[derive(Debug, Clone, Deserialize)]
pub struct MetricsSnapshot {
    #[serde(default)]
    pub schema_version: u32,
    pub date: String,
    #[serde(default)]
    pub captured_at: Option<String>,
    /// Empty for a v1 snapshot (no task list was recorded).
    #[serde(default)]
    pub tasks: Vec<TaskSnapshot>,
}

impl MetricsSnapshot {
    /// `true` when the snapshot carries a task list (`schema_version >= 2`).
    pub fn has_task_list(&self) -> bool {
        self.schema_version >= TASK_LIST_SCHEMA_VERSION
    }
}

/// Flattens the task tree depth-first (parents before children) into the
/// snapshot representation.
pub fn flatten_tasks(tree: &[TaskIndex]) -> Vec<TaskSnapshot> {
    fn walk(nodes: &[TaskIndex], out: &mut Vec<TaskSnapshot>) {
        for n in nodes {
            out.push(TaskSnapshot {
                id: n.id.clone(),
                title: n.title.clone(),
                status: n.status.clone(),
                assignee: n.assignee.clone(),
                priority: n.priority.clone(),
            });
            walk(&n.children, out);
        }
    }
    let mut out = Vec::new();
    walk(tree, &mut out);
    out
}

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

/// How a [`TaskStateAt`] was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStateSource {
    /// A v2 snapshot dated exactly the requested day.
    Snapshot,
    /// The nearest earlier v2 snapshot plus replayed status events.
    SnapshotPlusEvents,
    /// No usable snapshot: estimated backwards from the current task tree.
    EventsEstimate,
}

/// Task state restored for a requested day by [`get_snapshot_at`].
#[derive(Debug, Clone, Serialize)]
pub struct TaskStateAt {
    pub requested_date: String,
    pub source: TaskStateSource,
    /// `true` only when read straight from a snapshot of that exact day.
    pub exact: bool,
    /// Date of the snapshot used as the base, if any.
    pub snapshot_date: Option<String>,
    /// Number of `task.status_changed` events applied to the base.
    pub events_applied: usize,
    pub tasks: Vec<TaskSnapshot>,
    pub warnings: Vec<String>,
}

/// A parsed `task.status_changed` event.
#[derive(Debug, Clone)]
struct StatusChange {
    ts: DateTime<Utc>,
    task_id: String,
    from: Option<String>,
    to: Option<String>,
}

/// Loads every `task.status_changed` event, oldest first. Lines whose
/// timestamp or task id is unusable are counted in `warnings`.
fn load_status_changes(
    handoff_dir: &Path,
    warnings: &mut Vec<String>,
) -> Result<Vec<StatusChange>> {
    let events = read_events(
        handoff_dir,
        &EventFilters {
            event_type: Some(EVENT_TASK_STATUS_CHANGED.to_string()),
            limit: Some(usize::MAX),
            ..Default::default()
        },
    )?;
    let mut skipped = 0usize;
    let mut changes = Vec::new();
    for e in events {
        let (Some(task_id), Ok(ts)) = (e.task_id, DateTime::parse_from_rfc3339(&e.ts)) else {
            skipped += 1;
            continue;
        };
        let detail: Value = e
            .detail
            .as_deref()
            .and_then(|d| serde_json::from_str(d).ok())
            .unwrap_or(Value::Null);
        let status_of = |key: &str| detail.get(key).and_then(Value::as_str).map(str::to_string);
        changes.push(StatusChange {
            ts: ts.with_timezone(&Utc),
            task_id,
            from: status_of("from"),
            to: status_of("to"),
        });
    }
    if skipped > 0 {
        warnings.push(format!(
            "{skipped} task.status_changed event(s) without a valid timestamp or task id were ignored"
        ));
    }
    changes.sort_by_key(|c| c.ts);
    Ok(changes)
}

/// Replays `changes` with `after < ts < before` on `base`. A changed task that
/// is not in `base` (created after the snapshot) is added with its
/// title/assignee/priority taken from `current`, or from nothing when it no
/// longer exists. Returns the number of events applied.
fn replay_forward(
    base: &mut Vec<TaskSnapshot>,
    changes: &[StatusChange],
    after: DateTime<Utc>,
    before: DateTime<Utc>,
    current: &HashMap<String, TaskSnapshot>,
    warnings: &mut Vec<String>,
) -> usize {
    let mut index: HashMap<String, usize> = base
        .iter()
        .enumerate()
        .map(|(i, t)| (t.id.clone(), i))
        .collect();
    let mut applied = 0;
    let mut approximated: Vec<String> = Vec::new();
    for c in changes.iter().filter(|c| c.ts > after && c.ts < before) {
        let Some(to) = c.to.as_deref() else {
            warnings.push(format!(
                "task.status_changed for {} at {} has no 'to' status and was ignored",
                c.task_id,
                c.ts.to_rfc3339()
            ));
            continue;
        };
        applied += 1;
        match index.get(&c.task_id) {
            Some(&i) => base[i].status = to.to_string(),
            None => {
                let mut task = current.get(&c.task_id).cloned().unwrap_or(TaskSnapshot {
                    id: c.task_id.clone(),
                    title: c.task_id.clone(),
                    status: String::new(),
                    assignee: None,
                    priority: None,
                });
                task.status = to.to_string();
                approximated.push(c.task_id.clone());
                index.insert(c.task_id.clone(), base.len());
                base.push(task);
            }
        }
    }
    approximated.sort();
    approximated.dedup();
    for id in approximated {
        warnings.push(format!(
            "task {id} is not in the base snapshot; its title/assignee/priority come from the current task tree"
        ));
    }
    applied
}

/// Rewinds `current` to the end of the requested day (`cutoff` = the
/// exclusive upper bound): the `from` status of a task's first change at or
/// after `cutoff` is its status before it. Returns the number of tasks
/// rewound.
fn rewind_backward(
    current: &mut [TaskSnapshot],
    changes: &[StatusChange],
    cutoff: DateTime<Utc>,
    warnings: &mut Vec<String>,
) -> usize {
    let mut first_after: HashMap<&str, &StatusChange> = HashMap::new();
    for c in changes.iter().filter(|c| c.ts >= cutoff) {
        first_after.entry(c.task_id.as_str()).or_insert(c);
    }
    let mut rewound = 0;
    for task in current.iter_mut() {
        let Some(c) = first_after.get(task.id.as_str()) else {
            continue;
        };
        match c.from.as_deref() {
            Some(from) => {
                task.status = from.to_string();
                rewound += 1;
            }
            None => warnings.push(format!(
                "task {} changed status at {} without a recorded previous status; its current status was kept",
                task.id,
                c.ts.to_rfc3339()
            )),
        }
    }
    rewound
}

/// Exclusive end of `date` (UTC): midnight of the next day.
fn end_of_day(date: NaiveDate) -> Result<DateTime<Utc>> {
    let next = date
        .succ_opt()
        .ok_or_else(|| anyhow!("date {date} is out of range"))?;
    Ok(next
        .and_hms_opt(0, 0, 0)
        .expect("midnight is a valid time")
        .and_utc())
}

/// Restores the task list as of the end of `date` (`YYYY-MM-DD`, UTC). See the
/// module docs for the three strategies and their exactness.
pub fn get_snapshot_at(handoff_dir: &Path, date: &str) -> Result<TaskStateAt> {
    let day = NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .with_context(|| format!("Invalid date '{date}': expected YYYY-MM-DD"))?;
    let cutoff = end_of_day(day)?;
    let requested = day.format("%Y-%m-%d").to_string();

    let read = read_snapshots(handoff_dir)?;
    let mut warnings = read.warnings;

    let mut base: Option<MetricsSnapshot> = None;
    for env in read.snapshots.into_iter().rev() {
        let snap_date = env["date"].as_str().unwrap_or_default().to_string();
        if snap_date.as_str() > requested.as_str() {
            continue;
        }
        let snap: MetricsSnapshot = match serde_json::from_value(env) {
            Ok(s) => s,
            Err(e) => {
                warnings.push(format!(
                    "metrics snapshot {snap_date} has a malformed task list ({e}) and was skipped"
                ));
                continue;
            }
        };
        if snap.has_task_list() {
            base = Some(snap);
            break;
        }
        warnings.push(format!(
            "metrics snapshot {snap_date} is schema_version {} (no task list) and was skipped",
            snap.schema_version
        ));
    }

    let changes = load_status_changes(handoff_dir, &mut warnings)?;
    let (current_tree, _) = build_task_index(&handoff_dir.join("tasks"), u32::MAX)?;
    let current = flatten_tasks(&current_tree);

    if let Some(snap) = base {
        // The capture time bounds the replay; a missing/garbled one falls back
        // to the start of the snapshot day, which is safe because replaying an
        // already-reflected event only re-sets the same absolute status.
        let snap_day = NaiveDate::parse_from_str(&snap.date, "%Y-%m-%d")
            .with_context(|| format!("Invalid snapshot date '{}'", snap.date))?;
        let captured = snap
            .captured_at
            .as_deref()
            .and_then(|c| DateTime::parse_from_rfc3339(c).ok())
            .map(|c| c.with_timezone(&Utc))
            .unwrap_or_else(|| end_of_day(snap_day).map_or(cutoff, |e| e - Duration::days(1)));
        let current_by_id: HashMap<String, TaskSnapshot> =
            current.into_iter().map(|t| (t.id.clone(), t)).collect();
        let mut tasks = snap.tasks;
        let applied = replay_forward(
            &mut tasks,
            &changes,
            captured,
            cutoff,
            &current_by_id,
            &mut warnings,
        );
        // A snapshot taken on the requested day is exact only while nothing
        // changed between its capture and the end of that day.
        let (source, exact) = if applied == 0 && snap.date == requested {
            (TaskStateSource::Snapshot, true)
        } else {
            (TaskStateSource::SnapshotPlusEvents, false)
        };
        return Ok(TaskStateAt {
            requested_date: requested,
            source,
            exact,
            snapshot_date: Some(snap.date),
            events_applied: applied,
            tasks,
            warnings,
        });
    }

    let mut tasks = current;
    let rewound = rewind_backward(&mut tasks, &changes, cutoff, &mut warnings);
    warnings.push(
        "no task-list snapshot at or before this date: estimated from the current task tree and \
         events.jsonl; tasks created after this date that never changed status cannot be excluded"
            .to_string(),
    );
    Ok(TaskStateAt {
        requested_date: requested,
        source: TaskStateSource::EventsEstimate,
        exact: false,
        snapshot_date: None,
        events_applied: rewound,
        tasks,
        warnings,
    })
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

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn task(id: &str, status: &str) -> TaskSnapshot {
        TaskSnapshot {
            id: id.to_string(),
            title: format!("Task {id}"),
            status: status.to_string(),
            assignee: Some("alice".to_string()),
            priority: Some("high".to_string()),
        }
    }

    fn change(at: &str, id: &str, from: Option<&str>, to: Option<&str>) -> StatusChange {
        StatusChange {
            ts: ts(at),
            task_id: id.to_string(),
            from: from.map(str::to_string),
            to: to.map(str::to_string),
        }
    }

    #[test]
    fn v1_snapshot_deserializes_with_empty_task_list() {
        let snap: MetricsSnapshot = serde_json::from_value(json!({
            "schema_version": 1, "date": "2026-10-01",
            "captured_at": "2026-10-01T10:00:00+00:00", "metrics": {}
        }))
        .unwrap();
        assert!(snap.tasks.is_empty());
        assert!(!snap.has_task_list());
    }

    #[test]
    fn v2_snapshot_deserializes_task_list() {
        let snap: MetricsSnapshot = serde_json::from_value(json!({
            "schema_version": 2, "date": "2026-10-01", "metrics": {},
            "tasks": [{"id": "t1", "title": "A", "status": "todo"}]
        }))
        .unwrap();
        assert!(snap.has_task_list());
        assert_eq!(snap.tasks[0].id, "t1");
        assert_eq!(snap.tasks[0].assignee, None);
    }

    #[test]
    fn replay_forward_applies_only_events_inside_the_window() {
        let mut base = vec![task("t1", "todo"), task("t2", "todo")];
        let changes = vec![
            change("2026-10-01T09:00:00Z", "t1", Some("todo"), Some("done")), // before capture
            change("2026-10-02T09:00:00Z", "t1", Some("done"), Some("review")),
            change(
                "2026-10-02T10:00:00Z",
                "t2",
                Some("todo"),
                Some("in_progress"),
            ),
            change(
                "2026-10-04T00:00:00Z",
                "t2",
                Some("in_progress"),
                Some("done"),
            ), // at cutoff
        ];
        let mut w = Vec::new();
        let n = replay_forward(
            &mut base,
            &changes,
            ts("2026-10-01T12:00:00Z"),
            ts("2026-10-04T00:00:00Z"),
            &HashMap::new(),
            &mut w,
        );
        assert_eq!(n, 2);
        assert_eq!(base[0].status, "review");
        assert_eq!(base[1].status, "in_progress");
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn replay_forward_adds_task_unknown_to_base_with_warning() {
        let mut base = vec![task("t1", "todo")];
        let mut cur = HashMap::new();
        cur.insert("t9".to_string(), task("t9", "done"));
        let changes = vec![change(
            "2026-10-02T00:00:00Z",
            "t9",
            Some("todo"),
            Some("in_progress"),
        )];
        let mut w = Vec::new();
        replay_forward(
            &mut base,
            &changes,
            ts("2026-10-01T00:00:00Z"),
            ts("2026-10-03T00:00:00Z"),
            &cur,
            &mut w,
        );
        assert_eq!(base.len(), 2);
        assert_eq!(base[1].id, "t9");
        assert_eq!(base[1].status, "in_progress");
        assert_eq!(base[1].title, "Task t9");
        assert!(w.iter().any(|m| m.contains("t9")), "{w:?}");
    }

    #[test]
    fn replay_forward_ignores_event_without_to() {
        let mut base = vec![task("t1", "todo")];
        let changes = vec![change("2026-10-02T00:00:00Z", "t1", None, None)];
        let mut w = Vec::new();
        let n = replay_forward(
            &mut base,
            &changes,
            ts("2026-10-01T00:00:00Z"),
            ts("2026-10-03T00:00:00Z"),
            &HashMap::new(),
            &mut w,
        );
        assert_eq!(n, 0);
        assert_eq!(base[0].status, "todo");
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn rewind_backward_uses_from_of_first_event_after_cutoff() {
        let mut cur = vec![
            task("t1", "done"),
            task("t2", "done"),
            task("t3", "blocked"),
        ];
        let changes = vec![
            change(
                "2026-10-01T09:00:00Z",
                "t1",
                Some("todo"),
                Some("in_progress"),
            ),
            change(
                "2026-10-03T09:00:00Z",
                "t1",
                Some("in_progress"),
                Some("review"),
            ),
            change("2026-10-04T09:00:00Z", "t1", Some("review"), Some("done")),
            change("2026-10-05T09:00:00Z", "t3", None, Some("blocked")),
        ];
        let mut w = Vec::new();
        let n = rewind_backward(&mut cur, &changes, ts("2026-10-03T00:00:00Z"), &mut w);
        assert_eq!(n, 1);
        assert_eq!(
            cur[0].status, "in_progress",
            "first change after cutoff wins"
        );
        assert_eq!(cur[1].status, "done", "no later event: unchanged");
        assert_eq!(cur[2].status, "blocked", "no 'from': kept");
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("t3"));
    }

    #[test]
    fn rewind_backward_includes_event_exactly_at_cutoff() {
        let mut cur = vec![task("t1", "done")];
        let changes = vec![change(
            "2026-10-03T00:00:00Z",
            "t1",
            Some("review"),
            Some("done"),
        )];
        let mut w = Vec::new();
        let n = rewind_backward(&mut cur, &changes, ts("2026-10-03T00:00:00Z"), &mut w);
        assert_eq!(n, 1, "midnight of the next day belongs to the next day");
        assert_eq!(cur[0].status, "review");
    }

    #[test]
    fn get_snapshot_at_rejects_malformed_date() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(get_snapshot_at(tmp.path(), "2026/10/01").is_err());
        assert!(get_snapshot_at(tmp.path(), "2026-13-01").is_err());
    }

    #[test]
    fn get_snapshot_at_returns_exact_v2_snapshot_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        put(
            tmp.path(),
            "2026-10-02.json",
            &json!({"schema_version": 2, "date": "2026-10-02", "metrics": {},
                    "tasks": [{"id": "t1", "title": "A", "status": "todo"}]})
            .to_string(),
        );
        let st = get_snapshot_at(tmp.path(), "2026-10-02").unwrap();
        assert_eq!(st.source, TaskStateSource::Snapshot);
        assert!(st.exact);
        assert_eq!(st.tasks.len(), 1);
    }

    #[test]
    fn get_snapshot_at_skips_v1_snapshot_and_warns() {
        let tmp = tempfile::tempdir().unwrap();
        put(
            tmp.path(),
            "2026-10-02.json",
            &json!({"schema_version": 1, "date": "2026-10-02", "metrics": {}}).to_string(),
        );
        let st = get_snapshot_at(tmp.path(), "2026-10-02").unwrap();
        assert_eq!(st.source, TaskStateSource::EventsEstimate);
        assert!(st.warnings.iter().any(|w| w.contains("no task list")));
    }
}
