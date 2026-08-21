pub mod agents;
pub mod config;
pub mod docs;
pub mod git;
pub mod memory;
pub mod referrals;
pub mod sessions;
pub mod tasks;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Monotonic counter making temp file names unique per *call*, not just per
/// process — two threads writing the same file concurrently would otherwise
/// pick the same temp path and corrupt each other's payload.
static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many times to retry a rename that failed with a transient sharing
/// violation. Windows denies a replace while another process has the target
/// open without `FILE_SHARE_DELETE`; the VSCode extension polls `.handoff/`, so
/// this is a normal, short-lived collision rather than a real error.
#[cfg(windows)]
const RENAME_RETRIES: u32 = 10;

/// Rename `tmp` over `dest`, replacing it.
///
/// `std::fs::rename` maps to `MoveFileExW`/`SetFileInformationByHandle` on
/// Windows and does replace an existing file, so no separate remove-then-rename
/// dance is needed. What it does *not* guarantee on Windows is success while a
/// reader holds the destination open — that surfaces as `PermissionDenied`
/// (ERROR_ACCESS_DENIED / ERROR_SHARING_VIOLATION). See rust-lang/rust#123985.
#[cfg(windows)]
fn replace_file(tmp: &Path, dest: &Path) -> std::io::Result<()> {
    // 10 attempts, each failure followed by a sleep that doubles up to a 64ms
    // cap: 1+2+4+8+16+32+64*4 = 319ms of backoff in the worst case.
    let mut delay = std::time::Duration::from_millis(1);
    let mut last_err = None;

    for _ in 0..RENAME_RETRIES {
        match std::fs::rename(tmp, dest) {
            Ok(()) => return Ok(()),
            // Retry only transient contention; anything else (a missing
            // directory, a read-only file) will not fix itself.
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                last_err = Some(e);
                std::thread::sleep(delay);
                delay = (delay * 2).min(std::time::Duration::from_millis(64));
            }
            Err(e) => return Err(e),
        }
    }

    // Retries exhausted: surface the last contention error rather than
    // panicking, so the caller reports a normal write failure.
    Err(last_err.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "rename retries exhausted",
        )
    }))
}

/// On POSIX a same-directory rename is atomic and never blocked by open
/// readers, so a single call suffices.
#[cfg(not(windows))]
fn replace_file(tmp: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::rename(tmp, dest)
}

/// Write `content` to `path` atomically: write to a sibling temp file, fsync,
/// then rename over the target. A reader therefore observes either the old or
/// the new contents, never a partially-written file.
///
/// Used by every handoff write path (tasks, config, sessions, referrals) so that
/// the VSCode extension — which writes the same files — never reads torn data.
///
/// Both platforms replace an existing target; on Windows the rename is retried
/// briefly because a concurrent reader can make it fail transiently. See
/// [`replace_file`].
pub fn atomic_write(path: impl AsRef<Path>, content: &[u8]) -> Result<()> {
    use std::io::Write;

    let path = path.as_ref();
    let dir = path.parent().ok_or_else(|| {
        anyhow::anyhow!("Cannot determine parent directory for {}", path.display())
    })?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("Invalid file name for {}", path.display()))?;

    // Unique per process *and* per call, in the same directory so the rename
    // stays within one filesystem.
    let seq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp_name = format!(".{file_name}.tmp.{}.{seq}", std::process::id());
    let tmp_path = dir.join(tmp_name);

    let mut f = std::fs::File::create(&tmp_path)
        .with_context(|| format!("Failed to create temp file {}", tmp_path.display()))?;
    f.write_all(content)
        .with_context(|| format!("Failed to write temp file {}", tmp_path.display()))?;
    f.sync_all()
        .with_context(|| format!("Failed to sync temp file {}", tmp_path.display()))?;
    drop(f);

    replace_file(&tmp_path, path).map_err(|e| {
        // Best-effort cleanup so a failed rename doesn't leave a stray temp file.
        let _ = std::fs::remove_file(&tmp_path);
        anyhow::Error::new(e).context(format!(
            "Failed to rename {} -> {}",
            tmp_path.display(),
            path.display()
        ))
    })?;
    Ok(())
}

