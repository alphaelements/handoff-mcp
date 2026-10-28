//! Project-level calendar, label, and project-start tools.
//! - handoff_update_calendar: patch the `[calendar]` section.
//! - handoff_update_labels:   set the top-level `labels` array.
//! - handoff_start_project:   set `started_at` and (optionally) shift all task
//!   dates so the earliest start aligns to the project start date.

use std::path::Path;

use anyhow::Result;
use chrono::{Duration, NaiveDate, Utc};
use serde_json::Value;

use super::config_crud::{
    config_path, ensure_table, load_doc, save_doc, set_f64_map, set_mixed_array, set_opt_f64,
    set_opt_i64, set_opt_str, set_string_array, table_int, validate_work_hours_table,
};
use super::HandlerContext;

/// Accepted `schedule_mode` values.
const SCHEDULE_MODES: [&str; 2] = ["manual", "auto"];

/// Reject a `schedule_mode` argument that is neither absent, null (clear), nor
/// one of [`SCHEDULE_MODES`]. Write-time only: values already stored in
/// config.toml are never re-validated, so legacy files keep parsing.
pub(super) fn validate_schedule_mode(arg: Option<&Value>) -> Result<()> {
    match arg {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(s)) if SCHEDULE_MODES.contains(&s.as_str()) => Ok(()),
        Some(other) => anyhow::bail!(
            "Invalid schedule_mode {other}. Valid values: {}.",
            SCHEDULE_MODES.join(", ")
        ),
    }
}

/// Integer calendar fields that [`validate_calendar_hours`] checks.
const CALENDAR_INT_ARGS: [&str; 3] = ["work_start_hour", "work_end_hour", "max_parallel_tasks"];

/// Reject a `work_start_hour` / `work_end_hour` / `max_parallel_tasks` request
/// argument that is present but not an integer. The setters silently skip such
/// values, which would report success for a write that never happened.
fn validate_calendar_int_args(arguments: &Value) -> Result<()> {
    for field in CALENDAR_INT_ARGS {
        match arguments.get(field) {
            None | Some(Value::Null) => {}
            Some(v) if v.as_i64().is_some() => {}
            Some(other) => anyhow::bail!("Invalid {field} {other}: must be an integer."),
        }
    }
    Ok(())
}

/// Numeric percentage fields that [`validate_calendar_percent_args`] checks.
const CALENDAR_PERCENT_ARGS: [&str; 2] = ["max_utilization", "overwork_limit_percent"];

/// Reject a `max_utilization` / `overwork_limit_percent` request argument that
/// is present but not a number. `set_opt_f64` silently skips such values, which
/// would report success for a write that never happened.
fn validate_calendar_percent_args(arguments: &Value) -> Result<()> {
    for field in CALENDAR_PERCENT_ARGS {
        match arguments.get(field) {
            None | Some(Value::Null) => {}
            Some(v) if v.as_f64().is_some() => {}
            Some(other) => anyhow::bail!("Invalid {field} {other}: must be a number."),
        }
    }
    Ok(())
}

/// Validate the stored `[calendar]` percentages with the same bounds the read
/// path (`apply_percentages`) enforces by ignoring out-of-range values:
/// `max_utilization` > 0 and `overwork_limit_percent` >= 100. Rejecting on
/// write keeps a bad value from being silently replaced by the default later.
fn validate_calendar_percentages(cal: &dyn toml_edit::TableLike) -> Result<()> {
    let read = |field: &str| -> Result<Option<f64>> {
        match cal.get(field) {
            None => Ok(None),
            Some(item) => item
                .as_float()
                .or_else(|| item.as_integer().map(|i| i as f64))
                .map(Some)
                .ok_or_else(|| anyhow::anyhow!("Invalid {field}: must be a number.")),
        }
    };
    if let Some(p) = read("max_utilization")?.filter(|p| p.is_nan() || *p <= 0.0) {
        anyhow::bail!("Invalid max_utilization {p}: must be greater than 0.");
    }
    if let Some(p) = read("overwork_limit_percent")?.filter(|p| p.is_nan() || *p < 100.0) {
        anyhow::bail!("Invalid overwork_limit_percent {p}: must be at least 100.");
    }
    Ok(())
}

