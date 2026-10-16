//! Unit tests for the requirement-tree index and roll-up
//! (SPEC-TREE-003 / 004 / 006).

use super::*;
use crate::trace::types::TreeItemInput;

fn item(id: &str, layer: &str, parents: &[&str]) -> TraceItemInput {
    TraceItemInput {
        stable_id: id.to_string(),
        doc_id: "doc".to_string(),
        layer: Some(layer.to_string()),
        refines: Vec::new(),
        verifies: Vec::new(),
        method: None,
        has_test_refs: false,
        acceptance_labels: Vec::new(),
        derived: false,
        waived_axes: Vec::new(),
        def_hash: None,
        body_hash: None,
        link_baselines: Default::default(),
        needs: None,
        approval: "draft".to_string(),
        tree: TreeItemInput {
            parents: parents.iter().map(|p| p.to_string()).collect(),
            doc_path: "doc".to_string(),
            ..Default::default()
        },
    }
}

fn located(mut it: TraceItemInput, doc_path: &str, seq: usize) -> TraceItemInput {
    it.tree.doc_path = doc_path.to_string();
    it.tree.seq = seq;
    it
}

fn ordered(mut it: TraceItemInput, order: u32) -> TraceItemInput {
    it.tree.order = Some(order);
    it
}

fn declared(mut it: TraceItemInput, decl: TreeKindDecl) -> TraceItemInput {
    it.tree.kind_decl = Some(decl);
    it
}

fn labelled(mut it: TraceItemInput, label: &str) -> TraceItemInput {
    it.tree.condition = Some(label.to_string());
    it
}

fn labelled_for(mut it: TraceItemInput, parent: &str, label: &str) -> TraceItemInput {
    it.tree
        .condition_per_parent
        .insert(parent.to_string(), label.to_string());
    it
}

fn gaps_of(index: &TreeIndex, kind: TreeGapKind) -> Vec<(&str, &str)> {
    index
        .gaps()
        .iter()
        .filter(|g| g.kind == kind)
        .map(|g| (g.item.as_str(), g.detail.as_str()))
        .collect()
}

fn build(items: &[TraceItemInput]) -> TreeIndex {
    let refs: Vec<&TraceItemInput> = items.iter().collect();
    TreeIndex::build(&refs)
}

