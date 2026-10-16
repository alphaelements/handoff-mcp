//! Requirement-tree (decomposition DAG) model — wiki/300-requirement-tree-design.md,
//! SPEC-TREE-003 / 004 / 006: a **pure** module over [`TraceItemInput`]s.
//!
//! - [`TreeIndex::build`] turns each item's declared `parent`s into a
//!   validated DAG (`parents_of` / `children_of` / `roots_by_layer`), keeping
//!   only parents that exist, sit on the item's own layer and do not close a
//!   cycle. Everything else makes the item a root and is recorded as a
//!   [`TreeGap`]. Children and roots are in the stable order
//!   `(order.is_none(), order, doc_path, seq)`. Nothing here recurses: cycle
//!   detection is an iterative Tarjan SCC pass, and the topological order,
//!   depths and roll-up are all loops over explicit work lists, so a
//!   thousand-deep chain cannot overflow the stack.
//! - [`TreeKind`] resolution (SPEC-TREE-004) lives in the same build, because
//!   it needs the children and records the `kind_conflict` / `empty_group`
//!   gaps.
//! - [`compute_subtrees`] is the roll-up (SPEC-TREE-006): one [`Subtree`] per
//!   node, counting the *unique* leaves below it (a node shared by two
//!   parents is counted once inside any single ancestor's sub-tree, and
//!   appears in each parent's sub-tree), plus the de-duplicated project
//!   [`TreeTotals`].
//!
//! The refines / verifies evaluation (`engine.rs`) is untouched by this
//! module; `engine.rs` only *reads* the index to give composite nodes their
//! aggregated coverage and to feed [`compute_subtrees`].

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Serialize, Serializer};

use crate::storage::docs::model::TreeKindDecl;

use super::types::{ItemState, StateCounts, TraceItemInput};

/// The effective node kind of SPEC-TREE-004.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TreeKind {
    /// No children (and not declared `group` / `decision`).
    Leaf,
    /// Has children, or is declared `group`.
    Composite,
    /// Declared `decision` (a conditional branch); its children are the
    /// branch targets.
    Decision,
}

/// The tree-specific gap kinds the index records (SPEC-TREE-003 / 004).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TreeGapKind {
    /// A declared parent does not exist.
    ParentDangling,
    /// A declared parent is on another layer.
    ParentCrossLayer,
    /// The item is part of a parent cycle.
    TreeCycle,
    /// The item lists itself as a parent.
    SelfParent,
    /// `kind: group` with no children.
    EmptyGroup,
    /// `kind: leaf` on an item that has children.
    KindConflict,
}

/// One tree-structure finding. Kept apart from the V-model
/// [`super::types::Gap`] list so existing gap consumers are unaffected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TreeGap {
    pub kind: TreeGapKind,
    pub item: String,
    pub detail: String,
}

/// The decomposition DAG (SPEC-TREE-003).
#[derive(Debug, Clone, Default)]
pub struct TreeIndex {
    /// child -> its valid parents, in declaration order.
    pub parents_of: HashMap<String, Vec<String>>,
    /// parent -> its children, in the stable sibling order.
    pub children_of: HashMap<String, Vec<String>>,
    /// layer -> root items (no valid parent), in the stable sibling order.
    pub roots_by_layer: BTreeMap<String, Vec<String>>,
    kinds: HashMap<String, TreeKind>,
    /// Shortest distance from any root.
    depth: HashMap<String, u32>,
    /// Every node, children before parents.
    children_first: Vec<String>,
    gaps: Vec<TreeGap>,
    layers: HashMap<String, String>,
}

/// `(order.is_none(), order, doc_path, seq, id)` — the sibling sort key
/// (REQ-TREE-008); `id` only breaks ties between otherwise identical keys so
/// the order is total and deterministic.
type SortKey<'a> = (bool, u32, &'a str, usize, &'a str);

fn sort_key<'a>(item: &'a TraceItemInput, parent: Option<&str>) -> SortKey<'a> {
    let order = parent
        .and_then(|p| item.tree.order_per_parent.get(p))
        .copied()
        .or(item.tree.order);
    (
        order.is_none(),
        order.unwrap_or(0),
        item.tree.doc_path.as_str(),
        item.tree.seq,
        item.stable_id.as_str(),
    )
}

