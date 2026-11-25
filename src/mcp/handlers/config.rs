use anyhow::{Context, Result};
use serde_json::Value;
use toml_edit::{DocumentMut, Item, TableLike, Value as TomlValue};

use super::config_crud::validate_work_hours_table;
use super::HandlerContext;
use crate::storage::config::{
    ensure_table_like, read_config, render_config, CommandsConfig, Config, OnboardingConfig,
    ReleaseConfig, DONE_GUARD_MODES, ONBOARDING_PRESETS, ORPHAN_GUARD_MODES,
};
use crate::storage::config_preset::plan_preset;
use crate::storage::docs::layer::LayerRegistry;
use crate::storage::docs::layer_parse::is_parseable_id_prefix;
use crate::trace::profile::BUILTIN_PROFILE_NAMES;

pub fn handle_get(ctx: &HandlerContext, _arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;
    let config_path = handoff.join("config.toml");

    let raw = std::fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read config: {}", config_path.display()))?;
    let doc: DocumentMut = raw.parse().with_context(|| "Failed to parse config.toml")?;

    let toml_str = doc.to_string();
    let toml_value: toml::Value =
        toml::from_str(&toml_str).with_context(|| "Failed to deserialize config")?;
    let json_value =
        serde_json::to_value(&toml_value).with_context(|| "Failed to convert to JSON")?;

    serde_json::to_string_pretty(&json_value).context("Failed to format config")
}

/// Is `key` present in `arguments` with a value (JSON `null` counts as absent)?
fn has_argument(arguments: &Value, key: &str) -> bool {
    arguments.get(key).is_some_and(|value| !value.is_null())
}

/// `handoff_update_config(preset: ..., confirm?: bool)` (SPEC-ONB-010): applies
/// a config preset to an existing project.
///
/// Keys that had no value are simply set. When the preset would replace a
/// value the project set itself, nothing is written unless `confirm` is true:
/// the reply lists every change with its old and new value, so the caller can
/// show them and ask. The reply is JSON
/// `{preset, applied, changes: [{key, old, new}], overwrites: [key], message}`.
fn handle_preset(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let name = arguments
        .get("preset")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "'preset' must be a string: one of {}",
                ONBOARDING_PRESETS.join("/")
            )
        })?;
    if has_argument(arguments, "updates") {
        anyhow::bail!(
            "'preset' cannot be combined with 'updates': apply the preset first, then \
             update individual keys in a second call"
        );
    }
    let confirm = match arguments.get("confirm") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(confirm)) => *confirm,
        Some(other) => anyhow::bail!("'confirm' must be a boolean, got {other}"),
    };

    let config_path = ctx.handoff_dir.join("config.toml");
    let raw = std::fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read config: {}", config_path.display()))?;
    let plan = plan_preset(&raw, name)?;
    let overwrites: Vec<&str> = plan
        .overwrites()
        .iter()
        .map(|change| change.key.as_str())
        .collect();

    let applied = overwrites.is_empty() || confirm;
    if applied && !plan.changes.is_empty() {
        crate::storage::atomic_write(&config_path, plan.rendered.as_bytes())
            .with_context(|| format!("Failed to write config: {}", config_path.display()))?;
    }
    let message = if !applied {
        format!(
            "Nothing was written: preset '{}' would replace {} value(s) this project already \
             set ({}). Show the changes to the user, then call again with confirm=true to \
             apply them.",
            plan.preset,
            overwrites.len(),
            overwrites.join(", ")
        )
    } else if plan.changes.is_empty() {
        format!(
            "Preset '{}' is already applied; nothing changed.",
            plan.preset
        )
    } else {
        format!(
            "Applied preset '{}' ({} key(s) set).",
            plan.preset,
            plan.changes.len()
        )
    };

    serde_json::to_string_pretty(&serde_json::json!({
        "preset": plan.preset,
        "applied": applied,
        "changes": plan.changes,
        "overwrites": overwrites,
        "message": message,
    }))
    .context("Failed to format preset reply")
}

