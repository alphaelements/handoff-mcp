use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::HandlerContext;
use crate::storage::config::read_config;
use crate::storage::tasks::{
    build_task_index, build_task_index_with_expiry, find_task_dir_by_id, read_task, TaskIndex,
    TaskSummary,
};

/// Maximum depth (relative to the base project dir) scanned for nested
/// `.handoff/` child projects.
const MAX_CHILD_SCAN_DEPTH: usize = 5;

/// Directory names skipped while scanning for child projects.
const DEFAULT_SCAN_EXCLUDES: &[&str] = &["node_modules", ".git", "target", "dist", ".next"];

/// Allowed values of `fields`: the keys a serialized task node can carry.
const TASK_LIST_FIELDS: &[&str] = &[
    "id",
    "title",
    "status",
    "schedule",
    "dependencies",
    "order",
    "assignee",
    "lock",
    "children",
];

/// Keys that survive a `fields` projection regardless of what was asked for:
/// the task id, and the per-project annotations `include_children` adds.
const ALWAYS_KEPT_KEYS: &[&str] = &["id", "project_name", "project_dir", "task_ref"];

/// Reads `ids`: absent or `null` -> `None` (no id filter). Otherwise an array
/// of strings, trimmed, with blanks dropped and duplicates collapsed. An
/// array that normalizes to nothing is `Some(empty)`, which matches no task:
/// the caller asked for specific tasks, so "none" must not widen to "all".
fn parse_ids(arguments: &Value) -> Result<Option<HashSet<String>>> {
    let items = match arguments.get("ids") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Array(items)) => items,
        Some(other) => anyhow::bail!("'ids' must be an array of strings (got {other})"),
    };
    let mut ids = HashSet::with_capacity(items.len());
    for item in items {
        let Some(id) = item.as_str() else {
            anyhow::bail!("'ids' must be an array of strings (got element {item})");
        };
        let id = id.trim();
        if !id.is_empty() {
            ids.insert(id.to_string());
        }
    }
    Ok(Some(ids))
}

/// Reads `include_descendants`: absent or `null` -> `false`. Anything but a
/// boolean is an error rather than a silent `false`, so a typo cannot quietly
/// turn a parent selector back into a bare parent.
fn parse_include_descendants(arguments: &Value) -> Result<bool> {
    match arguments.get("include_descendants") {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(flag)) => Ok(*flag),
        Some(other) => anyhow::bail!("'include_descendants' must be a boolean (got {other})"),
    }
}

/// Node counts seen by [`apply_limit`], summed over every project listed.
#[derive(Default)]
struct LimitStats {
    matched: usize,
    returned: usize,
}

impl LimitStats {
    /// `task_tree_meta` for the response, only when `limit` actually cut nodes:
    /// a response that fits (or has no limit) keeps its pre-existing shape.
    fn meta(&self, limit: Option<usize>) -> Option<Value> {
        let limit = limit?;
        (self.returned < self.matched).then(|| {
            json!({
                "truncated": true,
                "limit": limit,
                "returned": self.returned,
                "matched": self.matched,
            })
        })
    }
}

fn count_nodes(tree: &[TaskIndex]) -> usize {
    tree.iter().map(|n| 1 + count_nodes(&n.children)).sum()
}

/// Keeps nodes in depth-first (pre-order) order while `budget` lasts: a node
/// costs 1 and its children are cut to what is left, so nested output is
/// bounded too.
fn take_nodes(tree: Vec<TaskIndex>, budget: &mut usize) -> Vec<TaskIndex> {
    let mut kept = Vec::new();
    for mut node in tree {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        node.children = take_nodes(std::mem::take(&mut node.children), budget);
        kept.push(node);
    }
    kept
}

/// Keeps the first `limit` nodes of `tree` counted depth-first (children
/// included), recording matched/returned counts in `stats`.
fn apply_limit(
    tree: Vec<TaskIndex>,
    limit: Option<usize>,
    stats: &mut LimitStats,
) -> Vec<TaskIndex> {
    let Some(limit) = limit else {
        return tree;
    };
    stats.matched += count_nodes(&tree);
    let mut budget = limit;
    let kept = take_nodes(tree, &mut budget);
    stats.returned += limit - budget;
    kept
}

