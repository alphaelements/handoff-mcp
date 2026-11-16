//! Config presets (SPEC-ONB-010): a named set of `config.toml` values applied
//! in one step by `handoff_init(preset=...)` and
//! `handoff_update_config(preset=...)`.
//!
//! A preset is applied to the config *text*, key by key, so everything else in
//! the file (comments, layout, sections this version does not model) stays as
//! it is. Applying computes a [`PresetPlan`] first and writes nothing: the
//! caller decides whether the plan may be written, which is how an existing
//! project gets to see what would be overwritten before it happens.

use anyhow::Result;
use serde::Serialize;
use serde_json::Value as JsonValue;
use toml_edit::{DocumentMut, Item, TableLike};

use super::config::{ensure_table_like, Config};

/// One value a preset sets.
#[derive(Debug, Clone, Copy)]
enum PresetValue {
    Text(&'static str),
    Flag(bool),
}

impl PresetValue {
    fn to_json(self) -> JsonValue {
        match self {
            Self::Text(text) => JsonValue::from(text),
            Self::Flag(flag) => JsonValue::from(flag),
        }
    }

    fn to_toml(self) -> toml_edit::Value {
        match self {
            Self::Text(text) => toml_edit::Value::from(text),
            Self::Flag(flag) => toml_edit::Value::from(flag),
        }
    }
}

/// `[trace] done_guard`: the one preset key with two spellings. It is either
/// the bare mode string, or a table whose `mode` key holds the mode next to
/// the `check_*` switches.
const DONE_GUARD_KEY: &str = "trace.done_guard";
const DONE_GUARD_MODE: &str = "mode";

/// The `beginner` preset: the smallest setup in which one person can take an
/// application from planning to release with handoff's guidance switched on.
const BEGINNER: &[(&str, PresetValue)] = &[
    ("trace.profile", PresetValue::Text("minimal")),
    (DONE_GUARD_KEY, PresetValue::Text("warn")),
    ("session_loop.profile", PresetValue::Text("standard")),
    ("onboarding.preset", PresetValue::Text("beginner")),
    ("onboarding.preflight", PresetValue::Text("run")),
    ("onboarding.next_step", PresetValue::Flag(true)),
    ("onboarding.smoke_check", PresetValue::Flag(true)),
    ("onboarding.release_guard", PresetValue::Text("warn")),
    ("onboarding.security_min_nfr", PresetValue::Flag(true)),
];

/// A change a preset makes to one config key.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PresetChange {
    /// Dotted config key, e.g. `trace.profile`.
    pub key: String,
    /// The value the project had set, `null` when the key had none.
    pub old: Option<JsonValue>,
    pub new: JsonValue,
}

/// What applying a preset to a config would do.
#[derive(Debug, Clone, PartialEq)]
pub struct PresetPlan {
    /// Canonical preset name.
    pub preset: &'static str,
    /// Keys whose value changes, in preset order. Empty when the config
    /// already holds every preset value.
    pub changes: Vec<PresetChange>,
    /// The config text with the preset applied; equal to the input when
    /// `changes` is empty.
    pub rendered: String,
}

impl PresetPlan {
    /// The changes that replace a value the project had set itself. These are
    /// what an existing project must confirm before the preset is written.
    pub fn overwrites(&self) -> Vec<&PresetChange> {
        self.changes
            .iter()
            .filter(|change| change.old.is_some())
            .collect()
    }
}

/// A named set of config values.
struct Preset {
    name: &'static str,
    entries: &'static [(&'static str, PresetValue)],
}

/// Every preset. `ONBOARDING_PRESETS` (the names `[onboarding] preset` and the
/// tool schemas accept) lists the same names; a test keeps the two in step.
const PRESETS: &[Preset] = &[Preset {
    name: "beginner",
    entries: BEGINNER,
}];

