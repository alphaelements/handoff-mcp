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
    /// Project-level start timestamp (pre-start mode). RFC3339 string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// Scheduling mode: "manual" or "auto".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_mode: Option<String>,
    /// Project-level label vocabulary.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "CalendarConfig::is_empty")]
    pub calendar: CalendarConfig,
    /// Team members keyed by stable assignee key.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub assignees: HashMap<String, AssigneeConfig>,
    /// Milestones keyed by milestone name.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub milestones: HashMap<String, MilestoneConfig>,
    #[serde(default, skip_serializing_if = "GanttViewConfig::is_empty")]
    pub gantt_view: GanttViewConfig,
    #[serde(default, skip_serializing_if = "EffortBudgetConfig::is_empty")]
    pub effort_budget: EffortBudgetConfig,
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

/// Project-wide working calendar. Mirrors VSCode `CalendarConfig`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CalendarConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_hours_per_day: Option<f64>,
    /// Weekday numbers (0=Sun..6=Sat) that are non-working.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub closed_weekdays: Vec<u32>,
    /// Specific YYYY-MM-DD dates that are non-working (override weekdays).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub closed_dates: Vec<String>,
    /// Specific YYYY-MM-DD dates that are working even if normally closed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_dates: Vec<String>,
    /// Per-weekday / per-date working-hour overrides. Key = weekday name or YYYY-MM-DD.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub day_hours: HashMap<String, f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overwork_limit_percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_utilization: Option<f64>,
}

impl CalendarConfig {
    pub fn is_empty(&self) -> bool {
        self.work_hours_per_day.is_none()
            && self.closed_weekdays.is_empty()
            && self.closed_dates.is_empty()
            && self.open_dates.is_empty()
            && self.day_hours.is_empty()
            && self.schedule_mode.is_none()
            && self.overwork_limit_percent.is_none()
            && self.max_utilization.is_none()
    }
}

/// A single team member's configuration. Mirrors VSCode `AssigneeConfig`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AssigneeConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_hours_per_day: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub closed_weekdays: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub closed_dates: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_dates: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub day_hours: HashMap<String, f64>,
}

/// A milestone definition. Mirrors VSCode `MilestoneConfig`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MilestoneConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Gantt view UI settings. Mirrors VSCode `GanttViewSettings`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GanttViewConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zoom: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_by_milestone: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_by_assignee: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_workload: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter_assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload_view: Option<String>,
}

impl GanttViewConfig {
    pub fn is_empty(&self) -> bool {
        self.sort.is_none()
            && self.zoom.is_none()
            && self.mode.is_none()
            && self.group_by_milestone.is_none()
            && self.group_by_assignee.is_none()
            && self.show_workload.is_none()
            && self.filter_assignee.is_none()
            && self.workload_view.is_none()
    }
}

/// Effort budget. Mirrors VSCode `budgetTotalHours`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EffortBudgetConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_hours: Option<f64>,
}

impl EffortBudgetConfig {
    pub fn is_empty(&self) -> bool {
        self.total_hours.is_none()
    }
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
            started_at: None,
            schedule_mode: None,
            labels: Vec::new(),
            calendar: CalendarConfig::default(),
            assignees: HashMap::new(),
            milestones: HashMap::new(),
            gantt_view: GanttViewConfig::default(),
            effort_budget: EffortBudgetConfig::default(),
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
    crate::storage::atomic_write(path, content.as_bytes())
        .with_context(|| format!("Failed to write config: {}", path.display()))?;
    Ok(())
}
