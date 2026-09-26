use std::path::Path;
use std::process::Command;

use anyhow::Result;

#[derive(Debug, Clone)]
pub struct GitState {
    pub branch: String,
    pub commit: String,
    pub dirty_files: Vec<String>,
}

pub fn capture_git_state(project_dir: &Path) -> Result<GitState> {
    let branch = run_git(project_dir, &["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap_or_else(|_| "unknown".to_string());

    let commit = run_git(project_dir, &["rev-parse", "--short", "HEAD"])
        .unwrap_or_else(|_| "unknown".to_string());

    let dirty_output = run_git(project_dir, &["status", "--porcelain"]).unwrap_or_default();

    let dirty_files: Vec<String> = dirty_output
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    Ok(GitState {
        branch,
        commit,
        dirty_files,
    })
}

/// `git rev-parse --short HEAD` in `project_dir`, or an empty string on any
/// failure (not in a git repo, `git` missing, detached weirdness, etc.) —
/// per wiki/220-vmodel-integration-design.md §2.6 ("`commit` 省略時は `git
/// rev-parse --short HEAD`（失敗時は空）"), used by `handoff_trace_record`
/// when the caller does not supply an explicit `commit`. Deliberately
/// distinct from [`capture_git_state`]'s `"unknown"` fallback (a different,
/// older call site with its own established contract) — this one's spec
/// explicitly calls for empty, not a placeholder string.
pub fn short_head_or_empty(project_dir: &Path) -> String {
    run_git(project_dir, &["rev-parse", "--short", "HEAD"]).unwrap_or_default()
}

fn run_git(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git").args(args).current_dir(dir).output()?;

    if !output.status.success() {
        anyhow::bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
