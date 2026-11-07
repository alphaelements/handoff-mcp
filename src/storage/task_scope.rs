//! The tasks `handoff_load_context` returns by default (`task_tree:"compact"`,
//! FR-V0473-002/003): the ones that belong to the current session, each with
//! the reason it was picked, instead of every task in the project.
//!
//! Everything here is pure. The handler hands in the task index it already
//! built, the previous session's `related_task_ids`, the task ids git knows
//! about and the current milestone, so the rules are tested without a project
//! on disk.
//!
//! Selection rules (SPEC-V0473-102, DS-V0473-102), applied in this order:
//!
//! 1. `in_progress` / `blocked` / `review` tasks (`active`) and the subtrees of
//!    `task_scope` (`scope`). Never counted against the limit.
//! 2. The previous session's `related_task_ids`, not finished
//!    (`previous_session`, at most [`PREVIOUS_SESSION_MAX`]).
//! 3. Their siblings, [`SIBLINGS_PER_PARENT_MAX`] per parent (`sibling`).
//! 4. Tasks whose dependencies are all done (`unblocked`, at most
//!    [`UNBLOCKED_MAX`]).
//! 5. Tasks named by the branch or the latest commits (`branch`).
//! 6. `todo` tasks of the current milestone (`milestone`).
//! 7. `todo` tasks without a milestone, by priority then newest id
//!    (`priority`).
//!
//! Rules 2-7 share one budget (`compact_task_limit`). Ancestors of selected
//! tasks are kept so the tree stays a tree; they are not counted.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::config::MilestoneConfig;
use super::tasks::{is_terminal_status, TaskIndex, TaskSummary};

/// Rule 2: most previous-session tasks taken.
pub const PREVIOUS_SESSION_MAX: usize = 20;
/// Rule 3: most siblings taken per parent.
pub const SIBLINGS_PER_PARENT_MAX: usize = 10;
/// Rule 4: most newly unblocked tasks taken.
pub const UNBLOCKED_MAX: usize = 10;
/// `session_guidance.suggested_fields.related_task_ids` is cut to this.
pub const RELATED_TASK_IDS_MAX: usize = 20;

/// Reasons a task is in the compact tree (`scope_reason`).
pub const REASON_ACTIVE: &str = "active";
pub const REASON_SCOPE: &str = "scope";
pub const REASON_PREVIOUS_SESSION: &str = "previous_session";
pub const REASON_SIBLING: &str = "sibling";
pub const REASON_UNBLOCKED: &str = "unblocked";
pub const REASON_BRANCH: &str = "branch";
pub const REASON_MILESTONE: &str = "milestone";
pub const REASON_PRIORITY: &str = "priority";

/// Pointer to the ways of getting the tasks a compact tree leaves out.
pub const COMPACT_HINT: &str = "task_tree lists only the tasks that belong to this session \
(task_tree_meta says how many were left out). Fetch the others with \
handoff_list_tasks(status_filter, ids, limit, fields), or call handoff_load_context \
with task_tree:\"full\" for the whole tree.";

/// What `task_tree_meta.hint` says when the whole tree was asked for.
pub const FULL_HINT: &str =
    "task_tree:\"full\" lists every task. The default, task_tree:\"compact\", \
lists only the tasks that belong to this session.";

/// Everything the selection needs besides the task index.
pub struct ScopeRequest<'a> {
    /// `task_scope`: ids whose whole subtree is included.
    pub explicit_scope: &'a [String],
    /// `related_task_ids` of the previous session, in the order it recorded them.
    pub previous_related: &'a [String],
    /// Task ids named by the branch and the latest commit subjects.
    pub branch_ids: &'a BTreeSet<String>,
    /// The milestone rule 6 selects from.
    pub current_milestone: Option<&'a str>,
    /// `compact_task_limit`: the budget of rules 2-7.
    pub limit: usize,
}

/// The compact `task_tree` and its `task_tree_meta`.
pub struct CompactView {
    pub tree: Vec<Value>,
    pub meta: Value,
    /// `task_scope` ids that are not tasks of this project.
    pub unknown_scope: Vec<String>,
}

/// Statuses of work that is under way: always part of the compact tree.
const ACTIVE_STATUSES: [&str; 3] = ["in_progress", "blocked", "review"];

/// `[milestones.<name>] status` of the milestone being worked on.
const ACTIVE_MILESTONE_STATUS: &str = "active";

/// A task with its place in the tree. Positions follow a depth-first walk, so
/// comparing positions compares tree order.
struct Flat<'a> {
    task: &'a TaskIndex,
    parent: Option<usize>,
    children: Vec<usize>,
}

struct Index<'a> {
    nodes: Vec<Flat<'a>>,
    roots: Vec<usize>,
    by_id: HashMap<&'a str, usize>,
}

impl<'a> Index<'a> {
    fn new(tree: &'a [TaskIndex]) -> Self {
        let mut index = Index {
            nodes: Vec::new(),
            roots: Vec::new(),
            by_id: HashMap::new(),
        };
        index.roots = index.add(tree, None);
        index
    }

    fn add(&mut self, tree: &'a [TaskIndex], parent: Option<usize>) -> Vec<usize> {
        let mut positions = Vec::with_capacity(tree.len());
        for task in tree {
            let position = self.nodes.len();
            // The first task of an id wins, matching a lookup in tree order.
            self.by_id.entry(task.id.as_str()).or_insert(position);
            self.nodes.push(Flat {
                task,
                parent,
                children: Vec::new(),
            });
            let children = self.add(&task.children, Some(position));
            self.nodes[position].children = children;
            positions.push(position);
        }
        positions
    }

    fn task(&self, position: usize) -> &'a TaskIndex {
        self.nodes[position].task
    }

    fn find(&self, id: &str) -> Option<usize> {
        self.by_id.get(id).copied()
    }

    fn is_finished(&self, position: usize) -> bool {
        is_terminal_status(&self.task(position).status)
    }

    fn milestone_of(&self, position: usize) -> Option<&'a str> {
        self.task(position)
            .schedule
            .as_ref()
            .and_then(|s| s.milestone.as_deref())
    }
}