fn find_preset(name: &str) -> Result<&'static Preset> {
    PRESETS
        .iter()
        .find(|preset| preset.name == name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unknown preset '{name}': expected one of {}",
                PRESETS
                    .iter()
                    .map(|preset| preset.name)
                    .collect::<Vec<_>>()
                    .join("/")
            )
        })
}

/// The canonical name of preset `name`, or an error that lists the presets
/// this version accepts.
pub fn resolve_preset_name(name: &str) -> Result<&'static str> {
    find_preset(name).map(|preset| preset.name)
}

/// Computes what applying preset `name` to `config_text` would change.
/// Nothing is written.
pub fn plan_preset(config_text: &str, name: &str) -> Result<PresetPlan> {
    let preset = find_preset(name)?;
    // The preset is added to a config that is already readable; applying it to
    // a broken file would not repair it and could hide what is wrong.
    let not_valid = |cause: &dyn std::fmt::Display| {
        // One line, and in the message itself: tool errors show only the
        // outermost message.
        let cause = cause.to_string();
        let cause = cause.split_whitespace().collect::<Vec<_>>().join(" ");
        anyhow::anyhow!("config.toml is not valid, so the preset was not applied: {cause}")
    };
    toml::from_str::<Config>(config_text).map_err(|e| not_valid(&e))?;
    let mut doc: DocumentMut = config_text.parse().map_err(|e| not_valid(&e))?;

    let mut changes = Vec::new();
    for (key, value) in preset.entries {
        let path = entry_path(&doc, key);
        let old = current_value(&doc, &path);
        let new = value.to_json();
        if old.as_ref() == Some(&new) {
            continue;
        }
        set_value(&mut doc, &path, value.to_toml())?;
        changes.push(PresetChange {
            key: key.to_string(),
            // A blank string is what clearing a key leaves behind, not a
            // value the project chose.
            old: old.filter(|old| old.as_str() != Some("")),
            new,
        });
    }

    // An unedited document renders back to the text it was parsed from.
    let rendered = doc.to_string();
    toml::from_str::<Config>(&rendered).map_err(|e| {
        anyhow::anyhow!(
            "applying the preset would make config.toml invalid, so nothing was written: {e}"
        )
    })?;

    Ok(PresetPlan {
        preset: preset.name,
        changes,
        rendered,
    })
}

/// Where a preset key lives in `doc`: its dotted segments, with
/// `trace.done_guard` pointed at the table's `mode` when the project wrote the
/// table form.
fn entry_path(doc: &DocumentMut, key: &'static str) -> Vec<&'static str> {
    let mut path: Vec<&'static str> = key.split('.').collect();
    if key == DONE_GUARD_KEY && item_at(doc, &path).is_some_and(Item::is_table_like) {
        path.push(DONE_GUARD_MODE);
    }
    path
}

fn item_at<'a>(doc: &'a DocumentMut, path: &[&str]) -> Option<&'a Item> {
    let (first, rest) = path.split_first()?;
    rest.iter()
        .try_fold(doc.get(first)?, |item, segment| item.get(segment))
}

/// The value stored at `path` as JSON, `None` when there is none.
fn current_value(doc: &DocumentMut, path: &[&str]) -> Option<JsonValue> {
    let value = item_at(doc, path)?.as_value()?;
    Some(match value {
        toml_edit::Value::String(text) => JsonValue::from(text.value().as_str()),
        toml_edit::Value::Boolean(flag) => JsonValue::from(*flag.value()),
        toml_edit::Value::Integer(number) => JsonValue::from(*number.value()),
        other => JsonValue::from(other.to_string().trim()),
    })
}

