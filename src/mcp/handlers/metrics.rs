use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::{json, Value};

use super::HandlerContext;
use crate::storage::atomic_write;
use crate::storage::config::read_config;
use crate::storage::docs::{find_doc_by_id, read_doc};
use crate::storage::metrics_snapshots::SNAPSHOT_DIR;
use crate::storage::tasks::{build_task_index, is_terminal_status, TaskIndex};

pub fn handle(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let assignee_filter = arguments.get("assignee").and_then(|v| v.as_str());
    let plan_id = arguments.get("plan_id").and_then(|v| v.as_str());
    let result = compute_metrics(&ctx.handoff_dir, assignee_filter, plan_id)?;
    serde_json::to_string_pretty(&result).map_err(Into::into)
}

/// Version of the snapshot envelope written by [`write_snapshot`]. Bump on any
/// incompatible change to the envelope fields.
const SNAPSHOT_SCHEMA_VERSION: u32 = 1;

/// Writes today's project-wide metrics snapshot to
/// `.handoff/metrics_snapshots/<YYYY-MM-DD>.json` (UTC date, matching the
/// "today" `handoff_get_metrics` uses for overdue detection), overwriting any
/// earlier snapshot of the same day (FR-503 / SPEC-503). Returns the envelope
/// that was persisted and its `.handoff/`-relative path.
///
/// Envelope: `{ schema_version, date, captured_at, metrics }` where `metrics`
/// is the unfiltered `handoff_get_metrics` result.
pub(crate) fn write_snapshot(handoff: &Path) -> Result<(Value, String)> {
    let now = Utc::now();
    let date = now.format("%Y-%m-%d").to_string();
    let snapshot = json!({
        "schema_version": SNAPSHOT_SCHEMA_VERSION,
        "date": date,
        "captured_at": now.to_rfc3339(),
        "metrics": compute_metrics(handoff, None, None)?,
    });

    let dir = handoff.join(SNAPSHOT_DIR);
    std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    let bytes = serde_json::to_vec_pretty(&snapshot)?;
    let rel_path = format!("{SNAPSHOT_DIR}/{date}.json");
    atomic_write(handoff.join(&rel_path), &bytes)?;
    Ok((snapshot, rel_path))
}

/// `handoff_snapshot_metrics`: manual counterpart of the snapshot
/// `handoff_save_context` takes. Returns the persisted snapshot plus its
/// `.handoff/`-relative `path`.
pub fn handle_snapshot(ctx: &HandlerContext, _arguments: &Value) -> Result<String> {
    let (mut snapshot, rel_path) = write_snapshot(&ctx.handoff_dir)?;
    snapshot["path"] = json!(rel_path);
    serde_json::to_string_pretty(&snapshot).map_err(Into::into)
}

/// Dev stages that count as "covered" in `requirement_coverage.coverage_percent`
/// (`implemented` and everything beyond it).
const COVERED_DEV_STAGES: &[&str] = &["implemented", "tested", "verified"];

/// Per-milestone running totals while walking the task tree.
#[derive(Default)]
struct MilestoneAcc {
    done: u32,
    total: u32,
    estimate_hours: f64,
    actual_hours: f64,
    /// Distinct requirement `stable_id`s linked (`link_type = "requirement"`)
    /// from this milestone's tasks.
    requirement_ids: HashSet<String>,
}

/// Which tasks of the tree are counted (DS-P4-007: `assignee` and `plan_id`
/// combine with AND).
struct Filter<'a> {
    assignee: Option<&'a str>,
    /// `None` = no plan filter; `Some(ids)` = only these tasks and their
    /// descendants.
    plan_task_ids: Option<HashSet<String>>,
}

/// Accumulators filled by [`collect_metrics`].
#[derive(Default)]
struct Acc {
    total: u32,
    by_status: HashMap<String, u32>,
    estimate_sum: f64,
    actual_sum: f64,
    remaining_sum: f64,
    overdue_tasks: Vec<Value>,
    milestones: HashMap<String, MilestoneAcc>,
}

/// Task ids of the plan document `plan_id` (a document id or slug). An
/// unknown plan yields an empty set, so the metrics come back empty rather
/// than as an error (DS-P4-007).
fn plan_task_ids(handoff: &Path, plan_id: &str) -> Result<HashSet<String>> {
    let doc = match read_doc(handoff, plan_id)? {
        Some(doc) => Some(doc),
        None => find_doc_by_id(handoff, plan_id)?,
    };
    Ok(doc
        .map(|d| d.task_ids.into_iter().collect())
        .unwrap_or_default())
}

