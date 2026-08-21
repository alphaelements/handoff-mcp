//! `resolve_handoff_dir` / `detect_worktree` behavior across plain
//! directories, regular git repos, and git worktree checkouts.
//!
//! Multi-worktree agent sessions (spec §3.1.1/§3.1.2) share one `.handoff/`
//! rooted at the primary worktree; a secondary worktree has no `.handoff/`
//! of its own until t240.2 symlinks one in. This module pins the *detection*
//! half of that contract: given a project dir, find where `.handoff/`
//! actually lives (or should be created), without ever touching the
//! filesystem beyond reading/removing a stale symlink.

use std::path::Path;
use std::process::Command;

use handoff_mcp::storage::{detect_worktree, ensure_handoff_exists, resolve_handoff_dir};

fn run(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .status()
        .expect("failed to run git");
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

fn init_repo(dir: &Path) {
    run(dir, &["init", "-b", "main"]);
    run(dir, &["config", "user.email", "test@example.com"]);
    run(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("README.md"), "hello").unwrap();
    run(dir, &["add", "."]);
    run(dir, &["commit", "-m", "init"]);
}

/// Step 3 fallback: a plain (non-git) directory with no `.handoff/` yet
/// resolves to `project_dir/.handoff`, matching pre-init behavior.
#[test]
fn resolve_handoff_dir_non_git_directory_falls_back_to_project_join() {
    let dir = tempfile::tempdir().unwrap();

    let resolved = resolve_handoff_dir(dir.path()).unwrap();

    assert_eq!(resolved, dir.path().join(".handoff"));
}

/// Step 1: an existing `.handoff/` directory is always used as-is, git or
/// not.
#[test]
fn resolve_handoff_dir_uses_existing_handoff_dir() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".handoff")).unwrap();

    let resolved = resolve_handoff_dir(dir.path()).unwrap();

    assert_eq!(resolved, dir.path().join(".handoff"));
}

/// A regular (non-worktree) git repo with no `.handoff/` yet behaves like
/// Step 3: no primary worktree to redirect to, so it falls back to the
/// project-local path pending `handoff_init`.
#[test]
fn resolve_handoff_dir_regular_git_repo_falls_back_when_uninitialized() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());

    let resolved = resolve_handoff_dir(dir.path()).unwrap();

    assert_eq!(resolved, dir.path().join(".handoff"));
}

/// `detect_worktree` must not report a plain git repo as a worktree
/// checkout — `git rev-parse --git-common-dir` returns `.git` (relative) for
/// a non-worktree repo, which is the signal we use to distinguish the two.
#[test]
fn detect_worktree_returns_none_for_non_worktree_repo() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());

    assert!(detect_worktree(dir.path()).is_none());
}

/// `detect_worktree` returns `None` outside any git repository.
#[test]
fn detect_worktree_returns_none_for_non_git_directory() {
    let dir = tempfile::tempdir().unwrap();

    assert!(detect_worktree(dir.path()).is_none());
}

/// Step 2 happy path: a secondary worktree with no local `.handoff/` and a
/// primary worktree that does have one resolves to the primary's path.
#[test]
fn resolve_handoff_dir_worktree_finds_primary_handoff_dir() {
    let primary = tempfile::tempdir().unwrap();
    init_repo(primary.path());
    std::fs::create_dir_all(primary.path().join(".handoff")).unwrap();

    let wt_parent = tempfile::tempdir().unwrap();
    let wt_path = wt_parent.path().join("secondary-wt");
    run(
        primary.path(),
        &[
            "worktree",
            "add",
            "-b",
            "secondary",
            wt_path.to_str().unwrap(),
        ],
    );

    // Sanity: the secondary worktree has no `.handoff/` of its own yet.
    assert!(!wt_path.join(".handoff").exists());

    let info = detect_worktree(&wt_path).expect("secondary worktree should be detected");
    assert!(info.is_worktree);
    assert_eq!(
        std::fs::canonicalize(&info.primary_dir).unwrap(),
        std::fs::canonicalize(primary.path()).unwrap()
    );

    let resolved = resolve_handoff_dir(&wt_path).unwrap();
    assert_eq!(
        std::fs::canonicalize(&resolved).unwrap(),
        std::fs::canonicalize(primary.path().join(".handoff")).unwrap()
    );
}

/// Step 2 error path: a secondary worktree with no local `.handoff/`, whose
/// primary worktree also has none, must error rather than silently create a
/// project-local `.handoff/` inside the secondary worktree (that would
/// defeat the whole point of sharing state).
#[test]
fn resolve_handoff_dir_worktree_without_primary_handoff_dir_errors() {
    let primary = tempfile::tempdir().unwrap();
    init_repo(primary.path());
    // Deliberately no `.handoff/` in the primary.

    let wt_parent = tempfile::tempdir().unwrap();
    let wt_path = wt_parent.path().join("secondary-wt");
    run(
        primary.path(),
        &[
            "worktree",
            "add",
            "-b",
            "secondary",
            wt_path.to_str().unwrap(),
        ],
    );

    let result = resolve_handoff_dir(&wt_path);
    assert!(
        result.is_err(),
        "expected an error requesting handoff_init on the primary worktree, got {result:?}"
    );
}

/// Step 1.5: a broken `.handoff` symlink (target removed) must be detected,
/// removed, and treated as absent — falling through to Step 2/3 instead of
/// `resolve_handoff_dir` returning a dangling path.
#[cfg(unix)]
#[test]
fn resolve_handoff_dir_removes_broken_symlink_and_falls_back() {
    let dir = tempfile::tempdir().unwrap();
    let missing_target = dir.path().join("does-not-exist");
    let link = dir.path().join(".handoff");
    std::os::unix::fs::symlink(&missing_target, &link).unwrap();

    assert!(
        link.symlink_metadata().is_ok(),
        "symlink itself should exist prior to resolution"
    );

    let resolved = resolve_handoff_dir(dir.path()).unwrap();

    // Falls back to Step 3 (non-git, no primary worktree available).
    assert_eq!(resolved, dir.path().join(".handoff"));
    assert!(
        link.symlink_metadata().is_err(),
        "broken symlink should have been removed"
    );
}

/// `ensure_handoff_exists` must route through `resolve_handoff_dir` so a
/// secondary worktree sees the primary's initialized `.handoff/` rather than
/// reporting "not initialized".
#[test]
fn ensure_handoff_exists_resolves_through_worktree() {
    let primary = tempfile::tempdir().unwrap();
    init_repo(primary.path());
    std::fs::create_dir_all(primary.path().join(".handoff")).unwrap();

    let wt_parent = tempfile::tempdir().unwrap();
    let wt_path = wt_parent.path().join("secondary-wt");
    run(
        primary.path(),
        &[
            "worktree",
            "add",
            "-b",
            "secondary",
            wt_path.to_str().unwrap(),
        ],
    );

    let resolved = ensure_handoff_exists(&wt_path).unwrap();
    assert_eq!(
        std::fs::canonicalize(&resolved).unwrap(),
        std::fs::canonicalize(primary.path().join(".handoff")).unwrap()
    );
}