/// Projects task-node JSON down to `fields` (plus [`ALWAYS_KEPT_KEYS`]).
/// `children` is kept, and projected recursively, only when requested or when
/// `force_children` is set (an `ids` selection or any other active filter: a
/// matched task may sit below an ancestor kept only for connectivity, and
/// dropping `children` would leave that ancestor alone in the result).
fn project_nodes(nodes: Vec<Value>, fields: &[String], force_children: bool) -> Vec<Value> {
    let keep_children = force_children || fields.iter().any(|f| f == "children");
    nodes
        .into_iter()
        .map(|node| {
            let mut node = super::list_args::project_object(node, |k| {
                ALWAYS_KEPT_KEYS.contains(&k)
                    || (force_children && k == "children")
                    || fields.iter().any(|f| f == k)
            });
            if keep_children {
                if let Some(Value::Array(children)) = node.get_mut("children").map(Value::take) {
                    node["children"] =
                        Value::Array(project_nodes(children, fields, force_children));
                }
            }
            node
        })
        .collect()
}

/// Serializes `tree` and applies the optional `fields` projection.
fn nodes_to_json(
    tree: &[TaskIndex],
    fields: Option<&[String]>,
    force_children: bool,
) -> Result<Vec<Value>> {
    let nodes = match serde_json::to_value(tree).context("Failed to serialize task list")? {
        Value::Array(nodes) => nodes,
        other => anyhow::bail!("task tree serialized to a non-array: {other}"),
    };
    Ok(match fields {
        Some(fields) => project_nodes(nodes, fields, force_children),
        None => nodes,
    })
}

