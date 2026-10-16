//! `handoff_trace_tree` (wiki/300-requirement-tree-design.md, SPEC-TREE-008):
//! read-only nested view of the requirement tree (decomposition DAG).
//!
//! The tree structure, kinds and per-node roll-up all come from the
//! [`TraceGraph`]'s own [`crate::trace::tree::TreeIndex`] and
//! [`crate::trace::tree::Subtree`] — the very values `_trace_report.json`
//! (schema v3) is built from — so both views show the same numbers (NFR-005).
//! This module only selects (root / scope / layer / filter), bounds (depth),
//! projects (fields) and shapes them. Nothing here recurses: the selection
//! walks and the nested JSON assembly use explicit work lists, so a very
//! deep chain cannot overflow the stack.
//!
//! Load: the same fully read-only load `handoff_trace_lint` / `_next` use
//! (E6) — it never writes under `.handoff/`.

use std::collections::{HashMap, HashSet};

use anyhow::{bail, Result};
use serde_json::{json, Map, Value};

use super::trace::{
    approval_str, collect_item_meta, empty_result_diagnostics, load_trace_input_read_only, ItemMeta,
};
use super::{HandlerContext, Warning};
use crate::trace::tree::{TreeIndex, TreeKind};
use crate::trace::types::{ItemState, SuspectKind};
use crate::trace::{TraceGraph, TraceItemInput};

/// `own.<key>` names accepted by `fields`.
const OWN_KEYS: &[&str] = &[
    "state",
    "dev_stage",
    "approval",
    "coverage",
    "suspect",
    "tasks",
    "acceptance_criteria",
];

/// `subtree.<key>` names accepted by `fields` (the serialized `Subtree`).
const SUBTREE_KEYS: &[&str] = &[
    "depth",
    "parents",
    "children",
    "leaves",
    "state",
    "dev_stage",
    "approval",
    "excluded",
    "suspect",
    "progress",
    "tasks",
    "all_tasks",
];

const ITEM_PRIORITIES: &[&str] = &["P0", "P1", "P2", "P3"];

/// Node keys dropped when `fields` is given: only the structural keys
/// (`id`, `kind`, `parents`, `has_children`, `children`) always remain.
const OPTIONAL_NODE_KEYS: &[&str] = &["title", "layer", "doc", "seq", "order", "priority"];

/// `filter.*`: a node matches when it satisfies every given condition.
#[derive(Default)]
struct NodeFilter {
    state: Option<Vec<ItemState>>,
    priority: Option<HashSet<String>>,
    suspect: Option<bool>,
    gaps_only: bool,
    kind: Option<Vec<TreeKind>>,
}

impl NodeFilter {
    fn is_active(&self) -> bool {
        self.state.is_some()
            || self.priority.is_some()
            || self.suspect.is_some()
            || self.gaps_only
            || self.kind.is_some()
    }
}

/// `fields`: the `own.*` / `subtree.*` keys to return.
struct FieldSelection {
    own: Vec<String>,
    subtree: Vec<String>,
}

struct TreeParams {
    root: Option<String>,
    scope: Option<String>,
    depth: Option<usize>,
    layer: Option<String>,
    filter: NodeFilter,
    fields: Option<FieldSelection>,
}

fn optional<'a>(arguments: &'a Value, key: &str) -> Option<&'a Value> {
    arguments.get(key).filter(|v| !v.is_null())
}

fn string_param(value: Option<&Value>, name: &str) -> Result<Option<String>> {
    match value {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => bail!("{name}: must be a string, got {other}"),
    }
}

/// A non-empty array of strings, each one of `allowed` (when given).
fn enum_array(value: Option<&Value>, name: &str, allowed: &[&str]) -> Result<Option<Vec<String>>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(entries) = value.as_array() else {
        bail!("{name}: must be an array of strings, got {value}");
    };
    if entries.is_empty() {
        bail!("{name}: must not be empty (omit it to match everything)");
    }
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(s) = entry.as_str() else {
            bail!("{name}: every entry must be a string, got {entry}");
        };
        if !allowed.contains(&s) {
            bail!(
                "{name}: unknown value {s:?}; expected one of {}",
                allowed.join(", ")
            );
        }
        out.push(s.to_string());
    }
    Ok(Some(out))
}

