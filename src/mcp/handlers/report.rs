//! `handoff_report` (FR-513 / SPEC-513): report engine foundation.
//!
//! One tool, six actions: `generate` renders a report from a built-in (or
//! `.handoff/templates/` override) Handlebars template and stores it under
//! `.handoff/reports/`; `list` / `get` read it back; `submit` / `approve` /
//! `reject` drive the approval workflow (see
//! [`crate::report::ReportStatus::can_transition_to`]).

use std::collections::HashSet;
use std::path::Path;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use super::HandlerContext;
use crate::report::inspection::{build_inspection_data, InspectionInputs};
use crate::report::store;
use crate::report::verification::{build_verification_data, VerificationInputs};
use crate::report::{effort, weekly};
use crate::report::{ReportEngine, ReportScope, ReportStatus, ReportType};
use crate::storage::config::read_config;
use crate::storage::runs;
use crate::storage::tasks::{find_task_dir_by_id, task_status_only};
use crate::storage::test_runs::find_test_run;

pub fn handle_report(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let action = opt_str(arguments, "action")?.ok_or_else(|| {
        anyhow!("'action' parameter is required (generate|list|get|submit|approve|reject)")
    })?;
    let out = match action {
        "generate" => generate(ctx, arguments)?,
        "list" => list(ctx, arguments)?,
        "get" => get(ctx, arguments)?,
        "submit" => submit(ctx, arguments)?,
        "approve" => approve(ctx, arguments)?,
        "reject" => reject(ctx, arguments)?,
        other => bail!(
            "Unknown action '{other}' for 'action' (expected generate|list|get|submit|approve|reject)"
        ),
    };
    Ok(serde_json::to_string_pretty(&out)?)
}

/// `Ok(None)` when the key is absent or null; an error when present but not a
/// string, so a wrongly typed argument is never silently ignored.
fn opt_str<'a>(arguments: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => bail!("'{key}' must be a string"),
    }
}

fn required_str<'a>(arguments: &'a Value, key: &str) -> Result<&'a str> {
    opt_str(arguments, key)?.ok_or_else(|| anyhow!("'{key}' parameter is required"))
}

fn generate(ctx: &HandlerContext, arguments: &Value) -> Result<Value> {
    let report_type = ReportType::parse(required_str(arguments, "report_type")?)?;
    let scope: ReportScope = match arguments.get("scope") {
        None | Some(Value::Null) => ReportScope::default(),
        Some(v) => {
            serde_json::from_value(v.clone()).map_err(|e| anyhow!("Invalid 'scope': {e}"))?
        }
    };
    let data = match arguments.get("data") {
        None | Some(Value::Null) => json!({}),
        Some(v) if v.is_object() => v.clone(),
        Some(_) => bail!("'data' must be an object"),
    };

    let (scope, data) = match report_type {
        ReportType::Weekly => weekly_data(ctx, scope, data)?,
        ReportType::Effort => effort_data(ctx, scope, data)?,
        _ => (scope, data),
    };

    let data = match report_type {
        ReportType::Verification => verification_data(ctx, &scope, data)?,
        ReportType::Inspection => inspection_data(ctx, &scope, data)?,
        _ => data,
    };

    let mut engine = ReportEngine::new()?;
    let custom_templates = engine.load_custom_templates(&ctx.handoff_dir)?;
    let meta = store::create_report(
        &engine,
        &ctx.handoff_dir,
        report_type,
        scope,
        &data,
        ctx.agent_id.as_deref(),
    )?;
    Ok(json!({ "report": meta, "custom_templates": custom_templates }))
}

fn list(ctx: &HandlerContext, arguments: &Value) -> Result<Value> {
    let type_filter = opt_str(arguments, "report_type")?
        .map(ReportType::parse)
        .transpose()?;
    let status_filter = opt_str(arguments, "status")?
        .map(ReportStatus::parse)
        .transpose()?;

    let mut list = store::list_reports(&ctx.handoff_dir)?;
    list.reports.retain(|m| {
        type_filter.is_none_or(|t| m.report_type == t)
            && status_filter.is_none_or(|s| m.status == s)
    });
    Ok(json!({ "reports": list.reports, "warnings": list.warnings }))
}