fn compute_metrics(
    handoff: &Path,
    assignee_filter: Option<&str>,
    plan_id: Option<&str>,
) -> Result<Value> {
    let tasks_dir = handoff.join("tasks");

    let (tree, _) = build_task_index(&tasks_dir, u32::MAX)?;

    let today = Utc::now().format("%Y-%m-%d").to_string();

    let filter = Filter {
        assignee: assignee_filter,
        plan_task_ids: plan_id.map(|id| plan_task_ids(handoff, id)).transpose()?,
    };
    let mut acc = Acc::default();
    collect_metrics(&tree, &filter, false, &today, &mut acc);

    let done_count = *acc.by_status.get("done").unwrap_or(&0);
    let skipped_count = *acc.by_status.get("skipped").unwrap_or(&0);
    let completion_percent = if acc.total > 0 {
        ((done_count + skipped_count) as f64 / acc.total as f64) * 100.0
    } else {
        0.0
    };

    let budget = read_budget(handoff);

    // AI-effort multiplier: the raw estimate is the human-effort estimate;
    // multiplying by ai_estimate_multiplier yields the expected AI-effort hours.
    // Raw estimates are preserved; only the adjusted view is derived here.
    let multiplier = read_config(&handoff.join("config.toml"))
        .map(|c| c.settings.ai_estimate_multiplier)
        .unwrap_or(0.2);

    let dev_stages = requirement_dev_stages(handoff);

    let milestone_list: Vec<Value> = acc
        .milestones
        .into_iter()
        .map(|(name, ms)| {
            json!({
                "name": name,
                "done": ms.done,
                "total": ms.total,
                "estimate_hours": ms.estimate_hours,
                "adjusted_estimate_hours": ms.estimate_hours * multiplier,
                "actual_hours": ms.actual_hours,
                "requirement_coverage": requirement_coverage(&ms.requirement_ids, &dev_stages),
            })
        })
        .collect();

    Ok(json!({
        "total": acc.total,
        "by_status": acc.by_status,
        "completion_percent": (completion_percent * 10.0).round() / 10.0,
        "total_estimate_hours": acc.estimate_sum,
        "ai_estimate_multiplier": multiplier,
        "total_adjusted_estimate_hours": acc.estimate_sum * multiplier,
        "total_actual_hours": acc.actual_sum,
        "total_remaining_hours": acc.remaining_sum,
        "overdue_count": acc.overdue_tasks.len(),
        "overdue_tasks": acc.overdue_tasks,
        "budget": budget,
        "milestones": milestone_list,
    }))
}

