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

use anyhow::{bail, Result};
use serde_json::{json, Value};

use super::HandlerContext;
use crate::storage::config::read_config;
use crate::trace::types::{GapKind, TaskLinkRole};
use crate::trace::TraceGraph;

const DEFAULT_LIMIT: u64 = 20;

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

fn gap_kind_str(kind: GapKind) -> &'static str {
    match kind {
        GapKind::Unverified => "unverified",
        GapKind::Unrefined => "unrefined",
        GapKind::Orphan => "orphan",
        GapKind::Dangling => "dangling",
        GapKind::InvalidLink => "invalid_link",
        GapKind::Cycle => "cycle",
        GapKind::DuplicateId => "duplicate_id",
        GapKind::TaskUnlinked => "task_unlinked",
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
/// count against it).
/// The exact `SubItem.dev_stage` vocabulary `docs.rs`'s own
/// `set_dev_stage` action enforces (`VALID_DEV_STAGES` there) — duplicated
/// here rather than widened to `pub(crate)` for this one extra caller, the
/// same "a non-pub helper/constant with exactly one external caller stays
/// duplicated, not exported" convention this module's own
/// `infer_role_from_category` doc comment already states, applied here so
/// this module adds zero lines to `docs.rs` (out of this task's scope, and
/// under concurrent edit by another developer in this session).
const VALID_DEV_STAGES: [&str; 5] = [
    "not_started",
    "in_progress",
    "implemented",
    "tested",
    "verified",
];

/// The exact vocabulary [`gap_kind_str`] (this module's own copy of
/// `trace.rs`'s `gap_kind_str`) can produce — kept in lockstep with that
/// function's match arms so `select.gap_kinds` rejects a typo'd kind rather
/// than silently matching nothing (the same policy `trace_lint`'s `rules`
/// filter applies to an unknown rule id, t360.20.34).
const KNOWN_GAP_KINDS: [&str; 8] = [
    "unverified",
    "unrefined",
    "orphan",
    "dangling",
    "invalid_link",
    "cycle",
    "duplicate_id",
    "task_unlinked",
];

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
        Some(v) => {
            let arr = v
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("items: must be an array of stable ids, got {v}"))?;
            Some(
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect(),
            )
        }
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
        Some(v) => {
            let arr = v.as_array().ok_or_else(|| {
                anyhow::anyhow!("select.layers: must be an array of layer ids, got {v}")
            })?;
            Some(
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect(),
            )
        }
    };
    let select_gap_kinds: Option<Vec<String>> = match select_arg.and_then(|s| s.get("gap_kinds")) {
        None => None,
        Some(v) => {
            let arr = v.as_array().ok_or_else(|| {
                anyhow::anyhow!("select.gap_kinds: must be an array of gap kind ids, got {v}")
            })?;
            let kinds: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            for kind in &kinds {
                if !KNOWN_GAP_KINDS.contains(&kind.as_str()) {
                    bail!(
                        "select.gap_kinds: unknown gap kind {kind:?}; expected one of \
                             {KNOWN_GAP_KINDS:?}"
                    );
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

    // select.gap_kinds needs the full gap list — built lazily (only when
    // requested) since it's the one part of this scan that needs the whole
    // TraceGraph rather than a flat pass over `docs`.
    let gap_map: Option<HashMap<String, HashSet<&'static str>>> =
        select_gap_kinds.as_ref().map(|_| {
            let graph = TraceGraph::build(&read_only.loaded.trace_input);
            let mut m: HashMap<String, HashSet<&'static str>> = HashMap::new();
            for gap in graph.gaps() {
                if let Some(id) = &gap.item {
                    m.entry(id.clone())
                        .or_default()
                        .insert(gap_kind_str(gap.kind));
                }
            }
            m
        });

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
                    if sub.dev_stage.as_deref() != Some(stage) {
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
                if let Some(existing_task_id) =
                    existing_roles.get(&stable_id).and_then(|m| m.get(role))
                {
                    skipped.push(json!({
                        "item": stable_id,
                        "role": role,
                        "existing": existing_task_id,
                    }));
                    continue;
                }
                if role == "executes"
                    && runs_latest.get(&stable_id).map(String::as_str) == Some("pass")
                {
                    // Already passing with no executes task yet — nothing to
                    // generate (not a target, and not reported as skipped:
                    // `skipped` is reserved for the idempotent "already has
                    // a same-role task" case, §4.9).
                    continue;
                }

                if !seen_target_ids.insert(stable_id.clone()) {
                    continue;
                }
                targets.push(TargetItem {
                    stable_id,
                    role,
                    title: sub.description.clone(),
                    layer,
                    scope_paths: doc.scope_paths.clone(),
                });
            }
        }
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
        let title = format!("{} {}", target.stable_id, target.title);
        if mode == "preview" {
            entries.push(json!({
                "item": target.stable_id,
                "role": target.role,
                "title": title,
            }));
            continue;
        }

        let mut task_val = json!({
            "title": title,
            "labels": [format!("layer:{}", target.layer)],
            "scope_paths": target.scope_paths,
            "requirement_ids": [target.stable_id],
            "requirement_roles": { target.stable_id.clone(): target.role },
        });
        if let Some(h) = estimate_hours {
            task_val["schedule"] = json!({ "estimate_hours": h });
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
        for line in msg.lines().skip(1) {
            if !line.is_empty() {
                warnings.push(line.to_string());
            }
        }

        entries.push(json!({
            "task_id": new_id,
            "item": target.stable_id,
            "role": target.role,
            "title": title,
        }));
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
}