pub fn handle_update(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    if has_argument(arguments, "preset") {
        return handle_preset(ctx, arguments);
    }
    if has_argument(arguments, "confirm") {
        anyhow::bail!("'confirm' is only used together with 'preset'");
    }

    let handoff = &ctx.handoff_dir;
    let config_path = handoff.join("config.toml");

    let updates = arguments
        .get("updates")
        .ok_or_else(|| anyhow::anyhow!("'updates' parameter is required"))?;

    let updates = updates
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("'updates' must be an object"))?;

    // Separate typed keys (settings/dashboard/project) from raw TOML keys
    let typed_keys = [
        "settings.history_limit",
        "settings.session_prune_limit",
        "settings.done_task_limit",
        "settings.compact_task_limit",
        "settings.auto_git_summary",
        "settings.require_estimate_hours",
        "settings.ai_estimate_multiplier",
        "settings.default_estimate_hours",
        "settings.schedule_stale_notice",
        "settings.layer_missing_notice",
        "settings.context_files",
        "settings.memory_enabled",
        "settings.memory_dup_threshold",
        "settings.memory_query_min_score",
        "settings.memory_query_relative_threshold",
        "settings.memory_query_limit",
        "settings.memory_stale_days",
        "settings.memory_injected_gc_days",
        "settings.timer_provider",
        "settings.timer_authority_ttl_secs",
        "settings.timer_idle_timeout_minutes",
        "dashboard.scan_dirs",
        "dashboard.exclude_patterns",
        "dashboard.max_depth",
        "project.name",
        "project.description",
    ];

    let mut has_typed = false;
    let mut has_raw = false;

    for key in updates.keys() {
        if typed_keys.contains(&key.as_str()) {
            has_typed = true;
        } else {
            has_raw = true;
        }
    }

    let mut applied = Vec::new();

    // All-or-nothing: every key is applied to in-memory state and validated
    // first; `config.toml` is written once, at the very end, so a rejected key
    // anywhere in the batch leaves the file untouched (t-vfix.21).
    let mut typed_rendered: Option<String> = None;

    // Handle typed keys via existing Config struct
    if has_typed {
        let mut config = read_config(&config_path)?;
        for (key, value) in updates {
            match key.as_str() {
                "settings.history_limit" => {
                    // Deprecated since v0.47.3 and inert: a number is still
                    // stored (DS-V0473-311) but never prunes, and the reply
                    // says so; null removes the key.
                    if value.is_null() {
                        config.settings.history_limit = None;
                        applied.push("settings.history_limit = null (removed)".to_string());
                    } else {
                        let n = value
                            .as_u64()
                            .and_then(|n| u32::try_from(n).ok())
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "settings.history_limit must be a non-negative integer or null"
                                )
                            })?;
                        config.settings.history_limit = Some(n);
                        applied.push(format!("settings.history_limit = {n}"));
                        applied.push(
                            "Warning: settings.history_limit is deprecated and no longer prunes \
                             sessions; use settings.session_prune_limit (pass null to remove \
                             history_limit)"
                                .to_string(),
                        );
                    }
                }
                "settings.session_prune_limit" => {
                    if value.is_null() {
                        config.settings.session_prune_limit = None;
                        applied.push("settings.session_prune_limit = null (removed)".to_string());
                    } else {
                        let n = value
                            .as_u64()
                            .and_then(|n| u32::try_from(n).ok())
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "settings.session_prune_limit must be a non-negative integer or null"
                                )
                            })?;
                        config.settings.session_prune_limit = Some(n);
                        applied.push(format!("settings.session_prune_limit = {n}"));
                    }
                }
                "settings.done_task_limit" => {
                    if let Some(n) = value.as_u64() {
                        config.settings.done_task_limit = n as u32;
                        applied.push(format!("settings.done_task_limit = {n}"));
                    }
                }
                "settings.compact_task_limit" => {
                    let n = value
                        .as_u64()
                        .and_then(|n| u32::try_from(n).ok())
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "settings.compact_task_limit must be a non-negative integer"
                            )
                        })?;
                    config.settings.compact_task_limit = n;
                    applied.push(format!("settings.compact_task_limit = {n}"));
                }
                "settings.auto_git_summary" => {
                    if let Some(b) = value.as_bool() {
                        config.settings.auto_git_summary = b;
                        applied.push(format!("settings.auto_git_summary = {b}"));
                    }
                }
                "settings.require_estimate_hours" => {
                    if let Some(b) = value.as_bool() {
                        config.settings.require_estimate_hours = b;
                        applied.push(format!("settings.require_estimate_hours = {b}"));
                    }
                }
                "settings.schedule_stale_notice" => {
                    let Some(b) = value.as_bool() else {
                        anyhow::bail!("settings.schedule_stale_notice must be a boolean");
                    };
                    config.settings.schedule_stale_notice = b;
                    applied.push(format!("settings.schedule_stale_notice = {b}"));
                }
                "settings.layer_missing_notice" => {
                    let Some(b) = value.as_bool() else {
                        anyhow::bail!("settings.layer_missing_notice must be a boolean");
                    };
                    config.settings.layer_missing_notice = b;
                    applied.push(format!("settings.layer_missing_notice = {b}"));
                }
                "settings.ai_estimate_multiplier" => {
                    if let Some(n) = value.as_f64() {
                        if n < 0.0 {
                            anyhow::bail!("settings.ai_estimate_multiplier must be >= 0");
                        }
                        config.settings.ai_estimate_multiplier = n;
                        applied.push(format!("settings.ai_estimate_multiplier = {n}"));
                    }
                }
                "settings.default_estimate_hours" => {
                    let Some(n) = value.as_f64() else {
                        anyhow::bail!("settings.default_estimate_hours must be a number");
                    };
                    if !n.is_finite() || n <= 0.0 {
                        anyhow::bail!("settings.default_estimate_hours must be > 0");
                    }
                    config.settings.default_estimate_hours = Some(n);
                    applied.push(format!("settings.default_estimate_hours = {n}"));
                }
                "settings.context_files" => {
                    if let Some(arr) = value.as_array() {
                        config.settings.context_files = arr
                            .iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect();
                        applied.push(format!(
                            "settings.context_files = {:?}",
                            config.settings.context_files
                        ));
                    }
                }
                "settings.memory_enabled" => {
                    let Some(b) = value.as_bool() else {
                        anyhow::bail!("settings.memory_enabled must be a boolean");
                    };
                    config.settings.memory_enabled = b;
                    applied.push(format!("settings.memory_enabled = {b}"));
                }
                "settings.memory_dup_threshold" => {
                    let Some(n) = value.as_f64() else {
                        anyhow::bail!("settings.memory_dup_threshold must be a number");
                    };
                    if !(0.0..=1.0).contains(&n) {
                        anyhow::bail!("settings.memory_dup_threshold must be between 0 and 1");
                    }
                    config.settings.memory_dup_threshold = n;
                    applied.push(format!("settings.memory_dup_threshold = {n}"));
                }
                "settings.memory_query_min_score" => {
                    let Some(n) = value.as_f64() else {
                        anyhow::bail!("settings.memory_query_min_score must be a number");
                    };
                    if n < 0.0 {
                        anyhow::bail!("settings.memory_query_min_score must be >= 0");
                    }
                    config.settings.memory_query_min_score = n;
                    applied.push(format!("settings.memory_query_min_score = {n}"));
                }
                "settings.memory_query_relative_threshold" => {
                    let Some(n) = value.as_f64() else {
                        anyhow::bail!("settings.memory_query_relative_threshold must be a number");
                    };
                    if !(0.0..=1.0).contains(&n) {
                        anyhow::bail!(
                            "settings.memory_query_relative_threshold must be between 0 and 1"
                        );
                    }
                    config.settings.memory_query_relative_threshold = n;
                    applied.push(format!("settings.memory_query_relative_threshold = {n}"));
                }
                "settings.memory_query_limit" => {
                    let Some(n) = value.as_u64() else {
                        anyhow::bail!("settings.memory_query_limit must be a non-negative integer");
                    };
                    if n == 0 {
                        anyhow::bail!("settings.memory_query_limit must be >= 1");
                    }
                    config.settings.memory_query_limit = n as u32;
                    applied.push(format!("settings.memory_query_limit = {n}"));
                }
                "settings.memory_stale_days" => {
                    let Some(n) = value.as_i64() else {
                        anyhow::bail!("settings.memory_stale_days must be an integer");
                    };
                    if n < 0 {
                        anyhow::bail!("settings.memory_stale_days must be >= 0");
                    }
                    config.settings.memory_stale_days = n;
                    applied.push(format!("settings.memory_stale_days = {n}"));
                }
                "settings.memory_injected_gc_days" => {
                    let Some(n) = value.as_i64() else {
                        anyhow::bail!("settings.memory_injected_gc_days must be an integer");
                    };
                    if n < 0 {
                        anyhow::bail!("settings.memory_injected_gc_days must be >= 0");
                    }
                    config.settings.memory_injected_gc_days = n;
                    applied.push(format!("settings.memory_injected_gc_days = {n}"));
                }
                "settings.timer_provider" => {
                    let Some(s) = value.as_str() else {
                        anyhow::bail!("settings.timer_provider must be a string");
                    };
                    let valid = ["auto", "vscode", "mcp", "off"];
                    if !valid.contains(&s) {
                        anyhow::bail!(
                            "settings.timer_provider must be one of: {}",
                            valid.join(", ")
                        );
                    }
                    config.settings.timer_provider = s.to_string();
                    applied.push(format!("settings.timer_provider = {s}"));
                }
                "settings.timer_authority_ttl_secs" => {
                    let Some(n) = value.as_u64() else {
                        anyhow::bail!(
                            "settings.timer_authority_ttl_secs must be a positive integer"
                        );
                    };
                    if n == 0 {
                        anyhow::bail!("settings.timer_authority_ttl_secs must be >= 1");
                    }
                    config.settings.timer_authority_ttl_secs = n;
                    applied.push(format!("settings.timer_authority_ttl_secs = {n}"));
                }
                "settings.timer_idle_timeout_minutes" => {
                    let Some(n) = value.as_u64() else {
                        anyhow::bail!(
                            "settings.timer_idle_timeout_minutes must be a non-negative integer"
                        );
                    };
                    config.settings.timer_idle_timeout_minutes = n;
                    applied.push(format!("settings.timer_idle_timeout_minutes = {n}"));
                }
                "dashboard.scan_dirs" => {
                    if let Some(arr) = value.as_array() {
                        config.dashboard.scan_dirs = arr
                            .iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect();
                        applied.push(format!(
                            "dashboard.scan_dirs = {:?}",
                            config.dashboard.scan_dirs
                        ));
                    }
                }
                "dashboard.exclude_patterns" => {
                    if let Some(arr) = value.as_array() {
                        config.dashboard.exclude_patterns = arr
                            .iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect();
                        applied.push(format!(
                            "dashboard.exclude_patterns = {:?}",
                            config.dashboard.exclude_patterns
                        ));
                    }
                }
                "dashboard.max_depth" => {
                    let Some(n) = value.as_u64() else {
                        anyhow::bail!("dashboard.max_depth must be a non-negative integer");
                    };
                    if n == 0 {
                        anyhow::bail!("dashboard.max_depth must be >= 1");
                    }
                    config.dashboard.max_depth = n as usize;
                    applied.push(format!("dashboard.max_depth = {n}"));
                }
                "project.name" => {
                    if let Some(s) = value.as_str() {
                        config.project.name = s.to_string();
                        applied.push(format!("project.name = {s}"));
                    }
                }
                "project.description" => {
                    if let Some(s) = value.as_str() {
                        config.project.description = Some(s.to_string());
                        applied.push(format!("project.description = {s}"));
                    }
                }
                _ => {}
            }
        }
        typed_rendered = Some(render_config(&config)?);
    }

    // Handle raw TOML keys (calendar, assignees, effort_budget, gantt_view, etc.)
    if has_raw {
        // Raw keys edit the text the typed keys would have produced, so both
        // halves land in the single write below.
        let raw = match &typed_rendered {
            Some(rendered) => rendered.clone(),
            None => std::fs::read_to_string(&config_path)
                .with_context(|| format!("Failed to read config: {}", config_path.display()))?,
        };
        let mut doc: DocumentMut = raw
            .parse()
            .with_context(|| "Failed to parse config.toml for raw update")?;

        for (key, value) in updates {
            if typed_keys.contains(&key.as_str()) {
                continue;
            }

            // Reject `milestones.<name>.status`: milestone status changes
            // must go through handoff_update_milestone, which enforces
            // lifecycle transitions and the release guard (t454).
            if is_milestone_status_key(key) {
                anyhow::bail!(
                    "Cannot set '{key}' via handoff_update_config. \
                     Use handoff_update_milestone to change milestone status \
                     (lifecycle transition and release_guard are enforced there)."
                );
            }

            // The generic raw-key path must enforce the same write-time
            // schedule_mode enum as handoff_update_calendar.
            if RAW_SCHEDULE_MODE_KEYS.contains(&key.as_str()) {
                super::calendar::validate_schedule_mode(Some(value))
                    .map_err(|e| anyhow::anyhow!("Invalid value for {key}: {e}"))?;
            }

            let parts: Vec<&str> = key.split('.').collect();
            if parts.is_empty() {
                applied.push(format!("{key}: invalid key"));
                continue;
            }

            match set_toml_value(&mut doc, &parts, value) {
                Ok(()) => applied.push(format!("{key} = {value}")),
                Err(e) => applied.push(format!("{key}: error ({e})")),
            }
        }

        let rendered = doc.to_string();
        if updates
            .keys()
            .any(|k| k.starts_with(REQUIREMENT_GATE_KEY_PREFIX))
        {
            validate_requirement_gate(&rendered)?;
        }
        validate_trace_scalar_keys(updates.keys().map(String::as_str), &rendered)?;
        if updates.keys().any(|k| is_session_loop_key(k)) {
            validate_session_loop(&rendered)?;
        }
        if updates.keys().any(|k| is_section_key(k, COMMANDS_SECTION)) {
            validate_commands(&rendered)?;
        }
        if updates
            .keys()
            .any(|k| is_section_key(k, ONBOARDING_SECTION))
        {
            validate_onboarding(&rendered)?;
        }
        if updates.keys().any(|k| is_section_key(k, RELEASE_SECTION)) {
            validate_release(&rendered)?;
        }
        if updates
            .keys()
            .any(|k| k == "calendar" || k.starts_with("calendar."))
        {
            if let Some(cal) = doc.get("calendar").and_then(Item::as_table_like) {
                super::calendar::validate_calendar_hours(cal)?;
            }
        }
        validate_raw_assignee_work_hours(&doc, updates.keys().map(String::as_str))?;
        applied.extend(unparseable_id_prefix_warnings(
            updates.keys().map(String::as_str),
            &rendered,
        ));
        typed_rendered = Some(rendered);
    }

    if let Some(rendered) = typed_rendered {
        crate::storage::atomic_write(&config_path, rendered.as_bytes())
            .with_context(|| format!("Failed to write config: {}", config_path.display()))?;
    }

    if applied.is_empty() {
        Ok("No updates applied".to_string())
    } else {
        Ok(format!("Updated config:\n{}", applied.join("\n")))
    }
}

