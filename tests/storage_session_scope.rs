//! `detect_session_scope` (t250.1 — SessionScope 自動検出): given a resolved
//! project dir, classify it as `"primary"`, `"worktree"`, or `"ephemeral"`
//! based on the same git-worktree detection `resolve_handoff_dir` already
//! uses, without requiring the caller to pass a scope explicitly (spec
//! §4.1 FR-2.1 — scope is never an accepted parameter, always inferred).

use std::path::Path;
use std::process::Command;

use handoff_mcp::storage::detect_session_scope;

fn run(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
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

/// A plain non-git directory is treated as ephemeral — there is no
/// worktree/primary relationship to speak of, so it must not be silently
/// promoted to "primary".
#[test]
fn detect_session_scope_non_git_directory_is_ephemeral() {
    let dir = tempfile::tempdir().unwrap();

    assert_eq!(detect_session_scope(dir.path()), "ephemeral");
}

/// A regular (non-worktree) git repository — the common case, and also the
/// primary worktree itself before any secondary worktree exists — is
/// "primary".
#[test]
fn detect_session_scope_regular_git_repo_is_primary() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());

    assert_eq!(detect_session_scope(dir.path()), "primary");
}

/// A linked git worktree (secondary checkout) is "worktree".
#[test]
fn detect_session_scope_linked_worktree_is_worktree() {
    let primary = tempfile::tempdir().unwrap();
    init_repo(primary.path());

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

    assert_eq!(detect_session_scope(&wt_path), "worktree");
}

/// The primary worktree itself (the checkout `git worktree add` was run
/// from) must still resolve to "primary", even after a secondary worktree
/// has been created from it.
#[test]
fn detect_session_scope_primary_stays_primary_after_worktree_added() {
    let primary = tempfile::tempdir().unwrap();
    init_repo(primary.path());

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

    assert_eq!(detect_session_scope(primary.path()), "primary");
}