/// Resolve the user's home directory.
///
/// `HOME` is the POSIX variable; Windows does not set it and uses `USERPROFILE`
/// instead. Mirrors the same fallback order as [`crate::setup`] so a `~/` path
/// resolves identically whether it came from `config.toml` or from setup.
fn home_dir() -> Option<String> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .filter(|h| !h.is_empty())
}

/// Expand a leading `~/` against the user's home directory.
///
/// Returns `path` unchanged when it has no `~/` prefix or when no home
/// directory can be determined — callers treat a still-tilde'd path as a
/// directory that does not exist and skip it, which is the safe outcome.
///
/// Only a `~/` (or `~\`) prefix is expanded; bare `~` and `~user/…` are left
/// alone. The remainder is assumed relative — `Path::join` discards the home
/// prefix if it is not (`~//etc` yields `/etc`), which is acceptable because
/// this only reads `scan_dirs` the user configured and the result is used
/// solely to test existence and enumerate directories.
pub fn expand_tilde(path: &str) -> String {
    // Accept both separators: `config.toml` is hand-edited and a Windows user
    // may well write `~\pro`.
    let rest = path.strip_prefix("~/").or_else(|| path.strip_prefix(r"~\"));

    if let Some(rest) = rest {
        if let Some(home) = home_dir() {
            // Join via `Path` so the platform separator is used rather than a
            // hardcoded `/`.
            return Path::new(&home).join(rest).to_string_lossy().into_owned();
        }
    }
    path.to_string()
}

/// Describes the git worktree relationship of `project_dir`, when it is one.
///
/// `primary_dir` is the checkout that owns the shared `.git` directory (the
/// worktree `git worktree add` was run *from*, or the original clone).
/// `common_dir` is the raw `--git-common-dir` output resolved to an absolute
/// path — for the primary worktree itself this is `primary_dir/.git`; for a
/// linked worktree it is the same shared `.git`, reached via
/// `<primary>/.git/worktrees/<name>`'s `commondir` file.
#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    pub is_worktree: bool,
    pub primary_dir: PathBuf,
    pub common_dir: PathBuf,
}

/// Detect whether `project_dir` is inside a git worktree checkout, and if
/// so, locate the primary worktree that owns the shared `.git` directory.
///
/// Returns `None` for a non-git directory, and also for a *regular* (non-
/// worktree) git repository — `--git-common-dir` reports a `.git` directory
/// local to the repo itself in that case, so there is no "primary" to
/// redirect to; the caller already has everything it needs at
/// `project_dir`.
pub fn detect_worktree(project_dir: &Path) -> Option<WorktreeInfo> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(project_dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let common_dir_raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if common_dir_raw.is_empty() {
        return None;
    }

    // `--git-common-dir` is relative to `project_dir` unless it is already
    // absolute (older/newer git versions differ here).
    let common_dir = {
        let p = Path::new(&common_dir_raw);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            project_dir.join(p)
        }
    };

    // A regular (non-worktree) repo's common dir is its own `.git`, i.e. a
    // direct child of `project_dir`. A linked worktree's common dir lives
    // under the *primary* checkout instead, so comparing parents tells them
    // apart without needing `--git-dir` as a second data point.
    let common_parent = common_dir.parent()?;
    let project_dir_canon = std::fs::canonicalize(project_dir).ok();
    let common_parent_canon = std::fs::canonicalize(common_parent).ok();

    let is_worktree = match (&project_dir_canon, &common_parent_canon) {
        (Some(a), Some(b)) => a != b,
        // If either side fails to canonicalize, fall back to a direct
        // (non-canonicalized) comparison rather than guessing.
        _ => project_dir != common_parent,
    };

    if !is_worktree {
        return None;
    }

    Some(WorktreeInfo {
        is_worktree: true,
        primary_dir: common_parent.to_path_buf(),
        common_dir,
    })
}

