//! The recommended next step for a project (SPEC-ONB-011): which of nine
//! states the project is in, and the one operation that moves it on.
//!
//! This module decides; it reads nothing itself. What it needs to know comes
//! through [`Project`], asked for in the order the states are checked and only
//! as far as the decision needs: a project that is not ready is never asked
//! for its tasks, and one with a task in review never for its documents. The
//! `handoff_next_step` tool (`crate::mcp::handlers::next_step`) supplies the
//! answers from the project's files.
//!
//! It is not `handoff_trace_next`: that one walks the verification items of
//! the trace graph, this one looks at the project as a whole.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use anyhow::Result;
use serde::Serialize;

use crate::storage::config::{MILESTONE_CANCELLED_STATUS, MILESTONE_RELEASED_STATUS};
use crate::storage::tasks::is_terminal_status;

/// Task statuses the states are decided from.
const STATUS_REVIEW: &str = "review";
const STATUS_IN_PROGRESS: &str = "in_progress";
const STATUS_TODO: &str = "todo";
const STATUS_BLOCKED: &str = "blocked";

/// The state of a project, in the order the states are checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// The project has no `.handoff/`.
    NotInitialized,
    /// The readiness diagnosis (`handoff_doctor`, without results) fails.
    NotReady,
    /// A task waits for review.
    ReviewPending,
    /// A task is in progress, or a session is active.
    InProgress,
    /// Neither a finalized brainstorm report nor a requirement item exists.
    NoBrainstorm,
    /// The requirement layer has no item.
    NoRequirements,
    /// A `todo` task has no unfinished dependency.
    ReadyTasks,
    /// No task is unfinished and a milestone is not released yet.
    ReleaseReady,
    /// None of the above.
    AllDone,
}

/// The one operation recommended for the state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Action {
    /// What to do, in a few words.
    pub title: String,
    /// How: a slash command, a tool call, or a sentence to act on.
    pub command: String,
    /// Why this is the next step.
    pub reason: String,
}

/// The answer of `handoff_next_step`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NextStep {
    pub state: State,
    pub action: Action,
    pub facts: Facts,
}

/// The counts the decision looked at. A group the decision did not need is
/// left out: it was not read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Facts {
    pub initialized: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doctor: Option<DoctorFacts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tasks: Option<TaskCounts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_sessions: Option<usize>,
    /// Finalized brainstorm reports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brainstorm_reports: Option<usize>,
    /// Items of the requirement layer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requirement_items: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub milestones: Option<MilestoneCounts>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorFacts {
    pub ready: bool,
    /// Ids of the checks that failed, in report order.
    pub failed: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskCounts {
    pub total: usize,
    /// Tasks per status; a status no task has is left out.
    pub by_status: BTreeMap<String, usize>,
    /// Tasks that are neither done nor skipped.
    pub unfinished: usize,
    /// `todo` tasks whose dependencies are all done or skipped.
    pub ready: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MilestoneCounts {
    pub total: usize,
    /// Milestones that are neither released nor cancelled, in display order.
    pub unreleased: Vec<String>,
}

/// A readiness check that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedCheck {
    pub id: String,
    pub detail: String,
    /// What the diagnosis says to do about it.
    pub next_step: String,
}

/// The outcome of the readiness diagnosis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readiness {
    pub ready: bool,
    pub failed: Vec<FailedCheck>,
}

/// A task of the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskFact {
    pub id: String,
    pub title: String,
    pub status: String,
    pub dependencies: Vec<String>,
}

/// An active session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFact {
    pub id: String,
    pub summary: String,
}

/// The work that is tracked: every task, and the sessions that are active.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Work {
    pub tasks: Vec<TaskFact>,
    pub active_sessions: Vec<SessionFact>,
}

/// What was planned so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Planning {
    pub brainstorm_reports: usize,
    pub requirement_items: usize,
}

/// A milestone and its lifecycle status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MilestoneFact {
    pub name: String,
    pub status: String,
}

/// What the decision can ask about a project. Each method is called at most
/// once, and only when the states before it did not apply.
pub trait Project {
    /// A name for the project, used to suggest `handoff_init`.
    fn name(&self) -> String;
    /// Whether the project has a `.handoff/` directory.
    fn initialized(&self) -> bool;
    fn readiness(&self) -> Result<Readiness>;
    fn work(&self) -> Result<Work>;
    fn planning(&self) -> Result<Planning>;
    /// Every milestone, in display order.
    fn milestones(&self) -> Result<Vec<MilestoneFact>>;
}

