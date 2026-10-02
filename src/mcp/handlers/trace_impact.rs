//! `handoff_trace_impact` (wiki/260-vmodel-m2-design.md §4.2, M2-06, FR-404):
//! read-only "what would happen if..." impact analysis over 4 mutually
//! exclusive entry points:
//!
//! - `item` + `proposed?`/`proposed_file?`: a proposed Markdown block for one
//!   item (its own heading plus body). Parsed the same way a real layer sync
//!   would (`parse_layer_body`), giving a hypothetical new `def_hash`/
//!   `ac_hash` set to compare against every stored downstream baseline.
//!   `proposed`/`proposed_file` both omitted means "assume changed" (§4.2):
//!   a sentinel hash guaranteed to differ from every real hash is used
//!   instead, so every existing baseline for this item reads as stale.
//! - `doc` + `proposed_body`/`proposed_body_file`: a full layer document body
//!   text. Every item common to both the proposed parse and the current
//!   corpus whose `def_hash`/`ac_hash` would change is treated the same way
//!   as the `item` entry point above, for the whole document at once.
//! - `file`: the M1-era `handoff_doc_req_impact` entry point (wiki/260 §4.2:
//!   "req_impact のファイル照合を docs_query.rs から共通関数に切り出す") —
//!   every requirement whose `impl_refs`/`test_refs`/`scope_paths` matches
//!   this file is treated as "implementation changed" (no `def_hash`
//!   simulation: a code/test file changing does not itself change a
//!   requirement's own definition text, so `would_suspect` naturally stays
//!   empty for this entry point — only `rerun_candidates` is populated).
//! - `git_diff: true`: same as `file`, but against every path `git diff
//!   HEAD --name-only` reports changed.
//!
//! Output: `{changed, would_suspect: {links, tasks}, rerun_candidates,
//! potential: [{id, depth}], truncated, warnings}` (§4.2). `potential` is a
//! BFS over the same `refines`/`verifies` reverse-edge index starting from
//! `changed`, reporting every id reachable at depth >= 2 — "if the
//! intermediate item also changed" ripple, informational only (E1: nothing
//! here is written, and suspects never actually propagate past one hop).
//!
//! Read-only (E6, like `trace_suspect(action="list")`): stays on
//! `trace::load_trace_input_read_only` directly, never calling
//! `resync_direct_edited_layer_docs` or writing any derived file — a directly
//! edited, not-yet-synced layer document's *stored* `def_hash`/`link_baselines`
//! are used as-is (the same accepted E6 gap `trace_suspect`'s M2-08
//! hand-off note describes, not new to this tool).
//!
//! **Rework round 2 (reviewer finding)**: unlike `trace_suspect(action=
//! "list")`, this tool has no write-classified sibling action sharing its
//! name, so it is the first tool in `READ_ONLY_TOOLS` that would otherwise
//! have an actual write path — `trace::load_trace_input`'s `runs::sync` call
//! writes `runs/_latest.json` (and `.handoff/.gitignore`) on the cache's
//! first materialization. `load_trace_input_read_only` (a plain,
//! non-reconciling read of `runs/_latest.json`) avoids that entirely, so the
//! `READ_ONLY_TOOLS` classification and this tool's "never writes" claim
//! (`src/mcp/tools.rs`, `skills/handoff-docs/SKILL.md`) are both literally
//! true — verified by `tests/trace_impact_e2e.rs`'s
//! `trace_impact_never_writes_to_handoff`, which no longer needs a warm-up
//! call before its byte-invariance snapshot.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::docs_query::{
    find_affected_requirements, git_diff_changed_files, normalize_req_impact_path,
};
use super::trace::load_trace_input_read_only;
use super::HandlerContext;
use crate::storage::config::read_config;
use crate::storage::docs::layer_parse::{default_prefix_table, parse_layer_body, ParsedItem};
use crate::trace::TraceItemInput;

