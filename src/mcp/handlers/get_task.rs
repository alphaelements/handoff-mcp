use anyhow::Result;
use serde_json::Value;

use super::HandlerContext;
use crate::storage::tasks::{
    find_dependents, find_task_dir_by_id, read_task, suggest_task_id, TaskLink,
};

/// wiki/260-vmodel-m2-design.md §4.11/§3.4 (M2-13): `get_task`/
/// `task_checklist(view)`'s `trace: {layers, blockers}` field — `None` when
/// `task_links` has no `requirement`-type entry (§3.4's "requirement リンク
/// のないタスクは何もしない", shared with `compute_task_blockers_for_task`'s
/// own early return).
///
/// Read-only (E6, wiki/260 §4.11's "読み取り専用（E6 の経路）"): `DocSet::load`
/// never writes, and `runs::load_latest_readonly` (never `runs::sync`) never
/// writes `runs/_latest.json`. A directly-edited (on-disk, never
/// `doc_save`d) layer document is **not** re-synced in memory here — unlike
/// `trace_readonly`'s full E6 loader (`handoff_trace_lint`/
/// `handoff_trace_suspect`/`handoff_trace_impact`), this path only backs a
/// single task's advisory view and must stay within the much tighter PR-6
/// (≤50ms) budget those tools don't have; `trace_report`/`trace_lint` remain
/// the authoritative, fully-resynced view. See
/// `compute_task_blockers_for_task`'s own doc comment for the rest of this
/// function's scope (`doc_profile_overrides` is not applied either).
pub(crate) fn load_task_trace_view(
    handoff: &std::path::Path,
    task_id: &str,
    task_links: &[TaskLink],
) -> Result<Option<Value>> {
    // PR-2/PR-6 fast path (wiki/260 §3.4/§4.11: "requirement リンクのない
    // タスクには何もしない" / "層・ブロッカーは…リンクのあるタスクだけ"): the
    // overwhelming majority of `get_task`/`task_checklist(view)` calls are
    // for a task with no `requirement`-type link at all — never pay for a
    // `DocSet::load` (a full-corpus read) for those.
    if !task_links.iter().any(|l| l.link_type == "requirement") {
        return Ok(None);
    }
    let doc_set = crate::storage::docs::DocSet::load(handoff)?;
    let trace_config = crate::storage::config::read_config(&handoff.join("config.toml"))
        .map(|c| c.trace)
        .unwrap_or_default();
    let registry = crate::storage::docs::layer::LayerRegistry::build(&trace_config.layer);
    let runs_cache = crate::storage::runs::load_latest_readonly(handoff)?;
    let view = crate::trace::compute_task_blockers_for_task(
        doc_set.docs(),
        &registry,
        &trace_config,
        &runs_cache,
        task_id,
        task_links,
    );
    Ok(view.map(|v| serde_json::json!({ "layers": v.layers, "blockers": v.blockers })))
}

pub fn handle(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;
    let tasks_dir = handoff.join("tasks");

    let task_id = arguments
        .get("task_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'task_id' parameter is required"))?;

    let task_dir = find_task_dir_by_id(&tasks_dir, task_id)?
        .ok_or_else(|| anyhow::anyhow!("{}", suggest_task_id(&tasks_dir, task_id)))?;

    let (data, status) = read_task(&task_dir)?
        .ok_or_else(|| anyhow::anyhow!("Task file not found in {}", task_dir.display()))?;

    // `links` stays the legacy `Vec<String>` for backward compatibility with
    // existing clients (skills / VSCode extension). `task_links` is an
    // additive field carrying the normalized, deduplicated view from the
    // `links()` accessor (wiki/130-document-management.md §9.1), so callers
    // that understand typed links (doc/url/file/task) can read them without
    // re-deriving the merge themselves.
    let normalized_links = data.links();

    // Reverse of `dependencies`: tasks that depend ON this one. A reviewer
    // deciding whether an apparently-unwired piece of this task is actually
    // deferred to a later stage of the same breakdown needs this to check the
    // later task's own scope, rather than guessing from prose alone. Opt-in
    // (default false): finding dependents scans every task file in the
    // project, and most `handoff_get_task` callers have no use for it.
    let include_dependents = arguments
        .get("include_dependents")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let dependents = if include_dependents {
        Some(find_dependents(&tasks_dir, task_id)?)
    } else {
        None
    };

    let trace = load_task_trace_view(handoff, task_id, &data.task_links)?;

    // Opt-in like `dependents`, and for the same reason: answering it can
    // mean reading every document and the task's subtree, which the plain
    // read must never pay for. The key is added only when asked for, so the
    // default response stays what it always was.
    let orphan = match arguments.get(INCLUDE_ORPHAN) {
        None | Some(Value::Null) | Some(Value::Bool(false)) => None,
        Some(Value::Bool(true)) => Some(
            stored_task_is_orphan(handoff, &task_dir, &data, &status)
                .map_err(|e| anyhow::anyhow!("orphan check failed for task {task_id}: {e:#}"))?,
        ),
        Some(_) => anyhow::bail!("'{INCLUDE_ORPHAN}' must be a boolean"),
    };

    let mut result = serde_json::json!({
        "id": data.id,
        "title": data.title,
        "status": status,
        "notes": data.notes,
        "priority": data.priority,
        "created_at": data.created_at,
        "updated_at": data.updated_at,
        "completed_at": data.completed_at,
        "labels": data.labels,
        "links": data.links,
        "task_links": normalized_links,
        "done_criteria": data.done_criteria,
        "schedule": data.schedule,
        "dependencies": data.dependencies,
        "dependents": dependents,
        "order": data.order,
        "assignee": data.assignee,
        "lock": data.lock,
        "scope_paths": data.scope_paths,
        "trace": trace,
    });
    if let Some(orphan) = orphan {
        result["orphan"] = Value::Bool(orphan);
    }

    serde_json::to_string_pretty(&result).map_err(Into::into)
}

/// Argument that asks `handoff_get_task` for the `orphan` field.
const INCLUDE_ORPHAN: &str = "include_orphan";

/// Whether the stored task is an orphan: the shared predicate
/// (`crate::trace::orphan`, the one the `task_orphan` lint rule, the orphan
/// guard and the reports use) applied to the task as it is on disk, with its
/// subtree and the scope guard. Read-only.
fn stored_task_is_orphan(
    handoff: &std::path::Path,
    task_dir: &std::path::Path,
    data: &crate::storage::tasks::TaskData,
    status: &str,
) -> Result<bool> {
    crate::storage::orphan_tasks::task_is_orphan(
        handoff,
        crate::trace::orphan::TaskFacts {
            id: data.id.clone(),
            title: data.title.clone(),
            status: status.to_string(),
            labels: data.labels.clone(),
            has_requirement_link: crate::trace::orphan::has_requirement_link(&data.task_links),
            children: Vec::new(),
        },
        Some(task_dir),
        // Nothing is pending: the task is judged as stored.
        |_docs| false,
    )
}
