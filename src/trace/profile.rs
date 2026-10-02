//! Project-wide profile resolution (wiki/260-vmodel-m2-design.md §2.1,
//! M2-01, FR-201): the 4 built-in profiles (`minimal`/`standard`/`full`/
//! `bugfix`) plus `[trace.profiles.<name>]` project-defined ones (with
//! `extends`), resolved into an effective `{layers, implicit_acceptance}`.
//!
//! This is deliberately the **project-wide** resolution only — a
//! per-document `trace_profile` override and its tree-inheritance onto
//! descendant items (wiki/260 §2.1 規則 1-4) is M2-03's scope. Per-document
//! overrides are exposed via `handoff_doc_save`'s `trace_profile` argument
//! (`src/mcp/handlers/docs.rs`), not a separate `handoff_trace_profile` tool.
//! [`super::engine::resolve_in_use_layers`]'s tier-2 priority
//! (`configured_layers` ＞ `profile_layers` ＞ auto) is this module's only
//! consumer of [`resolve_project_profile`] today.

use crate::storage::config::TraceConfig;
use crate::storage::docs::layer::LayerRegistry;

/// One of the 4 built-in profiles' `{layers, implicit_acceptance}` (wiki/260
/// §2.1's table). `bugfix`'s display-name overrides
/// (`requirement`→"再現条件", `acceptance`→"回帰テスト") are a display-only
/// concern for `_trace_report.json`/VSCode (§5.1), not this resolution.
fn builtin_profile(name: &str) -> Option<(&'static [&'static str], bool)> {
    match name {
        "minimal" => Some((&["requirement", "acceptance"], true)),
        "standard" => Some((
            &["requirement", "basic_spec", "acceptance", "system_test"],
            false,
        )),
        "full" => Some((
            &[
                "requirement",
                "basic_spec",
                "detailed_spec",
                "acceptance",
                "system_test",
                "unit_test",
            ],
            false,
        )),
        "bugfix" => Some((&["requirement", "acceptance"], true)),
        _ => None,
    }
}

/// The 4 built-in profile names, in a fixed order — reserved for a future
/// profile-listing surface (no consumer yet; `resolve_project_profile` and
/// `resolve_profile_by_name` accept a name directly and don't need this
/// list).
pub const BUILTIN_PROFILE_NAMES: &[&str] = &["minimal", "standard", "full", "bugfix"];

/// A fully resolved profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProfile {
    pub name: String,
    pub layers: Vec<String>,
    pub implicit_acceptance: bool,
    /// `true` when `name` is one of the 4 built-ins (not a
    /// `[trace.profiles.<name>]` entry).
    pub builtin: bool,
    /// NFR-006 (wiki/270 §2.7): the resolved `max_generated_per_call` cap, if
    /// any `[trace.profiles.<name>]` entry in the `extends` chain set one.
    /// Built-in profiles never set this (`None`). Same first-write-wins
    /// resolution order as `layers`/`implicit_acceptance`: the first entry in
    /// the chain (starting at `name` itself) that sets a value wins over any
    /// ancestor's value reached later via `extends`.
    pub max_generated_per_call: Option<u32>,
}

const MAX_EXTENDS_DEPTH: usize = 8;

