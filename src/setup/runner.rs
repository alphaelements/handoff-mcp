//! Runs `[commands]` for `handoff-mcp setup --check --run` and
//! `handoff-mcp release-check --run` (SPEC-ONB-002, SPEC-ONB-040,
//! SPEC-ONB-093).
//!
//! This is the only place in handoff-mcp that executes a command taken from
//! `config.toml`, and it is reachable only from the CLI a user types. The MCP
//! server never calls into this module: `handoff_doctor` and
//! `handoff_release_check` return the commands and judge the results an agent
//! reports (see `doctor`).
//!
//! A command is a shell command line, so it runs through the platform's
//! shell (`sh -c` on Unix, `cmd /C` on Windows) in the project directory.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::doctor::{CommandResult, CommandToRun, OUTPUT_TAIL_MAX_LINES};

/// How often a running command is checked for completion.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// How much of the end of a command's output is read back. Twenty lines fit
/// many times over; the bound keeps a chatty command from being loaded whole.
const OUTPUT_TAIL_MAX_BYTES: u64 = 64 * 1024;

/// Longest output line kept, in characters.
const OUTPUT_LINE_MAX_CHARS: usize = 500;

/// What happened when a command was run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Execution {
    /// The exit code, when the command ran to completion.
    pub exit_code: Option<i64>,
    pub duration_ms: u64,
    /// The last lines of the command's combined output.
    pub output_tail: String,
    /// Why there is no exit code: the command did not start, timed out, or
    /// was ended by a signal.
    pub error: Option<String>,
}

/// Runs every command in `pending`, in order and whatever the earlier ones
/// did, and returns their results in the form the checks judge. Each command
/// is announced on stderr, so that JSON printed to stdout stays pure.
pub(crate) fn run_pending(pending: &[CommandToRun], project_dir: &Path) -> Vec<CommandResult> {
    pending
        .iter()
        .map(|pending| {
            eprintln!("  running {}: {}", pending.id, pending.command);
            let execution = run_command(
                &pending.command,
                project_dir,
                Duration::from_secs(pending.timeout_sec),
            );
            CommandResult {
                id: pending.id.to_string(),
                command: pending.command.clone(),
                exit_code: execution.exit_code,
                duration_ms: Some(execution.duration_ms),
                output_tail: Some(execution.output_tail),
                error: execution.error,
            }
        })
        .collect()
}

/// Runs `command` through the shell in `project_dir`, stopping it (and what
/// it started) after `timeout`.
pub(crate) fn run_command(command: &str, project_dir: &Path, timeout: Duration) -> Execution {
    let started = Instant::now();
    let elapsed_ms =
        |started: Instant| u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let start_failure = |reason: String| Execution {
        exit_code: None,
        duration_ms: elapsed_ms(started),
        output_tail: String::new(),
        error: Some(format!("could not start: {reason}")),
    };

    // Output goes to a file rather than a pipe: a process the command leaves
    // behind could hold a pipe open and block the read long past the timeout.
    let capture = match OutputCapture::create() {
        Ok(capture) => capture,
        Err(e) => return start_failure(format!("cannot capture output ({e})")),
    };
    let (stdout, stderr) = match capture.handles() {
        Ok(handles) => handles,
        Err(e) => return start_failure(format!("cannot capture output ({e})")),
    };

    let mut child = match shell_command(command)
        .current_dir(project_dir)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return start_failure(e.to_string()),
    };

    // `None`: a timeout too large to add to the clock is no limit at all.
    let deadline = started.checked_add(timeout);
    let outcome = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if deadline.is_some_and(|deadline| Instant::now() >= deadline) => {
                stop_process_tree(&mut child);
                break Err(format!("timed out after {}", describe(timeout)));
            }
            Ok(None) => std::thread::sleep(POLL_INTERVAL),
            Err(e) => {
                stop_process_tree(&mut child);
                break Err(format!("could not wait for the command ({e})"));
            }
        }
    };

    let duration_ms = elapsed_ms(started);
    let output_tail = capture.tail();
    match outcome {
        Ok(status) => match status.code() {
            Some(code) => Execution {
                exit_code: Some(i64::from(code)),
                duration_ms,
                output_tail,
                error: None,
            },
            None => Execution {
                exit_code: None,
                duration_ms,
                output_tail,
                error: Some(ended_without_exit_code(status)),
            },
        },
        Err(error) => Execution {
            exit_code: None,
            duration_ms,
            output_tail,
            error: Some(error),
        },
    }
}

fn describe(timeout: Duration) -> String {
    if timeout.subsec_nanos() == 0 {
        format!("{}s", timeout.as_secs())
    } else {
        format!("{}ms", timeout.as_millis())
    }
}

#[cfg(not(windows))]
fn shell_command(command: &str) -> Command {
    let mut shell = Command::new("sh");
    shell.arg("-c").arg(command);
    shell
}

#[cfg(windows)]
fn shell_command(command: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let mut shell = Command::new("cmd");
    // `raw_arg`: cmd.exe parses its own command line, so the text must reach
    // it as written rather than quoted for a C runtime.
    shell.arg("/C").raw_arg(command);
    shell
}

