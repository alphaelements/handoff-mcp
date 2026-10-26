//! `handoff_trace_tasks` (wiki/260-vmodel-m2-design.md §4.9, M2-16, FR-605):
//! generates one task per V-model item that still lacks the task role it
//! needs — `implements` for a left-side (definition) item with no task
//! implementing it yet, `executes` for a right-side (verification) item
//! with no task executing it yet *and* whose latest recorded result is not
//! `"pass"` (an already-passing verifier needs no task). Idempotent: an item
//! that already has a task holding the role this call would otherwise
//! generate is reported in `skipped`, never regenerated.
//!
//! `mode` defaults to `"preview"` (no write). `mode="apply"` creates the
//! tasks via [`super::update_task::handle_create`] — the exact same
//! creation path `handoff_update_task` itself uses for a brand-new task
//! (§4.9's own completion criterion: "作成は `update_task` の作成処理を共通
//! 関数として使う") — so every invariant that path already enforces
//! (status/priority validation, the done-guard pre-check, dependency
//! validation, `require_estimate_hours`, the `requirement_ids`/
//! `requirement_roles` reverse-link write) applies identically here, with no
//! parallel task-writing code of this module's own.
//!
//! Item scanning goes through the same E6 fully-read-only load
//! (`trace_readonly::load_trace_input_fully_read_only`) every other
//! read-mostly trace tool in this session uses (`trace_lint`/`trace_matrix`/
//! `trace_impact`) — in-memory-only layer-doc resync, `runs::
//! load_latest_readonly` (never `runs::sync`), `task_ids` resolved from the
//! task side without self-repair. This module's only write path at all is
//! task creation (`mode="apply"`'s calls into `update_task::handle_create`,
//! which in turn writes exactly one task file per generated task plus that
//! task's own reverse requirement link) — nothing here ever touches a layer
//! document.
//!
//! `items`/`select` argument validation (t360.20.16 rework, reviewer round
//! 1): both keys are checked for *presence* before either is ever read as an
//! array/object, so `items`/`select` being mutually exclusive is enforced
//! even when one of the two is itself malformed — a scalar `items: "X"`, a
//! non-array `select.layers`/`select.gap_kinds`, a non-string
//! `select.dev_stage`, or a `select` that isn't an object all bail rather
//! than silently collapsing to "no filter" (the same defect class
//! `trace_lint`'s `rules` filter was fixed for, t360.20.34). An unknown
//! `select.gap_kinds` id or a `select.dev_stage` outside the 5-value
//! `SubItem.dev_stage` enum also bail, rather than silently matching zero
//! items. A `stable_id` assigned to more than one `SubItem` (a malformed
//! trace graph, `GapKind::DuplicateId`) still yields at most one target per
//! call, so `mode="apply"` can never create two tasks for the same item in a
//! single invocation.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{bail, Result};
use serde_json::{json, Value};

use super::HandlerContext;
use crate::storage::config::{push_config_warning, read_config, read_trace_config_lenient};
use crate::storage::docs::layer::LayerRegistry;
use crate::storage::docs::model::{UNSET_DEV_STAGE, VALID_DEV_STAGES};
use crate::storage::events::{append_event, EventRecord};
use crate::storage::tasks::{build_task_index, TaskIndex};
use crate::trace::profile::resolve_project_profile;
use crate::trace::types::{GapKind, TaskLinkRole};
use crate::trace::TraceGraph;

const DEFAULT_LIMIT: u64 = 20;

/// `events.jsonl` event kinds written by `mode="apply"` (t402). `EventRecord`
/// has no `stable_id` field, so it travels in `detail` as compact JSON.
const EVENT_TRACE_TASKS_CREATE: &str = "trace_tasks_create";
const EVENT_TRACE_TASKS_SKIP: &str = "trace_tasks_skip";

/// `skipped[].reason` values: the existing task was found through its
/// requirement link (the authority) or, failing that, through its title.
const SKIP_REASON_LINK: &str = "existing_link";
const SKIP_REASON_TITLE: &str = "title_match";

/// REQ-GUI-REQ-002: a SubItem carrying a `- mock:` attribute gets a second,
/// separate VRT verification task next to its ordinary implementation task.
/// Its title is `"[VRT] {stable_id} {title}"`; it is its own idempotency key
/// (matched on the full title) and carries no requirement link, so the
/// ordinary task's `implements` bookkeeping is untouched.
const VRT_TITLE_PREFIX: &str = "[VRT]";
const VRT_ROLE: &str = "vrt";
const VRT_LABELS: [&str; 2] = ["vrt", "gui-verification"];
/// The three fixed done_criteria of every VRT task (REQ-GUI-REQ-002).
const VRT_DONE_CRITERIA: [&str; 3] = [
    "ゴールデンスクリーンショット取得済み",
    "VRT（ビジュアルリグレッションテスト）PASS",
    "AI 視覚検証 PASS",
];

fn to_json(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

/// §2.5 role inference duplicated from `docs::infer_role_from_category`
/// (`category == "check"` -> `"executes"`, else `"implements"`) rather than
/// widening that function's visibility for this one extra caller — the same
/// "a non-pub helper with exactly one external caller stays duplicated, not
/// exported" convention `trace_scaffold.rs`'s own
/// `resolve_doc_by_slug_or_id` doc comment already states for this
/// codebase, applied here so this module adds zero lines to `docs.rs`
/// (out of this task's scope, and under concurrent edit by another
/// developer in this session for `link_task`/`trace_lint` work).
fn infer_role_from_category(category: &str) -> &'static str {
    if category == "check" {
        "executes"
    } else {
        "implements"
    }
}

/// Existing-task lookups built in one pass over the task tree.
struct TitleIndex {
    /// Leading title token (the stable_id) -> id of the first task with it.
    by_token: HashMap<String, String>,
    /// Full title -> id of the first task with exactly that title (the VRT
    /// task's idempotency key).
    by_title: HashMap<String, String>,
}

/// stable_id -> id of the first existing task whose title is
/// `"{stable_id} {title}"` (the exact format this module generates), across
/// every task of the project in any status. A second line of idempotency
/// defence next to the requirement-link map: a task that lost (or never got)
/// its link — a failed link write, a hand-created task, a concurrent
/// registrar — must still stop a duplicate being generated. Only the leading
/// whitespace-delimited token is considered, so `REQ-0010 ...` never matches
/// `REQ-001`.
fn title_stable_id_index(handoff: &Path) -> Result<TitleIndex> {
    let tasks_dir = handoff.join("tasks");
    let mut index = TitleIndex {
        by_token: HashMap::new(),
        by_title: HashMap::new(),
    };
    if !tasks_dir.is_dir() {
        return Ok(index);
    }
    fn walk(nodes: &[TaskIndex], index: &mut TitleIndex) {
        for node in nodes {
            if let Some(token) = node.title.split_whitespace().next() {
                index
                    .by_token
                    .entry(token.to_string())
                    .or_insert_with(|| node.id.clone());
            }
            index
                .by_title
                .entry(node.title.clone())
                .or_insert_with(|| node.id.clone());
            walk(&node.children, index);
        }
    }
    let (tree, _) = build_task_index(&tasks_dir, u32::MAX)?;
    walk(&tree, &mut index);
    Ok(index)
}

/// Appends one `events.jsonl` line. The task / skip decision this records has
/// already been made (and, for a creation, committed), so a failed audit write
/// must not fail the call — it is surfaced in `warnings` instead of being lost.
fn log_event(
    handoff: &Path,
    event: &str,
    task_id: Option<&str>,
    detail: Value,
    warnings: &mut Vec<String>,
) {
    let record = EventRecord {
        ts: chrono::Utc::now().to_rfc3339(),
        event: event.to_string(),
        task_id: task_id.map(String::from),
        agent_id: None,
        session_id: None,
        detail: Some(detail.to_string()),
    };
    if let Err(e) = append_event(handoff, record) {
        warnings.push(format!("failed to record {event} event: {e}"));
    }
}

/// One item eligible for task generation, resolved ahead of `limit`
/// truncation and ahead of `mode` branching (preview/apply share this same
/// target list — `mode` only decides whether `update_task::handle_create`
/// actually runs for each one).
struct TargetItem {
    stable_id: String,
    role: &'static str,
    /// `SubItem.description` — the item's own title text.
    title: String,
    layer: String,
    scope_paths: Vec<String>,
    /// `(label, text)` per acceptance criterion that has stored text; feeds
    /// the generated task's `done_criteria`.
    acceptance_texts: Vec<(String, String)>,
    /// Id of the document the item was read from — named in the not-backfilled
    /// warning so the caller knows which document to re-save.
    doc_id: String,
    /// Number of acceptance criteria stored without text (`AcRef.text` is
    /// `None`, i.e. last synced by a binary that did not store it).
    unbackfilled_acceptance: usize,
    /// REQ-MS-PIPE-003: milestone for the generated task — the item's
    /// effective milestone (own, else its document's), else the call's
    /// `default_milestone`, else none.
    milestone: Option<String>,
    /// REQ-GUI-REQ-002: this target is the item's `[VRT]` verification task
    /// rather than its ordinary implementation/execution task.
    vrt: bool,
}

impl TargetItem {
    /// The `role` reported in `planned`/`created`/`skipped` entries.
    fn role_label(&self) -> &'static str {
        if self.vrt {
            VRT_ROLE
        } else {
            self.role
        }
    }

    fn task_title(&self) -> String {
        if self.vrt {
            format!("{VRT_TITLE_PREFIX} {} {}", self.stable_id, self.title)
        } else {
            format!("{} {}", self.stable_id, self.title)
        }
    }
}

