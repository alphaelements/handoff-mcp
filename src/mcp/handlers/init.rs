use anyhow::Result;
use serde_json::Value;

use super::HandlerContext;
use crate::storage;
use crate::storage::config::ONBOARDING_PRESETS;
use crate::storage::config_preset;

/// `handoff_init` runs before `.handoff/` exists, so unlike every other
/// handler it must not rely on `ctx.handoff_dir` (which the dispatch layer
/// may not have been able to populate for an uninitialized project). Only
/// `ctx.project_dir` is used here; the handoff directory is computed
/// directly via `storage::init_handoff`.
pub fn handle(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let project_dir = &ctx.project_dir;

    let project_name = arguments
        .get("project_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("project_name is required"))?;

    let description = arguments
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let preset = match arguments.get("preset") {
        None | Some(Value::Null) => None,
        Some(Value::String(name)) => Some(config_preset::resolve_preset_name(name)?),
        Some(other) => anyhow::bail!(
            "preset must be a string (one of {}), got {other}",
            ONBOARDING_PRESETS.join("/")
        ),
    };

    let applied =
        storage::init_handoff_with_preset(project_dir, project_name, description, preset)?;

    // Version marker (spec §3.7): lets `handoff_load_context` warn when a
    // different handoff-mcp binary later opens the same shared `.handoff/`,
    // since mixed versions can silently ignore each other's lock fields.
    let handoff_dir = storage::handoff_dir(project_dir);
    storage::atomic_write(
        handoff_dir.join("version"),
        env!("CARGO_PKG_VERSION").as_bytes(),
    )?;

    let mut reply = format!(
        "Initialized handoff tracking for '{}' at {}/.handoff/\n\
         Created: config.toml, sessions/, tasks/, memory/",
        project_name,
        project_dir.display()
    );
    if let Some(plan) = applied {
        let settings = plan
            .changes
            .iter()
            .map(|change| format!("{} = {}", change.key, change.new))
            .collect::<Vec<_>>()
            .join(", ");
        reply.push_str(&format!("\nApplied preset '{}': {settings}", plan.preset));
    }
    Ok(reply)
}