fn parse_kind(name: &str) -> TreeKind {
    match name {
        "composite" => TreeKind::Composite,
        "decision" => TreeKind::Decision,
        _ => TreeKind::Leaf,
    }
}

fn parse_filter(value: Option<&Value>) -> Result<NodeFilter> {
    let Some(value) = value else {
        return Ok(NodeFilter::default());
    };
    let Some(map) = value.as_object() else {
        bail!("filter: must be an object, got {value}");
    };
    let states = enum_array(
        map.get("state").filter(|v| !v.is_null()),
        "filter.state",
        &["passing", "failing", "blocked", "not_run", "uncovered"],
    )?;
    let state = match states {
        None => None,
        Some(names) => {
            let mut states = Vec::with_capacity(names.len());
            for name in names {
                states.push(
                    serde_json::from_value::<ItemState>(Value::String(name.clone()))
                        .map_err(|e| anyhow::anyhow!("filter.state: {name:?}: {e}"))?,
                );
            }
            Some(states)
        }
    };
    let priority = enum_array(
        map.get("priority").filter(|v| !v.is_null()),
        "filter.priority",
        ITEM_PRIORITIES,
    )?
    .map(|v| v.into_iter().collect());
    let kind = enum_array(
        map.get("kind").filter(|v| !v.is_null()),
        "filter.kind",
        &["leaf", "composite", "decision"],
    )?
    .map(|v| v.iter().map(|n| parse_kind(n)).collect());
    let suspect = match map.get("suspect").filter(|v| !v.is_null()) {
        None => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(other) => bail!("filter.suspect: must be a boolean, got {other}"),
    };
    let gaps_only = match map.get("gaps_only").filter(|v| !v.is_null()) {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(other) => bail!("filter.gaps_only: must be a boolean, got {other}"),
    };
    Ok(NodeFilter {
        state,
        priority,
        suspect,
        gaps_only,
        kind,
    })
}

fn parse_fields(value: Option<&Value>) -> Result<Option<FieldSelection>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(entries) = value.as_array() else {
        bail!("fields: must be an array of own.<key> / subtree.<key>, got {value}");
    };
    if entries.is_empty() {
        bail!("fields: must not be empty (omit it to return every field)");
    }
    let mut selection = FieldSelection {
        own: Vec::new(),
        subtree: Vec::new(),
    };
    for entry in entries {
        let Some(field) = entry.as_str() else {
            bail!("fields: every entry must be a string, got {entry}");
        };
        let (target, allowed, key) = if let Some(key) = field.strip_prefix("own.") {
            (&mut selection.own, OWN_KEYS, key)
        } else if let Some(key) = field.strip_prefix("subtree.") {
            (&mut selection.subtree, SUBTREE_KEYS, key)
        } else {
            bail!("fields: {field:?} must start with 'own.' or 'subtree.'");
        };
        if !allowed.contains(&key) {
            bail!(
                "fields: unknown key {field:?}; expected one of {}",
                allowed.join(", ")
            );
        }
        if !target.iter().any(|k| k == key) {
            target.push(key.to_string());
        }
    }
    Ok(Some(selection))
}

impl TreeParams {
    fn parse(arguments: &Value) -> Result<Self> {
        let root = string_param(optional(arguments, "root"), "root")?;
        let scope = string_param(optional(arguments, "scope"), "scope")?;
        if root.is_some() && scope.is_some() {
            bail!("'root' and 'scope' are mutually exclusive");
        }
        let depth = match optional(arguments, "depth") {
            None => None,
            Some(v) => match v.as_u64() {
                Some(d) => Some(usize::try_from(d).unwrap_or(usize::MAX)),
                None => bail!("depth: must be an integer >= 0, got {v}"),
            },
        };
        Ok(Self {
            root,
            scope,
            depth,
            layer: string_param(optional(arguments, "layer"), "layer")?,
            filter: parse_filter(optional(arguments, "filter"))?,
            fields: parse_fields(optional(arguments, "fields"))?,
        })
    }
}