impl TreeIndex {
    /// Builds the index from `items` (one entry per stable id — the caller
    /// passes the first occurrence of a duplicated id, as the engine does).
    /// Items without a layer take no part in the tree.
    pub fn build(items: &[&TraceItemInput]) -> Self {
        let mut ids: Vec<&TraceItemInput> = items
            .iter()
            .copied()
            .filter(|it| it.layer.is_some())
            .collect();
        ids.sort_by(|a, b| a.stable_id.cmp(&b.stable_id));
        let position: HashMap<&str, usize> = ids
            .iter()
            .enumerate()
            .map(|(i, it)| (it.stable_id.as_str(), i))
            .collect();

        let mut gaps: Vec<TreeGap> = Vec::new();
        // Candidate edges (child -> parent), before cycle removal.
        let mut candidate: Vec<Vec<usize>> = vec![Vec::new(); ids.len()];
        for (child_idx, item) in ids.iter().enumerate() {
            let mut seen: HashSet<&str> = HashSet::new();
            for parent in &item.tree.parents {
                let parent = parent.as_str();
                if !seen.insert(parent) {
                    continue;
                }
                if parent == item.stable_id {
                    gaps.push(TreeGap {
                        kind: TreeGapKind::SelfParent,
                        item: item.stable_id.clone(),
                        detail: format!("'{}' lists itself as its own parent", item.stable_id),
                    });
                    continue;
                }
                let Some(&parent_idx) = position.get(parent) else {
                    gaps.push(TreeGap {
                        kind: TreeGapKind::ParentDangling,
                        item: item.stable_id.clone(),
                        detail: format!(
                            "parent '{parent}' of '{}' does not exist; treated as a root",
                            item.stable_id
                        ),
                    });
                    continue;
                };
                if ids[parent_idx].layer != item.layer {
                    gaps.push(TreeGap {
                        kind: TreeGapKind::ParentCrossLayer,
                        item: item.stable_id.clone(),
                        detail: format!(
                            "parent '{parent}' of '{}' is on a different layer; the parent link \
                             is ignored",
                            item.stable_id
                        ),
                    });
                    continue;
                }
                candidate[child_idx].push(parent_idx);
            }
        }

        // Drop every edge that lies inside a cycle (a strongly connected
        // component of more than one node) and report each member once.
        let components = strongly_connected_components(&candidate);
        let mut component_of = vec![usize::MAX; ids.len()];
        for (c, members) in components.iter().enumerate() {
            for &m in members {
                component_of[m] = c;
            }
        }
        for members in components.iter().filter(|c| c.len() > 1) {
            let mut names: Vec<&str> = members.iter().map(|&m| ids[m].stable_id.as_str()).collect();
            names.sort_unstable();
            let detail = format!("parent links form a cycle among: {}", names.join(", "));
            for name in &names {
                gaps.push(TreeGap {
                    kind: TreeGapKind::TreeCycle,
                    item: (*name).to_string(),
                    detail: detail.clone(),
                });
            }
        }

        let mut parents_of: HashMap<String, Vec<String>> = HashMap::new();
        let mut children_unsorted: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut root_idx: Vec<usize> = Vec::new();
        for (child_idx, parents) in candidate.iter().enumerate() {
            let mut valid = Vec::new();
            for &parent_idx in parents {
                let in_cycle = component_of[child_idx] == component_of[parent_idx]
                    && components[component_of[child_idx]].len() > 1;
                if in_cycle {
                    continue;
                }
                valid.push(parent_idx);
                children_unsorted
                    .entry(parent_idx)
                    .or_default()
                    .push(child_idx);
            }
            if valid.is_empty() {
                root_idx.push(child_idx);
            } else {
                parents_of.insert(
                    ids[child_idx].stable_id.clone(),
                    valid.iter().map(|&p| ids[p].stable_id.clone()).collect(),
                );
            }
        }

        let mut children_of: HashMap<String, Vec<String>> = HashMap::new();
        for (parent_idx, mut kids) in children_unsorted {
            let parent_id = ids[parent_idx].stable_id.as_str();
            kids.sort_by(|&a, &b| {
                sort_key(ids[a], Some(parent_id)).cmp(&sort_key(ids[b], Some(parent_id)))
            });
            children_of.insert(
                ids[parent_idx].stable_id.clone(),
                kids.iter().map(|&k| ids[k].stable_id.clone()).collect(),
            );
        }

        root_idx.sort_by(|&a, &b| sort_key(ids[a], None).cmp(&sort_key(ids[b], None)));
        let mut roots_by_layer: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for &r in &root_idx {
            if let Some(layer) = &ids[r].layer {
                roots_by_layer
                    .entry(layer.clone())
                    .or_default()
                    .push(ids[r].stable_id.clone());
            }
        }

        // Kinds (SPEC-TREE-004).
        let mut kinds = HashMap::with_capacity(ids.len());
        for item in &ids {
            let has_children = children_of
                .get(&item.stable_id)
                .is_some_and(|c| !c.is_empty());
            let kind = match item.tree.kind_decl {
                Some(TreeKindDecl::Decision) => TreeKind::Decision,
                Some(TreeKindDecl::Group) => {
                    if !has_children {
                        gaps.push(TreeGap {
                            kind: TreeGapKind::EmptyGroup,
                            item: item.stable_id.clone(),
                            detail: format!(
                                "'{}' is declared kind: group but has no children",
                                item.stable_id
                            ),
                        });
                    }
                    TreeKind::Composite
                }
                Some(TreeKindDecl::Leaf) if has_children => {
                    gaps.push(TreeGap {
                        kind: TreeGapKind::KindConflict,
                        item: item.stable_id.clone(),
                        detail: format!(
                            "'{}' is declared kind: leaf but has children; treated as composite",
                            item.stable_id
                        ),
                    });
                    TreeKind::Composite
                }
                Some(TreeKindDecl::Leaf) | None => {
                    if has_children {
                        TreeKind::Composite
                    } else {
                        TreeKind::Leaf
                    }
                }
            };
            kinds.insert(item.stable_id.clone(), kind);
        }

        let children_first = children_first_order(&ids, &parents_of, &children_of);
        let mut depth: HashMap<String, u32> = HashMap::with_capacity(ids.len());
        for id in children_first.iter().rev() {
            let d = parents_of
                .get(id)
                .map(|ps| {
                    ps.iter()
                        .filter_map(|p| depth.get(p))
                        .min()
                        .map_or(0, |d| d + 1)
                })
                .unwrap_or(0);
            depth.insert(id.clone(), d);
        }

        gaps.sort_by(|a, b| {
            (a.item.as_str(), a.kind as u8, a.detail.as_str()).cmp(&(
                b.item.as_str(),
                b.kind as u8,
                b.detail.as_str(),
            ))
        });
        let layers = ids
            .iter()
            .filter_map(|it| it.layer.clone().map(|l| (it.stable_id.clone(), l)))
            .collect();

        Self {
            parents_of,
            children_of,
            roots_by_layer,
            kinds,
            depth,
            children_first,
            gaps,
            layers,
        }
    }

