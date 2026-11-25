//! v0.47.3 WS6 (FR-V0473-016): reading memories must never change a tracked
//! file. Drives the real `handoff-mcp` binary (CLI mode, real filesystem, real
//! `git`) against a project whose `.handoff/` is tracked by the project's own
//! Git repository.
//!
//! The `.handoff/.gitignore` is never written by the test: `handoff-mcp init`
//! (and the sidecar-writing paths) generate it, and the assertions below run
//! against exactly that. A hand-written ignore file would hide a missing entry.
//!
//! Known exceptions to "reads never write" (documented in the v0.47.3 plan,
//! section 4.6 (2) item 8, and in the CHANGELOG), pinned here so they cannot
//! widen unnoticed:
//!
//! - On a project whose `.handoff/.gitignore` predates the recommended list,
//!   the first sidecar write of a read tool appends the missing ignore line
//!   once (plan section 4.6 (2) item 8; pinned by `handoff_gitignore_e2e`).
//!   A freshly initialised project has every line already, so the first pass
//!   below must be clean too.
//! - `load_context` registers the agent and moves an `open` session to `active`.
//! - `list_tasks` / `dashboard` reclaim an expired task lease (lazy scan,
//!   wiki/190 allowlist): the task file is rewritten and a `task.expired`
//!   event is appended. See
//!   `expired_lease_is_reclaimed_by_list_tasks_but_not_by_memory_or_doc_reads`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_handoff-mcp");

/// Run `handoff-mcp <args...>` with `dir` as the working directory (the CLI
/// resolves the project from it), asserting success.
fn run(dir: &Path, args: &[&str]) -> String {
    let output = run_raw(dir, args);
    assert!(
        output.status.success(),
        "handoff-mcp {args:?} failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn run_raw(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .output()
        .expect("spawn handoff-mcp")
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn commit_all(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    git(
        dir,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-q",
            "-m",
            message,
        ],
    );
}

/// [`commit_all`] that also succeeds when there is nothing to commit.
fn commit_all_allow_empty(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    git(
        dir,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            message,
        ],
    );
}

/// A project repo that tracks `.handoff/`, with two memories, a document and a
/// task, everything committed.
fn tracked_project() -> (TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q"]);

    run(&dir, &["init", "--project-name", "memory-read-e2e"]);
    // Lower the BM25 floor so a 2-memory corpus still produces matches.
    let cfg = dir.join(".handoff/config.toml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(
        &cfg,
        text.replace(
            "memory_query_min_score = 2.0",
            "memory_query_min_score = 0.0",
        ),
    )
    .unwrap();

    run(
        &dir,
        &[
            "memory",
            "save",
            "--text",
            "always use atomic_write for handoff files",
            "--keywords",
            "atomic_write",
        ],
    );
    run(
        &dir,
        &[
            "memory",
            "save",
            "--text",
            "the gantt chart export needs a work calendar",
            "--keywords",
            "gantt",
        ],
    );
    run(
        &dir,
        &[
            "doc",
            "save",
            "--slug",
            "atomic-notes",
            "--title",
            "Atomic write notes",
            "--body",
            "# Atomic write notes\n\nAlways use atomic_write for handoff files.\n",
        ],
    );
    run(&dir, &["task", "update", "--title", "a task"]);
    (tmp, dir)
}

/// Every file under `dir` (excluding `.git`) -> its bytes.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(&path).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// The read-only surface that must leave tracked files alone.
fn exercise_read_tools(dir: &Path, session_a: &str, session_b: &str) {
    for session in [session_a, session_b] {
        // Repeated: the first call injects (writes the sidecar), later ones are
        // suppressed by it.
        for _ in 0..3 {
            run(
                dir,
                &[
                    "memory",
                    "query",
                    "--text",
                    "atomic_write handoff files gantt calendar",
                    "--session-id",
                    session,
                ],
            );
        }
    }
    // No session id at all.
    run(
        dir,
        &["memory", "query", "--text", "atomic_write handoff files"],
    );
    // SessionStart hook shape (no arguments), then the review-list variant.
    run(dir, &["memory", "cleanup"]);
    run(dir, &["memory", "cleanup", "--stale-days", "0"]);
    // doc_query with and without a session.
    for session in [Some(session_a), Some(session_b), None] {
        let mut args = vec!["doc", "query", "--text", "atomic write handoff files"];
        if let Some(session) = session {
            args.extend(["--session-id", session]);
        }
        run(dir, &args);
    }
}

