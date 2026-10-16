//! Create-time validation of requirement-tree (`parent`) edits
//! (wiki/300-requirement-tree-design.md §2.2, SPEC-TREE-002): a **pure
//! function** shared by `handoff_trace_update` (`upsert_item`) and
//! `handoff_doc_save` / `handoff_doc_update_section` (layer documents), so
//! both write paths reject exactly the same bad decompositions before any
//! byte is written.
//!
//! Checks run per edited item, in this order: `dotted_id`, `duplicate_id`,
//! `self_parent`, `parent_dangling`, `parent_cross_layer`, `tree_cycle`.
//! Multiple parents are allowed (the decomposition is a DAG), so there is no
//! "multiple parent" check.
//!
//! The checks are evaluated against the graph **after** the edits are
//! applied, so a cycle that only exists through a combination of edits (two
//! ops of one `trace_update` call, or several items of one saved document) is
//! found. Only parents *newly declared* by an edit (absent from the item's
//! previous declaration, or all of them when the item's layer changed) are
//! checked for `self_parent` / `parent_dangling` / `parent_cross_layer` /
//! `tree_cycle`: an item that already carries a legacy-invalid parent stays
//! editable (those are `trace_lint`'s findings), the same "only links added
//! by this edit" policy `validate_new_links` applies to `refines`/`verifies`.

use std::collections::{HashMap, HashSet, VecDeque};

use serde::Serialize;

/// One item of the decomposition graph as the validator needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeNode {
    pub id: String,
    /// The id of the document that owns this item.
    pub doc_id: String,
    /// Effective layer (`sub.layer.or(doc.layer)`).
    pub layer: Option<String>,
    /// Declared parents, verbatim.
    pub parents: Vec<String>,
}

/// An item's state before an edit, used to find the *newly declared* parents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeNodePrevious {
    pub layer: Option<String>,
    pub parents: Vec<String>,
}

/// One edited (created or rewritten) item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEdit {
    /// The item as it will stand after the edit.
    pub node: TreeNode,
    /// The same item's state in the same document before the edit; `None`
    /// when the edit creates it (which also arms the `duplicate_id` check).
    pub previous: Option<TreeNodePrevious>,
    /// The id is written with a dot in its heading (`DSP-1.2.3`), which the
    /// ID grammar silently splits into `DSP-1` + a title — set by callers
    /// that only see the parsed heading (`doc_save`). A `.` inside
    /// [`TreeNode::id`] itself is always flagged.
    pub dotted_heading: bool,
}

/// Error / warning code (`snake_case` on the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TreeErrorCode {
    DottedId,
    DuplicateId,
    SelfParent,
    ParentDangling,
    ParentCrossLayer,
    TreeCycle,
}

impl TreeErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DottedId => "dotted_id",
            Self::DuplicateId => "duplicate_id",
            Self::SelfParent => "self_parent",
            Self::ParentDangling => "parent_dangling",
            Self::ParentCrossLayer => "parent_cross_layer",
            Self::TreeCycle => "tree_cycle",
        }
    }
}

/// `{code, item, message, path?}` — the error shape of SPEC-TREE-002.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TreeError {
    pub code: TreeErrorCode,
    pub item: String,
    pub message: String,
    /// `tree_cycle` only: the cycle, following `parent` links from `item`
    /// back to `item` (first and last entries are both `item`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<Vec<String>>,
}

/// Which write path is validating — they differ on the two checks that are
/// about the item's *name* rather than the tree shape:
///
/// - [`Self::TraceUpdate`] rejects both a dotted id and an id another
///   document already owns.
/// - [`Self::DocSave`] (`doc_save` / `doc_update_section`) only *warns* about
///   a dotted id (the body is the user's own Markdown and stays valid), and
///   leaves cross-document id collisions to the pre-existing post-save
///   `stable_id ... already exists in other document(s)` warning, so the
///   long-standing "saved anyway, but flagged" behavior does not change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeWritePath {
    TraceUpdate,
    DocSave,
}

/// [`validate_tree_edits`]'s outcome: `errors` reject the write, `warnings`
/// are surfaced to the caller.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeValidation {
    pub errors: Vec<TreeError>,
    pub warnings: Vec<TreeError>,
}

