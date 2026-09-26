//! `handoff_trace_record` (wiki/220-vmodel-integration-design.md §2.6/§3.1,
//! FR-302) — M1 (t360.8). Records one execution batch (a set of
//! `{item, result}` pairs, e.g. one CI run or one manual verification pass)
//! as a single `runs/<run_id>.json` file and refreshes the derived
//! `runs/_latest.json` cache. The `handoff_trace_report`/`handoff_trace_slice`
//! tools named alongside this one in wiki/220 §3 are a separate, later task
//! (t360.9/t360.10) — not implemented here.

use anyhow::Result;
use serde_json::{json, Value};

use super::HandlerContext;
use crate::storage::docs::read_all_docs;
use crate::storage::runs::{is_valid_result, record_run, RunResultInput};

/// `handoff_trace_record` (§3.1). Input: `results: [{item, result, note?,
/// evidence?[]}]` (required, non-empty), `executor_kind?` (`"ai"` | `"human"`,
/// default `"ai"`), `executor_id?`, `commit?` (defaults to `git rev-parse
/// --short HEAD`, empty on failure), `task_id?`. Output: `{run_id, recorded,
/// warnings}`.
pub fn handle_trace_record(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;
    let project_dir = &ctx.project_dir;

    let results_val = arguments
        .get("results")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("'results' (non-empty array) is required"))?;
    if results_val.is_empty() {
        anyhow::bail!("'results' must not be empty");
    }

    let mut warnings = Vec::new();
    let mut inputs = Vec::with_capacity(results_val.len());
    for (i, entry) in results_val.iter().enumerate() {
        let item = entry
            .get("item")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("results[{i}].item is required"))?;
        let result = entry
            .get("result")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("results[{i}].result is required"))?;
        if !is_valid_result(result) {
            anyhow::bail!(
                "results[{i}].result={result:?} is invalid (must be one of pass, fail, blocked, \
                 not_run, skipped)"
            );
        }
        let note = entry.get("note").and_then(|v| v.as_str());
        let evidence: Vec<String> = entry
            .get("evidence")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        inputs.push(RunResultInput {
            item,
            result,
            note,
            evidence,
        });
    }

    let executor_kind = arguments
        .get("executor_kind")
        .and_then(|v| v.as_str())
        .unwrap_or("ai");
    if executor_kind != "ai" && executor_kind != "human" {
        anyhow::bail!("executor_kind={executor_kind:?} must be \"ai\" or \"human\"");
    }
    let executor_id = arguments.get("executor_id").and_then(|v| v.as_str());
    let commit = match arguments.get("commit").and_then(|v| v.as_str()) {
        Some(c) => Some(c.to_string()),
        None => Some(crate::storage::git::short_head_or_empty(project_dir)),
    };
    let task_id = arguments
        .get("task_id")
        .and_then(|v| v.as_str())
        .map(String::from);

    let docs = read_all_docs(handoff)?;
    let (run_id, mut record_warnings) = record_run(
        handoff,
        &docs,
        &inputs,
        executor_kind,
        executor_id,
        commit,
        task_id,
    )?;
    warnings.append(&mut record_warnings);

    let out = json!({
        "run_id": run_id,
        "recorded": inputs.len(),
        "warnings": warnings,
    });
    Ok(serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string()))
}
