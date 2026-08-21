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

/// A git submodule's `.git` is also a *file* (`gitdir: ../../.git/modules/…`)
/// pointing outside the submodule directory — the same shape
/// `--git-common-dir` reports for a linked worktree. Without a submodule-
/// specific guard, `detect_worktree` would misidentify the submodule
/// checkout as a worktree and try to redirect `.handoff/` resolution to a
/// "primary" that is actually just the superproject.
#[test]
fn detect_worktree_returns_none_for_submodule() {
    let parent = tempfile::tempdir().unwrap();
    init_repo(parent.path());

    // A second repo to be added as the submodule's own upstream.
    let sub_upstream = tempfile::tempdir().unwrap();
    init_repo(sub_upstream.path());

    run(
        parent.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            sub_upstream.path().to_str().unwrap(),
            "sub",
        ],
    );

    let submodule_path = parent.path().join("sub");
    assert!(
        submodule_path.join(".git").is_file(),
        "submodule's .git should be a file, not a directory"
    );

    assert!(
        detect_worktree(&submodule_path).is_none(),
        "a submodule checkout must not be misdetected as a linked worktree"
    );
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

/// Step 2b (spec §3.1.2): resolving `.handoff/` from a secondary worktree
/// must leave behind a real symlink at `<secondary>/.handoff` pointing at
/// the primary's `.handoff/`, so a plain `ls -la` and any tool that only
/// knows `project_dir/.handoff` (not `resolve_handoff_dir`) still finds it.
#[cfg(unix)]
#[test]
fn resolve_handoff_dir_creates_symlink_in_secondary_worktree() {
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

    assert!(!wt_path.join(".handoff").exists());

    let resolved = resolve_handoff_dir(&wt_path).unwrap();
    assert_eq!(
        std::fs::canonicalize(&resolved).unwrap(),
        std::fs::canonicalize(primary.path().join(".handoff")).unwrap()
    );

    let link = wt_path.join(".handoff");
    let meta = link.symlink_metadata().expect("symlink should exist now");
    assert!(
        meta.file_type().is_symlink(),
        "expected a symlink at {}",
        link.display()
    );
    assert_eq!(
        std::fs::canonicalize(&link).unwrap(),
        std::fs::canonicalize(primary.path().join(".handoff")).unwrap()
    );
}

/// A second call to `resolve_handoff_dir` after the symlink already exists
/// must take the Step 1 "live symlink" path and return the same target
/// without erroring (idempotent).
#[cfg(unix)]
#[test]
fn resolve_handoff_dir_is_idempotent_once_symlink_exists() {
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

    resolve_handoff_dir(&wt_path).unwrap();
    let second = resolve_handoff_dir(&wt_path).unwrap();

    assert_eq!(
        std::fs::canonicalize(&second).unwrap(),
        std::fs::canonicalize(primary.path().join(".handoff")).unwrap()
    );
}

/// If the symlink at `<secondary>/.handoff` is broken (e.g. the primary
/// worktree was recreated at a different inode) `resolve_handoff_dir` must
/// remove it and re-create a fresh, working symlink rather than leaving the
/// stale one or erroring.
#[cfg(unix)]
#[test]
fn resolve_handoff_dir_recreates_broken_symlink() {
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

    // Pre-create a broken symlink pointing at a nonexistent path, simulating
    // a stale link left over from a moved/removed primary.
    let link = wt_path.join(".handoff");
    std::os::unix::fs::symlink(wt_path.join("does-not-exist"), &link).unwrap();
    assert!(link.symlink_metadata().is_ok());
    assert!(!link.exists(), "sanity: link should be broken");

    let resolved = resolve_handoff_dir(&wt_path).unwrap();
    assert_eq!(
        std::fs::canonicalize(&resolved).unwrap(),
        std::fs::canonicalize(primary.path().join(".handoff")).unwrap()
    );

    let meta = link
        .symlink_metadata()
        .expect("a symlink should exist again");
    assert!(meta.file_type().is_symlink());
    assert!(
        link.exists(),
        "recreated symlink should resolve to a live target"
    );
}

