//! V-model layer definitions (wiki/220-vmodel-integration-design.md §2.1,
//! M1 t360.4): the 6 built-in layers' `side`/`level`/`pair`/default ID
//! prefixes, plus resolution of a layer's *effective* ID-prefix allow-list
//! against project config (`[trace.id_prefixes]`, §2.1).
//!
//! This module deliberately stops at "static table + config merge" — body
//! heading parsing against these prefixes (t360.5), layer synchronization
//! and the write-guard on body-owned `SubItem` fields (t360.6), and
//! `TaskLink.role`-based link authority (t360.7) are later tasks' scope.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

/// Which side of the V a layer sits on (wiki/220 §2.1's table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerSide {
    /// Definition side: `requirement`, `basic_spec`, `detailed_spec`.
    Left,
    /// Verification side: `acceptance`, `system_test`, `unit_test`.
    Right,
}

impl LayerSide {
    pub fn as_str(self) -> &'static str {
        match self {
            LayerSide::Left => "left",
            LayerSide::Right => "right",
        }
    }
}

/// One built-in layer's fixed metadata.
#[derive(Debug, Clone, Copy)]
pub struct LayerDef {
    /// The `layer` id as written in frontmatter/body (`- layer: <id>`) and
    /// `[trace] layers`/`[trace.id_prefixes]` config keys.
    pub id: &'static str,
    /// Human-readable display name (wiki/220 §2.1's table, for future UI
    /// use — not otherwise consumed by M1).
    pub display_name: &'static str,
    pub side: LayerSide,
    /// 1-based depth within its side (`requirement`/`acceptance` = 1, down
    /// to `detailed_spec`/`unit_test` = 3).
    pub level: u8,
    /// The layer id this one verifies/is verified by on the opposite side.
    pub pair: &'static str,
    /// Default allowed ID prefixes for this layer's body headings (§2.2),
    /// before any `[trace.id_prefixes]` project additions.
    pub default_id_prefixes: &'static [&'static str],
}

/// The 6 built-in layers, in the fixed order wiki/220 §2.1's table lists
/// them (left side top-to-bottom, then right side top-to-bottom).
pub const BUILTIN_LAYERS: &[LayerDef] = &[
    LayerDef {
        id: "requirement",
        display_name: "要件",
        side: LayerSide::Left,
        level: 1,
        pair: "acceptance",
        default_id_prefixes: &["REQ", "FR", "NFR"],
    },
    LayerDef {
        id: "basic_spec",
        display_name: "基本仕様",
        side: LayerSide::Left,
        level: 2,
        pair: "system_test",
        default_id_prefixes: &["SPEC", "BS"],
    },
    LayerDef {
        id: "detailed_spec",
        display_name: "詳細仕様",
        side: LayerSide::Left,
        level: 3,
        pair: "unit_test",
        default_id_prefixes: &["DS"],
    },
    LayerDef {
        id: "acceptance",
        display_name: "受入検証",
        side: LayerSide::Right,
        level: 1,
        pair: "requirement",
        default_id_prefixes: &["AT", "AC"],
    },
    LayerDef {
        id: "system_test",
        display_name: "システムテスト",
        side: LayerSide::Right,
        level: 2,
        pair: "basic_spec",
        default_id_prefixes: &["ST"],
    },
    LayerDef {
        id: "unit_test",
        display_name: "ユニットテスト",
        side: LayerSide::Right,
        level: 3,
        pair: "detailed_spec",
        default_id_prefixes: &["UT"],
    },
];

/// Looks up a built-in layer by id. Returns `None` for a project-defined
/// (non-built-in) layer id — M1 only ships the 6 built-ins (wiki/220 §1.1:
/// arbitrary layer declaration is FR-201/M2 scope).
pub fn builtin_layer(id: &str) -> Option<&'static LayerDef> {
    BUILTIN_LAYERS.iter().find(|l| l.id == id)
}