fn strings(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

fn gap_kinds(index: &TreeIndex) -> Vec<(TreeGapKind, &str)> {
    index
        .gaps()
        .iter()
        .map(|g| (g.kind, g.item.as_str()))
        .collect()
}

// -- index ------------------------------------------------------------

#[test]
fn children_are_ordered_by_order_then_doc_path_then_seq() {
    let items = vec![
        item("REQ-A", "requirement", &[]),
        ordered(
            located(item("REQ-B", "requirement", &["REQ-A"]), "z-doc", 0),
            2,
        ),
        ordered(
            located(item("REQ-C", "requirement", &["REQ-A"]), "z-doc", 1),
            1,
        ),
        located(item("REQ-D", "requirement", &["REQ-A"]), "a-doc", 1),
        located(item("REQ-E", "requirement", &["REQ-A"]), "a-doc", 0),
    ];
    let index = build(&items);
    assert_eq!(
        strings(index.children("REQ-A")),
        vec!["REQ-C", "REQ-B", "REQ-E", "REQ-D"],
        "ordered siblings first (ascending), then document path, then seq"
    );
}

#[test]
fn a_per_parent_order_overrides_the_global_order_under_that_parent_only() {
    let mut shared = item("REQ-X", "requirement", &["REQ-A", "REQ-B"]);
    shared.tree.order = Some(5);
    shared.tree.order_per_parent.insert("REQ-B".into(), 1);
    let items = vec![
        item("REQ-A", "requirement", &[]),
        item("REQ-B", "requirement", &[]),
        ordered(item("REQ-Y", "requirement", &["REQ-A"]), 3),
        ordered(item("REQ-Z", "requirement", &["REQ-B"]), 2),
        shared,
    ];
    let index = build(&items);
    assert_eq!(strings(index.children("REQ-A")), vec!["REQ-Y", "REQ-X"]);
    assert_eq!(strings(index.children("REQ-B")), vec!["REQ-X", "REQ-Z"]);
    assert_eq!(strings(index.parents("REQ-X")), vec!["REQ-A", "REQ-B"]);
    assert_eq!(index.dag_shared_count(), 1);
}

#[test]
fn roots_are_grouped_by_layer_in_the_stable_order() {
    let items = vec![
        item("REQ-B", "requirement", &[]),
        item("REQ-A", "requirement", &[]),
        ordered(item("REQ-Z", "requirement", &[]), 1),
        item("SPEC-A", "basic_spec", &[]),
        item("REQ-C", "requirement", &["REQ-A"]),
    ];
    let index = build(&items);
    assert_eq!(
        strings(&index.roots_by_layer["requirement"]),
        vec!["REQ-Z", "REQ-A", "REQ-B"]
    );
    assert_eq!(strings(&index.roots_by_layer["basic_spec"]), vec!["SPEC-A"]);
    assert!(!index.roots_by_layer["requirement"].contains(&"REQ-C".to_string()));
}

#[test]
fn invalid_parents_make_the_item_a_root_and_record_a_gap() {
    let items = vec![
        item("REQ-A", "requirement", &[]),
        item("SPEC-A", "basic_spec", &[]),
        item("REQ-1", "requirement", &["REQ-404"]),
        item("REQ-2", "requirement", &["SPEC-A"]),
        item("REQ-3", "requirement", &["REQ-3"]),
        item("REQ-4", "requirement", &["REQ-A", "REQ-404"]),
    ];
    let index = build(&items);
    for root in ["REQ-1", "REQ-2", "REQ-3"] {
        assert!(index.parents(root).is_empty(), "{root} is a root");
        assert!(index.roots_by_layer["requirement"].contains(&root.to_string()));
    }
    // A partially valid declaration keeps its valid parent.
    assert_eq!(strings(index.parents("REQ-4")), vec!["REQ-A"]);
    assert_eq!(
        gap_kinds(&index),
        vec![
            (TreeGapKind::ParentDangling, "REQ-1"),
            (TreeGapKind::ParentCrossLayer, "REQ-2"),
            (TreeGapKind::SelfParent, "REQ-3"),
            (TreeGapKind::ParentDangling, "REQ-4"),
        ]
    );
}

#[test]
fn cycles_including_multi_parent_ones_are_cut_and_reported_per_member() {
    // A <- B <- C, with C also declaring A as a second parent route:
    // A's parent is C closes A -> C -> B -> A (through two parents of C).
    let items = vec![
        item("REQ-A", "requirement", &["REQ-C"]),
        item("REQ-B", "requirement", &["REQ-A", "REQ-R"]),
        item("REQ-C", "requirement", &["REQ-B", "REQ-R"]),
        item("REQ-R", "requirement", &[]),
        item("REQ-OK", "requirement", &["REQ-R"]),
    ];
    let index = build(&items);
    let cyclic: Vec<&str> = index
        .gaps()
        .iter()
        .filter(|g| g.kind == TreeGapKind::TreeCycle)
        .map(|g| g.item.as_str())
        .collect();
    assert_eq!(cyclic, vec!["REQ-A", "REQ-B", "REQ-C"]);
    // Edges inside the cycle are gone; edges to outside nodes survive.
    assert!(index.parents("REQ-A").is_empty());
    assert_eq!(strings(index.parents("REQ-B")), vec!["REQ-R"]);
    assert_eq!(strings(index.parents("REQ-C")), vec!["REQ-R"]);
    assert_eq!(
        strings(index.children("REQ-R")),
        vec!["REQ-B", "REQ-C", "REQ-OK"]
    );
    assert_eq!(index.len(), 5, "every node is still in the tree");
    assert_eq!(
        index.children_first().len(),
        5,
        "the order covers every node"
    );
}

#[test]
fn items_without_a_layer_take_no_part_in_the_tree() {
    let mut layerless = item("NOTE-1", "requirement", &["REQ-A"]);
    layerless.layer = None;
    let items = vec![item("REQ-A", "requirement", &[]), layerless];
    let index = build(&items);
    assert!(!index.contains("NOTE-1"));
    assert!(index.children("REQ-A").is_empty());
}

#[test]
fn depth_is_the_shortest_distance_from_any_root_and_chain_order_is_children_first() {
    // REQ-D is reachable via A -> B -> D (depth 2) and A -> D (depth 1).
    let items = vec![
        item("REQ-A", "requirement", &[]),
        item("REQ-B", "requirement", &["REQ-A"]),
        item("REQ-D", "requirement", &["REQ-B", "REQ-A"]),
    ];
    let index = build(&items);
    assert_eq!(index.depth("REQ-A"), Some(0));
    assert_eq!(index.depth("REQ-B"), Some(1));
    assert_eq!(index.depth("REQ-D"), Some(1));
    let order = index.children_first();
    let pos = |id: &str| order.iter().position(|x| x == id).unwrap();
    assert!(pos("REQ-D") < pos("REQ-B") && pos("REQ-B") < pos("REQ-A"));
}

#[test]
fn a_very_deep_chain_builds_and_rolls_up_without_recursion() {
    let depth = 5_000;
    let mut items = vec![item("REQ-0", "requirement", &[])];
    for n in 1..depth {
        let parent = format!("REQ-{}", n - 1);
        items.push(item(&format!("REQ-{n}"), "requirement", &[parent.as_str()]));
    }
    let index = build(&items);
    assert_eq!(
        index.depth(&format!("REQ-{}", depth - 1)),
        Some(depth as u32 - 1)
    );
    let facts = items
        .iter()
        .map(|it| (it.stable_id.clone(), facts(Some(ItemState::Passing))))
        .collect();
    let rollup = compute_subtrees(&index, &facts);
    assert_eq!(
        rollup.subtrees["REQ-0"].leaves, 1,
        "only the last node is a leaf"
    );
    assert_eq!(rollup.totals.max_depth, depth as u32 - 1);
}

// -- kinds ------------------------------------------------------------

#[test]
fn kinds_follow_children_and_declarations() {
    let items = vec![
        item("REQ-P", "requirement", &[]),
        item("REQ-C", "requirement", &["REQ-P"]),
        item("REQ-L", "requirement", &[]),
        declared(item("REQ-G", "requirement", &[]), TreeKindDecl::Group),
        declared(
            item("REQ-LK", "requirement", &["REQ-P"]),
            TreeKindDecl::Leaf,
        ),
        declared(item("REQ-LC", "requirement", &[]), TreeKindDecl::Leaf),
        item("REQ-LCC", "requirement", &["REQ-LC"]),
        declared(item("REQ-D", "requirement", &[]), TreeKindDecl::Decision),
        declared(item("REQ-DC", "requirement", &[]), TreeKindDecl::Decision),
        item("REQ-DCC", "requirement", &["REQ-DC"]),
    ];
    let index = build(&items);
    let kind = |id: &str| index.kind(id).unwrap();
    assert_eq!(kind("REQ-P"), TreeKind::Composite);
    assert_eq!(kind("REQ-C"), TreeKind::Leaf);
    assert_eq!(kind("REQ-L"), TreeKind::Leaf);
    assert_eq!(kind("REQ-G"), TreeKind::Composite, "group without children");
    assert_eq!(
        kind("REQ-LK"),
        TreeKind::Leaf,
        "leaf declaration, no children"
    );
    assert_eq!(
        kind("REQ-LC"),
        TreeKind::Composite,
        "leaf declaration with children"
    );
    assert_eq!(
        kind("REQ-D"),
        TreeKind::Decision,
        "childless decision stays decision"
    );
    assert_eq!(
        kind("REQ-DC"),
        TreeKind::Decision,
        "decision with children stays decision"
    );
    assert_eq!(
        gap_kinds(&index),
        vec![
            (TreeGapKind::EmptyBranch, "REQ-D"),
            (TreeGapKind::ConditionMissing, "REQ-DCC"),
            (TreeGapKind::EmptyGroup, "REQ-G"),
            (TreeGapKind::KindConflict, "REQ-LC"),
        ],
        "a childless decision gets empty_branch, never empty_group"
    );
    // Roll-up leaves: a childless group is an empty branch, a childless
    // decision is still a leaf, a decision with children is a branch.
    assert!(index.is_branch("REQ-G"));
    assert!(!index.is_branch("REQ-D"));
    assert!(index.is_branch("REQ-DC"));
    assert!(!index.is_branch("REQ-C"));
}

// -- roll-up ----------------------------------------------------------

fn facts(state: Option<ItemState>) -> TreeNodeFacts {
    TreeNodeFacts {
        state,
        exclusion: None,
        dev_stage: None,
        approval: "draft".to_string(),
        suspect: false,
        tasks: Vec::new(),
    }
}

fn facts_map(entries: Vec<(&str, TreeNodeFacts)>) -> HashMap<String, TreeNodeFacts> {
    entries
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
}

#[test]
fn rollup_counts_unique_leaves_states_stages_and_approval() {
    let items = vec![
        item("REQ-A", "requirement", &[]),
        item("REQ-1", "requirement", &["REQ-A"]),
        item("REQ-2", "requirement", &["REQ-A"]),
        item("REQ-3", "requirement", &["REQ-A"]),
        item("REQ-4", "requirement", &["REQ-A"]),
    ];
    let index = build(&items);
    let mut f1 = facts(Some(ItemState::Passing));
    f1.dev_stage = Some("tested".into());
    f1.approval = "approved".into();
    let mut f2 = facts(Some(ItemState::Passing));
    f2.dev_stage = Some("implemented".into());
    f2.approval = "review".into();
    let mut f3 = facts(Some(ItemState::Failing));
    f3.dev_stage = Some("in_progress".into());
    let f4 = facts(Some(ItemState::Uncovered));
    let rollup = compute_subtrees(
        &index,
        &facts_map(vec![
            ("REQ-A", facts(Some(ItemState::Failing))),
            ("REQ-1", f1),
            ("REQ-2", f2),
            ("REQ-3", f3),
            ("REQ-4", f4),
        ]),
    );
    let a = &rollup.subtrees["REQ-A"];
    assert_eq!(a.leaves, 4, "the composite's own state is not counted");
    assert_eq!(
        a.state,
        StateCounts {
            passing: 2,
            failing: 1,
            blocked: 0,
            not_run: 0,
            uncovered: 1
        }
    );
    assert_eq!(
        a.dev_stage,
        StageCounts {
            not_started: 1,
            in_progress: 1,
            implemented: 1,
            tested: 1
        }
    );
    assert_eq!(
        a.approval,
        ApprovalCounts {
            draft: 2,
            review: 1,
            approved: 1
        }
    );
    assert_eq!(a.depth, 0);
    assert_eq!(
        strings(&a.children),
        vec!["REQ-1", "REQ-2", "REQ-3", "REQ-4"]
    );
    assert!(a.parents.is_empty());
    assert_eq!(a.progress.verified_ratio, Some(0.5));
    assert_eq!(a.progress.implemented_ratio, Some(0.5));
    // A leaf's own sub-tree is itself.
    let leaf = &rollup.subtrees["REQ-3"];
    assert_eq!(leaf.leaves, 1);
    assert_eq!(leaf.state.failing, 1);
    assert_eq!(leaf.depth, 1);
    assert_eq!(strings(&leaf.parents), vec!["REQ-A"]);
    assert!(leaf.children.is_empty());
}

#[test]
fn waived_and_na_leaves_are_excluded_from_states_and_ratios() {
    let items = vec![
        item("REQ-A", "requirement", &[]),
        item("REQ-1", "requirement", &["REQ-A"]),
        item("REQ-2", "requirement", &["REQ-A"]),
        item("REQ-3", "requirement", &["REQ-A"]),
        item("REQ-4", "requirement", &["REQ-A"]),
        item("REQ-5", "requirement", &["REQ-A"]),
    ];
    let index = build(&items);
    let mut waived = facts(Some(ItemState::Uncovered));
    waived.exclusion = Some(LeafExclusion::Waived);
    waived.dev_stage = Some("tested".into());
    let mut na = facts(Some(ItemState::Uncovered));
    na.exclusion = Some(LeafExclusion::Na);
    let mut passing = facts(Some(ItemState::Passing));
    passing.dev_stage = Some("implemented".into());
    let mut tested = facts(Some(ItemState::NotRun));
    tested.dev_stage = Some("tested".into());
    let no_state = facts(None); // out of scope: counts as excluded na
    let rollup = compute_subtrees(
        &index,
        &facts_map(vec![
            ("REQ-A", facts(None)),
            ("REQ-1", waived),
            ("REQ-2", na),
            ("REQ-3", passing),
            ("REQ-4", tested),
            ("REQ-5", no_state),
        ]),
    );
    let a = &rollup.subtrees["REQ-A"];
    assert_eq!(a.leaves, 5);
    assert_eq!(a.excluded, ExcludedCounts { waived: 1, na: 2 });
    assert_eq!(a.state.passing, 1);
    assert_eq!(a.state.not_run, 1);
    assert_eq!(
        a.state.uncovered, 0,
        "excluded leaves are not in the state counts"
    );
    // Denominator 5 - 3 = 2; the waived leaf's `tested` does not count.
    assert_eq!(a.progress.verified_ratio, Some(0.5));
    assert_eq!(a.progress.implemented_ratio, Some(1.0));
    // ... but the stage counts still include every leaf.
    assert_eq!(a.dev_stage.tested, 2);
}

#[test]
fn progress_is_none_without_countable_leaves_and_serializes_rounded() {
    let items = vec![
        declared(item("REQ-G", "requirement", &[]), TreeKindDecl::Group),
        item("REQ-W", "requirement", &[]),
        item("REQ-P", "requirement", &[]),
        item("REQ-1", "requirement", &["REQ-P"]),
        item("REQ-2", "requirement", &["REQ-P"]),
        item("REQ-3", "requirement", &["REQ-P"]),
    ];
    let index = build(&items);
    let mut waived = facts(None);
    waived.exclusion = Some(LeafExclusion::Waived);
    let rollup = compute_subtrees(
        &index,
        &facts_map(vec![
            ("REQ-G", facts(None)),
            ("REQ-W", waived),
            ("REQ-P", facts(None)),
            ("REQ-1", facts(Some(ItemState::Passing))),
            ("REQ-2", facts(Some(ItemState::NotRun))),
            ("REQ-3", facts(Some(ItemState::NotRun))),
        ]),
    );
    let empty_group = &rollup.subtrees["REQ-G"];
    assert_eq!(empty_group.leaves, 0);
    assert_eq!(empty_group.progress, Progress::default());
    let all_waived = &rollup.subtrees["REQ-W"];
    assert_eq!(all_waived.progress.verified_ratio, None);
    assert_eq!(all_waived.progress.implemented_ratio, None);

    let p = &rollup.subtrees["REQ-P"];
    let third = p.progress.verified_ratio.unwrap();
    assert!((third - 1.0 / 3.0).abs() < 1e-12, "held at full precision");
    let json = serde_json::to_value(p).unwrap();
    assert_eq!(json["progress"]["verified_ratio"], 0.3333);
    assert_eq!(json["progress"]["implemented_ratio"], 0.0);
    assert!(json["progress"]["implemented_ratio"].is_number());
    let none_json = serde_json::to_value(all_waived).unwrap();
    assert!(none_json["progress"]["verified_ratio"].is_null());
}

#[test]
fn a_shared_node_is_counted_in_every_parent_but_once_per_ancestor_and_in_totals() {
    //   ROOT ─ A ─ X(shared) ─ L1, L2
    //     └──── B ─ X
    let items = vec![
        item("REQ-ROOT", "requirement", &[]),
        item("REQ-A", "requirement", &["REQ-ROOT"]),
        item("REQ-B", "requirement", &["REQ-ROOT"]),
        item("REQ-X", "requirement", &["REQ-A", "REQ-B"]),
        item("REQ-L1", "requirement", &["REQ-X"]),
        item("REQ-L2", "requirement", &["REQ-X"]),
    ];
    let index = build(&items);
    let entries: Vec<(&str, TreeNodeFacts)> = items
        .iter()
        .map(|it| {
            let leaf = it.stable_id.starts_with("REQ-L");
            let mut f = facts(if leaf { Some(ItemState::Passing) } else { None });
            if leaf {
                f.tasks = vec![format!("t-{}", it.stable_id)];
            }
            if it.stable_id == "REQ-X" {
                f.suspect = true;
                f.tasks = vec!["t-x".to_string()];
            }
            (it.stable_id.as_str(), f)
        })
        .collect();
    let rollup = compute_subtrees(&index, &facts_map(entries));
    // Each direct parent of the shared node sees its whole sub-tree.
    assert_eq!(rollup.subtrees["REQ-A"].leaves, 2);
    assert_eq!(rollup.subtrees["REQ-B"].leaves, 2);
    // The common ancestor reaches the shared node twice but counts it once.
    let root = &rollup.subtrees["REQ-ROOT"];
    assert_eq!(root.leaves, 2, "unique leaves below the root");
    assert_eq!(root.suspect, 1, "the shared suspect node is counted once");
    assert_eq!(root.all_tasks, 3, "t-x, t-REQ-L1, t-REQ-L2 once each");
    assert!(root.tasks.is_empty());
    assert_eq!(rollup.subtrees["REQ-X"].tasks, vec!["t-x".to_string()]);
    assert_eq!(rollup.subtrees["REQ-X"].all_tasks, 3);
    // Totals are de-duplicated project-wide.
    let totals = &rollup.totals;
    assert_eq!(totals.nodes, 6);
    assert_eq!(totals.leaves, 2);
    assert_eq!(totals.composites, 4);
    assert_eq!(totals.decisions, 0);
    assert_eq!(totals.dag_shared, 1);
    assert_eq!(totals.max_depth, 3);
    assert_eq!(totals.state.passing, 2);
    assert_eq!(totals.progress.verified_ratio, Some(1.0));
}

#[test]
fn totals_count_decisions_and_childless_decisions_as_leaves() {
    let items = vec![
        declared(item("REQ-D", "requirement", &[]), TreeKindDecl::Decision),
        item("REQ-Y", "requirement", &["REQ-D"]),
        declared(item("REQ-D2", "requirement", &[]), TreeKindDecl::Decision),
    ];
    let index = build(&items);
    let rollup = compute_subtrees(
        &index,
        &facts_map(vec![
            ("REQ-D", facts(None)),
            ("REQ-Y", facts(Some(ItemState::Passing))),
            ("REQ-D2", facts(Some(ItemState::NotRun))),
        ]),
    );
    assert_eq!(rollup.totals.decisions, 2);
    assert_eq!(
        rollup.totals.leaves, 2,
        "REQ-Y and the childless decision REQ-D2"
    );
    assert_eq!(rollup.subtrees["REQ-D"].leaves, 1);
    assert_eq!(rollup.subtrees["REQ-D2"].leaves, 1);
}

#[test]
fn an_isolated_item_is_a_depth_zero_leaf_with_one_leaf() {
    let items = vec![item("REQ-1", "requirement", &[])];
    let index = build(&items);
    let rollup = compute_subtrees(
        &index,
        &facts_map(vec![("REQ-1", facts(Some(ItemState::NotRun)))]),
    );
    let s = &rollup.subtrees["REQ-1"];
    assert_eq!((s.depth, s.leaves, s.state.not_run), (0, 1, 1));
    assert_eq!(index.kind("REQ-1"), Some(TreeKind::Leaf));
}

// -- select kind and branch conditions ---------------------------------

#[test]
fn a_select_is_a_branch_only_when_it_has_children() {
    let items = vec![
        declared(item("REQ-S", "requirement", &[]), TreeKindDecl::Select),
        labelled(item("REQ-C", "requirement", &["REQ-S"]), "a"),
        declared(item("REQ-S0", "requirement", &[]), TreeKindDecl::Select),
    ];
    let index = build(&items);
    assert_eq!(index.kind("REQ-S"), Some(TreeKind::Select));
    assert!(index.is_branch("REQ-S"));
    assert!(!index.is_branch("REQ-S0"), "childless select is a leaf");
    let rollup = compute_subtrees(&index, &facts_map(vec![]));
    assert_eq!(rollup.totals.selects, 2);
    assert_eq!(rollup.totals.decisions, 0);
    assert_eq!(rollup.totals.composites, 0);
}

#[test]
fn a_label_is_resolved_per_edge_and_per_parent_wins_over_the_default() {
    let items = vec![
        declared(item("REQ-A", "requirement", &[]), TreeKindDecl::Decision),
        declared(item("REQ-B", "requirement", &[]), TreeKindDecl::Select),
        declared(item("REQ-G", "requirement", &[]), TreeKindDecl::Group),
        labelled_for(
            labelled_for(
                item("REQ-X", "requirement", &["REQ-A", "REQ-B"]),
                "REQ-A",
                "yes",
            ),
            "REQ-B",
            "case:2",
        ),
        labelled_for(
            labelled(
                item("REQ-Y", "requirement", &["REQ-A", "REQ-B", "REQ-G"]),
                "dflt",
            ),
            "REQ-B",
            "other",
        ),
        labelled(item("REQ-Z", "requirement", &["REQ-G"]), "ignored"),
    ];
    let index = build(&items);
    let x = index.conditions("REQ-X").unwrap();
    assert_eq!(x.get("REQ-A").map(String::as_str), Some("yes"));
    assert_eq!(x.get("REQ-B").map(String::as_str), Some("case:2"));
    assert_eq!(index.condition("REQ-A", "REQ-X"), Some("yes"));
    assert_eq!(index.condition("REQ-B", "REQ-X"), Some("case:2"));
    // default fills A, the per-parent entry wins for B, the group edge has none.
    assert_eq!(index.condition("REQ-A", "REQ-Y"), Some("dflt"));
    assert_eq!(index.condition("REQ-B", "REQ-Y"), Some("other"));
    assert_eq!(index.condition("REQ-G", "REQ-Y"), None);
    assert_eq!(index.conditions("REQ-Y").unwrap().len(), 2);
    // A non-branch parent never carries a label; neither does an unlabelled edge.
    assert_eq!(index.conditions("REQ-Z"), None);
    assert_eq!(index.condition("REQ-G", "REQ-Z"), None);
    assert_eq!(index.conditions("REQ-A"), None);
}

#[test]
fn condition_on_non_branch_fires_for_a_label_under_a_non_branch_parent() {
    let items = vec![
        item("REQ-G", "requirement", &[]),
        item("REQ-P", "requirement", &[]),
        declared(item("REQ-D", "requirement", &[]), TreeKindDecl::Decision),
        // default label, only non-branch parents
        labelled(item("REQ-1", "requirement", &["REQ-G"]), "yes"),
        // per-parent label on a valid non-branch parent
        labelled_for(item("REQ-2", "requirement", &["REQ-P"]), "REQ-P", "yes"),
        // default label with one branch parent: fine
        labelled(item("REQ-3", "requirement", &["REQ-G", "REQ-D"]), "yes"),
    ];
    let index = build(&items);
    let hits = gaps_of(&index, TreeGapKind::ConditionOnNonBranch);
    assert_eq!(
        hits.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
        vec!["REQ-1", "REQ-2"]
    );
    assert!(hits[1].1.contains("REQ-P"));
}

#[test]
fn labels_stay_silent_while_the_declared_parent_is_dangling() {
    let items = vec![
        labelled(item("REQ-1", "requirement", &["REQ-NOPE"]), "yes"),
        labelled_for(
            item("REQ-2", "requirement", &["REQ-NOPE"]),
            "REQ-NOPE",
            "yes",
        ),
    ];
    let index = build(&items);
    assert!(gaps_of(&index, TreeGapKind::ConditionOnNonBranch).is_empty());
    assert!(gaps_of(&index, TreeGapKind::AttrUnknownParent).is_empty());
    assert_eq!(gaps_of(&index, TreeGapKind::ParentDangling).len(), 2);
    // A rootless label is quiet too.
    let index = build(&[labelled(item("REQ-R", "requirement", &[]), "yes")]);
    assert!(index.gaps().is_empty());
}

#[test]
fn condition_missing_names_each_branch_parent_without_a_label() {
    let items = vec![
        declared(item("REQ-A", "requirement", &[]), TreeKindDecl::Decision),
        declared(item("REQ-B", "requirement", &[]), TreeKindDecl::Select),
        item("REQ-G", "requirement", &[]),
        labelled(item("REQ-1", "requirement", &["REQ-A"]), "yes"),
        item("REQ-2", "requirement", &["REQ-A"]),
        // labelled for A only: missing under B
        labelled_for(
            item("REQ-3", "requirement", &["REQ-A", "REQ-B"]),
            "REQ-A",
            "no",
        ),
        // composite parent never needs a label
        item("REQ-4", "requirement", &["REQ-G"]),
    ];
    let index = build(&items);
    let hits = gaps_of(&index, TreeGapKind::ConditionMissing);
    assert_eq!(
        hits.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
        vec!["REQ-2", "REQ-3"]
    );
    assert!(hits[0].1.contains("REQ-A"));
    assert!(hits[1].1.contains("REQ-B"));
}

#[test]
fn condition_duplicate_compares_trimmed_and_case_insensitively_once_per_label() {
    let items = vec![
        declared(item("REQ-S", "requirement", &[]), TreeKindDecl::Select),
        labelled(item("REQ-1", "requirement", &["REQ-S"]), "Yes"),
        labelled(item("REQ-2", "requirement", &["REQ-S"]), " yes "),
        labelled(item("REQ-3", "requirement", &["REQ-S"]), "no"),
        labelled(item("REQ-4", "requirement", &["REQ-S"]), "default"),
        labelled(item("REQ-5", "requirement", &["REQ-S"]), "DEFAULT"),
        labelled(item("REQ-6", "requirement", &["REQ-S"]), "default"),
    ];
    let index = build(&items);
    let hits = gaps_of(&index, TreeGapKind::ConditionDuplicate);
    assert_eq!(hits.len(), 2, "one gap per duplicated label: {hits:?}");
    assert!(hits.iter().all(|(i, _)| *i == "REQ-S"));
    assert!(hits
        .iter()
        .any(|(_, d)| d.contains("REQ-1") && d.contains("REQ-2")));
    assert!(hits
        .iter()
        .any(|(_, d)| d.contains("REQ-4") && d.contains("REQ-5") && d.contains("REQ-6")));
}

#[test]
fn attr_unknown_parent_checks_the_declared_list_not_the_validated_one() {
    let items = vec![
        declared(item("REQ-A", "requirement", &[]), TreeKindDecl::Decision),
        {
            let mut it = labelled_for(
                item("REQ-1", "requirement", &["REQ-A", "REQ-LATER"]),
                "REQ-A",
                "yes",
            );
            // declared-but-nonexistent parent: forward reference, fine
            it.tree.order_per_parent.insert("REQ-LATER".into(), 1);
            // not declared at all: one gap per key
            it.tree.order_per_parent.insert("REQ-OTHER".into(), 2);
            it.tree
                .condition_per_parent
                .insert("REQ-ELSE".into(), "x".into());
            it
        },
    ];
    let index = build(&items);
    let hits = gaps_of(&index, TreeGapKind::AttrUnknownParent);
    assert_eq!(hits.len(), 2, "{hits:?}");
    assert!(hits.iter().all(|(i, _)| *i == "REQ-1"));
    assert!(hits.iter().any(|(_, d)| d.contains("order@REQ-OTHER")));
    assert!(hits.iter().any(|(_, d)| d.contains("condition@REQ-ELSE")));
    // The undeclared condition@ must not leak into the resolved labels.
    assert_eq!(index.conditions("REQ-1").unwrap().len(), 1);
}

#[test]
fn decision_arity_flags_more_than_two_children_but_not_a_select() {
    let mut items = vec![
        declared(item("REQ-D", "requirement", &[]), TreeKindDecl::Decision),
        declared(item("REQ-S", "requirement", &[]), TreeKindDecl::Select),
    ];
    for n in 1..=3 {
        items.push(labelled(
            item(&format!("REQ-D{n}"), "requirement", &["REQ-D"]),
            &format!("c{n}"),
        ));
        items.push(labelled(
            item(&format!("REQ-S{n}"), "requirement", &["REQ-S"]),
            &format!("c{n}"),
        ));
    }
    let index = build(&items);
    let hits = gaps_of(&index, TreeGapKind::DecisionArity);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0, "REQ-D");
    assert!(hits[0].1.contains("select"));
    // exactly two children is fine
    let index = build(
        &items[..2]
            .iter()
            .cloned()
            .chain(
                items[2..]
                    .iter()
                    .filter(|i| i.stable_id != "REQ-D3")
                    .cloned(),
            )
            .collect::<Vec<_>>(),
    );
    assert!(gaps_of(&index, TreeGapKind::DecisionArity).is_empty());
}