/// A hash value guaranteed to never equal a real FNV-1a `def_hash`/`ac_hash`
/// (always a non-empty hex string) — used when `item` mode's `proposed`/
/// `proposed_file` are both omitted ("§4.2 省略時は「変更されたと仮定」"):
/// every existing baseline for the item compares unequal to this sentinel,
/// so it always reads as "would become suspect" without pretending to know
/// what the real new hash would be.
const ASSUMED_CHANGED_SENTINEL: &str = "";

pub fn handle_trace_impact(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;
    let project_dir = &ctx.project_dir;

    let item_arg = arguments.get("item").and_then(|v| v.as_str());
    let doc_arg = arguments.get("doc").and_then(|v| v.as_str());
    let file_arg = arguments.get("file").and_then(|v| v.as_str());
    let git_diff = arguments
        .get("git_diff")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let limit = arguments
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(50) as usize;

    let selected: Vec<&str> = [
        item_arg.is_some().then_some("item"),
        doc_arg.is_some().then_some("doc"),
        file_arg.is_some().then_some("file"),
        git_diff.then_some("git_diff"),
    ]
    .into_iter()
    .flatten()
    .collect();
    if selected.is_empty() {
        bail!(
            "handoff_trace_impact requires exactly one of 'item', 'doc', 'file', or \
             'git_diff: true'"
        );
    }
    if selected.len() > 1 {
        bail!(
            "handoff_trace_impact requires exactly one of 'item', 'doc', 'file', or \
             'git_diff: true' — got {}",
            selected.join(", ")
        );
    }

    let mut warnings: Vec<String> = Vec::new();
    let loaded = load_trace_input_read_only(handoff, Vec::new())?;
    warnings.extend(loaded.config_warnings.clone());
    let by_id: HashMap<&str, &TraceItemInput> = loaded
        .trace_input
        .items
        .iter()
        .map(|i| (i.stable_id.as_str(), i))
        .collect();

    // `changed_hash`: id -> hypothetical new hash, for the `item`/`doc` entry
    // points only. Empty for `file`/`git_diff` (no def_hash simulation, see
    // this module's doc comment) — `would_suspect` naturally comes out empty
    // for those two, and only `changed_ids`/`rerun_candidates` are populated.
    let mut changed_hash: HashMap<String, String> = HashMap::new();
    let mut changed_ids: Vec<String> = Vec::new();
    // Ids present in the current corpus but missing from the proposed text
    // — a deletion or rename the proposal makes (rework round 2 reviewer
    // finding: silently ignoring these gave a false "no impact" reading for
    // one of the most common edits this tool exists to catch). Populated
    // only for the `item`/`doc` entry points where a proposed text was
    // actually parsed — `file`/`git_diff` never simulate a definition at
    // all (see this module's doc comment), so there is nothing to compare.
    let mut removed_ids: Vec<String> = Vec::new();

    if let Some(item_id) = item_arg {
        let Some(current) = by_id.get(item_id) else {
            bail!("item '{item_id}' not found in the trace graph");
        };
        let proposed_text = read_text_arg(arguments, "proposed", "proposed_file")?;
        match proposed_text {
            None => {
                changed_hash.insert(item_id.to_string(), ASSUMED_CHANGED_SENTINEL.to_string());
                changed_ids.push(item_id.to_string());
            }
            Some(text) => {
                let (registry, id_prefixes) = load_layer_registry(handoff)?;
                let prefix_table = default_prefix_table(&registry, &id_prefixes);
                let parsed = parse_layer_body(&text, current.layer.as_deref(), &prefix_table);
                match parsed.items.iter().find(|p| p.id == item_id) {
                    Some(parsed_item) => {
                        insert_parsed_item_changes(
                            parsed_item,
                            &by_id,
                            &mut changed_hash,
                            &mut changed_ids,
                        );
                        // Only this one item's own acceptance criteria are in
                        // scope for `item` mode — the proposed block never
                        // claims to represent the rest of the document.
                        let kept = kept_ids_from_parsed(std::slice::from_ref(parsed_item));
                        let ac_prefix = format!("{item_id}#");
                        removed_ids.extend(
                            by_id
                                .keys()
                                .filter(|id| id.starts_with(&ac_prefix) && !kept.contains(**id))
                                .map(|id| id.to_string()),
                        );
                    }
                    None => {
                        warnings.push(format!(
                            "'proposed' text has no heading for item '{item_id}' — treating it \
                             as changed without a computed hash"
                        ));
                        changed_hash
                            .insert(item_id.to_string(), ASSUMED_CHANGED_SENTINEL.to_string());
                        changed_ids.push(item_id.to_string());
                    }
                }
            }
        }
    } else if let Some(doc_ref) = doc_arg {
        let target_doc = loaded
            .docs
            .iter()
            .find(|d| d.id == doc_ref || d.slug == doc_ref)
            .ok_or_else(|| anyhow::anyhow!("doc '{doc_ref}' not found"))?;
        // `doc` mode is defined over layer documents only (wiki/260 §4.2:
        // "層文書の本文全体"). A non-layer document's stable-id SubItems live
        // in its frontmatter, not in body notation, so `parse_layer_body`
        // finds none of them — without this guard every one of them would be
        // misreported as `removed` (reviewer round 2: an unchanged M1
        // requirement document's own body reported 79 false removals).
        if target_doc.layer.is_none() {
            bail!(
                "doc '{doc_ref}' is not a layer document (no `layer` in its frontmatter) — \
                 'doc' mode only analyzes layer documents; use 'item' or 'file' instead"
            );
        }
        let proposed_body = read_text_arg(arguments, "proposed_body", "proposed_body_file")?
            .ok_or_else(|| {
                anyhow::anyhow!("'doc' requires 'proposed_body' or 'proposed_body_file'")
            })?;
        let (registry, id_prefixes) = load_layer_registry(handoff)?;
        let prefix_table = default_prefix_table(&registry, &id_prefixes);
        let parsed = parse_layer_body(&proposed_body, target_doc.layer.as_deref(), &prefix_table);
        for parsed_item in &parsed.items {
            insert_parsed_item_changes(parsed_item, &by_id, &mut changed_hash, &mut changed_ids);
        }
        // Every id the target document currently owns (its own items plus
        // their materialized implicit-acceptance items, §2.5) that the
        // proposed body no longer defines at all — a deletion, not a
        // `def_hash` change (§4.2 rework round 2).
        let kept = kept_ids_from_parsed(&parsed.items);
        removed_ids.extend(
            by_id
                .iter()
                .filter(|(id, item)| item.doc_id == target_doc.id && !kept.contains(**id))
                .map(|(id, _)| id.to_string()),
        );
    } else {
        // `file` / `git_diff` — the M1-era req_impact entry point (shared
        // matching core, wiki/260 §4.2).
        let target_files: Vec<String> = if let Some(f) = file_arg {
            vec![f.to_string()]
        } else {
            git_diff_changed_files(project_dir)
        };
        let normalized_targets: Vec<String> = target_files
            .iter()
            .map(|f| normalize_req_impact_path(f))
            .collect();
        let affected = find_affected_requirements(&loaded.docs, &normalized_targets);
        let mut ids: BTreeSet<String> = BTreeSet::new();
        for a in &affected {
            ids.insert(a.stable_id.clone());
        }
        changed_ids = ids.into_iter().collect();
        if changed_ids.is_empty() {
            warnings.push("no requirement matched the given file(s)".to_string());
        }
    }

    finish(
        &loaded.trace_input.items,
        &loaded.trace_input.task_requirement_links,
        &by_id,
        changed_hash,
        changed_ids,
        removed_ids,
        limit,
        warnings,
    )
}

