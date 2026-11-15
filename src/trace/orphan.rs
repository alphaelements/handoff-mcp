//! Orphan tasks: tasks that sit outside requirement traceability.
//!
//! A task is an orphan when nothing ties it to a requirement: it carries no
//! `requirement`-type task link, it is not deliberately excluded, and it is
//! not a pure grouping task whose work is traced through its children. This
//! module holds the one predicate every consumer shares — the `task_orphan`
//! lint rule, the orphan guard on `review`/`done` transitions, the creation
//! guidance, and the report section — so they can never disagree about which
//! tasks are orphans. Pure: no file I/O (the loaders live in
//! `crate::storage::orphan_tasks`).
//!
//! The rule, for one task:
//!
//! - it is **accounted for** when it has a `requirement`-type link, carries
//!   the [`NO_TRACE_LABEL`] label, or has status `skipped`;
//! - a task with children that is not accounted for itself is still excluded
//!   when no task below it is an orphan, i.e. every leaf under it is
//!   accounted for (a parent's own link never vouches for its children, and a
//!   child's own link never vouches for the grandchildren);
//! - every other task is an orphan, whatever its status (`done` included).
//!
//! Scope guard: a project with no layer document does not use requirement
//! traceability at all, so nothing in it is an orphan
//! ([`traceability_in_use`]).

use crate::storage::docs::DocMetadata;
use crate::storage::tasks::{TaskIndex, TaskLink};

/// Label that deliberately excludes a task from requirement traceability.
pub const NO_TRACE_LABEL: &str = "no-trace";

/// `TaskLink::link_type` of a link to a requirement (layer) item.
const REQUIREMENT_LINK_TYPE: &str = "requirement";

/// The one task status that excludes a task from the orphan check.
const SKIPPED_STATUS: &str = "skipped";

/// What the orphan predicate needs to know about one task and the tasks
/// nested under it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskFacts {
    pub id: String,
    pub title: String,
    pub status: String,
    pub labels: Vec<String>,
    pub has_requirement_link: bool,
    pub children: Vec<TaskFacts>,
}

impl TaskFacts {
    /// The facts of an indexed task and its whole subtree.
    pub fn from_index(task: &TaskIndex) -> Self {
        TaskFacts {
            id: task.id.clone(),
            title: task.title.clone(),
            status: task.status.clone(),
            labels: task.labels.clone(),
            has_requirement_link: has_requirement_link(&task.task_links),
            children: task.children.iter().map(TaskFacts::from_index).collect(),
        }
    }

    /// Linked to a requirement, labelled [`NO_TRACE_LABEL`], or skipped: the
    /// task itself needs nothing more.
    fn accounted_for(&self) -> bool {
        self.has_requirement_link
            || self.status == SKIPPED_STATUS
            || self.labels.iter().any(|l| l == NO_TRACE_LABEL)
    }
}

/// One orphan task, as the lint rule and the report list it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanTask {
    pub id: String,
    pub title: String,
    pub status: String,
}

/// True when `links` holds at least one `requirement`-type link. Links of any
/// other type (`doc`, `url`, `file`, `task`) do not count.
pub fn has_requirement_link(links: &[TaskLink]) -> bool {
    links.iter().any(|l| l.link_type == REQUIREMENT_LINK_TYPE)
}

/// Scope guard: the project uses requirement traceability when at least one
/// of its documents declares a `layer`.
pub fn traceability_in_use(docs: &[DocMetadata]) -> bool {
    docs.iter().any(|d| d.layer.is_some())
}

/// Every orphan among `tasks` and their descendants, in natural task-id
/// order. Always empty when `traceability_in_use` is false.
pub fn orphan_tasks(tasks: &[TaskFacts], traceability_in_use: bool) -> Vec<OrphanTask> {
    let mut out = Vec::new();
    if !traceability_in_use {
        return out;
    }
    for task in tasks {
        collect_orphans(task, &mut out);
    }
    out.sort_by(|a, b| super::lint::natural_cmp(&a.id, &b.id));
    out
}

/// Whether `task` itself — judged together with the tasks nested under it —
/// is an orphan. Always false when `traceability_in_use` is false.
pub fn is_orphan(task: &TaskFacts, traceability_in_use: bool) -> bool {
    if !traceability_in_use {
        return false;
    }
    let mut found = Vec::new();
    collect_orphans(task, &mut found);
    found.iter().any(|o| o.id == task.id)
}

/// Appends the orphans of `task`'s subtree (the task included) to `out` and
/// returns whether the subtree is free of them.
fn collect_orphans(task: &TaskFacts, out: &mut Vec<OrphanTask>) -> bool {
    let mut below_is_clean = true;
    for child in &task.children {
        // Every child is visited: each orphan below is reported, not just
        // the first one.
        below_is_clean &= collect_orphans(child, out);
    }
    let traced_through_children = !task.children.is_empty() && below_is_clean;
    let orphan = !task.accounted_for() && !traced_through_children;
    if orphan {
        out.push(OrphanTask {
            id: task.id.clone(),
            title: task.title.clone(),
            status: task.status.clone(),
        });
    }
    !orphan && below_is_clean
}

/// What to do about an orphan task: the text the lint finding and the guard
/// messages share.
pub fn suggested_action(task_id: &str) -> String {
    format!(
        "Link task {task_id} to the requirement it implements with \
         handoff_update_task(task: {{id: \"{task_id}\", requirement_ids: [...]}}) \
         (handoff_trace_propose suggests candidates), or add the label \
         \"{NO_TRACE_LABEL}\" if it is deliberately outside requirement traceability."
    )
}

#[cfg(test)]
mod tests;
