//! `handoff_trace_matrix` (wiki/260-vmodel-m2-design.md §4.4, t360.20.9/
//! M2-09): read-only (E6) rendering of the whole trace graph as a flat
//! tree/edges table, in CSV or Markdown, for VSCode's matrix export (t135,
//! §5.4: "TS 側で CSV / Markdown の生成を再実装しない", NFR-005) and any other
//! external-tool consumer. Uses the same fully-read-only load
//! (`trace_readonly::load_trace_input_fully_read_only`) `handoff_trace_lint`/
//! `handoff_trace_suspect`'s `list`/`baseline(dry_run)`/`handoff_trace_impact`
//! already use — in-memory-only layer-doc resync, `runs::load_latest_readonly`,
//! `task_ids` resolved from the task side without self-repair — so this tool
//! never writes anything under `.handoff/`. The only write this tool can ever
//! make is `output_file` itself (§4.4), resolved against `ctx.project_dir`
//! (not `.handoff/`) and rejected outright if it would escape the project
//! directory.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use anyhow::Result;
use serde_json::{json, Value};

use super::trace_readonly::{dedup_preserve_order, load_trace_input_fully_read_only};
use super::HandlerContext;
use crate::trace::matrix::{
    build_edges_table, build_tree_table, collect_item_layers, default_root_layer,
    ordered_in_use_layers, render_csv, render_markdown, resolve_columns,
};
use crate::trace::types::TraceInput;
use crate::trace::TraceGraph;

fn string_array_arg(arguments: &Value, key: &str) -> Vec<String> {
    arguments
        .get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// stable_id -> every task id with a `requirement`-type link to it,
/// regardless of role (sorted, deduped) — §4.4's `tasks` column doesn't
/// distinguish implements vs. executes, unlike `trace.rs`'s private
/// `tasks_by_item`/`trace_slice`'s `tasks[].role` output. Small, local
/// duplication of that function's id-collection half rather than widening
/// its (and its return type's) visibility for this one caller — same policy
/// already applied elsewhere in this module family (e.g.
/// `trace_propose.rs`'s duplicated scope-overlap check).
fn tasks_by_item(trace_input: &TraceInput) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for link in &trace_input.task_requirement_links {
        out.entry(link.stable_id.clone())
            .or_default()
            .push(link.task_id.clone());
    }
    for tasks in out.values_mut() {
        tasks.sort();
        tasks.dedup();
    }
    out
}

/// Resolves `rel` against `project_dir` (§4.4: "output_file?（プロジェクト内
/// の相対パス。外に出るパスは拒否）") — rejects an absolute path or one
/// containing a `..` component outright, before ever touching the
/// filesystem (no `canonicalize`, which would fail for a file that doesn't
/// exist yet, the common case here).
fn resolve_output_file(project_dir: &Path, handoff_dir: &Path, rel: &str) -> Result<PathBuf> {
    let candidate = Path::new(rel);
    if candidate.is_absolute() {
        anyhow::bail!(
            "output_file must be a relative path inside the project, got an absolute path: {rel:?}"
        );
    }
    if candidate
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        anyhow::bail!("output_file must not escape the project directory: {rel:?}");
    }
    let resolved = project_dir.join(candidate);
    // §4.4/§6: this tool is E6 read-only for `.handoff/` — `output_file` is
    // its only write and must never land inside `.handoff/` (it would
    // bypass `WRITE_MUTEX`, which `READ_ONLY_TOOLS` membership skips, and
    // could clobber task/doc/config files). The first-component check also
    // covers a case-insensitive filesystem (`.HANDOFF/...`).
    let first_is_handoff = candidate
        .components()
        .find_map(|c| match c {
            Component::Normal(n) => Some(n),
            _ => None,
        })
        .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(".handoff"));
    if first_is_handoff || resolved.starts_with(handoff_dir) {
        anyhow::bail!("output_file must not point inside .handoff/: {rel:?}");
    }
    Ok(resolved)
}