/// Split-brain guard (spec §3.1.4): if the primary's `.handoff/config.toml`
/// names a *different* project than the one `resolve_handoff_dir` is being
/// asked to resolve for, this is a warning, not a hard error — resolution
/// still succeeds and returns the primary's path.
#[cfg(unix)]
#[test]
fn resolve_handoff_dir_detects_split_brain_by_project_name() {
    let primary = tempfile::tempdir().unwrap();
    init_repo(primary.path());
    let primary_handoff = primary.path().join(".handoff");
    std::fs::create_dir_all(&primary_handoff).unwrap();
    let config = handoff_mcp::storage::config::Config::new("primary-project", "");
    handoff_mcp::storage::config::write_config(&primary_handoff.join("config.toml"), &config)
        .unwrap();

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

    // Resolution should still succeed (this is a warning, not a hard stop)
    // and the symlink should still be created.
    let resolved = resolve_handoff_dir(&wt_path).unwrap();
    assert_eq!(
        std::fs::canonicalize(&resolved).unwrap(),
        std::fs::canonicalize(&primary_handoff).unwrap()
    );
    assert!(wt_path.join(".handoff").symlink_metadata().is_ok());
}

/// `auto_link = false` in the *primary's* config.toml disables automatic
/// symlink creation: `resolve_handoff_dir` still resolves to the primary's
/// `.handoff/` for reads/writes, but must not leave a symlink behind at
/// `<secondary>/.handoff`.
#[cfg(unix)]
#[test]
fn resolve_handoff_dir_skips_symlink_when_auto_link_disabled() {
    let primary = tempfile::tempdir().unwrap();
    init_repo(primary.path());
    let primary_handoff = primary.path().join(".handoff");
    std::fs::create_dir_all(&primary_handoff).unwrap();
    let mut config = handoff_mcp::storage::config::Config::new("no-autolink-project", "");
    config.worktree.auto_link = false;
    handoff_mcp::storage::config::write_config(&primary_handoff.join("config.toml"), &config)
        .unwrap();

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

    let resolved = resolve_handoff_dir(&wt_path).unwrap();
    assert_eq!(
        std::fs::canonicalize(&resolved).unwrap(),
        std::fs::canonicalize(&primary_handoff).unwrap()
    );
    assert!(
        wt_path.join(".handoff").symlink_metadata().is_err(),
        "no symlink should be created when auto_link = false"
    );
}

/// Step 3 (spec §3.1.3): the primary's `[worktree] handoff_root` overrides
/// where the shared `.handoff/` actually lives, redirecting resolution to
/// an explicit path instead of `<primary>/.handoff` itself.
#[test]
fn resolve_handoff_dir_honors_handoff_root_override() {
    let primary = tempfile::tempdir().unwrap();
    init_repo(primary.path());
    let primary_handoff = primary.path().join(".handoff");
    std::fs::create_dir_all(&primary_handoff).unwrap();

    // The actual shared root lives elsewhere; `handoff_root` points at it.
    let shared_root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(shared_root.path().join("tasks")).unwrap();

    let mut config = handoff_mcp::storage::config::Config::new("override-project", "");
    config.worktree.handoff_root = Some(shared_root.path().to_string_lossy().to_string());
    handoff_mcp::storage::config::write_config(&primary_handoff.join("config.toml"), &config)
        .unwrap();

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

    let resolved = resolve_handoff_dir(&wt_path).unwrap();
    assert_eq!(
        std::fs::canonicalize(&resolved).unwrap(),
        std::fs::canonicalize(shared_root.path()).unwrap(),
        "should redirect to the handoff_root override, not <primary>/.handoff"
    );
}

/// Runtime target loss: if the path `resolve_handoff_dir` previously
/// resolved to becomes inaccessible (primary worktree deleted), a caller
/// using `ensure_handoff_exists` must get a clear, actionable error instead
/// of a generic "not found" or an implicit local re-init.
#[test]
fn ensure_handoff_exists_errors_clearly_when_target_vanishes() {
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

    // Prime resolution once (creates the symlink where supported).
    let _ = resolve_handoff_dir(&wt_path);

    // Now delete the primary's .handoff entirely, simulating the primary
    // worktree being moved/removed.
    std::fs::remove_dir_all(primary.path().join(".handoff")).unwrap();

    let result = ensure_handoff_exists(&wt_path);
    assert!(result.is_err(), "expected an error, got {result:?}");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("no longer accessible") || msg.contains("Run handoff_init"),
        "error should clearly explain the target vanished: {msg}"
    );
}