/// Ids the proposed text still defines: every parsed item's own id, plus
/// each of its acceptance bullets' implicit id (`"{id}#{label}"`, §2.5's
/// materialized-implicit-acceptance-item id shape). Anything the current
/// corpus has in the same scope (the whole target document for `doc` mode,
/// or just the one item + its ACs for `item` mode) but that is absent from
/// this set is a deletion the proposal makes (§4.2 rework round 2).
fn kept_ids_from_parsed(parsed_items: &[ParsedItem]) -> BTreeSet<String> {
    let mut kept = BTreeSet::new();
    for item in parsed_items {
        kept.insert(item.id.clone());
        for ac in &item.acceptance {
            kept.insert(format!("{}#{}", item.id, ac.label));
        }
    }
    kept
}

/// Reads `text_key`/`file_key` (exactly like `handoff_trace_ingest`'s
/// `output`/`output_file` pair, `src/mcp/handlers/trace_ingest.rs`): the
/// inline value takes priority when both are given, `Ok(None)` when neither
/// is given.
fn read_text_arg(arguments: &Value, text_key: &str, file_key: &str) -> Result<Option<String>> {
    if let Some(t) = arguments.get(text_key).and_then(|v| v.as_str()) {
        return Ok(Some(t.to_string()));
    }
    if let Some(path) = arguments.get(file_key).and_then(|v| v.as_str()) {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {file_key} {path:?}"))?;
        return Ok(Some(content));
    }
    Ok(None)
}

