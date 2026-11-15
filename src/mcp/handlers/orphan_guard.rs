//! The orphan guard (`[trace] orphan_guard`, REQ-TRC-002).
//!
//! `handoff_update_task` and `handoff_bulk_update_tasks` consult this when a
//! task enters `review`/`done` — an existing task changing status, or a new
//! task created straight into one of them. A task the shared predicate
//! (`crate::trace::orphan`) calls an orphan gets a warning line (`warn`, the
//! default) or the whole call is rejected (`block`, unless `force`); `off`
//! does nothing. The verdict is the same predicate the `task_orphan` lint
//! rule and the report use, so a `no-trace` task, a parent traced through
//! its children, and every task of a project with no layer document pass
//! untouched.
//!
//! The judgement is made before anything is written and on the task as the
//! call would leave it: its labels and requirement links after the update.

use std::path::Path;

use anyhow::Result;

use crate::storage::config::read_config;
use crate::storage::docs::DocMetadata;
use crate::storage::orphan_tasks::task_is_orphan;
use crate::trace::orphan::{suggested_action, TaskFacts};

/// Machine-readable code of the `warn`-mode line.
const ORPHAN_TASK_CODE: &str = "ORPHAN_TASK";

const MODE_OFF: &str = "off";
const MODE_BLOCK: &str = "block";

/// A task as a call entering `review`/`done` would leave it.
pub(super) struct EnteringTask<'a> {
    pub id: &'a str,
    pub title: &'a str,
    /// The status being entered.
    pub status: &'a str,
    /// Labels after the call.
    pub labels: &'a [String],
    /// The task's directory; `None` for a task that does not exist yet
    /// (which therefore has no children).
    pub task_dir: Option<&'a Path>,
}

/// What a rejected call was doing, for the `block` message.
#[derive(Clone, Copy)]
pub(super) enum Entering {
    /// An existing task changes status.
    StatusChange,
    /// A new task is created directly in the guarded status.
    Creation,
}

/// `[trace] orphan_guard` of the project. An unreadable `config.toml` reads
/// as the default mode, like the done guard: the advisory must not vanish,
/// and must not turn stricter, because of an unrelated config problem.
pub(super) fn resolve_mode(handoff_dir: &Path) -> String {
    read_config(&handoff_dir.join("config.toml"))
        .map(|c| c.trace.orphan_guard_mode().to_string())
        .unwrap_or_else(|_| crate::storage::config::DEFAULT_ORPHAN_GUARD_MODE.to_string())
}

/// Whether `status` is one the guard watches.
pub(super) fn is_guarded_status(status: &str) -> bool {
    status == "review" || status == "done"
}

/// Judges `task` entering a guarded status.
///
/// The task's requirement links once the call has been applied come in two
/// parts: `keeps_requirement_link` (a link it already has and the call does
/// not remove) and `gains_requirement_link`, which answers, given the
/// project's documents, whether the call adds one (an id only counts when it
/// resolves).
///
/// Returns the `warn`-mode line to append to the response, `None` when the
/// guard has nothing to say, and an error when `block` rejects the call. A
/// failure to gather what the judgement needs is an error under `block`
/// (fail closed) and a warning line under `warn` (the advisory never fails a
/// call it would only have commented on).
pub(super) fn check_entering(
    handoff_dir: &Path,
    force: bool,
    kind: Entering,
    task: &EnteringTask<'_>,
    keeps_requirement_link: bool,
    gains_requirement_link: impl FnOnce(&[DocMetadata]) -> bool,
) -> Result<Option<String>> {
    let mode = resolve_mode(handoff_dir);
    let blocking = mode == MODE_BLOCK;
    if mode == MODE_OFF || (blocking && force) {
        return Ok(None);
    }
    let orphan = match judge(
        handoff_dir,
        task,
        keeps_requirement_link,
        gains_requirement_link,
    ) {
        Ok(orphan) => orphan,
        // One message with the whole cause: the tool response shows only the
        // outermost error text.
        Err(e) if blocking => anyhow::bail!(
            "orphan_guard check failed: {e:#}. [trace] orphan_guard = \"block\" rejects the \
             call while the check cannot be made; pass force: true to override, or change \
             [trace] orphan_guard."
        ),
        Err(e) => return Ok(Some(format!("Warning: orphan_guard check failed: {e:#}"))),
    };
    if !orphan {
        return Ok(None);
    }
    if blocking {
        anyhow::bail!("{}", block_message(kind, task));
    }
    Ok(Some(warn_line(task.id)))
}

