pub mod config;
pub mod git;
pub mod sessions;
pub mod tasks;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

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

    let config = config::Config::new(project_name, description);
    config::write_config(&dir.join("config.toml"), &config)?;

    Ok(())
}
