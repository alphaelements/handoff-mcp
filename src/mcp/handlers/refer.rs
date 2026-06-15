use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::Value;

use super::resolve_project_dir;
use crate::storage::config::read_config;
use crate::storage::expand_tilde;
use crate::storage::referrals::{is_valid_referral_type, write_referral, ReferralData};
use crate::storage::tasks::validate_priority;

pub fn handle(arguments: &Value) -> Result<String> {
    let source_project_dir = resolve_project_dir(arguments)?;

    let source_handoff = source_project_dir.join(".handoff");
    if !source_handoff.join("config.toml").exists() {
        anyhow::bail!(
            "Source project is not initialized: {}",
            source_project_dir.display()
        );
    }

    let source_config = read_config(&source_handoff.join("config.toml"))?;

    let target_dir = resolve_target(arguments, &source_config.dashboard.scan_dirs)?;

    let target_handoff = target_dir.join(".handoff");
    if !target_handoff.exists() {
        anyhow::bail!(
            "Target project is not initialized (no .handoff/): {}",
            target_dir.display()
        );
    }

    let summary = arguments
        .get("summary")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'summary' is required"))?;

    let referral_type = arguments
        .get("referral_type")
        .and_then(|v| v.as_str())
        .unwrap_or("request");

    if !is_valid_referral_type(referral_type) {
        anyhow::bail!(
            "Invalid referral_type: '{referral_type}'. Must be one of: improvement, bug, request, info"
        );
    }

    let priority = arguments.get("priority").and_then(|v| v.as_str());
    validate_priority(priority)?;

    let details = arguments
        .get("details")
        .and_then(|v| v.as_str())
        .map(String::from);

    let tasks: Vec<Value> = arguments
        .get("tasks")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let context = arguments.get("context").cloned();

    let now_dt = Utc::now();
    let now = now_dt.to_rfc3339();
    let id = format!("ref-{}", now_dt.format("%Y%m%d-%H%M%S-%f"));

    let data = ReferralData {
        id: id.clone(),
        source_project: source_config.project.name.clone(),
        source_project_dir: source_project_dir.to_string_lossy().to_string(),
        created_at: now,
        referral_type: referral_type.to_string(),
        summary: summary.to_string(),
        details,
        priority: priority.map(String::from),
        tasks,
        context,
    };

    let referrals_dir = target_handoff.join("referrals");
    write_referral(&referrals_dir, &data)?;

    let target_name = if target_handoff.join("config.toml").exists() {
        read_config(&target_handoff.join("config.toml"))
            .map(|c| c.project.name)
            .unwrap_or_else(|_| target_dir.to_string_lossy().to_string())
    } else {
        target_dir.to_string_lossy().to_string()
    };

    let mut msg = format!(
        "Referral sent: {id}\n  From: {}\n  To: {target_name}\n  Type: {referral_type}\n  Summary: {summary}",
        source_config.project.name
    );

    for w in collect_refer_warnings(&data) {
        msg.push_str(&format!("\n{w}"));
    }

    Ok(msg)
}

fn collect_refer_warnings(data: &ReferralData) -> Vec<String> {
    let mut warnings = Vec::new();

    if data.details.is_none() {
        warnings.push(
            "Warning: No details. The target project won't know what specifically to do. \
             Add a 'details' field describing the change, its impact, and what needs updating."
                .to_string(),
        );
    }

    if data.tasks.is_empty() {
        warnings.push(
            "Warning: No tasks. Consider adding suggested tasks with done_criteria \
             so the target project has actionable items to work from."
                .to_string(),
        );
    }

    if data.context.is_none() {
        warnings.push(
            "Warning: No context. Add a 'context' field with references to the source \
             (branch, commit, spec docs, MR links) so the target can trace back to the origin."
                .to_string(),
        );
    }

    if data.priority.is_none() {
        warnings.push(
            "Warning: No priority. Set 'priority' (low/medium/high) so the target project \
             can triage this referral appropriately."
                .to_string(),
        );
    }

    for (i, task) in data.tasks.iter().enumerate() {
        let has_criteria = task
            .get("done_criteria")
            .and_then(|v| v.as_array())
            .is_some_and(|a| !a.is_empty());
        if !has_criteria {
            let title = task
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("(untitled)");
            warnings.push(format!(
                "Warning: Task #{} '{}' has no done_criteria. \
                 Add criteria so the target knows when the task is complete.",
                i + 1,
                title
            ));
        }
    }

    warnings
}

fn resolve_target(arguments: &Value, scan_dirs: &[String]) -> Result<PathBuf> {
    if let Some(dir) = arguments.get("target_project_dir").and_then(|v| v.as_str()) {
        let path = PathBuf::from(dir);
        return std::fs::canonicalize(&path)
            .with_context(|| format!("Invalid target project path: {}", path.display()));
    }

    if let Some(name) = arguments.get("target_project").and_then(|v| v.as_str()) {
        return resolve_by_name(name, scan_dirs);
    }

    anyhow::bail!("Either 'target_project' or 'target_project_dir' is required")
}

fn resolve_by_name(name: &str, scan_dirs: &[String]) -> Result<PathBuf> {
    for scan_dir in scan_dirs {
        let expanded = expand_tilde(scan_dir);
        let expanded_path = Path::new(&expanded);

        if !expanded_path.exists() {
            continue;
        }

        let entries = match std::fs::read_dir(expanded_path) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                continue;
            }

            let config_path = entry.path().join(".handoff/config.toml");
            if !config_path.exists() {
                continue;
            }

            if let Ok(config) = read_config(&config_path) {
                if config.project.name == name {
                    return Ok(entry.path());
                }
            }
        }
    }

    anyhow::bail!(
        "Target project '{name}' not found in scan_dirs. \
         Use 'target_project_dir' with an absolute path instead."
    )
}