fn set_value(doc: &mut DocumentMut, path: &[&str], value: toml_edit::Value) -> Result<()> {
    let Some((leaf, parents)) = path.split_last() else {
        anyhow::bail!("empty preset key");
    };
    let mut table: &mut dyn TableLike = doc.as_table_mut();
    for parent in parents {
        table = ensure_table_like(table, parent)?;
    }
    table.insert(leaf, Item::Value(value));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::config::{render_config, Config, ONBOARDING_PRESETS};
    use serde_json::json;

    fn fresh() -> String {
        render_config(&Config::new("demo", "")).unwrap()
    }

    fn parse(text: &str) -> Config {
        toml::from_str(text).unwrap()
    }

    fn change<'a>(plan: &'a PresetPlan, key: &str) -> &'a PresetChange {
        plan.changes
            .iter()
            .find(|c| c.key == key)
            .unwrap_or_else(|| panic!("no change for {key}: {:?}", plan.changes))
    }

    const BEGINNER_KEYS: [&str; 9] = [
        "trace.profile",
        "trace.done_guard",
        "session_loop.profile",
        "onboarding.preset",
        "onboarding.preflight",
        "onboarding.next_step",
        "onboarding.smoke_check",
        "onboarding.release_guard",
        "onboarding.security_min_nfr",
    ];

    fn assert_beginner_values(config: &Config) {
        assert_eq!(config.trace.profile.as_deref(), Some("minimal"));
        assert_eq!(config.trace.done_guard.mode, "warn");
        assert_eq!(config.session_loop.profile.as_deref(), Some("standard"));
        assert_eq!(config.onboarding.preset(), Some("beginner"));
        assert_eq!(config.onboarding.preflight(), "run");
        assert!(config.onboarding.next_step());
        assert!(config.onboarding.smoke_check());
        assert_eq!(config.onboarding.release_guard(), "warn");
        assert!(config.onboarding.security_min_nfr());
    }

    #[test]
    fn every_accepted_preset_name_is_a_preset_with_valid_values() {
        assert_eq!(
            PRESETS.iter().map(|preset| preset.name).collect::<Vec<_>>(),
            ONBOARDING_PRESETS,
            "the names the config and the tool schemas accept are the presets that exist"
        );
        for name in ONBOARDING_PRESETS {
            assert!(!find_preset(name).unwrap().entries.is_empty(), "{name}");
            let plan = plan_preset(&fresh(), name).unwrap();
            let config = parse(&plan.rendered);
            assert_eq!(
                config.onboarding.preset(),
                Some(*name),
                "a preset records its own name"
            );
            // A preset is written without going through the per-key checks of
            // `handoff_update_config`, so its own values must pass them.
            assert_eq!(config.onboarding.validate(), Ok(()), "{name}");
            assert_eq!(config.session_loop.validate(), Ok(()), "{name}");
            let profile = config.trace.profile.as_deref().unwrap_or("");
            assert!(
                profile.is_empty()
                    || crate::trace::profile::BUILTIN_PROFILE_NAMES.contains(&profile),
                "{name}: trace.profile '{profile}' is not a built-in profile"
            );
            assert!(
                crate::storage::config::DONE_GUARD_MODES
                    .contains(&config.trace.done_guard.mode.as_str()),
                "{name}"
            );
        }
    }

    #[test]
    fn unknown_preset_names_are_rejected_with_the_accepted_names() {
        for bad in ["unknown", "", "Beginner", "beginner "] {
            let err = resolve_preset_name(bad).unwrap_err().to_string();
            assert!(err.contains(&format!("'{bad}'")), "{bad}: {err}");
            assert!(err.contains("beginner"), "{bad}: {err}");
            assert!(plan_preset(&fresh(), bad).is_err(), "{bad}");
        }
        assert_eq!(resolve_preset_name("beginner").unwrap(), "beginner");
    }

    #[test]
    fn beginner_sets_every_key_on_a_fresh_config_without_overwriting_anything() {
        let before = fresh();
        let plan = plan_preset(&before, "beginner").unwrap();

        assert_eq!(plan.preset, "beginner");
        assert_eq!(
            plan.changes
                .iter()
                .map(|c| c.key.as_str())
                .collect::<Vec<_>>(),
            BEGINNER_KEYS
        );
        assert!(plan.changes.iter().all(|c| c.old.is_none()));
        assert!(plan.overwrites().is_empty());
        assert_eq!(change(&plan, "trace.profile").new, json!("minimal"));
        assert_eq!(change(&plan, "onboarding.next_step").new, json!(true));

        assert_beginner_values(&parse(&plan.rendered));
        // The written text carries each key explicitly, so `handoff_get_config`
        // (which serves the file as is) shows them.
        let raw: toml::Value = toml::from_str(&plan.rendered).unwrap();
        assert_eq!(raw["trace"]["done_guard"].as_str(), Some("warn"));
        assert_eq!(raw["onboarding"]["preflight"].as_str(), Some("run"));
        assert_eq!(raw["onboarding"]["security_min_nfr"].as_bool(), Some(true));
    }

    #[test]
    fn everything_outside_the_preset_keys_is_left_as_written() {
        let before = "# my notes\n[project]\nname = \"demo\"  # the name\n\n[future_section]\nkey = 1\n\n[trace]\nauto_layer = true\n";
        let plan = plan_preset(before, "beginner").unwrap();
        assert!(plan.rendered.contains("# my notes\n"), "{}", plan.rendered);
        assert!(plan.rendered.contains("name = \"demo\"  # the name\n"));
        assert!(plan.rendered.contains("[future_section]\nkey = 1\n"));
        assert!(plan.rendered.contains("auto_layer = true\n"));
        assert_beginner_values(&parse(&plan.rendered));
    }

    #[test]
    fn a_differing_existing_value_is_reported_as_an_overwrite() {
        let before = format!("{}\n[trace]\nprofile = \"standard\"\n", fresh());
        let plan = plan_preset(&before, "beginner").unwrap();

        let overwrites = plan.overwrites();
        assert_eq!(overwrites.len(), 1, "{overwrites:?}");
        assert_eq!(
            overwrites[0],
            &PresetChange {
                key: "trace.profile".to_string(),
                old: Some(json!("standard")),
                new: json!("minimal"),
            }
        );
        assert_eq!(plan.changes.len(), BEGINNER_KEYS.len());
        assert_beginner_values(&parse(&plan.rendered));
    }

    #[test]
    fn overwrites_cover_session_loop_done_guard_and_onboarding_values() {
        let before = "[project]\nname = \"demo\"\n\n[trace]\ndone_guard = \"block\"\n\n[session_loop]\nprofile = \"elite\"\n\n[onboarding]\npreflight = \"check\"\nnext_step = false\n";
        let plan = plan_preset(before, "beginner").unwrap();
        let overwritten: Vec<(&str, &JsonValue)> = plan
            .overwrites()
            .iter()
            .map(|c| (c.key.as_str(), c.old.as_ref().unwrap()))
            .collect();
        assert_eq!(
            overwritten,
            vec![
                ("trace.done_guard", &json!("block")),
                ("session_loop.profile", &json!("elite")),
                ("onboarding.preflight", &json!("check")),
                ("onboarding.next_step", &json!(false)),
            ]
        );
        assert_beginner_values(&parse(&plan.rendered));
    }

    #[test]
    fn values_already_at_the_preset_are_not_changes() {
        let before = "[project]\nname = \"demo\"\n\n[trace]\nprofile = \"minimal\"\n\n[onboarding]\nnext_step = true\n";
        let plan = plan_preset(before, "beginner").unwrap();
        let keys: Vec<&str> = plan.changes.iter().map(|c| c.key.as_str()).collect();
        assert!(!keys.contains(&"trace.profile"), "{keys:?}");
        assert!(!keys.contains(&"onboarding.next_step"), "{keys:?}");
        assert_eq!(keys.len(), BEGINNER_KEYS.len() - 2);
        assert!(plan.overwrites().is_empty());
    }

    #[test]
    fn applying_twice_changes_nothing_the_second_time() {
        let once = plan_preset(&fresh(), "beginner").unwrap();
        let twice = plan_preset(&once.rendered, "beginner").unwrap();
        assert!(twice.changes.is_empty(), "{:?}", twice.changes);
        assert_eq!(twice.rendered, once.rendered);
    }

    /// An empty string is what clearing a key leaves behind; it is not a
    /// choice the project made.
    #[test]
    fn a_blank_existing_value_is_set_without_counting_as_an_overwrite() {
        let before = "[project]\nname = \"demo\"\n\n[trace]\nprofile = \"\"\n\n[session_loop]\nprofile = \"\"\n";
        let plan = plan_preset(before, "beginner").unwrap();
        assert!(plan.overwrites().is_empty(), "{:?}", plan.overwrites());
        assert_eq!(change(&plan, "trace.profile").old, None);
        assert_beginner_values(&parse(&plan.rendered));
    }

    /// `[trace.done_guard]` written as a table also carries the `check_*`
    /// switches; the preset changes its mode and nothing else.
    #[test]
    fn done_guard_table_keeps_its_switches() {
        let before = "[project]\nname = \"demo\"\n\n[trace.done_guard]\nmode = \"block\"\ncheck_code_drift = false\n";
        let plan = plan_preset(before, "beginner").unwrap();
        assert_eq!(
            change(&plan, "trace.done_guard").old,
            Some(json!("block")),
            "{:?}",
            plan.changes
        );
        let config = parse(&plan.rendered);
        assert_eq!(config.trace.done_guard.mode, "warn");
        assert!(!config.trace.done_guard.check_code_drift);
        assert!(config.trace.done_guard.check_suspects);

        // A table that leaves `mode` to its default is not an overwrite.
        let no_mode = "[project]\nname = \"demo\"\n\n[trace.done_guard]\ncheck_suspects = false\n";
        let plan = plan_preset(no_mode, "beginner").unwrap();
        assert_eq!(change(&plan, "trace.done_guard").old, None);
        let config = parse(&plan.rendered);
        assert_eq!(config.trace.done_guard.mode, "warn");
        assert!(!config.trace.done_guard.check_suspects);
    }

    #[test]
    fn sections_written_inline_are_extended_not_replaced() {
        let before = "onboarding = { smoke_timeout_sec = 90 }\ntrace = { auto_layer = true }\nsession_loop = { profile = \"elite\" }\n\n[project]\nname = \"demo\"\n";
        let plan = plan_preset(before, "beginner").unwrap();
        assert_eq!(
            plan.overwrites(),
            vec![&PresetChange {
                key: "session_loop.profile".to_string(),
                old: Some(json!("elite")),
                new: json!("standard"),
            }]
        );
        let raw: toml::Value = toml::from_str(&plan.rendered).unwrap();
        assert_eq!(
            raw["onboarding"]["smoke_timeout_sec"].as_integer(),
            Some(90)
        );
        assert_eq!(raw["trace"]["auto_layer"].as_bool(), Some(true));
        assert_beginner_values(&parse(&plan.rendered));
    }

    #[test]
    fn a_config_that_is_not_valid_is_an_error() {
        for broken in [
            "[project\nname = ",
            "[settings]\nmulti_session = true\n",
            "trace = \"oops\"\n[project]\nname = \"demo\"\n",
        ] {
            let err = plan_preset(broken, "beginner").unwrap_err().to_string();
            assert!(err.contains("config.toml is not valid"), "{broken}: {err}");
            assert!(
                err.len() > "config.toml is not valid, so the preset was not applied: ".len(),
                "the message names the cause: {err}"
            );
            assert!(!err.contains('\n'), "{err}");
        }
    }

    #[test]
    fn changes_serialize_with_an_explicit_null_for_a_key_that_had_no_value() {
        let plan = plan_preset(&fresh(), "beginner").unwrap();
        let json = serde_json::to_value(change(&plan, "trace.profile")).unwrap();
        assert_eq!(
            json,
            json!({ "key": "trace.profile", "old": null, "new": "minimal" })
        );
    }
}