/// The generated task's `done_criteria`: one unchecked entry per acceptance
/// criterion (`"<label>: <text>"`), or — for an item with no stored AC text —
/// a single title-based criterion so the task is never created without any.
fn done_criteria_json(acceptance_texts: &[(String, String)], title: &str) -> Value {
    if acceptance_texts.is_empty() {
        return json!([{ "item": format!("{title} の実装と検証"), "checked": false }]);
    }
    Value::Array(
        acceptance_texts
            .iter()
            .map(|(label, text)| json!({ "item": format!("{label}: {text}"), "checked": false }))
            .collect(),
    )
}

/// The fixed done_criteria of a VRT task (REQ-GUI-REQ-002), all unchecked.
fn vrt_done_criteria_json() -> Value {
    Value::Array(
        VRT_DONE_CRITERIA
            .iter()
            .map(|item| json!({ "item": item, "checked": false }))
            .collect(),
    )
}

/// Reads `v` as an array of strings. A non-array, or an array holding a
/// non-string element, bails — silently dropping such elements would shrink
/// the filter/target set unnoticed (the same "key present but wrong shape is
/// an error, never 'no filter'" policy as the surrounding `items`/`select`
/// parsing).
fn string_array(key: &str, noun: &str, v: &Value) -> Result<Vec<String>> {
    let arr = v
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("{key}: must be an array of {noun}, got {v}"))?;
    arr.iter()
        .map(|e| {
            e.as_str().map(String::from).ok_or_else(|| {
                anyhow::anyhow!("{key}: every element must be a string ({noun}), got {e}")
            })
        })
        .collect()
}

