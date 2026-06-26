use anyhow::{Context, Result};
use serde_json::Value;

use super::resolve_project_dir;
use crate::storage::ensure_handoff_exists;
use crate::storage::referrals::{
    change_referral_status, is_valid_referral_status, read_referral_by_id, read_referral_summaries,
};

pub fn handle_list(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;
    let referrals_dir = handoff.join("referrals");

    let status_filter = arguments.get("status_filter").and_then(|v| v.as_str());

    if let Some(filter) = status_filter {
        if !is_valid_referral_status(filter) {
            anyhow::bail!(
                "Invalid status_filter: '{filter}'. Must be one of: open, acknowledged, resolved"
            );
        }
    }

    let summaries = read_referral_summaries(&referrals_dir, status_filter)?;

    let result = serde_json::json!({
        "referrals": summaries,
        "total": summaries.len(),
    });

    serde_json::to_string_pretty(&result).context("Failed to serialize referrals")
}

pub fn handle_get(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;
    let referrals_dir = handoff.join("referrals");

    let referral_id = arguments
        .get("referral_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'referral_id' is required"))?;

    let (data, status) = read_referral_by_id(&referrals_dir, referral_id)?
        .ok_or_else(|| anyhow::anyhow!("Referral not found: {referral_id}"))?;

    let mut result = serde_json::to_value(&data).context("Failed to serialize referral")?;
    result["status"] = Value::String(status);

    serde_json::to_string_pretty(&result).context("Failed to serialize referral")
}

pub fn handle_update(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;
    let referrals_dir = handoff.join("referrals");

    let referral_id = arguments
        .get("referral_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'referral_id' is required"))?;

    let status = arguments
        .get("status")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'status' is required"))?;

    change_referral_status(&referrals_dir, referral_id, status)?;

    Ok(format!(
        "Updated referral {referral_id}: status -> {status}"
    ))
}