/// The numeric parts of a task id (`t439.12` -> `[439, 12]`) for ordering ids
/// the way people read them (`t9` before `t10`). An id that is not of that
/// shape sorts by its text alone.
fn id_key(id: &str) -> (Vec<u64>, &str) {
    let numbers: Option<Vec<u64>> = id
        .strip_prefix('t')
        .map(|rest| rest.split('.').map(|part| part.parse().ok()).collect())
        .unwrap_or(None);
    (numbers.unwrap_or_default(), id)
}

fn priority_rank(priority: Option<&str>) -> u8 {
    match priority {
        Some("high") => 0,
        Some("medium") => 1,
        Some("low") => 2,
        _ => 3,
    }
}

/// Rules 1-7 over one index: who is in, and why.
struct Selector<'i, 'a> {
    index: &'i Index<'a>,
    reason: Vec<Option<&'static str>>,
    /// Tasks taken by rules 2-7.
    used: usize,
    limit: usize,
    truncated: bool,
}

impl<'i, 'a> Selector<'i, 'a> {
    fn new(index: &'i Index<'a>, limit: usize) -> Self {
        Selector {
            index,
            reason: vec![None; index.nodes.len()],
            used: 0,
            limit,
            truncated: false,
        }
    }

    /// Rule 1: taken without counting. Keeps an earlier reason.
    fn take_free(&mut self, position: usize, reason: &'static str) {
        self.reason[position].get_or_insert(reason);
    }

    /// Rules 2-7: counted once, and only while the budget lasts. A task an
    /// earlier rule took is left as it is.
    fn take(&mut self, position: usize, reason: &'static str) {
        if self.reason[position].is_some() {
            return;
        }
        if self.used >= self.limit {
            self.truncated = true;
            return;
        }
        self.reason[position] = Some(reason);
        self.used += 1;
    }

    /// Keep the first `cap` candidates of a rule; dropping any is a truncation.
    fn capped(&mut self, mut candidates: Vec<usize>, cap: usize) -> Vec<usize> {
        if candidates.len() > cap {
            self.truncated = true;
            candidates.truncate(cap);
        }
        candidates
    }

    fn positions_where(&self, keep: impl Fn(usize, &TaskIndex) -> bool) -> Vec<usize> {
        (0..self.index.nodes.len())
            .filter(|&p| keep(p, self.index.task(p)))
            .collect()
    }

    /// Rule 1.
    fn active_and_scope(&mut self, explicit_scope: &[String]) -> Vec<String> {
        for position in self.positions_where(|_, t| ACTIVE_STATUSES.contains(&t.status.as_str())) {
            self.take_free(position, REASON_ACTIVE);
        }
        let mut unknown = Vec::new();
        let mut seen = BTreeSet::new();
        for id in explicit_scope.iter().filter(|id| seen.insert(id.as_str())) {
            let Some(root) = self.index.find(id) else {
                unknown.push(id.clone());
                continue;
            };
            let mut pending = vec![root];
            while let Some(position) = pending.pop() {
                if !self.index.is_finished(position) {
                    self.take_free(position, REASON_SCOPE);
                }
                pending.extend(self.index.nodes[position].children.iter().rev());
            }
        }
        unknown
    }

    /// Rules 2 and 3.
    fn previous_session_and_siblings(&mut self, previous_related: &[String]) {
        let mut seen = BTreeSet::new();
        let previous: Vec<usize> = previous_related
            .iter()
            .filter_map(|id| self.index.find(id))
            .filter(|&p| !self.index.is_finished(p) && seen.insert(p))
            .collect();
        let previous = self.capped(previous, PREVIOUS_SESSION_MAX);
        for &position in &previous {
            self.take(position, REASON_PREVIOUS_SESSION);
        }

        let mut parents: Vec<Option<usize>> = Vec::new();
        for &position in &previous {
            let parent = self.index.nodes[position].parent;
            if !parents.contains(&parent) {
                parents.push(parent);
            }
        }
        for parent in parents {
            let siblings = match parent {
                Some(p) => &self.index.nodes[p].children,
                None => &self.index.roots,
            };
            let mut candidates: Vec<usize> = siblings
                .iter()
                .copied()
                .filter(|&p| !self.index.is_finished(p))
                .collect();
            candidates.sort_by_key(|&p| {
                let task = self.index.task(p);
                (task.order.is_none(), task.order, id_key(&task.id))
            });
            for position in self.capped(candidates, SIBLINGS_PER_PARENT_MAX) {
                self.take(position, REASON_SIBLING);
            }
        }
    }

    /// Rule 4. A dependency the index does not list counts as done: tasks past
    /// `done_task_limit` are never listed, and they are the finished ones.
    fn unblocked(&mut self) {
        let index = self.index;
        let candidates = self.positions_where(|_, t| {
            t.status == "todo"
                && !t.dependencies.is_empty()
                && t.dependencies
                    .iter()
                    .all(|dep| index.find(dep).is_none_or(|p| index.is_finished(p)))
        });
        for position in self.capped(candidates, UNBLOCKED_MAX) {
            self.take(position, REASON_UNBLOCKED);
        }
    }

    /// Rule 5.
    fn named_by_git(&mut self, branch_ids: &BTreeSet<String>) {
        let candidates =
            self.positions_where(|p, t| !self.index.is_finished(p) && branch_ids.contains(&t.id));
        for position in candidates {
            self.take(position, REASON_BRANCH);
        }
    }

    /// Rule 6.
    fn current_milestone(&mut self, milestone: Option<&str>) {
        let Some(milestone) = milestone else {
            return;
        };
        let candidates = self.positions_where(|p, t| {
            t.status == "todo" && self.index.milestone_of(p) == Some(milestone)
        });
        for position in candidates {
            self.take(position, REASON_MILESTONE);
        }
    }

