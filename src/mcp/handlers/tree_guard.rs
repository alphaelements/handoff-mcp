//! Glue between the write handlers (`handoff_trace_update`'s `upsert_item`,
//! `handoff_doc_save`, `handoff_doc_update_section`) and the pure requirement
//! tree validator ([`crate::trace::tree_validate`], SPEC-TREE-002): builds the
//! validator's graph view from stored documents / freshly parsed bodies and
//! renders its errors in the one wire shape every write path shares
//! (`{code, item, message, path?}`).

use std::collections::HashMap;
use std::fmt;

use serde_json::{json, Value};

use anyhow::Result;

use crate::storage::docs::layer_parse::{parse_layer_body, ParseWarningKind, ParsedItem};
use crate::storage::docs::model::DocMetadata;
use crate::storage::docs::read_doc_body;
use crate::trace::lint::ItemTreeText;
use crate::trace::tree_validate::{TreeError, TreeNode, TreeNodePrevious};

/// A write was rejected by the requirement-tree validation. Carried through
/// `anyhow` so the caller can downcast to emit the structured errors.
#[derive(Debug, Clone)]
pub(crate) struct TreeValidationFailed {
    pub errors: Vec<TreeError>,
    /// Text prepended to the message (e.g. `ops[2]: `); empty for the
    /// document tools.
    prefix: String,
}

impl TreeValidationFailed {
    pub fn new(errors: Vec<TreeError>) -> Self {
        Self {
            errors,
            prefix: String::new(),
        }
    }

    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// The structured errors as the JSON array every response embeds.
    pub fn errors_json(&self) -> Value {
        serde_json::to_value(&self.errors).unwrap_or_else(|_| json!([]))
    }
}

impl fmt::Display for TreeValidationFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = self
            .errors
            .iter()
            .map(|e| format!("[{}] {}: {}", e.code.as_str(), e.item, e.message))
            .collect();
        write!(
            f,
            "{}tree validation failed: {}",
            self.prefix,
            parts.join("; ")
        )
    }
}

impl std::error::Error for TreeValidationFailed {}

/// The error text `doc_save` / `doc_update_section` return: the human line
/// followed by the structured errors as JSON, so a caller can parse them.
pub(crate) fn doc_error_message(failed: &TreeValidationFailed) -> String {
    format!("{failed}\nerrors: {}", failed.errors_json())
}

/// One stored document's items as validator nodes (implicit acceptance
/// items, which can never carry a `parent`, are skipped).
pub(crate) fn nodes_from_stored_doc(doc: &DocMetadata) -> Vec<TreeNode> {
    let Some(verification) = &doc.verification else {
        return Vec::new();
    };
    verification
        .items
        .iter()
        .flat_map(|item| item.sub_items.iter())
        .filter(|sub| sub.implicit_of.is_none())
        .filter_map(|sub| {
            let id = sub.stable_id.as_ref()?;
            Some(TreeNode {
                id: id.clone(),
                doc_id: doc.id.clone(),
                layer: sub.layer.clone().or_else(|| doc.layer.clone()),
                parents: sub.tree_parents.clone(),
            })
        })
        .collect()
}

/// A freshly parsed item as a validator node.
pub(crate) fn node_from_parsed(item: &ParsedItem, doc_id: &str) -> TreeNode {
    TreeNode {
        id: item.id.clone(),
        doc_id: doc_id.to_string(),
        layer: item.effective_layer.clone(),
        parents: item.ext_attrs.tree_parents.clone(),
    }
}

/// A previously parsed item's state, for the "newly declared parents" rule.
pub(crate) fn previous_from_parsed(item: &ParsedItem) -> TreeNodePrevious {
    TreeNodePrevious {
        layer: item.effective_layer.clone(),
        parents: item.ext_attrs.tree_parents.clone(),
    }
}

/// A stored item's state before an edit.
pub(crate) fn previous_from_stored(doc: &DocMetadata) -> HashMap<String, TreeNodePrevious> {
    nodes_from_stored_doc(doc)
        .into_iter()
        .map(|n| {
            (
                n.id,
                TreeNodePrevious {
                    layer: n.layer,
                    parents: n.parents,
                },
            )
        })
        .collect()
}

/// SPEC-TREE-013: the stable_ids of items that lost **every** declared parent
/// because the items in `removed` no longer exist. A DAG child that still has
/// at least one surviving parent is not orphaned; neither is an item whose
/// parents were already dangling for an unrelated reason (it must declare at
/// least one of the `removed` ids). `docs` is the corpus *after* the write, so
/// an id that was merely moved to another document still counts as present.
/// Sorted and de-duplicated.
pub(crate) fn orphaned_children(docs: &[DocMetadata], removed: &[String]) -> Vec<String> {
    if removed.is_empty() {
        return Vec::new();
    }
    let nodes: Vec<TreeNode> = docs.iter().flat_map(nodes_from_stored_doc).collect();
    let existing: std::collections::HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let mut orphans: Vec<String> = nodes
        .iter()
        .filter(|n| {
            n.parents.iter().any(|p| removed.contains(p))
                && n.parents.iter().all(|p| !existing.contains(p.as_str()))
        })
        .map(|n| n.id.clone())
        .collect();
    orphans.sort();
    orphans.dedup();
    orphans
}

