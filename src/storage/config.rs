use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{Context, Result};
use serde::de::{self, SeqAccess};
use serde::{Deserialize, Deserializer, Serialize};

/// Convert a weekday name to its number (0=Sun..6=Sat).
pub fn weekday_to_num(s: &str) -> Option<u32> {
    match s.to_lowercase().as_str() {
        "sun" | "sunday" => Some(0),
        "mon" | "monday" => Some(1),
        "tue" | "tuesday" => Some(2),
        "wed" | "wednesday" => Some(3),
        "thu" | "thursday" => Some(4),
        "fri" | "friday" => Some(5),
        "sat" | "saturday" => Some(6),
        _ => None,
    }
}

fn deserialize_weekdays<'de, D>(deserializer: D) -> std::result::Result<Vec<u32>, D::Error>
where
    D: Deserializer<'de>,
{
    struct WeekdayVisitor;

    impl<'de> de::Visitor<'de> for WeekdayVisitor {
        type Value = Vec<u32>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("an array of weekday numbers (0-6) or names (\"sun\"..\"sat\")")
        }

        fn visit_seq<A>(self, mut seq: A) -> std::result::Result<Vec<u32>, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut vals = Vec::new();
            while let Some(elem) = seq.next_element::<toml::Value>()? {
                match &elem {
                    toml::Value::Integer(n) => {
                        let n = *n as u32;
                        if n > 6 {
                            return Err(de::Error::custom(format!(
                                "weekday number {n} out of range 0-6"
                            )));
                        }
                        vals.push(n);
                    }
                    toml::Value::String(s) => {
                        let n = weekday_to_num(s).ok_or_else(|| {
                            de::Error::custom(format!("unknown weekday name: \"{s}\""))
                        })?;
                        vals.push(n);
                    }
                    _ => {
                        return Err(de::Error::custom(
                            "closed_weekdays elements must be integers or strings",
                        ));
                    }
                }
            }
            Ok(vals)
        }
    }

    deserializer.deserialize_seq(WeekdayVisitor)
}

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
    /// Multi-worktree `.handoff/` sharing settings (spec §3.1.3). `serde(default)`
    /// keeps every config.toml written before this field existed parsing
    /// cleanly, with `auto_link` defaulting to `true`.
    #[serde(default)]
    pub worktree: WorktreeConfig,
    /// V-model layer/trace settings (wiki/220-vmodel-integration-design.md
    /// §2.1, M1 t360.4): `[trace] layers = [...]` (which of the 6 built-in
    /// layers are "in use"; omitted means auto-detect once M2's parsing
    /// lands — t360.4 only stores the config value) and
    /// `[trace.id_prefixes]` (project-added ID prefixes per layer, appended
    /// to the built-in defaults, never replacing them — §2.1).
    #[serde(default, skip_serializing_if = "TraceConfig::is_empty")]
    pub trace: TraceConfig,
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
    /// Default 2.0.
    #[serde(default = "default_memory_query_min_score")]
    pub memory_query_min_score: f64,
    /// Relative threshold (0.0–1.0) for `memory_query`: after the absolute
    /// `min_score` floor, a candidate is dropped unless its score is at least
    /// `top_score × relative_threshold`. Prevents low-relevance "tail" matches
    /// from riding a strong top hit. 0.0 disables (keep everything above
    /// `min_score`). Default 0.3.
    #[serde(default = "default_memory_query_relative_threshold")]
    pub memory_query_relative_threshold: f64,
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
    /// Accepts integers or weekday names ("sun", "sat", etc.) in TOML.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "deserialize_weekdays"
    )]
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
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "deserialize_weekdays"
    )]
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

/// Multi-worktree `.handoff/` sharing settings. Mirrors spec §3.1.3.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeConfig {
    /// Explicit override for where the shared `.handoff/` lives, taking
    /// precedence over auto-detection via the primary worktree. Supports a
    /// leading `~/` (expanded via [`crate::storage::expand_tilde`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_root: Option<String>,
    /// Whether `resolve_handoff_dir` should automatically create a symlink
    /// from a secondary worktree back to the shared `.handoff/`. Default
    /// `true`.
    #[serde(default = "default_auto_link")]
    pub auto_link: bool,
    /// Multi-WT session-loop settings. Mirrors spec §3.6. `serde(default)`
    /// keeps every config.toml written before this sub-section existed
    /// parsing cleanly, with `auto_assign` defaulting to `false`.
    #[serde(default)]
    pub session_loop: WorktreeSessionLoopConfig,
}