    /// `true` when `id` takes part in the tree (has a layer).
    pub fn contains(&self, id: &str) -> bool {
        self.kinds.contains_key(id)
    }

    /// The valid parents of `id` (empty for a root).
    pub fn parents(&self, id: &str) -> &[String] {
        self.parents_of.get(id).map_or(&[], Vec::as_slice)
    }

    /// The children of `id` in the stable sibling order.
    pub fn children(&self, id: &str) -> &[String] {
        self.children_of.get(id).map_or(&[], Vec::as_slice)
    }

    /// `id`'s effective kind (SPEC-TREE-004).
    pub fn kind(&self, id: &str) -> Option<TreeKind> {
        self.kinds.get(id).copied()
    }

    /// `true` for a node that rolls its children up: it has children, or it
    /// is declared `group`. A childless `decision` / `leaf` is a roll-up leaf
    /// (it is the thing that gets verified), a childless `group` is an empty
    /// branch that contributes nothing.
    pub fn is_branch(&self, id: &str) -> bool {
        match self.kind(id) {
            Some(TreeKind::Composite) => true,
            Some(TreeKind::Decision) => !self.children(id).is_empty(),
            Some(TreeKind::Leaf) | None => false,
        }
    }

    /// The shortest distance from any root (a root is 0).
    pub fn depth(&self, id: &str) -> Option<u32> {
        self.depth.get(id).copied()
    }

    /// Every node, children before parents; the order is deterministic.
    pub fn children_first(&self) -> &[String] {
        &self.children_first
    }

    /// The layer of `id`.
    pub fn layer(&self, id: &str) -> Option<&str> {
        self.layers.get(id).map(String::as_str)
    }

    /// Structure findings (dangling / cross-layer / cyclic / self parents,
    /// conflicting or empty `kind` declarations), sorted by item.
    pub fn gaps(&self) -> &[TreeGap] {
        &self.gaps
    }

