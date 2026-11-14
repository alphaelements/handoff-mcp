use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

pub mod doctor;
mod infer;
mod runner;
mod scaffold;

const HOOK_SERVER: &str = "handoff";
const HOOK_TOOL_QUERY: &str = "handoff_memory_query";
const CLAUDE_MD_MARKER: &str = "## Session Handoff";

const CLAUDE_MD_TEMPLATE: &str = include_str!("../templates/claude-md-section.md");

fn settings_path() -> Result<PathBuf> {
    // An empty HOME/USERPROFILE would silently resolve to a relative
    // `.claude/settings.json`, so treat it as unset. Mirrors
    // `storage::expand_tilde`'s home resolution.
    let home = std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .filter(|h| !h.is_empty())
        .context("cannot determine home directory (HOME / USERPROFILE not set)")?;
    Ok(Path::new(&home).join(".claude").join("settings.json"))
}

fn mcp_json_path() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    cwd.join(".mcp.json")
}

fn claude_md_path() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    cwd.join("CLAUDE.md")
}

fn has_claude_md_handoff_section(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    text.contains(CLAUDE_MD_MARKER)
}

fn ensure_claude_md_section(path: &Path, force: bool) -> Result<bool> {
    let has_section = path.exists() && has_claude_md_handoff_section(path);

    if has_section && !force {
        return Ok(false);
    }

    let mut content = if path.exists() {
        std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?
    } else {
        String::from("# CLAUDE.md\n")
    };

    if has_section {
        content = replace_claude_md_section(&content);
    }

    if !content.ends_with('\n') {
        content.push('\n');
    }
    content.push('\n');
    content.push_str(CLAUDE_MD_TEMPLATE);

    std::fs::write(path, &content)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(true)
}

fn replace_claude_md_section(text: &str) -> String {
    let Some(start) = text.find(CLAUDE_MD_MARKER) else {
        return text.to_string();
    };
    let after_marker = start + CLAUDE_MD_MARKER.len();
    let end = text[after_marker..]
        .find("\n## ")
        .map(|pos| after_marker + pos)
        .unwrap_or(text.len());
    let mut result = text[..start].trim_end_matches('\n').to_string();
    let trailing = text[end..].trim_start_matches('\n');
    if !trailing.is_empty() {
        result.push_str("\n\n");
        result.push_str(trailing);
    }
    if !result.ends_with('\n') {
        result.push('\n');
    }
    result
}

fn build_mcp_server_entry() -> Value {
    serde_json::json!({
        "type": "stdio",
        "command": "handoff-mcp",
        "args": [],
        "env": {}
    })
}

fn ensure_mcp_json_entry(path: &Path) -> Result<bool> {
    let mut root = if path.exists() {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        serde_json::from_str::<Value>(&text)
            .with_context(|| format!("failed to parse {}", path.display()))?
    } else {
        serde_json::json!({ "mcpServers": {} })
    };

    let servers = root
        .as_object_mut()
        .context(".mcp.json root is not an object")?
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .context(".mcp.json 'mcpServers' is not an object")?;

    if servers.contains_key(HOOK_SERVER) {
        return Ok(false);
    }

    servers.insert(HOOK_SERVER.to_string(), build_mcp_server_entry());

    let text = serde_json::to_string_pretty(&root)?;
    std::fs::write(path, text + "\n")
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(true)
}

fn ensure_global_mcp_entry(settings: &mut Value) -> Result<bool> {
    let obj = settings
        .as_object_mut()
        .context("settings.json root is not an object")?;
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .context("settings.json 'mcpServers' is not an object")?;

    if servers.contains_key(HOOK_SERVER) {
        return Ok(false);
    }

    servers.insert(HOOK_SERVER.to_string(), build_mcp_server_entry());
    Ok(true)
}

fn has_global_mcp_entry(settings: &Value) -> bool {
    settings
        .get("mcpServers")
        .and_then(|s| s.get(HOOK_SERVER))
        .is_some()
}

fn remove_global_mcp_entry(settings: &mut Value) -> bool {
    let Some(servers) = settings
        .get_mut("mcpServers")
        .and_then(|v| v.as_object_mut())
    else {
        return false;
    };
    let removed = servers.remove(HOOK_SERVER).is_some();
    if removed && servers.is_empty() {
        if let Some(obj) = settings.as_object_mut() {
            obj.remove("mcpServers");
        }
    }
    removed
}

fn has_mcp_json_entry(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(root) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    root.get("mcpServers")
        .and_then(|s| s.get(HOOK_SERVER))
        .is_some()
}

fn read_settings(path: &Path) -> Result<Value> {
    if path.exists() {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
    } else {
        Ok(Value::Object(serde_json::Map::new()))
    }
}

fn write_settings(path: &Path, value: &Value) -> Result<()> {
    let parent = path
        .parent()
        .context("settings path has no parent directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create {}", parent.display()))?;

    let text = serde_json::to_string_pretty(value)?;
    let tmp = parent.join(".settings.json.tmp");
    std::fs::write(&tmp, text + "\n")
        .with_context(|| format!("failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("failed to rename {} -> {}", tmp.display(), path.display()))
}

fn mcp_tool_hook(tool: &str, input: Value) -> Value {
    serde_json::json!({
        "type": "mcp_tool",
        "server": HOOK_SERVER,
        "tool": tool,
        "input": input
    })
}

fn build_hooks_config() -> BTreeMap<&'static str, Value> {
    let mut hooks = BTreeMap::new();

    hooks.insert(
        "UserPromptSubmit",
        serde_json::json!([{
            "hooks": [mcp_tool_hook(HOOK_TOOL_QUERY, serde_json::json!({
                "project_dir": "${CLAUDE_PROJECT_DIR}",
                "session_id": "${session_id}",
                "text": "${prompt}"
            }))]
        }]),
    );

    hooks.insert(
        "PreToolUse",
        serde_json::json!([{
            "matcher": "Edit|Write|MultiEdit",
            "hooks": [mcp_tool_hook(HOOK_TOOL_QUERY, serde_json::json!({
                "project_dir": "${CLAUDE_PROJECT_DIR}",
                "session_id": "${session_id}",
                "tool_name": "${tool_name}",
                "text": "${tool_input.file_path}",
                "file_paths": ["${tool_input.file_path}"]
            }))]
        }]),
    );

    hooks
}

