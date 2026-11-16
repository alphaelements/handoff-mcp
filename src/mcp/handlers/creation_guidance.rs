//! Requirement-link guidance on task creation (REQ-TRC-003 / REQ-TRC-004).
//!
//! A task created without a requirement link is where an orphan starts, so
//! the creating call says so right away: `handoff_update_task` answers with
//! two `Info` lines (link it or add the requirement; `handoff_trace_propose`
//! finds candidates), and `handoff_import_context` with one summary line for
//! the whole import. The lines are information, never a warning and never a
//! rejection.
//!
//! Whether a new task deserves the hint is the shared orphan predicate
//! (`crate::trace::orphan`): no hint for a `no-trace` or skipped task, and
//! none at all in a project with no layer document. `handoff_trace_tasks`
//! does not go through here: the tasks it generates come from the trace
//! items themselves.

use std::path::Path;

use anyhow::Result;

use crate::storage::orphan_tasks::{load_task_facts, read_docs_for_scope_guard};
use crate::trace::orphan::{
    is_orphan, orphan_tasks, traceability_in_use, TaskFacts, NO_TRACE_LABEL,
};

/// Code of the "this task has no requirement link" line.
const REQUIREMENT_LINK_MISSING_CODE: &str = "REQUIREMENT_LINK_MISSING";
/// Code of the "`handoff_trace_propose` finds candidates" line.
const TRACE_PROPOSE_CODE: &str = "TRACE_PROPOSE";

/// How many task ids the import summary line names before it counts the
/// rest: a large import must not turn the line into a wall of ids (the lint
/// lists every one of them).
const IMPORT_SUMMARY_MAX_IDS: usize = 20;

/// A task `handoff_update_task` has just created.
pub(super) struct CreatedTask<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub status: &'a str,
    pub labels: &'a [String],
    /// The creating call named at least one requirement id. Such a call gets
    /// no hint even when an id did not resolve: that has its own warning.
    pub names_requirement_ids: bool,
}

/// The lines to append to the response of the call that created `task`
/// (each without a leading newline); empty when there is nothing to say.
///
/// The task is already on disk when this runs, so a failure to read the
/// project's documents cannot fail the call: it is reported as one warning
/// line instead of being dropped.
pub(super) fn lines_for_created_task(handoff_dir: &Path, task: &CreatedTask<'_>) -> Vec<String> {
    match deserves_guidance(handoff_dir, task) {
        Ok(true) => vec![link_missing_line(task.id), trace_propose_line(task.id)],
        Ok(false) => Vec::new(),
        Err(e) => vec![format!(
            "Warning: requirement-link guidance check failed: {e:#}"
        )],
    }
}

fn deserves_guidance(handoff_dir: &Path, task: &CreatedTask<'_>) -> Result<bool> {
    if task.names_requirement_ids {
        return Ok(false);
    }
    // A brand-new task has no children and, naming no requirement, no link.
    let facts = TaskFacts {
        id: task.id.to_string(),
        title: task.title.to_string(),
        status: task.status.to_string(),
        labels: task.labels.to_vec(),
        has_requirement_link: false,
        children: Vec::new(),
    };
    // Cheapest first, as in the orphan guard: a task the predicate clears by
    // its own label or status never needs the documents.
    if !is_orphan(&facts, true) {
        return Ok(false);
    }
    let in_use = traceability_in_use(&read_docs_for_scope_guard(handoff_dir)?);
    Ok(is_orphan(&facts, in_use))
}

fn link_missing_line(task_id: &str) -> String {
    format!(
        "Info [{REQUIREMENT_LINK_MISSING_CODE}]: Task {task_id} has no requirement link. Link it \
         to an existing requirement with handoff_update_task(task: {{id: \"{task_id}\", \
         requirement_ids: [...]}}), or add the missing requirement first; label the task \
         \"{NO_TRACE_LABEL}\" if it is deliberately outside requirement traceability."
    )
}

fn trace_propose_line(task_id: &str) -> String {
    format!(
        "Info [{TRACE_PROPOSE_CODE}]: handoff_trace_propose(task_id: \"{task_id}\") lists \
         existing requirements that may already cover this task and drafts a new one when none \
         does."
    )
}