/// `handoff_trace_tasks` (§4.9). Input: exactly one of `items: [stable_id]`
/// or `select: {layers?, gap_kinds?, dev_stage?}` (omitting both scans every
/// item in the project); `parent_id?` (every generated task becomes a child
/// of this one, if given); `estimate_hours?` (required for `mode="apply"`
/// when `[settings] require_estimate_hours` is enabled project-wide — see
/// this function's own upfront check, not `update_task`'s per-task one,
/// since every generated task starts in status `"todo"`, which that
/// per-task rule exempts); `mode?` (`"preview"` default | `"apply"`);
/// `limit?` (default 20, caps generated/planned entries — `skipped` doesn't
/// count against it); `default_milestone?` (REQ-MS-PIPE-003: milestone for
/// generated tasks whose item has no effective milestone of its own).
/// An item with a `- mock:` attribute additionally yields a `[VRT]` task
/// (REQ-GUI-REQ-002).
pub fn handle_trace_tasks(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    // t360.20.16 rework (reviewer, round 1): `items`/`select` used to be read
    // with `.and_then(as_array)`/implicit object access, which collapses
    // "key present but wrong shape" into `None` ("no filter") exactly like
    // the `rules` defect `trace_lint` already fixed (t360.20.34) — a scalar
    // `items: "REQ-001"` used to silently mean "scan every item", and a
    // non-object `select` used to silently mean "no select filter" (and,
    // worse, let a malformed `items` slip past the mutual-exclusion check
    // below, since that check only ever saw the already-collapsed `None`).
    // Checking key *presence* first — before ever calling `.as_array()`/
    // `.is_object()` — keeps "key absent" (`None`, no filter) and "key
    // present but wrong type" (bail) distinguishable, matching `trace_lint`'s
    // own `rules_filter` parsing.
    let items_key_present = arguments.get("items").is_some();
    let select_key_present = arguments.get("select").is_some();
    if items_key_present && select_key_present {
        bail!("'items' and 'select' are mutually exclusive");
    }

    let items_arg: Option<Vec<String>> = match arguments.get("items") {
        None => None,
        Some(v) => Some(string_array("items", "stable ids", v)?),
    };
    let select_arg = match arguments.get("select") {
        None => None,
        Some(v) => {
            if !v.is_object() {
                bail!("select: must be an object, got {v}");
            }
            Some(v)
        }
    };
    let select_layers: Option<HashSet<String>> = match select_arg.and_then(|s| s.get("layers")) {
        None => None,
        Some(v) => Some(
            string_array("select.layers", "layer ids", v)?
                .into_iter()
                .collect(),
        ),
    };
    let select_gap_kinds: Option<Vec<String>> = match select_arg.and_then(|s| s.get("gap_kinds")) {
        None => None,
        Some(v) => {
            let kinds = string_array("select.gap_kinds", "gap kind ids", v)?;
            for kind in &kinds {
                if !GapKind::ALL.iter().any(|k| k.as_str() == kind) {
                    let known: Vec<&str> = GapKind::ALL.iter().map(|k| k.as_str()).collect();
                    bail!("select.gap_kinds: unknown gap kind {kind:?}; expected one of {known:?}");
                }
            }
            Some(kinds)
        }
    };
    let select_dev_stage = match select_arg.and_then(|s| s.get("dev_stage")) {
        None => None,
        Some(v) => {
            let stage = v
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("select.dev_stage: must be a string, got {v}"))?;
            if !VALID_DEV_STAGES.contains(&stage) {
                bail!(
                    "select.dev_stage: invalid dev_stage {stage:?}; expected one of \
                     {VALID_DEV_STAGES:?}"
                );
            }
            Some(stage)
        }
    };

    let parent_id = arguments.get("parent_id").and_then(|v| v.as_str());
    let estimate_hours = arguments.get("estimate_hours").and_then(|v| v.as_f64());
    if let Some(h) = estimate_hours {
        if h.is_nan() || h <= 0.0 {
            bail!("'estimate_hours' must be > 0 (got {h})");
        }
    }

    let default_milestone: Option<String> = match arguments.get("default_milestone") {
        None => None,
        Some(v) => match v.as_str().map(str::trim) {
            Some(m) if !m.is_empty() => Some(m.to_string()),
            _ => bail!("default_milestone: must be a non-empty string, got {v}"),
        },
    };

    let mode = arguments
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("preview");
    if mode != "preview" && mode != "apply" {
        bail!("Unknown mode '{mode}'; expected 'preview' or 'apply'.");
    }
    let limit = arguments
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_LIMIT) as usize;

    let require_estimate_hours = read_config(&handoff.join("config.toml"))
        .map(|c| c.settings.require_estimate_hours)
        .unwrap_or(true);
    if mode == "apply" && require_estimate_hours && estimate_hours.is_none() {
        bail!(
            "'estimate_hours' is required for mode=\"apply\" when [settings] \
             require_estimate_hours is enabled (the project default): every task \
             handoff_trace_tasks generates starts in status \"todo\" (exempt from \
             handoff_update_task's own per-task estimate rule), so this tool enforces its own \
             upfront, whole-batch estimate requirement instead of silently creating tasks with \
             no effort estimate. Pass estimate_hours (hours, > 0), or disable \
             require_estimate_hours project-wide."
        );
    }
    // wiki/260 §3.4 (M2-13): the same fail-safe fallback `update_task`'s own
    // `handle()` uses for an unrecognized `[trace] done_guard` value — never
    // silently escalate to "block", never silently disable it as "off".
    // Irrelevant in practice for every task this module creates today (they
    // always start in status "todo", which the done-guard check inside
    // `update_task::handle_create` only even looks at for "review"/"done"),
    // kept here (rather than hardcoding "warn" with no config read at all)
    // so a future caller of `handle_create` from this module that *does*
    // create straight into "review"/"done" automatically gets the project's
    // real configured policy, not a stale assumption.
    let done_guard = read_config(&handoff.join("config.toml"))
        .map(|c| c.trace.done_guard)
        .ok()
        .filter(|v| v == "block" || v == "off")
        .unwrap_or_else(|| "warn".to_string());

    // E6 (wiki/260 §4.9's own note: "グラフは読み取り専用経路（trace_readonly.rs）
    // で作り"): the same fully-read-only load trace_lint/trace_matrix/trace_impact
    // share — in-memory-only layer-doc resync, runs::load_latest_readonly
    // (never runs::sync), task_ids resolved from the task side without
    // self-repair. This module's only writes are the task creations below
    // (mode="apply"), never anything document-shaped.
    let read_only = super::trace_readonly::load_trace_input_fully_read_only(handoff, Vec::new())?;
    let mut warnings = read_only.warnings.clone();
    warnings.extend(read_only.loaded.config_warnings.clone());
    super::trace_readonly::dedup_preserve_order(&mut warnings);

    // stable_id -> {role -> task_id of the first task holding it}, built
    // from the task-side TaskRequirementLink list (the authority, D3) —
    // exactly what makes an item's existing task "the same role" skip
    // idempotent (§4.9: "同じ role のタスクがある項目は skipped").
    let mut existing_roles: HashMap<String, HashMap<&'static str, String>> = HashMap::new();
    for link in &read_only.loaded.trace_input.task_requirement_links {
        let role = match link.role {
            TaskLinkRole::Implements => "implements",
            TaskLinkRole::Executes => "executes",
        };
        existing_roles
            .entry(link.stable_id.clone())
            .or_default()
            .entry(role)
            .or_insert_with(|| link.task_id.clone());
    }
    let title_index = title_stable_id_index(handoff)?;

    // select.gap_kinds needs the full gap list — built lazily (only when
    // requested) since it's the one part of this scan that needs the whole
    // TraceGraph rather than a flat pass over `docs`.
    //
    // SPEC-TREE-014: the same graph also tells which `parent` composites carry
    // no integrated acceptance criteria (their leaves get the tasks, not
    // them) — built only when a gap filter or any decomposition edge needs it.
    let has_tree = read_only
        .loaded
        .trace_input
        .items
        .iter()
        .any(|i| !i.tree.parents.is_empty());
    let graph: Option<TraceGraph> = (select_gap_kinds.is_some() || has_tree)
        .then(|| TraceGraph::build(&read_only.loaded.trace_input));
    let gap_map: Option<HashMap<String, HashSet<&'static str>>> = select_gap_kinds
        .as_ref()
        .zip(graph.as_ref())
        .map(|(_, graph)| {
            let mut m: HashMap<String, HashSet<&'static str>> = HashMap::new();
            for gap in graph.gaps() {
                if let Some(id) = &gap.item {
                    m.entry(id.clone()).or_default().insert(gap.kind.as_str());
                }
            }
            m
        });
    let mut composites_without_task: Vec<String> = Vec::new();

    let docs = &read_only.loaded.docs;
    let runs_latest = &read_only.loaded.trace_input.runs_latest;

    let mut targets: Vec<TargetItem> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    let mut seen_explicit: HashSet<String> = HashSet::new();
    // t360.20.16 rework (reviewer, round 1): a `stable_id` assigned in more
    // than one document (`GapKind::DuplicateId` — a malformed trace graph,
    // but not one this read-only scan may assume away) must still become at
    // most one target per call, so `mode="apply"` can never create two tasks
    // for the same item in a single invocation. Only the first SubItem to
    // reach this id wins; later ones are dropped silently (not reported in
    // `skipped`, which is reserved for the idempotent "already has a
    // same-role task" case, §4.9 — a duplicate-id collision is a distinct
    // situation `trace_lint`'s own `duplicate_id` rule already surfaces).
    let mut seen_target_ids: HashSet<String> = HashSet::new();
    let mut seen_vrt_ids: HashSet<String> = HashSet::new();

    for doc in docs.iter() {
        let Some(v) = &doc.verification else {
            continue;
        };
        for item in &v.items {
            for sub in &item.sub_items {
                let Some(stable_id) = sub.stable_id.clone() else {
                    continue;
                };
                if let Some(ids) = &items_arg {
                    if !ids.contains(&stable_id) {
                        continue;
                    }
                    seen_explicit.insert(stable_id.clone());
                }
                let Some(layer) = sub.layer.clone().or_else(|| doc.layer.clone()) else {
                    // No resolvable layer (wiki/220 §2.1's "層なし") — no
                    // side, so this item can't be classified left/right.
                    continue;
                };
                if let Some(layers) = &select_layers {
                    if !layers.contains(&layer) {
                        continue;
                    }
                }
                if let Some(stage) = select_dev_stage {
                    // A never-set dev_stage counts as `not_started` (the same
                    // fallback `doc_req_list`'s dev_stage filter applies).
                    if sub.dev_stage.as_deref().unwrap_or(UNSET_DEV_STAGE) != stage {
                        continue;
                    }
                }
                if let Some(kinds) = &select_gap_kinds {
                    let matches = gap_map
                        .as_ref()
                        .and_then(|m| m.get(&stable_id))
                        .is_some_and(|set| kinds.iter().any(|k| set.contains(k.as_str())));
                    if !matches {
                        continue;
                    }
                }

                let role = infer_role_from_category(&sub.category);
                let milestone = sub
                    .effective_milestone(doc)
                    .map(String::from)
                    .or_else(|| default_milestone.clone());
                let acceptance_texts: Vec<(String, String)> = sub
                    .acceptance
                    .iter()
                    .filter_map(|ac| Some((ac.label.clone(), ac.text.clone()?)))
                    .collect();
                let unbackfilled_acceptance =
                    sub.acceptance.iter().filter(|ac| ac.text.is_none()).count();

                'ordinary: {
                    if let Some(existing_task_id) =
                        existing_roles.get(&stable_id).and_then(|m| m.get(role))
                    {
                        skipped.push(json!({
                            "item": stable_id,
                            "role": role,
                            "existing": existing_task_id,
                            "reason": SKIP_REASON_LINK,
                        }));
                        break 'ordinary;
                    }
                    if let Some(existing_task_id) = title_index.by_token.get(&stable_id) {
                        skipped.push(json!({
                            "item": stable_id,
                            "role": role,
                            "existing": existing_task_id,
                            "reason": SKIP_REASON_TITLE,
                        }));
                        break 'ordinary;
                    }
                    if role == "executes"
                        && runs_latest.get(&stable_id).map(String::as_str) == Some("pass")
                    {
                        // Already passing with no executes task yet — nothing to
                        // generate (not a target, and not reported as skipped:
                        // `skipped` is reserved for the idempotent "already has
                        // a same-role task" case, §4.9).
                        break 'ordinary;
                    }

                    if role == "implements"
                        && graph
                            .as_ref()
                            .is_some_and(|g| g.is_unintegrated_composite(&stable_id))
                    {
                        if !composites_without_task.contains(&stable_id) {
                            composites_without_task.push(stable_id.clone());
                        }
                        break 'ordinary;
                    }
                    if !seen_target_ids.insert(stable_id.clone()) {
                        break 'ordinary;
                    }
                    targets.push(TargetItem {
                        stable_id: stable_id.clone(),
                        role,
                        title: sub.description.clone(),
                        layer: layer.clone(),
                        scope_paths: doc.scope_paths.clone(),
                        acceptance_texts: acceptance_texts.clone(),
                        doc_id: doc.id.clone(),
                        unbackfilled_acceptance,
                        milestone: milestone.clone(),
                        vrt: false,
                    });
                }

                // REQ-GUI-REQ-002: a mock-bearing item also gets its VRT task.
                if sub.mock.is_some() && seen_vrt_ids.insert(stable_id.clone()) {
                    let vrt_target = TargetItem {
                        stable_id: stable_id.clone(),
                        role,
                        title: sub.description.clone(),
                        layer,
                        scope_paths: doc.scope_paths.clone(),
                        acceptance_texts: Vec::new(),
                        doc_id: doc.id.clone(),
                        unbackfilled_acceptance: 0,
                        milestone,
                        vrt: true,
                    };
                    match title_index.by_title.get(&vrt_target.task_title()) {
                        Some(existing_task_id) => skipped.push(json!({
                            "item": stable_id,
                            "role": VRT_ROLE,
                            "existing": existing_task_id,
                            "reason": SKIP_REASON_TITLE,
                        })),
                        None => targets.push(vrt_target),
                    }
                }
            }
        }
    }

    // t402: audit trail for the idempotency guard. Apply only — preview is
    // read-only and must leave the project untouched.
    if mode == "apply" {
        for skip in &skipped {
            log_event(
                handoff,
                EVENT_TRACE_TASKS_SKIP,
                skip["existing"].as_str(),
                json!({
                    "stable_id": skip["item"],
                    "role": skip["role"],
                    "reason": skip["reason"],
                }),
                &mut warnings,
            );
        }
    }

    if !composites_without_task.is_empty() {
        warnings.push(format!(
            "no task generated for composite item(s) without integrated acceptance criteria \
             (create tasks for their leaves instead, or give the composite its own acceptance \
             criteria): {}",
            composites_without_task.join(", ")
        ));
    }

    if let Some(ids) = &items_arg {
        for id in ids {
            if !seen_explicit.contains(id) {
                warnings.push(format!("item '{id}' not found in the project"));
            }
        }
    }

    let total_targets = targets.len();
    if total_targets > limit {
        targets.truncate(limit);
        warnings.push(format!(
            "results truncated to limit={limit} of {total_targets} matching items"
        ));
    }

    let tasks_dir = handoff.join("tasks");
    let mut entries: Vec<Value> = Vec::new();

    for target in &targets {
        // The title-based fallback criterion is generated unchanged; this only
        // tells the caller why the item's own acceptance criteria are absent.
        // Only when *no* AC has text: a partly backfilled item still yields
        // real criteria from the ones that do.
        if target.acceptance_texts.is_empty() && target.unbackfilled_acceptance > 0 {
            warnings.push(format!(
                "Item {} has {} acceptance criteria but text is not backfilled. \
                 Re-save doc {} to populate AcRef.text.",
                target.stable_id, target.unbackfilled_acceptance, target.doc_id
            ));
        }
        let title = target.task_title();
        if mode == "preview" {
            let mut entry = json!({
                "item": target.stable_id,
                "role": target.role_label(),
                "title": title,
            });
            if target.vrt {
                entry["kind"] = json!(VRT_ROLE);
            }
            entries.push(entry);
            continue;
        }

        let mut task_val = if target.vrt {
            // No requirement link: the VRT task is keyed by its title alone,
            // leaving the ordinary task as the item's sole `implements` link.
            json!({
                "title": title,
                "labels": VRT_LABELS,
                "scope_paths": target.scope_paths,
                "done_criteria": vrt_done_criteria_json(),
            })
        } else {
            json!({
                "title": title,
                "labels": [format!("layer:{}", target.layer)],
                "scope_paths": target.scope_paths,
                "requirement_ids": [target.stable_id],
                "requirement_roles": { target.stable_id.clone(): target.role },
                "done_criteria": done_criteria_json(&target.acceptance_texts, &title),
            })
        };
        let mut schedule = serde_json::Map::new();
        if let Some(h) = estimate_hours {
            schedule.insert("estimate_hours".to_string(), json!(h));
        }
        if let Some(ms) = &target.milestone {
            schedule.insert("milestone".to_string(), json!(ms));
        }
        if !schedule.is_empty() {
            task_val["schedule"] = Value::Object(schedule);
        }
        let create_args = match parent_id {
            Some(pid) => json!({ "parent_id": pid }),
            None => json!({}),
        };

        let (new_id, msg) = super::update_task::handle_create(
            &tasks_dir,
            &title,
            &task_val,
            &create_args,
            require_estimate_hours,
            handoff,
            &done_guard,
            false,
        )?;
        // `handle_create`'s own message is "Created task <id>: <title>
        // [<status>]" optionally followed by "\nWarning: ..." lines (e.g. a
        // requirement-link resolution warning) — the confirmation line
        // itself is redundant with this call's own structured `created[]`
        // entry below, so only the warning lines (if any) are folded in.
        // A warning this call already reported (the config-load problem is
        // surfaced by the earlier lenient read and again by `handle_create`'s
        // requirement-link resolution) is listed once.
        for line in msg.lines().skip(1) {
            if !line.is_empty() && !warnings.iter().any(|w| w == line) {
                warnings.push(line.to_string());
            }
        }

        log_event(
            handoff,
            EVENT_TRACE_TASKS_CREATE,
            Some(&new_id),
            json!({ "stable_id": target.stable_id, "role": target.role_label() }),
            &mut warnings,
        );
        let mut entry = json!({
            "task_id": new_id,
            "item": target.stable_id,
            "role": target.role_label(),
            "title": title,
        });
        if target.vrt {
            entry["kind"] = json!(VRT_ROLE);
        }
        entries.push(entry);
    }

    // NFR-006 (wiki/270 §2.7): an additional safety-net warning when the
    // actual apply-mode created count exceeds the resolved project profile's
    // `max_generated_per_call` — generation itself stays governed by `limit`
    // (already applied above); this never undoes a creation. Unlike
    // `handoff_trace_scaffold` (whose generated items all belong to one
    // `target_doc`, so a per-document `trace_profile` override makes sense),
    // this tool's targets can span every layer/document in the project, so
    // only the project *default* profile (`[trace] profile`) is resolved
    // here — there is no single document whose override would apply to the
    // whole batch.
    if mode == "apply" {
        let (trace_config, config_warning) = read_trace_config_lenient(handoff);
        push_config_warning(&mut warnings, config_warning);
        let registry = LayerRegistry::build(&trace_config.layer);
        let (resolved, _) = resolve_project_profile(&trace_config, &registry);
        if let Some(max) = resolved.and_then(|p| p.max_generated_per_call) {
            let created_count = entries.len() as u32;
            if created_count > max {
                warnings.push(format!(
                    "Generated {created_count} items, exceeding profile limit of {max}"
                ));
            }
        }
    }

    let mut response = json!({
        "mode": mode,
        "skipped": skipped,
        "warnings": warnings,
    });
    let key = if mode == "apply" {
        "created"
    } else {
        "planned"
    };
    response[key] = Value::Array(entries);

    Ok(to_json(&response))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::docs::DocMetadata;
    use crate::storage::tasks::{find_task_dir_by_id, read_task};
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn ctx(handoff: PathBuf) -> HandlerContext {
        HandlerContext {
            agent_id: None,
            project_dir: handoff.parent().unwrap().to_path_buf(),
            handoff_dir: handoff,
        }
    }

    fn setup() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();
        (tmp, handoff)
    }

    fn layer_doc(id: &str, slug: &str, layer: &str, scope_paths: &[&str]) -> DocMetadata {
        let mut doc = DocMetadata::new(
            id.to_string(),
            slug.to_string(),
            format!("Title {id}"),
            "spec".to_string(),
            "2026-09-28T00:00:00Z".to_string(),
        );
        doc.layer = Some(layer.to_string());
        doc.scope_paths = scope_paths.iter().map(|s| s.to_string()).collect();
        doc
    }

    fn save(c: &HandlerContext, doc_id: &str, body: &str) {
        super::super::docs::handle_doc_save(c, &json!({ "doc_id": doc_id, "body": body })).unwrap();
    }

    #[test]
    fn requires_mutually_exclusive_items_and_select() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff);
        let err = handle_trace_tasks(
            &c,
            &json!({ "items": ["REQ-1"], "select": { "layers": ["requirement"] } }),
        );
        assert!(err.is_err());
    }

    #[test]
    fn rejects_unknown_mode() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff);
        let err = handle_trace_tasks(&c, &json!({ "mode": "bogus" }));
        assert!(err.is_err());
    }

    /// Reviewer feedback (round 1): a scalar `items` (e.g. `items: "REQ-001"`)
    /// used to be silently collapsed to "no filter" by
    /// `.and_then(|v| v.as_array())`, which — combined with `mode="apply"` —
    /// mass-created a task for every item in the project. It must bail
    /// instead, and must never reach task creation.
    #[test]
    fn rejects_items_that_is_not_an_array() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement", &[]);
        crate::storage::docs::write_doc(&handoff, &doc).unwrap();
        save(
            &c,
            "doc-req",
            "# Requirements\n\n### REQ-001 A\n\nStatement.\n\n### REQ-002 B\n\nStatement.\n",
        );

        let err = handle_trace_tasks(
            &c,
            &json!({ "items": "REQ-001", "mode": "apply", "estimate_hours": 1.0 }),
        );
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("items"), "{msg}");

        // Confirm the fail-open regression is actually closed: no task was
        // created for *any* item (the mass-creation the reviewer reproduced
        // against the real binary).
        let task_dirs = std::fs::read_dir(handoff.join("tasks"))
            .map(|d| d.filter_map(|e| e.ok()).count())
            .unwrap_or(0);
        assert_eq!(task_dirs, 0, "a malformed 'items' must create nothing");
    }

    /// A non-object `select` must bail rather than silently mean "no select
    /// filter".
    #[test]
    fn rejects_select_that_is_not_an_object() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff);
        let err = handle_trace_tasks(&c, &json!({ "select": "requirement" }));
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("select"), "{msg}");
    }

    /// The mutual-exclusion check must fire even when `items` is itself
    /// malformed (a scalar) — it used to be bypassed because the malformed
    /// `items` had already been collapsed to `None` before the check ran.
    #[test]
    fn mutual_exclusion_detected_even_when_items_is_malformed() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff);
        let err = handle_trace_tasks(
            &c,
            &json!({ "items": "REQ-001", "select": { "layers": ["requirement"] } }),
        );
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("mutually exclusive"), "{msg}");
    }

    /// An unknown `select.gap_kinds` id must bail, not silently match zero
    /// items (the same policy `trace_lint`'s `rules` filter already applies
    /// to an unknown rule id).
    #[test]
    fn select_gap_kinds_rejects_unknown_kind() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff);
        let err = handle_trace_tasks(&c, &json!({ "select": { "gap_kinds": ["bogus"] } }));
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("bogus"), "{msg}");
    }

    /// A `select.dev_stage` outside the 5-value `SubItem.dev_stage` enum
    /// must bail, not silently match zero items.
    #[test]
    fn select_dev_stage_rejects_invalid_value() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff);
        let err = handle_trace_tasks(&c, &json!({ "select": { "dev_stage": "bogus" } }));
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("bogus"), "{msg}");
    }

    /// `select.gap_kinds` actually filters the scan to items carrying that
    /// gap kind: REQ-001 has no verifier at all (`"unverified"`), REQ-002 is
    /// fully verified by a passing AT (no `"unverified"` gap for it).
    #[test]
    fn select_gap_kinds_filters_to_known_kind() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let req_doc = layer_doc("doc-req", "req-doc", "requirement", &[]);
        crate::storage::docs::write_doc(&handoff, &req_doc).unwrap();
        save(
            &c,
            "doc-req",
            "# Requirements\n\n### REQ-001 A\n\nStatement.\n\n### REQ-002 B\n\nStatement.\n",
        );
        let at_doc = layer_doc("doc-at", "at-doc", "acceptance", &[]);
        crate::storage::docs::write_doc(&handoff, &at_doc).unwrap();
        save(
            &c,
            "doc-at",
            "### AT-002 Check B\n\n- verifies: REQ-002\n- method: manual\n\nStatement.\n",
        );
        super::super::trace::handle_trace_record(
            &c,
            &json!({ "results": [{ "item": "AT-002", "result": "pass" }] }),
        )
        .unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_tasks(&c, &json!({ "select": { "gap_kinds": ["unverified"] } })).unwrap(),
        )
        .unwrap();
        let items: Vec<&str> = out["planned"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["item"].as_str().unwrap())
            .collect();
        assert!(items.contains(&"REQ-001"), "{out}");
        assert!(!items.contains(&"REQ-002"), "{out}");
    }

    /// `select.dev_stage` filters the scan to items carrying that exact
    /// dev_stage.
    #[test]
    fn select_dev_stage_filters_to_matching_items() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement", &[]);
        crate::storage::docs::write_doc(&handoff, &doc).unwrap();
        save(
            &c,
            "doc-req",
            "# Requirements\n\n### REQ-001 A\n\nStatement.\n\n### REQ-002 B\n\nStatement.\n",
        );
        super::super::docs::handle_doc_verify(
            &c,
            &json!({
                "doc_id": "doc-req",
                "action": "set_dev_stage",
                "sub_item_id": "REQ-001",
                "dev_stage": "in_progress",
            }),
        )
        .unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_tasks(&c, &json!({ "select": { "dev_stage": "in_progress" } })).unwrap(),
        )
        .unwrap();
        let items: Vec<&str> = out["planned"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["item"].as_str().unwrap())
            .collect();
        assert_eq!(items, vec!["REQ-001"], "{out}");
    }

    /// An item with no `dev_stage` set counts as `not_started` (the same
    /// fallback `doc_req_list`'s `dev_stage` filter applies), so
    /// `select.dev_stage = "not_started"` matches it.
    #[test]
    fn select_dev_stage_not_started_matches_items_with_no_dev_stage() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement", &[]);
        crate::storage::docs::write_doc(&handoff, &doc).unwrap();
        save(
            &c,
            "doc-req",
            "# Requirements\n\n### REQ-001 A\n\nStatement.\n\n### REQ-002 B\n\nStatement.\n",
        );
        super::super::docs::handle_doc_verify(
            &c,
            &json!({
                "doc_id": "doc-req",
                "action": "set_dev_stage",
                "sub_item_id": "REQ-001",
                "dev_stage": "in_progress",
            }),
        )
        .unwrap();
        // Layer sync may stamp an explicit `not_started`; clear it so REQ-002
        // genuinely carries no dev_stage.
        let mut doc = crate::storage::docs::read_doc(&handoff, "req-doc")
            .unwrap()
            .unwrap();
        for sub in doc
            .verification
            .iter_mut()
            .flat_map(|v| v.items.iter_mut())
            .flat_map(|i| i.sub_items.iter_mut())
            .filter(|s| s.stable_id.as_deref() == Some("REQ-002"))
        {
            sub.dev_stage = None;
        }
        crate::storage::docs::write_doc(&handoff, &doc).unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_tasks(&c, &json!({ "select": { "dev_stage": "not_started" } })).unwrap(),
        )
        .unwrap();
        let items: Vec<&str> = out["planned"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["item"].as_str().unwrap())
            .collect();
        assert_eq!(items, vec!["REQ-002"], "{out}");
    }

    /// A non-string element of `items` must be rejected, not silently
    /// dropped (which would shrink the target set unnoticed).
    #[test]
    fn items_with_a_non_string_element_is_rejected() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff);
        for bad in [json!(["REQ-001", 7]), json!([null]), json!([["REQ-001"]])] {
            let err = handle_trace_tasks(&c, &json!({ "items": bad }));
            let msg = err.unwrap_err().to_string();
            assert!(msg.contains("items"), "{msg}");
        }
    }

    /// Same policy for the other string-array filters.
    #[test]
    fn select_string_arrays_reject_non_string_elements() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff);
        for select in [
            json!({ "layers": ["requirement", 1] }),
            json!({ "gap_kinds": ["unverified", 1] }),
        ] {
            let err = handle_trace_tasks(&c, &json!({ "select": select }));
            assert!(err.is_err());
        }
    }

    /// The shared gap-kind vocabulary is what `select.gap_kinds` accepts.
    #[test]
    fn select_gap_kinds_accepts_every_shared_gap_kind() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff);
        for kind in GapKind::ALL {
            handle_trace_tasks(&c, &json!({ "select": { "gap_kinds": [kind.as_str()] } }))
                .unwrap_or_else(|e| panic!("{kind:?}: {e}"));
        }
    }

    /// A `stable_id` assigned in two documents (`GapKind::DuplicateId`) must
    /// still produce at most one target — `mode="apply"` must never create
    /// two tasks for the same item from a single call.
    #[test]
    fn duplicate_stable_id_across_docs_produces_one_target() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc1 = layer_doc("doc-req-1", "req-doc-1", "requirement", &[]);
        crate::storage::docs::write_doc(&handoff, &doc1).unwrap();
        save(
            &c,
            "doc-req-1",
            "# Requirements\n\n### REQ-001 A\n\nStatement.\n",
        );
        let doc2 = layer_doc("doc-req-2", "req-doc-2", "requirement", &[]);
        crate::storage::docs::write_doc(&handoff, &doc2).unwrap();
        save(&c, "doc-req-2", "### REQ-001 A duplicate\n\nStatement.\n");

        let out: Value =
            serde_json::from_str(&handle_trace_tasks(&c, &json!({})).unwrap()).unwrap();
        let planned = out["planned"].as_array().unwrap();
        let count = planned.iter().filter(|p| p["item"] == "REQ-001").count();
        assert_eq!(count, 1, "{out}");
    }

    /// Preview mode never writes a task, and reports a left-side item
    /// lacking an `implements` task as `planned`.
    #[test]
    fn preview_lists_left_side_item_without_implements_task() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement", &["src/auth/"]);
        crate::storage::docs::write_doc(&handoff, &doc).unwrap();
        save(
            &c,
            "doc-req",
            "# Requirements\n\n### REQ-001 Lockout\n\nStatement.\n",
        );

        let out: Value =
            serde_json::from_str(&handle_trace_tasks(&c, &json!({})).unwrap()).unwrap();
        assert_eq!(out["mode"], "preview", "{out}");
        let planned = out["planned"].as_array().unwrap();
        assert_eq!(planned.len(), 1, "{out}");
        assert_eq!(planned[0]["item"], "REQ-001");
        assert_eq!(planned[0]["role"], "implements");
        assert_eq!(planned[0]["title"], "REQ-001 Lockout");
        assert!(out["skipped"].as_array().unwrap().is_empty());

        // Preview never writes: no task directory of any id exists
        // afterwards (top-level ids are `t<N>`, so probing a fixed id like
        // "1" would pass vacuously even if preview had created a task).
        let task_dirs = std::fs::read_dir(handoff.join("tasks"))
            .map(|d| d.filter_map(|e| e.ok()).count())
            .unwrap_or(0);
        assert_eq!(task_dirs, 0, "preview must not create any task");
    }

    fn tree_setup(parent_extra: &str) -> (TempDir, PathBuf, HandlerContext) {
        let (tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        crate::storage::docs::write_doc(
            &handoff,
            &layer_doc("doc-req", "req-doc", "requirement", &[]),
        )
        .unwrap();
        save(
            &c,
            "doc-req",
            &format!(
                "# Requirements\n\n### REQ-001 Parent\n\n{parent_extra}Parent text.\n\n\
                 ### REQ-002 Leaf one\n\n- parent: REQ-001\n\nL1.\n\n\
                 ### REQ-003 Leaf two\n\n- parent: REQ-001\n\nL2.\n"
            ),
        );
        (tmp, handoff, c)
    }

    fn planned_items(out: &Value) -> Vec<String> {
        out["planned"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["item"].as_str().unwrap().to_string())
            .collect()
    }

    /// SPEC-TREE-014: a composite without integrated acceptance criteria gets
    /// no task of its own — its leaves do.
    #[test]
    fn composite_without_integrated_acceptance_gets_no_task_but_its_leaves_do() {
        let (_tmp, _handoff, c) = tree_setup("");
        let out: Value =
            serde_json::from_str(&handle_trace_tasks(&c, &json!({})).unwrap()).unwrap();
        assert_eq!(planned_items(&out), vec!["REQ-002", "REQ-003"], "{out}");
        let warnings = out["warnings"].to_string();
        assert!(
            warnings.contains("REQ-001"),
            "composite exclusion not reported: {warnings}"
        );
    }

    #[test]
    fn composite_with_integrated_acceptance_criteria_is_still_a_task_target() {
        let (_tmp, _handoff, c) = tree_setup("受入基準:\n- AC1: combined behaviour\n\n");
        let out: Value =
            serde_json::from_str(&handle_trace_tasks(&c, &json!({})).unwrap()).unwrap();
        assert_eq!(
            planned_items(&out),
            vec!["REQ-001", "REQ-002", "REQ-003"],
            "{out}"
        );
    }

    #[test]
    fn explicitly_requesting_an_unintegrated_composite_is_refused_with_a_warning() {
        let (_tmp, _handoff, c) = tree_setup("");
        let out: Value =
            serde_json::from_str(&handle_trace_tasks(&c, &json!({"items": ["REQ-001"]})).unwrap())
                .unwrap();
        assert!(planned_items(&out).is_empty(), "{out}");
        assert!(out["warnings"].to_string().contains("REQ-001"), "{out}");
    }

    /// Apply mode creates a real task via the shared `update_task::handle_create`
    /// path: requirement link + role + labels + scope_paths all land exactly
    /// as §4.9 specifies, and a second apply call is idempotent (skipped).
    #[test]
    fn apply_creates_task_with_link_labels_scope_and_is_idempotent() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement", &["src/auth/login.rs"]);
        crate::storage::docs::write_doc(&handoff, &doc).unwrap();
        save(
            &c,
            "doc-req",
            "# Requirements\n\n### REQ-001 Lockout\n\nStatement.\n",
        );

        let out: Value = serde_json::from_str(
            &handle_trace_tasks(&c, &json!({ "mode": "apply", "estimate_hours": 2.0 })).unwrap(),
        )
        .unwrap();
        assert_eq!(out["mode"], "apply", "{out}");
        let created = out["created"].as_array().unwrap();
        assert_eq!(created.len(), 1, "{out}");
        let task_id = created[0]["task_id"].as_str().unwrap().to_string();
        assert_eq!(created[0]["item"], "REQ-001");
        assert_eq!(created[0]["role"], "implements");
        assert_eq!(created[0]["title"], "REQ-001 Lockout");

        let task_dir = find_task_dir_by_id(&handoff.join("tasks"), &task_id)
            .unwrap()
            .unwrap();
        let (data, _status) = read_task(&task_dir).unwrap().unwrap();
        assert_eq!(data.title, "REQ-001 Lockout");
        assert_eq!(data.labels, vec!["layer:requirement".to_string()]);
        assert_eq!(data.scope_paths, vec!["src/auth/login.rs".to_string()]);
        let link = data
            .task_links
            .iter()
            .find(|l| l.label.as_deref() == Some("REQ-001"))
            .unwrap();
        assert_eq!(link.role.as_deref(), Some("implements"));

        // Second apply call: REQ-001 already has an `implements` task ->
        // skipped, nothing new created (idempotent).
        let second: Value = serde_json::from_str(
            &handle_trace_tasks(&c, &json!({ "mode": "apply", "estimate_hours": 2.0 })).unwrap(),
        )
        .unwrap();
        assert!(second["created"].as_array().unwrap().is_empty(), "{second}");
        let skipped = second["skipped"].as_array().unwrap();
        assert_eq!(skipped.len(), 1, "{second}");
        assert_eq!(skipped[0]["item"], "REQ-001");
        assert_eq!(skipped[0]["role"], "implements");
        assert_eq!(skipped[0]["existing"], task_id);
    }

    fn req_doc_setup(body: &str) -> (TempDir, PathBuf, HandlerContext) {
        let (tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement", &[]);
        crate::storage::docs::write_doc(&handoff, &doc).unwrap();
        save(&c, "doc-req", body);
        (tmp, handoff, c)
    }

    fn count_tasks(handoff: &std::path::Path) -> usize {
        let tasks_dir = handoff.join("tasks");
        if !tasks_dir.exists() {
            return 0;
        }
        let (_, summary) = crate::storage::tasks::build_task_index(&tasks_dir, u32::MAX).unwrap();
        summary.total as usize
    }

    fn trace_tasks_events(
        handoff: &std::path::Path,
        kind: &str,
    ) -> Vec<crate::storage::events::EventRecord> {
        crate::storage::events::read_events(
            handoff,
            &crate::storage::events::EventFilters {
                since: None,
                task_id: None,
                agent_id: None,
                event_type: Some(kind.to_string()),
                limit: Some(1000),
            },
        )
        .unwrap()
    }

    /// t402: a task whose title already starts with the item's stable_id
    /// (`"{stable_id} {title}"`) but carries no requirement link (link write
    /// failed / was removed / created by another tool) still counts as the
    /// existing task: apply must skip it with reason `title_match`, never
    /// create a duplicate.
    #[test]
    fn apply_skips_item_whose_task_exists_by_title_only() {
        let (_tmp, handoff, c) =
            req_doc_setup("# Requirements\n\n### REQ-001 Lockout\n\nStatement.\n");
        super::super::update_task::handle(&c, &json!({ "task": { "title": "REQ-001 Lockout" } }))
            .unwrap();
        assert_eq!(count_tasks(&handoff), 1);

        let out: Value = serde_json::from_str(
            &handle_trace_tasks(&c, &json!({ "mode": "apply", "estimate_hours": 1.0 })).unwrap(),
        )
        .unwrap();
        assert!(out["created"].as_array().unwrap().is_empty(), "{out}");
        let skipped = out["skipped"].as_array().unwrap();
        assert_eq!(skipped.len(), 1, "{out}");
        assert_eq!(skipped[0]["item"], "REQ-001");
        assert_eq!(skipped[0]["reason"], "title_match");
        assert_eq!(count_tasks(&handoff), 1, "no duplicate task");
    }

    /// The title match is on the whole leading token: a task titled
    /// `REQ-0010 ...` must not suppress `REQ-001`.
    #[test]
    fn title_match_requires_exact_stable_id_token() {
        let (_tmp, handoff, c) =
            req_doc_setup("# Requirements\n\n### REQ-001 Lockout\n\nStatement.\n");
        super::super::update_task::handle(
            &c,
            &json!({ "task": { "title": "REQ-0010 Something else" } }),
        )
        .unwrap();
        let out: Value = serde_json::from_str(
            &handle_trace_tasks(&c, &json!({ "mode": "apply", "estimate_hours": 1.0 })).unwrap(),
        )
        .unwrap();
        assert_eq!(out["created"].as_array().unwrap().len(), 1, "{out}");
        assert_eq!(count_tasks(&handoff), 2);
    }

    /// Title-only detection also protects the preview: nothing is planned.
    #[test]
    fn preview_reports_title_only_task_as_skipped() {
        let (_tmp, _handoff, c) =
            req_doc_setup("# Requirements\n\n### REQ-001 Lockout\n\nStatement.\n");
        super::super::update_task::handle(&c, &json!({ "task": { "title": "REQ-001 Lockout" } }))
            .unwrap();
        let out: Value =
            serde_json::from_str(&handle_trace_tasks(&c, &json!({})).unwrap()).unwrap();
        assert!(out["planned"].as_array().unwrap().is_empty(), "{out}");
        assert_eq!(out["skipped"][0]["reason"], "title_match", "{out}");
    }

    /// t402: calling apply twice for the same scope never duplicates tasks,
    /// even across many items, and both the creations and the skips are
    /// recorded in events.jsonl.
    #[test]
    fn double_apply_creates_once_and_logs_create_and_skip_events() {
        let (_tmp, handoff, c) = req_doc_setup(
            "# Requirements\n\n### REQ-001 A\n\nStatement.\n\n### REQ-002 B\n\nStatement.\n",
        );
        let args = json!({ "select": { "layers": ["requirement"] }, "mode": "apply", "estimate_hours": 1.0 });
        let first: Value = serde_json::from_str(&handle_trace_tasks(&c, &args).unwrap()).unwrap();
        assert_eq!(first["created"].as_array().unwrap().len(), 2, "{first}");
        let second: Value = serde_json::from_str(&handle_trace_tasks(&c, &args).unwrap()).unwrap();
        assert!(second["created"].as_array().unwrap().is_empty(), "{second}");
        assert_eq!(second["skipped"].as_array().unwrap().len(), 2, "{second}");
        assert_eq!(count_tasks(&handoff), 2);

        let creates = trace_tasks_events(&handoff, "trace_tasks_create");
        assert_eq!(creates.len(), 2);
        let detail: Value = serde_json::from_str(creates[0].detail.as_deref().unwrap()).unwrap();
        assert_eq!(detail["stable_id"], "REQ-001");
        assert!(creates[0].task_id.is_some());
        assert!(!creates[0].ts.is_empty());

        let skips = trace_tasks_events(&handoff, "trace_tasks_skip");
        assert_eq!(skips.len(), 2);
        let detail: Value = serde_json::from_str(skips[0].detail.as_deref().unwrap()).unwrap();
        assert_eq!(detail["stable_id"], "REQ-001");
        assert_eq!(detail["reason"], "existing_link");
        assert_eq!(skips[0].task_id, creates[0].task_id);
    }

    /// Preview is read-only: it must not append to events.jsonl.
    #[test]
    fn preview_writes_no_events() {
        let (_tmp, handoff, c) = req_doc_setup("# Requirements\n\n### REQ-001 A\n\nStatement.\n");
        handle_trace_tasks(&c, &json!({})).unwrap();
        assert!(!handoff.join("events.jsonl").exists());
    }

    /// NFR-006 (wiki/270 §2.7): when the resolved project profile's
    /// `max_generated_per_call` is exceeded by `mode="apply"`'s actual
    /// created count, a warning is added. Generation itself stays governed
    /// by `limit` — the profile cap is an additional safety net only.
    #[test]
    fn apply_warns_when_created_count_exceeds_profile_max() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        std::fs::write(
            handoff.join("config.toml"),
            "[project]\nname = \"t\"\n\n[trace]\nprofile = \"test\"\n\n\
             [trace.profiles.test]\nextends = \"standard\"\nmax_generated_per_call = 2\n",
        )
        .unwrap();
        let doc = layer_doc("doc-req", "req-doc", "requirement", &[]);
        crate::storage::docs::write_doc(&handoff, &doc).unwrap();
        save(
            &c,
            "doc-req",
            "# Requirements\n\n### REQ-001 A\n\nStatement.\n\n\
             ### REQ-002 B\n\nStatement.\n\n### REQ-003 C\n\nStatement.\n",
        );

        let out: Value = serde_json::from_str(
            &handle_trace_tasks(&c, &json!({ "mode": "apply", "estimate_hours": 1.0 })).unwrap(),
        )
        .unwrap();
        let created = out["created"].as_array().unwrap();
        assert_eq!(created.len(), 3, "{out}");
        let warnings = out["warnings"].as_array().unwrap();
        assert!(
            warnings.iter().any(|w| w
                .as_str()
                .unwrap_or("")
                .contains("Generated 3 items, exceeding profile limit of 2")),
            "expected a profile-limit warning, got: {warnings:?}"
        );
    }

    /// Same profile cap, but `limit=1` keeps the created count at/under it —
    /// no warning.
    #[test]
    fn apply_does_not_warn_when_within_profile_max() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        std::fs::write(
            handoff.join("config.toml"),
            "[project]\nname = \"t\"\n\n[trace]\nprofile = \"test\"\n\n\
             [trace.profiles.test]\nextends = \"standard\"\nmax_generated_per_call = 2\n",
        )
        .unwrap();
        let doc = layer_doc("doc-req", "req-doc", "requirement", &[]);
        crate::storage::docs::write_doc(&handoff, &doc).unwrap();
        save(
            &c,
            "doc-req",
            "# Requirements\n\n### REQ-001 A\n\nStatement.\n\n\
             ### REQ-002 B\n\nStatement.\n\n### REQ-003 C\n\nStatement.\n",
        );

        let out: Value = serde_json::from_str(
            &handle_trace_tasks(
                &c,
                &json!({ "mode": "apply", "estimate_hours": 1.0, "limit": 1 }),
            )
            .unwrap(),
        )
        .unwrap();
        let created = out["created"].as_array().unwrap();
        assert_eq!(created.len(), 1, "{out}");
        let warnings = out["warnings"].as_array().unwrap();
        assert!(
            !warnings
                .iter()
                .any(|w| w.as_str().unwrap_or("").contains("exceeding profile limit")),
            "did not expect a profile-limit warning, got: {warnings:?}"
        );
    }

    /// A right-side (check) item with no run at all is a target (not yet
    /// "passing"); one whose latest recorded result is already "pass" is
    /// not, even though neither has an `executes` task.
    #[test]
    fn right_side_item_is_a_target_unless_already_passing() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let req_doc = layer_doc("doc-req", "req-doc", "requirement", &[]);
        crate::storage::docs::write_doc(&handoff, &req_doc).unwrap();
        save(
            &c,
            "doc-req",
            "# Requirements\n\n### REQ-001 A\n\nStatement.\n\n### REQ-002 B\n\nStatement.\n",
        );
        let at_doc = layer_doc("doc-at", "at-doc", "acceptance", &[]);
        crate::storage::docs::write_doc(&handoff, &at_doc).unwrap();
        save(
            &c,
            "doc-at",
            "### AT-001 Check A\n\n- verifies: REQ-001\n- method: manual\n\nStatement.\n\n\
             ### AT-002 Check B\n\n- verifies: REQ-002\n- method: manual\n\nStatement.\n",
        );

        super::super::trace::handle_trace_record(
            &c,
            &json!({ "results": [{ "item": "AT-002", "result": "pass" }] }),
        )
        .unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_tasks(&c, &json!({ "select": { "layers": ["acceptance"] } })).unwrap(),
        )
        .unwrap();
        let planned = out["planned"].as_array().unwrap();
        let items: Vec<&str> = planned
            .iter()
            .map(|p| p["item"].as_str().unwrap())
            .collect();
        assert!(items.contains(&"AT-001"), "{out}");
        assert!(!items.contains(&"AT-002"), "{out}");
    }

    /// `limit` caps the number of planned/created entries, with a warning,
    /// without touching `skipped`.
    #[test]
    fn limit_truncates_and_warns() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement", &[]);
        crate::storage::docs::write_doc(&handoff, &doc).unwrap();
        save(
            &c,
            "doc-req",
            "# Requirements\n\n### REQ-001 A\n\nStatement.\n\n### REQ-002 B\n\nStatement.\n",
        );

        let out: Value =
            serde_json::from_str(&handle_trace_tasks(&c, &json!({ "limit": 1 })).unwrap()).unwrap();
        assert_eq!(out["planned"].as_array().unwrap().len(), 1, "{out}");
        assert!(out["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("truncated")));
    }

    /// `estimate_hours` is required for `mode="apply"` when
    /// `require_estimate_hours` is enabled (the project default).
    #[test]
    fn apply_requires_estimate_hours_when_project_requires_it() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let doc = layer_doc("doc-req", "req-doc", "requirement", &[]);
        crate::storage::docs::write_doc(&handoff, &doc).unwrap();
        save(
            &c,
            "doc-req",
            "# Requirements\n\n### REQ-001 A\n\nStatement.\n",
        );

        let err = handle_trace_tasks(&c, &json!({ "mode": "apply" }));
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("estimate_hours"));
    }

    /// `parent_id` makes every generated task a child of that task.
    #[test]
    fn apply_with_parent_id_creates_a_child_task() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        std::fs::create_dir_all(handoff.join("tasks").join("1-parent")).unwrap();
        crate::storage::tasks::write_task(
            &handoff.join("tasks").join("1-parent"),
            "todo",
            &crate::storage::tasks::TaskData {
                id: "1".to_string(),
                title: "Parent".to_string(),
                notes: None,
                priority: None,
                created_at: None,
                updated_at: None,
                completed_at: None,
                labels: Vec::new(),
                links: Vec::new(),
                task_links: Vec::new(),
                done_criteria: Vec::new(),
                schedule: None,
                dependencies: Vec::new(),
                order: None,
                assignee: None,
                lock: None,
                scope_paths: Vec::new(),
                extra: HashMap::new(),
            },
        )
        .unwrap();

        let doc = layer_doc("doc-req", "req-doc", "requirement", &[]);
        crate::storage::docs::write_doc(&handoff, &doc).unwrap();
        save(
            &c,
            "doc-req",
            "# Requirements\n\n### REQ-001 A\n\nStatement.\n",
        );

        let out: Value = serde_json::from_str(
            &handle_trace_tasks(
                &c,
                &json!({ "mode": "apply", "parent_id": "1", "estimate_hours": 1.0 }),
            )
            .unwrap(),
        )
        .unwrap();
        let task_id = out["created"][0]["task_id"].as_str().unwrap();
        assert!(task_id.starts_with("1."), "{out}");
    }

    fn created_task_criteria(handoff: &std::path::Path, out: &Value) -> Vec<(String, bool)> {
        let task_id = out["created"][0]["task_id"].as_str().unwrap();
        let task_dir = find_task_dir_by_id(&handoff.join("tasks"), task_id)
            .unwrap()
            .unwrap();
        let (data, _) = read_task(&task_dir).unwrap().unwrap();
        data.done_criteria
            .into_iter()
            .map(|c| (c.item, c.checked))
            .collect()
    }

    /// t404: an item with acceptance criteria yields one unchecked
    /// done_criterion per AC (`"<label>: <text>"`), for GWT/EARS/text alike.
    #[test]
    fn apply_sets_done_criteria_from_acceptance_texts() {
        let (_tmp, handoff, c) = req_doc_setup(
            "# Requirements\n\n### REQ-001 Lockout\n\nStatement.\n\n受入基準:\n\
- AC1: Given 4回失敗済み When 5回目に失敗する Then ロックされる\n\
- AC2: WHEN ロック中 THE SYSTEM SHALL ログインを拒否する\n\
- AC3: 15分後に解除される\n",
        );
        let out: Value = serde_json::from_str(
            &handle_trace_tasks(&c, &json!({ "mode": "apply", "estimate_hours": 1.0 })).unwrap(),
        )
        .unwrap();
        assert_eq!(
            created_task_criteria(&handoff, &out),
            vec![
                (
                    "AC1: Given 4回失敗済み When 5回目に失敗する Then ロックされる".to_string(),
                    false
                ),
                (
                    "AC2: WHEN ロック中 THE SYSTEM SHALL ログインを拒否する".to_string(),
                    false
                ),
                ("AC3: 15分後に解除される".to_string(), false),
            ]
        );
    }

    /// t404: an item with no acceptance criteria still gets one title-based
    /// fallback criterion.
    #[test]
    fn apply_sets_title_based_done_criterion_when_item_has_no_acceptance() {
        let (_tmp, handoff, c) =
            req_doc_setup("# Requirements\n\n### REQ-001 Lockout\n\nStatement.\n");
        let out: Value = serde_json::from_str(
            &handle_trace_tasks(&c, &json!({ "mode": "apply", "estimate_hours": 1.0 })).unwrap(),
        )
        .unwrap();
        assert_eq!(
            created_task_criteria(&handoff, &out),
            vec![("REQ-001 Lockout の実装と検証".to_string(), false)]
        );
    }

    /// A requirement document whose stored `AcRef`s carry no text (last synced
    /// by a binary that predates `AcRef.text`) and has no body to re-parse.
    fn doc_with_unbackfilled_acceptance(handoff: &std::path::Path, text: [Option<&str>; 2]) {
        use crate::storage::docs::model::{AcRef, SubItem, Verification, VerificationItem};
        let mut doc = layer_doc("doc-old", "old-doc", "requirement", &[]);
        let ac = |label: &str, text: Option<&str>| AcRef {
            label: label.to_string(),
            kind: "text".to_string(),
            ac_hash: None,
            text: text.map(String::from),
        };
        let sub = SubItem {
            description: "Lockout".to_string(),
            stable_id: Some("REQ-001".to_string()),
            acceptance: vec![ac("AC1", text[0]), ac("AC2", text[1])],
            ..Default::default()
        };
        doc.verification = Some(Verification {
            status: "pending".to_string(),
            created_at: "2026-09-28T00:00:00Z".to_string(),
            updated_at: "2026-09-28T00:00:00Z".to_string(),
            items: vec![VerificationItem {
                fragment_seq: None,
                heading: "Lockout".to_string(),
                status: "pending".to_string(),
                impl_refs: Vec::new(),
                test_refs: Vec::new(),
                reviewer: None,
                verified_at: None,
                notes: String::new(),
                content_hash_at_verify: None,
                category: "requirement".to_string(),
                sub_items: vec![sub],
                label: None,
            }],
        });
        crate::storage::docs::write_doc(handoff, &doc).unwrap();
    }

    fn backfill_warnings(out: &Value) -> Vec<String> {
        out["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|w| w.as_str())
            .filter(|w| w.contains("not backfilled"))
            .map(String::from)
            .collect()
    }

    /// t409: acceptance entries without stored text warn (preview and apply),
    /// while the title-based fallback criterion is still generated.
    #[test]
    fn warns_when_acceptance_entries_have_no_backfilled_text() {
        for mode in ["preview", "apply"] {
            let (_tmp, handoff) = setup();
            let c = ctx(handoff.clone());
            doc_with_unbackfilled_acceptance(&handoff, [None, None]);
            let out: Value = serde_json::from_str(
                &handle_trace_tasks(&c, &json!({ "mode": mode, "estimate_hours": 1.0 })).unwrap(),
            )
            .unwrap();
            assert_eq!(
                backfill_warnings(&out),
                vec![
                    "Item REQ-001 has 2 acceptance criteria but text is not backfilled. \
                     Re-save doc doc-old to populate AcRef.text."
                        .to_string()
                ],
                "mode={mode}: {out}"
            );
            if mode == "apply" {
                assert_eq!(
                    created_task_criteria(&handoff, &out),
                    vec![("REQ-001 Lockout の実装と検証".to_string(), false)]
                );
            }
        }
    }

    /// t409: a partly backfilled item yields real criteria, so no warning.
    #[test]
    fn no_backfill_warning_when_some_acceptance_text_is_present() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        doc_with_unbackfilled_acceptance(&handoff, [Some("locks"), None]);
        let out: Value =
            serde_json::from_str(&handle_trace_tasks(&c, &json!({})).unwrap()).unwrap();
        assert!(backfill_warnings(&out).is_empty(), "{out}");
    }

    /// t409: an item with no acceptance entries at all never warns.
    #[test]
    fn no_backfill_warning_when_item_has_no_acceptance() {
        let (_tmp, _handoff, c) =
            req_doc_setup("# Requirements\n\n### REQ-001 Lockout\n\nStatement.\n");
        let out: Value =
            serde_json::from_str(&handle_trace_tasks(&c, &json!({})).unwrap()).unwrap();
        assert!(backfill_warnings(&out).is_empty(), "{out}");
    }

    // ---- REQ-GUI-REQ-002 (VRT task generation) / REQ-MS-PIPE-003 (default_milestone) ----

    fn apply_args() -> Value {
        json!({ "mode": "apply", "estimate_hours": 1.0 })
    }

    fn run(c: &HandlerContext, args: Value) -> Value {
        serde_json::from_str(&handle_trace_tasks(c, &args).unwrap()).unwrap()
    }

    fn read_created(handoff: &std::path::Path, task_id: &str) -> crate::storage::tasks::TaskData {
        let dir = find_task_dir_by_id(&handoff.join("tasks"), task_id)
            .unwrap()
            .unwrap();
        read_task(&dir).unwrap().unwrap().0
    }

    fn created_by_title<'a>(out: &'a Value, title: &str) -> &'a Value {
        out["created"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["title"] == title)
            .unwrap_or_else(|| panic!("no created entry titled {title:?}: {out}"))
    }

    const MOCK_BODY: &str =
        "# Requirements\n\n### REQ-001 Login screen\n\n- mock: mocks/login.html\n\nStatement.\n";

    /// AC1/AC2: a mock-bearing SubItem yields the implementation task plus a
    /// `[VRT]` task with the three fixed done_criteria and the vrt labels.
    #[test]
    fn mock_item_generates_vrt_task_with_fixed_criteria() {
        let (_tmp, handoff, c) = req_doc_setup(MOCK_BODY);
        let out = run(&c, apply_args());
        assert_eq!(out["created"].as_array().unwrap().len(), 2, "{out}");
        created_by_title(&out, "REQ-001 Login screen");
        let vrt = created_by_title(&out, "[VRT] REQ-001 Login screen");
        assert_eq!(vrt["item"], "REQ-001");
        assert_eq!(vrt["kind"], "vrt");

        let data = read_created(&handoff, vrt["task_id"].as_str().unwrap());
        let items: Vec<&str> = data.done_criteria.iter().map(|d| d.item.as_str()).collect();
        assert_eq!(items, VRT_DONE_CRITERIA.to_vec());
        assert_eq!(items.len(), 3);
        assert!(data.done_criteria.iter().all(|d| !d.checked));
        assert!(data.labels.contains(&"vrt".to_string()));
        assert!(data.labels.contains(&"gui-verification".to_string()));
    }

    /// AC3: no mock attribute -> only the ordinary task, no `kind` surprises.
    #[test]
    fn item_without_mock_generates_only_the_ordinary_task() {
        let (_tmp, handoff, c) =
            req_doc_setup("# Requirements\n\n### REQ-001 Lockout\n\nStatement.\n");
        let out = run(&c, apply_args());
        assert_eq!(out["created"].as_array().unwrap().len(), 1, "{out}");
        assert_eq!(out["created"][0]["title"], "REQ-001 Lockout");
        assert!(out["created"][0].get("kind").is_none(), "{out}");
        assert_eq!(count_tasks(&handoff), 1);
    }

    #[test]
    fn preview_lists_vrt_entry_and_writes_nothing() {
        let (_tmp, handoff, c) = req_doc_setup(MOCK_BODY);
        let out = run(&c, json!({}));
        let planned = out["planned"].as_array().unwrap();
        assert_eq!(planned.len(), 2, "{out}");
        assert!(planned
            .iter()
            .any(|e| e["title"] == "[VRT] REQ-001 Login screen" && e["kind"] == "vrt"));
        assert_eq!(count_tasks(&handoff), 0);
    }

    /// The VRT task is idempotent on its own: a second apply creates nothing
    /// and reports the VRT task as skipped; deleting only the ordinary task's
    /// presence (VRT exists, ordinary exists) never regenerates either.
    #[test]
    fn vrt_task_is_idempotent() {
        let (_tmp, handoff, c) = req_doc_setup(MOCK_BODY);
        let first = run(&c, apply_args());
        assert_eq!(first["created"].as_array().unwrap().len(), 2);
        let vrt_id = created_by_title(&first, "[VRT] REQ-001 Login screen")["task_id"]
            .as_str()
            .unwrap()
            .to_string();
        let second = run(&c, apply_args());
        assert!(second["created"].as_array().unwrap().is_empty(), "{second}");
        assert!(
            second["skipped"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["item"] == "REQ-001"
                    && s["existing"] == vrt_id.as_str()
                    && s["role"] == "vrt"),
            "{second}"
        );
        assert_eq!(count_tasks(&handoff), 2);
    }

    /// An already-existing ordinary task does not suppress the (missing) VRT task.
    #[test]
    fn existing_ordinary_task_does_not_suppress_vrt_task() {
        let (_tmp, handoff, c) = req_doc_setup(MOCK_BODY);
        super::super::update_task::handle(
            &c,
            &json!({ "task": { "title": "REQ-001 Login screen" } }),
        )
        .unwrap();
        let out = run(&c, apply_args());
        assert_eq!(out["created"].as_array().unwrap().len(), 1, "{out}");
        created_by_title(&out, "[VRT] REQ-001 Login screen");
        assert_eq!(count_tasks(&handoff), 2);
    }

    /// AC4/AC6: default_milestone is applied to every generated task (ordinary
    /// and VRT); without it no milestone is set.
    #[test]
    fn default_milestone_applies_to_generated_tasks() {
        let (_tmp, handoff, c) = req_doc_setup(MOCK_BODY);
        let mut args = apply_args();
        args["default_milestone"] = json!("v0-1");
        let out = run(&c, args);
        for e in out["created"].as_array().unwrap() {
            let data = read_created(&handoff, e["task_id"].as_str().unwrap());
            assert_eq!(
                data.schedule.as_ref().and_then(|s| s.milestone.as_deref()),
                Some("v0-1"),
                "{e}"
            );
        }
    }

    #[test]
    fn no_default_milestone_leaves_tasks_without_milestone() {
        let (_tmp, handoff, c) = req_doc_setup(MOCK_BODY);
        let out = run(&c, apply_args());
        for e in out["created"].as_array().unwrap() {
            let data = read_created(&handoff, e["task_id"].as_str().unwrap());
            assert_eq!(data.schedule.and_then(|s| s.milestone), None, "{e}");
        }
    }

    /// AC5: the SubItem's own milestone beats default_milestone; an item
    /// without one still receives the default.
    #[test]
    fn item_milestone_beats_default_milestone() {
        let (_tmp, handoff, c) = req_doc_setup(
            "# Requirements\n\n### REQ-001 A\n\n- milestone: v0-2\n\nS.\n\n### REQ-002 B\n\nS.\n",
        );
        let mut args = apply_args();
        args["default_milestone"] = json!("v0-1");
        let out = run(&c, args);
        let ms = |title: &str| {
            let e = created_by_title(&out, title);
            read_created(&handoff, e["task_id"].as_str().unwrap())
                .schedule
                .and_then(|s| s.milestone)
        };
        assert_eq!(ms("REQ-001 A").as_deref(), Some("v0-2"));
        assert_eq!(ms("REQ-002 B").as_deref(), Some("v0-1"));
    }

    /// The owning document's milestone is the item's inherited value
    /// (REQ-MS-DOC-001) and so also beats default_milestone.
    #[test]
    fn document_milestone_beats_default_milestone() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let mut doc = layer_doc("doc-req", "req-doc", "requirement", &[]);
        doc.milestone = Some("v0-3".to_string());
        crate::storage::docs::write_doc(&handoff, &doc).unwrap();
        save(&c, "doc-req", "# Requirements\n\n### REQ-001 A\n\nS.\n");
        let mut args = apply_args();
        args["default_milestone"] = json!("v0-1");
        let out = run(&c, args);
        let data = read_created(&handoff, out["created"][0]["task_id"].as_str().unwrap());
        assert_eq!(
            data.schedule.and_then(|s| s.milestone).as_deref(),
            Some("v0-3")
        );
    }

    #[test]
    fn default_milestone_must_be_a_non_empty_string() {
        let (_tmp, _handoff, c) = req_doc_setup(MOCK_BODY);
        for bad in [json!(5), json!(""), json!("  ")] {
            let err = handle_trace_tasks(&c, &json!({ "default_milestone": bad })).unwrap_err();
            assert!(err.to_string().contains("default_milestone"), "{err}");
        }
    }
}