/// The effective allowed ID-prefix set for `layer_id`: the built-in
/// defaults plus any project-configured additions from
/// `[trace.id_prefixes]` (wiki/220 §2.1: "既定に追加される" — config never
/// replaces the built-in list, only appends to it, and duplicates are not
/// added twice). A non-built-in `layer_id` with no config entry yields an
/// empty list.
pub fn id_prefixes_for(
    layer_id: &str,
    config_id_prefixes: &HashMap<String, Vec<String>>,
) -> Vec<String> {
    let mut prefixes: Vec<String> = builtin_layer(layer_id)
        .map(|l| {
            l.default_id_prefixes
                .iter()
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();
    if let Some(extra) = config_id_prefixes.get(layer_id) {
        for p in extra {
            if !prefixes.contains(p) {
                prefixes.push(p.clone());
            }
        }
    }
    prefixes
}

/// One `[[trace.layer]]` project-defined layer declaration (wiki/260-vmodel-m2-design.md
/// §2.1, M2-01, FR-101 residual). Deserialized as authored from `config.toml`
/// — validation (duplicate id, non-reciprocal `pair`, etc.) happens in
/// [`LayerRegistry::build`], not here, so a malformed entry can still be
/// reported as a per-layer warning instead of failing the whole config parse.
/// `side`/`level`/`pair` are all `#[serde(default)]` (empty string / `0`) so
/// a `[[trace.layer]]` entry missing any of them (or an out-of-range
/// `level`) still deserializes — `LayerRegistry::build` is what rejects it,
/// not `toml::from_str` for the whole `config.toml` (§2.1: "その層を無効に
/// して warning（設定エラーで全体を止めない）"). `level` is `i64` rather
/// than `u8` for the same reason: an out-of-range value (negative, or > 255)
/// must reach `build`'s validation as a per-layer warning instead of making
/// `toml`'s own deserializer reject the file outright.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct CustomLayerConfig {
    /// §2.1 fail-safe: a missing `id` (like a missing `side`/`level`/`pair`)
    /// must not fail `toml::from_str` for the whole `config.toml` —
    /// `LayerRegistry::build` already warns and skips entries with an empty
    /// id, so validation stays there, not at the deserialize boundary.
    #[serde(default)]
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// `"left"` | `"right"`.
    #[serde(default)]
    pub side: String,
    #[serde(default)]
    pub level: i64,
    /// The layer id this one pairs with on the opposite side — must point
    /// back (§2.1: "pair が相互でない" is rejected).
    #[serde(default)]
    pub pair: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub id_prefixes: Vec<String>,
}

/// One registry entry — either a built-in layer or a validated
/// `[[trace.layer]]` custom one, in the same shape so callers never need to
/// branch on origin (only `builtin` is exposed, for display purposes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredLayer {
    pub id: String,
    pub display_name: String,
    pub side: LayerSide,
    pub level: u8,
    pub pair: String,
    pub default_id_prefixes: Vec<String>,
    pub builtin: bool,
}

impl From<&LayerDef> for RegisteredLayer {
    fn from(l: &LayerDef) -> Self {
        RegisteredLayer {
            id: l.id.to_string(),
            display_name: l.display_name.to_string(),
            side: l.side,
            level: l.level,
            pair: l.pair.to_string(),
            default_id_prefixes: l
                .default_id_prefixes
                .iter()
                .map(|s| s.to_string())
                .collect(),
            builtin: true,
        }
    }
}

/// The project's full set of layers: the 6 built-ins plus any valid
/// `[[trace.layer]]` custom declarations (wiki/260 §2.1, M2-01). Every M1
/// direct reference to `BUILTIN_LAYERS`/`builtin_layer` outside this module
/// goes through a `LayerRegistry` instead, so a project-defined layer
/// participates in parsing/sync/derivation exactly like a built-in one.
#[derive(Debug, Clone, Default)]
pub struct LayerRegistry {
    layers: Vec<RegisteredLayer>,
    /// Non-fatal validation issues (§2.1: "その層を無効にして warning") —
    /// the offending declaration is dropped from `layers`, not the whole
    /// config.
    pub warnings: Vec<String>,
}