    /// Rule 7.
    fn by_priority(&mut self) {
        let mut candidates =
            self.positions_where(|p, t| t.status == "todo" && self.index.milestone_of(p).is_none());
        candidates.sort_by(|&a, &b| {
            let (a, b) = (self.index.task(a), self.index.task(b));
            priority_rank(a.priority.as_deref())
                .cmp(&priority_rank(b.priority.as_deref()))
                .then_with(|| id_key(&b.id).cmp(&id_key(&a.id)))
        });
        for position in candidates {
            self.take(position, REASON_PRIORITY);
        }
    }
}

/// One node as it appears in a compact tree: the task's own fields, `children`
/// only for the nodes that were kept, and `scope_reason` for selected tasks
/// (an ancestor kept for structure has none).
fn node_json(task: &TaskIndex, reason: Option<&str>, children: Vec<Value>) -> Result<Value> {
    // `children` and `task_links` are never serialized from the original: the
    // kept children are passed in, and `task_links` is `serde(skip)`.
    let own = TaskIndex {
        id: task.id.clone(),
        title: task.title.clone(),
        status: task.status.clone(),
        schedule: task.schedule.clone(),
        dependencies: task.dependencies.clone(),
        order: task.order,
        assignee: task.assignee.clone(),
        priority: None,
        lock: task.lock.clone(),
        task_links: Vec::new(),
        children: Vec::new(),
    };
    let mut node = serde_json::to_value(&own).context("serialize task")?;
    if let Some(map) = node.as_object_mut() {
        if let Some(reason) = reason {
            map.insert("scope_reason".to_string(), json!(reason));
        }
        if !children.is_empty() {
            map.insert("children".to_string(), Value::Array(children));
        }
    }
    Ok(node)
}

/// The nodes of `positions` that are selected or have a selected descendant,
/// in their original order. `kept` records every position that was emitted.
fn render(
    index: &Index,
    reason: &[Option<&'static str>],
    positions: &[usize],
    kept: &mut [bool],
) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for &position in positions {
        let children = render(index, reason, &index.nodes[position].children, kept)?;
        if reason[position].is_some() || !children.is_empty() {
            kept[position] = true;
            out.push(node_json(index.task(position), reason[position], children)?);
        }
    }
    Ok(out)
}

/// Select the tasks of `tree` that belong to the session and render them.
/// `summary` is the aggregate of the whole project; the meta reports what the
/// tree leaves out of it.
pub fn compact_view(
    tree: &[TaskIndex],
    summary: &TaskSummary,
    request: &ScopeRequest,
) -> Result<CompactView> {
    let index = Index::new(tree);
    let mut selector = Selector::new(&index, request.limit);
    let unknown_scope = selector.active_and_scope(request.explicit_scope);
    selector.previous_session_and_siblings(request.previous_related);
    selector.unblocked();
    selector.named_by_git(request.branch_ids);
    selector.current_milestone(request.current_milestone);
    selector.by_priority();
    let Selector {
        reason, truncated, ..
    } = selector;

    let mut kept = vec![false; index.nodes.len()];
    let rendered = render(&index, &reason, &index.roots, &mut kept)?;

    let mut shown: BTreeMap<&str, u32> = BTreeMap::new();
    let mut omitted_without_milestone = 0usize;
    for (position, &is_kept) in kept.iter().enumerate() {
        if is_kept {
            *shown
                .entry(index.task(position).status.as_str())
                .or_insert(0) += 1;
        } else if index.milestone_of(position).is_none() {
            omitted_without_milestone += 1;
        }
    }
    let shown_total = kept.iter().filter(|&&k| k).count();
    let omitted_by_status: BTreeMap<&str, u32> = summary
        .by_status
        .iter()
        .filter_map(|(status, &count)| {
            let left = count.saturating_sub(shown.get(status.as_str()).copied().unwrap_or(0));
            (left > 0).then_some((status.as_str(), left))
        })
        .collect();

    let meta = json!({
        "mode": "compact",
        "included": reason.iter().flatten().count(),
        "omitted_total": (summary.total as usize).saturating_sub(shown_total),
        "omitted_by_status": omitted_by_status,
        "omitted_without_milestone": omitted_without_milestone,
        "truncated": truncated,
        "hint": COMPACT_HINT,
    });
    Ok(CompactView {
        tree: rendered,
        meta,
        unknown_scope,
    })
}

/// `task_tree_meta` of `task_tree:"full"`: nothing is left out of the tree.
pub fn full_meta(tree: &[TaskIndex]) -> Value {
    json!({
        "mode": "full",
        "included": Index::new(tree).nodes.len(),
        "omitted_total": 0,
        "omitted_by_status": {},
        "omitted_without_milestone": 0,
        "truncated": false,
        "hint": FULL_HINT,
    })
}

/// The milestone rule 6 selects from: the `active` milestone with the lowest
/// `order`, else the one the `in_progress` tasks belong to (lowest `order`).
/// A milestone without a status is not `active`.
pub fn current_milestone(
    milestones: &HashMap<String, MilestoneConfig>,
    tree: &[TaskIndex],
) -> Option<String> {
    // Unordered milestones last; the name settles ties so the choice does not
    // depend on the map's iteration order.
    let rank = |name: &str| {
        let order = milestones.get(name).and_then(|m| m.order);
        (order.unwrap_or(i64::MAX), name.to_string())
    };
    let active = milestones
        .iter()
        .filter(|(_, m)| m.status.as_deref() == Some(ACTIVE_MILESTONE_STATUS))
        .map(|(name, _)| rank(name))
        .min();
    if let Some((_, name)) = active {
        return Some(name);
    }

    fn in_progress_milestones<'a>(tree: &'a [TaskIndex], out: &mut BTreeSet<&'a str>) {
        for task in tree {
            if task.status == "in_progress" {
                if let Some(name) = task.schedule.as_ref().and_then(|s| s.milestone.as_deref()) {
                    out.insert(name);
                }
            }
            in_progress_milestones(&task.children, out);
        }
    }
    let mut names = BTreeSet::new();
    in_progress_milestones(tree, &mut names);
    names.into_iter().map(rank).min().map(|(_, name)| name)
}