impl Default for WorktreeConfig {
    fn default() -> Self {
        Self {
            handoff_root: None,
            auto_link: default_auto_link(),
            session_loop: WorktreeSessionLoopConfig::default(),
        }
    }
}

fn default_auto_link() -> bool {
    true
}

/// Multi-WT session-loop settings, nested under `[worktree.session_loop]`.
/// Mirrors spec §3.6.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeSessionLoopConfig {
    /// Whether the manager should automatically assign tasks to worktrees.
    /// Default `false` (backward compatible with single-WT flow).
    #[serde(default)]
    pub auto_assign: bool,
    /// Default merge strategy used when consolidating WT branches:
    /// `"rebase-merge"` | `"merge-commit"` | `"squash-merge"`. Default
    /// `"merge-commit"`.
    #[serde(default = "default_merge_strategy")]
    pub merge_strategy: String,
    /// Maximum number of concurrently active worktrees. Default `4`.
    #[serde(default = "default_max_concurrent_wts")]
    pub max_concurrent_wts: u32,
    /// Whether to automatically remove a worktree after its branch is
    /// merged, without prompting the user. Default `false`.
    #[serde(default)]
    pub auto_cleanup: bool,
}

impl Default for WorktreeSessionLoopConfig {
    fn default() -> Self {
        Self {
            auto_assign: false,
            merge_strategy: default_merge_strategy(),
            max_concurrent_wts: default_max_concurrent_wts(),
            auto_cleanup: false,
        }
    }
}

fn default_merge_strategy() -> String {
    "merge-commit".to_string()
}

fn default_max_concurrent_wts() -> u32 {
    4
}

fn default_history_limit() -> u32 {
    20
}

/// `[trace]` config (wiki/220-vmodel-integration-design.md §2.1, M1
/// t360.4): the "used layers"/"extra ID prefixes" settings the built-in
/// 6-layer table (`storage::docs::layer::BUILTIN_LAYERS`) reads. Layer
/// *parsing* is t360.5's concern — this struct only stores the config
/// value, so it's independently testable and reviewable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceConfig {
    /// Layer ids considered "in use" (wiki/220 §2.1: "使用中の層"). Empty
    /// (the default, and TOML-omitted) means "not explicitly configured" —
    /// once M2 body parsing lands, an empty value falls back to
    /// auto-detecting which layers have at least one item; t360.4 itself
    /// makes no such determination.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<String>,
    /// Project-added ID prefixes per layer id (`[trace.id_prefixes]`),
    /// appended to that layer's built-in defaults — never replacing them
    /// (wiki/220 §2.1: "既定に追加される").
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub id_prefixes: HashMap<String, Vec<String>>,
    /// `[[trace.layer]]` project-defined layers (wiki/260-vmodel-m2-design.md
    /// §2.1, M2-01, FR-101 residual) — validated into a
    /// `storage::docs::layer::LayerRegistry` by every consumer, never used
    /// raw.
    #[serde(default, skip_serializing_if = "Vec::is_empty", rename = "layer")]
    pub layer: Vec<super::docs::layer::CustomLayerConfig>,
    /// Project default profile name (`[trace] profile`, wiki/260 §2.1):
    /// one of the 4 built-in profiles (`minimal`/`standard`/`full`/`bugfix`)
    /// or a key in `profiles`. Omitted/empty means "auto" (M1's own
    /// used-layers auto-detection, unchanged — §2.1's priority: `layers` >
    /// `profile` > auto).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// `[trace] done_guard` (wiki/260 §3.4): `"warn"` (default) | `"block"` |
    /// `"off"`. Enforcement is M2-13's scope — this struct only stores the
    /// setting.
    #[serde(
        default = "default_done_guard",
        skip_serializing_if = "is_default_done_guard"
    )]
    pub done_guard: String,
    /// `[trace.profiles.<name>]` project-defined profiles (wiki/260 §2.1).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub profiles: HashMap<String, TraceProfileConfig>,
    /// `[trace.lint]` (wiki/260 §4.3): per-rule severity overrides and
    /// policy (`require`) rules. Evaluation is M2-08's scope — this struct
    /// only stores the setting so every M2 config key lives here (M2-01's
    /// done_criteria).
    #[serde(default, skip_serializing_if = "TraceLintConfig::is_empty")]
    pub lint: TraceLintConfig,
    /// `[trace] auto_layer` (t391.1, REQ-VGAP-007): when `true`, `doc_save`
    /// infers a V-model `layer` from `doc_type` (see [`Self::infer_layer`])
    /// for documents saved without a `layer` argument. Default `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_layer: bool,
}