/// `Some(error)` when `id` contains a `.` — `DSP-1.2.3` would be silently
/// split into the item `DSP-1` plus a title (wiki/300 §2.6).
pub fn dotted_id_error(id: &str) -> Option<TreeError> {
    id.contains('.').then(|| TreeError {
        code: TreeErrorCode::DottedId,
        item: id.to_string(),
        message: format!(
            "id '{id}' contains '.', which the ID grammar splits into a shorter id plus a \
             title; use dash-separated segments instead (e.g. DSP-001-002)"
        ),
        path: None,
    })
}

/// Validates `edits` against the graph `before` (see the module docs).
///
/// `before` is every item that exists prior to the edits (an edited item's
/// own old entry may be present; the edit replaces it). Errors are returned
/// edit by edit in input order, and within one edit in the documented check
/// order.
pub fn validate_tree_edits(
    before: &[TreeNode],
    edits: &[TreeEdit],
    write_path: TreeWritePath,
) -> TreeValidation {
    let mut owners: HashMap<&str, HashSet<&str>> = HashMap::new();
    let mut after: HashMap<&str, &TreeNode> = HashMap::with_capacity(before.len() + edits.len());
    for node in before {
        owners
            .entry(node.id.as_str())
            .or_default()
            .insert(node.doc_id.as_str());
        after.insert(node.id.as_str(), node);
    }
    for edit in edits {
        after.insert(edit.node.id.as_str(), &edit.node);
    }

    let mut out = TreeValidation::default();
    // A cycle is one fact however many of its members were edited: keyed by
    // its sorted member set so it is reported once.
    let mut reported_cycles: HashSet<Vec<String>> = HashSet::new();
    for edit in edits {
        let node = &edit.node;
        let id = node.id.as_str();

        let dotted_error = dotted_id_error(id).or_else(|| {
            edit.dotted_heading.then(|| TreeError {
                code: TreeErrorCode::DottedId,
                item: id.to_string(),
                message: format!(
                    "the heading of '{id}' writes the id with a '.', which the ID grammar \
                     splits into '{id}' plus a title; use dash-separated segments instead \
                     (e.g. DSP-001-002)"
                ),
                path: None,
            })
        });
        if let Some(error) = dotted_error {
            match write_path {
                TreeWritePath::TraceUpdate => out.errors.push(error),
                TreeWritePath::DocSave => out.warnings.push(error),
            }
        }

        if write_path == TreeWritePath::TraceUpdate
            && edit.previous.is_none()
            && owners
                .get(id)
                .is_some_and(|docs| docs.iter().any(|d| *d != node.doc_id))
        {
            out.errors.push(TreeError {
                code: TreeErrorCode::DuplicateId,
                item: id.to_string(),
                message: format!("id '{id}' already exists in another document"),
                path: None,
            });
        }

        let all_parents_are_new = edit
            .previous
            .as_ref()
            .is_none_or(|prev| prev.layer != node.layer);
        let new_parents: Vec<&str> = node
            .parents
            .iter()
            .map(String::as_str)
            .filter(|p| {
                all_parents_are_new
                    || edit
                        .previous
                        .as_ref()
                        .is_none_or(|prev| !prev.parents.iter().any(|q| q == p))
            })
            .collect();

        for parent in &new_parents {
            if *parent == id {
                out.errors.push(TreeError {
                    code: TreeErrorCode::SelfParent,
                    item: id.to_string(),
                    message: format!("'{id}' lists itself as its own parent"),
                    path: None,
                });
            }
        }
        for parent in &new_parents {
            if *parent != id && !after.contains_key(parent) {
                out.errors.push(TreeError {
                    code: TreeErrorCode::ParentDangling,
                    item: id.to_string(),
                    message: format!("parent '{parent}' of '{id}' does not exist"),
                    path: None,
                });
            }
        }
        for parent in &new_parents {
            if let Some(parent_node) = after.get(parent) {
                if *parent != id && parent_node.layer != node.layer {
                    out.errors.push(TreeError {
                        code: TreeErrorCode::ParentCrossLayer,
                        item: id.to_string(),
                        message: format!(
                            "parent '{parent}' is on layer {} but '{id}' is on layer {}; \
                             parents must be on the same layer (use refines across layers)",
                            layer_label(&parent_node.layer),
                            layer_label(&node.layer),
                        ),
                        path: None,
                    });
                }
            }
        }
        for parent in &new_parents {
            let usable = *parent != id
                && after
                    .get(parent)
                    .is_some_and(|parent_node| parent_node.layer == node.layer);
            if !usable {
                continue;
            }
            if let Some(path) = path_back_to(id, parent, &after) {
                let mut members: Vec<String> = path.clone();
                members.sort_unstable();
                members.dedup();
                if !reported_cycles.insert(members) {
                    continue;
                }
                out.errors.push(TreeError {
                    code: TreeErrorCode::TreeCycle,
                    item: id.to_string(),
                    message: format!("parent links form a cycle: {}", path.join(" -> ")),
                    path: Some(path),
                });
            }
        }
    }
    out
}