fn has_handoff_hook(arr: &Value) -> bool {
    let Some(entries) = arr.as_array() else {
        return false;
    };
    for entry in entries {
        let Some(hooks) = entry.get("hooks").and_then(|v| v.as_array()) else {
            continue;
        };
        for hook in hooks {
            if hook.get("server").and_then(|v| v.as_str()) == Some(HOOK_SERVER) {
                return true;
            }
        }
    }
    false
}

/// Legacy event that used to carry a synchronous `handoff_memory_cleanup`
/// hook. Removed as a desired hook in this fix (see wiki/100-stdio-concurrency.md)
/// because it was the trigger for the VSCode hang under many parallel
/// sub-agents. Still used to detect and migrate away stale installs from
/// before this fix.
const LEGACY_SESSION_START_EVENT: &str = "SessionStart";

/// True if `settings` still has a handoff-owned hook registered under the
/// legacy `SessionStart` event (i.e. an install performed before this fix
/// removed the synchronous cleanup hook). Used both to auto-migrate on
/// `setup` re-run and to flag the issue in `setup --check`.
fn has_legacy_session_start_hook(settings: &Value) -> bool {
    settings
        .get("hooks")
        .and_then(|h| h.get(LEGACY_SESSION_START_EVENT))
        .map(has_handoff_hook)
        .unwrap_or(false)
}

/// Removes handoff-owned hook entries from the legacy `SessionStart` event,
/// leaving any other (non-handoff) hooks registered under that event intact.
/// Returns true if anything was removed.
fn strip_legacy_session_start_hook(hooks_obj: &mut serde_json::Map<String, Value>) -> bool {
    let Some(arr) = hooks_obj
        .get_mut(LEGACY_SESSION_START_EVENT)
        .and_then(|v| v.as_array_mut())
    else {
        return false;
    };

    let before = arr.len();
    arr.retain(|entry| {
        let Some(hooks) = entry.get("hooks").and_then(|v| v.as_array()) else {
            return true;
        };
        !hooks
            .iter()
            .any(|h| h.get("server").and_then(|v| v.as_str()) == Some(HOOK_SERVER))
    });
    let removed = arr.len() != before;

    if arr.is_empty() {
        hooks_obj.remove(LEGACY_SESSION_START_EVENT);
    }

    removed
}

/// What `handoff-mcp setup` was asked to do. Build one with
/// [`SetupCommand::parse`], or from `SetupOptions::default()`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SetupOptions {
    /// `--check`: report the current state, change nothing.
    pub check: bool,
    /// `--uninstall`: remove the handoff hooks (and the global MCP entry).
    pub uninstall: bool,
    /// `--mcp-json`: only add the MCP server entry.
    pub mcp_json: bool,
    /// `-y` / `--yes`: answer every prompt with yes.
    pub yes: bool,
    /// `--global`: use `~/.claude/settings.json` instead of `.mcp.json`.
    pub global: bool,
    /// `--force`: replace an existing `Session Handoff` section.
    pub force: bool,
    /// `--claude-md-scaffold`: only add the project sections to `CLAUDE.md`
    /// (and record the detected commands), without prompting.
    pub claude_md_scaffold: bool,
    /// `--run` (with `--check`): run the `[commands]` commands and judge the
    /// project by their results.
    pub run_commands: bool,
    /// `--json` (with `--check`): print the diagnosis as the JSON
    /// `handoff_doctor` returns, and nothing else.
    pub json: bool,
}

/// A parsed `handoff-mcp setup ...` command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupCommand {
    Run(SetupOptions),
    Help,
}

/// Usage text of `handoff-mcp setup`, shared by `setup --help` and the
/// top-level help.
pub const SETUP_USAGE: &str = "\
SETUP:
    handoff-mcp setup              Install hooks + MCP server + CLAUDE.md sections,
                                   and record detected commands in [commands]
    handoff-mcp setup --global     Use ~/.claude/settings.json mcpServers (not .mcp.json)
    handoff-mcp setup --check      Check if hooks, MCP, CLAUDE.md and [commands] are ready
                                   (exit code 1 when the project is not ready)
    handoff-mcp setup --check --run
                                   Also run the [commands] commands and judge the results
    handoff-mcp setup --check --json
                                   Print the diagnosis as JSON (same as handoff_doctor)
    handoff-mcp setup --uninstall  Remove handoff hooks and MCP entry
    handoff-mcp setup --mcp-json   Add handoff MCP server entry only (non-interactive)
    handoff-mcp setup --claude-md-scaffold
                                   Add the project sections to CLAUDE.md and record
                                   detected commands only (non-interactive)
    handoff-mcp setup --force      Replace existing CLAUDE.md handoff section with latest
    handoff-mcp setup -y           Skip all confirmation prompts";

impl SetupCommand {
    /// Parses the arguments that follow `setup`. An argument this command
    /// does not know is an error: silently ignoring a misspelled flag would
    /// run a different (and file-modifying) action than the one asked for.
    pub fn parse(args: &[String]) -> Result<Self> {
        let mut opts = SetupOptions::default();
        for arg in args {
            match arg.as_str() {
                "--help" | "-h" => return Ok(Self::Help),
                "--check" => opts.check = true,
                "--uninstall" => opts.uninstall = true,
                "--mcp-json" => opts.mcp_json = true,
                "-y" | "--yes" => opts.yes = true,
                "--global" => opts.global = true,
                "--force" => opts.force = true,
                "--claude-md-scaffold" => opts.claude_md_scaffold = true,
                "--run" => opts.run_commands = true,
                "--json" => opts.json = true,
                other => anyhow::bail!(
                    "unknown setup argument '{other}'. Run `handoff-mcp setup --help` for usage."
                ),
            }
        }
        opts.validate()?;
        Ok(Self::Run(opts))
    }
}