#[cfg(unix)]
fn ended_without_exit_code(status: std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match status.signal() {
        Some(signal) => format!("ended by signal {signal}"),
        None => "ended without an exit code".to_string(),
    }
}

#[cfg(not(unix))]
fn ended_without_exit_code(_status: std::process::ExitStatus) -> String {
    "ended without an exit code".to_string()
}

/// Stops `child` and the processes it started, then reaps it.
///
/// The command stays in this process's process group on purpose, so Ctrl-C in
/// the terminal reaches it like any other foreground command. The price is
/// that a timeout has to find the descendants itself; a process started in
/// the instant between the listing and the kill can be missed.
fn stop_process_tree(child: &mut Child) {
    kill_descendants(child.id());
    // Covers the shell itself when the platform tool above was unavailable.
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn kill_descendants(root: u32) {
    let Ok(listing) = Command::new("ps")
        .args(["-A", "-o", "pid=,ppid="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return;
    };
    let parents: Vec<(u32, u32)> = String::from_utf8_lossy(&listing.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
        })
        .collect();
    let mut tree = vec![root];
    let mut next = 0;
    while next < tree.len() {
        let parent = tree[next];
        let children: Vec<u32> = parents
            .iter()
            .filter(|(pid, ppid)| *ppid == parent && !tree.contains(pid))
            .map(|(pid, _)| *pid)
            .collect();
        tree.extend(children);
        next += 1;
    }
    let _ = Command::new("kill")
        .arg("-KILL")
        .args(tree.iter().map(u32::to_string))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(windows)]
fn kill_descendants(root: u32) {
    let _ = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &root.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(any(unix, windows)))]
fn kill_descendants(_root: u32) {}

/// A temporary file a command's stdout and stderr are written to. Removed
/// when dropped.
struct OutputCapture {
    path: PathBuf,
    file: std::fs::File,
}

impl OutputCapture {
    fn create() -> std::io::Result<Self> {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.subsec_nanos());
        let path = std::env::temp_dir().join(format!(
            "handoff-mcp-check-{}-{}-{unique}.log",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        // `create_new`: never write through a file (or link) someone else put
        // at this name.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(Self { path, file })
    }

    /// Two handles onto the file sharing one write position, so stdout and
    /// stderr interleave in the order they were written.
    fn handles(&self) -> std::io::Result<(Stdio, Stdio)> {
        Ok((
            Stdio::from(self.file.try_clone()?),
            Stdio::from(self.file.try_clone()?),
        ))
    }

    /// The last [`OUTPUT_TAIL_MAX_LINES`] lines written so far. Output that
    /// cannot be read back is reported as empty: the exit code, not the
    /// output, decides the result.
    fn tail(&self) -> String {
        let mut file = &self.file;
        let Ok(length) = file.seek(SeekFrom::End(0)) else {
            return String::new();
        };
        let from = length.saturating_sub(OUTPUT_TAIL_MAX_BYTES);
        let mut bytes = Vec::new();
        if file.seek(SeekFrom::Start(from)).is_err() || file.read_to_end(&mut bytes).is_err() {
            return String::new();
        }
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<String> = text
            .trim_end()
            .lines()
            .map(|line| {
                line.trim_end()
                    .chars()
                    .take(OUTPUT_LINE_MAX_CHARS)
                    .collect()
            })
            .collect();
        lines[lines.len().saturating_sub(OUTPUT_TAIL_MAX_LINES)..].join("\n")
    }
}

impl Drop for OutputCapture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Instant;

    const LONG: Duration = Duration::from_secs(30);

    fn run(command: &str) -> (tempfile::TempDir, Execution) {
        let dir = tempfile::tempdir().unwrap();
        let execution = run_command(command, dir.path(), LONG);
        (dir, execution)
    }

    #[test]
    fn a_successful_command_reports_exit_code_zero() {
        let (_dir, execution) = run("true");
        assert_eq!(execution.exit_code, Some(0));
        assert_eq!(execution.error, None);
        assert_eq!(execution.output_tail, "");
    }

    #[test]
    fn a_failing_command_reports_its_exit_code() {
        let (_dir, execution) = run("exit 3");
        assert_eq!(execution.exit_code, Some(3));
        assert_eq!(execution.error, None);
    }

