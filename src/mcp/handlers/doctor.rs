//! `handoff_doctor` (SPEC-ONB-002): reports whether the project is ready for
//! the task loop.
//!
//! This handler never runs a command and never starts a process. It hands the
//! commands that should be run back to the caller (`commands_to_run`) and
//! judges the results the caller reports in `results`. The diagnosis itself,
//! shared with `handoff-mcp setup --check`, is `crate::setup::doctor`.

use std::path::PathBuf;

use anyhow::Result;
use serde_json::Value;

use super::HandlerContext;
use crate::setup::doctor::{diagnose, CommandResult, DoctorRequest, ResultSource};

pub fn handle(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let checks = parse_checks(arguments.get("checks"))?;
    let results = parse_results(arguments.get("results"))?;
    let home_dir = crate::storage::home_dir().map(PathBuf::from);

    let report = diagnose(&DoctorRequest {
        project_dir: &ctx.project_dir,
        home_dir: home_dir.as_deref(),
        checks: checks.as_deref(),
        results: &results,
        source: ResultSource::Reported,
        // Reporting results is how an agent asks for a judgment of a run.
        run_requested: !results.is_empty(),
    })?;
    Ok(serde_json::to_string_pretty(&report)?)
}

fn parse_checks(value: Option<&Value>) -> Result<Option<Vec<String>>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str().map(String::from).ok_or_else(|| {
                    anyhow::anyhow!("`checks` must be an array of check ids (strings), got {item}")
                })
            })
            .collect::<Result<Vec<_>>>()
            .map(Some),
        Some(other) => {
            anyhow::bail!("`checks` must be an array of check ids (strings), got {other}")
        }
    }
}

/// Parses the `results` argument `handoff_doctor` and `handoff_release_check`
/// share.
pub(super) fn parse_results(value: Option<&Value>) -> Result<Vec<CommandResult>> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                serde_json::from_value::<CommandResult>(item.clone())
                    .map_err(|e| anyhow::anyhow!("`results[{index}]` is not a valid result: {e}"))
            })
            .collect(),
        Some(other) => anyhow::bail!(
            "`results` must be an array of {{id, command, exit_code?, duration_ms?, \
             output_tail?, error?}}, got {other}"
        ),
    }
}