fn load_layer_registry(
    handoff: &Path,
) -> Result<(
    crate::storage::docs::layer::LayerRegistry,
    HashMap<String, Vec<String>>,
)> {
    let trace_config = read_config(&handoff.join("config.toml"))
        .map(|c| c.trace)
        .unwrap_or_default();
    let registry = crate::storage::docs::layer::LayerRegistry::build(&trace_config.layer);
    Ok((registry, trace_config.id_prefixes))
}

/// Folds one parsed item's hypothetical `def_hash` (and, per acceptance
/// bullet, `ac_hash`) into `changed_hash`/`changed_ids` — only for ids that
/// already exist in the current corpus (`by_id`) with a *different* current
/// hash (wiki/260 §4.2: "def_hash が変わる項目を求める"). A brand-new id
/// present only in the proposed text has no downstream baseline to compare
/// against yet, so it is not reported as "changed" here.
fn insert_parsed_item_changes(
    parsed_item: &ParsedItem,
    by_id: &HashMap<&str, &TraceItemInput>,
    changed_hash: &mut HashMap<String, String>,
    changed_ids: &mut Vec<String>,
) {
    if let Some(current) = by_id.get(parsed_item.id.as_str()) {
        if current.def_hash.as_deref() != Some(parsed_item.def_hash.as_str()) {
            changed_hash.insert(parsed_item.id.clone(), parsed_item.def_hash.clone());
            changed_ids.push(parsed_item.id.clone());
        }
    }
    for ac in &parsed_item.acceptance {
        let implicit_id = format!("{}#{}", parsed_item.id, ac.label);
        if let Some(current) = by_id.get(implicit_id.as_str()) {
            if current.def_hash.as_deref() != Some(ac.ac_hash.as_str()) {
                changed_hash.insert(implicit_id.clone(), ac.ac_hash.clone());
                changed_ids.push(implicit_id);
            }
        }
    }
}