/// Applies the dedicated assignee tools' `work_start_hour` / `work_end_hour`
/// rules to every `[assignees.<key>]` entry a raw update touched. Only touched
/// entries are checked, so a legacy out-of-range entry elsewhere in the file
/// does not block unrelated writes. A key of exactly `assignees` (whole-table
/// replacement) checks every entry.
fn validate_raw_assignee_work_hours<'a>(
    doc: &DocumentMut,
    keys: impl Iterator<Item = &'a str>,
) -> Result<()> {
    let Some(assignees) = doc.get("assignees").and_then(Item::as_table_like) else {
        return Ok(());
    };
    for key in keys {
        let mut parts = key.split('.');
        if parts.next() != Some("assignees") {
            continue;
        }
        let touched: Vec<&str> = match parts.next() {
            Some(name) => vec![name],
            None => assignees.iter().map(|(name, _)| name).collect(),
        };
        for name in touched {
            if let Some(entry) = assignees.get(name).and_then(Item::as_table_like) {
                validate_work_hours_table(entry)
                    .map_err(|e| anyhow::anyhow!("Invalid assignees.{name}: {e}"))?;
            }
        }
    }
    Ok(())
}

/// Raw dotted keys that write `schedule_mode` (calendar table and top level).
const RAW_SCHEDULE_MODE_KEYS: [&str; 2] = ["calendar.schedule_mode", "schedule_mode"];

