//! `handoff_report` (FR-513 / SPEC-513): report engine foundation.
//!
//! One tool, six actions: `generate` renders a report from a built-in (or
//! `.handoff/templates/` override) Handlebars template and stores it under
//! `.handoff/reports/`; `list` / `get` read it back; `submit` / `approve` /
//! `reject` drive the approval workflow (see
//! [`crate::report::ReportStatus::can_transition_to`]).

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use super::HandlerContext;
use crate::report::store;
use crate::report::{ReportEngine, ReportScope, ReportStatus, ReportType};

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