impl LayerRegistry {
    /// Builds a registry from the built-in 6 layers plus `custom`
    /// (`[trace] layer` = `[[trace.layer]]` entries), validating each custom
    /// declaration against §2.1's rules 1-4:
    /// - duplicate id (against an already-accepted layer, built-in or
    ///   custom) — disabled, warning.
    /// - redefining a built-in id — disabled, warning.
    /// - unknown `side` (not `"left"`/`"right"`) — disabled, warning.
    /// - `level < 1` — disabled, warning.
    /// - `pair` not reciprocal (the paired layer's own `pair` doesn't point
    ///   back, or is missing) or same-side — disabled, warning.
    /// - an `id_prefixes` entry colliding with another (accepted) layer's
    ///   default prefixes — disabled, warning.
    pub fn build(custom: &[CustomLayerConfig]) -> Self {
        let mut layers: Vec<RegisteredLayer> =
            BUILTIN_LAYERS.iter().map(RegisteredLayer::from).collect();
        let mut warnings = Vec::new();

        // Pass 1: structural validation (id/side/level) + a combined
        // candidate map (built-ins + every structurally-valid custom entry,
        // even ones that will later fail the reciprocity/prefix checks) so
        // two new custom layers can validly pair with *each other*
        // regardless of declaration order.
        let mut candidates: Vec<RegisteredLayer> = layers.clone();
        let mut structurally_valid: Vec<&CustomLayerConfig> = Vec::new();
        let mut seen_ids: HashSet<String> = layers.iter().map(|l| l.id.clone()).collect();

        for c in custom {
            if c.id.is_empty() {
                warnings.push("trace.layer: skipped an entry with an empty id".to_string());
                continue;
            }
            if seen_ids.contains(&c.id) {
                warnings.push(format!(
                    "trace.layer '{}': duplicate layer id, disabled",
                    c.id
                ));
                continue;
            }
            let side = match c.side.as_str() {
                "left" => LayerSide::Left,
                "right" => LayerSide::Right,
                other => {
                    warnings.push(format!(
                        "trace.layer '{}': invalid side \"{other}\" (must be \"left\" or \"right\"), disabled",
                        c.id
                    ));
                    continue;
                }
            };
            if c.level < 1 {
                warnings.push(format!(
                    "trace.layer '{}': level must be >= 1, disabled",
                    c.id
                ));
                continue;
            }
            let Ok(level) = u8::try_from(c.level) else {
                warnings.push(format!(
                    "trace.layer '{}': level {} is out of range (must be 1-255), disabled",
                    c.id, c.level
                ));
                continue;
            };
            seen_ids.insert(c.id.clone());
            structurally_valid.push(c);
            candidates.push(RegisteredLayer {
                id: c.id.clone(),
                display_name: c.display_name.clone().unwrap_or_else(|| c.id.clone()),
                side,
                level,
                pair: c.pair.clone(),
                default_id_prefixes: c.id_prefixes.clone(),
                builtin: false,
            });
        }

        // Pass 2: reciprocity + prefix-collision, against the full
        // candidate set built above.
        for c in structurally_valid {
            let this = candidates
                .iter()
                .find(|l| l.id == c.id)
                .expect("just inserted");
            let Some(paired) = candidates.iter().find(|l| l.id == this.pair) else {
                warnings.push(format!(
                    "trace.layer '{}': pair \"{}\" is not a known layer, disabled",
                    c.id, c.pair
                ));
                continue;
            };
            if paired.pair != this.id {
                warnings.push(format!(
                    "trace.layer '{}': pair \"{}\" does not point back (not reciprocal), disabled",
                    c.id, c.pair
                ));
                continue;
            }
            if paired.side == this.side {
                warnings.push(format!(
                    "trace.layer '{}': pair \"{}\" must be on the opposite side, disabled",
                    c.id, c.pair
                ));
                continue;
            }
            let collides = layers.iter().any(|existing| {
                existing.id != this.id
                    && this
                        .default_id_prefixes
                        .iter()
                        .any(|p| existing.default_id_prefixes.contains(p))
            });
            if collides {
                warnings.push(format!(
                    "trace.layer '{}': id_prefixes collide with another layer's prefixes, disabled",
                    c.id
                ));
                continue;
            }
            layers.push(this.clone());
        }

        // Pass 3: fixpoint reciprocity cleanup. Pass 2 above checks each
        // custom entry's pair against the *candidate* set, which still
        // contains layers pass 2 itself goes on to reject later in the same
        // loop (e.g. a prefix collision) — so a layer whose only partner got
        // rejected after it was already accepted would otherwise survive
        // with a `pair` pointing at nothing in the final registry. Repeatedly
        // drop any non-built-in layer whose pair is missing or no longer
        // reciprocal in `layers`, until nothing more changes, so the
        // registry's own invariant ("every accepted layer's pair is present
        // and points back") always holds (§2.1).
        loop {
            let mut to_remove: Vec<(String, String)> = Vec::new();
            for l in &layers {
                if l.builtin {
                    continue; // built-ins are mutually reciprocal by construction.
                }
                let reciprocal = layers
                    .iter()
                    .find(|p| p.id == l.pair)
                    .is_some_and(|p| p.pair == l.id);
                if !reciprocal {
                    to_remove.push((l.id.clone(), l.pair.clone()));
                }
            }
            if to_remove.is_empty() {
                break;
            }
            for (id, pair) in &to_remove {
                warnings.push(format!(
                    "trace.layer '{id}': pair \"{pair}\" was disabled, so this layer is disabled too"
                ));
            }
            let removed_ids: HashSet<&str> = to_remove.iter().map(|(id, _)| id.as_str()).collect();
            layers.retain(|l| !removed_ids.contains(l.id.as_str()));
        }

        LayerRegistry { layers, warnings }
    }

