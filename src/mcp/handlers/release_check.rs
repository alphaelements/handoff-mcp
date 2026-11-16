//! `handoff_release_check` (SPEC-ONB-040): the pre-release check of a
//! milestone.
//!
//! This module gathers the facts (tasks, requirement items, tracked files,
//! the record) and keeps the record up to date; the judging is
//! `crate::release::check`. Like `handoff_doctor`, it never runs a command
//! and never starts a process: the project's `.handoff/` is found by reading
//! files, the tracked files come from the Git index file, and the commands
//! are handed back to the caller in `commands_to_run`. Only the
//! `handoff-mcp release-check --run` CLI executes them, and it reports the
//! results through [`run`] the same way an agent does.

use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

use super::doctor::parse_results;
use super::trace_readonly::load_trace_input_fully_read_only;
use super::HandlerContext;
use crate::release::check::{
    evaluate, manual_item_id, CommandEvidence, ReleaseInputs, ReleaseReport, RequirementFacts,
    TaskFact, COMMAND_ITEMS,
};
use crate::release::record::{read_record, write_record, ManualRecord, ReleaseRecord};
use crate::report::defect::BUG_LABEL;
use crate::setup::doctor::{locate_handoff_dir, validate_results, CommandResult, ResultSource};
use crate::setup::secrets::check_secrets_untracked;
use crate::storage::config::{read_config, Config};
use crate::storage::docs::layer::REQUIREMENT_LAYER;
use crate::storage::tasks::read_task;
use crate::trace::types::ItemState;
use crate::trace::TraceGraph;

/// One pre-release check.
pub(crate) struct ReleaseCheckRequest<'a> {
    pub project_dir: &'a Path,
    pub milestone: &'a str,
    /// Results of the commands the caller ran.
    pub results: &'a [CommandResult],
    /// Who ran them.
    pub source: ResultSource,
    /// Ids of the manual items to record as confirmed.
    pub confirm: &'a [String],
}

/// Checks the milestone and, when results or confirmations were passed in,
/// puts them on record. A request that cannot be honored (an unknown
/// milestone, a malformed result, a confirmation of an item that is not
/// configured) is an error and records nothing.
pub(crate) fn run(request: &ReleaseCheckRequest<'_>) -> Result<ReleaseReport> {
    let handoff_dir = locate_handoff_dir(request.project_dir).ok_or_else(|| {
        anyhow::anyhow!(
            "No .handoff/ directory found in {}. Run handoff_init first.",
            request.project_dir.display()
        )
    })?;
    let config = read_config(&handoff_dir.join("config.toml"))?;
    require_milestone(&config, request.milestone)?;
    validate_results(request.results, COMMAND_ITEMS, None)?;
    validate_confirmations(&config, request.confirm)?;

    let now = chrono::Utc::now().to_rfc3339();
    let mut record =
        read_record(&handoff_dir, request.milestone)?.unwrap_or_else(|| ReleaseRecord {
            milestone: request.milestone.to_string(),
            ..ReleaseRecord::default()
        });
    for id in request.confirm {
        record.manual.insert(
            id.clone(),
            ManualRecord {
                confirmed_at: now.clone(),
            },
        );
    }

    let facts = gather(
        request.project_dir,
        &handoff_dir,
        &config,
        request.milestone,
    )?;
    let evaluation = evaluate(&facts.inputs(
        request.milestone,
        &config,
        Some(&record),
        CommandEvidence::Given {
            results: request.results,
            source: request.source,
        },
        &now,
    ));

    if !evaluation.judged.is_empty() || !request.confirm.is_empty() {
        for (id, command) in &evaluation.judged {
            record.commands.insert((*id).to_string(), command.clone());
        }
        record.milestone = request.milestone.to_string();
        record.updated_at = now;
        write_record(&handoff_dir, &record)?;
    }
    Ok(evaluation.report)
}

/// The check as releasing a milestone reads it (SPEC-ONB-041): the items
/// that follow from the project's data are judged now, and each command item
/// is answered from the last result on record, or reported as not verified
/// when there is none. It hands out no command, starts no process and writes
/// nothing; `handoff_dir` is the project's `.handoff/` as the caller already
/// resolved it.
pub(crate) fn check_against_record(
    project_dir: &Path,
    handoff_dir: &Path,
    milestone: &str,
) -> Result<ReleaseReport> {
    let config = read_config(&handoff_dir.join("config.toml"))?;
    require_milestone(&config, milestone)?;
    let record = read_record(handoff_dir, milestone)?;
    let facts = gather(project_dir, handoff_dir, &config, milestone)?;
    let now = chrono::Utc::now().to_rfc3339();
    let evaluation = evaluate(&facts.inputs(
        milestone,
        &config,
        record.as_ref(),
        CommandEvidence::Recorded,
        &now,
    ));
    Ok(evaluation.report)
}

/// The facts an evaluation needs that come from the project's files.
struct Facts {
    tasks: Vec<TaskFact>,
    requirements: std::result::Result<RequirementFacts, String>,
    secrets: crate::setup::doctor::Check,
}