/// Validate the resulting `[calendar]` fields: the shared
/// `work_start_hour` / `work_end_hour` rules, `max_parallel_tasks` >= 1, and
/// the `max_utilization` / `overwork_limit_percent` bounds.
/// Called on the post-update table so a partial patch is checked against the
/// values already stored. Write-time only: config.toml read paths never
/// re-validate, so legacy files keep parsing.
pub(super) fn validate_calendar_hours(cal: &dyn toml_edit::TableLike) -> Result<()> {
    validate_work_hours_table(cal)?;
    if let Some(n) = table_int(cal, "max_parallel_tasks")?.filter(|n| *n < 1) {
        anyhow::bail!("max_parallel_tasks must be >= 1, got {n}.");
    }
    validate_calendar_percentages(cal)
}

/// handoff_update_calendar — patch the `[calendar]` section. Only provided
/// fields are changed; passing JSON null clears a field.
pub fn handle_update_calendar(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    // Validate before touching config.toml so a rejected request writes nothing.
    validate_schedule_mode(arguments.get("schedule_mode"))?;
    validate_calendar_int_args(arguments)?;
    validate_calendar_percent_args(arguments)?;

    let path = config_path(ctx)?;
    let mut doc = load_doc(&path)?;

    let cal = ensure_table(&mut doc, "calendar")?;
    cal.set_implicit(false);
    set_opt_f64(
        cal,
        "work_hours_per_day",
        arguments.get("work_hours_per_day"),
    );
    set_mixed_array(cal, "closed_weekdays", arguments.get("closed_weekdays"));
    set_string_array(cal, "closed_dates", arguments.get("closed_dates"));
    set_string_array(cal, "open_dates", arguments.get("open_dates"));
    set_f64_map(cal, "day_hours", arguments.get("day_hours"));
    set_opt_str(cal, "schedule_mode", arguments.get("schedule_mode"));
    set_opt_f64(
        cal,
        "overwork_limit_percent",
        arguments.get("overwork_limit_percent"),
    );
    set_opt_f64(cal, "max_utilization", arguments.get("max_utilization"));
    set_opt_i64(cal, "work_start_hour", arguments.get("work_start_hour"));
    set_opt_i64(cal, "work_end_hour", arguments.get("work_end_hour"));
    set_opt_i64(
        cal,
        "max_parallel_tasks",
        arguments.get("max_parallel_tasks"),
    );
    // `doc` is only in memory until save_doc, so a failure here writes nothing.
    validate_calendar_hours(cal)?;

    // schedule_mode is also accepted at the top level for parity with VSCode.
    match arguments.get("schedule_mode") {
        Some(Value::Null) => {
            doc.remove("schedule_mode");
        }
        Some(Value::String(mode)) => {
            doc["schedule_mode"] = toml_edit::Item::Value(toml_edit::Value::from(mode.as_str()));
        }
        _ => {}
    }

    save_doc(&path, &doc)?;
    Ok("Updated calendar configuration".to_string())
}

/// handoff_update_labels — replace the top-level `labels` array.
pub fn handle_update_labels(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let path = config_path(ctx)?;
    let labels = arguments
        .get("labels")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("'labels' (array of strings) is required"))?;

    let mut doc = load_doc(&path)?;
    let mut arr = toml_edit::Array::new();
    for it in labels {
        if let Some(s) = it.as_str() {
            arr.push(s);
        }
    }
    let count = arr.len();
    doc["labels"] = toml_edit::Item::Value(toml_edit::Value::Array(arr));

    save_doc(&path, &doc)?;
    Ok(format!("Set {count} project label(s)"))
}