/// Every document's items as validator nodes; a document with an entry in
/// `bodies` (an in-memory body not yet on disk) is parsed from that body
/// instead of read from its stored `SubItem`s.
pub(crate) fn nodes_from_corpus(
    docs: &[DocMetadata],
    bodies: &HashMap<&str, &str>,
    prefix_table: &HashMap<String, Vec<String>>,
) -> Vec<TreeNode> {
    let mut out = Vec::new();
    for doc in docs {
        match bodies.get(doc.id.as_str()) {
            Some(body) => {
                let parsed = parse_layer_body(body, doc.layer.as_deref(), prefix_table);
                out.extend(parsed.items.iter().map(|it| node_from_parsed(it, &doc.id)));
            }
            None => out.extend(nodes_from_stored_doc(doc)),
        }
    }
    out
}

/// `true` when `item`'s heading line in `body` writes its id with a `.`
/// directly after it (`### DSP-1.2.3 …` is parsed as the item `DSP-1`).
pub(crate) fn heading_writes_dotted_id(body: &str, item: &ParsedItem) -> bool {
    let Some(line) = body.lines().nth(item.start_line.saturating_sub(1)) else {
        return false;
    };
    let text = line.trim_start().trim_start_matches('#').trim_start();
    text.strip_prefix(item.id.as_str())
        .and_then(|rest| rest.strip_prefix('.'))
        .is_some_and(|rest| rest.chars().next().is_some_and(|c| c.is_ascii_digit()))
}

/// The source-text facts `trace_lint`'s requirement-tree rules read, for
/// every item of every layer document: the heading's dotted-id shape, the
/// `invalid_order` parse warnings and the acceptance-free body. Parses each
/// layer document's body afresh (read-only), so the result does not depend on
/// whether the document was out of sync with its stored matrix. The first
/// occurrence of a duplicated id wins, as in the trace graph.
pub(crate) fn collect_item_tree_text(
    handoff: &std::path::Path,
    docs: &[DocMetadata],
    prefix_table: &HashMap<String, Vec<String>>,
) -> Result<HashMap<String, ItemTreeText>> {
    let mut out: HashMap<String, ItemTreeText> = HashMap::new();
    for doc in docs.iter().filter(|d| d.layer.is_some()) {
        let Some(body) = read_doc_body(handoff, &doc.slug)? else {
            continue;
        };
        let parsed = parse_layer_body(&body, doc.layer.as_deref(), prefix_table);
        for item in &parsed.items {
            let invalid_order = parsed
                .warnings
                .iter()
                .filter(|w| {
                    w.kind == ParseWarningKind::InvalidOrder
                        && w.id.as_deref() == Some(item.id.as_str())
                })
                .map(ToString::to_string)
                .collect();
            out.entry(item.id.clone()).or_insert_with(|| ItemTreeText {
                dotted_heading: heading_writes_dotted_id(&body, item),
                invalid_order,
                statement: item.def_statement.clone(),
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::docs::layer::LayerRegistry;
    use crate::storage::docs::layer_parse::default_prefix_table;

    fn parse(body: &str) -> Vec<ParsedItem> {
        let registry = LayerRegistry::build(&[]);
        let table = default_prefix_table(&registry, &HashMap::new());
        parse_layer_body(body, Some("requirement"), &table).items
    }

    #[test]
    fn dotted_heading_is_detected_only_for_id_dot_digit() {
        let body = "# Doc\n\n## REQ-1.2.3 概要\n\n本文\n\n## REQ-2. 概要\n\n本文\n";
        let items = parse(body);
        let dotted = items.iter().find(|i| i.id == "REQ-1").expect("REQ-1");
        assert!(heading_writes_dotted_id(body, dotted));
        let req = items.iter().find(|i| i.id == "REQ-2").expect("REQ-2");
        assert!(
            !heading_writes_dotted_id(body, req),
            "'REQ-1. 概要' is an ordinary separator, not a dotted id"
        );
    }

    #[test]
    fn display_lists_every_error_and_json_carries_the_shape() {
        use crate::trace::tree_validate::{validate_tree_edits, TreeEdit, TreeWritePath};
        let v = validate_tree_edits(
            &[],
            &[TreeEdit {
                node: TreeNode {
                    id: "REQ-A".into(),
                    doc_id: "d".into(),
                    layer: Some("requirement".into()),
                    parents: vec!["REQ-A".into()],
                },
                previous: None,
                dotted_heading: false,
            }],
            TreeWritePath::TraceUpdate,
        );
        let failed = TreeValidationFailed::new(v.errors);
        assert!(failed.to_string().contains("[self_parent] REQ-A"));
        assert_eq!(failed.errors_json()[0]["code"], "self_parent");
    }
}