#[test]
fn memory_query_cleanup_and_doc_query_leave_tracked_files_untouched() {
    let (_tmp, dir) = tracked_project();
    commit_all(&dir, "fixture");

    // First pass. `handoff-mcp init` already wrote the full recommended
    // `.handoff/.gitignore`, so even the very first read in a freshly
    // initialised project has no one-time setup to do: nothing tracked may
    // change. (The first-time append on a legacy `.gitignore` is the one
    // documented exception; `handoff_gitignore_e2e` pins it separately.)
    exercise_read_tools(&dir, "warm-a", "warm-b");
    assert_eq!(
        git(&dir, &["status", "--porcelain"]),
        "",
        "the first reads in a freshly initialised project must not change a tracked file"
    );
    commit_all_allow_empty(&dir, "baseline");
    assert_eq!(
        git(&dir, &["status", "--porcelain"]),
        "",
        "baseline must be clean"
    );
    let before = snapshot(&dir.join(".handoff/memory"))
        .into_iter()
        .filter(|(path, _)| !path.starts_with("injected"))
        .collect::<BTreeMap<_, _>>();
    assert!(
        before.len() >= 2,
        "both memories must be present: {:?}",
        before.keys()
    );
    let tracked_before = git(&dir, &["ls-files", "-s"]);

    // Measured pass with brand-new session ids, so every sidecar this pass
    // writes is a new file: if the implementation's .gitignore did not cover
    // it, it would show up as untracked.
    exercise_read_tools(&dir, "fresh-a", "fresh-b");

    assert_eq!(
        git(&dir, &["status", "--porcelain"]),
        "",
        "reads must not change or add any tracked-looking file"
    );
    assert_eq!(
        git(&dir, &["ls-files", "-s"]),
        tracked_before,
        "index unchanged"
    );
    let after = snapshot(&dir.join(".handoff/memory"))
        .into_iter()
        .filter(|(path, _)| !path.starts_with("injected"))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(after, before, "memory files byte-identical");

    // The measured pass really did exercise the sidecar write path (otherwise
    // the porcelain check above would prove nothing).
    let sidecars = std::fs::read_dir(dir.join(".handoff/memory/injected"))
        .unwrap()
        .count();
    assert!(
        sidecars >= 4,
        "warm and fresh sessions each leave a sidecar, found {sidecars}"
    );
}

#[test]
fn memory_files_hold_no_usage_stats_after_queries() {
    let (_tmp, dir) = tracked_project();
    exercise_read_tools(&dir, "s-a", "s-b");
    for entry in std::fs::read_dir(dir.join(".handoff/memory"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
    {
        let text = std::fs::read_to_string(entry.path()).unwrap();
        assert!(
            !text.contains("hit_count") && !text.contains("last_referenced_at"),
            "{} carries usage stats:\n{text}",
            entry.path().display()
        );
    }
}

/// Seed an expired lease on task `t1` (the claim path refuses to create one in
/// the past, so write it straight onto the task file).
fn expire_lease_on_t1(dir: &Path) {
    let tasks_dir = dir.join(".handoff/tasks");
    let task_dir = handoff_mcp::storage::tasks::find_task_dir_by_id(&tasks_dir, "t1")
        .unwrap()
        .expect("task t1 exists");
    let (mut data, _status) = handoff_mcp::storage::tasks::read_task(&task_dir)
        .unwrap()
        .unwrap();
    data.lock = Some(handoff_mcp::storage::tasks::TaskLock {
        agent_id: "agent-old".to_string(),
        session_id: "session-old".to_string(),
        claimed_at: "2020-01-01T00:00:00+00:00".to_string(),
        lease_expires_at: "2020-01-01T00:30:00+00:00".to_string(),
        lease_ttl_seconds: 1800,
    });
    handoff_mcp::storage::tasks::write_task(&task_dir, "in_progress", &data).unwrap();
}

/// Documents the known exception: `task list` lazily reclaims an expired lease
/// (rewrites the task file, appends `task.expired`), while the memory and doc
/// read tools leave the same fixture alone.
#[test]
fn expired_lease_is_reclaimed_by_list_tasks_but_not_by_memory_or_doc_reads() {
    let (_tmp, dir) = tracked_project();
    expire_lease_on_t1(&dir);
    exercise_read_tools(&dir, "warm-a", "warm-b");
    commit_all(&dir, "baseline with an expired lease");
    assert_eq!(git(&dir, &["status", "--porcelain"]), "");

    // Memory / doc reads: the expired lock is not touched.
    exercise_read_tools(&dir, "fresh-a", "fresh-b");
    assert_eq!(
        git(&dir, &["status", "--porcelain"]),
        "",
        "memory_* and doc_query must not reclaim expired leases"
    );

    // The documented exception: listing tasks reclaims it.
    run(&dir, &["task", "list"]);
    let status = git(&dir, &["status", "--porcelain"]);
    assert!(
        !status.is_empty(),
        "task list is the documented exception and reclaims the expired lease"
    );
    let events = std::fs::read_to_string(dir.join(".handoff/events.jsonl")).unwrap();
    assert!(
        events.contains("task.expired"),
        "the reclaim is logged as task.expired: {events}"
    );
}
