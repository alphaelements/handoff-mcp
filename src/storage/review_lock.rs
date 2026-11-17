//! Exclusive write lock for the review board (RB-009, SPEC-RB-024).
//!
//! Every write to `.handoff/review_board.md` (add, update, init, resolve) is a
//! read-modify-write. Two writers interleaving would lose one change, so each
//! takes this flock-style lock first, re-reads the board inside it, and keeps
//! it until the write has finished or failed. Readers never take it.
//!
//! The lock lives in its own file, never in the board, so the board's bytes
//! stay entirely the user's. The guard is RAII: the lock is released when it
//! is dropped, on success, on an early `?` return and on panic unwinding. The
//! OS also releases it if the process dies.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fs2::FileExt;

/// File name of the lock inside `.handoff/`.
pub const LOCK_FILE: &str = "review_board.lock";

/// Path of the lock file for a `.handoff/` directory.
pub fn lock_path(handoff_dir: &Path) -> PathBuf {
    handoff_dir.join(LOCK_FILE)
}

/// Holds the exclusive board lock until dropped.
#[derive(Debug)]
pub struct BoardLock {
    file: File,
}

impl BoardLock {
    /// Blocks until the exclusive lock is held. The lock file is created on
    /// demand and is never removed (removing it would let two writers lock
    /// two different inodes).
    pub fn acquire(handoff_dir: &Path) -> Result<Self> {
        let path = lock_path(handoff_dir);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("Failed to open lock file {}", path.display()))?;
        file.lock_exclusive()
            .with_context(|| format!("Failed to lock {}", path.display()))?;
        Ok(Self { file })
    }
}

impl Drop for BoardLock {
    fn drop(&mut self) {
        // An unlock failure cannot be reported from `drop`, and the lock is
        // released anyway when `file` closes right after this, so ignoring
        // the result loses nothing.
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn acquire_creates_the_lock_file() {
        let d = dir();
        let _lock = BoardLock::acquire(d.path()).unwrap();
        assert!(lock_path(d.path()).exists());
    }

    #[test]
    fn a_second_writer_waits_until_the_first_drops_the_lock() {
        let d = dir();
        let first = BoardLock::acquire(d.path()).unwrap();
        let (tx, rx) = mpsc::channel();
        let path = d.path().to_path_buf();
        let handle = std::thread::spawn(move || {
            let _second = BoardLock::acquire(&path).unwrap();
            tx.send(()).unwrap();
        });
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "second writer must block while the first holds the lock"
        );
        drop(first);
        rx.recv_timeout(Duration::from_secs(10))
            .expect("second writer acquires after release");
        handle.join().unwrap();
    }

    #[test]
    fn the_lock_is_released_when_the_holder_errors_out() {
        fn failing_write(dir: &Path) -> Result<()> {
            let _lock = BoardLock::acquire(dir)?;
            anyhow::bail!("write failed");
        }
        let d = dir();
        assert!(failing_write(d.path()).is_err());
        // Would hang here if the guard leaked; probe without blocking.
        let probe = OpenOptions::new()
            .write(true)
            .open(lock_path(d.path()))
            .unwrap();
        probe.try_lock_exclusive().expect("lock is free");
    }

    #[test]
    fn the_lock_is_released_when_the_holder_panics() {
        let d = dir();
        let path = d.path().to_path_buf();
        let result = std::thread::spawn(move || {
            let _lock = BoardLock::acquire(&path).unwrap();
            panic!("boom");
        })
        .join();
        assert!(result.is_err());
        let probe = OpenOptions::new()
            .write(true)
            .open(lock_path(d.path()))
            .unwrap();
        probe.try_lock_exclusive().expect("lock is free");
    }

    #[test]
    fn a_missing_directory_is_an_error_not_a_hang() {
        let d = dir();
        assert!(BoardLock::acquire(&d.path().join("absent")).is_err());
    }
}