    /// Number of nodes that have more than one valid parent.
    pub fn dag_shared_count(&self) -> usize {
        self.parents_of.values().filter(|p| p.len() > 1).count()
    }

    /// Number of nodes in the tree.
    pub fn len(&self) -> usize {
        self.kinds.len()
    }

    /// `true` when no item takes part in the tree.
    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }
}

/// Iterative Tarjan: strongly connected components of `adj` (edges are
/// child -> parent). Every node belongs to exactly one component.
fn strongly_connected_components(adj: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let n = adj.len();
    let mut index = vec![usize::MAX; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut components: Vec<Vec<usize>> = Vec::new();
    let mut counter = 0usize;

    for root in 0..n {
        if index[root] != usize::MAX {
            continue;
        }
        let mut call_stack: Vec<(usize, usize)> = vec![(root, 0)];
        index[root] = counter;
        low[root] = counter;
        counter += 1;
        stack.push(root);
        on_stack[root] = true;

        while let Some(&(v, edge)) = call_stack.last() {
            if edge < adj[v].len() {
                call_stack.last_mut().expect("non-empty").1 += 1;
                let w = adj[v][edge];
                if index[w] == usize::MAX {
                    index[w] = counter;
                    low[w] = counter;
                    counter += 1;
                    stack.push(w);
                    on_stack[w] = true;
                    call_stack.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
            } else {
                call_stack.pop();
                if let Some(&(parent, _)) = call_stack.last() {
                    low[parent] = low[parent].min(low[v]);
                }
                if low[v] == index[v] {
                    let mut component = Vec::new();
                    while let Some(w) = stack.pop() {
                        on_stack[w] = false;
                        component.push(w);
                        if w == v {
                            break;
                        }
                    }
                    components.push(component);
                }
            }
        }
    }
    components
}

/// Kahn's algorithm from the leaves up: a node is emitted once all of its
/// children have been. The queue is seeded and fed in id order so the result
/// is deterministic.
fn children_first_order(
    ids: &[&TraceItemInput],
    parents_of: &HashMap<String, Vec<String>>,
    children_of: &HashMap<String, Vec<String>>,
) -> Vec<String> {
    let mut remaining_children: HashMap<&str, usize> = HashMap::with_capacity(ids.len());
    let mut queue: std::collections::VecDeque<&str> = std::collections::VecDeque::new();
    for item in ids {
        let n = children_of.get(&item.stable_id).map_or(0, Vec::len);
        remaining_children.insert(item.stable_id.as_str(), n);
        if n == 0 {
            queue.push_back(item.stable_id.as_str());
        }
    }
    let mut order = Vec::with_capacity(ids.len());
    while let Some(id) = queue.pop_front() {
        order.push(id.to_string());
        if let Some(parents) = parents_of.get(id) {
            for parent in parents {
                if let Some(n) = remaining_children.get_mut(parent.as_str()) {
                    *n -= 1;
                    if *n == 0 {
                        queue.push_back(parent.as_str());
                    }
                }
            }
        }
    }
    order
}

// ---------------------------------------------------------------------
// Roll-up (SPEC-TREE-006)
// ---------------------------------------------------------------------

/// Why a leaf is left out of the state counts and the progress ratios.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeafExclusion {
    /// Horizontal coverage is `waived`.
    Waived,
    /// Horizontal coverage is `na`, or the item is outside its effective
    /// scope (no state is derived for it).
    Na,
}

/// The per-item facts the roll-up counts; produced by the engine.
#[derive(Debug, Clone)]
pub struct TreeNodeFacts {
    /// The item's verification state; `None` when it has none (excluded).
    pub state: Option<ItemState>,
    pub exclusion: Option<LeafExclusion>,
    /// `SubItem.dev_stage` (`None` = `not_started`).
    pub dev_stage: Option<String>,
    /// `draft` | `review` | `approved` (anything else counts as `draft`).
    pub approval: String,
    /// The item carries at least one suspect of any kind.
    pub suspect: bool,
    /// Task ids linked directly to this item, sorted and unique.
    pub tasks: Vec<String>,
}

/// `dev_stage` counts of a sub-tree's leaves.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct StageCounts {
    pub not_started: usize,
    pub in_progress: usize,
    pub implemented: usize,
    pub tested: usize,
}