fn default_done_guard() -> String {
    "warn".to_string()
}

fn is_default_done_guard(v: &str) -> bool {
    v == "warn"
}

/// One `[trace.profiles.<name>]` entry (wiki/260 §2.1's example: `extends`,
/// `layers`, `implicit_acceptance`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraceProfileConfig {
    /// A built-in profile (`minimal`/`standard`/`full`/`bugfix`) or another
    /// project profile this one extends. Resolution/cycle-detection is a
    /// consumer concern (§2.1); this struct only stores the raw value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extends: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub implicit_acceptance: Option<bool>,
    /// NFR-006 (wiki/270-vmodel-m3-design.md §2.7): an additional safety-net
    /// cap on how many items `handoff_trace_scaffold`/`handoff_trace_tasks`
    /// may generate in one `mode="apply"` call under this profile. Generation
    /// itself is still controlled by each tool's own `limit` argument; this
    /// is only a warning when that count also exceeds this configured value.
    /// `None` (default) means no cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_generated_per_call: Option<u32>,
    /// FR-202 (wiki/270-vmodel-m3-design.md §2.1): `<layer id> -> [required
    /// layer id, ...]` — the coverage this profile requires for an item on
    /// that layer whose own `SubItem.needs` is `None` (unset). `None`
    /// (default, and TOML-omitted) means "derive naturally from `layers`"
    /// (`src/trace/profile.rs`'s `natural_default_needs`): a left-side layer
    /// requires verify-coverage from its `pair` layer when that pair is also
    /// in `layers`, and from the next-deeper left-side layer in `layers`
    /// (for vertical/refine coverage) when one exists. A layer with no
    /// entry here (explicit or derived) requires no coverage at all for that
    /// layer's items (same effect as that item authoring `- needs:` with an
    /// empty value).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_needs: Option<BTreeMap<String, Vec<String>>>,
}

/// `[trace.lint]` (wiki/260 §4.3).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraceLintConfig {
    /// Per-rule severity override (`"error"|"warning"|"info"|"off"`), keyed
    /// by rule id (`[trace.lint.rules]`).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub rules: HashMap<String, String>,
    /// `[[trace.lint.require]]` policy rules (§4.3's `p0-needs-verification`
    /// example).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub require: Vec<TraceLintRequireRule>,
}

impl TraceLintConfig {
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty() && self.require.is_empty()
    }
}

/// One `[[trace.lint.require]]` policy rule (§4.3). `id`/`need` are
/// `#[serde(default)]` (empty string) for the same fail-safe reason as
/// `CustomLayerConfig`'s fields (`storage::docs::layer`, M2-01 rework): a
/// `[[trace.lint.require]]` entry missing either must still deserialize, so
/// evaluation (M2-08's scope) can reject it as one invalid rule rather than
/// `toml::from_str` failing the whole `config.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraceLintRequireRule {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub when: TraceLintRequireWhen,
    /// `verified_by | refined_by | implemented_by_task | passing |
    /// no_suspect | auto_test`.
    #[serde(default)]
    pub need: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
}

