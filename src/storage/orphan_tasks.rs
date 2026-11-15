//! Loads what the shared orphan-task predicate (`crate::trace::orphan`)
//! judges: the project's task tree and whether the project uses requirement
//! traceability at all. Read-only.

use std::path::Path;

use anyhow::Result;

use super::docs::{read_all_docs_with_unreadable, DocMetadata};
use super::tasks::{build_task_children_index, build_task_index};
use crate::trace::orphan::{is_orphan, orphan_tasks, traceability_in_use, OrphanTask, TaskFacts};

/// Every task of the project as [`TaskFacts`] trees, top-level tasks first.
pub fn load_task_facts(handoff_dir: &Path) -> Result<Vec<TaskFacts>> {
    let (tree, _summary) = build_task_index(&handoff_dir.join("tasks"), u32::MAX)?;
    Ok(tree.iter().map(TaskFacts::from_index).collect())
}

/// The project's orphan tasks, given its already-loaded documents (the scope
/// guard reads them: no layer document, no orphan).
pub fn load_orphan_tasks_with_docs(
    handoff_dir: &Path,
    docs: &[DocMetadata],
) -> Result<Vec<OrphanTask>> {
    if !traceability_in_use(docs) {
        return Ok(Vec::new());
    }
    Ok(orphan_tasks(&load_task_facts(handoff_dir)?, true))
}

/// The project's orphan tasks, in natural task-id order. An error when the
/// scope guard cannot be decided (see [`read_docs_for_scope_guard`]).
pub fn load_orphan_tasks(handoff_dir: &Path) -> Result<Vec<OrphanTask>> {
    load_orphan_tasks_with_docs(handoff_dir, &read_docs_for_scope_guard(handoff_dir)?)
}

/// The project's documents, for a caller that is about to decide the scope
/// guard ([`traceability_in_use`]) from them.
///
/// A document whose frontmatter cannot be parsed is left out of the list, so
/// a project whose layer documents are all unreadable would look exactly
/// like one that has none. That is not something to guess past: when no
/// readable document declares a layer but some document could not be read,
/// this is an error naming those documents, and the caller reports that its
/// check could not be made (the orphan guard fails closed under `block`).
/// One readable layer document settles the question, whatever else is
/// unreadable.
pub fn read_docs_for_scope_guard(handoff_dir: &Path) -> Result<Vec<DocMetadata>> {
    let (docs, unreadable) = read_all_docs_with_unreadable(handoff_dir)?;
    if !traceability_in_use(&docs) && !unreadable.is_empty() {
        let slugs: Vec<&str> = unreadable.iter().map(|u| u.slug.as_str()).collect();
        anyhow::bail!(
            "cannot tell whether this project uses requirement traceability: {} document(s) \
             could not be read ({}). Repair them with handoff_doc_repair_frontmatter",
            unreadable.len(),
            slugs.join(", ")
        );
    }
    Ok(docs)
}

/// The shared predicate for one task, with what it needs read from disk.
///
/// `own` is the task as it is to be judged (its status, labels and whether it
/// keeps a requirement link), without children; `task_dir` is its directory,
/// `None` for a task that does not exist yet (which has no children).
/// `gains_requirement_link` answers, given the project's documents, whether a
/// pending call adds a link to it (an id only counts when it resolves); a
/// caller judging the stored task passes `|_| false`.
///
/// Asked twice, cheapest first. What the task holds by itself (its label,
/// its status, a link it keeps) needs no I/O; children, an added link and
/// the scope guard can only ever clear a task, never condemn one, so a task
/// the predicate clears on those facts alone is cleared for good. Only a
/// task that is still an orphan then pays for reading the documents and its
/// subtree — a linked task costs nothing extra.
///
/// An error when the scope guard cannot be decided (see
/// [`read_docs_for_scope_guard`]) or the subtree cannot be read.
pub fn task_is_orphan(
    handoff_dir: &Path,
    mut own: TaskFacts,
    task_dir: Option<&Path>,
    gains_requirement_link: impl FnOnce(&[DocMetadata]) -> bool,
) -> Result<bool> {
    own.children = Vec::new();
    if !is_orphan(&own, true) {
        return Ok(false);
    }

    let docs = read_docs_for_scope_guard(handoff_dir)?;
    let in_use = traceability_in_use(&docs);
    if !in_use {
        // Nothing is an orphan in such a project; skip reading the subtree.
        return Ok(false);
    }
    own.has_requirement_link = own.has_requirement_link || gains_requirement_link(&docs);
    if let Some(dir) = task_dir {
        own.children = load_child_task_facts(dir)?;
    }
    Ok(is_orphan(&own, in_use))
}

/// The tasks nested under the task in `task_dir`, as [`TaskFacts`] trees:
/// what judging that one task needs besides its own facts.
pub fn load_child_task_facts(task_dir: &Path) -> Result<Vec<TaskFacts>> {
    Ok(build_task_children_index(task_dir)?
        .iter()
        .map(TaskFacts::from_index)
        .collect())
}