/// Resolves `name` (a built-in profile, or a key in `trace.profiles`)
/// against its `extends` chain, detecting cycles and unknown targets as
/// warnings rather than errors (§2.1: a config problem disables/falls back,
/// it never stops the whole request). Layer ids not known to `registry` are
/// dropped from the result with a warning.
pub fn resolve_profile_by_name(
    name: &str,
    trace_config: &TraceConfig,
    registry: &LayerRegistry,
) -> (Option<ResolvedProfile>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut chain_seen = std::collections::HashSet::new();
    let mut current = name.to_string();
    let mut layers: Option<Vec<String>> = None;
    let mut implicit_acceptance: Option<bool> = None;
    let mut max_generated_per_call: Option<u32> = None;
    let mut depth = 0;

    loop {
        if !chain_seen.insert(current.clone()) {
            warnings.push(format!(
                "trace.profiles '{name}': extends chain has a cycle at '{current}', stopping resolution"
            ));
            break;
        }
        depth += 1;
        if depth > MAX_EXTENDS_DEPTH {
            warnings.push(format!(
                "trace.profiles '{name}': extends chain too deep (> {MAX_EXTENDS_DEPTH}), stopping resolution"
            ));
            break;
        }
        if let Some((builtin_layers, builtin_implicit)) = builtin_profile(&current) {
            if layers.is_none() {
                layers = Some(builtin_layers.iter().map(|s| s.to_string()).collect());
            }
            if implicit_acceptance.is_none() {
                implicit_acceptance = Some(builtin_implicit);
            }
            break; // built-ins never extend further.
        }
        let Some(custom) = trace_config.profiles.get(&current) else {
            warnings.push(format!(
                "trace.profiles '{name}': references unknown profile '{current}', stopping resolution"
            ));
            break;
        };
        if layers.is_none() && !custom.layers.is_empty() {
            layers = Some(custom.layers.clone());
        }
        if implicit_acceptance.is_none() {
            implicit_acceptance = custom.implicit_acceptance;
        }
        if max_generated_per_call.is_none() {
            max_generated_per_call = custom.max_generated_per_call;
        }
        match &custom.extends {
            Some(next) if !next.is_empty() => current = next.clone(),
            _ => break,
        }
    }

    let Some(mut resolved_layers) = layers else {
        return (None, warnings);
    };
    resolved_layers.retain(|l| {
        let known = registry.get(l).is_some();
        if !known {
            warnings.push(format!(
                "trace.profiles '{name}': references unknown layer '{l}', ignored"
            ));
        }
        known
    });

    let builtin = builtin_profile(name).is_some();
    (
        Some(ResolvedProfile {
            name: name.to_string(),
            layers: resolved_layers,
            implicit_acceptance: implicit_acceptance.unwrap_or(false),
            builtin,
            max_generated_per_call,
        }),
        warnings,
    )
}

