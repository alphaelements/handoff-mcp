//! Readiness diagnosis (SPEC-ONB-002, SPEC-ONB-092): is this project set up
//! well enough for the task loop to run?
//!
//! One implementation serves both entry points: the `handoff_doctor` MCP tool
//! and the `handoff-mcp setup --check` CLI. Given the same project and the
//! same command results, they answer the same.
//!
//! **This module never starts a process and never runs a command.** It reads
//! files and judges results that someone else produced:
//!
//! - the MCP tool returns the commands that should be run
//!   (`commands_to_run`); the agent runs them and reports back;
//! - the CLI runs them itself (`setup --check --run`, see `runner`) and passes
//!   the results in.
//!
//! That is why the handoff directory is located here by reading files only,
//! instead of through `storage::resolve_handoff_dir` (which asks `git` about
//! worktrees), and why a diagnosis writes nothing.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use super::scaffold::{missing_sections, SCAFFOLD_SECTIONS, TODO_MARKER};
use crate::storage::config::{
    read_config, CommandsConfig, Config, COMMAND_KEYS, DEFAULT_COMMAND_TIMEOUT_SEC,
};

/// Name the handoff MCP server is registered under by `handoff-mcp setup` and
/// the plugins.
const HANDOFF_SERVER_NAME: &str = "handoff";
/// Appears in the command line of every way to start the server (the
/// `handoff-mcp` binary, `npx handoff-mcp-server`).
const HANDOFF_BINARY_NAME: &str = "handoff-mcp";
/// Key prefix of the handoff-mcp plugin in Claude Code's installed-plugin list
/// (`handoff-mcp@<marketplace>`).
const HANDOFF_PLUGIN_PREFIX: &str = "handoff-mcp@";

/// How many marked lines the `claude_md_todo` detail lists before summarizing.
const TODO_LINES_LISTED: usize = 10;

/// Exit codes a POSIX shell uses for a command it could not run.
const SHELL_EXIT_NOT_EXECUTABLE: i64 = 126;
const SHELL_EXIT_NOT_FOUND: i64 = 127;

/// Id of the check that reports whether `[commands]` defines what a run needs.
const COMMANDS_DEFINED: &str = "commands_defined";

const DEFINE_TEST_NEXT_STEP: &str = "Define the test command: \
    `handoff_update_config(updates: {\"commands.test\": \"<your test command>\"})`.";

/// Outcome of one check. `skip` means the check was not carried out (nothing
/// to judge, or no result was given); it is never counted as a pass.
/// `manual` means a person has to confirm the item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Pass,
    Fail,
    Skip,
    Manual,
}

/// Who ran a command whose result was judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ResultSource {
    /// The `handoff-mcp` CLI ran it.
    Executed,
    /// An agent ran it and reported the result.
    Reported,
}

/// One diagnosed item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub id: &'static str,
    pub status: CheckStatus,
    /// What was found, or why the check was skipped. Never empty.
    pub detail: String,
    /// What to do about it. Always present on a `fail`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_step: Option<String>,
    /// The `[commands]` value a command check is about.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<ResultSource>,
}

/// A command that still has to be run for its check to be judged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandToRun {
    pub id: &'static str,
    pub command: String,
    pub timeout_sec: u64,
}

/// The diagnosis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorReport {
    /// `true` when nothing failed. A skipped check does not lower it.
    pub ready: bool,
    pub checks: Vec<Check>,
    pub commands_to_run: Vec<CommandToRun>,
    pub warnings: Vec<String>,
}

/// The result of running one command, as handed to [`diagnose`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandResult {
    /// Id of the check the command belongs to (`cmd_test`, ...).
    pub id: String,
    /// The command line that was actually run.
    pub command: String,
    #[serde(default)]
    pub exit_code: Option<i64>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    /// Last lines of the command's output.
    #[serde(default)]
    pub output_tail: Option<String>,
    /// Why the command could not be run to completion (it did not start, or
    /// it timed out).
    #[serde(default)]
    pub error: Option<String>,
}

/// What to diagnose.
#[derive(Debug, Clone)]
pub struct DoctorRequest<'a> {
    pub project_dir: &'a Path,
    /// The user's home directory, where client-wide MCP registrations live.
    pub home_dir: Option<&'a Path>,
    /// Restrict the diagnosis to these check ids; `None` runs every check.
    pub checks: Option<&'a [String]>,
    pub results: &'a [CommandResult],
    /// Who produced `results`.
    pub source: ResultSource,
    /// A command run was asked for (results were reported, or the CLI was
    /// given `--run`). The project then cannot be ready without a test
    /// command.
    pub run_requested: bool,
}

/// Every check id, in report order.
pub const CHECK_IDS: &[&str] = &[
    "handoff_initialized",
    "mcp_registered",
    "claude_md_sections",
    "claude_md_todo",
    COMMANDS_DEFINED,
    "cmd_install",
    "cmd_build",
    "cmd_test",
    "cmd_lint",
    "cmd_typecheck",
    "smoke_defined",
];

/// Checks that judge a command's result, with their `[commands]` key.
/// `format` is not among them: a formatter rewrites files.
pub const COMMAND_CHECKS: &[(&str, &str)] = &[
    ("cmd_install", "install"),
    ("cmd_build", "build"),
    ("cmd_test", "test"),
    ("cmd_lint", "lint"),
    ("cmd_typecheck", "typecheck"),
];

/// Longest output tail quoted in a check's `detail`.
pub const OUTPUT_TAIL_MAX_LINES: usize = 20;

/// The `.handoff/` directory of `project_dir`, found by reading files only:
/// the project's own (a directory or a live symlink), or for a linked git
/// worktree the primary checkout's. `None` when there is none.
pub fn locate_handoff_dir(project_dir: &Path) -> Option<PathBuf> {
    let local = project_dir.join(".handoff");
    if local.is_dir() {
        return Some(local);
    }
    let primary_handoff = primary_checkout(project_dir)?.join(".handoff");
    if !primary_handoff.is_dir() {
        return None;
    }
    // The primary's config may point every worktree at a shared directory.
    Some(
        crate::storage::worktree_handoff_root_override(&primary_handoff).unwrap_or(primary_handoff),
    )
}

/// The primary checkout of the linked git worktree at `project_dir`, read
/// from git's own bookkeeping: a worktree's `.git` is a file naming its
/// admin directory (`<primary>/.git/worktrees/<name>`), whose `commondir`
/// file points back at the shared `.git`. A submodule's `.git` file names a
/// directory without `commondir`, so it is not mistaken for a worktree.
fn primary_checkout(project_dir: &Path) -> Option<PathBuf> {
    let pointer = std::fs::read_to_string(project_dir.join(".git")).ok()?;
    let admin_dir = project_dir.join(pointer.trim().strip_prefix("gitdir:")?.trim());
    let common_dir = std::fs::read_to_string(admin_dir.join("commondir")).ok()?;
    let common_dir = std::fs::canonicalize(admin_dir.join(common_dir.trim())).ok()?;
    common_dir.parent().map(Path::to_path_buf)
}

