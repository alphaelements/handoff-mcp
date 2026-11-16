//! Judges the pre-release check items of a milestone (SPEC-ONB-040).
//!
//! Everything here is a function of the facts it is handed: the tasks and
//! requirement items of the milestone, the outcome of the secrets check, the
//! command results a caller reported (or the last recorded ones) and the
//! confirmations on record. It reads no file, writes none and starts no
//! process; gathering the facts and keeping the record is the caller's job
//! (`crate::mcp::handlers::release_check`).

use serde::Serialize;

use super::record::{CommandRecord, ReleaseRecord};
use crate::setup::doctor::{
    judge_command, Check, CheckStatus, CommandResult, CommandToRun, ResultSource,
    DEPENDENCIES_AUDITED,
};
use crate::storage::config::Config;

/// Every leaf task of the milestone is done or skipped.
pub const TASKS_DONE: &str = "tasks_done";
/// No task of the milestone labelled as a bug is still open.
pub const NO_OPEN_BUGS: &str = "no_open_bugs";
/// Every requirement item of the milestone is verified.
pub const REQUIREMENTS_VERIFIED: &str = "requirements_verified";
/// `[commands] test` succeeded.
pub const TESTS_PASS: &str = "tests_pass";
/// `[commands] smoke` succeeded.
pub const SMOKE_PASS: &str = "smoke_pass";
/// Prefix of the items a person confirms: `manual:<name>` for every name in
/// `[release] manual_checks`.
pub const MANUAL_PREFIX: &str = "manual:";

/// The items that judge a command's result, with their `[commands]` key, in
/// the order the commands are handed out.
pub const COMMAND_ITEMS: &[(&str, &str)] = &[
    (TESTS_PASS, "test"),
    (SMOKE_PASS, "smoke"),
    (DEPENDENCIES_AUDITED, "audit"),
];

/// How many task or requirement ids a detail lists before summarizing. The
/// item's `refs` always carry all of them.
const REFS_LISTED: usize = 20;

/// One item of the check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReleaseItem {
    pub id: String,
    pub status: CheckStatus,
    /// What was found, or why the item was not judged. Never empty.
    pub detail: String,
    /// Ids of the tasks or requirement items the finding is about.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refs: Option<Vec<String>>,
    /// Who ran the command a command item was judged from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<ResultSource>,
    /// When the command result was judged, or the item was confirmed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<String>,
}

/// The check of one milestone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReleaseReport {
    pub milestone: String,
    /// `true` only when every item passed. An item that was skipped or still
    /// waits for a person keeps it `false`.
    pub ok: bool,
    pub items: Vec<ReleaseItem>,
    /// Commands whose result is still needed.
    pub commands_to_run: Vec<CommandToRun>,
}

/// A task of the milestone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskFact {
    pub id: String,
    pub status: String,
    /// None of the task's descendants belongs to the milestone.
    pub is_leaf: bool,
    /// The task carries the bug label.
    pub is_bug: bool,
}

/// The requirement items of the milestone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequirementFacts {
    /// How many requirement items are planned for the milestone.
    pub total: usize,
    /// Those whose verification state is not `passing`: `(id, state)`.
    pub not_passing: Vec<(String, String)>,
    /// Documents that could not be read, so their items are unknown.
    pub unreadable: Vec<String>,
}

/// Where the result of a command item comes from.
#[derive(Debug, Clone, Copy)]
pub enum CommandEvidence<'a> {
    /// The results handed in with this call. A command without one is not
    /// judged: an older, recorded result is never used in its place.
    Given {
        results: &'a [CommandResult],
        source: ResultSource,
    },
    /// The last recorded results, for a step that must not ask for a run.
    Recorded,
}

/// Everything an evaluation looks at.
pub struct ReleaseInputs<'a> {
    pub milestone: &'a str,
    pub config: &'a Config,
    /// The tasks scheduled on the milestone.
    pub tasks: &'a [TaskFact],
    /// The milestone's requirement items, or why they could not be read.
    pub requirements: Result<&'a RequirementFacts, &'a str>,
    /// The outcome of the `secrets_untracked` check.
    pub secrets: Check,
    /// What is on record for the milestone.
    pub record: Option<&'a ReleaseRecord>,
    pub evidence: CommandEvidence<'a>,
    /// The time of this evaluation (RFC 3339).
    pub now: &'a str,
}

/// The outcome of [`evaluate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    pub report: ReleaseReport,
    /// The command results that were judged in this evaluation, ready to be
    /// put on record, keyed by item id.
    pub judged: Vec<(&'static str, CommandRecord)>,
}

/// The id of the manual item named `name`.
pub fn manual_item_id(name: &str) -> String {
    format!("{MANUAL_PREFIX}{name}")
}