/// The one summary line for an import that created the top-level tasks
/// `created_top_level_ids` (and everything nested under them): which of the
/// imported tasks are orphans. `None` when none is.
pub(super) fn summary_for_import(
    handoff_dir: &Path,
    created_top_level_ids: &[String],
) -> Result<Option<String>> {
    if created_top_level_ids.is_empty() {
        return Ok(None);
    }
    let in_use = traceability_in_use(&read_docs_for_scope_guard(handoff_dir)?);
    if !in_use {
        return Ok(None);
    }
    let imported: Vec<TaskFacts> = load_task_facts(handoff_dir)?
        .into_iter()
        .filter(|t| created_top_level_ids.contains(&t.id))
        .collect();
    let orphans = orphan_tasks(&imported, in_use);
    if orphans.is_empty() {
        return Ok(None);
    }
    let shown: Vec<&str> = orphans
        .iter()
        .take(IMPORT_SUMMARY_MAX_IDS)
        .map(|o| o.id.as_str())
        .collect();
    let mut listed = shown.join(", ");
    if orphans.len() > shown.len() {
        listed.push_str(&format!(" and {} more", orphans.len() - shown.len()));
    }
    Ok(Some(format!(
        "Info [{REQUIREMENT_LINK_MISSING_CODE}]: {} imported task(s) have no requirement link: \
         {listed}. Link each to a requirement with handoff_update_task(task: {{id, \
         requirement_ids: [...]}}) (handoff_trace_propose(task_id) suggests candidates), or label \
         it \"{NO_TRACE_LABEL}\"; handoff_trace_lint(rules: [\"task_orphan\"]) lists them again.",
        orphans.len(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn created<'a>(labels: &'a [String], status: &'a str, names: bool) -> CreatedTask<'a> {
        CreatedTask {
            id: "t1",
            title: "Title",
            status,
            labels,
            names_requirement_ids: names,
        }
    }

    /// A project directory whose documents cannot be read (`docs` is a file).
    fn unreadable_project() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("docs"), "not a directory").unwrap();
        tmp
    }

    #[test]
    fn both_lines_are_info_level_single_lines_naming_the_task() {
        for line in [link_missing_line("t7"), trace_propose_line("t7")] {
            assert!(line.starts_with("Info ["), "{line}");
            assert!(line.contains("t7"), "{line}");
            assert!(!line.contains('\n'), "{line}");
            assert!(!line.contains("Warning"), "{line}");
        }
        assert!(link_missing_line("t7").contains("requirement_ids"));
        assert!(link_missing_line("t7").contains("\"no-trace\""));
        assert!(trace_propose_line("t7").contains("handoff_trace_propose(task_id: \"t7\")"));
    }

    /// A task cleared by its own facts never reads the documents: with
    /// unreadable documents it still gets a clean, empty answer.
    #[test]
    fn a_task_cleared_by_its_own_facts_never_reads_the_documents() {
        let tmp = unreadable_project();
        let no_trace = vec!["no-trace".to_string()];
        assert!(lines_for_created_task(tmp.path(), &created(&no_trace, "todo", false)).is_empty());
        assert!(lines_for_created_task(tmp.path(), &created(&[], "skipped", false)).is_empty());
        assert!(lines_for_created_task(tmp.path(), &created(&[], "todo", true)).is_empty());
    }

    #[test]
    fn a_failed_check_is_reported_as_one_warning_line_not_dropped() {
        let tmp = unreadable_project();
        let lines = lines_for_created_task(tmp.path(), &created(&[], "todo", false));
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].starts_with("Warning: requirement-link guidance check failed: "),
            "{lines:?}"
        );
    }

    #[test]
    fn a_project_without_documents_gets_no_guidance() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(lines_for_created_task(tmp.path(), &created(&[], "todo", false)).is_empty());
        assert_eq!(
            summary_for_import(tmp.path(), &["t1".to_string()]).unwrap(),
            None
        );
    }

    #[test]
    fn an_import_that_created_nothing_has_no_summary_and_reads_nothing() {
        let tmp = unreadable_project();
        assert_eq!(summary_for_import(tmp.path(), &[]).unwrap(), None);
        assert!(summary_for_import(tmp.path(), &["t1".to_string()]).is_err());
    }
}