/// `approval` counts of a sub-tree's leaves.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ApprovalCounts {
    pub draft: usize,
    pub review: usize,
    pub approved: usize,
}

/// Leaves left out of the state counts and the progress ratios.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ExcludedCounts {
    pub waived: usize,
    pub na: usize,
}

fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

fn serialize_ratio<S: Serializer>(value: &Option<f64>, serializer: S) -> Result<S::Ok, S::Error> {
    match value {
        Some(v) => serializer.serialize_f64(round4(*v)),
        None => serializer.serialize_none(),
    }
}

/// `verified_ratio` / `implemented_ratio`; `None` when no leaf is left to
/// count. Held as a full-precision `f64`; serialized rounded to 4 decimals.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Progress {
    #[serde(serialize_with = "serialize_ratio")]
    pub verified_ratio: Option<f64>,
    #[serde(serialize_with = "serialize_ratio")]
    pub implemented_ratio: Option<f64>,
}

/// One node's sub-tree roll-up.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Subtree {
    pub depth: u32,
    pub parents: Vec<String>,
    pub children: Vec<String>,
    pub leaves: usize,
    pub state: StateCounts,
    pub dev_stage: StageCounts,
    pub approval: ApprovalCounts,
    pub excluded: ExcludedCounts,
    /// Nodes in the sub-tree (the node itself included) with a suspect.
    pub suspect: usize,
    pub progress: Progress,
    /// Tasks linked directly to this node.
    pub tasks: Vec<String>,
    /// Unique tasks linked to any node of the sub-tree (the node included).
    pub all_tasks: usize,
}

/// Project-wide, duplicate-free aggregate (a node shared by several parents
/// is counted once).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TreeTotals {
    pub nodes: usize,
    /// Roll-up leaves (see [`TreeIndex::is_branch`]).
    pub leaves: usize,
    pub composites: usize,
    pub decisions: usize,
    pub max_depth: u32,
    pub dag_shared: usize,
    pub state: StateCounts,
    pub progress: Progress,
}

/// The roll-up result.
#[derive(Debug, Clone, Default)]
pub struct TreeRollup {
    pub subtrees: HashMap<String, Subtree>,
    pub totals: TreeTotals,
}

#[derive(Default, Clone)]
struct LeafTally {
    leaves: usize,
    state: StateCounts,
    dev_stage: StageCounts,
    approval: ApprovalCounts,
    excluded: ExcludedCounts,
    verified: usize,
    implemented: usize,
}

impl LeafTally {
    /// The tally of the single leaf described by `facts`.
    fn of_leaf(facts: &TreeNodeFacts) -> Self {
        let mut tally = Self::default();
        tally.add(facts);
        tally
    }

    fn add(&mut self, facts: &TreeNodeFacts) {
        self.leaves += 1;
        let stage = facts.dev_stage.as_deref().unwrap_or("not_started");
        let implemented = matches!(stage, "implemented" | "tested" | "verified");
        match stage {
            "in_progress" => self.dev_stage.in_progress += 1,
            "implemented" => self.dev_stage.implemented += 1,
            "tested" | "verified" => self.dev_stage.tested += 1,
            _ => self.dev_stage.not_started += 1,
        }
        match facts.approval.as_str() {
            "review" => self.approval.review += 1,
            "approved" => self.approval.approved += 1,
            _ => self.approval.draft += 1,
        }
        let exclusion = facts.exclusion.or(match facts.state {
            Some(_) => None,
            None => Some(LeafExclusion::Na),
        });
        match exclusion {
            Some(LeafExclusion::Waived) => self.excluded.waived += 1,
            Some(LeafExclusion::Na) => self.excluded.na += 1,
            None => {
                if let Some(state) = facts.state {
                    self.state.record(state);
                    if state == ItemState::Passing {
                        self.verified += 1;
                    }
                }
                if implemented {
                    self.implemented += 1;
                }
            }
        }
    }

    fn merge(&mut self, other: &Self) {
        self.leaves += other.leaves;
        self.state.passing += other.state.passing;
        self.state.failing += other.state.failing;
        self.state.blocked += other.state.blocked;
        self.state.not_run += other.state.not_run;
        self.state.uncovered += other.state.uncovered;
        self.dev_stage.not_started += other.dev_stage.not_started;
        self.dev_stage.in_progress += other.dev_stage.in_progress;
        self.dev_stage.implemented += other.dev_stage.implemented;
        self.dev_stage.tested += other.dev_stage.tested;
        self.approval.draft += other.approval.draft;
        self.approval.review += other.approval.review;
        self.approval.approved += other.approval.approved;
        self.excluded.waived += other.excluded.waived;
        self.excluded.na += other.excluded.na;
        self.verified += other.verified;
        self.implemented += other.implemented;
    }