/// Judges every item of the milestone's pre-release check.
pub fn evaluate(inputs: &ReleaseInputs<'_>) -> Evaluation {
    let mut items = vec![
        tasks_done(inputs.tasks),
        no_open_bugs(inputs.tasks),
        requirements_verified(inputs.requirements),
    ];
    let mut commands_to_run = Vec::new();
    let mut judged = Vec::new();
    let timeout_sec = inputs.config.onboarding.command_timeout_sec();

    let mut command_items = Vec::new();
    for (id, key) in COMMAND_ITEMS.iter().copied() {
        let configured = inputs.config.commands.get(key);
        let item = match inputs.evidence {
            CommandEvidence::Given { results, source } => {
                let reported = results.iter().find(|result| result.id == id);
                let (check, waiting) = judge_command(id, key, configured, reported, source);
                if waiting {
                    if let Some(command) = configured {
                        commands_to_run.push(CommandToRun {
                            id,
                            command: command.to_string(),
                            timeout_sec,
                        });
                    }
                }
                let item = item_from_check(check.clone(), inputs.now);
                // Only a result that was used has a source.
                if let (Some(source), Some(command)) = (check.source, configured) {
                    judged.push((
                        id,
                        CommandRecord {
                            status: check.status,
                            command: command.to_string(),
                            exit_code: check.exit_code,
                            source,
                            checked_at: inputs.now.to_string(),
                            duration_ms: check.duration_ms,
                            detail: item.detail.clone(),
                        },
                    ));
                }
                item
            }
            CommandEvidence::Recorded => recorded_command(id, key, configured, inputs.record),
        };
        command_items.push(item);
    }
    // Report order: tests, smoke, secrets, audit.
    let audit = command_items.pop();
    items.extend(command_items);
    items.push(item_from_check(inputs.secrets.clone(), inputs.now));
    items.extend(audit);

    for name in inputs.config.release.manual_checks() {
        items.push(manual_item(name, inputs.record));
    }

    Evaluation {
        report: ReleaseReport {
            milestone: inputs.milestone.to_string(),
            ok: items.iter().all(|item| item.status == CheckStatus::Pass),
            items,
            commands_to_run,
        },
        judged,
    }
}

fn plain_item(id: &str, status: CheckStatus, detail: impl Into<String>) -> ReleaseItem {
    ReleaseItem {
        id: id.to_string(),
        status,
        detail: detail.into(),
        refs: None,
        source: None,
        checked_at: None,
    }
}

/// `entries` as `id (state)`, the first [`REFS_LISTED`] of them.
fn listed(entries: &[(&str, &str)]) -> String {
    let mut shown: Vec<String> = entries
        .iter()
        .take(REFS_LISTED)
        .map(|(id, state)| format!("{id} ({state})"))
        .collect();
    if entries.len() > REFS_LISTED {
        shown.push(format!("and {} more", entries.len() - REFS_LISTED));
    }
    shown.join(", ")
}

fn is_finished(status: &str) -> bool {
    crate::storage::tasks::is_terminal_status(status)
}

fn tasks_done(tasks: &[TaskFact]) -> ReleaseItem {
    let leaves: Vec<&TaskFact> = tasks.iter().filter(|task| task.is_leaf).collect();
    if leaves.is_empty() {
        return plain_item(
            TASKS_DONE,
            CheckStatus::Pass,
            "no task is scheduled on this milestone, so none is unfinished",
        );
    }
    let unfinished: Vec<(&str, &str)> = leaves
        .iter()
        .filter(|task| !is_finished(&task.status))
        .map(|task| (task.id.as_str(), task.status.as_str()))
        .collect();
    if unfinished.is_empty() {
        return plain_item(
            TASKS_DONE,
            CheckStatus::Pass,
            format!(
                "all {} leaf task(s) of the milestone are done or skipped",
                leaves.len()
            ),
        );
    }
    ReleaseItem {
        refs: Some(unfinished.iter().map(|(id, _)| id.to_string()).collect()),
        ..plain_item(
            TASKS_DONE,
            CheckStatus::Fail,
            format!(
                "{} of {} leaf task(s) of the milestone are not done or skipped: {}",
                unfinished.len(),
                leaves.len(),
                listed(&unfinished)
            ),
        )
    }
}

fn no_open_bugs(tasks: &[TaskFact]) -> ReleaseItem {
    let bugs: Vec<&TaskFact> = tasks.iter().filter(|task| task.is_bug).collect();
    let open: Vec<(&str, &str)> = bugs
        .iter()
        .filter(|task| !is_finished(&task.status))
        .map(|task| (task.id.as_str(), task.status.as_str()))
        .collect();
    if open.is_empty() {
        return plain_item(
            NO_OPEN_BUGS,
            CheckStatus::Pass,
            format!(
                "no open bug task on the milestone ({} bug task(s), all done or skipped)",
                bugs.len()
            ),
        );
    }
    ReleaseItem {
        refs: Some(open.iter().map(|(id, _)| id.to_string()).collect()),
        ..plain_item(
            NO_OPEN_BUGS,
            CheckStatus::Fail,
            format!(
                "{} bug task(s) of the milestone are still open: {}",
                open.len(),
                listed(&open)
            ),
        )
    }
}