/// The project *default* profile (`[trace] profile`, §2.1) — `None` when
/// unset/empty (falls through to auto-detection, per
/// [`super::engine::resolve_in_use_layers`]'s priority order). When both
/// `[trace] layers` and `[trace] profile` are set, `[trace] layers` wins
/// (already enforced by `resolve_in_use_layers`'s own tier order) but a
/// warning is added here so the caller is told their profile is being
/// ignored (§2.1: "`layers` とプロファイルを両方書いた場合は `layers` を
/// 使い warning を出す").
pub fn resolve_project_profile(
    trace_config: &TraceConfig,
    registry: &LayerRegistry,
) -> (Option<ResolvedProfile>, Vec<String>) {
    match trace_config.profile.as_deref().filter(|s| !s.is_empty()) {
        Some(name) => {
            let (resolved, mut warnings) = resolve_profile_by_name(name, trace_config, registry);
            if !trace_config.layers.is_empty() {
                warnings.push(format!(
                    "[trace] layers and [trace] profile = \"{name}\" are both set; [trace] layers takes priority"
                ));
            }
            (resolved, warnings)
        }
        None => (None, Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> LayerRegistry {
        LayerRegistry::build(&[])
    }

    #[test]
    fn builtin_minimal_resolves_with_implicit_acceptance() {
        let (resolved, warnings) =
            resolve_profile_by_name("minimal", &TraceConfig::default(), &registry());
        assert!(warnings.is_empty());
        let p = resolved.unwrap();
        assert!(p.builtin);
        assert!(p.implicit_acceptance);
        assert_eq!(p.layers, vec!["requirement", "acceptance"]);
    }

    #[test]
    fn builtin_standard_resolves_without_implicit_acceptance() {
        let (resolved, _) =
            resolve_profile_by_name("standard", &TraceConfig::default(), &registry());
        let p = resolved.unwrap();
        assert!(!p.implicit_acceptance);
        assert_eq!(
            p.layers,
            vec!["requirement", "basic_spec", "acceptance", "system_test"]
        );
    }

    #[test]
    fn custom_profile_extends_standard_and_overrides_layers() {
        use crate::storage::config::TraceProfileConfig;
        let mut cfg = TraceConfig::default();
        cfg.profiles.insert(
            "web".to_string(),
            TraceProfileConfig {
                extends: Some("standard".to_string()),
                layers: vec!["requirement".to_string(), "acceptance".to_string()],
                implicit_acceptance: Some(false),
                ..Default::default()
            },
        );
        let (resolved, warnings) = resolve_profile_by_name("web", &cfg, &registry());
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        let p = resolved.unwrap();
        assert!(!p.builtin);
        assert_eq!(p.layers, vec!["requirement", "acceptance"]);
    }

    #[test]
    fn custom_profile_inherits_layers_from_extends_when_unset() {
        use crate::storage::config::TraceProfileConfig;
        let mut cfg = TraceConfig::default();
        cfg.profiles.insert(
            "web".to_string(),
            TraceProfileConfig {
                extends: Some("standard".to_string()),
                layers: Vec::new(),
                implicit_acceptance: None,
                ..Default::default()
            },
        );
        let (resolved, _) = resolve_profile_by_name("web", &cfg, &registry());
        let p = resolved.unwrap();
        assert_eq!(
            p.layers,
            vec!["requirement", "basic_spec", "acceptance", "system_test"]
        );
        assert!(!p.implicit_acceptance);
    }

    #[test]
    fn extends_cycle_is_reported_and_does_not_hang() {
        use crate::storage::config::TraceProfileConfig;
        let mut cfg = TraceConfig::default();
        cfg.profiles.insert(
            "a".to_string(),
            TraceProfileConfig {
                extends: Some("b".to_string()),
                layers: Vec::new(),
                implicit_acceptance: None,
                ..Default::default()
            },
        );
        cfg.profiles.insert(
            "b".to_string(),
            TraceProfileConfig {
                extends: Some("a".to_string()),
                layers: Vec::new(),
                implicit_acceptance: None,
                ..Default::default()
            },
        );
        let (resolved, warnings) = resolve_profile_by_name("a", &cfg, &registry());
        assert!(resolved.is_none());
        assert!(warnings.iter().any(|w| w.contains("cycle")));
    }

    #[test]
    fn unknown_profile_name_reports_warning_and_no_resolution() {
        let (resolved, warnings) =
            resolve_profile_by_name("nonexistent", &TraceConfig::default(), &registry());
        assert!(resolved.is_none());
        assert!(warnings.iter().any(|w| w.contains("unknown profile")));
    }

    #[test]
    fn unknown_layer_in_profile_is_dropped_with_warning() {
        use crate::storage::config::TraceProfileConfig;
        let mut cfg = TraceConfig::default();
        cfg.profiles.insert(
            "web".to_string(),
            TraceProfileConfig {
                extends: None,
                layers: vec!["requirement".to_string(), "made_up_layer".to_string()],
                implicit_acceptance: Some(false),
                ..Default::default()
            },
        );
        let (resolved, warnings) = resolve_profile_by_name("web", &cfg, &registry());
        let p = resolved.unwrap();
        assert_eq!(p.layers, vec!["requirement"]);
        assert!(warnings.iter().any(|w| w.contains("made_up_layer")));
    }

    #[test]
    fn project_default_profile_is_none_when_unset() {
        let (resolved, warnings) = resolve_project_profile(&TraceConfig::default(), &registry());
        assert!(resolved.is_none());
        assert!(warnings.is_empty());
    }

    #[test]
    fn project_default_profile_resolves_configured_name() {
        let cfg = TraceConfig {
            profile: Some("full".to_string()),
            ..TraceConfig::default()
        };
        let (resolved, _) = resolve_project_profile(&cfg, &registry());
        assert_eq!(resolved.unwrap().layers.len(), 6);
    }

    /// §2.1: "`layers` とプロファイルを両方書いた場合は `layers` を使い
    /// warning を出す" — the engine already prefers `layers` (tier-1 over
    /// tier-2 in `resolve_in_use_layers`), but the warning itself must come
    /// from here so it reaches `trace_report`/`trace_slice`'s `warnings` via
    /// `config_warnings`.
    #[test]
    fn project_default_profile_warns_when_layers_is_also_set() {
        let cfg = TraceConfig {
            profile: Some("full".to_string()),
            layers: vec!["requirement".to_string(), "acceptance".to_string()],
            ..TraceConfig::default()
        };
        let (resolved, warnings) = resolve_project_profile(&cfg, &registry());
        assert!(resolved.is_some(), "profile itself must still resolve");
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("layers") && w.contains("profile") && w.contains("full")),
            "expected a layers-and-profile-both-set warning, got: {warnings:?}"
        );
    }

    #[test]
    fn project_default_profile_with_only_layers_set_has_no_priority_warning() {
        let cfg = TraceConfig {
            profile: None,
            layers: vec!["requirement".to_string()],
            ..TraceConfig::default()
        };
        let (resolved, warnings) = resolve_project_profile(&cfg, &registry());
        assert!(resolved.is_none());
        assert!(warnings.is_empty());
    }

    /// NFR-006 (wiki/270 §2.7): a profile's own `max_generated_per_call`
    /// resolves straight through, with no `extends` chain involved.
    #[test]
    fn resolve_profile_by_name_returns_own_max_generated_per_call() {
        use crate::storage::config::TraceProfileConfig;
        let mut cfg = TraceConfig::default();
        cfg.profiles.insert(
            "test".to_string(),
            TraceProfileConfig {
                layers: vec!["requirement".to_string()],
                max_generated_per_call: Some(2),
                ..Default::default()
            },
        );
        let (resolved, warnings) = resolve_profile_by_name("test", &cfg, &registry());
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(resolved.unwrap().max_generated_per_call, Some(2));
    }

    /// A profile without its own `max_generated_per_call` inherits its
    /// `extends` ancestor's value — same "first unset value wins from the
    /// nearest ancestor that sets it" rule `layers`/`implicit_acceptance`
    /// already follow.
    #[test]
    fn resolve_profile_by_name_inherits_max_generated_per_call_from_extends() {
        use crate::storage::config::TraceProfileConfig;
        let mut cfg = TraceConfig::default();
        cfg.profiles.insert(
            "base".to_string(),
            TraceProfileConfig {
                layers: vec!["requirement".to_string()],
                max_generated_per_call: Some(5),
                ..Default::default()
            },
        );
        cfg.profiles.insert(
            "derived".to_string(),
            TraceProfileConfig {
                extends: Some("base".to_string()),
                ..Default::default()
            },
        );
        let (resolved, _) = resolve_profile_by_name("derived", &cfg, &registry());
        assert_eq!(resolved.unwrap().max_generated_per_call, Some(5));
    }

    /// A built-in profile (no `[trace.profiles.<name>]` entry at all) has no
    /// cap — NFR-006 is opt-in, project-defined profiles only.
    #[test]
    fn resolve_profile_by_name_builtin_has_no_max_generated_per_call() {
        let (resolved, _) =
            resolve_profile_by_name("standard", &TraceConfig::default(), &registry());
        assert_eq!(resolved.unwrap().max_generated_per_call, None);
    }

    /// A profile's own value wins over an ancestor's, mirroring
    /// `custom_profile_extends_standard_and_overrides_layers`'s own-value-wins
    /// assertion for `layers`.
    #[test]
    fn resolve_profile_by_name_own_max_generated_per_call_overrides_extends() {
        use crate::storage::config::TraceProfileConfig;
        let mut cfg = TraceConfig::default();
        cfg.profiles.insert(
            "base".to_string(),
            TraceProfileConfig {
                layers: vec!["requirement".to_string()],
                max_generated_per_call: Some(5),
                ..Default::default()
            },
        );
        cfg.profiles.insert(
            "derived".to_string(),
            TraceProfileConfig {
                extends: Some("base".to_string()),
                max_generated_per_call: Some(2),
                ..Default::default()
            },
        );
        let (resolved, _) = resolve_profile_by_name("derived", &cfg, &registry());
        assert_eq!(resolved.unwrap().max_generated_per_call, Some(2));
    }
}
