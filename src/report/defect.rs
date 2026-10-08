//! Defect register data (R7: FR-531 / SPEC-531).
//!
//! The register lists the project's bug tasks (label `bug`; the FR-522 bug
//! flow adds `bug:<fix|defer|waive|not_a_bug>` for the disposition) and the
//! trace items that currently fail, so a failure nobody has filed a bug for
//! is visible. [`build_defect_data`] is a pure function from the tasks and a
//! `handoff_trace_report` result (with `items[]`) to the JSON `defect.md.hbs`
//! renders as `data`.
//!
//! Output shape (absent values are `null`):
//!
//! ```text
//! filters        { period: { start, end } | null, assignee, layers, items }
//! trace          { available, error }
//! stats          { total, open, closed, open_without_disposition,
//!                  failing_items, failing_items_without_open_bug,
//!                  by_status, by_priority, by_disposition }   by_*: [ { name, count } ]
//! defects        [ { id, title, status, priority, disposition, assignee,
//!                    created_at, completed_at, items, layers } ]
//! failing_items  [ { id, layer, title, tasks: [ { id, role } ], bug_tasks, has_open_bug } ]
//! warnings       [ string ]
//! ```
//!
//! Filters (all optional): `period` keeps bugs whose `created_at` lies in it,
//! `assignee` keeps that person's bugs, and `layers` / `items` keep bugs
//! linked (task `requirement` link) to a matching trace item and the failing
//! items that match. A bug's `priority` is its task priority (its severity).
//! `bug_tasks` of a failing item lists every bug task linked to it, whatever
//! the filters; `has_open_bug` is true when one of them is not done/skipped.

use std::collections::{BTreeMap, HashMap};

use anyhow::{bail, Result};
use serde_json::{json, Value};

use super::period::Period;
use super::weekly::{natural_cmp, ts_date};
use super::{ReportScope, ReportType};
use crate::storage::tasks::{is_terminal_status, TaskData};

/// Label marking a task as a bug.
pub const BUG_LABEL: &str = "bug";

/// Prefix of the disposition label (`bug:fix`).
const DISPOSITION_PREFIX: &str = "bug:";

/// Dispositions the bug flow defines (`handoff_trace_update` docs).
const DISPOSITIONS: [&str; 4] = ["fix", "defer", "waive", "not_a_bug"];

/// `link_type` of a task link to a requirement / trace item.
const REQUIREMENT_LINK: &str = "requirement";

/// Group name for a bug without a priority / disposition.
const NO_PRIORITY: &str = "(none)";
const UNDECIDED: &str = "undecided";

/// Fields a defect report accepts besides `label`.
const SUPPORTED_SCOPE: &[&str] = &["period", "from", "to", "layers", "items", "assignee"];

pub fn validate_scope(scope: &ReportScope) -> Result<()> {
    scope.require_only(
        ReportType::Defect,
        SUPPORTED_SCOPE,
        "use scope.period / scope.from / scope.to / scope.layers / scope.items / scope.assignee",
    )
}

/// Whether `task` is a bug.
pub fn is_bug(task: &TaskData) -> bool {
    task.labels.iter().any(|l| l == BUG_LABEL)
}

fn disposition(task: &TaskData) -> Option<&str> {
    task.labels
        .iter()
        .find_map(|l| l.strip_prefix(DISPOSITION_PREFIX))
}

/// Ids of the trace items `task` is linked to. A requirement link stores the
/// item's stable id in its `label` (its `target` is the layer document), as
/// the trace adapter reads it.
fn linked_items(task: &TaskData) -> Vec<String> {
    task.links()
        .into_iter()
        .filter(|l| l.link_type == REQUIREMENT_LINK)
        .filter_map(|l| l.label)
        .collect()
}

pub struct DefectInputs<'a> {
    /// Every task with its current status.
    pub tasks: &'a [(TaskData, String)],
    /// The trace report with `items[]`, or why it could not be built (the
    /// bug list is still produced; the failing-item section is not).
    pub trace: std::result::Result<&'a Value, &'a str>,
    pub period: Option<Period>,
    pub assignee: Option<&'a str>,
    pub layers: &'a [String],
    pub items: &'a [String],
}