    pub fn get(&self, id: &str) -> Option<&RegisteredLayer> {
        self.layers.iter().find(|l| l.id == id)
    }

    pub fn all(&self) -> &[RegisteredLayer] {
        &self.layers
    }

    /// The effective allowed ID-prefix set for `layer_id` (registry
    /// defaults, built-in or custom, plus any project-configured
    /// `[trace.id_prefixes]` additions — same "append, never replace" rule
    /// as the free-function [`id_prefixes_for`]).
    pub fn id_prefixes_for(
        &self,
        layer_id: &str,
        config_id_prefixes: &HashMap<String, Vec<String>>,
    ) -> Vec<String> {
        let mut prefixes: Vec<String> = self
            .get(layer_id)
            .map(|l| l.default_id_prefixes.clone())
            .unwrap_or_default();
        if let Some(extra) = config_id_prefixes.get(layer_id) {
            for p in extra {
                if !prefixes.contains(p) {
                    prefixes.push(p.clone());
                }
            }
        }
        prefixes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_layers_table_matches_wiki_220_section_2_1() {
        let ids: Vec<&str> = BUILTIN_LAYERS.iter().map(|l| l.id).collect();
        assert_eq!(
            ids,
            vec![
                "requirement",
                "basic_spec",
                "detailed_spec",
                "acceptance",
                "system_test",
                "unit_test",
            ]
        );

        let requirement = builtin_layer("requirement").unwrap();
        assert_eq!(requirement.side, LayerSide::Left);
        assert_eq!(requirement.level, 1);
        assert_eq!(requirement.pair, "acceptance");
        assert_eq!(requirement.default_id_prefixes, &["REQ", "FR", "NFR"]);

        let unit_test = builtin_layer("unit_test").unwrap();
        assert_eq!(unit_test.side, LayerSide::Right);
        assert_eq!(unit_test.level, 3);
        assert_eq!(unit_test.pair, "detailed_spec");
        assert_eq!(unit_test.default_id_prefixes, &["UT"]);

        // `AT` stays first (handoff_trace_scaffold derives new ids from the
        // first prefix); `AC` is accepted too so `AC-<scope>-NNN` headings
        // are extracted without a `[trace.id_prefixes]` override.
        let acceptance = builtin_layer("acceptance").unwrap();
        assert_eq!(acceptance.default_id_prefixes, &["AT", "AC"]);
    }

    #[test]
    fn every_builtin_layers_pair_is_reciprocal() {
        // wiki/220 §2.1's table: each layer's `pair` must itself point back
        // at the original layer, forming 3 left<->right couples.
        for layer in BUILTIN_LAYERS {
            let paired = builtin_layer(layer.pair).unwrap_or_else(|| {
                panic!(
                    "{}'s pair '{}' is not a built-in layer",
                    layer.id, layer.pair
                )
            });
            assert_eq!(
                paired.pair, layer.id,
                "{} <-> {} pairing must be reciprocal",
                layer.id, layer.pair
            );
            assert_ne!(
                paired.side, layer.side,
                "paired layers must be on opposite sides"
            );
        }
    }

    #[test]
    fn builtin_layer_returns_none_for_unknown_id() {
        assert!(builtin_layer("nonexistent").is_none());
    }

    #[test]
    fn id_prefixes_for_builtin_layer_with_no_config_returns_defaults_only() {
        let prefixes = id_prefixes_for("requirement", &HashMap::new());
        assert_eq!(prefixes, vec!["REQ", "FR", "NFR"]);
    }

    #[test]
    fn id_prefixes_for_appends_config_additions_without_duplicating() {
        let mut config = HashMap::new();
        config.insert(
            "requirement".to_string(),
            vec!["UC".to_string(), "REQ".to_string()],
        );
        let prefixes = id_prefixes_for("requirement", &config);
        // "REQ" already a default, must not be duplicated; "UC" is appended.
        assert_eq!(prefixes, vec!["REQ", "FR", "NFR", "UC"]);
    }

    #[test]
    fn id_prefixes_for_unknown_layer_with_config_entry_returns_only_config() {
        let mut config = HashMap::new();
        config.insert("custom_layer".to_string(), vec!["CUS".to_string()]);
        let prefixes = id_prefixes_for("custom_layer", &config);
        assert_eq!(prefixes, vec!["CUS"]);
    }

    // -- LayerRegistry (wiki/260 §2.1, M2-01) --

    fn ux_spec() -> CustomLayerConfig {
        CustomLayerConfig {
            id: "ux_spec".to_string(),
            display_name: Some("UX 仕様".to_string()),
            side: "left".to_string(),
            level: 2,
            pair: "usability_test".to_string(),
            id_prefixes: vec!["UX".to_string()],
        }
    }

    fn usability_test() -> CustomLayerConfig {
        CustomLayerConfig {
            id: "usability_test".to_string(),
            display_name: None,
            side: "right".to_string(),
            level: 2,
            pair: "ux_spec".to_string(),
            id_prefixes: vec!["UT2".to_string()],
        }
    }

    #[test]
    fn registry_with_no_custom_layers_matches_builtin_only() {
        let registry = LayerRegistry::build(&[]);
        assert!(registry.warnings.is_empty());
        assert_eq!(registry.all().len(), BUILTIN_LAYERS.len());
        assert!(registry.get("requirement").unwrap().builtin);
    }

    #[test]
    fn registry_accepts_a_valid_reciprocal_custom_layer_pair() {
        let registry = LayerRegistry::build(&[ux_spec(), usability_test()]);
        assert!(
            registry.warnings.is_empty(),
            "unexpected warnings: {:?}",
            registry.warnings
        );
        let ux = registry.get("ux_spec").unwrap();
        assert!(!ux.builtin);
        assert_eq!(ux.side, LayerSide::Left);
        assert_eq!(ux.level, 2);
        assert_eq!(ux.pair, "usability_test");
        assert_eq!(ux.display_name, "UX 仕様");
        let ut = registry.get("usability_test").unwrap();
        assert_eq!(
            ut.display_name, "usability_test",
            "defaults to id when unset"
        );
    }

    #[test]
    fn registry_rejects_duplicate_id_against_builtin() {
        let dup = CustomLayerConfig {
            id: "requirement".to_string(),
            side: "left".to_string(),
            level: 1,
            pair: "acceptance".to_string(),
            ..Default::default()
        };
        let registry = LayerRegistry::build(&[dup]);
        assert!(registry.warnings.iter().any(|w| w.contains("duplicate")));
        // The built-in definition wins; the custom one is dropped entirely.
        assert_eq!(registry.all().len(), BUILTIN_LAYERS.len());
    }

    #[test]
    fn registry_rejects_non_reciprocal_pair() {
        let bad = CustomLayerConfig {
            id: "ux_spec".to_string(),
            side: "left".to_string(),
            level: 2,
            pair: "acceptance".to_string(), // acceptance.pair == "requirement", not "ux_spec"
            ..Default::default()
        };
        let registry = LayerRegistry::build(&[bad]);
        assert!(registry.get("ux_spec").is_none());
        assert!(registry
            .warnings
            .iter()
            .any(|w| w.contains("not reciprocal")));
    }

    #[test]
    fn registry_rejects_same_side_pair() {
        // Two new custom layers that mutually reference each other (so the
        // reciprocity check alone would pass) but sit on the same side.
        let a_left = CustomLayerConfig {
            id: "a_left".to_string(),
            side: "left".to_string(),
            level: 2,
            pair: "b_left".to_string(),
            ..Default::default()
        };
        let b_left = CustomLayerConfig {
            id: "b_left".to_string(),
            side: "left".to_string(),
            level: 3,
            pair: "a_left".to_string(),
            ..Default::default()
        };
        let registry = LayerRegistry::build(&[a_left, b_left]);
        assert!(registry.get("a_left").is_none());
        assert!(registry.get("b_left").is_none());
        assert!(registry
            .warnings
            .iter()
            .any(|w| w.contains("opposite side")));
    }

    #[test]
    fn registry_rejects_level_below_one() {
        let bad = CustomLayerConfig {
            id: "ux_spec".to_string(),
            side: "left".to_string(),
            level: 0,
            pair: "usability_test".to_string(),
            ..Default::default()
        };
        let registry = LayerRegistry::build(&[bad]);
        assert!(registry.get("ux_spec").is_none());
        assert!(registry.warnings.iter().any(|w| w.contains("level")));
    }

    #[test]
    fn registry_rejects_invalid_side() {
        let bad = CustomLayerConfig {
            id: "ux_spec".to_string(),
            side: "up".to_string(),
            level: 2,
            pair: "usability_test".to_string(),
            ..Default::default()
        };
        let registry = LayerRegistry::build(&[bad]);
        assert!(registry.get("ux_spec").is_none());
        assert!(registry.warnings.iter().any(|w| w.contains("invalid side")));
    }

    #[test]
    fn registry_rejects_prefix_collision_with_another_layer() {
        let mut clashing = ux_spec();
        clashing.id_prefixes = vec!["REQ".to_string()]; // already a requirement prefix
        let registry = LayerRegistry::build(&[clashing, usability_test()]);
        assert!(registry.get("ux_spec").is_none());
        assert!(registry.warnings.iter().any(|w| w.contains("collide")));
        // usability_test's only pair is ux_spec, which got disabled above —
        // it must not survive as a non-reciprocal orphan (§2.1's registry
        // invariant: every accepted layer's pair is present and reciprocal).
        assert!(
            registry.get("usability_test").is_none(),
            "usability_test's partner was disabled, so it must be disabled too: {:?}",
            registry.all()
        );
        assert!(registry
            .warnings
            .iter()
            .any(|w| w.contains("usability_test") && w.contains("disabled")));
        for l in registry.all() {
            let partner = registry
                .get(&l.pair)
                .unwrap_or_else(|| panic!("{}'s pair '{}' must be in the registry", l.id, l.pair));
            assert_eq!(
                partner.pair, l.id,
                "{} <-> {} pairing must stay reciprocal in the final registry",
                l.id, l.pair
            );
        }
    }

    /// §2.1: a malformed `[[trace.layer]]` entry (missing required fields)
    /// must still deserialize cleanly — validation happens in
    /// `LayerRegistry::build`, not `toml::from_str`, so one bad custom layer
    /// never fails parsing the whole `config.toml` (and every other `[trace]`
    /// key along with it).
    #[test]
    fn custom_layer_config_with_missing_fields_deserializes_and_is_disabled_by_the_registry() {
        let parsed: CustomLayerConfig = toml::from_str("id = \"ux_spec\"").unwrap();
        assert_eq!(parsed.id, "ux_spec");
        assert_eq!(parsed.side, "");
        assert_eq!(parsed.level, 0);
        assert_eq!(parsed.pair, "");

        let registry = LayerRegistry::build(&[parsed]);
        assert!(registry.get("ux_spec").is_none());
        assert!(
            !registry.warnings.is_empty(),
            "a missing-field custom layer must produce a warning, not silently vanish"
        );
    }

    /// Round-3 companion to `config.rs::trace_layer_entry_missing_id_still_parses_whole_config`:
    /// the `#[serde(default)]` on `id` is only fail-safe because `build`
    /// skips the resulting empty-id entry with a warning — pin that half too.
    #[test]
    fn custom_layer_config_missing_id_is_skipped_with_a_warning() {
        let parsed: CustomLayerConfig =
            toml::from_str("side = \"left\"\nlevel = 2\npair = \"usability_test\"").unwrap();
        assert_eq!(parsed.id, "");

        let registry = LayerRegistry::build(&[parsed]);
        assert_eq!(registry.all().len(), BUILTIN_LAYERS.len());
        assert!(registry.get("").is_none());
        assert!(registry.warnings.iter().any(|w| w.contains("empty id")));
    }

    #[test]
    fn registry_rejects_level_out_of_u8_range() {
        let bad = CustomLayerConfig {
            id: "ux_spec".to_string(),
            side: "left".to_string(),
            level: 300,
            pair: "usability_test".to_string(),
            ..Default::default()
        };
        let registry = LayerRegistry::build(&[bad]);
        assert!(registry.get("ux_spec").is_none());
        assert!(registry.warnings.iter().any(|w| w.contains("out of range")));
    }

    #[test]
    fn registry_id_prefixes_for_custom_layer_appends_config_additions() {
        let registry = LayerRegistry::build(&[ux_spec(), usability_test()]);
        let mut config = HashMap::new();
        config.insert("ux_spec".to_string(), vec!["UXX".to_string()]);
        assert_eq!(
            registry.id_prefixes_for("ux_spec", &config),
            vec!["UX".to_string(), "UXX".to_string()]
        );
    }
}