/// Raw-ref reverse index: literal authored `refines`/`verifies` reference
/// text (`"REQ-003"` or `"REQ-003#AC2"`) -> every child item that references
/// it that way, with its link type. Built once per call, shared by
/// `would_suspect.links`, `rerun_candidates`, and the `potential` BFS.
fn reverse_ref_index(items: &[TraceItemInput]) -> HashMap<&str, Vec<(&str, &'static str)>> {
    let mut idx: HashMap<&str, Vec<(&str, &'static str)>> = HashMap::new();
    for item in items {
        for r in &item.refines {
            idx.entry(r.as_str())
                .or_default()
                .push((item.stable_id.as_str(), "refines"));
        }
        for r in &item.verifies {
            idx.entry(r.as_str())
                .or_default()
                .push((item.stable_id.as_str(), "verifies"));
        }
    }
    idx
}

#[allow(clippy::too_many_arguments)]
fn finish(
    items: &[TraceItemInput],
    task_links: &[crate::trace::TaskRequirementLink],
    by_id: &HashMap<&str, &TraceItemInput>,
    changed_hash: HashMap<String, String>,
    mut changed_ids: Vec<String>,
    mut removed_ids: Vec<String>,
    limit: usize,
    mut warnings: Vec<String>,
) -> Result<String> {
    changed_ids.sort();
    changed_ids.dedup();
    removed_ids.sort();
    removed_ids.dedup();

    let reverse_idx = reverse_ref_index(items);

    // removed: an id the proposal deletes outright (§4.2 rework round 2) —
    // its direct downstream references and task links, which would become
    // `dangling` once the proposal lands, never `suspect` (a suspect
    // baseline comparison needs a *current* hash on the removed side, which
    // no longer exists).
    let mut removed_entries: Vec<Value> = Vec::new();
    for id in &removed_ids {
        let downstream_refs: Vec<Value> = reverse_idx
            .get(id.as_str())
            .map(|children| {
                children
                    .iter()
                    .map(|(child, link_type)| json!({"child": child, "type": link_type}))
                    .collect()
            })
            .unwrap_or_default();
        let mut removed_tasks: Vec<String> = task_links
            .iter()
            .filter(|l| &l.stable_id == id)
            .map(|l| l.task_id.clone())
            .collect();
        removed_tasks.sort();
        removed_tasks.dedup();
        warnings.push(format!(
            "'{id}' is referenced by the current corpus but is missing from the proposed \
             text — removing it would leave {} downstream reference(s) and {} task link(s) \
             dangling, not suspect",
            downstream_refs.len(),
            removed_tasks.len(),
        ));
        removed_entries.push(json!({
            "id": id,
            "downstream_refs": downstream_refs,
            "tasks": removed_tasks,
        }));
    }

    // would_suspect.links: a downstream item's own stored baseline for a
    // changed upstream ref that no longer matches the hypothetical new hash.
    let mut link_entries: Vec<(String, String, &'static str)> = Vec::new();
    for (changed_id, new_hash) in &changed_hash {
        let Some(children) = reverse_idx.get(changed_id.as_str()) else {
            continue;
        };
        for (child_id, link_type) in children {
            let Some(child_item) = by_id.get(*child_id) else {
                continue;
            };
            if let Some(baseline) = child_item.link_baselines.get(changed_id.as_str()) {
                if baseline != new_hash {
                    link_entries.push((child_id.to_string(), changed_id.clone(), *link_type));
                }
            }
        }
    }
    link_entries.sort();

    // would_suspect.tasks: a task's own baseline_hash for a changed whole-item
    // id that no longer matches the hypothetical new hash.
    let mut task_entries: Vec<(String, String)> = Vec::new();
    for link in task_links {
        if let Some(new_hash) = changed_hash.get(&link.stable_id) {
            if let Some(baseline) = &link.baseline_hash {
                if baseline != new_hash {
                    task_entries.push((link.task_id.clone(), link.stable_id.clone()));
                }
            }
        }
    }
    task_entries.sort();

    // rerun_candidates (§4.2: "その検証項目を再実行の候補"): every changed
    // id's own verifying children (any verifies edge to it, suspect or not —
    // the thing it verifies changed, regardless of whether a baseline was
    // even recorded), plus a changed id that is itself a verification item
    // (has a `method` attribute or declared `test` refs) — its own text
    // changed, so it is itself the test to rerun.
    let mut rerun: BTreeSet<String> = BTreeSet::new();
    for id in &changed_ids {
        if let Some(item) = by_id.get(id.as_str()) {
            if item.method.is_some() || item.has_test_refs {
                rerun.insert(id.clone());
            }
        }
        if let Some(children) = reverse_idx.get(id.as_str()) {
            for (child_id, link_type) in children {
                if *link_type == "verifies" {
                    rerun.insert(child_id.to_string());
                }
            }
        }
    }

    // potential: BFS over the same reverse-edge index from `changed_ids`;
    // depth 0 = changed itself, depth 1 = direct children (already covered
    // above), depth >= 2 reported here (§4.2: "2ホップ目以降で参考表示").
    let mut depth_of: HashMap<&str, usize> = HashMap::new();
    let mut queue: VecDeque<&str> = VecDeque::new();
    for id in &changed_ids {
        depth_of.insert(id.as_str(), 0);
        queue.push_back(id.as_str());
    }
    while let Some(cur) = queue.pop_front() {
        let d = depth_of[cur];
        if let Some(children) = reverse_idx.get(cur) {
            for (child, _link_type) in children {
                if !depth_of.contains_key(child) {
                    depth_of.insert(child, d + 1);
                    queue.push_back(child);
                }
            }
        }
    }
    let mut potential: Vec<(usize, String)> = depth_of
        .into_iter()
        .filter(|(_, d)| *d >= 2)
        .map(|(id, d)| (d, id.to_string()))
        .collect();
    potential.sort();
    let truncated = potential.len() > limit;
    potential.truncate(limit);

    let out = json!({
        "changed": changed_ids,
        "removed": removed_entries,
        "would_suspect": {
            "links": link_entries.into_iter().map(|(child, upstream, link_type)| json!({
                "child": child,
                "upstream": upstream,
                "type": link_type,
            })).collect::<Vec<_>>(),
            "tasks": task_entries.into_iter().map(|(task, item)| json!({
                "task": task,
                "item": item,
            })).collect::<Vec<_>>(),
        },
        "rerun_candidates": rerun.into_iter().collect::<Vec<_>>(),
        "potential": potential.into_iter().map(|(depth, id)| json!({"id": id, "depth": depth})).collect::<Vec<_>>(),
        "truncated": truncated,
        "warnings": warnings,
    });
    Ok(serde_json::to_string_pretty(&out)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::{TaskLinkRole, TaskRequirementLink};

    fn item(id: &str, refines: &[&str], verifies: &[&str]) -> TraceItemInput {
        TraceItemInput {
            stable_id: id.to_string(),
            doc_id: "doc-1".to_string(),
            layer: Some("requirement".to_string()),
            refines: refines.iter().map(|s| s.to_string()).collect(),
            verifies: verifies.iter().map(|s| s.to_string()).collect(),
            method: None,
            has_test_refs: false,
            acceptance_labels: Vec::new(),
            derived: false,
            waived_axes: Vec::new(),
            def_hash: Some(format!("{id}-hash")),
            body_hash: None,
            link_baselines: std::collections::BTreeMap::new(),
            needs: None,
        }
    }

    #[test]
    fn reverse_ref_index_groups_by_literal_raw_ref() {
        let items = vec![
            item("REQ-001", &[], &[]),
            item("SPEC-001", &["REQ-001"], &[]),
            item("ST-001", &[], &["REQ-001#AC1"]),
        ];
        let idx = reverse_ref_index(&items);
        assert_eq!(idx.get("REQ-001").map(|v| v.len()), Some(1));
        assert_eq!(idx["REQ-001"][0], ("SPEC-001", "refines"));
        assert_eq!(idx["REQ-001#AC1"][0], ("ST-001", "verifies"));
    }

    #[test]
    fn finish_flags_a_link_whose_baseline_no_longer_matches_the_new_hash() {
        let mut child = item("SPEC-001", &["REQ-001"], &[]);
        child
            .link_baselines
            .insert("REQ-001".to_string(), "old-hash".to_string());
        let items = vec![item("REQ-001", &[], &[]), child];
        let by_id: HashMap<&str, &TraceItemInput> =
            items.iter().map(|i| (i.stable_id.as_str(), i)).collect();
        let mut changed_hash = HashMap::new();
        changed_hash.insert("REQ-001".to_string(), "new-hash".to_string());

        let out = finish(
            &items,
            &[],
            &by_id,
            changed_hash,
            vec!["REQ-001".to_string()],
            Vec::new(),
            50,
            Vec::new(),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["would_suspect"]["links"][0]["child"], "SPEC-001");
        assert_eq!(v["would_suspect"]["links"][0]["upstream"], "REQ-001");
        assert_eq!(v["would_suspect"]["links"][0]["type"], "refines");
    }

    #[test]
    fn finish_never_flags_an_unbaselined_link_as_would_suspect() {
        // No `link_baselines` entry at all — unbaselined, never a suspect
        // (§3.1/§3.2: distinct classification, never silently guessed).
        let child = item("SPEC-001", &["REQ-001"], &[]);
        let items = vec![item("REQ-001", &[], &[]), child];
        let by_id: HashMap<&str, &TraceItemInput> =
            items.iter().map(|i| (i.stable_id.as_str(), i)).collect();
        let mut changed_hash = HashMap::new();
        changed_hash.insert("REQ-001".to_string(), "new-hash".to_string());

        let out = finish(
            &items,
            &[],
            &by_id,
            changed_hash,
            vec!["REQ-001".to_string()],
            Vec::new(),
            50,
            Vec::new(),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["would_suspect"]["links"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn finish_reports_a_would_suspect_task_link() {
        let items = vec![item("REQ-001", &[], &[])];
        let by_id: HashMap<&str, &TraceItemInput> =
            items.iter().map(|i| (i.stable_id.as_str(), i)).collect();
        let mut changed_hash = HashMap::new();
        changed_hash.insert("REQ-001".to_string(), "new-hash".to_string());
        let task_links = vec![TaskRequirementLink {
            task_id: "t1".to_string(),
            stable_id: "REQ-001".to_string(),
            role: TaskLinkRole::Implements,
            baseline_hash: Some("old-hash".to_string()),
        }];

        let out = finish(
            &items,
            &task_links,
            &by_id,
            changed_hash,
            vec!["REQ-001".to_string()],
            Vec::new(),
            50,
            Vec::new(),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["would_suspect"]["tasks"][0]["task"], "t1");
        assert_eq!(v["would_suspect"]["tasks"][0]["item"], "REQ-001");
    }

    #[test]
    fn finish_rerun_candidates_include_direct_verifiers_of_a_changed_item() {
        let items = vec![item("REQ-001", &[], &[]), item("ST-001", &[], &["REQ-001"])];
        let by_id: HashMap<&str, &TraceItemInput> =
            items.iter().map(|i| (i.stable_id.as_str(), i)).collect();

        let out = finish(
            &items,
            &[],
            &by_id,
            HashMap::new(),
            vec!["REQ-001".to_string()],
            Vec::new(),
            50,
            Vec::new(),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let rerun: Vec<&str> = v["rerun_candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert_eq!(rerun, vec!["ST-001"]);
    }

    #[test]
    fn finish_potential_reports_only_depth_2_and_beyond() {
        // REQ-001 <- SPEC-001 (depth 1, not "potential") <- ST-001 (depth 2).
        let items = vec![
            item("REQ-001", &[], &[]),
            item("SPEC-001", &["REQ-001"], &[]),
            item("ST-001", &[], &["SPEC-001"]),
        ];
        let by_id: HashMap<&str, &TraceItemInput> =
            items.iter().map(|i| (i.stable_id.as_str(), i)).collect();

        let out = finish(
            &items,
            &[],
            &by_id,
            HashMap::new(),
            vec!["REQ-001".to_string()],
            Vec::new(),
            50,
            Vec::new(),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let potential = v["potential"].as_array().unwrap();
        assert_eq!(potential.len(), 1);
        assert_eq!(potential[0]["id"], "ST-001");
        assert_eq!(potential[0]["depth"], 2);
    }

    #[test]
    fn finish_potential_is_truncated_by_limit_and_reports_truncated() {
        let items = vec![
            item("REQ-001", &[], &[]),
            item("SPEC-001", &["REQ-001"], &[]),
            item("ST-001", &[], &["SPEC-001"]),
            item("ST-002", &[], &["SPEC-001"]),
        ];
        let by_id: HashMap<&str, &TraceItemInput> =
            items.iter().map(|i| (i.stable_id.as_str(), i)).collect();

        let out = finish(
            &items,
            &[],
            &by_id,
            HashMap::new(),
            vec!["REQ-001".to_string()],
            Vec::new(),
            1,
            Vec::new(),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["potential"].as_array().unwrap().len(), 1);
        assert_eq!(v["truncated"], true);
    }

    #[test]
    fn insert_parsed_item_changes_skips_a_brand_new_id_not_in_the_corpus() {
        let items = [item("REQ-001", &[], &[])];
        let by_id: HashMap<&str, &TraceItemInput> =
            items.iter().map(|i| (i.stable_id.as_str(), i)).collect();
        let mut changed_hash = HashMap::new();
        let mut changed_ids = Vec::new();

        let (registry, id_prefixes) = (
            crate::storage::docs::layer::LayerRegistry::build(&[]),
            HashMap::new(),
        );
        let prefix_table = default_prefix_table(&registry, &id_prefixes);
        let parsed = parse_layer_body("# REQ-999: brand new\nSome text.\n", None, &prefix_table);
        let parsed_item = &parsed.items[0];
        insert_parsed_item_changes(parsed_item, &by_id, &mut changed_hash, &mut changed_ids);

        assert!(changed_hash.is_empty());
        assert!(changed_ids.is_empty());
    }

    /// Rework round 2 reviewer finding: `finish`'s new `removed_ids` argument
    /// must surface a deleted id's direct downstream references and task
    /// links as `removed[]`, distinct from `would_suspect` (which needs a
    /// *current* hash on the removed side to compare against — there is
    /// none once it's gone).
    #[test]
    fn finish_reports_removed_item_with_downstream_refs_and_tasks() {
        let items = vec![
            item("REQ-001", &[], &[]),
            item("SPEC-001", &["REQ-001"], &[]),
        ];
        let by_id: HashMap<&str, &TraceItemInput> =
            items.iter().map(|i| (i.stable_id.as_str(), i)).collect();
        let task_links = vec![TaskRequirementLink {
            task_id: "t1".to_string(),
            stable_id: "REQ-001".to_string(),
            role: TaskLinkRole::Implements,
            baseline_hash: Some("old-hash".to_string()),
        }];

        let out = finish(
            &items,
            &task_links,
            &by_id,
            HashMap::new(),
            Vec::new(),
            vec!["REQ-001".to_string()],
            50,
            Vec::new(),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["removed"][0]["id"], "REQ-001", "{out}");
        assert_eq!(
            v["removed"][0]["downstream_refs"][0]["child"], "SPEC-001",
            "{out}"
        );
        assert_eq!(
            v["removed"][0]["downstream_refs"][0]["type"], "refines",
            "{out}"
        );
        assert_eq!(v["removed"][0]["tasks"][0], "t1", "{out}");
        assert!(
            v["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w.as_str().unwrap().contains("REQ-001")),
            "removal must also be surfaced in warnings: {out}"
        );
        // Not a `would_suspect` entry: there is no current hash on the
        // removed side to compare a baseline against.
        assert_eq!(
            v["would_suspect"]["links"].as_array().unwrap().len(),
            0,
            "{out}"
        );
    }

    #[test]
    fn kept_ids_from_parsed_includes_item_id_and_implicit_ac_ids() {
        let (registry, id_prefixes) = (
            crate::storage::docs::layer::LayerRegistry::build(&[]),
            HashMap::new(),
        );
        let prefix_table = default_prefix_table(&registry, &id_prefixes);
        let parsed = parse_layer_body(
            "## REQ-001 kept\n\nBody text.\n\n受入基準:\n- AC1: 条件1\n",
            None,
            &prefix_table,
        );
        let kept = kept_ids_from_parsed(&parsed.items);
        assert!(kept.contains("REQ-001"), "{kept:?}");
        assert!(
            kept.iter().any(|id| id == "REQ-001#AC1"),
            "expected an implicit AC1 id in {kept:?}"
        );
    }
}