/// Diagnoses the project. Fails only on a request that cannot be honored (an
/// unknown check id, a malformed result); every finding about the project is
/// a [`Check`] in the report.
pub fn diagnose(request: &DoctorRequest<'_>) -> Result<DoctorReport> {
    let selected = selected_checks(request.checks)?;
    validate_results(request.results, &selected)?;

    let mut warnings = Vec::new();
    let config = load_config(request.project_dir);
    let claude_md = std::fs::read(request.project_dir.join("CLAUDE.md"))
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    let commands = config.as_ref().ok().map(|config| &config.commands);
    let test_undefined = commands.is_none_or(|commands| commands.get("test").is_none());
    // Without a readable config there are no commands, so the fallback is
    // never handed out; it only keeps the value total.
    let timeout_sec = config
        .as_ref()
        .map(|config| config.onboarding.command_timeout_sec())
        .unwrap_or(DEFAULT_COMMAND_TIMEOUT_SEC);

    // A command run was asked for but there is no test command to run.
    let run_without_test = request.run_requested && test_undefined;
    // `commands_defined` is the check that reports that; a filter can leave
    // it out of the report.
    let commands_defined_selected = selected.contains(&COMMANDS_DEFINED);

    let mut checks = Vec::new();
    let mut commands_to_run = Vec::new();
    for id in selected {
        let check = match id {
            "handoff_initialized" => check_initialized(&config),
            "mcp_registered" => check_mcp_registered(request.project_dir, request.home_dir),
            "claude_md_sections" => check_sections(claude_md.as_deref()),
            "claude_md_todo" => check_todo(claude_md.as_deref()),
            COMMANDS_DEFINED => check_commands_defined(commands, run_without_test),
            "smoke_defined" => check_smoke_defined(commands),
            command_check => {
                let key = command_key(command_check)
                    .ok_or_else(|| anyhow::anyhow!("no check named '{command_check}'"))?;
                let configured = commands.and_then(|commands| commands.get(key));
                let reported = request.results.iter().find(|r| r.id == command_check);
                let (check, waiting) =
                    check_command(command_check, key, configured, reported, request.source);
                if waiting {
                    if let Some(command) = configured {
                        commands_to_run.push(CommandToRun {
                            id: command_check,
                            command: command.to_string(),
                            timeout_sec,
                        });
                    }
                }
                check
            }
        };
        checks.push(check);
    }

    let any_failed = checks.iter().any(|c| c.status == CheckStatus::Fail);
    if run_without_test && !commands_defined_selected {
        // Nothing in the report says why the project is not ready, or says it
        // only in part: another check failing must not hide this reason.
        warnings.push(format!(
            "not ready: a command run was requested but `[commands] test` is not defined. {}",
            DEFINE_TEST_NEXT_STEP
        ));
    }

    Ok(DoctorReport {
        ready: !any_failed && !run_without_test,
        checks,
        commands_to_run,
        warnings,
    })
}

/// The requested check ids in report order, or every check without a filter.
fn selected_checks(filter: Option<&[String]>) -> Result<Vec<&'static str>> {
    let Some(filter) = filter else {
        return Ok(CHECK_IDS.to_vec());
    };
    let unknown: Vec<&str> = filter
        .iter()
        .map(String::as_str)
        .filter(|id| !CHECK_IDS.contains(id))
        .collect();
    if !unknown.is_empty() {
        anyhow::bail!(
            "unknown check id(s): {}. Known ids: {}",
            unknown.join(", "),
            CHECK_IDS.join(", ")
        );
    }
    if filter.is_empty() {
        anyhow::bail!(
            "`checks` is empty: name at least one check, or omit it to run all of them. \
             Known ids: {}",
            CHECK_IDS.join(", ")
        );
    }
    Ok(CHECK_IDS
        .iter()
        .copied()
        .filter(|id| filter.iter().any(|wanted| wanted == id))
        .collect())
}

fn command_key(check_id: &str) -> Option<&'static str> {
    COMMAND_CHECKS
        .iter()
        .find(|(id, _)| *id == check_id)
        .map(|(_, key)| *key)
}