/// Resolve the actual `.handoff/` directory for `project_dir`, following the
/// multi-worktree redirection rules (spec §3.1.1/§3.1.2):
///
/// - Step 1: `project_dir/.handoff` already exists (directory, or a live
///   symlink) → use it as-is.
/// - Step 1.5: `project_dir/.handoff` is a *broken* symlink (stale from a
///   worktree that was removed/moved) → warn, remove it, and fall through.
/// - Step 2: no local `.handoff/` and `project_dir` is a linked git worktree
///   → look for `.handoff/` in the primary worktree. Found → return that
///   path (creating a symlink back is t240.2's job, not this function's).
///   Not found → error, asking the caller to run `handoff_init` on the
///   primary.
/// - Step 3: otherwise (no local `.handoff/`, not a worktree, or a regular
///   repo) → return `project_dir/.handoff`, the pre-init placeholder path
///   callers already expect.
pub fn resolve_handoff_dir(project_dir: &Path) -> Result<PathBuf> {
    let local = project_dir.join(".handoff");

    match local.symlink_metadata() {
        Ok(meta) if meta.file_type().is_symlink() => {
            // Distinguish a live symlink (Step 1) from a broken one (Step
            // 1.5) by trying to resolve what it points at.
            if local.exists() {
                return Ok(local);
            }
            eprintln!(
                "handoff-mcp: removing broken .handoff symlink at {}",
                local.display()
            );
            std::fs::remove_file(&local).with_context(|| {
                format!(
                    "Failed to remove broken .handoff symlink at {}",
                    local.display()
                )
            })?;
        }
        Ok(_) => {
            // A real directory (or file) already at `.handoff` — use as-is.
            return Ok(local);
        }
        Err(_) => {
            // Nothing at `.handoff` yet; fall through to worktree detection.
        }
    }

    if let Some(info) = detect_worktree(project_dir) {
        let primary_handoff = info.primary_dir.join(".handoff");
        if primary_handoff.exists() {
            return Ok(primary_handoff);
        }
        anyhow::bail!(
            ".handoff/ not found in this worktree or in the primary worktree ({}). \
             Run handoff_init in the primary worktree first.",
            info.primary_dir.display()
        );
    }

    Ok(local)
}

/// Legacy, infallible accessor kept for call sites that only need a path to
/// check existence against (`hdir.exists()`) rather than a resolved,
/// guaranteed-initialized directory. Delegates to [`resolve_handoff_dir`]
/// and falls back to the plain `project_dir/.handoff` path — matching prior
/// behavior — whenever resolution fails (i.e. before `handoff_init`, or in a
/// worktree whose primary has not been initialized either).
pub fn handoff_dir(project_dir: &Path) -> PathBuf {
    resolve_handoff_dir(project_dir).unwrap_or_else(|_| project_dir.join(".handoff"))
}

pub fn ensure_handoff_exists(project_dir: &Path) -> Result<PathBuf> {
    let dir = resolve_handoff_dir(project_dir)?;
    if !dir.exists() {
        anyhow::bail!(
            ".handoff/ directory not found in {}. Run handoff_init first.",
            project_dir.display()
        );
    }
    Ok(dir)
}

pub fn init_handoff(project_dir: &Path, project_name: &str, description: &str) -> Result<()> {
    let dir = handoff_dir(project_dir);
    if dir.exists() {
        anyhow::bail!(
            ".handoff/ already exists in {}. Project is already initialized.",
            project_dir.display()
        );
    }

    std::fs::create_dir_all(dir.join("sessions")).context("Failed to create .handoff/sessions/")?;
    std::fs::create_dir_all(dir.join("tasks")).context("Failed to create .handoff/tasks/")?;
    std::fs::create_dir_all(dir.join("memory")).context("Failed to create .handoff/memory/")?;

    let config = config::Config::new(project_name, description);
    config::write_config(&dir.join("config.toml"), &config)?;

    Ok(())
}