    fn progress(&self) -> Progress {
        let denominator = self.leaves - self.excluded.waived - self.excluded.na;
        if denominator == 0 {
            return Progress::default();
        }
        Progress {
            verified_ratio: Some(self.verified as f64 / denominator as f64),
            implemented_ratio: Some(self.implemented as f64 / denominator as f64),
        }
    }
}

/// Rolls the leaves up into one [`Subtree`] per node plus the
/// duplicate-free [`TreeTotals`] (SPEC-TREE-006). `facts` must hold an entry
/// for every node of `index`; a node without one is skipped.
pub fn compute_subtrees(index: &TreeIndex, facts: &HashMap<String, TreeNodeFacts>) -> TreeRollup {
    let ids: Vec<&str> = index.children_first().iter().map(String::as_str).collect();
    let position: HashMap<&str, usize> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    let child_positions: Vec<Vec<usize>> = ids
        .iter()
        .map(|id| {
            index
                .children(id)
                .iter()
                .filter_map(|c| position.get(c.as_str()).copied())
                .collect()
        })
        .collect();
    // Per-node lookups resolved once, so the walks below touch only arrays.
    let node_facts: Vec<Option<&TreeNodeFacts>> = ids.iter().map(|id| facts.get(*id)).collect();
    let leaf_tally: Vec<Option<LeafTally>> = ids
        .iter()
        .zip(&node_facts)
        .map(|(id, f)| match f {
            Some(f) if !index.is_branch(id) => Some(LeafTally::of_leaf(f)),
            _ => None,
        })
        .collect();

    let mut rollup = TreeRollup::default();
    // `seen[j] == stamp` marks node j as visited by the current walk.
    let mut seen = vec![0usize; ids.len()];
    let mut stamp = 0usize;
    let mut stack: Vec<usize> = Vec::new();

    for (root, id) in ids.iter().enumerate() {
        let Some(own) = node_facts[root] else {
            continue;
        };
        stamp += 1;
        stack.clear();
        stack.push(root);
        seen[root] = stamp;
        let mut tally = LeafTally::default();
        let mut suspect = 0usize;
        let mut tasks: HashSet<&str> = HashSet::new();
        while let Some(node) = stack.pop() {
            if let Some(node_facts) = node_facts[node] {
                if node_facts.suspect {
                    suspect += 1;
                }
                tasks.extend(node_facts.tasks.iter().map(String::as_str));
            }
            if let Some(leaf) = &leaf_tally[node] {
                tally.merge(leaf);
            }
            for &child in &child_positions[node] {
                if seen[child] != stamp {
                    seen[child] = stamp;
                    stack.push(child);
                }
            }
        }
        rollup.subtrees.insert(
            (*id).to_string(),
            Subtree {
                depth: index.depth(id).unwrap_or(0),
                parents: index.parents(id).to_vec(),
                children: index.children(id).to_vec(),
                leaves: tally.leaves,
                state: tally.state,
                dev_stage: tally.dev_stage,
                approval: tally.approval,
                excluded: tally.excluded,
                suspect,
                progress: tally.progress(),
                tasks: own.tasks.clone(),
                all_tasks: tasks.len(),
            },
        );
    }

    let mut totals_tally = LeafTally::default();
    let mut totals = TreeTotals {
        nodes: ids.len(),
        dag_shared: index.dag_shared_count(),
        ..TreeTotals::default()
    };
    for (i, id) in ids.iter().enumerate() {
        match index.kind(id) {
            Some(TreeKind::Composite) => totals.composites += 1,
            Some(TreeKind::Decision) => totals.decisions += 1,
            Some(TreeKind::Leaf) | None => {}
        }
        totals.max_depth = totals.max_depth.max(index.depth(id).unwrap_or(0));
        if let Some(leaf) = &leaf_tally[i] {
            totals_tally.merge(leaf);
        }
    }
    totals.leaves = totals_tally.leaves;
    totals.state = totals_tally.state;
    totals.progress = totals_tally.progress();
    rollup.totals = totals;
    rollup
}

#[cfg(test)]
mod tests;
