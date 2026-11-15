//! `handoff_next_step` (SPEC-ONB-011): the state of the project and the one
//! operation to recommend.
//!
//! The decision is `crate::context::next_step`; this module answers its
//! questions from the project's files. It writes nothing and starts no
//! process: `.handoff/` is found by reading files (like `handoff_doctor`),
//! and the readiness diagnosis it consults is the one that never runs a
//! command.
//!
//! `handoff_load_context` embeds the same answer as its `next_step` key when
//! `[onboarding] next_step` is on (see [`for_project`]).

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::Value;

use super::brainstorm::finalized_count;
use super::HandlerContext;
use crate::context::next_step::{
    next_step, FailedCheck, MilestoneFact, NextStep, Planning, Project, Readiness, SessionFact,
    TaskFact, Work,
};
use crate::setup::doctor::{
    diagnose, locate_handoff_dir, CheckStatus, DoctorRequest, ResultSource,
};
use crate::storage::config::read_config;
use crate::storage::docs::layer::REQUIREMENT_LAYER;
use crate::storage::docs::DocSet;
use crate::storage::sessions::read_session_entries;
use crate::storage::tasks::collect_all_tasks;

/// Status of a session somebody is working in.
const SESSION_ACTIVE: &str = "active";

/// Name suggested for a project whose directory has none (a file system
/// root).
const UNNAMED_PROJECT: &str = "project";

/// A project as its files describe it.
struct ProjectOnDisk<'a> {
    project_dir: &'a Path,
    /// The project's `.handoff/`, when it has one.
    handoff_dir: Option<PathBuf>,
}

impl ProjectOnDisk<'_> {
    /// The `.handoff/` directory. Only asked for once the project is known
    /// to be initialized.
    fn handoff(&self) -> Result<&Path> {
        self.handoff_dir.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "No .handoff/ directory found in {}",
                self.project_dir.display()
            )
        })
    }
}

impl Project for ProjectOnDisk<'_> {
    fn name(&self) -> String {
        self.project_dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| UNNAMED_PROJECT.to_string())
    }

    fn initialized(&self) -> bool {
        self.handoff_dir.is_some()
    }

    fn readiness(&self) -> Result<Readiness> {
        let home_dir = crate::storage::home_dir().map(PathBuf::from);
        // The diagnosis without results: nothing is run, the command checks
        // are skipped and a skip does not lower `ready`.
        let report = diagnose(&DoctorRequest {
            project_dir: self.project_dir,
            home_dir: home_dir.as_deref(),
            checks: None,
            results: &[],
            source: ResultSource::Reported,
            run_requested: false,
        })?;
        Ok(Readiness {
            ready: report.ready,
            failed: report
                .checks
                .into_iter()
                .filter(|check| check.status == CheckStatus::Fail)
                .map(|check| FailedCheck {
                    id: check.id.to_string(),
                    detail: check.detail,
                    // A failed check always says what to do; should one not,
                    // the decision points at `handoff_doctor` instead.
                    next_step: check.next_step.unwrap_or_default(),
                })
                .collect(),
        })
    }

    fn work(&self) -> Result<Work> {
        let handoff = self.handoff()?;
        let mut tasks = Vec::new();
        collect_all_tasks(&handoff.join("tasks"), &mut tasks)?;
        let active_sessions = read_session_entries(&handoff.join("sessions"))?
            .into_iter()
            .filter(|entry| entry.status == SESSION_ACTIVE)
            .map(|entry| SessionFact {
                // `read_session_entries` gives every entry an id (a session
                // file without one gets it from its file name).
                id: entry.data.id.unwrap_or_default(),
                summary: entry.data.summary,
            })
            .collect();
        Ok(Work {
            tasks: tasks
                .into_iter()
                .map(|(data, status)| TaskFact {
                    id: data.id,
                    title: data.title,
                    status,
                    dependencies: data.dependencies,
                })
                .collect(),
            active_sessions,
        })
    }

    fn planning(&self) -> Result<Planning> {
        let handoff = self.handoff()?;
        let docs = DocSet::load(handoff)?;
        let requirement_items = docs
            .docs()
            .iter()
            .flat_map(|doc| {
                doc.verification
                    .iter()
                    .flat_map(|verification| &verification.items)
                    .flat_map(|item| &item.sub_items)
                    .filter(|sub| sub.stable_id.is_some())
                    .filter(|sub| {
                        sub.layer.as_deref().or(doc.layer.as_deref()) == Some(REQUIREMENT_LAYER)
                    })
            })
            .count();
        // A document that cannot be read may be the one holding the
        // requirements: "there are none" cannot be said without it.
        if requirement_items == 0 && !docs.unreadable().is_empty() {
            let slugs: Vec<&str> = docs
                .unreadable()
                .iter()
                .map(|doc| doc.slug.as_str())
                .collect();
            anyhow::bail!(
                "{} document(s) could not be read, so whether the project has requirement \
                 items is unknown: {}. Repair them with handoff_doc_repair_frontmatter",
                slugs.len(),
                slugs.join(", ")
            );
        }
        Ok(Planning {
            brainstorm_reports: finalized_count(handoff)?,
            requirement_items,
        })
    }

    fn milestones(&self) -> Result<Vec<MilestoneFact>> {
        let config = read_config(&self.handoff()?.join("config.toml"))?;
        let mut milestones: Vec<(Option<i64>, MilestoneFact)> = config
            .milestones
            .iter()
            .map(|(name, milestone)| {
                (
                    milestone.order,
                    MilestoneFact {
                        name: name.clone(),
                        status: milestone.effective_status().to_string(),
                    },
                )
            })
            .collect();
        // The order `handoff_list_milestones` shows them in: by `order`,
        // unordered ones last, ties by name.
        milestones.sort_by(|(a_order, a), (b_order, b)| {
            (a_order.is_none(), a_order, &a.name).cmp(&(b_order.is_none(), b_order, &b.name))
        });
        Ok(milestones
            .into_iter()
            .map(|(_, milestone)| milestone)
            .collect())
    }
}