/// Layer display rank: in-use layers first (in their configured order), then
/// any other layer that has tree items, alphabetically.
fn layer_rank(graph: &TraceGraph) -> HashMap<String, usize> {
    let mut rank: HashMap<String, usize> = HashMap::new();
    for layer in &graph.in_use_layers().layers {
        let next = rank.len();
        rank.entry(layer.clone()).or_insert(next);
    }
    for layer in graph.tree().roots_by_layer.keys() {
        let next = rank.len();
        rank.entry(layer.clone()).or_insert(next);
    }
    rank
}

/// The selected roots, in display order.
fn select_roots(
    params: &TreeParams,
    graph: &TraceGraph,
    ranks: &HashMap<String, usize>,
    warnings: &mut Vec<Warning>,
) -> Result<Vec<String>> {
    let tree = graph.tree();
    if let Some(layer) = &params.layer {
        if !ranks.contains_key(layer) {
            let mut known: Vec<&str> = ranks.keys().map(String::as_str).collect();
            known.sort_unstable();
            bail!(
                "layer: unknown layer {layer:?}; known layers: {}",
                known.join(", ")
            );
        }
    }
    let layer_ok = |id: &str| {
        params
            .layer
            .as_deref()
            .is_none_or(|layer| tree.layer(id) == Some(layer))
    };

    if let Some(root) = &params.root {
        if !tree.contains(root) {
            bail!("root: '{root}' is not an item of the requirement tree");
        }
        if !layer_ok(root) {
            bail!(
                "root: '{root}' is on layer {:?}, not {:?}",
                tree.layer(root).unwrap_or_default(),
                params.layer.as_deref().unwrap_or_default()
            );
        }
        return Ok(vec![root.clone()]);
    }

    // Position of every true root in the stable (layer, sibling) order.
    let mut global: Vec<&String> = Vec::new();
    let mut layers: Vec<(&String, &Vec<String>)> = tree.roots_by_layer.iter().collect();
    layers.sort_by_key(|(layer, _)| ranks.get(*layer).copied().unwrap_or(usize::MAX));
    for (_, ids) in layers {
        global.extend(ids);
    }

    let mut selected: Vec<String> = match &params.scope {
        None => global.iter().map(|id| (*id).clone()).collect(),
        Some(prefix) => {
            let matches = |id: &str| id.starts_with(prefix.as_str());
            let mut ids: Vec<String> = tree
                .children_first()
                .iter()
                .filter(|id| matches(id) && tree.parents(id).iter().all(|p| !matches(p)))
                .cloned()
                .collect();
            let position: HashMap<&str, usize> = global
                .iter()
                .enumerate()
                .map(|(i, id)| (id.as_str(), i))
                .collect();
            ids.sort_by_key(|id| {
                (
                    tree.layer(id)
                        .and_then(|l| ranks.get(l).copied())
                        .unwrap_or(usize::MAX),
                    position.get(id.as_str()).copied().unwrap_or(usize::MAX),
                    id.clone(),
                )
            });
            if ids.is_empty() {
                warnings.push(Warning::from(format!(
                    "scope {prefix:?} matched no item of the requirement tree"
                )));
            }
            ids
        }
    };
    selected.retain(|id| layer_ok(id));
    Ok(selected)
}

/// Everything a node's JSON needs that the graph itself does not carry.
struct NodeContext<'a> {
    graph: &'a TraceGraph,
    meta: HashMap<String, ItemMeta>,
    inputs: HashMap<&'a str, &'a TraceItemInput>,
    /// Per item, the distinct kinds of suspect it carries (SPEC-TREE-008:
    /// `own.suspect` is a `string[]`; empty when the item has none).
    suspect_kinds: HashMap<&'a str, Vec<SuspectKind>>,
    /// Items carrying a tree-structure finding (parent_dangling, ...).
    tree_gap_items: HashSet<&'a str>,
}