fn get(ctx: &HandlerContext, arguments: &Value) -> Result<Value> {
    let report_id = required_str(arguments, "report_id")?;
    let meta = store::read_meta(&ctx.handoff_dir, report_id)?;
    let body = store::read_body(&ctx.handoff_dir, report_id)?;
    Ok(json!({ "report": meta, "body": body }))
}

fn submit(ctx: &HandlerContext, arguments: &Value) -> Result<Value> {
    let report_id = required_str(arguments, "report_id")?;
    let meta = store::transition(
        &ctx.handoff_dir,
        report_id,
        ReportStatus::Submitted,
        ctx.agent_id.as_deref(),
        opt_str(arguments, "comment")?,
    )?;
    Ok(json!({ "report": meta }))
}

/// The reviewer is the explicit `reviewer` argument, else the calling agent.
fn reviewer<'a>(ctx: &'a HandlerContext, arguments: &'a Value) -> Result<&'a str> {
    opt_str(arguments, "reviewer")?
        .or(ctx.agent_id.as_deref())
        .ok_or_else(|| anyhow!("'reviewer' parameter is required (no agent identity is known)"))
}

fn approve(ctx: &HandlerContext, arguments: &Value) -> Result<Value> {
    let report_id = required_str(arguments, "report_id")?;
    let reviewer = reviewer(ctx, arguments)?;
    let meta = store::transition(
        &ctx.handoff_dir,
        report_id,
        ReportStatus::Approved,
        Some(reviewer),
        opt_str(arguments, "comment")?,
    )?;
    Ok(json!({ "report": meta }))
}

fn reject(ctx: &HandlerContext, arguments: &Value) -> Result<Value> {
    let report_id = required_str(arguments, "report_id")?;
    let reviewer = reviewer(ctx, arguments)?;
    let comment = required_str(arguments, "comment")?;
    let meta = store::transition(
        &ctx.handoff_dir,
        report_id,
        ReportStatus::RevisionRequested,
        Some(reviewer),
        Some(comment),
    )?;
    Ok(json!({ "report": meta }))
}

/// Collects the data a verification report renders (FR-514): the trace
/// report (items, layer statuses), the latest recorded results and their
/// executors, the campaign named by `scope.campaign`, and the status of
/// tasks following up failures. Keys the caller passed in `data` overlay the
/// collected ones, so a caller can still override any value.
fn verification_data(ctx: &HandlerContext, scope: &ReportScope, overrides: Value) -> Result<Value> {
    let handoff = &ctx.handoff_dir;
    let campaign = scope
        .campaign
        .as_deref()
        .map(|id| find_test_run(handoff, id)?.ok_or_else(|| anyhow!("Campaign '{id}' not found")))
        .transpose()?;

    let trace_report = trace_items_report(ctx)?;
    let latest = runs::load_latest_readonly(handoff)?.items;
    let run_ids: HashSet<&str> = latest.values().map(|l| l.run_id.as_str()).collect();
    let executors = runs::executors_for_runs(handoff, &run_ids)?;
    let project_name = read_config(&handoff.join("config.toml"))?.project.name;
    let task_status = |id: &str| task_status_label(handoff, id);

    let mut data = build_verification_data(
        scope,
        &VerificationInputs {
            trace_report: &trace_report,
            latest: &latest,
            executors: &executors,
            task_status: &task_status,
            campaign: campaign.as_ref(),
            project_name: &project_name,
            author: ctx.agent_id.as_deref(),
            evidence_href_prefix: &evidence_href_prefix(&ctx.project_dir, handoff),
        },
    )?;
    if let (Some(collected), Value::Object(extra)) = (data.as_object_mut(), overrides) {
        collected.extend(extra);
    }
    Ok(data)
}

/// The trace report with `items[]` (and no gap list): the report needs
/// every item's id, layer, title, state, waivers and tasks.
fn trace_items_report(ctx: &HandlerContext) -> Result<Value> {
    // `limit: 0`: the report needs `items[]`, not the gap list.
    Ok(serde_json::from_str(&super::trace::handle_trace_report(
        ctx,
        &json!({ "include_items": true, "limit": 0 }),
    )?)?)
}