impl Facts {
    fn inputs<'a>(
        &'a self,
        milestone: &'a str,
        config: &'a Config,
        record: Option<&'a ReleaseRecord>,
        evidence: CommandEvidence<'a>,
        now: &'a str,
    ) -> ReleaseInputs<'a> {
        ReleaseInputs {
            milestone,
            config,
            tasks: &self.tasks,
            requirements: self.requirements.as_ref().map_err(String::as_str),
            secrets: self.secrets.clone(),
            record,
            evidence,
            now,
        }
    }
}

fn gather(
    project_dir: &Path,
    handoff_dir: &Path,
    config: &Config,
    milestone: &str,
) -> Result<Facts> {
    Ok(Facts {
        tasks: milestone_tasks(&handoff_dir.join("tasks"), milestone)?,
        // Documents that cannot be read are a finding of the check, not a
        // reason to answer nothing about the tasks and the commands.
        requirements: milestone_requirements(handoff_dir, milestone).map_err(|e| format!("{e:#}")),
        secrets: check_secrets_untracked(project_dir, &config.onboarding.secret_file_patterns()),
    })
}

fn require_milestone(config: &Config, milestone: &str) -> Result<()> {
    if config.milestones.contains_key(milestone) {
        return Ok(());
    }
    let mut known: Vec<&str> = config.milestones.keys().map(String::as_str).collect();
    known.sort_unstable();
    if known.is_empty() {
        anyhow::bail!(
            "Milestone '{milestone}' not found: the project defines no milestone. Create one \
             with handoff_add_milestone."
        );
    }
    anyhow::bail!(
        "Milestone '{milestone}' not found. Defined milestones: {}.",
        known.join(", ")
    );
}

/// Every id in `confirm` must be a manual item the project configures: a
/// misspelled id would otherwise be recorded and never shown.
fn validate_confirmations(config: &Config, confirm: &[String]) -> Result<()> {
    let accepted: Vec<String> = config
        .release
        .manual_checks()
        .into_iter()
        .map(manual_item_id)
        .collect();
    for id in confirm {
        if accepted.contains(id) {
            continue;
        }
        if accepted.is_empty() {
            anyhow::bail!(
                "confirm: '{id}' cannot be confirmed: the project defines no manual check. \
                 Define them in `[release] manual_checks`."
            );
        }
        anyhow::bail!(
            "confirm: '{id}' is not a manual check of this project. Accepted: {}.",
            accepted.join(", ")
        );
    }
    Ok(())
}

/// The tasks scheduled on `milestone`, parents before their children. A task
/// without a milestone of its own belongs to its nearest ancestor's, so the
/// subtasks of a milestone task count even when only the parent names it.
///
/// A task is a leaf of the milestone when none of its descendants belongs to
/// the milestone: a parent whose subtasks were all moved to another milestone
/// is still this milestone's own work.
fn milestone_tasks(tasks_dir: &Path, milestone: &str) -> Result<Vec<TaskFact>> {
    let mut tasks = Vec::new();
    collect_tasks(tasks_dir, None, milestone, &mut tasks)?;
    Ok(tasks)
}

/// Adds the tasks under `dir` that belong to `milestone` to `out`.
fn collect_tasks(
    dir: &Path,
    inherited: Option<&str>,
    milestone: &str,
    out: &mut Vec<TaskFact>,
) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    let mut task_dirs = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_dir() && !name.starts_with('.') && !name.starts_with('_') {
            task_dirs.push(entry.path());
        }
    }
    task_dirs.sort();

    for task_dir in task_dirs {
        let Some((data, status)) = read_task(&task_dir)? else {
            continue;
        };
        let own = data
            .schedule
            .as_ref()
            .and_then(|schedule| schedule.milestone.as_deref())
            .filter(|name| !name.is_empty());
        let effective = own.or(inherited);
        let slot = (effective == Some(milestone)).then(|| {
            out.push(TaskFact {
                id: data.id.clone(),
                status,
                is_leaf: true,
                is_bug: data.labels.iter().any(|label| label == BUG_LABEL),
            });
            out.len() - 1
        });
        collect_tasks(&task_dir, effective, milestone, out)?;
        if let Some(slot) = slot {
            // Everything pushed after the task itself is one of its
            // descendants on the milestone.
            out[slot].is_leaf = out.len() == slot + 1;
        }
    }
    Ok(())
}

fn state_label(state: Option<ItemState>) -> &'static str {
    match state {
        Some(ItemState::Passing) => "passing",
        Some(ItemState::Uncovered) => "uncovered",
        Some(ItemState::NotRun) => "not_run",
        Some(ItemState::Blocked) => "blocked",
        Some(ItemState::Failing) => "failing",
        // A requirement the trace graph does not track has no verification.
        None => "untracked",
    }
}

/// The requirement items planned for `milestone` and their verification
/// state, read without writing anything (no resync, no cache refresh).
fn milestone_requirements(handoff_dir: &Path, milestone: &str) -> Result<RequirementFacts> {
    let read_only = load_trace_input_fully_read_only(handoff_dir, Vec::new())?;
    let graph = TraceGraph::build(&read_only.loaded.trace_input);
    let mut facts = RequirementFacts {
        unreadable: read_only
            .unreadable
            .iter()
            .map(|doc| doc.slug.clone())
            .collect(),
        ..RequirementFacts::default()
    };
    for doc in &read_only.loaded.docs {
        let Some(verification) = &doc.verification else {
            continue;
        };
        for sub in verification.items.iter().flat_map(|item| &item.sub_items) {
            let Some(id) = &sub.stable_id else {
                continue;
            };
            let layer = sub.layer.as_deref().or(doc.layer.as_deref());
            if layer != Some(REQUIREMENT_LAYER) || sub.effective_milestone(doc) != Some(milestone) {
                continue;
            }
            facts.total += 1;
            let state = graph.state(id);
            if state != Some(ItemState::Passing) {
                facts
                    .not_passing
                    .push((id.clone(), state_label(state).to_string()));
            }
        }
    }
    facts.not_passing.sort();
    Ok(facts)
}

