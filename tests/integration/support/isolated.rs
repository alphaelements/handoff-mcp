//! Process isolation for tests that rewrite process-global state.
//!
//! All integration tests share one test binary (and therefore one process), so
//! a test that permanently changes an environment variable (`PATH`,
//! `HANDOFF_TYPST_BIN`, `TYPST_FONT_PATHS`, ...) would leak that change into
//! every other test running at the same time. [`run`] re-executes the current
//! test binary for exactly one test, so the mutation stays in a throwaway
//! child process; in the child it just runs the body.

use std::process::Command;

/// Set in the child process so the re-executed test runs its body.
const CHILD_ENV: &str = "HANDOFF_TEST_ISOLATED_CHILD";

/// Runs `body` in a dedicated child process of this test binary.
///
/// `module_path` must be `module_path!()` of the calling test file and
/// `test_name` the name of the calling `#[test]` function; together they form
/// the exact libtest name that the child is told to run. The parent fails if
/// the child fails, or if the child did not actually run that one test (a stale
/// `test_name` would otherwise select zero tests and "pass").
pub fn run(module_path: &str, test_name: &str, body: impl FnOnce()) {
    if std::env::var_os(CHILD_ENV).is_some() {
        body();
        return;
    }
    // `module_path!()` is "<crate>::<module>[::...]"; libtest names omit the crate.
    let full_name = match module_path.split_once("::") {
        Some((_, rest)) => format!("{rest}::{test_name}"),
        None => test_name.to_string(),
    };
    let exe = std::env::current_exe().expect("path of the running test binary");
    let output = Command::new(exe)
        .args([
            full_name.as_str(),
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, "1")
        .output()
        .expect("spawn isolated test child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Forward the child's diagnostics (e.g. "SKIPPED: ...") so they stay
    // visible to anything that scans this process's output.
    print!("{stdout}");
    eprint!("{stderr}");
    assert!(
        output.status.success(),
        "isolated test `{full_name}` failed\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(
        stdout.contains(&format!("test {full_name} ... ok")),
        "isolated child did not run `{full_name}` (stale test_name?)\n--- stdout ---\n{stdout}"
    );
}