fn layer_label(layer: &Option<String>) -> String {
    layer
        .as_deref()
        .map_or_else(|| "(none)".to_string(), |l| format!("'{l}'"))
}

/// Breadth-first search (no recursion, visited set) following parent links
/// from `first_parent`; returns the shortest `start -> first_parent -> ... ->
/// start` path when `start` is reachable, i.e. when declaring `first_parent`
/// as a parent of `start` closes a cycle. Only same-layer, existing parents
/// are followed (other edges are reported by their own checks and are not
/// part of the effective graph).
fn path_back_to(
    start: &str,
    first_parent: &str,
    after: &HashMap<&str, &TreeNode>,
) -> Option<Vec<String>> {
    let mut predecessor: HashMap<&str, &str> = HashMap::new();
    let mut visited: HashSet<&str> = HashSet::new();
    let mut queue: VecDeque<&str> = VecDeque::new();
    visited.insert(first_parent);
    queue.push_back(first_parent);
    while let Some(current) = queue.pop_front() {
        let Some(current_node) = after.get(current) else {
            continue;
        };
        for next in &current_node.parents {
            let next = next.as_str();
            if next == current {
                continue;
            }
            let Some(next_node) = after.get(next) else {
                continue;
            };
            if next_node.layer != current_node.layer {
                continue;
            }
            if next == start {
                let mut chain = vec![current];
                let mut cursor = current;
                while let Some(prev) = predecessor.get(cursor) {
                    chain.push(prev);
                    cursor = prev;
                }
                chain.reverse();
                let mut path = vec![start.to_string()];
                path.extend(chain.into_iter().map(str::to_string));
                path.push(start.to_string());
                return Some(path);
            }
            if visited.insert(next) {
                predecessor.insert(next, current);
                queue.push_back(next);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, doc: &str, layer: &str, parents: &[&str]) -> TreeNode {
        TreeNode {
            id: id.to_string(),
            doc_id: doc.to_string(),
            layer: Some(layer.to_string()),
            parents: parents.iter().map(|p| p.to_string()).collect(),
        }
    }

    fn create(n: TreeNode) -> TreeEdit {
        TreeEdit {
            node: n,
            previous: None,
            dotted_heading: false,
        }
    }

    fn rewrite(n: TreeNode, prev_parents: &[&str]) -> TreeEdit {
        let previous = Some(TreeNodePrevious {
            layer: n.layer.clone(),
            parents: prev_parents.iter().map(|p| p.to_string()).collect(),
        });
        TreeEdit {
            node: n,
            previous,
            dotted_heading: false,
        }
    }

    fn codes(v: &TreeValidation) -> Vec<(&'static str, &str)> {
        v.errors
            .iter()
            .map(|e| (e.code.as_str(), e.item.as_str()))
            .collect()
    }

    fn base() -> Vec<TreeNode> {
        vec![
            node("REQ-A", "d1", "requirement", &[]),
            node("REQ-B", "d1", "requirement", &["REQ-A"]),
            node("SPEC-A", "d2", "basic_spec", &[]),
        ]
    }

    #[test]
    fn valid_multi_parent_edit_passes() {
        let v = validate_tree_edits(
            &base(),
            &[create(node(
                "REQ-C",
                "d1",
                "requirement",
                &["REQ-A", "REQ-B"],
            ))],
            TreeWritePath::TraceUpdate,
        );
        assert!(v.errors.is_empty(), "{v:?}");
        assert!(v.warnings.is_empty());
    }

    #[test]
    fn self_parent_is_rejected_even_among_other_parents() {
        let v = validate_tree_edits(
            &base(),
            &[create(node(
                "REQ-C",
                "d1",
                "requirement",
                &["REQ-B", "REQ-C"],
            ))],
            TreeWritePath::TraceUpdate,
        );
        assert_eq!(codes(&v), vec![("self_parent", "REQ-C")]);
    }

    #[test]
    fn dangling_parent_rejects_the_whole_edit() {
        let v = validate_tree_edits(
            &base(),
            &[create(node(
                "REQ-C",
                "d1",
                "requirement",
                &["REQ-A", "REQ-ZZZ"],
            ))],
            TreeWritePath::TraceUpdate,
        );
        assert_eq!(codes(&v), vec![("parent_dangling", "REQ-C")]);
        assert!(v.errors[0].message.contains("REQ-ZZZ"));
    }

    #[test]
    fn cross_layer_parent_is_rejected_including_mixed_declarations() {
        let v = validate_tree_edits(
            &base(),
            &[create(node(
                "REQ-C",
                "d1",
                "requirement",
                &["REQ-A", "SPEC-A"],
            ))],
            TreeWritePath::TraceUpdate,
        );
        assert_eq!(codes(&v), vec![("parent_cross_layer", "REQ-C")]);
        assert!(v.errors[0].message.contains("SPEC-A"));
    }

    #[test]
    fn cycle_through_existing_chain_reports_the_path() {
        // REQ-A <- REQ-B already (B's parent is A). Making A's parent B
        // closes the loop A -> B -> A.
        let v = validate_tree_edits(
            &base(),
            &[rewrite(node("REQ-A", "d1", "requirement", &["REQ-B"]), &[])],
            TreeWritePath::TraceUpdate,
        );
        assert_eq!(codes(&v), vec![("tree_cycle", "REQ-A")]);
        assert_eq!(
            v.errors[0].path.as_deref(),
            Some(
                &[
                    "REQ-A".to_string(),
                    "REQ-B".to_string(),
                    "REQ-A".to_string()
                ][..]
            )
        );
    }

    #[test]
    fn cycle_through_a_second_parent_of_a_dag_is_detected() {
        // REQ-X has parents REQ-A and REQ-B; REQ-A gaining REQ-X as parent
        // is a cycle reachable only through the DAG's second branch.
        let mut before = base();
        before.push(node("REQ-X", "d1", "requirement", &["REQ-B", "REQ-A"]));
        let v = validate_tree_edits(
            &before,
            &[rewrite(node("REQ-A", "d1", "requirement", &["REQ-X"]), &[])],
            TreeWritePath::TraceUpdate,
        );
        assert_eq!(codes(&v), vec![("tree_cycle", "REQ-A")]);
        let path = v.errors[0].path.as_ref().unwrap();
        assert_eq!(path.first().map(String::as_str), Some("REQ-A"));
        assert_eq!(path.last().map(String::as_str), Some("REQ-A"));
        assert!(path.contains(&"REQ-X".to_string()));
    }

    #[test]
    fn cycle_made_only_by_a_combination_of_two_edits_is_detected() {
        let before = vec![
            node("REQ-A", "d1", "requirement", &[]),
            node("REQ-B", "d1", "requirement", &[]),
        ];
        let v = validate_tree_edits(
            &before,
            &[
                rewrite(node("REQ-A", "d1", "requirement", &["REQ-B"]), &[]),
                rewrite(node("REQ-B", "d1", "requirement", &["REQ-A"]), &[]),
            ],
            TreeWritePath::TraceUpdate,
        );
        assert!(
            v.errors.iter().any(|e| e.code == TreeErrorCode::TreeCycle),
            "{v:?}"
        );
    }

    #[test]
    fn dotted_id_is_an_error_for_trace_update_and_a_warning_for_doc_save() {
        let edit = create(node("DSP-1.2", "d1", "requirement", &[]));
        let rejected = validate_tree_edits(
            &base(),
            std::slice::from_ref(&edit),
            TreeWritePath::TraceUpdate,
        );
        assert_eq!(codes(&rejected), vec![("dotted_id", "DSP-1.2")]);
        let warned = validate_tree_edits(&base(), &[edit], TreeWritePath::DocSave);
        assert!(warned.errors.is_empty());
        assert_eq!(warned.warnings.len(), 1);
        assert_eq!(warned.warnings[0].code, TreeErrorCode::DottedId);

        let heading = TreeEdit {
            dotted_heading: true,
            ..create(node("DSP-1", "d1", "requirement", &[]))
        };
        let warned = validate_tree_edits(&base(), &[heading], TreeWritePath::DocSave);
        assert_eq!(warned.warnings.len(), 1);
    }

    #[test]
    fn duplicate_id_is_rejected_only_when_the_edit_creates_it() {
        let created = validate_tree_edits(
            &base(),
            &[create(node("REQ-A", "d9", "requirement", &[]))],
            TreeWritePath::TraceUpdate,
        );
        assert_eq!(codes(&created), vec![("duplicate_id", "REQ-A")]);

        // `doc_save` leaves cross-document collisions to its own warning.
        let saved = validate_tree_edits(
            &base(),
            &[create(node("REQ-A", "d9", "requirement", &[]))],
            TreeWritePath::DocSave,
        );
        assert!(
            saved.errors.is_empty() && saved.warnings.is_empty(),
            "{saved:?}"
        );

        // Rewriting the item in its own document is not a duplicate.
        let rewritten = validate_tree_edits(
            &base(),
            &[rewrite(node("REQ-A", "d1", "requirement", &[]), &[])],
            TreeWritePath::TraceUpdate,
        );
        assert!(rewritten.errors.is_empty(), "{rewritten:?}");
    }

    #[test]
    fn checks_run_in_the_documented_order_for_one_edit() {
        let edit = create(node(
            "REQ-A.1",
            "d9",
            "requirement",
            &["REQ-A.1", "REQ-NOPE", "SPEC-A"],
        ));
        let v = validate_tree_edits(&base(), &[edit], TreeWritePath::TraceUpdate);
        let order: Vec<&str> = v.errors.iter().map(|e| e.code.as_str()).collect();
        assert_eq!(
            order,
            vec![
                "dotted_id",
                "self_parent",
                "parent_dangling",
                "parent_cross_layer"
            ]
        );
    }

    #[test]
    fn already_declared_legacy_invalid_parents_do_not_block_an_unrelated_edit() {
        let before = vec![node("REQ-A", "d1", "requirement", &["REQ-GONE"])];
        let v = validate_tree_edits(
            &before,
            &[rewrite(
                node("REQ-A", "d1", "requirement", &["REQ-GONE"]),
                &["REQ-GONE"],
            )],
            TreeWritePath::TraceUpdate,
        );
        assert!(v.errors.is_empty(), "{v:?}");
    }

    #[test]
    fn a_layer_change_rechecks_every_existing_parent() {
        let before = base();
        let mut edit = rewrite(node("REQ-B", "d1", "basic_spec", &["REQ-A"]), &["REQ-A"]);
        edit.previous.as_mut().unwrap().layer = Some("requirement".to_string());
        let v = validate_tree_edits(&before, &[edit], TreeWritePath::TraceUpdate);
        assert_eq!(codes(&v), vec![("parent_cross_layer", "REQ-B")]);
    }

    #[test]
    fn error_serializes_to_the_documented_shape() {
        let v = validate_tree_edits(
            &base(),
            &[rewrite(node("REQ-A", "d1", "requirement", &["REQ-B"]), &[])],
            TreeWritePath::TraceUpdate,
        );
        let json = serde_json::to_value(&v.errors[0]).unwrap();
        assert_eq!(json["code"], "tree_cycle");
        assert_eq!(json["item"], "REQ-A");
        assert!(json["message"].is_string());
        assert_eq!(json["path"][0], "REQ-A");
    }
}