/// Status of a task for the failure follow-up tables. An unreadable task is
/// shown as "unknown" rather than failing the whole report: the table's job
/// is to point at the task, and the id is shown either way.
fn task_status_label(handoff_dir: &Path, id: &str) -> String {
    find_task_dir_by_id(&handoff_dir.join("tasks"), id)
        .ok()
        .flatten()
        .and_then(|dir| task_status_only(&dir).ok().flatten())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Prefix turning a project-relative evidence path into a link that resolves
/// from `<handoff>/reports/`. Falls back to the absolute project path when
/// `.handoff/` lives outside the project directory.
fn evidence_href_prefix(project_dir: &Path, handoff_dir: &Path) -> String {
    match handoff_dir.strip_prefix(project_dir) {
        Ok(rel) => "../".repeat(rel.components().count() + 1),
        Err(_) => format!("{}/", project_dir.display()),
    }
}

/// Collects the data an inspection certificate renders (FR-523): the
/// approved campaign named by `scope.campaign` (required), the trace report
/// for item titles / waivers / follow-up tasks, and the task statuses. Keys
/// the caller passed in `data` overlay the collected ones.
fn inspection_data(ctx: &HandlerContext, scope: &ReportScope, overrides: Value) -> Result<Value> {
    let handoff = &ctx.handoff_dir;
    let id = scope
        .campaign
        .as_deref()
        .ok_or_else(|| anyhow!("scope.campaign is required for an inspection report"))?;
    let campaign =
        find_test_run(handoff, id)?.ok_or_else(|| anyhow!("Campaign '{id}' not found"))?;
    let trace_report = trace_items_report(ctx)?;
    let project_name = read_config(&handoff.join("config.toml"))?.project.name;
    let task_status = |id: &str| task_status_label(handoff, id);

    let mut data = build_inspection_data(
        scope,
        &InspectionInputs {
            trace_report: &trace_report,
            task_status: &task_status,
            campaign: &campaign,
            project_name: &project_name,
            author: ctx.agent_id.as_deref(),
            evidence_href_prefix: &evidence_href_prefix(&ctx.project_dir, handoff),
        },
    )?;
    if let (Some(collected), Value::Object(extra)) = (data.as_object_mut(), overrides) {
        collected.extend(extra);
    }
    Ok(data)
}

/// Collects the data an effort report renders (FR-524) from the tasks and
/// `time_log.jsonl`, for the scope's optional period and assignee (see
/// [`effort`]). Returns the scope with a resolved period's `from`/`to` filled
/// in, so the stored report states exactly which days it covers. Keys the
/// caller passed in `data` overlay the collected ones.
fn effort_data(
    ctx: &HandlerContext,
    mut scope: ReportScope,
    overrides: Value,
) -> Result<(ReportScope, Value)> {
    effort::validate_scope(&scope)?;
    let period = effort::resolve_period(&scope)?;
    if let Some(period) = period {
        scope.from = Some(period.start.to_string());
        scope.to = Some(period.end.to_string());
    }

    let mut data =
        effort::collect_effort_data(&ctx.handoff_dir, period, scope.assignee.as_deref())?;
    if let (Some(base), Value::Object(extra)) = (data.as_object_mut(), overrides) {
        base.extend(extra);
    }
    Ok((scope, data))
}

/// Collects the data a weekly report renders (FR-515): tasks, time log,
/// status-change events, milestones, and the trace report's verification
/// progress for the scope's period (see [`weekly::resolve_period`]). Returns
/// the scope with the resolved `from`/`to` filled in, so the stored report
/// states exactly which days it covers. Keys the caller passed in `data`
/// overlay the collected ones.
fn weekly_data(
    ctx: &HandlerContext,
    mut scope: ReportScope,
    overrides: Value,
) -> Result<(ReportScope, Value)> {
    let period = weekly::resolve_period(&scope, chrono::Utc::now().date_naive())?;
    scope.from = Some(period.start.to_string());
    scope.to = Some(period.end.to_string());

    let mut data = weekly::collect_weekly_data(&ctx.handoff_dir, period)?;
    // A project whose trace graph cannot be built (broken layer document,
    // unreadable config) still gets its weekly report; the reason is rendered
    // in the Verification Progress section instead of failing the whole call.
    data["verification"] = match super::trace::handle_trace_report(ctx, &json!({ "limit": 0 }))
        .and_then(|out| serde_json::from_str::<Value>(&out).map_err(Into::into))
    {
        Ok(trace_report) => weekly::verification_block(&trace_report),
        Err(e) => weekly::verification_unavailable(&format!("{e:#}")),
    };

    if let (Some(base), Value::Object(extra)) = (data.as_object_mut(), overrides) {
        base.extend(extra);
    }
    Ok((scope, data))
}
