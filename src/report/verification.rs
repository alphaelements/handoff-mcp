//! Data collection for the verification report (R1: STR/ATR — FR-514 /
//! SPEC-514).
//!
//! [`build_verification_data`] is pure: the caller (`handoff_report
//! generate`) loads the trace report, the latest recorded results, the
//! campaign and the task statuses, and this module turns them into the
//! `data` object `verification.md.hbs` renders. Two sources of verdicts:
//!
//! - **campaign** (`scope.campaign`): the campaign checklist, one row per
//!   checklist entry, with its own result / evidence / note / verifier;
//! - **layer / item scope**: every trace item in the selected layers (or the
//!   named items), with its latest recorded run (`runs/_latest.json`), or —
//!   when the item was never run — a waiver or the graph-derived state.
//!
//! `scope.layers`, `scope.items` and `scope.statuses` narrow either source;
//! the summary and the per-layer table are computed over the rows that
//! remain, so the report is always internally consistent.

use std::collections::{HashMap, HashSet};

use anyhow::{bail, Result};
use serde::Serialize;
use serde_json::{json, Map, Value};

use super::ReportScope;
use crate::storage::runs::LatestItemResult;
use crate::storage::test_runs::{CheckResult, TestRunRecord};

/// Everything [`build_verification_data`] needs, already loaded by the caller.
pub struct VerificationInputs<'a> {
    /// `handoff_trace_report` output including `items[]` (needs `id`,
    /// `layer`, `title`, `state`, `waivers`, `tasks`) plus `trace_layers` and
    /// `layer_statuses`.
    pub trace_report: &'a Value,
    /// `runs/_latest.json`'s per-item latest results.
    pub latest: &'a HashMap<String, LatestItemResult>,
    /// `run_id` -> who executed it (display string).
    pub executors: &'a HashMap<String, String>,
    /// Status of a task by id, for the failure follow-up table. Only called
    /// for tasks linked to failed / blocked rows.
    pub task_status: &'a dyn Fn(&str) -> String,
    /// The campaign named by `scope.campaign`.
    pub campaign: Option<&'a TestRunRecord>,
    pub project_name: &'a str,
    /// Who generated the report (the report's author / verifier).
    pub author: Option<&'a str>,
    /// Prefix that turns a project-relative evidence path into a link valid
    /// from the report file's own directory (e.g. `../../`).
    pub evidence_href_prefix: &'a str,
}

/// Result values a report row can carry, in display order.
const RESULTS: [CheckResult; 5] = [
    CheckResult::Pass,
    CheckResult::Fail,
    CheckResult::Blocked,
    CheckResult::Waived,
    CheckResult::Pending,
];

#[derive(Debug, Serialize)]
struct EvidenceLink {
    path: String,
    href: String,
    #[serde(rename = "type")]
    evidence_type: String,
    caption: String,
    /// Ready-to-print Markdown link; safe inside a table cell.
    link: String,
}

#[derive(Debug, Serialize)]
struct TaskRef {
    id: String,
    status: String,
}

#[derive(Debug, Serialize)]
pub(super) struct Row {
    pub(super) item_id: String,
    pub(super) title: String,
    pub(super) layer: String,
    pub(super) result: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    executed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    verified_by: Option<String>,
    evidence: Vec<EvidenceLink>,
    pub(super) note: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    acceptance_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) waiver_reason: Option<String>,
    tasks: Vec<TaskRef>,
}

/// The trace-report facts about one item that the rows need.
pub(super) struct ItemMeta {
    title: String,
    layer: String,
    state: String,
    verify_waiver: Option<String>,
    task_ids: Vec<String>,
}

pub(super) fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