pub fn handle_trace_matrix(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let format = arguments
        .get("format")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("format is required: must be \"markdown\" or \"csv\""))?;
    if format != "markdown" && format != "csv" {
        anyhow::bail!("format={format:?} must be \"markdown\" or \"csv\"");
    }

    let shape = arguments
        .get("shape")
        .and_then(|v| v.as_str())
        .unwrap_or("tree");
    if shape != "tree" && shape != "edges" {
        anyhow::bail!("shape={shape:?} must be \"tree\" or \"edges\"");
    }

    let include_tasks = arguments
        .get("include_tasks")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let requested_layers = string_array_arg(arguments, "layers");
    let requested_layers_opt = if requested_layers.is_empty() {
        None
    } else {
        Some(requested_layers.as_slice())
    };

    // E6: fully read-only load (in-memory-only resync, runs::load_latest_readonly,
    // task_ids resolved from the task side without self-repair) — same
    // contract handoff_trace_lint/handoff_trace_impact already use.
    let read_only = load_trace_input_fully_read_only(handoff, Vec::new())?;
    let graph = TraceGraph::build(&read_only.loaded.trace_input);
    let registry = read_only.loaded.layer_registry.all();

    let mut warnings = read_only.warnings.clone();
    warnings.extend(read_only.loaded.config_warnings.clone());
    dedup_preserve_order(&mut warnings);

    let ordered = ordered_in_use_layers(&graph, registry);
    let (columns, column_warnings) = resolve_columns(&ordered, requested_layers_opt);
    warnings.extend(column_warnings);

    let item_layers = collect_item_layers(&read_only.loaded.trace_input.items);

    let mut root_layer_out: Option<String> = None;
    let table = if shape == "tree" {
        let root_layer = match arguments.get("root_layer").and_then(|v| v.as_str()) {
            Some(explicit) => {
                if !registry.iter().any(|l| l.id == explicit) {
                    anyhow::bail!("root_layer={explicit:?} is not a registered layer id");
                }
                explicit.to_string()
            }
            // §7: no left layer currently in use at all (e.g. a layerless
            // project) falls through to an empty string — `build_tree_table`
            // then legitimately finds zero root items and returns a
            // correctly-headered, zero-row table rather than this handler
            // needing a separate empty-table construction path.
            None => default_root_layer(&ordered, registry).unwrap_or_default(),
        };
        root_layer_out = if root_layer.is_empty() {
            None
        } else {
            Some(root_layer.clone())
        };
        let tasks = tasks_by_item(&read_only.loaded.trace_input);
        build_tree_table(
            &graph,
            &item_layers,
            &tasks,
            &root_layer,
            &columns,
            include_tasks,
        )
    } else {
        build_edges_table(&graph, &item_layers)
    };

    let content = if format == "csv" {
        render_csv(&table)
    } else {
        render_markdown(&table)
    };

    let mut out = json!({
        "format": format,
        "shape": shape,
        "columns": table.headers,
        "rows": table.rows.len(),
        "warnings": warnings,
    });
    if shape == "tree" {
        out["root_layer"] = json!(root_layer_out);
    }

    if let Some(output_file) = arguments.get("output_file").and_then(|v| v.as_str()) {
        let resolved = resolve_output_file(&ctx.project_dir, &ctx.handoff_dir, output_file)?;
        if let Some(parent) = resolved.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                anyhow::anyhow!("failed to create directory for output_file {output_file:?}: {e}")
            })?;
        }
        std::fs::write(&resolved, &content)
            .map_err(|e| anyhow::anyhow!("failed to write output_file {output_file:?}: {e}"))?;
        out["output_file"] = json!(output_file);
    } else {
        out["content"] = json!(content);
    }

    Ok(serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_output_file_accepts_a_plain_relative_path_inside_the_project() {
        let project = Path::new("/p");
        let got = resolve_output_file(project, &project.join(".handoff"), "out/m.csv").unwrap();
        assert_eq!(got, project.join("out/m.csv"));
    }

    #[test]
    fn resolve_output_file_rejects_any_path_inside_handoff() {
        let project = Path::new("/p");
        let handoff = project.join(".handoff");
        for rel in [
            ".handoff/config.toml",
            "./.handoff/tasks/x.json",
            ".HANDOFF/config.toml",
        ] {
            let err = resolve_output_file(project, &handoff, rel).unwrap_err();
            assert!(err.to_string().contains(".handoff"), "{rel}: {err}");
        }
    }

    #[test]
    fn resolve_output_file_rejects_absolute_and_parent_dir_paths() {
        let project = Path::new("/p");
        let handoff = project.join(".handoff");
        assert!(resolve_output_file(project, &handoff, "/tmp/x.csv").is_err());
        assert!(resolve_output_file(project, &handoff, "a/../../x.csv").is_err());
    }
}
