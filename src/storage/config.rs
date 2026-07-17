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
    /// Require `estimate_hours` when creating/updating leaf tasks. Default true.
    #[serde(default = "default_require_estimate_hours")]
    pub require_estimate_hours: bool,
    /// Multiplier applied to AI-entered `estimate_hours` to derive the
    /// adjusted (AI-effort) estimate at aggregation time. Default 0.2.
    #[serde(default = "default_ai_estimate_multiplier")]
    pub ai_estimate_multiplier: f64,
    #[serde(default)]
    pub context_files: Vec<String>,
    /// Master switch for the memory feature (save/query/cleanup). Default true.
    #[serde(default = "default_memory_enabled")]
    pub memory_enabled: bool,
    /// Jaccard threshold above which `memory_save` treats a save as a
    /// near-duplicate `conflict` for the AI to merge. Default 0.72.
    #[serde(default = "default_memory_dup_threshold")]
    pub memory_dup_threshold: f64,
    /// BM25 relevance floor for `memory_query`; scores below are not returned.
    /// Default 0.5.
    #[serde(default = "default_memory_query_min_score")]
    pub memory_query_min_score: f64,
    /// Maximum number of memories `memory_query` returns per call. Default 5.
    #[serde(default = "default_memory_query_limit")]
    pub memory_query_limit: u32,
    /// Days after which an un(re)referenced memory is flagged `stale` by
    /// `memory_cleanup`. Default 60.
    #[serde(default = "default_memory_stale_days")]
    pub memory_stale_days: i64,
    /// Age (days) past which `memory_cleanup` garbage-collects a per-session
    /// `injected/` sidecar. Default 14.
    #[serde(default = "default_memory_injected_gc_days")]
    pub memory_injected_gc_days: i64,
    /// Timer provider mode: "auto" (authority-based), "vscode" (always delegate),
    /// "mcp" (always internal), "off" (disabled). Default "auto".
    #[serde(default = "default_timer_provider")]
    pub timer_provider: String,
    /// Heartbeat staleness threshold in seconds for authority.json. Default 30.
    #[serde(default = "default_timer_authority_ttl_secs")]
    pub timer_authority_ttl_secs: u64,
    /// Idle timeout in minutes for MCP fallback timer. Default 10.
    #[serde(default = "default_timer_idle_timeout_minutes")]
    pub timer_idle_timeout_minutes: u64,
    /// Allow multiple active sessions simultaneously. Default false (single-active).
    #[serde(default)]
    pub multi_session: bool,
    #[serde(default)]
    pub custom_fields: HashMap<String, toml::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardConfig {
    #[serde(default = "default_scan_dirs")]
    pub scan_dirs: Vec<String>,
    #[serde(default)]
    pub exclude_patterns: Vec<String>,
    /// Maximum directory depth for recursive scanning. Default 5 (mirrors VSCode side).
    #[serde(default = "default_max_depth")]
    pub max_depth: usize,
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

fn default_require_estimate_hours() -> bool {
    true
}

fn default_ai_estimate_multiplier() -> f64 {
    0.2
}

fn default_memory_enabled() -> bool {
    true
}

fn default_memory_dup_threshold() -> f64 {
    0.72
}

fn default_memory_query_min_score() -> f64 {
    0.1
}

fn default_memory_query_limit() -> u32 {
    5
}

fn default_memory_stale_days() -> i64 {
    60
}

fn default_memory_injected_gc_days() -> i64 {
    14
}

fn default_timer_provider() -> String {
    "auto".to_string()
}

fn default_timer_authority_ttl_secs() -> u64 {
    30
}

fn default_timer_idle_timeout_minutes() -> u64 {
    10
}

fn default_scan_dirs() -> Vec<String> {
    vec!["~/pro/".to_string()]
}

fn default_max_depth() -> usize {
    5
}

impl Default for SettingsConfig {
    fn default() -> Self {
        Self {
            history_limit: default_history_limit(),
            done_task_limit: default_done_task_limit(),
            auto_git_summary: default_auto_git_summary(),
            require_estimate_hours: default_require_estimate_hours(),
            ai_estimate_multiplier: default_ai_estimate_multiplier(),
            context_files: Vec::new(),
            memory_enabled: default_memory_enabled(),
            memory_dup_threshold: default_memory_dup_threshold(),
            memory_query_min_score: default_memory_query_min_score(),
            memory_query_limit: default_memory_query_limit(),
            memory_stale_days: default_memory_stale_days(),
            memory_injected_gc_days: default_memory_injected_gc_days(),
            timer_provider: default_timer_provider(),
            timer_authority_ttl_secs: default_timer_authority_ttl_secs(),
            timer_idle_timeout_minutes: default_timer_idle_timeout_minutes(),
            multi_session: false,
            custom_fields: HashMap::new(),
        }
    }
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            scan_dirs: default_scan_dirs(),
            exclude_patterns: Vec::new(),
            max_depth: default_max_depth(),
        }
    }
}

impl Config {
    pub fn new(name: &str, description: &str) -> Self {
        let settings = SettingsConfig {
            multi_session: true,
            ..SettingsConfig::default()
        };
        Self {
            project: ProjectConfig {
                name: name.to_string(),
                description: if description.is_empty() {
                    None
                } else {
                    Some(description.to_string())
                },
            },
            settings,
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