/// Dotted-key prefix of the `[trace.requirement_gate]` section (SPEC-RQ-010).
const REQUIREMENT_GATE_KEY_PREFIX: &str = "trace.requirement_gate";

/// Rejects (before anything is written) a config text whose
/// `[trace.requirement_gate]` is malformed or holds an invalid value.
fn validate_requirement_gate(rendered: &str) -> Result<()> {
    let config: Config = toml::from_str(rendered)
        .map_err(|e| anyhow::anyhow!("invalid [trace.requirement_gate]: {e}"))?;
    if let Some(gate) = &config.trace.requirement_gate {
        gate.validate().map_err(|e| anyhow::anyhow!(e))?;
    }
    Ok(())
}

/// Top-level `[session_loop]` section (SPEC-ELITE-031) — not the multi-WT
/// `[worktree.session_loop]`.
const SESSION_LOOP_SECTION: &str = "session_loop";

/// Does this update key write the `[session_loop]` section, as a whole
/// (`session_loop`) or one of its keys (`session_loop.profile`)?
fn is_session_loop_key(key: &str) -> bool {
    is_section_key(key, SESSION_LOOP_SECTION)
}

/// Does this update key write the top-level `section`, as a whole (`section`)
/// or one of its keys (`section.key`)?
fn is_section_key(key: &str, section: &str) -> bool {
    key.strip_prefix(section)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}

/// Top-level `[commands]` section (SPEC-ONB-001).
const COMMANDS_SECTION: &str = "commands";

/// Rejects (before anything is written) a config text whose `[commands]` is
/// not a table of strings: a known key holding anything else would make the
/// whole `config.toml` unreadable for every other tool. Only the section
/// itself is looked at, and only when the update writes it, so a bad value
/// elsewhere never blocks setting a command.
fn validate_commands(rendered: &str) -> Result<()> {
    let invalid = |e: &dyn std::fmt::Display| anyhow::anyhow!("invalid [{COMMANDS_SECTION}]: {e}");
    let root: toml::Value = toml::from_str(rendered).map_err(|e| invalid(&e))?;
    match root.get(COMMANDS_SECTION) {
        None => Ok(()),
        Some(section) => section
            .clone()
            .try_into::<CommandsConfig>()
            .map(|_| ())
            .map_err(|e| invalid(&format!("every command must be a string ({e})"))),
    }
}

/// Top-level `[onboarding]` section (SPEC-ONB-091).
const ONBOARDING_SECTION: &str = "onboarding";

/// Rejects (before anything is written) a config text whose `[onboarding]`
/// holds a value of the wrong type or a mode the readers would not honor.
/// Like [`validate_commands`], it looks at that section alone and is only
/// called when the update writes it.
fn validate_onboarding(rendered: &str) -> Result<()> {
    let invalid =
        |e: &dyn std::fmt::Display| anyhow::anyhow!("invalid [{ONBOARDING_SECTION}]: {e}");
    let root: toml::Value = toml::from_str(rendered).map_err(|e| invalid(&e))?;
    let Some(section) = root.get(ONBOARDING_SECTION) else {
        return Ok(());
    };
    let onboarding = section
        .clone()
        .try_into::<OnboardingConfig>()
        .map_err(|e| invalid(&e))?;
    onboarding.validate().map_err(|e| anyhow::anyhow!(e))
}

/// Top-level `[release]` section (SPEC-ONB-040).
const RELEASE_SECTION: &str = "release";