impl NodeContext<'_> {
    fn priority(&self, id: &str) -> Option<&str> {
        self.meta.get(id).and_then(|m| m.priority.as_deref())
    }

    fn own_json(&self, id: &str) -> Value {
        let meta = self.meta.get(id);
        let tasks = self
            .graph
            .subtree(id)
            .map(|s| s.tasks.clone())
            .unwrap_or_default();
        json!({
            "state": self.graph.state(id),
            "dev_stage": meta.and_then(|m| m.dev_stage.as_deref()),
            "approval": meta
                .map(|m| approval_str(m.approval.as_deref(), &m.status))
                .unwrap_or("draft"),
            "coverage": {
                "horizontal": self.graph.item_horizontal(id),
                "vertical": self.graph.item_vertical(id),
            },
            "suspect": self.suspect_kinds.get(id).cloned().unwrap_or_default(),
            "tasks": tasks,
            "acceptance_criteria": meta.map_or(0, |m| m.acceptance.len()),
        })
    }

    /// The node without its `children` (the caller sets those).
    fn node_json(&self, id: &str, fields: Option<&FieldSelection>) -> Map<String, Value> {
        let tree = self.graph.tree();
        let meta = self.meta.get(id);
        let mut node = Map::new();
        node.insert("id".into(), json!(id));
        node.insert("title".into(), json!(meta.map_or("", |m| m.title.as_str())));
        node.insert("kind".into(), json!(tree.kind(id)));
        node.insert("layer".into(), json!(tree.layer(id)));
        node.insert("doc".into(), json!(meta.map(|m| m.doc_id.as_str())));
        node.insert("seq".into(), json!(meta.and_then(|m| m.fragment_seq)));
        node.insert(
            "order".into(),
            json!(self.inputs.get(id).and_then(|i| i.tree.order)),
        );
        node.insert("priority".into(), json!(self.priority(id)));
        node.insert("own".into(), self.own_json(id));
        node.insert(
            "subtree".into(),
            serde_json::to_value(self.graph.subtree(id)).unwrap_or(Value::Null),
        );
        node.insert("parents".into(), json!(tree.parents(id)));
        node.insert("has_children".into(), json!(!tree.children(id).is_empty()));

        if let Some(fields) = fields {
            for key in OPTIONAL_NODE_KEYS {
                node.remove(*key);
            }
            project(&mut node, "own", &fields.own);
            project(&mut node, "subtree", &fields.subtree);
        }
        node
    }

    fn has_suspect(&self, id: &str) -> bool {
        self.suspect_kinds.contains_key(id)
    }

    /// SPEC-TREE-008 / REQ-TREE-086 AC2: a node has a gap when it is
    /// uncovered or failing, carries a suspect, or has a tree-structure
    /// finding. `not_run` / `blocked` are work in progress, not gaps, and
    /// generic trace gaps are deliberately not consulted (an unverified
    /// item already reads `uncovered`).
    fn has_gap(&self, id: &str) -> bool {
        self.graph
            .state(id)
            .is_some_and(|s| matches!(s, ItemState::Uncovered | ItemState::Failing))
            || self.has_suspect(id)
            || self.tree_gap_items.contains(id)
    }

    fn matches(&self, id: &str, filter: &NodeFilter) -> bool {
        let tree = self.graph.tree();
        if let Some(states) = &filter.state {
            if !self.graph.state(id).is_some_and(|s| states.contains(&s)) {
                return false;
            }
        }
        if let Some(priorities) = &filter.priority {
            if !self.priority(id).is_some_and(|p| priorities.contains(p)) {
                return false;
            }
        }
        if let Some(want) = filter.suspect {
            if self.has_suspect(id) != want {
                return false;
            }
        }
        if filter.gaps_only && !self.has_gap(id) {
            return false;
        }
        if let Some(kinds) = &filter.kind {
            if !tree.kind(id).is_some_and(|k| kinds.contains(&k)) {
                return false;
            }
        }
        true
    }
}