fn requirements_verified(requirements: Result<&RequirementFacts, &str>) -> ReleaseItem {
    let facts = match requirements {
        Ok(facts) => facts,
        Err(reason) => {
            return plain_item(
                REQUIREMENTS_VERIFIED,
                CheckStatus::Fail,
                format!("the requirement documents could not be read: {reason}"),
            )
        }
    };
    // An unreadable document may hold items of this milestone: without it the
    // rest cannot be called complete.
    if !facts.unreadable.is_empty() {
        return plain_item(
            REQUIREMENTS_VERIFIED,
            CheckStatus::Fail,
            format!(
                "{} document(s) could not be read, so their requirement items are unknown: {}. \
                 Repair them (`handoff_doc_repair_frontmatter`) and check again",
                facts.unreadable.len(),
                facts.unreadable.join(", ")
            ),
        );
    }
    if facts.total == 0 {
        return plain_item(
            REQUIREMENTS_VERIFIED,
            CheckStatus::Skip,
            "not checked: no requirement item is planned for this milestone",
        );
    }
    if facts.not_passing.is_empty() {
        return plain_item(
            REQUIREMENTS_VERIFIED,
            CheckStatus::Pass,
            format!(
                "all {} requirement item(s) of the milestone are passing",
                facts.total
            ),
        );
    }
    let entries: Vec<(&str, &str)> = facts
        .not_passing
        .iter()
        .map(|(id, state)| (id.as_str(), state.as_str()))
        .collect();
    ReleaseItem {
        refs: Some(entries.iter().map(|(id, _)| id.to_string()).collect()),
        ..plain_item(
            REQUIREMENTS_VERIFIED,
            CheckStatus::Fail,
            format!(
                "{} of {} requirement item(s) of the milestone are not passing: {}",
                entries.len(),
                facts.total,
                listed(&entries)
            ),
        )
    }
}

/// A release item from a judged [`Check`]. An item has no `next_step` field,
/// so what to do about a failure goes into its detail.
fn item_from_check(check: Check, now: &str) -> ReleaseItem {
    let detail = match &check.next_step {
        Some(next_step) => format!("{}\nNext: {next_step}", check.detail),
        None => check.detail,
    };
    ReleaseItem {
        source: check.source,
        // A command result is dated by the call that judged it.
        checked_at: check.source.map(|_| now.to_string()),
        ..plain_item(check.id, check.status, detail)
    }
}

/// A command item answered from the record instead of a result of this call.
fn recorded_command(
    id: &str,
    key: &str,
    configured: Option<&str>,
    record: Option<&ReleaseRecord>,
) -> ReleaseItem {
    let Some(command) = configured else {
        return plain_item(
            id,
            CheckStatus::Skip,
            format!("not verified: `[commands] {key}` is not defined"),
        );
    };
    let Some(recorded) = record.and_then(|record| record.commands.get(id)) else {
        return plain_item(
            id,
            CheckStatus::Skip,
            format!(
                "not verified: no result is on record for `{command}`. Run \
                 `handoff_release_check` and report the result"
            ),
        );
    };
    if recorded.command != command {
        return plain_item(
            id,
            CheckStatus::Skip,
            format!(
                "not verified: the result on record ({}) is for `{}`, but `[commands] {key}` is \
                 now `{command}`",
                recorded.checked_at, recorded.command
            ),
        );
    }
    ReleaseItem {
        source: Some(recorded.source),
        checked_at: Some(recorded.checked_at.clone()),
        ..plain_item(
            id,
            recorded.status,
            format!(
                "result on record from {}: {}",
                recorded.checked_at, recorded.detail
            ),
        )
    }
}

