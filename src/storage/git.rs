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

/// `git tag --points-at HEAD` in `project_dir` (wiki/270-vmodel-m3-design.md
/// §2.4, M3-06): every tag name whose ref currently points at `HEAD`, in
/// whatever order `git` itself returns them (no further sorting imposed) —
/// empty when `HEAD` has no tag, not in a git repo, or `git` is unavailable
/// (mirrors [`short_head_or_empty`]'s "failure reads as absent" contract,
/// not [`capture_git_state`]'s `"unknown"` placeholder — `trace_baseline
/// create`'s `tag?` auto-resolution treats "no tag" and "can't tell" the
/// same way: fall back to `null`).
pub fn resolve_tags_at_head(project_dir: &Path) -> Vec<String> {
    let output = run_git(project_dir, &["tag", "--points-at", "HEAD"]).unwrap_or_default();
    output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod resolve_tags_at_head_tests {
    use super::*;

    fn init_repo(dir: &Path) {
        run_git(dir, &["init", "-q", "-b", "main"]).unwrap();
        run_git(dir, &["config", "user.email", "test@example.com"]).unwrap();
        run_git(dir, &["config", "user.name", "Test"]).unwrap();
        std::fs::write(dir.join("a.txt"), "hello").unwrap();
        run_git(dir, &["add", "a.txt"]).unwrap();
        run_git(dir, &["commit", "-q", "-m", "initial"]).unwrap();
    }

    #[test]
    fn empty_when_head_has_no_tag() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        assert!(resolve_tags_at_head(tmp.path()).is_empty());
    }

    #[test]
    fn returns_the_single_tag_pointing_at_head() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        run_git(tmp.path(), &["tag", "v1.0.0"]).unwrap();
        assert_eq!(resolve_tags_at_head(tmp.path()), vec!["v1.0.0".to_string()]);
    }

    #[test]
    fn returns_every_tag_when_head_has_multiple() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        run_git(tmp.path(), &["tag", "v1.0.0"]).unwrap();
        run_git(tmp.path(), &["tag", "release-a"]).unwrap();
        let mut tags = resolve_tags_at_head(tmp.path());
        tags.sort();
        assert_eq!(tags, vec!["release-a".to_string(), "v1.0.0".to_string()]);
    }

    #[test]
    fn empty_when_not_a_git_repository() {
        let tmp = tempfile::tempdir().unwrap();
        // No `git init` at all — not a repo.
        assert!(resolve_tags_at_head(tmp.path()).is_empty());
    }
}