/// Rejects (before anything is written) a config text whose `[release]`
/// holds a value of the wrong type or a list of manual checks the readers
/// would silently reduce. Like [`validate_commands`], it looks at that
/// section alone and is only called when the update writes it.
fn validate_release(rendered: &str) -> Result<()> {
    let invalid = |e: &dyn std::fmt::Display| anyhow::anyhow!("invalid [{RELEASE_SECTION}]: {e}");
    let root: toml::Value = toml::from_str(rendered).map_err(|e| invalid(&e))?;
    let Some(section) = root.get(RELEASE_SECTION) else {
        return Ok(());
    };
    let release = section
        .clone()
        .try_into::<ReleaseConfig>()
        .map_err(|e| invalid(&e))?;
    release.validate().map_err(|e| anyhow::anyhow!(e))
}

/// Rejects (before anything is written) a config text whose `[session_loop]`
/// is malformed or names a pipeline profile the session-execute workflow
/// would refuse. Only called when the update writes that section, so a
/// hand-edited bad value never blocks an unrelated update.
fn validate_session_loop(rendered: &str) -> Result<()> {
    let config: Config = toml::from_str(rendered)
        .map_err(|e| anyhow::anyhow!("invalid [{SESSION_LOOP_SECTION}]: {e}"))?;
    config
        .session_loop
        .validate()
        .map_err(|e| anyhow::anyhow!(e))
}

/// Dotted key of `[trace] orphan_guard` (SPEC-TRC-003).
const ORPHAN_GUARD_KEY: &str = "trace.orphan_guard";

/// Rejects a config text whose `[trace] orphan_guard` is not one of
/// [`ORPHAN_GUARD_MODES`]. Looks at the raw value, so a value of the wrong
/// type (a boolean, a number, a table) gets the same message naming the
/// accepted modes as a misspelled one. An absent key is the default.
fn validate_orphan_guard(rendered: &str) -> Result<()> {
    let expected = ORPHAN_GUARD_MODES.join("/");
    let root: toml::Value = toml::from_str(rendered).map_err(|e| {
        anyhow::anyhow!("invalid {ORPHAN_GUARD_KEY}: {e} (expected one of {expected})")
    })?;
    let Some(value) = root.get("trace").and_then(|t| t.get("orphan_guard")) else {
        return Ok(());
    };
    match value.as_str() {
        Some(mode) if ORPHAN_GUARD_MODES.contains(&mode) => Ok(()),
        Some(mode) => {
            anyhow::bail!("invalid {ORPHAN_GUARD_KEY} '{mode}': expected one of {expected}")
        }
        None => anyhow::bail!(
            "invalid {ORPHAN_GUARD_KEY} {value}: expected one of {expected} (a string)"
        ),
    }
}

/// Rejects (before anything is written) an update that sets `trace.profile`,
/// `trace.layers`, `trace.done_guard` or `trace.orphan_guard` to a value the
/// readers would not honor: `done_guard` must be one of [`DONE_GUARD_MODES`]
/// and `orphan_guard` one of [`ORPHAN_GUARD_MODES`]; `profile` must be
/// empty ("auto"), a built-in profile or a `[trace.profiles.<name>]` entry in
/// the resulting config; every `layers` entry must be a built-in layer or a
/// valid `[[trace.layer]]` entry. Only the keys being written are checked, so
/// an unrelated update is never blocked by a pre-existing bad value.
fn validate_trace_scalar_keys<'a>(
    keys: impl Iterator<Item = &'a str>,
    rendered: &str,
) -> Result<()> {
    let (mut profile, mut layers, mut done_guard, mut orphan_guard) = (false, false, false, false);
    for key in keys {
        match key {
            "trace.profile" => profile = true,
            "trace.layers" => layers = true,
            k if k == "trace.done_guard" || k.starts_with("trace.done_guard.") => done_guard = true,
            // A deeper key (`trace.orphan_guard.x`) turns the value into a
            // table, which is not a mode either.
            k if k == ORPHAN_GUARD_KEY || k.starts_with("trace.orphan_guard.") => {
                orphan_guard = true
            }
            _ => {}
        }
    }
    if orphan_guard {
        validate_orphan_guard(rendered)?;
    }
    if !(profile || layers || done_guard) {
        return Ok(());
    }
    let touched: Vec<&str> = [
        (profile, "trace.profile"),
        (layers, "trace.layers"),
        (done_guard, "trace.done_guard"),
    ]
    .into_iter()
    .filter_map(|(set, name)| set.then_some(name))
    .collect();
    let config: Config = toml::from_str(rendered)
        .map_err(|e| anyhow::anyhow!("invalid {}: {e}", touched.join(", ")))?;
    let trace = &config.trace;

    if done_guard && !DONE_GUARD_MODES.contains(&trace.done_guard.mode.as_str()) {
        anyhow::bail!(
            "invalid trace.done_guard '{}': expected one of {}",
            trace.done_guard.mode,
            DONE_GUARD_MODES.join("/")
        );
    }
    if profile {
        if let Some(name) = trace.profile.as_deref().filter(|n| !n.is_empty()) {
            if !BUILTIN_PROFILE_NAMES.contains(&name) && !trace.profiles.contains_key(name) {
                let mut known: Vec<&str> = BUILTIN_PROFILE_NAMES.to_vec();
                known.extend(trace.profiles.keys().map(String::as_str));
                known.sort_unstable();
                anyhow::bail!(
                    "invalid trace.profile '{name}': expected one of {} (define custom \
                     profiles under [trace.profiles.<name>] first)",
                    known.join("/")
                );
            }
        }
    }
    if layers {
        let registry = LayerRegistry::build(&trace.layer);
        let unknown: Vec<&str> = trace
            .layers
            .iter()
            .map(String::as_str)
            .filter(|id| registry.get(id).is_none())
            .collect();
        if !unknown.is_empty() {
            let known: Vec<&str> = registry.all().iter().map(|l| l.id.as_str()).collect();
            anyhow::bail!(
                "invalid trace.layers: unknown layer(s) {}; expected any of {}",
                unknown.join(", "),
                known.join("/")
            );
        }
    }
    Ok(())
}

/// Code of the warning `handoff_update_config` adds for an ID prefix the
/// heading parser can never match.
const UNPARSEABLE_ID_PREFIX_CODE: &str = "UNPARSEABLE_ID_PREFIX";

