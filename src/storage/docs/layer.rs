//! V-model layer definitions (wiki/220-vmodel-integration-design.md §2.1,
//! M1 t360.4): the 6 built-in layers' `side`/`level`/`pair`/default ID
//! prefixes, plus resolution of a layer's *effective* ID-prefix allow-list
//! against project config (`[trace.id_prefixes]`, §2.1).
//!
//! This module deliberately stops at "static table + config merge" — body
//! heading parsing against these prefixes (t360.5), layer synchronization
//! and the write-guard on body-owned `SubItem` fields (t360.6), and
//! `TaskLink.role`-based link authority (t360.7) are later tasks' scope.

use std::collections::HashMap;

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
        default_id_prefixes: &["AT"],
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
}