/// A trace item as far as the register needs it.
struct TraceItem<'a> {
    id: &'a str,
    layer: &'a str,
    title: &'a str,
    state: &'a str,
    tasks: Vec<Value>,
}

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn trace_items(report: &Value) -> Vec<TraceItem<'_>> {
    report
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some(TraceItem {
                        id: str_of(item, "id")?,
                        layer: str_of(item, "layer").unwrap_or_default(),
                        title: str_of(item, "title").unwrap_or_default(),
                        state: str_of(item, "state").unwrap_or_default(),
                        tasks: item
                            .get("tasks")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn counted(counts: BTreeMap<String, usize>) -> Vec<Value> {
    counts
        .into_iter()
        .map(|(name, count)| json!({ "name": name, "count": count }))
        .collect()
}

pub fn build_defect_data(inputs: &DefectInputs) -> Result<Value> {
    let item_filter = !inputs.layers.is_empty() || !inputs.items.is_empty();
    let trace_items = match inputs.trace {
        Ok(report) => trace_items(report),
        Err(reason) if item_filter => {
            bail!("scope.layers / scope.items need the trace graph, which is unavailable: {reason}")
        }
        Err(_) => Vec::new(),
    };
    let by_id: HashMap<&str, &TraceItem> = trace_items.iter().map(|i| (i.id, i)).collect();
    let item_matches = |id: &str| -> bool {
        let layer_ok =
            |layer: &str| inputs.layers.is_empty() || inputs.layers.iter().any(|l| l == layer);
        let id_ok = inputs.items.is_empty() || inputs.items.iter().any(|i| i == id);
        by_id.get(id).is_some_and(|i| layer_ok(i.layer)) && id_ok
    };

    let mut warnings: Vec<String> = Vec::new();
    let mut bugs: Vec<&(TaskData, String)> =
        inputs.tasks.iter().filter(|(d, _)| is_bug(d)).collect();
    bugs.sort_by(|a, b| natural_cmp(&a.0.id, &b.0.id));

    for (data, _) in &bugs {
        if let Some(value) = disposition(data) {
            if !DISPOSITIONS.contains(&value) {
                warnings.push(format!(
                    "task {} has unknown bug disposition '{value}' (expected {})",
                    data.id,
                    DISPOSITIONS.join("|")
                ));
            }
        }
    }

    // Bug tasks per item, over all bugs: a failing item is "covered" by a bug
    // whether or not the bug passes this report's filters.
    let mut bugs_of_item: HashMap<String, Vec<&(TaskData, String)>> = HashMap::new();
    for bug in &bugs {
        for item in linked_items(&bug.0) {
            bugs_of_item.entry(item).or_default().push(bug);
        }
    }

    let selected: Vec<&&(TaskData, String)> = bugs
        .iter()
        .filter(|(data, _)| inputs.assignee.is_none_or(|a| data.assignee.as_deref() == Some(a)))
        .filter(|(data, _)| {
            let Some(period) = inputs.period else {
                return true;
            };
            match data.created_at.as_deref().map(ts_date) {
                Some(Some(date)) => period.contains(date),
                _ => {
                    warnings.push(format!(
                        "task {} has a missing or unparseable created_at and was left out of the period",
                        data.id
                    ));
                    false
                }
            }
        })
        .filter(|(data, _)| !item_filter || linked_items(data).iter().any(|id| item_matches(id)))
        .collect();

    let mut by_status: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_priority: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_disposition: BTreeMap<String, usize> = BTreeMap::new();
    let mut open = 0usize;
    let mut open_without_disposition = 0usize;
    let defects: Vec<Value> = selected
        .iter()
        .map(|(data, status)| {
            let disp = disposition(data);
            if !is_terminal_status(status) {
                open += 1;
                if disp.is_none() {
                    open_without_disposition += 1;
                }
            }
            *by_status.entry(status.clone()).or_default() += 1;
            *by_priority
                .entry(data.priority.clone().unwrap_or_else(|| NO_PRIORITY.into()))
                .or_default() += 1;
            *by_disposition
                .entry(disp.unwrap_or(UNDECIDED).to_string())
                .or_default() += 1;

            let items = linked_items(data);
            let mut layers: Vec<&str> = items
                .iter()
                .filter_map(|id| by_id.get(id.as_str()).map(|i| i.layer))
                .collect();
            layers.sort_unstable();
            layers.dedup();
            json!({
                "id": data.id,
                "title": data.title,
                "status": status,
                "priority": data.priority,
                "disposition": disp,
                "assignee": data.assignee,
                "created_at": data.created_at,
                "completed_at": data.completed_at,
                "items": items,
                "layers": layers,
            })
        })
        .collect();

    let mut failing: Vec<&TraceItem> = trace_items
        .iter()
        .filter(|i| i.state == "failing" && item_matches(i.id))
        .collect();
    failing.sort_by(|a, b| natural_cmp(a.id, b.id));
    let mut without_open_bug = 0usize;
    let failing_items: Vec<Value> = failing
        .iter()
        .map(|item| {
            let linked = bugs_of_item
                .get(item.id)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let has_open_bug = linked.iter().any(|(_, s)| !is_terminal_status(s));
            if !has_open_bug {
                without_open_bug += 1;
            }
            json!({
                "id": item.id,
                "layer": item.layer,
                "title": item.title,
                "tasks": item.tasks,
                "bug_tasks": linked.iter().map(|(d, _)| d.id.as_str()).collect::<Vec<_>>(),
                "has_open_bug": has_open_bug,
            })
        })
        .collect();

    let (available, error) = match inputs.trace {
        Ok(_) => (true, None),
        Err(reason) => (false, Some(reason)),
    };
    Ok(json!({
        "filters": {
            "period": inputs.period.map(|p| json!({
                "start": p.start.to_string(),
                "end": p.end.to_string(),
            })),
            "assignee": inputs.assignee,
            "layers": inputs.layers,
            "items": inputs.items,
        },
        "trace": { "available": available, "error": error },
        "stats": {
            "total": defects.len(),
            "open": open,
            "closed": defects.len() - open,
            "open_without_disposition": open_without_disposition,
            "failing_items": failing_items.len(),
            "failing_items_without_open_bug": without_open_bug,
            "by_status": counted(by_status),
            "by_priority": counted(by_priority),
            "by_disposition": counted(by_disposition),
        },
        "defects": defects,
        "failing_items": failing_items,
        "warnings": warnings,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::tasks::TaskLink;

    fn task(id: &str, status: &str, f: impl FnOnce(&mut TaskData)) -> (TaskData, String) {
        let mut data = TaskData {
            id: id.into(),
            title: format!("Title {id}"),
            notes: None,
            priority: None,
            created_at: None,
            updated_at: None,
            completed_at: None,
            labels: vec![],
            links: vec![],
            task_links: vec![],
            done_criteria: vec![],
            schedule: None,
            dependencies: vec![],
            order: None,
            assignee: None,
            lock: None,
            scope_paths: vec![],
            extra: HashMap::new(),
        };
        f(&mut data);
        (data, status.into())
    }

    fn bug(id: &str, status: &str, f: impl FnOnce(&mut TaskData)) -> (TaskData, String) {
        task(id, status, |t| {
            t.labels = vec![BUG_LABEL.into()];
            f(t)
        })
    }

    fn link(t: &mut TaskData, item: &str) {
        t.task_links.push(TaskLink {
            target: "doc-1".into(),
            link_type: REQUIREMENT_LINK.into(),
            label: Some(item.into()),
            ..Default::default()
        });
    }

    fn trace() -> Value {
        json!({ "items": [
            { "id": "AT-1", "layer": "acceptance", "title": "a", "state": "failing",
              "tasks": [{ "id": "b1", "role": "implements" }] },
            { "id": "ST-2", "layer": "system", "title": "s", "state": "failing", "tasks": [] },
            { "id": "ST-10", "layer": "system", "title": "s10", "state": "passing", "tasks": [] },
            { "id": "REQ-1", "layer": "requirement", "title": "r", "state": "uncovered", "tasks": [] },
        ]})
    }

    fn build<'a>(
        tasks: &'a [(TaskData, String)],
        trace: &'a Value,
        f: impl FnOnce(&mut DefectInputs<'a>),
    ) -> Result<Value> {
        let mut inputs = DefectInputs {
            tasks,
            trace: Ok(trace),
            period: None,
            assignee: None,
            layers: &[],
            items: &[],
        };
        f(&mut inputs);
        build_defect_data(&inputs)
    }

    fn ids(v: &Value) -> Vec<&str> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn scope_allows_filters_only() {
        let ok = ReportScope {
            layers: vec!["system".into()],
            assignee: Some("a".into()),
            period: Some("2026-10".into()),
            ..Default::default()
        };
        assert!(validate_scope(&ok).is_ok());
        for bad in [
            ReportScope {
                campaign: Some("c".into()),
                ..Default::default()
            },
            ReportScope {
                milestone: Some("m".into()),
                ..Default::default()
            },
            ReportScope {
                statuses: vec!["fail".into()],
                ..Default::default()
            },
        ] {
            assert!(validate_scope(&bad)
                .unwrap_err()
                .to_string()
                .contains("defect"));
        }
    }

    #[test]
    fn only_bug_labelled_tasks_are_listed_in_natural_order() {
        let tasks = [
            bug("b10", "todo", |_| {}),
            bug("b2", "done", |_| {}),
            task("t1", "todo", |_| {}),
        ];
        let data = build(&tasks, &trace(), |_| {}).unwrap();
        assert_eq!(ids(&data["defects"]), ["b2", "b10"]);
        assert_eq!(data["stats"]["total"], 2);
        assert_eq!(data["stats"]["open"], 1);
        assert_eq!(data["stats"]["closed"], 1);
    }

    #[test]
    fn dispositions_priorities_and_statuses_are_grouped() {
        let tasks = [
            bug("b1", "todo", |t| {
                t.labels.push("bug:fix".into());
                t.priority = Some("high".into());
            }),
            bug("b2", "todo", |_| {}),
            bug("b3", "done", |t| t.labels.push("bug:defer".into())),
            bug("b4", "todo", |t| t.labels.push("bug:fixx".into())),
        ];
        let data = build(&tasks, &trace(), |_| {}).unwrap();
        let stats = &data["stats"];
        assert_eq!(data["defects"][0]["disposition"], "fix");
        assert!(data["defects"][1]["disposition"].is_null());
        assert_eq!(
            stats["by_disposition"],
            json!([
                {"name": "defer", "count": 1}, {"name": "fix", "count": 1},
                {"name": "fixx", "count": 1}, {"name": "undecided", "count": 1}
            ])
        );
        assert_eq!(
            stats["by_priority"],
            json!([{"name": "(none)", "count": 3}, {"name": "high", "count": 1}])
        );
        assert_eq!(
            stats["by_status"],
            json!([{"name": "done", "count": 1}, {"name": "todo", "count": 3}])
        );
        // b1 has a disposition; b2 does not; b4's is unknown but present.
        assert_eq!(stats["open_without_disposition"], 1);
        let warnings = data["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].as_str().unwrap().contains("fixx"));
    }

    #[test]
    fn failing_items_show_their_bug_tasks_and_whether_one_is_open() {
        let tasks = [
            bug("b1", "todo", |t| link(t, "AT-1")),
            bug("b2", "done", |t| link(t, "ST-2")),
        ];
        let data = build(&tasks, &trace(), |_| {}).unwrap();
        // Only failing items; natural order (ST-2 before ST-10 is moot here).
        assert_eq!(ids(&data["failing_items"]), ["AT-1", "ST-2"]);
        assert_eq!(data["failing_items"][0]["bug_tasks"], json!(["b1"]));
        assert_eq!(data["failing_items"][0]["has_open_bug"], true);
        assert_eq!(data["failing_items"][1]["bug_tasks"], json!(["b2"]));
        assert_eq!(
            data["failing_items"][1]["has_open_bug"], false,
            "a closed bug does not cover a still-failing item"
        );
        assert_eq!(data["stats"]["failing_items"], 2);
        assert_eq!(data["stats"]["failing_items_without_open_bug"], 1);
        assert_eq!(data["defects"][0]["items"], json!(["AT-1"]));
        assert_eq!(data["defects"][0]["layers"], json!(["acceptance"]));
    }

    #[test]
    fn layer_and_item_filters_apply_to_bugs_and_failing_items() {
        let tasks = [
            bug("b1", "todo", |t| link(t, "AT-1")),
            bug("b2", "todo", |t| link(t, "ST-2")),
            bug("b3", "todo", |_| {}),
        ];
        let layers = ["system".to_string()];
        let data = build(&tasks, &trace(), |i| i.layers = &layers).unwrap();
        assert_eq!(ids(&data["defects"]), ["b2"]);
        assert_eq!(ids(&data["failing_items"]), ["ST-2"]);

        let items = ["AT-1".to_string()];
        let data = build(&tasks, &trace(), |i| i.items = &items).unwrap();
        assert_eq!(ids(&data["defects"]), ["b1"]);

        // Both given: both must match.
        let data = build(&tasks, &trace(), |i| {
            i.layers = &layers;
            i.items = &items;
        })
        .unwrap();
        assert!(data["defects"].as_array().unwrap().is_empty());
        assert!(data["failing_items"].as_array().unwrap().is_empty());
    }

    #[test]
    fn period_filters_by_created_at_and_warns_about_missing_dates() {
        let tasks = [
            bug("b1", "todo", |t| {
                t.created_at = Some("2026-10-02T09:00:00+00:00".into())
            }),
            bug("b2", "todo", |t| {
                t.created_at = Some("2026-09-02T09:00:00+00:00".into())
            }),
            bug("b3", "todo", |_| {}),
        ];
        let period = Period::parse_month("2026-10");
        let data = build(&tasks, &trace(), |i| i.period = period).unwrap();
        assert_eq!(ids(&data["defects"]), ["b1"]);
        assert_eq!(data["filters"]["period"]["start"], "2026-10-01");
        let warnings = data["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].as_str().unwrap().contains("b3"));
    }

    #[test]
    fn assignee_filter_is_an_exact_match() {
        let tasks = [
            bug("b1", "todo", |t| t.assignee = Some("alice".into())),
            bug("b2", "todo", |t| t.assignee = Some("alice2".into())),
        ];
        let data = build(&tasks, &trace(), |i| i.assignee = Some("alice")).unwrap();
        assert_eq!(ids(&data["defects"]), ["b1"]);
    }

    #[test]
    fn unavailable_trace_keeps_the_bug_list_but_refuses_item_filters() {
        let tasks = [bug("b1", "todo", |_| {})];
        let inputs = DefectInputs {
            tasks: &tasks,
            trace: Err("broken layer doc"),
            period: None,
            assignee: None,
            layers: &[],
            items: &[],
        };
        let data = build_defect_data(&inputs).unwrap();
        assert_eq!(data["trace"]["available"], false);
        assert_eq!(data["trace"]["error"], "broken layer doc");
        assert_eq!(ids(&data["defects"]), ["b1"]);
        assert!(data["failing_items"].as_array().unwrap().is_empty());

        let layers = ["system".to_string()];
        let filtered = DefectInputs {
            layers: &layers,
            ..inputs
        };
        assert!(build_defect_data(&filtered)
            .unwrap_err()
            .to_string()
            .contains("broken layer doc"));
    }
}