/// Distinct suspect kinds per item, in the graph's deterministic suspect order.
fn suspect_kinds_by_item(graph: &TraceGraph) -> HashMap<&str, Vec<SuspectKind>> {
    let mut by_item: HashMap<&str, Vec<SuspectKind>> = HashMap::new();
    for suspect in graph.suspects() {
        let kinds = by_item.entry(suspect.item.as_str()).or_default();
        if !kinds.contains(&suspect.kind) {
            kinds.push(suspect.kind);
        }
    }
    by_item
}

/// Keeps only `keys` of the object under `field`; removes the field when no
/// key was requested.
fn project(node: &mut Map<String, Value>, field: &str, keys: &[String]) {
    if keys.is_empty() {
        node.remove(field);
        return;
    }
    if let Some(Value::Object(map)) = node.get_mut(field) {
        map.retain(|k, _| keys.iter().any(|wanted| wanted == k));
    }
}

/// Every node reachable from `roots` through `children`, restricted by
/// `in_scope`.
fn reachable_from(
    tree: &TreeIndex,
    roots: &[String],
    in_scope: &dyn Fn(&str) -> bool,
) -> HashSet<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut stack: Vec<&str> = roots.iter().map(String::as_str).collect();
    while let Some(id) = stack.pop() {
        if !seen.insert(id.to_string()) {
            continue;
        }
        for child in tree.children(id) {
            if in_scope(child) && !seen.contains(child) {
                stack.push(child);
            }
        }
    }
    seen
}

/// `matches` plus every ancestor of a match inside `within`.
fn with_ancestors(
    tree: &TreeIndex,
    matches: Vec<&str>,
    within: &HashSet<String>,
) -> HashSet<String> {
    let mut keep: HashSet<String> = HashSet::new();
    let mut stack = matches;
    while let Some(id) = stack.pop() {
        if !keep.insert(id.to_string()) {
            continue;
        }
        for parent in tree.parents(id) {
            if within.contains(parent) && !keep.contains(parent) {
                stack.push(parent);
            }
        }
    }
    keep
}

/// One emitted occurrence of a node in the nested output.
struct Emitted {
    id: String,
    parent: Option<usize>,
    /// `true` when the node's `children` array is present (possibly empty).
    expanded: bool,
}