/// `session_guidance.suggested_fields.related_task_ids`: unfinished work in
/// flight, the previous session's unfinished tasks and the tasks this agent
/// claimed, at most [`RELATED_TASK_IDS_MAX`]. The flag is true when more were
/// found than fit.
pub fn related_task_ids(
    tree: &[TaskIndex],
    previous_related: &[String],
    claimed: &[String],
) -> (Vec<String>, bool) {
    let index = Index::new(tree);
    let in_flight = (0..index.nodes.len())
        .filter(|&p| ACTIVE_STATUSES.contains(&index.task(p).status.as_str()))
        .map(|p| index.task(p).id.clone());
    let previous = previous_related
        .iter()
        .filter(|id| index.find(id).is_some_and(|p| !index.is_finished(p)))
        .cloned();
    let claimed = claimed
        .iter()
        .filter(|id| index.find(id).is_some())
        .cloned();

    let mut seen = BTreeSet::new();
    let mut ids: Vec<String> = in_flight
        .chain(previous)
        .chain(claimed)
        .filter(|id| seen.insert(id.clone()))
        .collect();
    let truncated = ids.len() > RELATED_TASK_IDS_MAX;
    ids.truncate(RELATED_TASK_IDS_MAX);
    (ids, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, status: &str) -> TaskIndex {
        TaskIndex {
            id: id.to_string(),
            title: format!("title {id}"),
            status: status.to_string(),
            schedule: None,
            dependencies: Vec::new(),
            order: None,
            assignee: None,
            priority: None,
            lock: None,
            task_links: Vec::new(),
            children: Vec::new(),
        }
    }

    fn parent(mut t: TaskIndex, kids: Vec<TaskIndex>) -> TaskIndex {
        t.children = kids;
        t
    }

    fn milestone(mut t: TaskIndex, name: &str) -> TaskIndex {
        t.schedule = Some(crate::storage::tasks::Schedule {
            start_date: None,
            due_date: None,
            estimate_hours: None,
            actual_hours: None,
            remaining_hours: None,
            milestone: Some(name.to_string()),
            pinned: None,
            baseline_start: None,
            baseline_due: None,
        });
        t
    }

    fn priority(mut t: TaskIndex, p: &str) -> TaskIndex {
        t.priority = Some(p.to_string());
        t
    }

    fn order(mut t: TaskIndex, n: u32) -> TaskIndex {
        t.order = Some(n);
        t
    }

    fn deps(mut t: TaskIndex, d: &[&str]) -> TaskIndex {
        t.dependencies = d.iter().map(|s| s.to_string()).collect();
        t
    }

    fn summarize(tree: &[TaskIndex]) -> TaskSummary {
        fn walk(tree: &[TaskIndex], s: &mut TaskSummary) {
            for t in tree {
                s.total += 1;
                *s.by_status.entry(t.status.clone()).or_insert(0) += 1;
                walk(&t.children, s);
            }
        }
        let mut s = TaskSummary {
            total: 0,
            by_status: BTreeMap::new(),
            total_estimate_hours: None,
            total_actual_hours: None,
            completion_rate: None,
            overdue_count: 0,
        };
        walk(tree, &mut s);
        s
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    struct Req {
        scope: Vec<String>,
        previous: Vec<String>,
        branch: BTreeSet<String>,
        milestone: Option<String>,
        limit: usize,
    }

    impl Req {
        fn new(limit: usize) -> Self {
            Req {
                scope: Vec::new(),
                previous: Vec::new(),
                branch: BTreeSet::new(),
                milestone: None,
                limit,
            }
        }
        fn run(&self, tree: &[TaskIndex]) -> CompactView {
            compact_view(
                tree,
                &summarize(tree),
                &ScopeRequest {
                    explicit_scope: &self.scope,
                    previous_related: &self.previous,
                    branch_ids: &self.branch,
                    current_milestone: self.milestone.as_deref(),
                    limit: self.limit,
                },
            )
            .expect("compact view")
        }
    }

    /// `(id, scope_reason)` of every node, depth first; `None` for a node kept
    /// only as an ancestor.
    fn nodes(view: &CompactView) -> Vec<(String, Option<String>)> {
        fn walk(nodes: &[Value], out: &mut Vec<(String, Option<String>)>) {
            for n in nodes {
                out.push((
                    n["id"].as_str().unwrap().to_string(),
                    n["scope_reason"].as_str().map(str::to_string),
                ));
                if let Some(kids) = n["children"].as_array() {
                    walk(kids, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(&view.tree, &mut out);
        out
    }

    fn reasons(view: &CompactView) -> BTreeMap<String, String> {
        nodes(view)
            .into_iter()
            .filter_map(|(id, r)| r.map(|r| (id, r)))
            .collect()
    }

    fn reason_of(view: &CompactView, id: &str) -> Option<String> {
        reasons(view).get(id).cloned()
    }

    fn ids(view: &CompactView) -> Vec<String> {
        nodes(view).into_iter().map(|(id, _)| id).collect()
    }

    fn count_reason(view: &CompactView, reason: &str) -> usize {
        reasons(view).values().filter(|r| *r == reason).count()
    }

    // --- rule 1 -----------------------------------------------------------

    #[test]
    fn active_tasks_are_all_included_and_ignore_the_limit() {
        let tree: Vec<TaskIndex> = (1..=50)
            .map(|i| task(&format!("t{i}"), "in_progress"))
            .chain([
                task("t51", "blocked"),
                task("t52", "review"),
                task("t53", "todo"),
            ])
            .collect();
        // 52 active tasks against a budget of 40: all of them are in, and the
        // budget of the other rules is not reduced by them (40 remain).
        let view = Req::new(40).run(&tree);
        assert_eq!(count_reason(&view, REASON_ACTIVE), 52);
        assert_eq!(reason_of(&view, "t53").as_deref(), Some("priority"));
        let view = Req::new(0).run(&tree);
        assert_eq!(count_reason(&view, REASON_ACTIVE), 52);
        assert_eq!(reason_of(&view, "t53"), None, "an unrelated todo stays out");
    }

    #[test]
    fn explicit_scope_includes_the_unfinished_subtree_without_using_the_budget() {
        let tree = vec![
            parent(
                task("t75", "todo"),
                vec![
                    task("t75.1", "todo"),
                    parent(
                        task("t75.2", "todo"),
                        vec![task("t75.2.1", "todo"), task("t75.2.2", "done")],
                    ),
                ],
            ),
            task("t76", "todo"),
        ];
        let mut req = Req::new(0);
        req.scope = strings(&["t75"]);
        let view = req.run(&tree);
        for id in ["t75", "t75.1", "t75.2", "t75.2.1"] {
            assert_eq!(reason_of(&view, id).as_deref(), Some("scope"), "{id}");
        }
        assert!(
            !ids(&view).contains(&"t75.2.2".to_string()),
            "a done task of the subtree is left out"
        );
        assert!(!ids(&view).contains(&"t76".to_string()));
    }

    #[test]
    fn explicit_scope_names_a_subtask_and_reports_unknown_ids() {
        let tree = vec![parent(
            task("t1", "todo"),
            vec![task("t1.1", "todo"), task("t1.2", "todo")],
        )];
        let mut req = Req::new(0);
        req.scope = strings(&["t1.2", "t99999"]);
        let view = req.run(&tree);
        assert_eq!(reason_of(&view, "t1.2").as_deref(), Some("scope"));
        assert_eq!(reason_of(&view, "t1.1"), None);
        assert_eq!(view.unknown_scope, vec!["t99999".to_string()]);
    }

    #[test]
    fn an_empty_scope_equals_no_scope() {
        let tree = vec![task("t1", "in_progress"), task("t2", "todo")];
        let a = Req::new(40).run(&tree);
        let mut b = Req::new(40);
        b.scope = Vec::new();
        assert_eq!(a.tree, b.run(&tree).tree);
    }

    // --- rules 2 and 3 -------------------------------------------------------

    #[test]
    fn previous_session_tasks_are_taken_in_recorded_order_up_to_twenty() {
        let tree: Vec<TaskIndex> = (1..=30).map(|i| task(&format!("t{i}"), "todo")).collect();
        let mut req = Req::new(100);
        // Reverse order: the recorded order wins over the tree order.
        req.previous = (1..=30).rev().map(|i| format!("t{i}")).collect();
        let view = req.run(&tree);
        let mut got: Vec<String> = reasons(&view)
            .into_iter()
            .filter(|(_, r)| r == REASON_PREVIOUS_SESSION)
            .map(|(id, _)| id)
            .collect();
        got.sort();
        let mut want: Vec<String> = (11..=30).map(|i| format!("t{i}")).collect();
        want.sort();
        assert_eq!(got, want);
        assert_eq!(
            view.meta["truncated"], true,
            "five previous tasks were dropped"
        );
    }

    #[test]
    fn previous_session_skips_done_skipped_and_unknown_tasks() {
        let tree = vec![
            task("t1", "done"),
            task("t2", "skipped"),
            task("t3", "todo"),
        ];
        let mut req = Req::new(40);
        req.previous = strings(&["t1", "t2", "t3", "t404"]);
        let view = req.run(&tree);
        assert_eq!(
            reasons(&view),
            BTreeMap::from([("t3".to_string(), "previous_session".to_string())])
        );
        assert_eq!(view.meta["truncated"], false);
    }

    #[test]
    fn a_task_in_progress_that_the_previous_session_also_listed_stays_active() {
        let tree = vec![task("t1", "in_progress")];
        let mut req = Req::new(40);
        req.previous = strings(&["t1"]);
        let view = req.run(&tree);
        assert_eq!(reason_of(&view, "t1").as_deref(), Some("active"));
        assert_eq!(view.meta["included"], 1);
    }

    #[test]
    fn siblings_are_capped_per_parent_in_order_and_count_the_previous_task() {
        let kids: Vec<TaskIndex> = (1..=241)
            .map(|i| order(task(&format!("t1.{i}"), "todo"), 1000 - i))
            .collect();
        let tree = vec![parent(task("t1", "todo"), kids)];
        let mut req = Req::new(100);
        // The lowest order first: t1.241 (order 759), t1.240, ... The previous
        // task is one of the ten, so nine siblings are added.
        req.previous = strings(&["t1.241"]);
        let view = req.run(&tree);
        assert_eq!(
            reason_of(&view, "t1.241").as_deref(),
            Some("previous_session")
        );
        let siblings: Vec<String> = reasons(&view)
            .into_iter()
            .filter(|(_, r)| r == REASON_SIBLING)
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            siblings.len(),
            9,
            "ten per parent, one is previous: {siblings:?}"
        );
        for i in 233..=240 {
            assert!(siblings.contains(&format!("t1.{i}")), "t1.{i} missing");
        }
        assert!(siblings.contains(&"t1.232".to_string()));
        assert_eq!(view.meta["truncated"], true);
    }

    #[test]
    fn siblings_without_order_come_last_by_id() {
        let tree = vec![parent(
            task("t1", "todo"),
            vec![
                task("t1.10", "todo"),
                task("t1.9", "todo"),
                order(task("t1.20", "todo"), 1),
                task("t1.1", "done"),
            ],
        )];
        let mut req = Req::new(100);
        req.previous = strings(&["t1.20"]);
        let view = req.run(&tree);
        assert_eq!(reason_of(&view, "t1.9").as_deref(), Some("sibling"));
        assert_eq!(reason_of(&view, "t1.10").as_deref(), Some("sibling"));
        assert_eq!(
            reason_of(&view, "t1.1"),
            None,
            "a done sibling is not taken"
        );
    }

    #[test]
    fn top_level_previous_tasks_use_the_top_level_as_their_siblings() {
        let tree: Vec<TaskIndex> = (1..=15)
            .map(|i| order(task(&format!("t{i}"), "todo"), i))
            .collect();
        let mut req = Req::new(100);
        req.previous = strings(&["t1"]);
        let view = req.run(&tree);
        assert_eq!(count_reason(&view, REASON_SIBLING), 9);
        assert_eq!(reason_of(&view, "t10").as_deref(), Some("sibling"));
        assert_ne!(reason_of(&view, "t11").as_deref(), Some("sibling"));
    }

    // --- rule 4 -------------------------------------------------------------

    #[test]
    fn unblocked_tasks_have_every_dependency_done() {
        let tree = vec![
            task("t1", "done"),
            task("t2", "todo"),
            deps(task("t3", "todo"), &["t1"]),
            deps(task("t4", "todo"), &["t1", "t2"]),
            deps(task("t5", "todo"), &["t6"]),
            task("t6", "done"),
        ];
        let view = Req::new(40).run(&tree);
        assert_eq!(reason_of(&view, "t3").as_deref(), Some("unblocked"));
        assert_eq!(reason_of(&view, "t5").as_deref(), Some("unblocked"));
        assert_ne!(
            reason_of(&view, "t4").as_deref(),
            Some("unblocked"),
            "t2 is not done"
        );
        assert_ne!(reason_of(&view, "t2").as_deref(), Some("unblocked"));
    }

    #[test]
    fn a_dependency_missing_from_the_index_counts_as_done() {
        // Finished tasks beyond `done_task_limit` are not in the index.
        let tree = vec![deps(task("t9", "todo"), &["t1"])];
        let view = Req::new(40).run(&tree);
        assert_eq!(reason_of(&view, "t9").as_deref(), Some("unblocked"));
    }

    #[test]
    fn unblocked_tasks_are_capped_at_ten() {
        let mut tree = vec![task("t0", "done")];
        tree.extend((1..=15).map(|i| deps(task(&format!("t{i}"), "todo"), &["t0"])));
        let view = Req::new(100).run(&tree);
        assert_eq!(count_reason(&view, REASON_UNBLOCKED), 10);
        assert_eq!(view.meta["truncated"], true);
    }

    // --- rule 5 -------------------------------------------------------------

    #[test]
    fn branch_ids_select_existing_unfinished_tasks_only() {
        let tree = vec![
            task("t431", "todo"),
            task("t430", "done"),
            parent(
                milestone(task("t439", "todo"), "other"),
                vec![task("t439.1", "todo")],
            ),
        ];
        let mut req = Req::new(40);
        req.branch = ["t431", "t430", "t439.1", "t99999"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let view = req.run(&tree);
        assert_eq!(reason_of(&view, "t431").as_deref(), Some("branch"));
        assert_eq!(reason_of(&view, "t439.1").as_deref(), Some("branch"));
        assert_eq!(reason_of(&view, "t430"), None);
        assert_eq!(
            reason_of(&view, "t439"),
            None,
            "the parent is only an ancestor"
        );
        assert!(ids(&view).contains(&"t439".to_string()));
    }

    // --- rules 6 and 7 --------------------------------------------------------

    #[test]
    fn the_current_milestone_contributes_its_todos_in_tree_order() {
        let tree = vec![
            milestone(task("t1", "todo"), "v1"),
            milestone(task("t2", "todo"), "v2"),
            milestone(task("t3", "done"), "v1"),
            milestone(task("t4", "todo"), "v1"),
        ];
        let mut req = Req::new(40);
        req.milestone = Some("v1".to_string());
        let view = req.run(&tree);
        assert_eq!(reason_of(&view, "t1").as_deref(), Some("milestone"));
        assert_eq!(reason_of(&view, "t4").as_deref(), Some("milestone"));
        assert_eq!(reason_of(&view, "t2"), None);
        assert_eq!(reason_of(&view, "t3"), None);
    }

    #[test]
    fn without_a_current_milestone_rule_six_selects_nothing() {
        let tree = vec![milestone(task("t1", "todo"), "v1")];
        let view = Req::new(40).run(&tree);
        assert!(reasons(&view).is_empty());
    }

    #[test]
    fn remaining_budget_goes_to_milestone_less_todos_by_priority_then_newest_id() {
        let tree = vec![
            priority(task("t9", "todo"), "high"),
            priority(task("t10", "todo"), "high"),
            priority(task("t11", "todo"), "low"),
            priority(task("t12", "todo"), "medium"),
            task("t13", "todo"),
            milestone(priority(task("t14", "todo"), "high"), "other"),
        ];
        let view = Req::new(4).run(&tree);
        let picked: BTreeSet<String> = reasons(&view)
            .into_iter()
            .filter(|(_, r)| r == REASON_PRIORITY)
            .map(|(id, _)| id)
            .collect();
        // high: t10 before t9 (newest first), then medium t12, then low t11;
        // the unset-priority t13 and the milestone task t14 do not fit/qualify.
        assert_eq!(
            picked,
            ["t10", "t9", "t12", "t11"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        );
        assert_eq!(view.meta["truncated"], true, "t13 did not fit");
    }

    // --- limit, ancestors, order ------------------------------------------------

    #[test]
    fn the_limit_covers_rules_two_to_seven_and_drops_later_rules_first() {
        let tree = vec![
            parent(task("t1", "done"), vec![task("t1.1", "todo")]),
            deps(task("t2", "todo"), &["t0"]),
            milestone(task("t3", "todo"), "v1"),
            task("t4", "todo"),
            task("t5", "in_progress"),
        ];
        let mut req = Req::new(3);
        req.previous = strings(&["t1.1"]);
        req.milestone = Some("v1".to_string());
        let view = req.run(&tree);
        assert_eq!(reason_of(&view, "t5").as_deref(), Some("active"));
        assert_eq!(
            reason_of(&view, "t1.1").as_deref(),
            Some("previous_session")
        );
        assert_eq!(reason_of(&view, "t2").as_deref(), Some("unblocked"));
        assert_eq!(reason_of(&view, "t3").as_deref(), Some("milestone"));
        assert_eq!(reason_of(&view, "t4"), None, "the budget of 3 is spent");
        assert_eq!(view.meta["truncated"], true);
        assert_eq!(view.meta["included"], 4);
    }

    #[test]
    fn a_zero_limit_keeps_only_rule_one() {
        let tree = vec![task("t1", "in_progress"), task("t2", "todo")];
        let mut req = Req::new(0);
        req.previous = strings(&["t2"]);
        let view = req.run(&tree);
        assert_eq!(ids(&view), vec!["t1".to_string()]);
        assert_eq!(view.meta["truncated"], true);
    }

    #[test]
    fn ancestors_keep_the_tree_connected_and_do_not_use_the_budget() {
        let deep = |p: &str, n: usize| {
            parent(
                task(p, "done"),
                vec![parent(
                    task(&format!("{p}.1"), "done"),
                    (1..=n)
                        .map(|i| task(&format!("{p}.1.{i}"), "todo"))
                        .collect(),
                )],
            )
        };
        let tree = vec![deep("t1", 5), deep("t2", 5)];
        let mut req = Req::new(5);
        req.previous = strings(&["t1.1.1", "t1.1.2", "t1.1.3", "t1.1.4", "t1.1.5"]);
        let view = req.run(&tree);
        assert_eq!(count_reason(&view, REASON_PREVIOUS_SESSION), 5);
        // t1 and t1.1 are done ancestors: present, without a reason.
        assert_eq!(
            nodes(&view)[..2],
            [("t1".to_string(), None), ("t1.1".to_string(), None)]
        );
        assert!(!ids(&view).iter().any(|id| id.starts_with("t2")));
        assert_eq!(view.meta["included"], 5);
    }

    #[test]
    fn done_children_of_a_selected_parent_are_not_included() {
        let tree = vec![parent(
            task("t1", "in_progress"),
            vec![task("t1.1", "done"), task("t1.2", "skipped")],
        )];
        let view = Req::new(40).run(&tree);
        assert_eq!(ids(&view), vec!["t1".to_string()]);
    }

    #[test]
    fn the_output_keeps_the_sibling_order_of_the_original_tree() {
        let tree = vec![
            task("t3", "todo"),
            task("t1", "todo"),
            parent(
                task("t2", "todo"),
                vec![task("t2.b", "todo"), task("t2.a", "todo")],
            ),
        ];
        let mut req = Req::new(40);
        req.previous = strings(&["t2.a", "t1", "t3", "t2.b"]);
        let view = req.run(&tree);
        assert_eq!(
            ids(&view),
            ["t3", "t1", "t2", "t2.b", "t2.a"]
                .map(String::from)
                .to_vec()
        );
    }

    #[test]
    fn a_task_in_two_rules_counts_once_and_keeps_the_first_reason() {
        let tree = vec![task("t1", "todo"), task("t2", "todo")];
        let mut req = Req::new(1);
        req.previous = strings(&["t1"]);
        req.branch = ["t1".to_string(), "t2".to_string()].into_iter().collect();
        let view = req.run(&tree);
        assert_eq!(reason_of(&view, "t1").as_deref(), Some("previous_session"));
        // t1 spent the budget of 1, t2 does not fit.
        assert_eq!(reason_of(&view, "t2"), None);
    }

    // --- task_tree_meta -------------------------------------------------------------

    #[test]
    fn meta_counts_what_was_left_out() {
        let tree = vec![
            task("t1", "in_progress"),
            milestone(task("t2", "todo"), "v1"),
            task("t3", "todo"),
            task("t4", "done"),
            task("t5", "todo"),
        ];
        let view = Req::new(40).run(&tree);
        // t3 and t5 qualify for rule 7 and fit; make the budget tight instead.
        assert_eq!(view.meta["omitted_total"], 2, "t2 (other milestone) and t4");
        let mut tight = Req::new(1);
        tight.previous = Vec::new();
        let view = tight.run(&tree);
        assert_eq!(view.meta["mode"], "compact");
        assert_eq!(view.meta["included"], 2);
        assert_eq!(view.meta["omitted_total"], 3);
        assert_eq!(
            view.meta["omitted_by_status"],
            json!({"done": 1, "todo": 2})
        );
        assert_eq!(view.meta["omitted_without_milestone"], 2);
        assert_eq!(view.meta["truncated"], true);
    }

    #[test]
    fn meta_when_everything_fits() {
        let tree = vec![task("t1", "in_progress"), task("t2", "in_progress")];
        let view = Req::new(40).run(&tree);
        assert_eq!(view.meta["omitted_total"], 0);
        assert_eq!(view.meta["omitted_by_status"], json!({}));
        assert_eq!(view.meta["truncated"], false);
        let keys: Vec<&str> = view
            .meta
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        for key in [
            "mode",
            "included",
            "omitted_total",
            "omitted_by_status",
            "omitted_without_milestone",
            "truncated",
            "hint",
        ] {
            assert!(keys.contains(&key), "{key} missing: {keys:?}");
        }
    }

    #[test]
    fn meta_omitted_total_counts_tasks_the_index_never_listed() {
        // `done_task_limit` can leave tasks out of the tree that the summary
        // still counts; they are omitted as well.
        let tree = vec![task("t1", "in_progress")];
        let mut summary = summarize(&tree);
        summary.total = 4;
        summary.by_status.insert("done".to_string(), 3);
        let view = compact_view(
            &tree,
            &summary,
            &ScopeRequest {
                explicit_scope: &[],
                previous_related: &[],
                branch_ids: &BTreeSet::new(),
                current_milestone: None,
                limit: 40,
            },
        )
        .expect("compact view");
        assert_eq!(view.meta["omitted_total"], 3);
        assert_eq!(view.meta["omitted_by_status"], json!({"done": 3}));
    }

    #[test]
    fn full_meta_reports_nothing_omitted() {
        let tree = vec![parent(
            task("t1", "todo"),
            vec![task("t1.1", "done"), task("t1.2", "todo")],
        )];
        let meta = full_meta(&tree);
        assert_eq!(meta["mode"], "full");
        assert_eq!(meta["included"], 3);
        assert_eq!(meta["omitted_total"], 0);
        assert_eq!(meta["omitted_by_status"], json!({}));
        assert_eq!(meta["omitted_without_milestone"], 0);
        assert_eq!(meta["truncated"], false);
    }

    #[test]
    fn meta_hint_names_the_ways_to_get_the_rest() {
        let view = Req::new(40).run(&[task("t1", "todo")]);
        let hint = view.meta["hint"].as_str().unwrap();
        for word in [
            "handoff_list_tasks",
            "status_filter",
            "ids",
            "limit",
            "fields",
            "task_tree",
            "full",
        ] {
            assert!(hint.contains(word), "{word} missing from {hint}");
        }
    }

    #[test]
    fn selected_nodes_carry_their_task_fields_and_scope_reason() {
        let mut t = task("t1", "in_progress");
        t.assignee = Some("alice".to_string());
        t.dependencies = strings(&["t0"]);
        t.children = vec![task("t1.1", "done")];
        let view = Req::new(40).run(&[t]);
        let node = &view.tree[0];
        assert_eq!(node["title"], "title t1");
        assert_eq!(node["assignee"], "alice");
        assert_eq!(node["dependencies"], json!(["t0"]));
        assert_eq!(node["scope_reason"], "active");
        assert!(
            node.get("children").is_none(),
            "no unselected child is listed"
        );
    }

    // --- current milestone -------------------------------------------------------------

    fn milestones(defs: &[(&str, Option<&str>, Option<i64>)]) -> HashMap<String, MilestoneConfig> {
        defs.iter()
            .map(|(name, status, order)| {
                (
                    name.to_string(),
                    MilestoneConfig {
                        status: status.map(str::to_string),
                        order: *order,
                        ..MilestoneConfig::default()
                    },
                )
            })
            .collect()
    }

    #[test]
    fn the_lowest_order_active_milestone_is_current() {
        let ms = milestones(&[
            ("b", Some("active"), Some(2)),
            ("a", Some("active"), Some(1)),
            ("c", Some("planning"), Some(0)),
            ("d", None, Some(0)),
        ]);
        assert_eq!(current_milestone(&ms, &[]).as_deref(), Some("a"));
    }

    #[test]
    fn an_unordered_active_milestone_sorts_last_and_ties_break_by_name() {
        let ms = milestones(&[
            ("z", Some("active"), None),
            ("y", Some("active"), None),
            ("x", Some("active"), Some(5)),
        ]);
        assert_eq!(current_milestone(&ms, &[]).as_deref(), Some("x"));
        let ms = milestones(&[("z", Some("active"), None), ("y", Some("active"), None)]);
        assert_eq!(current_milestone(&ms, &[]).as_deref(), Some("y"));
    }

    #[test]
    fn without_an_active_milestone_the_one_of_the_in_progress_tasks_is_current() {
        let ms = milestones(&[("v1", None, Some(2)), ("v2", Some("planning"), Some(1))]);
        let tree = vec![
            milestone(task("t1", "in_progress"), "v1"),
            milestone(task("t2", "todo"), "v2"),
        ];
        assert_eq!(current_milestone(&ms, &tree).as_deref(), Some("v1"));
        let tree = vec![
            milestone(task("t1", "in_progress"), "v1"),
            milestone(task("t2", "in_progress"), "v2"),
        ];
        assert_eq!(
            current_milestone(&ms, &tree).as_deref(),
            Some("v2"),
            "lower order"
        );
    }

    #[test]
    fn no_active_milestone_and_no_in_progress_task_means_no_current_milestone() {
        let ms = milestones(&[("v1", None, Some(1))]);
        let tree = vec![milestone(task("t1", "todo"), "v1")];
        assert_eq!(current_milestone(&ms, &tree), None);
    }

    // --- related_task_ids ----------------------------------------------------------------

    #[test]
    fn related_ids_are_in_flight_then_previous_then_claimed_without_duplicates() {
        let tree = vec![
            task("t1", "in_progress"),
            task("t2", "todo"),
            task("t3", "done"),
            task("t4", "todo"),
            task("t5", "todo"),
            task("t6", "review"),
        ];
        let (ids, truncated) = related_task_ids(
            &tree,
            &strings(&["t2", "t3", "t404", "t1"]),
            &strings(&["t2", "t5", "t404"]),
        );
        assert_eq!(ids, strings(&["t1", "t6", "t2", "t5"]));
        assert!(!truncated);
    }

    #[test]
    fn related_ids_are_cut_at_twenty_and_flagged() {
        let tree: Vec<TaskIndex> = (1..=25)
            .map(|i| task(&format!("t{i}"), "in_progress"))
            .collect();
        let (ids, truncated) = related_task_ids(&tree, &[], &[]);
        assert_eq!(ids.len(), 20);
        assert!(truncated);
        let tree: Vec<TaskIndex> = (1..=20)
            .map(|i| task(&format!("t{i}"), "in_progress"))
            .collect();
        let (ids, truncated) = related_task_ids(&tree, &[], &[]);
        assert_eq!(ids.len(), 20);
        assert!(!truncated);
    }

    #[test]
    fn a_polluted_previous_session_cannot_flood_the_related_ids() {
        let tree: Vec<TaskIndex> = (1..=260).map(|i| task(&format!("t{i}"), "todo")).collect();
        let previous: Vec<String> = (1..=260).map(|i| format!("t{i}")).collect();
        let (ids, truncated) = related_task_ids(&tree, &previous, &[]);
        assert_eq!(ids.len(), 20);
        assert!(truncated);
    }

    #[test]
    fn plain_todos_are_not_related() {
        let tree = vec![task("t1", "todo"), task("t2", "todo")];
        let (ids, truncated) = related_task_ids(&tree, &[], &[]);
        assert!(ids.is_empty());
        assert!(!truncated);
    }
}
