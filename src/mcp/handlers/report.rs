//! `handoff_report` (FR-513 / SPEC-513): report engine foundation.
//!
//! One tool, seven actions: `generate` renders a report from a built-in (or
//! `.handoff/templates/` override) Handlebars template and stores it under
//! `.handoff/reports/`; `list` / `get` read it back; `submit` / `approve` /
//! `reject` / `publish` drive the approval workflow (see
//! [`crate::report::ReportStatus::can_transition_to`]).

use std::collections::HashSet;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use super::doc_export::{self, PdfOptions, PdfSource, FORMAT_PDF};
use super::HandlerContext;
use crate::report::inspection::{build_inspection_data, InspectionInputs};
use crate::report::store;
use crate::report::verification::{build_verification_data, VerificationInputs};
use crate::report::{completion, defect, effort, milestone, monthly, weekly};
use crate::report::{ReportEngine, ReportMeta, ReportScope, ReportStatus, ReportType};
use crate::storage::config::read_config;
use crate::storage::docs::pdf::template::CoverMeta;
use crate::storage::runs;
use crate::storage::tasks::{collect_all_tasks, find_task_dir_by_id, task_status_only};
use crate::storage::test_runs::find_test_run;
use crate::storage::time_log::read_time_log;

pub fn handle_report(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let action = opt_str(arguments, "action")?.ok_or_else(|| {
        anyhow!("'action' parameter is required (generate|list|get|submit|approve|reject|publish)")
    })?;
    let pdf = pdf_options(ctx, arguments, action)?;
    let out = match action {
        "generate" => generate(ctx, arguments, pdf.as_ref())?,
        "list" => list(ctx, arguments)?,
        "get" => get(ctx, arguments, pdf.as_ref())?,
        "submit" => submit(ctx, arguments)?,
        "approve" => approve(ctx, arguments)?,
        "reject" => reject(ctx, arguments)?,
        "publish" => publish(ctx, arguments)?,
        other => bail!(
            "Unknown action '{other}' for 'action' (expected generate|list|get|submit|approve|reject|publish)"
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

/// Template used for report PDFs unless the caller names another one.
const REPORT_PDF_TEMPLATE: &str = "report";

/// The report-specific part of the `format` argument (REQ-PDF-007):
/// `Some(options)` for `format: "pdf"`, `None` for the Markdown default. The
/// PDF options without `format: "pdf"`, and `format: "pdf"` on an action that
/// renders nothing, are errors rather than silently ignored.
fn pdf_options(
    ctx: &HandlerContext,
    arguments: &Value,
    action: &str,
) -> Result<Option<PdfOptions>> {
    let pdf = match opt_str(arguments, "format")? {
        None | Some("markdown") => false,
        Some(FORMAT_PDF) => true,
        Some(other) => bail!("Unknown format '{other}' for 'format' (expected markdown|pdf)"),
    };
    if !pdf {
        if let Some(key) = ["output_dir", "template", "include_toc"]
            .into_iter()
            .chain(doc_export::COVER_ARGUMENTS)
            .find(|k| arguments.get(*k).is_some_and(|v| !v.is_null()))
        {
            bail!("'{key}' requires format=\"pdf\"");
        }
        return Ok(None);
    }
    if !matches!(action, "generate" | "get") {
        bail!("'format' is only supported by action=generate|get (got action={action})");
    }
    PdfOptions::from_arguments(ctx, arguments).map(Some)
}

/// Cover-page metadata for a report: its type (and scope label) as the
/// title, generation date as the date.
fn report_cover(meta: &ReportMeta) -> CoverMeta {
    let type_name = meta.report_type.as_str();
    let mut title = type_name.to_string();
    if let Some(first) = title.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    CoverMeta {
        title: format!("{title} report"),
        subtitle: meta.scope.label.clone(),
        date: doc_export::cover_date(&meta.generated_at),
        ..CoverMeta::default()
    }
}

/// Renders the stored Markdown of `meta` as PDF and returns the `pdf` object
/// added to the tool response.
fn render_report_pdf(
    ctx: &HandlerContext,
    meta: &ReportMeta,
    options: &PdfOptions,
) -> Result<Value> {
    let body = store::read_body(&ctx.handoff_dir, &meta.report_id)?;
    let pdf = doc_export::render_pdf(
        ctx,
        PdfSource {
            slug: &meta.report_id,
            markdown: &body,
            cover: report_cover(meta),
            default_template: Some(REPORT_PDF_TEMPLATE),
        },
        options,
    )
    .with_context(|| {
        format!(
            "report {} exists, but rendering it as PDF failed",
            meta.report_id
        )
    })?;
    Ok(json!({
        "pdf_path": pdf.pdf_path.display().to_string(),
        "typ_path": pdf.typ_path.display().to_string(),
        "warnings": pdf.warnings,
    }))
}

fn generate(ctx: &HandlerContext, arguments: &Value, pdf: Option<&PdfOptions>) -> Result<Value> {
    let report_type = ReportType::parse(required_str(arguments, "report_type")?)?;
    if pdf.is_some() {
        // Before the report is created: a missing Typst must not leave a
        // report behind that the caller believes failed.
        doc_export::require_typst()?;
    }
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
        ReportType::Monthly => monthly_data(ctx, scope, data)?,
        ReportType::Milestone => milestone_data(ctx, scope, data)?,
        ReportType::Defect => defect_data(ctx, scope, data)?,
        ReportType::Completion => completion_data(ctx, scope, data)?,
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
    let mut out = json!({ "report": meta, "custom_templates": custom_templates });
    if let Some(options) = pdf {
        out["pdf"] = render_report_pdf(ctx, &meta, options)?;
    }
    Ok(out)
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

fn get(ctx: &HandlerContext, arguments: &Value, pdf: Option<&PdfOptions>) -> Result<Value> {
    let report_id = required_str(arguments, "report_id")?;
    let meta = store::read_meta(&ctx.handoff_dir, report_id)?;
    let body = store::read_body(&ctx.handoff_dir, report_id)?;
    let mut out = json!({ "report": meta, "body": body });
    if let Some(options) = pdf {
        doc_export::require_typst()?;
        out["pdf"] = render_report_pdf(ctx, &meta, options)?;
    }
    Ok(out)
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

fn publish(ctx: &HandlerContext, arguments: &Value) -> Result<Value> {
    let report_id = required_str(arguments, "report_id")?;
    let meta = store::transition(
        &ctx.handoff_dir,
        report_id,
        ReportStatus::Published,
        ctx.agent_id.as_deref(),
        opt_str(arguments, "comment")?,
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
    weekly::validate_scope(&scope)?;
    let period = weekly::resolve_period(&scope, chrono::Utc::now().date_naive())?;
    scope.from = Some(period.start.to_string());
    scope.to = Some(period.end.to_string());

    let mut data = weekly::collect_weekly_data(&ctx.handoff_dir, period)?;
    data["verification"] = verification_progress(ctx);

    if let (Some(base), Value::Object(extra)) = (data.as_object_mut(), overrides) {
        base.extend(extra);
    }
    Ok((scope, data))
}

/// The verification-progress block of the periodic and milestone reports. A
/// project whose trace graph cannot be built (broken layer document,
/// unreadable config) still gets its report; the reason is rendered in the
/// verification section instead of failing the whole call.
fn verification_progress(ctx: &HandlerContext) -> Value {
    match super::trace::handle_trace_report(ctx, &json!({ "limit": 0 }))
        .and_then(|out| serde_json::from_str::<Value>(&out).map_err(Into::into))
    {
        Ok(trace_report) => weekly::verification_block(&trace_report),
        Err(e) => weekly::verification_unavailable(&format!("{e:#}")),
    }
}

/// Merges the keys the caller passed in `data` over the collected ones.
fn overlay(data: &mut Value, overrides: Value) {
    if let (Some(base), Value::Object(extra)) = (data.as_object_mut(), overrides) {
        base.extend(extra);
    }
}

fn all_tasks(ctx: &HandlerContext) -> Result<Vec<(crate::storage::tasks::TaskData, String)>> {
    let mut tasks = Vec::new();
    collect_all_tasks(&ctx.handoff_dir.join("tasks"), &mut tasks)?;
    Ok(tasks)
}

/// Collects the data a monthly report renders (FR-531, R4): the weekly
/// roll-up of the scope's period (a calendar month `2026-10`, an ISO week, a
/// date range, or `from` + `to`; default: the current month) plus the trend
/// of the metrics snapshots in it (see [`monthly`]). Returns the scope with
/// the resolved `from`/`to` filled in. Keys the caller passed in `data`
/// overlay the collected ones.
fn monthly_data(
    ctx: &HandlerContext,
    mut scope: ReportScope,
    overrides: Value,
) -> Result<(ReportScope, Value)> {
    monthly::validate_scope(&scope)?;
    let period = monthly::resolve_period(&scope, chrono::Utc::now().date_naive())?;
    scope.from = Some(period.start.to_string());
    scope.to = Some(period.end.to_string());

    let mut data = monthly::collect_monthly_data(&ctx.handoff_dir, period)?;
    data["verification"] = verification_progress(ctx);
    overlay(&mut data, overrides);
    Ok((scope, data))
}

/// Collects the data a milestone report renders (FR-531, R6) for
/// `scope.milestone`: planned vs actual dates, effort, bugs, and the
/// project-wide verification state (see [`milestone`]). Keys the caller
/// passed in `data` overlay the collected ones.
fn milestone_data(
    ctx: &HandlerContext,
    scope: ReportScope,
    overrides: Value,
) -> Result<(ReportScope, Value)> {
    let name = milestone::validate_scope(&scope)?;
    let milestones = read_config(&ctx.handoff_dir.join("config.toml"))?.milestones;
    let tasks = all_tasks(ctx)?;
    let requirements = milestone_requirements(ctx, name).map_err(|e| format!("{e:#}"));
    let mut data = milestone::build_milestone_data(&milestone::MilestoneInputs {
        name,
        config: milestones.get(name),
        tasks: &tasks,
        requirements: requirements.as_deref().map_err(String::as_str),
        today: chrono::Utc::now().date_naive(),
    })?;
    data["verification"] = verification_progress(ctx);
    overlay(&mut data, overrides);
    Ok((scope, data))
}

/// The requirements whose effective milestone (item, else document) is
/// `name`, via the same `doc_req_list` listing the requirement tools use.
fn milestone_requirements(
    ctx: &HandlerContext,
    name: &str,
) -> Result<Vec<milestone::MilestoneRequirement>> {
    let list = super::docs_query::handle_doc_req_list(
        ctx,
        &json!({ "milestone_filter": name, "limit": u32::MAX }),
    )?;
    milestone::parse_requirements(&serde_json::from_str(&list)?)
}

/// Collects the data a defect report renders (FR-531, R7): the bug tasks
/// (`bug` label) and the failing trace items, narrowed by the scope's
/// period (bug creation date), assignee, layers and items (see [`defect`]).
/// A trace graph that cannot be built is rendered in the Failing Items
/// section rather than failing the call, unless layers/items need it. Returns
/// the scope with a resolved period's `from`/`to` filled in. Keys the caller
/// passed in `data` overlay the collected ones.
fn defect_data(
    ctx: &HandlerContext,
    mut scope: ReportScope,
    overrides: Value,
) -> Result<(ReportScope, Value)> {
    defect::validate_scope(&scope)?;
    let period = effort::resolve_period(&scope)?;
    if let Some(period) = period {
        scope.from = Some(period.start.to_string());
        scope.to = Some(period.end.to_string());
    }
    let tasks = all_tasks(ctx)?;
    let trace = trace_items_report(ctx).map_err(|e| format!("{e:#}"));
    let mut data = defect::build_defect_data(&defect::DefectInputs {
        tasks: &tasks,
        trace: trace.as_ref().map_err(String::as_str),
        period,
        assignee: scope.assignee.as_deref(),
        layers: &scope.layers,
        items: &scope.items,
    })?;
    overlay(&mut data, overrides);
    Ok((scope, data))
}

/// Collects the data a completion report renders (FR-531, R8): everything
/// the other reports cover, for the whole project. Refused (nothing written)
/// unless every layer is approved, and when the trace graph cannot be built —
/// a completion report must not state a status it could not check (see
/// [`completion`]). Keys the caller passed in `data` overlay the collected
/// ones.
fn completion_data(
    ctx: &HandlerContext,
    scope: ReportScope,
    overrides: Value,
) -> Result<(ReportScope, Value)> {
    completion::validate_scope(&scope)?;
    let handoff = &ctx.handoff_dir;
    let trace_report = trace_items_report(ctx)?;
    completion::require_complete(&trace_report)?;

    let config = read_config(&handoff.join("config.toml"))?;
    let tasks = all_tasks(ctx)?;
    let time_log = read_time_log(handoff)?;
    let reports = store::list_reports(handoff)?;
    let mut data = completion::build_completion_data(&completion::CompletionInputs {
        project_name: &config.project.name,
        tasks: &tasks,
        time_log: &time_log,
        milestones: &config.milestones,
        trace_report: &trace_report,
        reports: &reports.reports,
    })?;
    overlay(&mut data, overrides);
    Ok((scope, data))
}