pub fn handle_trace_tree(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let params = TreeParams::parse(arguments)?;

    let loaded = load_trace_input_read_only(&ctx.handoff_dir, Vec::new())?;
    let graph = TraceGraph::build(&loaded.trace_input);
    let tree = graph.tree();

    let mut warnings: Vec<Warning> = loaded
        .config_warnings
        .iter()
        .cloned()
        .map(Warning::from)
        .collect();
    if tree.is_empty() {
        warnings.extend(empty_result_diagnostics(&loaded, &graph));
    }

    let ranks = layer_rank(&graph);
    let roots = select_roots(&params, &graph, &ranks, &mut warnings)?;

    let mut inputs: HashMap<&str, &TraceItemInput> = HashMap::new();
    for input in &loaded.trace_input.items {
        inputs.entry(input.stable_id.as_str()).or_insert(input);
    }
    let node_ctx = NodeContext {
        graph: &graph,
        meta: collect_item_meta(&loaded.docs),
        inputs,
        suspect_kinds: suspect_kinds_by_item(&graph),
        tree_gap_items: graph.tree_gaps().iter().map(|g| g.item.as_str()).collect(),
    };

    // Selection: reachable nodes, narrowed to matches + ancestors.
    let scope = params.scope.clone();
    let in_scope = move |id: &str| scope.as_deref().is_none_or(|p| id.starts_with(p));
    let reachable = reachable_from(tree, &roots, &in_scope);
    let keep: HashSet<String> = if params.filter.is_active() {
        let matches: Vec<&str> = reachable
            .iter()
            .map(String::as_str)
            .filter(|id| node_ctx.matches(id, &params.filter))
            .collect();
        with_ancestors(tree, matches, &reachable)
    } else {
        reachable
    };

    // Depth-first pre-order walk. A node with several parents is expanded
    // at its first occurrence only; later occurrences (and nodes below the
    // depth limit) carry `children: null` with `has_children: true`.
    let mut emitted: Vec<Emitted> = Vec::new();
    let mut expanded_ids: HashSet<String> = HashSet::new();
    let mut stack: Vec<(String, Option<usize>, usize)> = roots
        .iter()
        .filter(|id| keep.contains(*id))
        .rev()
        .map(|id| (id.clone(), None, 0))
        .collect();
    while let Some((id, parent, depth)) = stack.pop() {
        let index = emitted.len();
        let children = tree.children(&id);
        let within_depth = params.depth.is_none_or(|limit| depth < limit);
        let expanded = children.is_empty() || (within_depth && !expanded_ids.contains(&id));
        if expanded {
            expanded_ids.insert(id.clone());
            for child in children.iter().rev().filter(|c| keep.contains(*c)) {
                stack.push((child.clone(), Some(index), depth + 1));
            }
        }
        emitted.push(Emitted {
            id,
            parent,
            expanded,
        });
    }

    // Totals over the unique nodes shown, with the project-roll-up's own
    // definitions (a roll-up leaf is a non-branch node).
    let mut unique: HashSet<&str> = HashSet::new();
    let (mut leaves, mut composites, mut decisions, mut max_depth, mut dag_shared) =
        (0usize, 0usize, 0usize, 0u32, 0usize);
    let mut truncated_subtrees = 0usize;
    for node in &emitted {
        let id = node.id.as_str();
        if !node.expanded {
            truncated_subtrees += 1;
        }
        if !unique.insert(id) {
            continue;
        }
        match tree.kind(id) {
            Some(TreeKind::Composite) => composites += 1,
            Some(TreeKind::Decision) => decisions += 1,
            Some(TreeKind::Leaf) | None => {}
        }
        if !tree.is_branch(id) {
            leaves += 1;
        }
        max_depth = max_depth.max(tree.depth(id).unwrap_or(0));
        if tree.parents(id).len() > 1 {
            dag_shared += 1;
        }
    }

    // Assemble the nested JSON children-first: a child's index is always
    // greater than its parent's.
    let mut children_of: Vec<Vec<usize>> = vec![Vec::new(); emitted.len()];
    let mut root_indices: Vec<usize> = Vec::new();
    for (index, node) in emitted.iter().enumerate() {
        match node.parent {
            Some(parent) => children_of[parent].push(index),
            None => root_indices.push(index),
        }
    }
    let mut built: Vec<Option<Value>> = vec![None; emitted.len()];
    for index in (0..emitted.len()).rev() {
        let node = &emitted[index];
        let mut value = node_ctx.node_json(&node.id, params.fields.as_ref());
        let children = if node.expanded {
            Value::Array(
                children_of[index]
                    .iter()
                    .filter_map(|c| built[*c].take())
                    .collect(),
            )
        } else {
            Value::Null
        };
        value.insert("children".into(), children);
        built[index] = Some(Value::Object(value));
    }
    let roots_json: Vec<Value> = root_indices
        .into_iter()
        .filter_map(|i| built[i].take())
        .collect();

    let response = json!({
        "roots": roots_json,
        "totals": {
            "nodes": unique.len(),
            "leaves": leaves,
            "composites": composites,
            "decisions": decisions,
            "max_depth": max_depth,
            "dag_shared": dag_shared,
            "truncated_subtrees": truncated_subtrees,
        },
        "warnings": warnings,
    });
    Ok(serde_json::to_string(&response)?)
}