fn manual_item(name: &str, record: Option<&ReleaseRecord>) -> ReleaseItem {
    let id = manual_item_id(name);
    match record.and_then(|record| record.manual.get(&id)) {
        Some(confirmation) => ReleaseItem {
            checked_at: Some(confirmation.confirmed_at.clone()),
            ..plain_item(
                &id,
                CheckStatus::Pass,
                format!("confirmed on {}", confirmation.confirmed_at),
            )
        },
        None => plain_item(
            &id,
            CheckStatus::Manual,
            format!(
                "waiting for a person to confirm `{name}`. Once it is confirmed, call \
                 `handoff_release_check` with `confirm: [\"{id}\"]`"
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release::record::ManualRecord;
    use crate::setup::secrets::SECRETS_UNTRACKED;

    const NOW: &str = "2026-11-15T10:00:00+00:00";

    fn config(extra: &str) -> Config {
        toml::from_str(&format!(
            "[project]\nname = \"demo\"\n\n[milestones.v1]\n\n{extra}"
        ))
        .unwrap()
    }

    /// A config with all three commands and one manual check.
    fn full_config() -> Config {
        config(
            "[commands]\ntest = \"npm test\"\nsmoke = \"./smoke.sh\"\naudit = \"npm audit\"\n\n\
             [release]\nmanual_checks = [\"docs_updated\"]\n",
        )
    }

    fn task(id: &str, status: &str) -> TaskFact {
        TaskFact {
            id: id.to_string(),
            status: status.to_string(),
            is_leaf: true,
            is_bug: false,
        }
    }

    fn secrets(status: CheckStatus) -> Check {
        match status {
            CheckStatus::Fail => Check::failed(
                SECRETS_UNTRACKED,
                "Git tracks 1 file(s) whose name matches a secrets pattern: `.env`.",
                "Stop tracking each file (`git rm --cached <path>`).",
            ),
            other => Check::new(SECRETS_UNTRACKED, other, "secrets detail"),
        }
    }

    fn result(id: &str, command: &str, exit_code: i64) -> CommandResult {
        CommandResult {
            id: id.to_string(),
            command: command.to_string(),
            exit_code: Some(exit_code),
            duration_ms: Some(900),
            output_tail: None,
            error: None,
        }
    }

    fn all_passing_results() -> Vec<CommandResult> {
        vec![
            result(TESTS_PASS, "npm test", 0),
            result(SMOKE_PASS, "./smoke.sh", 0),
            result(DEPENDENCIES_AUDITED, "npm audit", 0),
        ]
    }

    fn confirmed_record() -> ReleaseRecord {
        let mut record = ReleaseRecord {
            milestone: "v1".to_string(),
            ..ReleaseRecord::default()
        };
        record.manual.insert(
            "manual:docs_updated".to_string(),
            ManualRecord {
                confirmed_at: "2026-11-14T09:00:00+00:00".to_string(),
            },
        );
        record
    }

    fn verified() -> RequirementFacts {
        RequirementFacts {
            total: 2,
            ..RequirementFacts::default()
        }
    }

    struct Case {
        config: Config,
        tasks: Vec<TaskFact>,
        requirements: Result<RequirementFacts, String>,
        secrets: Check,
        record: Option<ReleaseRecord>,
        results: Vec<CommandResult>,
    }

    impl Case {
        /// A milestone where every item passes.
        fn passing() -> Self {
            Self {
                config: full_config(),
                tasks: vec![task("t1", "done"), task("t2", "skipped")],
                requirements: Ok(verified()),
                secrets: secrets(CheckStatus::Pass),
                record: Some(confirmed_record()),
                results: all_passing_results(),
            }
        }

        fn evaluate_with(&self, evidence: CommandEvidence<'_>) -> Evaluation {
            evaluate(&ReleaseInputs {
                milestone: "v1",
                config: &self.config,
                tasks: &self.tasks,
                requirements: self.requirements.as_ref().map_err(String::as_str),
                secrets: self.secrets.clone(),
                record: self.record.as_ref(),
                evidence,
                now: NOW,
            })
        }

        fn evaluate(&self) -> Evaluation {
            self.evaluate_with(CommandEvidence::Given {
                results: &self.results,
                source: ResultSource::Reported,
            })
        }

        fn report(&self) -> ReleaseReport {
            self.evaluate().report
        }
    }

    fn item<'a>(report: &'a ReleaseReport, id: &str) -> &'a ReleaseItem {
        report
            .items
            .iter()
            .find(|item| item.id == id)
            .unwrap_or_else(|| panic!("no item `{id}` in {report:?}"))
    }

    fn status(report: &ReleaseReport, id: &str) -> CheckStatus {
        item(report, id).status
    }

    fn ids(report: &ReleaseReport) -> Vec<&str> {
        report.items.iter().map(|item| item.id.as_str()).collect()
    }

    // --- shape (SPEC-ONB-040 AC1) ---

    #[test]
    fn the_items_come_in_the_documented_order_with_a_detail_each() {
        for case in [Case::passing(), {
            let mut bare = Case::passing();
            bare.config = config("");
            bare.results.clear();
            bare.record = None;
            bare
        }] {
            let report = case.report();
            assert_eq!(report.milestone, "v1");
            let mut expected = vec![
                "tasks_done",
                "no_open_bugs",
                "requirements_verified",
                "tests_pass",
                "smoke_pass",
                "secrets_untracked",
                "dependencies_audited",
            ];
            if !case.config.release.manual_checks().is_empty() {
                expected.push("manual:docs_updated");
            }
            assert_eq!(ids(&report), expected);
            for item in &report.items {
                assert!(!item.detail.trim().is_empty(), "{}: empty detail", item.id);
            }
        }
    }

    #[test]
    fn a_report_serializes_to_the_documented_shape() {
        let mut case = Case::passing();
        case.tasks.push(task("t3", "todo"));
        let json = serde_json::to_value(case.report()).unwrap();
        assert_eq!(
            json.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["milestone", "ok", "items", "commands_to_run"]
        );
        let keys = |id: &str| -> Vec<String> {
            json["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["id"] == id)
                .unwrap()
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect()
        };
        assert_eq!(keys("tasks_done"), ["id", "status", "detail", "refs"]);
        assert_eq!(keys("no_open_bugs"), ["id", "status", "detail"]);
        assert_eq!(
            keys("tests_pass"),
            ["id", "status", "detail", "source", "checked_at"]
        );
        assert_eq!(
            keys("manual:docs_updated"),
            ["id", "status", "detail", "checked_at"]
        );
        for item in json["items"].as_array().unwrap() {
            assert!(
                ["pass", "fail", "skip", "manual"].contains(&item["status"].as_str().unwrap()),
                "{item}"
            );
        }
    }

    // --- ok (SPEC-ONB-040, SPEC-ONB-092 AC3) ---

    #[test]
    fn ok_is_true_only_when_every_item_passes() {
        let report = Case::passing().report();
        assert_eq!(report.items.len(), 8);
        for item in &report.items {
            assert_eq!(item.status, CheckStatus::Pass, "{item:?}");
        }
        assert!(report.ok);
        assert!(report.commands_to_run.is_empty());
    }

    // SPEC-ONB-092 AC3
    #[test]
    fn a_single_skipped_item_makes_ok_false() {
        // One at a time: each way an item can be skipped.
        let skipped_secrets = {
            let mut case = Case::passing();
            case.secrets = secrets(CheckStatus::Skip);
            case
        };
        let no_requirements = {
            let mut case = Case::passing();
            case.requirements = Ok(RequirementFacts::default());
            case
        };
        let no_smoke_result = {
            let mut case = Case::passing();
            case.results.retain(|r| r.id != SMOKE_PASS);
            case
        };
        let audit_undefined = {
            let mut case = Case::passing();
            case.config = config(
                "[commands]\ntest = \"npm test\"\nsmoke = \"./smoke.sh\"\n\n\
                 [release]\nmanual_checks = [\"docs_updated\"]\n",
            );
            case.results.retain(|r| r.id != DEPENDENCIES_AUDITED);
            case
        };
        for (case, skipped) in [
            (skipped_secrets, "secrets_untracked"),
            (no_requirements, "requirements_verified"),
            (no_smoke_result, "smoke_pass"),
            (audit_undefined, "dependencies_audited"),
        ] {
            let report = case.report();
            let not_passing: Vec<(&str, CheckStatus)> = report
                .items
                .iter()
                .filter(|item| item.status != CheckStatus::Pass)
                .map(|item| (item.id.as_str(), item.status))
                .collect();
            assert_eq!(
                not_passing,
                vec![(skipped, CheckStatus::Skip)],
                "exactly one item is skipped"
            );
            assert!(!report.ok, "a skip is never counted as a pass: {skipped}");
        }
    }

    #[test]
    fn a_single_manual_or_failed_item_makes_ok_false() {
        let mut unconfirmed = Case::passing();
        unconfirmed.record = None;
        let report = unconfirmed.report();
        assert_eq!(status(&report, "manual:docs_updated"), CheckStatus::Manual);
        assert!(!report.ok);

        let mut failing = Case::passing();
        failing.results[0].exit_code = Some(2);
        let report = failing.report();
        assert_eq!(status(&report, TESTS_PASS), CheckStatus::Fail);
        assert!(!report.ok);
    }

    // --- tasks_done (SPEC-ONB-040 AC2) ---

    #[test]
    fn an_unfinished_leaf_task_fails_tasks_done_and_is_referenced() {
        let mut case = Case::passing();
        case.tasks = vec![
            task("t1", "done"),
            task("t2", "in_progress"),
            task("t3", "skipped"),
            task("t4", "review"),
            task("t5", "blocked"),
            task("t6", "todo"),
        ];
        let report = case.report();
        let tasks_done = item(&report, TASKS_DONE);
        assert_eq!(tasks_done.status, CheckStatus::Fail);
        assert_eq!(
            tasks_done.refs.as_deref(),
            Some(&["t2", "t4", "t5", "t6"].map(String::from)[..])
        );
        assert!(
            tasks_done.detail.contains("t2 (in_progress)"),
            "{}",
            tasks_done.detail
        );
        assert!(
            tasks_done.detail.contains("4 of 6"),
            "{}",
            tasks_done.detail
        );
        assert!(!report.ok);
    }

    #[test]
    fn only_leaf_tasks_decide_tasks_done() {
        let mut case = Case::passing();
        // A parent still marked in progress, with every child finished.
        case.tasks = vec![
            TaskFact {
                is_leaf: false,
                ..task("t1", "in_progress")
            },
            task("t1.1", "done"),
            task("t1.2", "skipped"),
        ];
        let report = case.report();
        let tasks_done = item(&report, TASKS_DONE);
        assert_eq!(
            tasks_done.status,
            CheckStatus::Pass,
            "{}",
            tasks_done.detail
        );
        assert_eq!(tasks_done.refs, None);
        assert!(tasks_done.detail.contains("2 leaf task(s)"));
    }

    #[test]
    fn a_milestone_without_tasks_passes_tasks_done_and_says_so() {
        let mut case = Case::passing();
        case.tasks.clear();
        let report = case.report();
        let tasks_done = item(&report, TASKS_DONE);
        assert_eq!(tasks_done.status, CheckStatus::Pass);
        assert!(
            tasks_done.detail.contains("no task is scheduled"),
            "{}",
            tasks_done.detail
        );
    }

    #[test]
    fn a_long_list_of_unfinished_tasks_is_summarized_but_fully_referenced() {
        let mut case = Case::passing();
        case.tasks = (0..25).map(|i| task(&format!("t{i:02}"), "todo")).collect();
        let report = case.report();
        let tasks_done = item(&report, TASKS_DONE);
        assert_eq!(tasks_done.refs.as_ref().unwrap().len(), 25);
        assert!(tasks_done.detail.contains("t19 (todo)"));
        assert!(!tasks_done.detail.contains("t20 (todo)"));
        assert!(tasks_done.detail.contains("and 5 more"));
    }

    // --- no_open_bugs ---

    #[test]
    fn an_open_bug_task_fails_no_open_bugs_at_any_depth() {
        let mut case = Case::passing();
        case.tasks = vec![
            task("t1", "done"),
            TaskFact {
                is_bug: true,
                ..task("t2", "done")
            },
            TaskFact {
                is_bug: true,
                is_leaf: false,
                ..task("t3", "in_progress")
            },
            TaskFact {
                is_bug: true,
                ..task("t3.1", "skipped")
            },
        ];
        let report = case.report();
        let bugs = item(&report, NO_OPEN_BUGS);
        assert_eq!(bugs.status, CheckStatus::Fail);
        assert_eq!(bugs.refs.as_deref(), Some(&["t3".to_string()][..]));
        assert!(bugs.detail.contains("t3 (in_progress)"), "{}", bugs.detail);
        // The parent is not a leaf, so the task item itself still passes.
        assert_eq!(status(&report, TASKS_DONE), CheckStatus::Pass);
    }

    #[test]
    fn closed_bugs_and_open_tasks_that_are_not_bugs_pass_no_open_bugs() {
        let mut case = Case::passing();
        case.tasks = vec![
            TaskFact {
                is_bug: true,
                ..task("t1", "done")
            },
            task("t2", "todo"),
        ];
        let report = case.report();
        assert_eq!(status(&report, NO_OPEN_BUGS), CheckStatus::Pass);
        assert_eq!(item(&report, NO_OPEN_BUGS).refs, None);
    }

    // --- requirements_verified ---

    #[test]
    fn a_requirement_that_is_not_passing_fails_and_is_referenced() {
        let mut case = Case::passing();
        case.requirements = Ok(RequirementFacts {
            total: 3,
            not_passing: vec![
                ("REQ-2".to_string(), "failing".to_string()),
                ("REQ-3".to_string(), "not_run".to_string()),
            ],
            unreadable: Vec::new(),
        });
        let report = case.report();
        let requirements = item(&report, REQUIREMENTS_VERIFIED);
        assert_eq!(requirements.status, CheckStatus::Fail);
        assert_eq!(
            requirements.refs.as_deref(),
            Some(&["REQ-2".to_string(), "REQ-3".to_string()][..])
        );
        assert!(
            requirements.detail.contains("REQ-2 (failing)")
                && requirements.detail.contains("REQ-3 (not_run)"),
            "{}",
            requirements.detail
        );
        assert!(requirements.detail.contains("2 of 3"));
    }

    #[test]
    fn a_milestone_without_requirements_is_skipped_not_passed() {
        let mut case = Case::passing();
        case.requirements = Ok(RequirementFacts::default());
        let report = case.report();
        let requirements = item(&report, REQUIREMENTS_VERIFIED);
        assert_eq!(requirements.status, CheckStatus::Skip);
        assert!(
            requirements.detail.contains("no requirement item"),
            "{}",
            requirements.detail
        );
    }

    #[test]
    fn requirements_that_cannot_be_read_fail_instead_of_passing_unseen() {
        let mut unreadable = Case::passing();
        unreadable.requirements = Ok(RequirementFacts {
            total: 2,
            not_passing: Vec::new(),
            unreadable: vec!["req-auth".to_string()],
        });
        let report = unreadable.report();
        let requirements = item(&report, REQUIREMENTS_VERIFIED);
        assert_eq!(requirements.status, CheckStatus::Fail);
        assert!(
            requirements.detail.contains("req-auth"),
            "{}",
            requirements.detail
        );

        // Also when nothing at all could be read for the milestone.
        let mut hidden = Case::passing();
        hidden.requirements = Ok(RequirementFacts {
            total: 0,
            not_passing: Vec::new(),
            unreadable: vec!["req-auth".to_string()],
        });
        assert_eq!(
            status(&hidden.report(), REQUIREMENTS_VERIFIED),
            CheckStatus::Fail
        );

        let mut broken = Case::passing();
        broken.requirements = Err("config.toml: bad layer".to_string());
        let report = broken.report();
        let requirements = item(&report, REQUIREMENTS_VERIFIED);
        assert_eq!(requirements.status, CheckStatus::Fail);
        assert!(
            requirements.detail.contains("bad layer"),
            "{}",
            requirements.detail
        );
    }

    // --- command items (SPEC-ONB-040 AC3, shared design) ---

    // SPEC-ONB-040 AC3
    #[test]
    fn without_results_the_command_items_are_skipped_and_handed_back() {
        let mut case = Case::passing();
        case.results.clear();
        let evaluation = case.evaluate();
        let report = &evaluation.report;
        for id in [TESTS_PASS, SMOKE_PASS, DEPENDENCIES_AUDITED] {
            let command = item(report, id);
            assert_eq!(command.status, CheckStatus::Skip, "{id}");
            assert_eq!(command.source, None, "{id}");
            assert_eq!(command.checked_at, None, "{id}");
            assert!(
                command.detail.contains("no result was given"),
                "{}",
                command.detail
            );
        }
        assert!(!report.ok);
        assert_eq!(
            report.commands_to_run,
            vec![
                CommandToRun {
                    id: TESTS_PASS,
                    command: "npm test".to_string(),
                    timeout_sec: 600
                },
                CommandToRun {
                    id: SMOKE_PASS,
                    command: "./smoke.sh".to_string(),
                    timeout_sec: 600
                },
                CommandToRun {
                    id: DEPENDENCIES_AUDITED,
                    command: "npm audit".to_string(),
                    timeout_sec: 600
                },
            ]
        );
        assert!(evaluation.judged.is_empty(), "nothing to put on record");
    }

    /// A result on record is not a result of this call: it must not turn a
    /// command item into a pass (SPEC-ONB-040).
    #[test]
    fn a_recorded_result_is_not_used_when_no_result_is_given() {
        let mut case = Case::passing();
        let with_results = case.evaluate();
        let mut record = confirmed_record();
        for (id, command) in &with_results.judged {
            record.commands.insert(id.to_string(), command.clone());
        }
        assert_eq!(record.commands.len(), 3);
        case.record = Some(record);
        case.results.clear();
        let report = case.report();
        for id in [TESTS_PASS, SMOKE_PASS, DEPENDENCIES_AUDITED] {
            assert_eq!(status(&report, id), CheckStatus::Skip, "{id}");
        }
        assert_eq!(report.commands_to_run.len(), 3);
        assert!(!report.ok);
    }

    #[test]
    fn an_undefined_command_is_skipped_and_not_handed_back() {
        let mut case = Case::passing();
        case.config = config("[commands]\ntest = \"npm test\"\n");
        case.results.clear();
        let report = case.report();
        for id in [SMOKE_PASS, DEPENDENCIES_AUDITED] {
            assert_eq!(status(&report, id), CheckStatus::Skip, "{id}");
            assert!(item(&report, id).detail.contains("not defined"));
        }
        assert_eq!(
            report
                .commands_to_run
                .iter()
                .map(|c| c.id)
                .collect::<Vec<_>>(),
            vec![TESTS_PASS]
        );
    }

    #[test]
    fn the_time_limit_handed_out_follows_the_config() {
        let mut case = Case::passing();
        case.config =
            config("[commands]\ntest = \"npm test\"\n\n[onboarding]\ncommand_timeout_sec = 45\n");
        case.results.clear();
        assert_eq!(case.report().commands_to_run[0].timeout_sec, 45);
    }

    #[test]
    fn reported_results_are_judged_and_made_ready_for_the_record() {
        let mut case = Case::passing();
        case.results = vec![
            result(TESTS_PASS, "npm test", 0),
            result(SMOKE_PASS, "./smoke.sh", 3),
            CommandResult {
                exit_code: None,
                error: Some("timed out after 600s".to_string()),
                output_tail: Some("resolving...\n".to_string()),
                ..result(DEPENDENCIES_AUDITED, "npm audit", 0)
            },
        ];
        let evaluation = case.evaluate();
        let report = &evaluation.report;

        let tests = item(report, TESTS_PASS);
        assert_eq!(tests.status, CheckStatus::Pass);
        assert_eq!(tests.source, Some(ResultSource::Reported));
        assert_eq!(tests.checked_at.as_deref(), Some(NOW));

        let smoke = item(report, SMOKE_PASS);
        assert_eq!(smoke.status, CheckStatus::Fail);
        assert!(smoke.detail.contains("exit code 3"), "{}", smoke.detail);

        let audit = item(report, DEPENDENCIES_AUDITED);
        assert_eq!(audit.status, CheckStatus::Fail);
        assert!(audit.detail.contains("timed out"), "{}", audit.detail);
        assert!(audit.detail.contains("resolving..."), "{}", audit.detail);
        assert!(report.commands_to_run.is_empty());

        assert_eq!(
            evaluation
                .judged
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>(),
            vec![TESTS_PASS, SMOKE_PASS, DEPENDENCIES_AUDITED]
        );
        let (_, recorded_smoke) = &evaluation.judged[1];
        assert_eq!(
            recorded_smoke,
            &CommandRecord {
                status: CheckStatus::Fail,
                command: "./smoke.sh".to_string(),
                exit_code: Some(3),
                source: ResultSource::Reported,
                checked_at: NOW.to_string(),
                duration_ms: Some(900),
                detail: smoke.detail.clone(),
            }
        );
        let (_, recorded_audit) = &evaluation.judged[2];
        assert_eq!(recorded_audit.exit_code, None);
        assert_eq!(recorded_audit.status, CheckStatus::Fail);
    }

    #[test]
    fn results_carry_the_source_they_were_given() {
        let case = Case::passing();
        let evaluation = case.evaluate_with(CommandEvidence::Given {
            results: &case.results,
            source: ResultSource::Executed,
        });
        assert_eq!(
            item(&evaluation.report, TESTS_PASS).source,
            Some(ResultSource::Executed)
        );
        assert!(evaluation
            .judged
            .iter()
            .all(|(_, record)| record.source == ResultSource::Executed));
    }

    #[test]
    fn a_result_for_another_command_fails_and_is_not_recorded() {
        let mut case = Case::passing();
        case.results[0] = result(TESTS_PASS, "true", 0);
        let evaluation = case.evaluate();
        let tests = item(&evaluation.report, TESTS_PASS);
        assert_eq!(tests.status, CheckStatus::Fail);
        assert!(tests.detail.contains("does not match"), "{}", tests.detail);
        assert_eq!(tests.source, None);
        assert_eq!(tests.checked_at, None);
        assert_eq!(
            evaluation
                .report
                .commands_to_run
                .iter()
                .map(|c| c.id)
                .collect::<Vec<_>>(),
            vec![TESTS_PASS],
            "the command is handed out again"
        );
        assert!(
            evaluation.judged.iter().all(|(id, _)| *id != TESTS_PASS),
            "a result that was not used is not put on record"
        );
    }

    // --- secrets ---

    #[test]
    fn the_secrets_check_is_reported_with_what_to_do() {
        let mut case = Case::passing();
        case.secrets = secrets(CheckStatus::Fail);
        let report = case.report();
        let secrets_item = item(&report, SECRETS_UNTRACKED);
        assert_eq!(secrets_item.status, CheckStatus::Fail);
        assert!(
            secrets_item.detail.contains("`.env`"),
            "{}",
            secrets_item.detail
        );
        assert!(
            secrets_item.detail.contains("git rm --cached"),
            "a release item has no next_step field, so the detail carries it: {}",
            secrets_item.detail
        );
        assert!(!report.ok);
    }

    // --- manual items (SPEC-ONB-040 AC4) ---

    #[test]
    fn a_manual_item_waits_until_it_is_confirmed() {
        let mut case = Case::passing();
        case.config = config("[release]\nmanual_checks = [\"docs_updated\", \"store_listing\"]\n");
        case.record = None;
        let report = case.report();
        for id in ["manual:docs_updated", "manual:store_listing"] {
            let manual = item(&report, id);
            assert_eq!(manual.status, CheckStatus::Manual, "{id}");
            assert_eq!(manual.checked_at, None);
            assert!(
                manual.detail.contains(&format!("confirm: [\"{id}\"]")),
                "the detail says how to confirm: {}",
                manual.detail
            );
        }

        case.record = Some(confirmed_record());
        let report = case.report();
        let confirmed = item(&report, "manual:docs_updated");
        assert_eq!(confirmed.status, CheckStatus::Pass);
        assert_eq!(
            confirmed.checked_at.as_deref(),
            Some("2026-11-14T09:00:00+00:00")
        );
        assert_eq!(
            status(&report, "manual:store_listing"),
            CheckStatus::Manual,
            "confirming one item does not confirm another"
        );
    }

    #[test]
    fn a_confirmation_of_a_check_that_is_no_longer_configured_is_not_shown() {
        let mut case = Case::passing();
        case.config = config("[commands]\ntest = \"npm test\"\n");
        let report = case.report();
        assert!(ids(&report).iter().all(|id| !id.starts_with(MANUAL_PREFIX)));
    }

    // --- the last recorded results (SPEC-ONB-041) ---

    fn recorded(case: &mut Case) {
        let judged = case.evaluate().judged;
        let mut record = confirmed_record();
        for (id, mut command) in judged {
            command.checked_at = "2026-11-10T08:00:00+00:00".to_string();
            record.commands.insert(id.to_string(), command);
        }
        case.record = Some(record);
    }

    #[test]
    fn recorded_results_are_used_when_asked_for_and_dated() {
        let mut case = Case::passing();
        recorded(&mut case);
        let evaluation = case.evaluate_with(CommandEvidence::Recorded);
        let report = &evaluation.report;
        for id in [TESTS_PASS, SMOKE_PASS, DEPENDENCIES_AUDITED] {
            let command = item(report, id);
            assert_eq!(command.status, CheckStatus::Pass, "{id}");
            assert_eq!(
                command.checked_at.as_deref(),
                Some("2026-11-10T08:00:00+00:00")
            );
            assert_eq!(command.source, Some(ResultSource::Reported));
            assert!(
                command.detail.contains("2026-11-10T08:00:00+00:00"),
                "the detail dates the record: {}",
                command.detail
            );
        }
        assert!(report.ok);
        assert!(report.commands_to_run.is_empty());
        assert!(
            evaluation.judged.is_empty(),
            "reading the record judges nothing new"
        );
    }

    #[test]
    fn a_recorded_failure_stays_a_failure() {
        let mut case = Case::passing();
        case.results[0].exit_code = Some(1);
        recorded(&mut case);
        let report = case.evaluate_with(CommandEvidence::Recorded).report;
        assert_eq!(status(&report, TESTS_PASS), CheckStatus::Fail);
        assert!(!report.ok);
    }

    #[test]
    fn a_command_without_a_record_is_unverified() {
        let mut case = Case::passing();
        case.record = Some(confirmed_record());
        let report = case.evaluate_with(CommandEvidence::Recorded).report;
        for id in [TESTS_PASS, SMOKE_PASS, DEPENDENCIES_AUDITED] {
            let command = item(&report, id);
            assert_eq!(command.status, CheckStatus::Skip, "{id}");
            assert!(
                command.detail.contains("no result is on record"),
                "{}",
                command.detail
            );
            assert_eq!(command.checked_at, None);
        }
        assert!(report.commands_to_run.is_empty());

        case.record = None;
        let report = case.evaluate_with(CommandEvidence::Recorded).report;
        assert_eq!(status(&report, TESTS_PASS), CheckStatus::Skip);
    }

    /// A record only speaks for the command it was made with.
    #[test]
    fn a_record_for_a_command_that_has_since_changed_is_unverified() {
        let mut case = Case::passing();
        recorded(&mut case);
        case.config = config(
            "[commands]\ntest = \"npm run test:all\"\nsmoke = \"./smoke.sh\"\n\n\
             [release]\nmanual_checks = [\"docs_updated\"]\n",
        );
        let report = case.evaluate_with(CommandEvidence::Recorded).report;
        let tests = item(&report, TESTS_PASS);
        assert_eq!(tests.status, CheckStatus::Skip);
        assert!(
            tests.detail.contains("npm test") && tests.detail.contains("npm run test:all"),
            "{}",
            tests.detail
        );
        assert_eq!(status(&report, SMOKE_PASS), CheckStatus::Pass);
        // No longer defined at all.
        let audit = item(&report, DEPENDENCIES_AUDITED);
        assert_eq!(audit.status, CheckStatus::Skip);
        assert!(audit.detail.contains("not defined"), "{}", audit.detail);
    }
}