/// `[[trace.lint.require]]`'s `when` selector (§4.3: "使えるキー: layer,
/// priority, method, doc, approval"). A field being `None`/empty means "no
/// filter on this key".
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraceLintRequireWhen {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub priority: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// M3-05 (wiki/270-vmodel-m3-design.md §2.3/§4.3, FR-406): `None` means
    /// "no filter on this key"; `Some(values)` matches an item whose
    /// resolved `approval` is any one of `values` (e.g. `["review",
    /// "approved"]`). Backward compat: a single TOML string (the pre-M3-05
    /// `Option<String>` shape, `when.approval = "approved"`) deserializes as
    /// a 1-element `Vec` via [`deserialize_approval_values`].
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_approval_values"
    )]
    pub approval: Option<Vec<String>>,
}

/// Accepts either a single TOML string or an array of strings for
/// `when.approval` (M3-05) — `#[serde(untagged)]` on a helper enum, the
/// standard serde idiom for "one value or many" (same shape as
/// `deserialize_weekdays` above, but via an enum since TOML strings/arrays
/// are both straightforward `Deserialize` targets here, unlike that
/// function's int-or-name per-element parsing).
fn deserialize_approval_values<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }

    Ok(
        Option::<OneOrMany>::deserialize(deserializer)?.map(|v| match v {
            OneOrMany::One(s) => vec![s],
            OneOrMany::Many(v) => v,
        }),
    )
}

impl Default for TraceConfig {
    fn default() -> Self {
        TraceConfig {
            layers: Vec::new(),
            id_prefixes: HashMap::new(),
            layer: Vec::new(),
            profile: None,
            done_guard: default_done_guard(),
            profiles: HashMap::new(),
            lint: TraceLintConfig::default(),
            auto_layer: false,
        }
    }
}

impl TraceConfig {
    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
            && self.id_prefixes.is_empty()
            && self.layer.is_empty()
            && self.profile.is_none()
            && is_default_done_guard(&self.done_guard)
            && self.profiles.is_empty()
            && self.lint.is_empty()
            && !self.auto_layer
    }

    /// `doc_type` -> `layer` inference table for `[trace] auto_layer`.
    /// `None` when `auto_layer` is off or the `doc_type` takes no layer
    /// (`adr`/`guide`/`note`/unknown).
    pub fn infer_layer(&self, doc_type: &str) -> Option<&'static str> {
        if !self.auto_layer {
            return None;
        }
        match doc_type {
            "spec" => Some("requirement"),
            "design" => Some("detailed_spec"),
            _ => None,
        }
    }
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
    2.0
}