pub fn handle(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let milestone = arguments
        .get("milestone")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow::anyhow!("`milestone` is required: the name of the milestone"))?;
    let results = parse_results(arguments.get("results"))?;
    let confirm = parse_confirm(arguments.get("confirm"))?;
    let report = run(&ReleaseCheckRequest {
        project_dir: &ctx.project_dir,
        milestone,
        results: &results,
        source: ResultSource::Reported,
        confirm: &confirm,
    })?;
    Ok(serde_json::to_string_pretty(&report)?)
}

fn parse_confirm(value: Option<&Value>) -> Result<Vec<String>> {
    let invalid = |got: &Value| {
        anyhow::anyhow!(
            "`confirm` must be an array of manual item ids (for example \
             [\"manual:docs_updated\"]), got {got}"
        )
    };
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().map(String::from).ok_or_else(|| invalid(item)))
            .collect(),
        Some(other) => Err(invalid(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    const COMMANDS: &str =
        "[commands]\ntest = \"npm test\"\nsmoke = \"./smoke.sh\"\naudit = \"npm audit\"\n";
    const MANUAL: &str = "[release]\nmanual_checks = [\"docs_updated\"]\n";

    struct Project {
        dir: tempfile::TempDir,
        ctx: HandlerContext,
    }

    impl Project {
        /// An initialized project with milestones `v1` and `v2`; `config_tail`
        /// is appended to its config.
        fn new(config_tail: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = std::fs::canonicalize(dir.path()).unwrap();
            let ctx = HandlerContext {
                agent_id: None,
                project_dir: root.clone(),
                handoff_dir: root.join(".handoff"),
            };
            let project = Self { dir, ctx };
            project.call("handoff_init", json!({ "project_name": "demo" }));
            let config = project.handoff().join("config.toml");
            let mut text = std::fs::read_to_string(&config).unwrap();
            text.push_str(&format!(
                "\n[milestones.v1]\n\n[milestones.v2]\n\n{config_tail}"
            ));
            std::fs::write(&config, text).unwrap();
            project
        }

        fn handoff(&self) -> PathBuf {
            self.ctx.handoff_dir.clone()
        }

        fn try_call(&self, tool: &str, arguments: Value) -> std::result::Result<String, String> {
            let response = super::super::handle_tool_call(&self.ctx, tool, &arguments);
            let result = response.result.expect("a tool result");
            let text = result["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if result["isError"].as_bool().unwrap_or(false) {
                Err(text)
            } else {
                Ok(text)
            }
        }

        fn call(&self, tool: &str, arguments: Value) -> String {
            self.try_call(tool, arguments.clone())
                .unwrap_or_else(|e| panic!("{tool}({arguments}) failed: {e}"))
        }

        fn check(&self, arguments: Value) -> Value {
            serde_json::from_str(&self.call("handoff_release_check", arguments)).unwrap()
        }

        fn check_err(&self, arguments: Value) -> String {
            self.try_call("handoff_release_check", arguments)
                .expect_err("the call must be rejected")
        }

        fn task(&self, id: &str, status: &str, milestone: Option<&str>, labels: &[&str]) {
            let mut task = json!({
                "id": id, "title": format!("Task {id}"), "status": status, "labels": labels,
                "schedule": { "estimate_hours": 1.0 },
            });
            if let Some(milestone) = milestone {
                task["schedule"]["milestone"] = json!(milestone);
            }
            self.call("handoff_update_task", json!({ "task": task }));
        }

        /// Adds a subtask of `parent`; ids are assigned in creation order
        /// (`<parent>.1`, `<parent>.2`, ...).
        fn subtask(&self, parent: &str, status: &str, milestone: Option<&str>) {
            let mut task = json!({
                "title": format!("Subtask of {parent}"), "status": status,
                "schedule": { "estimate_hours": 1.0 },
            });
            if let Some(milestone) = milestone {
                task["schedule"]["milestone"] = json!(milestone);
            }
            self.call(
                "handoff_update_task",
                json!({ "task": task, "parent_id": parent }),
            );
        }

        fn record_path(&self) -> PathBuf {
            self.handoff().join("release/v1.json")
        }

        fn record(&self) -> Value {
            serde_json::from_str(&std::fs::read_to_string(self.record_path()).unwrap()).unwrap()
        }

        /// Every file under `.handoff/` with its content.
        fn snapshot(&self) -> Vec<(PathBuf, Vec<u8>)> {
            let mut files = Vec::new();
            let mut stack = vec![self.handoff()];
            while let Some(current) = stack.pop() {
                for entry in std::fs::read_dir(&current).unwrap() {
                    let path = entry.unwrap().path();
                    if path.is_dir() {
                        stack.push(path);
                    } else {
                        files.push((path.clone(), std::fs::read(&path).unwrap()));
                    }
                }
            }
            files.sort();
            files
        }
    }

    fn item<'a>(report: &'a Value, id: &str) -> &'a Value {
        report["items"]
            .as_array()
            .expect("items")
            .iter()
            .find(|item| item["id"] == json!(id))
            .unwrap_or_else(|| panic!("no item `{id}` in {report}"))
    }

    fn status<'a>(report: &'a Value, id: &str) -> &'a str {
        item(report, id)["status"].as_str().expect("status")
    }

    fn passing_results() -> Value {
        json!([
            { "id": "tests_pass", "command": "npm test", "exit_code": 0, "duration_ms": 1500 },
            { "id": "smoke_pass", "command": "./smoke.sh", "exit_code": 0 },
            { "id": "dependencies_audited", "command": "npm audit", "exit_code": 0 },
        ])
    }

    // --- requests that cannot be honored ---

    // SPEC-ONB-040 AC5
    #[test]
    fn an_unknown_milestone_is_an_error_that_names_the_known_ones() {
        let project = Project::new(COMMANDS);
        let err = project.check_err(json!({ "milestone": "v9" }));
        assert!(err.contains("Milestone 'v9' not found"), "{err}");
        assert!(err.contains("v1, v2"), "{err}");
        assert!(!project.handoff().join("release").exists());

        let err = project.check_err(json!({}));
        assert!(err.contains("`milestone` is required"), "{err}");
        let err = project.check_err(json!({ "milestone": "" }));
        assert!(err.contains("`milestone` is required"), "{err}");
    }

    #[test]
    fn a_project_without_milestones_says_how_to_create_one() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join(".handoff")).unwrap();
        std::fs::write(
            root.join(".handoff/config.toml"),
            "[project]\nname = \"demo\"\n",
        )
        .unwrap();
        let err = run(&ReleaseCheckRequest {
            project_dir: &root,
            milestone: "v1",
            results: &[],
            source: ResultSource::Reported,
            confirm: &[],
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("handoff_add_milestone"), "{err}");
    }

    #[test]
    fn a_project_that_is_not_initialized_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = run(&ReleaseCheckRequest {
            project_dir: dir.path(),
            milestone: "v1",
            results: &[],
            source: ResultSource::Reported,
            confirm: &[],
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("handoff_init"), "{err}");
        assert!(!dir.path().join(".handoff").exists());
    }

    #[test]
    fn malformed_results_and_confirmations_are_rejected_and_record_nothing() {
        let project = Project::new(&format!("{COMMANDS}{MANUAL}"));
        let before = project.snapshot();
        for (arguments, expected) in [
            (
                json!({ "milestone": "v1", "results": [{ "id": "cmd_test", "command": "npm test", "exit_code": 0 }] }),
                "is not a command check",
            ),
            (
                json!({ "milestone": "v1", "results": [{ "id": "tests_pass", "command": "npm test" }] }),
                "give `exit_code`",
            ),
            (
                json!({ "milestone": "v1", "results": [
                    { "id": "tests_pass", "command": "npm test", "exit_code": 0 },
                    { "id": "tests_pass", "command": "npm test", "exit_code": 1 },
                ] }),
                "more than one result",
            ),
            (
                json!({ "milestone": "v1", "results": { "id": "tests_pass" } }),
                "`results` must be an array",
            ),
            (
                json!({ "milestone": "v1", "confirm": ["manual:docs_update"] }),
                "is not a manual check of this project. Accepted: manual:docs_updated",
            ),
            (
                json!({ "milestone": "v1", "confirm": ["docs_updated"] }),
                "is not a manual check",
            ),
            (
                json!({ "milestone": "v1", "confirm": "manual:docs_updated" }),
                "`confirm` must be an array",
            ),
            (
                json!({ "milestone": "v1", "confirm": [1] }),
                "`confirm` must be an array",
            ),
            // A good result next to a bad confirmation: nothing is recorded.
            (
                json!({ "milestone": "v1", "results": passing_results(), "confirm": ["manual:nope"] }),
                "is not a manual check",
            ),
            // Misspelled arguments are not dropped.
            (
                json!({ "milestone": "v1", "result": passing_results() }),
                "unknown argument(s): result",
            ),
            (
                json!({ "milestone": "v1", "confirmed": ["manual:docs_updated"] }),
                "unknown argument(s): confirmed",
            ),
        ] {
            let err = project.check_err(arguments.clone());
            assert!(err.contains(expected), "{arguments}: {err}");
            assert_eq!(project.snapshot(), before, "{arguments} wrote something");
        }
    }

    #[test]
    fn confirming_without_manual_checks_configured_says_where_to_define_them() {
        let project = Project::new(COMMANDS);
        let err = project.check_err(json!({ "milestone": "v1", "confirm": ["manual:x"] }));
        assert!(err.contains("[release] manual_checks"), "{err}");
    }

    // --- nothing passed in: nothing written (SPEC-ONB-040 AC3) ---

    #[test]
    fn a_call_without_results_skips_the_commands_and_writes_nothing() {
        let project = Project::new(&format!("{COMMANDS}{MANUAL}"));
        project.task("t1", "done", Some("v1"), &[]);
        let before = project.snapshot();
        let report = project.check(json!({ "milestone": "v1" }));
        assert_eq!(report["milestone"], json!("v1"));
        assert_eq!(report["ok"], json!(false));
        assert_eq!(status(&report, "tasks_done"), "pass");
        assert_eq!(status(&report, "tests_pass"), "skip");
        assert_eq!(status(&report, "smoke_pass"), "skip");
        assert_eq!(status(&report, "dependencies_audited"), "skip");
        assert_eq!(status(&report, "requirements_verified"), "skip");
        assert_eq!(status(&report, "secrets_untracked"), "skip");
        assert_eq!(status(&report, "manual:docs_updated"), "manual");
        assert_eq!(
            report["commands_to_run"],
            json!([
                { "id": "tests_pass", "command": "npm test", "timeout_sec": 600 },
                { "id": "smoke_pass", "command": "./smoke.sh", "timeout_sec": 600 },
                { "id": "dependencies_audited", "command": "npm audit", "timeout_sec": 600 },
            ])
        );
        assert_eq!(project.snapshot(), before, "a plain check must not write");
    }

    // --- the record (SPEC-ONB-040 AC6) ---

    #[test]
    fn reported_results_are_judged_and_recorded() {
        let project = Project::new(COMMANDS);
        let report = project.check(json!({
            "milestone": "v1",
            "results": [
                { "id": "tests_pass", "command": "npm test", "exit_code": 0, "duration_ms": 1500 },
                { "id": "smoke_pass", "command": "./smoke.sh", "exit_code": 7, "output_tail": "boom\n" },
                { "id": "dependencies_audited", "command": "npm audit", "error": "timed out after 600s" },
            ]
        }));
        assert_eq!(status(&report, "tests_pass"), "pass");
        assert_eq!(item(&report, "tests_pass")["source"], json!("reported"));
        assert_eq!(status(&report, "smoke_pass"), "fail");
        assert_eq!(status(&report, "dependencies_audited"), "fail");
        assert_eq!(report["commands_to_run"], json!([]));

        let record = project.record();
        assert_eq!(record["milestone"], json!("v1"));
        let checked_at = item(&report, "tests_pass")["checked_at"].clone();
        assert!(chrono::DateTime::parse_from_rfc3339(checked_at.as_str().unwrap()).is_ok());
        assert_eq!(record["updated_at"], checked_at);
        assert_eq!(
            record["commands"]["tests_pass"],
            json!({
                "status": "pass", "command": "npm test", "exit_code": 0, "source": "reported",
                "checked_at": checked_at, "duration_ms": 1500, "detail": "exit code 0",
            })
        );
        let smoke = &record["commands"]["smoke_pass"];
        assert_eq!(smoke["status"], json!("fail"));
        assert_eq!(smoke["exit_code"], json!(7));
        assert_eq!(smoke["source"], json!("reported"));
        assert!(smoke["detail"].as_str().unwrap().contains("boom"));
        let audit = &record["commands"]["dependencies_audited"];
        assert_eq!(audit["status"], json!("fail"));
        assert!(audit.get("exit_code").is_none(), "{audit}");
        assert!(audit["detail"].as_str().unwrap().contains("timed out"));
        assert!(record.get("manual").is_none());
    }

    /// SPEC-ONB-040: a call without results does not fall back on the record.
    #[test]
    fn a_later_call_without_results_does_not_use_or_touch_the_record() {
        let project = Project::new(COMMANDS);
        let first = project.check(json!({ "milestone": "v1", "results": passing_results() }));
        assert_eq!(status(&first, "tests_pass"), "pass");
        let recorded = std::fs::read(project.record_path()).unwrap();

        let second = project.check(json!({ "milestone": "v1" }));
        for id in ["tests_pass", "smoke_pass", "dependencies_audited"] {
            assert_eq!(status(&second, id), "skip", "{id}");
            assert!(item(&second, id).get("checked_at").is_none(), "{id}");
        }
        assert_eq!(second["commands_to_run"].as_array().unwrap().len(), 3);
        assert_eq!(std::fs::read(project.record_path()).unwrap(), recorded);
    }

    #[test]
    fn results_of_separate_calls_accumulate_and_a_newer_one_replaces_the_older() {
        let project = Project::new(COMMANDS);
        project.check(json!({ "milestone": "v1", "results": [
            { "id": "tests_pass", "command": "npm test", "exit_code": 1 },
        ] }));
        project.check(json!({ "milestone": "v1", "results": [
            { "id": "smoke_pass", "command": "./smoke.sh", "exit_code": 0 },
        ] }));
        let record = project.record();
        assert_eq!(record["commands"]["tests_pass"]["status"], json!("fail"));
        assert_eq!(record["commands"]["smoke_pass"]["status"], json!("pass"));

        project.check(json!({ "milestone": "v1", "results": [
            { "id": "tests_pass", "command": "npm test", "exit_code": 0 },
        ] }));
        let record = project.record();
        assert_eq!(record["commands"]["tests_pass"]["status"], json!("pass"));
        assert_eq!(record["commands"]["smoke_pass"]["status"], json!("pass"));
        // Each milestone has its own record.
        assert!(!project.handoff().join("release/v2.json").exists());
    }

    #[test]
    fn a_result_for_another_command_is_not_recorded() {
        let project = Project::new(COMMANDS);
        project.check(json!({ "milestone": "v1", "results": [
            { "id": "tests_pass", "command": "npm test", "exit_code": 0 },
        ] }));
        let recorded = std::fs::read(project.record_path()).unwrap();
        let report = project.check(json!({ "milestone": "v1", "results": [
            { "id": "tests_pass", "command": "true", "exit_code": 0 },
        ] }));
        assert_eq!(status(&report, "tests_pass"), "fail");
        assert_eq!(
            std::fs::read(project.record_path()).unwrap(),
            recorded,
            "the last usable result stays on record"
        );
    }

    // --- manual items (SPEC-ONB-040 AC4) ---

    #[test]
    fn a_manual_item_is_manual_until_confirmed_and_passes_afterwards() {
        let project = Project::new(&format!("{COMMANDS}{MANUAL}"));
        let before = project.check(json!({ "milestone": "v1" }));
        assert_eq!(status(&before, "manual:docs_updated"), "manual");

        let confirming =
            project.check(json!({ "milestone": "v1", "confirm": ["manual:docs_updated"] }));
        assert_eq!(status(&confirming, "manual:docs_updated"), "pass");
        let confirmed_at =
            project.record()["manual"]["manual:docs_updated"]["confirmed_at"].clone();
        assert!(chrono::DateTime::parse_from_rfc3339(confirmed_at.as_str().unwrap()).is_ok());
        assert_eq!(
            item(&confirming, "manual:docs_updated")["checked_at"],
            confirmed_at
        );

        // A later call, with nothing passed in, still sees the confirmation.
        let after = project.check(json!({ "milestone": "v1" }));
        assert_eq!(status(&after, "manual:docs_updated"), "pass");
        assert_eq!(
            item(&after, "manual:docs_updated")["checked_at"],
            confirmed_at
        );
        // The other milestone is not confirmed by it.
        let other = project.check(json!({ "milestone": "v2" }));
        assert_eq!(status(&other, "manual:docs_updated"), "manual");
    }

    #[test]
    fn confirmations_and_command_results_do_not_overwrite_each_other() {
        let project = Project::new(&format!("{COMMANDS}{MANUAL}"));
        project.check(json!({ "milestone": "v1", "results": passing_results() }));
        project.check(json!({ "milestone": "v1", "confirm": ["manual:docs_updated"] }));
        let record = project.record();
        assert_eq!(record["commands"].as_object().unwrap().len(), 3);
        assert!(record["manual"]["manual:docs_updated"].is_object());

        project.check(json!({ "milestone": "v1", "results": [
            { "id": "tests_pass", "command": "npm test", "exit_code": 2 },
        ] }));
        let record = project.record();
        assert_eq!(record["commands"]["tests_pass"]["status"], json!("fail"));
        assert!(record["manual"]["manual:docs_updated"].is_object());
    }

    #[test]
    fn a_broken_record_is_reported_and_left_alone() {
        let project = Project::new(&format!("{COMMANDS}{MANUAL}"));
        std::fs::create_dir_all(project.handoff().join("release")).unwrap();
        std::fs::write(project.record_path(), "{ broken").unwrap();
        for arguments in [
            json!({ "milestone": "v1" }),
            json!({ "milestone": "v1", "results": passing_results() }),
            json!({ "milestone": "v1", "confirm": ["manual:docs_updated"] }),
        ] {
            let err = project.check_err(arguments);
            assert!(err.contains("repair or delete"), "{err}");
            assert_eq!(std::fs::read(project.record_path()).unwrap(), b"{ broken");
        }
    }

    // --- ok ---

    #[test]
    fn ok_becomes_true_when_every_item_passes() {
        let project = Project::new(&format!("{COMMANDS}{MANUAL}"));
        project.task("t1", "done", Some("v1"), &[]);
        requirement_docs(&project, "v1");
        project.call(
            "handoff_trace_record",
            json!({ "results": [{ "item": "AT-1", "result": "pass" }] }),
        );
        git(&project.ctx.project_dir, &["init", "-q"]);
        let report = project.check(json!({
            "milestone": "v1",
            "results": passing_results(),
            "confirm": ["manual:docs_updated"],
        }));
        let statuses: Vec<(&str, &str)> = report["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| {
                (
                    item["id"].as_str().unwrap(),
                    item["status"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            statuses,
            vec![
                ("tasks_done", "pass"),
                ("no_open_bugs", "pass"),
                ("requirements_verified", "pass"),
                ("tests_pass", "pass"),
                ("smoke_pass", "pass"),
                ("secrets_untracked", "pass"),
                ("dependencies_audited", "pass"),
                ("manual:docs_updated", "pass"),
            ]
        );
        assert_eq!(report["ok"], json!(true));

        // SPEC-ONB-092 AC3: the same project, one command not reported.
        let report = project.check(json!({
            "milestone": "v1",
            "results": [
                { "id": "tests_pass", "command": "npm test", "exit_code": 0 },
                { "id": "smoke_pass", "command": "./smoke.sh", "exit_code": 0 },
            ],
        }));
        assert_eq!(status(&report, "dependencies_audited"), "skip");
        assert_eq!(report["ok"], json!(false));
    }

    // --- tasks (SPEC-ONB-040 AC2) ---

    #[test]
    fn unfinished_tasks_of_the_milestone_fail_tasks_done() {
        let project = Project::new(COMMANDS);
        project.task("t1", "done", Some("v1"), &[]);
        project.task("t2", "in_progress", Some("v1"), &[]);
        project.task("t3", "todo", Some("v2"), &[]);
        project.task("t4", "todo", None, &[]);
        let report = project.check(json!({ "milestone": "v1" }));
        assert_eq!(status(&report, "tasks_done"), "fail");
        assert_eq!(item(&report, "tasks_done")["refs"], json!(["t2"]));

        let other = project.check(json!({ "milestone": "v2" }));
        assert_eq!(item(&other, "tasks_done")["refs"], json!(["t3"]));
    }

    #[test]
    fn subtasks_belong_to_their_parents_milestone_unless_they_name_another() {
        let project = Project::new(COMMANDS);
        // The parent names the milestone; its children do not.
        project.task("t1", "in_progress", Some("v1"), &[]);
        project.subtask("t1", "done", None); // t1.1
        project.subtask("t1", "todo", None); // t1.2
        project.subtask("t1.2", "todo", None); // t1.2.1
                                               // A child moved to another milestone.
        project.subtask("t1", "todo", Some("v2")); // t1.3
        assert!(project
            .handoff()
            .join("tasks/t1-task-t1/t1.2-subtask-of-t1/t1.2.1-subtask-of-t1-2")
            .is_dir());
        let report = project.check(json!({ "milestone": "v1" }));
        assert_eq!(status(&report, "tasks_done"), "fail");
        assert_eq!(
            item(&report, "tasks_done")["refs"],
            json!(["t1.2.1"]),
            "only unfinished leaves: not the parents t1 and t1.2, not t1.3 of v2"
        );
        let other = project.check(json!({ "milestone": "v2" }));
        assert_eq!(item(&other, "tasks_done")["refs"], json!(["t1.3"]));
    }

    /// A task scheduled on the milestone whose subtasks all belong elsewhere
    /// is the milestone's own unfinished work: it must not vanish from the
    /// check because it is a parent.
    #[test]
    fn a_task_with_no_subtask_on_the_milestone_counts_as_its_leaf() {
        let project = Project::new(COMMANDS);
        project.task("t1", "in_progress", Some("v1"), &[]);
        project.subtask("t1", "todo", Some("v2")); // t1.1
        let report = project.check(json!({ "milestone": "v1" }));
        assert_eq!(status(&report, "tasks_done"), "fail");
        assert_eq!(item(&report, "tasks_done")["refs"], json!(["t1"]));

        // A grandchild back on the milestone makes t1 a parent again.
        project.subtask("t1.1", "todo", Some("v1")); // t1.1.1
        let report = project.check(json!({ "milestone": "v1" }));
        assert_eq!(item(&report, "tasks_done")["refs"], json!(["t1.1.1"]));
        let other = project.check(json!({ "milestone": "v2" }));
        assert_eq!(item(&other, "tasks_done")["refs"], json!(["t1.1"]));
    }

    #[test]
    fn an_open_bug_task_of_the_milestone_fails_no_open_bugs() {
        let project = Project::new(COMMANDS);
        project.task("t1", "done", Some("v1"), &["bug"]);
        project.task("t2", "blocked", Some("v1"), &["bug", "ui"]);
        project.task("t3", "todo", Some("v1"), &["debug"]);
        project.task("t4", "todo", Some("v2"), &["bug"]);
        let report = project.check(json!({ "milestone": "v1" }));
        assert_eq!(status(&report, "no_open_bugs"), "fail");
        assert_eq!(item(&report, "no_open_bugs")["refs"], json!(["t2"]));
    }

    // --- requirements ---

    /// A requirement planned for `milestone` with one acceptance test and an
    /// implementing task (without both, the requirement counts as uncovered),
    /// and a second requirement without a milestone.
    fn requirement_docs(project: &Project, milestone: &str) {
        project.call(
            "handoff_doc_save",
            json!({
                "slug": "reqs", "title": "Requirements", "layer": "requirement",
                "body": format!(
                    "# Requirements\n\n### REQ-1 Login\n\n- milestone: {milestone}\n\nA user can log in.\n\n\
                     ### REQ-2 Later\n\nNot planned yet.\n"
                ),
            }),
        );
        project.call(
            "handoff_doc_save",
            json!({
                "slug": "acceptance", "title": "Acceptance", "layer": "acceptance",
                "body": "# Acceptance\n\n### AT-1 Login works\n\n- verifies: REQ-1\n\nLog in.\n",
            }),
        );
        project.call(
            "handoff_update_task",
            json!({ "task": {
                "id": "impl", "title": "Implement login", "status": "done",
                "schedule": { "estimate_hours": 1.0 }, "requirement_ids": ["REQ-1"],
            } }),
        );
    }

    #[test]
    fn requirements_of_the_milestone_must_be_passing() {
        let project = Project::new(COMMANDS);
        requirement_docs(&project, "v1");
        let before = project.check(json!({ "milestone": "v1" }));
        let requirements = item(&before, "requirements_verified");
        assert_eq!(requirements["status"], json!("fail"));
        assert_eq!(requirements["refs"], json!(["REQ-1"]));
        assert!(
            requirements["detail"]
                .as_str()
                .unwrap()
                .contains("REQ-1 (not_run)"),
            "{requirements}"
        );

        project.call(
            "handoff_trace_record",
            json!({ "results": [{ "item": "AT-1", "result": "fail" }] }),
        );
        let failing = project.check(json!({ "milestone": "v1" }));
        assert!(item(&failing, "requirements_verified")["detail"]
            .as_str()
            .unwrap()
            .contains("REQ-1 (failing)"));

        project.call(
            "handoff_trace_record",
            json!({ "results": [{ "item": "AT-1", "result": "pass" }] }),
        );
        let passing = project.check(json!({ "milestone": "v1" }));
        assert_eq!(status(&passing, "requirements_verified"), "pass");
        assert!(item(&passing, "requirements_verified")
            .get("refs")
            .is_none());

        // REQ-2 has no milestone and v2 has no requirement: not checked.
        let other = project.check(json!({ "milestone": "v2" }));
        assert_eq!(status(&other, "requirements_verified"), "skip");
    }

    #[test]
    fn a_documents_milestone_applies_to_its_items() {
        let project = Project::new(COMMANDS);
        project.call(
            "handoff_doc_save",
            json!({
                "slug": "reqs", "title": "Requirements", "layer": "requirement", "milestone": "v2",
                "body": "# Requirements\n\n### REQ-1 A\n\nA.\n\n### REQ-2 B\n\n- milestone: v1\n\nB.\n",
            }),
        );
        let v2 = project.check(json!({ "milestone": "v2" }));
        assert_eq!(item(&v2, "requirements_verified")["refs"], json!(["REQ-1"]));
        let v1 = project.check(json!({ "milestone": "v1" }));
        assert_eq!(item(&v1, "requirements_verified")["refs"], json!(["REQ-2"]));
    }

    #[test]
    fn checking_requirements_writes_nothing() {
        let project = Project::new(COMMANDS);
        requirement_docs(&project, "v1");
        project.call(
            "handoff_trace_record",
            json!({ "results": [{ "item": "AT-1", "result": "pass" }] }),
        );
        // Edit a document behind the tool's back: a read must not resync it.
        let doc = project.handoff().join("docs/_doc.reqs.md");
        let mut text = std::fs::read_to_string(&doc).unwrap();
        text.push_str("\n### REQ-3 Added by hand\n\n- milestone: v1\n\nNew.\n");
        std::fs::write(&doc, text).unwrap();
        let before = project.snapshot();
        let report = project.check(json!({ "milestone": "v1" }));
        assert_eq!(
            item(&report, "requirements_verified")["refs"],
            json!(["REQ-3"]),
            "the edit is seen"
        );
        assert_eq!(project.snapshot(), before, "but nothing is written");
    }

    // --- secrets ---

    fn git(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("git must be installed to run these tests");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // SPEC-ONB-050 AC1, AC2, AC4, AC7
    #[test]
    fn a_tracked_secrets_file_fails_the_release_check() {
        let project = Project::new(COMMANDS);
        let root = project.ctx.project_dir.clone();
        let outside = project.check(json!({ "milestone": "v1" }));
        assert_eq!(status(&outside, "secrets_untracked"), "skip");

        git(&root, &["init", "-q"]);
        std::fs::write(root.join(".env"), "TOKEN=x\n").unwrap();
        std::fs::write(root.join(".gitignore"), ".env\n").unwrap();
        git(&root, &["add", ".gitignore"]);
        let ignored = project.check(json!({ "milestone": "v1" }));
        assert_eq!(status(&ignored, "secrets_untracked"), "pass");

        git(&root, &["add", "-f", ".env"]);
        let tracked = project.check(json!({ "milestone": "v1" }));
        let secrets = item(&tracked, "secrets_untracked");
        assert_eq!(secrets["status"], json!("fail"));
        let detail = secrets["detail"].as_str().unwrap();
        assert!(detail.contains("`.env`"), "{detail}");
        assert!(detail.contains("contents were not scanned"), "{detail}");

        // AC7: configured patterns replace the defaults.
        let config = project.handoff().join("config.toml");
        let mut text = std::fs::read_to_string(&config).unwrap();
        text.push_str("\n[onboarding]\nsecret_file_patterns = [\"*.tfstate\"]\n");
        std::fs::write(&config, text).unwrap();
        let custom = project.check(json!({ "milestone": "v1" }));
        assert_eq!(status(&custom, "secrets_untracked"), "pass");
    }

    // --- who ran the commands ---

    #[test]
    fn results_run_by_the_cli_are_recorded_as_executed() {
        let project = Project::new(COMMANDS);
        let results: Vec<CommandResult> = serde_json::from_value(passing_results()).unwrap();
        let report = run(&ReleaseCheckRequest {
            project_dir: &project.ctx.project_dir,
            milestone: "v1",
            results: &results,
            source: ResultSource::Executed,
            confirm: &[],
        })
        .unwrap();
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(item(&json, "tests_pass")["source"], json!("executed"));
        assert_eq!(
            project.record()["commands"]["tests_pass"]["source"],
            json!("executed")
        );
    }

    /// The temporary directory outlives every assertion above.
    #[test]
    fn the_fixture_is_a_real_initialized_project() {
        let project = Project::new("");
        assert!(project.dir.path().join(".handoff/config.toml").is_file());
    }
}
