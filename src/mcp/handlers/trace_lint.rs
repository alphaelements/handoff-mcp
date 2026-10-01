//! `handoff_trace_lint` (wiki/260-vmodel-m2-design.md §4.3, t360.20.8/M2-08):
//! read-only (E6) lint over the whole trace graph — structural gaps (M1),
//! change (suspect/unbaselined), tailoring (waivers/acceptance/profile),
//! drift (FR-801: unsynced bodies, task_ids drift, dangling task links,
//! orphaned/legacy data, unreadable frontmatter), and project-defined
//! `[[trace.lint.require]]` policy rules. See `src/trace/lint.rs` for the
//! pure rule-evaluation core this handler wires up to the E6 read-only load
//! (`src/mcp/handlers/trace_readonly.rs`).
//!
//! CLI exit codes (§4.3/§5.3, `src/cli.rs`): `0` = no finding at or above
//! `fail_on`, `1` = at least one, `2` = usage/config error — an unknown rule
//! id in `rules`/`--rules`, an invalid `fail_on`/`format`, a `config.toml`
//! that exists but fails to parse, a `[[trace.lint.require]]` entry with an
//! empty/unknown `id`/`need`/`severity`, a `[trace.lint.rules]` override
//! naming an unknown rule id or an unrecognized severity value
//! (`validate_lint_config`, `src/trace/lint.rs`), or this handler returning
//! `Err` for any other reason — see `cli.rs`'s own doc comment on its `trace
//! lint` special case for why every handler error is treated as a
//! usage/config error for this one tool.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use serde_json::{json, Value};

use super::trace_readonly::{dedup_preserve_order, load_trace_input_fully_read_only};
use super::HandlerContext;
use crate::storage::config::{read_config, TraceLintConfig};
use crate::trace::lint::{
    evaluate, is_known_rule_id, validate_lint_config, ItemLintMeta, LintContext, LintFinding,
    Severity,
};
use crate::trace::TraceGraph;

/// `SubItem.status` -> the approval axis (wiki/260 §3.3/E12) — mirrors
/// `trace.rs`'s private `approval_str` (duplicated rather than imported: that
/// function lives in a file already carrying unrelated `_trace_report.json`
/// JSON-shaping responsibilities this handler has no other reason to depend
/// on).
fn approval_str(status: &str) -> &'static str {
    if status == "verified" {
        "approved"
    } else {
        "draft"
    }
}

/// `stable_id -> {doc_slug, priority, approval}` from `docs` — everything
/// `src/trace/lint.rs`'s `require` rules (`when.doc`/`when.priority`/
/// `when.approval`) and every other rule's `doc` output field need beyond
/// `TraceGraph`/`TraceInput` themselves.
fn collect_item_lint_meta(
    docs: &[crate::storage::docs::DocMetadata],
) -> HashMap<String, ItemLintMeta> {
    let mut out = HashMap::new();
    for doc in docs {
        let Some(v) = &doc.verification else {
            continue;
        };
        for item in &v.items {
            for sub in &item.sub_items {
                let Some(id) = sub.stable_id.clone() else {
                    continue;
                };
                out.entry(id).or_insert_with(|| ItemLintMeta {
                    doc_slug: doc.slug.clone(),
                    priority: sub.priority.clone(),
                    approval: approval_str(&sub.status).to_string(),
                });
            }
        }
    }
    out
}

fn severity_str(s: Severity) -> &'static str {
    s.as_str()
}

fn finding_json(f: &LintFinding) -> Value {
    json!({
        "rule": f.rule,
        "severity": severity_str(f.severity),
        "item": f.item,
        "task": f.task,
        "doc": f.doc,
        "message": f.message,
    })
}

/// Renders `findings` as the `text` format (`format: "text"`, CLI-oriented,
/// §4.3) — one line per finding, `severity[RULE] item/task/doc: message`,
/// already in the deterministic order `evaluate` returned them in.
fn render_text(findings: &[LintFinding]) -> String {
    let mut out = String::new();
    for f in findings {
        let subject = f
            .item
            .clone()
            .or_else(|| f.task.clone())
            .or_else(|| f.doc.clone())
            .unwrap_or_default();
        out.push_str(&format!(
            "{}[{}] {}: {}\n",
            severity_str(f.severity),
            f.rule,
            subject,
            f.message
        ));
    }
    out
}

/// Loads `[trace.lint]` from `config.toml` (M2-08 rework, reviewer round 1
/// MAJOR finding): a missing `config.toml` is the project default (every
/// other `read_config(...).unwrap_or_default()` call site's reasoning
/// applies equally here — no config file is simply "nothing configured
/// yet"), but a `config.toml` that exists and fails to parse is a real
/// usage/config error and must bail rather than silently falling back to
/// defaults — wiki/260 §4.3 reserves CLI exit 2 for exactly this, and a CI
/// job relying on `[[trace.lint.require]]` policy rules must not see them
/// vanish because the file became unparseable.
fn load_trace_lint_config(handoff: &std::path::Path) -> Result<TraceLintConfig> {
    let config_path = handoff.join("config.toml");
    if !config_path.exists() {
        return Ok(TraceLintConfig::default());
    }
    let config = read_config(&config_path)?;
    validate_lint_config(&config.trace.lint).map_err(|e| anyhow::anyhow!(e))?;
    Ok(config.trace.lint)
}