impl SetupOptions {
    /// Rejects combinations that name two different actions.
    fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !(self.check && self.uninstall),
            "--check and --uninstall cannot be used together"
        );
        for (set, flag) in [(self.run_commands, "--run"), (self.json, "--json")] {
            anyhow::ensure!(
                !set || self.check,
                "{flag} is only used together with --check"
            );
        }
        if self.claude_md_scaffold {
            for (set, flag) in [
                (self.check, "--check"),
                (self.uninstall, "--uninstall"),
                (self.mcp_json, "--mcp-json"),
                (self.global, "--global"),
                (self.force, "--force"),
            ] {
                anyhow::ensure!(
                    !set,
                    "--claude-md-scaffold cannot be used together with {flag}"
                );
            }
        }
        Ok(())
    }
}

pub fn run_setup(check_only: bool, uninstall: bool) -> Result<()> {
    run_setup_with_opts(check_only, uninstall, false, false, false, false)
}

pub fn run_setup_with_opts(
    check_only: bool,
    uninstall: bool,
    mcp_json: bool,
    yes: bool,
    global: bool,
    force: bool,
) -> Result<()> {
    run(SetupOptions {
        check: check_only,
        uninstall,
        mcp_json,
        yes,
        global,
        force,
        ..SetupOptions::default()
    })
    .map(|_exit_code| ())
}

/// Exit code of a setup run that did what was asked.
const EXIT_OK: i32 = 0;
/// Exit code of `setup --check` when the project is not ready.
const EXIT_NOT_READY: i32 = 1;

/// Runs `handoff-mcp setup` in the current directory and returns the process
/// exit code: 0, or 1 when `--check` found the project not ready. A setup
/// that could not be carried out is an `Err`.
pub fn run(opts: SetupOptions) -> Result<i32> {
    opts.validate()?;

    if opts.claude_md_scaffold {
        let project_dir = current_project_dir()?;
        run_project_scaffold(&project_dir, true)?;
        return Ok(EXIT_OK);
    }

    if opts.check && opts.json {
        // JSON mode prints the diagnosis only, so it needs neither the hook
        // settings nor a home directory to read them from.
        let report = diagnose_current_project(opts.run_commands)?;
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(readiness_exit_code(&report));
    }

    let path = settings_path()?;
    let mut settings = read_settings(&path)?;

    if opts.check {
        run_check(&settings, &path, opts.global)?;
        let report = diagnose_current_project(opts.run_commands)?;
        println!("\n{}", doctor::render_text(&report).trim_end());
        return Ok(readiness_exit_code(&report));
    }

    if opts.uninstall {
        run_uninstall(&mut settings, &path, opts.global)?;
        return Ok(EXIT_OK);
    }

    if opts.mcp_json {
        run_mcp_json_only(opts.global, &mut settings, &path)?;
        return Ok(EXIT_OK);
    }

    let project_dir = current_project_dir()?;
    run_install(&mut settings, &path, opts.yes, opts.global, opts.force)?;
    run_project_scaffold(&project_dir, opts.yes)?;

    println!("\nRestart Claude Code for changes to take effect.");
    Ok(EXIT_OK)
}

fn readiness_exit_code(report: &doctor::DoctorReport) -> i32 {
    if report.ready {
        EXIT_OK
    } else {
        EXIT_NOT_READY
    }
}

/// The readiness diagnosis of the current directory (SPEC-ONB-002), the same
/// one `handoff_doctor` gives. With `run_commands` the `[commands]` commands
/// are executed here first and judged by their results; this CLI path is the
/// only one that ever runs them.
fn diagnose_current_project(run_commands: bool) -> Result<doctor::DoctorReport> {
    let project_dir = current_project_dir()?;
    // The MCP tool is handed a canonical project path; use the same form so
    // both entry points describe the project identically. A directory that
    // cannot be resolved is diagnosed as it is: the checks then report what
    // they cannot find.
    let project_dir = std::fs::canonicalize(&project_dir).unwrap_or(project_dir);
    let home_dir = crate::storage::home_dir().map(PathBuf::from);
    let request = |results: &[doctor::CommandResult], run_requested: bool| {
        doctor::diagnose(&doctor::DoctorRequest {
            project_dir: &project_dir,
            home_dir: home_dir.as_deref(),
            checks: None,
            results,
            source: doctor::ResultSource::Executed,
            run_requested,
        })
    };
    if !run_commands {
        return request(&[], false);
    }

    let pending = request(&[], false)?.commands_to_run;
    let results: Vec<doctor::CommandResult> = pending
        .iter()
        .map(|pending| {
            // Progress goes to stderr so that `--json` output stays pure.
            eprintln!("  running {}: {}", pending.id, pending.command);
            let execution = runner::run_command(
                &pending.command,
                &project_dir,
                std::time::Duration::from_secs(pending.timeout_sec),
            );
            doctor::CommandResult {
                id: pending.id.to_string(),
                command: pending.command.clone(),
                exit_code: execution.exit_code,
                duration_ms: Some(execution.duration_ms),
                output_tail: Some(execution.output_tail),
                error: execution.error,
            }
        })
        .collect();
    request(&results, true)
}

fn current_project_dir() -> Result<PathBuf> {
    std::env::current_dir().context("cannot determine the current directory")
}