#[test]
fn empty_branch_flags_childless_decisions_and_selects_only() {
    let items = vec![
        declared(item("REQ-D", "requirement", &[]), TreeKindDecl::Decision),
        declared(item("REQ-S", "requirement", &[]), TreeKindDecl::Select),
        declared(item("REQ-G", "requirement", &[]), TreeKindDecl::Group),
        declared(item("REQ-L", "requirement", &[]), TreeKindDecl::Leaf),
    ];
    let index = build(&items);
    assert_eq!(
        gap_kinds(&index),
        vec![
            (TreeGapKind::EmptyBranch, "REQ-D"),
            (TreeGapKind::EmptyGroup, "REQ-G"),
            (TreeGapKind::EmptyBranch, "REQ-S"),
        ]
    );
}

#[test]
fn all_lists_every_variant() {
    // The exhaustive match stops compiling when a variant is added; give the
    // new one the next position, and the length check below then fails until
    // `TreeGapKind::ALL` lists it too.
    fn position(kind: TreeGapKind) -> usize {
        match kind {
            TreeGapKind::ParentDangling => 0,
            TreeGapKind::ParentCrossLayer => 1,
            TreeGapKind::TreeCycle => 2,
            TreeGapKind::SelfParent => 3,
            TreeGapKind::EmptyGroup => 4,
            TreeGapKind::KindConflict => 5,
            TreeGapKind::ConditionMissing => 6,
            TreeGapKind::ConditionDuplicate => 7,
            TreeGapKind::ConditionOnNonBranch => 8,
            TreeGapKind::AttrUnknownParent => 9,
            TreeGapKind::DecisionArity => 10,
            TreeGapKind::EmptyBranch => 11,
        }
    }
    const VARIANT_COUNT: usize = 12;
    assert_eq!(TreeGapKind::ALL.len(), VARIANT_COUNT);
    for (i, kind) in TreeGapKind::ALL.into_iter().enumerate() {
        assert_eq!(position(kind), i, "{kind:?} is out of place in ALL");
    }
}