/// Decides the state of `project` and the one operation to recommend. Fails
/// only when something the decision needs cannot be read.
pub fn next_step(project: &dyn Project) -> Result<NextStep> {
    let mut facts = Facts::default();

    // 1. not_initialized
    if !project.initialized() {
        let name = project.name();
        return Ok(NextStep {
            state: State::NotInitialized,
            action: Action {
                title: "Initialize handoff for this project".to_string(),
                command: format!("handoff_init(project_name: {})", quoted(&name)),
                reason: "The project has no .handoff/ directory, so nothing is tracked yet. \
                         For a first project, add preset: \"beginner\"."
                    .to_string(),
            },
            facts,
        });
    }
    facts.initialized = true;

    // 2. not_ready
    let readiness = project.readiness()?;
    facts.doctor = Some(DoctorFacts {
        ready: readiness.ready,
        failed: readiness
            .failed
            .iter()
            .map(|check| check.id.clone())
            .collect(),
    });
    if !readiness.ready {
        return Ok(NextStep {
            state: State::NotReady,
            action: not_ready_action(&readiness),
            facts,
        });
    }

    // 3. review_pending / 4. in_progress
    let mut work = project.work()?;
    work.tasks
        .sort_by(|a, b| natural_order(&a.id, &b.id).then_with(|| a.id.cmp(&b.id)));
    let ready: Vec<&TaskFact> = ready_tasks(&work.tasks);
    let unfinished = work
        .tasks
        .iter()
        .filter(|task| !is_terminal_status(&task.status))
        .count();
    let mut by_status = BTreeMap::new();
    for task in &work.tasks {
        *by_status.entry(task.status.clone()).or_insert(0) += 1;
    }
    facts.tasks = Some(TaskCounts {
        total: work.tasks.len(),
        by_status: by_status.clone(),
        unfinished,
        ready: ready.len(),
    });
    facts.active_sessions = Some(work.active_sessions.len());
    let with_status = |status: &str| -> Vec<&TaskFact> {
        work.tasks
            .iter()
            .filter(|task| task.status == status)
            .collect()
    };

    let in_review = with_status(STATUS_REVIEW);
    if let Some(first) = in_review.first() {
        return Ok(NextStep {
            state: State::ReviewPending,
            action: Action {
                title: format!("Confirm task {}, which waits for review", first.id),
                command: format!("handoff_get_task(task_id: {})", quoted(&first.id)),
                reason: format!(
                    "{} task(s) are in review; the first is {} ({}). Go through its \
                     done_criteria with the user, then set it to done, or back to in_progress \
                     if something is missing.",
                    in_review.len(),
                    first.id,
                    first.title
                ),
            },
            facts,
        });
    }

    let in_progress = with_status(STATUS_IN_PROGRESS);
    if let Some(first) = in_progress.first() {
        return Ok(NextStep {
            state: State::InProgress,
            action: Action {
                title: format!("Resume task {}", first.id),
                command: format!("handoff_get_task(task_id: {})", quoted(&first.id)),
                reason: format!(
                    "{} task(s) are in progress; the first is {} ({}). Finish what was \
                     started before taking on something new.",
                    in_progress.len(),
                    first.id,
                    first.title
                ),
            },
            facts,
        });
    }
    if let Some(session) = work.active_sessions.first() {
        return Ok(NextStep {
            state: State::InProgress,
            action: Action {
                title: format!("Resume the active session {}", session.id),
                command: format!("handoff_get_session(session_id: {})", quoted(&session.id)),
                reason: format!(
                    "{} session(s) are active and no task is in progress; the first is {} \
                     ({}). Continue it, or close it with handoff_save_context when its work is \
                     finished.",
                    work.active_sessions.len(),
                    session.id,
                    one_line(&session.summary)
                ),
            },
            facts,
        });
    }

    // 5. no_brainstorm / 6. no_requirements
    let planning = project.planning()?;
    facts.brainstorm_reports = Some(planning.brainstorm_reports);
    facts.requirement_items = Some(planning.requirement_items);
    if planning.requirement_items == 0 {
        if planning.brainstorm_reports == 0 {
            return Ok(NextStep {
                state: State::NoBrainstorm,
                action: Action {
                    title: "Decide what to build".to_string(),
                    command: "/brainstorm-loop".to_string(),
                    reason: "The project has no finalized brainstorm report and no requirement \
                             item yet. A brainstorm settles what to build and for whom."
                        .to_string(),
                },
                facts,
            });
        }
        return Ok(NextStep {
            state: State::NoRequirements,
            action: Action {
                title: "Turn the brainstorm into requirements".to_string(),
                command: "/requirement-loop".to_string(),
                reason: format!(
                    "{} finalized brainstorm report(s) exist, but the requirement layer has no \
                     item yet.",
                    planning.brainstorm_reports
                ),
            },
            facts,
        });
    }

    // 7. ready_tasks
    if let Some(first) = ready.first() {
        return Ok(NextStep {
            state: State::ReadyTasks,
            action: Action {
                title: "Work on the tasks that are ready".to_string(),
                command: "/session-loop".to_string(),
                reason: format!(
                    "{} todo task(s) have no unfinished dependency; the first is {} ({}).",
                    ready.len(),
                    first.id,
                    first.title
                ),
            },
            facts,
        });
    }

    // 8. release_ready
    let milestones = project.milestones()?;
    let unreleased: Vec<&MilestoneFact> = milestones
        .iter()
        .filter(|milestone| {
            milestone.status != MILESTONE_RELEASED_STATUS
                && milestone.status != MILESTONE_CANCELLED_STATUS
        })
        .collect();
    facts.milestones = Some(MilestoneCounts {
        total: milestones.len(),
        unreleased: unreleased
            .iter()
            .map(|milestone| milestone.name.clone())
            .collect(),
    });
    if unfinished == 0 {
        if let Some(milestone) = unreleased.first() {
            return Ok(NextStep {
                state: State::ReleaseReady,
                action: Action {
                    title: format!(
                        "Check whether milestone '{}' can be released",
                        milestone.name
                    ),
                    command: format!(
                        "handoff_release_check(milestone: {})",
                        quoted(&milestone.name)
                    ),
                    reason: format!(
                        "No task is unfinished ({} task(s), all done or skipped) and milestone \
                         '{}' is not released yet (status: {}).",
                        work.tasks.len(),
                        milestone.name,
                        milestone.status
                    ),
                },
                facts,
            });
        }
    }

    // 9. all_done: none of the above.
    let action = if unfinished > 0 {
        // Unfinished work exists, but none of it can start.
        let blocked = by_status.get(STATUS_BLOCKED).copied().unwrap_or(0);
        let waiting = by_status.get(STATUS_TODO).copied().unwrap_or(0);
        Action {
            title: "Clear what keeps the remaining tasks from starting".to_string(),
            command: "handoff_list_tasks()".to_string(),
            reason: format!(
                "{unfinished} task(s) are unfinished but none is ready: {blocked} blocked, \
                 {waiting} todo waiting for a dependency that is unfinished or does not exist. \
                 Resolve the blockers or the dependencies with the user."
            ),
        }
    } else {
        Action {
            title: "Plan what comes next".to_string(),
            command: "/brainstorm-loop".to_string(),
            reason: format!(
                "Every task is done or skipped ({} task(s)) and no milestone waits to be \
                 released. Start the next round of planning.",
                work.tasks.len()
            ),
        }
    };
    Ok(NextStep {
        state: State::AllDone,
        action,
        facts,
    })
}