/// Records the commands detected in the project's manifests under
/// `[commands]` and adds the project sections `CLAUDE.md` lacks
/// (SPEC-ONB-001). Each of the two files is only written when it has
/// something to gain, and only after the user agreed (`assume_yes` skips the
/// question).
fn run_project_scaffold(project_dir: &Path, assume_yes: bool) -> Result<()> {
    let inference = infer::infer_commands(project_dir);
    for note in &inference.notes {
        println!("  {note}");
    }

    let config_path = crate::storage::handoff_dir(project_dir).join("config.toml");
    let listing = |commands: &[(&'static str, String)]| {
        commands
            .iter()
            .map(|(key, command)| format!("{key} = \"{command}\""))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if inference.commands.is_empty() {
        println!(
            "  [commands]: no commands detected in the project's manifests. Write them under \
             [commands] in .handoff/config.toml."
        );
    } else {
        match scaffold::pending_commands(&config_path, &inference.commands) {
            Ok(None) => {
                let detected: Vec<_> = inference
                    .commands
                    .iter()
                    .map(|(key, command)| (*key, command.clone()))
                    .collect();
                println!(
                    "  [commands]: .handoff/config.toml not found, so the detected commands \
                     ({}) were not recorded. Initialize the project (`handoff_init`), then run \
                     `handoff-mcp setup` again.",
                    listing(&detected)
                );
            }
            Ok(Some(pending)) if pending.is_empty() => {
                println!("  [commands]: nothing to add to .handoff/config.toml");
            }
            Ok(Some(pending)) => {
                let agreed = assume_yes || {
                    println!();
                    prompt_yn(&format!(
                        "Record detected commands in .handoff/config.toml [commands] ({})?",
                        listing(&pending)
                    ))
                };
                if agreed {
                    match scaffold::record_inferred_commands(&config_path, &inference.commands)? {
                        scaffold::CommandsOutcome::Added(added) => println!(
                            "  [commands]: added {} to .handoff/config.toml",
                            listing(&added)
                        ),
                        // The file changed between the question and the write.
                        scaffold::CommandsOutcome::Unchanged
                        | scaffold::CommandsOutcome::NoConfig => {
                            println!("  [commands]: nothing was added to .handoff/config.toml");
                        }
                    }
                } else {
                    println!("  [commands]: skipped");
                }
            }
            // A config.toml that cannot be read is the user's to repair; it
            // must not keep CLAUDE.md from being prepared, so say so and
            // carry on.
            Err(e) => {
                eprintln!("  [commands]: skipped, .handoff/config.toml is not usable ({e:#})");
            }
        }
    }

    let claude_md = project_dir.join("CLAUDE.md");
    let missing = scaffold::missing_section_titles(&claude_md)?;
    if missing.is_empty() {
        println!("  CLAUDE.md: project sections already present");
    } else {
        let listing = missing.join(", ");
        let agreed = assume_yes || {
            println!();
            prompt_yn(&format!("Add project sections to CLAUDE.md ({listing})?"))
        };
        if agreed {
            let commands = scaffold::effective_commands(&config_path, &inference.commands);
            let outcome = scaffold::ensure_scaffold_sections(&claude_md, &commands)?;
            let verb = if outcome.created {
                "created with"
            } else {
                "added"
            };
            println!(
                "  CLAUDE.md: {verb} project sections: {}",
                outcome.added.join(", ")
            );
        } else {
            println!("  CLAUDE.md: project sections skipped");
        }
    }

    let unfilled = scaffold::todo_marker_count(&claude_md);
    if unfilled > 0 {
        println!(
            "  CLAUDE.md: {unfilled} field(s) still marked {}. Fill each one in (write `n/a` \
             when the project has no such step).",
            scaffold::TODO_MARKER
        );
    }
    Ok(())
}

fn run_check(settings: &Value, path: &Path, global: bool) -> Result<()> {
    println!("Settings file: {}", path.display());

    let hooks_obj = settings.get("hooks");
    let desired = build_hooks_config();
    let mut all_ok = true;

    for event in desired.keys() {
        let installed = hooks_obj
            .and_then(|h| h.get(*event))
            .map(has_handoff_hook)
            .unwrap_or(false);

        if installed {
            println!("  {event}: installed");
        } else {
            println!("  {event}: NOT installed");
            all_ok = false;
        }
    }

    if has_legacy_session_start_hook(settings) {
        println!(
            "  {LEGACY_SESSION_START_EVENT}: legacy handoff_memory_cleanup hook found (removed in this version)"
        );
        println!(
            "\nWARNING: a synchronous SessionStart cleanup hook from an older install was \
             found. This is the known trigger for VSCode hangs under many parallel \
             sub-agents. Run `handoff-mcp setup` (without --check) to remove it."
        );
        all_ok = false;
    }

    if global {
        if has_global_mcp_entry(settings) {
            println!("  settings.json mcpServers: handoff server configured (global)");
        } else {
            println!("  settings.json mcpServers: handoff server NOT configured (global)");
            all_ok = false;
        }
    } else {
        let mcp_path = mcp_json_path();
        if has_mcp_json_entry(&mcp_path) {
            println!("  .mcp.json: handoff server configured");
        } else if mcp_path.exists() {
            println!("  .mcp.json: handoff server NOT configured");
            all_ok = false;
        } else {
            println!("  .mcp.json: not found (hooks need it — run `handoff-mcp setup`)");
            all_ok = false;
        }
    }

    let claude_md = claude_md_path();
    if has_claude_md_handoff_section(&claude_md) {
        println!("  CLAUDE.md: handoff section present");
    } else if claude_md.exists() {
        println!("  CLAUDE.md: handoff section NOT present");
        all_ok = false;
    } else {
        println!("  CLAUDE.md: not found");
        all_ok = false;
    }

    if all_ok {
        println!("\nAll hooks are configured. Memory auto-injection is active.");
    } else {
        println!("\nSome items need attention. Run `handoff-mcp setup` to install/migrate them.");
    }

    Ok(())
}

fn prompt_yn(question: &str) -> bool {
    use std::io::{self, Write};
    print!("{question} [Y/n] ");
    io::stdout().flush().ok();
    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return true;
    }
    let trimmed = input.trim().to_lowercase();
    trimmed.is_empty() || trimmed == "y" || trimmed == "yes"
}

fn run_mcp_json_only(global: bool, settings: &mut Value, settings_path: &Path) -> Result<()> {
    if global {
        if has_global_mcp_entry(settings) {
            println!("  settings.json mcpServers: handoff server already configured (global). Nothing to do.");
            return Ok(());
        }
        let added = ensure_global_mcp_entry(settings)?;
        if added {
            write_settings(settings_path, settings)?;
            println!("  settings.json mcpServers: added handoff server entry (global)");
            println!("\nRestart Claude Code for changes to take effect.");
        }
    } else {
        let mcp_path = mcp_json_path();
        if has_mcp_json_entry(&mcp_path) {
            println!("  .mcp.json: handoff server already configured. Nothing to do.");
            return Ok(());
        }
        let added = ensure_mcp_json_entry(&mcp_path)?;
        if added {
            println!("  .mcp.json: added handoff server entry");
            println!("\nRestart Claude Code for changes to take effect.");
        }
    }
    Ok(())
}

fn run_install(
    settings: &mut Value,
    path: &Path,
    yes: bool,
    global: bool,
    force: bool,
) -> Result<()> {
    let obj = settings
        .as_object_mut()
        .context("settings.json root is not an object")?;

    let hooks_val = obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let hooks_obj = hooks_val
        .as_object_mut()
        .context("settings.json 'hooks' is not an object")?;

    let desired = build_hooks_config();
    let mut installed = 0u32;
    let mut skipped = 0u32;

    for (event, config) in desired {
        let existing = hooks_obj.get(event);
        if existing.map(has_handoff_hook).unwrap_or(false) {
            println!("  {event}: already installed, skipping");
            skipped += 1;
            continue;
        }

        match existing {
            Some(Value::Array(arr)) => {
                let mut merged = arr.clone();
                if let Some(new_entries) = config.as_array() {
                    merged.extend(new_entries.iter().cloned());
                }
                hooks_obj.insert(event.to_string(), Value::Array(merged));
                println!("  {event}: merged with existing hooks");
            }
            _ => {
                hooks_obj.insert(event.to_string(), config);
                println!("  {event}: installed");
            }
        }
        installed += 1;
    }

    let migrated = strip_legacy_session_start_hook(hooks_obj);
    if migrated {
        println!(
            "  {LEGACY_SESSION_START_EVENT}: removed legacy handoff_memory_cleanup hook \
             (known VSCode hang trigger)"
        );
    }

    let mut settings_dirty = installed > 0 || migrated;

    if global {
        if has_global_mcp_entry(settings) {
            println!("  settings.json mcpServers: handoff server already configured (global)");
        } else {
            let added = ensure_global_mcp_entry(settings)?;
            if added {
                println!("  settings.json mcpServers: added handoff server entry (global)");
                settings_dirty = true;
            }
        }
    }

    if settings_dirty {
        write_settings(path, settings)?;
        println!("\nWrote {path}", path = path.display());
        if installed > 0 {
            println!("{installed} hook(s) installed, {skipped} already present.");
        }
        if migrated {
            println!("Legacy SessionStart cleanup hook removed. See README for migration details.");
        }
    } else {
        println!("\nAll hooks already installed.");
    }

    if !global {
        let mcp_path = mcp_json_path();
        let needs_mcp = !has_mcp_json_entry(&mcp_path);

        if needs_mcp {
            let should_write = if yes {
                true
            } else {
                println!();
                prompt_yn("Add handoff server to .mcp.json? (required for hooks)")
            };

            if should_write {
                let added = ensure_mcp_json_entry(&mcp_path)?;
                if added {
                    println!("  .mcp.json: added handoff server entry");
                }
            } else {
                println!(
                    "  .mcp.json: skipped. Hooks will not work without a handoff server.\n  \
                     Run `handoff-mcp setup --mcp-json` later, or add it manually."
                );
            }
        } else {
            println!("  .mcp.json: handoff server already configured");
        }
    }

    // CLAUDE.md template
    let claude_md = claude_md_path();
    let has_section = has_claude_md_handoff_section(&claude_md);
    if has_section && !force {
        println!("  CLAUDE.md: handoff section already present");
    } else {
        let prompt_msg = if has_section {
            "Replace existing session handoff section in CLAUDE.md?"
        } else {
            "Add session handoff section to CLAUDE.md?"
        };
        let should_write = if yes || force {
            true
        } else {
            println!();
            prompt_yn(prompt_msg)
        };

        if should_write {
            let existed = claude_md.exists();
            let added = ensure_claude_md_section(&claude_md, force)?;
            if added {
                let verb = if has_section {
                    "replaced"
                } else if existed {
                    "appended to"
                } else {
                    "created"
                };
                println!("  CLAUDE.md: session handoff section {verb}");
            }
        } else {
            println!("  CLAUDE.md: skipped");
        }
    }

    Ok(())
}

fn run_uninstall(settings: &mut Value, path: &Path, global: bool) -> Result<()> {
    let mut anything_removed = false;

    if let Some(hooks_obj) = settings.get_mut("hooks").and_then(|v| v.as_object_mut()) {
        let events: Vec<String> = hooks_obj.keys().cloned().collect();
        let mut removed = 0u32;

        for event in &events {
            let Some(arr) = hooks_obj.get_mut(event).and_then(|v| v.as_array_mut()) else {
                continue;
            };

            let before = arr.len();
            arr.retain(|entry| {
                let Some(hooks) = entry.get("hooks").and_then(|v| v.as_array()) else {
                    return true;
                };
                !hooks
                    .iter()
                    .any(|h| h.get("server").and_then(|v| v.as_str()) == Some(HOOK_SERVER))
            });

            let after = arr.len();
            if before != after {
                println!("  {event}: removed handoff hook(s)");
                removed += 1;
            }

            if arr.is_empty() {
                hooks_obj.remove(event);
            }
        }

        if hooks_obj.is_empty() {
            if let Some(obj) = settings.as_object_mut() {
                obj.remove("hooks");
            }
        }

        if removed > 0 {
            anything_removed = true;
            println!("{removed} hook event(s) cleaned up.");
        }
    }

    if global && remove_global_mcp_entry(settings) {
        println!("  settings.json mcpServers: removed handoff server entry (global)");
        anything_removed = true;
    }

    if anything_removed {
        write_settings(path, settings)?;
        println!("\nWrote {path}", path = path.display());
        println!("\nRestart Claude Code for changes to take effect.");
    } else {
        println!("No handoff hooks found. Nothing to remove.");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_hooks_has_two_events() {
        let hooks = build_hooks_config();
        assert_eq!(hooks.len(), 2);
        assert!(hooks.contains_key("UserPromptSubmit"));
        assert!(hooks.contains_key("PreToolUse"));
        assert!(!hooks.contains_key("SessionStart"));
    }

    #[test]
    fn has_handoff_hook_detects_presence() {
        let arr = serde_json::json!([{
            "hooks": [{"type": "mcp_tool", "server": "handoff", "tool": "handoff_memory_query"}]
        }]);
        assert!(has_handoff_hook(&arr));
    }

    #[test]
    fn has_handoff_hook_returns_false_for_other_server() {
        let arr = serde_json::json!([{
            "hooks": [{"type": "mcp_tool", "server": "other", "tool": "other_tool"}]
        }]);
        assert!(!has_handoff_hook(&arr));
    }

    #[test]
    fn has_handoff_hook_returns_false_for_empty() {
        assert!(!has_handoff_hook(&serde_json::json!([])));
    }

    #[test]
    fn install_into_empty_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let mut settings = Value::Object(serde_json::Map::new());
        run_install(&mut settings, &path, false, false, false).unwrap();

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let hooks = written.get("hooks").unwrap().as_object().unwrap();
        assert!(hooks.contains_key("UserPromptSubmit"));
        assert!(hooks.contains_key("PreToolUse"));
        assert!(!hooks.contains_key("SessionStart"));
    }

    #[test]
    fn install_merges_with_existing_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let mut settings = serde_json::json!({
            "hooks": {
                "UserPromptSubmit": [{
                    "hooks": [{"type": "command", "command": "my-other-hook"}]
                }]
            }
        });

        run_install(&mut settings, &path, false, false, false).unwrap();

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let user_prompt = written["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(user_prompt.len(), 2);
        assert!(has_handoff_hook(&written["hooks"]["UserPromptSubmit"]));
    }

    #[test]
    fn install_skips_if_already_present() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let mut settings = Value::Object(serde_json::Map::new());
        run_install(&mut settings, &path, false, false, false).unwrap();

        let mut settings2 = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        run_install(&mut settings2, &path, false, false, false).unwrap();

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let user_prompt = written["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(user_prompt.len(), 1);
        assert!(!written["hooks"]
            .as_object()
            .unwrap()
            .contains_key("SessionStart"));
    }

    #[test]
    fn uninstall_removes_handoff_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let mut settings = Value::Object(serde_json::Map::new());
        run_install(&mut settings, &path, false, false, false).unwrap();

        let mut settings2: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        run_uninstall(&mut settings2, &path, false).unwrap();

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(written.get("hooks").is_none());
    }

    #[test]
    fn check_and_uninstall_conflict_is_rejected() {
        assert!(run_setup(true, true).is_err());
    }

    // --- command line ---

    fn parse(args: &[&str]) -> Result<SetupCommand> {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        SetupCommand::parse(&args)
    }

    #[test]
    fn no_arguments_is_a_plain_install() {
        assert_eq!(
            parse(&[]).unwrap(),
            SetupCommand::Run(SetupOptions::default())
        );
    }

    #[test]
    fn every_flag_sets_exactly_its_own_option() {
        let cases: [(&str, SetupOptions); 8] = [
            (
                "--check",
                SetupOptions {
                    check: true,
                    ..Default::default()
                },
            ),
            (
                "--uninstall",
                SetupOptions {
                    uninstall: true,
                    ..Default::default()
                },
            ),
            (
                "--mcp-json",
                SetupOptions {
                    mcp_json: true,
                    ..Default::default()
                },
            ),
            (
                "-y",
                SetupOptions {
                    yes: true,
                    ..Default::default()
                },
            ),
            (
                "--yes",
                SetupOptions {
                    yes: true,
                    ..Default::default()
                },
            ),
            (
                "--global",
                SetupOptions {
                    global: true,
                    ..Default::default()
                },
            ),
            (
                "--force",
                SetupOptions {
                    force: true,
                    ..Default::default()
                },
            ),
            (
                "--claude-md-scaffold",
                SetupOptions {
                    claude_md_scaffold: true,
                    ..Default::default()
                },
            ),
        ];
        for (flag, expected) in cases {
            assert_eq!(
                parse(&[flag]).unwrap(),
                SetupCommand::Run(expected),
                "{flag}"
            );
        }
        assert_eq!(
            parse(&["--global", "-y", "--force"]).unwrap(),
            SetupCommand::Run(SetupOptions {
                global: true,
                yes: true,
                force: true,
                ..Default::default()
            })
        );
    }

    #[test]
    fn run_and_json_are_modifiers_of_check() {
        assert_eq!(
            parse(&["--check", "--run", "--json"]).unwrap(),
            SetupCommand::Run(SetupOptions {
                check: true,
                run_commands: true,
                json: true,
                ..Default::default()
            })
        );
        assert_eq!(
            parse(&["--json", "--check"]).unwrap(),
            SetupCommand::Run(SetupOptions {
                check: true,
                json: true,
                ..Default::default()
            })
        );
        for alone in ["--run", "--json"] {
            for with in [vec![], vec!["-y"], vec!["--uninstall"], vec!["--mcp-json"]] {
                let mut args = with.clone();
                args.push(alone);
                let err = parse(&args).unwrap_err().to_string();
                assert!(
                    err.contains(alone) && err.contains("--check"),
                    "{args:?}: {err}"
                );
            }
        }
    }

    #[test]
    fn help_wins_wherever_it_appears() {
        assert_eq!(parse(&["--help"]).unwrap(), SetupCommand::Help);
        assert_eq!(parse(&["-y", "-h"]).unwrap(), SetupCommand::Help);
    }

    #[test]
    fn unknown_arguments_are_errors_that_name_the_argument() {
        for bad in ["--chek", "--claude-md-scafold", "check", "-Y", ""] {
            let err = parse(&["-y", bad]).unwrap_err().to_string();
            assert!(err.contains(&format!("'{bad}'")), "{bad}: {err}");
        }
    }

    #[test]
    fn scaffold_only_cannot_be_combined_with_another_action() {
        for other in [
            "--check",
            "--uninstall",
            "--mcp-json",
            "--global",
            "--force",
        ] {
            let err = parse(&["--claude-md-scaffold", other])
                .unwrap_err()
                .to_string();
            assert!(err.contains(other), "{other}: {err}");
        }
        assert!(parse(&["--claude-md-scaffold", "-y"]).is_ok());
        assert!(parse(&["--check", "--uninstall"]).is_err());
    }

    #[test]
    fn install_preserves_key_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let original = r#"{
  "env": {"A": "1", "B": "2"},
  "model": "opus",
  "permissions": {"defaultMode": "auto"}
}
"#;
        std::fs::write(&path, original).unwrap();

        let mut settings: Value = serde_json::from_str(original).unwrap();
        run_install(&mut settings, &path, false, false, false).unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        let env_pos = written.find("\"env\"").unwrap();
        let hooks_pos = written.find("\"hooks\"").unwrap();
        let model_pos = written.find("\"model\"").unwrap();
        assert!(env_pos < model_pos, "env should come before model");
        assert!(
            hooks_pos > env_pos,
            "hooks should be appended after existing keys"
        );
    }

    #[test]
    fn install_removes_legacy_session_start_cleanup_hook() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        // Simulate a settings.json written by a pre-fix version of
        // `handoff-mcp setup`, which still installed a SessionStart
        // cleanup hook (the very hang trigger this task removes).
        let mut settings = serde_json::json!({
            "hooks": {
                "SessionStart": [{
                    "hooks": [{
                        "type": "mcp_tool",
                        "server": "handoff",
                        "tool": "handoff_memory_cleanup",
                        "input": {"project_dir": "${CLAUDE_PROJECT_DIR}"}
                    }]
                }],
                "UserPromptSubmit": [{
                    "hooks": [{"type": "mcp_tool", "server": "handoff", "tool": "handoff_memory_query"}]
                }],
                "PreToolUse": [{
                    "matcher": "Edit|Write|MultiEdit",
                    "hooks": [{"type": "mcp_tool", "server": "handoff", "tool": "handoff_memory_query"}]
                }]
            }
        });

        run_install(&mut settings, &path, false, false, false).unwrap();

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let hooks = written.get("hooks").unwrap().as_object().unwrap();
        assert!(
            !hooks.contains_key("SessionStart"),
            "legacy SessionStart cleanup hook must be removed by re-running setup"
        );
        assert!(hooks.contains_key("UserPromptSubmit"));
        assert!(hooks.contains_key("PreToolUse"));
    }

    #[test]
    fn install_preserves_non_handoff_session_start_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let mut settings = serde_json::json!({
            "hooks": {
                "SessionStart": [
                    {"hooks": [{"type": "command", "command": "my-other-hook"}]},
                    {"hooks": [{"type": "mcp_tool", "server": "handoff", "tool": "handoff_memory_cleanup"}]}
                ]
            }
        });

        run_install(&mut settings, &path, false, false, false).unwrap();

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let session_start = written["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(session_start.len(), 1);
        assert!(!has_handoff_hook(&written["hooks"]["SessionStart"]));
    }

    #[test]
    fn check_flags_legacy_session_start_cleanup_hook() {
        let settings = serde_json::json!({
            "hooks": {
                "SessionStart": [{
                    "hooks": [{"type": "mcp_tool", "server": "handoff", "tool": "handoff_memory_cleanup"}]
                }]
            }
        });

        assert!(has_legacy_session_start_hook(&settings));
    }

    #[test]
    fn check_does_not_flag_when_no_legacy_hook_present() {
        let settings = serde_json::json!({
            "hooks": {
                "UserPromptSubmit": [{
                    "hooks": [{"type": "mcp_tool", "server": "handoff", "tool": "handoff_memory_query"}]
                }]
            }
        });

        assert!(!has_legacy_session_start_hook(&settings));
    }

    #[test]
    fn uninstall_preserves_other_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let mut settings = serde_json::json!({
            "hooks": {
                "UserPromptSubmit": [
                    {"hooks": [{"type": "command", "command": "my-hook"}]},
                    {"hooks": [{"type": "mcp_tool", "server": "handoff", "tool": "handoff_memory_query"}]}
                ]
            }
        });
        run_uninstall(&mut settings, &path, false).unwrap();

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let user_prompt = written["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(user_prompt.len(), 1);
        assert!(!has_handoff_hook(&written["hooks"]["UserPromptSubmit"]));
    }

    #[test]
    fn ensure_mcp_json_creates_file_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mcp.json");
        assert!(!path.exists());

        let added = ensure_mcp_json_entry(&path).unwrap();
        assert!(added);
        assert!(path.exists());

        let root: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(root["mcpServers"]["handoff"]["command"]
            .as_str()
            .unwrap()
            .contains("handoff-mcp"));
    }

    #[test]
    fn ensure_mcp_json_adds_to_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mcp.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"context7":{"type":"stdio","command":"npx"}}}"#,
        )
        .unwrap();

        let added = ensure_mcp_json_entry(&path).unwrap();
        assert!(added);

        let root: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(root["mcpServers"].get("context7").is_some());
        assert!(root["mcpServers"].get("handoff").is_some());
    }

    #[test]
    fn ensure_mcp_json_skips_when_already_present() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mcp.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"handoff":{"type":"stdio","command":"handoff-mcp"}}}"#,
        )
        .unwrap();

        let added = ensure_mcp_json_entry(&path).unwrap();
        assert!(!added);
    }

    #[test]
    fn has_mcp_json_entry_checks_correctly() {
        let dir = tempfile::tempdir().unwrap();

        let absent = dir.path().join("absent.json");
        assert!(!has_mcp_json_entry(&absent));

        let present = dir.path().join("present.json");
        std::fs::write(&present, r#"{"mcpServers":{"handoff":{"type":"stdio"}}}"#).unwrap();
        assert!(has_mcp_json_entry(&present));

        let no_handoff = dir.path().join("other.json");
        std::fs::write(
            &no_handoff,
            r#"{"mcpServers":{"context7":{"type":"stdio"}}}"#,
        )
        .unwrap();
        assert!(!has_mcp_json_entry(&no_handoff));
    }

    // --- Global MCP entry tests ---

    #[test]
    fn ensure_global_mcp_adds_to_empty_settings() {
        let mut settings = Value::Object(serde_json::Map::new());
        let added = ensure_global_mcp_entry(&mut settings).unwrap();
        assert!(added);
        assert!(has_global_mcp_entry(&settings));
        assert!(settings["mcpServers"]["handoff"]["command"]
            .as_str()
            .unwrap()
            .contains("handoff-mcp"));
    }

    #[test]
    fn ensure_global_mcp_preserves_existing_servers() {
        let mut settings = serde_json::json!({
            "mcpServers": {
                "context7": {"type": "stdio", "command": "npx"}
            }
        });
        let added = ensure_global_mcp_entry(&mut settings).unwrap();
        assert!(added);
        assert!(settings["mcpServers"].get("context7").is_some());
        assert!(settings["mcpServers"].get("handoff").is_some());
    }

    #[test]
    fn ensure_global_mcp_skips_when_present() {
        let mut settings = serde_json::json!({
            "mcpServers": {
                "handoff": {"type": "stdio", "command": "handoff-mcp"}
            }
        });
        let added = ensure_global_mcp_entry(&mut settings).unwrap();
        assert!(!added);
    }

    #[test]
    fn remove_global_mcp_entry_removes_handoff() {
        let mut settings = serde_json::json!({
            "mcpServers": {
                "handoff": {"type": "stdio"},
                "context7": {"type": "stdio"}
            }
        });
        assert!(remove_global_mcp_entry(&mut settings));
        assert!(settings["mcpServers"].get("context7").is_some());
        assert!(settings["mcpServers"].get("handoff").is_none());
    }

    #[test]
    fn remove_global_mcp_entry_cleans_empty_section() {
        let mut settings = serde_json::json!({
            "mcpServers": {
                "handoff": {"type": "stdio"}
            }
        });
        assert!(remove_global_mcp_entry(&mut settings));
        assert!(settings.get("mcpServers").is_none());
    }

    #[test]
    fn remove_global_mcp_entry_returns_false_when_absent() {
        let mut settings = Value::Object(serde_json::Map::new());
        assert!(!remove_global_mcp_entry(&mut settings));
    }

    #[test]
    fn uninstall_with_global_removes_mcp_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let mut settings = serde_json::json!({
            "hooks": {
                "UserPromptSubmit": [{
                    "hooks": [{"type": "mcp_tool", "server": "handoff", "tool": "handoff_memory_query"}]
                }]
            },
            "mcpServers": {
                "handoff": {"type": "stdio", "command": "handoff-mcp"}
            }
        });

        run_uninstall(&mut settings, &path, true).unwrap();

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(written.get("hooks").is_none());
        assert!(written.get("mcpServers").is_none());
    }

    // --- CLAUDE.md tests ---

    #[test]
    fn ensure_claude_md_creates_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CLAUDE.md");
        assert!(!path.exists());

        let added = ensure_claude_md_section(&path, false).unwrap();
        assert!(added);

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains(CLAUDE_MD_MARKER));
        assert!(content.contains("handoff-mcp"));
    }

    #[test]
    fn ensure_claude_md_appends_to_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CLAUDE.md");
        std::fs::write(&path, "# My Project\n\nSome existing content.\n").unwrap();

        let added = ensure_claude_md_section(&path, false).unwrap();
        assert!(added);

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.starts_with("# My Project"));
        assert!(content.contains("Some existing content."));
        assert!(content.contains(CLAUDE_MD_MARKER));
    }

    #[test]
    fn ensure_claude_md_skips_when_marker_present() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CLAUDE.md");
        std::fs::write(
            &path,
            "# Project\n\n## Session Handoff\n\nCustom handoff instructions.\n",
        )
        .unwrap();

        let added = ensure_claude_md_section(&path, false).unwrap();
        assert!(!added);

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("Custom handoff instructions."));
        assert!(!content.contains("handoff_load_context"));
    }

    #[test]
    fn has_claude_md_handoff_section_detects_correctly() {
        let dir = tempfile::tempdir().unwrap();

        let absent = dir.path().join("absent.md");
        assert!(!has_claude_md_handoff_section(&absent));

        let present = dir.path().join("present.md");
        std::fs::write(&present, "# X\n\n## Session Handoff\n\nfoo\n").unwrap();
        assert!(has_claude_md_handoff_section(&present));

        let without = dir.path().join("without.md");
        std::fs::write(&without, "# X\n\nSome content\n").unwrap();
        assert!(!has_claude_md_handoff_section(&without));
    }

    // --- Force replace tests ---

    #[test]
    fn ensure_claude_md_force_replaces_existing_section() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CLAUDE.md");
        std::fs::write(
            &path,
            "# Project\n\n## Session Handoff\n\nOld stale content here.\n",
        )
        .unwrap();

        let added = ensure_claude_md_section(&path, false).unwrap();
        assert!(!added, "should skip without force");

        let added = ensure_claude_md_section(&path, true).unwrap();
        assert!(added, "should replace with force");

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(!content.contains("Old stale content"));
        assert!(content.contains("handoff-mcp"));
        assert!(content.starts_with("# Project"));
    }

    #[test]
    fn force_replace_preserves_sections_after() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CLAUDE.md");
        std::fs::write(
            &path,
            "# Project\n\n## Session Handoff\n\nOld content.\n\n## Other Section\n\nKeep this.\n",
        )
        .unwrap();

        let added = ensure_claude_md_section(&path, true).unwrap();
        assert!(added);

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("## Other Section"));
        assert!(content.contains("Keep this."));
        assert!(content.contains("handoff-mcp"));
        assert!(!content.contains("Old content."));
    }

    #[test]
    fn replace_claude_md_section_strips_only_handoff() {
        let text = "# Top\n\n## Session Handoff\n\nOld stuff.\n\n## Build\n\ncargo test\n";
        let result = replace_claude_md_section(text);
        assert!(!result.contains("Old stuff."));
        assert!(!result.contains("Session Handoff"));
        assert!(result.contains("# Top"));
        assert!(result.contains("## Build"));
        assert!(result.contains("cargo test"));
    }

    #[test]
    fn replace_claude_md_section_handles_eof() {
        let text = "# Top\n\n## Session Handoff\n\nOnly section.\n";
        let result = replace_claude_md_section(text);
        assert!(!result.contains("Only section."));
        assert!(result.contains("# Top"));
    }
}