/// handoff_start_project — record the project start timestamp and, when
/// `shift_dates` is true, move every task's start/due dates so the earliest
/// start lands on `start_date`. Mirrors the VSCode startProject command.
pub fn handle_start_project(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let path = config_path(ctx)?;

    // start_date defaults to today (UTC).
    let start_date_str = match arguments.get("start_date").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => Utc::now().format("%Y-%m-%d").to_string(),
    };
    let start_date = NaiveDate::parse_from_str(&start_date_str, "%Y-%m-%d").map_err(|_| {
        anyhow::anyhow!("Invalid start_date '{start_date_str}' (expected YYYY-MM-DD)")
    })?;

    let mut doc = load_doc(&path)?;
    doc["started_at"] = toml_edit::Item::Value(toml_edit::Value::from(start_date_str.as_str()));
    save_doc(&path, &doc)?;

    let shift = arguments
        .get("shift_dates")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !shift {
        return Ok(format!("Set project start to {start_date_str}"));
    }

    let tasks_dir = path
        .parent()
        .map(|p| p.join("tasks"))
        .ok_or_else(|| anyhow::anyhow!("Cannot locate tasks dir"))?;
    let shifted = shift_all_dates(&tasks_dir, start_date)?;

    Ok(format!(
        "Set project start to {start_date_str} and shifted dates on {shifted} task(s)"
    ))
}

/// Find the earliest task start date and shift all task start/due dates so that
/// earliest start aligns to `target`. Returns the number of tasks changed.
fn shift_all_dates(tasks_dir: &Path, target: NaiveDate) -> Result<usize> {
    use crate::storage::tasks::{read_task, write_task};

    let dirs = collect_task_dirs(tasks_dir)?;

    // Pass 1: find earliest start date.
    let mut earliest: Option<NaiveDate> = None;
    for dir in &dirs {
        if let Some((data, _)) = read_task(dir)? {
            if let Some(sd) = data.schedule.as_ref().and_then(|s| s.start_date.as_deref()) {
                if let Ok(d) = NaiveDate::parse_from_str(sd, "%Y-%m-%d") {
                    earliest = Some(earliest.map_or(d, |e| e.min(d)));
                }
            }
        }
    }
    let earliest = match earliest {
        Some(d) => d,
        None => return Ok(0), // no dated tasks → nothing to shift
    };
    let delta = (target - earliest).num_days();
    if delta == 0 {
        return Ok(0);
    }

    // Pass 2: apply the shift.
    let mut count = 0;
    for dir in &dirs {
        if let Some((mut data, status)) = read_task(dir)? {
            let mut changed = false;
            if let Some(sched) = data.schedule.as_mut() {
                if let Some(sd) = sched.start_date.as_deref() {
                    if let Ok(d) = NaiveDate::parse_from_str(sd, "%Y-%m-%d") {
                        sched.start_date = Some(shift(d, delta));
                        changed = true;
                    }
                }
                if let Some(dd) = sched.due_date.as_deref() {
                    if let Ok(d) = NaiveDate::parse_from_str(dd, "%Y-%m-%d") {
                        sched.due_date = Some(shift(d, delta));
                        changed = true;
                    }
                }
            }
            if changed {
                data.updated_at = Some(Utc::now().to_rfc3339());
                write_task(dir, &status, &data)?;
                count += 1;
            }
        }
    }
    Ok(count)
}

fn shift(date: NaiveDate, delta_days: i64) -> String {
    (date + Duration::days(delta_days))
        .format("%Y-%m-%d")
        .to_string()
}

fn collect_task_dirs(dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut out = Vec::new();
    collect_into(dir, &mut out)?;
    Ok(out)
}

fn collect_into(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            if crate::storage::tasks::find_task_file(&path)?.is_some() {
                out.push(path.clone());
            }
            collect_into(&path, out)?;
        }
    }
    Ok(())
}