fn default_memory_query_relative_threshold() -> f64 {
    0.3
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
            memory_query_relative_threshold: default_memory_query_relative_threshold(),
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
            worktree: WorktreeConfig::default(),
            trace: TraceConfig::default(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_config(toml_str: &str) -> Config {
        toml::from_str(toml_str).unwrap()
    }

    #[test]
    fn trace_auto_layer_defaults_false_and_deserializes() {
        let cfg = parse_config("[project]\nname = \"test\"\n");
        assert!(!cfg.trace.auto_layer);
        let cfg = parse_config("[project]\nname = \"test\"\n[trace]\nauto_layer = true\n");
        assert!(cfg.trace.auto_layer);
        assert!(!cfg.trace.is_empty());
    }

    #[test]
    fn trace_auto_layer_infers_layer_from_doc_type() {
        let on = TraceConfig {
            auto_layer: true,
            ..TraceConfig::default()
        };
        assert_eq!(on.infer_layer("spec"), Some("requirement"));
        assert_eq!(on.infer_layer("design"), Some("detailed_spec"));
        for doc_type in ["adr", "guide", "note", "unknown"] {
            assert_eq!(on.infer_layer(doc_type), None, "{doc_type}");
        }
        assert_eq!(TraceConfig::default().infer_layer("spec"), None);
    }

    #[test]
    fn closed_weekdays_string_names() {
        let cfg = parse_config(
            r#"
[project]
name = "test"
[calendar]
closed_weekdays = ["sun", "sat"]
"#,
        );
        assert_eq!(cfg.calendar.closed_weekdays, vec![0, 6]);
    }

    #[test]
    fn closed_weekdays_integer_values() {
        let cfg = parse_config(
            r#"
[project]
name = "test"
[calendar]
closed_weekdays = [0, 6]
"#,
        );
        assert_eq!(cfg.calendar.closed_weekdays, vec![0, 6]);
    }

    #[test]
    fn closed_weekdays_mixed() {
        let cfg = parse_config(
            r#"
[project]
name = "test"
[calendar]
closed_weekdays = ["sun", 6]
"#,
        );
        assert_eq!(cfg.calendar.closed_weekdays, vec![0, 6]);
    }

    #[test]
    fn closed_weekdays_empty() {
        let cfg = parse_config(
            r#"
[project]
name = "test"
"#,
        );
        assert!(cfg.calendar.closed_weekdays.is_empty());
    }

    /// M1 t360.4 (wiki/220-vmodel-integration-design.md §2.1): `[trace]
    /// layers` and `[trace.id_prefixes]` parse into `TraceConfig`.
    #[test]
    fn trace_config_parses_layers_and_id_prefixes() {
        let cfg = parse_config(
            r#"
[project]
name = "test"
[trace]
layers = ["requirement", "basic_spec", "acceptance", "system_test"]
[trace.id_prefixes]
requirement = ["UC"]
"#,
        );
        assert_eq!(
            cfg.trace.layers,
            vec!["requirement", "basic_spec", "acceptance", "system_test"]
        );
        assert_eq!(
            cfg.trace.id_prefixes.get("requirement"),
            Some(&vec!["UC".to_string()])
        );
    }

    /// No `[trace]` section at all must parse cleanly to an empty
    /// `TraceConfig` (NFR-001/002: every pre-M1 `config.toml` still parses).
    #[test]
    fn trace_config_defaults_to_empty_when_section_absent() {
        let cfg = parse_config(
            r#"
[project]
name = "test"
"#,
        );
        assert!(cfg.trace.layers.is_empty());
        assert!(cfg.trace.id_prefixes.is_empty());
    }

    /// NFR-004 (no spurious diff): a `Config` with no `[trace]` settings
    /// must round-trip through `toml::to_string` without gaining a `[trace]`
    /// section — otherwise every existing `config.toml` would show a diff
    /// the first time handoff-mcp re-writes it, even though nothing was
    /// configured.
    #[test]
    fn trace_section_is_absent_from_serialized_config_when_empty() {
        let cfg = Config::new("test", "");
        let toml_str = toml::to_string(&cfg).unwrap();
        assert!(
            !toml_str.contains("[trace]"),
            "empty TraceConfig must not appear in serialized config: {toml_str}"
        );
    }

    /// The inverse of the above: once `layers`/`id_prefixes` are set, they
    /// must round-trip through a full serialize/parse cycle.
    #[test]
    fn trace_section_round_trips_when_set() {
        let mut cfg = Config::new("test", "");
        cfg.trace.layers = vec!["requirement".to_string(), "acceptance".to_string()];
        cfg.trace
            .id_prefixes
            .insert("requirement".to_string(), vec!["UC".to_string()]);
        let toml_str = toml::to_string(&cfg).unwrap();
        let back: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(back.trace.layers, cfg.trace.layers);
        assert_eq!(back.trace.id_prefixes, cfg.trace.id_prefixes);
    }

    #[test]
    fn closed_weekdays_full_names() {
        let cfg = parse_config(
            r#"
[project]
name = "test"
[calendar]
closed_weekdays = ["sunday", "saturday"]
"#,
        );
        assert_eq!(cfg.calendar.closed_weekdays, vec![0, 6]);
    }

    #[test]
    fn assignee_closed_weekdays_strings() {
        let cfg = parse_config(
            r#"
[project]
name = "test"
[assignees.alice]
closed_weekdays = ["mon", "fri"]
"#,
        );
        let alice = cfg.assignees.get("alice").unwrap();
        assert_eq!(alice.closed_weekdays, vec![1, 5]);
    }

    #[test]
    fn closed_weekdays_invalid_name() {
        let result = toml::from_str::<Config>(
            r#"
[project]
name = "test"
[calendar]
closed_weekdays = ["funday"]
"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn closed_weekdays_out_of_range() {
        let result = toml::from_str::<Config>(
            r#"
[project]
name = "test"
[calendar]
closed_weekdays = [7]
"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn round_trip_preserves_integer_format() {
        let cfg = parse_config(
            r#"
[project]
name = "test"
[calendar]
closed_weekdays = ["sun", "sat"]
"#,
        );
        let serialized = toml::to_string_pretty(&cfg).unwrap();
        let re_parsed = parse_config(&serialized);
        assert_eq!(re_parsed.calendar.closed_weekdays, vec![0, 6]);
    }

    // -- [trace] M2 config keys (wiki/260 §2.1/§3.4/§4.3, M2-01) --

    #[test]
    fn trace_config_default_done_guard_is_warn() {
        let cfg = parse_config(
            r#"
[project]
name = "test"
"#,
        );
        assert_eq!(cfg.trace.done_guard, "warn");
        assert!(
            cfg.trace.is_empty(),
            "an all-default [trace] must stay 'empty' (no spurious diff, NFR-004)"
        );
    }

    #[test]
    fn trace_config_parses_custom_layer_declarations() {
        let cfg = parse_config(
            r#"
[project]
name = "test"

[[trace.layer]]
id = "ux_spec"
display_name = "UX 仕様"
side = "left"
level = 2
pair = "usability_test"
id_prefixes = ["UX"]

[[trace.layer]]
id = "usability_test"
side = "right"
level = 2
pair = "ux_spec"
"#,
        );
        assert_eq!(cfg.trace.layer.len(), 2);
        assert_eq!(cfg.trace.layer[0].id, "ux_spec");
        assert_eq!(cfg.trace.layer[0].display_name.as_deref(), Some("UX 仕様"));
        assert_eq!(cfg.trace.layer[0].id_prefixes, vec!["UX".to_string()]);
        assert_eq!(cfg.trace.layer[1].display_name, None);
    }

    /// A `[[trace.layer]]` entry missing `id` (e.g. a typo'd or half-written
    /// entry) must still deserialize the *whole* `config.toml` — `id` is
    /// `LayerRegistry::build`'s validation concern (it already warns+skips
    /// empty ids at `layer.rs`'s `build`), not `toml::from_str`'s. Before the
    /// M2-01 rework fix, `id: String` had no `#[serde(default)]` so a missing
    /// `id` failed `toml::from_str` for the entire file, silently discarding
    /// every other `[trace]` setting (id_prefixes, profile, profiles, lint,
    /// done_guard) for the whole project — the same fail-open class already
    /// fixed for `side`/`level`/`pair` in round 2.
    #[test]
    fn trace_layer_entry_missing_id_still_parses_whole_config() {
        let cfg = parse_config(
            r#"
[project]
name = "test"

[[trace.layer]]
side = "left"
level = 2
pair = "usability_test"

[trace]
done_guard = "error"
"#,
        );
        assert_eq!(cfg.trace.layer.len(), 1);
        assert_eq!(cfg.trace.layer[0].id, "");
        assert_eq!(cfg.trace.layer[0].side, "left");
        // The rest of [trace] must not be discarded by the malformed entry.
        assert_eq!(cfg.trace.done_guard, "error");
    }

    #[test]
    fn trace_config_parses_profile_and_profiles() {
        let cfg = parse_config(
            r#"
[project]
name = "test"

[trace]
profile = "web"
done_guard = "block"

[trace.profiles.web]
extends = "standard"
layers = ["requirement", "ux_spec", "acceptance", "usability_test"]
implicit_acceptance = false
"#,
        );
        assert_eq!(cfg.trace.profile.as_deref(), Some("web"));
        assert_eq!(cfg.trace.done_guard, "block");
        let web = cfg.trace.profiles.get("web").unwrap();
        assert_eq!(web.extends.as_deref(), Some("standard"));
        assert_eq!(web.layers.len(), 4);
        assert_eq!(web.implicit_acceptance, Some(false));
    }

    /// NFR-006 (wiki/270 §2.7): `max_generated_per_call` round-trips through
    /// TOML (deserialize) and is omitted from re-serialization when unset
    /// (`skip_serializing_if = "Option::is_none"`, same convention as every
    /// other optional field on this struct).
    #[test]
    fn trace_profile_config_parses_max_generated_per_call() {
        let cfg = parse_config(
            r#"
[project]
name = "test"

[trace.profiles.test]
max_generated_per_call = 2
"#,
        );
        let test_profile = cfg.trace.profiles.get("test").unwrap();
        assert_eq!(test_profile.max_generated_per_call, Some(2));
    }

    #[test]
    fn trace_profile_config_max_generated_per_call_defaults_to_none() {
        let cfg = parse_config(
            r#"
[project]
name = "test"

[trace.profiles.test]
layers = ["requirement"]
"#,
        );
        let test_profile = cfg.trace.profiles.get("test").unwrap();
        assert_eq!(test_profile.max_generated_per_call, None);
    }

    #[test]
    fn trace_profile_config_omits_max_generated_per_call_when_none_on_serialize() {
        let profile = TraceProfileConfig {
            max_generated_per_call: None,
            ..Default::default()
        };
        let toml_str = toml::to_string(&profile).unwrap();
        assert!(
            !toml_str.contains("max_generated_per_call"),
            "expected no max_generated_per_call key, got: {toml_str}"
        );
    }

    #[test]
    fn trace_profile_config_serializes_max_generated_per_call_when_set() {
        let profile = TraceProfileConfig {
            max_generated_per_call: Some(5),
            ..Default::default()
        };
        let toml_str = toml::to_string(&profile).unwrap();
        assert!(toml_str.contains("max_generated_per_call = 5"));
    }

    /// FR-202 (wiki/270 §2.1): `default_needs` round-trips through TOML as a
    /// `<layer id> -> [layer id, ...]` table.
    #[test]
    fn trace_profile_config_parses_default_needs() {
        let cfg = parse_config(
            r#"
[project]
name = "test"

[trace.profiles.test]
layers = ["requirement", "acceptance"]

[trace.profiles.test.default_needs]
requirement = ["acceptance"]
"#,
        );
        let test_profile = cfg.trace.profiles.get("test").unwrap();
        assert_eq!(
            test_profile
                .default_needs
                .as_ref()
                .and_then(|m| m.get("requirement")),
            Some(&vec!["acceptance".to_string()])
        );
    }

    #[test]
    fn trace_profile_config_default_needs_defaults_to_none() {
        let cfg = parse_config(
            r#"
[project]
name = "test"

[trace.profiles.test]
layers = ["requirement"]
"#,
        );
        let test_profile = cfg.trace.profiles.get("test").unwrap();
        assert_eq!(test_profile.default_needs, None);
    }

    #[test]
    fn trace_profile_config_omits_default_needs_when_none_on_serialize() {
        let profile = TraceProfileConfig {
            default_needs: None,
            ..Default::default()
        };
        let toml_str = toml::to_string(&profile).unwrap();
        assert!(
            !toml_str.contains("default_needs"),
            "expected no default_needs key, got: {toml_str}"
        );
    }

    #[test]
    fn trace_profile_config_serializes_default_needs_when_set() {
        let mut default_needs = BTreeMap::new();
        default_needs.insert("requirement".to_string(), vec!["acceptance".to_string()]);
        let profile = TraceProfileConfig {
            default_needs: Some(default_needs),
            ..Default::default()
        };
        let toml_str = toml::to_string(&profile).unwrap();
        assert!(toml_str.contains("[default_needs]"));
        assert!(toml_str.contains("requirement = [\"acceptance\"]"));
    }

    #[test]
    fn trace_config_parses_lint_rules_and_require() {
        let cfg = parse_config(
            r#"
[project]
name = "test"

[trace.lint.rules]
unbaselined = "off"

[[trace.lint.require]]
id = "p0-needs-verification"
need = "verified_by"
severity = "error"

[trace.lint.require.when]
layer = "requirement"
priority = ["P0", "P1"]
"#,
        );
        assert_eq!(
            cfg.trace.lint.rules.get("unbaselined").map(String::as_str),
            Some("off")
        );
        assert_eq!(cfg.trace.lint.require.len(), 1);
        let rule = &cfg.trace.lint.require[0];
        assert_eq!(rule.id, "p0-needs-verification");
        assert_eq!(rule.need, "verified_by");
        assert_eq!(rule.severity.as_deref(), Some("error"));
        assert_eq!(rule.when.layer.as_deref(), Some("requirement"));
        assert_eq!(rule.when.priority, vec!["P0".to_string(), "P1".to_string()]);
    }

    /// M3-05 (wiki/270-vmodel-m3-design.md §2.3/§4.3, FR-406): `when.approval`
    /// accepts a TOML array of approval values (`["review", "approved"]`),
    /// matching either.
    #[test]
    fn trace_lint_require_when_approval_parses_an_array() {
        let cfg = parse_config(
            r#"
[project]
name = "test"

[[trace.lint.require]]
id = "needs-approval"
need = "no_suspect"

[trace.lint.require.when]
approval = ["review", "approved"]
"#,
        );
        assert_eq!(
            cfg.trace.lint.require[0].when.approval,
            Some(vec!["review".to_string(), "approved".to_string()])
        );
    }

    /// Backward compat (§2.3): a single string `when.approval = "approved"`
    /// (the pre-M3-05 `Option<String>` shape) is read as a 1-element array.
    #[test]
    fn trace_lint_require_when_approval_parses_a_single_string_for_backward_compat() {
        let cfg = parse_config(
            r#"
[project]
name = "test"

[[trace.lint.require]]
id = "needs-approval"
need = "no_suspect"

[trace.lint.require.when]
approval = "approved"
"#,
        );
        assert_eq!(
            cfg.trace.lint.require[0].when.approval,
            Some(vec!["approved".to_string()])
        );
    }

    /// Omitted `when.approval` stays `None` (no filter on the approval axis).
    #[test]
    fn trace_lint_require_when_approval_defaults_to_none_when_omitted() {
        let cfg = parse_config(
            r#"
[project]
name = "test"

[[trace.lint.require]]
id = "needs-verification"
need = "verified_by"

[trace.lint.require.when]
layer = "requirement"
"#,
        );
        assert_eq!(cfg.trace.lint.require[0].when.approval, None);
    }

    /// The array form must round-trip through re-serialization unchanged.
    #[test]
    fn trace_lint_require_when_approval_array_round_trips_through_serialize() {
        let cfg = parse_config(
            r#"
[project]
name = "test"

[[trace.lint.require]]
id = "needs-approval"
need = "no_suspect"

[trace.lint.require.when]
approval = ["review", "approved"]
"#,
        );
        let serialized = toml::to_string_pretty(&cfg).unwrap();
        let re_parsed = parse_config(&serialized);
        assert_eq!(
            re_parsed.trace.lint.require[0].when.approval,
            Some(vec!["review".to_string(), "approved".to_string()])
        );
    }

    /// A `[[trace.lint.require]]` entry missing `id`/`need` must still parse
    /// (M2-01 rework, same fail-safe rationale as `[[trace.layer]]`'s
    /// missing-field handling) — evaluation of the rule is M2-08's scope,
    /// not this struct's.
    #[test]
    fn trace_lint_require_rule_with_missing_id_and_need_still_parses() {
        let cfg = parse_config(
            r#"
[project]
name = "test"

[[trace.lint.require]]
severity = "error"
"#,
        );
        assert_eq!(cfg.trace.lint.require.len(), 1);
        assert_eq!(cfg.trace.lint.require[0].id, "");
        assert_eq!(cfg.trace.lint.require[0].need, "");
    }

    #[test]
    fn trace_config_round_trips_through_serialize() {
        let cfg = parse_config(
            r#"
[project]
name = "test"

[trace]
profile = "web"

[[trace.layer]]
id = "ux_spec"
side = "left"
level = 2
pair = "usability_test"
"#,
        );
        let serialized = toml::to_string_pretty(&cfg).unwrap();
        let re_parsed = parse_config(&serialized);
        assert_eq!(re_parsed.trace.profile.as_deref(), Some("web"));
        assert_eq!(re_parsed.trace.layer.len(), 1);
        assert_eq!(re_parsed.trace.layer[0].id, "ux_spec");
    }
}