/// The action for a project that is not ready: what the diagnosis says about
/// the first check that failed.
fn not_ready_action(readiness: &Readiness) -> Action {
    let Some(first) = readiness.failed.first() else {
        // `ready` is false without a failed check only for a diagnosis that
        // was asked to judge a run, which this one is not; still answer.
        return Action {
            title: "Run the readiness check".to_string(),
            command: "handoff_doctor()".to_string(),
            reason: "handoff_doctor reports that the project is not ready.".to_string(),
        };
    };
    let ids: Vec<&str> = readiness
        .failed
        .iter()
        .map(|check| check.id.as_str())
        .collect();
    Action {
        title: format!("Fix the readiness check `{}`", first.id),
        command: if first.next_step.trim().is_empty() {
            "handoff_doctor()".to_string()
        } else {
            first.next_step.clone()
        },
        reason: format!(
            "handoff_doctor reports that the project is not ready: {} check(s) failed ({}). \
             `{}`: {}",
            ids.len(),
            ids.join(", "),
            first.id,
            one_line(&first.detail)
        ),
    }
}

/// The `todo` tasks whose dependencies are all done or skipped. A dependency
/// that names no task is not resolved: nothing can finish it.
fn ready_tasks(tasks: &[TaskFact]) -> Vec<&TaskFact> {
    let finished = |id: &str| {
        tasks
            .iter()
            .any(|task| task.id == id && is_terminal_status(&task.status))
    };
    tasks
        .iter()
        .filter(|task| task.status == STATUS_TODO)
        .filter(|task| task.dependencies.iter().all(|id| finished(id)))
        .collect()
}