/// The shared predicate, applied to the task as the call would leave it
/// (`crate::storage::orphan_tasks::task_is_orphan` reads what it needs).
fn judge(
    handoff_dir: &Path,
    task: &EnteringTask<'_>,
    keeps_requirement_link: bool,
    gains_requirement_link: impl FnOnce(&[DocMetadata]) -> bool,
) -> Result<bool> {
    task_is_orphan(
        handoff_dir,
        TaskFacts {
            id: task.id.to_string(),
            title: task.title.to_string(),
            status: task.status.to_string(),
            labels: task.labels.to_vec(),
            has_requirement_link: keeps_requirement_link,
            children: Vec::new(),
        },
        task.task_dir,
        gains_requirement_link,
    )
}

fn warn_line(task_id: &str) -> String {
    format!(
        "Warning [{ORPHAN_TASK_CODE}]: Task {task_id} has no requirement link. {} The status \
         change was applied ([trace] orphan_guard = \"warn\").",
        suggested_action(task_id)
    )
}

fn block_message(kind: Entering, task: &EnteringTask<'_>) -> String {
    let rejected = match kind {
        Entering::StatusChange => "rejects this status change".to_string(),
        Entering::Creation => format!(
            "rejects creating this task directly in status {:?}",
            task.status
        ),
    };
    format!(
        "orphan_guard: task {id} has no requirement link. {action} [trace] orphan_guard = \
         \"block\" {rejected}; pass force: true to override, or change [trace] orphan_guard.",
        id = task.id,
        action = suggested_action(task.id),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entering<'a>(id: &'a str, status: &'a str) -> EnteringTask<'a> {
        EnteringTask {
            id,
            title: "Title",
            status,
            labels: &[],
            task_dir: None,
        }
    }

    #[test]
    fn only_review_and_done_are_guarded() {
        assert!(is_guarded_status("review"));
        assert!(is_guarded_status("done"));
        for status in ["todo", "in_progress", "blocked", "skipped", ""] {
            assert!(!is_guarded_status(status), "{status}");
        }
    }

    #[test]
    fn the_mode_of_a_project_without_a_config_is_the_default() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(resolve_mode(tmp.path()), "warn");
    }

    #[test]
    fn the_mode_of_an_unparseable_config_is_the_default() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("config.toml"), "[trace\norphan_guard = ").unwrap();
        assert_eq!(resolve_mode(tmp.path()), "warn");
    }

    #[test]
    fn the_mode_is_read_from_config_with_the_fail_safe_for_unknown_values() {
        let tmp = tempfile::tempdir().unwrap();
        let write = |mode: &str| {
            std::fs::write(
                tmp.path().join("config.toml"),
                format!("[project]\nname = \"t\"\n[trace]\norphan_guard = \"{mode}\"\n"),
            )
            .unwrap()
        };
        for mode in ["off", "warn", "block"] {
            write(mode);
            assert_eq!(resolve_mode(tmp.path()), mode);
        }
        write("strict");
        assert_eq!(resolve_mode(tmp.path()), "warn");
    }

    #[test]
    fn the_block_message_names_what_was_rejected_and_how_to_get_past_it() {
        let change = block_message(Entering::StatusChange, &entering("t1", "review"));
        assert!(change.starts_with("orphan_guard: task t1 has no requirement link. "));
        assert!(change.contains("rejects this status change"));
        assert!(change.contains("force: true"));
        assert!(change.contains(&suggested_action("t1")));

        let creation = block_message(Entering::Creation, &entering("t2", "done"));
        assert!(creation.contains("rejects creating this task directly in status \"done\""));
    }

    #[test]
    fn the_warn_line_carries_the_code_the_shared_action_and_the_mode() {
        let line = warn_line("t1");
        assert!(line.starts_with("Warning [ORPHAN_TASK]: Task t1 has no requirement link. "));
        assert!(line.contains(&suggested_action("t1")));
        assert!(line.ends_with("([trace] orphan_guard = \"warn\")."));
        assert!(!line.contains('\n'));
    }

    /// A task that is accounted for by itself (its label, or a link it
    /// keeps) is cleared before any document is read: here reading the
    /// documents would fail, and `block` would turn that into an error.
    #[test]
    fn a_task_accounted_for_by_itself_is_cleared_without_reading_the_documents() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("docs"), "not a directory").unwrap();
        std::fs::write(
            tmp.path().join("config.toml"),
            "[project]\nname = \"t\"\n[trace]\norphan_guard = \"block\"\n",
        )
        .unwrap();
        let never = |_: &[DocMetadata]| -> bool { panic!("the documents must not be consulted") };

        let labels = vec!["no-trace".to_string()];
        let exempt = EnteringTask {
            labels: &labels,
            ..entering("t1", "done")
        };
        assert_eq!(
            check_entering(
                tmp.path(),
                false,
                Entering::StatusChange,
                &exempt,
                false,
                never
            )
            .unwrap(),
            None
        );
        let linked = entering("t2", "review");
        assert_eq!(
            check_entering(
                tmp.path(),
                false,
                Entering::StatusChange,
                &linked,
                true,
                never
            )
            .unwrap(),
            None
        );
        // Neither: the documents are needed, and here they cannot be read.
        let bare = entering("t3", "review");
        assert!(check_entering(
            tmp.path(),
            false,
            Entering::StatusChange,
            &bare,
            false,
            |_| false
        )
        .is_err());
    }

    /// A project whose documents cannot be judged (here: `docs` is a file,
    /// not a directory) fails closed under `block` and only comments under
    /// `warn`.
    #[test]
    fn a_failed_judgement_blocks_under_block_and_warns_under_warn() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("docs"), "not a directory").unwrap();
        let task = entering("t1", "done");

        std::fs::write(
            tmp.path().join("config.toml"),
            "[project]\nname = \"t\"\n[trace]\norphan_guard = \"block\"\n",
        )
        .unwrap();
        let err = check_entering(
            tmp.path(),
            false,
            Entering::StatusChange,
            &task,
            false,
            |_| false,
        )
        .unwrap_err();
        // The outermost message alone (what a tool response shows) carries
        // the cause and the way out.
        let shown = err.to_string();
        assert!(shown.starts_with("orphan_guard check failed: "), "{shown}");
        assert!(shown.contains("docs"), "{shown}");
        assert!(shown.contains("force: true"), "{shown}");
        // force skips the check altogether.
        assert_eq!(
            check_entering(
                tmp.path(),
                true,
                Entering::StatusChange,
                &task,
                false,
                |_| false
            )
            .unwrap(),
            None
        );

        std::fs::write(
            tmp.path().join("config.toml"),
            "[project]\nname = \"t\"\n[trace]\norphan_guard = \"warn\"\n",
        )
        .unwrap();
        let line = check_entering(
            tmp.path(),
            false,
            Entering::StatusChange,
            &task,
            false,
            |_| false,
        )
        .unwrap()
        .expect("a warning line");
        assert!(
            line.starts_with("Warning: orphan_guard check failed: "),
            "{line}"
        );

        std::fs::write(
            tmp.path().join("config.toml"),
            "[project]\nname = \"t\"\n[trace]\norphan_guard = \"off\"\n",
        )
        .unwrap();
        assert_eq!(
            check_entering(
                tmp.path(),
                false,
                Entering::StatusChange,
                &task,
                false,
                |_| false
            )
            .unwrap(),
            None
        );
    }
}