/// `stable_id -> dev_stage` for every requirement (non-`check`) item in
/// `.handoff/docs/_requirements_summary.json`. A missing or unparsable file
/// yields an empty map: the summary is a derived file regenerated from the
/// layer documents, so "no data yet" is its normal pre-first-write state and
/// must not make `handoff_get_metrics` fail (same policy as the
/// `load_context` health summaries).
fn requirement_dev_stages(handoff: &Path) -> HashMap<String, String> {
    let path = handoff.join("docs").join("_requirements_summary.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return HashMap::new();
    };
    let Ok(summary) = serde_json::from_slice::<Value>(&bytes) else {
        return HashMap::new();
    };
    summary
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|item| item.get("category").and_then(Value::as_str) != Some("check"))
                .filter_map(|item| {
                    let id = item.get("stable_id")?.as_str()?;
                    // Same fallback as `aggregate_requirements`: no dev_stage = not started.
                    let stage = item
                        .get("dev_stage")
                        .and_then(Value::as_str)
                        .unwrap_or("not_started");
                    Some((id.to_string(), stage.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Requirement progress for a set of linked `stable_id`s (DS-P4-003). Each
/// requirement counts under exactly one dev_stage; ids absent from the
/// summary (dangling links, `check` items) are not counted. `coverage_percent`
/// is the share at `implemented` or beyond (0.0 when nothing is linked).
fn requirement_coverage(ids: &HashSet<String>, dev_stages: &HashMap<String, String>) -> Value {
    let mut by_stage: HashMap<&str, u32> = HashMap::new();
    for stage in ids.iter().filter_map(|id| dev_stages.get(id)) {
        *by_stage.entry(stage.as_str()).or_insert(0) += 1;
    }
    let total: u32 = by_stage.values().sum();
    let covered: u32 = COVERED_DEV_STAGES
        .iter()
        .map(|s| by_stage.get(s).copied().unwrap_or(0))
        .sum();
    let coverage_percent = if total > 0 {
        (covered as f64 / total as f64 * 1000.0).round() / 10.0
    } else {
        0.0
    };
    let count = |stage: &str| by_stage.get(stage).copied().unwrap_or(0);
    json!({
        "total": total,
        "not_started": count("not_started"),
        "in_progress": count("in_progress"),
        "implemented": count("implemented"),
        "tested": count("tested"),
        "verified": count("verified"),
        "coverage_percent": coverage_percent,
    })
}

/// Walks `tree`, counting every task that passes `filter`. `in_plan` is true
/// once an ancestor was named by the plan, so a plan task's whole subtree is
/// counted.
fn collect_metrics(
    tree: &[TaskIndex],
    filter: &Filter<'_>,
    in_plan: bool,
    today: &str,
    acc: &mut Acc,
) {
    for node in tree {
        let node_in_plan = in_plan
            || filter
                .plan_task_ids
                .as_ref()
                .is_some_and(|ids| ids.contains(&node.id));
        let plan_ok = filter.plan_task_ids.is_none() || node_in_plan;
        let assignee_ok = match filter.assignee {
            Some(f) => node.assignee.as_deref() == Some(f),
            None => true,
        };

        if plan_ok && assignee_ok {
            acc.total += 1;
            *acc.by_status.entry(node.status.clone()).or_insert(0) += 1;

            if let Some(ref sched) = node.schedule {
                if let Some(est) = sched.estimate_hours {
                    acc.estimate_sum += est;
                }
                if let Some(act) = sched.actual_hours {
                    acc.actual_sum += act;
                }
                if let Some(rem) = sched.remaining_hours {
                    acc.remaining_sum += rem;
                }

                if let Some(ref due) = sched.due_date {
                    if !is_terminal_status(&node.status) && due.as_str() < today {
                        let days_overdue = days_between(due, today).unwrap_or(0);
                        acc.overdue_tasks.push(json!({
                            "id": node.id,
                            "title": node.title,
                            "due_date": due,
                            "days_overdue": days_overdue,
                        }));
                    }
                }

                if let Some(ref ms) = sched.milestone {
                    let entry = acc.milestones.entry(ms.clone()).or_default();
                    entry.total += 1;
                    if is_terminal_status(&node.status) {
                        entry.done += 1;
                    }
                    if let Some(est) = sched.estimate_hours {
                        entry.estimate_hours += est;
                    }
                    if let Some(act) = sched.actual_hours {
                        entry.actual_hours += act;
                    }
                    // A requirement link keeps its `stable_id` in `label`
                    // (`target` is the owning document's id).
                    entry.requirement_ids.extend(
                        node.task_links
                            .iter()
                            .filter(|l| l.link_type == "requirement")
                            .filter_map(|l| l.label.clone()),
                    );
                }
            }
        }

        collect_metrics(&node.children, filter, node_in_plan, today, acc);
    }
}

fn days_between(from: &str, to: &str) -> Option<i64> {
    let from_date = chrono::NaiveDate::parse_from_str(from, "%Y-%m-%d").ok()?;
    let to_date = chrono::NaiveDate::parse_from_str(to, "%Y-%m-%d").ok()?;
    Some((to_date - from_date).num_days())
}

fn read_budget(handoff: &Path) -> Value {
    let config_path = handoff.join("config.toml");
    let content = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(_) => return Value::Null,
    };

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("total_hours") {
            if let Some(val_str) = trimmed.split('=').nth(1) {
                if let Ok(total) = val_str.trim().parse::<f64>() {
                    return json!({ "total_hours": total });
                }
            }
        }
    }

    Value::Null
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::docs::model::DocMetadata;
    use crate::storage::docs::write_doc;
    use crate::storage::tasks::{write_task, Schedule, TaskData, TaskLink};
    use std::path::PathBuf;

    struct Fixture {
        _tmp: tempfile::TempDir,
        handoff: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(handoff.join("tasks")).unwrap();
        std::fs::create_dir_all(handoff.join("docs")).unwrap();
        Fixture { _tmp: tmp, handoff }
    }

    /// Writes a task under `parent_dir` (the tasks dir or another task's dir).
    fn add_task(
        parent_dir: &Path,
        id: &str,
        status: &str,
        assignee: Option<&str>,
        milestone: Option<&str>,
        requirement_labels: &[&str],
    ) -> PathBuf {
        let dir = parent_dir.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let data = TaskData {
            id: id.to_string(),
            title: format!("Task {id}"),
            notes: None,
            priority: None,
            created_at: None,
            updated_at: None,
            completed_at: None,
            labels: Vec::new(),
            links: Vec::new(),
            task_links: requirement_labels
                .iter()
                .map(|l| TaskLink {
                    target: "req-doc".to_string(),
                    link_type: "requirement".to_string(),
                    label: Some((*l).to_string()),
                    ..Default::default()
                })
                .collect(),
            done_criteria: Vec::new(),
            schedule: Some(Schedule {
                milestone: milestone.map(String::from),
                estimate_hours: Some(2.0),
                ..Default::default()
            }),
            dependencies: Vec::new(),
            order: None,
            assignee: assignee.map(String::from),
            lock: None,
            scope_paths: Vec::new(),
            extra: Default::default(),
        };
        write_task(&dir, status, &data).unwrap();
        dir
    }

    fn add_plan(handoff: &Path, id: &str, slug: &str, task_ids: &[&str]) {
        let mut doc = DocMetadata::new(
            id.to_string(),
            slug.to_string(),
            "Plan".to_string(),
            "plan".to_string(),
            Utc::now().to_rfc3339(),
        );
        doc.task_ids = task_ids.iter().map(|s| s.to_string()).collect();
        write_doc(handoff, &doc).unwrap();
    }

    fn write_summary(handoff: &Path, items: &[(&str, Option<&str>, &str)]) {
        let items: Vec<Value> = items
            .iter()
            .map(|(id, stage, category)| {
                let mut v = json!({ "stable_id": id, "category": category });
                if let Some(s) = stage {
                    v["dev_stage"] = json!(s);
                }
                v
            })
            .collect();
        std::fs::write(
            handoff.join("docs").join("_requirements_summary.json"),
            json!({ "total": items.len(), "items": items }).to_string(),
        )
        .unwrap();
    }

    fn milestone<'a>(metrics: &'a Value, name: &str) -> &'a Value {
        metrics["milestones"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == name)
            .unwrap_or_else(|| panic!("milestone {name} missing in {metrics}"))
    }

    #[test]
    fn plan_id_counts_only_plan_tasks_and_their_children() {
        let f = fixture();
        let tasks = f.handoff.join("tasks");
        let t1 = add_task(&tasks, "t1", "done", None, None, &[]);
        add_task(&t1, "t1.1", "todo", None, None, &[]);
        add_task(&tasks, "t2", "todo", None, None, &[]);
        add_task(&tasks, "t3", "todo", None, None, &[]);
        add_plan(&f.handoff, "plan-1", "impl-plan", &["t1", "t3"]);

        let all = compute_metrics(&f.handoff, None, None).unwrap();
        assert_eq!(all["total"], 4);

        let by_id = compute_metrics(&f.handoff, None, Some("plan-1")).unwrap();
        assert_eq!(by_id["total"], 3, "t1 + child t1.1 + t3, not t2");
        assert_eq!(by_id["by_status"]["done"], 1);
        assert_eq!(by_id["by_status"]["todo"], 2);

        let by_slug = compute_metrics(&f.handoff, None, Some("impl-plan")).unwrap();
        assert_eq!(by_slug["total"], 3);
    }

    #[test]
    fn plan_id_for_a_child_task_does_not_pull_in_its_parent() {
        let f = fixture();
        let tasks = f.handoff.join("tasks");
        let t1 = add_task(&tasks, "t1", "todo", None, None, &[]);
        add_task(&t1, "t1.1", "todo", None, None, &[]);
        add_plan(&f.handoff, "plan-1", "impl-plan", &["t1.1"]);

        let m = compute_metrics(&f.handoff, None, Some("plan-1")).unwrap();
        assert_eq!(m["total"], 1);
    }

    #[test]
    fn unknown_plan_id_yields_empty_metrics_not_an_error() {
        let f = fixture();
        add_task(&f.handoff.join("tasks"), "t1", "todo", None, None, &[]);

        let m = compute_metrics(&f.handoff, None, Some("no-such-plan")).unwrap();
        assert_eq!(m["total"], 0);
        assert_eq!(m["milestones"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn plan_id_combines_with_assignee() {
        let f = fixture();
        let tasks = f.handoff.join("tasks");
        add_task(&tasks, "t1", "todo", Some("alice"), None, &[]);
        add_task(&tasks, "t2", "todo", Some("bob"), None, &[]);
        add_task(&tasks, "t3", "todo", Some("alice"), None, &[]);
        add_plan(&f.handoff, "plan-1", "impl-plan", &["t1", "t2"]);

        let m = compute_metrics(&f.handoff, Some("alice"), Some("plan-1")).unwrap();
        assert_eq!(m["total"], 1, "only t1: in the plan AND assigned to alice");
    }

    #[test]
    fn milestone_requirement_coverage_aggregates_linked_dev_stages() {
        let f = fixture();
        let tasks = f.handoff.join("tasks");
        // Two tasks share R-2 (must count once); R-9 is dangling (not in the summary).
        add_task(&tasks, "t1", "todo", None, Some("m1"), &["R-1", "R-2"]);
        add_task(
            &tasks,
            "t2",
            "todo",
            None,
            Some("m1"),
            &["R-2", "R-3", "R-4", "R-9"],
        );
        add_task(&tasks, "t3", "todo", None, Some("m1"), &["R-5", "CHK-1"]);
        write_summary(
            &f.handoff,
            &[
                ("R-1", None, "requirement"),
                ("R-2", Some("in_progress"), "requirement"),
                ("R-3", Some("implemented"), "requirement"),
                ("R-4", Some("tested"), "requirement"),
                ("R-5", Some("verified"), "requirement"),
                ("CHK-1", Some("tested"), "check"),
                ("R-unlinked", Some("tested"), "requirement"),
            ],
        );

        let m = compute_metrics(&f.handoff, None, None).unwrap();
        let cov = &milestone(&m, "m1")["requirement_coverage"];
        assert_eq!(cov["total"], 5);
        assert_eq!(cov["not_started"], 1);
        assert_eq!(cov["in_progress"], 1);
        assert_eq!(cov["implemented"], 1);
        assert_eq!(cov["tested"], 1);
        assert_eq!(cov["verified"], 1);
        assert_eq!(cov["coverage_percent"], 60.0);
    }

    #[test]
    fn coverage_percent_is_rounded_to_one_decimal() {
        let f = fixture();
        add_task(
            &f.handoff.join("tasks"),
            "t1",
            "todo",
            None,
            Some("m1"),
            &["R-1", "R-2", "R-3"],
        );
        write_summary(
            &f.handoff,
            &[
                ("R-1", Some("implemented"), "requirement"),
                ("R-2", None, "requirement"),
                ("R-3", None, "requirement"),
            ],
        );
        let m = compute_metrics(&f.handoff, None, None).unwrap();
        assert_eq!(
            milestone(&m, "m1")["requirement_coverage"]["coverage_percent"],
            33.3
        );
    }

    #[test]
    fn milestone_without_requirement_links_or_summary_has_zero_coverage() {
        let f = fixture();
        add_task(
            &f.handoff.join("tasks"),
            "t1",
            "todo",
            None,
            Some("m1"),
            &[],
        );

        // No _requirements_summary.json at all.
        let m = compute_metrics(&f.handoff, None, None).unwrap();
        let cov = &milestone(&m, "m1")["requirement_coverage"];
        assert_eq!(cov["total"], 0);
        assert_eq!(cov["coverage_percent"], 0.0);

        // A corrupt summary degrades to the same empty coverage.
        std::fs::write(
            f.handoff.join("docs").join("_requirements_summary.json"),
            "{not json",
        )
        .unwrap();
        let m = compute_metrics(&f.handoff, None, None).unwrap();
        assert_eq!(milestone(&m, "m1")["requirement_coverage"]["total"], 0);
    }

    #[test]
    fn plan_filter_scopes_milestone_requirement_coverage() {
        let f = fixture();
        let tasks = f.handoff.join("tasks");
        add_task(&tasks, "t1", "todo", None, Some("m1"), &["R-1"]);
        add_task(&tasks, "t2", "todo", None, Some("m1"), &["R-2"]);
        write_summary(
            &f.handoff,
            &[
                ("R-1", Some("tested"), "requirement"),
                ("R-2", None, "requirement"),
            ],
        );
        add_plan(&f.handoff, "plan-1", "impl-plan", &["t1"]);

        let m = compute_metrics(&f.handoff, None, Some("plan-1")).unwrap();
        let ms = milestone(&m, "m1");
        assert_eq!(ms["total"], 1);
        assert_eq!(ms["requirement_coverage"]["total"], 1);
        assert_eq!(ms["requirement_coverage"]["coverage_percent"], 100.0);
    }
}