pub fn handle_trace_lint(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let trace_config = load_trace_lint_config(handoff)?;

    // t360.20.32 (M2-S8 reviewer): a `rules` array that is *present* but
    // yields zero ids is a usage error, not "no filter requested" — the CLI's
    // `--rules ""` (`cli.rs::parse_value`'s comma-split + empty-string
    // filter, `ARRAY_FIELDS`) turns an empty string into `rules: []` before
    // this handler ever sees it, so a bare `.filter(|s| !s.is_empty())` here
    // would silently fold that back into "omitted" and run with every rule
    // enabled — the exact "nothing to report" footgun the unknown-rule-id
    // check below already guards against. Distinguishing "key absent"
    // (`None`, no filter) from "key present but empty" (bail) requires
    // matching on the array itself rather than filtering after collecting.
    let rules_filter: Option<HashSet<String>> = match arguments
        .get("rules")
        .and_then(|v| v.as_array())
    {
        Some(arr) => {
            let ids: HashSet<String> = arr
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            if ids.is_empty() {
                anyhow::bail!("rules: must not be empty (omit --rules entirely to run every rule)");
            }
            Some(ids)
        }
        None => None,
    };
    // An unknown rule id is rejected rather than silently dropped
    // (`trace_suspect`'s `kinds` filter applies the same policy): a typo'd
    // `--rules unverfied` would otherwise filter out every finding and read
    // as "nothing to report" — the one answer this tool must never give by
    // accident.
    if let Some(ids) = &rules_filter {
        for id in ids {
            if !is_known_rule_id(id, &trace_config) {
                anyhow::bail!(
                    "rules: unknown rule id {id:?} (not a built-in rule or a \
                     [[trace.lint.require]] id)"
                );
            }
        }
    }

    let fail_on = arguments
        .get("fail_on")
        .and_then(|v| v.as_str())
        .unwrap_or("error");
    let fail_on_severity = match fail_on {
        "error" => Severity::Error,
        "warning" => Severity::Warning,
        other => anyhow::bail!("fail_on={other:?} must be \"error\" or \"warning\""),
    };

    let format = arguments
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("json");
    if format != "json" && format != "text" {
        anyhow::bail!("format={format:?} must be \"json\" or \"text\"");
    }

    let limit = arguments
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);

    // E6: fully read-only load (in-memory-only resync, runs::load_latest_readonly,
    // task_ids resolved from the task side without self-repair).
    let read_only = load_trace_input_fully_read_only(handoff, Vec::new())?;
    let graph = TraceGraph::build(&read_only.loaded.trace_input);
    let item_meta = collect_item_lint_meta(&read_only.loaded.docs);

    let lint_ctx = LintContext {
        docs: &read_only.loaded.docs,
        item_meta: &item_meta,
        unreadable: &read_only.unreadable,
        task_ids_drift: &read_only.task_ids_drift,
        per_doc_sync_warnings: &read_only.per_doc_sync_warnings,
        resynced_doc_slugs: &read_only.resynced_doc_slugs,
    };

    let mut findings = evaluate(
        &graph,
        &read_only.loaded.trace_input,
        &lint_ctx,
        &trace_config,
        rules_filter.as_ref(),
    );

    let mut counts = json!({"error": 0, "warning": 0, "info": 0});
    for f in &findings {
        let key = severity_str(f.severity);
        counts[key] = json!(counts[key].as_i64().unwrap_or(0) + 1);
    }

    // `exit_code` reflects every matching finding, not just the ones that
    // survive `limit` below: `limit` only bounds the response size, and
    // `counts` is likewise computed before truncation (a `--limit 0` run
    // must not report exit 0 while `counts.error > 0`).
    let has_failing = findings.iter().any(|f| f.severity >= fail_on_severity);
    let exit_code = if has_failing { 1 } else { 0 };

    let matching_total = findings.len();
    // M2-08 rework (reviewer round 1 MAJOR finding): `read_only.warnings`
    // alone omits `read_only.loaded.config_warnings` — the layer-registry and
    // profile-resolution warnings wiki/260 §2.1 requires `trace_lint` to
    // surface the same as `trace_report` does. Deduped (preserving order)
    // because a registry warning also gets re-emitted into
    // `read_only.warnings` once per in-memory-resynced layer document
    // (`sync_layer_items_local` rebuilds its own `LayerRegistry` and extends
    // its warnings with it) — without dedup, N resynced documents would turn
    // one misconfigured `[[trace.layer]]` into N+1 copies of the same
    // warning.
    let mut warnings = read_only.warnings.clone();
    warnings.extend(read_only.loaded.config_warnings.clone());
    dedup_preserve_order(&mut warnings);
    if let Some(limit) = limit {
        if findings.len() > limit {
            findings.truncate(limit);
            warnings.push(format!(
                "findings truncated to limit={limit} of {matching_total} matching entries"
            ));
        }
    }

    let mut out = json!({
        "findings": findings.iter().map(finding_json).collect::<Vec<_>>(),
        "counts": counts,
        "exit_code": exit_code,
        "warnings": warnings,
    });
    if format == "text" {
        out["text"] = json!(render_text(&findings));
    }

    Ok(serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string()))
}