pub fn handle(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let project_dir = &ctx.project_dir;
    let handoff = &ctx.handoff_dir;
    let tasks_dir = handoff.join("tasks");
    let config_path = handoff.join("config.toml");

    // Validate the narrowing arguments before touching the task store.
    let ids = parse_ids(arguments)?;
    let include_descendants = parse_include_descendants(arguments)?;
    let limit = super::list_args::parse_limit(arguments)?;
    let fields = super::list_args::parse_fields(arguments, TASK_LIST_FIELDS)?;

    // Lazy scan (spec 3.3.5, 7.2) is now folded into
    // `build_task_index_with_expiry` itself (P-M6, wiki/240-performance-
    // design.md §3 C6 / §4): the tree this call builds below already
    // reflects any lease it reclaims along the way, in the same single pass,
    // instead of a separate `scan_expired_leases` walk before it. Reclaiming
    // here is limited to *this* project's own tree, per wiki/190's
    // "Lazy scan の対象操作" allowlist — the cross-project child scan below
    // (`include_children`) uses the read-only `build_task_index` instead
    // (rework round 2: must not mutate another project's task files).
    let done_task_limit = if config_path.exists() {
        read_config(&config_path)
            .map(|c| c.settings.done_task_limit)
            .unwrap_or(10)
    } else {
        10
    };

    let (tree, summary, _expired_ids) = build_task_index_with_expiry(&tasks_dir, done_task_limit)?;

    let status_filter = arguments.get("status_filter").and_then(|v| v.as_str());
    let assignee_filter = arguments.get("assignee_filter").and_then(|v| v.as_str());
    let milestone_filter = arguments.get("milestone_filter").and_then(|v| v.as_str());
    let priority_filter = arguments.get("priority_filter").and_then(|v| v.as_str());
    let label_filter = arguments.get("label_filter").and_then(|v| v.as_str());
    // wiki/260-vmodel-m2-design.md §4.11 (M2-13, FR-602): `layer`/`role`
    // filter a task down to its `requirement`-type `task_links` — `role`
    // alone needs no document read at all (it's a plain `TaskLink.role`
    // compare); `layer` needs each linked stable_id's effective layer, so
    // the one-pass `DocSet` scan below only runs when `layer` is actually
    // given (§3.4/§4.11: "指定時だけ DocSet のメタデータで stable_id →
    // 層を引く").
    let layer_filter = arguments.get("layer").and_then(|v| v.as_str());
    let role_filter = arguments.get("role").and_then(|v| v.as_str());
    let stable_id_layer: Option<HashMap<String, String>> = layer_filter.map(|_| {
        let docs = crate::storage::docs::DocSet::load(handoff)
            .map(|d| d.docs().to_vec())
            .unwrap_or_default();
        crate::trace::adapter::collect_trace_items(&docs)
            .into_iter()
            .filter_map(|i| i.layer.map(|l| (i.stable_id, l)))
            .collect()
    });

    let filters = Filters {
        status: status_filter,
        assignee: assignee_filter,
        milestone: milestone_filter,
        priority: priority_filter,
        label: label_filter,
        layer: layer_filter,
        role: role_filter,
        stable_id_layer: stable_id_layer.as_ref(),
        ids: ids.as_ref(),
        include_descendants,
    };

    // Order: filters (ids included) -> limit -> fields projection.
    let filtered_tree = if filters.any_active() {
        filter_tree(&tree, &filters, &tasks_dir, false)
    } else {
        tree
    };
    let mut limit_stats = LimitStats::default();
    let filtered_tree = apply_limit(filtered_tree, limit, &mut limit_stats);
    // A narrowed listing keeps `children` through a `fields` projection, or the
    // subtasks that matched would vanish behind their ancestors.
    let keep_matched_children = filters.any_active();

    let include_children = arguments
        .get("include_children")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !include_children {
        let mut result = serde_json::json!({
            "task_tree": nodes_to_json(&filtered_tree, fields.as_deref(), keep_matched_children)?,
            "task_summary": summary,
        });
        insert_limit_meta(&mut result, &limit_stats, limit);
        return serde_json::to_string_pretty(&result).context("Failed to serialize task list");
    }

    // Determine own project name for annotation (defaults to config, fallback to "root").
    let own_project_name = if config_path.exists() {
        read_config(&config_path)
            .map(|c| c.project.name)
            .unwrap_or_else(|_| "root".to_string())
    } else {
        "root".to_string()
    };
    let own_project_dir = project_dir.to_string_lossy().to_string();

    // `fields` is applied once at the end, over the annotated nodes (the
    // annotation keys are in `ALWAYS_KEPT_KEYS`).
    let mut aggregated: Vec<Value> =
        annotate_tasks_json(filtered_tree, &own_project_name, &own_project_dir);
    let mut total_summary = summary;

    for (child_name, child_dir) in discover_child_projects(project_dir, MAX_CHILD_SCAN_DEPTH) {
        let child_handoff = child_dir.join(".handoff");
        let child_tasks_dir = child_handoff.join("tasks");
        let child_config_path = child_handoff.join("config.toml");
        let child_done_task_limit = read_config(&child_config_path)
            .map(|c| c.settings.done_task_limit)
            .unwrap_or(10);

        let (child_tree, child_summary) =
            match build_task_index(&child_tasks_dir, child_done_task_limit) {
                Ok(v) => v,
                Err(_) => continue,
            };

        // Note: `layer` filtering for a child project reuses the *root*
        // project's `stable_id_layer` map built above — correct only when
        // the child shares the same requirement documents as the root. This
        // is a pre-existing `include_children` limitation class (the same
        // `filters`/`Filters` instance is already reused verbatim for every
        // child here for `priority`/`label` too), not one this task
        // introduces; `role` filtering is unaffected (no document lookup).
        let child_filtered = if filters.any_active() {
            filter_tree(&child_tree, &filters, &child_tasks_dir, false)
        } else {
            child_tree
        };
        let child_filtered = apply_limit(child_filtered, limit, &mut limit_stats);

        let child_dir_str = child_dir.to_string_lossy().to_string();
        aggregated.extend(annotate_tasks_json(
            child_filtered,
            &child_name,
            &child_dir_str,
        ));
        merge_summary(&mut total_summary, &child_summary);
    }

    if let Some(fields) = fields.as_deref() {
        aggregated = project_nodes(aggregated, fields, keep_matched_children);
    }

    let mut result = serde_json::json!({
        "task_tree": aggregated,
        "task_summary": total_summary,
    });
    insert_limit_meta(&mut result, &limit_stats, limit);

    serde_json::to_string_pretty(&result).context("Failed to serialize task list")
}

fn insert_limit_meta(result: &mut Value, stats: &LimitStats, limit: Option<usize>) {
    if let Some(meta) = stats.meta(limit) {
        result["task_tree_meta"] = meta;
    }
}

fn merge_summary(base: &mut TaskSummary, other: &TaskSummary) {
    base.total += other.total;
    for (status, count) in &other.by_status {
        *base.by_status.entry(status.clone()).or_insert(0) += count;
    }
    base.overdue_count += other.overdue_count;
    if let Some(other_est) = other.total_estimate_hours {
        base.total_estimate_hours = Some(base.total_estimate_hours.unwrap_or(0.0) + other_est);
    }
    if let Some(other_act) = other.total_actual_hours {
        base.total_actual_hours = Some(base.total_actual_hours.unwrap_or(0.0) + other_act);
    }
    if base.total > 0 {
        let done = *base.by_status.get("done").unwrap_or(&0) as f64;
        let skipped = *base.by_status.get("skipped").unwrap_or(&0) as f64;
        base.completion_rate = Some((done + skipped) / base.total as f64);
    }
}