    // SPEC-ONB-093 AC3
    #[test]
    fn the_command_line_is_interpreted_by_the_shell() {
        let (dir, execution) = run(
            "printf 'a\\nb\\n' | grep b > out.txt && test -s out.txt && echo \"done $((1+1))\"",
        );
        assert_eq!(execution.exit_code, Some(0), "{execution:?}");
        assert_eq!(execution.output_tail, "done 2");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("out.txt")).unwrap(),
            "b\n"
        );

        let (_dir, execution) = run("true && false || exit 7");
        assert_eq!(execution.exit_code, Some(7));
    }

    #[test]
    fn the_command_runs_in_the_project_directory() {
        let (dir, execution) = run("pwd && touch marker");
        assert_eq!(execution.exit_code, Some(0));
        assert!(dir.path().join("marker").exists());
        assert_eq!(
            std::fs::canonicalize(&execution.output_tail).unwrap(),
            std::fs::canonicalize(dir.path()).unwrap()
        );
    }

    // SPEC-ONB-002 AC8
    #[test]
    fn a_command_that_does_not_exist_fails_with_the_shells_exit_code() {
        let (_dir, execution) = run("handoff-no-such-command-xyz --flag");
        assert_eq!(execution.exit_code, Some(127));
        assert!(
            execution
                .output_tail
                .contains("handoff-no-such-command-xyz"),
            "the shell's message is kept: {:?}",
            execution.output_tail
        );
    }

    #[test]
    fn a_project_directory_that_does_not_exist_is_a_start_failure() {
        let dir = tempfile::tempdir().unwrap();
        let execution = run_command("true", &dir.path().join("gone"), LONG);
        assert_eq!(execution.exit_code, None);
        let error = execution.error.expect("an error");
        assert!(error.starts_with("could not start"), "{error}");
    }

    // SPEC-ONB-002 AC8
    #[test]
    fn a_command_that_runs_too_long_is_stopped_with_what_it_started() {
        let dir = tempfile::tempdir().unwrap();
        let started = Instant::now();
        // The shell starts a child that would outlive it and write a file.
        let execution = run_command(
            "echo before; (sleep 3; touch survived) & sleep 30",
            dir.path(),
            Duration::from_millis(400),
        );
        let elapsed = started.elapsed();

        assert_eq!(execution.exit_code, None, "{execution:?}");
        let error = execution.error.clone().expect("an error");
        assert!(error.contains("timed out"), "{error}");
        assert!(
            elapsed < Duration::from_secs(5),
            "the run must end at the timeout, took {elapsed:?}"
        );
        assert!(execution.duration_ms >= 400, "{execution:?}");
        assert_eq!(execution.output_tail, "before");

        std::thread::sleep(Duration::from_millis(3200));
        assert!(
            !dir.path().join("survived").exists(),
            "a process the command started kept running after the timeout"
        );
    }

    #[test]
    fn output_of_stdout_and_stderr_is_kept_up_to_the_last_twenty_lines() {
        let (_dir, execution) =
            run("i=1; while [ $i -le 30 ]; do echo \"out $i\"; i=$((i+1)); done; echo err >&2");
        let lines: Vec<&str> = execution.output_tail.lines().collect();
        assert_eq!(lines.len(), 20, "{lines:?}");
        assert_eq!(lines[0], "out 12");
        assert_eq!(lines[18], "out 30");
        assert_eq!(lines[19], "err");
    }

    #[test]
    fn a_command_cannot_wait_for_input() {
        let dir = tempfile::tempdir().unwrap();
        let started = Instant::now();
        let execution = run_command("cat; echo eof", dir.path(), LONG);
        assert_eq!(execution.exit_code, Some(0));
        assert_eq!(execution.output_tail, "eof");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_command_ended_by_a_signal_is_an_error_not_an_exit_code() {
        let (_dir, execution) = run("kill -KILL $$");
        assert_eq!(execution.exit_code, None, "{execution:?}");
        assert!(
            execution.error.as_deref().unwrap_or("").contains("signal"),
            "{execution:?}"
        );
    }

    #[test]
    fn output_that_is_not_utf8_does_not_break_the_run() {
        let (_dir, execution) = run("printf 'caf\\351\\n'");
        assert_eq!(execution.exit_code, Some(0));
        assert!(execution.output_tail.starts_with("caf"), "{execution:?}");
    }

    /// `[onboarding] command_timeout_sec` is a number someone typed; an
    /// absurdly large one must mean "no limit", not a crash.
    #[test]
    fn a_timeout_too_large_to_represent_means_no_limit() {
        let dir = tempfile::tempdir().unwrap();
        for timeout in [Duration::MAX, Duration::from_secs(u64::MAX / 2)] {
            let execution = run_command("exit 5", dir.path(), timeout);
            assert_eq!(execution.exit_code, Some(5), "{timeout:?}");
        }
    }

    #[test]
    fn duration_is_measured() {
        let (_dir, execution) = run("sleep 0.3");
        assert!(execution.duration_ms >= 300, "{execution:?}");
        assert!(execution.duration_ms < 5000, "{execution:?}");
    }
}

/// The Windows branch (`cmd /C`) cannot be exercised by the Unix tests above.
#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    const LONG: Duration = Duration::from_secs(30);

    #[test]
    fn the_command_line_reaches_cmd_as_written() {
        let dir = tempfile::tempdir().unwrap();
        let execution = run_command("echo hello&& exit 3", dir.path(), LONG);
        assert_eq!(execution.exit_code, Some(3), "{execution:?}");
        assert_eq!(execution.output_tail, "hello");
        assert_eq!(execution.error, None);
    }

    #[test]
    fn a_successful_command_reports_exit_code_zero() {
        let dir = tempfile::tempdir().unwrap();
        let execution = run_command("exit 0", dir.path(), LONG);
        assert_eq!(execution.exit_code, Some(0), "{execution:?}");
    }
}
