pub mod config;
pub mod git;
pub mod memory;
pub mod referrals;
pub mod sessions;
pub mod tasks;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Write `content` to `path` atomically: write to a sibling temp file, fsync,
/// then rename over the target. A rename within the same directory is atomic on
/// POSIX, so a concurrent reader never observes a partially-written file.
///
/// Used by every handoff write path (tasks, config, sessions, referrals) so that
/// the VSCode extension — which writes the same files — never reads torn data.
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

    // Unique-per-process temp name in the same directory (so rename is atomic).
    let tmp_name = format!(".{file_name}.tmp.{}", std::process::id());
    let tmp_path = dir.join(tmp_name);

    let mut f = std::fs::File::create(&tmp_path)
        .with_context(|| format!("Failed to create temp file {}", tmp_path.display()))?;
    f.write_all(content)
        .with_context(|| format!("Failed to write temp file {}", tmp_path.display()))?;
    f.sync_all()
        .with_context(|| format!("Failed to sync temp file {}", tmp_path.display()))?;
    drop(f);

    std::fs::rename(&tmp_path, path).with_context(|| {
        // Best-effort cleanup so a failed rename doesn't leave a stray temp file.
        let _ = std::fs::remove_file(&tmp_path);
        format!(
            "Failed to rename {} -> {}",
            tmp_path.display(),
            path.display()
        )
    })?;
    Ok(())
}

pub fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
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