/// Annotate a task tree with `project_name`/`project_dir` and a composite
/// `task_ref` disambiguator (`{project_name}-{project_dir_hash}:{id}`) for
/// cross-project display purposes.
///
/// The original `id` field is deliberately left **unmodified**: it is the
/// value accepted by `handoff_get_task`/`handoff_update_task`/etc. (scoped by
/// the sibling `project_dir` field), and it is also what `dependencies`
/// entries reference. Rewriting `id` would make it unusable with every other
/// tool and would desync it from `dependencies`, which still contain raw ids.
///
/// `project_name` alone is free-form user text with no cross-directory
/// uniqueness guarantee, so two child projects can share the same name. The
/// `task_ref` additionally embeds a short hash of `project_dir` (which is
/// unique per filesystem location) to guarantee it never collides even when
/// `project_name` is duplicated.
fn annotate_tasks_json(tasks: Vec<TaskIndex>, project_name: &str, project_dir: &str) -> Vec<Value> {
    let dir_hash = short_hash(project_dir);
    tasks
        .into_iter()
        .map(|t| {
            let mut v = serde_json::to_value(&t).unwrap_or_default();
            if let Some(obj) = v.as_object_mut() {
                obj.insert("project_name".to_string(), json!(project_name));
                obj.insert("project_dir".to_string(), json!(project_dir));
                if let Some(id) = obj.get("id").and_then(|v| v.as_str()).map(String::from) {
                    obj.insert(
                        "task_ref".to_string(),
                        json!(format!("{project_name}-{dir_hash}:{id}")),
                    );
                }
                if let Some(children) = obj.remove("children") {
                    if let Ok(child_tasks) = serde_json::from_value::<Vec<TaskIndex>>(children) {
                        obj.insert(
                            "children".to_string(),
                            json!(annotate_tasks_json(child_tasks, project_name, project_dir)),
                        );
                    }
                }
            }
            v
        })
        .collect()
}

/// Deterministic short hash (FNV-1a, hex-encoded) used to disambiguate
/// composite task IDs across child projects that share a `project_name`.
/// Not a dependency addition — plain FNV-1a is sufficient since this only
/// needs to be stable and collision-resistant for distinct directory paths,
/// not cryptographically secure.
fn short_hash(input: &str) -> String {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET_BASIS;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:08x}")
}

fn discover_child_projects(base: &Path, max_depth: usize) -> Vec<(String, PathBuf)> {
    let mut results = Vec::new();
    scan_children(base, 1, max_depth, &mut results);
    results
}

fn scan_children(dir: &Path, depth: usize, max_depth: usize, results: &mut Vec<(String, PathBuf)>) {
    if depth > max_depth {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') || DEFAULT_SCAN_EXCLUDES.contains(&name_str.as_ref()) {
            continue;
        }
        let path = entry.path();
        let config_path = path.join(".handoff").join("config.toml");
        if config_path.exists() {
            if let Ok(config) = read_config(&config_path) {
                results.push((config.project.name.clone(), path.clone()));
            }
        }
        scan_children(&path, depth + 1, max_depth, results);
    }
}

struct Filters<'a> {
    status: Option<&'a str>,
    assignee: Option<&'a str>,
    milestone: Option<&'a str>,
    priority: Option<&'a str>,
    label: Option<&'a str>,
    /// wiki/260 §4.11 (M2-13): matches a task with at least one
    /// `requirement`-type `task_links` entry whose linked stable_id's
    /// effective layer equals this value (via `stable_id_layer`).
    layer: Option<&'a str>,
    /// wiki/260 §4.11 (M2-13): matches a task with at least one
    /// `requirement`-type `task_links` entry whose `role` (defaulting to
    /// `"implements"` when unset, §2.5) equals this value.
    role: Option<&'a str>,
    /// stable_id -> effective layer, built once by the caller only when
    /// `layer` is set (§3.4: "指定時だけ"). `None` when `layer` is unset, or
    /// when it is set but `DocSet::load` failed (treated as "no layer data
    /// available" — every `layer`-filtered match then fails closed rather
    /// than panicking or silently ignoring the filter).
    stable_id_layer: Option<&'a HashMap<String, String>>,
    /// Exact task ids to keep (already trimmed/de-duplicated). `Some(empty)`
    /// matches nothing; `None` is "no id filter".
    ids: Option<&'a HashSet<String>>,
    /// With `ids`: a node matched by id also keeps its whole subtree, which is
    /// still narrowed by every other filter (the `ids` test is waived for the
    /// descendants only). No effect without `ids`.
    include_descendants: bool,
}