/// One `Warning [UNPARSEABLE_ID_PREFIX]: ...` line per ID prefix written by
/// this update that no heading can match (see [`is_parseable_id_prefix`]:
/// lowercase, a leading digit, `-`, non-ASCII, empty, ...). Such a prefix is
/// saved as given — it is a warning, not a rejection — but an item written
/// under it would never be recognized, so the caller is told at the moment
/// the prefix is registered instead of after a document comes back empty.
///
/// Only what the update touched is checked, like the other trace keys: a
/// `trace.id_prefixes.<layer>` key checks that layer's list, `trace.id_prefixes`
/// (or `trace`) every list, and `trace.layer` (or `trace`) the `id_prefixes`
/// of each `[[trace.layer]]` declaration. A config the update left unparsable
/// yields nothing here; it is not this check's finding.
fn unparseable_id_prefix_warnings<'a>(
    keys: impl Iterator<Item = &'a str>,
    rendered: &str,
) -> Vec<String> {
    const TABLE_KEY: &str = "trace.id_prefixes";
    let (mut all_layers, mut custom_layers) = (false, false);
    let mut touched_layers: Vec<&str> = Vec::new();
    for key in keys {
        match key {
            "trace" => (all_layers, custom_layers) = (true, true),
            TABLE_KEY => all_layers = true,
            "trace.layer" => custom_layers = true,
            k => {
                if let Some(layer) = k
                    .strip_prefix(TABLE_KEY)
                    .and_then(|rest| rest.strip_prefix('.'))
                {
                    touched_layers.push(layer);
                }
            }
        }
    }
    if !(all_layers || custom_layers) && touched_layers.is_empty() {
        return Vec::new();
    }
    let Ok(config) = toml::from_str::<Config>(rendered) else {
        return Vec::new();
    };
    let trace = &config.trace;

    let mut lists: Vec<(String, &[String])> = trace
        .id_prefixes
        .iter()
        .filter(|(layer, _)| all_layers || touched_layers.contains(&layer.as_str()))
        .map(|(layer, prefixes)| (format!("{TABLE_KEY}.{layer}"), prefixes.as_slice()))
        .collect();
    if custom_layers {
        lists.extend(trace.layer.iter().map(|l| {
            (
                format!("trace.layer '{}' id_prefixes", l.id),
                l.id_prefixes.as_slice(),
            )
        }));
    }
    lists.sort_by(|a, b| a.0.cmp(&b.0));

    lists
        .into_iter()
        .filter_map(|(source, prefixes)| {
            let bad: Vec<String> = prefixes
                .iter()
                .filter(|p| !is_parseable_id_prefix(p))
                .map(|p| format!("'{p}'"))
                .collect();
            (!bad.is_empty()).then(|| {
                format!(
                    "Warning [{UNPARSEABLE_ID_PREFIX_CODE}]: {source} contains {}, which no \
                     heading can match: an ID prefix starts with an ASCII uppercase letter and \
                     continues with ASCII uppercase letters or digits only (e.g. RB, RB2, NFR2). \
                     The value was saved as given; items written under it will not be \
                     recognized.",
                    bad.join(", ")
                )
            })
        })
        .collect()
}

/// Returns true when `key` matches the pattern `milestones.<name>.status`,
/// i.e. it starts with `milestones.`, ends with `.status`, and has exactly
/// three dot-separated parts.  Such keys must be rejected by
/// `handoff_update_config` because writing them would bypass the lifecycle
/// transition check and the release guard in `handoff_update_milestone`.
fn is_milestone_status_key(key: &str) -> bool {
    let parts: Vec<&str> = key.split('.').collect();
    parts.len() == 3 && parts[0] == "milestones" && parts[2] == "status"
}

/// Deepest dotted key `handoff_update_config` accepts (`a.b.c.d`).
const MAX_KEY_DEPTH: usize = 4;

fn set_toml_value(doc: &mut DocumentMut, parts: &[&str], json_val: &Value) -> Result<()> {
    let toml_val = json_to_toml_value(json_val)?;
    let Some((leaf, parents)) = parts.split_last() else {
        anyhow::bail!("Empty key");
    };
    if parts.len() > MAX_KEY_DEPTH {
        anyhow::bail!("Key depth > {MAX_KEY_DEPTH} not supported");
    }

    let mut table: &mut dyn TableLike = doc.as_table_mut();
    for parent in parents {
        table = ensure_table_like(table, parent)?;
    }
    table.insert(leaf, Item::Value(toml_val));
    Ok(())
}

