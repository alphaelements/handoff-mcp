use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub project: ProjectConfig,
    #[serde(default)]
    pub settings: SettingsConfig,
    #[serde(default)]
    pub dashboard: DashboardConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsConfig {
    #[serde(default = "default_history_limit")]
    pub history_limit: u32,
    #[serde(default = "default_done_task_limit")]
    pub done_task_limit: u32,
    #[serde(default = "default_auto_git_summary")]
    pub auto_git_summary: bool,
    #[serde(default)]
    pub context_files: Vec<String>,
    #[serde(default)]
    pub custom_fields: HashMap<String, toml::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardConfig {
    #[serde(default = "default_scan_dirs")]
    pub scan_dirs: Vec<String>,
    #[serde(default)]
    pub exclude_patterns: Vec<String>,
}

fn default_history_limit() -> u32 {
    20
}

fn default_done_task_limit() -> u32 {
    10
}

fn default_auto_git_summary() -> bool {
    true
}

fn default_scan_dirs() -> Vec<String> {
    vec!["~/pro/".to_string()]
}

impl Default for SettingsConfig {
    fn default() -> Self {
        Self {
            history_limit: default_history_limit(),
            done_task_limit: default_done_task_limit(),
            auto_git_summary: default_auto_git_summary(),
            context_files: Vec::new(),
            custom_fields: HashMap::new(),
        }
    }
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            scan_dirs: default_scan_dirs(),
            exclude_patterns: Vec::new(),
        }
    }
}

impl Config {
    pub fn new(name: &str, description: &str) -> Self {
        Self {
            project: ProjectConfig {
                name: name.to_string(),
                description: if description.is_empty() {
                    None
                } else {
                    Some(description.to_string())
                },
            },
            settings: SettingsConfig::default(),
            dashboard: DashboardConfig::default(),
        }
    }
}

pub fn read_config(path: &Path) -> Result<Config> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read config: {}", path.display()))?;
    let config: Config = toml::from_str(&content)
        .with_context(|| format!("Failed to parse config: {}", path.display()))?;
    Ok(config)
}

pub fn write_config(path: &Path, config: &Config) -> Result<()> {
    let content = toml::to_string_pretty(config).context("Failed to serialize config")?;
    std::fs::write(path, content)
        .with_context(|| format!("Failed to write config: {}", path.display()))?;
    Ok(())
}