/// The next step for the project at `project_dir`, whose `.handoff/` is
/// `handoff_dir` (`None` for a project that is not initialized).
pub(crate) fn for_project(project_dir: &Path, handoff_dir: Option<&Path>) -> Result<NextStep> {
    next_step(&ProjectOnDisk {
        project_dir,
        handoff_dir: handoff_dir.map(Path::to_path_buf),
    })
}

pub fn handle(ctx: &HandlerContext, _arguments: &Value) -> Result<String> {
    let handoff_dir = locate_handoff_dir(&ctx.project_dir);
    let step = for_project(&ctx.project_dir, handoff_dir.as_deref())?;
    Ok(serde_json::to_string_pretty(&step)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const READY_CLAUDE_MD: &str = "# Demo\n\n## Session Handoff\n\nx\n\n## Build & Test\n\n- test: `true`\n\n## Coding Rules\n\nr\n\n## Project Structure\n\ns\n\n## Documentation Contract\n\nd\n";

    struct Fixture {
        _dir: tempfile::TempDir,
        ctx: HandlerContext,
    }

    impl Fixture {
        /// An initialized project that passes the readiness diagnosis;
        /// `config_tail` is appended to its config.
        fn ready(config_tail: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = std::fs::canonicalize(dir.path()).unwrap();
            let ctx = HandlerContext {
                agent_id: None,
                project_dir: root.clone(),
                handoff_dir: root.join(".handoff"),
            };
            let fixture = Self { _dir: dir, ctx };
            fixture.call("handoff_init", json!({ "project_name": "demo" }));
            let config = root.join(".handoff/config.toml");
            let mut text = std::fs::read_to_string(&config).unwrap();
            text.push_str(&format!("\n[commands]\ntest = \"true\"\n\n{config_tail}"));
            std::fs::write(&config, text).unwrap();
            std::fs::write(root.join("CLAUDE.md"), READY_CLAUDE_MD).unwrap();
            fixture
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

        fn step(&self) -> Value {
            serde_json::from_str(&self.call("handoff_next_step", json!({}))).unwrap()
        }

        fn doc(&self, slug: &str, layer: &str, body: &str) {
            self.call(
                "handoff_doc_save",
                json!({ "slug": slug, "title": slug, "layer": layer, "body": body }),
            );
        }

        fn done_task(&self) {
            self.call(
                "handoff_update_task",
                json!({ "task": {
                    "id": "t1", "title": "One", "status": "done",
                    "schedule": { "estimate_hours": 1.0 },
                } }),
            );
        }
    }

    const REQUIREMENT_BODY: &str = "# Requirements\n\n### REQ-1 Login\n\nA user can log in.\n";

    /// Only items of the requirement layer count as requirements.
    #[test]
    fn items_of_other_layers_are_not_requirement_items() {
        let fixture = Fixture::ready("");
        fixture.doc(
            "acceptance",
            "acceptance",
            "# Acceptance\n\n### AT-1 Login works\n\nLog in.\n",
        );
        fixture.call(
            "handoff_doc_save",
            json!({ "slug": "notes", "title": "Notes", "body": "# Notes\n\n### REQ-9 Not an item\n\nx\n" }),
        );
        let step = fixture.step();
        assert_eq!(step["state"], json!("no_brainstorm"), "{step}");
        assert_eq!(step["facts"]["requirement_items"], json!(0));

        fixture.doc("reqs", "requirement", REQUIREMENT_BODY);
        let step = fixture.step();
        assert_eq!(step["facts"]["requirement_items"], json!(1), "{step}");
        assert_ne!(step["state"], json!("no_brainstorm"));
    }

    /// A document that cannot be read may hold the requirements: the tool
    /// says so instead of sending the user back to a brainstorm.
    #[test]
    fn an_unreadable_document_is_an_error_when_no_requirement_item_is_found() {
        let fixture = Fixture::ready("");
        let docs = fixture.ctx.handoff_dir.join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(
            docs.join("_doc.broken.md"),
            "---\nid: [unterminated\n---\n# Broken\n",
        )
        .unwrap();
        let error = fixture
            .try_call("handoff_next_step", json!({}))
            .expect_err("unknown is not zero");
        assert!(
            error.contains("broken") && error.contains("handoff_doc_repair_frontmatter"),
            "{error}"
        );

        // With a requirement item on file the answer does not depend on it.
        fixture.doc("reqs", "requirement", REQUIREMENT_BODY);
        fixture.done_task();
        assert_eq!(fixture.step()["state"], json!("all_done"));
    }

    /// Milestones are offered in the order `handoff_list_milestones` shows
    /// them: by `order`, the unordered ones last, ties by name.
    #[test]
    fn the_milestone_offered_for_release_follows_the_display_order() {
        let fixture = Fixture::ready(
            "[milestones.alpha]\n\n[milestones.zeta]\norder = 1\n\n[milestones.beta]\norder = 2\n\n\
             [milestones.first]\norder = 0\nstatus = \"released\"\n\n[milestones.gone]\norder = 0\nstatus = \"cancelled\"\n",
        );
        fixture.doc("reqs", "requirement", REQUIREMENT_BODY);
        fixture.done_task();
        let step = fixture.step();
        assert_eq!(step["state"], json!("release_ready"), "{step}");
        assert_eq!(
            step["facts"]["milestones"],
            json!({ "total": 5, "unreleased": ["zeta", "beta", "alpha"] })
        );
        assert_eq!(
            step["action"]["command"],
            json!("handoff_release_check(milestone: \"zeta\")")
        );
        let listed: Value =
            serde_json::from_str(&fixture.call("handoff_list_milestones", json!({}))).unwrap();
        let names: Vec<&String> = listed["milestones"].as_object().unwrap().keys().collect();
        assert_eq!(names, ["first", "gone", "zeta", "beta", "alpha"]);
    }

    /// Sessions that are open or paused are not somebody's work in progress.
    #[test]
    fn only_active_sessions_count_as_work_in_progress() {
        let fixture = Fixture::ready("");
        fixture.doc("reqs", "requirement", REQUIREMENT_BODY);
        fixture.done_task();
        let sessions = fixture.ctx.handoff_dir.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let session = |id: &str| json!({ "version": 1, "id": id, "summary": "Later" }).to_string();
        std::fs::write(
            sessions.join("20260101-000000-a.open.json"),
            session("s-open"),
        )
        .unwrap();
        std::fs::write(
            sessions.join("20260101-000001-b.paused.json"),
            session("s-paused"),
        )
        .unwrap();
        std::fs::write(
            sessions.join("20260101-000002-c.closed.json"),
            session("s-closed"),
        )
        .unwrap();
        let step = fixture.step();
        assert_eq!(step["state"], json!("all_done"), "{step}");
        assert_eq!(step["facts"]["active_sessions"], json!(0));

        std::fs::write(
            sessions.join("20260101-000003-d.active.json"),
            session("s-active"),
        )
        .unwrap();
        let step = fixture.step();
        assert_eq!(step["state"], json!("in_progress"), "{step}");
        assert_eq!(
            step["action"]["command"],
            json!("handoff_get_session(session_id: \"s-active\")")
        );
    }

    /// Subtasks are tasks: one in review deep in the tree is found.
    #[test]
    fn tasks_at_any_depth_are_counted() {
        let fixture = Fixture::ready("");
        fixture.done_task();
        for (parent, status) in [("t1", "done"), ("t1.1", "review")] {
            fixture.call(
                "handoff_update_task",
                json!({
                    "task": { "title": "Sub", "status": status, "schedule": { "estimate_hours": 1.0 } },
                    "parent_id": parent,
                }),
            );
        }
        let step = fixture.step();
        assert_eq!(step["state"], json!("review_pending"), "{step}");
        assert_eq!(
            step["action"]["command"],
            json!("handoff_get_task(task_id: \"t1.1.1\")")
        );
        assert_eq!(step["facts"]["tasks"]["total"], json!(3));
    }

    #[test]
    fn a_directory_without_a_name_still_gets_a_project_name_suggested() {
        let project = ProjectOnDisk {
            project_dir: Path::new("/"),
            handoff_dir: None,
        };
        assert_eq!(project.name(), UNNAMED_PROJECT);
        assert!(!project.initialized());
        let named = ProjectOnDisk {
            project_dir: Path::new("/work/my app"),
            handoff_dir: None,
        };
        assert_eq!(named.name(), "my app");
    }
}