pub(super) fn index_items(trace_report: &Value) -> HashMap<String, ItemMeta> {
    let Some(items) = trace_report.get("items").and_then(Value::as_array) else {
        return HashMap::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let id = str_field(item, "id")?;
            let verify_waiver = item
                .get("waivers")
                .and_then(Value::as_array)
                .and_then(|ws| {
                    ws.iter()
                        .find(|w| str_field(w, "axis") == Some("verify"))
                        .map(|w| str_field(w, "reason").unwrap_or_default().to_string())
                });
            let task_ids = item
                .get("tasks")
                .and_then(Value::as_array)
                .map(|ts| {
                    ts.iter()
                        .filter_map(|t| str_field(t, "id").map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            Some((
                id.to_string(),
                ItemMeta {
                    title: str_field(item, "title").unwrap_or_default().to_string(),
                    layer: str_field(item, "layer").unwrap_or_default().to_string(),
                    state: str_field(item, "state").unwrap_or_default().to_string(),
                    verify_waiver,
                    task_ids,
                },
            ))
        })
        .collect()
}

fn in_use_layers(trace_report: &Value) -> Vec<String> {
    trace_report
        .get("trace_layers")
        .and_then(|t| t.get("in_use"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|l| l.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn has_scheme(path: &str) -> bool {
    path.contains("://") || path.starts_with("mailto:")
}

/// Percent-encodes the characters that would end a Markdown link target or a
/// table cell early.
fn encode_href(href: &str) -> String {
    let mut out = String::with_capacity(href.len());
    for c in href.chars() {
        match c {
            ' ' => out.push_str("%20"),
            '(' => out.push_str("%28"),
            ')' => out.push_str("%29"),
            '|' => out.push_str("%7C"),
            '<' => out.push_str("%3C"),
            '>' => out.push_str("%3E"),
            c if c.is_control() => out.push_str(&format!("%{:02X}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn escape_link_label(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    for c in label.chars() {
        match c {
            '[' | ']' | '|' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\n' | '\r' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

fn evidence_link(prefix: &str, path: &str, evidence_type: &str, caption: &str) -> EvidenceLink {
    let absolute = has_scheme(path) || path.starts_with('/') || path.starts_with('#');
    let href = if absolute {
        encode_href(path)
    } else {
        encode_href(&format!("{prefix}{path}"))
    };
    let label = if caption.trim().is_empty() {
        path
    } else {
        caption
    };
    EvidenceLink {
        link: format!("[{}]({href})", escape_link_label(label)),
        path: path.to_string(),
        href,
        evidence_type: evidence_type.to_string(),
        caption: caption.to_string(),
    }
}

/// Maps a recorded run result / graph state to a report verdict. `None` for
/// values that carry no verdict (`not_run`, `skipped`, ...).
fn verdict_from_name(name: &str) -> Option<CheckResult> {
    match name {
        "pass" | "passing" => Some(CheckResult::Pass),
        "fail" | "failing" => Some(CheckResult::Fail),
        "blocked" => Some(CheckResult::Blocked),
        _ => None,
    }
}

/// The follow-up tasks of a row. Only failed / blocked rows list them (the
/// "Failed and Blocked Items" table is their only consumer), so the status
/// lookup is not paid for passing rows.
fn task_refs(
    result: CheckResult,
    ids: &[String],
    task_status: &dyn Fn(&str) -> String,
) -> Vec<TaskRef> {
    if !matches!(result, CheckResult::Fail | CheckResult::Blocked) {
        return Vec::new();
    }
    ids.iter()
        .map(|id| TaskRef {
            id: id.clone(),
            status: task_status(id),
        })
        .collect()
}

/// One row per checklist entry. `task_status` and `evidence_href_prefix`
/// are the corresponding [`VerificationInputs`] fields.
pub(super) fn campaign_rows(
    campaign: &TestRunRecord,
    items: &HashMap<String, ItemMeta>,
    task_status: &dyn Fn(&str) -> String,
    evidence_href_prefix: &str,
) -> Vec<Row> {
    campaign
        .checklist
        .iter()
        .map(|check| {
            let meta = items.get(&check.item_id);
            Row {
                item_id: check.item_id.clone(),
                title: meta
                    .map(|m| m.title.clone())
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| check.acceptance_text.clone()),
                layer: meta.map(|m| m.layer.clone()).unwrap_or_default(),
                result: check.result.as_str(),
                executed_at: check.verified_at.clone(),
                verified_by: check.verified_by.clone(),
                evidence: check
                    .evidence
                    .iter()
                    .map(|e| {
                        evidence_link(evidence_href_prefix, &e.path, &e.evidence_type, &e.caption)
                    })
                    .collect(),
                note: check.note.clone(),
                acceptance_text: Some(check.acceptance_text.clone()),
                waiver_reason: meta.and_then(|m| m.verify_waiver.clone()),
                tasks: meta
                    .map(|m| task_refs(check.result, &m.task_ids, task_status))
                    .unwrap_or_default(),
            }
        })
        .collect()
}

fn item_rows(
    items: &HashMap<String, ItemMeta>,
    layer_order: &[String],
    inputs: &VerificationInputs,
) -> Vec<Row> {
    let layer_rank = |layer: &str| {
        layer_order
            .iter()
            .position(|l| l == layer)
            .unwrap_or(layer_order.len())
    };
    let mut ids: Vec<&String> = items.keys().collect();
    ids.sort_by(|a, b| {
        (layer_rank(&items[*a].layer), a.as_str()).cmp(&(layer_rank(&items[*b].layer), b.as_str()))
    });
    ids.into_iter()
        .map(|id| {
            let meta = &items[id];
            let latest = inputs.latest.get(id);
            let result = latest
                .and_then(|l| verdict_from_name(&l.result))
                .or_else(|| meta.verify_waiver.is_some().then_some(CheckResult::Waived))
                .or_else(|| verdict_from_name(&meta.state))
                .unwrap_or(CheckResult::Pending);
            Row {
                item_id: id.clone(),
                title: meta.title.clone(),
                layer: meta.layer.clone(),
                result: result.as_str(),
                executed_at: latest.map(|l| l.executed_at.clone()),
                verified_by: latest.and_then(|l| inputs.executors.get(&l.run_id).cloned()),
                evidence: latest
                    .map(|l| {
                        l.evidence
                            .iter()
                            .map(|path| {
                                let kind = if has_scheme(path) { "url" } else { "file" };
                                evidence_link(inputs.evidence_href_prefix, path, kind, "")
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                note: latest.map(|l| l.note.clone()).unwrap_or_default(),
                acceptance_text: None,
                waiver_reason: meta.verify_waiver.clone(),
                tasks: task_refs(result, &meta.task_ids, inputs.task_status),
            }
        })
        .collect()
}

fn percent(count: usize, total: usize) -> f64 {
    if total == 0 {
        return 0.0;
    }
    ((count as f64 / total as f64) * 1000.0).round() / 10.0
}

fn count_of(rows: &[&Row], result: CheckResult) -> usize {
    rows.iter().filter(|r| r.result == result.as_str()).count()
}

pub(super) fn summary_json(rows: &[&Row]) -> Value {
    let total = rows.len();
    let mut map = Map::new();
    map.insert("total".into(), json!(total));
    for result in RESULTS {
        let count = count_of(rows, result);
        map.insert(result.as_str().into(), json!(count));
        map.insert(
            format!("{}_pct", result.as_str()),
            json!(percent(count, total)),
        );
    }
    Value::Object(map)
}

/// Validates `scope.statuses` entries against the verdict vocabulary.
fn parse_statuses(statuses: &[String]) -> Result<HashSet<&'static str>> {
    let mut out = HashSet::new();
    for s in statuses {
        match CheckResult::parse(s) {
            Some(r) => {
                out.insert(r.as_str());
            }
            None => bail!(
                "Unknown status '{s}' in scope.statuses (expected one of: {})",
                RESULTS.map(|r| r.as_str()).join(", ")
            ),
        }
    }
    Ok(out)
}

/// Builds the `data` object for `verification.md.hbs`. See the module docs
/// for the scope semantics.
pub fn build_verification_data(scope: &ReportScope, inputs: &VerificationInputs) -> Result<Value> {
    let status_filter = parse_statuses(&scope.statuses)?;
    let items = index_items(inputs.trace_report);
    let layer_order = in_use_layers(inputs.trace_report);
    let mut warnings: Vec<String> = Vec::new();

    let candidates = match (scope.campaign.as_deref(), inputs.campaign) {
        (Some(_), Some(campaign)) => campaign_rows(
            campaign,
            &items,
            inputs.task_status,
            inputs.evidence_href_prefix,
        ),
        (Some(id), None) => bail!("Campaign '{id}' not found"),
        (None, _) => item_rows(&items, &layer_order, inputs),
    };

    let known_ids: HashSet<&str> = candidates.iter().map(|r| r.item_id.as_str()).collect();
    for id in &scope.items {
        if !known_ids.contains(id.as_str()) {
            warnings.push(format!("Item '{id}' was not found in the report scope"));
        }
    }
    let known_layers: HashSet<&str> = layer_order
        .iter()
        .map(String::as_str)
        .chain(candidates.iter().map(|r| r.layer.as_str()))
        .collect();
    for layer in &scope.layers {
        if !known_layers.contains(layer.as_str()) {
            warnings.push(format!("Layer '{layer}' is not an in-use layer"));
        }
    }

    let rows: Vec<Row> = candidates
        .into_iter()
        .filter(|r| scope.layers.is_empty() || scope.layers.contains(&r.layer))
        .filter(|r| scope.items.is_empty() || scope.items.contains(&r.item_id))
        .filter(|r| status_filter.is_empty() || status_filter.contains(r.result))
        .collect();
    let all: Vec<&Row> = rows.iter().collect();

    let layer_statuses = inputs.trace_report.get("layer_statuses");
    let mut layer_names: Vec<String> = layer_order
        .iter()
        .filter(|l| rows.iter().any(|r| &r.layer == *l) || scope.layers.contains(l))
        .cloned()
        .collect();
    for row in &rows {
        if !layer_names.contains(&row.layer) {
            layer_names.push(row.layer.clone());
        }
    }
    let layers: Vec<Value> = layer_names
        .iter()
        .map(|name| {
            let in_layer: Vec<&Row> = rows.iter().filter(|r| &r.layer == name).collect();
            let mut summary = summary_json(&in_layer);
            summary["layer"] = json!(if name.is_empty() { "(unknown)" } else { name });
            summary["status"] = json!(layer_statuses
                .and_then(|s| str_field(s, name))
                .unwrap_or("-"));
            summary
        })
        .collect();

    let failures: Vec<&Row> = rows
        .iter()
        .filter(|r| {
            r.result == CheckResult::Fail.as_str() || r.result == CheckResult::Blocked.as_str()
        })
        .collect();
    let approver = inputs.campaign.and_then(|c| c.approved_by.clone());
    let waived: Vec<Value> = rows
        .iter()
        .filter(|r| r.result == CheckResult::Waived.as_str())
        .map(|r| {
            let reason = r
                .waiver_reason
                .clone()
                .filter(|s| !s.is_empty())
                .or_else(|| Some(r.note.clone()).filter(|s| !s.is_empty()))
                .unwrap_or_default();
            json!({
                "item_id": r.item_id,
                "title": r.title,
                "layer": r.layer,
                "reason": reason,
                "approver": approver.clone().or_else(|| r.verified_by.clone()).unwrap_or_default(),
            })
        })
        .collect();

    let campaign = inputs.campaign.map(|c| {
        json!({
            "id": c.test_run_id,
            "label": c.label,
            "status": c.campaign_status.as_str(),
            "completed_at": c.completed_at,
            "approved_by": c.approved_by,
            "approved_at": c.approved_at,
        })
    });

    Ok(json!({
        "project_name": inputs.project_name,
        "author": inputs.author,
        "campaign": campaign,
        "summary": summary_json(&all),
        "layers": layers,
        "results": rows,
        "failures": failures,
        "waived": waived,
        "warnings": warnings,
    }))
}

#[cfg(test)]
mod tests;
