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
    // 1+2+4+8+16+32+64*3 ≈ 255ms of total backoff across the 9 retries.
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

pub fn handoff_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(".handoff")
}

pub fn ensure_handoff_exists(project_dir: &Path) -> Result<PathBuf> {
    let dir = handoff_dir(project_dir);
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