/// `text` as a quoted argument of a tool call. A name is free text, so a
/// quote or a backslash in it is escaped.
fn quoted(text: &str) -> String {
    serde_json::Value::String(text.to_string()).to_string()
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Orders ids the way a person reads them: runs of digits compare as
/// numbers, so `t2` comes before `t10` and `t1.2` before `t1.10`.
fn natural_order(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let (x, y) = (take_number(&mut a), take_number(&mut b));
                let (x, y) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                let ordering = x.len().cmp(&y.len()).then_with(|| x.cmp(y));
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
                a.next();
                b.next();
            }
        }
    }
}

/// Takes the run of digits `chars` is at.
fn take_number(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut digits = String::new();
    while let Some(digit) = chars.peek().copied().filter(char::is_ascii_digit) {
        digits.push(digit);
        chars.next();
    }
    digits
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A project described in memory. It records what was asked of it.
    #[derive(Default)]
    struct Fake {
        initialized: bool,
        failed: Vec<FailedCheck>,
        work: Work,
        planning: Planning,
        milestones: Vec<MilestoneFact>,
        /// Refuse to answer for these groups, as an unreadable file would.
        unreadable: Vec<&'static str>,
        asked: RefCell<Vec<&'static str>>,
    }

    impl Fake {
        /// Initialized and ready, with nothing else.
        fn ready() -> Self {
            Self {
                initialized: true,
                ..Self::default()
            }
        }

        /// Ready, with requirements and a finalized brainstorm on file.
        fn planned() -> Self {
            Self {
                planning: Planning {
                    brainstorm_reports: 1,
                    requirement_items: 3,
                },
                ..Self::ready()
            }
        }

        fn with_tasks(mut self, tasks: &[(&str, &str)]) -> Self {
            self.work.tasks = tasks.iter().map(|(id, status)| task(id, status)).collect();
            self
        }

        fn ask(&self, group: &'static str) -> Result<()> {
            self.asked.borrow_mut().push(group);
            if self.unreadable.contains(&group) {
                anyhow::bail!("cannot read the {group}");
            }
            Ok(())
        }

        fn step(&self) -> NextStep {
            next_step(self).expect("a next step")
        }

        fn asked(&self) -> Vec<&'static str> {
            self.asked.borrow().clone()
        }
    }

    impl Project for Fake {
        fn name(&self) -> String {
            "demo".to_string()
        }

        fn initialized(&self) -> bool {
            self.initialized
        }

        fn readiness(&self) -> Result<Readiness> {
            self.ask("readiness")?;
            Ok(Readiness {
                ready: self.failed.is_empty(),
                failed: self.failed.clone(),
            })
        }

        fn work(&self) -> Result<Work> {
            self.ask("work")?;
            Ok(self.work.clone())
        }

        fn planning(&self) -> Result<Planning> {
            self.ask("planning")?;
            Ok(self.planning)
        }

        fn milestones(&self) -> Result<Vec<MilestoneFact>> {
            self.ask("milestones")?;
            Ok(self.milestones.clone())
        }
    }

    fn task(id: &str, status: &str) -> TaskFact {
        TaskFact {
            id: id.to_string(),
            title: format!("Title of {id}"),
            status: status.to_string(),
            dependencies: Vec::new(),
        }
    }

    fn milestone(name: &str, status: &str) -> MilestoneFact {
        MilestoneFact {
            name: name.to_string(),
            status: status.to_string(),
        }
    }

    fn failed(id: &str) -> FailedCheck {
        FailedCheck {
            id: id.to_string(),
            detail: format!("{id} is\nmissing"),
            next_step: format!("Run the fix for {id}."),
        }
    }

    fn session(id: &str) -> SessionFact {
        SessionFact {
            id: id.to_string(),
            summary: "Working on\nlogin".to_string(),
        }
    }

    /// One project per state, built to be in that state and no earlier one.
    fn one_project_per_state() -> Vec<(State, Fake)> {
        vec![
            (State::NotInitialized, Fake::default()),
            (
                State::NotReady,
                Fake {
                    failed: vec![failed("claude_md_sections"), failed("commands_defined")],
                    ..Fake::ready()
                },
            ),
            (
                State::ReviewPending,
                Fake::ready().with_tasks(&[("t1", "done"), ("t2", "review")]),
            ),
            (
                State::InProgress,
                Fake::ready().with_tasks(&[("t1", "todo"), ("t2", "in_progress")]),
            ),
            (State::NoBrainstorm, Fake::ready()),
            (
                State::NoRequirements,
                Fake {
                    planning: Planning {
                        brainstorm_reports: 1,
                        requirement_items: 0,
                    },
                    ..Fake::ready()
                },
            ),
            (
                State::ReadyTasks,
                Fake::planned().with_tasks(&[("t1", "done"), ("t2", "todo")]),
            ),
            (
                State::ReleaseReady,
                Fake {
                    milestones: vec![milestone("v1", "active")],
                    ..Fake::planned().with_tasks(&[("t1", "done"), ("t2", "skipped")])
                },
            ),
            (
                State::AllDone,
                Fake {
                    milestones: vec![milestone("v1", "released")],
                    ..Fake::planned().with_tasks(&[("t1", "done")])
                },
            ),
        ]
    }

    /// Is `command` a slash command, a tool call or a sentence to act on?
    fn command_form(command: &str) -> &'static str {
        if command.starts_with('/') && !command.contains(' ') {
            "slash command"
        } else if command.starts_with("handoff_") && command.ends_with(')') {
            "tool call"
        } else if command.contains(' ') && command.ends_with('.') {
            "request"
        } else {
            "none"
        }
    }

    // SPEC-ONB-011 AC1, AC2
    #[test]
    fn each_of_the_nine_states_is_recognized_and_has_one_action() {
        let projects = one_project_per_state();
        assert_eq!(projects.len(), 9);
        let mut names = Vec::new();
        for (state, project) in &projects {
            let step = project.step();
            assert_eq!(step.state, *state);
            let action = &step.action;
            for (field, value) in [
                ("title", &action.title),
                ("command", &action.command),
                ("reason", &action.reason),
            ] {
                assert!(!value.trim().is_empty(), "{state:?}: empty {field}");
            }
            assert_ne!(
                command_form(&action.command),
                "none",
                "{state:?}: `{}` is not a slash command, a tool call or a request",
                action.command
            );
            assert!(
                !action.title.contains('\n') && !action.reason.contains('\n'),
                "{state:?}: {action:?}"
            );
            names.push(serde_json::to_value(step.state).unwrap());
        }
        assert_eq!(
            names,
            [
                "not_initialized",
                "not_ready",
                "review_pending",
                "in_progress",
                "no_brainstorm",
                "no_requirements",
                "ready_tasks",
                "release_ready",
                "all_done"
            ]
        );
    }

    #[test]
    fn the_command_of_each_state_is_the_documented_operation() {
        let commands: Vec<String> = one_project_per_state()
            .iter()
            .map(|(_, project)| project.step().action.command)
            .collect();
        assert_eq!(
            commands,
            [
                "handoff_init(project_name: \"demo\")",
                "Run the fix for claude_md_sections.",
                "handoff_get_task(task_id: \"t2\")",
                "handoff_get_task(task_id: \"t2\")",
                "/brainstorm-loop",
                "/requirement-loop",
                "/session-loop",
                "handoff_release_check(milestone: \"v1\")",
                "/brainstorm-loop",
            ]
        );
    }

    #[test]
    fn the_answer_serializes_to_state_action_and_facts() {
        let project = Fake {
            milestones: vec![milestone("v1", "active"), milestone("v0", "released")],
            ..Fake::planned().with_tasks(&[("t1", "done"), ("t2", "skipped")])
        };
        let json = serde_json::to_value(project.step()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "state": "release_ready",
                "action": {
                    "title": "Check whether milestone 'v1' can be released",
                    "command": "handoff_release_check(milestone: \"v1\")",
                    "reason": "No task is unfinished (2 task(s), all done or skipped) and \
                               milestone 'v1' is not released yet (status: active).",
                },
                "facts": {
                    "initialized": true,
                    "doctor": { "ready": true, "failed": [] },
                    "tasks": {
                        "total": 2,
                        "by_status": { "done": 1, "skipped": 1 },
                        "unfinished": 0,
                        "ready": 0,
                    },
                    "active_sessions": 0,
                    "brainstorm_reports": 1,
                    "requirement_items": 3,
                    "milestones": { "total": 2, "unreleased": ["v1"] },
                },
            })
        );
        assert_eq!(
            json.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["state", "action", "facts"]
        );
        assert_eq!(
            json["action"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            ["title", "command", "reason"]
        );
    }

    /// The facts are the counts the decision looked at, and nothing is read
    /// beyond the state that applied.
    #[test]
    fn only_what_the_decision_needs_is_read_and_reported() {
        let expected: [(State, &[&str], &[&str]); 9] = [
            (State::NotInitialized, &[], &["initialized"]),
            (State::NotReady, &["readiness"], &["initialized", "doctor"]),
            (
                State::ReviewPending,
                &["readiness", "work"],
                &["initialized", "doctor", "tasks", "active_sessions"],
            ),
            (
                State::InProgress,
                &["readiness", "work"],
                &["initialized", "doctor", "tasks", "active_sessions"],
            ),
            (
                State::NoBrainstorm,
                &["readiness", "work", "planning"],
                &[
                    "initialized",
                    "doctor",
                    "tasks",
                    "active_sessions",
                    "brainstorm_reports",
                    "requirement_items",
                ],
            ),
            (
                State::NoRequirements,
                &["readiness", "work", "planning"],
                &[
                    "initialized",
                    "doctor",
                    "tasks",
                    "active_sessions",
                    "brainstorm_reports",
                    "requirement_items",
                ],
            ),
            (
                State::ReadyTasks,
                &["readiness", "work", "planning"],
                &[
                    "initialized",
                    "doctor",
                    "tasks",
                    "active_sessions",
                    "brainstorm_reports",
                    "requirement_items",
                ],
            ),
            (
                State::ReleaseReady,
                &["readiness", "work", "planning", "milestones"],
                &[
                    "initialized",
                    "doctor",
                    "tasks",
                    "active_sessions",
                    "brainstorm_reports",
                    "requirement_items",
                    "milestones",
                ],
            ),
            (
                State::AllDone,
                &["readiness", "work", "planning", "milestones"],
                &[
                    "initialized",
                    "doctor",
                    "tasks",
                    "active_sessions",
                    "brainstorm_reports",
                    "requirement_items",
                    "milestones",
                ],
            ),
        ];
        for ((state, project), (expected_state, asked, fact_keys)) in
            one_project_per_state().into_iter().zip(expected)
        {
            assert_eq!(state, expected_state);
            let step = project.step();
            assert_eq!(project.asked(), asked, "{state:?}");
            let facts = serde_json::to_value(&step.facts).unwrap();
            assert_eq!(
                facts.as_object().unwrap().keys().collect::<Vec<_>>(),
                fact_keys,
                "{state:?}"
            );
        }
    }

    /// What is not needed is not read, so it cannot fail the answer either.
    #[test]
    fn an_unreadable_later_group_does_not_break_an_earlier_state() {
        let mut project = Fake::ready().with_tasks(&[("t1", "review")]);
        project.unreadable = vec!["planning", "milestones"];
        assert_eq!(project.step().state, State::ReviewPending);

        // When it is needed, the failure is reported, not guessed around.
        let mut project = Fake::ready();
        project.unreadable = vec!["planning"];
        let err = next_step(&project).unwrap_err().to_string();
        assert!(err.contains("cannot read the planning"), "{err}");
    }

    // --- the order of the states ---

    #[test]
    fn an_earlier_state_wins_over_every_later_one() {
        // Everything at once: not ready, a task in review, one in progress,
        // an active session, nothing planned, a ready task, a milestone.
        let mut project = Fake {
            failed: vec![failed("claude_md_todo")],
            milestones: vec![milestone("v1", "planning")],
            ..Fake::ready().with_tasks(&[("t1", "review"), ("t2", "in_progress"), ("t3", "todo")])
        };
        project.work.active_sessions = vec![session("s-1")];
        assert_eq!(project.step().state, State::NotReady);

        project.failed.clear();
        assert_eq!(project.step().state, State::ReviewPending);

        project.work.tasks[0].status = "done".to_string();
        let step = project.step();
        assert_eq!(step.state, State::InProgress);
        assert_eq!(step.action.command, "handoff_get_task(task_id: \"t2\")");

        // Without a task in progress, the active session keeps the state.
        project.work.tasks[1].status = "done".to_string();
        let step = project.step();
        assert_eq!(step.state, State::InProgress);
        assert_eq!(
            step.action.command,
            "handoff_get_session(session_id: \"s-1\")"
        );
        assert!(
            step.action.reason.contains("(Working on login)"),
            "{}",
            step.action.reason
        );

        // A ready task does not skip the planning states.
        project.work.active_sessions.clear();
        assert_eq!(project.step().state, State::NoBrainstorm);
        project.planning.brainstorm_reports = 2;
        assert_eq!(project.step().state, State::NoRequirements);
        project.planning.requirement_items = 1;
        assert_eq!(project.step().state, State::ReadyTasks);

        project.work.tasks[2].status = "skipped".to_string();
        assert_eq!(project.step().state, State::ReleaseReady);
        project.milestones[0].status = "released".to_string();
        assert_eq!(project.step().state, State::AllDone);
    }

    /// Requirement items without a brainstorm report are enough to move past
    /// both planning states.
    #[test]
    fn requirements_written_without_a_brainstorm_count_as_planned() {
        let project = Fake {
            planning: Planning {
                brainstorm_reports: 0,
                requirement_items: 1,
            },
            ..Fake::ready().with_tasks(&[("t1", "todo")])
        };
        assert_eq!(project.step().state, State::ReadyTasks);
    }

    // --- not_ready ---

    #[test]
    fn a_project_that_is_not_ready_is_told_what_the_first_failed_check_says() {
        let project = Fake {
            failed: vec![failed("claude_md_sections"), failed("secrets_untracked")],
            ..Fake::ready()
        };
        let step = project.step();
        assert_eq!(
            step.action,
            Action {
                title: "Fix the readiness check `claude_md_sections`".to_string(),
                command: "Run the fix for claude_md_sections.".to_string(),
                reason: "handoff_doctor reports that the project is not ready: 2 check(s) \
                         failed (claude_md_sections, secrets_untracked). `claude_md_sections`: \
                         claude_md_sections is missing"
                    .to_string(),
            }
        );
        assert_eq!(
            step.facts.doctor,
            Some(DoctorFacts {
                ready: false,
                failed: vec![
                    "claude_md_sections".to_string(),
                    "secrets_untracked".to_string()
                ],
            })
        );
    }

    #[test]
    fn a_failed_check_without_advice_points_at_the_diagnosis() {
        let mut check = failed("commands_defined");
        check.next_step = "  ".to_string();
        let project = Fake {
            failed: vec![check],
            ..Fake::ready()
        };
        assert_eq!(project.step().action.command, "handoff_doctor()");
    }

    // --- tasks ---

    #[test]
    fn the_first_task_is_chosen_in_reading_order_whatever_order_they_arrive_in() {
        let project =
            Fake::ready().with_tasks(&[("t10", "review"), ("t9.10", "review"), ("t9.2", "review")]);
        let step = project.step();
        assert_eq!(step.action.command, "handoff_get_task(task_id: \"t9.2\")");
        assert!(
            step.action.reason.starts_with("3 task(s) are in review"),
            "{}",
            step.action.reason
        );
    }

    #[test]
    fn ids_are_ordered_the_way_a_person_reads_them() {
        let mut ids = vec![
            "t10", "t2", "t1.10", "t1.2", "t1", "t-onb.4", "t-onb.10", "t02", "a", "t",
        ];
        ids.sort_by(|a, b| natural_order(a, b).then_with(|| a.cmp(b)));
        assert_eq!(
            ids,
            ["a", "t", "t-onb.4", "t-onb.10", "t1", "t1.2", "t1.10", "t02", "t2", "t10"]
        );
        assert_eq!(natural_order("t1", "t1"), Ordering::Equal);
        assert_eq!(natural_order("t007", "t7"), Ordering::Equal);
    }

    #[test]
    fn a_todo_task_is_ready_only_when_every_dependency_is_finished() {
        let depends_on = |id: &str, status: &str, dependencies: &[&str]| TaskFact {
            dependencies: dependencies.iter().map(|d| d.to_string()).collect(),
            ..task(id, status)
        };
        let mut project = Fake::planned();
        project.work.tasks = vec![
            task("t1", "done"),
            task("t2", "skipped"),
            task("t3", "blocked"),
            // Waits for a blocked task.
            depends_on("t4", "todo", &["t1", "t3"]),
            // Waits for a task that does not exist.
            depends_on("t5", "todo", &["t99"]),
            // Both dependencies are finished.
            depends_on("t6", "todo", &["t1", "t2"]),
            // Not todo: never "ready", whatever its dependencies.
            depends_on("t7", "blocked", &["t1"]),
        ];
        let step = project.step();
        assert_eq!(step.state, State::ReadyTasks);
        assert!(
            step.action
                .reason
                .starts_with("1 todo task(s) have no unfinished dependency; the first is t6 "),
            "{}",
            step.action.reason
        );
        let counts = step.facts.tasks.unwrap();
        assert_eq!(counts.total, 7);
        assert_eq!(counts.ready, 1);
        assert_eq!(counts.unfinished, 5);
        assert_eq!(
            counts.by_status,
            BTreeMap::from([
                ("blocked".to_string(), 2),
                ("done".to_string(), 1),
                ("skipped".to_string(), 1),
                ("todo".to_string(), 3),
            ])
        );
    }

    // --- release_ready and all_done ---

    #[test]
    fn the_first_milestone_that_can_still_be_released_is_offered() {
        let project = Fake {
            milestones: vec![
                milestone("v0", "released"),
                milestone("dropped", "cancelled"),
                milestone("v1", "frozen"),
                milestone("v2", "planning"),
            ],
            ..Fake::planned().with_tasks(&[("t1", "done")])
        };
        let step = project.step();
        assert_eq!(step.state, State::ReleaseReady);
        assert_eq!(
            step.action.command,
            "handoff_release_check(milestone: \"v1\")"
        );
        assert_eq!(
            step.facts.milestones,
            Some(MilestoneCounts {
                total: 4,
                unreleased: vec!["v1".to_string(), "v2".to_string()],
            })
        );
    }

    #[test]
    fn released_and_cancelled_milestones_leave_nothing_to_release() {
        for milestones in [
            vec![],
            vec![milestone("v0", "released")],
            vec![milestone("v0", "released"), milestone("x", "cancelled")],
        ] {
            let project = Fake {
                milestones,
                ..Fake::planned().with_tasks(&[("t1", "done")])
            };
            let step = project.step();
            assert_eq!(step.state, State::AllDone);
            assert_eq!(step.action.command, "/brainstorm-loop");
            assert!(
                step.action.reason.contains("Every task is done or skipped"),
                "{}",
                step.action.reason
            );
        }
    }

    /// A project without any task has no unfinished one.
    #[test]
    fn a_planned_project_without_tasks_is_offered_its_unreleased_milestone() {
        let project = Fake {
            milestones: vec![milestone("v1", "planning")],
            ..Fake::planned()
        };
        assert_eq!(project.step().state, State::ReleaseReady);
    }

    /// Unfinished tasks that cannot start are not "all done": the state name
    /// is the catch-all, the action says what is actually the case.
    #[test]
    fn unfinished_tasks_that_cannot_start_are_named_instead_of_called_done() {
        let mut project = Fake {
            milestones: vec![milestone("v1", "active")],
            ..Fake::planned().with_tasks(&[("t1", "done"), ("t2", "blocked"), ("t3", "todo")])
        };
        project.work.tasks[2].dependencies = vec!["t2".to_string()];
        let step = project.step();
        assert_eq!(
            step.state,
            State::AllDone,
            "an unfinished task keeps the milestone from being offered for release"
        );
        assert_eq!(step.action.command, "handoff_list_tasks()");
        assert_eq!(
            step.action.reason,
            "2 task(s) are unfinished but none is ready: 1 blocked, 1 todo waiting for a \
             dependency that is unfinished or does not exist. Resolve the blockers or the \
             dependencies with the user."
        );
        assert!(
            !step.action.reason.contains("Every task is done"),
            "{}",
            step.action.reason
        );
    }

    /// Names are free text; a tool call built from one stays a tool call.
    #[test]
    fn names_with_quotes_are_escaped_in_the_command() {
        let project = Fake {
            milestones: vec![milestone("v1 \"gold\"", "active")],
            ..Fake::planned().with_tasks(&[("t1", "done")])
        };
        assert_eq!(
            project.step().action.command,
            "handoff_release_check(milestone: \"v1 \\\"gold\\\"\")"
        );
    }

    // SPEC-ONB-011 AC3 (the decision itself is deterministic)
    #[test]
    fn the_same_project_gets_the_same_answer_every_time() {
        for (state, project) in one_project_per_state() {
            assert_eq!(project.step(), project.step(), "{state:?}");
        }
    }
}