fn json_to_toml_value(val: &Value) -> Result<TomlValue> {
    match val {
        Value::String(s) => Ok(TomlValue::from(s.as_str())),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(TomlValue::from(i))
            } else if let Some(f) = n.as_f64() {
                Ok(TomlValue::from(f))
            } else {
                anyhow::bail!("Unsupported number: {n}")
            }
        }
        Value::Bool(b) => Ok(TomlValue::from(*b)),
        Value::Array(arr) => {
            let mut toml_arr = toml_edit::Array::new();
            for item in arr {
                toml_arr.push(json_to_toml_value(item)?);
            }
            Ok(TomlValue::Array(toml_arr))
        }
        Value::Object(obj) => {
            let mut inline = toml_edit::InlineTable::new();
            for (k, v) in obj {
                inline.insert(k, json_to_toml_value(v)?);
            }
            Ok(TomlValue::InlineTable(inline))
        }
        Value::Null => Ok(TomlValue::from("")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn project() -> (tempfile::TempDir, HandlerContext) {
        let tmp = tempfile::tempdir().unwrap();
        let handoff_dir = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff_dir).unwrap();
        std::fs::write(handoff_dir.join("config.toml"), "[project]\nname = \"p\"\n").unwrap();
        let ctx = HandlerContext {
            agent_id: None,
            project_dir: tmp.path().to_path_buf(),
            handoff_dir,
        };
        (tmp, ctx)
    }

    #[test]
    fn update_sets_default_estimate_hours_and_get_reports_it() {
        let (_tmp, ctx) = project();
        handle_update(
            &ctx,
            &json!({ "updates": { "settings.default_estimate_hours": 2.0 } }),
        )
        .unwrap();
        let got: Value = serde_json::from_str(&handle_get(&ctx, &json!({})).unwrap()).unwrap();
        assert_eq!(got["settings"]["default_estimate_hours"], json!(2.0));
    }

    #[test]
    fn update_rejects_non_positive_or_non_numeric_default_estimate_hours() {
        for bad in [json!(0), json!(-1.5), json!("2"), json!(null)] {
            let (_tmp, ctx) = project();
            let err = handle_update(
                &ctx,
                &json!({ "updates": { "settings.default_estimate_hours": bad } }),
            )
            .unwrap_err();
            assert!(
                err.to_string().contains("settings.default_estimate_hours"),
                "{bad}: {err}"
            );
            let raw = std::fs::read_to_string(ctx.handoff_dir.join("config.toml")).unwrap();
            assert!(!raw.contains("default_estimate_hours"), "{raw}");
        }
    }

    #[test]
    fn update_raw_key_rejects_invalid_schedule_mode_and_writes_nothing() {
        for key in ["calendar.schedule_mode", "schedule_mode"] {
            for bad in [json!("bogus"), json!(1), json!(true)] {
                let (_tmp, ctx) = project();
                let err = handle_update(&ctx, &json!({ "updates": { key: bad } })).unwrap_err();
                assert!(
                    err.to_string().contains("schedule_mode"),
                    "{key} {bad}: {err}"
                );
                let raw = std::fs::read_to_string(ctx.handoff_dir.join("config.toml")).unwrap();
                assert!(!raw.contains("schedule_mode"), "{key} {bad}: {raw}");
            }
        }
    }

    #[test]
    fn update_raw_key_accepts_valid_schedule_mode_and_null() {
        for key in ["calendar.schedule_mode", "schedule_mode"] {
            for ok in ["manual", "auto"] {
                let (_tmp, ctx) = project();
                handle_update(&ctx, &json!({ "updates": { key: ok } })).unwrap();
                let raw = std::fs::read_to_string(ctx.handoff_dir.join("config.toml")).unwrap();
                assert!(
                    raw.contains(&format!("schedule_mode = \"{ok}\"")),
                    "{key}: {raw}"
                );
            }
        }
        let (_tmp, ctx) = project();
        handle_update(
            &ctx,
            &json!({ "updates": { "calendar.schedule_mode": "auto" } }),
        )
        .unwrap();
        handle_update(
            &ctx,
            &json!({ "updates": { "calendar.schedule_mode": null } }),
        )
        .unwrap();
    }

    // --- [commands] (SPEC-ONB-001, SPEC-ONB-093) ---

    #[test]
    fn update_sets_a_command_and_get_reports_it() {
        let (_tmp, ctx) = project();
        handle_update(
            &ctx,
            &json!({ "updates": {
                "commands.test": "make test | tee log && echo done",
                "commands.e2e": "npm run e2e"
            } }),
        )
        .unwrap();
        let got: Value = serde_json::from_str(&handle_get(&ctx, &json!({})).unwrap()).unwrap();
        assert_eq!(
            got["commands"]["test"],
            json!("make test | tee log && echo done")
        );
        assert_eq!(got["commands"]["e2e"], json!("npm run e2e"));
        let config = read_config(&ctx.handoff_dir.join("config.toml")).unwrap();
        assert_eq!(
            config.commands.get("test"),
            Some("make test | tee log && echo done")
        );
    }

    #[test]
    fn update_rejects_a_command_that_is_not_a_string_and_writes_nothing() {
        for (key, bad) in [
            ("commands.test", json!(123)),
            ("commands.smoke", json!(true)),
            ("commands.build", json!(["make"])),
            ("commands", json!("make")),
            ("commands", json!({ "test": 1 })),
        ] {
            let (_tmp, ctx) = project();
            let before = std::fs::read_to_string(ctx.handoff_dir.join("config.toml")).unwrap();
            let err = handle_update(&ctx, &json!({ "updates": { key: bad } })).unwrap_err();
            assert!(err.to_string().contains("[commands]"), "{key} {bad}: {err}");
            let after = std::fs::read_to_string(ctx.handoff_dir.join("config.toml")).unwrap();
            assert_eq!(after, before, "{key} {bad}");
        }
    }

    #[test]
    fn update_of_a_command_is_not_blocked_by_a_bad_value_elsewhere() {
        let (_tmp, ctx) = project();
        let path = ctx.handoff_dir.join("config.toml");
        std::fs::write(
            &path,
            "[project]\nname = \"p\"\n\n[settings]\ndone_task_limit = \"many\"\n",
        )
        .unwrap();
        handle_update(
            &ctx,
            &json!({ "updates": { "commands.test": "make test" } }),
        )
        .unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("test = \"make test\""), "{raw}");
    }

    /// Setting the section as a whole stores an inline table; a later
    /// single-key update must add to it, not replace it.
    #[test]
    fn single_key_update_keeps_commands_set_as_a_whole_object() {
        let (_tmp, ctx) = project();
        handle_update(
            &ctx,
            &json!({ "updates": { "commands": { "test": "make test", "smoke": "./smoke.sh" } } }),
        )
        .unwrap();
        handle_update(&ctx, &json!({ "updates": { "commands.build": "make" } })).unwrap();
        let config = read_config(&ctx.handoff_dir.join("config.toml")).unwrap();
        assert_eq!(config.commands.get("test"), Some("make test"));
        assert_eq!(config.commands.get("smoke"), Some("./smoke.sh"));
        assert_eq!(config.commands.get("build"), Some("make"));
    }

    /// The same holds for every section, not only the new ones.
    #[test]
    fn single_key_update_keeps_a_calendar_set_as_a_whole_object() {
        let (_tmp, ctx) = project();
        handle_update(
            &ctx,
            &json!({ "updates": { "calendar": {
                "work_hours_per_day": 6,
                "closed_weekdays": ["sat", "sun"]
            } } }),
        )
        .unwrap();
        handle_update(
            &ctx,
            &json!({ "updates": { "calendar.work_hours_per_day": 7 } }),
        )
        .unwrap();
        let config = read_config(&ctx.handoff_dir.join("config.toml")).unwrap();
        assert_eq!(config.calendar.work_hours_per_day, Some(7.0));
        assert_eq!(config.calendar.closed_weekdays, vec![6, 0]);
    }

    #[test]
    fn update_with_null_clears_a_command() {
        let (_tmp, ctx) = project();
        handle_update(
            &ctx,
            &json!({ "updates": { "commands.test": "make test" } }),
        )
        .unwrap();
        handle_update(&ctx, &json!({ "updates": { "commands.test": null } })).unwrap();
        let config = read_config(&ctx.handoff_dir.join("config.toml")).unwrap();
        assert_eq!(config.commands.get("test"), None);
    }

    /// A typed-key update re-serializes the whole config; `[commands]` must
    /// survive it.
    #[test]
    fn typed_key_update_keeps_the_commands_section() {
        let (_tmp, ctx) = project();
        handle_update(
            &ctx,
            &json!({ "updates": { "commands.test": "make test" } }),
        )
        .unwrap();
        handle_update(
            &ctx,
            &json!({ "updates": { "settings.default_estimate_hours": 2.0 } }),
        )
        .unwrap();
        let config = read_config(&ctx.handoff_dir.join("config.toml")).unwrap();
        assert_eq!(config.commands.get("test"), Some("make test"));
    }

    // --- [release] and [onboarding] secret_file_patterns (SPEC-ONB-040/050) ---

    #[test]
    fn update_sets_manual_checks_and_secret_file_patterns() {
        let (_tmp, ctx) = project();
        handle_update(
            &ctx,
            &json!({ "updates": {
                "release.manual_checks": ["docs_updated", "changelog"],
                "onboarding.secret_file_patterns": ["*.tfstate"],
            } }),
        )
        .unwrap();
        let config = read_config(&ctx.handoff_dir.join("config.toml")).unwrap();
        assert_eq!(
            config.release.manual_checks(),
            ["docs_updated", "changelog"]
        );
        assert_eq!(config.onboarding.secret_file_patterns(), ["*.tfstate"]);
        let shown = handle_get(&ctx, &json!({})).unwrap();
        assert!(shown.contains("docs_updated"), "{shown}");
        assert!(shown.contains("*.tfstate"), "{shown}");
    }

    #[test]
    fn update_rejects_invalid_release_and_pattern_values_without_writing() {
        let (_tmp, ctx) = project();
        let path = ctx.handoff_dir.join("config.toml");
        let before = std::fs::read_to_string(&path).unwrap();
        for (updates, expected) in [
            (
                json!({ "release.manual_checks": ["a", "a"] }),
                "more than once",
            ),
            (json!({ "release.manual_checks": ["a", " "] }), "blank"),
            (
                json!({ "release.manual_checks": "docs_updated" }),
                "invalid [release]",
            ),
            (json!({ "release.manual_checks": [1] }), "invalid [release]"),
            (
                json!({ "onboarding.secret_file_patterns": [".env", ""] }),
                "onboarding.secret_file_patterns",
            ),
            (
                json!({ "onboarding.secret_file_patterns": ".env" }),
                "invalid [onboarding]",
            ),
        ] {
            let err = handle_update(&ctx, &json!({ "updates": updates }))
                .unwrap_err()
                .to_string();
            assert!(err.contains(expected), "{updates}: {err}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "{updates}");
        }
    }

    // --- milestones.*.status bypass guard (t454) ---

    /// `milestones.<name>.status` must be rejected by `update_config` because
    /// it bypasses the lifecycle transition check and the release guard that
    /// `handoff_update_milestone` enforces. The error message should direct
    /// the caller to use `handoff_update_milestone` instead.
    #[test]
    fn update_config_rejects_milestones_status_key() {
        let (_tmp, ctx) = project();
        // Seed a milestone so the table exists.
        let path = ctx.handoff_dir.join("config.toml");
        std::fs::write(
            &path,
            "[project]\nname = \"p\"\n\n[milestones.v1]\ndate = \"2026-01-01\"\n",
        )
        .unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        for status_val in [json!("released"), json!("active"), json!("planning")] {
            let err = handle_update(
                &ctx,
                &json!({ "updates": { "milestones.v1.status": status_val } }),
            )
            .unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("handoff_update_milestone"),
                "error should mention handoff_update_milestone: {msg}"
            );
            assert!(
                msg.contains("milestones") && msg.contains("status"),
                "error should mention the rejected key: {msg}"
            );
            // Config file must be untouched.
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                before,
                "config must not be written for status_val={status_val}"
            );
        }
    }

    /// Other milestones subkeys (date, color, description, etc.) should still
    /// pass through `update_config` without issue — only `status` is blocked.
    #[test]
    fn update_config_allows_non_status_milestone_keys() {
        let (_tmp, ctx) = project();
        let path = ctx.handoff_dir.join("config.toml");
        std::fs::write(
            &path,
            "[project]\nname = \"p\"\n\n[milestones.v1]\ndate = \"2026-01-01\"\n",
        )
        .unwrap();

        let result = handle_update(
            &ctx,
            &json!({ "updates": { "milestones.v1.date": "2026-06-01" } }),
        );
        assert!(
            result.is_ok(),
            "non-status key should be accepted: {result:?}"
        );

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("2026-06-01"), "date should be updated: {raw}");
    }

    /// A typed-key update re-serializes the whole config; `[release]` and the
    /// patterns must survive it.
    #[test]
    fn typed_key_update_keeps_the_release_section_and_the_patterns() {
        let (_tmp, ctx) = project();
        handle_update(
            &ctx,
            &json!({ "updates": {
                "release.manual_checks": ["docs_updated"],
                "onboarding.secret_file_patterns": [],
            } }),
        )
        .unwrap();
        handle_update(
            &ctx,
            &json!({ "updates": { "settings.default_estimate_hours": 2.0 } }),
        )
        .unwrap();
        let config = read_config(&ctx.handoff_dir.join("config.toml")).unwrap();
        assert_eq!(config.release.manual_checks(), ["docs_updated"]);
        assert_eq!(
            config.onboarding.secret_file_patterns,
            Some(Vec::new()),
            "an explicitly empty list is kept"
        );
    }
}