/// Rejects results that cannot be judged, so a caller's mistake is never
/// mistaken for a finding about the project.
fn validate_results(results: &[CommandResult], selected: &[&'static str]) -> Result<()> {
    let command_ids = || {
        COMMAND_CHECKS
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>()
            .join(", ")
    };
    for (index, result) in results.iter().enumerate() {
        if command_key(&result.id).is_none() {
            anyhow::bail!(
                "results[{index}]: '{}' is not a command check. Results are accepted for: {}",
                result.id,
                command_ids()
            );
        }
        if !selected.contains(&result.id.as_str()) {
            anyhow::bail!(
                "results[{index}]: '{}' is not among the requested `checks`",
                result.id
            );
        }
        if results[..index]
            .iter()
            .any(|earlier| earlier.id == result.id)
        {
            anyhow::bail!(
                "results: more than one result for '{}'; report each command once",
                result.id
            );
        }
        if result.exit_code.is_none() && result.error.is_none() {
            anyhow::bail!(
                "results[{index}] ('{}'): give `exit_code`, or `error` when the command \
                 could not be run to completion",
                result.id
            );
        }
        // An `error` is the reason a check fails with; one that says nothing
        // cannot be judged either way.
        if result
            .error
            .as_deref()
            .is_some_and(|error| error.trim().is_empty())
        {
            anyhow::bail!(
                "results[{index}] ('{}'): `error` is empty. Leave it out when the command ran \
                 to completion, or say why it did not (for example 'timed out after 600s')",
                result.id
            );
        }
    }
    Ok(())
}

/// The project's config, or why it cannot be used (one line).
fn load_config(project_dir: &Path) -> std::result::Result<Config, String> {
    let Some(handoff_dir) = locate_handoff_dir(project_dir) else {
        return Err("no .handoff/ directory was found".to_string());
    };
    read_config(&handoff_dir.join("config.toml")).map_err(|e| single_line(&format!("{e:#}")))
}

fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn base_check(id: &'static str, status: CheckStatus, detail: impl Into<String>) -> Check {
    Check {
        id,
        status,
        detail: detail.into(),
        next_step: None,
        command: None,
        exit_code: None,
        duration_ms: None,
        source: None,
    }
}

fn failed(id: &'static str, detail: impl Into<String>, next_step: impl Into<String>) -> Check {
    Check {
        next_step: Some(next_step.into()),
        ..base_check(id, CheckStatus::Fail, detail)
    }
}

fn check_initialized(config: &std::result::Result<Config, String>) -> Check {
    const ID: &str = "handoff_initialized";
    match config {
        Ok(config) => base_check(
            ID,
            CheckStatus::Pass,
            format!("initialized as project '{}'", config.project.name),
        ),
        Err(reason) => failed(
            ID,
            format!("the project is not usable by handoff: {reason}"),
            "Initialize the project: call `handoff_init(project_name: \"<name>\")`, or run \
             `handoff-mcp init --project-name <name>` in the project directory. If \
             `.handoff/config.toml` exists, repair what the detail names instead.",
        ),
    }
}

/// Does this MCP server entry start handoff-mcp? Matched by the conventional
/// name, or by the command it runs when it was registered under another one.
fn is_handoff_server(name: &str, entry: &JsonValue) -> bool {
    if name == HANDOFF_SERVER_NAME {
        return true;
    }
    let command = entry.get("command").and_then(JsonValue::as_str);
    let args = entry
        .get("args")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(JsonValue::as_str);
    command
        .into_iter()
        .chain(args)
        .any(|word| word.contains(HANDOFF_BINARY_NAME))
}

fn has_handoff_server(servers: Option<&JsonValue>) -> bool {
    servers
        .and_then(JsonValue::as_object)
        .is_some_and(|servers| {
            servers
                .iter()
                .any(|(name, entry)| is_handoff_server(name, entry))
        })
}

fn read_json(path: &Path) -> Option<JsonValue> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// The places a handoff server registration was found in.
fn mcp_registrations(project_dir: &Path, home_dir: Option<&Path>) -> Vec<String> {
    let mut found = Vec::new();
    if has_handoff_server(
        read_json(&project_dir.join(".mcp.json"))
            .and_then(|j| j.get("mcpServers").cloned())
            .as_ref(),
    ) {
        found.push(".mcp.json".to_string());
    }
    let Some(home) = home_dir else {
        return found;
    };

    if has_handoff_server(
        read_json(&home.join(".claude/settings.json"))
            .and_then(|j| j.get("mcpServers").cloned())
            .as_ref(),
    ) {
        found.push("~/.claude/settings.json".to_string());
    }

    if let Some(claude_json) = read_json(&home.join(".claude.json")) {
        let user_scope = has_handoff_server(claude_json.get("mcpServers"));
        // `claude mcp add` without `-s user` stores the server per project,
        // keyed by the project's path.
        let canonical = std::fs::canonicalize(project_dir).ok();
        let project_scope = [Some(project_dir), canonical.as_deref()]
            .into_iter()
            .flatten()
            .any(|dir| {
                has_handoff_server(
                    claude_json
                        .get("projects")
                        .and_then(|projects| projects.get(dir.to_string_lossy().as_ref()))
                        .and_then(|project| project.get("mcpServers")),
                )
            });
        if user_scope || project_scope {
            found.push("~/.claude.json".to_string());
        }
    }

    let plugin = read_json(&home.join(".claude/plugins/installed_plugins.json")).and_then(|j| {
        j.get("plugins")?
            .as_object()?
            .keys()
            .find(|key| key.starts_with(HANDOFF_PLUGIN_PREFIX))
            .cloned()
    });
    if let Some(plugin) = plugin {
        found.push(format!("Claude Code plugin {plugin}"));
    }

    let codex = std::fs::read_to_string(home.join(".codex/config.toml"))
        .ok()
        .and_then(|text| toml::from_str::<toml::Value>(&text).ok())
        .and_then(|config| serde_json::to_value(config.get("mcp_servers")?).ok());
    if has_handoff_server(codex.as_ref()) {
        found.push("~/.codex/config.toml".to_string());
    }
    found
}

fn check_mcp_registered(project_dir: &Path, home_dir: Option<&Path>) -> Check {
    const ID: &str = "mcp_registered";
    let found = mcp_registrations(project_dir, home_dir);
    if !found.is_empty() {
        return base_check(
            ID,
            CheckStatus::Pass,
            format!("handoff server registered in {}", found.join(", ")),
        );
    }
    // Not a failure: clients and registration methods this does not know
    // exist, so an entry it cannot see proves nothing.
    Check {
        next_step: Some(
            "If your client does not list the handoff tools, run `handoff-mcp setup --mcp-json` \
             in the project directory (or install the handoff-mcp plugin) and restart the client."
                .to_string(),
        ),
        ..base_check(
            ID,
            CheckStatus::Skip,
            "no handoff server entry was found in .mcp.json, ~/.claude/settings.json, \
             ~/.claude.json, the Claude Code plugin list or ~/.codex/config.toml; other clients \
             and registration methods are not inspected",
        )
    }
}

fn check_sections(claude_md: Option<&str>) -> Check {
    const ID: &str = "claude_md_sections";
    const NEXT_STEP: &str = "Run `handoff-mcp setup --claude-md-scaffold` in the project \
                             directory to add the missing sections, then fill in the fields it \
                             marks.";
    let Some(text) = claude_md else {
        return failed(
            ID,
            "CLAUDE.md was not found in the project directory",
            NEXT_STEP,
        );
    };
    let missing: Vec<&str> = missing_sections(text)
        .iter()
        .map(|section| section.title)
        .collect();
    if missing.is_empty() {
        return base_check(
            ID,
            CheckStatus::Pass,
            format!(
                "CLAUDE.md has all {} project sections",
                SCAFFOLD_SECTIONS.len()
            ),
        );
    }
    failed(
        ID,
        format!("CLAUDE.md lacks the section(s): {}", missing.join(", ")),
        NEXT_STEP,
    )
}

fn check_todo(claude_md: Option<&str>) -> Check {
    const ID: &str = "claude_md_todo";
    let Some(text) = claude_md else {
        return base_check(ID, CheckStatus::Skip, "CLAUDE.md was not found");
    };
    let marked: Vec<(usize, usize)> = text
        .lines()
        .enumerate()
        .map(|(index, line)| (index + 1, line.matches(TODO_MARKER).count()))
        .filter(|(_, count)| *count > 0)
        .collect();
    if marked.is_empty() {
        return base_check(
            ID,
            CheckStatus::Pass,
            format!("CLAUDE.md has no `{TODO_MARKER}` marker left"),
        );
    }
    let total: usize = marked.iter().map(|(_, count)| count).sum();
    let mut lines: Vec<String> = marked
        .iter()
        .take(TODO_LINES_LISTED)
        .map(|(line, _)| line.to_string())
        .collect();
    if marked.len() > TODO_LINES_LISTED {
        lines.push(format!("and {} more", marked.len() - TODO_LINES_LISTED));
    }
    let noun = if marked.len() == 1 { "line" } else { "lines" };
    failed(
        ID,
        format!(
            "{total} `{TODO_MARKER}` marker(s) left in CLAUDE.md ({noun} {})",
            lines.join(", ")
        ),
        format!(
            "Replace each `{TODO_MARKER}` in CLAUDE.md with the real value, or with `n/a` when \
             the project has no such step."
        ),
    )
}

fn check_commands_defined(commands: Option<&CommandsConfig>, run_without_test: bool) -> Check {
    const ID: &str = COMMANDS_DEFINED;
    let defined: Vec<&str> = COMMAND_KEYS
        .iter()
        .copied()
        .filter(|key| commands.is_some_and(|commands| commands.get(key).is_some()))
        .collect();
    if defined.is_empty() {
        return failed(
            ID,
            "`[commands]` in .handoff/config.toml defines no command",
            "Run `handoff-mcp setup --claude-md-scaffold` in the project directory to detect \
             the commands, or set them yourself: \
             `handoff_update_config(updates: {\"commands.test\": \"<your test command>\"})`.",
        );
    }
    if run_without_test {
        return failed(
            ID,
            format!(
                "`[commands] test` is not defined, so a command run cannot show that the \
                 project works (defined: {})",
                defined.join(", ")
            ),
            DEFINE_TEST_NEXT_STEP,
        );
    }
    let undefined: Vec<&str> = COMMAND_KEYS
        .iter()
        .copied()
        .filter(|key| !defined.contains(key))
        .collect();
    let mut detail = format!("defined: {}", defined.join(", "));
    if !undefined.is_empty() {
        detail.push_str(&format!("; not defined: {}", undefined.join(", ")));
    }
    base_check(ID, CheckStatus::Pass, detail)
}

fn check_smoke_defined(commands: Option<&CommandsConfig>) -> Check {
    const ID: &str = "smoke_defined";
    match commands.and_then(|commands| commands.get("smoke")) {
        Some(command) => Check {
            command: Some(command.to_string()),
            ..base_check(
                ID,
                CheckStatus::Pass,
                format!("`[commands] smoke` is defined: `{command}`"),
            )
        },
        None => base_check(ID, CheckStatus::Skip, "`[commands] smoke` is not defined"),
    }
}

/// Judges one command check. The second value says whether the command is
/// still waiting to be run (no usable result was given for it).
fn check_command(
    id: &'static str,
    key: &str,
    configured: Option<&str>,
    reported: Option<&CommandResult>,
    source: ResultSource,
) -> (Check, bool) {
    let with_command = |check: Check| Check {
        command: configured.map(str::to_string),
        ..check
    };
    let Some(result) = reported else {
        let check = match configured {
            Some(command) => base_check(
                id,
                CheckStatus::Skip,
                format!("not run: no result was given for `{command}`"),
            ),
            None => base_check(
                id,
                CheckStatus::Skip,
                format!("`[commands] {key}` is not defined"),
            ),
        };
        return (with_command(check), true);
    };

    // A result only counts for the command the config names right now.
    if configured != Some(result.command.as_str()) {
        let expected = match configured {
            Some(command) => format!("`[commands] {key}` (`{command}`)"),
            None => format!("`[commands] {key}`, which is not defined"),
        };
        let check = failed(
            id,
            format!(
                "the reported command (`{}`) does not match {expected}; the result was not used",
                result.command
            ),
            "Run the command exactly as `commands_to_run` gives it and report that result.",
        );
        return (with_command(check), true);
    }

    let judged = |check: Check| Check {
        duration_ms: result.duration_ms,
        source: Some(source),
        ..with_command(check)
    };
    if let Some(error) = &result.error {
        let check = failed(
            id,
            with_output_tail(
                format!(
                    "the command could not be run to completion: {}",
                    single_line(error)
                ),
                result.output_tail.as_deref(),
            ),
            format!(
                "Make `[commands] {key}` a command that starts and finishes within the time \
                 limit (`[onboarding] command_timeout_sec`), then run the check again."
            ),
        );
        return (judged(check), false);
    }
    // `validate_results` guarantees an exit code when there is no error.
    let exit_code = result.exit_code.unwrap_or_default();
    let check = if exit_code == 0 {
        base_check(id, CheckStatus::Pass, "exit code 0")
    } else {
        let meaning = match exit_code {
            SHELL_EXIT_NOT_EXECUTABLE => " (not executable)",
            SHELL_EXIT_NOT_FOUND => " (command not found)",
            _ => "",
        };
        failed(
            id,
            with_output_tail(
                format!("exit code {exit_code}{meaning}"),
                result.output_tail.as_deref(),
            ),
            format!(
                "Make the command succeed (exit code 0), or correct `[commands] {key}` if the \
                 command itself is wrong."
            ),
        )
    };
    (
        judged(Check {
            exit_code: Some(exit_code),
            ..check
        }),
        false,
    )
}

/// `summary`, followed by the last [`OUTPUT_TAIL_MAX_LINES`] lines of the
/// command's output when there is any. Blank lines before and after the
/// quoted part are dropped.
fn with_output_tail(summary: String, output: Option<&str>) -> String {
    let lines: Vec<&str> = output
        .unwrap_or_default()
        .lines()
        .map(str::trim_end)
        .collect();
    let is_text = |line: &&str| !line.is_empty();
    let Some(last) = lines.iter().rposition(is_text) else {
        return summary;
    };
    let tail = &lines[(last + 1).saturating_sub(OUTPUT_TAIL_MAX_LINES)..=last];
    let first = tail.iter().position(is_text).unwrap_or(0);
    let tail = &tail[first..];
    format!(
        "{summary}\noutput (last {} line(s)):\n{}",
        tail.len(),
        tail.join("\n")
    )
}

/// Renders a report for a terminal.
pub fn render_text(report: &DoctorReport) -> String {
    let mut text = String::from("Readiness:\n");
    for check in &report.checks {
        let status = match check.status {
            CheckStatus::Pass => "pass",
            CheckStatus::Fail => "fail",
            CheckStatus::Skip => "skip",
            CheckStatus::Manual => "manual",
        };
        let mut detail_lines = check.detail.lines();
        text.push_str(&format!(
            "  [{status}] {}: {}\n",
            check.id,
            detail_lines.next().unwrap_or_default()
        ));
        for line in detail_lines {
            text.push_str(&format!("         {line}\n"));
        }
        if let Some(next_step) = &check.next_step {
            text.push_str(&format!("         next: {next_step}\n"));
        }
    }
    for warning in &report.warnings {
        text.push_str(&format!("  warning: {warning}\n"));
    }
    let failed = report
        .checks
        .iter()
        .filter(|check| check.status == CheckStatus::Fail)
        .count();
    if report.ready {
        text.push_str("\nReady: yes\n");
    } else {
        text.push_str(&format!("\nReady: no ({failed} check(s) failed)\n"));
    }
    if !report.commands_to_run.is_empty() {
        text.push_str(&format!(
            "{} command(s) were not run. `handoff-mcp setup --check --run` runs them.\n",
            report.commands_to_run.len()
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::scaffold::TODO_MARKER;

    const FILLED_CLAUDE_MD: &str = "# Demo\n\n## Session Handoff\n\nx\n\n## Build & Test\n\n- test: `true`\n\n## Coding Rules\n\nr\n\n## Project Structure\n\ns\n\n## Documentation Contract\n\nd\n";

    struct Project {
        root: tempfile::TempDir,
        home: PathBuf,
        dir: PathBuf,
    }

    impl Project {
        /// An empty directory: not initialized, no CLAUDE.md, no registration.
        fn empty() -> Self {
            let root = tempfile::tempdir().unwrap();
            let home = root.path().join("home");
            let dir = root.path().join("project");
            std::fs::create_dir_all(&home).unwrap();
            std::fs::create_dir_all(&dir).unwrap();
            Self { root, home, dir }
        }

        /// A project where every file-based check passes; `commands` is the
        /// body of its `[commands]` section.
        fn ready(commands: &str) -> Self {
            let project = Self::empty();
            project.write_config(&format!(
                "[project]\nname = \"demo\"\n\n[commands]\n{commands}"
            ));
            project.write("CLAUDE.md", FILLED_CLAUDE_MD);
            project.write(
                ".mcp.json",
                r#"{"mcpServers":{"handoff":{"type":"stdio","command":"handoff-mcp"}}}"#,
            );
            project
        }

        fn write(&self, name: &str, content: &str) {
            let path = self.dir.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }

        fn write_config(&self, content: &str) {
            self.write(".handoff/config.toml", content);
        }

        fn write_home(&self, name: &str, content: &str) {
            let path = self.home.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }

        fn request<'a>(&'a self, results: &'a [CommandResult]) -> DoctorRequest<'a> {
            DoctorRequest {
                project_dir: &self.dir,
                home_dir: Some(&self.home),
                checks: None,
                results,
                source: ResultSource::Reported,
                run_requested: !results.is_empty(),
            }
        }

        fn diagnose(&self) -> DoctorReport {
            diagnose(&self.request(&[])).unwrap()
        }

        fn diagnose_with(&self, results: &[CommandResult]) -> DoctorReport {
            diagnose(&self.request(results)).unwrap()
        }
    }

    fn check<'a>(report: &'a DoctorReport, id: &str) -> &'a Check {
        report
            .checks
            .iter()
            .find(|c| c.id == id)
            .unwrap_or_else(|| panic!("no check `{id}` in {report:?}"))
    }

    fn status(report: &DoctorReport, id: &str) -> CheckStatus {
        check(report, id).status
    }

    fn result(id: &str, command: &str, exit_code: i64) -> CommandResult {
        CommandResult {
            id: id.to_string(),
            command: command.to_string(),
            exit_code: Some(exit_code),
            duration_ms: Some(1200),
            output_tail: None,
            error: None,
        }
    }

    fn failed_ids(report: &DoctorReport) -> Vec<&'static str> {
        report
            .checks
            .iter()
            .filter(|c| c.status == CheckStatus::Fail)
            .map(|c| c.id)
            .collect()
    }

    // --- shape (SPEC-ONB-002 AC1) ---

    #[test]
    fn every_check_is_reported_in_order_with_a_detail_and_fails_carry_a_next_step() {
        for project in [
            Project::empty(),
            Project::ready("test = \"true\"\n"),
            Project::ready(""),
        ] {
            for results in [vec![], vec![result("cmd_test", "true", 1)]] {
                let report = project.diagnose_with(&results);
                assert_eq!(
                    report.checks.iter().map(|c| c.id).collect::<Vec<_>>(),
                    CHECK_IDS
                );
                for c in &report.checks {
                    assert!(!c.detail.trim().is_empty(), "{}: empty detail", c.id);
                    if c.status == CheckStatus::Fail {
                        assert!(
                            c.next_step.as_deref().is_some_and(|s| !s.trim().is_empty()),
                            "{}: a failed check must say what to do",
                            c.id
                        );
                    }
                    assert_ne!(c.status, CheckStatus::Manual, "{}", c.id);
                }
            }
        }
    }

    #[test]
    fn statuses_serialize_as_lowercase_words() {
        let json = serde_json::to_value([
            CheckStatus::Pass,
            CheckStatus::Fail,
            CheckStatus::Skip,
            CheckStatus::Manual,
        ])
        .unwrap();
        assert_eq!(json, serde_json::json!(["pass", "fail", "skip", "manual"]));
        assert_eq!(
            serde_json::to_value([ResultSource::Executed, ResultSource::Reported]).unwrap(),
            serde_json::json!(["executed", "reported"])
        );
    }

    #[test]
    fn report_serializes_to_the_documented_shape() {
        let project = Project::ready("test = \"true\"\nlint = \"false\"\n");
        let report = project.diagnose_with(&[result("cmd_lint", "false", 1)]);
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(
            json.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["ready", "checks", "commands_to_run", "warnings"]
        );
        let lint = json["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == "cmd_lint")
            .unwrap();
        assert_eq!(
            lint.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec![
                "id",
                "status",
                "detail",
                "next_step",
                "command",
                "exit_code",
                "duration_ms",
                "source"
            ]
        );
        let initialized = &json["checks"][0];
        assert_eq!(
            initialized.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["id", "status", "detail"],
            "fields that do not apply are left out"
        );
        assert_eq!(
            json["commands_to_run"][0],
            serde_json::json!({ "id": "cmd_test", "command": "true", "timeout_sec": 600 })
        );
    }

    // --- a project that is ready ---

    #[test]
    fn a_prepared_project_is_ready_and_no_command_check_passes_without_a_result() {
        let project = Project::ready("test = \"cargo test\"\nbuild = \"cargo build\"\n");
        let report = project.diagnose();

        assert!(report.ready, "{report:?}");
        assert!(failed_ids(&report).is_empty());
        for id in [
            "handoff_initialized",
            "mcp_registered",
            "claude_md_sections",
            "claude_md_todo",
            "commands_defined",
        ] {
            assert_eq!(status(&report, id), CheckStatus::Pass, "{id}");
        }
        // SPEC-ONB-002 AC4 / SPEC-ONB-092 AC1
        for (id, _) in COMMAND_CHECKS {
            assert_eq!(status(&report, id), CheckStatus::Skip, "{id}");
            assert_eq!(check(&report, id).source, None, "{id}");
            assert_eq!(check(&report, id).exit_code, None, "{id}");
        }
        assert_eq!(
            report.commands_to_run,
            vec![
                CommandToRun {
                    id: "cmd_build",
                    command: "cargo build".to_string(),
                    timeout_sec: 600
                },
                CommandToRun {
                    id: "cmd_test",
                    command: "cargo test".to_string(),
                    timeout_sec: 600
                },
            ]
        );
        assert_eq!(
            check(&report, "cmd_test").command.as_deref(),
            Some("cargo test")
        );
        assert!(check(&report, "cmd_test").detail.contains("not run"));
        assert!(check(&report, "cmd_lint").detail.contains("not defined"));
        assert_eq!(check(&report, "cmd_lint").command, None);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    }

    #[test]
    fn timeout_comes_from_the_onboarding_section() {
        let project = Project::ready("test = \"true\"\n");
        project.write_config(
            "[project]\nname = \"demo\"\n\n[commands]\ntest = \"true\"\n\n[onboarding]\ncommand_timeout_sec = 42\n",
        );
        assert_eq!(project.diagnose().commands_to_run[0].timeout_sec, 42);
    }

    // --- handoff_initialized ---

    #[test]
    fn an_uninitialized_project_fails_handoff_initialized() {
        let project = Project::empty();
        let report = project.diagnose();
        assert!(!report.ready);
        let c = check(&report, "handoff_initialized");
        assert_eq!(c.status, CheckStatus::Fail);
        assert!(c.next_step.as_deref().unwrap().contains("handoff_init"));
        assert_eq!(status(&report, "commands_defined"), CheckStatus::Fail);
        assert_eq!(status(&report, "smoke_defined"), CheckStatus::Skip);
        assert!(report.commands_to_run.is_empty());
        assert!(
            !project.dir.join(".handoff").exists(),
            "a diagnosis creates nothing"
        );
    }

    #[test]
    fn an_unreadable_config_fails_handoff_initialized_with_the_reason() {
        let project = Project::ready("");
        project.write_config("[project\nname = ");
        let report = project.diagnose();
        let c = check(&report, "handoff_initialized");
        assert_eq!(c.status, CheckStatus::Fail);
        assert!(c.detail.contains("config.toml"), "{}", c.detail);
        assert!(!c.detail.contains('\n'), "{}", c.detail);
        assert!(!report.ready);

        let missing = Project::ready("");
        std::fs::remove_file(missing.dir.join(".handoff/config.toml")).unwrap();
        assert_eq!(
            status(&missing.diagnose(), "handoff_initialized"),
            CheckStatus::Fail
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_handoff_directory_counts_as_initialized() {
        let project = Project::ready("test = \"true\"\n");
        let shared = project.root.path().join("shared-handoff");
        std::fs::rename(project.dir.join(".handoff"), &shared).unwrap();
        std::os::unix::fs::symlink(&shared, project.dir.join(".handoff")).unwrap();
        assert_eq!(
            locate_handoff_dir(&project.dir),
            Some(project.dir.join(".handoff"))
        );
        assert_eq!(
            status(&project.diagnose(), "handoff_initialized"),
            CheckStatus::Pass
        );

        // A dangling link is not an initialized project, and is left alone.
        std::fs::remove_dir_all(&shared).unwrap();
        assert_eq!(locate_handoff_dir(&project.dir), None);
        assert_eq!(
            status(&project.diagnose(), "handoff_initialized"),
            CheckStatus::Fail
        );
        assert!(project.dir.join(".handoff").symlink_metadata().is_ok());
    }

    /// A linked git worktree shares the primary checkout's `.handoff/`. It is
    /// found from git's own files, without asking `git`.
    #[test]
    fn a_linked_worktree_uses_the_primary_checkouts_handoff_directory() {
        let project = Project::ready("test = \"true\"\n");
        // The files `git worktree add ../wt` leaves behind.
        let primary = &project.dir;
        let worktree = project.root.path().join("wt");
        let admin = primary.join(".git/worktrees/wt");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(admin.join("commondir"), "../..\n").unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", admin.display()),
        )
        .unwrap();

        let located = locate_handoff_dir(&worktree).expect("the primary's .handoff");
        assert_eq!(
            std::fs::canonicalize(located).unwrap(),
            std::fs::canonicalize(primary.join(".handoff")).unwrap()
        );
        assert!(
            !worktree.join(".handoff").exists(),
            "locating must not create a link"
        );

        // A submodule's `.git` file has no `commondir`: not a worktree.
        let module = project.root.path().join("module");
        let module_git = primary.join(".git/modules/module");
        std::fs::create_dir_all(&module_git).unwrap();
        std::fs::create_dir_all(&module).unwrap();
        std::fs::write(
            module.join(".git"),
            format!("gitdir: {}\n", module_git.display()),
        )
        .unwrap();
        assert_eq!(locate_handoff_dir(&module), None);
        // Garbage in `.git` is not an error either.
        std::fs::write(module.join(".git"), "not a gitdir line").unwrap();
        assert_eq!(locate_handoff_dir(&module), None);
    }

    // --- mcp_registered ---

    #[test]
    fn mcp_registration_is_found_in_each_known_place() {
        let entry = r#"{"mcpServers":{"handoff":{"type":"stdio","command":"handoff-mcp"}}}"#;
        type Register = fn(&Project, &str);
        let places: [(&str, Register); 6] = [
            (".mcp.json", |p, e| p.write(".mcp.json", e)),
            (".claude/settings.json", |p, e| {
                p.write_home(".claude/settings.json", e)
            }),
            (".claude.json", |p, e| p.write_home(".claude.json", e)),
            (".claude.json", |p, _| {
                let project_key = p.dir.to_string_lossy().to_string();
                let content = serde_json::json!({
                    "projects": { project_key: { "mcpServers": {
                        "handoff": { "command": "handoff-mcp" }
                    } } }
                });
                p.write_home(".claude.json", &content.to_string());
            }),
            ("plugin", |p, _| {
                p.write_home(
                    ".claude/plugins/installed_plugins.json",
                    r#"{"version":2,"plugins":{"handoff-mcp@some-marketplace":[{"version":"0.48.0"}]}}"#,
                )
            }),
            (".codex/config.toml", |p, _| {
                p.write_home(
                    ".codex/config.toml",
                    "[mcp_servers.handoff]\ncommand = \"handoff-mcp\"\n",
                )
            }),
        ];
        for (place, register) in places {
            let project = Project::ready("test = \"true\"\n");
            std::fs::remove_file(project.dir.join(".mcp.json")).unwrap();
            assert_eq!(
                status(&project.diagnose(), "mcp_registered"),
                CheckStatus::Skip
            );
            register(&project, entry);
            let report = project.diagnose();
            let c = check(&report, "mcp_registered");
            assert_eq!(c.status, CheckStatus::Pass, "{place}: {}", c.detail);
            assert!(c.detail.contains(place), "{place}: {}", c.detail);
        }
    }

    #[test]
    fn a_server_registered_under_another_name_is_recognized_by_its_command() {
        for entry in [
            r#"{"mcpServers":{"memory":{"command":"/usr/local/bin/handoff-mcp","args":[]}}}"#,
            r#"{"mcpServers":{"h":{"command":"npx","args":["-y","handoff-mcp-server"]}}}"#,
        ] {
            let project = Project::ready("test = \"true\"\n");
            project.write(".mcp.json", entry);
            assert_eq!(
                status(&project.diagnose(), "mcp_registered"),
                CheckStatus::Pass,
                "{entry}"
            );
        }
    }

    /// Other clients and registration methods exist, so "not found" is not
    /// proof of absence: the check is skipped, never failed.
    #[test]
    fn no_known_registration_is_a_skip_that_says_where_it_looked() {
        for other in [
            None,
            Some(r#"{"mcpServers":{"context7":{"command":"npx"}}}"#),
            Some("{ not json"),
            Some(r#"{"mcpServers":"oops"}"#),
        ] {
            let project = Project::ready("test = \"true\"\n");
            match other {
                Some(content) => project.write(".mcp.json", content),
                None => std::fs::remove_file(project.dir.join(".mcp.json")).unwrap(),
            }
            let report = project.diagnose();
            let c = check(&report, "mcp_registered");
            assert_eq!(c.status, CheckStatus::Skip, "{other:?}");
            assert!(c.detail.contains(".mcp.json"), "{}", c.detail);
            assert!(
                c.next_step
                    .as_deref()
                    .unwrap_or("")
                    .contains("setup --mcp-json"),
                "{:?}",
                c.next_step
            );
            assert!(report.ready, "a skip does not lower ready: {other:?}");
        }
    }

    #[test]
    fn without_a_home_directory_only_the_project_is_inspected() {
        let project = Project::ready("test = \"true\"\n");
        let mut request = project.request(&[]);
        request.home_dir = None;
        assert_eq!(
            status(&diagnose(&request).unwrap(), "mcp_registered"),
            CheckStatus::Pass
        );
        std::fs::remove_file(project.dir.join(".mcp.json")).unwrap();
        assert_eq!(
            status(&diagnose(&request).unwrap(), "mcp_registered"),
            CheckStatus::Skip
        );
    }

    // --- CLAUDE.md ---

    #[test]
    fn missing_sections_fail_and_are_named() {
        let project = Project::ready("test = \"true\"\n");
        project.write(
            "CLAUDE.md",
            "# Demo\n\n## build & test\n\nx\n\n### Coding Rules\n\ny\n\n```\n## Project Structure\n```\n",
        );
        let report = project.diagnose();
        let c = check(&report, "claude_md_sections");
        assert_eq!(c.status, CheckStatus::Fail);
        assert!(
            c.detail
                .contains("Project Structure, Documentation Contract"),
            "{}",
            c.detail
        );
        assert!(!c.detail.contains("Coding Rules"), "{}", c.detail);
        assert!(c
            .next_step
            .as_deref()
            .unwrap()
            .contains("setup --claude-md-scaffold"));
        assert!(!report.ready);
    }

    #[test]
    fn a_missing_claude_md_fails_sections_and_skips_the_todo_check() {
        let project = Project::ready("test = \"true\"\n");
        std::fs::remove_file(project.dir.join("CLAUDE.md")).unwrap();
        let report = project.diagnose();
        assert_eq!(status(&report, "claude_md_sections"), CheckStatus::Fail);
        assert!(check(&report, "claude_md_sections")
            .detail
            .contains("not found"));
        let todo = check(&report, "claude_md_todo");
        assert_eq!(todo.status, CheckStatus::Skip);
        assert!(todo.detail.contains("not found"), "{}", todo.detail);
    }

    // SPEC-ONB-002 AC2
    #[test]
    fn a_single_todo_marker_fails_the_todo_check_and_readiness() {
        let project = Project::ready("test = \"true\"\n");
        project.write(
            "CLAUDE.md",
            &format!("{FILLED_CLAUDE_MD}\n- smoke: {TODO_MARKER}\n"),
        );
        let report = project.diagnose();
        let c = check(&report, "claude_md_todo");
        assert_eq!(c.status, CheckStatus::Fail);
        assert!(c.detail.contains("1 "), "{}", c.detail);
        assert!(c.detail.contains("line 23"), "{}", c.detail);
        assert!(c.next_step.as_deref().unwrap().contains("n/a"));
        assert!(!report.ready);
        assert_eq!(failed_ids(&report), vec!["claude_md_todo"]);
    }

    #[test]
    fn many_todo_markers_are_counted_and_the_line_list_is_bounded() {
        let project = Project::ready("test = \"true\"\n");
        let markers: String = (0..30)
            .map(|i| format!("- f{i}: {TODO_MARKER} and {TODO_MARKER}\n"))
            .collect();
        project.write("CLAUDE.md", &format!("{FILLED_CLAUDE_MD}{markers}"));
        let c = check(&project.diagnose(), "claude_md_todo").clone();
        assert_eq!(c.status, CheckStatus::Fail);
        assert!(c.detail.contains("60 "), "{}", c.detail);
        assert!(c.detail.len() < 400, "{}", c.detail);
    }

    // --- commands_defined / smoke_defined ---

    #[test]
    fn no_command_at_all_fails_commands_defined() {
        for config in [
            "[project]\nname = \"demo\"\n",
            "[project]\nname = \"demo\"\n\n[commands]\n",
            "[project]\nname = \"demo\"\n\n[commands]\ntest = \"  \"\ne2e = \"x\"\n",
        ] {
            let project = Project::ready("");
            project.write_config(config);
            let report = project.diagnose();
            let c = check(&report, "commands_defined");
            assert_eq!(c.status, CheckStatus::Fail, "{config}");
            assert!(c.next_step.as_deref().unwrap().contains("commands.test"));
            assert!(!report.ready);
        }
    }

    #[test]
    fn commands_without_a_test_command_are_ready_until_a_run_is_requested() {
        let project = Project::ready("build = \"make\"\n");

        let report = project.diagnose();
        assert_eq!(status(&report, "commands_defined"), CheckStatus::Pass);
        assert!(check(&report, "commands_defined").detail.contains("build"));
        assert!(report.ready, "{report:?}");
        assert_eq!(status(&report, "cmd_test"), CheckStatus::Skip);

        // Results were reported: without a test command the run cannot show
        // that the project works.
        let report = project.diagnose_with(&[result("cmd_build", "make", 0)]);
        assert_eq!(status(&report, "cmd_build"), CheckStatus::Pass);
        assert_eq!(status(&report, "cmd_test"), CheckStatus::Skip);
        let c = check(&report, "commands_defined");
        assert_eq!(c.status, CheckStatus::Fail);
        assert!(c.detail.contains("test"), "{}", c.detail);
        assert!(!report.ready);

        // The CLI's --run with nothing to report is a requested run too.
        let mut request = project.request(&[]);
        request.run_requested = true;
        assert!(!diagnose(&request).unwrap().ready);
    }

    #[test]
    fn smoke_is_reported_as_defined_or_skipped_never_failed() {
        let with = Project::ready("test = \"true\"\nsmoke = \"./smoke.sh\"\n");
        let c = check(&with.diagnose(), "smoke_defined").clone();
        assert_eq!(c.status, CheckStatus::Pass);
        assert!(c.detail.contains("./smoke.sh"), "{}", c.detail);

        let without = Project::ready("test = \"true\"\n");
        let report = without.diagnose();
        let c = check(&report, "smoke_defined");
        assert_eq!(c.status, CheckStatus::Skip);
        assert!(c.detail.contains("not defined"), "{}", c.detail);
        assert!(report.ready);
        assert!(
            report
                .commands_to_run
                .iter()
                .all(|c| c.id != "smoke_defined"),
            "the smoke command is not run by the diagnosis"
        );
    }

    // --- command results ---

    // SPEC-ONB-002 AC3
    #[test]
    fn a_reported_failure_fails_the_check_with_exit_code_and_source() {
        let project = Project::ready("test = \"cargo test\"\n");
        let report = project.diagnose_with(&[result("cmd_test", "cargo test", 1)]);
        let c = check(&report, "cmd_test");
        assert_eq!(c.status, CheckStatus::Fail);
        assert_eq!(c.exit_code, Some(1));
        assert_eq!(c.source, Some(ResultSource::Reported));
        assert_eq!(c.duration_ms, Some(1200));
        assert_eq!(c.command.as_deref(), Some("cargo test"));
        assert!(c.detail.contains("exit code 1"), "{}", c.detail);
        assert!(!report.ready);
        assert!(
            report.commands_to_run.is_empty(),
            "a judged command is not asked for again"
        );
    }

    #[test]
    fn a_reported_success_passes_the_check() {
        let project = Project::ready("test = \"cargo test\"\nlint = \"cargo clippy\"\n");
        let report = project.diagnose_with(&[result("cmd_test", "cargo test", 0)]);
        let c = check(&report, "cmd_test");
        assert_eq!(c.status, CheckStatus::Pass);
        assert_eq!(c.exit_code, Some(0));
        assert_eq!(c.source, Some(ResultSource::Reported));
        assert!(report.ready, "{report:?}");
        // The command that has no result yet is still waiting.
        assert_eq!(status(&report, "cmd_lint"), CheckStatus::Skip);
        assert_eq!(
            report
                .commands_to_run
                .iter()
                .map(|c| c.id)
                .collect::<Vec<_>>(),
            vec!["cmd_lint"]
        );
    }

    #[test]
    fn results_carry_the_source_they_were_given() {
        let project = Project::ready("test = \"true\"\n");
        let results = [result("cmd_test", "true", 0)];
        let mut request = project.request(&results);
        request.source = ResultSource::Executed;
        let report = diagnose(&request).unwrap();
        assert_eq!(
            check(&report, "cmd_test").source,
            Some(ResultSource::Executed)
        );
    }

    // SPEC-ONB-002 AC5
    #[test]
    fn a_result_for_a_different_command_is_not_used() {
        let project = Project::ready("test = \"cargo test\"\n");
        for reported in ["cargo test --lib", "true", "", " cargo test "] {
            let report = project.diagnose_with(&[result("cmd_test", reported, 0)]);
            let c = check(&report, "cmd_test");
            assert_eq!(c.status, CheckStatus::Fail, "{reported:?}");
            assert!(c.detail.contains("does not match"), "{}", c.detail);
            assert_eq!(c.exit_code, None, "the result was not adopted");
            assert_eq!(c.source, None);
            assert_eq!(c.command.as_deref(), Some("cargo test"));
            assert!(!report.ready);
            assert_eq!(
                report.commands_to_run,
                vec![CommandToRun {
                    id: "cmd_test",
                    command: "cargo test".to_string(),
                    timeout_sec: 600
                }],
                "the right command is still waiting to be run"
            );
        }
    }

    #[test]
    fn a_result_for_an_undefined_command_fails_as_a_mismatch() {
        let project = Project::ready("test = \"cargo test\"\n");
        let report = project.diagnose_with(&[
            result("cmd_test", "cargo test", 0),
            result("cmd_lint", "cargo clippy", 0),
        ]);
        let c = check(&report, "cmd_lint");
        assert_eq!(c.status, CheckStatus::Fail);
        assert!(c.detail.contains("not defined"), "{}", c.detail);
        assert!(report.commands_to_run.is_empty());
    }

    // SPEC-ONB-092 AC2
    #[test]
    fn an_error_fails_the_check_with_the_reason() {
        let project = Project::ready("test = \"cargo test\"\nbuild = \"cargo build\"\n");
        let report = project.diagnose_with(&[
            CommandResult {
                id: "cmd_test".to_string(),
                command: "cargo test".to_string(),
                exit_code: None,
                duration_ms: Some(600_000),
                output_tail: None,
                error: Some("timed out after 600s".to_string()),
            },
            CommandResult {
                id: "cmd_build".to_string(),
                command: "cargo build".to_string(),
                // An error wins even when an exit code came with it.
                exit_code: Some(0),
                duration_ms: None,
                output_tail: None,
                error: Some("could not start: No such file or directory".to_string()),
            },
        ]);
        let test = check(&report, "cmd_test");
        assert_eq!(test.status, CheckStatus::Fail);
        assert!(
            test.detail.contains("timed out after 600s"),
            "{}",
            test.detail
        );
        assert_eq!(test.source, Some(ResultSource::Reported));
        assert_eq!(test.exit_code, None);
        assert_eq!(test.duration_ms, Some(600_000));
        let build = check(&report, "cmd_build");
        assert_eq!(build.status, CheckStatus::Fail);
        assert!(build.detail.contains("could not start"), "{}", build.detail);
        assert!(!report.ready);
    }

    #[test]
    fn a_failure_quotes_a_bounded_tail_of_the_output() {
        let project = Project::ready("test = \"cargo test\"\n");
        let output: String = (1..=50).map(|i| format!("line {i}\n")).collect();
        let mut failing = result("cmd_test", "cargo test", 101);
        failing.output_tail = Some(output);
        let report = project.diagnose_with(&[failing]);
        let detail = &check(&report, "cmd_test").detail;
        assert!(detail.contains("exit code 101"), "{detail}");
        assert!(detail.contains("line 50"), "{detail}");
        assert!(detail.contains("line 31"), "{detail}");
        assert!(!detail.contains("line 30\n"), "{detail}");
        assert_eq!(
            detail.lines().filter(|l| l.starts_with("line ")).count(),
            OUTPUT_TAIL_MAX_LINES
        );

        // A success does not carry the output around.
        let mut passing = result("cmd_test", "cargo test", 0);
        passing.output_tail = Some("all good\n".to_string());
        let report = project.diagnose_with(&[passing]);
        assert!(!check(&report, "cmd_test").detail.contains("all good"));
    }

    #[test]
    fn blank_lines_around_the_output_are_not_quoted() {
        let project = Project::ready("test = \"npm run test\"\n");
        let mut failing = result("cmd_test", "npm run test", 2);
        failing.output_tail = Some("\n\n> test\n> exit 2\n\n   \n".to_string());
        let report = project.diagnose_with(&[failing]);
        assert_eq!(
            check(&report, "cmd_test").detail,
            "exit code 2\noutput (last 2 line(s)):\n> test\n> exit 2"
        );

        let mut silent = result("cmd_test", "npm run test", 2);
        silent.output_tail = Some("\n  \n".to_string());
        let report = project.diagnose_with(&[silent]);
        assert_eq!(check(&report, "cmd_test").detail, "exit code 2");
    }

    #[test]
    fn well_known_shell_exit_codes_are_explained() {
        let project = Project::ready("test = \"nope\"\n");
        let report = project.diagnose_with(&[result("cmd_test", "nope", 127)]);
        assert!(check(&report, "cmd_test")
            .detail
            .contains("command not found"));
        let report = project.diagnose_with(&[result("cmd_test", "nope", 126)]);
        assert!(check(&report, "cmd_test").detail.contains("not executable"));
    }

    // --- request validation ---

    #[test]
    fn malformed_results_are_rejected_before_anything_is_judged() {
        let project = Project::ready("test = \"true\"\nformat = \"fmt\"\nsmoke = \"s\"\n");
        let neither = CommandResult {
            id: "cmd_test".to_string(),
            command: "true".to_string(),
            exit_code: None,
            duration_ms: None,
            output_tail: None,
            error: None,
        };
        let cases: Vec<(Vec<CommandResult>, &str)> = vec![
            (vec![result("cmd_format", "fmt", 0)], "cmd_format"),
            (vec![result("smoke_defined", "s", 0)], "smoke_defined"),
            (vec![result("handoff_initialized", "x", 0)], "cmd_test"),
            (vec![result("nope", "x", 0)], "nope"),
            (
                vec![result("cmd_test", "true", 0), result("cmd_test", "true", 1)],
                "more than one",
            ),
            (vec![neither], "exit_code"),
        ];
        for (results, expected) in cases {
            let err = diagnose(&project.request(&results))
                .unwrap_err()
                .to_string();
            assert!(err.contains(expected), "{results:?}: {err}");
        }
    }

    /// An `error` that says nothing is not a reason. Judging it would either
    /// fail a command that succeeded (an agent filling every field sends
    /// `error: ""` next to `exit_code: 0`) or report a failure without a
    /// cause, so the caller is told to send it properly instead.
    #[test]
    fn a_blank_error_is_rejected_instead_of_judged() {
        let project = Project::ready("test = \"true\"\n");
        for (exit_code, error) in [
            (None, ""),
            (Some(0), ""),
            (Some(0), " \n\t"),
            (Some(1), " "),
        ] {
            let blank = CommandResult {
                id: "cmd_test".to_string(),
                command: "true".to_string(),
                exit_code,
                duration_ms: None,
                output_tail: None,
                error: Some(error.to_string()),
            };
            let err = diagnose(&project.request(&[blank]))
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("`error` is empty") && err.contains("cmd_test"),
                "{exit_code:?} {error:?}: {err}"
            );
        }
    }

    #[test]
    fn results_reject_unknown_fields_so_a_misspelled_key_is_not_dropped() {
        let parsed: std::result::Result<CommandResult, _> = serde_json::from_value(
            serde_json::json!({ "id": "cmd_test", "command": "true", "exitCode": 0 }),
        );
        assert!(parsed.is_err());
        let ok: CommandResult = serde_json::from_value(
            serde_json::json!({ "id": "cmd_test", "command": "true", "exit_code": 0 }),
        )
        .unwrap();
        assert_eq!(ok.exit_code, Some(0));
    }

    // --- checks filter ---

    #[test]
    fn a_checks_filter_limits_the_report_and_the_commands_to_run() {
        let project = Project::ready("test = \"true\"\nbuild = \"make\"\n");
        project.write("CLAUDE.md", &format!("{FILLED_CLAUDE_MD}{TODO_MARKER}\n"));
        let wanted = vec!["cmd_test".to_string(), "handoff_initialized".to_string()];
        let mut request = project.request(&[]);
        request.checks = Some(&wanted);
        let report = diagnose(&request).unwrap();
        assert_eq!(
            report.checks.iter().map(|c| c.id).collect::<Vec<_>>(),
            vec!["handoff_initialized", "cmd_test"],
            "report order, not request order"
        );
        assert_eq!(
            report
                .commands_to_run
                .iter()
                .map(|c| c.id)
                .collect::<Vec<_>>(),
            vec!["cmd_test"]
        );
        assert!(
            report.ready,
            "the failing claude_md_todo was not asked for: {report:?}"
        );
    }

    #[test]
    fn a_checks_filter_rejects_unknown_ids_and_results_outside_it() {
        let project = Project::ready("test = \"true\"\nbuild = \"make\"\n");
        let unknown = vec!["cmd_test".to_string(), "cmd_tset".to_string()];
        let mut request = project.request(&[]);
        request.checks = Some(&unknown);
        let err = diagnose(&request).unwrap_err().to_string();
        assert!(
            err.contains("cmd_tset") && err.contains("handoff_initialized"),
            "{err}"
        );

        let only_test = vec!["cmd_test".to_string()];
        let results = [result("cmd_build", "make", 0)];
        let mut request = project.request(&results);
        request.checks = Some(&only_test);
        let err = diagnose(&request).unwrap_err().to_string();
        assert!(err.contains("cmd_build"), "{err}");

        let empty: Vec<String> = Vec::new();
        let mut request = project.request(&[]);
        request.checks = Some(&empty);
        assert!(
            diagnose(&request).is_err(),
            "an empty filter asks for nothing"
        );
    }

    #[test]
    fn a_requested_run_without_a_test_command_is_not_ready_even_when_filtered() {
        let project = Project::ready("build = \"make\"\n");
        let only_build = vec!["cmd_build".to_string()];
        let results = [result("cmd_build", "make", 0)];
        let mut request = project.request(&results);
        request.checks = Some(&only_build);
        let report = diagnose(&request).unwrap();
        assert_eq!(status(&report, "cmd_build"), CheckStatus::Pass);
        assert!(!report.ready);
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("commands.test") || w.contains("[commands] test")),
            "the reason is given even though no reported check failed: {:?}",
            report.warnings
        );
    }

    /// Another failure must not hide the missing test command: with
    /// `commands_defined` filtered out, the warning is the only place that
    /// names it, and fixing the reported failure alone would not make the
    /// project ready.
    #[test]
    fn a_filtered_run_without_a_test_command_warns_even_when_another_check_fails() {
        let project = Project::ready("build = \"make\"\n");
        let only_build = vec!["cmd_build".to_string()];
        let results = [result("cmd_build", "make", 2)];
        let mut request = project.request(&results);
        request.checks = Some(&only_build);
        let report = diagnose(&request).unwrap();
        assert_eq!(failed_ids(&report), vec!["cmd_build"]);
        assert!(!report.ready);
        assert_eq!(
            report
                .warnings
                .iter()
                .filter(|w| w.contains("[commands] test"))
                .count(),
            1,
            "{:?}",
            report.warnings
        );
    }

    /// When `commands_defined` is part of the report it carries the reason
    /// itself, so the warning would only repeat it.
    #[test]
    fn a_run_without_a_test_command_is_not_warned_about_when_commands_defined_says_it() {
        let project = Project::ready("build = \"make\"\n");
        let results = [result("cmd_build", "make", 0)];
        for wanted in [
            None,
            Some(vec![
                "commands_defined".to_string(),
                "cmd_build".to_string(),
            ]),
        ] {
            let mut request = project.request(&results);
            request.checks = wanted.as_deref();
            let report = diagnose(&request).unwrap();
            assert_eq!(failed_ids(&report), vec!["commands_defined"]);
            assert!(!report.ready);
            assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        }
    }

    // --- never starts a process (SPEC-ONB-002 AC6) ---

    /// The diagnosis and its MCP handler must stay free of anything that can
    /// start a process, and of the CLI's command runner. The real-binary
    /// tripwire test shows the behavior; this keeps the code from growing a
    /// path the tripwire's scenarios do not happen to reach.
    #[test]
    fn the_diagnosis_and_its_handler_cannot_start_a_process() {
        let production = |source: &'static str| {
            source
                .split("#[cfg(test)]")
                .next()
                .unwrap_or_default()
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        for (file, source) in [
            ("setup/doctor.rs", production(include_str!("doctor.rs"))),
            (
                "mcp/handlers/doctor.rs",
                production(include_str!("../mcp/handlers/doctor.rs")),
            ),
        ] {
            for forbidden in [
                "process::",
                "std::process",
                "Command::new",
                "spawn",
                "runner",
                "resolve_handoff_dir",
                "ensure_handoff_exists",
                "detect_worktree",
                "storage::handoff_dir",
                "storage::git",
            ] {
                assert!(
                    !source.contains(forbidden),
                    "{file} must not use `{forbidden}`: it could start a process"
                );
            }
        }
    }

    // --- no side effects ---

    #[test]
    fn a_diagnosis_changes_nothing_on_disk() {
        fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
            let mut files = Vec::new();
            let mut stack = vec![dir.to_path_buf()];
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
        let project = Project::ready("test = \"touch ran-test\"\nbuild = \"touch ran-build\"\n");
        let before = snapshot(project.root.path());
        project.diagnose();
        project.diagnose_with(&[result("cmd_test", "touch ran-test", 0)]);
        project.diagnose_with(&[result("cmd_test", "something else", 1)]);
        assert_eq!(snapshot(project.root.path()), before);
        assert!(!project.dir.join("ran-test").exists());
        assert!(!project.dir.join("ran-build").exists());
    }

    // --- text rendering ---

    #[test]
    fn text_rendering_lists_every_check_and_the_verdict() {
        let project = Project::ready("test = \"cargo test\"\n");
        project.write("CLAUDE.md", &format!("{FILLED_CLAUDE_MD}{TODO_MARKER}\n"));
        let report = project.diagnose();
        let text = render_text(&report);
        for c in &report.checks {
            assert!(text.contains(c.id), "{}: {text}", c.id);
        }
        assert!(text.contains("[fail] claude_md_todo"), "{text}");
        assert!(text.contains("[pass] handoff_initialized"), "{text}");
        assert!(text.contains("[skip] cmd_test"), "{text}");
        assert!(text.contains("next:"), "{text}");
        assert!(text.contains("Ready: no"), "{text}");
        assert!(
            text.contains("--run"),
            "the commands that are waiting are mentioned: {text}"
        );

        let ready = Project::ready("test = \"true\"\n");
        let text = render_text(&ready.diagnose_with(&[result("cmd_test", "true", 0)]));
        assert!(text.contains("Ready: yes"), "{text}");
        assert!(text.contains("[pass] cmd_test"), "{text}");
    }
}
