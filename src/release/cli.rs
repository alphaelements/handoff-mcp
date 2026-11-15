//! `handoff-mcp release-check <milestone>` (SPEC-ONB-040): the pre-release
//! check from a terminal.
//!
//! It prints the same JSON the `handoff_release_check` tool returns. With
//! `--run` it executes the commands the check hands out (through the shell,
//! see `crate::setup::runner`) and judges their results; this, typed by a
//! user, is the only way the release check ever runs a command.
//!
//! Exit code: 0 when the check is `ok`, 1 when it is not, 2 when the request
//! could not be carried out (a usage error, an unknown milestone).

use anyhow::Result;
use serde_json::json;

use super::check::ReleaseReport;
use crate::mcp::handlers::release_check::{run as run_check, ReleaseCheckRequest};
use crate::setup::doctor::ResultSource;
use crate::setup::runner;

/// Exit code of a check in which every item passed.
const EXIT_OK: i32 = 0;
/// Exit code of a check with an item that did not pass.
const EXIT_NOT_OK: i32 = 1;
/// Exit code of a request that could not be carried out.
const EXIT_ERROR: i32 = 2;

/// Usage text of `handoff-mcp release-check`.
pub const USAGE: &str = "\
handoff-mcp release-check — pre-release check of a milestone

USAGE:
    handoff-mcp release-check <milestone> [--run] [--confirm <id>[,<id>...]]
                                          [--project-dir <path>]

    Prints the check as JSON: {milestone, ok, items, commands_to_run}.

OPTIONS:
    --run                   Run the [commands] test / smoke / audit commands and judge
                            their results (without it, those items are `skip`)
    --confirm <ids>         Record manual items as confirmed, e.g. manual:docs_updated
    --project-dir <path>    Project directory (default: current directory)
    -h, --help              Print this help

EXIT CODE:
    0  every item passed
    1  an item failed, was skipped, or waits for a person
    2  the check could not be carried out (usage error, unknown milestone)";

/// A parsed `handoff-mcp release-check ...` command line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Invocation {
    Help,
    Check(Options),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Options {
    milestone: String,
    run_commands: bool,
    confirm: Vec<String>,
    project_dir: Option<String>,
}

/// Parses the arguments that follow `release-check`. An argument this
/// command does not know is an error: a misspelled `--run` must not turn
/// into a check that silently runs nothing.
fn parse(args: &[String]) -> Result<Invocation> {
    let mut options = Options::default();
    let mut milestone: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_string())),
            _ => (arg, None),
        };
        let mut value_of = |name: &str, what: &str| -> Result<String> {
            if let Some(value) = &inline {
                return Ok(value.clone());
            }
            index += 1;
            args.get(index)
                .filter(|value| !value.starts_with("--"))
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("{name} needs {what}"))
        };
        match flag {
            "--help" | "-h" => return Ok(Invocation::Help),
            "--run" if inline.is_none() => options.run_commands = true,
            "--confirm" => {
                let ids = value_of("--confirm", "the id of a manual item (manual:<name>)")?;
                options.confirm.extend(
                    ids.split(',')
                        .map(str::trim)
                        .filter(|id| !id.is_empty())
                        .map(String::from),
                );
            }
            "--project-dir" => {
                options.project_dir = Some(value_of("--project-dir", "a directory")?);
            }
            "--milestone" => {
                let name = value_of("--milestone", "a milestone name")?;
                set_milestone(&mut milestone, name)?;
            }
            other if other.starts_with('-') => anyhow::bail!(
                "unknown argument '{arg}'. Run `handoff-mcp release-check --help` for usage."
            ),
            _ => set_milestone(&mut milestone, arg.to_string())?,
        }
        index += 1;
    }
    options.milestone = milestone.filter(|name| !name.is_empty()).ok_or_else(|| {
        anyhow::anyhow!(
            "name the milestone to check: `handoff-mcp release-check <milestone>`. \
             `handoff-mcp milestone list` shows the defined ones."
        )
    })?;
    Ok(Invocation::Check(options))
}

fn set_milestone(milestone: &mut Option<String>, name: String) -> Result<()> {
    if let Some(first) = milestone {
        anyhow::bail!(
            "unexpected argument '{name}': the milestone is already '{first}'. \
             One milestone is checked per call."
        );
    }
    *milestone = Some(name);
    Ok(())
}