impl Filters<'_> {
    fn any_active(&self) -> bool {
        self.ids.is_some()
            || self.status.is_some()
            || self.assignee.is_some()
            || self.milestone.is_some()
            || self.priority.is_some()
            || self.label.is_some()
            || self.layer.is_some()
            || self.role.is_some()
    }

    /// Whether evaluating this filter set needs the task's own `TaskData`
    /// (`priority`/`label`/`layer`/`role` all do; `status`/`assignee`/
    /// `milestone` are already present on the lighter-weight `TaskIndex`).
    fn needs_task_data(&self) -> bool {
        self.priority.is_some()
            || self.label.is_some()
            || self.layer.is_some()
            || self.role.is_some()
    }

    /// wiki/260 §4.11/§3.4 (M2-13): does `link` (one of the task's
    /// `task_links`) satisfy the active `layer`/`role` filters? Only
    /// `link_type == "requirement"` entries are ever eligible.
    fn requirement_link_matches(&self, link: &crate::storage::tasks::TaskLink) -> bool {
        if link.link_type != "requirement" {
            return false;
        }
        if let Some(role) = self.role {
            let effective_role = link.role.as_deref().unwrap_or("implements");
            if effective_role != role {
                return false;
            }
        }
        if let Some(layer) = self.layer {
            let Some(stable_id) = link.label.as_deref() else {
                return false;
            };
            let Some(map) = self.stable_id_layer else {
                return false;
            };
            if map.get(stable_id).map(String::as_str) != Some(layer) {
                return false;
            }
        }
        true
    }

    /// Whether `node` was itself picked by `ids` and therefore waives the
    /// `ids` test for its descendants (`include_descendants`).
    fn expands_subtree(&self, node: &TaskIndex) -> bool {
        self.include_descendants && self.ids.is_some_and(|ids| ids.contains(&node.id))
    }

    /// `ids_waived`: an ancestor was picked by `ids` with `include_descendants`,
    /// so this node is not required to be listed in `ids` itself.
    fn matches(
        &self,
        node: &TaskIndex,
        data: Option<&crate::storage::tasks::TaskData>,
        ids_waived: bool,
    ) -> bool {
        if let Some(ids) = self.ids {
            if !ids_waived && !ids.contains(&node.id) {
                return false;
            }
        }
        if let Some(status) = self.status {
            if node.status != status {
                return false;
            }
        }
        if let Some(assignee) = self.assignee {
            if node.assignee.as_deref() != Some(assignee) {
                return false;
            }
        }
        if let Some(milestone) = self.milestone {
            let ms = node.schedule.as_ref().and_then(|s| s.milestone.as_deref());
            if ms != Some(milestone) {
                return false;
            }
        }
        if let Some(priority) = self.priority {
            if let Some(d) = data {
                if d.priority.as_deref() != Some(priority) {
                    return false;
                }
            } else {
                return false;
            }
        }
        if let Some(label) = self.label {
            if let Some(d) = data {
                if !d.labels.iter().any(|l| l == label) {
                    return false;
                }
            } else {
                return false;
            }
        }
        if self.layer.is_some() || self.role.is_some() {
            let Some(d) = data else { return false };
            if !d
                .task_links
                .iter()
                .any(|l| self.requirement_link_matches(l))
            {
                return false;
            }
        }
        true
    }
}

fn filter_tree(
    tree: &[TaskIndex],
    filters: &Filters,
    tasks_dir: &Path,
    ids_waived: bool,
) -> Vec<TaskIndex> {
    tree.iter()
        .filter_map(|node| {
            let children_waived = ids_waived || filters.expands_subtree(node);
            let children = filter_tree(&node.children, filters, tasks_dir, children_waived);
            let data = if filters.needs_task_data() {
                find_task_dir_by_id(tasks_dir, &node.id)
                    .ok()
                    .flatten()
                    .and_then(|dir| read_task(&dir).ok().flatten())
                    .map(|(d, _status)| d)
            } else {
                None
            };
            if filters.matches(node, data.as_ref(), ids_waived) || !children.is_empty() {
                Some(TaskIndex {
                    id: node.id.clone(),
                    title: node.title.clone(),
                    status: node.status.clone(),
                    schedule: node.schedule.clone(),
                    dependencies: node.dependencies.clone(),
                    order: node.order,
                    assignee: node.assignee.clone(),
                    priority: node.priority.clone(),
                    lock: node.lock.clone(),
                    task_links: node.task_links.clone(),
                    children,
                })
            } else {
                None
            }
        })
        .collect()
}