/// Runs `handoff-mcp release-check` with the arguments that follow the
/// command name and returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    match parse(args).and_then(|invocation| match invocation {
        Invocation::Help => {
            println!("{USAGE}");
            Ok(EXIT_OK)
        }
        Invocation::Check(options) => {
            let report = check(&options)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(if report.ok { EXIT_OK } else { EXIT_NOT_OK })
        }
    }) {
        Ok(code) => code,
        Err(e) => {
            let error = json!({ "error": format!("{e:#}") });
            println!(
                "{}",
                serde_json::to_string_pretty(&error).unwrap_or_else(|_| error.to_string())
            );
            EXIT_ERROR
        }
    }
}

fn check(options: &Options) -> Result<ReleaseReport> {
    let project_dir = crate::mcp::handlers::resolve_project_dir(&match &options.project_dir {
        Some(dir) => json!({ "project_dir": dir }),
        None => json!({}),
    })?;
    let request = |results: &[crate::setup::doctor::CommandResult], confirm: &[String]| {
        run_check(&ReleaseCheckRequest {
            project_dir: &project_dir,
            milestone: &options.milestone,
            results,
            source: ResultSource::Executed,
            confirm,
        })
    };
    // The first pass rejects a request that cannot be honored before any
    // command is run, and records the confirmations.
    let first = request(&[], &options.confirm)?;
    if !options.run_commands || first.commands_to_run.is_empty() {
        return Ok(first);
    }
    let results = runner::run_pending(&first.commands_to_run, &project_dir);
    request(&results, &[])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Invocation> {
        parse(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
    }

    fn options(args: &[&str]) -> Options {
        match parse_args(args).unwrap() {
            Invocation::Check(options) => options,
            Invocation::Help => panic!("{args:?} parsed as help"),
        }
    }

    #[test]
    fn the_milestone_is_the_positional_argument() {
        assert_eq!(
            options(&["v1"]),
            Options {
                milestone: "v1".to_string(),
                ..Options::default()
            }
        );
        assert_eq!(options(&["--milestone", "v1"]).milestone, "v1");
        assert_eq!(options(&["--milestone=v1"]).milestone, "v1");
        assert_eq!(options(&["--run", "v0.48.0"]).milestone, "v0.48.0");
    }

    #[test]
    fn flags_are_read_in_any_order() {
        let parsed = options(&[
            "--confirm",
            "manual:a, manual:b",
            "v1",
            "--run",
            "--project-dir=/p",
            "--confirm=manual:c",
        ]);
        assert_eq!(
            parsed,
            Options {
                milestone: "v1".to_string(),
                run_commands: true,
                confirm: vec![
                    "manual:a".to_string(),
                    "manual:b".to_string(),
                    "manual:c".to_string()
                ],
                project_dir: Some("/p".to_string()),
            }
        );
        assert!(!options(&["v1"]).run_commands);
    }

    #[test]
    fn help_wins_over_everything_else() {
        assert_eq!(parse_args(&["--help"]).unwrap(), Invocation::Help);
        assert_eq!(
            parse_args(&["v1", "--run", "-h"]).unwrap(),
            Invocation::Help
        );
    }

    #[test]
    fn requests_that_name_no_single_milestone_are_rejected() {
        for (args, expected) in [
            (vec![], "name the milestone"),
            (vec!["--run"], "name the milestone"),
            (vec![""], "name the milestone"),
            (vec!["v1", "v2"], "unexpected argument 'v2'"),
            (vec!["v1", "--milestone", "v2"], "unexpected argument 'v2'"),
        ] {
            let err = parse_args(&args).unwrap_err().to_string();
            assert!(err.contains(expected), "{args:?}: {err}");
        }
    }

    #[test]
    fn unknown_and_incomplete_flags_are_rejected() {
        for (args, expected) in [
            (vec!["v1", "--rum"], "unknown argument '--rum'"),
            (vec!["v1", "-x"], "unknown argument '-x'"),
            (vec!["v1", "--run=yes"], "unknown argument '--run=yes'"),
            (vec!["v1", "--confirm"], "--confirm needs"),
            (vec!["v1", "--confirm", "--run"], "--confirm needs"),
            (vec!["v1", "--project-dir"], "--project-dir needs"),
            (vec!["--milestone"], "--milestone needs"),
        ] {
            let err = parse_args(&args).unwrap_err().to_string();
            assert!(err.contains(expected), "{args:?}: {err}");
        }
    }
}
